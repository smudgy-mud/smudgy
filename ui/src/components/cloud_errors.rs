//! Shared mapping from [`CloudError`] to user-facing strings for the cloud /
//! account / social UI surfaces.

use smudgy_cloud::CloudError;

/// Renders a [`CloudError`] as a short, user-facing message.
pub fn display_error(err: &CloudError) -> String {
    if let CloudError::InvalidInput(reason) | CloudError::StructuralConflict(reason) = err
        && let Some(key) = merge_refusal_key(reason)
    {
        return crate::i18n::translate(key);
    }
    match err {
        CloudError::AreaNotFound(id) => {
            crate::i18n::t!("cloud-error-area-not-found", "id" => id.to_string())
        }
        CloudError::RoomNotFound(room) => crate::i18n::t!(
            "cloud-error-room-not-found",
            "room" => room.room_number.to_string(),
            "area" => room.area_id.to_string()
        ),
        CloudError::ExitNotFound(id) => {
            crate::i18n::t!("cloud-error-exit-not-found", "id" => id.to_string())
        }
        CloudError::LabelNotFound(id) => {
            crate::i18n::t!("cloud-error-label-not-found", "id" => id.to_string())
        }
        CloudError::ShapeNotFound(id) => {
            crate::i18n::t!("cloud-error-shape-not-found", "id" => id.to_string())
        }
        CloudError::PropertyNotFound {
            entity_type,
            entity_id,
            property_name,
        } => crate::i18n::t!(
            "cloud-error-property-not-found",
            "property" => property_name,
            "entity_type" => entity_type,
            "entity_id" => entity_id
        ),
        CloudError::InvalidInput(detail) => {
            crate::i18n::t!("cloud-error-invalid-input", "detail" => detail)
        }
        CloudError::DatabaseError(detail) => {
            crate::i18n::t!("cloud-error-database", "detail" => detail)
        }
        CloudError::NetworkError(detail) => {
            crate::i18n::t!("cloud-error-network", "detail" => detail)
        }
        CloudError::SerializationError(detail) => {
            crate::i18n::t!("cloud-error-serialization", "detail" => detail)
        }
        CloudError::AuthenticationError(detail) => {
            crate::i18n::t!("cloud-error-authentication", "detail" => detail)
        }
        CloudError::CredentialChanged => crate::i18n::t!("cloud-error-unauthorized"),
        CloudError::PermissionDenied(detail) => {
            crate::i18n::t!("cloud-error-permission", "detail" => detail)
        }
        CloudError::InternalError(detail) => {
            crate::i18n::t!("cloud-error-internal", "detail" => detail)
        }
        CloudError::LocalCommitPending { .. } => {
            crate::i18n::t!("cloud-error-local-commit-pending")
        }
        CloudError::PendingOperations(detail) => {
            crate::i18n::t!("cloud-error-pending", "detail" => detail)
        }
        // Authentication failures often carry an internal English diagnostic
        // such as `no credential configured`.  That detail is useful in logs,
        // but it is neither actionable nor suitable for a localized UI.
        CloudError::Unauthorized(_) => crate::i18n::t!("cloud-error-unauthorized"),
        CloudError::EmailNotVerified => crate::i18n::t!("cloud-error-email-unverified"),
        CloudError::NotFoundOrNoAccess => crate::i18n::t!("cloud-error-not-found"),
        CloudError::NameUnavailable(detail) => {
            crate::i18n::t!("cloud-error-name-unavailable", "detail" => detail)
        }
        CloudError::UpgradeRequired => crate::i18n::t!("cloud-error-upgrade-required"),
        CloudError::VersionUnavailable(version) if version.is_empty() => {
            crate::i18n::t!("cloud-error-version-unavailable")
        }
        CloudError::VersionUnavailable(version) => crate::i18n::t!(
            "cloud-error-version-unavailable-number",
            "version" => version
        ),
        CloudError::VersionNotYanked => crate::i18n::t!("cloud-error-version-not-yanked"),
        CloudError::RevisionConflict { .. } => {
            crate::i18n::t!("cloud-error-revision-conflict")
        }
        CloudError::ProjectionChanged { .. } => {
            crate::i18n::t!("cloud-error-projection-changed")
        }
        CloudError::OperationIdReused => crate::i18n::t!("cloud-error-operation-reused"),
        CloudError::StructuralConflict(detail) => {
            crate::i18n::t!("cloud-error-structural-conflict", "detail" => detail)
        }
        CloudError::InvalidConnection(detail) => {
            crate::i18n::t!("cloud-error-invalid-connection", "detail" => detail)
        }
    }
}

