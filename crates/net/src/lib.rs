//! Conservative, machine-wide budgets for news and filings, with guarded redirects and local caching.
#[cfg(test)]
mod tests;
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

pub const GROUPS: [&str; 4] = ["finnhub", "sec", "yahoo-news", "coindesk"];
const BODY_CAP: usize = 8 * 1024 * 1024;

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
    ]
    .into_iter()
    .map(|(host, group, seconds, per_minute, daily_cap)| HostPolicy {
        host,
        group,
        min_interval: Duration::from_secs(seconds),
        per_minute,
        daily_cap,
        concurrency: 1,
        cache_ttl: if matches!(group, "coindesk" | "yahoo-news") { Duration::from_secs(300) } else { Duration::ZERO },
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
            settings_error: true,
            ..Self::default()
        }
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

pub struct Guard {
    config: Config,
    settings: RwLock<Settings>,
    client: reqwest::Client,
    policies: RwLock<Vec<HostPolicy>>,
    cache: Mutex<HashMap<(String, String), Arc<CacheSlot>>>,
    cooldowns: RwLock<HashMap<String, LocalCooldown>>,
    #[cfg(test)]
    fixture: Option<reqwest::Url>,
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
            config,
            settings: RwLock::new(settings),
            client,
            policies: RwLock::new(policies()),
            cache: Mutex::new(HashMap::new()),
            cooldowns: RwLock::new(HashMap::new()),
            #[cfg(test)]
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
             CREATE TABLE IF NOT EXISTS counters (key TEXT PRIMARY KEY, count INTEGER NOT NULL);",
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
    fn reserve_owned(&self, policy: &HostPolicy, owner: &str) -> Result<Option<Duration>, Refused> {
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
        let count: u32 = tx
            .query_row(
                "SELECT count FROM requests WHERE \"group\"=?1 AND day=?2",
                params![policy.group, now / 86_400_000],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if count >= policy.daily_cap {
            return Err(Refused::Budget);
        }
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
        tx.execute(
            "INSERT INTO requests VALUES (?1,?2,1) ON CONFLICT(\"group\",day) DO UPDATE SET count=count+1",
            params![policy.group, now / 86_400_000],
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
        // With an unreadable ledger, choose the highest floor rather than risk shortening an escalation.
        let prior = local.get(group).map_or(0, |p| p.strikes).max(durable_strikes.unwrap_or(2));
        let until_for = |strikes: u32| {
            (now + [21600, 43200, 86400][strikes.min(2) as usize])
                .max(retry.and_then(|v| retry_after(v, now)).unwrap_or(0))
                .min(now + 604800)
        };
        let mut pause = LocalCooldown {
            until: until_for(prior),
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
                let until = until_for(strikes);
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
        self.identity(group)?;
        let request = request.build().map_err(|_| Refused::InvalidRequest)?;
        let policy = self.policy(group, request.url())?;
        if request.method() != reqwest::Method::GET {
            return self.send_uncached(group, request, daily_limit).await;
        }
        let key = (group.to_owned(), request.url().to_string());
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
        if let Some((at, result)) = &*slot.result.lock().expect("cache result lock") {
            if slot.generation.load(Ordering::Acquire) != generation
                || (result.is_ok() && at.elapsed() < policy.cache_ttl)
            {
                return result.clone();
            }
        }
        let result = self.send_uncached(group, request, daily_limit).await;
        // Failures are only delivered to current waiters, never reused as cached content.
        *slot.result.lock().expect("cache result lock") = Some((Instant::now(), result.clone()));
        slot.generation.fetch_add(1, Ordering::Release);
        result
    }
    /// Preflight without reserving budget or making a connection.
    pub fn check(&self, group: &str) -> Result<(), Refused> {
        self.identity(group)?;
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
    ) -> Result<(Vec<u8>, ResponseMetadata), Refused> {
        let max_wait = if group == "yahoo-news" && !self.config.max_wait.is_zero() {
            Duration::from_secs(30)
        } else {
            self.config.max_wait
        };
        let deadline = Instant::now() + max_wait;
        for hop in 0..=3 {
            let mut policy = self.policy(group, request.url())?;
            policy.daily_cap = policy.daily_cap.min(daily_limit);
            let owner = format!(
                "{}-{}-{}",
                std::process::id(),
                Utc::now().timestamp_micros(),
                OWNER.fetch_add(1, Ordering::Relaxed)
            );
            loop {
                self.identity(group)?;
                match self.reserve_owned(&policy, &owner)? {
                    None => break,
                    Some(wait) if Instant::now() + wait <= deadline => tokio::time::sleep(wait).await,
                    _ => return Err(Refused::Budget),
                }
            }
            let _lease = Lease { guard: self, group, owner };
            let ua = self.identity(group)?;
            request.headers_mut().insert(reqwest::header::USER_AGENT, ua.parse().map_err(|_| Refused::InvalidRequest)?);
            *request.timeout_mut() = Some(Duration::from_secs(30));
            let original = request.url().clone();
            let replay = request.try_clone();
            #[cfg(test)]
            if let Some(base) = &self.fixture {
                assert_eq!(base.host_str(), Some("127.0.0.1"));
                *request.url_mut() = base.clone();
            }
            let mut response = match self.client.execute(request).await {
                Ok(r) => r,
                Err(_) => return Err(self.transport(group)?),
            };
            let status = response.status().as_u16();
            let retry =
                response.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).map(str::to_owned);
            if matches!(status, 403 | 429 | 503) {
                return Err(self.pause(group, &format!("HTTP {status}"), retry.as_deref())?);
            }
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                let target = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| original.join(v).ok())
                    .ok_or_else(|| Refused::Redirect("<invalid>".into()))?;
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
            loop {
                let chunk = match response.chunk().await {
                    Ok(c) => c,
                    Err(_) => return Err(self.transport(group)?),
                };
                let Some(chunk) = chunk else { break };
                if body.len() + chunk.len() > BODY_CAP {
                    return Err(Refused::BodyTooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            if html_response(&content_type, &body) && blocked_body(&body) {
                return Err(self.pause(group, "Access denied / captcha", retry.as_deref())?);
            }
            if !(200..300).contains(&status) && status != 304 {
                return Err(Refused::Http(status));
            }
            let header = |name| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
            let metadata = ResponseMetadata {
                status,
                etag: header(reqwest::header::ETAG),
                last_modified: header(reqwest::header::LAST_MODIFIED),
            };
            if status == 304 {
                body.clear();
            }
            let db = self.db()?;
            db.execute("DELETE FROM transport_failures WHERE \"group\"=?1", [group])?;
            db.execute("DELETE FROM cooldowns WHERE \"group\"=?1", [group])?;
            self.cooldowns.write().expect("cooldown lock").remove(group);
            return Ok((body, metadata));
        }
        unreachable!("redirect loop is bounded")
    }
    pub fn status(&self) -> Result<Vec<Status>, Refused> {
        let db = self.db()?;
        let settings = self.settings.read().expect("settings lock").clone();
        GROUPS
            .into_iter()
            .map(|group| {
                let cap = policies().into_iter().find(|p| p.group == group).expect("registered group").daily_cap;
                let requests_today = db
                    .query_row(
                        "SELECT count FROM requests WHERE \"group\"=?1 AND day=?2",
                        params![group, Utc::now().timestamp() / 86400],
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
                    enabled: settings.enabled(group),
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
