//! Server-local ordinary package settings. Connections are short lived so an idle store never
//! holds a Windows file handle across server deletion. Callers serialize domain operations with
//! the package-state guard; `SQLite` transactions provide the on-disk commit boundary.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Values = HashMap<String, Value>;
pub type ScopeValues = HashMap<String, Values>;
const FILE: &str = "smudgy-state.sqlite3";
const MAX_VALUE_BYTES: usize = 4 * 1024 * 1024;

/// Portable ordinary settings. `None` explicitly means unset; omitted keys are untouched.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsSnapshot {
    pub format: u32,
    pub package: String,
    pub values: BTreeMap<String, Option<Value>>,
    #[serde(default)]
    pub parameters: Vec<smudgy_script::PackageParameter>,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id: i64,
    pub created: String,
    pub script: bool,
    pub snapshot: SettingsSnapshot,
}

#[derive(Clone)]
struct CachedScope {
    stamp: (SystemTime, u64),
    values: ScopeValues,
    bytes: usize,
}

type ScopeCache = HashMap<(PathBuf, String), CachedScope>;
fn cache() -> &'static Mutex<ScopeCache> {
    static CACHE: OnceLock<Mutex<ScopeCache>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

fn location(dir: &Path) -> Result<(PathBuf, String)> {
    if dir
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|n| n == "profiles")
    {
        Ok((
            dir.parent()
                .and_then(Path::parent)
                .context("missing server directory")?
                .to_path_buf(),
            dir.file_name()
                .context("missing profile name")?
                .to_str()
                .context("invalid profile name")?
                .to_string(),
        ))
    } else {
        Ok((dir.to_path_buf(), String::new()))
    }
}

fn stamp(server: &Path) -> Result<(SystemTime, u64)> {
    let metadata = fs::metadata(server.join(FILE))?;
    Ok((metadata.modified()?, metadata.len()))
}

fn invalidate(server: &Path) {
    cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|(path, _), _| path != server);
}

