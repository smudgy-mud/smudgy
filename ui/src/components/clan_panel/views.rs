//! The clans panel's views: the list of clans, a clan's page with its four
//! tabs, and its dialogs.

use iced::widget::{
    button, checkbox, column, container, pick_list, row, rule, space, text, text_input,
};
use iced::{Alignment, Color, Length};
use smudgy_cloud::Uuid;
use smudgy_cloud::clan_access::{GrantThrough, IndexedResource, OutsideShare, ResourceGrant};
use smudgy_cloud::clan_secrets::{ClanSecretGrant, SecretRecipient};
use smudgy_cloud::clans::{ClanGrant, ClanGroup, ClanMember, GrantRecipient, GrantScope, action};

use super::editor::{self, EditorContext, scope_parts};
use super::{
    AccessTab, Assign, ClanChoice, ClanPage, ClanPanel, Confirm, GroupTab, MEMBERS_PER_PAGE,
    Message, Modal, Tab, can_remove, can_revoke_invitation, filtered, group_palette, is_last_owner,
    nickname_or_fallback,
};
use crate::presets::{self, Chip, ScopeKind};
use crate::theme::{self, Element as ThemedElement};

impl ClanPanel {
    pub fn view(&self) -> ThemedElement<'_, Message> {
        match &self.open {
            Some(page) => self.page_view(page),
            None => self.list_view(),
        }
    }

    /// The open dialog, which the host lays over its whole page.
    pub fn modal_view(&self) -> Option<ThemedElement<'_, Message>> {
        let page = self.open.as_ref()?;
        let modal = page.modal.as_ref()?;
        Some(self.dialog(page, modal))
    }

    // ===================== the list =====================

    /// The Secret ownership offers naming the caller, each with Accept and
    /// Decline. A joint offer names the others it would make owners with
    /// the caller; once the caller accepts, it waits for them.
    fn offers_view(&self) -> ThemedElement<'_, Message> {
        let me = self.me();
        let mut col = column![
            row![
                text(crate::i18n::t!("clans-secret-offers")).size(15),
                badge(self.offers.len().to_string()),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
        ]
        .spacing(8);
        for offer in &self.offers {
            let clan = self
                .clans
                .iter()
                .flatten()
                .find(|clan| clan.id == offer.clan_id)
                .map(|clan| clan.name.clone())
                .unwrap_or_default();
            let others: Vec<String> = offer
                .recipients
                .iter()
                .filter(|recipient| Some(recipient.user_id) != me)
                .map(|recipient| nickname_or_fallback(recipient.nickname.clone()))
                .collect();
            let waiting: Vec<String> = offer
                .recipients
                .iter()
                .filter(|recipient| Some(recipient.user_id) != me && !recipient.accepted)
                .map(|recipient| nickname_or_fallback(recipient.nickname.clone()))
                .collect();
            let accepted = me.is_some_and(|me| offer.accepted_by(me));
            let what = match offer.initiator_nickname.clone() {
                Some(initiator) => crate::i18n::t!(
                    "clans-secret-offer",
                    "initiator" => initiator,
                    "secret" => offer.secret_name.clone(),
                    "clan" => clan
                ),
                None => crate::i18n::t!(
                    "clans-secret-offer-from-a-member",
                    "secret" => offer.secret_name.clone(),
                    "clan" => clan
                ),
            };
            let mut about = column![text(what).size(13)].spacing(2).width(Length::Fill);
            if accepted {
                about = about.push(muted(crate::i18n::t!(
                    "clans-secret-offer-accepted",
                    "others" => waiting.join(&crate::i18n::t!("mapper-multi-list-separator"))
                )));
            } else if !others.is_empty() {
                about = about.push(muted(crate::i18n::t!(
                    "clans-secret-offer-joint",
                    "others" => others.join(&crate::i18n::t!("mapper-multi-list-separator"))
                )));
            }
            let mut line = row![about].spacing(8).align_y(Alignment::Center);
            if !accepted {
                line = line.push(small_button(
                    crate::i18n::t!("social-accept"),
                    theme::builtins::button::primary,
                    Message::AcceptOffer(offer.secret_id, offer.id),
                ));
            }
            line = line.push(small_button(
                crate::i18n::t!("social-decline"),
                theme::builtins::button::secondary,
                Message::DeclineOffer(offer.secret_id, offer.id),
            ));
            col = col.push(line);
        }
        col.into()
    }

    fn list_view(&self) -> ThemedElement<'_, Message> {
        let mut col = column![
            row![
                text(crate::i18n::t!("clans-title")).size(20),
                space::horizontal(),
                refresh_button(),
            ]
            .align_y(Alignment::Center),
            self.feedback(),
            row![
                text_input(
                    crate::i18n::ts!("clans-name-placeholder"),
                    &self.new_clan_name
                )
                .on_input(Message::NewClanNameChanged)
                .on_submit(Message::CreateClan)
                .width(220),
                button(text(crate::i18n::t!("action-create")).size(13))
                    .style(theme::builtins::button::primary)
                    .padding([4, 10])
                    .on_press(Message::CreateClan),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(12);

        if !self.invitations.is_empty() {
            col = col.push(rule::horizontal(1));
            col = col.push(text(crate::i18n::t!("clans-invitations")).size(15));
            for invitation in &self.invitations {
                col = col.push(
                    row![
                        text(match invitation.inviter_nickname.clone() {
                            Some(inviter) => crate::i18n::t!(
                                "clans-invited-you",
                                "inviter" => inviter,
                                "clan" => invitation.clan_name.clone()
                            ),
                            None => crate::i18n::t!(
                                "clans-invited-you-by-a-member",
                                "clan" => invitation.clan_name.clone()
                            ),
                        })
                        .size(13),
                        space::horizontal(),
                        small_button(
                            crate::i18n::t!("social-accept"),
                            theme::builtins::button::primary,
                            Message::AcceptInvitation(invitation.id),
                        ),
                        small_button(
                            crate::i18n::t!("social-decline"),
                            theme::builtins::button::secondary,
                            Message::DeclineInvitation(invitation.id),
                        ),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                );
            }
        }

        if !self.offers.is_empty() {
            col = col.push(rule::horizontal(1));
            col = col.push(self.offers_view());
        }
        if !self.map_offers.offers.is_empty() || self.map_offers.error.is_some() {
            col = col.push(rule::horizontal(1));
            let clans = self.clans.as_deref().unwrap_or_default();
            let account = self.account();
            col = col.push(
                self.map_offers
                    .view(self.me(), |clan_id| {
                        clans
                            .iter()
                            .find(|clan| clan.id == clan_id)
                            .map(|clan| clan.name.clone())
                            .unwrap_or_default()
                    })
                    .map(move |message| Message::MapOffers(account, message)),
            );
        }

        col = col.push(rule::horizontal(1));
        col = match &self.clans {
            None => col.push(text(crate::i18n::t!("state-loading")).size(13)),
            Some(clans) if clans.is_empty() => {
                col.push(text(crate::i18n::t!("clans-empty")).size(13))
            }
            Some(clans) => {
                let mut list = column![].spacing(2);
                for clan in clans {
                    let mut line = row![text(clan.name.clone()).size(14)]
                        .spacing(10)
                        .align_y(Alignment::Center);
                    if clan.is_owner {
                        line = line.push(badge(crate::i18n::t!("clans-owner-badge")));
                    }
                    line = line.push(muted(crate::i18n::t!(
                        "clans-member-count",
                        "count" => clan.member_count
                    )));
                    list = list.push(
                        button(line)
                            .style(theme::builtins::button::list_item)
                            .width(Length::Fill)
                            .padding([6, 10])
                            .on_press(Message::OpenClan(clan.id)),
                    );
                }
                col.push(list)
            }
        };
        col.into()
    }

    // ===================== a clan's page =====================

    fn page_view<'a>(&'a self, page: &'a ClanPage) -> ThemedElement<'a, Message> {
        let choices: Vec<ClanChoice> = self
            .clans
            .iter()
            .flatten()
            .map(|clan| ClanChoice {
                id: clan.id,
                name: clan.name.clone(),
            })
            .collect();
        let current = ClanChoice {
            id: page.clan.id,
            name: page.clan.name.clone(),
        };
        let switcher: ThemedElement<'a, Message> =
            pick_list(choices, Some(current), Message::SwitchClan)
                .text_size(20)
                .padding([4, 10])
                .into();
        let header = row![
            switcher,
            space::horizontal(),
            refresh_button(),
            button(text(crate::i18n::t!("clans-all-clans")).size(14))
                .style(theme::builtins::button::quiet_link)
                .padding([4, 8])
                .on_press(Message::CloseClan),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let tabs = row![
            tab_button(
                crate::i18n::t!("clans-tab-members"),
                page.tab == Tab::Members,
                Message::TabSelected(Tab::Members)
            ),
            tab_button(
                crate::i18n::t!("clans-tab-groups"),
                page.tab == Tab::Groups,
                Message::TabSelected(Tab::Groups)
            ),
            tab_button(
                crate::i18n::t!("clans-tab-access"),
                page.tab == Tab::Access,
                Message::TabSelected(Tab::Access)
            ),
            tab_button(
                crate::i18n::t!("clans-tab-settings"),
                page.tab == Tab::Settings,
                Message::TabSelected(Tab::Settings)
            ),
        ]
        .spacing(4);

        let mut col = column![header, tabs, rule::horizontal(1), self.feedback()].spacing(12);
        if page.loading {
            return col
                .push(text(crate::i18n::t!("state-loading")).size(13))
                .into();
        }
        let me = self.me();
        col = col.push(match page.tab {
            Tab::Members => self.members_tab(page, me),
            Tab::Groups => self.groups_tab(page, me),
            Tab::Access => self.access_tab(page),
            Tab::Settings => self.settings_tab(page, me),
        });
        col.into()
    }

    // ===================== Members =====================

    fn members_tab<'a>(
        &'a self,
        page: &'a ClanPage,
        me: Option<Uuid>,
    ) -> ThemedElement<'a, Message> {
        let clan = &page.clan;
        let mut heading = row![
            text(crate::i18n::t!("clans-members")).size(16),
            muted(clan.member_count.to_string()),
            space::horizontal(),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if clan.can(action::INVITE) {
            heading = heading.push(
                button(text(crate::i18n::t!("clans-invite")).size(13))
                    .style(theme::builtins::button::primary)
                    .padding([5, 14])
                    .on_press(Message::OpenInvite),
            );
        }
        let mut col = column![heading].spacing(10);

        match &page.members {
            Some(members) => {
                col = col.push(
                    row![
                        muted(crate::i18n::t!("clans-column-member")).width(160),
                        muted(crate::i18n::t!("clans-column-groups")),
                    ]
                    .spacing(10),
                );
                let start = page.member_page * MEMBERS_PER_PAGE;
                for member in members.iter().skip(start).take(MEMBERS_PER_PAGE) {
                    col = col.push(rule::horizontal(1));
                    col = col.push(self.member_row(page, member, me));
                }
                if members.len() > MEMBERS_PER_PAGE {
                    let end = (start + MEMBERS_PER_PAGE).min(members.len());
                    let mut paging = row![muted(crate::i18n::t!(
                        "clans-page-range",
                        "first" => start + 1,
                        "last" => end,
                        "total" => members.len()
                    ))]
                    .spacing(10)
                    .align_y(Alignment::Center);
                    paging = paging.push(space::horizontal());
                    let previous = button(text(crate::i18n::t!("clans-page-previous")).size(13))
                        .style(theme::builtins::button::quiet_link)
                        .padding([2, 6]);
                    paging = paging.push(if page.member_page > 0 {
                        previous.on_press(Message::MembersPage(page.member_page - 1))
                    } else {
                        previous
                    });
                    let next = button(text(crate::i18n::t!("clans-page-next")).size(13))
                        .style(theme::builtins::button::quiet_link)
                        .padding([2, 6]);
                    paging = paging.push(if end < members.len() {
                        next.on_press(Message::MembersPage(page.member_page + 1))
                    } else {
                        next
                    });
                    col = col.push(paging);
                }
            }
            None => {
                col = col.push(muted(crate::i18n::t!("clans-members-hidden")));
                if let Some(me) = me {
                    col = col.push(rule::horizontal(1));
                    let mut chips = row![].spacing(6);
                    if clan.is_owner {
                        chips = chips.push(owner_chip());
                    }
                    for group in page.custom_groups().filter(|group| group.is_member) {
                        chips = chips.push(group_chip(group));
                    }
                    let mut line = row![
                        column![
                            text(self.my_nickname()).size(14),
                            muted(crate::i18n::t!("clans-you")),
                        ]
                        .width(160),
                        chips.wrap(),
                        space::horizontal(),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center);
                    if page
                        .custom_groups()
                        .any(|group| page.assignable(group, me, me) != Assign::No)
                    {
                        line = line.push(link_button(
                            crate::i18n::t!("clans-edit-groups"),
                            Message::EditMemberGroups(me),
                        ));
                    }
                    col = col.push(line);
                }
            }
        }

        let invited: Vec<_> = page.pending.iter().collect();
        if !invited.is_empty() {
            col = col.push(space::vertical().height(8));
            col = col.push(text(crate::i18n::t!("clans-invited")).size(16));
            for invitation in invited {
                let mut chips = row![].spacing(6);
                for group in invitation.group_ids.iter().filter_map(|id| page.group(*id)) {
                    chips = chips.push(group_chip(group));
                }
                chips = chips.push(badge(crate::i18n::t!("clans-pending-badge")));
                let mut line = row![
                    column![
                        text(nickname_or_fallback(invitation.nickname.clone())).size(14),
                        chips.wrap(),
                    ]
                    .spacing(4),
                    space::horizontal(),
                ]
                .spacing(10)
                .align_y(Alignment::Center);
                if me.is_some_and(|me| can_revoke_invitation(clan, invitation, me)) {
                    line = line.push(link_button(
                        crate::i18n::t!("clans-revoke"),
                        Message::RevokeInvitation(invitation.id),
                    ));
                }
                col = col.push(rule::horizontal(1));
                col = col.push(line);
            }
        }
        col.into()
    }

    fn member_row<'a>(
        &'a self,
        page: &'a ClanPage,
        member: &'a ClanMember,
        me: Option<Uuid>,
    ) -> ThemedElement<'a, Message> {
        let clan = &page.clan;
        let mut name = column![text(nickname_or_fallback(member.nickname.clone())).size(14)];
        if Some(member.user_id) == me {
            name = name.push(muted(crate::i18n::t!("clans-you")));
        }
        let mut chips = row![].spacing(6);
        if member.is_owner {
            chips = chips.push(owner_chip());
        }
        for group in member.group_ids.iter().filter_map(|id| page.group(*id)) {
            chips = chips.push(group_chip(group));
        }
        let mut line = row![name.width(160), chips.wrap(), space::horizontal()]
            .spacing(10)
            .align_y(Alignment::Center);
        let editable = me.is_some_and(|me| {
            clan.is_owner
                || can_remove(clan, member, me)
                || page
                    .custom_groups()
                    .any(|group| page.assignable(group, member.user_id, me) != Assign::No)
        });
        if editable {
            line = line.push(link_button(
                crate::i18n::t!("clans-edit-groups"),
                Message::EditMemberGroups(member.user_id),
            ));
        }
        line.into()
    }

    fn my_nickname(&self) -> String {
        self.cloud
            .snapshot
            .get()
            .profile
            .as_ref()
            .and_then(|profile| profile.nickname.clone())
            .unwrap_or_else(|| crate::i18n::t!("social-no-nickname"))
    }

    // ===================== Groups =====================

    fn groups_tab<'a>(
        &'a self,
        page: &'a ClanPage,
        me: Option<Uuid>,
    ) -> ThemedElement<'a, Message> {
        let mut heading = row![
            text(crate::i18n::t!("clans-groups")).size(16),
            space::horizontal()
        ]
        .align_y(Alignment::Center);
        if page.clan.can(action::CREATE_GROUP) {
            heading = heading.push(
                button(text(crate::i18n::t!("clans-new-group")).size(13))
                    .style(theme::builtins::button::primary)
                    .padding([5, 14])
                    .on_press(Message::OpenNewGroup),
            );
        }
        let mut list = column![].spacing(4).width(220);
        for group in &page.groups {
            let selected = page.selected_group == Some(group.id);
            list = list.push(
                button(group_chip(group))
                    .style(if selected {
                        theme::builtins::button::list_item_selected
                    } else {
                        theme::builtins::button::list_item
                    })
                    .width(Length::Fill)
                    .padding([6, 8])
                    .on_press(Message::SelectGroup(group.id)),
            );
        }
        let detail: ThemedElement<'a, Message> =
            match page.selected_group.and_then(|id| page.group(id)) {
                Some(group) => self.group_detail(page, group, me),
                None => muted(crate::i18n::t!("clans-no-groups")).into(),
            };
        column![
            heading,
            row![
                list,
                container(detail)
                    .width(Length::Fill)
                    .padding(14)
                    .style(theme::builtins::container::card)
            ]
            .spacing(16),
        ]
        .spacing(12)
        .into()
    }

    fn group_detail<'a>(
        &'a self,
        page: &'a ClanPage,
        group: &'a ClanGroup,
        me: Option<Uuid>,
    ) -> ThemedElement<'a, Message> {
        let mut title =
            row![text(group.name.clone()).size(18), space::horizontal()].align_y(Alignment::Center);
        if group.can(action::RENAME_GROUP) || group.can(action::DELETE_GROUP) {
            title = title.push(link_button(
                crate::i18n::t!("clans-edit-group"),
                Message::OpenEditGroup(group.id),
            ));
        }
        let tabs = row![
            tab_button(
                crate::i18n::t!("clans-group-permissions"),
                page.group_tab == GroupTab::Permissions,
                Message::GroupTabSelected(GroupTab::Permissions),
            ),
            tab_button(
                crate::i18n::t!("clans-group-members"),
                page.group_tab == GroupTab::Members,
                Message::GroupTabSelected(GroupTab::Members),
            ),
        ]
        .spacing(4);
        let body = match page.group_tab {
            GroupTab::Permissions => self.group_permissions(page, group),
            GroupTab::Members => self.group_members(page, group, me),
        };
        let mut detail = column![title];
        if group.builtin.as_deref() == Some("owners") {
            detail = detail.push(muted(crate::i18n::t!("clans-group-owners-note")));
        }
        detail
            .push(tabs)
            .push(rule::horizontal(1))
            .push(body)
            .spacing(10)
            .into()
    }

    fn context<'a>(&self, page: &'a ClanPage) -> EditorContext<'a> {
        EditorContext {
            groups: &page.groups,
            members: page.members.as_deref(),
            folders: &page.folders,
            maps: &page.maps,
            packages: &page.packages,
            grants: &page.grants,
            owner: page.clan.is_owner,
        }
    }

    fn group_permissions<'a>(
        &'a self,
        page: &'a ClanPage,
        group: &'a ClanGroup,
    ) -> ThemedElement<'a, Message> {
        let context = self.context(page);
        let mut col = column![].spacing(10);
        let grants = page.grants_to(group.id);
        if group.builtin.as_deref() == Some("owners") {
            col = col.push(muted(crate::i18n::t!("permissions-owners-implicit")));
        } else if grants.is_empty() {
            col = col.push(muted(crate::i18n::t!("clans-group-no-permissions")));
        }
        for grant in grants {
            col = col.push(self.grant_row(page, &context, grant));
            col = col.push(rule::horizontal(1));
        }
        {
            let buttons = row![
                button(text(crate::i18n::t!("permissions-manage")).size(13))
                    .style(theme::builtins::button::secondary)
                    .padding([5, 12])
                    .on_press(Message::AddPermission),
            ]
            .spacing(8);
            col = col.push(buttons);
        }
        if !page.can_grant() {
            col = col.push(muted(crate::i18n::t!("clans-group-permissions-view-only")));
        }
        col.into()
    }

    /// One grant as a group's permission: its scope, chips and a note, with
    /// Edit and Remove for those who may change it.
    fn grant_row<'a>(
        &'a self,
        page: &'a ClanPage,
        context: &EditorContext<'_>,
        grant: &'a ClanGrant,
    ) -> ThemedElement<'a, Message> {
        let (kind, _) = scope_parts(&grant.scope);
        let mut title = row![
            text(context.scope_label(&grant.scope)).size(14),
            badge(scope_kind_label(kind, &grant.scope)),
            space::horizontal(),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if page.can_change(grant) {
            title = title.push(link_button(
                crate::i18n::t!("action-edit"),
                Message::EditGrant(grant.id),
            ));
            title = title.push(link_button(
                crate::i18n::t!("action-remove"),
                Message::RemoveGrantPressed(grant.id),
            ));
        }
        let mut col = column![
            title,
            text(
                grant
                    .actions
                    .iter()
                    .map(|action| presets::action_label(action))
                    .collect::<Vec<_>>()
                    .join(" · ")
            )
            .size(12)
        ]
        .spacing(6);
        for note in grant_notes(grant) {
            col = col.push(muted(note));
        }
        if page.confirm == Some(Confirm::RemoveGrant(grant.id)) {
            col = col.push(confirm_box(
                crate::i18n::t!("clans-remove-permission-confirm"),
                crate::i18n::t!("action-remove"),
                Message::RemoveGrantConfirmed(grant.id),
            ));
        }
        col.into()
    }

    fn group_members<'a>(
        &'a self,
        page: &'a ClanPage,
        group: &'a ClanGroup,
        me: Option<Uuid>,
    ) -> ThemedElement<'a, Message> {
        let mut col = column![].spacing(8);
        if !group.can(action::INSPECT_GROUP) {
            return col
                .push(muted(crate::i18n::t!("clans-group-roster-hidden")))
                .into();
        }
        let roster = match &page.roster {
            Some((id, roster)) if *id == group.id => roster,
            _ => {
                return col.push(muted(crate::i18n::t!("state-loading"))).into();
            }
        };
        let rows = match roster {
            Ok(rows) => rows,
            Err(error) => {
                return col
                    .push(
                        text(error.clone())
                            .size(13)
                            .style(theme::builtins::text::danger),
                    )
                    .into();
            }
        };
        let mut heading = row![
            muted(crate::i18n::t!("clans-shown-count", "count" => rows.len())),
            space::horizontal(),
        ]
        .align_y(Alignment::Center);
        if !group.is_builtin() && group.can(action::ASSIGN_GROUP) && page.members.is_some() {
            heading = heading.push(
                button(text(crate::i18n::t!("clans-add-member")).size(13))
                    .style(theme::builtins::button::secondary)
                    .padding([5, 12])
                    .on_press(Message::OpenAddGroupMember),
            );
        }
        col = col.push(heading);
        for user in rows {
            let mut line = row![
                text(nickname_or_fallback(user.nickname.clone())).size(14),
                space::horizontal(),
            ]
            .align_y(Alignment::Center);
            if me.is_some_and(|me| page.assignable(group, user.user_id, me) == Assign::Yes) {
                line = line.push(link_button(
                    crate::i18n::t!("action-remove"),
                    Message::RemoveGroupMember(user.user_id),
                ));
            }
            col = col.push(line);
            col = col.push(rule::horizontal(1));
        }
        col.into()
    }

    // ===================== Access =====================

    fn access_tab<'a>(&'a self, page: &'a ClanPage) -> ThemedElement<'a, Message> {
        let tabs = row![
            tab_button(
                crate::i18n::t!("clans-access-maps"),
                page.access_tab == AccessTab::Maps,
                Message::AccessTabSelected(AccessTab::Maps),
            ),
            tab_button(
                crate::i18n::t!("clans-access-packages"),
                page.access_tab == AccessTab::Packages,
                Message::AccessTabSelected(AccessTab::Packages),
            ),
            tab_button(
                crate::i18n::t!("clans-access-secrets"),
                page.access_tab == AccessTab::Secrets,
                Message::AccessTabSelected(AccessTab::Secrets),
            ),
        ]
        .spacing(4);
        let (list, detail) = match page.access_tab {
            AccessTab::Maps => (self.map_list(page), self.map_detail(page)),
            AccessTab::Packages => (self.package_list(page), self.package_detail(page)),
            AccessTab::Secrets => (self.secret_list(page), self.secret_detail(page)),
        };
        column![
            tabs,
            rule::horizontal(1),
            row![
                container(list).width(240),
                container(detail)
                    .width(Length::Fill)
                    .padding(14)
                    .style(theme::builtins::container::card),
            ]
            .spacing(16),
        ]
        .spacing(12)
        .into()
    }

    fn resource_button<'a>(
        page: &'a ClanPage,
        row_: &'a IndexedResource,
        subtitle: String,
        indent: u16,
    ) -> ThemedElement<'a, Message> {
        let selected = page.selected_resource == Some(row_.id);
        container(
            button(column![text(row_.name.clone()).size(14), muted(subtitle)].spacing(2))
                .style(if selected {
                    theme::builtins::button::list_item_selected
                } else {
                    theme::builtins::button::list_item
                })
                .width(Length::Fill)
                .padding([6, 10])
                .on_press(Message::SelectResource(row_.id)),
        )
        .padding(iced::Padding {
            left: f32::from(indent),
            ..iced::Padding::ZERO
        })
        .into()
    }

    fn map_list<'a>(&'a self, page: &'a ClanPage) -> ThemedElement<'a, Message> {
        let mut col = column![].spacing(2);
        let maps: Vec<&IndexedResource> = page.maps.iter().filter(|row| row.clan_owned()).collect();
        let mut shown = Vec::new();
        for folder in page.folders.iter().filter(|row| row.clan_owned()) {
            col = col.push(Self::resource_button(
                page,
                folder,
                crate::i18n::t!("clans-kind-folder"),
                0,
            ));
            for map in maps
                .iter()
                .filter(|map| map.atlas_id.map(|id| id.0) == Some(folder.id))
            {
                shown.push(map.id);
                col = col.push(Self::resource_button(
                    page,
                    map,
                    crate::i18n::t!("clans-kind-map"),
                    16,
                ));
            }
        }
        for map in maps.iter().filter(|map| !shown.contains(&map.id)) {
            col = col.push(Self::resource_button(
                page,
                map,
                crate::i18n::t!("clans-kind-map"),
                0,
            ));
        }
        if page.folders.is_empty() && maps.is_empty() {
            col = col.push(muted(crate::i18n::t!("clans-access-no-maps")));
        }
        col.into()
    }

    fn map_detail<'a>(&'a self, page: &'a ClanPage) -> ThemedElement<'a, Message> {
        let Some(row_) = page.selected_resource.and_then(|id| {
            page.maps
                .iter()
                .chain(&page.folders)
                .find(|row| row.id == id && row.clan_owned())
        }) else {
            return muted(crate::i18n::t!("clans-access-pick")).into();
        };
        let folder = row_.kind == "atlas";
        let mut title = row![
            text(row_.name.clone()).size(18),
            badge(if folder {
                crate::i18n::t!("clans-kind-folder")
            } else {
                crate::i18n::t!("clans-kind-map")
            }),
            space::horizontal(),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if !folder {
            title = title.push(link_button(
                crate::i18n::t!("clans-open-map"),
                Message::OpenMap(smudgy_cloud::AreaId(row_.id)),
            ));
            title = title.push(link_button(
                crate::i18n::t!("clans-open-share"),
                Message::OpenMapAccess(smudgy_cloud::AreaId(row_.id)),
            ));
        }
        let mut col = column![title].spacing(10);
        if !folder
            && let Some(atlas) = row_.atlas_id
            && let Some(name) = page
                .folders
                .iter()
                .find(|f| f.id == atlas.0)
                .map(|f| f.name.clone())
        {
            col = col.push(muted(crate::i18n::t!("clans-in-folder", "folder" => name)));
        }
        let scope = if folder {
            ScopeKind::Folders
        } else {
            ScopeKind::Maps
        };
        col = col.push(self.assignments(page, row_, scope));
        if let Some(shares) = &row_.outside_shares {
            col = col.push(self.outside_shares(page, shares));
        }
        col.into()
    }

    /// A resource's group assignments from the access index, or the
    /// caller's own access where they may not inspect the others.
    fn assignments<'a>(
        &'a self,
        page: &'a ClanPage,
        row_: &'a IndexedResource,
        scope: ScopeKind,
    ) -> ThemedElement<'a, Message> {
        let context = self.context(page);
        let mut col =
            column![text(crate::i18n::t!("clans-group-assignments")).size(15)].spacing(10);
        match &row_.grants {
            Some(grants) => {
                if grants.is_empty() {
                    col = col.push(muted(crate::i18n::t!("clans-no-assignments")));
                }
                for assignment in grants {
                    col = col.push(self.assignment_row(page, &context, assignment));
                    col = col.push(rule::horizontal(1));
                }
            }
            None => {
                col = col.push(muted(crate::i18n::t!("clans-your-access")));
                col = col.push(action_chips(row_.actions.iter().map(String::as_str)));
            }
        }
        if page.can_grant() {
            col = col.push(
                button(text(crate::i18n::t!("clans-assign-group")).size(13))
                    .style(theme::builtins::button::primary)
                    .padding([5, 14])
                    .on_press(Message::AssignGroup(scope, row_.id)),
            );
        }
        col.into()
    }

    fn assignment_row<'a>(
        &'a self,
        page: &'a ClanPage,
        context: &EditorContext<'_>,
        assignment: &'a ResourceGrant,
    ) -> ThemedElement<'a, Message> {
        let grant = page
            .grants
            .iter()
            .find(|grant| grant.id == assignment.grant_id);
        let mut title = row![
            recipient_chip(page, context, assignment.recipient),
            space::horizontal()
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if let Some(grant) = grant.filter(|grant| page.can_change(grant)) {
            title = title.push(link_button(
                crate::i18n::t!("action-edit"),
                Message::EditGrant(grant.id),
            ));
            if assignment.through == GrantThrough::Direct {
                title = title.push(link_button(
                    crate::i18n::t!("action-remove"),
                    Message::RemoveGrantPressed(grant.id),
                ));
            }
        }
        let mut col = column![
            title,
            action_chips(assignment.actions.iter().map(String::as_str))
        ]
        .spacing(6);
        match assignment.through {
            GrantThrough::Clan => col = col.push(muted(crate::i18n::t!("clans-through-clan"))),
            GrantThrough::Atlas => {
                let through = assignment
                    .atlas_id
                    .and_then(|atlas| page.folders.iter().find(|row| row.id == atlas.0))
                    .map_or_else(
                        || crate::i18n::t!("clans-through-a-folder"),
                        |row| crate::i18n::t!("clans-through-folder", "folder" => row.name.clone()),
                    );
                col = col.push(muted(through));
            }
            GrantThrough::Direct | GrantThrough::Other => {}
        }
        if let Some(grant) = grant {
            for note in grant_notes(grant)
                .into_iter()
                .filter(|_| grant.may_grant.is_some())
            {
                col = col.push(muted(note));
            }
            if page.confirm == Some(Confirm::RemoveGrant(grant.id)) {
                col = col.push(confirm_box(
                    crate::i18n::t!("clans-remove-permission-confirm"),
                    crate::i18n::t!("action-remove"),
                    Message::RemoveGrantConfirmed(grant.id),
                ));
            }
        }
        col.into()
    }

    /// A Clan-owned map's outside shares: who it is shared with outside the
    /// clan, by whom, with Revoke for the sharer and the clan's owners.
    fn outside_shares<'a>(
        &'a self,
        page: &'a ClanPage,
        shares: &'a [OutsideShare],
    ) -> ThemedElement<'a, Message> {
        let me = self.me();
        let mut col = column![
            text(crate::i18n::t!("clans-outside-shares", "clan" => page.clan.name.clone()))
                .size(15),
            muted(crate::i18n::t!("clans-outside-shares-note")),
        ]
        .spacing(8);
        if shares.is_empty() {
            col = col.push(muted(crate::i18n::t!("clans-outside-shares-none")));
        }
        for share in shares {
            let mut line = row![
                column![
                    text(nickname_or_fallback(share.grantee_nickname.clone())).size(14),
                    muted(crate::i18n::t!(
                        "clans-outside-shared-by",
                        "name" => nickname_or_fallback(share.grantor_nickname.clone())
                    )),
                ]
                .spacing(2),
                space::horizontal(),
            ]
            .align_y(Alignment::Center);
            if page.clan.is_owner || me == Some(share.grantor_id) {
                line = line.push(link_button(
                    crate::i18n::t!("clans-revoke"),
                    Message::RevokeOutsideShare(share.id),
                ));
            }
            col = col.push(line);
        }
        col.into()
    }

    fn package_list<'a>(&'a self, page: &'a ClanPage) -> ThemedElement<'a, Message> {
        let mut col = column![].spacing(2);
        for package in &page.packages {
            col = col.push(Self::resource_button(
                page,
                package,
                crate::i18n::t!("clans-kind-package"),
                0,
            ));
        }
        if page.packages.is_empty() {
            col = col.push(muted(crate::i18n::t!("clans-access-no-packages")));
        }
        col.into()
    }

    fn package_detail<'a>(&'a self, page: &'a ClanPage) -> ThemedElement<'a, Message> {
        let Some(package) = page
            .selected_resource
            .and_then(|id| page.packages.iter().find(|row| row.id == id))
        else {
            return muted(crate::i18n::t!("clans-access-pick")).into();
        };
        let context = self.context(page);
        let mut col = column![
            row![
                text(package.name.clone()).size(18),
                space::horizontal(),
                link_button(
                    crate::i18n::t!("clans-open-package"),
                    Message::OpenPackage(package.name.clone()),
                ),
            ]
            .align_y(Alignment::Center),
            row![
                badge(crate::i18n::t!("clans-kind-package")),
                muted(page.clan.name.clone())
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            text(crate::i18n::t!("clans-group-assignments")).size(15),
        ]
        .spacing(10);
        if let Some(assignments) = &package.grants {
            if assignments.is_empty() {
                col = col.push(muted(crate::i18n::t!("clans-no-assignments")));
            }
            for assignment in assignments {
                col = col.push(self.assignment_row(page, &context, assignment));
                col = col.push(rule::horizontal(1));
            }
            if page.can_grant() {
                col = col.push(
                    button(text(crate::i18n::t!("clans-assign-group")).size(13))
                        .style(theme::builtins::button::primary)
                        .padding([5, 14])
                        .on_press(Message::AssignGroup(ScopeKind::Packages, package.id)),
                );
            }
            return col.into();
        }
        let grants = page.grants_on_package(package.id);
        if grants.is_empty() {
            col = col.push(muted(crate::i18n::t!("clans-your-access")));
            col = col.push(action_chips(package.actions.iter().map(String::as_str)));
        }
        for grant in grants {
            let actions: Vec<&str> = grant
                .actions
                .iter()
                .map(String::as_str)
                .filter(|action| {
                    action.starts_with("package.") && *action != action::CREATE_PACKAGE
                })
                .collect();
            let mut title = row![
                recipient_chip(page, &context, grant.recipient),
                space::horizontal()
            ]
            .spacing(8)
            .align_y(Alignment::Center);
            if page.can_change(grant) {
                title = title.push(link_button(
                    crate::i18n::t!("action-edit"),
                    Message::EditGrant(grant.id),
                ));
                if matches!(grant.scope, GrantScope::Packages { .. }) {
                    title = title.push(link_button(
                        crate::i18n::t!("action-remove"),
                        Message::RemoveGrantPressed(grant.id),
                    ));
                }
            }
            col = col.push(title);
            col = col.push(action_chips(actions));
            if matches!(grant.scope, GrantScope::Clan) {
                col = col.push(muted(crate::i18n::t!("clans-through-clan")));
            }
            if page.confirm == Some(Confirm::RemoveGrant(grant.id)) {
                col = col.push(confirm_box(
                    crate::i18n::t!("clans-remove-permission-confirm"),
                    crate::i18n::t!("action-remove"),
                    Message::RemoveGrantConfirmed(grant.id),
                ));
            }
            col = col.push(rule::horizontal(1));
        }
        if page.can_grant() {
            col = col.push(
                button(text(crate::i18n::t!("clans-assign-group")).size(13))
                    .style(theme::builtins::button::primary)
                    .padding([5, 14])
                    .on_press(Message::AssignGroup(ScopeKind::Packages, package.id)),
            );
        }
        col.into()
    }

    fn secret_list<'a>(&'a self, page: &'a ClanPage) -> ThemedElement<'a, Message> {
        let mut col = column![].spacing(2);
        for secret in &page.secrets {
            let map = secret
                .area_id
                .and_then(|area| page.maps.iter().find(|row| row.id == area.0))
                .map(|row| row.name.clone());
            let badge_text = ownership_label(secret.ownership.as_deref());
            let subtitle = match map {
                Some(map) => format!("{badge_text} · {map}"),
                None => badge_text,
            };
            col = col.push(Self::resource_button(page, secret, subtitle, 0));
        }
        if page.secrets.is_empty() {
            col = col.push(muted(crate::i18n::t!("clans-access-no-secrets")));
        }
        col.into()
    }

    fn secret_detail<'a>(&'a self, page: &'a ClanPage) -> ThemedElement<'a, Message> {
        let Some(secret) = page
            .selected_resource
            .and_then(|id| page.secrets.iter().find(|row| row.id == id))
        else {
            return muted(crate::i18n::t!("clans-access-pick")).into();
        };
        let manages = secret.can("manage_access");
        let mut title = row![
            dot(secret
                .color
                .as_deref()
                .and_then(smudgy_cloud::parse_css_color)),
            text(secret.name.clone()).size(18),
            badge(ownership_label(secret.ownership.as_deref())),
            space::horizontal(),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if let Some(area) = secret.area_id {
            title = title.push(link_button(
                crate::i18n::t!("clans-open-map"),
                Message::OpenMap(area),
            ));
        }
        let mut col = column![
            title,
            muted(crate::i18n::t!("clans-your-access")),
            action_chips(secret.actions.iter().map(String::as_str)),
            text(crate::i18n::t!("clans-group-assignments")).size(15),
        ]
        .spacing(10);
        match &page.secret_grants {
            Some((id, Ok(grants))) if *id == secret.id => {
                if grants.is_empty() {
                    col = col.push(muted(crate::i18n::t!("clans-no-assignments")));
                }
                // A manager who does not own the Secret changes no other
                // manager's grant.
                let owns = secret.can("manage_ownership");
                for grant in grants {
                    let changes = manages
                        && (owns
                            || !grant
                                .actions
                                .iter()
                                .any(|action| action == presets::secret_action::MANAGE_ACCESS));
                    col = col.push(self.secret_grant_row(page, secret.id, grant, changes));
                    col = col.push(rule::horizontal(1));
                }
            }
            Some((id, Err(error))) if *id == secret.id => {
                col = col.push(
                    text(error.clone())
                        .size(13)
                        .style(theme::builtins::text::danger),
                );
            }
            _ => col = col.push(muted(crate::i18n::t!("state-loading"))),
        }
        if secret.ownership.as_deref() == Some("clan") {
            col = col.push(muted(crate::i18n::t!("clans-secret-clan-owned-note")));
        }
        if manages {
            col = col.push(
                button(text(crate::i18n::t!("clans-assign-group")).size(13))
                    .style(theme::builtins::button::primary)
                    .padding([5, 14])
                    .on_press(Message::AssignSecretGroup(secret.id)),
            );
        }
        col.into()
    }

    fn secret_grant_row<'a>(
        &'a self,
        page: &'a ClanPage,
        secret: Uuid,
        grant: &'a ClanSecretGrant,
        manages: bool,
    ) -> ThemedElement<'a, Message> {
        let who: ThemedElement<'a, Message> = match grant.recipient {
            SecretRecipient::Group { group_id } => page.group(group_id).map_or_else(
                || chip(crate::i18n::t!("clans-unknown-group"), None),
                group_chip,
            ),
            SecretRecipient::User { .. } => text(
                grant
                    .nickname
                    .clone()
                    .unwrap_or_else(|| crate::i18n::t!("social-no-nickname")),
            )
            .size(14)
            .into(),
        };
        let mut title = row![who, space::horizontal()]
            .spacing(8)
            .align_y(Alignment::Center);
        if manages {
            title = title.push(link_button(
                crate::i18n::t!("action-edit"),
                Message::EditSecretGrant(secret, grant.id),
            ));
            title = title.push(link_button(
                crate::i18n::t!("action-remove"),
                Message::RemoveSecretGrant(secret, grant.id),
            ));
        }
        column![
            title,
            action_chips(grant.actions.iter().map(String::as_str))
        ]
        .spacing(6)
        .into()
    }

    // ===================== Settings =====================

    fn settings_tab<'a>(
        &'a self,
        page: &'a ClanPage,
        me: Option<Uuid>,
    ) -> ThemedElement<'a, Message> {
        let clan = &page.clan;
        let editable = clan.can(action::EDIT_PROFILE);
        let mut name = text_input(
            crate::i18n::ts!("clans-name-placeholder"),
            &page.profile_name,
        )
        .width(Length::Fill);
        let mut description = text_input(
            crate::i18n::ts!("clans-description-placeholder"),
            &page.profile_description,
        )
        .width(Length::Fill);
        if editable {
            name = name
                .on_input(Message::ProfileNameChanged)
                .on_submit(Message::SaveProfile);
            description = description
                .on_input(Message::ProfileDescriptionChanged)
                .on_submit(Message::SaveProfile);
        }
        let mut col = column![
            text(crate::i18n::t!("clans-details")).size(16),
            column![muted(crate::i18n::t!("clans-name-label")), name].spacing(4),
            column![
                muted(crate::i18n::t!("clans-description-label")),
                description
            ]
            .spacing(4),
        ]
        .spacing(10)
        .max_width(560);
        if editable {
            col = col.push(
                button(text(crate::i18n::t!("action-save")).size(13))
                    .style(theme::builtins::button::primary)
                    .padding([5, 14])
                    .on_press(Message::SaveProfile),
            );
        }

        col = col.push(rule::horizontal(1));
        let last_owner = me.is_some_and(|me| is_last_owner(clan, page.members.as_deref(), me));
        let leave = button(text(crate::i18n::t!("clans-leave-clan")).size(13))
            .style(theme::builtins::button::secondary)
            .padding([5, 12]);
        let mut buttons = row![if last_owner {
            leave
        } else {
            leave.on_press(Message::LeaveClanPressed)
        }]
        .spacing(8);
        if clan.is_owner {
            buttons = buttons.push(
                button(text(crate::i18n::t!("clans-delete-clan")).size(13))
                    .style(theme::builtins::button::danger)
                    .padding([5, 12])
                    .on_press(Message::DeleteClanPressed),
            );
        }
        col = col.push(buttons);
        if last_owner {
            col = col.push(muted(crate::i18n::t!("cloud-error-last-owner")));
        }
        match page.confirm {
            Some(Confirm::DeleteClan) => {
                col = col.push(confirm_box(
                    crate::i18n::t!("clans-delete-clan-confirm", "name" => clan.name.clone()),
                    crate::i18n::t!("action-delete"),
                    Message::DeleteClanConfirmed,
                ));
            }
            Some(Confirm::LeaveClan) => {
                col = col.push(confirm_box(
                    crate::i18n::t!("clans-leave-confirm", "name" => clan.name.clone()),
                    crate::i18n::t!("clans-leave"),
                    Message::LeaveClanConfirmed,
                ));
                if page.maps.iter().any(|row| row.owned_by_me) {
                    col = col.push(
                        checkbox(self.leave_copy)
                            .label(crate::i18n::t!("clans-leave-copy-maps"))
                            .size(14)
                            .text_size(12)
                            .on_toggle(Message::LeaveCopyToggled),
                    );
                }
            }
            _ => {}
        }
        col.into()
    }

    fn feedback(&self) -> ThemedElement<'_, Message> {
        let mut col = column![].spacing(6);
        if let Some(error) = &self.error {
            col = col.push(text(error).size(13).style(theme::builtins::text::danger));
        }
        if let Some(notice) = &self.notice {
            col = col.push(text(notice).size(13).style(theme::builtins::text::success));
        }
        col.into()
    }

    // ===================== dialogs =====================

    #[allow(clippy::too_many_lines)] // one arm per dialog
    fn dialog<'a>(&'a self, page: &'a ClanPage, modal: &'a Modal) -> ThemedElement<'a, Message> {
        let me = self.me();
        let footer = |save: String, message: Message| -> ThemedElement<'a, Message> {
            row![
                space::horizontal(),
                button(text(crate::i18n::t!("action-cancel")).size(13))
                    .style(theme::builtins::button::secondary)
                    .padding([5, 14])
                    .on_press(Message::CloseModal),
                button(text(save).size(13))
                    .style(theme::builtins::button::primary)
                    .padding([5, 14])
                    .on_press(message),
            ]
            .spacing(8)
            .into()
        };
        let error_line = |error: &Option<String>| -> Option<ThemedElement<'a, Message>> {
            error.clone().map(|error| {
                text(error)
                    .size(13)
                    .style(theme::builtins::text::danger)
                    .into()
            })
        };
        match modal {
            Modal::Invite {
                nickname,
                groups,
                error,
            } => {
                let mut body = column![
                    text_input(crate::i18n::ts!("social-nickname-placeholder"), nickname)
                        .on_input(Message::InviteNicknameChanged)
                        .on_submit(Message::SendInvite)
                        .width(Length::Fill),
                ]
                .spacing(10);
                let proposable: Vec<&ClanGroup> = page
                    .custom_groups()
                    .filter(|group| group.can(action::ASSIGN_GROUP))
                    .collect();
                if !proposable.is_empty() {
                    body = body.push(muted(crate::i18n::t!("clans-invite-groups")));
                    for group in proposable {
                        let id = group.id;
                        body = body.push(
                            row![
                                checkbox(groups.contains(&id))
                                    .size(14)
                                    .on_toggle(move |on| Message::InviteGroupToggled(id, on)),
                                group_chip(group),
                            ]
                            .spacing(8)
                            .align_y(Alignment::Center),
                        );
                    }
                }
                if let Some(error) = error_line(error) {
                    body = body.push(error);
                }
                editor::card(
                    crate::i18n::t!("clans-invite-title", "clan" => page.clan.name.clone()),
                    Message::CloseModal,
                    body.into(),
                    footer(crate::i18n::t!("clans-invite"), Message::SendInvite),
                )
            }
            Modal::MemberGroups {
                user_id,
                checked,
                owner,
                confirm_remove,
                error,
            } => {
                let member = page.member(*user_id);
                let name = member.map_or_else(
                    || self.my_nickname(),
                    |member| nickname_or_fallback(member.nickname.clone()),
                );
                let mut body = column![].spacing(10);
                for group in page.custom_groups() {
                    let id = group.id;
                    let assign = me.map_or(Assign::No, |me| page.assignable(group, *user_id, me));
                    let currently = checked.contains(&id);
                    let mut entry = checkbox(currently).size(14);
                    let allowed = match assign {
                        Assign::Yes => true,
                        // Leaving a group oneself is always allowed.
                        Assign::NotSelf => currently,
                        Assign::No => false,
                    };
                    if allowed {
                        entry = entry.on_toggle(move |on| Message::MemberGroupToggled(id, on));
                    }
                    let mut line = row![entry, group_chip(group)]
                        .spacing(8)
                        .align_y(Alignment::Center);
                    if assign == Assign::NotSelf && !currently {
                        line = line.push(muted(crate::i18n::t!("clans-self-assign-reason")));
                    }
                    body = body.push(line);
                }
                if page.custom_groups().next().is_none() {
                    body = body.push(muted(crate::i18n::t!("clans-no-groups")));
                }
                if page.clan.is_owner {
                    let owners = page
                        .members
                        .as_ref()
                        .map_or(1, |members| members.iter().filter(|m| m.is_owner).count());
                    let mut owner_box = checkbox(*owner)
                        .label(crate::i18n::t!("clans-owner-chip"))
                        .size(14)
                        .text_size(13);
                    let was_owner = member.is_some_and(|member| member.is_owner);
                    if !(was_owner && owners <= 1) {
                        owner_box = owner_box.on_toggle(Message::MemberOwnerToggled);
                    }
                    body = body.push(rule::horizontal(1));
                    body = body.push(owner_box);
                }
                if let Some(member) = member
                    && me.is_some_and(|me| can_remove(&page.clan, member, me))
                {
                    body = body.push(rule::horizontal(1));
                    if *confirm_remove {
                        body = body.push(confirm_box(
                            crate::i18n::t!(
                                "clans-remove-confirm",
                                "name" => name.clone(),
                                "clan" => page.clan.name.clone()
                            ),
                            crate::i18n::t!("clans-remove-member"),
                            Message::RemoveMemberConfirmed(*user_id),
                        ));
                    } else {
                        body = body.push(
                            button(text(crate::i18n::t!("clans-remove-member")).size(13))
                                .style(theme::builtins::button::danger)
                                .padding([5, 12])
                                .on_press(Message::RemoveMemberPressed),
                        );
                    }
                }
                if let Some(error) = error_line(error) {
                    body = body.push(error);
                }
                editor::card(
                    crate::i18n::t!("clans-member-groups-title", "name" => name),
                    Message::CloseModal,
                    body.into(),
                    footer(crate::i18n::t!("action-save"), Message::SaveMemberGroups),
                )
            }
            Modal::Group {
                group_id,
                name,
                color,
                confirm_delete,
                error,
            } => {
                let group = group_id.and_then(|id| page.group(id));
                let builtin = group.is_some_and(ClanGroup::is_builtin);
                // Its name and color change with group.rename alone.
                let renames = group.is_none_or(|group| group.can(action::RENAME_GROUP));
                let mut input = text_input(crate::i18n::ts!("clans-group-name-placeholder"), name)
                    .width(Length::Fill);
                if !builtin && renames {
                    input = input
                        .on_input(Message::GroupNameChanged)
                        .on_submit(Message::SaveGroup);
                }
                let mut swatches = row![].spacing(6);
                for hex in group_palette(&page.clan.name) {
                    let selected = color
                        .as_deref()
                        .is_some_and(|c| c.eq_ignore_ascii_case(&hex));
                    swatches = swatches.push(swatch(&hex, selected, renames));
                }
                let mut body = column![
                    column![muted(crate::i18n::t!("clans-group-name-label")), input].spacing(4),
                    column![
                        muted(crate::i18n::t!("clans-group-color-label")),
                        swatches.wrap()
                    ]
                    .spacing(4),
                ]
                .spacing(12);
                if let Some(group) = group
                    && !builtin
                    && group.can(action::DELETE_GROUP)
                {
                    body = body.push(rule::horizontal(1));
                    if *confirm_delete {
                        body = body.push(confirm_box(
                            crate::i18n::t!("clans-delete-group-confirm", "name" => group.name.clone()),
                            crate::i18n::t!("action-delete"),
                            Message::DeleteGroupConfirmed(group.id),
                        ));
                    } else {
                        body = body.push(
                            button(text(crate::i18n::t!("clans-delete-group")).size(13))
                                .style(theme::builtins::button::danger)
                                .padding([5, 12])
                                .on_press(Message::DeleteGroupPressed),
                        );
                    }
                }
                if group_id.is_none() {
                    body = body.push(muted(crate::i18n::t!("clans-new-group-note")));
                }
                if let Some(error) = error_line(error) {
                    body = body.push(error);
                }
                let (title, save) = if group_id.is_some() {
                    (
                        crate::i18n::t!("clans-edit-group"),
                        crate::i18n::t!("action-save"),
                    )
                } else {
                    (
                        crate::i18n::t!("clans-new-group"),
                        crate::i18n::t!("action-create"),
                    )
                };
                let actions = if renames {
                    footer(save, Message::SaveGroup)
                } else {
                    row![
                        space::horizontal(),
                        button(text(crate::i18n::t!("action-close")).size(13))
                            .style(theme::builtins::button::secondary)
                            .padding([5, 14])
                            .on_press(Message::CloseModal),
                    ]
                    .into()
                };
                editor::card(title, Message::CloseModal, body.into(), actions)
            }
            Modal::AddGroupMember {
                group_id,
                filter,
                error,
            } => {
                let group = page.group(*group_id);
                let in_group: Vec<Uuid> = match &page.roster {
                    Some((id, Ok(rows))) if id == group_id => {
                        rows.iter().map(|row| row.user_id).collect()
                    }
                    _ => Vec::new(),
                };
                let mut body = column![
                    text_input(crate::i18n::ts!("clans-filter-placeholder"), filter)
                        .on_input(Message::AddMemberFilterChanged)
                        .width(Length::Fill),
                ]
                .spacing(8);
                let mut any = false;
                if let (Some(members), Some(group)) = (&page.members, group) {
                    for member in
                        filtered(members, filter).filter(|m| !in_group.contains(&m.user_id))
                    {
                        any = true;
                        let assign =
                            me.map_or(Assign::No, |me| page.assignable(group, member.user_id, me));
                        let mut line = row![
                            text(nickname_or_fallback(member.nickname.clone())).size(14),
                            space::horizontal(),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center);
                        line = match assign {
                            Assign::Yes => line.push(small_button(
                                crate::i18n::t!("action-add"),
                                theme::builtins::button::secondary,
                                Message::AddGroupMember(member.user_id),
                            )),
                            Assign::NotSelf => {
                                line.push(muted(crate::i18n::t!("clans-self-assign-reason")))
                            }
                            Assign::No => line,
                        };
                        body = body.push(line);
                    }
                }
                if !any {
                    body = body.push(muted(crate::i18n::t!("clans-add-member-none")));
                }
                if let Some(error) = error_line(error) {
                    body = body.push(error);
                }
                editor::card(
                    crate::i18n::t!(
                        "clans-add-member-title",
                        "group" => group.map(|g| g.name.clone()).unwrap_or_default()
                    ),
                    Message::CloseModal,
                    body.into(),
                    row![
                        space::horizontal(),
                        button(text(crate::i18n::t!("action-done")).size(13))
                            .style(theme::builtins::button::secondary)
                            .padding([5, 14])
                            .on_press(Message::CloseModal),
                    ]
                    .into(),
                )
            }
            Modal::GroupPermissions(editor) => super::permissions::view(editor, page),
            Modal::Grant(grant_editor) => {
                let context = self.context(page);
                let title = match grant_editor.editing {
                    Some(_) => crate::i18n::t!("clans-editor-title-edit"),
                    None if grant_editor.scope_fixed => crate::i18n::t!(
                        "clans-editor-title-assign",
                        "resource" => context.scope_label(&editor::grant_scope(grant_editor.scope, &grant_editor.targets))
                    ),
                    None => crate::i18n::t!(
                        "clans-editor-title-add",
                        "group" => grant_editor
                            .recipient
                            .map(|recipient| context.recipient_name(recipient))
                            .unwrap_or_default()
                    ),
                };
                let body = editor::view(grant_editor, &context, Message::Editor);
                editor::card(
                    title,
                    Message::CloseModal,
                    body,
                    footer(crate::i18n::t!("action-save"), Message::SaveGrant),
                )
            }
            Modal::SecretGrant(secret_editor) => {
                let name = page
                    .secrets
                    .iter()
                    .find(|row| row.id == secret_editor.secret)
                    .map(|row| row.name.clone())
                    .unwrap_or_default();
                let body = editor::secret_view(
                    secret_editor,
                    &page.groups,
                    page.members.as_deref(),
                    Message::SecretEditor,
                );
                editor::card(
                    crate::i18n::t!("clans-editor-title-assign", "resource" => name),
                    Message::CloseModal,
                    body,
                    footer(crate::i18n::t!("action-save"), Message::SaveSecretGrant),
                )
            }
        }
    }
}

