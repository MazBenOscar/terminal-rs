use super::*;
use std::sync::{Arc, Mutex};
use tr_core::price_alerts::{Direction, Repeat};

#[test]
fn finished_close_ignores_forming_bar_fires_on_finished_close_and_deduplicates() {
    use tr_core::price_alerts::Rule;
    let start = 1_792_713_600; // UTC bucket boundary, fixed sample data.
    for (direction, finished, forming) in [(Direction::Above, 99., 101.), (Direction::Below, 101., 99.)] {
        let mut book = Book::default();
        let id = book.add("BTCUSD Curncy".into(), 100., direction, true).unwrap();
        book.set_rule(id, Some(Rule::Formula { expression: "C".into(), timeframe: Timeframe::H4 })).unwrap();
        book.set_repeat(id, Repeat::Rearm { cooldown_secs: 60 }).unwrap();
        book.alerts[0].completed_only = true;
        book.alerts[0].created_at = start;
        let bars = vec![
            Bar::new(start, finished, finished, finished, finished, 1.),
            Bar::new(start + 14_400, forming, forming, forming, forming, 1.),
        ];
        let mut q = Observation {
            symbol: "BTCUSD Curncy".into(),
            price: forming,
            ts: start + 14_400 + 100,
            simulated: true,
            source: "test".into(),
        };
        let observation = finished_observation(&book.alerts[0], &bars, &q).unwrap();
        assert_eq!(observation.price, finished, "forming bar must not be used");
        assert!(merge_rule(&mut book, id, Ok(observation), true, q.ts).is_empty());
        q.ts = start + 28_800;
        let observation = finished_observation(&book.alerts[0], &bars, &q).unwrap();
        assert_eq!(observation.price, forming);
        assert_eq!(merge_rule(&mut book, id, Ok(observation.clone()), true, q.ts).len(), 1);
        assert!(merge_rule(&mut book, id, Ok(observation.clone()), true, q.ts + 100).is_empty());
        let mut restored: Book = serde_json::from_str(&serde_json::to_string(&book).unwrap()).unwrap();
        assert!(
            merge_rule(&mut restored, id, Ok(observation), true, q.ts + 200).is_empty(),
            "same bar stays consumed across restart"
        );
        q.ts = start + 100;
        assert!(finished_observation(&book.alerts[0], &bars[..1], &q).is_err());
        // Existing Formula alerts still evaluate the forming bar, unchanged.
        assert_eq!(book.alerts[0].rule.as_ref().unwrap().value(&bars).unwrap(), forming);
    }
}

#[test]
fn finished_close_uses_equity_session_grid_and_calendar_month_end() {
    use tr_core::price_alerts::Rule;
    let parse = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp();
    let mut book = Book::default();
    let id = book.add("AAPL US Equity".into(), 100., Direction::Above, true).unwrap();
    book.set_rule(id, Some(Rule::Formula { expression: "C".into(), timeframe: Timeframe::H4 })).unwrap();
    let bar = Bar::new(parse("2026-10-23T13:30:00Z"), 101., 101., 101., 101., 1.);
    let mut q = Observation {
        symbol: "AAPL US Equity".into(),
        price: 101.,
        ts: parse("2026-10-23T16:30:00Z"),
        simulated: true,
        source: "test".into(),
    };
    assert!(finished_observation(&book.alerts[0], &[bar], &q).is_err(), "16:00 UTC is not this session bar's close");
    q.ts = parse("2026-10-23T17:30:00Z");
    assert_eq!(finished_observation(&book.alerts[0], &[bar], &q).unwrap().price, 101.);
    book.set_rule(id, Some(Rule::Formula { expression: "C".into(), timeframe: Timeframe::MN1 })).unwrap();
    let bar = Bar::new(parse("2026-02-01T00:00:00Z"), 101., 101., 101., 101., 1.);
    q.ts = parse("2026-02-28T23:58:00Z");
    assert!(finished_observation(&book.alerts[0], &[bar], &q).is_err());
    q.ts = parse("2026-02-28T23:59:00Z");
    assert!(finished_observation(&book.alerts[0], &[bar], &q).is_ok());
}

#[test]
fn review_equity_hourly_close_ignores_forming_half_past_bar() {
    use tr_core::price_alerts::Rule;
    let parse = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp();
    let mut book = Book::default();
    let id = book.add("AAPL US Equity".into(), 100., Direction::Above, true).unwrap();
    book.set_rule(id, Some(Rule::Formula { expression: "C".into(), timeframe: Timeframe::H1 })).unwrap();
    book.alerts[0].completed_only = true;
    // Yahoo 60m US-equity bars (the default equity path, H1 is not resampled) are labelled
    // at the session open grid: 09:30 ET = 13:30 UTC covers 13:30-14:30 UTC.
    let bars = [
        Bar::new(parse("2026-10-22T19:30:00Z"), 99., 99., 99., 99., 1.),
        Bar::new(parse("2026-10-23T13:30:00Z"), 101., 101., 101., 101., 1.),
    ];
    let q = Observation {
        symbol: "AAPL US Equity".into(),
        price: 101.,
        ts: parse("2026-10-23T14:05:00Z"),
        simulated: true,
        source: "test".into(),
    };
    let observation = finished_observation(&book.alerts[0], &bars, &q).unwrap();
    assert_eq!(observation.price, 99., "the 13:30 bar is still forming at 14:05");
}

