//! Publisher pages for Analyze: one explicit, SSRF-safe fetch per click.
//!
//! The client is deliberately separate from the guard's provider client: no redirects, no
//! retries, no proxy, no cookie store, and a resolver that drops every non-public address.
//! URL checks run before any I/O and again on every redirect hop, so a redirect can never
//! reach an address the resolver would have rejected.

#[cfg(test)]
#[path = "publisher_tests.rs"]
mod tests;

use crate::{Guard, Refused};
use chrono::Utc;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs},
    sync::Arc,
    time::Duration,
};

const USER_AGENT: &str = concat!("terminal-rs/", env!("CARGO_PKG_VERSION"), " (personal research; publisher)");
const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_BODY: usize = 2 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const DAILY_CAP: i64 = 60;
const MINUTE_CAP: i64 = 6;
const HOST_SPACING: Duration = Duration::from_secs(10);
const ROBOTS_TTL: i64 = 86_400;
const ROBOTS_FAILURE_TTL: i64 = 3_600;
const MAX_ROBOTS: usize = 256 * 1024;
const MAX_ROBOTS_PATH: usize = 2_048;

#[derive(Clone)]
pub(crate) struct RobotsBody {
    text: String,
    truncated: bool,
}

#[derive(Clone)]
pub(crate) struct RobotsCache {
    pub(crate) expires_at: i64,
    result: Result<RobotsBody, Reason>,
}
const COOLDOWN_DEFAULT: i64 = 3_600;
const COOLDOWN_MAX: i64 = 604_800;

/// Publisher hosts the user never wants fetched, plus the large paywalls. Matches a host or
/// any subdomain. Analyzer falls back to the headline and summary.
const SKIP: [&str; 12] = [
    "bloomberg.com",
    "wsj.com",
    "economist.com",
    "nikkei.com",
    "ft.com",
    "seekingalpha.com",
    "investing.com",
    "theblock.co",
    "oilprice.com",
    "zerohedge.com",
    "reuters.com",
    "apnews.com",
];

/// Query keys that identify an aggregator/provider credential. Such links are never fetched,
/// and no provider key is ever attached to a publisher request.
const KEY_PARAMS: &[&str] = &[
    "token",
    "apikey",
    "api-key",
    "api_key",
    "access_token",
    "x-finnhub-token",
    "x-amz-signature",
    "x-amz-credential",
    "signature",
    "sig",
    "client_secret",
    "auth",
    "key",
    "secret",
    "password",
];

/// Shared with the story-body save path: keyed links must not be persisted, even as failure rows.
pub fn is_keyed_url(url: &reqwest::Url) -> bool {
    url.query_pairs().any(|(key, _)| KEY_PARAMS.contains(&key.to_ascii_lowercase().as_str()))
}

fn normalize_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Offline,
    Disabled,
    InvalidUrl,
    /// Deny list, paywall skip list, or another host we never contact.
    Skipped,
    /// The aggregator link carries a provider key or token.
    KeyedLink,
    Redirect,
    NotHtml,
    TooLarge,
    Http(u16),
    /// 403 / 429 / 451: publisher refused this client.
    Refused(u16),
    Cooldown,
    Budget,
    Pacing,
    Robots,
    RobotsUnavailable,
    Transport,
    Ledger,
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Offline => f.write_str("offline"),
            Self::Disabled => f.write_str("publisher fetching disabled"),
            Self::InvalidUrl => f.write_str("invalid publisher URL"),
            Self::Skipped => f.write_str("publisher not fetched"),
            Self::KeyedLink => f.write_str("publisher not fetched (provider key)"),
            Self::Redirect => f.write_str("redirect refused"),
            Self::NotHtml => f.write_str("not an article page"),
            Self::TooLarge => f.write_str("page too large"),
            Self::Http(status) => write!(f, "publisher error (HTTP {status})"),
            Self::Refused(status) => write!(f, "publisher refused (HTTP {status})"),
            Self::Cooldown => f.write_str("publisher cooldown"),
            Self::Budget => f.write_str("publisher budget exhausted"),
            Self::Pacing => f.write_str("publisher rate limited"),
            Self::Robots => f.write_str("robots.txt disallows"),
            Self::RobotsUnavailable => f.write_str("robots.txt unavailable"),
            Self::Transport => f.write_str("publisher unreachable"),
            Self::Ledger => f.write_str("network ledger unavailable"),
        }
    }
}

