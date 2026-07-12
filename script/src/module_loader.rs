use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use deno_core::error::ModuleLoaderError;
use deno_core::{
    ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader, ModuleSource,
    ModuleSourceCode, ModuleSpecifier, ModuleType, ResolutionKind,
};
use deno_error::JsErrorBox;
use deno_semver::{Version, VersionReq};
use serde::Deserialize;
use serde_json::Value;

use crate::transpiler::transpile;
use crate::{generic_loader_error, ModulePolicy};

type SourceMapCache = RefCell<HashMap<String, Vec<u8>>>;

pub trait ImportProvider {
    fn resolve(
        &mut self,
        specifier: &ModuleSpecifier,
        referrer: &str,
        kind: ResolutionKind,
    ) -> Option<Result<ModuleSpecifier, ModuleLoaderError>> {
        let _ = (specifier, referrer, kind);
        None
    }

    fn load(
        &mut self,
        specifier: &ModuleSpecifier,
        maybe_referrer: Option<&ModuleLoadReferrer>,
    ) -> Option<Result<ModuleSource, ModuleLoaderError>> {
        let _ = (specifier, maybe_referrer);
        None
    }
}

pub struct ScriptModuleLoader {
    cwd: PathBuf,
    policy: ModulePolicy,
    source_maps: SourceMapCache,
    import_provider: RefCell<Box<dyn ImportProvider>>,
    /// Native npm support (deno npm stack). `npm:` specifiers are loaded via async
    /// `load()` (deno's async/!Send npm stack can't run under the sync ImportProvider
    /// or a nested block_on), so npm lives here rather than as an ImportProvider.
    npm: Option<std::rc::Rc<crate::npm_resolver::SmudgyNpmServices>>,
    /// `smudgy://` shared-package resolution. Like npm, loaded via async `load()`
    /// (network fetch on the session runtime), so it lives here rather than as an
    /// ImportProvider. `None` disables `smudgy://` imports.
    package_provider: Option<std::rc::Rc<dyn crate::package_resolver::PackageProvider>>,
}

/// Which manifest declaration authorizes a `smudgy://` reference made FROM a package module.
///
/// A code `import` is authorized *only* by `dependencies` — importing a package's code is the
/// version-locked closure relationship `dependencies` describes, and a `requires` root is
/// explicitly the thing that is *not* imported (`REQUIRED-PACKAGES.md`). Consuming a producer's
/// interop surface (`smudgy:state|events|procedures/…`) is authorized by `dependencies` **or**
/// `requires`: both guarantee the producer is installed and typed — all the gate exists to ensure
/// (interop.md §9) — and event-bus consumption is precisely what `requires` declares.
#[derive(Clone, Copy)]
enum DepGate {
    /// A `smudgy://…` code import — must appear in `dependencies`.
    CodeImport,
    /// A `smudgy:state|events|procedures/…` interop consumer — `dependencies` or `requires`.
    InteropConsume,
}

impl ScriptModuleLoader {
    pub fn new(cwd: PathBuf, policy: ModulePolicy) -> Self {
        Self::with_import_provider(cwd, policy, Box::new(JsrImportProvider::default()))
    }

    pub fn with_import_provider(
        cwd: PathBuf,
        policy: ModulePolicy,
        import_provider: Box<dyn ImportProvider>,
    ) -> Self {
        Self {
            cwd,
            policy,
            source_maps: Default::default(),
            import_provider: RefCell::new(import_provider),
            npm: None,
            package_provider: None,
        }
    }

    /// Loader with jsr resolution (via the ImportProvider) plus native npm
    /// resolution (async, handled in `load`).
    pub fn with_npm(
        cwd: PathBuf,
        policy: ModulePolicy,
        npm: std::rc::Rc<crate::npm_resolver::SmudgyNpmServices>,
    ) -> Self {
        Self {
            npm: Some(npm),
            ..Self::with_import_provider(cwd, policy, Box::new(JsrImportProvider::default()))
        }
    }

    /// Loader with jsr + native npm + `smudgy://` shared-package resolution. The
    /// package provider (if any) is consulted asynchronously in `load`.
    pub fn with_npm_and_packages(
        cwd: PathBuf,
        policy: ModulePolicy,
        npm: std::rc::Rc<crate::npm_resolver::SmudgyNpmServices>,
        package_provider: Option<std::rc::Rc<dyn crate::package_resolver::PackageProvider>>,
    ) -> Self {
        Self {
            npm: Some(npm),
            package_provider,
            ..Self::with_import_provider(cwd, policy, Box::new(JsrImportProvider::default()))
        }
    }

