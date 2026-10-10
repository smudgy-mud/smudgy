//! Publishing a package into a clan. The first publish of a local package
//! chooses its owner: the account, or one of its clans that lets it create
//! packages (`package.create`). A package's owner never changes. On a clan's
//! package, what the pane offers follows the clan's actions, which are
//! clan-wide: `package.publish` for new versions, `package.retire` for
//! yanking and retiring them, `package.manage_availability` for making it
//! public or private. A clan shares its packages by membership, so friend
//! sharing doesn't apply (packages.md §1).

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use iced::alignment::Vertical;
use iced::widget::{column, pick_list, row, text};
use smudgy_cloud::clans::{ClanSummary, action};
use smudgy_cloud::cloud_api::FriendView;
use smudgy_cloud::package_api::{PackageGrantView, VersionListItem};
use smudgy_cloud::{CloudError, Uuid};
use smudgy_core::models::local_packages::{PublishOwner, PublishRefusal};

use crate::components::cloud_errors::display_error;

use super::packages::PublicationStatus;
use super::{AutomationsWindow, Elem, Message, common};

/// What an owned package pane shows from the cloud.
#[derive(Debug, Clone)]
pub struct OwnedShare {
    pub id: Uuid,
    pub is_public: bool,
    /// The clan that owns it; `None` for the account's own package.
    pub clan: Option<OwnedClan>,
    pub friends: Vec<FriendView>,
    pub grants: Vec<PackageGrantView>,
    pub versions: Vec<VersionListItem>,
}

/// The clan owning a package, with the account's clan-wide actions there
/// (none once the account has left it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedClan {
    pub id: Uuid,
    pub actions: BTreeSet<String>,
}

impl OwnedClan {
    /// The clan that owns a package, `owner_id`, with the account's actions
    /// as its clan list says.
    #[must_use]
    pub fn of(owner_id: Uuid, clans: &[ClanSummary]) -> Self {
        Self {
            id: owner_id,
            actions: clans
                .iter()
                .find(|clan| clan.id == owner_id)
                .map(|clan| clan.actions.clone())
                .unwrap_or_default(),
        }
    }

    #[must_use]
    pub fn can(&self, action: &str) -> bool {
        self.actions.contains(action)
    }
}

/// One choice of the first publish's owner picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerChoice {
    pub owner: PublishOwner,
    pub label: String,
}

impl fmt::Display for OwnerChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

/// The clans that let the account create packages, by name.
#[must_use]
pub fn creating_clans(clans: &[ClanSummary]) -> Vec<(Uuid, String)> {
    let mut creating: Vec<(Uuid, String)> = clans
        .iter()
        .filter(|clan| clan.can(action::CREATE_PACKAGE))
        .map(|clan| (clan.id, clan.name.clone()))
        .collect();
    creating.sort_by_key(|(_, name)| name.to_lowercase());
    creating
}

/// The first publish's owner choices: Me, then each clan that lets the
/// account create packages.
#[must_use]
pub fn owner_choices(clans: &[(Uuid, String)]) -> Vec<OwnerChoice> {
    std::iter::once(OwnerChoice {
        owner: PublishOwner::Me,
        label: crate::i18n::t!("package-owner-me"),
    })
    .chain(clans.iter().map(|(id, name)| OwnerChoice {
        owner: PublishOwner::Clan(*id),
        label: name.clone(),
    }))
    .collect()
}

/// A clan's name when the account is in it, else the clan label.
fn clan_label(names: &HashMap<Uuid, String>, clan: Uuid) -> String {
    names
        .get(&clan)
        .cloned()
        .unwrap_or_else(|| crate::i18n::t!("package-owner-clan"))
}

/// A publish's failure in words: the refusals a clan's package meets read
/// plainly, naming the clan; anything else keeps its own message.
#[must_use]
pub fn publish_error(error: &anyhow::Error, names: &HashMap<Uuid, String>) -> String {
    match error.downcast_ref::<PublishRefusal>() {
        Some(PublishRefusal::ClanCreate(clan)) => {
            crate::i18n::t!("package-clan-cannot-create", "clan" => clan_label(names, *clan))
        }
        Some(PublishRefusal::ClanPublish(clan)) => {
            crate::i18n::t!("package-clan-cannot-publish", "clan" => clan_label(names, *clan))
        }
        Some(PublishRefusal::ClaimedForAnotherOwner(PublishOwner::Me)) => {
            crate::i18n::t!("package-claimed-for-me")
        }
        Some(PublishRefusal::ClaimedForAnotherOwner(PublishOwner::Clan(clan))) => {
            crate::i18n::t!("package-claimed-for-clan", "clan" => clan_label(names, *clan))
        }
        None => format!("{error:#}"),
    }
}

