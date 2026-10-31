"""Stage trusted, validated PGO exports for the existing release packagers."""
import argparse
import json
from pathlib import Path
import shutil
import sys

import pgo


def check(manifest_path, target, destination):
    manifest = pgo.verify_artifact(manifest_path, target, portable=True)
    pgo.require(pgo.sha(destination) == manifest["executable_sha256"], "Packaged PGO executable changed")
    return manifest


def stage(manifest_path, target, destination):
    manifest = pgo.verify_artifact(manifest_path, target, portable=True)
    destination = Path(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(manifest["executable"], destination)
    pgo.require(pgo.sha(destination) == manifest["executable_sha256"], "Staged PGO executable changed")
    return manifest


def flatpak(manifest_path, arch, base_path, output, runtime_version=None):
    # Optional dependency, used only for the Linux packaging overlay.
    import yaml

    target = {"x86_64":"x86_64-unknown-linux-gnu", "aarch64":"aarch64-unknown-linux-gnu"}[arch]
    manifest = pgo.verify_artifact(manifest_path, target, portable=True)
    base = yaml.safe_load(Path(base_path).read_text(encoding="utf-8"))
    pgo.require(runtime_version is None or runtime_version == base["runtime-version"],
                "Runtime override differs from the Flatpak manifest")
    expected_sdk = f"{base['sdk']}/{arch}/{base['runtime-version']}"
    sdk = manifest["build_identity"].get("flatpak") or {}
    refs = [value for section in ("Application", "Runtime") for key, value in sdk.get(section, {}).items()
            if key in {"runtime", "sdk"}]
    pgo.require(expected_sdk in refs, f"Train/build Linux PGO inside the release SDK: {expected_sdk}")
    pgo.require(len(base["modules"]) == 1 and base["modules"][0]["name"] == "smudgy",
                "Review the PGO overlay after changing Flatpak modules")
    module = base["modules"][0]
    # Reuse all release assets/install commands, but never rebuild the validated app.
    installs = [command for command in module["build-commands"] if command.startswith("install ")]
    app_install = "install -Dm755 target/release-full/smudgy /app/bin/smudgy.bin"
    pgo.require(app_install in installs, "Flatpak app install command changed; review the PGO overlay")
    pgo.require(not module.get("post-install") and not module.get("cleanup") and not base.get("cleanup"),
                "Review Flatpak post-install/cleanup commands before PGO packaging")
    output = Path(output).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    binary = output.parent/"smudgy-pgo.bin"
    stage(manifest_path, target, binary)
    module["build-commands"] = [f"echo '{manifest['executable_sha256']}  smudgy-pgo.bin' | sha256sum -c -"] + [
        command.replace("target/release-full/smudgy ", "smudgy-pgo.bin ") if command == app_install else command
        for command in installs]
    for source in module["sources"]:
        pgo.require(source["type"] == "dir", "Review new Flatpak sources before PGO packaging")
        source["path"] = str((Path(base_path).resolve().parent/source["path"]).resolve())
    module["sources"].append(dict(type="file", path=str(binary), **{"dest-filename":"smudgy-pgo.bin"}))
    # Preserve the exact validated bytes in the package as well as in staging.
    base.setdefault("build-options", {}).update({"strip": False, "no-debuginfo": True})
    output.write_text(json.dumps(base, indent=2) + "\n", encoding="utf-8")
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    copy = commands.add_parser("stage")
    copy.add_argument("manifest", type=Path)
    copy.add_argument("destination", type=Path)
    copy.add_argument("--target", choices=sorted(pgo.TARGETS), required=True)
    verify = commands.add_parser("check")
    verify.add_argument("manifest", type=Path)
    verify.add_argument("destination", type=Path)
    verify.add_argument("--target", choices=sorted(pgo.TARGETS), required=True)
    linux = commands.add_parser("flatpak")
    linux.add_argument("manifest", type=Path)
    linux.add_argument("output", type=Path)
    linux.add_argument("--arch", choices=["x86_64", "aarch64"], required=True)
    linux.add_argument("--runtime-version", help="Expected release runtime branch")
    linux.add_argument("--base", type=Path, default=pgo.ROOT/"packaging/linux/org.smudgy.Smudgy.yml")
    args = parser.parse_args()
    if args.command == "stage":
        stage(args.manifest, args.target, args.destination)
    elif args.command == "check":
        check(args.manifest, args.target, args.destination)
    else:
        print(flatpak(args.manifest, args.arch, args.base, args.output, args.runtime_version))


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ImportError) as error:
        sys.exit(str(error))
