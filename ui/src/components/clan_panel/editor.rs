//! Individual action editors for grants and Secret access. Unknown grant actions are preserved.

use std::collections::BTreeSet;
use std::fmt;

use iced::widget::{button, checkbox, column, container, pick_list, row, scrollable, space, text};
use iced::{Alignment, Length};
use smudgy_cloud::clan_access::{GrantBody, IndexedResource};
use smudgy_cloud::clan_secrets::{ClanSecretGrant, SecretRecipient};
use smudgy_cloud::clans::{ClanGrant, ClanGroup, ClanMember, GrantRecipient, GrantScope, action};
use smudgy_cloud::{AreaId, AtlasId, Uuid};

use crate::presets::{self, Kind, ScopeKind};
use crate::theme::{self, Element as ThemedElement};

/// A group in a picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupChoice {
    pub id: Uuid,
    pub name: String,
}

impl fmt::Display for GroupChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name)
    }
}

#[derive(Debug, Clone)]
pub enum EditorMessage {
    RecipientPicked(GroupChoice),
    ScopePicked(ScopeKind),
    TargetToggled(Uuid, bool),
    ActionToggled(&'static str, bool),
    MayGrantToggled(&'static str, bool),
}

/// The open grant editor.
#[derive(Debug, Clone)]
pub struct GrantEditor {
    /// The grant being changed: its recipient and scope stay.
    pub editing: Option<Uuid>,
    pub recipient: Option<GrantRecipient>,
    /// The recipient came with the dialog (a group's page, or Edit).
    pub recipient_fixed: bool,
    pub scope: ScopeKind,
    /// The scope came with the dialog (a resource's page, or Edit).
    pub scope_fixed: bool,
    /// The named groups, folders, maps or packages of a scope that names them.
    pub targets: BTreeSet<Uuid>,
    /// The chosen actions this client knows.
    pub actions: BTreeSet<&'static str>,
    /// Actions this client does not know, kept as they are.
    pub unknown: BTreeSet<String>,
    /// The individual actions a delegation may give.
    pub may_grant: BTreeSet<String>,
    /// The scope names a Member-owned map, whose grants take only the map
    /// actions its owners hold.
    pub member_map: bool,
    pub error: Option<String>,
}

impl GrantEditor {
    fn blank(scope: ScopeKind) -> Self {
        Self {
            editing: None,
            recipient: None,
            recipient_fixed: false,
            scope,
            scope_fixed: false,
            targets: BTreeSet::new(),
            actions: BTreeSet::new(),
            unknown: BTreeSet::new(),
            may_grant: BTreeSet::new(),
            member_map: false,
            error: None,
        }
    }

    /// "Add permission" on a group's page.
    #[must_use]
    #[cfg(test)]
    pub fn for_group(group: Uuid) -> Self {
        Self {
            recipient: Some(GrantRecipient::Group { group_id: group }),
            recipient_fixed: true,
            ..Self::blank(ScopeKind::Folders)
        }
    }

    /// "Assign group" on a resource's page: the scope names it alone.
    #[must_use]
    pub fn for_resource(scope: ScopeKind, id: Uuid) -> Self {
        Self {
            scope_fixed: true,
            targets: BTreeSet::from([id]),
            ..Self::blank(scope)
        }
    }

    /// Edit: a grant's actions, with its recipient and scope fixed.
    #[must_use]
    pub fn editing(grant: &ClanGrant) -> Self {
        let (scope, targets) = scope_parts(&grant.scope);
        let mut editor = Self {
            editing: Some(grant.id),
            recipient: Some(grant.recipient),
            recipient_fixed: true,
            scope_fixed: true,
            targets,
            ..Self::blank(scope)
        };
        for action in &grant.actions {
            match known(action) {
                Some(known) => {
                    editor.actions.insert(known);
                }
                None => {
                    editor.unknown.insert(action.clone());
                }
            }
        }
        if let Some(may) = &grant.may_grant {
            editor.may_grant = may.iter().cloned().collect();
        }
        editor
    }

    /// The kinds this editor shows rows for.
    #[must_use]
    pub fn kinds(&self) -> Vec<Kind> {
        Kind::ALL
            .into_iter()
            .filter(|kind| *kind != Kind::Secret && kind.applies_to(self.scope))
            .collect()
    }

    pub(super) fn allowed(&self, action: &str) -> bool {
        if self.member_map {
            presets::takes_on_member_map(action)
        } else {
            presets::allowed_on(action, self.scope)
        }
    }

    /// Whether a member recipient may take `action` here.
    #[must_use]
    pub fn member_may(&self, action: &str) -> bool {
        presets::allowed_for_member_on(action, self.scope, self.targets.len())
    }

    /// Whether the chosen actions hand out access.
    #[must_use]
    pub fn delegates(&self) -> bool {
        self.actions.contains(action::MANAGE_GRANTS)
    }

    pub fn update(&mut self, message: EditorMessage) {
        self.error = None;
        match message {
            EditorMessage::RecipientPicked(group) => {
                if !self.recipient_fixed {
                    self.recipient = Some(GrantRecipient::Group { group_id: group.id });
                }
            }
            EditorMessage::ScopePicked(scope) => {
                if !self.scope_fixed && scope != self.scope {
                    self.scope = scope;
                    self.targets.clear();
                    let allowed: Vec<&'static str> = self
                        .actions
                        .iter()
                        .copied()
                        .filter(|action| presets::allowed_on(action, scope))
                        .collect();
                    self.actions = allowed.into_iter().collect();
                }
            }
            EditorMessage::TargetToggled(id, on) => {
                if !self.scope_fixed {
                    if on {
                        self.targets.insert(id);
                    } else {
                        self.targets.remove(&id);
                    }
                }
            }
            EditorMessage::ActionToggled(action, on) => {
                if self.member_map
                    && !on
                    && action == action::READ_AREA
                    && self.actions.iter().any(|a| *a != action::READ_AREA)
                {
                    return;
                }
                if on {
                    self.actions.insert(action);
                    if self.member_map {
                        self.actions.insert(action::READ_AREA);
                    }
                    if action == action::MANAGE_GRANTS {
                        self.actions.insert(action::INSPECT_GRANTS);
                    }
                } else {
                    self.actions.remove(action);
                }
            }
            EditorMessage::MayGrantToggled(action, on) => {
                if on {
                    self.may_grant.insert(action.to_string());
                } else {
                    self.may_grant.remove(action);
                }
            }
        }
    }

    /// The single grant this editor writes.
    ///
    /// # Errors
    /// The reason to show when the editor is not ready: no recipient, no
    /// target, or no action.
    pub fn writes(&self, member_recipient: bool) -> Result<Vec<(GrantScope, GrantBody)>, String> {
        if self.recipient.is_none() {
            return Err(crate::i18n::t!("clans-editor-choose-group"));
        }
        if self.scope != ScopeKind::Clan && self.targets.is_empty() {
            return Err(crate::i18n::t!("clans-editor-choose-target"));
        }
        let mut chosen: Vec<String> = self
            .actions
            .iter()
            .copied()
            .filter(|action| self.allowed(action))
            .filter(|action| !member_recipient || self.member_may(action))
            .map(ToString::to_string)
            .collect();
        chosen.extend(self.unknown.iter().cloned());
        if chosen.is_empty() {
            return Err(crate::i18n::t!("clans-editor-choose-action"));
        }
        let may_grant = |actions: &[String]| {
            actions
                .iter()
                .any(|action| action == action::MANAGE_GRANTS)
                .then(|| self.may_grant.iter().cloned().collect::<Vec<_>>())
        };
        Ok(vec![(
            grant_scope(self.scope, &self.targets),
            GrantBody {
                may_grant: may_grant(&chosen),
                actions: chosen,
            },
        )])
    }
}

/// The client's `&'static str` for an action it knows.
fn known(action: &str) -> Option<&'static str> {
    Kind::of_action(action)?
        .actions()
        .find(|known| *known == action)
}

/// A scope's kind and the IDs it names.
#[must_use]
pub fn scope_parts(scope: &GrantScope) -> (ScopeKind, BTreeSet<Uuid>) {
    match scope {
        GrantScope::Clan => (ScopeKind::Clan, BTreeSet::new()),
        GrantScope::Groups { ids } => (ScopeKind::Groups, ids.iter().copied().collect()),
        GrantScope::Atlases { ids } => (ScopeKind::Folders, ids.iter().map(|id| id.0).collect()),
        GrantScope::Areas { ids } => (ScopeKind::Maps, ids.iter().map(|id| id.0).collect()),
        GrantScope::Packages { ids } => (ScopeKind::Packages, ids.iter().copied().collect()),
    }
}

/// The wire scope of a scope kind naming `targets`.
#[must_use]
pub fn grant_scope(scope: ScopeKind, targets: &BTreeSet<Uuid>) -> GrantScope {
    match scope {
        ScopeKind::Clan => GrantScope::Clan,
        ScopeKind::Groups => GrantScope::Groups {
            ids: targets.iter().copied().collect(),
        },
        ScopeKind::Folders => GrantScope::Atlases {
            ids: targets.iter().copied().map(AtlasId).collect(),
        },
        ScopeKind::Maps => GrantScope::Areas {
            ids: targets.iter().copied().map(AreaId).collect(),
        },
        ScopeKind::Packages => GrantScope::Packages {
            ids: targets.iter().copied().collect(),
        },
    }
}

/// What the editor's view needs from the clan page.
pub struct EditorContext<'a> {
    pub groups: &'a [ClanGroup],
    pub members: Option<&'a [ClanMember]>,
    pub folders: &'a [IndexedResource],
    pub maps: &'a [IndexedResource],
    pub packages: &'a [IndexedResource],
    pub owner: bool,
}