// ===================== helpers =====================

/// The notes under a grant: what its scope covers over time, and what a
/// delegation may hand out.
fn grant_notes(grant: &ClanGrant) -> Vec<String> {
    let mut notes = Vec::new();
    match grant.scope {
        GrantScope::Clan => notes.push(crate::i18n::t!("clans-note-clan")),
        GrantScope::Atlases { .. } => notes.push(crate::i18n::t!("clans-note-folders")),
        _ => {}
    }
    if let Some(may) = &grant.may_grant {
        let label = may
            .iter()
            .map(|action| presets::action_label(action))
            .collect::<Vec<_>>()
            .join(", ");
        notes.push(crate::i18n::t!("clans-note-may-grant", "preset" => label));
    }
    if !grant.delegated.is_empty() {
        notes.push(crate::i18n::t!("clans-note-delegated"));
    }
    notes
}

/// A scope's kind for its badge: singular when it names one resource.
fn scope_kind_label(kind: ScopeKind, scope: &GrantScope) -> String {
    let one = scope_parts(scope).1.len() == 1;
    match (kind, one) {
        (ScopeKind::Folders, true) => crate::i18n::t!("clans-kind-folder"),
        (ScopeKind::Maps, true) => crate::i18n::t!("clans-kind-map"),
        (ScopeKind::Packages, true) => crate::i18n::t!("clans-kind-package"),
        (ScopeKind::Groups, true) => crate::i18n::t!("clans-editor-group"),
        _ => kind.label(),
    }
}

