//! Data model for the Automations window: the script tree, the status model,
//! and the (client-side) package dependency graph derivations described in
//! `docs/new-automations-window.md`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use smudgy_core::models::shared_packages::LockedPackage;
use smudgy_core::models::{self, aliases, hotkeys, packages, triggers};
use smudgy_core::session::runtime::{
    AutomationBody, AutomationDelta, AutomationKind, AutomationSummary, Origin,
};

use super::{AutomationsWindow, Message};

/// Live script-created automations for the open session, kept in sync from the per-session
/// automation broadcast and keyed by creator [`Origin`]. The sidebar nests each creator's
/// aliases/triggers under its module/package node. Disk-authored automations are not here
/// (they come from the on-disk model); only `Module`/`Package`-origin ones are streamed.
#[derive(Default)]
pub struct LiveAutomations {
    by_origin: HashMap<Origin, CreatorAutomations>,
}

/// One creator's script-created aliases/triggers (name → live detail).
#[derive(Default)]
pub struct CreatorAutomations {
    pub aliases: BTreeMap<String, LiveAutomation>,
    pub triggers: BTreeMap<String, LiveAutomation>,
}

/// A single script-created automation's live state, mirrored from the runtime's introspection
/// stream — its on/off flag plus the read-only pattern/body shown in the detail pane.
#[derive(Clone)]
pub struct LiveAutomation {
    pub enabled: bool,
    /// Match pattern(s), joined for display. Empty when it has none.
    pub pattern: Arc<str>,
    /// What it does (command text, script source, or none). Display-only.
    pub body: AutomationBody,
}

impl LiveAutomations {
    /// Replace all state from a fresh snapshot (sent when the window subscribes, and after a
    /// session reload).
    pub fn reset(&mut self, summaries: &[AutomationSummary]) {
        self.by_origin.clear();
        for summary in summaries {
            self.upsert(summary);
        }
    }

    /// Apply a batch of incremental changes on top of the current state.
    pub fn apply(&mut self, deltas: &[AutomationDelta]) {
        for delta in deltas {
            match delta {
                AutomationDelta::Upserted(summary) => self.upsert(summary),
                AutomationDelta::EnabledChanged {
                    kind,
                    origin,
                    name,
                    enabled,
                } => {
                    if let Some(creator) = self.by_origin.get_mut(origin)
                        && let Some(slot) = creator.map_mut(*kind).get_mut(name)
                    {
                        slot.enabled = *enabled;
                    }
                }
                AutomationDelta::Removed { kind, origin, name } => {
                    if let Some(creator) = self.by_origin.get_mut(origin) {
                        creator.map_mut(*kind).remove(name);
                    }
                }
            }
        }
    }

    fn upsert(&mut self, summary: &AutomationSummary) {
        let creator = self.by_origin.entry(summary.origin.clone()).or_default();
        creator.map_mut(summary.kind).insert(
            summary.name.clone(),
            LiveAutomation {
                enabled: summary.enabled,
                pattern: summary.pattern.clone(),
                body: summary.body.clone(),
            },
        );
    }

    /// A local module's automations, keyed by its `modules/`-relative subpath.
    pub fn module(&self, subpath: &str) -> Option<&CreatorAutomations> {
        self.by_origin.get(&Origin::Module {
            subpath: subpath.to_string(),
        })
    }

    /// An installed package's automations, matched by owner/name across any resolved version.
    pub fn package(&self, owner: &str, name: &str) -> Option<&CreatorAutomations> {
        self.by_origin
            .iter()
            .find_map(|(origin, creator)| match origin {
                Origin::Package {
                    owner: o, name: n, ..
                } if o == owner && n == name => Some(creator),
                _ => None,
            })
    }
}

impl CreatorAutomations {
    fn map_mut(&mut self, kind: AutomationKind) -> &mut BTreeMap<String, LiveAutomation> {
        match kind {
            AutomationKind::Alias => &mut self.aliases,
            AutomationKind::Trigger => &mut self.triggers,
            // Hotkeys are not streamed as automation deltas (they live in the runtime's own
            // `HotkeyId` map, not the trigger introspection mirror), so this is never reached.
            AutomationKind::Hotkey => unreachable!("hotkeys are not tracked as automation deltas"),
        }
    }
}

