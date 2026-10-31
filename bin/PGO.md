# Profile-guided builds

`pgo.py` runs an opt-in Rust PGO cycle on a native build host. Release packaging
can consume its validated export without rebuilding the app. Ordinary releases
remain available. PGO is not enabled automatically in release CI.

| Platform | Training/build target | Packaging input |
| --- | --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` | One export |
| Linux x64 | `x86_64-unknown-linux-gnu` | One export built in the release Flatpak SDK |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | One export built in the release Flatpak SDK |
| Mac Intel | `x86_64-apple-darwin` | Intel export plus Apple Silicon export |
| Mac Apple Silicon | `aarch64-apple-darwin` | Apple Silicon export plus Intel export |

Use a fresh run directory per target and source revision. Train each architecture
on representative hardware. Do not merge profiles across architectures, Rust
toolchains, source revisions, native V8 builds, or operating systems. The helper
requires the selected target to match the Rust compiler's host. This is not a
cross-compilation or emulation benchmark workflow.

## Prepare and freeze inputs

Use Python 3.11+ and the repository's pinned Rust toolchain. Install its matching
merger with `rustup component add llvm-tools-preview`. Install the normal native
build dependencies. Materialize dependency patches with `cargo patch-crate
--force`, and generate/normalize the shipping third-party notices **before** the
first instrumented build. Notices are compiled into the executable.

Freeze source, Cargo.lock, generated patches, Cargo configuration, compiler,
native archives, and build flags for the whole cycle. Local dependency contents
and selected native archives are fingerprinted, including files outside Git.
The helper rejects implicit Rust flags/wrappers and profile overrides; supply
explicit base flags through repeated `--rustflag` arguments to `generate`.

Keep profiles, scenario files, logs, data homes and package fixtures outside the
source tree, or under ignored `target/`. Do not commit user scripts, private
corpora, credentials, profiles or machine-specific paths. Workload environments
and command arguments are recorded: do not put secrets in them. Use isolated
test data homes instead of a personal profile directory.

On macOS, pin Xcode/SDK and `MACOSX_DEPLOYMENT_TARGET` before training. Both slices
must satisfy the deployment target promised by the final bundle. On Linux,
perform the complete cycle **inside the matching release Flatpak SDK**, with the
required Rust toolchain and llvm-tools installed there. A host-built Ubuntu
binary is not a substitute: it can link against a newer glibc. `/.flatpak-info`
is recorded, and packaging requires the SDK name, architecture and branch from
the release manifest. Its commit is retained for auditing; the current release
manifest does not pin runtime commits. Prepare release metadata before training;
PGO packaging does not restamp it.

## Train Rust

Run each command from the same checkout and environment. These examples use
POSIX shell syntax; on Windows use `python` and normal PowerShell paths. Replace
the external paths and target with the native runner's values.

```sh
python3 bin/pgo.py generate /path/to/run --target aarch64-apple-darwin
python3 bin/pgo.py train /path/to/run --label replay --scenario /path/to/replay.json
python3 bin/pgo.py train /path/to/run --label captures --scenario /path/to/captures.json
python3 bin/pgo.py train /path/to/run --label packages --scenario /path/to/packages.json
python3 bin/pgo.py train /path/to/run --label interactive --scenario /path/to/interactive.json
python3 bin/pgo.py merge /path/to/run
python3 bin/pgo.py use /path/to/run
```

The helper builds `smudgy_ui` with `web-audio-cpal`, the `release-full` profile and
an explicit target. Build-time processes cannot contribute to the accepted
training set. Only profiles from successful training scenarios are merged.
`generate` and `use` use separate Cargo output directories. Keep both and their
logs until the result is accepted. PGO coverage diagnostics fail the build gate;
investigate them rather than silently carrying stale profiles forward.

A scenario is an external JSON file:

```json
{
  "args": ["--data-dir", "/path/to/isolated-data"],
  "env": {},
  "categories": ["packages", "captures"],
  "inputs": ["/path/to/fixture-input", "/path/to/verification-script.py"],
  "verify": ["python3", "/path/to/verification-script.py"]
}
```

List immutable inputs using absolute paths. The verifier runs from the scenario
file's directory after a normal application exit, with `PGO_SCENARIO_OUTPUT` and
`PGO_EXECUTABLE` in its environment. It must fail on incomplete work, wrong trigger
counts, wrong results or ordering. Training profiles are flushed on normal exit;
force-killing the application is not an accepted training run. The helper owns
`LLVM_PROFILE_FILE` and uses process/module-specific profile filenames.

Training should cover more than a counter callback benchmark:

- Replay and capture-heavy triggers, nested dispatch, trigger enable/disable and
  replacement during callbacks, and multiple concurrent sessions.
- Representative larger packages: object/string work, asynchronous operations,
  timers, local mapper operations, UI updates and enough duration to exercise GC.
- Startup, loading/reloading packages, scrolling and ordinary interactive use.

Weight scenarios by intended use, not merely package size. Repeating a tiny
benchmark many times can dominate the profile. Record package versions, scenario
counts and durations. The minimum automated category gates are `replay`,
`captures`, and `interactive`; category labels alone do not establish meaningful
coverage. A release candidate should include the package workload above too.

## Validate and export

Run representative scenarios again against `use`, plus distinct held-out inputs
and packages that did not train the profile:

```sh
python3 bin/pgo.py validate /path/to/run --label replay --scenario /path/to/replay.json
python3 bin/pgo.py validate /path/to/run --label captures --scenario /path/to/captures.json
python3 bin/pgo.py validate /path/to/run --label packages --scenario /path/to/packages.json
python3 bin/pgo.py validate /path/to/run --label interactive --scenario /path/to/interactive.json
python3 bin/pgo.py validate /path/to/run --label heldout --scenario /path/to/heldout.json
python3 bin/pgo.py export /path/to/run
```

Export requires successful verification and all four categories: `replay`,
`captures`, `interactive`, `heldout`. It preserves the executable and its mode,
and copies a PDB if present. Keep the original build directories for other debug
artifacts, including macOS object files/dSYMs; export does not generate dSYMs.
Measure the optimized executable against an ordinary build of the same source,
features and native archive. Do not benchmark during compilation. Compare exact
results, latency distributions, CPU and memory on both training and held-out
workloads. No fixed performance gain is assumed across platforms.

Schema 2 exports contain target, source hashes, build identity, profile digest,
and validation receipts. Local `verify` also rechecks the original build/run.
`verify --portable --target TARGET` permits moving the complete `dist` directory
between **trusted** build/package runners. It checks binary hashes, consistent
target/build/validation receipts and byte-identical checkout files without
requiring the original absolute build paths. This is an integrity check, not an
artifact signature or proof that an untrusted runner executed the stated tests.
Preserve executable permissions when transferring Unix artifacts.

Existing schema 1 Windows exports remain usable with their original helper.
They cannot be relabeled schema 2: create a fresh run for the new portable format.

## Native V8 PGO, then fresh Rust PGO

Rust instrumentation does not train a precompiled V8 archive. Native V8 PGO has
its own Clang instrument/merge/use cycle. For each target:

1. Pin a complete `rusty_v8` source checkout matching Cargo.lock, its patches,
   features, Clang version, profile runtime and GN configuration. Preserve a
   source manifest and an ordinary archive for matched controls.
2. Build the native archive with GN `chrome_pgo_phase=1`. Keep the same LTO choice
   in generation and use. Build the Smudgy training host against that archive
   with **ordinary Rust**, and link the matching Clang profile runtime. On Unix
   this may require an explicit compiler-driver/linker setting: a static archive
   does not carry the executable's link command. Do not assume rustc's profile
   runtime is compatible with V8's Clang version.
3. Run the representative workloads above, exit normally, and collect only
   application profiles in a dedicated directory. Keep native build-tool
   profiles out of it. Merge with **that Clang toolchain's** `llvm-profdata`.
4. Rebuild the same native source/configuration with `chrome_pgo_phase=2` and
   `pgo_data_path` selecting that merged profile. Inspect missing/stale-profile
   diagnostics and test the native archive's lifecycle, isolate and threading
   behavior. Preserve the resulting archive, generated Rust bindings and GN args.
5. Select those immutable native outputs for a **new Rust PGO cycle**. Do not
   reuse a Rust profile collected against a different native archive.

The helper accepts the resulting per-target native inputs via
`RUSTY_V8_ARCHIVE`, `RUSTY_V8_SRC_BINDING_PATH` and
`SMUDGY_NATIVE_PGO_MANIFEST`. The latter points to the native build driver's JSON
receipt, with this contract (paths are absolute on the Rust training runner):

```json
{
  "schema": 1,
  "stage": "native-pgo-use",
  "target": "aarch64-apple-darwin",
  "compiler": "full pinned Clang version and build identity",
  "source_sha256": "digest of the pinned native source manifest",
  "archive": {"sha256": "archive SHA-256"},
  "bindings": {"sha256": "bindings SHA-256"},
  "profile": {"path": "/path/to/native.profdata", "sha256": "profile SHA-256"}
}
```

The helper rejects target mismatches or changed archive, bindings, manifest or
profile. It fingerprints the receipt throughout Rust generation/use/validation.
The receipt is supplied by the native build driver; it does not independently
prove that Clang used the profile. Native V8 compilation/training is **not
automated by `pgo.py`**. Qualifying the native compiler/runtime/linker combination
on each Linux/Mac architecture is still required before shipping native PGO.

## Package validated exports

For macOS, transfer both native exports to a packaging Mac with the identical
checkout and normal signing prerequisites:

```sh
bin/release-mac.sh --pgo-arm64 /path/to/arm/dist/manifest.json \
  --pgo-x86_64 /path/to/intel/dist/manifest.json
