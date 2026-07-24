//! The rich manifest editor for an owned (local) package.
//!
//! A structured form over `smudgy.package.json`: the
//! file itself is hidden from the owned-package source browser, and this editor is the
//! way an author edits the manifest's `version`/`description`/`entry`, aligned hosts,
//! dependencies, declared parameters, and the requested permission set (the deno-native
//! `net`/`read`/`write`/`env`/`run`/`ffi`/`sys` allowlists plus the `smudgy` op-capability
//! flags).
//!
//! The on-disk manifest stays canonical: the form is seeded from the parsed
//! [`PackageManifest`] when the package opens, edited as a [`ManifestDraft`], and on Save
//! re-serialized back to `smudgy.package.json` (which is exactly what publish reads). A
//! draft that fails to project back to a valid manifest (an empty version, a param with no
//! key, a default that doesn't parse for its type) surfaces a message and is not written.

use std::collections::HashSet;

use iced::alignment::Vertical;
use iced::widget::{
    Column, button, checkbox, column, container, mouse_area, pick_list, radio, row, text,
    text_input,
};
use iced::{Length, Padding};

use smudgy_core::models::local_packages;
use smudgy_core::models::shared_packages::{
    ImportPolicy, PackageManifest, PackageParameter, PackagePermissions, ParamKind, ParamOption,
    SmudgyCapabilities, running_smudgy_release,
};

use crate::assets::{bootstrap_icons, fonts};
use crate::theme::builtins::button as button_style;
use crate::update::Update;

use super::common;
use super::{AutomationsWindow, Elem, Event, Message};

/// The editable form representation of a [`PackageManifest`]. Every scalar is held as a
/// `String` (the text-input buffer) and every list as a `Vec<String>` of row buffers, so
/// the form can hold transient/invalid intermediate states the manifest type can't; it is
/// projected back to a [`PackageManifest`] only on Save ([`Self::to_manifest`]).
#[derive(Debug, Clone)]
pub struct ManifestDraft {
    pub version: String,
    pub description: String,
    pub entry: String,
    /// The minimum smudgy version the package runs on (`min_smudgy_version`), or blank for
    /// no floor. Must parse as a semver version to save — the engine refuses to load a
    /// package whose floor it can't read (fail-closed), so a typo must not reach disk.
    pub min_smudgy_version: String,
    pub hosts: Vec<String>,
    pub dependencies: Vec<String>,
    /// Required packages (`smudgy://owner/name[@range]`): co-installed top-level roots consumed
    /// over the event bus + types, never imported. See `script/REQUIRED-PACKAGES.md`.
    pub requires: Vec<String>,
    pub params: Vec<ParamDraft>,
    pub net: Vec<String>,
    pub read: Vec<String>,
    pub write: Vec<String>,
    pub env: Vec<String>,
    /// Programs the package may spawn (`permissions.run`). A subprocess is not sandboxed —
    /// declaring any entry makes installers see the "effectively full access" warning.
    pub run: Vec<String>,
    /// Native libraries the package may load (`permissions.ffi`), same full-access weight as
    /// [`run`](Self::run).
    pub ffi: Vec<String>,
    /// System-info kinds the package may query (`permissions.sys`), e.g. `hostname`, `osRelease`.
    pub sys: Vec<String>,
    /// How far outside the smudgy ecosystem the package may download code to run
    /// (`permissions.import`): None / public registries (npm, jsr) / anywhere. A separate axis from
    /// `net`.
    pub import: ImportPolicy,
    /// The smudgy op-capability flags are reused verbatim — they're already plain booleans.
    pub caps: SmudgyCapabilities,
    /// Whether other-owner packages may `import` this package's modules (default `true`). When
    /// `false`, the loader rejects cross-owner imports — the events-only library / private-package
    /// switch.
    pub importable: bool,
    /// The most recent projection/save failure, shown above the form. Cleared on any edit.
    pub error: Option<String>,
}

impl Default for ManifestDraft {
    fn default() -> Self {
        Self {
            version: String::new(),
            description: String::new(),
            entry: String::new(),
            min_smudgy_version: String::new(),
            hosts: Vec::new(),
            dependencies: Vec::new(),
            requires: Vec::new(),
            params: Vec::new(),
            net: Vec::new(),
            read: Vec::new(),
            write: Vec::new(),
            env: Vec::new(),
            run: Vec::new(),
            ffi: Vec::new(),
            sys: Vec::new(),
            import: ImportPolicy::None,
            caps: SmudgyCapabilities::default(),
            // Packages are importable unless the author opts out — match the manifest default.
            importable: true,
            error: None,
        }
    }
}

/// One declared parameter, in editable form. `default` is kept as text and parsed to a
/// JSON value per [`kind`](Self::kind) on Save (so a transient half-typed number is allowed).
/// `options` apply to a `Dropdown`; `fields` describe a `List`'s element or a `Table`'s columns.
/// They persist across kind switches (so toggling away and back doesn't lose work) but only the
/// ones relevant to the current kind are projected to the manifest.
#[derive(Debug, Clone, Default)]
pub struct ParamDraft {
    pub key: String,
    pub label: String,
    pub kind: ParamKind,
    pub required: bool,
    pub secret: bool,
    /// For a `Dropdown`, the chosen default option value (or empty); otherwise the text default.
    pub default: String,
    /// The selectable choices for a `Dropdown`.
    pub options: Vec<OptionDraft>,
    /// The element spec of a `List` (one entry) or the columns of a `Table` (one per column).
    pub fields: Vec<SubParamDraft>,
}

/// One `Dropdown` choice, in editable form. A blank label falls back to the value when shown.
#[derive(Debug, Clone, Default)]
pub struct OptionDraft {
    pub value: String,
    pub label: String,
}

/// A scalar sub-parameter, in editable form: a `List`'s element or one `Table` column. Containers
/// never nest, so a sub-param's kind is always scalar (`String`/`Bool`/`Number`/`Dropdown`).
#[derive(Debug, Clone)]
pub struct SubParamDraft {
    /// The column key (the object key each row stores under). Required for a table column; ignored
    /// for a list element, whose values are stored bare.
    pub key: String,
    pub label: String,
    pub kind: ParamKind,
    /// The choices when this sub-param is a `Dropdown`.
    pub options: Vec<OptionDraft>,
}

impl Default for SubParamDraft {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            kind: ParamKind::String,
            options: Vec::new(),
        }
    }
}

/// The manifest string-lists, so one set of add/remove/set edits drives them all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListField {
    Host,
    Dependency,
    /// `requires` — co-installed top-level roots.
    Requires,
    /// `permissions.net`
    Net,
    /// `permissions.read`
    Read,
    /// `permissions.write`
    Write,
    /// `permissions.env`
    Env,
    /// `permissions.run` — programs the package may spawn (full-access weight).
    Run,
    /// `permissions.ffi` — native libraries the package may load (full-access weight).
    Ffi,
    /// `permissions.sys` — system-info kinds the package may query.
    Sys,
}

/// The `permissions.smudgy` op-capability flags, addressed individually by the toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    CreateAliases,
    CreateTriggers,
    Send,
    SendDirect,
    Echo,
    ReachOthers,
    ChangeDisplay,
    MapperRead,
    MapperWrite,
    Widgets,
    InteropRead,
    InteropWrite,
    Panes,
    GmcpSend,
    Input,
}

/// A single field-level edit to the open manifest draft. Folded into one [`Message`] variant
/// so the manifest form (many small inputs) doesn't balloon the window's message enum.
#[derive(Debug, Clone)]
pub enum ManifestEdit {
    Version(String),
    Description(String),
    Entry(String),
    /// Set `min_smudgy_version` — the minimum smudgy version the package runs on (blank =
    /// no floor).
    MinSmudgyVersion(String),
    /// Toggle `importable` (whether other-owner packages may import this one's modules).
    Importable(bool),
    /// Set `permissions.import` — how far outside the smudgy ecosystem this package may download
    /// code (none / public registries / anywhere).
    ImportPolicy(ImportPolicy),
    AddItem(ListField),
    /// Append a specific value to a list (vs [`AddItem`](Self::AddItem)'s blank row) — the
    /// dependency picker uses this to insert a correctly-owned `smudgy://owner/name` specifier.
    AddItemValue(ListField, String),
    RemoveItem(ListField, usize),
    SetItem(ListField, usize, String),
    AddParam,
    RemoveParam(usize),
    ParamKey(usize, String),
    ParamLabel(usize, String),
    ParamKind(usize, ParamKind),
    ParamRequired(usize, bool),
    ParamSecret(usize, bool),
    ParamDefault(usize, String),
    // Dropdown options on param `i`.
    ParamAddOption(usize),
    ParamRemoveOption(usize, usize),
    ParamOptionValue(usize, usize, String),
    ParamOptionLabel(usize, usize, String),
    // Sub-parameters (list element / table columns) on param `i`.
    ParamAddField(usize),
    ParamRemoveField(usize, usize),
    ParamFieldKey(usize, usize, String),
    ParamFieldLabel(usize, usize, String),
    ParamFieldKind(usize, usize, ParamKind),
    // Dropdown options on sub-parameter `j` of param `i`.
    ParamFieldAddOption(usize, usize),
    ParamFieldRemoveOption(usize, usize, usize),
    ParamFieldOptionValue(usize, usize, usize, String),
    ParamFieldOptionLabel(usize, usize, usize, String),
    Cap(Cap, bool),
}

/// The tabs the manifest editor groups its fields under. The package identity (version,
/// description, entry) stays pinned above the tabs; everything else lives under one tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ManifestTab {
    /// Hosts, dependencies, and declared parameters.
    #[default]
    Settings,
    /// The `permissions.smudgy` op-capability toggles.
    Capabilities,
    /// `permissions.net` — outbound hosts.
    Network,
    /// `permissions.read` + `permissions.write` — filesystem paths.
    Files,
    /// `permissions.env` + `permissions.sys` + `permissions.run` + `permissions.ffi` —
    /// environment variables, system info, subprocesses, and native libraries.
    System,
}

/// A `pick_list`-friendly wrapper over [`ParamKind`] (which lives in `smudgy_script` and has
/// no `Display`), mirroring `editors::FolderChoice`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KindChoice {
    String,
    Bool,
    Number,
    Dropdown,
    List,
    Table,
}

impl KindChoice {
    /// Every kind — the picker for a top-level parameter.
    const ALL: [KindChoice; 6] = [
        KindChoice::String,
        KindChoice::Bool,
        KindChoice::Number,
        KindChoice::Dropdown,
        KindChoice::List,
        KindChoice::Table,
    ];

    /// The scalar kinds — the picker for a sub-parameter (a list element / table column), since
    /// containers never nest.
    const SCALAR: [KindChoice; 4] = [
        KindChoice::String,
        KindChoice::Bool,
        KindChoice::Number,
        KindChoice::Dropdown,
    ];
}

impl From<ParamKind> for KindChoice {
    fn from(kind: ParamKind) -> Self {
        match kind {
            ParamKind::String => KindChoice::String,
            ParamKind::Bool => KindChoice::Bool,
            ParamKind::Number => KindChoice::Number,
            ParamKind::Dropdown => KindChoice::Dropdown,
            ParamKind::List => KindChoice::List,
            ParamKind::Table => KindChoice::Table,
        }
    }
}

impl From<KindChoice> for ParamKind {
    fn from(choice: KindChoice) -> Self {
        match choice {
            KindChoice::String => ParamKind::String,
            KindChoice::Bool => ParamKind::Bool,
            KindChoice::Number => ParamKind::Number,
            KindChoice::Dropdown => ParamKind::Dropdown,
            KindChoice::List => ParamKind::List,
            KindChoice::Table => ParamKind::Table,
        }
    }
}

impl std::fmt::Display for KindChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            KindChoice::String => "Text",
            KindChoice::Bool => "Boolean",
            KindChoice::Number => "Number",
            KindChoice::Dropdown => "Dropdown",
            KindChoice::List => "List",
            KindChoice::Table => "Table",
        })
    }
}

