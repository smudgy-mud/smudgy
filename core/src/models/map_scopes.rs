//! Per-user cloud-map scoping: which server entries each cloud atlas (or
//! genuinely atlas-less area) is shown on.
//!
//! Cloud atlases are account-global — every atlas otherwise shows on every
//! server — so this store carries the client-local, 1:n association from a
//! cloud atlas to the server *entries* it participates on. The scope key is the
//! server entry name: the stable, user-authored object that already survives a
//! game re-host (the user edits the entry's host field and nothing else moves).
//! Hosts never enter this store as keys.
//!
//! An atlas (or area) absent from the store — or present with an empty entry
//! set — is **Unassigned**: it participates on every entry, exactly like an
//! atlas that lists the current entry. Only an atlas with a non-empty entry set
//! that omits the current entry is excluded there. That exclusion is the payoff:
//! cross-game stock-zone collisions (every Diku Midgaard) drop out of room
//! identification on the entries they don't belong to.
//!
//! A clan's folders and maps are scoped by their **clan** alone: one record per
//! clan, never per folder or map, because a clan plays one game. A clan's
//! "Plays on" hosts home it onto the matching entries ([`MapScopes::home_clans`]).
//!
//! **Invariant:** the scope key is the server-entry *name*, which is immutable
//! today (no rename operation exists; a server's name is its directory name). If
//! a server-entry rename operation is ever built, it MUST rewrite the
//! association targets in this store — atlas, area and clan records and the
//! homed-clan ledger — within the same operation, otherwise every association
//! keyed on the old name silently detaches.

use crate::get_smudgy_home;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use smudgy_cloud::mapper::ScopeExclusions;
use smudgy_cloud::{AreaId, AtlasId, Uuid};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::{fs, io};

use super::persistence::write_atomic;

/// File name of the association store in the smudgy home.
const MAP_SCOPES_FILE: &str = "map-scopes.json";

/// The server entries a cloud atlas (or atlas-less area) is shown on. An empty
/// entry set is equivalent to the record's absence: **Unassigned**, i.e.
/// participating everywhere. Empty records are pruned on save.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeAssociation {
    /// Server-entry names this atlas/area is shown on. Empty = Unassigned.
    #[serde(default)]
    pub entries: BTreeSet<String>,
}

impl ScopeAssociation {
    /// True when this record carries no entries — the same as being absent
    /// (Unassigned; participates everywhere).
    #[must_use]
    pub fn is_unassigned(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A single targeted mutation of the association store, expressed in terms of
/// the [`MapScopes`] setters and replayed by the daemon against its
/// authoritative copy via [`MapScopes::apply`].
///
/// Editors emit deltas rather than whole-store snapshots so that a concurrent
/// write — a bind/rescue toast, first-sight homing, or another editor's edit —
/// landing between an editor reading its snapshot and the daemon adopting it is
/// never clobbered: each delta touches only its own target, leaving every other
/// association as the authority holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeDelta {
    /// Replace the full entry set an atlas is shown on (empty = Unassigned).
    SetAtlasEntries {
        atlas_id: AtlasId,
        entries: BTreeSet<String>,
    },
    /// Replace the full entry set an atlas-less area is shown on.
    SetAreaEntries {
        area_id: AreaId,
        entries: BTreeSet<String>,
    },
    /// Show or hide an atlas on a single entry, leaving its other entries alone.
    SetAtlasEntry {
        atlas_id: AtlasId,
        entry: String,
        show: bool,
    },
    /// Show or hide an atlas-less area on a single entry.
    SetAreaEntry {
        area_id: AreaId,
        entry: String,
        show: bool,
    },
    /// Record first sight of an atlas (the §5 homing bookkeeping).
    MarkSeen { atlas_id: AtlasId },
    /// Replace the full entry set a clan's folders and maps are shown on.
    SetClanEntries {
        clan_id: Uuid,
        entries: BTreeSet<String>,
    },
    /// Show or hide a clan on a single entry.
    SetClanEntry {
        clan_id: Uuid,
        entry: String,
        show: bool,
    },
}

/// Where an atlas/area sits relative to one server entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeState {
    /// Associated with this entry (its name is in the record's entry set).
    Here,
    /// Unassigned: absent from the store, or present with no entries. Shown on
    /// every entry, including this one.
    Unassigned,
    /// Associated only with *other* entries — excluded here.
    Elsewhere,
}

