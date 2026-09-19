//! Same-process invalidation hints, scoped to a verified API origin and viewer.
//! Hints carry no documents or authority; subscribers still fetch through their
//! own credential-bound sync path. Weak entries retain no dormant accounts.

use parking_lot::Mutex;
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Weak},
};
use tokio::sync::watch;
use uuid::Uuid;

type Scope = (String, Uuid);
static SCOPES: LazyLock<Mutex<HashMap<Scope, Weak<CloudChanges>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) struct CloudChanges {
    subscribers: Mutex<HashMap<Uuid, watch::Sender<u64>>>,
}

impl CloudChanges {
    pub(super) fn for_viewer(origin: &str, viewer: Uuid) -> Arc<Self> {
        let mut scopes = SCOPES.lock();
        scopes.retain(|_, scope| scope.strong_count() > 0);
        let slot = scopes
            .entry((origin.trim_end_matches('/').to_string(), viewer))
            .or_default();
        if let Some(scope) = slot.upgrade() {
            return scope;
        }
        let scope = Arc::new(Self {
            subscribers: Mutex::new(HashMap::new()),
        });
        *slot = Arc::downgrade(&scope);
        scope
    }

    pub(super) fn subscribe(&self, writer: Uuid) -> watch::Receiver<u64> {
        self.subscribers
            .lock()
            .entry(writer)
            .or_insert_with(|| watch::channel(0).0)
            .subscribe()
    }

    pub(super) fn publish(&self, writer: Uuid) {
        let mut subscribers = self.subscribers.lock();
        subscribers.retain(|_, sender| sender.receiver_count() > 0);
        for (subscriber, sender) in subscribers.iter() {
            if *subscriber != writer {
                sender.send_modify(|generation| *generation = generation.wrapping_add(1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_are_coalesced_and_isolated_by_origin_and_verified_viewer() {
        let viewer = Uuid::new_v4();
        let first = CloudChanges::for_viewer("https://one.example/api/", viewer);
        let same = CloudChanges::for_viewer("https://one.example/api", viewer);
        let other_origin = CloudChanges::for_viewer("https://two.example/api", viewer);
        let other_viewer = CloudChanges::for_viewer("https://one.example/api", Uuid::new_v4());
        let mut receiver = same.subscribe(Uuid::new_v4());
        let origin_receiver = other_origin.subscribe(Uuid::new_v4());
        let viewer_receiver = other_viewer.subscribe(Uuid::new_v4());
        for _ in 0..3 {
            first.publish(Uuid::nil());
        }
        assert!(receiver.has_changed().unwrap());
        assert_eq!(*receiver.borrow_and_update(), 3);
        assert!(!origin_receiver.has_changed().unwrap());
        assert!(!viewer_receiver.has_changed().unwrap());
        let weak = Arc::downgrade(&first);
        drop((first, same));
        assert!(weak.upgrade().is_none());
        assert!(receiver.has_changed().is_err());
    }

    #[test]
    fn own_writes_do_not_hide_foreign_hints() {
        let scope = CloudChanges::for_viewer("https://writers.example/api", Uuid::new_v4());
        let writer = Uuid::new_v4();
        let mut changes = scope.subscribe(writer);
        scope.publish(writer);
        assert!(!changes.has_changed().unwrap());
        scope.publish(Uuid::new_v4());
        scope.publish(writer);
        assert!(changes.has_changed().unwrap());
        assert_eq!(*changes.borrow_and_update(), 1);
    }
}
