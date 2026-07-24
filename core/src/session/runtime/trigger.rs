use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use anyhow::{Result, bail};
use regex::{Regex, RegexSet};

use super::{
    ActionQueue, ScriptAction,
    matcher::PatternSet,
    origin::{
        AutomationBody, AutomationDelta, AutomationKind, AutomationSummary, IsolateId, Origin,
    },
};

/// One automation's introspectable state for the `session.triggers`/`session.aliases`
/// registries. Mirrors what the JS handle exposes: its `enabled` flag and its
/// read-back `pattern` (the first pattern's source). Refreshed by the [`Manager`] on every
/// add/enable/remove so the synchronous introspection ops can read it without crossing into
/// the (non-`OpState`) [`Manager`].
#[derive(Clone, Debug)]
pub struct AutomationEntry {
    pub enabled: bool,
    pub pattern: String,
    pub priority: i32,
    pub fallthrough: bool,
}

/// `name -> entry` within one `(IsolateId, Origin)` namespace.
type AutomationNamespace = HashMap<String, AutomationEntry>;

/// Stop state for one alias/trigger dispatch, partitioned by creator so one package cannot
/// suppress another package's (or the user's) automations.
type FallthroughScopes = HashMap<(IsolateId, Origin), Arc<AtomicBool>>;

/// The introspection mirror the `get`/`list`/`exists` ops read. Keyed by
/// `(IsolateId, Origin)` exactly like the [`Manager`]'s own indices, so a caller only ever
/// sees its OWN `(isolate, origin)` automations (origin-scoped). Shared (the same
/// `Rc`) into every isolate's ops at construction; the [`Manager`] owns the write side and
/// keeps it consistent with its `Vec`s.
#[derive(Default, Debug)]
pub struct AutomationRegistry {
    pub aliases: HashMap<(IsolateId, Origin), AutomationNamespace>,
    pub triggers: HashMap<(IsolateId, Origin), AutomationNamespace>,
}

/// The shared introspection mirror handed to both the [`Manager`] (writer) and the ops
/// (readers). A fresh one is built per engine, so a reload clears it.
pub type SharedAutomationRegistry = Rc<RefCell<AutomationRegistry>>;
use crate::session::{
    runtime::{
        RuntimeAction,
        script_engine::{FunctionId, ScriptId},
    },
    styled_line::StyledLine,
};

#[derive(Debug)]
pub struct Manager {
    spawned_actions: ActionQueue,
    triggers: Vec<Trigger>,
    aliases: Vec<Trigger>,
    trigger_regex_set_map: Vec<usize>, // Maps index in PatternSet to index in triggers
    trigger_regex_patterns_map: Vec<usize>,
    trigger_regex_set: PatternSet,
    raw_trigger_regex_set_map: Vec<usize>,
    raw_trigger_regex_patterns_map: Vec<usize>,
    raw_trigger_regex_set: PatternSet,
    prompt_trigger_regex_set_map: Vec<usize>,
    prompt_trigger_regex_patterns_map: Vec<usize>,
    prompt_trigger_regex_set: PatternSet,
    prompt_raw_trigger_regex_set_map: Vec<usize>,
    prompt_raw_trigger_regex_patterns_map: Vec<usize>,
    prompt_raw_trigger_regex_set: PatternSet,
    alias_regex_set_map: Vec<usize>,
    alias_regex_patterns_map: Vec<usize>,
    alias_regex_set: PatternSet,
    // Keyed by `(IsolateId, Origin)`: the isolate dimension (see `PACKAGE-ISOLATES.md`) lets
    // the *same* `(origin, name)` automation coexist across isolates — e.g. a package loaded
    // both in `Main` and in its own sandbox registers two namespaces instead of clobbering
    // via upsert.
    trigger_indices: HashMap<(IsolateId, Origin), HashMap<String, usize>>,
    alias_indices: HashMap<(IsolateId, Origin), HashMap<String, usize>>,
    /// Indices into `triggers` of every trigger that declares a `line_limit`. A side list so the
    /// per-incoming-line `count_tested_lines` self-limit tick visits only the (rare) line-limited
    /// triggers instead of scanning all of them — keeping the common no-line-limit profile O(1)
    /// per line rather than O(trigger-count). Recomputed in
    /// [`rebuild_trigger_regex_set`](Self::rebuild_trigger_regex_set), the same dirty-gated point
    /// the trigger `PatternSet`s rebuild, so it never holds stale indices.
    line_limited_triggers: Vec<usize>,
    trigger_regex_set_dirty: bool,
    alias_regex_set_dirty: bool,
    command_separator: Arc<String>,
    /// While ≥1 window is subscribed (the runtime sets this from the automation broadcast's
    /// receiver count, so it covers any number of windows), each add/enable on a
    /// script-created (non-`User`) automation records an [`AutomationDelta`] here; the
    /// runtime flushes them at its queue-drain point. Empty and unrecorded otherwise.
    recording: bool,
    automation_deltas: Vec<AutomationDelta>,
    /// Introspection mirror shared with the `get`/`list`/`exists` ops. The `Manager`
    /// is the sole writer; it refreshes the entry on every add/enable/remove so a synchronous
    /// op read sees the live `enabled`/`pattern`.
    automation_registry: SharedAutomationRegistry,
    /// Whether any trigger (enabled or not) carries a raw pattern. Shared with the
    /// connection's [`VtProcessor`], which captures `StyledLine::raw` — a per-line
    /// lossy copy of the wire bytes whose only consumer is raw matching — only while
    /// this is set. Kept true across enable/disable so a disabled raw trigger's
    /// re-enable never races the capture of in-flight lines.
    raw_wanted: Arc<AtomicBool>,
}

/// Feature-gated observation handle for trigger benchmarks.
///
/// The runtime action queue remains an internal implementation detail; benches
/// only need to count, clear, and test whether trigger actions were emitted.
#[cfg(feature = "bench-api")]
#[derive(Clone, Debug)]
pub struct BenchActionQueue(ActionQueue);

#[cfg(feature = "bench-api")]
impl BenchActionQueue {
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.borrow().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }

    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }
}

/// A single regex capture group from a trigger/alias match.
///
/// Captures are carried as an **ordered** `Vec<MatchCapture>`: position in the vec *is*
/// the group number (`captures[0]` is the whole match, `captures[1..]` the parenthesized
/// groups in pattern order). `name` is `Some` only for named groups (`(?<name>…)`). A
/// group that did not participate in the match has an empty `value`.
///
/// `MatchCapture` also carries host-routed interop deliveries (event/watch/procedure
/// captures), whose names are the fixed literals `event`/`payload`/`path`/`snapshot`/
/// `sender`. `name` is a `Cow` to serve both producers: trigger captures own their
/// dynamic, author-written group names; interop deliveries borrow their literals with no
/// per-delivery allocation.
#[derive(Debug, Clone)]
pub struct MatchCapture {
    /// The named-group name (`(?<name>…)`) or an interop capture's literal name; `None`
    /// for an unnamed group.
    pub name: Option<std::borrow::Cow<'static, str>>,
    /// The matched text, or empty when the group did not participate.
    pub value: String,
}

