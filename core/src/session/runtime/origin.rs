//! Provenance of a created automation: which script/module/package called
//! `createAlias`/`createTrigger`. Used to key the trigger [`Manager`](super::trigger)'s
//! alias/trigger namespaces so a package managing its own `heal` alias never collides with
//! the user's, and so re-creating an automation upserts within its own namespace.
//!
//! Runtime-only — never persisted. Hand-authored (disk) and inline-script automations are
//! [`Origin::User`]; ESM module/package creations carry the importing module's identity,
//! which the `smudgy:core` virtual module bakes in. The op layer parses the descriptor once
//! per module (`op_smudgy_interop_resolve_creator` interns it, strictly) and the creation
//! ops receive the interned id.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use serde::Deserialize;

/// Who created an automation — the keying namespace in the trigger [`Manager`](super::trigger).
/// `User` is the global-by-name namespace; `Module`/`Package` give each creator
/// its own namespace (so `disableAlias` and idempotent re-creation stay scoped, and a
/// package's automation can coexist with a same-named user one).
///
/// Both non-`User` variants hold their payload behind an `Arc`: an origin is cloned into
/// every queued action and every namespace key, and the shared handle keeps that a refcount
/// bump rather than one-to-three string allocations.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    /// Hand-authored on disk, or any inline alias/trigger script: the global namespace.
    User,
    /// A local `modules/` file, keyed by its `modules/`-relative subpath (e.g. `combat/healer.ts`).
    Module(Arc<str>),
    /// An installed `smudgy://owner/name` package at a concrete resolved version. All of a
    /// package's modules share this namespace; two coexisting versions are distinct.
    Package(Arc<PackageOrigin>),
}

/// The `owner`/`name`/`version` coordinates of a [`Origin::Package`], shared behind its `Arc`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PackageOrigin {
    pub owner: String,
    pub name: String,
    pub version: String,
}

/// Which V8 isolate an automation/script/function lives in. `Main` is the trusted shared
/// isolate (user scripts, local `modules/`, and — today — every package); a `Package`
/// keys a sandboxed install by its *root* package. Runtime-only, like [`Origin`], and
/// pointer-sized: the package coordinates live behind one `Arc`, so cloning into every
/// action/registry key is a single refcount bump.
///
/// This is threaded through the trigger [`Manager`](super::trigger) keys (`(IsolateId, Origin,
/// name)`), the per-isolate function/script registries, and the
/// [`RuntimeAction`](super::RuntimeAction) routing. The `Package` variant keys a sandboxed
/// install by its root package; see `script/PACKAGE-ISOLATES.md` for the isolate-set model.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum IsolateId {
    /// The trusted, shared, allow-all isolate.
    Main,
    /// A sandboxed install, keyed by the root package that owns the isolate.
    Package(Arc<PackageIsolate>),
}

/// The resolved coordinates of a sandboxed package isolate. Shared behind the `Arc` in
/// [`IsolateId::Package`] so an isolate id stays one word wide wherever it is carried.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PackageIsolate {
    pub owner: Arc<str>,
    pub name: Arc<str>,
    pub version: Arc<str>,
}

/// The instance part of a widget token that no live isolate ever carries (instances are
/// allocated from 1). [`IsolateId::from_widget_token`] returns it for a malformed token, so
/// the dispatch-time instance check drops the callback instead of running it anywhere.
pub const NO_ISOLATE_INSTANCE: u64 = 0;

impl IsolateId {
    /// The isolate of the package installed at `owner/name` at `version`.
    #[must_use]
    pub fn package(
        owner: impl Into<Arc<str>>,
        name: impl Into<Arc<str>>,
        version: impl Into<Arc<str>>,
    ) -> Self {
        Self::Package(Arc::new(PackageIsolate {
            owner: owner.into(),
            name: name.into(),
            version: version.into(),
        }))
    }

    /// Encode for the leaf `smudgy_widgets` crate, which cannot name this type. Round-tripped
    /// through [`smudgy_cloud::WidgetIsolate`] so a widget callback can be dispatched back to
    /// its creating isolate. The token is `<instance>\u{1f}<role>`: `instance` names the exact
    /// isolate *instantiation* (an engine reload rebuilds every isolate under the same role,
    /// and a callback minted before the rebuild holds a `v8::Global` bound to the disposed
    /// heap — the instance mismatch is what lets dispatch drop it instead of touching v8),
    /// while `role` is the stable `IsolateId`. `\u{1f}` (ASCII unit separator) cannot occur in
    /// validated package coords, so it is an unambiguous field delimiter.
    #[must_use]
    pub fn to_widget_token(&self, instance: u64) -> String {
        match self {
            IsolateId::Main => format!("{instance}\u{1f}main"),
            IsolateId::Package(pkg) => format!(
                "{instance}\u{1f}pkg\u{1f}{}\u{1f}{}\u{1f}{}",
                pkg.owner, pkg.name, pkg.version
            ),
        }
    }

