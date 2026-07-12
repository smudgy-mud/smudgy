use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};

use super::SessionId;
use super::runtime::Runtime;

/// Per-session v8 inspector endpoints (set once a session's runtime is built, when
/// debugging is enabled). Kept separate from the `Runtime` entry because the bound
/// address isn't known until after the runtime thread constructs its script engine.
static INSPECTOR_ADDRESSES: OnceLock<Mutex<HashMap<SessionId, SocketAddr>>> = OnceLock::new();

fn inspector_addresses() -> &'static Mutex<HashMap<SessionId, SocketAddr>> {
    INSPECTOR_ADDRESSES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record the v8 inspector endpoint for a session.
///
/// # Panics
///
/// Panics if the inspector-address mutex is poisoned.
pub fn set_inspector_address(session_id: SessionId, addr: SocketAddr) {
    inspector_addresses().lock().unwrap().insert(session_id, addr);
}

/// Get the v8 inspector endpoint for a session, if one is listening (debug mode).
/// The UI's "Show Inspector" affordance spawns `smudgy_inspector <addr>` with this.
///
/// # Panics
///
/// Panics if the inspector-address mutex is poisoned.
#[must_use]
pub fn get_inspector_address(session_id: SessionId) -> Option<SocketAddr> {
    inspector_addresses()
        .lock()
        .unwrap()
        .get(&session_id)
        .copied()
}

/// Shared map of active session runtimes, keyed by session id.
type SessionRegistry = Arc<Mutex<HashMap<SessionId, Arc<Runtime>>>>;

/// Global registry of all active sessions
static SESSION_REGISTRY: OnceLock<SessionRegistry> = OnceLock::new();

/// Get the global session registry
pub fn get_registry() -> SessionRegistry {
    SESSION_REGISTRY
        .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
        .clone()
}

/// Register a new session in the global registry
///
/// # Panics
///
/// Panics if the session-registry mutex is poisoned.
pub fn register_session(session_id: SessionId, runtime: Arc<Runtime>) {
    let registry = get_registry();
    let mut sessions = registry.lock().unwrap();
    sessions.insert(session_id, runtime);
    log::info!("Registered session {session_id} in global registry");
}

/// Unregister a session from the global registry
///
/// # Panics
///
/// Panics if the session-registry or inspector-address mutex is poisoned.
pub fn unregister_session(session_id: SessionId) {
    let registry = get_registry();
    let mut sessions = registry.lock().unwrap();
    inspector_addresses().lock().unwrap().remove(&session_id);
    if sessions.remove(&session_id).is_some() {
        log::info!("Unregistered session {session_id} from global registry");
    } else {
        log::warn!(
            "Attempted to unregister non-existent session {session_id}"
        );
    }
}

/// Get all active session IDs
///
/// # Panics
///
/// Panics if the session-registry mutex is poisoned.
#[must_use]
pub fn get_all_session_ids() -> Vec<SessionId> {
    let registry = get_registry();
    let sessions = registry.lock().unwrap();
    sessions.keys().copied().collect()
}

/// Get a specific runtime by session ID
///
/// # Panics
///
/// Panics if the session-registry mutex is poisoned.
#[must_use]
pub fn get_runtime(session_id: SessionId) -> Option<Arc<Runtime>> {
    let registry = get_registry();
    let sessions = registry.lock().unwrap();
    sessions.get(&session_id).cloned()
}

/// Get the number of active sessions
///
/// # Panics
///
/// Panics if the session-registry mutex is poisoned.
#[must_use]
pub fn session_count() -> usize {
    let registry = get_registry();
    let sessions = registry.lock().unwrap();
    sessions.len()
}
