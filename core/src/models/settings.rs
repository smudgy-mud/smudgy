use crate::get_smudgy_home;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
/// The three channels are distinguished purely by the semver suffix:
/// - [`Release`](BuildChannel::Release) — a clean `X.Y.Z` with no suffix: the
///   published build. Prod API, `smudgy/` data.
/// - [`ReleaseCandidate`](BuildChannel::ReleaseCandidate) — a prerelease whose
///   first identifier is `rc` (`0.4.0-rc1`, `0.3.2-rc-final`,
///   `0.4.0-rc19-the-final`). A candidate for an imminent release: it behaves
///   exactly like a `Release` for the API endpoint, data dir, and keyring, and
///   raises no upgrade notifications, but stays detectable (this channel, the
///   title marker) so a candidate is never mistaken for the published release.
/// - [`Dev`](BuildChannel::Dev) — any other suffixed version (`0.4.0-beta`,
///   `0.3.2+ci`): an isolated dev/pre-release build (dev API, `smudgy-dev/`
///   data + keyring, script inspector on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildChannel {
    Release,
    ReleaseCandidate,
    Dev,
}

/// This binary's [`BuildChannel`], derived at compile time from its version.
#[must_use]
pub const fn build_channel() -> BuildChannel {
    channel_of(env!("CARGO_PKG_VERSION").as_bytes())
}

/// `true` for an isolated dev/pre-release build (the [`BuildChannel::Dev`]
/// channel): a suffixed version that is *not* a release candidate. Drives the
/// dev API default, the `smudgy-dev/` data dir and keyring, and the script
/// inspector. A release candidate is **not** a dev build — see [`build_channel`].
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
    version_is_rc(env!("CARGO_PKG_VERSION").as_bytes())
}

