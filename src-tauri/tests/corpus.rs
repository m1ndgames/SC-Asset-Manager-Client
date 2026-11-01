//! Corpus-wide invariants, run against committed fixtures.
//!
//! These are the assertions that would catch a game patch changing the log
//! format. They run in CI with no access to a real install — the fixtures are
//! redacted slices of 11 real sessions spanning game builds 10989003
//! (Dec 2025) to 12913470 (Oct 2026). See `tests/fixtures/README.md`.

use std::fs;
use std::path::{Path, PathBuf};

use sc_asset_manager_client_lib::ids::TradeKind;
use sc_asset_manager_client_lib::log::parse::{parse_line, ParsedLine};
use sc_asset_manager_client_lib::log::scan::{scan, DEFAULT_HOLD_MS};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture_files() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(fixtures_dir())
        .expect("fixtures directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "log"))
        .collect();
    v.sort();
    assert!(!v.is_empty(), "no fixtures found");
    v
}

fn read(p: &Path) -> String {
    String::from_utf8_lossy(&fs::read(p).expect("fixture readable")).into_owned()
}

/// The headline expectation from PLAN.md §12, now with the real numbers.
///
/// The current session logs 4 buys and 3 sells, and one buy fails 1.184 s
/// later. A correct parse therefore yields 6 trades, not 7 — one assertion
/// exercising extraction *and* the grace window.
#[test]
fn current_session_yields_six_trades_not_seven() {
    let r = scan(
        &read(&fixtures_dir().join("session-12913470-current.log")),
        DEFAULT_HOLD_MS,
    );

    assert_eq!(r.buys, 4, "buy request lines");
    assert_eq!(r.sells, 3, "sell request lines");
    assert_eq!(r.errors, 1, "failure lines");

    assert_eq!(r.trades.len(), 6, "surviving trades");
    assert_eq!(r.discarded.len(), 1, "the failed buy");
    assert_eq!(r.discarded[0].delta_ms, 1184);
    assert_eq!(r.discarded[0].error.result, "CargoCreationFailed");
    assert_eq!(r.discarded[0].trade.kind, TradeKind::Buy);
    assert!(r.unmatched_errors.is_empty());

    // The discarded one is the published id vector 1 line.
    assert_eq!(r.discarded[0].trade.id, "4ed7a396-7398-590d-be1c-d3a012c0dcb6");
}

/// Across every fixture: nothing unparseable. A non-empty `malformed` list is
/// precisely the signal that the log format moved.
#[test]
fn no_class_matching_line_is_unparseable() {
    let mut malformed = Vec::new();
    for f in fixture_files() {
        for line in read(&f).lines() {
            if let Some(ParsedLine::Malformed { raw, missing, .. }) = parse_line(line) {
                malformed.push(format!("{}: missing {:?}\n  {raw}", f.display(), missing));
            }
        }
    }
    assert!(
        malformed.is_empty(),
        "{} unparseable line(s):\n{}",
        malformed.len(),
        malformed.join("\n")
    );
}

/// The corpus totals. If a future fixture is added these move; that is fine,
/// but it should be a deliberate edit.
#[test]
fn corpus_totals_are_stable() {
    let (mut buys, mut sells) = (0, 0);
    for f in fixture_files() {
        let r = scan(&read(&f), DEFAULT_HOLD_MS);
        buys += r.buys;
        sells += r.sells;
    }
    assert_eq!(buys, 21, "buy lines across the corpus");
    assert_eq!(sells, 70, "sell lines across the corpus");
}

/// Summed cargo-box parity holds on 90 of 91 trade lines.
///
/// The single exception is one `boxSize[16] | unitAmount[7]` sell with
/// `quantity[576]`. Pinning the count to exactly 1 means both a regression
/// (parity breaking widely) and a silent fix (the anomaly disappearing) show
/// up as a failure rather than going unnoticed.
#[test]
fn parity_holds_except_for_one_known_anomaly() {
    let mut mismatches = Vec::new();
    for f in fixture_files() {
        for line in read(&f).lines() {
            if let Some(ParsedLine::Trade(t)) = parse_line(line) {
                if t.parity.is_mismatch() {
                    mismatches.push((f.file_name().unwrap().to_string_lossy().into_owned(), t));
                }
            }
        }
    }
    assert_eq!(
        mismatches.len(),
        1,
        "expected exactly one parity anomaly, got: {:?}",
        mismatches
            .iter()
            .map(|(f, t)| format!("{f}: {:?} q={}", t.parity, t.quantity_scu))
            .collect::<Vec<_>>()
    );
    let (_, t) = &mismatches[0];
    assert_eq!(t.quantity_scu, 576.0);
    assert_eq!(t.box_groups.len(), 1);
}

