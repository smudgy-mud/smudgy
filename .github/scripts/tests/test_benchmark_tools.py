from __future__ import annotations

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path
from unittest.mock import patch

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

    def test_procedure_controls_survive_the_broad_change_budget(self) -> None:
        result = self.selector.select(self.config, ["bench/Cargo.toml"])
        self.assertTrue({"script_dispatch", "procedures", "interop_ops"}.issubset(
            result["criterion_targets"]
        ))
        self.assertNotIn("procedure_calls", result["criterion_targets"])
        self.assertEqual(result["candidate_only_criterion_targets"], ["procedure_calls"])

    def test_procedure_runtime_paths_are_covered(self) -> None:
        for path in (
            "core/src/session/runtime/action.rs",
            "core/src/session/runtime/dispatch.rs",
            "core/src/session/runtime/js/smudgy.ts",
            "core/src/session/runtime/procedure_calls.rs",
            "core/src/session/runtime/message_bus.rs",
        ):
            with self.subTest(path=path):
                result = self.selector.select(self.config, [path])
                self.assertTrue({"script_dispatch", "procedures"}.issubset(
                    result["criterion_targets"]
                ))
                self.assertEqual(result["uncovered_files"], [])
                self.assertEqual(result["candidate_only_criterion_targets"], ["procedure_calls"])

    def test_rpc_harness_changes_select_its_paired_controls(self) -> None:
        result = self.selector.select(self.config, ["bench/benches/procedure_calls.rs"])
        self.assertEqual(result["criterion_targets"], ["runner_control", "procedures"])
        self.assertEqual(result["candidate_only_criterion_targets"], ["procedure_calls"])
        self.assertEqual(result["uncovered_files"], [])

    def test_unrelated_changes_do_not_select_rpc_timings(self) -> None:
        result = self.selector.select(self.config, ["core/src/session/connection.rs"])
        self.assertEqual(result["candidate_only_criterion_targets"], [])

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

    def test_extracted_native_hot_paths_still_select_paired_benchmarks(self) -> None:
        cases = {
            "protocol/src/telnet.rs": {"ingest"},
            "protocol/src/responders.rs": {"ingest"},
            "protocol/src/transcode.rs": {"ingest"},
            "protocol/src/vt.rs": {"ingest"},
            "protocol/src/sgr.rs": {"ingest"},
            "session_model/src/styled_line.rs": {"ingest", "terminal_buffer"},
            "ui_shared/src/terminal_buffer.rs": {"terminal_buffer"},
            "ui_shared/src/terminal_buffer/selection.rs": {"terminal_buffer"},
            "ui_shared/src/split_terminal_pane/terminal_pane/spans.rs": {"terminal_buffer"},
            "ui_shared/src/prefs.rs": {"terminal_buffer"},
        }
        for path, expected in cases.items():
            with self.subTest(path=path):
                result = self.selector.select(self.config, [path])
                self.assertTrue(expected.issubset(result["criterion_targets"]))
                self.assertTrue(result["has_benchmarks"])
                self.assertEqual(result["uncovered_files"], [])

        result = self.selector.select(self.config, ["protocol/src/vt.rs"])
        self.assertEqual(result["callgrind_targets"], ["ingest_callgrind"])

    def test_truncated_changed_file_list_is_an_explicit_gap(self) -> None:
        result = self.selector.select(
            self.config,
            ["docs/first.md"],
            expected_file_count=3_001,
        )
        self.assertEqual(result["coverage_status"], "gap")
        self.assertFalse(result["file_list_complete"])
        self.assertTrue(result["scope_gaps"])


