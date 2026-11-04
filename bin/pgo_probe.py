"""Build the real application with profile generation and use on a native runner and
report which functions the profile-use compile found no data for.

This never trains a release profile. The only workload is a usage error (`--data-dir`
without a value): the application exits through the C runtime before opening a window,
which still runs the instrumentation's atexit writer, so one profile is flushed that
holds a record for every function the instrumented link kept. The use build therefore
reports exactly the structural gaps, if any, that `pgo.py` would reject in a release
build; on Apple targets it is the evidence that linking the training binary without
`-dead_strip` (bin/pgo-apple-link.sh) leaves none. A probe run has no training or
validation receipts: `merge`, `export`, `verify` and packaging refuse it.
"""
import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import sys

import pgo


USAGE_ERROR = 2


def flush_profile(executable, raw):
    """Run the instrumented application to its usage-error exit and return the profile."""
    raw.mkdir(parents=True)
    env = dict(os.environ, LLVM_PROFILE_FILE=str(raw/"smudgy-%p-%m.profraw"))
    result = subprocess.run([executable, "--data-dir"], cwd=pgo.ROOT, env=env,
                            capture_output=True, text=True, errors="replace")
    pgo.require(result.returncode == USAGE_ERROR,
                f"Expected the usage-error exit {USAGE_ERROR}, got {result.returncode}: {result.stderr.strip()}")
    profiles = pgo.raw_profiles(raw.parent)
    pgo.require(profiles, "No profile was flushed on exit")
    return profiles


def probe(run, target=None, rustflags=()):
    run = run.resolve()
    pgo.build(run, "generate", rustflags, target)
    config = pgo.read(run/"run.json")
    generated = pgo.check_build(run/"generate/manifest.json", config["target"])
    output = run/"probe"
    profiles = flush_profile(generated["executable"], output/"raw")
    tool = pgo.profdata_tool(config["identity"]["compiler"])
    subprocess.run([str(tool), "merge", "--failure-mode=any", "-o", str(run/"merged.profdata"), *profiles], check=True)
    summary = subprocess.check_output([str(tool), "show", str(run/"merged.profdata")], text=True, encoding="utf-8")
    (run/"profile-summary.txt").write_text(summary, encoding="utf-8")
    # No workload receipts: nothing downstream of `use` can accept this run.
    pgo.write(run/"merge.json", dict(sha256=pgo.sha(run/"merged.profdata"), tool=str(tool), raw=profiles,
                                     workloads=[], probe=True))
    error = None
    try:
        pgo.build(run, "use")
    except RuntimeError as failure:
        error = str(failure)
    built = pgo.read(run/"use/manifest.json") if (run/"use/manifest.json").is_file() else {}
    result = dict(schema=1, stage="pgo-probe", target=config["target"], revision=config["revision"],
                  compiler=config["identity"]["compiler"], deployment_target=config["identity"].get("deployment_target"),
                  host=f"{platform.system()} {platform.release()} {platform.machine()}", use_exit_code=built.get("exit_code"), use_seconds=built.get("seconds"),
                  executable=built.get("executable"), executable_sha256=built.get("executable_sha256"),
                  generate_flags=pgo.read(run/"generate/manifest.json")["flags"],
                  diagnostics=built.get("pgo_diagnostics", []), unlinked_crates=built.get("unlinked_crates", {}), error=error)
    pgo.write(output/"result.json", result)
    print(json.dumps({key: result[key] for key in ("target", "use_exit_code", "diagnostics", "unlinked_crates", "error")},
                     indent=2), flush=True)
    pgo.require(error is None, error)
    print(f"PASS: {config['target']} profile-use build reports no diagnostics beyond crates rustc did not link")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", type=Path, help="Fresh run directory")
    parser.add_argument("--target", choices=sorted(pgo.TARGETS), help="Assert the runner's native target")
    parser.add_argument("--rustflag", action="append", default=[], help="Base compiler argument, as for pgo.py generate")
    try:
        args = parser.parse_args()
        probe(args.path, args.target, args.rustflag)
    except (RuntimeError, OSError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
