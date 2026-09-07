//! Named-layout application planning: a pure projection from a template,
//! the live workspace, and the apply mode to a complete, validated mutation
//! plan — produced in full before any window is touched.
//!
//! The projection composes the binding engine (sessions onto slots) with
//! window matching (template windows onto live in-scope windows, ordinally
//! by stable-id creation order — template windows with no realizable
//! referenced content for the mode are excluded first, so a window the
//! scoping empties can never claim a live window) and settles every
//! structural question up front: fewer live windows (script applies fold
//! the excess into the ordinally-last in-scope window; user restores adopt
//! visually empty live windows — the initiating window first — and create
//! windows only past those), more live windows (the extras keep their
//! arrangement, minus any pane the template claims), unmatched sessions
//! (user restores ask keep-or-close; script applies keep silently),
//! unmatched slots (runtime vacancies later opens adopt), and live panes
//! the template does not know (riders that follow their group, or re-place
//! within their current window when the group dissolves — never through
//! the cross-window placement chain, which could displace an out-of-scope
//! window).
//!
//! Because the keep-or-close prompt is asynchronous and the workspace
//! drifts while it is open, a plan with open [`ApplyPlan::questions`] (or
//! pending [`ApplyPlan::spawns`]) is a *preview*: the projection re-runs
//! with the answers (and the spawned sessions) as inputs, and only a plan
//! that is [`ApplyPlan::is_executable`] — revalidated against the live
//! workspace it will actually mutate — reaches the executor.
//!
//! Scope is the footprint predicate of `docs/panes.md` §18: only windows
//! hosting at least one pane of a referenced server participate, and for
//! script applies the referenced set is further cut to the calling
//! session's server, so a script can never move another server's panes
//! even when the template names them. [`validate_conservation`] proves the
//! resulting partition: every in-scope pane lands exactly once, and no
//! out-of-scope pane is mentioned at all.

use std::collections::{HashMap, HashSet};

use smudgy_core::session::SessionId;
use smudgy_core::session::runtime::pane::{MAIN_PANE_KEY, PaneKey};

use crate::pane_groups::{Blueprint, Tab, TabId};
use crate::windows::smudgy_window::PaneRef;

use super::binding;
use super::dto;
use super::restore::{DescriptorKey, PendingPane, descriptor_key_from_dto};

/// Who initiated the apply — the axis every footprint-scoping guardrail
/// (`docs/panes.md` §18) hangs off.
#[derive(Debug, Clone, Copy)]
pub enum ApplyMode<'a> {
    /// Script-initiated: layout-only and content-scoped to the calling
    /// session's server. Never spawns, closes, prompts, or touches OS
    /// windows.
    Script { calling_server: &'a str },
    /// User-initiated: the full flow — spawn missing slots, ask keep-or-
    /// close for omitted live sessions, adopt visually empty live windows,
    /// create whatever is still missing.
    User {
        /// Stable id of the window whose surface drove the apply. When it
        /// is visually empty it is adopted before any other empty window,
        /// so a restore started from a fresh window's connect surface
        /// lands in that window rather than beside it.
        initiating: Option<u64>,
    },
}

impl ApplyMode<'_> {
    fn is_script(self) -> bool {
        matches!(self, Self::Script { .. })
    }
}

/// One live session, in open (session-id) order — the live ordinal the
/// binding engine ties against.
#[derive(Debug, Clone)]
pub struct LiveSessionInfo {
    pub id: SessionId,
    pub server: String,
    pub profile: String,
}

/// One bound live pane as the planner sees it: its grid reference, its
/// durable descriptor (`None` for the main pane), and its current
/// eyeball-hidden state (riders keep theirs).
#[derive(Debug, Clone)]
pub struct LivePane {
    pub pane: PaneRef,
    pub descriptor: Option<DescriptorKey>,
    pub hidden: bool,
}

/// One live window in stable-id creation order: its stable workspace id and
/// its bound panes grouped exactly as its tab groups group them (group
/// membership is what riders ride along with).
#[derive(Debug, Clone)]
pub struct LiveWindow {
    pub stable_id: u64,
    /// Whether the window shows only the connect surface: no bound panes
    /// or pending panes of live sessions. Invisible vacancies left by closed
    /// sessions do not prevent adoption.
    pub empty: bool,
    pub groups: Vec<Vec<LivePane>>,
}

/// The live workspace as a pure value: sessions in open order, windows in
/// stable-id creation order.
#[derive(Debug, Clone, Default)]
pub struct LiveWorkspace {
    pub sessions: Vec<LiveSessionInfo>,
    pub windows: Vec<LiveWindow>,
}

/// A user's answer for one live session the template omits. Closing is
/// never silent: absent an answer the session is a question, not a close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OmittedAnswer {
    Keep,
    Close,
}

/// One planned tab: a live pane placed by the template, a bound session's
/// not-yet-materialized pane staged as a placeholder, or an unmatched
/// slot's pane staged as part of a runtime vacancy.
#[derive(Debug, Clone, PartialEq)]
pub enum PlannedTab {
    Bound {
        pane: PaneRef,
        hidden: bool,
    },
    Pending {
        session: SessionId,
        descriptor: DescriptorKey,
        hidden: bool,
    },
    Vacant {
        /// Index into [`ApplyPlan::vacancies`].
        vacancy: usize,
        /// `None` marks the vacancy's retained main tab.
        descriptor: Option<DescriptorKey>,
        hidden: bool,
    },
}

/// The planned split forest, mirroring the template's shape with resolved
/// tabs (riders already folded in).
#[derive(Debug, Clone, PartialEq)]
pub enum PlanNode {
    Group {
        tabs: Vec<PlannedTab>,
        selected: usize,
    },
    Split {
        axis: dto::Axis,
        sizing: dto::Sizing,
        a: Box<PlanNode>,
        b: Box<PlanNode>,
    },
}

/// Where a planned window lands: an existing in-scope live window (rebuilt
/// in place), or — user restores only — a visually empty live window
/// adopted at the template's stored geometry, or a window to create at
/// that geometry.
#[derive(Debug, Clone, PartialEq)]
pub enum WindowTarget {
    Existing {
        stable_id: u64,
    },
    /// A visually empty live window claimed for a template window with no
    /// ordinal match. The executor installs into it exactly as it would
    /// into an [`Self::Existing`] target, and additionally applies the
    /// stored geometry — the same treatment a created window gets.
    Adopted {
        stable_id: u64,
        geometry: dto::Geometry,
        maximized: bool,
    },
    New {
        geometry: dto::Geometry,
        maximized: bool,
    },
}

/// One window's complete planned content.
#[derive(Debug, Clone)]
pub struct PlannedWindow {
    pub target: WindowTarget,
    pub clusters: Vec<(f32, PlanNode)>,
    /// The explicitly planned active session (user mode, from the
    /// template's active slot); `None` preserves the window's current
    /// active session, repaired if it left.
    pub active: Option<SessionId>,
}

/// One unmatched template slot: the descriptors a later open adopts by.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedVacancy {
    pub server: String,
    pub profile: String,
}

/// One session a user restore must spawn before the plan can execute.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnSlot {
    pub slot: u64,
    pub server: String,
    pub profile: String,
    pub connect: bool,
}

/// The complete mutation plan.
#[derive(Debug, Clone)]
pub struct ApplyPlan {
    /// Whether the plan was projected under script scoping (the executor
    /// must never close or create OS windows for such a plan).
    pub script_scoped: bool,
    pub windows: Vec<PlannedWindow>,
    pub vacancies: Vec<PlannedVacancy>,
    /// Template-claimed panes to strip out of extra in-scope live windows
    /// (windows beyond the template's count, which otherwise keep their
    /// arrangement), keyed by the source window's stable id.
    pub removals: Vec<(u64, PaneRef)>,
    /// Live panes the template does not know, already folded into
    /// [`Self::windows`] — listed for reporting and conservation checks.
    pub riders: Vec<PaneRef>,
    /// Live sessions the template omits and no answer covers yet. Non-empty
    /// only for user restores; the projection re-runs once they are
    /// answered.
    pub questions: Vec<SessionId>,
    /// Omitted sessions the user answered `Close`.
    pub close_sessions: Vec<SessionId>,
    /// Slots a user restore spawns before re-projecting. Always empty for
    /// script applies.
    pub spawns: Vec<SpawnSlot>,
}

impl ApplyPlan {
    /// Whether the plan may be executed as-is: nothing left to ask, nothing
    /// left to spawn.
    #[must_use]
    pub fn is_executable(&self) -> bool {
        self.questions.is_empty() && self.spawns.is_empty()
    }
}

/// Why no plan could be projected.
#[derive(Debug, Clone, PartialEq)]
pub enum ApplyError {
    /// The template holds no windows — or none with realizable referenced
    /// content for the mode (nothing to apply).
    EmptyTemplate,
    /// Script mode: the template does not reference the calling session's
    /// server, so a content-scoped apply would have nothing to move.
    NotReferenced { server: String },
    /// Script mode: no live window hosts a calling-server pane (no live
    /// footprint to rearrange, and no window to fold into).
    NoLiveFootprint,
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyTemplate => write!(f, "the layout holds no windows"),
            Self::NotReferenced { server } => {
                write!(f, "the layout does not reference server '{server}'")
            }
            Self::NoLiveFootprint => {
                write!(f, "no live window hosts a pane of the layout's servers")
            }
        }
    }
}

impl std::error::Error for ApplyError {}

/// The scope both the projection and the conservation check derive from the
/// same inputs, so they can never disagree about what is in play.
struct Scope<'a> {
    /// Servers whose panes the apply may move.
    referenced: HashSet<&'a str>,
    /// Template slots of referenced servers, in template ordinal order
    /// (indices into `template.sessions`).
    participating: Vec<usize>,
    /// Indices into `live.windows` hosting at least one referenced-server
    /// pane, in creation order.
    in_scope: Vec<usize>,
}