#[test]
fn review_close_alert_does_not_fire_on_bar_closed_before_creation() {
    use tr_core::price_alerts::Rule;
    let now = 1_792_742_400 + 3_600; // one hour after a 4H bucket end
    let mut book = Book::default();
    let id = book.add("BTCUSD Curncy".into(), 100., Direction::Below, true).unwrap();
    book.set_rule(id, Some(Rule::Formula { expression: "C".into(), timeframe: Timeframe::H4 })).unwrap();
    book.alerts[0].completed_only = true;
    book.alerts[0].created_at = now;
    // Alert created at `now`; the latest finished 4H bar closed an hour earlier below the level.
    let q = Observation {
        symbol: "BTCUSD Curncy".into(),
        price: 95.,
        ts: 1_792_742_400,
        simulated: true,
        source: "test".into(),
    };
    assert!(merge_rule(&mut book, id, Ok(q.clone()), true, now).is_empty(), "no new finished close since creation");
    let mut book: Book = serde_json::from_value(serde_json::to_value(book).unwrap()).unwrap();
    let mut q = q;
    q.ts = now;
    assert!(merge_rule(&mut book, id, Ok(q.clone()), true, now).is_empty(), "creation boundary is not new");
    q.ts = now + 10_800;
    assert_eq!(merge_rule(&mut book, id, Ok(q.clone()), true, q.ts).len(), 1, "the next finished bar can fire");
    let mut legacy = serde_json::to_value(&book).unwrap();
    legacy["alerts"][0].as_object_mut().unwrap().remove("created_at");
    let legacy: Book = serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.alerts[0].created_at, 0);
}

#[test]
fn review_equity_daily_close_is_finished_at_session_close() {
    use tr_core::price_alerts::Rule;
    let parse = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp();
    let mut book = Book::default();
    let id = book.add("AAPL US Equity".into(), 100., Direction::Above, true).unwrap();
    book.set_rule(id, Some(Rule::Formula { expression: "C".into(), timeframe: Timeframe::D1 })).unwrap();
    book.alerts[0].completed_only = true;
    // Yahoo daily bar for the regular session 13:30-20:00 UTC.
    let bars = [Bar::new(parse("2026-10-22T13:30:00Z"), 101., 101., 101., 101., 1.)];
    // alert_observation() is the last regular-hours M1 bar; it is the freshest quote until tomorrow's open.
    let q = Observation {
        symbol: "AAPL US Equity".into(),
        price: 101.,
        ts: parse("2026-10-22T19:59:00Z"),
        simulated: true,
        source: "test".into(),
    };
    // The bucket end used is 00:00 UTC, which a regular-session quote never reaches; the next
    // fresh quote is tomorrow 13:30 UTC, so the daily close fires at the next open instead.
    assert!(finished_observation(&book.alerts[0], &bars, &q).is_ok(), "session closed at 20:00 UTC");
}

#[test]
fn equity_finished_ends_follow_provider_labels_dst_and_short_sessions() {
    use tr_core::price_alerts::Rule;
    let parse = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp();
    for (tf, start, end) in [
        (Timeframe::H1, "2026-10-23T13:30:00Z", "2026-10-23T14:30:00Z"),
        (Timeframe::H1, "2026-10-23T19:30:00Z", "2026-10-23T20:00:00Z"),
        (Timeframe::H4, "2026-10-23T17:30:00Z", "2026-10-23T20:00:00Z"),
        (Timeframe::D1, "2026-03-06T14:30:00Z", "2026-03-06T21:00:00Z"),
        (Timeframe::D1, "2026-03-09T13:30:00Z", "2026-03-09T20:00:00Z"),
        (Timeframe::D1, "2026-11-27T14:30:00Z", "2026-11-27T18:00:00Z"),
        (Timeframe::H1, "2026-11-27T17:30:00Z", "2026-11-27T18:00:00Z"),
    ] {
        let mut book = Book::default();
        let id = book.add("AAPL US Equity".into(), 100., Direction::Above, true).unwrap();
        book.set_rule(id, Some(Rule::Formula { expression: "C".into(), timeframe: tf })).unwrap();
        let bar = Bar::new(parse(start), 101., 101., 101., 101., 1.);
        let mut q = Observation {
            symbol: "AAPL US Equity".into(),
            price: 101.,
            ts: parse(end) - 120,
            simulated: true,
            source: "test".into(),
        };
        assert!(finished_observation(&book.alerts[0], &[bar], &q).is_err(), "{tf:?} {start} still forming");
        q.ts += 60;
        assert_eq!(
            finished_observation(&book.alerts[0], &[bar], &q).unwrap().ts,
            parse(end),
            "{tf:?} {start} finished"
        );
    }
}

