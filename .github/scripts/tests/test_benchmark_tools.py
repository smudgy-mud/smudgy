from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
SCRIPTS = ROOT / ".github" / "scripts"


def load_script(name: str):
    path = SCRIPTS / name
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ScopeSelectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.selector = load_script("select-pr-benchmarks.py")
        cls.config = json.loads(
            (ROOT / ".github" / "benchmark-scope.json").read_text(encoding="utf-8")
        )

    def test_connection_change_has_partial_dual_coverage(self) -> None:
        result = self.selector.select(
            self.config, ["core/src/session/connection.rs"]
        )
        self.assertEqual(result["coverage_status"], "partial")
        self.assertEqual(
            result["criterion_targets"], ["runner_control", "ingest"]
        )
        self.assertEqual(result["callgrind_targets"], ["ingest_callgrind"])
        self.assertEqual(result["uncovered_files"], [])

    def test_documentation_change_needs_no_benchmark(self) -> None:
        result = self.selector.select(self.config, ["docs/performance.md"])
        self.assertEqual(result["coverage_status"], "not_applicable")
        self.assertFalse(result["has_benchmarks"])

    def test_unknown_rust_change_is_explicit_gap(self) -> None:
        result = self.selector.select(self.config, ["core/src/new_hot_path.rs"])
        self.assertEqual(result["coverage_status"], "gap")
        self.assertEqual(result["uncovered_files"], ["core/src/new_hot_path.rs"])
        self.assertFalse(result["has_benchmarks"])

    def test_broad_change_is_budgeted_and_explicitly_partial(self) -> None:
        result = self.selector.select(self.config, ["Cargo.lock"])
        self.assertEqual(result["coverage_status"], "partial")
        self.assertEqual(len(result["criterion_targets"]), 6)
        self.assertEqual(result["criterion_targets"][0], "runner_control")
        self.assertTrue(result["omitted_criterion_targets"])
        self.assertIn(
            "PR measurement budget",
            [entry["rule"] for entry in result["coverage"]],
        )

    def test_explicit_rules_override_generic_file_filters(self) -> None:
        for path in (
            "bench/logs/synthetic-long-session.log",
            "bench/item_names.txt",
            "script/src/runtime.ts",
        ):
            with self.subTest(path=path):
                result = self.selector.select(self.config, [path])
                self.assertTrue(result["has_benchmarks"])
                self.assertIn(path, result["performance_relevant_files"])
                self.assertNotIn(path, result["uncovered_files"])

    def test_repository_paths_select_the_benchmarks_that_exercise_them(self) -> None:
        cases = {
            "core/src/models/triggers.rs": "trigger_engine",
            "widgets/src/widget.rs": "interop_ops",
            "cloud/src/backends/local.rs": "mapper_scale",
            "ui/src/terminal_buffer.rs": "terminal_buffer",
            "ui/src/terminal_buffer/selection.rs": "terminal_buffer",
        }
        for path, target in cases.items():
            with self.subTest(path=path):
                result = self.selector.select(self.config, [path])
                self.assertIn(target, result["criterion_targets"])
                self.assertEqual(result["uncovered_files"], [])

    def test_truncated_changed_file_list_is_an_explicit_gap(self) -> None:
        result = self.selector.select(
            self.config,
            ["docs/first.md"],
            expected_file_count=3_001,
        )
        self.assertEqual(result["coverage_status"], "gap")
        self.assertFalse(result["file_list_complete"])
        self.assertTrue(result["scope_gaps"])