/// The client-local atlas/area → server-entry association store, persisted as
/// `map-scopes.json` in the smudgy home. See the module docs for semantics.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct MapScopes {
    /// Cloud atlas id → the entries it is shown on. Atlas-level association is
    /// the norm (a shared area's atlas is visible with §4.1 un-redaction).
    #[serde(default)]
    atlases: HashMap<AtlasId, ScopeAssociation>,
    /// Genuinely atlas-less cloud area id → the entries it is shown on. Only
    /// for areas with no atlas container; areas inside an atlas are scoped by
    /// their atlas.
    #[serde(default)]
    areas: HashMap<AreaId, ScopeAssociation>,
    /// Atlas ids the client has ever observed, so "first sight" is detectable
    /// for the §5 recipient-homing defaults (built later). Bookkeeping only —
    /// it never affects exclusion.
    #[serde(default)]
    seen_atlases: HashSet<AtlasId>,
    /// Clan id → the entries the clan's folders and maps are shown on. Atlas
    /// and area records are never consulted for a clan's content.
    #[serde(default)]
    clans: HashMap<Uuid, ScopeAssociation>,
    /// Clan id → the entries homing has added to it from the clan's hosts.
    /// Homing adds an entry to a clan at most once, so an entry the user later
    /// unchecks stays unchecked.
    #[serde(default)]
    homed_clans: HashMap<Uuid, BTreeSet<String>>,
}

/// One local server entry as recipient-homing evidence (§5): its name — the
/// stable scope key — and the host/port it currently points at. Built from
/// [`crate::models::server::Server`] at the call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEntry {
    /// The server-entry name (the association key).
    pub name: String,
    /// The entry's configured host, as authored.
    pub host: String,
    /// The entry's configured port.
    pub port: u16,
}

/// Every local server entry as homing evidence. Entries whose configuration
/// cannot be read are left out; an unreadable smudgy home yields none.
#[must_use]
pub fn host_entries() -> Vec<HostEntry> {
    crate::models::server::list_servers()
        .unwrap_or_default()
        .into_iter()
        .map(|server| HostEntry {
            name: server.name,
            host: server.config.host,
            port: server.config.port,
        })
        .collect()
}

/// Split a grantor host hint into its host and optional port. A port is
/// recognized only when the hint's last `:`-delimited segment parses as a
/// `u16` (so bare hosts, and hosts whose trailing colon-segment isn't a port —
/// e.g. an IPv6 literal — carry no port constraint). Evidence only: no
/// aliasing, no DNS, no inference (§5.1).
fn split_hint(hint: &str) -> (&str, Option<u16>) {
    match hint.rsplit_once(':') {
        Some((host, tail)) => match tail.parse::<u16>() {
            Ok(port) => (host, Some(port)),
            Err(_) => (hint, None),
        },
        None => (hint, None),
    }
}

/// Match grantor-authored host hints against the local server entries (§5.1):
/// **case-insensitive host equality; the port compared only when the hint
/// carries one** (a bare `arctic.org` hint matches an `arctic.org` entry on any
/// port; `arctic.org:2700` matches only port 2700). A hint that matches nothing
/// falls through — there is no aliasing or inference. Returns the names of the
/// matching entries (the set to associate the shared atlas/area with).
#[must_use]
pub fn match_host_hints(hints: &[String], entries: &[HostEntry]) -> BTreeSet<String> {
    let mut matched = BTreeSet::new();
    for hint in hints {
        let hint = hint.trim();
        if hint.is_empty() {
            continue;
        }
        let (hint_host, hint_port) = split_hint(hint);
        for entry in entries {
            if !entry.host.trim().eq_ignore_ascii_case(hint_host) {
                continue;
            }
            if hint_port.is_some_and(|port| port != entry.port) {
                continue;
            }
            matched.insert(entry.name.clone());
        }
    }
    matched
}

/// Classify a (possibly absent) association against one entry.
fn classify(assoc: Option<&ScopeAssociation>, entry: &str) -> ScopeState {
    match assoc {
        Some(assoc) if !assoc.entries.is_empty() => {
            if assoc.entries.contains(entry) {
                ScopeState::Here
            } else {
                ScopeState::Elsewhere
            }
        }
        _ => ScopeState::Unassigned,
    }
}

impl MapScopes {
    /// Loads the association store from `map-scopes.json`, or the empty default
    /// if the file is missing or cannot be read/parsed (logged). Never fails —
    /// a bad scopes file must not block startup.
    #[must_use]
    pub fn load() -> Self {
        match Self::try_load() {
            Ok(scopes) => scopes,
            Err(e) => {
                eprintln!("Warning: Failed to load map scopes, using empty defaults: {e}");
                Self::default()
            }
        }
    }

