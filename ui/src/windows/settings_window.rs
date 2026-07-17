//! The settings window: account lifecycle (passwordless signup and login
//! via emailed one-time codes), handle management, client preferences
//! (appearance, input, logging), and the security tab (API keys + sessions).
//!
//! All server calls run as `Task::perform` futures against the shared
//! [`CloudApiClient`]; outcomes that change app-global account state bubble
//! up as [`Event`]s for the daemon's `CloudAccount` controller. Preference
//! changes likewise bubble up ([`Event::SettingsChanged`]) — the daemon
//! persists and propagates them; this window never writes `settings.json`.

use iced::widget::{
    button, checkbox, column, container, markdown, pick_list, row, rule, slider, space, text,
    text_input,
};
use iced::{Alignment, Background, Color, Length, Task};
use smudgy_core::models::settings::{
    CommandInputBehavior, Settings, ThemeTweaks, clear_update_check_seed, load_settings,
};
use smudgy_cloud::cloud_api::{ApiKeyInfo, AuthSession, CreatedApiKey, SessionInfo, UserProfile};
use smudgy_cloud::{CloudError, Uuid};
use smudgy_i18n::LocalePreference;

use crate::cloud_account::CloudHandles;
use crate::components::cloud_errors::display_error;
use crate::components::color_picker::{self, ColorPicker};
use crate::components::social_panel::{self, SocialPanel};
use crate::prefs;
use crate::theme::{self, Element as ThemedElement};
use crate::update::Update;

/// The bundled third-party license notices, shown in the Licenses tab. This
/// file is generated from the dependency graph and the hand-written font /
/// icon / runtime preamble by `cargo about generate about.hbs -o
/// THIRD-PARTY-NOTICES.md` (see `about.toml` / `about.hbs` at the repo root).
const THIRD_PARTY_NOTICES: &str = include_str!("../../../THIRD-PARTY-NOTICES.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Account,
    Preferences,
    Security,
    Friends,
    Licenses,
}

/// Where the signed-out account flow currently is. Panels render from this
/// tracked state — the code-entry step, for example, only exists because an
/// email submission put us there, not as a free-floating tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountFlow {
    /// The unified passwordless entry: one email field. There is no password
    /// and no separate "create account" step — the server creates the account
    /// on first sight, and a nickname (when needed) is chosen after sign-in.
    SignIn,
    /// A one-time code was mailed to `email`; awaiting the paste. Empty `email`
    /// means the user arrived with a code in hand and still has to tell us the
    /// address.
    EnterCode { email: String },
}

/// The tabs of the theme-tweak panel under the Theme picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TweakTab {
    Adjust,
    Colors,
}

/// Which adjustment slider moved (all range `-1.0..=1.0`, `0.0` neutral).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TweakSlider {
    Background,
    Brightness,
    Contrast,
    Saturation,
}

#[derive(Debug, Clone)]
pub enum Message {
    TabSelected(Tab),
    ShowFlow(AccountFlow),

    EmailChanged(String),
    /// Submit the email to get a one-time code (the unified entry: signs in an
    /// existing account or creates a new one server-side — same call either way).
    CodeRequested,
    CodeRequestResult(Result<(), CloudError>),

    CodeChanged(String),
    VerifySubmitted,
    VerifyResult(Result<AuthSession, CloudError>),
    ResendCode,
    ResendResult(Result<(), CloudError>),

    NicknameChanged(String),
    NicknameSubmitted,
    NicknameResult(Result<Box<UserProfile>, CloudError>),
    /// Reveal or hide the nickname editor in the signed-in view. The editor is
    /// buried behind a button so its presence isn't mistaken for a required step.
    EditNickname(bool),

    RefreshStatusPressed,
    SignOutPressed,
    SignOutEverywhereChanged(bool),

    SecurityRefresh,
    ApiKeysLoaded(Result<Vec<ApiKeyInfo>, CloudError>),
    SessionsLoaded(Result<Vec<SessionInfo>, CloudError>),
    CreateApiKeyPressed,
    ApiKeyCreated(Result<CreatedApiKey, CloudError>),
    DismissCreatedKey,
    RevokeApiKey(Uuid),
    ApiKeyRevoked(Result<(), CloudError>),
    RevokeSession(Uuid),
    SessionRevoked(Result<(), CloudError>),

    PrefFontSelected(String),
    PrefLocaleSelected(LocalePreference),
    PrefFontSizeChanged(String),
    PrefFontSizeSubmitted,
    PrefLineLengthChanged(String),
    PrefLineLengthSubmitted,
    PrefThemeSelected(String),
    PrefScrollbackChanged(String),
    PrefScrollbackSubmitted,
    PrefSeparatorChanged(String),
    PrefSeparatorSubmitted,
    PrefRawPrefixChanged(String),
    PrefRawPrefixSubmitted,
    PrefCommandInputBehaviorSelected(CommandInputBehavior),
    PrefHidePaneHeadersToggled(bool),
    PrefLoggingToggled(bool),
    PrefRawLoggingToggled(bool),
    PrefAdvancedScriptingToggled(bool),
    PrefAutoCheckForUpdatesToggled(bool),
    SystemFontsLoaded(Vec<String>),

    TweakTabSelected(TweakTab),
    /// A tweak slider moved mid-drag: local preview only, nothing emitted.
    TweakSliderChanged(TweakSlider, f32),
    /// The drag finished; commit (sessions restyle on release, not per tick).
    TweakSliderReleased,
    TweakSwatchPressed(&'static str),
    TweakPicker(color_picker::Message),
    TweakClearOverride,
    TweakResetSliders,
    TweakResetOverrides,

    /// A link inside the rendered license notices was clicked.
    OpenNoticesLink(markdown::Uri),

    Social(social_panel::Message),
}

#[derive(Debug, Clone)]
pub enum Event {
    /// A login or email verification minted a session.
    SessionEstablished(Box<AuthSession>),
    /// User signed out (optionally revoking every session server-side).
    SignOut { everywhere: bool },
    /// Fresh profile data (nickname change etc.).
    ProfileUpdated(Box<UserProfile>),
    /// Ask the account controller to re-probe `/me`.
    Poke,
    /// A preference committed in the Preferences tab. The daemon persists
    /// `settings.json` and propagates the change; this window only emits.
    SettingsChanged(Box<Settings>),
}

pub struct SettingsWindow {
    cloud: CloudHandles,
    tab: Tab,
    flow: AccountFlow,

    /// The email for the unified passwordless sign-in card.
    email: String,

    /// The one-time code field, shared by the signed-out verify and the
    /// signed-in-but-unverified verify.
    code_input: String,

    nickname_input: String,
    /// Whether the signed-in nickname editor is revealed (it is hidden behind a
    /// button by default so it isn't mistaken for a required step).
    editing_nickname: bool,
    sign_out_everywhere: bool,

    busy: Option<&'static str>,
    error: Option<String>,
    notice: Option<String>,

    api_keys: Option<Vec<ApiKeyInfo>>,
    sessions: Option<Vec<SessionInfo>>,
    created_key: Option<CreatedApiKey>,
    security_error: Option<String>,

    /// The live preferences model; every committed change is mirrored here
    /// and emitted whole as [`Event::SettingsChanged`].
    settings: Settings,
    /// Raw text for the numeric preference fields; validity is computed at
    /// render and only valid parses commit into [`Self::settings`].
    font_size_input: String,
    line_length_input: String,
    scrollback_input: String,
    separator_input: String,
    raw_prefix_input: String,
    /// Monospaced system font families, `None` until the first Preferences
    /// tab open kicks off enumeration.
    system_fonts: Option<Vec<String>>,

