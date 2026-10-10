//! Review cloud filing effects before moving maps or emptying a folder.
use super::{Event, MapEditorWindow, Message, modals::Modal, multi_select::MultiMessage};
use crate::{components::cloud_errors::display_error, update::Update};
use iced::Task;
use smudgy_cloud::{
    AreaId, AtlasId, MapDestination, MapStorage, Uuid, access_review::ReviewedFiling,
};

#[derive(Debug, Clone)]
pub enum Request {
    Maps {
        ids: Vec<AreaId>,
        destination: MapDestination,
        multi: bool,
        clan: bool,
    },
    EmptyAtlas {
        id: AtlasId,
        maps: Vec<AreaId>,
        destination: AtlasId,
    },
    DeleteSelection {
        dialog: Box<super::multi_select::DeleteDialog>,
    },
}

#[derive(Debug, Clone)]
pub struct Dialog {
    pub id: Uuid,
    pub request: Request,
    pub names: Vec<String>,
    pub reviews: Option<Vec<(String, ReviewedFiling)>>,
    pub error: Option<String>,
}

/// A new destination remains a real folder even if review fails or is cancelled.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub created: Option<AtlasId>,
    pub reviews: Result<Vec<(String, ReviewedFiling)>, String>,
}

impl MapEditorWindow {
    pub(super) fn review_filing(&mut self, request: Request) -> Update<Message, Event> {
        let (ids, atlas_id) = match &request {
            Request::Maps {
                ids, destination, ..
            } => (ids.clone(), destination.atlas_id),
            Request::EmptyAtlas {
                maps, destination, ..
            } => (maps.clone(), Some(*destination)),
            Request::DeleteSelection { dialog } => {
                (dialog.cloud_leftovers(), dialog.cloud_destination().0)
            }
        };
        let new_folder = match &request {
            Request::DeleteSelection { dialog } if atlas_id.is_none() => {
                dialog.cloud_destination().1
            }
            _ => None,
        };
        let ids: Vec<_> = ids
            .into_iter()
            .filter(|id| self.mapper.area_storage(id) == MapStorage::Cloud)
            .collect();
        let atlas = self.mapper.get_current_atlas();
        let names = ids
            .iter()
            .filter_map(|id| atlas.get_area(id))
            .map(|area| area.get_name().to_string())
            .collect();
        let id = Uuid::new_v4();
        self.modal = Some(Modal::ReviewFiling(Dialog {
            id,
            request,
            names,
            reviews: None,
            error: None,
        }));
        let mapper = self.mapper.clone();
        Update::with_task(Task::perform(
            async move {
                let mut created = None;
                let reviews = async {
                    let atlas_id = match new_folder {
                        Some(name) => {
                            let id = mapper
                                .create_atlas_at(name, MapStorage::Cloud)
                                .await
                                .map_err(|error| display_error(&error))?
                                .id;
                            created = Some(id);
                            Some(id)
                        }
                        None => atlas_id,
                    };
                    let mut reviews = Vec::new();
                    for area_id in ids {
                        let name = mapper
                            .get_current_atlas()
                            .get_area(&area_id)
                            .map(|area| area.get_name().to_string())
                            .unwrap_or_default();
                        let review = mapper
                            .review_filing(area_id, atlas_id)
                            .await
                            .map_err(|error| display_error(&error))?;
                        reviews.push((name, review));
                    }
                    Ok(reviews)
                }
                .await;
                Prepared { created, reviews }
            },
            move |result| Message::FilingReviewed(id, result),
        ))
    }

    pub(super) fn filing_reviewed(&mut self, id: Uuid, result: Prepared) -> Update<Message, Event> {
        let event = result.created.and_then(|id| self.associate_new_atlas(id));
        let update = Update::new(self.fetch_atlases(), event);
        let Some(Modal::ReviewFiling(dialog)) = &mut self.modal else {
            return update;
        };
        if dialog.id != id {
            return update;
        }
        match result.reviews {
            Ok(reviews) => dialog.reviews = Some(reviews),
            Err(error) => dialog.error = Some(error),
        }
        update
    }

