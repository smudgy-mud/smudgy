use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Context as AnyhowContext, Result};
use deno_cache_dir::npm::NpmCacheDir;
use deno_config::deno_json::NodeModulesLinkerMode;
use deno_core::error::ModuleLoaderError;
use deno_core::{FastString, ModuleSource, ModuleSourceCode, ModuleSpecifier};
use deno_error::JsErrorBox;
use deno_maybe_sync::new_rc;
use deno_node::{NodeExtInitServices, NodeRequireLoader};
use deno_npm::NpmSystemInfo;
use deno_npm::resolution::{AddPkgReqsOptions, NpmResolutionSnapshot, NpmVersionResolver};
use deno_npm_cache::{
    DownloadError, NpmCache, NpmCacheHttpClient, NpmCacheHttpClientBytesResponse,
    NpmCacheHttpClientResponse, NpmCacheSetting, NpmPackumentFormat, RegistryInfoProvider,
    TarballCache,
};
use deno_npmrc::{NpmRc, NpmRegistryUrl};
use deno_permissions::{OpenAccessKind, PermissionsContainer};
use deno_resolver::cjs::analyzer::{
    DenoCjsCodeAnalyzer, MemberReExport, ModuleExportAnalyzer, ModuleExportsAndReExports,
    ModuleForExportAnalysis, NullNodeAnalysisCache,
};
use deno_resolver::cjs::{CjsTracker, IsCjsResolutionMode};
use deno_resolver::loader::{LoadedModuleSource, NpmModuleLoader, RequestedModuleType};
use deno_resolver::npm::managed::{
    ManagedInNpmPkgCheckerCreateOptions, ManagedNpmResolverCreateOptions, NpmResolutionCell,
};
use deno_resolver::npm::{
    CreateInNpmPkgCheckerOptions, DenoInNpmPackageChecker, NpmReqResolver, NpmReqResolverOptions,
    NpmResolver, NpmResolverCreateOptions,
};
use deno_resolver::npmrc::ResolvedNpmRcRc;
use deno_semver::npm::NpmPackageReqReference;
use deno_semver::package::PackageReq;
use node_resolver::analyze::{CjsModuleExportAnalyzer, NodeCodeTranslator, NodeCodeTranslatorMode};
use node_resolver::cache::NodeResolutionSys;
use node_resolver::{
    DenoIsBuiltInNodeModuleChecker, NodeResolver, NodeResolverOptions, PackageJsonResolver,
    PackageJsonThreadLocalCache, ResolutionMode,
};
use reqwest::header::{ETAG, IF_NONE_MATCH};
use sys_traits::impls::RealSys;

use crate::generic_loader_error;

type SmudgyNpmResolver = NpmResolver<RealSys>;
type SmudgyNodeResolver = NodeResolver<
    DenoInNpmPackageChecker,
    DenoIsBuiltInNodeModuleChecker,
    SmudgyNpmResolver,
    RealSys,
>;
type SmudgyNodeResolverRc = node_resolver::NodeResolverRc<
    DenoInNpmPackageChecker,
    DenoIsBuiltInNodeModuleChecker,
    SmudgyNpmResolver,
    RealSys,
>;
type SmudgyNpmModuleLoader = NpmModuleLoader<
    DenoCjsCodeAnalyzer<RealSys>,
    DenoInNpmPackageChecker,
    DenoIsBuiltInNodeModuleChecker,
    SmudgyNpmResolver,
    RealSys,
>;

pub struct SmudgyNpmServices {
    editor: RefCell<Option<crate::npm_editor::NpmEditor>>,
    pub in_npm_package_checker: DenoInNpmPackageChecker,
    pub npm_resolver: SmudgyNpmResolver,
    node_resolver: SmudgyNodeResolverRc,
    req_resolver: NpmReqResolver<
        DenoInNpmPackageChecker,
        DenoIsBuiltInNodeModuleChecker,
        SmudgyNpmResolver,
        RealSys,
    >,
    registry_info_provider: Arc<RegistryInfoProvider<ReqwestNpmCacheHttpClient, RealSys>>,
    tarball_cache: Arc<TarballCache<ReqwestNpmCacheHttpClient, RealSys>>,
    npm_module_loader: SmudgyNpmModuleLoader,
    npm_resolution: deno_resolver::npm::managed::NpmResolutionCellRc,
    npm_system_info: NpmSystemInfo,
    version_resolver: NpmVersionResolver,
}

impl SmudgyNpmServices {
    pub fn new(
        data_dir: PathBuf,
    ) -> Result<(
        Rc<Self>,
        NodeExtInitServices<DenoInNpmPackageChecker, SmudgyNpmResolver, RealSys>,
    )> {
        Self::new_with_cache_setting(data_dir, NpmCacheSetting::Use)
    }

