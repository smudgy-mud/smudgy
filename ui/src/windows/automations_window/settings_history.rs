//! Ordinary settings transfer and recovery. Every operation captures its destination and revision;
//! asynchronous clipboard/database results can never apply to a newly selected package/profile.

use iced::widget::{button, column, container, mouse_area, opaque, row, scrollable, text, tooltip};
use iced::{Background, Length, Task};
use smudgy_core::models::shared_packages::{
    self, LockedPackage, PackageParamCommit, PackageParamMutation, PackageParameter, ParamKind,
    ParamValueScope, ParameterScope,
};
use smudgy_core::storage::{HistoryEntry, SettingsSnapshot};

use super::packages::ParamConfig;
use super::{AutomationsWindow, Elem, Event, Message, common};
use crate::theme::builtins::button as button_style;
use crate::update::Update;
use crate::widgets::{dropdown::Dropdown, modal_layer::ModalLayer};

// Allow room for schema metadata and formatting around the 4 MiB stored-value limit.
const MAX_CLIPBOARD_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Destination {
    server: String,
    package: LockedPackage,
    profile: String,
    params: Vec<PackageParameter>,
    revision: i64,
    request: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use smudgy_core::models::shared_packages::{PackageManifest, UpdateMode};

    fn destination() -> Destination {
        let params = PackageManifest::parse(
            &json!({"version":"1.0.0","params":[
                {"key":"enabled","type":"bool"}, {"key":"count","type":"number"},
                {"key":"name"}, {"key":"token","secret":true},
                {"key":"rows","type":"table","fields":[{"key":"label"}]}
            ]})
            .to_string(),
        )
        .unwrap()
        .params;
        Destination {
            server: "destination-server".into(),
            profile: "Alt".into(),
            package: LockedPackage::new("smudgy://author/example", UpdateMode::Auto),
            params,
            revision: 0,
            request: 0,
        }
    }

    fn snapshot(values: serde_json::Value) -> SettingsSnapshot {
        SettingsSnapshot {
            format: 1,
            package: "smudgy://author/example".into(),
            values: serde_json::from_value(values).unwrap(),
            parameters: Vec::new(),
            version: Some("1.0.0".into()),
        }
    }

    #[test]
    fn preview_preserves_false_zero_unset_and_absent_fields() {
        let destination = destination();
        let current =
            snapshot(json!({"enabled":true,"count":9,"name":"keep or clear","token":"never show"}));
        let incoming = snapshot(json!({"enabled":false,"count":0}));
        let preview = preview_values(&destination, &incoming, &current).unwrap();
        assert_eq!(preview.mutations.len(), 2);
        assert!(preview.mutations.contains(&PackageParamMutation::SetValue {
            key: "enabled".into(),
            value: json!(false)
        }));
        assert!(preview.mutations.contains(&PackageParamMutation::SetValue {
            key: "count".into(),
            value: json!(0)
        }));
        let clear =
            preview_values(&destination, &snapshot(json!({"name":null})), &current).unwrap();
        assert_eq!(
            clear.mutations,
            vec![PackageParamMutation::ClearValue { key: "name".into() }]
        );
        let empty = preview_values(&destination, &snapshot(json!({"name":""})), &current).unwrap();
        assert_eq!(
            empty.mutations,
            vec![PackageParamMutation::SetValue {
                key: "name".into(),
                value: json!("")
            }]
        );
    }

    #[test]
    fn preview_skips_secrets_unknown_fields_and_incompatible_tables_without_exposing_values() {
        let incoming = snapshot(
            json!({"token":"sensitive","removed":123,"rows":[{"label":"a","removed":"b"}],"enabled":"false"}),
        );
        let result = preview_values(&destination(), &incoming, &snapshot(json!({}))).unwrap();
        assert!(result.mutations.is_empty());
        assert_eq!(result.skipped.len(), 4);
        assert!(!format!("{:?}", result.changes).contains("sensitive"));
    }

    #[test]
    fn transfer_rejects_other_packages_and_unknown_formats() {
        let destination = destination();
        let current = snapshot(json!({}));
        let mut incoming = snapshot(json!({"name":"value"}));
        incoming.package = "smudgy://someone/else".into();
        assert!(preview_values(&destination, &incoming, &current).is_err());
        incoming.package = current.package.clone();
        incoming.format = 2;
        assert!(preview_values(&destination, &incoming, &current).is_err());
    }
    #[test]
    fn history_tooltips_bound_fields_unicode_and_values_and_omit_secrets() {
        let incoming = snapshot(
            json!({"enabled":false,"count":0,"name":"αβγ".repeat(100),"token":"sensitive","unknown":1}),
        );
        let current = snapshot(json!({"enabled":true,"count":7,"name":"before"}));
        let preview = preview_values(&destination(), &incoming, &current).unwrap();
        let summary = HistorySummary::from_preview(&preview);
        assert_eq!(summary.changes.len(), 3);
        assert_eq!(summary.skipped, 2);
        assert!(summary.changes[0].description.contains('7'));
        assert!(summary.changes[0].description.contains('0'));
        assert!(summary.changes[2].description.contains('…'));
        assert!(summary.changes[2].description.chars().count() < 200);
        assert!(!format!("{summary:?}").contains("sensitive"));
        let many = Preview {
            mutations: vec![],
            changes: vec![preview.changes[0].clone(); 50],
            remaining: 0,
            skipped: vec![],
        };
        let summary = HistorySummary::from_preview(&many);
        assert_eq!(summary.changes.len(), TOOLTIP_FIELDS);
        assert_eq!(summary.remaining, 45);
        assert!(!tooltip_text("long\nlabel\tvalue").contains(['\n', '\t']));
    }

    fn collection_destination() -> Destination {
        let mut target = destination();
        target.params = PackageManifest::parse(&json!({"version":"1.0.0","params":[
            {"key":"items","label":"Items","type":"list","fields":[{"key":"item"}]},
            {"key":"rows","label":"Supplies","type":"table","fields":[
                {"key":"name","label":"Name"}, {"key":"count","label":"Quantity","type":"number"}
            ]}
        ]}).to_string()).unwrap().params;
        target
    }

    #[test]
    fn inserted_and_removed_entries_do_not_repeat_unchanged_rows() {
        let target = collection_destination();
        let current = snapshot(
            json!({"rows":[{"name":"keep first","count":1},{"name":"keep last","count":2}]}),
        );
        let incoming = snapshot(
            json!({"rows":[{"name":"keep first","count":1},{"name":"new item","count":3},{"name":"keep last","count":2}]}),
        );
        let added = preview_values(&target, &incoming, &current).unwrap();
        assert_eq!(added.changes.len(), 1);
        assert!(added.changes[0].description.contains("new item"));
        assert!(!added.changes[0].description.contains("keep"));
        assert!(!added.changes[0].description.contains(['{', '}', '[', ']']));
        let removed = preview_values(&target, &current, &incoming).unwrap();
        assert_eq!(removed.changes.len(), 1);
        assert!(removed.changes[0].description.contains("new item"));
        assert_ne!(added.changes[0].description, removed.changes[0].description);
    }

    #[test]
    fn edited_row_shows_only_the_changed_cell() {
        let target = collection_destination();
        let current = snapshot(json!({"rows":[{"name":"unchanged name","count":2}]}));
        let incoming = snapshot(json!({"rows":[{"name":"unchanged name","count":3}]}));
        let preview = preview_values(&target, &incoming, &current).unwrap();
        assert_eq!(preview.changes.len(), 1);
        let change = &preview.changes[0];
        assert!(change.label.contains("Quantity"));
        assert!(change.description.contains('2') && change.description.contains('3'));
        assert!(!change.description.contains("unchanged name"));
    }

    #[test]
    fn duplicate_entries_reordering_and_large_changes_have_honest_summaries() {
        let target = collection_destination();
        let current = snapshot(json!({"items":["same","same","last"]}));
        let incoming = snapshot(json!({"items":["same","last"]}));
        let preview = preview_values(&target, &incoming, &current).unwrap();
        assert_eq!(preview.changes.len(), 1);
        assert!(preview.changes[0].description.contains("same"));
        let reordered = snapshot(json!({"items":["last","same","same"]}));
        let preview = preview_values(&target, &reordered, &current).unwrap();
        assert_eq!(preview.changes.len(), 1);
        assert_eq!(
            preview.changes[0].description,
            crate::i18n::t!("package-settings-change-reorder")
        );
        let large = snapshot(json!({"items":(0..250).map(|n| n.to_string()).collect::<Vec<_>>()}));
        let preview = preview_values(&target, &large, &snapshot(json!({"items":[]}))).unwrap();
        assert_eq!(preview.changes.len(), 100);
        assert_eq!(preview.remaining, 150);
        assert_eq!(HistorySummary::from_preview(&preview).remaining, 245);
        assert_eq!(
            preview.mutations.len(),
            1,
            "truncation must not truncate the restored values"
        );
    }

    #[test]
    fn values_use_friendly_choice_labels_and_boolean_names() {
        let target = destination();
        assert_eq!(
            display_value(&target.params[0], &json!(false)),
            crate::i18n::t!("package-settings-value-off")
        );
        let parameter = PackageManifest::parse(&json!({"version":"1.0.0","params":[
            {"key":"mode","type":"dropdown","options":[{"value":"internal_id","label":"Friendly choice"}]}
        ]}).to_string()).unwrap().params.remove(0);
        assert_eq!(
            display_value(&parameter, &json!("internal_id")),
            "Friendly choice"
        );
        assert_eq!(
            display_value(&parameter, &json!("")),
            crate::i18n::t!("package-settings-value-empty")
        );
    }

    fn use_test_home() {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            smudgy_core::set_smudgy_home(tempfile::tempdir().unwrap().keep());
        });
    }

    #[test]
    fn clipboard_roundtrip_between_servers_and_profiles_survives_reinstall() {
        use_test_home();
        let mut target = destination();
        target.server = format!("settings-transfer-target-{}", std::process::id());
        let source = format!("settings-transfer-source-{}", std::process::id());
        target.package.parameter_scope = ParameterScope::Profile;
        let specifier = target.package.specifier.clone();
        for server in [&source, &target.server] {
            shared_packages::save_lock(
                server,
                &shared_packages::SharedPackageLock {
                    packages: vec![target.package.clone()],
                },
            )
            .unwrap();
            shared_packages::prepare_package_parameters(server, &target.package, &target.params)
                .unwrap();
        }
        target.params.extend(
            PackageManifest::parse(r#"{"version":"1.0.0","params":[{"key":"destination_only"}]}"#)
                .unwrap()
                .params,
        );
        shared_packages::prepare_package_parameters(
            &target.server,
            &target.package,
            &target.params,
        )
        .unwrap();
        shared_packages::save_param_value_scoped(
            &source,
            ParamValueScope::Profile("Main"),
            &specifier,
            "enabled",
            json!(false),
        )
        .unwrap();
        shared_packages::save_param_value_scoped(
            &source,
            ParamValueScope::Profile("Main"),
            &specifier,
            "count",
            json!(0),
        )
        .unwrap();
        shared_packages::save_param_value_scoped(
            &target.server,
            target.scope(),
            &specifier,
            "name",
            json!("clear this"),
        )
        .unwrap();
        shared_packages::save_param_value_scoped(
            &target.server,
            target.scope(),
            &specifier,
            "destination_only",
            json!("keep this"),
        )
        .unwrap();
        let copied = shared_packages::settings_snapshot(
            &source,
            ParamValueScope::Profile("Main"),
            &specifier,
            &destination().params,
        )
        .unwrap();
        let clipboard = serde_json::to_string(&copied).unwrap();
        assert!(!clipboard.contains("token"));
        let pasted: SettingsSnapshot = serde_json::from_str(&clipboard).unwrap();
        target.revision =
            shared_packages::parameter_revision(&target.server, target.scope(), &specifier)
                .unwrap();
        let preview = preview(&target, &pasted).unwrap();
        assert_eq!(
            shared_packages::commit_package_params_at_revision(
                &target.server,
                target.scope(),
                &target.package,
                target.revision,
                &preview.mutations
            )
            .unwrap(),
            PackageParamCommit::Applied
        );
        let read = |key| {
            shared_packages::get_param_value_scoped_checked(
                &target.server,
                target.scope(),
                &specifier,
                key,
            )
            .unwrap()
        };
        assert_eq!(read("enabled"), Some(json!(false)));
        assert_eq!(read("count"), Some(json!(0)));
        assert_eq!(read("name"), None);
        assert_eq!(read("destination_only"), Some(json!("keep this")));
        shared_packages::uninstall_package(&target.server, &specifier).unwrap();
        shared_packages::install_package(&target.server, &specifier, UpdateMode::Auto, true)
            .unwrap();
        assert_eq!(read("count"), Some(json!(0)));
        assert_eq!(
            shared_packages::load_lock(&target.server)
                .unwrap()
                .find(&specifier)
                .unwrap()
                .parameter_scope,
            ParameterScope::Profile
        );
        assert!(
            !shared_packages::settings_history(
                &target.server,
                target.scope(),
                &specifier,
                &target.params
            )
            .unwrap()
            .is_empty()
        );
    }

    fn settings_window(target: &Destination) -> AutomationsWindow {
        use_test_home();
        let mut window = AutomationsWindow::new(
            iced::window::Id::unique(),
            target.server.clone(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        window.param_config = Some(ParamConfig {
            specifier: target.package.specifier.clone(),
            expected_package: Some(target.package.clone()),
            parameter_scope: ParameterScope::Global,
            profile_name: target.profile.clone(),
            available: true,
            params: target.params.clone(),
            values: Default::default(),
            secret_stored: Default::default(),
            touched: Default::default(),
            error: None,
            saved: false,
            revision: target.revision,
            saving: false,
        });
        window
    }

    #[test]
    fn clipboard_rejects_non_settings_and_incompatible_only_payloads() {
        let target = destination();
        for text in [
            None,
            Some(""),
            Some("plain text"),
            Some("[]"),
            Some("null"),
            Some("{bad json}"),
            Some("{}"),
        ] {
            assert!(parse_clipboard(&target, text).is_err());
        }
        for values in [
            json!({}),
            json!({"unknown":"x"}),
            json!({"token":"secret"}),
            json!({"enabled":"false"}),
            json!({"rows":[{"removed":"x"}]}),
            json!({"count":[]}),
        ] {
            let text = serde_json::to_string(&snapshot(values)).unwrap();
            assert!(parse_clipboard(&target, Some(&text)).is_err());
        }
        let mut incoming = snapshot(json!({"name":"hello"}));
        incoming.parameters = target.params.clone();
        incoming
            .parameters
            .iter_mut()
            .find(|p| p.key == "name")
            .unwrap()
            .secret = true;
        assert!(
            parse_clipboard(&target, Some(&serde_json::to_string(&incoming).unwrap())).is_err()
        );
        incoming
            .parameters
            .iter_mut()
            .find(|p| p.key == "name")
            .unwrap()
            .secret = false;
        incoming
            .parameters
            .iter_mut()
            .find(|p| p.key == "name")
            .unwrap()
            .kind = ParamKind::Number;
        assert!(
            parse_clipboard(&target, Some(&serde_json::to_string(&incoming).unwrap())).is_err()
        );
    }

    #[test]
    fn clipboard_accepts_compatible_values_including_clears_and_partial_matches() {
        let target = destination();
        for values in [
            json!({"enabled":false}),
            json!({"count":0}),
            json!({"name":null}),
            json!({"name":""}),
            json!({"rows":[]}),
            json!({"unknown":3,"token":"secret","name":"usable"}),
        ] {
            let text = serde_json::to_string(&snapshot(values)).unwrap();
            assert!(parse_clipboard(&target, Some(&text)).is_ok());
        }
        let escaped = serde_json::to_string(&snapshot(json!({"name":"value"})))
            .unwrap()
            .replace("author", r"\u0061uthor");
        assert!(parse_clipboard(&target, Some(&escaped)).is_ok());
    }

    #[test]
    fn clipboard_fast_filters_run_before_materializing_values() {
        let oversized = " ".repeat(MAX_CLIPBOARD_BYTES + 1);
        assert!(!plausible_settings_clipboard(Some(&oversized)));
        assert!(
            parse_clipboard(&destination(), Some(&oversized))
                .unwrap_err()
                .to_string()
                .contains("16 MiB")
        );
        assert!(!plausible_settings_clipboard(Some("[1,2,3]")));
        assert!(plausible_settings_clipboard(Some(" \n{\"format\":1}\t ")));
        // Invalid values would fail typed deserialization, but header rejection must win.
        let wrong_package = r#"{"format":1,"package":"other","values":42}"#;
        assert!(
            parse_clipboard(&destination(), Some(wrong_package))
                .unwrap_err()
                .to_string()
                .contains("different package")
        );
        let wrong_format = r#"{"format":999,"package":"other","values":42}"#;
        assert!(
            parse_clipboard(&destination(), Some(wrong_format))
                .unwrap_err()
                .to_string()
                .contains("Unsupported settings format")
        );
    }

    #[test]
    fn clipboard_menu_checks_are_scoped_to_one_opening_and_destination() {
        let target = destination();
        let mut window = settings_window(&target);
        let check = window.update_settings_history(SettingsMessage::Menu(true));
        assert!(check.task.units() > 0);
        assert!(!window.settings_can_paste());
        let original = window.settings_destination().unwrap();
        let original_menu = window.settings_menu_request;
        assert_eq!(
            window.settings_request, target.request,
            "menu checks must not cancel pending settings actions"
        );
        assert_eq!(
            window
                .update_settings_history(SettingsMessage::Menu(true))
                .task
                .units(),
            0
        );
        assert_eq!(
            window
                .update_settings_history(SettingsMessage::Paste)
                .task
                .units(),
            0
        );
        assert!(window.settings_menu_open);
        let _ = window.update_settings_history(SettingsMessage::MenuClipboardChecked(
            original_menu,
            original.clone(),
            true,
        ));
        assert!(window.settings_can_paste());
        let _ = window.update_settings_history(SettingsMessage::Menu(false));
        let _ = window.update_settings_history(SettingsMessage::MenuClipboardChecked(
            original_menu,
            original.clone(),
            true,
        ));
        assert!(!window.settings_can_paste());
        let _ = window.update_settings_history(SettingsMessage::Menu(true));
        let reopened = window.settings_destination().unwrap();
        let reopened_menu = window.settings_menu_request;
        let _ = window.update_settings_history(SettingsMessage::MenuClipboardChecked(
            original_menu,
            original,
            true,
        ));
        assert!(!window.settings_can_paste());
        let _ = window.update_settings_history(SettingsMessage::MenuClipboardChecked(
            reopened_menu,
            reopened.clone(),
            false,
        ));
        assert!(!window.settings_can_paste());
        assert!(window.param_config.as_ref().unwrap().error.is_none());
        assert!(window.settings_dialog.is_none());
        let _ = window.update_settings_history(SettingsMessage::MenuClipboardChecked(
            reopened_menu,
            reopened.clone(),
            true,
        ));
        assert!(window.settings_can_paste());
        window.param_config.as_mut().unwrap().params[0].secret = true;
        assert!(
            !window.settings_can_paste(),
            "changed schema invalidates the check"
        );
        window.param_config.as_mut().unwrap().params[0].secret = false;
        window.param_config.as_mut().unwrap().profile_name = "Other".into();
        let _ = window.update_settings_history(SettingsMessage::MenuClipboardChecked(
            reopened_menu,
            reopened.clone(),
            true,
        ));
        assert!(!window.settings_can_paste());
        window.param_config.as_mut().unwrap().profile_name = target.profile;
        window.param_config.as_mut().unwrap().revision += 1;
        assert!(!window.settings_can_paste());
        window.param_config.as_mut().unwrap().revision -= 1;
        window.param_config.as_mut().unwrap().saving = true;
        assert!(!window.settings_can_paste());
        window.param_config.as_mut().unwrap().saving = false;
        assert!(window.settings_can_paste());
        let paste = window.update_settings_history(SettingsMessage::Paste);
        assert!(
            paste.task.units() > 0,
            "Paste must read the clipboard again"
        );
        assert!(!window.settings_menu_open);
        assert!(window.settings_paste_ready.is_none());
        let _ = window.update_settings_history(SettingsMessage::MenuClipboardChecked(
            reopened_menu,
            reopened,
            true,
        ));
        assert!(!window.settings_can_paste());
    }

    #[test]
    fn late_clipboard_and_history_results_cannot_reopen_cancelled_or_retargeted_dialogs() {
        use_test_home();
        let target = destination();
        let mut window = settings_window(&target);
        assert!(window.settings_destination_matches(&target));
        for source in [PreviewSource::Clipboard, PreviewSource::History] {
            let preview = preview_values(
                &target,
                &snapshot(json!({"name":"new"})),
                &snapshot(json!({})),
            )
            .unwrap();
            let _ = window.update_settings_history(SettingsMessage::Loaded(
                target.clone(),
                Ok(Loaded::Preview(preview, source)),
            ));
            let Some(SettingsDialog {
                kind: DialogKind::Preview(_, actual),
                ..
            }) = window.settings_dialog.as_ref()
            else {
                panic!("matching preview result should open its dialog");
            };
            assert_eq!(
                std::mem::discriminant(actual),
                std::mem::discriminant(&source)
            );
        }
        window.open_settings_dialog(target.clone(), DialogKind::History(vec![]));
        let _ = window.update(Message::OpenPalette);
        assert!(
            !window.palette_open,
            "modal must block background shortcuts"
        );
        let _ = window.update_settings_history(SettingsMessage::Cancel);
        let _ = window.update_settings_history(SettingsMessage::Loaded(
            target.clone(),
            Ok(Loaded::History(vec![])),
        ));
        assert!(window.settings_dialog.is_none());
        let _ = window.update_settings_history(SettingsMessage::MenuClipboardChecked(
            0,
            target.clone(),
            true,
        ));
        assert!(window.param_config.as_ref().unwrap().error.is_none());
        window.settings_request = target.request;
        window.param_config.as_mut().unwrap().profile_name = "Another profile".into();
        let _ = window
            .update_settings_history(SettingsMessage::Loaded(target, Ok(Loaded::History(vec![]))));
        assert!(window.settings_dialog.is_none());
    }
}

impl Destination {
    fn scope(&self) -> ParamValueScope<'_> {
        match self.package.parameter_scope {
            ParameterScope::Global => ParamValueScope::Global,
            ParameterScope::Profile => ParamValueScope::Profile(&self.profile),
        }
    }

    fn snapshot(&self) -> anyhow::Result<SettingsSnapshot> {
        shared_packages::settings_snapshot(
            &self.server,
            self.scope(),
            &self.package.specifier,
            &self.params,
        )
    }
}

#[derive(Debug, Clone)]
pub enum SettingsMessage {
    Menu(bool),
    Copy,
    Paste,
    MenuClipboardChecked(u64, Destination, bool),
    History,
    Reset,
    Forget,
    Select(usize),
    Cancel,
    Apply,
    Loaded(Destination, Result<Loaded, String>),
    Finished(Destination, Result<Box<ParamConfig>, String>),
}

#[derive(Debug, Clone)]
pub enum Loaded {
    Copy(String),
    History(Vec<HistoryChoice>),
    Preview(Preview, PreviewSource),
    Forgotten,
}

#[derive(Debug, Clone, Copy)]
pub enum PreviewSource {
    Clipboard,
    History,
}

#[derive(Debug, Clone)]
pub struct Preview {
    mutations: Vec<PackageParamMutation>,
    changes: Vec<SettingChange>,
    remaining: usize,
    skipped: Vec<String>,
}

impl Preview {
    fn push_change(&mut self, change: SettingChange) {
        if self.changes.len() < 100 {
            self.changes.push(change);
        } else {
            self.remaining += 1;
        }
    }
}

#[derive(Debug, Clone)]
struct SettingChange {
    label: String,
    description: String,
}

#[derive(Debug, Clone)]
pub struct HistoryChoice {
    entry: HistoryEntry,
    summary: HistorySummary,
}

#[derive(Debug, Clone)]
struct HistorySummary {
    changes: Vec<SettingChange>,
    remaining: usize,
    skipped: usize,
}

const TOOLTIP_FIELDS: usize = 5;
const TOOLTIP_CHARS: usize = 72;

fn tooltip_text(text: &str) -> String {
    let mut chars = text.chars().map(|c| if c.is_control() { ' ' } else { c });
    let mut truncated: String = chars.by_ref().take(TOOLTIP_CHARS).collect();
    if chars.next().is_some() {
        truncated.push('…');
    }
    truncated
}

impl HistorySummary {
    fn from_preview(preview: &Preview) -> Self {
        Self {
            changes: preview
                .changes
                .iter()
                .take(TOOLTIP_FIELDS)
                .map(|change| SettingChange {
                    label: tooltip_text(&change.label),
                    description: change.description.clone(),
                })
                .collect(),
            remaining: preview.changes.len().saturating_sub(TOOLTIP_FIELDS) + preview.remaining,
            skipped: preview.skipped.len(),
        }
    }
}

fn view_history_summary(summary: &HistorySummary) -> Elem<'_> {
    let mut body =
        column![text(crate::i18n::t!("package-settings-diff-title")).size(12)].spacing(10);
    if summary.changes.is_empty() && summary.skipped == 0 {
        body = body.push(
            text(crate::i18n::t!("package-settings-no-changes"))
                .size(12)
                .style(common::muted),
        );
    }
    for change in &summary.changes {
        body = body.push(
            column![
                text(&change.label).size(12),
                text(&change.description).size(12).style(common::muted),
            ]
            .spacing(2),
        );
    }
    if summary.remaining > 0 {
        body = body.push(text(crate::i18n::t!("package-settings-diff-more", "count" => summary.remaining.to_string())).size(11).style(common::muted));
    }
    if summary.skipped > 0 {
        body = body.push(text(crate::i18n::t!("package-settings-diff-skipped", "count" => summary.skipped.to_string())).size(11).style(common::muted));
    }
    container(body)
        .width(400)
        .padding(12)
        .style(common::card_style)
        .into()
}

