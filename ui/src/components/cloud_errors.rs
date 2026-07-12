//! Shared mapping from [`CloudError`] to user-facing strings for the cloud /
//! account / social UI surfaces.

use smudgy_cloud::CloudError;

/// Renders a [`CloudError`] as a short, user-facing message.
pub fn display_error(err: &CloudError) -> String {
    match err {
        CloudError::Unauthorized(msg) if !msg.is_empty() => msg.clone(),
        CloudError::EmailNotVerified => "Verify your email to use this feature.".to_string(),
        CloudError::NotFoundOrNoAccess => "Not found.".to_string(),
        other => other.to_string(),
    }
}
