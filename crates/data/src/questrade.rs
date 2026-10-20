//! Questrade market data (OHLC candles) for Questrade clients, through a personal app's
//! manual-authorization refresh token. Optional: without a token this source is inactive and
//! nothing else changes. Questrade serves Canadian clients only, so it is one data source
//! among several, never a requirement of the scanner.
//!
//! Each refresh-token redemption returns a *new* refresh token and invalidates the old one,
//! so every rotation is handed to the token sink (Keychain / owner-only file) before the
//! access token is used. Redemption is serialized so two requests never spend the same token.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use tr_core::{Bar, Timeframe};

use crate::MarketData;

pub const LOGIN_URL: &str = "https://login.questrade.com";
pub const API_CENTRE_URL: &str = "https://login.questrade.com/APIAccess/UserApps.aspx";
/// Questrade returns at most this many candles per request.
const MAX_CANDLES: usize = 2000;
/// Spacing between API requests: about 16 per second, under the 20/s market-data limit.
const REQUEST_SPACING: Duration = Duration::from_millis(60);
/// US listing venues, preferred when a ticker also trades in Canada.
const US_EXCHANGES: [&str; 5] = ["NASDAQ", "NYSE", "NYSEAM", "ARCA", "BATS"];

/// Persists the latest refresh token (`None` = forget it).
pub type TokenSink = Box<dyn Fn(Option<&str>) -> Result<()> + Send + Sync>;

/// A cheap, cloneable handle: the provider routes through it, and Settings swaps its token
/// at runtime without rebuilding the provider.
#[derive(Clone)]
pub struct Questrade {
    inner: Arc<Inner>,
}

struct Inner {
    client: reqwest::Client,
    login: String,
    session: tokio::sync::Mutex<Session>,
    /// Mirrors `session.refresh.is_some()` for synchronous routing decisions.
    configured: AtomicBool,
    /// Earliest time the next API request may start.
    next_slot: tokio::sync::Mutex<Instant>,
    ids: std::sync::Mutex<HashMap<String, u64>>,
    sink: TokenSink,
}

#[derive(Default)]
struct Session {
    refresh: Option<String>,
    access: Option<Access>,
}

struct Access {
    token: String,
    server: String,
    expires: Instant,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    refresh_token: String,
    api_server: String,
}

