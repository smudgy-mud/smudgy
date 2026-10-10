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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

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

/// Whose deletion the user asked for: the account (its user id, when known)
/// and the credential generation it was asked under. A deletion is
/// outstanding only for that account while that credential is in use:
/// signing out or in ends it, and an answer to `DELETE /me` that arrives
/// after either says nothing of the account signed in now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeletionKey {
    pub user_id: Option<smudgy_cloud::Uuid>,
    pub generation: u64,
}

impl DeletionKey {
    /// Whether this deletion is of the account in use: asked under
    /// credential generation `generation`, for account `user` when both are
    /// known.
    #[must_use]
    pub fn applies(self, generation: u64, user: Option<smudgy_cloud::Uuid>) -> bool {
        self.generation == generation
            && match (self.user_id, user) {
                (Some(asked), Some(user)) => asked == user,
                _ => true,
            }
    }
}

/// What an answer that the account a deletion names is gone does here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeletionAnswer {
    /// The account and credential in use: sign out and forget the account.
    SignOut,
    /// Signed out since: forget what this computer keeps for that account.
    Forget(smudgy_cloud::Uuid),
    /// Another credential is in use, or the account is unknown.
    Ignore,
}

/// What an account-gone answer for `key` does, under credential
/// `generation`, with `user` signed in as far as known, and whether a
/// credential is in use.
fn deletion_answer(
    key: DeletionKey,
    generation: u64,
    user: Option<smudgy_cloud::Uuid>,
    credential_in_use: bool,
) -> DeletionAnswer {
    if key.applies(generation, user) {
        return DeletionAnswer::SignOut;
    }
    match key.user_id {
        Some(asked) if !credential_in_use => DeletionAnswer::Forget(asked),
        _ => DeletionAnswer::Ignore,
    }
}

/// Removes, off the UI thread, the map cache and queued writes this
/// computer keeps for `user_id`.
fn forget_on_disk(user_id: smudgy_cloud::Uuid) {
    match smudgy_core::get_smudgy_home() {
        Ok(home) => {
            std::thread::spawn(move || {
                crate::account_deletion::forget_account_on_disk(&home, user_id);
            });
        }
        Err(err) => log::warn!("cannot find the cloud map cache to clear: {err}"),
    }
}

/// Everything a window needs to talk to the cloud, cheap to clone.
#[derive(Clone)]
pub struct CloudHandles {
    pub snapshot: AccountHandle,
    pub credentials: CredentialSource,
    pub client: CloudApiClient,
    pub base_url: Arc<String>,
    /// App-lifetime serialization for mutations of one local package. A task keeps its permit
    /// even if the Automations window that launched it is closed or replaced.
    pub(crate) package_operations: PackageOperationGate,
}

