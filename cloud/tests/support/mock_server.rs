//! Router assembly, server spawn, and ergonomic test helpers.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::HeaderValue;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use chrono::{Duration, Utc};
use parking_lot::Mutex;
use smudgy_cloud::AreaId;
use uuid::Uuid;

use super::state::{
    API_KEY_PREFIX, ApiKeyRecord, AreaPropRecord, AreaRecord, AtlasRecord, BlockRecord, ExitRecord,
    FriendStatus, FriendshipRecord, GrantRecord, LabelRecord, MockState, RoomPropRecord,
    RoomRecord, SESSION_PREFIX, SessionRecord, ShapeRecord, UserRecord, gen_token,
};
use super::{areas, clone, identity, mutations, secret_grants, secrets, shares, social, transfers};

pub type Shared = Arc<Mutex<MockState>>;

/// A user minted by [`MockHandle::create_user`], with both credential kinds.
#[derive(Debug, Clone)]
pub struct TestUser {
    pub id: Uuid,
    pub email: String,
    pub api_key: String,
    pub session_token: String,
}

/// Scope selector for [`MockHandle::grant`].
#[derive(Debug, Clone, Copy)]
pub enum GrantScope {
    Area(AreaId),
    Atlas(Uuid),
}

/// A room a Secret's content names, for the Secret pokes on [`MockHandle`]:
/// a room of the Secret's map, one of the Secret's own rooms, or a room of
/// another map.
#[derive(Debug, Clone, Copy)]
pub enum SecretPlace {
    Map(i32),
    Own(i32),
    Elsewhere(AreaId, i32),
}

/// Secret `secret` of map `area`, for a state poke.
fn secret_mut(st: &mut MockState, area: AreaId, secret: Uuid) -> &mut super::state::SecretRecord {
    st.areas
        .get_mut(&area.0)
        .expect("area exists")
        .secrets
        .iter_mut()
        .find(|candidate| candidate.id == secret)
        .expect("Secret exists")
}

/// Capability flags for [`MockHandle::grant`].
#[derive(Debug, Clone, Copy, Default)]
pub struct GrantFlags {
    pub can_edit: bool,
    pub can_reshare: bool,
    pub can_copy: bool,
    pub can_admin: bool,
}

impl GrantFlags {
    pub const VIEW_ONLY: Self = Self {
        can_edit: false,
        can_reshare: false,
        can_copy: false,
        can_admin: false,
    };

    #[must_use]
    pub const fn edit() -> Self {
        Self {
            can_edit: true,
            can_reshare: false,
            can_copy: false,
            can_admin: false,
        }
    }

    /// A full-deputy grant: `can_admin` implies the lower caps server-side.
    #[must_use]
    pub const fn admin() -> Self {
        Self {
            can_edit: false,
            can_reshare: false,
            can_copy: false,
            can_admin: true,
        }
    }
}

pub struct MockServer;

pub struct MockHandle {
    pub base_url: String,
    pub state: Shared,
    pub addr: SocketAddr,
}

impl MockServer {
    /// Bind 127.0.0.1:0, spawn the server, return the handle.
    pub async fn spawn() -> MockHandle {
        let state: Shared = Arc::new(Mutex::new(MockState::default()));
        let app = router(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock listener");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            // Runs until the test runtime is torn down.
            let _ = axum::serve(listener, app).await;
        });
        MockHandle {
            base_url: format!("http://{addr}"),
            state,
            addr,
        }
    }
}

