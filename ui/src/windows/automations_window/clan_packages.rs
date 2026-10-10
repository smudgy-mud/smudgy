//! Clan sections in Private & Shared. The access index decides which packages
//! are listed; resolving and installing retain the registry's authorization.

use futures::{StreamExt, stream};
use iced::Length;
use iced::widget::{Column, button, column, container, text};
use smudgy_cloud::clan_access::{IndexedResource, ResourceKind};
use smudgy_cloud::{CloudApiClient, CloudError, Uuid};

use crate::components::cloud_errors::display_error;
use crate::theme::builtins::button as button_style;
use crate::update::Update;

use super::packages::{AccountReadFence, card_with_trailing_action};
use super::{AutomationsWindow, Elem, Event, Message, Selection, common};

#[derive(Debug, Clone)]
pub struct ClanPackages {
    pub id: Uuid,
    pub name: String,
    pub packages: Result<Vec<IndexedResource>, CloudError>,
}

/// Fetch one permission-filtered index per clan, with bounded concurrency.
/// No per-package detail request is needed just to list or install a package.
pub async fn load(client: &CloudApiClient) -> Result<Vec<ClanPackages>, CloudError> {
    let clans = client.clans().await?.clans;
    let mut sections: Vec<_> = stream::iter(clans)
        .map(|clan| async move {
            let packages = client.clan_resources(clan.id, ResourceKind::Packages).await;
            ClanPackages {
                id: clan.id,
                name: clan.name,
                packages,
            }
        })
        .buffer_unordered(4)
        .collect()
        .await;
    sections.sort_by_cached_key(|section| (section.name.to_lowercase(), section.id));
    Ok(sections)
}

impl AutomationsWindow {
    pub(super) fn clan_packages_loaded(
        &mut self,
        request: u64,
        account_fence: AccountReadFence,
        result: Result<Vec<ClanPackages>, CloudError>,
    ) -> Update<Message, Event> {
        if request == self.shared_list_request
            && self.account_read_is_current(account_fence)
            && self.selection == Selection::Shared
        {
            self.clan_packages = Some(result);
        }
        Update::none()
    }

