#!/usr/bin/env python3
"""Aggregate replicated Criterion arms and classify PR performance signals."""

from __future__ import annotations

import argparse
import json
import math
from collections import defaultdict
from pathlib import Path
from typing import Any


def read_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot read {path}: {error}") from error


def geometric_mean(values: list[float]) -> float:
    if not values or any(value <= 0 or not math.isfinite(value) for value in values):
        raise SystemExit(f"cannot take geometric mean of {values!r}")
    return math.exp(sum(math.log(value) for value in values) / len(values))


def percent_ratio(candidate: float, baseline: float) -> float:
    return (candidate / baseline - 1.0) * 100.0


def replicate_spread(values: list[float]) -> float:
    if not values or any(value <= 0 or not math.isfinite(value) for value in values):
        raise SystemExit(f"cannot calculate replicate spread of {values!r}")
    return (max(values) / min(values) - 1.0) * 100.0


def manifest_child(manifest_path: Path, value: Any) -> Path:
    if not isinstance(value, str) or not value:
        raise SystemExit(f"invalid artifact path in {manifest_path}: {value!r}")
    path = Path(value)
    return path if path.is_absolute() else manifest_path.parent / path


def read_measurements(path: Path) -> dict[str, tuple[str, float]]:
    raw = read_json(path)
    if not isinstance(raw, list) or not raw:
        raise SystemExit(f"expected a non-empty measurement array in {path}")
    result: dict[str, tuple[str, float]] = {}
    for item in raw:
        if not isinstance(item, dict):
            raise SystemExit(f"invalid measurement in {path}")
        name, unit, value = item.get("name"), item.get("unit"), item.get("value")
        if (
            not isinstance(name, str)
            or not isinstance(unit, str)
            or isinstance(value, bool)
            or not isinstance(value, (int, float))
            or value <= 0
            or not math.isfinite(value)
        ):
            raise SystemExit(f"invalid measurement in {path}: {item!r}")
        if name in result:
            raise SystemExit(f"duplicate benchmark {name!r} in {path}")
        result[name] = (unit, float(value))
    return result


