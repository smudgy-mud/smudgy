//! Embedded TypeScript language-service engine for authoring-time intelligence.
//!
//! The engine runs the vendored TypeScript compiler inside Smudgy's existing Deno
//! runtime. Its compiler host is an immutable in-memory table: it has no filesystem,
//! network, environment, subprocess, or FFI access. A UI-facing worker owns one engine
//! and replaces its project snapshot whenever editable authoring state changes.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use deno_core::{FastString, serde_v8};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::language_service::{
    CompletionItem, CompletionItemId, CompletionKind, CompletionResult, DefinitionResult,
    DefinitionTarget, Diagnostic, DiagnosticCode, DiagnosticSeverity, DiagnosticsResult,
    DocumentId, FormattingOptions, FormattingResult, HoverResult, InsertTextFormat, Language,
    LanguageServiceLibrary, MAX_DEFINITION_TARGETS, MAX_DIAGNOSTICS_PER_DOCUMENT,
    MAX_FORMATTING_EDITS, MAX_FORMATTING_REPLACEMENT_BYTES, MAX_HOVER_BYTES,
    MAX_RESULT_METADATA_BYTES, MAX_SIGNATURE_HELP_DOCUMENTATION_BYTES,
    MAX_SIGNATURE_HELP_PARAMETERS, MarkupContent, MarkupKind, SignatureHelpParameter,
    SignatureHelpResult, TextEdit, Utf16Position, Utf16Range, validate_document_text,
};
use crate::{
    ModulePolicy, Permissions, PermissionsContainer, ScriptRuntime, ScriptRuntimeOptions,
    WorkerMode, permission_descriptor_parser,
};

const TYPESCRIPT_JS: &str = include_str!("../vendor/typescript/lib/typescript.js");
const DRIVER_JS: &str = include_str!("language_service_driver.js");

include!(concat!(env!("OUT_DIR"), "/dts_libs.rs"));

const SHIM_JS: &str = r#"
globalThis.module = { exports: {} };
globalThis.exports = globalThis.module.exports;
globalThis.process = { argv: [], env: {}, platform: "smudgy", cwd: () => "/", nextTick: (f) => f() };
globalThis.console = { log() {}, error() {}, warn() {}, info() {}, debug() {} };
"#;

const BIND_JS: &str = r#"
globalThis.ts = globalThis.module.exports;
if (!globalThis.ts || typeof globalThis.ts.createLanguageService !== "function") {
  throw new Error("failed to load embedded TypeScript LanguageService");
}
"#;

/// One source file in the authoring project snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFile {
    pub document_id: DocumentId,
    pub file_name: String,
    pub uri: String,
    pub language: Language,
    pub text: String,
}

impl ProjectFile {
    /// Builds a validated file with a normalized, absolute VFS name.
    pub fn new(
        document_id: DocumentId,
        file_name: impl Into<String>,
        uri: impl Into<String>,
        language: Language,
        text: impl Into<String>,
    ) -> Result<Self> {
        let file_name = normalize_file_name(&file_name.into())?;
        let uri = uri.into();
        if uri.is_empty() {
            bail!("language-service URI cannot be empty");
        }
        let text = text.into();
        validate_document_text(&text).context("validate language-service document")?;
        Ok(Self {
            document_id,
            file_name,
            uri,
            language,
            text,
        })
    }
}

/// Persistent authoring-time TypeScript service over host-owned snapshots.
pub struct EmbeddedLanguageService {
    runtime: ScriptRuntime,
    files: BTreeMap<DocumentId, ProjectFile>,
    ids_by_file_name: BTreeMap<String, DocumentId>,
    _data_dir: TemporaryDataDir,
}

impl EmbeddedLanguageService {
    /// Boots the embedded compiler and installs the vendored standard libraries.
    #[cfg(test)]
    pub fn new() -> Result<Self> {
        Self::new_with_libraries(Vec::new())
    }

    /// Boots the embedded compiler with additional immutable declaration libraries.
    pub fn new_with_libraries(extra_libraries: Vec<LanguageServiceLibrary>) -> Result<Self> {
        let data_dir = TemporaryDataDir::new()?;
        let tokio = Rc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("build language-service Tokio runtime")?,
        );
        let permissions = PermissionsContainer::new(
            permission_descriptor_parser(),
            Permissions::none_without_prompt(),
        );
        let mut runtime = ScriptRuntime::new(ScriptRuntimeOptions {
            extensions: Vec::new(),
            data_dir: data_dir.path().to_path_buf(),
            webstorage_dir: None,
            module_policy: ModulePolicy {
                allow_https: false,
                ..Default::default()
            },
            inspector: None,
            tokio,
            package_provider: None,
            permissions: Some(permissions),
            broadcast_channel: None,
            workers: WorkerMode::Disabled,
            max_live_workers_override: None,
        })
        .context("boot embedded language-service runtime")?;

        execute(&mut runtime, "[smudgy:lsp:shim]", SHIM_JS)?;
        execute(&mut runtime, "[smudgy:lsp:typescript]", TYPESCRIPT_JS)?;
        execute(&mut runtime, "[smudgy:lsp:bind]", BIND_JS)?;
        execute(&mut runtime, "[smudgy:lsp:driver]", DRIVER_JS)?;

        let mut service = Self {
            runtime,
            files: BTreeMap::new(),
            ids_by_file_name: BTreeMap::new(),
            _data_dir: data_dir,
        };
        let mut libraries = esnext_standard_libraries(LIBS)?;

