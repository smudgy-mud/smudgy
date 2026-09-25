//! Browser persistence adapter. The shared catalog never imports browser APIs.

use smudgy_session_model::input_policy::CommandSyntax;
use smudgy_session_model::{naming, workspace::Workspace};
use smudgy_ui_shared::settings_appearance::Appearance;
use smudgy_ui_shared::settings_input::InputPreferences;
use smudgy_web_client::connect_manager::{Catalog, SecretChange, SecretString};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/src/storage.js")]
extern "C" {
    #[wasm_bindgen(catch, js_name = loadCatalog)]
    async fn load_catalog_js() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = saveCatalog)]
    async fn save_catalog_js(expected: &str, json: &str, secret: &str) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = loadProfilePassword)]
    async fn load_profile_password_js(server: &str, profile: &str) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = listLayouts)]
    async fn list_layouts_js(server: &str) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = layoutExists)]
    async fn layout_exists_js(server: &str, folded_name: &str) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = loadLayout)]
    async fn load_layout_js(server: &str, folded_name: &str) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = saveLayout)]
    async fn save_layout_js(
        server: &str,
        folded_name: &str,
        name: &str,
        json: &str,
        replace: bool,
    ) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = renameLayout)]
    async fn rename_layout_js(
        server: &str,
        from_folded: &str,
        to_folded: &str,
        to_name: &str,
        replace: bool,
    ) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = deleteLayout)]
    async fn delete_layout_js(server: &str, folded_name: &str) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = loadLastWindow)]
    async fn load_last_window_js() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = saveLastWindow)]
    async fn save_last_window_js(json: &str) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = loadSettings)]
    async fn load_settings_js() -> Result<JsValue, JsValue>;

    #[wasm_bindgen(catch, js_name = saveSettings)]
    async fn save_settings_js(json: &str) -> Result<JsValue, JsValue>;
}

#[derive(Debug, Clone)]
pub struct BrowserSettings {
    pub appearance: Appearance,
    pub input: InputPreferences,
    pub command_syntax: CommandSyntax,
    pub theme: String,
}

impl Default for BrowserSettings {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            input: InputPreferences::default(),
            command_syntax: CommandSyntax::default(),
            theme: "Smudgy".to_owned(),
        }
    }
}

pub async fn load_settings() -> Result<BrowserSettings, String> {
    let value = load_settings_js().await.map_err(|error| js_error(&error))?;
    if value.is_null() || value.is_undefined() {
        return Ok(BrowserSettings::default());
    }
    let json = value
        .as_string()
        .ok_or("IndexedDB returned non-string browser settings")?;
    let record: serde_json::Value = serde_json::from_str(&json)
        .map_err(|error| format!("invalid browser settings: {error}"))?;
    let schema = record.get("schema").and_then(serde_json::Value::as_u64);
    if !matches!(schema, Some(1..=4)) {
        return Err("unsupported browser settings schema".to_owned());
    }
    let appearance = serde_json::from_value::<Appearance>(
        record
            .get("appearance")
            .cloned()
            .ok_or("missing terminal appearance")?,
    )
    .map_err(|error| format!("invalid terminal appearance: {error}"))?
    .validate()
    .map_err(str::to_owned)?;
    let input = if schema == Some(1) {
        InputPreferences::default()
    } else {
        serde_json::from_value::<InputPreferences>(
            record
                .get("input")
                .cloned()
                .ok_or("missing browser input preferences")?,
        )
        .map_err(|error| format!("invalid browser input preferences: {error}"))?
    };
    let command_syntax = if matches!(schema, Some(3 | 4)) {
        serde_json::from_value::<CommandSyntax>(
            record
                .get("command_syntax")
                .cloned()
                .ok_or("missing browser command syntax")?,
        )
        .map_err(|error| format!("invalid browser command syntax: {error}"))?
    } else {
        CommandSyntax::default()
    };
    let theme = if schema == Some(4) {
        record
            .get("theme")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or("missing browser theme")?
            .to_owned()
    } else {
        "Smudgy".to_owned()
    };
    Ok(BrowserSettings {
        appearance,
        input: input.validate().map_err(str::to_owned)?,
        command_syntax: command_syntax.validate().map_err(str::to_owned)?,
        theme,
    })
}