impl EditorContext<'_> {
    /// The name of the group or member a grant goes to.
    #[must_use]
    pub fn recipient_name(&self, recipient: GrantRecipient) -> String {
        match recipient {
            GrantRecipient::Group { group_id } => self
                .groups
                .iter()
                .find(|group| group.id == group_id)
                .map_or_else(
                    || crate::i18n::t!("clans-unknown-group"),
                    |group| group.name.clone(),
                ),
            GrantRecipient::User { user_id } => self
                .members
                .and_then(|members| members.iter().find(|member| member.user_id == user_id))
                .and_then(|member| member.nickname.clone())
                .unwrap_or_else(|| crate::i18n::t!("social-no-nickname")),
        }
    }

    /// The choosable targets of a scope kind, by name: the clan's own
    /// groups, folders, maps and packages. A Member-owned map's access is
    /// its owners' to give, from the map editor.
    fn targets(&self, scope: ScopeKind) -> Vec<(Uuid, String)> {
        self.named(scope, true)
    }

    /// The resources of a scope kind the caller knows, by name; with
    /// `clan_owned`, only the clan's own.
    fn named(&self, scope: ScopeKind, clan_owned: bool) -> Vec<(Uuid, String)> {
        let rows = |rows: &[IndexedResource]| {
            rows.iter()
                .filter(|row| !clan_owned || row.clan_owned())
                .map(|row| (row.id, row.name.clone()))
                .collect()
        };
        match scope {
            ScopeKind::Clan => Vec::new(),
            ScopeKind::Groups => self
                .groups
                .iter()
                .filter(|group| !group.is_builtin())
                .map(|group| (group.id, group.name.clone()))
                .collect(),
            ScopeKind::Folders => rows(self.folders),
            ScopeKind::Maps => rows(self.maps),
            ScopeKind::Packages => rows(self.packages),
        }
    }

    /// A short description of the resources a scope names.
    #[must_use]
    pub fn scope_label(&self, scope: &GrantScope) -> String {
        let (kind, ids) = scope_parts(scope);
        if kind == ScopeKind::Clan {
            return crate::i18n::t!("presets-scope-clan");
        }
        let names: Vec<String> = self
            .named(kind, false)
            .into_iter()
            .filter(|(id, _)| ids.contains(id))
            .map(|(_, name)| name)
            .collect();
        let hidden = ids.len().saturating_sub(names.len());
        let names = names.join(&crate::i18n::t!("mapper-multi-list-separator"));
        match (hidden, names.is_empty()) {
            (0, _) => names,
            (_, true) => crate::i18n::t!("clans-scope-more", "count" => hidden),
            (_, false) => crate::i18n::t!(
                "clans-scope-names-and-more",
                "names" => names,
                "count" => hidden
            ),
        }
    }
}

