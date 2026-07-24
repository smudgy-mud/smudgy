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

    /// Serialization error
    SerializationError(String),

    /// Authentication error
    AuthenticationError(String),

    /// Permission denied
    PermissionDenied(String),

    /// Internal error
    InternalError(String),

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
    /// compound operation). Carries the server's stable reason string.
    StructuralConflict(String),

    /// 422 `invalid_connection` — a Connection payload failed validation.
    /// Carries the stable reason code (`too_many_members`, `wrong_area`,
    /// `invalid_endpoint`, `non_orthogonal`, `invalid_point`, …).
    InvalidConnection(String),
}

impl fmt::Display for CloudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
        }
    }
}

impl std::error::Error for CloudError {}

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
            (409, _) => Self::NameUnavailable(message.to_string()),
            (426, _) => Self::UpgradeRequired,
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
        matches!(self, Self::NetworkError(_))
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