    fn new_with_cache_setting(
        data_dir: PathBuf,
        cache_setting: NpmCacheSetting,
    ) -> Result<(
        Rc<Self>,
        NodeExtInitServices<DenoInNpmPackageChecker, SmudgyNpmResolver, RealSys>,
    )> {
        let sys = RealSys;
        // Global-cache layout only: packages live under `<data_dir>/npm/<registry>/...`
        // and `require()` finds them through the resolution snapshot
        // (`maybe_node_modules_path: None` below). There must be NO
        // `<data_dir>/node_modules` mode anywhere -- see the paired
        // `has_node_modules_dir = false` in lib.rs, which keeps deno_node's
        // require() on the global-cache lookup path.
        let npm_root = data_dir.join("npm");
        std::fs::create_dir_all(&npm_root)
            .with_context(|| format!("failed to create npm cache dir {}", npm_root.display()))?;

        let npmrc = Arc::new(
            NpmRc::default()
                .as_resolved(&NpmRegistryUrl::for_npm(&sys))
                .context("failed to resolve npm registry configuration")?,
        );
        let npmrc_rc: ResolvedNpmRcRc = new_rc(npmrc.as_ref().clone());
        let npm_cache_dir = new_rc(NpmCacheDir::new(
            &sys,
            npm_root,
            npmrc.get_all_known_registries_urls(),
        ));
        let npm_resolution = new_rc(NpmResolutionCell::new(NpmResolutionSnapshot::default()));
        let node_resolution_sys = NodeResolutionSys::new(sys.clone(), None);
        let npm_system_info = NpmSystemInfo::default();

        let npm_resolver = NpmResolver::<RealSys>::new(NpmResolverCreateOptions::Managed(
            ManagedNpmResolverCreateOptions {
                npm_cache_dir: npm_cache_dir.clone(),
                sys: node_resolution_sys.clone(),
                maybe_node_modules_path: None,
                npm_system_info: npm_system_info.clone(),
                npmrc: npmrc_rc.clone(),
                npm_resolution: npm_resolution.clone(),
                linker_mode: NodeModulesLinkerMode::Isolated,
            },
        ));
        let in_npm_package_checker = DenoInNpmPackageChecker::new(
            CreateInNpmPkgCheckerOptions::Managed(ManagedInNpmPkgCheckerCreateOptions {
                root_cache_dir_url: npm_cache_dir.root_dir_url(),
                maybe_node_modules_path: None,
            }),
        );
        let pkg_json_resolver = new_rc(PackageJsonResolver::new(
            sys.clone(),
            Some(new_rc(PackageJsonThreadLocalCache)),
        ));
        let node_resolver = new_rc(SmudgyNodeResolver::new(
            in_npm_package_checker.clone(),
            DenoIsBuiltInNodeModuleChecker,
            npm_resolver.clone(),
            pkg_json_resolver.clone(),
            node_resolution_sys.clone(),
            NodeResolverOptions::default(),
        ));
        let cjs_tracker = new_rc(CjsTracker::new(
            in_npm_package_checker.clone(),
            pkg_json_resolver.clone(),
            IsCjsResolutionMode::ImplicitTypeCommonJs,
            Vec::new(),
        ));
        // Real CJS export analysis (deno_ast), so `import { Client } from
        // "npm:discord.js"` works: the ESM wrapper synthesized for a CJS module
        // re-exports the names this analyzer finds (and follows re-exports).
        // Mirrors deno_resolver's own deno_ast-feature wiring without pulling in
        // deno_graph; the analysis cache is a no-op (sources come from the local
        // npm cache, and parsing is per-load).
        let cjs_code_analyzer = DenoCjsCodeAnalyzer::new(
            new_rc(NullNodeAnalysisCache),
            cjs_tracker.clone(),
            new_rc(SmudgyModuleExportAnalyzer),
        );
        let cjs_module_export_analyzer = new_rc(CjsModuleExportAnalyzer::new(
            cjs_code_analyzer,
            in_npm_package_checker.clone(),
            node_resolver.clone(),
            npm_resolver.clone(),
            pkg_json_resolver.clone(),
            sys.clone(),
        ));
        let node_code_translator = new_rc(NodeCodeTranslator::new(
            cjs_module_export_analyzer,
            NodeCodeTranslatorMode::ModuleLoader,
        ));
        let npm_module_loader = SmudgyNpmModuleLoader::new(
            cjs_tracker.clone(),
            in_npm_package_checker.clone(),
            node_code_translator,
            sys.clone(),
        );
        let node_require_loader = Rc::new(SmudgyNodeRequireLoader {
            cjs_tracker,
            in_npm_package_checker: in_npm_package_checker.clone(),
        });
        let req_resolver = NpmReqResolver::new(NpmReqResolverOptions {
            in_npm_pkg_checker: in_npm_package_checker.clone(),
            node_resolver: node_resolver.clone(),
            npm_resolver: npm_resolver.clone(),
            sys: sys.clone(),
        });
        let npm_cache = Arc::new(NpmCache::new(
            npm_cache_dir,
            sys.clone(),
            cache_setting,
            npmrc.clone(),
        ));
        let http_client = Arc::new(ReqwestNpmCacheHttpClient);
        let registry_info_provider = Arc::new(RegistryInfoProvider::new(
            npm_cache.clone(),
            http_client.clone(),
            npmrc.clone(),
            NpmPackumentFormat::Full,
        ));
        let tarball_cache = Arc::new(TarballCache::new(
            npm_cache,
            http_client,
            sys.clone(),
            npmrc,
            None,
        ));

        let services = Rc::new(Self {
            editor: RefCell::new(None),
            in_npm_package_checker: in_npm_package_checker.clone(),
            npm_resolver: npm_resolver.clone(),
            node_resolver: node_resolver.clone(),
            req_resolver,
            registry_info_provider,
            tarball_cache,
            npm_module_loader,
            npm_resolution,
            npm_system_info,
            version_resolver: NpmVersionResolver::default(),
        });
        let node_services = NodeExtInitServices {
            node_require_loader,
            node_resolver,
            pkg_json_resolver,
            sys,
        };

        Ok((services, node_services))
    }

    /// Resolve + (lazily) install + load an `npm:` specifier into a deno
    /// `ModuleSource`. This is `async` and is driven by deno_core's event loop via
    /// `ModuleLoadResponse::Async`: the whole deno npm stack is tokio-async and
    /// `!Send`, so it must run on the session thread under the live runtime -- NOT
    /// under a nested `block_on`, which deadlocks the current-thread runtime.
    pub async fn load_npm_async(
        &self,
        specifier: &ModuleSpecifier,
        referrer: &ModuleSpecifier,
    ) -> Result<ModuleSource, ModuleLoaderError> {
        let req_ref = NpmPackageReqReference::from_specifier(specifier).map_err(|err| {
            generic_loader_error(format!("invalid npm specifier {specifier}: {err}"))
        })?;
        self.ensure_package(req_ref.req()).await?;
        let resolved = self
            .req_resolver
            .resolve_req_reference(
                &req_ref,
                referrer,
                ResolutionMode::Import,
                node_resolver::NodeResolutionKind::Execution,
            )
            .map_err(|err| generic_loader_error(format!("failed resolving {specifier}: {err}")))?;
        let file_url = resolved.into_url().map_err(|err| {
            generic_loader_error(format!(
                "npm resolved to a value that could not become a file URL: {err}"
            ))
        })?;
        let (module_type, source) = self.load_npm_module(&file_url, Some(referrer)).await?;
        if let Err(error) = self.queue_editor_types(&req_ref, specifier, referrer, &file_url) {
            log::warn!("Could not refresh npm editor types for {specifier}: {error:#}");
        }
        // Requested as `npm:...` but found at `file_url`, so its internal imports
        // resolve relative to the real installed file path.
        Ok(ModuleSource::new_with_redirect(
            module_type,
            ModuleSourceCode::String(source.into()),
            specifier,
            &file_url,
            None,
        ))
    }

    pub fn is_npm_package_specifier(&self, specifier: &ModuleSpecifier) -> bool {
        node_resolver::InNpmPackageChecker::in_npm_package(&self.in_npm_package_checker, specifier)
    }

    pub(crate) fn enable_editor_types(
        &self,
        directory: PathBuf,
        observer: crate::npm_editor::NpmTypeObserver,
    ) {
        *self.editor.borrow_mut() = Some(crate::npm_editor::NpmEditor {
            directory,
            observer,
        });
    }