impl ManifestDraft {
    /// Seed an editable draft from a parsed manifest (on opening the owned package, and after
    /// a successful Save so the buffers re-mirror the canonical, re-serialized form).
    #[must_use]
    pub fn from_manifest(manifest: &PackageManifest) -> Self {
        Self {
            version: manifest.version.clone(),
            description: manifest.description.clone(),
            entry: manifest.entry.clone().unwrap_or_default(),
            min_smudgy_version: manifest.min_smudgy_version.clone().unwrap_or_default(),
            hosts: manifest.hosts.clone(),
            dependencies: manifest.dependencies.clone(),
            requires: manifest.requires.clone(),
            params: manifest.params.iter().map(ParamDraft::from_param).collect(),
            net: manifest.permissions.net.clone(),
            read: manifest.permissions.read.clone(),
            write: manifest.permissions.write.clone(),
            env: manifest.permissions.env.clone(),
            run: manifest.permissions.run.clone(),
            ffi: manifest.permissions.ffi.clone(),
            sys: manifest.permissions.sys.clone(),
            import: manifest.permissions.import,
            caps: manifest.permissions.smudgy,
            importable: manifest.importable,
            error: None,
        }
    }

    /// Project the draft back to a [`PackageManifest`], trimming entries and dropping blank
    /// rows. Returns a human-readable message (not a manifest) when the draft can't form a
    /// valid manifest yet — a missing version, a param with no key, or a default that doesn't
    /// parse for its declared type — so nothing is written until it does.
    fn to_manifest(&self) -> Result<PackageManifest, String> {
        let version = self.version.trim().to_string();
        if version.is_empty() {
            return Err("Version is required (e.g. 1.0.0).".to_string());
        }
        let entry = {
            let entry = self.entry.trim();
            (!entry.is_empty()).then(|| entry.to_string())
        };
        // Blank = no floor; anything else must be a plain semver version. Saving an
        // unreadable floor would brick the package for installers (the load-gate is
        // fail-closed), so this is a hard error, unlike the advisory `version` warning.
        let min_smudgy_version = {
            let min = self.min_smudgy_version.trim();
            if !min.is_empty() && semver::Version::parse(min).is_err() {
                return Err(format!(
                    "Requires-smudgy \u{201c}{min}\u{201d} isn't a version (e.g. 0.4.0)."
                ));
            }
            (!min.is_empty()).then(|| min.to_string())
        };

        let mut params = Vec::with_capacity(self.params.len());
        let mut seen_keys = HashSet::new();
        for (i, param) in self.params.iter().enumerate() {
            if param.key.trim().is_empty() {
                return Err(format!("Parameter #{} needs a key.", i + 1));
            }
            let projected = project_param(param).map_err(|reason| {
                format!("Parameter \u{201c}{}\u{201d}: {reason}", param.key.trim())
            })?;
            // Param values are keyed by `key` (case-insensitively), so duplicates would collide in
            // storage and the value editor — reject them, mirroring the table-column key check.
            if !seen_keys.insert(projected.key.to_lowercase()) {
                return Err(format!(
                    "Duplicate parameter key \u{201c}{}\u{201d}.",
                    projected.key
                ));
            }
            params.push(projected);
        }

        // `mapper: ["write"]` implies `read` (the manifest normalizes this at parse); keep the
        // projected manifest consistent so a write-only draft round-trips as read+write.
        let mut caps = self.caps;
        if caps.mapper_write {
            caps.mapper_read = true;
        }

        Ok(PackageManifest {
            version,
            description: self.description.trim().to_string(),
            entry,
            min_smudgy_version,
            dependencies: clean_list(&self.dependencies),
            requires: clean_list(&self.requires),
            hosts: clean_list(&self.hosts),
            params,
            permissions: PackagePermissions {
                net: clean_list(&self.net),
                read: clean_list(&self.read),
                write: clean_list(&self.write),
                env: clean_list(&self.env),
                run: clean_list(&self.run),
                ffi: clean_list(&self.ffi),
                sys: clean_list(&self.sys),
                import: self.import,
                smudgy: caps,
            },
            importable: self.importable,
        })
    }
}

impl ParamDraft {
    fn from_param(param: &PackageParameter) -> Self {
        let mut fields: Vec<SubParamDraft> =
            param.fields.iter().map(SubParamDraft::from_param).collect();
        // A container with no declared sub-params (only reachable from a hand-edited manifest) would
        // otherwise be uneditable — a list shows no element editor at all. Seed one so the editor can
        // always show and project a valid element/column.
        if param.kind.is_container() && fields.is_empty() {
            fields.push(SubParamDraft::default());
        }
        Self {
            key: param.key.clone(),
            label: param.label.clone().unwrap_or_default(),
            kind: param.kind,
            required: param.required,
            secret: param.secret,
            default: default_to_text(param.default.as_ref()),
            options: param.options.iter().map(OptionDraft::from_option).collect(),
            fields,
        }
    }
}

impl OptionDraft {
    fn from_option(option: &ParamOption) -> Self {
        Self {
            value: option.value.clone(),
            label: option.label.clone().unwrap_or_default(),
        }
    }
}

impl SubParamDraft {
    fn from_param(param: &PackageParameter) -> Self {
        Self {
            key: param.key.clone(),
            label: param.label.clone().unwrap_or_default(),
            kind: param.kind,
            options: param.options.iter().map(OptionDraft::from_option).collect(),
        }
    }
}

/// Trim a label buffer to an optional non-empty `String`.
fn trim_opt(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Project a parameter draft to a [`PackageParameter`], validating per kind. The caller has already
/// rejected an empty key. Only the kind-relevant sub-shape is projected: a `Dropdown`'s options, a
/// `List`'s element, a `Table`'s columns — leftovers from a prior kind are dropped.
fn project_param(draft: &ParamDraft) -> Result<PackageParameter, String> {
    let options = if draft.kind == ParamKind::Dropdown {
        let options = clean_options(&draft.options)?;
        if options.is_empty() {
            return Err("a dropdown needs at least one option.".to_string());
        }
        options
    } else {
        Vec::new()
    };
    let fields = project_fields(draft)?;
    let default = project_default(draft, &options)?;
    Ok(PackageParameter {
        key: draft.key.trim().to_string(),
        label: trim_opt(&draft.label),
        // Secrets are stored as keyring strings, so only a String param may be secret. Gating it
        // here (not just hiding the toggle) keeps a kind switch from leaving a non-String param
        // marked secret, which the value editor would render as a dead-end secret box.
        secret: draft.kind == ParamKind::String && draft.secret,
        required: draft.required,
        kind: draft.kind,
        default,
        options,
        fields,
    })
}

/// Project a `List`'s element spec or a `Table`'s columns from the draft's sub-parameters (empty
/// for a scalar param). Blank sub-rows are dropped; a table's column keys must be present and unique.
fn project_fields(draft: &ParamDraft) -> Result<Vec<PackageParameter>, String> {
    match draft.kind {
        ParamKind::List => {
            let element = draft
                .fields
                .first()
                .ok_or_else(|| "a list needs an element type.".to_string())?;
            // A list's values are stored bare (not keyed), so the element key is cosmetic — default
            // it rather than demanding one.
            let key = trim_opt(&element.key).unwrap_or_else(|| "value".to_string());
            Ok(vec![project_sub_param(element, key, "the list element")?])
        }
        ParamKind::Table => {
            let mut columns = Vec::new();
            let mut seen = HashSet::new();
            for field in &draft.fields {
                let Some(key) = trim_opt(&field.key) else {
                    // A column with no key is an unfinished row — drop it (matching the list editor's
                    // blank-row handling) rather than failing the save.
                    continue;
                };
                if !seen.insert(key.to_lowercase()) {
                    return Err(format!("duplicate column key \u{201c}{key}\u{201d}."));
                }
                columns.push(project_sub_param(
                    field,
                    key.clone(),
                    &format!("column \u{201c}{key}\u{201d}"),
                )?);
            }
            if columns.is_empty() {
                return Err("a table needs at least one column with a key.".to_string());
            }
            Ok(columns)
        }
        _ => Ok(Vec::new()),
    }
}

/// Project one scalar sub-parameter (a list element / table column) under the resolved `key`. `what`
/// names it for error messages.
fn project_sub_param(
    field: &SubParamDraft,
    key: String,
    what: &str,
) -> Result<PackageParameter, String> {
    if field.kind.is_container() {
        return Err(format!("{what} can't itself be a list or table."));
    }
    let options = if field.kind == ParamKind::Dropdown {
        let options = clean_options(&field.options)?;
        if options.is_empty() {
            return Err(format!(
                "{what} is a dropdown and needs at least one option."
            ));
        }
        options
    } else {
        Vec::new()
    };
    Ok(PackageParameter {
        key,
        label: trim_opt(&field.label),
        secret: false,
        required: false,
        kind: field.kind,
        default: None,
        options,
        fields: Vec::new(),
    })
}

/// Clean a dropdown's option drafts: trim, drop blank-value rows, reject duplicate values, and drop a
/// label that merely echoes its value.
fn clean_options(options: &[OptionDraft]) -> Result<Vec<ParamOption>, String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for option in options {
        let value = option.value.trim();
        if value.is_empty() {
            continue;
        }
        if !seen.insert(value.to_string()) {
            return Err(format!("duplicate option value \u{201c}{value}\u{201d}."));
        }
        let label = trim_opt(&option.label).filter(|label| label != value);
        out.push(ParamOption {
            value: value.to_string(),
            label,
        });
    }
    Ok(out)
}

/// The default value for a param draft, projected per kind: scalars via [`parse_default`], a dropdown
/// validated against its options, containers having none.
fn project_default(
    draft: &ParamDraft,
    options: &[ParamOption],
) -> Result<Option<serde_json::Value>, String> {
    match draft.kind {
        ParamKind::String | ParamKind::Bool | ParamKind::Number => {
            parse_default(draft.kind, &draft.default)
        }
        ParamKind::Dropdown => {
            let default = draft.default.trim();
            if default.is_empty() {
                Ok(None)
            } else if options.iter().any(|o| o.value == default) {
                Ok(Some(serde_json::Value::String(default.to_string())))
            } else {
                Err("the default must be one of the options.".to_string())
            }
        }
        ParamKind::List | ParamKind::Table => Ok(None),
    }
}

