"""Probe coverage must remain strict outside reviewed Mac startup helpers."""
import unittest

import pgo_smoke as smoke


ARM_WARNING = (
    "warning: probe.42cd7a695e74b762-cgu.0: no profile data available for function "
    "_RINvNtCsa9BKTri5B3M_3std2rt10lang_startuECs5JAvRntY6Pu_5probe "
    "Hash = 742261418966908927 up to 0 count discarded\nwarning: 1 warning emitted\n"
)
INTEL_WARNING = ARM_WARNING.replace("Csa9BKTri5B3M", "CsgejaSCmAoRz")


def record(name, counts):
    return f"  {name}:\n    Hash: 0x123\n    Counters: {len(counts)}\n    Block counts: {counts}\n"


class SmokeTests(unittest.TestCase):
    def test_exact_counts_from_both_training_processes(self):
        profile = record("main", [2]) + record("module;_RNvCs5JAvRntY6Pu_5probe4work", [21001, 3001])
        smoke.check_probe_counts(profile)
        for changed in (profile.replace("21001", "21000"), profile.replace("3001", "3000"),
                        profile.replace("[2]", "[1]"), record("main", [2]), ""):
            with self.subTest(profile=changed), self.assertRaises(RuntimeError):
                smoke.check_probe_counts(changed)

    def test_clean_compiler_output_needs_no_exception(self):
        self.assertEqual(smoke.check_use_diagnostics("", "x86_64-pc-windows-msvc", "any compiler"), [])

    def test_reviewed_helpers_are_target_specific(self):
        for target, warning in (("aarch64-apple-darwin", ARM_WARNING),
                                ("x86_64-apple-darwin", INTEL_WARNING)):
            with self.subTest(target=target):
                self.assertEqual(len(smoke.check_use_diagnostics(warning, target, smoke.REVIEWED_COMPILER)), 1)
        for target in ("x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu", "x86_64-apple-darwin"):
            with self.subTest(target=target), self.assertRaises(RuntimeError):
                smoke.check_use_diagnostics(ARM_WARNING, target, smoke.REVIEWED_COMPILER)

    def test_changed_compiler_requires_new_review(self):
        with self.assertRaises(RuntimeError):
            smoke.check_use_diagnostics(ARM_WARNING, "aarch64-apple-darwin", "rustc 1.98.0")

    def test_missing_application_function_is_not_reviewed(self):
        with self.assertRaises(RuntimeError):
            smoke.check_use_diagnostics(ARM_WARNING.replace("10lang_startuE", "4work"),
                                        "aarch64-apple-darwin", smoke.REVIEWED_COMPILER)

    def test_changed_hash_counts_or_additional_diagnostics_are_rejected(self):
        for changed in (ARM_WARNING.replace("742261418966908927", "42"),
                        ARM_WARNING.replace("up to 0", "up to 1"),
                        ARM_WARNING + "warning: profile hash mismatch\n",
                        ARM_WARNING + "warning: unrelated linker diagnostic\n"):
            with self.subTest(diagnostic=changed), self.assertRaises(RuntimeError):
                smoke.check_use_diagnostics(changed, "aarch64-apple-darwin", smoke.REVIEWED_COMPILER)


if __name__ == "__main__":
    unittest.main()
