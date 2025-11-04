#!/usr/bin/env python3
import argparse
from thrml_sudoku import solve_sudoku


def _format_board(board):
    lines = []
    for r, row in enumerate(board):
        line = " ".join(str(v) for v in row[0:3]) + "  |  " + " ".join(str(v) for v in row[3:6]) + "  |  " + " ".join(str(v) for v in row[6:9])
        lines.append(line)
        if r in (2, 5):
            lines.append("-" * len(line))
    return "\n".join(lines)


def main():
    p = argparse.ArgumentParser(description="Sudoku solver using THRML")
    p.add_argument("puzzle", help="81-char puzzle string; use 0 or . for blanks")
    p.add_argument("--warmup", type=int, default=5000, help="Gibbs warmup steps (default: 5000)")
    p.add_argument("--steps-per-sample", type=int, default=100, help="Steps between collected samples")
    p.add_argument("--n-samples", type=int, default=32, help="Number of samples to collect and evaluate")
    p.add_argument("--self-bias", type=float, default=2.5, help="Bias to keep current value (stabilizes sampling)")
    p.add_argument("--mode", choices=["row", "cell"], default="row", help="Sampling mode: row-permutation or per-cell")
    p.add_argument("--seed", type=int, default=0, help="RNG seed")
    args = p.parse_args()

    board = solve_sudoku(
        args.puzzle,
        warmup_steps=args.warmup,
        steps_per_sample=args.steps_per_sample,
        n_samples=args.n_samples,
        self_bias=args.self_bias,
        mode=args.mode,
        seed=args.seed,
    )
    print(_format_board(board))


if __name__ == "__main__":
    main()
