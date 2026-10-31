"""Failure-path tests for the PGO artifact and workload gates; no Rust build."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import sys
from unittest.mock import Mock, patch

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
        self.training_record = dict(exit_code=0, verification_exit_code=0, scenario_sha256="trained-scenario",
                                    inputs={"C:/workloads/corpus.log": "trained-corpus"},
                                    scenario={"categories": ["replay", "captures", "interactive"]},
                                    raw={"smudgy-1-1.profraw": "counts"})
        pgo.write(self.run/"merge.json", dict(sha256=pgo.sha(self.run/"merged.profdata"), raw={},
                                              workloads=[self.training_record]))
        for stage in ("generate", "use"):
            directory = self.run/stage
            directory.mkdir()
            exe = directory/"smudgy.exe"
            exe.write_bytes(stage.encode())
            pgo.write(directory/"manifest.json", dict(target=self.target, exit_code=0, inputs_verified=True,
                executable=str(exe), executable_sha256=pgo.sha(exe), pgo_diagnostics=[],
                profile_sha256=pgo.sha(self.run/"merged.profdata") if stage == "use" else None))

    def validation(self, categories=None, scenario_sha256="heldout-scenario", inputs=None):
        output = self.run/"validation/test"
        output.mkdir(parents=True)
        pgo.write(output/"manifest.json", dict(exit_code=0, verification_exit_code=0,
            executable_sha256=pgo.sha(self.run/"use/smudgy.exe"), scenario_sha256=scenario_sha256,
            inputs={"C:/workloads/heldout.log": "unseen-corpus"} if inputs is None else inputs,
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

    def test_missing_training_categories_cannot_merge(self):
        output = self.run/"training/test"
        output.mkdir(parents=True)
        pgo.write(output/"manifest.json", dict(exit_code=0, verification_exit_code=0,
            executable_sha256=pgo.sha(self.run/"generate/smudgy.exe"),
            scenario={"categories": ["replay"]}, raw={"smudgy-1-1.profraw": "counts"}))
        (self.run/"merge.json").unlink()
        with self.assertRaisesRegex(RuntimeError, "interactive coverage"):
            pgo.merge(self.run)

    def test_training_run_without_a_profile_cannot_count_toward_coverage(self):
        output = self.run/"training/silent"
        output.mkdir(parents=True)
        pgo.write(output/"manifest.json", dict(exit_code=0, verification_exit_code=0,
            executable_sha256=pgo.sha(self.run/"generate/smudgy.exe"),
            scenario={"categories": ["replay", "captures", "interactive"]}, raw={}))
        (self.run/"merge.json").unlink()
        with self.assertRaisesRegex(RuntimeError, "No profile was collected"):
            pgo.merge(self.run)

    def test_heldout_scenario_reused_from_training_cannot_export(self):
        self.validation(scenario_sha256=self.training_record["scenario_sha256"])
        with self.assertRaisesRegex(RuntimeError, "Held-out scenario was used in training"):
            pgo.export(self.run)
        self.assertFalse((self.run/"dist").exists())

    def test_heldout_scenario_without_unseen_inputs_cannot_export(self):
        for inputs in ({}, dict(self.training_record["inputs"])):
            with self.subTest(inputs=inputs):
                self.validation(inputs=inputs)
                with self.assertRaisesRegex(RuntimeError, "input file absent from training"):
                    pgo.export(self.run)
                (self.run/"validation/test/manifest.json").unlink()
                (self.run/"validation/test").rmdir()

    def test_packaging_from_another_revision_is_rejected(self):
        self.validation()
        pgo.export(self.run)
        with patch.object(pgo, "head_revision", return_value="another-commit"):
            with self.assertRaisesRegex(RuntimeError, "revision differs"):
                pgo.verify_artifact(self.run/"dist/manifest.json")

    def test_export_records_and_verifies_symbols(self):
        pdb = self.run/"use/smudgy.pdb"
        pdb.write_bytes(b"symbols")
        manifest = pgo.read(self.run/"use/manifest.json")
        manifest.update(pdb=str(pdb), pdb_sha256=pgo.sha(pdb))
        pgo.write(self.run/"use/manifest.json", manifest)
        self.validation()
        pgo.export(self.run)
        exported = pgo.read(self.run/"dist/manifest.json")
        self.assertEqual(exported["pdb_sha256"], pgo.sha(pdb))
        pgo.verify_artifact(self.run/"dist/manifest.json")
        (self.run/"dist/smudgy.pdb").write_bytes(b"other symbols")
        with self.assertRaisesRegex(RuntimeError, "symbols hash mismatch"):
            pgo.verify_artifact(self.run/"dist/manifest.json")

    def collected_workload(self, label, verifier):
        scenario = self.run/"scenario.json"
        pgo.write(scenario, {"verify": ["verifier"], "categories": ["replay"]})
        output = self.run/"training"/label

        def launch(command, cwd, env, stdout, stderr):
            self.assertTrue(env["LLVM_PROFILE_FILE"].startswith(str(output/"raw")))
            (output/"raw/smudgy-1-1.profraw").write_bytes(b"counts")
            return Mock(pid=1, wait=Mock(return_value=0))

        with patch.object(pgo.subprocess, "Popen", side_effect=launch), \
             patch.object(pgo.subprocess, "run", side_effect=verifier):
            pgo.workload(self.run, label, scenario, True)
        return output

    def test_verifier_runs_without_profile_collection(self):
        seen = {}

        def verifier(command, cwd, env, stdout, stderr):
            seen["env"] = env
            return Mock(returncode=0)

        output = self.collected_workload("clean", verifier)
        self.assertNotIn("LLVM_PROFILE_FILE", seen["env"])
        self.assertEqual(seen["env"]["PGO_SCENARIO_OUTPUT"], str(output))
        record = pgo.read(output/"manifest.json")
        self.assertEqual(list(record["raw"]), [str(output/"raw/smudgy-1-1.profraw")])
        self.assertEqual(record["verification_exit_code"], 0)

    def test_verifier_that_alters_the_profile_fails_the_run(self):
        def verifier(command, cwd, env, stdout, stderr):
            (self.run/"training/tampered/raw/smudgy-2-2.profraw").write_bytes(b"verifier counts")
            return Mock(returncode=0)

        with self.assertRaisesRegex(RuntimeError, "Verifier altered the collected profile"):
            self.collected_workload("tampered", verifier)
        record = pgo.read(self.run/"training/tampered/manifest.json")
        self.assertEqual(len(record["raw"]), 1)

    def test_profdata_must_come_from_the_rustc_llvm(self):
        sysroot = self.run/"sysroot"
        tool = sysroot/f"lib/rustlib/{pgo.TARGET}/bin/llvm-profdata.exe"
        tool.parent.mkdir(parents=True)
        tool.write_bytes(b"")
        (sysroot/"bin").mkdir()
        (sysroot/"bin"/("rustc.exe" if sys.platform == "win32" else "rustc")).write_bytes(b"")

        def capture(*command):
            if command == ("rustc", "--print", "sysroot"):
                return str(sysroot)
            if command == (str(tool), "--version"):
                # rustup's llvm-tools carry the Rust release suffix; rustc -vV does not.
                return "LLVM (http://llvm.org/):\n  LLVM version 22.1.6-rust-1.97.1-stable\n  Optimized build."
            raise AssertionError(command)

        with patch.object(pgo, "capture", capture):
            with self.assertRaisesRegex(RuntimeError, "same LLVM release"):
                pgo.profdata_tool("rustc 1.97.1 (abc 2026-10-01)\nhost: x86_64-pc-windows-msvc\nLLVM version: 22.1.5")
            self.assertEqual(pgo.profdata_tool("rustc 1.97.1 (abc 2026-10-01)\nhost: x86_64-pc-windows-msvc\nLLVM version: 22.1.6"), tool)

    def test_pgo_diagnostics_match_llvm_warnings_not_the_command_echo(self):
        diagnostics = [
            "warning: C:\\src\\lib.rs: no profile data available for function _ZN3foo3bar Hash = 1",
            "warning: Function control flow change detected (hash mismatch) _ZN3foo3baz Hash = 2",
            "warning: Inconsistent number of counts in _ZN3foo3qux: the profile may be stale",
            "  warning: profile data may be out of date for function _ZN3foo3fin",
            # An unreadable or foreign profile compiles without PGO; rustc only warns.
            "warning: C:\\pgo\\run\\merged.profdata: invalid instrumentation profile data (bad magic)",
            "warning: C:\\pgo\\run\\merged.profdata: unsupported instrumentation profile format version",
            "warning: C:\\pgo\\run\\merged.profdata: Not an IR level instrumentation profile",
            # Colour escapes from an inherited CARGO_TERM_COLOR must not hide a diagnostic.
            "\x1b[1m\x1b[33mwarning\x1b[0m\x1b[1m: C:\\src\\lib.rs: no profile data available for function _ZN3foo3bar",
        ]
        noise = [
            "     Running `rustc --crate-name smudgy -Cprofile-use=C:/pgo/run/merged.profdata -Cllvm-args=-pgo-warn-missing-function`",
            "[smudgy_ui 0.5.9] cargo:rerun-if-changed=THIRD-PARTY-NOTICES.md",
            "   Compiling profile-missing-crate v0.1.0",
            "warning: unused variable: `profile_missing`",
            "warning: unused import: `stale_profile_reader`",
        ]
        for line in diagnostics:
            self.assertTrue(pgo.PGO_DIAGNOSTIC.search(line), line)
        for line in noise:
            self.assertFalse(pgo.PGO_DIAGNOSTIC.search(line), line)

    def test_flags_that_hide_or_redirect_diagnostics_are_rejected(self):
        for index, flag in enumerate(["-Awarnings", "--cap-lints=allow", "-Cllvm-args=-no-pgo-warn-mismatch",
                                      "-Cprofile-use=C:/elsewhere.profdata", "-Zprofile", "-Cllvm-args=-pgo-warn-missing-function"]):
            with self.subTest(flag=flag):
                with self.assertRaisesRegex(RuntimeError, "managed by this tool"):
                    pgo.build(self.run/f"fresh-{index}", "generate", [flag])
        with patch.object(pgo, "toolchain_rustc", side_effect=RuntimeError("stop before cargo")):
            with self.assertRaisesRegex(RuntimeError, "stop before cargo"):
                pgo.build(self.run/"fresh-ok", "generate", ["-Cdebuginfo=1", "-Ctarget-cpu=x86-64-v3"])

    def test_training_without_declared_inputs_cannot_anchor_heldout_evidence(self):
        merge = pgo.read(self.run/"merge.json")
        merge["workloads"][0]["inputs"] = {}
        pgo.write(self.run/"merge.json", merge)
        self.validation()
        with self.assertRaisesRegex(RuntimeError, "Training scenarios must declare"):
            pgo.export(self.run)
        self.assertFalse((self.run/"dist").exists())

    def test_scenarios_are_validated_before_launch(self):
        scenario = self.run/"scenario.json"
        cases = [
            ({"verify": ["v"], "categories": ["replay"], "env": {"PGO_EXECUTABLE": "x"}}, "collector owns"),
            ({"verify": ["v"], "categories": ["replay"], "env": {"LLVM_PROFILE_FILE": "x"}}, "collector owns"),
            ({"verify": ["v"], "categories": ["replay"], "args": ["--data-dir", 7]}, "must be strings"),
            ({"verify": ["v"], "categories": ["replay"], "env": {"HOME": None}}, "must be strings"),
            ({"verify": ["v"], "categories": ["replay"], "inputs": [["corpus.log"]]}, "must be strings"),
        ]
        for index, (content, message) in enumerate(cases):
            with self.subTest(content=content), patch.object(pgo.subprocess, "Popen") as launch:
                pgo.write(scenario, content)
                with self.assertRaisesRegex(RuntimeError, message):
                    pgo.workload(self.run, f"invalid-{index}", scenario, True)
                launch.assert_not_called()

    def test_relative_inputs_resolve_against_the_scenario_file(self):
        directory = self.run/"workloads"
        directory.mkdir()
        corpus = directory/"corpus.log"
        corpus.write_bytes(b"lines")
        scenario = directory/"replay.json"
        pgo.write(scenario, {"verify": ["v"], "categories": ["replay"], "inputs": ["corpus.log"]})
        output = self.run/"training/relative"

        def launch(command, cwd, env, stdout, stderr):
            (output/"raw/smudgy-1-1.profraw").write_bytes(b"counts")
            return Mock(pid=1, wait=Mock(return_value=0))

        with patch.object(pgo.subprocess, "Popen", side_effect=launch), \
             patch.object(pgo.subprocess, "run", return_value=Mock(returncode=0)):
            pgo.workload(self.run, "relative", scenario, True)
        self.assertEqual(pgo.read(output/"manifest.json")["inputs"], {str(corpus.resolve()): pgo.sha(corpus)})

    def test_command_output_requests_utf8_decoding(self):
        with patch.object(pgo.subprocess, "check_output", return_value="ok\n") as run:
            self.assertEqual(pgo.capture("tool", "--version"), "ok")
        self.assertEqual(run.call_args.kwargs.get("encoding"), "utf-8")

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






if __name__ == "__main__":
    unittest.main()
