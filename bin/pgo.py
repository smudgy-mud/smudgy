"""Opt-in native-target PGO: generate, collect, merge, use, validate, and export.

Run `python bin/pgo.py --help`. Workloads are external JSON files; no corpus,
credentials, personal data home, or machine-specific path is built into this tool.
"""
import argparse
import configparser
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import tomllib

TARGETS = {
    "x86_64-pc-windows-msvc": ("win32", "smudgy.exe"),
    "x86_64-unknown-linux-gnu": ("linux", "smudgy"),
    "aarch64-unknown-linux-gnu": ("linux", "smudgy"),
    "x86_64-apple-darwin": ("darwin", "smudgy"),
    "aarch64-apple-darwin": ("darwin", "smudgy"),
}
ROOT = Path(__file__).resolve().parents[1]
FEATURES = "smudgy_ui/web-audio-cpal"
COMPILER_OVERRIDES = {
    "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC", "CARGO_BUILD_RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
}


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def read(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def write(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def capture(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True, encoding="utf-8").strip()


def host_target():
    compiler = capture("rustc", "-vV")
    host = re.search(r"^host: (.+)$", compiler, re.M)
    require(host is not None, "rustc did not report its host target")
    return host[1]


def native_target(target=None):
    host = host_target()
    target = target or host
    require(target in TARGETS, f"Unsupported PGO target: {target}")
    require(TARGETS[target][0] == sys.platform and target == host,
            f"Train and build {target} on its matching native Rust host; this host is {host}")
    return target


def executable_name(target):
    require(target in TARGETS, f"Unsupported PGO target: {target}")
    return TARGETS[target][1]


def source_files():
    names = capture("git", "ls-files", "-c", "-o", "--exclude-standard", "-z").split("\0")
    return {name: sha(ROOT/name) for name in sorted(set(names))
            if name and (ROOT/name).is_file()}


def flatpak_environment():
    """Keep stable SDK identity, excluding ephemeral sandbox instance IDs."""
    path = Path("/.flatpak-info")
    if not path.is_file():
        return None
    info = configparser.ConfigParser(interpolation=None)
    info.read(path, encoding="utf-8")
    return {section: {key: info.get(section, key) for key in keys if info.has_option(section, key)}
            for section, keys in {"Application": ("name", "runtime", "sdk"),
                                  "Runtime": ("name", "runtime", "sdk"),
                                  "Instance": ("runtime-commit", "app-commit", "arch", "branch")}.items()}


def native_inputs(target):
    """Fingerprint optional externally built V8 inputs, including their profile."""
    files = {}
    for name in ("RUSTY_V8_ARCHIVE", "RUSTY_V8_SRC_BINDING_PATH"):
        value = os.environ.get(name)
        if value:
            path = Path(value).resolve()
            require(path.is_file(), f"PGO requires a local, immutable {name}")
            files[name] = dict(path=str(path), sha256=sha(path))
    manifest_path = os.environ.get("SMUDGY_NATIVE_PGO_MANIFEST")
    if manifest_path:
        manifest = read(manifest_path)
        require(manifest.get("schema") == 1 and manifest.get("stage") == "native-pgo-use",
                "Expected a native profile-use manifest")
        require(manifest.get("target") == target, "Native V8 PGO target differs from Rust target")
        require(manifest.get("compiler") and manifest.get("source_sha256"), "Native manifest needs compiler and source provenance")
        for name, key in (("RUSTY_V8_ARCHIVE", "archive"), ("RUSTY_V8_SRC_BINDING_PATH", "bindings")):
            require(name in files and files[name]["sha256"] == manifest[key]["sha256"],
                    f"Native PGO {key} differs from selected V8 input")
        profile = manifest["profile"]
        require(sha(profile["path"]) == profile["sha256"], "Native PGO profile changed")
        files["native_pgo"] = dict(path=str(Path(manifest_path).resolve()),
                                   sha256=sha(manifest_path), manifest=manifest)
    return files


def identity(target=None):
    """Include local patch sources and Cargo configuration, not just Git HEAD."""
    forbidden = [key for key in os.environ if os.environ[key] and (
        key in COMPILER_OVERRIDES | {"RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"}
        or key.startswith("CARGO_PROFILE_") or key.endswith("_RUSTFLAGS"))]
    require(not forbidden, f"Unset build overrides before PGO: {forbidden}")
    configs = {}
    config_directories = [directory/".cargo" for directory in [ROOT, *ROOT.parents]]
    config_directories.append(Path(os.environ.get("CARGO_HOME") or Path.home()/".cargo"))
    for directory in config_directories:
        for name in ("config", "config.toml"):
            path = directory / name
            if path.is_file():
                content = path.read_text(encoding="utf-8")
                require("rustflags" not in content, f"Move rustflags from {path} into --rustflag; implicit flags are unsupported")
                config = tomllib.loads(content)
                require(not config.get("include"),
                        f"Flatten included Cargo configs before PGO: {path}; all build inputs must be fingerprinted")
                require(not any(config.get("build", {}).get(key) for key in
                                ("rustc", "rustc-wrapper", "rustc-workspace-wrapper")),
                        f"Remove compiler/wrapper overrides from {path}; PGO must use the rustc resolved on PATH")
                require(not (COMPILER_OVERRIDES | {"RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"}) & config.get("env", {}).keys(),
                        f"Remove compiler/flag environment overrides from {path}")
                configs[str(path)] = sha(path)
    target = native_target(target)
    files = source_files()
    # Cargo metadata resolves the same shipping feature graph, including path
    # dependencies outside the checkout and generated target/patch sources.
    metadata = json.loads(capture("cargo", "metadata", "--locked", "--format-version", "1",
                                  "--filter-platform", target, "--features", FEATURES))
    local_dependencies = {}
    for package in metadata["packages"]:
        if package["source"] is not None:
            continue
        directory = Path(package["manifest_path"]).parent.resolve()
        if directory.is_relative_to(ROOT) and not directory.is_relative_to(ROOT/"target"):
            continue
        local_dependencies[str(directory)] = {
            str(p.relative_to(directory)): sha(p) for p in sorted(directory.rglob("*"))
            if p.is_file() and not any(part in {".git", "target", "__pycache__"} for part in p.relative_to(directory).parts)
        }
    native_environment = {name: os.environ.get(name) for name in (
        "RUSTC", "RUSTY_V8_ARCHIVE", "V8_FROM_SOURCE", "V8_FORCE_DEBUG",
        "RUSTY_V8_SRC_BINDING_PATH", "SMUDGY_NATIVE_PGO_MANIFEST", "GN_ARGS", "EXTRA_GN_ARGS",
        "CC", "CXX", "CL", "CFLAGS", "CXXFLAGS", "LDFLAGS", "LIB", "INCLUDE",
        "MACOSX_DEPLOYMENT_TARGET", "SDKROOT")}
    return dict(compiler=capture("rustc", "-vV"), cargo=capture("cargo", "-V"),
                target=target, files=files, configs=configs, local_dependencies=local_dependencies,
                native_inputs=native_inputs(target), flatpak=flatpak_environment(),
                native_environment=native_environment, helper_sha256=sha(__file__))


def build(run, stage, rustflags=(), target=None):
    target = native_target(target if stage == "generate" else read(run/"run.json")["target"])
    current = identity(target)
    if stage == "generate":
        run.mkdir(parents=True, exist_ok=False)
        require(not any("profile-" in flag or "pgo-" in flag for flag in rustflags), "PGO flags are managed by this tool")
        config = dict(source=str(ROOT), revision=capture("git", "rev-parse", "HEAD"),
                      target=target, features=FEATURES, base_flags=list(rustflags), identity=current)
        write(run/"run.json", config)
        (run/"raw-unassigned").mkdir()
        flags = [*rustflags, f"-Cprofile-generate={run/'raw-unassigned'}"]
    else:
        config = read(run/"run.json")
        require(current == config["identity"], "Source, toolchain, configuration or local dependencies changed; start a new PGO run")
        merge = read(run/"merge.json")
        require(sha(run/"merged.profdata") == merge["sha256"], "Merged profile changed")
        check_build(run/"generate/manifest.json", target)
        flags = [*config["base_flags"], f"-Cprofile-use={run/'merged.profdata'}", "-Cllvm-args=-pgo-warn-missing-function"]
    output = run/stage
    output.mkdir()
    env = dict(os.environ, CARGO_TARGET_DIR=str(output), CARGO_ENCODED_RUSTFLAGS="\x1f".join(flags))
    env.pop("LLVM_PROFILE_FILE", None)
    command = ["cargo", "build", "--locked", "--profile", "release-full", "--target", target,
               "-p", "smudgy_ui", "--bin", "smudgy", "--features", FEATURES]
    manifest = dict(stage=stage, target=target, source=str(ROOT), run=str(run), command=command, flags=flags, started=time.time(),
                    profile_sha256=sha(run/"merged.profdata") if stage == "use" else None)
    write(output/"manifest.json", manifest)
    print(f"{stage}: {output/'build.log'}", flush=True)
    with (output/"build.log").open("w", encoding="utf-8") as log:
        result = subprocess.run(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
    manifest.update(exit_code=result.returncode, seconds=time.time()-manifest["started"])
    write(output/"manifest.json", manifest)
    require(result.returncode == 0, f"Build failed; see {output/'build.log'}")
    require(identity(target) == current, "Inputs changed during compilation; discard this run")
    log = (output/"build.log").read_text(encoding="utf-8", errors="replace")
    diagnostics = [line for line in log.splitlines() if re.search(r"(no profile data|profile.*(out of date|hash mismatch|missing)|function control flow change)", line, re.I)]
    exe = output/target/"release-full"/executable_name(target)
    manifest.update(executable=str(exe), executable_sha256=sha(exe), pgo_diagnostics=diagnostics,
                    inputs_verified=True)
    pdb = exe.with_suffix(".pdb")
    if pdb.exists():
        manifest.update(pdb=str(pdb), pdb_sha256=sha(pdb))
    write(output/"manifest.json", manifest)
    require(not diagnostics, f"PGO coverage diagnostics require investigation: {output/'build.log'}")
    print(json.dumps({key: manifest[key] for key in ("stage", "seconds", "executable")}), flush=True)


def check_build_receipt(build, target=None):
    require(build.get("exit_code") == 0 and build.get("inputs_verified") and not build.get("pgo_diagnostics"), "Build is incomplete or has unresolved PGO diagnostics")
    require(target is None or build.get("target") == target, "Build target differs from PGO run")


def check_build(path, target=None):
    build = read(path)
    check_build_receipt(build, target)
    require(sha(build["executable"]) == build["executable_sha256"], "Executable differs from build manifest")
    return build


def workload(run, label, scenario_path, training):
    require(re.fullmatch(r"[A-Za-z0-9_-]+", label), "Use letters, digits, underscores and hyphens in labels")
    config = read(run/"run.json")
    require(identity(config["target"]) == config["identity"], "Build inputs changed")
    stage = "generate" if training else "use"
    built = check_build(run/stage/"manifest.json", config["target"])
    scenario = read(scenario_path)
    require(scenario.get("verify") and scenario.get("categories"), "Scenario needs a verification command and workload categories")
    extra_env = scenario.get("env", {})
    require("LLVM_PROFILE_FILE" not in extra_env, "The collector owns LLVM_PROFILE_FILE")
    output = run/("training" if training else "validation")/label
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, **extra_env, PGO_SCENARIO_OUTPUT=str(output), PGO_EXECUTABLE=built["executable"])
    if training:
        (output/"raw").mkdir()
        env["LLVM_PROFILE_FILE"] = str(output/"raw/smudgy-%p-%m.profraw")
    else:
        env.pop("LLVM_PROFILE_FILE", None)
    inputs = {str(Path(p).resolve()): sha(p) for p in scenario.get("inputs", [])}
    command = [built["executable"], *scenario.get("args", [])]
    manifest = dict(stage=stage, executable_sha256=built["executable_sha256"], scenario=scenario,
                    scenario_sha256=sha(scenario_path), inputs=inputs, command=command, started=time.time())
    write(output/"manifest.json", manifest)
    print(f"{label}: close the application normally when the workload is complete", flush=True)
    with (output/"application.log").open("w", encoding="utf-8") as log:
        process = subprocess.Popen(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
        manifest["pid"] = process.pid
        write(output/"manifest.json", manifest)
        code = process.wait()
    manifest.update(exit_code=code, seconds=time.time()-manifest["started"])
    write(output/"manifest.json", manifest)
    require(code == 0, "Application did not exit normally; run cannot be accepted")
    require(all(sha(path) == digest for path, digest in inputs.items()), "Workload inputs changed")
    # The verifier checks exact expected counts/completion and held-out evidence.
    # It executes without a shell and only after the application has exited.
    with (output/"verify.log").open("w", encoding="utf-8") as log:
        result = subprocess.run(scenario["verify"], cwd=scenario_path.parent, env=env, stdout=log, stderr=subprocess.STDOUT)
    manifest["verification_exit_code"] = result.returncode
    manifest["raw"] = {str(p): sha(p) for p in sorted((output/"raw").glob("*.profraw"))} if training else {}
    write(output/"manifest.json", manifest)
    require(result.returncode == 0, f"Workload verification failed: {output/'verify.log'}")
    require(not training or manifest["raw"], "No profile was flushed on normal exit")


def accepted(run, directory, executable_hash):
    records = []
    for path in sorted((run/directory).glob("*/manifest.json")):
        record = read(path)
        check_workload_receipt(record, executable_hash)
        records.append(record)
    require(records, f"No verified {directory} workloads")
    return records


def check_workload_receipt(record, executable_hash):
    require(record.get("exit_code") == 0 and record.get("verification_exit_code") == 0,
            "Incomplete or failed workload receipt; preserve it and start a fresh run")
    require(record["executable_sha256"] == executable_hash, "Wrong executable in workload receipt")


def check_coverage(records, validation):
    categories = {c for r in records for c in r["scenario"]["categories"]}
    if validation:
        require({"replay", "captures", "heldout", "interactive"} <= categories,
                "Validation requires replay, captures, held-out and interactive coverage")
    else:
        require({"replay", "captures", "interactive"} <= categories,
                "Training requires replay, captures and interactive coverage")


def merge(run):
    require(not (run/"merge.json").exists(), "Profiles already merged; start a fresh run")
    config = read(run/"run.json")
    require(identity(config["target"]) == config["identity"], "Build inputs changed")
    generated = check_build(run/"generate/manifest.json", config["target"])
    records = accepted(run, "training", generated["executable_sha256"])
    check_coverage(records, validation=False)
    raw = {p: digest for r in records for p, digest in r["raw"].items()}
    require(raw and all(sha(p) == digest for p, digest in raw.items()), "Raw profiles are missing or changed")
    tool = profdata_tool()
    require(tool.is_file(), "Install llvm-tools-preview for the repository toolchain")
    subprocess.run([str(tool), "merge", "--failure-mode=any", "-o", str(run/"merged.profdata"), *raw], check=True)
    summary = subprocess.check_output([str(tool), "show", str(run/"merged.profdata")], text=True, encoding="utf-8")
    (run/"profile-summary.txt").write_text(summary, encoding="utf-8")
    write(run/"merge.json", dict(sha256=sha(run/"merged.profdata"), tool=str(tool), raw=raw, workloads=records))


def export(run):
    config = read(run/"run.json")
    require(identity(config["target"]) == config["identity"], "Build inputs changed")
    built = check_build(run/"use/manifest.json", config["target"])
    require(built["profile_sha256"] == sha(run/"merged.profdata"), "Profile changed after compilation")
    records = accepted(run, "validation", built["executable_sha256"])
    check_coverage(records, validation=True)
    output = run/"dist"
    output.mkdir()
    name = executable_name(config["target"])
    shutil.copy2(built["executable"], output/name)
    if built.get("pdb"):
        require(sha(built["pdb"]) == built["pdb_sha256"], "Symbols changed")
        shutil.copy2(built["pdb"], output/"smudgy.pdb")
    manifest = dict(schema=2, stage="validated-pgo", target=config["target"], run=str(run), executable=str(output/name),
                    executable_name=name, executable_sha256=sha(output/name), revision=config["revision"],
                    build=built, build_identity=config["identity"],
                    profile_sha256=sha(run/"merged.profdata"), validation=records)
    write(output/"manifest.json", manifest)
    print(output/"manifest.json")


def profdata_tool():
    # Tools run on the compiler host, not the cross-compilation target.
    host = host_target()
    name = "llvm-profdata.exe" if sys.platform == "win32" else "llvm-profdata"
    return Path(capture("rustc", "--print", "sysroot"))/"lib/rustlib"/host/"bin"/name


def verify_artifact(path, target=None, portable=False):
    manifest = read(path)
    require(manifest.get("schema") == 2 and manifest.get("stage") == "validated-pgo", "Expected a validated PGO export manifest")
    recorded_target = manifest["target"]
    require(recorded_target == manifest["build_identity"]["target"], "Inconsistent PGO target provenance")
    require(target is None or target == recorded_target, "PGO artifact target mismatch")
    require(manifest["executable_name"] == executable_name(recorded_target), "Wrong exported executable name")
    executable = Path(path).resolve().parent/manifest["executable_name"]
    require(sha(executable) == manifest["executable_sha256"], "PGO artifact hash mismatch")
    if portable:
        require(target is not None, "Portable verification requires an explicit target")
        require(source_files() == manifest["build_identity"]["files"], "Packaging source differs from validated PGO inputs")
        built = manifest["build"]
        check_build_receipt(built, recorded_target)
        require(built["executable_sha256"] == manifest["executable_sha256"] and built["profile_sha256"] == manifest["profile_sha256"], "Inconsistent PGO build provenance")
        records = manifest["validation"]
        require(records, "No verified validation workloads")
        for record in records:
            check_workload_receipt(record, built["executable_sha256"])
        check_coverage(records, validation=True)
        manifest["executable"] = str(executable)
        return manifest
    run = Path(manifest["run"])
    config = read(run/"run.json")
    require(recorded_target == config["target"], "PGO export target differs from original run")
    require(identity(recorded_target) == config["identity"] == manifest["build_identity"], "Packaging source differs from validated PGO inputs")
    built = check_build(run/"use/manifest.json", recorded_target)
    require(manifest["build"] == built, "Build receipt changed")
    require(manifest["executable_sha256"] == built["executable_sha256"], "PGO artifact hash mismatch")
    require(manifest["profile_sha256"] == built["profile_sha256"] == sha(run/"merged.profdata"), "PGO profile changed")
    require(manifest["validation"] == accepted(run, "validation", built["executable_sha256"]), "Validation records changed")
    manifest["executable"] = str(executable)
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stage", choices=["generate", "train", "merge", "use", "validate", "export", "verify"])
    parser.add_argument("path", type=Path, help="Fresh run directory; for verify, the exported manifest")
    parser.add_argument("--rustflag", action="append", default=[], help="Base compiler argument, repeated; use --rustflag=-Cdebuginfo=1 to retain symbols")
    parser.add_argument("--label")
    parser.add_argument("--scenario", type=Path)
    parser.add_argument("--target", choices=sorted(TARGETS), help="Generation target (defaults to native host), or expected export target")
    parser.add_argument("--portable", action="store_true", help="Verify a trusted runner export without its original build directory; requires --target and identical source files")
    args = parser.parse_args()
    require(not args.portable or args.stage == "verify", "--portable is only for verification")
    require(args.target is None or args.stage in {"generate", "verify"}, "Later stages inherit the generation target")
    run = args.path.resolve()
    if args.stage in {"generate", "use"}:
        require(args.stage == "generate" or not args.rustflag, "Use inherits the generation flags")
        build(run, args.stage, args.rustflag, args.target)
    elif args.stage in {"train", "validate"}:
        require(args.label and args.scenario, "Supply --label and --scenario")
        workload(run, args.label, args.scenario.resolve(), args.stage == "train")
    elif args.stage == "verify":
        print(json.dumps(verify_artifact(run, args.target, args.portable)))
    else:
        {"merge": merge, "export": export}[args.stage](run)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