    /// Inverse of [`Self::to_widget_token`]: the isolate role plus the instance nonce of the
    /// instantiation that minted the callback. An unknown/malformed token yields
    /// `(Main, NO_ISOLATE_INSTANCE)` — inert by construction, since no live isolate has
    /// instance [`NO_ISOLATE_INSTANCE`], so dispatch drops the callback rather than running a
    /// foreign v8 handle in the trusted isolate.
    #[must_use]
    pub fn from_widget_token(token: &str) -> (Self, u64) {
        let (instance, role) = token.split_once('\u{1f}').unwrap_or(("", token));
        let instance = instance.parse::<u64>().unwrap_or(NO_ISOLATE_INSTANCE);
        let mut parts = role.split('\u{1f}');
        let id = match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some("pkg"), Some(owner), Some(name), Some(version)) => {
                IsolateId::package(owner, name, version)
            }
            _ => IsolateId::Main,
        };
        (id, instance)
    }
}

/// Whether a script-created automation is an alias, a trigger, or a hotkey (the tree nests each
/// under its kind beneath the creator). `Hotkey` is keyed for parity — script-created hotkeys
/// share the `(IsolateId, Origin, name)` keying so `createHotkey`/`delete()` are origin-scoped like
/// aliases/triggers — but hotkeys are NOT tracked in the trigger [`Manager`]'s introspection mirror
/// (they live in the dispatch's own `HotkeyId` map), so the `aliases`/`triggers` registry maps
/// below carry only `Alias`/`Trigger`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AutomationKind {
    Alias,
    Trigger,
    Hotkey,
}

/// The version- and isolate-independent identity a `singleton` automation reserves
/// session-wide (see `PACKAGE-ISOLATES.md`). A `Package` deliberately **drops its resolved
/// version** (so `mapper@1` and `mapper@2` share one slot) and the isolate dimension (so
/// copies in different isolates collapse to one); `User`/`Module` origins have neither a
/// version nor cross-isolate fan-out, so their singleton key is identical to their normal
/// per-origin namespace key (nothing to drop). Note this only equates the *identity scope* —
/// the *operation* still differs: a non-singleton re-create upserts the definition in place,
/// whereas a singleton re-create no-ops under first-writer-wins, discarding the new body.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SingletonOrigin {
    User,
    Module { subpath: String },
    Package { owner: String, name: String },
}

/// One reserved `singleton` automation identity — `(origin-sans-version, kind, name)`. The
/// first creation op to insert this into the session-global [`SingletonRegistry`] wins; a
/// later create of the same identity, in any isolate at any version, no-ops and reports
/// `created == false` (see `PACKAGE-ISOLATES.md`, first-writer-wins).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SingletonKey {
    pub origin: SingletonOrigin,
    pub kind: AutomationKind,
    pub name: Arc<str>,
}

/// Session-global reservations for `singleton` automations, shared (the same `Rc`) into
/// every isolate's ops — legal because all isolates live on the one session thread
/// (see `PACKAGE-ISOLATES.md`). A fresh set is built per [`ScriptEngine`](super::script_engine),
/// so a session reload clears every reservation.
///
/// The winning isolate is retained so a sandbox discarded after `load_modules` returns an
/// error can release exactly the seats it acquired while its queued registrations are purged.
/// It must not release a seat an earlier successful isolate won under the same
/// version-independent key. Top-level errors reported later by the event-loop pump do not
/// discard the isolate and therefore do not use this cleanup.
///
/// **Known gap (normal delete/teardown).** Deleting a successfully registered singleton or
/// tearing down its live isolate does not free its key, so re-creating it within the same
/// engine generation still no-ops. Current install/uninstall/trust/update lifecycle changes
/// rebuild the engine and therefore the registry.
#[derive(Debug, Default)]
pub struct SingletonReservations {
    owners: HashMap<SingletonKey, IsolateId>,
}

impl SingletonReservations {
    /// First-writer-wins reservation. Records the winner for failed-load cleanup.
    pub(crate) fn reserve(&mut self, key: SingletonKey, isolate: IsolateId) -> bool {
        match self.owners.entry(key) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(isolate);
                true
            }
            std::collections::hash_map::Entry::Occupied(_) => false,
        }
    }

    #[must_use]
    pub(crate) fn contains(&self, key: &SingletonKey) -> bool {
        self.owners.contains_key(key)
    }

    /// Release only reservations actually won by a failed isolate.
    pub(crate) fn release_isolate(&mut self, isolate: &IsolateId) {
        self.owners.retain(|_, owner| owner != isolate);
    }
}

pub type SingletonRegistry = Rc<RefCell<SingletonReservations>>;

