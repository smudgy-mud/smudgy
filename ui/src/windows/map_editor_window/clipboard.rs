//! Cut records identities. Paste commits a move; cancellation preserves them.

use std::sync::Arc;
use std::time::Instant;

use iced::{Point, Vector};
use smudgy_map_widget::map_editor::{EntityId, Selection};

use super::{Event, MapEditorWindow, Message, commands, moves, secrets};
use crate::update::Update;

impl MapEditorWindow {
    pub(super) fn can_paste_copy(&mut self, clipboard: &commands::EntityClipboard) -> bool {
        let Some((map, source, generation)) = clipboard.copy_from else {
            return false;
        };
        let allowed =
            generation == self.mapper.auth_projection_revision()
                && self.mapper.get_current_atlas().get_area(&map).is_some_and(
                    |area| match source {
                        smudgy_cloud::SourceId::Map => area.effective_access().can_copy,
                        smudgy_cloud::SourceId::Private => true,
                        smudgy_cloud::SourceId::Secret(_) => area
                            .meta()
                            .sources
                            .iter()
                            .find(|bundle| bundle.source == source)
                            .is_some_and(|bundle| bundle.can("copy")),
                    },
                );
        if !allowed {
            self.clipboard_notice(crate::i18n::t!("mapper-copy-source-denied"));
        }
        allowed
    }

    fn clipboard_notice(&mut self, message: String) -> Update<Message, Event> {
        self.editor_notice = Some((Instant::now(), message));
        Update::none()
    }

    pub(super) fn source_copy_snapshot(
        &mut self,
        include_boundary_links: bool,
    ) -> Option<(commands::EntityClipboard, usize)> {
        let map = self.editor.area_id()?;
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(&map)?;
        let Some(moves::SelectionPlace::One(source)) = self.selection_place(&area) else {
            self.clipboard_notice(crate::i18n::t!("mapper-clipboard-one-source"));
            return None;
        };
        let allowed = match source {
            smudgy_cloud::SourceId::Map => area.effective_access().can_copy,
            smudgy_cloud::SourceId::Private => true,
            smudgy_cloud::SourceId::Secret(_) => area
                .meta()
                .sources
                .iter()
                .find(|bundle| bundle.source == source)
                .is_some_and(|bundle| bundle.can("copy")),
        };
        if !allowed {
            self.clipboard_notice(crate::i18n::t!("mapper-copy-source-denied"));
            return None;
        }
        let document = super::document::Document::of(&area, source)?;
        let content = document.content();
        let selection: Selection = self
            .editor
            .selection()
            .iter()
            .map(|entity| match entity {
                EntityId::SourceRoom(_, number) => EntityId::Room(number),
                other => other,
            })
            .collect();
        let mut snapshot =
            commands::snapshot_document(content, &selection, true, include_boundary_links);
        let source_area = if source.is_map() {
            map
        } else {
            area.source_layers()
                .iter()
                .find(|layer| layer.source() == source)?
                .area_id()
        };
        snapshot.source_area_id = Some(source_area);
        snapshot.source_map_id = Some(map);
        snapshot.copy_from = Some((map, source, self.mapper.auth_projection_revision()));
        Some((
            snapshot,
            commands::boundary_links_in_document(content, &selection),
        ))
    }

    pub(super) fn stage_cut(&mut self) -> Update<Message, Event> {
        if self.moving {
            return Update::none();
        }
        let Some(map) = self.editor.area_id() else {
            return Update::none();
        };
        let atlas = self.mapper.get_current_atlas();
        let Some(area) = atlas.get_area(&map) else {
            return Update::none();
        };
        let Some(moves::SelectionPlace::One(source)) = self.selection_place(&area) else {
            return self.clipboard_notice(crate::i18n::t!("mapper-clipboard-one-source"));
        };
        if !secrets::can_remove(&area, source) && !secrets::can(&area, source, "edit") {
            return self.clipboard_notice(crate::i18n::t!("mapper-cut-remove-required"));
        }
        let content = self.selected_content(&area);
        if content.is_empty() {
            return Update::none();
        }
        let cache = if source.is_map() {
            area.clone()
        } else {
            let Some(layer) = area
                .source_layers()
                .iter()
                .find(|layer| layer.source() == source)
            else {
                return Update::none();
            };
            layer.area().clone()
        };
        let normalized: Selection = self
            .editor
            .selection()
            .iter()
            .map(|entity| match entity {
                EntityId::SourceRoom(_, number) => EntityId::Room(number),
                other => other,
            })
            .collect();
        let snapshot =
            commands::snapshot_selection(&atlas, *cache.get_id(), &normalized, true, false);
        let cut = commands::CutSelection {
            id: smudgy_cloud::Uuid::new_v4(),
            map,
            source,
            revision: cache.get_rev(),
            auth_revision: self.mapper.auth_projection_revision(),
            selection: self.editor.selection().iter().collect(),
            content,
            center: commands::clipboard_center(&snapshot),
        };
        self.clipboard.store(Arc::new(commands::EntityClipboard {
            cut: Some(cut),
            ..Default::default()
        }));
        self.clipboard_notice(crate::i18n::t!("mapper-cut-ready"))
    }