    /// Which tab of the theme-tweak panel is showing.
    tweak_tab: TweakTab,
    /// The open override picker on the Colors tab: the slot being edited and
    /// the picker's live state. Closed on theme switch.
    tweak_picker: Option<(&'static str, ColorPicker)>,

    social: SocialPanel,

    /// The parsed license notices, built on first Licenses-tab open. Parsing
    /// the ~600 KB Markdown document once (rather than on every `view`) keeps
    /// re-rendering the tab cheap.
    notices: Option<markdown::Content>,
}

impl SettingsWindow {
    pub fn new(cloud: CloudHandles) -> Self {
        let snapshot = cloud.snapshot.get();
        let nickname_input = snapshot
            .profile
            .as_ref()
            .and_then(|p| p.nickname.clone())
            .unwrap_or_default();
        let social = SocialPanel::new(cloud.clone());
        let settings = load_settings();
        let font_size_input = settings.terminal_font_size.to_string();
        let line_length_input = settings
            .terminal_line_length
            .map(|len| len.to_string())
            .unwrap_or_default();
        let scrollback_input = settings.scrollback_length.to_string();
        let separator_input = settings.command_separator.clone();
        let raw_prefix_input = settings.raw_line_prefix.clone();
        Self {
            cloud,
            tab: Tab::Account,
            flow: AccountFlow::SignIn,
            email: String::new(),
            code_input: String::new(),
            nickname_input,
            editing_nickname: false,
            sign_out_everywhere: false,
            busy: None,
            error: None,
            notice: None,
            api_keys: None,
            sessions: None,
            created_key: None,
            security_error: None,
            settings,
            font_size_input,
            line_length_input,
            scrollback_input,
            separator_input,
            raw_prefix_input,
            system_fonts: None,
            tweak_tab: TweakTab::Adjust,
            tweak_picker: None,
            social,
            notices: None,
        }
    }

    fn clear_feedback(&mut self) {
        self.error = None;
        self.notice = None;
    }

    /// Drop every per-account cache (security tab, social panel) so data
    /// loaded for one account can never be rendered under another. Called on
    /// sign-out and whenever a new session is established.
    fn reset_account_caches(&mut self) {
        // A different account may be signing in: never carry one account's
        // identity inputs into the next. Clear the typed email (so a prior
        // user's address isn't left prefilled in the sign-in card on a shared
        // device) and the nickname field, and collapse the editor.
        self.email.clear();
        self.nickname_input.clear();
        self.editing_nickname = false;
        self.api_keys = None;
        self.sessions = None;
        self.created_key = None;
        self.security_error = None;
        self.social = SocialPanel::new(self.cloud.clone());
    }

    /// Extracts the emailed one-time code from pasted input. Codes are
    /// numeric, so everything but ASCII digits (whitespace, dashes, stray
    /// punctuation from the email body) is dropped.
    fn extract_code(input: &str) -> String {
        input.chars().filter(char::is_ascii_digit).collect()
    }