fn router(state: Shared) -> Router {
    Router::new()
        .merge(
            super::clans::routes()
                .merge(super::clan_resources::routes())
                .layer(middleware::from_fn_with_state(
                    state.clone(),
                    credentials_first,
                )),
        )
        .merge(super::clan_maps::routes())
        .merge(super::clan_secrets::routes())
        // identity
        .route("/auth/login", post(identity::login))
        .route("/auth/verify-email", post(identity::verify_email))
        .route("/auth/logout", post(identity::logout))
        .route("/auth/refresh", post(identity::refresh_session))
        .route(
            "/me",
            get(identity::get_me)
                .patch(identity::patch_me)
                .delete(identity::delete_me),
        )
        .route(
            "/me/api-keys",
            get(identity::list_api_keys).post(identity::create_api_key),
        )
        .route("/me/api-keys/:key_id", delete(identity::delete_api_key))
        .route("/me/sessions", get(identity::list_sessions))
        .route("/me/sessions/:session_id", delete(identity::delete_session))
        // social
        .route("/users/lookup", get(social::lookup))
        .route("/friends", get(social::list_friends))
        .route(
            "/friends/requests",
            get(social::list_requests).post(social::send_request),
        )
        .route(
            "/friends/requests/:user_id/accept",
            post(social::accept_request),
        )
        .route("/friends/requests/:user_id", delete(social::delete_request))
        .route("/friends/:user_id", delete(social::unfriend))
        .route("/blocks", get(social::list_blocks))
        .route(
            "/blocks/:user_id",
            put(social::block).delete(social::unblock),
        )
        // shares
        .route(
            "/shares",
            post(shares::create_share).get(shares::list_shares),
        )
        .route(
            "/shares/:grant_id",
            axum::routing::patch(shares::patch_share).delete(shares::delete_share),
        )
        // areas + sync
        .route("/sync", get(areas::sync))
        .route("/areas", get(areas::list_areas).post(areas::create_area))
        .route(
            "/areas/:area_id",
            get(areas::get_area)
                .put(areas::update_area)
                .delete(areas::delete_area),
        )
        .route("/areas/:area_id/mutations", post(mutations::area_mutations))
        .route(
            "/areas/:area_id/local-move",
            get(areas::review_local_move).post(areas::finish_local_move),
        )
        .route("/areas/:area_id/shares", get(shares::area_shares))
        .route(
            "/areas/:area_id/secrets",
            get(clone::list_secrets).post(secrets::create_secret),
        )
        .route("/areas/:area_id/moves", post(super::reviewed_moves::commit))
        .route(
            "/areas/:area_id/filing-review",
            post(super::filing::preview),
        )
        .route(
            "/areas/:area_id/moves/preview",
            post(super::reviewed_moves::preview),
        )
        .route(
            "/secrets/:secret_id",
            axum::routing::patch(secrets::rename_secret).delete(secrets::delete_secret),
        )
        .route(
            "/secrets/:secret_id/grants",
            get(secret_grants::list_grants).post(secret_grants::create_grant),
        )
        .route(
            "/secrets/:secret_id/grants/:grant_id",
            axum::routing::patch(secret_grants::update_grant).delete(secret_grants::revoke_grant),
        )
        .route("/areas/:area_id/copy", post(clone::copy_area))
        .route(
            "/areas/:area_id/properties/:name",
            put(areas::upsert_area_property).delete(areas::delete_area_property),
        )
        .route("/areas/:area_id/labels", post(areas::create_label))
        .route(
            "/areas/:area_id/labels/:label_id",
            put(areas::update_label).delete(areas::delete_label),
        )
        .route("/areas/:area_id/shapes", post(areas::create_shape))
        .route(
            "/areas/:area_id/shapes/:shape_id",
            put(areas::update_shape).delete(areas::delete_shape),
        )
        // The per-entity exit and room-delete routes are gone from the mock:
        // on the real server they are thin envelope wrappers over the same
        // compound appliers, and every client path drives
        // POST /areas/{id}/mutations (see `mutations.rs`).
        .route(
            "/areas/:area_id/rooms/:room_number/properties/:name",
            put(areas::upsert_room_property).delete(areas::delete_room_property),
        )
        .route(
            "/areas/:area_id/rooms/:room_number/tags/:tag",
            put(areas::add_room_tag).delete(areas::remove_room_tag),
        )
        // NOTE the contract's bare room-upsert path: PUT /areas/{id}/{number}
        .route("/areas/:area_id/:room_number", put(areas::upsert_room))
        // atlas copy
        .route("/atlases/:atlas_id/copy", post(clone::copy_atlas))
        // ownership transfer
        .route(
            "/areas/:area_id/transfer",
            post(transfers::create_area_transfer),
        )
        .route(
            "/atlases/:atlas_id/transfer",
            post(transfers::create_atlas_transfer),
        )
        .route("/transfers", get(transfers::list_transfers))
        .route(
            "/clans/:clan_id/transfers",
            get(transfers::list_clan_transfers),
        )
        .route(
            "/transfers/:transfer_id/accept",
            post(transfers::accept_transfer),
        )
        .route(
            "/transfers/:transfer_id/decline",
            post(transfers::decline_transfer),
        )
        .route(
            "/transfers/:transfer_id",
            delete(transfers::cancel_transfer),
        )
        // Mirror the server's pre-routing version gate (see `function_handler`):
        // every request passes through it before any handler runs.
        .layer(middleware::from_fn_with_state(state.clone(), version_gate))
        .with_state(state)
}

