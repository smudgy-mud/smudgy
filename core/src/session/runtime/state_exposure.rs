//! State exposures bound to the session store: the runtime form of a definition's `state`
//! list ([`crate::models::state_exposure::StateExposure`]).
//!
//! Each exposed path is a store binding ([`SessionStore::bind_exposure`]): a shared
//! [`StoreBindingCell`] the flush rewrites whenever a comparable path is written, so a fire
//! reads the committed store with one lock-free pointer load per exposed path and never
//! borrows the store. The set hangs off the automation's shared identity, so nothing new
//! travels per fire; an automation without exposures carries `None` and its fire path is
//! unchanged.
//!
//! Send text references (`$name.path`, `${name["key"].path}`) resolve here:
//! [`ExposedState::name`] finds the exposed name and [`ExposedName::write_reference`] the
//! exposed path the reference sits at or below, walks the remaining segments on the loaded
//! [`Node`] with folded comparisons, and writes the value straight into the line being
//! built. Unknown references write nothing, the captures rule.

use std::sync::Arc;

use smudgy_cloud::{Node, StoreBindingCell};

use crate::models::state_exposure::{ReferenceSegments, ResolvedExposure, StateExposure};

use super::store::SessionStore;

/// One automation's exposures, bound at registration.
#[derive(Debug)]
pub struct ExposedState {
    names: Vec<ExposedName>,
}

/// One exposed name and the paths beneath it, shaped by [`ResolvedExposure::bound_paths`]:
/// no two paths overlap, so a reference sits at or below at most one of them, and paths that
/// share leading segments spell them identically, so the JavaScript intermediates they meet
/// at are one object.
#[derive(Debug)]
pub(crate) struct ExposedName {
    name: String,
    paths: Vec<ExposedPath>,
}

/// One exposed path: its segments relative to the name's root, in the spelling that keys
/// the JavaScript intermediates (the store folds; JavaScript is exact), and the cell holding
/// its latest committed snapshot.
#[derive(Debug)]
pub(crate) struct ExposedPath {
    segments: Vec<String>,
    cell: Arc<StoreBindingCell>,
}

/// What binding a definition's exposures produced: the bound set (`None` when nothing
/// usable was exposed) and one warning per dropped entry.
pub(crate) struct BoundExposures {
    pub state: Option<Arc<ExposedState>>,
    pub warnings: Vec<String>,
}

impl ExposedState {
    /// Bind every usable exposure of one definition. An entry that fails to resolve, or whose
    /// name repeats an earlier entry's, is dropped with a warning rather than refusing the
    /// whole automation.
    pub(crate) fn bind(store: &mut SessionStore, exposures: &[StateExposure]) -> BoundExposures {
        let mut names: Vec<ExposedName> = Vec::with_capacity(exposures.len());
        let mut warnings = Vec::new();
        for (index, exposure) in exposures.iter().enumerate() {
            let resolved = match exposure.resolve() {
                Ok(resolved) => resolved,
                Err(err) => {
                    warnings.push(format!("state exposure {} ignored: {err}", index + 1));
                    continue;
                }
            };
            if names
                .iter()
                .any(|name| name.name.eq_ignore_ascii_case(&resolved.name))
            {
                warnings.push(format!(
                    "state exposure {} ignored: the name {:?} is already used by another exposure",
                    index + 1,
                    resolved.name
                ));
                continue;
            }
            names.push(ExposedName::bind(store, resolved));
        }
        BoundExposures {
            state: (!names.is_empty()).then(|| Arc::new(Self { names })),
            warnings,
        }
    }

    pub(crate) fn names(&self) -> &[ExposedName] {
        &self.names
    }

    /// The exposed name spelled `name` (ASCII-folded, as Send text folds), if any.
    pub(crate) fn name(&self, name: &str) -> Option<&ExposedName> {
        self.names
            .iter()
            .find(|exposed| exposed.name.eq_ignore_ascii_case(name))
    }
}

