# Linux packaging (Flatpak)

Smudgy ships on Linux as a self-distributed **Flatpak bundle** — a single
`smudgy-v<version>-<arch>.flatpak` file (x86_64 and aarch64) a user installs with
`flatpak install --user smudgy-v<version>-<arch>.flatpak`. This is the Linux
counterpart of the Windows Inno Setup installer (`bin/release.ps1`) and the macOS
`.dmg` (`bin/release-mac.sh`).

The app id is **`org.smudgy.Smudgy`** (macOS uses `org.smudgy`, which is only two
components and is not a valid Flatpak/D-Bus id).

## Build

```sh
# One command. Installs the runtimes if missing, builds, and bundles.
bin/release-linux.sh

# Signed (recommended for distribution): embeds the public key so the user's
# `flatpak install` trusts the origin with no manual key import.
SMUDGY_GPG_KEYID=<your-gpg-key-id> bin/release-linux.sh
```

Output: `dist/smudgy-v<version>-<arch>.flatpak`.

For a validated profile-guided executable, use `--pgo-manifest` after training
inside the matching release SDK. See [the PGO runbook](../../bin/PGO.md) for
per-architecture training, verification and packaging prerequisites.

Prerequisites: `flatpak` + **network access at build time** (see strategy below).
The build tool (`org.flatpak.Builder`), the freedesktop `25.08` runtimes, and the
`rust-stable` + `llvm20` extensions are installed automatically from Flathub on
first run. Deno 2.9.5's Rust crate family requires Rust 1.95.0 or newer; the
`rust-stable` SDK extension is used deliberately and the manifest prints
`rustc --version` before building so the toolchain is auditable in the build
log — a pre-1.95 SDK fails later during compilation, and the printed version
is how to diagnose it.

