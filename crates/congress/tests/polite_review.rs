//! Review tests for the polite Congress refresh: discovery-date advancement,
//! transport-failure behaviour, provider switch and pause display.

use std::sync::Mutex as StdMutex;

use anyhow::{Result, anyhow};
use chrono::{NaiveDate, Utc};
use tokio::sync::Mutex;
use tr_congress::Db;
use tr_congress::house::IndexEntry;
use tr_congress::refresh::{RefreshConfig, Site, Source, paused_sources, refresh, refresh_minutes};
use tr_congress::roster::Roster;
use tr_congress::senate::{ReportKind, ReportRef};

const GOOD_PDF: &[u8] = include_bytes!("fixtures/house/8221321.pdf");
const GOOD_PTR: &str = include_str!("fixtures/senate/ptr_stocks.html");

/// House index of `count` PTRs; doc "bad" (if present) is not a PDF. Senate PTRs fail in transport.
struct Probe {
    house: usize,
    bad_house_doc: bool,
    senate: usize,
    senate_transport_fails: bool,
    seen: StdMutex<Vec<String>>,
}

impl Probe {
    fn hits(&self, prefix: &str) -> usize {
        self.seen.lock().unwrap().iter().filter(|s| s.starts_with(prefix)).count()
    }
}

impl Source for Probe {
    async fn roster_for(&self, site: Site) -> Result<Roster> {
        self.seen.lock().unwrap().push(format!("roster:{site:?}"));
        Ok(Roster::default())
    }

    async fn house_index(&self, year: i32) -> Result<Vec<IndexEntry>> {
        self.seen.lock().unwrap().push(format!("index:{year}"));
        let today = Utc::now().date_naive();
        let mut ids: Vec<String> = (0..self.house).map(|i| i.to_string()).collect();
        if self.bad_house_doc {
            ids.push("bad".into());
        }
        Ok(ids
            .into_iter()
            .map(|doc_id| IndexEntry {
                doc_id,
                prefix: String::new(),
                first: "Test".into(),
                last: "House".into(),
                suffix: String::new(),
                filing_type: "P".into(),
                state_district: "CA01".into(),
                year,
                filing_date: Some(today),
            })
            .filter(|_| chrono::Datelike::year(&today) == year)
            .collect())
    }

    async fn house_pdf(&self, entry: &IndexEntry) -> Result<Vec<u8>> {
        self.seen.lock().unwrap().push(format!("pdf:{}", entry.doc_id));
        if entry.doc_id == "bad" {
            // e.g. a malformed filing, or an HTML page served with 200 for a PDF URL
            return Ok(b"%PDF-1.4 truncated garbage".to_vec());
        }
        Ok(GOOD_PDF.to_vec())
    }

    async fn senate_reports(&self, _since: Option<NaiveDate>) -> Result<Vec<ReportRef>> {
        self.seen.lock().unwrap().push("search".into());
        Ok((0..self.senate)
            .map(|i| ReportRef {
                id: i.to_string(),
                kind: ReportKind::Electronic,
                first: "Test".into(),
                last: "Senator".into(),
                office: String::new(),
                title: "PTR".into(),
                url: format!("https://efdsearch.senate.gov/{i}"),
                date_received: Some(Utc::now().date_naive()),
            })
            .collect())
    }

    async fn senate_ptr(&self, r: &ReportRef) -> Result<String> {
        self.seen.lock().unwrap().push(format!("ptr:{}", r.id));
        if self.senate_transport_fails {
            // What a WAF that drops/resets connections looks like to reqwest.
            return Err(anyhow!("error sending request: connection reset by peer"));
        }
        Ok(GOOD_PTR.into())
    }
}

fn probe() -> Probe {
    Probe { house: 2, bad_house_doc: false, senate: 0, senate_transport_fails: false, seen: Default::default() }
}

/// One permanently unparseable filing in the window pushes a report error every run, so the
/// chamber's discovery date never advances: the window (and House years / Senate search pages)
/// grows without bound and the bad document is re-downloaded on every scheduled run.
#[tokio::test]
async fn parse_error_filing_does_not_freeze_discovery_window() {
    let db = Mutex::new(Db::open_in_memory().unwrap());
    let src = Probe { bad_house_doc: true, ..probe() };
    for _ in 0..3 {
        let cfg = RefreshConfig::incremental(&*db.lock().await);
        refresh(&src, &db, &cfg).await.unwrap();
    }
    // Immediate runs must not retry a stored parse error within the daily budget.
    assert_eq!(src.hits("pdf:bad"), 1);
    let today = Utc::now().date_naive().to_string();
    assert_eq!(
        db.lock().await.meta("last_refresh_date_house").unwrap(),
        Some(today),
        "House discovery date must advance once every reachable filing has been stored"
    );
}

/// Refusals stop a site, but a block that manifests as dropped/reset connections does not: the
/// whole PTR queue (up to 200) is still attempted, one new connection per second.
#[tokio::test]
async fn consecutive_transport_failures_stop_the_queue() {
    let db = Mutex::new(Db::open_in_memory().unwrap());
    let src = Probe { house: 0, senate: 200, senate_transport_fails: true, ..probe() };
    let cfg = RefreshConfig::incremental(&*db.lock().await);
    let r = refresh(&src, &db, &cfg).await.unwrap();
    assert_eq!(r.errors.len(), src.hits("ptr:"));
    assert!(src.hits("ptr:") <= 5, "kept requesting after {} consecutive transport failures", src.hits("ptr:"));
}

/// The data layer treats TR_PROVIDER case-insensitively; the Congress switch must agree.
#[test]
fn sample_provider_is_case_insensitive_like_the_data_layer() {
    assert_eq!(refresh_minutes(Some("SAMPLE"), None), 0);
    assert_eq!(refresh_minutes(Some("Sample"), None), 0);
}

/// A huge Retry-After saturates to i64::MAX: the site is paused forever and the status line
/// claims it was paused until the Unix epoch.
#[test]
fn huge_retry_after_has_sane_pause_text() {
    let db = Db::open_in_memory().unwrap();
    let now = Utc::now().timestamp();
    db.refuse_source("senate.gov", now, Some(i64::MAX)).unwrap();
    let paused = paused_sources(&db).unwrap();
    assert_eq!(paused.len(), 1);
    assert!(!paused[0].contains("1970") && !paused[0].contains("1969"), "{}", paused[0]);
}

/// Sanity: a successful run advances the House date and only fetches each PDF once.
#[tokio::test]
async fn clean_runs_advance_date_and_do_not_refetch() {
    let db = Mutex::new(Db::open_in_memory().unwrap());
    let src = probe();
    for _ in 0..2 {
        let cfg = RefreshConfig::incremental(&*db.lock().await);
        let r = refresh(&src, &db, &cfg).await.unwrap();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
    }
    assert_eq!(src.hits("pdf:"), 2);
    assert_eq!(db.lock().await.meta("last_refresh_date_house").unwrap(), Some(Utc::now().date_naive().to_string()));
}
