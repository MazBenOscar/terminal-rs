use super::*;
use std::sync::{Arc, Mutex};
use tr_core::price_alerts::{Direction, Repeat};

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
