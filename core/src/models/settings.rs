use crate::get_smudgy_home;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use std::{fs, io};

use super::persistence::write_atomic;

/// The distribution channel this binary was built for, decided at compile time
/// from `CARGO_PKG_VERSION`. It is the single source of truth for every
/// dev-vs-release behavior split — the default API endpoint
/// ([`DEFAULT_API_BASE_URL`]), the data dir and OS-keyring namespace
/// ([`crate::get_smudgy_home`] / [`crate::models::auth::keyring_service`]), the
/// window-title marker, the script inspector, and the upgrade nags — so those
/// can never drift apart. `bin/bump-version.sh` and `assets/installer.iss`
/// mirror this rule (in Bash and Inno Pascal) and must stay in sync.
///
/// The five channels are distinguished purely by the semver suffix:
/// - [`Release`](BuildChannel::Release) — a clean `X.Y.Z` with no suffix: the
///   published build. Prod API, `smudgy/` data.
/// - [`ReleaseCandidate`](BuildChannel::ReleaseCandidate) — a prerelease whose
///   first identifier is `rc` (`0.4.0-rc1`, `0.3.2-rc-final`,
///   `0.4.0-rc19-the-final`). A candidate for an imminent release: it behaves
///   exactly like a `Release` for the API endpoint, data dir, and keyring, and
///   raises no upgrade notifications, but stays detectable (this channel, the
///   title marker) so a candidate is never mistaken for the published release.
/// - [`PublicTestBuild`](BuildChannel::PublicTestBuild) and
///   [`Nightly`](BuildChannel::Nightly) — prereleases beginning with `ptb` or
///   `nightly` (`0.5.7-ptb`, `0.5.7-ptb37`, `0.5.7-nightly.1`). They inherit
///   the same prod-like behavior and notification suppression as an RC, while
///   their title marker includes the build timestamp and Git commit.
/// - [`Dev`](BuildChannel::Dev) — any other suffixed version (`0.4.0-beta`,
///   `0.3.2+ci`): an isolated dev/pre-release build (dev API, `smudgy-dev/`
///   data + keyring, script inspector on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildChannel {
    Release,
    ReleaseCandidate,
    PublicTestBuild,
    Nightly,
    Dev,
}

/// This binary's [`BuildChannel`], derived at compile time from its version.
#[must_use]
pub const fn build_channel() -> BuildChannel {
    channel_of(env!("CARGO_PKG_VERSION").as_bytes())
}

/// `true` for an isolated dev/pre-release build (the [`BuildChannel::Dev`]
/// channel): a suffixed version that is not an RC, PTB, or nightly. Drives the
/// dev API default, the `smudgy-dev/` data dir and keyring, and the script
/// inspector. A prod-like prerelease is **not** a dev build — see
/// [`build_channel`].
#[must_use]
pub const fn is_dev_build() -> bool {
    matches!(build_channel(), BuildChannel::Dev)
}

/// `true` when this build is a release candidate
/// ([`BuildChannel::ReleaseCandidate`]): its semver prerelease segment begins
/// with an `rc` identifier. An RC ships like a release (prod API, `smudgy/`
/// data) but suppresses upgrade notifications; this predicate is what makes that
/// suppression and the title marker possible while keeping the candidate
/// detectable.
#[must_use]
pub const fn is_release_candidate() -> bool {
    matches!(build_channel(), BuildChannel::ReleaseCandidate)
}

/// `true` for any prod-like prerelease channel: RC, public test build, or
/// nightly. These builds share the release API, data, and keyring and suppress
/// upgrade notifications.
#[must_use]
pub const fn is_preview_build() -> bool {
    matches!(
        build_channel(),
        BuildChannel::ReleaseCandidate | BuildChannel::PublicTestBuild | BuildChannel::Nightly
    )
}

/// Classify a raw semver string's bytes into a [`BuildChannel`]. Pure (no env),
/// so it is unit-testable with arbitrary versions; the public predicates wrap it
/// around `CARGO_PKG_VERSION`.
const fn channel_of(bytes: &[u8]) -> BuildChannel {
    if version_has_prerelease_marker(bytes, b"rc") {
        BuildChannel::ReleaseCandidate
    } else if version_has_prerelease_marker(bytes, b"ptb") {
        BuildChannel::PublicTestBuild
    } else if version_has_prerelease_marker(bytes, b"nightly") {
        BuildChannel::Nightly
    } else if version_has_suffix(bytes) {
        BuildChannel::Dev
    } else {
        BuildChannel::Release
    }
}

/// `true` when the version carries any semver suffix — a `-prerelease` and/or
/// `+build` (e.g. `0.4.0-beta`, `0.3.2+ci`).
const fn version_has_suffix(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'-' || bytes[i] == b'+' {
            return true;
        }
        i += 1;
    }
    false
}

/// `true` when the version's prerelease segment begins with `marker`
/// (case-insensitive), followed by a digit, `-`, `.`, `+`, or end of string —
/// but never another letter. For example, marker `rc` matches `0.4.0-rc1` but
/// not `0.4.0-rcedar`; marker `ptb` matches `0.5.7-ptb37` but not
/// `0.5.7-ptbeta`.
///
/// Deliberately permissive after the marker: numbered and decorated builds such
/// as `rc19`, `ptb37`, and `nightly-2026-09-01` stay in their intended channel.
/// Assumes valid-semver input (prerelease charset `[0-9A-Za-z.-]`), as enforced
/// upstream by `bin/bump-version.sh` and Cargo's own `CARGO_PKG_VERSION` parse.
const fn version_has_prerelease_marker(bytes: &[u8], marker: &[u8]) -> bool {
    // Locate the prerelease segment (first '-'); a `+build` suffix alone never
    // marks a prerelease channel, and a clean `X.Y.Z` has no suffix at all. When there is no
    // '-', the loop leaves `i == bytes.len()`, so `pre` is one past the end and
    // the length guard below returns `false` before any indexing.
    let mut i = 0;
    while i < bytes.len() && bytes[i] != b'-' {
        i += 1;
    }
    let pre = i + 1; // first byte after the '-' (may be len + 1 when no '-')
    if marker.is_empty() || pre + marker.len() > bytes.len() {
        return false;
    }

    let mut marker_index = 0;
    while marker_index < marker.len() {
        let actual = bytes[pre + marker_index];
        let expected = marker[marker_index];
        if actual != expected && actual != expected.to_ascii_uppercase() {
            return false;
        }
        marker_index += 1;
    }

    // The marker must end here, not extend into another word.
    let after = pre + marker.len();
    if after >= bytes.len() {
        return true; // version ends exactly at the marker
    }
    let c = bytes[after];
    matches!(c, b'-' | b'.' | b'+') || c.is_ascii_digit()
}

/// The API base URL used when [`Settings::api_base_url`] is not set. Derived from
/// the [`build_channel`]: a [`Dev`](BuildChannel::Dev) build defaults to the dev
/// API; a release or prod-like prerelease defaults to prod. Override via
/// `api_base_url` in settings.json.
pub const DEFAULT_API_BASE_URL: &str = if is_dev_build() {
    "https://api.dev.smudgy.org"
} else {
    "https://api.smudgy.org"
};

/// Largest link-tooltip delay accepted by the preferences UI and renderer.
/// Keeps a hand-edited settings file from scheduling an effectively unbounded
/// redraw deadline.
pub const MAX_LINK_TOOLTIP_DELAY_MS: u64 = 60_000;

/// How an SGR bold attribute is presented in terminal output.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case", from = "TerminalBoldModeCompat")]
pub enum TerminalBoldMode {
    /// Increase the selected terminal font's weight without changing color.
    Bold,
    /// Use the bright ANSI palette without changing font weight.
    Bright,
    /// Increase font weight and use the bright ANSI palette.
    BoldAndBright,
}

