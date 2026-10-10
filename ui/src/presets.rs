//! Permission presets: the client's named bundles of the server's inline
//! actions (clans.md §5.2, §8.4).
//!
//! The service stores each grant's own actions and has no roles. A preset is
//! a fixed action set of one [`Kind`]; a grant shows a preset's name only
//! when its actions are exactly that set, and "Custom" otherwise
//! ([`matching_preset`], [`preset_or_custom`]).
//!
//! Each kind also has actions that are never in a preset
//! ([`Kind::separate`]): they are offered as their own checkboxes beside the
//! preset choice, so giving one is always a deliberate step. The rest of a
//! kind's actions are its core ([`Kind::core`]), which the presets are made
//! of and which Custom shows as checkboxes.
//!
//! The clan actions come from [`smudgy_cloud::clans::action`]; one Secret's
//! own grants use the Secret actions (`read`, `add`, ...), which
//! [`Kind::Secret`] covers.

use std::collections::BTreeSet;

use smudgy_cloud::clans::action;

/// The Secret actions a grant on one Secret carries (clans.md §8.4).
pub mod secret_action {
    /// Reading its content; every grant holds it.
    pub const READ: &str = "read";
    /// Adding content to it.
    pub const ADD: &str = "add";
    /// Changing its content.
    pub const EDIT: &str = "edit";
    /// Removing its content.
    pub const REMOVE: &str = "remove";
    /// Seeing who reads it, and sharing it within one's own actions.
    pub const MANAGE_ACCESS: &str = "manage_access";
    /// Taking it along when copying its map. Never in a preset.
    pub const COPY: &str = "copy";
}

/// What a set of actions is about. Each kind has its own presets and its own
/// separate actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// Map content: on one map, a folder's maps, or every Clan-owned map.
    Map,
    /// Looking after folders: renaming, filing, accepting maps, creating
    /// maps.
    FolderCuration,
    /// Every Clan-owned Secret on a map, a folder's maps, or the clan's maps.
    ClanSecrets,
    /// One Secret's own grant (Secret actions, not clan actions).
    Secret,
    /// One package, several, or all of the clan's.
    Package,
    /// The clan's members, groups, and handing out access.
    ClanAdministration,
}

/// A named, fixed bundle of one kind's actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Preset {
    /// Map: `area.read`.
    MapReader,
    /// Map: Reader with `area.add`, `area.edit`.
    MapContributor,
    /// Map: Contributor with `area.remove_content`.
    MapEditor,
    /// Folder upkeep: renaming, filing and accepting maps, creating,
    /// renaming and moving maps.
    Curator,
    /// Clan-owned Secrets: `secret.read`.
    SecretsReader,
    /// Clan-owned Secrets: Reader with `secret.add`, `secret.edit`.
    SecretsContributor,
    /// Clan-owned Secrets: Contributor with `secret.remove_content`.
    SecretsEditor,
    /// One Secret: `read`.
    SecretReader,
    /// One Secret: `read`, `add`, `edit`.
    SecretContributor,
    /// One Secret: `read`, `add`, `edit`, `remove`.
    SecretEditor,
    /// One Secret: `read`, `manage_access`.
    SecretAccessManager,
    /// Package: `package.read`.
    PackageReader,
    /// Package: Reader with `package.edit_draft`.
    PackageDrafter,
    /// Package: Drafter with `package.edit_metadata`, `package.publish`,
    /// `package.retire`.
    PackageMaintainer,
    /// Clan administration: `clan.read_members`, `clan.invite`.
    Recruiter,
    /// Clan administration: Recruiter with `clan.revoke_invitation`,
    /// `clan.remove_member`.
    Officer,
    /// Clan administration on named groups: `group.inspect_assignments`,
    /// `group.assign`.
    GroupLead,
    /// Handing out access within a scope: `grant.inspect`, `grant.manage`,
    /// with a map preset as what it may hand out ([`delegable`]). Only clan
    /// owners give it.
    FolderManager,
}

/// What a grant covers, as the server's scope kinds name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ScopeKind {
    /// Clan administration, group administration and resource creation.
    /// Resource access requires a named scope.
    Clan,
    /// Named groups.
    Groups,
    /// Named folders and the maps filed in them, now and later.
    Folders,
    /// Exactly the named maps.
    Maps,
    /// Exactly the named packages.
    Packages,
}

/// How a grant's actions read as chips: preset names where a part of it is
/// exactly a preset, and single actions otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Chip {
    Preset(Preset),
    Action(&'static str),
}

impl Chip {
    /// The chip's text.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Preset(preset) => preset.label(),
            Self::Action(action) => action_label(action),
        }
    }
}

