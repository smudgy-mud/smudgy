//! The `__smudgy_fire_state` object an exposing inline body reads: one property per exposed
//! name, each a deep-frozen snapshot of the store values the automation exposes.
//!
//! Materialization happens per change, not per fire. A leaf (one bound cell's snapshot) is
//! built directly from its [`Node`] into frozen V8 objects the first time the cell holds
//! that snapshot and reused, by identity, for every later fire that finds the same `Arc` in
//! the cell; the per-automation state object is likewise rebuilt only when one of its leaves
//! changed. A fire whose exposed values did not change therefore loads one pointer per
//! exposed path, compares it, and reuses the cached globals, creating no V8 object and
//! allocating nothing.
//!
//! The cache is part of the isolate bundle: `v8::Global` handles are isolate-bound and die
//! with it, which is also when the store's binding cells are rebound.

use std::collections::HashMap;
use std::sync::Arc;

use deno_core::v8;
use smudgy_cloud::{Node, StoreBindingCell};

use super::super::state_exposure::{ExposedName, ExposedState};
use super::ScriptId;

/// The frozen V8 form of one cell's snapshot, keyed by the cell's address.
struct LeafEntry {
    /// Pins the cell so its address stays this entry's for the cache's lifetime.
    _cell: Arc<StoreBindingCell>,
    /// The snapshot the object was built from; identity decides reuse.
    source: Arc<Node>,
    object: v8::Global<v8::Value>,
}

/// One automation's state object and the leaf snapshots it was built from, in the
/// automation's `(name, path)` order.
struct StateEntry {
    sources: Vec<Arc<Node>>,
    object: v8::Global<v8::Object>,
}

#[derive(Default)]
pub(super) struct FireStateCache {
    leaves: HashMap<usize, LeafEntry>,
    states: HashMap<ScriptId, StateEntry>,
    key: Option<v8::Global<v8::String>>,
}