    /// Enforce that a `smudgy://` reference made FROM a package module is declared in that
    /// package's manifest — as a `dependency` for a code import, or a `dependency`/`requires` for
    /// an interop consumer (see [`DepGate`]). References from user modules (a non-canonical
    /// referrer) are unrestricted, as are self-references; only `smudgy://` is gated. Fails *open*
    /// if the importing package isn't in the provider cache (shouldn't happen on the load path)
    /// rather than reject a legitimate import.
    fn enforce_declared_smudgy_dep(
        &self,
        spec: &crate::package_resolver::SmudgySpecifier,
        referrer: &str,
        gate: DepGate,
    ) -> Result<(), ModuleLoaderError> {
        let Ok(referrer_url) = ModuleSpecifier::parse(referrer) else {
            return Ok(());
        };
        // Only a package's own modules (canonical referrer) are gated.
        let Some(coords) = crate::package_resolver::parse_canonical(&referrer_url) else {
            return Ok(());
        };
        let imported = spec.package_key();
        // A package may always reference itself by its full specifier.
        if imported == coords.key {
            return Ok(());
        }
        let Some(provider) = &self.package_provider else {
            return Ok(());
        };
        let Some(pkg) = provider.get_cached(&coords.key, &coords.version) else {
            return Ok(());
        };

        // Match on the package key (ignoring any `@range`), so a ranged declaration like
        // `smudgy://owner/util@^1.2` authorizes `import "smudgy://…/util"`. A code import checks
        // `dependencies` only; an interop consumer also accepts a `requires` root (consumed over
        // the bus, never imported), so a standalone producer needs no `dependencies` entry.
        let declared = pkg
            .manifest
            .smudgy_dependencies()
            .iter()
            .any(|dep| dep.key == imported)
            || matches!(gate, DepGate::InteropConsume)
                && pkg
                    .manifest
                    .smudgy_requires()
                    .iter()
                    .any(|dep| dep.key == imported);
        if declared {
            return Ok(());
        }
        let message = match gate {
            DepGate::CodeImport => format!(
                "package {} imports undeclared smudgy:// dependency {} — add it to the package manifest's \"dependencies\"",
                coords.key.name,
                spec.to_user_specifier()
            ),
            DepGate::InteropConsume => format!(
                "package {} consumes undeclared smudgy:// package {} — add it to the package manifest's \"requires\" (or \"dependencies\" if you also import its code)",
                coords.key.name,
                spec.to_user_specifier()
            ),
        };
        Err(generic_loader_error(message))
    }

    /// Resolve a kind-scheme (`smudgy:state/…` / `smudgy:events/…`) specifier: parse + fold the
    /// producer reference, and gate it as an interop consumer — consuming a package's state/events
    /// *is* depending on it (interop.md §9), authorized by a `requires` root or a
    /// `dependencies` code import (see [`DepGate::InteropConsume`]).
    fn resolve_kind_scheme(
        &self,
        kind: crate::interop_extract::InteropKind,
        rest: &str,
        referrer: &str,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        let (url, package) = crate::package_resolver::kind_scheme_url(kind, rest)
            .map_err(generic_loader_error)?;
        if let Some(key) = package {
            let spec = crate::package_resolver::SmudgySpecifier::parse(&key.to_user_specifier())
                .map_err(|err| {
                    generic_loader_error(format!("invalid producer {}: {err}", key.to_user_specifier()))
                })?;
            self.enforce_declared_smudgy_dep(&spec, referrer, DepGate::InteropConsume)?;
        }
        Ok(url)
    }

    /// Enforce this isolate's `import` policy (`PackagePermissions::import`, threaded in via
    /// [`ModulePolicy::import_policy`]) on a resolved remote import. The level decides which schemes
    /// may download code: `None` → only the smudgy ecosystem; `Registries` → + `npm:`/`jsr:` (and
    /// the `https://jsr.io` modules jsr resolves to); `Any` → + arbitrary `https:`/`http:`. The main
    /// isolate runs at `Any`. Non-remote schemes (`file`/`node`/`smudgy`/`smudgy-pkg`) are not
    /// code-download sources and always pass.
    ///
    /// Checked in [`Self::resolve`], the single point every import passes before any fetch, so a
    /// denied `jsr:`/`npm:` import never even reaches the registry for metadata.
    fn enforce_import_allowed(
        &self,
        resolved: &ModuleSpecifier,
    ) -> Result<(), ModuleLoaderError> {
        let scheme = resolved.scheme();
        if self.policy.import_policy.allows_import(scheme, resolved.host_str().unwrap_or_default()) {
            return Ok(());
        }
        // Name what was blocked in the package's own terms (the source, not the rewritten host).
        let source = match scheme {
            "npm" => "npm",
            "jsr" => "jsr",
            _ => "the web",
        };
        Err(generic_loader_error(format!(
            "import of {resolved} is blocked by this package's permissions: it may not download code \
             from {source} (raise its \"import\" permission in the package manifest)"
        )))
    }

    fn load_sync(&self, specifier: &ModuleSpecifier) -> Result<ModuleSource, ModuleLoaderError> {
        let source = match specifier.scheme() {
            "file" => {
                let path = specifier
                    .to_file_path()
                    .map_err(|_| generic_loader_error(format!("{specifier} is not a file URL")))?;
                // Name the path on failure — a bare `os error 2` (file not found), e.g. from
                // a relative import to a missing/renamed file, is otherwise undiagnosable.
                std::fs::read_to_string(&path).map_err(|e| {
                    generic_loader_error(format!("failed to read module {}: {e}", path.display()))
                })?
            }
            "http" | "https" => {
                if !self.policy.allow_https {
                    return Err(generic_loader_error(format!(
                        "https imports are disabled: {specifier}"
                    )));
                }
                // Must run the blocking HTTP off the async event-loop thread:
                // `load()` returns `Sync`, so it executes inside deno's async
                // context, where creating+dropping reqwest::blocking's tokio
                // runtime panics ("drop a runtime within an async context").
                // fetch_text isolates it on a spawned thread (as fetch_json does).
                fetch_text(specifier.as_str())?
            }
            "node" => {
                return Err(generic_loader_error(format!(
                    "node builtin {specifier} was not handled by deno_node"
                )));
            }
            "npm" => {
                return Err(generic_loader_error(format!(
                    "npm package {specifier} was not resolved before loading"
                )));
            }
            scheme => {
                return Err(generic_loader_error(format!(
                    "unsupported module scheme {scheme}: {specifier}"
                )));
            }
        };

        let module_type = if specifier.path().ends_with(".json") {
            ModuleType::Json
        } else {
            ModuleType::JavaScript
        };
        let (code, source_map) =
            transpile(specifier, &source).map_err(JsErrorBox::from_err)?;
        if let Some(source_map) = source_map {
            self.source_maps
                .borrow_mut()
                .insert(specifier.to_string(), source_map.to_vec());
        }

        Ok(ModuleSource::new(
            module_type,
            ModuleSourceCode::String(code.into()),
            specifier,
            None,
        ))
    }
}

