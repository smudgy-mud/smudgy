//! App-global cloud account state.
//!
//! One [`CloudAccount`] lives in the daemon. It owns the shared
//! [`CredentialSource`] (so logging in hot-upgrades every live mapper), the
//! [`CloudApiClient`], and a lock-free [`AccountSnapshot`] that every window
//! reads through a cheap [`AccountHandle`] clone.
//!
//! Accounts are the only credential: the mapper authenticates with the
//! logged-in session, and signing out leaves it credential-less (cached maps
//! keep working; the sync engine idles in a logged-out state).
//!
//! Auth flows themselves (login forms, token paste, …) live in the settings
//! window; it reports outcomes upward as events which the daemon feeds back
//! into this controller (`establish_session`, `sign_out`, …).

use std::sync::Arc;

use arc_swap::ArcSwap;
use iced::Task;
use smudgy_cloud::cloud_api::{AuthSession, CloudApiClient, SessionInfo, UserProfile};
use smudgy_cloud::{CloudError, Credential, CredentialSource};
use smudgy_core::models::auth::{
    self, AccountInfo, clear_session_token, load_session_token, save_session_token,
};
use smudgy_core::models::settings::{load_settings, set_dismissed_upgrade_version};

/// Read-only view of the account state, refreshed by the controller.
#[derive(Debug, Clone, Default)]
pub struct AccountSnapshot {
    /// Profile from `GET /me` (or the persisted copy from a prior run).
    pub profile: Option<UserProfile>,
    /// A session credential is active (user logged in).
    pub signed_in: bool,
    pub email_verified: bool,
    /// Verified but the requested nickname was already taken — user must pick another.
    pub needs_nickname: bool,
    /// Initial `/me` probe still in flight.
    pub busy: bool,
    /// Last bootstrap/refresh error worth surfacing (transport problems).
    pub last_error: Option<String>,
    /// The server rejected this build as too old (426). Drives the "out of
    /// date" banner (with its click-to-open download link).
    pub upgrade_required: bool,
    /// The newest client version the server advertised as a soft nudge and the
    /// user hasn't dismissed — drives the dismissable "upgrade available" popup.
    /// `None` when current, not signaled, or dismissed.
    pub upgrade_available: Option<String>,
}

impl AccountSnapshot {
    /// Whether the "verify your email to use cloud features" banner applies.
    #[must_use]
    pub fn show_verify_banner(&self) -> bool {
        self.signed_in && !self.email_verified
    }

    /// Whether the "smudgy is out of date — download an update" banner applies.
    #[must_use]
    pub fn show_upgrade_banner(&self) -> bool {
        self.upgrade_required
    }

    /// The version to advertise in the (dismissable) "upgrade available" popup,
    /// or `None` if it shouldn't show.
    #[must_use]
    pub fn upgrade_prompt(&self) -> Option<&str> {
        self.upgrade_available.as_deref()
    }

    // Convenience accessor for the account nickname, kept alongside the other
    // snapshot read helpers for the account/profile display surfaces.
    #[allow(dead_code)]
    #[must_use]
    pub fn nickname_text(&self) -> Option<String> {
        self.profile.as_ref().and_then(|p| p.nickname.clone())
    }
}

/// Cheap clonable read handle on the snapshot.
#[derive(Clone)]
pub struct AccountHandle(Arc<ArcSwap<AccountSnapshot>>);

impl AccountHandle {
    #[must_use]
    pub fn get(&self) -> Arc<AccountSnapshot> {
        self.0.load_full()
    }
}

/// Everything a window needs to talk to the cloud, cheap to clone.
#[derive(Clone)]
pub struct CloudHandles {
    pub snapshot: AccountHandle,
    pub credentials: CredentialSource,
    pub client: CloudApiClient,
    pub base_url: Arc<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    /// `/me` result for the credential generation it was issued under.
    ProfileLoaded(u64, Result<UserProfile, CloudError>),
    /// `POST /auth/refresh` result, tagged with the credential generation it
    /// was issued under (the launch + ~24h keep-alive). The session token is
    /// unchanged on success, so there is nothing to persist.
    SessionRefreshed(u64, Result<SessionInfo, CloudError>),
    /// Unauthenticated `GET /health` update-check result. Works signed out, so
    /// it carries no credential generation. `Ok` means the build is in range
    /// (a behind-but-allowed build had its newest version captured into
    /// [`CloudApiClient::upgrade_available`]); a `426` arrives as an
    /// [`CloudError`] whose [`CloudError::is_upgrade_required`] is set.
    UpdateCheckCompleted(Result<(), CloudError>),
}

