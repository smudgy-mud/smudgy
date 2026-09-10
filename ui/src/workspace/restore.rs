//! Restoration plumbing: turning a loaded template into window plans,
//! session slots, placeholders, and replay bookkeeping — plus the geometry
//! clamp and the shared vacancy/placeholder types the windows host. Driven
//! by user gesture (a named-layout apply or a server's last-session
//! restore), never at launch.
//!
//! Restore reserves the full persisted geometry immediately: every window
//! opens at its clamped stored placement with its complete split forest
//! installed, script panes standing as unbound placeholder tabs in their
//! final positions. Panes then fill **in place** as `PaneOpened`
//! definitions arrive and match by stable descriptor — the slot's stored
//! geometry wins over the script's placement request — so restore never
//! visibly reflows. Unknown panes use normal placement; placeholders whose
//! pane never materializes are reaped when the session's runtime reports
//! ready, without blocking anything else.
//!
//! The eyeball-hidden preference replays through the existing user-toggle
//! path (`report_user_hidden`) exactly once per pane. The runtime cannot
//! accept the report before it is ready, so replays owed to a session queue
//! here and flush at its `RuntimeReady`; panes materializing after that
//! replay immediately.

use std::collections::{HashMap, HashSet};

use smudgy_core::session::SessionId;
use smudgy_core::session::runtime::pane::{self, PaneKey, PaneNamespace};

use crate::pane_groups::{Blueprint, Tab};
use crate::windows::smudgy_window::PaneRef;

use super::dto;

/// A script pane's durable identity in comparable form: namespace plus
/// case-folded name — core's interning key, folded through the same
/// function the registry uses.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DescriptorKey {
    User {
        name: String,
    },
    Package {
        owner: String,
        package: String,
        name: String,
    },
}

/// The comparable key for a live pane definition's identity.
#[must_use]
pub fn descriptor_key(namespace: &PaneNamespace, name: &str) -> DescriptorKey {
    let name = pane::fold(name);
    match namespace {
        PaneNamespace::User => DescriptorKey::User { name },
        PaneNamespace::Package(pkg) => DescriptorKey::Package {
            owner: pkg.owner.to_string(),
            package: pkg.name.to_string(),
            name,
        },
    }
}

/// The comparable key for a persisted descriptor. Stored names are already
/// folded (the sanitizer guarantees it), but folding again keeps the two
/// construction paths byte-for-byte identical.
#[must_use]
pub fn descriptor_key_from_dto(namespace: &dto::Namespace, name: &str) -> DescriptorKey {
    let name = pane::fold(name);
    match namespace {
        dto::Namespace::User => DescriptorKey::User { name },
        dto::Namespace::Package {
            owner,
            name: package,
        } => DescriptorKey::Package {
            owner: owner.clone(),
            package: package.clone(),
            name,
        },
    }
}

/// The persisted identity a descriptor key stands for — the inverse of
/// [`descriptor_key_from_dto`], for serializing a pane that exists only as
/// a descriptor (a placeholder whose pane has not materialized). No live
/// definition exists to read a display name from, so the diagnostics-only
/// `display` field stays absent; identity is complete without it.
#[must_use]
pub fn identity_from_key(key: &DescriptorKey) -> dto::PaneIdentity {
    match key {
        DescriptorKey::User { name } => dto::PaneIdentity::Script {
            namespace: dto::Namespace::User,
            name: name.clone(),
            display: None,
        },
        DescriptorKey::Package {
            owner,
            package,
            name,
        } => dto::PaneIdentity::Script {
            namespace: dto::Namespace::Package {
                owner: owner.clone(),
                name: package.clone(),
            },
            name: name.clone(),
            display: None,
        },
    }
}

/// One placeholder awaiting materialization in a window: the tab standing
/// in the pane's final position, and the eyeball-hidden preference to apply
/// when the pane arrives.
#[derive(Debug, Clone, Copy)]
pub struct PendingPane {
    pub tab: crate::pane_groups::TabId,
    pub hidden: bool,
}

/// A vacated session slot, hosted by the window where the session's main
/// pane lived: its descriptors for the binding engine, its retained tabs
/// (now placeholders), and the creation ordinal adoption ties against.
#[derive(Debug)]
pub struct SessionVacancy {
    pub server: String,
    pub profile: String,
    pub ordinal: u64,
    pub main_tab: crate::pane_groups::TabId,
    pub panes: Vec<VacantPane>,
}

/// One vacated script pane: its durable identity and its retained tab.
#[derive(Debug)]
pub struct VacantPane {
    pub key: DescriptorKey,
    pub tab: crate::pane_groups::TabId,
    pub hidden: bool,
}