        let mut library_roots = Vec::new();
        for library in extra_libraries {
            let file_name = normalize_file_name(&library.file_name)?;
            validate_document_text(&library.text)
                .with_context(|| format!("validate language-service library {file_name}"))?;
            if libraries.contains_key(&file_name) {
                bail!("duplicate language-service library {file_name}");
            }
            if library.is_root {
                library_roots.push(file_name.clone());
            }
            libraries.insert(file_name, library.text);
        }
        library_roots.sort();
        library_roots.dedup();
        let initialized: InitializeResult = service.call(
            "initialize",
            &json!({ "libraries": libraries, "libraryRoots": library_roots }),
        )?;
        if initialized.typescript_version.is_empty() {
            bail!("embedded TypeScript service reported no version");
        }
        Ok(service)
    }

    /// Atomically replaces the complete project snapshot.
    pub fn replace_project(&mut self, files: Vec<ProjectFile>) -> Result<()> {
        let mut next_files = BTreeMap::new();
        let mut next_ids = BTreeMap::new();
        let mut wire_files = BTreeMap::new();
        for mut file in files {
            file.file_name = normalize_file_name(&file.file_name)?;
            validate_document_text(&file.text)
                .with_context(|| format!("validate language-service file {}", file.file_name))?;
            if next_files.contains_key(&file.document_id) {
                bail!("duplicate language-service document {}", file.document_id);
            }
            if next_ids
                .insert(file.file_name.clone(), file.document_id)
                .is_some()
            {
                bail!("duplicate language-service VFS name {}", file.file_name);
            }
            wire_files.insert(file.file_name.clone(), file.text.clone());
            next_files.insert(file.document_id, file);
        }

        let _: ReplaceProjectResult =
            self.call("replaceProject", &json!({ "files": wire_files }))?;
        self.files = next_files;
        self.ids_by_file_name = next_ids;
        Ok(())
    }

    /// Returns TypeScript syntactic, semantic, and suggestion diagnostics.
    pub fn diagnostics(&mut self, document_id: DocumentId) -> Result<DiagnosticsResult> {
        let file_name = self.file(document_id)?.file_name.clone();
        let raw: RawDiagnostics = self.call("diagnostics", &json!({ "fileName": file_name }))?;
        let items = raw
            .diagnostics
            .into_iter()
            .take(MAX_DIAGNOSTICS_PER_DOCUMENT)
            .map(|diagnostic| Diagnostic {
                range: diagnostic.range.unwrap_or_default(),
                severity: diagnostic_severity(&diagnostic.category),
                code: Some(DiagnosticCode::Number(diagnostic.code)),
                source: Some("typescript".to_owned()),
                message: diagnostic.message,
                related_information: Vec::new(),
            })
            .collect();
        Ok(DiagnosticsResult { items })
    }

    /// Returns completion entries at an exact UTF-16 position.
    pub fn completion(
        &mut self,
        document_id: DocumentId,
        position: Utf16Position,
    ) -> Result<CompletionResult> {
        self.validate_position(document_id, position)?;
        let file_name = self.file(document_id)?.file_name.clone();
        let raw: RawCompletion = self.call(
            "completion",
            &json!({ "fileName": file_name, "position": position }),
        )?;
        let mut items = Vec::with_capacity(raw.entries.len());
        for (index, entry) in raw.entries.into_iter().enumerate() {
            let id = CompletionItemId::new(
                u64::try_from(index)
                    .ok()
                    .and_then(|value| value.checked_add(1))
                    .context("completion item count overflow")?,
            )
            .context("completion item identity overflow")?;
            let inserted = entry
                .insert_text
                .clone()
                .unwrap_or_else(|| entry.name.clone());
            let primary_edit = entry.replacement_span.map(|range| TextEdit {
                range,
                new_text: inserted.clone(),
            });
            items.push(CompletionItem {
                id,
                label: entry.name,
                detail: None,
                documentation: None,
                kind: completion_kind(&entry.kind),
                deprecated: entry.deprecated,
                filter_text: None,
                sort_text: entry.sort_text,
                insert_text: Some(inserted),
                insert_text_format: InsertTextFormat::PlainText,
                primary_edit,
                additional_edits: Vec::new(),
            });
        }
        Ok(CompletionResult {
            is_incomplete: raw.is_incomplete,
            items,
        })
    }

    /// Returns quick information at an exact UTF-16 position.
    pub fn hover(
        &mut self,
        document_id: DocumentId,
        position: Utf16Position,
    ) -> Result<Option<HoverResult>> {
        self.validate_position(document_id, position)?;
        let file_name = self.file(document_id)?.file_name.clone();
        let raw: RawHoverResponse = self.call(
            "hover",
            &json!({ "fileName": file_name, "position": position }),
        )?;
        Ok(raw.hover.map(|hover| {
            let mut value = String::new();
            if !hover.display.is_empty() {
                value.push_str("```ts\n");
                value.push_str(&hover.display);
                value.push_str("\n```");
            }
            if !hover.documentation.is_empty() {
                if !value.is_empty() {
                    value.push_str("\n\n");
                }
                value.push_str(&hover.documentation);
            }
            truncate_markdown(&mut value, MAX_HOVER_BYTES);
            HoverResult {
                range: hover.range,
                contents: MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                },
            }
        }))
    }

    /// Returns TypeScript's selected call signature at an exact UTF-16 position.
    pub fn signature_help(
        &mut self,
        document_id: DocumentId,
        position: Utf16Position,
    ) -> Result<Option<SignatureHelpResult>> {
        self.validate_position(document_id, position)?;
        let file_name = self.file(document_id)?.file_name.clone();
        let raw: RawSignatureHelpResponse = self.call(
            "signatureHelp",
            &json!({ "fileName": file_name, "position": position }),
        )?;
        let Some(help) = raw.signature_help else {
            return Ok(None);
        };

        help.applicable_range
            .to_byte_range(&self.file(document_id)?.text)
            .context("signature-help range is outside the requested document")?;
        if !signature_range_contains_request(help.applicable_range, position) {
            bail!("signature-help range does not contain the requested position");
        }
        if help.parameters.len() > MAX_SIGNATURE_HELP_PARAMETERS {
            bail!(
                "signature help returned {} parameters; maximum is {MAX_SIGNATURE_HELP_PARAMETERS}",
                help.parameters.len()
            );
        }
        if help.signature_count == 0 || help.selected_signature >= help.signature_count {
            bail!("signature help returned invalid overload metadata");
        }
        if help
            .active_parameter
            .is_some_and(|index| usize::from(index) >= help.parameters.len())
        {
            bail!("signature help returned an invalid active parameter");
        }

        let documentation_bytes = help
            .parameters
            .iter()
            .try_fold(help.documentation.len(), |total, parameter| {
                total.checked_add(parameter.documentation.len())
            })
            .context("signature-help documentation size overflow")?;
        if documentation_bytes > MAX_SIGNATURE_HELP_DOCUMENTATION_BYTES {
            bail!(
                "signature help returned {documentation_bytes} documentation bytes; maximum is \
                 {MAX_SIGNATURE_HELP_DOCUMENTATION_BYTES}"
            );
        }
        let metadata_bytes = help
            .parameters
            .iter()
            .try_fold(
                help.prefix
                    .len()
                    .checked_add(help.separator.len())
                    .and_then(|total| total.checked_add(help.suffix.len()))
                    .and_then(|total| total.checked_add(documentation_bytes))
                    .context("signature-help metadata size overflow")?,
                |total, parameter| total.checked_add(parameter.label.len()),
            )
            .context("signature-help metadata size overflow")?;
        if metadata_bytes > MAX_RESULT_METADATA_BYTES {
            bail!(
                "signature help returned {metadata_bytes} metadata bytes; maximum is \
                 {MAX_RESULT_METADATA_BYTES}"
            );
        }

        Ok(Some(SignatureHelpResult {
            applicable_range: help.applicable_range,
            prefix: help.prefix,
            separator: help.separator,
            suffix: help.suffix,
            parameters: help
                .parameters
                .into_iter()
                .map(|parameter| SignatureHelpParameter {
                    label: parameter.label,
                    documentation: markdown_content(parameter.documentation),
                    is_optional: parameter.is_optional,
                    is_rest: parameter.is_rest,
                })
                .collect(),
            active_parameter: help.active_parameter,
            selected_signature: help.selected_signature,
            signature_count: help.signature_count,
            argument_count: help.argument_count,
            documentation: markdown_content(help.documentation),
        }))
    }

    /// Resolves definition targets that are part of the current project snapshot.
    pub fn definition(
        &mut self,
        document_id: DocumentId,
        position: Utf16Position,
    ) -> Result<DefinitionResult> {
        self.validate_position(document_id, position)?;
        let file_name = self.file(document_id)?.file_name.clone();
        let raw: RawDefinitions = self.call(
            "definition",
            &json!({ "fileName": file_name, "position": position }),
        )?;
        let targets = raw
            .definitions
            .into_iter()
            .filter_map(|definition| {
                let target_id = self.ids_by_file_name.get(&definition.file_name).copied()?;
                let file = self.files.get(&target_id)?;
                Some(DefinitionTarget {
                    document_id: target_id,
                    target_range: definition.range,
                    target_selection_range: definition.range,
                    analyzed_uri: Some(file.uri.clone()),
                })
            })
            .take(MAX_DEFINITION_TARGETS)
            .collect();
        Ok(DefinitionResult { targets })
    }

    /// Returns whole-document TypeScript formatting edits.
    pub fn formatting(
        &mut self,
        document_id: DocumentId,
        options: FormattingOptions,
    ) -> Result<FormattingResult> {
        let file_name = self.file(document_id)?.file_name.clone();
        let raw: RawFormatting = self.call(
            "format",
            &json!({
                "fileName": file_name,
                "options": {
                    "tabSize": options.tab_size,
                    "insertSpaces": options.insert_spaces,
                }
            }),
        )?;
        // Formatting edits cannot be truncated: a partial edit list would
        // corrupt the document. An oversized result fails this one request.
        if raw.edits.len() > MAX_FORMATTING_EDITS {
            bail!(
                "formatting produced {} edits; the limit is {MAX_FORMATTING_EDITS}",
                raw.edits.len()
            );
        }
        let replacement_bytes: usize = raw.edits.iter().map(|edit| edit.new_text.len()).sum();
        if replacement_bytes > MAX_FORMATTING_REPLACEMENT_BYTES {
            bail!(
                "formatting produced {replacement_bytes} replacement bytes; the limit is {MAX_FORMATTING_REPLACEMENT_BYTES}"
            );
        }
        Ok(FormattingResult { edits: raw.edits })
    }

    fn file(&self, document_id: DocumentId) -> Result<&ProjectFile> {
        self.files
            .get(&document_id)
            .ok_or_else(|| anyhow!("unknown language-service document {document_id}"))
    }

    fn validate_position(&self, document_id: DocumentId, position: Utf16Position) -> Result<()> {
        position
            .to_byte_offset(&self.file(document_id)?.text)
            .context("validate language-service position")?;
        Ok(())
    }

    fn call<P, R>(&mut self, method: &str, params: &P) -> Result<R>
    where
        P: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let request = serde_json::to_string(&json!({
            "method": method,
            "params": params,
        }))
        .context("serialize language-service request")?;
        let source = format!(
            "globalThis.__SMUDGY_LANGUAGE_SERVICE_HANDLE({})",
            serde_json::to_string(&request)?
        );
        let value = self
            .runtime
            .deno_runtime()
            .execute_script("[smudgy:lsp:request]", FastString::from(source))
            .with_context(|| format!("execute language-service {method}"))?;
        let response: String = {
            deno_core::scope!(scope, self.runtime.deno_runtime());
            let local = deno_core::v8::Local::new(scope, value);
            serde_v8::from_v8(scope, local)
                .with_context(|| format!("read language-service {method} response"))?
        };
        let response: DriverResponse<R> = serde_json::from_str(&response)
            .with_context(|| format!("decode language-service {method} response"))?;
        if response.ok {
            response
                .result
                .ok_or_else(|| anyhow!("language-service {method} returned no result"))
        } else {
            bail!(
                "language-service {method} failed: {}",
                response.error.unwrap_or_else(|| "unknown error".to_owned())
            )
        }
    }
}

