#!/usr/bin/env python3
"""Fail when Smudgy's reviewed Web Audio release wiring drifts."""

from __future__ import annotations

import pathlib
import sys
import tomllib


ROOT = pathlib.Path(__file__).resolve().parent.parent
DENO_AUDIO_REV = "ccc327d25f0f358509dd735cf8fce7db710b2b40"
WEB_AUDIO_API_REV = "75dfdeb992f9ae8e63a76e3fcf471ac56f31836d"
RELEASE_FEATURE = "smudgy_ui/web-audio-cpal"
CARGO_ABOUT_VERSION = "0.9.0"
CARGO_BUNDLE_VERSION = "0.7.0"
WINDOWS_NOTICE_NORMALIZER = "python bin/normalize-third-party-notices.py"
POSIX_NOTICE_NORMALIZER = "python3 bin/normalize-third-party-notices.py"


def load_toml(relative: str) -> dict:
    with (ROOT / relative).open("rb") as source:
        return tomllib.load(source)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def require_exact_git_dependency(
    dependency: dict,
    *,
    name: str,
    git: str,
    revision: str,
) -> None:
    require(
        dependency == {"git": git, "rev": revision, "default-features": False},
        f"{name} must use only the reviewed immutable pin with defaults disabled",
    )


def require_feature(manifest: dict, name: str, expected: set[str], *, crate: str) -> None:
    actual = set(manifest.get("features", {}).get(name, []))
    require(
        actual == expected,
        f"{crate} feature {name!r}: expected {sorted(expected)}, got {sorted(actual)}",
    )


def require_optional_workspace_edge(dependency: dict, *, crate: str, name: str) -> None:
    require(
        dependency == {"workspace": True, "optional": True},
        f"{crate} {name} must be exactly optional and inherit the reviewed workspace pin",
    )


def dependency_sites(manifest: dict, dependency: str) -> set[str]:
    sites = {
        section
        for section in ("dependencies", "dev-dependencies", "build-dependencies")
        if dependency in manifest.get(section, {})
    }
    for target, target_manifest in manifest.get("target", {}).items():
        for section in ("dependencies", "dev-dependencies", "build-dependencies"):
            if dependency in target_manifest.get(section, {}):
                sites.add(f"target.{target}.{section}")
    return sites


def continued_shell_commands(text: str) -> list[str]:
    """Join backslash-continued shell commands while ignoring comments/blank lines."""
    commands: list[str] = []
    pending = ""
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        pending = f"{pending} {line}".strip()
        if pending.endswith("\\"):
            pending = pending[:-1].rstrip()
            continue
        commands.append(pending)
        pending = ""
    require(not pending, "release shell script ends in an unfinished command")
    return commands


def yaml_list_item(text: str, header: str) -> list[str]:
    """Return one indentation-bounded YAML list item, with blank lines removed."""
    lines = text.splitlines()
    matches = [index for index, line in enumerate(lines) if line.strip() == header]
    require(len(matches) == 1, f"expected exactly one YAML item {header!r}")
    start = matches[0]
    indentation = len(lines[start]) - len(lines[start].lstrip())
    item = [lines[start].strip()]
    for line in lines[start + 1 :]:
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if len(line) - len(line.lstrip()) <= indentation:
            break
        item.append(stripped)
    return item


def yaml_mapping_entry(text: str, header: str) -> list[str]:
    """Return one indentation-bounded YAML mapping entry as stripped lines."""
    lines = text.splitlines()
    matches = [index for index, line in enumerate(lines) if line.strip() == header]
    require(len(matches) == 1, f"expected exactly one YAML mapping {header!r}")
    start = matches[0]
    indentation = len(lines[start]) - len(lines[start].lstrip())
    entry = [lines[start].strip()]
    for line in lines[start + 1 :]:
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if len(line) - len(line.lstrip()) <= indentation:
            break
        entry.append(stripped)
    return entry