/// Expands a bash-style inline template against an ordered list of [`MatchCapture`]s in a
/// single left-to-right tokenizing pass.
///
/// Grammar (see the `JSDoc` in `js/smudgy.js` for the user-facing contract):
/// - `${N}` / `${name}` — braced reference; `N` is a (multi-digit) group number,
///   `name` an identifier resolving a named group.
/// - `$N` — a **single** digit group reference (so `$10` is group 1 then a literal `0`;
///   use `${10}` for group ten).
/// - `$name` — an identifier (`[A-Za-z_][A-Za-z0-9_]*`) resolving a named group.
/// - `$$` — a literal `$`.
/// - A `$` not starting any of the above is emitted literally.
///
/// Unknown / empty / non-participating groups expand to the empty string.
#[must_use]
pub fn expand_template(template: &str, captures: &[MatchCapture]) -> String {
    let lookup_index = |idx: usize| -> &str { captures.get(idx).map_or("", |c| c.value.as_str()) };
    let lookup_name = |name: &str| -> &str {
        captures
            .iter()
            .find(|c| c.name.as_deref() == Some(name))
            .map_or("", |c| c.value.as_str())
    };

    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            // Advance one full UTF-8 char (templates may contain non-ASCII text).
            let ch = template[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        // We are at a `$`.
        let next = bytes.get(i + 1).copied();
        match next {
            Some(b'$') => {
                out.push('$');
                i += 2;
            }
            Some(b'{') => {
                // `${...}` — scan to the closing brace.
                if let Some(rel_close) = template[i + 2..].find('}') {
                    let inner = &template[i + 2..i + 2 + rel_close];
                    if inner.chars().all(|c| c.is_ascii_digit()) && !inner.is_empty() {
                        if let Ok(idx) = inner.parse::<usize>() {
                            out.push_str(lookup_index(idx));
                        }
                    } else {
                        out.push_str(lookup_name(inner));
                    }
                    i += 2 + rel_close + 1;
                } else {
                    // No closing brace: emit the `$` literally and continue past it.
                    out.push('$');
                    i += 1;
                }
            }
            Some(b'0'..=b'9') => {
                // Single-digit group reference.
                let digit = (next.unwrap() - b'0') as usize;
                out.push_str(lookup_index(digit));
                i += 2;
            }
            Some(c) if c == b'_' || c.is_ascii_alphabetic() => {
                // `$identifier` — consume the identifier run.
                let start = i + 1;
                let mut end = start;
                while end < bytes.len()
                    && (bytes[end] == b'_' || bytes[end].is_ascii_alphanumeric())
                {
                    end += 1;
                }
                let name = &template[start..end];
                out.push_str(lookup_name(name));
                i = end;
            }
            _ => {
                // Lone `$` (end of string or followed by something inert): literal.
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

/// Splits an outgoing chunk into commands: always on '\n', additionally on
/// `separator` when it is non-empty.
#[must_use]
pub fn split_commands<'a>(text: &'a str, separator: &str) -> Vec<&'a str> {
    if separator.is_empty() {
        text.split('\n').collect()
    } else {
        text.split('\n')
            .flat_map(|chunk| chunk.split(separator))
            .collect()
    }
}

#[derive(Clone, Copy)]
enum TriggerMatchType {
    Normal,
    Raw,
}

pub struct PushTriggerParams<'a> {
    pub isolate: IsolateId,
    pub origin: Origin,
    pub name: &'a Arc<String>,
    pub patterns: &'a Arc<Vec<String>>,
    pub raw_patterns: &'a Arc<Vec<String>>,
    pub anti_patterns: &'a Arc<Vec<String>>,
    pub action: ScriptAction,
    pub prompt: bool,
    pub enabled: bool,
    pub priority: i32,
    pub fallthrough: bool,
    pub fire_limit: Option<u32>,
    pub line_limit: Option<u32>,
    /// Display-only body source for the read-only detail pane: the JS/TS eval string, or a
    /// function's `toString()`. `None` for plaintext (the command is recoverable from
    /// `action`) or when no source was supplied.
    pub source: Option<Arc<str>>,
}

impl Manager {
    pub(crate) fn new(
        spawned_actions: ActionQueue,
        command_separator: Arc<String>,
        automation_registry: SharedAutomationRegistry,
    ) -> Self {
        let triggers = Vec::new();
        let aliases = Vec::new();
        let trigger_indices = HashMap::new();
        let alias_indices = HashMap::new();
        let trigger_regex_set = PatternSet::empty();
        let raw_trigger_regex_set = PatternSet::empty();
        let prompt_trigger_regex_set = PatternSet::empty();
        let prompt_raw_trigger_regex_set = PatternSet::empty();
        let alias_regex_set = PatternSet::empty();

        Self {
            alias_regex_set,
            trigger_regex_set,
            raw_trigger_regex_set,
            prompt_trigger_regex_set,
            prompt_raw_trigger_regex_set,
            alias_regex_set_map: Vec::new(),
            trigger_regex_set_map: Vec::new(),
            raw_trigger_regex_set_map: Vec::new(),
            prompt_trigger_regex_set_map: Vec::new(),
            prompt_raw_trigger_regex_set_map: Vec::new(),
            alias_regex_patterns_map: Vec::new(),
            trigger_regex_patterns_map: Vec::new(),
            raw_trigger_regex_patterns_map: Vec::new(),
            prompt_trigger_regex_patterns_map: Vec::new(),
            prompt_raw_trigger_regex_patterns_map: Vec::new(),
            aliases,
            triggers,
            alias_indices,
            trigger_indices,
            line_limited_triggers: Vec::new(),
            spawned_actions,
            trigger_regex_set_dirty: true,
            alias_regex_set_dirty: true,
            command_separator,
            recording: false,
            automation_deltas: Vec::new(),
            automation_registry,
            raw_wanted: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Construct the real trigger manager plus its feature-gated queue
    /// observation handle for workspace benchmarks.
    #[cfg(feature = "bench-api")]
    #[must_use]
    pub fn new_for_bench(
        command_separator: Arc<String>,
        automation_registry: SharedAutomationRegistry,
    ) -> (Self, BenchActionQueue) {
        let spawned_actions: ActionQueue = Rc::new(RefCell::default());
        let queue = BenchActionQueue(spawned_actions.clone());
        (
            Self::new(spawned_actions, command_separator, automation_registry),
            queue,
        )
    }

    /// The shared "any trigger has a raw pattern" flag, for wiring into the
    /// connection's raw-byte capture.
    #[must_use]
    pub fn raw_wanted_flag(&self) -> Arc<AtomicBool> {
        self.raw_wanted.clone()
    }

    /// Continue writing to a predecessor manager's flag cell instead of this
    /// manager's own. A reload rebuilds the manager but keeps the connection —
    /// and with it the `VtProcessor`'s clone of the old cell — alive; adopting
    /// keeps that clone live. Syncs the cell to this manager's (empty) trigger
    /// set; the reloading modules' re-registrations raise it again.
    pub fn adopt_raw_wanted_flag(&mut self, flag: Arc<AtomicBool>) {
        self.raw_wanted = flag;
        self.refresh_raw_wanted();
    }

    /// Recompute [`Self::raw_wanted`] after a trigger mutation. Runs eagerly (not at the
    /// dirty-gated `PatternSet` rebuild) so capture starts before the next line arrives,
    /// not after it.
    fn refresh_raw_wanted(&self) {
        let wanted = self
            .triggers
            .iter()
            .any(|trigger| !trigger.raw_patterns.is_empty());
        self.raw_wanted.store(wanted, Ordering::Relaxed);
    }

    /// Set by the runtime from the automation broadcast's receiver count: record deltas
    /// while ≥1 window is subscribed. Turning recording off drops any buffered deltas (the
    /// next subscriber gets a fresh reset first).
    pub fn set_recording(&mut self, on: bool) {
        if !on && self.recording {
            self.automation_deltas.clear();
        }
        self.recording = on;
    }

    /// Whether any automations window is subscribed (gates delta recording).
    fn is_watched(&self) -> bool {
        self.recording
    }

    /// The current full set of script-created (non-`User`) automations, for the reset a
    /// window receives when it starts watching. User/disk automations are shown from disk
    /// and scripts can't touch the user namespace, so they're excluded.
    pub fn automation_reset(&self) -> Vec<AutomationSummary> {
        let aliases = self
            .aliases
            .iter()
            .filter(|item| item.origin != Origin::User)
            .map(|item| Self::summary(AutomationKind::Alias, item));
        let triggers = self
            .triggers
            .iter()
            .filter(|item| item.origin != Origin::User)
            .map(|item| Self::summary(AutomationKind::Trigger, item));
        aliases.chain(triggers).collect()
    }

    /// Whether there are buffered deltas to flush (checked at the runtime drain point).
    pub fn has_automation_deltas(&self) -> bool {
        !self.automation_deltas.is_empty()
    }

    /// Drains the buffered deltas for the runtime to emit.
    pub fn take_automation_deltas(&mut self) -> Vec<AutomationDelta> {
        std::mem::take(&mut self.automation_deltas)
    }

    fn summary(kind: AutomationKind, item: &Trigger) -> AutomationSummary {
        AutomationSummary {
            kind,
            origin: item.origin.clone(),
            name: item.name.clone(),
            enabled: item.enabled,
            pattern: Self::pattern_display(item),
            body: Self::body_display(item),
        }
    }

    /// The match pattern(s) joined into one display string: regex sources for the match
    /// patterns first, then the raw patterns, ` | `-separated. Empty when there are none.
    fn pattern_display(item: &Trigger) -> Arc<str> {
        let mut out = String::new();
        for re in item.patterns.iter().chain(item.raw_patterns.iter()) {
            if !out.is_empty() {
                out.push_str(" | ");
            }
            out.push_str(re.as_str());
        }
        Arc::from(out)
    }

    /// What the automation does, for the read-only detail pane. Prefers the captured `source`
    /// (eval string / function `toString()`); for plaintext the command is recovered from the
    /// `ScriptAction` itself.
    fn body_display(item: &Trigger) -> AutomationBody {
        match &item.script {
            ScriptAction::SendRaw(s) | ScriptAction::SendSimple(s) => AutomationBody::Command(
                item.source.clone().unwrap_or_else(|| Arc::from(s.as_str())),
            ),
            ScriptAction::EvalJavascript(_) | ScriptAction::CallJavascriptFunction(_) => {
                AutomationBody::Script(item.source.clone())
            }
            ScriptAction::Noop => AutomationBody::Noop,
        }
    }

    /// Replaces the separator used to split plaintext alias/trigger bodies
    /// into commands. Used by the `ApplySettings` handler for live updates.
    pub fn set_command_separator(&mut self, separator: Arc<String>) {
        self.command_separator = separator;
    }

    /// The pattern source the JS `.pattern` handle reads back: the first pattern's regex
    /// source, or empty when an automation has none.
    fn pattern_of(item: &Trigger) -> String {
        item.patterns
            .first()
            .map_or_else(String::new, |re| re.as_str().to_string())
    }

    /// Mirror one automation into the shared introspection registry. `kind` selects the
    /// alias/trigger map; the entry is keyed by `(isolate, origin)` then name.
    fn registry_upsert(&self, kind: AutomationKind, item: &Trigger) {
        let entry = AutomationEntry {
            enabled: item.enabled,
            pattern: Self::pattern_of(item),
            priority: item.priority,
            fallthrough: item.fallthrough,
        };
        let key = (item.isolate.clone(), item.origin.clone());
        let mut registry = self.automation_registry.borrow_mut();
        // The introspection mirror tracks only aliases/triggers; hotkeys are keyed for
        // origin-scoping but live in dispatch's own `HotkeyId` map, never reaching this helper.
        let map = match kind {
            AutomationKind::Alias => &mut registry.aliases,
            AutomationKind::Trigger => &mut registry.triggers,
            AutomationKind::Hotkey => return,
        };
        map.entry(key).or_default().insert(item.name.clone(), entry);
    }

    /// Drop one automation from the shared introspection registry (on remove).
    fn registry_remove(
        &self,
        kind: AutomationKind,
        isolate: &IsolateId,
        origin: &Origin,
        name: &str,
    ) {
        let mut registry = self.automation_registry.borrow_mut();
        let map = match kind {
            AutomationKind::Alias => &mut registry.aliases,
            AutomationKind::Trigger => &mut registry.triggers,
            AutomationKind::Hotkey => return,
        };
        if let Some(namespace) = map.get_mut(&(isolate.clone(), origin.clone())) {
            namespace.remove(name);
        }
    }

    /// Flip one automation's `enabled` in the shared introspection registry (on enable/disable).
    fn registry_set_enabled(
        &self,
        kind: AutomationKind,
        isolate: &IsolateId,
        origin: &Origin,
        name: &str,
        enabled: bool,
    ) {
        let mut registry = self.automation_registry.borrow_mut();
        let map = match kind {
            AutomationKind::Alias => &mut registry.aliases,
            AutomationKind::Trigger => &mut registry.triggers,
            AutomationKind::Hotkey => return,
        };
        if let Some(entry) = map
            .get_mut(&(isolate.clone(), origin.clone()))
            .and_then(|namespace| namespace.get_mut(name))
        {
            entry.enabled = enabled;
        }
    }

    fn add_or_update_alias(&mut self, alias: Trigger) {
        debug!(
            "Adding or updating alias: {:?}, {:?}, {:?}",
            alias.origin, alias.name, alias.patterns
        );
        self.registry_upsert(AutomationKind::Alias, &alias);
        let delta = (self.is_watched() && alias.origin != Origin::User)
            .then(|| AutomationDelta::Upserted(Self::summary(AutomationKind::Alias, &alias)));
        // Keyed by (isolate, origin, name): re-creating the same alias in the same isolate
        // upserts in place, while a same-named alias from a different origin OR a different
        // isolate coexists.
        let key = (alias.isolate.clone(), alias.origin.clone());
        if let Some(index) = self
            .alias_indices
            .get(&key)
            .and_then(|by_name| by_name.get(&alias.name))
            .copied()
        {
            *self.aliases.get_mut(index).unwrap() = alias;
        } else {
            let index = self.aliases.len();
            self.alias_indices
                .entry(key)
                .or_default()
                .insert(alias.name.clone(), index);
            self.aliases.push(alias);
        }
        // Defer the (expensive) PatternSet rebuild to the next outgoing line,
        // exactly like triggers do via `trigger_regex_set_dirty`. Rebuilding
        // eagerly on every insert made loading N aliases O(N²) — and since each
        // rebuild recompiles the aho-corasick automaton + regexes (far slower in
        // debug builds), a large profile/package alias set could stall the
        // runtime for tens of seconds at session start, delaying `Connect`.
        self.alias_regex_set_dirty = true;
        if let Some(delta) = delta {
            self.automation_deltas.push(delta);
        }
    }

    fn add_or_update_trigger(&mut self, trigger: Trigger) {
        trace!(
            "Adding or updating trigger: {:?}, {:?}",
            trigger.name, trigger.patterns
        );
        self.registry_upsert(AutomationKind::Trigger, &trigger);
        let delta = (self.is_watched() && trigger.origin != Origin::User)
            .then(|| AutomationDelta::Upserted(Self::summary(AutomationKind::Trigger, &trigger)));
        let key = (trigger.isolate.clone(), trigger.origin.clone());
        if let Some(index) = self
            .trigger_indices
            .get(&key)
            .and_then(|by_name| by_name.get(&trigger.name))
            .copied()
        {
            *self.triggers.get_mut(index).unwrap() = trigger;
        } else {
            let index = self.triggers.len();
            self.trigger_indices
                .entry(key)
                .or_default()
                .insert(trigger.name.clone(), index);
            self.triggers.push(trigger);
        }

        self.trigger_regex_set_dirty = true;
        self.refresh_raw_wanted();
        if let Some(delta) = delta {
            self.automation_deltas.push(delta);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn push_javascript_alias(
        &mut self,
        isolate: IsolateId,
        origin: Origin,
        name: &Arc<String>,
        patterns: &Arc<Vec<String>>,
        script_id: ScriptId,
        priority: i32,
        fallthrough: bool,
        fire_limit: Option<u32>,
        source: Option<Arc<str>>,
    ) -> Result<()> {
        self.add_or_update_alias(
            Trigger::new_alias(
                isolate,
                origin,
                name.to_string(),
                patterns.iter(),
                ScriptAction::EvalJavascript(script_id),
                priority,
                fallthrough,
                fire_limit,
            )?
            .with_source(source),
        );
        Ok(())
    }

    pub fn push_trigger(&mut self, params: PushTriggerParams) -> Result<()> {
        self.add_or_update_trigger(
            Trigger::new(
                params.isolate,
                params.origin,
                params.name.to_string(),
                params.patterns.iter(),
                params.raw_patterns.iter(),
                params.anti_patterns.iter(),
                params.action,
                params.prompt,
                params.enabled,
                params.priority,
                params.fallthrough,
                params.fire_limit,
                params.line_limit,
            )?
            .with_source(params.source),
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn push_javascript_function_alias(
        &mut self,
        isolate: IsolateId,
        origin: Origin,
        name: Arc<String>,
        patterns: Arc<Vec<String>>,
        function_id: FunctionId,
        priority: i32,
        fallthrough: bool,
        fire_limit: Option<u32>,
        source: Option<Arc<str>>,
    ) -> Result<()> {
        self.add_or_update_alias(
            Trigger::new_alias(
                isolate,
                origin,
                name.to_string(),
                patterns.iter(),
                ScriptAction::CallJavascriptFunction(function_id),
                priority,
                fallthrough,
                fire_limit,
            )?
            .with_source(source),
        );
        Ok(())
    }

    pub fn push_simple_alias(
        &mut self,
        isolate: IsolateId,
        origin: Origin,
        name: Arc<String>,
        patterns: Arc<Vec<String>>,
        script: Arc<String>,
        priority: i32,
        fallthrough: bool,
        fire_limit: Option<u32>,
    ) -> Result<()> {
        self.add_or_update_alias(Trigger::new_alias(
            isolate,
            origin,
            name.to_string(),
            patterns.iter(),
            ScriptAction::SendSimple(script),
            priority,
            fallthrough,
            fire_limit,
        )?);
        Ok(())
    }

    pub fn enable_alias(
        &mut self,
        isolate: &IsolateId,
        origin: &Origin,
        name: &str,
        enabled: bool,
    ) {
        let mut changed = false;
        if let Some(index) = self
            .alias_indices
            .get(&(isolate.clone(), origin.clone()))
            .and_then(|by_name| by_name.get(name))
            .copied()
            && let Some(alias) = self.aliases.get_mut(index)
        {
            trace!(
                "{} alias: {:?}, {:?}",
                if enabled { "Enabling" } else { "Disabling" },
                alias.name,
                alias.patterns
            );
            alias.enabled = enabled;
            changed = true;
        }
        if changed {
            self.registry_set_enabled(AutomationKind::Alias, isolate, origin, name, enabled);
        }
        if changed && self.is_watched() && *origin != Origin::User {
            self.automation_deltas
                .push(AutomationDelta::EnabledChanged {
                    kind: AutomationKind::Alias,
                    origin: origin.clone(),
                    name: name.to_string(),
                    enabled,
                });
        }
    }

    pub fn enable_trigger(
        &mut self,
        isolate: &IsolateId,
        origin: &Origin,
        name: &str,
        enabled: bool,
    ) {
        let mut changed = false;
        if let Some(index) = self
            .trigger_indices
            .get(&(isolate.clone(), origin.clone()))
            .and_then(|by_name| by_name.get(name))
            .copied()
            && let Some(trigger) = self.triggers.get_mut(index)
        {
            trace!(
                "{} trigger: {:?}, {:?}",
                if enabled { "Enabling" } else { "Disabling" },
                trigger.name,
                trigger.patterns
            );
            trigger.enabled = enabled;
            changed = true;
        }
        if changed {
            self.registry_set_enabled(AutomationKind::Trigger, isolate, origin, name, enabled);
        }
        if changed && self.is_watched() && *origin != Origin::User {
            self.automation_deltas
                .push(AutomationDelta::EnabledChanged {
                    kind: AutomationKind::Trigger,
                    origin: origin.clone(),
                    name: name.to_string(),
                    enabled,
                });
        }
    }

    /// Remove an alias by its `(isolate, origin, name)` key: drop it from the `Vec`,
    /// rebuild the name→index map and the alias `PatternSet` (so its matcher slot is actually
    /// freed — leaving `enabled=false` would keep it resident), drop its introspection-registry
    /// entry, and emit a [`AutomationDelta::Removed`] for the watching UI. A no-op if the key
    /// is unknown (e.g. a double `delete()`).
    pub fn remove_alias(&mut self, isolate: &IsolateId, origin: &Origin, name: &str) {
        if Self::remove_named(
            &mut self.aliases,
            &mut self.alias_indices,
            isolate,
            origin,
            name,
        ) {
            self.alias_regex_set_dirty = true;
            self.registry_remove(AutomationKind::Alias, isolate, origin, name);
            if self.is_watched() && *origin != Origin::User {
                self.automation_deltas.push(AutomationDelta::Removed {
                    kind: AutomationKind::Alias,
                    origin: origin.clone(),
                    name: name.to_string(),
                });
            }
        }
    }

    /// Remove a trigger by its `(isolate, origin, name)` key: the trigger counterpart of
    /// [`remove_alias`](Self::remove_alias). Marks every trigger `PatternSet` dirty so the slot
    /// is freed across the normal/raw/prompt tiers.
    pub fn remove_trigger(&mut self, isolate: &IsolateId, origin: &Origin, name: &str) {
        if Self::remove_named(
            &mut self.triggers,
            &mut self.trigger_indices,
            isolate,
            origin,
            name,
        ) {
            self.trigger_regex_set_dirty = true;
            self.refresh_raw_wanted();
            self.registry_remove(AutomationKind::Trigger, isolate, origin, name);
            if self.is_watched() && *origin != Origin::User {
                self.automation_deltas.push(AutomationDelta::Removed {
                    kind: AutomationKind::Trigger,
                    origin: origin.clone(),
                    name: name.to_string(),
                });
            }
        }
    }

    /// Remove `name` from a `Vec<Trigger>` + its `(isolate, origin) -> name -> index` map,
    /// keeping the remaining indices consistent. `Vec::remove` shifts every later element down
    /// one, so after the removal we rebuild the map from the surviving entries (cheap relative to
    /// the `PatternSet` recompile the caller defers anyway). Returns whether anything was removed.
    fn remove_named(
        items: &mut Vec<Trigger>,
        indices: &mut HashMap<(IsolateId, Origin), HashMap<String, usize>>,
        isolate: &IsolateId,
        origin: &Origin,
        name: &str,
    ) -> bool {
        let key = (isolate.clone(), origin.clone());
        let Some(index) = indices
            .get(&key)
            .and_then(|by_name| by_name.get(name))
            .copied()
        else {
            return false;
        };
        items.remove(index);
        // The `Vec::remove` shifted later items down one, invalidating every stored index past
        // `index`. Rebuild the whole name→index map from the surviving `Vec` order.
        indices.clear();
        for (i, item) in items.iter().enumerate() {
            indices
                .entry((item.isolate.clone(), item.origin.clone()))
                .or_default()
                .insert(item.name.clone(), i);
        }
        true
    }

    ///
    /// Builds pattern sets for triggers, raw triggers, prompt triggers, and raw prompt triggers
    ///
    /// This could be heavily DRY-ed up, but it just needs to create, for each type of trigger:
    ///  - a `PatternSet` to test when that type of trigger is being tested
    ///  - a `Vec<usize>` to map the indices of the `PatternSet` to the indices of the triggers
    ///  - a `Vec<usize>` to map the indices of the `PatternSet` to the indices of the patterns
    fn rebuild_trigger_regex_set(&mut self) {
        let start = std::time::Instant::now();

        let mut priority_order: Vec<usize> = (0..self.triggers.len()).collect();
        // `sort_by` is stable: equal-priority automations retain their registration order.
        priority_order.sort_by(|&a, &b| self.triggers[b].priority.cmp(&self.triggers[a].priority));

        self.trigger_regex_set = PatternSet::build(
            priority_order
                .iter()
                .flat_map(|&i| self.triggers[i].patterns.iter().map(regex::Regex::as_str)),
        )
        .unwrap();

        self.trigger_regex_set_map = priority_order
            .iter()
            .flat_map(|&i| {
                let trigger = &self.triggers[i];
                let mut v = Vec::with_capacity(trigger.patterns.len());
                for _ in 0..trigger.patterns.len() {
                    v.push(i);
                }
                v
            })
            .collect();
        self.trigger_regex_patterns_map = priority_order
            .iter()
            .flat_map(|&i| {
                self.triggers[i]
                    .patterns
                    .iter()
                    .enumerate()
                    .map(|(i, _pattern)| i)
            })
            .collect();

        self.raw_trigger_regex_set = PatternSet::build(priority_order.iter().flat_map(|&i| {
            self.triggers[i]
                .raw_patterns
                .iter()
                .map(regex::Regex::as_str)
        }))
        .unwrap();
        self.raw_trigger_regex_set_map = priority_order
            .iter()
            .flat_map(|&i| {
                let trigger = &self.triggers[i];
                let mut v = Vec::with_capacity(trigger.raw_patterns.len());
                for _ in 0..trigger.raw_patterns.len() {
                    v.push(i);
                }
                v
            })
            .collect();
        self.raw_trigger_regex_patterns_map = priority_order
            .iter()
            .flat_map(|&i| {
                self.triggers[i]
                    .raw_patterns
                    .iter()
                    .enumerate()
                    .map(|(i, _pattern)| i)
            })
            .collect();

        self.prompt_trigger_regex_set = PatternSet::build(
            priority_order
                .iter()
                .filter(|&&i| self.triggers[i].fire_on_prompts())
                .flat_map(|&i| self.triggers[i].patterns.iter().map(regex::Regex::as_str)),
        )
        .unwrap();
        self.prompt_trigger_regex_set_map = priority_order
            .iter()
            .filter(|&&i| self.triggers[i].fire_on_prompts())
            .flat_map(|&i| {
                let trigger = &self.triggers[i];
                let mut v = Vec::with_capacity(trigger.patterns.len());
                for _ in 0..trigger.patterns.len() {
                    v.push(i);
                }
                v
            })
            .collect();
        self.prompt_trigger_regex_patterns_map = priority_order
            .iter()
            .filter(|&&i| self.triggers[i].fire_on_prompts())
            .flat_map(|&i| {
                self.triggers[i]
                    .patterns
                    .iter()
                    .enumerate()
                    .map(|(i, _pattern)| i)
            })
            .collect();

        self.prompt_raw_trigger_regex_set = PatternSet::build(
            priority_order
                .iter()
                .filter(|&&i| self.triggers[i].fire_on_prompts())
                .flat_map(|&i| {
                    self.triggers[i]
                        .raw_patterns
                        .iter()
                        .map(regex::Regex::as_str)
                }),
        )
        .unwrap();
        self.prompt_raw_trigger_regex_set_map = priority_order
            .iter()
            .filter(|&&i| self.triggers[i].fire_on_prompts())
            .flat_map(|&i| {
                let trigger = &self.triggers[i];
                let mut v = Vec::with_capacity(trigger.raw_patterns.len());
                for _ in 0..trigger.raw_patterns.len() {
                    v.push(i);
                }
                v
            })
            .collect();
        self.prompt_raw_trigger_regex_patterns_map = priority_order
            .iter()
            .filter(|&&i| self.triggers[i].fire_on_prompts())
            .flat_map(|&i| {
                self.triggers[i]
                    .raw_patterns
                    .iter()
                    .enumerate()
                    .map(|(i, _pattern)| i)
            })
            .collect();

        // The only triggers `count_tested_lines` must visit per line; recomputed here, the
        // dirty-gated rebuild point, so it tracks the trigger `Vec` without per-mutation upkeep.
        self.rebuild_line_limited_triggers();

        debug!("Time to rebuild trigger regex sets: {:?}", start.elapsed());
    }

    /// Recompute [`line_limited_triggers`](Self::line_limited_triggers) from the current trigger
    /// `Vec`: the indices whose `line_limit` is set. See the field docs for why this is a side
    /// list rather than a per-line scan.
    fn rebuild_line_limited_triggers(&mut self) {
        self.line_limited_triggers = self
            .triggers
            .iter()
            .enumerate()
            .filter(|(_, trigger)| trigger.line_limit.is_some())
            .map(|(i, _)| i)
            .collect();
    }

    fn rebuild_alias_regex_set(&mut self) {
        let mut priority_order: Vec<usize> = (0..self.aliases.len()).collect();
        priority_order.sort_by(|&a, &b| self.aliases[b].priority.cmp(&self.aliases[a].priority));

        self.alias_regex_set = PatternSet::build(
            priority_order
                .iter()
                .flat_map(|&i| self.aliases[i].patterns.iter().map(regex::Regex::as_str)),
        )
        .unwrap();
        self.alias_regex_set_map = priority_order
            .iter()
            .flat_map(|&i| {
                let alias = &self.aliases[i];
                let mut v = Vec::with_capacity(alias.patterns.len());
                for _ in 0..alias.patterns.len() {
                    v.push(i);
                }
                v
            })
            .collect();
        self.alias_regex_patterns_map = priority_order
            .iter()
            .flat_map(|&i| {
                self.aliases[i]
                    .patterns
                    .iter()
                    .enumerate()
                    .map(|(i, _pattern)| i)
            })
            .collect();
    }

    #[allow(clippy::too_many_arguments)]
    fn process_line_inner(
        &self,
        line: &str,
        depth: u32,
        pattern_set: &PatternSet,
        triggers: &[Trigger],
        regex_set_to_triggers_map: &[usize],
        regex_set_to_patterns_map: &[usize],
        match_type: TriggerMatchType,
        is_captured: Option<Arc<AtomicBool>>,
        fallthrough_scopes: &mut FallthroughScopes,
    ) -> Result<()> {
        if depth > 100 {
            bail!(
                "Script processor bailing, depth limit reached. Do you have an alias that triggers itself?"
            );
        }
        // Time the match only when debug logging is compiled in: `log_enabled!(Debug)`
        // const-folds to `false` under `release_max_level_info` (release/bench), so the timer is
        // a dead `None` and the whole block — both clock reads — is optimized away.
        let timer = log::log_enabled!(log::Level::Debug).then(Instant::now);
        let matches = pattern_set.matched_indices(line);
        if let Some(start) = timer {
            debug!("Time to test pattern matches: {:?}", start.elapsed());
        }

        if !matches.is_empty() {
            for match_indices in matches.chunk_by(|a, b| {
                regex_set_to_triggers_map.get(*a).unwrap()
                    == regex_set_to_triggers_map.get(*b).unwrap()
            }) {
                let match_idx = match_indices[0];
                let trigger = triggers
                    .get(*regex_set_to_triggers_map.get(match_idx).unwrap())
                    .unwrap();

                if !trigger.enabled || trigger.anti_patterns.is_match(line) {
                    continue;
                }

                debug!(
                    "Trigger matched: {:?}, /{}/",
                    trigger.name(),
                    pattern_set.patterns().get(match_idx).unwrap()
                );

                let pattern_idx = *regex_set_to_patterns_map.get(match_idx).unwrap();
                let stopped = fallthrough_scopes
                    .entry((trigger.isolate.clone(), trigger.origin.clone()))
                    .or_insert_with(|| Arc::new(AtomicBool::new(false)))
                    .clone();
                trigger.run(
                    line,
                    match_type,
                    pattern_idx,
                    &is_captured,
                    stopped,
                    &self.spawned_actions,
                    depth + 1,
                )?;
            }
        }
        Ok(())
    }

    /// Queue the auto-removal of a self-limited automation, routed by whether it is an alias or a
    /// trigger (the same split the dispatch handlers use). Best-effort: the action lands at the
    /// back of the spawned-action queue and the `Manager` applies it on its own thread.
    fn queue_self_removal(&self, item: &Trigger) {
        let action = if item.is_alias {
            RuntimeAction::RemoveAlias(
                item.isolate.clone(),
                item.origin.clone(),
                Arc::new(item.name.clone()),
            )
        } else {
            RuntimeAction::RemoveTrigger(
                item.isolate.clone(),
                item.origin.clone(),
                Arc::new(item.name.clone()),
            )
        };
        self.spawned_actions.borrow_mut().push_back(action);
    }

    /// Bump `lines_tested` on every enabled trigger that declares a `lineLimit`, queueing each
    /// one's removal as it reaches the limit. Trigger-only (called from the incoming-line paths).
    /// Iterates only [`line_limited_triggers`](Self::line_limited_triggers), so unlimited
    /// triggers — the common case — cost nothing per line rather than an O(trigger-count) scan.
    /// Counts one tested line per incoming line regardless of how many tiers (raw/normal)
    /// evaluate it.
    fn count_tested_lines(&self) {
        for &idx in &self.line_limited_triggers {
            let trigger = &self.triggers[idx];
            // `line_limited_triggers` only holds `line_limit.is_some()` indices; the self-limit
            // arithmetic still needs the concrete bound.
            let Some(limit) = trigger.line_limit else {
                continue;
            };
            if !trigger.enabled {
                continue;
            }
            let tested = trigger.lines_tested.get() + 1;
            trigger.lines_tested.set(tested);
            if tested >= limit && trigger.fire_limit.is_none_or(|fl| trigger.fires.get() < fl) {
                self.queue_self_removal(trigger);
            }
        }
    }

    pub fn process_outgoing_line(&mut self, line: &str) -> Result<()> {
        // Lazily rebuild the alias PatternSet here (mirrors how
        // `process_incoming_line` rebuilds the trigger set) so alias inserts at
        // load time stay O(1) and we pay one rebuild on the first command.
        if self.alias_regex_set_dirty {
            self.rebuild_alias_regex_set();
            self.alias_regex_set_dirty = false;
        }
        self.process_nested_outgoing_line(line, 0)
    }

    pub fn process_nested_outgoing_line(&self, line: &str, depth: u32) -> Result<()> {
        let is_captured = Arc::new(AtomicBool::new(false));
        let mut fallthrough_scopes = FallthroughScopes::new();

        self.process_line_inner(
            line,
            depth,
            &self.alias_regex_set,
            &self.aliases,
            &self.alias_regex_set_map,
            &self.alias_regex_patterns_map,
            TriggerMatchType::Normal,
            Some(is_captured.clone()),
            &mut fallthrough_scopes,
        )?;

        self.spawned_actions
            .borrow_mut()
            .push_back(RuntimeAction::SendRawUnless(
                is_captured,
                Arc::new(line.to_string()),
            ));
        Ok(())
    }

    /// Execute a matched plaintext command template. This happens at dispatch time (rather than
    /// match-discovery time) so a prior automation can stop this invocation before it captures or
    /// sends anything. Each separated command begins its own alias frame.
    pub(crate) fn run_simple_automation(
        &self,
        script: &str,
        captures: &[MatchCapture],
        depth: u32,
    ) -> Result<()> {
        let evaluated = expand_template(script, captures);
        for line in split_commands(&evaluated, &self.command_separator) {
            self.process_nested_outgoing_line(line, depth)?;
        }
        Ok(())
    }

    /// Count an invocation only after it actually begins running. A match skipped by an earlier
    /// `fallthrough(false)` therefore consumes neither `fireLimit` nor its one-shot lifetime.
    pub(crate) fn record_fire(
        &self,
        isolate: &IsolateId,
        origin: &Origin,
        name: &str,
        is_alias: bool,
    ) {
        let indices = if is_alias {
            &self.alias_indices
        } else {
            &self.trigger_indices
        };
        let items = if is_alias {
            &self.aliases
        } else {
            &self.triggers
        };
        let Some(&index) = indices
            .get(&(isolate.clone(), origin.clone()))
            .and_then(|namespace| namespace.get(name))
        else {
            return;
        };
        let Some(item) = items.get(index) else {
            return;
        };

        let fires = item.fires.get() + 1;
        item.fires.set(fires);
        if item.fire_limit.is_some_and(|limit| fires >= limit) {
            self.queue_self_removal(item);
        }
    }

    /// Match `line` against the complete-line trigger sets, queuing the matched triggers'
    /// actions. Does **not** enqueue [`RuntimeAction::CompleteLineTriggersProcessed`] — the
    /// caller owns that, so it can splice a post-trigger `sys:receive` emit between the trigger
    /// cascade and the line's transform/route step (see the `HandleIncomingLine` dispatch arm).
    pub fn process_incoming_line(&mut self, line: &Arc<StyledLine>) -> Result<()> {
        trace!("Processing incoming line: {line:?}");
        if self.trigger_regex_set_dirty {
            self.rebuild_trigger_regex_set();
            self.trigger_regex_set_dirty = false;
        }

        // Zero-cost unless debug logging is compiled in; see `process_line_inner`.
        let timer = log::log_enabled!(log::Level::Debug).then(Instant::now);

        let mut fallthrough_scopes = FallthroughScopes::new();
        if let Some(line) = line.raw() {
            debug!("Processing raw line: {line:?}");
            self.process_line_inner(
                line,
                0,
                &self.raw_trigger_regex_set,
                &self.triggers,
                &self.raw_trigger_regex_set_map,
                &self.raw_trigger_regex_patterns_map,
                TriggerMatchType::Raw,
                None,
                &mut fallthrough_scopes,
            )?;
        }

        self.process_line_inner(
            line,
            0,
            &self.trigger_regex_set,
            &self.triggers,
            &self.trigger_regex_set_map,
            &self.trigger_regex_patterns_map,
            TriggerMatchType::Normal,
            None,
            &mut fallthrough_scopes,
        )?;

        // Self-limit: one tested-line tick per incoming complete line for every
        // `lineLimit` trigger (no-op for the common unlimited case).
        self.count_tested_lines();

        if let Some(start) = timer {
            debug!(
                "Time to match and dispatch triggers on incoming line: {:?}",
                start.elapsed()
            );
        }

        Ok(())
    }

    pub fn process_partial_line(&self, line: Arc<StyledLine>) -> Result<()> {
        trace!("Processing incoming partial line: {line:?}");

        // Zero-cost unless debug logging is compiled in; see `process_line_inner`.
        let timer = log::log_enabled!(log::Level::Debug).then(Instant::now);

        let mut fallthrough_scopes = FallthroughScopes::new();
        if let Some(line) = line.raw() {
            self.process_line_inner(
                line,
                0,
                &self.prompt_raw_trigger_regex_set,
                &self.triggers,
                &self.prompt_raw_trigger_regex_set_map,
                &self.prompt_raw_trigger_regex_patterns_map,
                TriggerMatchType::Raw,
                None,
                &mut fallthrough_scopes,
            )?;
        }

        self.process_line_inner(
            &line,
            0,
            &self.prompt_trigger_regex_set,
            &self.triggers,
            &self.prompt_trigger_regex_set_map,
            &self.prompt_trigger_regex_patterns_map,
            TriggerMatchType::Normal,
            None,
            &mut fallthrough_scopes,
        )?;

        if let Some(start) = timer {
            debug!(
                "Time to match and dispatch triggers on incoming partial line: {:?}",
                start.elapsed()
            );
        }

        self.spawned_actions
            .borrow_mut()
            .push_back(RuntimeAction::PartialLineTriggersProcessed(line));
        Ok(())
    }
}

#[derive(Debug)]
struct Trigger {
    /// The isolate this automation was registered in. Source of truth for both the
    /// `(IsolateId, Origin)` registry key and the isolate stamped into the v8-routed
    /// actions [`run`](Trigger::run) emits (its `ScriptId`/`FunctionId` index *this*
    /// isolate's registries).
    isolate: IsolateId,
    origin: Origin,
    name: String,
    patterns: Vec<Regex>,
    raw_patterns: Vec<Regex>,
    anti_patterns: RegexSet,
    script: ScriptAction,
    prompt: bool,
    enabled: bool,
    /// Higher values are evaluated first; equal values retain registration order.
    priority: i32,
    /// Whether later matches in this automation's creator scope may run.
    fallthrough: bool,
    /// Whether this entry lives in the alias `Vec` (matched on outgoing input) vs the trigger
    /// `Vec` (matched on incoming lines). Drives the `RemoveAlias`/`RemoveTrigger` self-limit
    /// removal kind. `Trigger` is reused for both by construction; this is the discriminant.
    is_alias: bool,
    /// Self-limit: auto-remove after this many fires. `None` ⇒ unbounded; `Some(1)` ⇒
    /// one-shot.
    fire_limit: Option<u32>,
    /// Self-limit (trigger-only): auto-remove after this many tested lines. Aliases match
    /// input rather than server lines, so this is always `None` for them.
    line_limit: Option<u32>,
    /// Times this automation has fired. `Cell` so the matcher can bump it through the `&self`
    /// processing path without a `&mut Manager`.
    fires: Cell<u32>,
    /// Times this trigger has been evaluated against an incoming line (only tracked when
    /// `line_limit` is set, to avoid per-line cost for the common unlimited case).
    lines_tested: Cell<u32>,
    /// Display-only body source for the automations window's read-only detail pane: the
    /// JS/TS eval string, or a function's `toString()` passed in good faith from JS-land.
    /// `None` for plaintext bodies (recoverable from `script`) or when none was supplied.
    /// Never executed — purely what the UI renders.
    source: Option<Arc<str>>,
}

impl Trigger {
    #[allow(clippy::too_many_arguments)]
    pub fn new<
        TIterPattern,
        TIterRawPattern,
        TIterAntiPattern,
        TPatternStr,
        TRawPatternStr,
        TAntiPatternStr,
    >(
        isolate: IsolateId,
        origin: Origin,
        name: String,
        patterns: TIterPattern,
        raw_patterns: TIterRawPattern,
        anti_patterns: TIterAntiPattern,
        script: ScriptAction,
        prompt: bool,
        enabled: bool,
        priority: i32,
        fallthrough: bool,
        fire_limit: Option<u32>,
        line_limit: Option<u32>,
    ) -> Result<Self>
    where
        TPatternStr: AsRef<str>,
        TRawPatternStr: AsRef<str>,
        TAntiPatternStr: AsRef<str>,
        TIterPattern: Iterator<Item = TPatternStr>,
        TIterRawPattern: Iterator<Item = TRawPatternStr>,
        TIterAntiPattern: Iterator<Item = TAntiPatternStr>,
    {
        let patterns: Vec<_> = patterns
            .map(|pattern| Regex::new(pattern.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        let raw_patterns: Vec<_> = raw_patterns
            .map(|pattern| Regex::new(pattern.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        let anti_patterns = RegexSet::new(anti_patterns)?;

        Ok(Self {
            isolate,
            origin,
            name,
            patterns,
            raw_patterns,
            anti_patterns,
            script,
            prompt,
            enabled,
            priority,
            fallthrough,
            is_alias: false,
            fire_limit,
            line_limit,
            fires: Cell::new(0),
            lines_tested: Cell::new(0),
            source: None,
        })
    }

    pub fn new_alias<TIterPattern, TPatternStr>(
        isolate: IsolateId,
        origin: Origin,
        name: String,
        patterns: TIterPattern,
        script: ScriptAction,
        priority: i32,
        fallthrough: bool,
        fire_limit: Option<u32>,
    ) -> Result<Self>
    where
        TPatternStr: AsRef<str>,
        TIterPattern: Iterator<Item = TPatternStr>,
    {
        let mut alias = Self::new(
            isolate,
            origin,
            name,
            patterns,
            std::iter::empty::<&str>(),
            std::iter::empty::<&str>(),
            script,
            false,
            true,
            priority,
            fallthrough,
            fire_limit,
            // Aliases match input, not server lines, so `lineLimit` is ignored for them.
            None,
        )?;
        alias.is_alias = true;
        Ok(alias)
    }

    /// Attaches the display-only body source (see [`Trigger::source`]). Chained off `new`/
    /// `new_alias` at the push sites that have it.
    #[must_use]
    fn with_source(mut self, source: Option<Arc<str>>) -> Self {
        self.source = source;
        self
    }

    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &self,
        line: &str,
        match_type: TriggerMatchType,
        pattern_idx: usize,
        is_captured: &Option<Arc<AtomicBool>>,
        stopped: Arc<AtomicBool>,
        spawned_actions: &ActionQueue,
        depth: u32,
    ) -> Result<()> {
        let pattern = match match_type {
            TriggerMatchType::Normal => self.patterns.get(pattern_idx).unwrap(),
            TriggerMatchType::Raw => self.raw_patterns.get(pattern_idx).unwrap(),
        };
        // Ordered captures: position is the group number (index 0 = whole match), `name` set
        // only for named groups. The list is shared by the JS handlers (numeric/named
        // `matches` object) and the inline `SendSimple` template expansion.
        let captures: Arc<Vec<MatchCapture>> = Arc::new(
            pattern
                .capture_names()
                .zip(pattern.captures(line).unwrap().iter())
                .map(|(name, value)| MatchCapture {
                    name: name.map(|n| std::borrow::Cow::Owned(n.to_string())),
                    value: value.map_or_else(String::new, |m| m.as_str().to_string()),
                })
                .collect(),
        );

        spawned_actions
            .borrow_mut()
            .push_back(RuntimeAction::RunAutomation {
                isolate: self.isolate.clone(),
                origin: self.origin.clone(),
                name: Arc::new(self.name.clone()),
                script: self.script.clone(),
                matches: captures,
                depth,
                is_captured: is_captured.clone(),
                stopped,
                fallthrough: self.fallthrough,
                is_alias: self.is_alias,
            });
        Ok(())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn fire_on_prompts(&self) -> bool {
        self.prompt
    }
}

#[cfg(test)]
mod tests {
    use super::{MatchCapture, expand_template, split_commands};

    mod raw_wanted {
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        use super::super::{Manager, PushTriggerParams, ScriptAction};
        use crate::session::runtime::origin::{IsolateId, Origin};

        fn manager() -> Manager {
            Manager::new(
                std::rc::Rc::default(),
                Arc::new(";".to_string()),
                std::rc::Rc::default(),
            )
        }

        fn push(manager: &mut Manager, name: &str, raw_patterns: Vec<String>) {
            manager
                .push_trigger(PushTriggerParams {
                    isolate: IsolateId::Main,
                    origin: Origin::User,
                    name: &Arc::new(name.to_string()),
                    patterns: &Arc::new(vec!["plain".to_string()]),
                    raw_patterns: &Arc::new(raw_patterns),
                    anti_patterns: &Arc::new(Vec::new()),
                    action: ScriptAction::SendSimple(Arc::new("ok".to_string())),
                    prompt: false,
                    enabled: true,
                    priority: 0,
                    fallthrough: true,
                    fire_limit: None,
                    line_limit: None,
                    source: None,
                })
                .unwrap();
        }

        #[test]
        fn flag_tracks_raw_pattern_existence_across_mutations() {
            let mut m = manager();
            let flag = m.raw_wanted_flag();
            assert!(!flag.load(Ordering::Relaxed), "empty manager wants no raw");

            push(&mut m, "plain-only", Vec::new());
            assert!(
                !flag.load(Ordering::Relaxed),
                "plain triggers don't ask for raw"
            );

            push(&mut m, "raw", vec!["\\x1b\\[31m".to_string()]);
            assert!(
                flag.load(Ordering::Relaxed),
                "a raw pattern raises the flag"
            );

            m.remove_trigger(&IsolateId::Main, &Origin::User, "raw");
            assert!(
                !flag.load(Ordering::Relaxed),
                "removing the last raw trigger lowers it"
            );

            // An upsert that drops the raw pattern lowers it too.
            push(&mut m, "raw2", vec!["raw".to_string()]);
            assert!(flag.load(Ordering::Relaxed));
            push(&mut m, "raw2", Vec::new());
            assert!(!flag.load(Ordering::Relaxed), "upsert away the raw pattern");
        }

        #[test]
        fn adopted_flag_cell_keeps_feeding_the_old_clone() {
            let mut old = manager();
            push(&mut old, "raw", vec!["raw".to_string()]);
            let connection_clone = old.raw_wanted_flag();
            assert!(connection_clone.load(Ordering::Relaxed));

            // Reload: a fresh manager adopts the old cell; the connection's
            // clone immediately reflects the empty new manager…
            let mut fresh = manager();
            fresh.adopt_raw_wanted_flag(old.raw_wanted_flag());
            assert!(!connection_clone.load(Ordering::Relaxed));

            // …and re-registration into the NEW manager raises the OLD clone.
            push(&mut fresh, "raw", vec!["raw".to_string()]);
            assert!(connection_clone.load(Ordering::Relaxed));
        }
    }

    /// Build captures from a list of `(name, value)` pairs; position is the group number.
    fn caps(items: &[(Option<&str>, &str)]) -> Vec<MatchCapture> {
        items
            .iter()
            .map(|(name, value)| MatchCapture {
                name: name.map(|n| std::borrow::Cow::Owned(n.to_string())),
                value: (*value).to_string(),
            })
            .collect()
    }

    #[test]
    fn template_expands_double_digit_groups_without_clobber() {
        // 11 groups (index 0 = whole match, 1..=10 = groups). `${10}` must resolve group ten,
        // and bare `$1` must resolve group one even when followed by another digit.
        let captures = caps(&[
            (None, "WHOLE"),
            (None, "g1"),
            (None, "g2"),
            (None, "g3"),
            (None, "g4"),
            (None, "g5"),
            (None, "g6"),
            (None, "g7"),
            (None, "g8"),
            (None, "g9"),
            (None, "g10"),
        ]);
        // `${10}` is group ten; `${1}` is group one — no collision.
        assert_eq!(expand_template("x ${10} ${1}", &captures), "x g10 g1");
        // Bare `$10` is group one followed by a literal `0` (single-digit rule).
        assert_eq!(expand_template("$10", &captures), "g10");
        // ^ group one ("g1") + literal "0" == "g1" + "0" == "g10". Make the distinction explicit
        // with a group whose value is unambiguous.
        let captures2 = caps(&[(None, "WHOLE"), (None, "ONE")]);
        assert_eq!(expand_template("$10", &captures2), "ONE0");
    }

    #[test]
    fn template_dollar_escape_and_named_groups() {
        let captures = caps(&[(None, "WHOLE"), (None, "g1"), (Some("name"), "NAMED")]);
        assert_eq!(
            expand_template("x $1 $$ ${name}", &captures),
            "x g1 $ NAMED"
        );
        // `$name` identifier form resolves the same named group.
        assert_eq!(expand_template("$name", &captures), "NAMED");
    }

    #[test]
    fn template_unknown_and_empty_groups_expand_empty() {
        let captures = caps(&[(None, "WHOLE"), (None, "")]);
        // Out-of-range index, unknown name, and an empty group all expand to "".
        assert_eq!(expand_template("[${9}]", &captures), "[]");
        assert_eq!(expand_template("[${missing}]", &captures), "[]");
        assert_eq!(expand_template("[$1]", &captures), "[]");
    }

    #[test]
    fn template_lone_and_malformed_dollar_is_literal() {
        let captures = caps(&[(None, "WHOLE")]);
        // Trailing `$`, `$` before a space, and an unterminated `${` are all literal `$`.
        assert_eq!(expand_template("end$", &captures), "end$");
        assert_eq!(expand_template("a $ b", &captures), "a $ b");
        assert_eq!(expand_template("${oops", &captures), "${oops");
    }

    #[test]
    fn template_whole_match_is_index_zero() {
        let captures = caps(&[(None, "the whole thing"), (None, "g1")]);
        assert_eq!(expand_template("[$0]", &captures), "[the whole thing]");
        assert_eq!(expand_template("[${0}]", &captures), "[the whole thing]");
    }

    #[test]
    fn default_separator_splits_commands() {
        assert_eq!(
            split_commands("north;south;east", ";"),
            vec!["north", "south", "east"]
        );
    }

    #[test]
    fn multi_char_separator_splits_and_preserves_single_occurrences() {
        assert_eq!(
            split_commands("say hi; you;;north", ";;"),
            vec!["say hi; you", "north"]
        );
    }

    #[test]
    fn empty_separator_only_splits_on_newlines() {
        assert_eq!(
            split_commands("say a;b\nnorth", ""),
            vec!["say a;b", "north"]
        );
    }

    #[test]
    fn newline_always_splits() {
        assert_eq!(
            split_commands("north\nsouth;east", ";"),
            vec!["north", "south", "east"]
        );
    }
}
