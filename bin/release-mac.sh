#!/usr/bin/env bash
# Builds, signs, notarizes, and packages a universal macOS smudgy release.
#
# The macOS counterpart of bin/release.ps1. Produces a notarized, stapled
# Smudgy.dmg holding a universal (arm64 + x86_64) Smudgy.app, with the bundled
# smudgy_inspector DevTools sidecar placed next to the app binary in
# Contents/MacOS/ -- where the app's spawn_inspector() looks for it (it resolves
# the helper next to its own current_exe(); see ui/src/windows/smudgy_window.rs).
#
# Prerequisites (one-time):
#   - Xcode command-line tools          (codesign, xcrun, ditto, lipo)  -- xcode-select --install
#   - Rust + both darwin targets        rustup target add aarch64-apple-darwin x86_64-apple-darwin
#   - cargo install cargo-bundle --version 0.7.0
#   - cargo install --locked cargo-about --version 0.9.0 --features cli
#   - brew install create-dmg
#   - A "Developer ID Application" identity in the login keychain.
#   - Notarization credentials, any of (keychain preferred locally):
#       * keychain profile (default name "smudgy"):
#           xcrun notarytool store-credentials smudgy \
#             --apple-id you@example.com --team-id TEAMID --password <app-specific-password>
#       * App Store Connect API key (CI-preferred):
#           SMUDGY_NOTARY_KEY_FILE  SMUDGY_NOTARY_KEY_ID  SMUDGY_NOTARY_ISSUER
#       * Apple ID + app-specific password (fallback):
#           SMUDGY_NOTARY_APPLE_ID  SMUDGY_NOTARY_TEAM_ID  SMUDGY_NOTARY_PASSWORD
#
# Signing is the macOS counterpart of release.ps1's Azure signing: each
# signature carries Apple's secure --timestamp (like the RFC 3161 timestamp used
# on Windows), and the .app and .dmg are notarized and stapled so Gatekeeper
# accepts them offline.
#
# Overridable config (environment): SMUDGY_SIGN_IDENTITY, SMUDGY_NOTARY_PROFILE.
#
# Usage: bin/release-mac.sh [--skip-build] [--pgo-arm64 <manifest> --pgo-x86_64 <manifest>]
#   --skip-build   re-sign / re-package an already-built Smudgy.app (mirrors
#                  release.ps1's -SkipBuild).

set -euo pipefail

# --- configuration (override via environment) -------------------------------
SIGN_IDENTITY="${SMUDGY_SIGN_IDENTITY:-Developer ID Application: Walter Kalata (2BCH3URPFS)}"
NOTARY_PROFILE="${SMUDGY_NOTARY_PROFILE:-smudgy}"
PROFILE=release-full
TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
# The triple whose cargo-bundle output we reuse as the .app skeleton (Info.plist
# + Smudgy.icns). Its thin Mach-O binaries are replaced by the universal (lipo'd)
# ones below, so picking one here loses no architecture.
BUNDLE_TARGET=aarch64-apple-darwin

# --- argument parsing -------------------------------------------------------
SKIP_BUILD=0
PGO_ARM64=""
PGO_X86_64=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build) SKIP_BUILD=1 ;;
        --pgo-arm64) PGO_ARM64="${2:?--pgo-arm64 requires a manifest}"; shift ;;
        --pgo-x86_64) PGO_X86_64="${2:?--pgo-x86_64 requires a manifest}"; shift ;;
        -h|--help)
            echo "Usage: bin/release-mac.sh [--skip-build] [--pgo-arm64 <manifest> --pgo-x86_64 <manifest>]"
            echo "  Builds, signs, notarizes, and packages a universal Smudgy.dmg."
            echo "  --skip-build   re-sign / re-package an already-built Smudgy.app."
            echo "  See the header of this script for prerequisites and env vars."
            exit 0 ;;
        *) echo "error: unknown argument '$1' (try --help)" >&2; exit 1 ;;
    esac
    shift
