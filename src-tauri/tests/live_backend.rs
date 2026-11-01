//! Phase 1's live verification. Ignored by default — it needs real
//! credentials in `%APPDATA%\sc-asset-manager-client\config.toml` and it
//! writes to the configured Firebase project.
//!
//!   cargo test --test live_backend -- --ignored --nocapture

use sc_asset_manager_client_lib::config;
use sc_asset_manager_client_lib::probe::run_probe;

#[tokio::test]
#[ignore = "writes to the configured Firebase project"]
async fn phase1_probe_against_real_backend() {
    let cfg = config::load().expect("config loads");
    if !cfg.backend_ready() {
        eprintln!("backend not configured; skipping");
        return;
    }

    let r = run_probe(&cfg).await.expect("probe runs");

    eprintln!("\n=== Phase 1 probe ===");
    if let Some(id) = &r.test_asset_id {
        eprintln!("test asset left in place: {id}");
        eprintln!("  item: {}", r.test_asset_item.as_deref().unwrap_or("?"));
    }
    eprintln!("idempotent upsert: {:?}", r.idempotent);
    eprintln!();
    for f in &r.findings {
        eprintln!("[{}] {}", if f.as_assumed { "OK " } else { "!! " }, f.question);
        eprintln!("      -> {}", f.answer);
    }
    if !r.errors.is_empty() {
        eprintln!("\nerrors:");
        for e in &r.errors {
            eprintln!("  - {e}");
        }
    }
    eprintln!();

    assert!(r.errors.is_empty(), "probe reported errors: {:?}", r.errors);
    assert_eq!(r.idempotent, Some(true), "upsert must be idempotent");

    let broken: Vec<&str> = r
        .findings
        .iter()
        .filter(|f| !f.as_assumed)
        .map(|f| f.question.as_str())
        .collect();
    assert!(
        broken.is_empty(),
        "assumptions this client is built on do not hold: {broken:#?}"
    );
}
