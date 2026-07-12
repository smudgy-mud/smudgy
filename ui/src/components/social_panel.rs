//! The friends & blocks panel: handle lookup → friend request, incoming /
//! outgoing request lists, the friends list (with an inline unfriend
//! confirmation), and blocks.
//!
//! Embedded as the "Friends" tab of the settings window today; self-contained
//! so it can later be reused as a dockable pane. The host is responsible for
//! the verified-email gate: when the account's email isn't verified (or
//! [`SocialPanel::needs_email_verification`] reports a server-side
//! `EmailNotVerified`), render a gate instead of this panel's `view()`.
//!
//! Enumeration resistance: a sent request always reports "Request sent." —
//! the UI never distinguishes created / duplicate / blocked / nonexistent.
//! Lookup misses are uniform server-side, so relaying "no exact match" leaks
//! nothing beyond what the server already answers.

use iced::widget::{button, column, container, row, rule, space, text, text_input};
use iced::{Alignment, Task};
use smudgy_cloud::cloud_api::{
    BlockView, FriendRequestView, FriendRequests, FriendView, TransferDirection, TransferView,
    UserRef,
};
use smudgy_cloud::{CloudError, Uuid};

use crate::cloud_account::CloudHandles;
use crate::components::cloud_errors::display_error;
use crate::theme::{self, Element as ThemedElement};

const NO_MATCH: &str = "No user with that nickname.";
const NICKNAME_PLACEHOLDER: &str = "nickname";

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    FriendsLoaded(Result<Vec<FriendView>, CloudError>),
    RequestsLoaded(Result<FriendRequests, CloudError>),
    BlocksLoaded(Result<Vec<BlockView>, CloudError>),

    AddHandleChanged(String),
    SendRequestPressed,
    SendLookupFinished(Result<UserRef, CloudError>),
    RequestSent(Result<(), CloudError>),

    AcceptRequest(Uuid),
    AcceptFinished(Result<(), CloudError>),
    CancelRequest(Uuid),
    CancelFinished(Result<(), CloudError>),

    // Ownership-transfer offers (received + offered).
    TransfersLoaded(Result<(Vec<TransferView>, Vec<TransferView>), CloudError>),
    AcceptTransfer(Uuid),
    DeclineTransfer(Uuid),
    CancelTransfer(Uuid),
    TransferActionFinished(Result<(), CloudError>),

    UnfriendPressed(Uuid),
    UnfriendConfirmed(Uuid),
    UnfriendKept,
    UnfriendFinished(Result<(), CloudError>),

    BlockHandleChanged(String),
    BlockPressed,
    BlockConfirmed,
    BlockCancelled,
    BlockLookupFinished(Result<UserRef, CloudError>),
    BlockFinished(Result<(), CloudError>),
    Unblock(Uuid),
    UnblockFinished(Result<(), CloudError>),
}

pub struct SocialPanel {
    cloud: CloudHandles,

    add_nickname_input: String,
    block_nickname_input: String,

    /// `None` = still loading (or never loaded).
    friends: Option<Vec<FriendView>>,
    incoming: Option<Vec<FriendRequestView>>,
    outgoing: Option<Vec<FriendRequestView>>,
    blocks: Option<Vec<BlockView>>,
    /// Ownership-transfer offers addressed to / initiated by the viewer.
    incoming_transfers: Option<Vec<TransferView>>,
    outgoing_transfers: Option<Vec<TransferView>>,

    /// The friend whose Unfriend button is in its "really?" state.
    confirming_unfriend: Option<Uuid>,
    /// The Block button is in its "really?" state.
    confirming_block: bool,

    busy: Option<&'static str>,
    error: Option<String>,
    notice: Option<String>,
    /// The server answered `EmailNotVerified`: the host should show the gate.
    email_unverified: bool,
}

impl SocialPanel {
    pub fn new(cloud: CloudHandles) -> Self {
        Self {
            cloud,
            add_nickname_input: String::new(),
            block_nickname_input: String::new(),
            friends: None,
            incoming: None,
            outgoing: None,
            blocks: None,
            incoming_transfers: None,
            outgoing_transfers: None,
            confirming_unfriend: None,
            confirming_block: false,
            busy: None,
            error: None,
            notice: None,
            email_unverified: false,
        }
    }

    /// Whether any list has loaded yet (used by the host to decide whether a
    /// tab open should trigger [`Self::refresh`]).
    pub fn is_loaded(&self) -> bool {
        self.friends.is_some()
    }

    /// The server rejected a call with `EmailNotVerified` since the last
    /// refresh — the host should render its verify-email gate.
    pub fn needs_email_verification(&self) -> bool {
        self.email_unverified
    }