impl AutomationsWindow {
    /// The clan names the account knows, for labels.
    pub(super) fn clan_names(&self) -> HashMap<Uuid, String> {
        self.package_clan_names.clone().unwrap_or_default()
    }

    /// Learns the account's clans: their names for labels, and those that
    /// let it create packages for the first publish's owner choices. A
    /// choice no longer offered falls back to Me; an interrupted first
    /// publish keeps the owner it chose.
    pub(super) fn learn_publish_clans(&mut self, name: &str, clans: &[ClanSummary]) {
        let names = self.package_clan_names.get_or_insert_with(HashMap::new);
        for clan in clans {
            names.insert(clan.id, clan.name.clone());
        }
        self.publish_clans = creating_clans(clans);
        let claimed =
            smudgy_core::models::local_packages::publication_claim_owner(&self.server_name, name)
                .ok()
                .flatten();
        self.publish_owner = match (claimed, self.publish_owner) {
            (Some(owner), _) => owner,
            (None, PublishOwner::Clan(clan))
                if self.publish_clans.iter().any(|(id, _)| *id == clan) =>
            {
                PublishOwner::Clan(clan)
            }
            (None, _) => PublishOwner::Me,
        };
    }

    /// The owner the next publish uses: the picked one on a first publish;
    /// a bound package keeps its own.
    pub(super) fn publish_target(&self) -> PublishOwner {
        if matches!(self.publication_status, PublicationStatus::Bound(_)) {
            PublishOwner::Me
        } else {
            self.publish_owner
        }
    }

    /// Whether the account may publish a new version of the open package:
    /// always for its own, with `package.publish` for a clan's.
    pub(super) fn may_publish_here(&self) -> bool {
        self.share_clan
            .as_ref()
            .is_none_or(|clan| clan.can(action::PUBLISH_PACKAGE))
    }

    /// Whether the account may yank, un-yank and retire the open package's
    /// versions.
    pub(super) fn may_retire_here(&self) -> bool {
        self.share_clan
            .as_ref()
            .is_none_or(|clan| clan.can(action::RETIRE_PACKAGE_VERSION))
    }

    /// Whether the account may make the open package public or private.
    pub(super) fn may_change_availability_here(&self) -> bool {
        self.share_clan
            .as_ref()
            .is_none_or(|clan| clan.can(action::MANAGE_PACKAGE_AVAILABILITY))
    }

    /// A sharing change's failure in words: on a clan's package the
    /// uniform 404 means the clan doesn't let the account do it.
    pub(super) fn share_refusal(&self, error: &CloudError) -> String {
        match (&self.share_clan, error) {
            (Some(clan), CloudError::NotFoundOrNoAccess) => crate::i18n::t!(
                "package-clan-refused",
                "clan" => self.package_clan_label(clan.id)
            ),
            _ => display_error(error),
        }
    }

