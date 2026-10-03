use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use smudgy_cloud::{DependencyKind, ResolvedDependency};
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::package_cache::{CachedModule, CachedResolution, PackageCache};
use smudgy_script::{PackageDependency, PackageKey, PackageManifest};

use super::{Config, below};

const OWNER: &str = "kapusniak";
const GMCP_SOURCE: &str = r#"
import gmcp from "smudgy:state/gmcp";
export const nukefire = gmcp;
export function watchMessage(name, handler) { return gmcp.watch(name, handler); }
export function onMessage(name, handler) {
  return gmcp.onWrite(name, (path, value) => {
    if (path.toLowerCase() === name.toLowerCase() && value !== undefined) handler(value);
  });
}
"#;

const PLANNER_PROBE: &str = r#"
globalThis.__packingRealm = __PACKING_REALM__;
let __packingLastPhase = "";
layoutPlannerState.subscribe((state) => {
  const key = [state.startedAt, state.status, state.phase].join("/");
  if (key === __packingLastPhase) return;
  __packingLastPhase = key;
  globalThis.__packingMeasure("planner", JSON.stringify({
    realm: __PACKING_REALM__, startedAt: state.startedAt, status: state.status,
    phase: state.phase, operation: state.operation, terminalReason: state.terminalReason,
    nodes: state.nodes, residents: state.residents, edges: state.edges,
    source: state.context?.source,
  }));
});
layoutWorkerDiagnostics.subscribe((data) => {
  globalThis.__packingMeasure("worker-terminal", JSON.stringify({ realm: __PACKING_REALM__, ...data }));
});
"#;

const TIDY_PROBE: &str = r#"
const __packingTidy = nukefireMapper.tidyAllMaps.bind(nukefireMapper);
nukefireMapper.tidyAllMaps = (...args) => {
  globalThis.__packingMeasure("tidy-start", "{}");
  return __packingTidy(...args).finally(() => globalThis.__packingMeasure("tidy-settled", "{}"));
};
const __packingStopTidy = nukefireMapper.stopTidy.bind(nukefireMapper);
nukefireMapper.stopTidy = () => {
  const accepted = __packingStopTidy();
  globalThis.__packingMeasure("tidy-stop-handled", JSON.stringify({ accepted }));
  return accepted;
};
"#;

fn probe_workers(files: &mut [(String, String)]) {
    let (_, source) = files
        .iter_mut()
        .find(|(name, _)| name == "worker-client.ts")
        .unwrap();
    for (seam, probe) in [
        (
            "this.#finish(request, false, reason);",
            "globalThis.__packingMeasure?.('worker-cancel-settled', JSON.stringify({realm:globalThis.__packingRealm,purpose:'persistent',id:request.id,operation:request.operation,reason:reason?.name}));",
        ),
        (
            "this.#finishRepair(repair, \"failure\", reason);",
            "globalThis.__packingMeasure?.('worker-cancel-settled', JSON.stringify({realm:globalThis.__packingRealm,purpose:'constraint-repair',id:repair.id,reason:reason?.name}));",
        ),
    ] {
        assert!(source.contains(seam), "known Worker cancellation seam");
        // First match is the cancellation path, after Worker retirement/settlement.
        *source = source.replacen(seam, &format!("{seam}\n    {probe}"), 1);
    }
    for purpose in ["persistent", "constraint-repair"] {
        let seam = format!("const worker = this.#factory(\"{purpose}\");");
        assert!(source.contains(&seam), "known Worker construction seam");
        let replacement = format!(
            r"
    const __packingCreateStart = performance.now();
    globalThis.__packingMeasure?.('worker-create-start', JSON.stringify({{purpose:'{purpose}'}}));
    {seam}
    globalThis.__packingMeasure?.('worker-create-end', JSON.stringify({{purpose:'{purpose}',elapsedMs:performance.now()-__packingCreateStart}}));
"
        );
        *source = source.replacen(&seam, &replacement, 1);
    }
}

fn read_package(path: &Path) -> (PackageManifest, Vec<(String, String)>) {
    let manifest =
        PackageManifest::parse(&std::fs::read_to_string(path.join("smudgy.package.json")).unwrap())
            .unwrap();
    let mut files = std::fs::read_dir(path)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|ext| ext == "ts" || ext == "tsx")
                && !entry.file_name().to_string_lossy().contains(".test.")
        })
        .map(|entry| {
            (
                entry.file_name().to_str().unwrap().to_string(),
                std::fs::read_to_string(entry.path()).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.0.cmp(&right.0));
    (manifest, files)
}

fn package_path(root: &Path, configured: Option<&str>, name: &str) -> std::path::PathBuf {
    configured.map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../packages")
                .join(name)
        },
        |relative| below(root, relative),
    )
}