/// Trim each entry and drop the blanks — the form keeps empty rows while editing, but they
/// must not reach the serialized manifest (an empty `net`/`read` allowlist entry, etc.).
fn clean_list(items: &[String]) -> Vec<String> {
    items
        .iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// Render a stored param `default` JSON value back to its text-buffer form for editing.
fn default_to_text(value: Option<&serde_json::Value>) -> String {
    match value {
        None => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// Parse a param's `default` text buffer into the JSON value for its declared type. An empty
/// buffer is "no default" (`Ok(None)`); a non-empty buffer must parse for the type.
///
/// This **normalizes** the default to its declared `kind`: a `String` default is kept verbatim
/// (significant surrounding whitespace preserved), but `Bool`/`Number` are trimmed and parsed. A
/// stored default whose JSON type doesn't match `kind` — only reachable from a hand-edited or
/// forked manifest — is therefore coerced to the kind's type on save (the structured editor itself
/// always writes a default matching the kind). Integer parsing mirrors `serde_json`'s own
/// semantics (`i64`, then `u64`, then `f64`) so any integer it round-trips is preserved exactly.
fn parse_default(kind: ParamKind, text: &str) -> Result<Option<serde_json::Value>, String> {
    match kind {
        ParamKind::String => {
            // Keep String defaults verbatim — a default value may legitimately carry leading/
            // trailing whitespace (a prefix/separator), unlike hostnames or specifiers. Only a
            // truly-empty buffer (not a whitespace-only one) means "no default".
            if text.is_empty() {
                Ok(None)
            } else {
                Ok(Some(serde_json::Value::String(text.to_string())))
            }
        }
        ParamKind::Bool => match text.trim() {
            "" => Ok(None),
            "true" => Ok(Some(serde_json::Value::Bool(true))),
            "false" => Ok(Some(serde_json::Value::Bool(false))),
            _ => Err("default must be true or false.".to_string()),
        },
        ParamKind::Number => {
            let text = text.trim();
            if text.is_empty() {
                Ok(None)
            } else if let Ok(int) = text.parse::<i64>() {
                Ok(Some(serde_json::Value::Number(int.into())))
            } else if let Ok(uint) = text.parse::<u64>() {
                Ok(Some(serde_json::Value::Number(uint.into())))
            } else if let Some(num) = text
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
            {
                Ok(Some(serde_json::Value::Number(num)))
            } else {
                Err("default must be a number.".to_string())
            }
        }
        // Dropdown defaults are validated against the declared options in [`project_default`];
        // containers have no text default. Reached only defensively (this is called for scalars).
        ParamKind::Dropdown | ParamKind::List | ParamKind::Table => Ok(None),
    }
}

/// Resolve a [`ListField`] to its draft list.
fn list_mut(draft: &mut ManifestDraft, field: ListField) -> &mut Vec<String> {
    match field {
        ListField::Host => &mut draft.hosts,
        ListField::Dependency => &mut draft.dependencies,
        ListField::Requires => &mut draft.requires,
        ListField::Net => &mut draft.net,
        ListField::Read => &mut draft.read,
        ListField::Write => &mut draft.write,
        ListField::Env => &mut draft.env,
        ListField::Run => &mut draft.run,
        ListField::Ffi => &mut draft.ffi,
        ListField::Sys => &mut draft.sys,
    }
}

/// Apply one capability toggle. Turning on "change maps" (`mapper_write`) also turns on
/// "read maps" (`mapper_read`) — write implies read in the manifest model.
fn set_cap(caps: &mut SmudgyCapabilities, cap: Cap, on: bool) {
    match cap {
        Cap::CreateAliases => caps.create_aliases = on,
        Cap::CreateTriggers => caps.create_triggers = on,
        Cap::Send => caps.send = on,
        Cap::SendDirect => caps.send_direct = on,
        Cap::Echo => caps.echo = on,
        Cap::ReachOthers => caps.reach_others = on,
        Cap::ChangeDisplay => caps.change_display = on,
        Cap::MapperRead => caps.mapper_read = on,
        Cap::MapperWrite => {
            caps.mapper_write = on;
            if on {
                caps.mapper_read = true;
            }
        }
        Cap::Widgets => caps.widgets = on,
        Cap::InteropRead => caps.interop_read = on,
        Cap::InteropWrite => caps.interop_write = on,
        Cap::Panes => caps.panes = on,
        Cap::GmcpSend => caps.gmcp_send = on,
        Cap::Input => caps.input = on,
    }
}

// ============================================================================
// Update-side
// ============================================================================

impl AutomationsWindow {
    /// Apply a field-level edit to the open manifest draft and mark it unsaved. A no-op if no
    /// draft is open (the owned-package pane isn't showing).
    pub(super) fn apply_manifest_edit(&mut self, edit: ManifestEdit) -> Update<Message, Event> {
        let Some(draft) = self.manifest_draft.as_mut() else {
            return Update::none();
        };
        draft.error = None;
        match edit {
            ManifestEdit::Version(value) => draft.version = value,
            ManifestEdit::Description(value) => draft.description = value,
            ManifestEdit::Entry(value) => draft.entry = value,
            ManifestEdit::MinSmudgyVersion(value) => draft.min_smudgy_version = value,
            ManifestEdit::Importable(value) => draft.importable = value,
            ManifestEdit::ImportPolicy(value) => draft.import = value,
            ManifestEdit::AddItem(field) => list_mut(draft, field).push(String::new()),
            ManifestEdit::AddItemValue(field, value) => {
                let list = list_mut(draft, field);
                // Don't add a duplicate the picker already inserted (compare the bare
                // `smudgy://owner/name`, ignoring any `@range` an existing entry carries).
                let bare = value.split('@').next().unwrap_or(&value);
                if !list
                    .iter()
                    .any(|e| e.trim().split('@').next() == Some(bare))
                {
                    list.push(value);
                }
            }
            ManifestEdit::RemoveItem(field, i) => {
                let list = list_mut(draft, field);
                if i < list.len() {
                    list.remove(i);
                }
            }
            ManifestEdit::SetItem(field, i, value) => {
                if let Some(slot) = list_mut(draft, field).get_mut(i) {
                    *slot = value;
                }
            }
            ManifestEdit::AddParam => draft.params.push(ParamDraft::default()),
            ManifestEdit::RemoveParam(i) => {
                if i < draft.params.len() {
                    draft.params.remove(i);
                }
            }
            ManifestEdit::ParamKey(i, value) => {
                if let Some(param) = draft.params.get_mut(i) {
                    param.key = value;
                }
            }
            ManifestEdit::ParamLabel(i, value) => {
                if let Some(param) = draft.params.get_mut(i) {
                    param.label = value;
                }
            }
            ManifestEdit::ParamKind(i, kind) => {
                if let Some(param) = draft.params.get_mut(i) {
                    param.kind = kind;
                    // A container needs at least one sub-parameter to be meaningful; seed one the
                    // first time the author switches to List/Table so the sub-editor isn't empty.
                    if kind.is_container() && param.fields.is_empty() {
                        param.fields.push(SubParamDraft::default());
                    }
                    // Only a String may be secret (secrets are keyring strings); clear the flag when
                    // leaving String so it can't linger on a kind that has no secret box.
                    if kind != ParamKind::String {
                        param.secret = false;
                    }
                }
            }
            ManifestEdit::ParamRequired(i, on) => {
                if let Some(param) = draft.params.get_mut(i) {
                    param.required = on;
                }
            }
            ManifestEdit::ParamSecret(i, on) => {
                if let Some(param) = draft.params.get_mut(i) {
                    param.secret = on;
                }
            }
            ManifestEdit::ParamDefault(i, value) => {
                if let Some(param) = draft.params.get_mut(i) {
                    param.default = value;
                }
            }
            ManifestEdit::ParamAddOption(i) => {
                if let Some(param) = draft.params.get_mut(i) {
                    param.options.push(OptionDraft::default());
                }
            }
            ManifestEdit::ParamRemoveOption(i, j) => {
                if let Some(param) = draft.params.get_mut(i)
                    && j < param.options.len()
                {
                    param.options.remove(j);
                }
            }
            ManifestEdit::ParamOptionValue(i, j, value) => {
                if let Some(option) = draft.params.get_mut(i).and_then(|p| p.options.get_mut(j)) {
                    option.value = value;
                }
            }
            ManifestEdit::ParamOptionLabel(i, j, value) => {
                if let Some(option) = draft.params.get_mut(i).and_then(|p| p.options.get_mut(j)) {
                    option.label = value;
                }
            }
            ManifestEdit::ParamAddField(i) => {
                if let Some(param) = draft.params.get_mut(i) {
                    param.fields.push(SubParamDraft::default());
                }
            }
            ManifestEdit::ParamRemoveField(i, j) => {
                if let Some(param) = draft.params.get_mut(i)
                    && j < param.fields.len()
                {
                    param.fields.remove(j);
                }
            }
            ManifestEdit::ParamFieldKey(i, j, value) => {
                if let Some(field) = draft.params.get_mut(i).and_then(|p| p.fields.get_mut(j)) {
                    field.key = value;
                }
            }
            ManifestEdit::ParamFieldLabel(i, j, value) => {
                if let Some(field) = draft.params.get_mut(i).and_then(|p| p.fields.get_mut(j)) {
                    field.label = value;
                }
            }
            ManifestEdit::ParamFieldKind(i, j, kind) => {
                if let Some(field) = draft.params.get_mut(i).and_then(|p| p.fields.get_mut(j)) {
                    field.kind = kind;
                }
            }
            ManifestEdit::ParamFieldAddOption(i, j) => {
                if let Some(field) = draft.params.get_mut(i).and_then(|p| p.fields.get_mut(j)) {
                    field.options.push(OptionDraft::default());
                }
            }
            ManifestEdit::ParamFieldRemoveOption(i, j, k) => {
                if let Some(field) = draft.params.get_mut(i).and_then(|p| p.fields.get_mut(j))
                    && k < field.options.len()
                {
                    field.options.remove(k);
                }
            }
            ManifestEdit::ParamFieldOptionValue(i, j, k, value) => {
                if let Some(option) = draft
                    .params
                    .get_mut(i)
                    .and_then(|p| p.fields.get_mut(j))
                    .and_then(|f| f.options.get_mut(k))
                {
                    option.value = value;
                }
            }
            ManifestEdit::ParamFieldOptionLabel(i, j, k, value) => {
                if let Some(option) = draft
                    .params
                    .get_mut(i)
                    .and_then(|p| p.fields.get_mut(j))
                    .and_then(|f| f.options.get_mut(k))
                {
                    option.label = value;
                }
            }
            ManifestEdit::Cap(cap, on) => set_cap(&mut draft.caps, cap, on),
        }
        self.manifest_dirty = true;
        Update::none()
    }

    /// Project the draft back to a manifest and write it to `smudgy.package.json` (the form is
    /// canonical only on disk — publish reads the same file). On success the package is reloaded
    /// and the draft re-seeded from the freshly-parsed manifest; on a projection/serialize/write
    /// failure the reason is shown above the form and nothing is written.
    pub(super) fn save_manifest(&mut self) -> Update<Message, Event> {
        let Some(name) = self
            .local_package
            .as_ref()
            .map(|package| package.name.clone())
        else {
            return Update::none();
        };
        let manifest = match self.manifest_draft.as_ref().map(ManifestDraft::to_manifest) {
            Some(Ok(manifest)) => manifest,
            Some(Err(reason)) => {
                if let Some(draft) = self.manifest_draft.as_mut() {
                    draft.error = Some(reason);
                }
                return Update::none();
            }
            None => return Update::none(),
        };
        let json = match serde_json::to_string_pretty(&manifest) {
            Ok(json) => format!("{json}\n"),
            Err(e) => {
                if let Some(draft) = self.manifest_draft.as_mut() {
                    draft.error = Some(format!("Failed to serialize manifest: {e}"));
                }
                return Update::none();
            }
        };
        if let Err(e) =
            local_packages::write_local_file(&self.server_name, &name, "smudgy.package.json", &json)
        {
            if let Some(draft) = self.manifest_draft.as_mut() {
                draft.error = Some(format!("Save failed: {e}"));
            }
            return Update::none();
        }
        // Reload from disk so the pane (header version, dependency count) and the draft re-mirror
        // the canonical manifest; the dependency graph is rebuilt in case the deps changed.
        match local_packages::load_local_package(&self.server_name, &name) {
            Ok(Some(package)) => {
                self.manifest_draft = Some(ManifestDraft::from_manifest(&package.manifest));
                self.local_package = Some(Box::new(package));
            }
            // Re-reading right after a successful write should never fail; if it somehow does, keep
            // the in-memory package's manifest in step with what we just wrote so the header/meta
            // can't show a stale version while the draft (and disk) hold the new one.
            _ => {
                if let Some(pkg) = self.local_package.as_deref() {
                    self.local_package = Some(Box::new(local_packages::LocalPackage {
                        manifest: manifest.clone(),
                        ..pkg.clone()
                    }));
                }
                self.manifest_draft = Some(ManifestDraft::from_manifest(&manifest));
            }
        }
        // Re-seed the inline "Settings" editor: a manifest save can add, remove, or re-flag params,
        // so the value editor must track the new schema (preserving the on-disk values it pre-fills).
        if let Some(params) = self
            .local_package
            .as_deref()
            .map(|p| p.manifest.params.clone())
        {
            let spec = self.local_own_spec(&name);
            self.seed_param_config(spec, params);
        }
        self.manifest_dirty = false;
        // A successful save returns to the read-only summary (now showing the saved manifest).
        self.manifest_editing = false;
        self.rebuild_graph();
        Update::with_task(self.show_toast("Saved manifest."))
    }

    /// Enter the structured editor from the read-only summary, (re)seeding the draft from the
    /// current on-disk manifest so the form starts from exactly what the summary showed.
    pub(super) fn begin_manifest_edit(&mut self) -> Update<Message, Event> {
        if let Some(package) = self.local_package.as_deref() {
            self.manifest_draft = Some(ManifestDraft::from_manifest(&package.manifest));
        }
        self.manifest_dirty = false;
        self.manifest_editing = true;
        Update::none()
    }

    /// "Cancel": discard unsaved manifest edits, re-seed the draft from the on-disk manifest, and
    /// return to the read-only summary.
    pub(super) fn revert_manifest(&mut self) -> Update<Message, Event> {
        if let Some(package) = self.local_package.as_deref() {
            self.manifest_draft = Some(ManifestDraft::from_manifest(&package.manifest));
        }
        self.manifest_dirty = false;
        self.manifest_editing = false;
        Update::none()
    }
}

// ============================================================================
// View-side
// ============================================================================

/// Label column width for the single-line manifest fields (wider than the script editors'
/// to fit "Description").
const LABEL_WIDTH: f32 = 104.0;

impl AutomationsWindow {
    /// The manifest section embedded in the owned-package pane (in place of the hidden
    /// `smudgy.package.json` file). Defaults to a read-only summary of the current manifest with an
    /// "Edit" button that drops into the structured tabbed editor; in edit mode, Save/Cancel return
    /// to the summary. Renders nothing off-pane.
    pub(super) fn view_manifest_section(&self) -> Elem<'_> {
        let Some(package) = self.local_package.as_deref() else {
            return iced::widget::space::vertical()
                .height(Length::Fixed(0.0))
                .into();
        };
        let body: Elem<'_> = if self.manifest_editing {
            match self.manifest_draft.as_ref() {
                Some(draft) => self.manifest_editor_body(draft),
                None => {
                    return iced::widget::space::vertical()
                        .height(Length::Fixed(0.0))
                        .into();
                }
            }
        } else {
            manifest_readonly_body(&package.manifest)
        };
        container(body)
            .padding(16.0)
            .width(Length::Fill)
            .style(common::card_style)
            .into()
    }

    /// The editing body: the identity fields pinned above the tab bar, the active tab, and the
    /// edit action bar.
    fn manifest_editor_body<'a>(&'a self, draft: &'a ManifestDraft) -> Elem<'a> {
        let mut body = Column::new()
            .spacing(14.0)
            .push(common::section_label("Edit manifest"))
            .push(
                text("A structured editor for smudgy.package.json — what you publish.")
                    .size(12.0)
                    .style(common::muted),
            );

        if let Some(error) = &draft.error {
            body = body.push(manifest_error(error));
        }

        // Identity / entry (pinned above the tabs).
        body = body.push(field_row(
            "Version",
            text_input("e.g. 1.0.0", &draft.version)
                .on_input(|v| Message::EditManifest(ManifestEdit::Version(v)))
                .size(14.0)
                .into(),
        ));
        if !draft.version.trim().is_empty() && semver::Version::parse(draft.version.trim()).is_err()
        {
            body = body.push(
                text("Not a valid semver version yet — required before you can publish (e.g. 1.2.3).")
                    .size(11.0)
                    .style(common::warning),
            );
        }
        body = body.push(field_row(
            "Description",
            text_input(
                "What this package does (shown in Discover)",
                &draft.description,
            )
            .on_input(|v| Message::EditManifest(ManifestEdit::Description(v)))
            .size(14.0)
            .into(),
        ));
        body = body.push(field_row(
            "Entry",
            text_input("index.ts", &draft.entry)
                .on_input(|v| Message::EditManifest(ManifestEdit::Entry(v)))
                .size(14.0)
                .into(),
        ));

        // Tabs group the rest of the manifest; the package identity above stays pinned across them.
        body = body.push(manifest_tab_bar(self.manifest_tab, draft));
        body = body.push(match self.manifest_tab {
            ManifestTab::Settings => {
                manifest_tab_settings(draft, &self.dependency_candidates(draft))
            }
            ManifestTab::Capabilities => manifest_capabilities(draft.caps),
            ManifestTab::Network => manifest_tab_network(draft),
            ManifestTab::Files => manifest_tab_files(draft),
            ManifestTab::System => manifest_tab_system(draft),
        });

        body = body.push(self.manifest_edit_bar());
        body.into()
    }

    /// Candidate `smudgy://owner/name` specifiers for the dependency picker: the author's own local
    /// packages and their installed packages, minus the package being edited and anything already
    /// listed. Lets the author insert a correctly-owned specifier instead of hand-typing the owner —
    /// which, after "Make a copy", easily points at the *original* author's package (whose latest
    /// version is older), silently pinning that old version when this package is published.
    fn dependency_candidates(&self, draft: &ManifestDraft) -> Vec<String> {
        // Bare `smudgy://owner/name` of each already-listed dependency (drop any `@range`), so a
        // package already depended-on isn't offered again.
        let already: std::collections::HashSet<&str> = draft
            .dependencies
            .iter()
            .filter_map(|d| d.trim().split('@').next())
            .filter(|s| !s.is_empty())
            .collect();
        let self_spec = self
            .local_package
            .as_ref()
            .map(|p| self.local_own_spec(&p.name));
        let mut candidates: Vec<String> = self
            .local_packages
            .iter()
            .map(|name| self.local_own_spec(name))
            .chain(self.installed_packages.iter().map(|p| p.specifier.clone()))
            .filter(|spec| Some(spec) != self_spec.as_ref())
            .filter(|spec| !already.contains(spec.as_str()))
            .collect();
        candidates.sort();
        candidates.dedup();
        candidates
    }

    /// The edit-mode action bar: Cancel (discard edits + back to the read-only summary) and Save
    /// (commit + back to the summary; enabled only when there are unsaved changes).
    fn manifest_edit_bar(&self) -> Elem<'_> {
        let mut bar = row![]
            .spacing(12.0)
            .align_y(Vertical::Center)
            .padding(Padding {
                top: 8.0,
                bottom: 0.0,
                left: 0.0,
                right: 0.0,
            });
        if self.manifest_dirty {
            bar = bar
                .push(text("\u{25CF}").size(9.0).style(common::accent))
                .push(text("Unsaved changes").size(13.0).style(common::muted));
        }
        bar = bar.push(iced::widget::space::horizontal());
        bar = bar.push(
            button(text("Cancel").size(13.0))
                .style(button_style::secondary)
                .on_press(Message::RevertManifest),
        );
        bar = bar.push(
            button(text("Save manifest").size(13.0))
                .style(button_style::primary)
                .on_press_maybe(self.manifest_dirty.then_some(Message::SaveManifest)),
        );
        bar.into()
    }
}