    fn queue_editor_types(
        &self,
        request: &NpmPackageReqReference,
        specifier: &ModuleSpecifier,
        referrer: &ModuleSpecifier,
        execution_url: &ModuleSpecifier,
    ) -> Result<()> {
        let Some(editor) = self.editor.borrow().clone() else {
            return Ok(());
        };
        // Types-mode resolution honors conditional exports, renamed subpaths, and
        // version selectors. A package without declarations still gets JS inference.
        let target = self
            .req_resolver
            .resolve_req_reference(
                request,
                referrer,
                ResolutionMode::Import,
                node_resolver::NodeResolutionKind::Types,
            )
            .ok()
            .and_then(|resolved| resolved.into_url().ok())
            .unwrap_or_else(|| execution_url.clone())
            .to_file_path()
            .map_err(|()| anyhow::anyhow!("npm type target is not a file"))?;
        let snapshot = self
            .npm_resolution
            .snapshot()
            .subset(std::slice::from_ref(request.req()));
        let resolver = self
            .npm_resolver
            .as_managed()
            .context("managed npm resolver")?;
        let packages = snapshot
            .all_system_packages(&self.npm_system_info)
            .into_iter()
            .map(|package| {
                Ok(crate::npm_editor::EditorPackage {
                    id: package.id.as_serialized().to_string(),
                    directory: resolver.resolve_pkg_folder_from_pkg_id(&package.id)?,
                    dependencies: package
                        .dependencies
                        .iter()
                        .map(|(name, id)| (name.to_string(), id.as_serialized().to_string()))
                        .collect(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        editor.enqueue(packages, target, specifier.as_str().to_string());
        Ok(())
    }

    /// Resolve an ESM import using Node semantics when its referrer is inside the managed npm
    /// cache. This is the same boundary used by Deno's `RawDenoResolver`: once execution enters
    /// an npm package, bare dependencies, package `imports`/`exports`, extension handling, and
    /// legacy bare built-ins such as `"module"` all belong to the Node resolver.
    pub fn resolve_package_import(
        &self,
        specifier: &str,
        referrer: &ModuleSpecifier,
    ) -> Result<Option<ModuleSpecifier>, ModuleLoaderError> {
        if referrer.scheme() != "file" || !self.is_npm_package_specifier(referrer) {
            return Ok(None);
        }

        let resolution = self
            .node_resolver
            .resolve(
                specifier,
                referrer,
                ResolutionMode::Import,
                node_resolver::NodeResolutionKind::Execution,
            )
            .map_err(|err| {
                generic_loader_error(format!(
                    "failed resolving npm package import {specifier} from {referrer}: {err}"
                ))
            })?;
        resolution.into_url().map(Some).map_err(|err| {
            generic_loader_error(format!(
                "npm package import {specifier} from {referrer} did not resolve to a URL: {err}"
            ))
        })
    }

    async fn load_npm_module(
        &self,
        specifier: &ModuleSpecifier,
        maybe_referrer: Option<&ModuleSpecifier>,
    ) -> Result<(deno_core::ModuleType, String), ModuleLoaderError> {
        let loaded = self
            .npm_module_loader
            .load(
                Cow::Borrowed(specifier),
                maybe_referrer,
                &RequestedModuleType::None,
                // Recursive CJS analysis may read only npm-root sources through the loader's
                // policy-aware provider. No fallback means an out-of-root edge degrades to
                // missing named exports instead of bypassing Smudgy's module policy.
                None,
            )
            .await
            .map_err(|err| {
                generic_loader_error(format!("failed loading npm module {specifier}: {err}"))
            })?;
        let module_type = if loaded.media_type == deno_ast::MediaType::Json {
            deno_core::ModuleType::Json
        } else {
            deno_core::ModuleType::JavaScript
        };
        let source = match loaded.source {
            LoadedModuleSource::String(text) => text.into_owned(),
            LoadedModuleSource::Bytes(bytes) => {
                String::from_utf8_lossy(bytes.as_ref()).into_owned()
            }
            LoadedModuleSource::ArcStr(text) => text.to_string(),
            LoadedModuleSource::ArcBytes(bytes) => {
                String::from_utf8_lossy(bytes.as_ref()).into_owned()
            }
        };
        Ok((module_type, source))
    }

    async fn ensure_package(&self, req: &PackageReq) -> Result<(), ModuleLoaderError> {
        {
            let snapshot = self.npm_resolution.snapshot();
            let result = snapshot
                .add_pkg_reqs(
                    self.registry_info_provider.as_ref(),
                    AddPkgReqsOptions {
                        package_reqs: std::slice::from_ref(req),
                        version_resolver: &self.version_resolver,
                        should_dedup: false,
                    },
                    None,
                )
                .await;
            if let Some(err) = result.results.into_iter().find_map(Result::err) {
                return Err(generic_loader_error(format!(
                    "failed resolving npm package {req}: {err}"
                )));
            }
            let snapshot = result.dep_graph_result.map_err(|err| {
                generic_loader_error(format!(
                    "failed resolving npm dependencies for {req}: {err}"
                ))
            })?;
            let packages = snapshot.all_system_packages(&self.npm_system_info);
            self.npm_resolution.set_snapshot(snapshot);
            for package in packages {
                let Some(dist) = &package.dist else {
                    continue;
                };
                self.tarball_cache
                    .ensure_package(&package.id.nv, dist)
                    .await
                    .map_err(|err| {
                        generic_loader_error(format!(
                            "failed caching npm package {}: {err}",
                            package.id.nv
                        ))
                    })?;
            }
            Ok(())
        }
    }
}

#[derive(Debug)]
struct ReqwestNpmCacheHttpClient;

#[async_trait::async_trait(?Send)]
impl NpmCacheHttpClient for ReqwestNpmCacheHttpClient {
    async fn download_with_retries_on_any_tokio_runtime(
        &self,
        url: deno_core::url::Url,
        maybe_auth: Option<String>,
        maybe_etag: Option<String>,
        _maybe_registry_config: Option<&deno_npmrc::RegistryConfig>,
    ) -> Result<NpmCacheHttpClientResponse, DownloadError> {
        // This download path is driven by `futures::executor::block_on` (a bare
        // executor that does NOT tick the tokio runtime), so `tokio::spawn_blocking
        // ().await` would hang -- its JoinHandle completion waker is never delivered.
        // Run blocking reqwest on a dedicated OS thread and await a futures oneshot,
        // which the bare executor CAN poll (same isolation as the jsr loader).
        let (tx, rx) = deno_core::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let result: Result<NpmCacheHttpClientResponse, DownloadError> = (move || {
                let client = reqwest::blocking::Client::new();
                let mut request = client.get(url.to_string());
                if let Some(auth) = maybe_auth {
                    request = request.header(reqwest::header::AUTHORIZATION, auth);
                }
                if let Some(etag) = maybe_etag {
                    request = request.header(IF_NONE_MATCH, etag);
                }
                let response = request.send().map_err(|err| DownloadError {
                    status_code: err.status().map(|status| status.as_u16()),
                    error: JsErrorBox::generic(err.to_string()),
                })?;
                if response.status() == reqwest::StatusCode::NOT_MODIFIED {
                    return Ok(NpmCacheHttpClientResponse::NotModified);
                }
                if response.status() == reqwest::StatusCode::NOT_FOUND {
                    return Ok(NpmCacheHttpClientResponse::NotFound);
                }
                if !response.status().is_success() {
                    return Err(DownloadError {
                        status_code: Some(response.status().as_u16()),
                        error: JsErrorBox::generic(format!(
                            "GET failed with {}",
                            response.status()
                        )),
                    });
                }
                let etag = response
                    .headers()
                    .get(ETAG)
                    .and_then(|value| value.to_str().ok())
                    .map(ToOwned::to_owned);
                let bytes = response.bytes().map_err(|err| DownloadError {
                    status_code: err.status().map(|status| status.as_u16()),
                    error: JsErrorBox::generic(err.to_string()),
                })?;
                Ok(NpmCacheHttpClientResponse::Bytes(
                    NpmCacheHttpClientBytesResponse {
                        bytes: bytes.to_vec(),
                        etag,
                    },
                ))
            })();
            let _ = tx.send(result);
        });
        rx.await.map_err(|err| DownloadError {
            status_code: None,
            error: JsErrorBox::generic(err.to_string()),
        })?
    }
}

/// Parses a module with `deno_ast` for [`DenoCjsCodeAnalyzer`]: the parse
/// decides script-vs-module (so genuinely-ESM files inside npm packages are
/// left alone) and names a CJS module's exports/re-exports, cjs-module-lexer
/// style. Equivalent to deno_resolver's `DenoAstModuleExportAnalyzer` minus
/// its deno_graph parsed-source cache.
#[derive(Debug)]
struct SmudgyModuleExportAnalyzer;

impl ModuleExportAnalyzer for SmudgyModuleExportAnalyzer {
    fn parse_module(
        &self,
        specifier: deno_core::url::Url,
        media_type: deno_ast::MediaType,
        source: std::sync::Arc<str>,
    ) -> Result<Box<dyn ModuleForExportAnalysis>, JsErrorBox> {
        let parsed = deno_ast::parse_program(deno_ast::ParseParams {
            specifier,
            text: source,
            media_type,
            capture_tokens: true,
            scope_analysis: false,
            maybe_syntax: None,
        })
        .map_err(JsErrorBox::from_err)?;
        Ok(Box::new(AnalyzedModule(parsed)))
    }
}

struct AnalyzedModule(deno_ast::ParsedSource);

impl ModuleForExportAnalysis for AnalyzedModule {
    fn specifier(&self) -> &deno_core::url::Url {
        self.0.specifier()
    }

    fn compute_is_script(&self) -> bool {
        self.0.compute_is_script()
    }

    fn analyze_cjs(&self) -> ModuleExportsAndReExports {
        let analysis = self.0.analyze_cjs();
        let exports = analysis.exports;
        let reexports = analysis.reexports;
        let mut member_reexports = Vec::new();

        // Ported from Deno's MIT-licensed DenoAstModuleExportAnalyzer (v2.9.5). deno_ast's
        // ordinary CJS analyzer recognizes a bare `require()` re-export but not the
        // `module.exports = require("./inner").MEMBER` wrapper used by graphql-tag and peers.
        if exports.is_empty() && reexports.is_empty() {
            if let Some((specifier, member)) = find_module_exports_require_member(&self.0) {
                member_reexports.push(MemberReExport { specifier, member });
            }
        }

        ModuleExportsAndReExports {
            exports,
            reexports,
            member_reexports,
        }
    }

    fn analyze_es_runtime_exports(&self) -> ModuleExportsAndReExports {
        let analysis = self.0.analyze_es_runtime_exports();
        ModuleExportsAndReExports {
            exports: analysis.exports,
            reexports: analysis.reexports,
            member_reexports: Vec::new(),
        }
    }

    fn analyze_member_export_props(&self) -> BTreeMap<String, Vec<String>> {
        use deno_ast::ProgramRef;
        use deno_ast::swc::ast::ModuleItem;

        // One top-level walk composes `exports.MEMBER = IDENT` aliases with static
        // `IDENT.X = ...` assignments, then advertises only X for the selected MEMBER.
        let mut exports_aliases = Vec::new();
        let mut ident_props: HashMap<String, Vec<String>> = HashMap::new();
        let mut walk = |stmt: &deno_ast::swc::ast::Stmt| {
            if let Some((member, ident)) = match_exports_member_to_ident(stmt) {
                exports_aliases.push((member, ident));
            } else if let Some((ident, prop)) = match_identifier_property(stmt) {
                ident_props.entry(ident).or_default().push(prop);
            }
        };
        match self.0.program_ref() {
            ProgramRef::Module(module) => {
                for item in &module.body {
                    if let ModuleItem::Stmt(stmt) = item {
                        walk(stmt);
                    }
                }
            }
            ProgramRef::Script(script) => {
                for stmt in &script.body {
                    walk(stmt);
                }
            }
        }

        let mut out = BTreeMap::new();
        for (member, ident) in exports_aliases {
            let Some(props) = ident_props.get(&ident) else {
                continue;
            };
            let mut props = props.clone();
            props.sort();
            props.dedup();
            out.insert(member, props);
        }
        out
    }
}

fn find_module_exports_require_member(parsed: &deno_ast::ParsedSource) -> Option<(String, String)> {
    use deno_ast::ProgramRef;
    use deno_ast::swc::ast::ModuleItem;

    match parsed.program_ref() {
        ProgramRef::Module(module) => module.body.iter().find_map(|item| match item {
            ModuleItem::Stmt(stmt) => match_module_exports_require_member(stmt),
            ModuleItem::ModuleDecl(_) => None,
        }),
        ProgramRef::Script(script) => script
            .body
            .iter()
            .find_map(match_module_exports_require_member),
    }
}

fn match_module_exports_require_member(
    stmt: &deno_ast::swc::ast::Stmt,
) -> Option<(String, String)> {
    use deno_ast::swc::ast::{AssignOp, AssignTarget, Expr, Lit, MemberProp, SimpleAssignTarget};

    let assign = match stmt {
        deno_ast::swc::ast::Stmt::Expr(expr) => expr.expr.as_assign()?,
        _ => return None,
    };
    if assign.op != AssignOp::Assign {
        return None;
    }
    let target_member = match &assign.left {
        AssignTarget::Simple(SimpleAssignTarget::Member(member)) => member,
        _ => return None,
    };
    if !is_module_exports_member(target_member) {
        return None;
    }
    let outer_member = match &*assign.right {
        Expr::Member(member) => member,
        _ => return None,
    };
    let member = match &outer_member.prop {
        MemberProp::Ident(ident) => ident.sym.to_string(),
        MemberProp::Computed(computed) => match &*computed.expr {
            Expr::Lit(Lit::Str(value)) => value.value.as_str()?.to_string(),
            _ => return None,
        },
        MemberProp::PrivateName(_) => return None,
    };
    let call = match &*outer_member.obj {
        Expr::Call(call) => call,
        _ => return None,
    };
    Some((call_expr_require_spec(call)?, member))
}

fn match_exports_member_to_ident(stmt: &deno_ast::swc::ast::Stmt) -> Option<(String, String)> {
    use deno_ast::swc::ast::{AssignOp, AssignTarget, SimpleAssignTarget};

    let assign = match stmt {
        deno_ast::swc::ast::Stmt::Expr(expr) => expr.expr.as_assign()?,
        _ => return None,
    };
    if assign.op != AssignOp::Assign {
        return None;
    }
    let target_member = match &assign.left {
        AssignTarget::Simple(SimpleAssignTarget::Member(member)) => member,
        _ => return None,
    };
    let member = exports_member_name(target_member)?;
    let ident = assign.right.as_ident()?;
    Some((member, ident.sym.to_string()))
}

fn match_identifier_property(stmt: &deno_ast::swc::ast::Stmt) -> Option<(String, String)> {
    use deno_ast::swc::ast::{AssignOp, AssignTarget, Expr, Lit, MemberProp, SimpleAssignTarget};

    let assign = match stmt {
        deno_ast::swc::ast::Stmt::Expr(expr) => expr.expr.as_assign()?,
        _ => return None,
    };
    if assign.op != AssignOp::Assign {
        return None;
    }
    let member = match &assign.left {
        AssignTarget::Simple(SimpleAssignTarget::Member(member)) => member,
        _ => return None,
    };
    let ident = member.obj.as_ident()?;
    let prop = match &member.prop {
        MemberProp::Ident(prop) => prop.sym.to_string(),
        MemberProp::Computed(computed) => match &*computed.expr {
            Expr::Lit(Lit::Str(value)) => value.value.as_str()?.to_string(),
            _ => return None,
        },
        MemberProp::PrivateName(_) => return None,
    };
    Some((ident.sym.to_string(), prop))
}

fn exports_member_name(member: &deno_ast::swc::ast::MemberExpr) -> Option<String> {
    use deno_ast::swc::ast::{Expr, Lit, MemberProp};

    let prop = match &member.prop {
        MemberProp::Ident(prop) => prop.sym.to_string(),
        MemberProp::Computed(computed) => match &*computed.expr {
            Expr::Lit(Lit::Str(value)) => value.value.as_str()?.to_string(),
            _ => return None,
        },
        MemberProp::PrivateName(_) => return None,
    };
    let object_is_exports = match &*member.obj {
        Expr::Ident(ident) => &*ident.sym == "exports",
        Expr::Member(inner) => is_module_exports_member(inner),
        _ => false,
    };
    object_is_exports.then_some(prop)
}

fn is_module_exports_member(member: &deno_ast::swc::ast::MemberExpr) -> bool {
    use deno_ast::swc::ast::{Expr, Lit, MemberProp};

    let Expr::Ident(object) = &*member.obj else {
        return false;
    };
    if &*object.sym != "module" {
        return false;
    }
    match &member.prop {
        MemberProp::Ident(prop) => &*prop.sym == "exports",
        MemberProp::Computed(computed) => matches!(
            &*computed.expr,
            Expr::Lit(Lit::Str(value)) if value.value.as_str() == Some("exports")
        ),
        MemberProp::PrivateName(_) => false,
    }
}

fn call_expr_require_spec(call: &deno_ast::swc::ast::CallExpr) -> Option<String> {
    use deno_ast::swc::ast::Lit;

    let ident = call.callee.as_expr()?.as_ident()?;
    if &*ident.sym != "require" {
        return None;
    }
    let arg = call.args.first()?;
    if arg.spread.is_some() {
        return None;
    }
    match arg.expr.as_lit()? {
        Lit::Str(value) => value.value.as_str().map(str::to_string),
        _ => None,
    }
}

struct SmudgyNodeRequireLoader {
    cjs_tracker: deno_resolver::cjs::CjsTrackerRc<DenoInNpmPackageChecker, RealSys>,
    in_npm_package_checker: DenoInNpmPackageChecker,
}

impl NodeRequireLoader for SmudgyNodeRequireLoader {
    fn ensure_read_permission<'a>(
        &self,
        permissions: &mut PermissionsContainer,
        path: Cow<'a, Path>,
    ) -> Result<Cow<'a, Path>, JsErrorBox> {
        let original_url = ModuleSpecifier::from_file_path(&path).ok();
        if original_url.as_ref().is_some_and(|specifier| {
            node_resolver::InNpmPackageChecker::in_npm_package(
                &self.in_npm_package_checker,
                specifier,
            )
        }) {
            let canonical = std::fs::canonicalize(&path).map_err(JsErrorBox::from_err)?;
            let canonical_url = ModuleSpecifier::from_file_path(&canonical).map_err(|()| {
                JsErrorBox::generic(format!(
                    "resolved npm require target is not a local file path: {}",
                    canonical.display()
                ))
            })?;
            if node_resolver::InNpmPackageChecker::in_npm_package(
                &self.in_npm_package_checker,
                &canonical_url,
            ) {
                return Ok(Cow::Owned(canonical));
            }
        }

        permissions
            .check_open(path, OpenAccessKind::Read, Some("require"))
            .map(deno_permissions::CheckedPath::into_path)
            .map_err(JsErrorBox::from_err)
    }

    fn load_text_file_lossy(&self, path: &Path) -> Result<FastString, JsErrorBox> {
        let bytes = std::fs::read(path).map_err(JsErrorBox::from_err)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned().into())
    }