impl TerminalBoldMode {
    pub const ALL: [Self; 3] = [Self::Bold, Self::Bright, Self::BoldAndBright];

    #[must_use]
    pub const fn uses_bold_weight(self) -> bool {
        matches!(self, Self::Bold | Self::BoldAndBright)
    }

    #[must_use]
    pub const fn uses_bright_palette(self) -> bool {
        matches!(self, Self::Bright | Self::BoldAndBright)
    }
}

impl Default for TerminalBoldMode {
    fn default() -> Self {
        Self::BoldAndBright
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum TerminalBoldModeName {
    Bold,
    Bright,
    BoldAndBright,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TerminalBoldModeCompat {
    Name(TerminalBoldModeName),
    LegacyBool(bool),
}

impl From<TerminalBoldModeCompat> for TerminalBoldMode {
    fn from(value: TerminalBoldModeCompat) -> Self {
        match value {
            TerminalBoldModeCompat::Name(TerminalBoldModeName::Bold) => Self::Bold,
            TerminalBoldModeCompat::Name(TerminalBoldModeName::Bright) => Self::Bright,
            TerminalBoldModeCompat::Name(TerminalBoldModeName::BoldAndBright) => {
                Self::BoldAndBright
            }
            TerminalBoldModeCompat::LegacyBool(true) => Self::BoldAndBright,
            TerminalBoldModeCompat::LegacyBool(false) => Self::Bold,
        }
    }
}

/// Represents the global application settings.
///
/// Loaded from / saved to `settings.json` in the main smudgy config directory.
///
/// Note: any `api_key` entry in settings.json is ignored — cloud access is
/// account-based (session tokens in the OS secret store), not key-based.
// Independent user toggles persisted as JSON; the enum/state-machine shape the
// `struct_excessive_bools` suggestion points at doesn't fit a settings file.
#[allow(clippy::struct_excessive_bools)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Settings {
    /// Opaque application locale preference (`system` or a BCP-47 catalog
    /// tag). Core persists this language-independent value but does not
    /// interpret locale policy or depend on the localization layer.
    #[serde(default = "default_locale_preference")]
    pub locale: String,
    /// The maximum number of lines to keep in the scrollback buffer.
    #[serde(default = "default_scrollback_length")]
    pub scrollback_length: usize,
    /// Override for the smudgy.org API base URL; `None` means [`DEFAULT_API_BASE_URL`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base_url: Option<String>,

    /// The client version the "an upgrade is available" prompt was dismissed for
    /// (the "Dismiss for this version" button). While the server advertises this
    /// same version the prompt stays hidden; a newer version shows it again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_upgrade_version: Option<String>,

    /// Whether smudgy may contact smudgy.org to check for a newer client
    /// version. This is the **only** automatic network call made while signed
    /// out, so cloud-averse users can turn it off entirely; the in-app setting
    /// and the installer's "check for updates" checkbox both write here. On by
    /// default — the check is an unauthenticated, no-payload `GET /health`.
    #[serde(default = "default_true")]
    pub auto_check_for_updates: bool,

    /// Show smudgy as the game being played on a Discord client running on
    /// this machine: "Playing smudgy" while the app is open, plus one server
    /// hostname while connected (its display name instead when the host is
    /// an IP or localhost). That label is the entire disclosure. Everything
    /// rides Discord's local IPC pipe, which is also why it works signed
    /// out and is inert on machines without Discord. On by default; the
    /// Preferences toggle is the opt-out for anyone who'd rather keep what
    /// they play to themselves.
    #[serde(default = "default_true")]
    pub discord_rich_presence: bool,

    /// The client version the map editor's "sign in to use cloud maps" banner
    /// was dismissed at. While this equals the running client version the banner
    /// stays hidden; upgrading to a newer version surfaces it once more (mirrors
    /// [`Self::dismissed_upgrade_version`]). Lets users opposed to cloud
    /// features hide the prompt for good without re-dismissing every launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_signin_banner_version: Option<String>,

    /// Terminal font family name (bundled or system); rendering falls back
    /// to Geist Mono when the family can't be resolved.
    #[serde(default = "default_terminal_font_family")]
    pub terminal_font_family: String,
    /// Terminal font size in pixels. Line height derives from this
    /// (`size * 1.25`).
    #[serde(default = "default_terminal_font_size")]
    pub terminal_font_size: f32,
    /// Allow the terminal font's ligatures and contextual alternates. Off by
    /// default: MUD output assumes a fixed character grid, and substitutions
    /// merging `=>` or `fi` into one glyph break column alignment.
    #[serde(default)]
    pub terminal_font_ligatures: bool,
    /// Choose whether SGR bold changes the selected font's weight, promotes
    /// ordinary ANSI foregrounds to the bright palette, or does both. The alias
    /// migrates the former boolean (`false` = bold, `true` = both).
    #[serde(default, alias = "terminal_bold_is_bright")]
    pub terminal_bold_mode: TerminalBoldMode,
    /// When enabled, ANSI blink (SGR slow/rapid blink) has no visual effect.
    /// Off by default.
    #[serde(default)]
    pub terminal_disable_blink: bool,
    /// Maximum terminal line length in columns; `None` wraps to the pane
    /// width. This is client-side wrapping only (no NAWS negotiation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_line_length: Option<u16>,
    /// Time the pointer must remain over a terminal link before its tooltip is
    /// shown, in milliseconds. Zero shows tooltips immediately.
    #[serde(default)]
    pub link_tooltip_delay_ms: u64,
    /// Named theme: terminal color scheme plus app background/accent.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Map server-supplied 256-color and truecolor values through the
    /// selected theme's perceptual color cube. Off renders those RGB values
    /// literally; the 16 named ANSI colors remain theme-controlled either
    /// way.
    #[serde(default = "default_true")]
    pub theme_extended_colors: bool,
    /// Non-destructive per-theme adjustments, keyed by theme name. The base
    /// schemes are never modified; tweaks survive switching themes and back.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub theme_tweaks: std::collections::HashMap<String, ThemeTweaks>,

    /// Disk budget for the image cache (`<home>/cache/images/`), in MiB. A startup
    /// LRU sweep (oldest fetches first, across all servers) trims to this; the
    /// in-memory decoded-image cap is a separate internal constant.
    #[serde(default = "default_image_cache_max_mb")]
    pub image_cache_max_mb: u64,

    /// Separates multiple commands on one input line. Empty disables
    /// splitting entirely.
    #[serde(default = "default_command_separator")]
    pub command_separator: String,
    /// Lines starting with this prefix are sent verbatim: no separator
    /// splitting AND no alias matching. Empty disables the prefix.
    #[serde(default = "default_raw_line_prefix")]
    pub raw_line_prefix: String,
    /// What the session command input does with the text after you press
    /// Enter (and, for the default, when it loses focus). See
    /// [`CommandInputBehavior`].
    #[serde(default)]
    pub command_input_behavior: CommandInputBehavior,
    /// Mask the command input while the server hides echo (telnet `WILL
    /// ECHO`, the standard password-prompt signal — RFC 857). On by default;
    /// off answers the negotiation the same way but leaves the input
    /// unmasked (the Mudlet `mDisablePasswordMasking` opt-out).
    #[serde(default = "default_true")]
    pub mask_input_on_server_echo: bool,
    /// Up/Down history navigation always cycles through entries that have
    /// the *unselected* part of the current input as a prefix (an empty, or
    /// fully selected input matches *every* entry).
    /// This prefix matching can optionally be case-sensitive. Off by default.
    #[serde(default)]
    pub history_case_sensitive_match: bool,
    /// How many recent commands the Up/Down input history remembers, per
    /// server profile. `0` means unlimited (never evict).
    #[serde(default = "default_max_history")]
    pub max_history: usize,

    /// Hide session/pane headers (title bars) unless the window's toolbar is
    /// expanded — the distraction-free default. Off shows every header all
    /// the time. A pane created with `titleBar: 'always-show'` keeps its
    /// header regardless.
    #[serde(default = "default_true")]
    pub hide_pane_headers: bool,

    /// Map areas the user disabled: excluded from room identification and
    /// auto-routing (fed into `smudgy_cloud::Mapper::set_disabled_areas`).
    /// Ids may reference areas that no longer exist or haven't synced yet.
    ///
    /// This is the **derived effective set** — exactly the `area_id`s in
    /// [`Self::map_area_prefs`] with `disabled == true`. It is written for
    /// backwards compatibility (older clients read only this field); the
    /// timestamped prefs are authoritative for the cross-device sync.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_map_areas: Vec<smudgy_cloud::AreaId>,

    /// Timestamped per-area enable/disable preferences — the local mirror of
    /// the cloud `/me/area-prefs` rows, used as the offline cache and the
    /// last-write-wins basis for cross-device sync. A present entry is an
    /// *explicit* preference (`disabled` true or false); an absent area
    /// defaults to enabled. `updated_at` is the time of the local change, or
    /// the server-stamped time once a reconcile has adopted the server row.
    ///
    /// Migrated from a legacy [`Self::disabled_map_areas`]-only settings file
    /// at load time (see the UI's `init`); the two stay consistent on save.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub map_area_prefs: Vec<MapAreaPref>,

    #[serde(default)]
    pub logging: LoggingSettings,

    /// Persisted Web Audio output policy. Session rows use the durable
    /// server/profile pair instead of the process-local session id; sandbox
    /// rows use their versionless package root. The UI applies these values
    /// before publishing a physical runtime generation.
    #[serde(default, skip_serializing_if = "AudioSettings::is_default")]
    pub audio: AudioSettings,

    /// Unlocks advanced scripting features that bypass the package sandbox: "Remove sandbox"
    /// (run an installed package with full authority on the main isolate) and the script
    /// inspector. Off by default — these are powerful, footgun-prone affordances, so they stay
    /// hidden until the user explicitly opts in here.
    #[serde(default)]
    pub advanced_scripting_features: bool,
}

/// One persisted per-area enable/disable preference. The local mirror of a
/// `/me/area-prefs` row; see [`Settings::map_area_prefs`].
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MapAreaPref {
    pub area_id: smudgy_cloud::AreaId,
    pub disabled: bool,
    pub updated_at: DateTime<Utc>,
}

/// Maximum durable session policies retained from a hand-edited settings
/// file. The UI normally creates far fewer; this keeps corrupt input bounded.
pub const MAX_AUDIO_SESSION_SETTINGS: usize = 256;
/// Maximum versionless sandbox-root policies retained under one session.
pub const MAX_AUDIO_PACKAGE_SETTINGS: usize = 256;

/// One independently muted volume setting. The persisted unit is a whole
/// percent so UI round-trips are exact; mixer conversion happens at the edge.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioGainSettings {
    #[serde(
        default = "default_audio_volume",
        deserialize_with = "deserialize_audio_volume"
    )]
    pub volume: u8,
    #[serde(default)]
    pub muted: bool,
}

