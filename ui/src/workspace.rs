//! Durable workspace state: the versioned on-disk template for windows,
//! session slots, and pane arrangement.
//!
//! The app never restores at launch — startup is always the clean
//! no-active-sessions view. Instead each server keeps a LAST-SESSION
//! snapshot at `<home>/<server>/last-session.json`: whenever the autosave
//! pipeline settles a snapshot, the server owning the active session gets
//! its window footprint captured and written, so every server retains the
//! most recent arrangement in which it was active. The connect surface
//! automatically restores the chosen profile's panes when its first session
//! opens in a fresh window. The explicit "Restore last session" action restores
//! all saved profiles, driving the same flow named layouts use.
//! A snapshot is a *template*, not a dump of the
//! live model: it stores stable descriptors (server/profile names, pane
//! namespace + folded name) and structural facts (split trees, tab order,
//! selection), never runtime ids — `SessionId`, `PaneKey`,
//! `TabId`, `GroupId`, and `window::Id` all restart from scratch each run
//! and must never reach disk.
//!
//! [`dto`] defines the schema; [`file`] is the never-fatal template reader
//! named layouts degrade through; [`last_session`] locates and reads the
//! per-server snapshots (side-effect-free — autosave owns those files);
//! [`snapshot`] builds the DTO from the live model; [`autosave`] owns the
//! dirty/debounce/checkpoint scheduling and the runtime↔durable identity
//! maps; [`writer`] is the single-writer worker and the process-global
//! snapshot cell the forced-shutdown hook flushes from.
//!
//! [`binding`] is the pure engine mapping live sessions onto template
//! slots, shared by user-initiated restore, runtime vacancy adoption, and
//! named-layout application; [`restore`] turns a loaded template into
//! window plans, placeholders, and replay bookkeeping.
//!
//! Named layouts reuse the same template format verbatim: [`layouts`] is
//! the per-server store (`<server>/layouts/<name>.json`, explicit save
//! only), and [`apply`] is the pure projection that turns a stored
//! template plus the live workspace into a validated mutation plan.

pub mod apply;
pub mod autosave;
pub mod binding;
pub mod dto;
pub mod file;
pub mod last_session;
pub mod layouts;
pub mod preferences;
pub mod restore;
pub mod snapshot;
pub mod writer;

/// Which stored template a user-initiated restore reads: a named layout in
/// the acting server's `layouts/` store, or that server's last-session
/// snapshot. Both drive the identical apply flow — plan, keep-or-close
/// questions, spawns, revalidation — differing only in where the template
/// bytes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateSource {
    Named(String),
    LastSession,
}

impl std::fmt::Display for TemplateSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Named(name) => write!(f, "layout '{name}'"),
            Self::LastSession => write!(f, "the last session"),
        }
    }
}
