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
        self.target = "x86_64-pc-windows-msvc"
        self.identity = {"source": "same", "target": self.target, "files": {"source.rs": "digest"}}
        pgo.write(self.run/"run.json", {"identity": self.identity, "revision": "test", "target": self.target})
        self.patch_identity = patch.object(pgo, "identity", return_value=self.identity)
        self.patch_identity.start()
        self.addCleanup(self.patch_identity.stop)
        self.patch_target = patch.object(pgo, "native_target", side_effect=lambda target=None: target or self.target)
        self.patch_target.start()
        self.addCleanup(self.patch_target.stop)
        (self.run/"merged.profdata").write_bytes(b"profile")
        for stage in ("generate", "use"):
            directory = self.run/stage
            directory.mkdir()
            exe = directory/"smudgy.exe"
            exe.write_bytes(stage.encode())
            pgo.write(directory/"manifest.json", dict(target=self.target, exit_code=0, inputs_verified=True,
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
            pgo.write(self.run/"run.json", {"identity": before, "target": self.target})
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

    def test_native_targets_and_executable_names(self):
        self.patch_target.stop()
        for target, (platform, name) in pgo.TARGETS.items():
            with self.subTest(target=target), patch.object(pgo, "host_target", return_value=target), patch.object(pgo.sys, "platform", platform):
                self.assertEqual(pgo.native_target(), target)
                self.assertEqual(pgo.executable_name(target), name)
                other = next(t for t in pgo.TARGETS if t != target)
                with self.assertRaisesRegex(RuntimeError, "matching native"):
                    pgo.native_target(other)

    def test_profdata_uses_host_tool_and_platform_suffix(self):
        for target, (platform, _) in pgo.TARGETS.items():
            with self.subTest(target=target), patch.object(pgo, "host_target", return_value=target), patch.object(pgo.sys, "platform", platform), patch.object(pgo, "capture", return_value=str(self.run)):
                tool = pgo.profdata_tool()
                self.assertEqual(tool.parent.parent.name, target)
                self.assertEqual(tool.name, "llvm-profdata.exe" if platform == "win32" else "llvm-profdata")

    def test_portable_export_does_not_need_original_run(self):
        self.validation()
        pgo.export(self.run)
        path = self.run/"dist/manifest.json"
        manifest = pgo.read(path)
        manifest["run"] = "missing-original-run"
        manifest["executable"] = "missing-original-executable"
        pgo.write(path, manifest)
        with patch.object(pgo, "source_files", return_value=self.identity["files"]):
            verified = pgo.verify_artifact(path, self.target, portable=True)
            self.assertEqual(Path(verified["executable"]).read_bytes(), b"use")
            with self.assertRaisesRegex(RuntimeError, "target mismatch"):
                pgo.verify_artifact(path, "aarch64-apple-darwin", portable=True)
        with patch.object(pgo, "source_files", return_value={}):
            with self.assertRaisesRegex(RuntimeError, "Packaging source"):
                pgo.verify_artifact(path, self.target, portable=True)

    def test_portable_export_rejects_inconsistent_receipts(self):
        self.validation()
        pgo.export(self.run)
        path = self.run/"dist/manifest.json"
        original = pgo.read(path)
        for section, key, value in (("build", "target", "aarch64-apple-darwin"),
                                    ("build", "exit_code", 1),
                                    ("build", "profile_sha256", "changed")):
            with self.subTest(key=key), patch.object(pgo, "source_files", return_value=self.identity["files"]):
                changed = json.loads(json.dumps(original))
                changed[section][key] = value
                pgo.write(path, changed)
                with self.assertRaises(RuntimeError):
                    pgo.verify_artifact(path, self.target, portable=True)

    def test_native_pgo_target_and_archive_are_checked(self):
        archive = self.run/"v8.a"
        binding = self.run/"binding.rs"
        profile = self.run/"native.profdata"
        for path in (archive, binding, profile):
            path.write_bytes(path.name.encode())
        manifest_path = self.run/"native.json"
        manifest = dict(schema=1, stage="native-pgo-use", target=self.target,
                        compiler="clang test", source_sha256="native-source-digest",
                        archive={"sha256":pgo.sha(archive)}, bindings={"sha256":pgo.sha(binding)},
                        profile={"path":str(profile), "sha256":pgo.sha(profile)})
        pgo.write(manifest_path, manifest)
        with patch.dict(pgo.os.environ, {"RUSTY_V8_ARCHIVE":str(archive),
                                        "RUSTY_V8_SRC_BINDING_PATH":str(binding),
                                        "SMUDGY_NATIVE_PGO_MANIFEST":str(manifest_path)}, clear=True):
            self.assertIn("native_pgo", pgo.native_inputs(self.target))
            with self.assertRaisesRegex(RuntimeError, "target differs"):
                pgo.native_inputs("aarch64-apple-darwin")
            archive.write_bytes(b"replaced")
            with self.assertRaisesRegex(RuntimeError, "archive differs"):
                pgo.native_inputs(self.target)

    def test_unix_export_retains_executable_mode(self):
        self.validation()
        for target in ("x86_64-unknown-linux-gnu", "aarch64-apple-darwin"):
            with self.subTest(target=target):
                config = pgo.read(self.run/"run.json")
                config["target"] = target
                self.identity["target"] = target
                config["identity"] = self.identity
                pgo.write(self.run/"run.json", config)
                built = pgo.read(self.run/"use/manifest.json")
                built["target"] = target
                pgo.write(self.run/"use/manifest.json", built)
                if (self.run/"dist").exists():
                    import shutil
                    shutil.rmtree(self.run/"dist")
                original = self.run/"use/smudgy.exe"
                original.chmod(0o755)
                pgo.export(self.run)
                self.assertEqual((self.run/"dist/smudgy").read_bytes(), b"use")
                if sys.platform != "win32":
                    self.assertEqual((self.run/"dist/smudgy").stat().st_mode & 0o777, 0o755)

    def test_mismatched_build_target_cannot_export(self):
        self.validation()
        built = pgo.read(self.run/"use/manifest.json")
        built["target"] = "aarch64-apple-darwin"
        pgo.write(self.run/"use/manifest.json", built)
        with self.assertRaisesRegex(RuntimeError, "Build target"):
            pgo.export(self.run)
        self.assertFalse((self.run/"dist").exists())


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
