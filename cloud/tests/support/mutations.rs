//! POST /areas/{id}/mutations — the compound mutation envelope endpoint.
//! Mirrors the real server's `mapping::{contract, executor, ops}`: receipt-
//! gated idempotency per `(actor, operation_id)` (a replay is served only to
//! a caller whose access fingerprint still matches the accept-time one), the
//! single-Area-precondition rule, MANDATORY access-fingerprint plus
//! projected-revision CAS checks, ordered all-or-nothing application, and
//! exactly one revision bump per touched aggregate (public only when the
//! aggregate's public projection changed; an all-no-op envelope bumps
//! nothing at all).

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::areas::{
    DIRECTIONS, H_ALIGN, SHAPE_TYPES, V_ALIGN, check_enum, double_option,
    embedded_exit_json, embedded_label_json, embedded_shape_json, require_caps,
};
use super::connections::{
    self, NewExitLink, attach_for_new_exit, cleanup_after_exit_delete, maintain_after_retarget,
};
use super::projection::connection_verdict_in;
use super::http::{
    authenticate, bad_request, err, err_with_details, not_found, ok, parse_area_id,
};
use super::mock_server::Shared;
use super::state::{
    AreaPropRecord, AreaRecord, Caps, ExitRecord, LabelRecord, MockState, MutationReceipt,
    RoomPropRecord, RoomRecord, ShapeRecord, access_fingerprint,
};

/// Most operations one envelope may carry (mirrors `contract.rs`).
pub const MAX_MUTATION_OPERATIONS: usize = 256;

/// The wire envelope (`MutationEnvelope<Vec<AreaMutation>>`).
#[derive(Debug, Deserialize)]
struct WireEnvelope {
    operation_id: Uuid,
    #[serde(default)]
    preconditions: Vec<WirePrecondition>,
    payload: Vec<AreaOp>,
}

#[derive(Debug, Deserialize)]
struct WirePrecondition {
    resource: String,
    id: Uuid,
    expected_rev: i64,
    #[serde(default)]
    access_fingerprint: Option<String>,
}