/// A leaf automation or a folder, mirroring the on-disk model. Folders hold a
/// nested map of children (the tree is built from each script's `package` path).
#[derive(Debug, Clone)]
pub enum Script {
    Alias(models::aliases::AliasDefinition),
    Hotkey(models::hotkeys::HotkeyDefinition),
    Trigger(models::triggers::TriggerDefinition),
    /// A folder of nested children. Folder enable state lives in the package
    /// tree (`packages.json`), not here; the placeholder `bool` mirrors the
    /// loader's shape and is intentionally never read.
    Folder(#[allow(dead_code)] bool, BTreeMap<String, Script>),
}

impl Script {
    /// The folder (package path) this script lives under, if any.
    pub fn folder_name(&self) -> Option<&str> {
        match self {
            Script::Alias(a) => a.package.as_deref(),
            Script::Hotkey(h) => h.package.as_deref(),
            Script::Trigger(t) => t.package.as_deref(),
            Script::Folder(_, _) => None,
        }
    }

    /// This node's own `enabled` flag (folders report `true`).
    pub fn own_enabled(&self) -> bool {
        match self {
            Script::Alias(a) => a.enabled,
            Script::Hotkey(h) => h.enabled,
            Script::Trigger(t) => t.enabled,
            Script::Folder(_, _) => true,
        }
    }
}

/// Identifies a script by its (folder, name) pair for tree selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptKey {
    pub folder_name: Option<String>,
    pub script_name: String,
}

/// A node's at-a-glance health. Drives the colored status dot everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus {
    /// Enabled & healthy (green).
    Ok,
    /// Enabled but broken — e.g. a pattern won't compile (red).
    Error,
    /// Needs attention but not broken (orange/amber) — e.g. an installed package whose newest
    /// version is held back because it demands more permissions than were granted (the update is
    /// blocked until the user reviews + grants it, `PACKAGE-ISOLATES-CONSENT-TRUST.md`).
    Warning,
    /// Turned off; won't run (grey).
    Disabled,
}

/// The per-row pattern type in the unified trigger pattern list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternKind {
    /// A regex that must match the line for the trigger to fire.
    Match,
    /// A regex that must *not* match (inverts).
    Anti,
    /// A raw (verbatim) regex pattern.
    Raw,
}

impl PatternKind {
    pub const ALL: [PatternKind; 3] = [PatternKind::Match, PatternKind::Anti, PatternKind::Raw];

    pub fn label(self) -> &'static str {
        match self {
            PatternKind::Match => crate::i18n::ts!("pattern-kind-match"),
            PatternKind::Anti => crate::i18n::ts!("pattern-kind-anti"),
            PatternKind::Raw => crate::i18n::ts!("pattern-kind-raw"),
        }
    }
}

impl std::fmt::Display for PatternKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Flattens a trigger's three pattern vectors into the unified, ordered row list
/// the editor edits (Match rows first, then Anti, then Raw).
pub fn trigger_rows(trigger: &triggers::TriggerDefinition) -> Vec<(PatternKind, String)> {
    let mut rows = Vec::new();
    if let Some(patterns) = &trigger.patterns {
        rows.extend(patterns.iter().map(|p| (PatternKind::Match, p.clone())));
    }
    if let Some(anti) = &trigger.anti_patterns {
        rows.extend(anti.iter().map(|p| (PatternKind::Anti, p.clone())));
    }
    if let Some(raw) = &trigger.raw_patterns {
        rows.extend(raw.iter().map(|p| (PatternKind::Raw, p.clone())));
    }
    if rows.is_empty() {
        rows.push((PatternKind::Match, String::new()));
    }
    rows
}