impl AudioGainSettings {
    #[must_use]
    pub fn is_default(self) -> bool {
        self == Self::default()
    }

    /// Mixer-linear value corresponding to the persisted whole percent.
    #[must_use]
    pub fn linear(self) -> f32 {
        f32::from(self.volume) / 100.0
    }

    /// Replace the remembered volume while retaining independent mute state.
    #[must_use]
    pub const fn with_volume(self, volume: u8) -> Self {
        Self {
            volume: if volume > 100 { 100 } else { volume },
            muted: self.muted,
        }
    }

    /// Replace mute while retaining the remembered volume.
    #[must_use]
    pub const fn with_muted(self, muted: bool) -> Self {
        Self {
            volume: self.volume,
            muted,
        }
    }
}

impl Default for AudioGainSettings {
    fn default() -> Self {
        Self {
            volume: default_audio_volume(),
            muted: false,
        }
    }
}

const fn default_audio_volume() -> u8 {
    100
}

/// Be liberal with hand-edited JSON: any JSON number is rounded and clamped;
/// other value kinds fall back to the audible default instead of making the
/// whole settings file unreadable.
fn deserialize_audio_volume<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let Some(value) = value.as_f64() else {
        return Ok(default_audio_volume());
    };
    if !value.is_finite() {
        return Ok(default_audio_volume());
    }
    Ok(value.round().clamp(0.0, 100.0) as u8)
}

/// Versionless sandbox-root policy under one durable session identity.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PackageAudioSettings {
    pub owner: String,
    pub name: String,
    #[serde(default)]
    pub gain: AudioGainSettings,
}

/// Audio policy for every process-local session opened from one durable
/// server/profile pair.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SessionAudioSettings {
    pub server: String,
    pub profile: String,
    #[serde(default)]
    pub gain: AudioGainSettings,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<PackageAudioSettings>,
}

/// Complete persisted audio policy.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioSettings {
    #[serde(default)]
    pub master: AudioGainSettings,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<SessionAudioSettings>,
}

impl AudioSettings {
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.master == AudioGainSettings::default() && self.sessions.is_empty()
    }

    /// Find the durable policy for a server/profile pair.
    #[must_use]
    pub fn session(&self, server: &str, profile: &str) -> Option<&SessionAudioSettings> {
        self.sessions
            .iter()
            .find(|row| row.server == server && row.profile == profile)
    }

    /// Find or create the durable policy for a server/profile pair.
    pub fn session_mut(&mut self, server: &str, profile: &str) -> &mut SessionAudioSettings {
        if let Some(index) = self
            .sessions
            .iter()
            .position(|row| row.server == server && row.profile == profile)
        {
            return &mut self.sessions[index];
        }
        if self.sessions.len() >= MAX_AUDIO_SESSION_SETTINGS {
            self.sessions.remove(0);
        }
        self.sessions.push(SessionAudioSettings {
            server: server.to_string(),
            profile: profile.to_string(),
            gain: AudioGainSettings::default(),
            packages: Vec::new(),
        });
        self.sessions
            .last_mut()
            .expect("a just-pushed audio session exists")
    }

    /// Canonicalize and bound policy loaded from editable JSON. Duplicate
    /// identities resolve last-writer-wins, matching their textual order.
    pub fn normalize(&mut self) {
        let mut sessions = std::collections::BTreeMap::new();
        for mut session in std::mem::take(&mut self.sessions) {
            if session.server.is_empty() || session.profile.is_empty() {
                continue;
            }
            let mut packages = std::collections::BTreeMap::new();
            for mut package in session.packages {
                package.owner.make_ascii_lowercase();
                package.name.make_ascii_lowercase();
                if package.owner.is_empty() || package.name.is_empty() {
                    continue;
                }
                let key = (package.owner.clone(), package.name.clone());
                if package.gain.is_default() {
                    packages.remove(&key);
                } else {
                    packages.insert(key, package);
                }
            }
            session.packages = packages
                .into_values()
                .take(MAX_AUDIO_PACKAGE_SETTINGS)
                .collect();
            let key = (session.server.clone(), session.profile.clone());
            if session.gain.is_default() && session.packages.is_empty() {
                sessions.remove(&key);
            } else {
                sessions.insert(key, session);
            }
        }
        self.sessions = sessions
            .into_values()
            .take(MAX_AUDIO_SESSION_SETTINGS)
            .collect();
    }
}

impl SessionAudioSettings {
    #[must_use]
    pub fn package(&self, owner: &str, name: &str) -> Option<&PackageAudioSettings> {
        self.packages.iter().find(|row| {
            row.owner.eq_ignore_ascii_case(owner) && row.name.eq_ignore_ascii_case(name)
        })
    }