    pub fn update(&mut self, message: Message) -> Update<Message, Event> {
        match message {
            Message::TabSelected(tab) => {
                self.tab = tab;
                if tab == Tab::Preferences && self.system_fonts.is_none() {
                    return Update::with_task(enumerate_system_fonts());
                }
                if tab == Tab::Security && self.api_keys.is_none() {
                    return self.refresh_security();
                }
                if tab == Tab::Friends
                    && self.cloud.snapshot.get().email_verified
                    && !self.social.is_loaded()
                {
                    return Update::with_task(self.social.refresh().map(Message::Social));
                }
                if tab == Tab::Licenses && self.notices.is_none() {
                    self.notices = Some(markdown::Content::parse(THIRD_PARTY_NOTICES));
                }
                Update::none()
            }
            Message::ShowFlow(flow) => {
                self.flow = flow;
                // A code is transient to one code-entry visit: don't carry it
                // across a Back / "I already have a code" navigation.
                self.code_input.clear();
                self.clear_feedback();
                Update::none()
            }

            // ===== unified passwordless sign-in card =====
            Message::EmailChanged(v) => {
                self.email = v;
                Update::none()
            }
            Message::CodeRequested => {
                if self.busy.is_some() {
                    return Update::none();
                }
                self.clear_feedback();
                let email = self.email.trim().to_string();
                if !email.contains('@') {
                    self.error = Some("Enter a valid email address.".to_string());
                    return Update::none();
                }
                self.busy = Some("Emailing you a code…");
                let client = self.cloud.client.clone();
                let target = email.clone();
                Update::with_task(Task::perform(
                    async move { client.login(&target).await },
                    Message::CodeRequestResult,
                ))
            }
            Message::CodeRequestResult(result) => {
                self.busy = None;
                match result {
                    Ok(()) => {
                        // Uniform 202 whether the email already had an account or
                        // was just created on first sight (enumeration
                        // resistance). The code-entry step renders from this state.
                        self.flow = AccountFlow::EnterCode {
                            email: self.email.trim().to_string(),
                        };
                        Update::none()
                    }
                    Err(err) => {
                        self.error = Some(display_error(&err));
                        Update::none()
                    }
                }
            }

            // ===== code entry (shared by first-time verify and returning login) =====
            Message::CodeChanged(v) => {
                self.code_input = v;
                Update::none()
            }
            Message::VerifySubmitted => {
                if self.busy.is_some() {
                    return Update::none();
                }
                self.clear_feedback();
                let code = Self::extract_code(&self.code_input);
                if code.is_empty() {
                    self.error = Some("Paste the code from the email.".to_string());
                    return Update::none();
                }
                let email = self.code_target_email();
                if email.is_empty() {
                    self.error = Some(
                        "Enter your email first so we know which account the code is for."
                            .to_string(),
                    );
                    return Update::none();
                }
                self.busy = Some("Verifying…");
                let client = self.cloud.client.clone();
                Update::with_task(Task::perform(
                    async move { client.verify_email(&email, &code).await },
                    Message::VerifyResult,
                ))
            }
            Message::VerifyResult(result) => {
                self.busy = None;
                match result {
                    Ok(session) => {
                        self.code_input.clear();
                        self.flow = AccountFlow::SignIn;
                        // Verification mints a fresh session, possibly for a
                        // different account than whatever was cached here.
                        self.reset_account_caches();
                        self.notice = Some(if session.needs_nickname {
                            "Signed in! Choose a nickname to claim your handle.".to_string()
                        } else {
                            "Signed in — you're all set.".to_string()
                        });
                        Update::with_event(Event::SessionEstablished(Box::new(session)))
                    }
                    Err(CloudError::NotFoundOrNoAccess) => {
                        // The server's uniform 404 also covers the attempt
                        // rate-limit, so the copy must not promise the code
                        // is merely mistyped.
                        self.error = Some(
                            "That code is invalid or expired — request a new one.".to_string(),
                        );
                        Update::none()
                    }
                    Err(err) => {
                        self.error = Some(display_error(&err));
                        Update::none()
                    }
                }
            }
            Message::ResendCode => {
                if self.busy.is_some() {
                    return Update::none();
                }
                self.clear_feedback();
                let email = self.code_target_email();
                if email.is_empty() {
                    self.error = Some(
                        "Enter your email in the form first so we know where to send it."
                            .to_string(),
                    );
                    return Update::none();
                }
                self.busy = Some("Sending…");
                let client = self.cloud.client.clone();
                Update::with_task(Task::perform(
                    async move { client.login(&email).await },
                    Message::ResendResult,
                ))
            }
            Message::ResendResult(result) => {
                self.busy = None;
                match result {
                    Ok(()) => {
                        self.notice = Some(
                            "If that address has an account, a fresh code is on its way \
                             (it replaces any earlier one)."
                                .to_string(),
                        );
                    }
                    Err(err) => self.error = Some(display_error(&err)),
                }
                Update::none()
            }

            // ===== signed-in profile =====
            Message::NicknameChanged(v) => {
                self.nickname_input = v;
                Update::none()
            }
            Message::NicknameSubmitted => {
                if self.busy.is_some() {
                    return Update::none();
                }
                self.clear_feedback();
                let nickname = self.nickname_input.trim().to_string();
                if let Some(problem) = nickname_problem(&nickname) {
                    self.error = Some(problem);
                    return Update::none();
                }
                self.busy = Some("Saving nickname…");
                let client = self.cloud.client.clone();
                Update::with_task(Task::perform(
                    async move { client.set_nickname(&nickname).await.map(Box::new) },
                    Message::NicknameResult,
                ))
            }
            Message::NicknameResult(result) => {
                self.busy = None;
                match result {
                    Ok(profile) => {
                        // Collapse the editor back behind its button on success.
                        self.editing_nickname = false;
                        self.notice = profile.nickname.clone().map(|h| format!("You are now {h}."));
                        Update::with_event(Event::ProfileUpdated(profile))
                    }
                    Err(err) => {
                        self.error = Some(display_error(&err));
                        Update::none()
                    }
                }
            }
            Message::EditNickname(editing) => {
                self.editing_nickname = editing;
                self.clear_feedback();
                if editing {
                    // Seed the editor with the current handle so a small tweak
                    // doesn't start from an empty field.
                    self.nickname_input = self
                        .cloud
                        .snapshot
                        .get()
                        .profile
                        .as_ref()
                        .and_then(|p| p.nickname.clone())
                        .unwrap_or_default();
                }
                Update::none()
            }

            Message::RefreshStatusPressed => Update::with_event(Event::Poke),
            Message::SignOutPressed => {
                let everywhere = self.sign_out_everywhere;
                self.sign_out_everywhere = false;
                self.reset_account_caches();
                self.notice = Some("Signed out.".to_string());
                Update::with_event(Event::SignOut { everywhere })
            }
            Message::SignOutEverywhereChanged(v) => {
                self.sign_out_everywhere = v;
                Update::none()
            }

            // ===== security tab =====
            Message::SecurityRefresh => self.refresh_security(),
            Message::ApiKeysLoaded(result) => {
                match result {
                    Ok(keys) => {
                        self.api_keys = Some(keys);
                        self.security_error = None;
                    }
                    Err(err) => self.security_error = Some(display_error(&err)),
                }
                Update::none()
            }
            Message::SessionsLoaded(result) => {
                match result {
                    Ok(sessions) => self.sessions = Some(sessions),
                    Err(err) => self.security_error = Some(display_error(&err)),
                }
                Update::none()
            }
            Message::CreateApiKeyPressed => {
                let client = self.cloud.client.clone();
                Update::with_task(Task::perform(
                    async move { client.create_api_key().await },
                    Message::ApiKeyCreated,
                ))
            }
            Message::ApiKeyCreated(result) => match result {
                Ok(created) => {
                    self.created_key = Some(created);
                    self.refresh_security()
                }
                Err(err) => {
                    self.security_error = Some(match err {
                        CloudError::Unauthorized(_) => {
                            "Creating API keys requires being logged in (not just an API key)."
                                .to_string()
                        }
                        other => display_error(&other),
                    });
                    Update::none()
                }
            },
            Message::DismissCreatedKey => {
                self.created_key = None;
                Update::none()
            }
            Message::RevokeApiKey(id) => {
                let client = self.cloud.client.clone();
                Update::with_task(Task::perform(
                    async move { client.delete_api_key(id).await },
                    Message::ApiKeyRevoked,
                ))
            }
            Message::ApiKeyRevoked(result) => {
                if let Err(err) = result {
                    self.security_error = Some(display_error(&err));
                }
                self.refresh_security()
            }
            Message::RevokeSession(id) => {
                let client = self.cloud.client.clone();
                Update::with_task(Task::perform(
                    async move { client.delete_session(id).await },
                    Message::SessionRevoked,
                ))
            }
            Message::SessionRevoked(result) => {
                if let Err(err) = result {
                    self.security_error = Some(display_error(&err));
                }
                self.refresh_security()
            }

            // ===== preferences tab =====
            // Committed changes mirror into `self.settings` and re-emit the
            // whole model; numeric buffers that don't parse commit nothing.
            Message::PrefFontSelected(family) => {
                self.settings.terminal_font_family = family;
                self.settings_changed()
            }
            Message::PrefLocaleSelected(locale) => {
                self.settings.locale = locale;
                smudgy_i18n::activate(locale);
                self.settings_changed()
            }
            // Typing only edits the buffer; commits happen on Enter so a
            // partially-typed value never runs the save/fan-out pipeline
            // (for scrollback that would destructively trim the buffer).
            Message::PrefFontSizeChanged(value) => {
                self.font_size_input = value;
                Update::none()
            }
            Message::PrefFontSizeSubmitted => {
                match self.font_size_input.trim().parse::<f32>() {
                    Ok(size) if (8.0..=40.0).contains(&size) => {
                        self.settings.terminal_font_size = size;
                        self.settings_changed()
                    }
                    _ => Update::none(),
                }
            }
            Message::PrefLineLengthChanged(value) => {
                self.line_length_input = value;
                Update::none()
            }
            Message::PrefLineLengthSubmitted => {
                if self.line_length_input.trim().is_empty() {
                    // Empty is a valid commit: wrap to the window width.
                    self.settings.terminal_line_length = None;
                    return self.settings_changed();
                }
                match self.line_length_input.trim().parse::<u16>() {
                    Ok(len) if (20..=1000).contains(&len) => {
                        self.settings.terminal_line_length = Some(len);
                        self.settings_changed()
                    }
                    _ => Update::none(),
                }
            }
            Message::PrefThemeSelected(name) => {
                self.settings.theme = name;
                // The panel now edits the new theme's own tweak entry; an
                // open picker would point at the old theme's slot.
                self.tweak_picker = None;
                self.settings_changed()
            }
            Message::PrefScrollbackChanged(value) => {
                self.scrollback_input = value;
                Update::none()
            }
            Message::PrefScrollbackSubmitted => {
                match self.scrollback_input.trim().parse::<usize>() {
                    Ok(lines) if (100..=10_000_000).contains(&lines) => {
                        self.settings.scrollback_length = lines;
                        self.settings_changed()
                    }
                    _ => Update::none(),
                }
            }
            // Unlike the numeric fields (whose commit is gated to Enter so a
            // half-typed value can't destructively trim the buffer), the
            // separator and prefix are short and free-form: commit on every
            // edit. The previous Enter-only path silently dropped a value that
            // was typed and then clicked away from — it never reached
            // settings.json, so the runtime kept loading the default.
            Message::PrefSeparatorChanged(value) => {
                // Separators are a handful of characters at most.
                self.separator_input = value.chars().take(4).collect();
                self.settings.command_separator = self.separator_input.clone();
                self.settings_changed()
            }
            Message::PrefSeparatorSubmitted => {
                self.settings.command_separator = self.separator_input.clone();
                self.settings_changed()
            }
            Message::PrefRawPrefixChanged(value) => {
                self.raw_prefix_input = value;
                self.settings.raw_line_prefix = self.raw_prefix_input.clone();
                self.settings_changed()
            }
            Message::PrefRawPrefixSubmitted => {
                self.settings.raw_line_prefix = self.raw_prefix_input.clone();
                self.settings_changed()
            }
            Message::PrefCommandInputBehaviorSelected(behavior) => {
                self.settings.command_input_behavior = behavior;
                self.settings_changed()
            }
            Message::PrefHidePaneHeadersToggled(hide) => {
                self.settings.hide_pane_headers = hide;
                self.settings_changed()
            }
            Message::PrefLoggingToggled(enabled) => {
                self.settings.logging.enabled = enabled;
                self.settings_changed()
            }
            Message::PrefRawLoggingToggled(enabled) => {
                self.settings.logging.log_raw = enabled;
                self.settings_changed()
            }
            Message::PrefAdvancedScriptingToggled(enabled) => {
                self.settings.advanced_scripting_features = enabled;
                self.settings_changed()
            }
            Message::PrefAutoCheckForUpdatesToggled(enabled) => {
                self.settings.auto_check_for_updates = enabled;
                // The user took explicit control of this preference, so drop the
                // installer seed; `settings.json` is authoritative from now on.
                clear_update_check_seed();
                self.settings_changed()
            }
            Message::SystemFontsLoaded(fonts) => {
                self.system_fonts = Some(fonts);
                Update::none()
            }

            // ===== theme tweak panel =====
            Message::TweakTabSelected(tab) => {
                self.tweak_tab = tab;
                Update::none()
            }
            Message::TweakSliderChanged(which, value) => {
                // Mid-drag: the local model (and thus the preview strip)
                // updates, but nothing is emitted until release.
                let entry = self.tweak_entry();
                match which {
                    TweakSlider::Background => entry.background = value,
                    TweakSlider::Brightness => entry.brightness = value,
                    TweakSlider::Contrast => entry.contrast = value,
                    TweakSlider::Saturation => entry.saturation = value,
                }
                Update::none()
            }
            Message::TweakSliderReleased => self.tweaks_changed(),
            Message::TweakResetSliders => {
                let entry = self.tweak_entry();
                entry.background = 0.0;
                entry.brightness = 0.0;
                entry.contrast = 0.0;
                entry.saturation = 0.0;
                self.tweaks_changed()
            }
            Message::TweakSwatchPressed(slot) => {
                if self
                    .tweak_picker
                    .as_ref()
                    .is_some_and(|(open, _)| *open == slot)
                {
                    self.tweak_picker = None;
                } else {
                    // Seed from the slot's current *effective* color, so the
                    // picker opens on what the user is looking at.
                    let color = prefs::slot_color(&self.effective_palette(), slot)
                        .unwrap_or(Color::WHITE);
                    self.tweak_picker = Some((slot, ColorPicker::from_color(color)));
                }
                Update::none()
            }
            Message::TweakPicker(message) => {
                let Some((slot, picker)) = &mut self.tweak_picker else {
                    return Update::none();
                };
                let slot = *slot;
                match picker.update(message) {
                    color_picker::Event::Preview => {
                        let hex = color_picker::to_hex(picker.color());
                        self.tweak_entry().overrides.insert(slot.to_string(), hex);
                        Update::none()
                    }
                    color_picker::Event::Committed(color) => {
                        let hex = color_picker::to_hex(color);
                        self.tweak_entry().overrides.insert(slot.to_string(), hex);
                        self.tweaks_changed()
                    }
                }
            }
            Message::TweakClearOverride => {
                let Some((slot, _)) = self.tweak_picker.take() else {
                    return Update::none();
                };
                let key = self.tweak_key().to_string();
                if let Some(entry) = self.settings.theme_tweaks.get_mut(&key) {
                    entry.overrides.remove(slot);
                }
                self.tweaks_changed()
            }
            Message::TweakResetOverrides => {
                self.tweak_picker = None;
                self.tweak_entry().overrides.clear();
                self.tweaks_changed()
            }

            // ===== licenses tab =====
            // The notices are reference text and ship no links, so a click has
            // nothing to open. This arm exists only because `markdown::view` is
            // generic over a link message.
            Message::OpenNoticesLink(_url) => Update::none(),

            // ===== friends tab =====
            Message::Social(message) => {
                Update::with_task(self.social.update(message).map(Message::Social))
            }
        }
    }