impl From<Refused> for Reason {
    fn from(value: Refused) -> Self {
        match value {
            Refused::Offline => Self::Offline,
            Refused::Disabled => Self::Disabled,
            Refused::InvalidRequest => Self::InvalidUrl,
            Refused::BodyTooLarge => Self::TooLarge,
            Refused::Http(status) => Self::Http(status),
            Refused::Redirect(_) => Self::Redirect,
            Refused::Budget => Self::Budget,
            Refused::Paused { .. } => Self::Cooldown,
            Refused::Busy => Self::Pacing,
            Refused::Ledger => Self::Ledger,
            Refused::UnknownHost | Refused::NeedsContact | Refused::Transport => Self::Transport,
        }
    }
}

/// A fetched publisher page. The noarchive flag comes from X-Robots-Tag; the extractor also
/// checks the robots meta tag, and the caller stores only pages neither marks noarchive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    pub url: String,
    pub html: String,
    pub etag: Option<String>,
    pub noarchive: bool,
    pub fetched_at: i64,
}

/// Fetch one publisher page. Call only from an explicit Analyze or Re-analyze action.
pub async fn fetch_article(url: &str) -> Result<Fetched, Reason> {
    let guard = crate::shared().map_err(Reason::from)?;
    Publisher::new(guard)?.fetch(url).await
}

/// A per-call publisher client. The robots cache lives in the guard so it is shared across
/// Analyze clicks without persisting page or robots content.
pub struct Publisher<'a> {
    guard: &'a Guard,
    client: reqwest::Client,
}

impl<'a> Publisher<'a> {
    pub fn new(guard: &'a Guard) -> Result<Self, Reason> {
        Ok(Self { guard, client: build_client(Arc::new(PublicResolver))? })
    }

    #[cfg(test)]
    fn with_client(guard: &'a Guard, client: reqwest::Client) -> Self {
        Self { guard, client }
    }

    /// One article fetch: URL policy, robots policy, budget, redirects and response checks.
    pub async fn fetch(&self, url: &str) -> Result<Fetched, Reason> {
        if self.guard.offline() {
            return Err(Reason::Offline);
        }
        if !self.guard.publisher_enabled() {
            return Err(Reason::Disabled);
        }
        let url = reqwest::Url::parse(url).map_err(|_| Reason::InvalidUrl)?;
        validate(&url)?;
        // One Analyze click consumes one publisher budget unit; redirect hops are part of it.
        self.charge_group()?;
        self.request_html(&url).await
    }

    async fn request_html(&self, start: &reqwest::Url) -> Result<Fetched, Reason> {
        let mut url = start.clone();
        // Pace once per host per Analyze click. Redirect hops on the same host are part of the
        // same click; a redirect to another host is paced independently.
        let mut paced: HashSet<String> = HashSet::new();
        for hop in 0..=MAX_REDIRECTS {
            validate(&url)?;
            let host = normalize_host(url.host_str().ok_or(Reason::InvalidUrl)?);
            // Canonicalize once for all policy/cache/ledger keys and the actual request.
            url.set_host(Some(&host)).map_err(|_| Reason::InvalidUrl)?;
            self.robots_allowed(&url, &mut paced).await?;
            self.claim_host(&url, &mut paced)?;
            let raw = self.send_once(&url).await?;
            if matches!(raw.status, 301 | 302 | 303 | 307 | 308) {
                if hop == MAX_REDIRECTS {
                    return Err(Reason::Redirect);
                }
                let target = redirect_target(&url, raw.location.as_deref())?;
                url = target;
                continue;
            }
            if !(200..300).contains(&raw.status) {
                return Err(Reason::Http(raw.status));
            }
            if !html_content_type(&raw.content_type) {
                return Err(Reason::NotHtml);
            }
            return Ok(Fetched {
                url: url.to_string(),
                html: String::from_utf8_lossy(&raw.body).into_owned(),
                etag: raw.etag,
                noarchive: raw.noarchive,
                fetched_at: Utc::now().timestamp(),
            });
        }
        Err(Reason::Redirect)
    }