done
if [[ -n "$PGO_ARM64" || -n "$PGO_X86_64" ]]; then
    [[ -n "$PGO_ARM64" && -n "$PGO_X86_64" && "$SKIP_BUILD" -eq 0 ]] || {
        echo "error: PGO requires both architecture manifests and cannot use --skip-build" >&2
        exit 1
    }
    # Resolve before changing cwd. The two profiles were trained on separate native hosts.
    PGO_ARM64="$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$PGO_ARM64")"
    PGO_X86_64="$(python3 -c 'import pathlib,sys; print(pathlib.Path(sys.argv[1]).resolve())' "$PGO_X86_64")"
fi

cd "$(dirname "$0")/.."
repo_root="$(pwd)"
if [[ -n "$PGO_ARM64" ]]; then
    python3 bin/pgo.py verify "$PGO_ARM64" --portable --target aarch64-apple-darwin >/dev/null
    python3 bin/pgo.py verify "$PGO_X86_64" --portable --target x86_64-apple-darwin >/dev/null
fi

# --- prerequisite checks ----------------------------------------------------
need() { command -v "$1" >/dev/null 2>&1 || { echo "error: '$1' not found; $2" >&2; exit 1; }; }
need cargo      "install Rust (https://rustup.rs)"
need codesign   "install the Xcode command-line tools (xcode-select --install)"
need xcrun      "install the Xcode command-line tools (xcode-select --install)"
need ditto      "install the Xcode command-line tools (xcode-select --install)"
need lipo       "install the Xcode command-line tools (xcode-select --install)"
need create-dmg "brew install create-dmg"
[[ "$(cargo bundle --version 2>/dev/null)" == "cargo-bundle v0.7.0" ]] || {
    echo "error: cargo-bundle 0.7.0 required; run 'cargo install cargo-bundle --version 0.7.0'" >&2
    exit 1
}
[[ "$(cargo about --version 2>/dev/null)" == "cargo-about 0.9.0" ]] || {
    echo "error: cargo-about 0.9.0 required; run 'cargo install --locked cargo-about --version 0.9.0 --features cli'" >&2
    exit 1
}

# The signing identity must be in a keychain codesign can see.
if ! security find-identity -v -p codesigning | grep -qF "$SIGN_IDENTITY"; then
    echo "error: signing identity not found in keychain:" >&2
    echo "         $SIGN_IDENTITY" >&2
    echo "       (override with SMUDGY_SIGN_IDENTITY). Available identities:" >&2
    security find-identity -v -p codesigning >&2 || true
    exit 1
fi

# Resolve notarization credentials, in preference order:
#   1. keychain profile (local dev; created with `notarytool store-credentials`)
#   2. App Store Connect API key (CI-preferred: scoped, revocable, no Apple ID
#      password): SMUDGY_NOTARY_KEY_FILE (path to the .p8) + SMUDGY_NOTARY_KEY_ID
#      + SMUDGY_NOTARY_ISSUER
#   3. Apple ID + app-specific password (legacy fallback):
#      SMUDGY_NOTARY_APPLE_ID + SMUDGY_NOTARY_TEAM_ID + SMUDGY_NOTARY_PASSWORD
# notarytool store-credentials saves a login-keychain item under service
# "com.apple.gke.notary.tool", account "com.apple.gke.notary.tool.saved-creds.<profile>".
notary_creds=()
if security find-generic-password \
        -s "com.apple.gke.notary.tool" \
        -a "com.apple.gke.notary.tool.saved-creds.$NOTARY_PROFILE" >/dev/null 2>&1; then
    notary_creds=(--keychain-profile "$NOTARY_PROFILE")
    echo "==> Notarizing via keychain profile '$NOTARY_PROFILE'"