impl ModuleLoader for ScriptModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        kind: ResolutionKind,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        if specifier.starts_with("node:") {
            return ModuleSpecifier::parse(specifier).map_err(JsErrorBox::from_err);
        }

        // `smudgy:params`: a per-importer virtual module. Bind it to the importing
        // package (a canonical referrer); a user/top-level importer binds to no package.
        if specifier == "smudgy:params" {
            let importer = ModuleSpecifier::parse(referrer)
                .ok()
                .and_then(|url| crate::package_resolver::parse_canonical(&url))
                .map(|coords| coords.key);
            return Ok(crate::package_resolver::params_module_url(importer.as_ref()));
        }

        // `smudgy:core`: a per-importer virtual module whose creation functions
        // (createAlias/createTrigger/createHotkey) are bound to the importing module's
        // provenance, so automations it creates are attributed to it. Keyed on the referrer
        // so each importer gets its own instance (package modules coarsen to one per
        // package@version; local modules are per-file), mirroring `smudgy:params`.
        if specifier == "smudgy:core" {
            return Ok(crate::package_resolver::core_module_url(referrer));
        }

        // `smudgy:widgets` (the script-driven UI surface) and its `smudgy:widgets/jsx-runtime`
        // (auto-appended by the automatic JSX runtime). EXACT-STRING match, placed BEFORE the
        // `smudgy://` branch and the `resolve_import` fallthrough: both specifiers parse with
        // scheme `smudgy` (== MARKER_SCHEME) and would otherwise mis-route into the package
        // marker loader and hard-fail. The author subpath is consumed entirely by this match,
        // so the `/jsx-runtime` slash never round-trips as a real subpath. Mirrors `smudgy:core`.
        if specifier == "smudgy:widgets" {
            return Ok(crate::package_resolver::widgets_module_url(referrer));
        }
        if specifier == "smudgy:widgets/jsx-runtime" {
            return Ok(crate::package_resolver::widgets_jsx_runtime_url());
        }

        // `smudgy:state/…` / `smudgy:events/…`: kind-scheme consumer modules — host-synthesized
        // stubs over a producer's declared handles; importing one never evaluates the producer
        // (interop.md §4). Like `smudgy:widgets`, matched BEFORE the `smudgy://` branch
        // and the fallthrough (they parse as bare MARKER-scheme URLs). Consuming a package is a
        // dependency (§9), so the same manifest gate as a code import applies.
        if let Some(rest) = specifier.strip_prefix("smudgy:state/") {
            return self.resolve_kind_scheme(
                crate::interop_extract::InteropKind::State,
                rest,
                referrer,
            );
        }
        if let Some(rest) = specifier.strip_prefix("smudgy:events/") {
            return self.resolve_kind_scheme(
                crate::interop_extract::InteropKind::Event,
                rest,
                referrer,
            );
        }
        if let Some(rest) = specifier.strip_prefix("smudgy:procedures/") {
            return self.resolve_kind_scheme(
                crate::interop_extract::InteropKind::Procedure,
                rest,
                referrer,
            );
        }
        // Kind-scheme misspellings fail loudly with the intended spelling (interop.md §4) instead of
        // falling through to the package-marker loader's mystifying "package not found".
        if specifier == "smudgy:state" || specifier == "smudgy:events" || specifier == "smudgy:procedures" {
            return Err(generic_loader_error(format!(
                "{specifier} needs a producer: import from {specifier}/<owner>/<package>"
            )));
        }
        if specifier == "smudgy:event" || specifier.starts_with("smudgy:event/") {
            return Err(generic_loader_error(format!(
                "unknown module {specifier} — did you mean smudgy:events/…?"
            )));
        }
        if specifier == "smudgy:states" || specifier.starts_with("smudgy:states/") {
            return Err(generic_loader_error(format!(
                "unknown module {specifier} — did you mean smudgy:state/…?"
            )));
        }
        if specifier == "smudgy:procedure" || specifier.starts_with("smudgy:procedure/") {
            return Err(generic_loader_error(format!(
                "unknown module {specifier} — did you mean smudgy:procedures/…?"
            )));
        }
        if specifier == "smudgy:message"
            || specifier == "smudgy:messages"
            || specifier.starts_with("smudgy:message/")
            || specifier.starts_with("smudgy:messages/")
        {
            return Err(generic_loader_error(format!(
                "unknown module {specifier} — messages are procedures now; import from smudgy:procedures/…"
            )));
        }

        // A user `smudgy://owner/name[/sub]` import: parse with our own parser (path-based,
        // not url::Url, so the marker/canonical URL spaces round-trip) and return the
        // version-less marker URL. `load()` resolves the version and redirects to the
        // canonical pinned URL. Relative imports from inside a package use the
        // canonical `smudgy-pkg:` scheme and fall through to the normal path below.
        if specifier.starts_with("smudgy://") {
            let spec = crate::package_resolver::SmudgySpecifier::parse(specifier).map_err(|err| {
                generic_loader_error(format!("invalid smudgy specifier {specifier}: {err}"))
            })?;
            // Dep-gating: a PACKAGE's module may only import `smudgy://` packages it
            // declared in its manifest `dependencies`. User modules (`<server>/modules/`,
            // `file://`) are unrestricted, and only `smudgy://` is gated (jsr:/npm:/url
            // imports are not). This keeps the manifest's smudgy:// dep list authoritative
            // for the backend's permission closure.
            self.enforce_declared_smudgy_dep(&spec, referrer, DepGate::CodeImport)?;
            // A user-level (file://) code import of a package: recorded so the host can warn
            // when the target declares interop handles — on main, a trusted package's home
            // load can't be scrubbed, so this import hands out live producer handles
            // (interop.md §1/§3, the accepted residual). The synthetic entry auto-imports
            // every installed package with a file: specifier and is machinery, not a user
            // script — it must not record, or every trusted handle package would warn on
            // every boot.
            if referrer.starts_with("file:") && referrer != crate::SYNTHETIC_ENTRY_SPECIFIER {
                if let Some(provider) = &self.package_provider {
                    provider.note_user_code_import(&spec.package_key());
                }
            }
            // Referrer-aware resolution: an import made FROM inside a package (a canonical
            // referrer) is tagged with that package instance (key + version), so the
            // provider selects the target's version from this importer's locked deps, and
            // two coexisting versions of the importer select independently. User/top-level
            // imports (non-canonical referrer) stay bare and resolve via lockfile / latest.
            let referrer_coords = ModuleSpecifier::parse(referrer)
                .ok()
                .and_then(|url| crate::package_resolver::parse_canonical(&url));
            let spec = match referrer_coords {
                Some(coords) => spec.with_referrer(coords.key, coords.version),
                None => spec,
            };
            return Ok(spec.to_marker_url());
        }

        let referrer = if referrer == "." || referrer.is_empty() {
            ModuleSpecifier::from_directory_path(&self.cwd)
                .map_err(|_| generic_loader_error("current directory is not a file URL"))?
                .to_string()
        } else {
            referrer.to_string()
        };
        let resolved = if specifier.starts_with("npm:") {
            ModuleSpecifier::parse(specifier).map_err(JsErrorBox::from_err)?
        } else {
            deno_core::resolve_import(specifier, &referrer).map_err(JsErrorBox::from_err)?
        };

        // Gate remote-code imports against this isolate's `import` allowlist BEFORE the jsr provider
        // (whose `resolve` fetches registry metadata) or the npm/https loaders run, so a denied
        // import fetches nothing at all. A no-op for the allow-all main isolate.
        self.enforce_import_allowed(&resolved)?;

        if let Some(result) = self
            .import_provider
            .borrow_mut()
            .resolve(&resolved, &referrer, kind)
        {
            return result;
        }

        match resolved.scheme() {
            // `smudgy-pkg` is the canonical scheme a package's relative imports resolve
            // into (e.g. `./util` from within a package); `smudgy` markers are emitted
            // by the early return above and never reach here.
            "file" | "http" | "https" | "node" | "npm" | "smudgy" | "smudgy-pkg" => Ok(resolved),
            scheme => Err(generic_loader_error(format!(
                "unsupported module scheme {scheme}: {resolved}"
            ))),
        }
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        maybe_referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        // npm: load async so deno_core drives it on the session runtime (its npm
        // stack is async + !Send; a nested block_on deadlocks the current-thread rt).
        if module_specifier.scheme() == "npm" {
            if let Some(npm) = &self.npm {
                let npm = npm.clone();
                let specifier = module_specifier.clone();
                let referrer = maybe_referrer.map(|r| r.specifier.clone());
                let cwd = self.cwd.clone();
                return ModuleLoadResponse::Async(Box::pin(async move {
                    let referrer = match referrer {
                        Some(referrer) => referrer,
                        None => ModuleSpecifier::from_directory_path(&cwd).map_err(|_| {
                            generic_loader_error("current directory is not a file URL")
                        })?,
                    };
                    npm.load_npm_async(&specifier, &referrer).await
                }));
            }
        }
        // smudgy:// marker → async resolve version + fetch + redirect to canonical.
        // Like npm, the fetch is async + runs on the session runtime (never block_on).
        if module_specifier.scheme() == crate::package_resolver::MARKER_SCHEME {
            if let Some(provider) = &self.package_provider {
                let provider = provider.clone();
                let specifier = module_specifier.clone();
                return ModuleLoadResponse::Async(Box::pin(async move {
                    crate::package_resolver::load_marker_module(provider, &specifier).await
                }));
            }
            return ModuleLoadResponse::Sync(Err(generic_loader_error(format!(
                "smudgy package import requires a package provider: {module_specifier}"
            ))));
        }
        // smudgy:params virtual module → synthesize the per-importer params accessor.
        if module_specifier.scheme() == crate::package_resolver::PARAMS_SCHEME {
            return ModuleLoadResponse::Sync(crate::package_resolver::load_params_module(
                module_specifier,
            ));
        }
        // smudgy:core virtual module → synthesize the per-importer creation API.
        if module_specifier.scheme() == crate::package_resolver::CORE_SCHEME {
            return ModuleLoadResponse::Sync(crate::package_resolver::load_core_module(
                module_specifier,
            ));
        }
        // smudgy:widgets virtual module (+ its /jsx-runtime) → synthesize the widget surface.
        if module_specifier.scheme() == crate::package_resolver::WIDGETS_SCHEME {
            return ModuleLoadResponse::Sync(crate::package_resolver::load_widgets_module(
                module_specifier,
            ));
        }
        // smudgy:state / smudgy:events / smudgy:procedures kind-scheme consumer module →
        // synthesize the stub. Async because a package producer's entry source is fetched
        // through the provider (parsed for handle names, never evaluated); platform catalogs
        // synthesize inline.
        if module_specifier.scheme() == crate::package_resolver::STATE_SCHEME
            || module_specifier.scheme() == crate::package_resolver::EVENTS_SCHEME
            || module_specifier.scheme() == crate::package_resolver::PROCEDURES_SCHEME
        {
            let provider = self.package_provider.clone();
            let specifier = module_specifier.clone();
            return ModuleLoadResponse::Async(Box::pin(async move {
                crate::package_resolver::load_kind_scheme_module(provider, &specifier).await
            }));
        }
        // smudgy-pkg canonical (sub-module or relative import) → serve from the set
        // already fetched by the marker load above.
        if module_specifier.scheme() == crate::package_resolver::CANONICAL_SCHEME {
            if let Some(provider) = &self.package_provider {
                let provider = provider.clone();
                let specifier = module_specifier.clone();
                return ModuleLoadResponse::Async(Box::pin(async move {
                    crate::package_resolver::load_canonical_module(provider, &specifier).await
                }));
            }
            return ModuleLoadResponse::Sync(Err(generic_loader_error(format!(
                "smudgy package module requires a package provider: {module_specifier}"
            ))));
        }
        if let Some(result) = self
            .import_provider
            .borrow_mut()
            .load(module_specifier, maybe_referrer)
        {
            return ModuleLoadResponse::Sync(result);
        }
        ModuleLoadResponse::Sync(self.load_sync(module_specifier))
    }

    fn get_source_map(&self, file_name: &str) -> Option<Cow<'_, [u8]>> {
        self.source_maps
            .borrow()
            .get(file_name)
            .cloned()
            .map(Cow::Owned)
    }
}

