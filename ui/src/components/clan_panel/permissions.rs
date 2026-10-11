//! A group's permissions, selected by resource: each permission a switch,
//! with a preset picker per group of switches that sets them.
use super::editor::{Switch, pickers};
use super::{ClanPage, Message as PanelMessage, editor, permission_rows};
use crate::presets::{self, Kind, PresetPick, ScopeKind};
use crate::theme::{self, Element as ThemedElement};
use iced::widget::{button, column, container, row, rule, scrollable, space, text};
use iced::{Alignment, Length};
use smudgy_cloud::Uuid;
use smudgy_cloud::clan_access::{GrantBody, GrantChange};
use smudgy_cloud::clans::{ClanGrant, ClanGroup, GrantRecipient, GrantScope, action};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Clan,
    Maps,
    Packages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Target {
    pub kind: ScopeKind,
    pub id: Option<Uuid>,
}

impl Target {
    const CLAN: Self = Self {
        kind: ScopeKind::Clan,
        id: None,
    };
    fn scope(self) -> GrantScope {
        editor::grant_scope(self.kind, &self.id.into_iter().collect())
    }
    fn exact(self, grant: &ClanGrant) -> bool {
        grant.scope == self.scope()
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Tab(Tab),
    Select(Target),
    Toggle(&'static str, bool),
    Ceiling(&'static str, bool),
    /// A preset picked for the selected resource's permissions.
    Preset(PresetPick),
    /// A preset picked for what the group may hand out there.
    CeilingPreset(PresetPick),
}

/// The group's grant over one resource as the editor changes it. A group
/// holds at most one grant over a scope (clans.md §5.3).
#[derive(Debug, Clone)]
struct Draft {
    original: Option<ClanGrant>,
    actions: BTreeSet<String>,
    ceiling: BTreeSet<String>,
}

impl Draft {
    fn new(original: Option<ClanGrant>) -> Self {
        Self {
            actions: original
                .as_ref()
                .map(|grant| grant.actions.clone())
                .unwrap_or_default(),
            ceiling: original
                .as_ref()
                .and_then(|grant| grant.may_grant.clone())
                .unwrap_or_default(),
            original,
        }
    }

    /// The actions and ceiling the grant has before the edit.
    fn before(&self) -> (BTreeSet<String>, BTreeSet<String>) {
        let Some(grant) = &self.original else {
            return (BTreeSet::new(), BTreeSet::new());
        };
        (
            grant.actions.clone(),
            grant.may_grant.clone().unwrap_or_default(),
        )
    }

    /// The ceiling the edit leaves: none once the grant stops handing out
    /// access.
    fn after_ceiling(&self) -> BTreeSet<String> {
        if self.actions.contains(action::MANAGE_GRANTS) {
            self.ceiling.clone()
        } else {
            BTreeSet::new()
        }
    }
}

#[derive(Debug, Clone)]
pub struct Permissions {
    pub group: Uuid,
    name: String,
    owners: bool,
    viewer: Option<Uuid>,
    tab: Tab,
    selected_map: Option<Target>,
    selected_package: Option<Target>,
    drafts: BTreeMap<Target, Draft>,
    pub saving: bool,
    pub error: Option<String>,
}

/// One request a save sends: creating the group's grant over a resource, or
/// changing the one it holds there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Write {
    Create(GrantScope, GrantBody),
    Change(Uuid, GrantChange),
}

impl Permissions {
    pub fn new(group: &ClanGroup, page: &ClanPage, viewer: Option<Uuid>) -> Self {
        let mut editor = Self {
            group: group.id,
            name: group.name.clone(),
            owners: group.builtin.as_deref() == Some("owners"),
            viewer,
            tab: Tab::Clan,
            selected_map: None,
            selected_package: None,
            drafts: BTreeMap::new(),
            saving: false,
            error: None,
        };
        editor.ensure(Target::CLAN, page);
        editor
    }

    fn active(&self) -> Option<Target> {
        match self.tab {
            Tab::Clan => Some(Target::CLAN),
            Tab::Maps => self.selected_map,
            Tab::Packages => self.selected_package,
        }
    }

    /// The group's grant over exactly `target`, if it holds one.
    fn direct(&self, target: Target, page: &ClanPage) -> Option<ClanGrant> {
        page.grants_to(self.group)
            .into_iter()
            .find(|g| target.exact(g))
            .cloned()
    }

    /// Inheritance follows the pending folder edits as well as saved grants.
    /// A new permission has no grant ID until Save, so it cannot open an
    /// original-grant editor yet.
    fn inherited(
        &self,
        target: Target,
        page: &ClanPage,
    ) -> Vec<(GrantScope, BTreeSet<String>, Option<Uuid>)> {
        let mut inherited = Vec::new();
        for grant in page
            .grants_to(self.group)
            .into_iter()
            .filter(|g| !target.exact(g) && covers(g, target, page))
        {
            let actions = self
                .drafts
                .iter()
                .find(|(scope, _)| scope.exact(grant))
                .map_or_else(
                    || grant.actions.clone(),
                    |(_, draft)| {
                        grant
                            .actions
                            .intersection(&draft.actions)
                            .cloned()
                            .collect()
                    },
                );
            if !actions.is_empty() {
                inherited.push((grant.scope.clone(), actions, Some(grant.id)));
            }
        }
        for (scope, draft) in &self.drafts {
            if *scope == target || !scope_covers(&scope.scope(), target, page) {
                continue;
            }
            let (before, _) = draft.before();
            let added: BTreeSet<_> = draft.actions.difference(&before).cloned().collect();
            if !added.is_empty() {
                inherited.push((scope.scope(), added, None));
            }
        }
        inherited
    }

    fn ensure(&mut self, target: Target, page: &ClanPage) {
        if !self.drafts.contains_key(&target) {
            self.drafts
                .insert(target, Draft::new(self.direct(target, page)));
        }
    }

    /// A failed multi-request save reloads the committed prefix. Keeping
    /// the desired actions but refreshing originals makes retry idempotent.
    pub fn refresh(&mut self, page: &ClanPage) {
        for target in self.drafts.keys().copied().collect::<Vec<_>>() {
            let original = self.direct(target, page);
            if let Some(draft) = self.drafts.get_mut(&target) {
                let current = Draft::new(original);
                let before = Draft::new(draft.original.clone());
                let rebase =
                    |desired: &BTreeSet<String>, old: &BTreeSet<String>, new: &BTreeSet<String>| {
                        let removed: BTreeSet<_> = old.difference(desired).cloned().collect();
                        new.difference(&removed)
                            .cloned()
                            .chain(desired.difference(old).cloned())
                            .collect()
                    };
                draft.actions = rebase(&draft.actions, &before.actions, &current.actions);
                draft.ceiling = rebase(&draft.ceiling, &before.ceiling, &current.ceiling);
                draft.original = current.original;
            }
        }
        self.saving = false;
    }

    pub fn update(&mut self, message: Message, page: &ClanPage) {
        if self.saving {
            return;
        }
        self.error = None;
        match message {
            Message::Tab(tab) => self.tab = tab,
            Message::Select(target) => {
                self.ensure(target, page);
                match target.kind {
                    ScopeKind::Packages => self.selected_package = Some(target),
                    _ => self.selected_map = Some(target),
                }
            }
            Message::Toggle(action, on) => self.toggle(action, on, false, page),
            Message::Ceiling(action, on) => self.toggle(action, on, true, page),
            // A preset is the switches it sets, each under the rules of a
            // single toggle.
            Message::Preset(pick) => {
                for (action, on) in pick.changes() {
                    self.toggle(action, on, false, page);
                }
            }
            Message::CeilingPreset(pick) => {
                for (action, on) in pick.changes() {
                    self.toggle(action, on, true, page);
                }
            }
        }
    }

    fn toggle(&mut self, action: &'static str, on: bool, ceiling: bool, page: &ClanPage) {
        let Some(target) = self.active() else {
            return;
        };
        if self.implicit(target, page) || !self.allowed(target, action, page) {
            return;
        }
        if !ceiling
            && self
                .inherited(target, page)
                .iter()
                .any(|(_, actions, _)| actions.contains(action))
        {
            return;
        }
        if ceiling && !page.clan.is_owner {
            return;
        }
        let member_map = self.member_map(target, page);
        if let Some(draft) = self.drafts.get_mut(&target) {
            if member_map
                && !ceiling
                && !on
                && action == action::READ_AREA
                && draft.actions.iter().any(|a| a != action::READ_AREA)
            {
                return;
            }
            let set = if ceiling {
                &mut draft.ceiling
            } else {
                &mut draft.actions
            };
            if on {
                set.insert(action.to_string());
                if member_map && !ceiling {
                    set.insert(action::READ_AREA.to_string());
                }
            } else {
                set.remove(action);
            }
        }
    }

    /// Whether `target` is a Member-owned map.
    fn member_map(&self, target: Target, page: &ClanPage) -> bool {
        target.kind == ScopeKind::Maps
            && page
                .maps
                .iter()
                .any(|map| Some(map.id) == target.id && !map.clan_owned())
    }

    fn implicit(&self, target: Target, page: &ClanPage) -> bool {
        self.owners
            && (target.kind != ScopeKind::Maps
                || page
                    .maps
                    .iter()
                    .any(|r| Some(r.id) == target.id && r.clan_owned()))
    }

    fn allowed(&self, target: Target, permission: &str, page: &ClanPage) -> bool {
        if !presets::allowed_on(permission, target.kind) {
            return false;
        }
        if target.kind == ScopeKind::Maps {
            if let Some(map) = page
                .maps
                .iter()
                .find(|r| Some(r.id) == target.id && !r.clan_owned())
            {
                return map.owned_by_me && presets::takes_on_member_map(permission);
            }
        }
        if page.clan.is_owner {
            return true;
        }
        if permission.starts_with("grant.") || target.kind == ScopeKind::Clan {
            return false;
        }
        let resource = match target.kind {
            ScopeKind::Folders => page.folders.iter().find(|r| Some(r.id) == target.id),
            ScopeKind::Maps => page.maps.iter().find(|r| Some(r.id) == target.id),
            ScopeKind::Packages => page.packages.iter().find(|r| Some(r.id) == target.id),
            _ => None,
        };
        if !resource.is_some_and(|r| r.can(action::MANAGE_GRANTS)) {
            return false;
        }
        // The server remains authoritative. A delegate changes the
        // permissions within one of their covering delegations, whoever
        // gave them, except the grant's own on a grant that itself hands
        // out access (clans.md §1.2).
        let within = self.delegations(target, page).any(|grant| {
            grant
                .may_grant
                .as_ref()
                .is_some_and(|may| may.contains(permission))
        });
        let protected = self
            .drafts
            .get(&target)
            .and_then(|draft| draft.original.as_ref())
            .is_some_and(|grant| {
                grant.actions.contains(action::MANAGE_GRANTS)
                    && grant.direct_actions().contains(permission)
            });
        within && !protected
    }

    fn delegations<'a>(
        &'a self,
        target: Target,
        page: &'a ClanPage,
    ) -> impl Iterator<Item = &'a ClanGrant> + 'a {
        page.grants.iter().filter(move |grant| {
            let held = match grant.recipient {
                GrantRecipient::Group { group_id } => page.clan.group_ids.contains(&group_id),
                GrantRecipient::User { user_id } => Some(user_id) == self.viewer,
            };
            held && grant.actions.contains(action::MANAGE_GRANTS)
                && grant.scope != GrantScope::Clan
                && covers(grant, target, page)
        })
    }

    /// What a save sends: for each resource changed, the group's grant
    /// created, or a change naming exactly the switches changed, so another
    /// editor's changes meanwhile stay.
    #[must_use]
    pub fn writes(&self) -> Vec<Write> {
        let mut writes = Vec::new();
        for (target, draft) in &self.drafts {
            let ceiling = draft.after_ceiling();
            match &draft.original {
                None if draft.actions.is_empty() => {}
                None => writes.push(Write::Create(
                    target.scope(),
                    GrantBody {
                        actions: draft.actions.iter().cloned().collect(),
                        may_grant: draft
                            .actions
                            .contains(action::MANAGE_GRANTS)
                            .then(|| ceiling.into_iter().collect()),
                    },
                )),
                Some(grant) => {
                    let (actions, before_ceiling) = draft.before();
                    let change =
                        GrantChange::between(&actions, &before_ceiling, &draft.actions, &ceiling);
                    if !change.is_empty() {
                        writes.push(Write::Change(grant.id, change));
                    }
                }
            }
        }
        writes
    }
}

fn covers(grant: &ClanGrant, target: Target, page: &ClanPage) -> bool {
    scope_covers(&grant.scope, target, page)
}

fn scope_covers(scope: &GrantScope, target: Target, page: &ClanPage) -> bool {
    if target.kind == ScopeKind::Maps
        && page
            .maps
            .iter()
            .any(|r| Some(r.id) == target.id && !r.clan_owned())
    {
        return *scope == target.scope();
    }
    match (scope, target.kind, target.id) {
        (GrantScope::Clan, ScopeKind::Clan, _) => true,
        (GrantScope::Atlases { ids }, ScopeKind::Folders, Some(id)) => {
            ids.iter().any(|v| v.0 == id)
        }
        (GrantScope::Atlases { ids }, ScopeKind::Maps, Some(id)) => page
            .maps
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.atlas_id)
            .is_some_and(|atlas| ids.contains(&atlas)),
        (GrantScope::Areas { ids }, ScopeKind::Maps, Some(id)) => ids.iter().any(|v| v.0 == id),
        (GrantScope::Packages { ids }, ScopeKind::Packages, Some(id)) => ids.contains(&id),
        _ => false,
    }
}