    fn is_maybe_cjs(
        &self,
        specifier: &deno_core::url::Url,
    ) -> Result<bool, node_resolver::errors::PackageJsonLoadError> {
        self.cjs_tracker
            .is_maybe_cjs(specifier, deno_ast::MediaType::from_specifier(specifier))
    }

    fn is_maybe_cjs_from_require(
        &self,
        specifier: &deno_core::url::Url,
    ) -> Result<bool, node_resolver::errors::PackageJsonLoadError> {
        self.cjs_tracker
            .is_maybe_cjs_from_require(specifier, deno_ast::MediaType::from_specifier(specifier))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use deno_ast::{MediaType, ParseParams};
    use deno_npm::resolution::{
        SerializedNpmResolutionSnapshot, SerializedNpmResolutionSnapshotPackage,
    };
    use deno_npm::{NpmPackageId, NpmResolutionPackageSystemInfo};
    use deno_semver::package::PackageReq;
    use node_resolver::analyze::{CjsAnalysis, CjsCodeAnalyzer, EsmAnalysisMode};

    use super::*;

    fn analyzed(source: &str) -> AnalyzedModule {
        AnalyzedModule(
            deno_ast::parse_program(ParseParams {
                specifier: deno_core::url::Url::parse("file:///fixture.js").unwrap(),
                text: Arc::<str>::from(source),
                media_type: MediaType::JavaScript,
                capture_tokens: true,
                scope_analysis: false,
                maybe_syntax: None,
            })
            .unwrap(),
        )
    }

    fn snapshot_package(
        id: &NpmPackageId,
        dependencies: &[(&str, &NpmPackageId)],
    ) -> SerializedNpmResolutionSnapshotPackage {
        SerializedNpmResolutionSnapshotPackage {
            id: id.clone(),
            system: NpmResolutionPackageSystemInfo::default(),
            dist: None,
            dependencies: dependencies
                .iter()
                .map(|(name, id)| ((*name).into(), (*id).clone()))
                .collect(),
            optional_dependencies: HashSet::new(),
            optional_peer_dependencies: HashSet::new(),
            extra: None,
            is_deprecated: false,
            has_bin: false,
            has_scripts: false,
        }
    }

    #[test]
    fn npm_esm_bare_dependency_resolves_from_managed_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = std::fs::canonicalize(temp.path()).unwrap();
        let (services, _node_services) = SmudgyNpmServices::new(data_dir.clone()).unwrap();
        let app_id = NpmPackageId::from_serialized("fixture-app@1.0.0").unwrap();
        let dependency_id = NpmPackageId::from_serialized("fixture-dep@2.0.0").unwrap();
        let snapshot = SerializedNpmResolutionSnapshot {
            root_packages: HashMap::from([(
                PackageReq::from_str("fixture-app@1").unwrap(),
                app_id.clone(),
            )]),
            packages: vec![
                snapshot_package(&app_id, &[("fixture-dep", &dependency_id)]),
                snapshot_package(&dependency_id, &[]),
            ],
        };
        services
            .npm_resolution
            .set_snapshot(NpmResolutionSnapshot::new(snapshot.into_valid().unwrap()));

        let registry_dir = data_dir.join("npm").join("registry.npmjs.org");
        let app_dir = registry_dir.join("fixture-app").join("1.0.0");
        let dependency_dir = registry_dir.join("fixture-dep").join("2.0.0");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::create_dir_all(&dependency_dir).unwrap();
        std::fs::write(
            app_dir.join("package.json"),
            r#"{"name":"fixture-app","version":"1.0.0","type":"module"}"#,
        )
        .unwrap();
        std::fs::write(app_dir.join("index.mjs"), "import 'fixture-dep';\n").unwrap();
        std::fs::write(
            dependency_dir.join("package.json"),
            r#"{"name":"fixture-dep","version":"2.0.0","type":"module","exports":"./index.mjs"}"#,
        )
        .unwrap();
        std::fs::write(
            dependency_dir.join("index.mjs"),
            "export const value = 42;\n",
        )
        .unwrap();

        let referrer = ModuleSpecifier::from_file_path(app_dir.join("index.mjs")).unwrap();
        let resolved = services
            .resolve_package_import("fixture-dep", &referrer)
            .expect("resolve bare dependency")
            .expect("npm referrer should use Node resolution");
        let expected = ModuleSpecifier::from_file_path(dependency_dir.join("index.mjs")).unwrap();
        assert_eq!(resolved, expected);
    }

