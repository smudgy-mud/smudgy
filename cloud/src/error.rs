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
        }
    }
}

impl std::error::Error for CloudError {}

impl CloudError {
    /// Maps an HTTP error status plus the server's envelope `error` string to
    /// the client error taxonomy.
    #[must_use]
    pub fn from_status(status: u16, message: &str) -> Self {
        match status {
            401 => Self::Unauthorized(message.to_string()),
            403 if message.contains("email_not_verified") => Self::EmailNotVerified,
            403 => Self::PermissionDenied(message.to_string()),
            404 => Self::NotFoundOrNoAccess,
            // Package publish/delete conflicts carry a machine token in the message
            // (the envelope has no separate code field), mirrored from smudgy-api.
            409 if message.starts_with("version_unavailable") => {
                let version = message
                    .split_once(':')
                    .map(|(_, v)| v.trim().to_string())
                    .unwrap_or_default();
                Self::VersionUnavailable(version)
            }
            409 if message.contains("version_not_yanked") => Self::VersionNotYanked,
            409 => Self::NameUnavailable(message.to_string()),
            426 => Self::UpgradeRequired,
            _ => Self::NetworkError(format!("HTTP {status}: {message}")),
        }
    }

    /// True for errors that mean "the credential itself is bad" (prompt for
    /// login) rather than a per-resource denial.
    #[must_use]
    pub const fn is_auth_error(&self) -> bool {
        matches!(
            self,
            Self::Unauthorized(_) | Self::AuthenticationError(_)
        )
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