impl CloudHandles {
    /// The key a deletion asked for now carries: the account signed in and
    /// the credential generation in use.
    #[must_use]
    pub fn deletion_key(&self) -> DeletionKey {
        DeletionKey {
            user_id: self
                .snapshot
                .get()
                .profile
                .as_ref()
                .map(|profile| profile.id),
            generation: self.credentials.generation(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PackageOperationId(u64);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PackageOperationKey {
    server_name: String,
    package_name: String,
}

impl PackageOperationKey {
    fn new(server_name: &str, package_name: &str) -> Self {
        Self {
            // Server and package folders live on case-insensitive filesystems on the primary
            // platform. Treat spelling-only variants as one mutation domain on every platform.
            server_name: server_name.to_lowercase(),
            package_name: package_name.to_lowercase(),
        }
    }
}

#[derive(Debug)]
struct PackageOperationGateInner {
    next_id: AtomicU64,
    in_flight: Mutex<HashMap<PackageOperationKey, PackageOperationId>>,
}

/// A shared, non-blocking per-package operation gate. Callers never wait while holding the UI
/// thread: they either reserve the package immediately or explain that another change is active.
#[derive(Debug, Clone)]
pub(crate) struct PackageOperationGate(Arc<PackageOperationGateInner>);

impl Default for PackageOperationGate {
    fn default() -> Self {
        Self(Arc::new(PackageOperationGateInner {
            next_id: AtomicU64::new(1),
            in_flight: Mutex::new(HashMap::new()),
        }))
    }
}

impl PackageOperationGate {
    fn in_flight(&self) -> MutexGuard<'_, HashMap<PackageOperationKey, PackageOperationId>> {
        // A panic in unrelated task code must not permanently disable package management. The
        // map remains structurally valid because mutations happen while the guard is held.
        self.0
            .in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub(crate) fn try_acquire(
        &self,
        server_name: &str,
        package_name: &str,
    ) -> Option<PackageOperationPermit> {
        let key = PackageOperationKey::new(server_name, package_name);
        let mut in_flight = self.in_flight();
        if in_flight.contains_key(&key) {
            return None;
        }
        let id = PackageOperationId(self.0.next_id.fetch_add(1, Ordering::Relaxed));
        in_flight.insert(key.clone(), id);
        drop(in_flight);
        Some(PackageOperationPermit {
            gate: self.clone(),
            key,
            id,
        })
    }

    #[must_use]
    pub(crate) fn is_busy(&self, server_name: &str, package_name: &str) -> bool {
        self.in_flight()
            .contains_key(&PackageOperationKey::new(server_name, package_name))
    }
}

/// RAII reservation held by the complete mutation future. Its operation id separately fences the
/// UI completion because a new operation can start after this permit is released but before an
/// older completion message is processed.
#[derive(Debug)]
pub(crate) struct PackageOperationPermit {
    gate: PackageOperationGate,
    key: PackageOperationKey,
    id: PackageOperationId,
}

impl PackageOperationPermit {
    #[must_use]
    pub(crate) fn id(&self) -> PackageOperationId {
        self.id
    }

    /// Transfer this live reservation into the cloneable completion message. The gate remains
    /// closed until the daemon/window accepts that message, while dropping every message clone
    /// still releases it automatically.
    pub(crate) fn into_completion(self) -> PackageOperationCompletion {
        let id = self.id;
        PackageOperationCompletion {
            id,
            permit: Arc::new(Mutex::new(Some(self))),
        }
    }
}

impl Drop for PackageOperationPermit {
    fn drop(&mut self) {
        let mut in_flight = self.gate.in_flight();
        if in_flight.get(&self.key) == Some(&self.id) {
            in_flight.remove(&self.key);
        }
    }
}

/// Cloneable ownership handoff from a completed task to its UI message. Explicit `release` closes
/// the small future/message scheduling gap in which a replacement window could otherwise start a
/// conflicting operation before the old completion reconciles its durable result.
#[derive(Clone)]
pub(crate) struct PackageOperationCompletion {
    id: PackageOperationId,
    permit: Arc<Mutex<Option<PackageOperationPermit>>>,
}

impl std::fmt::Debug for PackageOperationCompletion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PackageOperationCompletion")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl PackageOperationCompletion {
    #[must_use]
    pub(crate) fn id(&self) -> PackageOperationId {
        self.id
    }

    pub(crate) fn release(&self) {
        self.take_permit();
    }

    pub(crate) fn take_permit(&self) -> Option<PackageOperationPermit> {
        self.permit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

/// Handles pointing at nothing: window unit tests exercise layout and
/// lifecycle logic that never touches the cloud, but constructing a window
/// requires the handle bundle.
#[cfg(test)]
pub(crate) fn test_handles() -> CloudHandles {
    let credentials = CredentialSource::new(None);
    CloudHandles {
        snapshot: AccountHandle(Arc::new(ArcSwap::from_pointee(AccountSnapshot::default()))),
        credentials: credentials.clone(),
        client: CloudApiClient::new("http://localhost", credentials),
        base_url: Arc::new("http://localhost".to_string()),
        package_operations: PackageOperationGate::default(),
    }
}

#[cfg(test)]
pub(crate) fn test_handles_signed_in(nickname: &str) -> CloudHandles {
    let credentials =
        CredentialSource::new(Some(Credential::Session("smudgy_sess_test".to_string())));
    let created_at = "2026-01-01T00:00:00Z".parse().unwrap();
    let snapshot = AccountSnapshot {
        profile: Some(UserProfile {
            id: "00000000-0000-0000-0000-000000000000".parse().unwrap(),
            email: "test@example.invalid".to_string(),
            nickname: Some(nickname.to_string()),
            requested_nickname: None,
            email_verified_at: Some(created_at),
            nickname_updated_at: None,
            created_at,
        }),
        signed_in: true,
        email_verified: true,
        ..AccountSnapshot::default()
    };
    CloudHandles {
        snapshot: AccountHandle(Arc::new(ArcSwap::from_pointee(snapshot))),
        credentials: credentials.clone(),
        client: CloudApiClient::new("http://localhost", credentials),
        base_url: Arc::new("http://localhost".to_string()),
        package_operations: PackageOperationGate::default(),
    }
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
    /// Some client sharing the account's credential (map sync, the map
    /// editor, packages, account calls) met a 401.
    CredentialRefused,
}

/// The shared credential source, as the identity of the subscription that
/// watches it for refusals. There is one account, so every value is the
/// same subscription.
struct RefusalWatch(CredentialSource);

impl std::hash::Hash for RefusalWatch {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        "credential-refusals".hash(state);
    }
}

/// Whether a refusal ends the session here: only while the user's deletion
/// of the account in use is outstanding (`deleting`), since an account being
/// deleted refuses everything but `DELETE /me`, and only of the credential
/// still in use. Other refusals (an expired or revoked session) keep their
/// path: the next `/me` probe or session refresh signs out.
fn refusal_ends_session(deleting: bool, credentials: &CredentialSource) -> bool {
    deleting && credentials.current_was_refused()
}

pub struct CloudAccount {
    credentials: CredentialSource,
    client: CloudApiClient,
    snapshot: Arc<ArcSwap<AccountSnapshot>>,
    base_url: Arc<String>,
    package_operations: PackageOperationGate,
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
    /// Mirrors `account.json`'s `deletion_requested`: the user asked to delete
    /// the account and nothing since has shown it still works. Keyed to that
    /// account and credential, so it never carries to another.
    deletion: Option<DeletionKey>,
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
        // A deletion asked for under the stored session; `account.json` is
        // cleared on sign-out, so without a session there is none.
        let deletion = cached_account
            .as_ref()
            .filter(|account| signed_in && account.deletion_requested)
            .map(|account| DeletionKey {
                user_id: account.user_id,
                generation: credentials.generation(),
            });
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
            package_operations: PackageOperationGate::default(),
            upgrade_dismissed_session: false,
            dismissed_upgrade_version: settings.dismissed_upgrade_version.clone(),
            auto_check_for_updates: settings.auto_check_for_updates,
            deletion,
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
            package_operations: self.package_operations.clone(),
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
    /// auth/offline handling. A prod-like prerelease build suppresses the
    /// banner outright — see the early return below.
    fn mark_upgrade_required(&self) -> Task<Message> {
        if smudgy_core::models::settings::is_preview_build() {
            // A preview build deliberately sits below its eventual release by
            // semver, so telling its tester to download the current release is
            // wrong. The cloud call still failed, but no nag is raised.
            log::warn!(
                "preview build got a 426 from the cloud; suppressing the out-of-date banner"
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
        // A preview never nags about an upgrade: a prod
        // `x-smudgy-upgrade-available` pointing at its eventual release is
        // noise. Suppress regardless of the dismissal/auto-check state.
        let show = !smudgy_core::models::settings::is_preview_build()
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
                        // An account being deleted answers nothing but
                        // `DELETE /me`: this one works.
                        self.deletion = None;
                        self.absorb_profile(profile);
                        self.recompute_upgrade_prompt();
                        Task::none()
                    }
                    Err(err) if err.is_upgrade_required() => self.mark_upgrade_required(),
                    Err(err) if err.is_auth_error() => {
                        log::info!("stored session rejected; signing out locally");
                        self.drop_rejected_session();
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
                        // there's nothing to persist or update locally. An
                        // account being deleted could not refresh, so a
                        // requested deletion never started.
                        log::debug!("cloud session refreshed");
                        self.clear_deletion();
                        self.recompute_upgrade_prompt();
                        Task::none()
                    }
                    Err(err) if err.is_upgrade_required() => self.mark_upgrade_required(),
                    Err(err) if err.is_auth_error() => {
                        // Session expired (past the 365-day idle window) or was
                        // revoked elsewhere: drop it locally, mirroring the
                        // failed `/me` probe path.
                        log::info!("session refresh rejected; signing out locally");
                        self.drop_rejected_session();
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
            Message::CredentialRefused => {
                if refusal_ends_session(self.deleting(), &self.credentials) {
                    // The deletion took: sign out and forget the account, as a
                    // refused `/me` does.
                    log::info!("the account being deleted was refused; signing out locally");
                    self.drop_rejected_session();
                }
                Task::none()
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
        // A new session (perhaps another account's) has no deletion pending.
        self.deletion = None;
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

    /// Profile data arrived (login, `/me`, nickname change…): cache it. A
    /// deletion asked for another account or credential is dropped, never
    /// written into this account's record.
    pub fn absorb_profile(&mut self, profile: UserProfile) {
        let generation = self.credentials.generation();
        if self
            .deletion
            .is_some_and(|key| !key.applies(generation, Some(profile.id)))
        {
            self.deletion = None;
        }
        let info = AccountInfo {
            user_id: Some(profile.id),
            email: profile.email.clone(),
            nickname: profile.nickname.clone(),
            email_verified: profile.email_verified_at.is_some(),
            needs_nickname: profile.email_verified_at.is_some() && profile.nickname.is_none(),
            deletion_requested: self.deletion.is_some(),
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
        self.deletion = None;
        self.mutate(|s| *s = AccountSnapshot::default());

        server_task
    }

    /// The stored session was refused (401): drop it locally. When the user
    /// had asked to delete the account, the refusal means the account is gone
    /// or being deleted, so what this computer keeps for it goes too.
    fn drop_rejected_session(&mut self) {
        let deleting = self.deleting();
        let _ = clear_session_token();
        self.credentials.set(None);
        if deleting {
            self.forget_deleted_account();
        } else {
            self.deletion = None;
        }
        self.mutate(|s| {
            s.signed_in = false;
            s.busy = false;
        });
    }

    /// The account signed in now, as far as known.
    fn current_user(&self) -> Option<smudgy_cloud::Uuid> {
        self.snapshot
            .load()
            .profile
            .as_ref()
            .map(|profile| profile.id)
    }

    /// Whether `key` names the account and credential in use.
    fn is_current(&self, key: DeletionKey) -> bool {
        key.applies(self.credentials.generation(), self.current_user())
    }

    /// Whether a deletion of the account in use is outstanding.
    fn deleting(&self) -> bool {
        self.deletion.is_some_and(|key| self.is_current(key))
    }

    /// The user asked to delete the account `key` names. Recorded (and
    /// persisted, so a refusal after a restart is still understood) only
    /// while that account and credential are in use.
    pub fn deletion_requested(&mut self, key: DeletionKey) {
        if !self.is_current(key) {
            log::info!("a deletion asked for an account no longer in use; ignoring it");
            return;
        }
        self.deletion = Some(key);
        self.persist_deletion(true);
    }

    /// The deletion `key` names did not happen: the account still works.
    pub fn deletion_refused(&mut self, key: DeletionKey) {
        if self.is_current(key) {
            self.clear_deletion();
        }
    }

    /// No deletion is outstanding.
    fn clear_deletion(&mut self) {
        if self.deletion.take().is_some() {
            self.persist_deletion(false);
        }
    }

    fn persist_deletion(&self, requested: bool) {
        if let Some(mut info) = auth::load_account() {
            info.deletion_requested = requested;
            if let Err(err) = auth::save_account(&info) {
                log::warn!("failed to persist the account deletion request: {err}");
            }
        }
    }

    /// The account `key` names is gone (or being deleted, which the server
    /// finishes on its own): sign out locally without the server round trip
    /// a sign-out makes, and remove what this computer keeps for the
    /// account's cloud maps. Signed out since (the deletion's own refusal
    /// signs out), what this computer keeps for the account `key` names
    /// still goes. An answer arriving while another credential is in use
    /// changes nothing.
    pub fn account_deleted(&mut self, key: DeletionKey) {
        match deletion_answer(
            key,
            self.credentials.generation(),
            self.current_user(),
            self.credentials.get().is_some(),
        ) {
            DeletionAnswer::SignOut => {
                let _ = clear_session_token();
                self.credentials.set(None);
                self.forget_deleted_account();
                self.mutate(|s| *s = AccountSnapshot::default());
            }
            DeletionAnswer::Forget(user_id) => {
                if self
                    .deletion
                    .is_some_and(|asked| asked.user_id.is_none_or(|asked| asked == user_id))
                {
                    self.deletion = None;
                }
                if auth::load_account().and_then(|info| info.user_id) == Some(user_id)
                    && let Err(err) = auth::clear_account()
                {
                    log::warn!("failed to clear account info: {err}");
                }
                forget_on_disk(user_id);
                if self.current_user() == Some(user_id) {
                    self.mutate(|s| *s = AccountSnapshot::default());
                }
            }
            DeletionAnswer::Ignore => {
                log::info!("a deletion answer for an account no longer in use; ignoring it");
            }
        }
    }

    /// Clears `account.json` and, off the UI thread, the deleted account's map
    /// cache and queued writes.
    fn forget_deleted_account(&mut self) {
        let user_id = self
            .snapshot
            .load()
            .profile
            .as_ref()
            .map(|profile| profile.id)
            .or_else(|| auth::load_account().and_then(|info| info.user_id));
        self.deletion = None;
        if let Err(err) = auth::clear_account() {
            log::warn!("failed to clear account info: {err}");
        }
        if let Some(user_id) = user_id {
            forget_on_disk(user_id);
        }
    }

    /// Each refusal of the account's credential, whichever client met it.
    pub fn refusals(&self) -> iced::Subscription<Message> {
        iced::Subscription::run_with(RefusalWatch(self.credentials.clone()), |watch| {
            futures::stream::unfold(watch.0.refusals(), |mut refusals| async move {
                refusals.changed().await.ok()?;
                Some((Message::CredentialRefused, refusals))
            })
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_refusal_during_a_deletion_ends_the_session() {
        let credentials =
            CredentialSource::new(Some(Credential::Session("smudgy_sess_mira".to_string())));
        assert!(
            !refusal_ends_session(true, &credentials),
            "nothing refused yet"
        );
        credentials.note_refused(&Credential::Session("smudgy_sess_mira".to_string()));
        assert!(refusal_ends_session(true, &credentials));
        assert!(
            !refusal_ends_session(false, &credentials),
            "an ordinary refusal keeps its path"
        );
        credentials.set(Some(Credential::Session("smudgy_sess_new".to_string())));
        assert!(
            !refusal_ends_session(true, &credentials),
            "a refusal of an earlier credential says nothing of this one"
        );
    }

    /// An account-gone answer signs out the account and credential in use;
    /// signed out since, it still forgets the account it names, by that
    /// account's id; while another credential is in use it does nothing.
    #[test]
    fn an_account_gone_answer_forgets_the_account_it_names() {
        let mira = smudgy_cloud::Uuid::from_u128(1);
        let iris = smudgy_cloud::Uuid::from_u128(2);
        let key = DeletionKey {
            user_id: Some(mira),
            generation: 3,
        };
        assert_eq!(
            deletion_answer(key, 3, Some(mira), true),
            DeletionAnswer::SignOut
        );
        assert_eq!(
            deletion_answer(key, 4, Some(mira), false),
            DeletionAnswer::Forget(mira),
            "signed out since, with the profile still remembered"
        );
        assert_eq!(
            deletion_answer(key, 4, None, false),
            DeletionAnswer::Forget(mira),
            "signed out since"
        );
        assert_eq!(
            deletion_answer(key, 4, Some(iris), true),
            DeletionAnswer::Ignore,
            "another account signed in since"
        );
        assert_eq!(
            deletion_answer(key, 4, Some(mira), true),
            DeletionAnswer::Ignore,
            "the same account signed in again under a new credential"
        );
        let unknown = DeletionKey {
            user_id: None,
            generation: 3,
        };
        assert_eq!(
            deletion_answer(unknown, 4, None, false),
            DeletionAnswer::Ignore
        );
    }

    #[test]
    fn package_operation_gate_is_shared_case_insensitive_and_raii() {
        let handles = test_handles();
        let replacement_window_handles = handles.clone();
        let first = handles
            .package_operations
            .try_acquire("Example Server", "Mapper")
            .expect("first operation reserves the package");
        let first_id = first.id();

        assert!(
            replacement_window_handles
                .package_operations
                .is_busy("example server", "mapper")
        );
        assert!(
            replacement_window_handles
                .package_operations
                .try_acquire("EXAMPLE SERVER", "MAPPER")
                .is_none()
        );
        let other = replacement_window_handles
            .package_operations
            .try_acquire("Example Server", "Other")
            .expect("a different package has an independent gate");
        drop(other);

        let completion = first.into_completion();
        assert!(
            replacement_window_handles
                .package_operations
                .is_busy("Example Server", "Mapper")
        );
        let completion_clone = completion.clone();
        completion.release();
        assert!(completion_clone.take_permit().is_none());
        let next = replacement_window_handles
            .package_operations
            .try_acquire("Example Server", "Mapper")
            .expect("dropping the permit releases the package");
        assert_ne!(next.id(), first_id);
    }
}
