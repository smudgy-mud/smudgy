//! Sharing-tab visibility and private grant renderers.

use super::*;
use iced::Padding;
use iced::widget::column;

impl AutomationsWindow {
    pub(in super::super) fn signed_out_banner<'a>(&self) -> Elem<'a> {
        container(text(crate::i18n::t!("package-sign-in-shared")).size(13.0))
            .width(Length::Fill)
            .padding(Padding {
                top: 10.0,
                bottom: 10.0,
                left: 14.0,
                right: 14.0,
            })
            .style(common::banner_style)
            .into()
    }

    pub(super) fn owned_sharing_section(&self) -> Elem<'_> {
        let mut col = Column::new()
            .spacing(10.0)
            .push(common::section_label(crate::i18n::ts!("package-sharing")));
        if self.share_package_id.is_none() {
            return col
                .push(
                    text(crate::i18n::t!("package-publish-before-sharing"))
                        .size(12.0)
                        .style(common::muted),
                )
                .into();
        }
        // Visibility card.
        col = col.push(
            container(
                row![
                    column![
                        text(if self.share_is_public {
                            crate::i18n::t!("package-public")
                        } else {
                            crate::i18n::t!("package-private")
                        })
                        .size(13.0),
                        text(if self.share_is_public {
                            crate::i18n::ts!("package-public-help")
                        } else if self.share_clan.is_some() {
                            crate::i18n::ts!("package-clan-private-help")
                        } else {
                            crate::i18n::ts!("package-private-help")
                        })
                        .size(11.0)
                        .style(common::muted),
                    ]
                    .spacing(2.0),
                    iced::widget::space::horizontal(),
                    button(
                        text(if self.share_is_public {
                            crate::i18n::t!("package-make-private")
                        } else {
                            crate::i18n::t!("package-make-public")
                        })
                        .size(12.0)
                    )
                    .style(button_style::secondary)
                    .on_press_maybe(
                        (!self.authoring_busy
                            && !self.share_busy
                            && self.may_change_availability_here())
                        .then_some(Message::SetVisibility(!self.share_is_public)),
                    ),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
            )
            .padding(12.0)
            .width(Length::Fill)
            .style(common::banner_style),
        );

        // A clan shares its packages by membership, not with friends.
        if self.share_clan.is_some() {
            return col.into();
        }

        // Friends list (private only).
        if !self.share_is_public {
            let mut friends = Column::new().spacing(4.0);
            if self.share_friends.is_empty() {
                friends = friends.push(
                    text(crate::i18n::t!("package-no-friends"))
                        .size(12.0)
                        .style(common::muted),
                );
            }
            for friend in &self.share_friends {
                let handle = friend
                    .nickname
                    .clone()
                    .unwrap_or_else(|| crate::i18n::t!("package-unknown-user"));
                let shared = self
                    .share_grants
                    .iter()
                    .any(|g| g.grantee_id == Some(friend.user_id) || g.all_friends);
                friends = friends.push(
                    row![
                        text(crate::assets::bootstrap_icons::PEOPLE)
                            .font(fonts::BOOTSTRAP_ICONS)
                            .size(13.0)
                            .style(common::muted),
                        text(handle).size(13.0),
                        iced::widget::space::horizontal(),
                        button(
                            text(if shared {
                                crate::i18n::t!("package-shared-check")
                            } else {
                                crate::i18n::t!("package-share")
                            })
                            .size(12.0),
                        )
                        .style(button_style::secondary)
                        .on_press_maybe(
                            (!self.authoring_busy && !self.share_busy)
                                .then_some(Message::ShareWithFriend(friend.user_id)),
                        ),
                    ]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
                );
            }
            col = col.push(friends);
        }
        col.into()
    }
}