fn checkbox_grid<'a, Message: Clone + 'a>(
    boxes: Vec<(String, bool, Option<Message>, Option<Message>)>,
) -> ThemedElement<'a, Message> {
    let mut grid = column![].spacing(6);
    let mut boxes = boxes.into_iter().peekable();
    while boxes.peek().is_some() {
        let mut line = row![].spacing(12);
        for _ in 0..2 {
            if let Some((label, checked, on, off)) = boxes.next() {
                let mut entry = checkbox(checked).label(label).size(14).text_size(13);
                if let (Some(on), Some(off)) = (on, off) {
                    entry = entry.on_toggle(move |now| if now { on.clone() } else { off.clone() });
                }
                line = line.push(container(entry).width(Length::FillPortion(1)));
            } else {
                line = line.push(space::horizontal().width(Length::FillPortion(1)));
            }
        }
        grid = grid.push(line);
    }
    grid.into()
}

/// The grant editor's body, its title row and buttons drawn by the caller.
pub fn view<'a, Message: Clone + 'a>(
    editor: &'a GrantEditor,
    context: &EditorContext<'_>,
    map: impl Fn(EditorMessage) -> Message + Copy + 'a,
) -> ThemedElement<'a, Message> {
    let label = |key: String| text(key).size(13).style(theme::builtins::text::muted);
    let mut col = column![].spacing(10);

    // Recipient.
    let recipient: ThemedElement<'a, Message> = match (editor.recipient, editor.recipient_fixed) {
        (Some(recipient), true) => text(context.recipient_name(recipient)).size(14).into(),
        (recipient, _) => {
            let groups: Vec<GroupChoice> = context
                .groups
                .iter()
                .map(|group| GroupChoice {
                    id: group.id,
                    name: group.name.clone(),
                })
                .collect();
            let selected = recipient
                .and_then(GrantRecipient::group)
                .and_then(|id| groups.iter().find(|group| group.id == id).cloned());
            pick_list(groups, selected, move |group| {
                map(EditorMessage::RecipientPicked(group))
            })
            .placeholder(crate::i18n::ts!("clans-editor-choose-group"))
            .text_size(13)
            .width(Length::Fill)
            .into()
        }
    };
    let member_recipient = matches!(editor.recipient, Some(GrantRecipient::User { .. }));
    col = col.push(
        column![
            label(if member_recipient {
                crate::i18n::t!("clans-editor-member")
            } else {
                crate::i18n::t!("clans-editor-group")
            }),
            recipient
        ]
        .spacing(4),
    );
    if member_recipient {
        col = col.push(
            text(crate::i18n::t!("clans-editor-member-note"))
                .size(12)
                .style(theme::builtins::text::muted),
        );
    }

    // Scope.
    {
        let scope: ThemedElement<'a, Message> = if editor.scope_fixed {
            text(match editor.scope {
                ScopeKind::Clan => editor.scope.label(),
                _ => context.scope_label(&grant_scope(editor.scope, &editor.targets)),
            })
            .size(14)
            .into()
        } else {
            pick_list(
                [
                    ScopeKind::Clan,
                    ScopeKind::Folders,
                    ScopeKind::Maps,
                    ScopeKind::Packages,
                    ScopeKind::Groups,
                ],
                Some(editor.scope),
                move |scope| map(EditorMessage::ScopePicked(scope)),
            )
            .text_size(13)
            .width(Length::Fill)
            .into()
        };
        col = col.push(column![label(crate::i18n::t!("clans-editor-scope")), scope].spacing(4));
        if !editor.scope_fixed && editor.scope != ScopeKind::Clan {
            let targets = context.targets(editor.scope);
            if targets.is_empty() {
                col = col.push(
                    text(crate::i18n::t!("clans-editor-targets-empty"))
                        .size(12)
                        .style(theme::builtins::text::muted),
                );
            } else {
                let boxes = targets
                    .into_iter()
                    .map(|(id, name)| {
                        (
                            name,
                            editor.targets.contains(&id),
                            Some(map(EditorMessage::TargetToggled(id, true))),
                            Some(map(EditorMessage::TargetToggled(id, false))),
                        )
                    })
                    .collect();
                col = col.push(
                    container(scrollable(checkbox_grid(boxes)).height(Length::Shrink))
                        .max_height(160),
                );
            }
        }
    }

    // Every permission is explicit; sections explain how it combines with others.
    for kind in editor.kinds() {
        let actions: Vec<_> = kind
            .actions()
            .filter(|action| {
                editor.allowed(action) && (context.owner || !action.starts_with("grant."))
            })
            .collect();
        if actions.is_empty() {
            continue;
        }
        col = col.push(text(kind.label()).size(16));
        for action in actions {
            col = col.push(super::permission_rows::permission(
                action,
                editor.actions.contains(action),
                (!member_recipient || editor.member_may(action))
                    && !(editor.member_map
                        && action == action::READ_AREA
                        && editor.actions.iter().any(|a| *a != action::READ_AREA)),
                move |on| map(EditorMessage::ActionToggled(action, on)),
            ));
        }
    }
    if editor.delegates() && context.owner {
        col = col.push(text(crate::i18n::t!("permissions-delegation-title")).size(16));
        col = col.push(
            text(crate::i18n::t!("permissions-delegation-help"))
                .size(13)
                .style(theme::builtins::text::muted),
        );
        for action in Kind::ALL
            .into_iter()
            .filter(|kind| *kind != Kind::Secret)
            .flat_map(Kind::actions)
            .filter(|action| {
                editor.allowed(action)
                    && !action.starts_with("grant.")
                    && !action.starts_with("clan.")
                    && !action.starts_with("group.")
            })
        {
            col = col.push(super::permission_rows::permission(
                action,
                editor.may_grant.contains(action),
                true,
                move |on| map(EditorMessage::MayGrantToggled(action, on)),
            ));
        }
    }

    if let Some(error) = &editor.error {
        col = col.push(text(error).size(13).style(theme::builtins::text::danger));
    }
    col.into()
}

