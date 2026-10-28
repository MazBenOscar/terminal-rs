use super::*;
use crate::{
    ParsedTrade,
    db::{FilingRecord, FilingStatus},
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

const FIXTURE: &str = include_str!("../tests/fixtures/bargo/trades_sample.json");
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data/bargo").join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn guard(&self, server: &Server, offline: bool) -> tr_net::Guard {
        tr_net::Guard::for_fixture(
            tr_net::Config {
                ledger: self.0.join("ledger.sqlite3"),
                data_dir: Some(self.0.join(format!("instance-{}", NEXT.fetch_add(1, Ordering::Relaxed)))),
                allow_external: !offline,
                max_wait: Duration::ZERO,
            },
            tr_net::Settings::default(),
            server.url.parse().unwrap(),
        )
        .unwrap()
    }
    fn age(&self, key: &str, seconds: i64) {
        rusqlite::Connection::open(self.0.join("ledger.sqlite3"))
            .unwrap()
            .execute("UPDATE slots SET at=at-?2,until=until-?2 WHERE key=?1", params![key, seconds * 1000])
            .unwrap();
    }
    fn unpace(&self) {
        rusqlite::Connection::open(self.0.join("ledger.sqlite3"))
            .unwrap()
            .execute("UPDATE last_request SET at=0", [])
            .unwrap();
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Server {
    url: String,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn new(body: &str, headers: &str) -> Self {
        Self::with_status(200, body, headers)
    }
    fn with_status(status: u16, body: &str, headers: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (h, s) = (hits.clone(), stop.clone());
        let response = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        );
        let worker = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                        let mut bytes = Vec::new();
                        let mut chunk = [0; 2048];
                        while !bytes.ends_with(b"\r\n\r\n") {
                            match stream.read(&mut chunk) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                            }
                        }
                        let request = String::from_utf8_lossy(&bytes);
                        assert!(!request.contains("senate.gov"));
                        assert!(request.starts_with("GET /free-apis/congress/v1/trades?"));
                        assert!(request.contains("chamber=senate"));
                        assert!(!request.lines().next().unwrap().contains("secret"));
                        assert!(!request.contains("token="));
                        h.fetch_add(1, Ordering::SeqCst);
                        stream.write_all(response.as_bytes()).unwrap();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("fixture listener: {e}"),
                }
            }
        });
        Self { url, hits, stop, worker: Some(worker) }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let joined = self.worker.take().unwrap().join();
        if !std::thread::panicking() {
            joined.unwrap();
        }
    }
}
fn page() -> Page {
    serde_json::from_str(FIXTURE).unwrap()
}
fn today() -> NaiveDate {
    "2026-10-27".parse().unwrap()
}

#[test]
fn real_fixture_roundtrip_source_amount_owner_and_repeat_dedupe() {
    let page = page();
    assert_eq!(page.trades.len(), 5);
    let mut db = Db::open_in_memory().unwrap();
    assert!(ingest(&mut db, &page.trades, true, today()).unwrap().is_empty());
    let first = db.latest_trades(100).unwrap();
    assert_eq!(first.len(), 5);
    for trade in &first {
        assert_eq!(trade.source, "Bargo");
        assert!(trade.owner.is_none());
        assert_eq!(trade.party, Party::Unknown);
        assert_eq!(trade.amount_range.as_deref(), Some("$1,001 - $15,000"));
        assert_eq!(trade.source_url, CREDIT_URL);
        assert!(!trade.source_url.contains("senate.gov"));
    }
    ingest(&mut db, &page.trades, false, today()).unwrap();
    assert_eq!(db.latest_trades(100).unwrap(), first);
    assert_eq!(page.trades[0].id().unwrap().len(), 46);
}