fn scope<'a>(
    template: &'a dto::Workspace,
    live: &'a LiveWorkspace,
    mode: ApplyMode<'a>,
) -> Result<Scope<'a>, ApplyError> {
    if template.windows.is_empty() {
        return Err(ApplyError::EmptyTemplate);
    }
    let template_servers: HashSet<&str> = template
        .sessions
        .iter()
        .map(|slot| slot.server.as_str())
        .collect();
    let referenced: HashSet<&str> = match mode {
        ApplyMode::Script { calling_server } => {
            if !template_servers.contains(calling_server) {
                return Err(ApplyError::NotReferenced {
                    server: calling_server.to_string(),
                });
            }
            std::iter::once(calling_server).collect()
        }
        ApplyMode::User { .. } => template_servers,
    };
    let participating: Vec<usize> = template
        .sessions
        .iter()
        .enumerate()
        .filter(|(_, slot)| referenced.contains(slot.server.as_str()))
        .map(|(index, _)| index)
        .collect();
    let server_of: HashMap<SessionId, &str> = live
        .sessions
        .iter()
        .map(|session| (session.id, session.server.as_str()))
        .collect();
    let in_scope: Vec<usize> = live
        .windows
        .iter()
        .enumerate()
        .filter(|(_, window)| {
            window.groups.iter().flatten().any(|pane| {
                server_of
                    .get(&pane.pane.session_id)
                    .is_some_and(|server| referenced.contains(server))
            })
        })
        .map(|(index, _)| index)
        .collect();
    if in_scope.is_empty() && mode.is_script() {
        return Err(ApplyError::NoLiveFootprint);
    }
    Ok(Scope {
        referenced,
        participating,
        in_scope,
    })
}

fn has_main(node: &dto::Node) -> bool {
    match node {
        dto::Node::Group(group) => group
            .tabs
            .iter()
            .any(|pane| matches!(pane.id, dto::PaneIdentity::Main)),
        dto::Node::Split(split) => has_main(&split.a) || has_main(&split.b),
    }
}

/// Limit automatic restore to the profile the user actually opened. Matching
/// uses the usual exact-profile then same-server fallback. Other saved profiles
/// and their windows are removed before planning, so automatic restore can never
/// open a session or displace another server's panes.
#[must_use]
pub fn for_opened_profile(template: &dto::Workspace, session: &LiveSessionInfo) -> dto::Workspace {
    let slots: Vec<_> = template
        .sessions
        .iter()
        .map(|slot| binding::SlotDescriptor {
            server: &slot.server,
            profile: &slot.profile,
        })
        .collect();
    let bound = binding::bind(
        &slots,
        &[binding::LiveSession {
            id: session.id,
            server: &session.server,
            profile: &session.profile,
        }],
    );
    let mut projected = template.clone();
    projected.sessions = template
        .sessions
        .iter()
        .zip(bound)
        .filter_map(|(slot, bound)| {
            bound.map(|_| dto::SessionSlot {
                profile: session.profile.clone(),
                ..slot.clone()
            })
        })
        .collect();
    let mut projected = projected.sanitized();
    // The main pane must stay in the window where the user opened it, even
    // when the saved window creation order put a detached script pane first.
    if let Some(index) = projected.windows.iter().position(|window| {
        window
            .clusters
            .iter()
            .any(|cluster| has_main(&cluster.root))
    }) {
        let main = projected.windows.remove(index);
        projected.windows.insert(0, main);
    }
    projected
}

/// Project the complete mutation plan. Pure: no window, store, or disk
/// access — everything the executor will do is decided here and can be
/// checked by [`validate_conservation`] before anything moves.
pub fn plan_apply(
    template: &dto::Workspace,
    live: &LiveWorkspace,
    mode: ApplyMode<'_>,
    answers: &HashMap<SessionId, OmittedAnswer>,
) -> Result<ApplyPlan, ApplyError> {
    let scope = scope(template, live, mode)?;

    // Bind live sessions onto the participating slots.
    let slots: Vec<binding::SlotDescriptor<'_>> = scope
        .participating
        .iter()
        .map(|&index| binding::SlotDescriptor {
            server: &template.sessions[index].server,
            profile: &template.sessions[index].profile,
        })
        .collect();
    let live_sessions: Vec<binding::LiveSession<'_, SessionId>> = live
        .sessions
        .iter()
        .map(|session| binding::LiveSession {
            id: session.id,
            server: &session.server,
            profile: &session.profile,
        })
        .collect();
    let bound = binding::bind(&slots, &live_sessions);

    let mut session_of_slot: HashMap<u64, SessionId> = HashMap::new();
    let mut bound_sessions: HashSet<SessionId> = HashSet::new();
    let mut vacancies: Vec<PlannedVacancy> = Vec::new();
    let mut vacancy_of_slot: HashMap<u64, usize> = HashMap::new();
    let mut spawns: Vec<SpawnSlot> = Vec::new();
    for (&slot_index, binding_result) in scope.participating.iter().zip(&bound) {
        let slot = &template.sessions[slot_index];
        match binding_result {
            Some(session) => {
                session_of_slot.insert(slot.id, *session);
                bound_sessions.insert(*session);
            }
            None => {
                vacancy_of_slot.insert(slot.id, vacancies.len());
                vacancies.push(PlannedVacancy {
                    server: slot.server.clone(),
                    profile: slot.profile.clone(),
                });
                if !mode.is_script() {
                    spawns.push(SpawnSlot {
                        slot: slot.id,
                        server: slot.server.clone(),
                        profile: slot.profile.clone(),
                        connect: slot.connect,
                    });
                }
            }
        }
    }
    let participating_slots: HashSet<u64> = scope
        .participating
        .iter()
        .map(|&index| template.sessions[index].id)
        .collect();

    // Omitted sessions: live, on a referenced server, bound to no slot.
    let mut questions: Vec<SessionId> = Vec::new();
    let mut close_sessions: Vec<SessionId> = Vec::new();
    if !mode.is_script() {
        for session in &live.sessions {
            if !scope.referenced.contains(session.server.as_str())
                || bound_sessions.contains(&session.id)
            {
                continue;
            }
            match answers.get(&session.id) {
                Some(OmittedAnswer::Close) => close_sessions.push(session.id),
                Some(OmittedAnswer::Keep) => {}
                None => questions.push(session.id),
            }
        }
    }
    let close_set: HashSet<SessionId> = close_sessions.iter().copied().collect();

    // Window matching considers only template windows with realizable
    // referenced content for this mode. A window the scoping empties (a
    // foreign-only window under script scoping, say) would still claim a
    // live window in a purely ordinal pairing — and since the executor
    // leaves an empty realization's live window untouched, any pane of
    // that window the plan places elsewhere would end up hosted twice.
    let effective: Vec<usize> = template
        .windows
        .iter()
        .enumerate()
        .filter(|(_, window)| {
            window.clusters.iter().any(|cluster| {
                node_has_realizable_content(&cluster.root, &participating_slots, &session_of_slot)
            })
        })
        .map(|(index, _)| index)
        .collect();
    if effective.is_empty() {
        return Err(ApplyError::EmptyTemplate);
    }

    // Ordinal matching: realizable template windows in list order against
    // in-scope live windows in creation order.
    let template_count = effective.len();
    let matched_count = template_count.min(scope.in_scope.len());
    let planned_count = if mode.is_script() {
        matched_count
    } else {
        template_count
    };
    // Planned index a realizable template window's content lands in: its
    // ordinal match, except that a script apply folds overflow into the
    // last in-scope window (never a partial application).
    let win_target = |position: usize| -> usize {
        if mode.is_script() {
            position.min(matched_count.saturating_sub(1))
        } else {
            position
        }
    };

    // A user restore adopts visually empty live windows before creating
    // new ones: an unmatched template window would otherwise open a fresh
    // OS window while an empty one (a fresh launch's connect surface, say)
    // sat unused beside it. The initiating window comes first, then the
    // remaining empty windows in stable-id creation order. Empty windows
    // host no panes, so they are never in scope — adoption and ordinal
    // matching can never claim the same live window. Script applies never
    // touch OS windows and never adopt.
    let adoptable: Vec<usize> = match mode {
        ApplyMode::Script { .. } => Vec::new(),
        ApplyMode::User { initiating } => {
            let mut empties: Vec<usize> = live
                .windows
                .iter()
                .enumerate()
                .filter(|(_, window)| window.empty)
                .map(|(index, _)| index)
                .collect();
            if let Some(front) = initiating.and_then(|stable_id| {
                empties
                    .iter()
                    .position(|&index| live.windows[index].stable_id == stable_id)
            }) {
                empties[..=front].rotate_right(1);
            }
            empties
        }
    };

    let mut planned: Vec<PlannedWindow> = Vec::with_capacity(planned_count);
    for (position, &template_index) in effective.iter().take(planned_count).enumerate() {
        let target = if position < matched_count {
            WindowTarget::Existing {
                stable_id: live.windows[scope.in_scope[position]].stable_id,
            }
        } else {
            let window = &template.windows[template_index];
            match adoptable.get(position - matched_count) {
                Some(&adopted) => WindowTarget::Adopted {
                    stable_id: live.windows[adopted].stable_id,
                    geometry: window.geometry.clone(),
                    maximized: window.maximized,
                },
                None => WindowTarget::New {
                    geometry: window.geometry.clone(),
                    maximized: window.maximized,
                },
            }
        };
        planned.push(PlannedWindow {
            target,
            clusters: Vec::new(),
            active: None,
        });
    }

    // A vacancy's script panes may stand only in the window its retained
    // main lands in (a cross-window vacancy ghost could never be adopted
    // whole, so it is not staged at all — the recorded vacate rule).
    let mut vacant_main_window: HashMap<u64, usize> = HashMap::new();
    for (position, &template_index) in effective.iter().enumerate() {
        for cluster in &template.windows[template_index].clusters {
            collect_vacant_mains(
                &cluster.root,
                win_target(position),
                &vacancy_of_slot,
                &mut vacant_main_window,
            );
        }
    }

    // Index the live panes the template can claim.
    let mut live_script: HashMap<(SessionId, DescriptorKey), PaneRef> = HashMap::new();
    for window in &live.windows {
        for pane in window.groups.iter().flatten() {
            if let Some(descriptor) = &pane.descriptor {
                live_script.insert((pane.pane.session_id, descriptor.clone()), pane.pane);
            }
        }
    }

    // Build every realizable template window's tree into its planned
    // window.
    let mut placements: HashMap<PaneRef, (usize, usize)> = HashMap::new();
    let mut group_counters = vec![0usize; planned.len()];
    for (position, &template_index) in effective.iter().enumerate() {
        let window = &template.windows[template_index];
        let target = win_target(position);
        let mut builder = Builder {
            session_of_slot: &session_of_slot,
            participating: &participating_slots,
            vacancy_of_slot: &vacancy_of_slot,
            vacant_main_window: &vacant_main_window,
            live_script: &live_script,
            placements: &mut placements,
            group_counters: &mut group_counters,
        };
        for cluster in &window.clusters {
            if let Some(node) = builder.build_node(&cluster.root, target) {
                planned[target].clusters.push((cluster.weight, node));
            }
        }
        if !mode.is_script()
            && planned[target].active.is_none()
            && let Some(active) = window
                .active_slot
                .and_then(|slot| session_of_slot.get(&slot).copied())
        {
            planned[target].active = Some(active);
        }
    }

    // Riders and removals over the in-scope windows.
    let mut riders: Vec<PaneRef> = Vec::new();
    let mut removals: Vec<(u64, PaneRef)> = Vec::new();
    for (position, &window_index) in scope.in_scope.iter().enumerate() {
        let window = &live.windows[window_index];
        if position >= matched_count {
            // Extra live window: keeps its arrangement; only panes the
            // template claimed elsewhere leave it.
            for pane in window.groups.iter().flatten() {
                if placements.contains_key(&pane.pane) {
                    removals.push((window.stable_id, pane.pane));
                }
            }
            continue;
        }
        for group in &window.groups {
            let anchor = group
                .iter()
                .find_map(|pane| placements.get(&pane.pane).copied());
            let unplaced: Vec<&LivePane> = group
                .iter()
                .filter(|pane| {
                    !placements.contains_key(&pane.pane)
                        && !close_set.contains(&pane.pane.session_id)
                })
                .collect();
            if unplaced.is_empty() {
                continue;
            }
            match anchor {
                // Riders follow the group their template-known mates landed
                // in, wherever it went.
                Some((planned_index, group_index)) => {
                    for pane in unplaced {
                        let pushed = push_rider(
                            &mut planned[planned_index].clusters,
                            group_index,
                            PlannedTab::Bound {
                                pane: pane.pane,
                                hidden: pane.hidden,
                            },
                        );
                        debug_assert!(pushed, "anchor group indices are plan-derived");
                        riders.push(pane.pane);
                    }
                }
                // The whole group is unknown to the template: it dissolves,
                // and its members re-place together within their current
                // window — never through the cross-window placement chain.
                None => {
                    let tabs: Vec<PlannedTab> = unplaced
                        .iter()
                        .map(|pane| {
                            riders.push(pane.pane);
                            PlannedTab::Bound {
                                pane: pane.pane,
                                hidden: pane.hidden,
                            }
                        })
                        .collect();
                    let weight = even_share_weight(&planned[position].clusters);
                    planned[position]
                        .clusters
                        .push((weight, PlanNode::Group { tabs, selected: 0 }));
                }
            }
        }
    }

    Ok(ApplyPlan {
        script_scoped: mode.is_script(),
        windows: planned,
        vacancies,
        removals,
        riders,
        questions,
        close_sessions,
        spawns,
    })
}