    /// robots.txt is fetched once per host every 24 h through the same client and group budget.
    /// A robots failure is a reason to use the summary, never a reason to skip robots.
    async fn robots_allowed(&self, url: &reqwest::Url, paced: &mut HashSet<String>) -> Result<(), Reason> {
        if url.path().len() > MAX_ROBOTS_PATH {
            return Err(Reason::RobotsUnavailable);
        }
        let host = url.host_str().ok_or(Reason::InvalidUrl)?;
        let now = Utc::now().timestamp();
        if let Some(entry) = self.guard.robots_cached(host)
            && now < entry.expires_at
        {
            return decide_cached_robots(&entry.result, url.path());
        }
        let mut robots = url.clone();
        robots.set_path("/robots.txt");
        robots.set_query(None);
        robots.set_fragment(None);
        // One host slot per click covers both robots and all same-host page hops.
        self.claim_host(&robots, paced)?;
        self.charge_group()?;
        let result = self.fetch_robots(&robots).await;
        let decision = decide_cached_robots(&result, url.path());
        if result.is_ok() || matches!(result, Err(Reason::Robots | Reason::RobotsUnavailable)) {
            let ttl = if result.is_ok() { ROBOTS_TTL } else { ROBOTS_FAILURE_TTL };
            self.guard.cache_robots(host, RobotsCache { expires_at: now + ttl, result });
        }
        decision
    }

    fn claim_host(&self, url: &reqwest::Url, paced: &mut HashSet<String>) -> Result<(), Reason> {
        let host = url.host_str().ok_or(Reason::InvalidUrl)?;
        if self.cooling_down(host)? {
            return Err(Reason::Cooldown);
        }
        if !paced.contains(host) {
            if !self.guard.claim_slot(&format!("publisher:{host}"), HOST_SPACING)? {
                return Err(Reason::Pacing);
            }
            paced.insert(host.to_owned());
        }
        Ok(())
    }

    async fn fetch_robots(&self, start: &reqwest::Url) -> Result<RobotsBody, Reason> {
        let mut url = start.clone();
        for hop in 0..=MAX_REDIRECTS {
            validate(&url)?;
            let raw = self.send_bounded(&url, MAX_ROBOTS, true).await.map_err(robots_failure)?;
            if matches!(raw.status, 301 | 302 | 303 | 307 | 308) {
                if hop == MAX_REDIRECTS {
                    return Err(Reason::RobotsUnavailable);
                }
                let mut target = redirect_target(&url, raw.location.as_deref()).map_err(robots_failure)?;
                let host = normalize_host(target.host_str().ok_or(Reason::RobotsUnavailable)?);
                target.set_host(Some(&host)).map_err(|_| Reason::RobotsUnavailable)?;
                // Another host's robots.txt says nothing about this host.
                if target.host_str() != start.host_str() {
                    return Err(Reason::RobotsUnavailable);
                }
                url = target;
                continue;
            }
            return match raw.status {
                200..=299 => {
                    let mut text = String::from_utf8_lossy(&raw.body).into_owned();
                    let truncated = raw.truncated || text.len() > MAX_ROBOTS;
                    let mut end = text.len().min(MAX_ROBOTS);
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text.truncate(end);
                    Ok(RobotsBody { text, truncated })
                }
                404 | 410 => Ok(RobotsBody { text: String::new(), truncated: false }),
                401 | 403 => Err(Reason::Robots),
                _ => Err(Reason::RobotsUnavailable),
            };
        }
        Err(Reason::RobotsUnavailable)
    }

    /// One bounded request: cooldown, group budget, send, body cap. Redirects are handled by
    /// the callers; this function never follows one.
    async fn send_once(&self, url: &reqwest::Url) -> Result<Raw, Reason> {
        self.send_bounded(url, MAX_BODY, false).await
    }