// ---- read-only summary -----------------------------------------------------

/// The default read-only "pretty" view of the current (on-disk) manifest, with an Edit button that
/// drops into the structured editor.
fn manifest_readonly_body(manifest: &PackageManifest) -> Elem<'_> {
    let header = row![
        common::section_label("Manifest"),
        iced::widget::space::horizontal(),
        button(
            row![
                text(bootstrap_icons::PENCIL)
                    .font(fonts::BOOTSTRAP_ICONS)
                    .size(12.0),
                text("Edit").size(13.0),
            ]
            .spacing(6.0)
            .align_y(Vertical::Center),
        )
        .style(button_style::secondary)
        .on_press(Message::ManifestBeginEdit),
    ]
    .align_y(Vertical::Center);

    let entry: Elem = match &manifest.entry {
        Some(entry) if !entry.is_empty() => text(entry.clone()).size(14.0).into(),
        _ => text("index.* (resolved automatically)")
            .size(14.0)
            .style(common::faint)
            .into(),
    };

    let perms = &manifest.permissions;
    column![
        header,
        // Trim version/description to match the pane header (which shows the same values trimmed).
        ro_block(
            "Version",
            text(manifest.version.trim().to_string()).size(14.0).into()
        ),
        ro_block(
            "Description",
            ro_text_or(manifest.description.trim(), "No description")
        ),
        ro_block("Entry", entry),
        ro_block(
            "Requires smudgy",
            ro_text_or(
                manifest.min_smudgy_version.as_deref().unwrap_or(""),
                "Any version"
            ),
        ),
        ro_block(
            "Aligned hosts",
            ro_value_list(&manifest.hosts, "Any host", true)
        ),
        ro_block(
            "Dependencies",
            ro_value_list(&manifest.dependencies, "None", true)
        ),
        ro_block("Parameters", ro_params(&manifest.params)),
        common::section_label("Permissions"),
        ro_block("Connections", ro_value_list(&perms.net, "None", true)),
        ro_block(
            "Code imports",
            ro_text_or(import_policy_summary(perms.import), "None")
        ),
        ro_block("Read files", ro_value_list(&perms.read, "None", true)),
        ro_block("Write files", ro_value_list(&perms.write, "None", true)),
        ro_block("Environment", ro_value_list(&perms.env, "None", true)),
        ro_block("System info", ro_value_list(&perms.sys, "None", true)),
        ro_block("Run programs", ro_value_list(&perms.run, "None", true)),
        ro_block("Native libraries", ro_value_list(&perms.ffi, "None", true)),
        ro_block("Capabilities", ro_caps(perms.smudgy)),
    ]
    .spacing(10.0)
    .into()
}

/// A read-only labelled block: a top-aligned muted label + its value(s).
fn ro_block<'a>(label: &str, value: Elem<'a>) -> Elem<'a> {
    row![
        container(text(label.to_string()).size(13.0).style(common::muted))
            .width(Length::Fixed(LABEL_WIDTH)),
        value,
    ]
    .spacing(12.0)
    .align_y(Vertical::Top)
    .into()
}

/// A value text, or a faint placeholder when the string is blank.
fn ro_text_or<'a>(value: &str, empty: &str) -> Elem<'a> {
    if value.trim().is_empty() {
        text(empty.to_string())
            .size(14.0)
            .style(common::faint)
            .into()
    } else {
        text(value.to_string()).size(14.0).into()
    }
}

/// A read-only list value: one line per non-blank entry (monospace when `mono`), or a faint
/// placeholder when the list is empty.
fn ro_value_list<'a>(items: &[String], empty: &str, mono: bool) -> Elem<'a> {
    let kept: Vec<&String> = items
        .iter()
        .filter(|item| !item.trim().is_empty())
        .collect();
    if kept.is_empty() {
        return text(empty.to_string())
            .size(13.0)
            .style(common::faint)
            .into();
    }
    let mut col = Column::new().spacing(2.0);
    for item in kept {
        let mut line = text(item.clone()).size(13.0);
        if mono {
            line = line.font(fonts::GEIST_MONO_VF);
        }
        col = col.push(line);
    }
    col.into()
}

/// A read-only parameter summary: one line per declared param (key, optional label, type + flags,
/// and a non-secret default).
fn ro_params<'a>(params: &[PackageParameter]) -> Elem<'a> {
    if params.is_empty() {
        return text("None").size(13.0).style(common::faint).into();
    }
    let mut col = Column::new().spacing(4.0);
    for param in params {
        let mut tags = vec![kind_summary(param)];
        if param.required {
            tags.push("required".to_string());
        }
        if param.secret {
            tags.push("secret".to_string());
        }
        let mut line = row![
            text(param.key.clone())
                .size(13.0)
                .font(fonts::GEIST_MONO_VF)
        ]
        .spacing(8.0)
        .align_y(Vertical::Center);
        if let Some(label) = &param.label
            && !label.is_empty()
        {
            line = line.push(text(label.clone()).size(12.0).style(common::muted));
        }
        line = line.push(text(tags.join(" · ")).size(12.0).style(common::faint));
        // A non-secret default value, if set.
        if !param.secret {
            let default = default_to_text(param.default.as_ref());
            if !default.is_empty() {
                line = line.push(
                    text(format!("default {default}"))
                        .size(12.0)
                        .style(common::faint),
                );
            }
        }
        col = col.push(line);
    }
    col.into()
}

/// The read-only type summary for a declared param: the plain kind name, plus a parenthetical for
/// the new shapes (a dropdown's option count, a list's element type, a table's column keys).
fn kind_summary(param: &PackageParameter) -> String {
    match param.kind {
        ParamKind::String => "text".to_string(),
        ParamKind::Bool => "boolean".to_string(),
        ParamKind::Number => "number".to_string(),
        ParamKind::Dropdown => format!("dropdown ({} options)", param.options.len()),
        ParamKind::List => {
            let element = param.fields.first().map_or("text", |f| kind_word(f.kind));
            format!("list of {element}")
        }
        ParamKind::Table => {
            let columns: Vec<&str> = param.fields.iter().map(|f| f.key.as_str()).collect();
            format!("table [{}]", columns.join(", "))
        }
    }
}

/// The bare one-word name of a (scalar) kind, for nesting inside [`kind_summary`].
fn kind_word(kind: ParamKind) -> &'static str {
    match kind {
        ParamKind::String => "text",
        ParamKind::Bool => "boolean",
        ParamKind::Number => "number",
        ParamKind::Dropdown => "dropdown",
        ParamKind::List => "list",
        ParamKind::Table => "table",
    }
}

/// A read-only capabilities list: human labels for the granted op-capabilities, or a sandbox note.
fn ro_caps<'a>(caps: SmudgyCapabilities) -> Elem<'a> {
    let labels = granted_cap_labels(caps);
    if labels.is_empty() {
        return text("Fully sandboxed — no special abilities.")
            .size(13.0)
            .style(common::faint)
            .into();
    }
    let mut col = Column::new().spacing(2.0);
    for label in labels {
        col = col.push(text(label).size(13.0));
    }
    col.into()
}