pub struct CloudAccount {
    credentials: CredentialSource,
    client: CloudApiClient,
    snapshot: Arc<ArcSwap<AccountSnapshot>>,
    base_url: Arc<String>,
    /// Soft "upgrade available" prompt dismissed for this session ("Dismiss").
    upgrade_dismissed_session: bool,
    /// Version the prompt was permanently dismissed for ("Dismiss for this
    /// version"); mirrors `settings.dismissed_upgrade_version`.
    dismissed_upgrade_version: Option<String>,
    /// Master switch for automatic update checks; mirrors
    /// `settings.auto_check_for_updates`. When off, the launch-time check is
    /// skipped and the soft "upgrade available" prompt stays suppressed, so a
    /// cloud-averse user sees no update nudges at all.
    auto_check_for_updates: bool,
}

impl CloudAccount {
    /// Loads persisted state (settings.json for the base URL, the secure
    /// session token, account.json) and kicks off the silent re-auth probe.
    pub fn new() -> (Self, Task<Message>) {
        let settings = load_settings();
        let base_url = Arc::new(settings.base_url().to_string());

        let stored_session = load_session_token();
        let signed_in = stored_session.is_some();

        let credentials = CredentialSource::new(stored_session.map(Credential::Session));
        let client = CloudApiClient::new(base_url.as_str(), credentials.clone());

        let cached_account = auth::load_account();
        let snapshot = AccountSnapshot {
            profile: None,
            signed_in,
            email_verified: cached_account.as_ref().is_some_and(|a| a.email_verified),
            needs_nickname: cached_account.as_ref().is_some_and(|a| a.needs_nickname),
            busy: signed_in,
            last_error: None,
            upgrade_required: false,
            upgrade_available: None,
        };

        let account = Self {
            credentials,
            client,
            snapshot: Arc::new(ArcSwap::from_pointee(snapshot)),
            base_url,
            upgrade_dismissed_session: false,
            dismissed_upgrade_version: settings.dismissed_upgrade_version.clone(),
            auto_check_for_updates: settings.auto_check_for_updates,
        };

        // On launch: slide the session deadline forward (so an install opened
        // within the year never lapses) and re-probe the profile. Both are
        // tagged with the credential generation, so a stale reply can't clobber
        // a login that lands while they're in flight.
        let task = if signed_in {
            Task::batch([account.refresh_session(), account.refresh_profile()])
        } else {
            Task::none()
        };

        (account, task)
    }

