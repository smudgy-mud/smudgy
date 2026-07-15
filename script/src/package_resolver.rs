//! The `smudgy://` module scheme: shared-package resolution for the session isolate.
//!
//! Mirrors [`crate::npm_resolver`] in shape: `resolve()` keeps the request cheap and
//! synchronous, `load()` does the async fetch + a redirect to a canonical URL (see
//! [`crate::module_loader`]). A package is an ES module (entry) plus optional
//! sub-modules and a manifest (`smudgy.package.json`); "installing" a package is just
//! associating its specifier with a profile so it is imported on session start —
//! identical resolution, and the *same* shared-isolate instance, as a script doing
//! `import "smudgy://owner/name"`. See `DESIGN.md`.
//!
//! "install == import == same instance" holds only *within* an isolate (see
//! `script/PACKAGE-ISOLATES.md`). A sandboxed installed package resolves into its own
//! isolate (own module cache), so it is a *distinct* instance from a copy a trusted
//! script imports directly into the main isolate.
//!
//! ## Two URL spaces (why)
//!
//! The user-facing specifier `smudgy://owner/name[/subpath]` is parsed by hand (not
//! `url::Url`): `resolve()` and `load()` work in two distinct, path-based URL spaces —
//! both with an empty authority (`scheme:///...`, like `file:///`) so `Url::join` and
//! [`deno_core::resolve_import`] behave for relative-import resolution (`./util` joins)
//! and deno's module-cache dedup:
//!
//! - **marker** (`smudgy:///owner/name[/subpath]`) — version-less, what
//!   `resolve()` returns for a `smudgy://…` import. It carries the package coordinate in
//!   a form `url` round-trips losslessly, and `load()` decodes it back.
//! - **canonical** (`smudgy-pkg:///owner/name/version/module-file`) — the
//!   version-pinned module identity `load()` redirects to. Because the resolved
//!   version is baked into the path, two imports of the same package resolve to the
//!   *same* canonical URL (one instance); relative imports inside a package resolve
//!   against it and stay within the package.

use std::collections::HashMap;
use std::rc::Rc;

use deno_core::error::ModuleLoaderError;
use deno_core::{ModuleSource, ModuleSourceCode, ModuleSpecifier};
use serde::{Deserialize, Serialize};

use crate::interop_extract::InteropKind;

/// URL scheme for the version-less marker `resolve()` emits for `smudgy://` imports.
pub const MARKER_SCHEME: &str = "smudgy";
/// URL scheme for the version-pinned canonical module identity `load()` redirects to.
pub const CANONICAL_SCHEME: &str = "smudgy-pkg";
/// URL scheme for the synthesized per-importer `smudgy:params` virtual module: each
/// importing package gets a module whose `get` is bound to *its own* param namespace.
pub const PARAMS_SCHEME: &str = "smudgy-params";
/// URL scheme for the synthesized per-importer `smudgy:core` virtual module: each importing
/// module gets a module whose creation functions (`createAlias`/`createTrigger`/…) are bound
/// to *its own* provenance, so the automations it creates are attributed to it.
pub const CORE_SCHEME: &str = "smudgy-core";
/// URL scheme for the synthesized `smudgy:widgets` virtual module (the script-driven UI
/// surface) and its `smudgy:widgets/jsx-runtime`. Per-importer like [`CORE_SCHEME`] so a
/// package's `createWidget` carries its own provenance.
pub const WIDGETS_SCHEME: &str = "smudgy-widgets";
/// URL scheme for the synthesized `smudgy:state/<producer>` consumer modules: host-built
/// state-handle stubs over a producer's declared handles, extracted statically from its
/// entry source — importing one never evaluates the producer (interop.md §4).
pub const STATE_SCHEME: &str = "smudgy-state";
/// URL scheme for the synthesized `smudgy:events/<producer>` consumer modules; the event
/// twin of [`STATE_SCHEME`], also serving the platform catalogs (`smudgy:events/sys`,
/// `smudgy:events/map`).
pub const EVENTS_SCHEME: &str = "smudgy-events";
/// URL scheme for the synthesized `smudgy:procedures/<producer>` consumer modules; the
/// procedure twin of [`STATE_SCHEME`] (interop.md §6). No platform procedures exist.
pub const PROCEDURES_SCHEME: &str = "smudgy-procedures";

/// A package coordinate without version or subpath — the version-cache and lockfile
/// key. `owner` is the publisher's (globally unique) nickname; `name` is unique
/// within that owner's namespace.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageKey {
    pub owner: String,
    pub name: String,
}

impl PackageKey {
    /// This key with both segments ASCII-folded — the fold the interop home registry and
    /// every home comparison use (interop.md §2's uniform structural fold).
    #[must_use]
    pub fn folded(&self) -> Self {
        Self {
            owner: self.owner.to_ascii_lowercase(),
            name: self.name.to_ascii_lowercase(),
        }
    }

    /// The package-level user specifier, e.g. `smudgy://wbk/mapper`.
    #[must_use]
    pub fn to_user_specifier(&self) -> String {
        format!("{MARKER_SCHEME}://{}/{}", self.owner, self.name)
    }
}

/// The importing package *instance* a referrer-aware import comes from: its
/// [`PackageKey`] plus its concrete resolved version. The version is load-bearing — two
/// coexisting versions of the same importer (e.g. `app@1` and `app@2`) lock different
/// dependency versions, so the referrer must distinguish them, otherwise their transitive
/// imports collapse to a single instance. Carried in the marker URL
/// and used to key the provider's locked-dep map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferrerRef {
    pub key: PackageKey,
    pub version: String,
}

/// A parsed user specifier `smudgy://owner/name[/subpath]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmudgySpecifier {
    pub owner: String,
    pub name: String,
    /// Module within a multi-module package; `None` addresses the entry module.
    pub subpath: Option<String>,
    /// The package instance whose module declared this import (its canonical referrer), if
    /// any. Carried in the marker URL so the provider selects the target's version from
    /// *this importer's* locked deps (referrer-aware resolution). `None` for a
    /// user-typed or top-level import (resolves via the lockfile / latest).
    pub referrer: Option<ReferrerRef>,
}

impl SmudgySpecifier {
    /// Parse `smudgy://owner/name[/subpath]`. Parsed by hand (not `url::Url`) so the
    /// marker/canonical URL spaces stay path-based and round-trip losslessly.
    ///
    /// # Errors
    /// Returns [`SmudgySpecifierError`] for a missing scheme, an empty component, or an
    /// unsafe subpath (`..`/backslash).
    pub fn parse(raw: &str) -> Result<Self, SmudgySpecifierError> {
        let rest = raw
            .strip_prefix("smudgy://")
            .ok_or(SmudgySpecifierError::MissingScheme)?;

        // owner / name [/ subpath...]
        let (owner, path) = rest
            .split_once('/')
            .ok_or(SmudgySpecifierError::EmptyComponent("name"))?;
        if owner.is_empty() {
            return Err(SmudgySpecifierError::EmptyComponent("owner"));
        }

        let path = path.trim_end_matches('/');
        let (name, subpath) = match path.split_once('/') {
            Some((name, sub)) => (name, Some(sub)),
            None => (path, None),
        };
        if name.is_empty() {
            return Err(SmudgySpecifierError::EmptyComponent("name"));
        }
        let subpath = match subpath {
            Some(sub) if !sub.is_empty() => {
                validate_subpath(sub)?;
                Some(sub.to_string())
            }
            _ => None,
        };

        Ok(Self {
            owner: owner.to_string(),
            name: name.to_string(),
            subpath,
            referrer: None,
        })
    }

    /// The package coordinate (drops the subpath).
    #[must_use]
    pub fn package_key(&self) -> PackageKey {
        PackageKey {
            owner: self.owner.clone(),
            name: self.name.clone(),
        }
    }

    /// Tag this import with the importing package instance (referrer-aware resolution).
    #[must_use]
    pub fn with_referrer(mut self, key: PackageKey, version: impl Into<String>) -> Self {
        self.referrer = Some(ReferrerRef {
            key,
            version: version.into(),
        });
        self
    }

    /// The importing package instance, if this import carries a referrer.
    #[must_use]
    pub fn referrer(&self) -> Option<&ReferrerRef> {
        self.referrer.as_ref()
    }

    /// Re-canonicalized user specifier (normalized, no trailing slash).
    #[must_use]
    pub fn to_user_specifier(&self) -> String {
        let mut out = format!("smudgy://{}/{}", self.owner, self.name);
        if let Some(sub) = &self.subpath {
            out.push('/');
            out.push_str(sub);
        }
        out
    }

    /// The version-less marker URL `resolve()` returns. Path-based + empty-authority so
    /// `url` round-trips it losslessly; [`Self::from_marker_url`] is the inverse. When a
    /// referrer is attached it is encoded as a `?referrer=owner%2Fname%40version`
    /// query so deno keys the module on `(target, referrer-instance)`: the same target
    /// imported from the same importer instance resolves to one marker (one selection),
    /// while two coexisting versions of the importer select independently.
    #[must_use]
    pub fn to_marker_url(&self) -> ModuleSpecifier {
        let mut path = format!("/{}/{}", self.owner, self.name);
        if let Some(sub) = &self.subpath {
            path.push('/');
            path.push_str(sub);
        }
        let mut url = ModuleSpecifier::parse(&format!("{MARKER_SCHEME}://{path}"))
            .expect("marker URL is well-formed for validated components");
        if let Some(referrer) = &self.referrer {
            url.query_pairs_mut().append_pair(
                "referrer",
                &format!(
                    "{}/{}@{}",
                    referrer.key.owner, referrer.key.name, referrer.version
                ),
            );
        }
        url
    }

    /// Recover a specifier (with any attached referrer) from a [`Self::to_marker_url`]
    /// output.
    #[must_use]
    pub fn from_marker_url(url: &ModuleSpecifier) -> Option<Self> {
        if url.scheme() != MARKER_SCHEME {
            return None;
        }
        let mut segments = url.path_segments()?;
        let owner = segments.next()?.to_string();
        let name = segments.next()?.to_string();
        let rest: Vec<&str> = segments.filter(|s| !s.is_empty()).collect();
        let subpath = if rest.is_empty() {
            None
        } else {
            Some(rest.join("/"))
        };
        if owner.is_empty() || name.is_empty() {
            return None;
        }
        let referrer = url
            .query_pairs()
            .find(|(key, _)| key == "referrer")
            .and_then(|(_, value)| parse_referrer(&value));
        Some(Self {
            owner,
            name,
            subpath,
            referrer,
        })
    }
}

/// Parse a marker `referrer=` query value (`owner/name@version`) back into a
/// [`ReferrerRef`]. Returns `None` for a malformed value (treated as no referrer).
fn parse_referrer(raw: &str) -> Option<ReferrerRef> {
    let (owner_name, version) = raw.rsplit_once('@')?;
    let (owner, name) = owner_name.rsplit_once('/')?;
    if owner.is_empty() || name.is_empty() || version.is_empty() {
        return None;
    }
    Some(ReferrerRef {
        key: PackageKey {
            owner: owner.to_string(),
            name: name.to_string(),
        },
        version: version.to_string(),
    })
}

/// A parsed `smudgy://` dependency from a manifest's `dependencies` list: a package
/// coordinate plus an optional semver range. Unlike an *import* specifier (which never
/// carries a version — the version comes from the importing package's locked deps or the
/// lockfile), a *dependency declaration* may pin its range with `@`:
/// `smudgy://owner/name@^1.2`. A range-less dependency means "any version" (resolved
/// to whatever the dependency tree locks). The raw range string is kept unparsed here;
/// `core`'s resolution engine parses + matches it (this crate stays semver-policy-free).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDependency {
    pub key: PackageKey,
    /// Raw semver range (`^1.2`, `=1.0.0`, …); `None` = unconstrained.
    pub range: Option<String>,
}

impl PackageDependency {
    /// Parse one `dependencies` entry. Returns `None` if it isn't a `smudgy://`
    /// dependency (jsr:/npm:/relative deps are resolved by their own stacks);
    /// `Some(Err(_))` if it is a `smudgy://` entry but malformed.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Result<Self, SmudgySpecifierError>> {
        if !raw.starts_with("smudgy://") {
            return None;
        }
        // The only `@` in a well-formed entry separates the trailing range — the owner
        // handle and name never contain one — so split on the last `@`. (A range itself,
        // e.g. `^1.2 || ^2`, contains no `@`.)
        let (spec_part, range) = match raw.rsplit_once('@') {
            Some((spec, range)) if !range.is_empty() => (spec, Some(range.to_string())),
            _ => (raw, None),
        };
        Some(SmudgySpecifier::parse(spec_part).map(|spec| Self {
            // A dependency is package-level: drop any subpath.
            key: spec.package_key(),
            range,
        }))
    }
}

/// Coordinates recovered from a [`CANONICAL_SCHEME`] URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalCoords {
    pub key: PackageKey,
    pub version: String,
    /// Concrete module file within the package, e.g. `index.js` or `lib/util.ts`.
    pub module_subpath: String,
}

/// Build the version-pinned canonical module URL for a concrete module file.
#[must_use]
pub fn canonical_url(key: &PackageKey, version: &str, module_subpath: &str) -> ModuleSpecifier {
    let module_subpath = module_subpath.trim_start_matches('/');
    let path = format!(
        "/{}/{}/{}/{}",
        key.owner, key.name, version, module_subpath
    );
    ModuleSpecifier::parse(&format!("{CANONICAL_SCHEME}://{path}"))
        .expect("canonical URL is well-formed for validated components")
}

/// Inverse of [`canonical_url`].
#[must_use]
pub fn parse_canonical(url: &ModuleSpecifier) -> Option<CanonicalCoords> {
    if url.scheme() != CANONICAL_SCHEME {
        return None;
    }
    let mut segments = url.path_segments()?;
    let owner = segments.next()?.to_string();
    let name = segments.next()?.to_string();
    let version = segments.next()?.to_string();
    let module_subpath = segments.collect::<Vec<_>>().join("/");
    if owner.is_empty()
        || name.is_empty()
        || version.is_empty()
        || module_subpath.is_empty()
    {
        return None;
    }
    Some(CanonicalCoords {
        key: PackageKey { owner, name },
        version,
        module_subpath,
    })
}

/// The per-importer `smudgy:params` virtual-module URL. `Some(key)` binds it to that
/// package's param namespace; `None` (a non-package importer — a user module / top-level)
/// binds to no package, so its `get` returns `undefined`. Two imports from the *same*
/// package produce the same URL (one module instance); the version is intentionally
/// dropped — params are per-package, not per-version.
#[must_use]
pub fn params_module_url(importer: Option<&PackageKey>) -> ModuleSpecifier {
    let path = match importer {
        Some(key) => format!("/{}/{}", key.owner, key.name),
        None => "/".to_string(),
    };
    ModuleSpecifier::parse(&format!("{PARAMS_SCHEME}://{path}"))
        .expect("params URL is well-formed for validated components")
}

/// The importing package a [`params_module_url`] addresses: `Some(Some(key))` for a
/// package, `Some(None)` for the no-package module, `None` if `url` isn't a params URL.
#[must_use]
pub fn parse_params_url(url: &ModuleSpecifier) -> Option<Option<PackageKey>> {
    if url.scheme() != PARAMS_SCHEME {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    match segments.as_slice() {
        [owner, name] => Some(Some(PackageKey {
            owner: (*owner).to_string(),
            name: (*name).to_string(),
        })),
        [] => Some(None),
        _ => None,
    }
}

/// The deno [`ModuleType`](deno_core::ModuleType) for a module file path.
#[must_use]
pub fn module_type_for(path: &str) -> deno_core::ModuleType {
    if path.ends_with(".json") {
        deno_core::ModuleType::Json
    } else {
        deno_core::ModuleType::JavaScript
    }
}

// ---------------------------------------------------------------------------
// Manifest (`smudgy.package.json`)
// ---------------------------------------------------------------------------

/// A package manifest. Parsed and recorded immediately; the `permissions` block is **enforced**
/// per sandboxed package isolate: the deno-native fields (`net`/`read`/`write`/`env`)
/// (`script/PACKAGE-ISOLATES-ENFORCEMENT.md`) — the isolate factory builds a restricted
/// `PermissionsContainer` from their closure union — and the `smudgy` op-capability block
/// (`script/PACKAGE-ISOLATES-OP-CAPABILITIES.md`), which gates smudgy's own ops via a
/// per-isolate `SmudgyGrants` built from the consented closure union. Lives in this crate (not
/// `core`/`map`) so the loader and the provider share it without a crate cycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageManifest {
    pub version: String,
    /// A short, human-readable description (surfaced in Discover search + the package page).
    /// The package's *name* is intentionally absent: it is implied by the package's folder name
    /// (and the published namespace), so it can never drift from it. A legacy `name` key in an
    /// existing manifest is tolerated (ignored) — there is no `deny_unknown_fields`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Entry module file (e.g. `index.ts`). When absent, the entry is resolved by
    /// trying conventional `index.*`/`mod.*` files (see [`ResolvedPackage::resolve_module`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// The minimum smudgy version this package runs on (a plain semver version, e.g.
    /// `"0.4.0"`), for packages that use script APIs newer than some clients have. Absent =
    /// runs anywhere. **Enforced** in `smudgy_core`, not here (this crate stays
    /// semver-policy-free): the install flow refuses to install past the floor, and the
    /// engine's resolution/version-capping refuses (or holds back) a version whose closure
    /// demands a newer smudgy. Compared against the running release with any build-channel
    /// prerelease suffix dropped — a dev/RC build of `X.Y.Z` counts as `X.Y.Z`. Older
    /// clients that predate the field ignore it (no `deny_unknown_fields`), so it is a
    /// guardrail for current clients, not a security boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_smudgy_version: Option<String>,
    /// Dependency specifiers: `jsr:…`, `npm:…`, `smudgy://…`, or relative.
    ///
    /// A `dependencies` entry means "I **import** this code into my isolate". A `smudgy://`
    /// dependency is import-gated and version-locked to *this* package's range. Contrast
    /// [`requires`](Self::requires), which means "this other package must be installed and running
    /// on its own; I consume it over the event bus + its types" and is never imported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    /// Required packages: `smudgy://owner/name[@range]` only. Each becomes its **own top-level
    /// install root** (its own version/update mode, one shared running instance), surfaced for
    /// co-installation when this package is installed. Distinct from [`dependencies`](Self::dependencies):
    /// a `requires` is **not imported** — it is consumed over the event bus and its types. A bare
    /// (range-less) entry tracks the latest. See `script/REQUIRED-PACKAGES.md`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// Aligned MUD hosts: projected into the server's discovery alignment when the package is
    /// published, so it surfaces under these MUDs in Discover. Empty = host-agnostic. Advisory
    /// only — never gates installing the package.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    /// Parameters/secrets the package declares (configured at install time). Accepts the
    /// legacy `options` key as an alias.
    #[serde(default, alias = "options", skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<PackageParameter>,
    /// Requested permissions, enforced per sandboxed package isolate, unioned across the
    /// dependency closure: the deno-native fields
    /// (`script/PACKAGE-ISOLATES-ENFORCEMENT.md`) and the `smudgy` op-capability block
    /// (`script/PACKAGE-ISOLATES-OP-CAPABILITIES.md`).
    #[serde(default, skip_serializing_if = "PackagePermissions::is_empty")]
    pub permissions: PackagePermissions,
    /// Whether **other-owner** packages may `import` this package's modules. When `false`, the
    /// module loader rejects a cross-owner `smudgy://` import of this package (own-owner siblings
    /// are exempt), enforcing events-only consumption for pure libraries and closing the
    /// secret-leak of a third party importing a private package. Default `true`. See
    /// `script/REQUIRED-PACKAGES.md`. (Note: this does not gate `op_smudgy_param_get` — that
    /// cross-package read is a separate concern.)
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub importable: bool,
}