elif [[ -n "${SMUDGY_NOTARY_KEY_FILE:-}" && -n "${SMUDGY_NOTARY_KEY_ID:-}" && -n "${SMUDGY_NOTARY_ISSUER:-}" ]]; then
    [[ -f "$SMUDGY_NOTARY_KEY_FILE" ]] || { echo "error: SMUDGY_NOTARY_KEY_FILE not found: $SMUDGY_NOTARY_KEY_FILE" >&2; exit 1; }
    notary_creds=(--key "$SMUDGY_NOTARY_KEY_FILE" --key-id "$SMUDGY_NOTARY_KEY_ID" --issuer "$SMUDGY_NOTARY_ISSUER")
    echo "==> Notarizing via App Store Connect API key $SMUDGY_NOTARY_KEY_ID"
elif [[ -n "${SMUDGY_NOTARY_APPLE_ID:-}" && -n "${SMUDGY_NOTARY_TEAM_ID:-}" && -n "${SMUDGY_NOTARY_PASSWORD:-}" ]]; then
    notary_creds=(--apple-id "$SMUDGY_NOTARY_APPLE_ID" --team-id "$SMUDGY_NOTARY_TEAM_ID" --password "$SMUDGY_NOTARY_PASSWORD")
    echo "==> Notarizing via environment credentials (Apple ID $SMUDGY_NOTARY_APPLE_ID)"
else
    echo "error: no notarization credentials found." >&2
    echo "       Create a keychain profile (preferred locally):" >&2
    echo "         xcrun notarytool store-credentials $NOTARY_PROFILE --apple-id <id> --team-id <team> --password <app-specific-pw>" >&2
    echo "       or export SMUDGY_NOTARY_KEY_FILE / SMUDGY_NOTARY_KEY_ID / SMUDGY_NOTARY_ISSUER (CI)," >&2
    echo "       or export SMUDGY_NOTARY_APPLE_ID / SMUDGY_NOTARY_TEAM_ID / SMUDGY_NOTARY_PASSWORD." >&2
    exit 1
fi

out_dir="$repo_root/target/release-mac"
mkdir -p "$out_dir"

# --- build ------------------------------------------------------------------
if [[ "$SKIP_BUILD" -eq 0 ]]; then
    # Materialize the patch-crate-managed dependency patches (patches/*.patch ->
    # target/patch/): [patch.crates-io] points there, so every cargo invocation
    # below (cargo-about included) fails to resolve on a fresh checkout or
    # after a `cargo clean` until they exist.
    command -v cargo-patch-crate >/dev/null 2>&1 || {
        echo "==> cargo install --locked patch-crate"
        cargo install --locked patch-crate
    }
    echo "==> cargo patch-crate --force"
    cargo patch-crate --force

    if [[ -z "$PGO_ARM64" ]]; then
        echo "==> Generating embedded third-party notices"
        cargo about generate --workspace --features smudgy_ui/web-audio-cpal \
            --locked about.hbs -o THIRD-PARTY-NOTICES.md
        python3 bin/normalize-third-party-notices.py
    fi

    # Make sure both darwin targets are installed (no-op if already present).
    if command -v rustup >/dev/null 2>&1; then
        rustup target add "${TARGETS[@]}" >/dev/null
    fi

    # Build the physical-Web-Audio app and the inspector sidecar for each
    # architecture -- mirrors release.ps1's package-qualified feature selection.
    for t in "${TARGETS[@]}"; do
        if [[ -n "$PGO_ARM64" ]]; then
            cargo build --locked --profile "$PROFILE" --target "$t" -p smudgy_inspector
            pgo_manifest="$PGO_ARM64"
            [[ "$t" == "x86_64-apple-darwin" ]] && pgo_manifest="$PGO_X86_64"
            python3 bin/pgo_package.py stage "$pgo_manifest" "target/$t/$PROFILE/smudgy" --target "$t"
        else
            cargo build --profile "$PROFILE" --target "$t" -p smudgy_ui -p smudgy_inspector \
                --features smudgy_ui/web-audio-cpal
        fi
    done

    # Assemble the .app skeleton from the bundle target's already-built smudgy_ui
    # (cargo-bundle reuses that compile -- same target dir, no rebuild).
    #
    # cargo-bundle 0.7.0 has NO package selector: its -p is the short form of
    # --profile, and there is no --package. So we can't pass `-p smudgy_ui`
    # (that parses as a second --profile -> "cannot be used multiple times").
    # Instead run it from ui/ so the current package IS smudgy_ui; cargo-bundle
    # still resolves the shared workspace target dir via cargo metadata, so the
    # Smudgy.app path located below is unchanged.
    #
    # Don't pass --bin: smudgy_ui has exactly one binary, so cargo-bundle picks
    # it automatically AND applies the package-level [package.metadata.bundle]
    # (name = "Smudgy") -> Smudgy.app. With --bin it instead expects a per-bin
    # [package.metadata.bundle.bin.smudgy] table, ignores that name override,
    # and emits smudgy_ui.app -- which the path below would then fail to find.
    echo "==> (cd ui && cargo bundle --profile $PROFILE --target $BUNDLE_TARGET --format osx --features smudgy_ui/web-audio-cpal)"
    ( cd ui
      # cargo-bundle 0.7.0 supports this variable. Its build must not replace PGO.
      [[ -z "$PGO_ARM64" ]] || export CARGO_BUNDLE_SKIP_BUILD=1
      cargo bundle --profile "$PROFILE" --target "$BUNDLE_TARGET" \
          --format osx --features smudgy_ui/web-audio-cpal )
