//! Thread-safe leases into an owner-thread registry of native callbacks.
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak, mpsc},
};

/// A callback address and liveness lease. This contains no V8 handle and can be
/// cloned or dropped by the UI, including while its creating isolate is retiring.
/// The widget's isolate-instance token must be checked before resolving it.
#[derive(Clone, Debug)]
pub struct CallbackLease(Arc<Liveness>);

/// Shared by every clone of a lease. Dropping the last clone queues the id for
/// its own registry, from whichever thread drops it; once that registry is gone
/// the report goes nowhere.
#[derive(Debug)]
struct Liveness {
    id: u64,
    deaths: mpsc::Sender<u64>,
}

impl Drop for Liveness {
    fn drop(&mut self) {
        let _ = self.deaths.send(self.id);
    }
}

struct Entry<F> {
    function: F,
    lease: Weak<Liveness>,
}

/// Only accessed on the script thread. The next build removes the entries whose
/// leases have died, so their functions drop here; the queue is lock-free and
/// the build costs one removal per death, independent of the live count. All
/// remaining handles drop with OpState on the same thread during teardown.
pub struct CallbackRegistry<F> {
    entries: BTreeMap<u64, Entry<F>>,
    next_id: u64,
    deaths: mpsc::Sender<u64>,
    dead: mpsc::Receiver<u64>,
}

impl<F> Default for CallbackRegistry<F> {
    fn default() -> Self {
        let (deaths, dead) = mpsc::channel();
        Self {
            entries: BTreeMap::new(),
            next_id: 0,
            deaths,
            dead,
        }
    }
}

impl<F> CallbackRegistry<F> {
    pub fn register(&mut self, function: F) -> CallbackLease {
        for id in self.dead.try_iter() {
            self.entries.remove(&id);
        }
        let id = self.next_id;
        self.next_id = id.checked_add(1).expect("widget callback id exhausted");
        let lease = Arc::new(Liveness {
            id,
            deaths: self.deaths.clone(),
        });
        self.entries.insert(
            id,
            Entry {
                function,
                lease: Arc::downgrade(&lease),
            },
        );
        CallbackLease(lease)
    }

    pub fn get(&self, callback: &CallbackLease) -> Option<&F> {
        self.entries
            .get(&callback.0.id)
            .filter(|entry| entry.lease.ptr_eq(&Arc::downgrade(&callback.0)))
            .map(|entry| &entry.function)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leases_cross_threads_but_functions_drop_only_with_the_registry() {
        struct Function(std::sync::mpsc::Sender<std::thread::ThreadId>);
        impl Drop for Function {
            fn drop(&mut self) {
                self.0.send(std::thread::current().id()).unwrap();
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let owner = std::thread::current().id();
        let mut registry = CallbackRegistry::default();
        let callback = registry.register(Function(tx.clone()));
        let queued = callback.clone();
        std::thread::spawn(move || drop(callback)).join().unwrap();
        assert!(rx.try_recv().is_err());
        assert!(registry.get(&queued).is_some());
        std::thread::spawn(move || drop(queued)).join().unwrap();
        assert!(rx.try_recv().is_err());
        let survivor = registry.register(Function(tx));
        assert_eq!(
            rx.recv().unwrap(),
            owner,
            "reclaiming runs on the owner thread"
        );
        drop(registry);
        assert_eq!(
            rx.recv().unwrap(),
            owner,
            "teardown runs on the owner thread"
        );
        std::thread::spawn(move || drop(survivor)).join().unwrap();
    }

    #[test]
    fn a_foreign_or_retired_registry_cannot_resolve_a_reused_id() {
        let mut original = CallbackRegistry::default();
        let callback = original.register(1);
        let mut replacement = CallbackRegistry::default();
        let current = replacement.register(2);
        assert_eq!(original.get(&callback), Some(&1));
        assert_eq!(replacement.get(&callback), None);
        assert_eq!(replacement.get(&current), Some(&2));
        drop(original);
        assert_eq!(replacement.get(&callback), None);
    }

    #[test]
    fn only_the_last_clone_of_a_lease_frees_its_entry() {
        let mut registry = CallbackRegistry::default();
        let first = registry.register(1);
        let clone = first.clone();
        drop(first);
        let _second = registry.register(2);
        assert_eq!(
            registry.entries.len(),
            2,
            "a surviving clone keeps the entry"
        );
        assert_eq!(registry.get(&clone), Some(&1));
        drop(clone);
        let _third = registry.register(3);
        assert_eq!(registry.entries.len(), 2, "the last clone frees the entry");
    }

    #[test]
    fn a_retired_registry_ignores_late_reports() {
        let mut original = CallbackRegistry::default();
        let late = original.register(1);
        drop(original);
        let mut replacement = CallbackRegistry::default();
        let current = replacement.register(2);
        // Both leases hold id 0; the late one reports only to its own registry.
        drop(late);
        let _next = replacement.register(3);
        assert_eq!(replacement.get(&current), Some(&2));
    }
}