#[test]
fn next_provider_bar_proves_completion_and_crypto_keeps_exact_end() {
    use tr_core::price_alerts::Rule;
    let start = 1_792_713_600;
    let mut book = Book::default();
    let id = book.add("BTCUSD Curncy".into(), 100., Direction::Above, true).unwrap();
    book.set_rule(id, Some(Rule::Formula { expression: "C".into(), timeframe: Timeframe::H1 })).unwrap();
    let bars = [Bar::new(start, 99., 99., 99., 99., 1.), Bar::new(start + 3600, 101., 101., 101., 101., 1.)];
    let mut q = Observation {
        symbol: "BTCUSD Curncy".into(),
        price: 101.,
        ts: start + 7199,
        simulated: true,
        source: "test".into(),
    };
    assert_eq!(finished_observation(&book.alerts[0], &bars, &q).unwrap().price, 99.);
    q.ts += 1;
    assert_eq!(finished_observation(&book.alerts[0], &bars, &q).unwrap().price, 101.);
    // A provider's shorter bar ends at the next label, not at a guessed UTC bucket.
    let bars = [bars[0], Bar { ts: start + 1800, ..bars[1] }];
    q.ts = start + 1800;
    assert_eq!(finished_observation(&book.alerts[0], &bars, &q).unwrap().ts, q.ts);
}

struct Fake {
    quote: Mutex<(f64, i64)>,
    calls: std::sync::atomic::AtomicUsize,
}
impl Fake {
    fn set(&self, price: f64, ts: i64) {
        *self.quote.lock().unwrap() = (price, ts);
    }
}
#[async_trait::async_trait]
impl tr_data::MarketData for Fake {
    fn name(&self) -> &str {
        "Fake"
    }
    async fn history(&self, _: &str, _: Timeframe, _: Option<i64>, _: usize) -> anyhow::Result<Vec<Bar>> {
        Ok(vec![])
    }
    async fn alert_observation(&self, symbol: &str) -> anyhow::Result<Observation> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (price, ts) = *self.quote.lock().unwrap();
        Ok(Observation { symbol: symbol.into(), price, ts, simulated: false, source: "Fake".into() })
    }
}

