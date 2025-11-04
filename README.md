THRML Sudoku Solver
===================

A minimal Sudoku solver built on the THRML library (extropic-ai/thrml). It models the puzzle as categorical variables with pairwise “not-equal” constraints implemented via THRML interaction groups and a custom `SoftmaxConditional` that forbids neighbor categories.

Quick start
-----------

- Requirements: Python 3.10+, `pip install thrml` (installs JAX and deps).
- Run (pure sampling):
  - Per-row permutation blocks (default):
    - `python3 main.py "53..7....6..195....98....6.8...6...34..8.3..17...2...6.6....28....419..5....8..79" --mode row --warmup 15000 --steps-per-sample 300 --n-samples 128 --self-bias 3.0`
  - Per-cell softmax updates:
    - `python3 main.py "..." --mode cell --warmup 15000 --steps-per-sample 300 --n-samples 128 --self-bias 2.5`

Design
------

- Variables: 81 `CategoricalNode`s (values 0..8 map to digits 1..9).
- Constraints: For each free cell, 20 interaction groups connect it to its row/col/box neighbors; the sampler sets logits to a large negative value for any category present among neighbors, effectively forbidding conflicts.
- Clues: Clamped as observed nodes via `BlockGibbsSpec`.
- Inference:
  - `--mode row`: A custom THRML sampler updates all free cells in a row simultaneously and enforces uniqueness within the row via a permutation-style update written in JAX. It uses neighbor constraints (row/col/box) and a self-bias term. If a cell has no legal digits transiently, it chooses the least-conflicting option and continues (still pure THRML).
  - `--mode cell`: A custom `SoftmaxConditional` that computes per-cell logits from neighbor states; self-bias stabilizes updates.
  - In both modes, multiple samples are collected and the board with the fewest conflicts is returned.

Notes
-----

- This is a pure THRML solver (no deterministic backtracking). Convergence depends on warmup, steps-per-sample, self-bias, and seed. Increase them if needed.
- Further improvement: add alternating column/box permutation blocks or soft annealing. If you want, I can extend the sampler schedule to alternate row/column (and box) updates for stronger consistency.
