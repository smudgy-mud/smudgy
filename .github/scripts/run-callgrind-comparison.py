#!/usr/bin/env python3
"""Prebuild and compare deterministic Gungraun/Callgrind benchmark targets."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

TARGET_RE = re.compile(r"^[a-z][a-z0-9_]*$")
BASELINE_NAME = "pr_main"


def run(
    command: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    stdout_path: Path,
    stderr_path: Path | None = None,
) -> None:
    print(f"+ ({cwd}) {' '.join(command)}", flush=True)
    stderr_path = stderr_path or stdout_path
    with stdout_path.open("w", encoding="utf-8") as stdout:
        if stderr_path == stdout_path:
            process = subprocess.run(
                command,
                cwd=cwd,
                env=env,
                stdout=stdout,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
            )
        else:
            with stderr_path.open("w", encoding="utf-8") as stderr:
                process = subprocess.run(
                    command,
                    cwd=cwd,
                    env=env,
                    stdout=stdout,
                    stderr=stderr,
                    text=True,
                    check=False,
                )
    if process.returncode:
        tail = stderr_path.read_text(
            encoding="utf-8", errors="replace"
        ).splitlines()[-40:]
        print("\n".join(tail), file=sys.stderr)
        raise SystemExit(
            f"command failed with exit code {process.returncode}; see {stderr_path}"
        )


def checked_child(parent: Path, name: str) -> Path:
    parent = parent.resolve()
    child = (parent / name).resolve()
    if child.parent != parent:
        raise SystemExit(f"unsafe target path outside {parent}: {child}")
    return child


def case_ids(root: Path, pattern: str) -> set[str]:
    return {
        path.parent.relative_to(root).as_posix()
        for path in root.glob(f"**/{pattern}")
    }


def require_identical_case_sets(
    baseline_cases: set[str], candidate_cases: set[str]
) -> None:
    if baseline_cases == candidate_cases:
        return
    missing = sorted(baseline_cases - candidate_cases)
    added = sorted(candidate_cases - baseline_cases)
    raise SystemExit(
        "Callgrind benchmark set changed between revisions; "
        f"missing={missing}, added={added}"
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline-dir", type=Path, required=True)
    parser.add_argument("--candidate-dir", type=Path, required=True)
    parser.add_argument("--target-root", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--target", action="append", required=True)
    parser.add_argument("--cpu-list")
    args = parser.parse_args()

    targets = list(dict.fromkeys(args.target))
    invalid = [target for target in targets if not TARGET_RE.fullmatch(target)]
    if invalid:
        raise SystemExit(f"invalid benchmark target names: {', '.join(invalid)}")
    if not re.fullmatch(r"[A-Za-z0-9_]+", BASELINE_NAME):
        raise SystemExit(f"invalid Gungraun baseline name: {BASELINE_NAME}")
    sources = {
        "baseline": args.baseline_dir.resolve(),
        "candidate": args.candidate_dir.resolve(),
    }
    for label, source in sources.items():
        if not (source / "Cargo.toml").is_file():
            raise SystemExit(f"{label} source has no Cargo.toml: {source}")

    args.target_root.mkdir(parents=True, exist_ok=True)
    output_dir = args.output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    target_dirs = {
        arm: checked_child(args.target_root, f"revision-{arm}")
        for arm in sources
    }
    base_env = {**os.environ, "CARGO_INCREMENTAL": "0"}

    def cargo(arm: str, tail: list[str]) -> tuple[list[str], dict[str, str]]:
        command = ["cargo", *tail]
        if args.cpu_list:
            command = ["taskset", "--cpu-list", args.cpu_list, *command]
        return command, {
            **base_env,
            "CARGO_TARGET_DIR": str(target_dirs[arm]),
        }

    # Both revisions are fully compiled before either one is measured.
    for arm in ("baseline", "candidate"):
        for target in targets:
            command, env = cargo(
                arm,
                [
                    "bench",
                    "--locked",
                    "-p",
                    "smudgy_bench",
                    "--features",
                    "callgrind",
                    "--no-run",
                    "--bench",
                    target,
                ],
            )
            run(
                command,
                cwd=sources[arm],
                env=env,
                stdout_path=output_dir / f"prebuild-callgrind-{arm}-{target}.txt",
            )

    for target in targets:
        command, env = cargo(
            "baseline",
            [
                "bench",
                "--locked",
                "-p",
                "smudgy_bench",
                "--features",
                "callgrind",
                "--bench",
                target,
                "--",
                f"--save-baseline={BASELINE_NAME}",
            ],
        )
        run(
            command,
            cwd=sources["baseline"],
            env=env,
            stdout_path=output_dir / f"callgrind-baseline-{target}.txt",
        )

    baseline_data = checked_child(target_dirs["baseline"], "gungraun")
    candidate_data = checked_child(target_dirs["candidate"], "gungraun")
    if not baseline_data.is_dir():
        raise SystemExit(f"Gungraun did not create baseline data at {baseline_data}")
    baseline_cases = case_ids(
        baseline_data, f"*.out.base@{BASELINE_NAME}"
    )
    if not baseline_cases:
        raise SystemExit(f"no Gungraun baseline cases found below {baseline_data}")
    if candidate_data.exists():
        shutil.rmtree(candidate_data)
    shutil.copytree(baseline_data, candidate_data)

    for target in targets:
        command, env = cargo(
            "candidate",
            [
                "bench",
                "--locked",
                "-p",
                "smudgy_bench",
                "--features",
                "callgrind",
                "--bench",
                target,
                "--",
                f"--baseline={BASELINE_NAME}",
                "--save-summary=pretty-json",
            ],
        )
        run(
            command,
            cwd=sources["candidate"],
            env=env,
            stdout_path=output_dir / f"callgrind-candidate-{target}.txt",
        )

    candidate_summary_paths = sorted(candidate_data.glob("**/summary.json"))
    if not candidate_summary_paths:
        raise SystemExit(f"no Gungraun summaries found below {candidate_data}")
    candidate_cases = {
        path.parent.relative_to(candidate_data).as_posix()
        for path in candidate_summary_paths
    }
    require_identical_case_sets(baseline_cases, candidate_cases)

    artifact_profiles = checked_child(output_dir, "profiles")
    if artifact_profiles.exists():
        shutil.rmtree(artifact_profiles)
    shutil.copytree(candidate_data, artifact_profiles)
    summary_paths = sorted(artifact_profiles.glob("**/summary.json"))
    manifest: dict[str, Any] = {
        "schema_version": 1,
        "baseline_name": BASELINE_NAME,
        "targets": targets,
        "summary_root": artifact_profiles.name,
        "baseline_cases": sorted(baseline_cases),
        "candidate_cases": sorted(candidate_cases),
        "summaries": [
            str(path.relative_to(output_dir)).replace("\\", "/")
            for path in summary_paths
        ],
    }
    manifest_path = output_dir / "callgrind-manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {len(summary_paths)} Callgrind summaries to {manifest_path}")


if __name__ == "__main__":
    main()
