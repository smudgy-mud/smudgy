//! The cross-tree dedupe / coexistence solve.
//!
//! Given every requirement the install closure places on each shared package, decide the
//! concrete version each requirement resolves to:
//!
//! - **Collapse** — semver-compatible requirements (same compatibility class) collapse to
//!   the **highest version any dependent locked**; never floats above an actual lock.
//! - **Coexist** — incompatible classes (e.g. `1.x` vs `2.x`) resolve to distinct
//!   versions (two instances, two canonical URLs).
//! - **Pin exemption** — an exact pin (an author `@=x` dep or a user install-pin) resolves
//!   to *exactly* its version, is exempt from upgrade-collapse, and coexists when it
//!   differs from a compatible group's collapsed version.
//! - **Warn** — a package that ends up at ≥2 distinct versions is flagged (the
//!   side-effect-collision risk in the shared isolate).
//!
//! Per-isolate (`script/PACKAGE-ISOLATES.md`, `script/PACKAGE-ISOLATES-RESOLUTION.md`): this
//! solve runs **once per isolate** — the rules above apply *within* an isolate, but there is no
//! collapse across isolates, and the duplicate-version warning is intra-isolate. Each isolate has
//! its own provider instance (`SmudgyPackageProvider::fork`) and runs the same solver over its own
//! closure (one isolate's, not the session's).
//!
//! Pure + synchronous: the provider walks the closure to gather [`DepRequirement`]s and
//! feeds them here, then resolves each edge at the solved version.

use std::collections::{BTreeSet, HashMap, VecDeque};

use semver::Version;
use smudgy_script::PackageKey;

/// One requirement on a shared package: a dependent locked `package` at concrete
/// `version`, with `is_pin` set when the requirement is exempt from upgrade-collapse (an
/// author `@=x` exact dep or a user install-pin). Also used for a top-level install root
/// (`version` = the version it resolves to, `is_pin` = a user install-pin).
#[derive(Debug, Clone)]
pub struct DepRequirement {
    pub package: PackageKey,
    pub version: String,
    pub is_pin: bool,
}

/// One importer→dependency edge discovered in the closure walk: the module of importer
/// instance `(importer, importer_version)` imports `dep`, which that importer locked at
/// `dep_version` (`dep_is_pin` = an author `@=x` exact dep). Used to compute which deps
/// *actually* load (only the deps of a surviving importer version load).
#[derive(Debug, Clone)]
pub struct DepEdge {
    pub importer: PackageKey,
    pub importer_version: String,
    pub dep: PackageKey,
    pub dep_version: String,
    pub dep_is_pin: bool,
}

/// The semver "compatibility class" of a version (caret semantics, as Cargo/npm use): two
/// versions share a class iff a `^` range on one admits the other. `>=1.0.0` groups by
/// major; `0.x.y` by minor; `0.0.z` by patch (each is its own class).
fn compat_class(v: &Version) -> (u64, u64, u64) {
    if v.major > 0 {
        (v.major, 0, 0)
    } else if v.minor > 0 {
        (0, v.minor, 0)
    } else {
        (0, 0, v.patch)
    }
}

/// Whether a declared dependency `range` is an exact single-version pin (`=x.y.z`) and so
/// exempt from upgrade-collapse. A bare/`^`/`~`/partial (`=1.2`) range is **not** a pin.
#[must_use]
pub fn is_exact_pin(range: &str) -> bool {
    semver::VersionReq::parse(range).is_ok_and(|req| {
        req.comparators.len() == 1
            && req.comparators[0].op == semver::Op::Exact
            && req.comparators[0].minor.is_some()
            && req.comparators[0].patch.is_some()
    })
}

/// The solved closure: per `(package, compatibility-class)` non-pin group, the collapsed
/// version (the highest non-pin version locked in that class).
#[derive(Debug, Default)]
pub struct Solve {
    collapsed: HashMap<(PackageKey, (u64, u64, u64)), Version>,
}

impl Solve {
    /// The concrete version an edge resolves to: a pin keeps its exact version; a non-pin
    /// collapses to the highest non-pin version in its compatibility class. A version that
    /// doesn't parse as semver, or has no recorded group, resolves to itself.
    #[must_use]
    pub fn resolve(&self, package: &PackageKey, version: &str, is_pin: bool) -> String {
        if is_pin {
            return version.to_string();
        }
        match Version::parse(version) {
            Ok(parsed) => self
                .collapsed
                .get(&(package.clone(), compat_class(&parsed)))
                .map_or_else(|| version.to_string(), ToString::to_string),
            Err(_) => version.to_string(),
        }
    }