/// A clan route answers a request without valid credentials 401, whatever
/// the shape of its path or body, mirroring the server's `credentialsFirst`
/// (src/clans/routes.ts): a shape refusal (a 400, or the 404 of a malformed
/// path ID), which a route may judge before it authenticates, is answered
/// only once the credentials are.
async fn credentials_first(State(state): State<Shared>, request: Request, next: Next) -> Response {
    let headers = request.headers().clone();
    let response = next.run(request).await;
    if matches!(response.status().as_u16(), 400 | 404)
        && let Err(denied) = super::http::authenticate(&state.lock(), &headers)
    {
        return denied;
    }
    response
}

/// Reject a too-old client with 426 before routing, mirroring the real
/// server's `enforce_client_version`. No-op unless a test raised the floor via
/// [`MockHandle::set_min_client_version`].
async fn version_gate(State(state): State<Shared>, request: Request, next: Next) -> Response {
    state.lock().http_requests.push((
        request.method().to_string(),
        request.uri().path().to_string(),
    ));
    let (rejection, upgrade) = {
        let st = state.lock();
        let headers = request.headers();
        (
            super::http::client_upgrade_rejection(&st, headers),
            super::http::upgrade_available_for(&st, headers),
        )
    };
    if let Some(response) = rejection {
        return response;
    }
    let mut response = next.run(request).await;
    // Soft upgrade hint for an in-range client, mirroring the server.
    if let Some(newest) = upgrade
        && let Ok(value) = HeaderValue::from_str(&newest)
    {
        response
            .headers_mut()
            .insert("x-smudgy-upgrade-available", value);
    }
    response
}

impl MockHandle {
    /// Insert a user with both credentials minted. `verified` claims the
    /// nickname (the handle) and sets `email_verified_at`.
    pub fn create_user(&self, email: &str, nickname: &str, verified: bool) -> TestUser {
        let mut st = self.state.lock();
        let user_id = Uuid::new_v4();
        st.users.push(UserRecord {
            id: user_id,
            email: email.to_string(),
            nickname: None,
            requested_nickname: Some(nickname.to_string()),
            email_verified_at: verified.then(Utc::now),
            nickname_updated_at: None,
            created_at: Utc::now(),
        });
        if verified {
            st.claim_nickname(user_id, nickname);
        }

        let api_key = gen_token(API_KEY_PREFIX);
        let key_suffix: String = api_key.chars().skip(api_key.len() - 8).collect();
        st.api_keys.insert(
            api_key.clone(),
            ApiKeyRecord {
                id: Uuid::new_v4(),
                user_id,
                key_suffix,
                created_at: Utc::now(),
                last_used_at: None,
            },
        );
        let session_token = gen_token(SESSION_PREFIX);
        st.sessions.insert(
            session_token.clone(),
            SessionRecord {
                id: Uuid::new_v4(),
                user_id,
                created_at: Utc::now(),
                expires_at: Utc::now() + Duration::days(365),
                last_used_at: None,
            },
        );
        TestUser {
            id: user_id,
            email: email.to_string(),
            api_key,
            session_token,
        }
    }

    /// Raise the mock's client-version floor (mirrors the server's
    /// `MIN_CLIENT_VERSION`). `None` by default leaves the gate disabled; pass
    /// `"0.0.0"` to explicitly disable it again.
    pub fn set_min_client_version(&self, version: &str) {
        self.state.lock().min_client_version = Some(version.to_string());
    }

    /// Advertise a newest-known version (mirrors `NEWEST_CLIENT_VERSION`) so an
    /// in-range client receives the soft `x-smudgy-upgrade-available` header.
    pub fn set_newest_client_version(&self, version: &str) {
        self.state.lock().newest_client_version = Some(version.to_string());
    }

