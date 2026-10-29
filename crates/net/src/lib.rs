//! Conservative, machine-wide budgets for news and filings, with guarded redirects and local caching.
#[cfg(test)]
mod tests;
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub const GROUPS: [&str; 10] = [
    "finnhub",
    "sec",
    "yahoo-news",
    "coindesk",
    "house-disclosures",
    "house-roster",
    "bargo",
    "regulators",
    "global-media",
    "tech",
];

fn host_budget(group: &str) -> bool {
    matches!(group, "regulators" | "global-media" | "tech")
}

/// Root classification, independent of MIME type. Skip declarations, processing
/// instructions and comments; never search inside HTML for a nested feed tag.
fn feed_signature(bytes: &[u8]) -> bool {
    let mut bytes = trim_signature_prefix(bytes);
    loop {
        let end = if bytes.starts_with(b"<?") {
            b"?>".as_slice()
        } else if bytes.starts_with(b"<!--") {
            b"-->".as_slice()
        } else if bytes.starts_with(b"<!DOCTYPE") {
            // Skip the declaration, including quoted '>' and internal subsets.
            // This is only signature detection: never resolve external entities.
            let mut quote = None;
            let mut depth = 0_u32;
            let Some(i) = bytes.iter().enumerate().skip(9).find_map(|(i, &b)| {
                if let Some(q) = quote {
                    if b == q {
                        quote = None;
                    }
                } else {
                    match b {
                        b'\'' | b'"' => quote = Some(b),
                        b'[' => depth += 1,
                        b']' => depth = depth.saturating_sub(1),
                        b'>' if depth == 0 => return Some(i),
                        _ => {}
                    }
                }
                None
            }) else {
                return false;
            };
            bytes = bytes[i + 1..].trim_ascii_start();
            continue;
        } else {
            break;
        };
        let Some(i) = bytes.windows(end.len()).position(|w| w == end) else { return false };
        bytes = bytes[i + end.len()..].trim_ascii_start();
    }
    let Some(bytes) = bytes.strip_prefix(b"<") else { return false };
    let name = bytes.split(|b| b.is_ascii_whitespace() || matches!(b, b'>' | b'/')).next().unwrap_or_default();
    let local = name.rsplit(|b| *b == b':').next().unwrap_or_default();
    bytes.get(name.len()).is_some() && matches!(local, b"rss" | b"feed" | b"RDF")
}

/// Permanent network prohibition, independent of switches, aliases and future policies.
fn disabled_host(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        let host = host.trim_end_matches('.');
        host == "senate.gov" || host.ends_with(".senate.gov")
    })
}
const BODY_CAP: usize = 8 * 1024 * 1024;

fn trim_signature_prefix(mut bytes: &[u8]) -> &[u8] {
    loop {
        bytes = bytes.trim_ascii_start();
        match bytes.strip_prefix(b"\xef\xbb\xbf") {
            Some(rest) => bytes = rest,
            None => return bytes,
        }
    }
}

fn house_roster_signature(bytes: &[u8]) -> bool {
    let mut text = String::from_utf8_lossy(bytes);
    if text.starts_with("<?xml") {
        let Some((_, rest)) = text.split_once("?>") else { return false };
        text = std::borrow::Cow::Owned(rest.trim_start().to_owned());
    }
    text.strip_prefix("<MemberData").is_some_and(|rest| rest.starts_with(['>', '/', ' ', '\t', '\r', '\n']))
}

#[derive(Clone, Debug)]
pub struct HostPolicy {
    pub host: &'static str,
    pub group: &'static str,
    pub min_interval: Duration,
    pub per_minute: u32,
    pub daily_cap: u32,
    pub concurrency: u8,
    /// Zero disables content caching; callers may configure later-phase sources.
    pub cache_ttl: Duration,
    pub user_agent: UserAgent,
}
#[derive(Clone, Debug)]
pub enum UserAgent {
    Research,
    SecContact,
    /// Existing Yahoo identity only; never use this for new source groups.
    YahooLegacy,
}
pub fn policies() -> Vec<HostPolicy> {
    [
        ("finnhub.io", "finnhub", 3, 20, 3000),
        ("www.sec.gov", "sec", 1, 60, 1500),
        ("data.sec.gov", "sec", 1, 60, 1500),
        ("efts.sec.gov", "sec", 1, 60, 1500),
        ("query1.finance.yahoo.com", "yahoo-news", 5, 12, 1000),
        ("query2.finance.yahoo.com", "yahoo-news", 5, 12, 1000),
        ("www.coindesk.com", "coindesk", 300, 1, 300),
        ("disclosures-clerk.house.gov", "house-disclosures", 3, 20, 400),
        ("clerk.house.gov", "house-roster", 3, 20, 20),
        ("www.bargo.ai", "bargo", 30, 2, 40),
        ("www.federalreserve.gov", "regulators", 900, 1, 120),
        ("www.ecb.europa.eu", "regulators", 900, 1, 120),
        ("www.bankofengland.co.uk", "regulators", 900, 1, 120),
        ("feeds.bbci.co.uk", "global-media", 900, 1, 120),
        ("search.cnbc.com", "global-media", 900, 1, 120),
        ("www.aljazeera.com", "global-media", 900, 1, 120),
        ("techcrunch.com", "tech", 1800, 1, 60),
        ("www.theverge.com", "tech", 1800, 1, 60),
        ("hnrss.org", "tech", 1800, 1, 60),
    ]
    .into_iter()
    .map(|(host, group, seconds, per_minute, daily_cap)| HostPolicy {
        host,
        group,
        min_interval: Duration::from_secs(seconds),
        per_minute,
        daily_cap,
        concurrency: 1,
        cache_ttl: match group {
            "coindesk" | "yahoo-news" => Duration::from_secs(300),
            "house-roster" => Duration::from_secs(86400),
            _ => Duration::ZERO,
        },
        user_agent: match group {
            "yahoo-news" => UserAgent::YahooLegacy,
            "sec" => UserAgent::SecContact,
            _ => UserAgent::Research,
        },
    })
    .collect()
}

