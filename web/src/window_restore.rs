//! Migration from the old tab-scoped restore formats. Current snapshots live
//! in `IndexedDB`, with exactly one saved window for this browser origin.

use serde::Deserialize;
use smudgy_session_model::workspace as dto;

const KEY: &str = "smudgy-last-window-v2";
const LEGACY_KEY: &str = "smudgy-last-window-v1";

#[derive(Debug, Deserialize)]
struct LegacySlot {
    server: String,
    profile: String,
    connect: bool,
    weight: f32,
}

#[derive(Debug, Deserialize)]
struct LegacySnapshot {
    version: u32,
    slots: Vec<LegacySlot>,
}

fn migrate_legacy(raw: &str) -> Option<dto::Workspace> {
    let legacy: LegacySnapshot = serde_json::from_str(raw).ok()?;
    if legacy.version != 1 || legacy.slots.is_empty() {
        return None;
    }
    let mut sessions = Vec::with_capacity(legacy.slots.len());
    let mut clusters = Vec::with_capacity(legacy.slots.len());
    for (index, slot) in legacy.slots.into_iter().enumerate() {
        if slot.server.is_empty()
            || slot.profile.is_empty()
            || !slot.weight.is_finite()
            || slot.weight <= 0.0
        {
            return None;
        }
        let id = u64::try_from(index).ok()?.checked_add(1)?;
        sessions.push(dto::SessionSlot {
            id,
            server: slot.server,
            profile: slot.profile,
            connect: slot.connect,
        });
        clusters.push(dto::Cluster {
            weight: slot.weight,
            root: dto::Node::Group(dto::Group {
                tabs: vec![dto::Pane {
                    slot: id,
                    id: dto::PaneIdentity::Main,
                    hidden: false,
                }],
                selected: 0,
            }),
        });
    }
    dto::Workspace {
        version: dto::SCHEMA_VERSION,
        sessions,
        windows: vec![dto::Window {
            id: 1,
            geometry: dto::Geometry::default(),
            maximized: false,
            active_slot: Some(1),
            clusters,
        }],
    }
    .sanitized_single_window()
    .ok()
}

pub fn load_legacy() -> Option<dto::Workspace> {
    let storage = web_sys::window()?.session_storage().ok()??;
    if let Some(raw) = storage.get_item(KEY).ok()?
        && let Ok(workspace) = serde_json::from_str::<dto::Workspace>(&raw)
        && let Ok(workspace) = workspace.sanitized_single_window()
    {
        return Some(workspace);
    }
    migrate_legacy(&storage.get_item(LEGACY_KEY).ok()??)
}

pub fn clear_legacy() {
    if let Some(storage) = web_sys::window().and_then(|window| window.session_storage().ok()?) {
        let _ = storage.remove_item(KEY);
        let _ = storage.remove_item(LEGACY_KEY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_weights_migrate_to_one_window_workspace() {
        let workspace = migrate_legacy(r#"{"version":1,"slots":[{"server":"Arctic","profile":"one","connect":true,"weight":2.0},{"server":"Arctic","profile":"two","connect":false,"weight":1.0}]}"#).unwrap();
        assert_eq!(workspace.sessions.len(), 2);
        assert_eq!(workspace.windows.len(), 1);
        assert_eq!(workspace.windows[0].clusters.len(), 2);
        assert!((workspace.windows[0].clusters[0].weight - 2.0).abs() < f32::EPSILON);
        assert!(!workspace.sessions[1].connect);
    }
}