fn ownership_label(ownership: Option<&str>) -> String {
    match ownership {
        Some("members") => crate::i18n::t!("clans-member-owned"),
        _ => crate::i18n::t!("clans-clan-owned"),
    }
}

/// A grant's actions as chips: preset names where they fit exactly, and
/// single actions otherwise.
fn action_chips<'a, 'b>(actions: impl IntoIterator<Item = &'b str>) -> ThemedElement<'a, Message> {
    let mut chips = row![].spacing(6);
    for item in presets::chips(actions) {
        chips = chips.push(match item {
            Chip::Preset(_) => chip(item.label(), None),
            Chip::Action(_) => plain_chip(item.label()),
        });
    }
    chips.wrap().into()
}

fn recipient_chip<'a>(
    page: &'a ClanPage,
    context: &EditorContext<'_>,
    recipient: GrantRecipient,
) -> ThemedElement<'a, Message> {
    match recipient {
        GrantRecipient::Group { group_id } => page.group(group_id).map_or_else(
            || chip(crate::i18n::t!("clans-unknown-group"), None),
            group_chip,
        ),
        GrantRecipient::User { .. } => text(context.recipient_name(recipient)).size(14).into(),
    }
}

fn group_color(group: &ClanGroup) -> Option<Color> {
    group
        .color
        .as_deref()
        .and_then(smudgy_cloud::parse_css_color)
}