    async fn send_bounded(&self, url: &reqwest::Url, limit: usize, truncate: bool) -> Result<Raw, Reason> {
        let host = url.host_str().ok_or(Reason::InvalidUrl)?;
        if self.cooling_down(host)? {
            return Err(Reason::Cooldown);
        }
        let response = self.client.get(url.clone()).send().await.map_err(|_| Reason::Transport)?;
        let status = response.status().as_u16();
        let header = |name: &str| response.headers().get(name).and_then(|value| value.to_str().ok()).map(str::to_owned);
        let retry_after = header("retry-after");
        if matches!(status, 403 | 429 | 451) {
            self.cool_down(url, retry_after.as_deref())?;
            return Err(Reason::Refused(status));
        }
        let noarchive = response
            .headers()
            .get_all("x-robots-tag")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|value| value.to_ascii_lowercase().contains("noarchive"));
        let content_type = header("content-type").unwrap_or_default();
        let location = header("location");
        let etag = header("etag");
        let mut response = response;
        let mut body = Vec::new();
        let mut truncated = false;
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > limit {
                        if !truncate {
                            return Err(Reason::TooLarge);
                        }
                        body.extend_from_slice(&chunk[..limit - body.len()]);
                        truncated = true;
                        break;
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(_) => return Err(Reason::Transport),
            }
        }
        Ok(Raw { status, content_type, location, etag, noarchive, body, truncated })
    }

    fn cooling_down(&self, host: &str) -> Result<bool, Reason> {
        Ok(self.guard.slot_until(&cooldown_key(host))?.is_some_and(|until| until > Utc::now().timestamp_millis()))
    }

    /// Publisher group budget: 60 a day and 6 a minute, shared by page and robots fetches.
    fn charge_group(&self) -> Result<(), Reason> {
        let now = Utc::now();
        let day = format!("publisher:day:{}", now.format("%Y%m%d"));
        let minute = format!("publisher:min:{}", now.format("%Y%m%d%H%M"));
        if !self.guard.reserve_counter(&day, DAILY_CAP)? {
            return Err(Reason::Budget);
        }
        if !self.guard.reserve_counter(&minute, MINUTE_CAP)? {
            self.guard.decrement_counter(&day)?;
            return Err(Reason::Budget);
        }
        Ok(())
    }

    /// A publisher refusal is a host cooldown, not a retry. Retry-After is honoured when present.
    fn cool_down(&self, url: &reqwest::Url, retry_after: Option<&str>) -> Result<(), Reason> {
        let host = url.host_str().ok_or(Reason::InvalidUrl)?;
        let seconds = retry_after.and_then(parse_retry_after).unwrap_or(COOLDOWN_DEFAULT).clamp(60, COOLDOWN_MAX);
        let key = cooldown_key(host);
        let now = Utc::now().timestamp_millis();
        let until = now.saturating_add(seconds.saturating_mul(1000));
        match self.guard.slot_until(&key)? {
            Some(existing) if existing >= until => {}
            // defer_slot extends an existing row, but only claim_slot inserts a missing one.
            Some(_) => self.guard.defer_slot(&key, Duration::from_secs(seconds as u64))?,
            None => {
                self.guard.claim_slot(&key, Duration::from_secs(seconds as u64))?;
            }
        }
        Ok(())
    }
}

fn cooldown_key(host: &str) -> String {
    format!("publisher:cooldown:{}", normalize_host(host))
}

fn redirect_target(current: &reqwest::Url, location: Option<&str>) -> Result<reqwest::Url, Reason> {
    let target = current.join(location.ok_or(Reason::Redirect)?).map_err(|_| Reason::Redirect)?;
    if current.scheme() == "https" && target.scheme() == "http" {
        return Err(Reason::Redirect);
    }
    Ok(target)
}

fn decide_cached_robots(result: &Result<RobotsBody, Reason>, path: &str) -> Result<(), Reason> {
    match result {
        // A partial robots file cannot establish permission. Cache the exact bounded bytes
        // and this flag so the first and all subsequent decisions are identical.
        Ok(body) if body.truncated => Err(Reason::Robots),
        Ok(body) => robots_decide(&body.text, path),
        Err(reason) => Err(*reason),
    }
}