    fn try_load() -> Result<Self> {
        let path = get_smudgy_home()?.join(MAP_SCOPES_FILE);
        match fs::read_to_string(&path) {
            Ok(content) => {
                let scopes: Self =
                    serde_json::from_str(&content).context("Failed to parse map-scopes.json")?;
                Ok(scopes)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).context(format!(
                "Failed to read map-scopes.json at {}",
                path.display()
            )),
        }
    }

    /// Persists the store to `map-scopes.json`, pruning empty (Unassigned)
    /// association records first so the file stays minimal.
    ///
    /// # Errors
    /// Returns an error if the smudgy home can't be resolved, serialization
    /// fails, or the file can't be written.
    pub fn save(&self) -> Result<()> {
        let mut pruned = self.clone();
        pruned.atlases.retain(|_, assoc| !assoc.is_unassigned());
        pruned.areas.retain(|_, assoc| !assoc.is_unassigned());
        pruned.clans.retain(|_, assoc| !assoc.is_unassigned());
        pruned.homed_clans.retain(|_, homed| !homed.is_empty());

        let path = get_smudgy_home()?.join(MAP_SCOPES_FILE);
        let json =
            serde_json::to_string_pretty(&pruned).context("Failed to serialize map scopes")?;
        write_atomic(&path, json.as_bytes()).context(format!(
            "Failed to write map-scopes.json at {}",
            path.display()
        ))?;
        Ok(())
    }

    /// Records that `atlas_id` has been observed. Returns `true` if this is the
    /// first sight (the id was newly added), so a caller can apply §5 homing
    /// defaults exactly once.
    pub fn mark_seen(&mut self, atlas_id: AtlasId) -> bool {
        self.seen_atlases.insert(atlas_id)
    }

    /// Whether `atlas_id` has been observed before.
    #[must_use]
    pub fn has_seen(&self, atlas_id: &AtlasId) -> bool {
        self.seen_atlases.contains(atlas_id)
    }

    /// The entries an atlas is shown on (empty = Unassigned).
    #[must_use]
    pub fn atlas_entries(&self, atlas_id: &AtlasId) -> BTreeSet<String> {
        self.atlases
            .get(atlas_id)
            .map(|assoc| assoc.entries.clone())
            .unwrap_or_default()
    }

    /// The entries an atlas-less area is shown on (empty = Unassigned).
    #[must_use]
    pub fn area_entries(&self, area_id: &AreaId) -> BTreeSet<String> {
        self.areas
            .get(area_id)
            .map(|assoc| assoc.entries.clone())
            .unwrap_or_default()
    }

    /// Where `atlas_id` sits relative to `entry`.
    #[must_use]
    pub fn atlas_scope(&self, atlas_id: &AtlasId, entry: &str) -> ScopeState {
        classify(self.atlases.get(atlas_id), entry)
    }

    /// Where the atlas-less area `area_id` sits relative to `entry`.
    #[must_use]
    pub fn area_scope(&self, area_id: &AreaId, entry: &str) -> ScopeState {
        classify(self.areas.get(area_id), entry)
    }

    /// The entries a clan's folders and maps are shown on (empty = Unassigned).
    #[must_use]
    pub fn clan_entries(&self, clan_id: &Uuid) -> BTreeSet<String> {
        self.clans
            .get(clan_id)
            .map(|assoc| assoc.entries.clone())
            .unwrap_or_default()
    }

    /// Where the clan `clan_id` sits relative to `entry`.
    #[must_use]
    pub fn clan_scope(&self, clan_id: &Uuid, entry: &str) -> ScopeState {
        classify(self.clans.get(clan_id), entry)
    }

    /// Replaces the full entry set a clan is shown on.
    pub fn set_clan_entries(&mut self, clan_id: Uuid, entries: BTreeSet<String>) {
        self.clans.insert(clan_id, ScopeAssociation { entries });
    }

    /// Shows or hides the clan `clan_id` on a single `entry`.
    pub fn set_clan_entry(&mut self, clan_id: Uuid, entry: &str, show: bool) {
        let assoc = self.clans.entry(clan_id).or_default();
        if show {
            assoc.entries.insert(entry.to_string());
        } else {
            assoc.entries.remove(entry);
        }
    }

    /// Homes clans onto server entries from their "Plays on" hosts: every
    /// entry whose host matches one of a clan's hosts ([`match_host_hints`])
    /// and that homing has not added to that clan before is added to it and
    /// recorded. Homing only adds, so it never undoes a choice, and an entry
    /// the user unchecks after homing added it stays unchecked. Returns
    /// whether the store changed.
    pub fn home_clans<'a>(
        &mut self,
        clans: impl IntoIterator<Item = (Uuid, &'a [String])>,
        entries: &[HostEntry],
    ) -> bool {
        let mut changed = false;
        for (clan_id, hosts) in clans {
            for entry in match_host_hints(hosts, entries) {
                if self
                    .homed_clans
                    .entry(clan_id)
                    .or_default()
                    .insert(entry.clone())
                {
                    self.clans.entry(clan_id).or_default().entries.insert(entry);
                    changed = true;
                }
            }
        }
        changed
    }

