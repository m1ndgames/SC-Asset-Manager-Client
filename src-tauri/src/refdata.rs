//! Reference data: the GUID → commodity name map, and the location list.
//!
//! Fetched **in Rust**, never by the webview. That is what keeps the CSP at
//! `default-src 'self'` — see PLAN.md §2. The webview receives the parsed
//! result over IPC and makes no outbound request of its own.
//!
//! ## Cached on disk, and usable offline
//!
//! A desktop app has to work with the network down, and reference data changes
//! rarely. So: load the cache at startup, serve from memory, refresh only on
//! request or when the cache is absent. A failed refresh keeps the cached copy
//! and reports the failure — it must never leave the app with no item names.
//!
//! ## `items.json` is deliberately not fetched
//!
//! The web app also publishes `items.json` (400 KB), but item and ammo
//! purchases are out of scope for this client: it reads commodity transactions
//! only. Fetching it would cost 400 KB per refresh to populate a combobox
//! nothing can reach.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config;
use crate::error::{AppError, AppResult};

/// One entry of `commodities.json`, keyed in that file by resource GUID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commodity {
    pub name: String,
    /// The web app's internal key, e.g. `Pressurized_Ice`. Carried through
    /// because it is what `ResourceType.<key>` in the log's own inventory lines
    /// uses, which makes it the join key if we ever need one.
    #[serde(default)]
    pub key: String,
}

/// The sentinel the web app puts at the head of its location list. It is a
/// placeholder in a `<select>`, not a place, and must never be offered as one.
const LOCATION_SENTINEL: &str = "<= UNINITIALIZED =>";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefData {
    /// resource GUID → commodity.
    pub commodities: BTreeMap<String, Commodity>,
    pub locations: Vec<String>,
    /// When this was last fetched successfully, RFC 3339. `None` for a cache
    /// that has never been populated.
    pub fetched_at: Option<String>,
    /// Where it came from, so Settings can show it.
    pub source_url: Option<String>,
}

impl RefData {
    pub fn resolve(&self, guid: &str) -> Option<&Commodity> {
        self.commodities.get(guid)
    }

    /// The display name for a GUID, or `None` when it does not resolve — which
    /// the review card must surface as an amber, required field rather than
    /// guessing.
    pub fn name_of(&self, guid: &str) -> Option<&str> {
        self.resolve(guid).map(|c| c.name.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.commodities.is_empty() && self.locations.is_empty()
    }
}

fn refdata_dir() -> AppResult<PathBuf> {
    Ok(config::config_dir()?.join("refdata"))
}

// ---------------------------------------------------------------------------
// Parsing — pure, so the odd shapes are tested without a network
// ---------------------------------------------------------------------------

/// `{"<guid>": {"name": ..., "key": ...}, ...}`
pub fn parse_commodities(text: &str) -> AppResult<BTreeMap<String, Commodity>> {
    serde_json::from_str(text)
        .map_err(|e| AppError::Refdata(format!("commodities.json is not valid: {e}")))
}

/// A flat array of names, minus the web app's placeholder and any blanks.
pub fn parse_locations(text: &str) -> AppResult<Vec<String>> {
    let raw: Vec<String> = serde_json::from_str(text)
        .map_err(|e| AppError::Refdata(format!("locations.json is not valid: {e}")))?;
    Ok(raw
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != LOCATION_SENTINEL)
        .collect())
}

// ---------------------------------------------------------------------------
// Disk cache
// ---------------------------------------------------------------------------

pub fn load_from(dir: &Path) -> RefData {
    let path = dir.join("refdata.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return RefData::default();
    };
    match serde_json::from_str(&text) {
        Ok(d) => d,
        Err(e) => {
            // Same reasoning as `state.json`: a corrupt cache must degrade to
            // "no names yet", never to a dead app.
            tracing::error!(error = %e, path = %path.display(), "refdata cache unreadable; ignoring it");
            RefData::default()
        }
    }
}

pub fn save_to(dir: &Path, d: &RefData) -> AppResult<()> {
    std::fs::create_dir_all(dir)
        .map_err(|e| AppError::Refdata(format!("cannot create {}: {e}", dir.display())))?;
    let bytes = serde_json::to_vec_pretty(d)
        .map_err(|e| AppError::Refdata(format!("cannot serialise refdata: {e}")))?;
    config::write_atomic(
        &dir.join("refdata.json.tmp"),
        &dir.join("refdata.json"),
        &bytes,
    )
    .map_err(|e| AppError::Refdata(e.to_string()))
}

pub fn load() -> RefData {
    match refdata_dir() {
        Ok(d) => load_from(&d),
        Err(e) => {
            tracing::error!(error = %e, "no config directory; reference data will not cache");
            RefData::default()
        }
    }
}

pub fn save(d: &RefData) -> AppResult<()> {
    save_to(&refdata_dir()?, d)
}

// ---------------------------------------------------------------------------
// Fetch
// ---------------------------------------------------------------------------

