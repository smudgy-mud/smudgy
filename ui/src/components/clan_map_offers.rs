//! The map ownership offers naming the caller, in Settings › Clans beside
//! the Secret ownership offers: a member offering their Member-owned map's
//! ownership, or a clan owner offering a Clan-owned map to members. Each has
//! Accept and Decline; a joint offer applies when its last recipient accepts.

use iced::widget::{button, column, row, text};
use iced::{Alignment, Length, Task};
use smudgy_cloud::clan_maps::{AreaOwnershipOffer, MapOwnership};
use smudgy_cloud::{CloudError, Uuid};

use crate::cloud_account::CloudHandles;
use crate::components::cloud_errors::display_error;
use crate::theme::{self, Element as ThemedElement};

#[derive(Debug, Clone)]
pub enum Message {
    Loaded(Result<Vec<AreaOwnershipOffer>, CloudError>),
    Accept(Uuid),
    Decline(Uuid),
    Answered(Result<(), CloudError>),
}

/// The map ownership offers naming the caller.
#[derive(Debug, Default)]
pub struct MapOffers {
    pub offers: Vec<AreaOwnershipOffer>,
    pub error: Option<String>,
}

impl MapOffers {
    /// Loads the offers again.
    pub fn refresh(cloud: &CloudHandles) -> Task<Message> {
        let client = cloud.client.clone();
        Task::perform(
            async move { client.my_area_offers().await },
            Message::Loaded,
        )
    }

    /// The offers the caller has not accepted yet.
    #[must_use]
    pub fn waiting(&self, me: Option<Uuid>) -> usize {
        self.offers
            .iter()
            .filter(|offer| !me.is_some_and(|me| offer.accepted_by(me)))
            .count()
    }

    pub fn update(&mut self, cloud: &CloudHandles, message: Message) -> Task<Message> {
        match message {
            Message::Loaded(result) => {
                match result {
                    Ok(offers) => {
                        self.offers = offers;
                        self.error = None;
                    }
                    Err(CloudError::NotFoundOrNoAccess) => self.offers.clear(),
                    Err(error) => self.error = Some(display_error(&error)),
                }
                Task::none()
            }
            Message::Accept(offer) | Message::Decline(offer) => {
                let accept = matches!(message, Message::Accept(_));
                let Some(area_id) = self
                    .offers
                    .iter()
                    .find(|known| known.id == offer)
                    .map(|known| known.area_id)
                else {
                    return Task::none();
                };
                self.error = None;
                let client = cloud.client.clone();
                Task::perform(
                    async move {
                        if accept {
                            client
                                .accept_area_offer(area_id, offer, None)
                                .await
                                .map(|_| ())
                        } else {
                            client.decline_area_offer(area_id, offer).await
                        }
                    },
                    Message::Answered,
                )
            }
            Message::Answered(result) => {
                if let Err(error) = result {
                    self.error = Some(match error {
                        CloudError::NotFoundOrNoAccess => {
                            crate::i18n::t!("clan-share-ownership-refused")
                        }
                        other => display_error(&other),
                    });
                }
                Self::refresh(cloud)
            }
        }
    }

    /// The offers, each with Accept and Decline. `clan_name` names a clan
    /// by id.
    pub fn view<'a>(
        &'a self,
        me: Option<Uuid>,
        clan_name: impl Fn(Uuid) -> String,
    ) -> ThemedElement<'a, Message> {
        let mut col = column![text(crate::i18n::t!("clan-map-offers")).size(15)].spacing(8);
        if let Some(error) = &self.error {
            col = col.push(
                text(error.clone())
                    .size(12)
                    .style(theme::builtins::text::danger),
            );
        }
        let separator = crate::i18n::t!("mapper-multi-list-separator");
        for offer in &self.offers {
            let what = offer_sentence(offer, clan_name(offer.clan_id));
            let others: Vec<String> = offer
                .recipients
                .iter()
                .filter(|recipient| Some(recipient.user_id) != me && !recipient.accepted)
                .map(|recipient| {
                    recipient
                        .nickname
                        .clone()
                        .unwrap_or_else(|| crate::i18n::t!("clan-maps-a-member"))
                })
                .collect();
            let accepted = me.is_some_and(|me| offer.accepted_by(me));
            let mut about = column![text(what).size(13)].spacing(2).width(Length::Fill);
            if accepted && !others.is_empty() {
                about = about.push(muted(crate::i18n::t!(
                    "clans-secret-offer-accepted",
                    "others" => others.join(&separator)
                )));
            } else if !others.is_empty() {
                about = about.push(muted(crate::i18n::t!(
                    "clans-secret-offer-joint",
                    "others" => others.join(&separator)
                )));
            }
            let mut line = row![about].spacing(8).align_y(Alignment::Center);
            if !accepted {
                line = line.push(small_button(
                    crate::i18n::t!("social-accept"),
                    theme::builtins::button::primary,
                    Message::Accept(offer.id),
                ));
            }
            line = line.push(small_button(
                crate::i18n::t!("social-decline"),
                theme::builtins::button::secondary,
                Message::Decline(offer.id),
            ));
            col = col.push(line);
        }
        col.into()
    }
}

