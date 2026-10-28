//! Local sockets only. No production transport or shared user ledger is constructed here.
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tr_edgar::{
    Company,
    scheduler::{Reply, Scheduler, Transport},
    store::Store,
};
use tr_net::{Refused, ResponseMetadata};
const NOW: i64 = 1_793_145_600; // 2026-10-27 UTC
// These tests isolate N2 metadata cadence. N3's XML request counts have dedicated tests.
fn metadata_only_apple() -> Vec<u8> {
    let mut json: serde_json::Value = serde_json::from_slice(include_bytes!("fixtures/submissions_aapl.json")).unwrap();
    for form in json["filings"]["recent"]["form"].as_array_mut().unwrap() {
        if matches!(form.as_str(), Some("4" | "4/A")) {
            *form = serde_json::json!("3");
        }
    }
    serde_json::to_vec(&json).unwrap()
}
struct Fake {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    task: Option<thread::JoinHandle<()>>,
    refused: Option<Refused>,
}
impl Fake {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (r, s) = (requests.clone(), stop.clone());
        let task = thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                        let mut bytes = vec![];
                        let mut buf = [0; 4096];
                        while !bytes.ends_with(b"\r\n\r\n") {
                            match stream.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => bytes.extend_from_slice(&buf[..n]),
                            }
                        }
                        let request = String::from_utf8_lossy(&bytes).into_owned();
                        if request.is_empty() {
                            continue;
                        }
                        let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                        let conditional = request.to_ascii_lowercase().contains("if-none-match: \"fixture\"");
                        r.lock().unwrap().push(request);
                        let body = if conditional {
                            vec![]
                        } else if path.starts_with("/files/") {
                            include_bytes!("fixtures/company_tickers.json").to_vec()
                        } else if path.starts_with("/cgi-bin/") {
                            include_bytes!("fixtures/current_8k.atom").to_vec()
                        } else if path.contains("CIK0000320193") {
                            metadata_only_apple()
                        } else {
                            let cik: u64 = path.split("CIK").nth(1).unwrap().trim_end_matches(".json").parse().unwrap();
                            serde_json::to_vec(&serde_json::json!({"cik":cik,"filings":{"recent":{
                                "accessionNumber":["0001104659-26-118875"],"filingDate":["2026-10-26"],"reportDate":["2026-10-26"],
                                "acceptanceDateTime":["2026-10-26T21:30:28Z"],"form":["8-K"],"items":["3.02,8.01"],
                                "primaryDocument":["filing.htm"],"primaryDocDescription":["Current report"]}}})).unwrap()
                        };
                        let status = if conditional { 304 } else { 200 };
                        let header = format!(
                            "HTTP/1.1 {status} Fixture\r\nConnection: close\r\nContent-Length: {}\r\nETag: \"fixture\"\r\nLast-Modified: Mon, 26 Oct 2026 22:00:00 GMT\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&body);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(2)),
                    Err(e) => panic!("local fake: {e}"),
                }
            }
        });
        Self { base, requests, stop, task: Some(task), refused: None }
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.task.take().unwrap().join();
    }
}
impl Transport for Fake {
    fn check(&self) -> Result<(), Refused> {
        self.refused.clone().map_or(Ok(()), Err)
    }
    async fn get(&self, url: &str, etag: Option<String>, modified: Option<String>) -> Result<Reply, Refused> {
        self.check()?;
        let original = reqwest::Url::parse(url).unwrap();
        assert!(matches!(original.host_str(), Some("www.sec.gov" | "data.sec.gov")));
        let local = format!(
            "{}{}{}",
            self.base,
            original.path(),
            original.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        assert_eq!(reqwest::Url::parse(&local).unwrap().host_str(), Some("127.0.0.1"));
        let mut request =
            reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap().get(local);
        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag);
        }
        if let Some(modified) = modified {
            request = request.header("If-Modified-Since", modified);
        }
        let response = request.send().await.unwrap();
        let metadata = ResponseMetadata {
            status: response.status().as_u16(),
            etag: response.headers().get("etag").map(|v| v.to_str().unwrap().into()),
            last_modified: response.headers().get("last-modified").map(|v| v.to_str().unwrap().into()),
        };
        Ok(Reply { metadata, body: response.bytes().await.unwrap().to_vec() })
    }
}
fn store() -> Store {
    let mut db = Store::memory().unwrap();
    db.replace_companies(&[
        Company { cik: 1526243, ticker: "PPTA".into(), title: "Perpetua".into() },
        Company { cik: 320193, ticker: "AAPL".into(), title: "Apple".into() },
        Company { cik: 42, ticker: "OTHER".into(), title: "Other".into() },
    ])
    .unwrap();
    db.set("map_checked", NOW).unwrap();
    db
}
#[tokio::test]
async fn two_feeds_one_enrichment_per_new_watched_cik_and_304_preserves_rows() {
    let fake = Fake::new();
    let requests = fake.requests.clone();
    let mut scheduler = Scheduler::new(store(), fake);
    let symbols = vec!["PPTA US Equity".into()];
    scheduler.tick(NOW, false, &symbols).await;
    assert!(requests.lock().unwrap().is_empty());
    scheduler.tick(NOW, true, &symbols).await;
    let paths: Vec<_> = requests.lock().unwrap().iter().map(|s| s.lines().next().unwrap().to_owned()).collect();
    assert_eq!(paths.iter().filter(|s| s.contains("/cgi-bin/")).count(), 2);
    assert_eq!(paths.iter().filter(|s| s.contains("/submissions/")).count(), 1);
    assert_eq!(paths.len(), 3);
    assert!(paths[2].contains("CIK0001526243.json"));
    let rows = scheduler.store.filings().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].primary_doc, "filing.htm");
    scheduler.tick(NOW + 1, true, &symbols).await;
    assert_eq!(requests.lock().unwrap().len(), 3);
    scheduler.tick(NOW + 600, true, &symbols).await;
    assert_eq!(requests.lock().unwrap().len(), 5);
    assert_eq!(scheduler.store.filings().unwrap(), rows);
    assert!(requests.lock().unwrap()[3].to_ascii_lowercase().contains("if-modified-since:"));
    assert!(requests.lock().unwrap()[3].to_ascii_lowercase().contains("if-none-match: \"fixture\""));
}
#[tokio::test]
async fn daily_sweep_spreads_first_run_no_history_pages_and_no_catchup_burst() {
    let fake = Fake::new();
    let requests = fake.requests.clone();
    let mut db = store();
    db.set("feed_checked", NOW).unwrap();
    // Suppress feed traffic throughout this test to isolate sweep slots.
    db.replace_companies(&[
        Company { cik: 320193, ticker: "AAPL".into(), title: "Apple".into() },
        Company { cik: 42, ticker: "OTHER".into(), title: "Other".into() },
    ])
    .unwrap();
    let mut scheduler = Scheduler::new(db, fake);
    let symbols = vec!["AAPL US Equity".into(), "OTHER US Equity".into()];
    scheduler.tick(NOW, true, &symbols).await;
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert_eq!(scheduler.store.number("next_sweep").unwrap(), NOW + 43200);
    // First imports run a minute apart (never a burst), not one 12 h sweep slot apart.
    scheduler.tick(NOW + 30, true, &symbols).await;
    assert_eq!(requests.lock().unwrap().len(), 1);
    scheduler.tick(NOW + 60, true, &symbols).await;
    assert_eq!(requests.lock().unwrap().len(), 2);
    let apple: Vec<_> = scheduler.store.filings().unwrap().into_iter().filter(|f| f.cik == 320193).collect();
    assert!(!apple.is_empty());
    assert!(apple.iter().all(|f| f.filed >= NOW + 60 - 30 * 86400));
    // Both imported: the regular sweep waits a full day per company.
    scheduler.store.set("feed_checked", NOW + 43200).unwrap();
    scheduler.tick(NOW + 43260, true, &symbols).await;
    assert_eq!(requests.lock().unwrap().len(), 2);
    // Restart-like long gap: still one sweep, no catch-up for every missed day.
    scheduler.store.set("map_checked", NOW + 5 * 86400).unwrap();
    scheduler.store.set("feed_checked", NOW + 5 * 86400).unwrap();
    scheduler.tick(NOW + 5 * 86400, true, &symbols).await;
    assert_eq!(requests.lock().unwrap().len(), 3);
    assert!(requests.lock().unwrap().iter().all(|r| r.contains("/submissions/CIK") && !r.contains("-submissions-")));
}
#[tokio::test]
async fn map_is_conditional_daily_and_refusals_make_zero_requests() {
    for refusal in [
        Refused::NeedsContact,
        Refused::Offline,
        Refused::Paused { until: NOW + 86400 },
        Refused::Budget,
        Refused::Disabled,
    ] {
        let mut fake = Fake::new();
        fake.refused = Some(refusal.clone());
        let requests = fake.requests.clone();
        let mut scheduler = Scheduler::new(Store::memory().unwrap(), fake);
        for now in [NOW, NOW + 60, NOW + 600] {
            scheduler.tick(now, true, &["AAPL US Equity".into()]).await;
        }
        assert!(requests.lock().unwrap().is_empty());
        assert_eq!(scheduler.status, tr_edgar::scheduler::refusal_label(&refusal));
    }
    let fake = Fake::new();
    let requests = fake.requests.clone();
    let mut scheduler = Scheduler::new(Store::memory().unwrap(), fake);
    let symbols = vec!["NOTAMAPPEDTICKER".into()];
    scheduler.tick(NOW, true, &symbols).await;
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert_eq!(scheduler.store.companies().unwrap().len(), 300);
    scheduler.tick(NOW + 86399, true, &symbols).await;
    assert_eq!(requests.lock().unwrap().len(), 1);
    scheduler.tick(NOW + 86400, true, &symbols).await;
    assert_eq!(requests.lock().unwrap().len(), 2);
    assert_eq!(scheduler.store.companies().unwrap().len(), 300);
    assert!(requests.lock().unwrap()[1].to_ascii_lowercase().contains("if-none-match:"));
}
#[tokio::test]
async fn next_daily_304_reuses_recent_json_for_ninety_days_without_history_requests() {
    let fake = Fake::new();
    let requests = fake.requests.clone();
    let mut scheduler = Scheduler::new(store(), fake);
    let symbols = vec!["AAPL US Equity".into()];
    scheduler.store.set("feed_checked", NOW).unwrap();
    scheduler.tick(NOW, true, &symbols).await;
    let first = scheduler.store.filings().unwrap();
    assert!(!first.is_empty());
    assert!(first.iter().all(|f| f.filed >= NOW - 30 * 86400));
    scheduler.store.set("map_checked", NOW + 86400).unwrap();
    scheduler.store.set("feed_checked", NOW + 86400).unwrap();
    scheduler.tick(NOW + 86400, true, &symbols).await;
    let second = scheduler.store.filings().unwrap();
    assert!(second.len() > first.len());
    assert!(second.iter().all(|f| f.filed >= NOW + 86400 - 90 * 86400));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| r.contains("/submissions/CIK0000320193.json")));
    assert!(requests[1].to_ascii_lowercase().contains("if-none-match:"));
}

