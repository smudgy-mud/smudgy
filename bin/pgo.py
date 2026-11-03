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
# The build stamps one of these into the binary (core/build.rs), so they are inputs.
EMBEDDED_COMMIT_VARIABLES = ("SMUDGY_GIT_COMMIT", "GITHUB_SHA", "CI_COMMIT_SHA")
# LLVM profile diagnostics as rustc reports them: unreadable or unapplied profiles
# ("invalid/unsupported/malformed instrumentation profile"), stale or mismatched
# functions, and functions without data. Only diagnostic lines match, even when a
# colour escape precedes them, so the `-vv` command echo never does.
PGO_DIAGNOSTIC = re.compile(
    r"^\s*(?:\x1b\[[0-9;]*m\s*)*(?:warning|error)\b.*(?:instrumentation profile|no profile data available"
    r"|control flow change detected|hash mismatch|inconsistent number of counts|profile data may be out of date)", re.I)
# Base flags that would hide or reconfigure those diagnostics, or override PGO itself.
MANAGED_FLAG_PREFIXES = ("-A", "--cap-lints", "-Cllvm-args", "-Cprofile", "-Zprofile")
RESERVED_SCENARIO_VARIABLES = {"LLVM_PROFILE_FILE", "PGO_SCENARIO_OUTPUT", "PGO_EXECUTABLE"}
LLVM_RELEASE = re.compile(r"LLVM version:? (\d+\.\d+\.\d+)")


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


def head_revision():
    return capture("git", "rev-parse", "HEAD")


def worktree_status():
    return capture("git", "status", "--porcelain")


def toolchain_rustc():
    """The PATH-selected toolchain's own compiler. Cargo is pinned to it so the
    compilation and the profile merge cannot resolve to different LLVMs."""
    sysroot = Path(capture("rustc", "--print", "sysroot"))
    rustc = sysroot/"bin"/("rustc.exe" if sys.platform == "win32" else "rustc")
    require(rustc.is_file(), f"Toolchain compiler missing: {rustc}")
    return sysroot, rustc


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
        "MACOSX_DEPLOYMENT_TARGET", "SDKROOT", *EMBEDDED_COMMIT_VARIABLES)}
    return dict(compiler=capture("rustc", "-vV"), cargo=capture("cargo", "-V"),
                target=target, files=files, configs=configs, local_dependencies=local_dependencies,
                native_inputs=native_inputs(target), flatpak=flatpak_environment(),
                native_environment=native_environment, helper_sha256=sha(__file__))


