//! State exposures: the session-store roots an alias, trigger, or hotkey declares it reads,
//! persisted on the definition as its `state` list. Each exposure names one root (a platform
//! producer, or a state handle inside the `user` producer or a package) and the paths beneath
//! it the automation reads; each becomes a name the action spells (`$prompt.hp` in Send text,
//! `prompt.hp` in JavaScript). The runtime binds every exposed path to a store cell at
//! registration ([`crate::session::runtime::ExposedState`]); an automation with an empty list
//! pays nothing at fire time.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::session::runtime::{ProducerKey, StorePath};

/// One exposed store root and the paths beneath it the automation reads.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct StateExposure {
    /// The producer address in [`ProducerKey`]'s display form: `user`, `gmcp`, `msdp`,
    /// `mssp`, or `smudgy://owner/name`.
    pub producer: String,
    /// The state handle's name string (`createState('prompt', …)` → `prompt`). Required for
    /// the `user` and package producers; absent for the platform producers, whose whole tree
    /// is the one root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// The name the action spells, when the author renamed it (`as` on disk). Omitted when
    /// it equals [`StateExposure::default_name`].
    #[serde(default, rename = "as", skip_serializing_if = "Option::is_none")]
    pub name_override: Option<String>,
    /// Store paths relative to the root, in the author's spelling (folded at lookup); `""`
    /// is the root itself.
    #[serde(default)]
    pub paths: Vec<String>,
}

/// One relative path of a resolved exposure together with its absolute store address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    /// The path beneath the root as the author spelled it (`Char.Vitals`; empty = the root).
    pub relative: StorePath,
    /// The address bound in the store: the handle joined with `relative`, or `relative`
    /// itself under a platform producer.
    pub full: StorePath,
}

/// A validated exposure with its addresses parsed, the form the runtime binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedExposure {
    /// The identifier the action uses (override or default).
    pub name: String,
    pub producer: ProducerKey,
    /// The root inside the producer's tree: the handle as one segment, or the tree root for
    /// a platform producer.
    pub root: StorePath,
    pub paths: Vec<ResolvedPath>,
}

/// One path of an exposure as the runtime binds it, see [`ResolvedExposure::bound_paths`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundPath {
    /// The segments beneath the root, in the spelling that keys the JavaScript intermediates.
    pub segments: Vec<String>,
    /// The store address bound: the handle joined with the path, or the path itself under a
    /// platform producer.
    pub full: StorePath,
}

impl ResolvedExposure {
    /// The paths the runtime binds and JavaScript keys, derived from `paths` in order. A path
    /// at or below one already kept is dropped (the kept path's cell covers it for both
    /// spellings) and a path above kept paths replaces them, so no two overlap. A kept path
    /// that shares leading segments with an earlier kept path under the fold takes that
    /// path's spelling for them, so the JavaScript intermediates the name's paths meet at are
    /// one object: exposing `Char.Vitals` and `char.Status` binds `Char.Vitals` and
    /// `Char.Status`. A JavaScript reference above an exposed node must use this spelling;
    /// Send text folds and is unaffected.
    #[must_use]
    pub fn bound_paths(&self) -> Vec<BoundPath> {
        let mut bound: Vec<BoundPath> = Vec::with_capacity(self.paths.len());
        for path in &self.paths {
            let segments = path.relative.segments();
            if bound.iter().any(|kept| is_prefix(&kept.segments, segments)) {
                continue;
            }
            bound.retain(|kept| !is_prefix(segments, &kept.segments));
            let mut spelled = segments.to_vec();
            for kept in &bound {
                for (segment, earlier) in spelled.iter_mut().zip(&kept.segments) {
                    if !segment.eq_ignore_ascii_case(earlier) {
                        break;
                    }
                    segment.clone_from(earlier);
                }
            }
            bound.push(BoundPath {
                segments: spelled,
                full: path.full.clone(),
            });
        }
        bound
    }
}

