#!/usr/bin/env bash
# Builds and packages a self-distributed smudgy Flatpak (Linux).
#
# The Linux counterpart of bin/release.ps1 (Windows) and bin/release-mac.sh
# (macOS). Produces dist/smudgy-<version>-<arch>.flatpak: a single-file bundle a
# user installs with `flatpak install --user smudgy-<version>-<arch>.flatpak`,
# then runs from their app menu or `flatpak run org.smudgy.Smudgy`.
#
# Builds for the host architecture by default; pass --arch aarch64 to target ARM64
# (the manifest is arch-agnostic). aarch64 builds are fast on a native ARM64 box or
# CI runner; on an x86_64 host they require QEMU emulation (see --arch below).
#
# Build strategy (see packaging/linux/README.md): the flatpak-builder module is
# granted build-time network so cargo fetches crates and the `v8` crate downloads
# its prebuilt librusty_v8 archive normally — the simplest correct path for a
# private, non-Flathub bundle. It is not bit-reproducible.
#
# Prerequisites:
#   - flatpak (the build tool org.flatpak.Builder and the freedesktop 25.08
#     runtimes + rust-stable/llvm20 extensions are installed here if missing)
#   - network access at build time
#
# Signing (optional but recommended for distribution): set SMUDGY_GPG_KEYID to a
# GPG secret-key id. The bundle is then signed and its public key embedded, so a
# user's `flatpak install` trusts the origin without a manual key import. The mac
# and Windows scripts read their signing identity from the environment the same
# way.
#
# Usage: bin/release-linux.sh [--arch <x86_64|aarch64>] [--skip-build]
#   --arch <arch>  target architecture (default: this host's). Cross-building
#                  (e.g. aarch64 on an x86_64 host) needs QEMU emulation
#                  (qemu-user-static + binfmt); a native ARM64 box/CI is far faster.
#   --skip-build   re-bundle from an existing repo (skips the compile step).

set -euo pipefail

# --- configuration ----------------------------------------------------------
RUNTIME_VERSION="${SMUDGY_RUNTIME_VERSION:-25.08}"
ARCH=""   # --arch <arch>; defaults to this host's arch (resolved after arg parse)
APP_ID=org.smudgy.Smudgy
FLATHUB_URL=https://dl.flathub.org/repo/flathub.flatpakrepo

# --- argument parsing -------------------------------------------------------
SKIP_BUILD=0
PGO_MANIFEST=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --arch) ARCH="${2:?--arch requires a value}"; shift ;;
        --arch=*) ARCH="${1#*=}" ;;
        --skip-build) SKIP_BUILD=1 ;;
        --pgo-manifest) PGO_MANIFEST="${2:?--pgo-manifest requires a value}"; shift ;;
        -h|--help)
            echo "Usage: bin/release-linux.sh [--arch <x86_64|aarch64>] [--skip-build] [--pgo-manifest <manifest>]"
            echo "  Builds and bundles a self-distributed smudgy Flatpak for --arch"
            echo "  (default: this host's). Set SMUDGY_GPG_KEYID to sign the bundle."
            echo "  See the script header for details."
            exit 0 ;;
        *) echo "error: unknown argument '$1' (try --help)" >&2; exit 1 ;;
    esac
    shift
done
if [[ -n "$PGO_MANIFEST" ]]; then
    [[ "$SKIP_BUILD" -eq 0 ]] || { echo "error: --pgo-manifest cannot use --skip-build" >&2; exit 1; }
    PGO_MANIFEST="$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$PGO_MANIFEST")"
fi

cd "$(dirname "$0")/.."
repo_root="$(pwd)"

# --- prerequisite checks ----------------------------------------------------
need() { command -v "$1" >/dev/null 2>&1 || { echo "error: '$1' not found; $2" >&2; exit 1; }; }
need flatpak "install flatpak (https://flatpak.org)"
# The build tool is org.flatpak.Builder (a flatpak), not the host's flatpak-builder
# — the host's can be too old for current runtimes. Installed below.

# Target architecture: --arch override, else this host's default.
ARCH="${ARCH:-$(flatpak --default-arch 2>/dev/null || echo x86_64)}"
# The target is buildable if flatpak runs it natively, OR if a QEMU binfmt handler
# with the 'F' (fix_binary) flag is registered for it. `flatpak --supported-arches`
# reports only *native* arches and never reflects binfmt, so we must check binfmt
# ourselves. The F flag is essential: it makes the kernel hold the emulator open so
# it works inside flatpak-builder's bwrap sandbox. Emulated builds run the entire
# compile under QEMU and are VERY slow.
binfmt="/proc/sys/fs/binfmt_misc/qemu-$ARCH"
if flatpak --supported-arches 2>/dev/null | grep -qx "$ARCH"; then
    :   # native — no emulation needed
