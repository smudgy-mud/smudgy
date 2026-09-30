//! Process-global, TTL-bounded memory of **registry facts** from package update checks.
//!
//! The background update checker (UI-side) runs once per session open. Sessions share a
//! process, so a package checked by one session's sweep should not cost the network
//! again when another session opens moments later — that is the user-facing contract:
//! no re-check of the same package within the TTL window. What parks here is the
//! server's *answer* — the package's status, its latest live version with manifest,
//! modules, and dependency edges, the closure nodes, and the yanked/deleted status of
//! each installed version that was asked about. Nothing per-server parks here: consent,
//! staged versions, pins, and dismissals live in each server's lockfile, and every
//! session-open check re-evaluates its own server's entries against these shared facts.
//! Caching *outcomes* instead would smuggle one server's decisions into another's
//! (a pin on server A masking server B's update; an offer whose "current" names A's
//! version being pinned on B).
//!
//! The cache is in-memory only — persisting it to disk would buy little while inviting
//! staleness. Keys are case-folded `owner/name` pairs ([`facts_key`]): registry
//! identities are case-insensitive. (Accepted wrinkle: the facts are as the *checking
//! viewer* saw them, so signing in within the TTL can briefly keep an anonymous
//! `not_found` for a private-granted package.)

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use smudgy_cloud::package_api::{
    AvailableWithSmudgyUpgrade, CheckUpdatesResult, UpdateCheckClosureNode, UpdateCheckInstalled,
    UpdateCheckLatest,
};

/// How long checked facts stay fresh. Within this window another session's checker
/// reuses them instead of re-asking the registry.
const FACTS_TTL: Duration = Duration::from_mins(5);

/// A `(owner, name, version)` triple naming one concrete published package version —
/// enough to re-resolve a closure node when an offer is later accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageVersionRef {
    pub owner: String,
    pub name: String,
    pub version: String,
}

/// The case-folded cache key for one package. Owner nicknames and package names are
/// case-insensitive identities on the registry, and the same package may be installed
/// by several servers, so the key must not depend on any one lockfile's spelling.
#[must_use]
pub fn facts_key(owner: &str, name: &str) -> String {
    format!(
        "{}/{}",
        owner.to_ascii_lowercase(),
        name.to_ascii_lowercase()
    )
}

/// One package's parked registry facts: a `check-updates` result decomposed into its
/// server-truth parts, with the per-version `installed` statuses accumulated across
/// checks (two servers staging different versions each ask about their own).
struct StoredFacts {
    fetched_at: Instant,
    owner: String,
    name: String,
    status: String,
    latest: Option<UpdateCheckLatest>,
    available_with_smudgy_upgrade: Option<AvailableWithSmudgyUpgrade>,
    closure: Vec<UpdateCheckClosureNode>,
    /// Per queried installed version: the status the server reported for it (`None`
    /// when the server answered null — a version it has never seen).
    installed: HashMap<String, Option<UpdateCheckInstalled>>,
}

/// A TTL-bounded registry-facts store. The process-global instance is [`global`];
/// tests build their own with [`FactsCache::with_ttl`] to keep time control local.
pub struct FactsCache {
    ttl: Duration,
    entries: Mutex<HashMap<String, StoredFacts>>,
}

impl FactsCache {
    /// A cache whose facts stay fresh for `ttl`.
    #[must_use]
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// The still-fresh facts for `key`, reassembled as the `CheckUpdatesResult` a
    /// batch call would have returned for an entry staged at `installed`. `None` when
    /// the facts are stale or absent — or when `installed` names a version no fresh
    /// check has asked the server about, whose yanked/deleted status is therefore
    /// unknown (the caller batches that entry into its network call; the parked facts
    /// survive for entries they *can* answer). A stale entry is evicted on the way
    /// out, so the map never accumulates dead weight beyond one TTL window per package.
    #[must_use]
    pub fn get_fresh(&self, key: &str, installed: Option<&str>) -> Option<CheckUpdatesResult> {
        self.get_fresh_at(key, installed, Instant::now())
    }

    /// Parks one entry's check result, splitting it into the package's facts (status,
    /// latest, closure — replaced wholesale) and the `installed` status of the version
    /// the request asked about (merged into the per-version map, which older fresh
    /// answers for other versions survive). Restarts the freshness window.
    pub fn put(&self, key: &str, installed: Option<&str>, result: &CheckUpdatesResult) {
        self.put_at(key, installed, result, Instant::now());
    }

