//! Opt-in measurements of installed `NukeFire` packages in the real session host.
//! Set `SMUDGY_PACKING_BENCH_ROOT` and optionally `SMUDGY_PACKING_BENCH_CONFIG`.
//! Config paths and all generated stores, package copies and results stay below
//! that root. Without a config, only a synthetic map is used. The test measures
//! host publication, not an iced paint, and never edits a user profile.

#[path = "support/packing_bench/mod.rs"]
mod packing_bench;

#[tokio::test]
#[ignore = "host benchmark: run alone with --ignored --nocapture"]
async fn installed_packages_remain_responsive_during_layout() {
    packing_bench::run().await;
}