fn merge_refusal_key(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "merge_areas_no_sources" => "cloud-error-merge-areas-no-sources",
        "merge_areas_same_area" => "cloud-error-merge-areas-same-area",
        "merge_areas_no_rooms" => "cloud-error-merge-areas-no-rooms",
        "merge_areas_room_not_found" => "cloud-error-merge-areas-room-not-found",
        "merge_areas_mixed_tiers" => "cloud-error-merge-areas-mixed-tiers",
        "merge_areas_unsupported_storage" => "cloud-error-merge-areas-unsupported-storage",
        "merge_requires_full_projection" => "cloud-error-merge-requires-full-projection",
        "merge_areas_busy" => "cloud-error-merge-areas-busy",
        "merge_areas_source_changed" => "cloud-error-merge-areas-source-changed",
        "merge_areas_room_numbers_exhausted" => "cloud-error-merge-areas-room-numbers-exhausted",
        "merge_areas_invalid_translation" => "cloud-error-merge-areas-invalid-translation",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_refusals_use_specific_localized_messages() {
        for (code, key) in [
            (
                "merge_areas_no_sources",
                "cloud-error-merge-areas-no-sources",
            ),
            ("merge_areas_same_area", "cloud-error-merge-areas-same-area"),
            ("merge_areas_no_rooms", "cloud-error-merge-areas-no-rooms"),
            (
                "merge_areas_room_not_found",
                "cloud-error-merge-areas-room-not-found",
            ),
            (
                "merge_areas_mixed_tiers",
                "cloud-error-merge-areas-mixed-tiers",
            ),
            (
                "merge_areas_unsupported_storage",
                "cloud-error-merge-areas-unsupported-storage",
            ),
            (
                "merge_requires_full_projection",
                "cloud-error-merge-requires-full-projection",
            ),
            ("merge_areas_busy", "cloud-error-merge-areas-busy"),
            (
                "merge_areas_source_changed",
                "cloud-error-merge-areas-source-changed",
            ),
            (
                "merge_areas_room_numbers_exhausted",
                "cloud-error-merge-areas-room-numbers-exhausted",
            ),
            (
                "merge_areas_invalid_translation",
                "cloud-error-merge-areas-invalid-translation",
            ),
        ] {
            let error = if code == "merge_areas_invalid_translation" {
                CloudError::InvalidInput(code.to_string())
            } else {
                CloudError::StructuralConflict(code.to_string())
            };
            let rendered = display_error(&error);
            assert_eq!(rendered, crate::i18n::translate(key), "{code}");
            assert!(!rendered.contains(code), "{rendered}");
            for catalog in smudgy_i18n::available_catalogs() {
                let translator = smudgy_i18n::Translator::for_tag(catalog.tag).unwrap();
                let translated = translator.translate(key);
                assert!(!translated.is_empty() && !translated.contains('⟦'), "{key}");
                if catalog.tag != "en-US" {
                    assert_ne!(
                        translated,
                        smudgy_i18n::Translator::default().translate(key),
                        "{} must translate {key}",
                        catalog.tag
                    );
                }
            }
        }
    }

    #[test]
    fn unknown_merge_refusals_keep_their_details() {
        let rendered = display_error(&CloudError::StructuralConflict(
            "merge_areas_future_reason".to_string(),
        ));
        assert!(rendered.contains("merge_areas_future_reason"));
    }

    #[test]
    fn unauthorized_errors_do_not_leak_internal_diagnostics() {
        let rendered = display_error(&CloudError::Unauthorized(
            "no credential configured".to_string(),
        ));

        assert!(!rendered.contains("credential"));
        assert!(!rendered.contains("configured"));
    }
}