/// The template-tree builder for one template window.
struct Builder<'a> {
    session_of_slot: &'a HashMap<u64, SessionId>,
    participating: &'a HashSet<u64>,
    vacancy_of_slot: &'a HashMap<u64, usize>,
    vacant_main_window: &'a HashMap<u64, usize>,
    live_script: &'a HashMap<(SessionId, DescriptorKey), PaneRef>,
    placements: &'a mut HashMap<PaneRef, (usize, usize)>,
    group_counters: &'a mut Vec<usize>,
}

impl Builder<'_> {
    fn build_node(&mut self, node: &dto::Node, target: usize) -> Option<PlanNode> {
        match node {
            dto::Node::Group(group) => self.build_group(group, target),
            dto::Node::Split(split) => {
                let a = self.build_node(&split.a, target);
                let b = self.build_node(&split.b, target);
                match (a, b) {
                    (Some(a), Some(b)) => Some(PlanNode::Split {
                        axis: split.axis,
                        sizing: split.sizing,
                        a: Box::new(a),
                        b: Box::new(b),
                    }),
                    (Some(only), None) | (None, Some(only)) => Some(only),
                    (None, None) => None,
                }
            }
        }
    }

    fn build_group(&mut self, group: &dto::Group, target: usize) -> Option<PlanNode> {
        let mut tabs: Vec<PlannedTab> = Vec::with_capacity(group.tabs.len());
        let mut placed: Vec<Option<PaneRef>> = Vec::with_capacity(group.tabs.len());
        let mut selected = 0;
        for (index, pane) in group.tabs.iter().enumerate() {
            let Some(tab) = self.build_tab(pane, target) else {
                continue;
            };
            if index == group.selected {
                selected = tabs.len();
            }
            placed.push(match &tab {
                PlannedTab::Bound { pane, .. } => Some(*pane),
                _ => None,
            });
            tabs.push(tab);
        }
        if tabs.is_empty() {
            return None;
        }
        let group_index = self.group_counters[target];
        self.group_counters[target] += 1;
        for pane in placed.into_iter().flatten() {
            self.placements.insert(pane, (target, group_index));
        }
        Some(PlanNode::Group { tabs, selected })
    }

    fn build_tab(&mut self, pane: &dto::Pane, target: usize) -> Option<PlannedTab> {
        if !self.participating.contains(&pane.slot) {
            // A slot outside the referenced set (a foreign server under
            // script scoping): its template content is not realized.
            return None;
        }
        if let Some(&session) = self.session_of_slot.get(&pane.slot) {
            return Some(match &pane.id {
                dto::PaneIdentity::Main => PlannedTab::Bound {
                    pane: PaneRef {
                        session_id: session,
                        key: MAIN_PANE_KEY,
                    },
                    hidden: pane.hidden,
                },
                dto::PaneIdentity::Script {
                    namespace, name, ..
                } => {
                    let descriptor = descriptor_key_from_dto(namespace, name);
                    match self.live_script.get(&(session, descriptor.clone())) {
                        Some(&live_pane) => PlannedTab::Bound {
                            pane: live_pane,
                            hidden: pane.hidden,
                        },
                        // The bound session has no such pane yet: stage a
                        // placeholder its materialization will claim.
                        None => PlannedTab::Pending {
                            session,
                            descriptor,
                            hidden: pane.hidden,
                        },
                    }
                }
            });
        }
        let vacancy = *self.vacancy_of_slot.get(&pane.slot)?;
        match &pane.id {
            dto::PaneIdentity::Main => Some(PlannedTab::Vacant {
                vacancy,
                descriptor: None,
                hidden: pane.hidden,
            }),
            dto::PaneIdentity::Script {
                namespace, name, ..
            } => {
                if self.vacant_main_window.get(&pane.slot) == Some(&target) {
                    Some(PlannedTab::Vacant {
                        vacancy,
                        descriptor: Some(descriptor_key_from_dto(namespace, name)),
                        hidden: pane.hidden,
                    })
                } else {
                    // The vacancy's main lands elsewhere: a retained script
                    // tab here could never be adopted with it.
                    None
                }
            }
        }
    }
}

/// Whether any pane under `node` realizes as planned content for the
/// current scope: a participating slot's pane that is either bound (main
/// and script tabs both realize, live or pending) or an unmatched slot's
/// main (which realizes as a vacancy tab). A vacancy's script tab realizes
/// only beside its main, so it never counts on its own; a foreign slot's
/// pane never realizes at all.
fn node_has_realizable_content(
    node: &dto::Node,
    participating: &HashSet<u64>,
    session_of_slot: &HashMap<u64, SessionId>,
) -> bool {
    match node {
        dto::Node::Group(group) => group.tabs.iter().any(|pane| {
            participating.contains(&pane.slot)
                && (matches!(pane.id, dto::PaneIdentity::Main)
                    || session_of_slot.contains_key(&pane.slot))
        }),
        dto::Node::Split(split) => {
            node_has_realizable_content(&split.a, participating, session_of_slot)
                || node_has_realizable_content(&split.b, participating, session_of_slot)
        }
    }
}

/// Record, per unmatched slot, the planned window its main pane lands in.
fn collect_vacant_mains(
    node: &dto::Node,
    target: usize,
    vacancy_of_slot: &HashMap<u64, usize>,
    out: &mut HashMap<u64, usize>,
) {
    match node {
        dto::Node::Group(group) => {
            for pane in &group.tabs {
                if matches!(pane.id, dto::PaneIdentity::Main)
                    && vacancy_of_slot.contains_key(&pane.slot)
                {
                    out.entry(pane.slot).or_insert(target);
                }
            }
        }
        dto::Node::Split(split) => {
            collect_vacant_mains(&split.a, target, vacancy_of_slot, out);
            collect_vacant_mains(&split.b, target, vacancy_of_slot, out);
        }
    }
}

/// Append `tab` to the `group_index`-th group (DFS order over the planned
/// clusters). `false` when no such group exists.
fn push_rider(clusters: &mut [(f32, PlanNode)], group_index: usize, tab: PlannedTab) -> bool {
    let mut counter = 0usize;
    let mut tab = Some(tab);
    for (_, node) in clusters.iter_mut() {
        if push_rider_node(node, group_index, &mut counter, &mut tab) {
            return true;
        }
    }
    false
}