fi

# --- locate the .app --------------------------------------------------------
app="target/$BUNDLE_TARGET/$PROFILE/bundle/osx/Smudgy.app"
if [[ ! -d "$app" ]]; then
    # Be tolerant of cargo-bundle layout differences across versions.
    app="$(/usr/bin/find "target/$BUNDLE_TARGET/$PROFILE/bundle" -maxdepth 2 -name '*.app' -type d -print 2>/dev/null | head -1 || true)"
fi
[[ -n "$app" && -d "$app" ]] || { echo "error: Smudgy.app was not produced (run without --skip-build first?)" >&2; exit 1; }
macos_dir="$app/Contents/MacOS"
resources_dir="$app/Contents/Resources"
mkdir -p "$resources_dir"
install -m 644 THIRD-PARTY-NOTICES.md "$resources_dir/THIRD-PARTY-NOTICES.md"
cmp -s THIRD-PARTY-NOTICES.md "$resources_dir/THIRD-PARTY-NOTICES.md" || {
    echo "error: third-party notice was not copied into Smudgy.app" >&2
    exit 1
}

# --- fuse universal binaries into the bundle --------------------------------
# The app finds the sidecar next to its own executable (current_exe().parent()),
# i.e. Contents/MacOS/smudgy_inspector -- so it sits beside Contents/MacOS/smudgy.
assert_universal() {
    local f="$1" archs
    [[ -f "$f" ]] || { echo "error: expected Mach-O missing: $f" >&2; exit 1; }
    archs="$(lipo -archs "$f")"
    for want in arm64 x86_64; do
        case " $archs " in
            *" $want "*) ;;
            *) echo "error: $f is missing $want (has: $archs)" >&2; exit 1 ;;
        esac
    done
}

if [[ "$SKIP_BUILD" -eq 0 ]]; then
    if [[ -n "$PGO_ARM64" ]]; then
        python3 bin/pgo_package.py check "$PGO_ARM64" "target/aarch64-apple-darwin/$PROFILE/smudgy" --target aarch64-apple-darwin
        python3 bin/pgo_package.py check "$PGO_X86_64" "target/x86_64-apple-darwin/$PROFILE/smudgy" --target x86_64-apple-darwin
    fi
    echo "==> Fusing universal binaries (arm64 + x86_64)"
    for bin in smudgy smudgy_inspector; do
        inputs=()
        for t in "${TARGETS[@]}"; do
            src="target/$t/$PROFILE/$bin"
            [[ -f "$src" ]] || { echo "error: built binary missing: $src" >&2; exit 1; }
            inputs+=("$src")
        done
        lipo -create "${inputs[@]}" -output "$macos_dir/$bin"
        chmod +x "$macos_dir/$bin"
        assert_universal "$macos_dir/$bin"
    done
