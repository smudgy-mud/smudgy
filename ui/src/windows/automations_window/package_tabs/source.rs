//! Source-tab browsers for installed and local packages.

use super::super::packages::{
    FilePreview, SOURCE_PREVIEW_CAP_BYTES, classify_source, file_row, human_size,
};
use super::*;
use iced::Padding;
use iced::widget::{column, scrollable};
use smudgy_cloud::package_api::ResolvedPackageWire;
use smudgy_core::models::local_packages::LocalPackage;

impl AutomationsWindow {
    pub(super) fn installed_source_browser(&self) -> Elem<'_> {
        let detail = self.installed_detail.as_deref();
        let mut files = Column::new().spacing(2.0);
        if let Some(detail) = detail {
            for module in &detail.modules {
                let selected =
                    self.installed_selected_file.as_deref() == Some(module.subpath.as_str());
                files = files.push(file_row(
                    &module.subpath,
                    selected,
                    Message::SelectInstalledFile(module.subpath.clone()),
                ));
            }
        }

        let right: Elem = match detail {
            None => container(
                text(crate::i18n::t!("package-loading"))
                    .size(13.0)
                    .style(common::muted),
            )
            .padding(10.0)
            .into(),
            Some(detail) if detail.modules.is_empty() => container(
                text(crate::i18n::t!("package-no-source-files"))
                    .size(13.0)
                    .style(common::muted),
            )
            .padding(10.0)
            .into(),
            Some(detail) => match self.installed_selected_file.as_deref() {
                Some(subpath) => self.installed_source_view(detail, subpath),
                None => container(
                    text(crate::i18n::t!("package-select-source"))
                        .size(13.0)
                        .style(common::muted),
                )
                .padding(10.0)
                .into(),
            },
        };

