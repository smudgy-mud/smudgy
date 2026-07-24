#!/usr/bin/env python3
"""Select targeted PR benchmark suites from a trusted path-to-suite map."""

from __future__ import annotations

import argparse
import fnmatch
import json
from pathlib import Path, PurePosixPath
from typing import Any


def read_object(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise SystemExit(f"expected a JSON object in {path}")
    return value


def normalized(path: str) -> str:
    value = path.strip().replace("\\", "/")
    while value.startswith("./"):
        value = value[2:]
    return str(PurePosixPath(value))


def matches(path: str, patterns: list[str]) -> bool:
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)


def ordered(values: set[str], canonical: list[str]) -> list[str]:
    return [value for value in canonical if value in values]


def select(
    config: dict[str, Any],
    changed_files: list[str],
    expected_file_count: int | None = None,
) -> dict[str, Any]:
    all_criterion = config["criterion_targets"]
    all_callgrind = config["callgrind_targets"]
    if not all(isinstance(value, str) for value in [*all_criterion, *all_callgrind]):
        raise SystemExit("benchmark target lists must contain strings")
    max_criterion = config.get("max_pr_criterion_targets")
    if (
        isinstance(max_criterion, bool)
        or not isinstance(max_criterion, int)
        or max_criterion < 1
    ):
        raise SystemExit("max_pr_criterion_targets must be a positive integer")

    files = sorted({normalized(path) for path in changed_files if path.strip()})
    ignored: list[str] = config.get("ignore", [])
    extensions: set[str] = set(config.get("performance_extensions", []))
    performance_files: set[str] = set(config.get("performance_files", []))
    rules: list[dict[str, Any]] = config.get("rules", [])

    criterion: set[str] = set()
    callgrind: set[str] = set()
    reasons: list[dict[str, Any]] = []
    relevant_files: list[str] = []
    uncovered_files: list[str] = []
    scope_gaps: list[str] = []
    partial = False

    for path in files:
        matched_rules = [
            rule for rule in rules if matches(path, rule.get("globs", []))
        ]
        if not matched_rules and (
            matches(path, ignored)
            or (
                Path(path).suffix not in extensions
                and path not in performance_files
            )
        ):
            continue

        relevant_files.append(path)
        for rule in matched_rules:
            if rule.get("all_criterion"):
                criterion.update(all_criterion)
            criterion.update(rule.get("criterion", []))
            if rule.get("criterion_from_filename"):
                target = Path(path).stem
                if target in all_criterion:
                    criterion.add(target)
                elif target == "ingest_callgrind":
                    callgrind.add(target)
                elif target == "runner_control":
                    criterion.add(target)
                else:
                    uncovered_files.append(path)

            if rule.get("all_callgrind"):
                callgrind.update(all_callgrind)
            callgrind.update(rule.get("callgrind", []))

            coverage = rule.get("coverage", "direct")
            partial |= coverage == "partial"
            reason = {
                "rule": rule.get("name", "unnamed rule"),
                "file": path,
                "coverage": coverage,
                "reason": rule.get("reason", ""),
            }
            if rule.get("limitation"):
                reason["limitation"] = rule["limitation"]
            if reason not in reasons:
                reasons.append(reason)

        if not matched_rules:
            uncovered_files.append(path)

    uncovered_files = sorted(set(uncovered_files))
    file_list_complete = (
        expected_file_count is None or expected_file_count == len(files)
    )
    if not file_list_complete:
        scope_gaps.append(
            "Changed-file list is incomplete: GitHub reported "
            f"{expected_file_count} changed files, but {len(files)} filenames "
            "were collected. Unlisted paths were not classified."
        )
    ordered_criterion = ordered(criterion, all_criterion)
    omitted_criterion: list[str] = []
    if len(ordered_criterion) > max_criterion:
        omitted_criterion = ordered_criterion[max_criterion:]
        ordered_criterion = ordered_criterion[:max_criterion]
        criterion = set(ordered_criterion)
        partial = True
        reasons.append(
            {
                "rule": "PR measurement budget",
                "file": "multiple mapped targets",
                "coverage": "partial",
                "reason": (
                    f"Runs the first {max_criterion} mapped product targets in "
                    "the configured priority order."
                ),
                "limitation": (
                    "The PR measurement budget omitted: "
                    + ", ".join(f"`{target}`" for target in omitted_criterion)
                    + ". The uncapped suite still runs on main, weekly, and releases."
                ),
            }
        )
    if criterion:
        criterion.add("runner_control")

    if not relevant_files:
        status = "gap" if scope_gaps else "not_applicable"
    elif uncovered_files or scope_gaps:
        status = "gap"
    elif partial:
        status = "partial"
    else:
        status = "direct"

    return {
        "schema_version": 1,
        "changed_files": files,
        "expected_changed_file_count": expected_file_count,
        "file_list_complete": file_list_complete,
        "performance_relevant_files": relevant_files,
        "criterion_targets": ordered(
            criterion, ["runner_control", *all_criterion]
        ),
        "omitted_criterion_targets": omitted_criterion,
        "callgrind_targets": ordered(callgrind, all_callgrind),
        "coverage_status": status,
        "coverage": reasons,
        "uncovered_files": uncovered_files,
        "scope_gaps": scope_gaps,
        "has_criterion": bool(criterion),
        "has_callgrind": bool(callgrind),
        "has_benchmarks": bool(criterion or callgrind),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--config",
        type=Path,
        default=Path(".github/benchmark-scope.json"),
    )
    parser.add_argument("--changed-file", action="append", default=[])
    parser.add_argument(
        "--expected-file-count",
        type=int,
        help="changed_files count from the pull request object",
    )
    parser.add_argument(
        "--changed-files",
        type=Path,
        help="newline-delimited changed-file list",
    )
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--github-output",
        type=Path,
        help="optional GitHub Actions output file",
    )
    args = parser.parse_args()

    changed_files = list(args.changed_file)
    if args.changed_files:
        try:
            changed_files.extend(
                args.changed_files.read_text(encoding="utf-8").splitlines()
            )
        except OSError as error:
            raise SystemExit(f"cannot read {args.changed_files}: {error}") from error
    if not changed_files:
        raise SystemExit("provide --changed-file or --changed-files")
    if args.expected_file_count is not None and args.expected_file_count < 0:
        raise SystemExit("--expected-file-count must be non-negative")

    result = select(
        read_object(args.config),
        changed_files,
        expected_file_count=args.expected_file_count,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    compact = json.dumps(result, separators=(",", ":"))
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")

    if args.github_output:
        with args.github_output.open("a", encoding="utf-8") as output:
            output.write(f"has_benchmarks={str(result['has_benchmarks']).lower()}\n")
            output.write(f"has_criterion={str(result['has_criterion']).lower()}\n")
            output.write(f"has_callgrind={str(result['has_callgrind']).lower()}\n")
            output.write(f"coverage_status={result['coverage_status']}\n")
            output.write(f"scope_json={compact}\n")

    print(
        "selected "
        f"{len(result['criterion_targets'])} Criterion and "
        f"{len(result['callgrind_targets'])} Callgrind targets; "
        f"coverage={result['coverage_status']}"
    )


if __name__ == "__main__":
    main()