/// Whether `prefix` is an ancestor-or-equal of `path` under the ASCII fold.
fn is_prefix(prefix: &[String], path: &[String]) -> bool {
    prefix.len() <= path.len()
        && prefix
            .iter()
            .zip(path)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// The names JavaScript refuses as a binding: its reserved words, and `let`, `eval`, and
/// `arguments`, which a strict-mode declaration may not bind. An exposure cannot take one of
/// these as its name: a body could not spell it as an identifier, and the typing bridge could
/// not declare it. Kept alphabetical (ASCII order) for binary search.
pub const JS_RESERVED_NAMES: &[&str] = &[
    "arguments",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "eval",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

/// The names the inline `with` scope provides to every alias, trigger, and hotkey body: the
/// `const` globals of the language-service bridge (`smudgy-inline.d.ts`), minus `matches`,
/// which is a per-fire global with no collision. An exposure under one of these names shadows
/// that member inside its own body only. The bridge's drift test pins this list to the
/// declarations. Kept alphabetical (ASCII order) for binary search.
pub const INLINE_SCOPE_NAMES: &[&str] = &[
    "Area",
    "aliases",
    "buffer",
    "byId",
    "byName",
    "capture",
    "command",
    "createAlias",
    "createDerived",
    "createEvent",
    "createHotkey",
    "createProcedure",
    "createState",
    "createTimer",
    "createTrigger",
    "createTriggers",
    "echo",
    "events",
    "fallthrough",
    "getDataDir",
    "getProfile",
    "getSessions",
    "getSettings",
    "gmcp",
    "hotkeys",
    "id",
    "input",
    "layout",
    "line",
    "link",
    "mapper",
    "pattern",
    "reload",
    "send",
    "sendRaw",
    "session",
    "style",
    "submission",
    "timers",
    "triggers",
    "userAutomations",
    "vars",
];

/// The ECMAScript standard globals an inline body is likely to reach for. An exposure
/// under one of these names shadows it inside that body (`Math.floor` would read the
/// exposed value's `floor`), which the editor points out. Kept alphabetical (ASCII order)
/// for binary search.
pub const ECMASCRIPT_GLOBALS: &[&str] = &[
    "AggregateError",
    "Array",
    "ArrayBuffer",
    "Atomics",
    "BigInt",
    "BigInt64Array",
    "BigUint64Array",
    "Boolean",
    "DataView",
    "Date",
    "Error",
    "EvalError",
    "FinalizationRegistry",
    "Float32Array",
    "Float64Array",
    "Function",
    "Infinity",
    "Int16Array",
    "Int32Array",
    "Int8Array",
    "Intl",
    "JSON",
    "Map",
    "Math",
    "NaN",
    "Number",
    "Object",
    "Promise",
    "Proxy",
    "RangeError",
    "ReferenceError",
    "Reflect",
    "RegExp",
    "Set",
    "SharedArrayBuffer",
    "String",
    "Symbol",
    "SyntaxError",
    "TypeError",
    "URIError",
    "Uint16Array",
    "Uint32Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "WeakMap",
    "WeakRef",
    "WeakSet",
    "decodeURI",
    "decodeURIComponent",
    "encodeURI",
    "encodeURIComponent",
    "escape",
    "globalThis",
    "isFinite",
    "isNaN",
    "parseFloat",
    "parseInt",
    "undefined",
    "unescape",
];

/// The Deno and web-platform globals the runtime provides to an inline body (`console`,
/// `prompt`, `fetch`, the timers, streams). An exposure under one of these names shadows it
/// inside that body, which the editor points out. Kept alphabetical (ASCII order) for
/// binary search.
pub const DENO_GLOBALS: &[&str] = &[
    "AbortController",
    "AbortSignal",
    "Blob",
    "BroadcastChannel",
    "CustomEvent",
    "Deno",
    "Event",
    "EventTarget",
    "File",
    "FormData",
    "Headers",
    "MessageChannel",
    "MessagePort",
    "ReadableStream",
    "Request",
    "Response",
    "TextDecoder",
    "TextEncoder",
    "TransformStream",
    "URL",
    "URLSearchParams",
    "WebSocket",
    "Worker",
    "WritableStream",
    "alert",
    "atob",
    "btoa",
    "clearInterval",
    "clearTimeout",
    "confirm",
    "console",
    "crypto",
    "fetch",
    "localStorage",
    "location",
    "navigator",
    "performance",
    "prompt",
    "queueMicrotask",
    "self",
    "sessionStorage",
    "setInterval",
    "setTimeout",
    "structuredClone",
    "window",
];

/// What an exposure's name shadows inside its body's JavaScript, when it shadows anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownGlobal {
    /// An inline API member ([`INLINE_SCOPE_NAMES`]).
    Smudgy,
    /// An ECMAScript global ([`ECMASCRIPT_GLOBALS`]).
    JavaScript,
    /// A Deno or web-platform global ([`DENO_GLOBALS`]).
    Deno,
}

/// Why an exposure is unusable. Surfaced verbatim in the editor's error bar and in the
/// runtime's drop-with-warning path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StateExposureError {
    #[error("unknown producer {producer:?}")]
    UnknownProducer { producer: String },
    #[error("{producer} state is read through a handle; none was given")]
    MissingHandle { producer: String },
    #[error("{producer} has no state handles; remove the handle")]
    UnexpectedHandle { producer: String },
    #[error("invalid handle {handle:?}: {message}")]
    InvalidHandle { handle: String, message: String },
    #[error("no paths are exposed")]
    NoPaths,
    #[error("invalid path {path:?}: {message}")]
    InvalidPath { path: String, message: String },
    #[error("{name:?} is not a valid name")]
    InvalidName { name: String },
    #[error("{name:?} is a JavaScript keyword and cannot be a name")]
    ReservedName { name: String },
    #[error("the name {name:?} is already used by another exposure")]
    DuplicateName { name: String },
}