fn group_chip<'a>(group: &ClanGroup) -> ThemedElement<'a, Message> {
    chip(group.name.clone(), group_color(group))
}

fn owner_chip<'a>() -> ThemedElement<'a, Message> {
    chip(
        crate::i18n::t!("clans-owner-chip"),
        Some(Color::from_rgb8(0xd9, 0xa4, 0x41)),
    )
}

/// A rounded label in a group's color, or the text color without one.
fn chip<'a>(label: String, color: Option<Color>) -> ThemedElement<'a, Message> {
    container(
        text(label)
            .size(12)
            .style(move |theme: &crate::Theme| text::Style {
                color: Some(color.unwrap_or(theme.styles.text.normal)),
            }),
    )
    .padding([2, 9])
    .style(move |theme: &crate::Theme| {
        let tint = color.unwrap_or(theme.styles.text.normal);
        container::Style {
            background: Some(iced::Background::Color(tint.scale_alpha(0.16))),
            border: iced::border::rounded(10.0),
            ..Default::default()
        }
    })
    .into()
}

/// A square label for one action.
fn plain_chip<'a>(label: String) -> ThemedElement<'a, Message> {
    container(text(label).size(12))
        .padding([2, 8])
        .style(|theme: &crate::Theme| container::Style {
            background: Some(iced::Background::Color(
                theme.styles.text.normal.scale_alpha(0.08),
            )),
            border: iced::border::rounded(3.0),
            ..Default::default()
        })
        .into()
}