#[test]
fn efd_dedupe_keeps_roster_party_and_marks_seen() {
    let mut db = Db::open_in_memory().unwrap();
    let row = &page().trades[0];
    let pid = db
        .upsert_politician(&NewPolitician {
            name: "Boozman, John".into(),
            chamber: Chamber::Senate,
            party: Party::Republican,
            state: Some("AR".into()),
            district: None,
            bioguide: Some("B001236".into()),
        })
        .unwrap();
    db.insert_filing(
        &FilingRecord {
            id: "senate:old-efd".into(),
            chamber: Chamber::Senate,
            politician_id: pid,
            filed_date: row.disclosure_date.unwrap(),
            url: "".into(),
            paper: false,
            status: FilingStatus::Parsed,
            error: None,
            is_amendment: false,
        },
        &[ParsedTrade {
            row: 0,
            owner: Some("Spouse".into()),
            ticker: row.ticker.clone(),
            asset_name: row.asset.clone().unwrap(),
            asset_type: None,
            tx_type: TxType::SalePartial,
            tx_date: row.transaction_date.unwrap(),
            notification_date: None,
            amount: AmountRange::parse(row.amount_range.as_deref().unwrap()),
            comment: None,
        }],
    )
    .unwrap();
    db.set_tracked(pid, true).unwrap();
    for _ in 0..2 {
        ingest(&mut db, &page().trades, false, today()).unwrap();
    }
    assert_eq!(db.stats().unwrap().trades, 5);
    let trades = db.latest_trades(100).unwrap();
    assert!(trades.iter().all(|t| t.politician_id == pid && t.party == Party::Republican));
    assert!(trades.iter().any(|t| t.filing_id == "senate:old-efd" && t.owner.as_deref() == Some("Spouse")));
    assert_eq!(db.conn().query_row("SELECT count(*) FROM bargo_seen", [], |r| r.get::<_, i64>(0)).unwrap(), 5);
    assert!(db.alerts(false, 100).unwrap().is_empty());
}

#[test]
fn alert_freshness_no_initial_backlog_future_or_duplicate_alerts() {
    for (initial, age, expected) in [(true, 0, 0), (false, 0, 1), (false, 2, 1), (false, 3, 0), (false, -1, 0)] {
        let mut db = Db::open_in_memory().unwrap();
        let pid = db
            .upsert_politician(&NewPolitician {
                name: "John Boozman".into(),
                chamber: Chamber::Senate,
                party: Party::Unknown,
                state: Some("AR".into()),
                district: None,
                bioguide: None,
            })
            .unwrap();
        db.set_tracked(pid, true).unwrap();
        let mut row = page().trades.remove(0);
        row.disclosure_date = Some(today() - chrono::Duration::days(age));
        assert_eq!(ingest(&mut db, &[row.clone()], initial, today()).unwrap().len(), expected);
        assert!(ingest(&mut db, &[row], false, today()).unwrap().is_empty());
        assert_eq!(db.alerts(false, 100).unwrap().len(), expected);
    }
}

#[test]
fn key_header_only_sensitive_invalid_header_fails_closed() {
    let request = request("trades", Some("fixture-secret")).query(&[("chamber", "senate")]).build().unwrap();
    assert_eq!(request.headers()["X-Api-Key"], "fixture-secret");
    assert!(request.headers()["X-Api-Key"].is_sensitive());
    assert!(!request.url().as_str().contains("secret"));
    assert!(!request.url().as_str().contains("token"));
    assert!(!request.headers().contains_key("Authorization"));
    assert!(super::request("health", Some("bad\nsecret")).build().is_err());
    assert!(!super::request("health", None).build().unwrap().headers().contains_key("X-Api-Key"));
}