/// One operation of a compound mutation — the `op`-tagged wire alphabet of
/// the server's `ops::AreaMutation`. Serialization doubles as the canonical
/// form the receipt request-hash covers, so key order and whitespace in the
/// incoming JSON never affect deduplication.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum AreaOp {
    UpsertRoom {
        room_number: i32,
        body: UpsertRoomBody,
    },
    DeleteRoom {
        room_number: i32,
    },
    UpsertRoomProperty {
        room_number: i32,
        name: String,
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_secret: Option<bool>,
    },
    DeleteRoomProperty {
        room_number: i32,
        name: String,
    },
    AddRoomTag {
        room_number: i32,
        tag: String,
    },
    RemoveRoomTag {
        room_number: i32,
        tag: String,
    },
    UpsertAreaProperty {
        name: String,
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_secret: Option<bool>,
    },
    DeleteAreaProperty {
        name: String,
    },
    CreateExit {
        room_number: i32,
        body: CreateExitBody,
    },
    UpdateExit {
        exit_id: Uuid,
        body: UpdateExitBody,
    },
    DeleteExit {
        exit_id: Uuid,
    },
    CreateLabel {
        body: CreateLabelBody,
    },
    UpdateLabel {
        label_id: Uuid,
        body: UpdateLabelBody,
    },
    DeleteLabel {
        label_id: Uuid,
    },
    CreateShape {
        body: CreateShapeBody,
    },
    UpdateShape {
        shape_id: Uuid,
        body: UpdateShapeBody,
    },
    DeleteShape {
        shape_id: Uuid,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpsertRoomBody {
    title: Option<String>,
    description: Option<String>,
    level: Option<i32>,
    x: Option<f32>,
    y: Option<f32>,
    color: Option<String>,
    is_secret: Option<bool>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    external_id: Option<Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreateExitBody {
    /// Client-minted entity id (v2 contract); minted here when absent.
    #[serde(default)]
    id: Option<Uuid>,
    from_direction: String,
    to_area_id: Option<Uuid>,
    to_room_number: Option<i32>,
    to_direction: Option<String>,
    path: Option<String>,
    is_hidden: bool,
    is_closed: bool,
    is_locked: bool,
    weight: f32,
    command: Option<String>,
    is_secret: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateExitBody {
    from_direction: Option<String>,
    to_area_id: Option<Uuid>,
    to_room_number: Option<i32>,
    to_direction: Option<String>,
    path: Option<String>,
    is_hidden: Option<bool>,
    is_closed: Option<bool>,
    is_locked: Option<bool>,
    weight: Option<f32>,
    command: Option<String>,
    is_secret: Option<bool>,
    clear_to: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreateLabelBody {
    /// Client-minted entity id (v2 contract); minted here when absent.
    #[serde(default)]
    id: Option<Uuid>,
    level: Option<i32>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    horizontal_alignment: String,
    vertical_alignment: String,
    text: String,
    color: Option<String>,
    background_color: Option<String>,
    font_size: Option<i32>,
    font_weight: Option<i32>,
    is_secret: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateLabelBody {
    level: Option<i32>,
    x: Option<f32>,
    y: Option<f32>,
    width: Option<f32>,
    height: Option<f32>,
    horizontal_alignment: Option<String>,
    vertical_alignment: Option<String>,
    text: Option<String>,
    color: Option<String>,
    background_color: Option<String>,
    font_size: Option<i32>,
    font_weight: Option<i32>,
    is_secret: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreateShapeBody {
    /// Client-minted entity id (v2 contract); minted here when absent.
    #[serde(default)]
    id: Option<Uuid>,
    level: Option<i32>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    background_color: Option<String>,
    stroke_color: Option<String>,
    shape_type: String,
    border_radius: Option<f32>,
    stroke_width: Option<f32>,
    is_secret: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateShapeBody {
    level: Option<i32>,
    x: Option<f32>,
    y: Option<f32>,
    width: Option<f32>,
    height: Option<f32>,
    background_color: Option<String>,
    stroke_color: Option<String>,
    shape_type: Option<String>,
    /// The update endpoint's field is `radius` (create/response use
    /// `border_radius`), mirroring the server's asymmetry.
    radius: Option<f32>,
    stroke_width: Option<f32>,
    is_secret: Option<bool>,
}

/// The receipt request hash (`contract::request_hash`): SHA-256 over the
/// scope area plus the canonical serialization of the ordered operations.
fn request_hash(area_id: Uuid, ops: &[AreaOp]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(area_id.as_bytes());
    hasher.update(serde_json::to_vec(ops).expect("mutation operations always serialize"));
    hex::encode(hasher.finalize())
}

/// Caller/scope facts resolved once per envelope (`ops::ApplyCtx`).
struct ApplyCtx {
    viewer: Uuid,
    area_id: Uuid,
    cleared: bool,
    see: bool,
}

/// How revisions must move for one applied operation (`ops::OpOutcome`).
struct OpOutcome {
    result: Value,
    /// Whether any scope-area row changed at all. An idempotent no-op (a
    /// tag re-add) reports false so an all-no-op envelope moves no revision
    /// counter — the row triggers never fired when no row changed either.
    changed: bool,
    /// Whether the scope area's public projection changed.
    public_changed: bool,
    foreign_bumps: Vec<(Uuid, bool)>,
}

impl OpOutcome {
    fn scoped(result: Value, public_changed: bool) -> Self {
        Self {
            result,
            changed: true,
            public_changed,
            foreign_bumps: Vec::new(),
        }
    }
}

/// Room tags are stored trimmed-UPPERCASE (the case-insensitive tag
/// invariant); every entry path into the applier normalizes here, so a
/// compound-route tag can never bypass the invariant the per-entity route
/// enforces. Empty after trimming is a validation failure.
fn normalize_tag(tag: &str) -> Result<String, Response> {
    let normalized = tag.trim().to_uppercase();
    if normalized.is_empty() {
        return Err(bad_request("tag must not be empty"));
    }
    Ok(normalized)
}

type Working = BTreeMap<Uuid, AreaRecord>;

/// POST /areas/{id}/mutations.
pub async fn area_mutations(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };

    // Fast-path authorization, like the route handler: uniform 404 for an
    // absent area or a caller without can_edit.
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }

    let envelope: WireEnvelope = match serde_json::from_str(&body) {
        Ok(envelope) => envelope,
        Err(e) => return bad_request(&format!("invalid mutation envelope: {e}")),
    };
    if envelope.payload.is_empty() {
        return bad_request("empty mutation");
    }
    if envelope.payload.len() > MAX_MUTATION_OPERATIONS {
        return bad_request(&format!("too many operations (max {MAX_MUTATION_OPERATIONS})"));
    }

    let request_hash = request_hash(area_id, &envelope.payload);

    // The caller's CURRENT fingerprint, resolved before the receipt gate
    // (auth/caps precede any stored-result disclosure, exactly like the
    // server's early caps derivation).
    let current_fingerprint = access_fingerprint(&caps);

    // Receipt gate: an identical resend replays the stored body verbatim
    // (nothing re-applies, no revision moves) — but only to a caller whose
    // projection still matches the accept-time fingerprint; anyone else gets
    // `projection_changed` and refetches. A different body under an
    // already-used id is a client bug.
    if let Some(receipt) = st.mutation_receipts.get(&(viewer, envelope.operation_id)) {
        if receipt.request_hash != request_hash {
            return err(409, "operation_id_reused");
        }
        if receipt.access_fingerprint != current_fingerprint {
            return err_with_details(
                409,
                "projection_changed",
                json!({ "access_fingerprint": current_fingerprint }),
            );
        }
        let stored = receipt.result.clone();
        st.mutation_log.push((envelope.operation_id, true));
        return finish(&mut st, ok(stored));
    }

    // Preconditions: exactly one, naming the scope area, judged against the
    // CALLER'S projection (full rev for secret-seers, public rev otherwise).
    let projected_rev = {
        let area = st.areas.get(&area_id).expect("caps proved the area exists");
        if caps.see_secrets() {
            area.rev
        } else {
            area.public_rev
        }
    };
    let precondition = match envelope.preconditions.as_slice() {
        [p] if p.resource == "area" && p.id == area_id => p,
        _ => return bad_request("expected exactly one precondition naming the mutated area"),
    };
    // The fingerprint is mandatory: rev and public_rev are independent
    // counters that can numerically coincide, so a bare revision could pass
    // against the wrong projection class right after a capability change.
    let Some(precondition_fingerprint) = precondition.access_fingerprint.as_deref() else {
        return bad_request("area precondition requires the access fingerprint");
    };
    if precondition_fingerprint != current_fingerprint {
        return err_with_details(
            409,
            "projection_changed",
            json!({ "access_fingerprint": current_fingerprint }),
        );
    }
    if precondition.expected_rev != projected_rev {
        return err_with_details(
            409,
            "revision_conflict",
            json!({
                "resource": "area",
                "id": area_id,
                "expected_rev": precondition.expected_rev,
                "current_rev": projected_rev,
                "operation_id": envelope.operation_id,
            }),
        );
    }

    // Apply in order against a working copy — any failure returns with
    // nothing applied (the transaction-rollback analogue).
    let ctx = ApplyCtx {
        viewer,
        area_id,
        cleared: caps.cleared(),
        see: caps.see_secrets(),
    };
    let mut working: Working = st.areas.clone();
    let mut results: Vec<Value> = Vec::with_capacity(envelope.payload.len());
    let mut scope_changed = false;
    let mut scope_public = false;
    let mut foreign: BTreeMap<Uuid, bool> = BTreeMap::new();
    for op in envelope.payload {
        match apply_op(&st, &mut working, &ctx, op) {
            Ok(outcome) => {
                scope_changed |= outcome.changed;
                scope_public |= outcome.public_changed;
                for (foreign_area, public) in outcome.foreign_bumps {
                    if foreign_area == area_id {
                        scope_public |= public;
                    } else {
                        *foreign.entry(foreign_area).or_default() |= public;
                    }
                }
                results.push(outcome.result);
            }
            Err(response) => return response,
        }
    }

    // Commit, then exactly one bump per touched aggregate (sorted order),
    // reporting each at the caller's own projection of it. Foreign areas the
    // caller cannot view still bump but stay out of the response. An
    // envelope whose every operation was a no-op (idempotent tag re-adds)
    // moves no counter at all — matching the row triggers, which never
    // fired when no row changed — and reports the standing revision instead.
    st.areas = working;
    let mut bumps = foreign;
    if scope_changed || scope_public {
        bumps.insert(area_id, scope_public);
    }
    let mut versions: Vec<Value> = Vec::new();
    if bumps.is_empty() {
        versions.push(json!({
            "resource": "area",
            "id": area_id,
            "rev": projected_rev,
            "deleted": false,
        }));
    }
    for (bump_area, public) in &bumps {
        st.bump(Some(*bump_area), *public, false);
        let Some(area) = st.areas.get(bump_area) else {
            continue;
        };
        if *bump_area == area_id {
            versions.push(json!({
                "resource": "area",
                "id": bump_area,
                "rev": if ctx.see { area.rev } else { area.public_rev },
                "deleted": false,
            }));
        } else if let Some(foreign_caps) =
            st.caps(viewer, *bump_area).filter(|c| c.can_view)
        {
            versions.push(json!({
                "resource": "area",
                "id": bump_area,
                "rev": if foreign_caps.see_secrets() { area.rev } else { area.public_rev },
                "deleted": false,
            }));
        }
    }

    let result = json!({
        "operation_id": envelope.operation_id,
        "versions": versions,
        "data": results,
    });
    st.mutation_receipts.insert(
        (viewer, envelope.operation_id),
        MutationReceipt {
            request_hash,
            access_fingerprint: current_fingerprint,
            result: result.clone(),
        },
    );
    st.mutation_log.push((envelope.operation_id, false));
    finish(&mut st, ok(result))
}

/// Applies the response-drop test hook: a queued drop swallows this (fully
/// committed) response and serves a 500 in its place.
fn finish(st: &mut MockState, response: Response) -> Response {
    if st.drop_mutation_responses > 0 {
        st.drop_mutation_responses -= 1;
        return err(500, "injected response loss");
    }
    response
}

fn apply_op(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    op: AreaOp,
) -> Result<OpOutcome, Response> {
    match op {
        AreaOp::UpsertRoom { room_number, body } => {
            apply_upsert_room(st, working, ctx, room_number, &body)
        }
        AreaOp::DeleteRoom { room_number } => apply_delete_room(working, ctx, room_number),
        AreaOp::UpsertRoomProperty {
            room_number,
            name,
            value,
            is_secret,
        } => apply_upsert_room_property(working, ctx, room_number, name, &value, is_secret),
        AreaOp::DeleteRoomProperty { room_number, name } => {
            apply_delete_room_property(working, ctx, room_number, name)
        }
        AreaOp::AddRoomTag { room_number, tag } => {
            apply_room_tag(working, ctx, room_number, &tag, true)
        }
        AreaOp::RemoveRoomTag { room_number, tag } => {
            apply_room_tag(working, ctx, room_number, &tag, false)
        }
        AreaOp::UpsertAreaProperty {
            name,
            value,
            is_secret,
        } => apply_upsert_area_property(working, ctx, name, &value, is_secret),
        AreaOp::DeleteAreaProperty { name } => apply_delete_area_property(working, ctx, name),
        AreaOp::CreateExit { room_number, body } => {
            apply_create_exit(st, working, ctx, room_number, body)
        }
        AreaOp::UpdateExit { exit_id, body } => apply_update_exit(st, working, ctx, exit_id, body),
        AreaOp::DeleteExit { exit_id } => apply_delete_exit(working, ctx, exit_id),
        AreaOp::CreateLabel { body } => apply_create_label(working, ctx, body),
        AreaOp::UpdateLabel { label_id, body } => apply_update_label(working, ctx, label_id, &body),
        AreaOp::DeleteLabel { label_id } => apply_delete_label(working, ctx, label_id),
        AreaOp::CreateShape { body } => apply_create_shape(working, ctx, body),
        AreaOp::UpdateShape { shape_id, body } => apply_update_shape(working, ctx, shape_id, &body),
        AreaOp::DeleteShape { shape_id } => apply_delete_shape(working, ctx, shape_id),
    }
}

fn scope_area<'a>(working: &'a mut Working, ctx: &ApplyCtx) -> &'a mut AreaRecord {
    working
        .get_mut(&ctx.area_id)
        .expect("scope area presence was proven by authorization")
}

/// One room's echo (`ops.rs` room result): full record at the caller's
/// projection — properties filtered by see_secrets, exits projected with the
/// survival predicate and hidden-target redaction.
fn room_echo(st: &MockState, working: &Working, ctx: &ApplyCtx, room_number: i32) -> Value {
    let area = &working[&ctx.area_id];
    let room = &area.rooms[&room_number];
    let properties: Vec<Value> = room
        .properties
        .iter()
        .filter(|(_, p)| ctx.see || !p.is_secret)
        .map(|(name, p)| json!({"name": name, "value": p.value}))
        .collect();
    let exits = project_room_exits(st, working, ctx, room_number);
    json!({
        "area_id": ctx.area_id,
        "room_number": room.room_number,
        "title": room.title,
        "description": room.description,
        "color": room.color,
        "level": room.level,
        "x": room.x,
        "y": room.y,
        "created_at": room.created_at,
        "external_id": room.external_id,
        "properties": properties,
        "exits": exits,
    })
}

/// One room's projected exits (`MapQueries::project_room_exits`): the §6
/// closure evaluated over each exit's whole Connection — every member, both
/// endpoint rooms, and every concrete destination — so the echo can never
/// show a member of a group the full projection omits. A surviving exit
/// into a non-viewable area keeps its row but has its destination nulled.
fn project_room_exits(
    st: &MockState,
    working: &Working,
    ctx: &ApplyCtx,
    room_number: i32,
) -> Vec<Value> {
    let area = &working[&ctx.area_id];
    let mut out = Vec::new();
    for exit in area
        .exits
        .iter()
        .filter(|e| e.from_room_number == room_number)
    {
        let survives = area
            .connections
            .iter()
            .find(|connection| connection.id == exit.connection_id)
            .is_some_and(|connection| {
                !connection_verdict_in(
                    working,
                    |target| st.caps(ctx.viewer, target),
                    area,
                    connection,
                )
                .omitted(ctx.see)
            });
        if !survives {
            continue;
        }
        let visible = exit.to_area_id.is_none_or(|to_area| {
            to_area == ctx.area_id || st.caps(ctx.viewer, to_area).is_some_and(|c| c.can_view)
        });
        let mut projected = embedded_exit_json(exit);
        if !visible {
            projected["to_area_id"] = Value::Null;
            projected["to_room_number"] = Value::Null;
            projected["to_direction"] = Value::Null;
        }
        out.push(projected);
    }
    out
}

/// The destination matrix (`resolve_exit_destination_tx`), against the
/// working copy and without revision side effects: viewable target,
/// link-or-placeholder, the bounded secret-room 409 oracle. Returns whether
/// a destination placeholder room was created — a new PUBLIC row whose
/// appearance the caller folds into the target area's public-projection
/// decision, regardless of the exit's own secrecy.
fn resolve_destination(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    to_area_id: Uuid,
    to_room_number: Option<i32>,
) -> Result<bool, Response> {
    let caps = st.caps(ctx.viewer, to_area_id).unwrap_or(Caps::NONE);
    let same_area = to_area_id == ctx.area_id;
    let target_cleared = if same_area {
        ctx.cleared
    } else {
        caps.see_secrets()
    };

    if !same_area && !caps.can_view {
        return Err(not_found());
    }
    let Some(to_room) = to_room_number else {
        return Ok(false);
    };

    let existing_secret = working
        .get(&to_area_id)
        .and_then(|a| a.rooms.get(&to_room))
        .map(|room| room.is_secret);
    match existing_secret {
        Some(secret) => {
            if secret && !target_cleared {
                // 409 only for callers who can edit the target; view-only
                // callers get the uniform 404 (no secret-room oracle).
                if same_area || caps.can_edit {
                    return Err(err(409, "room number unavailable"));
                }
                return Err(not_found());
            }
            // Visible (or cleared) room -> link, nothing to create.
            Ok(false)
        }
        None => {
            if !same_area && !caps.can_edit {
                return Err(not_found());
            }
            let mut created = false;
            if let Some(area) = working.get_mut(&to_area_id) {
                area.rooms.insert(to_room, RoomRecord::placeholder(to_room));
                created = true;
            }
            Ok(created)
        }
    }
}

fn apply_upsert_room(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    room_number: i32,
    body: &UpsertRoomBody,
) -> Result<OpOutcome, Response> {
    if body.is_secret.is_some() && !ctx.cleared {
        return Err(not_found());
    }
    let (old_secret, new_secret) = {
        let area = scope_area(working, ctx);
        match area.rooms.get_mut(&room_number) {
            Some(room) => {
                if room.is_secret && !ctx.cleared {
                    return Err(err(409, "room number unavailable"));
                }
                let old = room.is_secret;
                if let Some(title) = body.title.clone() {
                    room.title = title;
                }
                if let Some(description) = body.description.clone() {
                    room.description = description;
                }
                if let Some(level) = body.level {
                    room.level = level;
                }
                if let Some(x) = body.x {
                    room.x = x;
                }
                if let Some(y) = body.y {
                    room.y = y;
                }
                if let Some(color) = body.color.clone() {
                    room.color = color;
                }
                if let Some(binding) = body.external_id.clone() {
                    room.external_id = binding;
                }
                room.is_secret = body.is_secret.unwrap_or(room.is_secret);
                (Some(old), room.is_secret)
            }
            None => {
                let secret = body.is_secret.unwrap_or(false);
                area.rooms.insert(
                    room_number,
                    RoomRecord {
                        room_number,
                        title: body.title.clone().unwrap_or_default(),
                        description: body.description.clone().unwrap_or_default(),
                        level: body.level.unwrap_or(0),
                        x: body.x.unwrap_or(0.0),
                        y: body.y.unwrap_or(0.0),
                        color: body.color.clone().unwrap_or_default(),
                        is_secret: secret,
                        external_id: body.external_id.clone().flatten(),
                        ..RoomRecord::placeholder(room_number)
                    },
                );
                (None, secret)
            }
        }
    };
    let public_changed = match old_secret {
        None => !new_secret,
        Some(old) => !(old && new_secret),
    };
    Ok(OpOutcome::scoped(
        json!({"entity": "room", "room": room_echo(st, working, ctx, room_number)}),
        public_changed,
    ))
}

fn apply_delete_room(
    working: &mut Working,
    ctx: &ApplyCtx,
    room_number: i32,
) -> Result<OpOutcome, Response> {
    let old_secret = working
        .get(&ctx.area_id)
        .and_then(|a| a.rooms.get(&room_number))
        .map(|room| room.is_secret)
        .ok_or_else(not_found)?;
    // Tightened over the legacy delete route: removing a secret room
    // requires clearance (uniform 404 otherwise).
    if old_secret && !ctx.cleared {
        return Err(not_found());
    }

    // Cross-area effects, gathered before the rows change: outbound exit
    // targets (their rows cascade away) and inbound exit origins (their
    // rows lose destinations).
    let mut partners: BTreeMap<Uuid, bool> = BTreeMap::new();
    for exit in &working[&ctx.area_id].exits {
        if exit.from_room_number == room_number
            && let Some(to_area) = exit.to_area_id
            && to_area != ctx.area_id
        {
            *partners.entry(to_area).or_default() |= !exit.is_secret;
        }
    }
    for (host_id, host) in working.iter() {
        if *host_id == ctx.area_id {
            continue;
        }
        for exit in &host.exits {
            if exit.to_area_id == Some(ctx.area_id) && exit.to_room_number == Some(room_number) {
                *partners.entry(*host_id).or_default() |= !exit.is_secret;
            }
        }
    }

    // §3.3 repair, in the server's order: outgoing exits removed, inbound
    // destinations (any area) nulled, this area's touched Connections
    // converted to dangling or removed as orphans — then the room itself.
    connections::repair_after_room_delete(working, ctx.area_id, room_number);
    scope_area(working, ctx).rooms.remove(&room_number);

    // A secret room's removal moves no public projection anywhere (legacy
    // suppression semantics).
    let suppress = old_secret;
    Ok(OpOutcome {
        result: json!({"entity": "room_deleted", "room_number": room_number}),
        changed: true,
        public_changed: !suppress,
        foreign_bumps: partners
            .into_iter()
            .map(|(area_id, any_public)| (area_id, any_public && !suppress))
            .collect(),
    })
}

fn apply_upsert_room_property(
    working: &mut Working,
    ctx: &ApplyCtx,
    room_number: i32,
    name: String,
    value: &str,
    is_secret: Option<bool>,
) -> Result<OpOutcome, Response> {
    if is_secret.is_some() && !ctx.cleared {
        return Err(not_found());
    }
    let area = scope_area(working, ctx);
    let Some(room) = area.rooms.get_mut(&room_number) else {
        return Err(err(404, "Room not found"));
    };
    let (old_secret, new_secret) = match room.properties.get_mut(&name) {
        Some(existing) => {
            if existing.is_secret && !ctx.cleared {
                return Err(err(409, "property name unavailable"));
            }
            let old = existing.is_secret;
            existing.value = value.to_string();
            existing.is_secret = is_secret.unwrap_or(existing.is_secret);
            (old, existing.is_secret)
        }
        None => {
            let secret = is_secret.unwrap_or(false);
            room.properties.insert(
                name.clone(),
                RoomPropRecord {
                    value: value.to_string(),
                    is_secret: secret,
                },
            );
            (secret, secret)
        }
    };
    Ok(OpOutcome::scoped(
        json!({"entity": "room_property", "room_number": room_number, "name": name}),
        !(old_secret && new_secret),
    ))
}

fn apply_delete_room_property(
    working: &mut Working,
    ctx: &ApplyCtx,
    room_number: i32,
    name: String,
) -> Result<OpOutcome, Response> {
    let area = scope_area(working, ctx);
    let Some(room) = area.rooms.get_mut(&room_number) else {
        return Err(err(404, "Room not found"));
    };
    let Some(prop) = room.properties.remove(&name) else {
        return Err(err(404, "Room property not found"));
    };
    Ok(OpOutcome::scoped(
        json!({"entity": "room_property_deleted", "room_number": room_number, "name": name}),
        !prop.is_secret,
    ))
}

/// Add or remove one room tag, normalized trim+UPPERCASE (`ops.rs`
/// `normalize_tag`). Tags carry no secrecy: an actual insert or removal is
/// always public content; an idempotent re-add changes nothing and must not
/// move any counter.
fn apply_room_tag(
    working: &mut Working,
    ctx: &ApplyCtx,
    room_number: i32,
    tag: &str,
    add: bool,
) -> Result<OpOutcome, Response> {
    let normalized = normalize_tag(tag)?;
    let area = scope_area(working, ctx);
    let Some(room) = area.rooms.get_mut(&room_number) else {
        return Err(err(404, "Room not found"));
    };
    if add {
        let inserted = room.tags.insert(normalized.clone());
        Ok(OpOutcome {
            result: json!({"entity": "room_tag", "room_number": room_number, "tag": normalized}),
            changed: inserted,
            public_changed: inserted,
            foreign_bumps: Vec::new(),
        })
    } else {
        // The deployed server 404s an absent tag and rolls the envelope
        // back (`delete_room_tag_in_tx` checks `rows_affected`).
        if !room.tags.remove(&normalized) {
            return Err(err(404, "Not found"));
        }
        Ok(OpOutcome::scoped(
            json!({"entity": "room_tag_removed", "room_number": room_number, "tag": normalized}),
            true,
        ))
    }
}

fn apply_upsert_area_property(
    working: &mut Working,
    ctx: &ApplyCtx,
    name: String,
    value: &str,
    is_secret: Option<bool>,
) -> Result<OpOutcome, Response> {
    if is_secret.is_some() && !ctx.cleared {
        return Err(not_found());
    }
    let area = scope_area(working, ctx);
    let (old_secret, new_secret) = match area.properties.get_mut(&name) {
        Some(existing) => {
            if existing.is_secret && !ctx.cleared {
                return Err(err(409, "property name unavailable"));
            }
            let old = existing.is_secret;
            existing.value = value.to_string();
            existing.created_at = chrono::Utc::now();
            existing.is_secret = is_secret.unwrap_or(existing.is_secret);
            (old, existing.is_secret)
        }
        None => {
            let secret = is_secret.unwrap_or(false);
            area.properties.insert(
                name.clone(),
                AreaPropRecord {
                    value: value.to_string(),
                    is_secret: secret,
                    created_at: chrono::Utc::now(),
                },
            );
            (secret, secret)
        }
    };
    Ok(OpOutcome::scoped(
        json!({"entity": "area_property", "name": name}),
        !(old_secret && new_secret),
    ))
}

fn apply_delete_area_property(
    working: &mut Working,
    ctx: &ApplyCtx,
    name: String,
) -> Result<OpOutcome, Response> {
    let area = scope_area(working, ctx);
    let Some(prop) = area.properties.remove(&name) else {
        return Err(err(404, "Property not found"));
    };
    Ok(OpOutcome::scoped(
        json!({"entity": "area_property_deleted", "name": name}),
        !prop.is_secret,
    ))
}

fn apply_create_exit(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    room_number: i32,
    body: CreateExitBody,
) -> Result<OpOutcome, Response> {
    check_enum(&body.from_direction, &DIRECTIONS, "direction")?;
    if let Some(d) = &body.to_direction {
        check_enum(d, &DIRECTIONS, "direction")?;
    }
    if body.is_secret.is_some() && !ctx.cleared {
        return Err(not_found());
    }

    let mut destination_placeholder = false;
    if let Some(to_area_id) = body.to_area_id {
        destination_placeholder =
            resolve_destination(st, working, ctx, to_area_id, body.to_room_number)?;
    }

    // From-room placeholder (always allowed — the caller can edit the host).
    // Placeholder rooms are public rows: their creation is a public
    // projection change regardless of the exit's own secrecy.
    let from_placeholder = {
        let area = scope_area(working, ctx);
        let created = !area.rooms.contains_key(&room_number);
        area.rooms
            .entry(room_number)
            .or_insert_with(|| RoomRecord::placeholder(room_number));
        created
    };

    // The Connection carrying the new exit: auto-pair with the unique
    // reciprocal one-member candidate, or mint a one-member Connection with
    // §1.5 anchors and §4.3 port slots (mirrors `attach_for_new_exit`).
    let connection_id = attach_for_new_exit(
        working,
        ctx.area_id,
        &NewExitLink {
            from_room: room_number,
            from_direction: body.from_direction.clone(),
            to_area_id: body.to_area_id,
            to_room_number: body.to_room_number,
            to_direction: body.to_direction.clone(),
            is_secret: body.is_secret.unwrap_or(false),
        },
        ctx.cleared,
    );

    let exit = ExitRecord {
        id: body.id.unwrap_or_else(Uuid::new_v4),
        from_room_number: room_number,
        from_direction: body.from_direction,
        to_area_id: body.to_area_id,
        to_room_number: body.to_room_number,
        to_direction: body.to_direction,
        path: body.path.unwrap_or_default(),
        is_hidden: body.is_hidden,
        is_closed: body.is_closed,
        is_locked: body.is_locked,
        weight: body.weight,
        command: body.command.unwrap_or_default(),
        connection_id,
        is_secret: body.is_secret.unwrap_or(false),
    };
    let result = json!({"entity": "exit", "exit": embedded_exit_json(&exit)});
    let new_secret = exit.is_secret;
    let same_area_placeholder = destination_placeholder && exit.to_area_id == Some(ctx.area_id);
    let foreign_bumps = match exit.to_area_id {
        Some(to) if to != ctx.area_id => vec![(to, !new_secret || destination_placeholder)],
        _ => Vec::new(),
    };
    scope_area(working, ctx).exits.push(exit);
    Ok(OpOutcome {
        result,
        changed: true,
        public_changed: !new_secret || from_placeholder || same_area_placeholder,
        foreign_bumps,
    })
}

fn apply_update_exit(
    st: &MockState,
    working: &mut Working,
    ctx: &ApplyCtx,
    exit_id: Uuid,
    body: UpdateExitBody,
) -> Result<OpOutcome, Response> {
    if let Some(d) = &body.from_direction {
        check_enum(d, &DIRECTIONS, "direction")?;
    }
    if let Some(d) = &body.to_direction {
        check_enum(d, &DIRECTIONS, "direction")?;
    }

    let current = working[&ctx.area_id]
        .exits
        .iter()
        .find(|e| e.id == exit_id)
        .cloned()
        .ok_or_else(not_found)?;
    if body.is_secret.is_some() && !ctx.cleared {
        return Err(not_found());
    }
    if current.is_secret && !ctx.cleared {
        return Err(not_found());
    }

    let clear_to = body.clear_to.unwrap_or(false);
    let (new_to_area, new_to_room) = if clear_to {
        (None, None)
    } else {
        (
            body.to_area_id.or(current.to_area_id),
            body.to_room_number.or(current.to_room_number),
        )
    };
    let destination_changed =
        clear_to || body.to_area_id.is_some() || body.to_room_number.is_some();

    // §3.2: a member of a pair cannot be made non-reciprocal in place —
    // retargets and departure-direction changes on a two-member Connection
    // are refused with the unlink-then-edit prompt. Traversal-only fields
    // (path, command, weight, flags, secrecy) stay editable.
    let member_count = working[&ctx.area_id]
        .exits
        .iter()
        .filter(|e| e.connection_id == current.connection_id)
        .count();
    if member_count == 2 && (destination_changed || body.from_direction.is_some()) {
        return Err(err_with_details(
            409,
            "structural_conflict",
            json!({ "reason": "unlink_before_edit" }),
        ));
    }

    let mut destination_placeholder = false;
    if destination_changed
        && let Some(to_area_id) = new_to_area
    {
        destination_placeholder = resolve_destination(st, working, ctx, to_area_id, new_to_room)?;
    }

    let updated = {
        let area = scope_area(working, ctx);
        let exit = area
            .exits
            .iter_mut()
            .find(|e| e.id == exit_id)
            .expect("existence checked above");
        if let Some(d) = body.from_direction {
            exit.from_direction = d;
        }
        if clear_to {
            exit.to_area_id = None;
            exit.to_room_number = None;
            exit.to_direction = None;
        } else {
            if let Some(a) = body.to_area_id {
                exit.to_area_id = Some(a);
            }
            if let Some(r) = body.to_room_number {
                exit.to_room_number = Some(r);
            }
            if let Some(d) = body.to_direction {
                exit.to_direction = Some(d);
            }
        }
        if let Some(p) = body.path {
            exit.path = p;
        }
        if let Some(v) = body.is_hidden {
            exit.is_hidden = v;
        }
        if let Some(v) = body.is_closed {
            exit.is_closed = v;
        }
        if let Some(v) = body.is_locked {
            exit.is_locked = v;
        }
        if let Some(v) = body.weight {
            exit.weight = v;
        }
        if let Some(c) = body.command {
            exit.command = c;
        }
        if let Some(s) = body.is_secret {
            exit.is_secret = s;
        }
        exit.clone()
    };

    // A destination change on a (now guaranteed) one-member Connection
    // atomically maintains endpoint B per §3.2.
    if destination_changed {
        maintain_after_retarget(working, ctx.area_id, current.connection_id);
    }

    let new_secret = updated.is_secret;
    // A same-area destination placeholder is a new public room in the scope.
    let same_area_placeholder =
        destination_placeholder && updated.to_area_id == Some(ctx.area_id);
    let public_changed = !(current.is_secret && new_secret) || same_area_placeholder;
    let mut foreign_bumps: Vec<(Uuid, bool)> = Vec::new();
    let target_unchanged = current.to_area_id == updated.to_area_id;
    if let Some(old_to) = current.to_area_id.filter(|to| *to != ctx.area_id) {
        // An unchanged target follows the secret-transition predicate (a
        // reveal is public news there too); a departed target saw the exit
        // at its old secrecy.
        let old_public = if target_unchanged {
            !(current.is_secret && new_secret)
        } else {
            !current.is_secret
        };
        foreign_bumps.push((old_to, old_public || (target_unchanged && destination_placeholder)));
    }
    if let Some(new_to) = updated
        .to_area_id
        .filter(|to| *to != ctx.area_id && Some(*to) != current.to_area_id)
    {
        foreign_bumps.push((new_to, !new_secret || destination_placeholder));
    }
    Ok(OpOutcome {
        result: json!({"entity": "exit", "exit": embedded_exit_json(&updated)}),
        changed: true,
        public_changed,
        foreign_bumps,
    })
}

fn apply_delete_exit(
    working: &mut Working,
    ctx: &ApplyCtx,
    exit_id: Uuid,
) -> Result<OpOutcome, Response> {
    let exit = {
        let area = scope_area(working, ctx);
        let Some(idx) = area.exits.iter().position(|e| e.id == exit_id) else {
            return Err(not_found());
        };
        // Tightened over the legacy delete route: removing a secret exit
        // requires clearance (uniform 404 otherwise).
        if area.exits[idx].is_secret && !ctx.cleared {
            return Err(not_found());
        }
        area.exits.remove(idx)
    };
    // §3.3: the last member takes the Connection with it.
    cleanup_after_exit_delete(working, ctx.area_id, exit.connection_id);
    let foreign_bumps = match exit.to_area_id {
        Some(to) if to != ctx.area_id => vec![(to, !exit.is_secret)],
        _ => Vec::new(),
    };
    Ok(OpOutcome {
        result: json!({"entity": "exit_deleted", "exit_id": exit_id}),
        changed: true,
        public_changed: !exit.is_secret,
        foreign_bumps,
    })
}

fn apply_create_label(
    working: &mut Working,
    ctx: &ApplyCtx,
    body: CreateLabelBody,
) -> Result<OpOutcome, Response> {
    check_enum(&body.horizontal_alignment, &H_ALIGN, "horizontal alignment")?;
    check_enum(&body.vertical_alignment, &V_ALIGN, "vertical alignment")?;
    if body.is_secret.is_some() && !ctx.cleared {
        return Err(not_found());
    }
    let label = LabelRecord {
        id: body.id.unwrap_or_else(Uuid::new_v4),
        level: body.level.unwrap_or(0),
        x: body.x,
        y: body.y,
        width: body.width,
        height: body.height,
        horizontal_alignment: body.horizontal_alignment,
        vertical_alignment: body.vertical_alignment,
        text: body.text,
        color: body.color.unwrap_or_else(|| "black".to_string()),
        background_color: body.background_color.unwrap_or_else(|| "white".to_string()),
        font_size: body.font_size.unwrap_or(12),
        font_weight: body.font_weight.unwrap_or(400),
        is_secret: body.is_secret.unwrap_or(false),
    };
    let result = json!({"entity": "label", "label": embedded_label_json(&label)});
    let secret = label.is_secret;
    scope_area(working, ctx).labels.push(label);
    Ok(OpOutcome::scoped(result, !secret))
}

fn apply_update_label(
    working: &mut Working,
    ctx: &ApplyCtx,
    label_id: Uuid,
    body: &UpdateLabelBody,
) -> Result<OpOutcome, Response> {
    if let Some(a) = &body.horizontal_alignment {
        check_enum(a, &H_ALIGN, "horizontal alignment")?;
    }
    if let Some(a) = &body.vertical_alignment {
        check_enum(a, &V_ALIGN, "vertical alignment")?;
    }
    if body.is_secret.is_some() && !ctx.cleared {
        return Err(not_found());
    }
    let cleared = ctx.cleared;
    let area = scope_area(working, ctx);
    let Some(label) = area.labels.iter_mut().find(|l| l.id == label_id) else {
        return Err(err(404, "Label not found"));
    };
    if label.is_secret && !cleared {
        return Err(not_found());
    }
    let old_secret = label.is_secret;
    if let Some(v) = body.level {
        label.level = v;
    }
    if let Some(v) = body.x {
        label.x = v;
    }
    if let Some(v) = body.y {
        label.y = v;
    }
    if let Some(v) = body.width {
        label.width = v;
    }
    if let Some(v) = body.height {
        label.height = v;
    }
    if let Some(v) = body.horizontal_alignment.clone() {
        label.horizontal_alignment = v;
    }
    if let Some(v) = body.vertical_alignment.clone() {
        label.vertical_alignment = v;
    }
    if let Some(v) = body.text.clone() {
        label.text = v;
    }
    if let Some(v) = body.color.clone() {
        label.color = v;
    }
    if let Some(v) = body.background_color.clone() {
        label.background_color = v;
    }
    if let Some(v) = body.font_size {
        label.font_size = v;
    }
    if let Some(v) = body.font_weight {
        label.font_weight = v;
    }
    if let Some(v) = body.is_secret {
        label.is_secret = v;
    }
    Ok(OpOutcome::scoped(
        json!({"entity": "label", "label": embedded_label_json(label)}),
        !(old_secret && label.is_secret),
    ))
}

fn apply_delete_label(
    working: &mut Working,
    ctx: &ApplyCtx,
    label_id: Uuid,
) -> Result<OpOutcome, Response> {
    let area = scope_area(working, ctx);
    let Some(idx) = area.labels.iter().position(|l| l.id == label_id) else {
        return Err(err(404, "Label not found"));
    };
    let label = area.labels.remove(idx);
    Ok(OpOutcome::scoped(
        json!({"entity": "label_deleted", "label_id": label_id}),
        !label.is_secret,
    ))
}

fn apply_create_shape(
    working: &mut Working,
    ctx: &ApplyCtx,
    body: CreateShapeBody,
) -> Result<OpOutcome, Response> {
    check_enum(&body.shape_type, &SHAPE_TYPES, "shape type")?;
    if body.is_secret.is_some() && !ctx.cleared {
        return Err(not_found());
    }
    let shape = ShapeRecord {
        id: body.id.unwrap_or_else(Uuid::new_v4),
        level: body.level.unwrap_or(0),
        x: body.x,
        y: body.y,
        width: body.width,
        height: body.height,
        background_color: Some(body.background_color.unwrap_or_else(|| "grey".to_string())),
        stroke_color: Some(body.stroke_color.unwrap_or_else(|| "transparent".to_string())),
        shape_type: body.shape_type,
        border_radius: body.border_radius.unwrap_or(0.0),
        stroke_width: body.stroke_width.unwrap_or(1.0),
        is_secret: body.is_secret.unwrap_or(false),
    };
    let result = json!({"entity": "shape", "shape": embedded_shape_json(&shape)});
    let secret = shape.is_secret;
    scope_area(working, ctx).shapes.push(shape);
    Ok(OpOutcome::scoped(result, !secret))
}

fn apply_update_shape(
    working: &mut Working,
    ctx: &ApplyCtx,
    shape_id: Uuid,
    body: &UpdateShapeBody,
) -> Result<OpOutcome, Response> {
    if let Some(t) = &body.shape_type {
        check_enum(t, &SHAPE_TYPES, "shape type")?;
    }
    if body.is_secret.is_some() && !ctx.cleared {
        return Err(not_found());
    }
    let cleared = ctx.cleared;
    let area = scope_area(working, ctx);
    let Some(shape) = area.shapes.iter_mut().find(|s| s.id == shape_id) else {
        return Err(err(404, "Shape not found"));
    };
    if shape.is_secret && !cleared {
        return Err(not_found());
    }
    let old_secret = shape.is_secret;
    if let Some(v) = body.level {
        shape.level = v;
    }
    if let Some(v) = body.x {
        shape.x = v;
    }
    if let Some(v) = body.y {
        shape.y = v;
    }
    if let Some(v) = body.width {
        shape.width = v;
    }
    if let Some(v) = body.height {
        shape.height = v;
    }
    if let Some(v) = body.background_color.clone() {
        shape.background_color = Some(v);
    }
    if let Some(v) = body.stroke_color.clone() {
        shape.stroke_color = Some(v);
    }
    if let Some(v) = body.shape_type.clone() {
        shape.shape_type = v;
    }
    if let Some(v) = body.radius {
        shape.border_radius = v;
    }
    if let Some(v) = body.stroke_width {
        shape.stroke_width = v;
    }
    if let Some(v) = body.is_secret {
        shape.is_secret = v;
    }
    Ok(OpOutcome::scoped(
        json!({"entity": "shape", "shape": embedded_shape_json(shape)}),
        !(old_secret && shape.is_secret),
    ))
}

fn apply_delete_shape(
    working: &mut Working,
    ctx: &ApplyCtx,
    shape_id: Uuid,
) -> Result<OpOutcome, Response> {
    let area = scope_area(working, ctx);
    let Some(idx) = area.shapes.iter().position(|s| s.id == shape_id) else {
        return Err(err(404, "Shape not found"));
    };
    let shape = area.shapes.remove(idx);
    Ok(OpOutcome::scoped(
        json!({"entity": "shape_deleted", "shape_id": shape_id}),
        !shape.is_secret,
    ))
}