#[tokio::test]
async fn disk_cursors_and_validators_survive_scheduler_restart() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data/edgar");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!(
        "restart-{}-{}.sqlite3",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    let fake = Fake::new();
    let requests = fake.requests.clone();
    let mut scheduler = Scheduler::new(Store::open(&path).unwrap(), fake);
    scheduler.tick(NOW, true, &["NOTMAPPED".into()]).await;
    assert_eq!(requests.lock().unwrap().len(), 1);
    drop(scheduler);
    let fake = Fake::new();
    let requests = fake.requests.clone();
    let mut scheduler = Scheduler::new(Store::open(&path).unwrap(), fake);
    scheduler.tick(NOW + 60, true, &["NOTMAPPED".into()]).await;
    assert!(requests.lock().unwrap().is_empty());
    assert_eq!(scheduler.store.companies().unwrap().len(), 300);
    scheduler.tick(NOW + 86400, true, &["NOTMAPPED".into()]).await;
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert!(requests.lock().unwrap()[0].to_ascii_lowercase().contains("if-none-match:"));
    drop(scheduler);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn refusal_on_first_fetch_stops_cycle_without_provider_fallback() {
    struct Refuse {
        calls: std::sync::atomic::AtomicUsize,
    }
    impl Transport for Refuse {
        fn check(&self) -> Result<(), Refused> {
            Ok(())
        }
        async fn get(&self, _: &str, _: Option<String>, _: Option<String>) -> Result<Reply, Refused> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(Refused::Paused { until: NOW + 21600 })
        }
    }
    let transport = Refuse { calls: Default::default() };
    let mut scheduler = Scheduler::new(store(), transport);
    scheduler.tick(NOW, true, &["PPTA".into()]).await;
    assert!(scheduler.status.starts_with("Paused until"));
    assert_eq!(scheduler.store.number("feed_checked").unwrap(), NOW);
}

