"""Stage trusted, validated PGO exports for the existing release packagers."""
import argparse
import hashlib
import json
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile

import pgo


MAC_SLICES = {"arm64": "aarch64-apple-darwin", "x86_64": "x86_64-apple-darwin"}
# Build inputs the two Mac exports must share before their slices may be fused. Paths to
# per-architecture native archives legitimately differ, so those variables are not paired.
PAIRED_ENVIRONMENT = ("MACOSX_DEPLOYMENT_TARGET", "SDKROOT", "CC", "CXX", "CFLAGS", "CXXFLAGS", "LDFLAGS",
                      "V8_FROM_SOURCE", "V8_FORCE_DEBUG", "GN_ARGS", "EXTRA_GN_ARGS", *pgo.EMBEDDED_COMMIT_VARIABLES)


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


def pair_identity(manifest):
    identity = manifest["build_identity"]
    environment = identity.get("native_environment") or {}
    return dict(
        revision=manifest["revision"], files=identity["files"], compiler=identity.get("compiler"),
        cargo=identity.get("cargo"), helper_sha256=identity.get("helper_sha256"),
        # Cargo configs and local dependencies are keyed by machine paths; pair their contents.
        configs=sorted(identity.get("configs", {}).values()),
        local_dependencies=sorted(json.dumps(tree, sort_keys=True) for tree in identity.get("local_dependencies", {}).values()),
        environment={name: environment.get(name) for name in PAIRED_ENVIRONMENT},
        base_flags=[flag for flag in manifest["build"].get("flags", [])
                    if not flag.startswith(("-Cprofile-use=", "-Cllvm-args="))])


def check_pair(arm64, x86_64):
    """Two Mac exports may only be fused when they were built from one frozen checkout,
    toolchain, flag set and build environment; each must also record its deployment target."""
    first, second = pair_identity(arm64), pair_identity(x86_64)
    different = [key for key in first if first[key] != second[key]]
    pgo.require(not different, f"Mac exports differ in {', '.join(different)}; rebuild both from one frozen checkout and environment")
    for manifest in (arm64, x86_64):
        pgo.require(manifest["build_identity"].get("deployment_target"),
                    "Mac export predates deployment-target recording; export again with this helper")


def pair(arm64_path, x86_64_path):
    manifests = {arch: pgo.verify_artifact(path, target, portable=True)
                 for (arch, target), path in zip(MAC_SLICES.items(), (arm64_path, x86_64_path))}
    check_pair(manifests["arm64"], manifests["x86_64"])
    return manifests


def universal(binary, arm64_path, x86_64_path, receipt):
    """Prove a fused Mach-O holds exactly the two validated slices, byte for byte, and
    record its pre-signature digest. lipo copies each input verbatim at an aligned offset,
    so thinning the slice back out must reproduce the export's hash."""
    binary = Path(binary).resolve()
    manifests = pair(arm64_path, x86_64_path)
    archs = pgo.capture("lipo", "-archs", str(binary)).split()
    pgo.require(sorted(archs) == sorted(MAC_SLICES), f"Universal binary must hold exactly arm64 and x86_64, not {archs}")
    slices = {}
    with tempfile.TemporaryDirectory() as temp:
        for arch, manifest in manifests.items():
            thin = Path(temp)/arch
            subprocess.run(["lipo", str(binary), "-thin", arch, "-output", str(thin)], check=True)
            pgo.require(pgo.sha(thin) == manifest["executable_sha256"], f"Universal {arch} slice differs from the validated export")
            slices[arch] = dict(executable_sha256=manifest["executable_sha256"], target=manifest["target"],
                                deployment_target=manifest["build_identity"]["deployment_target"])
    receipt = Path(receipt).resolve()
    receipt.parent.mkdir(parents=True, exist_ok=True)
    pgo.write(receipt, dict(schema=1, stage="universal-pgo", universal=str(binary), universal_sha256=pgo.sha(binary),
                            revision=manifests["arm64"]["revision"], slices=slices))
    return receipt


def committed(manifest_path, repo, arch, ostree="ostree", app_id="org.smudgy.Smudgy", branch="master"):
    """Hash the executable inside the exported ostree commit, which is what build-bundle
    ships; the build directory that flatpak-builder leaves behind is not."""
    target = {"x86_64": "x86_64-unknown-linux-gnu", "aarch64": "aarch64-unknown-linux-gnu"}[arch]
    manifest = pgo.verify_artifact(manifest_path, target, portable=True)
    command = [*shlex.split(ostree), f"--repo={Path(repo).resolve()}", "cat", f"app/{app_id}/{arch}/{branch}", "/files/bin/smudgy.bin"]
    result = subprocess.run(command, stdout=subprocess.PIPE, check=True)
    pgo.require(hashlib.sha256(result.stdout).hexdigest() == manifest["executable_sha256"],
                f"Committed Flatpak executable differs from the validated export: {' '.join(command)}")
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
    # /.flatpak-info records refs with their kind prefix: runtime/<id>/<arch>/<branch>.
    refs = [value.removeprefix("runtime/") for section in ("Application", "Runtime")
            for key, value in sdk.get(section, {}).items() if key in {"runtime", "sdk"}]
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
    # The overlay compiles nothing, so the module's build-time network grant (and any other
    # build option meant for the compile) is dropped; the app's finish-args are untouched.
    module.pop("build-options", None)
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
    shipped = commands.add_parser("committed", help="Compare the executable in the exported ostree commit with the export")
    shipped.add_argument("manifest", type=Path)
    shipped.add_argument("repo", type=Path)
    shipped.add_argument("--arch", choices=["x86_64", "aarch64"], required=True)
    shipped.add_argument("--ostree", default="ostree", help="ostree command, e.g. 'flatpak run --command=ostree org.flatpak.Builder'")
    mac = commands.add_parser("pair", help="Require both Mac exports to share one checkout, toolchain and environment")
    mac.add_argument("arm64", type=Path)
    mac.add_argument("x86_64", type=Path)
    fused = commands.add_parser("universal", help="Verify a lipo output against both Mac exports and write a receipt")
    fused.add_argument("binary", type=Path)
    fused.add_argument("--arm64", type=Path, required=True)
    fused.add_argument("--x86_64", type=Path, required=True)
    fused.add_argument("--receipt", type=Path, default=pgo.ROOT/"target/pgo-package/mac-universal.json")
    args = parser.parse_args()
    if args.command == "stage":
        stage(args.manifest, args.target, args.destination)
    elif args.command == "check":
        check(args.manifest, args.target, args.destination)
    elif args.command == "committed":
        committed(args.manifest, args.repo, args.arch, args.ostree)
    elif args.command == "pair":
        pair(args.arm64, args.x86_64)
    elif args.command == "universal":
        print(universal(args.binary, args.arm64, args.x86_64, args.receipt))
    else:
        print(flatpak(args.manifest, args.arch, args.base, args.output, args.runtime_version))


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ImportError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