/// Settings contain no API keys. Deliberately no Debug implementation (contact privacy).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub pause_all: bool,
    pub finnhub: bool,
    pub sec: bool,
    pub yahoo_news: bool,
    pub coindesk: bool,
    pub regulators: bool,
    pub global_media: bool,
    pub tech: bool,
    /// Absence means enabled, including when loading pre-N6 settings.
    pub disabled_feeds: std::collections::BTreeSet<String>,
    pub house_disclosures: bool,
    pub house_roster: bool,
    pub bargo: bool,
    /// Explicit opt-in: legacy `bargo: true` alone must not enable keyless traffic.
    pub bargo_keyless: bool,
    pub sec_contact: String,
    #[serde(skip)]
    pub settings_error: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            pause_all: false,
            finnhub: true,
            sec: true,
            yahoo_news: true,
            coindesk: true,
            regulators: true,
            global_media: true,
            tech: true,
            disabled_feeds: Default::default(),
            house_disclosures: true,
            house_roster: true,
            bargo: false,
            bargo_keyless: false,
            sec_contact: String::new(),
            settings_error: false,
        }
    }
}
impl Settings {
    pub fn corrupt() -> Self {
        Self {
            pause_all: true,
            finnhub: false,
            sec: false,
            yahoo_news: false,
            coindesk: false,
            house_disclosures: false,
            house_roster: false,
            bargo: false,
            settings_error: true,
            ..Self::default()
        }
    }
    /// Resolve secret-store presence before starting any worker. A missing switch defaults on
    /// only with a key; a legacy enabled switch without the new opt-in never enables keyless.
    pub fn resolve_bargo(&mut self, document: Option<&[u8]>, has_key: bool) {
        if self.settings_error {
            return;
        }
        let explicit = document
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok())
            .and_then(|doc| doc.get("data_sources")?.get("bargo")?.as_bool());
        self.bargo = explicit.unwrap_or(has_key) && (has_key || self.bargo_keyless);
    }
    /// Missing legacy section is supported; malformed settings always fail closed.
    pub fn from_document(bytes: &[u8]) -> Self {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else { return Self::corrupt() };
        if !value.is_object() {
            return Self::corrupt();
        }
        value.get("data_sources").map_or_else(Self::default, |section| Self::from_section(section.clone()))
    }
    pub fn from_section(value: serde_json::Value) -> Self {
        serde_json::from_value(value).unwrap_or_else(|_| Self::corrupt())
    }
    pub fn enabled(&self, group: &str) -> bool {
        !self.pause_all
            && !self.settings_error
            && match group {
                "finnhub" => self.finnhub,
                "sec" => self.sec,
                "yahoo-news" => self.yahoo_news,
                "coindesk" => self.coindesk,
                "regulators" => self.regulators,
                "global-media" => self.global_media,
                "tech" => self.tech,
                "house-disclosures" => self.house_disclosures,
                "house-roster" => self.house_roster,
                "bargo" => self.bargo,
                _ => false,
            }
    }
}
pub fn valid_email(email: &str) -> bool {
    email.is_ascii()
        && reqwest::header::HeaderValue::from_str(email).is_ok()
        && !email.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '(' | ')' | '<' | '>'))
        && email.split_once('@').is_some_and(|(name, domain)| {
            !name.is_empty()
                && domain.contains('.')
                && !domain.contains('@')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
        })
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum Refused {
    #[error("Offline (test instance)")]
    Offline,
    #[error("Source disabled")]
    Disabled,
    #[error("Unknown host or group")]
    UnknownHost,
    #[error("SEC needs contact email")]
    NeedsContact,
    #[error("Request budget unavailable")]
    Budget,
    #[error("Source busy (spacing wait exceeded)")]
    Busy,
    #[error("Source paused until {until}")]
    Paused { until: i64 },
    #[error("Transport failure")]
    Transport,
    #[error("HTTP {0}")]
    Http(u16),
    #[error("Response exceeds 8 MiB")]
    BodyTooLarge,
    #[error("Invalid request")]
    InvalidRequest,
    #[error("Network ledger unavailable (check absolute HOME / XDG_DATA_HOME / TR_NET_LEDGER and permissions)")]
    Ledger,
    #[error("Redirect refused: target host {0}")]
    Redirect(String),
}
impl From<rusqlite::Error> for Refused {
    fn from(_: rusqlite::Error) -> Self {
        Self::Ledger
    }
}

