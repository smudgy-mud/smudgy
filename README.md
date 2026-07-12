# smudgy

A modern MUD client for Windows, macOS, and Linux, built in Rust.

**[www.smudgy.org](https://www.smudgy.org)** — downloads, documentation, and
the scripting reference.

## Features

- Native builds for Windows, macOS, and Linux
- GPU-accelerated rendering via [iced-rs](https://iced.rs)
- Hotkeys, aliases, and triggers
- Customizable themes, mapper, and widgets
- Scriptable, resizable and repositionable panels
- Light social features (share script packages or maps with friends)
- Extremely fast trigger engine
  - Powered by Rust's regex and aho-corasick libraries, the same matching
    engines that power ripgrep
  - Smudgy's internal benchmarks profile a session with 10,000 triggers
    sustaining over 100 MB/s of throughput
- Scripts powered by Google V8 JavaScript engine
  - Scripts are JIT-compiled by V8's optimizing compilers
  - Scripts have access to the npm and jsr package ecosystems
- Full autocomplete, type-checking, and inline docs when editing scripts with
  your preferred IDE (VS Code, Neovim, etc.)
- A dedicated Smudgy package repository for uploading, sharing, reviewing,
  and updating script packages
  - Installed packages run in sandboxed isolates governed by Deno's
    permission system — they can't run programs, touch the filesystem, or
    reach the network without explicit approval
- First-class GMCP support

## Roadmap

- MSDP support
- Further trigger engine optimization, particularly around trigger
  insertion/removal
- Support for other MUD standards, e.g. MCCP, MXP, MNES
- More accessible documentation
- More interesting example packages and tutorials
- Inline script editor
- Expanded widget support

## Building from source

```sh
cargo run
```

builds and runs the client with a stable Rust toolchain. See
[CHANGELOG.md](CHANGELOG.md) for what's new.

## License

GPL-3.0-or-later — see [LICENSE](LICENSE).
