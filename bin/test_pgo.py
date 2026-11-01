"""Failure-path tests for the PGO artifact and workload gates; no Rust build."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import sys
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("pgo", Path(__file__).with_name("pgo.py"))
pgo = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pgo)


class PgoTests(unittest.TestCase):
    def test_command_output_uses_utf8_on_windows(self):
        self.assertEqual(pgo.capture(sys.executable, "-c", "import sys; sys.stdout.buffer.write('世界'.encode('utf-8'))"), "世界")

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.run = Path(self.temp.name)
        pgo.write(self.run/"run.json", {"identity": {"source": "same"}, "revision": "test"})
        self.patch_identity = patch.object(pgo, "identity", return_value={"source": "same"})
        self.patch_identity.start()
        self.addCleanup(self.patch_identity.stop)
        (self.run/"merged.profdata").write_bytes(b"profile")
        for stage in ("generate", "use"):
            directory = self.run/stage
            directory.mkdir()
            exe = directory/"smudgy.exe"
            exe.write_bytes(stage.encode())
            pgo.write(directory/"manifest.json", dict(exit_code=0, inputs_verified=True,
                executable=str(exe), executable_sha256=pgo.sha(exe), pgo_diagnostics=[],
                profile_sha256=pgo.sha(self.run/"merged.profdata") if stage == "use" else None))

    def validation(self, categories=None):
        output = self.run/"validation/test"
        output.mkdir(parents=True)
        pgo.write(output/"manifest.json", dict(exit_code=0, verification_exit_code=0,
            executable_sha256=pgo.sha(self.run/"use/smudgy.exe"),
            scenario={"categories": categories or ["replay", "captures", "heldout", "interactive"]}))

    def test_export_verifies_and_preserves_exact_artifact(self):
        self.validation()
        pgo.export(self.run)
        manifest = pgo.verify_artifact(self.run/"dist/manifest.json")
        self.assertEqual(Path(manifest["executable"]).read_bytes(), b"use")

    def test_changed_artifact_is_rejected(self):
        self.validation()
        pgo.export(self.run)
        (self.run/"dist/smudgy.exe").write_bytes(b"replaced")
        with self.assertRaisesRegex(RuntimeError, "hash mismatch"):
            pgo.verify_artifact(self.run/"dist/manifest.json")

    def test_changed_profile_cannot_be_exported(self):
        self.validation()
        (self.run/"merged.profdata").write_bytes(b"another profile")
        with self.assertRaisesRegex(RuntimeError, "Profile changed"):
            pgo.export(self.run)

    def test_missing_heldout_validation_cannot_export(self):
        self.validation(["replay", "captures", "interactive"])
        with self.assertRaisesRegex(RuntimeError, "held-out"):
            pgo.export(self.run)
        self.assertFalse((self.run/"dist").exists())

    def test_failed_validation_cannot_export(self):
        self.validation()
        record = pgo.read(self.run/"validation/test/manifest.json")
        record["verification_exit_code"] = 1
        pgo.write(self.run/"validation/test/manifest.json", record)
        with self.assertRaisesRegex(RuntimeError, "failed workload"):
            pgo.export(self.run)

    def test_compiled_notices_are_part_of_input_identity(self):
        # The settings window embeds this generated file with include_str!.
        # Regenerating it between training and use must invalidate the profile.
        self.patch_identity.stop()
        notice = self.run/"THIRD-PARTY-NOTICES.md"
        notice.write_text("original embedded notices", encoding="utf-8")

        def capture(*command):
            if command[0] == "git":
                return "THIRD-PARTY-NOTICES.md"
            if command[:2] == ("cargo", "metadata"):
                return json.dumps({"packages": []})
            return "test toolchain"

        with (
            patch.object(pgo, "ROOT", self.run),
            patch.object(pgo, "capture", capture),
            patch.dict(pgo.os.environ, {
                "CARGO_HOME": str(self.run), "HOME": str(self.run), "USERPROFILE": str(self.run),
            }, clear=True),
        ):
            before = pgo.identity()
            pgo.write(self.run/"run.json", {"identity": before})
            notice.write_text("regenerated embedded notices", encoding="utf-8")
            self.assertNotEqual(before["files"], pgo.identity()["files"])
            with self.assertRaisesRegex(RuntimeError, "changed"):
                pgo.build(self.run, "use")

    def test_changed_source_cannot_build_with_old_profile(self):
        with patch.object(pgo, "identity", return_value={"source": "different"}):
            with self.assertRaisesRegex(RuntimeError, "changed"):
                pgo.build(self.run, "use")

    def test_missing_training_categories_cannot_merge(self):
        output = self.run/"training/test"
        output.mkdir(parents=True)
        pgo.write(output/"manifest.json", dict(exit_code=0, verification_exit_code=0,
            executable_sha256=pgo.sha(self.run/"generate/smudgy.exe"),
            scenario={"categories": ["replay"]}, raw={}))
        with self.assertRaisesRegex(RuntimeError, "interactive coverage"):
            pgo.merge(self.run)

    def test_abnormal_exit_is_not_verified_or_accepted(self):
        scenario = self.run/"scenario.json"
        pgo.write(scenario, {"verify": ["must-not-run"], "categories": ["replay"]})
        with patch.object(pgo.subprocess, "Popen") as launch, patch.object(pgo.subprocess, "run") as verify:
            launch.return_value.pid = 123
            launch.return_value.wait.return_value = 1
            with self.assertRaisesRegex(RuntimeError, "did not exit normally"):
                pgo.workload(self.run, "aborted", scenario, True)
            verify.assert_not_called()
        record = pgo.read(self.run/"training/aborted/manifest.json")
        self.assertEqual(record["exit_code"], 1)
        self.assertNotIn("verification_exit_code", record)


    def test_compiler_environment_overrides_cannot_bypass_identity(self):
        self.patch_identity.stop()
        for variable in pgo.COMPILER_OVERRIDES:
            with self.subTest(variable=variable), patch.dict(pgo.os.environ, {variable: "another-compiler"}, clear=True):
                with self.assertRaisesRegex(RuntimeError, "Unset build overrides"):
                    pgo.identity()


    def test_cargo_config_cannot_select_a_different_compiler(self):
        self.patch_identity.stop()
        directory = self.run/".cargo"
        directory.mkdir()
        contents = [
            '[build]\nrustc = "other-rustc"\n',
            '[build]\nrustc-wrapper = "wrapper"\n',
            '[build]\nrustc-workspace-wrapper = "wrapper"\n',
            '[env]\nCARGO_BUILD_RUSTC = { value = "other-rustc", force = true }\n',
            'include = ["another-compiler.toml"]\n',
        ]
        with patch.object(pgo, "ROOT", self.run), patch.dict(pgo.os.environ, {"CARGO_HOME": str(directory)}, clear=True):
            for content in contents:
                with self.subTest(content=content):
                    (directory/"config.toml").write_text(content, encoding="utf-8")
                    with self.assertRaisesRegex(RuntimeError, "Remove compiler|Flatten included"):
                        pgo.identity()


if __name__ == "__main__":
    unittest.main()