// ===========================================================================
// One Clan Secret's grant
// ===========================================================================

#[derive(Debug, Clone)]
pub enum SecretEditorMessage {
    RecipientPicked(GroupChoice),
    ActionToggled(&'static str, bool),
}

/// The open editor of a grant on one Clan Secret.
#[derive(Debug, Clone)]
pub struct SecretGrantEditor {
    pub secret: Uuid,
    pub editing: Option<Uuid>,
    pub recipient: Option<SecretRecipient>,
    pub actions: BTreeSet<&'static str>,
    /// What the grant may give when the viewer manages the Secret's access
    /// without owning it: the actions they hold and those the grant already
    /// has, never `manage_access`. `None` for one of its owners.
    pub bound: Option<BTreeSet<&'static str>>,
    pub error: Option<String>,
}

impl SecretGrantEditor {
    /// Bounds the editor by `mine`, the viewer's actions on the Secret: one
    /// who manages its access but does not own it gives only actions they
    /// hold or the grant already has, and never `manage_access`.
    #[must_use]
    pub fn within<'a>(mut self, mine: impl IntoIterator<Item = &'a str>) -> Self {
        let mine: Vec<&str> = mine.into_iter().collect();
        if mine.contains(&"manage_ownership") {
            self.bound = None;
            return self;
        }
        let before: Vec<&'static str> = if self.editing.is_some() {
            self.actions.iter().copied().collect()
        } else {
            Vec::new()
        };
        self.bound = Some(
            mine.into_iter()
                .filter_map(known)
                .chain(before)
                .chain([presets::secret_action::READ])
                .filter(|action| *action != presets::secret_action::MANAGE_ACCESS)
                .collect(),
        );
        self
    }

    /// Whether the grant may give `action` here.
    #[must_use]
    pub fn allows(&self, action: &str) -> bool {
        self.bound
            .as_ref()
            .is_none_or(|bound| bound.contains(action))
    }

    /// "Assign group" on a Secret: Reader to start with.
    #[must_use]
    pub fn new(secret: Uuid) -> Self {
        Self {
            secret,
            editing: None,
            recipient: None,
            actions: BTreeSet::from([presets::secret_action::READ]),
            bound: None,
            error: None,
        }
    }

    /// Edit one of the Secret's grants.
    #[must_use]
    pub fn editing(secret: Uuid, grant: &ClanSecretGrant) -> Self {
        let actions: BTreeSet<&'static str> = grant
            .actions
            .iter()
            .filter_map(|action| known(action))
            .collect();
        Self {
            secret,
            editing: Some(grant.id),
            recipient: Some(grant.recipient),
            actions,
            bound: None,
            error: None,
        }
    }

    pub fn update(&mut self, message: SecretEditorMessage) {
        self.error = None;
        match message {
            SecretEditorMessage::RecipientPicked(group) => {
                if self.editing.is_none() {
                    self.recipient = Some(SecretRecipient::Group { group_id: group.id });
                }
            }
            SecretEditorMessage::ActionToggled(action, on) => {
                if on {
                    self.actions.insert(action);
                } else if action != presets::secret_action::READ {
                    self.actions.remove(action);
                }
            }
        }
    }
}