    fn get_fresh_at(
        &self,
        key: &str,
        installed: Option<&str>,
        now: Instant,
    ) -> Option<CheckUpdatesResult> {
        let mut entries = self.lock_entries();
        let facts = entries.get(key)?;
        // Freshness is strict: an entry exactly TTL old has expired.
        if now.saturating_duration_since(facts.fetched_at) >= self.ttl {
            entries.remove(key);
            return None;
        }
        let installed = match installed {
            None => None,
            Some(version) => *facts.installed.get(version)?,
        };
        Some(CheckUpdatesResult {
            owner: facts.owner.clone(),
            name: facts.name.clone(),
            status: facts.status.clone(),
            installed,
            latest: facts.latest.clone(),
            available_with_smudgy_upgrade: facts.available_with_smudgy_upgrade.clone(),
            closure: facts.closure.clone(),
        })
    }

    fn put_at(
        &self,
        key: &str,
        installed: Option<&str>,
        result: &CheckUpdatesResult,
        now: Instant,
    ) {
        let mut entries = self.lock_entries();
        let stale = entries
            .get(key)
            .is_none_or(|facts| now.saturating_duration_since(facts.fetched_at) >= self.ttl);
        let facts = if stale {
            entries.insert(
                key.to_string(),
                StoredFacts {
                    fetched_at: now,
                    owner: result.owner.clone(),
                    name: result.name.clone(),
                    status: result.status.clone(),
                    latest: result.latest.clone(),
                    available_with_smudgy_upgrade: result.available_with_smudgy_upgrade.clone(),
                    closure: result.closure.clone(),
                    installed: HashMap::new(),
                },
            );
            entries.get_mut(key).expect("just inserted")
        } else {
            let facts = entries.get_mut(key).expect("checked fresh above");
            facts.fetched_at = now;
            facts.status.clone_from(&result.status);
            facts.latest.clone_from(&result.latest);
            facts
                .available_with_smudgy_upgrade
                .clone_from(&result.available_with_smudgy_upgrade);
            facts.closure.clone_from(&result.closure);
            facts
        };
        if let Some(version) = installed {
            facts
                .installed
                .insert(version.to_string(), result.installed);
        }
    }

    fn lock_entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, StoredFacts>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The process-global facts cache (5-minute TTL) every session's checker shares.
#[must_use]
pub fn global() -> &'static FactsCache {
    static GLOBAL: OnceLock<FactsCache> = OnceLock::new();
    GLOBAL.get_or_init(|| FactsCache::with_ttl(FACTS_TTL))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(latest_version: Option<&str>) -> CheckUpdatesResult {
        CheckUpdatesResult {
            owner: "wbk".into(),
            name: "mapper".into(),
            status: "ok".into(),
            installed: Some(UpdateCheckInstalled {
                yanked: false,
                deleted: false,
            }),
            latest: latest_version.map(|version| UpdateCheckLatest {
                version: version.to_string(),
                published_at: chrono::Utc::now(),
                manifest: serde_json::json!({ "name": "mapper", "version": version }),
                modules: Vec::new(),
                dependencies: Vec::new(),
            }),
            available_with_smudgy_upgrade: None,
            closure: Vec::new(),
        }
    }

    #[test]
    fn fresh_facts_round_trip() {
        let cache = FactsCache::with_ttl(Duration::from_mins(5));
        let key = facts_key("wbk", "mapper");
        assert!(cache.get_fresh(&key, Some("1.2.0")).is_none());
        let mut registry_result = result(Some("1.3.0"));
        registry_result.available_with_smudgy_upgrade = Some(AvailableWithSmudgyUpgrade {
            package_version: "1.4.0".into(),
            minimum_smudgy_version: "0.5.9".into(),
        });
        cache.put(&key, Some("1.2.0"), &registry_result);
        let hit = cache
            .get_fresh(&key, Some("1.2.0"))
            .expect("fresh facts answer the same query");
        assert_eq!(
            hit.latest.as_ref().map(|l| l.version.as_str()),
            Some("1.3.0")
        );
        assert_eq!(
            hit.installed,
            Some(UpdateCheckInstalled {
                yanked: false,
                deleted: false
            })
        );
        assert_eq!(
            hit.available_with_smudgy_upgrade
                .as_ref()
                .map(|upgrade| upgrade.package_version.as_str()),
            Some("1.4.0")
        );
        // A different package is a miss; the key is case-folded.
        assert!(
            cache
                .get_fresh(&facts_key("wbk", "other"), Some("1.2.0"))
                .is_none()
        );
        assert!(
            cache
                .get_fresh(&facts_key("WBK", "Mapper"), Some("1.2.0"))
                .is_some()
        );
    }