/// Daemon-side restoration bookkeeping: hidden replays owed to not-yet-
/// ready runtimes, which sessions have reported ready, and the vacancy
/// ordinal well.
#[derive(Debug, Default)]
pub struct RestoreState {
    pending_hidden: HashMap<SessionId, Vec<(PaneKey, bool)>>,
    ready: HashSet<SessionId>,
    next_vacancy_ordinal: u64,
}

impl RestoreState {
    /// Whether `session`'s runtime has reported ready (replays for it may
    /// be sent immediately).
    #[must_use]
    pub fn is_ready(&self, session: SessionId) -> bool {
        self.ready.contains(&session)
    }

    /// Queue one eyeball replay for a session whose runtime is not ready
    /// yet.
    pub fn owe_hidden(&mut self, session: SessionId, key: PaneKey, hidden: bool) {
        self.pending_hidden
            .entry(session)
            .or_default()
            .push((key, hidden));
    }

    /// The session's runtime is ready: drain its owed replays (send each
    /// through `report_user_hidden` now) and remember the readiness. Fires
    /// on script reloads too, harmlessly — nothing new is owed by then.
    pub fn mark_ready(&mut self, session: SessionId) -> Vec<(PaneKey, bool)> {
        self.ready.insert(session);
        self.pending_hidden.remove(&session).unwrap_or_default()
    }

    /// Forget a closed session's bookkeeping.
    pub fn forget_session(&mut self, session: SessionId) {
        self.pending_hidden.remove(&session);
        self.ready.remove(&session);
    }

    /// Mint the next vacancy ordinal (adoption prefers exact profile, then
    /// lowest ordinal — creation order).
    pub fn next_vacancy_ordinal(&mut self) -> u64 {
        let ordinal = self.next_vacancy_ordinal;
        self.next_vacancy_ordinal += 1;
        ordinal
    }
}

/// Everything one restored window needs installed: the layout blueprint,
/// the placeholder registrations, the bound panes to mark eyeball-hidden
/// locally, the hidden replays to queue, and the explicit active session.
pub struct WindowPlan {
    pub clusters: Vec<(f32, Blueprint<PaneRef>)>,
    pub pending: Vec<(SessionId, DescriptorKey, PendingPane)>,
    pub hidden_bound: Vec<PaneRef>,
    /// Replays for panes bound at install time (mains): sent at the owning
    /// session's `RuntimeReady`.
    pub replays: Vec<(SessionId, PaneKey, bool)>,
    pub active: Option<SessionId>,
}

/// Translate one persisted window into its installable plan. Panes whose
/// slot spawned no session (a failed open) drop out; the blueprint
/// assembly collapses whatever that empties.
#[must_use]
pub fn plan_window(template: &dto::Window, session_of: &HashMap<u64, SessionId>) -> WindowPlan {
    let mut plan = WindowPlan {
        clusters: Vec::with_capacity(template.clusters.len()),
        pending: Vec::new(),
        hidden_bound: Vec::new(),
        replays: Vec::new(),
        active: template
            .active_slot
            .and_then(|slot| session_of.get(&slot).copied()),
    };
    for cluster in &template.clusters {
        let node = plan_node(&cluster.root, session_of, &mut plan);
        plan.clusters.push((cluster.weight, node));
    }
    plan
}

fn plan_node(
    node: &dto::Node,
    session_of: &HashMap<u64, SessionId>,
    plan: &mut WindowPlan,
) -> Blueprint<PaneRef> {
    match node {
        dto::Node::Group(group) => {
            let mut tabs = Vec::with_capacity(group.tabs.len());
            let mut selected = 0;
            for (index, pane) in group.tabs.iter().enumerate() {
                let Some(&session) = session_of.get(&pane.slot) else {
                    continue;
                };
                let tab = match &pane.id {
                    dto::PaneIdentity::Main => {
                        let slot = PaneRef {
                            session_id: session,
                            key: smudgy_core::session::runtime::pane::MAIN_PANE_KEY,
                        };
                        if pane.hidden {
                            plan.hidden_bound.push(slot);
                        }
                        // The main pane materializes at install; its replay
                        // is owed from the start.
                        plan.replays.push((
                            session,
                            smudgy_core::session::runtime::pane::MAIN_PANE_KEY,
                            pane.hidden,
                        ));
                        Tab::bound(slot)
                    }
                    dto::PaneIdentity::Script {
                        namespace, name, ..
                    } => {
                        let tab = Tab::placeholder();
                        plan.pending.push((
                            session,
                            descriptor_key_from_dto(namespace, name),
                            PendingPane {
                                tab: tab.id(),
                                hidden: pane.hidden,
                            },
                        ));
                        tab
                    }
                };
                if index == group.selected {
                    selected = tabs.len();
                }
                tabs.push(tab);
            }
            Blueprint::Group { tabs, selected }
        }
        dto::Node::Split(split) => Blueprint::Split {
            axis: match split.axis {
                dto::Axis::Horizontal => iced::widget::pane_grid::Axis::Horizontal,
                dto::Axis::Vertical => iced::widget::pane_grid::Axis::Vertical,
            },
            sizing: match split.sizing {
                dto::Sizing::Ratio(ratio) => crate::pane_groups::SplitSizing::Ratio(ratio),
                dto::Sizing::Px { px, sized_first } => {
                    crate::pane_groups::SplitSizing::Px { px, sized_first }
                }
            },
            a: Box::new(plan_node(&split.a, session_of, plan)),
            b: Box::new(plan_node(&split.b, session_of, plan)),
        },
    }
}

