//! A Secret's or Private's own data fields: properties kept by the place
//! itself rather than by one of its rooms, shown on its page in the style
//! of the map's data fields. A value changes, and a field is added, with
//! the place's `edit`; a field goes with its `remove`. Private additions
//! are the viewer's own and always writable.

use iced::Length;
use iced::alignment::Vertical;
use iced::widget::{Column, button, row, text, text_input};
use smudgy_cloud::mapper::area_cache::AreaCache;
use smudgy_cloud::mutation::AreaMutation;
use smudgy_cloud::{AreaId, SourceId};

use crate::assets::{bootstrap_icons, fonts};
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;

use super::commands::{CoalesceKey, Command, EntityRef, FieldId, Mutation};
use super::secrets::{SecretsMessage, bundle};
use super::{MapEditorWindow, Message};

/// The place's own fields, by name.
#[must_use]
pub fn fields(area: &AreaCache, source: SourceId) -> Vec<(String, String)> {
    let mut fields: Vec<(String, String)> = bundle(area, source)
        .map(|bundle| {
            bundle
                .properties
                .iter()
                .map(|property| (property.name.clone(), property.value.clone()))
                .collect()
        })
        .unwrap_or_default();
    fields.sort();
    fields
}

/// Whether the viewer may change a field's value or add one.
#[must_use]
pub fn may_edit(area: &AreaCache, source: SourceId) -> bool {
    match source {
        SourceId::Map => false,
        SourceId::Private => true,
        SourceId::Secret(_) => bundle(area, source).is_some_and(|bundle| bundle.can("edit")),
    }
}

/// Whether the viewer may take a field away.
#[must_use]
pub fn may_remove(area: &AreaCache, source: SourceId) -> bool {
    match source {
        SourceId::Map => false,
        SourceId::Private => true,
        SourceId::Secret(_) => bundle(area, source).is_some_and(|bundle| bundle.can("remove")),
    }
}

/// One write of `operation` to `source`, the place's own field.
fn write(map: AreaId, source: SourceId, operation: AreaMutation) -> Mutation {
    Mutation::SourceBatch {
        area_id: map,
        source,
        operations: vec![operation],
        description: "Set the place's data field".to_string(),
        split_paired_exit: false,
    }
}

fn prior_value(area: &AreaCache, source: SourceId, name: &str) -> Option<String> {
    bundle(area, source)?
        .properties
        .iter()
        .find(|property| property.name == name)
        .map(|property| property.value.clone())
}

/// Sets field `name` of `source` on `area`; consecutive edits of one field
/// make one undo step.
#[must_use]
pub fn set_field(
    area: &AreaCache,
    source: SourceId,
    name: String,
    value: String,
) -> Option<Command> {
    if source.is_map() {
        return None;
    }
    let map = *area.get_id();
    let undo = match prior_value(area, source, &name) {
        Some(prior) => AreaMutation::UpsertAreaProperty {
            name: name.clone(),
            value: prior,
        },
        None => AreaMutation::DeleteAreaProperty { name: name.clone() },
    };
    Some(
        Command::new(
            vec![write(
                map,
                source,
                AreaMutation::UpsertAreaProperty {
                    name: name.clone(),
                    value,
                },
            )],
            vec![write(map, source, undo)],
        )
        .coalescing(CoalesceKey::with_detail(
            EntityRef::Place(map, source),
            FieldId::Property,
            name,
        )),
    )
}

/// Takes field `name` away from `source` on `area`.
#[must_use]
pub fn delete_field(area: &AreaCache, source: SourceId, name: String) -> Option<Command> {
    if source.is_map() {
        return None;
    }
    let map = *area.get_id();
    let prior = prior_value(area, source, &name)?;
    Some(Command::new(
        vec![write(
            map,
            source,
            AreaMutation::DeleteAreaProperty { name: name.clone() },
        )],
        vec![write(
            map,
            source,
            AreaMutation::UpsertAreaProperty { name, value: prior },
        )],
    ))
}

fn message(message: SecretsMessage) -> Message {
    Message::Secrets(message)
}

/// Whether the place's page has a Data fields section: when it keeps a
/// field, or the viewer may add one.
#[must_use]
pub fn section_applies(area: &AreaCache, source: SourceId) -> bool {
    !source.is_map() && (may_edit(area, source) || !fields(area, source).is_empty())
}

