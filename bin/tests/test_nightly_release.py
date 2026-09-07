"""Offline tests: no GitHub writes, application builds, or git commits."""

from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import tomllib
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("nightly", ROOT / "bin/nightly-release.py")
nightly = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(nightly)
SOURCE = "a" * 40
TAG = "v0.5.7-ptb.5"
VERSION = TAG[1:]
NOW = datetime(2026, 9, 7, tzinfo=timezone.utc)


@contextmanager
def working_directory(path):
    previous = Path.cwd()
    os.chdir(path)
    try:
        yield
    finally:
        os.chdir(previous)


def metadata():
    return {"schema": 1, "version": VERSION, "source_sha": SOURCE, "created_at": NOW.isoformat()}


def run(status="completed", conclusion="failure", attempt=3):
    return {"id": 10, "status": status, "conclusion": conclusion, "run_attempt": attempt}


class NumberingTests(unittest.TestCase):
    def test_legacy_numbers_reserve_their_place(self):
        tags = ["v0.5.7-ptb", "v0.5.7-ptb2", "v0.5.7-ptb4", "v0.5.6-ptb.99"]
        self.assertEqual(nightly.next_version("0.5.7-ptb", tags), VERSION)

    def test_numeric_order_and_other_spelling(self):
        tags = ["v0.5.7-ptb.9", "v0.5.7-ptb.10", "v0.5.7-ptb-11", "v0.5.7-ptbeta99"]
        self.assertEqual(nightly.next_version("0.5.7-ptb", tags), "0.5.7-ptb.12")
        self.assertEqual(nightly.next_version("0.5.8-ptb", tags), "0.5.8-ptb.1")

    def test_base_must_be_unnumbered_ptb(self):
        for base in ["0.5.7", VERSION, "0.5.7-rc", "01.5.7-ptb"]:
            with self.subTest(base=base), self.assertRaises(ValueError):
                nightly.next_version(base, [])


class RecoveryTests(unittest.TestCase):
    def decision(self, release=None, runs=None, now=NOW):
        return nightly.existing_action(SOURCE, TAG, metadata(), release, runs or [], now)[0]

    def test_receipt_survives_workflow_run_expiry(self):
        release = {"draft": False, "body": nightly.receipt(SOURCE, TAG)}
        self.assertEqual(self.decision(release, now=NOW + timedelta(days=500)), "skip")

    def test_partial_or_draft_release_is_not_completion(self):
        self.assertEqual(self.decision({"draft": False, "body": "notes"}), "dispatch")
        self.assertEqual(self.decision({"draft": True, "body": nightly.receipt(SOURCE, TAG)}), "dispatch")

    def test_other_sources_receipt_does_not_suppress_recovery(self):
        release = {"draft": False, "body": nightly.receipt("b" * 40, TAG)}
        self.assertEqual(self.decision(release), "dispatch")

    def test_active_run_or_retry_never_dispatches_again(self):
        for status in ["queued", "in_progress", "waiting", "pending", "requested"]:
            with self.subTest(status=status):
                self.assertEqual(self.decision(runs=[run(status=status)]), "skip")

    def test_exhausted_cancelled_and_retriable_runs_keep_reservation(self):
        for attempt in [1, 2, 3, 4]:
            for conclusion in ["failure", "cancelled", "timed_out", "success"]:
                with self.subTest(attempt=attempt, conclusion=conclusion):
                    self.assertEqual(self.decision(runs=[run(attempt=attempt, conclusion=conclusion)]), "skip")

    def test_interrupted_tag_then_dispatch_is_recovered(self):
        self.assertEqual(self.decision(), "dispatch")

    def test_missing_old_run_does_not_restart_forever(self):
        self.assertEqual(self.decision(now=NOW + timedelta(days=31)), "skip")

    def test_tag_annotation_must_match_commit_parent(self):
        with patch.object(nightly, "git", side_effect=[nightly.ANNOTATION + json.dumps(metadata()), "c" * 40 + " " + "b" * 40]):
            with self.assertRaisesRegex(ValueError, "sole parent"):
                nightly.tag_metadata(TAG)


