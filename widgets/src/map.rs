use std::{cell::RefCell, collections::HashMap, mem, rc::Rc};

use smudgy_cloud::{AreaId, Mapper, Uuid};
use smudgy_map_widget::{
    Update,
    map_view::{self, MapView, Message as MapMessage},
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
        let map_ptr = self.view.as_ptr();
        let element = unsafe { (&*map_ptr).view() };
        unsafe { mem::transmute::<_, map_view::Element<'static, MapMessage>>(element) }
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