    pub fn create_area(&self, owner: &TestUser, name: &str) -> AreaId {
        let mut st = self.state.lock();
        let seq = st.next_seq();
        let area = AreaRecord::new(Uuid::new_v4(), owner.id, None, name.to_string(), seq);
        let id = area.id;
        st.areas.insert(id, area);
        AreaId(id)
    }

    pub fn create_atlas(&self, owner: &TestUser, name: &str) -> Uuid {
        let mut st = self.state.lock();
        let id = Uuid::new_v4();
        st.atlases.insert(
            id,
            AtlasRecord {
                id,
                user_id: owner.id,
                clan_id: None,
                name: name.to_string(),
                created_at: Utc::now(),
                rev: 1,
            },
        );
        id
    }

    /// Create an area already filed in `atlas` (its `atlas_id` set), so the
    /// projection surfaces the denormalized `atlas_name` (§4.1).
    pub fn create_area_in_atlas(&self, owner: &TestUser, name: &str, atlas: Uuid) -> AreaId {
        let mut st = self.state.lock();
        let seq = st.next_seq();
        let area = AreaRecord::new(Uuid::new_v4(), owner.id, Some(atlas), name.to_string(), seq);
        let id = area.id;
        st.areas.insert(id, area);
        AreaId(id)
    }

    /// Direct state poke: add a room (no rev bump — test setup).
    pub fn add_room(&self, area: AreaId, room_number: i32, title: &str) {
        let mut st = self.state.lock();
        let area = st.areas.get_mut(&area.0).expect("area exists");
        area.rooms.insert(
            room_number,
            RoomRecord {
                identity: Uuid::new_v4(),
                title: title.to_string(),
                ..RoomRecord::placeholder(room_number)
            },
        );
    }

    /// Direct state poke: add an exit; `to` is `(area, room_number)`. The
    /// exit attaches to a Connection through the same server-style rules as
    /// the mutation endpoint (auto-pair the unique reciprocal one-member
    /// candidate, else a fresh one-member Connection), so seeded state
    /// always satisfies the v2 membership invariants.
    pub fn add_exit(
        &self,
        area: AreaId,
        from_room: i32,
        direction: &str,
        to: Option<(AreaId, i32)>,
    ) -> Uuid {
        let mut st = self.state.lock();
        let id = Uuid::new_v4();
        let connection_id = super::connections::attach_for_new_exit(
            &mut st.areas,
            area.0,
            &super::connections::NewExitLink {
                from_room,
                from_direction: direction.to_string(),
                to_area_id: to.map(|(a, _)| a.0),
                to_room_number: to.map(|(_, n)| n),
                to_direction: None,
                new_connection_id: None,
            },
        );
        let area = st.areas.get_mut(&area.0).expect("area exists");
        area.exits.push(ExitRecord {
            to_room_identity: None,
            id,
            from_room_number: from_room,
            from_direction: direction.to_string(),
            to_area_id: to.map(|(a, _)| a.0),
            to_room_number: to.map(|(_, n)| n),
            to_direction: None,
            path: String::new(),
            is_hidden: false,
            door: None,
            weight: 1.0,
            command: String::new(),
            connection_id,
            to_secret: None,
        });
        id
    }

    pub fn add_label(&self, area: AreaId, text: &str) -> Uuid {
        let mut st = self.state.lock();
        let id = Uuid::new_v4();
        let area = st.areas.get_mut(&area.0).expect("area exists");
        area.labels.push(LabelRecord {
            id,
            level: 0,
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 20.0,
            horizontal_alignment: "Center".to_string(),
            vertical_alignment: "Center".to_string(),
            text: text.to_string(),
            color: "black".to_string(),
            background_color: "white".to_string(),
            font_size: 12,
            font_weight: 400,
        });
        id
    }

    pub fn add_shape(&self, area: AreaId) -> Uuid {
        let mut st = self.state.lock();
        let id = Uuid::new_v4();
        let area = st.areas.get_mut(&area.0).expect("area exists");
        area.shapes.push(ShapeRecord {
            id,
            level: 0,
            x: 0.0,
            y: 0.0,
            width: 50.0,
            height: 50.0,
            background_color: Some("grey".to_string()),
            stroke_color: Some("transparent".to_string()),
            shape_type: "Rectangle".to_string(),
            border_radius: 0.0,
            stroke_width: 1.0,
        });
        id
    }