/// serde default for [`PackageManifest::importable`]: packages are importable unless they opt out.
fn default_true() -> bool {
    true
}

/// `skip_serializing_if` for a default-`true` bool: omit it from serialized manifests when `true`.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_true(value: &bool) -> bool {
    *value
}

impl PackageManifest {
    /// Parse a `smudgy.package.json` body.
    ///
    /// # Errors
    /// Returns the underlying [`serde_json::Error`] on malformed JSON.
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// The `smudgy://` dependencies declared in `dependencies`, parsed with their ranges.
    /// Non-`smudgy://` deps (jsr:/npm:/relative) and malformed `smudgy://` entries are
    /// skipped — the loader's dep-gating surfaces a malformed/undeclared dep at import.
    #[must_use]
    pub fn smudgy_dependencies(&self) -> Vec<PackageDependency> {
        self.dependencies
            .iter()
            .filter_map(|dep| match PackageDependency::parse(dep) {
                Some(Ok(parsed)) => Some(parsed),
                _ => None,
            })
            .collect()
    }

    /// The `requires` entries, parsed with their ranges. A `requires` entry is always a
    /// `smudgy://` package — non-`smudgy://` and malformed entries are skipped here and surfaced
    /// at publish/install. Distinct from [`smudgy_dependencies`](Self::smudgy_dependencies): these
    /// are co-installed as top-level roots, not imported.
    #[must_use]
    pub fn smudgy_requires(&self) -> Vec<PackageDependency> {
        self.requires
            .iter()
            .filter_map(|dep| match PackageDependency::parse(dep) {
                Some(Ok(parsed)) => Some(parsed),
                _ => None,
            })
            .collect()
    }
}

/// A configurable parameter (or secret) a package declares.
///
/// Scalar kinds (`String`/`Bool`/`Number`/`Dropdown`) store a single value. Container kinds
/// (`List`/`Table`) describe their shape with [`fields`](Self::fields): a `List` declares exactly
/// one element spec, a `Table` declares one spec per column. A `Dropdown` (scalar or nested) enumerates
/// its choices in [`options`](Self::options). Every sub-spec in `fields` is itself a scalar parameter —
/// containers never nest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageParameter {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Secret values go to the OS keychain; non-secret to profile settings. Only meaningful for
    /// scalar string params (secrets are stored as keyring strings).
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(rename = "type", default)]
    pub kind: ParamKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// The selectable choices for a [`ParamKind::Dropdown`]. Empty for every other kind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ParamOption>,
    /// Sub-parameter specs: the single element spec of a [`ParamKind::List`], or one spec per column
    /// of a [`ParamKind::Table`]. Each is a scalar parameter (never a `List`/`Table` — containers do
    /// not nest). Empty for scalar params.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<PackageParameter>,
}

/// One selectable choice of a [`ParamKind::Dropdown`] parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamOption {
    /// The value stored (and handed to scripts) when this choice is selected.
    pub value: String,
    /// The text shown in the dropdown. Falls back to [`value`](Self::value) when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ParamOption {
    /// The user-facing label, falling back to the raw value when none was given.
    #[must_use]
    pub fn display_label(&self) -> &str {
        self.label.as_deref().filter(|l| !l.is_empty()).unwrap_or(&self.value)
    }
}

/// The value type of a [`PackageParameter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamKind {
    #[default]
    String,
    Bool,
    Number,
    /// A single value chosen from a declared set of [`PackageParameter::options`].
    Dropdown,
    /// A variable-length list of a single element type ([`PackageParameter::fields`]`[0]`).
    List,
    /// A grid whose columns are declared by [`PackageParameter::fields`]; each row is an object.
    Table,
}

impl ParamKind {
    /// Whether this kind holds a variable number of sub-values (`List`/`Table`) described by
    /// [`PackageParameter::fields`], as opposed to a single scalar value.
    #[must_use]
    pub fn is_container(self) -> bool {
        matches!(self, ParamKind::List | ParamKind::Table)
    }

    /// Whether this kind holds a single value (everything that is not a container).
    #[must_use]
    pub fn is_scalar(self) -> bool {
        !self.is_container()
    }
}

/// How far outside the smudgy ecosystem a package may download code to run — the `import`
/// permission (`PACKAGE-ISOLATES-ENFORCEMENT.md`). A single escalating choice, NOT a host list:
/// each level is a strict superset of the one before, so the dependency-closure union is the **max**
/// and "fits the consent" is `<=`. Enforced at the module loader (the loader calls `allows_import`
/// per remote import) — deno's own `allow_import` is inert in this stack. `smudgy://` package
/// imports are governed by the manifest `dependencies` closure, not by this, so they load at every
/// level (including `None`).
///
/// Wire form: a lowercase string (`"none"`/`"registries"`/`"any"`); absent ⇒ `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportPolicy {
    /// Only the smudgy ecosystem (`smudgy://` deps + the built-in `smudgy:` modules) — no
    /// `npm:`/`jsr:`/`https:` downloads. The safe default.
    #[default]
    None,
    /// Adds the public registries: `npm:` and `jsr:` imports (and the `https://jsr.io` sub-modules
    /// jsr resolves to). Still no arbitrary `https:`/`http:` host.
    Registries,
    /// Adds arbitrary `https:`/`http:` imports from any host, on top of the registries.
    Any,
}

/// The host `jsr:` imports resolve to (`https://jsr.io/…`). At [`ImportPolicy::Registries`] the
/// loader must let a jsr module load its own `https://jsr.io` sub-modules even though arbitrary
/// `https:` is otherwise denied — so this is the single `https:` host allowed below `Any`.
const JSR_REGISTRY_HOST: &str = "jsr.io";

impl ImportPolicy {
    /// Whether this is the default (no external imports) — drives `skip_serializing_if`.
    #[must_use]
    pub fn is_none(&self) -> bool {
        matches!(self, ImportPolicy::None)
    }

    /// Whether this policy permits importing a module of `scheme` from `host` (`host` matters only
    /// for `http`/`https`). The module loader calls this in `resolve()` to gate every remote import:
    ///
    /// - `npm:` / `jsr:` — allowed at `Registries` and above.
    /// - `http:` / `https:` — allowed at `Any`; at `Registries`, only the `jsr.io` CDN (so a jsr
    ///   package's own `https://jsr.io` sub-modules load); never at `None`.
    /// - any other scheme (`smudgy://`, `smudgy-pkg:`, `file:`, …) is not an external-code download,
    ///   so it is always permitted here (it is gated elsewhere — e.g. `smudgy://` dep-gating).
    #[must_use]
    pub(crate) fn allows_import(self, scheme: &str, host: &str) -> bool {
        match scheme {
            "npm" | "jsr" => self >= ImportPolicy::Registries,
            "http" | "https" => match self {
                ImportPolicy::Any => true,
                ImportPolicy::Registries => host.eq_ignore_ascii_case(JSR_REGISTRY_HOST),
                ImportPolicy::None => false,
            },
            _ => true,
        }
    }
}

/// Requested permissions, enforced per sandboxed package isolate. The deno-native fields
/// (`net`/`read`/`write`/`env`, `script/PACKAGE-ISOLATES-ENFORCEMENT.md`) are each a
/// deny-by-default allowlist: an absent/empty field denies that kind entirely. Value formats:
/// `net` entries are `host:port` (only that port) or bare `host` (any port); `read`/`write` are
/// paths whose directory grants cover the whole subtree (and may use the `$DATA` placeholder,
/// host-expanded before enforcement); `env` are exact var names.
///
/// `import` is a tri-state [`ImportPolicy`] (not a host list) and a separate axis from `net`: `net`
/// governs runtime network *connections* (opening sockets / `fetch`, where the package could send
/// your data out), while `import` governs *downloading code to run* (`None` → smudgy:// only,
/// `Registries` → + npm/jsr, `Any` → + arbitrary https/http). Granting one never implies the other.
///
/// The `smudgy` field (`script/PACKAGE-ISOLATES-OP-CAPABILITIES.md`) carries the op-capability
/// set — smudgy's own ops (send/echo/automations/display/mapper/widgets), gated per isolate from
/// the consented set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackagePermissions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub net: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// How far outside the smudgy ecosystem this package may download code to run
    /// ([`ImportPolicy`]): `None` (default) = smudgy:// only; `Registries` = + npm/jsr; `Any` = +
    /// arbitrary https/http. Enforced at the module loader, not via deno's permission container.
    #[serde(default, skip_serializing_if = "ImportPolicy::is_none")]
    pub import: ImportPolicy,
    /// The smudgy op-capability set. Absent ⇒ every smudgy capability denied.
    #[serde(default, skip_serializing_if = "SmudgyCapabilities::is_empty")]
    pub smudgy: SmudgyCapabilities,
}

impl PackagePermissions {
    /// Whether no permissions are requested (drives `skip_serializing_if`).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.net.is_empty()
            && self.read.is_empty()
            && self.write.is_empty()
            && self.env.is_empty()
            && self.import.is_none()
            && self.smudgy.is_empty()
    }

    /// Fold another package's permissions into this one as a per-field set-union
    /// (deduped, first-seen order). This is the dependency-closure union a sandboxed
    /// package isolate enforces: its container grants the union of `permissions` across
    /// the root package and every transitive `smudgy://` dependency
    /// (`script/PACKAGE-ISOLATES-ENFORCEMENT.md`). Union is monotonic, so folding the
    /// same package twice (reachable via multiple paths, or coexisting versions) is a
    /// no-op.
    pub fn merge(&mut self, other: &Self) {
        merge_dedup(&mut self.net, &other.net);
        merge_dedup(&mut self.read, &other.read);
        merge_dedup(&mut self.write, &other.write);
        merge_dedup(&mut self.env, &other.env);
        // `import` is an ordered lattice (None < Registries < Any); the closure union is the max.
        self.import = self.import.max(other.import);
        self.smudgy.merge(&other.smudgy);
    }

    /// The per-field additions in `self` (a newly-resolved closure union) not already **covered**
    /// by `baseline` (the consented union — `∅`/`Self::default()` when never consented) — the
    /// "additionally wants" delta an update re-prompt shows
    /// (`script/PACKAGE-ISOLATES-CONSENT-TRUST.md`). Pure + side-effect-free.
    ///
    /// "Covered" is the same per-field containment [`is_within`](Self::is_within) uses, mirroring
    /// deno's permission descriptors (`PACKAGE-ISOLATES-ENFORCEMENT.md`): a consented bare `net`
    /// host covers any `host:port` on it (an exact `host:port` only itself), a consented
    /// `read`/`write` path covers its whole subtree (path-prefix), and `env` is an exact (trimmed)
    /// name match. So a re-ordered, re-cased, narrower-port, or deeper-path entry is not a spurious
    /// "added". An all-empty result means the new union only shrank or stayed within the grant (no
    /// new exposure → auto-accept); a non-empty field is what the user must newly consent to.
    /// Returned entries keep their original new-union spelling so the re-prompt displays them
    /// verbatim.
    #[must_use]
    pub fn added_since(&self, baseline: &Self) -> Self {
        Self {
            net: added_entries(&self.net, &baseline.net, PermField::Net),
            read: added_entries(&self.read, &baseline.read, PermField::Path),
            write: added_entries(&self.write, &baseline.write, PermField::Path),
            env: added_entries(&self.env, &baseline.env, PermField::Env),
            // The "additionally wants" for `import` is the new level only when it escalates past the
            // consented one (a higher level is a superset); otherwise nothing new.
            import: if self.import > baseline.import { self.import } else { ImportPolicy::None },
            smudgy: self.smudgy.added_since(&baseline.smudgy),
        }
    }

    /// Whether `self` grants **nothing beyond** `ceiling` — every entry of `self` is **covered** by
    /// some entry of `ceiling` under the same per-field containment deno's permission descriptors
    /// use (`PACKAGE-ISOLATES-ENFORCEMENT.md`): a consented bare `net` host covers any
    /// `host:port` on it (an exact `host:port` covers only that port); a consented `read`/`write`
    /// path covers its whole subtree; `env` is an exact (trimmed) name match. The permission-aware
    /// resolver uses this to pick the highest package version whose **closure** union fits the
    /// user's consented grant (`script/PACKAGE-ISOLATES-CONSENT-TRUST.md`): a version is loadable
    /// iff its closure union `is_within` the consented union; otherwise it demands more than was
    /// granted and is skipped (the package stays at a fitting version, or refuses to load if none
    /// fits). Mirroring the descriptor containment — not exact string membership — keeps this gate
    /// in agreement with what the package's restricted [`crate::PermissionsContainer`] will allow.
    #[must_use]
    pub fn is_within(&self, ceiling: &Self) -> bool {
        self.net.iter().all(|e| covered_by(PermField::Net, &ceiling.net, e))
            && self.read.iter().all(|e| covered_by(PermField::Path, &ceiling.read, e))
            && self.write.iter().all(|e| covered_by(PermField::Path, &ceiling.write, e))
            && self.env.iter().all(|e| covered_by(PermField::Env, &ceiling.env, e))
            // `import` fits iff it asks for no higher level than the ceiling grants.
            && self.import <= ceiling.import
            && self.smudgy.is_within(&ceiling.smudgy)
    }
}

/// The **smudgy op-capability set** a package declares under `permissions.smudgy`, enforced per
/// sandboxed package isolate (`script/PACKAGE-ISOLATES-OP-CAPABILITIES.md`). Unlike
/// the deno-native `net`/`read`/`write`/`env` allowlists, these gate smudgy's *own* ops — the
/// capabilities deno's permission model knows nothing about (sending to the game, changing what you
/// see, creating automations, the mapper, widgets). Each is a simple boolean: a capability is either
/// requested or not (no allowlist of targets), so containment is field-wise boolean implication.
///
/// Stored normalized as booleans but serialized in the manifest's array-of-tokens form via
/// [`SmudgyCapabilitiesWire`] (so `{ "session": ["send", "echo"], "display": ["change"] }` round-trips
/// to/from the lockfile's `consented_permissions`). `mapper: ["write"]` implies `read` (normalized at
/// parse). Unknown tokens are ignored (forward-compatible: a future capability this client doesn't
/// know can't be enforced, and the op for it doesn't exist here anyway).
// A flat set of independent capability flags (the taxonomy), not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "SmudgyCapabilitiesWire", into = "SmudgyCapabilitiesWire")]
pub struct SmudgyCapabilities {
    /// `automations: ["aliases"]` — create aliases (`createAlias` + `set_alias_enabled`).
    pub create_aliases: bool,
    /// `automations: ["triggers"]` — create triggers (`createTrigger` + `set_trigger_enabled`).
    pub create_triggers: bool,
    /// `session: ["send"]` — send commands as if typed (through the user's aliases).
    pub send: bool,
    /// `session: ["send-direct"]` — send raw commands, bypassing the user's aliases.
    pub send_direct: bool,
    /// `session: ["echo"]` — write text to the user's screen.
    pub echo: bool,
    /// `session: ["reach-others"]` — enumerate + act on the user's *other* connected sessions.
    pub reach_others: bool,
    /// `display: ["change"]` — gag/insert/replace/highlight/remove game text (the deception risk).
    pub change_display: bool,
    /// `mapper: ["read"]` — read the user's maps (implied by `mapper_write`).
    pub mapper_read: bool,
    /// `mapper: ["write"]` — change the user's maps (implies `mapper_read`).
    pub mapper_write: bool,
    /// `widgets: ["create"]` — create & change on-screen widgets (`iced_jsx`).
    pub widgets: bool,
    /// `interop: ["read"]` — consume the cross-package interop surface: read/watch session-store
    /// state and subscribe to events (client `sys:`/`map:` + other packages). The legacy
    /// `events: ["subscribe"]` token aliases onto this at parse.
    pub interop_read: bool,
    /// `interop: ["write"]` — produce on the cross-package interop surface: publish session-store
    /// state and emit events (own namespace only). The legacy `events: ["emit"]` token aliases
    /// onto this at parse.
    pub interop_write: bool,
    /// `panes: ["create"]` — create/close/write session output panes and route lines into them.
    pub panes: bool,
    /// `gmcp: ["send"]` — send GMCP messages to the game and manage GMCP modules
    /// (`gmcp.send`/`enableModule`/`disableModule`/`mergeKeys`, `docs/gmcp-plan.md` §6.3).
    /// Outbound GMCP can drive server-side state, so it is the moral equivalent of
    /// `session: ["send"]` and deliberately rides with neither `interop` grant.
    pub gmcp_send: bool,
}

impl SmudgyCapabilities {
    /// Every capability granted — the runtime grant the main/trusted isolate gets, and the "full
    /// smudgy access" a consent record carries when the user grants everything requested.
    #[must_use]
    pub fn all() -> Self {
        Self {
            create_aliases: true,
            create_triggers: true,
            send: true,
            send_direct: true,
            echo: true,
            reach_others: true,
            change_display: true,
            mapper_read: true,
            mapper_write: true,
            widgets: true,
            interop_read: true,
            interop_write: true,
            panes: true,
            gmcp_send: true,
        }
    }

    /// Whether no capability is requested (drives `skip_serializing_if`, and the all-smudgy-denied
    /// sandbox default).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Fold another package's capabilities in as a per-field OR — the dependency-closure union a
    /// sandboxed isolate enforces (`PACKAGE-ISOLATES-OP-CAPABILITIES.md`, alongside the deno-native
    /// [`PackagePermissions::merge`]). Monotonic, so folding twice is a no-op.
    pub fn merge(&mut self, other: &Self) {
        self.create_aliases |= other.create_aliases;
        self.create_triggers |= other.create_triggers;
        self.send |= other.send;
        self.send_direct |= other.send_direct;
        self.echo |= other.echo;
        self.reach_others |= other.reach_others;
        self.change_display |= other.change_display;
        self.mapper_read |= other.mapper_read;
        self.mapper_write |= other.mapper_write;
        self.widgets |= other.widgets;
        self.interop_read |= other.interop_read;
        self.interop_write |= other.interop_write;
        self.panes |= other.panes;
        self.gmcp_send |= other.gmcp_send;
    }

    /// Whether `self` requests **nothing beyond** `ceiling` — every capability `self` wants is also
    /// granted by `ceiling` (field-wise boolean implication). The smudgy-capability analogue of the
    /// per-field containment [`PackagePermissions::is_within`] uses for `net`/`read`/`write`/`env`, so
    /// the permission-aware resolver caps a version on its smudgy asks too: a version that newly wants
    /// `send-direct` the user never granted is blocked, exactly like a widening `net`
    /// (`PACKAGE-ISOLATES-OP-CAPABILITIES.md`). Because `mapper_write` implies `mapper_read` at
    /// parse, a write grant covers a read ask.
    #[must_use]
    pub fn is_within(&self, ceiling: &Self) -> bool {
        (!self.create_aliases || ceiling.create_aliases)
            && (!self.create_triggers || ceiling.create_triggers)
            && (!self.send || ceiling.send)
            && (!self.send_direct || ceiling.send_direct)
            && (!self.echo || ceiling.echo)
            && (!self.reach_others || ceiling.reach_others)
            && (!self.change_display || ceiling.change_display)
            && (!self.mapper_read || ceiling.mapper_read)
            && (!self.mapper_write || ceiling.mapper_write)
            && (!self.widgets || ceiling.widgets)
            && (!self.interop_read || ceiling.interop_read)
            && (!self.interop_write || ceiling.interop_write)
            && (!self.panes || ceiling.panes)
            && (!self.gmcp_send || ceiling.gmcp_send)
    }