fn open(server: &Path) -> Result<Connection> {
    anyhow::ensure!(
        server.is_dir(),
        "Server directory is unavailable: {}",
        server.display()
    );
    let mut db = Connection::open(server.join(FILE)).context("open server settings database")?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    db.pragma_update(None, "synchronous", "FULL")?;
    let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    anyhow::ensure!(
        version <= 1,
        "Settings database was created by a newer Smudgy version"
    );
    if version == 0 {
        let tx = db.transaction()?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS settings (
            scope TEXT NOT NULL, package TEXT NOT NULL, value TEXT NOT NULL,
            revision INTEGER NOT NULL DEFAULT 1, active INTEGER NOT NULL DEFAULT 1,
            PRIMARY KEY(scope,package));
            CREATE TABLE IF NOT EXISTS history (
            id INTEGER PRIMARY KEY, scope TEXT NOT NULL, package TEXT NOT NULL,
            created TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%S UTC','now')),
            value TEXT NOT NULL, parameters TEXT NOT NULL DEFAULT '[]', version TEXT, source TEXT NOT NULL DEFAULT 'saved');
            CREATE TABLE IF NOT EXISTS package_schema(package TEXT PRIMARY KEY, parameters TEXT NOT NULL, version TEXT, revision INTEGER NOT NULL DEFAULT 1, secret_keys TEXT NOT NULL DEFAULT '[]');
            CREATE INDEX IF NOT EXISTS history_package ON history(scope,package,id);
            CREATE TABLE IF NOT EXISTS recovery (package TEXT PRIMARY KEY, profile_scope INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS restoration_notice (package TEXT PRIMARY KEY, skipped TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS deleted_profiles (scope TEXT PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS imported (scope TEXT PRIMARY KEY);
            PRAGMA user_version=1;")?;
        tx.commit()?;
    }
    Ok(db)
}

fn import_scope(db: &mut Connection, dir: &Path, scope: &str) -> Result<()> {
    if db
        .query_row("SELECT 1 FROM imported WHERE scope=?", [scope], |_| Ok(()))
        .optional()?
        .is_some()
    {
        return Ok(());
    }
    let path = dir.join("smudgy.params.json");
    let values: ScopeValues = match fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str(&contents)
            .with_context(|| format!("parse {} before migration", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => return Err(e.into()),
    };
    let tx = db.transaction()?;
    for (package, values) in values {
        let encoded = encode(&values)?;
        tx.execute(
            "INSERT OR IGNORE INTO settings(scope,package,value) VALUES (?,?,?)",
            params![scope, package, encoded],
        )?;
    }
    tx.execute("INSERT INTO imported(scope) VALUES (?)", [scope])?;
    tx.commit()?;
    // Originals remain untouched as migration backups. The marker makes subsequent JSON edits
    // irrelevant; a failed import never marks the scope migrated.
    Ok(())
}

fn encode(values: &Values) -> Result<String> {
    let ordered: BTreeMap<_, _> = values.iter().collect();
    let text = serde_json::to_string(&ordered)?;
    anyhow::ensure!(
        text.len() <= MAX_VALUE_BYTES,
        "Package settings exceed the 4 MiB limit"
    );
    Ok(text)
}

/// Reads ordinary values, using the committed in-memory snapshot while the file is unchanged.
/// # Errors
/// Returns storage or migration errors; unavailable storage is never treated as empty.
pub(crate) fn load(dir: &Path) -> Result<ScopeValues> {
    let (server, scope) = location(dir)?;
    if let Ok(current_stamp) = stamp(&server) {
        let cached = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = cached.get(&(server.clone(), scope.clone()))
            && entry.stamp == current_stamp
        {
            return Ok(entry.values.clone());
        }
    }
    let mut db = open(&server)?;
    import_scope(&mut db, dir, &scope)?;
    let mut statement =
        db.prepare("SELECT package,value FROM settings WHERE scope=? AND active=1")?;
    let rows = statement.query_map([&scope], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut values = HashMap::new();
    let mut bytes = 0;
    for row in rows {
        let (package, json) = row?;
        bytes += json.len();
        let value: Values = serde_json::from_str(&json)?;
        if !value.is_empty() {
            values.insert(package, value);
        }
    }
    let entry = CachedScope {
        stamp: stamp(&server)?,
        values: values.clone(),
        bytes,
    };
    let mut cached = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Bound idle memory even if many servers/profiles have been visited.
    if cached.len() >= 64
        || cached.values().map(|v| v.bytes).sum::<usize>() + bytes > 16 * 1024 * 1024
    {
        cached.clear();
    }
    if bytes <= 16 * 1024 * 1024 {
        cached.insert((server, scope), entry);
    }
    Ok(values)
}

/// Reads one value without cloning other packages or large unrelated table parameters.
/// # Errors
/// Returns storage errors.
pub(crate) fn get(dir: &Path, package: &str, key: &str) -> Result<Option<Value>> {
    let (server, scope) = location(dir)?;
    if let Ok(current_stamp) = stamp(&server) {
        let cached = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = cached.get(&(server, scope))
            && entry.stamp == current_stamp
        {
            return Ok(entry
                .values
                .get(package)
                .and_then(|values| values.get(key))
                .cloned());
        }
    }
    Ok(load(dir)?
        .get(package)
        .and_then(|values| values.get(key))
        .cloned())
}

fn record(tx: &rusqlite::Transaction<'_>, scope: &str, package: &str, value: &str) -> Result<()> {
    record_with_source(tx, scope, package, value, false)
}

fn record_with_source(
    tx: &rusqlite::Transaction<'_>,
    scope: &str,
    package: &str,
    value: &str,
    script: bool,
) -> Result<()> {
    if value == "{}" {
        return Ok(());
    } // Resets are internal state, not configurations.
    let latest: Option<String> = tx
        .query_row(
            "SELECT value FROM history WHERE scope=? AND package=? ORDER BY id DESC LIMIT 1",
            params![scope, package],
            |r| r.get(0),
        )
        .optional()?;
    let mut coalesced = false;
    if script && latest.as_deref() != Some(value) {
        let updated=tx.execute("UPDATE history SET value=?3 WHERE id=(SELECT id FROM history WHERE scope=?1 AND package=?2 ORDER BY id DESC LIMIT 1)
            AND source='script' AND created>=strftime('%Y-%m-%d %H:%M:%S UTC','now','-5 seconds')
            AND parameters=COALESCE((SELECT parameters FROM package_schema WHERE package=?2),'[]')
            AND version IS (SELECT version FROM package_schema WHERE package=?2)",params![scope,package,value])?;
        coalesced = updated > 0;
    }
    if latest.as_deref() != Some(value) && !coalesced {
        tx.execute(
            "INSERT INTO history(scope,package,value,source,parameters,version) VALUES (?1,?2,?3,?4,
            COALESCE((SELECT parameters FROM package_schema WHERE package=?2),'[]'),
            (SELECT version FROM package_schema WHERE package=?2))",
            params![scope, package, value, if script { "script" } else { "saved" }],
        )?;
    }
    prune_history(tx, scope, package)
}

fn prune_history(tx: &rusqlite::Transaction<'_>, scope: &str, package: &str) -> Result<()> {
    // Retain at most 20 distinct revisions and 4 MiB per package/scope. Current/reinstall
    // values live separately and cannot be evicted by this routine history retention.
    tx.execute("DELETE FROM history WHERE scope=?1 AND package=?2 AND id NOT IN
        (SELECT id FROM (SELECT id, ROW_NUMBER() OVER (ORDER BY id DESC) AS n,
        SUM(length(CAST(value AS BLOB))+length(CAST(parameters AS BLOB))) OVER (ORDER BY id DESC) AS bytes FROM history WHERE scope=?1 AND package=?2)
        WHERE n<=20 AND bytes<=4194304)", params![scope,package])?;
    Ok(())
}

/// Atomically replaces a scope, recording changed configurations and updating revisions.
/// # Errors
/// Returns storage, serialization, or migration errors.
pub(crate) fn save(dir: &Path, values: &ScopeValues) -> Result<()> {
    save_with_source(dir, values, false)
}

/// Saves one script mutation; successive script writes within five seconds share a revision in
/// browsable history. Current values and optimistic revisions still commit on every change.
/// # Errors
/// Returns storage errors.
pub(crate) fn save_with_source(dir: &Path, values: &ScopeValues, script: bool) -> Result<()> {
    let (server, scope) = location(dir)?;
    let mut db = open(&server)?;
    import_scope(&mut db, dir, &scope)?;
    anyhow::ensure!(
        db.query_row(
            "SELECT 1 FROM deleted_profiles WHERE scope=?",
            [&scope],
            |_| Ok(())
        )
        .optional()?
        .is_none(),
        "The destination profile was deleted"
    );
    let tx = db.transaction()?;
    let old: HashMap<String, String> = tx
        .prepare("SELECT package,value FROM settings WHERE scope=? AND active=1")?
        .query_map([&scope], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut packages: std::collections::BTreeSet<&String> = old.keys().collect();
    packages.extend(values.keys());
    for package in packages {
        let new = encode(values.get(package).unwrap_or(&Values::new()))?;
        if old.get(package) == Some(&new) {
            continue;
        }
        if let Some(previous) = old.get(package) {
            record(&tx, &scope, package, previous)?;
        }
        record_with_source(&tx, &scope, package, &new, script)?;
        tx.execute("INSERT INTO settings(scope,package,value) VALUES (?,?,?)
            ON CONFLICT(scope,package) DO UPDATE SET value=excluded.value,revision=revision+1,active=1",
            params![scope,package,new])?;
    }
    tx.commit()?;
    invalidate(&server);
    Ok(())
}

/// Current optimistic-concurrency token, including intentionally empty/reset settings.
/// # Errors
/// Returns database or migration errors.
pub(crate) fn revision(dir: &Path, package: &str) -> Result<i64> {
    let (server, scope) = location(dir)?;
    let mut db = open(&server)?;
    import_scope(&mut db, dir, &scope)?;
    Ok(db
        .query_row(
            "SELECT COALESCE((SELECT revision FROM settings WHERE scope=?1 AND package=?2),0) + COALESCE((SELECT revision FROM package_schema WHERE package=?2),0)",
            params![scope, package],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// Archives all scopes before a package row is removed. Repeated cleanup is harmless.
/// # Errors
/// Returns database errors.
pub(crate) fn archive(server: &Path, package: &str, profile_scope: Option<bool>) -> Result<()> {
    let mut db = open(server)?;
    let tx = db.transaction()?;
    let entries: Vec<(String, String)> = tx
        .prepare("SELECT scope,value FROM settings WHERE package=? AND active=1")?
        .query_map([package], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (scope, value) in entries {
        record(&tx, &scope, package, &value)?;
    }
    if let Some(profile_scope) = profile_scope {
        tx.execute("INSERT INTO recovery VALUES (?,?) ON CONFLICT(package) DO UPDATE SET profile_scope=excluded.profile_scope", params![package,profile_scope])?;
    }
    tx.execute(
        "UPDATE settings SET active=0,revision=revision+1 WHERE package=? AND active=1",
        [package],
    )?;
    tx.commit()?;
    invalidate(server);
    Ok(())
}

/// Preserves uninstall recovery before the external package lockfile changes.
/// # Errors
/// Returns database errors. The package must not be removed if checkpointing fails.
pub(crate) fn checkpoint(server: &Path, package: &str, profile_scope: bool) -> Result<()> {
    let mut db = open(server)?;
    let tx = db.transaction()?;
    let entries: Vec<(String, String)> = tx
        .prepare("SELECT scope,value FROM settings WHERE package=? AND active=1")?
        .query_map([package], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (scope, value) in entries {
        record(&tx, &scope, package, &value)?;
    }
    tx.execute("INSERT INTO recovery VALUES (?,?) ON CONFLICT(package) DO UPDATE SET profile_scope=excluded.profile_scope", params![package,profile_scope])?;
    tx.commit()?;
    Ok(())
}

/// Reactivates saved values, including empty reset states. The recovery record survives retries.
/// # Errors
/// Returns database errors.
pub(crate) fn restore(server: &Path, package: &str) -> Result<Option<bool>> {
    let mut db = open(server)?;
    let tx = db.transaction()?;
    let scope = tx
        .query_row(
            "SELECT profile_scope FROM recovery WHERE package=?",
            [package],
            |r| r.get(0),
        )
        .optional()?;
    let restored = tx.execute(
        "UPDATE settings SET active=1,revision=revision+1 WHERE package=? AND active=0",
        [package],
    )?;
    if restored > 0 {
        tx.execute(
            "INSERT OR IGNORE INTO restoration_notice VALUES (?, '[]')",
            [package],
        )?;
    }
    tx.commit()?;
    invalidate(server);
    Ok(scope)
}

/// Lists recent nonempty configurations for one package and scope.
/// # Errors
/// Returns storage or malformed-data errors.
pub(crate) fn history(dir: &Path, package: &str) -> Result<Vec<HistoryEntry>> {
    let (server, scope) = location(dir)?;
    let mut db = open(&server)?;
    import_scope(&mut db, dir, &scope)?;
    let mut statement = db.prepare("SELECT id,created,value,parameters,version,source FROM history WHERE scope=? AND package=? ORDER BY id DESC")?;
    let rows = statement.query_map(params![scope, package], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;
    rows.map(|row| {
        let (id, created, value, parameters, version, source) = row?;
        let parameters: Vec<smudgy_script::PackageParameter> = serde_json::from_str(&parameters)?;
        let values: BTreeMap<String, Value> = serde_json::from_str(&value)?;
        let mut values: BTreeMap<_, _> = values.into_iter().map(|(k, v)| (k, Some(v))).collect();
        for param in &parameters {
            values.entry(param.key.clone()).or_insert(None);
        }
        Ok(HistoryEntry {
            id,
            created,
            script: source == "script",
            snapshot: SettingsSnapshot {
                format: 1,
                package: package.to_string(),
                values,
                parameters,
                version,
            },
        })
    })
    .collect()
}

/// Deletes only browsable history, preserving current and reinstall state.
/// # Errors
/// Returns database errors.
pub(crate) fn clear_history(dir: &Path, package: &str) -> Result<()> {
    let (server, scope) = location(dir)?;
    open(&server)?.execute(
        "DELETE FROM history WHERE scope=? AND package=?",
        params![scope, package],
    )?;
    Ok(())
}

/// Removes current profile values when its directory is deleted. History remains recoverable.
/// # Errors
/// Returns database errors.
pub(crate) fn remove_profile(server: &Path, profile: &str) -> Result<()> {
    if !server.join(FILE).try_exists()? {
        return Ok(());
    }
    let mut db = open(server)?;
    let tx = db.transaction()?;
    tx.execute(
        "UPDATE settings SET value='{}',active=1,revision=revision+1 WHERE scope=?",
        [profile],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO deleted_profiles VALUES (?)",
        [profile],
    )?;
    tx.commit()?;
    invalidate(server);
    Ok(())
}

/// Reopens a deliberately recreated profile without inheriting its old current settings.
/// # Errors
/// Returns database errors.
pub(crate) fn create_profile(server: &Path, profile: &str) -> Result<()> {
    if server.join(FILE).try_exists()? {
        open(server)?.execute("DELETE FROM deleted_profiles WHERE scope=?", [profile])?;
    }
    Ok(())
}

/// Registers the active schema, preserving incompatible ordinary data in history and removing
/// secret keys from every ordinary record. This is also the compatibility gate after reinstall.
/// # Errors
/// Returns storage or serialization errors; no partial schema change is committed.
pub(crate) fn register_schema(
    server: &Path,
    package: &str,
    parameters: &[smudgy_script::PackageParameter],
    version: Option<&str>,
) -> Result<()> {
    let mut db = open(server)?;
    let ordinary: Vec<_> = parameters.iter().filter(|p| !p.secret).cloned().collect();
    let encoded = serde_json::to_string(&ordinary)?;
    let secret_keys = serde_json::to_string(
        &parameters
            .iter()
            .filter(|p| p.secret)
            .map(|p| &p.key)
            .collect::<Vec<_>>(),
    )?;
    let existing: Option<(String, Option<String>, String)> = db
        .query_row(
            "SELECT parameters,version,secret_keys FROM package_schema WHERE package=?",
            [package],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if existing.as_ref().is_some_and(|(p, v, secrets)| {
        p == &encoded && v.as_deref() == version && secrets == &secret_keys
    }) {
        return Ok(());
    }
    let tx = db.transaction()?;
    let entries: Vec<(String, String)> = tx
        .prepare("SELECT scope,value FROM settings WHERE package=?")?
        .query_map([package], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (scope, raw) in entries {
        let mut values: Values = serde_json::from_str(&raw)?;
        values.retain(|key, _| !parameters.iter().any(|p| p.key == *key && p.secret));
        record(&tx, &scope, package, &encode(&values)?)?;
        // Unknown/renamed and incompatible fields remain in history. They do not become active
        // values for a different manifest, nor get coerced into the new shape.
        let previous_keys: Vec<_> = values.keys().cloned().collect();
        values.retain(|key, value| {
            ordinary.iter().any(|p| {
                p.key == *key
                    && crate::models::shared_packages::validate_package_param_value(p, value)
                        .is_ok()
            })
        });
        let skipped: Vec<_> = previous_keys
            .into_iter()
            .filter(|key| !values.contains_key(key))
            .collect();
        if !skipped.is_empty() {
            let prior: Option<String> = tx
                .query_row(
                    "SELECT skipped FROM restoration_notice WHERE package=?",
                    [package],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(prior) = prior {
                let mut keys: std::collections::BTreeSet<String> = serde_json::from_str(&prior)?;
                keys.extend(skipped);
                tx.execute(
                    "UPDATE restoration_notice SET skipped=? WHERE package=?",
                    params![serde_json::to_string(&keys)?, package],
                )?;
            }
        }
        let current = encode(&values)?;
        if current != raw {
            tx.execute(
                "UPDATE settings SET value=?,revision=revision+1 WHERE scope=? AND package=?",
                params![current, scope, package],
            )?;
        }
    }
    let history: Vec<(i64, String, String)> = tx
        .prepare("SELECT id,value,parameters FROM history WHERE package=?")?
        .query_map([package], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, raw, schema) in history {
        let mut values: Values = serde_json::from_str(&raw)?;
        let mut schema: Vec<smudgy_script::PackageParameter> = serde_json::from_str(&schema)?;
        values.retain(|key, _| !parameters.iter().any(|p| p.key == *key && p.secret));
        schema.retain(|p| !parameters.iter().any(|new| new.key == p.key && new.secret));
        if values.is_empty() {
            tx.execute("DELETE FROM history WHERE id=?", [id])?;
        } else {
            tx.execute(
                "UPDATE history SET value=?,parameters=? WHERE id=?",
                params![encode(&values)?, serde_json::to_string(&schema)?, id],
            )?;
        }
    }
    tx.execute("INSERT INTO package_schema(package,parameters,version,secret_keys) VALUES (?,?,?,?) ON CONFLICT(package) DO UPDATE SET parameters=excluded.parameters,version=excluded.version,secret_keys=excluded.secret_keys,revision=revision+1",params![package,encoded,version,secret_keys])?;
    tx.commit()?;
    invalidate(server);
    Ok(())
}

/// Consumes the one-time reinstall notification after the manifest compatibility pass.
/// # Errors
/// Returns database errors.
pub(crate) fn take_restoration_notice(server: &Path, package: &str) -> Result<Option<Vec<String>>> {
    let mut db = open(server)?;
    let tx = db.transaction()?;
    let notice: Option<String> = tx
        .query_row(
            "SELECT skipped FROM restoration_notice WHERE package=?",
            [package],
            |r| r.get(0),
        )
        .optional()?;
    let notice = notice.map(|text| serde_json::from_str(&text)).transpose()?;
    tx.execute("DELETE FROM restoration_notice WHERE package=?", [package])?;
    tx.commit()?;
    Ok(notice)
}

/// Enumerates stored profiles, including scopes with no legacy JSON file.
/// # Errors
/// Returns database errors.
pub(crate) fn profiles(server: &Path) -> Result<Vec<String>> {
    if !server.join(FILE).try_exists()? {
        return Ok(Vec::new());
    }
    let db = open(server)?;
    Ok(db
        .prepare("SELECT DISTINCT scope FROM settings WHERE scope<>''")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Copies ordinary state and history during a local package rename, without retiring the source.
/// The existing rename transaction handles lockfile publication and secret movement separately.
/// # Errors
/// Returns database errors.
pub(crate) fn copy_identity(server: &Path, from: &str, to: &str) -> Result<()> {
    let mut db = open(server)?;
    let tx = db.transaction()?;
    tx.execute("DELETE FROM settings WHERE package=?", [to])?;
    tx.execute("INSERT INTO settings(scope,package,value,revision,active) SELECT scope,?2,value,revision+1,active FROM settings WHERE package=?1",params![from,to])?;
    tx.execute("INSERT INTO history(scope,package,created,value,parameters,version,source) SELECT scope,?2,created,value,parameters,version,source FROM history WHERE package=?1 ORDER BY id",params![from,to])?;
    let scopes: Vec<String> = tx
        .prepare("SELECT DISTINCT scope FROM history WHERE package=?")?
        .query_map([to], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for scope in scopes {
        prune_history(&tx, &scope, to)?;
    }
    tx.execute("DELETE FROM package_schema WHERE package=?", [to])?;
    tx.execute("INSERT INTO package_schema(package,parameters,version,revision,secret_keys) SELECT ?2,parameters,version,revision+1,secret_keys FROM package_schema WHERE package=?1",params![from,to])?;
    tx.execute("DELETE FROM recovery WHERE package=?", [to])?;
    tx.commit()?;
    invalidate(server);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PACKAGE: &str = "smudgy://author/example";

    fn values(value: Value) -> ScopeValues {
        HashMap::from([(PACKAGE.into(), HashMap::from([("color".into(), value)]))])
    }

    #[test]
    fn imports_once_and_keeps_original_as_backup() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("smudgy.params.json");
        let original = serde_json::to_string(&values(json!("blue"))).unwrap();
        fs::write(&file, &original).unwrap();
        assert_eq!(load(dir.path()).unwrap(), values(json!("blue")));
        save(dir.path(), &values(json!("green"))).unwrap();
        invalidate(dir.path());
        assert_eq!(load(dir.path()).unwrap(), values(json!("green")));
        assert_eq!(fs::read_to_string(file).unwrap(), original);
    }

    #[test]
    fn corrupt_legacy_scope_is_never_marked_imported() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("smudgy.params.json");
        fs::write(&file, "broken").unwrap();
        assert!(load(dir.path()).is_err());
        fs::write(&file, serde_json::to_string(&values(json!(false))).unwrap()).unwrap();
        assert_eq!(load(dir.path()).unwrap(), values(json!(false)));
    }

    #[test]
    fn uninstall_and_reopen_restore_every_scope_and_reset_is_not_history() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profiles").join("Main");
        save(dir.path(), &values(json!("blue"))).unwrap();
        save(&profile, &values(json!("green"))).unwrap();
        checkpoint(dir.path(), PACKAGE, true).unwrap();
        archive(dir.path(), PACKAGE, None).unwrap();
        invalidate(dir.path());
        assert!(load(dir.path()).unwrap().is_empty());
        assert!(load(&profile).unwrap().is_empty());
        assert_eq!(restore(dir.path(), PACKAGE).unwrap(), Some(true));
        assert_eq!(load(dir.path()).unwrap(), values(json!("blue")));
        assert_eq!(load(&profile).unwrap(), values(json!("green")));
        save(&profile, &HashMap::new()).unwrap();
        assert_eq!(history(&profile, PACKAGE).unwrap().len(), 1);
        archive(dir.path(), PACKAGE, None).unwrap();
        restore(dir.path(), PACKAGE).unwrap();
        assert!(load(&profile).unwrap().is_empty());
        assert_eq!(load(dir.path()).unwrap(), values(json!("blue")));
    }

    #[test]
    fn history_is_bounded_and_does_not_evict_reinstall_values() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..45 {
            save(dir.path(), &values(json!(n))).unwrap();
        }
        assert_eq!(history(dir.path(), PACKAGE).unwrap().len(), 20);
        let revision = revision(dir.path(), PACKAGE).unwrap();
        save(dir.path(), &values(json!(44))).unwrap();
        assert_eq!(revision, super::revision(dir.path(), PACKAGE).unwrap());
        checkpoint(dir.path(), PACKAGE, false).unwrap();
        archive(dir.path(), PACKAGE, None).unwrap();
        clear_history(dir.path(), PACKAGE).unwrap();
        restore(dir.path(), PACKAGE).unwrap();
        assert_eq!(load(dir.path()).unwrap(), values(json!(44)));
    }

    #[test]
    fn schema_changes_preserve_incompatible_values_and_exclude_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let schema = |params| {
            smudgy_script::PackageManifest::parse(
                &json!({"version":"1.0.0","params":params}).to_string(),
            )
            .unwrap()
            .params
        };
        let first = schema(json!([{"key":"color"},{"key":"optional"}]));
        register_schema(dir.path(), PACKAGE, &first, Some("1.0.0")).unwrap();
        save(dir.path(), &values(json!("blue"))).unwrap();
        let snapshot = &history(dir.path(), PACKAGE).unwrap()[0].snapshot;
        assert_eq!(snapshot.values.get("optional"), Some(&None));
        let second = schema(json!([{"key":"color","type":"bool"}]));
        register_schema(dir.path(), PACKAGE, &second, Some("2.0.0")).unwrap();
        assert!(load(dir.path()).unwrap().is_empty());
        assert_eq!(
            history(dir.path(), PACKAGE).unwrap()[0].snapshot.values["color"],
            Some(json!("blue"))
        );
        let secret = schema(json!([{"key":"color","secret":true}]));
        register_schema(dir.path(), PACKAGE, &secret, Some("3.0.0")).unwrap();
        assert!(history(dir.path(), PACKAGE).unwrap().is_empty());
    }

    #[test]
    fn oversized_write_leaves_current_and_history_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &values(json!("blue"))).unwrap();
        assert!(save(dir.path(), &values(json!("x".repeat(MAX_VALUE_BYTES)))).is_err());
        assert_eq!(load(dir.path()).unwrap(), values(json!("blue")));
        assert_eq!(history(dir.path(), PACKAGE).unwrap().len(), 1);
    }

    #[test]
    fn failed_current_write_rolls_back_the_history_too() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &values(json!("blue"))).unwrap();
        open(dir.path()).unwrap().execute_batch("CREATE TRIGGER reject_change BEFORE UPDATE ON settings BEGIN SELECT RAISE(FAIL,'injected write failure'); END;").unwrap();
        assert!(save(dir.path(), &values(json!("green"))).is_err());
        assert_eq!(load(dir.path()).unwrap(), values(json!("blue")));
        assert_eq!(history(dir.path(), PACKAGE).unwrap().len(), 1);
    }

    #[test]
    fn deleted_profile_rejects_late_writes_and_recreation_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profiles").join("Main");
        save(&profile, &values(json!("blue"))).unwrap();
        remove_profile(dir.path(), "Main").unwrap();
        assert!(save(&profile, &values(json!("late"))).is_err());
        create_profile(dir.path(), "Main").unwrap();
        assert!(load(&profile).unwrap().is_empty());
        assert_eq!(history(&profile, PACKAGE).unwrap().len(), 1);
    }

    #[test]
    fn rapid_script_writes_coalesce_without_replacing_manual_history() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &values(json!("manual"))).unwrap();
        save_with_source(dir.path(), &values(json!("script1")), true).unwrap();
        // Avoid wall-clock timing assumptions on slow test hosts.
        open(dir.path()).unwrap().execute("UPDATE history SET created=strftime('%Y-%m-%d %H:%M:%S UTC','now','+1 day') WHERE source='script'",[]).unwrap();
        save_with_source(dir.path(), &values(json!("script2")), true).unwrap();
        let history = history(dir.path(), PACKAGE).unwrap();
        assert_eq!(history.len(), 2);
        assert!(history[0].script);
        assert_eq!(history[0].snapshot.values["color"], Some(json!("script2")));
        assert_eq!(history[1].snapshot.values["color"], Some(json!("manual")));
    }

    #[test]
    fn same_version_schema_edits_invalidate_editors_and_scrub_new_secret_keys() {
        let dir = tempfile::tempdir().unwrap();
        register_schema(dir.path(), PACKAGE, &[], Some("1.0.0")).unwrap();
        // A formerly undeclared key can become a secret without changing ordinary declarations.
        save(dir.path(), &values(json!("now-sensitive"))).unwrap();
        let before = revision(dir.path(), PACKAGE).unwrap();
        let schema = smudgy_script::PackageManifest::parse(
            r#"{"version":"1.0.0","params":[{"key":"color","secret":true}]}"#,
        )
        .unwrap()
        .params;
        register_schema(dir.path(), PACKAGE, &schema, Some("1.0.0")).unwrap();
        assert!(revision(dir.path(), PACKAGE).unwrap() > before);
        assert!(get(dir.path(), PACKAGE, "color").unwrap().is_none());
        assert!(history(dir.path(), PACKAGE).unwrap().is_empty());
    }

    #[test]
    fn renamed_history_keeps_order_and_retention_limits() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..25 {
            save(dir.path(), &values(json!(n))).unwrap();
        }
        copy_identity(dir.path(), PACKAGE, "smudgy://local/renamed").unwrap();
        copy_identity(dir.path(), PACKAGE, "smudgy://local/renamed").unwrap();
        let entries = history(dir.path(), "smudgy://local/renamed").unwrap();
        assert_eq!(entries.len(), 20);
        assert_eq!(entries[0].snapshot.values["color"], Some(json!(24)));
        assert_eq!(entries[19].snapshot.values["color"], Some(json!(5)));
    }

    #[test]
    #[ignore = "manual latency sample; no machine-dependent performance assertions"]
    fn settings_latency_sample() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &values(json!("blue"))).unwrap();
        get(dir.path(), PACKAGE, "color").unwrap();
        let started = std::time::Instant::now();
        for _ in 0..10_000 {
            assert_eq!(
                get(dir.path(), PACKAGE, "color").unwrap(),
                Some(json!("blue"))
            );
        }
        eprintln!("10,000 cached setting reads: {:?}", started.elapsed());
        let started = std::time::Instant::now();
        for n in 0..100 {
            save_with_source(dir.path(), &values(json!(n)), true).unwrap();
        }
        eprintln!("100 durable setting writes: {:?}", started.elapsed());
    }
}