impl Drop for EmbeddedLanguageService {
    fn drop(&mut self) {
        let result: Result<DisposeResult> = self.call("dispose", &json!({}));
        let _ = result;
    }
}

fn execute(runtime: &mut ScriptRuntime, name: &'static str, source: &str) -> Result<()> {
    runtime
        .deno_runtime()
        .execute_script(name, FastString::from(source.to_owned()))
        .with_context(|| format!("execute {name}"))?;
    Ok(())
}

fn normalize_file_name(value: &str) -> Result<String> {
    let replaced = value.replace('\\', "/");
    let mut parts = Vec::new();
    for part in replaced.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    bail!("language-service VFS path escapes its root: {value}");
                }
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        bail!("language-service VFS path cannot be empty");
    }
    Ok(format!("/{}", parts.join("/")))
}

/// Builds the only TypeScript standard-library surface visible to authored code.
///
/// `LIBS` contains every declaration shipped with the compiler because other users of
/// that generated table need the full set. The authoring service deliberately exposes
/// only ESNext and its recursive `/// <reference lib>` dependencies: making DOM or
/// WebWorker files visible to `fileExists` would let an authored reference directive opt
/// into host capabilities which Smudgy did not grant.
fn esnext_standard_libraries(
    available: &'static [(&'static str, &'static str)],
) -> Result<BTreeMap<String, Cow<'static, str>>> {
    const ROOT_NAME: &str = "lib.esnext.d.ts";

    let mut by_name = BTreeMap::new();
    for (name, contents) in available {
        if by_name.insert(*name, *contents).is_some() {
            bail!("duplicate vendored TypeScript library {name}");
        }
    }
    let root = *by_name
        .get(ROOT_NAME)
        .context("vendored lib.esnext.d.ts is missing")?;
    let mut libraries = BTreeMap::from([
        ("/lib.d.ts".to_owned(), Cow::Borrowed(root)),
        (format!("/{ROOT_NAME}"), Cow::Borrowed(root)),
    ]);
    let mut pending =
        referenced_lib_names(root).context("parse references in vendored lib.esnext.d.ts")?;
    let mut visited = BTreeSet::new();

    while let Some(reference) = pending.pop() {
        if !visited.insert(reference.clone()) {
            continue;
        }
        if is_host_environment_lib(&reference) {
            bail!(
                "vendored ESNext library closure unexpectedly references host library {reference}"
            );
        }
        let name = format!("lib.{reference}.d.ts");
        let contents = *by_name
            .get(name.as_str())
            .with_context(|| format!("vendored ESNext dependency {name} is missing"))?;
        pending.extend(
            referenced_lib_names(contents)
                .with_context(|| format!("parse references in vendored {name}"))?,
        );
        libraries.insert(format!("/{name}"), Cow::Borrowed(contents));
    }

    Ok(libraries)
}

fn referenced_lib_names(source: &str) -> Result<Vec<String>> {
    let mut references = Vec::new();
    for (line_index, line) in source.lines().enumerate() {
        let Some(directive) = line.trim_start().strip_prefix("///") else {
            continue;
        };
        let directive = directive.trim_start();
        let Some(attributes) = directive.strip_prefix("<reference") else {
            continue;
        };
        if !attributes.chars().next().is_some_and(|character| {
            character.is_ascii_whitespace() || matches!(character, '/' | '>')
        }) {
            continue;
        }
        let Some(tag_end) = attributes.find('>') else {
            bail!(
                "unterminated TypeScript reference directive on line {}",
                line_index + 1
            );
        };
        if let Some(name) =
            reference_attribute(&attributes[..tag_end], "lib").with_context(|| {
                format!(
                    "parse TypeScript reference directive on line {}",
                    line_index + 1
                )
            })?
        {
            let name = name.trim().to_ascii_lowercase();
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            {
                bail!("invalid TypeScript lib reference {name:?}");
            }
            references.push(name);
        }
    }
    Ok(references)
}

fn reference_attribute(attributes: &str, requested: &str) -> Result<Option<String>> {
    let bytes = attributes.as_bytes();
    let mut cursor = 0;
    let mut value = None;
    while cursor < bytes.len() {
        while cursor < bytes.len() && (bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b'/')
        {
            cursor += 1;
        }
        if cursor == bytes.len() {
            break;
        }

        let name_start = cursor;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric() || matches!(bytes[cursor], b'-' | b'_'))
        {
            cursor += 1;
        }
        if cursor == name_start {
            bail!("invalid reference attribute name");
        }
        let name = &attributes[name_start..cursor];
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            bail!("reference attribute {name} has no value");
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let quote = *bytes
            .get(cursor)
            .filter(|quote| matches!(quote, b'\'' | b'"'))
            .with_context(|| format!("reference attribute {name} is not quoted"))?;
        cursor += 1;
        let value_start = cursor;
        while cursor < bytes.len() && bytes[cursor] != quote {
            cursor += 1;
        }
        if cursor == bytes.len() {
            bail!("reference attribute {name} is unterminated");
        }
        let attribute_value = &attributes[value_start..cursor];
        cursor += 1;

        if name.eq_ignore_ascii_case(requested)
            && value.replace(attribute_value.to_owned()).is_some()
        {
            bail!("duplicate reference attribute {requested}");
        }
    }
    Ok(value)
}

fn is_host_environment_lib(name: &str) -> bool {
    name == "dom"
        || name.starts_with("dom.")
        || name == "webworker"
        || name.starts_with("webworker.")
        || name == "scripthost"
}

fn diagnostic_severity(category: &str) -> DiagnosticSeverity {
    match category {
        "Error" => DiagnosticSeverity::Error,
        "Warning" => DiagnosticSeverity::Warning,
        "Suggestion" => DiagnosticSeverity::Hint,
        _ => DiagnosticSeverity::Information,
    }
}