    /// The versions of each package that **actually load**, by BFS from the solved
    /// top-level `roots` along `edges`: a module loads at its solved version, and only the
    /// deps of *that* version's module load. So a dep pulled in solely by a collapsed-away
    /// importer version never loads (and never warns). `roots` are the top-level installs.
    #[must_use]
    pub fn loaded_versions(
        &self,
        roots: &[DepRequirement],
        edges: &[DepEdge],
    ) -> HashMap<PackageKey, BTreeSet<String>> {
        let mut loaded: HashMap<PackageKey, BTreeSet<String>> = HashMap::new();
        let mut queue: VecDeque<(PackageKey, String)> = VecDeque::new();
        let enqueue = |loaded: &mut HashMap<PackageKey, BTreeSet<String>>,
                           queue: &mut VecDeque<(PackageKey, String)>,
                           package: PackageKey,
                           version: String| {
            if loaded.entry(package.clone()).or_default().insert(version.clone()) {
                queue.push_back((package, version));
            }
        };
        for root in roots {
            let version = self.resolve(&root.package, &root.version, root.is_pin);
            enqueue(&mut loaded, &mut queue, root.package.clone(), version);
        }
        while let Some((importer, importer_version)) = queue.pop_front() {
            // Only the loaded (solved) module's own deps load; a collapsed-away version's
            // edges (importer_version != the surviving version) never fire.
            for edge in edges
                .iter()
                .filter(|edge| edge.importer == importer && edge.importer_version == importer_version)
            {
                let version = self.resolve(&edge.dep, &edge.dep_version, edge.dep_is_pin);
                enqueue(&mut loaded, &mut queue, edge.dep.clone(), version);
            }
        }
        loaded
    }

    /// Packages that **actually load** at ≥2 distinct versions (the duplicate-version
    /// warning set), each with its sorted distinct versions. Computed over the loaded
    /// closure, so compatible ranges that collapse to one version don't warn and deps of
    /// collapsed-away versions don't warn — only genuine coexistence does.
    #[must_use]
    pub fn loaded_duplicates(
        &self,
        roots: &[DepRequirement],
        edges: &[DepEdge],
    ) -> Vec<(PackageKey, Vec<String>)> {
        let mut out: Vec<(PackageKey, Vec<String>)> = self
            .loaded_versions(roots, edges)
            .into_iter()
            .filter(|(_, versions)| versions.len() >= 2)
            .map(|(package, versions)| (package, versions.into_iter().collect()))
            .collect();
        // Deterministic order for stable warning output.
        out.sort_by_key(|a| a.0.to_user_specifier());
        out
    }
}

