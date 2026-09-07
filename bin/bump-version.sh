#!/usr/bin/env bash
# Bump the smudgy version everywhere it lives.
#
# Usage: bin/bump-version.sh [--preview] [--force] <new-version>
# Normal bumps keep source at X.Y.Z-ptb. --preview stamps X.Y.Z-ptb.N
# for the current base and leaves CHANGELOG.md alone. --force permits other
# version conventions (including a stable release); it does not skip validation.
# Materialize dependency patches with cargo patch-crate --force first.
#
# <new-version> is a full semver string: MAJOR.MINOR.PATCH with an optional
# `-prerelease` and/or `+build` suffix (e.g. 0.3.2, 0.4.0-beta, 0.4.0-rc.1+ci).
# For convenience, compact PTB/nightly spellings such as `0.5.7ptb37` are
# accepted and normalized to valid semver (`0.5.7-ptb37`) before files change.
#
# The suffix picks a build channel (mirrors core's `build_channel`),
# all derived at compile time from the version — this script does not edit
# settings.rs:
#   - clean X.Y.Z              -> release: prod API, smudgy/ data, bare title.
#   - prerelease starting `rc` -> release candidate: behaves like a release
#       (prod API, smudgy/ data) but raises no upgrade notifications and is
#       tagged "RELEASE CANDIDATE <version>" so a tester knows which one they run
#       (e.g. 0.3.2-rc1, 0.4.0-rc19-the-final-final).
#   - prerelease starting `ptb` or `nightly` -> prod-like preview: the same API,
#       data, keyring, installer identity, and notification behavior as an RC;
#       the title also shows the build time and Git commit when available.
#   - any other suffix         -> dev/pre-release: dev API, smudgy-dev/ data,
#       "DEV BUILD" title (e.g. 0.4.0-beta).
# So bumping the version is all that's needed to retarget a build at a channel.
#
# Updates (in the smudgy repo):
#   - every smudgy_* crate's Cargo.toml        (lock-step [package] versions:
#       discovered from top-level crate manifests)
#   - assets/installer.iss                     (MyAppVersion)
#   - CHANGELOG.md                             (stamps "## [<version>] - Unreleased" with today's date)
#   - Cargo.lock                               (refreshed via cargo metadata)
#
# Updates (in the sibling smudgy-web repo, if present):
#   - terraform/smudgy-web.tf                  (newest_client_version, the soft "upgrade available" nudge)
#     Override the repo location with SMUDGY_WEB_DIR; skipped with a warning if not found.
#
# The script edits files only; review and commit the result yourself (note the
# smudgy-web change is a *separate* repo and needs its own commit).

set -euo pipefail

cd "$(dirname "$0")/.."

PREVIEW=false
FORCE=false
usage() {
    echo "Usage: $0 [--preview] [--force] <new-version>"
    echo "  <X.Y.Z-ptb>               change the source version"
    echo "  --preview <X.Y.Z-ptb.N>   stamp a build of the current source version"
    echo "  --force <version>         allow another convention, e.g. a stable release"
}
while [[ $# -gt 0 ]]; do
    case "$1" in
        --preview) PREVIEW=true ;;
        --force) FORCE=true ;;
        --help|-h) usage; exit 0 ;;
        --*) usage >&2; exit 1 ;;
        *) break ;;
    esac
    shift