        row![
            container(scrollable(files)).width(Length::Fixed(220.0)),
            container(right)
                .width(Length::Fill)
                .height(Length::Fixed(320.0))
                .style(common::code_surface_style),
        ]
        .spacing(12.0)
        .into()
    }

    /// Render the right-hand pane of the installed-package source browser for the selected
    /// (non-README) file: its fetched source, or a placeholder for the loading / binary / oversized
    /// / error states. The body is read from the content-addressed cache keyed by the module's
    /// `content_hash`; the fetch is kicked off in [`Self::ensure_selected_source`].
    pub(super) fn installed_source_view<'a>(
        &'a self,
        detail: &'a ResolvedPackageWire,
        subpath: &str,
    ) -> Elem<'a> {
        let placeholder = |message: String| -> Elem<'a> {
            container(text(message).size(13.0).style(common::muted))
                .padding(10.0)
                .into()
        };
        let Some(module) = detail.modules.iter().find(|m| m.subpath == subpath) else {
            return placeholder(crate::i18n::t!("package-source-missing"));
        };
        match self.installed_source.get(&module.content_hash) {
            None | Some(FilePreview::Loading) => {
                placeholder(crate::i18n::t!("package-source-fetching"))
            }
            Some(FilePreview::Text { source, bidi, nul }) => {
                let code = scrollable(
                    container(text(source.as_str()).size(12.0).font(fonts::GEIST_MONO_VF))
                        .padding(10.0)
                        .width(Length::Fill),
                )
                .height(Length::Fill);
                // Trojan-Source warning: if the body carries bidi/invisible control characters, the
                // rendered order can differ from what the engine runs, so caution the auditor rather
                // than trusting their eyes. Pinned above the (scrolling) source so it stays visible.
                if !*bidi && !*nul {
                    code.height(Length::Fixed(320.0)).into()
                } else {
                    let mut warnings = Column::new().spacing(4.0);
                    if *bidi {
                        warnings = warnings.push(
                            text(crate::i18n::t!("package-source-bidi-warning"))
                                .size(11.0)
                                .style(common::muted),
                        );
                    }
                    if *nul {
                        warnings = warnings.push(
                            text(crate::i18n::t!("package-source-nul-warning"))
                                .size(11.0)
                                .style(common::muted),
                        );
                    }
                    column![
                        container(warnings)
                            .padding(8.0)
                            .width(Length::Fill)
                            .style(common::banner_style),
                        code,
                    ]
                    .height(Length::Fixed(320.0))
                    .into()
                }
            }
            Some(FilePreview::Binary { size }) => placeholder(crate::i18n::t!(
                "package-source-binary",
                "size" => human_size(*size)
            )),
            Some(FilePreview::TooLarge { size }) => placeholder(crate::i18n::t!(
                "package-source-too-large",
                "size" => human_size(*size),
                "limit" => human_size(SOURCE_PREVIEW_CAP_BYTES)
            )),
            Some(FilePreview::Error(error)) => placeholder(crate::i18n::t!(
                "package-source-load-error",
                "error" => error.to_string()
            )),
        }
    }

    pub(super) fn owned_file_browser<'a>(&'a self, package: &'a LocalPackage) -> Elem<'a> {
        // A platform-aware "reveal the package folder in the OS file manager" affordance, so the
        // author can drag files in, open the folder in an external editor, or use git.
        let reveal_label = if cfg!(target_os = "windows") {
            crate::i18n::ts!("package-show-explorer")
        } else if cfg!(target_os = "macos") {
            crate::i18n::ts!("package-show-finder")
        } else {
            crate::i18n::ts!("package-open-folder")
        };
        let source_header = row![
            common::section_label(crate::i18n::ts!("package-tab-source")),
            iced::widget::space::horizontal(),
            button(text(reveal_label).size(11.0))
                .style(button_style::subtle)
                .on_press(Message::RevealPackageFolder),
        ]
        .align_y(Vertical::Center);
        let mut files = Column::new().spacing(2.0).push(source_header);
        let readme_selected = self.owned_selected_file.is_none()
            || self.owned_selected_file.as_deref() == Some("README.md");
        if package.readme.is_some() {
            files = files.push(file_row(
                "README.md",
                readme_selected,
                Message::SelectOwnedFile("README.md".to_string()),
            ));
        }
        // `smudgy.package.json` is intentionally not listed: the manifest is edited through the
        // rich manifest editor (`view_manifest_section`) instead of a raw text editor.
        for module in &package.modules {
            let selected = self.owned_selected_file.as_deref() == Some(module.subpath.as_str());
            files = files.push(file_row(
                &module.subpath,
                selected,
                Message::SelectOwnedFile(module.subpath.clone()),
            ));
        }

        let right: Elem<'a> = if self.owned_selected_file.is_none() {
            if let Some(readme) = &self.local_readme {
                scrollable(readme::view(readme))
                    .height(Length::Fixed(340.0))
                    .into()
            } else {
                container(
                    text(crate::i18n::t!("package-select-file-edit"))
                        .size(13.0)
                        .style(common::muted),
                )
                .padding(10.0)
                .into()
            }
        } else {
            let selected = self.owned_selected_file.as_deref().unwrap_or_default();
            let preview = package
                .modules
                .iter()
                .find(|module| module.subpath == selected)
                .map(|module| classify_source(module.content.clone()));
            match preview {
                Some(FilePreview::TooLarge { size }) => container(
                    text(crate::i18n::t!(
                        "package-source-too-large",
                        "size" => human_size(size),
                        "limit" => human_size(SOURCE_PREVIEW_CAP_BYTES)
                    ))
                    .size(13.0)
                    .style(common::muted),
                )
                .padding(10.0)
                .into(),
                Some(FilePreview::Binary { size }) => container(
                    text(crate::i18n::t!(
                        "package-source-binary",
                        "size" => human_size(size)
                    ))
                    .size(13.0)
                    .style(common::muted),
                )
                .padding(10.0)
                .into(),
                Some(FilePreview::Text {
                    source, nul: true, ..
                }) => column![
                    container(
                        text(crate::i18n::t!("package-source-nul-warning"))
                            .size(11.0)
                            .style(common::muted),
                    )
                    .padding(8.0)
                    .width(Length::Fill)
                    .style(common::banner_style),
                    scrollable(
                        container(text(source).size(12.0).font(fonts::GEIST_MONO_VF))
                            .padding(10.0)
                            .width(Length::Fill),
                    )
                    .height(Length::Fixed(300.0)),
                ]
                .into(),
                _ => {
                    let editor = self.code_editor_view(Length::Fixed(300.0));
                    column![
                        editor,
                        row![
                            iced::widget::space::horizontal(),
                            button(text(crate::i18n::t!("action-save")).size(12.0))
                                .style(button_style::primary)
                                .on_press_maybe(
                                    (!self.authoring_busy).then_some(Message::SaveOwnedFile),
                                ),
                        ]
                        .padding(Padding {
                            top: 6.0,
                            bottom: 0.0,
                            left: 0.0,
                            right: 0.0,
                        })
                        .align_y(Vertical::Center),
                    ]
                    .spacing(0.0)
                    .into()
                }
            }
        };

        row![
            container(scrollable(files)).width(Length::Fixed(220.0)),
            container(right)
                .width(Length::Fill)
                .style(common::code_surface_style)
                .padding(6.0),
        ]
        .spacing(12.0)
        .into()
    }
}