impl StateExposure {
    /// Whether `name` is an identifier (`[A-Za-z_][A-Za-z0-9_]*`): the shape every exposed
    /// name must have so both `$name.path` and a bare `name` parse.
    #[must_use]
    pub fn is_identifier(name: &str) -> bool {
        !name.is_empty() && identifier_len(name) == name.len()
    }

    /// Whether `name` is a JavaScript reserved word ([`JS_RESERVED_NAMES`]), which no
    /// exposure may take as its name.
    #[must_use]
    pub fn is_reserved(name: &str) -> bool {
        JS_RESERVED_NAMES.binary_search(&name).is_ok()
    }

    /// Whether `name` can be an exposure's name: an identifier that is not a reserved word.
    #[must_use]
    pub fn is_valid_name(name: &str) -> bool {
        Self::is_identifier(name) && !Self::is_reserved(name)
    }

    /// What an exposure named `name` shadows inside its body, if anything: an inline API
    /// member, an ECMAScript global, or a Deno global. Exact-case, as JavaScript resolves
    /// names.
    #[must_use]
    pub fn known_global(name: &str) -> Option<KnownGlobal> {
        if INLINE_SCOPE_NAMES.binary_search(&name).is_ok() {
            Some(KnownGlobal::Smudgy)
        } else if ECMASCRIPT_GLOBALS.binary_search(&name).is_ok() {
            Some(KnownGlobal::JavaScript)
        } else if DENO_GLOBALS.binary_search(&name).is_ok() {
            Some(KnownGlobal::Deno)
        } else {
            None
        }
    }