/// Rebuilds a trigger's three pattern vectors from the unified row list.
pub fn rows_into_trigger(
    rows: &[(PatternKind, String)],
    trigger: &mut triggers::TriggerDefinition,
) {
    let collect = |kind: PatternKind| -> Option<Vec<String>> {
        let v: Vec<String> = rows
            .iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, s)| s.clone())
            .collect();
        if v.is_empty() { None } else { Some(v) }
    };
    trigger.patterns = collect(PatternKind::Match);
    trigger.anti_patterns = collect(PatternKind::Anti);
    trigger.raw_patterns = collect(PatternKind::Raw);
}

/// The client-side package dependency graph, derived from the lockfile plus
/// each installed package's resolved `dependencies`. Specifiers are the keys.
#[derive(Debug, Clone, Default)]
pub struct PackageGraph {
    /// `specifier -> (range, dep specifiers)` — a package's declared requires.
    pub requires: HashMap<String, Vec<DepEdge>>,
    /// Direct-install intent: the user installed it at top level.
    pub direct: HashSet<String>,
    /// Owned (authored) packages — always directly enabled (their own source).
    pub owned: HashSet<String>,
    /// The user's direct enable intent for controllable packages.
    pub intent: HashMap<String, bool>,
    /// Resolved version per specifier (best-effort, from the last resolve).
    pub resolved: HashMap<String, String>,
}

/// One edge in the requires graph: the dependency specifier + its declared range.
#[derive(Debug, Clone)]
pub struct DepEdge {
    pub specifier: String,
    pub range: String,
}

impl PackageGraph {
    /// Packages whose `requires` include `id`.
    pub fn required_by(&self, id: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .requires
            .iter()
            .filter(|(_, edges)| edges.iter().any(|e| e.specifier == id))
            .map(|(parent, _)| parent.clone())
            .collect();
        out.sort();
        out
    }

    /// A dependency-only package: not directly installed, not owned, but required
    /// by something — its on/off follows its dependents.
    pub fn is_dep_only(&self, id: &str) -> bool {
        !self.direct.contains(id) && !self.owned.contains(id) && !self.required_by(id).is_empty()
    }

    /// Effective-enabled: the user turned it on (or owns it), or some
    /// effectively-enabled dependent needs it. Guards against cycles.
    pub fn effectively_enabled(&self, id: &str) -> bool {
        let mut visited = HashSet::new();
        self.eff(id, &mut visited)
    }

    fn eff(&self, id: &str, visited: &mut HashSet<String>) -> bool {
        if !visited.insert(id.to_string()) {
            return false;
        }
        if self.owned.contains(id) || self.intent.get(id).copied().unwrap_or(false) {
            return true;
        }
        self.required_by(id)
            .iter()
            .any(|parent| self.eff(parent, visited))
    }

    /// The switch is interactive only when nothing else forces it on:
    /// not dep-only, and no *enabled* package currently requires it.
    pub fn controllable(&self, id: &str) -> bool {
        if self.is_dep_only(id) {
            return false;
        }
        !self
            .required_by(id)
            .iter()
            .any(|parent| self.effectively_enabled(parent))
    }

    /// Enabled dependents currently forcing `id` on (for the "Required by …" note).
    pub fn enabled_dependents(&self, id: &str) -> Vec<String> {
        self.required_by(id)
            .into_iter()
            .filter(|parent| self.effectively_enabled(parent))
            .collect()
    }

    /// Whether a dependency row shown UNDER `parent` should read as live. Such a row
    /// exists because `parent` pulls `child` in, so it follows the parent's context:
    /// it greys once `parent` is no longer effectively enabled, rather than reporting
    /// `child`'s global state (which stays on for a separately-installed `child` that
    /// runs on its own — that belongs to `child`'s own row, not this edge). The
    /// operative term is the parent; the `child` term is a guard, since `parent`
    /// requiring `child` already implies [`effectively_enabled`](Self::effectively_enabled)
    /// of `child` whenever the parent is enabled — kept so the predicate is
    /// self-contained for any edge it's asked about.
    pub fn dep_edge_active(&self, parent: &str, child: &str) -> bool {
        self.effectively_enabled(parent) && self.effectively_enabled(child)
    }
}

