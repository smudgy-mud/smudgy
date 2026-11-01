"""Exercise the host Rust PGO runtime and matching merger without building Smudgy.

This checks tool availability, profile flush/merge/use, and equal program output.
It does not measure performance or validate a Smudgy/V8 build.
"""
import argparse
import os
from pathlib import Path
import re
import subprocess
import sys

import pgo


REVIEWED_COMPILER = "rustc 1.97.1 (8bab26f4f 2026-07-14)"
# These exact startup helpers were emitted before linking but absent from the
# Mac probe profiles. Darwin can remove dead functions and their profile data:
# https://reviews.llvm.org/D25456 . This is not an application-wide exception.
MAC_STARTUP_CRATES = {
    "aarch64-apple-darwin": ("Csa9BKTri5B3M", "CsgtPOCBgevO"),
    "x86_64-apple-darwin": ("CsgejaSCmAoRz", "CsbAqs9W1eE8G"),
}


def check_probe_counts(profile):
    records = {name: [int(n) for n in counts.split(", ")] for name, counts in re.findall(
        r"^  (.+):\n    Hash: 0x[0-9a-f]+\n    Counters: \d+\n    Block counts: \[([0-9, ]+)\]$",
        profile, re.MULTILINE)}
    work = [counts for name, counts in records.items() if name.endswith("5probe4work")]
    pgo.require(records.get("main") == [2] and work == [[21001, 3001]],
                "Probe profile must contain both process entries and exact work/branch counts")


def check_use_diagnostics(stderr, target, compiler):
    if not stderr.strip():
        return []
    pgo.require(target in MAC_STARTUP_CRATES and compiler.splitlines()[:1] == [REVIEWED_COMPILER],
                "Unexpected profile-use diagnostics; inspect use.log")
    std, core = MAC_STARTUP_CRATES[target]
    helpers = {
        f"_RINvNt{std}_3std2rt10lang_startuECs5JAvRntY6Pu_5probe",
        f"_RNSNvYNCINvNt{std}_3std2rt10lang_startuE0INtNtNt{core}_4core3ops8function"
        "6FnOnceuE9call_once6vtableCs5JAvRntY6Pu_5probe",
    }
    expected = {f"warning: probe.42cd7a695e74b762-cgu.0: no profile data available for function {name} "
                "Hash = 742261418966908927 up to 0 count discarded" for name in helpers}
    lines = [line for line in stderr.splitlines() if line.strip()]
    diagnostics = lines[:-1]
    pgo.require(diagnostics and len(diagnostics) == len(set(diagnostics))
                and set(diagnostics) <= expected
                and lines[-1] == f"warning: {len(diagnostics)} warning{'s' if len(diagnostics) != 1 else ''} emitted",
                "Unexpected profile-use diagnostics; inspect use.log")
    return diagnostics


def smoke(output, target=None):
    target = pgo.native_target(target)
    compiler = pgo.capture("rustc", "-vV")
    tool = pgo.profdata_tool()
    pgo.require(tool.is_file(), "Install llvm-tools-preview for the repository toolchain")
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    source = output/"probe.rs"
    source.write_text('''use std::hint::black_box;
#[inline(never)]
fn work(n: u64) -> u64 {
    if n % 7 == 0 { n.wrapping_mul(3) } else { n ^ 37 }
}
fn main() {
    let count: u64 = std::env::args().nth(1).unwrap().parse().unwrap();
    let sum: u64 = (0..count).map(|n| work(black_box(n))).sum();
    println!("{sum}");
}
''', encoding="utf-8")
    suffix = ".exe" if sys.platform == "win32" else ""
    generated = output/f"generate{suffix}"
    optimized = output/f"use{suffix}"
    raw = output/"raw"
    raw.mkdir()
    env = dict(os.environ)
    env.pop("LLVM_PROFILE_FILE", None)
    # Exercise the same optimization pipeline as Cargo's release-full profile.
    # LTO and partitioning affect which functions survive to instrumentation.
    command = ["rustc", str(source), "--target", target, "-Copt-level=3",
               "-Clto=fat", "-Ccodegen-units=1"]
    subprocess.run([*command, f"-Cprofile-generate={raw}", "-o", str(generated)], env=env, check=True)
    results = {}
    for count in (2000, 19001):
        results[count] = subprocess.check_output([str(generated), str(count)], text=True,
            env=dict(env, LLVM_PROFILE_FILE=str(raw/"probe-%p-%m.profraw"))).strip()
    profiles = list(raw.glob("*.profraw"))
    pgo.require(len(profiles) == 2, "Expected one flushed profile per training process")
    merged = output/"merged.profdata"
    subprocess.run([str(tool), "merge", "--failure-mode=any", "-o", str(merged), *map(str, profiles)], check=True)
    summary = subprocess.check_output([str(tool), "show", str(merged)], text=True)
    profile = subprocess.check_output([str(tool), "show", "--all-functions", "--counts", str(merged)], text=True)
    (output/"profile.txt").write_text(profile, encoding="utf-8")
    check_probe_counts(profile)
    built = subprocess.run([*command, f"-Cprofile-use={merged}", "-Cllvm-args=-pgo-warn-missing-function",
                            "-o", str(optimized)], env=env, capture_output=True, text=True)
    (output/"use.log").write_text(built.stdout + built.stderr, encoding="utf-8")
    if built.returncode or built.stderr.strip():
        print(built.stdout, end="")
        print(built.stderr, end="", file=sys.stderr)
    pgo.require(built.returncode == 0, f"Inspect {output/'use.log'}")
    reviewed = check_use_diagnostics(built.stderr, target, compiler)
    if reviewed:
        print(f"Reviewed {len(reviewed)} known Mac startup-helper diagnostics for this probe only; see use.log")
    for count, expected in results.items():
        actual = subprocess.check_output([str(optimized), str(count)], env=env, text=True).strip()
        pgo.require(actual == expected, "Optimized output differs from instrumented output")
    pgo.write(output/"result.json", dict(target=target, compiler=compiler, reviewed_smoke_diagnostics=reviewed,
        tool=str(tool), command=command, profile_sha256=pgo.sha(merged), summary=summary, outputs=results,
        executable_sha256=pgo.sha(optimized), passed=True))
    print(f"PASS: {target}; flushed {len(profiles)} profiles, merged, used, and verified both outputs")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=pgo.ROOT/"target/pgo-smoke")
    parser.add_argument("--target", choices=sorted(pgo.TARGETS), help="Assert the runner's native target")
    try:
        args = parser.parse_args()
        smoke(args.output, args.target)
    except (RuntimeError, OSError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
