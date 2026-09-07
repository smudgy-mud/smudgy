#!/usr/bin/env python3
"""Run accessibility and editor focus regressions in Smudgy's patched iced crates."""

from __future__ import annotations

import pathlib
import shutil
import subprocess
import tempfile
import tomllib


ROOT = pathlib.Path(__file__).resolve().parent.parent
PATCH_ROOT = ROOT / "target" / "patch"
CRATES = ("iced_runtime-0.14.0", "iced_winit-0.14.0", "iced-code-editor")
SUPPORT_PATCHES = ("cosmic-text-0.15.0", "iced_graphics-0.14.0")


def run(*args: str, cwd: pathlib.Path) -> None:
    print(f"+ {' '.join(args)}", flush=True)
    subprocess.run(args, cwd=cwd, check=True)


def run_silent(*args: str, cwd: pathlib.Path) -> None:
    print(f"+ {' '.join(args)}", flush=True)
    subprocess.run(args, cwd=cwd, check=True, stdout=subprocess.DEVNULL)


def remove_unlocked_optional_edges(manifest_path: pathlib.Path) -> None:
    """Keep dependency unit tests on the application's exact Cargo.lock graph."""
    text = manifest_path.read_text(encoding="utf-8")
    replacements = {
        "iced-code-editor": (
            ('two-face = ["dep:two-face"]\n', 'two-face = []\n'),
            (
                'two-face = { version = "0.5", optional = true, default-features = false, features = ["syntect-fancy"] }\n',
                "",
            ),
            ("[workspace]\n", ""),
        ),
        "iced_runtime": (
            ('selector = ["dep:iced_selector"]\n', "selector = []\nsipper = []\n"),
            (
                "[dependencies.iced_selector]\nversion = \"0.14.0\"\noptional = true\n\n",
                "",
            ),
            (
                "[dependencies.sipper]\nversion = \"0.1\"\noptional = true\n\n",
                "",
            ),
        ),
        "iced_winit": (
            ('sysinfo = ["dep:sysinfo"]\n', "sysinfo = []\n"),
            (
                "[dependencies.sysinfo]\nversion = \"0.33\"\noptional = true\n\n",
                "",
            ),
        ),
    }
    crate = tomllib.loads(text)["package"]["name"]

    for old, new in replacements[crate]:
        if text.count(old) != 1:
            raise RuntimeError(
                f"expected one reviewed optional edge in {manifest_path}: {old!r}"
            )
        text = text.replace(old, new)

    manifest_path.write_text(text, encoding="utf-8")


def locked_packages(lockfile: pathlib.Path) -> set[tuple[str, str, str, str]]:
    with lockfile.open("rb") as source:
        packages = tomllib.load(source)["package"]

    return {
        (
            package["name"],
            package["version"],
            package.get("source", ""),
            package.get("checksum", ""),
        )
        for package in packages
    }


def main() -> None:
    # Always rematerialize so the tests exercise the tracked patch files, not
    # unrecorded edits left in the ignored target/patch working directories.
    run("cargo", "patch-crate", "--force", cwd=ROOT)

    with tempfile.TemporaryDirectory(prefix="smudgy-iced-accessibility-") as temporary:
        temporary_root = pathlib.Path(temporary)
        workspace = temporary_root / "workspace"
        workspace.mkdir()

        for crate in CRATES:
            source = PATCH_ROOT / crate
            if not source.is_dir():
                raise RuntimeError(f"patch-crate did not materialize {source}")
            shutil.copytree(source, workspace / crate)
            remove_unlocked_optional_edges(workspace / crate / "Cargo.toml")

        for crate in SUPPORT_PATCHES:
            source = PATCH_ROOT / crate
            if not source.is_dir():
                raise RuntimeError(f"patch-crate did not materialize {source}")
            shutil.copytree(source, temporary_root / crate)

        (workspace / "Cargo.toml").write_text(
            """\
[workspace]
members = ["iced_runtime-0.14.0", "iced_winit-0.14.0", "iced-code-editor"]
resolver = "2"

[patch.crates-io]
iced_runtime = { path = "iced_runtime-0.14.0" }
iced_winit = { path = "iced_winit-0.14.0" }
cosmic-text = { path = "../cosmic-text-0.15.0" }
iced_graphics = { path = "../iced_graphics-0.14.0" }
""",
            encoding="utf-8",
        )
        shutil.copy2(ROOT / "Cargo.lock", workspace / "Cargo.lock")

        # Cargo insists on pruning packages that belong only to Smudgy's main
        # workspace. Seed that normalization with its lock, then prove the
        # smaller graph introduced no package/version outside the reviewed lock.
        run_silent("cargo", "metadata", "--format-version", "1", cwd=workspace)
        root_packages = locked_packages(ROOT / "Cargo.lock")
        test_packages = locked_packages(workspace / "Cargo.lock")
        unexpected = sorted(test_packages - root_packages)
        if unexpected:
            raise RuntimeError(
                f"patch tests resolved packages outside the application lock: {unexpected}"
            )

        run(
            "cargo",
            "test",
            "-p",
            "iced-code-editor",
            "--lib",
            "input_focus_survives_sticky_header_transitions",
            "--locked",
            "--",
            "--test-threads=1",
            cwd=workspace,
        )
        run(
            "cargo",
            "test",
            "-p",
            "iced_runtime",
            "--lib",
            "--locked",
            cwd=workspace,
        )
        run(
            "cargo",
            "test",
            "-p",
            "iced_winit",
            "--lib",
            "--locked",
            cwd=workspace,
        )


if __name__ == "__main__":
    main()