/// Parse a refresh-token redemption.
fn parse_token(json: &str) -> Result<(String, Duration, String, String)> {
    let t: TokenResponse = serde_json::from_str(json).context("parsing the Questrade token response")?;
    let server = t.api_server.trim_end_matches('/').to_string();
    if t.access_token.is_empty() || t.refresh_token.is_empty() || !server.starts_with("https://") {
        bail!("Questrade returned an incomplete token response");
    }
    Ok((t.access_token, Duration::from_secs(t.expires_in), t.refresh_token, server))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SymbolHit {
    symbol: String,
    symbol_id: u64,
    #[serde(default)]
    listing_exchange: String,
    #[serde(default)]
    currency: String,
}

#[derive(Debug, Deserialize)]
struct SymbolSearch {
    // The reference documents `symbols`; its sample response shows `symbol`.
    #[serde(alias = "symbol", default)]
    symbols: Vec<SymbolHit>,
}

/// The exact ticker match, preferring a US listing quoted in USD over a Canadian one.
fn pick_symbol(json: &str, ticker: &str) -> Result<Option<u64>> {
    let found: SymbolSearch = serde_json::from_str(json).context("parsing Questrade symbol search")?;
    let mut exact: Vec<_> = found.symbols.into_iter().filter(|s| s.symbol.eq_ignore_ascii_case(ticker)).collect();
    exact.sort_by_key(|s| !(US_EXCHANGES.contains(&s.listing_exchange.as_str()) && s.currency == "USD"));
    Ok(exact.first().map(|s| s.symbol_id))
}

#[derive(Debug, Deserialize)]
struct Candle {
    start: String,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

#[derive(Debug, Deserialize)]
struct Candles {
    #[serde(default)]
    candles: Vec<Candle>,
}

/// Parse `markets/candles/:id`; each bar is stamped with its candle's start time.
fn parse_candles(json: &str) -> Result<Vec<Bar>> {
    let page: Candles = serde_json::from_str(json).context("parsing Questrade candles")?;
    page.candles
        .into_iter()
        .map(|c| {
            let ts = DateTime::parse_from_rfc3339(&c.start).with_context(|| format!("candle time {}", c.start))?;
            Ok(Bar::new(ts.timestamp(), c.open, c.high, c.low, c.close, c.volume))
        })
        .collect()
}

/// Questrade's error body (`{"code":1017,"message":"…"}`) as ` — message (code N)`.
fn api_error(body: &str) -> String {
    #[derive(Deserialize)]
    struct ApiError {
        code: Option<i64>,
        message: Option<String>,
    }
    match serde_json::from_str::<ApiError>(body) {
        Ok(ApiError { message: Some(m), code }) if !m.trim().is_empty() => {
            let code = code.map_or(String::new(), |c| format!(" (code {c})"));
            format!(" — {}{code}", m.trim())
        }
        _ => String::new(),
    }
}

fn interval(tf: Timeframe) -> &'static str {
    match tf {
        Timeframe::M1 => "OneMinute",
        Timeframe::M5 => "FiveMinutes",
        Timeframe::M15 => "FifteenMinutes",
        Timeframe::M30 => "HalfHour",
        Timeframe::H1 => "OneHour",
        Timeframe::H4 => "FourHours",
        Timeframe::D1 => "OneDay",
        Timeframe::W1 => "OneWeek",
        Timeframe::MN1 => "OneMonth",
    }
}

/// Calendar seconds likely to hold `bars` candles: markets trade about 5 of 7 days and,
/// intraday, 6.5 of 24 hours (regular session). Generous, since the result is clipped.
fn window(tf: Timeframe, bars: usize) -> i64 {
    let weekdays = 7.0 / 5.0;
    let factor = if tf.is_intraday() { weekdays * 24.0 / 6.5 } else { weekdays };
    (tf.seconds() as f64 * bars as f64 * factor * 1.2) as i64 + 7 * 86_400
}

impl Questrade {
    pub fn new(refresh: Option<String>, sink: TokenSink) -> Result<Self> {
        Self::with_login(refresh, sink, LOGIN_URL)
    }

    /// `login` is `https://practicelogin.questrade.com` for practice accounts.
    pub fn with_login(refresh: Option<String>, sink: TokenSink, login: &str) -> Result<Self> {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build()?;
        let refresh = refresh.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        Ok(Self {
            inner: Arc::new(Inner {
                client,
                login: login.trim_end_matches('/').to_string(),
                configured: AtomicBool::new(refresh.is_some()),
                next_slot: tokio::sync::Mutex::new(Instant::now()),
                session: tokio::sync::Mutex::new(Session { refresh, access: None }),
                ids: std::sync::Mutex::new(HashMap::new()),
                sink,
            }),
        })
    }

    pub fn is_configured(&self) -> bool {
        self.inner.configured.load(Ordering::Relaxed)
    }

    /// Replace (or with `None`, forget) the refresh token, persisting it first.
    pub async fn set_refresh_token(&self, token: Option<String>) -> Result<()> {
        let token = token.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        (self.inner.sink)(token.as_deref())?;
        let mut session = self.inner.session.lock().await;
        self.inner.configured.store(token.is_some(), Ordering::Relaxed);
        *session = Session { refresh: token, access: None };
        drop(session);
        self.inner.ids.lock().expect("symbol cache").clear();
        Ok(())
    }

    /// A valid access token and API server, redeeming the refresh token when needed.
    async fn access(&self) -> Result<(String, String)> {
        let mut session = self.inner.session.lock().await;
        if let Some(a) = &session.access
            && a.expires > Instant::now() + Duration::from_secs(30)
        {
            return Ok((a.token.clone(), a.server.clone()));
        }
        let refresh = session.refresh.clone().ok_or_else(|| anyhow!("Questrade is not set up"))?;
        let resp = self
            .inner
            .client
            .get(format!("{}/oauth2/token", self.inner.login))
            .query(&[("grant_type", "refresh_token"), ("refresh_token", refresh.as_str())])
            .send()
            .await
            // reqwest errors carry the URL, whose query holds the refresh token.
            .map_err(|e| anyhow!("contacting Questrade login: {}", e.without_url()))?;
        if !resp.status().is_success() {
            session.access = None;
            bail!(
                "Questrade rejected the refresh token (HTTP {}). Tokens expire after 7 days unused and \
                 work once; generate a new one in the Questrade API Centre",
                resp.status().as_u16()
            );
        }
        let body = resp.text().await.map_err(|e| anyhow!("reading the Questrade token: {}", e.without_url()))?;
        let (token, ttl, next, server) = parse_token(&body)?;
        // The old refresh token is now spent: keep the new one before anything else can fail.
        if let Err(e) = (self.inner.sink)(Some(&next)) {
            tracing::error!("could not save the rotated Questrade token, re-enter it after restart: {e:#}");
        }
        session.refresh = Some(next);
        session.access = Some(Access { token: token.clone(), server: server.clone(), expires: Instant::now() + ttl });
        Ok((token, server))
    }

    /// GET an API path, retrying once after an expired access token or a rate limit.
    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<String> {
        for attempt in 0..2 {
            let (token, server) = self.access().await?;
            self.pace().await;
            let resp =
                self.inner.client.get(format!("{server}/v1/{path}")).bearer_auth(&token).query(query).send().await?;
            let status = resp.status();
            if status.is_success() {
                return Ok(resp.text().await?);
            }
            if attempt == 0 && status == reqwest::StatusCode::UNAUTHORIZED {
                self.inner.session.lock().await.access = None;
                continue;
            }
            if attempt == 0 && status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let reset = resp
                    .headers()
                    .get("X-RateLimit-Reset")
                    .and_then(|v| v.to_str().ok()?.parse::<i64>().ok())
                    .map_or(1, |at| (at - Utc::now().timestamp()).clamp(1, 5));
                tokio::time::sleep(Duration::from_secs(reset as u64)).await;
                continue;
            }
            let detail = api_error(&resp.text().await.unwrap_or_default());
            let hint = if status == reqwest::StatusCode::FORBIDDEN {
                ". Symbol search needs both OAuth scopes: tick \"Retrieve balances, positions, orders and \
                 executions\" and \"Retrieve delayed and real-time market data\" for the app in the API Centre, \
                 then generate a new token"
            } else {
                ""
            };
            bail!("Questrade {path}: HTTP {}{detail}{hint}", status.as_u16());
        }
        bail!("Questrade {path}: retry failed")
    }

    /// Wait for this request's slot so concurrent scans stay under the rate limit.
    async fn pace(&self) {
        let start = {
            let mut next = self.inner.next_slot.lock().await;
            let start = (*next).max(Instant::now());
            *next = start + REQUEST_SPACING;
            start
        };
        tokio::time::sleep_until(start.into()).await;
    }

    async fn symbol_id(&self, ticker: &str) -> Result<u64> {
        let key = ticker.to_ascii_uppercase();
        if let Some(id) = self.inner.ids.lock().expect("symbol cache").get(&key) {
            return Ok(*id);
        }
        let json = self.get("symbols/search", &[("prefix", key.clone())]).await?;
        let id = pick_symbol(&json, &key)?.ok_or_else(|| anyhow!("Questrade has no symbol {key}"))?;
        self.inner.ids.lock().expect("symbol cache").insert(key, id);
        Ok(id)
    }

    /// One daily candle for `ticker`, for Settings' connection test.
    pub async fn test(&self, ticker: &str) -> Result<Bar> {
        self.history(ticker, Timeframe::D1, None, 5).await?.pop().ok_or_else(|| anyhow!("no candles for {ticker}"))
    }
}