    pub fn package_mut(&mut self, owner: &str, name: &str) -> &mut PackageAudioSettings {
        if let Some(index) = self.packages.iter().position(|row| {
            row.owner.eq_ignore_ascii_case(owner) && row.name.eq_ignore_ascii_case(name)
        }) {
            return &mut self.packages[index];
        }
        if self.packages.len() >= MAX_AUDIO_PACKAGE_SETTINGS {
            self.packages.remove(0);
        }
        self.packages.push(PackageAudioSettings {
            owner: owner.to_ascii_lowercase(),
            name: name.to_ascii_lowercase(),
            gain: AudioGainSettings::default(),
        });
        self.packages
            .last_mut()
            .expect("a just-pushed package policy exists")
    }
}

/// Non-destructive adjustments layered on a base color scheme.
///
/// Sliders are `-1.0..=1.0` with `0.0` neutral. `background` moves only the
/// surface colors (terminal background, input strip, selection) so text
/// contrast is preserved; `brightness`/`saturation` move only the text
/// colors; `contrast` expands every text color's distance from the (tweaked)
/// background. Individual `overrides` (slot name → `#rrggbb`) are applied
/// last, verbatim.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct ThemeTweaks {
    #[serde(default)]
    pub background: f32,
    #[serde(default)]
    pub brightness: f32,
    #[serde(default)]
    pub contrast: f32,
    #[serde(default)]
    pub saturation: f32,
    /// Slot names: `background`, `foreground`, `input_background`,
    /// `selection`, `echo`, `warn`, `output`, `ansi0`..`ansi15`.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub overrides: std::collections::HashMap<String, String>,
}

impl ThemeTweaks {
    /// True when every slider is neutral and no override is set.
    #[must_use]
    pub fn is_neutral(&self) -> bool {
        self.background == 0.0
            && self.brightness == 0.0
            && self.contrast == 0.0
            && self.saturation == 0.0
            && self.overrides.is_empty()
    }
}

/// Session logging behavior.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoggingSettings {
    /// Write the plaintext screen log (post-ANSI-strip, post-gag, includes
    /// echoes and sent commands) to `<server>/logs/`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Additionally write a raw log of the exact bytes received from the
    /// server (including ANSI escape sequences) to a sibling `.raw.log`
    /// file. Applies to connections opened after the change.
    #[serde(default)]
    pub log_raw: bool,
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            log_raw: false,
        }
    }
}

/// What the session command input does with the just-sent text after you
/// press Enter — and, for the default, what happens when the input loses
/// focus with text still in it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CommandInputBehavior {
    /// Select the sent text (so the next keystroke replaces it), then clear
    /// the input entirely when it loses focus. The default.
    #[default]
    SelectAllClearOnBlur,
    /// Select the sent text and leave it; it persists (selected) until
    /// replaced. Losing focus does nothing. The historical behavior.
    SelectAll,
    /// Clear the input immediately on send.
    Clear,
}

impl CommandInputBehavior {
    /// Every variant in display order — the source for the preferences picker.
    pub const ALL: [CommandInputBehavior; 3] = [
        CommandInputBehavior::SelectAllClearOnBlur,
        CommandInputBehavior::SelectAll,
        CommandInputBehavior::Clear,
    ];

    /// A short human label for the preferences picker.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            CommandInputBehavior::SelectAllClearOnBlur => {
                "Select all on send, clear when unfocused"
            }
            CommandInputBehavior::SelectAll => "Select all on send",
            CommandInputBehavior::Clear => "Clear on send",
        }
    }
}

impl std::fmt::Display for CommandInputBehavior {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// The read-only view of the app settings exposed to scripts via `getSettings()`
/// (`smudgy:core`). Carries only the display/behavior settings a script can reasonably act
/// on — never anything sensitive (no API base URL, dismissal bookkeeping, or secrets). Sent
/// to the runtime as a snapshot and refreshed live by the UI's `ApplySettings`; serialized
/// camelCase to match the JS contract.
#[derive(Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScriptSettings {
    /// Separates multiple commands on one input line; empty disables splitting.
    pub command_separator: String,
    /// Lines starting with this prefix are sent verbatim; empty disables it.
    pub raw_line_prefix: String,
    /// The scrollback buffer's maximum line count.
    pub scrollback_length: usize,
    /// Terminal font family name.
    pub terminal_font_family: String,
    /// Terminal font size in pixels (line height is `size * 1.25`).
    pub terminal_font_size: f32,
    /// Maximum terminal line length in columns; absent means wrap to pane width.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_line_length: Option<u16>,
    /// The active color-scheme name.
    pub theme: String,
    /// What the command input does with the text after a send, as a camelCase tag
    /// (`selectAllClearOnBlur` / `selectAll` / `clear`). A view-only string (not the persisted
    /// `snake_case` form) so the script-facing enum casing matches the rest of the API.
    pub command_input_behavior: String,
    /// The resolved terminal color scheme (theme + tweaks) as `#rrggbb` hex. Resolved by the
    /// UI (which owns scheme resolution), so it is absent until the first `ApplySettings`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub palette: Option<ScriptPalette>,
}

impl From<&Settings> for ScriptSettings {
    fn from(s: &Settings) -> Self {
        Self {
            command_separator: s.command_separator.clone(),
            raw_line_prefix: s.raw_line_prefix.clone(),
            scrollback_length: s.scrollback_length,
            terminal_font_family: s.terminal_font_family.clone(),
            terminal_font_size: s.terminal_font_size,
            terminal_line_length: s.terminal_line_length,
            theme: s.theme.clone(),
            command_input_behavior: match s.command_input_behavior {
                CommandInputBehavior::SelectAllClearOnBlur => "selectAllClearOnBlur",
                CommandInputBehavior::SelectAll => "selectAll",
                CommandInputBehavior::Clear => "clear",
            }
            .to_string(),
            // The effective palette is resolved by the UI crate (color schemes live there) and
            // filled in via `ApplySettings`; the core-side snapshot starts without it.
            palette: None,
        }
    }
}

/// The resolved terminal palette as web hex colors (`#rrggbb`), for scripts that want to
/// match the terminal's scheme (e.g. coloring widget or `highlight()` output). Mirrors the
/// UI's effective `TerminalPalette` after theme tweaks are applied.
#[derive(Serialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScriptPalette {
    /// The 16 ANSI colors, indexed `[normal 8, bright 8]` (black, red, green, yellow, blue,
    /// magenta, cyan, white).
    pub ansi: Vec<String>,
    pub foreground: String,
    pub background: String,
    pub echo: String,
    pub warn: String,
    pub output: String,
    pub selection: String,
    pub input_background: String,
    /// The app accent color; absent when the scheme falls back to the foreground.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
}

impl Settings {
    /// Returns the smudgy.org API base URL, falling back to [`DEFAULT_API_BASE_URL`]
    /// when no override is configured.
    #[must_use]
    pub fn base_url(&self) -> &str {
        self.api_base_url.as_deref().unwrap_or(DEFAULT_API_BASE_URL)
    }
}

/// Helper for serde default scrollback length.
fn default_scrollback_length() -> usize {
    100_000
}

fn default_max_history() -> usize {
    1000
}

fn default_terminal_font_family() -> String {
    "Geist Mono".to_string()
}

const fn default_terminal_font_size() -> f32 {
    16.0
}

fn default_image_cache_max_mb() -> u64 {
    256
}

fn default_theme() -> String {
    "Smudgy".to_string()
}

fn default_command_separator() -> String {
    ";".to_string()
}

fn default_raw_line_prefix() -> String {
    "\\\\".to_string()
}

const fn default_true() -> bool {
    true
}