#[derive(Default)]
struct JsrImportProvider {
    package_meta: HashMap<String, JsrPackageMeta>,
    version_meta: HashMap<(String, String), JsrVersionMeta>,
}

impl ImportProvider for JsrImportProvider {
    fn resolve(
        &mut self,
        specifier: &ModuleSpecifier,
        _referrer: &str,
        _kind: ResolutionKind,
    ) -> Option<Result<ModuleSpecifier, ModuleLoaderError>> {
        if specifier.scheme() == "jsr" {
            Some(self.resolve_jsr_specifier(specifier))
        } else {
            None
        }
    }
}

impl JsrImportProvider {
    fn resolve_jsr_specifier(
        &mut self,
        specifier: &ModuleSpecifier,
    ) -> Result<ModuleSpecifier, ModuleLoaderError> {
        let request = parse_jsr_specifier(specifier)?;
        let version = self.resolve_version(&request.package, &request.range)?;
        let version_meta = self.version_meta(&request.package, &version)?;
        let export_path = version_meta.resolve_export(&request.export)?;

        ModuleSpecifier::parse(&format!(
            "https://jsr.io/{}/{}/{}",
            request.package,
            version,
            export_path.trim_start_matches("./")
        ))
        .map_err(JsErrorBox::from_err)
    }