/// Serves AAPL normally; CIK 42 always answers with a non-pausing HTTP error.
struct Failing {
    requests: Arc<Mutex<Vec<String>>>,
}
impl Transport for Failing {
    fn check(&self) -> Result<(), Refused> {
        Ok(())
    }
    async fn get(&self, url: &str, _: Option<String>, _: Option<String>) -> Result<Reply, Refused> {
        self.requests.lock().unwrap().push(url.to_owned());
        if url.contains("CIK0000000042") {
            return Err(Refused::Http(404));
        }
        assert!(url.contains("CIK0000320193"), "unexpected request {url}");
        Ok(Reply { body: metadata_only_apple(), metadata: ResponseMetadata { status: 200, ..Default::default() } })
    }
}
fn quiet_store() -> Store {
    let db = store();
    // Feed and map cadences in the future: only enrichment and sweep traffic remains.
    db.set("feed_checked", NOW + 30 * 86400).unwrap();
    db.set("map_checked", NOW + 30 * 86400).unwrap();
    db
}

#[tokio::test]
async fn review_failing_cik_starves_daily_sweep() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut scheduler = Scheduler::new(quiet_store(), Failing { requests: requests.clone() });
    let symbols = vec!["OTHER US Equity".into(), "AAPL US Equity".into()];
    for minute in 0..2 * 24 * 60 {
        scheduler.tick(NOW + minute * 60, true, &symbols).await;
    }
    let requests = requests.lock().unwrap();
    let apple = requests.iter().filter(|u| u.contains("CIK0000320193")).count();
    assert!(apple >= 1, "AAPL never swept in two days; requests: {requests:?}");
}