    /// Reloads friends, requests (both directions), and blocks.
    pub fn refresh(&mut self) -> Task<Message> {
        self.email_unverified = false;
        self.confirming_unfriend = None;
        self.confirming_block = false;
        let friends_client = self.cloud.client.clone();
        let blocks_client = self.cloud.client.clone();
        Task::batch([
            Task::perform(
                async move { friends_client.friends().await },
                Message::FriendsLoaded,
            ),
            self.refresh_requests(),
            self.refresh_transfers(),
            Task::perform(
                async move { blocks_client.blocks().await },
                Message::BlocksLoaded,
            ),
        ])
    }

    fn refresh_requests(&self) -> Task<Message> {
        let client = self.cloud.client.clone();
        Task::perform(
            async move { client.friend_requests().await },
            Message::RequestsLoaded,
        )
    }

    /// Load both directions of live transfer offers in one task.
    fn refresh_transfers(&self) -> Task<Message> {
        let client = self.cloud.client.clone();
        Task::perform(
            async move {
                let received = client.transfers(TransferDirection::Received).await?;
                let offered = client.transfers(TransferDirection::Offered).await?;
                Ok((received, offered))
            },
            Message::TransfersLoaded,
        )
    }

    /// Routes an error to the shared error line — except `EmailNotVerified`,
    /// which flips the panel back to the host's gate view instead.
    fn absorb_error(&mut self, err: &CloudError) {
        if matches!(err, CloudError::EmailNotVerified) {
            self.email_unverified = true;
        } else {
            self.error = Some(display_error(err));
        }
    }