#[derive(Debug, Clone)]
enum DialogKind {
    History(Vec<HistoryChoice>),
    Preview(Preview, PreviewSource),
    Reset,
    Forget,
}

#[derive(Debug, Clone)]
pub(super) struct SettingsDialog {
    destination: Destination,
    kind: DialogKind,
    busy: bool,
    error: Option<String>,
}

fn message(value: SettingsMessage) -> Message {
    Message::SettingsHistory(value)
}

fn task(
    destination: Destination,
    operation: impl FnOnce(&Destination) -> anyhow::Result<Loaded> + Send + 'static,
) -> Update<Message, Event> {
    Update::new(
        Task::perform(
            async move {
                let worker = destination.clone();
                let result = tokio::task::spawn_blocking(move || {
                    operation(&worker).map_err(|e| e.to_string())
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(std::convert::identity);
                message(SettingsMessage::Loaded(destination, result))
            },
            std::convert::identity,
        ),
        None,
    )
}

// Values are formatted as people see them in Settings, never as JSON or patch syntax.
fn display_value(parameter: &PackageParameter, value: &serde_json::Value) -> String {
    let text = match value {
        serde_json::Value::String(value) if value.is_empty() => {
            crate::i18n::t!("package-settings-value-empty")
        }
        serde_json::Value::String(value) => parameter
            .options
            .iter()
            .find(|option| option.value == *value)
            .map_or_else(
                || value.clone(),
                |option| option.display_label().to_string(),
            ),
        serde_json::Value::Bool(true) => crate::i18n::t!("package-settings-value-on"),
        serde_json::Value::Bool(false) => crate::i18n::t!("package-settings-value-off"),
        serde_json::Value::Number(value) => value.to_string(),
        _ => crate::i18n::t!("package-settings-value-empty"),
    };
    tooltip_text(&text)
}

fn scalar_change(
    parameter: &PackageParameter,
    label: String,
    before: Option<&serde_json::Value>,
    after: Option<&serde_json::Value>,
) -> SettingChange {
    let description = match (before, after) {
        (None, Some(value)) => {
            crate::i18n::t!("package-settings-change-add", "value" => display_value(parameter, value))
        }
        (Some(value), None) => {
            crate::i18n::t!("package-settings-change-remove", "value" => display_value(parameter, value))
        }
        (Some(before), Some(after)) => crate::i18n::t!("package-settings-change-replace",
            "before" => display_value(parameter, before), "after" => display_value(parameter, after)),
        (None, None) => unreachable!("unchanged values have no summary"),
    };
    SettingChange { label, description }
}

fn parameter_changes(
    parameter: &PackageParameter,
    before: Option<&serde_json::Value>,
    after: Option<&serde_json::Value>,
    changes: &mut Preview,
) {
    let label = parameter.label.as_deref().unwrap_or(&parameter.key);
    if !parameter.kind.is_container() {
        changes.push_change(scalar_change(parameter, label.to_string(), before, after));
        return;
    }
    let old = before
        .and_then(serde_json::Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let new = after
        .and_then(serde_json::Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    if old.is_empty() && new.is_empty() {
        changes.push_change(scalar_change(parameter, label.to_string(), before, after));
        return;
    }
    // Match complete entries first so inserting a row does not make all following rows
    // appear changed. Use the same diff engine already present in the dependency graph.
    let mut input = imara_diff::InternedInput {
        before: Vec::new(),
        after: Vec::new(),
        interner: imara_diff::Interner::new(old.len() + new.len()),
    };
    input.update_before(old.iter());
    input.update_after(new.iter());
    let mut old_tokens = input.before.clone();
    let mut new_tokens = input.after.clone();
    old_tokens.sort_unstable_by_key(|token| token.0);
    new_tokens.sort_unstable_by_key(|token| token.0);
    if old_tokens == new_tokens {
        changes.push_change(SettingChange {
            label: label.to_string(),
            description: crate::i18n::t!("package-settings-change-reorder"),
        });
        return;
    }
    let mut diff = imara_diff::Diff::compute(imara_diff::Algorithm::Histogram, &input);
    diff.postprocess_no_heuristic(&input);
    for hunk in diff.hunks() {
        let removed = &old[hunk.before.start as usize..hunk.before.end as usize];
        let added = &new[hunk.after.start as usize..hunk.after.end as usize];
        for index in 0..removed.len().max(added.len()) {
            let before = removed.get(index);
            let after = added.get(index);
            let position = if after.is_some() {
                hunk.after.start
            } else {
                hunk.before.start
            } as usize
                + index
                + 1;
            let entry_label = crate::i18n::t!("package-settings-change-entry", "setting" => label, "number" => position.to_string());
            if parameter.kind == ParamKind::Table {
                match (before, after) {
                    (Some(before), Some(after)) => {
                        for field in parameter.fields.iter().filter(|field| !field.secret) {
                            let old_cell = before.get(&field.key);
                            let new_cell = after.get(&field.key);
                            if old_cell != new_cell {
                                let column = field.label.as_deref().unwrap_or(&field.key);
                                changes.push_change(scalar_change(
                                    field,
                                    format!("{entry_label} · {column}"),
                                    old_cell,
                                    new_cell,
                                ));
                            }
                        }
                    }
                    _ => {
                        let value = after.or(before).expect("changed row");
                        let details = parameter
                            .fields
                            .iter()
                            .filter(|field| !field.secret)
                            .filter_map(|field| {
                                value.get(&field.key).map(|value| {
                                    format!(
                                        "{}: {}",
                                        field.label.as_deref().unwrap_or(&field.key),
                                        display_value(field, value)
                                    )
                                })
                            })
                            .collect::<Vec<_>>()
                            .join(" · ");
                        let description = if after.is_some() {
                            crate::i18n::t!("package-settings-change-add-row", "details" => tooltip_text(&details))
                        } else {
                            crate::i18n::t!("package-settings-change-remove-row", "details" => tooltip_text(&details))
                        };
                        changes.push_change(SettingChange {
                            label: entry_label,
                            description,
                        });
                    }
                }
            } else if let Some(element) = parameter.fields.first() {
                changes.push_change(scalar_change(element, entry_label, before, after));
            }
        }
    }
}

fn preview(destination: &Destination, snapshot: &SettingsSnapshot) -> anyhow::Result<Preview> {
    anyhow::ensure!(snapshot.format == 1, "Unsupported settings format");
    anyhow::ensure!(
        snapshot.package == destination.package.specifier,
        "These settings belong to a different package"
    );
    anyhow::ensure!(
        shared_packages::parameter_revision(
            &destination.server,
            destination.scope(),
            &snapshot.package
        )? == destination.revision,
        "Settings changed. Reopen Settings before applying a configuration."
    );
    let current = destination.snapshot()?;
    preview_values(destination, snapshot, &current)
}

// Run before parsing or scheduling work. Check size first, even before trimming,
// so oversized clipboard text takes constant time to reject.
fn plausible_settings_clipboard(contents: Option<&str>) -> bool {
    contents.is_some_and(|text| {
        text.len() <= MAX_CLIPBOARD_BYTES && {
            let text = text.trim_ascii();
            text.starts_with('{') && text.ends_with('}')
        }
    })
}

// Serde skips the other fields without constructing their values. Borrow the
// package name when possible; escaped JSON strings remain supported.
#[derive(serde::Deserialize)]
struct ClipboardHeader<'a> {
    format: u32,
    #[serde(borrow)]
    package: std::borrow::Cow<'a, str>,
}

/// Shared by the menu check and Paste. No storage reads, diff generation, or retained
/// clipboard text are needed to decide whether this package can use the payload.
fn parse_clipboard(
    destination: &Destination,
    contents: Option<&str>,
) -> anyhow::Result<SettingsSnapshot> {
    let contents = contents.ok_or_else(|| anyhow::anyhow!("The clipboard is empty"))?;
    anyhow::ensure!(
        contents.len() <= MAX_CLIPBOARD_BYTES,
        "Clipboard settings exceed the 16 MiB limit"
    );
    anyhow::ensure!(
        plausible_settings_clipboard(Some(contents)),
        "The clipboard does not contain Smudgy settings"
    );
    let header: ClipboardHeader<'_> = serde_json::from_str(contents)
        .map_err(|_| anyhow::anyhow!("The clipboard does not contain Smudgy settings"))?;
    anyhow::ensure!(header.format == 1, "Unsupported settings format");
    anyhow::ensure!(
        header.package == destination.package.specifier,
        "These settings belong to a different package"
    );
    let snapshot: SettingsSnapshot = serde_json::from_str(contents)
        .map_err(|_| anyhow::anyhow!("The clipboard does not contain Smudgy settings"))?;
    anyhow::ensure!(
        snapshot
            .values
            .iter()
            .any(
                |(key, value)| compatible_parameter(destination, &snapshot, key, value.as_ref())
                    .is_some()
            ),
        "The clipboard has no settings this package can use"
    );
    Ok(snapshot)
}

fn compatible_parameter<'a>(
    destination: &'a Destination,
    snapshot: &SettingsSnapshot,
    key: &str,
    value: Option<&serde_json::Value>,
) -> Option<&'a PackageParameter> {
    let parameter = destination
        .params
        .iter()
        .find(|parameter| parameter.key == key && !parameter.secret)?;
    if snapshot
        .parameters
        .iter()
        .any(|p| p.key == key && (p.secret || p.kind != parameter.kind))
        || value.is_some_and(|value| {
            shared_packages::validate_package_param_value(parameter, value).is_err()
        })
    {
        None
    } else {
        Some(parameter)
    }
}

fn check_menu_clipboard(request: u64, destination: Destination) -> Task<Message> {
    // Chain the clipboard read directly into the worker so arbitrary clipboard contents
    // never become a window message (which is logged at trace level).
    iced::clipboard::read().then(move |contents| {
        let destination = destination.clone();
        if !plausible_settings_clipboard(contents.as_deref()) {
            return Task::done(message(SettingsMessage::MenuClipboardChecked(
                request,
                destination,
                false,
            )));
        }
        Task::perform(
            async move {
                let worker = destination.clone();
                let usable = tokio::task::spawn_blocking(move || {
                    parse_clipboard(&worker, contents.as_deref()).is_ok()
                })
                .await
                .unwrap_or(false);
                message(SettingsMessage::MenuClipboardChecked(
                    request,
                    destination,
                    usable,
                ))
            },
            std::convert::identity,
        )
    })
}

fn preview_values(
    destination: &Destination,
    snapshot: &SettingsSnapshot,
    current: &SettingsSnapshot,
) -> anyhow::Result<Preview> {
    anyhow::ensure!(snapshot.format == 1, "Unsupported settings format");
    anyhow::ensure!(
        snapshot.package == destination.package.specifier,
        "These settings belong to a different package"
    );
    let mut result = Preview {
        mutations: Vec::new(),
        changes: Vec::new(),
        remaining: 0,
        skipped: Vec::new(),
    };
    for (key, value) in &snapshot.values {
        let Some(parameter) = compatible_parameter(destination, snapshot, key, value.as_ref())
        else {
            result.skipped.push(key.clone());
            continue;
        };
        let old = current.values.get(key).and_then(Option::as_ref);
        if old == value.as_ref() {
            continue;
        }
        parameter_changes(parameter, old, value.as_ref(), &mut result);
        result.mutations.push(value.as_ref().map_or_else(
            || PackageParamMutation::ClearValue { key: key.clone() },
            |v| PackageParamMutation::SetValue {
                key: key.clone(),
                value: v.clone(),
            },
        ));
    }
    Ok(result)
}

/// A committed write must still notify sessions if refreshing the editor fails afterward.
fn commit_settings(
    destination: &Destination,
    mutations: &[PackageParamMutation],
) -> Result<ParamConfig, String> {
    match shared_packages::commit_package_params_at_revision(
        &destination.server,
        destination.scope(),
        &destination.package,
        destination.revision,
        mutations,
    )
    .map_err(|error| error.to_string())?
    {
        PackageParamCommit::StateChanged => {
            Err("Settings changed. Reopen Settings before applying a configuration.".into())
        }
        PackageParamCommit::Applied => {
            let mut config = ParamConfig::seed(
                &destination.server,
                &destination.profile,
                destination.package.clone(),
                destination.params.clone(),
            )
            .unwrap_or_else(|error| {
                ParamConfig::unavailable(
                    destination.package.specifier.clone(),
                    destination.package.parameter_scope,
                    &destination.profile,
                    destination.params.clone(),
                    format!("Settings saved, but the editor could not reload them: {error}"),
                )
            });
            config.saved = true;
            Ok(config)
        }
    }
}

impl AutomationsWindow {
    pub(super) fn save_settings_edits(
        &mut self,
        mutations: Vec<PackageParamMutation>,
    ) -> Update<Message, Event> {
        self.settings_request = self.settings_request.wrapping_add(1);
        let Some(destination) = self.settings_destination() else {
            return Update::none();
        };
        if let Some(config) = &mut self.param_config {
            config.saving = true;
        }
        Update::new(
            Task::perform(
                async move {
                    let worker = destination.clone();
                    let result =
                        tokio::task::spawn_blocking(move || commit_settings(&worker, &mutations))
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(std::convert::identity);
                    message(SettingsMessage::Finished(destination, result.map(Box::new)))
                },
                std::convert::identity,
            ),
            None,
        )
    }

    fn settings_destination(&self) -> Option<Destination> {
        let config = self.param_config.as_ref()?;
        let package = config.expected_package.clone()?;
        self.param_config_edit_available(config)
            .then(|| Destination {
                server: self.server_name.clone(),
                package,
                profile: config.profile_name.clone(),
                params: config.params.clone(),
                revision: config.revision,
                request: self.settings_request,
            })
    }

    fn settings_destination_matches(&self, destination: &Destination) -> bool {
        self.param_config.as_ref().is_some_and(|c| {
            self.settings_request == destination.request
                && self.server_name == destination.server
                && c.expected_package.as_ref() == Some(&destination.package)
                && c.profile_name == destination.profile
                && c.revision == destination.revision
                && c.params == destination.params
        })
    }

    fn open_settings_dialog(&mut self, destination: Destination, kind: DialogKind) {
        self.settings_dialog = Some(SettingsDialog {
            destination,
            kind,
            busy: false,
            error: None,
        });
    }

    pub(super) fn update_settings_history(
        &mut self,
        action: SettingsMessage,
    ) -> Update<Message, Event> {
        if let SettingsMessage::Menu(open) = action {
            if self.settings_menu_open == open {
                return Update::none();
            }
            self.settings_menu_request = self.settings_menu_request.wrapping_add(1);
            self.settings_paste_ready = None;
            self.settings_menu_open = open;
            return if open && let Some(destination) = self.settings_destination() {
                Update::with_task(check_menu_clipboard(
                    self.settings_menu_request,
                    destination,
                ))
            } else {
                Update::none()
            };
        }
        if let SettingsMessage::MenuClipboardChecked(request, destination, usable) = action {
            if request == self.settings_menu_request
                && self.settings_menu_open
                && self.settings_destination_matches(&destination)
            {
                self.settings_paste_ready = usable.then_some(destination);
            }
            return Update::none();
        }
        if matches!(action, SettingsMessage::Paste) && !self.settings_can_paste() {
            return Update::none();
        }
        self.settings_menu_open = false;
        self.settings_paste_ready = None;
        if matches!(action, SettingsMessage::Cancel)
            && self.settings_dialog.as_ref().is_some_and(|d| d.busy)
        {
            return Update::none();
        }
        if matches!(
            action,
            SettingsMessage::Copy
                | SettingsMessage::Paste
                | SettingsMessage::History
                | SettingsMessage::Reset
                | SettingsMessage::Forget
                | SettingsMessage::Cancel
        ) {
            self.settings_request = self.settings_request.wrapping_add(1);
        }
        match action {
            SettingsMessage::Menu(_) | SettingsMessage::MenuClipboardChecked(..) => unreachable!(),
            SettingsMessage::Cancel => {
                if self.settings_dialog.as_ref().is_none_or(|d| !d.busy) {
                    self.settings_dialog = None;
                }
            }
            SettingsMessage::Copy => {
                if let Some(destination) = self.settings_destination() {
                    return task(destination, |d| {
                        let contents = serde_json::to_string_pretty(&d.snapshot()?)?;
                        anyhow::ensure!(
                            contents.len() <= MAX_CLIPBOARD_BYTES,
                            "Clipboard settings exceed the 16 MiB limit"
                        );
                        Ok(Loaded::Copy(contents))
                    });
                }
            }
            SettingsMessage::Paste => {
                if let Some(destination) = self.settings_destination() {
                    return Update::with_task(iced::clipboard::read().then(move |contents| {
                        task(destination.clone(), move |d| {
                            let snapshot = parse_clipboard(d, contents.as_deref())?;
                            Ok(Loaded::Preview(
                                preview(d, &snapshot)?,
                                PreviewSource::Clipboard,
                            ))
                        })
                        .task
                    }));
                }
            }
            SettingsMessage::History => {
                if let Some(destination) = self.settings_destination() {
                    return task(destination, |d| {
                        shared_packages::with_local_package_transaction(&d.server, |_| {
                            anyhow::ensure!(
                                shared_packages::parameter_revision(
                                    &d.server,
                                    d.scope(),
                                    &d.package.specifier
                                )? == d.revision,
                                "Settings changed. Reopen Settings before viewing history."
                            );
                            let current = d.snapshot()?;
                            let entries = shared_packages::settings_history(
                                &d.server,
                                d.scope(),
                                &d.package.specifier,
                                &d.params,
                            )?;
                            let choices = entries
                                .into_iter()
                                .map(|entry| {
                                    let preview = preview_values(d, &entry.snapshot, &current)?;
                                    Ok(HistoryChoice {
                                        entry,
                                        summary: HistorySummary::from_preview(&preview),
                                    })
                                })
                                .collect::<anyhow::Result<Vec<_>>>()?;
                            Ok(Loaded::History(choices))
                        })
                    });
                }
            }
            SettingsMessage::Reset => {
                if let Some(destination) = self.settings_destination() {
                    self.open_settings_dialog(destination, DialogKind::Reset);
                }
            }
            SettingsMessage::Forget => {
                if let Some(destination) = self.settings_destination() {
                    self.open_settings_dialog(destination, DialogKind::Forget);
                }
            }
            SettingsMessage::Select(index) => {
                if let Some(dialog) = &self.settings_dialog
                    && let DialogKind::History(entries) = &dialog.kind
                    && let Some(entry) = entries.get(index)
                {
                    let snapshot = entry.entry.snapshot.clone();
                    let mut destination = dialog.destination.clone();
                    self.settings_request = self.settings_request.wrapping_add(1);
                    destination.request = self.settings_request;
                    return task(destination, move |d| {
                        Ok(Loaded::Preview(
                            preview(d, &snapshot)?,
                            PreviewSource::History,
                        ))
                    });
                }
            }
            SettingsMessage::Loaded(destination, result) => {
                if !self.settings_destination_matches(&destination) {
                    return Update::none();
                }
                match result {
                    Ok(Loaded::Copy(contents)) => {
                        return Update::new(
                            Task::batch([
                                iced::clipboard::write(contents),
                                self.show_toast(
                                    if destination.params.iter().any(|param| param.secret) {
                                        crate::i18n::t!("package-settings-copied-with-secrets")
                                    } else {
                                        crate::i18n::t!("package-settings-copied-clipboard")
                                    },
                                ),
                            ]),
                            None,
                        );
                    }
                    Ok(Loaded::History(entries)) => {
                        self.open_settings_dialog(destination, DialogKind::History(entries))
                    }
                    Ok(Loaded::Preview(preview, source)) => {
                        self.open_settings_dialog(destination, DialogKind::Preview(preview, source))
                    }
                    Ok(Loaded::Forgotten) => self.settings_dialog = None,
                    Err(error) => {
                        if let Some(dialog) = &mut self.settings_dialog {
                            dialog.error = Some(error);
                            dialog.busy = false;
                        } else if let Some(config) = &mut self.param_config {
                            config.error = Some(error);
                        }
                    }
                }
            }
            SettingsMessage::Apply => {
                let Some(dialog) = self.settings_dialog.clone().filter(|d| !d.busy) else {
                    return Update::none();
                };
                if !self.settings_destination_matches(&dialog.destination) {
                    self.settings_dialog = None;
                    return Update::none();
                }
                if let Some(current) = &mut self.settings_dialog {
                    current.busy = true;
                    current.error = None;
                }
                if matches!(dialog.kind, DialogKind::Forget) {
                    return task(dialog.destination, |d| {
                        shared_packages::clear_settings_history(
                            &d.server,
                            d.scope(),
                            &d.package.specifier,
                        )?;
                        Ok(Loaded::Forgotten)
                    });
                }
                let mutations = match dialog.kind {
                    DialogKind::Preview(preview, _) => preview.mutations,
                    DialogKind::Reset => dialog
                        .destination
                        .params
                        .iter()
                        .filter(|p| !p.secret)
                        .map(|p| PackageParamMutation::ClearValue { key: p.key.clone() })
                        .collect(),
                    _ => return Update::none(),
                };
                let destination = dialog.destination;
                return Update::new(
                    Task::perform(
                        async move {
                            let worker = destination.clone();
                            let result = tokio::task::spawn_blocking(move || {
                                commit_settings(&worker, &mutations)
                            })
                            .await
                            .map_err(|error| error.to_string())
                            .and_then(std::convert::identity);
                            message(SettingsMessage::Finished(destination, result.map(Box::new)))
                        },
                        std::convert::identity,
                    ),
                    None,
                );
            }
            SettingsMessage::Finished(destination, result) => {
                let matches = self.settings_destination_matches(&destination);
                match result {
                    Ok(config) => {
                        if matches {
                            self.param_config = Some(*config);
                            self.settings_dialog = None;
                        }
                        return Update::new(
                            self.show_toast(crate::i18n::t!("package-saved")),
                            Some(Event::ScriptsChanged {
                                server_name: destination.server,
                            }),
                        );
                    }
                    Err(error) if matches => {
                        if let Some(dialog) = &mut self.settings_dialog {
                            dialog.busy = false;
                            dialog.error = Some(error);
                        } else if let Some(config) = &mut self.param_config {
                            config.saving = false;
                            config.error = Some(error);
                        }
                    }
                    Err(_) => {}
                }
            }
        }
        Update::none()
    }

    fn settings_can_paste(&self) -> bool {
        self.settings_menu_open
            && self
                .settings_paste_ready
                .as_ref()
                .is_some_and(|destination| self.settings_destination_matches(destination))
            && self
                .param_config
                .as_ref()
                .is_some_and(|config| self.param_config_edit_available(config))
    }

    pub(super) fn view_settings_menu(&self) -> Elem<'_> {
        let trigger = button(text("⋯"))
            .style(button_style::secondary)
            .on_press_maybe(
                self.param_config
                    .as_ref()
                    .is_some_and(|c| !c.saving)
                    .then(|| message(SettingsMessage::Menu(!self.settings_menu_open))),
            );
        let mut entries = column![].spacing(2);
        for (label, action) in [
            (
                crate::i18n::t!("package-settings-copy"),
                SettingsMessage::Copy,
            ),
            (
                crate::i18n::t!("package-settings-paste"),
                SettingsMessage::Paste,
            ),
            (
                crate::i18n::t!("package-settings-history"),
                SettingsMessage::History,
            ),
            (
                crate::i18n::t!("package-settings-reset"),
                SettingsMessage::Reset,
            ),
        ] {
            let enabled = !matches!(action, SettingsMessage::Paste) || self.settings_can_paste();
            entries = entries.push(
                button(text(label).size(12))
                    .width(Length::Fill)
                    .padding([6, 10])
                    .style(button_style::link)
                    .on_press_maybe(enabled.then(|| message(action))),
            );
        }
        Dropdown::new(
            trigger,
            self.settings_menu_open.then(|| {
                container(entries)
                    .width(200)
                    .padding(6)
                    .style(common::card_style)
                    .into()
            }),
            message(SettingsMessage::Menu(false)),
        )
        .into()
    }

    pub(super) fn view_settings_dialog<'a>(&'a self, dialog: &'a SettingsDialog) -> Elem<'a> {
        let destination = &dialog.destination;
        let scope = match destination.package.parameter_scope {
            ParameterScope::Global => crate::i18n::t!("package-settings-all-profiles"),
            ParameterScope::Profile => destination.profile.clone(),
        };
        let (title, action) = match &dialog.kind {
            DialogKind::History(_) => (crate::i18n::t!("package-settings-history-title"), None),
            DialogKind::Preview(preview, source) => {
                let title = match source {
                    PreviewSource::Clipboard => crate::i18n::t!("package-settings-paste-title"),
                    PreviewSource::History => crate::i18n::t!("package-settings-restore-title"),
                };
                let action = (!preview.mutations.is_empty()).then(|| match source {
                    PreviewSource::Clipboard => crate::i18n::t!("package-settings-paste-action"),
                    PreviewSource::History => crate::i18n::t!("package-settings-restore-action"),
                });
                (title, action)
            }
            DialogKind::Reset => (
                crate::i18n::t!("package-settings-reset-title"),
                Some(crate::i18n::t!("action-reset")),
            ),
            DialogKind::Forget => (
                crate::i18n::t!("package-settings-forget-title"),
                Some(crate::i18n::t!("action-delete")),
            ),
        };
        let changes_settings =
            action.is_some() && matches!(dialog.kind, DialogKind::Preview(..) | DialogKind::Reset);
        let mut body = column![
            text(title).size(18),
            text(format!(
                "{} · {} · {}",
                super::model::package_display_name(&destination.package.specifier),
                destination.server,
                scope
            ))
            .size(12)
            .style(common::muted),
        ]
        .spacing(12);
        match &dialog.kind {
            DialogKind::History(entries) => {
                if entries.is_empty() {
                    body = body.push(
                        text(crate::i18n::t!("package-settings-history-empty"))
                            .size(13)
                            .style(common::muted),
                    );
                } else {
                    let mut items = column![].spacing(2);
                    for (index, choice) in entries.iter().enumerate() {
                        let entry = &choice.entry;
                        let mut metadata = Vec::new();
                        if let Some(version) = &entry.snapshot.version {
                            metadata.push(version.clone());
                        }
                        if entry.script {
                            metadata.push(crate::i18n::t!("package-settings-script-change"));
                        }
                        let content = row![
                            text(history_time(&entry.created))
                                .size(13)
                                .width(Length::Fill),
                            text(metadata.join(" · ")).size(11).style(common::muted),
                        ]
                        .spacing(16)
                        .align_y(iced::alignment::Vertical::Center);
                        items = items.push(
                            tooltip(
                                button(content)
                                    .width(Length::Fill)
                                    .padding([8, 10])
                                    .style(button_style::link)
                                    .on_press(message(SettingsMessage::Select(index))),
                                view_history_summary(&choice.summary),
                                tooltip::Position::Right,
                            )
                            .gap(10)
                            .delay(std::time::Duration::from_millis(350))
                            .snap_within_viewport(true),
                        );
                    }
                    body = body.push(scrollable(items).height(Length::Shrink));
                }
            }
            DialogKind::Preview(preview, _) => {
                let mut changes = column![].spacing(12);
                if preview.changes.is_empty() && preview.skipped.is_empty() {
                    changes = changes.push(
                        text(crate::i18n::t!("package-settings-no-changes"))
                            .size(13)
                            .style(common::muted),
                    );
                }
                for change in &preview.changes {
                    changes = changes.push(
                        column![
                            text(&change.label).size(13),
                            text(&change.description).size(12).style(common::muted),
                        ]
                        .spacing(2),
                    );
                }
                if preview.remaining > 0 {
                    changes = changes.push(text(crate::i18n::t!("package-settings-diff-more", "count" => preview.remaining.to_string())).size(12).style(common::muted));
                }
                if !preview.skipped.is_empty() {
                    let labels = preview
                        .skipped
                        .iter()
                        .take(8)
                        .map(|key| {
                            tooltip_text(
                                destination
                                    .params
                                    .iter()
                                    .find(|param| param.key == *key)
                                    .and_then(|param| param.label.as_deref())
                                    .unwrap_or(key),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    let labels = if preview.skipped.len() > 8 {
                        format!("{labels}…")
                    } else {
                        labels
                    };
                    changes = changes.push(tooltip(
                        text(crate::i18n::t!("package-settings-diff-skipped", "count" => preview.skipped.len().to_string())).size(12).style(common::muted),
                        container(text(labels).size(12)).padding(10).max_width(400).style(common::card_style),
                        tooltip::Position::Top,
                    ).delay(std::time::Duration::from_millis(350)).snap_within_viewport(true));
                }
                body = body.push(scrollable(changes).height(Length::Shrink));
            }
            DialogKind::Reset => {
                body = body.push(text(crate::i18n::t!("package-settings-reset-confirm")).size(13))
            }
            DialogKind::Forget => {
                body = body.push(text(crate::i18n::t!("package-settings-forget-confirm")).size(13))
            }
        }
        if changes_settings && destination.params.iter().any(|param| param.secret) {
            body = body.push(
                text(crate::i18n::t!("package-settings-no-secrets"))
                    .size(12)
                    .style(common::muted),
            );
        }
        if changes_settings
            && self
                .param_config
                .as_ref()
                .is_some_and(|config| !config.touched.is_empty())
        {
            body = body.push(
                text(crate::i18n::t!("package-settings-unsaved-warning"))
                    .size(12)
                    .style(common::muted),
            );
        }
        if let Some(error) = &dialog.error {
            body = body.push(text(error).style(common::danger));
        }
        let mut actions = row![].spacing(8).align_y(iced::alignment::Vertical::Center);
        if let DialogKind::History(entries) = &dialog.kind
            && !entries.is_empty()
        {
            actions = actions.push(
                button(button_style::underlined(
                    text(crate::i18n::t!("package-settings-forget")).size(12),
                ))
                .style(button_style::quiet_link)
                .on_press_maybe((!dialog.busy).then(|| message(SettingsMessage::Forget))),
            );
        }
        if matches!(dialog.kind, DialogKind::Preview(_, PreviewSource::History)) {
            actions = actions.push(
                button(text(crate::i18n::t!("action-back")))
                    .style(button_style::link)
                    .on_press_maybe((!dialog.busy).then(|| message(SettingsMessage::History))),
            );
        }
        actions = actions.push(iced::widget::space::horizontal());
        actions = actions.push(
            button(text(if action.is_some() {
                crate::i18n::t!("action-cancel")
            } else {
                crate::i18n::t!("action-close")
            }))
            .style(button_style::link)
            .on_press_maybe((!dialog.busy).then(|| message(SettingsMessage::Cancel))),
        );
        if let Some(action) = action {
            actions = actions.push(
                button(text(action))
                    .style(button_style::primary)
                    .on_press_maybe((!dialog.busy).then(|| message(SettingsMessage::Apply))),
            );
        }
        body = body.push(actions);
        let backdrop = mouse_area(
            container(iced::widget::space::vertical())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|theme: &crate::theme::Theme| container::Style {
                    background: Some(Background::Color(theme.styles.general.overlay_background)),
                    ..Default::default()
                }),
        )
        .on_press(message(SettingsMessage::Cancel));
        let card = container(body)
            .padding(20)
            .max_width(600)
            .max_height(600)
            .style(common::card_style);
        ModalLayer::new(
            iced::widget::stack![
                backdrop,
                container(opaque(card))
                    .center_x(Length::Fill)
                    .center_y(Length::Fill),
            ],
            message(SettingsMessage::Cancel),
        )
        .into()
    }
}

fn history_time(created: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(created, "%Y-%m-%d %H:%M:%S UTC")
        .map(|time| {
            time.and_utc()
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d  %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| created.to_string())
}