const MAP_READER: &[&str] = &[action::READ_AREA];
const MAP_CONTRIBUTOR: &[&str] = &[action::READ_AREA, action::ADD_TO_AREA, action::EDIT_AREA];
const MAP_EDITOR: &[&str] = &[
    action::READ_AREA,
    action::ADD_TO_AREA,
    action::EDIT_AREA,
    action::REMOVE_FROM_AREA,
];
const CURATOR: &[&str] = &[
    action::RENAME_ATLAS,
    action::ACCEPT_FILING,
    action::ACCEPT_TRANSFER,
    action::CREATE_AREA,
    action::RENAME_AREA,
    action::REFILE_AREA,
];
const SECRETS_READER: &[&str] = &[action::READ_SECRETS];
const SECRETS_CONTRIBUTOR: &[&str] = &[
    action::READ_SECRETS,
    action::ADD_TO_SECRETS,
    action::EDIT_SECRETS,
];
const SECRETS_EDITOR: &[&str] = &[
    action::READ_SECRETS,
    action::ADD_TO_SECRETS,
    action::EDIT_SECRETS,
    action::REMOVE_FROM_SECRETS,
];
const SECRET_READER: &[&str] = &[secret_action::READ];
const SECRET_CONTRIBUTOR: &[&str] = &[secret_action::READ, secret_action::ADD, secret_action::EDIT];
const SECRET_EDITOR: &[&str] = &[
    secret_action::READ,
    secret_action::ADD,
    secret_action::EDIT,
    secret_action::REMOVE,
];
const SECRET_ACCESS_MANAGER: &[&str] = &[secret_action::READ, secret_action::MANAGE_ACCESS];
const PACKAGE_READER: &[&str] = &[action::READ_PACKAGE];
const PACKAGE_DRAFTER: &[&str] = &[action::READ_PACKAGE, action::EDIT_PACKAGE_DRAFT];
const PACKAGE_MAINTAINER: &[&str] = &[
    action::READ_PACKAGE,
    action::EDIT_PACKAGE_DRAFT,
    action::EDIT_PACKAGE_METADATA,
    action::PUBLISH_PACKAGE,
    action::RETIRE_PACKAGE_VERSION,
];
const RECRUITER: &[&str] = &[action::READ_MEMBERS, action::INVITE];
const OFFICER: &[&str] = &[
    action::READ_MEMBERS,
    action::INVITE,
    action::REVOKE_INVITATION,
    action::REMOVE_MEMBER,
];
const GROUP_LEAD: &[&str] = &[action::INSPECT_GROUP, action::ASSIGN_GROUP];
const FOLDER_MANAGER: &[&str] = &[action::INSPECT_GRANTS, action::MANAGE_GRANTS];

/// The membership part of clan administration.
const MEMBERSHIP_FACET: &[&str] = OFFICER;

impl Kind {
    /// Every kind, in the order a picker lists them.
    pub const ALL: [Self; 6] = [
        Self::Map,
        Self::FolderCuration,
        Self::ClanSecrets,
        Self::Secret,
        Self::Package,
        Self::ClanAdministration,
    ];