fn dependencies(manifest: &PackageManifest, versions: &[(&str, &str)]) -> Vec<ResolvedDependency> {
    let mut result = Vec::new();
    for (kind, declarations) in [
        (DependencyKind::Dependency, &manifest.dependencies),
        (DependencyKind::Requires, &manifest.requires),
    ] {
        for raw in declarations {
            let declaration = PackageDependency::parse(raw)
                .expect("package dependency")
                .unwrap();
            let version = versions
                .iter()
                .find(|(name, _)| *name == declaration.key.name)
                .unwrap()
                .1;
            result.push(ResolvedDependency {
                owner_nickname: declaration.key.owner,
                name: declaration.key.name,
                range: declaration.range.unwrap_or_else(|| "*".into()),
                resolved_version: version.into(),
                kind,
            });
        }
    }
    result
}

fn install(
    server: &str,
    name: &str,
    manifest: &PackageManifest,
    files: Vec<(String, String)>,
    dependencies: Vec<ResolvedDependency>,
    source_fingerprint: &Value,
) -> Value {
    let instrumented = fingerprint(manifest, &files);
    let cache = PackageCache::new().unwrap();
    let mut modules = Vec::new();
    for (subpath, source) in files {
        let hash = format!("{:x}", Sha256::digest(source.as_bytes()));
        cache.write_blob(&hash, &source).unwrap();
        modules.push(CachedModule {
            is_entry: subpath == "index.ts",
            subpath,
            content_hash: hash,
            media_type: "application/typescript".into(),
            byte_size: i64::try_from(source.len()).unwrap(),
        });
    }
    let mut hashes = modules
        .iter()
        .map(|module| format!("{}={}", module.subpath, module.content_hash))
        .collect::<Vec<_>>();
    hashes.sort();
    let integrity = hashes.join(";");
    cache
        .write_meta(
            &PackageKey {
                owner: OWNER.into(),
                name: name.into(),
            },
            &manifest.version,
            &CachedResolution {
                version: manifest.version.clone(),
                integrity: integrity.clone(),
                manifest: manifest.clone(),
                modules,
                dependencies,
            },
        )
        .unwrap();
    let specifier = format!("smudgy://{OWNER}/{name}");
    shared_packages::install_package(server, &specifier, UpdateMode::Auto, true).unwrap();
    shared_packages::record_resolution(server, &specifier, &manifest.version, &integrity).unwrap();
    let lock = shared_packages::load_lock(server).unwrap();
    let row = lock.find(&specifier).unwrap();
    assert!(!row.trusted);
    assert!(
        shared_packages::record_consent_if_unchanged(server, row, &manifest.permissions).unwrap()
    );
    json!({"name":name,"source":source_fingerprint,"instrumented":instrumented,"trusted":false})
}

fn fingerprint(manifest: &PackageManifest, files: &[(String, String)]) -> Value {
    let modules = files.iter().map(|(name, content)| json!({"subpath":name,"sha256":format!("{:x}",Sha256::digest(content.as_bytes()))})).collect::<Vec<_>>();
    json!({"version":manifest.version,"manifestSha256":format!("{:x}",Sha256::digest(serde_json::to_vec(manifest).unwrap())),"modules":modules})
}

