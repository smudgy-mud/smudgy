use deno_ast::{MediaType, ParseDiagnosticsError, ParseParams, SourceMapOption, SourceTextInfo};
use deno_core::ModuleSpecifier;
use deno_core::SourceMapData;

pub type ModuleContents = (String, Option<SourceMapData>);

fn should_transpile(media_type: MediaType) -> bool {
    matches!(
        media_type,
        MediaType::Jsx
            | MediaType::TypeScript
            | MediaType::Mts
            | MediaType::Cts
            | MediaType::Dts
            | MediaType::Dmts
            | MediaType::Dcts
            | MediaType::Tsx
    )
}

pub fn transpile(
    module_specifier: &ModuleSpecifier,
    code: &str,
) -> Result<ModuleContents, deno_ast::TranspileError> {
    let media_type = if module_specifier.as_str().starts_with("node:") {
        MediaType::TypeScript
    } else {
        MediaType::from_specifier(module_specifier)
    };

    // Interop name injection (interop.md §4) runs ahead of everything — including the
    // plain-JS early return below, since JS modules declare handles too. Same-line splices
    // only, so line numbers (and therefore stack traces) are unaffected.
    let injected = crate::interop_extract::inject_inferred_handle_names(module_specifier, code);
    let code = injected.as_deref().unwrap_or(code);

    if !should_transpile(media_type) {
        return Ok((code.to_string(), None));
    }

    let source = SourceTextInfo::from_string(code.to_string());
    let parsed = deno_ast::parse_module(ParseParams {
        specifier: module_specifier.clone(),
        text: source.text(),
        media_type,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })
    .map_err(|err| deno_ast::TranspileError::ParseErrors(ParseDiagnosticsError(vec![err])))?;

    let transpiled = parsed
        .transpile(
            // Automatic JSX runtime: `<Column/>` desugars to `jsx(Column, props)` with the
            // factories imported from `import_source/jsx-runtime` -- i.e. `smudgy:widgets`'s
            // synthesized jsx-runtime -- so no `React.createElement` and no `globalThis.React`
            // shim. `import_source` is a GLOBAL default applied to EVERY transpiled .jsx/.tsx;
            // a third-party file that wants a different host sets `/** @jsxImportSource X */`.
            &deno_ast::TranspileOptions {
                jsx: Some(deno_ast::JsxRuntime::Automatic(deno_ast::JsxAutomaticOptions {
                    import_source: Some("smudgy:widgets".to_string()),
                    development: false,
                })),
                ..Default::default()
            },
            &deno_ast::TranspileModuleOptions::default(),
            &deno_ast::EmitOptions {
                source_map: SourceMapOption::Separate,
                inline_sources: true,
                ..Default::default()
            },
        )?
        .into_source();

    Ok((
        transpiled.text,
        transpiled.source_map.map(|map| map.into_bytes().into()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emit(name: &str, code: &str) -> String {
        let spec = ModuleSpecifier::parse(&format!("file:///{name}")).expect("valid url");
        transpile(&spec, code).expect("transpiles").0
    }

    #[test]
    fn jsx_uses_the_automatic_smudgy_widgets_runtime() {
        let out = emit("hud.tsx", "const x = <Column spacing={2} />;");
        // Automatic runtime imports the factories from `smudgy:widgets/jsx-runtime` and calls
        // `jsx(...)` -- no `React.createElement`, no fake global React, no dev runtime.
        assert!(
            out.contains("smudgy:widgets/jsx-runtime"),
            "expected jsx-runtime import, got:\n{out}"
        );
        assert!(out.contains("jsx"), "expected a jsx() call, got:\n{out}");
        assert!(
            !out.contains("React.createElement"),
            "classic React pragma must be gone, got:\n{out}"
        );
        assert!(
            !out.contains("jsx-dev-runtime"),
            "development:false => no dev runtime, got:\n{out}"
        );
    }

    #[test]
    fn multi_child_jsx_imports_jsxs() {
        let out = emit(
            "hud.tsx",
            "const x = <Column><Text>a</Text><Text>b</Text></Column>;",
        );
        assert!(out.contains("jsxs"), "2+ children => jsxs import, got:\n{out}");
    }

    #[test]
    fn plain_typescript_is_unaffected_by_the_jsx_option() {
        let out = emit("mod.ts", "export const n: number = 1;");
        assert!(out.contains("export const n = 1"), "type stripped, got:\n{out}");
        assert!(
            !out.contains("jsx-runtime"),
            "no JSX => no runtime import, got:\n{out}"
        );
    }

    #[test]
    fn handle_names_are_injected_before_type_stripping() {
        let out = emit(
            "mod.ts",
            r#"
import { createState } from "smudgy:core";
export const vitals = createState<{ hp: number }>();
"#,
        );
        assert!(
            out.contains(r#"createState("vitals")"#),
            "inferred name survives transpile, got:\n{out}"
        );
    }

    #[test]
    fn handle_names_are_injected_in_plain_js_too() {
        let out = emit(
            "mod.js",
            r#"
import { createEvent } from "smudgy:core";
export const prompt = createEvent();
"#,
        );
        assert!(
            out.contains(r#"createEvent("prompt")"#),
            "JS skips transpile but not injection, got:\n{out}"
        );
    }
}