    /// The kind's presets, smallest first.
    #[must_use]
    pub const fn presets(self) -> &'static [Preset] {
        match self {
            Self::Map => &[Preset::MapReader, Preset::MapContributor, Preset::MapEditor],
            Self::FolderCuration => &[Preset::Curator],
            Self::ClanSecrets => &[
                Preset::SecretsReader,
                Preset::SecretsContributor,
                Preset::SecretsEditor,
            ],
            Self::Secret => &[
                Preset::SecretReader,
                Preset::SecretContributor,
                Preset::SecretEditor,
                Preset::SecretAccessManager,
            ],
            Self::Package => &[
                Preset::PackageReader,
                Preset::PackageDrafter,
                Preset::PackageMaintainer,
            ],
            Self::ClanAdministration => &[
                Preset::Recruiter,
                Preset::Officer,
                Preset::GroupLead,
                Preset::FolderManager,
            ],
        }
    }

    /// The actions the kind's presets are made of, which Custom offers as
    /// checkboxes.
    #[must_use]
    pub const fn core(self) -> &'static [&'static str] {
        match self {
            Self::Map => MAP_EDITOR,
            Self::FolderCuration => CURATOR,
            Self::ClanSecrets => SECRETS_EDITOR,
            Self::Secret => &[
                secret_action::READ,
                secret_action::ADD,
                secret_action::EDIT,
                secret_action::REMOVE,
                secret_action::MANAGE_ACCESS,
            ],
            Self::Package => PACKAGE_MAINTAINER,
            Self::ClanAdministration => &[
                action::READ_MEMBERS,
                action::INVITE,
                action::REVOKE_INVITATION,
                action::REMOVE_MEMBER,
                action::INSPECT_GROUP,
                action::ASSIGN_GROUP,
                action::INSPECT_GRANTS,
                action::MANAGE_GRANTS,
            ],
        }
    }

    /// The kind's actions that are never in a preset: separate checkboxes,
    /// held by clan owners alone until someone gives them.
    #[must_use]
    pub const fn separate(self) -> &'static [&'static str] {
        match self {
            Self::Map => &[
                action::COPY_AREA,
                action::SHARE_AREA_EXTERNALLY,
                action::DELETE_AREA,
                action::CREATE_MEMBER_OWNED_SECRET,
                action::CREATE_CLAN_OWNED_SECRET,
            ],
            Self::FolderCuration => &[
                action::CREATE_MEMBER_OWNED_AREA,
                action::CREATE_ATLAS,
                action::READ_ATLAS,
            ],
            Self::ClanSecrets => &[action::MANAGE_SECRET_ACCESS, action::COPY_SECRETS],
            Self::Secret => &[secret_action::COPY],
            Self::Package => &[
                action::MANAGE_PACKAGE_AVAILABILITY,
                action::DELETE_PACKAGE,
                action::CREATE_PACKAGE,
            ],
            Self::ClanAdministration => &[
                action::EDIT_PROFILE,
                action::CREATE_GROUP,
                action::RENAME_GROUP,
                action::DELETE_GROUP,
            ],
        }
    }

    /// Every action of the kind: its core, then its separate actions.
    pub fn actions(self) -> impl Iterator<Item = &'static str> {
        self.core().iter().chain(self.separate()).copied()
    }

    /// The kind an action belongs to, or `None` for an action this client
    /// does not know (and `atlas.delete`, which no grant carries).
    #[must_use]
    pub fn of_action(action: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.actions().any(|known| known == action))
    }

    /// Whether some of the kind's actions apply to a grant of `scope`.
    #[must_use]
    pub fn applies_to(self, scope: ScopeKind) -> bool {
        self.actions().any(|action| allowed_on(action, scope))
    }

    /// The kind's actions that apply to a grant of `scope`, core first.
    #[must_use]
    pub fn actions_on(self, scope: ScopeKind) -> Vec<&'static str> {
        self.actions()
            .filter(|action| allowed_on(action, scope))
            .collect()
    }

    /// The kind's presets whose every action applies to `scope`.
    #[must_use]
    pub fn presets_on(self, scope: ScopeKind) -> Vec<Preset> {
        self.presets()
            .iter()
            .copied()
            .filter(|preset| preset.applies_to(scope))
            .collect()
    }

    /// The kind's name.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Map => crate::i18n::t!("presets-kind-map"),
            Self::FolderCuration => crate::i18n::t!("presets-kind-folder-curation"),
            Self::ClanSecrets => crate::i18n::t!("presets-kind-clan-secrets"),
            Self::Secret => crate::i18n::t!("presets-kind-secret"),
            Self::Package => crate::i18n::t!("presets-kind-package"),
            Self::ClanAdministration => crate::i18n::t!("presets-kind-clan-administration"),
        }
    }

    /// The parts of the kind's core that presets match one at a time: one
    /// for most kinds; membership, groups and handing out access for clan
    /// administration, whose presets each cover one of those.
    fn facets(self) -> Vec<&'static [&'static str]> {
        match self {
            Self::ClanAdministration => vec![MEMBERSHIP_FACET, GROUP_LEAD, FOLDER_MANAGER],
            _ => vec![self.core()],
        }
    }
}

impl Preset {
    /// Every preset, kind by kind.
    pub const ALL: [Self; 18] = [
        Self::MapReader,
        Self::MapContributor,
        Self::MapEditor,
        Self::Curator,
        Self::SecretsReader,
        Self::SecretsContributor,
        Self::SecretsEditor,
        Self::SecretReader,
        Self::SecretContributor,
        Self::SecretEditor,
        Self::SecretAccessManager,
        Self::PackageReader,
        Self::PackageDrafter,
        Self::PackageMaintainer,
        Self::Recruiter,
        Self::Officer,
        Self::GroupLead,
        Self::FolderManager,
    ];

    /// The preset's kind.
    #[must_use]
    pub const fn kind(self) -> Kind {
        match self {
            Self::MapReader | Self::MapContributor | Self::MapEditor => Kind::Map,
            Self::Curator => Kind::FolderCuration,
            Self::SecretsReader | Self::SecretsContributor | Self::SecretsEditor => {
                Kind::ClanSecrets
            }
            Self::SecretReader
            | Self::SecretContributor
            | Self::SecretEditor
            | Self::SecretAccessManager => Kind::Secret,
            Self::PackageReader | Self::PackageDrafter | Self::PackageMaintainer => Kind::Package,
            Self::Recruiter | Self::Officer | Self::GroupLead | Self::FolderManager => {
                Kind::ClanAdministration
            }
        }
    }