impl FireStateCache {
    /// The global property name the exposing wrapper reads.
    pub(super) fn key<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
    ) -> v8::Local<'s, v8::String> {
        let key = self.key.get_or_insert_with(|| {
            let local = v8::String::new(scope, "__smudgy_fire_state").unwrap();
            v8::Global::new(scope, local)
        });
        v8::Local::new(scope, &*key)
    }

    /// The state object for `script_id`'s exposures as the cells hold them now: the cached
    /// object when no exposed path changed since it was built, else a rebuilt one.
    pub(super) fn state_object<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        script_id: ScriptId,
        exposed: &ExposedState,
    ) -> v8::Local<'s, v8::Object> {
        if let Some(entry) = self.states.get(&script_id)
            && Self::unchanged(entry, exposed)
        {
            return v8::Local::new(scope, &entry.object);
        }
        let count = exposed.names().len();
        let mut sources = Vec::with_capacity(count);
        let mut names: Vec<v8::Local<v8::Name>> = Vec::with_capacity(count);
        let mut values: Vec<v8::Local<v8::Value>> = Vec::with_capacity(count);
        for name in exposed.names() {
            values.push(self.name_object(scope, name, &mut sources));
            names.push(v8_string(scope, name.name()).into());
        }
        // No prototype: the object is a `with` scope, and `Object.prototype` members must not
        // shadow the user API or globals inside the body.
        let null = v8::null(scope).into();
        let object = v8::Object::with_prototype_and_properties(scope, null, &names, &values);
        object.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
        self.states.insert(
            script_id,
            StateEntry {
                sources,
                object: v8::Global::new(scope, object),
            },
        );
        object
    }

    /// Whether every exposed path's cell still holds the snapshot `entry` was built from.
    fn unchanged(entry: &StateEntry, exposed: &ExposedState) -> bool {
        let mut sources = entry.sources.iter();
        for name in exposed.names() {
            for path in name.paths() {
                let current = path.cell().load();
                if !sources
                    .next()
                    .is_some_and(|source| Arc::ptr_eq(source, &current))
                {
                    return false;
                }
            }
        }
        true
    }

    /// The frozen form of `cell`'s current snapshot, materialized only when the snapshot is
    /// not the one already cached for this cell.
    fn leaf<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        cell: &Arc<StoreBindingCell>,
    ) -> (Arc<Node>, v8::Local<'s, v8::Value>) {
        let current = cell.load();
        let key = Arc::as_ptr(cell) as usize;
        if let Some(entry) = self.leaves.get(&key)
            && Arc::ptr_eq(&entry.source, &current)
        {
            return (current, v8::Local::new(scope, &entry.object));
        }
        let object = materialize(scope, &current);
        self.leaves.insert(
            key,
            LeafEntry {
                _cell: cell.clone(),
                source: current.clone(),
                object: v8::Global::new(scope, object),
            },
        );
        (current, object)
    }

    /// One name's value: the leaf itself for a root exposure, else an ordinary object
    /// carrying every intermediate up to each exposed path, with the exposed node at its
    /// end (`undefined` when the store has nothing there). Intermediates are keyed by the
    /// path's segments as bound (the exposure's spelling, unified across the name's paths
    /// under the fold), so paths that share a prefix meet at one object; they are frozen
    /// once every path is placed, and leaves are frozen by construction.
    fn name_object<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        name: &ExposedName,
        sources: &mut Vec<Arc<Node>>,
    ) -> v8::Local<'s, v8::Value> {
        if let [path] = name.paths()
            && path.segments().is_empty()
        {
            let (source, value) = self.leaf(scope, path.cell());
            let value = exposed_value(scope, &source, value);
            sources.push(source);
            return value;
        }
        let root = v8::Object::new(scope);
        let mut created = vec![root];
        for path in name.paths() {
            let (source, value) = self.leaf(scope, path.cell());
            let value = exposed_value(scope, &source, value);
            sources.push(source);
            let Some((last, intermediates)) = path.segments().split_last() else {
                continue;
            };
            let mut cursor = root;
            for segment in intermediates {
                let key = v8_string(scope, segment);
                // Own properties only: an inherited `constructor` or `toString` is not an
                // intermediate this name created.
                let existing = (cursor.has_own_property(scope, key.into()) == Some(true))
                    .then(|| cursor.get(scope, key.into()))
                    .flatten()
                    .and_then(|value| v8::Local::<v8::Object>::try_from(value).ok());
                cursor = if let Some(object) = existing {
                    object
                } else {
                    let object = v8::Object::new(scope);
                    cursor.create_data_property(scope, key.into(), object.into());
                    created.push(object);
                    object
                };
            }
            let key = v8_string(scope, last);
            cursor.create_data_property(scope, key.into(), value);
        }
        for object in created {
            object.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
        }
        root.into()
    }
}

/// The value placed at an exposed path: the leaf, except that a `Null` cell (the store has
/// nothing there, or holds a null) reads as `undefined`.
fn exposed_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    source: &Node,
    leaf: v8::Local<'s, v8::Value>,
) -> v8::Local<'s, v8::Value> {
    if source.is_null() {
        v8::undefined(scope).into()
    } else {
        leaf
    }
}

/// Build the deep-frozen V8 form of `node` straight from the tree: scalars as primitives,
/// containers as frozen ordinary objects and arrays keyed by the store's display spelling.
fn materialize<'s>(scope: &mut v8::PinScope<'s, '_>, node: &Node) -> v8::Local<'s, v8::Value> {
    match node {
        Node::Null => v8::null(scope).into(),
        Node::Bool(flag) => v8::Boolean::new(scope, *flag).into(),
        Node::Number(number) => v8::Number::new(scope, number.as_f64().unwrap_or(f64::NAN)).into(),
        Node::String(text) => v8_string(scope, text).into(),
        Node::Array(array) => {
            let items: Vec<v8::Local<v8::Value>> = array
                .items()
                .iter()
                .map(|item| materialize(scope, item))
                .collect();
            let array = v8::Array::new_with_elements(scope, &items);
            array.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
            array.into()
        }
        Node::Object(object) => {
            let out = v8::Object::new(scope);
            for (key, child) in object.iter() {
                let value = materialize(scope, child);
                let key = v8_string(scope, key);
                out.create_data_property(scope, key.into(), value);
            }
            out.set_integrity_level(scope, v8::IntegrityLevel::Frozen);
            out.into()
        }
    }
}

fn v8_string<'s>(scope: &mut v8::PinScope<'s, '_>, text: &str) -> v8::Local<'s, v8::String> {
    v8::String::new(scope, text).unwrap_or_else(|| v8::String::empty(scope))
}
