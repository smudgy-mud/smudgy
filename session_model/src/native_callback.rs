//! Thread-safe leases into an owner-thread registry of native callbacks.
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

/// A callback address and liveness lease. This contains no V8 handle and can be
/// cloned or dropped by the UI, including while its creating isolate is retiring.
/// The widget's isolate-instance token must be checked before resolving it.
#[derive(Clone, Debug)]
pub struct CallbackLease {
    id: u64,
    lease: Arc<()>,
}

struct Entry<F> {
    function: F,
    lease: Weak<()>,
}

/// Only accessed on the script thread. Dead leases are swept at the next build;
/// all remaining handles drop with OpState on the same thread during teardown.
pub struct CallbackRegistry<F> {
    entries: BTreeMap<u64, Entry<F>>,
    next_id: u64,
}

impl<F> Default for CallbackRegistry<F> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            next_id: 0,
        }
    }
}

impl<F> CallbackRegistry<F> {
    pub fn register(&mut self, function: F) -> CallbackLease {
        self.entries
            .retain(|_, entry| entry.lease.strong_count() != 0);
        let id = self.next_id;
        self.next_id = id.checked_add(1).expect("widget callback id exhausted");
        let lease = Arc::new(());
        self.entries.insert(
            id,
            Entry {
                function,
                lease: Arc::downgrade(&lease),
            },
        );
        CallbackLease { id, lease }
    }

    pub fn get(&self, callback: &CallbackLease) -> Option<&F> {
        self.entries
            .get(&callback.id)
            .filter(|entry| entry.lease.ptr_eq(&Arc::downgrade(&callback.lease)))
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
            "sweeping runs on the owner thread"
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
}