    /// The capabilities requested by `self` (a newly-resolved closure union) but **not** by
    /// `baseline` (the consented set) — the smudgy half of the "additionally wants" update delta
    /// (`PACKAGE-ISOLATES-OP-CAPABILITIES.md`, alongside [`PackagePermissions::added_since`]). An
    /// all-false result means the new union only shrank or stayed within the grant (auto-accept).
    #[must_use]
    pub fn added_since(&self, baseline: &Self) -> Self {
        Self {
            create_aliases: self.create_aliases && !baseline.create_aliases,
            create_triggers: self.create_triggers && !baseline.create_triggers,
            send: self.send && !baseline.send,
            send_direct: self.send_direct && !baseline.send_direct,
            echo: self.echo && !baseline.echo,
            reach_others: self.reach_others && !baseline.reach_others,
            change_display: self.change_display && !baseline.change_display,
            mapper_read: self.mapper_read && !baseline.mapper_read,
            mapper_write: self.mapper_write && !baseline.mapper_write,
            widgets: self.widgets && !baseline.widgets,
            interop_read: self.interop_read && !baseline.interop_read,
            interop_write: self.interop_write && !baseline.interop_write,
            panes: self.panes && !baseline.panes,
            gmcp_send: self.gmcp_send && !baseline.gmcp_send,
        }
    }
}

/// The manifest/lockfile wire shape of [`SmudgyCapabilities`]: a `sys`-style op allowlist
/// (`PACKAGE-ISOLATES.md` form), one array of string tokens per manifest key. The booleans
/// are projected to/from these tokens; unknown tokens are dropped on the way in (forward-compat),
/// and `mapper_write` round-trips as `["write"]` (which re-implies `read`).
///
/// `events` is a legacy alias group: `events: ["subscribe"]` parses as `interop: ["read"]` and
/// `events: ["emit"]` as `interop: ["write"]`, so pre-interop manifests and consent records keep
/// working unmigrated. Serialization **dual-emits** it: both the canonical `interop` tokens and
/// the legacy `events` tokens are written, so a package resaved/republished on this build — and a
/// consent record re-serialized by ordinary use — stays readable by a pre-interop client (which
/// drops the unknown `interop` key but still honors `events`) and by a downgraded build. Without
/// the dual-emit the migration was one-directional and downgrade-hostile.
///
/// The alias is a deprecation bridge, not a second spelling: it is REMOVED at the start of the
/// 0.5.x branch (decided 2026-07-05, with session-store phase 1), at which point serialization
/// emits only `interop`. The version assert below this struct fails any 0.5+ build that still
/// carries it, so the removal can't be forgotten. A manifest still declaring only `events` after
/// the removal parses as requesting no interop capability (the ordinary unknown-key rule) — the
/// intended hard cut.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SmudgyCapabilitiesWire {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    automations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    session: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    mapper: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    display: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    widgets: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    interop: Vec<String>,
    /// Legacy alias of `interop` (`emit` → `write`, `subscribe` → `read`). Parsed, and dual-emitted
    /// beside `interop` for pre-interop/downgrade compatibility; removed at 0.5.x (see the struct
    /// docs + the version assert below).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    events: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    panes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    gmcp: Vec<String>,
}

// Build-time sunset for the legacy `events` capability alias. `const` panics are compile
// errors, so the first build whose crate version leaves the 0.4.x line (0.5.0-rc1 included)
// fails HERE until the alias is deleted. Scoped to exactly what must go: the `events` wire
// field above, its two `has_token(&wire.events, …)` fallbacks in `From<SmudgyCapabilitiesWire>`,
// the legacy-token DUAL-EMIT in `From<SmudgyCapabilities>` (the `events` vec it fills), the
// `legacy_events_tokens_alias_onto_interop` + `smudgy_interop_tokens_parse_and_round_trip`
// test expectations, and this assert.
const _: () = {
    let major = decimal_version_component(env!("CARGO_PKG_VERSION_MAJOR"));
    let minor = decimal_version_component(env!("CARGO_PKG_VERSION_MINOR"));
    assert!(
        major == 0 && minor < 5,
        "the legacy `events` capability alias (events: [\"emit\"/\"subscribe\"] -> interop) is \
         scheduled for removal at the start of the 0.5.x branch: delete the `events` wire field, \
         its alias parsing in From<SmudgyCapabilitiesWire>, the alias test, and this assert"
    );
};

/// Const-context parse of one `CARGO_PKG_VERSION_*` component (a plain decimal string) for the
/// alias-sunset assert above.
const fn decimal_version_component(component: &str) -> u64 {
    let bytes = component.as_bytes();
    let mut value = 0u64;
    let mut i = 0;
    while i < bytes.len() {
        assert!(bytes[i].is_ascii_digit(), "version components are decimal");
        value = value * 10 + (bytes[i] - b'0') as u64;
        i += 1;
    }
    value
}

/// Whether `tokens` contains `tok` (trimmed, case-insensitive — manifest authors shouldn't be
/// tripped by casing/whitespace, mirroring the `net`-host leniency).
fn has_token(tokens: &[String], tok: &str) -> bool {
    tokens.iter().any(|t| t.trim().eq_ignore_ascii_case(tok))
}

impl From<SmudgyCapabilitiesWire> for SmudgyCapabilities {
    fn from(wire: SmudgyCapabilitiesWire) -> Self {
        let mapper_write = has_token(&wire.mapper, "write");
        Self {
            create_aliases: has_token(&wire.automations, "aliases"),
            create_triggers: has_token(&wire.automations, "triggers"),
            send: has_token(&wire.session, "send"),
            send_direct: has_token(&wire.session, "send-direct"),
            echo: has_token(&wire.session, "echo"),
            reach_others: has_token(&wire.session, "reach-others"),
            change_display: has_token(&wire.display, "change"),
            // `write` implies `read`: a writer can read.
            mapper_read: has_token(&wire.mapper, "read") || mapper_write,
            mapper_write,
            widgets: has_token(&wire.widgets, "create"),
            interop_read: has_token(&wire.interop, "read") || has_token(&wire.events, "subscribe"),
            interop_write: has_token(&wire.interop, "write") || has_token(&wire.events, "emit"),
            panes: has_token(&wire.panes, "create"),
            gmcp_send: has_token(&wire.gmcp, "send"),
        }
    }
}

impl From<SmudgyCapabilities> for SmudgyCapabilitiesWire {
    fn from(caps: SmudgyCapabilities) -> Self {
        let mut automations = Vec::new();
        if caps.create_aliases {
            automations.push("aliases".to_string());
        }
        if caps.create_triggers {
            automations.push("triggers".to_string());
        }
        let mut session = Vec::new();
        if caps.send {
            session.push("send".to_string());
        }
        if caps.send_direct {
            session.push("send-direct".to_string());
        }
        if caps.echo {
            session.push("echo".to_string());
        }
        if caps.reach_others {
            session.push("reach-others".to_string());
        }
        let mut display = Vec::new();
        if caps.change_display {
            display.push("change".to_string());
        }
        // `write` implies `read`, so emit just `write` when both hold (it round-trips back to both).
        let mut mapper = Vec::new();
        if caps.mapper_write {
            mapper.push("write".to_string());
        } else if caps.mapper_read {
            mapper.push("read".to_string());
        }
        let mut widgets = Vec::new();
        if caps.widgets {
            widgets.push("create".to_string());
        }
        let mut interop = Vec::new();
        if caps.interop_read {
            interop.push("read".to_string());
        }
        if caps.interop_write {
            interop.push("write".to_string());
        }
        // Dual-emit the legacy `events` tokens beside the canonical `interop` ones until the
        // 0.5.x cut (the version assert by `SmudgyCapabilitiesWire` fails the build that must drop
        // this). A pre-interop client drops the unknown `interop` key, so without the legacy
        // tokens a manifest resaved / republished on this build — or a consent record
        // re-serialized by mere use, then read after a downgrade — would silently lose its event
        // capability (`NotCapable` at runtime, or a package that refuses to load). `read`→
        // `subscribe`, `write`→`emit`; on old clients this grants only events (they have no
        // store), which is the correct subset of the interop capability requested.
        let mut events = Vec::new();
        if caps.interop_read {
            events.push("subscribe".to_string());
        }
        if caps.interop_write {
            events.push("emit".to_string());
        }
        let mut panes = Vec::new();
        if caps.panes {
            panes.push("create".to_string());
        }
        let mut gmcp = Vec::new();
        if caps.gmcp_send {
            gmcp.push("send".to_string());
        }
        Self {
            automations,
            session,
            mapper,
            display,
            widgets,
            interop,
            events,
            panes,
            gmcp,
        }
    }
}

/// Per-field permission semantics for the containment checks (`PACKAGE-ISOLATES-ENFORCEMENT.md`),
/// so [`is_within`](PackagePermissions::is_within) and
/// [`added_since`](PackagePermissions::added_since) agree with what the restricted container grants.
#[derive(Clone, Copy)]
enum PermField {
    /// `net`: a bare `host` grant covers any port; an exact `host:port` covers only that port.
    Net,
    /// `read`/`write`: a path grant covers its whole subtree (prefix at a component boundary).
    Path,
    /// `env`: exact (trimmed) variable-name match — no subtree.
    Env,
}

/// Entries of `new` not **covered** by any entry of `old` under `field`'s containment semantics — a
/// per-field set difference preserving `new`'s order and original spelling, de-duped within `new`
/// (so a list repeating an ask reports it once). Using containment (not exact membership) is what
/// mirrors deno's descriptors: a narrower `net` port or a deeper `read` path already covered by
/// `old` is *not* reported as added.
fn added_entries(new: &[String], old: &[String], field: PermField) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    new.iter()
        .filter(|entry| !covered_by(field, old, entry) && seen.insert(dedup_key(field, entry)))
        .cloned()
        .collect()
}

/// Whether any grant in `ceiling` covers `requested` under `field`'s containment semantics.
fn covered_by(field: PermField, ceiling: &[String], requested: &str) -> bool {
    match field {
        PermField::Net => ceiling.iter().any(|grant| host_covers(grant, requested)),
        PermField::Path => {
            // A `$DATA/..` escape is **dropped** by the enforcement guardrail (it would leave the
            // data dir), so it grants and needs nothing — mirror that here so the resolver gate, the
            // delta, and the container all agree: a *requested* escape is inert (already "covered",
            // never blocks a version or shows as "added"), and a *granted* escape covers nothing.
            path_entry_dropped(requested)
                || ceiling
                    .iter()
                    .any(|grant| !path_entry_dropped(grant) && path_covers(grant, requested))
        }
        PermField::Env => ceiling
            .iter()
            .any(|grant| normalize_plain(grant) == normalize_plain(requested)),
    }
}

/// Whether a `read`/`write` entry is **dropped** by the enforcement guardrail
/// (`PACKAGE-ISOLATES-ENFORCEMENT.md`, mirroring `script_engine::expand_data_placeholder`): a
/// `$DATA/<sub>` (or `$DATA\<sub>`) whose subpath has a `..` component would escape the data dir, so
/// the engine drops it. Such an entry is inert — it grants nothing and can never be a real exposure.
fn path_entry_dropped(entry: &str) -> bool {
    let Some(rest) = entry.trim().strip_prefix("$DATA") else {
        return false;
    };
    let sub = match rest.chars().next() {
        None => return false,                              // bare `$DATA`
        Some('/' | '\\') => rest.trim_start_matches(['/', '\\']),
        Some(_) => return false,                           // `$DATABASE` etc. — not the placeholder
    };
    sub.split(['/', '\\']).any(|component| component == "..")
}

/// The within-`new` dedup key for `field` (so repeated literals collapse to one displayed line):
/// `net` is host-case-insensitive, paths/env are case-sensitive (trimmed).
fn dedup_key(field: PermField, entry: &str) -> String {
    match field {
        PermField::Net => normalize_host(entry),
        PermField::Path | PermField::Env => normalize_plain(entry),
    }
}

/// Whether the consented `net` grant `ceiling` covers the `requested` host entry, mirroring deno's
/// `NetDescriptor` (`PACKAGE-ISOLATES-ENFORCEMENT.md`): hosts compare case-insensitively, and a
/// bare-host grant (no port) covers every port while a `host:port` grant covers only that exact
/// port. (Manifest `net` entries are `host` or `host:port`; a colon suffix that isn't a `u16` port
/// is taken as part of the host.)
fn host_covers(ceiling: &str, requested: &str) -> bool {
    let (ceiling_host, ceiling_port) = split_host_port(ceiling);
    let (requested_host, requested_port) = split_host_port(requested);
    ceiling_host == requested_host && (ceiling_port.is_none() || ceiling_port == requested_port)
}

/// Split a `net` entry into its lowercased host and optional `u16` port. A trailing `:<digits>`
/// that parses as a `u16` is the port; anything else is taken wholesale as the host.
fn split_host_port(entry: &str) -> (String, Option<u16>) {
    let entry = entry.trim();
    match entry
        .rsplit_once(':')
        .and_then(|(host, port)| port.parse::<u16>().ok().map(|port| (host, port)))
    {
        Some((host, port)) => (host.to_ascii_lowercase(), Some(port)),
        None => (entry.to_ascii_lowercase(), None),
    }
}

/// Whether the consented `read`/`write` grant `ceiling` covers the `requested` path, mirroring
/// deno's path descriptors (`PACKAGE-ISOLATES-ENFORCEMENT.md`): a directory grant covers its
/// whole subtree, so `ceiling` covers `requested` iff they are equal or `requested` is nested under
/// `ceiling` at a path-component boundary. Compared on the manifest (placeholder) form — both the
/// consented and the candidate entries are recorded pre-expansion — with `\` normalized to `/`, any
/// `.`/`..` components resolved lexically, and a trailing separator ignored, so `$DATA/maps` covers
/// `$DATA/maps/regions/eu.json` but not the prefix-sharing sibling `$DATA/maps-2`.
fn path_covers(ceiling: &str, requested: &str) -> bool {
    let ceiling = normalize_path_key(ceiling);
    let requested = normalize_path_key(requested);
    requested == ceiling
        || requested
            .strip_prefix(ceiling.as_str())
            .is_some_and(|tail| tail.starts_with('/'))
}