#[test]
fn sweep_and_poll_slots_shared_by_two_guards_fresh_store_cannot_add_poll() {
    let temp = Temp::new();
    let server = Server::new(FIXTURE, "");
    let first = temp.guard(&server, false);
    let second = temp.guard(&server, false);
    assert_eq!(schedule(&first, true).unwrap(), Some(Run::Sweep));
    assert_eq!(schedule(&second, true).unwrap(), None);
    assert_eq!(schedule(&second, false).unwrap(), None);
    temp.age("bargo:poll", 10801);
    assert_eq!(schedule(&second, true).unwrap(), None);
    assert_eq!(schedule(&second, false).unwrap(), Some(Run::Poll));
    assert_eq!(schedule(&first, false).unwrap(), None);
    temp.age("bargo:sweep", 86401);
    assert_eq!(schedule(&first, false).unwrap(), Some(Run::Sweep));
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn sync_first_sweep_only_then_three_hour_poll_no_portal_and_offline_zero() {
    let temp = Temp::new();
    let server = Server::new(FIXTURE, "X-RateLimit-Remaining: 25\r\nX-RateLimit-Rows-Remaining: 95\r\n");
    let first = temp.guard(&server, false);
    let second = temp.guard(&server, false);
    let db = Mutex::new(Db::open_in_memory().unwrap());
    sync(&first, &db, None).await.unwrap();
    sync(&second, &db, None).await.unwrap();
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    assert_eq!(db.lock().await.stats().unwrap().trades, 5);
    temp.age("bargo:poll", 10801);
    temp.unpace();
    sync(&second, &db, None).await.unwrap();
    assert_eq!(server.hits.load(Ordering::SeqCst), 2);
    assert_eq!(db.lock().await.stats().unwrap().trades, 5);
    let temp = Temp::new();
    let offline = temp.guard(&server, true);
    let before = server.hits.load(Ordering::SeqCst);
    let fresh = Mutex::new(Db::open_in_memory().unwrap());
    let error = sync(&offline, &fresh, Some("fixture-secret")).await.unwrap_err();
    assert!(error.to_string().contains("Offline"));
    assert_eq!(server.hits.load(Ordering::SeqCst), before);
    assert!(fresh.lock().await.meta("bargo_initialized").unwrap().is_none());
}

#[tokio::test]
async fn full_sweep_page_low_headers_stops_before_another_request() {
    for keyed in [true, false] {
        let temp = Temp::new();
        let mut fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        fixture["trades"] = serde_json::json!(vec![fixture["trades"][0].clone(); if keyed { 250 } else { 100 }]);
        let server = Server::new(&fixture.to_string(), "X-RateLimit-Remaining: 4\r\nX-RateLimit-Rows-Remaining: 0\r\n");
        let guard = temp.guard(&server, false);
        let db = Mutex::new(Db::open_in_memory().unwrap());
        sync(&guard, &db, if keyed { Some("fixture-secret") } else { None }).await.unwrap();
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        assert_eq!(db.lock().await.stats().unwrap().trades, 1);
    }
}

#[tokio::test]
async fn alerts_survive_a_later_page_refusal_without_retrying() {
    let temp = Temp::new();
    let mut fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
    fixture["trades"][0]["disclosure_date"] = serde_json::json!(Utc::now().date_naive().to_string());
    fixture["trades"] = serde_json::json!(vec![fixture["trades"][0].clone(); 250]);
    let server = Server::new(&fixture.to_string(), "");
    let guard = temp.guard(&server, false);
    let db = Db::open_in_memory().unwrap();
    db.set_meta("bargo_initialized", "1").unwrap();
    db.set_meta("bargo_initial_complete", "1").unwrap();
    let pid = db
        .upsert_politician(&NewPolitician {
            name: "John Boozman".into(),
            chamber: Chamber::Senate,
            party: Party::Unknown,
            state: Some("AR".into()),
            district: None,
            bioguide: None,
        })
        .unwrap();
    db.set_tracked(pid, true).unwrap();
    let db = Mutex::new(db);
    // Fixture max_wait=0 deliberately refuses the next page's 30-second pacing wait.
    let report = sync(&guard, &db, Some("fixture-secret")).await.unwrap();
    assert_eq!(report.alerts.len(), 1);
    assert!(report.problem.unwrap().contains("busy"));
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    assert_eq!(db.lock().await.alerts(false, 10).unwrap().len(), 1);
}

#[tokio::test]
async fn source_401_is_key_error_and_429_is_paused_without_retry() {
    for code in [401, 429] {
        let temp = Temp::new();
        let server = Server::with_status(code, "{\"error\":\"fixture-secret\"}", "");
        let guard = temp.guard(&server, false);
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let report = sync(&guard, &db, Some("fixture-secret")).await.unwrap();
        assert!(!report.problem.unwrap().contains("fixture-secret"));
        let line = status(&guard, None, true);
        if code == 401 {
            assert!(line.contains("key error"));
        }
        assert!(line.contains("paused until"));
        assert!(sync(&guard, &db, Some("fixture-secret")).await.is_err());
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn status_labels_updated_keyless_paused_and_off() {
    let temp = Temp::new();
    let server = Server::new(FIXTURE, "");
    let guard = temp.guard(&server, false);
    let line = status(&guard, Some(Utc::now().timestamp() - 7200), true);
    assert!(line.contains("Senate via Bargo · updated 2h ago"));
    assert!(status(&guard, None, false).contains("needs key for full history"));
    guard.configure(tr_net::Settings { bargo: false, ..Default::default() });
    assert_eq!(status(&guard, None, false), crate::refresh::SENATE_PAUSED);
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
}

#[test]
fn openapi_members_have_no_party_and_state_is_already_in_trades() {
    let spec: serde_json::Value = serde_json::from_str(include_str!("../tests/fixtures/bargo/openapi.json")).unwrap();
    let members = &spec["components"]["schemas"]["MemberSummary"]["properties"];
    assert!(members.get("party").is_none());
    assert!(members.get("state").is_some());
    assert!(spec["components"]["schemas"]["Trade"]["properties"].get("state").is_some());
    assert_eq!(CREDIT, "Senate data: Bargo (bargo.ai)");
    assert_eq!(reqwest::Url::parse(CREDIT_URL).unwrap().scheme(), "https");
}