fn completion_kind(kind: &str) -> CompletionKind {
    match kind {
        "method" => CompletionKind::Method,
        "function" | "local function" | "call" => CompletionKind::Function,
        "constructor" | "construct" => CompletionKind::Constructor,
        "property" | "getter" | "setter" | "accessor" | "index" | "JSX attribute" => {
            CompletionKind::Property
        }
        "class" | "local class" => CompletionKind::Class,
        "interface" => CompletionKind::Interface,
        "type" => CompletionKind::TypeAlias,
        "module" | "external module name" => CompletionKind::Module,
        "enum" => CompletionKind::Enum,
        "enum member" => CompletionKind::EnumMember,
        "const" => CompletionKind::Constant,
        "var" | "let" | "local var" | "parameter" | "using" | "await using" => {
            CompletionKind::Variable
        }
        "type parameter" => CompletionKind::TypeParameter,
        "keyword" | "primitive type" => CompletionKind::Keyword,
        "directory" => CompletionKind::Folder,
        "script" => CompletionKind::File,
        "alias" | "label" | "link" | "link name" | "link text" => CompletionKind::Reference,
        "string" => CompletionKind::Value,
        _ => CompletionKind::Text,
    }
}

/// Bounds a Markdown payload to `limit` bytes, cutting on a character
/// boundary and marking the cut.
fn truncate_markdown(value: &mut String, limit: usize) {
    const ELLIPSIS: &str = "\u{2026}";
    if value.len() <= limit {
        return;
    }
    let mut cut = limit.saturating_sub(ELLIPSIS.len());
    while cut > 0 && !value.is_char_boundary(cut) {
        cut -= 1;
    }
    value.truncate(cut);
    value.push_str(ELLIPSIS);
}

fn markdown_content(value: String) -> Option<MarkupContent> {
    (!value.is_empty()).then_some(MarkupContent {
        kind: MarkupKind::Markdown,
        value,
    })
}