    #[must_use]
    pub fn handles(&self) -> CloudHandles {
        CloudHandles {
            snapshot: AccountHandle(self.snapshot.clone()),
            credentials: self.credentials.clone(),
            client: self.client.clone(),
            base_url: self.base_url.clone(),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> Arc<AccountSnapshot> {
        self.snapshot.load_full()
    }

    /// Whether automatic update checks are enabled — the master switch for the
    /// launch-time and periodic checks and the soft upgrade prompt.
    #[must_use]
    pub fn auto_check_for_updates(&self) -> bool {
        self.auto_check_for_updates
    }

    fn mutate(&self, f: impl FnOnce(&mut AccountSnapshot)) {
        let mut next = (*self.snapshot.load_full()).clone();
        f(&mut next);
        self.snapshot.store(Arc::new(next));
    }

    /// Fire a `/me` probe tagged with the current credential generation so a
    /// stale response can't clobber a newer login.
    fn refresh_profile(&self) -> Task<Message> {
        let client = self.client.clone();
        let generation = self.credentials.generation();
        Task::perform(async move { client.me().await }, move |result| {
            Message::ProfileLoaded(generation, result)
        })
    }

    /// Slide the active session's idle deadline forward (`POST /auth/refresh`).
    /// This is the keep-alive driven by launch and the ~24h timer: an
    /// actively-running client refreshes long before the 365-day deadline, so
    /// it is never logged out for inactivity. No-op when signed out. The token
    /// is unchanged on success, so nothing is persisted; the result is tagged
    /// with the credential generation to ignore stale replies.
    pub fn refresh_session(&self) -> Task<Message> {
        if self.credentials.get().is_none() {
            return Task::none();
        }
        let client = self.client.clone();
        let generation = self.credentials.generation();
        Task::perform(async move { client.refresh().await }, move |result| {
            Message::SessionRefreshed(generation, result)
        })
    }

    /// Poll the unauthenticated `GET /health` to check for a newer client
    /// build. Works signed out — this is the only smudgy.org request a
    /// cloud-averse user makes, and only while [`Self::auto_check_for_updates`]
    /// is on (the caller gates on the same flag). The result lands as
    /// [`Message::UpdateCheckCompleted`] and feeds the existing upgrade prompts.
    pub fn check_for_updates(&self) -> Task<Message> {
        let client = self.client.clone();
        Task::perform(
            async move { client.check_for_updates().await },
            Message::UpdateCheckCompleted,
        )
    }

    /// Adopt the latest `auto_check_for_updates` preference (the in-app toggle
    /// or the installer seed). Flipping it off immediately clears any soft
    /// "upgrade available" prompt; flipping it on re-evaluates from the last
    /// observed server signal.
    pub fn set_auto_check_for_updates(&mut self, enabled: bool) {
        self.auto_check_for_updates = enabled;
        self.recompute_upgrade_prompt();
    }

    /// Record that the server rejected this build as too old (426). Drives the
    /// dismissable "out of date" banner, whose link opens the download page only
    /// when the user clicks it (there is no autonomous auto-open). It is neither
    /// an auth nor a transient error, so it gets its own arm ahead of the
    /// auth/offline handling. A release-candidate build suppresses the banner
    /// outright — see the early return below.
    fn mark_upgrade_required(&self) -> Task<Message> {
        if smudgy_core::models::settings::is_release_candidate() {
            // A release candidate ships *ahead* of the version it is a candidate
            // for, so by semver its version sits below that release — a prod 426
            // would be expected, and telling the tester to "download a newer
            // version" is wrong (they are deliberately on a pre-release). Swallow
            // the banner; the cloud call still failed, but no nag is raised. (The
            // common case never reaches here: an RC's base version sits at or
            // above the prod floor, which is the previous release.)
            log::warn!(
                "release-candidate build got a 426 from the cloud; suppressing the out-of-date banner"
            );
            self.mutate(|s| s.busy = false);
            return Task::none();
        }
        log::warn!("cloud rejected this client as out of date; surfacing upgrade prompt");
        self.mutate(|s| {
            s.upgrade_required = true;
            s.busy = false;
        });
        Task::none()
    }

    /// Re-evaluate the soft "upgrade available" prompt from the client's last
    /// observed `x-smudgy-upgrade-available` signal, honoring the session and
    /// per-version dismissals, and publish the result to the snapshot.
    fn recompute_upgrade_prompt(&self) {
        let advertised = self.client.upgrade_available();
        // A release candidate never nags about an upgrade: it is itself a
        // pre-release of an upcoming version, so a prod `x-smudgy-upgrade-available`
        // pointing at that very release is noise. Suppress regardless of the
        // dismissal/auto-check state.
        let show = !smudgy_core::models::settings::is_release_candidate()
            && self.auto_check_for_updates
            && advertised.as_deref().is_some_and(|version| {
                !self.upgrade_dismissed_session
                    && self.dismissed_upgrade_version.as_deref() != Some(version)
            });
        self.mutate(|s| s.upgrade_available = if show { advertised } else { None });
    }

    /// "Dismiss": hide the upgrade prompt for the rest of this session.
    pub fn dismiss_upgrade(&mut self) {
        self.upgrade_dismissed_session = true;
        self.recompute_upgrade_prompt();
    }

    /// "Dismiss for this version": persist the dismissal so the prompt stays
    /// hidden until a newer version is advertised.
    pub fn dismiss_upgrade_for_version(&mut self) {
        if let Some(version) = self.client.upgrade_available() {
            if let Err(e) = set_dismissed_upgrade_version(&version) {
                log::warn!("failed to persist dismissed upgrade version: {e}");
            }
            self.dismissed_upgrade_version = Some(version);
        }
        self.upgrade_dismissed_session = true;
        self.recompute_upgrade_prompt();
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ProfileLoaded(generation, result) => {
                if generation != self.credentials.generation() {
                    // Credential changed while the probe was in flight.
                    return Task::none();
                }
                match result {
                    Ok(profile) => {
                        self.absorb_profile(profile);
                        self.recompute_upgrade_prompt();
                        Task::none()
                    }
                    Err(err) if err.is_upgrade_required() => self.mark_upgrade_required(),
                    Err(err) if err.is_auth_error() => {
                        log::info!("stored session rejected; signing out locally");
                        let _ = clear_session_token();
                        self.credentials.set(None);
                        self.mutate(|s| {
                            s.signed_in = false;
                            s.busy = false;
                        });
                        Task::none()
                    }
                    Err(err) => {
                        // Offline or server trouble: keep the cached identity,
                        // stop spinning, note the error.
                        log::warn!("cloud profile probe failed: {err}");
                        self.mutate(|s| {
                            s.busy = false;
                            s.last_error = Some(err.to_string());
                        });
                        Task::none()
                    }
                }
            }
            Message::SessionRefreshed(generation, result) => {
                if generation != self.credentials.generation() {
                    // Credential changed while the refresh was in flight.
                    return Task::none();
                }
                match result {
                    Ok(_) => {
                        // Slid forward server-side; the token is unchanged, so
                        // there's nothing to persist or update locally.
                        log::debug!("cloud session refreshed");
                        self.recompute_upgrade_prompt();
                        Task::none()
                    }
                    Err(err) if err.is_upgrade_required() => self.mark_upgrade_required(),
                    Err(err) if err.is_auth_error() => {
                        // Session expired (past the 365-day idle window) or was
                        // revoked elsewhere: drop it locally, mirroring the
                        // failed `/me` probe path.
                        log::info!("session refresh rejected; signing out locally");
                        let _ = clear_session_token();
                        self.credentials.set(None);
                        self.mutate(|s| {
                            s.signed_in = false;
                            s.busy = false;
                        });
                        Task::none()
                    }
                    Err(err) => {
                        // Offline / transient: keep the session and retry on the
                        // next launch or timer tick — the 365-day window easily
                        // absorbs missed refreshes.
                        log::warn!("cloud session refresh failed: {err}");
                        Task::none()
                    }
                }
            }
            Message::UpdateCheckCompleted(result) => match result {
                Ok(()) => {
                    // In range. A behind-but-allowed build had its newest
                    // version captured into the client; surface the soft prompt.
                    self.recompute_upgrade_prompt();
                    Task::none()
                }
                Err(err) if err.is_upgrade_required() => self.mark_upgrade_required(),
                Err(err) => {
                    // Offline or server trouble: leave the prompts untouched and
                    // retry on the next launch.
                    log::warn!("cloud update check failed: {err}");
                    Task::none()
                }
            },
        }
    }

    /// A login / email-verification just minted a session: persist it, swap
    /// credentials (hot-upgrading every mapper), and update the snapshot.
    pub fn establish_session(&mut self, session: AuthSession) -> Task<Message> {
        if let Err(err) = save_session_token(&session.session_token) {
            log::warn!("failed to persist session token: {err}");
        }
        self.credentials
            .set(Some(Credential::Session(session.session_token.clone())));
        let needs_nickname = session.needs_nickname;
        self.mutate(|s| {
            s.signed_in = true;
            s.busy = false;
            s.needs_nickname = needs_nickname;
            s.last_error = None;
        });
        self.absorb_profile(session.user);
        Task::none()
    }

    /// Profile data arrived (login, `/me`, nickname change…): cache it.
    pub fn absorb_profile(&mut self, profile: UserProfile) {
        let info = AccountInfo {
            user_id: Some(profile.id),
            email: profile.email.clone(),
            nickname: profile.nickname.clone(),
            email_verified: profile.email_verified_at.is_some(),
            needs_nickname: profile.email_verified_at.is_some() && profile.nickname.is_none(),
        };
        if let Err(err) = auth::save_account(&info) {
            log::warn!("failed to persist account info: {err}");
        }
        self.mutate(|s| {
            s.email_verified = profile.email_verified_at.is_some();
            s.needs_nickname = profile.email_verified_at.is_some() && profile.nickname.is_none();
            s.profile = Some(profile);
            s.busy = false;
            s.last_error = None;
        });
    }

    /// Sign out locally (and best-effort on the server). `everywhere` revokes
    /// every session on the account, not just this one.
    pub fn sign_out(&mut self, everywhere: bool) -> Task<Message> {
        // The revocation future only runs after this `update` returns, i.e.
        // *after* we swap the shared credential out below. Snapshot the
        // current (session) credential into a detached client so the server
        // call still authenticates as the session being revoked.
        let revoke_client = CloudApiClient::new(
            self.base_url.as_str(),
            CredentialSource::new(self.credentials.get()),
        );
        let server_task = Task::future(async move {
            if everywhere && let Ok(sessions) = revoke_client.sessions().await {
                for session in sessions {
                    let _ = revoke_client.delete_session(session.id).await;
                }
            }
            let _ = revoke_client.logout().await;
        })
        .discard();

        if let Err(err) = clear_session_token() {
            log::warn!("failed to clear stored session token: {err}");
        }
        if let Err(err) = auth::clear_account() {
            log::warn!("failed to clear account info: {err}");
        }
        self.credentials.set(None);
        self.mutate(|s| *s = AccountSnapshot::default());

        server_task
    }

    /// Re-probe `/me` (e.g. after the user says "I verified my email").
    pub fn poke(&self) -> Task<Message> {
        if self.credentials.get().is_some() {
            self.mutate(|s| s.busy = true);
            self.refresh_profile()
        } else {
            Task::none()
        }
    }
}
