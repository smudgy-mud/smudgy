//! Review sharing before a personal cloud map leaves the service.

use iced::Task;
use smudgy_cloud::relocation::LocalMoveReview;
use smudgy_cloud::{AreaId, AtlasId, CloudError, MapDestination, MapStorage, RelocationMode};

use super::{Event, MapEditorWindow, Message, modals::Modal, multi_select::MultiMessage};
use crate::components::cloud_errors::display_error;
use crate::update::Update;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Maps {
        ids: Vec<AreaId>,
        destination: MapDestination,
        multi: bool,
    },
    Atlas(AtlasId),
}

impl MapEditorWindow {
    pub(super) fn review_local_move(&mut self, request: Request) -> Update<Message, Event> {
        let ids = match &request {
            Request::Maps { ids, .. } => ids.clone(),
            Request::Atlas(id) => self
                .mapper
                .get_current_atlas()
                .areas()
                .filter(|area| area.meta().atlas_id == Some(*id))
                .map(|area| *area.get_id())
                .collect(),
        };
        let atlas = self.mapper.get_current_atlas();
        let names = ids
            .iter()
            .filter_map(|id| atlas.get_area(id))
            .map(|area| area.get_name().to_string())
            .collect();
        self.modal = Some(Modal::ReviewLocalMove {
            request: request.clone(),
            reviews: None,
            error: None,
            names,
        });
        let mapper = self.mapper.clone();
        Update::with_task(Task::perform(
            async move { mapper.review_local_moves(&ids).await },
            move |result| Message::LocalMoveReviewed(request.clone(), result),
        ))
    }

    pub(super) fn local_move_reviewed(
        &mut self,
        request: Request,
        result: Result<Vec<LocalMoveReview>, CloudError>,
    ) -> Update<Message, Event> {
        let Some(Modal::ReviewLocalMove {
            request: waiting,
            reviews,
            error,
            ..
        }) = &mut self.modal
        else {
            return Update::none();
        };
        if waiting != &request {
            return Update::none();
        }
        match result {
            Ok(found) if found.iter().all(|review| !review.has_shares) => {
                self.modal = None;
                self.execute_local_move(request, found)
            }
            Ok(found) => {
                *reviews = Some(found);
                Update::none()
            }
            Err(failure) => {
                *error = Some(display_error(&failure));
                Update::none()
            }
        }
    }

    pub(super) fn confirm_local_move(&mut self) -> Update<Message, Event> {
        let Some(Modal::ReviewLocalMove {
            request,
            reviews: Some(reviews),
            error: None,
            ..
        }) = self.modal.take()
        else {
            return Update::none();
        };
        self.execute_local_move(request, reviews)
    }

    fn execute_local_move(
        &mut self,
        request: Request,
        reviews: Vec<LocalMoveReview>,
    ) -> Update<Message, Event> {
        let mapper = self.mapper.clone();
        match request {
            Request::Maps {
                ids,
                destination,
                multi,
            } => {
                if multi {
                    self.multi.busy = true;
                    self.multi.confirming_delete = false;
                }
                Update::with_task(Task::perform(
                    async move {
                        mapper.relocate_areas_reviewed(ids, destination, RelocationMode::Move, &reviews).await
                        .map_err(|failure| match &failure.completed {
                            Some(done) => {
                                let atlas = mapper.get_current_atlas();
                                let names = done.destination_ids.iter().filter_map(|id| atlas.get_area(id))
                                    .map(|area| area.get_name().to_string()).collect::<Vec<_>>().join(", ");
                                crate::i18n::t!("mapper-relocation-duplicate-notice", "error" => display_error(&failure.error), "name" => names)
                            }
                            None => failure.to_string(),
                        })
                    },
                    move |result| {
                        if multi {
                            Message::Multi(MultiMessage::Moved(result.map(|moved| {
                                moved
                                    .source_ids
                                    .into_iter()
                                    .zip(moved.destination_ids)
                                    .collect()
                            })))
                        } else {
                            Message::MoveAreaCompleted(result.and_then(|moved| {
                                moved
                                    .destination_ids
                                    .into_iter()
                                    .next()
                                    .ok_or_else(|| "move returned no map".to_string())
                            }))
                        }
                    },
                ))
            }
            Request::Atlas(id) => Update::with_task(Task::perform(
                async move {
                    mapper.relocate_atlas_reviewed(id, MapStorage::Local, RelocationMode::Move, &reviews).await
                    .map_err(|failure| match &failure.completed {
                        Some(done) => crate::i18n::t!("mapper-relocation-duplicate-notice", "error" => display_error(&failure.error), "name" => done.destination_atlas_name.clone()),
                        None => failure.to_string(),
                    })
                },
                Message::MoveAtlasStorageCompleted,
            )),
        }
    }
}