impl AutomationsWindow {
    /// Loads aliases/triggers/hotkeys into the nested script tree.
    pub(super) fn load_scripts_message(&self) -> Message {
        let mut errors = Vec::new();

        let aliases = aliases::load_aliases(&self.server_name)
            .map_err(|e| errors.push(e.to_string()))
            .unwrap_or_default()
            .into_iter()
            .map(|(name, alias)| (name, Script::Alias(alias)));
        let hotkeys = hotkeys::load_hotkeys(&self.server_name)
            .map_err(|e| errors.push(e.to_string()))
            .unwrap_or_default()
            .into_iter()
            .map(|(name, hotkey)| (name, Script::Hotkey(hotkey)));
        let triggers = triggers::load_triggers(&self.server_name)
            .map_err(|e| errors.push(e.to_string()))
            .unwrap_or_default()
            .into_iter()
            .map(|(name, trigger)| (name, Script::Trigger(trigger)));

        let combined: Vec<(String, Script)> =
            aliases.into_iter().chain(hotkeys).chain(triggers).collect();

        let mut scripts = BTreeMap::new();
        for (name, script) in combined {
            match upsert_script_folder(&mut scripts, script.folder_name()) {
                Ok(folder) => {
                    folder.insert(name, script);
                }
                Err(e) => errors.push(e),
            }
        }
        Message::ScriptsLoaded(scripts, Arc::new(errors))
    }

    /// Adds the folder-tree's folders (incl. empty ones) into the script map so
    /// they render with no scripts inside. Idempotent.
    pub(super) fn merge_folders(&mut self) {
        for path in packages::collect_folder_paths(&self.packages) {
            let _ = upsert_script_folder(&mut self.scripts, Some(&path));
        }
    }

    pub(super) fn serialize_scripts(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut aliases_map = std::collections::HashMap::new();
        let mut hotkeys_map = std::collections::HashMap::new();
        let mut triggers_map = std::collections::HashMap::new();
        collect_scripts(
            &self.scripts,
            &mut aliases_map,
            &mut hotkeys_map,
            &mut triggers_map,
        );
        aliases::save_aliases(&self.server_name, &aliases_map)
            .map_err(|e| crate::i18n::t!("automation-save-aliases-failed", "error" => e.to_string()))?;
        hotkeys::save_hotkeys(&self.server_name, &hotkeys_map)
            .map_err(|e| crate::i18n::t!("automation-save-hotkeys-failed", "error" => e.to_string()))?;
        triggers::save_triggers(&self.server_name, &triggers_map)
            .map_err(|e| crate::i18n::t!("automation-save-triggers-failed", "error" => e.to_string()))?;
        Ok(())
    }

    pub(super) fn script_exists(&self, name: &str) -> bool {
        fn rec(scripts: &BTreeMap<String, Script>, name: &str) -> bool {
            for (script_name, script) in scripts {
                // Case-insensitive: these names become files on disk, and
                // Windows/macOS filesystems treat `Combat` and `combat` as one.
                if models::naming::names_conflict(script_name, name) {
                    return true;
                }
                if let Script::Folder(_, children) = script
                    && rec(children, name)
                {
                    return true;
                }
            }
            false
        }
        rec(&self.scripts, name)
    }

    pub(super) fn remove_script_by_name(&mut self, name: &str) {
        fn rec(scripts: &mut BTreeMap<String, Script>, name: &str) -> bool {
            if scripts.remove(name).is_some() {
                return true;
            }
            for script in scripts.values_mut() {
                if let Script::Folder(_, children) = script
                    && rec(children, name)
                {
                    return true;
                }
            }
            false
        }
        rec(&mut self.scripts, name);
    }

    /// Looks up a leaf script by its (folder, name) key.
    pub(super) fn find_script(&self, key: &ScriptKey) -> Option<Script> {
        fn rec(scripts: &BTreeMap<String, Script>, name: &str) -> Option<Script> {
            for (script_name, script) in scripts {
                if script_name == name && !matches!(script, Script::Folder(_, _)) {
                    return Some(script.clone());
                }
                if let Script::Folder(_, children) = script
                    && let Some(found) = rec(children, name)
                {
                    return Some(found);
                }
            }
            None
        }
        rec(&self.scripts, &key.script_name)
    }