#[tokio::test]
async fn review_failing_pending_cik_retried_every_cycle() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let db = quiet_store();
    db.set("pending:42", 1).unwrap();
    // Sweep slot already used today, so only the pending path can request.
    db.set("next_sweep", NOW + 2 * 86400).unwrap();
    let mut scheduler = Scheduler::new(db, Failing { requests: requests.clone() });
    let symbols = vec!["OTHER US Equity".into()];
    for minute in 0..24 * 60 {
        scheduler.tick(NOW + minute * 60, true, &symbols).await;
    }
    let failing = requests.lock().unwrap().iter().filter(|u| u.contains("CIK0000000042")).count();
    assert!(failing <= 3, "{failing} requests in one day for a CIK that always fails");
}

#[tokio::test]
async fn review_added_symbol_waits_for_stale_sweep_slot() {
    let fake = Fake::new();
    let requests = fake.requests.clone();
    let mut scheduler = Scheduler::new(quiet_store(), fake);
    scheduler.tick(NOW, true, &["AAPL US Equity".into()]).await;
    assert_eq!(requests.lock().unwrap().len(), 1);
    // The user adds a second symbol a minute later. With two CIKs a slot is every 12 h.
    let symbols = vec!["AAPL US Equity".into(), "PPTA US Equity".into()];
    for minute in 1..=12 * 60 + 2 {
        scheduler.tick(NOW + minute * 60, true, &symbols).await;
    }
    let ppta = requests.lock().unwrap().iter().filter(|r| r.contains("CIK0001526243")).count();
    assert_eq!(ppta, 1, "newly watched PPTA had no first import within one 12 h slot");
}

#[tokio::test]
async fn pending_failures_obey_one_six_twenty_four_hour_backoffs_and_drop() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let db = quiet_store();
    db.set("pending:42", 1).unwrap();
    db.set("next_sweep", NOW + 10 * 86400).unwrap();
    let mut scheduler = Scheduler::new(db, Failing { requests: requests.clone() });
    for (seconds, count) in [(0, 1), (600, 1), (3599, 1), (3600, 2), (25199, 2), (25200, 3), (86400, 3), (111600, 3)] {
        scheduler.tick(NOW + seconds, true, &["OTHER".into()]).await;
        assert_eq!(requests.lock().unwrap().len(), count, "at {seconds}");
    }
    assert_eq!(scheduler.store.number("failures:42").unwrap(), 3);
    assert_eq!(scheduler.store.number("retry:42").unwrap(), NOW + 25200 + 86400);
    assert_eq!(scheduler.store.get("pending:42").unwrap(), None);
    assert_eq!(scheduler.store.number("pending_dropped:42").unwrap(), 1);
}