/// The author-facing labels for the granted op-capabilities (read-only summary), in a stable
/// order. Each leads with the `smudgy:core` API the capability gates, so an author maps a row
/// straight to the call in their code (the end-user-facing wording lives in the install consent
/// window, `packages.rs`).
fn granted_cap_labels(caps: SmudgyCapabilities) -> Vec<&'static str> {
    let mut out = Vec::new();
    if caps.create_aliases {
        out.push("createAlias \u{2014} define input aliases");
    }
    if caps.create_triggers {
        out.push("createTrigger / createTriggers \u{2014} act on game output");
    }
    if caps.send {
        out.push("send \u{2014} send commands as if typed (runs through your aliases)");
    }
    if caps.send_direct {
        out.push("sendRaw \u{2014} send straight to the game (bypasses your aliases)");
    }
    if caps.echo {
        out.push("echo \u{2014} print text to your screen");
    }
    if caps.reach_others {
        out.push("getSessions / byName \u{2014} reach your other connected sessions");
    }
    if caps.change_display {
        out.push("line / buffer \u{2014} gag, highlight, insert, or replace text");
    }
    if caps.mapper_read {
        out.push("mapper \u{2014} read your maps");
    }
    if caps.mapper_write {
        out.push("mapper \u{2014} change your maps");
    }
    if caps.widgets {
        out.push("widgets \u{2014} create and change on-screen widgets");
    }
    if caps.interop_write {
        out.push("emit / set \u{2014} broadcast events and publish shared state");
    }
    if caps.interop_read {
        out.push("on / get / watch \u{2014} listen for events and read shared state");
    }
    if caps.panes {
        out.push("pane \u{2014} create or interact with split panes");
    }
    if caps.gmcp_send {
        out.push("gmcp.send \u{2014} send GMCP messages to the game and manage GMCP modules");
    }
    out
}

/// The "Parameters" block: one editable card per declared parameter, plus an add button.
fn manifest_params(draft: &ManifestDraft) -> Elem<'_> {
    let mut col = Column::new()
        .spacing(8.0)
        .push(common::section_label("Parameters"))
        .push(
            text("Values configured at install time. Required ones gate loading until set; secrets go to the OS keychain.")
                .size(11.0)
                .style(common::muted),
        );
    if draft.params.is_empty() {
        col = col.push(text("No parameters.").size(12.0).style(common::faint));
    }
    for (i, param) in draft.params.iter().enumerate() {
        col = col.push(param_card(i, param));
    }
    col.push(add_button(
        "Add parameter",
        Message::EditManifest(ManifestEdit::AddParam),
    ))
    .into()
}

/// One parameter's editable card.
fn param_card(i: usize, param: &ParamDraft) -> Elem<'_> {
    let kind_picker = pick_list(
        KindChoice::ALL.to_vec(),
        Some(KindChoice::from(param.kind)),
        move |choice| Message::EditManifest(ManifestEdit::ParamKind(i, choice.into())),
    )
    .text_size(13.0);

    let top = row![
        text_input("key (e.g. apiToken)", &param.key)
            .on_input(move |v| Message::EditManifest(ManifestEdit::ParamKey(i, v)))
            .size(14.0)
            .width(Length::Fill),
        kind_picker,
        button(
            text(bootstrap_icons::TRASH_3)
                .font(fonts::BOOTSTRAP_ICONS)
                .size(14.0)
        )
        .style(button_style::secondary)
        .on_press(Message::EditManifest(ManifestEdit::RemoveParam(i)))
        .padding(8),
    ]
    .spacing(8.0)
    .align_y(Vertical::Center);

    let label_row = text_input("label shown to users (optional)", &param.label)
        .on_input(move |v| Message::EditManifest(ManifestEdit::ParamLabel(i, v)))
        .size(14.0);

    let mut body = Column::new().spacing(8.0).push(top).push(label_row);
    body = body.push(param_kind_config(i, param));
    body = body.push(param_toggles(i, param));

    container(body)
        .padding(12.0)
        .width(Length::Fill)
        .style(common::banner_style)
        .into()
}

/// The kind-specific configuration block for a parameter card: a default field for a scalar, an
/// options editor + default picker for a dropdown, an element editor for a list, a columns editor
/// for a table.
fn param_kind_config(i: usize, param: &ParamDraft) -> Elem<'_> {
    match param.kind {
        ParamKind::String | ParamKind::Bool | ParamKind::Number => default_text_row(i, param),
        ParamKind::Dropdown => column![
            common::section_label("Options"),
            options_editor(OptionPath::Param(i), &param.options),
            dropdown_default_picker(i, param),
        ]
        .spacing(8.0)
        .into(),
        ParamKind::List => {
            let mut col = Column::new()
                .spacing(8.0)
                .push(common::section_label("Each entry is"));
            if let Some(element) = param.fields.first() {
                col = col.push(sub_param_editor(i, 0, element, false));
            }
            col.into()
        }
        ParamKind::Table => {
            let mut col = Column::new()
                .spacing(8.0)
                .push(common::section_label("Columns"))
                .push(
                    text("Each row stores one value per column, keyed by the column key.")
                        .size(11.0)
                        .style(common::muted),
                );
            for (j, field) in param.fields.iter().enumerate() {
                col = col.push(sub_param_editor(i, j, field, true));
            }
            col.push(add_button(
                "Add column",
                Message::EditManifest(ManifestEdit::ParamAddField(i)),
            ))
            .into()
        }
    }
}

/// The scalar default text input for a String/Bool/Number param.
fn default_text_row(i: usize, param: &ParamDraft) -> Elem<'_> {
    let placeholder = match param.kind {
        ParamKind::Bool => "default: true or false",
        ParamKind::Number => "default number (optional)",
        _ => "default text (optional)",
    };
    text_input(placeholder, &param.default)
        .on_input(move |v| Message::EditManifest(ManifestEdit::ParamDefault(i, v)))
        .size(14.0)
        .into()
}

/// The Required (always) + Secret (text params only — secrets are stored as keyring strings)
/// toggles for a parameter card.
fn param_toggles(i: usize, param: &ParamDraft) -> Elem<'_> {
    let mut toggles = row![
        checkbox(param.required)
            .label("Required")
            .size(14)
            .text_size(13)
            .on_toggle(move |v| Message::EditManifest(ManifestEdit::ParamRequired(i, v))),
    ]
    .spacing(20.0)
    .align_y(Vertical::Center);
    if param.kind == ParamKind::String {
        toggles = toggles.push(
            checkbox(param.secret)
                .label("Secret")
                .size(14)
                .text_size(13)
                .on_toggle(move |v| Message::EditManifest(ManifestEdit::ParamSecret(i, v))),
        );
    }
    toggles.into()
}

/// Where an options editor routes its edits: a top-level dropdown param, or a dropdown table column.
#[derive(Debug, Clone, Copy)]
enum OptionPath {
    /// The dropdown param at index `i`.
    Param(usize),
    /// Column `j` of the table param at index `i`.
    Field(usize, usize),
}

impl OptionPath {
    fn add(self) -> ManifestEdit {
        match self {
            OptionPath::Param(i) => ManifestEdit::ParamAddOption(i),
            OptionPath::Field(i, j) => ManifestEdit::ParamFieldAddOption(i, j),
        }
    }
    fn remove(self, k: usize) -> ManifestEdit {
        match self {
            OptionPath::Param(i) => ManifestEdit::ParamRemoveOption(i, k),
            OptionPath::Field(i, j) => ManifestEdit::ParamFieldRemoveOption(i, j, k),
        }
    }
    fn value(self, k: usize, value: String) -> ManifestEdit {
        match self {
            OptionPath::Param(i) => ManifestEdit::ParamOptionValue(i, k, value),
            OptionPath::Field(i, j) => ManifestEdit::ParamFieldOptionValue(i, j, k, value),
        }
    }
    fn label(self, k: usize, value: String) -> ManifestEdit {
        match self {
            OptionPath::Param(i) => ManifestEdit::ParamOptionLabel(i, k, value),
            OptionPath::Field(i, j) => ManifestEdit::ParamFieldOptionLabel(i, j, k, value),
        }
    }
}

/// An editor for a dropdown's choices: a value + label row per option (each with a remove button),
/// plus an add button. Drives either a top-level dropdown or a dropdown table column via [`OptionPath`].
fn options_editor(path: OptionPath, options: &[OptionDraft]) -> Elem<'_> {
    let mut col = Column::new().spacing(6.0);
    if options.is_empty() {
        col = col.push(text("No options yet.").size(12.0).style(common::faint));
    }
    for (k, option) in options.iter().enumerate() {
        col = col.push(
            row![
                text_input("value (stored)", &option.value)
                    .on_input(move |v| Message::EditManifest(path.value(k, v)))
                    .size(14.0)
                    .width(Length::Fill),
                text_input("label (optional)", &option.label)
                    .on_input(move |v| Message::EditManifest(path.label(k, v)))
                    .size(14.0)
                    .width(Length::Fill),
                button(
                    text(bootstrap_icons::TRASH_3)
                        .font(fonts::BOOTSTRAP_ICONS)
                        .size(13.0)
                )
                .style(button_style::secondary)
                .on_press(Message::EditManifest(path.remove(k)))
                .padding(6),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center),
        );
    }
    col.push(add_button("Add option", Message::EditManifest(path.add())))
        .into()
}

/// A `pick_list` to choose a dropdown param's default from its declared options (plus "(no default)").
fn dropdown_default_picker(i: usize, param: &ParamDraft) -> Elem<'_> {
    let mut choices = vec![DefaultChoice::none()];
    for option in &param.options {
        let value = option.value.trim();
        if !value.is_empty() {
            choices.push(DefaultChoice {
                value: value.to_string(),
                label: {
                    let label = option.label.trim();
                    if label.is_empty() {
                        value.to_string()
                    } else {
                        label.to_string()
                    }
                },
            });
        }
    }
    let current = param.default.trim();
    let selected = choices
        .iter()
        .find(|c| c.value == current)
        .cloned()
        .or_else(|| choices.first().cloned());
    row![
        container(text("Default").size(13.0).style(common::muted)).width(Length::Fixed(72.0)),
        pick_list(choices, selected, move |choice: DefaultChoice| {
            Message::EditManifest(ManifestEdit::ParamDefault(i, choice.value))
        })
        .text_size(13.0),
    ]
    .spacing(8.0)
    .align_y(Vertical::Center)
    .into()
}

/// A `pick_list` entry for a dropdown default. An empty `value` is the "(no default)" sentinel.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DefaultChoice {
    value: String,
    label: String,
}

impl DefaultChoice {
    fn none() -> Self {
        Self {
            value: String::new(),
            label: "(no default)".to_string(),
        }
    }
}

impl std::fmt::Display for DefaultChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

/// One scalar sub-parameter editor — a list element (`is_column == false`) or a table column
/// (`is_column == true`). A column shows a key input and a remove button; a list element shows
/// neither (its values are stored bare and a list has exactly one element type). A dropdown
/// sub-param nests its own options editor.
fn sub_param_editor<'a>(i: usize, j: usize, field: &'a SubParamDraft, is_column: bool) -> Elem<'a> {
    let kind_picker = pick_list(
        KindChoice::SCALAR.to_vec(),
        Some(KindChoice::from(field.kind)),
        move |choice| Message::EditManifest(ManifestEdit::ParamFieldKind(i, j, choice.into())),
    )
    .text_size(13.0);

    let mut header = row![].spacing(8.0).align_y(Vertical::Center);
    if is_column {
        header = header.push(
            text_input("column key", &field.key)
                .on_input(move |v| Message::EditManifest(ManifestEdit::ParamFieldKey(i, j, v)))
                .size(14.0)
                .width(Length::Fill),
        );
    } else {
        header = header.push(
            container(text("a value of type").size(13.0).style(common::muted)).width(Length::Fill),
        );
    }
    header = header.push(kind_picker);
    if is_column {
        header = header.push(
            button(
                text(bootstrap_icons::TRASH_3)
                    .font(fonts::BOOTSTRAP_ICONS)
                    .size(13.0),
            )
            .style(button_style::secondary)
            .on_press(Message::EditManifest(ManifestEdit::ParamRemoveField(i, j)))
            .padding(6),
        );
    }

    let label_row = text_input("label (optional)", &field.label)
        .on_input(move |v| Message::EditManifest(ManifestEdit::ParamFieldLabel(i, j, v)))
        .size(14.0);

    let mut col = Column::new().spacing(6.0).push(header).push(label_row);
    if field.kind == ParamKind::Dropdown {
        col = col
            .push(common::section_label("Options"))
            .push(options_editor(OptionPath::Field(i, j), &field.options));
    }
    container(col)
        .padding(8.0)
        .width(Length::Fill)
        .style(common::code_surface_style)
        .into()
}