/// Solve a closure's requirements into per-class collapsed versions. Non-semver
/// versions are skipped (they can't participate in compatibility grouping).
#[must_use]
pub fn solve(requirements: &[DepRequirement]) -> Solve {
    let mut collapsed: HashMap<(PackageKey, (u64, u64, u64)), Version> = HashMap::new();
    for req in requirements.iter().filter(|req| !req.is_pin) {
        let Ok(version) = Version::parse(&req.version) else {
            continue;
        };
        let class = compat_class(&version);
        collapsed
            .entry((req.package.clone(), class))
            .and_modify(|highest| {
                if version > *highest {
                    *highest = version.clone();
                }
            })
            .or_insert(version);
    }
    Solve { collapsed }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> PackageKey {
        PackageKey {
            owner: "wbk".into(),
            name: name.into(),
        }
    }

    fn range_req(name: &str, version: &str) -> DepRequirement {
        DepRequirement {
            package: key(name),
            version: version.into(),
            is_pin: false,
        }
    }

    /// Build a closure where each `(importer, dep_version, is_pin)` is a distinct top-level
    /// install at `1.0.0` depending on `util` at the given version. Returns the solve plus
    /// the roots + edges for `loaded_duplicates`.
    fn util_closure(importers: &[(&str, &str, bool)]) -> (Solve, Vec<DepRequirement>, Vec<DepEdge>) {
        let mut requirements = Vec::new();
        let mut roots = Vec::new();
        let mut edges = Vec::new();
        for (name, version, is_pin) in importers {
            let root = range_req(name, "1.0.0");
            requirements.push(root.clone());
            roots.push(root);
            requirements.push(DepRequirement {
                package: key("util"),
                version: (*version).into(),
                is_pin: *is_pin,
            });
            edges.push(DepEdge {
                importer: key(name),
                importer_version: "1.0.0".into(),
                dep: key("util"),
                dep_version: (*version).into(),
                dep_is_pin: *is_pin,
            });
        }
        (solve(&requirements), roots, edges)
    }

    #[test]
    fn exact_pin_detection() {
        assert!(is_exact_pin("=1.0.0"));
        assert!(is_exact_pin("=2.3.4"));
        // Ranges and partial exacts are NOT single-version pins.
        assert!(!is_exact_pin("^1.2"));
        assert!(!is_exact_pin("~1.2.0"));
        assert!(!is_exact_pin("1.2.3")); // bare == caret, not a pin
        assert!(!is_exact_pin("=1.2")); // no patch -> a range, not a single version
        assert!(!is_exact_pin("*"));
        assert!(!is_exact_pin("garbage"));
    }

    #[test]
    fn compatible_ranges_collapse_to_highest_locked() {
        // A and B both depend on util in the 1.x class, locking 1.3.0 and 1.4.0.
        let (solve, roots, edges) =
            util_closure(&[("a", "1.3.0", false), ("b", "1.4.0", false)]);
        assert_eq!(solve.resolve(&key("util"), "1.3.0", false), "1.4.0");
        assert_eq!(solve.resolve(&key("util"), "1.4.0", false), "1.4.0");
        // One instance loads -> no duplicate-version warning.
        assert!(solve.loaded_duplicates(&roots, &edges).is_empty());
    }

    #[test]
    fn never_floats_above_the_highest_lock() {
        // Even though 1.9.0 may be published, nobody locked it, so the group stays at the
        // highest LOCKED version.
        let reqs = [range_req("util", "1.2.0"), range_req("util", "1.5.0")];
        let solve = solve(&reqs);
        assert_eq!(solve.resolve(&key("util"), "1.2.0", false), "1.5.0");
    }

    #[test]
    fn incompatible_majors_coexist() {
        let (solve, roots, edges) =
            util_closure(&[("a", "1.4.0", false), ("b", "2.0.1", false)]);
        assert_eq!(solve.resolve(&key("util"), "1.4.0", false), "1.4.0");
        assert_eq!(solve.resolve(&key("util"), "2.0.1", false), "2.0.1");
        let dups = solve.loaded_duplicates(&roots, &edges);
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].1, vec!["1.4.0", "2.0.1"]);
    }

    #[test]
    fn zerover_minors_are_incompatible() {
        // Per semver, 0.1.x and 0.2.x are incompatible classes (breaking by minor).
        let cross_minor = solve(&[range_req("util", "0.1.5"), range_req("util", "0.2.0")]);
        assert_eq!(cross_minor.resolve(&key("util"), "0.1.5", false), "0.1.5");
        assert_eq!(cross_minor.resolve(&key("util"), "0.2.0", false), "0.2.0");
        // But 0.1.x collapses within its own minor.
        let same_minor = solve(&[range_req("util", "0.1.2"), range_req("util", "0.1.9")]);
        assert_eq!(same_minor.resolve(&key("util"), "0.1.2", false), "0.1.9");
    }

    #[test]
    fn exact_pin_is_exempt_and_coexists_when_older() {
        // Worked example: A^1->1.3.0, B^1->1.4.0, C pins =1.1.0, D^2->2.0.1.
        let (solve, roots, edges) = util_closure(&[
            ("a", "1.3.0", false),
            ("b", "1.4.0", false),
            ("c", "1.1.0", true),
            ("d", "2.0.1", false),
        ]);
        // A and B share the collapsed 1.4.0.
        assert_eq!(solve.resolve(&key("util"), "1.3.0", false), "1.4.0");
        assert_eq!(solve.resolve(&key("util"), "1.4.0", false), "1.4.0");
        // The pin keeps EXACTLY 1.1.0 (not collapsed up to 1.4.0).
        assert_eq!(solve.resolve(&key("util"), "1.1.0", true), "1.1.0");
        // The incompatible major is its own instance.
        assert_eq!(solve.resolve(&key("util"), "2.0.1", false), "2.0.1");
        // Exactly three coexisting versions LOAD -> one duplicate-version warning.
        let dups = solve.loaded_duplicates(&roots, &edges);
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].0, key("util"));
        assert_eq!(dups[0].1, vec!["1.1.0", "1.4.0", "2.0.1"]);
    }

    #[test]
    fn a_pin_equal_to_the_collapse_shares_the_instance() {
        // C pins =1.4.0, which equals the non-pin group's collapse -> they share (one
        // instance), so no warning.
        let (solve, roots, edges) = util_closure(&[
            ("a", "1.3.0", false),
            ("b", "1.4.0", false),
            ("c", "1.4.0", true),
        ]);
        assert_eq!(solve.resolve(&key("util"), "1.4.0", true), "1.4.0");
        assert_eq!(solve.resolve(&key("util"), "1.3.0", false), "1.4.0");
        assert!(
            solve.loaded_duplicates(&roots, &edges).is_empty(),
            "pin == collapse -> shared, no warning"
        );
    }

    #[test]
    fn a_pin_above_the_collapse_does_not_pull_non_pins_up() {
        // C pins =1.5.0 (higher than any non-pin lock). Non-pins stay at their highest
        // LOCKED (1.4.0); the pin is its own coexisting instance.
        let (solve, roots, edges) = util_closure(&[
            ("a", "1.3.0", false),
            ("b", "1.4.0", false),
            ("c", "1.5.0", true),
        ]);
        assert_eq!(solve.resolve(&key("util"), "1.3.0", false), "1.4.0");
        assert_eq!(solve.resolve(&key("util"), "1.5.0", true), "1.5.0");
        let dups = solve.loaded_duplicates(&roots, &edges);
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].1, vec!["1.4.0", "1.5.0"]);
    }

    #[test]
    fn distinct_packages_dont_interfere() {
        let solve = solve(&[range_req("util", "1.0.0"), range_req("other", "2.0.0")]);
        assert_eq!(solve.resolve(&key("util"), "1.0.0", false), "1.0.0");
        assert_eq!(solve.resolve(&key("other"), "2.0.0", false), "2.0.0");
    }

    #[test]
    fn dep_of_a_collapsed_away_importer_version_does_not_warn() {
        // app coexists at 1.0.0 and 2.0.0 (incompatible majors). app@1.0.0 deps helper@1.0.0;
        // app@2.0.0 deps helper@2.0.0. Both app versions actually load (incompatible), so
        // BOTH helper versions load -> helper warns.
        let roots = vec![range_req("app", "1.0.0"), range_req("app", "2.0.0")];
        let requirements = vec![
            range_req("app", "1.0.0"),
            range_req("app", "2.0.0"),
            range_req("helper", "1.0.0"),
            range_req("helper", "2.0.0"),
        ];
        let edges = vec![
            DepEdge { importer: key("app"), importer_version: "1.0.0".into(), dep: key("helper"), dep_version: "1.0.0".into(), dep_is_pin: false },
            DepEdge { importer: key("app"), importer_version: "2.0.0".into(), dep: key("helper"), dep_version: "2.0.0".into(), dep_is_pin: false },
        ];
        let coexisting = solve(&requirements);
        let dups = coexisting.loaded_duplicates(&roots, &edges);
        // Both app majors genuinely load (incompatible), so both helper majors load too.
        assert!(
            dups.iter().any(|(package, _)| *package == key("helper")),
            "both app majors load -> helper coexists"
        );

        // Now make the two app versions COMPATIBLE (1.0.0 and 1.4.0): app collapses to
        // 1.4.0, so app@1.0.0 never loads and its helper@1.0.0 is never imported. Only
        // helper@2.0.0 (app@1.4.0's dep) loads -> NO spurious helper warning.
        let roots = vec![range_req("app", "1.0.0"), range_req("app", "1.4.0")];
        let requirements = vec![
            range_req("app", "1.0.0"),
            range_req("app", "1.4.0"),
            range_req("helper", "1.0.0"),
            range_req("helper", "2.0.0"),
        ];
        let edges = vec![
            DepEdge { importer: key("app"), importer_version: "1.0.0".into(), dep: key("helper"), dep_version: "1.0.0".into(), dep_is_pin: false },
            DepEdge { importer: key("app"), importer_version: "1.4.0".into(), dep: key("helper"), dep_version: "2.0.0".into(), dep_is_pin: false },
        ];
        let collapsing = solve(&requirements);
        let loaded = collapsing.loaded_versions(&roots, &edges);
        assert_eq!(loaded[&key("app")].iter().collect::<Vec<_>>(), vec!["1.4.0"]);
        assert_eq!(loaded[&key("helper")].iter().collect::<Vec<_>>(), vec!["2.0.0"]);
        assert!(
            collapsing.loaded_duplicates(&roots, &edges).is_empty(),
            "helper@1.0.0 was pulled only by the collapsed-away app@1.0.0 -> no warning"
        );
    }
}