#[tokio::test]
async fn three_daily_failures_skip_seven_days_and_explain_the_company() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut scheduler = Scheduler::new(quiet_store(), Failing { requests: requests.clone() });
    for day in 0..9 {
        scheduler.tick(NOW + day * 86400, true, &["OTHER".into()]).await;
    }
    assert_eq!(requests.lock().unwrap().len(), 3);
    assert!(scheduler.status.contains("1 company could not be read: CIK 42"), "{}", scheduler.status);
    assert_eq!(scheduler.store.number("skip:42").unwrap(), NOW + 9 * 86400);
    scheduler.tick(NOW + 9 * 86400, true, &["OTHER".into()]).await;
    assert_eq!(requests.lock().unwrap().len(), 4);
}

struct Scripted {
    replies: Mutex<std::collections::VecDeque<Reply>>,
    requests: Mutex<Vec<(String, Option<String>)>>,
    slots: Mutex<Vec<(String, Duration)>>,
    pending_allowed: bool,
}
impl Scripted {
    fn new(bodies: Vec<Vec<u8>>) -> Self {
        Self {
            replies: Mutex::new(
                bodies
                    .into_iter()
                    .map(|body| Reply {
                        body,
                        metadata: ResponseMetadata {
                            status: 200,
                            etag: Some("bad-or-good".into()),
                            last_modified: Some("fixture".into()),
                        },
                    })
                    .collect(),
            ),
            requests: Mutex::new(vec![]),
            slots: Mutex::new(vec![]),
            pending_allowed: true,
        }
    }
}
impl Transport for Scripted {
    fn check(&self) -> Result<(), Refused> {
        Ok(())
    }
    fn pending_allowed(&self) -> Result<bool, Refused> {
        Ok(self.pending_allowed)
    }
    fn claim_slot(&self, key: &str, interval: Duration) -> Result<bool, Refused> {
        self.slots.lock().unwrap().push((key.into(), interval));
        Ok(true)
    }
    async fn get(&self, url: &str, etag: Option<String>, _: Option<String>) -> Result<Reply, Refused> {
        self.requests.lock().unwrap().push((url.into(), etag));
        Ok(self.replies.lock().unwrap().pop_front().expect("unexpected request"))
    }
}
#[tokio::test]
async fn parse_and_decode_failures_back_off_without_saving_validators_or_bodies() {
    for body in [b"{".to_vec(), vec![0xff]] {
        let db = quiet_store();
        db.set("pending:42", 1).unwrap();
        db.set("next_sweep", NOW + 86400).unwrap();
        let mut scheduler = Scheduler::new(db, Scripted::new(vec![body]));
        scheduler.tick(NOW, true, &["OTHER".into()]).await;
        scheduler.tick(NOW + 600, true, &["OTHER".into()]).await;
        assert_eq!(scheduler.store.number("attempt:42").unwrap(), NOW);
        assert_eq!(scheduler.store.number("failures:42").unwrap(), 1);
        assert_eq!(scheduler.store.number("retry:42").unwrap(), NOW + 3600);
        for key in ["etag:sub:42", "modified:sub:42", "body:42"] {
            assert_eq!(scheduler.store.get(key).unwrap(), None);
        }
    }
}

#[tokio::test]
async fn invalid_map_and_feed_bodies_never_save_their_validators() {
    let mut map = Scheduler::new(Store::memory().unwrap(), Scripted::new(vec![b"not json".to_vec()]));
    map.tick(NOW, true, &["OTHER".into()]).await;
    assert_eq!(map.store.get("etag:map").unwrap(), None);
    assert_eq!(map.store.get("modified:map").unwrap(), None);
    let db = quiet_store();
    db.set("feed_checked", 0).unwrap();
    let mut feed = Scheduler::new(db, Scripted::new(vec![b"not xml".to_vec()]));
    feed.tick(NOW, true, &["OTHER".into()]).await;
    assert_eq!(feed.store.get("etag:feed:8-K").unwrap(), None);
    assert_eq!(feed.store.get("modified:feed:8-K").unwrap(), None);
}