    pub(super) fn paste_cut(
        &mut self,
        cut: commands::CutSelection,
        at: Option<Point>,
    ) -> Update<Message, Event> {
        if self.moving || self.clipboard_reposition_in_flight.is_some() {
            return Update::none();
        }
        if self.editor.area_id() != Some(cut.map) {
            return self.clipboard_notice(crate::i18n::t!("mapper-cut-cross-map-unavailable"));
        }
        let atlas = self.mapper.get_current_atlas();
        let Some(area) = atlas.get_area(&cut.map) else {
            return Update::none();
        };
        let revision = if cut.source.is_map() {
            Some(area.get_rev())
        } else {
            area.source_layers()
                .iter()
                .find(|layer| layer.source() == cut.source)
                .map(|layer| layer.area().get_rev())
        };
        if revision != Some(cut.revision)
            || self.mapper.auth_projection_revision() != cut.auth_revision
        {
            return self.clipboard_notice(crate::i18n::t!("mapper-cut-stale"));
        }
        let to = self.add_to();
        if to == cut.source {
            if !secrets::can(&area, to, "edit") {
                return Update::none();
            }
            let Some(at) = at else {
                return self.clipboard_notice(crate::i18n::t!("mapper-cut-same-source"));
            };
            let offset = cut
                .center
                .map_or(Vector::new(0.0, 0.0), |center| at - center);
            let selection = cut.selection.into_iter().collect();
            let command = commands::move_selection(
                &atlas,
                cut.map,
                &selection,
                smudgy_map_widget::viewport::snap_offset(offset),
            );
            if command.is_none() {
                return Update::none();
            }
            let (update, operations) = self.push_command_tracked(command);
            if !operations.is_empty() {
                self.clipboard_reposition_in_flight = Some(cut.id);
                let mapper = self.mapper.clone();
                let acknowledgement = iced::Task::perform(
                    async move {
                        for operation in operations {
                            loop {
                                if mapper.wait_for_mutation(operation).await.is_ok() {
                                    break;
                                }
                                if mapper.is_operation_pending(cut.map, operation) {
                                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                                    continue;
                                }
                                tokio::task::yield_now().await;
                                if mapper.wait_for_mutation(operation).await.is_err() {
                                    return false;
                                }
                                break;
                            }
                        }
                        true
                    },
                    move |acknowledged| Message::CutRepositionCompleted {
                        id: cut.id,
                        acknowledged,
                    },
                );
                return Update::new(
                    iced::Task::batch([update.task, acknowledgement]),
                    update.event,
                );
            }
            return update;
        }
        if !secrets::can_remove(&area, cut.source) || !secrets::can_add(&area, to) {
            return self.clipboard_notice(crate::i18n::t!("mapper-cut-remove-required"));
        }
        self.clipboard_cut_in_flight = Some(cut.id);
        self.start_move(
            cut.source,
            to,
            cut.content,
            moves::MoveKind::Asked { undoable: true },
        )
    }