// ---- tabs ------------------------------------------------------------------

/// The tab strip: one button per [`ManifestTab`], the active one highlighted, each carrying a
/// faint count of how many entries it currently holds (so the populated tabs are visible at a glance).
fn manifest_tab_bar(active: ManifestTab, draft: &ManifestDraft) -> Elem<'_> {
    let files = nonblank(&draft.read) + nonblank(&draft.write);
    let system =
        nonblank(&draft.env) + nonblank(&draft.sys) + nonblank(&draft.run) + nonblank(&draft.ffi);
    row![
        tab_button(active, ManifestTab::Settings, "Settings", None),
        tab_button(
            active,
            ManifestTab::Capabilities,
            "Capabilities",
            Some(granted_cap_count(draft.caps))
        ),
        tab_button(
            active,
            ManifestTab::Network,
            "Network",
            Some(nonblank(&draft.net) + usize::from(draft.import != ImportPolicy::None))
        ),
        tab_button(active, ManifestTab::Files, "Files", Some(files)),
        tab_button(active, ManifestTab::System, "System", Some(system)),
    ]
    .spacing(4.0)
    .align_y(Vertical::Center)
    .into()
}

/// One tab button. Active uses the selected list-item fill; inactive is quiet. A `count` of `0`
/// renders no badge (so an empty tab isn't visually noisy).
fn tab_button<'a>(
    active: ManifestTab,
    tab: ManifestTab,
    label: &str,
    count: Option<usize>,
) -> Elem<'a> {
    let mut content = row![text(label.to_string()).size(13.0)]
        .spacing(6.0)
        .align_y(Vertical::Center);
    if let Some(n) = count.filter(|n| *n > 0) {
        content = content.push(text(n.to_string()).size(11.0).style(common::faint));
    }
    button(content)
        .style(if active == tab {
            button_style::list_item_selected
        } else {
            button_style::list_item
        })
        .on_press(Message::SelectManifestTab(tab))
        .padding(Padding {
            top: 6.0,
            bottom: 6.0,
            left: 12.0,
            right: 12.0,
        })
        .into()
}

/// Entries in `list` that aren't blank (matches what [`clean_list`] keeps on save).
fn nonblank(list: &[String]) -> usize {
    list.iter().filter(|item| !item.trim().is_empty()).count()
}

/// How many op-capabilities are granted (a rough indicator for the tab badge).
fn granted_cap_count(caps: SmudgyCapabilities) -> usize {
    [
        caps.create_aliases,
        caps.create_triggers,
        caps.send,
        caps.send_direct,
        caps.echo,
        caps.reach_others,
        caps.change_display,
        caps.mapper_read,
        caps.mapper_write,
        caps.widgets,
        caps.interop_read,
        caps.interop_write,
        caps.panes,
        caps.gmcp_send,
    ]
    .iter()
    .filter(|granted| **granted)
    .count()
}

/// The framing note shared by the permission tabs (Network/Files/System).
fn perm_note<'a>() -> Elem<'a> {
    text("A sandboxed install is denied anything not listed here.")
        .size(11.0)
        .style(common::muted)
        .into()
}

/// Why a publisher's dependency choices reach further than their own machine: a publish
/// freezes the whole tree, so installs only ever move forward when this package is re-published.
fn dependency_lock_note<'a>() -> Elem<'a> {
    container(
        text(
            "Publishing locks the exact version of every \
             dependency at PUBLISH time. Users of your package will \
             only realize newer versions of your dependencies if you release a new version of \
             your package that updates them.",
        )
        .size(11.0)
        .style(common::muted),
    )
    .width(Length::Fill)
    .padding(Padding {
        top: 8.0,
        bottom: 8.0,
        left: 12.0,
        right: 12.0,
    })
    .style(common::banner_style)
    .into()
}

/// Settings tab: aligned hosts, dependencies, and declared parameters. `dep_candidates` are the
/// author's own packages offered by the dependency picker (see [`AutomationsWindow::dependency_candidates`]).
fn manifest_tab_settings<'a>(draft: &'a ManifestDraft, dep_candidates: &[String]) -> Elem<'a> {
    // The dependency block: the free-text editor (smudgy://, jsr:, npm:, relative), an
    // owned/installed-package picker that inserts a correctly-owned specifier, then the lock note.
    let mut deps = column![list_editor(
        "Dependencies",
        Some("Other smudgy packages this one imports: smudgy://owner/name@^1.2. Currently this only supports smudgy:// packages. Version management for jsr and npm packages is handled by the module loaders backing `import` and `require`, and any jsr or npm package may be imported by a package with permission to access those registries."),
        ListField::Dependency,
        &draft.dependencies,
        "smudgy://owner/name@^1.0",
        "dependency",
    )]
    .spacing(8.0);
    if !dep_candidates.is_empty() {
        deps = deps.push(
            pick_list(dep_candidates.to_vec(), None::<String>, |chosen: String| {
                Message::EditManifest(ManifestEdit::AddItemValue(ListField::Dependency, chosen))
            })
            .placeholder("Add one of your installed or local packages\u{2026}")
            .text_size(13.0),
        );
    }
    deps = deps.push(dependency_lock_note());

    // "Requires smudgy": the min_smudgy_version floor, with live feedback — a red-path
    // warning when it won't save (not a version) and an advisory one when it parses but
    // exceeds this build (legit while targeting an unreleased smudgy, so it never blocks).
    let mut requires_smudgy = column![
        field_row(
            "Requires smudgy",
            text_input("any version", &draft.min_smudgy_version)
                .on_input(|v| Message::EditManifest(ManifestEdit::MinSmudgyVersion(v)))
                .size(14.0)
                .into(),
        ),
        text(
            "The minimum smudgy version this package runs on. Generally used to prevent users of this package on older smudgy versions from auto-updating to a new package version that won't run. Leave empty to allow any smudgy version.",
        )
        .size(12.0)
        .style(common::muted),
    ]
    .spacing(4.0);
    let min = draft.min_smudgy_version.trim();
    if !min.is_empty() {
        match semver::Version::parse(min) {
            Err(_) => {
                requires_smudgy = requires_smudgy.push(
                    text("Not a version. Must be in semver format, e.g. 0.3.5 (or empty) to save.")
                        .size(11.0)
                        .style(common::warning),
                );
            }
            Ok(parsed) if parsed > running_smudgy_release() => {
                requires_smudgy = requires_smudgy.push(
                    text(format!(
                        "Newer than this smudgy ({}) — installing and loading are refused \
                         below {parsed} (your local dev copy stays exempt).",
                        running_smudgy_release()
                    ))
                    .size(11.0)
                    .style(common::warning),
                );
            }
            Ok(_) => {}
        }
    }

    column![
        list_editor(
            "Aligned MUD hosts",
            Some("Hosts this package targets in Discover, applied when you publish. Leave empty for host-agnostic."),
            ListField::Host,
            &draft.hosts,
            "e.g. aardwolf.org",
            "host",
        ),
        requires_smudgy,
        deps,
        list_editor(
            "Required packages",
            Some("Packages that will be automatically installed _alongside_ this one, but will run in their own separate sandbox. smudgy://owner/name[@^1.2]."),
            ListField::Requires,
            &draft.requires,
            "smudgy://owner/arctic-prompt",
            "required package",
        ),
        column![
            checkbox(draft.importable)
                .label("Allow others to import this package")
                .size(14)
                .text_size(13)
                .on_toggle(|v| Message::EditManifest(ManifestEdit::Importable(v))),
            text(
                "Off: only your packages may import this one's modules.  Other packages may still import it but will only receive its types.  Note: This is subject to change and it is likely in the future that even your own packages will only receive types as well.",
            )
            .size(12.0)
            .style(common::muted),
        ]
        .spacing(4.0),
        manifest_params(draft),
    ]
    .spacing(16.0)
    .into()
}

/// Network tab: the `permissions.net` connection allowlist + the public-registry import toggle
/// (`permissions.import`). Connecting to a host and downloading code from one are separate grants.
fn manifest_tab_network(draft: &ManifestDraft) -> Elem<'_> {
    column![
        perm_note(),
        list_editor(
            "Allowed hosts",
            Some("hostname or hostname:port."),
            ListField::Net,
            &draft.net,
            "comms.example.org:6667",
            "host",
        ),
        import_policy_picker(draft.import),
    ]
    .spacing(16.0)
    .into()
}

/// The tri-state `permissions.import` chooser: how far outside the smudgy ecosystem the package may
/// download code to run. A separate axis from the host allowlist above (connecting to a host vs.
/// downloading code from one). `smudgy://` package imports work at every level.
fn import_policy_picker<'a>(selected: ImportPolicy) -> Elem<'a> {
    let on_pick = |v| Message::EditManifest(ManifestEdit::ImportPolicy(v));
    let choice = |label: &'static str, value: ImportPolicy| {
        radio(label, value, Some(selected), on_pick)
            .size(16)
            .text_size(13)
    };
    column![
        common::section_label("Code imports"),
        choice(
            "No modules outside of the smudgy ecosystem",
            ImportPolicy::None
        ),
        choice(
            "Can download and run modules from public registries (npm, jsr)",
            ImportPolicy::Registries
        ),
        choice(
            "Can download and run modules from anywhere",
            ImportPolicy::Any
        ),
    ]
    .spacing(6.0)
    .into()
}

/// The read-only one-line summary of an `import` policy (the "Code imports" row). `None` returns the
/// empty string so `ro_text_or` renders its "None" placeholder.
fn import_policy_summary(policy: ImportPolicy) -> &'static str {
    match policy {
        ImportPolicy::None => "",
        ImportPolicy::Registries => "Public registries (npm, jsr)",
        ImportPolicy::Any => "Anywhere on the web",
    }
}

/// Files tab: `permissions.read` + `permissions.write`. A path outside `$DATA` changes the
/// grant's meaning — a read reaches the user's files, a write can rewrite config/scripts/other
/// packages (effectively full access) — so escaping entries surface the same warning installers
/// will see.
fn manifest_tab_files(draft: &ManifestDraft) -> Elem<'_> {
    let mut col = column![
        perm_note(),
        list_editor(
            "Readable paths",
            Some("$DATA is the package's data dir"),
            ListField::Read,
            &draft.read,
            "$DATA/maps",
            "path",
        ),
    ]
    .spacing(16.0);
    if escapes_data(&draft.read) {
        col = col.push(
            text(
                "A readable path outside $DATA reaches the user's own files. Installers will see \
                 it flagged. Prefer $DATA unless reading their files is the point.",
            )
            .size(11.0)
            .style(common::warning),
        );
    }
    col = col.push(list_editor(
        "Writable paths",
        None,
        ListField::Write,
        &draft.write,
        "$DATA",
        "path",
    ));
    if escapes_data(&draft.write) {
        col = col.push(full_access_author_note(
            "A writable path outside $DATA can rewrite the user's config, scripts, or other \
             packages — it effectively un-sandboxes the package.",
        ));
    }
    col.into()
}