fn parse_retry_after(value: &str) -> Option<i64> {
    let now = Utc::now().timestamp();
    value.trim().parse::<u64>().ok().map(|seconds| seconds.min(COOLDOWN_MAX as u64) as i64).or_else(|| {
        chrono::DateTime::parse_from_rfc2822(value).ok().map(|at| (at.timestamp() - now).clamp(1, COOLDOWN_MAX))
    })
}

fn robots_failure(reason: Reason) -> Reason {
    match reason {
        Reason::Refused(401 | 403) => Reason::Robots,
        Reason::Budget | Reason::Cooldown | Reason::Offline | Reason::Disabled | Reason::Ledger => reason,
        _ => Reason::RobotsUnavailable,
    }
}

struct Raw {
    status: u16,
    content_type: String,
    location: Option<String>,
    etag: Option<String>,
    noarchive: bool,
    body: Vec<u8>,
    truncated: bool,
}

fn build_client(resolver: Arc<dyn Resolve>) -> Result<reqwest::Client, Reason> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .dns_resolver(resolver)
        // No cookie store and no default authorization: reqwest adds cookies only when asked.
        .build()
        .map_err(|_| Reason::Transport)
}

fn html_content_type(value: &str) -> bool {
    matches!(
        value.split(';').next().unwrap_or("").trim().to_ascii_lowercase().as_str(),
        "text/html" | "application/xhtml+xml"
    )
}

/// Path policy for the first URL and every redirect hop, before any socket is opened.
fn validate(url: &reqwest::Url) -> Result<(), Reason> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Reason::InvalidUrl);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Reason::InvalidUrl);
    }
    if !matches!(url.port(), None | Some(80) | Some(443)) {
        return Err(Reason::InvalidUrl);
    }
    let host = url.host_str().ok_or(Reason::InvalidUrl)?;
    // host_str keeps IPv6 brackets, so strip them before checking for an IP literal.
    let bare = host.strip_prefix('[').and_then(|host| host.strip_suffix(']')).unwrap_or(host);
    // Reject IP literals, IPv6 zone identifiers and any bracketed remainder. A domain name
    // never carries these characters after URL normalization.
    if bare.parse::<IpAddr>().is_ok()
        || bare.contains(':')
        || bare.contains('[')
        || bare.contains(']')
        || bare.contains('%')
    {
        return Err(Reason::InvalidUrl);
    }
    let host = normalize_host(host);
    if host.is_empty() {
        return Err(Reason::InvalidUrl);
    }
    if crate::disabled_host(url) {
        return Err(Reason::Skipped);
    }
    if SKIP
        .iter()
        .any(|domain| host == *domain || host.strip_suffix(domain).is_some_and(|prefix| prefix.ends_with('.')))
    {
        return Err(Reason::Skipped);
    }
    if is_keyed_url(url) {
        return Err(Reason::KeyedLink);
    }
    Ok(())
}

/// DNS resolver used by the publisher client. It drops every non-public address and fails the
/// request when no address survives; reqwest then connects only to the returned addresses.
struct PublicResolver;

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            if host.parse::<IpAddr>().is_ok() {
                return Err(io_error("IP-literal host"));
            }
            let addrs = tokio::task::spawn_blocking(move || {
                (host.as_str(), 0_u16).to_socket_addrs().map(|addrs| addrs.collect::<Vec<_>>())
            })
            .await
            .map_err(|_| io_error("resolver task failed"))?
            .map_err(|_| io_error("resolution failed"))?;
            public_addrs(addrs)
        })
    }
}

fn public_addrs(addrs: Vec<SocketAddr>) -> Result<Addrs, Box<dyn std::error::Error + Send + Sync>> {
    let allowed: Vec<SocketAddr> = addrs.into_iter().filter(|addr| public_address(addr.ip())).collect();
    if allowed.is_empty() {
        return Err(io_error("no public address"));
    }
    Ok(Box::new(allowed.into_iter()))
}

fn io_error(message: &'static str) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(std::io::Error::other(message))
}

fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => public_v6(ip),
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || (a == 100 && (64..=127).contains(&b))
        || (a == 198 && (b == 18 || b == 19))
        || (a == 192 && b == 0 && c == 0)
        || a >= 240)
}