elif [[ -e "$binfmt" ]] && grep -q '^flags:.*F' "$binfmt"; then
    echo "==> Cross-building $ARCH under QEMU emulation (binfmt qemu-$ARCH, F flag set) — expect a SLOW compile."
else
    echo "error: cannot build for '$ARCH' on this host." >&2
    echo "       flatpak runs natively: $(flatpak --supported-arches 2>/dev/null | tr '\n' ' ')" >&2
    if [[ -e "$binfmt" ]]; then
        echo "       A qemu-$ARCH binfmt handler exists but lacks the 'F' flag, so it can't" >&2
        echo "       run inside flatpak's sandbox. Re-register it with F (reinstall" >&2
        echo "       qemu-user-static, or: docker run --privileged --rm tonistiigi/binfmt --install $ARCH)." >&2
    else
        echo "       Build on a native $ARCH machine (fastest), or install qemu-user-static" >&2
        echo "       (registers an F-flagged binfmt handler) to cross-build under emulation." >&2
    fi
    exit 1
fi

manifest="packaging/linux/$APP_ID.yml"
metainfo="packaging/linux/$APP_ID.metainfo.xml"
desktop="packaging/linux/$APP_ID.desktop"
repo_dir="$repo_root/target/flatpak-repo"          # multi-arch ostree repo (shared)
build_dir="$repo_root/target/flatpak-build-$ARCH"  # per-arch build + state dirs
mkdir -p "$repo_root/target" "$repo_root/dist"     # must exist on a fresh checkout (CI)
[[ -f "$manifest" ]] || { echo "error: manifest not found: $manifest" >&2; exit 1; }
if [[ -n "$PGO_MANIFEST" ]]; then
    need python3 "install Python 3.11+ and bin/requirements-pgo.txt"
    python3 bin/pgo_package.py flatpak "$PGO_MANIFEST" \
        "target/pgo-package/$ARCH/flatpak.json" --arch "$ARCH" --base "$manifest" --runtime-version "$RUNTIME_VERSION"
    manifest="target/pgo-package/$ARCH/flatpak.json"
fi

# Version from ui/Cargo.toml — the single source of truth bump-version.sh keeps
# every crate in lock-step with (and the value the client sends as
# X-Smudgy-Client-Version).
version="$(sed -n 's/^version = "\(.*\)"/\1/p' ui/Cargo.toml | head -1)"
[[ -n "$version" ]] || { echo "error: could not read version from ui/Cargo.toml" >&2; exit 1; }
echo "==> smudgy $version -> Flatpak ($ARCH, runtime $RUNTIME_VERSION)"

# --- runtimes (idempotent) --------------------------------------------------
flatpak remote-add --user --if-not-exists flathub "$FLATHUB_URL"
echo "==> Ensuring $ARCH runtimes are installed"
flatpak install --user -y --noninteractive --arch="$ARCH" flathub \
    "org.freedesktop.Platform//$RUNTIME_VERSION" \
    "org.freedesktop.Sdk//$RUNTIME_VERSION" \
    "org.freedesktop.Sdk.Extension.rust-stable//$RUNTIME_VERSION" \
    "org.freedesktop.Sdk.Extension.llvm20//$RUNTIME_VERSION"
# The build tool itself (flatpak-builder 1.4.x, matched to current runtimes).
echo "==> Ensuring org.flatpak.Builder is installed"
flatpak install --user -y --noninteractive flathub org.flatpak.Builder

# --- stamp the metainfo <release> to match this version ---------------------
# Keeps the bundle's AppStream release in sync with the crate version. A no-op
# when they already match (idempotent). bump-version.sh could own this instead.
today="$(date +%F)"
if [[ -z "$PGO_MANIFEST" ]] && grep -q '<release ' "$metainfo"; then
    sed -i -E "s|<release version=\"[^\"]*\" date=\"[^\"]*\" />|<release version=\"$version\" date=\"$today\" />|" "$metainfo"
fi

# --- validate metadata via the SDK (advisory; flatpak-builder re-validates) --
# Run the validators from the SDK we already require, so this doesn't depend on
# host-installed appstream/desktop-file-utils. Warnings are advisory: don't fail
# a release on a benign lint (flatpak-builder aborts on real errors during export).
sdk_run() { flatpak run --filesystem="$repo_root" --branch="$RUNTIME_VERSION" --command="$1" "org.freedesktop.Sdk" "${@:2}"; }
echo "==> Validating metainfo (advisory)"
sdk_run appstreamcli validate --no-net "$metainfo" || echo "    (appstreamcli reported issues — review above)"
echo "==> Validating desktop entry (advisory)"
sdk_run desktop-file-validate "$desktop" || echo "    (desktop-file-validate reported issues — review above)"