#[async_trait]
impl MarketData for Questrade {
    fn name(&self) -> &str {
        "questrade"
    }

    async fn history(&self, symbol: &str, tf: Timeframe, end: Option<i64>, limit: usize) -> Result<Vec<Bar>> {
        let id = self.symbol_id(symbol).await?;
        let mut until = end.unwrap_or_else(|| Utc::now().timestamp());
        let mut out: Vec<Bar> = Vec::with_capacity(limit);
        // Walk backwards in pages of at most MAX_CANDLES until `limit` bars are held.
        for _ in 0..limit.div_ceil(MAX_CANDLES) + 2 {
            if out.len() >= limit {
                break;
            }
            let from = until - window(tf, (limit - out.len()).min(MAX_CANDLES));
            let at = |ts: i64| {
                DateTime::<Utc>::from_timestamp(ts, 0).unwrap_or_default().to_rfc3339_opts(SecondsFormat::Secs, false)
            };
            let query = [("startTime", at(from)), ("endTime", at(until - 1)), ("interval", interval(tf).to_string())];
            let page = parse_candles(&self.get(&format!("markets/candles/{id}"), &query).await?)?;
            let Some(first) = page.first().map(|b| b.ts) else { break };
            out.extend(page);
            until = first;
        }
        out.sort_by_key(|b| b.ts);
        out.dedup_by_key(|b| b.ts);
        Ok(crate::clip(out, end, limit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_token_and_normalizes_the_server() {
        let json = r#"{"access_token":"C3lTUKuNQrAAmSD","token_type":"Bearer","expires_in":1800,
            "refresh_token":"aSBe7wAAdx88QTbw","api_server":"https://api01.iq.questrade.com/"}"#;
        let (access, ttl, refresh, server) = parse_token(json).unwrap();
        assert_eq!((access.as_str(), refresh.as_str()), ("C3lTUKuNQrAAmSD", "aSBe7wAAdx88QTbw"));
        assert_eq!((ttl, server.as_str()), (Duration::from_secs(1800), "https://api01.iq.questrade.com"));
        assert!(
            parse_token(r#"{"access_token":"","expires_in":1,"refresh_token":"x","api_server":"https://a"}"#).is_err()
        );
        assert!(
            parse_token(r#"{"access_token":"a","expires_in":1,"refresh_token":"x","api_server":"http://a"}"#).is_err()
        );
    }

    #[test]
    fn picks_the_exact_us_listing() {
        // Shape of the documented sample, which uses `symbol` for the list.
        let json = r#"{"symbol":[
            {"symbol":"BMO.PRJ.TO","symbolId":9300,"listingExchange":"TSX","currency":"CAD"},
            {"symbol":"BMO.TO","symbolId":9301,"listingExchange":"TSX","currency":"CAD"},
            {"symbol":"BMO","symbolId":9292,"listingExchange":"NYSE","currency":"USD"}]}"#;
        assert_eq!(pick_symbol(json, "bmo").unwrap(), Some(9292));
        assert_eq!(pick_symbol(json, "BMO.TO").unwrap(), Some(9301));
        assert_eq!(pick_symbol(r#"{"symbols":[]}"#, "BMO").unwrap(), None);
    }

    #[test]
    fn parses_candles_at_their_start_time() {
        let json = r#"{"candles":[
            {"start":"2014-01-02T00:00:00.000000-05:00","end":"2014-01-03T00:00:00.000000-05:00",
             "low":70.3,"high":70.78,"open":70.68,"close":70.73,"volume":983609}]}"#;
        let bars = parse_candles(json).unwrap();
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].ts, 1_388_638_800);
        assert_eq!((bars[0].close, bars[0].volume), (70.73, 983_609.0));
        assert!(parse_candles(r#"{"candles":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn api_errors_are_readable() {
        assert_eq!(
            api_error(r#"{"code":1017,"message":"Access token is invalid"}"#),
            " — Access token is invalid (code 1017)"
        );
        assert_eq!(api_error("<html>forbidden</html>"), "");
    }

    #[test]
    fn windows_cover_trading_days() {
        // 300 daily candles need well over 300 calendar days.
        assert!(window(Timeframe::D1, 300) > 420 * 86_400);
        assert!(window(Timeframe::M5, 100) > 100 * 300);
        assert_eq!(interval(Timeframe::M30), "HalfHour");
    }

    #[tokio::test]
    async fn token_exchange_errors_never_contain_the_token() {
        // Nothing listens on port 9: the exchange fails before any response.
        let q = Questrade::with_login(Some("SECRET-REFRESH-TOKEN".into()), Box::new(|_| Ok(())), "https://127.0.0.1:9")
            .unwrap();
        let error = format!("{:#}", q.history("AAPL", Timeframe::D1, None, 5).await.unwrap_err());
        assert!(error.contains("contacting Questrade login"), "{error}");
        assert!(!error.contains("SECRET-REFRESH-TOKEN"), "{error}");
    }

    #[tokio::test]
    async fn token_changes_are_persisted_and_forget_clears_state() {
        let saved = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = saved.clone();
        let sink: TokenSink = Box::new(move |t| {
            log.lock().unwrap().push(t.map(str::to_string));
            Ok(())
        });
        let q = Questrade::new(None, sink).unwrap();
        assert!(!q.is_configured());
        q.set_refresh_token(Some("  abc  ".into())).await.unwrap();
        assert!(q.is_configured());
        q.set_refresh_token(None).await.unwrap();
        assert!(!q.is_configured());
        assert_eq!(*saved.lock().unwrap(), vec![Some("abc".to_string()), None]);
        assert!(q.history("AAPL", Timeframe::D1, None, 5).await.unwrap_err().to_string().contains("not set up"));
    }
}