pub fn prepare(root: &Path, config: &Config, server: &str, quiet: bool, discovery: u64) -> Value {
    let gmcp = PackageManifest::parse(
        r#"{"version":"1.0.0","entry":"index.ts","permissions":{"smudgy":{"interop":["read"]}}}"#,
    )
    .unwrap();
    let gmcp_files = vec![("index.ts".into(), GMCP_SOURCE.into())];
    let gmcp_original = fingerprint(&gmcp, &gmcp_files);
    let gmcp_record = install(
        server,
        "nukefire-gmcp",
        &gmcp,
        gmcp_files,
        vec![],
        &gmcp_original,
    );
    let (layout, mut layout_files) = read_package(&package_path(
        root,
        config.layout_package_path.as_deref(),
        "map-layout",
    ));
    let layout_original = fingerprint(&layout, &layout_files);
    probe_workers(&mut layout_files);
    let (mapper, mut mapper_files) = read_package(&package_path(
        root,
        config.mapper_package_path.as_deref(),
        "nukefire-mapper",
    ));
    let mapper_original = fingerprint(&mapper, &mapper_files);
    let (scripts, mut scripts_files) = read_package(&package_path(
        root,
        config.scripts_package_path.as_deref(),
        "nukefire-scripts",
    ));
    let scripts_original = fingerprint(&scripts, &scripts_files);
    let versions = [
        ("nukefire-gmcp", "1.0.0"),
        ("map-layout", layout.version.as_str()),
        ("nukefire-mapper", mapper.version.as_str()),
    ];
    instrument_mapper(&mut mapper_files, quiet, discovery);
    instrument_scripts(&mut scripts_files);
    let mapper_dependencies = dependencies(&mapper, &versions);
    let scripts_dependencies = dependencies(&scripts, &versions);
    let layout_record = install(
        server,
        "map-layout",
        &layout,
        layout_files,
        vec![],
        &layout_original,
    );
    let mapper_record = install(
        server,
        "nukefire-mapper",
        &mapper,
        mapper_files,
        mapper_dependencies,
        &mapper_original,
    );
    let scripts_record = install(
        server,
        "nukefire-scripts",
        &scripts,
        scripts_files,
        scripts_dependencies,
        &scripts_original,
    );
    json!([gmcp_record, layout_record, mapper_record, scripts_record])
}

fn instrument_mapper(files: &mut [(String, String)], quiet: bool, discovery: u64) {
    for (subpath, source) in files {
        if subpath == "mapper.ts" {
            let seam =
                "await this.#syncSnapshot(snapshot, allowExistingReflow, runGeneration, signal);";
            assert!(
                source.contains(seam),
                "known durable topology completion seam"
            );
            *source = source.replacen(seam, &format!("{seam}\n    globalThis.__packingMeasure?.('snapshot-synchronized', JSON.stringify({{center:snapshot.center,allowExistingReflow,containsDiscovery:snapshot.rooms.some(room=>room.vnum==={discovery})}}));"), 1);
        }
        if subpath == "index.ts" {
            if !quiet {
                assert!(
                    source.contains("names: areaNames(),"),
                    "known mapper options seam"
                );
                *source = source.replacen(
                    "names: areaNames(),",
                    "names: areaNames(),\n  updateCoordinates: false,",
                    1,
                );
            }
            source.push_str(&PLANNER_PROBE.replace("__PACKING_REALM__", "\"mapper\""));
            source.push_str(TIDY_PROBE);
        }
    }
}

fn instrument_scripts(files: &mut [(String, String)]) {
    for (subpath, source) in files {
        if subpath == "index.ts" {
            *source = format!(
                "import './commands.ts';\nimport {{ layoutPlannerState, layoutWorkerDiagnostics }} from 'smudgy://kapusniak/map-layout';\n{}",
                PLANNER_PROBE.replace("__PACKING_REALM__", "\"scripts\"")
            );
        } else if subpath == "welcome.tsx" {
            *source = "export function open() {}".into();
        }
    }
}

pub fn main_probe(discovery: u64, origin: u64, known: u64) -> String {
    format!(
        r#"
import {{ mapper, getSessions }} from "smudgy:core";
import {{ room }} from "smudgy:events/map";
import {{ manualLayoutApplied }} from "smudgy:events/kapusniak/nukefire-mapper";
await mapper.ready();
room.on((location) => {{
  const area = mapper.getAreaById(location.areaId);
  const current = area?.room(location.roomNumber);
  const fresh = mapper.findRoomByExternalId("{discovery}");
  const origin = mapper.findRoomByExternalId("{origin}");
  const known = mapper.findRoomByExternalId("{known}");
  const linked = origin?.exits.some((exit) => exit.to_area_id === area.id && exit.to_room_number === fresh?.room_number) ?? false;
  const initialComplete = origin?.exits.some((exit) => exit.to_area_id === area.id && exit.to_room_number === known?.room_number) ?? false;
  globalThis.__packingMeasure("marker-publication", JSON.stringify({{
    vnum: Number(current?.externalId), discoveryPresent: !!fresh, linked,
    chartComplete: !!fresh && linked, initialComplete,
  }}));
}});
manualLayoutApplied.from(getSessions()[0]).on((ack) => {{
  globalThis.__packingMeasure("manual-ack", JSON.stringify(ack));
}});
let nextBeat = performance.now() + 25;
setInterval(() => {{
  const now = performance.now();
  globalThis.__packingMeasure("heartbeat", JSON.stringify({{ delayMs: Math.max(0, now - nextBeat) }}));
  nextBeat = now + 25;
}}, 25);
globalThis.__packingMeasure("probe-ready", "{{}}");
"#
    )
}