def build(run, stage, rustflags=(), target=None):
    target = native_target(target if stage == "generate" else read(run/"run.json")["target"])
    current = identity(target)
    if stage == "generate":
        run.mkdir(parents=True, exist_ok=False)
        require(not any(flag.startswith(MANAGED_FLAG_PREFIXES) or "pgo-" in flag for flag in rustflags),
                "PGO flags, lint levels, lint caps and LLVM arguments are managed by this tool")
        config = dict(source=str(ROOT), revision=head_revision(), worktree_status=worktree_status(),
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
    _, rustc = toolchain_rustc()
    # Plain output keeps diagnostics matchable regardless of the operator's colour setting.
    env = dict(os.environ, CARGO_TARGET_DIR=str(output), CARGO_ENCODED_RUSTFLAGS="\x1f".join(flags), RUSTC=str(rustc),
               CARGO_TERM_COLOR="never")
    env.pop("LLVM_PROFILE_FILE", None)
    # `-vv` lifts Cargo's `--cap-lints allow` on dependency crates, which would otherwise
    # silence their profile diagnostics along with every other warning.
    command = ["cargo", "build", "-vv", "--locked", "--profile", "release-full", "--target", target,
               "-p", "smudgy_ui", "--bin", "smudgy", "--features", FEATURES]
    manifest = dict(stage=stage, target=target, source=str(ROOT), run=str(run), command=command, flags=flags,
                    rustc=str(rustc), started=time.time(), profile_sha256=sha(run/"merged.profdata") if stage == "use" else None)
    write(output/"manifest.json", manifest)
    print(f"{stage}: {output/'build.log'}", flush=True)
    with (output/"build.log").open("w", encoding="utf-8") as log:
        result = subprocess.run(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
    manifest.update(exit_code=result.returncode, seconds=time.time()-manifest["started"])
    write(output/"manifest.json", manifest)
    require(result.returncode == 0, f"Build failed; see {output/'build.log'}")
    require(identity(target) == current, "Inputs changed during compilation; discard this run")
    log = (output/"build.log").read_text(encoding="utf-8", errors="replace")
    diagnostics = [line for line in log.splitlines() if PGO_DIAGNOSTIC.search(line)]
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
    strings = lambda values: all(isinstance(value, str) for value in values)
    require(isinstance(extra_env, dict) and strings(extra_env) and strings(extra_env.values())
            and strings(scenario.get("args", [])) and strings(scenario["verify"]) and strings(scenario.get("inputs", [])),
            "Scenario args, verify, inputs and env entries must be strings")
    require(not RESERVED_SCENARIO_VARIABLES & extra_env.keys(), "The collector owns LLVM_PROFILE_FILE, PGO_SCENARIO_OUTPUT and PGO_EXECUTABLE")
    output = run/("training" if training else "validation")/label
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, **extra_env, PGO_SCENARIO_OUTPUT=str(output), PGO_EXECUTABLE=built["executable"])
    if training:
        (output/"raw").mkdir()
        env["LLVM_PROFILE_FILE"] = str(output/"raw/smudgy-%p-%m.profraw")
    else:
        env.pop("LLVM_PROFILE_FILE", None)
    # Relative inputs are relative to the scenario file, like the verifier's working directory.
    inputs = {str(path): sha(path) for path in ((scenario_path.parent/p).resolve() for p in scenario.get("inputs", []))}
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
    # Seal the collected profile before anything else runs.
    manifest["raw"] = raw_profiles(output) if training else {}
    write(output/"manifest.json", manifest)
    require(not training or manifest["raw"], "No profile was flushed on normal exit")
    # The verifier checks exact expected counts/completion and held-out evidence.
    # It executes without a shell, only after the application has exited, and without
    # the collector's LLVM_PROFILE_FILE, so nothing it launches can add to the profile.
    verifier_env = {key: value for key, value in env.items() if key != "LLVM_PROFILE_FILE"}
    with (output/"verify.log").open("w", encoding="utf-8") as log:
        result = subprocess.run(scenario["verify"], cwd=scenario_path.parent, env=verifier_env, stdout=log, stderr=subprocess.STDOUT)
    manifest["verification_exit_code"] = result.returncode
    write(output/"manifest.json", manifest)
    require(result.returncode == 0, f"Workload verification failed: {output/'verify.log'}")
    require(not training or raw_profiles(output) == manifest["raw"], "Verifier altered the collected profile; run cannot be accepted")


def raw_profiles(output):
    return {str(p): sha(p) for p in sorted((output/"raw").glob("*.profraw"))}


def accepted(run, directory, executable_hash):
    records = []
    for path in sorted((run/directory).glob("*/manifest.json")):
        record = read(path)
        check_workload_receipt(record, executable_hash, training=directory == "training")
        records.append(record)
    require(records, f"No verified {directory} workloads")
    return records


def check_workload_receipt(record, executable_hash, training=False):
    require(record.get("exit_code") == 0 and record.get("verification_exit_code") == 0,
            "Incomplete or failed workload receipt; preserve it and start a fresh run")
    require(record["executable_sha256"] == executable_hash, "Wrong executable in workload receipt")
    require(not training or record.get("raw"), "No profile was collected by a training receipt; it cannot count toward coverage")


def check_heldout(records, training):
    """Held-out evidence must be absent from training: a scenario training never ran
    that hashes at least one input file training never hashed."""
    require(training and all(record["inputs"] for record in training),
            "Training scenarios must declare their input files so held-out evidence can be compared")
    trained_scenarios = {record["scenario_sha256"] for record in training}
    trained_inputs = {digest for record in training for digest in record["inputs"].values()}
    for record in records:
        if "heldout" not in record["scenario"]["categories"]:
            continue
        require(record["scenario_sha256"] not in trained_scenarios, "Held-out scenario was used in training")
        require(record["inputs"] and not set(record["inputs"].values()) <= trained_inputs,
                "Held-out scenario must declare an input file absent from training")


def training_evidence(records):
    return [dict(scenario_sha256=record["scenario_sha256"], inputs=record["inputs"]) for record in records]


def check_symbols(manifest, built, executable):
    if manifest.get("pdb_sha256"):
        require(manifest["pdb_sha256"] == built.get("pdb_sha256") == sha(executable.with_name("smudgy.pdb")),
                "PGO symbols hash mismatch")


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
    tool = profdata_tool(config["identity"]["compiler"])
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
    training = training_evidence(read(run/"merge.json")["workloads"])
    check_heldout(records, training)
    output = run/"dist"
    output.mkdir()
    name = executable_name(config["target"])
    shutil.copy2(built["executable"], output/name)
    require(sha(output/name) == built["executable_sha256"], "Exported executable differs from the validated build")
    manifest = dict(schema=2, stage="validated-pgo", target=config["target"], run=str(run), executable=str(output/name),
                    executable_name=name, executable_sha256=built["executable_sha256"], revision=config["revision"],
                    worktree_status=config.get("worktree_status"), build=built, build_identity=config["identity"],
                    profile_sha256=sha(run/"merged.profdata"), validation=records, training=training)
    if built.get("pdb"):
        require(sha(built["pdb"]) == built["pdb_sha256"], "Symbols changed")
        shutil.copy2(built["pdb"], output/"smudgy.pdb")
        require(sha(output/"smudgy.pdb") == built["pdb_sha256"], "Exported symbols differ from the validated build")
        manifest["pdb_sha256"] = built["pdb_sha256"]
    write(output/"manifest.json", manifest)
    print(output/"manifest.json")


def profdata_tool(compiler):
    """The toolchain's llvm-profdata for the compiler host, required to share rustc's LLVM."""
    # Tools run on the compiler host, not the cross-compilation target.
    sysroot, _ = toolchain_rustc()
    name = "llvm-profdata.exe" if sys.platform == "win32" else "llvm-profdata"
    tool = sysroot/"lib/rustlib"/host_target()/"bin"/name
    require(tool.is_file(), "Install llvm-tools-preview for the repository toolchain")
    # rustup's tools print the release with a `-rust-<version>-<channel>` suffix that
    # `rustc -vV` omits, so compare the numeric release only.
    rustc_llvm = LLVM_RELEASE.search(compiler)
    tool_llvm = LLVM_RELEASE.search(capture(str(tool), "--version"))
    require(rustc_llvm and tool_llvm and rustc_llvm.group(1) == tool_llvm.group(1),
            "llvm-profdata must come from the same LLVM release as rustc; install llvm-tools-preview for this toolchain")
    return tool


def verify_artifact(path, target=None, portable=False):
    manifest = read(path)
    require(manifest.get("schema") == 2 and manifest.get("stage") == "validated-pgo", "Expected a validated PGO export manifest")
    recorded_target = manifest["target"]
    require(recorded_target == manifest["build_identity"]["target"], "Inconsistent PGO target provenance")
    require(target is None or target == recorded_target, "PGO artifact target mismatch")
    require(manifest["executable_name"] == executable_name(recorded_target), "Wrong exported executable name")
    executable = Path(path).resolve().parent/manifest["executable_name"]
    require(sha(executable) == manifest["executable_sha256"], "PGO artifact hash mismatch")
    require("training" in manifest, "Export predates held-out training evidence; export again with this helper")
    # Packaging must happen at the validated commit, whether or not the run directory is present.
    require(head_revision() == manifest["revision"], "Packaging checkout revision differs from the validated PGO run")
    if portable:
        require(target is not None, "Portable verification requires an explicit target")
        require(source_files() == manifest["build_identity"]["files"], "Packaging source differs from validated PGO inputs")
        built = manifest["build"]
        check_build_receipt(built, recorded_target)
        require(built["executable_sha256"] == manifest["executable_sha256"] and built["profile_sha256"] == manifest["profile_sha256"], "Inconsistent PGO build provenance")
        check_symbols(manifest, built, executable)
        records = manifest["validation"]
        require(records, "No verified validation workloads")
        for record in records:
            check_workload_receipt(record, built["executable_sha256"])
        check_coverage(records, validation=True)
        check_heldout(records, manifest["training"])
        manifest["executable"] = str(executable)
        return manifest
    run = Path(manifest["run"])
    config = read(run/"run.json")
    require(recorded_target == config["target"], "PGO export target differs from original run")
    require(manifest["revision"] == config["revision"], "PGO export revision differs from original run")
    require(identity(recorded_target) == config["identity"] == manifest["build_identity"], "Packaging source differs from validated PGO inputs")
    built = check_build(run/"use/manifest.json", recorded_target)
    require(manifest["build"] == built, "Build receipt changed")
    require(manifest["executable_sha256"] == built["executable_sha256"], "PGO artifact hash mismatch")
    check_symbols(manifest, built, executable)
    require(manifest["profile_sha256"] == built["profile_sha256"] == sha(run/"merged.profdata"), "PGO profile changed")
    records = accepted(run, "validation", built["executable_sha256"])
    require(manifest["validation"] == records, "Validation records changed")
    require(manifest["training"] == training_evidence(read(run/"merge.json")["workloads"]), "Training records changed")
    check_heldout(records, manifest["training"])
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