fn default_locale_preference() -> String {
    "system".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            locale: default_locale_preference(),
            scrollback_length: default_scrollback_length(),
            api_base_url: None,
            dismissed_upgrade_version: None,
            auto_check_for_updates: true,
            discord_rich_presence: true,
            dismissed_signin_banner_version: None,
            terminal_font_family: default_terminal_font_family(),
            terminal_font_size: default_terminal_font_size(),
            terminal_font_ligatures: false,
            terminal_bold_mode: TerminalBoldMode::default(),
            terminal_disable_blink: false,
            terminal_line_length: None,
            link_tooltip_delay_ms: 0,
            theme: default_theme(),
            theme_extended_colors: true,
            theme_tweaks: std::collections::HashMap::new(),
            image_cache_max_mb: default_image_cache_max_mb(),
            command_separator: default_command_separator(),
            raw_line_prefix: default_raw_line_prefix(),
            command_input_behavior: CommandInputBehavior::default(),
            mask_input_on_server_echo: true,
            history_case_sensitive_match: false,
            max_history: default_max_history(),
            hide_pane_headers: true,
            disabled_map_areas: Vec::new(),
            map_area_prefs: Vec::new(),
            logging: LoggingSettings::default(),
            audio: AudioSettings::default(),
            advanced_scripting_features: false,
        }
    }
}

/// Loads the global application settings from `settings.json`.
///
/// If the file does not exist or cannot be parsed, returns the default settings.
/// Errors during file reading (other than not found) or parsing are logged.
///
/// # Returns
///
/// The loaded `Settings` or `Settings::default()`.
#[must_use]
pub fn load_settings() -> Settings {
    let mut settings = match try_load_settings() {
        Ok(settings) => settings,
        Err(e) => {
            eprintln!("Warning: Failed to load settings, using defaults: {e}");
            Settings::default()
        }
    };
    // While the installer's update-check seed is present it overrides the
    // persisted auto-check value, so every load (the UI checkbox and the
    // update-check master switch alike) sees the installer's choice until the
    // user changes the setting in-app (see [`apply_update_check_seed`]).
    apply_update_check_seed(&mut settings);
    settings
}

/// Internal helper function to attempt loading settings and return a Result.
///
/// # Errors
///
/// This function can return an error in the following cases:
/// - If the smudgy home directory cannot be determined (e.g., `dirs::home_dir()` is `None`).
/// - If reading `settings.json` fails for reasons other than the file not being found (e.g., permission issues).
/// - If parsing the content of `settings.json` fails (e.g., invalid JSON format).
fn try_load_settings() -> Result<Settings> {
    let smudgy_dir = get_smudgy_home()?;
    let settings_path = smudgy_dir.join("settings.json");

    try_load_settings_from(&settings_path)
}

fn try_load_settings_from(settings_path: &std::path::Path) -> Result<Settings> {
    match fs::read_to_string(&settings_path) {
        Ok(content) => {
            let mut settings: Settings =
                serde_json::from_str(&content).context("Failed to parse settings.json")?;
            settings.audio.normalize();
            Ok(settings)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // File not found is expected on first run, return default.
            Ok(Settings::default())
        }
        Err(e) => {
            // Other read errors are propagated
            Err(e).context(format!(
                "Failed to read settings.json at {}",
                settings_path.display()
            ))
        }
    }
}

/// Atomically replace only the audio policy in the latest settings file.
///
/// Unlike [`load_settings`], this merge is deliberately fallible: a malformed
/// or unreadable existing file is preserved and reported to the caller. A
/// missing file is the normal first-run case and starts from defaults.
pub fn merge_audio_settings(audio: &AudioSettings) -> Result<()> {
    let settings_path = get_smudgy_home()?.join("settings.json");
    merge_audio_settings_at(&settings_path, audio)
}

fn merge_audio_settings_at(settings_path: &std::path::Path, audio: &AudioSettings) -> Result<()> {
    let mut latest = try_load_settings_from(settings_path)?;
    latest.audio = audio.clone();
    save_settings_to(settings_path, &latest)
}

/// File name of the installer's update-check seed. The Windows installer writes
/// it (containing `0`) when the user clears its "check for updates" task. While
/// the file is present it **overrides** [`Settings::auto_check_for_updates`] on
/// every launch (the installer's choice wins over `settings.json`); the app
/// deletes it the moment the user changes the setting in-app, after which
/// `settings.json` is authoritative. macOS and Linux ship no installer task, so
/// they never write a seed and simply take the default (on).
const UPDATE_CHECK_SEED_FILE: &str = "update-check-seed";

/// Override `settings.auto_check_for_updates` from the installer seed when one is
/// present, so the installer's "check for updates" choice takes precedence over
/// the value in `settings.json`. Called by [`load_settings`] on every load, so
/// the override holds across launches until the user changes the setting in-app
/// (which deletes the seed via [`clear_update_check_seed`]). The seed is never
/// consumed here. Best-effort: a missing or malformed seed leaves `settings`
/// untouched.
pub fn apply_update_check_seed(settings: &mut Settings) {
    if let Some(value) = read_update_check_seed() {
        settings.auto_check_for_updates = value;
    }
}

/// The installer seed's boolean value (`0` = off, `1` = on), or `None` when no
/// recognizable seed is present. Read errors other than "not found" are logged
/// and treated as absent — a bad seed must never block startup.
fn read_update_check_seed() -> Option<bool> {
    let seed_path = get_smudgy_home().ok()?.join(UPDATE_CHECK_SEED_FILE);
    match fs::read_to_string(&seed_path) {
        Ok(contents) => parse_update_check_seed(&contents),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!("Warning: failed to read update-check seed: {e}");
            None
        }
    }
}