/// The fields themselves: each name with its value, an input where the
/// viewer may edit and a trash button where they may remove, then a row to
/// add one; read-only, "name: value" lines.
pub fn view<'a>(
    window: &'a MapEditorWindow,
    area: &AreaCache,
    source: SourceId,
) -> ThemedElement<'a, Message> {
    let editable = may_edit(area, source);
    let removable = may_remove(area, source);
    let fields = fields(area, source);
    let mut section = Column::new().spacing(4);
    if !editable {
        for (name, value) in fields {
            let mut line = row![text(format!("{name}: {value}")).size(12)].spacing(6);
            if let Some(move_to) = super::moves::property_move(
                window,
                source,
                smudgy_cloud::mutation::PropertyAddress {
                    name,
                    room_number: None,
                    room_source: SourceId::Map,
                },
            ) {
                line = line.push(move_to);
            }
            section = section.push(line);
        }
        return section.into();
    }
    for (name, value) in fields {
        let edited = name.clone();
        let mut line = row![
            text(name.clone()).size(13).width(Length::FillPortion(2)),
            text_input(crate::i18n::ts!("inspector-value-placeholder"), &value)
                .size(13)
                .on_input(move |value| message(SecretsMessage::FieldChanged(edited.clone(), value)))
                .width(Length::FillPortion(3)),
        ]
        .spacing(4)
        .align_y(Vertical::Center);
        if removable {
            line = line.push(
                button(
                    text(bootstrap_icons::TRASH_3)
                        .font(fonts::BOOTSTRAP_ICONS)
                        .size(14.0),
                )
                .style(builtins::button::toolbar)
                .on_press(message(SecretsMessage::FieldRemoved(name.clone()))),
            );
        }
        if let Some(move_to) = super::moves::property_move(
            window,
            source,
            smudgy_cloud::mutation::PropertyAddress {
                name,
                room_number: None,
                room_source: SourceId::Map,
            },
        ) {
            line = line.push(move_to);
        }
        section = section.push(line);
    }
    let draft = &window.secrets.new_field;
    let ready = !draft.0.trim().is_empty();
    section = section.push(
        row![
            text_input(crate::i18n::ts!("inspector-name-placeholder"), &draft.0)
                .size(13)
                .on_input(|name| message(SecretsMessage::NewFieldNameChanged(name)))
                .width(Length::FillPortion(2)),
            text_input(crate::i18n::ts!("inspector-value-placeholder"), &draft.1)
                .size(13)
                .on_input(|value| message(SecretsMessage::NewFieldValueChanged(value)))
                .on_submit(message(SecretsMessage::FieldAdded))
                .width(Length::FillPortion(3)),
            button(text(crate::i18n::t!("action-add")).size(13))
                .style(builtins::button::secondary)
                .on_press_maybe(ready.then(|| message(SecretsMessage::FieldAdded))),
        ]
        .spacing(4)
        .align_y(Vertical::Center),
    );
    section.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::windows::map_editor_window::commands::CommandStack;
    use crate::windows::map_editor_window::source_rooms::tests::loaded;

    fn area(mapper: &smudgy_cloud::Mapper, area_id: AreaId) -> std::sync::Arc<AreaCache> {
        mapper
            .get_current_atlas()
            .get_area(&area_id)
            .expect("loaded")
    }

    #[tokio::test]
    async fn a_secrets_own_fields_are_set_changed_and_removed_in_it() {
        let (mapper, area_id, secret) = loaded().await;
        assert!(fields(&area(&mapper, area_id), secret).is_empty());
        assert!(section_applies(&area(&mapper, area_id), secret));

        let mut stack = CommandStack::default();
        let set = set_field(
            &area(&mapper, area_id),
            secret,
            "route".to_string(),
            "via the cellar".to_string(),
        )
        .expect("a field");
        let Mutation::SourceBatch {
            area_id: written,
            source,
            ..
        } = &set.redo_mutations()[0]
        else {
            panic!("a place's field is written to the place");
        };
        assert_eq!((*written, *source), (area_id, secret));
        let _ = stack.push_and_apply(&mapper, set);
        assert_eq!(
            fields(&area(&mapper, area_id), secret),
            [("route".to_string(), "via the cellar".to_string())]
        );
        // The map's own fields never see it.
        assert_eq!(area(&mapper, area_id).properties().count(), 0);

        let change = set_field(
            &area(&mapper, area_id),
            secret,
            "route".to_string(),
            "through the shelf".to_string(),
        )
        .expect("a change");
        let _ = stack.push_and_apply(&mapper, change);
        assert_eq!(
            fields(&area(&mapper, area_id), secret)[0].1,
            "through the shelf"
        );

        let removal =
            delete_field(&area(&mapper, area_id), secret, "route".to_string()).expect("a removal");
        let _ = stack.push_and_apply(&mapper, removal);
        assert!(fields(&area(&mapper, area_id), secret).is_empty());
        // Nothing to take away twice.
        assert!(delete_field(&area(&mapper, area_id), secret, "route".to_string()).is_none());
    }

    #[tokio::test]
    async fn the_map_has_its_own_fields_elsewhere() {
        let (mapper, area_id, _) = loaded().await;
        let area = area(&mapper, area_id);
        assert!(set_field(&area, SourceId::Map, "a".into(), "b".into()).is_none());
        assert!(!section_applies(&area, SourceId::Map));
        assert!(may_edit(&area, SourceId::Private));
        assert!(may_remove(&area, SourceId::Private));
    }
}