/// Classify a raw semver string's bytes into a [`BuildChannel`]. Pure (no env),
/// so it is unit-testable with arbitrary versions; the public predicates wrap it
/// around `CARGO_PKG_VERSION`.
const fn channel_of(bytes: &[u8]) -> BuildChannel {
    if version_is_rc(bytes) {
        BuildChannel::ReleaseCandidate
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

/// `true` when the version's prerelease segment begins with an `rc` identifier:
/// `rc` (case-insensitive) at the start of the prerelease, followed by the end
/// of that identifier — a digit, `-`, `.`, `+`, or end of string — but never a
/// letter (so `0.4.0-release` and `0.4.0-rcedar` are *not* release candidates).
/// Matches `0.4.0-rc1`, `0.3.2-rc-pre-release`, and `0.4.0-rc19-the-final-final`.
///
/// Deliberately permissive after `rc`: because `0.3.2-rc-pre-release` must be a
/// candidate, *any* prerelease that begins with `rc` — including `rc-<free
/// text>` like `0.4.0-rc-debug` — is treated as one and therefore ships to
/// **prod** (prod API, the shared `smudgy/` data dir, no upgrade nags). To get
/// an isolated dev build, name the prerelease so it does **not** start with `rc`
/// (e.g. `0.4.0-debug`, `0.4.0-beta`). Assumes valid-semver input (prerelease
/// charset `[0-9A-Za-z.-]`), as enforced upstream by `bin/bump-version.sh` and
/// Cargo's own `CARGO_PKG_VERSION` parse.
const fn version_is_rc(bytes: &[u8]) -> bool {
    // Locate the prerelease segment (first '-'); a `+build` suffix alone never
    // marks an RC, and a clean `X.Y.Z` has no suffix at all. When there is no
    // '-', the loop leaves `i == bytes.len()`, so `pre` is one past the end and
    // the length guard below returns `false` before any indexing.
    let mut i = 0;
    while i < bytes.len() && bytes[i] != b'-' {
        i += 1;
    }
    let pre = i + 1; // first byte after the '-' (may be len + 1 when no '-')
    // Need at least the two bytes "rc".
    if pre + 1 >= bytes.len() {
        return false;
    }
    // `rc` start, case-insensitive (matching literals keeps this a const fn and
    // sidesteps clippy::manual_ignore_case_cmp on a `to_ascii_lowercase` compare).
    if !matches!(bytes[pre], b'r' | b'R') || !matches!(bytes[pre + 1], b'c' | b'C') {
        return false;
    }
    // The `rc` identifier must end here, not extend into another word.
    let after = pre + 2;
    if after >= bytes.len() {
        return true; // version ends exactly at "...-rc"
    }
    let c = bytes[after];
    matches!(c, b'-' | b'.' | b'+') || c.is_ascii_digit()
}

/// The API base URL used when [`Settings::api_base_url`] is not set. Derived from
/// the [`build_channel`]: a [`Dev`](BuildChannel::Dev) build defaults to the dev
/// API; a release or release candidate defaults to prod. Override via
/// `api_base_url` in settings.json.
pub const DEFAULT_API_BASE_URL: &str = if is_dev_build() {
    "https://api.dev.smudgy.org"
} else {
    "https://api.smudgy.org"
};

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
    /// Maximum terminal line length in columns; `None` wraps to the pane
    /// width. This is client-side wrapping only (no NAWS negotiation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_line_length: Option<u16>,
    /// Named theme: terminal color scheme plus app background/accent.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Non-destructive per-theme adjustments, keyed by theme name. The base
    /// schemes are never modified; tweaks survive switching themes and back.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub theme_tweaks: std::collections::HashMap<String, ThemeTweaks>,

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

fn default_terminal_font_family() -> String {
    "Geist Mono".to_string()
}

const fn default_terminal_font_size() -> f32 {
    16.0
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
            dismissed_signin_banner_version: None,
            terminal_font_family: default_terminal_font_family(),
            terminal_font_size: default_terminal_font_size(),
            terminal_line_length: None,
            theme: default_theme(),
            theme_tweaks: std::collections::HashMap::new(),
            command_separator: default_command_separator(),
            raw_line_prefix: default_raw_line_prefix(),
            command_input_behavior: CommandInputBehavior::default(),
            mask_input_on_server_echo: true,
            hide_pane_headers: true,
            disabled_map_areas: Vec::new(),
            map_area_prefs: Vec::new(),
            logging: LoggingSettings::default(),
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

    match fs::read_to_string(&settings_path) {
        Ok(content) => {
            let settings: Settings =
                serde_json::from_str(&content).context("Failed to parse settings.json")?;
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

    let json_content =
        serde_json::to_string_pretty(settings).context("Failed to serialize settings")?;

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
        use BuildChannel::{Dev, Release, ReleaseCandidate};

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

        // Dev/pre-release — suffixed, but not an `rc` identifier. The `rc` must
        // not bleed into another word.
        assert_eq!(channel_of(b"0.4.0-beta"), Dev);
        assert_eq!(channel_of(b"0.4.0-alpha.1"), Dev);
        assert_eq!(channel_of(b"0.4.0-release"), Dev);
        assert_eq!(channel_of(b"0.4.0-rcedar"), Dev);

        // The public predicates are the two views the call sites use.
        assert!(version_is_rc(b"0.3.2-rc1"));
        assert!(!version_is_rc(b"0.3.2"));
        assert!(!version_is_rc(b"0.4.0-beta"));
    }

    #[test]
    fn release_candidate_is_not_a_dev_build() {
        // The whole point: an RC inherits release behavior (prod API, `smudgy/`
        // home, release keyring) precisely because it is *not* a dev build.
        assert_eq!(channel_of(b"0.3.2-rc1"), BuildChannel::ReleaseCandidate);
        assert_ne!(channel_of(b"0.3.2-rc1"), BuildChannel::Dev);
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
    fn locale_preference_roundtrips_without_changing_legacy_defaults() {
        let mut settings = Settings::default();
        settings.locale = "zh-TW".to_string();
        let json = serde_json::to_string(&settings).expect("settings serialize");
        let parsed: Settings = serde_json::from_str(&json).expect("settings parse");
        assert_eq!(parsed.locale, "zh-TW");
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
        assert!(!settings.advanced_scripting_features, "advanced features default off");

        let on = Settings {
            advanced_scripting_features: true,
            ..Settings::default()
        };
        let parsed: Settings =
            serde_json::from_str(&serde_json::to_string(&on).unwrap()).unwrap();
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
        let parsed: Settings =
            serde_json::from_str(&serde_json::to_string(&off).unwrap()).unwrap();
        assert!(!parsed.auto_check_for_updates);
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
        assert_eq!(parsed.dismissed_signin_banner_version.as_deref(), Some("1.2.3"));

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
