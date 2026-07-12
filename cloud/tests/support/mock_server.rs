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
    API_KEY_PREFIX, ApiKeyRecord, AreaPropRecord, AreaRecord, AtlasRecord, BlockRecord,
    ExitRecord, FriendStatus, FriendshipRecord, GrantRecord, LabelRecord, MockState,
    RoomPropRecord, RoomRecord, SESSION_PREFIX, SessionRecord, ShapeRecord, UserRecord, gen_token,
};
use super::{areas, clone, identity, shares, social, transfers};

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

/// Capability flags for [`MockHandle::grant`].
#[derive(Debug, Clone, Copy, Default)]
pub struct GrantFlags {
    pub can_edit: bool,
    pub can_reshare: bool,
    pub can_copy: bool,
    pub include_secrets: bool,
    pub can_admin: bool,
}

impl GrantFlags {
    pub const VIEW_ONLY: Self = Self {
        can_edit: false,
        can_reshare: false,
        can_copy: false,
        include_secrets: false,
        can_admin: false,
    };

    #[must_use]
    pub const fn edit() -> Self {
        Self {
            can_edit: true,
            can_reshare: false,
            can_copy: false,
            include_secrets: false,
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
            include_secrets: false,
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
        // identity
        .route("/auth/signup", post(identity::signup))
        .route("/auth/login", post(identity::login))
        .route("/auth/verify-email", post(identity::verify_email))
        .route("/auth/logout", post(identity::logout))
        .route("/auth/refresh", post(identity::refresh_session))
        .route("/me", get(identity::get_me).patch(identity::patch_me))
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
        .route("/areas/:area_id/shares", get(shares::area_shares))
        .route("/areas/:area_id/secret-marks", post(clone::secret_marks))
        .route("/areas/:area_id/secrets", get(clone::list_secrets))
        .route("/areas/:area_id/preview", get(clone::preview_area))
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
        .route(
            "/areas/:area_id/exits/:exit_id",
            put(areas::update_exit).delete(areas::delete_exit),
        )
        .route(
            "/areas/:area_id/rooms/:room_number",
            delete(areas::delete_room),
        )
        .route(
            "/areas/:area_id/rooms/:room_number/properties/:name",
            put(areas::upsert_room_property).delete(areas::delete_room_property),
        )
        .route(
            "/areas/:area_id/rooms/:room_number/tags/:tag",
            put(areas::add_room_tag).delete(areas::remove_room_tag),
        )
        .route(
            "/areas/:area_id/rooms/:room_number/exits",
            post(areas::create_exit),
        )
        // NOTE the contract's bare room-upsert path: PUT /areas/{id}/{number}
        .route("/areas/:area_id/:room_number", put(areas::upsert_room))
        // atlas copy
        .route("/atlases/:atlas_id/copy", post(clone::copy_atlas))
        // ownership transfer
        .route("/areas/:area_id/transfer", post(transfers::create_area_transfer))
        .route("/atlases/:atlas_id/transfer", post(transfers::create_atlas_transfer))
        .route("/transfers", get(transfers::list_transfers))
        .route("/transfers/:transfer_id/accept", post(transfers::accept_transfer))
        .route("/transfers/:transfer_id/decline", post(transfers::decline_transfer))
        .route("/transfers/:transfer_id", delete(transfers::cancel_transfer))
        // Mirror the server's pre-routing version gate (see `function_handler`):
        // every request passes through it before any handler runs.
        .layer(middleware::from_fn_with_state(state.clone(), version_gate))
        .with_state(state)
}

/// Reject a too-old client with 426 before routing, mirroring the real
/// server's `enforce_client_version`. No-op unless a test raised the floor via
/// [`MockHandle::set_min_client_version`].
async fn version_gate(State(state): State<Shared>, request: Request, next: Next) -> Response {
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
        && let Ok(value) = HeaderValue::from_str(&newest) {
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
                name: name.to_string(),
                created_at: Utc::now(),
            },
        );
        id
    }

    /// Direct state poke: add a room (no rev bump — test setup).
    pub fn add_room(&self, area: AreaId, room_number: i32, title: &str, is_secret: bool) {
        let mut st = self.state.lock();
        let area = st.areas.get_mut(&area.0).expect("area exists");
        area.rooms.insert(
            room_number,
            RoomRecord {
                title: title.to_string(),
                is_secret,
                ..RoomRecord::placeholder(room_number)
            },
        );
    }

    /// Direct state poke: add an exit; `to` is `(area, room_number)`.
    pub fn add_exit(
        &self,
        area: AreaId,
        from_room: i32,
        direction: &str,
        to: Option<(AreaId, i32)>,
        is_secret: bool,
    ) -> Uuid {
        let mut st = self.state.lock();
        let id = Uuid::new_v4();
        let area = st.areas.get_mut(&area.0).expect("area exists");
        area.exits.push(ExitRecord {
            id,
            from_room_number: from_room,
            from_direction: direction.to_string(),
            to_area_id: to.map(|(a, _)| a.0),
            to_room_number: to.map(|(_, n)| n),
            to_direction: None,
            path: String::new(),
            is_hidden: false,
            is_closed: false,
            is_locked: false,
            weight: 1.0,
            command: String::new(),
            style: "Normal".to_string(),
            color: String::new(),
            is_secret,
        });
        id
    }

    pub fn add_label(&self, area: AreaId, text: &str, is_secret: bool) -> Uuid {
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
            is_secret,
        });
        id
    }

    pub fn add_shape(&self, area: AreaId, is_secret: bool) -> Uuid {
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
            is_secret,
        });
        id
    }

    pub fn set_area_property(&self, area: AreaId, name: &str, value: &str, is_secret: bool) {
        let mut st = self.state.lock();
        let area = st.areas.get_mut(&area.0).expect("area exists");
        area.properties.insert(
            name.to_string(),
            AreaPropRecord {
                value: value.to_string(),
                is_secret,
                created_at: Utc::now(),
            },
        );
    }

    pub fn set_room_property(
        &self,
        area: AreaId,
        room_number: i32,
        name: &str,
        value: &str,
        is_secret: bool,
    ) {
        let mut st = self.state.lock();
        let area = st.areas.get_mut(&area.0).expect("area exists");
        let room = area.rooms.get_mut(&room_number).expect("room exists");
        room.properties.insert(
            name.to_string(),
            RoomPropRecord {
                value: value.to_string(),
                is_secret,
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
            include_secrets: flags.include_secrets,
            can_admin: flags.can_admin,
            parent_grant_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        });
        id
    }

    /// Bump both revs (a generic "something changed" poke).
    pub fn bump_rev(&self, area: AreaId) {
        let mut st = self.state.lock();
        st.bump(Some(area.0), true, false);
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
