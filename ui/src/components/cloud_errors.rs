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
        CloudError::ServiceUnavailable(_) => crate::i18n::t!("cloud-error-service-unavailable"),
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
        CloudError::PackageNameUnavailable(name) => {
            crate::i18n::t!("cloud-error-package-name-unavailable", "name" => name)
        }
        CloudError::BodyBeingCollected => crate::i18n::t!("cloud-error-body-being-collected"),
        CloudError::TooLarge(detail) => {
            crate::i18n::t!("cloud-error-too-large", "detail" => detail)
        }
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
        CloudError::SecretKeepsRoomData(room) => {
            crate::i18n::t!("cloud-error-merge-secret-room-data", "room" => room.to_string())
        }
        CloudError::LastOwner => crate::i18n::t!("cloud-error-last-owner"),
        CloudError::ClanNotEmpty => crate::i18n::t!("cloud-error-clan-not-empty"),
        CloudError::ClanDissolving => crate::i18n::t!("cloud-error-clan-dissolving"),
        CloudError::AtlasNotEmpty => crate::i18n::t!("cloud-error-atlas-not-empty"),
        CloudError::AlreadyMember => crate::i18n::t!("cloud-error-already-member"),
        CloudError::NameInUse => crate::i18n::t!("cloud-error-name-in-use"),
        CloudError::TransferAlreadyPending => {
            crate::i18n::t!("cloud-error-transfer-already-pending")
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
        "merge_areas_busy" => "cloud-error-merge-areas-busy",
        "merge_areas_source_changed" => "cloud-error-merge-areas-source-changed",
        "merge_areas_room_numbers_exhausted" => "cloud-error-merge-areas-room-numbers-exhausted",
        "merge_areas_invalid_translation" => "cloud-error-merge-areas-invalid-translation",
        "secret_area_level" => "cloud-error-secret-area-level",
        "secret_view_only" => "cloud-error-secret-view-only",
        "secret_unavailable" => "cloud-error-secret-unavailable",
        "secret_cannot_add" => "cloud-error-secret-cannot-add",
        "secret_cannot_edit" => "cloud-error-secret-cannot-edit",
        "secret_cannot_remove" => "cloud-error-secret-cannot-remove",
        "secret_linked_map_rooms" => "cloud-error-secret-linked-map-rooms",
        "secret_link_between_secrets" => "cloud-error-secret-link-between-secrets",
        "secret_link_into_other_map" => "cloud-error-secret-link-into-other-map",
        "secret_map_exit_retarget" => "cloud-error-secret-map-exit-retarget",
        "move_busy" => "cloud-error-move-busy",
        "move_drops_places" => "cloud-error-move-drops-places",
        "move_splits_links" => "cloud-error-move-splits-links",
        "access_review_required" => "cloud-error-access-review-required",
        "stale_access_review" => "cloud-error-stale-access-review",
        "move_property_conflict" => "cloud-error-move-property-conflict",
        "room_number_exists" => "cloud-error-room-number-exists",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_review_refusals_give_localized_recovery_instructions() {
        for reason in [
            "access_review_required",
            "stale_access_review",
            "move_property_conflict",
        ] {
            let key = merge_refusal_key(reason).expect("a specific recovery message");
            assert!(
                !display_error(&CloudError::StructuralConflict(reason.into())).contains(reason)
            );
            for catalog in smudgy_i18n::available_catalogs() {
                let translator = smudgy_i18n::Translator::for_tag(catalog.tag).unwrap();
                let text = translator.translate(key);
                assert!(
                    !text.contains('⟦') && text != key,
                    "{}: {text}",
                    catalog.tag
                );
            }
        }
    }

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
            ("secret_area_level", "cloud-error-secret-area-level"),
            ("secret_view_only", "cloud-error-secret-view-only"),
            ("secret_unavailable", "cloud-error-secret-unavailable"),
            ("secret_cannot_add", "cloud-error-secret-cannot-add"),
            ("secret_cannot_edit", "cloud-error-secret-cannot-edit"),
            ("secret_cannot_remove", "cloud-error-secret-cannot-remove"),
            (
                "secret_linked_map_rooms",
                "cloud-error-secret-linked-map-rooms",
            ),
            (
                "secret_link_between_secrets",
                "cloud-error-secret-link-between-secrets",
            ),
            (
                "secret_link_into_other_map",
                "cloud-error-secret-link-into-other-map",
            ),
            (
                "secret_map_exit_retarget",
                "cloud-error-secret-map-exit-retarget",
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
    fn clan_conflicts_use_their_own_localized_messages() {
        for (error, key) in [
            (CloudError::LastOwner, "cloud-error-last-owner"),
            (CloudError::ClanNotEmpty, "cloud-error-clan-not-empty"),
            (CloudError::AtlasNotEmpty, "cloud-error-atlas-not-empty"),
            (CloudError::AlreadyMember, "cloud-error-already-member"),
            (CloudError::NameInUse, "cloud-error-name-in-use"),
            (
                CloudError::TransferAlreadyPending,
                "cloud-error-transfer-already-pending",
            ),
        ] {
            let rendered = display_error(&error);
            assert_eq!(rendered, crate::i18n::translate(key), "{key}");
            assert!(!rendered.contains('_'), "{rendered}");
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
    fn package_refusals_use_their_own_localized_messages() {
        let taken = display_error(&CloudError::PackageNameUnavailable("My-Lib".to_string()));
        assert!(taken.contains("My-Lib"), "{taken}");
        assert!(!taken.contains('_'), "{taken}");
        let collected = display_error(&CloudError::BodyBeingCollected);
        assert_eq!(
            collected,
            crate::i18n::translate("cloud-error-body-being-collected")
        );
        for catalog in smudgy_i18n::available_catalogs() {
            let translator = smudgy_i18n::Translator::for_tag(catalog.tag).unwrap();
            for key in [
                "cloud-error-body-being-collected",
                "cloud-error-clan-not-empty",
                "package-owner-clan",
            ] {
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
            let render = |translator: smudgy_i18n::Translator| smudgy_i18n::t!(translator, "cloud-error-package-name-unavailable", "name" => "My-Lib");
            let translated = render(translator);
            assert!(
                translated.contains("My-Lib") && !translated.contains('⟦'),
                "{}: {translated}",
                catalog.tag
            );
            let confirm = |translator: smudgy_i18n::Translator| smudgy_i18n::t!(translator, "clans-delete-clan-confirm", "name" => "Guild");
            let translated = confirm(smudgy_i18n::Translator::for_tag(catalog.tag).unwrap());
            assert!(
                translated.contains("Guild") && !translated.contains('⟦'),
                "{}: {translated}",
                catalog.tag
            );
        }
    }

    #[test]
    fn a_held_write_reads_as_a_moment_to_wait_in_every_language() {
        let error = CloudError::from_status(503, "Service temporarily unavailable");
        let shown = display_error(&error);
        assert_eq!(
            shown,
            crate::i18n::translate("cloud-error-service-unavailable")
        );
        assert!(!shown.contains("503"), "{shown}");
        let english =
            smudgy_i18n::Translator::default().translate("cloud-error-service-unavailable");
        for catalog in smudgy_i18n::available_catalogs() {
            let translated = smudgy_i18n::Translator::for_tag(catalog.tag)
                .unwrap()
                .translate("cloud-error-service-unavailable");
            assert!(
                !translated.is_empty() && !translated.contains('⟦'),
                "{}",
                catalog.tag
            );
            if catalog.tag != "en-US" {
                assert_ne!(translated, english, "{} must translate it", catalog.tag);
            }
        }
    }

    #[test]
    fn a_dissolving_clan_has_its_own_localized_message() {
        let error = CloudError::from_status(409, "clan_dissolving");
        assert_eq!(
            display_error(&error),
            crate::i18n::translate("cloud-error-clan-dissolving")
        );
        let english = smudgy_i18n::Translator::default().translate("cloud-error-clan-dissolving");
        assert!(english.contains("being dissolved"), "{english}");
        for catalog in smudgy_i18n::available_catalogs() {
            let translated = smudgy_i18n::Translator::for_tag(catalog.tag)
                .unwrap()
                .translate("cloud-error-clan-dissolving");
            assert!(
                !translated.is_empty() && !translated.contains('⟦'),
                "{}",
                catalog.tag
            );
            if catalog.tag != "en-US" {
                assert_ne!(translated, english, "{} must translate it", catalog.tag);
            }
        }
    }

    #[test]
    fn a_size_cap_is_localized_and_keeps_the_servers_detail() {
        let detail = "bundle too large: 95000001 bytes (max 95000000)";
        let rendered = display_error(&CloudError::TooLarge(detail.to_string()));
        assert!(rendered.contains(detail), "{rendered}");
        let render = |translator: smudgy_i18n::Translator| smudgy_i18n::t!(translator, "cloud-error-too-large", "detail" => detail);
        let english = render(smudgy_i18n::Translator::default());
        for catalog in smudgy_i18n::available_catalogs() {
            let translated = render(smudgy_i18n::Translator::for_tag(catalog.tag).unwrap());
            assert!(
                translated.contains(detail) && !translated.contains('⟦'),
                "{}: {translated}",
                catalog.tag
            );
            if catalog.tag != "en-US" {
                assert_ne!(translated, english, "{} must translate it", catalog.tag);
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