/// Normalize a path entry for prefix comparison: trimmed, `\` → `/`, `.`/`..` components resolved
/// lexically (mirroring deno's `normalize_path` — `.` is dropped, `..` pops the parent), and a
/// trailing separator ignored. Resolving `..` is what keeps the textual subtree compare honest:
/// `deno_permissions` canonicalizes a non-`$DATA` `read`/`write` path before granting it (a
/// `$DATA/..` escape is instead *dropped* upstream — see [`path_entry_dropped`]), so without this a
/// consented `/home/u/srv` would *textually* appear to cover `/home/u/srv/../other` while the
/// container actually grants the resolved `/home/u/other` — an escape the user never consented to.
/// A `..` that would climb above the root is clamped (an absolute path can't escape `/`; a relative
/// one keeps a leading `..`).
fn normalize_path_key(path: &str) -> String {
    let slashed = path.trim().replace('\\', "/");
    let is_absolute = slashed.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for component in slashed.split('/') {
        match component {
            "" | "." => {} // a leading/trailing/doubled separator or a `.` — no-op
            ".." => {
                if stack.last().is_some_and(|&last| last != "..") {
                    stack.pop(); // climb out of a real parent we just descended into
                } else if !is_absolute {
                    stack.push(".."); // relative path: can't pop the root, so retain the `..`
                }
            }
            other => stack.push(other),
        }
    }
    let joined = stack.join("/");
    if is_absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Comparison key for a `net` host entry: trimmed + lowercased (DNS hosts are case-insensitive;
/// a `host:port` port suffix is digits, so lowercasing leaves it intact).
fn normalize_host(entry: &str) -> String {
    entry.trim().to_lowercase()
}

/// Comparison key for a path/env entry: trimmed only (filesystem paths and env var names are
/// case-sensitive on the platforms that matter, so casing is significant).
fn normalize_plain(entry: &str) -> String {
    entry.trim().to_string()
}

/// Append each entry of `src` to `dst` that is not already present (set-union preserving
/// first-seen order). Lists are tiny (a handful of hosts/paths), so the linear membership
/// scan is fine and keeps the result deterministic.
fn merge_dedup(dst: &mut Vec<String>, src: &[String]) {
    for item in src {
        if !dst.contains(item) {
            dst.push(item.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// One ES module's source within a fetched package.
#[derive(Debug, Clone, PartialEq)]
pub struct PackageModuleSource {
    /// File path within the package, e.g. `index.js`, `lib/util.ts`.
    pub subpath: String,
    pub text: String,
}

/// A fully fetched package at a concrete version, cached by `(key, version)`.
#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub key: PackageKey,
    pub resolved_version: String,
    pub manifest: PackageManifest,
    /// Content hash from the backend, verified by the provider on fetch.
    pub integrity: String,
    pub modules: Vec<PackageModuleSource>,
}

impl ResolvedPackage {
    /// Exact lookup of a module file already named with its concrete subpath.
    #[must_use]
    pub fn module_source(&self, module_subpath: &str) -> Option<&PackageModuleSource> {
        self.modules.iter().find(|m| m.subpath == module_subpath)
    }

    /// Resolve a user subpath (or `None` = entry) to a concrete module file, applying
    /// extension and `index.*` resolution.
    ///
    /// # Errors
    /// Returns [`PackageError::NotFound`] when no module matches.
    pub fn resolve_module(&self, subpath: Option<&str>) -> Result<&PackageModuleSource, PackageError> {
        let candidates = match subpath {
            None => entry_candidates(self.manifest.entry.as_deref()),
            Some(sub) => subpath_candidates(sub),
        };
        for candidate in &candidates {
            if let Some(module) = self.module_source(candidate) {
                return Ok(module);
            }
        }
        Err(PackageError::NotFound(format!(
            "package {} has no module for {}",
            self.key.name,
            subpath.unwrap_or("<entry>")
        )))
    }
}

/// Resolves and fetches `smudgy://` packages for the module loader.
///
/// `?Send`/current-thread: the provider runs on the session thread under deno's event
/// loop (driven via [`ModuleLoadResponse::Async`](deno_core::ModuleLoadResponse)),
/// like the npm stack — never under a nested `block_on`.
#[async_trait::async_trait(?Send)]
pub trait PackageProvider {
    /// Resolve a package to a concrete version and fetch its whole module set (cached
    /// by resolved version). When `referrer` is `Some`, the version is selected from that
    /// importing package instance's locked deps (referrer-aware resolution); when
    /// `None`, from the lockfile / latest. For auto-update packages this re-resolves latest
    /// (with an offline fallback); for pinned packages it returns the pinned version.
    /// Integrity is verified here on every fetch.
    ///
    /// # Errors
    /// Returns [`PackageError`] on resolution, network, integrity, or manifest failure.
    async fn resolve_package(
        &self,
        key: &PackageKey,
        referrer: Option<&ReferrerRef>,
    ) -> Result<Rc<ResolvedPackage>, PackageError>;

    /// In-memory lookup of an already-fetched set (no I/O). Used by the canonical-URL
    /// load path, which only runs after `resolve_package` populated the set.
    fn get_cached(&self, key: &PackageKey, version: &str) -> Option<Rc<ResolvedPackage>>;

    /// The package this session most recently resolved for `key` (no I/O). Lets the
    /// host build a [`crate::LoadReport`] (version, declared options, integrity) after
    /// modules evaluate, without re-resolving.
    fn get_resolved(&self, key: &PackageKey) -> Option<Rc<ResolvedPackage>>;

    /// The deno-native permission union across **this provider's isolate closure** — the
    /// `permissions` of the root package merged with every transitive `smudgy://`
    /// dependency (`script/PACKAGE-ISOLATES-ENFORCEMENT.md`). The per-package isolate
    /// factory reads this to build the isolate's restricted [`crate::PermissionsContainer`].
    /// Because each isolate gets its own provider (the cloud `fork`, or a fresh
    /// test resolver), the provider's closure *is* that isolate's closure.
    ///
    /// The default denies everything (an empty union) — the safe default for a provider
    /// that does not track a closure. The cloud provider folds the union during its
    /// `solve_closure` pre-pass; this in-memory one unions the packages it holds.
    fn closure_permissions(&self) -> PackagePermissions {
        PackagePermissions::default()
    }

    /// Every package this provider has resolved for its isolate so far — i.e. the packages whose
    /// code was (or is about to be) evaluated in that isolate, roots and transitive `smudgy://`
    /// dependencies alike. Because each isolate has its own provider, this is the isolate's
    /// package set. The host reads it after module loading to detect code-imported copies of
    /// packages whose interop home is another isolate (the session-store "stumble" diagnostic).
    ///
    /// The default (empty) simply disables that diagnostic for providers that don't track
    /// resolutions.
    fn loaded_packages(&self) -> Vec<PackageKey> {
        Vec::new()
    }

    /// Resolve a producer package for link-time consumer-stub synthesis (the `smudgy:state/` /
    /// `smudgy:events/` schemes). Semantically a *read of the producer's declarations*, not a
    /// code load: implementations must not record the fetch in [`Self::loaded_packages`] (it
    /// would misfire the code-import stumble diagnostic at a consumer who never code-imported
    /// anything) nor treat it as an install (consuming an uninstalled producer must leave it
    /// uninstalled). The default delegates to [`Self::resolve_package`], which over-reports on
    /// both counts — tracking providers override.
    ///
    /// # Errors
    /// Returns [`PackageError`] on resolution, network, integrity, or manifest failure.
    async fn resolve_package_for_stub(
        &self,
        key: &PackageKey,
    ) -> Result<Rc<ResolvedPackage>, PackageError> {
        self.resolve_package(key, None).await
    }

    /// Inform the provider which packages' interop home is the isolate it serves
    /// (interop.md §3). The host calls this once per isolate, before modules load; the
    /// load path then scrubs interop-handle exports from every *other* package's modules
    /// ([`Self::is_home_load`]). The default ignores it — paired with the `is_home_load`
    /// default, providers that don't track homes serve everything unscrubbed.
    fn set_home_packages(&self, _homes: Vec<PackageKey>) {}

    /// Whether `key`'s interop home is the isolate this provider serves. `true` (the
    /// default, and the answer whenever [`Self::set_home_packages`] was never called)
    /// serves the source as-authored; `false` scrubs interop-handle exports before
    /// evaluation, so a code-importing consumer fails at link instead of receiving a live
    /// producer handle the home gate would refuse anyway.
    fn is_home_load(&self, _key: &PackageKey) -> bool {
        true
    }

    /// Record that a non-home load of `key` had interop-handle exports scrubbed. The host
    /// reads [`Self::scrubbed_packages`] after module loading (failed or not) to emit the
    /// teaching notice naming the scheme imports — which also dresses a resulting link
    /// error ("does not provide an export named …") when the consumer imported a handle.
    fn note_scrubbed(&self, _key: &PackageKey) {}

    /// The packages this provider scrubbed handle exports from (see [`Self::note_scrubbed`]).
    fn scrubbed_packages(&self) -> Vec<PackageKey> {
        Vec::new()
    }

    /// Record that non-package (user-script / local-module) code code-imported `key`. On
    /// main, a trusted package's home load cannot be scrubbed (one module map, one
    /// instance), so a user script obtains live producer handles this way — the accepted
    /// interop.md §1 residual; the host turns this record into a one-time warning when the
    /// package declares interop handles.
    fn note_user_code_import(&self, _key: &PackageKey) {}

    /// The packages user-level code code-imported (see [`Self::note_user_code_import`]).
    fn user_code_imports(&self) -> Vec<PackageKey> {
        Vec::new()
    }
}

/// The module text to serve for `key` in this provider's isolate: home loads serve the
/// source as-authored; a non-home load of the package's ENTRY module has its interop-handle
/// exports scrubbed (interop.md §3). Entry-only on purpose: the entry is the consumer-facing
/// surface, while internal modules import each other's handles — scrubbing those would break
/// the copy's own intra-package links, and reaching a handle through an internal module was
/// always documented depth, not a sealed boundary.
fn serve_text<'a>(
    provider: &Rc<dyn PackageProvider>,
    key: &PackageKey,
    canonical: &ModuleSpecifier,
    text: &'a str,
    is_entry: bool,
) -> std::borrow::Cow<'a, str> {
    if !is_entry || provider.is_home_load(key) {
        return std::borrow::Cow::Borrowed(text);
    }
    match crate::interop_extract::scrub_handle_exports(canonical, text) {
        Some((scrubbed, names)) => {
            provider.note_scrubbed(key);
            log::warn!(
                "smudgy: removed interop handle exports ({}) from the code-imported (non-home) copy of smudgy://{}/{} — consume them via smudgy:state/{}/{} (or smudgy:events/…, smudgy:procedures/…)",
                names.join(", "),
                key.owner,
                key.name,
                key.owner,
                key.name
            );
            std::borrow::Cow::Owned(scrubbed)
        }
        None => std::borrow::Cow::Borrowed(text),
    }
}

/// `load()`'s async handler for a marker URL: resolve the version, fetch the set, and
/// redirect the requested specifier to its version-pinned canonical URL (this is what
/// makes install == import == one shared instance).
///
/// # Errors
/// Returns a [`ModuleLoaderError`] on a bad marker, resolution failure, a missing
/// module, or transpile failure.
pub(crate) async fn load_marker_module(
    provider: Rc<dyn PackageProvider>,
    marker: &ModuleSpecifier,
) -> Result<ModuleSource, ModuleLoaderError> {
    let spec = SmudgySpecifier::from_marker_url(marker)
        .ok_or_else(|| crate::generic_loader_error(format!("invalid smudgy marker {marker}")))?;
    let key = spec.package_key();
    let fetched = provider
        .resolve_package(&key, spec.referrer())
        .await
        .map_err(|err| crate::generic_loader_error(err.to_string()))?;
    // Import-deny: a package that declares `importable: false` may only be imported by
    // *same-owner* packages. A cross-owner package import is rejected — the events-only
    // library / private-package switch (`script/REQUIRED-PACKAGES.md`). The referrer is only
    // present when the importer is itself a package; user/top-level modules carry no referrer
    // and are exempt (the user may import their own installed packages). This is independent of
    // isolate trust: trust grants permissions, it does not bypass another package's import-deny.
    if !fetched.manifest.importable {
        if let Some(referrer) = spec.referrer() {
            if referrer.key.owner != key.owner {
                return Err(crate::generic_loader_error(format!(
                    "package {}/{} is not importable: it declares \"importable\": false, so {}/{} may not `import` it — consume it via the package's `requires` + its events/types instead",
                    key.owner, key.name, referrer.key.owner, referrer.key.name
                )));
            }
        }
    }
    let module = fetched
        .resolve_module(spec.subpath.as_deref())
        .map_err(|err| crate::generic_loader_error(err.to_string()))?;
    let canonical = canonical_url(&key, &fetched.resolved_version, &module.subpath);
    let is_entry = fetched
        .resolve_module(None)
        .is_ok_and(|entry| entry.subpath == module.subpath);
    let text = serve_text(&provider, &key, &canonical, &module.text, is_entry);
    let (code, _source_map) = crate::transpiler::transpile(&canonical, &text)
        .map_err(|err| crate::generic_loader_error(format!("failed transpiling {canonical}: {err}")))?;
    Ok(ModuleSource::new_with_redirect(
        module_type_for(&module.subpath),
        ModuleSourceCode::String(code.into()),
        marker,
        &canonical,
        None,
    ))
}

/// `load()`'s async handler for a canonical URL (a sub-module or relative import within
/// an already-fetched package). Serves from the in-memory set populated by
/// [`load_marker_module`].
///
/// # Errors
/// Returns a [`ModuleLoaderError`] on a bad URL, an unfetched package, a missing
/// module, or transpile failure.
pub(crate) async fn load_canonical_module(
    provider: Rc<dyn PackageProvider>,
    url: &ModuleSpecifier,
) -> Result<ModuleSource, ModuleLoaderError> {
    let coords = parse_canonical(url)
        .ok_or_else(|| crate::generic_loader_error(format!("invalid canonical package url {url}")))?;
    let fetched = provider
        .get_cached(&coords.key, &coords.version)
        .ok_or_else(|| {
            crate::generic_loader_error(format!(
                "package {} @{} was not fetched before {url}",
                coords.key.name, coords.version
            ))
        })?;
    let module = fetched.module_source(&coords.module_subpath).ok_or_else(|| {
        crate::generic_loader_error(format!(
            "package {} has no module {}",
            coords.key.name, coords.module_subpath
        ))
    })?;
    let is_entry = fetched
        .resolve_module(None)
        .is_ok_and(|entry| entry.subpath == module.subpath);
    let text = serve_text(&provider, &coords.key, url, &module.text, is_entry);
    let (code, _source_map) = crate::transpiler::transpile(url, &text)
        .map_err(|err| crate::generic_loader_error(format!("failed transpiling {url}: {err}")))?;
    Ok(ModuleSource::new(
        module_type_for(&coords.module_subpath),
        ModuleSourceCode::String(code.into()),
        url,
        None,
    ))
}

/// `load()`'s handler for a [`PARAMS_SCHEME`] URL: synthesize the per-importer
/// `smudgy:params` module. `get(key)` is bound to the importing package's specifier
/// (baked in at synthesis) and bridges to the host via the `globalThis.__smudgy_param_get`
/// hook the smudgy ops extension installs. A no-package module's `get` returns `undefined`.
///
/// Scoping here is an ergonomic correctness feature, not a security boundary — the shared
/// isolate is allow-all (`DESIGN.md`), so a script can already reach anything; binding
/// `get` to the caller just makes `get('KEY')` return *this* package's value by default.
///
/// # Errors
/// Returns a [`ModuleLoaderError`] for a malformed params URL.
pub(crate) fn load_params_module(url: &ModuleSpecifier) -> Result<ModuleSource, ModuleLoaderError> {
    let importer = parse_params_url(url)
        .ok_or_else(|| crate::generic_loader_error(format!("invalid smudgy:params url {url}")))?;
    // JSON-encode the specifier (or "") so it embeds as a safe JS string literal.
    let spec = importer.map(|key| key.to_user_specifier()).unwrap_or_default();
    let spec_literal = serde_json::to_string(&spec).expect("a string always serializes");
    let code = format!(
        "const __spec = {spec_literal};\n\
         export function get(key) {{\n\
         \x20 return __spec ? globalThis.__smudgy_param_get(__spec, key) : undefined;\n\
         }}\n\
         export default {{ get }};\n"
    );
    Ok(ModuleSource::new(
        deno_core::ModuleType::JavaScript,
        ModuleSourceCode::String(code.into()),
        url,
        None,
    ))
}

/// The per-importer `smudgy:core` virtual-module URL. Keyed on the importing module's
/// `referrer` so each importer gets its own synthesized instance (and so its own bound
/// creator): a package module coarsens to one instance per `owner/name/version` (all the
/// package's modules share a creator namespace, like [`params_module_url`]); a local
/// `file://` module is keyed per-file; anything else falls back to the user namespace. The
/// creator descriptor is recovered from this URL by [`load_core_module`].
#[must_use]
pub fn core_module_url(referrer: &str) -> ModuleSpecifier {
    // A package's modules share one creator (coarsen to owner/name/version, dropping the
    // module subpath) — mirrors smudgy:params' per-package scoping.
    if let Some(coords) = ModuleSpecifier::parse(referrer)
        .ok()
        .and_then(|url| parse_canonical(&url))
    {
        let path = format!("/pkg/{}/{}/{}", coords.key.owner, coords.key.name, coords.version);
        return ModuleSpecifier::parse(&format!("{CORE_SCHEME}://{path}"))
            .expect("core package URL is well-formed for validated components");
    }
    // A local module: keyed per-file. The full referrer URL rides in a `?ref=` query so it
    // round-trips losslessly (core resolves it to a module subpath, which this crate can't
    // — the loader's cwd is not the modules root).
    if referrer.starts_with("file://") {
        let mut url = ModuleSpecifier::parse(&format!("{CORE_SCHEME}:///mod"))
            .expect("core module URL is well-formed");
        url.query_pairs_mut().append_pair("ref", referrer);
        return url;
    }
    ModuleSpecifier::parse(&format!("{CORE_SCHEME}:///user"))
        .expect("core user URL is well-formed")
}

/// Recover the creator descriptor (as a JSON object literal) from a [`core_module_url`]
/// output. The shapes are `{"kind":"user"}`, `{"kind":"package","owner","name","version"}`,
/// and `{"kind":"module","referrer"}`. Returns `None` for a URL that isn't a well-formed
/// [`CORE_SCHEME`] URL.
fn core_creator_json(url: &ModuleSpecifier) -> Option<String> {
    if url.scheme() != CORE_SCHEME {
        return None;
    }
    creator_json_from_path(url)
}

/// Recover the creator descriptor JSON from a synthesized per-importer URL's path segments.
/// Shared by `smudgy:core` and `smudgy:widgets` (identical provenance encoding); the caller
/// has already scheme-dispatched in `load()`, so this does not re-check the scheme.
fn creator_json_from_path(url: &ModuleSpecifier) -> Option<String> {
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let value = match segments.as_slice() {
        ["user"] => serde_json::json!({ "kind": "user" }),
        ["pkg", owner, name, version] => serde_json::json!({
            "kind": "package",
            "owner": owner,
            "name": name,
            "version": version,
        }),
        ["mod"] => {
            let referrer = url
                .query_pairs()
                .find(|(key, _)| key == "ref")
                .map(|(_, value)| value.into_owned())?;
            serde_json::json!({ "kind": "module", "referrer": referrer })
        }
        _ => return None,
    };
    Some(value.to_string())
}

/// `load()`'s handler for a [`CORE_SCHEME`] URL: synthesize the per-importer `smudgy:core`
/// module. Its `createAlias`/`createTrigger`/`createHotkey` are bound to the importing
/// module's *creator* (baked in at synthesis from the referrer), so an automation the
/// importer creates is attributed to it. Bridges to the host via the
/// `globalThis.__smudgy_create_api` hook the smudgy ops extension installs.
///
/// Like [`load_params_module`], the scoping is an ergonomic-provenance feature, not a
/// security boundary — the shared isolate is allow-all (`DESIGN.md`).
///
/// # Errors
/// Returns a [`ModuleLoaderError`] for a malformed `smudgy:core` URL.
pub(crate) fn load_core_module(url: &ModuleSpecifier) -> Result<ModuleSource, ModuleLoaderError> {
    let creator = core_creator_json(url)
        .ok_or_else(|| crate::generic_loader_error(format!("invalid smudgy:core url {url}")))?;
    // Named exports cover the full convenience surface. The `create*` members are
    // creator-bound (baked from this importer's `__creator`); the rest are the shared,
    // provenance-free convenience set.
    //
    // The session id is CONSTANT for a runtime's life, so the stable handles
    // (`session`/`currentSession`/`mapper`/`id`) are re-exported BY NAME: reading the
    // api getter once here snapshots an immutable value, which is correct. The genuinely
    // live-state members are exposed as FUNCTIONS instead of value exports -- `getSessions()`
    // (the connected-session set changes) and `getProfile()` (profile fields read live) --
    // so a stale snapshot is impossible. `mapper` is a value export here (not in the
    // extension entry) because this synthesized module evaluates long after `mapper.ts`
    // installs `globalThis.mapper`, so the getter yields the real, immutable mapper.
    let code = format!(
        "const __creator = {creator};\n\
         const __api = globalThis.__smudgy_create_api(__creator);\n\
         export const createAlias = __api.createAlias;\n\
         export const createTrigger = __api.createTrigger;\n\
         export const createTriggers = __api.createTriggers;\n\
         export const createTimer = __api.createTimer;\n\
         export const createHotkey = __api.createHotkey;\n\
         export const triggers = __api.triggers;\n\
         export const aliases = __api.aliases;\n\
         export const timers = __api.timers;\n\
         export const hotkeys = __api.hotkeys;\n\
         export const send = __api.send;\n\
         export const sendRaw = __api.sendRaw;\n\
         export const echo = __api.echo;\n\
         export const style = __api.style;\n\
         export const link = __api.link;\n\
         export const reload = __api.reload;\n\
         export const capture = __api.capture;\n\
         export const line = __api.line;\n\
         export const buffer = __api.buffer;\n\
         export const vars = __api.vars;\n\
         export const byName = __api.byName;\n\
         export const getSessions = __api.getSessions;\n\
         export const getProfile = __api.getProfile;\n\
         export const getSettings = __api.getSettings;\n\
         export const getDataDir = __api.getDataDir;\n\
         export const userAutomations = __api.userAutomations;\n\
         export const session = __api.session;\n\
         export const currentSession = __api.currentSession;\n\
         export const mapper = __api.mapper;\n\
         export const id = __api.id;\n\
         export const createState = __api.createState;\n\
         export const createEvent = __api.createEvent;\n\
         export const createProcedure = __api.createProcedure;\n\
         export const createDerived = __api.createDerived;\n\
         export const events = __api.events;\n\
         export const gmcp = __api.gmcp;\n\
         export default __api;\n"
    );
    Ok(ModuleSource::new(
        deno_core::ModuleType::JavaScript,
        ModuleSourceCode::String(code.into()),
        url,
        None,
    ))
}

/// The per-importer `smudgy:widgets` virtual-module URL. Mirrors [`core_module_url`] exactly
/// (same provenance encoding) so the bound creator is recovered the same way. `createWidget`
/// is provenance-bearing (mounts are keyed by `(isolate, origin, name)`).
#[must_use]
pub fn widgets_module_url(referrer: &str) -> ModuleSpecifier {
    if let Some(coords) = ModuleSpecifier::parse(referrer)
        .ok()
        .and_then(|url| parse_canonical(&url))
    {
        let path = format!("/pkg/{}/{}/{}", coords.key.owner, coords.key.name, coords.version);
        return ModuleSpecifier::parse(&format!("{WIDGETS_SCHEME}://{path}"))
            .expect("widgets package URL is well-formed for validated components");
    }
    if referrer.starts_with("file://") {
        let mut url = ModuleSpecifier::parse(&format!("{WIDGETS_SCHEME}:///mod"))
            .expect("widgets module URL is well-formed");
        url.query_pairs_mut().append_pair("ref", referrer);
        return url;
    }
    ModuleSpecifier::parse(&format!("{WIDGETS_SCHEME}:///user"))
        .expect("widgets user URL is well-formed")
}

/// The fixed `smudgy:widgets/jsx-runtime` URL. Provenance-free (the jsx-runtime only exposes
/// the pure builder factories `jsx`/`jsxs`/`Fragment`), so a single shared instance.
#[must_use]
pub fn widgets_jsx_runtime_url() -> ModuleSpecifier {
    ModuleSpecifier::parse(&format!("{WIDGETS_SCHEME}:///jsx-runtime"))
        .expect("widgets jsx-runtime URL is well-formed")
}

/// `load()`'s handler for a [`WIDGETS_SCHEME`] URL. Two shapes: the fixed `/jsx-runtime`
/// module (the automatic-JSX-runtime target, exporting `jsx`/`jsxs`/`Fragment`) and the
/// per-importer `smudgy:widgets` module (the user-facing surface, creator-bound). Both bridge
/// to the host via the `globalThis.__smudgy_make_widgets` hook the `smudgy_widgets` extension
/// installs; like [`load_core_module`] the source is emitted directly (no transpile, no redirect).
///
/// # Errors
/// Returns a [`ModuleLoaderError`] for a malformed `smudgy:widgets` URL.
pub(crate) fn load_widgets_module(
    url: &ModuleSpecifier,
) -> Result<ModuleSource, ModuleLoaderError> {
    // The jsx-runtime is provenance-free and shared; dispatch it before creator recovery.
    let code = if url.path() == "/jsx-runtime" {
        "const __w = globalThis.__smudgy_make_widgets({ kind: \"user\" });\n\
         export const jsx = __w.jsx;\n\
         export const jsxs = __w.jsxs;\n\
         export const Fragment = __w.Fragment;\n"
            .to_string()
    } else {
        let creator = creator_json_from_path(url).ok_or_else(|| {
            crate::generic_loader_error(format!("invalid smudgy:widgets url {url}"))
        })?;
        format!(
            "const __creator = {creator};\n\
             const __w = globalThis.__smudgy_make_widgets(__creator);\n\
             export const createWidget = __w.createWidget;\n\
             export const removeWidget = __w.removeWidget;\n\
             export const extractMarkdownLinks = __w.extractMarkdownLinks;\n\
             export const Column = __w.Column;\n\
             export const Row = __w.Row;\n\
             export const Stack = __w.Stack;\n\
             export const Container = __w.Container;\n\
             export const Text = __w.Text;\n\
             export const ProgressBar = __w.ProgressBar;\n\
             export const Scrollable = __w.Scrollable;\n\
             export const Markdown = __w.Markdown;\n\
             export const Modal = __w.Modal;\n\
             export const TextEditor = __w.TextEditor;\n\
             export const Button = __w.Button;\n\
             export const MapView = __w.MapView;\n\
             export default __w;\n"
        )
    };
    Ok(ModuleSource::new(
        deno_core::ModuleType::JavaScript,
        ModuleSourceCode::String(code.into()),
        url,
        None,
    ))
}

// ---------------------------------------------------------------------------
// Kind-scheme consumer modules (smudgy:state/, smudgy:events/)
// ---------------------------------------------------------------------------

/// The producer a kind-scheme import addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KindSchemeTarget {
    /// A reserved single-segment platform producer (`sys`, `map`, `gmcp`, `user`).
    Platform(String),
    /// A package producer (`smudgy:state/<owner>/<name>`), case-folded.
    Package(PackageKey),
}

/// A parsed kind-scheme reference: which kind, which producer, and optionally which single
/// handle (the `smudgy:state/<owner>/<pkg>/<name>` subpath form).
#[derive(Debug, Clone)]
pub(crate) struct KindSchemeRef {
    pub kind: InteropKind,
    pub target: KindSchemeTarget,
    /// Case-folded handle name of a single-handle subpath import.
    pub handle: Option<String>,
}

/// Single-segment kind-scheme paths reserved for the platform (interop.md §4). `sys`/`map` are the
/// host event catalogs; `gmcp` is the host GMCP producer (state + readiness events,
/// `docs/gmcp-plan.md`); `user` is reserved unpublished (main-isolate code shares user
/// handles by ordinary import). Reservation is unconditional, so a package owner who happens
/// to take one of these nicknames stays unaddressable through the schemes rather than
/// shadowing the platform.
const PLATFORM_PRODUCERS: [&str; 5] = ["sys", "map", "gmcp", "msdp", "user"];

/// The host event catalog of a platform producer: `(export name, canonical event name)`.
/// Mirrored by the `declare module "smudgy:events/sys"` / `"smudgy:events/map"` /
/// `"smudgy:events/gmcp"` / `"smudgy:events/msdp"` blocks in `smudgy-core.d.ts`
/// (drift-checked by a test in core's `script_typings.rs`).
#[must_use]
pub fn platform_event_catalog(producer: &str) -> &'static [&'static str] {
    match producer {
        "sys" => &["connect", "disconnect", "send", "receive"],
        "map" => &["room"],
        "gmcp" | "msdp" => &["ready", "closed"],
        _ => &[],
    }
}

/// Whether a platform producer publishes **state** — i.e. `smudgy:state/<producer>` loads a
/// consumer module over its store subtree (`docs/gmcp-plan.md` §3,
/// `docs/gmcp-mapping-plan.md` §9): the module exports one root-addressed handle (named and
/// default), so `.value` / `.watch` / `.onWrite` / `.bind` cover the whole tree.
#[must_use]
pub fn platform_state_producer(producer: &str) -> bool {
    matches!(producer, "gmcp" | "msdp")
}

fn kind_scheme(kind: InteropKind) -> &'static str {
    match kind {
        InteropKind::State => STATE_SCHEME,
        InteropKind::Event => EVENTS_SCHEME,
        InteropKind::Procedure => PROCEDURES_SCHEME,
    }
}

