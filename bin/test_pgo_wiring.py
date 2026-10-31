"""Negative controls for the release feature audit's PGO packaging branches."""
import contextlib
import importlib.util
import io
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("audio_audit", Path(__file__).with_name("check-web-audio-wiring.py"))
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)


class WiringTests(unittest.TestCase):
    def test_current_release_paths_pass(self):
        with contextlib.redirect_stdout(io.StringIO()):
            audit.main()

    def rejects(self, name, before, after, error):
        path = audit.ROOT/name
        original = path.read_text(encoding="utf-8")
        self.assertIn(before, original)
        changed = original.replace(before, after, 1)
        read_text = Path.read_text

        def read(selected, *args, **kwargs):
            return changed if selected == path else read_text(selected, *args, **kwargs)

        with patch.object(Path, "read_text", read), self.assertRaisesRegex(RuntimeError, error):
            audit.main()

    def test_rust_pgo_cannot_drop_the_shipping_feature(self):
        self.rejects("bin/pgo.py", '"--features", FEATURES]', '"--no-default-features"]',
                     "PGO build must retain")

    def test_mac_inspector_only_build_cannot_become_the_default(self):
        self.rejects("bin/release-mac.sh", 'if [[ -n "$PGO_ARM64" ]]; then\n            cargo build',
                     'if true; then\n            cargo build', "inspector-only build")

    def test_mac_bundle_cannot_skip_the_default_app_build(self):
        self.rejects("bin/release-mac.sh", '[[ -z "$PGO_ARM64" ]] || export CARGO_BUNDLE_SKIP_BUILD=1',
                     'export CARGO_BUNDLE_SKIP_BUILD=1', "bundle must skip rebuilding only")

    def test_mac_requires_both_architecture_verifications(self):
        self.rejects("bin/release-mac.sh", '--portable --target x86_64-apple-darwin >/dev/null',
                     '--portable >/dev/null', "must verify the x86_64-apple-darwin export")

    def test_flatpak_overlay_requires_the_reviewed_base_manifest(self):
        self.rejects("bin/release-linux.sh", '--base "$manifest" --runtime-version',
                     '--base other.yml --runtime-version', "overlay must derive from the reviewed manifest")


if __name__ == "__main__":
    unittest.main()