/// The Secret grant editor's body.
pub fn secret_view<'a, Message: Clone + 'a>(
    editor: &'a SecretGrantEditor,
    groups: &[ClanGroup],
    members: Option<&[ClanMember]>,
    map: impl Fn(SecretEditorMessage) -> Message + Copy + 'a,
) -> ThemedElement<'a, Message> {
    let label = |key: String| text(key).size(13).style(theme::builtins::text::muted);
    let recipient: ThemedElement<'a, Message> = match (editor.recipient, editor.editing) {
        (Some(SecretRecipient::Group { group_id }), Some(_)) => text(
            groups
                .iter()
                .find(|group| group.id == group_id)
                .map_or_else(
                    || crate::i18n::t!("clans-unknown-group"),
                    |group| group.name.clone(),
                ),
        )
        .size(14)
        .into(),
        (Some(SecretRecipient::User { user_id }), _) => text(
            members
                .and_then(|members| members.iter().find(|member| member.user_id == user_id))
                .and_then(|member| member.nickname.clone())
                .unwrap_or_else(|| crate::i18n::t!("social-no-nickname")),
        )
        .size(14)
        .into(),
        (recipient, _) => {
            let choices: Vec<GroupChoice> = groups
                .iter()
                .map(|group| GroupChoice {
                    id: group.id,
                    name: group.name.clone(),
                })
                .collect();
            let selected = recipient
                .and_then(|recipient| recipient.group())
                .and_then(|id| choices.iter().find(|choice| choice.id == id).cloned());
            pick_list(choices, selected, move |group| {
                map(SecretEditorMessage::RecipientPicked(group))
            })
            .placeholder(crate::i18n::ts!("clans-editor-choose-group"))
            .text_size(13)
            .width(Length::Fill)
            .into()
        }
    };
    let mut col =
        column![column![label(crate::i18n::t!("clans-editor-group")), recipient].spacing(4)]
            .spacing(10);
    for action in Kind::Secret
        .actions()
        .filter(|action| editor.allows(action))
    {
        col = col.push(super::permission_rows::permission(
            action,
            editor.actions.contains(action),
            action != presets::secret_action::READ,
            move |on| map(SecretEditorMessage::ActionToggled(action, on)),
        ));
    }
    if let Some(error) = &editor.error {
        col = col.push(text(error).size(13).style(theme::builtins::text::danger));
    }
    col.into()
}