    pub(super) fn confirm_filing(&mut self) -> Update<Message, Event> {
        if !matches!(
            &self.modal,
            Some(Modal::ReviewFiling(Dialog {
                reviews: Some(_),
                error: None,
                ..
            }))
        ) {
            return Update::none();
        }
        let Some(Modal::ReviewFiling(dialog)) = self.modal.take() else {
            return Update::none();
        };
        let reviews: Vec<_> = dialog
            .reviews
            .unwrap_or_default()
            .into_iter()
            .map(|(_, review)| review)
            .collect();
        let mapper = self.mapper.clone();
        match dialog.request {
            Request::DeleteSelection { dialog } => {
                self.multi.busy = true;
                self.multi.confirming_delete = false;
                super::multi_select::delete_submit(&dialog, mapper, reviews)
            }
            Request::Maps {
                ids,
                destination,
                multi,
                clan,
            } => {
                if multi {
                    self.multi.busy = true;
                    self.multi.confirming_delete = false;
                }
                Update::with_task(Task::perform(
                    async move {
                        if clan {
                            let area = ids
                                .first()
                                .copied()
                                .ok_or_else(|| "No map selected".to_string())?;
                            let review = reviews
                                .into_iter()
                                .next()
                                .ok_or_else(|| "Missing filing review".to_string())?;
                            mapper
                                .commit_reviewed_filing(review)
                                .await
                                .map_err(|error| display_error(&error))?;
                            Ok(vec![(area, area)])
                        } else {
                            mapper
                                .relocate_areas_with_filing_reviews(ids, destination, &reviews)
                                .await
                                .map(|result| {
                                    result
                                        .source_ids
                                        .into_iter()
                                        .zip(result.destination_ids)
                                        .collect()
                                })
                                .map_err(|failure| failure.to_string())
                        }
                    },
                    move |result: Result<Vec<(AreaId, AreaId)>, String>| {
                        if multi {
                            Message::Multi(MultiMessage::Moved(result))
                        } else {
                            Message::MoveAreaCompleted(result.and_then(|moved| {
                                moved
                                    .first()
                                    .map(|(_, id)| *id)
                                    .ok_or_else(|| "Move returned no map".to_string())
                            }))
                        }
                    },
                ))
            }
            Request::EmptyAtlas { id, .. } => Update::with_task(Task::perform(
                async move {
                    for review in reviews {
                        mapper
                            .commit_reviewed_filing(review)
                            .await
                            .map_err(|error| display_error(&error))?;
                    }
                    mapper
                        .delete_atlas(id)
                        .await
                        .map_err(|error| display_error(&error))
                },
                Message::AtlasDeleted,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::links::fixture::{KEEP, area, maps};
    use super::*;

    #[tokio::test]
    async fn late_filing_review_keeps_created_folder_visible_without_reopening_cancelled_dialog() {
        let mut window = super::super::test_window(maps().await, area(KEEP));
        window.server_name = Some("Test MUD".into());
        let created = AtlasId(Uuid::new_v4());
        window.modal = None;
        let update = window.filing_reviewed(
            Uuid::new_v4(),
            Prepared {
                created: Some(created),
                reviews: Err("Review refused".into()),
            },
        );
        assert!(window.modal.is_none());
        assert!(
            window
                .map_scopes
                .atlas_entries(&created)
                .contains("Test MUD")
        );
        assert!(matches!(
            update.event,
            Some(Event::ScopeAssociationsChanged(_))
        ));

        let id = Uuid::new_v4();
        window.modal = Some(Modal::ReviewFiling(Dialog {
            id,
            request: Request::EmptyAtlas {
                id: created,
                maps: vec![area(KEEP)],
                destination: created,
            },
            names: vec!["Keep".into()],
            reviews: None,
            error: None,
        }));
        let _ = window.filing_reviewed(
            Uuid::new_v4(),
            Prepared {
                created: None,
                reviews: Err("Old request".into()),
            },
        );
        assert!(matches!(
            &window.modal,
            Some(Modal::ReviewFiling(Dialog { error: None, .. }))
        ));
        let _ = window.filing_reviewed(
            id,
            Prepared {
                created: None,
                reviews: Err("Fresh review required".into()),
            },
        );
        let _ = window.confirm_filing();
        assert!(
            matches!(&window.modal, Some(Modal::ReviewFiling(Dialog { error: Some(error), .. })) if error == "Fresh review required")
        );
        assert!(
            window
                .mapper
                .get_current_atlas()
                .get_area(&area(KEEP))
                .is_some()
        );
    }
}
