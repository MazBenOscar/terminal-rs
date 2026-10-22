//! Venue-specific crypto spot data. Binance mirrors may fail over within Binance;
//! a failed request never changes the exchange, market type, or quote currency.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::DateTime;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tr_core::crypto::{CryptoInstrument, CryptoVenue};
use tr_core::{Bar, Security, Timeframe};

use crate::{LiveFeed, Quote};

const BINANCE_REST: [&str; 2] = ["https://data-api.binance.vision", "https://api.binance.com"];
const BINANCE_WS: [&str; 2] = ["wss://data-stream.binance.vision/ws", "wss://stream.binance.com:9443/ws"];
const COINBASE_REST: &str = "https://api.exchange.coinbase.com";
const COINBASE_WS: &str = "wss://ws-feed.exchange.coinbase.com";

/// Resolve legacy aliases once into a concrete venue and product. Unsupported market
/// types fail before any network access.
pub fn resolved_instrument(sec: &Security) -> Result<CryptoInstrument> {
    let instrument = CryptoInstrument::from_security(sec).map_err(anyhow::Error::msg)?;
    instrument.require_spot().map_err(anyhow::Error::msg)?;
    Ok(instrument)
}

/// Versioned route identity invalidates histories written before explicit venues.
pub fn route_key(sec: &Security) -> Result<String> {
    Ok(format!("crypto-v2:{}", resolved_instrument(sec)?.canonical_ticker()))
}

pub fn binance_interval(tf: Timeframe) -> &'static str {
    match tf {
        Timeframe::M2 | Timeframe::M3 => "1m",
        Timeframe::H2 => "1h",
        Timeframe::MN3 => "1M",
        Timeframe::M1 => "1m",
        Timeframe::M5 => "5m",
        Timeframe::M15 => "15m",
        Timeframe::M30 => "30m",
        Timeframe::H1 => "1h",
        Timeframe::H4 => "4h",
        Timeframe::D1 => "1d",
        Timeframe::W1 => "1w",
        Timeframe::MN1 => "1M",
    }
}

fn num(v: &Value) -> Option<f64> {
    let value = match v {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }?;
    value.is_finite().then_some(value)
}

fn valid_bar(b: &Bar) -> bool {
    b.ts > 0
        && [b.open, b.high, b.low, b.close, b.volume].iter().all(|v| v.is_finite())
        && b.low > 0.0
        && b.high >= b.open.max(b.close)
        && b.low <= b.open.min(b.close)
        && b.volume >= 0.0
}

fn checked_bars(mut bars: Vec<Bar>) -> Result<Vec<Bar>> {
    if bars.iter().any(|b| !valid_bar(b)) {
        bail!("invalid crypto candle values");
    }
    bars.sort_by_key(|b| b.ts);
    bars.dedup_by_key(|b| b.ts);
    Ok(bars)
}

/// Parse `/api/v3/klines`: arrays of `[open_ms, o, h, l, c, v, close_ms, ...]` with
/// prices as strings.
pub fn parse_binance_klines(json: &str) -> Result<Vec<Bar>> {
    let rows: Vec<Vec<Value>> = serde_json::from_str(json).context("parsing Binance klines")?;
    let bars: Option<Vec<_>> = rows
        .iter()
        .map(|r| {
            Some(Bar::new(
                r.first()?.as_i64()? / 1000,
                num(r.get(1)?)?,
                num(r.get(2)?)?,
                num(r.get(3)?)?,
                num(r.get(4)?)?,
                num(r.get(5)?)?,
            ))
        })
        .collect();
    checked_bars(bars.ok_or_else(|| anyhow!("malformed Binance candle"))?)
}

/// Parse `/api/v3/ticker/24hr`: `(symbol, last, open_24h)`.
pub fn parse_binance_24hr(json: &str) -> Result<Vec<(String, f64, f64)>> {
    let rows: Vec<Value> = serde_json::from_str(json).context("parsing Binance 24hr ticker")?;
    Ok(rows
        .iter()
        .filter_map(|r| Some((r["symbol"].as_str()?.to_string(), num(&r["lastPrice"])?, num(&r["openPrice"])?)))
        .collect())
}

/// Parse a websocket `kline` event into the current bar.
pub fn parse_binance_ws(text: &str) -> Option<Bar> {
    let v: Value = serde_json::from_str(text).ok()?;
    let k = v.get("k")?;
    let bar =
        Bar::new(k["t"].as_i64()? / 1000, num(&k["o"])?, num(&k["h"])?, num(&k["l"])?, num(&k["c"])?, num(&k["v"])?);
    valid_bar(&bar).then_some(bar)
}

/// Parse Coinbase `/products/{id}/candles`: `[time, low, high, open, close, volume]`,
/// newest first. Returned ascending.
pub fn parse_coinbase_candles(json: &str) -> Result<Vec<Bar>> {
    let rows: Vec<Vec<Value>> = serde_json::from_str(json).context("parsing Coinbase candles")?;
    let bars: Option<Vec<_>> = rows
        .iter()
        .map(|r| {
            Some(Bar::new(
                r.first()?.as_i64()?,
                num(r.get(3)?)?,
                num(r.get(2)?)?,
                num(r.get(1)?)?,
                num(r.get(4)?)?,
                num(r.get(5)?)?,
            ))
        })
        .collect();
    checked_bars(bars.ok_or_else(|| anyhow!("malformed Coinbase candle"))?)
}

/// Parse Coinbase `/products/{id}/stats`: `(last, open_24h)`.
pub fn parse_coinbase_stats(json: &str) -> Result<(f64, f64)> {
    let v: Value = serde_json::from_str(json).context("parsing Coinbase stats")?;
    Ok((num(&v["last"]).ok_or_else(|| anyhow!("no last"))?, num(&v["open"]).ok_or_else(|| anyhow!("no open"))?))
}

/// Parse a Coinbase websocket `ticker` message: `(unix_secs, price, size)`.
pub fn parse_coinbase_ws(text: &str) -> Option<(i64, f64, f64)> {
    let v: Value = serde_json::from_str(text).ok()?;
    if v["type"] != "ticker" {
        return None;
    }
    let ts = DateTime::parse_from_rfc3339(v["time"].as_str()?).ok()?.timestamp();
    let price = num(&v["price"])?;
    let size = num(&v["last_size"])?;
    (ts > 0 && price > 0.0 && size >= 0.0).then_some((ts, price, size))
}

/// Coinbase granularity used to build `tf` bars, and whether resampling is needed.
fn coinbase_granularity(tf: Timeframe) -> (i64, bool) {
    match tf {
        Timeframe::M2 | Timeframe::M3 => (60, true),
        Timeframe::H2 => (3600, true),
        Timeframe::MN3 => (86_400, true),
        Timeframe::M1 => (60, false),
        Timeframe::M5 => (300, false),
        Timeframe::M15 => (900, false),
        Timeframe::M30 => (900, true),
        Timeframe::H1 => (3600, false),
        Timeframe::H4 => (3600, true),
        Timeframe::D1 => (86_400, false),
        Timeframe::W1 | Timeframe::MN1 => (86_400, true),
    }
}