    /// Every folder path in the tree (each `Script::Folder`, nested as a
    /// `/`-joined path), sorted with parents before children. This is the set of
    /// destinations the "move to folder" affordances offer — drawn from the live
    /// script tree (not `packages.json`) so it includes folders that exist only
    /// because a script's `package` field points at them.
    pub(super) fn all_folder_paths(&self) -> Vec<String> {
        fn rec(scripts: &BTreeMap<String, Script>, prefix: &str, out: &mut Vec<String>) {
            for (name, script) in scripts {
                if let Script::Folder(_, children) = script {
                    let path = if prefix.is_empty() {
                        name.clone()
                    } else {
                        format!("{prefix}/{name}")
                    };
                    out.push(path.clone());
                    rec(children, &path, out);
                }
            }
        }
        let mut out = Vec::new();
        rec(&self.scripts, "", &mut out);
        out.sort();
        out
    }

    /// The effective status of a leaf script (its own enable + folder enable + a
    /// compile error). Folders/modules report Ok unless disabled.
    pub(super) fn script_status(&self, script: &Script) -> NodeStatus {
        let folder_enabled = script
            .folder_name()
            .is_none_or(|path| packages::is_package_effectively_enabled(path, &self.packages));
        if !script.own_enabled() || !folder_enabled {
            return NodeStatus::Disabled;
        }
        if script_has_error(script) {
            return NodeStatus::Error;
        }
        NodeStatus::Ok
    }
}

/// Whether a script carries an obvious compile error (a regex that won't build).
pub fn script_has_error(script: &Script) -> bool {
    match script {
        Script::Alias(a) if a.language != models::ScriptLang::Plaintext => false,
        Script::Alias(a) => regex::Regex::new(&a.pattern).is_err() && !a.pattern.is_empty(),
        Script::Trigger(t) => {
            let bad = |v: &Option<Vec<String>>| {
                v.as_ref().is_some_and(|patterns| {
                    patterns
                        .iter()
                        .any(|p| !p.is_empty() && regex::Regex::new(p).is_err())
                })
            };
            bad(&t.patterns) || bad(&t.anti_patterns) || bad(&t.raw_patterns)
        }
        Script::Hotkey(_) | Script::Folder(_, _) => false,
    }
}

/// Walks the tree collecting leaves of each type into flat maps for serialization.
fn collect_scripts(
    scripts: &BTreeMap<String, Script>,
    aliases: &mut std::collections::HashMap<String, models::aliases::AliasDefinition>,
    hotkeys: &mut std::collections::HashMap<String, models::hotkeys::HotkeyDefinition>,
    triggers: &mut std::collections::HashMap<String, models::triggers::TriggerDefinition>,
) {
    for (name, script) in scripts {
        match script {
            Script::Alias(a) => {
                aliases.insert(name.clone(), a.clone());
            }
            Script::Hotkey(h) => {
                hotkeys.insert(name.clone(), h.clone());
            }
            Script::Trigger(t) => {
                triggers.insert(name.clone(), t.clone());
            }
            Script::Folder(_, children) => collect_scripts(children, aliases, hotkeys, triggers),
        }
    }
}

/// Ensures the folder chain `folder_name` exists in `scripts`, returning the
/// innermost folder's child map.
pub fn upsert_script_folder<'a>(
    scripts: &'a mut BTreeMap<String, Script>,
    folder_name: Option<&str>,
) -> Result<&'a mut BTreeMap<String, Script>, String> {
    let mut current = scripts;
    if let Some(folder_name) = folder_name {
        for (i, folder) in folder_name.split('/').enumerate() {
            match current.get(folder) {
                Some(Script::Folder(_, _)) => {}
                Some(_) => {
                    return Err(crate::i18n::t!(
                        "automation-script-folder-invalid",
                        "path" => folder_name.split('/').take(i).collect::<Vec<_>>().join("/")
                    ));
                }
                None => {
                    current.insert(folder.to_string(), Script::Folder(false, BTreeMap::new()));
                }
            }
            current = match current.get_mut(folder) {
                Some(Script::Folder(_, children)) => children,
                _ => return Err(crate::i18n::t!("automation-script-folder-create-failed")),
            };
        }
    }
    Ok(current)
}