```

Both exports are required. The inspector is built normally. Cargo-bundle's
verified `CARGO_BUNDLE_SKIP_BUILD` support prevents rebuilding the app. Both thin
binaries are checked again before `lipo` combines them. Signing changes the
staged bundle; the validated exports remain intact. Smoke-test the resulting
universal app on **both** architectures, including package loading and JIT use.

For Linux, install `bin/requirements-pgo.txt` in the packaging Python environment:

```sh
python3 -m pip install -r bin/requirements-pgo.txt
bin/release-linux.sh --arch x86_64 --pgo-manifest /path/to/x64/dist/manifest.json
bin/release-linux.sh --arch aarch64 --pgo-manifest /path/to/arm/dist/manifest.json
```

The generated Flatpak overlay retains the release assets and sandbox grants,
installs the validated binary, and disables debug extraction/stripping. The
installed binary is checked again after Flatpak Builder finishes. The original
manifest remains unchanged. Neither Unix PGO path accepts `--skip-build`.

Windows retains `bin/release.ps1 -PgoManifest <manifest>` and now explicitly
requires a Windows x64 export. Release commands retain their usual signing and
packaging behavior; `pgo.py` itself never signs or publishes.

## Tooling checks and qualification boundary

```sh
python3 -m pip install -r bin/requirements-pgo.txt
python3 -m unittest discover -s bin -p 'test_pgo*.py' -v
python3 bin/pgo_smoke.py --output /path/to/fresh-smoke-run
```

The CI tooling matrix covers all five targets, with separate native Intel/ARM
Mac and Linux jobs. The smoke test compiles
a tiny Rust program, collects profiles, merges them, builds with profile use,
and compares outputs. It checks the Rust PGO toolchain, not Smudgy performance,
native V8, Flatpak runtime compatibility, both Mac slices or signing. Native
application training, held-out comparisons and final-package smoke tests remain
required qualification steps. A tooling test pass alone must not enable PGO
release publication.

References: [Rust PGO](https://doc.rust-lang.org/rustc/profile-guided-optimization.html),
[Clang PGO](https://clang.llvm.org/docs/UsersManual.html#profile-guided-optimization),
[GN native PGO configuration](https://chromium.googlesource.com/chromium/src/build/config/+/refs/heads/main/compiler/pgo/BUILD.gn),
[Flatpak build options](https://docs.flatpak.org/en/latest/flatpak-builder-command-reference.html#build-options),
and [cargo-bundle 0.7.0](https://github.com/burtonageo/cargo-bundle/blob/v0.7.0/src/main.rs).
Runner labels follow [GitHub's hosted runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
For native builds, the configuration vendored with the pinned V8 source is the
authority when it differs from upstream documentation.
