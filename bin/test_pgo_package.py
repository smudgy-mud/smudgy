"""Packaging contract tests using synthetic, validated exports; no signing."""
from pathlib import Path
import tempfile
import unittest
import unittest.mock
from unittest.mock import patch

import yaml

import pgo
import pgo_package


# /.flatpak-info as flatpak 1.16 writes it inside `flatpak run <runtime>` (a [Runtime]
# section) and inside `flatpak build` (an [Application] section naming both refs).
# Refs carry their kind prefix; ephemeral paths and IDs are omitted here.
RUNTIME_INFO = """[Runtime]
name={name}
runtime=runtime/{name}/{arch}/{branch}

[Instance]
runtime-commit=b7aba340fd291e08bf26442d43a0e4adb6733da5a06d47ff4296a964af40cc8a
branch={branch}
arch={arch}
flatpak-version=1.16.6
session-bus-proxy=true
system-bus-proxy=true
extra-args=--usb-list=;--usb-list=;

[Environment]
ALSA_CONFIG_PATH=/usr/share/alsa/alsa-flatpak.conf
XDG_DATA_DIRS=/app/share:/usr/share:/usr/share/runtime/share:/run/host/user-share:/run/host/share
PYTHONUSERBASE=/var/data/python
"""
BUILD_INFO = """[Application]
name=org.smudgy.Smudgy
runtime=runtime/org.freedesktop.Platform/{arch}/{branch}
sdk=runtime/org.freedesktop.Sdk/{arch}/{branch}

[Instance]
branch={branch}
arch={arch}
flatpak-version=1.16.6
"""


