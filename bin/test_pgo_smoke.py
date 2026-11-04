"""The smoke exercises the same instrumented link as the application build and stays strict."""
import ast
import inspect
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import pgo_smoke as smoke


def record(name, counts):
    return f"  {name}:\n    Hash: 0x123\n    Counters: {len(counts)}\n    Block counts: {counts}\n"


def rustc_calls():
    """The smoke's compiler invocations, keyed by the output they produce."""
    calls = {}
    for node in ast.walk(ast.parse(inspect.getsource(smoke))):
        if isinstance(node, ast.Call) and ast.unparse(node.func).startswith("subprocess.") \
                and node.args and ast.unparse(node.args[0]).startswith("[*command"):
            arguments = ast.unparse(node.args[0])
            calls["generate" if "profile-generate" in arguments else "use"] = arguments
    return calls


class SmokeTests(unittest.TestCase):
    def test_smoke_resolves_profdata_with_the_compiler_it_records(self):
        # The helper gates llvm-profdata on rustc's LLVM release; the smoke must hand it
        # the same `rustc -vV` text it records, or every CI probe job fails before building.
        calls = [node for node in ast.walk(ast.parse(inspect.getsource(smoke)))
                 if isinstance(node, ast.Call) and ast.unparse(node.func) == "pgo.profdata_tool"]
        self.assertEqual([ast.unparse(arg) for call in calls for arg in call.args], ["compiler"])
        self.assertEqual(len(inspect.signature(smoke.pgo.profdata_tool).parameters), 1)

    def test_exact_counts_from_both_training_processes(self):
        profile = record("main", [2]) + record("module;_RNvCs5JAvRntY6Pu_5probe4work", [21001, 3001])
        smoke.check_probe_counts(profile)
        for changed in (profile.replace("21001", "21000"), profile.replace("3001", "3000"),
                        profile.replace("[2]", "[1]"), record("main", [2]), ""):
            with self.subTest(profile=changed), self.assertRaises(RuntimeError):
                smoke.check_probe_counts(changed)

    def test_only_the_instrumented_build_links_through_the_apple_script(self):
        # pgo.py adds the link flag to its generate stage alone; the smoke must mirror
        # that, or a dead-stripped smoke probe would report Darwin's missing records
        # that the application build no longer sees (or the reverse).
        calls = rustc_calls()
        self.assertEqual(sorted(calls), ["generate", "use"])
        self.assertIn("*pgo.instrumented_link_flags(target)", calls["generate"])
        self.assertNotIn("instrumented_link_flags", calls["use"])
        self.assertIn("-Cllvm-args=-pgo-warn-missing-function", calls["use"])
        self.assertNotIn("instrumented_link_flags", ast.unparse(ast.parse(inspect.getsource(smoke)))
                         .split("profile-use", 1)[1])

    def test_output_directory_may_exist_but_not_hold_a_previous_run(self):
        class Reached(Exception):
            pass

        def compiler(*command):
            raise Reached(command)

        with tempfile.TemporaryDirectory() as temp, patch.object(smoke.pgo, "native_target", lambda target=None: "x86_64-unknown-linux-gnu"), \
                patch.object(smoke.pgo, "capture", compiler):
            fresh = Path(temp)/"new"
            with self.assertRaises(Reached):
                smoke.smoke(fresh)
            self.assertTrue(fresh.is_dir())
            with self.assertRaises(Reached):
                smoke.smoke(fresh)
            (fresh/"result.json").write_text("{}")
            with self.assertRaisesRegex(RuntimeError, "not empty"):
                smoke.smoke(fresh)

    def test_any_profile_use_diagnostic_fails_the_smoke(self):
        # No compiler, target or helper name is exempt: the smoke has no review list.
        source = inspect.getsource(smoke)
        for forbidden in ("REVIEWED", "lang_start", "MAC_STARTUP", "reviewed"):
            self.assertNotIn(forbidden, source)
        self.assertIn('pgo.require(not built.stderr.strip(), f"Unexpected profile-use diagnostics', source)


if __name__ == "__main__":
    unittest.main()
