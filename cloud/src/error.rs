use std::fmt;

use crate::{AreaId, ExitId, LabelId, ShapeId, mapper::RoomKey};

/// Result type alias for map operations
pub type CloudResult<T> = Result<T, CloudError>;

/// Error types for map operations
#[derive(Debug, Clone)]
pub enum CloudError {
    /// Area not found
    AreaNotFound(AreaId),

    /// Room not found
    RoomNotFound(RoomKey),

    /// Exit not found
    ExitNotFound(ExitId),

    /// Label not found
    LabelNotFound(LabelId),

    /// Shape not found
    ShapeNotFound(ShapeId),

    /// Property not found
    PropertyNotFound {
        entity_type: String,
        entity_id: String,
        property_name: String,
    },

    /// Invalid input data
    InvalidInput(String),

    /// Database error
    DatabaseError(String),

    /// Network/HTTP error
    NetworkError(String),

    /// 503 — the service holds writes for a moment: a transfer's seal on
    /// the subject (`write_freeze`), or a hold on every write. A transport
    /// failure that retrying outlasts, so queued writes wait for it without
    /// spending their attempts.
    ServiceUnavailable(String),

    /// Serialization error
    SerializationError(String),

    /// Authentication error
    AuthenticationError(String),

    /// Permission denied
    PermissionDenied(String),

    /// Internal error
    InternalError(String),

    /// A durable local decision needs recovery. This is not a rolled-back write.
    LocalCommitPending { message: String, generation: u64 },

    /// `PendingOperations`
    PendingOperations(String),

    /// 401 — missing or invalid credential; the user must (re-)authenticate.
    Unauthorized(String),

    /// 403 `email_not_verified` — the account exists but cloud/social
    /// features are gated until the email is verified.
    EmailNotVerified,

    /// Uniform 404 — nonexistent *or* no access; the server never
    /// distinguishes, and neither may the UI.
    NotFoundOrNoAccess,

    /// 409 — room number or property name unavailable (secret collision or
    /// genuine conflict); surface inline as "name in use".
    NameUnavailable(String),

    /// 426 `client_upgrade_required` — this client is older than the server's
    /// minimum supported version. Terminal: no retry helps; the user must
    /// download a newer smudgy, so the UI opens the download page.
    UpgradeRequired,

    /// 409 `version_unavailable` — the publish target version number is already
    /// taken (live, yanked, or previously published then deleted) and can never
    /// be reused. Carries the offending version string. The remedy is to bump to
    /// a new number, so the publish UI surfaces this distinctly from a generic
    /// name collision.
    VersionUnavailable(String),

    /// 409 `version_not_yanked` — a version must be yanked before it can be
    /// deleted (delete is the heavy, two-step action). Yank it first.
    VersionNotYanked,

    /// 409 `package_name_unavailable` — another package holds this name, or held
    /// it once: package names are global and reserved forever, so the remedy is
    /// another name. Carries the name.
    PackageNameUnavailable(String),

    /// 409 `body_being_collected` on a bundle upload — garbage collection was
    /// deleting one of the publish's bodies, and the upload stored nothing. A
    /// publish begins again once; this surfaces when that retry met it too.
    BodyBeingCollected,

    /// 413 — the request is over one of the server's size caps (a package's
    /// manifest, README, module, version, or publish bundle). Carries the
    /// server's message naming the cap. Permanent: retrying the same request
    /// cannot succeed.
    TooLarge(String),

    /// 409 `revision_conflict` — the aggregate moved past the mutation's
    /// precondition. Carries what the caller expected and where the server's
    /// projection of the aggregate now stands; the pending queue refetches
    /// and re-validates before resending.
    RevisionConflict {
        id: uuid::Uuid,
        expected_rev: i64,
        current_rev: i64,
    },

    /// 409 `projection_changed` — the caller's capabilities on the aggregate
    /// changed (access fingerprint mismatch), so their whole projection may
    /// differ. Requires an authorization-aware refetch before any rebase,
    /// even if the numeric revision happens to match.
    ProjectionChanged { access_fingerprint: String },