    fn resolve_version(&mut self, package: &str, range: &str) -> Result<String, ModuleLoaderError> {
        let meta = self.package_meta(package)?;
        if range == "latest" {
            if let Some(latest) = &meta.latest {
                return Ok(latest.clone());
            }
        }

        let req = VersionReq::parse_from_specifier(range).map_err(|err| {
            generic_loader_error(format!("invalid jsr version range {range}: {err}"))
        })?;
        if req.tag().is_some() {
            return Err(generic_loader_error(format!(
                "unsupported jsr dist tag {range} for {package}"
            )));
        }

        meta.versions
            .keys()
            .filter_map(|version| {
                let parsed = Version::parse_standard(version).ok()?;
                req.matches(&parsed).then_some((parsed, version))
            })
            .max_by(|(a, _), (b, _)| a.cmp(b))
            .map(|(_, version)| version.clone())
            .ok_or_else(|| {
                generic_loader_error(format!(
                    "no jsr version of {package} satisfies range {range}"
                ))
            })
    }

    fn package_meta(&mut self, package: &str) -> Result<&JsrPackageMeta, ModuleLoaderError> {
        if !self.package_meta.contains_key(package) {
            let url = format!("https://jsr.io/{package}/meta.json");
            let meta = fetch_json::<JsrPackageMeta>(&url)?;
            self.package_meta.insert(package.to_string(), meta);
        }
        Ok(self
            .package_meta
            .get(package)
            .expect("package metadata cached"))
    }

    fn version_meta(
        &mut self,
        package: &str,
        version: &str,
    ) -> Result<&JsrVersionMeta, ModuleLoaderError> {
        let key = (package.to_string(), version.to_string());
        if !self.version_meta.contains_key(&key) {
            let url = format!("https://jsr.io/{package}/{version}_meta.json");
            let meta = fetch_json::<JsrVersionMeta>(&url)?;
            self.version_meta.insert(key.clone(), meta);
        }
        Ok(self
            .version_meta
            .get(&key)
            .expect("version metadata cached"))
    }
}

#[derive(Debug)]
struct JsrRequest {
    package: String,
    range: String,
    export: String,
}