pub fn ledger_path() -> Result<PathBuf, Refused> {
    ledger_path_with(&|key| std::env::var_os(key))
}
fn ledger_path_with(get: &impl Fn(&str) -> Option<std::ffi::OsString>) -> Result<PathBuf, Refused> {
    let absolute = |key| -> Result<Option<PathBuf>, Refused> {
        get(key)
            .map(|v| {
                let path = PathBuf::from(v);
                if path.is_absolute() { Ok(path) } else { Err(Refused::Ledger) }
            })
            .transpose()
    };
    // Validate every supplied path, even one overridden by a higher-priority setting.
    let ledger = absolute("TR_NET_LEDGER")?;
    let home = absolute("HOME")?;
    let xdg = absolute("XDG_DATA_HOME")?;
    if let Some(path) = ledger {
        return Ok(path);
    }
    let base = if cfg!(target_os = "macos") {
        home.ok_or(Refused::Ledger)?.join("Library/Application Support")
    } else if let Some(xdg) = xdg {
        xdg
    } else {
        home.ok_or(Refused::Ledger)?.join(".local/share")
    };
    Ok(base.join("terminal-rs-net/ledger.sqlite3"))
}
/// Explicit environment snapshot makes isolated tests possible without process-global env mutation.
pub struct Config {
    pub ledger: PathBuf,
    /// Captured only for the offline gate; never used to locate the ledger.
    pub data_dir: Option<PathBuf>,
    /// Legacy compatibility field; never overrides the TR_DATA_DIR offline gate.
    pub allow_external: bool,
    pub max_wait: Duration,
}
impl Config {
    pub fn from_env() -> Result<Self, Refused> {
        Self::from_lookup(|key| std::env::var_os(key))
    }
    fn from_lookup(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> Result<Self, Refused> {
        Ok(Self {
            ledger: ledger_path_with(&get)?,
            data_dir: get("TR_DATA_DIR").map(PathBuf::from),
            allow_external: get("TR_ALLOW_EXTERNAL").is_some_and(|v| v == "1"),
            max_wait: Duration::from_secs(5),
        })
    }
}
#[derive(Clone, Debug)]
pub struct Status {
    pub group: &'static str,
    pub enabled: bool,
    pub requests_today: u32,
    pub cap: u32,
    /// Unix milliseconds, matching the durable pacing ledger.
    pub last_request: Option<i64>,
    pub paused_until: Option<i64>,
    pub reason: Option<String>,
    pub offline: bool,
    pub needs_contact: bool,
}

/// Headers needed by incremental consumers. A 304 has no body and is a success.
#[derive(Clone, Debug, Default)]
pub struct ResponseMetadata {
    pub status: u16,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

/// Last reported provider allowances, shared across instances; timestamps use UTC seconds.
#[derive(Clone, Debug, Default)]
pub struct Remaining {
    pub at: i64,
    pub requests: Option<u32>,
    pub rows: Option<u32>,
}

pub struct Guard {
    // SENATE-BARGO: only fingerprints enter the guard/ledger, never credential values.
    bargo_bucket: RwLock<String>,
    config: Config,
    settings: RwLock<Settings>,
    client: reqwest::Client,
    policies: RwLock<Vec<HostPolicy>>,
    cache: Mutex<HashMap<(String, String), Arc<CacheSlot>>>,
    cooldowns: RwLock<HashMap<String, LocalCooldown>>,
    #[cfg(any(test, feature = "test-support"))]
    fixture: Option<reqwest::Url>,
}
fn fingerprint(value: &[u8]) -> String {
    Sha1::digest(value).iter().map(|b| format!("{b:02x}")).collect()
}
fn bargo_bucket(key: Option<&str>) -> String {
    key.filter(|k| !k.is_empty())
        .map_or_else(|| "bargo:keyless".into(), |key| format!("bargo:key:{}", fingerprint(key.as_bytes())))
}
fn sensitive_fingerprint(request: &reqwest::Request) -> String {
    let mut headers: Vec<_> = request
        .headers()
        .iter()
        .filter(|(name, value)| {
            value.is_sensitive() || matches!(name.as_str(), "x-api-key" | "authorization" | "x-finnhub-token")
        })
        .map(|(name, value)| (name.as_str(), value.as_bytes()))
        .collect();
    headers.sort();
    let mut digest = Sha1::new();
    for (name, value) in headers {
        digest.update(name.as_bytes());
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value);
    }
    digest.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
type FlightResult = (Instant, Result<(Vec<u8>, ResponseMetadata), Refused>);
#[derive(Default)]
struct CacheSlot {
    flight: tokio::sync::Mutex<()>,
    result: Mutex<Option<FlightResult>>,
    generation: AtomicU64,
}
#[derive(Clone)]
struct LocalCooldown {
    until: i64,
    strikes: u32,
    reason: String,
    durable: bool,
}
static OWNER: AtomicU64 = AtomicU64::new(0);
static SHARED: OnceLock<Guard> = OnceLock::new();
static SHARED_RETRY: Mutex<Option<Instant>> = Mutex::new(None);
static SHARED_SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);
/// Apply settings without opening the ledger (or initializing an HTTP client).
pub fn configure_shared(settings: Settings) {
    let mut current = SHARED_SETTINGS.write().expect("shared settings lock");
    if let Some(guard) = SHARED.get() {
        guard.configure(settings.clone());
    }
    *current = Some(settings);
}
pub fn shared() -> Result<&'static Guard, Refused> {
    let settings = SHARED_SETTINGS.read().expect("shared settings lock");
    shared_with(&SHARED, &SHARED_RETRY, || Guard::new(Config::from_env()?, settings.clone().unwrap_or_default()))
}
fn shared_with<'a>(
    slot: &'a OnceLock<Guard>,
    retry: &Mutex<Option<Instant>>,
    open: impl FnOnce() -> Result<Guard, Refused>,
) -> Result<&'a Guard, Refused> {
    let mut retry = retry.lock().expect("shared retry lock");
    if let Some(guard) = slot.get() {
        return Ok(guard);
    }
    if !opening_due(*retry, Instant::now()) {
        return Err(Refused::Ledger);
    }
    // Measure from the completion of the attempt, including a slow failed ledger open.
    let result = open();
    *retry = Some(Instant::now());
    let _ = slot.set(result?);
    Ok(slot.get().expect("initialized guard"))
}
fn opening_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|at| now.duration_since(at) >= Duration::from_secs(60))
}
impl Guard {
    pub fn new(config: Config, settings: Settings) -> Result<Self, Refused> {
        if !config.ledger.is_absolute() {
            return Err(Refused::Ledger);
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| Refused::Transport)?;
        let guard = Self {
            bargo_bucket: RwLock::new(bargo_bucket(None)),
            config,
            settings: RwLock::new(settings),
            client,
            policies: RwLock::new(policies()),
            cache: Mutex::new(HashMap::new()),
            cooldowns: RwLock::new(HashMap::new()),
            #[cfg(any(test, feature = "test-support"))]
            fixture: None,
        };
        guard.db()?.execute_batch("CREATE TABLE IF NOT EXISTS requests (\"group\" TEXT, day INTEGER, count INTEGER NOT NULL, PRIMARY KEY(\"group\",day));
            CREATE TABLE IF NOT EXISTS cooldowns (\"group\" TEXT PRIMARY KEY, until INTEGER, strikes INTEGER, reason TEXT);
            CREATE TABLE IF NOT EXISTS last_request (\"group\" TEXT PRIMARY KEY, at INTEGER);
            CREATE TABLE IF NOT EXISTS inflight (\"group\" TEXT PRIMARY KEY, until INTEGER);
            CREATE TABLE IF NOT EXISTS transport_failures (\"group\" TEXT PRIMARY KEY, count INTEGER);")?;
        let db = guard.db()?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS slots (key TEXT PRIMARY KEY, at INTEGER NOT NULL, until INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS counters (key TEXT PRIMARY KEY, count INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS bargo_sweeps (day INTEGER PRIMARY KEY, attempts INTEGER NOT NULL, retry_at INTEGER NOT NULL, reason TEXT);
             CREATE TABLE IF NOT EXISTS bargo_sweep_modes (day INTEGER NOT NULL, bucket TEXT NOT NULL, attempts INTEGER NOT NULL, retry_at INTEGER NOT NULL, reason TEXT, PRIMARY KEY(day,bucket));
             CREATE TABLE IF NOT EXISTS remaining (\"group\" TEXT PRIMARY KEY, at INTEGER NOT NULL, requests INTEGER, rows INTEGER);
             CREATE TABLE IF NOT EXISTS row_usage (\"group\" TEXT, day INTEGER, count INTEGER NOT NULL, PRIMARY KEY(\"group\",day));",
        )?;
        // Legacy global slots have no credential identity. Preserve them only for
        // keyless mode: they must never block a newly saved fingerprint's bootstrap.
        db.execute_batch(
            "INSERT OR IGNORE INTO bargo_sweep_modes SELECT day,'bargo:keyless',attempts,retry_at,reason FROM bargo_sweeps;
             INSERT OR IGNORE INTO slots SELECT 'bargo:keyless:sweep',at,until FROM slots WHERE key='bargo:sweep';
             INSERT OR IGNORE INTO slots SELECT 'bargo:keyless:poll',at,until FROM slots WHERE key='bargo:poll';",
        )?;
        let has_owner = |db: &Connection| -> rusqlite::Result<bool> {
            Ok(db
                .prepare("PRAGMA table_info(inflight)")?
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .iter()
                .any(|c| c == "owner"))
        };
        if !has_owner(&db)? {
            let altered = db.execute_batch("ALTER TABLE inflight ADD COLUMN owner TEXT");
            if !has_owner(&db)? {
                altered?;
            }
        }
        Ok(guard)
    }
    /// N1a: dependent-crate fixtures keep real authorization/budgets but send only to loopback.
    #[cfg(feature = "test-support")]
    pub fn for_fixture(config: Config, settings: Settings, base: reqwest::Url) -> Result<Self, Refused> {
        if base.scheme() != "http"
            || base.host_str() != Some("127.0.0.1")
            || !base.username().is_empty()
            || base.password().is_some()
        {
            return Err(Refused::InvalidRequest);
        }
        let mut guard = Self::new(config, settings)?;
        guard.fixture = Some(base);
        Ok(guard)
    }
    /// Machine-wide cadence independent of data directories. A failed ledger never grants work.
    pub fn claim_slot(&self, key: &str, interval: Duration) -> Result<bool, Refused> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = Utc::now().timestamp_millis();
        let interval = i64::try_from(interval.as_millis()).map_err(|_| Refused::InvalidRequest)?;
        let previous: Option<(i64, i64)> = tx
            .query_row("SELECT at,until FROM slots WHERE key=?1", [key], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        if previous.is_some_and(|(at, until)| now < until.max(at.saturating_add(interval))) {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO slots VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET at=excluded.at,until=excluded.until",
            params![key, now, now.saturating_add(interval)],
        )?;
        tx.commit()?;
        Ok(true)
    }
    /// Extend a claimed slot after a consumer failure. Other instances cannot shorten this backoff.
    pub fn defer_slot(&self, key: &str, interval: Duration) -> Result<(), Refused> {
        let until = Utc::now()
            .timestamp_millis()
            .saturating_add(i64::try_from(interval.as_millis()).map_err(|_| Refused::InvalidRequest)?);
        self.db()?.execute("UPDATE slots SET until=max(until,?2) WHERE key=?1", params![key, until])?;
        Ok(())
    }
    /// Durable consumer attempt counters accompany slots so a new instance cannot reset retries.
    pub fn increment_counter(&self, key: &str) -> Result<i64, Refused> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let count = tx.query_row(
            "INSERT INTO counters VALUES(?1,1) ON CONFLICT(key) DO UPDATE SET count=count+1 RETURNING count",
            [key],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(count)
    }
    /// Refund a consumer reservation when authorization prevented a send.
    pub fn decrement_counter(&self, key: &str) -> Result<(), Refused> {
        self.db()?.execute("UPDATE counters SET count=max(0,count-1) WHERE key=?1", [key])?;
        Ok(())
    }
    pub fn counter(&self, key: &str) -> Result<i64, Refused> {
        Ok(self
            .db()?
            .query_row("SELECT count FROM counters WHERE key=?1", [key], |r| r.get(0))
            .optional()?
            .unwrap_or(0))
    }
    pub fn reserve_counter(&self, key: &str, limit: i64) -> Result<bool, Refused> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let count: i64 =
            tx.query_row("SELECT count FROM counters WHERE key=?1", [key], |r| r.get(0)).optional()?.unwrap_or(0);
        if count >= limit {
            return Ok(false);
        }
        tx.execute("INSERT INTO counters VALUES(?1,1) ON CONFLICT(key) DO UPDATE SET count=count+1", [key])?;
        tx.commit()?;
        Ok(true)
    }
    /// Release a consumer slot only when no request was sent. Never used for HTTP failures.
    pub fn release_slot(&self, key: &str) -> Result<(), Refused> {
        self.db()?.execute("DELETE FROM slots WHERE key=?1", [key])?;
        Ok(())
    }
    /// Status follows the currently stored key; each send derives its own bucket independently.
    pub fn set_bargo_key(&self, key: Option<&str>) {
        *self.bargo_bucket.write().expect("Bargo bucket lock") = bargo_bucket(key);
    }
    /// Conservative 90% allowance, independent for keyless and each key fingerprint.
    pub fn bargo_allowance(&self, key: Option<&str>) -> Result<u32, Refused> {
        let bucket = bargo_bucket(key);
        let keyed = key.is_some_and(|k| !k.is_empty());
        let day = Utc::now().timestamp() / 86400;
        let db = self.db()?;
        let used: u32 = db
            .query_row("SELECT count FROM row_usage WHERE \"group\"=?1 AND day=?2", params![bucket, day], |r| r.get(0))
            .optional()?
            .unwrap_or(0);
        let requests: u32 = db
            .query_row("SELECT count FROM requests WHERE \"group\"=?1 AND day=?2", params![bucket, day], |r| r.get(0))
            .optional()?
            .unwrap_or(0);
        if requests >= if keyed { 40 } else { 10 } {
            return Ok(0);
        }
        let mut available = (if keyed { 900u32 } else { 90 }).saturating_sub(used);
        if let Some(remaining) = self.remaining(&bucket)?.filter(|r| r.at / 86400 == day) {
            if remaining.requests.is_some_and(|n| n <= if keyed { 10 } else { 3 }) {
                return Ok(0);
            }
            if let Some(rows) = remaining.rows {
                available = available.min(rows.saturating_sub(if keyed { 100 } else { 10 }));
            }
        }
        Ok(available)
    }
    /// A saved key lifts only an authentication pause. Other sources and other causes stay paused.
    pub fn bargo_key_saved(&self, key: &str) -> Result<(), Refused> {
        self.set_bargo_key(Some(key));
        let mut local = self.cooldowns.write().expect("cooldown lock");
        let db = self.db()?;
        let cleared =
            db.execute("DELETE FROM cooldowns WHERE \"group\"='bargo' AND reason='Key error (HTTP 401)'", [])?;
        let local_auth = local.get("bargo").is_some_and(|p| p.reason == "Key error (HTTP 401)");
        if local_auth {
            local.remove("bargo");
        }
        if cleared > 0 || local_auth {
            // An authentication deferral may retry with the replacement key, without resetting
            // any request/row counters, other pause causes, or the daily attempt bound.
            db.execute(
                "UPDATE bargo_sweep_modes SET retry_at=0,reason=NULL WHERE day=?1 AND bucket=?2",
                params![Utc::now().timestamp() / 86400, bargo_bucket(Some(key))],
            )?;
        }
        Ok(())
    }
    /// The first successful keyed sweep covers the free tier's full 90-day window.
    /// Failed attempts do not consume that bootstrap; later sweeps cover 60 days.
    pub fn bargo_sweep_days(&self) -> Result<i64, Refused> {
        let bucket = self.bargo_bucket.read().expect("Bargo bucket lock").clone();
        let completed: bool = self.db()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM slots WHERE key=?1)",
            [format!("{bucket}:sweep")],
            |r| r.get(0),
        )?;
        Ok(if bucket != "bargo:keyless" && !completed { 90 } else { 60 })
    }

    /// Daily UTC sweep per mode/key fingerprint before polls, with a durable
    /// machine-wide crash lease and at most eight attempts/day per fingerprint.
    /// A deferred attempt is not a completed 24-hour slot; only finish_bargo_sweep completes it.
    pub fn claim_bargo_run(&self, initial: bool) -> Result<Option<bool>, Refused> {
        if let Err(error) = self.check("bargo") {
            if error != Refused::Budget {
                return Err(error);
            }
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = Utc::now().timestamp_millis();
        let day = now / 86_400_000;
        let bucket = self.bargo_bucket.read().expect("Bargo bucket lock").clone();
        let sweep_slot = format!("{bucket}:sweep");
        let poll_slot = format!("{bucket}:poll");
        let completed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM slots WHERE key=?3 AND at/86400000=?1 AND until>?2)",
            params![day, now, sweep_slot],
            |r| r.get(0),
        )?;
        let state: Option<(u32, i64, Option<String>)> = tx
            .query_row(
                "SELECT attempts,retry_at,reason FROM bargo_sweep_modes WHERE day=?1 AND bucket=?2",
                params![day, bucket],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if !completed && state.as_ref().is_none_or(|(attempts, retry, _)| *attempts < 8 && now >= *retry) {
            tx.execute("INSERT INTO bargo_sweep_modes VALUES(?1,?3,1,?2,NULL) ON CONFLICT(day,bucket) DO UPDATE SET attempts=attempts+1,retry_at=excluded.retry_at,reason=NULL",
                params![day, now+900_000, bucket])?;
            // A sweep replaces a poll, and polls remain suppressed for three hours after it ran.
            tx.execute(
                "INSERT INTO slots VALUES(?3,?1,?2) ON CONFLICT(key) DO UPDATE SET at=excluded.at,until=excluded.until",
                params![now, now + 10_800_000, poll_slot],
            )?;
            tx.commit()?;
            return Ok(Some(true));
        }
        // A crashed/in-flight sweep has not yet run or recorded a deferral. No polls may pass it.
        let mut deferred = state.as_ref().is_some_and(|(_, _, reason)| reason.is_some());
        if !completed
            && state
                .as_ref()
                .is_some_and(|(attempts, retry, reason)| *attempts >= 8 && now >= *retry && reason.is_none())
        {
            tx.execute("UPDATE bargo_sweep_modes SET reason='Senate sweep deferred: interrupted; daily retry bound reached' WHERE day=?1 AND bucket=?2", params![day, bucket])?;
            deferred = true;
        }
        tx.commit()?;
        if initial || (!completed && !deferred) {
            return Ok(None);
        }
        Ok(self.claim_slot(&poll_slot, Duration::from_secs(10800))?.then_some(false))
    }
    pub fn finish_bargo_sweep(&self, reason: Option<&str>) -> Result<(), Refused> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = Utc::now().timestamp_millis();
        let bucket = self.bargo_bucket.read().expect("Bargo bucket lock").clone();
        tx.execute(
            "UPDATE bargo_sweep_modes SET reason=?2,retry_at=?3 WHERE day=?1 AND bucket=?4",
            params![now / 86_400_000, reason, if reason.is_some() { now + 900_000 } else { now }, bucket],
        )?;
        tx.execute(
            "INSERT INTO slots VALUES(?3,?1,?2) ON CONFLICT(key) DO UPDATE SET at=excluded.at,until=excluded.until",
            params![now, now + 10_800_000, format!("{bucket}:poll")],
        )?;
        if reason.is_none() {
            tx.execute(
                "INSERT INTO slots VALUES(?3,?1,?2) ON CONFLICT(key) DO UPDATE SET at=excluded.at,until=excluded.until",
                params![now, (now / 86_400_000 + 1) * 86_400_000, format!("{bucket}:sweep")],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn bargo_sweep_reason(&self) -> Result<Option<String>, Refused> {
        Ok(self
            .db()?
            .query_row(
                "SELECT reason FROM bargo_sweep_modes WHERE day=?1 AND bucket=?2",
                params![Utc::now().timestamp() / 86400, self.bargo_bucket.read().expect("Bargo bucket lock").clone()],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }
    pub fn remaining(&self, group: &str) -> Result<Option<Remaining>, Refused> {
        let bucket = self.bargo_bucket.read().expect("Bargo bucket lock").clone();
        let group = if group == "bargo" { bucket.as_str() } else { group };
        Ok(self
            .db()?
            .query_row("SELECT at,requests,rows FROM remaining WHERE \"group\"=?1", [group], |r| {
                Ok(Remaining { at: r.get(0)?, requests: r.get(1)?, rows: r.get(2)? })
            })
            .optional()?)
    }
    fn record_remaining(&self, group: &str, metadata: &Remaining) -> Result<(), Refused> {
        if metadata.requests.is_some() || metadata.rows.is_some() {
            self.db()?.execute(
                "INSERT INTO remaining VALUES(?1,?2,?3,?4) ON CONFLICT(\"group\") DO UPDATE SET at=excluded.at,
                 requests=CASE WHEN remaining.at/86400=excluded.at/86400 THEN min(COALESCE(remaining.requests,excluded.requests),COALESCE(excluded.requests,remaining.requests)) ELSE excluded.requests END,
                 rows=CASE WHEN remaining.at/86400=excluded.at/86400 THEN min(COALESCE(remaining.rows,excluded.rows),COALESCE(excluded.rows,remaining.rows)) ELSE excluded.rows END",
                params![group, Utc::now().timestamp(), metadata.requests, metadata.rows],
            )?;
        }
        Ok(())
    }
    pub fn clear_counter(&self, key: &str) -> Result<(), Refused> {
        self.db()?.execute("DELETE FROM counters WHERE key=?1", [key])?;
        Ok(())
    }
    pub fn set_cache_ttl(&self, group: &str, ttl: Duration) -> Result<(), Refused> {
        if !GROUPS.contains(&group) {
            return Err(Refused::UnknownHost);
        }
        for policy in self.policies.write().expect("policies lock").iter_mut().filter(|p| p.group == group) {
            policy.cache_ttl = ttl;
        }
        self.cache.lock().expect("cache lock").retain(|(g, _), _| g != group);
        Ok(())
    }
    fn db(&self) -> Result<Connection, Refused> {
        self.db_timeout(Duration::from_secs(5))
    }
    fn db_timeout(&self, timeout: Duration) -> Result<Connection, Refused> {
        if let Some(parent) = self.config.ledger.parent() {
            std::fs::create_dir_all(parent).map_err(|_| Refused::Ledger)?;
        }
        let db = Connection::open(&self.config.ledger)?;
        db.busy_timeout(timeout)?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        Ok(db)
    }
    pub fn configure(&self, settings: Settings) {
        *self.settings.write().expect("settings lock") = settings;
    }
    fn offline(&self) -> bool {
        self.config.data_dir.is_some() && !self.config.allow_external
    }
    fn bargo_mode_allowed(&self, keyed: bool) -> Result<(), Refused> {
        if !keyed && !self.settings.read().expect("settings lock").bargo_keyless {
            return Err(Refused::Disabled);
        }
        Ok(())
    }
    fn identity(&self, group: &str) -> Result<String, Refused> {
        if self.offline() {
            return Err(Refused::Offline);
        }
        if !GROUPS.contains(&group) {
            return Err(Refused::UnknownHost);
        }
        let settings = self.settings.read().expect("settings lock");
        if !settings.enabled(group) {
            return Err(Refused::Disabled);
        }
        match self.policies.read().expect("policies lock").iter().find(|p| p.group == group).map(|p| &p.user_agent) {
            Some(UserAgent::SecContact) => {
                if !valid_email(&settings.sec_contact) {
                    return Err(Refused::NeedsContact);
                }
                Ok(format!("terminal-rs/{} (personal research; {})", env!("CARGO_PKG_VERSION"), settings.sec_contact))
            }
            Some(UserAgent::YahooLegacy) => Ok("Mozilla/5.0 (compatible; terminal-rs)".into()),
            _ => Ok(format!("terminal-rs/{} (personal research; {group})", env!("CARGO_PKG_VERSION"))),
        }
    }
    // Check and reserve atomically across processes. Never hold a transaction while sleeping.
    #[cfg(test)]
    fn reserve(&self, policy: &HostPolicy) -> Result<Option<Duration>, Refused> {
        self.reserve_owned(policy, "test-reservation")
    }
    #[cfg(test)]
    fn reserve_owned(&self, policy: &HostPolicy, owner: &str) -> Result<Option<Duration>, Refused> {
        self.reserve_with_counter(policy, owner, None, None)
    }
    fn reserve_with_counter(
        &self,
        policy: &HostPolicy,
        owner: &str,
        counter: Option<(&str, u32)>,
        bargo_rows: Option<(u32, bool, &str)>,
    ) -> Result<Option<Duration>, Refused> {
        if let Some(pause) = self
            .cooldowns
            .read()
            .expect("cooldown lock")
            .get(policy.group)
            .filter(|p| !p.durable && p.until > Utc::now().timestamp())
        {
            return Err(Refused::Paused { until: pause.until });
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = Utc::now().timestamp_millis();
        let paused: Option<i64> =
            tx.query_row("SELECT until FROM cooldowns WHERE \"group\"=?1", [policy.group], |r| r.get(0)).optional()?;
        if let Some(until) = paused.filter(|u| *u > now / 1000) {
            return Err(Refused::Paused { until });
        }
        let usage_group = bargo_rows.map_or(policy.group, |(_, _, bucket)| bucket);
        let count: u32 = tx
            .query_row(
                "SELECT count FROM requests WHERE \"group\"=?1 AND day=?2",
                params![usage_group, now / 86_400_000],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if count >= policy.daily_cap {
            return Err(Refused::Budget);
        }
        let counter_key = if let Some((prefix, cap)) = counter {
            // Resolve the UTC day at each actual hop, including a redirect over midnight.
            let day = chrono::DateTime::from_timestamp_millis(now).ok_or(Refused::Ledger)?;
            let key = format!("{prefix}{}", day.format("%Y%m%d"));
            let count: u32 =
                tx.query_row("SELECT count FROM counters WHERE key=?1", [&key], |r| r.get(0)).optional()?.unwrap_or(0);
            if count >= cap {
                return Err(Refused::Budget);
            }
            Some(key)
        } else {
            None
        };
        let last: Option<i64> =
            tx.query_row("SELECT at FROM last_request WHERE \"group\"=?1", [policy.group], |r| r.get(0)).optional()?;
        let lease: Option<i64> =
            tx.query_row("SELECT until FROM inflight WHERE \"group\"=?1", [policy.group], |r| r.get(0)).optional()?;
        let interval = (policy.min_interval.as_millis() as i64).max(60_000 / i64::from(policy.per_minute));
        let next = last.map(|t| t + interval).unwrap_or(now).max(lease.unwrap_or(now));
        if next > now {
            // A live lease may be released as soon as the body finishes; poll it rather than
            // treating its crash-recovery expiry as the actual queue delay.
            let wait = if lease.is_some_and(|until| until > now) { (next - now).min(50) } else { next - now };
            return Ok(Some(Duration::from_millis(wait as u64)));
        }
        if let Some((rows, keyed, bucket)) = bargo_rows {
            self.bargo_mode_allowed(keyed)?;
            let remaining: Option<(i64, Option<u32>, Option<u32>)> = tx
                .query_row("SELECT at,requests,rows FROM remaining WHERE \"group\"=?1", [bucket], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?;
            if remaining.is_some_and(|(at, requests, available)| {
                at / 86400 == now / 86_400_000
                    && (requests.is_some_and(|n| n <= if keyed { 10 } else { 3 })
                        || (rows > 0
                            && available.is_some_and(|n| n.saturating_sub(if keyed { 100 } else { 10 }) < rows)))
            }) {
                return Err(Refused::Budget);
            }
            let used: u32 = tx
                .query_row(
                    "SELECT count FROM row_usage WHERE \"group\"=?1 AND day=?2",
                    params![bucket, now / 86_400_000],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0);
            // /health costs no rows, even after the row allowance has been consumed.
            if rows > 0 && used.saturating_add(rows) > if keyed { 900 } else { 90 } {
                return Err(Refused::Budget);
            }
            tx.execute("INSERT INTO row_usage VALUES(?1,?2,?3) ON CONFLICT(\"group\",day) DO UPDATE SET count=count+excluded.count",
                params![bucket, now/86_400_000, rows])?;
            // Account for sends even when subsequent responses omit headers.
            tx.execute("UPDATE remaining SET requests=max(0,requests-1),rows=max(0,rows-?2) WHERE \"group\"=?1 AND at/86400=?3",
                params![bucket, rows, now/86_400_000])?;
        }
        if let Some(key) = counter_key {
            tx.execute("INSERT INTO counters VALUES(?1,1) ON CONFLICT(key) DO UPDATE SET count=count+1", [&key])?;
        }
        tx.execute(
            "INSERT INTO requests VALUES (?1,?2,1) ON CONFLICT(\"group\",day) DO UPDATE SET count=count+1",
            params![usage_group, now / 86_400_000],
        )?;
        tx.execute(
            "INSERT INTO last_request VALUES (?1,?2) ON CONFLICT(\"group\") DO UPDATE SET at=excluded.at",
            params![policy.group, now],
        )?;
        tx.execute(
            "INSERT INTO inflight (\"group\",until,owner) VALUES (?1,?2,?3) ON CONFLICT(\"group\") DO UPDATE SET until=excluded.until,owner=excluded.owner",
            params![policy.group, now + 35_000, owner],
        )?;
        tx.commit()?;
        Ok(None)
    }
    fn pause(&self, group: &str, reason: &str, retry: Option<&str>) -> Result<Refused, Refused> {
        let now = Utc::now().timestamp();
        let mut local = self.cooldowns.write().expect("cooldown lock");
        let durable_strikes = self.db_timeout(Duration::from_millis(50)).and_then(|db| {
            db.query_row("SELECT strikes FROM cooldowns WHERE \"group\"=?1", [group], |r| r.get::<_, u32>(0))
                .optional()
                .map(|s| s.unwrap_or(0))
                .map_err(Refused::from)
        });
        // An unreadable ledger is unknown, not evidence of earlier strikes.
        let known = durable_strikes.is_ok();
        let prior = local.get(group).map_or(0, |p| p.strikes).max(durable_strikes.unwrap_or(0));
        let until_for = |strikes: u32, known: bool| {
            if known && strikes >= 2 && policies().iter().any(|p| host_budget(p.group) && p.host == group) {
                return 253_402_300_799;
            }
            (now + [21600, 43200, 86400][strikes.min(2) as usize])
                .max(retry.and_then(|v| retry_after(v, now)).unwrap_or(0))
                .min(now + 604800)
        };
        let mut pause = LocalCooldown {
            until: until_for(prior, known),
            strikes: prior.saturating_add(1),
            reason: reason.into(),
            durable: false,
        };
        // A brief bounded retry, then retain the refusal locally even if SQLite stays busy.
        for _ in 0..3 {
            let recorded = (|| -> Result<(i64, u32), Refused> {
                let mut db = self.db_timeout(Duration::from_millis(50))?;
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let strikes: u32 = tx
                    .query_row("SELECT strikes FROM cooldowns WHERE \"group\"=?1", [group], |r| r.get(0))
                    .optional()?
                    .unwrap_or(0);
                let strikes = strikes.max(prior);
                let until = until_for(strikes, true);
                tx.execute("INSERT INTO cooldowns VALUES (?1,?2,?3,?4) ON CONFLICT(\"group\") DO UPDATE SET until=excluded.until,strikes=excluded.strikes,reason=excluded.reason", params![group, until, strikes.saturating_add(1), reason])?;
                tx.commit()?;
                Ok((until, strikes.saturating_add(1)))
            })();
            if let Ok((until, strikes)) = recorded {
                pause.until = until;
                pause.strikes = strikes;
                pause.durable = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let until = pause.until;
        local.insert(group.into(), pause);
        Ok(Refused::Paused { until })
    }
    fn transport(&self, group: &str) -> Result<Refused, Refused> {
        let db = self.db()?;
        db.execute(
            "INSERT INTO transport_failures VALUES (?1,1) ON CONFLICT(\"group\") DO UPDATE SET count=count+1",
            [group],
        )?;
        let count: u32 =
            db.query_row("SELECT count FROM transport_failures WHERE \"group\"=?1", [group], |r| r.get(0))?;
        if count >= 3 { self.pause(group, "Repeated transport failures", None) } else { Ok(Refused::Transport) }
    }
    /// Uses the guard's own client: caller-supplied redirect/retry policies cannot bypass budgets.
    pub async fn send(&self, group: &str, request: reqwest::RequestBuilder) -> Result<Vec<u8>, Refused> {
        self.send_with_metadata(group, request).await.map(|(body, _)| body)
    }
    /// Metadata travels with the requested URL's flight, including redirect and coalesced results.
    pub async fn send_with_metadata(
        &self,
        group: &str,
        request: reqwest::RequestBuilder,
    ) -> Result<(Vec<u8>, ResponseMetadata), Refused> {
        self.send_with_metadata_budget(group, request, u32::MAX).await
    }
    /// A lower ceiling reserves capacity for higher-priority consumers. Enforced in each hop's
    /// IMMEDIATE budget transaction, not by a racy preflight status check.
    pub async fn send_with_metadata_budget(
        &self,
        group: &str,
        request: reqwest::RequestBuilder,
        daily_limit: u32,
    ) -> Result<(Vec<u8>, ResponseMetadata), Refused> {
        self.send_budgeted(group, request, daily_limit, None, None).await
    }
    /// A consumer's additional daily HTTP-hop ceiling, charged atomically with the host
    /// budget only after pacing/lease acquisition. Cache hits and unsent refusals cost zero.
    /// The UTC YYYYMMDD suffix is appended to `counter_prefix` at each send reservation.
    pub async fn send_with_consumer_budget(
        &self,
        group: &str,
        request: reqwest::RequestBuilder,
        daily_limit: u32,
        counter_prefix: &str,
        counter_limit: u32,
    ) -> Result<(Vec<u8>, ResponseMetadata), Refused> {
        self.send_budgeted(group, request, daily_limit, Some((counter_prefix, counter_limit)), None).await
    }
    /// Feed switches are checked before budget reservation, including each redirect hop.
    pub async fn send_feed(
        &self,
        group: &str,
        id: &str,
        request: reqwest::RequestBuilder,
    ) -> Result<(Vec<u8>, ResponseMetadata), Refused> {
        self.send_budgeted(group, request, u32::MAX, None, Some(id)).await
    }
    fn feed_enabled(&self, id: Option<&str>) -> Result<(), Refused> {
        if id.is_some_and(|id| self.settings.read().expect("settings lock").disabled_feeds.contains(id)) {
            Err(Refused::Disabled)
        } else {
            Ok(())
        }
    }
    async fn send_budgeted(
        &self,
        group: &str,
        request: reqwest::RequestBuilder,
        daily_limit: u32,
        counter: Option<(&str, u32)>,
        feed: Option<&str>,
    ) -> Result<(Vec<u8>, ResponseMetadata), Refused> {
        let request = request.build().map_err(|_| Refused::InvalidRequest)?;
        // This prohibition takes precedence even over offline/unknown-group errors.
        if disabled_host(request.url()) {
            return Err(Refused::Disabled);
        }
        self.identity(group)?;
        self.feed_enabled(feed)?;
        if group == "bargo" {
            let key = request.headers().get("X-Api-Key").and_then(|v| v.to_str().ok());
            self.bargo_mode_allowed(key.is_some_and(|k| !k.is_empty()))?;
            self.set_bargo_key(key);
        }
        let policy = self.policy(group, request.url())?;
        if request.method() != reqwest::Method::GET {
            return self.send_uncached(group, request, daily_limit, counter, feed).await;
        }
        let key = (group.to_owned(), format!("{}#{}", request.url(), sensitive_fingerprint(&request)));
        let slot = {
            let mut cache = self.cache.lock().expect("cache lock");
            // Bound memory without evicting in-flight slots.
            if cache.len() >= 256 {
                cache.retain(|_, slot| Arc::strong_count(slot) > 1);
            }
            cache.entry(key).or_insert_with(|| Arc::new(CacheSlot::default())).clone()
        };
        let generation = slot.generation.load(Ordering::Acquire);
        let _flight = slot.flight.lock().await;
        self.identity(group)?;
        self.feed_enabled(feed)?;
        if let Some((at, result)) = &*slot.result.lock().expect("cache result lock") {
            if slot.generation.load(Ordering::Acquire) != generation
                || (result.is_ok() && at.elapsed() < policy.cache_ttl)
            {
                return result.clone();
            }
        }
        let result = self.send_uncached(group, request, daily_limit, counter, feed).await;
        // Failures are only delivered to current waiters, never reused as cached content.
        *slot.result.lock().expect("cache result lock") = Some((Instant::now(), result.clone()));
        slot.generation.fetch_add(1, Ordering::Release);
        result
    }
    /// Preflight without reserving budget or making a connection.
    pub fn check(&self, group: &str) -> Result<(), Refused> {
        self.identity(group)?;
        if group == "bargo" {
            self.bargo_mode_allowed(*self.bargo_bucket.read().expect("Bargo bucket lock") != "bargo:keyless")?;
        }
        let status = self.status()?.into_iter().find(|s| s.group == group).ok_or(Refused::UnknownHost)?;
        if let Some(until) = status.paused_until {
            return Err(Refused::Paused { until });
        }
        if status.requests_today >= status.cap {
            return Err(Refused::Budget);
        }
        Ok(())
    }
    fn policy(&self, group: &str, url: &reqwest::Url) -> Result<HostPolicy, Refused> {
        if disabled_host(url) {
            return Err(Refused::Disabled);
        }
        if url.scheme() != "https"
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(Refused::UnknownHost);
        }
        self.policies
            .read()
            .expect("policies lock")
            .iter()
            .find(|p| p.group == group && Some(p.host) == url.host_str())
            .cloned()
            .ok_or(Refused::UnknownHost)
    }
    async fn send_uncached(
        &self,
        group: &str,
        mut request: reqwest::Request,
        daily_limit: u32,
        counter: Option<(&str, u32)>,
        feed: Option<&str>,
    ) -> Result<(Vec<u8>, ResponseMetadata), Refused> {
        // Bargo's longer spacing still uses one durable reservation per actual send.
        let key = request.headers().get("X-Api-Key").and_then(|v| v.to_str().ok()).map(str::to_owned);
        let keyed = key.as_ref().is_some_and(|k| !k.is_empty());
        let bucket = bargo_bucket(key.as_deref());
        let bargo_rows = if group == "bargo" {
            let rows = if request.url().path().ends_with("/trades") {
                request
                    .url()
                    .query_pairs()
                    .find(|(k, _)| k == "limit")
                    .and_then(|(_, v)| v.parse::<u32>().ok())
                    .unwrap_or(100)
                    .min(if keyed { 250 } else { 100 })
            } else {
                0
            };
            Some((rows, keyed, bucket.as_str()))
        } else {
            None
        };
        let max_wait = if group == "bargo" && !self.config.max_wait.is_zero() {
            Duration::from_secs(35)
        } else if group == "yahoo-news" && !self.config.max_wait.is_zero() {
            Duration::from_secs(30)
        } else {
            self.config.max_wait
        };
        let deadline = Instant::now() + max_wait;
        for hop in 0..=3 {
            let mut policy = self.policy(group, request.url())?;
            let feed_response = feed.is_some()
                || host_budget(group)
                || (group == "sec" && request.url().path() == "/news/pressreleases.rss");
            if host_budget(group) {
                policy.group = policy.host;
            }
            policy.daily_cap = policy.daily_cap.min(daily_limit);
            if group == "bargo" {
                self.set_bargo_key(key.as_deref());
                policy.daily_cap = policy.daily_cap.min(if keyed { 40 } else { 10 });
            }
            let owner = format!(
                "{}-{}-{}",
                std::process::id(),
                Utc::now().timestamp_micros(),
                OWNER.fetch_add(1, Ordering::Relaxed)
            );
            loop {
                self.identity(group)?;
                self.feed_enabled(feed)?;
                match self.reserve_with_counter(&policy, &owner, counter, bargo_rows)? {
                    None => break,
                    Some(wait) if Instant::now() + wait <= deadline => tokio::time::sleep(wait).await,
                    _ => return Err(Refused::Busy),
                }
            }
            let _lease = Lease { guard: self, group: policy.group, owner };
            let ua = self.identity(group)?;
            request.headers_mut().insert(reqwest::header::USER_AGENT, ua.parse().map_err(|_| Refused::InvalidRequest)?);
            *request.timeout_mut() = Some(Duration::from_secs(30));
            let original = request.url().clone();
            let replay = request.try_clone();
            #[cfg(any(test, feature = "test-support"))]
            if let Some(base) = &self.fixture {
                assert_eq!(base.host_str(), Some("127.0.0.1"));
                let mut local = base.clone();
                local.set_path(original.path());
                local.set_query(original.query());
                *request.url_mut() = local;
            }
            let mut response = match self.client.execute(request).await {
                Ok(r) => r,
                Err(_) => return Err(self.transport(policy.group)?),
            };
            let status = response.status().as_u16();
            let retry =
                response.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).map(str::to_owned);
            let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
            let metadata = ResponseMetadata { status, etag: header("etag"), last_modified: header("last-modified") };
            self.record_remaining(
                if group == "bargo" { &bucket } else { group },
                &Remaining {
                    at: Utc::now().timestamp(),
                    requests: header("x-ratelimit-remaining").and_then(|v| v.parse().ok()),
                    rows: header("x-ratelimit-rows-remaining").and_then(|v| v.parse().ok()),
                },
            )?;
            if matches!(status, 403 | 429 | 451 | 503) || (group == "bargo" && (status == 401 || status >= 500)) {
                let retry = if group == "bargo" && status == 429 {
                    let seconds = retry
                        .as_deref()
                        .and_then(|v| retry_after(v, Utc::now().timestamp()))
                        .unwrap_or(0)
                        .max((Utc::now().timestamp() / 86400 + 1) * 86400);
                    Some((seconds - Utc::now().timestamp()).to_string())
                } else {
                    retry
                };
                let reason = if group == "bargo" && status == 401 {
                    "Key error (HTTP 401)".to_owned()
                } else {
                    format!("HTTP {status}")
                };
                return Err(self.pause(policy.group, &reason, retry.as_deref())?);
            }
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                let target = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| original.join(v).ok())
                    .ok_or_else(|| Refused::Redirect("<invalid>".into()))?;
                if disabled_host(&target) {
                    return Err(Refused::Disabled);
                }
                if hop == 3 || self.policy(group, &target).is_err() {
                    return Err(Refused::Redirect(target.host_str().unwrap_or("<none>").to_owned()));
                }
                request = replay.ok_or(Refused::InvalidRequest)?;
                *request.url_mut() = target;
                continue;
            }
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            let mut body = Vec::new();
            if status != 304 {
                loop {
                    let chunk = match response.chunk().await {
                        Ok(c) => c,
                        Err(_) => return Err(self.transport(policy.group)?),
                    };
                    let Some(chunk) = chunk else { break };
                    if body.len() + chunk.len() > BODY_CAP {
                        return Err(Refused::BodyTooLarge);
                    }
                    body.extend_from_slice(&chunk);
                }
            }
            if feed_response && status == 200 && !feed_signature(&body) {
                return Err(self.pause(policy.group, "Not a feed (expected rss, feed or RDF root)", None)?);
            }
            // Signatures ignore the whitespace / UTF-8 BOM sometimes prepended by the Clerk.
            let signature = trim_signature_prefix(&body);
            let roster_refused =
                group == "house-roster" && (200..300).contains(&status) && !house_roster_signature(signature);
            let house_download_refused = group == "house-disclosures"
                && (200..300).contains(&status)
                && ((original.path().ends_with(".pdf") && !signature.starts_with(b"%PDF-"))
                    || (original.path().ends_with(".zip")
                        && ![b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08"]
                            .iter()
                            .any(|magic| body.starts_with(*magic))));
            if (!feed_response && html_response(&content_type, &body) && (blocked_body(&body) || group == "bargo"))
                || house_download_refused
                || roster_refused
            {
                return Err(self.pause(
                    group,
                    "Access denied / captcha or invalid disclosure download",
                    retry.as_deref(),
                )?);
            }
            if !(200..300).contains(&status) && status != 304 {
                return Err(Refused::Http(status));
            }
            if status == 304 {
                body.clear();
            }
            let db = self.db()?;
            // Refund unused row reservations only for a valid bounded trade page. Errors,
            // cancellation and malformed bodies retain their conservative full reservation.
            if let Some((reserved, _, bucket)) = bargo_rows.filter(|(rows, _, _)| *rows > 0) {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) {
                    if let Some(rows) =
                        value.get("trades").and_then(|v| v.as_array()).filter(|r| r.len() <= reserved as usize)
                    {
                        db.execute(
                            "UPDATE row_usage SET count=max(0,count-?1) WHERE \"group\"=?2 AND day=?3",
                            params![reserved - rows.len() as u32, bucket, Utc::now().timestamp() / 86400],
                        )?;
                    }
                }
            }
            db.execute("DELETE FROM transport_failures WHERE \"group\"=?1", [policy.group])?;
            if !host_budget(group) {
                db.execute("DELETE FROM cooldowns WHERE \"group\"=?1", [policy.group])?;
                self.cooldowns.write().expect("cooldown lock").remove(policy.group);
            }
            return Ok((body, metadata));
        }
        unreachable!("redirect loop is bounded")
    }
    /// Manual resume clears strikes, never budgets or request spacing.
    pub fn resume(&self, group: &str) -> Result<(), Refused> {
        if !GROUPS.contains(&group) {
            return Err(Refused::UnknownHost);
        }
        let keys: Vec<_> = policies()
            .into_iter()
            .filter(|p| p.group == group)
            .map(|p| if host_budget(group) { p.host } else { p.group })
            .collect();
        let db = self.db()?;
        let mut local = self.cooldowns.write().expect("cooldown lock");
        for key in keys {
            db.execute("DELETE FROM cooldowns WHERE \"group\"=?1", [key])?;
            local.remove(key);
        }
        Ok(())
    }
    pub fn host_status(&self, group: &str, url: &str) -> Result<Status, Refused> {
        let policy = self.policy(group, &reqwest::Url::parse(url).map_err(|_| Refused::InvalidRequest)?)?;
        if !host_budget(group) {
            return self.status()?.into_iter().find(|s| s.group == group).ok_or(Refused::UnknownHost);
        }
        let db = self.db()?;
        let key = policy.host;
        let settings = self.settings.read().expect("settings lock");
        let requests_today = db
            .query_row(
                "SELECT count FROM requests WHERE \"group\"=?1 AND day=?2",
                params![key, Utc::now().timestamp() / 86400],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let last_request =
            db.query_row("SELECT at FROM last_request WHERE \"group\"=?1", [key], |r| r.get(0)).optional()?;
        let paused: Option<(i64, String)> = db
            .query_row(
                "SELECT until,reason FROM cooldowns WHERE \"group\"=?1 AND until>?2",
                params![key, Utc::now().timestamp()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let local = self.cooldowns.read().expect("cooldown lock");
        let paused = local
            .get(key)
            .filter(|p| !p.durable && p.until > Utc::now().timestamp())
            .map(|p| (p.until, p.reason.clone()))
            .or(paused);
        Ok(Status {
            group: policy.group,
            enabled: settings.enabled(group),
            requests_today,
            cap: policy.daily_cap,
            last_request,
            paused_until: paused.as_ref().map(|p| p.0),
            reason: if settings.settings_error {
                Some("Corrupt data_sources settings; all external sources off".into())
            } else {
                paused.map(|p| format!("{key}: {}", p.1))
            },
            offline: self.offline(),
            needs_contact: false,
        })
    }
    pub fn status(&self) -> Result<Vec<Status>, Refused> {
        let db = self.db()?;
        let settings = self.settings.read().expect("settings lock").clone();
        GROUPS
            .into_iter()
            .map(|group| {
                if host_budget(group) {
                    let mut rows = policies()
                        .into_iter()
                        .filter(|p| p.group == group)
                        .map(|p| self.host_status(group, &format!("https://{}/", p.host)))
                        .collect::<Result<Vec<_>, _>>()?;
                    let mut row = rows.remove(0);
                    for next in rows {
                        row.requests_today += next.requests_today;
                        row.cap += next.cap;
                        row.last_request = row.last_request.max(next.last_request);
                        row.paused_until = row.paused_until.max(next.paused_until);
                        if let Some(reason) = next.reason {
                            row.reason = Some(row.reason.map_or(reason.clone(), |old| format!("{old}; {reason}")));
                        }
                    }
                    return Ok(row);
                }
                let bucket = self.bargo_bucket.read().expect("Bargo bucket lock").clone();
                let usage_group = if group == "bargo" { bucket.as_str() } else { group };
                let cap = if group == "bargo" && bucket == "bargo:keyless" {
                    10
                } else {
                    policies().into_iter().find(|p| p.group == group).expect("registered group").daily_cap
                };
                let requests_today = db
                    .query_row(
                        "SELECT count FROM requests WHERE \"group\"=?1 AND day=?2",
                        params![usage_group, Utc::now().timestamp() / 86400],
                        |r| r.get(0),
                    )
                    .optional()?
                    .unwrap_or(0);
                let last_request =
                    db.query_row("SELECT at FROM last_request WHERE \"group\"=?1", [group], |r| r.get(0)).optional()?;
                let paused: Option<(i64, String)> = db
                    .query_row(
                        "SELECT until,reason FROM cooldowns WHERE \"group\"=?1 AND until>?2",
                        params![group, Utc::now().timestamp()],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let local = self.cooldowns.read().expect("cooldown lock");
                let paused = local
                    .get(group)
                    .filter(|p| !p.durable && p.until > Utc::now().timestamp())
                    .map(|p| (p.until, p.reason.clone()))
                    .or(paused);
                Ok(Status {
                    group,
                    enabled: settings.enabled(group)
                        && (group != "bargo" || bucket != "bargo:keyless" || settings.bargo_keyless),
                    requests_today,
                    cap,
                    last_request,
                    paused_until: paused.as_ref().map(|p| p.0),
                    reason: if settings.settings_error {
                        Some("Corrupt data_sources settings; all external sources off".into())
                    } else {
                        paused.map(|p| p.1)
                    },
                    offline: self.offline(),
                    needs_contact: group == "sec" && !valid_email(&settings.sec_contact),
                })
            })
            .collect()
    }
}
struct Lease<'a> {
    guard: &'a Guard,
    group: &'a str,
    owner: String,
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Ok(db) = self.guard.db() {
            let _ = db.execute("DELETE FROM inflight WHERE \"group\"=?1 AND owner=?2", params![self.group, self.owner]);
        }
    }
}
fn retry_after(value: &str, now: i64) -> Option<i64> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|s| now.saturating_add(s.min(604800) as i64))
        .or_else(|| chrono::DateTime::parse_from_rfc2822(value).ok().map(|d| d.timestamp()))
        .map(|until| until.clamp(now + 1, now + 604800))
}
fn html_response(content_type: &str, body: &[u8]) -> bool {
    let mime = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    // Explicit structured formats win even when their payload happens to start with HTML.
    if mime.contains("json") || mime.contains("xml") || mime.contains("rss") || mime.contains("atom") {
        return false;
    }
    let text = String::from_utf8_lossy(body);
    let start = text.trim_start().chars().take(32).collect::<String>().to_ascii_lowercase();
    mime == "text/html" || start.starts_with("<!doctype html") || start.starts_with("<html")
}
fn blocked_body(body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    [
        "potential security risk",
        "sorry, a potential",
        "access denied",
        "verify you are human",
        "checking your browser",
        "captcha",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}
/// For diagnostic URLs only. Guard errors never include URLs, bodies, headers or contact details.
pub fn redact(value: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(value) else { return "<redacted>".into() };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_fragment(None);
    let pairs: Vec<_> = url
        .query_pairs()
        .map(|(k, v)| {
            let secret = matches!(
                k.to_ascii_lowercase().as_str(),
                "token" | "apikey" | "api_key" | "access_token" | "x-finnhub-token"
            );
            (k.into_owned(), if secret { "<redacted>".into() } else { v.into_owned() })
        })
        .collect();
    url.set_query(None);
    if !pairs.is_empty() {
        url.query_pairs_mut().extend_pairs(pairs);
    }
    url.to_string()
}
