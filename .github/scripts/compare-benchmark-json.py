#!/usr/bin/env python3
"""Build a concise PR comment from two benchmark-action JSON result sets."""

from __future__ import annotations

import argparse
import json
import math
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass(frozen=True)
class Measurement:
    name: str
    unit: str
    value: float


@dataclass(frozen=True)
class Delta:
    name: str
    unit: str
    baseline: float
    candidate: float
    percent: float


def read_measurements(path: Path) -> dict[str, Measurement]:
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot read {path}: {error}") from error
    if not isinstance(raw, list) or not raw:
        raise SystemExit(f"expected a non-empty JSON array in {path}")

    measurements: dict[str, Measurement] = {}
    for index, item in enumerate(raw):
        if not isinstance(item, dict):
            raise SystemExit(f"result {index} in {path} is not an object")
        name = item.get("name")
        unit = item.get("unit")
        value = item.get("value")
        if not isinstance(name, str) or not name:
            raise SystemExit(f"result {index} in {path} has no benchmark name")
        if not isinstance(unit, str) or not unit:
            raise SystemExit(f"benchmark {name!r} in {path} has no unit")
        if (
            isinstance(value, bool)
            or not isinstance(value, (int, float))
            or not math.isfinite(value)
            or value <= 0
        ):
            raise SystemExit(f"benchmark {name!r} in {path} has invalid value {value!r}")
        if name in measurements:
            raise SystemExit(f"duplicate benchmark {name!r} in {path}")
        measurements[name] = Measurement(name, unit, float(value))
    return measurements


def compare(
    baseline: dict[str, Measurement],
    candidate: dict[str, Measurement],
) -> list[Delta]:
    missing = sorted(set(baseline) - set(candidate))
    added = sorted(set(candidate) - set(baseline))
    if missing or added:
        details = []
        if missing:
            details.append(f"missing from candidate: {', '.join(missing)}")
        if added:
            details.append(f"new in candidate: {', '.join(added)}")
        raise SystemExit("benchmark sets differ; " + "; ".join(details))

    deltas: list[Delta] = []
    for name, base in baseline.items():
        current = candidate[name]
        if current.unit != base.unit:
            raise SystemExit(
                f"unit changed for {name!r}: {base.unit!r} -> {current.unit!r}"
            )
        deltas.append(
            Delta(
                name=name,
                unit=base.unit,
                baseline=base.value,
                candidate=current.value,
                percent=(current.value / base.value - 1.0) * 100.0,
            )
        )
    return deltas


def markdown_escape(value: str) -> str:
    return value.replace("\\", "\\\\").replace("|", "\\|").replace("\n", " ")


def compact_number(value: float) -> str:
    return f"{value:.6g}"


def render(
    deltas: list[Delta],
    *,
    repository: str,
    baseline_sha: str,
    candidate_sha: str,
    pr_number: int,
    run_url: str,
    noise_percent: float,
    max_rows: int,
) -> str:
    regressions = sum(delta.percent > noise_percent for delta in deltas)
    improvements = sum(delta.percent < -noise_percent for delta in deltas)
    stable = len(deltas) - regressions - improvements
    shown = sorted(deltas, key=lambda delta: abs(delta.percent), reverse=True)[:max_rows]

    baseline_link = (
        f"[`{baseline_sha[:12]}`]"
        f"(https://github.com/{repository}/commit/{baseline_sha})"
    )
    candidate_link = (
        f"[`{candidate_sha[:12]}`]"
        f"(https://github.com/{repository}/commit/{candidate_sha})"
    )

    lines = [
        "<!-- smudgy-benchmark-comparison -->",
        "## M8a benchmark comparison",
        "",
        f"PR #{pr_number}: current `main` {baseline_link} → candidate {candidate_link}.",
        (
            f"Across {len(deltas)} benchmarks: **{regressions} slower**, "
            f"**{improvements} faster**, and **{stable} within ±{noise_percent:g}%**."
        ),
        "",
        "Positive deltas are slower; smaller is better. "
        f"The table shows the {len(shown)} largest absolute changes.",
        "",
        "| | Benchmark | `main` | PR | Delta |",
        "|---|---|---:|---:|---:|",
    ]

    for delta in shown:
        if delta.percent > noise_percent:
            marker = "🔴"
        elif delta.percent < -noise_percent:
            marker = "🟢"
        else:
            marker = "⚪"
        lines.append(
            "| "
            + " | ".join(
                [
                    marker,
                    markdown_escape(delta.name),
                    f"{compact_number(delta.baseline)} {markdown_escape(delta.unit)}",
                    f"{compact_number(delta.candidate)} {markdown_escape(delta.unit)}",
                    f"{delta.percent:+.2f}%",
                ]
            )
            + " |"
        )

    lines.extend(
        [
            "",
            f"[Workflow run and complete comparison summary]({run_url})",
            "",
            "_Measured back-to-back on the same pinned `m8a.2xlarge`; "
            "this PR result is not added to longitudinal benchmark history._",
            "",
        ]
    )
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--baseline-sha", required=True)
    parser.add_argument("--candidate-sha", required=True)
    parser.add_argument("--pr-number", type=int, required=True)
    parser.add_argument("--run-url", required=True)
    parser.add_argument("--noise-percent", type=float, default=5.0)
    parser.add_argument("--max-rows", type=int, default=30)
    args = parser.parse_args()

    if args.pr_number <= 0:
        raise SystemExit("--pr-number must be positive")
    if args.noise_percent < 0 or not math.isfinite(args.noise_percent):
        raise SystemExit("--noise-percent must be a finite non-negative number")
    if args.max_rows <= 0:
        raise SystemExit("--max-rows must be positive")
    for label, sha in (
        ("baseline", args.baseline_sha),
        ("candidate", args.candidate_sha),
    ):
        if len(sha) != 40 or any(character not in "0123456789abcdef" for character in sha):
            raise SystemExit(f"{label} SHA is not a lowercase 40-character Git SHA")

    baseline = read_measurements(args.baseline)
    candidate = read_measurements(args.candidate)
    deltas = compare(baseline, candidate)
    report = render(
        deltas,
        repository=args.repository,
        baseline_sha=args.baseline_sha,
        candidate_sha=args.candidate_sha,
        pr_number=args.pr_number,
        run_url=args.run_url,
        noise_percent=args.noise_percent,
        max_rows=args.max_rows,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(report, encoding="utf-8")
    print(
        f"compared {len(deltas)} benchmarks and wrote PR report to {args.output}"
    )


if __name__ == "__main__":
    main()