class ApiTests(unittest.TestCase):
    def test_only_404_is_absent(self):
        for status in [401, 403, 429, 500]:
            result = subprocess.CompletedProcess([], 1, json.dumps({"status": str(status)}), "failure")
            with self.subTest(status=status), patch.object(nightly.subprocess, "run", return_value=result):
                with self.assertRaises(RuntimeError):
                    nightly.api("releases/tags/test", missing_ok=True)
        result = subprocess.CompletedProcess([], 1, '{"status":"404"}', "not found")
        with patch.object(nightly.subprocess, "run", return_value=result):
            self.assertIsNone(nightly.api("releases/tags/test", missing_ok=True))

    def test_pagination_does_not_lose_old_runs(self):
        with patch.object(nightly, "api", side_effect=[{"workflow_runs": [run()] * 100}, {"workflow_runs": [run()]}]) as api:
            self.assertEqual(len(list(nightly.pages("runs?head_sha=abc", "workflow_runs"))), 101)
            self.assertIn("&per_page=100&page=2", api.call_args.args[0])

    def test_completion_requires_all_platform_assets(self):
        release = {"id": 42, "draft": False, "prerelease": True, "body": "notes"}
        assets = [{"name": f"smudgy-v{VERSION}-{suffix}", "size": 10} for suffix in
                  ("x86_64.flatpak", "aarch64.flatpak", "setup.exe", "universal.dmg")]
        with patch.object(nightly, "tag_metadata", return_value=metadata()), patch.object(nightly, "pages", return_value=assets[:-1]), patch.object(nightly, "api", return_value=release) as api:
            with self.assertRaisesRegex(ValueError, "missing platform"):
                nightly.complete(TAG)
            self.assertEqual(api.call_count, 1)
        with patch.object(nightly, "tag_metadata", return_value=metadata()), patch.object(nightly, "pages", return_value=assets), patch.object(nightly, "api", return_value=release) as api:
            nightly.complete(TAG)
            self.assertIn(nightly.receipt(SOURCE, TAG), api.call_args.kwargs["payload"]["body"])


class CoordinatorTests(unittest.TestCase):
    def setUp(self):
        # These scenarios concern the 0.5.7 series, independently of whichever
        # base version main moves to next.
        manifest = patch.object(nightly.Path, "read_text", return_value='[package]\nversion = "0.5.7-ptb"\n')
        manifest.start()
        self.addCleanup(manifest.stop)

    def fake_git(self, *args):
        if args[0] == "rev-parse":
            return "c" * 40 if args[1].endswith("^{commit}") else SOURCE
        if args[0] == "tag":
            return "v0.5.7-ptb4\n" + TAG
        raise AssertionError(args)

    def test_unchanged_completed_source_skips_without_reading_runs(self):
        release = {"draft": False, "body": nightly.receipt(SOURCE, TAG)}
        with working_directory(ROOT), patch.object(nightly, "git", side_effect=self.fake_git), patch.object(nightly, "tag_metadata", side_effect=[None, metadata()]), patch.object(nightly, "api", return_value=release), patch.object(nightly, "pages") as pages:
            plan = nightly.make_plan()
        self.assertEqual(plan["action"], "skip")
        self.assertEqual(plan["tag"], TAG)
        pages.assert_not_called()

    def test_changed_source_reserves_new_number_without_changing_old_release(self):
        old = {**metadata(), "source_sha": "b" * 40}
        with working_directory(ROOT), patch.object(nightly, "git", side_effect=self.fake_git), patch.object(nightly, "tag_metadata", side_effect=[None, old]), patch.object(nightly, "api") as api:
            plan = nightly.make_plan()
        self.assertEqual(plan["action"], "create")
        self.assertEqual(plan["tag"], "v0.5.7-ptb.6")
        api.assert_not_called()

    def test_prepare_pushes_only_tag_and_keeps_source_parent(self):
        plan = {"action": "create", "source_sha": SOURCE, "version": VERSION, "tag": TAG}
        with patch.object(nightly, "validate_bump", return_value=["Cargo.lock"]), patch.object(nightly, "git") as git:
            nightly.prepare(plan)
        calls = [call.args for call in git.call_args_list]
        self.assertEqual(calls[0], ("checkout", "--detach", SOURCE))
        self.assertEqual(calls[-1], ("push", "origin", f"refs/tags/{TAG}:refs/tags/{TAG}"))
        self.assertNotIn("--force", [argument for call in calls for argument in call])
        annotation = json.loads(calls[-2][-1].removeprefix(nightly.ANNOTATION))
        self.assertEqual(annotation["source_sha"], SOURCE)


class BumpTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.original = {}
        for path in [*ROOT.glob("*/Cargo.toml"), ROOT / "assets/installer.iss", ROOT / "Cargo.lock", ROOT / "CHANGELOG.md", ROOT / "bin/bump-version.sh"]:
            relative = path.relative_to(ROOT)
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, target)
            if relative.as_posix() == "ui/Cargo.toml":
                # Keep the fixture's source base stable when the real main
                # version advances; preview guards compare against this file.
                text = target.read_text(encoding="utf-8")
                text = re.sub(r'^version = "[^"]+"', 'version = "0.5.7-ptb"', text, count=1, flags=re.M)
                target.write_text(text, encoding="utf-8")
            self.original[relative.as_posix()] = target.read_text(encoding="utf-8").strip()

    def bump(self, version=VERSION, flags=("--preview",)):
        # Isolate the shell script from Cargo/network. Lockfile validation is
        # tested separately against the real tracked lockfile structure below.
        return subprocess.run(["bash", "-c",
                               'cargo() { printf "%s\\n" "$*" > cargo-invocation.txt; }; export -f cargo; bash bin/bump-version.sh "$@"',
                               "test", *flags, version], cwd=self.root, text=True, capture_output=True,
                              env={**os.environ, "SMUDGY_WEB_DIR": str(self.root / "missing-web")})

    def assert_no_edits(self):
        for path, before in self.original.items():
            self.assertEqual((self.root / path).read_text(encoding="utf-8").strip(), before, path)
        self.assertFalse((self.root / "cargo-invocation.txt").exists())

    def test_normal_bump_keeps_source_unnumbered(self):
        result = self.bump("0.5.8-ptb", flags=())
        self.assertEqual(result.returncode, 0, result.stderr)
        data = tomllib.loads((self.root / "ui/Cargo.toml").read_text(encoding="utf-8"))
        self.assertEqual(data["package"]["version"], "0.5.8-ptb")

    def test_convention_breaking_source_bumps_require_force(self):
        for version in [VERSION, "0.5.8", "0.5.8-rc.1", "0.5.8-ptb10", "0.5.8-ptb-10"]:
            with self.subTest(version=version):
                result = self.bump(version, flags=())
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("--force", result.stderr)
                self.assert_no_edits()

    def test_preview_requires_current_base_and_dotted_positive_number(self):
        for version in ["0.5.8-ptb.1", "0.5.7-ptb5", "0.5.7-ptb-5", "0.5.7-ptb.0", "0.5.7-ptb.05"]:
            with self.subTest(version=version):
                result = self.bump(version)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("--force", result.stderr)
                self.assert_no_edits()

    def test_force_allows_explicit_stable_release(self):
        result = self.bump("0.5.8", flags=("--force",))
        self.assertEqual(result.returncode, 0, result.stderr)
        data = tomllib.loads((self.root / "ui/Cargo.toml").read_text(encoding="utf-8"))
        self.assertEqual(data["package"]["version"], "0.5.8")

    def test_force_does_not_allow_invalid_version(self):
        result = self.bump("not-a-version", flags=("--force",))
        self.assertNotEqual(result.returncode, 0)
        self.assert_no_edits()

    def test_force_preview_allows_explicit_alternate_convention(self):
        result = self.bump("0.5.8-nightly.1", flags=("--force", "--preview"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "CHANGELOG.md").read_text(encoding="utf-8").strip(), self.original["CHANGELOG.md"])

    def test_preview_bumps_every_owned_crate_and_preserves_changelog(self):
        result = self.bump()
        self.assertEqual(result.returncode, 0, result.stderr)
        owned = []
        for path in self.root.glob("*/Cargo.toml"):
            data = tomllib.loads(path.read_text(encoding="utf-8"))
            if data.get("package", {}).get("name", "").startswith("smudgy_"):
                owned.append(path.parent.name)
                self.assertEqual(data["package"]["version"], VERSION)
        self.assertTrue({"i18n", "audio", "audio_web", "theme", "widgets"} <= set(owned))
        self.assertEqual((self.root / "CHANGELOG.md").read_text(encoding="utf-8").strip(), self.original["CHANGELOG.md"])
        self.assertIn(f'#define MyAppVersion "{VERSION}"', (self.root / "assets/installer.iss").read_text())
        self.assertEqual((self.root / "cargo-invocation.txt").read_text().strip(), "metadata --format-version 1")
        self.assertIn("skipping smudgy-web", result.stdout)

    def test_preview_rejects_stable_before_editing(self):
        result = self.bump("0.5.8")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((self.root / "ui/Cargo.toml").read_text(encoding="utf-8").strip(), self.original["ui/Cargo.toml"])

    def prepare_validation_fixture(self):
        result = self.bump()
        self.assertEqual(result.returncode, 0, result.stderr)
        manifests = [path for path in self.original if path.endswith("/Cargo.toml")
                     and tomllib.loads(self.original[path]).get("package", {}).get("name", "").startswith("smudgy_")]
        names = {tomllib.loads(self.original[path])["package"]["name"] for path in manifests}
        lock = self.original["Cargo.lock"]
        blocks = lock.split("[[package]]")
        for i in range(1, len(blocks)):
            parsed = tomllib.loads(blocks[i])
            if parsed["name"] in names and "source" not in parsed:
                blocks[i] = re.sub(r'^version = "[^"]+"', f'version = "{VERSION}"', blocks[i], count=1, flags=re.M)
        lock = "[[package]]".join(blocks)
        for name in names:
            lock = re.sub(r'"' + re.escape(name) + r' [^" ]+"', f'"{name} {VERSION}"', lock)
        (self.root / "Cargo.lock").write_text(lock, encoding="utf-8")
        expected = {*manifests, "Cargo.lock", "assets/installer.iss"}

        def fake_git(*args):
            if args == ("rev-parse", "HEAD"):
                return SOURCE
            if args[0] == "ls-files":
                return "\n".join(manifests)
            if args[0] == "diff":
                return "\n".join(expected)
            if args[0] == "show":
                return self.original[args[1].split(":", 1)[1]]
            raise AssertionError(args)
        return fake_git, expected

    def test_version_only_diff_is_accepted(self):
        fake_git, expected = self.prepare_validation_fixture()
        with working_directory(self.root), patch.object(nightly, "git", side_effect=fake_git):
            self.assertEqual(set(nightly.validate_bump(SOURCE, VERSION)), expected)

    def test_dependency_resolution_change_is_rejected(self):
        fake_git, _ = self.prepare_validation_fixture()
        path = self.root / "Cargo.lock"
        text = path.read_text(encoding="utf-8").replace('checksum = "', 'checksum = "tampered', 1)
        path.write_text(text, encoding="utf-8")
        with working_directory(self.root), patch.object(nightly, "git", side_effect=fake_git):
            with self.assertRaisesRegex(ValueError, "dependency resolution"):
                nightly.validate_bump(SOURCE, VERSION)

    def test_source_advance_aborts_preparation(self):
        with patch.object(nightly, "git", return_value="b" * 40):
            with self.assertRaisesRegex(ValueError, "Source changed"):
                nightly.validate_bump(SOURCE, VERSION)


if __name__ == "__main__":
    unittest.main()