class AggregationTests(unittest.TestCase):
    def test_gungraun_baseline_name_is_cli_safe(self) -> None:
        runner = load_script("run-callgrind-comparison.py")
        self.assertRegex(runner.BASELINE_NAME, r"^[A-Za-z0-9_]+$")

    def write_paired_fixture(self, directory: Path) -> Path:
        measurements = []
        orders = [
            ["baseline", "candidate", "candidate", "baseline"],
            ["candidate", "baseline", "baseline", "candidate"],
        ]
        sequence = 0
        for block, order in enumerate(orders, start=1):
            for position, arm in enumerate(order, start=1):
                sequence += 1
                product = 110.0 if arm == "candidate" else 100.0
                control = 101.0 if arm == "candidate" else 100.0
                result_path = directory / f"result-{sequence}.json"
                result_path.write_text(
                    json.dumps(
                        [
                            {
                                "name": "ingest_pipeline/ansi_light",
                                "unit": "ns/iter",
                                "value": product,
                            },
                            {
                                "name": "runner_control/integer_mix",
                                "unit": "ns/iter",
                                "value": control,
                            },
                        ]
                    ),
                    encoding="utf-8",
                )
                measurements.append(
                    {
                        "sequence": sequence,
                        "block": block,
                        "position": position,
                        "arm": arm,
                        "results": result_path.name,
                        "health_before": {
                            "governors": ["performance"],
                            "load": [0.1, 0.1, 0.1],
                            "proc_stat": {
                                "total_ticks": sequence * 1_000,
                                "steal_ticks": 0,
                            },
                        },
                        "health_after": {
                            "governors": ["performance"],
                            "load": [0.2, 0.1, 0.1],
                            "proc_stat": {
                                "total_ticks": sequence * 1_000 + 500,
                                "steal_ticks": 0,
                            },
                        },
                    }
                )
        manifest = directory / "manifest.json"
        manifest.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "blocks": 2,
                    "targets": ["runner_control", "ingest"],
                    "measurements": measurements,
                }
            ),
            encoding="utf-8",
        )
        return manifest

    def test_replicated_change_is_confirmed(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            manifest = self.write_paired_fixture(directory)
            output = directory / "aggregate.json"
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPTS / "aggregate-paired-criterion.py"),
                    "--manifest",
                    str(manifest),
                    "--output",
                    str(output),
                ],
                check=True,
            )
            result = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(result["verdict"], "confirmed_regression_signal")
            self.assertTrue(result["environment"]["passed"])
            self.assertEqual(result["counts"]["confirmed_slower"], 1)

    def test_bimodal_replicates_are_inconclusive(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            manifest = self.write_paired_fixture(directory)
            for sequence in (3, 8):
                result_path = directory / f"result-{sequence}.json"
                measurements = json.loads(result_path.read_text(encoding="utf-8"))
                measurements[0]["value"] = 140.0
                result_path.write_text(json.dumps(measurements), encoding="utf-8")

            output = directory / "aggregate.json"
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPTS / "aggregate-paired-criterion.py"),
                    "--manifest",
                    str(manifest),
                    "--output",
                    str(output),
                ],
                check=True,
            )
            result = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(result["verdict"], "mixed_inconclusive")
            self.assertEqual(result["counts"]["confirmed_slower"], 0)
            self.assertEqual(result["counts"]["inconclusive"], 1)

    def test_noisy_replicates_with_small_average_are_inconclusive(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            manifest = self.write_paired_fixture(directory)
            for sequence, value in ((2, 90.0), (3, 110.0), (5, 90.0), (8, 110.0)):
                result_path = directory / f"result-{sequence}.json"
                measurements = json.loads(result_path.read_text(encoding="utf-8"))
                measurements[0]["value"] = value
                result_path.write_text(json.dumps(measurements), encoding="utf-8")

            output = directory / "aggregate.json"
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPTS / "aggregate-paired-criterion.py"),
                    "--manifest",
                    str(manifest),
                    "--output",
                    str(output),
                ],
                check=True,
            )
            result = json.loads(output.read_text(encoding="utf-8"))
            product = next(item for item in result["results"] if not item["control"])
            self.assertLess(abs(product["percent"]), 5.0)
            self.assertEqual(product["classification"], "inconclusive")
            self.assertEqual(result["verdict"], "mixed_inconclusive")

    def test_opposite_threshold_crossings_are_inconclusive(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            manifest = self.write_paired_fixture(directory)
            for sequence, value in ((2, 108.0), (3, 108.0), (5, 92.0), (8, 92.0)):
                result_path = directory / f"result-{sequence}.json"
                measurements = json.loads(result_path.read_text(encoding="utf-8"))
                measurements[0]["value"] = value
                result_path.write_text(json.dumps(measurements), encoding="utf-8")

            output = directory / "aggregate.json"
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPTS / "aggregate-paired-criterion.py"),
                    "--manifest",
                    str(manifest),
                    "--output",
                    str(output),
                ],
                check=True,
            )
            result = json.loads(output.read_text(encoding="utf-8"))
            product = next(item for item in result["results"] if not item["control"])
            self.assertEqual(
                [round(block["percent"]) for block in product["blocks"]],
                [8, -8],
            )
            self.assertEqual(product["classification"], "inconclusive")
            self.assertEqual(result["verdict"], "mixed_inconclusive")

    def test_callgrind_case_sets_must_be_identical(self) -> None:
        runner = load_script("run-callgrind-comparison.py")
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            baseline = root / "baseline"
            candidate = root / "candidate"
            for case in ("suite/case_a", "suite/case_b"):
                case_dir = baseline / case
                case_dir.mkdir(parents=True)
                (case_dir / "callgrind.case.out.base@pr_main").touch()
            for case in ("suite/case_a", "suite/case_b"):
                case_dir = candidate / case
                case_dir.mkdir(parents=True)
                (case_dir / "summary.json").touch()

            baseline_cases = runner.case_ids(
                baseline, "*.out.base@pr_main"
            )
            candidate_cases = runner.case_ids(candidate, "summary.json")
            runner.require_identical_case_sets(baseline_cases, candidate_cases)

            (candidate / "suite/case_b/summary.json").unlink()
            candidate_cases = runner.case_ids(candidate, "summary.json")
            with self.assertRaisesRegex(SystemExit, "missing=.*case_b"):
                runner.require_identical_case_sets(
                    baseline_cases, candidate_cases
                )

    def test_gungraun_v6_instruction_summary_is_extracted(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            summary = directory / "summary.json"
            summary.write_text(
                json.dumps(
                    {
                        "version": "6",
                        "module_path": "ingest_callgrind::ingest::ingest_pipeline",
                        "id": "ansi_light",
                        "profiles": [
                            {
                                "tool": "Callgrind",
                                "summaries": {
                                    "total": {
                                        "summary": {
                                            "Callgrind": {
                                                "Ir": {
                                                    "diffs": {
                                                        "diff_pct": "2.0",
                                                        "factor": "1.02",
                                                    },
                                                    "metrics": {
                                                        "Both": [
                                                            {"Int": 1020},
                                                            {"Int": 1000},
                                                        ]
                                                    },
                                                }
                                            }
                                        }
                                    }
                                },
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            manifest = directory / "manifest.json"
            manifest.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "summaries": [summary.name],
                    }
                ),
                encoding="utf-8",
            )
            output = directory / "callgrind.json"
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPTS / "aggregate-callgrind.py"),
                    "--manifest",
                    str(manifest),
                    "--output",
                    str(output),
                ],
                check=True,
            )
            result = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(result["counts"]["slower"], 1)
            self.assertEqual(result["results"][0]["candidate"], 1020.0)