    fn cached_editor_fixture(root: &Path) -> Rc<SmudgyNpmServices> {
        // Cache-only mode makes any accidental network dependency fail the test.
        let (services, _node_services) =
            SmudgyNpmServices::new_with_cache_setting(root.to_path_buf(), NpmCacheSetting::Only)
                .unwrap();
        let package = root.join("npm/registry.npmjs.org/@scope/library/1.0.0");
        std::fs::create_dir_all(package.join("dist")).unwrap();
        std::fs::write(
            package.parent().unwrap().join("registry.json"),
            r#"{
            "name":"@scope/library", "dist-tags":{"latest":"1.0.0"},
            "time":{"1.0.0":"2020-01-01T00:00:00.000Z"},
            "versions":{"1.0.0":{"name":"@scope/library","version":"1.0.0",
                "dist":{"tarball":"https://registry.invalid/library.tgz"}}}
        }"#,
        )
        .unwrap();
        std::fs::write(
            package.join("package.json"),
            r#"{
            "name":"@scope/library", "version":"1.0.0", "type":"module",
            "exports":{"./feature":{"types":"./dist/renamed.d.ts","default":"./dist/runtime.js"}}
        }"#,
        )
        .unwrap();
        std::fs::write(package.join("dist/runtime.js"), "export const value = 42;").unwrap();
        std::fs::write(
            package.join("dist/renamed.d.ts"),
            "export declare const value: 42;",
        )
        .unwrap();
        services
    }

    #[tokio::test]
    async fn loaded_npm_types_use_cached_exports_and_versioned_specifiers() {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let services = cached_editor_fixture(&root);
        let (published, mut received) = tokio::sync::mpsc::unbounded_channel();
        services.enable_editor_types(
            root.join("editor"),
            Arc::new(move |name, path| {
                published
                    .send((name.to_string(), path.to_path_buf()))
                    .unwrap();
            }),
        );
        let specifier = ModuleSpecifier::parse("npm:@scope/library@1/feature").unwrap();
        let referrer = ModuleSpecifier::from_file_path(root.join("user.ts")).unwrap();
        services
            .load_npm_async(&specifier, &referrer)
            .await
            .unwrap();
        let (name, path) =
            tokio::time::timeout(std::time::Duration::from_secs(10), received.recv())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(name, specifier.as_str());
        assert!(path.ends_with("dist/renamed.d.ts"));
        assert!(path.starts_with(root.join("editor")));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "export declare const value: 42;"
        );
        assert!(
            !root.join("node_modules").exists(),
            "no separate npm install"
        );
    }

    #[tokio::test]
    async fn editor_io_does_not_delay_npm_module_loading() {
        use std::sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        };
        use std::time::Duration;

        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let services = cached_editor_fixture(&root);
        let (started, mut received) = tokio::sync::mpsc::unbounded_channel();
        let (release, paused) = std::sync::mpsc::channel();
        let paused = Mutex::new(paused);
        let finished = Arc::new(AtomicBool::new(false));
        let observed = finished.clone();
        let (done, mut completed) = tokio::sync::mpsc::unbounded_channel();
        services.enable_editor_types(
            root.join("editor"),
            Arc::new(move |_, _| {
                started.send(()).unwrap();
                let _ = paused.lock().unwrap().recv_timeout(Duration::from_secs(15));
                observed.store(true, Ordering::SeqCst);
                done.send(()).unwrap();
            }),
        );
        let specifier = ModuleSpecifier::parse("npm:@scope/library@1/feature").unwrap();
        let referrer = ModuleSpecifier::from_file_path(root.join("user.ts")).unwrap();
        let loaded = tokio::time::timeout(
            Duration::from_secs(5),
            services.load_npm_async(&specifier, &referrer),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), received.recv())
            .await
            .unwrap()
            .unwrap();
        let was_waiting = !finished.load(Ordering::SeqCst);
        let _ = release.send(());
        tokio::time::timeout(Duration::from_secs(5), completed.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(loaded.unwrap().is_ok());
        assert!(
            was_waiting,
            "the module must finish loading while editor IO is still pending"
        );
    }

    fn write_esm_fixture(root: &std::path::Path, name: &str, source: &str) -> ModuleSpecifier {
        let dir = root.join("npm/registry.npmjs.org").join(name).join("1.0.0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            serde_json::json!({
                "name": name, "version": "1.0.0", "type": "module", "exports": "./index.mjs"
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(dir.join("index.mjs"), source).unwrap();
        ModuleSpecifier::from_file_path(dir.join("index.mjs")).unwrap()
    }

    #[test]
    fn smudgy_shorthand_does_not_intercept_transitive_scoped_npm_imports() {
        use crate::package_resolver::{
            InMemoryPackageProvider, PackageKey, PackageManifest, ResolvedPackage,
        };
        use deno_core::{ModuleLoader, ResolutionKind};

        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let (npm, _node_services) = SmudgyNpmServices::new(root.clone()).unwrap();
        let app_id = NpmPackageId::from_serialized("fixture-app@1.0.0").unwrap();
        let dep_id = NpmPackageId::from_serialized("@scope/dependency@1.0.0").unwrap();
        let transitive_id = NpmPackageId::from_serialized("@scope/transitive@1.0.0").unwrap();
        let snapshot = SerializedNpmResolutionSnapshot {
            root_packages: HashMap::from([(
                PackageReq::from_str("fixture-app@1").unwrap(),
                app_id.clone(),
            )]),
            packages: vec![
                snapshot_package(&app_id, &[("@scope/dependency", &dep_id)]),
                snapshot_package(&dep_id, &[("@scope/transitive", &transitive_id)]),
                snapshot_package(&transitive_id, &[]),
            ],
        };
        npm.npm_resolution
            .set_snapshot(NpmResolutionSnapshot::new(snapshot.into_valid().unwrap()));
        let app = write_esm_fixture(&root, "fixture-app", "import '@scope/dependency';");
        let dep = write_esm_fixture(&root, "@scope/dependency", "import '@scope/transitive';");
        let transitive = write_esm_fixture(&root, "@scope/transitive", "export const value = 42;");

        let mut packages = InMemoryPackageProvider::new();
        packages.insert(ResolvedPackage {
            key: PackageKey {
                owner: "local".into(),
                name: "scope".into(),
            },
            resolved_version: "1.0.0".into(),
            manifest: PackageManifest::parse(r#"{"version":"1.0.0"}"#).unwrap(),
            integrity: "local".into(),
            modules: vec![],
        });
        let loader = crate::module_loader::ScriptModuleLoader::with_npm_and_packages(
            root.clone(),
            crate::ModulePolicy::default(),
            npm,
            Some(Rc::new(packages)),
            deno_permissions::PermissionsContainer::allow_all(crate::permission_descriptor_parser()),
        );
        for (address, referrer, expected) in [
            ("@scope/dependency", &app, &dep),
            ("@scope/transitive", &dep, &transitive),
        ] {
            assert_eq!(
                loader
                    .resolve(address, referrer.as_str(), ResolutionKind::Import)
                    .unwrap(),
                *expected
            );
        }
        // A Node miss must stay a miss even with a local Smudgy package named scope.
        assert!(
            loader
                .resolve("@scope/missing", dep.as_str(), ResolutionKind::Import)
                .is_err()
        );
        let user = ModuleSpecifier::from_file_path(root.join("user.ts")).unwrap();
        assert_eq!(
            loader
                .resolve("@scope/dependency", user.as_str(), ResolutionKind::Import)
                .unwrap(),
            loader
                .resolve(
                    "smudgy:@scope/dependency",
                    user.as_str(),
                    ResolutionKind::Import
                )
                .unwrap(),
        );
    }

    #[test]
    fn cjs_analyzer_preserves_ordinary_and_bare_recursive_exports() {
        let ordinary = analyzed("exports.Client = class Client {};").analyze_cjs();
        assert!(ordinary.exports.iter().any(|name| name == "Client"));
        assert!(ordinary.member_reexports.is_empty());

        let recursive = analyzed("module.exports = require('./inner');").analyze_cjs();
        assert_eq!(recursive.reexports, ["./inner"]);
        assert!(recursive.member_reexports.is_empty());
    }

    #[test]
    fn cjs_member_wrapper_is_detected_and_narrowed() {
        let wrapper = analyzed("module.exports = require('./inner').gql;").analyze_cjs();
        assert_eq!(
            wrapper.member_reexports,
            [MemberReExport {
                specifier: "./inner".to_string(),
                member: "gql".to_string(),
            }]
        );

        let inner = analyzed(
            r#"
            exports.gql = parser;
            parser.parse = function () {};
            parser["reset"] = function () {};
            parser.parse = function () {};
            exports.unrelated = other;
            other.secret = true;
            "#,
        );
        assert_eq!(
            inner.analyze_member_export_props().get("gql"),
            Some(&vec!["parse".to_string(), "reset".to_string()])
        );
        assert_eq!(
            inner.analyze_member_export_props().get("unrelated"),
            Some(&vec!["secret".to_string()])
        );
        assert!(
            inner
                .analyze_es_runtime_exports()
                .member_reexports
                .is_empty()
        );
    }

    #[test]
    fn cjs_member_wrapper_rejects_dynamic_or_multi_hop_members() {
        for source in [
            "module.exports = require('./inner')[name];",
            "module.exports = require('./inner').gql.extra;",
            "module.exports = get('./inner').gql;",
        ] {
            assert!(analyzed(source).analyze_cjs().member_reexports.is_empty());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn composed_member_wrapper_narrows_final_export_set() {
        // Full-stack narrowing lock, driven through the real loader path
        // (`load_npm_module` -> `NpmModuleLoader::load` -> upstream
        // `CjsModuleExportAnalyzer::analyze_all_exports`): the wrapper's FINAL
        // translated export set contains only the names statically attached to
        // the selected member, and never the inner module's unrelated members
        // or their properties. The adapter-level tests above prove detection
        // and the props map; this one fails if the resolve/merge of
        // member_reexports ever regresses to a wholesale re-export.
        let temp = tempfile::tempdir().unwrap();
        // `NpmCacheDir` canonicalizes its root. macOS exposes temporary paths through
        // `/var` while canonicalization resolves them through `/private/var`, so build
        // every fixture path from the same canonical data directory.
        let data_dir = std::fs::canonicalize(temp.path()).unwrap();
        let (services, _node_services) = SmudgyNpmServices::new(data_dir.clone()).unwrap();
        // Under the npm cache root, so both files classify as npm-package CJS
        // and recursive analysis may read them directly.
        let pkg_dir = data_dir.join("npm").join("wrapper-pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("package.json"),
            r#"{"name":"wrapper-pkg","version":"1.0.0"}"#,
        )
        .unwrap();
        std::fs::write(
            pkg_dir.join("index.js"),
            "module.exports = require('./inner.js').narrowedMember;\n",
        )
        .unwrap();
        std::fs::write(
            pkg_dir.join("inner.js"),
            concat!(
                "exports.narrowedMember = alpha;\n",
                "alpha.narrowedAlpha = function () {};\n",
                "alpha.narrowedBeta = function () {};\n",
                "exports.unrelatedMember = omega;\n",
                "omega.unrelatedSecret = function () {};\n",
            ),
        )
        .unwrap();

        let entry = deno_core::url::Url::from_file_path(pkg_dir.join("index.js")).unwrap();
        assert!(
            services.is_npm_package_specifier(&entry),
            "fixture entry must be recognized beneath the canonical npm cache root"
        );
        let (module_type, source) = services.load_npm_module(&entry, None).await.unwrap();
        assert!(matches!(module_type, deno_core::ModuleType::JavaScript));
        assert!(
            source.contains("narrowedAlpha") && source.contains("narrowedBeta"),
            "selected member's props must be advertised: {source}"
        );
        assert!(
            !source.contains("unrelatedMember") && !source.contains("unrelatedSecret"),
            "unrelated inner members and their props must not leak into the wrapper: {source}"
        );
    }

    #[test]
    fn node_require_loader_uses_require_specific_cjs_classification() {
        // The two classification paths diverge on exactly one shape: an
        // extensionless file inside a `"type": "module"` package. Require-side
        // classification (`CjsTracker::is_maybe_cjs_from_require` ->
        // `check_for_require`) honors the module type only for files WITH an
        // extension, so the extensionless file stays Require (true); import-side
        // classification (`is_maybe_cjs` -> `check_based_on_pkg_json`) has no
        // extension clause outside npm roots, so the same file is Import
        // (false). Asserting both directions on the same specifier fails if
        // either method ever delegates to the other.
        let temp = tempfile::tempdir().unwrap();
        let (_services, node_services) = SmudgyNpmServices::new(temp.path().join("data")).unwrap();
        let pkg_dir = temp.path().join("esm-pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("package.json"),
            r#"{"name":"esm-pkg","version":"1.0.0","type":"module"}"#,
        )
        .unwrap();
        let extensionless = pkg_dir.join("extensionless-entry");
        std::fs::write(&extensionless, "module.exports = 1;\n").unwrap();
        let with_extension = pkg_dir.join("entry.js");
        std::fs::write(&with_extension, "export default 1;\n").unwrap();

        let extensionless = deno_core::url::Url::from_file_path(&extensionless).unwrap();
        let with_extension = deno_core::url::Url::from_file_path(&with_extension).unwrap();
        let loader = &node_services.node_require_loader;

        assert!(
            loader.is_maybe_cjs_from_require(&extensionless).unwrap(),
            "require() of an extensionless file in a type-module package must compile as CJS"
        );
        assert!(
            !loader.is_maybe_cjs(&extensionless).unwrap(),
            "import-side classification of the same file must stay ESM"
        );
        // A .js sibling is ESM on both sides: the require-specific extension
        // clause applies only to extensionless files.
        assert!(!loader.is_maybe_cjs_from_require(&with_extension).unwrap());
        assert!(!loader.is_maybe_cjs(&with_extension).unwrap());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cjs_analysis_without_loader_source_does_not_read_outside_npm_root() {
        let temp = tempfile::tempdir().unwrap();
        let npm_root = temp.path().join("npm");
        std::fs::create_dir_all(&npm_root).unwrap();

        let sys = RealSys;
        let npmrc = Arc::new(
            NpmRc::default()
                .as_resolved(&NpmRegistryUrl::for_npm(&sys))
                .unwrap(),
        );
        let npm_cache_dir = new_rc(NpmCacheDir::new(
            &sys,
            npm_root,
            npmrc.get_all_known_registries_urls(),
        ));
        let in_npm_package_checker = DenoInNpmPackageChecker::new(
            CreateInNpmPkgCheckerOptions::Managed(ManagedInNpmPkgCheckerCreateOptions {
                root_cache_dir_url: npm_cache_dir.root_dir_url(),
                maybe_node_modules_path: None,
            }),
        );
        let pkg_json_resolver = new_rc(PackageJsonResolver::new(
            sys,
            Some(new_rc(PackageJsonThreadLocalCache)),
        ));
        let cjs_tracker = new_rc(CjsTracker::new(
            in_npm_package_checker,
            pkg_json_resolver,
            IsCjsResolutionMode::ImplicitTypeCommonJs,
            Vec::new(),
        ));
        let analyzer = DenoCjsCodeAnalyzer::new(
            new_rc(NullNodeAnalysisCache),
            cjs_tracker,
            new_rc(SmudgyModuleExportAnalyzer),
        );

        // The file deliberately exists and advertises a named export. A recursive edge
        // outside the npm root reaches the analyzer with no loader-owned source; the result
        // must therefore be empty instead of reading this file behind the loader's back.
        let outside_path = temp.path().join("outside.js");
        std::fs::write(&outside_path, "exports.leaked = true;").unwrap();
        let outside = deno_core::url::Url::from_file_path(&outside_path).unwrap();
        let analysis = analyzer
            .analyze_cjs(&outside, None, EsmAnalysisMode::SourceImportsAndExports)
            .await
            .unwrap();

        match analysis {
            CjsAnalysis::Cjs(exports) => {
                assert!(exports.exports.is_empty());
                assert!(exports.reexports.is_empty());
                assert!(exports.member_reexports.is_empty());
            }
            CjsAnalysis::Esm(_, _) => panic!("missing loader source must degrade to empty CJS"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn loader_path_none_fallback_ignores_out_of_root_reexports() {
        // Locks the `None` fallback source provider at `load_npm_module`'s call
        // into `NpmModuleLoader::load`. With no fallback, a CJS re-export edge
        // that escapes the npm root degrades softly: the load still succeeds,
        // but the out-of-root file contributes no named exports because the
        // loader-side provider refuses to read it and there is nothing to fall
        // back to. Replacing that `None` with a disk-reading provider makes the
        // outside file's export appear and this test fail. The analyzer-level
        // test above covers the same policy one layer down; this one drives the
        // real `SmudgyNpmServices` call site end to end.
        let temp = tempfile::tempdir().unwrap();
        // Keep the fixture URL and `NpmCacheDir`'s canonical root in the same namespace
        // (`/private/var`, rather than the `/var` alias, on macOS).
        let data_dir = std::fs::canonicalize(temp.path()).unwrap();
        let (services, _node_services) = SmudgyNpmServices::new(data_dir.clone()).unwrap();
        let pkg_dir = data_dir.join("npm").join("escapist-pkg");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("package.json"),
            r#"{"name":"escapist-pkg","version":"1.0.0"}"#,
        )
        .unwrap();
        // Two levels up from the package escapes the npm cache root into the
        // data dir, where a syntactically valid CJS file really exists.
        std::fs::write(
            pkg_dir.join("index.js"),
            "module.exports = require('../../outside.js');\n",
        )
        .unwrap();
        std::fs::write(
            data_dir.join("outside.js"),
            "exports.leakedOutside = function () {};\n",
        )
        .unwrap();

        let entry = deno_core::url::Url::from_file_path(pkg_dir.join("index.js")).unwrap();
        assert!(
            services.is_npm_package_specifier(&entry),
            "fixture entry must be recognized beneath the canonical npm cache root"
        );
        let (module_type, source) = services
            .load_npm_module(&entry, None)
            .await
            .expect("an out-of-root re-export edge must degrade softly, not fail the load");
        assert!(matches!(module_type, deno_core::ModuleType::JavaScript));
        assert!(
            source.contains("export default"),
            "the CJS entry must still be translated to an ESM wrapper: {source}"
        );
        assert!(
            !source.contains("leakedOutside"),
            "an out-of-root file must contribute no named exports: {source}"
        );
    }
}