    /// 409 `operation_id_reused` — this operation id was already accepted
    /// with a different request body. A client bug or id collision; never
    /// retried automatically.
    OperationIdReused,

    /// 409 `structural_conflict` — the revision matched but the requested
    /// link topology is no longer valid (normally only possible in a
    /// compound operation). Also used for local merge refusals. Carries a
    /// stable reason string; presentation must distinguish argument refusals
    /// from concurrent changes.
    StructuralConflict(String),

    /// 422 `invalid_connection` — a Connection payload failed validation.
    /// Carries the stable reason code (`too_many_members`, `wrong_area`,
    /// `invalid_endpoint`, `non_orthogonal`, `invalid_point`, …).
    InvalidConnection(String),

    /// Internal dispatch guard: the authenticated credential changed after
    /// this cloud operation was queued. The viewer-scoped journal retains it
    /// until identity is resolved again.
    CredentialChanged,

    /// A room merge would delete map room `0`, on which a Secret (or the
    /// caller's Private additions) keeps data or which one of its exits
    /// leads to: deleting the room deletes every source's hold on it.
    SecretKeepsRoomData(crate::RoomNumber),

    /// 409 `last_owner` — the clan's last owner tried to leave or give up
    /// ownership. A clan always has an owner: make someone else one first.
    LastOwner,

    /// 409 `clan_not_empty` — a clan that still owns maps (in its folders)
    /// cannot be dissolved.
    ClanNotEmpty,

    /// 409 `clan_dissolving` — the clan is being dissolved: no map enters it
    /// (creating a map there, accepting a transfer into it, or accepting an
    /// offer to make a map Clan-owned), and its membership and its
    /// Member-owned maps hold still (demoting, removing or an owner leaving;
    /// writing, moving, renaming, refiling or deleting such a map, or its
    /// Secrets; accepting an ownership offer on it). Promotions still go
    /// through.
    ClanDissolving,

    /// 409 `atlas_not_empty` — a clan folder that still holds the clan's maps
    /// cannot be deleted.
    AtlasNotEmpty,

    /// 409 `already_member` — the invited user is already a member.
    AlreadyMember,

    /// 409 `name_in_use` — the clan already has a group with that name,
    /// ignoring case.
    NameInUse,

    /// 409 `transfer_already_pending` — the map or folder already has an
    /// offer waiting: it has one live offer at a time.
    TransferAlreadyPending,
}

