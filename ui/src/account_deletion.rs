//! Deleting the signed-in account (Settings › Account): the confirmation
//! rule, what an answer to `DELETE /me` means for this computer, which clans
//! a `last_owner` refusal names, and the local cleanup once the account is
//! gone.

use std::path::{Path, PathBuf};

use smudgy_cloud::clans::{ClanMember, ClanSummary};
use smudgy_cloud::{CloudApiClient, CloudError, Uuid};

/// Whether `typed` confirms the deletion: the account's nickname (its email
/// while it has none), exactly, ignoring surrounding whitespace.
#[must_use]
pub fn confirms(typed: &str, expected: &str) -> bool {
    let expected = expected.trim();
    !expected.is_empty() && typed.trim() == expected
}

/// What an answer to `DELETE /me` means here.
#[derive(Debug, Clone)]
pub enum Answer {
    /// The account is gone.
    Deleted,
    /// The credential is no longer accepted: the account is being deleted
    /// (its credentials then work only for `DELETE /me`, and a finished
    /// deletion leaves them unknown) or the session had already ended.
    /// Either way this computer is signed out of it.
    NoLongerAccepted,
    /// The account is the last owner of an active clan; nothing changed.
    LastOwner,
    /// Anything else: the deletion may not have started, or may have stopped
    /// partway. Asking `/me` tells which ([`after_failure`]).
    Failed(CloudError),
}

#[must_use]
pub fn read_answer(result: Result<(), CloudError>) -> Answer {
    match result {
        Ok(()) => Answer::Deleted,
        Err(CloudError::LastOwner) => Answer::LastOwner,
        Err(error) if error.is_auth_error() => Answer::NoLongerAccepted,
        Err(error) => Answer::Failed(error),
    }
}

/// Where a failed deletion left the account, from a `/me` probe after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterFailure {
    /// `/me` answers 401: the account is marked as being deleted, and the
    /// server finishes the deletion on its own.
    Deleting,
    /// `/me` answers: the deletion never started.
    Intact,
    /// `/me` failed too: unknown; repeating the deletion finishes it.
    Unknown,
}

#[must_use]
pub fn after_failure<T>(probe: &Result<T, CloudError>) -> AfterFailure {
    match probe {
        Ok(_) => AfterFailure::Intact,
        Err(error) if error.is_auth_error() => AfterFailure::Deleting,
        Err(_) => AfterFailure::Unknown,
    }
}

/// The caller's clans, each with its members when the caller owns it and
/// they could be read.
pub type OwnedClans = Vec<(ClanSummary, Option<Vec<ClanMember>>)>;

/// The clans a `last_owner` refusal is about: those the caller owns alone.
/// A clan whose members could not be read is named when the caller owns it,
/// so the message never leaves out a clan that holds the deletion back.
#[must_use]
pub fn last_owner_clans(clans: &[(ClanSummary, Option<Vec<ClanMember>>)], me: Uuid) -> Vec<String> {
    clans
        .iter()
        .filter(|(clan, members)| {
            clan.is_owner
                && members.as_deref().is_none_or(|members| {
                    crate::components::clan_panel::is_last_owner(clan, Some(members), me)
                })
        })
        .map(|(clan, _)| clan.name.clone())
        .collect()
}

/// Reads the caller's clans and, for those it owns, their members, for
/// [`last_owner_clans`].
pub async fn owned_clans(client: CloudApiClient) -> Result<OwnedClans, CloudError> {
    let overview = client.clans().await?;
    let mut clans = Vec::with_capacity(overview.clans.len());
    for clan in overview.clans {
        let members = if clan.is_owner {
            client.clan_members(clan.id).await.ok()
        } else {
            None
        };
        clans.push((clan, members));
    }
    Ok(clans)
}

/// Every mutation-journal root under the smudgy home: one per server entry
/// (`<home>/<server>/local/pending-cloud-mutations`).
#[must_use]
pub fn journal_roots(home: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(home) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path().join("local").join("pending-cloud-mutations"))
        .filter(|root| root.is_dir())
        .collect()
}