fn swatch<'a>(hex: &str, selected: bool, enabled: bool) -> ThemedElement<'a, Message> {
    let color = smudgy_cloud::parse_css_color(hex);
    button(space::horizontal().width(18).height(18))
        .padding(0)
        .style(move |theme: &crate::Theme, _status| button::Style {
            background: color.map(iced::Background::Color),
            border: iced::border::color(if selected {
                theme.styles.text.normal
            } else {
                Color::TRANSPARENT
            })
            .width(2.0)
            .rounded(9.0),
            ..Default::default()
        })
        .on_press_maybe(enabled.then(|| Message::GroupColorPicked(Some(hex.to_string()))))
        .into()
}

fn dot<'a>(color: Option<Color>) -> ThemedElement<'a, Message> {
    text("\u{25CF}")
        .size(14)
        .style(move |theme: &crate::Theme| text::Style {
            color: color.or(Some(theme.styles.text.normal.scale_alpha(0.4))),
        })
        .into()
}

fn muted<'a>(label: String) -> iced::widget::Text<'a, crate::Theme> {
    text(label).size(12).style(theme::builtins::text::muted)
}

fn badge<'a>(label: String) -> ThemedElement<'a, Message> {
    container(text(label).size(11).style(theme::builtins::text::muted))
        .padding([1, 6])
        .style(|theme: &crate::Theme| container::Style {
            border: iced::border::color(theme.styles.general.border)
                .width(1.0)
                .rounded(3.0),
            ..Default::default()
        })
        .into()
}

