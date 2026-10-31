use super::*;
use crate::{Config, Settings};
use reqwest::dns::{Name, Resolve, Resolving};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data/publisher").join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn guard(&self, offline: bool, settings: Settings) -> Guard {
        let config = Config {
            ledger: self.0.join("ledger.sqlite3"),
            data_dir: offline.then(|| self.0.join("data")),
            allow_external: !offline,
            max_wait: Duration::ZERO,
        };
        Guard::new(config, settings).unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}
impl Reply {
    fn new(status: u16) -> Self {
        Self { status, headers: Vec::new(), body: Vec::new() }
    }
    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
    fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }
    fn html(body: impl Into<Vec<u8>>) -> Self {
        Self::new(200).header("Content-Type", "text/html; charset=utf-8").body(body)
    }
    fn plain(body: impl Into<Vec<u8>>) -> Self {
        Self::new(200).header("Content-Type", "text/plain").body(body)
    }
    fn redirect(location: &str) -> Self {
        Self::new(302).header("Location", location)
    }
}

struct Server {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    paths: Arc<Mutex<Vec<String>>>,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(replies: Vec<Reply>) -> Self {
        Self::with_robots(Reply::plain(""), replies)
    }
    fn with_robots(robots: Reply, replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let paths = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (h, p, r, s) = (hits.clone(), paths.clone(), requests.clone(), stop.clone());
        let worker = thread::spawn(move || {
            let mut remaining: VecDeque<Reply> = replies.into();
            while !s.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream.set_read_timeout(Some(Duration::from_millis(400))).unwrap();
                        let mut bytes = Vec::new();
                        let mut buf = [0; 4096];
                        while !bytes.ends_with(b"\r\n\r\n") {
                            match stream.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => bytes.extend_from_slice(&buf[..n]),
                            }
                        }
                        h.fetch_add(1, Ordering::SeqCst);
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        let path = text
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .to_owned();
                        p.lock().unwrap().push(path.clone());
                        r.lock().unwrap().push(text);
                        let reply = if path.starts_with("/robots.txt") {
                            robots.clone()
                        } else if remaining.len() > 1 {
                            remaining.pop_front().unwrap()
                        } else {
                            remaining.front().cloned().unwrap_or_else(|| Reply::new(404))
                        };
                        let mut raw = format!(
                            "HTTP/1.1 {} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n",
                            reply.status,
                            reply.body.len()
                        );
                        for (name, value) in &reply.headers {
                            raw.push_str(&format!("{name}: {value}\r\n"));
                        }
                        raw.push_str("\r\n");
                        let mut out = raw.into_bytes();
                        out.extend_from_slice(&reply.body);
                        let _ = stream.write_all(&out);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self { addr, hits, paths, requests, stop, worker: Some(worker) }
    }
    fn article_paths(&self) -> Vec<String> {
        self.paths.lock().unwrap().iter().filter(|path| !path.starts_with("/robots.txt")).cloned().collect()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

struct MapResolver {
    addr: SocketAddr,
    names: Arc<Mutex<Vec<String>>>,
}
impl Resolve for MapResolver {
    fn resolve(&self, name: Name) -> Resolving {
        self.names.lock().unwrap().push(name.as_str().to_owned());
        let addrs: Addrs = Box::new(std::iter::once(self.addr));
        Box::pin(async move { Ok(addrs) })
    }
}

fn client_for(server: &Server, names: &Arc<Mutex<Vec<String>>>) -> reqwest::Client {
    build_client(Arc::new(MapResolver { addr: server.addr, names: names.clone() })).unwrap()
}

fn attach<'a>(guard: &'a Guard, server: &Server) -> (Publisher<'a>, Arc<Mutex<Vec<String>>>) {
    let names = Arc::new(Mutex::new(Vec::new()));
    let client = client_for(server, &names);
    (Publisher::with_client(guard, client), names)
}

const PAGE: &str = "<!doctype html><html><body><article><p>Story</p></article></body></html>";

#[test]
fn resolver_filter_rejects_every_non_public_range() {
    for ip in [
        "127.0.0.1",
        "10.0.0.1",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.0.1",
        "169.254.1.1",
        "169.254.169.254",
        "100.64.0.1",
        "100.127.255.255",
        "0.0.0.0",
        "0.1.2.3",
        "255.255.255.255",
        "224.0.0.1",
        "239.255.255.255",
        "192.0.2.1",
        "198.51.100.1",
        "203.0.113.1",
        "198.18.0.1",
        "198.19.255.255",
        "192.0.0.8",
        "240.0.0.1",
    ] {
        let ip: IpAddr = ip.parse().unwrap();
        assert!(!public_address(ip), "{ip} must be rejected");
    }
    for ip in ["8.8.8.8", "1.1.1.1", "93.184.216.34", "203.0.114.1", "198.20.0.1"] {
        let ip: IpAddr = ip.parse().unwrap();
        assert!(public_address(ip), "{ip} must be allowed");
    }
    for ip in [
        "::1",
        "::",
        "fc00::1",
        "fd12:3456::1",
        "fe80::1",
        "fec0::1",
        "ff02::1",
        "2001:db8::1",
        "2001::1",
        "2002:7f00:1::1",
        "::ffff:127.0.0.1",
        "::ffff:10.0.0.1",
        "::ffff:169.254.169.254",
        "64:ff9b::7f00:1",
        "64:ff9b::a00:1",
        "::7f00:1",
    ] {
        let ip: IpAddr = ip.parse().unwrap();
        assert!(!public_address(ip), "{ip} must be rejected");
    }
    for ip in ["2606:4700:4700::1111", "::ffff:8.8.8.8", "64:ff9b::808:808"] {
        let ip: IpAddr = ip.parse().unwrap();
        assert!(public_address(ip), "{ip} must be allowed");
    }
}

#[tokio::test]
async fn url_checks_refuse_before_any_request_or_resolution() {
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::new(vec![Reply::html(PAGE)]);
    let (publisher, names) = attach(&guard, &server);
    for (url, expected) in [
        ("https://user:pass@news.fixture/story", Reason::InvalidUrl),
        ("https://news.fixture:8080/story", Reason::InvalidUrl),
        ("http://127.0.0.1/story", Reason::InvalidUrl),
        ("http://[::1]/story", Reason::InvalidUrl),
        ("http://[::ffff:169.254.169.254]/story", Reason::InvalidUrl),
        ("http://[fe80::1%25eth0]/story", Reason::InvalidUrl),
        ("file:///etc/passwd", Reason::InvalidUrl),
        ("data:text/html,fixture", Reason::InvalidUrl),
        ("ftp://news.fixture/story", Reason::InvalidUrl),
        ("https://senate.gov/story", Reason::Skipped),
        ("https://www.senate.gov/story", Reason::Skipped),
        ("https://www.bloomberg.com/story", Reason::Skipped),
        ("https://asia.nikkei.com/story", Reason::Skipped),
        ("https://finnhub.io/api/news?id=1&token=secret", Reason::KeyedLink),
    ] {
        assert_eq!(publisher.fetch(url).await, Err(expected), "{url}");
    }
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
    assert!(names.lock().unwrap().is_empty(), "validation must precede DNS");
}

#[tokio::test]
async fn redirects_to_private_addresses_are_refused_without_resolution() {
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::new(vec![
        Reply::redirect("http://127.0.0.1/story"),
        Reply::redirect("http://169.254.169.254/latest/meta-data/"),
    ]);
    let (publisher, names) = attach(&guard, &server);
    assert_eq!(publisher.fetch("http://news.fixture/first").await, Err(Reason::InvalidUrl));
    // The host slot would defer a second fetch; this fixture asserts the redirect path instead.
    guard.release_slot("publisher:news.fixture").unwrap();
    assert_eq!(publisher.fetch("http://news.fixture/second").await, Err(Reason::InvalidUrl));
    assert_eq!(server.article_paths(), ["/first", "/second"]);
    let names = names.lock().unwrap().clone();
    assert!(!names.is_empty());
    assert!(names.iter().all(|name| name == "news.fixture"), "unexpected DNS names: {names:?}");
}

#[tokio::test]
async fn response_checks_refuse_bad_bodies_types_and_redirects() {
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::new(vec![Reply::html("a".repeat(MAX_BODY + 1))]);
    let (publisher, _) = attach(&guard, &server);
    assert_eq!(publisher.fetch("http://news.fixture/huge").await, Err(Reason::TooLarge));

    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::new(vec![Reply::new(200).header("Content-Type", "application/json").body("{}")]);
    let (publisher, _) = attach(&guard, &server);
    assert_eq!(publisher.fetch("http://news.fixture/json").await, Err(Reason::NotHtml));

    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::new(vec![Reply::new(302)]);
    let (publisher, _) = attach(&guard, &server);
    assert_eq!(publisher.fetch("http://news.fixture/missing").await, Err(Reason::Redirect));

    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::new(vec![Reply::redirect("../relative"), Reply::html(PAGE)]);
    let (publisher, _) = attach(&guard, &server);
    let fetched = publisher.fetch("http://news.fixture/dir/start").await.unwrap();
    assert_eq!(fetched.url, "http://news.fixture/relative");
    assert_eq!(server.article_paths(), ["/dir/start", "/relative"]);

    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let target = "http://news.fixture/hop";
    let server = Server::new(vec![Reply::redirect(target); 6]);
    let (publisher, _) = attach(&guard, &server);
    assert_eq!(publisher.fetch("http://news.fixture/start").await, Err(Reason::Redirect));
    assert_eq!(server.article_paths().len(), 6);
}

#[tokio::test]
async fn publisher_client_ignores_https_proxy_from_the_environment() {
    static ENV: Mutex<()> = Mutex::new(());
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::new(vec![Reply::html(PAGE)]);
    let sink = Sink::new();
    let (client, names) = {
        let _serial = ENV.lock().unwrap();
        // SAFETY: test-only environment mutation, serialized by ENV and restored immediately.
        unsafe { std::env::set_var("HTTPS_PROXY", &sink.url) };
        let names = Arc::new(Mutex::new(Vec::new()));
        let client = client_for(&server, &names);
        unsafe { std::env::remove_var("HTTPS_PROXY") };
        (client, names)
    };
    let publisher = Publisher::with_client(&guard, client);
    assert!(publisher.fetch("https://news.fixture/story").await.is_err());
    assert_eq!(sink.hits.load(Ordering::SeqCst), 0, "the publisher client must bypass proxy env");
    let names = names.lock().unwrap().clone();
    assert_eq!(names, ["news.fixture"]);
}

#[tokio::test]
async fn host_spacing_daily_cap_and_rate_limit_cooldown_are_ledger_backed() {
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::new(vec![Reply::html(PAGE), Reply::html(PAGE)]);
    let (publisher, _) = attach(&guard, &server);
    assert!(publisher.fetch("http://news.fixture/one").await.is_ok());
    assert_eq!(publisher.fetch("http://news.fixture/two").await, Err(Reason::Pacing));
    assert_eq!(server.article_paths(), ["/one"]);

    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::new(vec![Reply::html(PAGE)]);
    let (publisher, _) = attach(&guard, &server);
    let day = format!("publisher:day:{}", Utc::now().format("%Y%m%d"));
    for _ in 0..DAILY_CAP {
        assert!(guard.reserve_counter(&day, DAILY_CAP).unwrap());
    }
    assert_eq!(publisher.fetch("http://news.fixture/capped").await, Err(Reason::Budget));
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);

    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::new(vec![Reply::new(429).header("Retry-After", "120")]);
    let (publisher, _) = attach(&guard, &server);
    assert_eq!(publisher.fetch("http://news.fixture/limited").await, Err(Reason::Refused(429)));
    assert_eq!(publisher.fetch("http://news.fixture/limited").await, Err(Reason::Cooldown));
    assert_eq!(server.article_paths(), ["/limited"]);
}

#[tokio::test]
async fn robots_rules_and_failures_use_the_summary_instead_of_fetching() {
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::with_robots(Reply::plain("User-agent: *\nDisallow: /\n"), vec![Reply::html(PAGE)]);
    let (publisher, _) = attach(&guard, &server);
    assert_eq!(publisher.fetch("http://news.fixture/story").await, Err(Reason::Robots));
    assert!(server.article_paths().is_empty());

    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::with_robots(Reply::new(500), vec![Reply::html(PAGE)]);
    let (publisher, _) = attach(&guard, &server);
    assert_eq!(publisher.fetch("http://news.fixture/story").await, Err(Reason::RobotsUnavailable));
    assert!(server.article_paths().is_empty());

    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::with_robots(Reply::plain("User-agent: *\nDisallow: /private\n"), vec![Reply::html(PAGE)]);
    let (publisher, _) = attach(&guard, &server);
    assert!(publisher.fetch("http://news.fixture/story").await.is_ok());
}

#[test]
fn robots_parser_honours_specific_agent_wildcards_and_anchors() {
    assert!(robots_decide("", "/story").is_ok());
    assert!(robots_decide("User-agent: *\nDisallow: /private\n", "/story").is_ok());
    assert!(robots_decide("User-agent: *\nDisallow: /private\n", "/private/x").is_err());
    assert!(robots_decide("User-agent: terminal-rs\nDisallow: /\n", "/story").is_err());
    assert!(robots_decide("User-agent: terminal-rs\nDisallow:\n\nUser-agent: *\nDisallow: /\n", "/story").is_ok());
    assert!(robots_decide("User-agent: other\nDisallow: /\n\nUser-agent: *\nDisallow: /private\n", "/story").is_ok());
    assert!(robots_decide("User-agent: *\nDisallow: /*.pdf$\n", "/story.pdf").is_err());
    assert!(robots_decide("User-agent: *\nDisallow: /*.pdf$\n", "/story.pdf?x=1").is_ok());
    assert!(robots_decide("User-agent: *\nDisallow: /a\nAllow: /a/b\n", "/a/b/c").is_ok());
    assert!(robots_decide("User-agent: *\nDisallow: /a\nAllow: /a/b\n", "/a/c").is_err());
}

#[tokio::test]
async fn offline_and_disabled_settings_never_contact_the_publisher() {
    let temp = Temp::new();
    let server = Server::new(vec![Reply::html(PAGE)]);
    let offline = temp.guard(true, Settings::default());
    let (publisher, _) = attach(&offline, &server);
    assert_eq!(publisher.fetch("http://news.fixture/story").await, Err(Reason::Offline));

    let temp = Temp::new();
    let disabled = temp.guard(false, Settings { publisher: false, ..Default::default() });
    let (publisher, _) = attach(&disabled, &server);
    assert_eq!(publisher.fetch("http://news.fixture/story").await, Err(Reason::Disabled));
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn request_has_no_cookie_or_provider_credentials_and_marks_noarchive() {
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::new(vec![Reply::html(PAGE).header("X-Robots-Tag", "noindex, noarchive")]);
    let (publisher, _) = attach(&guard, &server);
    let fetched = publisher.fetch("http://news.fixture/story").await.unwrap();
    assert!(fetched.noarchive);
    let requests = server.requests.lock().unwrap();
    let article = requests.iter().find(|request| request.contains("GET /story")).unwrap();
    let lower = article.to_ascii_lowercase();
    assert!(!lower.contains("authorization:"));
    assert!(!lower.contains("x-api-key:"));
    assert!(!lower.contains("cookie:"));
}

struct Sink {
    url: String,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Sink {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (h, s) = (hits.clone(), stop.clone());
        let worker = thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        h.fetch_add(1, Ordering::SeqCst);
                        let _ =
                            stream.write_all(b"HTTP/1.1 502 Fixture\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self { url, hits, stop, worker: Some(worker) }
    }
}
impl Drop for Sink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

#[tokio::test]
async fn n8_cross_host_redirect_fetches_target_robots_before_target_page() {
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::with_robots(
        Reply::plain("User-agent: *\nDisallow: /private\n"),
        vec![Reply::redirect("//target.fixture/private/story"), Reply::html(PAGE)],
    );
    let (publisher, names) = attach(&guard, &server);
    assert_eq!(publisher.fetch("http://news.fixture/entry").await, Err(Reason::Robots));
    assert_eq!(server.article_paths(), ["/entry"]);
    assert_eq!(*server.paths.lock().unwrap(), ["/robots.txt", "/entry", "/robots.txt"]);
    assert!(names.lock().unwrap().iter().any(|name| name == "target.fixture"));
}

#[tokio::test]
async fn n8_robots_failures_are_cached_for_one_hour_and_take_one_host_slot() {
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::with_robots(Reply::new(503), vec![Reply::html(PAGE)]);
    let (publisher, _) = attach(&guard, &server);
    let now = Utc::now().timestamp();
    assert_eq!(publisher.fetch("http://news.fixture/story").await, Err(Reason::RobotsUnavailable));
    let mut entry = guard.robots_cached("news.fixture").unwrap();
    assert!((now + 3600..=Utc::now().timestamp() + 3600).contains(&entry.expires_at));
    assert!(guard.slot_until("publisher:news.fixture").unwrap().is_some());
    assert_eq!(publisher.fetch("http://news.fixture./story").await, Err(Reason::RobotsUnavailable));
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    entry.expires_at = now - 1;
    guard.cache_robots("news.fixture", entry);
    // An expired robots entry still cannot bypass the host's outstanding 10-second slot.
    assert_eq!(publisher.fetch("http://news.fixture/story").await, Err(Reason::Pacing));
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    guard.release_slot("publisher:news.fixture").unwrap();
    assert_eq!(publisher.fetch("http://news.fixture/story").await, Err(Reason::RobotsUnavailable));
    assert_eq!(server.hits.load(Ordering::SeqCst), 2);
}

struct FailingResolver(Arc<AtomicUsize>);
impl Resolve for FailingResolver {
    fn resolve(&self, _: Name) -> Resolving {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(io_error("fixture resolution failure")) })
    }
}

#[tokio::test]
async fn n8_robots_transport_failure_is_cached_without_network() {
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let client = build_client(Arc::new(FailingResolver(calls.clone()))).unwrap();
    let publisher = Publisher::with_client(&guard, client);
    assert_eq!(publisher.fetch("http://news.fixture/story").await, Err(Reason::RobotsUnavailable));
    assert_eq!(publisher.fetch("http://news.fixture/story").await, Err(Reason::RobotsUnavailable));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let entry = guard.robots_cached("news.fixture").unwrap();
    assert!(matches!(entry.result, Err(Reason::RobotsUnavailable)));
    assert!((Utc::now().timestamp() + 3590..=Utc::now().timestamp() + 3600).contains(&entry.expires_at));
}

#[tokio::test]
async fn n8_robots_and_relative_page_hops_share_one_slot_and_canonical_cache() {
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::new(vec![Reply::redirect("//NEWS.fixture./next"), Reply::html(PAGE)]);
    let (publisher, _) = attach(&guard, &server);
    assert!(publisher.fetch("http://NEWS.fixture./start").await.is_ok());
    assert_eq!(*server.paths.lock().unwrap(), ["/robots.txt", "/start", "/next"]);
    assert_eq!(publisher.fetch("http://news.fixture/again").await, Err(Reason::Pacing));
    assert_eq!(server.hits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn n8_protocol_relative_ip_and_overlong_robots_path_are_refused() {
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::new(vec![Reply::redirect("//127.0.0.1/private")]);
    let (publisher, names) = attach(&guard, &server);
    assert_eq!(publisher.fetch("http://news.fixture/start").await, Err(Reason::InvalidUrl));
    assert_eq!(server.article_paths(), ["/start"]);
    assert!(names.lock().unwrap().iter().all(|name| name == "news.fixture"));
    let path = format!("/{}", "a".repeat(MAX_ROBOTS_PATH));
    assert_eq!(robots_decide("", &path), Err(Reason::RobotsUnavailable));
    assert_eq!(publisher.fetch(&format!("http://news.fixture{path}")).await, Err(Reason::RobotsUnavailable));
    assert_eq!(server.hits.load(Ordering::SeqCst), 2);
}

#[test]
fn n8_redirect_join_rejects_https_downgrades_and_missing_location() {
    let current = reqwest::Url::parse("https://news.fixture/dir/start").unwrap();
    assert_eq!(redirect_target(&current, Some("http://news.fixture/plain")), Err(Reason::Redirect));
    assert_eq!(redirect_target(&current, None), Err(Reason::Redirect));
    assert_eq!(redirect_target(&current, Some("../next")).unwrap().as_str(), "https://news.fixture/next");
    assert_eq!(redirect_target(&current, Some("//target.fixture/next")).unwrap().scheme(), "https");
}

#[tokio::test]
async fn n8_all_credential_query_keys_are_refused_before_io() {
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::new(vec![Reply::html(PAGE)]);
    let (publisher, names) = attach(&guard, &server);
    for key in KEY_PARAMS {
        let url = format!("http://news.fixture/story?{}=secret", key.to_ascii_uppercase());
        assert_eq!(publisher.fetch(&url).await, Err(Reason::KeyedLink));
    }
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
    assert!(names.lock().unwrap().is_empty());
}

#[tokio::test]
async fn review_n8_redirect_target_is_checked_against_robots() {
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::with_robots(
        Reply::plain("User-agent: *\nDisallow: /private\n"),
        vec![Reply::redirect("http://news.fixture/private/story"), Reply::html(PAGE)],
    );
    let (publisher, _) = attach(&guard, &server);
    assert_eq!(publisher.fetch("http://news.fixture/story").await, Err(Reason::Robots));
    assert_eq!(server.article_paths(), ["/story"], "the disallowed redirect target must not be requested");
}

#[tokio::test]
async fn review_n8_trailing_dot_host_shares_cooldown() {
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let server = Server::new(vec![Reply::new(429).header("Retry-After", "600"), Reply::html(PAGE)]);
    let (publisher, _) = attach(&guard, &server);
    assert_eq!(publisher.fetch("http://news.fixture/a").await, Err(Reason::Refused(429)));
    assert_eq!(publisher.fetch("http://news.fixture./a").await, Err(Reason::Cooldown));
    assert_eq!(server.article_paths(), ["/a"]);
}

#[tokio::test]
async fn review_n8_cached_robots_decision_matches_first_decision() {
    let temp = Temp::new();
    let guard = temp.guard(false, Settings::default());
    let mut robots = String::from("User-agent: *\n");
    while robots.len() < 300 * 1024 {
        robots.push_str("# padding padding padding padding padding padding padding padding\n");
    }
    robots.push_str("Disallow: /story\n");
    let server = Server::with_robots(Reply::plain(robots), vec![Reply::html(PAGE)]);
    let (publisher, _) = attach(&guard, &server);
    assert_eq!(publisher.fetch("http://news.fixture/story").await, Err(Reason::Robots));
    assert_eq!(publisher.fetch("http://news.fixture/story").await, Err(Reason::Robots));
    assert!(server.article_paths().is_empty());
    let cached = guard.robots_cached("news.fixture").unwrap().result.unwrap();
    assert_eq!(cached.text.len(), MAX_ROBOTS);
    assert!(cached.truncated);
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
}

#[test]
fn review_n8_resolver_rejects_other_ipv4_embedding_prefixes() {
    for ip in ["64:ff9b:1::a00:1", "64:ff9b:1::7f00:1", "::ffff:0:7f00:1", "::ffff:0:a9fe:a9fe"] {
        let ip: IpAddr = ip.parse().unwrap();
        assert!(!public_address(ip), "{ip} must be rejected");
    }
}

#[test]
fn review_n8_numeric_and_odd_ipv4_forms_normalise_to_rejected_literals() {
    for url in [
        "http://2130706433/",
        "http://0x7f.1/",
        "http://0177.0.0.1/",
        "http://127.1/",
        "http://0/",
        "HTTP://LOCALHOST:80/",
        "http://[::]/",
        "http://[::ffff:7f00:1]/",
        "http://169.254.169.254./",
        "http://xn--nxasmq6b.senate.gov./",
        "https://WWW.Senate.Gov./x",
        "http://news.fixture/?ApiKey=1",
        "http://news.fixture/?access%5Ftoken=1",
    ] {
        let parsed = reqwest::Url::parse(url).unwrap();
        if url.contains("LOCALHOST") {
            // A name, not a literal: validate passes and the resolver must drop 127.0.0.1.
            assert!(validate(&parsed).is_ok(), "{url}");
            continue;
        }
        assert!(validate(&parsed).is_err(), "{url} -> {parsed}");
    }
    assert!(public_addrs(vec!["127.0.0.1:0".parse().unwrap(), "[::1]:0".parse().unwrap()]).is_err());
}