/// Removes what this computer keeps for a deleted account's cloud maps: its
/// map cache and its queued writes in every server entry.
pub fn forget_account_on_disk(home: &Path, user_id: Uuid) {
    smudgy_cloud::backends::cached::forget_viewer_on_disk(
        &home.join("maps"),
        &journal_roots(home),
        user_id,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn clan(name: &str, is_owner: bool) -> ClanSummary {
        ClanSummary {
            id: Uuid::new_v4(),
            name: name.to_string(),
            description: None,
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            member_count: 2,
            is_owner,
            group_ids: Vec::new(),
            actions: BTreeSet::new(),
        }
    }

    fn member(user: Uuid, is_owner: bool) -> ClanMember {
        serde_json::from_value(serde_json::json!({
            "user_id": user,
            "nickname": null,
            "is_owner": is_owner,
            "joined_at": "2026-01-01T00:00:00Z",
            "group_ids": [],
        }))
        .expect("a member")
    }

    #[test]
    fn only_the_exact_nickname_confirms() {
        assert!(confirms("mira", "mira"));
        assert!(confirms("  mira ", "mira"));
        assert!(!confirms("Mira", "mira"));
        assert!(!confirms("mir", "mira"));
        assert!(!confirms("", ""));
        assert!(!confirms("   ", " "));
    }

    #[test]
    fn answers_map_onto_what_this_computer_does() {
        assert!(matches!(read_answer(Ok(())), Answer::Deleted));
        assert!(matches!(
            read_answer(Err(CloudError::LastOwner)),
            Answer::LastOwner
        ));
        assert!(matches!(
            read_answer(Err(CloudError::Unauthorized("gone".into()))),
            Answer::NoLongerAccepted
        ));
        assert!(matches!(
            read_answer(Err(CloudError::NetworkError("offline".into()))),
            Answer::Failed(_)
        ));
    }

    #[test]
    fn a_probe_after_a_failure_says_where_the_account_is() {
        assert_eq!(after_failure(&Ok(())), AfterFailure::Intact);
        assert_eq!(
            after_failure::<()>(&Err(CloudError::Unauthorized("deleting".into()))),
            AfterFailure::Deleting
        );
        assert_eq!(
            after_failure::<()>(&Err(CloudError::NetworkError("offline".into()))),
            AfterFailure::Unknown
        );
    }

    #[test]
    fn a_refusal_names_the_clans_owned_alone() {
        let me = id(1);
        let other = id(2);
        let clans = vec![
            (
                clan("Alone", true),
                Some(vec![member(me, true), member(other, false)]),
            ),
            (
                clan("Shared", true),
                Some(vec![member(me, true), member(other, true)]),
            ),
            (clan("Member", false), None),
            (clan("Unread", true), None),
        ];
        assert_eq!(last_owner_clans(&clans, me), vec!["Alone", "Unread"]);
    }

    #[test]
    fn forgetting_an_account_reaches_every_server_entry() {
        let home = std::env::temp_dir().join(format!("smudgy-forget-account-{}", Uuid::new_v4()));
        let gone = Uuid::new_v4();
        let journal = |server: &str| {
            home.join(server)
                .join("local")
                .join("pending-cloud-mutations")
                .join("servers")
                .join("ns")
                .join("viewers")
                .join(gone.to_string())
        };
        let cache = home
            .join("maps")
            .join(smudgy_cloud::backends::cached::CACHE_FORMAT_NAMESPACE)
            .join(gone.to_string());
        for directory in [journal("Arctic"), journal("NukeFire"), cache.clone()] {
            std::fs::create_dir_all(&directory).unwrap();
        }
        std::fs::create_dir_all(home.join("Arctic").join("local").join("maps")).unwrap();

        forget_account_on_disk(&home, gone);

        assert!(!journal("Arctic").exists());
        assert!(!journal("NukeFire").exists());
        assert!(!cache.exists());
        assert!(home.join("Arctic").join("local").join("maps").exists());
        std::fs::remove_dir_all(home).ok();
    }
}