fn send(message: Message) -> PanelMessage {
    PanelMessage::GroupPermissions(message)
}

fn select<'a>(
    name: String,
    target: Target,
    active: Option<Target>,
    depth: u16,
) -> ThemedElement<'a, PanelMessage> {
    container(
        button(text(name).size(13))
            .padding([9, 10])
            .width(Length::Fill)
            .style(if active == Some(target) {
                theme::builtins::button::primary
            } else {
                theme::builtins::button::secondary
            })
            .on_press(send(Message::Select(target))),
    )
    .padding(iced::Padding::default().left(f32::from(depth)))
    .into()
}

pub fn view<'a>(editor: &'a Permissions, page: &'a ClanPage) -> ThemedElement<'a, PanelMessage> {
    let tab = |which, key| {
        button(text(crate::i18n::translate(key)).size(14))
            .padding([8, 16])
            .style(if editor.tab == which {
                theme::builtins::button::primary
            } else {
                theme::builtins::button::secondary
            })
            .on_press(send(Message::Tab(which)))
    };
    let tabs = row![
        tab(Tab::Clan, "permissions-tab-clan"),
        tab(Tab::Maps, "clans-access-maps"),
        tab(Tab::Packages, "clans-access-packages")
    ]
    .spacing(6);
    let active = editor.active();
    let details: ThemedElement<'a, PanelMessage> = match active {
        Some(target) => detail(editor, page, target),
        None => text(crate::i18n::t!("permissions-select-resource"))
            .size(14)
            .style(permission_rows::description)
            .into(),
    };
    let body: ThemedElement<'a, PanelMessage> = if editor.tab == Tab::Clan {
        scrollable(container(details).padding(iced::Padding::default().right(16)))
            .height(Length::Fill)
            .into()
    } else {
        let mut resources = column![].spacing(4);
        if editor.tab == Tab::Packages {
            for package in &page.packages {
                resources = resources.push(select(
                    package.name.clone(),
                    Target {
                        kind: ScopeKind::Packages,
                        id: Some(package.id),
                    },
                    active,
                    0,
                ));
            }
        } else {
            for folder in &page.folders {
                resources = resources.push(select(
                    folder.name.clone(),
                    Target {
                        kind: ScopeKind::Folders,
                        id: Some(folder.id),
                    },
                    active,
                    0,
                ));
                for map in page
                    .maps
                    .iter()
                    .filter(|m| m.atlas_id.is_some_and(|id| id.0 == folder.id))
                {
                    resources = resources.push(select(
                        map.name.clone(),
                        Target {
                            kind: ScopeKind::Maps,
                            id: Some(map.id),
                        },
                        active,
                        16,
                    ));
                }
            }
            let loose: Vec<_> = page
                .maps
                .iter()
                .filter(|m| {
                    m.atlas_id
                        .is_none_or(|id| !page.folders.iter().any(|f| f.id == id.0))
                })
                .collect();
            if !loose.is_empty() {
                resources = resources.push(
                    text(crate::i18n::t!("permissions-other-maps"))
                        .size(12)
                        .style(permission_rows::description),
                );
                for map in loose {
                    resources = resources.push(select(
                        map.name.clone(),
                        Target {
                            kind: ScopeKind::Maps,
                            id: Some(map.id),
                        },
                        active,
                        0,
                    ));
                }
            }
        }
        row![
            scrollable(resources).width(Length::FillPortion(1)),
            rule::vertical(1),
            scrollable(container(details).padding(iced::Padding::default().right(16)))
                .width(Length::FillPortion(2))
        ]
        .spacing(18)
        .height(Length::Fill)
        .into()
    };
    let cancel = button(text(crate::i18n::t!("action-cancel")))
        .on_press_maybe((!editor.saving).then_some(PanelMessage::CloseModal));
    let mut footer = row![space::horizontal(), cancel]
        .spacing(10)
        .align_y(Alignment::Center);
    let mut save = button(text(if editor.saving {
        crate::i18n::t!("mapper-saving")
    } else {
        crate::i18n::t!("action-save")
    }))
    .style(theme::builtins::button::primary);
    if !editor.saving && !editor.writes().is_empty() {
        save = save.on_press(PanelMessage::SaveGroupPermissions);
    }
    footer = footer.push(save);
    let mut col = column![
        text(crate::i18n::t!("permissions-title", "group" => editor.name.clone())).size(20),
        tabs,
        rule::horizontal(1),
        body
    ]
    .spacing(14);
    if let Some(error) = &editor.error {
        col = col.push(text(error).style(theme::builtins::text::danger));
    }
    container(col.push(footer))
        .padding(22)
        .width(Length::Fill)
        .height(Length::Fill)
        .max_width(940)
        .max_height(760)
        .style(theme::builtins::container::modal_card)
        .into()
}