/// The sentence an offer opens with. An initiator with no nickname has
/// whole sentences of their own, so the sentence still opens with a capital.
fn offer_sentence(offer: &AreaOwnershipOffer, clan: String) -> String {
    let map = offer.area_name.clone();
    let replace = offer.ownership == MapOwnership::Members && offer.replace;
    match (offer.initiator_nickname.clone(), replace) {
        (Some(initiator), true) => crate::i18n::t!(
            "clan-map-offer-replace",
            "initiator" => initiator,
            "map" => map,
            "clan" => clan
        ),
        (Some(initiator), false) => crate::i18n::t!(
            "clan-map-offer",
            "initiator" => initiator,
            "map" => map,
            "clan" => clan
        ),
        (None, true) => crate::i18n::t!(
            "clan-map-offer-replace-from-a-member",
            "map" => map,
            "clan" => clan
        ),
        (None, false) => crate::i18n::t!(
            "clan-map-offer-from-a-member",
            "map" => map,
            "clan" => clan
        ),
    }
}

fn muted<'a>(label: String) -> ThemedElement<'a, Message> {
    text(label)
        .size(12)
        .style(|theme: &crate::Theme| iced::widget::text::Style {
            color: Some(theme.styles.text.normal.scale_alpha(0.6)),
        })
        .into()
}

fn small_button<'a>(
    label: String,
    style: fn(&crate::Theme, iced::widget::button::Status) -> iced::widget::button::Style,
    message: Message,
) -> ThemedElement<'a, Message> {
    button(text(label).size(12))
        .style(style)
        .padding([2, 8])
        .on_press(message)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_the_caller_accepted_no_longer_wait_on_them() {
        let me = Uuid::from_u128(4);
        let offer: AreaOwnershipOffer = serde_json::from_value(serde_json::json!({
            "id": Uuid::from_u128(1),
            "area_id": Uuid::from_u128(2),
            "area_name": "Grove",
            "clan_id": Uuid::from_u128(3),
            "recipients": [
                { "user_id": me, "accepted": true },
                { "user_id": Uuid::from_u128(5), "accepted": false }
            ],
            "ownership": "members",
            "initiator_id": Uuid::from_u128(6),
            "created_at": "2026-10-07T00:00:00Z"
        }))
        .unwrap();
        let mut offers = MapOffers {
            offers: vec![offer],
            error: None,
        };
        assert_eq!(offers.waiting(Some(me)), 0);
        assert_eq!(offers.waiting(Some(Uuid::from_u128(5))), 1);
        offers.offers.clear();
        assert_eq!(offers.waiting(Some(me)), 0);
    }

    #[test]
    fn an_offer_from_a_member_with_no_nickname_opens_with_a_capital() {
        let offer: AreaOwnershipOffer = serde_json::from_value(serde_json::json!({
            "id": Uuid::from_u128(1),
            "area_id": Uuid::from_u128(2),
            "area_name": "Grove",
            "clan_id": Uuid::from_u128(3),
            "recipients": [{ "user_id": Uuid::from_u128(4), "accepted": false }],
            "ownership": "members",
            "replace": true,
            "initiator_id": Uuid::from_u128(6),
            "created_at": "2026-10-07T00:00:00Z"
        }))
        .unwrap();
        assert!(offer.initiator_nickname.is_none());
        let sentence = offer_sentence(&offer, "Lantern".to_string());
        assert!(sentence.contains("Grove") && sentence.contains("Lantern"));
        for catalog in smudgy_i18n::available_catalogs() {
            let translator = smudgy_i18n::Translator::for_tag(catalog.tag).unwrap();
            for id in [
                "clan-map-offer-from-a-member",
                "clan-map-offer-replace-from-a-member",
            ] {
                let first = translator.translate(id).chars().next().unwrap();
                assert!(!first.is_lowercase(), "{} {id}", catalog.tag);
            }
        }
    }
}
