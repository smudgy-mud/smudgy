"""Opt-in Windows PGO: generate, collect, merge, use, validate, and export.

Run `python bin/pgo.py --help`. Workloads are external JSON files; no corpus,
credentials, personal data home, or machine-specific path is built into this tool.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

TARGET = "x86_64-pc-windows-msvc"
ROOT = Path(__file__).resolve().parents[1]
FEATURES = "smudgy_ui/web-audio-cpal"


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


def identity():
    """Include local patch sources and Cargo configuration, not just Git HEAD."""
    forbidden = [key for key in os.environ if os.environ[key] and (
        key in {"RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"}
        or key.startswith("CARGO_PROFILE_") or key.endswith("_RUSTFLAGS"))]
    require(not forbidden, f"Unset build overrides before PGO: {forbidden}")
    configs = {}
    config_directories = [directory/".cargo" for directory in [ROOT, *ROOT.parents]]
    config_directories.append(Path(os.environ.get("CARGO_HOME", Path.home()/".cargo")))
    for directory in config_directories:
        for name in ("config", "config.toml"):
            path = directory / name
            if path.is_file():
                content = path.read_text(encoding="utf-8")
                require("rustflags" not in content, f"Move rustflags from {path} into --rustflag; implicit flags are unsupported")
                configs[str(path)] = sha(path)
    names = capture("git", "ls-files", "-c", "-o", "--exclude-standard", "-z").split("\0")
    files = {name: sha(ROOT/name) for name in sorted(set(names))
             if name and (ROOT/name).is_file()}
    # Cargo metadata resolves the same shipping feature graph, including path
    # dependencies outside the checkout and generated target/patch sources.
    metadata = json.loads(capture("cargo", "metadata", "--locked", "--format-version", "1",
                                  "--filter-platform", TARGET, "--features", FEATURES))
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
        "CC", "CXX", "CL", "CFLAGS", "CXXFLAGS", "LDFLAGS", "LIB", "INCLUDE")}
    return dict(compiler=capture("rustc", "-vV"), cargo=capture("cargo", "-V"),
                files=files, configs=configs, local_dependencies=local_dependencies,
                native_environment=native_environment, helper_sha256=sha(__file__))


def build(run, stage, rustflags=()):
    current = identity()
    if stage == "generate":
        run.mkdir(parents=True, exist_ok=False)
        require(not any("profile-" in flag or "pgo-" in flag for flag in rustflags), "PGO flags are managed by this tool")
        config = dict(source=str(ROOT), revision=capture("git", "rev-parse", "HEAD"),
                      target=TARGET, features=FEATURES, base_flags=list(rustflags), identity=current)
        write(run/"run.json", config)
        (run/"raw-unassigned").mkdir()
        flags = [*rustflags, f"-Cprofile-generate={run/'raw-unassigned'}"]
    else:
        config = read(run/"run.json")
        require(current == config["identity"], "Source, toolchain, configuration or local dependencies changed; start a new PGO run")
        merge = read(run/"merge.json")
        require(sha(run/"merged.profdata") == merge["sha256"], "Merged profile changed")
        check_build(run/"generate/manifest.json")
        flags = [*config["base_flags"], f"-Cprofile-use={run/'merged.profdata'}", "-Cllvm-args=-pgo-warn-missing-function"]
    output = run/stage
    output.mkdir()
    env = dict(os.environ, CARGO_TARGET_DIR=str(output), CARGO_ENCODED_RUSTFLAGS="\x1f".join(flags))
    env.pop("LLVM_PROFILE_FILE", None)
    command = ["cargo", "build", "--locked", "--profile", "release-full", "--target", TARGET,
               "-p", "smudgy_ui", "--bin", "smudgy", "--features", FEATURES]
    manifest = dict(stage=stage, source=str(ROOT), run=str(run), command=command, flags=flags, started=time.time(),
                    profile_sha256=sha(run/"merged.profdata") if stage == "use" else None)
    write(output/"manifest.json", manifest)
    print(f"{stage}: {output/'build.log'}", flush=True)
    with (output/"build.log").open("w", encoding="utf-8") as log:
        result = subprocess.run(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
    manifest.update(exit_code=result.returncode, seconds=time.time()-manifest["started"])
    write(output/"manifest.json", manifest)
    require(result.returncode == 0, f"Build failed; see {output/'build.log'}")
    require(identity() == current, "Inputs changed during compilation; discard this run")
    log = (output/"build.log").read_text(encoding="utf-8", errors="replace")
    diagnostics = [line for line in log.splitlines() if re.search(r"(no profile data|profile.*(out of date|hash mismatch|missing)|function control flow change)", line, re.I)]
    exe = output/TARGET/"release-full/smudgy.exe"
    manifest.update(executable=str(exe), executable_sha256=sha(exe), pgo_diagnostics=diagnostics,
                    inputs_verified=True)
    pdb = exe.with_suffix(".pdb")
    if pdb.exists():
        manifest.update(pdb=str(pdb), pdb_sha256=sha(pdb))
    write(output/"manifest.json", manifest)
    require(not diagnostics, f"PGO coverage diagnostics require investigation: {output/'build.log'}")
    print(json.dumps({key: manifest[key] for key in ("stage", "seconds", "executable")}), flush=True)


def check_build(path):
    build = read(path)
    require(build.get("exit_code") == 0 and build.get("inputs_verified") and not build.get("pgo_diagnostics"), "Build is incomplete or has unresolved PGO diagnostics")
    require(sha(build["executable"]) == build["executable_sha256"], "Executable differs from build manifest")
    return build


def workload(run, label, scenario_path, training):
    require(re.fullmatch(r"[A-Za-z0-9_-]+", label), "Use letters, digits, underscores and hyphens in labels")
    config = read(run/"run.json")
    require(identity() == config["identity"], "Build inputs changed")
    stage = "generate" if training else "use"
    built = check_build(run/stage/"manifest.json")
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
        require(record.get("exit_code") == 0 and record.get("verification_exit_code") == 0,
                f"Incomplete or failed workload: {path}; preserve it and start a fresh run")
        require(record["executable_sha256"] == executable_hash, f"Wrong executable in {path}")
        records.append(record)
    require(records, f"No verified {directory} workloads")
    return records


def merge(run):
    require(not (run/"merge.json").exists(), "Profiles already merged; start a fresh run")
    require(identity() == read(run/"run.json")["identity"], "Build inputs changed")
    generated = check_build(run/"generate/manifest.json")
    records = accepted(run, "training", generated["executable_sha256"])
    categories = {c for r in records for c in r["scenario"]["categories"]}
    require({"replay", "captures", "interactive"} <= categories, "Training requires replay, captures and interactive coverage")
    raw = {p: digest for r in records for p, digest in r["raw"].items()}
    require(raw and all(sha(p) == digest for p, digest in raw.items()), "Raw profiles are missing or changed")
    tool = Path(capture("rustc", "--print", "sysroot"))/f"lib/rustlib/{TARGET}/bin/llvm-profdata.exe"
    require(tool.is_file(), "Install llvm-tools-preview for the repository toolchain")
    subprocess.run([str(tool), "merge", "--failure-mode=any", "-o", str(run/"merged.profdata"), *raw], check=True)
    summary = subprocess.check_output([str(tool), "show", str(run/"merged.profdata")], text=True, encoding="utf-8")
    (run/"profile-summary.txt").write_text(summary, encoding="utf-8")
    write(run/"merge.json", dict(sha256=sha(run/"merged.profdata"), tool=str(tool), raw=raw, workloads=records))


def export(run):
    config = read(run/"run.json")
    require(identity() == config["identity"], "Build inputs changed")
    built = check_build(run/"use/manifest.json")
    require(built["profile_sha256"] == sha(run/"merged.profdata"), "Profile changed after compilation")
    records = accepted(run, "validation", built["executable_sha256"])
    categories = {c for r in records for c in r["scenario"]["categories"]}
    require({"replay", "captures", "heldout", "interactive"} <= categories,
            "Validation requires replay, captures, held-out and interactive coverage")
    output = run/"dist"
    output.mkdir()
    shutil.copy2(built["executable"], output/"smudgy.exe")
    if built.get("pdb"):
        require(sha(built["pdb"]) == built["pdb_sha256"], "Symbols changed")
        shutil.copy2(built["pdb"], output/"smudgy.pdb")
    manifest = dict(schema=1, stage="validated-pgo", run=str(run), executable=str(output/"smudgy.exe"),
                    executable_sha256=sha(output/"smudgy.exe"), revision=config["revision"],
                    profile_sha256=sha(run/"merged.profdata"), validation=records)
    write(output/"manifest.json", manifest)
    print(output/"manifest.json")


def verify_artifact(path):
    manifest = read(path)
    require(manifest.get("schema") == 1 and manifest.get("stage") == "validated-pgo", "Expected a validated PGO export manifest")
    run = Path(manifest["run"])
    require(identity() == read(run/"run.json")["identity"], "Packaging source differs from validated PGO inputs")
    built = check_build(run/"use/manifest.json")
    require(manifest["executable_sha256"] == built["executable_sha256"] == sha(manifest["executable"]), "PGO artifact hash mismatch")
    require(manifest["profile_sha256"] == built["profile_sha256"] == sha(run/"merged.profdata"), "PGO profile changed")
    require(manifest["validation"] == accepted(run, "validation", built["executable_sha256"]), "Validation records changed")
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stage", choices=["generate", "train", "merge", "use", "validate", "export", "verify"])
    parser.add_argument("path", type=Path, help="Fresh run directory; for verify, the exported manifest")
    parser.add_argument("--rustflag", action="append", default=[], help="Base compiler argument, repeated; use --rustflag=-Cdebuginfo=1 to retain symbols")
    parser.add_argument("--label")
    parser.add_argument("--scenario", type=Path)
    args = parser.parse_args()
    require(sys.platform == "win32", "This PGO workflow currently supports Windows x64 only")
    run = args.path.resolve()
    if args.stage in {"generate", "use"}:
        require(args.stage == "generate" or not args.rustflag, "Use inherits the generation flags")
        build(run, args.stage, args.rustflag)
    elif args.stage in {"train", "validate"}:
        require(args.label and args.scenario, "Supply --label and --scenario")
        workload(run, args.label, args.scenario.resolve(), args.stage == "train")
    elif args.stage == "verify":
        print(json.dumps(verify_artifact(run)))
    else:
        {"merge": merge, "export": export}[args.stage](run)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
