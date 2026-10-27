//! Fixture-only transport: never constructs a client or opens a socket.
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tr_edgar::{
    Company, Filing,
    scheduler::{Reply, Scheduler, Transport},
    store::Store,
};
use tr_net::{Refused, ResponseMetadata};
const NOW: i64 = 1_793_145_600;
#[derive(Default)]
struct Ledger {
    now: i64,
    slots: BTreeMap<String, i64>,
    counts: BTreeMap<String, i64>,
    requests: Vec<(i64, String)>,
    pending: bool,
    refusal: Option<Refused>,
    failure: Option<Refused>,
    malformed: bool,
}
#[derive(Clone)]
struct Fake(Arc<Mutex<Ledger>>);
impl Fake {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Ledger { now: NOW, pending: true, ..Default::default() })))
    }
    fn at(&self, now: i64) {
        self.0.lock().unwrap().now = now;
    }
    fn count(&self) -> usize {
        self.0.lock().unwrap().requests.len()
    }
}
impl Transport for Fake {
    fn check(&self) -> Result<(), Refused> {
        self.0.lock().unwrap().refusal.clone().map_or(Ok(()), Err)
    }
    fn pending_allowed(&self) -> Result<bool, Refused> {
        Ok(self.0.lock().unwrap().pending)
    }
    fn claim_slot(&self, key: &str, interval: Duration) -> Result<bool, Refused> {
        let mut l = self.0.lock().unwrap();
        let now = l.now;
        if l.slots.get(key).is_some_and(|end| *end > now) {
            return Ok(false);
        }
        l.slots.insert(key.into(), now + interval.as_secs() as i64);
        Ok(true)
    }
    fn increment_counter(&self, key: &str, _: i64) -> Result<i64, Refused> {
        let mut l = self.0.lock().unwrap();
        let count = l.counts.entry(key.into()).or_default();
        *count += 1;
        Ok(*count)
    }
    fn decrement_counter(&self, key: &str) -> Result<(), Refused> {
        let mut l = self.0.lock().unwrap();
        let count = l.counts.entry(key.into()).or_default();
        *count = (*count - 1).max(0);
        Ok(())
    }
    fn release_slot(&self, key: &str) -> Result<(), Refused> {
        self.0.lock().unwrap().slots.remove(key);
        Ok(())
    }
    async fn get(&self, url: &str, _: Option<String>, _: Option<String>) -> Result<Reply, Refused> {
        let mut l = self.0.lock().unwrap();
        let now = l.now;
        assert!(url.starts_with("https://www.sec.gov/Archives/"));
        assert!(!url.contains("/xsl"));
        l.requests.push((now, url.into()));
        if let Some(e) = &l.failure {
            return Err(e.clone());
        }
        Ok(Reply {
            body: if l.malformed {
                b"broken".to_vec()
            } else {
                include_bytes!("fixtures/form4_sale_10b5_1.xml").to_vec()
            },
            metadata: ResponseMetadata { status: 200, ..Default::default() },
        })
    }
}
fn store(n: usize) -> Store {
    let mut s = Store::memory().unwrap();
    s.replace_companies(&[Company { cik: 320193, ticker: "AAPL".into(), title: "Apple".into() }]).unwrap();
    for key in ["map_checked", "feed_checked", "next_sweep"] {
        s.set(key, NOW + 60 * 86400).unwrap();
    }
    for key in ["attempt:320193", "swept:320193", "initialized:320193"] {
        s.set(key, NOW).unwrap();
    }
    s.upsert(
        &(0..n)
            .map(|i| Filing {
                cik: 320193,
                ticker: "AAPL".into(),
                accession: format!("0000320193-26-{i:06}"),
                form: "4".into(),
                filed: NOW - i as i64,
                primary_doc: "xslF345X06/form4.xml".into(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();
    s
}
async fn tick(s: &mut Scheduler<Fake>, fake: &Fake, now: i64) {
    fake.at(now);
    s.tick(now, true, &["AAPL".into()]).await;
}
#[tokio::test]
async fn once_per_accession_newest_first_fifteen_second_spacing_four_per_minute() {
    let fake = Fake::new();
    let mut s = Scheduler::new(store(6), fake.clone());
    for seconds in 0..60 {
        tick(&mut s, &fake, NOW + seconds).await;
    }
    assert_eq!(fake.count(), 4);
    {
        let l = fake.0.lock().unwrap();
        assert!(l.requests.windows(2).all(|w| w[1].0 - w[0].0 >= 15));
        assert!(l.requests[0].1.contains("000032019326000000"));
    }
    assert!(s.status.contains("Reading insider forms: 2 left"), "{}", s.status);
    for seconds in 60..180 {
        tick(&mut s, &fake, NOW + seconds).await;
    }
    assert_eq!(fake.count(), 6);
    assert!(!s.status.contains("Reading insider forms"));
    for f in s.store.filings().unwrap() {
        assert_eq!(f.insiders.len(), 1);
    }
    // A different local store cannot reclaim accessions from the shared ledger.
    let mut second = Scheduler::new(store(6), fake.clone());
    tick(&mut second, &fake, NOW + 600).await;
    assert_eq!(fake.count(), 6);
    // A new data directory after the 30-day claim expires still cannot reread a form.
    let mut third = Scheduler::new(store(6), fake.clone());
    tick(&mut third, &fake, NOW + 31 * 86400).await;
    assert_eq!(fake.count(), 6);
}
#[tokio::test]
async fn cap_300_machine_wide_pending_reserve_visibility_and_refusal() {
    let day = chrono::DateTime::from_timestamp(NOW, 0).unwrap().format("%Y%m%d");
    let fake = Fake::new();
    fake.0.lock().unwrap().counts.insert(format!("sec:f4:day:{day}"), 299);
    let mut s = Scheduler::new(store(8), fake.clone());
    tick(&mut s, &fake, NOW).await;
    assert_eq!(fake.count(), 1);
    tick(&mut s, &fake, NOW + 60).await;
    assert_eq!(fake.count(), 1);
    let mut second = Scheduler::new(store(8), fake.clone());
    tick(&mut second, &fake, NOW + 120).await;
    assert_eq!(fake.count(), 1);
    let fake = Fake::new();
    fake.0.lock().unwrap().pending = false;
    let mut s = Scheduler::new(store(8), fake.clone());
    tick(&mut s, &fake, NOW).await;
    assert_eq!(fake.count(), 0);
    fake.0.lock().unwrap().pending = true;
    s.tick(NOW, false, &["AAPL".into()]).await;
    assert_eq!(fake.count(), 0);
    for refusal in [
        Refused::Offline,
        Refused::Disabled,
        Refused::Budget,
        Refused::NeedsContact,
        Refused::Paused { until: NOW + 3600 },
    ] {
        fake.0.lock().unwrap().refusal = Some(refusal.clone());
        tick(&mut s, &fake, NOW + 600).await;
        assert_eq!(fake.count(), 0);
        assert_eq!(s.status, tr_edgar::scheduler::refusal_label(&refusal));
    }
}
#[tokio::test]
async fn attempts_back_off_one_six_twenty_four_hours_then_details_unavailable() {
    for failure in [Refused::Http(404), Refused::Transport] {
        let fake = Fake::new();
        fake.0.lock().unwrap().failure = Some(failure);
        let mut s = Scheduler::new(store(1), fake.clone());
        for (sec, count) in [(0, 1), (15, 1), (3599, 1), (3600, 2), (25199, 2), (25200, 3), (111600, 3), (200000, 3)] {
            tick(&mut s, &fake, NOW + sec).await;
            assert_eq!(fake.count(), count, "at {sec}");
        }
        assert_eq!(s.store.number("f4:retry:0000320193-26-000000").unwrap(), NOW + 25200 + 86400);
        let f = s.store.filings().unwrap().remove(0);
        assert!(f.insider_unavailable);
        assert!(f.insiders.is_empty());
    }
}
#[tokio::test]
async fn malformed_immutable_form_is_terminal_without_source_failure() {
    let fake = Fake::new();
    fake.0.lock().unwrap().malformed = true;
    let mut s = Scheduler::new(store(1), fake.clone());
    for sec in [0, 60, 3600, 25200, 111600] {
        tick(&mut s, &fake, NOW + sec).await;
        assert_eq!(fake.count(), 1);
        assert!(!s.flags.unavailable);
        assert!(!s.status.contains("refresh unavailable"));
        assert_eq!(s.store.number("f4:retry:0000320193-26-000000").unwrap(), 0);
    }
    assert!(s.store.filings().unwrap()[0].insider_unavailable);
}
#[tokio::test]
async fn get_source_refusals_cost_no_attempt_backoff_or_daily_allowance() {
    for reason in [
        Refused::Paused { until: NOW + 3600 },
        Refused::Budget,
        Refused::Ledger,
        Refused::NeedsContact,
        Refused::Disabled,
        Refused::Offline,
        Refused::Busy,
    ] {
        let fake = Fake::new();
        fake.0.lock().unwrap().failure = Some(reason.clone());
        let mut s = Scheduler::new(store(2), fake.clone());
        tick(&mut s, &fake, NOW).await;
        assert_eq!(fake.count(), 1, "stop the cycle, do not try the next row");
        assert_eq!(s.status, tr_edgar::scheduler::refusal_label(&reason));
        for prefix in ["attempts", "retry"] {
            assert_eq!(s.store.number(&format!("f4:{prefix}:0000320193-26-000000")).unwrap(), 0);
        }
        assert!(fake.0.lock().unwrap().counts.values().all(|count| *count == 0));
        fake.0.lock().unwrap().failure = None;
        tick(&mut s, &fake, NOW + 60).await;
        assert_eq!(fake.count(), 2);
        assert!(s.store.filings().unwrap()[0].insider_parsed);
    }
}
#[tokio::test]
async fn contention_does_not_spend_daily_allowance_and_retry_status_counts_only_due() {
    let fake = Fake::new();
    let mut first = Scheduler::new(store(1), fake.clone());
    tick(&mut first, &fake, NOW).await;
    let before = fake.0.lock().unwrap().counts.clone();
    let mut second = Scheduler::new(store(1), fake.clone());
    tick(&mut second, &fake, NOW + 60).await;
    assert_eq!(fake.0.lock().unwrap().counts, before);
    let fake = Fake::new();
    fake.0.lock().unwrap().failure = Some(Refused::Http(404));
    let mut s = Scheduler::new(store(2), fake.clone());
    tick(&mut s, &fake, NOW).await;
    assert!(s.status.contains("Reading insider forms: 1 left (1 waiting to retry)"), "{}", s.status);
}
#[tokio::test]
async fn form_pacing_ignores_legacy_local_last_time_even_after_clock_reversal() {
    let fake = Fake::new();
    let db = store(1);
    db.set("f4:last", NOW + 365 * 86400).unwrap();
    let mut s = Scheduler::new(db, fake.clone());
    tick(&mut s, &fake, NOW).await;
    assert_eq!(fake.count(), 1);
}
#[tokio::test]
async fn issuer_only_window_xml_only_and_transport_pause_stops_immediately() {
    let fake = Fake::new();
    let mut db = store(5);
    let mut rows = db.filings().unwrap();
    // Replace candidates with fresh identities; the original issuer rows are outside the stored window.
    db.set("window:320193", NOW + 1).unwrap();
    rows[0].accession = "reporting".into();
    rows[0].reporting_owner = true;
    rows[0].filed = NOW + 2;
    rows[1].accession = "html".into();
    rows[1].primary_doc = "doc.htm".into();
    rows[1].filed = NOW + 2;
    db.upsert(&rows[..2]).unwrap();
    let mut s = Scheduler::new(db, fake.clone());
    tick(&mut s, &fake, NOW + 3).await;
    assert_eq!(fake.count(), 0);
    let fake = Fake::new();
    fake.0.lock().unwrap().failure = Some(Refused::Paused { until: NOW + 21600 });
    let mut s = Scheduler::new(store(4), fake.clone());
    tick(&mut s, &fake, NOW).await;
    assert_eq!(fake.count(), 1);
    assert!(s.status.starts_with("Paused until"));
}
#[tokio::test]
async fn durable_results_survive_reopening_and_never_refetch() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data/insiders");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!(
        "store-{}-{}.sqlite3",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    let db = store(1);
    let mut disk = Store::open(&path).unwrap();
    disk.replace_companies(&db.companies().unwrap()).unwrap();
    disk.upsert(&db.filings().unwrap()).unwrap();
    for key in ["map_checked", "feed_checked", "next_sweep", "attempt:320193", "swept:320193", "initialized:320193"] {
        disk.set(key, db.number(key).unwrap()).unwrap();
    }
    let fake = Fake::new();
    let mut s = Scheduler::new(disk, fake.clone());
    tick(&mut s, &fake, NOW).await;
    assert_eq!(fake.count(), 1);
    drop(s);
    let mut s = Scheduler::new(Store::open(&path).unwrap(), fake.clone());
    tick(&mut s, &fake, NOW + 600).await;
    assert_eq!(fake.count(), 1);
    assert_eq!(s.store.filings().unwrap()[0].insiders.len(), 1);
}
#[test]
fn thirty_day_accession_claim_is_shared_by_two_real_guards() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/test-data/insiders/ledger-{}-{}.sqlite3",
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
    let a = guard();
    let b = guard();
    let interval = Duration::from_secs(30 * 86400);
    assert!(a.claim_slot("sec:f4:accession", interval).unwrap());
    assert!(!b.claim_slot("sec:f4:accession", interval).unwrap());
    assert_eq!(a.increment_counter("sec:f4:day:20261027").unwrap(), 1);
    assert_eq!(b.increment_counter("sec:f4:day:20261027").unwrap(), 2);
    assert!(!a.reserve_counter("sec:f4:day:20261027", 2).unwrap());
    b.decrement_counter("sec:f4:day:20261027").unwrap();
    assert!(a.reserve_counter("sec:f4:day:20261027", 2).unwrap());
    assert!(!b.reserve_counter("sec:f4:day:20261027", 2).unwrap());
    a.release_slot("sec:f4:accession").unwrap();
    assert!(b.claim_slot("sec:f4:accession", interval).unwrap());
}
// ---- Review (N3): failing tests for defects found in production-readiness review ----
const BRK: u64 = 1067983;
fn issuer_and_owner_store(owner_row: bool, issuer_row: bool) -> Store {
    let mut s = Store::memory().unwrap();
    s.replace_companies(&[
        Company { cik: 320193, ticker: "AAPL".into(), title: "Apple".into() },
        Company { cik: BRK, ticker: "BRK-B".into(), title: "Berkshire".into() },
    ])
    .unwrap();
    for key in ["map_checked", "feed_checked", "next_sweep"] {
        s.set(key, NOW + 60 * 86400).unwrap();
    }
    for cik in [320193, BRK] {
        for key in ["attempt", "swept", "initialized"] {
            s.set(&format!("{key}:{cik}"), NOW).unwrap();
        }
    }
    let row = |cik: u64, ticker: &str, filed: i64| Filing {
        cik,
        ticker: ticker.into(),
        accession: "0000320193-26-000777".into(),
        form: "4".into(),
        filed,
        primary_doc: "xslF345X06/form4.xml".into(),
        // Submissions JSON lists Form 4s for both the issuer and the reporting owner
        // and does not label the role, so the owner's row arrives with reporting_owner=false.
        from_submissions: true,
        ..Default::default()
    };
    let mut rows = vec![];
    if owner_row {
        rows.push(row(BRK, "BRK-B", NOW));
    }
    if issuer_row {
        rows.push(row(320193, "AAPL", NOW - 10));
    }
    s.upsert(&rows).unwrap();
    s
}
#[tokio::test]
async fn review_issuer_mismatch_is_fetched_once() {
    let fake = Fake::new();
    let mut s = Scheduler::new(issuer_and_owner_store(true, false), fake.clone());
    for sec in [0, 60, 3600, 3660, 25200, 25260, 111600, 111660, 300000] {
        fake.at(NOW + sec);
        s.tick(NOW + sec, true, &["BRK-B".into()]).await;
    }
    assert_eq!(fake.count(), 1, "issuer-mismatch documents are permanent; never retry them");
    assert!(!s.flags.unavailable, "a reporting-owner document is not a source error");
    assert!(!s.status.contains("Reading insider forms"));
    let row = s.store.filings().unwrap().remove(0);
    assert!(row.reporting_owner);
    assert!(row.insiders.is_empty());
}
#[tokio::test]
async fn review_owner_row_does_not_starve_issuer_row_with_same_accession() {
    let fake = Fake::new();
    let mut s = Scheduler::new(issuer_and_owner_store(true, true), fake.clone());
    let symbols: Vec<String> = vec!["AAPL".into(), "BRK-B".into()];
    for sec in [0, 60, 120, 3600, 3660, 3720, 25200, 25260, 25320, 111600, 111660, 111720] {
        fake.at(NOW + sec);
        s.tick(NOW + sec, true, &symbols).await;
    }
    let issuer = s.store.filings().unwrap().into_iter().find(|f| f.cik == 320193).unwrap();
    assert!(
        !issuer.insiders.is_empty(),
        "AAPL's own Form 4 was never parsed (unavailable={})",
        issuer.insider_unavailable
    );
    assert_eq!(fake.count(), 1, "owner and issuer share a single immutable fetch");
    assert!(s.store.filings().unwrap().iter().find(|f| f.cik == BRK).unwrap().reporting_owner);
}
/// Transport whose first document request never completes, like a tick dropped by the
/// worker's `select!` when interest changes (Reload, visibility) mid-request.
#[derive(Clone)]
struct Hang(Fake, Arc<Mutex<bool>>);
impl Transport for Hang {
    fn check(&self) -> Result<(), Refused> {
        self.0.check()
    }
    fn pending_allowed(&self) -> Result<bool, Refused> {
        self.0.pending_allowed()
    }
    fn claim_slot(&self, key: &str, interval: Duration) -> Result<bool, Refused> {
        self.0.claim_slot(key, interval)
    }
    fn increment_counter(&self, key: &str, n: i64) -> Result<i64, Refused> {
        self.0.increment_counter(key, n)
    }
    async fn get(&self, url: &str, e: Option<String>, m: Option<String>) -> Result<Reply, Refused> {
        let hang = std::mem::replace(&mut *self.1.lock().unwrap(), false);
        if hang {
            {
                let mut l = self.0.0.lock().unwrap();
                let now = l.now;
                l.requests.push((now, url.into()));
            }
            std::future::pending::<()>().await;
        }
        self.0.get(url, e, m).await
    }
}
#[tokio::test]
async fn review_cancelled_fetch_is_not_refetched_within_a_minute() {
    let fake = Fake::new();
    let hang = Hang(fake.clone(), Arc::new(Mutex::new(true)));
    let mut s = Scheduler::new(store(1), hang.clone());
    fake.at(NOW);
    let cancelled = tokio::time::timeout(Duration::from_millis(50), s.tick(NOW, true, &["AAPL".into()])).await;
    assert!(cancelled.is_err());
    assert_eq!(fake.count(), 1);
    for sec in [60, 120] {
        fake.at(NOW + sec);
        s.tick(NOW + sec, true, &["AAPL".into()]).await;
    }
    assert_eq!(fake.count(), 1, "same accession requested again right after a cancelled request");
    assert_eq!(s.store.number("f4:retry:0000320193-26-000000").unwrap(), NOW + 3600);
    fake.at(NOW + 3600);
    s.tick(NOW + 3600, true, &["AAPL".into()]).await;
    assert_eq!(fake.count(), 2);
    assert_eq!(s.store.number("f4:retry:0000320193-26-000000").unwrap(), 0);
}