#[derive(Debug, Clone, Deserialize)]
struct JsrPackageMeta {
    latest: Option<String>,
    versions: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct JsrVersionMeta {
    exports: HashMap<String, Value>,
}

impl JsrVersionMeta {
    fn resolve_export(&self, export: &str) -> Result<String, ModuleLoaderError> {
        let key = if export == "." {
            Cow::Borrowed(".")
        } else {
            Cow::Owned(format!("./{}", export.trim_start_matches("./")))
        };
        let value = self.exports.get(key.as_ref()).ok_or_else(|| {
            generic_loader_error(format!("jsr package does not export {}", key.as_ref()))
        })?;
        export_value_to_path(value).ok_or_else(|| {
            generic_loader_error(format!(
                "jsr export {} is not a supported string export",
                key.as_ref()
            ))
        })
    }
}

fn parse_jsr_specifier(specifier: &ModuleSpecifier) -> Result<JsrRequest, ModuleLoaderError> {
    let raw = specifier.as_str().trim_start_matches("jsr:");
    if !raw.starts_with('@') {
        return Err(generic_loader_error(format!(
            "jsr package must be scoped: {specifier}"
        )));
    }

    let without_at = &raw[1..];
    let (scope, rest) = without_at.split_once('/').ok_or_else(|| {
        generic_loader_error(format!(
            "invalid jsr specifier, missing package name: {specifier}"
        ))
    })?;
    let (name_with_range, export) = rest.split_once('/').unwrap_or((rest, "."));
    let (name, range) = name_with_range
        .rsplit_once('@')
        .unwrap_or((name_with_range, "latest"));
    if scope.is_empty() || name.is_empty() || range.is_empty() {
        return Err(generic_loader_error(format!(
            "invalid jsr specifier: {specifier}"
        )));
    }

    Ok(JsrRequest {
        package: format!("@{scope}/{name}"),
        range: range.to_string(),
        export: export.to_string(),
    })
}

fn export_value_to_path(value: &Value) -> Option<String> {
    match value {
        Value::String(path) => Some(path.clone()),
        Value::Object(map) => ["deno", "import", "default"]
            .into_iter()
            .filter_map(|key| map.get(key))
            .find_map(export_value_to_path),
        _ => None,
    }
}

fn fetch_json<T>(url: &str) -> Result<T, ModuleLoaderError>
where
    T: serde::de::DeserializeOwned + Send + 'static,
{
    let url = url.to_string();
    let thread_url = url.clone();
    std::thread::spawn(move || {
        let response = reqwest::blocking::get(&thread_url)
            .map_err(|err| generic_loader_error(format!("GET {thread_url} failed: {err}")))?;
        if !response.status().is_success() {
            return Err(generic_loader_error(format!(
                "GET {thread_url} failed with {}",
                response.status()
            )));
        }
        response
            .json::<T>()
            .map_err(|err| generic_loader_error(format!("invalid JSON from {thread_url}: {err}")))
    })
    .join()
    .map_err(|_| generic_loader_error(format!("GET {url} panicked")))?
}

/// Fetch a module source body as text. Like [`fetch_json`], the blocking HTTP
/// runs on a spawned thread so reqwest::blocking's tokio runtime is created and
/// dropped off the async event-loop thread (avoids a runtime-drop panic).
fn fetch_text(url: &str) -> Result<String, ModuleLoaderError> {
    let url = url.to_string();
    let thread_url = url.clone();
    std::thread::spawn(move || {
        let response = reqwest::blocking::get(&thread_url)
            .map_err(|err| generic_loader_error(format!("GET {thread_url} failed: {err}")))?;
        if !response.status().is_success() {
            return Err(generic_loader_error(format!(
                "GET {thread_url} failed with {}",
                response.status()
            )));
        }
        response
            .text()
            .map_err(|err| generic_loader_error(format!("reading {thread_url} failed: {err}")))
    })
    .join()
    .map_err(|_| generic_loader_error(format!("GET {url} panicked")))?
}

#[cfg(test)]
mod dep_gating_tests {
    use super::*;
    use crate::package_resolver::{
        InMemoryPackageProvider, PackageKey, PackageManifest, PackageModuleSource, ResolvedPackage,
    };
    use std::rc::Rc;

    /// A loader whose provider holds one package `wbk/app@1.0.0` with the given
    /// manifest, so we can exercise dep-gating with a canonical referrer.
    fn loader_with_app(manifest_json: &str) -> ScriptModuleLoader {
        let mut provider = InMemoryPackageProvider::new();
        provider.insert(ResolvedPackage {
            key: PackageKey {
                owner: "wbk".into(),
                name: "app".into(),
            },
            resolved_version: "1.0.0".into(),
            manifest: PackageManifest::parse(manifest_json).unwrap(),
            integrity: "sha256-test".into(),
            modules: vec![PackageModuleSource {
                subpath: "index.ts".into(),
                text: "export {};".into(),
            }],
        });
        ScriptModuleLoader {
            cwd: std::env::temp_dir(),
            policy: ModulePolicy { allow_https: true, ..Default::default() },
            source_maps: Default::default(),
            import_provider: RefCell::new(Box::new(JsrImportProvider::default())),
            npm: None,
            package_provider: Some(Rc::new(provider)),
        }
    }

    /// A canonical referrer = a module inside the `app` package (what deno passes when
    /// `app`'s code imports something).
    const APP_REFERRER: &str = "smudgy-pkg:///wbk/app/1.0.0/index.ts";