    /// Every committed preference change emits the full settings model; the
    /// daemon persists and propagates it (never saved from this window).
    fn settings_changed(&self) -> Update<Message, Event> {
        Update::with_event(Event::SettingsChanged(Box::new(self.settings.clone())))
    }

    /// The canonical key for the current theme's tweak entry. Tweaks are
    /// keyed by the resolved palette name (the same lookup `prefs` does), so
    /// case differences in `settings.theme` can't fork entries.
    fn tweak_key(&self) -> &'static str {
        prefs::palette_by_name(&self.settings.theme).name
    }

    /// The current theme's tweak entry, created on first edit.
    fn tweak_entry(&mut self) -> &mut ThemeTweaks {
        let key = self.tweak_key();
        self.settings.theme_tweaks.entry(key.to_string()).or_default()
    }

    /// The current theme's base palette with its tweak entry applied — what
    /// the swatches, picker seeds, and preview strip render from.
    fn effective_palette(&self) -> prefs::TerminalPalette {
        let base = prefs::palette_by_name(&self.settings.theme);
        self.settings
            .theme_tweaks
            .get(base.name)
            .map_or_else(|| base.clone(), |tweaks| prefs::apply_tweaks(base, tweaks))
    }

    /// Commits a tweak edit: drops the entry if it became neutral (keeps
    /// `settings.json` tidy), then emits the full settings model.
    fn tweaks_changed(&mut self) -> Update<Message, Event> {
        let key = self.tweak_key();
        if self
            .settings
            .theme_tweaks
            .get(key)
            .is_some_and(ThemeTweaks::is_neutral)
        {
            self.settings.theme_tweaks.remove(key);
        }
        self.settings_changed()
    }

    /// The email a code request or verify should target: the tracked
    /// code-entry flow wins; then the signed-in profile; then whatever is
    /// typed into the sign-in card.
    fn code_target_email(&self) -> String {
        if let AccountFlow::EnterCode { email } = &self.flow
            && !email.is_empty()
        {
            return email.clone();
        }
        let snapshot = self.cloud.snapshot.get();
        snapshot
            .profile
            .as_ref()
            .map(|p| p.email.clone())
            .or_else(|| {
                let typed = self.email.trim();
                if typed.is_empty() {
                    None
                } else {
                    Some(typed.to_string())
                }
            })
            .unwrap_or_default()
    }

    fn refresh_security(&mut self) -> Update<Message, Event> {
        let keys_client = self.cloud.client.clone();
        let sessions_client = self.cloud.client.clone();
        Update::with_task(Task::batch([
            Task::perform(
                async move { keys_client.api_keys().await },
                Message::ApiKeysLoaded,
            ),
            Task::perform(
                async move { sessions_client.sessions().await },
                Message::SessionsLoaded,
            ),
        ]))
    }

    // ===================== views =====================

    pub fn view(&self) -> ThemedElement<'_, Message> {
        let nav = column![
            nav_button("Account", self.tab == Tab::Account, Tab::Account),
            nav_button("Preferences", self.tab == Tab::Preferences, Tab::Preferences),
            nav_button("Security", self.tab == Tab::Security, Tab::Security),
            nav_button("Friends", self.tab == Tab::Friends, Tab::Friends),
            nav_button("Licenses", self.tab == Tab::Licenses, Tab::Licenses),
        ]
        .spacing(4)
        .width(140);

        let content: ThemedElement<'_, Message> = match self.tab {
            Tab::Account => self.account_view(),
            Tab::Preferences => self.preferences_view(),
            Tab::Security => self.security_view(),
            Tab::Friends => self.friends_view(),
            Tab::Licenses => self.licenses_view(),
        };

        row![
            container(nav).padding(12),
            rule::vertical(1),
            container(iced::widget::scrollable(container(content).padding(16)))
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .into()
    }