impl ExposedName {
    fn bind(store: &mut SessionStore, resolved: ResolvedExposure) -> Self {
        let paths = resolved
            .bound_paths()
            .into_iter()
            .map(|path| ExposedPath {
                cell: store.bind_exposure(resolved.producer.clone(), path.full),
                segments: path.segments,
            })
            .collect();
        Self {
            name: resolved.name,
            paths,
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn paths(&self) -> &[ExposedPath] {
        &self.paths
    }

    /// Append the value of the reference `$name` + `tail` to `out`, where `tail` is the path
    /// text after the name (`""` for the root, `.char.vitals.hp`, `["Some-Pkg"].Msg`).
    /// Strings are written verbatim, numbers and booleans in their JSON spelling, objects and
    /// arrays as compact JSON; `null`, absence, a malformed tail, and a reference above every
    /// exposed path write nothing.
    pub(crate) fn write_reference(&self, tail: &str, out: &mut String) {
        for path in &self.paths {
            // The exposed path must be a prefix of the reference: consume one reference
            // segment per exposed segment, then walk whatever follows on the loaded node.
            let mut segments = ReferenceSegments::new(tail);
            let mut at_or_below = true;
            for exposed in &path.segments {
                match segments.next() {
                    Some(Ok(segment)) if segment.eq_ignore_ascii_case(exposed) => {}
                    _ => {
                        at_or_below = false;
                        break;
                    }
                }
            }
            if !at_or_below {
                continue;
            }
            let snapshot = path.cell.load();
            let mut node: &Node = &snapshot;
            for segment in segments {
                let Ok(segment) = segment else {
                    return;
                };
                let Some(child) = node.get(segment) else {
                    return;
                };
                node = child;
            }
            write_node(node, out);
            return;
        }
    }
}

impl ExposedPath {
    pub(crate) fn segments(&self) -> &[String] {
        &self.segments
    }

    pub(crate) fn cell(&self) -> &Arc<StoreBindingCell> {
        &self.cell
    }
}

/// Write one node in its Send text rendering (see [`ExposedState::write_reference`]).
fn write_node(node: &Node, out: &mut String) {
    use std::fmt::Write as _;
    match node {
        Node::Null => {}
        Node::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Node::Number(number) => {
            let _ = write!(out, "{number}");
        }
        Node::String(text) => out.push_str(text),
        Node::Array(_) | Node::Object(_) => {
            // SAFETY: `serde_json` emits valid UTF-8 (escapes are ASCII and string content is
            // copied from `str` slices), a `Vec<u8>` writer never fails, and a string-keyed
            // tree always serializes, so the buffer holds valid UTF-8 once the call returns.
            let _ = serde_json::to_writer(unsafe { out.as_mut_vec() }, node);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::runtime::{IsolateId, ProducerKey, StorePath};
    use serde_json::json;

    fn exposure(producer: &str, handle: Option<&str>, paths: &[&str]) -> StateExposure {
        StateExposure {
            producer: producer.to_string(),
            handle: handle.map(str::to_string),
            name_override: None,
            paths: paths.iter().map(ToString::to_string).collect(),
        }
    }

    fn set(store: &mut SessionStore, producer: ProducerKey, path: &str, value: serde_json::Value) {
        store
            .set(
                producer,
                StorePath::parse(path).unwrap(),
                value,
                IsolateId::Main,
                0,
            )
            .expect("set within budget");
        store.flush();
    }

    fn rendered(state: &ExposedState, name: &str, tail: &str) -> String {
        let mut out = String::new();
        if let Some(exposed) = state.name(name) {
            exposed.write_reference(tail, &mut out);
        }
        out
    }

    #[test]
    fn binding_drops_invalid_entries_and_duplicate_names_with_warnings() {
        let mut store = SessionStore::new();
        let mut renamed = exposure("user", Some("foo"), &["bar"]);
        renamed.name_override = Some("GMCP".to_string());
        let bound = ExposedState::bind(
            &mut store,
            &[
                exposure("gmcp", None, &["Char.Vitals"]),
                exposure("nope://", None, &[""]),
                renamed,
            ],
        );
        let state = bound.state.expect("one usable exposure");
        assert_eq!(state.names().len(), 1);
        assert_eq!(state.names()[0].name(), "gmcp");
        assert_eq!(bound.warnings.len(), 2, "{:?}", bound.warnings);
        assert!(bound.warnings[0].starts_with("state exposure 2 ignored: unknown producer"));
        assert!(bound.warnings[1].contains("\"GMCP\" is already used"));

        let nothing = ExposedState::bind(&mut store, &[exposure("gmcp", Some("x"), &[""])]);
        assert!(nothing.state.is_none(), "no usable exposure binds nothing");
        assert_eq!(nothing.warnings.len(), 1);
        assert!(ExposedState::bind(&mut store, &[]).state.is_none());
    }

    #[test]
    fn binding_normalizes_overlapping_paths_per_name() {
        let mut store = SessionStore::new();
        let bound = ExposedState::bind(
            &mut store,
            &[exposure(
                "gmcp",
                None,
                &[
                    "Char.Vitals.hp",
                    "Char.Vitals",
                    "char.VITALS",
                    "Room.Info",
                    "Char.Vitals.mp",
                ],
            )],
        );
        let state = bound.state.unwrap();
        let paths: Vec<Vec<String>> = state.names()[0]
            .paths()
            .iter()
            .map(|path| path.segments().to_vec())
            .collect();
        assert_eq!(paths, vec![vec!["Char", "Vitals"], vec!["Room", "Info"]]);
        // Exposing the root subsumes everything else under the name.
        let root = ExposedState::bind(
            &mut store,
            &[exposure("gmcp", None, &["Char.Vitals", "", "Room"])],
        )
        .state
        .unwrap();
        assert_eq!(root.names()[0].paths().len(), 1);
        assert!(root.names()[0].paths()[0].segments().is_empty());
    }

    #[test]
    fn binding_unifies_the_spelling_of_shared_intermediates_within_a_name() {
        let mut store = SessionStore::new();
        let bound = ExposedState::bind(
            &mut store,
            &[exposure(
                "gmcp",
                None,
                &[
                    "Char.Vitals",
                    "char.Status",
                    "CHAR.vitals.hp",
                    "Room.Info",
                    "room.Players",
                    "ROOM.info.name",
                ],
            )],
        )
        .state
        .unwrap();
        let paths: Vec<Vec<String>> = bound.names()[0]
            .paths()
            .iter()
            .map(|path| path.segments().to_vec())
            .collect();
        // Later paths take the earlier spelling of every shared leading segment; paths at or
        // below an exposed one still drop, and the remaining segments keep their own spelling.
        assert_eq!(
            paths,
            vec![
                vec!["Char", "Vitals"],
                vec!["Char", "Status"],
                vec!["Room", "Info"],
                vec!["Room", "Players"],
            ]
        );
        // Unification is per name: another name spells its own intermediates.
        let two = ExposedState::bind(
            &mut store,
            &[
                exposure("gmcp", None, &["Char.Vitals"]),
                exposure("msdp", None, &["char.Status"]),
            ],
        )
        .state
        .unwrap();
        assert_eq!(two.names()[1].paths()[0].segments(), ["char", "Status"]);
    }

    #[test]
    fn rebinding_reuses_the_cell_until_the_engine_resets() {
        let mut store = SessionStore::new();
        let gmcp = ProducerKey::Platform(crate::session::runtime::PlatformProducer::Gmcp);
        set(&mut store, gmcp.clone(), "Char.Vitals", json!({ "hp": 1 }));
        let exposures = [exposure("gmcp", None, &["Char.Vitals"])];
        let cell_of = |state: &ExposedState| state.names()[0].paths()[0].cell().clone();

        // A second registration of the same path (a re-saved automation, a second
        // automation exposing it) shares the first one's cell.
        let first = ExposedState::bind(&mut store, &exposures).state.unwrap();
        let again = ExposedState::bind(&mut store, &exposures).state.unwrap();
        assert!(Arc::ptr_eq(&cell_of(&first), &cell_of(&again)));

        // An engine reset (a reload) drops the store's cells; the registration that follows
        // binds a fresh cell seeded from the committed tree, which survives the reset. The
        // orphaned cell belongs to the old engine and no longer follows the store.
        store.reset_engine_state();
        let rebound = ExposedState::bind(&mut store, &exposures).state.unwrap();
        assert!(!Arc::ptr_eq(&cell_of(&first), &cell_of(&rebound)));
        assert_eq!(rendered(&rebound, "gmcp", ".Char.Vitals.hp"), "1");
        set(&mut store, gmcp, "Char.Vitals", json!({ "hp": 2 }));
        assert_eq!(rendered(&rebound, "gmcp", ".Char.Vitals.hp"), "2");
        assert_eq!(rendered(&first, "gmcp", ".Char.Vitals.hp"), "1");
    }

    #[test]
    fn references_read_the_bound_cells_with_folded_walks() {
        let mut store = SessionStore::new();
        let gmcp = ProducerKey::Platform(crate::session::runtime::PlatformProducer::Gmcp);
        set(
            &mut store,
            gmcp.clone(),
            "Char.Vitals",
            json!({ "hp": 100, "maxhp": 120.5, "alive": true, "name": "Bob", "gone": null, "tags": ["a", 1] }),
        );
        set(
            &mut store,
            ProducerKey::User,
            "foo.bar",
            json!({ "deep": "baz" }),
        );
        let mut renamed = exposure("user", Some("foo"), &["bar"]);
        renamed.name_override = Some("stats".to_string());
        let state = ExposedState::bind(
            &mut store,
            &[exposure("gmcp", None, &["Char.Vitals"]), renamed],
        )
        .state
        .unwrap();

        assert_eq!(rendered(&state, "gmcp", ".char.vitals.HP"), "100");
        assert_eq!(rendered(&state, "GMCP", ".Char.Vitals.maxhp"), "120.5");
        assert_eq!(rendered(&state, "gmcp", ".Char.Vitals.alive"), "true");
        assert_eq!(rendered(&state, "gmcp", ".Char.Vitals.name"), "Bob");
        assert_eq!(rendered(&state, "gmcp", ".Char.Vitals.gone"), "");
        assert_eq!(rendered(&state, "gmcp", ".Char.Vitals.tags"), r#"["a",1]"#);
        assert_eq!(
            rendered(&state, "gmcp", ".Char.Vitals"),
            r#"{"hp":100,"maxhp":120.5,"alive":true,"name":"Bob","gone":null,"tags":["a",1]}"#
        );
        assert_eq!(rendered(&state, "gmcp", "[\"Char\"].Vitals.hp"), "100");
        // Above the exposed path, beside it, absent below it, malformed, or an unknown name.
        assert_eq!(rendered(&state, "gmcp", ".Char"), "");
        assert_eq!(rendered(&state, "gmcp", ""), "");
        assert_eq!(rendered(&state, "gmcp", ".Room.Info"), "");
        assert_eq!(rendered(&state, "gmcp", ".Char.Vitals.nope"), "");
        assert_eq!(rendered(&state, "gmcp", ".Char.Vitals.hp.deeper"), "");
        assert_eq!(rendered(&state, "gmcp", ".Char.Vitals."), "");
        assert_eq!(rendered(&state, "foo", ".bar.deep"), "");
        // The renamed handle answers to its name, relative to the handle root.
        assert_eq!(rendered(&state, "stats", ".bar.deep"), "baz");
        assert_eq!(rendered(&state, "stats", ".bar"), r#"{"deep":"baz"}"#);
        assert_eq!(rendered(&state, "stats", ".BAR.DEEP"), "baz");

        // A write in the same turn is invisible until the flush rewrites the cell.
        store
            .set(
                gmcp.clone(),
                StorePath::parse("Char.Vitals.hp").unwrap(),
                json!(7),
                IsolateId::Main,
                0,
            )
            .unwrap();
        assert_eq!(rendered(&state, "gmcp", ".Char.Vitals.hp"), "100");
        store.flush();
        assert_eq!(rendered(&state, "gmcp", ".Char.Vitals.hp"), "7");
        // A root exposure renders the whole producer tree, and reads absence as empty.
        let root = ExposedState::bind(&mut store, &[exposure("gmcp", None, &[""])])
            .state
            .unwrap();
        assert!(rendered(&root, "gmcp", "").starts_with(r#"{"Char":{"Vitals":{"hp":7"#));
        assert_eq!(rendered(&root, "gmcp", ".Char.Vitals.hp"), "7");
        let unset = ExposedState::bind(&mut store, &[exposure("msdp", None, &[""])])
            .state
            .unwrap();
        assert_eq!(rendered(&unset, "msdp", ""), "");
    }
}