    /// The exact actions a grant of this preset carries.
    #[must_use]
    pub const fn actions(self) -> &'static [&'static str] {
        match self {
            Self::MapReader => MAP_READER,
            Self::MapContributor => MAP_CONTRIBUTOR,
            Self::MapEditor => MAP_EDITOR,
            Self::Curator => CURATOR,
            Self::SecretsReader => SECRETS_READER,
            Self::SecretsContributor => SECRETS_CONTRIBUTOR,
            Self::SecretsEditor => SECRETS_EDITOR,
            Self::SecretReader => SECRET_READER,
            Self::SecretContributor => SECRET_CONTRIBUTOR,
            Self::SecretEditor => SECRET_EDITOR,
            Self::SecretAccessManager => SECRET_ACCESS_MANAGER,
            Self::PackageReader => PACKAGE_READER,
            Self::PackageDrafter => PACKAGE_DRAFTER,
            Self::PackageMaintainer => PACKAGE_MAINTAINER,
            Self::Recruiter => RECRUITER,
            Self::Officer => OFFICER,
            Self::GroupLead => GROUP_LEAD,
            Self::FolderManager => FOLDER_MANAGER,
        }
    }

    /// The preset's actions as owned strings, as a grant body or a
    /// comparison wants them.
    #[must_use]
    pub fn action_set(self) -> BTreeSet<String> {
        self.actions().iter().map(ToString::to_string).collect()
    }

    /// Whether the preset hands out access, and so carries `may_grant`
    /// beside its actions. Only clan owners give such a grant.
    #[must_use]
    pub const fn delegates(self) -> bool {
        matches!(self, Self::FolderManager)
    }

    /// Whether every action of the preset applies to a grant of `scope`.
    #[must_use]
    pub fn applies_to(self, scope: ScopeKind) -> bool {
        self.actions()
            .iter()
            .all(|action| allowed_on(action, scope))
    }

    /// The preset's name.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::MapReader | Self::SecretsReader | Self::SecretReader | Self::PackageReader => {
                crate::i18n::t!("presets-reader")
            }
            Self::MapContributor | Self::SecretsContributor | Self::SecretContributor => {
                crate::i18n::t!("presets-contributor")
            }
            Self::MapEditor | Self::SecretsEditor | Self::SecretEditor => {
                crate::i18n::t!("presets-editor")
            }
            Self::Curator => crate::i18n::t!("presets-curator"),
            Self::SecretAccessManager => crate::i18n::t!("presets-access-manager"),
            Self::PackageDrafter => crate::i18n::t!("presets-drafter"),
            Self::PackageMaintainer => crate::i18n::t!("presets-maintainer"),
            Self::Recruiter => crate::i18n::t!("presets-recruiter"),
            Self::Officer => crate::i18n::t!("presets-officer"),
            Self::GroupLead => crate::i18n::t!("presets-group-lead"),
            Self::FolderManager => crate::i18n::t!("presets-folder-manager"),
        }
    }
}

impl std::fmt::Display for Preset {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.label())
    }
}

impl ScopeKind {
    /// Every scope kind, in the order a picker lists them.
    pub const ALL: [Self; 5] = [
        Self::Clan,
        Self::Folders,
        Self::Maps,
        Self::Packages,
        Self::Groups,
    ];

    /// The scope kind's name.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Clan => crate::i18n::t!("presets-scope-clan"),
            Self::Groups => crate::i18n::t!("presets-scope-groups"),
            Self::Folders => crate::i18n::t!("presets-scope-folders"),
            Self::Maps => crate::i18n::t!("presets-scope-maps"),
            Self::Packages => crate::i18n::t!("presets-scope-packages"),
        }
    }
}

impl std::fmt::Display for ScopeKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.label())
    }
}

/// Whether a clan grant of `scope` may carry `action` (clans.md §1.1).
/// Secret actions belong to one Secret's own grants and apply to no clan
/// scope; `atlas.delete` applies to none.
#[must_use]
pub fn allowed_on(action: &str, scope: ScopeKind) -> bool {
    if action == action::DELETE_ATLAS || Kind::of_action(action) == Some(Kind::Secret) {
        return false;
    }
    match scope {
        ScopeKind::Clan => {
            action.starts_with("clan.")
                || action.starts_with("group.")
                || matches!(action, action::CREATE_ATLAS | action::CREATE_PACKAGE)
        }
        ScopeKind::Groups => matches!(
            action,
            action::RENAME_GROUP
                | action::DELETE_GROUP
                | action::ASSIGN_GROUP
                | action::INSPECT_GROUP
        ),
        ScopeKind::Folders => {
            (action.starts_with("atlas.") && action != action::CREATE_ATLAS)
                || action.starts_with("area.")
                || action.starts_with("secret.")
                || matches!(action, action::INSPECT_GRANTS | action::MANAGE_GRANTS)
        }
        ScopeKind::Maps => {
            (action.starts_with("area.")
                && !matches!(
                    action,
                    action::CREATE_AREA | action::CREATE_MEMBER_OWNED_AREA
                ))
                || action.starts_with("secret.")
                || matches!(action, action::INSPECT_GRANTS | action::MANAGE_GRANTS)
        }
        ScopeKind::Packages => {
            (action.starts_with("package.") && action != action::CREATE_PACKAGE)
                || matches!(action, action::INSPECT_GRANTS | action::MANAGE_GRANTS)
        }
    }
}