fn push_rider_node(
    node: &mut PlanNode,
    group_index: usize,
    counter: &mut usize,
    tab: &mut Option<PlannedTab>,
) -> bool {
    match node {
        PlanNode::Group { tabs, .. } => {
            let here = *counter;
            *counter += 1;
            if here == group_index
                && let Some(tab) = tab.take()
            {
                tabs.push(tab);
                return true;
            }
            false
        }
        PlanNode::Split { a, b, .. } => {
            push_rider_node(a, group_index, counter, tab)
                || push_rider_node(b, group_index, counter, tab)
        }
    }
}

/// The average existing cluster weight — a rider cluster takes an even
/// share, matching the live model's new-cluster rule.
fn even_share_weight(clusters: &[(f32, PlanNode)]) -> f32 {
    if clusters.is_empty() {
        1.0
    } else {
        clusters.iter().map(|(weight, _)| *weight).sum::<f32>() / clusters.len() as f32
    }
}

// ---------------------------------------------------------------------
// Realization: planned windows into installable layout material
// ---------------------------------------------------------------------

/// One realized vacancy: everything a `SessionVacancy` needs except the
/// ordinal, which the executor mints from the shared well.
#[derive(Debug)]
pub struct RealizedVacancy {
    pub server: String,
    pub profile: String,
    pub main_tab: TabId,
    pub panes: Vec<super::restore::VacantPane>,
}

/// One planned window turned into installable material: the blueprint, the
/// placeholder registrations, the vacancy records, the local hidden marks,
/// and the eyeball replays the executor owes or sends.
#[derive(Debug)]
pub struct RealizedWindow {
    pub clusters: Vec<(f32, Blueprint<PaneRef>)>,
    pub pending: Vec<(SessionId, DescriptorKey, PendingPane)>,
    pub vacancies: Vec<RealizedVacancy>,
    pub hidden: Vec<PaneRef>,
    pub replays: Vec<(SessionId, PaneKey, bool)>,
    pub active: Option<SessionId>,
}

/// Turn one planned window into its installable material, minting the
/// placeholder tabs. Pure aside from tab-id minting.
#[must_use]
pub fn realize_window(window: &PlannedWindow, vacancies: &[PlannedVacancy]) -> RealizedWindow {
    let mut realized = RealizedWindow {
        clusters: Vec::with_capacity(window.clusters.len()),
        pending: Vec::new(),
        vacancies: Vec::new(),
        hidden: Vec::new(),
        replays: Vec::new(),
        active: window.active,
    };
    // Vacancy assembly: retained tabs collected per vacancy index, the main
    // marker splitting them into the record's main tab and pane list.
    let mut vacant_mains: HashMap<usize, TabId> = HashMap::new();
    let mut vacant_panes: HashMap<usize, Vec<super::restore::VacantPane>> = HashMap::new();
    for (weight, node) in &window.clusters {
        let blueprint = realize_node(node, &mut realized, &mut vacant_mains, &mut vacant_panes);
        realized.clusters.push((*weight, blueprint));
    }
    for (vacancy_index, main_tab) in vacant_mains {
        let descriptor = &vacancies[vacancy_index];
        realized.vacancies.push(RealizedVacancy {
            server: descriptor.server.clone(),
            profile: descriptor.profile.clone(),
            main_tab,
            panes: vacant_panes.remove(&vacancy_index).unwrap_or_default(),
        });
    }
    // Retained script tabs whose main never realized in this window have no
    // adoptable home; realize_node staged none (the planner already drops
    // them), so anything left here is a plan inconsistency.
    debug_assert!(vacant_panes.is_empty(), "vacant panes without a main");
    realized
}

fn realize_node(
    node: &PlanNode,
    realized: &mut RealizedWindow,
    vacant_mains: &mut HashMap<usize, TabId>,
    vacant_panes: &mut HashMap<usize, Vec<super::restore::VacantPane>>,
) -> Blueprint<PaneRef> {
    match node {
        PlanNode::Group { tabs, selected } => {
            let mut out = Vec::with_capacity(tabs.len());
            for planned in tabs {
                let tab = match planned {
                    PlannedTab::Bound { pane, hidden } => {
                        let tab = Tab::bound(*pane);
                        if *hidden {
                            realized.hidden.push(*pane);
                        }
                        realized.replays.push((pane.session_id, pane.key, *hidden));
                        tab
                    }
                    PlannedTab::Pending {
                        session,
                        descriptor,
                        hidden,
                    } => {
                        let tab = Tab::placeholder();
                        realized.pending.push((
                            *session,
                            descriptor.clone(),
                            PendingPane {
                                tab: tab.id(),
                                hidden: *hidden,
                            },
                        ));
                        tab
                    }
                    PlannedTab::Vacant {
                        vacancy,
                        descriptor,
                        hidden,
                    } => {
                        let tab = Tab::placeholder();
                        match descriptor {
                            None => {
                                vacant_mains.insert(*vacancy, tab.id());
                            }
                            Some(key) => {
                                vacant_panes.entry(*vacancy).or_default().push(
                                    super::restore::VacantPane {
                                        key: key.clone(),
                                        tab: tab.id(),
                                        hidden: *hidden,
                                    },
                                );
                            }
                        }
                        tab
                    }
                };
                out.push(tab);
            }
            Blueprint::Group {
                tabs: out,
                selected: *selected,
            }
        }
        PlanNode::Split { axis, sizing, a, b } => Blueprint::Split {
            axis: match axis {
                dto::Axis::Horizontal => iced::widget::pane_grid::Axis::Horizontal,
                dto::Axis::Vertical => iced::widget::pane_grid::Axis::Vertical,
            },
            sizing: match sizing {
                dto::Sizing::Ratio(ratio) => crate::pane_groups::SplitSizing::Ratio(*ratio),
                dto::Sizing::Px { px, sized_first } => crate::pane_groups::SplitSizing::Px {
                    px: *px,
                    sized_first: *sized_first,
                },
            },
            a: Box::new(realize_node(a, realized, vacant_mains, vacant_panes)),
            b: Box::new(realize_node(b, realized, vacant_mains, vacant_panes)),
        },
    }
}

// ---------------------------------------------------------------------
// Conservation
// ---------------------------------------------------------------------

