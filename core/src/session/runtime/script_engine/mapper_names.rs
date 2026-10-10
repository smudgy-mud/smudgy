//! The mapper script API's names are camelCase. Through Smudgy 0.5.x the
//! `snake_case` names earlier versions used keep working: `mapper.ts` exposes
//! both on what it returns and accepts both in what it takes, and the first
//! `snake_case` name a script uses draws one notice per isolate
//! ([`op_smudgy_mapper_warn_snake_case_once`]). Refusal codes keep their
//! `snake_case` spelling on the wire and are translated where a failure
//! message crosses into a script ([`script_reasons`]), with the old spelling
//! beside the new one until 0.6. Both shims go with the 0.6 gate in
//! `mapper_api.rs`.

use std::{borrow::Cow, sync::Arc};

use deno_core::{OpState, op2};

use crate::session::runtime::action::{ActionQueue, RuntimeAction};

/// Every refusal code a mapper call's failure can carry, as the wire and the
/// cloud client spell it, with the spelling scripts see.
const REASON_CODES: &[(&str, &str)] = &[
    ("merge_areas_busy", "mergeAreasBusy"),
    ("merge_areas_invalid_rooms", "mergeAreasInvalidRooms"),
    (
        "merge_areas_invalid_translation",
        "mergeAreasInvalidTranslation",
    ),
    ("merge_areas_mixed_tiers", "mergeAreasMixedTiers"),
    ("merge_areas_no_rooms", "mergeAreasNoRooms"),
    ("merge_areas_no_sources", "mergeAreasNoSources"),
    ("merge_areas_room_not_found", "mergeAreasRoomNotFound"),
    (
        "merge_areas_room_numbers_exhausted",
        "mergeAreasRoomNumbersExhausted",
    ),
    ("merge_areas_same_area", "mergeAreasSameArea"),
    ("merge_areas_source_changed", "mergeAreasSourceChanged"),
    (
        "merge_areas_unsupported_storage",
        "mergeAreasUnsupportedStorage",
    ),
    ("merge_cross_area_links", "mergeCrossAreaLinks"),
    ("merge_secret_room_data", "mergeSecretRoomData"),
    ("move_busy", "moveBusy"),
    ("move_splits_links", "moveSplitsLinks"),
    ("room_number_exists", "roomNumberExists"),
    ("secret_area_level", "secretAreaLevel"),
    ("secret_cannot_add", "secretCannotAdd"),
    ("secret_cannot_edit", "secretCannotEdit"),
    ("secret_cannot_remove", "secretCannotRemove"),
    ("secret_link_between_secrets", "secretLinkBetweenSecrets"),
    ("secret_link_into_other_map", "secretLinkIntoOtherMap"),
    ("secret_linked_map_rooms", "secretLinkedMapRooms"),
    ("secret_map_exit_retarget", "secretMapExitRetarget"),
    ("secret_view_only", "secretViewOnly"),
];

/// The camelCase spelling of a refusal code, when `word` is one.
fn reason_code(word: &str) -> Option<&'static str> {
    REASON_CODES
        .iter()
        .find_map(|(wire, script)| (*wire == word).then_some(*script))
}

/// A failure message as a script reads it: each refusal code in its
/// camelCase spelling, followed through 0.5.x by the `snake_case` spelling
/// earlier versions showed, so a script matching the old code keeps
/// matching (`roomNumberExists; formerly room_number_exists`).
pub(super) fn script_reasons(message: &str) -> Cow<'_, str> {
    let is_word = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_';
    if !message.contains('_') {
        return Cow::Borrowed(message);
    }
    let mut out = String::with_capacity(message.len() + 32);
    let mut rest = message;
    let mut changed = false;
    while let Some(start) = rest.find(is_word) {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest.find(|c: char| !is_word(c)).unwrap_or(rest.len());
        let word = &rest[..end];
        match reason_code(word) {
            Some(script) => {
                out.push_str(script);
                out.push_str("; formerly ");
                out.push_str(word);
                changed = true;
            }
            None => out.push_str(word),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    if changed {
        Cow::Owned(out)
    } else {
        Cow::Borrowed(message)
    }
}

/// Per-isolate latch for the `snake_case` notice.
struct SnakeCaseWarnIssued;

/// Echo the `snake_case` notice to the session, once per isolate: the first
/// `snake_case` mapper name a script reads or passes, the camelCase name that
/// replaced it, and when the old names go. Once per isolate, because a
/// script written against the old names uses dozens of them.
#[op2(fast)]
pub(super) fn op_smudgy_mapper_warn_snake_case_once(
    state: &mut OpState,
    #[string] old: &str,
    #[string] new: &str,
) {
    if state.try_borrow::<SnakeCaseWarnIssued>().is_some() {
        return;
    }
    state.put(SnakeCaseWarnIssued);
    log::warn!(
        "smudgy: a script used the snake_case mapper name {old} (supported through 0.5.x; it is {new} now)"
    );
    state
        .borrow::<ActionQueue>()
        .borrow_mut()
        .push_back(RuntimeAction::Echo(Arc::new(format!(
            "[mapper] A script used the snake_case mapper name {old}; it is {new} now. Every \
             mapper name is camelCase, and the snake_case names keep working through Smudgy \
             0.5.x. Scripts should switch to the camelCase names before 0.6."
        ))));
}

#[cfg(test)]
mod tests {
    use super::{REASON_CODES, script_reasons};

    #[test]
    fn refusal_codes_reach_scripts_in_camel_case_beside_the_old_spelling() {
        assert_eq!(
            script_reasons(
                "Failed to commit map changes: A room with that number already exists there. (room_number_exists)"
            ),
            "Failed to commit map changes: A room with that number already exists there. \
             (roomNumberExists; formerly room_number_exists)"
        );
        assert_eq!(
            script_reasons("merge_areas_no_rooms and merge_areas_no_sources"),
            "mergeAreasNoRooms; formerly merge_areas_no_rooms and \
             mergeAreasNoSources; formerly merge_areas_no_sources"
        );
        for untouched in [
            "Area not found",
            "The map's structure changed underneath this edit: link_target_missing",
            "an_area_id 67e55044-10b1-426f-9247-bb680e5fe0c8",
        ] {
            assert_eq!(script_reasons(untouched), untouched);
        }
    }

    #[test]
    fn every_camel_case_code_is_its_wire_code_without_underscores() {
        for (wire, script) in REASON_CODES {
            let mut expected = String::new();
            let mut upper = false;
            for c in wire.chars() {
                if c == '_' {
                    upper = true;
                } else if upper {
                    expected.push(c.to_ascii_uppercase());
                    upper = false;
                } else {
                    expected.push(c);
                }
            }
            assert_eq!(*script, expected, "{wire}");
        }
    }
}