/// Whether a grant to one member (not a group) may carry `action` on any
/// scope: clan administration, group and package actions. Map, folder and
/// Secret access goes to groups, and to a member only on a grant naming one
/// map ([`allowed_for_member_on`]).
#[must_use]
pub fn allowed_for_member(action: &str) -> bool {
    !(action.starts_with("atlas.") || action.starts_with("area.") || action.starts_with("secret."))
}

/// Whether a grant to one member over `scope`, naming `targets` resources,
/// may carry `action` (clans.md §1): what [`allowed_for_member`] allows, and
/// map and Secret actions on a grant naming exactly one map, Clan-owned or
/// Member-owned.
#[must_use]
pub fn allowed_for_member_on(action: &str, scope: ScopeKind, targets: usize) -> bool {
    allowed_for_member(action)
        || (scope == ScopeKind::Maps
            && targets == 1
            && (action.starts_with("area.") || action.starts_with("secret.")))
}

/// Whether a grant naming a Member-owned map may carry `action`: the map
/// actions its owners hold, never the clan's own (clans.md §7.4).
#[must_use]
pub fn takes_on_member_map(action: &str) -> bool {
    matches!(
        action,
        action::READ_AREA
            | action::ADD_TO_AREA
            | action::EDIT_AREA
            | action::REMOVE_FROM_AREA
            | action::RENAME_AREA
            | action::REFILE_AREA
            | action::DELETE_AREA
            | action::COPY_AREA
            | action::CREATE_MEMBER_OWNED_SECRET
    )
}

/// Whether a grant that hands out access may name `action` in its
/// `may_grant`: map, folder, Secret and package actions, never clan
/// administration, group membership or further delegation (clans.md §1.2).
#[must_use]
pub fn delegable(action: &str) -> bool {
    Kind::of_action(action).is_some_and(|kind| kind != Kind::Secret)
        && !(action.starts_with("clan.")
            || action.starts_with("group.")
            || action.starts_with("grant."))
}

/// The preset whose actions are exactly `actions`, or `None` for Custom.
#[must_use]
pub fn matching_preset<'a, I>(actions: I) -> Option<Preset>
where
    I: IntoIterator<Item = &'a str>,
{
    let actions: BTreeSet<&str> = actions.into_iter().collect();
    Preset::ALL.into_iter().find(|preset| {
        preset.actions().len() == actions.len()
            && preset
                .actions()
                .iter()
                .all(|action| actions.contains(action))
    })
}

/// The name of the preset whose actions are exactly `actions`, or "Custom".
#[must_use]
pub fn preset_or_custom<'a, I>(actions: I) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    matching_preset(actions).map_or_else(custom_label, Preset::label)
}

/// "Custom": the name of an action set that is no preset.
#[must_use]
pub fn custom_label() -> String {
    crate::i18n::t!("presets-custom")
}

/// The preset of `kind` that the kind's core actions in `actions` are
/// exactly, ignoring other kinds' actions and the kind's separate ones.
/// `None` when that part is empty or no preset: the editor shows Custom.
#[must_use]
pub fn core_preset<'a, I>(kind: Kind, actions: I) -> Option<Preset>
where
    I: IntoIterator<Item = &'a str>,
{
    let core: BTreeSet<&str> = actions
        .into_iter()
        .filter(|action| kind.core().contains(action))
        .collect();
    if core.is_empty() {
        return None;
    }
    matching_preset(core)
}

/// A grant's actions as chips, kind by kind: each part of a kind's core that
/// is exactly a preset shows as the preset; any other core actions, the
/// separate actions, and actions this client does not know show one by one.
#[must_use]
pub fn chips<'a, I>(actions: I) -> Vec<Chip>
where
    I: IntoIterator<Item = &'a str>,
{
    let actions: BTreeSet<&str> = actions.into_iter().collect();
    let mut chips = Vec::new();
    for kind in Kind::ALL {
        for facet in kind.facets() {
            let part: BTreeSet<&str> = facet
                .iter()
                .copied()
                .filter(|action| actions.contains(action))
                .collect();
            if part.is_empty() {
                continue;
            }
            let preset = kind
                .presets()
                .iter()
                .copied()
                .filter(|preset| preset.actions().iter().all(|action| facet.contains(action)))
                .find(|preset| {
                    preset.actions().len() == part.len()
                        && preset.actions().iter().all(|action| part.contains(action))
                });
            match preset {
                Some(preset) => chips.push(Chip::Preset(preset)),
                None => chips.extend(
                    facet
                        .iter()
                        .copied()
                        .filter(|action| part.contains(action))
                        .map(Chip::Action),
                ),
            }
        }
        chips.extend(
            kind.separate()
                .iter()
                .copied()
                .filter(|action| actions.contains(action))
                .map(Chip::Action),
        );
    }
    chips.extend(
        actions
            .into_iter()
            .filter(|action| Kind::of_action(action).is_none())
            .map(|action| Chip::Action(leak_unknown(action))),
    );
    chips
}