class CandidateOnlyTests(unittest.TestCase):
    def test_rpc_timings_are_prebuilt_but_excluded_from_paired_measurements(self) -> None:
        runner = load_script("run-paired-criterion.py")
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            for arm in ("baseline", "candidate"):
                (directory / arm).mkdir()
                (directory / arm / "Cargo.toml").touch()
            output = directory / "output"
            commands = []

            def fake_run(command, *, cwd, env, log_path):
                commands.append((command, cwd))
                log_path.write_text("ok", encoding="utf-8")
                if "SMUDGY_BENCH_AVAILABILITY_REPORT" in env:
                    Path(env["SMUDGY_BENCH_AVAILABILITY_REPORT"]).write_text(
                        '{"available":true}', encoding="utf-8"
                    )
                if "--output" in command:
                    Path(command[command.index("--output") + 1]).write_text(
                        "[]", encoding="utf-8"
                    )

            argv = [
                "run-paired-criterion.py", "--baseline-dir", str(directory / "baseline"),
                "--candidate-dir", str(directory / "candidate"),
                "--target-root", str(directory / "targets"), "--output-dir", str(output),
                "--converter", str(SCRIPTS / "criterion-to-benchmark-json.py"),
                "--target", "procedures", "--candidate-only-target", "procedure_calls",
                "--settle-seconds", "0",
            ]
            with patch.object(sys, "argv", argv), patch.object(runner, "run", fake_run), \
                    patch.object(runner.time, "sleep"), patch.object(runner, "health_snapshot", return_value={}):
                runner.main()
            manifest = json.loads((output / "paired-manifest.json").read_text())
            self.assertEqual(manifest["targets"], ["procedures"])
            self.assertEqual(len(manifest["measurements"]), 8)
            self.assertTrue(all("candidate-only" not in m["results"] for m in manifest["measurements"]))
            self.assertTrue((output / "candidate-only-procedure_calls.json").is_file())
            rpc = [(command, cwd) for command, cwd in commands if "procedure_calls" in command]
            self.assertEqual(len(rpc), 2)
            self.assertTrue(all(cwd == directory / "candidate" for _, cwd in rpc))
            self.assertIn("--no-run", rpc[0][0])
            first_measurement = next(i for i, (cmd, _) in enumerate(commands) if "--noplot" in cmd)
            self.assertTrue(all("--no-run" in cmd for cmd, _ in commands[:first_measurement]))

    def run_with_availability(self, report: str | None) -> tuple[list, bool]:
        runner = load_script("run-paired-criterion.py")
        commands = []
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            stale_result = directory / "candidate-only-procedure_calls.json"
            stale_result.write_text("stale", encoding="utf-8")

            def fake_run(command, *, cwd, env, log_path):
                commands.append(command)
                if report is not None and "SMUDGY_BENCH_AVAILABILITY_REPORT" in env:
                    Path(env["SMUDGY_BENCH_AVAILABILITY_REPORT"]).write_text(report, encoding="utf-8")

            with patch.object(runner, "run", fake_run), patch.object(runner.time, "sleep"), \
                    patch.object(runner, "health_snapshot", return_value={}):
                runner.run_candidate_only(
                    targets=["procedure_calls"], source=directory, target_dir=directory / "target",
                    output_dir=directory, converter=SCRIPTS / "criterion-to-benchmark-json.py",
                    cpu_list=None, settle_seconds=0, base_env={},
                )
            return commands, stale_result.exists()

    def test_unavailable_api_skips_conversion_and_removes_stale_estimates(self) -> None:
        commands, stale = self.run_with_availability('{"available":false}')
        self.assertEqual(len(commands), 1)
        self.assertFalse(stale)

    def test_missing_or_invalid_availability_is_a_failure(self) -> None:
        for report in (None, "broken", '{"available":1}', "[]"):
            with self.subTest(report=report), self.assertRaises(SystemExit):
                self.run_with_availability(report)


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

    def test_candidate_only_cases_cannot_enter_a_paired_comparison(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            manifest = self.write_paired_fixture(directory)
            result_path = directory / "result-2.json"
            results = json.loads(result_path.read_text())
            results.append({"name": "procedure_calls/new_api", "unit": "ns/iter", "value": 10})
            result_path.write_text(json.dumps(results), encoding="utf-8")
            process = subprocess.run(
                [sys.executable, str(SCRIPTS / "aggregate-paired-criterion.py"),
                 "--manifest", str(manifest), "--output", str(directory / "aggregate.json")],
                capture_output=True, text=True, check=False,
            )
            self.assertNotEqual(process.returncode, 0)
            self.assertIn("benchmark set changed", process.stderr)

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

    def test_candidate_only_suites_are_forwarded_to_the_runner(self) -> None:
        self.assertIn("'.candidate_only_criterion_targets[]'", self.workflow)
        self.assertIn('--candidate-only-target "${target}"', self.workflow)

    def test_artifact_failure_is_propagated_to_reporter(self) -> None:
        self.assertIn("id: measurements", self.workflow)
        self.assertIn("steps.measurements.outcome", self.workflow)
        self.assertIn("report_status=artifact_failure", self.workflow)

    def test_comment_lookup_is_paginated_and_slurped(self) -> None:
        self.assertIn("--paginate", self.comment_lookup())
        self.assertIn("--slurp", self.comment_lookup())

    def comment_lookup(self) -> str:
        return textwrap.dedent(self.workflow[
            self.workflow.index("comment_id=$(gh api"):
            self.workflow.index('if [[ -n "${comment_id}"')
        ])

    def run_comment_lookup(
        self, pages: list, *, api_status: int = 0
    ) -> subprocess.CompletedProcess[str]:
        bash = shutil.which("bash")
        if not bash or not shutil.which("jq"):
            self.skipTest("comment lookup integration tests require bash and jq")
        with tempfile.TemporaryDirectory() as temp:
            fixture = Path(temp) / "pages.json"
            fixture.write_text(json.dumps(pages), encoding="utf-8")
            # Reject unsupported gh arguments, then let real jq process all pages.
            script = """
set -euo pipefail
gh() {
  [[ "$#" == 4 && "$1" == api && "$2" == --paginate && "$3" == --slurp &&
     "$4" == "repos/smudgy-mud/smudgy/issues/246/comments?per_page=100" ]] || return 64
  [[ "${API_STATUS}" == 0 ]] || return "${API_STATUS}"
  cat "${COMMENT_PAGES}"
}
""" + self.comment_lookup() + '\nprintf "%s" "${comment_id}"\n'
            return subprocess.run(
                [bash, "-c", script], capture_output=True, text=True,
                env=os.environ | {
                    "GITHUB_REPOSITORY": "smudgy-mud/smudgy", "PR_NUMBER": "246",
                    "COMMENT_PAGES": fixture.as_posix(), "API_STATUS": str(api_status),
                },
            )

    def test_comment_lookup_finds_first_matching_comment_across_pages(self) -> None:
        marker = "<!-- smudgy-benchmark-comparison -->"
        result = self.run_comment_lookup([
            [{"id": 1, "body": None}, {"id": 2, "body": "unrelated"}],
            [{"id": 3}, {"id": 4, "body": f"{marker}\nreport"},
             {"id": 5, "body": marker}],
            [{"id": 6, "body": marker}],
        ])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "4")

    def test_comment_lookup_returns_empty_when_no_comment_matches(self) -> None:
        for pages in ([[]], [[{"id": 1, "body": "unrelated"}], []]):
            with self.subTest(pages=pages):
                result = self.run_comment_lookup(pages)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "")

    def test_comment_lookup_propagates_api_failure(self) -> None:
        result = self.run_comment_lookup([[]], api_status=7)
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