fn parse_levels(v: &Value) -> Result<Vec<crate::Level>> {
    let rows = v.as_array().ok_or_else(|| anyhow!("missing crypto book levels"))?;
    rows.iter()
        .map(|level| {
            let price = level.get(0).and_then(num).ok_or_else(|| anyhow!("invalid book price"))?;
            let size = level.get(1).and_then(num).ok_or_else(|| anyhow!("invalid book size"))?;
            if price <= 0.0 || size < 0.0 {
                bail!("invalid crypto book level");
            }
            Ok((price, size))
        })
        .collect()
}

/// Parse a Binance `/api/v3/depth` snapshot.
pub fn parse_binance_depth(json: &str, symbol: &str) -> Result<crate::OrderBook> {
    let v: serde_json::Value = serde_json::from_str(json).context("parsing Binance depth")?;
    if v["bids"].is_null() {
        bail!("unexpected Binance depth payload");
    }
    Ok(crate::OrderBook {
        symbol: symbol.into(),
        bids: parse_levels(&v["bids"])?,
        asks: parse_levels(&v["asks"])?,
        source: "Binance".into(),
        simulated: false,
    })
}

/// Parse a Coinbase Exchange level-2 `/book` snapshot (`[price, size, orders]`).
pub fn parse_coinbase_book(json: &str, symbol: &str, depth: usize) -> Result<crate::OrderBook> {
    let v: serde_json::Value = serde_json::from_str(json).context("parsing Coinbase book")?;
    if v["bids"].is_null() {
        bail!("unexpected Coinbase book payload");
    }
    let mut b = crate::OrderBook {
        symbol: symbol.into(),
        bids: parse_levels(&v["bids"])?,
        asks: parse_levels(&v["asks"])?,
        source: "Coinbase".into(),
        simulated: false,
    };
    b.bids.truncate(depth);
    b.asks.truncate(depth);
    Ok(b)
}

pub struct CryptoProvider {
    client: reqwest::Client,
    catalogs: crate::crypto_catalog::CatalogCache,
    metric_slots: tokio::sync::Semaphore,
    #[cfg(test)]
    test_rest: Option<String>,
}

