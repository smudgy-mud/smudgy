#!/usr/bin/env python3
"""Prebuild and run targeted Criterion suites in balanced ABBA/BAAB blocks."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

TARGET_RE = re.compile(r"^[a-z][a-z0-9_]*$")
ORDERS = (("baseline", "candidate", "candidate", "baseline"),
          ("candidate", "baseline", "baseline", "candidate"))


def proc_stat() -> dict[str, int]:
    try:
        first = Path("/proc/stat").read_text(encoding="utf-8").splitlines()[0]
        fields = [int(value) for value in first.split()[1:]]
    except (OSError, ValueError, IndexError):
        return {}
    return {
        "total_ticks": sum(fields),
        "idle_ticks": sum(fields[3:5]),
        "steal_ticks": fields[7] if len(fields) > 7 else 0,
    }


def glob_values(pattern: str) -> list[str]:
    values: list[str] = []
    for path in sorted(Path("/").glob(pattern.lstrip("/"))):
        try:
            values.append(path.read_text(encoding="utf-8").strip())
        except OSError:
            continue
    return values


def health_snapshot() -> dict[str, Any]:
    snapshot: dict[str, Any] = {
        "unix_time": time.time(),
        "proc_stat": proc_stat(),
        "governors": sorted(
            set(glob_values("/sys/devices/system/cpu/cpu*/cpufreq/scaling_governor"))
        ),
        "frequencies_khz": [
            int(value)
            for value in glob_values(
                "/sys/devices/system/cpu/cpu*/cpufreq/scaling_cur_freq"
            )
            if value.isdigit()
        ],
        "thermal_millidegrees": [
            int(value)
            for value in glob_values("/sys/class/thermal/thermal_zone*/temp")
            if value.lstrip("-").isdigit()
        ],
    }
    try:
        load = Path("/proc/loadavg").read_text(encoding="utf-8").split()
        snapshot["load"] = [float(value) for value in load[:3]]
        snapshot["runnable"] = load[3]
    except (OSError, ValueError, IndexError):
        snapshot["load"] = []
    return snapshot


def run(
    command: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    log_path: Path,
) -> None:
    print(f"+ ({cwd}) {' '.join(command)}", flush=True)
    with log_path.open("w", encoding="utf-8") as log:
        process = subprocess.run(
            command,
            cwd=cwd,
            env=env,
            stdout=log,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )
    if process.returncode:
        tail = log_path.read_text(encoding="utf-8", errors="replace").splitlines()[-40:]
        print("\n".join(tail), file=sys.stderr)
        raise SystemExit(
            f"command failed with exit code {process.returncode}; see {log_path}"
        )


def checked_child(parent: Path, name: str) -> Path:
    parent = parent.resolve()
    child = (parent / name).resolve()
    if child.parent != parent:
        raise SystemExit(f"unsafe target path outside {parent}: {child}")
    return child


def cargo_command(cpu_list: str | None, arguments: list[str]) -> list[str]:
    command = ["cargo", *arguments]
    if cpu_list:
        command = ["taskset", "--cpu-list", cpu_list, *command]
    return command


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline-dir", type=Path, required=True)
    parser.add_argument("--candidate-dir", type=Path, required=True)
    parser.add_argument("--target-root", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--converter", type=Path, required=True)
    parser.add_argument("--target", action="append", required=True)
    parser.add_argument("--blocks", type=int, default=2)
    parser.add_argument("--cpu-list")
    parser.add_argument("--settle-seconds", type=float, default=2.0)
    args = parser.parse_args()

    if args.blocks < 2:
        raise SystemExit("--blocks must be at least 2 for confirmation")
    if args.settle_seconds < 0:
        raise SystemExit("--settle-seconds cannot be negative")
    targets = list(dict.fromkeys(args.target))
    invalid = [target for target in targets if not TARGET_RE.fullmatch(target)]
    if invalid:
        raise SystemExit(f"invalid benchmark target names: {', '.join(invalid)}")

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

    base_env = os.environ.copy()
    base_env["CARGO_INCREMENTAL"] = "0"
    base_env["SMUDGY_BENCH_SKIP_SANITY"] = "1"

    for arm in ("baseline", "candidate"):
        env = {**base_env, "CARGO_TARGET_DIR": str(target_dirs[arm])}
        for target in targets:
            run(
                cargo_command(
                    args.cpu_list,
                    [
                        "bench",
                        "--locked",
                        "-p",
                        "smudgy_bench",
                        "--features",
                        "pr-benchmarks",
                        "--no-run",
                        "--bench",
                        target,
                    ],
                ),
                cwd=sources[arm],
                env=env,
                log_path=output_dir / f"prebuild-{arm}-{target}.txt",
            )

    time.sleep(max(args.settle_seconds, 3.0))
    manifest: dict[str, Any] = {
        "schema_version": 1,
        "targets": targets,
        "blocks": args.blocks,
        "cpu_list": args.cpu_list,
        "measurements": [],
    }

    sequence_number = 0
    for block in range(args.blocks):
        order = ORDERS[block % len(ORDERS)]
        for position, arm in enumerate(order):
            sequence_number += 1
            target_dir = target_dirs[arm]
            criterion_dir = checked_child(target_dir, "criterion")
            if criterion_dir.exists():
                shutil.rmtree(criterion_dir)

            time.sleep(args.settle_seconds)
            before = health_snapshot()
            combined_log = output_dir / f"criterion-{sequence_number:02d}-{arm}.txt"
            log_parts: list[Path] = []
            env = {**base_env, "CARGO_TARGET_DIR": str(target_dir)}
            for target in targets:
                log_path = output_dir / (
                    f"criterion-{sequence_number:02d}-{arm}-{target}.txt"
                )
                run(
                    cargo_command(
                        args.cpu_list,
                        [
                            "bench",
                            "--locked",
                            "-p",
                            "smudgy_bench",
                            "--features",
                            "pr-benchmarks",
                            "--bench",
                            target,
                            "--",
                            "--noplot",
                        ],
                    ),
                    cwd=sources[arm],
                    env=env,
                    log_path=log_path,
                )
                log_parts.append(log_path)
            after = health_snapshot()

            with combined_log.open("w", encoding="utf-8") as combined:
                for part in log_parts:
                    combined.write(f"===== {part.name} =====\n")
                    combined.write(part.read_text(encoding="utf-8", errors="replace"))
                    combined.write("\n")

            result_path = output_dir / (
                f"criterion-{sequence_number:02d}-{arm}.json"
            )
            convert_env = {
                **base_env,
                "PYTHONUTF8": "1",
            }
            run(
                [
                    sys.executable,
                    str(args.converter.resolve()),
                    "--criterion-root",
                    str(criterion_dir),
                    "--output",
                    str(result_path),
                    "--extra",
                    f"Pairing block: {block + 1}",
                    "--extra",
                    f"Arm: {arm}",
                ],
                cwd=Path.cwd(),
                env=convert_env,
                log_path=output_dir
                / f"convert-{sequence_number:02d}-{arm}.txt",
            )
            manifest["measurements"].append(
                {
                    "sequence": sequence_number,
                    "block": block + 1,
                    "position": position + 1,
                    "arm": arm,
                    "results": result_path.name,
                    "log": combined_log.name,
                    "health_before": before,
                    "health_after": after,
                }
            )

    manifest_path = output_dir / "paired-manifest.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {len(manifest['measurements'])} measurements to {manifest_path}")


if __name__ == "__main__":
    main()
