//! Sharing a Clan Secret from the Share dialog. Its recipients are its
//! clan's groups (Everyone first, the owners left out), which any member
//! sees, and its members, for a viewer who may read the member directory;
//! its grants follow the clan's rules: ownership authority writes any grant,
//! a holder of `manage_access` only within its own actions and never
//! `manage_access`.

use iced::Task;
use smudgy_cloud::Uuid;
use smudgy_cloud::clan_secrets::{ClanSecretGrant, SecretAccess, SecretRecipient};
use smudgy_cloud::clans::{ClanGroup, ClanMember};
use smudgy_cloud::cloud_api::SecretGrant;
use smudgy_cloud::{CloudApiClient, CloudError, SourceId};

use super::Message;
use super::modals::ShareMessage;

/// What the dialog shows for a picked Clan Secret beyond its grants.
#[derive(Debug, Clone, Default)]
pub struct ClanShareData {
    pub groups: Vec<ClanGroup>,
    /// `None` when the viewer may not read the member directory: groups are
    /// then the only recipients offered.
    pub members: Option<Vec<ClanMember>>,
    /// Who reads it now and why, as the viewer may see it.
    pub access: Option<SecretAccess>,
}

impl ClanShareData {
    /// The groups offered as recipients: Everyone first, then the clan's
    /// own groups.
    pub fn groups(&self) -> Vec<(Uuid, String)> {
        let mut groups: Vec<(Uuid, String)> = self
            .groups
            .iter()
            .filter(|group| group.builtin.as_deref() == Some("members"))
            .map(|group| (group.id, crate::i18n::t!("clan-maps-everyone")))
            .collect();
        groups.extend(
            self.groups
                .iter()
                .filter(|group| !group.is_builtin())
                .map(|group| (group.id, group.name.clone())),
        );
        groups
    }

    /// The members offered as recipients: everyone but the viewer.
    pub fn members(&self, viewer: Option<Uuid>) -> Vec<(Uuid, String)> {
        self.members
            .iter()
            .flatten()
            .filter(|member| Some(member.user_id) != viewer)
            .map(|member| {
                (
                    member.user_id,
                    member
                        .nickname
                        .clone()
                        .unwrap_or_else(|| crate::i18n::t!("social-no-nickname")),
                )
            })
            .collect()
    }

    /// A checked ID as a recipient, with its name.
    pub fn recipient(&self, id: Uuid, viewer: Option<Uuid>) -> Option<(String, SecretRecipient)> {
        if let Some((_, name)) = self.groups().into_iter().find(|(group, _)| *group == id) {
            return Some((name, SecretRecipient::Group { group_id: id }));
        }
        self.members(viewer)
            .into_iter()
            .find(|(user, _)| *user == id)
            .map(|(_, name)| (name, SecretRecipient::User { user_id: id }))
    }
}

/// A Clan Secret grant in the dialog's grant shape. Its grantee is the
/// member or group it names; ownership authority stands where an owner
/// Secret's map owner does, so every grant reads as issued by the owner and
/// authority may change any of them.
pub fn as_secret_grant(grant: ClanSecretGrant, data: &ClanShareData) -> SecretGrant {
    let (grantee_id, grantee_nickname) = match grant.recipient {
        SecretRecipient::User { user_id } => (user_id, grant.nickname),
        SecretRecipient::Group { group_id } => (
            group_id,
            data.groups()
                .into_iter()
                .find(|(id, _)| *id == group_id)
                .map(|(_, name)| name),
        ),
    };
    SecretGrant {
        id: grant.id,
        secret_id: SourceId::Secret(grant.secret_id),
        area_id: grant.area_id,
        owner_id: grant.grantor_id,
        grantor_id: grant.grantor_id,
        grantee_id,
        grantee_nickname,
        grantor_nickname: grant.grantor_nickname,
        actions: grant.actions,
        created_at: grant.created_at,
        updated_at: grant.updated_at,
    }
}

/// Loads a Clan Secret's grants, its clan's groups and the members the
/// viewer may see, and who reads it.
pub fn fetch(client: CloudApiClient, clan_id: Uuid, secret: SourceId) -> Task<Message> {
    Task::perform(
        async move {
            let grants = client.clan_secret_grants(&secret).await?;
            let recipients = client.clan_recipients(clan_id).await?;
            let mut data = ClanShareData {
                groups: recipients.groups,
                members: recipients.members,
                access: None,
            };
            data.access = match client.secret_access(&secret).await {
                Ok(access) => Some(access),
                Err(CloudError::NotFoundOrNoAccess) => None,
                Err(error) => return Err(error),
            };
            let grants = grants
                .into_iter()
                .map(|grant| as_secret_grant(grant, &data))
                .collect();
            Ok(Box::new((grants, data)))
        },
        move |result| Message::Share(ShareMessage::ClanLoaded { secret, result }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(n: u128, name: &str, builtin: Option<&str>) -> ClanGroup {
        ClanGroup {
            id: Uuid::from_u128(n),
            name: name.to_string(),
            color: None,
            builtin: builtin.map(str::to_string),
            is_member: true,
            created_by_me: false,
            actions: std::collections::BTreeSet::new(),
        }
    }

    /// Without the member directory, the groups are still recipients.
    #[test]
    fn without_the_directory_the_groups_are_the_recipients() {
        let data = ClanShareData {
            groups: vec![
                group(1, "Owners", Some("owners")),
                group(2, "All clan members", Some("members")),
                group(3, "Scouts", None),
            ],
            members: None,
            access: None,
        };
        let viewer = Some(Uuid::from_u128(9));
        assert!(data.members(viewer).is_empty());
        assert_eq!(
            data.groups()
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            [Uuid::from_u128(2), Uuid::from_u128(3)]
        );
        assert_eq!(
            data.recipient(Uuid::from_u128(3), viewer),
            Some((
                "Scouts".to_string(),
                SecretRecipient::Group {
                    group_id: Uuid::from_u128(3)
                }
            ))
        );
    }
}