    /// Every server-entry name the store refers to.
    #[must_use]
    pub fn entry_names(&self) -> BTreeSet<String> {
        self.atlases
            .values()
            .chain(self.areas.values())
            .chain(self.clans.values())
            .flat_map(|assoc| assoc.entries.iter())
            .chain(self.homed_clans.values().flatten())
            .cloned()
            .collect()
    }

    /// Removes the names of deleted server entries — those
    /// [`crate::models::server::server_exists`] reports gone — and homes clans
    /// onto the remaining entries from `clan_hosts` ([`Self::home_clans`]). A
    /// name whose existence can't be checked is kept. Returns whether the store
    /// changed.
    pub fn reconcile_with_servers(&mut self, clan_hosts: &HashMap<Uuid, Vec<String>>) -> bool {
        let mut changed = false;
        for name in self.entry_names() {
            if matches!(crate::models::server::server_exists(&name), Ok(false)) {
                changed |= self.forget_entry(&name);
            }
        }
        if !clan_hosts.is_empty() {
            changed |= self.home_clans(
                clan_hosts.iter().map(|(id, hosts)| (*id, hosts.as_slice())),
                &host_entries(),
            );
        }
        changed
    }

    /// Removes a deleted server entry's name from every record and from the
    /// homed-clan ledger. A dangling name would leave a folder associated only
    /// with an entry that no longer exists, which excludes it on every
    /// remaining entry. Returns whether the store changed.
    pub fn forget_entry(&mut self, entry: &str) -> bool {
        let mut changed = false;
        for assoc in self
            .atlases
            .values_mut()
            .chain(self.areas.values_mut())
            .chain(self.clans.values_mut())
        {
            changed |= assoc.entries.remove(entry);
        }
        for homed in self.homed_clans.values_mut() {
            changed |= homed.remove(entry);
        }
        changed
    }

    /// Replaces the full entry set an atlas is shown on. Passing an empty set
    /// makes the atlas Unassigned (the record is pruned on save).
    pub fn set_atlas_entries(&mut self, atlas_id: AtlasId, entries: BTreeSet<String>) {
        self.atlases.insert(atlas_id, ScopeAssociation { entries });
    }

    /// Replaces the full entry set an atlas-less area is shown on.
    pub fn set_area_entries(&mut self, area_id: AreaId, entries: BTreeSet<String>) {
        self.areas.insert(area_id, ScopeAssociation { entries });
    }

    /// Shows or hides `atlas_id` on a single `entry`, leaving the other entries
    /// untouched.
    pub fn set_atlas_entry(&mut self, atlas_id: AtlasId, entry: &str, show: bool) {
        let assoc = self.atlases.entry(atlas_id).or_default();
        if show {
            assoc.entries.insert(entry.to_string());
        } else {
            assoc.entries.remove(entry);
        }
    }

    /// Shows or hides the atlas-less area `area_id` on a single `entry`.
    pub fn set_area_entry(&mut self, area_id: AreaId, entry: &str, show: bool) {
        let assoc = self.areas.entry(area_id).or_default();
        if show {
            assoc.entries.insert(entry.to_string());
        } else {
            assoc.entries.remove(entry);
        }
    }

    /// Replays one [`ScopeDelta`] against this store in terms of the existing
    /// setters. The daemon applies editor-origin deltas to its authoritative
    /// copy; an editor applies its own deltas optimistically so its tree updates
    /// before the mirrored snapshot returns.
    pub fn apply(&mut self, delta: &ScopeDelta) {
        match delta {
            ScopeDelta::SetAtlasEntries { atlas_id, entries } => {
                self.set_atlas_entries(*atlas_id, entries.clone());
            }
            ScopeDelta::SetAreaEntries { area_id, entries } => {
                self.set_area_entries(*area_id, entries.clone());
            }
            ScopeDelta::SetAtlasEntry {
                atlas_id,
                entry,
                show,
            } => self.set_atlas_entry(*atlas_id, entry, *show),
            ScopeDelta::SetAreaEntry {
                area_id,
                entry,
                show,
            } => self.set_area_entry(*area_id, entry, *show),
            ScopeDelta::MarkSeen { atlas_id } => {
                self.mark_seen(*atlas_id);
            }
            ScopeDelta::SetClanEntries { clan_id, entries } => {
                self.set_clan_entries(*clan_id, entries.clone());
            }
            ScopeDelta::SetClanEntry {
                clan_id,
                entry,
                show,
            } => self.set_clan_entry(*clan_id, entry, *show),
        }
    }

