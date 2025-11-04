from __future__ import annotations

from dataclasses import dataclass
from typing import List, Tuple

import jax
import jax.numpy as jnp

import thrml
from thrml.block_management import Block, make_empty_block_state
from thrml.conditional_samplers import SoftmaxConditional, AbstractConditionalSampler
from thrml.interaction import InteractionGroup
from thrml.pgm import CategoricalNode, DEFAULT_NODE_SHAPE_DTYPES


def _parse_puzzle(puzzle: str) -> List[List[int | None]]:
    """Parse a Sudoku puzzle string into a 9x9 list of ints or None.

    Accepts digits 1-9 for clues, and 0 or '.' for blanks. Ignores whitespace.
    """
    chars = [c for c in puzzle if c in "0123456789."]
    if len(chars) != 81:
        raise ValueError("Puzzle must contain 81 cells of digits or dots")
    grid: List[List[int | None]] = []
    for r in range(9):
        row = []
        for c in range(9):
            ch = chars[r * 9 + c]
            if ch in ("0", "."):
                row.append(None)
            else:
                v = int(ch)
                if not (1 <= v <= 9):
                    raise ValueError("Digits must be 1..9")
                row.append(v)
        grid.append(row)
    return grid


def _neighbors_for_cell(r: int, c: int) -> List[Tuple[int, int]]:
    """Return list of 20 unique neighbor coordinates for a cell: row, col, box (excluding self)."""
    coords = set()
    # Row and column
    for k in range(9):
        if k != c:
            coords.add((r, k))
        if k != r:
            coords.add((k, c))
    # Box
    br, bc = (r // 3) * 3, (c // 3) * 3
    for rr in range(br, br + 3):
        for cc in range(bc, bc + 3):
            if (rr, cc) != (r, c):
                coords.add((rr, cc))
    # Ensure deterministic ordering
    ordered = sorted(list(coords))
    # Sanity: should be 20 unique neighbors
    if len(ordered) != 20:
        # In case ordering dedup “lost” something, fall back to explicit building
        row = [(r, k) for k in range(9) if k != c]
        col = [(k, c) for k in range(9) if k != r]
        box = [
            (rr, cc)
            for rr in range(br, br + 3)
            for cc in range(bc, bc + 3)
            if (rr, cc) != (r, c)
        ]
        seen = set()
        merged = []
        for t in row + col + box:
            if t not in seen:
                seen.add(t)
                merged.append(t)
        ordered = merged
    return ordered


class SudokuSoftmax(SoftmaxConditional):
    """A SoftmaxConditional that forbids choosing any category used by neighbors.

    - Categories are 0..8 representing numbers 1..9.
    - Parameters (logits) are 0 for allowed categories and a large negative value for forbidden ones.
    """

    K: int
    forbid_value: float = -1e9
    # Optional inertia to keep current value via a self-tail InteractionGroup payload
    # If present, payload is a dict with key 'alpha' of shape (n,)

    def compute_parameters(
        self,
        key,
        interactions: list,
        active_flags: list,
        states: list[list],
        sampler_state,
        output_sd,
    ):
        # Determine block size from output_sd
        # output_sd is a pytree (single array for CategoricalNode), shape (n,)
        n = None
        def _extract_shape(sd):
            nonlocal n
            if isinstance(sd, jax.ShapeDtypeStruct):
                n = sd.shape[0]
            return sd

        jax.tree.map(_extract_shape, output_sd)
        if n is None:
            raise RuntimeError("Could not infer block size from output_sd")

        logits = jnp.zeros((n, self.K), dtype=jnp.float32)

        # Aggregate neighbor category presence across all interaction groups, and optional self-bias
        present = jnp.zeros((n, self.K), dtype=jnp.float32)
        alpha_bias = jnp.zeros((n, self.K), dtype=jnp.float32)
        for ig_idx, ig in enumerate(states):
            if len(ig) == 0:
                continue
            neigh = ig[0]  # shape (n, m)
            flags = active_flags[ig_idx]  # shape (n, m) bool
            mask = flags.astype(jnp.float32)[..., None]

            # Payload may be a dict containing 'alpha' for self-bias IG
            payload = interactions[ig_idx]
            is_self_bias = False
            alpha = None
            try:
                # Works for simple dict payloads
                if isinstance(payload, dict) and 'alpha' in payload:
                    is_self_bias = True
                    alpha = payload['alpha']  # shape (n,)
            except Exception:
                is_self_bias = False

            oh = jax.nn.one_hot(neigh.astype(jnp.int32), self.K)  # (n, m, K)
            if is_self_bias and alpha is not None:
                # Use the first tail (m==1) to bias staying at current value
                current_oh = oh[:, 0, :]  # (n, K)
                alpha = jnp.squeeze(alpha, axis=-1) if alpha.ndim == 2 else alpha  # (n,)
                alpha_bias = alpha_bias + (current_oh * alpha[:, None])
            else:
                present = present + jnp.sum(oh * mask, axis=1)  # (n, K)

        # Any category present in neighbors is forbidden; if none allowed, pick those with minimal conflicts
        allowed = present == 0
        any_allowed = jnp.any(allowed, axis=1, keepdims=True)
        min_mask = present == jnp.min(present, axis=1, keepdims=True)
        allowed = jnp.where(any_allowed, allowed, min_mask)
        logits = jnp.where(allowed, logits, self.forbid_value) + alpha_bias
        # Ensure exact (n, K) shape in case of unintended broadcasted dims
        logits = jnp.reshape(logits, (n, self.K))

        return logits, sampler_state


@dataclass
class SudokuTHRMLSolver:
    K: int = 9
    warmup_steps: int = 5000
    steps_per_sample: int = 100
    n_samples: int = 32
    seed: int = 0
    self_bias: float = 2.5
    mode: str = "row"  # "row" permutation blocks or "cell" singleton softmax

    def _build_graph(self, grid: List[List[int | None]]):
        # Create nodes in row-major order
        nodes: List[List[CategoricalNode]] = [[CategoricalNode() for _ in range(9)] for _ in range(9)]

        # Blocks for all nodes, and lists for free/clamped
        all_nodes_block = Block([nodes[r][c] for r in range(9) for c in range(9)])

        # Build free blocks and interactions depending on mode
        clamped_nodes: List[CategoricalNode] = []
        clamped_values: List[int] = []
        for r in range(9):
            for c in range(9):
                v = grid[r][c]
                if v is not None:
                    clamped_nodes.append(nodes[r][c])
                    clamped_values.append(v - 1)

        clamped_block = Block(clamped_nodes) if clamped_nodes else None

        # Map from node -> (r, c)
        pos_of = {nodes[r][c]: (r, c) for r in range(9) for c in range(9)}

        interaction_groups: List[InteractionGroup] = []
        samplers: List[AbstractConditionalSampler] = []
        free_blocks: List[Block] = []

        if self.mode == "row":
            # Each free row is a block; sample a row-wise permutation (unique digits in the row)
            for r in range(9):
                row_free = [nodes[r][c] for c in range(9) if grid[r][c] is None]
                if not row_free:
                    continue
                head = Block(row_free)
                free_blocks.append(head)

                # Build neighbor lists per head index
                free_positions = [pos_of[n] for n in head]
                neighbor_lists: List[List[CategoricalNode]] = []
                for (rr, cc) in free_positions:
                    coords = _neighbors_for_cell(rr, cc)
                    neigh_nodes = [nodes[ar][ac] for (ar, ac) in coords]
                    if len(neigh_nodes) != 20:
                        raise RuntimeError("Unexpected neighbor count; expected 20")
                    neighbor_lists.append(neigh_nodes)

                n_free = len(head)
                for s in range(20):
                    tail_nodes_s = [neighbor_lists[i][s] for i in range(n_free)]
                    tail_block = Block(tail_nodes_s)
                    payload = jnp.zeros((n_free,), dtype=jnp.float32)
                    interaction_groups.append(InteractionGroup(payload, head_nodes=head, tail_nodes=[tail_block]))

                # Self-bias interaction for row
                tail_block = Block(list(head))
                payload = {"alpha": jnp.full((n_free,), float(self.self_bias), dtype=jnp.float32)}
                interaction_groups.append(InteractionGroup(payload, head_nodes=head, tail_nodes=[tail_block]))

                samplers.append(SudokuRowPermutationSampler(K=self.K))
        else:
            # cell-wise singleton softmax
            free_nodes: List[CategoricalNode] = [nodes[r][c] for r in range(9) for c in range(9) if grid[r][c] is None]
            free_blocks = [Block([n]) for n in free_nodes]

            for fb in free_blocks:
                head = fb
                (r, c) = pos_of[fb[0]]
                coords = _neighbors_for_cell(r, c)
                neigh_nodes = [nodes[rr][cc] for (rr, cc) in coords]
                for neigh in neigh_nodes:
                    tail_block = Block([neigh])
                    payload = jnp.zeros((1,), dtype=jnp.float32)
                    interaction_groups.append(InteractionGroup(payload, head_nodes=head, tail_nodes=[tail_block]))
                # Self-bias
                tail_block = Block([fb[0]])
                payload = {"alpha": jnp.full((1,), float(self.self_bias), dtype=jnp.float32)}
                interaction_groups.append(InteractionGroup(payload, head_nodes=head, tail_nodes=[tail_block]))

            samplers = [SudokuSoftmax(K=self.K) for _ in free_blocks]

        # Gibbs spec with constructed blocks
        gibbs_spec = thrml.BlockGibbsSpec(
            free_super_blocks=tuple(free_blocks),
            clamped_blocks=[clamped_block] if clamped_block is not None else [],
            node_shape_dtypes=DEFAULT_NODE_SHAPE_DTYPES,
        )

        program = thrml.FactorSamplingProgram(
            gibbs_spec=gibbs_spec,
            samplers=samplers,
            factors=(),
            other_interaction_groups=interaction_groups,
        )

        # Initial state for free blocks: one array per block with shape (1,)
        init_state_free: List = []
        if free_blocks:
            key = jax.random.PRNGKey(self.seed)
            keys = jax.random.split(key, len(free_blocks))
            for k, block in zip(keys, free_blocks):
                val = jax.random.randint(k, (len(block),), 0, self.K, dtype=jnp.uint8)
                init_state_free.append(val)

        # Clamped state
        clamp_state: List = []
        if clamped_block is not None:
            clamp_arr = jnp.array(clamped_values, dtype=jnp.uint8)
            clamp_state = [clamp_arr]

        return program, all_nodes_block, (free_blocks if free_blocks else None), clamped_block, init_state_free, clamp_state

    def solve(self, puzzle: str) -> List[List[int]]:
        grid = _parse_puzzle(puzzle)
        (
            program,
            all_nodes_block,
            free_block,
            clamped_block,
            init_state_free,
            clamp_state,
        ) = self._build_graph(grid)

        # Handle already-complete puzzle quickly
        if free_block is None:
            return [[(v if v is not None else 0) for v in row] for row in grid]

        schedule = thrml.SamplingSchedule(
            n_warmup=self.warmup_steps,
            n_samples=max(1, self.n_samples),
            steps_per_sample=max(0, self.steps_per_sample),
        )

        # Observe the entire board in row-major order
        key = jax.random.PRNGKey(self.seed)
        samples = thrml.sample_states(
            key,
            program,
            schedule,
            init_state_free,
            clamp_state,
            [all_nodes_block],
        )

        # samples is a list (one per observed block); each element has shape (n_samples, n_nodes)
        all_samples = samples[0]  # (n_samples, 81)
        best_board = _select_best_board_from_samples(all_samples)
        return best_board


class SudokuRowPermutationSampler(AbstractConditionalSampler):
    K: int

    def sample(
        self,
        key,
        interactions: list,
        active_flags: list,
        states: list[list],
        sampler_state,
        output_sd,
    ):
        # Determine block size
        n = None
        def _extract_shape(sd):
            nonlocal n
            if isinstance(sd, jax.ShapeDtypeStruct):
                n = sd.shape[0]
            return sd
        jax.tree.map(_extract_shape, output_sd)
        if n is None:
            raise RuntimeError("Could not infer block size from output_sd")

        # Compute neighbor-present counts and optional self-bias from interactions
        present = jnp.zeros((n, self.K), dtype=jnp.float32)
        bias = jnp.zeros((n, self.K), dtype=jnp.float32)
        for ig_idx, ig in enumerate(states):
            if len(ig) == 0:
                continue
            payload = interactions[ig_idx]
            flags = active_flags[ig_idx]  # (n, m)
            arr = ig[0]  # (n, m)
            if isinstance(payload, dict) and 'alpha' in payload:
                # self-bias IG: m should be 1
                oh = jax.nn.one_hot(arr[:, 0].astype(jnp.int32), self.K)
                alpha = payload['alpha']  # (n,) or (n,1)
                alpha = jnp.squeeze(alpha, axis=-1) if alpha.ndim == 2 else alpha
                bias = bias + oh * alpha[:, None]
            else:
                oh = jax.nn.one_hot(arr.astype(jnp.int32), self.K)  # (n, m, K)
                mask = flags.astype(jnp.float32)[..., None]
                present = present + jnp.sum(oh * mask, axis=1)

        allowed = present == 0  # (n, K)

        # Sequential assignment enforcing uniqueness using JAX fori_loop
        def body(carry, i):
            key, chosen_mask, out = carry
            key, sub = jax.random.split(key, 2)
            row_allowed = allowed[i]
            # prefer least conflicting when nothing allowed
            any_allowed = jnp.any(row_allowed)
            min_mask = present[i] == jnp.min(present[i])
            row_mask = jnp.where(any_allowed, row_allowed, min_mask)
            # enforce uniqueness across row
            row_mask = jnp.logical_and(row_mask, jnp.logical_not(chosen_mask))
            # ensure at least one candidate
            # if none, fall back to not-yet-chosen digits
            any_row = jnp.any(row_mask)
            row_mask = jnp.where(any_row, row_mask, jnp.logical_not(chosen_mask))
            # logits: bias on candidates, -inf elsewhere
            row_logits = jnp.where(row_mask, bias[i], -1e9)
            choice = jax.random.categorical(sub, row_logits)
            choice = jnp.clip(choice, 0, self.K - 1)
            out = out.at[i].set(choice.astype(jnp.uint8))
            chosen_mask = jnp.where(
                jnp.arange(self.K) == choice, jnp.ones((self.K,), dtype=bool), chosen_mask
            )
            return (key, chosen_mask, out), None

        init_out = jnp.zeros((n,), dtype=jnp.uint8)
        init_mask = jnp.zeros((self.K,), dtype=bool)
        (key, chosen_mask, out), _ = jax.lax.scan(body, (key, init_mask, init_out), jnp.arange(n))

        # Cast to expected dtype
        return out.astype(output_sd.dtype), sampler_state


def _select_best_board_from_samples(samples_arr):
    # samples_arr: (n_samples, 81) uint8 categories
    def to_board(vec):
        out = [[0 for _ in range(9)] for _ in range(9)]
        for i in range(81):
            r, c = divmod(i, 9)
            out[r][c] = int(vec[i]) + 1
        return out

    def count_conflicts(board):
        def dup_count(seq):
            seen = {}
            for v in seq:
                seen[v] = seen.get(v, 0) + 1
            return sum(max(0, c - 1) for c in seen.values())
        total = 0
        for r in range(9):
            total += dup_count(board[r])
        for c in range(9):
            total += dup_count([board[r][c] for r in range(9)])
        for br in range(0, 9, 3):
            for bc in range(0, 9, 3):
                cells = [board[r][c] for r in range(br, br + 3) for c in range(bc, bc + 3)]
                total += dup_count(cells)
        return total

    best_board = None
    best_conf = None
    for i in range(samples_arr.shape[0]):
        b = to_board(samples_arr[i])
        conf = count_conflicts(b)
        if best_conf is None or conf < best_conf:
            best_conf = conf
            best_board = b
            if conf == 0:
                break
    return best_board


def _vectorize_board(vec):
    out = [[0 for _ in range(9)] for _ in range(9)]
    for i in range(81):
        r, c = divmod(i, 9)
        out[r][c] = int(vec[i]) + 1
    return out


def _is_complete(board):
    # Quick check for zero conflicts
    def ok_row(row):
        s = set(row)
        return len(s) == 9 and 0 not in s
    for r in range(9):
        if not ok_row(board[r]):
            return False
    for c in range(9):
        col = [board[r][c] for r in range(9)]
        if not ok_row(col):
            return False
    for br in range(0, 9, 3):
        for bc in range(0, 9, 3):
            cells = [board[r][c] for r in range(br, br + 3) for c in range(bc, bc + 3)]
            if not ok_row(cells):
                return False
    return True


 


def solve_sudoku(
    puzzle: str,
    warmup_steps: int = 5000,
    seed: int = 0,
    steps_per_sample: int = 100,
    n_samples: int = 32,
    self_bias: float = 2.5,
    mode: str = "row",
) -> List[List[int]]:
    solver = SudokuTHRMLSolver(
        warmup_steps=warmup_steps,
        steps_per_sample=steps_per_sample,
        n_samples=n_samples,
        seed=seed,
        self_bias=self_bias,
        mode=mode,
    )
    return solver.solve(puzzle)
