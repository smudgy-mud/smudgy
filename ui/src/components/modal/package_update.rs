//! The package-update review modal: one needs-permissions offer, rendered with the
//! same risk-tier rows and full-access banner as the Automations window's install
//! and update cards (`components::permissions`).
//!
//! Opened from the needs-permissions toast's Review action. The modal itself only
//! renders the offer and reports the decision as an [`Event`]; the window closes it and
//! the daemon performs the consequences — Grant & update atomically records the new union and
//! stages the update if its source row is unchanged, Pin conditionally writes the pinned mode,
//! and Not now changes nothing (the toast's Later is the persisted dismissal).

use iced::Length;
use iced::Task;
use iced::alignment::Vertical;
use iced::widget::{button, column, container, row, scrollable, text};
use smudgy_core::models::shared_packages::LockedPackage;

use crate::components::permissions::{consent_can_row, full_access_banner, permission_can_lines};
use crate::components::toast::UpdateOffer;
use crate::i18n::t;
use crate::theme::{self, Element};
use crate::windows::automations_window::common;

/// The open modal's state: the offer under review, exactly as the toast carried it.
#[derive(Debug)]
pub struct State {
    offer: UpdateOffer,
}

impl State {
    #[must_use]
    pub fn new(offer: UpdateOffer) -> Self {
        Self { offer }
    }
}

/// A button press inside the modal.
#[derive(Debug, Clone)]
pub enum Message {
    Grant,
    Pin,
    NotNow,
}

/// The decision, for the window to close the modal on and the daemon to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Grant & update: prefetch, then atomically record consent and stage if the row is unchanged.
    Grant(Box<UpdateOffer>),
    /// Pin the currently staged `version` — the terminal "stop asking" answer.
    Pin {
        server_name: String,
        expected: Box<LockedPackage>,
        specifier: String,
        version: String,
    },
    /// Close without persisting anything.
    Close,
}

pub fn update(state: &mut State, message: Message) -> (Task<Message>, Option<Event>) {
    let event = match message {
        Message::Grant => Event::Grant(Box::new(state.offer.clone())),
        Message::Pin => match &state.offer.current {
            Some(version) => Event::Pin {
                server_name: state.offer.server_name.clone(),
                expected: Box::new(state.offer.expected.clone()),
                specifier: state.offer.specifier.clone(),
                version: version.clone(),
            },
            // The Pin button only renders when a staged version exists; an
            // impossible press degrades to a plain close.
            None => Event::Close,
        },
        Message::NotNow => Event::Close,
    };
    (Task::none(), Some(event))
}

pub fn view(state: &State) -> Element<'_, Message> {
    let offer = &state.offer;

    let versions = match &offer.current {
        Some(current) => t!(
            "package-update-versions",
            "current" => current.as_str(),
            "latest" => offer.latest.as_str()
        ),
        None => t!("package-update-version-new", "latest" => offer.latest.as_str()),
    };
    let header = row![
        text(offer.name.clone()).size(16.0),
        text(versions).size(13.0).style(common::muted),
    ]
    .spacing(12.0)
    .align_y(Vertical::Center);

    let mut content = column![header].spacing(12.0);

    // Version-floor precedence: an update this smudgy cannot run is not grantable —
    // say so up front and withhold the grant action entirely.
    if let Some(required) = &offer.needs_smudgy {
        content = content.push(
            container(
                text(t!("package-update-needs-smudgy", "version" => required.as_str()))
                    .size(13.0)
                    .style(common::warning),
            )
            .padding(12.0)
            .width(Length::Fill)
            .style(common::banner_style),
        );
    }

    content = content.push(text(t!("package-update-asks")).size(13.0));
    if let Some(banner) = full_access_banner(&offer.added) {
        content = content.push(banner);
    }
    let mut rows = column![].spacing(6.0);
    for line in permission_can_lines(&offer.added) {
        rows = rows.push(consent_can_row(&line));
    }
    content = content.push(scrollable(rows).height(Length::Fill));

    let mut actions = row![].spacing(10.0).align_y(Vertical::Center);
    if offer.needs_smudgy.is_none() {
        actions = actions.push(
            button(text(t!("package-update-grant")).size(13.0))
                .style(theme::builtins::button::primary)
                .padding([6, 14])
                .on_press(Message::Grant),
        );
    }
    if offer.current.is_some() {
        actions = actions.push(
            button(text(t!("package-update-pin")).size(12.0))
                .style(theme::builtins::button::secondary)
                .padding([6, 12])
                .on_press(Message::Pin),
        );
    }
    actions = actions.push(
        button(text(t!("package-update-not-now")).size(12.0))
            .style(theme::builtins::button::link)
            .padding([6, 12])
            .on_press(Message::NotNow),
    );
    content = content.push(actions);

    container(content).padding(20.0).width(Length::Fill).into()
}