    /// The first publish's "Publish as" picker, when a clan lets the
    /// account create packages, with what choosing a clan means.
    pub(super) fn publish_owner_picker(&self) -> Option<Elem<'_>> {
        if !matches!(
            self.publication_status,
            PublicationStatus::Unpublished | PublicationStatus::Checking
        ) || self.publish_clans.is_empty()
        {
            return None;
        }
        let choices = owner_choices(&self.publish_clans);
        let picked = choices
            .iter()
            .find(|choice| choice.owner == self.publish_owner)
            .cloned();
        let mut picker = column![
            row![
                text(crate::i18n::t!("package-publish-as"))
                    .size(12.0)
                    .style(common::muted),
                pick_list(choices, picked, Message::PublishOwnerPicked)
                    .text_size(12.0)
                    .padding([3, 8]),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center)
        ]
        .spacing(4.0);
        if let PublishOwner::Clan(clan) = self.publish_owner {
            picker = picker.push(
                text(crate::i18n::t!(
                    "package-publish-as-clan-help",
                    "clan" => self.package_clan_label(clan)
                ))
                .size(11.0)
                .style(common::muted),
            );
        }
        Some(picker.into())
    }

    /// A clan's package names its owner, and says when the account may not
    /// publish new versions of it.
    pub(super) fn clan_owner_lines(&self) -> Option<Elem<'_>> {
        let clan = self.share_clan.as_ref()?;
        let label = self.package_clan_label(clan.id);
        let mut lines = column![
            text(crate::i18n::t!("package-owner", "owner" => label.clone()))
                .size(12.0)
                .style(common::muted)
        ]
        .spacing(4.0);
        if !self.may_publish_here() {
            lines = lines.push(
                text(crate::i18n::t!("package-clan-cannot-publish", "clan" => label))
                    .size(12.0)
                    .style(common::muted),
            );
        }
        Some(lines.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clan(id: u128, name: &str, actions: &[&str]) -> ClanSummary {
        ClanSummary {
            id: Uuid::from_u128(id),
            name: name.to_string(),
            description: None,
            created_at: chrono::DateTime::UNIX_EPOCH,
            member_count: 3,
            is_owner: false,
            group_ids: Vec::new(),
            actions: actions.iter().map(ToString::to_string).collect(),
        }
    }

    #[test]
    fn only_clans_that_let_the_account_create_are_owner_choices() {
        let clans = [
            clan(1, "Rangers", &[action::PUBLISH_PACKAGE]),
            clan(2, "lantern Company", &[action::CREATE_PACKAGE]),
            clan(
                3,
                "Archive",
                &[action::CREATE_PACKAGE, action::PUBLISH_PACKAGE],
            ),
        ];
        let creating = creating_clans(&clans);
        assert_eq!(
            creating,
            [
                (Uuid::from_u128(3), "Archive".to_string()),
                (Uuid::from_u128(2), "lantern Company".to_string())
            ]
        );
        let choices = owner_choices(&creating);
        assert_eq!(
            choices
                .iter()
                .map(|choice| choice.owner)
                .collect::<Vec<_>>(),
            [
                PublishOwner::Me,
                PublishOwner::Clan(Uuid::from_u128(3)),
                PublishOwner::Clan(Uuid::from_u128(2))
            ]
        );
        assert!(owner_choices(&[]).len() == 1, "Me alone");
    }

    #[test]
    fn a_clan_package_follows_the_clans_actions() {
        let clans = [clan(1, "Rangers", &[action::PUBLISH_PACKAGE])];
        let owned = OwnedClan::of(Uuid::from_u128(1), &clans);
        assert!(owned.can(action::PUBLISH_PACKAGE));
        assert!(!owned.can(action::RETIRE_PACKAGE_VERSION));
        // A clan the account has left gives it nothing.
        assert!(OwnedClan::of(Uuid::from_u128(9), &clans).actions.is_empty());
    }

    #[test]
    fn clan_refusals_name_the_clan_in_every_language() {
        let clan = Uuid::from_u128(1);
        let names: HashMap<Uuid, String> = [(clan, "Rangers".to_string())].into();
        for refusal in [
            PublishRefusal::ClanCreate(clan),
            PublishRefusal::ClanPublish(clan),
            PublishRefusal::ClaimedForAnotherOwner(PublishOwner::Clan(clan)),
        ] {
            let message = publish_error(&anyhow::Error::new(refusal), &names);
            assert!(message.contains("Rangers"), "{message}");
        }
        // A clan the account isn't in is "Clan".
        let unnamed = publish_error(
            &anyhow::Error::new(PublishRefusal::ClanPublish(Uuid::from_u128(2))),
            &names,
        );
        assert!(unnamed.contains(&crate::i18n::t!("package-owner-clan")));
        // Anything else keeps its own message.
        assert_eq!(
            publish_error(&anyhow::anyhow!("disk full"), &names),
            "disk full"
        );
        for catalog in smudgy_i18n::available_catalogs() {
            let translator = smudgy_i18n::Translator::for_tag(catalog.tag).unwrap();
            let checks = [
                smudgy_i18n::t!(translator, "package-clan-cannot-create", "clan" => "Rangers"),
                smudgy_i18n::t!(translator, "package-clan-cannot-publish", "clan" => "Rangers"),
                smudgy_i18n::t!(translator, "package-clan-refused", "clan" => "Rangers"),
                smudgy_i18n::t!(translator, "package-claimed-for-clan", "clan" => "Rangers"),
                smudgy_i18n::t!(translator, "package-publish-as-clan-help", "clan" => "Rangers"),
                smudgy_i18n::t!(translator, "package-owner", "owner" => "Rangers"),
            ];
            for text in checks {
                assert!(
                    text.contains("Rangers") && !text.contains('⟦'),
                    "{}: {text}",
                    catalog.tag
                );
            }
            for key in [
                "package-publish-as",
                "package-owner-me",
                "package-claimed-for-me",
                "package-clan-private-help",
            ] {
                let text = translator.translate(key);
                assert!(
                    !text.is_empty() && !text.contains('⟦'),
                    "{}: {key}",
                    catalog.tag
                );
                if catalog.tag != "en-US" {
                    assert_ne!(
                        text,
                        smudgy_i18n::Translator::default().translate(key),
                        "{} must translate {key}",
                        catalog.tag
                    );
                }
            }
        }
    }
}