The manifest builds `smudgy_ui/web-audio-cpal`. It uses the host's
PulseAudio-compatible service (including PipeWire's PulseAudio service) for
physical output and retains hosted silent, time-advancing default and
`sinkId: "none"` contexts when compatible output is unavailable.

### Architectures

The manifest is architecture-agnostic — the `v8` crate downloads the matching
prebuilt for the sandbox arch, and cargo compiles natively. `bin/release-linux.sh`
builds for the host arch by default; pass `--arch` to target another:

```sh
bin/release-linux.sh --arch x86_64     # (default on an x86_64 host)
bin/release-linux.sh --arch aarch64    # ARM64
```

Each arch produces its own `dist/smudgy-v<version>-<arch>.flatpak`.

**Where to build aarch64:** run the command **on a native ARM64 machine or CI
runner** (e.g. a GitHub Actions `ubuntu-*-arm` runner, an ARM cloud VM, or an
Apple-Silicon Linux VM) — it's fast and reliable. Cross-building aarch64 on an
x86_64 host is possible but runs the entire compile in a QEMU-emulated sandbox
(very slow); it needs `qemu-user-static` + registered binfmt so `flatpak
--supported-arches` lists `aarch64`. The script refuses an unsupported arch with a
pointer to both options.

Install and run:

```sh
flatpak install --user dist/smudgy-v<version>-x86_64.flatpak   # unsigned bundles install as-is
flatpak run org.smudgy.Smudgy                                 # launch (works immediately)
```

**Launcher note.** The bundle ships a `.desktop` entry, and installing exports it to
`~/.local/share/flatpak/exports/share/applications/`. It appears in the GNOME/Ubuntu
app grid only once that path is in the session's `XDG_DATA_DIRS` — which
`/etc/profile.d/flatpak.sh` sets **at login**. So after a first `--user` install into
a pre-existing session, **log out and back in** to make the app show up in the
launcher. `flatpak run org.smudgy.Smudgy` works right away without that. (Flatpak
apps are never on `$PATH` by their bare name; alias it if you want a `smudgy` command.)

## Releasing (CI)

Pushing a `v<x.y.z>` tag triggers `.github/workflows/release.yml`, which
builds both arches on **native** runners (x86_64 + aarch64 — no emulation) and
publishes `smudgy-v<version>-<arch>.flatpak` to the wiki's `download:` media
namespace via `bin/dokuwiki-upload.sh`.

The upload is **SSH/scp**, not the DokuWiki JSON-RPC media API: `core.saveMedia`
carries the file base64-encoded in a JSON body and DokuWiki's PHP `memory_limit`
is exhausted on large binaries (a 38 MB bundle returns HTTP 500). scp drops the
file straight into the media dir, which DokuWiki serves as-is. Required repo
secrets: `SMUDGY_WIKI_SSH` (`user@host`), `SMUDGY_WIKI_MEDIA_DIR` (the server's
DokuWiki `data/media` path), `SMUDGY_WIKI_SSH_KEY` (deploy private key); optional
`SMUDGY_WIKI_SSH_PORT` and `SMUDGY_WIKI_URL` (enables a post-upload download
check). Because scp drops the file straight onto disk, DokuWiki's upload
extension-whitelist doesn't apply — `fetch.php` serves any existing media file —
so no `.flatpak` mime config is needed. The `download:` namespace does need to be
public-readable for anonymous downloads.

**Signing.** Like the Windows (Authenticode) and macOS (Developer ID + notarized)
builds, the Flatpak is GPG-signed: the OSTree commit is signed (so `flatpak
install` verifies the key embedded in the bundle) **and** a detached
`smudgy-v<version>-<arch>.flatpak.asc` is written and uploaded alongside, plus the
armored public key `smudgy-signing-key.asc`. CI imports the key from the
`SMUDGY_GPG_PRIVATE_KEY` secret (use a passphrase-less key for automation, or set
`SMUDGY_GPG_PASSPHRASE`); locally, `SMUDGY_GPG_KEYID=<keyid> bin/release-linux.sh`.
Publish the key fingerprint on smudgy.org so users have a trusted reference.

Users verify a download with:

```sh
gpg --import smudgy-signing-key.asc      # once
gpg --verify smudgy-v<version>-<arch>.flatpak.asc smudgy-v<version>-<arch>.flatpak
flatpak install --user smudgy-v<version>-<arch>.flatpak
```

## Files

| File | Role |
|------|------|
| `org.smudgy.Smudgy.yml` | flatpak-builder manifest |
| `smudgy-wrapper.sh` | launch wrapper — sets `--data-dir` (see *Data* below) |
| `org.smudgy.Smudgy.desktop` | desktop entry (app-menu icon) |
| `org.smudgy.Smudgy.metainfo.xml` | AppStream metadata |
| `org.smudgy.Smudgy.png` / `-512.png` | 256/512 icons (installed to `hicolor`) |
| `../../THIRD-PARTY-NOTICES.md` | feature-matched, five-target union dependency notices installed under `/app/share/doc/smudgy/` |

## Design decisions

**Base runtime — `org.freedesktop.Platform//25.08`.** The app is `winit` + `wgpu`
with no GTK, so the lean freedesktop base is correct. The `rust-stable` SDK
extension (rustc 1.96) provides the toolchain; it is build-time only and never
appears in `finish-args`.

**Data location — the host's `~/Documents/smudgy`.** Matches the Windows/macOS
builds and stays user-visible, so it also *shares data with a non-Flatpak install*
on the same machine. `dirs::document_dir()` does not resolve inside the sandbox
(the config dir is redirected to `~/.var/app/...`), so `smudgy-wrapper.sh` passes
the app's `--data-dir` flag explicitly. The manifest grants exactly this subtree —
`--filesystem=xdg-documents/smudgy:create` — **not** the whole Documents tree and
**not** `home`, which keeps the sandbox hole narrow. Inside the sandbox `$HOME` is
still the real home *path* (only `XDG_*_HOME` are redirected), so
`$HOME/Documents/smudgy` is the correct host path.

**Credentials — Secret Service.** The cloud session token and profile passwords
use the OS keyring. On Linux `smudgy_core` enables keyring's `sync-secret-service`
backend, and the manifest grants `--talk-name=org.freedesktop.secrets`. Without a
running secret service the app falls back to its obfuscated-file store.

**DevTools inspector — omitted (v1).** The `smudgy_inspector` sidecar uses
`wry`/WebKitGTK, which the freedesktop runtime does not ship. Its launch degrades
gracefully (logged, non-fatal), so it is left out. To ship it, switch the base to
`org.gnome.Platform` (bundles WebKitGTK) and build+install the sidecar too.

**Window icon.** `smudgy_ui` sets the iced window `application_id` to
`org.smudgy.Smudgy` on Linux, so the running window associates with the desktop
file (Wayland `app_id` / X11 `WM_CLASS`) and shows the app icon.

## Build strategy: network-enabled (current) vs offline/vendored

The manifest grants the **build module** `--share=network` so `cargo` fetches
crates and — importantly — the `v8` crate downloads its prebuilt `librusty_v8`
static archive from GitHub. This is the simplest correct path for a private,
non-Flathub bundle. It is **not** bit-reproducible and is not Flathub-eligible
(Flathub forbids build-time network).

To go **fully offline** later (reproducible / Flathub):

1. Vendor the cargo registry. From `flatpak/flatpak-builder-tools`:
   `python3 cargo/flatpak-cargo-generator.py Cargo.lock -o packaging/linux/cargo-sources.json`
   (regenerate whenever `Cargo.lock` changes; it ignores path/workspace deps and
   the vendored `iced_tiny_skia` `[patch.crates-io]`).
2. Pre-fetch the `librusty_v8` prebuilt as manifest `sources` and point the build
   at it. The Deno 2.9.5 engine chain is `deno_core 0.410 -> deno_v8 0.2 ->
   v8 150.4.0`; `deno_v8` enables the V8 backend with `simdutf`, so the assets are
   the `_simdutf_` variants for that pinned rusty_v8 release
   (`librusty_v8_simdutf_release_x86_64-unknown-linux-gnu.a.gz` +
   `src_binding_simdutf_release_x86_64-unknown-linux-gnu.rs` from the matching
   `denoland/rusty_v8` release). Set `RUSTY_V8_ARCHIVE` and
   `RUSTY_V8_SRC_BINDING_PATH` (absolute); never set `V8_FROM_SOURCE`.
   Re-check the suffix with `cargo tree -e features -i deno_v8` and
   `cargo tree -i v8@150.4.0` after any dependency bump. The experimental
   `quickjs`/`v8x` backend must remain inactive.
3. Drop the module's `build-args: [--share=network]` and build with
   `cargo build --profile release-full --offline`.

## Sandbox permissions (`finish-args`)

| Arg | Why |
|-----|-----|
| `--share=network` | MUD TCP, cloud/package HTTPS, `jsr:`/`npm:` script imports |
| `--share=ipc` | X11 SHM (render perf); harmless on Wayland |
| `--socket=wayland` + `--socket=fallback-x11` | display |
| `--socket=pulseaudio` | physical Web Audio output through the host PulseAudio/PipeWire service |
| `--device=dri` | GPU for `wgpu` (Vulkan/GL); not `all` |
| `--filesystem=xdg-documents/smudgy:create` | data dir (scoped, see above) |
| `--talk-name=org.freedesktop.secrets` | OS keyring (Secret Service is not a portal) |

URLs and the data folder open through the OpenURI/OpenDirectory portals, which
need no extra grant. V8/`deno_core` JIT runs under the default seccomp filter (no
`--allow=devel`).

**PulseAudio sandbox delta.** `--socket=pulseaudio` is a broad, static
application-sandbox grant: the PulseAudio protocol can expose both playback and
capture capabilities to native code in the Flatpak. Smudgy's reviewed CPAL path
opens output only, and Web Audio exposes no capture API or operation. Sandboxed
packages gain no module-loading, filesystem, network, subprocess, FFI,
system-information, or capture authority; their existing capability and
source-access checks are unchanged. The trusted Main isolate is already
allow-all, including FFI, so native code it is already authorized to load can
inherit the Flatpak's new audio-server reachability, including protocol-level
capture capability. That trusted/native residual is part of the disclosed broad
static sandbox delta, not a new Web Audio permission or API.