impl fmt::Display for CloudError {
    #[allow(clippy::too_many_lines)] // one arm per variant
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Self::InvalidInput(reason) | Self::StructuralConflict(reason) = self
            && let Some(message) = refusal_message(reason)
        {
            // Scripts use these stable codes to identify refusals.
            return write!(f, "{message} ({reason})");
        }
        match self {
            CloudError::LocalCommitPending { message, .. } => {
                write!(f, "Local commit pending recovery: {message}")
            }
            CloudError::AreaNotFound(id) => write!(f, "Area not found: {id}"),
            CloudError::RoomNotFound(room_key) => {
                write!(
                    f,
                    "Room {} not found in area {}",
                    room_key.room_number, room_key.area_id
                )
            }
            CloudError::ExitNotFound(id) => write!(f, "Exit not found: {id}"),
            CloudError::LabelNotFound(id) => write!(f, "Label not found: {id}"),
            CloudError::ShapeNotFound(id) => write!(f, "Shape not found: {id}"),
            CloudError::PropertyNotFound {
                entity_type,
                entity_id,
                property_name,
            } => {
                write!(
                    f,
                    "Property '{property_name}' not found on {entity_type} {entity_id}"
                )
            }
            CloudError::InvalidInput(msg) => write!(f, "Invalid input: {msg}"),
            CloudError::DatabaseError(msg) => write!(f, "Database error: {msg}"),
            CloudError::NetworkError(msg) => write!(f, "Network error: {msg}"),
            CloudError::ServiceUnavailable(msg) => {
                write!(f, "The map service is briefly holding changes: {msg}")
            }
            CloudError::SerializationError(msg) => write!(f, "Serialization error: {msg}"),
            CloudError::AuthenticationError(msg) => write!(f, "Authentication error: {msg}"),
            CloudError::PermissionDenied(msg) => write!(f, "Permission denied: {msg}"),
            CloudError::InternalError(msg) => write!(f, "Internal error: {msg}"),
            CloudError::PendingOperations(msg) => write!(f, "Pending operations: {msg}"),
            CloudError::Unauthorized(msg) => write!(f, "Not signed in: {msg}"),
            CloudError::EmailNotVerified => {
                write!(f, "Verify your email to use cloud features")
            }
            CloudError::NotFoundOrNoAccess => write!(f, "Not found or no access"),
            CloudError::NameUnavailable(msg) => write!(f, "Name unavailable: {msg}"),
            CloudError::UpgradeRequired => {
                write!(f, "This version of smudgy is out of date; please update")
            }
            CloudError::VersionUnavailable(version) => {
                if version.is_empty() {
                    write!(f, "That version number is already taken; choose a new one")
                } else {
                    write!(f, "Version {version} is already taken; choose a new one")
                }
            }
            CloudError::VersionNotYanked => write!(f, "Yank this version before deleting it"),
            CloudError::PackageNameUnavailable(name) => write!(
                f,
                "The package name {name} is taken; choose another. (package_name_unavailable)"
            ),
            CloudError::BodyBeingCollected => write!(
                f,
                "The server was tidying package storage; publish again in a minute. (body_being_collected)"
            ),
            CloudError::TooLarge(msg) => write!(f, "Too large to upload: {msg}"),
            CloudError::RevisionConflict {
                expected_rev,
                current_rev,
                ..
            } => write!(
                f,
                "Someone else changed this map (expected rev {expected_rev}, now {current_rev})"
            ),
            CloudError::ProjectionChanged { .. } => {
                write!(f, "Your access to this map changed; refreshing")
            }
            CloudError::OperationIdReused => {
                write!(
                    f,
                    "This operation id was already used for a different change"
                )
            }
            CloudError::StructuralConflict(reason) => {
                write!(
                    f,
                    "The map's structure changed underneath this edit: {reason}"
                )
            }
            CloudError::InvalidConnection(reason) => {
                write!(f, "Invalid connection: {reason}")
            }
            CloudError::CredentialChanged => {
                write!(f, "Map credential changed before the edit was sent")
            }
            CloudError::SecretKeepsRoomData(room) => write!(
                f,
                "A Secret keeps data for room {room}. Move or remove it first. \
                 (merge_secret_room_data)"
            ),
            CloudError::LastOwner => write!(f, "Make someone else an owner first. (last_owner)"),
            CloudError::ClanNotEmpty => {
                write!(
                    f,
                    "Move or delete the maps in its folders first. (clan_not_empty)"
                )
            }
            CloudError::ClanDissolving => {
                write!(f, "This clan is being dissolved. (clan_dissolving)")
            }
            CloudError::AtlasNotEmpty => {
                write!(f, "Move or delete its maps first. (atlas_not_empty)")
            }
            CloudError::AlreadyMember => write!(f, "Already a member. (already_member)"),
            CloudError::NameInUse => write!(f, "Name already in use. (name_in_use)"),
            CloudError::TransferAlreadyPending => write!(
                f,
                "It already has an offer waiting. (transfer_already_pending)"
            ),
        }
    }
}

impl std::error::Error for CloudError {}