    #[test]
    fn an_unasked_installed_version_is_a_miss_but_no_version_is_not() {
        // The `installed` status answers a question about one concrete version;
        // a server whose entry stages a DIFFERENT version cannot reuse it (its
        // yanked/deleted status is a fact of its own) — while an entry with no
        // staged version asked no question and needs no answer.
        let cache = FactsCache::with_ttl(Duration::from_mins(5));
        let key = facts_key("wbk", "mapper");
        cache.put(&key, Some("1.2.0"), &result(Some("1.3.0")));
        assert!(
            cache.get_fresh(&key, Some("1.1.0")).is_none(),
            "another server's staged version needs its own network answer"
        );
        let bare = cache
            .get_fresh(&key, None)
            .expect("a never-resolved entry reuses the package facts");
        assert_eq!(bare.installed, None);
        assert_eq!(
            bare.latest.as_ref().map(|l| l.version.as_str()),
            Some("1.3.0")
        );
    }

    #[test]
    fn put_merges_installed_versions_and_replaces_package_facts() {
        let cache = FactsCache::with_ttl(Duration::from_mins(5));
        let key = facts_key("wbk", "mapper");
        let t0 = Instant::now();
        cache.put_at(&key, Some("1.2.0"), &result(Some("1.3.0")), t0);
        // A second server's check for its own staged version merges in: both
        // versions now answer, and the package facts are the newest fetch's.
        let mut second = result(Some("1.4.0"));
        second.installed = Some(UpdateCheckInstalled {
            yanked: true,
            deleted: false,
        });
        cache.put_at(&key, Some("1.1.0"), &second, t0 + Duration::from_secs(30));
        let a = cache
            .get_fresh_at(&key, Some("1.2.0"), t0 + Duration::from_secs(60))
            .expect("the first query still answers");
        assert_eq!(a.latest.as_ref().map(|l| l.version.as_str()), Some("1.4.0"));
        assert_eq!(a.installed.map(|i| i.yanked), Some(false));
        let b = cache
            .get_fresh_at(&key, Some("1.1.0"), t0 + Duration::from_secs(60))
            .expect("the merged query answers");
        assert_eq!(b.installed.map(|i| i.yanked), Some(true));
    }

    #[test]
    fn facts_expire_at_ttl() {
        let cache = FactsCache::with_ttl(Duration::from_mins(5));
        let key = facts_key("wbk", "mapper");
        let t0 = Instant::now();
        cache.put_at(&key, Some("1.2.0"), &result(Some("1.3.0")), t0);
        // Just inside the window: fresh.
        assert!(
            cache
                .get_fresh_at(&key, Some("1.2.0"), t0 + Duration::from_secs(299))
                .is_some()
        );
        // Exactly at the boundary: expired (freshness is strict) and evicted.
        assert!(
            cache
                .get_fresh_at(&key, Some("1.2.0"), t0 + Duration::from_mins(5))
                .is_none()
        );
        assert!(cache.lock_entries().is_empty(), "stale facts are evicted");
        // A put over stale facts starts over — the old installed answers are gone.
        cache.put_at(
            &key,
            None,
            &result(Some("1.3.0")),
            t0 + Duration::from_mins(6),
        );
        assert!(
            cache
                .get_fresh_at(&key, Some("1.2.0"), t0 + Duration::from_mins(6))
                .is_none(),
            "an expired installed answer does not resurrect through a later put"
        );
    }

    #[test]
    fn zero_ttl_never_serves() {
        let cache = FactsCache::with_ttl(Duration::ZERO);
        let key = facts_key("wbk", "mapper");
        cache.put(&key, Some("1.2.0"), &result(Some("1.3.0")));
        assert!(cache.get_fresh(&key, Some("1.2.0")).is_none());
    }
}