    /// The default name: the handle name for handle-bearing producers, the producer name for
    /// the platform producers. A handle name that is not an identifier (an interop name may
    /// carry hyphens, or anything else a quoted store key admits) defaults to its identifier
    /// form: every character outside `[A-Za-z0-9_]` becomes `_` (so `my-state` is
    /// `my_state`), and a leading digit gets an `_` in front (`9lives` is `_9lives`). A name
    /// that would be a JavaScript reserved word gets a trailing `_` (`new` is `new_`). This is
    /// the one definition of the default; the editor displays it and the runtime binds it.
    #[must_use]
    pub fn default_name(&self) -> String {
        let raw = self
            .handle
            .as_deref()
            .map_or_else(|| self.producer.trim().to_ascii_lowercase(), str::to_owned);
        if Self::is_identifier(&raw) {
            return Self::unreserved(raw);
        }
        let mut name = String::with_capacity(raw.len() + 1);
        if raw.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            name.push('_');
        }
        name.extend(raw.chars().map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        }));
        Self::unreserved(name)
    }

    /// `name` with a trailing `_` when it is a reserved word, so a default is always usable.
    fn unreserved(mut name: String) -> String {
        if Self::is_reserved(&name) {
            name.push('_');
        }
        name
    }

    /// The name the action uses: the override when set, else the default.
    #[must_use]
    pub fn name(&self) -> String {
        self.name_override
            .as_deref()
            .map_or_else(|| self.default_name(), str::to_owned)
    }

    /// Validate this exposure on its own and parse its addresses: a known producer, a handle
    /// present exactly when the producer takes one, parseable paths that fit the depth limit
    /// once joined with the handle, and an identifier name. Name uniqueness across a list is
    /// [`resolve_all`]'s job.
    ///
    /// # Errors
    ///
    /// Returns the first [`StateExposureError`] found, in field order.
    pub fn resolve(&self) -> Result<ResolvedExposure, StateExposureError> {
        let producer = ProducerKey::parse(&self.producer).ok_or_else(|| {
            StateExposureError::UnknownProducer {
                producer: self.producer.clone(),
            }
        })?;
        let takes_handle = !matches!(producer, ProducerKey::Platform(_));
        let root = match (&self.handle, takes_handle) {
            (Some(handle), true) => StorePath::from_segments([handle.as_str()]).map_err(|err| {
                StateExposureError::InvalidHandle {
                    handle: handle.clone(),
                    message: err.to_string(),
                }
            })?,
            (None, false) => StorePath::root(),
            (None, true) => {
                return Err(StateExposureError::MissingHandle {
                    producer: producer.to_string(),
                });
            }
            (Some(_), false) => {
                return Err(StateExposureError::UnexpectedHandle {
                    producer: producer.to_string(),
                });
            }
        };
        if self.paths.is_empty() {
            return Err(StateExposureError::NoPaths);
        }
        let mut paths = Vec::with_capacity(self.paths.len());
        for path in &self.paths {
            let invalid = |err: &dyn std::fmt::Display| StateExposureError::InvalidPath {
                path: path.clone(),
                message: err.to_string(),
            };
            let relative = StorePath::parse(path).map_err(|err| invalid(&err))?;
            let full = root.joined(relative.clone()).map_err(|err| invalid(&err))?;
            paths.push(ResolvedPath { relative, full });
        }
        let name = self.name();
        if !Self::is_identifier(&name) {
            return Err(StateExposureError::InvalidName { name });
        }
        if Self::is_reserved(&name) {
            return Err(StateExposureError::ReservedName { name });
        }
        Ok(ResolvedExposure {
            name,
            producer,
            root,
            paths,
        })
    }
}

/// Validate a definition's whole exposure list: every entry resolves and no two entries share
/// a name under the ASCII fold.
///
/// # Errors
///
/// Returns the index of the offending entry with its error.
pub fn resolve_all(
    exposures: &[StateExposure],
) -> Result<Vec<ResolvedExposure>, (usize, StateExposureError)> {
    let mut resolved = Vec::with_capacity(exposures.len());
    let mut seen: HashSet<String> = HashSet::with_capacity(exposures.len());
    for (index, exposure) in exposures.iter().enumerate() {
        let entry = exposure.resolve().map_err(|err| (index, err))?;
        if !seen.insert(entry.name.to_ascii_lowercase()) {
            return Err((
                index,
                StateExposureError::DuplicateName { name: entry.name },
            ));
        }
        resolved.push(entry);
    }
    Ok(resolved)
}

/// The length of the identifier (`[A-Za-z_][A-Za-z0-9_]*`) at the start of `text`, 0 when
/// there is none. The one spelling of the name grammar: [`StateExposure::is_identifier`],
/// the runtime's template expander, and the editor's send-text highlighter all read it, so a
/// run the editor colors as a reference is exactly what the runtime resolves.
#[must_use]
pub fn identifier_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    let Some(first) = bytes.first() else {
        return 0;
    };
    if !(first.is_ascii_alphabetic() || *first == b'_') {
        return 0;
    }
    bytes
        .iter()
        .position(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
        .unwrap_or(bytes.len())
}

