#!/usr/bin/env python3
"""Render an evidence-calibrated PR performance report."""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
from typing import Any


def read_object(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot read {label} {path}: {error}") from error
    if not isinstance(value, dict):
        raise SystemExit(f"expected a JSON object in {path}")
    return value


def markdown_escape(value: str) -> str:
    return value.replace("\\", "\\\\").replace("|", "\\|").replace("\n", " ")


def compact_number(value: float) -> str:
    return f"{value:.6g}"


def coverage_text(status: str) -> str:
    return {
        "direct": "direct targeted coverage",
        "partial": "partial targeted coverage",
        "gap": "a benchmark coverage gap",
        "not_applicable": "no performance-relevant changed files",
    }.get(status, status)


def verdict_text(verdict: str) -> tuple[str, str]:
    return {
        "environment_inconclusive": (
            "Inconclusive",
            "runner health or wall-clock control checks failed",
        ),
        "mixed_inconclusive": (
            "Inconclusive",
            "at least one screened change failed block or replicate consistency",
        ),
        "confirmed_regression_signal": (
            "Confirmed regression signal",
            "one or more slowdowns crossed the screen threshold in both balanced blocks",
        ),
        "confirmed_improvement_signal": (
            "Confirmed improvement signal",
            "one or more speedups crossed the screen threshold in both balanced blocks",
        ),
        "no_confirmed_change": (
            "No confirmed wall-clock change",
            "no targeted cell crossed the threshold consistently in both blocks",
        ),
    }.get(verdict, ("Unknown", verdict))


def status_text(classification: str) -> str:
    return {
        "confirmed_slower": "confirmed slower",
        "confirmed_faster": "confirmed faster",
        "inconclusive": "inconclusive",
        "within_screen": "screened stable",
        "slower": "more instructions",
        "faster": "fewer instructions",
        "stable": "stable",
    }.get(classification, classification)


def render_coverage(scope: dict[str, Any]) -> list[str]:
    lines = [
        (
            f"Coverage is **{coverage_text(scope.get('coverage_status', 'unknown'))}**. "
            "Selected Criterion targets: "
            + (
                ", ".join(f"`{target}`" for target in scope.get("criterion_targets", []))
                or "none"
            )
            + ". Deterministic Callgrind targets: "
            + (
                ", ".join(f"`{target}`" for target in scope.get("callgrind_targets", []))
                or "none"
            )
            + "."
        )
    ]
    limitations = sorted(
        {
            entry["limitation"]
            for entry in scope.get("coverage", [])
            if isinstance(entry, dict) and entry.get("limitation")
        }
    )
    uncovered = scope.get("uncovered_files", [])
    if uncovered:
        limitations.append(
            "No benchmark mapping exists for: "
            + ", ".join(f"`{path}`" for path in uncovered)
            + "."
        )
    limitations.extend(
        gap for gap in scope.get("scope_gaps", []) if isinstance(gap, str) and gap
    )
    if limitations:
        lines.extend(["", "**Coverage limits:** " + " ".join(limitations)])
    return lines


def render_callgrind(callgrind: dict[str, Any], max_rows: int) -> list[str]:
    counts = callgrind.get("counts", {})
    lines = [
        "",
        "### Deterministic CPU counters",
        "",
        (
            "Gungraun/Callgrind found "
            f"**{counts.get('slower', 0)} cells with more instructions**, "
            f"**{counts.get('faster', 0)} with fewer**, and "
            f"**{counts.get('stable', 0)} within "
            f"+/-{callgrind.get('signal_percent', 0):g}%**."
        ),
        "",
        "| Status | Benchmark | `main` | PR | Delta |",
        "|---|---|---:|---:|---:|",
    ]
    for result in callgrind.get("results", [])[:max_rows]:
        lines.append(
            "| "
            + " | ".join(
                [
                    status_text(result.get("classification", "unknown")),
                    markdown_escape(str(result.get("name", ""))),
                    compact_number(float(result["baseline"])),
                    compact_number(float(result["candidate"])),
                    f"{float(result['percent']):+.2f}%",
                ]
            )
            + " |"
        )
    lines.extend(["", f"_{callgrind.get('limitation', '')}_"])
    return lines


def render(
    *,
    paired: dict[str, Any] | None,
    scope: dict[str, Any],
    callgrind: dict[str, Any] | None,
    repository: str,
    baseline_sha: str,
    candidate_sha: str,
    pr_number: int,
    run_url: str,
    max_rows: int,
    benchmark_status: str = "success",
) -> str:
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
        "## PR performance signal",
        "",
        f"PR #{pr_number}: current `main` {baseline_link} -> candidate {candidate_link}.",
        "",
    ]

    skipped_without_work = (
        benchmark_status == "skipped" and not scope.get("has_benchmarks", False)
    )
    if benchmark_status != "success" and not skipped_without_work:
        failure_text = {
            "failure": "benchmark execution failure",
            "cancelled": "benchmark execution cancellation",
            "skipped": "benchmark execution was unexpectedly skipped",
            "artifact_failure": "benchmark artifact download failure",
        }.get(benchmark_status, f"benchmark infrastructure status `{benchmark_status}`")
        lines.extend(
            [
                (
                    f"**Outcome: {failure_text}; no performance "
                    "conclusion.** Any partial measurements from this attempt are not "
                    "treated as review evidence."
                ),
                "",
            ]
        )
        lines.extend(render_coverage(scope))
        lines.extend(
            [
                "",
                f"[Workflow run and diagnostic artifacts]({run_url})",
                "",
                "_This report is a review aid, not a merge gate._",
                "",
            ]
        )
        return "\n".join(lines)

    if paired is None:
        if callgrind is not None:
            lines.extend(
                [
                    (
                        "**Outcome: deterministic CPU signal only.** No wall-clock target "
                        "was selected for these changes."
                    ),
                    "",
                ]
            )
            lines.extend(render_coverage(scope))
            lines.extend(render_callgrind(callgrind, max_rows))
            lines.extend(
                [
                    "",
                    (
                        f"[Workflow run, raw measurements, environment telemetry, and "
                        f"full JSON]({run_url})"
                    ),
                    "",
                    "_This targeted report is a review aid, not a merge gate. Full-suite "
                    "longitudinal history remains limited to `main`, weekly, and release "
                    "runs._",
                    "",
                ]
            )
            return "\n".join(lines)

        status = scope.get("coverage_status")
        if status == "not_applicable":
            lines.extend(
                [
                    "**Outcome: no benchmark run needed.** The changed files are not mapped "
                    "to performance-sensitive code.",
                    "",
                ]
            )
        else:
            lines.extend(
                [
                    "**Outcome: no relevant benchmark coverage.** This is a coverage gap, "
                    "not evidence that performance is unchanged.",
                    "",
                ]
            )
        lines.extend(render_coverage(scope))
        lines.extend(
            [
                "",
                f"[Workflow run and scope artifact]({run_url})",
                "",
                "_This report is a review aid, not a merge gate._",
                "",
            ]
        )
        return "\n".join(lines)

    verdict = paired.get("verdict", "unknown")
    heading, explanation = verdict_text(verdict)
    lines.extend(
        [
            f"**Outcome: {heading}.** {explanation.capitalize()}.",
            "",
        ]
    )
    lines.extend(render_coverage(scope))

    method = paired.get("method", {})
    counts = paired.get("counts", {})
    environment = paired.get("environment", {})
    lines.extend(
        [
            "",
            (
                f"Wall-clock method: {method.get('blocks', 0)} balanced `ABBA`/`BAAB` "
                f"blocks, with both revisions prebuilt. A change is confirmed only when "
                f"both blocks independently cross +/-{method.get('screen_percent', 0):g}% "
                "in the same direction and each arm's paired observations remain within "
                f"{method.get('max_replicate_spread_percent', 0):g}%."
            ),
            (
                f"Targeted cells: **{counts.get('confirmed_slower', 0)} confirmed slower**, "
                f"**{counts.get('confirmed_faster', 0)} confirmed faster**, "
                f"**{counts.get('inconclusive', 0)} inconclusive**, and "
                f"**{counts.get('within_screen', 0)} screened stable**."
            ),
            "",
        ]
    )

    if environment.get("passed"):
        lines.append(
            "Runner quality checks passed, including the paired wall-clock controls "
            f"(limit +/-{method.get('control_percent', 0):g}%)."
        )
    else:
        failures = environment.get("failures", [])
        lines.append(
            "**Runner quality checks failed; product deltas are not actionable.** "
            + " ".join(str(failure) for failure in failures)
        )
    warnings = environment.get("warnings", [])
    if warnings:
        lines.append(" Runner telemetry warning: " + " ".join(map(str, warnings)))

    product = [
        result for result in paired.get("results", []) if not result.get("control")
    ]
    priority = {
        "confirmed_slower": 0,
        "inconclusive": 1,
        "confirmed_faster": 2,
        "within_screen": 3,
    }
    shown = sorted(
        product,
        key=lambda result: (
            priority.get(result.get("classification"), 9),
            -abs(float(result.get("percent", 0))),
        ),
    )[:max_rows]
    if shown:
        lines.extend(
            [
                "",
                (
                    "| Status | Benchmark | `main` | PR | Overall | Block 1 | "
                    "Block 2 | Max replicate spread |"
                ),
                "|---|---|---:|---:|---:|---:|---:|---:|",
            ]
        )
        for result in shown:
            blocks = result.get("blocks", [])
            block_values = [
                f"{float(block.get('percent', 0)):+.2f}%"
                for block in blocks[:2]
            ]
            while len(block_values) < 2:
                block_values.append("n/a")
            replicate_spread = float(
                result.get("max_replicate_spread_percent", 0)
            )
            lines.append(
                "| "
                + " | ".join(
                    [
                        status_text(result.get("classification", "unknown")),
                        markdown_escape(str(result.get("name", ""))),
                        (
                            f"{compact_number(float(result['baseline']))} "
                            f"{markdown_escape(str(result['unit']))}"
                        ),
                        (
                            f"{compact_number(float(result['candidate']))} "
                            f"{markdown_escape(str(result['unit']))}"
                        ),
                        f"{float(result['percent']):+.2f}%",
                        *block_values,
                        f"{replicate_spread:.2f}%",
                    ]
                )
                + " |"
            )

    if callgrind:
        lines.extend(render_callgrind(callgrind, max_rows))

    lines.extend(
        [
            "",
            (
                f"[Workflow run, raw measurements, environment telemetry, and full JSON]"
                f"({run_url})"
            ),
            "",
            "_This targeted report is a review aid, not a merge gate. Full-suite "
            "longitudinal history remains limited to `main`, weekly, and release runs._",
            "",
        ]
    )
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--paired", type=Path)
    parser.add_argument("--scope", type=Path, required=True)
    parser.add_argument("--callgrind", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--baseline-sha", required=True)
    parser.add_argument("--candidate-sha", required=True)
    parser.add_argument("--pr-number", type=int, required=True)
    parser.add_argument("--run-url", required=True)
    parser.add_argument("--max-rows", type=int, default=20)
    parser.add_argument(
        "--benchmark-status",
        choices=("success", "failure", "cancelled", "skipped", "artifact_failure"),
        default="success",
    )
    args = parser.parse_args()

    if args.pr_number <= 0:
        raise SystemExit("--pr-number must be positive")
    if args.max_rows <= 0:
        raise SystemExit("--max-rows must be positive")
    for label, sha in (
        ("baseline", args.baseline_sha),
        ("candidate", args.candidate_sha),
    ):
        if len(sha) != 40 or any(character not in "0123456789abcdef" for character in sha):
            raise SystemExit(f"{label} SHA is not a lowercase 40-character Git SHA")

    scope = read_object(args.scope, "scope")
    paired = read_object(args.paired, "paired result") if args.paired else None
    callgrind = (
        read_object(args.callgrind, "Callgrind result") if args.callgrind else None
    )
    report = render(
        paired=paired,
        scope=scope,
        callgrind=callgrind,
        repository=args.repository,
        baseline_sha=args.baseline_sha,
        candidate_sha=args.candidate_sha,
        pr_number=args.pr_number,
        run_url=args.run_url,
        max_rows=args.max_rows,
        benchmark_status=args.benchmark_status,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(report, encoding="utf-8")
    print(f"wrote PR report to {args.output}")


if __name__ == "__main__":
    main()