fn refusal_message(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "merge_areas_no_sources" => "Choose at least one source area to merge.",
        "merge_areas_same_area" => {
            "Each source area must appear once and must differ from the destination."
        }
        "merge_areas_no_rooms" => {
            "Select at least one room from each partial source, or omit the room selection to merge the whole area."
        }
        "merge_areas_room_not_found" => {
            "A selected room is missing from its source area. Check the room numbers before merging."
        }
        "merge_areas_mixed_tiers" => {
            "All affected areas, including areas with links into the merge, must use the same storage: local or session."
        }
        "merge_areas_unsupported_storage" => {
            "This storage does not support area merges. Use supported local or session storage."
        }
        "merge_areas_busy" => {
            "An affected area has pending edits or another operation in progress. Finish or resolve those operations before merging."
        }
        "merge_areas_source_changed" => {
            "An affected area changed while the merge was being prepared. Review the current map and try again."
        }
        "merge_areas_room_numbers_exhausted" => {
            "The destination does not have enough available room numbers for this merge. Choose another destination."
        }
        "merge_areas_invalid_translation" => {
            "The translation or a resulting position is outside the supported range. Use finite coordinates and keep levels within the 32-bit integer range."
        }
        "merge_cross_area_links" => {
            "The room being merged away has links to or from other areas. Remove those links, then merge the rooms."
        }
        "move_busy" => {
            "The map has pending edits or another operation in progress. Wait for them to finish, then move again."
        }
        "move_drops_places" => {
            "Session maps can't hold Secrets or Private additions, so moving this map there would lose them. Copy it instead; the original keeps them."
        }
        "move_splits_links" => {
            "An exit or connection joins content being moved to content staying behind. Move both ends together."
        }
        "room_number_exists" => "A room with that number already exists there.",
        "secret_area_level" => "A Secret is managed in the map editor.",
        "secret_view_only" => "This Secret is view only.",
        "secret_unavailable" => "That Secret is no longer available to you.",
        "secret_cannot_add" => "You can't add to this Secret.",
        "secret_cannot_edit" => "You can't change this Secret's content.",
        "secret_cannot_remove" => "You can't remove anything from this Secret.",
        "secret_linked_map_rooms" => {
            "A clan's Secret on a map filed in the clan by link keeps only its own rooms. It can't hold data on the map's rooms, lead into them, or trade rooms with the map."
        }
        "secret_link_between_secrets" => "A link can't join two Secrets.",
        "secret_link_into_other_map" => {
            "A link into another map's Secret leads to one of its rooms, and no link leads into another map's Private additions."
        }
        "secret_map_exit_retarget" => {
            "A map exit can't lead into a Secret's room. Link from the Secret instead."
        }
        _ => return None,
    })
}

impl CloudError {
    /// Maps an HTTP error status plus the server's envelope `error` string to
    /// the client error taxonomy. Responses that may carry a structured
    /// `details` object (the CAS conflicts) go through [`Self::from_response`].
    #[must_use]
    pub fn from_status(status: u16, message: &str) -> Self {
        Self::from_response(status, message, None)
    }