# --- signing setup ----------------------------------------------------------
# Signing happens on the HOST (flatpak build-sign, below) after an unsigned
# build — NOT inside the org.flatpak.Builder sandbox, where gpg-agent access is
# unreliable. gpg honors GNUPGHOME; in CI point it at an imported, isolated
# keyring. A passphrase-less key is simplest for automation; SMUDGY_GPG_PASSPHRASE
# (loopback) covers the detached signature if the key is protected.
gpg_homedir_opt=(); gpg_pass_opt=(); sign_bundle=()
[[ -n "${GNUPGHOME:-}" ]] && gpg_homedir_opt=(--gpg-homedir="$GNUPGHOME")
[[ -n "${SMUDGY_GPG_PASSPHRASE:-}" ]] && gpg_pass_opt=(--pinentry-mode loopback --passphrase "$SMUDGY_GPG_PASSPHRASE")
if [[ -n "${SMUDGY_GPG_KEYID:-}" ]]; then
    echo "==> Will GPG-sign with key $SMUDGY_GPG_KEYID"
    pub="$repo_root/target/flatpak-pub.gpg"
    gpg --export "$SMUDGY_GPG_KEYID" > "$pub"   # embedded in the bundle for install-time verification
    sign_bundle=(--gpg-keys="$pub")
else
    echo "==> SMUDGY_GPG_KEYID not set — building an UNSIGNED bundle (set it + import the key to sign)"
fi

# --- build + export into a local ostree repo --------------------------------
# Run flatpak-builder from org.flatpak.Builder. Absolute paths throughout so the
# sandbox cwd is irrelevant; the manifest's `dir` source (path: ../..) resolves
# relative to the manifest file, not cwd. --state-dir lives under target/ (which
# the dir source skips) so the build cache is never copied into the sandbox.
state_dir="$repo_root/target/flatpak-builder-$ARCH"
builder=(flatpak run --filesystem=host org.flatpak.Builder)

if [[ "$SKIP_BUILD" -eq 0 ]]; then
    if [[ -n "$PGO_MANIFEST" ]]; then
        echo "==> flatpak-builder (packages the validated $ARCH PGO executable)"
    else
        echo "==> flatpak-builder (compiles smudgy in the $ARCH sandbox — this takes a while)"
    fi
    # No --install-deps-from: the runtimes are pre-installed above (on the host).
    # Letting flatpak-builder install them runs a sandboxed `flatpak install` that
    # needs a session D-Bus, which headless CI lacks ("Cannot autolaunch D-Bus").
    "${builder[@]}" --user --force-clean --arch="$ARCH" \
        --state-dir="$state_dir" --repo="$repo_dir" \
        "$build_dir" "$repo_root/$manifest"
    if [[ -n "$PGO_MANIFEST" ]]; then
        python3 bin/pgo_package.py check "$PGO_MANIFEST" "$build_dir/files/bin/smudgy.bin" \
            --target "$ARCH-unknown-linux-gnu"
    fi
else
    [[ -d "$repo_dir" ]] || { echo "error: --skip-build but $repo_dir is missing; run a full build first" >&2; exit 1; }
fi

# --- sign the repo commit (host gpg; embedded-key install verification) ------
if [[ -n "${SMUDGY_GPG_KEYID:-}" ]]; then
    echo "==> Signing the $APP_ID/$ARCH commit"
    flatpak build-sign "$repo_dir" "$APP_ID" --arch="$ARCH" \
        --gpg-sign="$SMUDGY_GPG_KEYID" "${gpg_homedir_opt[@]}"
fi

# --- produce the single-file .flatpak bundle --------------------------------
mkdir -p "$repo_root/dist"
# smudgy-v<version>-<arch>.flatpak — the 'v' matches the Windows/macOS release names.
bundle="$repo_root/dist/smudgy-v$version-$ARCH.flatpak"
echo "==> Bundling $bundle"
# --runtime-repo lets the installing machine pull the freedesktop runtime the
# bundle depends on (the bundle carries only the app, not the runtime).
flatpak build-bundle "${sign_bundle[@]}" \
    --arch="$ARCH" --runtime-repo="$FLATHUB_URL" \
    "$repo_dir" "$bundle" "$APP_ID"

# --- detached signature of the bundle (verifiable against the published key) --
if [[ -n "${SMUDGY_GPG_KEYID:-}" ]]; then
    echo "==> Writing detached signature ${bundle##*/}.asc"
    gpg "${gpg_pass_opt[@]}" --batch --yes --armor \
        --local-user "$SMUDGY_GPG_KEYID" --detach-sign --output "$bundle.asc" "$bundle"
    # The armored public key users import to verify the .asc (stable across releases).
    gpg --batch --yes --armor --export "$SMUDGY_GPG_KEYID" > "$repo_root/dist/smudgy-signing-key.asc"
fi

echo "==> Done: $bundle"
[[ -n "${SMUDGY_GPG_KEYID:-}" ]] && echo "    Signed: $bundle.asc  (verify: gpg --verify \"$bundle.asc\" \"$bundle\")"
echo "    Install: flatpak install --user \"$bundle\""
echo "    Run:     flatpak run $APP_ID"