fn recent(acc: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"cik":42,"filings":{"recent":{
        "accessionNumber":[acc], "filingDate":["2026-10-26"], "form":["8-K"]
    }}}))
    .unwrap()
}
#[tokio::test]
async fn pending_waits_for_accession_not_merely_a_successful_submissions_response() {
    let expected = "0000000042-26-000002";
    for arrives in [false, true] {
        let db = quiet_store();
        db.set("pending:42", serde_json::to_string(&vec![expected]).unwrap()).unwrap();
        db.set("next_sweep", NOW + 86400).unwrap();
        let body = recent("0000000042-26-000001");
        let final_body = if arrives { recent(expected) } else { body.clone() };
        let mut scheduler = Scheduler::new(db, Scripted::new(vec![body.clone(), final_body, body]));
        scheduler.tick(NOW, true, &["OTHER".into()]).await;
        assert!(scheduler.store.get("pending:42").unwrap().is_some());
        scheduler.tick(NOW + 600, true, &["OTHER".into()]).await;
        assert_eq!(scheduler.store.get("pending:42").unwrap().is_none(), arrives);
        scheduler.tick(NOW + 1200, true, &["OTHER".into()]).await;
        assert!(scheduler.store.get("pending:42").unwrap().is_none());
        assert_eq!(scheduler.store.number("pending_dropped:42").unwrap(), i64::from(!arrives));
        assert_eq!(scheduler.store.get("body:42").unwrap(), None);
    }
}

#[tokio::test]
async fn added_companies_import_without_bursts_and_large_lists_stretch_the_sweep() {
    let fake = Fake::new();
    let requests = fake.requests.clone();
    let mut scheduler = Scheduler::new(quiet_store(), fake);
    scheduler.tick(NOW, true, &["AAPL".into()]).await;
    let symbols = vec!["AAPL".into(), "OTHER".into(), "PPTA".into()];
    for (seconds, count) in [(59, 1), (60, 2), (119, 2), (120, 3), (600, 3)] {
        scheduler.tick(NOW + seconds, true, &symbols).await;
        assert_eq!(requests.lock().unwrap().len(), count);
    }
    let mut db = Store::memory().unwrap();
    let companies: Vec<_> =
        (1..=601).map(|cik| Company { cik, ticker: format!("C{cik}"), title: String::new() }).collect();
    db.replace_companies(&companies).unwrap();
    db.set("map_checked", NOW).unwrap();
    db.set("feed_checked", NOW).unwrap();
    let symbols: Vec<_> = companies.iter().map(|c| c.ticker.clone()).collect();
    let mut scheduler = Scheduler::new(db, Scripted::new(vec![recent("0000000042-26-000001")]));
    // CIK 1's deliberately mismatched response still consumes and advances a sweep turn.
    scheduler.tick(NOW, true, &symbols).await;
    assert!(scheduler.status.contains("Daily refresh every 2 days for 601 companies"));
    assert!(scheduler.store.number("next_sweep").unwrap() >= NOW + 144);
}

#[tokio::test]
async fn feed_budget_reserve_stops_company_work_not_the_feeds() {
    let db = quiet_store();
    db.set("feed_checked", 0).unwrap();
    db.set("pending:42", 1).unwrap();
    let mut transport = Scripted::new(vec![include_bytes!("fixtures/current_8k.atom").to_vec(); 2]);
    transport.pending_allowed = false;
    let mut scheduler = Scheduler::new(db, transport);
    scheduler.tick(NOW, true, &["OTHER".into()]).await;
    assert_eq!(scheduler.store.number("attempt:42").unwrap(), 0);
    assert!(scheduler.status.contains("reserve the feed budget"));
    assert!(scheduler.store.get("etag:feed:8-K").unwrap().is_some());
    assert!(scheduler.store.get("etag:feed:4").unwrap().is_some());
}