/// System tab: `permissions.env` + `permissions.sys` + `permissions.run` + `permissions.ffi`.
/// `run`/`ffi` carry the author-facing version of the consent window's full-access warning: an
/// entry here is honest to declare, but it is a trust ask, not a scoped grant.
fn manifest_tab_system(draft: &ManifestDraft) -> Elem<'_> {
    let mut col = column![
        perm_note(),
        list_editor(
            "Readable environment variables",
            Some("Exact variable names it may read."),
            ListField::Env,
            &draft.env,
            "MYPKG_TOKEN",
            "variable",
        ),
        list_editor(
            "System information",
            Some(
                "Deno system-info kinds it may query, one per entry: hostname, osRelease, \
                 osUptime, loadavg, networkInterfaces, systemMemoryInfo, uid, gid, cpus, homedir, \
                 statfs, getPriority, setPriority, userInfo. An unknown kind refuses to load.",
            ),
            ListField::Sys,
            &draft.sys,
            "hostname",
            "kind",
        ),
        list_editor(
            "Runnable programs",
            Some(
                "Program names (found on the user's PATH) or absolute paths this package may run \
                 as subprocesses.",
            ),
            ListField::Run,
            &draft.run,
            "git",
            "program",
        ),
    ]
    .spacing(16.0);
    if nonblank(&draft.run) > 0 {
        col = col.push(full_access_author_note(
            "A program this package runs is NOT sandboxed — it acts with the user's full \
             privileges, whatever the program is.",
        ));
    }
    col = col.push(list_editor(
        "Loadable native libraries",
        Some("Paths to dynamic libraries it may load via FFI ($DATA works here too)."),
        ListField::Ffi,
        &draft.ffi,
        "$DATA/native/helper.dll",
        "library",
    ));
    if nonblank(&draft.ffi) > 0 {
        col = col.push(full_access_author_note(
            "Native code this package loads is NOT sandboxed — it acts with the user's full \
             privileges.",
        ));
    }
    col.into()
}

/// Whether any non-blank path entry escapes the package's own `$DATA` dir (the author-side twin
/// of the installer-facing risk cliff — see `packages::data_scoped`).
fn escapes_data(paths: &[String]) -> bool {
    paths
        .iter()
        .filter(|p| !p.trim().is_empty())
        .any(|p| !super::packages::data_scoped(p))
}

/// The author-facing "this is a full-access ask" warning under a `run`/`ffi`/escaping-`write`
/// editor: names the consequence and what installers will be shown, so a heavyweight grant is
/// never declared casually.
fn full_access_author_note(consequence: &str) -> Elem<'_> {
    container(
        row![
            text("\u{26A0}").size(13.0).style(common::danger),
            text(format!(
                "{consequence} Installers will see a prominent \u{201c}effectively full \
                 access\u{201d} warning on the install window."
            ))
            .size(11.0)
            .style(common::warning),
        ]
        .spacing(8.0)
        .align_y(Vertical::Top),
    )
    .width(Length::Fill)
    .padding(Padding {
        top: 8.0,
        bottom: 8.0,
        left: 12.0,
        right: 12.0,
    })
    .style(common::banner_style)
    .into()
}

/// The `permissions.smudgy` op-capability toggles, grouped the way the manifest keys group them.
/// (The "Capabilities" tab already labels this section, so no heading is repeated here.)
fn manifest_capabilities<'a>(caps: SmudgyCapabilities) -> Elem<'a> {
    // "Read maps" is forced on while "Change maps" is on (write implies read): a checked,
    // non-interactive row, dimmed and labelled to explain why it can't be turned off here.
    let mapper_read: Elem<'a> = if caps.mapper_write {
        cap_row(
            checkbox(true).size(16).into(),
            "mapper",
            "read your maps (required by change maps)",
            common::faint,
            None,
        )
    } else {
        cap_check(
            "mapper",
            "read your maps",
            caps.mapper_read,
            Cap::MapperRead,
        )
    };

    column![
        text("The smudgy:core APIs your scripts may call (the permissions.smudgy block). Calling one you didn't request throws at runtime.")
            .size(11.0)
            .style(common::muted),
        cap_group("Automations", vec![
            cap_check("createAlias", "define input aliases", caps.create_aliases, Cap::CreateAliases),
            cap_check("createTrigger / createTriggers", "act on game output", caps.create_triggers, Cap::CreateTriggers),
        ]),
        cap_group("Session", vec![
            cap_check("send", "send commands as if typed (runs through your aliases)", caps.send, Cap::Send),
            cap_check("sendRaw", "send straight to the game (bypasses your aliases)", caps.send_direct, Cap::SendDirect),
            cap_check("echo", "print text to your screen", caps.echo, Cap::Echo),
            cap_check("input", "access, change, and focus input change; manage autocomplete list", caps.input, Cap::Input),
            cap_check("sessions / byName", "reach your other connected sessions", caps.reach_others, Cap::ReachOthers),
        ]),
        cap_group("Display", vec![
            cap_check("line / buffer", "gag, highlight, insert, or replace text", caps.change_display, Cap::ChangeDisplay),
        ]),
        cap_group("Mapper", vec![
            mapper_read,
            cap_check("mapper", "change your maps", caps.mapper_write, Cap::MapperWrite),
        ]),
        cap_group("Widgets", vec![
            cap_check("createWidget", "create & change on-screen widgets", caps.widgets, Cap::Widgets),
        ]),
        cap_group("Interop", vec![
            cap_check("emit / set", "broadcast events and publish shared state other packages can react to", caps.interop_write, Cap::InteropWrite),
            cap_check("on / get / watch", "listen for events and read shared state", caps.interop_read, Cap::InteropRead),
        ]),
        cap_group("Panes", vec![
            cap_check("pane",  "create or interact with split panes", caps.panes, Cap::Panes),
        ]),
        cap_group("GMCP", vec![
            cap_check("gmcp.send", "send GMCP messages to the game and manage GMCP modules", caps.gmcp_send, Cap::GmcpSend),
        ]),
    ]
    .spacing(16.0)
    .into()
}

/// A labelled, indented group of capability rows — the visual chunking the Capabilities tab needs
/// so the dozen toggles read as a handful of categories rather than one flat wall.
fn cap_group<'a>(title: &str, rows: Vec<Elem<'a>>) -> Elem<'a> {
    let mut items = Column::new().spacing(8.0).padding(Padding {
        top: 0.0,
        bottom: 0.0,
        left: 18.0,
        right: 0.0,
    });
    for row in rows {
        items = items.push(row);
    }
    column![
        text(title.to_uppercase())
            .size(11.0)
            .font(fonts::GEIST_VF)
            .style(common::muted),
        items,
    ]
    .spacing(8.0)
    .into()
}

/// One capability toggle: leads with the gated `smudgy:core` API in monospace (the call an author
/// scans for) followed by a muted plain-language gloss, so a row maps straight to the code. The box
/// toggles natively; the label toggles via a `mouse_area` (whole row is a click target, mirroring
/// [`common::pill_switch`]).
fn cap_check<'a>(api: &'a str, gloss: &'a str, value: bool, cap: Cap) -> Elem<'a> {
    cap_row(
        checkbox(value)
            .size(16)
            .on_toggle(move |v| Message::EditManifest(ManifestEdit::Cap(cap, v)))
            .into(),
        api,
        gloss,
        common::muted,
        Some(Message::EditManifest(ManifestEdit::Cap(cap, !value))),
    )
}

/// Assemble a capability row from its (already-built) checkbox element and the mono-API + gloss
/// label. `gloss_style` dims the description (a locked row uses `faint`); `on_press` makes the
/// label a click target when the row is interactive, and is omitted for the forced/locked
/// mapper-read row.
fn cap_row<'a>(
    box_: Elem<'a>,
    api: &'a str,
    gloss: &'a str,
    gloss_style: fn(&crate::theme::Theme) -> text::Style,
    on_press: Option<Message>,
) -> Elem<'a> {
    let label = row![
        text(api)
            .size(13.0)
            .font(fonts::GEIST_MONO_VF)
            .style(common::regular),
        text(gloss).size(13.0).style(gloss_style),
    ]
    .spacing(8.0)
    .align_y(Vertical::Center);
    let label: Elem<'a> = match on_press {
        Some(msg) => mouse_area(label).on_press(msg).into(),
        None => label.into(),
    };
    row![box_, label]
        .spacing(10.0)
        .align_y(Vertical::Center)
        .into()
}

// ---- shared form helpers ---------------------------------------------------

/// A left-labeled single-line field row (the manifest analogue of `editors::field_row`).
fn field_row<'a>(label: &str, control: Elem<'a>) -> Elem<'a> {
    row![
        container(text(label.to_string()).size(13.0).style(common::muted))
            .width(Length::Fixed(LABEL_WIDTH))
            .align_y(Vertical::Center)
            .height(Length::Fixed(34.0)),
        control,
    ]
    .spacing(12.0)
    .align_y(Vertical::Center)
    .into()
}

/// An editable string-list: a titled, hinted block of one text input per entry (each with a
/// remove button) plus an add button. Drives any [`ListField`].
fn list_editor<'a>(
    title: &str,
    hint: Option<&str>,
    field: ListField,
    items: &'a [String],
    placeholder: &'static str,
    add_label: &str,
) -> Elem<'a> {
    let mut col = Column::new()
        .spacing(6.0)
        .push(common::section_label(title));
    if let Some(hint) = hint {
        col = col.push(text(hint.to_string()).size(11.0).style(common::muted));
    }
    if items.is_empty() {
        col = col.push(text("None.").size(12.0).style(common::faint));
    }
    for (i, item) in items.iter().enumerate() {
        col = col.push(
            row![
                text_input(placeholder, item)
                    .on_input(move |v| Message::EditManifest(ManifestEdit::SetItem(field, i, v)))
                    .size(14.0)
                    .width(Length::Fill),
                button(
                    text(bootstrap_icons::TRASH_3)
                        .font(fonts::BOOTSTRAP_ICONS)
                        .size(14.0)
                )
                .style(button_style::secondary)
                .on_press(Message::EditManifest(ManifestEdit::RemoveItem(field, i)))
                .padding(8),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center),
        );
    }
    col.push(add_button(
        &format!("Add {add_label}"),
        Message::EditManifest(ManifestEdit::AddItem(field)),
    ))
    .into()
}

/// A small secondary "＋ Add …" button.
fn add_button<'a>(label: &str, msg: Message) -> Elem<'a> {
    button(
        row![
            text(bootstrap_icons::PLUS_LG)
                .font(fonts::BOOTSTRAP_ICONS)
                .size(12.0),
            text(label.to_string()).size(13.0),
        ]
        .spacing(6.0)
        .align_y(Vertical::Center),
    )
    .style(button_style::secondary)
    .on_press(msg)
    .into()
}