fn tab_button<'a>(label: String, selected: bool, message: Message) -> ThemedElement<'a, Message> {
    button(text(label).size(14))
        .style(if selected {
            theme::builtins::button::list_item_selected
        } else {
            theme::builtins::button::list_item
        })
        .padding([4, 12])
        .on_press(message)
        .into()
}

fn link_button<'a>(label: String, message: Message) -> ThemedElement<'a, Message> {
    button(text(label).size(13))
        .style(theme::builtins::button::quiet_link)
        .padding([2, 6])
        .on_press(message)
        .into()
}

fn refresh_button<'a>() -> ThemedElement<'a, Message> {
    button(text(crate::i18n::t!("action-refresh")).size(13))
        .style(theme::builtins::button::secondary)
        .padding([4, 10])
        .on_press(Message::Refresh)
        .into()
}

fn small_button<'a>(
    label: String,
    style: fn(&crate::Theme, button::Status) -> button::Style,
    message: Message,
) -> ThemedElement<'a, Message> {
    button(text(label).size(12))
        .style(style)
        .padding([2, 8])
        .on_press(message)
        .into()
}

fn confirm_box<'a>(
    question: String,
    confirm: String,
    message: Message,
) -> ThemedElement<'a, Message> {
    container(
        column![
            text(question).size(13).style(theme::builtins::text::danger),
            row![
                small_button(
                    crate::i18n::t!("action-cancel"),
                    theme::builtins::button::secondary,
                    Message::ConfirmCancelled,
                ),
                small_button(confirm, theme::builtins::button::primary, message),
            ]
            .spacing(8),
        ]
        .spacing(8),
    )
    .padding(10)
    .style(theme::builtins::container::modal_body)
    .into()
}