/// Prove the plan's partition over the same inputs it was projected from:
///
/// - every planned `Bound` tab is a real in-scope live pane, placed exactly
///   once;
/// - every in-scope live pane lands exactly once — placed, kept in an extra
///   window, or removed with its closing session — and a pane placed out of
///   an extra window has its removal recorded;
/// - no out-of-scope pane is mentioned anywhere;
/// - each vacancy's retained tabs stand in exactly one planned window,
///   around exactly one main;
/// - no empty planned window pairs with a live window whose panes are
///   placed elsewhere (the executor skips empty realizations, which would
///   leave those panes hosted twice);
/// - every adopted target is a visually empty live window hosting no
///   panes, claimed by exactly one planned window (installing wholesale
///   into a window with content would discard that content unrecorded);
/// - a script-scoped plan never spawns, asks, closes, creates, or adopts
///   windows.
///
/// The executor re-checks this against the live workspace it actually
/// mutates (the projection re-runs after prompts), so conservation is
/// proven over reality, never a stale snapshot.
pub fn validate_conservation(
    template: &dto::Workspace,
    live: &LiveWorkspace,
    mode: ApplyMode<'_>,
    plan: &ApplyPlan,
) -> Result<(), String> {
    let scope = scope(template, live, mode).map_err(|err| err.to_string())?;
    let close_set: HashSet<SessionId> = plan.close_sessions.iter().copied().collect();

    if plan.script_scoped {
        if !plan.spawns.is_empty() || !plan.questions.is_empty() || !plan.close_sessions.is_empty()
        {
            return Err("a script-scoped plan spawns, asks, or closes".to_string());
        }
        if plan.windows.iter().any(|window| {
            matches!(
                window.target,
                WindowTarget::New { .. } | WindowTarget::Adopted { .. }
            )
        }) {
            return Err("a script-scoped plan creates or adopts a window".to_string());
        }
    }

    // Adopted targets: each must be a live window that is visually empty
    // and hosts no panes (the wholesale install would silently discard
    // anything it showed), and no live window may be targeted twice.
    let mut claimed: HashSet<u64> = plan
        .windows
        .iter()
        .filter_map(|window| match window.target {
            WindowTarget::Existing { stable_id } => Some(stable_id),
            _ => None,
        })
        .collect();
    for window in &plan.windows {
        let WindowTarget::Adopted { stable_id, .. } = window.target else {
            continue;
        };
        let Some(live_window) = live
            .windows
            .iter()
            .find(|window| window.stable_id == stable_id)
        else {
            return Err(format!("adopted window {stable_id} does not exist"));
        };
        if !live_window.empty || live_window.groups.iter().any(|group| !group.is_empty()) {
            return Err(format!("adopted window {stable_id} is not empty"));
        }
        if !claimed.insert(stable_id) {
            return Err(format!("live window {stable_id} is targeted twice"));
        }
    }

    // Walk the planned trees: collect placements and vacancy locations.
    let mut placed: HashMap<PaneRef, usize> = HashMap::new();
    let mut vacancy_windows: HashMap<usize, HashSet<usize>> = HashMap::new();
    let mut vacancy_mains: HashMap<usize, usize> = HashMap::new();
    for (window_index, window) in plan.windows.iter().enumerate() {
        for (_, node) in &window.clusters {
            let mut stack = vec![node];
            while let Some(node) = stack.pop() {
                match node {
                    PlanNode::Group { tabs, .. } => {
                        for tab in tabs {
                            match tab {
                                PlannedTab::Bound { pane, .. } => {
                                    if placed.insert(*pane, window_index).is_some() {
                                        return Err(format!("{pane:?} is placed twice"));
                                    }
                                }
                                PlannedTab::Vacant {
                                    vacancy,
                                    descriptor,
                                    ..
                                } => {
                                    vacancy_windows
                                        .entry(*vacancy)
                                        .or_default()
                                        .insert(window_index);
                                    if descriptor.is_none() {
                                        *vacancy_mains.entry(*vacancy).or_insert(0) += 1;
                                    }
                                }
                                PlannedTab::Pending { .. } => {}
                            }
                        }
                    }
                    PlanNode::Split { a, b, .. } => {
                        stack.push(a);
                        stack.push(b);
                    }
                }
            }
        }
    }
    for (vacancy, windows) in &vacancy_windows {
        if windows.len() != 1 {
            return Err(format!("vacancy {vacancy} spans {} windows", windows.len()));
        }
        if vacancy_mains.get(vacancy) != Some(&1) {
            return Err(format!("vacancy {vacancy} lacks exactly one main tab"));
        }
    }

    // The in-scope pane universe, and each pane's window position. Only
    // `Existing` targets pair with in-scope live windows; adopted targets
    // claim out-of-scope empty windows, which host nothing to conserve.
    let matched_count = plan
        .windows
        .iter()
        .filter(|window| matches!(window.target, WindowTarget::Existing { .. }))
        .count();

    // An empty planned window paired with a live window is a double-hosting
    // hazard: the executor leaves an empty realization's live window as-is,
    // so any pane of that window the plan places elsewhere would end up
    // hosted in two windows at once. The planner never pairs a content-
    // empty template window (it is excluded before ordinal matching), so
    // any plan shaped this way is rejected outright.
    for (window_index, &live_index) in scope.in_scope.iter().enumerate().take(matched_count) {
        let Some(window) = plan.windows.get(window_index) else {
            break;
        };
        if !window.clusters.is_empty() {
            continue;
        }
        let strands_a_pane = live.windows[live_index]
            .groups
            .iter()
            .flatten()
            .any(|pane| {
                placed
                    .get(&pane.pane)
                    .is_some_and(|&placed_in| placed_in != window_index)
            });
        if strands_a_pane {
            return Err(format!(
                "empty planned window {window_index} leaves its live window's panes hosted twice"
            ));
        }
    }

    let removal_set: HashSet<(u64, PaneRef)> = plan.removals.iter().copied().collect();
    let mut seen: HashSet<PaneRef> = HashSet::new();
    for (position, &window_index) in scope.in_scope.iter().enumerate() {
        let window = &live.windows[window_index];
        for pane in window.groups.iter().flatten() {
            seen.insert(pane.pane);
            let is_placed = placed.contains_key(&pane.pane);
            let closing = close_set.contains(&pane.pane.session_id);
            if closing {
                if is_placed {
                    return Err(format!(
                        "{:?} belongs to a closing session yet is placed",
                        pane.pane
                    ));
                }
                continue;
            }
            if position < matched_count {
                if !is_placed && plan.is_executable() {
                    return Err(format!("{:?} in a rebuilt window is unplaced", pane.pane));
                }
            } else if is_placed && !removal_set.contains(&(window.stable_id, pane.pane)) {
                return Err(format!(
                    "{:?} leaves an extra window without a recorded removal",
                    pane.pane
                ));
            }
        }
    }

    // Everything the plan mentions must exist in scope — with one honest
    // exception: a freshly spawned session's main pane is placed by the
    // plan before any window hosts it (the install is what hosts it), so a
    // placed main of a live session hosted *nowhere* is legitimate. A pane
    // hosted in an out-of-scope window is never.
    let hosted_anywhere: HashSet<PaneRef> = live
        .windows
        .iter()
        .flat_map(|window| window.groups.iter().flatten().map(|pane| pane.pane))
        .collect();
    let live_ids: HashSet<SessionId> = live.sessions.iter().map(|session| session.id).collect();
    for pane in placed.keys() {
        if seen.contains(pane) {
            continue;
        }
        if hosted_anywhere.contains(pane) {
            return Err(format!("{pane:?} is out of scope yet placed"));
        }
        if pane.key != MAIN_PANE_KEY || !live_ids.contains(&pane.session_id) {
            return Err(format!(
                "{pane:?} is placed but is not an in-scope live pane"
            ));
        }
    }
    for (_, pane) in &plan.removals {
        if !placed.contains_key(pane) {
            return Err(format!("{pane:?} is removed but never placed"));
        }
    }
    for pane in &plan.riders {
        if !placed.contains_key(pane) {
            return Err(format!("rider {pane:?} is not in the planned trees"));
        }
    }
    // Out-of-scope windows must be untouched: no pane of theirs may appear.
    for (index, window) in live.windows.iter().enumerate() {
        if scope.in_scope.contains(&index) {
            continue;
        }
        for pane in window.groups.iter().flatten() {
            if placed.contains_key(&pane.pane) {
                return Err(format!("{:?} is out of scope yet placed", pane.pane));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_core::session::runtime::pane::{
        DefStateSpec, PaneKind, PaneNamespace, PaneRegistry,
    };

    fn session(id: u32, server: &str, profile: &str) -> LiveSessionInfo {
        LiveSessionInfo {
            id: SessionId::from(id),
            server: server.to_string(),
            profile: profile.to_string(),
        }
    }

    fn main_pane(session: u32) -> LivePane {
        LivePane {
            pane: PaneRef {
                session_id: SessionId::from(session),
                key: MAIN_PANE_KEY,
            },
            descriptor: None,
            hidden: false,
        }
    }

    /// Mint a real registry key for a user script pane named `name`.
    fn script_pane(registry: &mut PaneRegistry, session: u32, name: &str) -> LivePane {
        let key = registry
            .split(
                &PaneNamespace::User,
                name,
                PaneKind::Terminal,
                DefStateSpec {
                    title_bar: None,
                    hidden: None,
                    font_size: None,
                },
                None,
            )
            .expect("registry accepts plain splits")
            .def
            .key;
        LivePane {
            pane: PaneRef {
                session_id: SessionId::from(session),
                key,
            },
            descriptor: Some(DescriptorKey::User {
                name: name.to_string(),
            }),
            hidden: false,
        }
    }

    fn slot(id: u64, server: &str, profile: &str) -> dto::SessionSlot {
        dto::SessionSlot {
            id,
            server: server.to_string(),
            profile: profile.to_string(),
            connect: true,
        }
    }

    fn t_main(slot: u64) -> dto::Pane {
        dto::Pane {
            slot,
            id: dto::PaneIdentity::Main,
            hidden: false,
        }
    }

    fn t_script(slot: u64, name: &str) -> dto::Pane {
        dto::Pane {
            slot,
            id: dto::PaneIdentity::Script {
                namespace: dto::Namespace::User,
                name: name.to_string(),
                display: None,
            },
            hidden: false,
        }
    }

    fn t_group(tabs: Vec<dto::Pane>) -> dto::Node {
        dto::Node::Group(dto::Group { tabs, selected: 0 })
    }

    fn t_window(id: u64, clusters: Vec<dto::Node>) -> dto::Window {
        dto::Window {
            id,
            geometry: dto::Geometry::default(),
            maximized: false,
            active_slot: None,
            clusters: clusters
                .into_iter()
                .map(|root| dto::Cluster { weight: 1.0, root })
                .collect(),
        }
    }

    fn template(sessions: Vec<dto::SessionSlot>, windows: Vec<dto::Window>) -> dto::Workspace {
        dto::Workspace {
            version: dto::SCHEMA_VERSION,
            sessions,
            windows,
        }
    }

    fn live_window(stable_id: u64, groups: Vec<Vec<LivePane>>) -> LiveWindow {
        let empty = groups.iter().all(Vec::is_empty);
        LiveWindow {
            stable_id,
            empty,
            groups,
        }
    }

    fn user_mode() -> ApplyMode<'static> {
        ApplyMode::User { initiating: None }
    }

    fn no_answers() -> HashMap<SessionId, OmittedAnswer> {
        HashMap::new()
    }

    #[test]
    fn automatic_restore_only_uses_the_opened_profile_and_keeps_its_main_window() {
        let stored = template(
            vec![
                slot(1, "Arctic", "alt"),
                slot(2, "Arctic", "main"),
                slot(3, "Other", "foreign"),
            ],
            vec![
                t_window(1, vec![t_group(vec![t_script(2, "chat")])]),
                t_window(2, vec![t_group(vec![t_main(1)])]),
                t_window(3, vec![t_group(vec![t_main(2)])]),
                t_window(4, vec![t_group(vec![t_main(3)])]),
            ],
        );
        let opened = session(7, "Arctic", "main");
        let projected = for_opened_profile(&stored, &opened);
        assert_eq!(projected.sessions.len(), 1);
        assert_eq!(projected.sessions[0].id, 2);
        assert_eq!(
            projected
                .windows
                .iter()
                .map(|window| window.id)
                .collect::<Vec<_>>(),
            vec![3, 1]
        );
        let live = LiveWorkspace {
            sessions: vec![opened, session(8, "Other", "foreign")],
            windows: vec![
                live_window(20, vec![vec![main_pane(7)]]),
                live_window(21, vec![vec![main_pane(8)]]),
            ],
        };
        let plan = plan_apply(&projected, &live, user_mode(), &no_answers()).unwrap();
        assert!(plan.is_executable());
        assert!(plan.close_sessions.is_empty());
        assert!(plan.removals.is_empty());
        assert_eq!(
            plan.windows[0].target,
            WindowTarget::Existing { stable_id: 20 }
        );
        assert!(matches!(plan.windows[1].target, WindowTarget::New { .. }));
        validate_conservation(&projected, &live, user_mode(), &plan).unwrap();
        assert_eq!(bound_panes(&plan), vec![main_pane(7).pane]);
    }

    #[test]
    fn automatic_restore_falls_back_for_a_new_profile_but_never_another_server() {
        let stored = template(
            vec![slot(1, "Arctic", "old")],
            vec![t_window(1, vec![t_group(vec![t_main(1)])])],
        );
        let opened = session(7, "Arctic", "new");
        let projected = for_opened_profile(&stored, &opened);
        assert_eq!(projected.sessions[0].profile, "new");
        let live = LiveWorkspace {
            sessions: vec![opened],
            windows: vec![live_window(20, vec![vec![main_pane(7)]])],
        };
        assert!(
            plan_apply(&projected, &live, user_mode(), &no_answers())
                .unwrap()
                .is_executable()
        );
        let foreign = for_opened_profile(&stored, &session(8, "Other", "old"));
        assert!(foreign.sessions.is_empty());
        assert!(foreign.windows.is_empty());
    }

    /// Every planned Bound pane, in tree order.
    fn bound_panes(plan: &ApplyPlan) -> Vec<PaneRef> {
        let mut out = Vec::new();
        for window in &plan.windows {
            for (_, node) in &window.clusters {
                collect_bound(node, &mut out);
            }
        }
        out
    }

    fn collect_bound(node: &PlanNode, out: &mut Vec<PaneRef>) {
        match node {
            PlanNode::Group { tabs, .. } => {
                for tab in tabs {
                    if let PlannedTab::Bound { pane, .. } = tab {
                        out.push(*pane);
                    }
                }
            }
            PlanNode::Split { a, b, .. } => {
                collect_bound(a, out);
                collect_bound(b, out);
            }
        }
    }

    #[test]
    fn identity_apply_rebinds_exactly_and_conserves() {
        let mut registry = PaneRegistry::new();
        let chat = script_pane(&mut registry, 1, "chat");
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm")],
            windows: vec![live_window(1, vec![vec![main_pane(1)], vec![chat.clone()]])],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm")],
            vec![t_window(
                1,
                vec![
                    t_group(vec![t_main(10)]),
                    t_group(vec![t_script(10, "chat")]),
                ],
            )],
        );
        let plan = plan_apply(
            &template,
            &live,
            ApplyMode::Script {
                calling_server: "Arctic",
            },
            &no_answers(),
        )
        .expect("plan");
        assert!(plan.is_executable());
        assert!(plan.riders.is_empty());
        assert!(plan.removals.is_empty());
        assert_eq!(
            bound_panes(&plan),
            vec![
                PaneRef {
                    session_id: SessionId::from(1),
                    key: MAIN_PANE_KEY
                },
                chat.pane
            ]
        );
        validate_conservation(
            &template,
            &live,
            ApplyMode::Script {
                calling_server: "Arctic",
            },
            &plan,
        )
        .expect("conserves");
    }

    #[test]
    fn script_overflow_folds_into_the_last_in_scope_window() {
        // Two template windows, one live window: everything lands in the
        // single live window — never a partial application.
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm"), session(2, "Arctic", "alt")],
            windows: vec![live_window(1, vec![vec![main_pane(1)], vec![main_pane(2)]])],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm"), slot(11, "Arctic", "alt")],
            vec![
                t_window(1, vec![t_group(vec![t_main(10)])]),
                t_window(2, vec![t_group(vec![t_main(11)])]),
            ],
        );
        let mode = ApplyMode::Script {
            calling_server: "Arctic",
        };
        let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
        assert_eq!(plan.windows.len(), 1);
        assert_eq!(
            plan.windows[0].target,
            WindowTarget::Existing { stable_id: 1 },
            "folded into the only live window"
        );
        assert_eq!(plan.windows[0].clusters.len(), 2);
        assert_eq!(bound_panes(&plan).len(), 2);
        validate_conservation(&template, &live, mode, &plan).expect("conserves");
    }

    #[test]
    fn user_apply_creates_the_missing_windows_instead_of_folding() {
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm"), session(2, "Arctic", "alt")],
            windows: vec![live_window(1, vec![vec![main_pane(1)], vec![main_pane(2)]])],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm"), slot(11, "Arctic", "alt")],
            vec![
                t_window(1, vec![t_group(vec![t_main(10)])]),
                t_window(2, vec![t_group(vec![t_main(11)])]),
            ],
        );
        let plan = plan_apply(&template, &live, user_mode(), &no_answers()).expect("plan");
        assert!(plan.is_executable());
        assert_eq!(plan.windows.len(), 2);
        assert!(matches!(plan.windows[1].target, WindowTarget::New { .. }));
        // Session 2's main moved out of live window 1 into the new window;
        // window 1 is rebuilt, so no removal entry is needed.
        assert!(plan.removals.is_empty());
        validate_conservation(&template, &live, user_mode(), &plan).expect("conserves");
    }

    #[test]
    fn user_restore_adopts_the_initiating_empty_window_instead_of_creating() {
        // A restore driven from a fresh window's connect surface: the
        // session is already spawned (hosted nowhere — the install is what
        // hosts it) and the only live window is empty. The plan must land
        // in that window, at the template's stored geometry, not beside it.
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm")],
            windows: vec![live_window(7, vec![])],
        };
        let mut template = template(
            vec![slot(10, "Arctic", "imm")],
            vec![t_window(1, vec![t_group(vec![t_main(10)])])],
        );
        template.windows[0].geometry = dto::Geometry {
            x: 30.0,
            y: 40.0,
            width: 800.0,
            height: 600.0,
            scale: 1.0,
        };
        let mode = ApplyMode::User {
            initiating: Some(7),
        };
        let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
        assert!(plan.is_executable());
        assert_eq!(plan.windows.len(), 1);
        assert_eq!(
            plan.windows[0].target,
            WindowTarget::Adopted {
                stable_id: 7,
                geometry: template.windows[0].geometry.clone(),
                maximized: false,
            }
        );
        validate_conservation(&template, &live, mode, &plan).expect("conserves");
    }

    #[test]
    fn adoption_prefers_the_initiating_window_then_creation_order() {
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm")],
            windows: vec![live_window(1, vec![]), live_window(2, vec![])],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm")],
            vec![t_window(1, vec![t_group(vec![t_main(10)])])],
        );
        // The initiating window wins even though it is later in creation
        // order.
        let mode = ApplyMode::User {
            initiating: Some(2),
        };
        let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
        assert!(matches!(
            plan.windows[0].target,
            WindowTarget::Adopted { stable_id: 2, .. }
        ));
        validate_conservation(&template, &live, mode, &plan).expect("conserves");

        // Without an initiating window, creation order decides.
        let plan = plan_apply(&template, &live, user_mode(), &no_answers()).expect("plan");
        assert!(matches!(
            plan.windows[0].target,
            WindowTarget::Adopted { stable_id: 1, .. }
        ));
        validate_conservation(&template, &live, user_mode(), &plan).expect("conserves");
    }

    #[test]
    fn a_window_is_created_only_when_no_empty_window_remains() {
        // Two-window template, one empty live window: the first template
        // window adopts it, the second still opens a new window.
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm"), session(2, "Arctic", "alt")],
            windows: vec![live_window(9, vec![])],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm"), slot(11, "Arctic", "alt")],
            vec![
                t_window(1, vec![t_group(vec![t_main(10)])]),
                t_window(2, vec![t_group(vec![t_main(11)])]),
            ],
        );
        let mode = ApplyMode::User {
            initiating: Some(9),
        };
        let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
        assert_eq!(plan.windows.len(), 2);
        assert!(matches!(
            plan.windows[0].target,
            WindowTarget::Adopted { stable_id: 9, .. }
        ));
        assert!(matches!(plan.windows[1].target, WindowTarget::New { .. }));
        validate_conservation(&template, &live, mode, &plan).expect("conserves");
    }

    #[test]
    fn matched_windows_rebuild_and_the_overflow_adopts_the_empty_window() {
        // One in-scope live window and one empty window against a two-
        // window template: ordinal matching claims the in-scope window,
        // and the unmatched template window adopts the empty one instead
        // of creating a third.
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm"), session(2, "Arctic", "alt")],
            windows: vec![
                live_window(1, vec![vec![main_pane(1)], vec![main_pane(2)]]),
                live_window(2, vec![]),
            ],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm"), slot(11, "Arctic", "alt")],
            vec![
                t_window(1, vec![t_group(vec![t_main(10)])]),
                t_window(2, vec![t_group(vec![t_main(11)])]),
            ],
        );
        let plan = plan_apply(&template, &live, user_mode(), &no_answers()).expect("plan");
        assert_eq!(plan.windows.len(), 2);
        assert_eq!(
            plan.windows[0].target,
            WindowTarget::Existing { stable_id: 1 }
        );
        assert!(matches!(
            plan.windows[1].target,
            WindowTarget::Adopted { stable_id: 2, .. }
        ));
        validate_conservation(&template, &live, user_mode(), &plan).expect("conserves");
    }

    #[test]
    fn script_applies_never_adopt_empty_windows() {
        // The overflow-fold shape with an empty window present: a script
        // apply still folds into the last in-scope window — it never
        // touches OS windows, so the empty window is not claimed.
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm"), session(2, "Arctic", "alt")],
            windows: vec![
                live_window(1, vec![vec![main_pane(1)], vec![main_pane(2)]]),
                live_window(2, vec![]),
            ],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm"), slot(11, "Arctic", "alt")],
            vec![
                t_window(1, vec![t_group(vec![t_main(10)])]),
                t_window(2, vec![t_group(vec![t_main(11)])]),
            ],
        );
        let mode = ApplyMode::Script {
            calling_server: "Arctic",
        };
        let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
        assert_eq!(plan.windows.len(), 1);
        assert_eq!(
            plan.windows[0].target,
            WindowTarget::Existing { stable_id: 1 }
        );
        validate_conservation(&template, &live, mode, &plan).expect("conserves");

        // A script-scoped plan that adopts anyway is refused outright.
        let mut tampered = plan.clone();
        tampered.windows[0].target = WindowTarget::Adopted {
            stable_id: 2,
            geometry: dto::Geometry::default(),
            maximized: false,
        };
        let error = validate_conservation(&template, &live, mode, &tampered).expect_err("refused");
        assert!(error.contains("adopts"), "unexpected refusal: {error}");
    }

    #[test]
    fn placeholder_hosting_windows_are_not_empty_and_are_not_adopted() {
        // A window whose tabs are all placeholders hosts no bound panes,
        // but it is showing content — a restore must not clobber it. Its
        // live projection carries groups with no panes yet is flagged
        // non-empty.
        let placeholder_window = LiveWindow {
            stable_id: 3,
            empty: false,
            groups: vec![vec![]],
        };
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm")],
            windows: vec![placeholder_window],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm")],
            vec![t_window(1, vec![t_group(vec![t_main(10)])])],
        );
        let plan = plan_apply(&template, &live, user_mode(), &no_answers()).expect("plan");
        assert!(matches!(plan.windows[0].target, WindowTarget::New { .. }));
        validate_conservation(&template, &live, user_mode(), &plan).expect("conserves");

        // And a plan that adopts it anyway is refused.
        let mut tampered = plan.clone();
        tampered.windows[0].target = WindowTarget::Adopted {
            stable_id: 3,
            geometry: dto::Geometry::default(),
            maximized: false,
        };
        let error =
            validate_conservation(&template, &live, user_mode(), &tampered).expect_err("refused");
        assert!(error.contains("not empty"), "unexpected refusal: {error}");
    }

    #[test]
    fn extra_live_windows_keep_their_arrangement_minus_claimed_panes() {
        let mut registry = PaneRegistry::new();
        let chat = script_pane(&mut registry, 1, "chat");
        let notes = script_pane(&mut registry, 1, "notes");
        // Window 1 hosts the main; window 2 hosts two torn-out panes. The
        // template names only chat, placing it beside the main.
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm")],
            windows: vec![
                live_window(1, vec![vec![main_pane(1)]]),
                live_window(2, vec![vec![chat.clone(), notes.clone()]]),
            ],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm")],
            vec![t_window(
                1,
                vec![t_group(vec![t_main(10), t_script(10, "chat")])],
            )],
        );
        let mode = ApplyMode::Script {
            calling_server: "Arctic",
        };
        let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
        assert_eq!(plan.windows.len(), 1, "the extra window is not rebuilt");
        assert_eq!(plan.removals, vec![(2, chat.pane)]);
        let bound = bound_panes(&plan);
        assert!(bound.contains(&chat.pane));
        assert!(!bound.contains(&notes.pane), "notes stays where it is");
        validate_conservation(&template, &live, mode, &plan).expect("conserves");
    }

    #[test]
    fn omitted_sessions_ask_then_close_or_keep_per_answer() {
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm"), session(2, "Arctic", "extra")],
            windows: vec![live_window(1, vec![vec![main_pane(1)], vec![main_pane(2)]])],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm")],
            vec![t_window(1, vec![t_group(vec![t_main(10)])])],
        );
        // First projection: the omitted session is a question, and the plan
        // is not executable until it is answered.
        let preview = plan_apply(&template, &live, user_mode(), &no_answers()).expect("plan");
        assert_eq!(preview.questions, vec![SessionId::from(2)]);
        assert!(!preview.is_executable());

        // Keep: the session's main rides (its group dissolved).
        let keep: HashMap<_, _> = [(SessionId::from(2), OmittedAnswer::Keep)].into();
        let kept = plan_apply(&template, &live, user_mode(), &keep).expect("plan");
        assert!(kept.is_executable());
        assert!(kept.riders.contains(&PaneRef {
            session_id: SessionId::from(2),
            key: MAIN_PANE_KEY
        }));
        validate_conservation(&template, &live, user_mode(), &kept).expect("conserves");

        // Close: the session's panes vanish from the plan, recorded as a
        // close — never silently.
        let close: HashMap<_, _> = [(SessionId::from(2), OmittedAnswer::Close)].into();
        let closed = plan_apply(&template, &live, user_mode(), &close).expect("plan");
        assert!(closed.is_executable());
        assert_eq!(closed.close_sessions, vec![SessionId::from(2)]);
        assert!(!bound_panes(&closed).contains(&PaneRef {
            session_id: SessionId::from(2),
            key: MAIN_PANE_KEY
        }));
        validate_conservation(&template, &live, user_mode(), &closed).expect("conserves");
    }

    #[test]
    fn script_apply_never_asks_spawns_or_closes() {
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm"), session(2, "Arctic", "extra")],
            windows: vec![live_window(1, vec![vec![main_pane(1)], vec![main_pane(2)]])],
        };
        // The template wants two slots; only one live session matches by
        // profile — the other slot stays vacant (no spawn), and the extra
        // live session is kept (no question, no close).
        let template = template(
            vec![slot(10, "Arctic", "imm"), slot(11, "Arctic", "away")],
            vec![t_window(
                1,
                vec![t_group(vec![t_main(10)]), t_group(vec![t_main(11)])],
            )],
        );
        let mode = ApplyMode::Script {
            calling_server: "Arctic",
        };
        let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
        assert!(plan.is_executable());
        assert!(plan.questions.is_empty());
        assert!(plan.spawns.is_empty());
        assert!(plan.close_sessions.is_empty());
        // The extra session binds by same-server fallback... unless the
        // away slot took it. Exactly one of the two outcomes: both live
        // sessions are on the referenced server, so both bind (fallback
        // fills the away slot) — no vacancy in this arrangement.
        assert!(plan.vacancies.is_empty());
        validate_conservation(&template, &live, mode, &plan).expect("conserves");
    }

    #[test]
    fn script_apply_stages_vacancies_for_truly_unmatched_slots() {
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm")],
            windows: vec![live_window(1, vec![vec![main_pane(1)]])],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm"), slot(11, "Arctic", "second")],
            vec![t_window(
                1,
                vec![
                    t_group(vec![t_main(10)]),
                    t_group(vec![t_main(11), t_script(11, "chat")]),
                ],
            )],
        );
        let mode = ApplyMode::Script {
            calling_server: "Arctic",
        };
        let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
        assert!(
            plan.is_executable(),
            "vacancies do not block a script apply"
        );
        assert_eq!(
            plan.vacancies,
            vec![PlannedVacancy {
                server: "Arctic".to_string(),
                profile: "second".to_string(),
            }]
        );
        // The vacancy stages its main and its co-located script pane.
        let realized = realize_window(&plan.windows[0], &plan.vacancies);
        assert_eq!(realized.vacancies.len(), 1);
        assert_eq!(realized.vacancies[0].profile, "second");
        assert_eq!(realized.vacancies[0].panes.len(), 1);
        validate_conservation(&template, &live, mode, &plan).expect("conserves");
    }

    #[test]
    fn vacancy_script_tabs_are_confined_to_their_mains_window() {
        // The unmatched slot's main lands in window 1 but its chat pane is
        // stored torn out into window 2: the cross-window ghost is dropped.
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm"), session(2, "Arctic", "alt")],
            windows: vec![
                live_window(1, vec![vec![main_pane(1)]]),
                live_window(2, vec![vec![main_pane(2)]]),
            ],
        };
        let template = template(
            vec![
                slot(10, "Arctic", "imm"),
                slot(11, "Arctic", "alt"),
                slot(12, "Arctic", "gone"),
            ],
            vec![
                t_window(
                    1,
                    vec![t_group(vec![t_main(10)]), t_group(vec![t_main(12)])],
                ),
                t_window(2, vec![t_group(vec![t_main(11), t_script(12, "chat")])]),
            ],
        );
        let mode = ApplyMode::Script {
            calling_server: "Arctic",
        };
        let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
        let first = realize_window(&plan.windows[0], &plan.vacancies);
        let second = realize_window(&plan.windows[1], &plan.vacancies);
        assert_eq!(first.vacancies.len(), 1, "the vacancy homes with its main");
        assert!(
            first.vacancies[0].panes.is_empty(),
            "the torn-out ghost tab is dropped, not staged across windows"
        );
        assert!(second.vacancies.is_empty());
        validate_conservation(&template, &live, mode, &plan).expect("conserves");
    }

    #[test]
    fn riders_follow_their_group_and_dissolved_groups_replace_in_window() {
        let mut registry = PaneRegistry::new();
        let chat = script_pane(&mut registry, 1, "chat");
        let stray = script_pane(&mut registry, 1, "stray");
        let loner = script_pane(&mut registry, 1, "loner");
        // Live: [main, stray] grouped together; [chat] known; [loner] alone
        // in a group of its own. The template knows main and chat only.
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm")],
            windows: vec![live_window(
                1,
                vec![
                    vec![main_pane(1), stray.clone()],
                    vec![chat.clone()],
                    vec![loner.clone()],
                ],
            )],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm")],
            vec![t_window(
                1,
                vec![
                    t_group(vec![t_main(10)]),
                    t_group(vec![t_script(10, "chat")]),
                ],
            )],
        );
        let mode = ApplyMode::Script {
            calling_server: "Arctic",
        };
        let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
        assert_eq!(plan.riders.len(), 2);
        // stray follows its group-mate (the main) into the main's group.
        let PlanNode::Group { tabs, .. } = &plan.windows[0].clusters[0].1 else {
            panic!("main group expected");
        };
        assert!(tabs.contains(&PlannedTab::Bound {
            pane: stray.pane,
            hidden: false
        }));
        // loner's group dissolved with no known mate: it re-places as its
        // own cluster within the same window.
        assert_eq!(plan.windows[0].clusters.len(), 3);
        let PlanNode::Group { tabs, .. } = &plan.windows[0].clusters[2].1 else {
            panic!("rider cluster expected");
        };
        assert_eq!(
            tabs,
            &vec![PlannedTab::Bound {
                pane: loner.pane,
                hidden: false
            }]
        );
        validate_conservation(&template, &live, mode, &plan).expect("conserves");
    }

    #[test]
    fn content_empty_template_windows_claim_no_live_window() {
        // Under Arctic scoping the first template window holds only an
        // Elsewhere main — no realizable content. It must not claim live
        // window 1: a purely ordinal pairing left window 1 skipped by the
        // executor (empty realization) while its main was rebuilt into
        // window 2, hosting the pane twice.
        let mut registry = PaneRegistry::new();
        let chat = script_pane(&mut registry, 1, "chat");
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm")],
            windows: vec![
                live_window(1, vec![vec![main_pane(1)]]),
                live_window(2, vec![vec![chat.clone()]]),
            ],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm"), slot(11, "Elsewhere", "solo")],
            vec![
                t_window(1, vec![t_group(vec![t_main(11)])]),
                t_window(2, vec![t_group(vec![t_main(10), t_script(10, "chat")])]),
            ],
        );
        let mode = ApplyMode::Script {
            calling_server: "Arctic",
        };
        let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
        // One planned window, targeting live window 1; chat leaves live
        // window 2 with a recorded removal — nothing is hosted twice.
        assert_eq!(plan.windows.len(), 1);
        assert_eq!(
            plan.windows[0].target,
            WindowTarget::Existing { stable_id: 1 }
        );
        assert!(!plan.windows[0].clusters.is_empty());
        assert_eq!(plan.removals, vec![(2, chat.pane)]);
        let bound = bound_panes(&plan);
        assert!(bound.contains(&PaneRef {
            session_id: SessionId::from(1),
            key: MAIN_PANE_KEY
        }));
        assert!(bound.contains(&chat.pane));
        validate_conservation(&template, &live, mode, &plan).expect("conserves");

        // The double-hosting shape — the content-empty window paired with
        // live window 1 while window 2's rebuild claims every pane — must
        // be rejected by the hardened conservation check.
        let mut old_shape = plan.clone();
        old_shape.windows.insert(
            0,
            PlannedWindow {
                target: WindowTarget::Existing { stable_id: 1 },
                clusters: Vec::new(),
                active: None,
            },
        );
        old_shape.windows[1].target = WindowTarget::Existing { stable_id: 2 };
        old_shape.removals.clear();
        let error = validate_conservation(&template, &live, mode, &old_shape)
            .expect_err("the double-hosting shape is refused");
        assert!(
            error.contains("hosted twice"),
            "unexpected refusal: {error}"
        );
    }

    #[test]
    fn script_scoping_drops_foreign_slots_and_rides_foreign_panes() {
        // A mixed window: Arctic main shares a group with an Elsewhere
        // main. The template references both servers, but a Script apply
        // from Arctic must neither realize the foreign slot nor move the
        // foreign session's panes except as riders.
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm"), session(2, "Elsewhere", "solo")],
            windows: vec![live_window(1, vec![vec![main_pane(1), main_pane(2)]])],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm"), slot(11, "Elsewhere", "solo")],
            vec![t_window(
                1,
                vec![t_group(vec![t_main(10)]), t_group(vec![t_main(11)])],
            )],
        );
        let mode = ApplyMode::Script {
            calling_server: "Arctic",
        };
        let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
        // The foreign template group dropped entirely; the foreign live
        // main rides with its group-mate.
        assert_eq!(plan.windows[0].clusters.len(), 1);
        assert!(plan.riders.contains(&PaneRef {
            session_id: SessionId::from(2),
            key: MAIN_PANE_KEY
        }));
        assert!(plan.vacancies.is_empty(), "foreign slots stage no vacancy");
        validate_conservation(&template, &live, mode, &plan).expect("conserves");
    }

    #[test]
    fn fully_foreign_windows_are_out_of_scope_and_untouched() {
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm"), session(2, "Elsewhere", "solo")],
            windows: vec![
                live_window(1, vec![vec![main_pane(1)]]),
                live_window(2, vec![vec![main_pane(2)]]),
            ],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm")],
            vec![t_window(1, vec![t_group(vec![t_main(10)])])],
        );
        for mode in [
            ApplyMode::Script {
                calling_server: "Arctic",
            },
            user_mode(),
        ] {
            let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
            assert_eq!(plan.windows.len(), 1);
            assert_eq!(
                plan.windows[0].target,
                WindowTarget::Existing { stable_id: 1 }
            );
            let foreign_main = PaneRef {
                session_id: SessionId::from(2),
                key: MAIN_PANE_KEY,
            };
            assert!(!bound_panes(&plan).contains(&foreign_main));
            assert!(plan.removals.is_empty());
            validate_conservation(&template, &live, mode, &plan).expect("conserves");
        }
    }

    #[test]
    fn missing_live_script_panes_become_pending_placeholders() {
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm")],
            windows: vec![live_window(1, vec![vec![main_pane(1)]])],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm")],
            vec![t_window(
                1,
                vec![t_group(vec![t_main(10), t_script(10, "chat")])],
            )],
        );
        let mode = ApplyMode::Script {
            calling_server: "Arctic",
        };
        let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
        let PlanNode::Group { tabs, .. } = &plan.windows[0].clusters[0].1 else {
            panic!("group expected");
        };
        assert!(matches!(
            &tabs[1],
            PlannedTab::Pending { session, descriptor, .. }
                if *session == SessionId::from(1)
                    && *descriptor == DescriptorKey::User { name: "chat".to_string() }
        ));
        let realized = realize_window(&plan.windows[0], &plan.vacancies);
        assert_eq!(realized.pending.len(), 1);
        validate_conservation(&template, &live, mode, &plan).expect("conserves");
    }

    #[test]
    fn user_mode_spawns_missing_slots_and_the_replan_binds_them() {
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm")],
            windows: vec![live_window(1, vec![vec![main_pane(1)]])],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm"), slot(11, "Arctic", "second")],
            vec![t_window(
                1,
                vec![t_group(vec![t_main(10)]), t_group(vec![t_main(11)])],
            )],
        );
        let preview = plan_apply(&template, &live, user_mode(), &no_answers()).expect("plan");
        assert_eq!(
            preview.spawns,
            vec![SpawnSlot {
                slot: 11,
                server: "Arctic".to_string(),
                profile: "second".to_string(),
                connect: true,
            }]
        );
        assert!(!preview.is_executable());

        // The executor spawns and re-projects; the fresh session now binds
        // exactly and the plan settles.
        let respawned = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm"), session(2, "Arctic", "second")],
            windows: live.windows.clone(),
        };
        let plan = plan_apply(&template, &respawned, user_mode(), &no_answers()).expect("plan");
        assert!(plan.is_executable());
        assert!(plan.spawns.is_empty());
        assert!(bound_panes(&plan).contains(&PaneRef {
            session_id: SessionId::from(2),
            key: MAIN_PANE_KEY
        }));
        validate_conservation(&template, &respawned, user_mode(), &plan).expect("conserves");
    }

    #[test]
    fn template_hidden_state_stamps_bound_panes_and_riders_keep_theirs() {
        let mut registry = PaneRegistry::new();
        let mut chat = script_pane(&mut registry, 1, "chat");
        chat.hidden = true; // currently hidden, template says visible
        let mut stray = script_pane(&mut registry, 1, "stray");
        stray.hidden = true; // rider: keeps its hidden state
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm")],
            windows: vec![live_window(
                1,
                vec![vec![main_pane(1), stray.clone()], vec![chat.clone()]],
            )],
        };
        let mut template = template(
            vec![slot(10, "Arctic", "imm")],
            vec![t_window(
                1,
                vec![
                    t_group(vec![t_main(10)]),
                    t_group(vec![t_script(10, "chat")]),
                ],
            )],
        );
        // Template stamps the main hidden (an eyeballed-away arrangement).
        let dto::Node::Group(group) = &mut template.windows[0].clusters[0].root else {
            panic!("group expected");
        };
        group.tabs[0].hidden = true;

        let mode = ApplyMode::Script {
            calling_server: "Arctic",
        };
        let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
        let realized = realize_window(&plan.windows[0], &plan.vacancies);
        let main = PaneRef {
            session_id: SessionId::from(1),
            key: MAIN_PANE_KEY,
        };
        assert!(realized.hidden.contains(&main), "template hidden applies");
        assert!(
            !realized.hidden.contains(&chat.pane),
            "template visible overrides the live hidden state"
        );
        assert!(
            realized.hidden.contains(&stray.pane),
            "riders keep their current hidden state"
        );
        // Replays cover every bound pane exactly once.
        assert_eq!(realized.replays.len(), 3);
        validate_conservation(&template, &live, mode, &plan).expect("conserves");
    }

    #[test]
    fn unreferenced_and_empty_templates_are_errors() {
        let live = LiveWorkspace {
            sessions: vec![session(1, "Arctic", "imm")],
            windows: vec![live_window(1, vec![vec![main_pane(1)]])],
        };
        let empty = template(vec![slot(10, "Arctic", "imm")], vec![]);
        assert_eq!(
            plan_apply(&empty, &live, user_mode(), &no_answers()).unwrap_err(),
            ApplyError::EmptyTemplate
        );
        let foreign = template(
            vec![slot(10, "Elsewhere", "solo")],
            vec![t_window(1, vec![t_group(vec![t_main(10)])])],
        );
        assert_eq!(
            plan_apply(
                &foreign,
                &live,
                ApplyMode::Script {
                    calling_server: "Arctic"
                },
                &no_answers()
            )
            .unwrap_err(),
            ApplyError::NotReferenced {
                server: "Arctic".to_string()
            }
        );
    }

    #[test]
    fn duplicate_profiles_bind_ordinally_across_the_apply() {
        // Two identical live sessions, two identical slots: lowest live
        // ordinal to lowest stored ordinal, deterministically.
        let live = LiveWorkspace {
            sessions: vec![session(5, "Arctic", "imm"), session(9, "Arctic", "imm")],
            windows: vec![live_window(1, vec![vec![main_pane(5)], vec![main_pane(9)]])],
        };
        let template = template(
            vec![slot(10, "Arctic", "imm"), slot(11, "Arctic", "imm")],
            vec![t_window(
                1,
                vec![t_group(vec![t_main(10)]), t_group(vec![t_main(11)])],
            )],
        );
        let mode = ApplyMode::Script {
            calling_server: "Arctic",
        };
        let plan = plan_apply(&template, &live, mode, &no_answers()).expect("plan");
        assert_eq!(
            bound_panes(&plan),
            vec![
                PaneRef {
                    session_id: SessionId::from(5),
                    key: MAIN_PANE_KEY
                },
                PaneRef {
                    session_id: SessionId::from(9),
                    key: MAIN_PANE_KEY
                },
            ]
        );
        validate_conservation(&template, &live, mode, &plan).expect("conserves");
    }
}
