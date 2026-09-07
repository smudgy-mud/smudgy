//! Comparison of the open authored package with its greatest published version.

use std::sync::Arc;

use iced::Task;
use smudgy_cloud::VersionListItem;
use smudgy_core::models::local_packages::{self, LocalPackage};

use super::packages::{AccountReadFence, ShareSeq};
use super::{AutomationsWindow, Event, LocalPackageTab, Message, Selection};
use crate::update::Update;

#[derive(Debug, Clone, Default)]
pub(super) enum PublishedContent {
    #[default]
    Unknown,
    Checking(Arc<LocalPackage>),
    Compared(Arc<LocalPackage>, bool),
}

/// Include every reserved number when finding the greatest semver, but never call a yanked or
/// deleted version up to date. Upload dates and lexicographic ordering are not version ordering.
pub(super) fn is_latest_live(version: &str, versions: &[VersionListItem]) -> bool {
    let Ok(version) = semver::Version::parse(version) else {
        return false;
    };
    let Some(latest) = versions
        .iter()
        .filter_map(|item| {
            semver::Version::parse(&item.version)
                .ok()
                .map(|parsed| (parsed, item))
        })
        .max_by(|(a, _), (b, _)| a.cmp_precedence(b))
    else {
        return false;
    };
    latest.0 == version && !latest.1.yanked && !latest.1.deleted
}

pub(super) fn next_patch(version: &str, versions: &[VersionListItem]) -> Option<String> {
    let mut version = semver::Version::parse(version).ok()?;
    version.pre = semver::Prerelease::EMPTY;
    version.build = semver::BuildMetadata::EMPTY;
    loop {
        version.patch = version.patch.checked_add(1)?;
        let candidate = version.to_string();
        if !versions.iter().any(|item| item.version == candidate) {
            return Some(candidate);
        }
    }
}

impl AutomationsWindow {
    pub(super) fn published_content(&self) -> &PublishedContent {
        let snapshot = match &self.share_content {
            PublishedContent::Unknown => return &PublishedContent::Unknown,
            PublishedContent::Checking(snapshot) | PublishedContent::Compared(snapshot, _) => {
                snapshot
            }
        };
        if self.local_package.as_deref() == Some(snapshot.as_ref())
            && is_latest_live(&snapshot.manifest.version, &self.share_versions)
        {
            &self.share_content
        } else {
            &PublishedContent::Unknown
        }
    }

    pub(super) fn compare_owned_published_content(&mut self) -> Task<Message> {
        self.share_content = PublishedContent::Unknown;
        if self.local_package_tab != LocalPackageTab::Sharing {
            return Task::none();
        }
        let Some(package) = self.local_package.as_deref() else {
            return Task::none();
        };
        if !is_latest_live(&package.manifest.version, &self.share_versions) {
            return Task::none();
        }
        let Some(package_id) = self.share_package_id else {
            return Task::none();
        };
        let Some(owner) = self
            .cloud
            .snapshot
            .get()
            .profile
            .as_ref()
            .and_then(|profile| profile.nickname.clone())
        else {
            return Task::none();
        };
        let snapshot = Arc::new(package.clone());
        let seq = self.share_seq;
        let server = self.server_name.clone();
        let (fence, client) = self.frozen_package_client();
        self.share_content = PublishedContent::Checking(snapshot.clone());
        Task::perform(
            async move {
                let result = async {
                    let remote = client
                        .resolve_package(&owner, &snapshot.name, Some(&snapshot.manifest.version))
                        .await?;
                    anyhow::ensure!(
                        remote.package_id == package_id,
                        "resolved package identity changed"
                    );
                    let matches =
                        local_packages::matches_published_content(&snapshot, &remote).await?;
                    let current = local_packages::load_local_package(&server, &snapshot.name)?;
                    anyhow::ensure!(
                        current.as_ref() == Some(snapshot.as_ref()),
                        "local package changed during comparison"
                    );
                    Ok::<_, anyhow::Error>(matches)
                }
                .await
                .map_err(|error| error.to_string());
                (snapshot, result)
            },
            move |(snapshot, result)| Message::OwnedContentCompared {
                seq,
                fence,
                snapshot,
                result,
            },
        )
    }