    fn feedback(&self) -> ThemedElement<'_, Message> {
        let mut col = column![].spacing(6);
        if let Some(busy) = self.busy {
            col = col.push(text(busy).size(13));
        }
        if let Some(error) = &self.error {
            col = col.push(text(error).size(13).style(theme::builtins::text::danger));
        }
        if let Some(notice) = &self.notice {
            col = col.push(text(notice).size(13).style(theme::builtins::text::success));
        }
        col.into()
    }

    fn account_view(&self) -> ThemedElement<'_, Message> {
        let snapshot = self.cloud.snapshot.get();
        let mut col = column![text("Account").size(20)].spacing(12);

        if snapshot.busy {
            col = col.push(text("Checking account status…").size(13));
        }

        if snapshot.signed_in {
            col = col.push(self.signed_in_view(&snapshot));
        } else {
            col = col.push(self.signed_out_view());
        }

        col.push(self.feedback()).into()
    }

    fn signed_in_view(
        &self,
        snapshot: &crate::cloud_account::AccountSnapshot,
    ) -> ThemedElement<'_, Message> {
        let mut col = column![].spacing(10);

        if let Some(profile) = &snapshot.profile {
            col = col.push(
                row![
                    text(profile.email.clone()).size(14),
                    space::horizontal(),
                    if snapshot.email_verified {
                        text("verified")
                            .size(12)
                            .style(theme::builtins::text::success)
                    } else {
                        text("not verified")
                            .size(12)
                            .style(theme::builtins::text::danger)
                    },
                ]
                .align_y(Alignment::Center)
                .spacing(8),
            );
            col = col.push(
                text(match profile.nickname.clone() {
                    Some(handle) => format!("Signed in as {handle}"),
                    None => "Signed in (no nickname yet)".to_string(),
                })
                .size(14),
            );
        } else {
            col = col.push(text("Signed in.").size(14));
        }

        if !snapshot.email_verified {
            // The verify affordance lives inline: a signed-in-but-unverified
            // account always has exactly one next step. "Email me a code"
            // goes through `POST /auth/login`, which mails a fresh code.
            col = col.push(
                container(
                    column![
                        text(
                            "Verify your email to use cloud features \
                             (friends, sharing, sync).",
                        )
                        .size(13),
                        text_input("code from the email", &self.code_input)
                            .on_input(Message::CodeChanged)
                            .on_submit(Message::VerifySubmitted)
                            .width(380),
                        row![
                            button(text("Verify").size(13))
                                .style(theme::builtins::button::primary)
                                .padding([4, 10])
                                .on_press(Message::VerifySubmitted),
                            button(text("Email me a code").size(13))
                                .style(theme::builtins::button::secondary)
                                .padding([4, 10])
                                .on_press(Message::ResendCode),
                            button(text("Re-check").size(13))
                                .style(theme::builtins::button::secondary)
                                .padding([4, 10])
                                .on_press(Message::RefreshStatusPressed),
                        ]
                        .spacing(8),
                    ]
                    .spacing(8),
                )
                .padding(10)
                .style(theme::builtins::container::modal_body),
            );
        }

        col = col.push(self.nickname_section(snapshot));

        col = col.push(
            row![
                button(text("Sign out").size(13))
                    .style(theme::builtins::button::secondary)
                    .padding([4, 10])
                    .on_press(Message::SignOutPressed),
                checkbox(self.sign_out_everywhere)
                    .label("also sign out everywhere (all devices)")
                    .on_toggle(Message::SignOutEverywhereChanged),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );

        col.into()
    }

    /// The nickname affordance in the signed-in view, in one of three states:
    ///
    /// - **needs a handle** (verified, none allocated yet): a prominent claim
    ///   form — this is a real next step, so it isn't hidden.
    /// - **editing**: the editor was revealed by the "Change nickname" button.
    /// - **collapsed** (has a handle, not editing): just that button, so the
    ///   editor's presence isn't mistaken for something that must be done.
    fn nickname_section(
        &self,
        snapshot: &crate::cloud_account::AccountSnapshot,
    ) -> ThemedElement<'_, Message> {
        const NICKNAME_PLACEHOLDER: &str = "nickname (3-24 letters, digits, - or _)";

        if snapshot.needs_nickname {
            return column![
                text("Choose your nickname").size(14),
                text("This is your public handle — others find and friend you by it.").size(12),
                row![
                    text_input(NICKNAME_PLACEHOLDER, &self.nickname_input)
                        .on_input(Message::NicknameChanged)
                        .on_submit(Message::NicknameSubmitted)
                        .width(280),
                    button(text("Claim handle").size(13))
                        .style(theme::builtins::button::primary)
                        .padding([4, 10])
                        .on_press(Message::NicknameSubmitted),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ]
            .spacing(6)
            .into();
        }

        if self.editing_nickname {
            return column![
                row![
                    text_input(NICKNAME_PLACEHOLDER, &self.nickname_input)
                        .on_input(Message::NicknameChanged)
                        .on_submit(Message::NicknameSubmitted)
                        .width(280),
                    button(text("Save").size(13))
                        .style(theme::builtins::button::primary)
                        .padding([4, 10])
                        .on_press(Message::NicknameSubmitted),
                    button(text("Cancel").size(13))
                        .style(theme::builtins::button::secondary)
                        .padding([4, 10])
                        .on_press(Message::EditNickname(false)),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                text("Changing your nickname changes how others find you.").size(12),
            ]
            .spacing(4)
            .into();
        }

        button(text("Change nickname").size(13))
            .style(theme::builtins::button::secondary)
            .padding([4, 10])
            .on_press(Message::EditNickname(true))
            .into()
    }

    fn signed_out_view(&self) -> ThemedElement<'_, Message> {
        match &self.flow {
            AccountFlow::SignIn => self.sign_in_card(),
            AccountFlow::EnterCode { email } => self.enter_code_card(email),
        }
    }

    /// The unified passwordless sign-in card: one email field. There is no
    /// password and no separate "create account" — the server creates the
    /// account on first sight, and a nickname (when needed) is chosen after
    /// sign-in.
    fn sign_in_card(&self) -> ThemedElement<'_, Message> {
        column![
            text("Sign in").size(15),
            text_input("email", &self.email)
                .on_input(Message::EmailChanged)
                .on_submit(Message::CodeRequested)
                .width(280),
            text(
                "We'll email you a one-time code — there is no password. New to \
                 smudgy? Just enter your email; your account is created automatically.",
            )
            .size(12),
            button(text("Email me a code").size(14))
                .style(theme::builtins::button::primary)
                .padding([6, 16])
                .on_press(Message::CodeRequested),
            button(text("I already have a code").size(12))
                .style(theme::builtins::button::link)
                .padding([2, 0])
                .on_press(Message::ShowFlow(AccountFlow::EnterCode {
                    email: self.email.trim().to_string(),
                })),
        ]
        .spacing(8)
        .into()
    }

    /// Rendered once an email submission mailed a code. The same card serves
    /// first-time verification and returning-device login.
    fn enter_code_card(&self, email: &str) -> ThemedElement<'_, Message> {
        let mut col = column![text("Check your email").size(15)].spacing(8);

        if email.is_empty() {
            // Reached via "I already have a code" without a tracked address:
            // ask for the email alongside the code.
            col = col.push(
                text_input("email", &self.email)
                    .on_input(Message::EmailChanged)
                    .on_submit(Message::VerifySubmitted)
                    .width(280),
            );
        } else {
            col = col.push(
                text(format!(
                    "We emailed a code to {email}. Paste it below — codes expire \
                     after 15 minutes.",
                ))
                .size(13),
            );
        }

        col = col
            .push(
                text_input("code from the email", &self.code_input)
                    .on_input(Message::CodeChanged)
                    .on_submit(Message::VerifySubmitted)
                    .width(280),
            )
            .push(
                row![
                    button(text("Sign in").size(14))
                        .style(theme::builtins::button::primary)
                        .padding([6, 16])
                        .on_press(Message::VerifySubmitted),
                    button(text("Resend code").size(13))
                        .style(theme::builtins::button::secondary)
                        .padding([6, 12])
                        .on_press(Message::ResendCode),
                ]
                .spacing(8),
            )
            .push(
                button(text("Back").size(12))
                    .style(theme::builtins::button::link)
                    .padding([2, 0])
                    .on_press(Message::ShowFlow(AccountFlow::SignIn)),
            );

        col.into()
    }

    /// The Preferences tab: appearance, input handling, and logging.
    ///
    /// Every control commits on change (no save button); committed changes
    /// emit [`Event::SettingsChanged`]. Numeric fields follow the map
    /// inspector's convention: raw text in a buffer, validity computed at
    /// render, only valid parses commit.
    fn preferences_view(&self) -> ThemedElement<'_, Message> {
        let font_size_valid = matches!(
            self.font_size_input.trim().parse::<f32>(),
            Ok(size) if (8.0..=40.0).contains(&size)
        );
        let line_length_valid = self.line_length_input.trim().is_empty()
            || matches!(
                self.line_length_input.trim().parse::<u16>(),
                Ok(len) if (20..=1000).contains(&len)
            );
        let scrollback_valid = matches!(
            self.scrollback_input.trim().parse::<usize>(),
            Ok(lines) if (100..=10_000_000).contains(&lines)
        );

        let mut col = column![text("Preferences").size(20)].spacing(12);

        col = col.push(
            column![
                dim_text_owned(smudgy_i18n::t!("language")),
                pick_list(
                    LocalePreference::ALL.to_vec(),
                    Some(self.settings.locale),
                    Message::PrefLocaleSelected,
                )
                .text_size(13)
                .width(280),
                dim_text_owned(smudgy_i18n::t!("language-description")),
            ]
            .spacing(2),
        );
        col = col.push(rule::horizontal(1));

        // ===== appearance =====
        col = col.push(text("Appearance").size(15));
        col = col.push(
            column![
                dim_text("Terminal font"),
                pick_list(
                    self.font_options(),
                    Some(self.settings.terminal_font_family.clone()),
                    Message::PrefFontSelected,
                )
                .text_size(13)
                .width(280),
            ]
            .spacing(2),
        );
        col = col.push(pref_input(
            "Font size",
            "16",
            &self.font_size_input,
            font_size_valid,
            Some("Press Enter to apply."),
            120.0,
            Message::PrefFontSizeChanged,
            Message::PrefFontSizeSubmitted,
        ));
        col = col.push(pref_input(
            "Line length",
            "wrap to window",
            &self.line_length_input,
            line_length_valid,
            Some(
                "Maximum characters per line before wrapping; empty wraps to the \
                 window width. Press Enter to apply.",
            ),
            120.0,
            Message::PrefLineLengthChanged,
            Message::PrefLineLengthSubmitted,
        ));
        col = col.push(
            column![
                dim_text("Theme"),
                pick_list(
                    self.theme_options(),
                    Some(self.settings.theme.clone()),
                    Message::PrefThemeSelected,
                )
                .text_size(13)
                .width(280),
            ]
            .spacing(2),
        );
        col = col.push(self.tweak_panel());
        col = col.push(pref_input(
            "Scrollback",
            "100000",
            &self.scrollback_input,
            scrollback_valid,
            Some("Lines kept per session. Press Enter to apply."),
            140.0,
            Message::PrefScrollbackChanged,
            Message::PrefScrollbackSubmitted,
        ));
        col = col.push(
            column![
                checkbox(self.settings.hide_pane_headers)
                    .label("Hide panel headers unless the main menu is active")
                    .on_toggle(Message::PrefHidePaneHeadersToggled),
                dim_text(
                    "Session and pane title bars show only while a window's toolbar is \
                     expanded (headers are also the drag handles for rearranging panes; \
                     dividers still resize either way). Scripts can pin a pane's header on.",
                ),
            ]
            .spacing(2),
        );

        col = col.push(rule::horizontal(1));

        // ===== input =====
        col = col.push(text("Input").size(15));
        col = col.push(pref_input(
            "Command separator",
            ";",
            &self.separator_input,
            true,
            Some("Splits one input line into multiple commands."),
            80.0,
            Message::PrefSeparatorChanged,
            Message::PrefSeparatorSubmitted,
        ));
        col = col.push(pref_input(
            "Raw line prefix",
            "\\",
            &self.raw_prefix_input,
            true,
            Some(
                "Lines starting with this are sent exactly as typed, minus the prefix. Bypasses aliases and the command separator above.",
            ),
            80.0,
            Message::PrefRawPrefixChanged,
            Message::PrefRawPrefixSubmitted,
        ));
        col = col.push(
            column![
                dim_text("Command input"),
                pick_list(
                    CommandInputBehavior::ALL.to_vec(),
                    Some(self.settings.command_input_behavior),
                    Message::PrefCommandInputBehaviorSelected,
                )
                .text_size(13)
                .width(320),
                dim_text(
                    "What happens to the input box and its text after you press Enter",
                ),
            ]
            .spacing(2),
        );

        col = col.push(rule::horizontal(1));

        // ===== logging =====
        col = col.push(text("Logging").size(15));
        col = col.push(
            checkbox(self.settings.logging.enabled)
                .label("Write session logs (plain text)")
                .on_toggle(Message::PrefLoggingToggled),
        );
        col = col.push(
            column![
                checkbox(self.settings.logging.log_raw)
                    .label("Also write raw logs (includes ANSI color codes)")
                    .on_toggle(Message::PrefRawLoggingToggled),
                dim_text("Raw logs start with the next connection."),
            ]
            .spacing(2),
        );

        col = col.push(rule::horizontal(1));

        // ===== advanced =====
        col = col.push(text("Advanced").size(15));
        col = col.push(
            column![
                checkbox(self.settings.advanced_scripting_features)
                    .label("Enable advanced scripting features")
                    .on_toggle(Message::PrefAdvancedScriptingToggled),
                dim_text(
                    "Unlocks \u{201c}Remove sandbox\u{201d} (run an installed package with full \
                     access, as if you wrote it) and the script inspector in the Automations \
                     window. Reopen Automations after changing this.",
                ),
            ]
            .spacing(2),
        );

        col = col.push(rule::horizontal(1));

        // ===== updates =====
        col = col.push(text("Updates").size(15));
        col = col.push(
                checkbox(self.settings.auto_check_for_updates)
                    .label("Automatically check for updates")
                    .on_toggle(Message::PrefAutoCheckForUpdatesToggled),
        );

        col.into()
    }

    /// Font picker options: bundled families first, then monospaced system
    /// fonts (minus duplicates) once enumerated, plus a fallback entry for
    /// the configured family when it isn't in either list (e.g. a font that
    /// was uninstalled) — the pick_list must always show the real selection.
    fn font_options(&self) -> Vec<String> {
        let mut options: Vec<String> = prefs::BUNDLED_FONT_FAMILIES
            .iter()
            .map(|family| (*family).to_string())
            .collect();
        if let Some(system) = &self.system_fonts {
            options.extend(
                system
                    .iter()
                    .filter(|name| !prefs::BUNDLED_FONT_FAMILIES.contains(&name.as_str()))
                    .cloned(),
            );
        }
        let current = &self.settings.terminal_font_family;
        if !current.is_empty() && !options.iter().any(|option| option == current) {
            options.push(current.clone());
        }
        options
    }

    /// Theme picker options, with the same unknown-selection fallback as
    /// [`Self::font_options`].
    fn theme_options(&self) -> Vec<String> {
        let mut options: Vec<String> = prefs::palettes()
            .iter()
            .map(|palette| palette.name.to_string())
            .collect();
        let current = &self.settings.theme;
        if !current.is_empty() && !options.iter().any(|option| option == current) {
            options.push(current.clone());
        }
        options
    }

    /// The theme-tweak panel under the Theme picker: an Adjust tab (sliders),
    /// a Colors tab (per-slot overrides), and a live preview strip rendered
    /// from the effective palette.
    fn tweak_panel(&self) -> ThemedElement<'_, Message> {
        let tweaks = self
            .settings
            .theme_tweaks
            .get(self.tweak_key())
            .cloned()
            .unwrap_or_default();
        let effective = self.effective_palette();

        let tabs = row![
            tweak_tab_button("Adjust", self.tweak_tab == TweakTab::Adjust, TweakTab::Adjust),
            tweak_tab_button("Colors", self.tweak_tab == TweakTab::Colors, TweakTab::Colors),
        ]
        .spacing(4);

        let body: ThemedElement<'_, Message> = match self.tweak_tab {
            TweakTab::Adjust => tweak_adjust_view(&tweaks),
            TweakTab::Colors => self.tweak_colors_view(&effective, &tweaks),
        };

        container(column![tabs, body, preview_strip(&effective)].spacing(10))
            .padding(10)
            .width(320)
            .style(theme::builtins::container::modal_body)
            .into()
    }

    /// The Colors tab: the effective palette as a swatch grid (roles, then
    /// ANSI 0-7 / 8-15), the inline override picker, and reset.
    fn tweak_colors_view(
        &self,
        effective: &prefs::TerminalPalette,
        tweaks: &ThemeTweaks,
    ) -> ThemedElement<'_, Message> {
        const ROLE_LABELS: [&str; 7] = ["bg", "fg", "input", "sel", "echo", "warn", "out"];

        let open_slot = self.tweak_picker.as_ref().map(|(slot, _)| *slot);
        let swatch = |slot: &'static str, label: String| -> ThemedElement<'static, Message> {
            let color = prefs::slot_color(effective, slot).unwrap_or(Color::WHITE);
            column![
                tweak_swatch(
                    slot,
                    color,
                    tweaks.overrides.contains_key(slot),
                    open_slot == Some(slot),
                ),
                text(label).size(9),
            ]
            .spacing(1)
            .align_x(Alignment::Center)
            .into()
        };

        let slots = prefs::override_slots();
        let mut roles = row![].spacing(4);
        for (slot, label) in slots.iter().copied().zip(ROLE_LABELS) {
            roles = roles.push(swatch(slot, label.to_string()));
        }
        let mut ansi_low = row![].spacing(4);
        let mut ansi_high = row![].spacing(4);
        for (index, slot) in slots.iter().copied().skip(7).enumerate() {
            let element = swatch(slot, index.to_string());
            if index < 8 {
                ansi_low = ansi_low.push(element);
            } else {
                ansi_high = ansi_high.push(element);
            }
        }

        let mut col = column![roles, ansi_low, ansi_high].spacing(6);

        if let Some((slot, picker)) = &self.tweak_picker {
            col = col.push(
                column![
                    row![
                        dim_text_owned(format!("Override: {slot}")),
                        space::horizontal(),
                        button(text("Clear override").size(11))
                            .style(theme::builtins::button::secondary)
                            .padding([2, 6])
                            .on_press(Message::TweakClearOverride),
                    ]
                    .align_y(Alignment::Center)
                    .spacing(8),
                    picker.view().map(Message::TweakPicker),
                ]
                .spacing(4),
            );
        }

        col = col.push(
            button(text("Reset all colors").size(12))
                .style(theme::builtins::button::secondary)
                .padding([3, 8])
                .on_press(Message::TweakResetOverrides),
        );

        col.into()
    }

    fn security_view(&self) -> ThemedElement<'_, Message> {
        let mut col = column![
            row![
                text("Security").size(20),
                space::horizontal(),
                button(text("Refresh").size(13))
                    .style(theme::builtins::button::secondary)
                    .padding([4, 10])
                    .on_press(Message::SecurityRefresh),
            ]
            .align_y(Alignment::Center)
        ]
        .spacing(12);

        if let Some(error) = &self.security_error {
            col = col.push(text(error).size(13).style(theme::builtins::text::danger));
        }

        // ===== API keys =====
        col = col.push(text("API keys").size(15));
        if let Some(created) = &self.created_key {
            col = col.push(
                container(
                    column![
                        text("New API key — copy it now, it will not be shown again:").size(13),
                        text_input("", &created.api_key).size(13),
                        button(text("Done — I copied it").size(13))
                            .style(theme::builtins::button::primary)
                            .padding([4, 10])
                            .on_press(Message::DismissCreatedKey),
                    ]
                    .spacing(8),
                )
                .padding(10)
                .style(theme::builtins::container::modal_body),
            );
        }
        match &self.api_keys {
            None => col = col.push(text("Loading…").size(13)),
            Some(keys) if keys.is_empty() => {
                col = col.push(text("No API keys.").size(13));
            }
            Some(keys) => {
                for key in keys {
                    col = col.push(
                        row![
                            text(format!(
                                "…{}",
                                key.key_suffix.as_deref().unwrap_or("????????")
                            ))
                            .size(13)
                            .width(120),
                            text(format!("created {}", key.created_at.format("%Y-%m-%d")))
                                .size(12),
                            text(match &key.last_used_at {
                                Some(at) => format!("last used {}", at.format("%Y-%m-%d")),
                                None => "never used".to_string(),
                            })
                            .size(12),
                            space::horizontal(),
                            button(text("Revoke").size(12))
                                .style(theme::builtins::button::secondary)
                                .padding([2, 8])
                                .on_press(Message::RevokeApiKey(key.id)),
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center),
                    );
                }
            }
        }
        col = col.push(
            button(text("Create API key").size(13))
                .style(theme::builtins::button::primary)
                .padding([4, 10])
                .on_press(Message::CreateApiKeyPressed),
        );
        col = col.push(
            text("Creating a key requires a logged-in session; keys are shown once at creation.")
                .size(11),
        );

        col = col.push(rule::horizontal(1));

        // ===== sessions =====
        col = col.push(text("Sessions").size(15));
        match &self.sessions {
            None => col = col.push(text("Loading…").size(13)),
            Some(sessions) if sessions.is_empty() => {
                col = col.push(text("No active sessions.").size(13));
            }
            Some(sessions) => {
                for session in sessions {
                    col = col.push(
                        row![
                            text(format!(
                                "created {}",
                                session.created_at.format("%Y-%m-%d %H:%M")
                            ))
                            .size(12),
                            text(format!("expires {}", session.expires_at.format("%Y-%m-%d")))
                                .size(12),
                            text(match &session.last_used_at {
                                Some(at) => format!("last used {}", at.format("%Y-%m-%d %H:%M")),
                                None => "unused".to_string(),
                            })
                            .size(12),
                            space::horizontal(),
                            button(text("Revoke").size(12))
                                .style(theme::builtins::button::secondary)
                                .padding([2, 8])
                                .on_press(Message::RevokeSession(session.id)),
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center),
                    );
                }
            }
        }
        col = col.push(text("Revoking the session you're currently using signs you out.").size(11));

        col.into()
    }

    /// The Friends tab: the verified-email gate, or the social panel.
    ///
    /// The gate lives here (not in the panel) so its call-to-action can be a
    /// window-level message that switches to the Account tab.
    fn friends_view(&self) -> ThemedElement<'_, Message> {
        let snapshot = self.cloud.snapshot.get();
        if !snapshot.email_verified || self.social.needs_email_verification() {
            return column![
                text("Friends").size(20),
                text("Verify your email to add friends and share maps").size(14),
                button(text("Go to Account").size(13))
                    .style(theme::builtins::button::primary)
                    .padding([6, 16])
                    .on_press(Message::TabSelected(Tab::Account)),
            ]
            .spacing(12)
            .into();
        }
        self.social.view().map(Message::Social)
    }

    /// The Licenses tab: the bundled third-party notices (font / icon / runtime
    /// attributions plus every linked Rust library) rendered as Markdown. The
    /// document is parsed once into `self.notices`; the surrounding page
    /// `scrollable` (see [`Self::view`]) supplies the scrollbar, and the
    /// Markdown's own `# Open Source Licenses` heading serves as the title.
    fn licenses_view(&self) -> ThemedElement<'_, Message> {
        let Some(content) = &self.notices else {
            return text("Loading…").size(13).into();
        };
        let settings = markdown::Settings::with_text_size(
            13.0,
            markdown::Style::from_palette(iced::theme::Palette::DARK),
        );
        markdown::view(content.items(), settings).map(Message::OpenNoticesLink)
    }
}

