"""Exercise the host Rust PGO runtime and matching merger without building Smudgy.

This checks tool availability, profile flush/merge/use, and equal program output.
It does not measure performance or validate a Smudgy/V8 build.
"""
import argparse
import os
from pathlib import Path
import subprocess
import sys

import pgo


def smoke(output, target=None):
    target = pgo.native_target(target)
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
    command = ["rustc", str(source), "--target", target, "-Copt-level=3"]
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
    built = subprocess.run([*command, f"-Cprofile-use={merged}", "-Cllvm-args=-pgo-warn-missing-function",
                            "-o", str(optimized)], env=env, capture_output=True, text=True)
    (output/"use.log").write_text(built.stdout + built.stderr, encoding="utf-8")
    pgo.require(built.returncode == 0 and not built.stderr.strip(), f"Inspect {output/'use.log'}")
    for count, expected in results.items():
        actual = subprocess.check_output([str(optimized), str(count)], env=env, text=True).strip()
        pgo.require(actual == expected, "Optimized output differs from instrumented output")
    pgo.write(output/"result.json", dict(target=target, compiler=pgo.capture("rustc", "-vV"),
        tool=str(tool), profile_sha256=pgo.sha(merged), summary=summary, outputs=results,
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