// This transport uses a fixture clock for cadence but real shared ledger counters.
struct Counted(Scripted, tr_net::Guard);
impl Transport for Counted {
    fn check(&self) -> Result<(), Refused> {
        Ok(())
    }
    fn increment_counter(&self, key: &str, _: i64) -> Result<i64, Refused> {
        self.1.increment_counter(key)
    }
    fn clear_counter(&self, key: &str) -> Result<(), Refused> {
        self.1.clear_counter(key)
    }
    async fn get(&self, url: &str, etag: Option<String>, modified: Option<String>) -> Result<Reply, Refused> {
        self.0.get(url, etag, modified).await
    }
}
#[tokio::test]
async fn machine_retry_counters_survive_fresh_stores_and_guards() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/test-data/edgar/counters-{}-{}.sqlite3",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    let transport = |bodies| {
        Counted(
            Scripted::new(bodies),
            tr_net::Guard::new(
                tr_net::Config {
                    ledger: path.clone(),
                    data_dir: None,
                    allow_external: false,
                    max_wait: Duration::ZERO,
                },
                tr_net::Settings::default(),
            )
            .unwrap(),
        )
    };
    for (index, seconds) in [0, 3600, 25200].into_iter().enumerate() {
        let db = quiet_store();
        db.set("pending:42", 1).unwrap();
        db.set("next_sweep", NOW + 10 * 86400).unwrap();
        let mut scheduler = Scheduler::new(db, transport(vec![b"invalid json".to_vec()]));
        scheduler.tick(NOW + seconds, true, &["OTHER".into()]).await;
        assert_eq!(scheduler.store.number("failures:42").unwrap(), index as i64 + 1);
        assert_eq!(scheduler.store.number("retry:42").unwrap(), NOW + seconds + [3600, 21600, 86400][index]);
    }
    let db = quiet_store();
    db.set("pending:42", 1).unwrap();
    db.set("next_sweep", NOW + 10 * 86400).unwrap();
    let mut fourth = Scheduler::new(db, transport(vec![]));
    fourth.tick(NOW + 111600, true, &["OTHER".into()]).await;
    assert_eq!(fourth.store.number("attempt:42").unwrap(), 0);
    assert_eq!(fourth.store.number("pending_dropped:42").unwrap(), 1);
    // Consecutive daily failures also accumulate even if each daily turn is a fresh instance.
    for day in 2..=4 {
        let mut scheduler = Scheduler::new(quiet_store(), transport(vec![b"invalid json".to_vec()]));
        scheduler.tick(NOW + day * 86400, true, &["OTHER".into()]).await;
        if day == 4 {
            assert!(scheduler.status.contains("1 company could not be read: CIK 42"));
        }
    }
}

struct SharedFake(Arc<Fake>, tr_net::Guard);
impl Transport for SharedFake {
    fn increment_counter(&self, key: &str, _: i64) -> Result<i64, Refused> {
        self.1.increment_counter(key)
    }
    fn clear_counter(&self, key: &str) -> Result<(), Refused> {
        self.1.clear_counter(key)
    }
    fn defer_slot(&self, key: &str, interval: Duration) -> Result<(), Refused> {
        self.1.defer_slot(key, interval)
    }
    fn claim_slot(&self, key: &str, interval: Duration) -> Result<bool, Refused> {
        self.1.claim_slot(key, interval)
    }
    fn check(&self) -> Result<(), Refused> {
        self.0.check()
    }
    async fn get(&self, url: &str, etag: Option<String>, modified: Option<String>) -> Result<Reply, Refused> {
        self.0.get(url, etag, modified).await
    }
}

#[tokio::test]
async fn review_two_instances_double_feed_polling() {
    let fake = Arc::new(Fake::new());
    let requests = fake.requests.clone();
    // Two instances (e.g. the main app and a TR_ALLOW_EXTERNAL=1 throwaway) with separate data dirs.
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/test-data/edgar/shared-{}-{}.sqlite3",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    let guard = || {
        tr_net::Guard::new(
            tr_net::Config { ledger: path.clone(), data_dir: None, allow_external: false, max_wait: Duration::ZERO },
            tr_net::Settings::default(),
        )
        .unwrap()
    };
    let mut first = Scheduler::new(store(), SharedFake(fake.clone(), guard()));
    let mut second = Scheduler::new(store(), SharedFake(fake.clone(), guard()));
    let symbols = vec!["OTHER US Equity".into()];
    first.tick(NOW, true, &symbols).await;
    second.tick(NOW + 30, true, &symbols).await;
    let feeds = requests.lock().unwrap().iter().filter(|r| r.contains("/cgi-bin/")).count();
    assert!(feeds <= 2, "{feeds} feed requests within one 10-minute cycle across instances");
}

