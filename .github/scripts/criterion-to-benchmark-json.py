#!/usr/bin/env python3
"""Convert Criterion's machine-readable estimates into benchmark-action JSON."""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
from typing import Any


def read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise SystemExit(f"expected a JSON object in {path}")
    return value


def finite_number(value: Any, label: str, path: Path) -> float:
    if not isinstance(value, (int, float)) or not math.isfinite(value):
        raise SystemExit(f"invalid {label} in {path}: {value!r}")
    return float(value)


def convert(
    criterion_root: Path,
    context_lines: list[str] | None = None,
) -> list[dict[str, Any]]:
    results: list[dict[str, Any]] = []
    seen_names: set[str] = set()
    context_lines = context_lines or []

    for estimates_path in sorted(criterion_root.glob("**/new/estimates.json")):
        result_dir = estimates_path.parent
        benchmark_path = result_dir / "benchmark.json"
        sample_path = result_dir / "sample.json"
        if not benchmark_path.is_file() or not sample_path.is_file():
            raise SystemExit(f"incomplete Criterion result directory: {result_dir}")

        benchmark = read_json(benchmark_path)
        estimates = read_json(estimates_path)
        sample = read_json(sample_path)

        name = benchmark.get("full_id")
        if not isinstance(name, str) or not name:
            raise SystemExit(f"missing full_id in {benchmark_path}")
        if name in seen_names:
            raise SystemExit(f"duplicate Criterion benchmark id: {name}")
        seen_names.add(name)

        sampling_mode = sample.get("sampling_mode")
        # Criterion's linear samples support a per-iteration regression slope;
        # flat samples do not, so their mean is the corresponding typical
        # estimate. Keep the unit fixed across history to avoid chart splits.
        statistic = "slope" if sampling_mode == "Linear" and "slope" in estimates else "mean"
        estimate = estimates.get(statistic)
        if not isinstance(estimate, dict):
            raise SystemExit(f"missing {statistic} estimate in {estimates_path}")
        confidence = estimate.get("confidence_interval")
        if not isinstance(confidence, dict):
            raise SystemExit(f"missing confidence interval in {estimates_path}")

        point = finite_number(estimate.get("point_estimate"), "point estimate", estimates_path)
        lower = finite_number(confidence.get("lower_bound"), "lower bound", estimates_path)
        upper = finite_number(confidence.get("upper_bound"), "upper bound", estimates_path)
        if point <= 0 or lower <= 0 or upper <= 0 or lower > upper:
            raise SystemExit(f"invalid positive confidence interval in {estimates_path}")

        confidence_level = confidence.get("confidence_level", 0.95)
        throughput = benchmark.get("throughput")
        extra_lines = [
            *context_lines,
            f"Criterion statistic: {statistic}",
            f"Sampling: {sampling_mode}",
            f"{float(confidence_level):.0%} CI: {lower:.6g}..{upper:.6g} ns/iter",
        ]
        if throughput is not None:
            extra_lines.append(f"Throughput input: {json.dumps(throughput, sort_keys=True)}")

        results.append(
            {
                "name": name,
                "unit": "ns/iter",
                "value": point,
                "range": f"{lower:.6g}..{upper:.6g}",
                "extra": "\n".join(extra_lines),
            }
        )

    if not results:
        raise SystemExit(f"no fresh Criterion estimates found below {criterion_root}")
    return results


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--criterion-root",
        type=Path,
        default=Path("target/criterion"),
        help="Criterion output directory (default: target/criterion)",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("benchmark-results.json"),
        help="benchmark-action JSON destination",
    )
    parser.add_argument(
        "--extra",
        action="append",
        default=[],
        help="context line to prepend to every result tooltip (repeatable)",
    )
    args = parser.parse_args()

    results = convert(args.criterion_root, args.extra)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {len(results)} benchmark results to {args.output}")


if __name__ == "__main__":
    main()
