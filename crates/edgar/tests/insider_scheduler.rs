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
    for malformed in [false, true] {
        let fake = Fake::new();
        {
            let mut l = fake.0.lock().unwrap();
            l.malformed = malformed;
            if !malformed {
                l.failure = Some(Refused::Http(404));
            }
        }
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
}