/// A modal card: a title with Close, the body, and the footer buttons.
pub fn card<'a, Message: Clone + 'a>(
    title: String,
    close: Message,
    body: ThemedElement<'a, Message>,
    footer: ThemedElement<'a, Message>,
) -> ThemedElement<'a, Message> {
    container(
        column![
            row![
                text(title).size(18).width(Length::Fill),
                button(text(crate::i18n::t!("action-close")).size(13))
                    .style(theme::builtins::button::quiet_link)
                    .padding([2, 6])
                    .on_press(close),
            ]
            .align_y(Alignment::Center),
            crate::widgets::dialog::scrolling_body(body, footer),
        ]
        .spacing(14),
    )
    .padding(20)
    .max_width(560)
    .max_height(640)
    .style(theme::builtins::container::modal_card)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group() -> Uuid {
        Uuid::from_u128(7)
    }

    #[test]
    fn a_scope_takes_only_its_actions_and_needs_a_target() {
        let mut editor = GrantEditor::for_group(group());
        editor.update(EditorMessage::ScopePicked(ScopeKind::Clan));
        for action in [action::CREATE_AREA, action::RENAME_AREA] {
            editor.update(EditorMessage::ActionToggled(action, true));
        }
        editor.update(EditorMessage::ScopePicked(ScopeKind::Maps));
        // area.create is a folder's: a maps scope drops it.
        assert!(!editor.actions.contains(action::CREATE_AREA));
        assert!(editor.actions.contains(action::RENAME_AREA));
        assert_eq!(
            editor.writes(false),
            Err(crate::i18n::t!("clans-editor-choose-target"))
        );
        editor.update(EditorMessage::TargetToggled(Uuid::from_u128(3), true));
        let writes = editor.writes(false).unwrap();
        assert_eq!(
            writes[0].0,
            GrantScope::Areas {
                ids: vec![AreaId(Uuid::from_u128(3))]
            }
        );
        assert!(!editor.kinds().contains(&Kind::Package));
    }

    #[test]
    fn editing_keeps_unknown_actions() {
        let grant: ClanGrant = serde_json::from_value(serde_json::json!({
            "id": "11111111-1111-4111-8111-111111111111",
            "clan_id": "22222222-2222-4222-8222-222222222222",
            "recipient": { "group_id": "33333333-3333-4333-8333-333333333333" },
            "actions": ["area.read", "secret.read", "secret.add", "secret.edit", "area.future"],
            "scope": { "kind": "atlases", "ids": ["44444444-4444-4444-8444-444444444444"] },
            "issuer_id": "55555555-5555-4555-8555-555555555555",
            "parent_id": null,
            "created_at": "2026-10-07T00:00:00Z",
            "updated_at": "2026-10-07T00:00:00Z"
        }))
        .unwrap();
        let editor = GrantEditor::editing(&grant);
        assert_eq!(editor.scope, ScopeKind::Folders);
        let writes = editor.writes(false).unwrap();
        assert!(writes[0].1.actions.contains(&"area.future".to_string()));
    }

    #[test]
    fn a_member_takes_map_access_on_one_map_alone() {
        let mut editor = GrantEditor::for_resource(ScopeKind::Maps, Uuid::from_u128(5));
        editor.recipient = Some(GrantRecipient::User {
            user_id: Uuid::from_u128(6),
        });
        for action in [
            action::READ_AREA,
            action::ADD_TO_AREA,
            action::EDIT_AREA,
            action::REMOVE_FROM_AREA,
        ] {
            editor.update(EditorMessage::ActionToggled(action, true));
        }
        let writes = editor.writes(true).unwrap();
        assert_eq!(
            writes[0].1.actions.len(),
            4,
            "the creator's Editor grant keeps its actions"
        );
        let mut folder = GrantEditor::for_resource(ScopeKind::Folders, Uuid::from_u128(5));
        folder.recipient = editor.recipient;
        folder.update(EditorMessage::ActionToggled(action::READ_AREA, true));
        assert_eq!(
            folder.writes(true),
            Err(crate::i18n::t!("clans-editor-choose-action"))
        );
    }

    #[test]
    fn a_member_recipient_takes_no_map_access() {
        let mut editor = GrantEditor::for_resource(ScopeKind::Packages, Uuid::from_u128(5));
        editor.recipient = Some(GrantRecipient::User {
            user_id: Uuid::from_u128(6),
        });
        for action in ["package.read", "package.edit_draft"] {
            editor.update(EditorMessage::ActionToggled(action, true));
        }
        let writes = editor.writes(true).unwrap();
        assert_eq!(writes[0].1.actions, ["package.edit_draft", "package.read"]);
    }

    /// A Secret's access manager who does not own it gives only actions
    /// they hold or the grant already has, and never `manage_access`; an
    /// owner gives any.
    #[test]
    fn a_secret_access_manager_gives_only_what_they_hold() {
        let manager = ["read", "add", "manage_access"];
        let editor = SecretGrantEditor::new(Uuid::from_u128(1)).within(manager);
        assert!(editor.allows("add"));
        assert!(!editor.allows("edit") && !editor.allows("copy"));
        assert!(!editor.allows("manage_access"));

        let grant = ClanSecretGrant {
            id: Uuid::from_u128(2),
            secret_id: Uuid::from_u128(1),
            area_id: smudgy_cloud::AreaId(Uuid::from_u128(3)),
            clan_id: Uuid::from_u128(4),
            recipient: SecretRecipient::Group {
                group_id: Uuid::from_u128(5),
            },
            actions: ["read", "edit", "copy"]
                .iter()
                .map(ToString::to_string)
                .collect(),
            grantor_id: Uuid::from_u128(6),
            nickname: None,
            grantor_nickname: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let editing = SecretGrantEditor::editing(Uuid::from_u128(1), &grant).within(manager);
        assert!(
            editing.allows("edit") && editing.allows("copy"),
            "what the grant has stays"
        );
        assert!(!editing.allows("remove"));

        let owner = SecretGrantEditor::new(Uuid::from_u128(1)).within([
            "read",
            "manage_access",
            "manage_ownership",
        ]);
        assert!(owner.allows("manage_access"));
    }

    #[test]
    fn a_secret_grant_always_reads_and_copy_is_separate() {
        let mut editor = SecretGrantEditor::new(Uuid::from_u128(1));
        editor.update(SecretEditorMessage::ActionToggled("copy", true));
        editor.update(SecretEditorMessage::ActionToggled("read", false));
        assert!(editor.actions.contains("read"));
        assert!(editor.actions.contains("copy"));
    }
}