else
    for bin in smudgy smudgy_inspector; do
        [[ -f "$macos_dir/$bin" ]] || { echo "error: --skip-build but $macos_dir/$bin is missing; run a full build first" >&2; exit 1; }
    done
fi

# --- sign (inside-out) ------------------------------------------------------
# V8 JIT under the hardened runtime needs allow-jit AND allow-unsigned-executable-memory.
# No disable-library-validation: the embedded deno runtime has no ffi/napi (no native loading).
ents="$out_dir/entitlements.plist"
cat > "$ents" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.cs.allow-jit</key>
    <true/>
    <key>com.apple.security.cs.allow-unsigned-executable-memory</key>
    <true/>
</dict>
</plist>
PLIST

sign() {
    codesign --force --options runtime --timestamp \
        --entitlements "$ents" --sign "$SIGN_IDENTITY" "$1"
}

# Sign the nested helper first, then seal the app around it (avoids deprecated --deep).
echo "==> Signing smudgy_inspector (nested helper)"
sign "$macos_dir/smudgy_inspector"
echo "==> Signing Smudgy.app"
sign "$app"

echo "==> Verifying app signature"
codesign --verify --deep --strict --verbose=2 "$app"

# --- notarize + staple the app ----------------------------------------------
notarize() {
    local artifact="$1"
    echo "==> Submitting $(basename "$artifact") for notarization (may take a few minutes)"
    if ! xcrun notarytool submit "$artifact" "${notary_creds[@]}" --wait; then
        echo "error: notarization of $(basename "$artifact") failed." >&2
        echo "       Inspect the log: xcrun notarytool log <submission-id> (same credentials)" >&2
        exit 1
    fi
}

app_zip="$out_dir/Smudgy.zip"
rm -f "$app_zip"
ditto -c -k --keepParent "$app" "$app_zip"
notarize "$app_zip"
xcrun stapler staple "$app"
rm -f "$app_zip"

# --- package the .dmg -------------------------------------------------------
# create-dmg wants a source folder containing only the app to lay out.
dmg_src="$out_dir/dmg-src"
rm -rf "$dmg_src"
mkdir -p "$dmg_src"
ditto "$app" "$dmg_src/Smudgy.app"

dmg="$out_dir/Smudgy.dmg"
rm -f "$dmg"
echo "==> Building $dmg"
create-dmg \
    --volname "Smudgy" \
    --window-size 800 400 \
    --icon-size 100 \
    --icon "Smudgy.app" 200 190 \
    --hide-extension "Smudgy.app" \
    --app-drop-link 600 185 \
    "$dmg" "$dmg_src"
rm -rf "$dmg_src"

# Sign the .dmg ourselves instead of via create-dmg's --codesign: that option
# runs `codesign -s <id>` with no secure timestamp, so its signature would stop
# validating once the (short-lived) cert expires. Match the app/inspector
# signing here. (No --options runtime/entitlements: a .dmg is a container, not
# executable code.)
echo "==> Signing $dmg"
codesign --force --timestamp --sign "$SIGN_IDENTITY" "$dmg"

# --- notarize + staple the .dmg ---------------------------------------------
notarize "$dmg"
xcrun stapler staple "$dmg"

# --- verify -----------------------------------------------------------------
echo "==> Verifying notarization"
xcrun stapler validate "$app"
xcrun stapler validate "$dmg"
spctl --assess --type exec --verbose=2 "$app"
# Gatekeeper's view of the deliverable itself. Informational: the stapler
# validate above is the authoritative ticket check; spctl on a .dmg can be
# finicky across macOS versions, so don't fail an otherwise-good build on it.
spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg" || true

echo "==> Done: $dmg"