/// The author-facing scheme prefix (`smudgy:state` / `smudgy:events` / `smudgy:procedures`)
/// for diagnostics.
fn kind_scheme_display(kind: InteropKind) -> &'static str {
    match kind {
        InteropKind::State => "smudgy:state",
        InteropKind::Event => "smudgy:events",
        InteropKind::Procedure => "smudgy:procedures",
    }
}

/// Resolve the remainder of a `smudgy:state/…` / `smudgy:events/…` specifier (everything
/// after the kind prefix) to its internal scheme URL. Segments are ASCII case-folded — the
/// uniform fold applied everywhere interop names are structural (interop.md §2). Returns the URL
/// plus the addressed [`PackageKey`] (None for a platform producer) so the caller can apply
/// dependency gating.
///
/// # Errors
/// Returns a human-readable message for a malformed reference (wrong segment count, invalid
/// owner/name charset, or a handle subpath on a producer that has none).
pub(crate) fn kind_scheme_url(
    kind: InteropKind,
    rest: &str,
) -> Result<(ModuleSpecifier, Option<PackageKey>), String> {
    let display = kind_scheme_display(kind);
    let segments: Vec<String> = rest
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if segments.is_empty() {
        return Err(format!(
            "{display}/ needs a producer: import from {display}/<owner>/<package>"
        ));
    }
    let is_platform = PLATFORM_PRODUCERS.contains(&segments[0].as_str());
    let (path_segments, handle, package): (Vec<&str>, Option<&str>, Option<PackageKey>) =
        if is_platform {
            match segments.as_slice() {
                [producer] => (vec!["host", producer], None, None),
                [producer, handle] => (vec!["host", producer], Some(handle.as_str()), None),
                _ => {
                    return Err(format!(
                        "{display}/{rest} has too many segments for the platform producer {}",
                        segments[0]
                    ))
                }
            }
        } else {
            match segments.as_slice() {
                [_owner] => {
                    return Err(format!(
                        "{display}/{rest} is incomplete: import from {display}/<owner>/<package>"
                    ))
                }
                [owner, name] | [owner, name, _] => {
                    // Reuse the package-specifier parser for owner/name charset validation.
                    SmudgySpecifier::parse(&format!("smudgy://{owner}/{name}"))
                        .map_err(|err| format!("invalid producer in {display}/{rest}: {err}"))?;
                    let key = PackageKey {
                        owner: owner.clone(),
                        name: name.clone(),
                    };
                    let handle = if let [_, _, handle] = segments.as_slice() {
                        Some(handle.as_str())
                    } else {
                        None
                    };
                    (vec!["pkg", owner, name], handle, Some(key))
                }
                _ => {
                    return Err(format!(
                        "{display}/{rest} has too many segments: {display}/<owner>/<package>[/<handle>]"
                    ))
                }
            }
        };
    let mut url = ModuleSpecifier::parse(&format!("{}:///", kind_scheme(kind)))
        .expect("kind scheme base URL is well-formed");
    url.path_segments_mut()
        .expect("kind scheme URL has a path")
        .extend(path_segments);
    if let Some(handle) = handle {
        url.query_pairs_mut().append_pair("h", handle);
    }
    Ok((url, package))
}

/// Recover a [`KindSchemeRef`] from a [`kind_scheme_url`] output. `None` for a URL that
/// isn't a well-formed kind-scheme URL.
pub(crate) fn parse_kind_scheme_url(url: &ModuleSpecifier) -> Option<KindSchemeRef> {
    let kind = match url.scheme() {
        s if s == STATE_SCHEME => InteropKind::State,
        s if s == EVENTS_SCHEME => InteropKind::Event,
        s if s == PROCEDURES_SCHEME => InteropKind::Procedure,
        _ => return None,
    };
    let segments: Vec<String> = url
        .path_segments()?
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let target = match segments.as_slice() {
        [marker, producer] if marker == "host" => KindSchemeTarget::Platform(producer.clone()),
        [marker, owner, name] if marker == "pkg" => KindSchemeTarget::Package(PackageKey {
            owner: owner.clone(),
            name: name.clone(),
        }),
        _ => return None,
    };
    let handle = url
        .query_pairs()
        .find(|(key, _)| key == "h")
        .map(|(_, value)| value.into_owned());
    Some(KindSchemeRef {
        kind,
        target,
        handle,
    })
}

/// The producer spec baked into a synthesized consumer module — what the JS-side
/// `globalThis.__smudgy_interop_consumer` hook routes by: `"smudgy://owner/name"` for a
/// package, or the bare platform name (`"sys"`, `"map"`).
fn kind_scheme_producer_spec(target: &KindSchemeTarget) -> String {
    match target {
        KindSchemeTarget::Platform(producer) => producer.clone(),
        KindSchemeTarget::Package(key) => key.to_user_specifier(),
    }
}

/// Emit the consumer-module JS for `names` (original casing). Each handle is exported under
/// its name string (interop.md §4 naming rules) plus, when it differs, the case-folded spelling —
/// types steer authors to the canonical casing; the runtime stays lenient like every other
/// interop name. `selected` (a folded name) narrows to the single-handle subpath form, which
/// additionally default-exports the handle.
fn synthesize_consumer_code(
    producer_spec: &str,
    kind: InteropKind,
    names: &[String],
    selected: Option<&str>,
) -> String {
    let spec_literal = serde_json::to_string(producer_spec).expect("a string always serializes");
    let ctor = kind.as_str();
    let mut code = format!(
        "const __c = globalThis.__smudgy_interop_consumer({spec_literal});\n"
    );
    for (index, name) in names.iter().enumerate() {
        let folded = crate::interop_extract::fold_interop_name(name);
        if let Some(selected) = selected {
            if folded != selected {
                continue;
            }
        }
        let name_literal = serde_json::to_string(name).expect("a string always serializes");
        let folded_literal = serde_json::to_string(&folded).expect("a string always serializes");
        code.push_str(&format!(
            "const __h{index} = __c.{ctor}({name_literal});\nexport {{ __h{index} as {name_literal} }};\n"
        ));
        if folded != *name {
            code.push_str(&format!("export {{ __h{index} as {folded_literal} }};\n"));
        }
        if selected.is_some() {
            code.push_str(&format!("export default __h{index};\n"));
        }
    }
    code
}

/// `load()`'s handler for a [`STATE_SCHEME`] / [`EVENTS_SCHEME`] URL: synthesize the
/// consumer module. Package producers are resolved through the provider and their entry
/// source is *parsed, never evaluated* — link-time static stub synthesis (interop.md §4), so
/// consuming cannot instantiate the producer, cycles cannot deadlock, and a producer that
/// publishes late merely yields fallback reads meanwhile. Platform producers (`sys`, `map`)
/// synthesize from the fixed host catalog.
///
/// # Errors
/// Returns a [`ModuleLoaderError`] when the producer package cannot be fetched (nothing to
/// extract names from), its entry fails to parse, or a subpath names a handle the producer
/// does not declare (with a kind-mismatch hint when the other kind declares it).
pub(crate) async fn load_kind_scheme_module(
    provider: Option<Rc<dyn PackageProvider>>,
    url: &ModuleSpecifier,
) -> Result<ModuleSource, ModuleLoaderError> {
    let parsed = parse_kind_scheme_url(url)
        .ok_or_else(|| crate::generic_loader_error(format!("invalid kind-scheme url {url}")))?;
    let display = kind_scheme_display(parsed.kind);
    let producer_spec = kind_scheme_producer_spec(&parsed.target);

    let (names, other_kind_names): (Vec<String>, Vec<(InteropKind, Vec<String>)>) = match &parsed.target {
        KindSchemeTarget::Platform(producer) => {
            // A platform *state* producer serves one root-addressed consumer handle over
            // its whole subtree — synthesized directly (the generic per-handle flow below
            // maps export names onto named subtrees, and a root handle has no name).
            if parsed.kind == InteropKind::State && platform_state_producer(producer) {
                if parsed.handle.is_some() {
                    return Err(crate::generic_loader_error(format!(
                        "{display}/{producer} exports a single handle over the whole {producer} \
                         tree; import it bare: import {producer} from \"{display}/{producer}\""
                    )));
                }
                let code = format!(
                    "const __c = globalThis.__smudgy_interop_consumer(\"{producer}\");\n\
                     const __h0 = __c.state(\"\");\n\
                     export {{ __h0 as {producer} }};\n\
                     export default __h0;\n"
                );
                return Ok(ModuleSource::new(
                    deno_core::ModuleType::JavaScript,
                    ModuleSourceCode::String(code.into()),
                    url,
                    None,
                ));
            }
            let catalog = platform_event_catalog(producer);
            if catalog.is_empty() {
                return Err(crate::generic_loader_error(format!(
                    "{display}/{producer} is reserved for the platform but not published in this build"
                )));
            }
            if parsed.kind != InteropKind::Event {
                let hint = if platform_state_producer(producer) {
                    format!(
                        "{producer} publishes state and events; import from \
                         smudgy:state/{producer} or smudgy:events/{producer}"
                    )
                } else {
                    format!("{producer} publishes events; import from smudgy:events/{producer}")
                };
                return Err(crate::generic_loader_error(format!(
                    "{display}/{producer} does not exist — {hint}"
                )));
            }
            (catalog.iter().map(|s| (*s).to_string()).collect(), Vec::new())
        }
        KindSchemeTarget::Package(key) => {
            let provider = provider.ok_or_else(|| {
                crate::generic_loader_error(format!(
                    "consuming {display}/{}/{} requires a package provider",
                    key.owner, key.name
                ))
            })?;
            let fetched = provider.resolve_package_for_stub(key).await.map_err(|err| {
                crate::generic_loader_error(format!(
                    "cannot consume {display}/{}/{}: the producer package could not be fetched ({err})",
                    key.owner, key.name
                ))
            })?;
            let entry = fetched.resolve_module(None).map_err(|err| {
                crate::generic_loader_error(format!(
                    "cannot consume {display}/{}/{}: {err}",
                    key.owner, key.name
                ))
            })?;
            let entry_url = canonical_url(key, &fetched.resolved_version, &entry.subpath);
            let extraction =
                crate::interop_extract::extract_interop_handles(&entry_url, &entry.text)
                    .map_err(|err| {
                        crate::generic_loader_error(format!(
                            "cannot consume {display}/{}/{}: its entry module failed to parse: {err}",
                            key.owner, key.name
                        ))
                    })?;
            if !extraction.duplicates.is_empty() {
                log::warn!(
                    "smudgy: package {}/{} declares duplicate interop handle name(s): {} (first declaration wins)",
                    key.owner,
                    key.name,
                    extraction.duplicates.join(", ")
                );
            }
            for diagnostic in &extraction.export_diagnostics {
                log::warn!("smudgy: package {}/{}: {diagnostic}", key.owner, key.name);
            }
            let of = |kind: InteropKind| -> Vec<String> {
                extraction
                    .of_kind(kind)
                    .map(|h| h.name.clone())
                    .collect()
            };
            let others = [InteropKind::State, InteropKind::Event, InteropKind::Procedure]
                .into_iter()
                .filter(|kind| *kind != parsed.kind)
                .map(|kind| (kind, of(kind)))
                .collect();
            (of(parsed.kind), others)
        }
    };

    if let Some(handle) = &parsed.handle {
        let known = names
            .iter()
            .any(|n| crate::interop_extract::fold_interop_name(n) == *handle);
        if !known {
            let declared_as_other = other_kind_names.iter().find(|(_, other_names)| {
                other_names
                    .iter()
                    .any(|n| crate::interop_extract::fold_interop_name(n) == *handle)
            });
            let hint = if let Some((other_kind, _)) = declared_as_other {
                format!(
                    " — it is declared as a {} handle; import it from {}/…",
                    other_kind.as_str(),
                    kind_scheme_display(*other_kind)
                )
            } else if names.is_empty() {
                String::new()
            } else {
                format!(" (declared: {})", names.join(", "))
            };
            return Err(crate::generic_loader_error(format!(
                "{producer_spec} declares no {} handle named {handle}{hint}",
                parsed.kind.as_str()
            )));
        }
    }

    let code = synthesize_consumer_code(
        &producer_spec,
        parsed.kind,
        &names,
        parsed.handle.as_deref(),
    );
    Ok(ModuleSource::new(
        deno_core::ModuleType::JavaScript,
        ModuleSourceCode::String(code.into()),
        url,
        None,
    ))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A malformed `smudgy://` specifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmudgySpecifierError {
    MissingScheme,
    EmptyComponent(&'static str),
    InvalidSubpath(String),
}

impl std::fmt::Display for SmudgySpecifierError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingScheme => write!(f, "not a smudgy:// specifier"),
            Self::EmptyComponent(c) => write!(f, "empty {c}"),
            Self::InvalidSubpath(s) => write!(f, "invalid subpath: {s}"),
        }
    }
}

impl std::error::Error for SmudgySpecifierError {}

/// A package resolution/fetch failure.
#[derive(Debug, Clone)]
pub enum PackageError {
    Network(String),
    NotFound(String),
    IntegrityMismatch {
        specifier: String,
        expected: String,
        actual: String,
    },
    InvalidManifest(String),
    Offline(String),
    Other(String),
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(m) => write!(f, "package network error: {m}"),
            Self::NotFound(m) => write!(f, "package not found: {m}"),
            Self::IntegrityMismatch {
                specifier,
                expected,
                actual,
            } => write!(
                f,
                "integrity mismatch for {specifier}: expected {expected}, got {actual}"
            ),
            Self::InvalidManifest(m) => write!(f, "invalid package manifest: {m}"),
            Self::Offline(m) => write!(f, "offline and no cached package: {m}"),
            Self::Other(m) => write!(f, "package error: {m}"),
        }
    }
}

impl std::error::Error for PackageError {}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const MODULE_EXTENSIONS: [&str; 5] = [".ts", ".js", ".tsx", ".jsx", ".json"];
const INDEX_STEMS: [&str; 2] = ["index", "mod"];

