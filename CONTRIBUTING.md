# Contributing to Smudgy

Thank you for considering a contribution to Smudgy. Bug fixes,
documentation, tests, ideas, and larger improvements are all welcome.

Smudgy is primarily maintained by one person, so reviews may sometimes take
a little while. A quiet pull request has not been forgotten.

## AI-assisted contributions

AI-assisted contributions are welcome. If AI tools played a substantial role
in preparing a pull request, please include a brief note in the pull request
description naming the tools and how they were used. Routine autocomplete and
spelling or grammar corrections do not need to be disclosed.

AI assistance does not change how a contribution is reviewed. The things that
help most are the same for every pull request: explain the problem and the
chosen approach, keep the scope reviewable, include relevant tests or other
validation, and be available to discuss or revise the change. Contributors
are responsible for reviewing and standing behind everything they submit.

## Before starting

For bug fixes, documentation, tests, and other contained changes, feel free to
open a pull request directly.

For a new feature, substantial refactor, or change that introduces a notable
design or maintenance commitment, please open an issue first. Early discussion
can help shape the approach and avoid work in a direction that may not fit the
project. Draft pull requests are also welcome when code is the clearest way to
explore an idea.

## Building Smudgy

Smudgy pins its stable Rust toolchain in `rust-toolchain.toml`; rustup selects
and installs it automatically. From the repository root:

```sh
cargo run
```

The first build may take some time because Smudgy has a large Rust dependency
graph, including its embedded scripting runtime.

On Debian or Ubuntu, all-workspace builds also require the WebKitGTK
development package:

```sh
sudo apt-get install libwebkit2gtk-4.1-dev
```

## Checking a change

Run checks that are appropriate for the part of the project you changed.
Targeted checks are useful while iterating; replace `<package>` with a package
such as `smudgy_core` or `smudgy_ui`:

```sh
cargo fmt --all -- --check
cargo check -p <package> --locked
cargo test -p <package> --lib --tests --locked
cargo clippy -p <package> --all-targets --locked
```

The full checks used by CI are:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo test --workspace --lib --tests --locked -- \
  --skip models::shared_packages::tests::missing_required_params_tracks_only_unset_required_keys
```

The skipped test reaches the host operating system's credential service, which
is not deterministic on headless CI runners. It may be run normally in a local
desktop environment.

Please avoid introducing new compiler or Clippy warnings. Existing unrelated
warnings do not need to be fixed as part of your change. Documentation-only
changes do not need to run the full Rust test suite.

## Nightly releases

Keep the application version on `main` at `X.Y.Z-ptb`. The `Nightly` workflow
runs daily at 10:23 UTC (03:23 PDT / 02:23 PST), from the default branch. It
checks the current `main` commit before installing build tools. An unchanged
source with a completed release, active build, or recorded failed attempt does
not start another build. GitHub can delay scheduled runs; the time is a target.

For changed source, `bin/nightly-release.py` allocates `X.Y.Z-ptb.N` using the
largest existing numeric suffix for that base version. Legacy `ptbN` and
`ptb-N` tags also reserve numbers. Moving `main` to a new base starts at `.1`.
Keep tags even if a release fails or its assets are deleted: tags are the
durable number reservations, and must never be moved or reused.

The coordinator runs `bin/bump-version.sh --preview <version>` after preparing
Cargo patches. It validates all application manifests, installer metadata,
and the lockfile; unrelated dependency changes stop the run. The resulting
commit is a direct child of the source commit and is pushed only through its
annotated release tag. `main` retains its unnumbered version. Changelogs and
the backend's stable-client upgrade ceiling are unchanged.

The coordinator explicitly dispatches `Release` on that tag using
`GITHUB_TOKEN`: a tag push made with this token does not itself trigger the
push workflow. `Release` builds all four platform artifacts and publishes them
as a prerelease. After GitHub and configured wiki publication succeed, a
receipt in the release body records the original source SHA. The annotation
and receipt remain useful after Actions logs and artifacts expire. Preserve
the completion receipt when editing release notes.

Release attempts 1 and 2 use spot Linux pools; attempt 3 uses dedicated
on-demand pools. The existing retry workflow reruns only failed jobs, so jobs
that already succeeded do not need another build. Retrying never consumes
another PTB number. After the automatic retries are exhausted, inspect the
failure and manually rerun failed jobs in the same Release run. A cancellation
also requires manual recovery. Do not dispatch a fresh run just to retry a
spot failure: that resets the attempt count to 1.

If coordination stops after pushing a tag but before dispatching, the next
coordinator invocation recovers that tag when it is less than 24 hours old
and no Release run exists. Older tags with no retained run require manual
inspection and dispatch; they are not automatically rebuilt indefinitely.
For a confirmed never-dispatched tag, use
`gh workflow run release.yml --ref vX.Y.Z-ptb.N`. Do not change the tag.

Manual `Nightly` dispatch defaults to a read-only dry run:

```sh
gh workflow run nightly.yml --ref main
gh workflow run nightly.yml --ref main --field dry_run=false
```

Set repository variable `NIGHTLY_ENABLED=false` to pause coordination. Remove
it or set it to `true` to resume. GitHub disables scheduled workflows in public
repositories after 60 days without repository activity; re-enable `Nightly`
in Actions after extended dormancy. Before initial rollout, ensure tag rules
allow the workflow to create `v*` tags and the `release` environment allows
those tags. No PAT or new signing secrets are needed. The schedule becomes
active when the workflow is merged onto the default branch.

Test release tooling without compiling the application:

```sh
python -m unittest discover -s bin/tests -v
actionlint .github/workflows/nightly.yml .github/workflows/release.yml
```

## Translations

English (`en-US`) is the source catalog. The Traditional Chinese (`zh-TW`),
Polish (`pl-PL`), and Ukrainian (`uk-UA`) catalogs must contain every English
message ID and use the same Fluent variables. Run
`cargo test -p smudgy_i18n --lib --locked` to check catalog parity.

Changes to the Polish or Ukrainian catalogs require review from `@haniu03`.
The repository's `CODEOWNERS` rules request that review automatically.

## Pull requests

A helpful pull request:

- explains the problem and why the chosen approach addresses it;
- stays focused enough to review, or calls out intentional dependencies
  between changes;
- includes tests or describes other validation performed;
- notes important tradeoffs, compatibility concerns, or follow-up work; and
- includes screenshots or a short description of manual verification for
  visible UI changes, when practical.

Please mention checks you could not run or known limitations in the pull
request description. That context is more useful than presenting a change as
more complete than it is.

By submitting a contribution, you agree that it may be distributed under
Smudgy's [GPL-3.0-or-later license](LICENSE) and confirm that you have the right
to submit it.