/// A script-created automation's provenance + display state, sent to the UI so it can be
/// shown nested under its creating module/package. Carries only what the tree needs.
///
/// This UI-facing projection intentionally omits the [`IsolateId`] that the trigger
/// [`Manager`] key carries: while only [`IsolateId::Main`] exists no two summaries can share
/// `(origin, kind, name)`, so the UI keys by `Origin` alone. Threading the isolate here is a
/// cross-layer change into the automations window that matters once coexistence produces
/// same-`(origin, name)` automations in different isolates; see `PACKAGE-ISOLATES.md`.
#[derive(Clone, Debug)]
pub struct AutomationSummary {
    pub kind: AutomationKind,
    pub origin: Origin,
    pub name: String,
    pub enabled: bool,
    /// The automation's match pattern(s), joined for read-only display in the automations
    /// window (regex sources for match/raw patterns). Empty when it has none. Display-only.
    pub pattern: Arc<str>,
    /// What the automation does, for read-only display. Carried only for the script-created
    /// (`Module`/`Package`) origins that are actually streamed — never the user/disk set.
    pub body: AutomationBody,
    /// The name of the trigger this one is inside, so the tree can nest it. `None` for a
    /// top-level automation and for every alias.
    pub outer: Option<String>,
}

/// The body of a script-created automation, captured once at creation for the read-only
/// detail pane. Never executed from here — purely what the automations window renders.
#[derive(Clone, Debug)]
pub enum AutomationBody {
    /// Plaintext command(s) sent to the MUD verbatim.
    Command(Arc<str>),
    /// A JS/TS body. `Some` carries its source (the eval string, or a function's
    /// `toString()` passed in good faith from JS-land); `None` when no source was supplied
    /// (e.g. a compiled function created without a `script_source`).
    Script(Option<Arc<str>>),
    /// No action — neither a command nor a script.
    Noop,
}

/// One incremental change to the script-created automation set, streamed to a watching
/// automations window so it bookkeeps its own state instead of receiving full snapshots
/// (the set can reach tens of thousands for a bulk-creating package). Flushed per drain.
#[derive(Clone, Debug)]
pub enum AutomationDelta {
    /// Created, or its definition replaced under the same `(origin, kind, name)` key — may
    /// add a tree row.
    Upserted(AutomationSummary),
    /// An existing automation's `enabled` flag flipped — the hot path (item show/hide
    /// toggling). Just updates a status dot; never changes tree structure.
    EnabledChanged {
        kind: AutomationKind,
        origin: Origin,
        name: String,
        enabled: bool,
    },
    /// An automation was removed — by an explicit `delete()` or by hitting its
    /// `fireLimit`/`lineLimit` self-limit. Drops its tree row.
    Removed {
        kind: AutomationKind,
        origin: Origin,
        name: String,
    },
}

/// What a session's automation broadcast carries to watching windows.
#[derive(Clone, Debug)]
pub enum AutomationEvent {
    /// The full script-created set — sent when a window subscribes (and re-sent to all when
    /// another subscribes, since a broadcast can't replay). The window replaces its
    /// bookkeeping with this.
    Reset(Arc<Vec<AutomationSummary>>),
    /// A batch of incremental changes since the last drain, applied on top of the reset.
    Changed(Arc<Vec<AutomationDelta>>),
}

/// The creator descriptor the `smudgy:core` virtual module (and the inline-script API)
/// passes to the creation ops, as JSON. `module` carries the raw referrer URL; [`Origin`]
/// normalizes it to a `modules/`-relative subpath.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Creator {
    User,
    Module {
        referrer: String,
    },
    Package {
        owner: String,
        name: String,
        version: String,
    },
}

impl Origin {
    /// The namespace of the local `modules/` file at `subpath`.
    #[must_use]
    pub fn module(subpath: impl Into<Arc<str>>) -> Self {
        Self::Module(subpath.into())
    }

    /// The namespace of the package installed at `owner/name`, resolved to `version`.
    #[must_use]
    pub fn package(
        owner: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self::Package(Arc::new(PackageOrigin {
            owner: owner.into(),
            name: name.into(),
            version: version.into(),
        }))
    }

    /// Parse the JSON creator descriptor the `smudgy:core` facade passes. A malformed
    /// descriptor is an **error**: attribution is semantics — which trigger namespace an
    /// automation keys into, which producer subtree an interop write lands in, which
    /// namespace an event broadcasts under — so a garbage creator must fail loudly rather
    /// than silently attribute to the shared `user` namespace (`docs/interop.md` §3). The
    /// parse runs once per module/API construction (`op_smudgy_interop_resolve_creator`
    /// interns the result), so the failure surfaces at construction on every copy, before
    /// any per-call use. Unreachable in practice — the descriptor is host-minted via
    /// `JSON.stringify` in `__smudgy_make_api` — but the boundary stays strict.
    ///
    /// # Errors
    /// Returns the deserialization message when `json` is not a valid creator descriptor.
    pub fn try_from_creator_json(json: &str) -> Result<Self, String> {
        match serde_json::from_str::<Creator>(json) {
            Ok(Creator::User) => Ok(Self::User),
            Ok(Creator::Module { referrer }) => Ok(Self::module(module_subpath(&referrer))),
            Ok(Creator::Package {
                owner,
                name,
                version,
            }) => Ok(Self::package(owner, name, version)),
            Err(e) => Err(e.to_string()),
        }
    }