fn validate_subpath(sub: &str) -> Result<(), SmudgySpecifierError> {
    if sub.contains("..") || sub.contains('\\') || sub.starts_with('/') {
        return Err(SmudgySpecifierError::InvalidSubpath(sub.to_string()));
    }
    Ok(())
}

/// Candidate concrete files for the entry module, most-specific first.
fn entry_candidates(declared_entry: Option<&str>) -> Vec<String> {
    if let Some(entry) = declared_entry {
        return subpath_candidates(entry);
    }
    let mut out = Vec::new();
    for stem in INDEX_STEMS {
        for ext in MODULE_EXTENSIONS {
            out.push(format!("{stem}{ext}"));
        }
    }
    out
}

/// Candidate concrete files for a user subpath: exact, then `+ext`, then `/index.ext`.
fn subpath_candidates(sub: &str) -> Vec<String> {
    let sub = sub.trim_end_matches('/');
    let mut out = vec![sub.to_string()];
    if !MODULE_EXTENSIONS.iter().any(|ext| sub.ends_with(ext)) {
        for ext in MODULE_EXTENSIONS {
            out.push(format!("{sub}{ext}"));
        }
        for stem in INDEX_STEMS {
            for ext in MODULE_EXTENSIONS {
                out.push(format!("{sub}/{stem}{ext}"));
            }
        }
    }
    out
}

/// A simple in-memory [`PackageProvider`] backed by a fixed set of packages. Intended
/// for tests and as the shape a real provider mirrors.
#[derive(Debug, Default)]
pub struct InMemoryPackageProvider {
    by_version: HashMap<(PackageKey, String), Rc<ResolvedPackage>>,
    latest: HashMap<PackageKey, String>,
    /// Packages served through this provider (resolve or canonical-load hits), in first-served
    /// order — the [`PackageProvider::loaded_packages`] view of the owning isolate's package set.
    served: std::cell::RefCell<Vec<PackageKey>>,
    /// The packages whose interop home is this provider's isolate (folded keys). `None`
    /// means homes were never configured: every load is home, nothing is scrubbed.
    homes: std::cell::RefCell<Option<std::collections::HashSet<PackageKey>>>,
    /// Non-home loads that had handle exports scrubbed ([`PackageProvider::note_scrubbed`]).
    scrubbed: std::cell::RefCell<Vec<PackageKey>>,
    /// User-level code imports ([`PackageProvider::note_user_code_import`]).
    user_imports: std::cell::RefCell<Vec<PackageKey>>,
}

impl InMemoryPackageProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a package version, marking it the latest for its key.
    pub fn insert(&mut self, package: ResolvedPackage) {
        let key = package.key.clone();
        let version = package.resolved_version.clone();
        self.latest.insert(key.clone(), version.clone());
        self.by_version.insert((key, version), Rc::new(package));
    }

    /// Record `key` as served through this provider (deduplicated, first-served order).
    fn note_served(&self, key: &PackageKey) {
        let mut served = self.served.borrow_mut();
        if !served.contains(key) {
            served.push(key.clone());
        }
    }
}

#[async_trait::async_trait(?Send)]
impl PackageProvider for InMemoryPackageProvider {
    async fn resolve_package(
        &self,
        key: &PackageKey,
        _referrer: Option<&ReferrerRef>,
    ) -> Result<Rc<ResolvedPackage>, PackageError> {
        let version = self
            .latest
            .get(key)
            .ok_or_else(|| PackageError::NotFound(key.to_user_specifier()))?;
        let resolved = self
            .by_version
            .get(&(key.clone(), version.clone()))
            .cloned()
            .ok_or_else(|| PackageError::NotFound(key.to_user_specifier()))?;
        self.note_served(key);
        Ok(resolved)
    }

    fn get_cached(&self, key: &PackageKey, version: &str) -> Option<Rc<ResolvedPackage>> {
        let cached = self
            .by_version
            .get(&(key.clone(), version.to_string()))
            .cloned()?;
        self.note_served(key);
        Some(cached)
    }

    fn get_resolved(&self, key: &PackageKey) -> Option<Rc<ResolvedPackage>> {
        let version = self.latest.get(key)?;
        self.by_version
            .get(&(key.clone(), version.clone()))
            .cloned()
    }

    /// Union the `permissions` of **every** package version this provider holds. The
    /// per-isolate engine builds one provider per isolate, so a test seeds exactly that
    /// isolate's closure here (root + the `smudgy://` deps it resolves) — making the
    /// "all held packages" union equal to the closure union the cloud `fork` computes.
    fn closure_permissions(&self) -> PackagePermissions {
        let mut union = PackagePermissions::default();
        for package in self.by_version.values() {
            union.merge(&package.manifest.permissions);
        }
        union
    }

    fn loaded_packages(&self) -> Vec<PackageKey> {
        self.served.borrow().clone()
    }

    /// A stub fetch is a read of the producer's declarations, not a code load — serve it
    /// without recording the key in `served` (which would misfire the stumble diagnostic).
    async fn resolve_package_for_stub(
        &self,
        key: &PackageKey,
    ) -> Result<Rc<ResolvedPackage>, PackageError> {
        let version = self
            .latest
            .get(key)
            .ok_or_else(|| PackageError::NotFound(key.to_user_specifier()))?;
        self.by_version
            .get(&(key.clone(), version.clone()))
            .cloned()
            .ok_or_else(|| PackageError::NotFound(key.to_user_specifier()))
    }

    fn set_home_packages(&self, homes: Vec<PackageKey>) {
        *self.homes.borrow_mut() = Some(homes.iter().map(PackageKey::folded).collect());
    }

    fn is_home_load(&self, key: &PackageKey) -> bool {
        self.homes
            .borrow()
            .as_ref()
            .is_none_or(|set| set.contains(&key.folded()))
    }

    fn note_scrubbed(&self, key: &PackageKey) {
        let mut scrubbed = self.scrubbed.borrow_mut();
        if !scrubbed.contains(key) {
            scrubbed.push(key.clone());
        }
    }

    fn scrubbed_packages(&self) -> Vec<PackageKey> {
        self.scrubbed.borrow().clone()
    }

    fn note_user_code_import(&self, key: &PackageKey) {
        let mut imports = self.user_imports.borrow_mut();
        if !imports.contains(key) {
            imports.push(key.clone());
        }
    }