/// The inline projection/save error, styled like the editors' error bar.
fn manifest_error<'a>(message: &str) -> Elem<'a> {
    container(
        row![
            text(bootstrap_icons::EXCLAMATION_TRIANGLE)
                .font(fonts::BOOTSTRAP_ICONS)
                .size(13.0)
                .style(common::danger),
            text(message.to_string()).size(13.0).style(common::danger),
        ]
        .spacing(8.0)
        .align_y(Vertical::Center),
    )
    .width(Length::Fill)
    .padding(Padding {
        top: 8.0,
        bottom: 8.0,
        left: 12.0,
        right: 12.0,
    })
    .style(|theme: &crate::theme::Theme| container::Style {
        background: Some(iced::Background::Color(
            theme.styles.text.error.scale_alpha(0.1),
        )),
        border: iced::Border {
            color: theme.styles.text.error.scale_alpha(0.4),
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    })
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A manifest already in the form the editor would write (trimmed, mapper read implied by
    /// write), so `from_manifest` -> `to_manifest` is expected to be the identity.
    fn sample_manifest() -> PackageManifest {
        PackageManifest {
            version: "1.2.3".to_string(),
            description: "A test package".to_string(),
            entry: Some("index.ts".to_string()),
            min_smudgy_version: Some("0.3.0".to_string()),
            dependencies: vec![
                "smudgy://wbk/util@^1.0".to_string(),
                "jsr:@std/path".to_string(),
            ],
            requires: vec!["smudgy://wbk/arctic-prompt".to_string()],
            hosts: vec!["aardwolf.org".to_string()],
            params: vec![
                PackageParameter {
                    key: "apiToken".to_string(),
                    label: Some("API Token".to_string()),
                    secret: true,
                    required: true,
                    kind: ParamKind::String,
                    default: None,
                    options: Vec::new(),
                    fields: Vec::new(),
                },
                PackageParameter {
                    key: "retries".to_string(),
                    label: None,
                    secret: false,
                    required: false,
                    kind: ParamKind::Number,
                    default: Some(json!(3)),
                    options: Vec::new(),
                    fields: Vec::new(),
                },
                // An integer above i64::MAX: must survive the text round-trip via the u64 path.
                PackageParameter {
                    key: "bigid".to_string(),
                    label: None,
                    secret: false,
                    required: false,
                    kind: ParamKind::Number,
                    default: Some(json!(18_446_744_073_709_551_615_u64)),
                    options: Vec::new(),
                    fields: Vec::new(),
                },
                PackageParameter {
                    key: "verbose".to_string(),
                    label: None,
                    secret: false,
                    required: false,
                    kind: ParamKind::Bool,
                    default: Some(json!(true)),
                    options: Vec::new(),
                    fields: Vec::new(),
                },
                // A String default whose surrounding whitespace is significant.
                PackageParameter {
                    key: "prefix".to_string(),
                    label: None,
                    secret: false,
                    required: false,
                    kind: ParamKind::String,
                    default: Some(json!("  > ")),
                    options: Vec::new(),
                    fields: Vec::new(),
                },
                // A dropdown with a labelled + a bare option and a default that is one of them.
                PackageParameter {
                    key: "mode".to_string(),
                    label: Some("Mode".to_string()),
                    secret: false,
                    required: false,
                    kind: ParamKind::Dropdown,
                    default: Some(json!("fast")),
                    options: vec![
                        ParamOption {
                            value: "fast".to_string(),
                            label: None,
                        },
                        ParamOption {
                            value: "slow".to_string(),
                            label: Some("Careful".to_string()),
                        },
                    ],
                    fields: Vec::new(),
                },
                // A list of text entries (element key normalizes to "value").
                PackageParameter {
                    key: "aliases".to_string(),
                    label: None,
                    secret: false,
                    required: false,
                    kind: ParamKind::List,
                    default: None,
                    options: Vec::new(),
                    fields: vec![PackageParameter {
                        key: "value".to_string(),
                        label: None,
                        secret: false,
                        required: false,
                        kind: ParamKind::String,
                        default: None,
                        options: Vec::new(),
                        fields: Vec::new(),
                    }],
                },
                // A table with a text column and a dropdown column.
                PackageParameter {
                    key: "routes".to_string(),
                    label: None,
                    secret: false,
                    required: false,
                    kind: ParamKind::Table,
                    default: None,
                    options: Vec::new(),
                    fields: vec![
                        PackageParameter {
                            key: "from".to_string(),
                            label: Some("From".to_string()),
                            secret: false,
                            required: false,
                            kind: ParamKind::String,
                            default: None,
                            options: Vec::new(),
                            fields: Vec::new(),
                        },
                        PackageParameter {
                            key: "via".to_string(),
                            label: None,
                            secret: false,
                            required: false,
                            kind: ParamKind::Dropdown,
                            default: None,
                            options: vec![
                                ParamOption {
                                    value: "road".to_string(),
                                    label: None,
                                },
                                ParamOption {
                                    value: "portal".to_string(),
                                    label: None,
                                },
                            ],
                            fields: Vec::new(),
                        },
                    ],
                },
            ],
            permissions: PackagePermissions {
                net: vec!["comms.example.org:6667".to_string()],
                read: vec!["$DATA/maps".to_string()],
                write: vec!["$DATA".to_string()],
                env: vec!["MYPKG_TOKEN".to_string()],
                run: vec!["git".to_string()],
                ffi: vec!["$DATA/native/helper.dll".to_string()],
                sys: vec!["hostname".to_string(), "osRelease".to_string()],
                import: ImportPolicy::Registries,
                smudgy: SmudgyCapabilities {
                    create_aliases: true,
                    send: true,
                    echo: true,
                    mapper_read: true,
                    mapper_write: true,
                    ..SmudgyCapabilities::default()
                },
            },
            importable: false,
        }
    }

    #[test]
    fn draft_round_trips_a_normalized_manifest() {
        let manifest = sample_manifest();
        let back = ManifestDraft::from_manifest(&manifest)
            .to_manifest()
            .expect("a normalized manifest projects cleanly");
        assert_eq!(back, manifest);
    }

    #[test]
    fn large_integer_default_is_preserved_via_u64() {
        // i64::MAX < this <= u64::MAX — the f64 fallback would lose precision.
        assert_eq!(
            parse_default(ParamKind::Number, "18446744073709551615").unwrap(),
            Some(json!(18_446_744_073_709_551_615_u64))
        );
        // A real fractional number still takes the f64 path.
        assert_eq!(
            parse_default(ParamKind::Number, "1.5").unwrap(),
            Some(json!(1.5))
        );
        assert!(parse_default(ParamKind::Number, "not-a-number").is_err());
    }

    #[test]
    fn string_default_preserves_significant_whitespace() {
        assert_eq!(
            parse_default(ParamKind::String, "  > ").unwrap(),
            Some(json!("  > "))
        );
        // A truly-empty buffer is "no default".
        assert_eq!(parse_default(ParamKind::String, "").unwrap(), None);
    }

    #[test]
    fn mapper_write_implies_read_on_projection() {
        let draft = ManifestDraft {
            version: "1.0.0".to_string(),
            caps: SmudgyCapabilities {
                mapper_write: true,
                mapper_read: false,
                ..SmudgyCapabilities::default()
            },
            ..ManifestDraft::default()
        };
        let manifest = draft.to_manifest().expect("projects");
        assert!(
            manifest.permissions.smudgy.mapper_read && manifest.permissions.smudgy.mapper_write
        );
    }

    #[test]
    fn projection_rejects_empty_version_and_blank_param_key() {
        let no_version = ManifestDraft {
            version: "   ".to_string(),
            ..ManifestDraft::default()
        };
        assert!(no_version.to_manifest().is_err());

        let blank_key = ManifestDraft {
            version: "1.0.0".to_string(),
            params: vec![ParamDraft::default()],
            ..ManifestDraft::default()
        };
        assert!(blank_key.to_manifest().is_err());
    }

    #[test]
    fn projection_validates_min_smudgy_version() {
        // Not a version -> rejected (the load-gate is fail-closed, so a typo must not save).
        let bad = ManifestDraft {
            version: "1.0.0".to_string(),
            min_smudgy_version: "soon".to_string(),
            ..ManifestDraft::default()
        };
        assert!(bad.to_manifest().is_err());
        // Blank (whitespace) -> no floor.
        let blank = ManifestDraft {
            version: "1.0.0".to_string(),
            min_smudgy_version: "  ".to_string(),
            ..ManifestDraft::default()
        };
        assert_eq!(
            blank.to_manifest().expect("projects").min_smudgy_version,
            None
        );
        // A version (trimmed) -> kept.
        let ok = ManifestDraft {
            version: "1.0.0".to_string(),
            min_smudgy_version: " 0.4.0 ".to_string(),
            ..ManifestDraft::default()
        };
        assert_eq!(
            ok.to_manifest()
                .expect("projects")
                .min_smudgy_version
                .as_deref(),
            Some("0.4.0")
        );
    }

    #[test]
    fn blank_list_rows_are_dropped_on_projection() {
        let draft = ManifestDraft {
            version: "1.0.0".to_string(),
            net: vec!["host:1".to_string(), "  ".to_string(), String::new()],
            ..ManifestDraft::default()
        };
        let manifest = draft.to_manifest().expect("projects");
        assert_eq!(manifest.permissions.net, vec!["host:1".to_string()]);
    }

    /// A draft with one parameter of a given kind, plus the supplied edits applied.
    fn one_param_draft(kind: ParamKind, prep: impl FnOnce(&mut ParamDraft)) -> ManifestDraft {
        let mut param = ParamDraft {
            key: "p".to_string(),
            kind,
            ..ParamDraft::default()
        };
        // Switching to a container seeds a sub-param; mirror that here.
        if kind.is_container() {
            param.fields.push(SubParamDraft::default());
        }
        prep(&mut param);
        ManifestDraft {
            version: "1.0.0".to_string(),
            params: vec![param],
            ..ManifestDraft::default()
        }
    }

    #[test]
    fn dropdown_requires_options_and_validates_default() {
        // No options -> rejected.
        assert!(
            one_param_draft(ParamKind::Dropdown, |_| {})
                .to_manifest()
                .is_err()
        );
        // A default that isn't a declared option -> rejected.
        let bad_default = one_param_draft(ParamKind::Dropdown, |p| {
            p.options = vec![OptionDraft {
                value: "a".to_string(),
                label: String::new(),
            }];
            p.default = "z".to_string();
        });
        assert!(bad_default.to_manifest().is_err());
        // Options + a matching default -> projects, with blank option rows dropped.
        let ok = one_param_draft(ParamKind::Dropdown, |p| {
            p.options = vec![
                OptionDraft {
                    value: "a".to_string(),
                    label: "Ay".to_string(),
                },
                OptionDraft {
                    value: String::new(),
                    label: "blank".to_string(),
                },
            ];
            p.default = "a".to_string();
        });
        let manifest = ok.to_manifest().expect("projects");
        let param = &manifest.params[0];
        assert_eq!(param.options.len(), 1);
        assert_eq!(param.default, Some(json!("a")));
    }

    #[test]
    fn duplicate_dropdown_option_values_are_rejected() {
        let draft = one_param_draft(ParamKind::Dropdown, |p| {
            p.options = vec![
                OptionDraft {
                    value: "a".to_string(),
                    label: String::new(),
                },
                OptionDraft {
                    value: "a".to_string(),
                    label: "again".to_string(),
                },
            ];
        });
        assert!(draft.to_manifest().is_err());
    }

    #[test]
    fn list_projects_its_single_element_with_a_default_key() {
        // A switched-to List seeds one blank element whose key normalizes to "value".
        let draft = one_param_draft(ParamKind::List, |p| {
            p.fields[0].kind = ParamKind::Number;
        });
        let manifest = draft.to_manifest().expect("projects");
        let param = &manifest.params[0];
        assert_eq!(param.kind, ParamKind::List);
        assert_eq!(param.fields.len(), 1);
        assert_eq!(param.fields[0].key, "value");
        assert_eq!(param.fields[0].kind, ParamKind::Number);
    }

    #[test]
    fn table_requires_a_keyed_column_and_rejects_duplicates() {
        // A single blank (keyless) column -> no usable columns -> rejected.
        assert!(
            one_param_draft(ParamKind::Table, |_| {})
                .to_manifest()
                .is_err()
        );
        // Duplicate column keys (case-insensitive) -> rejected.
        let dup = one_param_draft(ParamKind::Table, |p| {
            p.fields[0].key = "From".to_string();
            p.fields.push(SubParamDraft {
                key: "from".to_string(),
                ..SubParamDraft::default()
            });
        });
        assert!(dup.to_manifest().is_err());
        // One keyed column + a trailing blank one -> the blank is dropped, the keyed one kept.
        let ok = one_param_draft(ParamKind::Table, |p| {
            p.fields[0].key = "from".to_string();
            p.fields.push(SubParamDraft::default());
        });
        let manifest = ok.to_manifest().expect("projects");
        assert_eq!(manifest.params[0].fields.len(), 1);
        assert_eq!(manifest.params[0].fields[0].key, "from");
    }

    #[test]
    fn secret_is_dropped_for_non_string_kinds() {
        // A secret flag survives only on a String param (secrets are keyring strings).
        let number = one_param_draft(ParamKind::Number, |p| p.secret = true);
        assert!(!number.to_manifest().expect("projects").params[0].secret);
        let text = ManifestDraft {
            version: "1.0.0".to_string(),
            params: vec![ParamDraft {
                key: "tok".to_string(),
                kind: ParamKind::String,
                secret: true,
                ..ParamDraft::default()
            }],
            ..ManifestDraft::default()
        };
        assert!(text.to_manifest().expect("projects").params[0].secret);
    }

    #[test]
    fn duplicate_top_level_param_keys_are_rejected() {
        let draft = ManifestDraft {
            version: "1.0.0".to_string(),
            params: vec![
                ParamDraft {
                    key: "x".to_string(),
                    ..ParamDraft::default()
                },
                // Case-insensitive collision with the first.
                ParamDraft {
                    key: "X".to_string(),
                    ..ParamDraft::default()
                },
            ],
            ..ManifestDraft::default()
        };
        assert!(draft.to_manifest().is_err());
    }

    #[test]
    fn from_param_seeds_an_element_for_an_empty_container_manifest() {
        // A hand-edited manifest list with no element still becomes editable (a seeded sub-param).
        let param = PackageParameter {
            key: "items".to_string(),
            label: None,
            secret: false,
            required: false,
            kind: ParamKind::List,
            default: None,
            options: Vec::new(),
            fields: Vec::new(),
        };
        let draft = ParamDraft::from_param(&param);
        assert_eq!(draft.fields.len(), 1);
    }
}