/// Map raw seed-file contents to the boolean it encodes: `0` = off, `1` = on
/// (surrounding whitespace ignored). Anything else is unrecognized (`None`),
/// leaving the persisted value in force.
fn parse_update_check_seed(contents: &str) -> Option<bool> {
    match contents.trim() {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

/// Delete the installer's update-check seed so `settings.json` becomes
/// authoritative for [`Settings::auto_check_for_updates`]. Called when the user
/// changes the update-check preference in-app. Best-effort and idempotent — a
/// missing seed is not an error.
pub fn clear_update_check_seed() {
    let Ok(smudgy_dir) = get_smudgy_home() else {
        return;
    };
    match fs::remove_file(smudgy_dir.join(UPDATE_CHECK_SEED_FILE)) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("Warning: failed to delete update-check seed: {e}"),
    }
}

/// Persist that the "an upgrade is available" prompt was dismissed for
/// `version` (load-modify-write of `settings.json`). Idempotent.
///
/// # Errors
///
/// Propagates the underlying settings load/save errors.
pub fn set_dismissed_upgrade_version(version: &str) -> Result<()> {
    let mut settings = load_settings();
    settings.dismissed_upgrade_version = Some(version.to_string());
    save_settings(&settings)
}

/// Persist that the map editor's "sign in to use cloud maps" banner was
/// dismissed at `version` (load-modify-write of `settings.json`). The banner
/// stays hidden until the running client version no longer matches. Idempotent.
///
/// # Errors
///
/// Propagates the underlying settings load/save errors.
pub fn set_dismissed_signin_banner_version(version: &str) -> Result<()> {
    let mut settings = load_settings();
    settings.dismissed_signin_banner_version = Some(version.to_string());
    save_settings(&settings)
}

/// Saves the global application settings to `settings.json`.
///
/// This will overwrite the existing file.
///
/// # Arguments
///
/// * `settings` - The `Settings` struct to save.
///
/// # Errors
///
/// Returns an error if:
/// - The smudgy home directory cannot be determined.
/// - The settings cannot be serialized to JSON.
/// - The `settings.json` file cannot be written to disk (e.g., permission issues, disk full).
pub fn save_settings(settings: &Settings) -> Result<()> {
    let smudgy_dir = get_smudgy_home()?;
    let settings_path = smudgy_dir.join("settings.json");

    save_settings_to(&settings_path, settings)
}

fn save_settings_to(settings_path: &std::path::Path, settings: &Settings) -> Result<()> {
    let mut normalized = settings.clone();
    normalized.audio.normalize();
    let json_content =
        serde_json::to_string_pretty(&normalized).context("Failed to serialize settings")?;

    write_atomic(&settings_path, json_content.as_bytes()).context(format!(
        "Failed to write settings.json at {}",
        settings_path.display()
    ))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_settings_default_to_an_omitted_audio_policy() {
        let settings: Settings = serde_json::from_str(r#"{"scrollback_length":5000}"#).unwrap();
        assert!(settings.audio.is_default());
        assert!(
            !serde_json::to_string(&Settings::default())
                .unwrap()
                .contains("\"audio\"")
        );
    }

    #[test]
    fn hand_edited_audio_volumes_are_rounded_clamped_and_type_safe() {
        let settings: Settings = serde_json::from_str(
            r#"{
                "audio": {
                    "master": {"volume": -8.2, "muted": true},
                    "sessions": [{
                        "server": "example",
                        "profile": "main",
                        "gain": {"volume": 140.6},
                        "packages": [{"owner":"Owner","name":"Bell","gain":{"volume":"bad"}}]
                    }]
                }
            }"#,
        )
        .unwrap();
        assert_eq!(settings.audio.master.volume, 0);
        assert!(settings.audio.master.muted);
        let session = settings.audio.session("example", "main").unwrap();
        assert_eq!(session.gain.volume, 100);
        assert_eq!(session.package("owner", "bell").unwrap().gain.volume, 100);
    }

    #[test]
    fn structured_audio_identity_does_not_alias_delimiter_shaped_pairs() {
        let mut audio = AudioSettings::default();
        audio.session_mut("a/b", "c").gain.volume = 20;
        audio.session_mut("a", "b/c").gain.volume = 80;
        audio.normalize();
        assert_eq!(audio.session("a/b", "c").unwrap().gain.volume, 20);
        assert_eq!(audio.session("a", "b/c").unwrap().gain.volume, 80);
    }

    #[test]
    fn audio_normalize_retains_nondefault_reinstall_policy_and_compacts_defaults() {
        let mut audio = AudioSettings::default();
        let session = audio.session_mut("server", "profile");
        session.package_mut("Owner", "Keep").gain.volume = 35;
        session.package_mut("Owner", "Drop");
        audio.session_mut("empty", "default");
        audio.normalize();

        assert_eq!(audio.sessions.len(), 1);
        let session = audio.session("server", "profile").unwrap();
        assert_eq!(session.packages.len(), 1);
        assert_eq!(session.package("owner", "keep").unwrap().gain.volume, 35);
        assert!(session.package("owner", "drop").is_none());
    }

    #[test]
    fn audio_normalize_later_defaults_reset_duplicate_session_and_package_rows() {
        let mut audio = AudioSettings {
            sessions: vec![
                SessionAudioSettings {
                    server: "server".into(),
                    profile: "profile".into(),
                    gain: AudioGainSettings {
                        volume: 35,
                        muted: false,
                    },
                    packages: Vec::new(),
                },
                SessionAudioSettings {
                    server: "server".into(),
                    profile: "profile".into(),
                    gain: AudioGainSettings::default(),
                    packages: Vec::new(),
                },
                SessionAudioSettings {
                    server: "other".into(),
                    profile: "profile".into(),
                    gain: AudioGainSettings::default(),
                    packages: vec![
                        PackageAudioSettings {
                            owner: "Owner".into(),
                            name: "Bell".into(),
                            gain: AudioGainSettings {
                                volume: 35,
                                muted: false,
                            },
                        },
                        PackageAudioSettings {
                            owner: "owner".into(),
                            name: "bell".into(),
                            gain: AudioGainSettings::default(),
                        },
                    ],
                },
            ],
            ..AudioSettings::default()
        };

        audio.normalize();
        assert!(audio.session("server", "profile").is_none());
        assert!(audio.session("other", "profile").is_none());
    }

    #[test]
    fn audio_merge_preserves_latest_unrelated_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let latest = Settings {
            scrollback_length: 12_345,
            locale: "pl-PL".to_string(),
            ..Settings::default()
        };
        save_settings_to(&path, &latest).unwrap();

        let mut audio = AudioSettings::default();
        audio.master = AudioGainSettings {
            volume: 45,
            muted: true,
        };
        merge_audio_settings_at(&path, &audio).unwrap();

        let merged = try_load_settings_from(&path).unwrap();
        assert_eq!(merged.scrollback_length, 12_345);
        assert_eq!(merged.locale, "pl-PL");
        assert_eq!(merged.audio.master, audio.master);
    }

    #[test]
    fn audio_merge_refuses_to_replace_malformed_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let malformed = b"{ this is not settings json";
        fs::write(&path, malformed).unwrap();

        let mut audio = AudioSettings::default();
        audio.master.volume = 45;
        assert!(merge_audio_settings_at(&path, &audio).is_err());
        assert_eq!(fs::read(&path).unwrap(), malformed);
    }

    #[test]
    fn script_settings_from_carries_display_settings_without_palette() {
        let settings = Settings::default();
        let script = ScriptSettings::from(&settings);
        assert_eq!(script.command_separator, settings.command_separator);
        assert_eq!(script.raw_line_prefix, settings.raw_line_prefix);
        assert_eq!(script.terminal_font_family, settings.terminal_font_family);
        assert_eq!(script.terminal_font_size, settings.terminal_font_size);
        assert_eq!(script.theme, settings.theme);
        // The default command-input behavior maps to its camelCase script-facing tag.
        assert_eq!(script.command_input_behavior, "selectAllClearOnBlur");
        // The palette is resolved by the UI crate, so it is absent in the core-side snapshot.
        assert!(script.palette.is_none());
    }

    #[test]
    fn script_settings_serializes_to_the_camelcase_contract() {
        // Mirrors the JS `Settings` interface in smudgy-core.d.ts: camelCase keys, the
        // command-input behavior as its snake_case wire string, and `palette` present only
        // when set. Pinning the shape keeps the op output and the author-facing typings aligned.
        let script = ScriptSettings {
            command_separator: ";".to_string(),
            raw_line_prefix: "\\".to_string(),
            scrollback_length: 5000,
            terminal_font_family: "Geist Mono".to_string(),
            terminal_font_size: 16.0,
            terminal_line_length: None,
            theme: "Smudgy".to_string(),
            command_input_behavior: "selectAll".to_string(),
            palette: Some(ScriptPalette {
                ansi: vec!["#000000".to_string(); 16],
                foreground: "#ffffff".to_string(),
                background: "#000000".to_string(),
                echo: "#abcdef".to_string(),
                warn: "#ff0000".to_string(),
                output: "#00ff00".to_string(),
                selection: "#222222".to_string(),
                input_background: "#111111".to_string(),
                accent: Some("#5599ff".to_string()),
            }),
        };
        let value = serde_json::to_value(&script).unwrap();
        assert_eq!(value["commandSeparator"], ";");
        assert_eq!(value["rawLinePrefix"], "\\");
        assert_eq!(value["scrollbackLength"], 5000);
        assert_eq!(value["terminalFontFamily"], "Geist Mono");
        assert_eq!(value["terminalFontSize"], 16.0);
        assert_eq!(value["commandInputBehavior"], "selectAll");
        assert_eq!(value["palette"]["inputBackground"], "#111111");
        assert_eq!(value["palette"]["accent"], "#5599ff");
        assert_eq!(value["palette"]["ansi"][0], "#000000");
        // Omitted optionals don't serialize (matching the optional `?` fields in the .d.ts).
        assert!(value.get("terminalLineLength").is_none());
    }

    #[test]
    fn build_channel_detection() {
        use BuildChannel::{Dev, Nightly, PublicTestBuild, Release, ReleaseCandidate};

        // Clean releases and `+build`-only metadata.
        assert_eq!(channel_of(b"0.3.2"), Release);
        assert_eq!(channel_of(b"1.0.0"), Release);
        assert_eq!(channel_of(b"0.3.2+ci"), Dev);

        // Release candidates — the user-facing naming examples, verbatim.
        assert_eq!(channel_of(b"0.3.2-rc1"), ReleaseCandidate);
        assert_eq!(channel_of(b"0.3.2-rc-pre-release"), ReleaseCandidate);
        assert_eq!(channel_of(b"0.4.0-rc19-the-final-final"), ReleaseCandidate);
        assert_eq!(channel_of(b"0.4.0-rc"), ReleaseCandidate); // bare `rc`
        assert_eq!(channel_of(b"0.4.0-rc.1"), ReleaseCandidate); // dotted id
        assert_eq!(channel_of(b"0.4.0-rc1+ci"), ReleaseCandidate); // rc + build meta
        assert_eq!(channel_of(b"0.4.0-RC1"), ReleaseCandidate); // case-insensitive

        // Public test and nightly builds follow the same marker and boundary
        // rules as release candidates, including numbered variants.
        assert_eq!(channel_of(b"0.5.7-ptb"), PublicTestBuild);
        assert_eq!(channel_of(b"0.5.7-ptb1"), PublicTestBuild);
        assert_eq!(channel_of(b"0.5.7-ptb37+ci"), PublicTestBuild);
        assert_eq!(channel_of(b"0.5.7-PTB.2"), PublicTestBuild);
        assert_eq!(channel_of(b"0.5.7-nightly"), Nightly);
        assert_eq!(channel_of(b"0.5.7-nightly1"), Nightly);
        assert_eq!(channel_of(b"0.5.7-NIGHTLY-2026-09-01"), Nightly);

        // Dev/pre-release — suffixed, but not an `rc` identifier. The `rc` must
        // not bleed into another word.
        assert_eq!(channel_of(b"0.4.0-beta"), Dev);
        assert_eq!(channel_of(b"0.4.0-alpha.1"), Dev);
        assert_eq!(channel_of(b"0.4.0-release"), Dev);
        assert_eq!(channel_of(b"0.4.0-rcedar"), Dev);
        assert_eq!(channel_of(b"0.5.7-ptbeta"), Dev);
        assert_eq!(channel_of(b"0.5.7-nightlyish"), Dev);

        assert!(version_has_prerelease_marker(b"0.3.2-rc1", b"rc"));
        assert!(!version_has_prerelease_marker(b"0.3.2", b"rc"));
        assert!(!version_has_prerelease_marker(b"0.4.0-beta", b"rc"));
    }

    #[test]
    fn prod_like_prereleases_are_not_dev_builds() {
        for version in [b"0.3.2-rc1".as_slice(), b"0.5.7-ptb37", b"0.5.7-nightly"] {
            assert_ne!(channel_of(version), BuildChannel::Dev);
        }
    }

    #[test]
    fn legacy_settings_with_api_key_still_parse() {
        let legacy = r#"{ "api_key": "smudgy_legacy_key", "scrollback_length": 5000 }"#;
        let settings: Settings = serde_json::from_str(legacy).expect("legacy settings parse");
        assert_eq!(settings.scrollback_length, 5000);
    }

    #[test]
    fn settings_without_disabled_map_areas_parse_with_empty_default() {
        let existing = r#"{ "scrollback_length": 5000 }"#;
        let settings: Settings = serde_json::from_str(existing).expect("existing settings parse");
        assert!(settings.disabled_map_areas.is_empty());
        assert_eq!(settings.locale, "system");
    }

    #[test]
    fn link_tooltip_delay_defaults_to_zero_and_roundtrips() {
        let existing = r#"{ "scrollback_length": 5000 }"#;
        let settings: Settings = serde_json::from_str(existing).expect("existing settings parse");
        assert_eq!(settings.link_tooltip_delay_ms, 0);

        let configured = Settings {
            link_tooltip_delay_ms: 375,
            ..Settings::default()
        };
        let parsed: Settings =
            serde_json::from_str(&serde_json::to_string(&configured).unwrap()).unwrap();
        assert_eq!(parsed.link_tooltip_delay_ms, 375);
    }

    #[test]
    fn terminal_bold_mode_defaults_to_bold_and_bright_and_roundtrips() {
        let existing = r#"{ "scrollback_length": 5000 }"#;
        let settings: Settings = serde_json::from_str(existing).expect("existing settings parse");
        assert_eq!(settings.terminal_bold_mode, TerminalBoldMode::BoldAndBright);

        for mode in TerminalBoldMode::ALL {
            let configured = Settings {
                terminal_bold_mode: mode,
                ..Settings::default()
            };
            let json = serde_json::to_string(&configured).unwrap();
            assert!(json.contains("terminal_bold_mode"));
            assert!(!json.contains("terminal_bold_is_bright"));
            let parsed: Settings = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.terminal_bold_mode, mode);
        }
    }

    #[test]
    fn terminal_disable_blink_defaults_off_and_round_trips_on() {
        // A settings file predating the field deserializes to off (blink
        // renders normally, today's behavior), not an error.
        let existing = r#"{ "scrollback_length": 5000 }"#;
        let settings: Settings = serde_json::from_str(existing).expect("parse");
        assert!(!settings.terminal_disable_blink);

        let opted_in = Settings {
            terminal_disable_blink: true,
            ..Settings::default()
        };
        let parsed: Settings =
            serde_json::from_str(&serde_json::to_string(&opted_in).unwrap()).expect("parse");
        assert!(parsed.terminal_disable_blink);
    }

    #[test]
    fn legacy_bold_is_bright_boolean_migrates_without_changing_rendering() {
        let enabled: Settings =
            serde_json::from_str(r#"{"terminal_bold_is_bright":true}"#).unwrap();
        assert_eq!(enabled.terminal_bold_mode, TerminalBoldMode::BoldAndBright);

        let disabled: Settings =
            serde_json::from_str(r#"{"terminal_bold_is_bright":false}"#).unwrap();
        assert_eq!(disabled.terminal_bold_mode, TerminalBoldMode::Bold);
    }

    #[test]
    fn terminal_bold_mode_rejects_unreleased_kebab_case_alias() {
        assert!(serde_json::from_str::<TerminalBoldMode>(r#""bold-and-bright""#).is_err());
    }

    #[test]
    fn locale_preference_roundtrips_without_changing_legacy_defaults() {
        let mut settings = Settings::default();
        settings.locale = "zh-TW".to_string();
        let json = serde_json::to_string(&settings).expect("settings serialize");
        let parsed: Settings = serde_json::from_str(&json).expect("settings parse");
        assert_eq!(parsed.locale, "zh-TW");
    }

    #[test]
    fn themed_extended_colors_default_on_and_round_trip_off() {
        let existing = r#"{ "scrollback_length": 5000 }"#;
        let settings: Settings = serde_json::from_str(existing).expect("existing settings parse");
        assert!(settings.theme_extended_colors);

        let literal = Settings {
            theme_extended_colors: false,
            ..Settings::default()
        };
        let parsed: Settings =
            serde_json::from_str(&serde_json::to_string(&literal).unwrap()).unwrap();
        assert!(!parsed.theme_extended_colors);
    }

    #[test]
    fn disabled_map_areas_roundtrip() {
        let area = smudgy_cloud::AreaId(smudgy_cloud::Uuid::from_u128(7));
        let settings = Settings {
            disabled_map_areas: vec![area],
            ..Settings::default()
        };

        let json = serde_json::to_string(&settings).expect("serialize");
        let parsed: Settings = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed.disabled_map_areas, vec![area]);

        // The empty default stays out of settings.json entirely.
        let json = serde_json::to_string(&Settings::default()).expect("serialize");
        assert!(!json.contains("disabled_map_areas"));
    }

    #[test]
    fn settings_without_map_area_prefs_parse_with_empty_default() {
        let existing = r#"{ "scrollback_length": 5000 }"#;
        let settings: Settings = serde_json::from_str(existing).expect("existing settings parse");
        assert!(settings.map_area_prefs.is_empty());
    }

    #[test]
    fn map_area_prefs_roundtrip() {
        let area = smudgy_cloud::AreaId(smudgy_cloud::Uuid::from_u128(7));
        let pref = MapAreaPref {
            area_id: area,
            disabled: true,
            updated_at: DateTime::from_timestamp(1_700_000_000, 0).expect("valid timestamp"),
        };
        let settings = Settings {
            map_area_prefs: vec![pref.clone()],
            ..Settings::default()
        };

        let json = serde_json::to_string(&settings).expect("serialize");
        let parsed: Settings = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed.map_area_prefs, vec![pref]);

        // The empty default stays out of settings.json entirely.
        let json = serde_json::to_string(&Settings::default()).expect("serialize");
        assert!(!json.contains("map_area_prefs"));
    }

    #[test]
    fn advanced_scripting_defaults_off_and_round_trips() {
        // An existing settings file without the field deserializes with the feature OFF.
        let existing = r#"{ "scrollback_length": 5000 }"#;
        let settings: Settings = serde_json::from_str(existing).expect("parse");
        assert!(
            !settings.advanced_scripting_features,
            "advanced features default off"
        );

        let on = Settings {
            advanced_scripting_features: true,
            ..Settings::default()
        };
        let parsed: Settings = serde_json::from_str(&serde_json::to_string(&on).unwrap()).unwrap();
        assert!(parsed.advanced_scripting_features);
    }

    #[test]
    fn update_check_seed_parses_known_values_only() {
        // The installer writes "0" (opted out); "1" is accepted for symmetry.
        assert_eq!(parse_update_check_seed("0"), Some(false));
        assert_eq!(parse_update_check_seed("1"), Some(true));
        // Surrounding whitespace/newlines are ignored.
        assert_eq!(parse_update_check_seed(" 0\n"), Some(false));
        assert_eq!(parse_update_check_seed("1\r\n"), Some(true));
        // Anything unrecognized leaves the persisted value in force.
        assert_eq!(parse_update_check_seed(""), None);
        assert_eq!(parse_update_check_seed("true"), None);
        assert_eq!(parse_update_check_seed("00"), None);
    }

    #[test]
    fn auto_check_for_updates_defaults_on_and_round_trips() {
        // A settings file predating the field deserializes with the check ON,
        // so existing installs keep getting update nudges.
        let existing = r#"{ "scrollback_length": 5000 }"#;
        let settings: Settings = serde_json::from_str(existing).expect("parse");
        assert!(settings.auto_check_for_updates, "update check defaults on");

        let off = Settings {
            auto_check_for_updates: false,
            ..Settings::default()
        };
        let parsed: Settings = serde_json::from_str(&serde_json::to_string(&off).unwrap()).unwrap();
        assert!(!parsed.auto_check_for_updates);
    }

    #[test]
    fn discord_rich_presence_defaults_on_and_round_trips() {
        // A settings file predating the field deserializes with presence ON —
        // the feature is opt-out via Preferences.
        let existing = r#"{ "scrollback_length": 5000 }"#;
        let settings: Settings = serde_json::from_str(existing).expect("parse");
        assert!(settings.discord_rich_presence, "presence defaults on");

        let off = Settings {
            discord_rich_presence: false,
            ..Settings::default()
        };
        let parsed: Settings = serde_json::from_str(&serde_json::to_string(&off).unwrap()).unwrap();
        assert!(!parsed.discord_rich_presence);
    }

    #[test]
    fn dismissed_signin_banner_version_round_trips_and_omits_when_unset() {
        let existing = r#"{ "scrollback_length": 5000 }"#;
        let settings: Settings = serde_json::from_str(existing).expect("parse");
        assert!(settings.dismissed_signin_banner_version.is_none());

        let dismissed = Settings {
            dismissed_signin_banner_version: Some("1.2.3".to_string()),
            ..Settings::default()
        };
        let parsed: Settings =
            serde_json::from_str(&serde_json::to_string(&dismissed).unwrap()).unwrap();
        assert_eq!(
            parsed.dismissed_signin_banner_version.as_deref(),
            Some("1.2.3")
        );

        // Unset stays out of settings.json entirely.
        let json = serde_json::to_string(&Settings::default()).expect("serialize");
        assert!(!json.contains("dismissed_signin_banner_version"));
    }

    #[test]
    fn command_input_behavior_defaults_and_round_trips() {
        // A settings file predating the field deserializes to the new default
        // (select-all + clear-on-blur), not an error.
        let existing = r#"{ "scrollback_length": 5000 }"#;
        let settings: Settings = serde_json::from_str(existing).expect("parse");
        assert_eq!(
            settings.command_input_behavior,
            CommandInputBehavior::SelectAllClearOnBlur
        );

        // Each variant survives a serialize/deserialize round-trip via its
        // snake_case wire form.
        for behavior in CommandInputBehavior::ALL {
            let with = Settings {
                command_input_behavior: behavior,
                ..Settings::default()
            };
            let json = serde_json::to_string(&with).expect("serialize");
            let parsed: Settings = serde_json::from_str(&json).expect("parse");
            assert_eq!(parsed.command_input_behavior, behavior);
        }

        // The wire form is snake_case.
        assert!(
            serde_json::to_string(&Settings {
                command_input_behavior: CommandInputBehavior::Clear,
                ..Settings::default()
            })
            .unwrap()
            .contains("\"command_input_behavior\":\"clear\"")
        );
    }

    #[test]
    fn mask_input_on_server_echo_defaults_on_and_round_trips_off() {
        // A settings file predating the field deserializes to on (auto-mask
        // enabled), not an error.
        let existing = r#"{ "scrollback_length": 5000 }"#;
        let settings: Settings = serde_json::from_str(existing).expect("parse");
        assert!(settings.mask_input_on_server_echo);

        // The opt-out survives a round-trip.
        let opted_out = Settings {
            mask_input_on_server_echo: false,
            ..Settings::default()
        };
        let parsed: Settings =
            serde_json::from_str(&serde_json::to_string(&opted_out).unwrap()).expect("parse");
        assert!(!parsed.mask_input_on_server_echo);
    }

    #[test]
    fn history_case_sensitive_match_defaults_off_and_round_trips_on() {
        // A settings file predating the field deserializes to off (case-
        // insensitive history-prefix matching being the default), not an
        // error.
        let existing = r#"{ "scrollback_length": 5000 }"#;
        let settings: Settings = serde_json::from_str(existing).expect("parse");
        assert!(!settings.history_case_sensitive_match);

        let opted_in = Settings {
            history_case_sensitive_match: true,
            ..Settings::default()
        };
        let parsed: Settings =
            serde_json::from_str(&serde_json::to_string(&opted_in).unwrap()).expect("parse");
        assert!(parsed.history_case_sensitive_match);
    }

    #[test]
    fn max_history_defaults_to_1000_and_round_trips_zero_for_unlimited() {
        // A settings file predating the field deserializes to the new default
        // of 1000, not an error.
        let existing = r#"{ "scrollback_length": 5000 }"#;
        let settings: Settings = serde_json::from_str(existing).expect("parse");
        assert_eq!(settings.max_history, 1000);

        let unlimited = Settings {
            max_history: 0,
            ..Settings::default()
        };
        let parsed: Settings =
            serde_json::from_str(&serde_json::to_string(&unlimited).unwrap()).expect("parse");
        assert_eq!(parsed.max_history, 0);
    }

    #[test]
    fn base_url_falls_back_to_default() {
        let settings = Settings::default();
        assert_eq!(settings.base_url(), DEFAULT_API_BASE_URL);

        let settings = Settings {
            api_base_url: Some("https://api.example.org".to_string()),
            ..Settings::default()
        };
        assert_eq!(settings.base_url(), "https://api.example.org");
    }
}
