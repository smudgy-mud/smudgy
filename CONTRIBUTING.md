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

Smudgy uses the stable Rust toolchain. From the repository root:

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