#[tokio::test]
async fn load_once_is_one_request_and_two_guards_share_the_24_hour_slot() {
    let fake = Arc::new(Fake::new());
    let requests = fake.requests.clone();
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/test-data/edgar/once-{}-{}.sqlite3",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    let guard = || {
        tr_net::Guard::new(
            tr_net::Config { ledger: path.clone(), data_dir: None, allow_external: false, max_wait: Duration::ZERO },
            tr_net::Settings::default(),
        )
        .unwrap()
    };
    let mut first = Scheduler::new(store(), SharedFake(fake.clone(), guard()));
    let mut second = Scheduler::new(store(), SharedFake(fake, guard()));
    first.load_once(NOW, "AAPL US Equity").await;
    assert!(first.status.contains("Loaded once"), "{}", first.status);
    let rows = first.store.filings().unwrap();
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|f| f.transient_symbol == "AAPL" && f.filed >= NOW - 30 * 86400));
    assert!(rows.iter().all(|f| !f.insider_queued));
    assert!(first.store.insider_backlog().unwrap().is_empty());
    for key in ["initialized:320193", "attempt:320193", "window:320193", "watched", "feed_checked"] {
        assert_eq!(first.store.number(key).unwrap(), 0, "one-shot must not start tracking: {key}");
    }
    first.load_once(NOW + 1, "AAPL US Equity").await;
    second.load_once(NOW + 2, "AAPL US Equity").await;
    assert!(first.status.contains("24 hours"));
    assert!(second.status.contains("24 hours"));
    assert!(second.store.filings().unwrap().is_empty());
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "no feeds, company map, history pages or Form 4 reads");
    assert!(requests[0].contains("/submissions/CIK0000320193.json"));
}

#[tokio::test]
async fn load_once_keeps_form4_metadata_without_queueing_document_reads() {
    let transport = Scripted::new(vec![include_bytes!("fixtures/submissions_aapl.json").to_vec()]);
    let mut scheduler = Scheduler::new(store(), transport);
    scheduler.load_once(NOW, "AAPL US Equity").await;
    assert!(scheduler.status.contains("Loaded once"), "{}", scheduler.status);
    let rows = scheduler.store.filings().unwrap();
    assert!(rows.iter().any(|f| f.form == "4"));
    assert!(rows.iter().all(|f| f.insiders.is_empty() && !f.insider_queued));
    assert!(scheduler.store.insider_backlog().unwrap().is_empty());
}

#[tokio::test]
async fn load_once_refuses_offline_contact_and_reserved_budget_without_requests() {
    for refusal in [Refused::Offline, Refused::NeedsContact] {
        let mut fake = Fake::new();
        fake.refused = Some(refusal);
        let requests = fake.requests.clone();
        let mut scheduler = Scheduler::new(store(), fake);
        scheduler.load_once(NOW, "AAPL US Equity").await;
        assert!(requests.lock().unwrap().is_empty());
        assert!(scheduler.status.contains("Load once:"));
        assert!(!scheduler.status.contains("Loaded once"));
    }
    let mut transport = Scripted::new(vec![]);
    transport.pending_allowed = false;
    let mut scheduler = Scheduler::new(store(), transport);
    scheduler.load_once(NOW, "AAPL US Equity").await;
    assert!(scheduler.status.contains("reserved feed budget"));
    assert!(scheduler.store.filings().unwrap().is_empty()); // Scripted panics on any unexpected get.
}

#[tokio::test]
async fn first_run_imports_every_watched_company_within_minutes_spaced_a_minute_apart() {
    // Fresh install: no previous "watched" signature, so no company carries a new: flag.
    let fake = Fake::new();
    let requests = fake.requests.clone();
    let mut scheduler = Scheduler::new(quiet_store(), fake);
    let symbols = vec!["AAPL US Equity".into(), "PPTA US Equity".into(), "OTHER US Equity".into()];
    let mut seen: Vec<(i64, usize)> = vec![];
    for minute in 0..=10 {
        scheduler.tick(NOW + minute * 60, true, &symbols).await;
        let count = requests.lock().unwrap().len();
        if seen.last().map(|s| s.1) != Some(count) {
            seen.push((minute, count));
        }
    }
    assert_eq!(requests.lock().unwrap().len(), 3, "all three first imports within ten minutes: {seen:?}");
    assert!(seen.windows(2).all(|w| w[1].0 - w[0].0 >= 1 && w[1].1 - w[0].1 == 1), "never a burst: {seen:?}");
    for cik in [320193, 1526243, 42] {
        assert!(scheduler.store.number(&format!("initialized:{cik}")).unwrap() > 0, "CIK {cik} not imported");
    }
}