fn detail<'a>(
    editor: &'a Permissions,
    page: &'a ClanPage,
    target: Target,
) -> ThemedElement<'a, PanelMessage> {
    let Some(draft) = editor.drafts.get(&target) else {
        return space::vertical().into();
    };
    let implicit = editor.implicit(target, page);
    let inherited = editor.inherited(target, page);
    let context = super::editor::EditorContext {
        groups: &page.groups,
        members: page.members.as_deref(),
        folders: &page.folders,
        maps: &page.maps,
        packages: &page.packages,
        grants: &page.grants,
        owner: page.clan.is_owner,
    };
    let mut col = column![text(context.scope_label(&target.scope())).size(17)].spacing(10);
    if implicit {
        col = col.push(
            text(crate::i18n::t!("permissions-owners-implicit"))
                .size(13)
                .style(permission_rows::description),
        );
    } else if target.kind == ScopeKind::Clan {
        col = col.push(
            text(crate::i18n::t!("permissions-clan-note"))
                .size(13)
                .style(permission_rows::description),
        );
    } else if target.kind == ScopeKind::Folders {
        col = col.push(
            text(crate::i18n::t!("permissions-folder-inheritance"))
                .size(13)
                .style(permission_rows::description),
        );
    } else if target.kind == ScopeKind::Maps
        && page
            .maps
            .iter()
            .any(|map| Some(map.id) == target.id && !map.clan_owned())
    {
        col = col.push(
            text(crate::i18n::t!("clan-share-member-owned-reach"))
                .size(13)
                .style(permission_rows::description),
        );
    } else {
        col = col.push(
            text(crate::i18n::t!("permissions-direct-note"))
                .size(13)
                .style(permission_rows::description),
        );
    }
    for (scope, actions, origin) in &inherited {
        let scope = context.scope_label(scope);
        let labels = actions
            .iter()
            .map(|a| presets::action_label(a))
            .collect::<Vec<_>>()
            .join(", ");
        col = col.push(
            text(
                crate::i18n::t!("permissions-inherited", "scope" => scope, "permissions" => labels),
            )
            .size(13)
            .style(permission_rows::description),
        );
        if let Some(grant) = origin.and_then(|id| page.grants.iter().find(|grant| grant.id == id))
            && page.can_change(grant)
            && editor.writes().is_empty()
        {
            col = col.push(
                button(text(crate::i18n::t!("permissions-edit-origin")).size(12))
                    .style(theme::builtins::button::quiet_link)
                    .on_press(PanelMessage::EditGrant(grant.id)),
            );
        }
    }
    for (heading, actions) in sections(target.kind) {
        let actions: Vec<_> = actions
            .into_iter()
            .filter(|a| {
                presets::allowed_on(a, target.kind)
                    || (implicit && target.kind == ScopeKind::Folders && *a == action::DELETE_ATLAS)
            })
            .filter(|a| {
                target.kind != ScopeKind::Maps
                    || !page
                        .maps
                        .iter()
                        .any(|m| Some(m.id) == target.id && !m.clan_owned())
                    || presets::takes_on_member_map(a)
            })
            .collect();
        if actions.is_empty() {
            continue;
        }
        let rows: Vec<Switch> = actions
            .into_iter()
            .map(|permission| {
                let inherited_here = inherited
                    .iter()
                    .any(|(_, actions, _)| actions.contains(permission));
                let implied = (permission == action::READ_AREA
                    && editor.member_map(target, page)
                    && draft.actions.iter().any(|a| a != action::READ_AREA))
                    || permission == action::INSPECT_GRANTS
                        && (draft.actions.contains(action::MANAGE_GRANTS)
                            || inherited
                                .iter()
                                .any(|(_, actions, _)| actions.contains(action::MANAGE_GRANTS)));
                Switch {
                    action: permission,
                    checked: implicit
                        || inherited_here
                        || implied
                        || draft.actions.contains(permission),
                    enabled: !implicit
                        && !inherited_here
                        && !implied
                        && !editor.saving
                        && editor.allowed(target, permission, page),
                }
            })
            .collect();
        col = col.push(text(heading).size(16));
        for picker in pickers(&rows, |preset| preset.applies_to(target.kind)) {
            let usable = picker.choices.len() > 1;
            col = col.push(picker.view(
                crate::i18n::t!("permissions-preset"),
                usable.then_some(|pick| send(Message::Preset(pick))),
            ));
        }
        for switch in rows {
            let permission = switch.action;
            col = col.push(permission_rows::noted(
                permission,
                switch.checked,
                switch.enabled,
                delegated_note(draft, permission, &context),
                move |on| send(Message::Toggle(permission, on)),
            ));
        }
    }
    if !implicit && draft.actions.contains(action::MANAGE_GRANTS) {
        col = col.push(text(crate::i18n::t!("permissions-delegation-title")).size(16));
        col = col.push(
            text(crate::i18n::t!("permissions-delegation-help"))
                .size(13)
                .style(permission_rows::description),
        );
        let rows: Vec<Switch> = Kind::ALL
            .into_iter()
            .filter(|k| *k != Kind::Secret)
            .flat_map(Kind::actions)
            .filter(|a| {
                presets::allowed_on(a, target.kind)
                    && !a.starts_with("grant.")
                    && !a.starts_with("clan.")
                    && !a.starts_with("group.")
            })
            .map(|permission| Switch {
                action: permission,
                checked: draft.ceiling.contains(permission),
                enabled: page.clan.is_owner && !editor.saving,
            })
            .collect();
        for picker in pickers(&rows, |preset| {
            preset.applies_to(target.kind) && !preset.delegates()
        }) {
            let usable = picker.choices.len() > 1;
            col = col.push(picker.view(
                picker.facet.kind.label(),
                usable.then_some(|pick| send(Message::CeilingPreset(pick))),
            ));
        }
        for switch in rows {
            let permission = switch.action;
            col = col.push(permission_rows::permission(
                permission,
                switch.checked,
                switch.enabled,
                move |on| send(Message::Ceiling(permission, on)),
            ));
        }
    }
    col.into()
}