    /// The atlases excluded on `entry`: those with a non-empty entry set that
    /// omits `entry`. Unassigned atlases (absent or empty) never appear here.
    /// This is one of the two inputs to the mapper's scope exclusion.
    #[must_use]
    pub fn excluded_atlases(&self, entry: &str) -> HashSet<AtlasId> {
        self.atlases
            .iter()
            .filter(|(_, assoc)| classify(Some(assoc), entry) == ScopeState::Elsewhere)
            .map(|(id, _)| *id)
            .collect()
    }

    /// The atlas-less areas excluded on `entry`. The area-id counterpart of
    /// [`Self::excluded_atlases`].
    #[must_use]
    pub fn excluded_areas(&self, entry: &str) -> HashSet<AreaId> {
        self.areas
            .iter()
            .filter(|(_, assoc)| classify(Some(assoc), entry) == ScopeState::Elsewhere)
            .map(|(id, _)| *id)
            .collect()
    }

    /// The clans excluded on `entry`: those shown only on other entries.
    #[must_use]
    pub fn excluded_clans(&self, entry: &str) -> HashSet<Uuid> {
        self.clans
            .iter()
            .filter(|(_, assoc)| classify(Some(assoc), entry) == ScopeState::Elsewhere)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Everything the scope keeps out on `entry`, for
    /// `Mapper::set_scope_exclusions`.
    #[must_use]
    pub fn exclusions(&self, entry: &str) -> ScopeExclusions {
        ScopeExclusions {
            atlases: self.excluded_atlases(entry),
            areas: self.excluded_areas(entry),
            clans: self.excluded_clans(entry),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::Uuid;

    fn atlas(n: u128) -> AtlasId {
        AtlasId(Uuid::from_u128(n))
    }

    fn area(n: u128) -> AreaId {
        AreaId(Uuid::from_u128(n))
    }

    fn entries(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn roundtrips_through_json() {
        let mut scopes = MapScopes::default();
        scopes.set_atlas_entries(atlas(1), entries(&["Arctic", "arctic-scripts"]));
        scopes.set_area_entries(area(2), entries(&["Aardwolf"]));
        scopes.mark_seen(atlas(1));
        scopes.mark_seen(atlas(9));

        let json = serde_json::to_string(&scopes).expect("serialize");
        let parsed: MapScopes = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed, scopes);
    }

    #[test]
    fn empty_default_parses_from_missing_fields() {
        let scopes: MapScopes = serde_json::from_str("{}").expect("parse");
        assert_eq!(scopes, MapScopes::default());
    }

    #[test]
    fn unassigned_participates_everywhere() {
        let scopes = MapScopes::default();
        // Absent: unassigned, so excluded on no entry.
        assert_eq!(
            scopes.atlas_scope(&atlas(1), "Arctic"),
            ScopeState::Unassigned
        );
        assert!(scopes.excluded_atlases("Arctic").is_empty());

        // Present but empty: still unassigned.
        let mut scopes = MapScopes::default();
        scopes.set_atlas_entries(atlas(1), entries(&[]));
        assert_eq!(
            scopes.atlas_scope(&atlas(1), "Arctic"),
            ScopeState::Unassigned
        );
        assert!(scopes.excluded_atlases("Arctic").is_empty());
    }

    #[test]
    fn exclusion_computation_atlas_and_area() {
        let mut scopes = MapScopes::default();
        scopes.set_atlas_entries(atlas(1), entries(&["Arctic"]));
        scopes.set_area_entries(area(2), entries(&["Arctic"]));

        // On Arctic: nothing excluded (both are shown here).
        assert!(scopes.excluded_atlases("Arctic").is_empty());
        assert!(scopes.excluded_areas("Arctic").is_empty());
        assert_eq!(scopes.atlas_scope(&atlas(1), "Arctic"), ScopeState::Here);

        // On Aardwolf: both excluded (associated only with Arctic).
        assert_eq!(
            scopes.excluded_atlases("Aardwolf"),
            [atlas(1)].into_iter().collect()
        );
        assert_eq!(
            scopes.excluded_areas("Aardwolf"),
            [area(2)].into_iter().collect()
        );
        assert_eq!(
            scopes.atlas_scope(&atlas(1), "Aardwolf"),
            ScopeState::Elsewhere
        );
    }

    #[test]
    fn single_entry_toggle_adds_and_removes() {
        let mut scopes = MapScopes::default();
        scopes.set_atlas_entry(atlas(1), "Arctic", true);
        scopes.set_atlas_entry(atlas(1), "arctic-scripts", true);
        assert_eq!(
            scopes.atlas_entries(&atlas(1)),
            entries(&["Arctic", "arctic-scripts"])
        );

        scopes.set_atlas_entry(atlas(1), "Arctic", false);
        assert_eq!(
            scopes.atlas_entries(&atlas(1)),
            entries(&["arctic-scripts"])
        );
        // Still excluded on Arctic (associated only with arctic-scripts now).
        assert_eq!(
            scopes.atlas_scope(&atlas(1), "Arctic"),
            ScopeState::Elsewhere
        );
    }

    #[test]
    fn deltas_targeting_different_atlases_survive_in_either_order() {
        // The lost-update regression: two independent writes (here, two atlases
        // homed to different entries) that in the old full-snapshot channel
        // would clobber each other must both survive when replayed as deltas,
        // regardless of the order the authoritative copy applies them.
        let d1 = ScopeDelta::SetAtlasEntries {
            atlas_id: atlas(1),
            entries: entries(&["Arctic"]),
        };
        let d2 = ScopeDelta::SetAtlasEntries {
            atlas_id: atlas(2),
            entries: entries(&["Aardwolf"]),
        };

        let mut forward = MapScopes::default();
        forward.apply(&d1);
        forward.apply(&d2);

        let mut reverse = MapScopes::default();
        reverse.apply(&d2);
        reverse.apply(&d1);

        // Order-independent, and neither write erased the other.
        assert_eq!(forward, reverse);
        assert_eq!(forward.atlas_entries(&atlas(1)), entries(&["Arctic"]));
        assert_eq!(forward.atlas_entries(&atlas(2)), entries(&["Aardwolf"]));
    }

    #[test]
    fn apply_covers_every_delta_variant() {
        let mut scopes = MapScopes::default();
        scopes.apply(&ScopeDelta::SetAtlasEntry {
            atlas_id: atlas(1),
            entry: "Arctic".to_string(),
            show: true,
        });
        scopes.apply(&ScopeDelta::SetAtlasEntries {
            atlas_id: atlas(2),
            entries: entries(&["Aardwolf", "Achaea"]),
        });
        scopes.apply(&ScopeDelta::SetAreaEntry {
            area_id: area(3),
            entry: "Arctic".to_string(),
            show: true,
        });
        scopes.apply(&ScopeDelta::SetAreaEntries {
            area_id: area(4),
            entries: entries(&["Aardwolf"]),
        });
        scopes.apply(&ScopeDelta::MarkSeen { atlas_id: atlas(5) });

        assert_eq!(scopes.atlas_entries(&atlas(1)), entries(&["Arctic"]));
        assert_eq!(
            scopes.atlas_entries(&atlas(2)),
            entries(&["Aardwolf", "Achaea"])
        );
        assert_eq!(scopes.area_entries(&area(3)), entries(&["Arctic"]));
        assert_eq!(scopes.area_entries(&area(4)), entries(&["Aardwolf"]));
        assert!(scopes.has_seen(&atlas(5)));

        // A hide delta removes just that entry, leaving the rest.
        scopes.apply(&ScopeDelta::SetAtlasEntry {
            atlas_id: atlas(2),
            entry: "Aardwolf".to_string(),
            show: false,
        });
        assert_eq!(scopes.atlas_entries(&atlas(2)), entries(&["Achaea"]));
    }

    #[test]
    fn mark_seen_reports_first_sight_only_once() {
        let mut scopes = MapScopes::default();
        assert!(scopes.mark_seen(atlas(1)), "first sight is newly seen");
        assert!(!scopes.mark_seen(atlas(1)), "second sight is not new");
        assert!(scopes.has_seen(&atlas(1)));
        assert!(!scopes.has_seen(&atlas(2)));
    }

    fn host_entry(name: &str, host: &str, port: u16) -> HostEntry {
        HostEntry {
            name: name.to_string(),
            host: host.to_string(),
            port,
        }
    }

    fn hints(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn bare_host_hint_matches_case_insensitively_any_port() {
        let servers = [
            host_entry("Arctic", "arctic.org", 2700),
            host_entry("arctic-scripts", "ARCTIC.ORG", 4000),
            host_entry("Aardwolf", "aardwolf.org", 4000),
        ];
        // A bare host hint ignores the port entirely and is case-insensitive on
        // the host, so both Arctic entries match and Aardwolf does not.
        let matched = match_host_hints(&hints(&["Arctic.Org"]), &servers);
        assert_eq!(matched, entries(&["Arctic", "arctic-scripts"]));
    }

    #[test]
    fn host_port_hint_constrains_to_that_port() {
        let servers = [
            host_entry("Arctic", "arctic.org", 2700),
            host_entry("arctic-scripts", "arctic.org", 4000),
        ];
        // A port-bearing hint matches only the entry on that exact port.
        let matched = match_host_hints(&hints(&["arctic.org:4000"]), &servers);
        assert_eq!(matched, entries(&["arctic-scripts"]));
    }

    #[test]
    fn near_miss_falls_through_no_inference() {
        let servers = [host_entry("Arctic", "arctic.org", 2700)];
        // A different host, and the right host on the wrong port, both miss —
        // there is no aliasing or host-equality inference.
        assert!(match_host_hints(&hints(&["mud.arctic.org"]), &servers).is_empty());
        assert!(match_host_hints(&hints(&["arctic.org:5000"]), &servers).is_empty());
        // Blank / whitespace hints are ignored, not matched to anything.
        assert!(match_host_hints(&hints(&["", "   "]), &servers).is_empty());
    }

    #[test]
    fn multiple_hints_union_their_matches() {
        let servers = [
            host_entry("Arctic", "arctic.org", 2700),
            host_entry("Aardwolf", "aardwolf.org", 4000),
        ];
        let matched = match_host_hints(&hints(&["arctic.org", "aardwolf.org:4000"]), &servers);
        assert_eq!(matched, entries(&["Aardwolf", "Arctic"]));
    }

    fn clan(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn a_clan_is_scoped_as_one() {
        let mut scopes = MapScopes::default();
        // A record for one of the clan's folders is a separate record; the
        // clan's own decides its content.
        scopes.set_atlas_entries(atlas(5), entries(&["Aardwolf"]));
        assert_eq!(
            scopes.clan_scope(&clan(1), "Arctic"),
            ScopeState::Unassigned
        );
        scopes.set_clan_entry(clan(1), "Arctic", true);
        assert_eq!(scopes.clan_scope(&clan(1), "Arctic"), ScopeState::Here);
        assert_eq!(
            scopes.clan_scope(&clan(1), "Aardwolf"),
            ScopeState::Elsewhere
        );
        let on_aardwolf = scopes.exclusions("Aardwolf");
        assert_eq!(on_aardwolf.clans, [clan(1)].into_iter().collect());
        assert!(scopes.exclusions("Arctic").clans.is_empty());
    }

    #[test]
    fn homing_adds_each_matching_entry_once() {
        let servers = [
            host_entry("Arctic", "mud.arctic.org", 2700),
            host_entry("arctic-alias", "ArcticMud.org", 2700),
            host_entry("Aardwolf", "aardwolf.org", 4000),
        ];
        let plays_on = hints(&["mud.arctic.org", "arcticmud.org"]);
        let mut scopes = MapScopes::default();
        assert!(scopes.home_clans([(clan(1), plays_on.as_slice())], &servers));
        assert_eq!(
            scopes.clan_entries(&clan(1)),
            entries(&["Arctic", "arctic-alias"])
        );
        // Nothing new to add: no change.
        assert!(!scopes.home_clans([(clan(1), plays_on.as_slice())], &servers));

        // The user unchecks one; homing never re-adds it.
        scopes.set_clan_entry(clan(1), "arctic-alias", false);
        assert!(!scopes.home_clans([(clan(1), plays_on.as_slice())], &servers));
        assert_eq!(scopes.clan_entries(&clan(1)), entries(&["Arctic"]));

        // A new entry for the game is added once it exists.
        let mut more = servers.to_vec();
        more.push(host_entry("arctic-laptop", "mud.arctic.org", 2700));
        assert!(scopes.home_clans([(clan(1), plays_on.as_slice())], &more));
        assert_eq!(
            scopes.clan_entries(&clan(1)),
            entries(&["Arctic", "arctic-laptop"])
        );
    }

    #[test]
    fn homing_respects_a_clan_host_port_and_skips_hostless_clans() {
        let servers = [
            host_entry("Arctic", "mud.arctic.org", 2700),
            host_entry("arctic-builder", "mud.arctic.org", 4000),
        ];
        let mut scopes = MapScopes::default();
        let plays_on = hints(&["mud.arctic.org:2700"]);
        assert!(scopes.home_clans(
            [(clan(1), plays_on.as_slice()), (clan(2), &[][..])],
            &servers
        ));
        assert_eq!(scopes.clan_entries(&clan(1)), entries(&["Arctic"]));
        assert_eq!(
            scopes.clan_scope(&clan(2), "Arctic"),
            ScopeState::Unassigned
        );
    }

    #[test]
    fn forgetting_an_entry_removes_it_everywhere() {
        let mut scopes = MapScopes::default();
        scopes.set_atlas_entries(atlas(1), entries(&["Arctic", "old"]));
        scopes.set_area_entries(area(2), entries(&["old"]));
        scopes.set_clan_entries(clan(3), entries(&["old"]));
        scopes.home_clans(
            [(clan(3), hints(&["old.example.org"]).as_slice())],
            &[host_entry("old", "old.example.org", 23)],
        );
        assert!(scopes.forget_entry("old"));
        assert_eq!(scopes.atlas_entries(&atlas(1)), entries(&["Arctic"]));
        assert_eq!(
            scopes.area_scope(&area(2), "Arctic"),
            ScopeState::Unassigned
        );
        assert_eq!(
            scopes.clan_scope(&clan(3), "Arctic"),
            ScopeState::Unassigned
        );
        assert!(scopes.exclusions("Arctic").atlases.is_empty());
        // A new entry under the same name is homed afresh.
        assert!(scopes.home_clans(
            [(clan(3), hints(&["old.example.org"]).as_slice())],
            &[host_entry("old", "old.example.org", 23)],
        ));
        assert!(!scopes.forget_entry("missing"));
    }

    #[test]
    fn entry_names_cover_every_record_and_the_ledger() {
        let mut scopes = MapScopes::default();
        scopes.set_atlas_entries(atlas(1), entries(&["a"]));
        scopes.set_area_entries(area(2), entries(&["b"]));
        scopes.set_clan_entries(clan(3), entries(&["c"]));
        scopes.home_clans(
            [(clan(4), hints(&["d.example.org"]).as_slice())],
            &[host_entry("d", "d.example.org", 23)],
        );
        scopes.set_clan_entry(clan(4), "d", false);
        assert_eq!(scopes.entry_names(), entries(&["a", "b", "c", "d"]));
    }

    #[test]
    fn clan_deltas_replay() {
        let mut scopes = MapScopes::default();
        scopes.apply(&ScopeDelta::SetClanEntries {
            clan_id: clan(1),
            entries: entries(&["Arctic", "Aardwolf"]),
        });
        scopes.apply(&ScopeDelta::SetClanEntry {
            clan_id: clan(1),
            entry: "Aardwolf".to_string(),
            show: false,
        });
        assert_eq!(scopes.clan_entries(&clan(1)), entries(&["Arctic"]));
        let json = serde_json::to_string(&scopes).expect("serialize");
        let parsed: MapScopes = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed, scopes);
    }

    /// A temporary smudgy home shared by this binary's tests (the first
    /// override wins), and a server name no other test uses.
    fn temp_server_name(label: &str) -> String {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static TEST_HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        TEST_HOME.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!(
                "smudgy-map-scopes-test-home-{}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("create temp home");
            crate::set_smudgy_home(dir.clone());
            dir
        });
        format!(
            "scopes-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn reconcile_drops_deleted_entries_and_homes_clans() {
        use crate::models::server::{ServerConfig, create_server, delete_server};
        let live = temp_server_name("live");
        let gone = temp_server_name("gone");
        let host = format!("{live}.example.org");
        create_server(&live, ServerConfig::new(host.clone(), 2700)).expect("create server");

        let mut scopes = MapScopes::default();
        scopes.set_atlas_entries(atlas(1), entries(&[&live, &gone]));
        scopes.set_clan_entries(clan(2), entries(&[&gone]));
        let clan_hosts = HashMap::from([(clan(3), vec![host.to_uppercase()])]);
        assert!(scopes.reconcile_with_servers(&clan_hosts));
        assert_eq!(scopes.atlas_entries(&atlas(1)), entries(&[&live]));
        assert_eq!(scopes.clan_scope(&clan(2), &live), ScopeState::Unassigned);
        assert_eq!(scopes.clan_entries(&clan(3)), entries(&[&live]));
        assert!(!scopes.reconcile_with_servers(&clan_hosts), "nothing new");

        delete_server(&live).expect("delete server");
        assert!(scopes.reconcile_with_servers(&clan_hosts));
        assert!(scopes.entry_names().is_empty());
    }

    #[test]
    fn save_prunes_empty_records() {
        // An emptied association is Unassigned and must not persist as a record.
        let mut scopes = MapScopes::default();
        scopes.set_atlas_entries(atlas(1), entries(&["Arctic"]));
        scopes.set_atlas_entry(atlas(1), "Arctic", false);

        let mut pruned = scopes.clone();
        pruned.atlases.retain(|_, assoc| !assoc.is_unassigned());
        assert!(
            pruned.atlases.is_empty(),
            "empty record pruned before write"
        );
    }
}