/// Fetch both files and return the result. Does not touch the cache — the
/// caller decides whether to keep it, so a partial failure cannot half-replace
/// good data.
pub async fn fetch(base_url: &str) -> AppResult<RefData> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(AppError::Refdata(
            "no SC-Asset-Manager URL set in Settings".into(),
        ));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent(concat!("sc-asset-manager-client/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| AppError::Refdata(format!("cannot build an HTTP client: {e}")))?;

    let commodities = get_text(&client, &format!("{base}/commodities.json")).await?;
    let locations = get_text(&client, &format!("{base}/locations.json")).await?;

    Ok(RefData {
        commodities: parse_commodities(&commodities)?,
        locations: parse_locations(&locations)?,
        fetched_at: Some(crate::model::now_iso()),
        source_url: Some(base.to_string()),
    })
}

async fn get_text(client: &reqwest::Client, url: &str) -> AppResult<String> {
    let res = client
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::Refdata(format!("cannot reach {url}: {e}")))?;
    if !res.status().is_success() {
        return Err(AppError::Refdata(format!(
            "{url} returned {}",
            res.status()
        )));
    }
    res.text()
        .await
        .map_err(|e| AppError::Refdata(format!("cannot read {url}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commodities_parse_from_the_published_shape() {
        let got = parse_commodities(
            r#"{"a11394c9-ad7c-404d-9209-3a5e57bb4aa4":{"name":"Ammonia","key":"Ammonia"},
                "91ad8b9e-426e-462a-a7f5-cf06eb79971a":{"name":"Evidence Box","key":"EvidenceBox"}}"#,
        )
        .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got["a11394c9-ad7c-404d-9209-3a5e57bb4aa4"].name, "Ammonia");
    }

    /// A missing `key` must not fail the whole file — one new field shape
    /// should not cost the user every item name.
    #[test]
    fn a_commodity_without_a_key_still_parses() {
        let got = parse_commodities(r#"{"g":{"name":"Thing"}}"#).unwrap();
        assert_eq!(got["g"].name, "Thing");
        assert_eq!(got["g"].key, "");
    }

    #[test]
    fn the_web_apps_placeholder_is_not_a_location() {
        let got = parse_locations(
            r#"["<= UNINITIALIZED =>", "Area18", "  ", "Baijini Point", ""]"#,
        )
        .unwrap();
        assert_eq!(got, vec!["Area18", "Baijini Point"]);
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        assert!(parse_commodities("{nope").is_err());
        assert!(parse_locations("{}").is_err());
    }

    #[test]
    fn an_unresolved_guid_is_reported_as_such() {
        let d = RefData {
            commodities: parse_commodities(r#"{"known":{"name":"Ammonia","key":"Ammonia"}}"#)
                .unwrap(),
            ..Default::default()
        };
        assert_eq!(d.name_of("known"), Some("Ammonia"));
        assert_eq!(d.name_of("unknown"), None);
    }

    #[test]
    fn the_cache_round_trips_and_degrades_safely() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_from(dir.path()).is_empty());

        let d = RefData {
            commodities: parse_commodities(r#"{"g":{"name":"Ammonia","key":"Ammonia"}}"#).unwrap(),
            locations: vec!["Area18".into()],
            fetched_at: Some("2026-11-01T18:00:00.000Z".into()),
            source_url: Some("https://sc-assetmanager.org".into()),
        };
        save_to(dir.path(), &d).unwrap();
        assert_eq!(load_from(dir.path()), d);

        std::fs::write(dir.path().join("refdata.json"), "{ truncated").unwrap();
        assert!(load_from(dir.path()).is_empty());
    }

    #[tokio::test]
    async fn an_empty_base_url_is_a_clear_error_not_a_request() {
        let e = fetch("   ").await.unwrap_err();
        assert_eq!(e.kind(), "refdata");
    }

    /// Every GUID the parser has ever seen in a real log must resolve, or the
    /// review card falls back to asking the user to name the item by hand.
    /// Guards against the published map dropping a commodity.
    #[test]
    fn every_corpus_guid_resolves() {
        let text = include_str!("../tests/fixtures/commodities.json");
        let map = parse_commodities(text).unwrap();
        let d = RefData {
            commodities: map,
            ..Default::default()
        };

        let mut checked = 0;
        for entry in std::fs::read_dir("tests/fixtures").unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("log") {
                continue;
            }
            let log = std::fs::read(&path).unwrap();
            let log = String::from_utf8_lossy(&log);
            for line in log.lines() {
                if let Some(crate::log::parse::ParsedLine::Trade(t)) =
                    crate::log::parse::parse_line(line)
                {
                    assert!(
                        d.resolve(&t.resource_guid).is_some(),
                        "{} does not resolve (from {})",
                        t.resource_guid,
                        path.display()
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked >= 91, "expected the whole corpus, saw {checked}");
    }
}