/// Whose access management added `action` to the group's grant, when it
/// came through a delegation: it ends with that delegation (clans.md §1.2).
fn delegated_note(
    draft: &Draft,
    action: &str,
    context: &super::editor::EditorContext<'_>,
) -> Option<String> {
    let grant = draft.original.as_ref()?;
    if !draft.actions.contains(action) {
        return None;
    }
    context.delegated_note(&grant.delegations_of(action))
}

fn sections(scope: ScopeKind) -> Vec<(String, Vec<&'static str>)> {
    if scope == ScopeKind::Clan {
        return vec![
            (
                crate::i18n::t!("permissions-section-clan"),
                Kind::ClanAdministration
                    .actions()
                    .filter(|a| a.starts_with("clan."))
                    .collect(),
            ),
            (
                crate::i18n::t!("permissions-section-groups"),
                Kind::ClanAdministration
                    .actions()
                    .filter(|a| a.starts_with("group."))
                    .collect(),
            ),
            (
                crate::i18n::t!("permissions-section-creation"),
                vec![action::CREATE_ATLAS, action::CREATE_PACKAGE],
            ),
        ];
    }
    let mut sections = vec![
        (
            Kind::FolderCuration.label(),
            vec![
                action::READ_ATLAS,
                action::RENAME_ATLAS,
                action::ACCEPT_FILING,
                action::ACCEPT_TRANSFER,
                action::CREATE_AREA,
                action::CREATE_MEMBER_OWNED_AREA,
                action::RENAME_AREA,
                action::REFILE_AREA,
                action::DELETE_ATLAS,
            ],
        ),
        (
            Kind::Map.label(),
            vec![
                action::READ_AREA,
                action::ADD_TO_AREA,
                action::EDIT_AREA,
                action::REMOVE_FROM_AREA,
                action::COPY_AREA,
                action::SHARE_AREA_EXTERNALLY,
                action::DELETE_AREA,
            ],
        ),
        (
            crate::i18n::t!("permissions-section-secret-creation"),
            vec![
                action::CREATE_MEMBER_OWNED_SECRET,
                action::CREATE_CLAN_OWNED_SECRET,
            ],
        ),
        (
            Kind::ClanSecrets.label(),
            Kind::ClanSecrets.actions().collect(),
        ),
        (Kind::Package.label(), Kind::Package.actions().collect()),
    ];
    sections.push((
        crate::i18n::t!("permissions-section-access"),
        vec![action::INSPECT_GRANTS, action::MANAGE_GRANTS],
    ));
    sections
}

#[cfg(test)]
mod tests;
