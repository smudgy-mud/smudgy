use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Context as AnyhowContext, Result};
use deno_cache_dir::npm::NpmCacheDir;
use deno_core::error::ModuleLoaderError;
use deno_core::{FastString, ModuleSource, ModuleSourceCode, ModuleSpecifier};
use deno_error::JsErrorBox;
use deno_maybe_sync::new_rc;
use deno_node::{NodeExtInitServices, NodeRequireLoader};
use deno_npm::resolution::{AddPkgReqsOptions, NpmResolutionSnapshot, NpmVersionResolver};
use deno_npm::NpmSystemInfo;
use deno_npm_cache::{
    DownloadError, NpmCache, NpmCacheHttpClient, NpmCacheHttpClientBytesResponse,
    NpmCacheHttpClientResponse, NpmCacheSetting, NpmPackumentFormat, RegistryInfoProvider,
    TarballCache,
};
use deno_npmrc::{NpmRc, NpmRegistryUrl};
use deno_permissions::PermissionsContainer;
use deno_resolver::cjs::analyzer::{
    DenoCjsCodeAnalyzer, ModuleExportAnalyzer, ModuleExportsAndReExports,
    ModuleForExportAnalysis, NullNodeAnalysisCache,
};
use deno_resolver::cjs::{CjsTracker, IsCjsResolutionMode};
use deno_resolver::loader::{LoadedModuleSource, NpmModuleLoader, RequestedModuleType};
use deno_resolver::npm::managed::{
    ManagedInNpmPkgCheckerCreateOptions, ManagedNpmResolverCreateOptions, NpmResolutionCell,
};
use deno_resolver::npm::{
    CreateInNpmPkgCheckerOptions, DenoInNpmPackageChecker, NpmReqResolver,
    NpmReqResolverOptions, NpmResolver, NpmResolverCreateOptions,
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
type SmudgyNpmModuleLoader = NpmModuleLoader<
    DenoCjsCodeAnalyzer<RealSys>,
    DenoInNpmPackageChecker,
    DenoIsBuiltInNodeModuleChecker,
    SmudgyNpmResolver,
    RealSys,
>;

pub struct SmudgyNpmServices {
    pub in_npm_package_checker: DenoInNpmPackageChecker,
    pub npm_resolver: SmudgyNpmResolver,
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
            },
        ));
        let in_npm_package_checker =
            DenoInNpmPackageChecker::new(CreateInNpmPkgCheckerOptions::Managed(
                ManagedInNpmPkgCheckerCreateOptions {
                    root_cache_dir_url: npm_cache_dir.root_dir_url(),
                    maybe_node_modules_path: None,
                },
            ));
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
            sys.clone(),
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
            node_code_translator,
            sys.clone(),
        );
        let node_require_loader = Rc::new(SmudgyNodeRequireLoader { cjs_tracker });
        let req_resolver = NpmReqResolver::new(NpmReqResolverOptions {
            in_npm_pkg_checker: in_npm_package_checker.clone(),
            node_resolver: node_resolver.clone(),
            npm_resolver: npm_resolver.clone(),
            sys: sys.clone(),
        });
        let npm_cache = Arc::new(NpmCache::new(
            npm_cache_dir,
            sys.clone(),
            NpmCacheSetting::Use,
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
            in_npm_package_checker: in_npm_package_checker.clone(),
            npm_resolver: npm_resolver.clone(),
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
        node_resolver::InNpmPackageChecker::in_npm_package(
            &self.in_npm_package_checker,
            specifier,
        )
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
            LoadedModuleSource::Bytes(bytes) => String::from_utf8_lossy(bytes.as_ref()).into_owned(),
            LoadedModuleSource::ArcStr(text) => text.to_string(),
            LoadedModuleSource::ArcBytes(bytes) => String::from_utf8_lossy(bytes.as_ref()).into_owned(),
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
                generic_loader_error(format!("failed resolving npm dependencies for {req}: {err}"))
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
                    error: JsErrorBox::generic(format!("GET failed with {}", response.status())),
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
        ModuleExportsAndReExports {
            exports: analysis.exports,
            reexports: analysis.reexports,
        }
    }

    fn analyze_es_runtime_exports(&self) -> ModuleExportsAndReExports {
        let analysis = self.0.analyze_es_runtime_exports();
        ModuleExportsAndReExports {
            exports: analysis.exports,
            reexports: analysis.reexports,
        }
    }
}

struct SmudgyNodeRequireLoader {
    cjs_tracker: deno_resolver::cjs::CjsTrackerRc<DenoInNpmPackageChecker, RealSys>,
}

impl NodeRequireLoader for SmudgyNodeRequireLoader {
    fn ensure_read_permission<'a>(
        &self,
        _permissions: &mut PermissionsContainer,
        path: Cow<'a, Path>,
    ) -> Result<Cow<'a, Path>, JsErrorBox> {
        Ok(path)
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
}