fn signature_range_contains_request(range: Utf16Range, position: Utf16Position) -> bool {
    range.start <= position && position <= range.end
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriverResponse<T> {
    ok: bool,
    result: Option<T>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializeResult {
    typescript_version: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReplaceProjectResult {
    #[allow(dead_code)]
    project_version: u64,
    #[allow(dead_code)]
    roots: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DisposeResult {
    #[allow(dead_code)]
    disposed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDiagnostics {
    diagnostics: Vec<RawDiagnostic>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDiagnostic {
    #[allow(dead_code)]
    file_name: Option<String>,
    code: i64,
    category: String,
    message: String,
    range: Option<Utf16Range>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCompletion {
    is_incomplete: bool,
    entries: Vec<RawCompletionEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCompletionEntry {
    name: String,
    kind: String,
    sort_text: Option<String>,
    insert_text: Option<String>,
    replacement_span: Option<Utf16Range>,
    deprecated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHover {
    range: Option<Utf16Range>,
    display: String,
    documentation: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHoverResponse {
    hover: Option<RawHover>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSignatureHelpResponse {
    signature_help: Option<RawSignatureHelp>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSignatureHelp {
    applicable_range: Utf16Range,
    prefix: String,
    separator: String,
    suffix: String,
    parameters: Vec<RawSignatureHelpParameter>,
    active_parameter: Option<u16>,
    selected_signature: u16,
    signature_count: u16,
    argument_count: u16,
    documentation: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSignatureHelpParameter {
    label: String,
    documentation: String,
    is_optional: bool,
    is_rest: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDefinitions {
    definitions: Vec<RawDefinition>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDefinition {
    file_name: String,
    range: Utf16Range,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFormatting {
    edits: Vec<TextEdit>,
}

struct TemporaryDataDir(PathBuf);

impl TemporaryDataDir {
    fn new() -> Result<Self> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir();
        for _ in 0..100 {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!(
                "smudgy-language-service-{}-{id}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("create {}", path.display()));
                }
            }
        }
        bail!("could not allocate a unique language-service data directory")
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDataDir {
    fn drop(&mut self) {
        let Some(name) = self.0.file_name().and_then(|name| name.to_str()) else {
            return;
        };
        let expected_prefix = format!("smudgy-language-service-{}-", std::process::id());
        if !name.starts_with(&expected_prefix)
            || self.0.parent() != Some(std::env::temp_dir().as_path())
        {
            log::error!(
                "refusing to remove unexpected language-service directory {}",
                self.0.display()
            );
            return;
        }
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "failed to remove language-service directory {}: {error}",
                    self.0.display()
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_kind_maps_current_typescript_symbol_categories() {
        for (kind, expected) in [
            ("call", CompletionKind::Function),
            ("construct", CompletionKind::Constructor),
            ("accessor", CompletionKind::Property),
            ("index", CompletionKind::Property),
            ("local class", CompletionKind::Class),
            ("type", CompletionKind::TypeAlias),
            ("using", CompletionKind::Variable),
            ("await using", CompletionKind::Variable),
            ("label", CompletionKind::Reference),
            ("link name", CompletionKind::Reference),
            ("string", CompletionKind::Value),
        ] {
            assert_eq!(completion_kind(kind), expected, "TypeScript kind {kind}");
        }
    }

    #[test]
    fn signature_range_must_contain_the_exact_request_position() {
        let range = Utf16Range {
            start: Utf16Position {
                line: 2,
                character: 4,
            },
            end: Utf16Position {
                line: 2,
                character: 9,
            },
        };
        assert!(signature_range_contains_request(
            range,
            Utf16Position {
                line: 2,
                character: 4,
            }
        ));
        assert!(signature_range_contains_request(
            range,
            Utf16Position {
                line: 2,
                character: 9,
            }
        ));
        assert!(!signature_range_contains_request(
            range,
            Utf16Position {
                line: 1,
                character: 8,
            }
        ));
        assert!(!signature_range_contains_request(
            range,
            Utf16Position {
                line: 3,
                character: 0,
            }
        ));
    }

    fn document_id(byte: u8) -> DocumentId {
        DocumentId::try_from([byte; 16]).expect("non-nil test document ID")
    }

    fn file(byte: u8, name: &str, language: Language, text: &str) -> ProjectFile {
        ProjectFile::new(
            document_id(byte),
            name,
            format!("smudgy-test://{name}"),
            language,
            text,
        )
        .expect("valid project file")
    }

    fn inline_bridge(byte: u8) -> ProjectFile {
        file(
            byte,
            "/projects/1/1/inline/context.d.ts",
            Language::TypeScript,
            concat!(
                "export {};\n",
                "declare global {\n",
                "  interface SmudgyUserVars { [key: string]: any; }\n",
                "  const vars: SmudgyUserVars;\n",
                "}\n",
            ),
        )
    }

    #[test]
    fn inline_project_shares_global_and_inferred_vars_state_across_scripts() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        service
            .replace_project(vec![
                inline_bridge(1),
                file(
                    2,
                    "/projects/1/1/inline/aliases/setup.js",
                    Language::JavaScript,
                    concat!(
                        "class Target { name = \"goblin\"; }\n",
                        "globalThis.rounds = 1;\n",
                        "vars.target = \"goblin\";\n",
                        "vars.targetObject = new Target();\n",
                    ),
                ),
                file(
                    3,
                    "/projects/1/1/inline/hotkeys/use.js",
                    Language::JavaScript,
                    concat!(
                        "globalThis.rounds.toUpperCase();\n",
                        "vars.target.toFixed();\n",
                        "vars.targetObject.name.toFixed();\n",
                        "vars.ta\n",
                    ),
                ),
            ])
            .expect("open inline project");

        let diagnostics = service
            .diagnostics(document_id(3))
            .expect("inline diagnostics");
        assert!(diagnostics.items.iter().any(|item| {
            item.code == Some(DiagnosticCode::Number(2339)) && item.message.contains("toUpperCase")
        }));
        assert!(diagnostics.items.iter().any(|item| {
            item.code == Some(DiagnosticCode::Number(2551)) && item.message.contains("toFixed")
        }));
        assert!(
            diagnostics
                .items
                .iter()
                .filter(|item| item.message.contains("toFixed"))
                .count()
                >= 2,
            "a source-local class assigned to vars must be represented structurally"
        );
        let completion = service
            .completion(
                document_id(3),
                Utf16Position {
                    line: 3,
                    character: 7,
                },
            )
            .expect("inferred vars completion");
        assert!(
            completion.items.iter().any(|item| item.label == "target"),
            "an inferred vars property must complete in another inline script"
        );

        service
            .replace_project(vec![
                inline_bridge(1),
                file(
                    2,
                    "/projects/1/1/inline/aliases/setup.js",
                    Language::JavaScript,
                    "globalThis.rounds = 1; vars.target = 42;\n",
                ),
                file(
                    3,
                    "/projects/1/1/inline/hotkeys/use.js",
                    Language::JavaScript,
                    "vars.target.toUpperCase();\n",
                ),
            ])
            .expect("replace inferred vars state");
        let diagnostics = service
            .diagnostics(document_id(3))
            .expect("updated inline diagnostics");
        assert!(diagnostics.items.iter().any(|item| {
            item.code == Some(DiagnosticCode::Number(2339)) && item.message.contains("toUpperCase")
        }));
    }

    #[test]
    fn inline_project_rejects_module_syntax_and_top_level_await_only() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        service
            .replace_project(vec![
                inline_bridge(1),
                file(
                    2,
                    "/projects/1/1/inline/triggers/body.js",
                    Language::JavaScript,
                    concat!(
                        "import { echo } from \"smudgy:core\";\n",
                        "export const value = 1;\n",
                        "void import.meta.url;\n",
                        "void import(\"smudgy:core\");\n",
                        "await Promise.resolve();\n",
                        "async function allowed() { await Promise.resolve(); }\n",
                    ),
                ),
            ])
            .expect("open inline syntax project");
        let diagnostics = service
            .diagnostics(document_id(2))
            .expect("inline syntax diagnostics");
        assert_eq!(
            diagnostics
                .items
                .iter()
                .filter(|item| item.code == Some(DiagnosticCode::Number(97001)))
                .count(),
            3,
        );
        assert_eq!(
            diagnostics
                .items
                .iter()
                .filter(|item| item.code == Some(DiagnosticCode::Number(97002)))
                .count(),
            1,
        );

        service
            .replace_project(vec![file(
                3,
                "/projects/1/1/modules/body.js",
                Language::JavaScript,
                "export const value = await Promise.resolve(1);\n",
            )])
            .expect("open module project");
        let diagnostics = service
            .diagnostics(document_id(3))
            .expect("module diagnostics");
        assert!(diagnostics.items.iter().all(|item| {
            item.code != Some(DiagnosticCode::Number(97001))
                && item.code != Some(DiagnosticCode::Number(97002))
        }));
    }

    #[test]
    fn embedded_service_provides_project_aware_typescript_intelligence() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        service
            .replace_project(vec![
                file(
                    1,
                    "/dep.ts",
                    Language::TypeScript,
                    "export function greet(name: string): string { return `Hello ${name}`; }\n",
                ),
                file(
                    2,
                    "/main.ts",
                    Language::TypeScript,
                    concat!(
                        "import { greet } from \"./dep.ts\";\n",
                        "const rocket = \"🚀\";\n",
                        "const wrong: number = \"bad\";\n",
                        "greet(rocket);\n",
                    ),
                ),
            ])
            .expect("open project");

        let diagnostics = service
            .diagnostics(document_id(2))
            .expect("typescript diagnostics");
        assert!(diagnostics.items.iter().any(|item| {
            item.code == Some(DiagnosticCode::Number(2322))
                && item.severity == DiagnosticSeverity::Error
        }));

        let hover = service
            .hover(
                document_id(2),
                Utf16Position {
                    line: 3,
                    character: 1,
                },
            )
            .expect("hover request")
            .expect("hover result");
        assert!(hover.contents.value.contains("greet"));

        let definitions = service
            .definition(
                document_id(2),
                Utf16Position {
                    line: 3,
                    character: 1,
                },
            )
            .expect("definition request");
        assert_eq!(definitions.targets.len(), 1);
        assert_eq!(definitions.targets[0].document_id, document_id(1));

        service
            .replace_project(vec![
                file(
                    1,
                    "/dep.ts",
                    Language::TypeScript,
                    "export function greet(name: string): string { return `Hello ${name}`; }\n",
                ),
                file(
                    2,
                    "/main.ts",
                    Language::TypeScript,
                    concat!(
                        "interface User { name: string; age: number }\n",
                        "const user: User = { name: \"Ada\", age: 37 };\n",
                        "user.na\n",
                    ),
                ),
            ])
            .expect("replace project");
        let completion = service
            .completion(
                document_id(2),
                Utf16Position {
                    line: 2,
                    character: 7,
                },
            )
            .expect("completion request");
        let name = completion
            .items
            .iter()
            .find(|item| item.label == "name")
            .expect("name completion");
        assert_eq!(
            name.primary_edit.as_ref().map(|edit| edit.range),
            Some(Utf16Range {
                start: Utf16Position {
                    line: 2,
                    character: 5,
                },
                end: Utf16Position {
                    line: 2,
                    character: 7,
                },
            })
        );

        service
            .replace_project(vec![file(
                2,
                "/main.ts",
                Language::TypeScript,
                "const alpha = 1;\nal\n",
            )])
            .expect("replace completion-prefix project");
        let completion = service
            .completion(
                document_id(2),
                Utf16Position {
                    line: 1,
                    character: 2,
                },
            )
            .expect("prefix-filtered completion request");
        let alpha = completion
            .items
            .iter()
            .take(8)
            .find(|item| item.label == "alpha")
            .expect("typed prefix keeps alpha in the visible completion budget");
        assert_eq!(
            alpha.primary_edit.as_ref().map(|edit| edit.range),
            Some(Utf16Range {
                start: Utf16Position {
                    line: 1,
                    character: 0,
                },
                end: Utf16Position {
                    line: 1,
                    character: 2,
                },
            })
        );

        let formatting = service
            .formatting(
                document_id(2),
                FormattingOptions {
                    tab_size: 2,
                    insert_spaces: true,
                },
            )
            .expect("formatting request");
        assert!(!formatting.edits.is_empty());
    }

    #[test]
    fn hover_converts_jsdoc_inline_links_to_safe_markdown() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        service
            .replace_project(vec![file(
                1,
                "/links.ts",
                Language::TypeScript,
                concat!(
                    "/** Creates a {@link createEvent}, ",
                    "{@linkcode createEvent code event}, ",
                    "{@linkplain createEvent|plain event}, and ",
                    "{@link Missing unresolved event}, ",
                    "{@linkcode Missing|unresolved code}, ",
                    "{@linkplain Missing unresolved plain}, ",
                    "{@link https://external.invalid|external event}, ",
                    "{@linkcode https://external.invalid external code}, ",
                    "{@linkplain https://external.invalid|external plain}. ",
                    "{@link createEvent|unsafe ](https://example.invalid) *label*}.\n",
                    " * @returns See {@link createEvent}.\n",
                    " */\n",
                    "function createEvent(): void {}\n",
                    "createEvent;\n",
                ),
            )])
            .expect("open JSDoc link project");

        let hover = service
            .hover(
                document_id(1),
                Utf16Position {
                    line: 4,
                    character: 1,
                },
            )
            .expect("hover request")
            .expect("hover result");
        let markdown = hover.contents.value;
        assert!(markdown.contains("[createEvent](#smudgy-jsdoc-link-1)"));
        assert!(markdown.contains("[`code event`](#smudgy-jsdoc-link-2)"));
        assert!(markdown.contains("[plain event](#smudgy-jsdoc-link-3)"));
        assert!(markdown.contains("[unresolved event](#smudgy-jsdoc-link-4)"));
        assert!(markdown.contains("[`unresolved code`](#smudgy-jsdoc-link-5)"));
        assert!(markdown.contains("[unresolved plain](#smudgy-jsdoc-link-6)"));
        assert!(markdown.contains("[external event](#smudgy-jsdoc-link-7)"));
        assert!(markdown.contains("[`external code`](#smudgy-jsdoc-link-8)"));
        assert!(markdown.contains("[external plain](#smudgy-jsdoc-link-9)"));
        assert!(markdown.contains(
            "[unsafe \\]\\(https://example\\.invalid\\) \\*label\\*](#smudgy-jsdoc-link-10)"
        ));
        assert!(markdown.contains("[createEvent](#smudgy-jsdoc-link-11)"));
        assert!(!markdown.contains("external.invalid"));
        assert!(!markdown.contains("](https://example.invalid)"));
        assert!(!markdown.contains("{@link"));
    }

    #[test]
    fn hover_recovers_normalized_jsdoc_namepath_labels_without_live_destinations() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        service
            .replace_project(vec![file(
                1,
                "/namepaths.ts",
                Language::TypeScript,
                concat!(
                    "/** Links {@link import(\"pkg\").Type imported label}, ",
                    "{@linkcode import(\"pkg\").Type|imported code}, ",
                    "{@linkplain import(\"pkg\").Type}, ",
                    "{@link module:\"pkg\" module label}, ",
                    "{@linkcode module:\"pkg\"|module code}, and ",
                    "{@linkplain module:\"pkg\"}. */\n",
                    "function useNamepaths(): void {}\n",
                    "useNamepaths;\n",
                ),
            )])
            .expect("open normalized-namepath project");

        let hover = service
            .hover(
                document_id(1),
                Utf16Position {
                    line: 2,
                    character: 1,
                },
            )
            .expect("hover request")
            .expect("hover result");
        let markdown = hover.contents.value;
        assert!(markdown.contains("[imported label](#smudgy-jsdoc-link-1)"));
        assert!(markdown.contains("[`imported code`](#smudgy-jsdoc-link-2)"));
        assert!(markdown.contains("[import \\(\"pkg\"\\)\\.Type](#smudgy-jsdoc-link-3)"));
        assert!(markdown.contains("[module label](#smudgy-jsdoc-link-4)"));
        assert!(markdown.contains("[`module code`](#smudgy-jsdoc-link-5)"));
        assert!(markdown.contains("[module :\"pkg\"](#smudgy-jsdoc-link-6)"));
        assert_eq!(markdown.matches("#smudgy-jsdoc-link-").count(), 6);
        assert!(!markdown.contains("](import"));
        assert!(!markdown.contains("](module"));
    }

    #[test]
    fn hover_recovers_generic_and_colon_namepath_labels_without_live_destinations() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        service
            .replace_project(vec![file(
                1,
                "/generic-namepaths.ts",
                Language::TypeScript,
                concat!(
                    "/** Links {@link Missing<T | U> generic label}, ",
                    "{@linkcode Missing<T | U>|generic code}, ",
                    "{@linkplain Missing<T | U>}, ",
                    "{@link Missing<Map<string, Array<T | U>>> nested label}, ",
                    "{@link event:ready event label}, ",
                    "{@linkplain event:ready}, ",
                    "{@linkcode external:vendor|external code}, ",
                    "{@link external:vendor}, ",
                    "{@linkplain namespace:\"tool kit\" namespace label}, and ",
                    "{@link namespace:tools}. */\n",
                    "function useGenericNamepaths(): void {}\n",
                    "useGenericNamepaths;\n",
                ),
            )])
            .expect("open generic and colon-namepath project");

        let hover = service
            .hover(
                document_id(1),
                Utf16Position {
                    line: 2,
                    character: 1,
                },
            )
            .expect("hover request")
            .expect("hover result");
        let markdown = hover.contents.value;
        assert!(markdown.contains("[generic label](#smudgy-jsdoc-link-1)"));
        assert!(markdown.contains("[`generic code`](#smudgy-jsdoc-link-2)"));
        assert!(markdown.contains("[Missing\\<T \\| U\\>](#smudgy-jsdoc-link-3)"));
        assert!(markdown.contains("[nested label](#smudgy-jsdoc-link-4)"));
        assert!(markdown.contains("[event label](#smudgy-jsdoc-link-5)"));
        assert!(markdown.contains("[event :ready](#smudgy-jsdoc-link-6)"));
        assert!(markdown.contains("[`external code`](#smudgy-jsdoc-link-7)"));
        assert!(markdown.contains("[external :vendor](#smudgy-jsdoc-link-8)"));
        assert!(markdown.contains("[namespace label](#smudgy-jsdoc-link-9)"));
        assert!(markdown.contains("[namespace :tools](#smudgy-jsdoc-link-10)"));
        assert_eq!(markdown.matches("#smudgy-jsdoc-link-").count(), 10);
        for destination in ["](Missing", "](event", "](external", "](namespace"] {
            assert!(!markdown.contains(destination));
        }
    }

    #[test]
    fn hover_recovers_labels_when_typescript_resolves_only_the_link_prefix() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        service
            .replace_project(vec![file(
                1,
                "/resolved-prefixes.ts",
                Language::TypeScript,
                concat!(
                    "const event = 1;\n",
                    "const https = 1;\n",
                    "const marker = 1; const Foo = 1;\n",
                    "/** Links {@link event:ready pretty event}, ",
                    "{@linkcode event:ready|event code}, ",
                    "{@linkplain event:ready}, ",
                    "{@link https://external.invalid pretty URL}, ",
                    "{@linkcode https://external.invalid|URL code}, ",
                    "{@linkplain https://external.invalid}, ",
                    "{@link marker (deprecated)}, ",
                    "{@linkcode marker [legacy]}, and ",
                    "{@link Foo :note}. */\n",
                    "function useResolvedPrefixes(): void {}\n",
                    "useResolvedPrefixes;\n",
                ),
            )])
            .expect("open resolved-prefix project");

        let hover = service
            .hover(
                document_id(1),
                Utf16Position {
                    line: 5,
                    character: 1,
                },
            )
            .expect("hover request")
            .expect("hover result");
        let markdown = hover.contents.value;
        assert!(markdown.contains("[pretty event](#smudgy-jsdoc-link-1)"));
        assert!(markdown.contains("[`event code`](#smudgy-jsdoc-link-2)"));
        assert!(markdown.contains("[event:ready](#smudgy-jsdoc-link-3)"));
        assert!(markdown.contains("[pretty URL](#smudgy-jsdoc-link-4)"));
        assert!(markdown.contains("[`URL code`](#smudgy-jsdoc-link-5)"));
        assert!(markdown.contains("[https://external\\.invalid](#smudgy-jsdoc-link-6)"));
        assert!(markdown.contains("[\\(deprecated\\)](#smudgy-jsdoc-link-7)"));
        assert!(markdown.contains("[`[legacy]`](#smudgy-jsdoc-link-8)"));
        assert!(markdown.contains("[:note](#smudgy-jsdoc-link-9)"));
        assert_eq!(markdown.matches("#smudgy-jsdoc-link-").count(), 9);
        assert!(!markdown.contains("](event"));
        assert!(!markdown.contains("](https"));
        assert!(!markdown.contains("](http"));
    }

    #[test]
    fn hover_keeps_jsdoc_example_fences_block_parseable() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        service
            .replace_project(vec![file(
                1,
                "/example.ts",
                Language::TypeScript,
                concat!(
                    "/** Demonstrates the helper.\n",
                    " * @example\n",
                    " * ```ts\n",
                    " * const value: number = helper();\n",
                    " * ```\n",
                    " */\n",
                    "function helper(): number { return 1; }\n",
                    "helper;\n",
                ),
            )])
            .expect("open JSDoc example project");

        let hover = service
            .hover(
                document_id(1),
                Utf16Position {
                    line: 7,
                    character: 1,
                },
            )
            .expect("hover request")
            .expect("hover result");
        assert!(hover.contents.value.contains("`@example`\n\n```ts\n"));
        assert!(
            hover
                .contents
                .value
                .contains("const value: number = helper();")
        );
    }

    #[test]
    fn signature_help_preserves_selected_overload_and_parameter_metadata() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        service
            .replace_project(vec![file(
                1,
                "/signature.ts",
                Language::TypeScript,
                concat!(
                    "function choose(value: string): string;\n",
                    "/** Picks a number via {@link choose}.\n",
                    " * @param value Number to choose.\n",
                    " * @param radix Optional radix via {@link choose}.\n",
                    " */\n",
                    "function choose(value: number, radix?: number): number;\n",
                    "function choose(value: string | number, radix?: number) { return value; }\n",
                    "choose(1, \n",
                ),
            )])
            .expect("open signature-help project");

        let help = service
            .signature_help(
                document_id(1),
                Utf16Position {
                    line: 7,
                    character: 10,
                },
            )
            .expect("signature-help request")
            .expect("signature-help result");
        assert_eq!(help.prefix, "choose(");
        assert_eq!(help.separator, ", ");
        assert_eq!(help.suffix, "): number");
        assert_eq!(help.selected_signature, 1);
        assert_eq!(help.signature_count, 2);
        assert_eq!(help.argument_count, 2);
        assert_eq!(help.active_parameter, Some(1));
        assert_eq!(help.parameters.len(), 2);
        assert_eq!(help.parameters[0].label, "value: number");
        assert!(!help.parameters[0].is_optional);
        assert!(!help.parameters[0].is_rest);
        assert_eq!(help.parameters[1].label, "radix?: number");
        assert!(help.parameters[1].is_optional);
        assert!(!help.parameters[1].is_rest);
        assert!(help.documentation.as_ref().is_some_and(|documentation| {
            documentation
                .value
                .contains("[choose](#smudgy-jsdoc-link-1)")
        }));
        assert!(
            help.parameters[1]
                .documentation
                .as_ref()
                .is_some_and(|documentation| documentation
                    .value
                    .contains("[choose](#smudgy-jsdoc-link-2)"))
        );

        service
            .replace_project(vec![file(
                1,
                "/signature.ts",
                Language::TypeScript,
                "function send(...messages: string[]): void {}\nsend(\"one\", \n",
            )])
            .expect("replace with variadic signature");
        let variadic = service
            .signature_help(
                document_id(1),
                Utf16Position {
                    line: 1,
                    character: 12,
                },
            )
            .expect("variadic signature-help request")
            .expect("variadic signature-help result");
        assert_eq!(variadic.active_parameter, Some(0));
        assert_eq!(variadic.argument_count, 2);
        assert!(variadic.parameters[0].is_rest);
    }

    #[test]
    fn embedded_service_loads_rooted_immutable_declaration_libraries() {
        let mut service = EmbeddedLanguageService::new_with_libraries(vec![
            LanguageServiceLibrary {
                file_name: "/types/payload.d.ts".to_owned(),
                text: Cow::Borrowed("interface LibraryPayload { name: string; age: number }\n"),
                is_root: false,
            },
            LanguageServiceLibrary {
                file_name: "/types/smudgy-test.d.ts".to_owned(),
                text: Cow::Borrowed(
                    "/// <reference path=\"./payload.d.ts\" />\n\
                     declare module \"smudgy:test\" {\n\
                       export function make(): LibraryPayload;\n\
                     }\n",
                ),
                is_root: true,
            },
        ])
        .expect("boot language service with immutable declarations");
        service
            .replace_project(vec![file(
                1,
                "/main.ts",
                Language::TypeScript,
                "import { make } from \"smudgy:test\";\n\
                 const item = make();\n\
                 item.na\n\
                 const wrong: number = item.name;\n",
            )])
            .expect("open library consumer");

        let diagnostics = service
            .diagnostics(document_id(1))
            .expect("library-backed diagnostics");
        assert!(
            !diagnostics
                .items
                .iter()
                .any(|item| item.code == Some(DiagnosticCode::Number(2307))),
            "the ambient module root must resolve"
        );
        assert!(
            diagnostics
                .items
                .iter()
                .any(|item| item.code == Some(DiagnosticCode::Number(2322))),
            "the referenced non-root declaration must supply LibraryPayload"
        );

        let completion = service
            .completion(
                document_id(1),
                Utf16Position {
                    line: 2,
                    character: 7,
                },
            )
            .expect("library-backed completion");
        assert!(completion.items.iter().any(|item| item.label == "name"));
    }

    #[test]
    fn esnext_library_closure_parses_reference_format_variants_and_requires_dependencies() {
        const COMPLETE: &[(&str, &str)] = &[
            (
                "lib.esnext.d.ts",
                "///\t<reference other='ignored' LIB = 'ES2023'/>\n",
            ),
            (
                "lib.es2023.d.ts",
                "/// <reference no-default-lib=\"true\" />\n",
            ),
        ];
        let libraries = esnext_standard_libraries(COMPLETE).expect("resolve fixture closure");
        assert_eq!(
            libraries.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["/lib.d.ts", "/lib.es2023.d.ts", "/lib.esnext.d.ts"]
        );

        const MISSING: &[(&str, &str)] = &[(
            "lib.esnext.d.ts",
            "/// <reference lib=\"es2099.missing\" />\n",
        )];
        let error = esnext_standard_libraries(MISSING)
            .expect_err("a missing transitive standard library must fail initialization");
        assert!(error.to_string().contains("lib.es2099.missing.d.ts"));
    }

    #[test]
    fn esnext_library_closure_never_exposes_host_environment_libraries() {
        let libraries = esnext_standard_libraries(LIBS).expect("resolve vendored ESNext closure");
        assert!(libraries.contains_key("/lib.d.ts"));
        assert!(libraries.contains_key("/lib.esnext.d.ts"));
        assert!(libraries.contains_key("/lib.es5.d.ts"));
        for unavailable in [
            "/lib.dom.d.ts",
            "/lib.dom.iterable.d.ts",
            "/lib.esnext.full.d.ts",
            "/lib.scripthost.d.ts",
            "/lib.webworker.d.ts",
            "/lib.webworker.importscripts.d.ts",
        ] {
            assert!(
                !libraries.contains_key(unavailable),
                "host library {unavailable} must not be resolver-visible"
            );
        }
    }

    #[test]
    fn authored_dom_lib_reference_cannot_enable_web_audio() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        service
            .replace_project(vec![file(
                1,
                "/audio.ts",
                Language::TypeScript,
                "/// <reference lib=\"dom\" />\nconst context = new AudioContext();\n",
            )])
            .expect("open audio consumer");

        let diagnostics = service
            .diagnostics(document_id(1))
            .expect("audio diagnostics");
        assert!(diagnostics.items.iter().any(|item| {
            item.code == Some(DiagnosticCode::Number(2304)) && item.message.contains("AudioContext")
        }));
    }

    #[test]
    fn authored_webworker_lib_reference_cannot_widen_smudgys_worker_contract() {
        let mut service =
            EmbeddedLanguageService::new_with_libraries(vec![LanguageServiceLibrary {
                file_name: "/smudgy/narrow-worker.d.ts".to_owned(),
                text: Cow::Borrowed(
                    "interface Worker {}\n\
                     interface WorkerOptions { type: \"module\"; name?: string; }\n\
                     declare var Worker: {\n\
                       readonly prototype: Worker;\n\
                       new(specifier: string, options: WorkerOptions): Worker;\n\
                     };\n",
                ),
                is_root: true,
            }])
            .expect("boot language service with narrowed Worker declaration");
        service
            .replace_project(vec![file(
                1,
                "/worker.ts",
                Language::TypeScript,
                "/// <reference lib=\"webworker\" />\n\
                 new Worker(\"worker.ts\");\n\
                 new Worker(\"worker.ts\", { type: \"classic\" });\n",
            )])
            .expect("open worker consumer");

        let diagnostics = service
            .diagnostics(document_id(1))
            .expect("worker diagnostics");
        assert!(
            diagnostics
                .items
                .iter()
                .any(|item| item.code == Some(DiagnosticCode::Number(2554))),
            "the Smudgy Worker constructor must continue to require options"
        );
        assert!(
            diagnostics.items.iter().any(|item| {
                item.code == Some(DiagnosticCode::Number(2322)) && item.message.contains("classic")
            }),
            "the Smudgy Worker constructor must continue to require module workers"
        );
    }

    #[test]
    fn exact_materialized_relative_imports_cover_static_dynamic_and_module_extensions() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        service
            .replace_project(vec![
                file(
                    10,
                    "/projects/7/8/modules/static.mts",
                    Language::TypeScript,
                    "export const esmValue = 1;\n",
                ),
                file(
                    11,
                    "/projects/7/8/modules/static.cts",
                    Language::TypeScript,
                    "export const commonValue = 2;\n",
                ),
                file(
                    12,
                    "/projects/7/8/modules/types.d.mts",
                    Language::TypeScript,
                    "export interface EsmShape { value: number }\n",
                ),
                file(
                    13,
                    "/projects/7/8/modules/types.d.cts",
                    Language::TypeScript,
                    "export interface CommonShape { value: number }\n",
                ),
                file(
                    14,
                    "/projects/7/8/modules/dynamic.mjs",
                    Language::JavaScript,
                    "export const dynamicEsm = 3;\n",
                ),
                file(
                    15,
                    "/projects/7/8/modules/dynamic.cjs",
                    Language::JavaScript,
                    "export const dynamicCommon = 4;\n",
                ),
                file(
                    16,
                    "/projects/7/8/modules/main.ts",
                    Language::TypeScript,
                    concat!(
                        "import { esmValue } from \"./static.mts\";\n",
                        "import { commonValue } from \"./static.cts\";\n",
                        "import type { EsmShape } from \"./types.d.mts\";\n",
                        "import type { CommonShape } from \"./types.d.cts\";\n",
                        "void import(\"./dynamic.mjs\");\n",
                        "void import(\"./dynamic.cjs\");\n",
                        "const values: [number, number, EsmShape, CommonShape] = [\n",
                        "  esmValue, commonValue, { value: 1 }, { value: 2 },\n",
                        "];\n",
                    ),
                ),
            ])
            .expect("open exact-import project");

        let diagnostics = service
            .diagnostics(document_id(16))
            .expect("exact relative-import diagnostics");
        assert!(
            !diagnostics
                .items
                .iter()
                .any(|item| item.code == Some(DiagnosticCode::Number(2307))),
            "every explicitly materialized static and dynamic target must resolve"
        );
    }

    #[test]
    fn runtime_package_specifiers_do_not_report_false_missing_module_diagnostics() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        service
            .replace_project(vec![file(
                17,
                "/projects/7/8/modules/packages.ts",
                Language::TypeScript,
                concat!(
                    "import npmPackage from \"npm:example@1\";\n",
                    "import * as jsrPackage from \"jsr:@scope/example@1\";\n",
                    "import { packageValue } from \"smudgy://owner/example\";\n",
                    "void import(\"npm:dynamic-example@1\");\n",
                    "void import(\"jsr:@scope/dynamic-example@1\");\n",
                    "void import(\"smudgy://owner/dynamic-example\");\n",
                    "import short from \"@example\";\n",
                    "import explicit from \"smudgy:@example\";\n",
                    "export { value } from \"@example/util\";\n",
                    "void import(\"@dynamic-example\");\n",
                    "void short; void explicit;\n",
                    "void npmPackage; void jsrPackage; void packageValue;\n",
                    "void import(\"./actually-missing.ts\");\n",
                ),
            )])
            .expect("open runtime-package import project");

        let diagnostics = service
            .diagnostics(document_id(17))
            .expect("runtime-package diagnostics");
        let missing_modules = diagnostics
            .items
            .iter()
            .filter(|item| item.code == Some(DiagnosticCode::Number(2307)))
            .collect::<Vec<_>>();
        assert_eq!(
            missing_modules.len(),
            1,
            "runtime-resolved package schemes must not inherit TypeScript's false TS2307"
        );
        assert!(
            missing_modules[0].message.contains("./actually-missing.ts"),
            "real missing relative modules must remain actionable"
        );
    }

    #[test]
    fn extensionless_relative_misses_and_actionable_auto_imports_stay_hidden() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        service
            .replace_project(vec![
                file(
                    20,
                    "/projects/9/10/modules/dep.ts",
                    Language::TypeScript,
                    "export const exportedValue = 1;\n",
                ),
                file(
                    21,
                    "/projects/9/10/modules/hidden.ts",
                    Language::TypeScript,
                    "export const hiddenAutoImport = 2;\n",
                ),
                file(
                    22,
                    "/projects/9/10/modules/miss.ts",
                    Language::TypeScript,
                    "import { exportedValue } from \"./dep\";\nvoid exportedValue;\n",
                ),
                file(
                    23,
                    "/projects/9/10/modules/completion.ts",
                    Language::TypeScript,
                    concat!(
                        "import { exp } from \"./dep.ts\";\n",
                        "const local = { memberValue: 1 };\n",
                        "local.mem\n",
                        "hiddenAu\n",
                    ),
                ),
            ])
            .expect("open exact-resolution completion project");

        let diagnostics = service
            .diagnostics(document_id(22))
            .expect("extensionless relative-import diagnostics");
        assert!(
            diagnostics
                .items
                .iter()
                .any(|item| item.code == Some(DiagnosticCode::Number(2307))),
            "an extensionless specifier must not guess the materialized .ts target"
        );

        let explicit_import = service
            .completion(
                document_id(23),
                Utf16Position {
                    line: 0,
                    character: 12,
                },
            )
            .expect("explicit import-clause completion");
        assert!(
            explicit_import
                .items
                .iter()
                .any(|item| item.label == "exportedValue"),
            "explicit import-clause completions remain directly insertable"
        );

        let member = service
            .completion(
                document_id(23),
                Utf16Position {
                    line: 2,
                    character: 9,
                },
            )
            .expect("member completion");
        assert!(
            member.items.iter().any(|item| item.label == "memberValue"),
            "member completions remain available"
        );

        let auto_import = service
            .completion(
                document_id(23),
                Utf16Position {
                    line: 3,
                    character: 8,
                },
            )
            .expect("auto-import completion exclusion");
        assert!(
            !auto_import
                .items
                .iter()
                .any(|item| item.label == "hiddenAutoImport"),
            "entries that require an unrepresentable import action must stay hidden"
        );
    }

    #[test]
    fn embedded_service_uses_lsp_lines_and_utf16_columns() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        service
            .replace_project(vec![file(
                1,
                "/line-endings.ts",
                Language::TypeScript,
                concat!(
                    "const target = 1;\r",
                    "const wrong: number = \"😀\";\u{2028}",
                    "target;\u{2029}",
                ),
            )])
            .expect("open mixed-line-ending project");

        let diagnostics = service
            .diagnostics(document_id(1))
            .expect("mixed-line-ending diagnostics");
        let mismatch = diagnostics
            .items
            .iter()
            .find(|diagnostic| diagnostic.code == Some(DiagnosticCode::Number(2322)))
            .expect("type mismatch diagnostic");
        assert_eq!(mismatch.range.start.line, 1);

        let definitions = service
            .definition(
                document_id(1),
                Utf16Position {
                    line: 2,
                    character: 1,
                },
            )
            .expect("definition after Unicode line separator");
        assert_eq!(definitions.targets.len(), 1);
        assert_eq!(definitions.targets[0].document_id, document_id(1));
        assert_eq!(definitions.targets[0].target_range.start.line, 0);
    }

    #[test]
    fn truncated_completion_result_is_marked_incomplete() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        let mut text = String::new();
        for index in 0..650 {
            use std::fmt::Write as _;
            writeln!(text, "const item{index} = {index};").expect("write fixture source");
        }
        text.push_str("ite");
        service
            .replace_project(vec![file(
                1,
                "/completion-cap.ts",
                Language::TypeScript,
                &text,
            )])
            .expect("open completion project");

        let result = service
            .completion(
                document_id(1),
                Utf16Position {
                    line: 650,
                    character: 3,
                },
            )
            .expect("bounded completion result");
        assert_eq!(result.items.len(), 500);
        assert!(result.is_incomplete);
    }

    #[test]
    fn project_snapshot_rejects_duplicate_authority() {
        let mut service = EmbeddedLanguageService::new().expect("boot language service");
        let error = service
            .replace_project(vec![
                file(1, "/one.ts", Language::TypeScript, "export {};"),
                file(1, "/two.ts", Language::TypeScript, "export {};"),
            ])
            .expect_err("duplicate document ID must fail");
        assert!(
            error
                .to_string()
                .contains("duplicate language-service document")
        );
    }
}
