//! Local browser package import. Compilation and persistence live in a
//! dedicated JavaScript worker; the UI only receives the installed name.

use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/package-installer.js")]
extern "C" {
    #[wasm_bindgen(catch, js_name = selectAndInstallPackage)]
    async fn select_and_install_js() -> Result<JsValue, JsValue>;
}

pub async fn select_and_install() -> Result<Option<String>, String> {
    let value = select_and_install_js().await.map_err(|error| {
        error
            .as_string()
            .unwrap_or_else(|| format!("Package import failed: {error:?}"))
    })?;
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_string()
        .map(Some)
        .ok_or_else(|| "Package installer returned an invalid package name".to_owned())
}