/// Enumerates monospaced system font families (deduped, sorted). Runs once,
/// kicked off the first time the Preferences tab opens; enumeration is tens
/// of milliseconds, so it rides an executor task rather than blocking a
/// frame.
fn enumerate_system_fonts() -> Task<Message> {
    Task::perform(
        async {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            let mut names: Vec<String> = db
                .faces()
                .filter(|face| face.monospaced)
                .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
                .collect();
            names.sort();
            names.dedup();
            names
        },
        Message::SystemFontsLoaded,
    )
}

/// De-emphasized text for field labels and helper lines under preference
/// controls (the map inspector's `field_label` convention, copied locally).
fn dim_text<'a>(label: &'static str) -> iced::widget::Text<'a, crate::Theme> {
    text(label).size(11).style(|theme: &crate::Theme| {
        iced::widget::text::Style {
            color: Some(theme.styles.text.normal.scale_alpha(0.6)),
        }
    })
}

/// A labeled text input for the Preferences tab: dimmed label above, the raw
/// buffer inside, an "invalid value" hint when it doesn't validate, and an
/// optional helper line below (the map inspector's `labeled_input`
/// convention, copied locally).
fn pref_input<'a>(
    label: &'static str,
    placeholder: &'static str,
    value: &str,
    valid: bool,
    helper: Option<&'static str>,
    width: f32,
    on_input: impl Fn(String) -> Message + 'a,
    on_submit: Message,
) -> ThemedElement<'a, Message> {
    let mut col = column![
        dim_text(label),
        text_input(placeholder, value)
            .size(14)
            .width(width)
            .on_input(on_input)
            .on_submit(on_submit),
    ]
    .spacing(2);

    if !valid {
        col = col.push(
            text("invalid value")
                .size(11)
                .style(theme::builtins::text::danger),
        );
    }
    if let Some(helper) = helper {
        col = col.push(dim_text(helper));
    }

    col.into()
}