done
if [[ $# -ne 1 ]]; then
    usage >&2
    exit 1
fi

REQUESTED_VERSION="$1"
NEW_VERSION="$REQUESTED_VERSION"

# Cargo package versions must be valid semver, which requires a '-' before a
# prerelease. Accept the common compact PTB/nightly spelling at this CLI
# boundary, then store its canonical semver equivalent everywhere.
COMPACT_PREVIEW_RE='^([0-9]+\.[0-9]+\.[0-9]+)([Pp][Tt][Bb]|[Nn][Ii][Gg][Hh][Tt][Ll][Yy])([0-9][0-9A-Za-z.-]*|[-.][0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'
if [[ "$NEW_VERSION" =~ $COMPACT_PREVIEW_RE ]]; then
    NEW_VERSION="${BASH_REMATCH[1]}-${BASH_REMATCH[2]}${BASH_REMATCH[3]}${BASH_REMATCH[4]}"
    echo "Normalizing $REQUESTED_VERSION -> $NEW_VERSION (Cargo requires semver prereleases to use '-')"
fi

# Full semver: X.Y.Z with optional -prerelease and/or +build. POSIX ERE (no
# non-capturing groups), so groups 1/2 capture the prerelease/build suffixes.
SEMVER_RE='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'
if ! [[ "$NEW_VERSION" =~ $SEMVER_RE ]]; then
    echo "error: '$NEW_VERSION' is not a valid semver version (X.Y.Z, optional -prerelease / +build)" >&2
    exit 1
fi
PRERELEASE="${BASH_REMATCH[1]}"   # includes leading '-', empty if none
BUILD="${BASH_REMATCH[2]}"        # includes leading '+', empty if none

# Pick the build channel from the suffix (mirrors core's `build_channel`). A
# prerelease whose first identifier is `rc`, `ptb`, or `nightly` is prod-like;
# the marker must end at the first '-', '.', '+', or digit so words like
# "rcedar" / "ptbeta" don't enter a preview channel. Any other suffix is dev;
# a clean X.Y.Z is a prod release.
PRE_ID="${PRERELEASE#-}"          # prerelease without the leading '-', empty if none
if [[ "$PRE_ID" =~ ^[Rr][Cc]($|[-.+0-9]) ]]; then
    API_BASE_URL="https://api.smudgy.org"
    CHANNEL="release-candidate"
elif [[ "$PRE_ID" =~ ^[Pp][Tt][Bb]($|[-.+0-9]) ]]; then
    API_BASE_URL="https://api.smudgy.org"
    CHANNEL="public-test-build"
elif [[ "$PRE_ID" =~ ^[Nn][Ii][Gg][Hh][Tt][Ll][Yy]($|[-.+0-9]) ]]; then
    API_BASE_URL="https://api.smudgy.org"
    CHANNEL="nightly"
elif [[ -n "$PRERELEASE" || -n "$BUILD" ]]; then
    API_BASE_URL="https://api.dev.smudgy.org"
    CHANNEL="dev / pre-release"
else
    API_BASE_URL="https://api.smudgy.org"
    CHANNEL="prod / release"
fi

if $PREVIEW && [[ "$CHANNEL" != "public-test-build" && "$CHANNEL" != "nightly" ]]; then
    echo "error: --preview requires a PTB or nightly version" >&2
    exit 1
fi

# Escape a string's regex-special dots for safe use as a sed/grep BRE pattern.
# (In BRE, '+' and '-' are literal — and GNU sed treats '\+' as a quantifier —
# so dots are the only semver character that needs escaping.)
escape_re() { printf '%s' "$1" | sed 's/\./\\./g'; }

CURRENT_VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' ui/Cargo.toml | head -1)

if [[ -z "$CURRENT_VERSION" ]]; then
    echo "error: could not read current version from ui/Cargo.toml" >&2
    exit 1
fi

if [[ "$NEW_VERSION" == "$CURRENT_VERSION" ]]; then
    echo "error: version is already $CURRENT_VERSION" >&2
    exit 1
fi

# Fail before touching files. Numbered builds are an explicit operation;
# ordinary source bumps must leave main eligible for the nightly coordinator.
BASE_RE='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)-ptb$'
if ! $FORCE; then
    if $PREVIEW; then
        NUMBERED_RE="^$(escape_re "$CURRENT_VERSION")\\.[1-9][0-9]*$"
        if ! [[ "$CURRENT_VERSION" =~ $BASE_RE && "$NEW_VERSION" =~ $NUMBERED_RE ]]; then
            echo "error: --preview requires $CURRENT_VERSION.N from an unnumbered X.Y.Z-ptb source; use --force to override the convention" >&2
            exit 1
        fi
    elif ! [[ "$NEW_VERSION" =~ $BASE_RE ]]; then
        echo "error: source versions must use X.Y.Z-ptb; use --preview for a numbered build or --force for another convention" >&2
        exit 1
    fi
fi

NEW_RE=$(escape_re "$NEW_VERSION")

echo "Bumping $CURRENT_VERSION -> $NEW_VERSION"
echo "  channel: $CHANNEL (build will default to $API_BASE_URL)"

# Crate package versions, kept in lock-step: every smudgy_* crate carries the same
# version (ui's is the source of truth the client sends as X-Smudgy-Client-Version).
# The set has historically drifted — widgets, inspector, and bench lagged behind — so
# replace each manifest's FIRST `version = "..."` line (the [package] version, which
# precedes any dependency requirement) regardless of its current value, rather than
# anchoring on ui's old version (which would silently skip a lagging crate).
#
# Use awk, not `sed -i '0,/re/s//.../'`: the `0,/re/` first-match address is a GNU sed
# extension. It works under Git Bash on Windows but on macOS/BSD sed it silently matches
# nothing — no error, exit 0, no edit — so every crate stayed unbumped. awk's first-match
# flag is portable across both. Write to a temp file then mv (rather than piping through
# `&&`) so an awk failure trips `set -e` instead of being swallowed by the AND-list.
for manifest in */Cargo.toml; do
    # Include path-dependency crates as well as workspace members. Third-party
    # crates are not part of the application's version series.
    grep -q '^name = "smudgy_' "$manifest" || continue
    awk -v ver="$NEW_VERSION" '
        !bumped && /^version = ".*"/ { sub(/".*"/, "\"" ver "\""); bumped = 1 }
        { print }
    ' "$manifest" > "$manifest.tmp"
    mv "$manifest.tmp" "$manifest"
    echo "  $manifest"
done

# Inno Setup installer
sed -i.bak "s/^#define MyAppVersion \".*\"/#define MyAppVersion \"$NEW_VERSION\"/" assets/installer.iss
rm assets/installer.iss.bak
echo "  assets/installer.iss"

# CHANGELOG.md: stamp the unreleased section for this version, if present.
TODAY=$(date +%Y-%m-%d)
if $PREVIEW; then
    echo "  (leaving CHANGELOG.md unchanged for preview build)"
elif grep -q "^## \[$NEW_RE\] - Unreleased" CHANGELOG.md; then
    sed -i.bak "s/^## \[$NEW_RE\] - Unreleased/## [$NEW_VERSION] - $TODAY/" CHANGELOG.md
    rm CHANGELOG.md.bak
    echo "  CHANGELOG.md (stamped $TODAY)"
else
    echo "  warning: CHANGELOG.md has no '## [$NEW_VERSION] - Unreleased' section; update it manually" >&2
fi

# Refresh Cargo.lock without building anything.
cargo metadata --format-version 1 > /dev/null
echo "  Cargo.lock"

# smudgy-web (separate repo): bump newest_client_version, the soft "upgrade
# available" nudge ceiling — but ONLY for a clean release. Advertising a
# dev/pre-release or preview version as "newest" would nudge every prod client
# toward a pre-release, and preview channels must raise no upgrade nags at all,
# so those channels skip the bump. min_client_version is
# always left alone (the hard 426 gate is rolled out by hand, prod-coordinated).
SMUDGY_WEB_DIR="${SMUDGY_WEB_DIR:-../smudgy-web}"
TF_FILE="$SMUDGY_WEB_DIR/terraform/smudgy-web.tf"
if [[ "$CHANNEL" != "prod / release" ]]; then
    echo "  (skipping smudgy-web newest_client_version bump for a $CHANNEL build)"
elif [[ -f "$TF_FILE" ]]; then
    sed -i.bak "s|^\( *newest_client_version *= *\"\)[^\"]*\(\"\)|\1$NEW_VERSION\2|" "$TF_FILE"
    rm "$TF_FILE.bak"
    echo "  $TF_FILE (newest_client_version -> $NEW_VERSION; review the nearby comment)"
else
    echo "  warning: $TF_FILE not found; skipped newest_client_version bump" >&2
    echo "           (set SMUDGY_WEB_DIR if the smudgy-web repo lives elsewhere)" >&2
fi

echo
echo "Done. Review with: git diff"
echo "Then commit the smudgy repo:   git commit -am \"chore: bump version to $NEW_VERSION\""
if [[ "$CHANNEL" == "prod / release" && -f "$TF_FILE" ]]; then
    echo "And commit smudgy-web separately (different repo):"
    echo "  (cd \"$SMUDGY_WEB_DIR\" && git commit -am \"chore: newest_client_version -> $NEW_VERSION\")"
fi
