//! Shared comparison of independently running roots declared by package manifests.

use smudgy_core::models::shared_packages::PackageManifest;

pub(crate) type RequirementSignature = Vec<(String, String, String)>;

pub(crate) fn signature(manifest: &PackageManifest) -> Option<RequirementSignature> {
    let mut requirements = manifest
        .requires
        .iter()
        .map(|raw| {
            let dependency = smudgy_script::PackageDependency::parse(raw)?.ok()?;
            let range = semver::VersionReq::parse(dependency.range.as_deref().unwrap_or("*"))
                .ok()?
                .to_string();
            Some((
                dependency.key.owner.to_ascii_lowercase(),
                dependency.key.name.to_ascii_lowercase(),
                range,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    requirements.sort_unstable();
    requirements.dedup();
    Some(requirements)
}

pub(crate) fn wire_signature(manifest: &serde_json::Value) -> Option<RequirementSignature> {
    signature(&serde_json::from_value(manifest.clone()).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_normalize_spelling_and_reject_invalid_requirements() {
        let manifest = |requires| serde_json::json!({"version": "1.0.0", "requires": requires});
        assert_eq!(
            wire_signature(&manifest(vec!["smudgy://A/Worker", "smudgy://a/worker@*"])),
            wire_signature(&manifest(vec!["smudgy://a/worker@*"]))
        );
        assert!(wire_signature(&manifest(vec!["not-a-package"])).is_none());
        assert!(wire_signature(&manifest(vec!["smudgy://a/worker@invalid"])).is_none());
        assert!(wire_signature(&serde_json::json!({"requires": 7})).is_none());
    }
}