    /// Direct state poke: add an owner Secret to the map. Returns its id.
    pub fn add_secret(&self, area: AreaId, name: &str) -> Uuid {
        let mut st = self.state.lock();
        let id = Uuid::new_v4();
        st.areas
            .get_mut(&area.0)
            .expect("area exists")
            .secrets
            .push(super::state::SecretRecord::new(id, name.to_string()));
        id
    }

    /// Direct state poke: one of Secret `secret`'s own rooms.
    pub fn add_secret_room(&self, area: AreaId, secret: Uuid, room_number: i32, title: &str) {
        assert!(
            room_number < super::state::STAND_IN,
            "a Secret room number in range"
        );
        let mut st = self.state.lock();
        let secret = secret_mut(&mut st, area, secret);
        secret.rooms.insert(
            room_number,
            RoomRecord {
                identity: Uuid::new_v4(),
                title: title.to_string(),
                ..RoomRecord::placeholder(room_number)
            },
        );
    }

    /// Direct state poke: an exit Secret `secret` keeps, from `from` to
    /// `to`, attached to a Connection by the same rules as a write (a
    /// unique reciprocal one-member exit pairs). A map room it names must
    /// exist; the Secret's stand-in for it is made as needed.
    pub fn add_secret_exit(
        &self,
        area: AreaId,
        secret: Uuid,
        from: SecretPlace,
        direction: &str,
        to: Option<SecretPlace>,
    ) -> Uuid {
        let mut st = self.state.lock();
        let map = st.areas.get(&area.0).expect("area exists").clone();
        let index = map
            .secrets
            .iter()
            .position(|candidate| candidate.id == secret)
            .expect("Secret exists");
        let key = |place: SecretPlace| -> (Uuid, i32) {
            match place {
                SecretPlace::Map(room) => {
                    assert!(map.rooms.contains_key(&room), "map room {room} exists");
                    (area.0, super::state::stand_in(room).expect("in range"))
                }
                SecretPlace::Own(room) => (area.0, room),
                SecretPlace::Elsewhere(other, room) => (other.0, room),
            }
        };
        let (_, from_key) = key(from);
        let to = to.map(key);
        let mut working = super::connections::Working::new();
        let mut doc = super::secrets::secret_doc(&map, &map.secrets[index]);
        doc.rooms
            .entry(from_key)
            .or_insert_with(|| RoomRecord::placeholder(from_key));
        if let Some((to_area, to_key)) = to
            && to_area == area.0
        {
            doc.rooms
                .entry(to_key)
                .or_insert_with(|| RoomRecord::placeholder(to_key));
        }
        working.insert(area.0, doc);
        let connection_id = super::connections::attach_for_new_exit(
            &mut working,
            area.0,
            &super::connections::NewExitLink {
                from_room: from_key,
                from_direction: direction.to_string(),
                to_area_id: to.map(|(to_area, _)| to_area),
                to_room_number: to.map(|(_, to_key)| to_key),
                to_direction: None,
                new_connection_id: None,
            },
        );
        let id = Uuid::new_v4();
        let mut doc = working.remove(&area.0).expect("just inserted");
        doc.exits.push(ExitRecord {
            to_room_identity: None,
            id,
            from_room_number: from_key,
            from_direction: direction.to_string(),
            to_area_id: to.map(|(to_area, _)| to_area),
            to_room_number: to.map(|(_, to_key)| to_key),
            to_direction: None,
            path: String::new(),
            is_hidden: true,
            door: None,
            weight: 1.0,
            command: String::new(),
            connection_id,
            to_secret: None,
        });
        let secret = secret_mut(&mut st, area, secret);
        super::secrets::store_doc(secret, doc);
        id
    }

