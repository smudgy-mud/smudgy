# Opt-in Windows PGO builds

For the current cross-platform workflow, portable exports and native V8 input
contract, see [PGO.md](PGO.md). This page retains the Windows command examples.

The default release workflow is unchanged. This local release lane requires
Python 3.11+, the repository Rust toolchain, `llvm-tools-preview`, and the usual
Windows build dependencies, including `cargo-about 0.9.0`. Run commands from the
checkout. Materialize dependency patches with `cargo patch-crate --force` first. Keep run directories outside the
source tree or under its ignored `target/` directory.

Cargo must use the same PATH-selected rustc that supplies the profile merger.
Compiler/wrapper overrides (including Cargo configuration equivalents) are
rejected. Flatten configuration includes before starting a new run so all
configuration inputs are fingerprinted.

```powershell
rustup component add llvm-tools-preview
cargo about generate --workspace --features smudgy_ui/web-audio-cpal --locked about.hbs -o THIRD-PARTY-NOTICES.md
python bin/normalize-third-party-notices.py
python bin/pgo.py generate C:/pgo/run-001 --rustflag=-Cdebuginfo=1
python bin/pgo.py train C:/pgo/run-001 --label replay --scenario C:/workloads/replay.json
python bin/pgo.py train C:/pgo/run-001 --label captures --scenario C:/workloads/captures.json
python bin/pgo.py train C:/pgo/run-001 --label interactive --scenario C:/workloads/interactive.json
python bin/pgo.py merge C:/pgo/run-001
python bin/pgo.py use C:/pgo/run-001
python bin/pgo.py validate C:/pgo/run-001 --label replay --scenario C:/workloads/replay.json
python bin/pgo.py validate C:/pgo/run-001 --label captures --scenario C:/workloads/captures.json
python bin/pgo.py validate C:/pgo/run-001 --label heldout --scenario C:/workloads/heldout.json
python bin/pgo.py validate C:/pgo/run-001 --label interactive --scenario C:/workloads/interactive.json
python bin/pgo.py export C:/pgo/run-001
```

Each scenario supplies application arguments for an **isolated data home**, an
optional environment, workload categories, input files to hash, and a verification
command. For example:

```json
{
  "args": ["--data-dir", "C:/workloads/replay-home", "--keyring-user", "pgo-test"],
  "env": {},
  "categories": ["replay"],
  "inputs": ["C:/workloads/replay-home/Test/modules/replay.ts", "C:/workloads/corpus.log"],
  "verify": ["python", "C:/workloads/check-replay.py"]
}
```

Use fresh output locations for each scenario. The collector launches the shipping
desktop executable; a workload driver or a human performs the scenario and closes
the application normally. Forced termination is a failed run. The verifier runs
after exit, receives `PGO_SCENARIO_OUTPUT` and `PGO_EXECUTABLE`, and must return
nonzero for incomplete runs, incorrect callback counts or other failed assertions.
It should retain its detailed results under that output directory. Scenarios and
verifiers are trusted executable inputs, not a sandbox. Keep private data outside
the public checkout. The helper does not provide an unattended UI driver.

Training must cover replay, capture delivery and representative interactive use
(startup, settings, terminal interaction and mapper activity). Validation must also
cover a held-out workload absent from training. Categories attest to the supplied
scenario; review the verifier and evidence, rather than relying on the label alone.
Measure foreground replay in balanced blocks after compilation finishes; keep
latency, exact callback vectors, total CPU and multi-session behavior distinct.

Generation and use share `release-full`, the Windows x64 target, audio feature
graph and explicit base flags. Keep source, toolchain, local patches, configuration
and symbol settings unchanged. The generated notices are embedded in the app;
prepare them before generation and keep them unchanged through packaging. The helper checks these inputs, records every stage,
merges dense profiles with the toolchain's LLVM tool and rejects unresolved profile
diagnostics. Use a new run directory after a failure or input change; do not clean
the checkout or silently reuse old profile data. `-Cdebuginfo=1` retains a PDB for
the final artifact when supplied at generation. Native prebuilt V8 is not retrained.

To package a validated export, pass its manifest explicitly in **both** phases:

```powershell
./bin/release.ps1 -BuildOnly -PgoManifest C:/pgo/run-001/dist/manifest.json
./bin/release.ps1 -SkipBuild -PgoManifest C:/pgo/run-001/dist/manifest.json
```

Packaging checks identity and hashes, signs a staging copy, and passes that exact
executable path to Inno Setup. The PGO lane packages the fingerprinted notices
without regenerating them, matching the text embedded in the validated app. The validated unsigned executable and symbols stay
intact. The inspector sidecar uses its ordinary release build. Signing and release
publication remain separate from PGO training and validation. Other architectures
and operating systems need independent training and validation before adoption.

Tooling tests (install `bin/requirements-pgo.txt` first):
`python -m unittest discover -s bin -p 'test_pgo*.py'`.