    fn clear_feedback(&mut self) {
        self.error = None;
        self.notice = None;
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => self.refresh(),

            // ===== list loads =====
            Message::FriendsLoaded(result) => {
                match result {
                    Ok(friends) => self.friends = Some(friends),
                    Err(err) => self.absorb_error(&err),
                }
                Task::none()
            }
            Message::RequestsLoaded(result) => {
                match result {
                    Ok(requests) => {
                        self.incoming = Some(requests.incoming);
                        self.outgoing = Some(requests.outgoing);
                    }
                    Err(err) => self.absorb_error(&err),
                }
                Task::none()
            }
            Message::BlocksLoaded(result) => {
                match result {
                    Ok(blocks) => self.blocks = Some(blocks),
                    Err(err) => self.absorb_error(&err),
                }
                Task::none()
            }

            // ===== add a friend =====
            Message::AddHandleChanged(v) => {
                self.add_nickname_input = v;
                Task::none()
            }
            Message::SendRequestPressed => {
                if self.busy.is_some() {
                    return Task::none();
                }
                self.clear_feedback();
                let handle = self.add_nickname_input.trim().to_string();
                if handle.is_empty() {
                    self.error = Some("Enter a nickname.".to_string());
                    return Task::none();
                }
                self.busy = Some("Sending request…");
                let client = self.cloud.client.clone();
                Task::perform(
                    async move { client.lookup(&handle).await },
                    Message::SendLookupFinished,
                )
            }
            Message::SendLookupFinished(result) => match result {
                Ok(user) => {
                    let client = self.cloud.client.clone();
                    Task::perform(
                        async move { client.send_friend_request(user.user_id).await },
                        Message::RequestSent,
                    )
                }
                Err(CloudError::NotFoundOrNoAccess) => {
                    self.busy = None;
                    self.error = Some(NO_MATCH.to_string());
                    Task::none()
                }
                Err(err) => {
                    self.busy = None;
                    self.absorb_error(&err);
                    Task::none()
                }
            },
            Message::RequestSent(result) => {
                self.busy = None;
                match result {
                    Ok(()) => {
                        // Uniform 202: never distinguish created / duplicate /
                        // blocked / nonexistent (enumeration resistance).
                        self.add_nickname_input.clear();
                        self.notice = Some("Request sent.".to_string());
                        self.refresh_requests()
                    }
                    Err(err) => {
                        self.absorb_error(&err);
                        Task::none()
                    }
                }
            }

            // ===== incoming / outgoing requests =====
            Message::AcceptRequest(user_id) => {
                self.clear_feedback();
                let client = self.cloud.client.clone();
                Task::perform(
                    async move { client.accept_friend_request(user_id).await },
                    Message::AcceptFinished,
                )
            }
            Message::AcceptFinished(result) => match result {
                // Uniform 404: the request may have been cancelled on the
                // other end — just resync silently.
                Ok(()) | Err(CloudError::NotFoundOrNoAccess) => self.refresh(),
                Err(err) => {
                    self.absorb_error(&err);
                    Task::none()
                }
            },
            Message::CancelRequest(user_id) => {
                self.clear_feedback();
                let client = self.cloud.client.clone();
                Task::perform(
                    async move { client.cancel_friend_request(user_id).await },
                    Message::CancelFinished,
                )
            }
            Message::CancelFinished(result) => match result {
                Ok(()) => self.refresh(),
                Err(err) => {
                    self.absorb_error(&err);
                    Task::none()
                }
            },

            // ===== transfers =====
            Message::TransfersLoaded(result) => {
                match result {
                    Ok((received, offered)) => {
                        self.incoming_transfers = Some(received);
                        self.outgoing_transfers = Some(offered);
                    }
                    Err(err) => self.absorb_error(&err),
                }
                Task::none()
            }
            Message::AcceptTransfer(id) => {
                self.clear_feedback();
                let client = self.cloud.client.clone();
                Task::perform(
                    async move { client.accept_transfer(id, None, None).await.map(|_| ()) },
                    Message::TransferActionFinished,
                )
            }
            Message::DeclineTransfer(id) => {
                self.clear_feedback();
                let client = self.cloud.client.clone();
                Task::perform(
                    async move { client.decline_transfer(id).await },
                    Message::TransferActionFinished,
                )
            }
            Message::CancelTransfer(id) => {
                self.clear_feedback();
                let client = self.cloud.client.clone();
                Task::perform(
                    async move { client.cancel_transfer(id).await },
                    Message::TransferActionFinished,
                )
            }
            // Uniform 404: the offer may have been responded to / cancelled on the
            // other end — just resync silently.
            Message::TransferActionFinished(result) => match result {
                Ok(()) | Err(CloudError::NotFoundOrNoAccess) => self.refresh(),
                Err(err) => {
                    self.absorb_error(&err);
                    Task::none()
                }
            },

            // ===== friends =====
            Message::UnfriendPressed(user_id) => {
                self.confirming_unfriend = Some(user_id);
                Task::none()
            }
            Message::UnfriendKept => {
                self.confirming_unfriend = None;
                Task::none()
            }
            Message::UnfriendConfirmed(user_id) => {
                self.confirming_unfriend = None;
                self.clear_feedback();
                let client = self.cloud.client.clone();
                Task::perform(
                    async move { client.unfriend(user_id).await },
                    Message::UnfriendFinished,
                )
            }
            Message::UnfriendFinished(result) => match result {
                Ok(()) => self.refresh(),
                Err(err) => {
                    self.absorb_error(&err);
                    Task::none()
                }
            },

            // ===== blocks =====
            Message::BlockHandleChanged(v) => {
                self.block_nickname_input = v;
                Task::none()
            }
            Message::BlockPressed => {
                if self.busy.is_some() {
                    return Task::none();
                }
                self.clear_feedback();
                if self.block_nickname_input.trim().is_empty() {
                    self.error = Some("Enter a nickname.".to_string());
                    return Task::none();
                }
                self.confirming_block = true;
                Task::none()
            }
            Message::BlockCancelled => {
                self.confirming_block = false;
                Task::none()
            }
            Message::BlockConfirmed => {
                self.confirming_block = false;
                if self.busy.is_some() {
                    return Task::none();
                }
                self.clear_feedback();
                let handle = self.block_nickname_input.trim().to_string();
                if handle.is_empty() {
                    return Task::none();
                }
                self.busy = Some("Blocking…");
                let client = self.cloud.client.clone();
                Task::perform(
                    async move { client.lookup(&handle).await },
                    Message::BlockLookupFinished,
                )
            }
            Message::BlockLookupFinished(result) => match result {
                Ok(user) => {
                    let client = self.cloud.client.clone();
                    Task::perform(
                        async move { client.block(user.user_id).await },
                        Message::BlockFinished,
                    )
                }
                Err(CloudError::NotFoundOrNoAccess) => {
                    self.busy = None;
                    self.error = Some(NO_MATCH.to_string());
                    Task::none()
                }
                Err(err) => {
                    self.busy = None;
                    self.absorb_error(&err);
                    Task::none()
                }
            },
            Message::BlockFinished(result) => {
                self.busy = None;
                match result {
                    Ok(()) => {
                        self.block_nickname_input.clear();
                        self.refresh()
                    }
                    Err(err) => {
                        self.absorb_error(&err);
                        Task::none()
                    }
                }
            }
            Message::Unblock(user_id) => {
                self.clear_feedback();
                let client = self.cloud.client.clone();
                Task::perform(
                    async move { client.unblock(user_id).await },
                    Message::UnblockFinished,
                )
            }
            Message::UnblockFinished(result) => match result {
                Ok(()) => self.refresh(),
                Err(err) => {
                    self.absorb_error(&err);
                    Task::none()
                }
            },
        }
    }

    // ===================== views =====================

    pub fn view(&self) -> ThemedElement<'_, Message> {
        let mut col = column![
            row![
                text("Friends").size(20),
                space::horizontal(),
                button(text("Refresh").size(13))
                    .style(theme::builtins::button::secondary)
                    .padding([4, 10])
                    .on_press(Message::Refresh),
            ]
            .align_y(Alignment::Center)
        ]
        .spacing(12);

        col = col.push(self.feedback());

        // ===== add a friend =====
        col = col.push(text("Add a friend").size(15));
        col = col.push(
            row![
                text_input(NICKNAME_PLACEHOLDER, &self.add_nickname_input)
                    .on_input(Message::AddHandleChanged)
                    .on_submit(Message::SendRequestPressed)
                    .width(220),
                button(text("Send request").size(13))
                    .style(theme::builtins::button::primary)
                    .padding([4, 10])
                    .on_press(Message::SendRequestPressed),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );

        col = col.push(rule::horizontal(1));

        // ===== incoming requests =====
        col = col.push(text("Incoming requests").size(15));
        col = match &self.incoming {
            None => col.push(text("Loading…").size(13)),
            Some(incoming) if incoming.is_empty() => {
                col.push(text("No incoming requests.").size(13))
            }
            Some(incoming) => {
                let mut col = col;
                for request in incoming {
                    col = col.push(
                        row![
                            text(nickname_or_fallback(request.nickname.clone())).size(13),
                            space::horizontal(),
                            button(text("Accept").size(12))
                                .style(theme::builtins::button::primary)
                                .padding([2, 8])
                                .on_press(Message::AcceptRequest(request.user_id)),
                            button(text("Decline").size(12))
                                .style(theme::builtins::button::secondary)
                                .padding([2, 8])
                                .on_press(Message::CancelRequest(request.user_id)),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                    );
                }
                col
            }
        };

        // ===== outgoing requests =====
        col = col.push(text("Outgoing requests").size(15));
        col = match &self.outgoing {
            None => col.push(text("Loading…").size(13)),
            Some(outgoing) if outgoing.is_empty() => {
                col.push(text("No outgoing requests.").size(13))
            }
            Some(outgoing) => {
                let mut col = col;
                for request in outgoing {
                    col = col.push(
                        row![
                            text(nickname_or_fallback(request.nickname.clone())).size(13),
                            text("(pending)").size(12),
                            space::horizontal(),
                            button(text("Cancel").size(12))
                                .style(theme::builtins::button::secondary)
                                .padding([2, 8])
                                .on_press(Message::CancelRequest(request.user_id)),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                    );
                }
                col
            }
        };

        // ===== ownership transfers (shown only when offers are live) =====
        let has_transfers = self
            .incoming_transfers
            .as_ref()
            .is_some_and(|t| !t.is_empty())
            || self
                .outgoing_transfers
                .as_ref()
                .is_some_and(|t| !t.is_empty());
        if has_transfers {
            col = col.push(rule::horizontal(1));
            col = col.push(text("Ownership transfers").size(15));
            if let Some(incoming) = &self.incoming_transfers {
                for t in incoming {
                    col = col.push(
                        row![
                            text(format!(
                                "{} wants to give you \u{201c}{}\u{201d}",
                                nickname_or_fallback(t.from_nickname.clone()),
                                t.subject_name.clone().unwrap_or_else(|| t.subject_kind.clone()),
                            ))
                            .size(13),
                            space::horizontal(),
                            button(text("Accept").size(12))
                                .style(theme::builtins::button::primary)
                                .padding([2, 8])
                                .on_press(Message::AcceptTransfer(t.id)),
                            button(text("Decline").size(12))
                                .style(theme::builtins::button::secondary)
                                .padding([2, 8])
                                .on_press(Message::DeclineTransfer(t.id)),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                    );
                }
            }
            if let Some(outgoing) = &self.outgoing_transfers {
                for t in outgoing {
                    col = col.push(
                        row![
                            text(format!(
                                "You offered \u{201c}{}\u{201d} to {}",
                                t.subject_name.clone().unwrap_or_else(|| t.subject_kind.clone()),
                                nickname_or_fallback(t.to_nickname.clone()),
                            ))
                            .size(13),
                            text("(pending)").size(12),
                            space::horizontal(),
                            button(text("Cancel").size(12))
                                .style(theme::builtins::button::secondary)
                                .padding([2, 8])
                                .on_press(Message::CancelTransfer(t.id)),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                    );
                }
            }
        }

        col = col.push(rule::horizontal(1));

        // ===== friends =====
        col = col.push(text("Friends").size(15));
        col = match &self.friends {
            None => col.push(text("Loading…").size(13)),
            Some(friends) if friends.is_empty() => col.push(text("No friends yet.").size(13)),
            Some(friends) => {
                let mut col = col;
                for friend in friends {
                    col = col.push(self.friend_row(friend));
                }
                col
            }
        };

        col = col.push(rule::horizontal(1));

        // ===== blocks =====
        col = col.push(text("Blocks").size(15));
        col = col.push(
            row![
                text_input(NICKNAME_PLACEHOLDER, &self.block_nickname_input)
                    .on_input(Message::BlockHandleChanged)
                    .on_submit(Message::BlockPressed)
                    .width(220),
                button(text("Block").size(13))
                    .style(theme::builtins::button::secondary)
                    .padding([4, 10])
                    .on_press(Message::BlockPressed),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
        if self.confirming_block {
            col = col.push(
                container(
                    column![
                        text(
                            "Blocking is silent — they won't be told. It removes any \
                             friendship and ends all sharing between you, both directions. \
                             Unblocking restores nothing.",
                        )
                        .size(13)
                        .style(theme::builtins::text::danger),
                        row![
                            button(text("Confirm block").size(12))
                                .style(theme::builtins::button::primary)
                                .padding([2, 8])
                                .on_press(Message::BlockConfirmed),
                            button(text("Cancel").size(12))
                                .style(theme::builtins::button::secondary)
                                .padding([2, 8])
                                .on_press(Message::BlockCancelled),
                        ]
                        .spacing(8),
                    ]
                    .spacing(8),
                )
                .padding(10)
                .style(theme::builtins::container::modal_body),
            );
        }
        col = match &self.blocks {
            None => col.push(text("Loading…").size(13)),
            Some(blocks) if blocks.is_empty() => col.push(text("No blocked users.").size(13)),
            Some(blocks) => {
                let mut col = col;
                for block in blocks {
                    col = col.push(
                        row![
                            text(nickname_or_fallback(block.nickname.clone())).size(13),
                            space::horizontal(),
                            button(text("Unblock").size(12))
                                .style(theme::builtins::button::secondary)
                                .padding([2, 8])
                                .on_press(Message::Unblock(block.user_id)),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                    );
                }
                col
            }
        };
        col = col.push(
            text("Blocked users can't see this entry; unblocking never restores shares.").size(11),
        );

        col.into()
    }

    fn friend_row(&self, friend: &FriendView) -> ThemedElement<'_, Message> {
        let info = row![
            text(nickname_or_fallback(friend.nickname.clone())).size(13),
            text(format!("since {}", friend.since.format("%Y-%m-%d"))).size(12),
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        if self.confirming_unfriend == Some(friend.user_id) {
            column![
                info,
                container(
                    column![
                        text(
                            "Really unfriend? This ends all map sharing between you, \
                             both directions.",
                        )
                        .size(13)
                        .style(theme::builtins::text::danger),
                        row![
                            button(text("Confirm").size(12))
                                .style(theme::builtins::button::primary)
                                .padding([2, 8])
                                .on_press(Message::UnfriendConfirmed(friend.user_id)),
                            button(text("Keep").size(12))
                                .style(theme::builtins::button::secondary)
                                .padding([2, 8])
                                .on_press(Message::UnfriendKept),
                        ]
                        .spacing(8),
                    ]
                    .spacing(8),
                )
                .padding(10)
                .style(theme::builtins::container::modal_body),
            ]
            .spacing(6)
            .into()
        } else {
            row![
                info,
                space::horizontal(),
                button(text("Unfriend").size(12))
                    .style(theme::builtins::button::secondary)
                    .padding([2, 8])
                    .on_press(Message::UnfriendPressed(friend.user_id)),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
        }
    }

    fn feedback(&self) -> ThemedElement<'_, Message> {
        let mut col = column![].spacing(6);
        if let Some(busy) = self.busy {
            col = col.push(text(busy).size(13));
        }
        if let Some(error) = &self.error {
            col = col.push(text(error).size(13).style(theme::builtins::text::danger));
        }
        if let Some(notice) = &self.notice {
            col = col.push(text(notice).size(13).style(theme::builtins::text::success));
        }
        col.into()
    }
}

fn nickname_or_fallback(nickname: Option<String>) -> String {
    nickname.unwrap_or_else(|| "(no nickname)".to_string())
}