/// [`dim_text`] for runtime strings (the open override's slot name).
fn dim_text_owned<'a>(label: String) -> iced::widget::Text<'a, crate::Theme> {
    text(label).size(11).style(|theme: &crate::Theme| {
        iced::widget::text::Style {
            color: Some(theme.styles.text.normal.scale_alpha(0.6)),
        }
    })
}

/// A tab selector for the tweak panel, styled like the window nav with a
/// selected state.
fn tweak_tab_button(
    label: &'static str,
    selected: bool,
    tab: TweakTab,
) -> ThemedElement<'static, Message> {
    button(text(label).size(12))
        .style(if selected {
            theme::builtins::button::list_item_selected
        } else {
            theme::builtins::button::list_item
        })
        .padding([3, 10])
        .on_press(Message::TweakTabSelected(tab))
        .into()
}

/// The Adjust tab: the four tweak sliders, their semantics hint, and reset.
fn tweak_adjust_view(tweaks: &ThemeTweaks) -> ThemedElement<'static, Message> {
    column![
        tweak_slider_row("Background", TweakSlider::Background, tweaks.background),
        tweak_slider_row("Brightness", TweakSlider::Brightness, tweaks.brightness),
        tweak_slider_row("Contrast", TweakSlider::Contrast, tweaks.contrast),
        tweak_slider_row("Saturation", TweakSlider::Saturation, tweaks.saturation),
        dim_text(
            "Background moves surfaces only; Contrast expands text away from the background.",
        ),
        button(text("Reset adjustments").size(12))
            .style(theme::builtins::button::secondary)
            .padding([3, 8])
            .on_press(Message::TweakResetSliders),
    ]
    .spacing(8)
    .into()
}

