"""Packaging contract tests using synthetic, validated exports; no signing."""
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import yaml

import pgo
import pgo_package


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.sources = {"source.rs": "source digest"}
        self.base = pgo.ROOT/"packaging/linux/org.smudgy.Smudgy.yml"
        self.source_patch = patch.object(pgo, "source_files", return_value=self.sources)
        self.source_patch.start()
        self.addCleanup(self.source_patch.stop)

    def artifact(self, target, sdk=None):
        directory = self.root/target
        directory.mkdir()
        exe = directory/pgo.executable_name(target)
        exe.write_bytes(b"validated executable")
        exe.chmod(0o755)
        digest = pgo.sha(exe)
        manifest = dict(schema=2, stage="validated-pgo", target=target,
                        executable_name=exe.name, executable_sha256=digest,
                        profile_sha256="profile digest",
                        build_identity=dict(files=self.sources, target=target, flatpak=sdk),
                        build=dict(target=target, exit_code=0, inputs_verified=True,
                                   executable_sha256=digest, profile_sha256="profile digest"),
                        validation=[dict(exit_code=0, verification_exit_code=0,
                                         executable_sha256=digest,
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
                sdk = {"Runtime": {"runtime": f"org.freedesktop.Sdk/{arch}/25.08"}}
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

    def test_flatpak_rejects_host_or_wrong_sdk(self):
        path = self.artifact("x86_64-unknown-linux-gnu")
        original = pgo.read(path)
        for sdk in (None, {"Runtime": {"runtime": "org.freedesktop.Sdk/x86_64/24.08"}},
                    {"Runtime": {"runtime": "org.freedesktop.Sdk/aarch64/25.08"}}):
            original["build_identity"]["flatpak"] = sdk
            pgo.write(path, original)
            with self.subTest(sdk=sdk), self.assertRaisesRegex(RuntimeError, "release SDK"):
                pgo_package.flatpak(path, "x86_64", self.base, self.root/"overlay.json")
        self.assertFalse((self.root/"overlay.json").exists())

    def test_flatpak_runtime_override_must_match_manifest(self):
        path = self.artifact("x86_64-unknown-linux-gnu")
        with self.assertRaisesRegex(RuntimeError, "Runtime override"):
            pgo_package.flatpak(path, "x86_64", self.base, self.root/"overlay.json", "24.08")


if __name__ == "__main__":
    unittest.main()