/// An action this client does not know, kept as its wire name for a chip.
/// The set of such names a server sends is small and fixed per version.
fn leak_unknown(action: &str) -> &'static str {
    static UNKNOWN: std::sync::LazyLock<std::sync::Mutex<BTreeSet<&'static str>>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(BTreeSet::new()));
    let mut unknown = UNKNOWN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(known) = unknown.get(action) {
        return known;
    }
    let leaked: &'static str = Box::leak(action.to_string().into_boxed_str());
    unknown.insert(leaked);
    leaked
}

/// An action's name for a checkbox or chip. An action this client does not
/// know shows as its wire name.
#[must_use]
pub fn action_label(action: &str) -> String {
    use crate::i18n::t;
    match action {
        action::EDIT_PROFILE => t!("presets-action-clan-edit-profile"),
        action::READ_MEMBERS => t!("presets-action-clan-read-members"),
        action::INVITE => t!("presets-action-clan-invite"),
        action::REVOKE_INVITATION => t!("presets-action-clan-revoke-invitation"),
        action::REMOVE_MEMBER => t!("presets-action-clan-remove-member"),
        action::CREATE_GROUP => t!("presets-action-group-create"),
        action::RENAME_GROUP => t!("presets-action-group-rename"),
        action::DELETE_GROUP => t!("presets-action-group-delete"),
        action::ASSIGN_GROUP => t!("presets-action-group-assign"),
        action::INSPECT_GROUP => t!("presets-action-group-inspect"),
        action::INSPECT_GRANTS => t!("presets-action-access-inspect"),
        action::MANAGE_GRANTS => t!("presets-action-access-manage"),
        action::CREATE_ATLAS => t!("presets-action-folder-create"),
        action::READ_ATLAS => t!("presets-action-folder-read"),
        action::RENAME_ATLAS => t!("presets-action-folder-rename"),
        action::ACCEPT_FILING => t!("presets-action-folder-accept-filing"),
        action::ACCEPT_TRANSFER => t!("presets-action-folder-accept-transfer"),
        action::CREATE_AREA => t!("presets-action-map-create"),
        action::CREATE_MEMBER_OWNED_AREA => t!("presets-action-map-create-member-owned"),
        action::READ_AREA => t!("presets-action-map-read"),
        action::ADD_TO_AREA => t!("presets-action-map-add"),
        action::EDIT_AREA => t!("presets-action-map-edit"),
        action::REMOVE_FROM_AREA => t!("presets-action-map-remove"),
        action::RENAME_AREA => t!("presets-action-map-rename"),
        action::REFILE_AREA => t!("presets-action-map-refile"),
        action::DELETE_AREA => t!("presets-action-map-delete"),
        action::COPY_AREA => t!("presets-action-map-copy"),
        action::SHARE_AREA_EXTERNALLY => t!("presets-action-map-share-external"),
        action::CREATE_MEMBER_OWNED_SECRET => t!("presets-action-secrets-create-member-owned"),
        action::CREATE_CLAN_OWNED_SECRET => t!("presets-action-secrets-create-clan-owned"),
        action::READ_SECRETS => t!("presets-action-secrets-read"),
        action::ADD_TO_SECRETS => t!("presets-action-secrets-add"),
        action::EDIT_SECRETS => t!("presets-action-secrets-edit"),
        action::REMOVE_FROM_SECRETS => t!("presets-action-secrets-remove"),
        action::MANAGE_SECRET_ACCESS => t!("presets-action-secrets-manage-access"),
        action::COPY_SECRETS => t!("presets-action-secrets-copy"),
        action::CREATE_PACKAGE => t!("presets-action-package-create"),
        action::READ_PACKAGE => t!("presets-action-package-read"),
        action::EDIT_PACKAGE_DRAFT => t!("presets-action-package-edit-draft"),
        action::EDIT_PACKAGE_METADATA => t!("presets-action-package-edit-metadata"),
        action::PUBLISH_PACKAGE => t!("presets-action-package-publish"),
        action::RETIRE_PACKAGE_VERSION => t!("presets-action-package-retire"),
        action::MANAGE_PACKAGE_AVAILABILITY => t!("presets-action-package-availability"),
        action::DELETE_PACKAGE => t!("presets-action-package-delete"),
        secret_action::READ => t!("presets-action-secret-read"),
        secret_action::ADD => t!("presets-action-secret-add"),
        secret_action::EDIT => t!("presets-action-secret-edit"),
        secret_action::REMOVE => t!("presets-action-secret-remove"),
        secret_action::MANAGE_ACCESS => t!("presets-action-secret-manage-access"),
        secret_action::COPY => t!("presets-action-secret-copy"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(actions: &[&str]) -> BTreeSet<String> {
        actions.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn every_preset_matches_its_own_actions_and_nothing_else_does() {
        for preset in Preset::ALL {
            assert_eq!(
                matching_preset(preset.actions().iter().copied()),
                Some(preset),
                "{preset:?}"
            );
        }
        for (index, preset) in Preset::ALL.iter().enumerate() {
            for other in &Preset::ALL[index + 1..] {
                let mine: BTreeSet<_> = preset.actions().iter().collect();
                let theirs: BTreeSet<_> = other.actions().iter().collect();
                assert_ne!(mine, theirs, "{preset:?} and {other:?} share actions");
            }
        }
    }

    #[test]
    fn matching_is_exact_and_ignores_order() {
        assert_eq!(
            matching_preset(["area.edit", "area.read", "area.add"]),
            Some(Preset::MapContributor)
        );
        assert_eq!(matching_preset(["area.read", "area.add"]), None);
        assert_eq!(
            matching_preset(["area.read", "area.add", "area.edit", "area.copy"]),
            None
        );
        assert_eq!(matching_preset(std::iter::empty::<&str>()), None);
        assert_eq!(
            matching_preset(["read", "manage_access"]),
            Some(Preset::SecretAccessManager)
        );
        assert_eq!(matching_preset(["read", "copy"]), None);
    }

    #[test]
    fn presets_hold_the_server_action_names() {
        assert_eq!(Preset::MapReader.actions(), &["area.read"]);
        assert_eq!(
            Preset::MapEditor.action_set(),
            owned(&["area.read", "area.add", "area.edit", "area.remove_content"])
        );
        assert_eq!(
            Preset::Curator.action_set(),
            owned(&[
                "atlas.rename",
                "atlas.accept_filing",
                "atlas.accept_transfer",
                "area.create",
                "area.rename",
                "area.refile"
            ])
        );
        assert_eq!(
            Preset::SecretsEditor.action_set(),
            owned(&[
                "secret.read",
                "secret.add",
                "secret.edit",
                "secret.remove_content"
            ])
        );
        assert_eq!(
            Preset::PackageMaintainer.actions(),
            &[
                "package.read",
                "package.edit_draft",
                "package.edit_metadata",
                "package.publish",
                "package.retire"
            ]
        );
        assert_eq!(
            Preset::Officer.actions(),
            &[
                "clan.read_members",
                "clan.invite",
                "clan.revoke_invitation",
                "clan.remove_member"
            ]
        );
        assert_eq!(
            Preset::GroupLead.actions(),
            &["group.inspect_assignments", "group.assign"]
        );
        assert_eq!(
            Preset::FolderManager.actions(),
            &["grant.inspect", "grant.manage"]
        );
        assert_eq!(
            Preset::SecretEditor.actions(),
            &["read", "add", "edit", "remove"]
        );
    }

    #[test]
    fn each_kind_presets_grow_from_its_core() {
        for kind in Kind::ALL {
            for preset in kind.presets() {
                assert_eq!(preset.kind(), kind);
                for action in preset.actions() {
                    assert!(kind.core().contains(action), "{preset:?}: {action}");
                    assert!(!kind.separate().contains(action), "{preset:?}: {action}");
                }
            }
        }
    }

    #[test]
    fn separate_actions_are_in_no_preset() {
        for kind in Kind::ALL {
            for action in kind.separate() {
                assert!(
                    Preset::ALL
                        .iter()
                        .all(|preset| !preset.actions().contains(action)),
                    "{action} is in a preset"
                );
            }
        }
        for action in [
            "area.copy",
            "area.share_external",
            "area.delete",
            "secret.create_member_owned",
            "secret.create_clan_owned",
            "area.create_member_owned",
            "secret.manage_access",
            "secret.copy",
            "copy",
            "package.manage_availability",
            "package.delete",
            "package.create",
            "clan.edit_profile",
            "group.create",
            "group.delete",
        ] {
            let kind = Kind::of_action(action).unwrap();
            assert!(kind.separate().contains(&action), "{action}");
        }
    }

    #[test]
    fn every_action_belongs_to_one_kind() {
        let mut seen = BTreeSet::new();
        for kind in Kind::ALL {
            for action in kind.actions() {
                assert!(seen.insert(action), "{action} is in two kinds");
            }
        }
        assert_eq!(Kind::of_action("atlas.delete"), None);
        assert_eq!(Kind::of_action("clan.unheard_of"), None);
    }

    #[test]
    fn scopes_take_the_actions_the_server_allows() {
        assert!(!allowed_on("area.read", ScopeKind::Clan));
        assert!(allowed_on("package.create", ScopeKind::Clan));
        assert!(!allowed_on("atlas.delete", ScopeKind::Clan));
        assert!(!allowed_on("atlas.delete", ScopeKind::Folders));
        assert!(allowed_on("group.assign", ScopeKind::Groups));
        assert!(!allowed_on("group.create", ScopeKind::Groups));
        assert!(!allowed_on("clan.invite", ScopeKind::Groups));
        assert!(allowed_on("area.create", ScopeKind::Folders));
        // Creating folders is the clan's alone.
        assert!(allowed_on("atlas.create", ScopeKind::Clan));
        assert!(!allowed_on("atlas.create", ScopeKind::Folders));
        assert!(!allowed_on("area.create", ScopeKind::Maps));
        assert!(!allowed_on("area.create_member_owned", ScopeKind::Maps));
        assert!(allowed_on("secret.read", ScopeKind::Maps));
        assert!(allowed_on("grant.manage", ScopeKind::Maps));
        assert!(!allowed_on("group.assign", ScopeKind::Maps));
        assert!(allowed_on("package.edit_draft", ScopeKind::Packages));
        assert!(!allowed_on("package.create", ScopeKind::Packages));
        assert!(!allowed_on("read", ScopeKind::Clan));
        assert!(Preset::GroupLead.applies_to(ScopeKind::Groups));
        assert!(!Preset::Recruiter.applies_to(ScopeKind::Groups));
        assert!(!Preset::Curator.applies_to(ScopeKind::Maps));
        assert!(Preset::FolderManager.applies_to(ScopeKind::Folders));
        assert_eq!(
            Kind::Package.presets_on(ScopeKind::Packages),
            Kind::Package.presets().to_vec()
        );
        assert!(!Kind::Package.applies_to(ScopeKind::Folders));
        assert!(!Kind::Secret.applies_to(ScopeKind::Clan));
    }

    #[test]
    fn members_take_no_map_access_and_delegation_no_administration() {
        assert!(allowed_for_member("clan.invite"));
        assert!(allowed_for_member("package.publish"));
        assert!(!allowed_for_member("area.read"));
        assert!(!allowed_for_member("atlas.rename"));
        assert!(!allowed_for_member("secret.read"));
        assert!(allowed_for_member_on("area.edit", ScopeKind::Maps, 1));
        assert!(allowed_for_member_on("secret.read", ScopeKind::Maps, 1));
        assert!(!allowed_for_member_on("area.edit", ScopeKind::Maps, 2));
        assert!(!allowed_for_member_on("area.read", ScopeKind::Folders, 1));
        assert!(!allowed_for_member_on("atlas.rename", ScopeKind::Maps, 1));
        assert!(allowed_for_member_on("clan.invite", ScopeKind::Clan, 0));
        assert!(delegable("area.read"));
        assert!(delegable("secret.read"));
        assert!(delegable("package.publish"));
        assert!(!delegable("clan.invite"));
        assert!(!delegable("group.assign"));
        assert!(!delegable("grant.manage"));
        assert!(!delegable("read"));
        assert!(
            Kind::Map
                .presets()
                .iter()
                .all(|preset| { preset.actions().iter().all(|action| delegable(action)) })
        );
    }

    #[test]
    fn core_presets_ignore_other_kinds_and_separate_actions() {
        let grant = [
            "area.read",
            "secret.create_member_owned",
            "secret.read",
            "secret.add",
            "secret.edit",
        ];
        assert_eq!(core_preset(Kind::Map, grant), Some(Preset::MapReader));
        assert_eq!(
            core_preset(Kind::ClanSecrets, grant),
            Some(Preset::SecretsContributor)
        );
        assert_eq!(core_preset(Kind::Package, grant), None);
        assert_eq!(core_preset(Kind::Map, ["area.read", "area.edit"]), None);
    }

    #[test]
    fn chips_name_exact_presets_and_list_the_rest() {
        assert_eq!(
            chips(["area.read", "secret.create_member_owned"]),
            vec![
                Chip::Preset(Preset::MapReader),
                Chip::Action("secret.create_member_owned")
            ]
        );
        assert_eq!(
            chips(["area.read", "area.edit"]),
            vec![Chip::Action("area.read"), Chip::Action("area.edit")]
        );
        assert_eq!(
            chips([
                "clan.read_members",
                "clan.invite",
                "group.assign",
                "group.inspect_assignments"
            ]),
            vec![
                Chip::Preset(Preset::Recruiter),
                Chip::Preset(Preset::GroupLead)
            ]
        );
        assert_eq!(
            chips(["package.read", "package.edit_draft", "package.delete"]),
            vec![
                Chip::Preset(Preset::PackageDrafter),
                Chip::Action("package.delete")
            ]
        );
        assert_eq!(
            chips(["read", "add", "edit", "copy"]),
            vec![
                Chip::Preset(Preset::SecretContributor),
                Chip::Action("copy")
            ]
        );
        assert_eq!(
            chips(["area.read", "clan.future_action"]),
            vec![
                Chip::Preset(Preset::MapReader),
                Chip::Action("clan.future_action")
            ]
        );
    }
}