/// The length of the dotted tail (`(.identifier)*`) that follows a bare `$name` reference.
/// A trailing `.` with no identifier after it is not part of the tail.
#[must_use]
pub fn dotted_tail_len(text: &str) -> usize {
    let mut len = 0;
    loop {
        let rest = &text[len..];
        if !rest.starts_with('.') {
            return len;
        }
        let ident = identifier_len(&rest[1..]);
        if ident == 0 {
            return len;
        }
        len += 1 + ident;
    }
}

/// A piece of a reference tail outside the path grammar: a hop with no identifier (`.9`),
/// an unquoted or empty bracket key (`[x]`, `[""]`), an unclosed quote, or a character no
/// hop starts with. The runtime writes nothing for such a reference; the editor marks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the reference tail leaves the path grammar")]
pub struct MalformedSegment;

/// The segments of a reference tail, in the store's path grammar: `.identifier` hops and
/// `["quoted"]` / `['quoted']` keys, with the first hop's leading `.` optional (a braced
/// reference may start straight at a bracket). Yields [`MalformedSegment`] once at the first
/// malformed piece, then ends. Lazy, so a fire-time walk allocates nothing.
pub struct ReferenceSegments<'a> {
    rest: &'a str,
    first: bool,
}

impl<'a> ReferenceSegments<'a> {
    /// The segments of `tail`, the text after a reference's name (`""` for the root,
    /// `.char.vitals.hp`, `["Some-Pkg"].Msg`).
    #[must_use]
    pub fn new(tail: &'a str) -> Self {
        Self {
            rest: tail,
            first: true,
        }
    }
}