    pub(super) fn clan_package_sections(&self) -> Elem<'_> {
        let mut body = Column::new().spacing(16);
        let sections = match &self.clan_packages {
            Some(Ok(sections)) => sections,
            state => {
                body = body.push(common::section_label(crate::i18n::ts!(
                    "package-clan-packages"
                )));
                return body
                    .push(match state {
                        Some(Err(error)) => {
                            text(display_error(error)).size(13).style(common::danger)
                        }
                        _ => text(crate::i18n::t!("package-loading"))
                            .size(13)
                            .style(common::muted),
                    })
                    .into();
            }
        };
        for section in sections {
            if section.packages.as_ref().is_ok_and(Vec::is_empty) {
                continue;
            }
            body = body.push(common::section_label(&section.name));
            match &section.packages {
                Ok(packages) => {
                    for package in packages {
                        body = body.push(self.clan_package_card(package));
                    }
                }
                Err(error) => {
                    body = body.push(text(display_error(error)).size(13).style(common::danger));
                }
            }
        }
        body.into()
    }

    fn clan_package_card(&self, package: &IndexedResource) -> Elem<'_> {
        let action = if self
            .local_packages
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&package.name))
        {
            common::badge(crate::i18n::t!("package-local"))
        } else if super::model::is_installed(&self.installed_packages, "", &package.name) {
            common::badge(crate::i18n::t!("package-installed"))
        } else {
            button(text(crate::i18n::t!("package-install")).size(12))
                .style(button_style::primary)
                .on_press(Message::InstallShared {
                    // Clan packages use their global name, without an owner nickname.
                    owner: String::new(),
                    name: package.name.clone(),
                })
                .into()
        };
        let mut content = column![text(package.name.clone()).size(15)].spacing(3);
        if package.is_public == Some(false) {
            content = content.push(common::badge(crate::i18n::t!("package-private")));
        }
        container(card_with_trailing_action(content, action))
            .padding(12)
            .width(Length::Fill)
            .style(common::card_style)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use smudgy_core::models::shared_packages::{LockedPackage, UpdateMode};

    fn resource(name: &str) -> IndexedResource {
        serde_json::from_value(json!({
            "kind": "package", "id": Uuid::new_v4(), "name": name,
            "owner": { "kind": "clan" }, "is_public": false, "actions": ["package.read"]
        }))
        .unwrap()
    }

    fn section(name: &str, packages: Vec<IndexedResource>) -> ClanPackages {
        ClanPackages {
            id: Uuid::new_v4(),
            name: name.into(),
            packages: Ok(packages),
        }
    }

    fn window() -> AutomationsWindow {
        AutomationsWindow::new(
            iced::window::Id::unique(),
            "clan-packages-test".into(),
            crate::cloud_account::test_handles_signed_in("owner"),
            smudgy_core::session::SessionId::from(1),
        )
    }

    #[tokio::test]
    async fn loads_only_authorized_indexes_and_keeps_partial_results() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requested = Vec::new();
            for _ in 0..5 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let n = socket.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buffer[..n]);
                }
                let request = String::from_utf8(request).unwrap();
                let path = request.split_whitespace().nth(1).unwrap().to_string();
                let mut status = "200 OK";
                let response = if path == "/clans" {
                    json!({"data": {"clans": (["Zebra", "alpha", "Empty", "Unavailable"]
                        .iter().enumerate().map(|(n, name)| json!({
                            "id": Uuid::from_u128(n as u128 + 1), "name": name,
                            "created_at": "2026-10-09T00:00:00Z", "member_count": 1,
                            "is_owner": true
                        })).collect::<Vec<_>>())}})
                } else {
                    assert!(path.ends_with("/resources?kind=packages"));
                    if path.contains(&Uuid::from_u128(4).to_string()) {
                        status = "404 Not Found";
                        json!({"error": {"code": "not_found", "message": "Not found"}})
                    } else if path.contains(&Uuid::from_u128(3).to_string()) {
                        json!({"data": []})
                    } else {
                        json!({"data": [resource("private-clan-tools")]})
                    }
                }
                .to_string();
                requested.push(path);
                socket.write_all(format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()
                ).as_bytes()).await.unwrap();
            }
            requested
        });
        let client = CloudApiClient::new(
            format!("http://{address}"),
            smudgy_cloud::CredentialSource::new(Some(smudgy_cloud::Credential::ApiKey(
                "test".into(),
            ))),
        );
        let sections = tokio::time::timeout(std::time::Duration::from_secs(5), load(&client))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            sections.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["alpha", "Empty", "Unavailable", "Zebra"]
        );
        assert_eq!(
            sections[0].packages.as_ref().unwrap()[0].name,
            "private-clan-tools"
        );
        assert!(sections[1].packages.as_ref().unwrap().is_empty());
        assert!(sections[2].packages.is_err());
        assert_eq!(sections[3].packages.as_ref().unwrap().len(), 1);
        assert_eq!(
            server.await.unwrap().len(),
            5,
            "one inventory request per clan, no per-package requests"
        );
    }

    #[test]
    fn clan_inventory_rejects_old_refreshes_and_accounts() {
        let mut window = window();
        let _ = window.open_shared();
        let fence = window.account_read_fence();
        let old_request = window.shared_list_request;
        let _ = window.load_shared_cloud_lists();
        let _ = window.clan_packages_loaded(
            old_request,
            fence,
            Ok(vec![section("Old", vec![resource("old")])]),
        );
        assert!(window.clan_packages.is_none());
        let current = window.shared_list_request;
        let _ = window.clan_packages_loaded(
            current,
            fence,
            Ok(vec![section("Current", vec![resource("new")])]),
        );
        assert_eq!(
            window.clan_packages.as_ref().unwrap().as_ref().unwrap()[0].name,
            "Current"
        );
        window.cloud.credentials.set(None);
        let _ = window.account_changed();
        let _ = window.clan_packages_loaded(
            current,
            fence,
            Ok(vec![section("Old account", vec![resource("secret")])]),
        );
        assert!(window.clan_packages.is_none());
    }

    #[tokio::test]
    async fn sections_show_clans_and_local_installed_and_install_actions() {
        use iced::advanced::{
            Layout,
            layout::Limits,
            renderer::Headless,
            widget::{Operation, Tree},
        };
        use iced::{Rectangle, Size};
        #[derive(Default)]
        struct Texts(Vec<String>);
        impl Operation for Texts {
            fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
                visit(self);
            }
            fn text(&mut self, _: Option<&iced::widget::Id>, _: Rectangle, text: &str) {
                self.0.push(text.into());
            }
        }
        let mut window = window();
        window.local_packages = vec!["local-tools".into()];
        window.installed_packages = vec![LockedPackage::new(
            "smudgy:@installed-tools",
            UpdateMode::Auto,
        )];
        window.clan_packages = Some(Ok(vec![
            section(
                "Explorers",
                vec![resource("local-tools"), resource("installed-tools")],
            ),
            section("No packages", vec![]),
            section("Mapmakers", vec![resource("map-tools")]),
        ]));
        let renderer = <iced::Renderer as Headless>::new(
            crate::assets::fonts::GEIST_VF,
            16.0.into(),
            Some("tiny-skia"),
        )
        .await
        .unwrap();
        let mut view = window.clan_package_sections();
        let mut tree = Tree::new(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &Limits::new(Size::ZERO, Size::new(500.0, 800.0)),
        );
        let mut texts = Texts::default();
        view.as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut texts);
        for expected in [
            "EXPLORERS",
            "MAPMAKERS",
            "local-tools",
            "installed-tools",
            "map-tools",
        ] {
            assert!(texts.0.iter().any(|s| s == expected), "missing {expected}");
        }
        assert!(!texts.0.iter().any(|s| s == "NO PACKAGES"));
        assert_eq!(
            texts
                .0
                .iter()
                .filter(|s| **s == crate::i18n::t!("package-local"))
                .count(),
            1
        );
        assert_eq!(
            texts
                .0
                .iter()
                .filter(|s| **s == crate::i18n::t!("package-installed"))
                .count(),
            1
        );
        let messages = crate::widgets::dialog::tests::check_actions(
            window.clan_package_sections(),
            (400, 600),
            &[crate::i18n::t!("package-install")],
            "clan-package-sections",
        )
        .await;
        assert_eq!(messages.len(), 2);
        assert!(messages.iter().all(|m| matches!(m, Message::InstallShared {owner, name} if owner.is_empty() && name == "map-tools")));
    }
}
