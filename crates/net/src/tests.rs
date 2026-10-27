use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data/net").join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn guard(&self, offline: bool) -> Guard {
        // An injected environment snapshot avoids unsafe global env changes in parallel tests.
        let data_dir = self.0.join(format!("instance-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
        let mut config = Config::from_lookup(|key| match key {
            "TR_NET_LEDGER" => Some(self.0.join("ledger.sqlite3").into_os_string()),
            "TR_DATA_DIR" => Some(data_dir.clone().into_os_string()),
            "TR_ALLOW_EXTERNAL" if !offline => Some("1".into()),
            _ => None,
        })
        .unwrap();
        config.max_wait = Duration::ZERO;
        Guard::new(config, Settings::default()).unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Server {
    url: reqwest::Url,
    hits: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(status: u16, headers: &str, body: &str) -> Self {
        Self::delayed(status, headers, body, Duration::ZERO)
    }
    fn delayed(status: u16, headers: &str, body: &str, delay: Duration) -> Self {
        Self::sequence(&[(status, headers, body)], delay)
    }
    fn sequence(responses: &[(u16, &str, &str)], delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url: reqwest::Url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (h, r, s) = (hits.clone(), requests.clone(), stop.clone());
        let responses: Vec<_> = responses
            .iter()
            .map(|(status, headers, body)| {
                format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
                    body.len()
                )
            })
            .collect();
        let worker = thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let index = h.fetch_add(1, Ordering::SeqCst).min(responses.len() - 1);
                        stream.set_nonblocking(false).unwrap();
                        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                        let mut bytes = Vec::new();
                        let mut buf = [0; 4096];
                        while !bytes.ends_with(b"\r\n\r\n") {
                            match stream.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => bytes.extend_from_slice(&buf[..n]),
                            }
                        }
                        r.lock().unwrap().push(String::from_utf8_lossy(&bytes).into_owned());
                        thread::sleep(delay);
                        let _ = stream.write_all(responses[index].as_bytes());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(2)),
                    Err(e) => panic!("fixture listener: {e}"),
                }
            }
        });
        Self { url, hits, requests, stop, worker: Some(worker) }
    }
    fn attach(&self, mut guard: Guard) -> Guard {
        guard.fixture = Some(self.url.clone());
        guard
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn request() -> reqwest::RequestBuilder {
    reqwest::Client::new().get("https://finnhub.io/api/v1/quote?token=secret&apikey=hidden")
}
fn age(guard: &Guard, group: &str) {
    guard.db().unwrap().execute("UPDATE last_request SET at=0 WHERE \"group\"=?1", [group]).unwrap();
}

#[tokio::test]
async fn offline_gate_opens_no_socket() {
    let temp = Temp::new();
    let server = Server::new(200, "", "{}");
    let guard = server.attach(temp.guard(true));
    assert_eq!(guard.send("finnhub", request()).await, Err(Refused::Offline));
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
    let status = guard.status().unwrap();
    assert!(status.iter().all(|s| s.offline && s.requests_today == 0 && s.last_request.is_none()));
}

#[tokio::test]
async fn unknown_host_and_group_are_refused_before_connect() {
    let temp = Temp::new();
    let server = Server::new(200, "", "{}");
    let guard = server.attach(temp.guard(false));
    for url in [
        "https://example.invalid/",
        "https://query1.finance.yahoo.com/",
        "http://finnhub.io/",
        "https://finnhub.io:8443/",
    ] {
        assert_eq!(guard.send("finnhub", reqwest::Client::new().get(url)).await, Err(Refused::UnknownHost));
    }
    assert_eq!(guard.send("unknown", request()).await, Err(Refused::UnknownHost));
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn independent_instances_share_pacing_and_daily_cap() {
    let temp = Temp::new();
    // These instances opt into external access with independently overridden data dirs.
    // Only the explicit machine ledger path is shared; no application data path enters Guard.
    let server = Server::new(200, "", "{}");
    let one = server.attach(temp.guard(false));
    let two = server.attach(temp.guard(false));
    assert_ne!(one.config.data_dir, two.config.data_dir);
    assert_eq!(one.config.ledger, two.config.ledger);
    assert_eq!(one.send("finnhub", request()).await.unwrap(), b"{}");
    assert_eq!(two.send("finnhub", request()).await, Err(Refused::Budget));
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    assert_eq!(two.status().unwrap()[0].requests_today, 1);
    one.db().unwrap().execute("UPDATE requests SET count=3000", []).unwrap();
    age(&one, "finnhub");
    assert_eq!(two.send("finnhub", request()).await, Err(Refused::Budget));
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    assert_eq!(two.status().unwrap()[0].requests_today, 3000);
}

#[test]
fn immediate_transactions_allow_only_one_reservation() {
    let temp = Temp::new();
    let one = temp.guard(false);
    let two = temp.guard(false);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let gate = barrier.clone();
    let task = thread::spawn(move || {
        gate.wait();
        one.reserve(&policies()[0]).unwrap().is_none()
    });
    barrier.wait();
    let reserved = two.reserve(&policies()[0]).unwrap().is_none();
    assert_ne!(reserved, task.join().unwrap());
    assert_eq!(two.status().unwrap()[0].requests_today, 1);
}

#[tokio::test]
async fn bounded_wait_uses_persisted_spacing() {
    let temp = Temp::new();
    let guard = temp.guard(false);
    let mut policy = policies()[0].clone();
    policy.min_interval = Duration::from_millis(30);
    policy.per_minute = 2000;
    assert!(guard.reserve(&policy).unwrap().is_none());
    guard.db().unwrap().execute("DELETE FROM inflight", []).unwrap();
    assert!(guard.reserve(&policy).unwrap().is_some());
    tokio::time::sleep(Duration::from_millis(35)).await;
    assert!(guard.reserve(&policy).unwrap().is_none());
}

#[tokio::test]
async fn send_waits_within_bound_and_old_days_do_not_exhaust_budget() {
    let temp = Temp::new();
    let server = Server::new(200, "", "{}");
    let one = server.attach(temp.guard(false));
    let mut two = server.attach(temp.guard(false));
    two.config.max_wait = Duration::from_millis(500);
    one.send("finnhub", request()).await.unwrap();
    let previous = Utc::now().timestamp_millis() - 2900;
    one.db().unwrap().execute("UPDATE last_request SET at=?1", [previous]).unwrap();
    one.db().unwrap().execute("UPDATE requests SET day=day-1, count=3000", []).unwrap();
    two.send("finnhub", request()).await.unwrap();
    let status = &two.status().unwrap()[0];
    assert_eq!(status.requests_today, 1);
    assert!(status.last_request.unwrap() >= previous + 3000);
    assert_eq!(server.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn concurrency_lease_covers_body_and_other_instances() {
    let temp = Temp::new();
    let server = Server::delayed(200, "", "{}", Duration::from_millis(150));
    let one = Arc::new(server.attach(temp.guard(false)));
    let two = server.attach(temp.guard(false));
    let task = {
        let one = one.clone();
        tokio::spawn(async move { one.send("finnhub", request()).await })
    };
    while server.hits.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    age(&two, "finnhub");
    assert_eq!(two.send("finnhub", request()).await, Err(Refused::Budget));
    task.await.unwrap().unwrap();
    assert_eq!(two.send("finnhub", request()).await.unwrap(), b"{}");
}

#[tokio::test]
async fn soft_blocks_persist_expire_and_escalate() {
    for (code, headers, body) in [
        (429, "Retry-After: 60\r\n", "slow down"),
        (403, "", "forbidden"),
        (503, "", "unavailable"),
        (200, "", "<html>captcha</html>"),
    ] {
        let temp = Temp::new();
        let server = Server::new(code, headers, body);
        let guard = server.attach(temp.guard(false));
        let before = Utc::now().timestamp();
        let Err(Refused::Paused { until }) = guard.send("finnhub", request()).await else { panic!("expected pause") };
        let expected = 21600;
        assert!((before + expected..=Utc::now().timestamp() + expected).contains(&until));
        drop(guard);
        let recreated = server.attach(temp.guard(false));
        assert_eq!(recreated.send("finnhub", request()).await, Err(Refused::Paused { until }));
        let status = &recreated.status().unwrap()[0];
        assert_eq!(status.paused_until, Some(until));
        assert!(status.reason.is_some());
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        recreated.db().unwrap().execute("UPDATE cooldowns SET until=0", []).unwrap();
        age(&recreated, "finnhub");
        let Err(Refused::Paused { until }) = recreated.send("finnhub", request()).await else {
            panic!("expected second pause")
        };
        assert!(until >= before + 43200);
        assert_eq!(server.hits.load(Ordering::SeqCst), 2);
    }
}

#[test]
fn retry_after_parsing_and_backoff_caps() {
    let now = Utc::now().timestamp();
    assert_eq!(retry_after("9999999999", now), Some(now + 604800));
    assert_eq!(retry_after("0", now), Some(now + 1));
    assert_eq!(retry_after("not a date", now), None);
    let date = chrono::DateTime::from_timestamp(now + 3600, 0).unwrap().to_rfc2822();
    assert_eq!(retry_after(&date, now), Some(now + 3600));
    let temp = Temp::new();
    let guard = temp.guard(false);
    for delay in [21600, 43200, 86400, 86400] {
        let start = Utc::now().timestamp();
        let Refused::Paused { until } = guard.pause("finnhub", "fixture", None).unwrap() else { panic!() };
        assert!((start + delay..=Utc::now().timestamp() + delay).contains(&until));
    }
}

#[tokio::test]
async fn sec_contact_identity_and_switches() {
    let temp = Temp::new();
    let server = Server::new(200, "", "{}");
    let guard = server.attach(temp.guard(false));
    assert_eq!(
        guard.send("sec", reqwest::Client::new().get("https://data.sec.gov/test")).await,
        Err(Refused::NeedsContact)
    );
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
    let mut settings = Settings { sec_contact: "research@example.test".into(), ..Default::default() };
    guard.configure(settings.clone());
    let ua = format!("terminal-rs/{} (personal research; research@example.test)", env!("CARGO_PKG_VERSION"));
    assert_eq!(guard.identity("sec").unwrap(), ua);
    guard
        .send("sec", reqwest::Client::new().get("https://data.sec.gov/test").header("User-Agent", "Browser spoof"))
        .await
        .unwrap();
    assert!(server.requests.lock().unwrap()[0].contains(&ua));
    assert!(!server.requests.lock().unwrap()[0].contains("Browser spoof"));
    assert!(guard.reserve(&policies()[1]).unwrap().is_some(), "SEC www/data share pacing");
    for group in ["sec", "finnhub", "coindesk"] {
        let ua = guard.identity(group).unwrap();
        assert!(ua.starts_with("terminal-rs/") && !ua.contains("Mozilla"));
    }
    assert_eq!(guard.identity("yahoo-news").unwrap(), "Mozilla/5.0 (compatible; terminal-rs)");
    settings.pause_all = true;
    guard.configure(settings.clone());
    assert_eq!(guard.send("finnhub", request()).await, Err(Refused::Disabled));
    assert!(guard.status().unwrap().iter().all(|s| !s.enabled));
    settings.pause_all = false;
    settings.finnhub = false;
    guard.configure(settings);
    assert_eq!(guard.send("finnhub", request()).await, Err(Refused::Disabled));
}

#[tokio::test]
async fn transport_failures_are_durable_and_errors_contain_no_secrets() {
    let temp = Temp::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url: reqwest::Url = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
    drop(listener);
    for attempt in 1..=3 {
        let mut guard = temp.guard(false);
        guard.fixture = Some(url.clone());
        age(&guard, "finnhub");
        let error = guard.send("finnhub", request()).await.unwrap_err();
        assert!(!format!("{error:?} {error}").contains("secret"));
        if attempt < 3 {
            assert_eq!(error, Refused::Transport);
        } else {
            assert!(matches!(error, Refused::Paused { .. }));
        }
    }
    assert!(temp.guard(false).status().unwrap()[0].paused_until.is_some());
    let text = redact("https://finnhub.io/a?token=secret&APIKEY=hidden&symbol=AAPL");
    assert!(!text.contains("secret") && !text.contains("hidden") && text.contains("AAPL"));
    assert_eq!(redact("unparseable token=secret"), "<redacted>");
}

#[tokio::test]
async fn oversized_body_and_redirects_do_not_escape_guard() {
    let temp = Temp::new();
    let server = Server::new(302, "Location: https://example.invalid/\r\n", "");
    let guard = server.attach(temp.guard(false));
    assert_eq!(guard.send("finnhub", request()).await, Err(Refused::Redirect("example.invalid".into())));
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    let large = "x".repeat(BODY_CAP + 1);
    let server = Server::new(200, "", &large);
    let guard = server.attach(temp.guard(false));
    age(&guard, "finnhub");
    assert_eq!(guard.send("finnhub", request()).await, Err(Refused::BodyTooLarge));
}

#[test]
fn environment_paths_and_offline_opt_in() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data/path-fixture");
    let lookup = |key: &str| match key {
        "HOME" => Some(root.clone().into_os_string()),
        "TR_DATA_DIR" => Some(root.join("throwaway").into_os_string()),
        _ => None,
    };
    let config = Config::from_lookup(lookup).unwrap();
    let base = if cfg!(target_os = "macos") { "Library/Application Support" } else { ".local/share" };
    assert_eq!(config.ledger, root.join(base).join("terminal-rs-net/ledger.sqlite3"));
    assert!(config.data_dir.is_some() && !config.allow_external);
    for flag in ["0", "true", "yes", "1"] {
        let config =
            Config::from_lookup(|key| if key == "TR_ALLOW_EXTERNAL" { Some(flag.into()) } else { lookup(key) })
                .unwrap();
        assert_eq!(config.allow_external, flag == "1");
    }
    let override_path = root.join("isolated-ledger.sqlite3");
    let config = Config::from_lookup(|key| {
        if key == "TR_NET_LEDGER" { Some(override_path.clone().into_os_string()) } else { lookup(key) }
    })
    .unwrap();
    assert_eq!(config.ledger, override_path);
    if !cfg!(target_os = "macos") {
        let xdg = root.join("xdg");
        let config =
            Config::from_lookup(
                |key| if key == "XDG_DATA_HOME" { Some(xdg.clone().into_os_string()) } else { lookup(key) },
            )
            .unwrap();
        assert_eq!(config.ledger, xdg.join("terminal-rs-net/ledger.sqlite3"));
    }
}

#[test]
fn status_and_registry() {
    let temp = Temp::new();
    let guard = temp.guard(false);
    let rows = guard.status().unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows.iter().map(|s| s.cap).collect::<Vec<_>>(), [3000, 1500, 1000, 300]);
    assert!(rows.iter().all(|s| s.enabled
        && s.requests_today == 0
        && s.last_request.is_none()
        && s.paused_until.is_none()
        && !s.offline));
    assert!(rows[1].needs_contact);
    let policies = policies();
    assert!(policies.iter().all(|p| p.concurrency == 1));
    assert_eq!(policies.iter().filter(|p| p.group == "sec").count(), 3);
    assert_eq!(policies.iter().filter(|p| p.group == "yahoo-news").count(), 2);
    for body in [
        "Potential security risk",
        "sorry, a potential",
        "ACCESS DENIED",
        "Verify you are human",
        "Checking your browser",
        "Captcha",
    ] {
        assert!(blocked_body(body.as_bytes()));
    }
    assert!(!blocked_body(b"{\"headline\":\"Market opens\"}"));
    assert!(valid_email("person@example.test"));
    for email in ["", "missing-at", "a@localhost", "a@b.c\r\nInjected", "a@@b.c", "(a)@b.c"] {
        assert!(!valid_email(email));
    }
}

// ---- Review regressions (all active). ----

#[tokio::test]
async fn review_short_retry_after_keeps_backoff_floor() {
    // Match congress::db::refuse_source: max(escalation, Retry-After).
    for (code, header) in [(429, "Retry-After: 0\r\n"), (403, "Retry-After: 1\r\n")] {
        let temp = Temp::new();
        let server = Server::new(code, header, "refused");
        let guard = server.attach(temp.guard(false));
        let before = Utc::now().timestamp();
        let Err(Refused::Paused { until }) = guard.send("finnhub", request()).await else { panic!("expected pause") };
        assert!(until >= before + 21600, "HTTP {code} with {header:?} paused only {}s", until - before);
    }
}

#[tokio::test]
async fn review_legit_news_mentioning_captcha_is_not_a_block() {
    let temp = Temp::new();
    let body = r#"{"news":[{"title":"Exchange adds CAPTCHA after access denied incidents"}]}"#;
    let server = Server::new(200, "", body);
    let guard = server.attach(temp.guard(false));
    let request = reqwest::Client::new().get("https://query1.finance.yahoo.com/v1/finance/search?q=COIN");
    assert_eq!(guard.send("yahoo-news", request).await, Ok(body.as_bytes().to_vec()));
    assert!(guard.status().unwrap()[2].paused_until.is_none());
}

#[tokio::test]
async fn review_yahoo_news_keeps_existing_user_agent() {
    let temp = Temp::new();
    let server = Server::new(200, "", "{}");
    let guard = server.attach(temp.guard(false));
    // YahooProvider sets this UA as a client default; RequestBuilder::build() does not carry it.
    let client = reqwest::Client::builder().user_agent("Mozilla/5.0 (compatible; terminal-rs)").build().unwrap();
    let request = client.get("https://query1.finance.yahoo.com/v1/finance/search?q=AAPL");
    guard.send("yahoo-news", request).await.unwrap();
    let seen = server.requests.lock().unwrap()[0].to_ascii_lowercase();
    assert!(seen.contains("user-agent: mozilla/5.0 (compatible; terminal-rs"), "{seen}");
}

#[tokio::test]
async fn review_back_to_back_coindesk_news_is_not_refused() {
    let temp = Temp::new();
    let server = Server::new(200, "", "<rss></rss>");
    let guard = server.attach(temp.guard(false));
    let feed = || reqwest::Client::new().get("https://www.coindesk.com/arc/outboundfeeds/rss/");
    guard.send("coindesk", feed()).await.unwrap();
    // E.g. a BTC and an ETH news pane open at once, or the user switches the pane's symbol.
    assert!(guard.send("coindesk", feed()).await.is_ok(), "second crypto news request refused");
    assert_eq!(server.hits.load(Ordering::SeqCst), 1, "must be served without a second request");
}

#[test]
fn paths_are_absolute_and_shared_open_retries_are_throttled() {
    for key in ["HOME", "XDG_DATA_HOME", "TR_NET_LEDGER"] {
        for value in ["", "relative/path"] {
            assert_eq!(ledger_path_with(&|k| (k == key).then(|| value.into())), Err(Refused::Ledger));
        }
    }
    let now = Instant::now();
    assert!(opening_due(None, now));
    assert!(!opening_due(Some(now), now + Duration::from_secs(59)));
    assert!(opening_due(Some(now), now + Duration::from_secs(60)));
    let config =
        Config { ledger: "relative.sqlite3".into(), data_dir: None, allow_external: true, max_wait: Duration::ZERO };
    assert!(matches!(Guard::new(config, Settings::default()), Err(Refused::Ledger)));
}

#[test]
fn shared_startup_failure_can_recover_after_one_minute() {
    let temp = Temp::new();
    let ledger = temp.0.join("blocked-ledger");
    std::fs::create_dir_all(&ledger).unwrap();
    let slot = OnceLock::new();
    let retry = Mutex::new(None);
    let attempts = AtomicUsize::new(0);
    let open = || {
        attempts.fetch_add(1, Ordering::SeqCst);
        Guard::new(
            Config {
                ledger: ledger.clone(),
                data_dir: Some(temp.0.clone()),
                allow_external: false,
                max_wait: Duration::ZERO,
            },
            Settings::default(),
        )
    };
    assert!(matches!(shared_with(&slot, &retry, open), Err(Refused::Ledger)));
    std::fs::remove_dir(&ledger).unwrap();
    assert!(matches!(shared_with(&slot, &retry, open), Err(Refused::Ledger)));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    *retry.lock().unwrap() = Some(Instant::now() - Duration::from_secs(60));
    assert!(shared_with(&slot, &retry, open).is_ok());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert!(shared_with(&slot, &retry, open).is_ok());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn ordinary_http_errors_are_not_content_cached() {
    for code in [400, 500] {
        let temp = Temp::new();
        let server = Server::sequence(&[(code, "", "not content"), (200, "", "<rss/>")], Duration::ZERO);
        let guard = server.attach(temp.guard(false));
        let feed = || reqwest::Client::new().get("https://www.coindesk.com/feed");
        assert_eq!(guard.send("coindesk", feed()).await, Err(Refused::Http(code)));
        age(&guard, "coindesk");
        assert_eq!(guard.send("coindesk", feed()).await.unwrap(), b"<rss/>");
        assert_eq!(server.hits.load(Ordering::SeqCst), 2);
    }
}

#[test]
fn corrupt_source_settings_fail_closed_and_email_is_header_safe() {
    for bytes in [
        br#"{"data_sources":{"sec":"yes"}}"#.as_slice(),
        br#"{"data_sources":null}"#,
        br#"{"data_sources":false}"#,
        b"{",
        b"[]",
    ] {
        let settings = Settings::from_document(bytes);
        assert!(settings.settings_error);
        assert!(GROUPS.iter().all(|group| !settings.enabled(group)));
        let temp = Temp::new();
        let guard = temp.guard(false);
        guard.configure(settings);
        assert!(guard.status().unwrap().iter().all(|s| !s.enabled && s.reason.as_deref().unwrap().contains("Corrupt")));
    }
    assert!(Settings::from_document(b"{}").enabled("finnhub"));
    for email in ["é@example.test", "a@é.test", "a\0@example.test", "a@example.test\r\nX: y", "a\u{7f}@example.test"]
    {
        assert!(!valid_email(email), "{email:?}");
    }
}

#[test]
fn lease_release_only_removes_its_own_reservation() {
    let temp = Temp::new();
    let guard = temp.guard(false);
    assert!(guard.reserve_owned(&policies()[0], "old").unwrap().is_none());
    let old = Lease { guard: &guard, group: "finnhub", owner: "old".into() };
    guard.db().unwrap().execute("UPDATE inflight SET until=0", []).unwrap();
    age(&guard, "finnhub");
    assert!(guard.reserve_owned(&policies()[0], "new").unwrap().is_none());
    drop(old);
    let owner: String = guard.db().unwrap().query_row("SELECT owner FROM inflight", [], |r| r.get(0)).unwrap();
    assert_eq!(owner, "new");
    drop(Lease { guard: &guard, group: "finnhub", owner: "new".into() });
    let count: u32 = guard.db().unwrap().query_row("SELECT count(*) FROM inflight", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn busy_cooldown_write_retries_and_keeps_local_escalation() {
    let temp = Temp::new();
    let guard = temp.guard(false);
    guard.pause("finnhub", "first", None).unwrap();
    let mut db = guard.db().unwrap();
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
    let before = Utc::now().timestamp();
    let start = Instant::now();
    let Refused::Paused { until } = guard.pause("finnhub", "busy", Some("0")).unwrap() else { panic!() };
    assert!(until >= before + 43200);
    assert!(start.elapsed() < Duration::from_secs(2));
    // The in-process pause is consulted before even trying the locked ledger.
    assert_eq!(guard.reserve(&policies()[0]), Err(Refused::Paused { until }));
    tx.rollback().unwrap();
    assert_eq!(guard.status().unwrap()[0].paused_until, Some(until));
    // A shorter write lock is retried and persisted, not just kept locally.
    let path = guard.config.ledger.clone();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let gate = barrier.clone();
    let lock = thread::spawn(move || {
        let mut db = Connection::open(path).unwrap();
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
        gate.wait();
        thread::sleep(Duration::from_millis(90));
        tx.rollback().unwrap();
    });
    barrier.wait();
    guard.pause("sec", "brief lock", None).unwrap();
    lock.join().unwrap();
    assert!(guard.cooldowns.read().unwrap()["sec"].durable);
}

#[tokio::test]
async fn html_only_markers_and_success_reset_strikes() {
    for mime in ["application/json", "application/rss+xml", "application/atom+xml", "text/xml", "application/xml"] {
        let temp = Temp::new();
        let server = Server::new(200, &format!("Content-Type: {mime}\r\n"), " <html>captcha</html>");
        let guard = server.attach(temp.guard(false));
        guard.send("finnhub", request()).await.unwrap();
    }
    for (headers, body) in
        [("Content-Type: text/html; charset=utf-8\r\n", "CAPTCHA"), ("", " \n<!DOCTYPE HTML><p>Access denied</p>")]
    {
        let temp = Temp::new();
        let server = Server::new(200, headers, body);
        let guard = server.attach(temp.guard(false));
        assert!(matches!(guard.send("finnhub", request()).await, Err(Refused::Paused { .. })));
    }
    let temp = Temp::new();
    let server = Server::sequence(
        &[(429, "", ""), (200, "Content-Type: application/json\r\n", "{\"captcha\":true}"), (429, "", "")],
        Duration::ZERO,
    );
    let guard = server.attach(temp.guard(false));
    assert!(matches!(guard.send("finnhub", request()).await, Err(Refused::Paused { .. })));
    guard.db().unwrap().execute("UPDATE cooldowns SET until=0", []).unwrap();
    age(&guard, "finnhub");
    guard.send("finnhub", request()).await.unwrap();
    age(&guard, "finnhub");
    let before = Utc::now().timestamp();
    let Err(Refused::Paused { until }) = guard.send("finnhub", request()).await else { panic!() };
    assert!((before + 21600..=Utc::now().timestamp() + 21600).contains(&until));
}

#[tokio::test]
async fn cache_coalesces_expires_and_never_touches_ledger_on_hits() {
    let temp = Temp::new();
    let server = Server::delayed(200, "", "<rss/>", Duration::from_millis(100));
    let guard = Arc::new(server.attach(temp.guard(false)));
    let feed = || reqwest::Client::new().get("https://www.coindesk.com/feed");
    let (one, two, three) =
        tokio::join!(guard.send("coindesk", feed()), guard.send("coindesk", feed()), guard.send("coindesk", feed()));
    assert_eq!(one.unwrap(), b"<rss/>");
    assert_eq!(two.unwrap(), b"<rss/>");
    assert_eq!(three.unwrap(), b"<rss/>");
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    let mut db = guard.db().unwrap();
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
    guard.send("coindesk", feed()).await.unwrap();
    tx.rollback().unwrap();
    assert_eq!(guard.status().unwrap()[3].requests_today, 1);
    guard.configure(Settings { coindesk: false, ..Default::default() });
    assert_eq!(guard.send("coindesk", feed()).await, Err(Refused::Disabled));
    guard.configure(Settings::default());
    let slot = guard.cache.lock().unwrap().values().next().unwrap().clone();
    slot.result.lock().unwrap().as_mut().unwrap().0 = Instant::now() - Duration::from_secs(301);
    assert_eq!(guard.send("coindesk", feed()).await, Err(Refused::Budget));
    age(&guard, "coindesk");
    guard.send("coindesk", feed()).await.unwrap();
    assert_eq!(server.hits.load(Ordering::SeqCst), 2);
    guard.set_cache_ttl("coindesk", Duration::ZERO).unwrap();
    assert_eq!(guard.send("coindesk", feed()).await, Err(Refused::Budget));
}

#[tokio::test]
async fn failures_are_not_cached_as_content_and_waiters_share_refusal() {
    let temp = Temp::new();
    let server = Server::sequence(&[(429, "", "refused"), (200, "", "<rss/>")], Duration::from_millis(30));
    let guard = server.attach(temp.guard(false));
    let feed = || reqwest::Client::new().get("https://www.coindesk.com/feed");
    let (one, two) = tokio::join!(guard.send("coindesk", feed()), guard.send("coindesk", feed()));
    assert!(matches!(one, Err(Refused::Paused { .. })));
    assert_eq!(one, two);
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    guard.db().unwrap().execute("UPDATE cooldowns SET until=0", []).unwrap();
    age(&guard, "coindesk");
    assert_eq!(guard.send("coindesk", feed()).await.unwrap(), b"<rss/>");
    assert_eq!(server.hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn different_yahoo_queries_queue_within_thirty_seconds() {
    let temp = Temp::new();
    let server = Server::delayed(200, "", "{}", Duration::from_millis(30));
    let mut guard = server.attach(temp.guard(false));
    guard.config.max_wait = Duration::from_secs(5);
    let query = |symbol| reqwest::Client::new().get("https://query1.finance.yahoo.com/search").query(&[("q", symbol)]);
    let start = Instant::now();
    let (one, two, three) = tokio::join!(
        guard.send("yahoo-news", query("AAPL")),
        guard.send("yahoo-news", query("COIN")),
        guard.send("yahoo-news", query("MSFT"))
    );
    one.unwrap();
    two.unwrap();
    three.unwrap();
    assert!(start.elapsed() >= Duration::from_secs(10));
    assert!(start.elapsed() < Duration::from_secs(30));
    assert_eq!(server.hits.load(Ordering::SeqCst), 3);
    assert_eq!(guard.status().unwrap()[2].requests_today, 3);
    guard.send("yahoo-news", query("AAPL")).await.unwrap();
    assert_eq!(server.hits.load(Ordering::SeqCst), 3);
}

fn fast_redirects(guard: &Guard) {
    for policy in guard.policies.write().unwrap().iter_mut() {
        policy.min_interval = Duration::ZERO;
        policy.per_minute = 100_000;
    }
}
#[tokio::test]
async fn redirects_are_same_group_guarded_counted_and_bounded() {
    let temp = Temp::new();
    let server = Server::sequence(
        &[
            (302, "Location: https://query2.finance.yahoo.com/next\r\n", ""),
            (307, "Location: /final\r\n", ""),
            (200, "", "{}"),
        ],
        Duration::ZERO,
    );
    let guard = server.attach(temp.guard(false));
    fast_redirects(&guard);
    guard.send("yahoo-news", reqwest::Client::new().get("https://query1.finance.yahoo.com/start")).await.unwrap();
    assert_eq!(server.hits.load(Ordering::SeqCst), 3);
    assert_eq!(guard.status().unwrap()[2].requests_today, 3);
    let server = Server::new(302, "Location: https://finnhub.io/loop?token=secret\r\n", "");
    let guard = server.attach(temp.guard(false));
    fast_redirects(&guard);
    assert_eq!(guard.send("finnhub", request()).await, Err(Refused::Redirect("finnhub.io".into())));
    assert_eq!(server.hits.load(Ordering::SeqCst), 4);
    assert_eq!(guard.status().unwrap()[0].requests_today, 4);
    let server = Server::new(302, "Location: https://data.sec.gov/no?token=secret\r\n", "");
    let guard = server.attach(temp.guard(false));
    fast_redirects(&guard);
    let error = guard.send("finnhub", request()).await.unwrap_err();
    assert_eq!(error, Refused::Redirect("data.sec.gov".into()));
    assert!(!error.to_string().contains("secret"));
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
}

#[test]
fn past_and_long_retry_after_respect_floor_and_cap() {
    let temp = Temp::new();
    let guard = temp.guard(false);
    let past = chrono::DateTime::from_timestamp(0, 0).unwrap().to_rfc2822();
    for (retry, seconds) in [(past.as_str(), 21600), ("0", 43200), ("1", 86400), ("9999999999", 604800)] {
        let before = Utc::now().timestamp();
        let Refused::Paused { until } = guard.pause("finnhub", "fixture", Some(retry)).unwrap() else { panic!() };
        assert!((before + seconds..=Utc::now().timestamp() + seconds).contains(&until));
    }
}