    /// The identity this origin reserves a `singleton` automation under: a `Package` drops its
    /// resolved version so every copy across isolates/versions shares one slot
    /// (see `PACKAGE-ISOLATES.md`).
    #[must_use]
    pub fn singleton_origin(&self) -> SingletonOrigin {
        match self {
            Self::User => SingletonOrigin::User,
            Self::Module(subpath) => SingletonOrigin::Module {
                subpath: subpath.to_string(),
            },
            Self::Package(pkg) => SingletonOrigin::Package {
                owner: pkg.owner.clone(),
                name: pkg.name.clone(),
            },
        }
    }
}

/// Normalize a local-module file URL to its `modules/`-relative subpath (the id the
/// automations UI keys module nodes by): everything after the last `/modules/` segment,
/// falling back to the raw referrer if that marker is absent.
fn module_subpath(referrer: &str) -> String {
    referrer
        .rsplit_once("/modules/")
        .map_or_else(|| referrer.to_string(), |(_, sub)| sub.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        AutomationKind, IsolateId, NO_ISOLATE_INSTANCE, Origin, SingletonKey, SingletonOrigin,
        SingletonReservations,
    };
    use std::sync::Arc;

    #[test]
    fn widget_token_round_trips_role_and_instance() {
        assert_eq!(
            IsolateId::from_widget_token(&IsolateId::Main.to_widget_token(7)),
            (IsolateId::Main, 7)
        );
        let pkg = IsolateId::package("wbk", "mapper", "1.4.0");
        assert_eq!(
            IsolateId::from_widget_token(&pkg.to_widget_token(42)),
            (pkg, 42)
        );
    }

    #[test]
    fn malformed_widget_token_is_inert() {
        // No live isolate carries NO_ISOLATE_INSTANCE, so each of these parses to a pair the
        // dispatch instance check rejects — never a runnable (Main, live-instance) pair.
        for token in [
            "",
            "main",
            "garbage",
            "pkg\u{1f}a\u{1f}b\u{1f}c",
            "\u{1f}main",
        ] {
            let (_, instance) = IsolateId::from_widget_token(token);
            assert_eq!(instance, NO_ISOLATE_INSTANCE, "token {token:?}");
        }
    }

    #[test]
    fn parses_creator_descriptors() {
        assert_eq!(
            Origin::try_from_creator_json(r#"{"kind":"user"}"#),
            Ok(Origin::User)
        );
        assert_eq!(
            Origin::try_from_creator_json(
                r#"{"kind":"package","owner":"wbk","name":"mapper","version":"1.4.0"}"#
            ),
            Ok(Origin::package("wbk", "mapper", "1.4.0"))
        );
        assert_eq!(
            Origin::try_from_creator_json(
                r#"{"kind":"module","referrer":"file:///c:/x/smudgy/srv/modules/combat/healer.ts"}"#
            ),
            Ok(Origin::module("combat/healer.ts"))
        );
        // A referrer with no `/modules/` marker keeps the raw value (still a stable key).
        assert_eq!(
            Origin::try_from_creator_json(r#"{"kind":"module","referrer":"file:///odd/path.ts"}"#),
            Ok(Origin::module("file:///odd/path.ts"))
        );
        // Malformed JSON is a loud error — attribution is semantics, never guessed.
        assert!(Origin::try_from_creator_json("not json").is_err());
    }

    #[test]
    fn failed_isolate_cleanup_releases_only_its_singleton_wins() {
        let winner = IsolateId::package("wbk", "winner", "1.0.0");
        let loser = IsolateId::package("wbk", "loser", "1.0.0");
        let key = SingletonKey {
            origin: SingletonOrigin::Package {
                owner: "wbk".to_string(),
                name: "shared".to_string(),
            },
            kind: AutomationKind::Trigger,
            name: Arc::from("ready"),
        };
        let mut reservations = SingletonReservations::default();
        assert!(reservations.reserve(key.clone(), winner.clone()));
        assert!(!reservations.reserve(key.clone(), loser.clone()));

        reservations.release_isolate(&loser);
        assert!(
            reservations.contains(&key),
            "a loser owns no seat to release"
        );
        reservations.release_isolate(&winner);
        assert!(!reservations.contains(&key));
    }
}