/// The bounding rectangle restored windows are clamped into, in physical
/// pixels — the union of all displays.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// The OS virtual screen (the union of every display), when the platform
/// exposes it. `None` leaves window placement to the OS.
#[must_use]
#[cfg(windows)]
pub fn virtual_screen_bounds() -> Option<DisplayBounds> {
    use winapi::um::winuser::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    // SAFETY: GetSystemMetrics reads global display metrics; no pointers.
    let (x, y, width, height) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    if width <= 0 || height <= 0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    Some(DisplayBounds {
        x: x as f32,
        y: y as f32,
        width: width as f32,
        height: height as f32,
    })
}

/// See the Windows variant; other platforms report nothing and restored
/// windows take OS-chosen positions at their stored (minimum-clamped) size.
#[must_use]
#[cfg(not(windows))]
pub fn virtual_screen_bounds() -> Option<DisplayBounds> {
    None
}

/// Clamp a stored window geometry to the available displays and the current
/// minimum size. Returns the logical position to request (`None` when no
/// display bounds are known — the OS places the window) and the logical
/// size. The stored scale is the conversion context between the logical
/// coordinates in the file and the physical display bounds; mixed-DPI
/// arrangements make this approximate, which clamping tolerates by
/// construction.
#[must_use]
pub fn clamp_geometry(
    geometry: &dto::Geometry,
    bounds: Option<DisplayBounds>,
    min_size: iced::Size,
) -> (Option<iced::Point>, iced::Size) {
    let scale = if geometry.scale.is_finite() && geometry.scale > 0.0 {
        geometry.scale
    } else {
        1.0
    };
    let width = geometry.width.max(min_size.width);
    let height = geometry.height.max(min_size.height);
    let Some(bounds) = bounds else {
        return (None, iced::Size::new(width, height));
    };

    // Work in physical pixels against the display union, then convert back.
    let physical_width = (width * scale).min(bounds.width);
    let physical_height = (height * scale).min(bounds.height);
    let x = (geometry.x * scale).clamp(bounds.x, bounds.x + bounds.width - physical_width);
    let y = (geometry.y * scale).clamp(bounds.y, bounds.y + bounds.height - physical_height);
    (
        Some(iced::Point::new(x / scale, y / scale)),
        iced::Size::new(
            (physical_width / scale).max(min_size.width),
            (physical_height / scale).max(min_size.height),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_core::session::runtime::pane::MAIN_PANE_KEY;

    const MIN: iced::Size = iced::Size::new(640.0, 400.0);

    fn bounds() -> DisplayBounds {
        DisplayBounds {
            x: 0.0,
            y: 0.0,
            width: 2560.0,
            height: 1440.0,
        }
    }

    fn geometry(x: f32, y: f32, width: f32, height: f32, scale: f32) -> dto::Geometry {
        dto::Geometry {
            x,
            y,
            width,
            height,
            scale,
        }
    }

    #[test]
    fn on_screen_geometry_passes_through() {
        let (position, size) = clamp_geometry(
            &geometry(100.0, 80.0, 1600.0, 900.0, 1.0),
            Some(bounds()),
            MIN,
        );
        assert_eq!(position, Some(iced::Point::new(100.0, 80.0)));
        assert_eq!(size, iced::Size::new(1600.0, 900.0));
    }

    #[test]
    fn off_screen_positions_are_pulled_back_in() {
        // Off the right/bottom edge (a disconnected monitor): the window is
        // pulled fully back into the virtual screen.
        let (position, size) = clamp_geometry(
            &geometry(9000.0, 9000.0, 800.0, 600.0, 1.0),
            Some(bounds()),
            MIN,
        );
        assert_eq!(
            position,
            Some(iced::Point::new(2560.0 - 800.0, 1440.0 - 600.0))
        );
        assert_eq!(size, iced::Size::new(800.0, 600.0));

        // Off the left/top (negative coordinates from a removed left-hand
        // monitor).
        let (position, _) = clamp_geometry(
            &geometry(-5000.0, -50.0, 800.0, 600.0, 1.0),
            Some(bounds()),
            MIN,
        );
        assert_eq!(position, Some(iced::Point::new(0.0, 0.0)));
    }

    #[test]
    fn oversized_windows_shrink_to_the_display_union() {
        let (position, size) = clamp_geometry(
            &geometry(0.0, 0.0, 9000.0, 9000.0, 1.0),
            Some(bounds()),
            MIN,
        );
        assert_eq!(position, Some(iced::Point::new(0.0, 0.0)));
        assert_eq!(size, iced::Size::new(2560.0, 1440.0));
    }

    #[test]
    fn stored_scale_converts_logical_to_physical_for_the_clamp() {
        // A 2x window: 700 logical = 1400 physical. At x=1000 logical
        // (2000 physical) it would overflow a 2560-wide screen; the clamp
        // works in physical pixels and hands back logical coordinates.
        let (position, size) = clamp_geometry(
            &geometry(1000.0, 0.0, 700.0, 500.0, 2.0),
            Some(bounds()),
            MIN,
        );
        assert_eq!(size, iced::Size::new(700.0, 500.0));
        let position = position.expect("bounds known");
        assert!((position.x - (2560.0 - 1400.0) / 2.0).abs() < 0.01);
        assert_eq!(position.y, 0.0);
    }

    #[test]
    fn a_sanitized_micro_scale_cannot_smuggle_a_position_past_the_clamp() {
        // A micro-scale would shrink any stored position into the display
        // bounds and then blow the clamped result back up into an
        // astronomical logical coordinate. Sanitizing first clamps the
        // scale into the trusted band, which caps the logical position the
        // display clamp can hand back.
        let workspace = dto::Workspace {
            version: dto::SCHEMA_VERSION,
            sessions: vec![dto::SessionSlot {
                id: 1,
                server: "Arctic".to_string(),
                profile: "imm".to_string(),
                connect: false,
            }],
            windows: vec![dto::Window {
                id: 1,
                geometry: dto::Geometry {
                    x: 1.0e9,
                    y: 1.0e9,
                    width: 800.0,
                    height: 600.0,
                    scale: 0.001,
                },
                maximized: false,
                active_slot: None,
                clusters: vec![dto::Cluster {
                    weight: 1.0,
                    root: dto::Node::Group(dto::Group {
                        tabs: vec![dto::Pane {
                            slot: 1,
                            id: dto::PaneIdentity::Main,
                            hidden: false,
                        }],
                        selected: 0,
                    }),
                }],
            }],
        };
        let sanitized = workspace.sanitized();
        let geometry = &sanitized.windows[0].geometry;
        assert_eq!(geometry.scale, 0.25, "the scale lands on the band's floor");

        let (position, _) = clamp_geometry(geometry, Some(bounds()), MIN);
        let position = position.expect("bounds known");
        // Bounded by the display union over the band's floor — worlds away
        // from the stored 1e9.
        assert!(position.x <= bounds().width / 0.25);
        assert!(position.y <= bounds().height / 0.25);
        assert!(position.x >= 0.0 && position.y >= 0.0);
    }

    #[test]
    fn sizes_respect_the_current_minimum() {
        let (_, size) = clamp_geometry(&geometry(0.0, 0.0, 100.0, 50.0, 1.0), Some(bounds()), MIN);
        assert_eq!(size, iced::Size::new(MIN.width, MIN.height));
    }

    #[test]
    fn unknown_bounds_leave_position_to_the_os() {
        let (position, size) =
            clamp_geometry(&geometry(123.0, 456.0, 800.0, 600.0, 1.0), None, MIN);
        assert_eq!(position, None);
        assert_eq!(size, iced::Size::new(800.0, 600.0));
    }

    #[test]
    fn descriptor_keys_fold_exactly_like_the_registry() {
        let display_cased = descriptor_key(&PaneNamespace::User, "ChatLog");
        let folded = descriptor_key(&PaneNamespace::User, "chatlog");
        assert_eq!(display_cased, folded);

        // Package namespaces compare owner/name verbatim (core's key does
        // not fold them) plus the folded pane name.
        let a = descriptor_key(&PaneNamespace::package("GTanger", "mapper"), "MiniMap");
        let b = descriptor_key_from_dto(
            &dto::Namespace::Package {
                owner: "GTanger".to_string(),
                name: "mapper".to_string(),
            },
            "minimap",
        );
        assert_eq!(a, b);
        let other_owner = descriptor_key_from_dto(
            &dto::Namespace::Package {
                owner: "gtanger".to_string(),
                name: "mapper".to_string(),
            },
            "minimap",
        );
        assert_ne!(a, other_owner);
    }

    #[test]
    fn identity_from_key_inverts_the_descriptor_key_exactly() {
        // A placeholder serialized from its descriptor alone must parse
        // back to the very same key, or restore-after-quick-quit would
        // stage a placeholder no PaneOpened can ever claim.
        let keys = [
            DescriptorKey::User {
                name: "chat".to_string(),
            },
            DescriptorKey::Package {
                owner: "GTanger".to_string(),
                package: "mapper".to_string(),
                name: "minimap".to_string(),
            },
        ];
        for key in keys {
            let identity = identity_from_key(&key);
            let dto::PaneIdentity::Script {
                namespace,
                name,
                display,
            } = &identity
            else {
                panic!("descriptor keys only ever name script panes");
            };
            assert_eq!(display, &None, "no live definition, no display name");
            assert_eq!(descriptor_key_from_dto(namespace, name), key);
        }
    }

    #[test]
    fn window_plans_bind_mains_and_stage_script_placeholders() {
        let template: dto::Window = serde_json::from_value(serde_json::json!({
            "id": 1,
            "geometry": {"x": 0.0, "y": 0.0, "width": 1600.0, "height": 900.0, "scale": 1.0},
            "active_slot": 2,
            "clusters": [
                {"weight": 1.0, "root": {"split": {
                    "axis": "vertical",
                    "sizing": {"ratio": 0.7},
                    "a": {"group": {"tabs": [{"slot": 1, "id": "main", "hidden": true}], "selected": 0}},
                    "b": {"group": {"tabs": [
                        {"slot": 1, "id": {"script": {"namespace": "user", "name": "chat"}}, "hidden": true},
                        {"slot": 2, "id": "main"}
                    ], "selected": 1}}
                }}},
                {"weight": 1.0, "root": {"group": {"tabs": [{"slot": 9, "id": "main"}], "selected": 0}}}
            ]
        }))
        .expect("template parses");
        // Slot 9 spawned no session (a failed open): its pane drops out.
        let session_of: HashMap<u64, SessionId> =
            [(1, SessionId::from(0)), (2, SessionId::from(1))].into();

        let plan = plan_window(&template, &session_of);
        assert_eq!(plan.active, Some(SessionId::from(1)));
        assert_eq!(
            plan.clusters.len(),
            2,
            "the dead cluster empties at assembly"
        );
        // One placeholder staged for the chat pane, hidden preference kept.
        assert_eq!(plan.pending.len(), 1);
        let (session, key, pending) = &plan.pending[0];
        assert_eq!(*session, SessionId::from(0));
        assert_eq!(
            *key,
            DescriptorKey::User {
                name: "chat".to_string()
            }
        );
        assert!(pending.hidden);
        // The hidden main is marked locally and owed a replay; the visible
        // main is owed its (false) replay too — once per restored pane.
        assert_eq!(plan.hidden_bound.len(), 1);
        assert_eq!(plan.replays.len(), 2);
        assert!(
            plan.replays
                .contains(&(SessionId::from(0), MAIN_PANE_KEY, true))
        );
        assert!(
            plan.replays
                .contains(&(SessionId::from(1), MAIN_PANE_KEY, false))
        );

        // Selection followed its tab: in the b group the survivor list is
        // [chat placeholder, slot-2 main] and index 1 was selected.
        let Blueprint::Split { b, .. } = &plan.clusters[0].1 else {
            panic!("split expected");
        };
        let Blueprint::Group { tabs, selected } = b.as_ref() else {
            panic!("group expected");
        };
        assert_eq!(tabs.len(), 2);
        assert_eq!(*selected, 1);
    }

    #[test]
    fn restore_bookkeeping_queues_replays_until_ready() {
        let mut state = RestoreState::default();
        let session = SessionId::from(3);
        assert!(!state.is_ready(session));
        state.owe_hidden(session, MAIN_PANE_KEY, true);

        let drained = state.mark_ready(session);
        assert_eq!(drained, vec![(MAIN_PANE_KEY, true)]);
        assert!(state.is_ready(session));
        // Ready sessions replay immediately; nothing accumulates.
        assert!(state.mark_ready(session).is_empty());

        assert_eq!(state.next_vacancy_ordinal(), 0);
        assert_eq!(state.next_vacancy_ordinal(), 1);
    }
}