    pub(super) fn finish_cut_reposition(&mut self, id: smudgy_cloud::Uuid, acknowledged: bool) {
        if self.clipboard_reposition_in_flight != Some(id) {
            return;
        }
        self.clipboard_reposition_in_flight = None;
        if acknowledged
            && self
                .clipboard
                .load()
                .cut
                .as_ref()
                .is_some_and(|cut| cut.id == id)
        {
            self.clipboard
                .store(Arc::new(commands::EntityClipboard::default()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::links::fixture::{CATACOMBS, KEEP, area, maps, maps_where, secret};
    use super::*;
    use smudgy_cloud::{RoomNumber, SourceId};

    #[tokio::test]
    async fn repositioned_cut_waits_for_acknowledgement_and_keeps_replacement_clipboards() {
        let mut window = super::super::test_window(maps().await, area(KEEP));
        window
            .editor
            .add_to_selection(EntityId::Room(RoomNumber(5)));
        let _ = window.stage_cut();
        let cut = window.clipboard.load().cut.clone().unwrap();
        let _ = window.paste_cut(cut.clone(), Some(Point::new(180.0, 180.0)));
        assert_eq!(window.clipboard_reposition_in_flight, Some(cut.id));
        assert_eq!(window.clipboard.load().cut.as_ref().unwrap().id, cut.id);
        window.finish_cut_reposition(cut.id, false);
        assert_eq!(
            window.clipboard.load().cut.as_ref().unwrap().id,
            cut.id,
            "discarded move keeps Cut"
        );
        window.clipboard_reposition_in_flight = Some(cut.id);
        let mut next = cut.clone();
        next.id = smudgy_cloud::Uuid::new_v4();
        window.clipboard.store(Arc::new(commands::EntityClipboard {
            cut: Some(next.clone()),
            ..Default::default()
        }));
        window.finish_cut_reposition(cut.id, true);
        assert_eq!(
            window.clipboard.load().cut.as_ref().unwrap().id,
            next.id,
            "an older acknowledgement cannot clear a newer Cut"
        );
        window.clipboard_reposition_in_flight = Some(next.id);
        window.finish_cut_reposition(next.id, true);
        assert!(window.clipboard.load().cut.is_none());
    }

    #[tokio::test]
    async fn cut_keeps_the_original_without_requiring_copy_and_cancel_keeps_the_ticket() {
        let mut window =
            super::super::test_window(maps_where(&["read", "remove"]).await, area(KEEP));
        window
            .editor
            .add_to_selection(EntityId::SourceRoom(secret(), RoomNumber(1)));
        let _ = window.stage_cut();
        let clipboard = window.clipboard.load_full();
        let cut = clipboard
            .cut
            .as_ref()
            .expect("Remove suffices to stage a move");
        assert!(
            window
                .mapper
                .get_current_atlas()
                .get_area(&area(KEEP))
                .unwrap()
                .source_layers()
                .iter()
                .find(|layer| layer.source() == secret())
                .unwrap()
                .own_room(RoomNumber(1))
                .is_some()
        );
        let _ = window.paste_cut(cut.clone(), None);
        assert!(
            window.moving,
            "paste loads the access review before removal"
        );
        let _ = window.handle_hotkey(super::super::Hotkey::Escape);
        assert!(!window.moving, "Escape restores editing");
        assert_eq!(window.clipboard.load().cut.as_ref().unwrap().id, cut.id);
        assert!(
            window
                .editor
                .selection()
                .contains(EntityId::SourceRoom(secret(), RoomNumber(1)))
        );
    }

    #[tokio::test]
    async fn cross_map_and_stale_cut_pastes_leave_the_original_and_clipboard_intact() {
        let mut window = super::super::test_window(maps().await, area(KEEP));
        window
            .editor
            .add_to_selection(EntityId::Room(RoomNumber(5)));
        let _ = window.stage_cut();
        let cut = window.clipboard.load().cut.clone().unwrap();
        let mut stale = cut.clone();
        stale.revision += 1;
        let _ = window.paste_cut(stale, None);
        assert!(!window.moving);
        let mut other = cut.clone();
        other.map = area(CATACOMBS);
        let _ = window.paste_cut(other, None);
        assert!(!window.moving);
        assert_eq!(window.clipboard.load().cut.as_ref().unwrap().id, cut.id);
        assert!(
            window
                .mapper
                .get_current_atlas()
                .get_area(&area(KEEP))
                .unwrap()
                .get_room(&RoomNumber(5))
                .is_some()
        );
    }

    #[tokio::test]
    async fn replacing_a_move_review_releases_the_editor_and_preserves_the_cut() {
        let mut window = super::super::test_window(maps().await, area(KEEP));
        window
            .editor
            .add_to_selection(EntityId::SourceRoom(secret(), RoomNumber(1)));
        let _ = window.stage_cut();
        let cut = window.clipboard.load().cut.clone().unwrap();
        let _ = window.paste_cut(cut.clone(), None);
        assert!(window.moving);
        let _ = window.update(Message::NewAreaRequested);
        assert!(!matches!(
            window.modal,
            Some(super::super::modals::Modal::ReviewMove { .. })
        ));
        assert!(!window.moving);
        assert!(window.running_move.is_none());
        assert_eq!(window.clipboard.load().cut.as_ref().unwrap().id, cut.id);
        assert!(
            window
                .mapper
                .get_current_atlas()
                .get_area(&area(KEEP))
                .unwrap()
                .source_layers()
                .iter()
                .find(|layer| layer.source() == secret())
                .unwrap()
                .own_room(RoomNumber(1))
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_copy_can_create_the_first_private_content_without_writing_the_map() {
        let mut window = super::super::test_window(maps().await, area(KEEP));
        window
            .editor
            .add_to_selection(EntityId::Room(RoomNumber(5)));
        assert!(window.copy_selection(false));
        window.secrets.add_to = Some((area(KEEP), SourceId::Private));
        let _ = window.paste_clipboard(None);
        let atlas = window.mapper.get_current_atlas();
        let map = atlas.get_area(&area(KEEP)).unwrap();
        assert_eq!(map.get_rooms().len(), 5);
        let private = map
            .source_layers()
            .iter()
            .find(|layer| layer.source() == SourceId::Private)
            .expect("the first paste creates Private content");
        let copied = private.own_room(RoomNumber(5)).unwrap();
        assert_eq!(copied.get_title(), "Tower");
        assert_eq!(copied.get_level(), 1);
        assert!(
            window
                .editor
                .selection()
                .contains(EntityId::SourceRoom(SourceId::Private, RoomNumber(5)))
        );
        let _ = window.stack.undo(&window.mapper);
        let atlas = window.mapper.get_current_atlas();
        let map = atlas.get_area(&area(KEEP)).unwrap();
        assert!(
            map.source_layers()
                .iter()
                .find(|layer| layer.source() == SourceId::Private)
                .is_none_or(|layer| layer.area().get_rooms().is_empty())
        );
        assert!(map.get_room(&RoomNumber(5)).is_some());
    }

    #[tokio::test]
    async fn secret_room_copy_never_substitutes_the_same_numbered_map_room() {
        let mut window = super::super::test_window(maps_where(&["read", "copy"]).await, area(KEEP));
        window
            .editor
            .add_to_selection(EntityId::SourceRoom(secret(), RoomNumber(1)));
        assert!(window.copy_selection(false));
        assert_eq!(window.clipboard.load().rooms[0].title, "Hidden Library");
        let _ = window.paste_clipboard(None);
        let atlas = window.mapper.get_current_atlas();
        let map = atlas.get_area(&area(KEEP)).unwrap();
        assert_eq!(map.get_room(&RoomNumber(1)).unwrap().get_title(), "Hall");
        assert_eq!(
            map.get_room(&RoomNumber(6)).unwrap().get_title(),
            "Hidden Library"
        );
    }

    #[tokio::test]
    async fn copied_secret_links_keep_map_anchors_and_remap_only_the_copied_rooms() {
        use super::super::links::fixture::{C_LIBRARY, link};
        let mut window = super::super::test_window(maps_where(&["read", "copy"]).await, area(KEEP));
        window
            .editor
            .add_to_selection(EntityId::SourceRoom(secret(), RoomNumber(1)));
        window
            .editor
            .add_to_selection(EntityId::Connection(link(C_LIBRARY)));
        assert!(window.copy_selection(false));
        let _ = window.paste_clipboard(None);
        let atlas = window.mapper.get_current_atlas();
        let map = atlas.get_area(&area(KEEP)).unwrap();
        let hall = map.get_room(&RoomNumber(1)).unwrap();
        let copied = map.get_room(&RoomNumber(6)).unwrap();
        let outward = hall
            .get_exits()
            .iter()
            .find(|exit| exit.from_direction == smudgy_cloud::ExitDirection::West)
            .expect("the copied link remains attached to the ordinary Hall");
        let inward = copied
            .get_exits()
            .iter()
            .find(|exit| exit.from_direction == smudgy_cloud::ExitDirection::East)
            .expect("the other end belongs to the copied Library");
        assert_eq!(outward.to_room_number, Some(RoomNumber(6)));
        assert_eq!(inward.to_room_number, Some(RoomNumber(1)));
        assert_eq!(outward.connection_id, inward.connection_id);
        let _ = window.stack.undo(&window.mapper);
        let atlas = window.mapper.get_current_atlas();
        let map = atlas.get_area(&area(KEEP)).unwrap();
        assert!(map.get_room(&RoomNumber(6)).is_none());
        assert!(
            !map.get_room(&RoomNumber(1))
                .unwrap()
                .get_exits()
                .iter()
                .any(|exit| exit.from_direction == smudgy_cloud::ExitDirection::West)
        );
        assert!(
            map.source_layers()
                .iter()
                .find(|layer| layer.source() == secret())
                .unwrap()
                .own_room(RoomNumber(1))
                .is_some()
        );
    }

    #[tokio::test]
    async fn copying_room_tags_to_private_preserves_them() {
        use super::super::links::fixture::{serving, the_maps};
        let mut maps = the_maps(&["read", "copy"]);
        maps[0].rooms[4].tags.insert("LOOKOUT".into());
        let mut window = super::super::test_window(serving(maps).await, area(KEEP));
        window
            .editor
            .add_to_selection(EntityId::Room(RoomNumber(5)));
        assert!(window.copy_selection(false));
        window.secrets.add_to = Some((area(KEEP), SourceId::Private));
        let _ = window.paste_clipboard(None);
        let atlas = window.mapper.get_current_atlas();
        let map = atlas.get_area(&area(KEEP)).unwrap();
        assert!(
            map.source_layers()
                .iter()
                .find(|layer| layer.source() == SourceId::Private)
                .unwrap()
                .own_room(RoomNumber(5))
                .unwrap()
                .has_tag("LOOKOUT")
        );
    }
}
