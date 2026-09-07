# smudgy

[![CI](https://github.com/smudgy-mud/smudgy/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/smudgy-mud/smudgy/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/smudgy-mud/smudgy?label=release)](https://github.com/smudgy-mud/smudgy/releases/latest)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)
[![Discord](https://img.shields.io/badge/chat-discord-5865F2?logo=discord&logoColor=white)](https://discord.gg/Jkgusu3qXQ)

A MUD client for Windows, macOS, and Linux, written in Rust.

**[Download](https://www.smudgy.org/download)** ·
**[Manual](https://www.smudgy.org/manual:start)** ·
**[Script reference](https://www.smudgy.org/scriptref:start)** ·
**[Discord](https://discord.gg/Jkgusu3qXQ)**

![The smudgy main window: a terminal pane on the left and the map on the right](https://www.smudgy.org/_media/screenshot.png)

## Features

- **Scripting in TypeScript or JavaScript.** Triggers, aliases, hotkeys, and
  timers run on an embedded V8 runtime built on Deno. Scripts can import
  modules from npm and jsr. The built-in editor has completion and type
  checking.
- **A full mapper.** Automatic mapping from GMCP or MSDP, an in-app map
  editor, route finding and speedwalks, and maps shared through the cloud.
- **Packages.** Install scripts other players wrote, or publish your own.
  Each package runs in its own sandbox and must ask for the permissions it
  uses.
- **A fast terminal.** GPU rendering, 256-color and truecolor, clickable
  links, and themes.
- **Panes and windows.** Split the window, stack panes into tabs, or tear a
  pane out into its own window. smudgy remembers the layout for each server.
- **Expansive and growing protocol support.** GMCP, MSDP, MCCP2, MCCP4, TLS, MTTS, NAWS, MNES, and UTF-8 with charset negotiation.
- **Script-built UI.** Scripts can add panels, buttons, tables, and Markdown
  views.

## Get started

1. Follow [First connection](https://www.smudgy.org/manual:getting-started:first-connection)
   in the manual to add a server and connect.
2. When you are ready to script, start with
   [Scripting: getting started](https://www.smudgy.org/manual:scripting:getting-started)
   and keep the [script reference](https://www.smudgy.org/scriptref:start)
   open beside it.

Questions and bug reports are welcome on [Discord](https://discord.gg/Jkgusu3qXQ)
and in the [issue tracker](https://github.com/smudgy-mud/smudgy/issues).

## Build from source

smudgy pins a stable Rust toolchain in `rust-toolchain.toml`. rustup installs
it on first use.

```sh
git clone https://github.com/smudgy-mud/smudgy.git
cd smudgy
cargo install patch-crate
cargo patch-crate
cargo run
```

`cargo patch-crate` writes the workspace's dependency patches from
[patches/](patches/) into `target/patch/`. Run it once before the first build,
and again after `cargo clean`.

The first build takes a while. The dependency graph includes V8.

On Debian or Ubuntu, install the audio and WebKitGTK development packages
first:

```sh
sudo apt-get install libasound2-dev libwebkit2gtk-4.1-dev
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the checks CI runs and how to send
a pull request. [CHANGELOG.md](CHANGELOG.md) lists what changed in each
release.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE) and
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
