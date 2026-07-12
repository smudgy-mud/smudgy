//! Shared session-store widget-binding cells (`smudgy/docs/interop.md` §7).
//!
//! A binding connects a widget property to a session-store path **host-side**: the store
//! (in `core`, on the session thread) writes the bound path's latest committed snapshot into
//! a [`StoreBindingCell`], and the widget's render closure (on the UI thread) reads the cell
//! every frame — a store update repaints the widget without a V8 tick.
//!
//! These types live **here** for the same crate-DAG reason as [`WidgetsEnabled`]
//! (`crate::WidgetsEnabled`): the cell readers are in the leaf `smudgy_widgets` crate, the
//! writer is `core`'s session store, and `smudgy_cloud` is the one crate both already depend
//! on. `core` seeds the [`StoreBindings`] registry into each isolate's `OpState`; the widget
//! build ops resolve a script's binding token (an id minted by `core`'s bind op) to its cell
//! there and capture the `Arc` in the render closure.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use parking_lot::RwLock;

use crate::store_node::Node;

/// One live binding value: the latest committed snapshot of the bound `(producer, path)`,
/// `Null` when the path is absent. Written by the session thread at store flush; read
/// lock-free by UI-thread render closures each frame. The cell is the store tree's one
/// cross-thread slot, which is exactly why it (and not the tree's interior edges) is an
/// `ArcSwap`; the [`Node`] it pins shares structure with the committed tree, so writing a
/// snapshot is an `Arc` bump, not a subtree clone.
#[derive(Debug)]
pub struct StoreBindingCell {
    value: ArcSwap<Node>,
}

impl StoreBindingCell {
    #[must_use]
    pub fn new(value: impl Into<Node>) -> Self {
        Self {
            value: ArcSwap::from_pointee(value.into()),
        }
    }

    /// The latest flushed snapshot (lock-free load).
    #[must_use]
    pub fn load(&self) -> Arc<Node> {
        self.value.load_full()
    }

    pub fn set(&self, value: impl Into<Node>) {
        self.value.store(Arc::new(value.into()));
    }
}

/// The session's binding-id → cell registry. `core`'s session store mints ids (deduped per
/// bound path) and owns the id's meaning; this map is the hand-off that lets the leaf widget
/// ops resolve a token id to its cell without naming any `core` type. Engine-scoped like the
/// store's watchers: `core` clears it on every engine rebuild (the old engine's tokens die
/// with the widgets that held them).
#[derive(Clone, Debug, Default)]
pub struct StoreBindings {
    cells: Arc<RwLock<HashMap<u32, Arc<StoreBindingCell>>>>,
}

impl StoreBindings {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, id: u32, cell: Arc<StoreBindingCell>) {
        self.cells.write().insert(id, cell);
    }

    /// The cell a binding token addresses, or `None` for a stale/unknown id (a token minted
    /// by a previous engine generation).
    #[must_use]
    pub fn cell(&self, id: u32) -> Option<Arc<StoreBindingCell>> {
        self.cells.read().get(&id).cloned()
    }

    pub fn clear(&self) {
        self.cells.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cells_round_trip_and_clear() {
        let bindings = StoreBindings::new();
        let cell = Arc::new(StoreBindingCell::new(json!(1)));
        bindings.insert(0, cell.clone());
        assert_eq!(*bindings.cell(0).unwrap().load(), json!(1));
        cell.set(json!({ "hp": 2 }));
        assert_eq!(*bindings.cell(0).unwrap().load(), json!({ "hp": 2 }));
        assert!(bindings.cell(1).is_none(), "unknown ids resolve to nothing");
        bindings.clear();
        assert!(bindings.cell(0).is_none(), "cleared on engine rebuild");
    }
}