pub async fn save_settings(settings: BrowserSettings) -> Result<(), String> {
    let appearance = settings.appearance.validate().map_err(str::to_owned)?;
    let input = settings.input.validate().map_err(str::to_owned)?;
    let command_syntax = settings.command_syntax.validate().map_err(str::to_owned)?;
    let theme = settings.theme.trim();
    if theme.is_empty() {
        return Err("browser theme is empty".to_owned());
    }
    let json = serde_json::json!({
        "schema": 4,
        "appearance": appearance,
        "input": input,
        "command_syntax": command_syntax,
        "theme": theme,
    })
    .to_string();
    save_settings_js(&json)
        .await
        .map_err(|error| js_error(&error))?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct LoadedWindow {
    pub workspace: Option<Workspace>,
    pub migrated: bool,
}

pub async fn load_last_window() -> Result<LoadedWindow, String> {
    let value = load_last_window_js()
        .await
        .map_err(|error| js_error(&error))?;
    if let Some(json) = value.as_string() {
        let workspace: Workspace = serde_json::from_str(&json)
            .map_err(|error| format!("invalid saved window: {error}"))?;
        return workspace
            .sanitized_single_window()
            .map(|workspace| LoadedWindow {
                workspace: Some(workspace),
                migrated: false,
            })
            .map_err(str::to_owned);
    }
    if !value.is_null() && !value.is_undefined() {
        return Err("IndexedDB returned a non-string window".to_owned());
    }
    let legacy = crate::window_restore::load_legacy();
    let migrated = legacy.is_some();
    Ok(LoadedWindow {
        workspace: legacy,
        migrated,
    })
}

pub async fn save_last_window(workspace: Workspace) -> Result<(), String> {
    let workspace = workspace.sanitized_single_window().map_err(str::to_owned)?;
    let json = serde_json::to_string(&workspace).map_err(|error| error.to_string())?;
    save_last_window_js(&json)
        .await
        .map_err(|error| js_error(&error))?;
    Ok(())
}

pub async fn load() -> Result<Catalog, String> {
    let value = load_catalog_js().await.map_err(|error| js_error(&error))?;
    let json = value
        .as_string()
        .ok_or("IndexedDB returned a non-string catalog")?;
    serde_json::from_str(&json).map_err(|error| format!("invalid saved catalog: {error}"))
}

pub async fn save(
    expected: u64,
    catalog: Catalog,
    secret: Option<SecretChange>,
) -> Result<Option<Catalog>, String> {
    let json = serde_json::to_string(&catalog).map_err(|error| error.to_string())?;
    let secret = serde_json::to_string(&secret).map_err(|error| error.to_string())?;
    let stored = save_catalog_js(&expected.to_string(), &json, &secret)
        .await
        .map_err(|error| js_error(&error))?;
    Ok(stored.as_bool().unwrap_or(false).then_some(catalog))
}

pub async fn load_password(server: &str, profile: &str) -> Result<Option<SecretString>, String> {
    let value = load_profile_password_js(server, profile)
        .await
        .map_err(|error| js_error(&error))?;
    if value.is_null() || value.is_undefined() {
        Ok(None)
    } else {
        value
            .as_string()
            .map(|value| Some(value.into()))
            .ok_or_else(|| "IndexedDB returned a non-string saved password".to_owned())
    }
}

fn layout_name(name: &str) -> Result<(String, String), String> {
    naming::validate_name(name)?;
    let name = name.trim().to_owned();
    let folded = name.to_lowercase();
    Ok((name, folded))
}

pub async fn list_layouts(server: &str) -> Result<Vec<String>, String> {
    let value = list_layouts_js(server)
        .await
        .map_err(|error| js_error(&error))?;
    let json = value
        .as_string()
        .ok_or("IndexedDB returned a non-string layout list")?;
    let mut names: Vec<String> =
        serde_json::from_str(&json).map_err(|error| format!("invalid layout list: {error}"))?;
    names.retain(|name| naming::validate_name(name).is_ok());
    names.sort_by_key(|name| name.trim().to_lowercase());
    Ok(names)
}

pub async fn layout_exists(server: &str, name: &str) -> Result<bool, String> {
    let (_, folded) = layout_name(name)?;
    layout_exists_js(server, &folded)
        .await
        .map_err(|error| js_error(&error))?
        .as_bool()
        .ok_or_else(|| "IndexedDB returned a non-boolean layout check".to_owned())
}

pub async fn load_layout(server: &str, name: &str) -> Result<Option<Workspace>, String> {
    let (_, folded) = layout_name(name)?;
    let value = load_layout_js(server, &folded)
        .await
        .map_err(|error| js_error(&error))?;
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    let json = value
        .as_string()
        .ok_or("IndexedDB returned a non-string layout")?;
    let workspace: Workspace =
        serde_json::from_str(&json).map_err(|error| format!("invalid saved layout: {error}"))?;
    workspace
        .sanitized_single_window()
        .map(Some)
        .map_err(str::to_owned)
}

pub async fn save_layout(
    server: &str,
    name: &str,
    workspace: Workspace,
    replace: bool,
) -> Result<(), String> {
    let (name, folded) = layout_name(name)?;
    let workspace = workspace.sanitized_single_window().map_err(str::to_owned)?;
    let mut json = serde_json::to_string_pretty(&workspace).map_err(|error| error.to_string())?;
    json.push('\n');
    save_layout_js(server, &folded, &name, &json, replace)
        .await
        .map_err(|error| js_error(&error))?;
    Ok(())
}

pub async fn rename_layout(
    server: &str,
    from: &str,
    to: &str,
    replace: bool,
) -> Result<Vec<String>, String> {
    let (_, from_folded) = layout_name(from)?;
    let (to_name, to_folded) = layout_name(to)?;
    rename_layout_js(server, &from_folded, &to_folded, &to_name, replace)
        .await
        .map_err(|error| js_error(&error))?;
    list_layouts(server).await
}

pub async fn delete_layout(server: &str, name: &str) -> Result<Vec<String>, String> {
    let (_, folded) = layout_name(name)?;
    delete_layout_js(server, &folded)
        .await
        .map_err(|error| js_error(&error))?;
    list_layouts(server).await
}

fn js_error(value: &JsValue) -> String {
    value
        .as_string()
        .unwrap_or_else(|| format!("IndexedDB error: {value:?}"))
}