def main() -> None:
    normalizer_path = ROOT / "bin/normalize-third-party-notices.py"
    require(
        normalizer_path.is_file(),
        "release notice normalizer is missing",
    )
    normalizer_text = normalizer_path.read_text(encoding="utf-8")
    normalizer_invariants = (
        'NOTICE = ROOT / "THIRD-PARTY-NOTICES.md"',
        'NOTICE.read_text(encoding="utf-8")',
        '"\\n".join(line.rstrip() for line in source.splitlines()).rstrip() + "\\n"',
        'NOTICE.write_text(normalized, encoding="utf-8", newline="\\n")',
    )
    require(
        all(normalizer_text.count(invariant) == 1 for invariant in normalizer_invariants),
        "release notice normalizer must retain its reviewed UTF-8/LF/trailing-whitespace behavior",
    )

    workspace = load_toml("Cargo.toml")
    about = load_toml("about.toml")
    ui = load_toml("ui/Cargo.toml")
    core = load_toml("core/Cargo.toml")
    script = load_toml("script/Cargo.toml")
    audio = load_toml("audio/Cargo.toml")
    audio_web = load_toml("audio_web/Cargo.toml")

    require_exact_git_dependency(
        workspace["workspace"]["dependencies"]["deno_audio"],
        name="deno_audio",
        git="https://github.com/smudgy-mud/deno_audio.git",
        revision=DENO_AUDIO_REV,
    )
    require_exact_git_dependency(
        audio_web["dev-dependencies"]["web-audio-api"],
        name="web-audio-api-rs",
        git="https://github.com/smudgy-mud/web-audio-api-rs",
        revision=WEB_AUDIO_API_REV,
    )
    workspace_dependencies = workspace["workspace"]["dependencies"]
    require(
        workspace_dependencies["cpal"] == {
            "version": "=0.18.2",
            "default-features": False,
        },
        "workspace CPAL must remain exact 0.18.2 with defaults disabled",
    )
    require(
        workspace_dependencies["kira"] == {
            "version": "0.12.3",
            "default-features": False,
        },
        "workspace Kira must remain 0.12.3 with defaults disabled",
    )

    require_optional_workspace_edge(
        audio["dependencies"]["cpal"], crate="smudgy_audio", name="cpal"
    )
    require(
        audio["dependencies"]["kira"] == {"workspace": True},
        "smudgy_audio Kira edge drifted",
    )
    require(
        audio_web["dependencies"]["deno_audio"] == {"workspace": True},
        "smudgy_audio_web deno_audio edge drifted",
    )
    require_optional_workspace_edge(
        script["build-dependencies"]["deno_audio"],
        crate="smudgy_script build",
        name="deno_audio",
    )
    require_optional_workspace_edge(
        script["dependencies"]["deno_audio"],
        crate="smudgy_script runtime",
        name="deno_audio",
    )
    require(
        script["dev-dependencies"]["deno_audio"] == {"workspace": True},
        "smudgy_script test-only deno_audio edge drifted",
    )
    for dependency in ("deno_audio", "smudgy_audio_web"):
        require_optional_workspace_edge(
            core["dependencies"][dependency], crate="smudgy_core", name=dependency
        )
    for dependency in ("futures", "smudgy_audio", "smudgy_audio_web"):
        require_optional_workspace_edge(
            ui["dependencies"][dependency], crate="smudgy_ui", name=dependency
        )

    allowed_consumers = {
        "cpal": {"audio:dependencies"},
        "kira": {"audio:dependencies"},
        "deno_audio": {
            "audio_web:dependencies",
            "core:dependencies",
            "script:build-dependencies",
            "script:dependencies",
            "script:dev-dependencies",
        },
    }
    observed_consumers = {dependency: set() for dependency in allowed_consumers}
    # theme/ and widgets/ are first-party UI path dependencies but deliberately
    # not workspace members, so include them in the native-stack escape scan.
    reviewed_crates = [*workspace["workspace"]["members"], "theme", "widgets"]
    for member in reviewed_crates:
        manifest = load_toml(f"{member}/Cargo.toml")
        for dependency in observed_consumers:
            observed_consumers[dependency].update(
                f"{member}:{site}"
                for site in dependency_sites(manifest, dependency)
            )
    for dependency, expected in allowed_consumers.items():
        require(
            observed_consumers[dependency] == expected,
            f"{dependency} consumers: expected {sorted(expected)}, "
            f"got {sorted(observed_consumers[dependency])}",
        )

    require_feature(
        audio,
        "physical-output",
        {"dep:cpal", "dep:rtrb"},
        crate="smudgy_audio",
    )
    require_feature(script, "web-audio", {"dep:deno_audio", "dep:libc"}, crate="smudgy_script")
    require_feature(
        core,
        "web-audio",
        {"dep:deno_audio", "dep:smudgy_audio_web", "smudgy_script/web-audio"},
        crate="smudgy_core",
    )
    require_feature(core, "web-audio-cpal", {"web-audio"}, crate="smudgy_core")
    require_feature(ui, "web-audio", {"smudgy_core/web-audio"}, crate="smudgy_ui")
    require_feature(
        ui,
        "web-audio-cpal",
        {
            "web-audio",
            "dep:futures",
            "dep:smudgy_audio",
            "dep:smudgy_audio_web",
            "smudgy_audio/physical-output",
        },
        crate="smudgy_ui",
    )
    release_targets = [
        "x86_64-pc-windows-msvc",
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
    ]
    require(
        about.get("targets") == release_targets,
        "cargo-about target union must exactly cover every shipped release target",
    )

    require(
        ui.get("features", {}).get("default") == ["web-audio-cpal"],
        "smudgy_ui default must be exactly the product web-audio-cpal feature",
    )
    for crate, manifest in (
        ("smudgy_core", core),
        ("smudgy_script", script),
        ("smudgy_audio", audio),
    ):
        require(
            not manifest.get("features", {}).get("default"),
            f"{crate} defaults must remain hardware-free",
        )

    lock = load_toml("Cargo.lock")
    locked_by_name: dict[str, list[dict]] = {}
    for package in lock["package"]:
        locked_by_name.setdefault(package["name"], []).append(package)

    def require_unique_locked(name: str, version: str, source: str) -> None:
        packages = locked_by_name.get(name, [])
        require(len(packages) == 1, f"Cargo.lock must contain exactly one {name}")
        require(packages[0].get("version") == version, f"Cargo.lock {name} version drifted")
        require(packages[0].get("source") == source, f"Cargo.lock {name} source drifted")

    registry = "registry+https://github.com/rust-lang/crates.io-index"
    require_unique_locked("cpal", "0.18.2", registry)
    require_unique_locked("kira", "0.12.3", registry)
    require_unique_locked(
        "deno_audio",
        "0.1.0-alpha.1",
        f"git+https://github.com/smudgy-mud/deno_audio.git?rev={DENO_AUDIO_REV}#{DENO_AUDIO_REV}",
    )
    require_unique_locked(
        "web-audio-api",
        "1.7.0",
        f"git+https://github.com/smudgy-mud/web-audio-api-rs?rev={WEB_AUDIO_API_REV}#{WEB_AUDIO_API_REV}",
    )

    windows_text = (ROOT / "bin/release.ps1").read_text(encoding="utf-8")
    windows_command = (
        "cargo build --profile release-full -p smudgy_ui -p smudgy_inspector "
        f"--features {RELEASE_FEATURE}"
    )
    require(
        [
            line.strip()
            for line in windows_text.splitlines()
            if line.strip().startswith(("cargo build ", "cargo bundle "))
        ]
        == [windows_command],
        "Windows release cargo build/bundle command list drifted",
    )
    notice_command = (
        "cargo about generate --workspace "
        f"--features {RELEASE_FEATURE} --locked about.hbs "
        "-o THIRD-PARTY-NOTICES.md"
    )
    require(
        [
            line.strip()
            for line in windows_text.splitlines()
            if line.strip().startswith("cargo about generate ")
        ]
        == [notice_command],
        "Windows release must generate the feature/target-union notice exactly",
    )
    require(
        sum(
            line.strip() == WINDOWS_NOTICE_NORMALIZER
            for line in windows_text.splitlines()
        )
        == 1,
        "Windows release must normalize the generated notice exactly once",
    )
    require(
        windows_text.count(
            f"$cargoAboutVersion = (& cargo about --version 2>$null)"
        )
        == 1
        and windows_text.count(
            f"$cargoAboutVersion -ne 'cargo-about {CARGO_ABOUT_VERSION}'"
        )
        == 1,
        "Windows release must require the pinned cargo-about version",
    )
    installer_text = (ROOT / "assets/installer.iss").read_text(encoding="utf-8")
    installer_notice = (
        'Source: "..\\THIRD-PARTY-NOTICES.md"; DestDir: "{app}"; '
        "Flags: ignoreversion"
    )
    require(
        sum(line.strip() == installer_notice for line in installer_text.splitlines()) == 1,
        "Windows installer must ship the canonical third-party notice exactly once",
    )

    mac_text = (ROOT / "bin/release-mac.sh").read_text(encoding="utf-8")
    mac_commands = continued_shell_commands(mac_text)
    mac_build = (
        'cargo build --profile "$PROFILE" --target "$t" '
        f"-p smudgy_ui -p smudgy_inspector --features {RELEASE_FEATURE}"
    )
    mac_bundle = (
        '( cd ui && cargo bundle --profile "$PROFILE" --target "$BUNDLE_TARGET" '
        f"--format osx --features {RELEASE_FEATURE} )"
    )
    require(
        [
            command
            for command in mac_commands
            if command.startswith(("cargo build ", "cargo bundle ", "( cd ui && cargo bundle "))
        ]
        == [
            mac_build,
            mac_bundle,
        ],
        "macOS release cargo build/bundle command list drifted",
    )
    require(
        mac_text.count(
            '[[ "$(cargo bundle --version 2>/dev/null)" '
            f'== "cargo-bundle v{CARGO_BUNDLE_VERSION}" ]]'
        )
        == 1,
        "macOS release must require the pinned cargo-bundle version",
    )
    require(
        [
            command
            for command in mac_commands
            if command.startswith("cargo about generate ")
        ]
        == [notice_command],
        "macOS release must generate the feature/target-union notice exactly",
    )
    require(
        mac_commands.count(POSIX_NOTICE_NORMALIZER) == 1,
        "macOS release must normalize the generated notice exactly once",
    )
    require(
        mac_text.count(
            f'[[ "$(cargo about --version 2>/dev/null)" == "cargo-about {CARGO_ABOUT_VERSION}" ]]'
        )
        == 1,
        "macOS release must require the pinned cargo-about version",
    )
    require(
        mac_text.count(
            'install -m 644 THIRD-PARTY-NOTICES.md '
            '"$resources_dir/THIRD-PARTY-NOTICES.md"'
        )
        == 1
        and mac_text.count(
            'cmp -s THIRD-PARTY-NOTICES.md '
            '"$resources_dir/THIRD-PARTY-NOTICES.md"'
        )
        == 1,
        "macOS release must copy and verify the canonical notice in the app bundle",
    )
    require(
        mac_text.count("TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)") == 1,
        "macOS release target list drifted from the cargo-about union",
    )

    flatpak_text = (ROOT / "packaging/linux/org.smudgy.Smudgy.yml").read_text(
        encoding="utf-8"
    )
    flatpak_lines = [line.strip() for line in flatpak_text.splitlines()]
    flatpak_build = (
        "- cargo build --profile release-full -p smudgy_ui --bin smudgy "
        f"--features {RELEASE_FEATURE}"
    )
    require(
        [
            line
            for line in flatpak_lines
            if line.startswith(("- cargo build ", "- cargo bundle "))
        ]
        == [flatpak_build],
        "Flatpak release cargo build/bundle command list drifted",
    )
    flatpak_notice = f"- {notice_command}"
    require(
        flatpak_lines.count(flatpak_notice) == 1,
        "Flatpak must generate the feature/target-union notice exactly once",
    )
    require(
        flatpak_lines.count(f"- {POSIX_NOTICE_NORMALIZER}") == 1,
        "Flatpak must normalize the generated notice exactly once",
    )
    flatpak_about_install = (
        "- cargo install --locked cargo-about "
        f"--version {CARGO_ABOUT_VERSION} --features cli"
    )
    require(
        flatpak_lines.count(flatpak_about_install) == 1,
        "Flatpak must install the pinned cargo-about version exactly once",
    )
    require(
        flatpak_lines.count("- install -Dm644 THIRD-PARTY-NOTICES.md") == 1
        and flatpak_lines.count(
            "/app/share/doc/smudgy/THIRD-PARTY-NOTICES.md"
        )
        == 1,
        "Flatpak must install the canonical third-party notice exactly once",
    )
    require(
        flatpak_text.count("--socket=pulseaudio") == 1
        and flatpak_lines.count("- --socket=pulseaudio") == 1,
        "Flatpak must grant the PulseAudio socket exactly once",
    )

    workflow_text = (ROOT / ".github/workflows/release.yml").read_text(
        encoding="utf-8"
    )
    cargo_about_install = (
        "cargo install --locked cargo-about "
        f"--version {CARGO_ABOUT_VERSION} --features cli"
    )
    require(
        workflow_text.count(cargo_about_install) == 2,
        "release workflow cargo-about setup drifted",
    )
    require(
        workflow_text.count(
            f"cargo install cargo-bundle --version {CARGO_BUNDLE_VERSION}"
        )
        == 1,
        "release workflow cargo-bundle setup drifted",
    )
    require(
        yaml_list_item(
            workflow_text, f"- name: Audit {RELEASE_FEATURE} release wiring"
        )
        == [
            f"- name: Audit {RELEASE_FEATURE} release wiring",
            "run: python bin/check-web-audio-wiring.py",
        ],
        "release workflow must run the exact physical feature audit as a gating step",
    )
    verify_job = yaml_mapping_entry(workflow_text, "verify-version:")
    audit_name = f"- name: Audit {RELEASE_FEATURE} release wiring"
    require(
        verify_job.count(audit_name) == 1
        and verify_job.count("run: python bin/check-web-audio-wiring.py") == 1,
        "physical feature audit must remain inside verify-version",
    )

    flatpak_job = yaml_mapping_entry(workflow_text, "flatpak:")
    windows_job = yaml_mapping_entry(workflow_text, "windows:")
    macos_job = yaml_mapping_entry(workflow_text, "macos:")
    require(
        flatpak_job.count("needs: verify-version") == 1
        and flatpak_job.count(
            "run: dbus-run-session -- bin/release-linux.sh --arch ${{ matrix.arch }}"
        )
        == 1
        and flatpak_job.count("- arch: x86_64") == 1
        and flatpak_job.count("- arch: aarch64") == 1,
        "Flatpak job must depend on the audit, invoke its release script, and ship both target arches",
    )
    linux_release_text = (ROOT / "bin/release-linux.sh").read_text(encoding="utf-8")
    require(
        [
            line.strip()
            for line in linux_release_text.splitlines()
            if line.strip().startswith("manifest=")
        ]
        == ['manifest="packaging/linux/$APP_ID.yml"']
        and linux_release_text.count("APP_ID=org.smudgy.Smudgy") == 1
        and linux_release_text.count('"$build_dir" "$repo_root/$manifest"') == 1,
        "Flatpak release script must build the reviewed application manifest",
    )
    require(
        windows_job.count("needs: verify-version") == 1
        and windows_job.count("runs-on: windows-latest") == 1
        and windows_job.count(f"run: {cargo_about_install}") == 1
        and windows_job.count("run: ./bin/release.ps1 -BuildOnly") == 1
        and windows_job.count("./bin/release.ps1 -SkipBuild") == 1,
        "Windows job must install pinned cargo-about, depend on the audit, and invoke both release-script stages",
    )
    require(
        macos_job.count("needs: verify-version") == 1
        and macos_job.count("runs-on: macos-latest") == 1
        and macos_job.count(cargo_about_install) == 1
        and macos_job.count(
            f"cargo install cargo-bundle --version {CARGO_BUNDLE_VERSION}"
        )
        == 1
        and macos_job.count("run: bin/release-mac.sh") == 1,
        "macOS job must install pinned packaging tools, depend on the audit, and invoke its release script on macOS",
    )

    for relative, release_text in (
        ("bin/release.ps1", windows_text),
        ("bin/release-mac.sh", mac_text),
        ("packaging/linux/org.smudgy.Smudgy.yml", flatpak_text),
    ):
        require(
            "--all-features" not in release_text,
            f"{relative} must select only the reviewed release feature",
        )

    metainfo_text = (
        ROOT / "packaging/linux/org.smudgy.Smudgy.metainfo.xml"
    ).read_text(encoding="utf-8")
    require(
        "Web Audio" in metainfo_text and "silent" in metainfo_text,
        "Flatpak package metadata must describe physical Web Audio and silent fallback",
    )

    notice_text = (ROOT / "THIRD-PARTY-NOTICES.md").read_text(encoding="utf-8")
    notice_lines = notice_text.splitlines()
    required_physical_notices = {
        "- cpal 0.18.2",
        "- alsa 0.11.0",
        "- alsa-sys 0.4.0",
        "- coreaudio-rs 0.14.2",
        "- mach2 0.6.0",
        "- windows 0.62.2",
        "- windows-core 0.62.2",
    }
    require(
        required_physical_notices.issubset(notice_lines),
        "canonical notice must cover CPAL plus Linux ALSA, macOS CoreAudio, and Windows edges",
    )
    require(
        notice_text.endswith("\n")
        and not notice_text.endswith("\n\n")
        and all(line == line.rstrip() for line in notice_lines),
        "canonical notice must retain deterministic diff-clean normalization",
    )

    print("Web Audio dependency, feature, lockfile, and physical-release wiring is exact.")


if __name__ == "__main__":
    try:
        main()
    except (KeyError, OSError, RuntimeError, tomllib.TOMLDecodeError) as error:
        print(f"Web Audio wiring audit failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