/// `shopPricePerCentiSCU x 100` must agree with `price / quantityScu` to within
/// 0.1% on every buy. This is the drift detector that does not depend on cargo
/// box data at all.
#[test]
fn buy_price_cross_check_holds_across_the_corpus() {
    let mut checked = 0;
    for f in fixture_files() {
        for line in read(&f).lines() {
            if let Some(ParsedLine::Trade(t)) = parse_line(line) {
                if t.kind != TradeKind::Buy {
                    continue;
                }
                let drift = t
                    .price_cross_check
                    .expect("every buy carries a cross-check");
                assert!(
                    drift < 0.001,
                    "{}: drift {drift} on {}",
                    f.display(),
                    t.id
                );
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 21);
}

/// Unit tokens are structural: buys always carry ` cSCU`, sells never do.
/// 21/21 and 70/70 across the corpus.
#[test]
fn quantity_unit_token_is_consistent_per_kind() {
    for f in fixture_files() {
        for line in read(&f).lines() {
            let Some(ParsedLine::Trade(t)) = parse_line(line) else {
                continue;
            };
            let raw = t.fields.get("quantity").expect("quantity field");
            match t.kind {
                TradeKind::Buy => assert!(
                    raw.contains("cSCU"),
                    "buy quantity should be cSCU-tagged: {raw}"
                ),
                TradeKind::Sell => assert!(
                    !raw.contains("SCU"),
                    "sell quantity should carry no unit token: {raw}"
                ),
            }
            // Whatever the tag, the stored raw value is pure number text.
            assert!(
                t.quantity_raw.chars().all(|c| c.is_ascii_digit() || c == '.'),
                "quantity_raw must stay numeric text: {}",
                t.quantity_raw
            );
        }
    }
}

/// Ids are deterministic and collide only on genuinely identical lines.
#[test]
fn ids_are_unique_per_distinct_trade_line() {
    use std::collections::HashMap;
    let mut by_id: HashMap<String, String> = HashMap::new();
    for f in fixture_files() {
        for line in read(&f).lines() {
            let Some(ParsedLine::Trade(t)) = parse_line(line) else {
                continue;
            };
            if let Some(prev) = by_id.insert(t.id.clone(), line.to_string()) {
                assert_eq!(
                    prev, line,
                    "two different lines produced the same id {}",
                    t.id
                );
            }
        }
    }
    assert!(by_id.len() >= 85, "got {} distinct ids", by_id.len());
}

/// Lines whose method token contains a nested `<lambda_1>` must land in
/// `Ignored`, never be mistaken for a trade. There are hundreds of these
/// (`LoadShopInventoryData::<lambda_1>::operator ()`), and they carry
/// `shopId`/`shopName` fields that a sloppier classifier could latch onto.
#[test]
fn lambda_method_lines_are_ignored() {
    let line = "<2026-11-01T09:10:35.415Z> [Notice] <CEntityComponentCommodityUIProvider::LoadShopInventoryData::<lambda_1>::operator ()> AddingCommodityBox - playerId[000000000000] shopId[863690278779] shopName[SCShop_Admin_lt_base_g] commodityName[ResourceType.Waste]";
    match parse_line(line) {
        Some(ParsedLine::Ignored { .. }) => {}
        other => panic!("expected Ignored, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Live install. Ignored by default so CI never needs a real Game.log.
//   cargo test --test corpus -- --ignored --nocapture
// ---------------------------------------------------------------------------

/// Scan the actual installed Game.log and print what the watcher would see.
///
/// This is the Phase 1 proving step from PLAN.md §11: real data, real
/// arithmetic, no tailer and no backend.
#[test]
#[ignore = "needs a real Star Citizen install"]
fn live_game_log_scan() {
    use sc_asset_manager_client_lib::log::discover;

    let Some(path) = discover::detect() else {
        eprintln!("no Game.log found; skipping");
        return;
    };
    eprintln!("scanning {}", path.display());

    let bytes = fs::read(&path).expect("readable");
    let r = scan(&String::from_utf8_lossy(&bytes), DEFAULT_HOLD_MS);

    eprintln!(
        "  {} lines, {} class-matched, {} buys, {} sells, {} failures",
        r.total_lines, r.class_matched, r.buys, r.sells, r.errors
    );
    eprintln!(
        "  {} detected, {} discarded, {} unmatched failures",
        r.trades.len(),
        r.discarded.len(),
        r.unmatched_errors.len()
    );
    eprintln!(
        "  drift signals -> unparseable {}, parity mismatches {}, price drift {}",
        r.malformed.len(),
        r.parity_mismatches,
        r.price_drift
    );
    eprintln!("  shops: {:?}", r.shop_names);
    eprintln!("  locationIds: {:?}", r.location_ids);

    for t in &r.trades {
        eprintln!(
            "  {:4} {}  {:>10.2} SCU @ {:<14} = {:>12} aUEC  {}  loc={:?}",
            format!("{:?}", t.line.kind),
            &t.line.log_timestamp[11..23],
            t.line.quantity_scu,
            t.line.unit_price_per_scu,
            t.line.total_auec,
            t.line.shop_name.as_deref().unwrap_or("—"),
            t.location_id_hint
        );
    }
    for d in &r.discarded {
        eprintln!(
            "  DISCARDED {:?} {} -> {} after {}ms",
            d.trade.kind, d.trade.id, d.error.result, d.delta_ms
        );
    }

    // The invariants must hold on live data too, whatever the counts are.
    assert!(
        r.malformed.is_empty(),
        "live log has {} unparseable class-matching line(s) — the format may have changed",
        r.malformed.len()
    );
    for t in &r.trades {
        if t.line.kind == TradeKind::Buy {
            let drift = t.line.price_cross_check.expect("buy");
            assert!(drift < 0.001, "price drift {drift} on {}", t.line.id);
        }
    }
}