class PackageTests(unittest.TestCase):
    def flatpak_info(self, template, **fields):
        """Parse a real-format /.flatpak-info through the helper's own reader."""
        path = self.root/"flatpak-info"
        path.write_text(template.format(**fields), encoding="utf-8")
        return pgo.flatpak_environment(path)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.sources = {"source.rs": "source digest"}
        self.base = pgo.ROOT/"packaging/linux/org.smudgy.Smudgy.yml"
        self.source_patch = patch.object(pgo, "source_files", return_value=self.sources)
        self.source_patch.start()
        self.addCleanup(self.source_patch.stop)
        self.revision_patch = patch.object(pgo, "head_revision", return_value="validated-commit")
        self.revision_patch.start()
        self.addCleanup(self.revision_patch.stop)

    def artifact(self, target, sdk=None, content=b"validated executable", **identity):
        directory = self.root/target
        directory.mkdir()
        exe = directory/pgo.executable_name(target)
        exe.write_bytes(content)
        exe.chmod(0o755)
        digest = pgo.sha(exe)
        darwin = target.endswith("apple-darwin")
        manifest = dict(schema=2, stage="validated-pgo", target=target, revision="validated-commit",
                        executable_name=exe.name, executable_sha256=digest,
                        profile_sha256="profile digest", worktree_status="",
                        build_identity=dict(files=self.sources, target=target, flatpak=sdk,
                                            compiler="rustc 1.97.1", cargo="cargo 1.97.1", helper_sha256="helper",
                                            deployment_target="deployment_target=11.0" if darwin else None,
                                            configs={f"/home/{target}/.cargo/config.toml": "config digest"},
                                            local_dependencies={f"/home/{target}/deps": {"lib.rs": "dep digest"}},
                                            native_environment=dict(MACOSX_DEPLOYMENT_TARGET="11.0", SDKROOT=None,
                                                                    RUSTY_V8_ARCHIVE=f"/home/{target}/v8.a"), **identity),
                        build=dict(target=target, exit_code=0, inputs_verified=True,
                                   executable_sha256=digest, profile_sha256="profile digest",
                                   flags=["-Cdebuginfo=1", f"-Cprofile-use=/runs/{target}/merged.profdata",
                                          "-Cllvm-args=-pgo-warn-missing-function"]),
                        training=[dict(scenario_sha256="trained scenario", inputs={"/corpus.log": "trained corpus"})],
                        validation=[dict(exit_code=0, verification_exit_code=0,
                                         executable_sha256=digest, scenario_sha256="held-out scenario",
                                         inputs={"/heldout.log": "unseen corpus"},
                                         scenario=dict(categories=["replay", "captures", "heldout", "interactive"]))])
        path = directory/"manifest.json"
        pgo.write(path, manifest)
        return path

    def test_stage_all_targets_checks_bytes_and_keeps_export(self):
        for target in pgo.TARGETS:
            with self.subTest(target=target):
                path = self.artifact(target)
                destination = self.root/"stage"/target/pgo.executable_name(target)
                pgo_package.stage(path, target, destination)
                self.assertEqual(destination.read_bytes(), b"validated executable")
                pgo_package.check(path, target, destination)
                destination.write_bytes(b"signed staging copy")
                with self.assertRaisesRegex(RuntimeError, "Packaged PGO executable changed"):
                    pgo_package.check(path, target, destination)
                pgo.verify_artifact(path, target, portable=True)

    def test_wrong_architecture_is_rejected_before_copy(self):
        path = self.artifact("aarch64-apple-darwin")
        destination = self.root/"must-not-exist"
        with self.assertRaisesRegex(RuntimeError, "target mismatch"):
            pgo_package.stage(path, "x86_64-apple-darwin", destination)
        self.assertFalse(destination.exists())

    def test_flatpak_overlay_preserves_assets_without_rebuilding(self):
        original = yaml.safe_load(self.base.read_text(encoding="utf-8"))
        for arch in ("x86_64", "aarch64"):
            with self.subTest(arch=arch):
                sdk = self.flatpak_info(RUNTIME_INFO, name="org.freedesktop.Sdk", arch=arch, branch="25.08")
                self.assertEqual(sdk["Runtime"]["runtime"], f"runtime/org.freedesktop.Sdk/{arch}/25.08")
                path = self.artifact(f"{arch}-unknown-linux-gnu", sdk)
                output = self.root/"overlays"/arch/"manifest.json"
                pgo_package.flatpak(path, arch, self.base, output, "25.08")
                overlay = pgo.read(output)
                module = overlay["modules"][0]
                commands = module["build-commands"]
                self.assertTrue(commands[0].endswith("| sha256sum -c -"))
                self.assertIn("install -Dm755 smudgy-pgo.bin /app/bin/smudgy.bin", commands)
                assets = [c for c in original["modules"][0]["build-commands"]
                          if c.startswith("install ") and "target/release-full" not in c]
                self.assertEqual(commands[2:], assets)
                self.assertEqual(overlay["finish-args"], original["finish-args"])
                self.assertEqual(Path(module["sources"][0]["path"]), pgo.ROOT)
                self.assertEqual(Path(module["sources"][1]["path"]).read_bytes(), b"validated executable")
                self.assertFalse(overlay["build-options"]["strip"])
                self.assertTrue(overlay["build-options"]["no-debuginfo"])

    def test_flatpak_build_sandbox_identity_is_accepted(self):
        sdk = self.flatpak_info(BUILD_INFO, arch="x86_64", branch="25.08")
        self.assertEqual(sdk["Application"]["sdk"], "runtime/org.freedesktop.Sdk/x86_64/25.08")
        path = self.artifact("x86_64-unknown-linux-gnu", sdk)
        pgo_package.flatpak(path, "x86_64", self.base, self.root/"overlay.json")
        self.assertTrue((self.root/"overlay.json").exists())

    def test_flatpak_rejects_host_or_wrong_sdk(self):
        path = self.artifact("x86_64-unknown-linux-gnu")
        original = pgo.read(path)
        cases = [
            ("host", None),
            ("platform not sdk", self.flatpak_info(RUNTIME_INFO, name="org.freedesktop.Platform", arch="x86_64", branch="25.08")),
            ("old branch", self.flatpak_info(RUNTIME_INFO, name="org.freedesktop.Sdk", arch="x86_64", branch="24.08")),
            ("other arch", self.flatpak_info(RUNTIME_INFO, name="org.freedesktop.Sdk", arch="aarch64", branch="25.08")),
            ("build on old branch", self.flatpak_info(BUILD_INFO, arch="x86_64", branch="24.08")),
        ]
        for label, sdk in cases:
            original["build_identity"]["flatpak"] = sdk
            pgo.write(path, original)
            with self.subTest(case=label), self.assertRaisesRegex(RuntimeError, "release SDK"):
                pgo_package.flatpak(path, "x86_64", self.base, self.root/"overlay.json")
        self.assertFalse((self.root/"overlay.json").exists())

    def test_flatpak_runtime_override_must_match_manifest(self):
        path = self.artifact("x86_64-unknown-linux-gnu")
        with self.assertRaisesRegex(RuntimeError, "Runtime override"):
            pgo_package.flatpak(path, "x86_64", self.base, self.root/"overlay.json", "24.08")


    def mac_pair(self, **x86_64_identity):
        arm64 = self.artifact("aarch64-apple-darwin", content=b"arm64 slice")
        x86_64 = self.artifact("x86_64-apple-darwin", content=b"x86_64 slice")
        manifest = pgo.read(x86_64)
        manifest["build_identity"].update(x86_64_identity)
        pgo.write(x86_64, manifest)
        return arm64, x86_64

    def test_mac_exports_pair_only_from_one_frozen_build_environment(self):
        arm64, x86_64 = self.mac_pair()
        pgo_package.pair(arm64, x86_64)
        original = pgo.read(x86_64)
        for key, value, error in (("compiler", "rustc 1.98.0", "compiler"), ("cargo", "cargo 1.98.0", "cargo"),
                                  ("helper_sha256", "other", "helper_sha256"),
                                  ("native_environment", dict(MACOSX_DEPLOYMENT_TARGET="12.0", RUSTY_V8_ARCHIVE="/x/v8.a"), "environment"),
                                  ("configs", {"/home/other/.cargo/config.toml": "other config"}, "configs"),
                                  ("deployment_target", None, "deployment-target")):
            with self.subTest(key=key):
                manifest = pgo.read(x86_64)
                manifest["build_identity"][key] = value
                pgo.write(x86_64, manifest)
                with self.assertRaisesRegex(RuntimeError, error):
                    pgo_package.pair(arm64, x86_64)
                pgo.write(x86_64, original)
        manifest = pgo.read(x86_64)
        manifest["build"]["flags"][0] = "-Ctarget-cpu=native"
        pgo.write(x86_64, manifest)
        with self.assertRaisesRegex(RuntimeError, "base_flags"):
            pgo_package.pair(arm64, x86_64)
        pgo.write(x86_64, original)
        # A different revision fails portable verification before pairing is reached.
        manifest = pgo.read(x86_64)
        manifest["revision"] = "other-commit"
        pgo.write(x86_64, manifest)
        with self.assertRaisesRegex(RuntimeError, "revision"):
            pgo_package.pair(arm64, x86_64)

    def test_paired_exports_ignore_machine_paths_but_not_contents(self):
        arm64, x86_64 = self.mac_pair(configs={"/Users/intel/.cargo/config.toml": "config digest"},
                                      local_dependencies={"/Users/intel/deps": {"lib.rs": "dep digest"}},
                                      native_environment=dict(MACOSX_DEPLOYMENT_TARGET="11.0", SDKROOT=None,
                                                              RUSTY_V8_ARCHIVE="/Users/intel/v8.a"))
        pgo_package.pair(arm64, x86_64)
        manifest = pgo.read(x86_64)
        manifest["build_identity"]["local_dependencies"] = {"/Users/intel/deps": {"lib.rs": "patched"}}
        pgo.write(x86_64, manifest)
        with self.assertRaisesRegex(RuntimeError, "local_dependencies"):
            pgo_package.pair(arm64, x86_64)

    def fake_lipo(self, slices):
        """lipo that answers -archs and writes the given bytes for `-thin <arch>`."""
        def run(command, **kwargs):
            self.assertEqual((command[0], command[2], command[4]), ("lipo", "-thin", "-output"))
            Path(command[5]).write_bytes(slices[command[3]])
            return unittest.mock.Mock(returncode=0)

        def capture(*command):
            self.assertEqual(command[:2], ("lipo", "-archs"))
            return " ".join(slices)
        return patch.object(pgo_package.subprocess, "run", run), patch.object(pgo, "capture", capture)

    def test_universal_binary_must_thin_back_into_both_validated_slices(self):
        arm64, x86_64 = self.mac_pair()
        fused = self.root/"Smudgy.app-smudgy"
        fused.write_bytes(b"fat header" + b"arm64 slice" + b"x86_64 slice")
        receipt = self.root/"universal.json"
        run, capture = self.fake_lipo({"arm64": b"arm64 slice", "x86_64": b"x86_64 slice"})
        with run, capture:
            pgo_package.universal(fused, arm64, x86_64, receipt)
        recorded = pgo.read(receipt)
        self.assertEqual(recorded["universal_sha256"], pgo.sha(fused))
        self.assertEqual(recorded["slices"]["arm64"]["executable_sha256"], pgo.read(arm64)["executable_sha256"])
        self.assertEqual(recorded["slices"]["x86_64"]["deployment_target"], "deployment_target=11.0")
        run, capture = self.fake_lipo({"arm64": b"arm64 slice", "x86_64": b"x86_64 slice tampered"})
        with run, capture, self.assertRaisesRegex(RuntimeError, "x86_64 slice differs"):
            pgo_package.universal(fused, arm64, x86_64, receipt)
        run, capture = self.fake_lipo({"arm64": b"arm64 slice"})
        with run, capture, self.assertRaisesRegex(RuntimeError, "exactly arm64 and x86_64"):
            pgo_package.universal(fused, arm64, x86_64, receipt)

    def test_committed_flatpak_executable_is_read_from_the_exported_commit(self):
        manifest = self.artifact("x86_64-unknown-linux-gnu", content=b"linux executable")
        commands = []

        def run(command, **kwargs):
            commands.append(command)
            return unittest.mock.Mock(returncode=0, stdout=b"linux executable")
        with patch.object(pgo_package.subprocess, "run", run):
            pgo_package.committed(manifest, self.root/"repo", "x86_64",
                                  ostree="flatpak run --filesystem=host --command=ostree org.flatpak.Builder")
        self.assertEqual(commands, [["flatpak", "run", "--filesystem=host", "--command=ostree", "org.flatpak.Builder",
                                     f"--repo={(self.root/'repo').resolve()}", "cat", "app/org.smudgy.Smudgy/x86_64/master",
                                     "/files/bin/smudgy.bin"]])
        with patch.object(pgo_package.subprocess, "run", return_value=unittest.mock.Mock(stdout=b"other bytes")), \
             self.assertRaisesRegex(RuntimeError, "Committed Flatpak executable differs"):
            pgo_package.committed(manifest, self.root/"repo", "x86_64")

    def test_flatpak_overlay_drops_the_build_time_network_grant(self):
        sdk = self.flatpak_info(BUILD_INFO, arch="x86_64", branch="25.08")
        manifest = self.artifact("x86_64-unknown-linux-gnu", sdk)
        overlay = pgo.read(pgo_package.flatpak(manifest, "x86_64", self.base, self.root/"overlay/flatpak.json"))
        self.assertNotIn("build-options", overlay["modules"][0])
        self.assertIn("--share=network", overlay["finish-args"])
        self.assertEqual(overlay["build-options"]["strip"], False)
        self.assertEqual(overlay["build-options"]["no-debuginfo"], True)


if __name__ == "__main__":
    unittest.main()