    /// Direct state poke: Secret `secret`'s own property `name` on `place`
    /// (one of its rooms, or its data for a map room).
    pub fn set_secret_room_property(
        &self,
        area: AreaId,
        secret: Uuid,
        place: SecretPlace,
        name: &str,
        value: &str,
    ) {
        let mut st = self.state.lock();
        let key = match place {
            SecretPlace::Map(room) => super::state::stand_in(room).expect("in range"),
            SecretPlace::Own(room) => room,
            SecretPlace::Elsewhere(..) => panic!("a Secret keeps data on its own map's rooms"),
        };
        let secret = secret_mut(&mut st, area, secret);
        secret
            .rooms
            .entry(key)
            .or_insert_with(|| RoomRecord::placeholder(key))
            .properties
            .insert(
                name.to_string(),
                RoomPropRecord {
                    value: value.to_string(),
                },
            );
    }

    /// Direct state poke: `grantor` shares Secret `secret` with `grantee`,
    /// with `read` and `actions`. Returns the grant id.
    pub fn grant_secret(
        &self,
        area: AreaId,
        secret: Uuid,
        grantor: &TestUser,
        grantee: &TestUser,
        actions: &[&'static str],
    ) -> Uuid {
        let mut st = self.state.lock();
        let id = Uuid::new_v4();
        secret_mut(&mut st, area, secret)
            .grants
            .push(super::state::SecretGrantRecord {
                id,
                grantor_id: grantor.id,
                grantee_id: grantee.id,
                actions: actions.iter().copied().collect(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            });
        id
    }

    /// Direct state poke: every grant of Secret `secret` to `grantee` goes.
    pub fn revoke_secret(&self, area: AreaId, secret: Uuid, grantee: &TestUser) {
        let mut st = self.state.lock();
        secret_mut(&mut st, area, secret)
            .grants
            .retain(|grant| grant.grantee_id != grantee.id);
    }

    /// A Secret's revision, as its readers see it.
    pub fn secret_rev(&self, area: AreaId, secret: Uuid) -> i64 {
        let mut st = self.state.lock();
        secret_mut(&mut st, area, secret).rev
    }

    pub fn set_area_property(&self, area: AreaId, name: &str, value: &str) {
        let mut st = self.state.lock();
        let area = st.areas.get_mut(&area.0).expect("area exists");
        area.properties.insert(
            name.to_string(),
            AreaPropRecord {
                value: value.to_string(),
                created_at: Utc::now(),
            },
        );
    }

    pub fn set_room_property(&self, area: AreaId, room_number: i32, name: &str, value: &str) {
        let mut st = self.state.lock();
        let area = st.areas.get_mut(&area.0).expect("area exists");
        let room = area.rooms.get_mut(&room_number).expect("room exists");
        room.properties.insert(
            name.to_string(),
            RoomPropRecord {
                value: value.to_string(),
            },
        );
    }

    /// Insert an Accepted friendship between the pair.
    pub fn befriend(&self, a: &TestUser, b: &TestUser) {
        let mut st = self.state.lock();
        st.friendships.push(FriendshipRecord {
            requester_id: a.id,
            addressee_id: b.id,
            status: FriendStatus::Accepted,
            created_at: Utc::now(),
            responded_at: Some(Utc::now()),
        });
    }

    pub fn block(&self, blocker: &TestUser, blocked: &TestUser) {
        let mut st = self.state.lock();
        st.blocks.push(BlockRecord {
            blocker_id: blocker.id,
            blocked_id: blocked.id,
            created_at: Utc::now(),
        });
    }

    /// Insert a ROOT grant (grantor = owner) directly. Returns the grant id.
    pub fn grant(
        &self,
        owner: &TestUser,
        grantee: &TestUser,
        scope: GrantScope,
        flags: GrantFlags,
    ) -> Uuid {
        self.grant_with_host_hints(owner, grantee, scope, flags, None)
    }

    /// Insert a ROOT grant directly, snapshotting `host_hints` (§4.2 advisory
    /// host strings). `None` mirrors a hint-less share. Returns the grant id.
    pub fn grant_with_host_hints(
        &self,
        owner: &TestUser,
        grantee: &TestUser,
        scope: GrantScope,
        flags: GrantFlags,
        host_hints: Option<Vec<String>>,
    ) -> Uuid {
        let mut st = self.state.lock();
        let id = Uuid::new_v4();
        let (area_id, atlas_id) = match scope {
            GrantScope::Area(a) => (Some(a.0), None),
            GrantScope::Atlas(a) => (None, Some(a)),
        };
        st.grants.push(GrantRecord {
            id,
            owner_id: owner.id,
            grantor_id: owner.id,
            grantee_id: grantee.id,
            area_id,
            atlas_id,
            can_edit: flags.can_edit,
            can_reshare: flags.can_reshare,
            can_copy: flags.can_copy,
            can_admin: flags.can_admin,
            host_hints,
            parent_grant_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        });
        id
    }

    /// Bump the map's revision (a generic "something changed" poke).
    pub fn bump_rev(&self, area: AreaId) {
        let mut st = self.state.lock();
        st.bump(Some(area.0));
    }

    /// Queue N compound-mutation responses to be dropped: each affected
    /// request is processed normally (committed, receipt stored) but the
    /// caller receives a 500 in place of the success body — a lost response
    /// on an applied mutation, for transport-retry/receipt-dedupe tests.
    pub fn drop_next_mutation_responses(&self, n: u32) {
        self.state.lock().drop_mutation_responses = n;
    }

    /// Queue N compound-mutation refusals: each affected request is rejected
    /// with a 400 before anything applies, a permanent verdict the client
    /// parks for review instead of retrying.
    pub fn refuse_next_mutations(&self, n: u32) {
        self.state.lock().refuse_mutations = n;
    }

    /// Runs `during` inside the next transfer acceptance, between its claim
    /// and its flip; `during` returning `true` stops the acceptance there.
    pub fn interrupt_next_acceptance(
        &self,
        during: fn(&mut super::state::MockState, &super::state::PendingTransferRecord) -> bool,
    ) {
        self.state.lock().interrupt_acceptance = Some(during);
    }

    /// Queue N area-delete failures: each affected `DELETE /areas/{id}`
    /// answers 500 and deletes nothing.
    pub fn fail_next_area_deletes(&self, n: u32) {
        self.state.lock().fail_area_deletes = n;
    }

    /// Queue N area-read failures: each `GET /areas/{id}` answers 500.
    /// Stop the next `n` account deletions right after the mark, with a 500.
    pub fn interrupt_next_account_deletions(&self, n: u32) {
        self.state.lock().interrupt_account_deletions = n;
    }

    /// Stop the next `n` clan dissolutions right after the clan is marked
    /// dissolving, with a 500; the clan stays dissolving until a repeat
    /// finishes it.
    pub fn interrupt_next_dissolutions(&self, n: u32) {
        self.state.lock().interrupt_dissolutions = n;
    }

    /// Lose the answer of the next `n` clan dissolutions after they commit,
    /// before the directory marks the clan dissolved.
    pub fn lose_next_dissolution_answers(&self, n: u32) {
        self.state.lock().lose_dissolution_answers = n;
    }

    /// Seals a map as a transfer's export does: its compound writes answer
    /// 503 `write_freeze` to its readers until [`Self::unseal`].
    pub fn seal(&self, area: AreaId) {
        self.state.lock().sealed_maps.insert(area.0);
    }

    pub fn unseal(&self, area: AreaId) {
        self.state.lock().sealed_maps.remove(&area.0);
    }

    /// How many requests reached `method path` so far.
    pub fn requests_to(&self, method: &str, path: &str) -> usize {
        self.state
            .lock()
            .http_requests
            .iter()
            .filter(|(m, p)| m == method && p == path)
            .count()
    }

    pub fn fail_next_area_reads(&self, n: u32) {
        self.state.lock().fail_area_reads = n;
    }

    /// Every mutation envelope the compound endpoint accepted, in arrival
    /// order: `(operation_id, replayed_from_receipt)`.
    pub fn mutation_requests(&self) -> Vec<(Uuid, bool)> {
        self.state.lock().mutation_log.clone()
    }

    /// The current revision of an area's map source.
    pub fn area_rev(&self, area: AreaId) -> i64 {
        let st = self.state.lock();
        st.areas.get(&area.0).expect("area exists").rev
    }

    /// Fish the latest UNCONSUMED one-time code for `email` out of state —
    /// the test-side stand-in for reading the code from the email.
    pub fn verify_code_for(&self, email: &str) -> Option<String> {
        let st = self.state.lock();
        let user_id = st.user_by_email(email)?.id;
        st.email_codes
            .iter()
            .rev()
            .find(|c| c.user_id == user_id && !c.consumed)
            .map(|c| c.code.clone())
    }
}