#[tokio::test]
async fn daemon_cycle_fires_persists_cools_down_and_seeds_after_restart() {
    let dir = std::env::temp_dir().join(format!("tr-alert-cycle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("price-alerts.json");
    let mut book = Book::default();
    let id = book.add("AAPL US Equity".into(), 100., Direction::Above, false).unwrap();
    book.set_repeat(id, Repeat::Rearm { cooldown_secs: 300 }).unwrap();
    // An armed alert in the other data mode must not interfere with the live baseline.
    book.add("AAPL US Equity".into(), 100., Direction::Above, true).unwrap();
    storage::save(&path, &book).unwrap();
    let now = chrono::Utc::now().timestamp();
    let provider = Arc::new(Fake { quote: Mutex::new((99., now - 10)), calls: Default::default() });
    let mut daemon = Monitor::default();
    let app = storage::Lock::acquire(&dir.join("app.lock")).unwrap();
    assert!(daemon.cycle(&dir, provider.clone()).await.unwrap().is_empty());
    assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(
        daemon.cycle_with(&dir, || panic!("Provider setup must idle with the app lock held")).await.unwrap().is_empty()
    );
    drop(app);
    // The first sample only seeds a baseline; the second cycle crosses it.
    assert!(daemon.cycle(&dir, provider.clone()).await.unwrap().is_empty());
    provider.set(101., now - 9);
    let fired = daemon.cycle(&dir, provider.clone()).await.unwrap();
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].alert.id, id);
    let saved = storage::load(&path).unwrap();
    assert_eq!(saved.events.len(), 1);
    assert_eq!(saved.alerts[0].status, Status::Armed);
    assert!(saved.alerts[0].last_fired.is_some());
    // A fresh crossing inside the cooldown must be consumed, not queued.
    provider.set(99., now - 8);
    assert!(daemon.cycle(&dir, provider.clone()).await.unwrap().is_empty());
    provider.set(101., now - 7);
    assert!(daemon.cycle(&dir, provider.clone()).await.unwrap().is_empty());
    assert_eq!(storage::load(&path).unwrap().events.len(), 1);
    // Expire cooldown on disk, then restart while already above the threshold.
    let mut saved = storage::load(&path).unwrap();
    saved.alerts[0].last_fired = Some(now - 301);
    storage::save(&path, &saved).unwrap();
    let mut restarted = Monitor::default();
    provider.set(102., now - 6);
    assert!(restarted.cycle(&dir, provider.clone()).await.unwrap().is_empty());
    assert_eq!(storage::load(&path).unwrap().events.len(), 1);
    // Restart really did seed: a subsequent new crossing can fire.
    provider.set(99., now - 5);
    assert!(restarted.cycle(&dir, provider.clone()).await.unwrap().is_empty());
    provider.set(101., now - 4);
    assert_eq!(restarted.cycle(&dir, provider).await.unwrap().len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn daemon_preserves_invalid_book_instead_of_overwriting_it() {
    let dir = std::env::temp_dir().join(format!("tr-alert-invalid-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("price-alerts.json");
    std::fs::write(&path, b"invalid").unwrap();
    let provider =
        Arc::new(Fake { quote: Mutex::new((99., chrono::Utc::now().timestamp())), calls: Default::default() });
    assert!(Monitor::default().cycle(&dir, provider).await.is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"invalid");
    std::fs::remove_dir_all(dir).unwrap();
}

/// The app must take over promptly and the daemon must discard in-flight work.
#[tokio::test]
async fn app_launch_takes_over_the_gate_without_waiting_out_a_daemon_cycle() {
    struct Slow;
    #[async_trait::async_trait]
    impl tr_data::MarketData for Slow {
        fn name(&self) -> &str {
            "Slow"
        }
        async fn history(&self, _: &str, _: Timeframe, _: Option<i64>, _: usize) -> anyhow::Result<Vec<Bar>> {
            Ok(vec![])
        }
        async fn alert_observation(&self, symbol: &str) -> anyhow::Result<Observation> {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            Ok(Observation {
                symbol: symbol.into(),
                price: 99.,
                ts: chrono::Utc::now().timestamp(),
                simulated: false,
                source: "Slow".into(),
            })
        }
    }
    let dir = std::env::temp_dir().join(format!("tr-alert-gate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("price-alerts.json");
    let mut book = Book::default();
    book.add("AAPL US Equity".into(), 100., Direction::Above, false).unwrap();
    storage::save(&path, &book).unwrap();

    let mut daemon = Monitor::default();
    let cycle_dir = dir.clone();
    let cycle = tokio::spawn(async move { daemon.cycle_with(&cycle_dir, || Arc::new(Slow)).await });
    // Let the cycle take both locks and enter the provider call.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let (tx, rx) = std::sync::mpsc::channel();
    let app_dir = dir.clone();
    std::thread::spawn(move || {
        let lock = storage::Lock::acquire_app(&app_dir.join("app.lock")).unwrap();
        let _ = tx.send(lock);
    });
    let takeover = rx.recv_timeout(std::time::Duration::from_millis(500));
    assert!(takeover.is_ok(), "app startup stayed blocked on app.lock while the daemon cycle was still running");
    let app_lock = takeover.unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(cycle.await.unwrap().unwrap().is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), before, "handoff must discard cycle results");
    drop(app_lock);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn fired_events_survive_a_failed_book_write() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("tr-alert-savefail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("price-alerts.json");
    let mut book = Book::default();
    let id = book.add("AAPL US Equity".into(), 100., Direction::Above, false).unwrap();
    storage::save(&path, &book).unwrap();

    let now = chrono::Utc::now().timestamp();
    let provider = Arc::new(Fake { quote: Mutex::new((99., now - 10)), calls: Default::default() });
    let mut daemon = Monitor::default();
    // Seed the baseline while the directory is still writable (this also creates
    // the lock files the next cycle will reuse).
    assert!(daemon.cycle(&dir, provider.clone()).await.unwrap().is_empty());
    // Freeze the directory so the atomic temp file cannot be created, then cross.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    provider.set(101., now - 9);
    let result = daemon.cycle(&dir, provider.clone()).await;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let events = result.expect("a fired alert must still be reported when the book cannot be written");
    assert_eq!(events.len(), 1, "the crossing must be delivered even though persistence failed");
    assert_eq!(events[0].alert.id, id);
    let failed_disk = storage::load(&path).unwrap();
    assert!(failed_disk.alerts[0].last_fired.is_none(), "failed write must leave the disk book untouched");
    provider.set(99., now - 8);
    assert!(daemon.cycle(&dir, provider.clone()).await.unwrap().is_empty());
    provider.set(101., now - 7);
    assert!(daemon.cycle(&dir, provider).await.unwrap().is_empty(), "failed persistence must not refire");
    assert!(storage::load(&path).unwrap().alerts[0].last_fired.is_some(), "next cycle retries persistence");
    std::fs::remove_dir_all(dir).unwrap();
}