fn public_v6(ip: Ipv6Addr) -> bool {
    if let Some(ip) = ip.to_ipv4_mapped() {
        return public_v4(ip);
    }
    let s = ip.segments();
    // Local-use NAT64 /48 and IPv4-translated /96 have different embedding layouts.
    // Reject both wholesale rather than risk interpreting their IPv4 payload incorrectly.
    if (s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1) || (s[..4] == [0, 0, 0, 0] && s[4] == 0xffff && s[5] == 0) {
        return false;
    }
    // NAT64 64:ff9b::/96 embeds an IPv4 address in the last two segments.
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6].iter().all(|segment| *segment == 0) {
        let embedded = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        return public_v4(embedded);
    }
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (s[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
        || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (s[0] & 0xffc0) == 0xfec0 // deprecated site-local fec0::/10
        || (s[0] == 0x2001 && (s[1] == 0x0db8 || s[1] == 0)) // documentation and Teredo
        || s[0] == 0x2002 // 6to4 embeds IPv4
        || (s[0] == 0 && s[1] == 0 && s[2] == 0 && s[3] == 0 && s[4] == 0 && s[5] == 0))
}

/// Minimal robots.txt parser: a matching user-agent group's Allow/Disallow rules decide.
fn robots_decide(body: &str, path: &str) -> Result<(), Reason> {
    if path.len() > MAX_ROBOTS_PATH {
        return Err(Reason::RobotsUnavailable);
    }
    let groups = parse_robots(body);
    let mut rules = Vec::new();
    let mut specific = false;
    for (agents, group_rules) in &groups {
        if agents.iter().any(|agent| agent.eq_ignore_ascii_case("terminal-rs")) {
            specific = true;
            rules.extend(group_rules.iter().cloned());
        }
    }
    // A specific group overrides the wildcard group even when it allows everything.
    if !specific {
        for (agents, group_rules) in &groups {
            if agents.iter().any(|agent| agent == "*") {
                rules.extend(group_rules.iter().cloned());
            }
        }
    }
    if allowed_by(&rules, path) { Ok(()) } else { Err(Reason::Robots) }
}

type Rule = (bool, String);

fn parse_robots(body: &str) -> Vec<(Vec<String>, Vec<Rule>)> {
    let mut groups = Vec::new();
    let mut agents: Vec<String> = Vec::new();
    let mut rules: Vec<Rule> = Vec::new();
    let mut seen_rules = false;
    for line in body.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once(':') else { continue };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        match key.as_str() {
            "user-agent" => {
                // A new user-agent line starts a group only after that group has directives;
                // consecutive user-agent lines share one group. Empty directives still count.
                if seen_rules {
                    groups.push((std::mem::take(&mut agents), std::mem::take(&mut rules)));
                    seen_rules = false;
                }
                if !value.is_empty() {
                    agents.push(value.to_owned());
                }
            }
            "allow" | "disallow" if !agents.is_empty() => {
                seen_rules = true;
                if !value.is_empty() {
                    rules.push((key == "allow", value.to_owned()));
                }
            }
            _ => {}
        }
    }
    if !agents.is_empty() {
        groups.push((agents, rules));
    }
    groups
}

fn allowed_by(rules: &[Rule], path: &str) -> bool {
    let mut best: Option<(usize, bool)> = None;
    for (allow, pattern) in rules {
        if robots_match(pattern, path) {
            let length = pattern.trim_end_matches('$').len();
            if best.is_none_or(|(previous, previous_allow)| {
                length > previous || (length == previous && !previous_allow && *allow)
            }) {
                best = Some((length, *allow));
            }
        }
    }
    best.is_none_or(|(_, allow)| allow)
}

/// Robots paths support * wildcards and a trailing $; otherwise they are prefixes.
fn robots_match(pattern: &str, path: &str) -> bool {
    let anchored = pattern.ends_with('$');
    let pattern = pattern.trim_end_matches('$').as_bytes();
    let path = path.as_bytes();
    let (mut p, mut t) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while t < path.len() {
        if p == pattern.len() {
            return !anchored;
        }
        if pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            mark = t;
        } else if p < pattern.len() && pattern[p] == path[t] {
            p += 1;
            t += 1;
        } else if let Some(at) = star {
            p = at + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    !anchored || p == pattern.len()
}