    pub(super) fn owned_content_compared(
        &mut self,
        seq: ShareSeq,
        fence: AccountReadFence,
        snapshot: Arc<LocalPackage>,
        result: Result<bool, String>,
    ) -> Update<Message, Event> {
        if seq != self.share_seq
            || !self.account_read_is_current(fence)
            || !matches!(&self.selection, Selection::OwnedPackage(name) if name == &snapshot.name)
        {
            return Update::none();
        }
        self.share_content = match result {
            Ok(matches) if self.local_package.as_deref() == Some(snapshot.as_ref()) => {
                PublishedContent::Compared(snapshot, matches)
            }
            Err(error) => {
                log::warn!(
                    "Could not compare published package {}: {error}",
                    snapshot.name
                );
                PublishedContent::Unknown
            }
            Ok(_) => PublishedContent::Unknown,
        };
        Update::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(number: &str) -> VersionListItem {
        VersionListItem {
            version: number.into(),
            yanked: false,
            deleted: false,
            published_at: chrono::Utc::now(),
        }
    }

    fn window() -> AutomationsWindow {
        AutomationsWindow::new(
            iced::window::Id::unique(),
            "sharing-status-test".into(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        )
    }

    fn package() -> Arc<LocalPackage> {
        Arc::new(LocalPackage {
            name: "tools".into(),
            manifest: smudgy_core::models::shared_packages::PackageManifest::parse(
                r#"{"version":"0.1.9"}"#,
            )
            .unwrap(),
            readme: None,
            modules: Vec::new(),
        })
    }

    #[test]
    fn sharing_latest_uses_semver_and_all_reserved_numbers() {
        let versions = vec![version("0.1.9"), version("0.1.10"), version("0.1.10-rc.1")];
        assert!(is_latest_live("0.1.10", &versions));
        assert!(!is_latest_live("0.1.9", &versions));
        assert!(!is_latest_live("0.1.10-rc.1", &versions));
        assert!(!is_latest_live("bad", &versions));
        assert!(!is_latest_live("0.1.10", &[]));
        for (yanked, deleted) in [(true, false), (false, true)] {
            let mut versions = versions.clone();
            versions[1].yanked = yanked;
            versions[1].deleted = deleted;
            assert!(!is_latest_live("0.1.10", &versions));
            assert!(!is_latest_live("0.1.9", &versions));
        }
    }

    #[test]
    fn sharing_patch_skips_reserved_numbers_and_handles_overflow() {
        let mut reserved = version("0.1.10");
        reserved.deleted = true;
        assert_eq!(next_patch("0.1.9", &[reserved]), Some("0.1.11".into()));
        assert_eq!(next_patch("0.1.9-rc.1", &[]), Some("0.1.10".into()));
        assert_eq!(next_patch("bad", &[]), None);
        assert_eq!(next_patch("0.1.18446744073709551615", &[]), None);
    }

    #[test]
    fn sharing_comparison_rejects_stale_results_and_local_edits() {
        let mut window = window();
        let snapshot = package();
        window.selection = Selection::OwnedPackage(snapshot.name.clone());
        window.local_package = Some(Box::new(snapshot.as_ref().clone()));
        window.share_versions = vec![version("0.1.9")];
        let seq = window.share_seq;
        let fence = window.account_read_fence();
        let _ = window.owned_content_compared(seq, fence, snapshot.clone(), Ok(true));
        assert!(matches!(
            window.published_content(),
            PublishedContent::Compared(_, true)
        ));
        window.local_package.as_deref_mut().unwrap().readme = Some("edited".into());
        assert!(matches!(
            window.published_content(),
            PublishedContent::Unknown
        ));
        let _ = window.owned_content_compared(seq, fence, snapshot.clone(), Ok(true));
        assert!(matches!(window.share_content, PublishedContent::Unknown));
        window.local_package = Some(Box::new(snapshot.as_ref().clone()));
        window.share_seq.bump();
        let _ = window.owned_content_compared(seq, fence, snapshot.clone(), Ok(true));
        assert!(matches!(window.share_content, PublishedContent::Unknown));
        window.account_epoch += 1;
        let _ = window.owned_content_compared(window.share_seq, fence, snapshot.clone(), Ok(true));
        assert!(matches!(window.share_content, PublishedContent::Unknown));
        let fence = window.account_read_fence();
        let _ =
            window.owned_content_compared(window.share_seq, fence, snapshot, Err("offline".into()));
        assert!(matches!(window.share_content, PublishedContent::Unknown));
    }
}