    /// Full response mapping: status, envelope `error` code/message, and the
    /// optional structured `details` object. Each 409 keeps its own variant —
    /// callers branch on the specific conflict, never on a collapsed bucket.
    #[must_use]
    pub fn from_response(status: u16, message: &str, details: Option<&serde_json::Value>) -> Self {
        match (status, message) {
            // A 400 is a permanent contract verdict (malformed envelope,
            // size bounds, missing precondition) — never a transport
            // failure, so it must not enter the retry/backoff path.
            (400, _) => Self::InvalidInput(message.to_string()),
            (401, _) => Self::Unauthorized(message.to_string()),
            (403, m) if m.contains("email_not_verified") => Self::EmailNotVerified,
            (403, _) => Self::PermissionDenied(message.to_string()),
            (404, _) => Self::NotFoundOrNoAccess,
            (409, "revision_conflict") => {
                let d = details.unwrap_or(&serde_json::Value::Null);
                Self::RevisionConflict {
                    id: d
                        .get("id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| uuid::Uuid::parse_str(s).ok())
                        .unwrap_or_default(),
                    expected_rev: d
                        .get("expected_rev")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0),
                    current_rev: d
                        .get("current_rev")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0),
                }
            }
            (409, "projection_changed") => Self::ProjectionChanged {
                access_fingerprint: details
                    .and_then(|d| d.get("access_fingerprint"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            },
            (409, "operation_id_reused") => Self::OperationIdReused,
            (409, "last_owner") => Self::LastOwner,
            (409, "clan_not_empty") => Self::ClanNotEmpty,
            (409, "clan_dissolving") => Self::ClanDissolving,
            (409, "atlas_not_empty") => Self::AtlasNotEmpty,
            (409, "already_member") => Self::AlreadyMember,
            (409, "name_in_use") => Self::NameInUse,
            (409, "transfer_already_pending") => Self::TransferAlreadyPending,
            (409, "structural_conflict") => Self::StructuralConflict(
                details
                    .and_then(|d| d.get("reason"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            ),
            (422, "invalid_connection") => Self::InvalidConnection(
                details
                    .and_then(|d| d.get("reason"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            ),
            // Package publish/delete conflicts carry a machine token in the message
            // (the envelope has no separate code field), mirrored from smudgy-api.
            (409, m) if m.starts_with("version_unavailable") => {
                let version = m
                    .split_once(':')
                    .map(|(_, v)| v.trim().to_string())
                    .unwrap_or_default();
                Self::VersionUnavailable(version)
            }
            (409, m) if m.contains("version_not_yanked") => Self::VersionNotYanked,
            (409, m) if m.starts_with("package_name_unavailable") => Self::PackageNameUnavailable(
                m.split_once(':')
                    .map(|(_, name)| name.trim().to_string())
                    .unwrap_or_default(),
            ),
            (409, m) if m.starts_with("body_being_collected") => Self::BodyBeingCollected,
            (409, _) => Self::NameUnavailable(message.to_string()),
            (413, _) => Self::TooLarge(message.to_string()),
            (426, _) => Self::UpgradeRequired,
            (503, _) => Self::ServiceUnavailable(message.to_string()),
            _ => Self::NetworkError(format!("HTTP {status}: {message}")),
        }
    }

    /// True for errors that mean "the credential itself is bad" (prompt for
    /// login) rather than a per-resource denial.
    #[must_use]
    pub const fn is_auth_error(&self) -> bool {
        matches!(self, Self::Unauthorized(_) | Self::AuthenticationError(_))
    }

    /// True for transient transport-level failures worth retrying/backing
    /// off (offline, DNS, timeouts) as opposed to server verdicts.
    #[must_use]
    pub const fn is_transport_error(&self) -> bool {
        matches!(self, Self::NetworkError(_) | Self::ServiceUnavailable(_))
    }

    /// True when the server rejected this client as too old (426). The only
    /// remedy is downloading a newer build, so callers surface the upgrade
    /// path (open the download page) rather than retrying.
    #[must_use]
    pub const fn is_upgrade_required(&self) -> bool {
        matches!(self, Self::UpgradeRequired)
    }
}

// Conversion from common error types
impl From<serde_json::Error> for CloudError {
    fn from(err: serde_json::Error) -> Self {
        CloudError::SerializationError(err.to_string())
    }
}

impl From<reqwest::Error> for CloudError {
    fn from(err: reqwest::Error) -> Self {
        CloudError::NetworkError(err.to_string())
    }
}

impl From<std::io::Error> for CloudError {
    fn from(err: std::io::Error) -> Self {
        CloudError::InternalError(err.to_string())
    }
}

impl From<uuid::Error> for CloudError {
    fn from(err: uuid::Error) -> Self {
        CloudError::InvalidInput(format!("Invalid UUID: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_refusals_explain_the_reason_and_preserve_the_code() {
        for (code, guidance) in [
            ("merge_areas_no_sources", "at least one source"),
            ("merge_areas_same_area", "must appear once"),
            ("merge_areas_no_rooms", "at least one room"),
            ("merge_areas_room_not_found", "Check the room numbers"),
            ("merge_areas_mixed_tiers", "same storage"),
            ("merge_areas_unsupported_storage", "local or session"),
            ("merge_areas_busy", "pending edits"),
            ("merge_areas_source_changed", "Review the current map"),
            ("merge_areas_room_numbers_exhausted", "room numbers"),
            ("merge_areas_invalid_translation", "supported range"),
            ("merge_cross_area_links", "other areas"),
            ("move_busy", "pending edits"),
            ("move_drops_places", "Copy it instead"),
            ("move_splits_links", "Move both ends together"),
            ("room_number_exists", "already exists"),
            ("secret_area_level", "map editor"),
            ("secret_view_only", "view only"),
            ("secret_unavailable", "no longer available"),
            ("secret_cannot_add", "add to this Secret"),
            ("secret_cannot_edit", "change this Secret"),
            ("secret_cannot_remove", "remove anything"),
            ("secret_linked_map_rooms", "only its own rooms"),
            ("secret_link_between_secrets", "two Secrets"),
            (
                "secret_link_into_other_map",
                "another map's Private additions",
            ),
            ("secret_map_exit_retarget", "Link from the Secret"),
        ] {
            let error = if code == "merge_areas_invalid_translation" {
                CloudError::InvalidInput(code.to_string())
            } else {
                CloudError::StructuralConflict(code.to_string())
            };
            let message = error.to_string();
            assert!(message.contains(guidance), "{code}: {message}");
            assert!(message.ends_with(&format!("({code})")), "{message}");
            assert!(
                !message.contains("structure changed underneath"),
                "{message}"
            );
        }
    }

    #[test]
    fn clan_conflicts_keep_their_own_variants() {
        for (code, expected) in [
            ("last_owner", CloudError::LastOwner),
            ("clan_not_empty", CloudError::ClanNotEmpty),
            ("clan_dissolving", CloudError::ClanDissolving),
            ("atlas_not_empty", CloudError::AtlasNotEmpty),
            ("already_member", CloudError::AlreadyMember),
            ("name_in_use", CloudError::NameInUse),
            (
                "transfer_already_pending",
                CloudError::TransferAlreadyPending,
            ),
        ] {
            let error = CloudError::from_status(409, code);
            assert_eq!(
                std::mem::discriminant(&error),
                std::mem::discriminant(&expected),
                "{code}"
            );
            assert!(error.to_string().ends_with(&format!("({code})")), "{error}");
        }
        assert!(matches!(
            CloudError::from_status(409, "That nickname is taken; please choose another."),
            CloudError::NameUnavailable(_)
        ));
    }

    #[test]
    fn unrelated_and_unknown_refusals_keep_their_diagnostics() {
        for reason in ["link_target_missing", "merge_areas_future_reason"] {
            assert_eq!(
                CloudError::StructuralConflict(reason.to_string()).to_string(),
                format!("The map's structure changed underneath this edit: {reason}")
            );
        }
        assert_eq!(
            CloudError::InvalidInput("invalid UUID".to_string()).to_string(),
            "Invalid input: invalid UUID"
        );
    }

    #[test]
    fn package_conflicts_keep_their_own_variants() {
        let error = CloudError::from_status(409, "package_name_unavailable: My-Lib");
        assert!(
            matches!(&error, CloudError::PackageNameUnavailable(name) if name == "My-Lib"),
            "{error:?}"
        );
        assert!(error.to_string().contains("My-Lib"), "{error}");
        for message in ["body_being_collected", "body_being_collected: 0123abcd"] {
            assert!(
                matches!(
                    CloudError::from_status(409, message),
                    CloudError::BodyBeingCollected
                ),
                "{message}"
            );
        }
        assert!(matches!(
            CloudError::from_status(409, "version_unavailable: 1.0.0"),
            CloudError::VersionUnavailable(version) if version == "1.0.0"
        ));
    }

    #[test]
    fn a_write_freeze_is_a_transport_failure_never_a_sign_out() {
        let error = CloudError::from_status(503, "Service temporarily unavailable");
        assert!(
            matches!(&error, CloudError::ServiceUnavailable(_)),
            "{error:?}"
        );
        assert!(error.is_transport_error(), "retried as a network failure");
        assert!(!error.is_auth_error());
        assert!(!error.is_upgrade_required());
    }

    #[test]
    fn a_size_cap_is_a_permanent_verdict_that_keeps_the_servers_message() {
        let message = "bundle too large: 95000001 bytes (max 95000000)";
        let error = CloudError::from_status(413, message);
        assert!(
            matches!(&error, CloudError::TooLarge(kept) if kept == message),
            "{error:?}"
        );
        assert!(
            !error.is_transport_error(),
            "retrying cannot shrink the request"
        );
        assert!(error.to_string().contains("95000000"), "{error}");
    }
}