impl<'a> Iterator for ReferenceSegments<'a> {
    type Item = Result<&'a str, MalformedSegment>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let first = std::mem::replace(&mut self.first, false);
        let bytes = self.rest.as_bytes();
        if bytes[0] == b'[' {
            let quote = bytes.get(1).copied();
            let Some(quote @ (b'"' | b'\'')) = quote else {
                self.rest = "";
                return Some(Err(MalformedSegment));
            };
            let Some(end) = self.rest[2..].find(char::from(quote)).map(|at| at + 2) else {
                self.rest = "";
                return Some(Err(MalformedSegment));
            };
            if end == 2 || bytes.get(end + 1) != Some(&b']') {
                self.rest = "";
                return Some(Err(MalformedSegment));
            }
            let key = &self.rest[2..end];
            self.rest = &self.rest[end + 2..];
            return Some(Ok(key));
        }
        let body = if bytes[0] == b'.' {
            &self.rest[1..]
        } else if first {
            self.rest
        } else {
            self.rest = "";
            return Some(Err(MalformedSegment));
        };
        let len = identifier_len(body);
        if len == 0 {
            self.rest = "";
            return Some(Err(MalformedSegment));
        }
        let (segment, rest) = body.split_at(len);
        self.rest = rest;
        Some(Ok(segment))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exposure(producer: &str, handle: Option<&str>, paths: &[&str]) -> StateExposure {
        StateExposure {
            producer: producer.to_string(),
            handle: handle.map(str::to_string),
            name_override: None,
            paths: paths.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn name_lists_are_sorted_for_binary_search() {
        assert!(JS_RESERVED_NAMES.is_sorted());
        assert!(INLINE_SCOPE_NAMES.is_sorted());
        assert!(ECMASCRIPT_GLOBALS.is_sorted());
        assert!(DENO_GLOBALS.is_sorted());
        assert!(StateExposure::is_reserved("class"));
        assert!(StateExposure::is_reserved("arguments"));
        assert!(!StateExposure::is_reserved("Class"));
        assert!(!StateExposure::is_reserved("stats"));
    }

    /// The inline-scope list is the bridge's own declarations, so a member added to
    /// `smudgy-inline.d.ts` must be added here too (and the reverse).
    #[test]
    fn inline_scope_names_match_the_bridge_declarations() {
        let mut declared: Vec<&str> =
            crate::models::script_typings::language_service_inline_bridge()
                .lines()
                .filter_map(|line| line.trim().strip_prefix("const "))
                .filter_map(|declaration| declaration.split_once(':').map(|(name, _)| name.trim()))
                .filter(|name| *name != "matches")
                .collect();
        declared.sort_unstable();
        assert_eq!(declared, INLINE_SCOPE_NAMES);
    }

    #[test]
    fn known_globals_are_bucketed_and_unknown_names_are_not() {
        for name in ["send", "gmcp", "line", "Area"] {
            assert_eq!(
                StateExposure::known_global(name),
                Some(KnownGlobal::Smudgy),
                "{name}"
            );
        }
        for name in ["Math", "JSON", "undefined", "parseInt"] {
            assert_eq!(
                StateExposure::known_global(name),
                Some(KnownGlobal::JavaScript),
                "{name}"
            );
        }
        for name in ["prompt", "console", "setTimeout", "Deno"] {
            assert_eq!(
                StateExposure::known_global(name),
                Some(KnownGlobal::Deno),
                "{name}"
            );
        }
        for name in ["matches", "stats", "math", "hp", "Send"] {
            assert_eq!(StateExposure::known_global(name), None, "{name}");
        }
    }

    #[test]
    fn reserved_words_are_rejected_as_names_and_defaults_avoid_them() {
        assert!(!StateExposure::is_valid_name("class"));
        assert!(StateExposure::is_valid_name("stats"));
        let mut renamed = exposure("user", Some("foo"), ["bar"].as_slice());
        renamed.name_override = Some("new".to_string());
        assert!(matches!(
            renamed.resolve(),
            Err(StateExposureError::ReservedName { name }) if name == "new"
        ));
        // A handle that is itself a reserved word defaults to the trailing-underscore form,
        // so the default is always usable without a rename.
        assert_eq!(
            exposure("user", Some("new"), [""].as_slice()).name(),
            "new_"
        );
        assert_eq!(
            exposure("user", Some("default"), ["hp"].as_slice()).name(),
            "default_"
        );
        assert!(
            exposure("user", Some("new"), [""].as_slice())
                .resolve()
                .is_ok()
        );
    }

    #[test]
    fn names_default_to_the_handle_or_the_producer() {
        assert_eq!(exposure("gmcp", None, [""].as_slice()).name(), "gmcp");
        assert_eq!(exposure("GMCP", None, [""].as_slice()).name(), "gmcp");
        assert_eq!(
            exposure("user", Some("prompt"), ["hp"].as_slice()).name(),
            "prompt"
        );
        assert_eq!(
            exposure(
                "smudgy://kapusniak/arctic-prompt",
                Some("prompt"),
                [""].as_slice()
            )
            .name(),
            "prompt"
        );
        // Non-identifier handle names default to their underscore form.
        assert_eq!(
            exposure("user", Some("my-state"), [""].as_slice()).name(),
            "my_state"
        );
        assert_eq!(
            exposure("user", Some("9lives"), [""].as_slice()).name(),
            "_9lives"
        );
        assert_eq!(
            exposure("user", Some("a.b c"), [""].as_slice()).name(),
            "a_b_c"
        );
        // An override wins over the default.
        let mut renamed = exposure("user", Some("foo"), ["bar"].as_slice());
        renamed.name_override = Some("stats".to_string());
        assert_eq!(renamed.name(), "stats");
        assert_eq!(renamed.default_name(), "foo");
    }

    #[test]
    fn identifier_rule() {
        for ok in ["a", "_", "gmcp", "Char_1", "_9"] {
            assert!(StateExposure::is_identifier(ok), "{ok}");
        }
        for bad in ["", "9a", "a-b", "a.b", "a b", "é", "$x"] {
            assert!(!StateExposure::is_identifier(bad), "{bad}");
        }
    }

    #[test]
    fn reference_grammar_spells_identifiers_tails_and_segments() {
        fn collect(tail: &str) -> Vec<Result<&str, MalformedSegment>> {
            ReferenceSegments::new(tail).collect()
        }
        assert_eq!(identifier_len("gmcp.Char"), 4);
        assert_eq!(identifier_len("_x1 y"), 3);
        assert_eq!(identifier_len("9a"), 0);
        assert_eq!(identifier_len("é"), 0);
        assert_eq!(identifier_len(""), 0);
        assert_eq!(dotted_tail_len(".char.vitals.hp rest"), 15);
        assert_eq!(dotted_tail_len(".char."), 5);
        assert_eq!(dotted_tail_len("."), 0);
        assert_eq!(dotted_tail_len(" .x"), 0);
        assert_eq!(collect(""), Vec::<Result<&str, MalformedSegment>>::new());
        assert_eq!(collect(".a.b_2"), vec![Ok("a"), Ok("b_2")]);
        assert_eq!(collect("a.b"), vec![Ok("a"), Ok("b")]);
        assert_eq!(
            collect("[\"Some-Pkg\"].Msg['x y']"),
            vec![Ok("Some-Pkg"), Ok("Msg"), Ok("x y")]
        );
        assert_eq!(collect(".a."), vec![Ok("a"), Err(MalformedSegment)]);
        assert_eq!(collect(".9"), vec![Err(MalformedSegment)]);
        assert_eq!(collect("[x]"), vec![Err(MalformedSegment)]);
        assert_eq!(collect("[\"\"]"), vec![Err(MalformedSegment)]);
        assert_eq!(collect("[\"open"), vec![Err(MalformedSegment)]);
        assert_eq!(collect(".a b"), vec![Ok("a"), Err(MalformedSegment)]);
    }

    #[test]
    fn resolve_parses_addresses_and_joins_the_handle() {
        let platform = exposure("gmcp", None, ["Char.Vitals", ""].as_slice())
            .resolve()
            .unwrap();
        assert_eq!(platform.name, "gmcp");
        assert_eq!(platform.root, StorePath::root());
        assert_eq!(
            platform.paths[0].full,
            StorePath::parse("Char.Vitals").unwrap()
        );
        assert_eq!(platform.paths[1].full, StorePath::root());

        let handle = exposure("user", Some("foo"), ["bar", ""].as_slice())
            .resolve()
            .unwrap();
        assert_eq!(handle.producer, ProducerKey::User);
        assert_eq!(handle.root, StorePath::parse("foo").unwrap());
        assert_eq!(handle.paths[0].relative, StorePath::parse("bar").unwrap());
        assert_eq!(handle.paths[0].full, StorePath::parse("foo.bar").unwrap());
        assert_eq!(handle.paths[1].full, StorePath::parse("foo").unwrap());

        let package = exposure(
            "smudgy://Kapusniak/Arctic-Prompt",
            Some("prompt"),
            ["groupies[\"Mr. Foo\"].hp"].as_slice(),
        )
        .resolve()
        .unwrap();
        assert_eq!(
            package.producer,
            ProducerKey::Package {
                owner: "kapusniak".to_string(),
                name: "arctic-prompt".to_string()
            }
        );
        assert_eq!(
            package.paths[0].full.segments(),
            ["prompt", "groupies", "Mr. Foo", "hp"]
        );
    }

    #[test]
    fn bound_paths_drop_covered_paths_and_unify_shared_spelling() {
        let resolved = exposure(
            "gmcp",
            None,
            [
                "Char.Vitals.hp",
                "Char.Vitals",
                "char.VITALS",
                "char.Status",
                "Room.Info",
                "ROOM.info.name",
                "room.Players",
            ]
            .as_slice(),
        )
        .resolve()
        .unwrap();
        let bound: Vec<Vec<String>> = resolved
            .bound_paths()
            .into_iter()
            .map(|path| path.segments)
            .collect();
        assert_eq!(
            bound,
            vec![
                vec!["Char", "Vitals"],
                vec!["Char", "Status"],
                vec!["Room", "Info"],
                vec!["Room", "Players"],
            ]
        );
        // The root subsumes every other path, and a handle root is the bound address.
        let root = exposure("user", Some("foo"), ["bar", "", "baz"].as_slice())
            .resolve()
            .unwrap()
            .bound_paths();
        assert_eq!(root.len(), 1);
        assert!(root[0].segments.is_empty());
        assert_eq!(root[0].full, StorePath::parse("foo").unwrap());
    }

    #[test]
    fn resolve_rejects_bad_producers_handles_paths_and_names() {
        assert!(matches!(
            exposure("nope://x", None, [""].as_slice()).resolve(),
            Err(StateExposureError::UnknownProducer { .. })
        ));
        assert!(matches!(
            exposure("user", None, [""].as_slice()).resolve(),
            Err(StateExposureError::MissingHandle { .. })
        ));
        assert!(matches!(
            exposure("smudgy://a/b", None, [""].as_slice()).resolve(),
            Err(StateExposureError::MissingHandle { .. })
        ));
        assert!(matches!(
            exposure("gmcp", Some("x"), [""].as_slice()).resolve(),
            Err(StateExposureError::UnexpectedHandle { .. })
        ));
        assert!(matches!(
            exposure("user", Some(""), [""].as_slice()).resolve(),
            Err(StateExposureError::InvalidHandle { .. })
        ));
        assert!(matches!(
            exposure("gmcp", None, [].as_slice()).resolve(),
            Err(StateExposureError::NoPaths)
        ));
        assert!(matches!(
            exposure("gmcp", None, ["Char..Vitals"].as_slice()).resolve(),
            Err(StateExposureError::InvalidPath { .. })
        ));
        assert!(matches!(
            exposure("gmcp", None, ["Char."].as_slice()).resolve(),
            Err(StateExposureError::InvalidPath { .. })
        ));
        // A path that fits on its own but not once joined with the handle.
        let deep = vec!["a"; 64].join(".");
        assert!(matches!(
            exposure("user", Some("h"), [deep.as_str()].as_slice()).resolve(),
            Err(StateExposureError::InvalidPath { .. })
        ));
        let mut bad_name = exposure("gmcp", None, [""].as_slice());
        bad_name.name_override = Some("not-a-name".to_string());
        assert!(matches!(
            bad_name.resolve(),
            Err(StateExposureError::InvalidName { .. })
        ));
    }

    #[test]
    fn resolve_all_rejects_folded_duplicate_names() {
        let mut renamed = exposure("user", Some("foo"), ["bar"].as_slice());
        renamed.name_override = Some("GMCP".to_string());
        let err = resolve_all(&[exposure("gmcp", None, [""].as_slice()), renamed]).unwrap_err();
        assert_eq!(err.0, 1);
        assert!(matches!(err.1, StateExposureError::DuplicateName { .. }));

        // Two producers with distinct default names coexist; the index of a later parse
        // failure is reported.
        let ok = resolve_all(&[
            exposure("gmcp", None, ["Char.Vitals"].as_slice()),
            exposure("user", Some("foo"), ["bar"].as_slice()),
        ])
        .unwrap();
        assert_eq!(ok.len(), 2);
        let err = resolve_all(&[
            exposure("gmcp", None, ["Char.Vitals"].as_slice()),
            exposure("user", None, ["bar"].as_slice()),
        ])
        .unwrap_err();
        assert_eq!(err.0, 1);
    }

    #[test]
    fn serde_uses_the_documented_keys_and_stays_sparse() {
        let full: StateExposure = serde_json::from_str(
            r#"{ "producer": "user", "handle": "foo", "as": "stats", "paths": ["bar"] }"#,
        )
        .unwrap();
        assert_eq!(full.handle.as_deref(), Some("foo"));
        assert_eq!(full.name_override.as_deref(), Some("stats"));
        assert_eq!(
            serde_json::to_string(&full).unwrap(),
            r#"{"producer":"user","handle":"foo","as":"stats","paths":["bar"]}"#
        );
        let sparse = exposure("gmcp", None, ["Char.Vitals"].as_slice());
        assert_eq!(
            serde_json::to_string(&sparse).unwrap(),
            r#"{"producer":"gmcp","paths":["Char.Vitals"]}"#
        );
        let back: StateExposure =
            serde_json::from_str(&serde_json::to_string(&sparse).unwrap()).unwrap();
        assert_eq!(back, sparse);
    }
}