/// One labeled tweak slider. Dragging updates the local model only (the
/// preview strip follows live); release commits, so sessions re-bake their
/// scrollback once per gesture instead of per tick.
fn tweak_slider_row(
    label: &'static str,
    which: TweakSlider,
    value: f32,
) -> ThemedElement<'static, Message> {
    column![
        row![
            dim_text(label),
            space::horizontal(),
            text(format!("{value:+.2}")).size(11),
        ]
        .align_y(Alignment::Center),
        slider(-1.0..=1.0, value, move |value| {
            Message::TweakSliderChanged(which, value)
        })
        .step(0.01)
        .on_release(Message::TweakSliderReleased),
    ]
    .spacing(2)
    .into()
}

/// A ~24x24 clickable swatch filled with a slot's effective color. An accent
/// border marks an explicit override; the open slot gets a text-colored ring.
fn tweak_swatch(
    slot: &'static str,
    color: Color,
    overridden: bool,
    selected: bool,
) -> ThemedElement<'static, Message> {
    button(space::horizontal().width(0.0))
        .width(24.0)
        .height(24.0)
        .padding(0)
        .style(move |theme: &crate::Theme, _status| iced::widget::button::Style {
            background: Some(Background::Color(color)),
            border: if selected {
                iced::border::color(theme.styles.text.normal)
                    .width(2.0)
                    .rounded(3.0)
            } else if overridden {
                iced::border::color(theme.styles.general.accent)
                    .width(2.0)
                    .rounded(3.0)
            } else {
                iced::border::color(theme.styles.general.border)
                    .width(1.0)
                    .rounded(3.0)
            },
            ..Default::default()
        })
        .on_press(Message::TweakSwatchPressed(slot))
        .into()
}

/// A sample of the effective palette: foreground text over the background,
/// plus chips of all 16 ANSI colors. Follows slider drags live.
fn preview_strip(palette: &prefs::TerminalPalette) -> ThemedElement<'static, Message> {
    let fg = palette.foreground;
    let bg = palette.background;

    let mut chips = row![].spacing(2);
    for color in palette.ansi.iter().copied() {
        chips = chips.push(
            container(space::horizontal().width(0.0))
                .width(12.0)
                .height(12.0)
                .style(move |_theme: &crate::Theme| iced::widget::container::Style {
                    background: Some(Background::Color(color)),
                    ..Default::default()
                }),
        );
    }

    container(
        column![
            text("The quick brown fox").size(13).style(move |_theme: &crate::Theme| {
                iced::widget::text::Style { color: Some(fg) }
            }),
            chips,
        ]
        .spacing(6),
    )
    .padding(10)
    .width(Length::Fill)
    .style(move |theme: &crate::Theme| iced::widget::container::Style {
        background: Some(Background::Color(bg)),
        border: iced::border::color(theme.styles.general.border).width(1.0),
        ..Default::default()
    })
    .into()
}

fn nav_button(label: &'static str, selected: bool, tab: Tab) -> ThemedElement<'static, Message> {
    button(text(label).size(14))
        .style(if selected {
            theme::builtins::button::list_item_selected
        } else {
            theme::builtins::button::list_item
        })
        .width(Length::Fill)
        .padding([6, 10])
        .on_press(Message::TabSelected(tab))
        .into()
}

/// Local nickname validation mirroring the server's `valid_nickname` so an
/// obviously-bad handle is rejected before the round-trip. `None` means valid.
fn nickname_problem(nickname: &str) -> Option<String> {
    let nickname = nickname.trim();
    if nickname.is_empty() {
        return Some("Enter a nickname.".to_string());
    }
    let len = nickname.chars().count();
    if !(3..=24).contains(&len)
        || !nickname
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Some("Nickname must be 3-24 characters: letters, digits, '-' or '_'.".to_string());
    }
    None
}
