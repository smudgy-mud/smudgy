#!/usr/bin/env python3
"""Extract deterministic instruction-count comparisons from Gungraun v6 JSON."""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
from typing import Any


def read_object(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise SystemExit(f"expected a JSON object in {path}")
    return value


def manifest_child(manifest_path: Path, value: Any) -> Path:
    if not isinstance(value, str) or not value:
        raise SystemExit(f"invalid artifact path in {manifest_path}: {value!r}")
    path = Path(value)
    return path if path.is_absolute() else manifest_path.parent / path


def metric_number(value: Any) -> float:
    if not isinstance(value, dict) or len(value) != 1:
        raise ValueError(f"invalid metric value: {value!r}")
    kind, number = next(iter(value.items()))
    if kind not in {"Int", "Float"} or isinstance(number, bool):
        raise ValueError(f"invalid metric value: {value!r}")
    result = float(number)
    if result < 0 or not math.isfinite(result):
        raise ValueError(f"invalid metric value: {value!r}")
    return result


def both_metrics(value: Any) -> tuple[float, float]:
    if not isinstance(value, dict) or "Both" not in value:
        raise ValueError(f"comparison has no new/old metrics: {value!r}")
    both = value["Both"]
    if isinstance(both, dict):
        new, old = both.get("left"), both.get("right")
    elif isinstance(both, list) and len(both) == 2:
        new, old = both
    else:
        raise ValueError(f"invalid Both metric pair: {both!r}")
    return metric_number(new), metric_number(old)


def instruction_comparison(summary: dict[str, Any], source: Path) -> dict[str, Any]:
    if str(summary.get("version")) != "6":
        raise SystemExit(f"unsupported Gungraun summary version in {source}")
    module_path = summary.get("module_path")
    if not isinstance(module_path, str) or not module_path:
        raise SystemExit(f"missing module_path in {source}")
    bench_id = summary.get("id")
    name = module_path if not bench_id else f"{module_path}/{bench_id}"

    profiles = summary.get("profiles")
    if isinstance(profiles, dict):
        profiles = profiles.get("0")
    if not isinstance(profiles, list):
        raise SystemExit(f"invalid profiles in {source}")

    for profile in profiles:
        if not isinstance(profile, dict) or profile.get("tool") != "Callgrind":
            continue
        total = profile.get("summaries", {}).get("total", {})
        tagged = total.get("summary", {})
        metrics = tagged.get("Callgrind") if isinstance(tagged, dict) else None
        if not isinstance(metrics, dict):
            continue
        instruction = metrics.get("Ir")
        if not isinstance(instruction, dict):
            continue
        try:
            candidate, baseline = both_metrics(instruction.get("metrics"))
        except ValueError as error:
            raise SystemExit(f"{source}: {error}") from error
        calculated = (candidate / baseline - 1.0) * 100.0 if baseline else math.inf
        diffs = instruction.get("diffs")
        reported = None
        if isinstance(diffs, dict):
            try:
                reported = float(diffs.get("diff_pct"))
            except (TypeError, ValueError):
                reported = None
        if reported is not None and math.isfinite(calculated):
            if not math.isclose(reported, calculated, rel_tol=1e-6, abs_tol=1e-6):
                raise SystemExit(
                    f"{source}: reported instruction delta {reported} "
                    f"does not match values ({calculated})"
                )
        return {
            "name": name,
            "metric": "Instructions",
            "unit": "instructions",
            "baseline": baseline,
            "candidate": candidate,
            "percent": calculated,
            "source": str(source),
        }
    raise SystemExit(f"no compared Callgrind instruction metric in {source}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--signal-percent", type=float, default=1.0)
    args = parser.parse_args()
    if args.signal_percent < 0 or not math.isfinite(args.signal_percent):
        raise SystemExit("--signal-percent must be finite and non-negative")

    manifest_path = args.manifest.resolve()
    manifest = read_object(manifest_path)
    if manifest.get("schema_version") != 1:
        raise SystemExit("unsupported Callgrind manifest")
    summaries = manifest.get("summaries")
    if not isinstance(summaries, list) or not summaries:
        raise SystemExit("Callgrind manifest has no summaries")

    results: list[dict[str, Any]] = []
    for path in summaries:
        source = manifest_child(manifest_path, path)
        result = instruction_comparison(read_object(source), source)
        result["source"] = path
        results.append(result)
    for result in results:
        if result["percent"] > args.signal_percent:
            result["classification"] = "slower"
        elif result["percent"] < -args.signal_percent:
            result["classification"] = "faster"
        else:
            result["classification"] = "stable"

    counts = {
        classification: sum(
            result["classification"] == classification for result in results
        )
        for classification in ("slower", "faster", "stable")
    }
    output = {
        "schema_version": 1,
        "framework": "Gungraun/Callgrind",
        "metric": "Instructions",
        "signal_percent": args.signal_percent,
        "counts": counts,
        "results": sorted(results, key=lambda item: abs(item["percent"]), reverse=True),
        "limitation": (
            "Deterministic instruction counts cover synchronous CPU work only; "
            "they are not wall-clock latency or throughput."
        ),
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2) + "\n", encoding="utf-8")
    print(f"aggregated {len(results)} Callgrind instruction comparisons")


if __name__ == "__main__":
    main()