    fn user_code_imports(&self) -> Vec<PackageKey> {
        self.user_imports.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(raw: &str) -> SmudgySpecifier {
        SmudgySpecifier::parse(raw).expect("valid specifier")
    }

    #[test]
    fn widgets_specifiers_collide_with_the_marker_scheme() {
        // Why exact-string resolve() arms are needed BEFORE the marker branch: both author
        // specifiers parse as the bare `smudgy` MARKER scheme, so without the early match they
        // would mis-route into the package marker loader and hard-fail.
        assert_eq!(
            ModuleSpecifier::parse("smudgy:widgets").unwrap().scheme(),
            MARKER_SCHEME
        );
        assert_eq!(
            ModuleSpecifier::parse("smudgy:widgets/jsx-runtime")
                .unwrap()
                .scheme(),
            MARKER_SCHEME
        );
    }

    #[test]
    fn widgets_synthesized_urls_use_the_widgets_scheme() {
        // The synthesized URLs carry the distinct hyphenated `smudgy-widgets` scheme so `load()`
        // can dispatch them, with the author subpath consumed entirely by the resolve() match.
        assert_eq!(widgets_jsx_runtime_url().scheme(), WIDGETS_SCHEME);
        assert_eq!(widgets_jsx_runtime_url().path(), "/jsx-runtime");
        assert_eq!(widgets_module_url("repl").scheme(), WIDGETS_SCHEME);
        assert_eq!(widgets_module_url("repl").path(), "/user");
        assert!(
            widgets_module_url("file:///srv/modules/hud.tsx")
                .path()
                .starts_with("/mod")
        );
    }

    #[test]
    fn widgets_modules_synthesize_without_error() {
        // Both shapes load: the shared, provenance-free jsx-runtime and a per-importer surface.
        assert!(load_widgets_module(&widgets_jsx_runtime_url()).is_ok());
        assert!(load_widgets_module(&widgets_module_url("repl")).is_ok());
    }

    #[test]
    fn param_kinds_round_trip_through_json() {
        // The `type` discriminator is lowercase; `options`/`fields` are omitted when empty and
        // carried verbatim when present. A list/table column is itself a scalar param.
        let manifest = PackageManifest {
            version: "1.0.0".to_string(),
            description: String::new(),
            entry: None,
            min_smudgy_version: None,
            dependencies: Vec::new(),
            requires: Vec::new(),
            hosts: Vec::new(),
            params: vec![
                PackageParameter {
                    key: "mode".to_string(),
                    label: Some("Mode".to_string()),
                    secret: false,
                    required: false,
                    kind: ParamKind::Dropdown,
                    default: Some(serde_json::Value::String("fast".to_string())),
                    options: vec![
                        ParamOption { value: "fast".to_string(), label: None },
                        ParamOption { value: "slow".to_string(), label: Some("Careful".to_string()) },
                    ],
                    fields: Vec::new(),
                },
                PackageParameter {
                    key: "aliases".to_string(),
                    label: None,
                    secret: false,
                    required: false,
                    kind: ParamKind::List,
                    default: None,
                    options: Vec::new(),
                    fields: vec![PackageParameter {
                        key: "item".to_string(),
                        label: None,
                        secret: false,
                        required: false,
                        kind: ParamKind::String,
                        default: None,
                        options: Vec::new(),
                        fields: Vec::new(),
                    }],
                },
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
                            label: None,
                            secret: false,
                            required: false,
                            kind: ParamKind::String,
                            default: None,
                            options: Vec::new(),
                            fields: Vec::new(),
                        },
                        PackageParameter {
                            key: "hops".to_string(),
                            label: None,
                            secret: false,
                            required: false,
                            kind: ParamKind::Number,
                            default: None,
                            options: Vec::new(),
                            fields: Vec::new(),
                        },
                    ],
                },
            ],
            permissions: PackagePermissions::default(),
            importable: true,
        };

        let json = serde_json::to_string(&manifest).expect("serialize");
        assert!(json.contains("\"type\":\"dropdown\""));
        assert!(json.contains("\"type\":\"list\""));
        assert!(json.contains("\"type\":\"table\""));
        // A bare String scalar omits its empty options/fields entirely.
        assert!(!json.contains("\"options\":[]"));
        let back = PackageManifest::parse(&json).expect("round-trips");
        assert_eq!(back, manifest);

        assert!(ParamKind::List.is_container() && ParamKind::Table.is_container());
        assert!(ParamKind::String.is_scalar() && ParamKind::Dropdown.is_scalar());
    }

    #[test]
    fn dropdown_option_label_falls_back_to_value() {
        assert_eq!(
            ParamOption { value: "x".to_string(), label: None }.display_label(),
            "x"
        );
        assert_eq!(
            ParamOption { value: "x".to_string(), label: Some(String::new()) }.display_label(),
            "x"
        );
        assert_eq!(
            ParamOption { value: "x".to_string(), label: Some("Ex".to_string()) }.display_label(),
            "Ex"
        );
    }

    #[test]
    fn parses_package_specifier() {
        let s = spec("smudgy://wbk/mapper");
        assert_eq!(s.owner, "wbk");
        assert_eq!(s.name, "mapper");
        assert_eq!(s.subpath, None);
    }

    #[test]
    fn parses_subpath_specifier() {
        let s = spec("smudgy://wbk/mapper/lib/util");
        assert_eq!(s.name, "mapper");
        assert_eq!(s.subpath.as_deref(), Some("lib/util"));
    }

    #[test]
    fn normalizes_trailing_slash() {
        assert_eq!(
            spec("smudgy://wbk/mapper/").to_user_specifier(),
            "smudgy://wbk/mapper"
        );
    }

    #[test]
    fn rejects_malformed_specifiers() {
        assert!(matches!(
            SmudgySpecifier::parse("https://example.com/x"),
            Err(SmudgySpecifierError::MissingScheme)
        ));
        // A bare owner with no name is rejected.
        assert!(matches!(
            SmudgySpecifier::parse("smudgy://wbk"),
            Err(SmudgySpecifierError::EmptyComponent("name"))
        ));
        // An empty owner segment is rejected.
        assert!(matches!(
            SmudgySpecifier::parse("smudgy:///mapper"),
            Err(SmudgySpecifierError::EmptyComponent("owner"))
        ));
        assert!(matches!(
            SmudgySpecifier::parse("smudgy://wbk/"),
            Err(SmudgySpecifierError::EmptyComponent("name"))
        ));
        assert!(matches!(
            SmudgySpecifier::parse("smudgy://wbk/mapper/../etc"),
            Err(SmudgySpecifierError::InvalidSubpath(_))
        ));
    }

    #[test]
    fn marker_url_round_trips() {
        for raw in [
            "smudgy://wbk/mapper",
            "smudgy://wbk/mapper/lib/util",
            "smudgy://Some-User/speedwalk",
        ] {
            let original = spec(raw);
            let marker = original.to_marker_url();
            // The marker is a valid URL that round-trips through string form.
            assert_eq!(
                ModuleSpecifier::parse(marker.as_str()).unwrap(),
                marker,
                "marker {marker} must round-trip as a URL"
            );
            let recovered =
                SmudgySpecifier::from_marker_url(&marker).expect("marker decodes back");
            assert_eq!(recovered, original, "marker for {raw} must decode losslessly");
        }
    }

    #[test]
    fn identical_specifiers_produce_identical_markers() {
        // The dedup guarantee starts here: deno keys its module map on resolve()'s
        // output, so two imports of the same package MUST resolve byte-identically.
        let a = spec("smudgy://wbk/mapper").to_marker_url();
        let b = spec("smudgy://wbk/mapper/").to_marker_url();
        assert_eq!(a.as_str(), b.as_str());
    }

    #[test]
    fn referrer_marker_round_trips_and_keys_the_module() {
        let app = PackageKey {
            owner: "user".into(),
            name: "app".into(),
        };
        let other = PackageKey {
            owner: "user".into(),
            name: "other".into(),
        };

        let from_app = spec("smudgy://wbk/util").with_referrer(app.clone(), "1.0.0");
        let marker = from_app.to_marker_url();
        // The marker round-trips back to the same specifier incl. its referrer instance.
        assert_eq!(SmudgySpecifier::from_marker_url(&marker), Some(from_app.clone()));
        let recovered = SmudgySpecifier::from_marker_url(&marker).unwrap();
        assert_eq!(recovered.referrer().map(|r| &r.key), Some(&app));
        assert_eq!(recovered.referrer().map(|r| r.version.as_str()), Some("1.0.0"));

        // Same target + same referrer instance → identical marker (one deno instance).
        let from_app_again = spec("smudgy://wbk/util").with_referrer(app.clone(), "1.0.0");
        assert_eq!(marker.as_str(), from_app_again.to_marker_url().as_str());

        // Same target + a DIFFERENT importer package → a different marker.
        let from_other = spec("smudgy://wbk/util").with_referrer(other, "1.0.0");
        assert_ne!(marker.as_str(), from_other.to_marker_url().as_str());

        // Same target + same importer but a DIFFERENT importer VERSION → a different marker
        // (so two coexisting versions of the importer select their deps independently).
        let from_app_v2 = spec("smudgy://wbk/util").with_referrer(app, "2.0.0");
        assert_ne!(marker.as_str(), from_app_v2.to_marker_url().as_str());

        // A bare (referrer-less) import stays bare and decodes to no referrer.
        let bare = spec("smudgy://wbk/util").to_marker_url();
        assert!(bare.query().is_none());
        assert_eq!(SmudgySpecifier::from_marker_url(&bare).unwrap().referrer(), None);
    }

    #[test]
    fn params_module_url_is_per_package_and_round_trips() {
        let app = PackageKey {
            owner: "wbk".into(),
            name: "app".into(),
        };
        let other = PackageKey {
            owner: "other".into(),
            name: "combat".into(),
        };

        let app_url = params_module_url(Some(&app));
        // Same package -> same URL (one synthesized module instance).
        assert_eq!(app_url, params_module_url(Some(&app)));
        // Different package -> different URL (independent param namespace).
        assert_ne!(app_url.as_str(), params_module_url(Some(&other)).as_str());
        // A non-package importer binds to no package.
        assert_ne!(app_url.as_str(), params_module_url(None).as_str());

        assert_eq!(parse_params_url(&app_url), Some(Some(app)));
        assert_eq!(parse_params_url(&params_module_url(None)), Some(None));
        // A non-params URL isn't recognized.
        assert_eq!(parse_params_url(&ModuleSpecifier::parse("file:///x").unwrap()), None);
    }

    #[test]
    fn params_module_synthesis_binds_to_the_importer() {
        let app = PackageKey {
            owner: "wbk".into(),
            name: "app".into(),
        };
        let source = load_params_module(&params_module_url(Some(&app))).expect("synthesizes");
        let ModuleSourceCode::String(code) = source.code else {
            panic!("expected string module source");
        };
        let code = code.as_str();
        // The importer's specifier is baked in, and get bridges to the host hook.
        assert!(code.contains("\"smudgy://wbk/app\""), "binds the caller's spec: {code}");
        assert!(code.contains("globalThis.__smudgy_param_get"));
        assert!(code.contains("export function get(key)"));

        // A no-package module's get short-circuits to undefined (empty spec is falsy).
        let none = load_params_module(&params_module_url(None)).expect("synthesizes");
        let ModuleSourceCode::String(code) = none.code else {
            panic!("expected string module source");
        };
        assert!(code.as_str().contains("const __spec = \"\""), "no package -> empty spec");
    }

    #[test]
    fn core_module_url_is_per_importer_and_round_trips() {
        // A package's modules coarsen to one creator instance per owner/name/version.
        let from_index = core_module_url("smudgy-pkg:///wbk/mapper/1.4.0/index.ts");
        let from_lib = core_module_url("smudgy-pkg:///wbk/mapper/1.4.0/lib/util.ts");
        assert_eq!(from_index, from_lib, "a package's modules share one core instance");
        // A different version is a different creator.
        let v2 = core_module_url("smudgy-pkg:///wbk/mapper/2.0.0/index.ts");
        assert_ne!(from_index.as_str(), v2.as_str());

        // Local modules are keyed per-file (distinct instances), and stable per file.
        let a = core_module_url("file:///srv/modules/combat/healer.ts");
        let b = core_module_url("file:///srv/modules/combat/damage.ts");
        assert_ne!(a.as_str(), b.as_str(), "two local modules get distinct core instances");
        assert_eq!(
            a.as_str(),
            core_module_url("file:///srv/modules/combat/healer.ts").as_str()
        );

        // The baked creator descriptor recovers from each URL (parse, don't assume key order).
        let pkg: serde_json::Value =
            serde_json::from_str(&core_creator_json(&from_index).unwrap()).unwrap();
        assert_eq!(pkg["kind"], "package");
        assert_eq!(pkg["owner"], "wbk");
        assert_eq!(pkg["name"], "mapper");
        assert_eq!(pkg["version"], "1.4.0");

        let module: serde_json::Value =
            serde_json::from_str(&core_creator_json(&a).unwrap()).unwrap();
        assert_eq!(module["kind"], "module");
        assert_eq!(module["referrer"], "file:///srv/modules/combat/healer.ts");

        // A non-file, non-package referrer (e.g. npm/jsr) falls back to the user namespace.
        let user: serde_json::Value =
            serde_json::from_str(&core_creator_json(&core_module_url("npm:left-pad")).unwrap())
                .unwrap();
        assert_eq!(user["kind"], "user");

        // A non-core URL isn't recognized.
        assert_eq!(
            core_creator_json(&ModuleSpecifier::parse("file:///x").unwrap()),
            None
        );
    }

    #[test]
    fn core_module_synthesis_binds_the_creator() {
        let url = core_module_url("smudgy-pkg:///wbk/mapper/1.4.0/index.ts");
        let ModuleSourceCode::String(code) = load_core_module(&url).expect("synthesizes").code
        else {
            panic!("expected string module source");
        };
        let code = code.as_str();
        assert!(code.contains(r#""kind":"package""#), "bakes the creator: {code}");
        assert!(code.contains("globalThis.__smudgy_create_api"), "{code}");
        assert!(code.contains("export const createAlias"), "{code}");
        assert!(code.contains("export const createTrigger"), "{code}");
        assert!(code.contains("export const createTriggers"), "{code}");
        // The creator-bound timer/hotkey factories + registries are named exports too.
        assert!(code.contains("export const createTimer"), "{code}");
        assert!(code.contains("export const createHotkey"), "{code}");
        assert!(code.contains("export const timers = __api.timers;"), "{code}");
        assert!(code.contains("export const hotkeys = __api.hotkeys;"), "{code}");
        // The convenience surface is delivered as named exports too.
        for name in [
            "send", "sendRaw", "echo", "style", "link", "reload", "capture", "line", "buffer",
            "vars", "byName",
        ] {
            assert!(
                code.contains(&format!("export const {name} = __api.{name};")),
                "missing convenience export {name}: {code}"
            );
        }
        // The interop handle constructors + the dynamic events lookup are named exports; no
        // string event bus (`on`/`once`/`emit`) is exported (interop.md §11).
        for name in ["createState", "createEvent", "createDerived", "events"] {
            assert!(
                code.contains(&format!("export const {name} = __api.{name};")),
                "missing interop export {name}: {code}"
            );
        }
        for name in ["emit", "on", "once"] {
            assert!(
                !code.contains(&format!("export const {name} ")),
                "removed string-event export {name} resurfaced: {code}"
            );
        }
        // The live-accessor members ride the default export, not named exports.
        assert!(code.contains("export default __api;"), "{code}");
    }

    #[test]
    fn kind_scheme_urls_fold_and_round_trip() {
        // Package form, case-folded, with the dep-gate key returned.
        let (url, key) = kind_scheme_url(InteropKind::State, "Kapusniak/Arctic-Prompt").unwrap();
        assert_eq!(url.as_str(), "smudgy-state:///pkg/kapusniak/arctic-prompt");
        assert_eq!(
            key.unwrap().to_user_specifier(),
            "smudgy://kapusniak/arctic-prompt"
        );
        let parsed = parse_kind_scheme_url(&url).unwrap();
        assert_eq!(parsed.kind, InteropKind::State);
        assert!(parsed.handle.is_none());
        assert!(matches!(parsed.target, KindSchemeTarget::Package(_)));

        // Single-handle subpath form: the handle rides the query, folded.
        let (url, _) = kind_scheme_url(InteropKind::State, "o/p/PromptState").unwrap();
        let parsed = parse_kind_scheme_url(&url).unwrap();
        assert_eq!(parsed.handle.as_deref(), Some("promptstate"));

        // Platform form: single segment, reserved unconditionally.
        let (url, key) = kind_scheme_url(InteropKind::Event, "sys").unwrap();
        assert_eq!(url.as_str(), "smudgy-events:///host/sys");
        assert!(key.is_none());
        let parsed = parse_kind_scheme_url(&url).unwrap();
        assert!(matches!(parsed.target, KindSchemeTarget::Platform(p) if p == "sys"));

        // Malformed references fail with the intended spelling.
        assert!(kind_scheme_url(InteropKind::State, "").is_err());
        assert!(kind_scheme_url(InteropKind::State, "loneowner").is_err());
        assert!(kind_scheme_url(InteropKind::Event, "a/b/c/d").is_err());
    }

    #[test]
    fn consumer_synthesis_exports_name_strings_with_folded_aliases() {
        let code = synthesize_consumer_code(
            "smudgy://o/p",
            InteropKind::State,
            &["PromptState".to_string(), "roster".to_string()],
            None,
        );
        assert!(code.contains(r#"globalThis.__smudgy_interop_consumer("smudgy://o/p")"#), "{code}");
        assert!(code.contains(r#"__c.state("PromptState")"#), "{code}");
        // Canonical casing + the folded lenient alias.
        assert!(code.contains(r#"as "PromptState""#), "{code}");
        assert!(code.contains(r#"as "promptstate""#), "{code}");
        // An already-folded name gets one export, and no default in whole-module form.
        assert_eq!(code.matches(r#"as "roster""#).count(), 1, "{code}");
        assert!(!code.contains("export default"), "{code}");

        // The subpath form narrows to the selected (folded) handle and default-exports it.
        let code = synthesize_consumer_code(
            "smudgy://o/p",
            InteropKind::Event,
            &["Prompt".to_string(), "other".to_string()],
            Some("prompt"),
        );
        assert!(code.contains(r#"__c.event("Prompt")"#), "{code}");
        assert!(code.contains("export default __h0;"), "{code}");
        assert!(!code.contains(r#""other""#), "{code}");
    }

    #[test]
    fn canonical_url_round_trips() {
        let key = PackageKey {
            owner: "wbk".into(),
            name: "mapper".into(),
        };
        let url = canonical_url(&key, "1.4.0", "lib/util.ts");
        let coords = parse_canonical(&url).expect("canonical decodes");
        assert_eq!(coords.key, key);
        assert_eq!(coords.version, "1.4.0");
        assert_eq!(coords.module_subpath, "lib/util.ts");
        assert_eq!(ModuleSpecifier::parse(url.as_str()).unwrap(), url);
    }

    #[test]
    fn relative_import_resolves_within_package() {
        // The crux: a relative import from inside a package must join against the
        // canonical URL and stay within the same package@version. This is why the
        // canonical scheme is path-based with an empty authority.
        let key = PackageKey {
            owner: "wbk".into(),
            name: "mapper".into(),
        };
        let entry = canonical_url(&key, "1.4.0", "index.js");
        let resolved = deno_core::resolve_import("./lib/util.js", entry.as_str())
            .expect("relative import resolves");
        let coords = parse_canonical(&resolved).expect("resolved is canonical");
        assert_eq!(coords.key, key);
        assert_eq!(coords.version, "1.4.0");
        assert_eq!(coords.module_subpath, "lib/util.js");
    }

    #[test]
    fn relative_parent_import_stays_in_package() {
        let key = PackageKey {
            owner: "wbk".into(),
            name: "mapper".into(),
        };
        let nested = canonical_url(&key, "2.0.0", "lib/inner/deep.js");
        let resolved = deno_core::resolve_import("../shared.js", nested.as_str())
            .expect("parent relative import resolves");
        let coords = parse_canonical(&resolved).expect("resolved is canonical");
        assert_eq!(coords.version, "2.0.0");
        assert_eq!(coords.module_subpath, "lib/shared.js");
    }

    #[test]
    fn manifest_parses_with_defaults() {
        // A legacy `name` key is tolerated (silently ignored) now that the name is implied from
        // the folder; an absent `description` defaults to empty.
        let manifest = PackageManifest::parse(r#"{ "name": "mapper", "version": "1.0.0" }"#)
            .expect("minimal manifest parses");
        assert_eq!(manifest.version, "1.0.0");
        assert!(manifest.description.is_empty());
        assert!(manifest.dependencies.is_empty());
        assert!(manifest.hosts.is_empty());
        assert!(manifest.params.is_empty());
        assert!(manifest.permissions.is_empty());
    }

    #[test]
    fn manifest_round_trips_description() {
        let manifest = PackageManifest::parse(r#"{ "version": "1.0.0", "description": "A handy mapper" }"#)
            .expect("manifest with description parses");
        assert_eq!(manifest.description, "A handy mapper");
        // A set description serializes back out…
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(json.contains(r#""description":"A handy mapper""#));
        // …but an empty one is omitted (skip_serializing_if).
        let empty = PackageManifest::parse(r#"{ "version": "1.0.0" }"#).unwrap();
        assert!(!serde_json::to_string(&empty).unwrap().contains("description"));
    }

    #[test]
    fn manifest_accepts_legacy_options_alias() {
        let manifest = PackageManifest::parse(
            r#"{ "name": "x", "version": "1.0.0", "options": [{ "key": "pg.url", "secret": true }] }"#,
        )
        .expect("legacy options key parses");
        assert_eq!(manifest.params.len(), 1);
        assert_eq!(manifest.params[0].key, "pg.url");
        assert!(manifest.params[0].secret);
    }

    #[test]
    fn parses_dependency_with_and_without_range() {
        let ranged = PackageDependency::parse("smudgy://wbk/util@^1.2")
            .expect("is a smudgy dep")
            .expect("parses");
        assert_eq!(ranged.key.owner, "wbk");
        assert_eq!(ranged.key.name, "util");
        assert_eq!(ranged.range.as_deref(), Some("^1.2"));

        let bare = PackageDependency::parse("smudgy://wbk/util")
            .expect("is a smudgy dep")
            .expect("parses");
        assert_eq!(bare.key.name, "util");
        assert_eq!(bare.range, None);

        // An exact pin (`@=x`) is just a range string the engine treats as a pin.
        let pinned = PackageDependency::parse("smudgy://wbk/util@=1.0.0")
            .unwrap()
            .unwrap();
        assert_eq!(pinned.range.as_deref(), Some("=1.0.0"));
    }

    #[test]
    fn dependency_parse_skips_non_smudgy() {
        assert!(PackageDependency::parse("jsr:@std/encoding@^1").is_none());
        assert!(PackageDependency::parse("npm:ms@2").is_none());
        assert!(PackageDependency::parse("./local.ts").is_none());
    }

    #[test]
    fn manifest_smudgy_dependencies_filters_and_parses() {
        let manifest = PackageManifest::parse(
            r#"{ "name": "app", "version": "1.0.0", "dependencies": [
                "jsr:@std/encoding@^1",
                "npm:ms@2",
                "smudgy://wbk/util@^1.2",
                "smudgy://other/helper"
            ] }"#,
        )
        .unwrap();
        let deps = manifest.smudgy_dependencies();
        assert_eq!(deps.len(), 2, "only the two smudgy:// deps");
        assert_eq!(deps[0].key.name, "util");
        assert_eq!(deps[0].range.as_deref(), Some("^1.2"));
        assert_eq!(deps[1].key.name, "helper");
        assert_eq!(deps[1].range, None);
    }

    #[test]
    fn manifest_full_round_trips() {
        let json = r#"{
            "name": "mapper",
            "version": "1.4.0",
            "entry": "index.ts",
            "min_smudgy_version": "0.3.0",
            "dependencies": ["jsr:@std/encoding@^1", "npm:ms@2", "smudgy://wbk/util"],
            "hosts": ["mud.arctic.org"],
            "params": [
                { "key": "pg.url", "label": "Postgres URL", "secret": true, "required": true },
                { "key": "autosave", "type": "bool", "default": true }
            ],
            "permissions": { "net": ["comms.coreclan.org:6379"] }
        }"#;
        let manifest = PackageManifest::parse(json).expect("full manifest parses");
        assert_eq!(manifest.dependencies.len(), 3);
        assert_eq!(manifest.min_smudgy_version.as_deref(), Some("0.3.0"));
        assert_eq!(manifest.hosts, vec!["mud.arctic.org"]);
        assert_eq!(manifest.params.len(), 2);
        assert!(manifest.params[0].secret);
        assert!(manifest.params[0].required);
        assert_eq!(manifest.params[1].kind, ParamKind::Bool);
        assert_eq!(manifest.permissions.net, vec!["comms.coreclan.org:6379"]);

        // Round-trip: serialize then re-parse yields an equal manifest.
        let serialized = serde_json::to_string(&manifest).unwrap();
        assert_eq!(PackageManifest::parse(&serialized).unwrap(), manifest);
    }

    fn package(version: &str, modules: &[(&str, &str)]) -> ResolvedPackage {
        ResolvedPackage {
            key: PackageKey {
                owner: "wbk".into(),
                name: "mapper".into(),
            },
            resolved_version: version.into(),
            manifest: PackageManifest::parse(&format!(
                r#"{{ "name": "mapper", "version": "{version}" }}"#
            ))
            .unwrap(),
            integrity: "sha256-test".into(),
            modules: modules
                .iter()
                .map(|(subpath, text)| PackageModuleSource {
                    subpath: (*subpath).to_string(),
                    text: (*text).to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn resolves_entry_module_by_convention() {
        let pkg = package("1.0.0", &[("index.ts", "export const x = 1;")]);
        let module = pkg.resolve_module(None).expect("entry resolves");
        assert_eq!(module.subpath, "index.ts");
    }

    #[test]
    fn resolves_subpath_with_extension_inference() {
        let pkg = package(
            "1.0.0",
            &[("index.ts", "x"), ("lib/util.ts", "export const u = 2;")],
        );
        let module = pkg.resolve_module(Some("lib/util")).expect("subpath resolves");
        assert_eq!(module.subpath, "lib/util.ts");
    }

    #[test]
    fn missing_module_errors() {
        let pkg = package("1.0.0", &[("index.ts", "x")]);
        assert!(matches!(
            pkg.resolve_module(Some("nope")),
            Err(PackageError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn in_memory_provider_resolves_latest() {
        let mut provider = InMemoryPackageProvider::new();
        provider.insert(package("1.0.0", &[("index.ts", "x")]));
        provider.insert(package("1.1.0", &[("index.ts", "y")]));
        let key = PackageKey {
            owner: "wbk".into(),
            name: "mapper".into(),
        };
        let resolved = provider.resolve_package(&key, None).await.expect("resolves");
        assert_eq!(resolved.resolved_version, "1.1.0");
        assert!(provider.get_cached(&key, "1.0.0").is_some());
    }

    #[test]
    fn package_permissions_merge_unions_per_field_and_dedups() {
        let mut base = PackagePermissions {
            net: vec!["a:1".into()],
            read: vec!["$DATA/x".into()],
            write: Vec::new(),
            env: vec!["TOK".into()],
            ..Default::default()
        };
        base.merge(&PackagePermissions {
            net: vec!["a:1".into(), "b".into()], // "a:1" already present → deduped
            read: vec!["$DATA/y".into()],
            write: vec!["$DATA/w".into()],
            env: Vec::new(),
            ..Default::default()
        });
        assert_eq!(base.net, vec!["a:1", "b"], "net unions and dedups (first-seen order)");
        assert_eq!(base.read, vec!["$DATA/x", "$DATA/y"], "read unions");
        assert_eq!(base.write, vec!["$DATA/w"], "write picks up the other's grant");
        assert_eq!(base.env, vec!["TOK"], "env keeps the original");
        // Merging an empty set is a no-op (the union is monotonic).
        let before = base.clone();
        base.merge(&PackagePermissions::default());
        assert_eq!(base, before);
    }

    #[test]
    fn package_permissions_added_since_is_normalized_per_field_set_difference() {
        // The update-delta: what a newly-resolved union asks for beyond the consented one.
        let consented = PackagePermissions {
            net: vec!["Host:6379".into()], // note the capital H
            read: vec!["$DATA/maps".into()],
            write: Vec::new(),
            env: vec!["TOK".into()],
            ..Default::default()
        };

        // Equal-or-shrunk → nothing ADDED (the auto-accept case): the re-cased host normalizes
        // equal, and dropping the env ask only shrinks exposure.
        let same_or_smaller = PackagePermissions {
            net: vec!["host:6379".into()], // re-cased host → not "added"
            read: vec!["$DATA/maps".into()],
            write: Vec::new(),
            env: Vec::new(), // env shrank
            ..Default::default()
        };
        assert!(
            same_or_smaller.added_since(&consented).is_empty(),
            "a re-cased or shrunk union adds nothing"
        );

        // A new version that genuinely ADDS asks: a new host, a new read path, a first write.
        let grown = PackagePermissions {
            net: vec!["host:6379".into(), "api.example.com".into()],
            read: vec!["$DATA/maps".into(), "$DATA/cache".into()],
            write: vec!["$DATA/maps".into()],
            env: vec!["TOK".into()],
            ..Default::default()
        };
        let added = grown.added_since(&consented);
        assert_eq!(added.net, vec!["api.example.com"], "only the genuinely-new host is added");
        assert_eq!(added.read, vec!["$DATA/cache"], "only the new read path is added");
        assert_eq!(added.write, vec!["$DATA/maps"], "a first write is wholly new");
        assert!(added.env.is_empty(), "the env ask was already consented");

        // Baseline ∅ (never consented) → the whole new union is added (deduped, original order).
        assert_eq!(
            grown.added_since(&PackagePermissions::default()),
            grown,
            "with no prior consent every entry is new"
        );
    }

    #[test]
    fn package_permissions_is_within_gates_the_permission_capped_resolver() {
        // `is_within` decides whether a candidate version's closure union fits the consented grant
        // (the permission-capped resolver loads the highest version that does; else refuses).
        let consented = PackagePermissions {
            net: vec!["Host:6379".into()],
            read: vec!["$DATA/maps".into()],
            write: Vec::new(),
            env: vec!["TOK".into()],
            ..Default::default()
        };

        // Equal, shrunk, or re-cased all fit (grant nothing beyond consent).
        assert!(consented.is_within(&consented), "a union is within itself");
        assert!(
            PackagePermissions {
                net: vec!["host:6379".into()], // re-cased
                read: Vec::new(),              // shrank
                write: Vec::new(),
                env: Vec::new(),
                ..Default::default()
            }
            .is_within(&consented),
            "a re-cased/shrunk union is within consent"
        );
        // The empty union fits any grant.
        assert!(PackagePermissions::default().is_within(&consented));

        // Any genuinely-new ask (host/path/var, or a first write) does NOT fit → that version is
        // blocked, the resolver must fall to a lower version (or refuse).
        assert!(
            !PackagePermissions {
                net: vec!["Host:6379".into(), "api.example.com".into()],
                ..consented.clone()
            }
            .is_within(&consented),
            "a new host exceeds the grant"
        );
        assert!(
            !PackagePermissions {
                write: vec!["$DATA/maps".into()],
                ..consented.clone()
            }
            .is_within(&consented),
            "a first write exceeds the grant"
        );
        // Nothing is within the empty consent except the empty union (the never-consented case:
        // only a zero-permission version could load).
        assert!(!consented.is_within(&PackagePermissions::default()));
        assert!(PackagePermissions::default().is_within(&PackagePermissions::default()));
    }

    #[test]
    fn is_within_net_uses_host_port_subsumption() {
        // A consented BARE host covers any `host:port` on it (deno `NetDescriptor` semantics) — the
        // grant is wider than the ask, so the version fits and the ask adds nothing.
        let bare = PackagePermissions {
            net: vec!["comms.coreclan.org".into()],
            ..Default::default()
        };
        let ported = PackagePermissions {
            net: vec!["comms.coreclan.org:6379".into()],
            ..Default::default()
        };
        assert!(ported.is_within(&bare), "a bare-host grant covers a specific port on it");
        assert!(ported.added_since(&bare).net.is_empty(), "...so it adds nothing");
        // Re-cased host still matches (case-insensitive).
        let recased = PackagePermissions {
            net: vec!["Comms.CoreClan.ORG:6379".into()],
            ..Default::default()
        };
        assert!(recased.is_within(&bare), "host comparison is case-insensitive");

        // An exact `host:port` grant covers ONLY that port: a bare (any-port) ask exceeds it, and so
        // does a different port — both are genuinely-new asks.
        assert!(!bare.is_within(&ported), "a port-scoped grant does not cover any-port");
        assert_eq!(
            bare.added_since(&ported).net,
            vec!["comms.coreclan.org"],
            "the any-port ask is newly added over a port-scoped grant"
        );
        let other_port = PackagePermissions {
            net: vec!["comms.coreclan.org:6380".into()],
            ..Default::default()
        };
        assert!(!other_port.is_within(&ported), "a port-scoped grant covers only its own port");
        assert_eq!(
            other_port.added_since(&ported).net,
            vec!["comms.coreclan.org:6380"],
            "a different port is a genuinely-new ask"
        );
    }

    #[test]
    fn import_policy_is_an_ordered_lattice_and_independent_of_net() {
        use ImportPolicy::{Any, None as ImpNone, Registries};
        // Ordered None < Registries < Any; the closure union is the max (escalates monotonically).
        assert!(ImpNone < Registries && Registries < Any);
        let mut p = PackagePermissions { import: ImpNone, ..Default::default() };
        p.merge(&PackagePermissions { import: Registries, ..Default::default() });
        assert_eq!(p.import, Registries, "merge raises to the higher level");
        p.merge(&PackagePermissions { import: ImpNone, ..Default::default() });
        assert_eq!(p.import, Registries, "merging a lower level is a no-op");

        // is_within: a level fits iff it is no higher than the ceiling.
        let any = PackagePermissions { import: Any, ..Default::default() };
        let reg = PackagePermissions { import: Registries, ..Default::default() };
        assert!(reg.is_within(&any), "Registries fits under Any");
        assert!(!any.is_within(&reg), "Any exceeds a Registries grant");
        assert!(PackagePermissions::default().is_within(&reg), "None fits any grant");

        // added_since: the escalation only — the new level when it exceeds the consented baseline.
        assert_eq!(any.added_since(&reg).import, Any, "raising to Any is newly requested");
        assert_eq!(reg.added_since(&any).import, ImpNone, "narrowing adds nothing");
        assert_eq!(reg.added_since(&reg).import, ImpNone, "the same level adds nothing");

        // `import` is independent of `net`: a pure-net grant leaves import at None and never covers
        // an import ask.
        let net_only = PackagePermissions { net: vec!["jsr.io".into()], ..Default::default() };
        assert_eq!(net_only.import, ImpNone, "a net grant doesn't grant import");
        assert!(!reg.is_within(&net_only), "a net grant does not cover an import ask");
    }

    #[test]
    fn import_policy_allows_import_decision_table() {
        use ImportPolicy::{Any, None as ImpNone, Registries};
        // The exact gate the loader runs, per scheme × level (host matters only for http/https).
        // npm / jsr: off at None, on at Registries and Any.
        for scheme in ["npm", "jsr"] {
            assert!(!ImpNone.allows_import(scheme, ""), "{scheme} denied at None");
            assert!(Registries.allows_import(scheme, ""), "{scheme} allowed at Registries");
            assert!(Any.allows_import(scheme, ""), "{scheme} allowed at Any");
        }
        // Arbitrary https/http: off at None and Registries, on only at Any.
        for scheme in ["http", "https"] {
            assert!(!ImpNone.allows_import(scheme, "cdn.example.com"), "{scheme} denied at None");
            assert!(
                !Registries.allows_import(scheme, "cdn.example.com"),
                "arbitrary {scheme} denied at Registries"
            );
            assert!(Any.allows_import(scheme, "cdn.example.com"), "{scheme} allowed at Any");
        }
        // The jsr.io CDN is the one https host allowed at Registries (a jsr package's own
        // sub-modules), case-insensitively — but still denied at None.
        assert!(Registries.allows_import("https", "jsr.io"), "jsr.io https allowed at Registries");
        assert!(Registries.allows_import("https", "JSR.IO"), "jsr.io match is case-insensitive");
        assert!(!ImpNone.allows_import("https", "jsr.io"), "jsr.io https denied at None");

        // Non-external schemes are never gated here (smudgy://, smudgy-pkg:, file:) at any level.
        for level in [ImpNone, Registries, Any] {
            assert!(level.allows_import("smudgy", ""), "smudgy:// never import-gated");
            assert!(level.allows_import("smudgy-pkg", ""), "smudgy-pkg: never import-gated");
            assert!(level.allows_import("file", ""), "file: never import-gated");
        }
    }

    #[test]
    fn is_within_paths_use_subtree_prefix() {
        // A consented directory grant covers its whole subtree (deno read/write semantics).
        let grant = PackagePermissions {
            read: vec!["$DATA/maps".into()],
            ..Default::default()
        };
        let nested = PackagePermissions {
            read: vec!["$DATA/maps/regions/eu.json".into()],
            ..Default::default()
        };
        assert!(nested.is_within(&grant), "a path under the grant is covered");
        assert!(nested.added_since(&grant).read.is_empty(), "...so it adds nothing");

        // A sibling that merely shares a string prefix is NOT in the subtree (component boundary).
        let sibling = PackagePermissions {
            read: vec!["$DATA/maps-2".into()],
            ..Default::default()
        };
        assert!(!sibling.is_within(&grant), "a prefix-sharing sibling is not in the subtree");
        assert_eq!(
            sibling.added_since(&grant).read,
            vec!["$DATA/maps-2"],
            "the sibling path is a genuinely-new ask"
        );
        // The reverse: a broader parent dir exceeds a grant scoped to a child.
        let parent = PackagePermissions {
            read: vec!["$DATA".into()],
            ..Default::default()
        };
        assert!(!parent.is_within(&grant), "a broader parent dir exceeds a child-scoped grant");

        // `write` uses the same subtree semantics, and a trailing slash / backslash is ignored.
        let w_grant = PackagePermissions {
            write: vec!["$DATA/maps/".into()],
            ..Default::default()
        };
        let w_nested = PackagePermissions {
            write: vec![r"$DATA\maps\cache".into()],
            ..Default::default()
        };
        assert!(w_nested.is_within(&w_grant), "write subtree containment matches read");
    }

    #[test]
    fn is_within_treats_dropped_dotdot_escape_as_inert() {
        // The enforcement guardrail DROPS a `$DATA/..` escape (it would leave the data dir), so it
        // grants nothing — `is_within`/`added_since` must mirror that: a requested escape neither
        // blocks an otherwise-fitting version nor shows as a new ask (the engine drops it exactly as
        // it would on a fresh install), keeping the resolver gate, the delta, and the container in
        // agreement.
        let consent = PackagePermissions {
            read: vec!["$DATA/maps".into()],
            ..Default::default()
        };
        let with_escape = PackagePermissions {
            read: vec!["$DATA/maps".into(), "$DATA/../secrets".into()],
            ..Default::default()
        };
        assert!(
            with_escape.is_within(&consent),
            "a dropped $DATA/.. escape is inert and must not block the version"
        );
        assert!(
            with_escape.added_since(&consent).read.is_empty(),
            "...and is never surfaced as a newly-added ask"
        );

        // A *granted* escape covers nothing — a real path under it is still a genuine new ask.
        let escape_grant = PackagePermissions {
            read: vec![r"$DATA\..\shared".into()],
            ..Default::default()
        };
        let real_path = PackagePermissions {
            read: vec!["$DATA/other".into()],
            ..Default::default()
        };
        assert!(
            !real_path.is_within(&escape_grant),
            "a dropped escape grant covers nothing"
        );
        assert_eq!(
            real_path.added_since(&consent).read,
            vec!["$DATA/other"],
            "a genuine new (non-escape) path is still added"
        );
    }

    #[test]
    fn is_within_resolves_dotdot_in_non_data_paths() {
        // Unlike a `$DATA/..` escape — which the engine
        // guardrail *drops* (handled by `is_within_treats_dropped_dotdot_escape_as_inert`) — a
        // non-`$DATA` absolute path is handed to `deno_permissions` verbatim, which canonicalizes
        // `..` and grants the RESOLVED path. So the subtree compare must resolve `..` the same way,
        // or a textual prefix match lets an escape slip through: a consented `/home/u/srv` does NOT
        // cover `/home/u/srv/../other` (the container would grant `/home/u/other`, an escape the user
        // never consented to), and the escape must be reported as a newly-added ask so the
        // auto-accept "shrink" branch can't silently record it.
        let consent = PackagePermissions {
            read: vec!["/home/u/srv".into()],
            ..Default::default()
        };
        let escape = PackagePermissions {
            read: vec!["/home/u/srv/../other".into()],
            ..Default::default()
        };
        assert!(
            !escape.is_within(&consent),
            "a `..` escaping the consented subtree is NOT covered (it resolves to /home/u/other)"
        );
        assert_eq!(
            escape.added_since(&consent).read,
            vec!["/home/u/srv/../other"],
            "...and the escape is surfaced as a newly-added ask (verbatim) so the shrink branch \
             must re-prompt for consent instead of silently recording it"
        );

        // A `..` that resolves back INTO the subtree is genuinely covered — the fix resolves `..`,
        // it does not blanket-reject every path that merely contains one.
        let in_subtree = PackagePermissions {
            read: vec!["/home/u/srv/sub/../deeper".into()],
            ..Default::default()
        };
        assert!(
            in_subtree.is_within(&consent),
            "a `..` that stays within the grant after resolution (/home/u/srv/deeper) is covered"
        );
        assert!(
            in_subtree.added_since(&consent).read.is_empty(),
            "...so it is not surfaced as a new ask"
        );
    }

    #[test]
    fn in_memory_closure_permissions_unions_held_packages() {
        // Each isolate is seeded with one provider over its closure; `closure_permissions`
        // returns the union of every held package's declared permissions (deny-all when none).
        fn pkg_with(name: &str, perms_json: &str) -> ResolvedPackage {
            ResolvedPackage {
                key: PackageKey {
                    owner: "wbk".into(),
                    name: name.into(),
                },
                resolved_version: "1.0.0".into(),
                manifest: PackageManifest::parse(&format!(
                    r#"{{ "name": "{name}", "version": "1.0.0", "permissions": {perms_json} }}"#
                ))
                .unwrap(),
                integrity: "test".into(),
                modules: vec![PackageModuleSource {
                    subpath: "index.js".into(),
                    text: String::new(),
                }],
            }
        }
        let mut provider = InMemoryPackageProvider::new();
        // An empty provider denies everything (the safe default).
        assert!(provider.closure_permissions().is_empty());
        provider.insert(pkg_with("root", r#"{ "read": ["$DATA/maps"] }"#));
        provider.insert(pkg_with("dep", r#"{ "net": ["host:6379"] }"#));
        let union = provider.closure_permissions();
        assert_eq!(union.net, vec!["host:6379"], "the dep's net joins the union");
        assert_eq!(union.read, vec!["$DATA/maps"], "the root's read joins the union");
        assert!(union.write.is_empty() && union.env.is_empty());
    }

    // -----------------------------------------------------------------------
    // smudgy op-capabilities
    // -----------------------------------------------------------------------

    fn perms_with_smudgy(smudgy_json: &str) -> PackagePermissions {
        let manifest = PackageManifest::parse(&format!(
            r#"{{ "name": "p", "version": "1.0.0", "permissions": {{ "smudgy": {smudgy_json} }} }}"#
        ))
        .expect("valid manifest");
        manifest.permissions
    }

    #[test]
    fn smudgy_block_parses_tokens_into_booleans() {
        let perms = perms_with_smudgy(
            r#"{ "automations": ["triggers"], "session": ["send", "echo"], "display": ["change"] }"#,
        );
        let s = perms.smudgy;
        assert!(s.create_triggers && s.send && s.echo && s.change_display);
        assert!(
            !s.create_aliases && !s.send_direct && !s.reach_others && !s.widgets,
            "un-requested tokens stay denied"
        );
    }

    #[test]
    fn smudgy_mapper_write_implies_read() {
        let write = perms_with_smudgy(r#"{ "mapper": ["write"] }"#).smudgy;
        assert!(write.mapper_write && write.mapper_read, "write implies read");
        let read = perms_with_smudgy(r#"{ "mapper": ["read"] }"#).smudgy;
        assert!(read.mapper_read && !read.mapper_write, "read alone is not write");
    }

    #[test]
    fn smudgy_tokens_are_case_insensitive_and_drop_unknowns() {
        let perms = perms_with_smudgy(r#"{ "session": ["SEND", " Reach-Others "], "widgets": ["bogus"] }"#);
        assert!(perms.smudgy.send && perms.smudgy.reach_others);
        assert!(!perms.smudgy.widgets, "an unknown widgets token is ignored, not granted");
    }

    #[test]
    fn smudgy_absent_block_denies_everything() {
        let manifest =
            PackageManifest::parse(r#"{ "name": "p", "version": "1.0.0" }"#).expect("valid manifest");
        assert!(manifest.permissions.smudgy.is_empty(), "no smudgy block ⇒ all denied");
    }

    #[test]
    fn smudgy_capabilities_round_trip_through_serde() {
        // A write grant round-trips to `["write"]` (re-implying read), so the booleans are stable.
        let caps = perms_with_smudgy(r#"{ "session": ["send"], "mapper": ["write"] }"#).smudgy;
        let json = serde_json::to_string(&caps).expect("serialize");
        let back: SmudgyCapabilities = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(caps, back, "capabilities survive a serde round-trip");
        assert!(back.mapper_read && back.mapper_write && back.send);
    }

    #[test]
    fn smudgy_interop_tokens_parse_and_round_trip() {
        let caps = perms_with_smudgy(r#"{ "interop": ["read", "write"] }"#).smudgy;
        assert!(caps.interop_read && caps.interop_write);
        let read_only = perms_with_smudgy(r#"{ "interop": ["read"] }"#).smudgy;
        assert!(read_only.interop_read && !read_only.interop_write);
        let json = serde_json::to_string(&caps).expect("serialize");
        assert!(
            json.contains(r#""interop""#) && json.contains(r#""events""#),
            "serialization dual-emits the canonical `interop` and the legacy `events` alias so \
             pre-interop clients (which drop the unknown `interop` key) keep the capability until \
             the 0.5.x cut: {json}"
        );
        let back: SmudgyCapabilities = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, caps, "the dual-emitted form round-trips to the same capabilities");
    }

    #[test]
    fn smudgy_gmcp_tokens_parse_and_round_trip() {
        let caps = perms_with_smudgy(r#"{ "gmcp": ["send"] }"#).smudgy;
        assert!(caps.gmcp_send);
        assert!(
            !caps.interop_read && !caps.interop_write && !caps.send,
            "gmcp:send rides with no other grant (docs/gmcp-plan.md §6.3)"
        );
        let json = serde_json::to_string(&caps).expect("serialize");
        assert!(json.contains(r#""gmcp""#), "{json}");
        let back: SmudgyCapabilities = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, caps, "gmcp tokens round-trip");
        // Containment: a gmcp ask needs a gmcp grant.
        let none = SmudgyCapabilities::default();
        assert!(!caps.is_within(&none));
        assert!(caps.added_since(&none).gmcp_send);
        assert!(caps.is_within(&SmudgyCapabilities::all()));
    }

    #[test]
    fn legacy_events_tokens_alias_onto_interop() {
        // Pre-interop manifests and unmigrated lockfile consent records use
        // `events: ["emit","subscribe"]`; each token grants the interop capability it aliases.
        // The alias — and this test — are deleted at 0.5.x (the version assert beside
        // `SmudgyCapabilitiesWire` enforces it).
        let caps = perms_with_smudgy(r#"{ "events": ["emit", "subscribe"] }"#).smudgy;
        assert!(
            caps.interop_read && caps.interop_write,
            "events tokens must grant the aliased interop capabilities"
        );
        let sub_only = perms_with_smudgy(r#"{ "events": ["subscribe"] }"#).smudgy;
        assert!(sub_only.interop_read && !sub_only.interop_write);
        let emit_only = perms_with_smudgy(r#"{ "events": ["emit"] }"#).smudgy;
        assert!(emit_only.interop_write && !emit_only.interop_read);
        // An aliased grant is *within* an interop grant and vice versa — consent comparisons
        // can never see a difference between the spellings.
        let canonical = perms_with_smudgy(r#"{ "interop": ["read", "write"] }"#).smudgy;
        assert!(caps.is_within(&canonical) && canonical.is_within(&caps));
        assert!(caps.added_since(&canonical).is_empty());
    }

    #[test]
    fn smudgy_is_within_is_boolean_implication() {
        let granted = SmudgyCapabilities {
            send: true,
            echo: true,
            ..Default::default()
        };
        let asks_subset = SmudgyCapabilities {
            send: true,
            ..Default::default()
        };
        let asks_more = SmudgyCapabilities {
            send: true,
            send_direct: true,
            ..Default::default()
        };
        assert!(asks_subset.is_within(&granted), "a subset of grants fits");
        assert!(
            !asks_more.is_within(&granted),
            "an ask for an un-granted capability (send-direct) does NOT fit"
        );
        // mapper write covers a read ask (write implies read).
        let writer = SmudgyCapabilities { mapper_write: true, mapper_read: true, ..Default::default() };
        let reader = SmudgyCapabilities { mapper_read: true, ..Default::default() };
        assert!(reader.is_within(&writer), "a write grant covers a read ask");
        assert!(!writer.is_within(&reader), "a read grant does NOT cover a write ask");
    }

    #[test]
    fn smudgy_added_since_is_the_capability_delta() {
        let old = SmudgyCapabilities { send: true, ..Default::default() };
        let new = SmudgyCapabilities { send: true, send_direct: true, change_display: true, ..Default::default() };
        let added = new.added_since(&old);
        assert!(added.send_direct && added.change_display, "the new asks are surfaced");
        assert!(!added.send, "an already-granted capability is not 'added'");
        assert!(
            new.added_since(&new).is_empty(),
            "no delta when nothing new is requested"
        );
        assert!(
            old.added_since(&new).is_empty(),
            "a shrink is not an addition (auto-accept)"
        );
    }

    #[test]
    fn package_permissions_is_within_and_added_since_cover_smudgy() {
        // A version that newly wants `send-direct` is capped out even if its deno perms fit.
        let consented = perms_with_smudgy(r#"{ "session": ["send"] }"#);
        let widening = perms_with_smudgy(r#"{ "session": ["send", "send-direct"] }"#);
        assert!(consented.is_within(&consented));
        assert!(
            !widening.is_within(&consented),
            "a widening smudgy ask blocks the version (resolution capping, §5)"
        );
        let delta = widening.added_since(&consented);
        assert!(
            delta.smudgy.send_direct && !delta.is_empty(),
            "the update delta surfaces the newly-wanted smudgy capability"
        );
    }

    #[test]
    fn merge_unions_smudgy_capabilities_across_the_closure() {
        // The closure union ORs each dep's smudgy capabilities.
        let mut root = perms_with_smudgy(r#"{ "session": ["send"] }"#);
        let dep = perms_with_smudgy(r#"{ "display": ["change"], "session": ["echo"] }"#);
        root.merge(&dep);
        assert!(root.smudgy.send && root.smudgy.echo && root.smudgy.change_display);
        assert!(!root.smudgy.send_direct, "un-requested capabilities stay denied after merge");
    }
}