    #[test]
    fn package_may_import_a_declared_smudgy_dep() {
        let loader = loader_with_app(
            r#"{ "name": "app", "version": "1.0.0", "dependencies": ["smudgy://wbk/util"] }"#,
        );
        let resolved =
            loader.resolve("smudgy://wbk/util", APP_REFERRER, ResolutionKind::Import);
        assert!(resolved.is_ok(), "a declared smudgy:// dep is allowed: {resolved:?}");
    }

    #[test]
    fn package_may_not_import_an_undeclared_smudgy_dep() {
        let loader = loader_with_app(r#"{ "name": "app", "version": "1.0.0", "dependencies": [] }"#);
        let resolved =
            loader.resolve("smudgy://other/evil", APP_REFERRER, ResolutionKind::Import);
        assert!(resolved.is_err(), "an undeclared smudgy:// dep is denied");
    }

    #[test]
    fn package_may_reference_itself_without_declaring() {
        let loader = loader_with_app(r#"{ "name": "app", "version": "1.0.0" }"#);
        let resolved = loader.resolve(
            "smudgy://wbk/app/lib/extra",
            APP_REFERRER,
            ResolutionKind::Import,
        );
        assert!(resolved.is_ok(), "self-reference is allowed: {resolved:?}");
    }

    #[test]
    fn user_module_may_import_any_smudgy_package() {
        let loader = loader_with_app(r#"{ "name": "app", "version": "1.0.0", "dependencies": [] }"#);
        // A non-canonical (user-module) referrer is unrestricted.
        let resolved = loader.resolve(
            "smudgy://anyone#1/anything",
            "file:///home/user/script.ts",
            ResolutionKind::Import,
        );
        assert!(resolved.is_ok(), "user modules are not gated: {resolved:?}");
    }

    #[test]
    fn package_may_consume_events_of_a_required_producer() {
        // A standalone producer declared via `requires` (consumed over the bus, never imported)
        // authorizes its interop consumer — the arctic-mapper/arctic-prompt case.
        let loader = loader_with_app(
            r#"{ "name": "app", "version": "1.0.0", "requires": ["smudgy://wbk/prod"] }"#,
        );
        let resolved = loader.resolve("smudgy:events/wbk/prod", APP_REFERRER, ResolutionKind::Import);
        assert!(resolved.is_ok(), "a `requires` root authorizes consuming its events: {resolved:?}");
    }

    #[test]
    fn package_may_consume_events_of_a_dependency_producer() {
        // A code `dependency` also authorizes consuming the same package's interop surface — else
        // `import "smudgy://wbk/prod"` would succeed while `import "smudgy:events/wbk/prod"` fails.
        let loader = loader_with_app(
            r#"{ "name": "app", "version": "1.0.0", "dependencies": ["smudgy://wbk/prod"] }"#,
        );
        let resolved = loader.resolve("smudgy:events/wbk/prod", APP_REFERRER, ResolutionKind::Import);
        assert!(resolved.is_ok(), "a `dependencies` entry authorizes consuming its events: {resolved:?}");
    }

    #[test]
    fn package_may_not_consume_events_of_an_undeclared_producer() {
        let loader = loader_with_app(r#"{ "name": "app", "version": "1.0.0" }"#);
        let err = loader
            .resolve("smudgy:events/wbk/prod", APP_REFERRER, ResolutionKind::Import)
            .unwrap_err()
            .to_string();
        assert!(err.contains("requires"), "the interop-gate diagnostic points at `requires`: {err}");
    }

    #[test]
    fn requires_does_not_authorize_a_code_import() {
        // The asymmetry: a `requires` root is consumed, not imported. It must NOT satisfy the
        // `dependencies`-only gate on a `smudgy://` code import.
        let loader = loader_with_app(
            r#"{ "name": "app", "version": "1.0.0", "requires": ["smudgy://wbk/prod"] }"#,
        );
        let resolved = loader.resolve("smudgy://wbk/prod", APP_REFERRER, ResolutionKind::Import);
        assert!(resolved.is_err(), "a `requires` root is not importable as a code dependency");
    }

    /// A provider holding one library `gandalf/lib@1.0.0` with the given `importable` flag.
    fn provider_with_lib(importable: bool) -> Rc<InMemoryPackageProvider> {
        let mut provider = InMemoryPackageProvider::new();
        let manifest_json = format!(
            r#"{{ "version": "1.0.0", "importable": {importable} }}"#
        );
        provider.insert(ResolvedPackage {
            key: PackageKey { owner: "gandalf".into(), name: "lib".into() },
            resolved_version: "1.0.0".into(),
            manifest: PackageManifest::parse(&manifest_json).unwrap(),
            integrity: "sha256-test".into(),
            modules: vec![PackageModuleSource {
                subpath: "index.ts".into(),
                text: "export const x = 1;".into(),
            }],
        });
        Rc::new(provider)
    }

    /// Run `load_marker_module` for an import of `gandalf/lib` from `referrer` (None = a
    /// user/top-level import, Some = another package's instance), on a current-thread runtime.
    fn load_lib(
        importable: bool,
        referrer: Option<(&str, &str, &str)>,
    ) -> Result<ModuleSource, ModuleLoaderError> {
        let provider = provider_with_lib(importable);
        let mut spec = crate::package_resolver::SmudgySpecifier::parse("smudgy://gandalf/lib").unwrap();
        if let Some((owner, name, version)) = referrer {
            spec = spec.with_referrer(
                PackageKey { owner: owner.into(), name: name.into() },
                version,
            );
        }
        let marker = spec.to_marker_url();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(crate::package_resolver::load_marker_module(provider, &marker))
    }

    #[test]
    fn non_importable_denies_cross_owner_package_import() {
        let result = load_lib(false, Some(("frodo", "app", "1.0.0")));
        assert!(result.is_err(), "a cross-owner import of importable:false is denied");
    }

    #[test]
    fn non_importable_allows_same_owner_package_import() {
        let result = load_lib(false, Some(("gandalf", "other", "1.0.0")));
        assert!(result.is_ok(), "a same-owner sibling may import it: {result:?}");
    }

    #[test]
    fn non_importable_allows_user_top_level_import() {
        // No referrer = a user/top-level import; the user may import their own installed packages.
        let result = load_lib(false, None);
        assert!(result.is_ok(), "a user/top-level import is exempt: {result:?}");
    }

    #[test]
    fn importable_allows_cross_owner_package_import() {
        let result = load_lib(true, Some(("frodo", "app", "1.0.0")));
        assert!(result.is_ok(), "an importable package is freely importable: {result:?}");
    }
}

/// The per-isolate `import` policy gate (`ModulePolicy::import_policy`), exercised through
/// `resolve()` across the three levels. Hermetic by construction: `resolve()` fetches nothing for
/// `npm:`/`http:`/`https:` (only `load()` does), so those resolve synchronously, and every denied
/// case short-circuits at the gate. Only `jsr:` fetches registry metadata *during* `resolve` (via
/// the import provider), so the allowed-`jsr:` case is covered by the decision table in
/// `package_resolver::tests::import_policy_allows_import_decision_table`, keeping this offline.
#[cfg(test)]
mod import_gate_tests {
    use super::*;
    use crate::ImportPolicy;

    const REFERRER: &str = "file:///pkg/index.ts";

    /// A loader whose only gate is the per-isolate `import` policy (no npm stack / package provider).
    fn loader(import_policy: ImportPolicy) -> ScriptModuleLoader {
        ScriptModuleLoader {
            cwd: std::env::temp_dir(),
            policy: ModulePolicy { allow_https: true, import_policy },
            source_maps: Default::default(),
            import_provider: RefCell::new(Box::new(JsrImportProvider::default())),
            npm: None,
            package_provider: None,
        }
    }

    fn resolve(loader: &ScriptModuleLoader, specifier: &str) -> Result<ModuleSpecifier, ModuleLoaderError> {
        loader.resolve(specifier, REFERRER, ResolutionKind::Import)
    }

    #[test]
    fn none_blocks_every_external_scheme() {
        let off = loader(ImportPolicy::None);
        let npm = resolve(&off, "npm:left-pad").unwrap_err().to_string();
        assert!(npm.contains("blocked") && npm.contains("npm"), "npm blocked at None: {npm}");
        // `jsr:` is rejected at the gate — BEFORE the jsr provider's metadata fetch (so, offline).
        let jsr = resolve(&off, "jsr:@std/assert").unwrap_err().to_string();
        assert!(jsr.contains("blocked") && jsr.contains("jsr"), "jsr blocked at None: {jsr}");
        let web = resolve(&off, "https://cdn.example.com/x.js").unwrap_err().to_string();
        assert!(web.contains("blocked") && web.contains("the web"), "arbitrary https blocked at None: {web}");
    }

    #[test]
    fn registries_allows_npm_and_the_jsr_cdn_but_not_arbitrary_web() {
        let reg = loader(ImportPolicy::Registries);
        assert!(resolve(&reg, "npm:left-pad").is_ok(), "npm allowed at Registries");
        // A jsr package's own `https://jsr.io` sub-module resolves (resolve does not fetch https).
        assert!(
            resolve(&reg, "https://jsr.io/@std/assert/1.0.0/mod.ts").is_ok(),
            "the jsr.io CDN is allowed at Registries"
        );
        // Any OTHER web host is blocked.
        let web = resolve(&reg, "https://cdn.example.com/x.js").unwrap_err().to_string();
        assert!(
            web.contains("blocked") && web.contains("the web"),
            "arbitrary https blocked at Registries: {web}"
        );
    }

    #[test]
    fn any_allows_arbitrary_web() {
        let any = loader(ImportPolicy::Any);
        assert!(resolve(&any, "npm:left-pad").is_ok(), "npm allowed at Any");
        assert!(resolve(&any, "https://cdn.example.com/x.js").is_ok(), "arbitrary https allowed at Any");
        assert!(resolve(&any, "http://192.0.2.1:8080/x.js").is_ok(), "arbitrary http allowed at Any");
    }

    #[test]
    fn the_import_policy_does_not_touch_smudgy_schemes() {
        // Even the strictest level (None) leaves smudgy's own schemes alone: the `smudgy:core`/
        // `smudgy:widgets` virtual modules and `smudgy://` package imports resolve in their own
        // branches before the gate (and `smudgy://` is dep-gated, not import-gated). Guards against
        // a refactor moving the gate ahead of those early-returns.
        let off = loader(ImportPolicy::None);
        assert!(resolve(&off, "smudgy:core").is_ok(), "smudgy:core is not import-gated");
        assert!(resolve(&off, "smudgy:widgets").is_ok(), "smudgy:widgets is not import-gated");
        // A `smudgy://` import from a user/file referrer (unrestricted by dep-gating) returns its
        // marker URL — the point is it is NOT rejected with the import-block error.
        assert!(resolve(&off, "smudgy://wbk/util").is_ok(), "smudgy:// is not import-gated");
    }
}