impl CryptoProvider {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("terminal-rs/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self {
            client,
            catalogs: Default::default(),
            metric_slots: tokio::sync::Semaphore::new(4),
            #[cfg(test)]
            test_rest: None,
        })
    }

    pub async fn crypto_catalog(&self, venue: CryptoVenue) -> Result<crate::crypto_catalog::Catalog> {
        self.catalogs.load(&self.client, venue).await
    }

    /// Market-data statistics only; the semaphore and deadline also bound time
    /// spent waiting behind another scan's requests.
    pub async fn crypto_stats(&self, symbol: &str) -> Result<crate::crypto_catalog::Rolling24h> {
        let route = resolved_instrument(&crate::security(symbol))?;
        tokio::time::timeout(Duration::from_secs(20), async {
            let _permit = self.metric_slots.acquire().await.context("crypto metric limiter closed")?;
            if route.venue == CryptoVenue::Coinbase {
                let body = self
                    .get(&format!("{}/products/{}/stats", self.quote_host(COINBASE_REST), route.native_symbol), &[])
                    .await?;
                return crate::crypto_catalog::parse_rolling(&route, &body, chrono::Utc::now().timestamp());
            }
            let mut last_error = anyhow!("no Binance statistics response");
            for host in BINANCE_REST {
                match async {
                    let body = self
                        .get(
                            &format!("{}/api/v3/ticker/24hr", self.quote_host(host)),
                            &[("symbol", route.native_symbol.clone())],
                        )
                        .await?;
                    crate::crypto_catalog::parse_rolling(&route, &body, chrono::Utc::now().timestamp())
                }
                .await
                {
                    Ok(statistics) => return Ok(statistics),
                    Err(error) => last_error = error,
                }
            }
            Err(last_error)
        })
        .await
        .context("crypto rolling statistics timed out")?
    }

    async fn get(&self, url: &str, query: &[(&str, String)]) -> Result<String> {
        let mut resp = self.client.get(url).query(query).send().await?;
        if !resp.status().is_success() {
            bail!("GET {url}: HTTP {}", resp.status());
        }
        const MAX_RESPONSE: usize = 8 * 1024 * 1024;
        if resp.content_length().is_some_and(|n| n > MAX_RESPONSE as u64) {
            bail!("crypto response exceeds size limit");
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await? {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE {
                bail!("crypto response exceeds size limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        String::from_utf8(bytes).context("crypto response is not UTF-8")
    }

    async fn binance_history(
        &self,
        host: &str,
        symbol: &str,
        tf: Timeframe,
        end: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Bar>> {
        let mut out: Vec<Bar> = Vec::new();
        let mut end_ms = end.map(|e| e.saturating_mul(1000).saturating_sub(1));
        for _ in 0..100 {
            if out.len() >= limit {
                break;
            }
            let n = (limit - out.len()).min(1000);
            let mut q = vec![
                ("symbol", symbol.to_string()),
                ("interval", binance_interval(tf).into()),
                ("limit", n.to_string()),
            ];
            if let Some(e) = end_ms {
                q.push(("endTime", e.to_string()));
            }
            let page = parse_binance_klines(&self.get(&format!("{host}/api/v3/klines"), &q).await?)?;
            let Some(first) = page.first() else { break };
            let next = first.ts.saturating_mul(1000).saturating_sub(1);
            if end_ms.is_some_and(|previous| next >= previous) {
                bail!("Binance history did not advance");
            }
            end_ms = Some(next);
            let got = page.len();
            out.splice(0..0, page);
            if got < n {
                break;
            }
        }
        Ok(out)
    }

    async fn coinbase_history(&self, product: &str, tf: Timeframe, end: Option<i64>, limit: usize) -> Result<Vec<Bar>> {
        let (gran, resample) = coinbase_granularity(tf);
        let per_bar = (tf.seconds() / gran).max(1) as usize;
        let want = limit.saturating_mul(per_bar);
        let mut end_ts = end.unwrap_or_else(|| chrono::Utc::now().timestamp());
        let mut out: Vec<Bar> = Vec::new();
        // Coinbase returns at most 300 candles per request; bound the walk.
        for _ in 0..40 {
            if out.len() >= want {
                break;
            }
            let start_ts = end_ts - 300 * gran;
            let fmt = |t: i64| DateTime::from_timestamp(t, 0).map(|d| d.to_rfc3339()).unwrap_or_default();
            let q = [("granularity", gran.to_string()), ("start", fmt(start_ts)), ("end", fmt(end_ts))];
            let page =
                parse_coinbase_candles(&self.get(&format!("{COINBASE_REST}/products/{product}/candles"), &q).await?)?;
            if page.is_empty() {
                break;
            }
            end_ts = page[0].ts - 1;
            let first = out.first().map(|f| f.ts);
            out.splice(0..0, page.into_iter().filter(|b| first.is_none_or(|f| b.ts < f)));
        }
        if let Some(e) = end {
            out.retain(|b| b.ts < e);
        }
        if resample {
            out = tf.resample(&out);
        }
        Ok(crate::clip(out, None, limit))
    }

    /// Crypto headlines from CoinDesk, narrowed to the asset when it is mentioned.
    pub async fn news(&self, sec: &Security) -> Result<Vec<crate::NewsItem>> {
        let items = crate::rss::parse_rss(&self.get(crate::rss::COINDESK, &[]).await?, "CoinDesk")?;
        let instrument = resolved_instrument(sec)?;
        let base = instrument.base.as_str();
        let name = sec.name().unwrap_or(base).split(['/', ' ']).next().unwrap_or(base).to_string();
        Ok(crate::rss::filter_relevant(items, &[base, &name]))
    }

    /// Order book snapshot from the requested venue only.
    pub async fn order_book(&self, key: &str, sec: &Security, depth: usize) -> Result<crate::OrderBook> {
        let route = resolved_instrument(sec)?;
        let depth = depth.clamp(1, 500);
        let limit = [5, 10, 20, 50, 100, 500].into_iter().find(|&l| l >= depth).unwrap_or(500);
        if route.venue == CryptoVenue::Coinbase {
            let text = self
                .get(&format!("{COINBASE_REST}/products/{}/book", route.native_symbol), &[("level", "2".into())])
                .await?;
            return parse_coinbase_book(&text, key, depth);
        }
        let mut last_error = None;
        for host in BINANCE_REST {
            let q = [("symbol", route.native_symbol.clone()), ("limit", limit.to_string())];
            match self.get(&format!("{host}/api/v3/depth"), &q).await.and_then(|j| parse_binance_depth(&j, key)) {
                Ok(mut book) => {
                    book.bids.truncate(depth);
                    book.asks.truncate(depth);
                    return Ok(book);
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("no Binance depth response")))
    }

    pub async fn history(&self, sec: &Security, tf: Timeframe, end: Option<i64>, limit: usize) -> Result<Vec<Bar>> {
        let route = resolved_instrument(sec)?;
        if end.is_some_and(|ts| ts <= 0 || DateTime::from_timestamp(ts, 0).is_none()) {
            bail!("invalid crypto history end timestamp");
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        if limit > 100_000 {
            bail!("crypto history limit exceeds 100000 bars");
        }
        if let Some((base, factor)) = tf.source_interval() {
            let count = limit.saturating_add(1).saturating_mul(factor).min(100_000);
            let bars = Box::pin(self.history(sec, base, end, count)).await?;
            return Ok(crate::clip(tf.resample(&bars), end, limit));
        }
        if route.venue == CryptoVenue::Coinbase {
            let bars = self.coinbase_history(&route.native_symbol, tf, end, limit).await?;
            if bars.is_empty() {
                bail!("no Coinbase candles for {}", route.native_symbol);
            }
            return checked_bars(bars);
        }
        let mut last_error = None;
        for host in BINANCE_REST {
            match self.binance_history(host, &route.native_symbol, tf, end, limit).await.and_then(checked_bars) {
                Ok(bars) if !bars.is_empty() => return Ok(crate::clip(bars, end, limit)),
                Ok(_) => last_error = Some(anyhow!("no Binance candles for {}", route.native_symbol)),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("no Binance history response")))
    }

    fn quote_host<'a>(&'a self, default: &'a str) -> &'a str {
        #[cfg(test)]
        if let Some(host) = &self.test_rest {
            return host;
        }
        default
    }

    async fn quote_for_route(&self, route: &CryptoInstrument) -> Result<Quote> {
        let (last, prev) = if route.venue == CryptoVenue::Coinbase {
            let text = self
                .get(&format!("{}/products/{}/stats", self.quote_host(COINBASE_REST), route.native_symbol), &[])
                .await?;
            parse_coinbase_stats(&text)?
        } else {
            let mut result = Err(anyhow!("no Binance quote response"));
            for host in BINANCE_REST {
                let host = self.quote_host(host);
                result = async {
                    let text = self
                        .get(&format!("{host}/api/v3/ticker/24hr"), &[("symbol", route.native_symbol.clone())])
                        .await?;
                    let value: Value = serde_json::from_str(&text)?;
                    if value["symbol"].as_str() != Some(route.native_symbol.as_str()) {
                        bail!("wrong Binance quote product");
                    }
                    Ok((
                        num(&value["lastPrice"]).ok_or_else(|| anyhow!("invalid last price"))?,
                        num(&value["openPrice"]).ok_or_else(|| anyhow!("invalid open price"))?,
                    ))
                }
                .await;
                if result.is_ok() {
                    break;
                }
            }
            result?
        };
        if !last.is_finite() || !prev.is_finite() || last <= 0.0 || prev <= 0.0 {
            bail!("invalid crypto quote for {}", route.canonical_ticker());
        }
        Ok(Quote {
            symbol: route.security().key(),
            last,
            prev_close: prev,
            ts: chrono::Utc::now().timestamp(),
            spark: Vec::new(),
            crypto_source: Some(route.clone()),
        })
    }

    /// Bound work by concrete product, isolate failed products, and cancel pending
    /// requests at the batch deadline. Duplicate aliases reuse one observation.
    pub async fn quotes(&self, secs: &[(String, Security)]) -> Result<Vec<Quote>> {
        let mut routes: std::collections::BTreeMap<String, (CryptoInstrument, Vec<String>)> =
            std::collections::BTreeMap::new();
        for (key, sec) in secs {
            match resolved_instrument(sec) {
                Ok(route) => {
                    let id = route.canonical_ticker();
                    if routes.len() >= 256 && !routes.contains_key(&id) {
                        continue;
                    }
                    routes.entry(id).or_insert_with(|| (route, Vec::new())).1.push(key.clone());
                }
                Err(error) => tracing::debug!("crypto quote {key}: {error:#}"),
            }
        }
        let mut requests = futures::stream::iter(routes.into_values())
            .map(|(route, keys)| async move { (keys, self.quote_for_route(&route).await) })
            .buffer_unordered(8);
        let deadline = tokio::time::sleep(Duration::from_secs(30));
        tokio::pin!(deadline);
        let mut out = Vec::new();
        loop {
            let result = tokio::select! {
                _ = &mut deadline => break,
                result = requests.next() => result,
            };
            let Some((keys, result)) = result else { break };
            match result {
                Ok(quote) => out.extend(keys.into_iter().map(|symbol| Quote { symbol, ..quote.clone() })),
                Err(error) => tracing::debug!("crypto quote failed: {error:#}"),
            }
        }
        if out.is_empty() && !secs.is_empty() {
            bail!("no crypto quotes available");
        }
        Ok(out)
    }

    /// Reconnect only within the resolved venue. Invalid/unsupported instruments
    /// produce no feed instead of switching products.
    pub fn live(sec: &Security, tf: Timeframe) -> Option<LiveFeed> {
        let route = resolved_instrument(sec).ok()?;
        let endpoints = match route.venue {
            CryptoVenue::Binance => BINANCE_WS.iter().map(|url| (*url).to_string()).collect(),
            CryptoVenue::Coinbase => vec![COINBASE_WS.to_string()],
        };
        Some(live_with(route, tf, endpoints, Duration::from_secs(30)))
    }
}

struct Recovery {
    backoff: u64,
    healthy: bool,
    failed: bool,
    last: Option<i64>,
}

impl Recovery {
    fn new() -> Self {
        Self { backoff: 1, healthy: false, failed: false, last: None }
    }

    async fn healthy(&mut self, venue: &str, tx: &mpsc::Sender<crate::LiveUpdate>) -> Result<()> {
        if self.healthy && self.failed {
            tx.send(crate::LiveUpdate::Status(format!("{venue} live stream restored; filling missed bars"))).await?;
            tx.send(crate::LiveUpdate::Reconnected { since: self.last }).await?;
        }
        self.healthy = true;
        self.failed = false;
        self.backoff = 1;
        Ok(())
    }
}

fn next_backoff(seconds: u64) -> u64 {
    seconds.saturating_mul(2).min(30)
}

fn live_with(route: CryptoInstrument, tf: Timeframe, endpoints: Vec<String>, idle: Duration) -> LiveFeed {
    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(async move {
        let mut recovery = Recovery::new();
        let mut attempt = 0usize;
        let venue = match route.venue {
            CryptoVenue::Binance => "Binance",
            CryptoVenue::Coinbase => "Coinbase",
        };
        while !tx.is_closed() {
            let endpoint = &endpoints[attempt % endpoints.len()];
            let url = match route.venue {
                CryptoVenue::Binance => {
                    format!("{endpoint}/{}@kline_{}", route.native_symbol.to_ascii_lowercase(), binance_interval(tf))
                }
                CryptoVenue::Coinbase => endpoint.clone(),
            };
            tokio::select! {
                biased;
                _ = tx.closed() => break,
                _ = crypto_ws(&url, &route, tf, &tx, &mut recovery, idle) => {}
            }
            if tx.is_closed() {
                break;
            }
            recovery.failed = true;
            let message = if recovery.healthy {
                format!("{venue} live stream disconnected; reconnecting…")
            } else {
                format!("{venue} live stream unavailable; retrying…")
            };
            if tx.send(crate::LiveUpdate::Status(message)).await.is_err() {
                break;
            }
            attempt = attempt.wrapping_add(1);
            tokio::select! {
                biased;
                _ = tx.closed() => break,
                _ = tokio::time::sleep(Duration::from_secs(recovery.backoff)) => {}
            }
            recovery.backoff = next_backoff(recovery.backoff);
        }
    });
    rx
}

async fn crypto_ws(
    url: &str,
    route: &CryptoInstrument,
    tf: Timeframe,
    tx: &mpsc::Sender<crate::LiveUpdate>,
    recovery: &mut Recovery,
    idle: Duration,
) -> Result<()> {
    let (mut ws, _) = tokio::time::timeout(Duration::from_secs(20), tokio_tungstenite::connect_async(url)).await??;
    let venue = match route.venue {
        CryptoVenue::Binance => "Binance",
        CryptoVenue::Coinbase => {
            let sub = serde_json::json!({
                "type": "subscribe", "product_ids": [&route.native_symbol],
                "channels": ["ticker", "heartbeats"]
            });
            ws.send(Message::Text(sub.to_string().into())).await?;
            "Coinbase"
        }
    };
    // Each connection starts a fresh aggregate; history/backfill merges preserve the original open.
    let mut aggregate: Option<Bar> = None;
    while let Some(msg) = tokio::time::timeout(idle, ws.next()).await? {
        let text = match msg? {
            Message::Text(text) => text,
            Message::Close(_) => break,
            // Tungstenite queues Pong automatically on read; flush it without sending a duplicate.
            Message::Ping(_) => {
                ws.flush().await?;
                continue;
            }
            _ => continue,
        };
        let value: Value = serde_json::from_str(text.as_str())?;
        let bar = match route.venue {
            CryptoVenue::Binance => {
                if value["k"]["s"].as_str() != Some(&route.native_symbol) {
                    continue;
                }
                let Some(bar) = parse_binance_ws(text.as_str()) else { continue };
                bar
            }
            CryptoVenue::Coinbase => {
                if value["product_id"].as_str() != Some(&route.native_symbol) {
                    continue;
                }
                if value["type"] == "heartbeat" {
                    recovery.healthy(venue, tx).await?;
                    continue;
                }
                let Some((ts, px, size)) = parse_coinbase_ws(text.as_str()) else { continue };
                let bucket = tf.bucket_start(ts);
                match aggregate.as_mut() {
                    Some(b) if b.ts == bucket => {
                        b.high = b.high.max(px);
                        b.low = b.low.min(px);
                        b.close = px;
                        b.volume += size;
                        *b
                    }
                    _ => *aggregate.insert(Bar::new(bucket, px, px, px, px, size)),
                }
            }
        };
        recovery.healthy(venue, tx).await?;
        tx.send(crate::LiveUpdate::Partial(bar)).await?;
        recovery.last = Some(bar.ts);
    }
    bail!("crypto websocket closed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_book_fixtures() {
        let b = parse_binance_depth(include_str!("../tests/fixtures/binance_depth.json"), "BTCUSD Curncy").unwrap();
        assert_eq!((b.bids.len(), b.asks.len()), (5, 5));
        assert!(b.bids.windows(2).all(|w| w[0].0 > w[1].0));
        assert!(b.asks.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(b.spread().unwrap() > 0.0 && !b.simulated);
        let c = parse_coinbase_book(include_str!("../tests/fixtures/coinbase_book.json"), "BTCUSD Curncy", 3).unwrap();
        assert_eq!((c.bids.len(), c.asks.len()), (3, 3));
        assert!(c.bids[0].0 < c.asks[0].0);
        assert_eq!(c.source, "Coinbase");
    }
    use tr_core::AssetClass;

    const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    fn fix(name: &str) -> String {
        std::fs::read_to_string(format!("{FIX}/{name}")).unwrap()
    }

    #[test]
    fn symbols_preserve_venue_and_quote_currency() {
        let btc = Security::new("BTCUSD", AssetClass::Crypto);
        let route = resolved_instrument(&btc).unwrap();
        assert_eq!(route.venue, CryptoVenue::Coinbase);
        assert_eq!(route.native_symbol, "BTC-USD");
        let usdt = Security::new("BTCUSDT", AssetClass::Crypto);
        assert_eq!(resolved_instrument(&usdt).unwrap().native_symbol, "BTCUSDT");
        assert_ne!(route_key(&btc).unwrap(), route_key(&usdt).unwrap());
        assert_eq!(
            route_key(&btc).unwrap(),
            route_key(&Security::new("COINBASE:BTC-USD", AssetClass::Crypto)).unwrap()
        );
        assert!(resolved_instrument(&Security::new("BINANCE:BTCUSDT.P", AssetClass::Crypto)).is_err());
        assert_eq!(binance_interval(Timeframe::MN1), "1M");
    }

    #[test]
    fn rejects_invalid_candle_values() {
        assert!(checked_bars(vec![Bar::new(1, 2.0, 3.0, 1.0, f64::NAN, 1.0)]).is_err());
        assert!(checked_bars(vec![Bar::new(1, 2.0, 3.0, 1.0, 2.0, -1.0)]).is_err());
        assert!(checked_bars(vec![Bar::new(1, 2.0, 1.0, 1.0, 2.0, 1.0)]).is_err());
    }

    #[test]
    fn malformed_exchange_payloads_fail_closed() {
        assert!(parse_binance_klines("[[1000,\"NaN\",2,1,2,1]]").is_err());
        assert!(parse_binance_klines("[[1000]]").is_err());
        assert!(parse_coinbase_candles("[[1000,1,3,2,2,-1]]").is_err());
        assert!(parse_coinbase_candles("[[1000.5,1,3,2,2,1]]").is_err());
        assert!(parse_binance_depth(r#"{ "bids": [["NaN","1"]], "asks": [] }"#, "BTC").is_err());
        assert!(parse_binance_depth(r#"{ "bids": [], "asks": null }"#, "BTC").is_err());
    }

    #[tokio::test]
    async fn rejects_http_failure_and_oversized_payload_before_decoding() {
        use std::io::{Read, Write};
        for response in [
            "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 99999999\r\nConnection: close\r\n\r\n",
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut buffer = [0u8; 4096];
                let _ = socket.read(&mut buffer);
                socket.write_all(response.as_bytes()).unwrap();
            });
            let error = CryptoProvider::new().unwrap().get(&url, &[]).await.unwrap_err();
            assert!(error.to_string().contains("429") || error.to_string().contains("size limit"));
            server.join().unwrap();
        }
    }

    #[tokio::test]
    async fn quotes_isolate_failed_products_and_share_alias_observations() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut request = [0; 4096];
                let len = socket.read(&mut request).unwrap();
                let valid = String::from_utf8_lossy(&request[..len]).contains("BTC-USD");
                let (status, body) =
                    if valid { ("200 OK", r#"{"last":"100","open":"95"}"#) } else { ("404 Not Found", "{}") };
                let response =
                    format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                socket.write_all(response.as_bytes()).unwrap();
            }
        });
        let mut provider = CryptoProvider::new().unwrap();
        provider.test_rest = Some(host);
        let keys = ["BTCUSD", "COINBASE:BTC-USD", "ZZZUSD", "BAD:BTCUSD"];
        let secs: Vec<_> = keys.iter().map(|key| (key.to_string(), Security::new(*key, AssetClass::Crypto))).collect();
        let quotes = provider.quotes(&secs).await.unwrap();
        assert_eq!(quotes.len(), 2);
        assert!(quotes.iter().all(|q| q.last == 100.0 && q.crypto_source.as_ref().unwrap().native_symbol == "BTC-USD"));
        assert_eq!(quotes[0].ts, quotes[1].ts);
        server.join().unwrap();
    }

    /// Explicit network contract check. Restricted venues fail visibly and never
    /// borrow a different exchange's product. Run manually with --ignored.
    #[tokio::test]
    #[ignore = "public exchange network availability and regional restrictions"]
    async fn live_catalog_quote_history_keep_exact_venue() {
        let provider = CryptoProvider::new().unwrap();
        for venue in [CryptoVenue::Binance, CryptoVenue::Coinbase] {
            let catalog = provider
                .crypto_catalog(venue)
                .await
                .unwrap_or_else(|error| panic!("{} catalog: {error:#}", venue.label()));
            let product = catalog
                .products
                .iter()
                .find(|p| p.base == "BTC" && p.quote == if venue == CryptoVenue::Binance { "USDT" } else { "USD" })
                .expect("BTC spot product");
            let key = product.security().key();
            let quotes = provider.quotes(&[(key, product.security())]).await.unwrap();
            let source = quotes[0].crypto_source.as_ref().unwrap();
            assert_eq!(source.canonical_ticker(), product.canonical_ticker());
            assert_eq!(source.venue, venue);
            let statistics = provider.crypto_stats(&product.security().key()).await.unwrap();
            statistics.validate().unwrap();
            assert_eq!(statistics.instrument.canonical_ticker(), product.canonical_ticker());
            eprintln!(
                "{} rolling24h: base volume {}, quote volume {:?}, change {:?}",
                venue.label(),
                statistics.base_volume,
                statistics.quote_volume,
                statistics.change_percent
            );
            let bars = provider.history(&product.security(), Timeframe::H1, None, 10).await.unwrap();
            assert!(!bars.is_empty());
            assert!(bars.iter().all(valid_bar));
            eprintln!(
                "{}: {} products, {} history bars, quote source {}",
                venue.label(),
                catalog.products.len(),
                bars.len(),
                source.canonical_ticker()
            );
        }
    }

    #[test]
    fn binance_fixtures() {
        let bars = parse_binance_klines(&fix("binance_klines.json")).unwrap();
        assert_eq!(bars.len(), 5);
        assert!(bars.windows(2).all(|w| w[1].ts - w[0].ts == 3600));
        assert!(bars.iter().all(|b| b.low <= b.close && b.high >= b.close && b.close > 1000.0));
        let t = parse_binance_24hr(&fix("binance_24hr.json")).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].0, "BTCUSDT");
        assert!(t[0].1 > 0.0 && t[0].2 > 0.0);
        let k = parse_binance_ws(&fix("binance_ws_kline.json")).unwrap();
        assert_eq!(k.ts, 1_791_778_980);
        assert_eq!(k.close, 83592.01);
        assert!(parse_binance_ws("{\"result\":null}").is_none());
    }

    #[test]
    fn coinbase_fixtures() {
        let bars = parse_coinbase_candles(&fix("coinbase_candles.json")).unwrap();
        assert_eq!(bars.len(), 5);
        assert!(bars.windows(2).all(|w| w[0].ts < w[1].ts));
        assert!(bars.iter().all(|b| b.low <= b.open && b.high >= b.open));
        let (last, open) = parse_coinbase_stats(&fix("coinbase_stats.json")).unwrap();
        assert_eq!((last, open), (2543.56, 2504.69));
        let (ts, px, size) = parse_coinbase_ws(&fix("coinbase_ws_ticker.json")).unwrap();
        assert_eq!(px, 83601.17);
        assert_eq!(size, 0.0012);
        assert_eq!(ts, 1_791_777_838);
        assert!(parse_coinbase_ws("{\"type\":\"subscriptions\"}").is_none());
    }

    async fn loopback() -> (tokio::net::TcpListener, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        (listener, url)
    }

    fn test_feed(symbol: &str, url: String, idle: Duration) -> LiveFeed {
        let route = resolved_instrument(&Security::new(symbol, AssetClass::Crypto)).unwrap();
        live_with(route, Timeframe::M1, vec![url], idle)
    }

    async fn update(rx: &mut LiveFeed) -> crate::LiveUpdate {
        tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.unwrap().unwrap()
    }

    fn kline(ts: i64) -> Message {
        Message::Text(
            serde_json::json!({
                "k": {"s": "BTCUSDT", "t": ts * 1000, "o": "2", "h": "3",
                      "l": "1", "c": "2", "v": "1"}
            })
            .to_string()
            .into(),
        )
    }

    #[tokio::test]
    async fn binance_recovers_after_valid_message_with_last_delivered_bucket() {
        let (listener, url) = loopback().await;
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(kline(60)).await.unwrap();
            ws.send(kline(120)).await.unwrap();
            ws.close(None).await.unwrap();
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(kline(180)).await.unwrap();
            while ws.next().await.is_some_and(|m| m.is_ok()) {}
        });
        let mut rx = test_feed("BINANCE:BTCUSDT", url, Duration::from_secs(2));
        for ts in [60, 120] {
            assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == ts));
        }
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Status(s)
            if s == "Binance live stream disconnected; reconnecting…"));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Status(s)
            if s == "Binance live stream restored; filling missed bars"));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Reconnected { since: Some(120) }));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == 180));
        assert!(tokio::time::timeout(Duration::from_millis(50), rx.recv()).await.is_err());
        drop(rx);
        tokio::time::timeout(Duration::from_secs(1), server).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn idle_timeout_retries_and_receiver_drop_stops_attempts() {
        let (listener, url) = loopback().await;
        let mut rx = test_feed("BINANCE:BTCUSDT", url, Duration::from_millis(100));
        let (tcp, _) = listener.accept().await.unwrap();
        let _first = tokio_tungstenite::accept_async(tcp).await.unwrap();
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Status(s)
            if s == "Binance live stream unavailable; retrying…"));
        let (tcp, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept()).await.unwrap().unwrap();
        let _second = tokio_tungstenite::accept_async(tcp).await.unwrap();
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Status(_)));
        drop(rx);
        assert!(tokio::time::timeout(Duration::from_millis(2100), listener.accept()).await.is_err());
    }

    #[tokio::test]
    async fn coinbase_heartbeats_keep_idle_product_healthy_and_ping_has_one_pong() {
        let (listener, url) = loopback().await;
        let mut rx = test_feed("COINBASE:BTC-USD", url, Duration::from_millis(200));
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let Message::Text(sub) = ws.next().await.unwrap().unwrap() else { panic!("missing subscribe") };
        let sub: Value = serde_json::from_str(sub.as_str()).unwrap();
        assert_eq!(sub["product_ids"], serde_json::json!(["BTC-USD"]));
        assert_eq!(sub["channels"], serde_json::json!(["ticker", "heartbeats"]));
        ws.send(Message::Ping(vec![1, 2].into())).await.unwrap();
        assert!(matches!(tokio::time::timeout(Duration::from_millis(100), ws.next()).await.unwrap(),
            Some(Ok(Message::Pong(bytes))) if bytes.as_ref() == [1, 2]));
        for _ in 0..6 {
            ws.send(Message::Text(
                serde_json::json!({
                    "type": "heartbeat", "product_id": "BTC-USD"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
            let mut other: Value = serde_json::from_str(&fix("coinbase_ws_ticker.json")).unwrap();
            other["product_id"] = serde_json::json!("ETH-USD");
            ws.send(Message::Text(other.to_string().into())).await.unwrap();
            assert!(tokio::time::timeout(Duration::from_millis(50), rx.recv()).await.is_err());
        }
        assert!(tokio::time::timeout(Duration::from_millis(20), ws.next()).await.is_err());
        assert!(tokio::time::timeout(Duration::from_millis(20), listener.accept()).await.is_err());
        drop(rx);
        assert!(tokio::time::timeout(Duration::from_millis(100), ws.next()).await.unwrap().is_some());
        assert!(tokio::time::timeout(Duration::from_millis(1100), listener.accept()).await.is_err());
    }

    #[tokio::test]
    async fn failed_initial_handshake_is_unavailable_not_reconnected() {
        let (listener, url) = loopback().await;
        let mut rx = test_feed("BINANCE:BTCUSDT", url, Duration::from_secs(2));
        let (tcp, _) = listener.accept().await.unwrap();
        drop(tcp);
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Status(s)
            if s == "Binance live stream unavailable; retrying…"));
        let (tcp, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept()).await.unwrap().unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        ws.send(kline(60)).await.unwrap();
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == 60));
        assert!(tokio::time::timeout(Duration::from_millis(50), rx.recv()).await.is_err());
    }

    #[tokio::test]
    async fn backoff_caps_and_resets_only_after_healthy() {
        let mut recovery = Recovery::new();
        let mut delays = vec![recovery.backoff];
        for _ in 0..6 {
            recovery.backoff = next_backoff(recovery.backoff);
            delays.push(recovery.backoff);
        }
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30]);
        let (tx, mut rx) = mpsc::channel(4);
        recovery.failed = true;
        recovery.healthy("Coinbase", &tx).await.unwrap();
        assert_eq!(recovery.backoff, 1);
        assert!(rx.try_recv().is_err());
        recovery.failed = true;
        recovery.backoff = 30;
        recovery.healthy("Coinbase", &tx).await.unwrap();
        assert_eq!(recovery.backoff, 1);
        assert!(matches!(rx.try_recv().unwrap(), crate::LiveUpdate::Status(s)
            if s == "Coinbase live stream restored; filling missed bars"));
        assert!(matches!(rx.try_recv().unwrap(), crate::LiveUpdate::Reconnected { since: None }));
        recovery.healthy("Coinbase", &tx).await.unwrap();
        assert!(rx.try_recv().is_err());
    }

    // ---- adversarial reconnect-recovery tests (task C03) ----

    fn kline_for(symbol: &str, ts: i64) -> Message {
        Message::Text(
            serde_json::json!({
                "k": {"s": symbol, "t": ts * 1000, "o": "2", "h": "3",
                      "l": "1", "c": "2", "v": "1"}
            })
            .to_string()
            .into(),
        )
    }

    /// Probe 1: a non-JSON text frame must not tear down an otherwise healthy
    /// Binance connection. One garbage frame forces a full reconnect cycle
    /// (status churn, backoff, Reconnected backfill) for no reason.
    #[tokio::test]
    #[ignore = "BUG: non-JSON text frame tears down an otherwise healthy connection via serde_json::from_str(text)?"]
    async fn non_json_text_frame_should_not_kill_connection() {
        let (listener, url) = loopback().await;
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(kline(60)).await.unwrap();
            // Garbage that is valid WebSocket text but not JSON.
            ws.send(Message::Text("this is not json".into())).await.unwrap();
            // Connection should survive and keep delivering bars.
            ws.send(kline(120)).await.unwrap();
            ws.send(kline(180)).await.unwrap();
            while ws.next().await.is_some_and(|m| m.is_ok()) {}
        });
        let mut rx = test_feed("BINANCE:BTCUSDT", url, Duration::from_secs(2));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == 60));
        // BUG: the next event is a disconnect status instead of the next Partial.
        // Correct behaviour: Partial(120) arrives and the connection stays up.
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == 120),
            "connection must survive a non-JSON text frame");
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == 180));
        drop(rx);
        let _ = tokio::time::timeout(Duration::from_secs(1), server).await;
    }

    /// Probe 1b: a JSON frame that is not an object must also be skipped.
    #[tokio::test]
    async fn non_object_json_text_frame_is_skipped_without_disconnect() {
        let (listener, url) = loopback().await;
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(kline(60)).await.unwrap();
            ws.send(Message::Text("123".into())).await.unwrap();
            ws.send(Message::Text("null".into())).await.unwrap();
            ws.send(Message::Text("[1,2,3]".into())).await.unwrap();
            ws.send(kline(120)).await.unwrap();
            while ws.next().await.is_some_and(|m| m.is_ok()) {}
        });
        let mut rx = test_feed("BINANCE:BTCUSDT", url, Duration::from_secs(2));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == 60));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == 120));
        // No status messages: the connection never dropped.
        assert!(tokio::time::timeout(Duration::from_millis(50), rx.recv()).await.is_err());
        drop(rx);
        let _ = tokio::time::timeout(Duration::from_secs(1), server).await;
    }

    /// Probe 1: a Binance kline for another symbol is ignored without
    /// disconnecting or emitting status.
    #[tokio::test]
    async fn binance_wrong_symbol_message_is_skipped_quietly() {
        let (listener, url) = loopback().await;
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(kline_for("ETHUSDT", 60)).await.unwrap();
            ws.send(kline_for("ETHUSDT", 120)).await.unwrap();
            ws.send(kline(180)).await.unwrap();
            while ws.next().await.is_some_and(|m| m.is_ok()) {}
        });
        let mut rx = test_feed("BINANCE:BTCUSDT", url, Duration::from_secs(2));
        // Only the BTCUSDT bar is delivered; the wrong-symbol frames are silent.
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == 180));
        assert!(tokio::time::timeout(Duration::from_millis(50), rx.recv()).await.is_err());
        drop(rx);
        let _ = tokio::time::timeout(Duration::from_secs(1), server).await;
    }

    /// Probe 2: Coinbase 'subscriptions', 'error', and ticker messages with bad
    /// price/size must be skipped without killing the connection. Heartbeats
    /// for the right product keep the stream healthy.
    #[tokio::test]
    async fn coinbase_control_and_bad_ticker_messages_do_not_disconnect() {
        let (listener, url) = loopback().await;
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            // read subscribe
            let _ = ws.next().await;
            // Subscriptions ack: no product_id, must be ignored.
            ws.send(Message::Text(
                r#"{"type":"subscriptions","channels":[{"name":"ticker","product_ids":["BTC-USD"]}]}"#.into(),
            ))
            .await
            .unwrap();
            // Error message: must be ignored (no product_id).
            ws.send(Message::Text(r#"{"type":"error","message":"auth required"}"#.into())).await.unwrap();
            // Ticker with product_id but bad price: must be ignored.
            ws.send(Message::Text(
                r#"{"type":"ticker","product_id":"BTC-USD","price":"not-a-number","last_size":"1","time":"2024-01-01T00:00:01.000000Z"}"#.into(),
            ))
            .await
            .unwrap();
            // Ticker with zero price: must be ignored.
            ws.send(Message::Text(
                r#"{"type":"ticker","product_id":"BTC-USD","price":"0","last_size":"1","time":"2024-01-01T00:00:02.000000Z"}"#.into(),
            ))
            .await
            .unwrap();
            // Ticker with negative size: must be ignored.
            ws.send(Message::Text(
                r#"{"type":"ticker","product_id":"BTC-USD","price":"100","last_size":"-5","time":"2024-01-01T00:00:03.000000Z"}"#.into(),
            ))
            .await
            .unwrap();
            // Ticker for a different product: must be ignored.
            ws.send(Message::Text(
                r#"{"type":"ticker","product_id":"ETH-USD","price":"100","last_size":"1","time":"2024-01-01T00:00:04.000000Z"}"#.into(),
            ))
            .await
            .unwrap();
            // Heartbeat for the right product keeps us healthy.
            ws.send(Message::Text(r#"{"type":"heartbeat","product_id":"BTC-USD"}"#.into())).await.unwrap();
            // A valid ticker should still flow.
            ws.send(Message::Text(
                r#"{"type":"ticker","product_id":"BTC-USD","price":"100.5","last_size":"1","time":"2024-01-01T00:00:05.000000Z"}"#.into(),
            ))
            .await
            .unwrap();
            while ws.next().await.is_some_and(|m| m.is_ok()) {}
        });
        let mut rx = test_feed("COINBASE:BTC-USD", url, Duration::from_secs(2));
        // Only the valid ticker produces a Partial; nothing disconnects.
        match update(&mut rx).await {
            crate::LiveUpdate::Partial(b) => assert!(b.close > 100.0),
            other => panic!("expected Partial, got {other:?}"),
        }
        assert!(tokio::time::timeout(Duration::from_millis(50), rx.recv()).await.is_err());
        drop(rx);
        let _ = tokio::time::timeout(Duration::from_secs(1), server).await;
    }

    /// Probe 3: when the first message after reconnect is for a different
    /// product, Reconnected{since} must still be the bucket of the last
    /// delivered bar (before the gap), not the wrong-product message.
    #[tokio::test]
    async fn recovery_since_uses_last_delivered_bar_not_wrong_product_first_message() {
        let (listener, url) = loopback().await;
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(kline(60)).await.unwrap();
            ws.send(kline(120)).await.unwrap();
            ws.close(None).await.unwrap();
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            // First message on the new connection is for another product.
            ws.send(kline_for("ETHUSDT", 150)).await.unwrap();
            ws.send(kline_for("ETHUSDT", 160)).await.unwrap();
            // Then a valid BTCUSDT bar triggers recovery.
            ws.send(kline(180)).await.unwrap();
            while ws.next().await.is_some_and(|m| m.is_ok()) {}
        });
        let mut rx = test_feed("BINANCE:BTCUSDT", url, Duration::from_secs(2));
        for ts in [60, 120] {
            assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == ts));
        }
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Status(s)
            if s == "Binance live stream disconnected; reconnecting…"));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Status(s)
            if s == "Binance live stream restored; filling missed bars"));
        // since must be 120 (last delivered bar), not None or a wrong-product ts.
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Reconnected { since: Some(120) }),
            "since must be the bucket of the last delivered bar");
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == 180));
        drop(rx);
        let _ = tokio::time::timeout(Duration::from_secs(1), server).await;
    }

    /// Probe 3b: if the first message after reconnect is a Coinbase heartbeat
    /// for the right product, Reconnected fires with the pre-gap since (the
    /// heartbeat is a valid liveness signal but does not advance `last`).
    #[tokio::test]
    async fn coinbase_heartbeat_first_after_reconnect_reports_pre_gap_since() {
        let (listener, url) = loopback().await;
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let _ = ws.next().await; // subscribe
            ws.send(Message::Text(
                r#"{"type":"ticker","product_id":"BTC-USD","price":"100","last_size":"1","time":"2024-01-01T00:10:00.000000Z"}"#.into(),
            ))
            .await
            .unwrap();
            ws.close(None).await.unwrap();
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let _ = ws.next().await; // subscribe
            // Heartbeat first: triggers Reconnected with the pre-gap since.
            ws.send(Message::Text(r#"{"type":"heartbeat","product_id":"BTC-USD"}"#.into())).await.unwrap();
            ws.send(Message::Text(
                r#"{"type":"ticker","product_id":"BTC-USD","price":"101","last_size":"1","time":"2024-01-01T00:11:00.000000Z"}"#.into(),
            ))
            .await
            .unwrap();
            while ws.next().await.is_some_and(|m| m.is_ok()) {}
        });
        let mut rx = test_feed("COINBASE:BTC-USD", url, Duration::from_secs(2));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(_)));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Status(s) if s.contains("disconnected")));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Status(s) if s.contains("restored")));
        // Heartbeat-triggered Reconnected carries the pre-gap bar bucket.
        match update(&mut rx).await {
            crate::LiveUpdate::Reconnected { since } => assert!(since.is_some(), "heartbeat recovery must report since"),
            other => panic!("expected Reconnected, got {other:?}"),
        }
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(_)));
        drop(rx);
        let _ = tokio::time::timeout(Duration::from_secs(1), server).await;
    }

    /// Probe 6: sparse messages that arrive within the idle timeout keep the
    /// connection alive; the timeout is per-read, not a global deadline.
    #[tokio::test]
    async fn sparse_messages_within_idle_timeout_keep_connection_alive() {
        let (listener, url) = loopback().await;
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            for i in 0..5 {
                ws.send(kline(60 * (i + 1))).await.unwrap();
                tokio::time::sleep(Duration::from_millis(80)).await;
            }
            while ws.next().await.is_some_and(|m| m.is_ok()) {}
        });
        // Idle 250ms; messages arrive every ~80ms so each read resets the timer.
        let mut rx = test_feed("BINANCE:BTCUSDT", url, Duration::from_millis(250));
        for i in 0..5 {
            assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == 60 * (i + 1)));
        }
        assert!(tokio::time::timeout(Duration::from_millis(50), rx.recv()).await.is_err());
        drop(rx);
        let _ = tokio::time::timeout(Duration::from_secs(1), server).await;
    }

    /// Probe 6b: if messages stop for longer than idle, the connection is
    /// treated as dead and the loop retries (with a status message).
    #[tokio::test]
    async fn silence_beyond_idle_timeout_triggers_reconnect_status() {
        let (listener, url) = loopback().await;
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(kline(60)).await.unwrap();
            // Go silent; client should time out and retry.
            while ws.next().await.is_some_and(|m| m.is_ok()) {}
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(kline(120)).await.unwrap();
            while ws.next().await.is_some_and(|m| m.is_ok()) {}
        });
        let mut rx = test_feed("BINANCE:BTCUSDT", url, Duration::from_millis(150));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == 60));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Status(s)
            if s == "Binance live stream disconnected; reconnecting…"));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Status(s) if s.contains("restored")));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Reconnected { .. }));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == 120));
        drop(rx);
        let _ = tokio::time::timeout(Duration::from_secs(1), server).await;
    }

    /// Probe 7: every Status string emitted across a full failure/recovery
    /// cycle is a fixed venue template — never a URL, raw payload, or secret.
    #[tokio::test]
    async fn status_messages_never_contain_urls_or_server_payloads() {
        let (listener, url) = loopback().await;
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(kline(60)).await.unwrap();
            ws.close(None).await.unwrap();
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(kline(120)).await.unwrap();
            while ws.next().await.is_some_and(|m| m.is_ok()) {}
        });
        let mut rx = test_feed("BINANCE:BTCUSDT", url, Duration::from_secs(2));
        let mut statuses = Vec::new();
        for _ in 0..2 {
            if let crate::LiveUpdate::Status(s) = update(&mut rx).await {
                statuses.push(s);
            }
        }
        // rest of the cycle
        for _ in 0..3 {
            match update(&mut rx).await {
                crate::LiveUpdate::Status(s) => statuses.push(s),
                _ => {}
            }
        }
        assert!(!statuses.is_empty());
        for s in &statuses {
            assert!(!s.contains("://"), "status leaked a URL: {s}");
            assert!(!s.contains("ws://") && !s.contains("wss://") && !s.contains("http"), "status leaked a URL: {s}");
            assert!(!s.contains('{') && !s.contains('}'), "status leaked a JSON payload: {s}");
            assert!(!s.contains("127.0.0.1") && !s.contains("localhost"), "status leaked a host: {s}");
            // Only the three fixed templates (plus worker templates elsewhere).
            assert!(
                s.starts_with("Binance live stream "),
                "unexpected status template: {s}"
            );
        }
        drop(rx);
        let _ = tokio::time::timeout(Duration::from_secs(1), server).await;
    }

    /// Validated behaviour: a Binance bar with invalid OHLCV is skipped without
    /// disconnecting (parse_binance_ws returns None → continue).
    #[tokio::test]
    async fn binance_invalid_bar_values_are_skipped_without_disconnect() {
        let (listener, url) = loopback().await;
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            ws.send(kline(60)).await.unwrap();
            // high < low: invalid bar, must be skipped.
            ws.send(Message::Text(
                serde_json::json!({
                    "k": {"s": "BTCUSDT", "t": 120_000, "o": "5", "h": "1",
                          "l": "4", "c": "5", "v": "1"}
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
            // NaN price: invalid, must be skipped.
            ws.send(Message::Text(
                serde_json::json!({
                    "k": {"s": "BTCUSDT", "t": 180_000, "o": "NaN", "h": "3",
                          "l": "1", "c": "2", "v": "1"}
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
            ws.send(kline(240)).await.unwrap();
            while ws.next().await.is_some_and(|m| m.is_ok()) {}
        });
        let mut rx = test_feed("BINANCE:BTCUSDT", url, Duration::from_secs(2));
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == 60));
        // Invalid bars are silently skipped; the connection survives.
        assert!(matches!(update(&mut rx).await, crate::LiveUpdate::Partial(b) if b.ts == 240));
        assert!(tokio::time::timeout(Duration::from_millis(50), rx.recv()).await.is_err());
        drop(rx);
        let _ = tokio::time::timeout(Duration::from_secs(1), server).await;
    }
}
