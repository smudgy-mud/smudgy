use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{Arc, Mutex},
};

use smudgy_cloud::{AreaId, Mapper, Uuid};
use smudgy_map_widget::{
    Update,
    map_view::{self, MapView, Message as MapMessage, SharedMapView},
};

thread_local! {
    static ACTIVE_STORE: RefCell<Option<MapStore>> = const { RefCell::new(None) };
}

pub type MapWidgetId = u64;

#[derive(Clone)]
pub struct MapStore {
    maps: Rc<RefCell<HashMap<MapWidgetId, Rc<MapEntry>>>>,
}

#[derive(Clone)]
pub struct MapHandle {
    entry: Rc<MapEntry>,
}

struct MapEntry {
    id: MapWidgetId,
    view: Rc<RefCell<MapView>>,
}

impl Default for MapStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MapStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            maps: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    #[must_use]
    pub fn ensure_map(&self, mapper: Mapper, id: MapWidgetId) -> MapHandle {
        if let Some(entry) = self.maps.borrow().get(&id) {
            return MapHandle {
                entry: Rc::clone(entry),
            };
        }

        let area_id = AreaId(Uuid::nil());
        let entry = Rc::new(MapEntry {
            id,
            view: Rc::new(RefCell::new(MapView::new(mapper, area_id))),
        });

        self.maps.borrow_mut().insert(id, Rc::clone(&entry));

        MapHandle { entry }
    }

    pub fn remove_map(&self, id: MapWidgetId) {
        self.maps.borrow_mut().remove(&id);
    }

    #[must_use]
    pub fn update_map(
        &self,
        id: MapWidgetId,
        message: MapMessage,
    ) -> Option<Update<MapMessage, map_view::Event>> {
        self.maps
            .borrow()
            .get(&id)
            .map(|entry| entry.update(message))
    }

    pub fn set_current_location(&self, area_id: AreaId, room_number: Option<i32>) {
        let entries: Vec<_> = self.maps.borrow().values().cloned().collect();
        for entry in entries {
            entry.set_current_location(area_id, room_number);
        }
    }
}

impl MapHandle {
    pub fn id(&self) -> MapWidgetId {
        self.entry.id
    }

    pub fn element(&self) -> map_view::Element<'static, MapMessage> {
        self.entry.element()
    }
}

impl MapEntry {
    fn update(&self, message: MapMessage) -> Update<MapMessage, map_view::Event> {
        self.view.borrow_mut().update(message)
    }

    fn set_current_location(&self, area_id: AreaId, room_number: Option<i32>) {
        self.view
            .borrow_mut()
            .update(MapMessage::SetPlayerLocation(area_id, room_number));
    }

    fn element(&self) -> map_view::Element<'static, MapMessage> {
        SharedMapView::new(Rc::clone(&self.view)).element()
    }
}

/// The cross-thread half of map-entry garbage collection: a queue of widget
/// ids whose last render closure has dropped. The render closure is the only
/// path back into the [`MapStore`] (via `ensure_map`), so once it is gone —
/// unmount, engine-rebuild clear, or cppgc collecting the JS element handle,
/// on whichever thread that happens — the id can never be rendered again and
/// its entry is safe to free. The UI thread drains the queue at the start of
/// each widget render pass.
#[derive(Clone, Default)]
pub struct MapReaper {
    dead: Arc<Mutex<Vec<MapWidgetId>>>,
}

impl MapReaper {
    /// Mint the drop guard that travels inside a map widget's render closure.
    #[must_use]
    pub fn guard(&self, id: MapWidgetId) -> MapReapGuard {
        MapReapGuard {
            id,
            dead: Arc::clone(&self.dead),
        }
    }

    /// Drain the ids queued since the previous call.
    #[must_use]
    pub fn take(&self) -> Vec<MapWidgetId> {
        self.dead
            .lock()
            .map(|mut dead| std::mem::take(&mut *dead))
            .unwrap_or_default()
    }
}

/// Queues its widget id on drop. Lives inside the render closure, so the last
/// closure clone to drop — from any thread — reports the id.
pub struct MapReapGuard {
    id: MapWidgetId,
    dead: Arc<Mutex<Vec<MapWidgetId>>>,
}

impl MapReapGuard {
    #[must_use]
    pub fn id(&self) -> MapWidgetId {
        self.id
    }
}

impl Drop for MapReapGuard {
    fn drop(&mut self) {
        // A poisoned lock means a panic is already unwinding; skipping the
        // push (a one-entry leak) beats a double panic.
        if let Ok(mut dead) = self.dead.lock() {
            dead.push(self.id);
        }
    }
}

pub fn with_store_context<R>(store: &MapStore, f: impl FnOnce() -> R) -> R {
    ACTIVE_STORE.with(|slot| {
        let previous = slot.replace(Some(store.clone()));
        let result = f();
        slot.replace(previous);
        result
    })
}

pub fn with_active_store<R>(f: impl FnOnce(&MapStore) -> R) -> Option<R> {
    ACTIVE_STORE
        .with(|slot| slot.borrow().clone())
        .map(|store| f(&store))
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::LocalBackend;

    fn test_mapper(tag: &str) -> Mapper {
        let cache_dir = std::env::temp_dir()
            .join("smudgy-widgets-test")
            .join(format!("{tag}-{}", std::process::id()));
        Mapper::new(
            Arc::new(LocalBackend::new(cache_dir.join("local"))),
            cache_dir,
        )
    }

    /// The guard rides in the render closure, whose clones are the JS element
    /// handle plus any mount: only the LAST clone dropping queues the id, and
    /// a drained id does not repeat.
    #[test]
    fn reaper_fires_on_last_closure_drop() {
        let reaper = MapReaper::default();
        let guard = reaper.guard(7);
        let closure: Arc<dyn Fn() -> MapWidgetId> = Arc::new(move || guard.id());
        let mount = Arc::clone(&closure);

        drop(closure);
        assert!(
            reaper.take().is_empty(),
            "a surviving clone must keep the entry alive"
        );

        drop(mount);
        assert_eq!(reaper.take(), vec![7]);
        assert!(reaper.take().is_empty(), "drained ids must not repeat");
    }

    /// Draining the reaper into `remove_map` frees the entry; an id that was
    /// never rendered (`ensure_map` never ran) drains as a no-op.
    #[tokio::test]
    async fn reaped_entries_leave_the_store() {
        let mapper = test_mapper("reap");
        let store = MapStore::new();
        let reaper = MapReaper::default();

        let rendered = reaper.guard(1);
        let never_rendered = reaper.guard(2);
        let handle = store.ensure_map(mapper, rendered.id());
        drop(handle);
        assert!(
            store
                .update_map(1, MapMessage::SetHoveredRoom(None))
                .is_some(),
            "the entry must exist while its closure lives"
        );

        drop(rendered);
        drop(never_rendered);
        for id in reaper.take() {
            store.remove_map(id);
        }
        assert!(
            store
                .update_map(1, MapMessage::SetHoveredRoom(None))
                .is_none(),
            "a reaped entry must be gone"
        );
    }
}