def environment_health(
    manifest: dict[str, Any],
    *,
    max_steal_percent: float,
    max_load: float,
    max_runnable: int,
) -> dict[str, Any]:
    failures: list[str] = []
    warnings: list[str] = []
    max_observed_steal = 0.0
    max_observed_load = 0.0
    max_observed_runnable = 0
    governors: set[str] = set()

    for measurement in manifest["measurements"]:
        before = measurement.get("health_before", {})
        after = measurement.get("health_after", {})
        governors.update(before.get("governors", []))
        governors.update(after.get("governors", []))

        load = before.get("load", [])
        if load:
            max_observed_load = max(max_observed_load, float(load[0]))
        runnable = str(before.get("runnable", "")).split("/", maxsplit=1)[0]
        if runnable.isdigit():
            max_observed_runnable = max(max_observed_runnable, int(runnable))

        before_stat = before.get("proc_stat", {})
        after_stat = after.get("proc_stat", {})
        total = after_stat.get("total_ticks", 0) - before_stat.get("total_ticks", 0)
        steal = after_stat.get("steal_ticks", 0) - before_stat.get("steal_ticks", 0)
        if total > 0 and steal >= 0:
            steal_percent = steal / total * 100.0
            max_observed_steal = max(max_observed_steal, steal_percent)

    if governors and governors != {"performance"}:
        failures.append(
            "CPU governor was not consistently `performance`: "
            + ", ".join(sorted(governors))
        )
    if not governors:
        warnings.append("CPU governor telemetry was unavailable.")
    if max_observed_steal > max_steal_percent:
        failures.append(
            f"CPU steal reached {max_observed_steal:.3f}% "
            f"(limit {max_steal_percent:.3f}%)."
        )
    if max_observed_load > max_load:
        warnings.append(
            f"Pre-measurement load reached {max_observed_load:.2f} "
            f"(diagnostic threshold {max_load:.2f}); controls decide validity."
        )
    if max_observed_runnable > max_runnable:
        failures.append(
            f"Pre-measurement runnable tasks reached {max_observed_runnable} "
            f"(limit {max_runnable})."
        )

    return {
        "passed": not failures,
        "failures": failures,
        "warnings": warnings,
        "governors": sorted(governors),
        "max_steal_percent": max_observed_steal,
        "max_pre_measurement_load": max_observed_load,
        "max_pre_measurement_runnable": max_observed_runnable,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--screen-percent", type=float, default=5.0)
    parser.add_argument("--control-percent", type=float, default=3.0)
    parser.add_argument("--max-replicate-spread-percent", type=float, default=5.0)
    parser.add_argument("--max-steal-percent", type=float, default=0.25)
    parser.add_argument("--max-load", type=float, default=1.5)
    parser.add_argument("--max-runnable", type=int, default=2)
    args = parser.parse_args()

    for name in (
        "screen_percent",
        "control_percent",
        "max_replicate_spread_percent",
        "max_steal_percent",
        "max_load",
    ):
        value = getattr(args, name)
        if value < 0 or not math.isfinite(value):
            raise SystemExit(f"--{name.replace('_', '-')} must be finite and non-negative")
    if args.max_runnable < 1:
        raise SystemExit("--max-runnable must be positive")

    manifest_path = args.manifest.resolve()
    manifest = read_json(manifest_path)
    if not isinstance(manifest, dict) or manifest.get("schema_version") != 1:
        raise SystemExit("unsupported paired manifest")
    measurements = manifest.get("measurements")
    if not isinstance(measurements, list) or not measurements:
        raise SystemExit("paired manifest has no measurements")
    blocks = int(manifest.get("blocks", 0))
    if blocks < 2:
        raise SystemExit("at least two pairing blocks are required")

    values: dict[str, dict[int, dict[str, list[float]]]] = defaultdict(
        lambda: defaultdict(lambda: defaultdict(list))
    )
    units: dict[str, str] = {}
    expected_names: set[str] | None = None
    for entry in measurements:
        arm = entry.get("arm")
        block = entry.get("block")
        if arm not in {"baseline", "candidate"} or not isinstance(block, int):
            raise SystemExit(f"invalid manifest entry: {entry!r}")
        result = read_measurements(manifest_child(manifest_path, entry.get("results")))
        names = set(result)
        if expected_names is None:
            expected_names = names
        elif names != expected_names:
            missing = sorted(expected_names - names)
            added = sorted(names - expected_names)
            raise SystemExit(
                f"benchmark set changed across measurements; missing={missing}, added={added}"
            )
        for name, (unit, value) in result.items():
            if name in units and units[name] != unit:
                raise SystemExit(f"unit changed for {name!r}")
            units[name] = unit
            values[name][block][arm].append(value)

    results: list[dict[str, Any]] = []
    for name in sorted(values):
        block_results: list[dict[str, float]] = []
        all_baseline: list[float] = []
        all_candidate: list[float] = []
        for block in range(1, blocks + 1):
            baseline = values[name][block]["baseline"]
            candidate = values[name][block]["candidate"]
            if len(baseline) != 2 or len(candidate) != 2:
                raise SystemExit(
                    f"{name!r} block {block} must contain two values per arm"
                )
            base_mean = geometric_mean(baseline)
            candidate_mean = geometric_mean(candidate)
            baseline_spread = replicate_spread(baseline)
            candidate_spread = replicate_spread(candidate)
            block_results.append(
                {
                    "block": block,
                    "baseline": base_mean,
                    "candidate": candidate_mean,
                    "percent": percent_ratio(candidate_mean, base_mean),
                    "baseline_spread_percent": baseline_spread,
                    "candidate_spread_percent": candidate_spread,
                }
            )
            all_baseline.extend(baseline)
            all_candidate.extend(candidate)

        baseline_mean = geometric_mean(all_baseline)
        candidate_mean = geometric_mean(all_candidate)
        percent = percent_ratio(candidate_mean, baseline_mean)
        block_percents = [block["percent"] for block in block_results]
        same_positive = all(value > args.screen_percent for value in block_percents)
        same_negative = all(value < -args.screen_percent for value in block_percents)
        max_replicate_spread = max(
            max(
                block["baseline_spread_percent"],
                block["candidate_spread_percent"],
            )
            for block in block_results
        )
        noisy_replicates = (
            max_replicate_spread > args.max_replicate_spread_percent
        )
        is_control = name.startswith("runner_control/")

        if is_control:
            classification = (
                "control_stable"
                if all(abs(value) <= args.control_percent for value in block_percents)
                and abs(percent) <= args.control_percent
                and not noisy_replicates
                else "control_noisy"
            )
        elif noisy_replicates:
            classification = "inconclusive"
        elif same_positive:
            classification = "confirmed_slower"
        elif same_negative:
            classification = "confirmed_faster"
        elif abs(percent) > args.screen_percent or any(
            abs(value) > args.screen_percent for value in block_percents
        ):
            classification = "inconclusive"
        else:
            classification = "within_screen"

        results.append(
            {
                "name": name,
                "unit": units[name],
                "baseline": baseline_mean,
                "candidate": candidate_mean,
                "percent": percent,
                "block_spread_percent": max(block_percents) - min(block_percents),
                "max_replicate_spread_percent": max_replicate_spread,
                "classification": classification,
                "control": is_control,
                "blocks": block_results,
            }
        )

    health = environment_health(
        manifest,
        max_steal_percent=args.max_steal_percent,
        max_load=args.max_load,
        max_runnable=args.max_runnable,
    )
    noisy_controls = [
        result["name"]
        for result in results
        if result["classification"] == "control_noisy"
    ]
    if noisy_controls:
        health["passed"] = False
        health["failures"].append(
            "Wall-clock controls exceeded the paired noise limit: "
            + ", ".join(noisy_controls)
        )

    product = [result for result in results if not result["control"]]
    counts = {
        classification: sum(
            result["classification"] == classification for result in product
        )
        for classification in (
            "confirmed_slower",
            "confirmed_faster",
            "inconclusive",
            "within_screen",
        )
    }
    if not health["passed"]:
        verdict = "environment_inconclusive"
    elif counts["inconclusive"]:
        verdict = "mixed_inconclusive"
    elif counts["confirmed_slower"]:
        verdict = "confirmed_regression_signal"
    elif counts["confirmed_faster"]:
        verdict = "confirmed_improvement_signal"
    else:
        verdict = "no_confirmed_change"

    output = {
        "schema_version": 1,
        "method": {
            "blocks": blocks,
            "orders": ["ABBA", "BAAB"],
            "screen_percent": args.screen_percent,
            "control_percent": args.control_percent,
            "max_replicate_spread_percent": args.max_replicate_spread_percent,
            "estimator": "geometric mean of candidate/baseline within each block",
            "confirmation": (
                "both balanced blocks must independently cross the screen "
                "threshold in the same direction and each arm's within-block "
                "replicates must satisfy the spread limit"
            ),
        },
        "verdict": verdict,
        "environment": health,
        "counts": counts,
        "results": results,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2) + "\n", encoding="utf-8")
    print(f"aggregated {len(results)} cells; verdict={verdict}")


if __name__ == "__main__":
    main()
