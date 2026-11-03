"""A probe run yields diagnostics evidence and nothing a release could consume."""
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

import pgo
import pgo_probe


class ProbeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.run = Path(self.temp.name)/"run"
        self.target = "aarch64-apple-darwin"
        self.identity = dict(target=self.target, compiler="rustc 1.97.1\nLLVM version: 22.1.6",
                             deployment_target="deployment_target=11.0", files={})
        for name in ("identity", "native_target", "head_revision", "worktree_status", "toolchain_rustc"):
            patcher = patch.object(pgo, name, return_value={"identity": self.identity, "native_target": self.target,
                                                            "head_revision": "probed", "worktree_status": "",
                                                            "toolchain_rustc": (self.run, self.run/"rustc")}[name])
            patcher.start()
            self.addCleanup(patcher.stop)

    def fake_build(self, use_diagnostics):
        """Stand in for the two Cargo builds: write the receipts pgo.build would."""
        def build(run, stage, rustflags=(), target=None, allow_dirty=False):
            if stage == "generate":
                run.mkdir(parents=True)
                pgo.write(run/"run.json", dict(target=self.target, revision="probed", worktree_status="",
                                               base_flags=list(rustflags), identity=self.identity))
            output = run/stage
            output.mkdir()
            exe = output/self.target/"release-full"/"smudgy"
            exe.parent.mkdir(parents=True)
            exe.write_bytes(stage.encode())
            unreviewed, reviewed = pgo.review_diagnostics(use_diagnostics if stage == "use" else [], self.target)
            pgo.write(output/"manifest.json", dict(stage=stage, target=self.target, exit_code=0, inputs_verified=True,
                executable=str(exe), executable_sha256=pgo.sha(exe), pgo_diagnostics=unreviewed,
                reviewed_diagnostics=reviewed, profile_sha256=pgo.sha(run/"merged.profdata") if stage == "use" else None))
            pgo.require(not unreviewed, "PGO coverage diagnostics require investigation")
        return build

    def flushing_application(self, code=pgo_probe.USAGE_ERROR):
        def run(command, env=None, **kwargs):
            if command[1:] == ["--data-dir"]:
                raw = Path(env["LLVM_PROFILE_FILE"]).parent
                (raw/"smudgy-1-1.profraw").write_bytes(b"counts")
                return Mock(returncode=code, stderr="error: --data-dir requires a value")
            self.assertEqual(command[:2], [str(self.run/"llvm-profdata"), "merge"])
            (self.run/"merged.profdata").write_bytes(b"merged")
            return Mock(returncode=0)
        return run

    def probe(self, diagnostics, code=pgo_probe.USAGE_ERROR):
        with patch.object(pgo, "build", self.fake_build(diagnostics)), \
             patch.object(pgo, "profdata_tool", return_value=self.run/"llvm-profdata"), \
             patch.object(pgo_probe.subprocess, "run", self.flushing_application(code)), \
             patch.object(pgo_probe.subprocess, "check_output", return_value="Total functions: 1\n"):
            pgo_probe.probe(self.run, self.target)

    def test_probe_records_reviewed_diagnostics_and_cannot_feed_a_release(self):
        helper = ("warning: smudgy-cgu.0: no profile data available for function "
                  "_RINvNtCsa9BKTri5B3M_3std2rt10lang_startuECs5JAvRntY6Pu_6smudgy Hash = 7 up to 0 count discarded")
        self.probe([helper])
        result = pgo.read(self.run/"probe/result.json")
        self.assertEqual((result["reviewed_diagnostics"], result["unreviewed_diagnostics"], result["error"]), ([helper], [], None))
        self.assertEqual(result["deployment_target"], "deployment_target=11.0")
        merge = pgo.read(self.run/"merge.json")
        self.assertEqual((merge["workloads"], merge["probe"]), ([], True))
        with self.assertRaisesRegex(RuntimeError, "already merged"):
            pgo.merge(self.run)
        with self.assertRaisesRegex(RuntimeError, "No verified validation workloads"):
            pgo.export(self.run)

    def test_probe_fails_on_unreviewed_diagnostics_but_keeps_the_evidence(self):
        missing = ("warning: smudgy-cgu.0: no profile data available for function "
                   "_RNvCs5JAvRntY6Pu_6smudgy4main Hash = 1 up to 0 count discarded")
        with self.assertRaisesRegex(RuntimeError, "require investigation"):
            self.probe([missing])
        result = pgo.read(self.run/"probe/result.json")
        self.assertEqual(result["unreviewed_diagnostics"], [missing])
        self.assertIn("require investigation", result["error"])

    def test_probe_requires_the_usage_error_exit(self):
        with self.assertRaisesRegex(RuntimeError, "usage-error exit 2, got 0"):
            self.probe([], code=0)


if __name__ == "__main__":
    unittest.main()
