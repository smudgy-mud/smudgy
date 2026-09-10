// Re-export specific types needed by the main application
pub use self::connect::Event as ConnectEvent;
pub use self::connect::Message as ConnectMessage;

use iced::Length;
use iced::Task;
use iced::widget::row;
use iced::widget::text;
use iced::widget::{column, container};

use crate::theme;
use crate::theme::Element;
// Import modal implementation modules
pub mod connect;
pub mod layouts;
pub mod package_update;

/// Enum representing the currently active modal.
// At most one modal exists per window, so the size spread between the
// connect form and the smaller modals costs nothing worth boxing for.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Modal {
    Connect(connect::State),
    Layouts(layouts::State),
    PackageUpdate(package_update::State),
}

/// Messages that can be sent to the active modal.
#[derive(Debug, Clone)]
pub enum Message {
    Connect(ConnectMessage),
    Layouts(layouts::Message),
    PackageUpdate(package_update::Message),
}

/// Events that can be emitted by the active modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Connect(ConnectEvent),
    Layouts(layouts::Event),
    PackageUpdate(package_update::Event),
}

impl Modal {
    /// Update the state of the active modal.
    /// Returns a Task for the modal and optionally an Event to be handled by the main app.
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        match (self, message) {
            // Route Connect messages to the connect modal's update
            (Modal::Connect(state), Message::Connect(msg)) => {
                let (task, event) = connect::update(state, msg);
                // Map the task's message type and the event type
                (task.map(Message::Connect), event.map(Event::Connect))
            }
            (Modal::Layouts(state), Message::Layouts(msg)) => {
                let (task, event) = layouts::update(state, msg);
                (task.map(Message::Layouts), event.map(Event::Layouts))
            }
            (Modal::PackageUpdate(state), Message::PackageUpdate(msg)) => {
                let (task, event) = package_update::update(state, msg);
                (
                    task.map(Message::PackageUpdate),
                    event.map(Event::PackageUpdate),
                )
            }
            // A message for a modal that is no longer the active one (a
            // task settling after the modal switched): dropped.
            _ => (Task::none(), None),
        }
    }

    /// Get the view for the active modal.
    pub fn view(&self) -> Element<'_, Message> {
        let (title, width, height, inner) = match self {
            Modal::Connect(state) => (
                crate::i18n::t!("toolbar-connect"),
                800.0,
                600.0,
                connect::view(state).map(Message::Connect),
            ),
            Modal::Layouts(state) => (
                crate::i18n::t!("toolbar-layouts"),
                560.0,
                480.0,
                layouts::view(state).map(Message::Layouts),
            ),
            Modal::PackageUpdate(state) => (
                crate::i18n::t!("package-update-modal-title"),
                560.0,
                480.0,
                package_update::view(state).map(Message::PackageUpdate),
            ),
        };

        let title_bar_text = text(title)
            .center()
            .width(Length::Fill)
            .height(Length::Fixed(34.0));

        container(column![
            container(row![title_bar_text])
                .style(theme::builtins::container::modal_title_bar)
                .width(Length::Fill)
                .height(Length::Fixed(34.0)),
            container(inner)
                .style(theme::builtins::container::modal_body)
                .width(Length::Fill)
                .height(Length::Fill),
        ])
        .width(Length::Fixed(width))
        .height(Length::Fixed(height))
        .style(theme::builtins::container::modal_container)
        .into()
    }

    /// Perform initial loading task when a modal is first shown (optional).
    /// This allows the connect modal to immediately fetch servers.
    pub fn initial_task(&self) -> Task<Message> {
        match self {
            // The connect modal loads its servers + first-server profiles
            // synchronously in `connect::State::opening`, so it opens fully
            // populated; the only start-up async work is fetching icons whose
            // observed `ICON` value the cache wasn't built from.
            Modal::Connect(state) => state.icon_refresh_task().map(Message::Connect),
            // The layouts modal lists its store synchronously too.
            Modal::Layouts(_) => Task::none(),
            // The review modal renders entirely from the offer it was opened with.
            Modal::PackageUpdate(_) => Task::none(),
        }
    }
}
