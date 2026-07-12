use iced::{Task, advanced::graphics::futures::MaybeSend};

pub struct Update<Message, Event> {
    pub task: Task<Message>,
    pub event: Option<Event>,
}

impl<Message, Event> Update<Message, Event>
where
    Message: Clone + MaybeSend + 'static,
    Event: Clone + MaybeSend + 'static,
{
    #[must_use]
    pub fn none() -> Self {
        Self {
            task: Task::none(),
            event: None,
        }
    }

    #[must_use]
    pub fn with_task(task: Task<Message>) -> Self {
        Self { task, event: None }
    }

    #[must_use]
    pub fn with_event(event: Event) -> Self {
        Self {
            task: Task::none(),
            event: Some(event),
        }
    }

    #[must_use]
    pub fn new(task: Task<Message>, event: Option<Event>) -> Self {
        Self { task, event }
    }

    #[must_use]
    pub fn map_message<T: Clone + MaybeSend + 'static>(
        self,
        f: impl FnMut(Message) -> T + MaybeSend + 'static,
    ) -> Update<T, Event> {
        Update::new(self.task.map(f), self.event)
    }
}

impl<Message, Event> Update<Message, Event>
where
    Message: Clone + MaybeSend + 'static,
    Event: Clone + MaybeSend + 'static + Into<Message>,
{
    pub fn into_task(self) -> Task<Message> {
        Task::from(self)
    }
}

impl<Message> From<Task<Message>> for Update<Message, ()>
where
    Message: Clone + MaybeSend + 'static,
{
    fn from(task: Task<Message>) -> Self {
        Update::new(task, None)
    }
}

impl<Message, Event> From<Update<Message, Event>> for Task<Message>
where
    Message: Clone + MaybeSend + 'static,
    Event: Into<Message>,
{
    fn from(update: Update<Message, Event>) -> Self {
        if let Some(event) = update.event {
            Task::batch([update.task, Task::done(event.into())])
        } else {
            update.task
        }
    }
}