/// Parses a `smudgy://owner/name` specifier into `(owner, name)`.
pub fn parse_specifier(specifier: &str) -> Option<(String, String)> {
    let rest = specifier.strip_prefix("smudgy://")?;
    let (owner, name) = rest.rsplit_once('/')?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some((owner.to_string(), name.to_string()))
}

/// A short display label for an installed-package specifier (the trailing name).
pub fn package_display_name(specifier: &str) -> &str {
    specifier.rsplit('/').next().unwrap_or(specifier)
}

/// The specifier a `LockedPackage` would carry for a given owner/name.
pub fn specifier_for(owner: &str, name: &str) -> String {
    format!("smudgy://{owner}/{name}")
}

/// Whether `owner/name` is present in the lockfile list.
pub fn is_installed(installed: &[LockedPackage], owner: &str, name: &str) -> bool {
    let specifier = specifier_for(owner, name);
    installed.iter().any(|p| p.specifier == specifier)
}

#[cfg(test)]
mod tests {
    use super::{DepEdge, PackageGraph};

    /// A directly-installed package: present in the lockfile (so in `direct`) with its own
    /// enable intent — exactly what `rebuild_graph` seeds for each installed entry.
    fn install(graph: &mut PackageGraph, spec: &str, enabled: bool) {
        graph.direct.insert(spec.to_string());
        graph.intent.insert(spec.to_string(), enabled);
    }

    /// Record that `parent` pulls `child` in (a `requires`-graph edge).
    fn imports(graph: &mut PackageGraph, parent: &str, child: &str) {
        graph
            .requires
            .entry(parent.to_string())
            .or_default()
            .push(DepEdge {
                specifier: child.to_string(),
                range: String::new(),
            });
    }

    #[test]
    fn dep_edge_row_greys_when_parent_disabled_but_dep_keeps_its_own_status() {
        // P imports D, and D is also separately installed (its own enabled lockfile entry).
        let mut graph = PackageGraph::default();
        install(&mut graph, "p", true);
        install(&mut graph, "d", true);
        imports(&mut graph, "p", "d");

        assert!(
            graph.dep_edge_active("p", "d"),
            "both on: D's row under P is live"
        );

        // Disable P. D keeps its own enabled entry, so it still runs on its own...
        graph.intent.insert("p".to_string(), false);
        assert!(
            graph.effectively_enabled("d"),
            "D still runs via its own install — its own row stays green",
        );
        // ...but its row UNDER P greys: the import via P is no longer active. This is the bug fix.
        assert!(!graph.dep_edge_active("p", "d"));
    }

    #[test]
    fn dep_edge_row_stays_live_via_another_enabled_requirer() {
        // P and Q both import D (D separately installed too).
        let mut graph = PackageGraph::default();
        install(&mut graph, "p", true);
        install(&mut graph, "q", true);
        install(&mut graph, "d", true);
        imports(&mut graph, "p", "d");
        imports(&mut graph, "q", "d");

        // Disable only P: D's row under P greys, but under still-enabled Q it stays live.
        graph.intent.insert("p".to_string(), false);
        assert!(
            !graph.dep_edge_active("p", "d"),
            "the import via the disabled P is dead"
        );
        assert!(graph.dep_edge_active("q", "d"), "Q still pulls D in");
        assert!(graph.effectively_enabled("d"));
    }

    #[test]
    fn dep_edge_row_follows_parent_for_a_pure_import_dep() {
        // P imports L, which has NO lockfile entry of its own (a pure transitive import).
        let mut graph = PackageGraph::default();
        install(&mut graph, "p", true);
        imports(&mut graph, "p", "l");

        assert!(graph.is_dep_only("l"));
        assert!(
            graph.dep_edge_active("p", "l"),
            "L is live while P pulls it in"
        );

        // Disable P: nothing else needs L, so both its global state and its row go inactive.
        graph.intent.insert("p".to_string(), false);
        assert!(!graph.effectively_enabled("l"));
        assert!(!graph.dep_edge_active("p", "l"));
    }
}