class ReportTests(unittest.TestCase):
    def test_report_distinguishes_confirmation_and_coverage(self) -> None:
        reporter = load_script("compare-benchmark-json.py")
        report = reporter.render(
            paired={
                "verdict": "confirmed_regression_signal",
                "method": {
                    "blocks": 2,
                    "screen_percent": 5.0,
                    "control_percent": 3.0,
                },
                "counts": {
                    "confirmed_slower": 1,
                    "confirmed_faster": 0,
                    "inconclusive": 0,
                    "within_screen": 0,
                },
                "environment": {"passed": True, "warnings": []},
                "results": [
                    {
                        "name": "ingest_pipeline/ansi_light",
                        "unit": "ns/iter",
                        "baseline": 100.0,
                        "candidate": 110.0,
                        "percent": 10.0,
                        "classification": "confirmed_slower",
                        "control": False,
                        "blocks": [{"percent": 9.0}, {"percent": 11.0}],
                    }
                ],
            },
            scope={
                "coverage_status": "partial",
                "criterion_targets": ["runner_control", "ingest"],
                "callgrind_targets": ["ingest_callgrind"],
                "coverage": [
                    {
                        "limitation": "Does not model socket readiness scheduling."
                    }
                ],
                "uncovered_files": [],
            },
            callgrind=None,
            repository="smudgy-mud/smudgy",
            baseline_sha="a" * 40,
            candidate_sha="b" * 40,
            pr_number=1,
            run_url="https://example.invalid/run",
            max_rows=20,
        )
        self.assertIn("Outcome: Confirmed regression signal", report)
        self.assertIn("partial targeted coverage", report)
        self.assertIn("Does not model socket readiness scheduling", report)
        self.assertIn("Block 1", report)
        self.assertIn("Max replicate spread", report)

    def test_report_supports_deterministic_only_scope(self) -> None:
        reporter = load_script("compare-benchmark-json.py")
        report = reporter.render(
            paired=None,
            scope={
                "coverage_status": "direct",
                "criterion_targets": [],
                "callgrind_targets": ["ingest_callgrind"],
                "coverage": [],
                "uncovered_files": [],
            },
            callgrind={
                "signal_percent": 1.0,
                "counts": {"slower": 0, "faster": 0, "stable": 1},
                "results": [
                    {
                        "name": "ingest",
                        "baseline": 1000,
                        "candidate": 1001,
                        "percent": 0.1,
                        "classification": "stable",
                    }
                ],
                "limitation": "Instruction counts are not wall-clock latency.",
            },
            repository="smudgy-mud/smudgy",
            baseline_sha="a" * 40,
            candidate_sha="b" * 40,
            pr_number=1,
            run_url="https://example.invalid/run",
            max_rows=20,
        )
        self.assertIn("deterministic CPU signal only", report)
        self.assertIn("Deterministic CPU counters", report)
        self.assertNotIn("no relevant benchmark coverage", report)

    def test_report_replaces_stale_results_when_execution_fails(self) -> None:
        reporter = load_script("compare-benchmark-json.py")
        report = reporter.render(
            paired={
                "verdict": "confirmed_regression_signal",
                "method": {},
                "counts": {},
                "environment": {},
                "results": [],
            },
            scope={
                "coverage_status": "direct",
                "criterion_targets": ["runner_control", "ingest"],
                "callgrind_targets": [],
                "coverage": [],
                "uncovered_files": [],
            },
            callgrind=None,
            repository="smudgy-mud/smudgy",
            baseline_sha="a" * 40,
            candidate_sha="b" * 40,
            pr_number=1,
            run_url="https://example.invalid/run",
            max_rows=20,
            benchmark_status="failure",
        )
        self.assertIn("benchmark execution failure", report)
        self.assertIn("no performance conclusion", report)
        self.assertNotIn("Confirmed regression signal", report)

    def test_report_treats_artifact_download_failure_as_infrastructure(self) -> None:
        reporter = load_script("compare-benchmark-json.py")
        report = reporter.render(
            paired=None,
            scope={
                "coverage_status": "direct",
                "criterion_targets": ["runner_control", "ingest"],
                "callgrind_targets": [],
                "coverage": [],
                "uncovered_files": [],
                "has_benchmarks": True,
            },
            callgrind=None,
            repository="smudgy-mud/smudgy",
            baseline_sha="a" * 40,
            candidate_sha="b" * 40,
            pr_number=1,
            run_url="https://example.invalid/run",
            max_rows=20,
            benchmark_status="artifact_failure",
        )
        self.assertIn("benchmark artifact download failure", report)
        self.assertIn("no performance conclusion", report)
        self.assertNotIn("no relevant benchmark coverage", report)


class WorkflowDefinitionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.workflow = (ROOT / ".github" / "workflows" / "benchmark.yml").read_text(
            encoding="utf-8"
        )

    def test_pr_file_count_is_passed_to_scope_selector(self) -> None:
        self.assertIn("changed_file_count=$(jq -r .changed_files", self.workflow)
        self.assertIn("--expected-file-count", self.workflow)

    def test_artifact_failure_is_propagated_to_reporter(self) -> None:
        self.assertIn("id: measurements", self.workflow)
        self.assertIn("steps.measurements.outcome", self.workflow)
        self.assertIn("report_status=artifact_failure", self.workflow)

    def test_comment_lookup_is_paginated_and_slurped(self) -> None:
        lookup = self.workflow[
            self.workflow.index("comment_id=$(gh api"):
            self.workflow.index('if [[ -n "${comment_id}"')
        ]
        self.assertIn("--paginate", lookup)
        self.assertIn("--slurp", lookup)


if __name__ == "__main__":
    unittest.main()
