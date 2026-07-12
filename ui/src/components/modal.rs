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

/// Enum representing the currently active modal.
#[derive(Debug)]
pub enum Modal {
    Connect(connect::State),
}

/// Messages that can be sent to the active modal.
#[derive(Debug, Clone)]
pub enum Message {
    Connect(ConnectMessage),
}

/// Events that can be emitted by the active modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Connect(ConnectEvent),
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
        }
    }

    /// Get the view for the active modal.
    pub fn view(&self) -> Element<'_, Message> {
        let inner = match self {
            Modal::Connect(state) => connect::view(state).map(Message::Connect),
        };

        let title_bar_text = text("Connect")
            .center()
            .width(Length::Fill)
            .height(Length::Fixed(34.0));

        container(column![
            container(row![title_bar_text])
                .style(theme::builtins::container::modal_title_bar)
                .width(Length::Fill)
                .height(Length::Fixed(34.0)),
            container(inner).style(theme::builtins::container::modal_body),
        ])
        .width(Length::Fixed(800.0))
        .height(Length::Fixed(600.0))
        .style(theme::builtins::container::modal_container)
        .into()
    }

    /// Perform initial loading task when a modal is first shown (optional).
    /// This allows the connect modal to immediately fetch servers.
    pub fn initial_task(&self) -> Task<Message> {
        match self {
            // The connect modal loads its servers + first-server profiles
            // synchronously in `connect::State::opening`, so it opens fully
            // populated and needs no initial async task.
            Modal::Connect(_) => Task::none(),
        }
    }
}
