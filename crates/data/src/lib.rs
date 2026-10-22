//! Market data behind a pluggable [`MarketData`] trait, addressed by Bloomberg-style
//! market keys (`AAPL US Equity`, `BTCUSD Curncy`, `US10Y Govt`, `CL1 Comdty`, `SPX Index`).
//!
//! Provider selection ([`from_env`]):
//! - `TR_PROVIDER=sample` forces deterministic sample data (fully offline, with simulated
//!   live ticks).
//! - Otherwise [`markets::Markets`] routes each asset class to a free source: Binance public
//!   REST/websocket for explicit Binance or Coinbase crypto products, Treasury.gov (FRED fallback) for US
//!   Treasury yields, Yahoo Finance for FX, indices, futures, ETFs and stocks, or Alpaca for
//!   US stocks/ETFs when `APCA_API_KEY_ID` + `APCA_API_SECRET_KEY` are set.
//!
//! The router is wrapped in [`Fallback`] for auxiliary demo views. Chart and scan
//! history surface provider failures; simulated chart history requires sample mode.

pub mod alpaca;
pub mod cache;
pub mod crypto;
pub mod crypto_catalog;
pub mod markets;
pub mod questrade;
pub mod rss;
pub mod sample;
pub mod scanstore;
pub mod treasury;
pub mod universe;
pub mod yahoo;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tr_core::{AssetClass, Bar, Security, Timeframe, catalog};

pub use cache::BarCache;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PriceBasis {
    #[default]
    Provider,
    Raw,
    SplitAdjusted,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryOptions {
    pub session: tr_core::session::Session,
    pub extended_hours: bool,
    pub price_basis: PriceBasis,
}
impl HistoryOptions {
    /// Equities default to the regular-session multi-hour grid. Only an explicit
    /// extended session opts out; scans and alerts always select regular hours.
    pub fn for_chart(mut self, class: AssetClass, tf: Timeframe) -> Self {
        if matches!(class, AssetClass::Equity | AssetClass::Etf)
            && matches!(tf, Timeframe::H2 | Timeframe::H4)
            && self.session == tr_core::session::Session::Provider
        {
            self.session = tr_core::session::Session::UsRegular;
        }
        self
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EventKind {
    Earnings,
    Dividend,
    Split,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartEvent {
    pub symbol: String,
    pub ts: i64,
    pub kind: EventKind,
    pub detail: String,
    pub source: String,
}

/// Latest quote for a symbol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    pub symbol: String,
    pub last: f64,
    pub prev_close: f64,
    pub ts: i64,
    /// Recent daily closes (oldest first) for sparklines; may be empty.
    #[serde(default)]
    pub spark: Vec<f64>,
    /// Actual fixed crypto product; absent for non-crypto and explicitly simulated quotes.
    #[serde(default)]
    pub crypto_source: Option<tr_core::crypto::CryptoInstrument>,
}

/// A symbol-search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub key: String,
    pub name: String,
    pub class: AssetClass,
    pub exchange: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub ticker: String,
    pub tenor_years: f64,
    pub yield_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct YieldCurve {
    /// Observation date (Unix seconds, midnight UTC).
    pub ts: i64,
    pub points: Vec<CurvePoint>,
}

impl YieldCurve {
    pub fn get(&self, ticker: &str) -> Option<f64> {
        self.points.iter().find(|p| p.ticker == ticker).map(|p| p.yield_pct)
    }
}

/// The US Treasury curve now, about a month ago and about a year ago.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct YieldCurves {
    pub current: YieldCurve,
    pub month_ago: Option<YieldCurve>,
    pub year_ago: Option<YieldCurve>,
}

/// A news headline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewsItem {
    pub title: String,
    pub publisher: String,
    pub link: String,
    /// Publication time (Unix seconds).
    pub ts: i64,
    /// Related tickers as reported by the source.
    #[serde(default)]
    pub tickers: Vec<String>,
}

/// One side's price level: (price, size).
pub type Level = (f64, f64);

/// Top of the order book: bids descending, asks ascending.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderBook {
    pub symbol: String,
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
    /// Venue the book came from (e.g. "Binance").
    pub source: String,
    /// True when the book is synthetic (sample mode or no public book for this market).
    pub simulated: bool,
}

impl OrderBook {
    pub fn mid(&self) -> Option<f64> {
        Some((self.bids.first()?.0 + self.asks.first()?.0) / 2.0)
    }

    pub fn spread(&self) -> Option<f64> {
        Some(self.asks.first()?.0 - self.bids.first()?.0)
    }

    /// Bucket levels to multiples of `tick` (bids round down, asks up), summing sizes.
    pub fn grouped(&self, tick: f64) -> OrderBook {
        if tick <= 0.0 {
            return self.clone();
        }
        let group = |levels: &[Level], up: bool| {
            let mut out: Vec<Level> = Vec::new();
            for &(p, q) in levels {
                let b = if up { (p / tick).ceil() } else { (p / tick).floor() } * tick;
                match out.last_mut() {
                    Some(last) if (last.0 - b).abs() < tick * 1e-6 => last.1 += q,
                    _ => out.push((b, q)),
                }
            }
            out
        };
        OrderBook { bids: group(&self.bids, false), asks: group(&self.asks, true), ..self.clone() }
    }
}

/// One listed option.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionContract {
    pub contract: String,
    pub strike: f64,
    pub last: Option<f64>,
    pub change: Option<f64>,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub volume: Option<u64>,
    pub open_interest: Option<u64>,
    /// Annualised implied volatility (0.25 = 25%).
    pub iv: Option<f64>,
    pub in_the_money: bool,
}

impl OptionContract {
    pub fn mid(&self) -> Option<f64> {
        match (self.bid, self.ask) {
            (Some(b), Some(a)) if b > 0.0 && a > 0.0 => Some((a + b) / 2.0),
            _ => self.last,
        }
    }
}

/// Calls and puts for one expiration, plus the other listed expirations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionChain {
    pub underlying: String,
    pub spot: f64,
    /// Continuous dividend yield used for greeks.
    pub dividend_yield: f64,
    /// All listed expirations (Unix seconds, ascending).
    pub expirations: Vec<i64>,
    /// The expiration shown.
    pub expiration: i64,
    pub calls: Vec<OptionContract>,
    pub puts: Vec<OptionContract>,
    /// Quote time (Unix seconds).
    pub ts: i64,
    pub source: String,
    /// Synthetic chain (offline sample mode or no source).
    pub simulated: bool,
}

/// Live bar updates for one symbol and timeframe: each message is the latest state of
/// the current (possibly still forming) bar. The feed stops when the receiver is dropped.
#[derive(Debug, Clone)]
pub enum LiveUpdate {
    /// A previously authenticated connection recovered after a failure.
    Reconnected {
        since: Option<i64>,
    },
    Partial(Bar),
    Snapshot(Bar),
    Status(String),
}
pub type LiveFeed = mpsc::Receiver<LiveUpdate>;
pub mod alpaca_stream;

/// History fetched for a scanner, with explicit synthetic-data provenance.
/// Unlike chart history, this path must never silently manufacture fallback prices.
#[derive(Debug, Clone)]
pub struct ScanHistory {
    pub bars: Vec<Bar>,
    pub simulated: bool,
}

/// Pick curve snapshots from per-date rows: latest, nearest at or before -30d and -365d.
pub fn pick_curves(mut rows: Vec<YieldCurve>) -> Option<YieldCurves> {
    rows.retain(|r| !r.points.is_empty());
    rows.sort_by_key(|r| r.ts);
    let current = rows.last()?.clone();
    let at = |days: i64| rows.iter().rev().find(|r| r.ts <= current.ts - days * 86_400).cloned();
    Some(YieldCurves { month_ago: at(30), year_ago: at(365), current })
}

impl Quote {
    pub fn change(&self) -> f64 {
        self.last - self.prev_close
    }

    pub fn change_pct(&self) -> f64 {
        if self.prev_close == 0.0 { 0.0 } else { self.change() / self.prev_close * 100.0 }
    }
}

/// A source of historical bars and quotes.
#[async_trait]
pub trait MarketData: Send + Sync {
    /// Short provider name shown in the status bar.
    fn name(&self) -> &str;

    /// Identity of the active scanner data route, without credentials. Wrappers and
    /// providers whose routing can change at runtime must forward/override this key.
    fn scan_cache_key(&self, _symbol: &str) -> String {
        self.name().to_string()
    }

    /// Exact crypto contract used by this source, never an inferred cross-venue alias.
    /// Sample/non-crypto sources return `None`; timestamps remain on bars and quotes.
    fn crypto_instrument(&self, _symbol: &str) -> Option<tr_core::crypto::CryptoInstrument> {
        None
    }

    /// Active exchange spot products. Unsupported/sample providers fail explicitly.
    async fn crypto_catalog(&self, _venue: tr_core::crypto::CryptoVenue) -> Result<crypto_catalog::Catalog> {
        anyhow::bail!("Crypto discovery requires a live exchange data provider")
    }
    async fn crypto_stats(&self, _symbol: &str) -> Result<crypto_catalog::Rolling24h> {
        anyhow::bail!("Rolling 24-hour crypto statistics are unavailable from this provider")
    }

    /// Up to `limit` bars at `tf` ending at or before `end` (Unix seconds; `None` = now),
    /// ascending by time. Paging older history means calling again with `end` set to the
    /// oldest bar already held.
    async fn history(&self, symbol: &str, tf: Timeframe, end: Option<i64>, limit: usize) -> Result<Vec<Bar>>;
    async fn chart_history(
        &self,
        symbol: &str,
        tf: Timeframe,
        end: Option<i64>,
        limit: usize,
        options: HistoryOptions,
    ) -> Result<Vec<Bar>> {
        if options != HistoryOptions::default() {
            anyhow::bail!("This provider does not support the requested extended-hours/price-basis options");
        }
        self.history(symbol, tf, end, limit).await
    }
    async fn chart_events(&self, _symbol: &str, _start: i64, _end: i64) -> Result<Vec<ChartEvent>> {
        anyhow::bail!("Corporate events are unavailable from this provider")
    }
    /// Condition history uses the same stable source as alert observations.
    async fn alert_history(&self, symbol: &str, tf: Timeframe, limit: usize) -> Result<ScanHistory> {
        self.scan_history(symbol, tf, limit).await
    }

    /// Scanner history bypasses chart caches. Providers that synthesize data or wrap
    /// other sources must override this method to preserve provenance and failures.
    async fn scan_history(&self, symbol: &str, tf: Timeframe, limit: usize) -> Result<ScanHistory> {
        Ok(ScanHistory { bars: self.history(symbol, tf, None, limit).await?, simulated: false })
    }

    /// Latest quotes for `symbols`. The default derives them from daily history.
    async fn quotes(&self, symbols: &[String]) -> Result<Vec<Quote>> {
        let mut out = Vec::with_capacity(symbols.len());
        for s in symbols {
            let bars = self.history(s, Timeframe::D1, None, 2).await?;
            if let Some(last) = bars.last() {
                let prev = if bars.len() > 1 { bars[bars.len() - 2].close } else { last.open };
                out.push(Quote {
                    symbol: s.clone(),
                    last: last.close,
                    prev_close: prev,
                    ts: last.ts,
                    spark: vec![],
                    crypto_source: None,
                });
            }
        }
        Ok(out)
    }

    /// Alert observations must preserve provenance and never substitute demo data.
    /// Use the current minute's close, not the daily sparkline's bar-open timestamp.
    async fn alert_observation(&self, symbol: &str) -> Result<tr_core::price_alerts::Observation> {
        let history = self.scan_history(symbol, Timeframe::M1, 2).await?;
        let bar = history.bars.last().ok_or_else(|| anyhow::anyhow!("No price observations available"))?;
        Ok(tr_core::price_alerts::Observation {
            symbol: symbol.into(),
            price: bar.close,
            ts: bar.ts,
            simulated: history.simulated,
            source: self.scan_cache_key(symbol),
        })
    }

    /// Stream live updates of the current bar, if the source supports it.
    fn live(&self, _symbol: &str, _tf: Timeframe) -> Option<LiveFeed> {
        None
    }

    /// Search securities beyond the built-in catalog (network sources).
    async fn search(&self, _query: &str) -> Result<Vec<SearchHit>> {
        Ok(Vec::new())
    }

    /// Recent headlines for a market key, or general market news for `None`.
    async fn news(&self, _symbol: Option<&str>) -> Result<Vec<NewsItem>> {
        Ok(Vec::new())
    }

    /// Option chain for one expiration (`None` = nearest).
    async fn options(&self, symbol: &str, _expiration: Option<i64>) -> Result<OptionChain> {
        anyhow::bail!("no options data for {symbol}")
    }

    /// Top `depth` levels of the order book, where a public book exists.
    async fn order_book(&self, symbol: &str, _depth: usize) -> Result<OrderBook> {
        anyhow::bail!("no public order book for {symbol}")
    }

    /// The US Treasury curve. The default assembles it from each tenor's daily history.
    async fn yield_curves(&self) -> Result<YieldCurves> {
        let tenors = catalog::treasury_curve();
        let mut by_tenor = Vec::new();
        for l in &tenors {
            by_tenor.push((l, self.history(&l.key(), Timeframe::D1, None, 400).await?));
        }
        let mut dates: Vec<i64> = by_tenor.iter().flat_map(|(_, b)| b.iter().map(|x| x.ts)).collect();
        dates.sort_unstable();
        dates.dedup();
        let rows = dates
            .into_iter()
            .map(|ts| YieldCurve {
                ts,
                points: by_tenor
                    .iter()
                    .filter_map(|(l, bars)| {
                        let b = bars.iter().rev().find(|b| b.ts <= ts)?;
                        Some(CurvePoint { ticker: l.ticker.into(), tenor_years: l.tenor_years?, yield_pct: b.close })
                    })
                    .collect(),
            })
            .collect();
        pick_curves(rows).ok_or_else(|| anyhow::anyhow!("no Treasury curve data"))
    }
}

pub type Provider = Arc<dyn MarketData>;

/// Serves from `primary`, falling back to sample data on error.
pub struct Fallback<P> {
    primary: P,
    sample: sample::SampleProvider,
    name: String,
}

impl<P: MarketData> Fallback<P> {
    pub fn new(primary: P) -> Self {
        let name = primary.name().to_string();
        Self { primary, sample: sample::SampleProvider::default(), name }
    }
}

#[async_trait]
impl<P: MarketData> MarketData for Fallback<P> {
    async fn chart_history(
        &self,
        symbol: &str,
        tf: Timeframe,
        end: Option<i64>,
        limit: usize,
        options: HistoryOptions,
    ) -> Result<Vec<Bar>> {
        self.primary.chart_history(symbol, tf, end, limit, options).await
    }
    async fn chart_events(&self, symbol: &str, start: i64, end: i64) -> Result<Vec<ChartEvent>> {
        self.primary.chart_events(symbol, start, end).await
    }
    async fn alert_history(&self, symbol: &str, tf: Timeframe, limit: usize) -> Result<ScanHistory> {
        self.primary.alert_history(symbol, tf, limit).await
    }
    async fn alert_observation(&self, symbol: &str) -> Result<tr_core::price_alerts::Observation> {
        self.primary.alert_observation(symbol).await
    }
    fn scan_cache_key(&self, symbol: &str) -> String {
        self.primary.scan_cache_key(symbol)
    }

    fn crypto_instrument(&self, symbol: &str) -> Option<tr_core::crypto::CryptoInstrument> {
        self.primary.crypto_instrument(symbol)
    }
    async fn crypto_catalog(&self, venue: tr_core::crypto::CryptoVenue) -> Result<crypto_catalog::Catalog> {
        self.primary.crypto_catalog(venue).await
    }
    async fn crypto_stats(&self, symbol: &str) -> Result<crypto_catalog::Rolling24h> {
        self.primary.crypto_stats(symbol).await
    }
    fn name(&self) -> &str {
        &self.name
    }

    async fn scan_history(&self, symbol: &str, tf: Timeframe, limit: usize) -> Result<ScanHistory> {
        // An unavailable market is a visible scanner error, never a synthetic match.
        self.primary.scan_history(symbol, tf, limit).await
    }

    async fn history(&self, symbol: &str, tf: Timeframe, end: Option<i64>, limit: usize) -> Result<Vec<Bar>> {
        self.primary.history(symbol, tf, end, limit).await
    }

    async fn quotes(&self, symbols: &[String]) -> Result<Vec<Quote>> {
        match self.primary.quotes(symbols).await {
            // If the primary produced nothing at all (offline), serve sample quotes rather
            // than an empty panel; partial results are never mixed with synthetic prices.
            Ok(mut q) => {
                let missing: Vec<String> = symbols
                    .iter()
                    .filter(|s| !crypto_request(s) && !q.iter().any(|x| &x.symbol == *s))
                    .cloned()
                    .collect();
                if !missing.is_empty() && q.is_empty() {
                    q.extend(self.sample.quotes(&missing).await.unwrap_or_default());
                }
                Ok(q)
            }
            Err(e) => {
                tracing::warn!("{} quotes failed, using sample data: {e:#}", self.name);
                let eligible: Vec<_> = symbols.iter().filter(|s| !crypto_request(s)).cloned().collect();
                if eligible.is_empty() {
                    return Err(e);
                }
                self.sample.quotes(&eligible).await
            }
        }
    }

    fn live(&self, symbol: &str, tf: Timeframe) -> Option<LiveFeed> {
        self.primary.live(symbol, tf)
    }

    async fn search(&self, query: &str) -> Result<Vec<SearchHit>> {
        self.primary.search(query).await
    }

    async fn news(&self, symbol: Option<&str>) -> Result<Vec<NewsItem>> {
        self.primary.news(symbol).await
    }

    async fn options(&self, symbol: &str, expiration: Option<i64>) -> Result<OptionChain> {
        match self.primary.options(symbol, expiration).await {
            Ok(c) => Ok(c),
            Err(e) if crypto_request(symbol) => Err(e),
            Err(e) => {
                tracing::warn!("{} options {symbol}: {e:#}; simulating", self.name);
                self.sample.options(symbol, expiration).await
            }
        }
    }

    /// Real books where the venue publishes one; otherwise a clearly flagged simulation.
    async fn order_book(&self, symbol: &str, depth: usize) -> Result<OrderBook> {
        match self.primary.order_book(symbol, depth).await {
            Ok(b) => Ok(b),
            Err(e) if crypto_request(symbol) => Err(e),
            Err(e) => {
                tracing::debug!("{} order book {symbol}: {e:#}; simulating", self.name);
                self.sample.order_book(symbol, depth).await
            }
        }
    }

    async fn yield_curves(&self) -> Result<YieldCurves> {
        match self.primary.yield_curves().await {
            Ok(c) => Ok(c),
            Err(e) => {
                tracing::warn!("{} yield curve failed, using sample data: {e:#}", self.name);
                self.sample.yield_curves().await
            }
        }
    }
}

/// A malformed explicit venue key must not be disguised as a simulated equity.
fn crypto_request(symbol: &str) -> bool {
    symbol.contains(':') || Security::parse(symbol).is_some_and(|s| s.class == AssetClass::Crypto)
}

/// Resolve a market key or bare ticker.
pub fn security(symbol: &str) -> Security {
    Security::parse(symbol).unwrap_or_else(|| Security::equity(symbol))
}

/// Build the provider described in the crate docs from environment variables.
pub fn from_env() -> Provider {
    from_env_with(None)
}

/// [`from_env`] with an optional Questrade source, active whenever it holds a token.
pub fn from_env_with(questrade: Option<questrade::Questrade>) -> Provider {
    // reqwest and the websocket client both use rustls; pin one crypto provider.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    if var("TR_PROVIDER").is_some_and(|p| p.eq_ignore_ascii_case("sample")) {
        return Arc::new(sample::SampleProvider::default().with_live_ticks());
    }
    let alpaca = match (var("APCA_API_KEY_ID"), var("APCA_API_SECRET_KEY")) {
        (Some(key), Some(secret)) => {
            let feed = var("APCA_DATA_FEED").unwrap_or_else(|| "iex".into());
            alpaca::AlpacaProvider::new(key, secret, feed).map_err(|e| tracing::warn!("alpaca init failed: {e:#}")).ok()
        }
        _ => None,
    };
    match markets::Markets::with_questrade(alpaca, questrade) {
        Ok(m) => Arc::new(Fallback::new(m)),
        Err(_) => Arc::new(sample::SampleProvider::default().with_live_ticks()),
    }
}

/// Keep at most `limit` bars ending at or before `end`.
pub(crate) fn clip(mut bars: Vec<Bar>, end: Option<i64>, limit: usize) -> Vec<Bar> {
    if let Some(end) = end {
        bars.retain(|b| b.ts < end);
    }
    if bars.len() > limit {
        bars.drain(..bars.len() - limit);
    }
    bars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping_and_simulated_books() {
        let b = OrderBook {
            symbol: "X".into(),
            bids: vec![(100.4, 1.0), (100.1, 2.0), (99.7, 1.0)],
            asks: vec![(100.6, 1.0), (100.9, 3.0), (101.2, 1.0)],
            source: "t".into(),
            simulated: false,
        };
        let g = b.grouped(0.5);
        assert_eq!(g.bids, vec![(100.0, 3.0), (99.5, 1.0)]);
        assert_eq!(g.asks, vec![(101.0, 4.0), (101.5, 1.0)]);
        assert!((b.mid().unwrap() - 100.5).abs() < 1e-9);
        let s = sample::simulated_book("BTCUSD Curncy", 65_000.0, 20, 7);
        assert!(s.simulated && s.bids.len() > 10 && s.asks.len() > 10);
        assert!(s.bids[0].0 < 65_000.0 && s.asks[0].0 > 65_000.0);
        assert!(s.bids.windows(2).all(|w| w[0].0 > w[1].0));
        assert_eq!(s, sample::simulated_book("BTCUSD Curncy", 65_000.0, 20, 7));
        // Simulated option chains: sorted strikes around spot, sane prices, parity-ish.
        let now = 1_791_800_000;
        let c = sample::simulated_chain("AAPL US Equity", 230.0, None, now);
        assert!(c.simulated && c.expirations.len() >= 10 && c.expiration > now);
        assert!(c.expirations.windows(2).all(|w| w[0] < w[1]));
        assert!(c.calls.windows(2).all(|w| w[0].strike < w[1].strike));
        assert!(c.calls.iter().any(|k| k.strike < 230.0) && c.calls.iter().any(|k| k.strike > 230.0));
        let deep = c.calls.first().unwrap();
        assert!(deep.in_the_money && deep.mid().unwrap() > 230.0 - deep.strike - 1.0);
        let later = sample::simulated_chain("AAPL US Equity", 230.0, Some(c.expirations[5]), now);
        assert_eq!(later.expiration, c.expirations[5]);
    }

    struct Broken;

    #[async_trait]
    impl MarketData for Broken {
        fn name(&self) -> &str {
            "broken"
        }
        async fn history(&self, _: &str, _: Timeframe, _: Option<i64>, _: usize) -> Result<Vec<Bar>> {
            anyhow::bail!("offline")
        }
    }

    #[tokio::test]
    async fn fallback_serves_sample_data() {
        let p = Fallback::new(Broken);
        assert!(
            p.chart_history(
                "AAPL",
                Timeframe::D1,
                None,
                100,
                HistoryOptions { extended_hours: true, ..Default::default() }
            )
            .await
            .is_err()
        );
        assert!(p.chart_events("AAPL", 1, 1000).await.is_err());
        assert!(p.alert_history("AAPL", Timeframe::D1, 100).await.is_err());
        assert!(p.history("AAPL", Timeframe::D1, None, 100).await.unwrap_err().to_string().contains("offline"));
        let q = p.quotes(&["AAPL".into()]).await.unwrap();
        assert_eq!(q.len(), 1);
        // Paging must surface the error instead of mixing in synthetic bars.
        assert!(p.history("AAPL", Timeframe::D1, Some(100), 100).await.is_err());
        assert_eq!(p.name(), "broken");
    }

    #[tokio::test]
    async fn scanner_never_silently_uses_chart_fallback() {
        let p = Fallback::new(Broken);
        assert!(p.history("AAPL", Timeframe::D1, None, 10).await.is_err());
        assert!(p.scan_history("AAPL", Timeframe::D1, 10).await.unwrap_err().to_string().contains("offline"));
        assert!(p.alert_observation("AAPL US Equity").await.unwrap_err().to_string().contains("offline"));
    }

    #[tokio::test]
    async fn explicit_sample_scanner_history_keeps_provenance_through_wrappers() {
        let p = Fallback::new(sample::SampleProvider::at(1_790_000_000, 100));
        let history = p.scan_history("AAPL", Timeframe::D1, 10).await.unwrap();
        assert!(history.simulated);
        assert_eq!(history.bars.len(), 10);
        let q = p.alert_observation("AAPL US Equity").await.unwrap();
        assert!(q.simulated);
        assert_eq!(q.source, "sample");
        assert_eq!(q.symbol, "AAPL US Equity");
    }

    #[tokio::test]
    async fn equity_multi_hour_scan_and_alert_bars_match_the_default_chart_grid() {
        let end = chrono::DateTime::parse_from_rfc3339("2026-03-09T20:00:00Z").unwrap().timestamp();
        let provider = sample::SampleProvider::at(end, 400);
        for tf in [Timeframe::H2, Timeframe::H4] {
            let chart =
                provider.chart_history("AAPL US Equity", tf, None, 30, HistoryOptions::default()).await.unwrap();
            let scan = provider.scan_history("AAPL US Equity", tf, 30).await.unwrap();
            let alert = provider.alert_history("AAPL US Equity", tf, 30).await.unwrap();
            assert!(!chart.is_empty());
            let chart_ts: Vec<i64> = chart.iter().map(|b| b.ts).collect();
            let scan_ts: Vec<i64> = scan.bars.iter().map(|b| b.ts).collect();
            let alert_ts: Vec<i64> = alert.bars.iter().map(|b| b.ts).collect();
            assert_eq!(chart_ts, scan_ts, "{tf}: chart grid != scan grid");
            assert_eq!(chart_ts, alert_ts, "{tf}: chart grid != alert grid");
            assert_eq!(chart, scan.bars);
            assert_eq!(chart, alert.bars);
            for bar in &chart {
                assert_eq!(tr_core::session::bar_interval(tf, tr_core::session::Session::UsRegular, bar.ts).0, bar.ts);
            }
            let extended = provider
                .chart_history(
                    "AAPL US Equity",
                    tf,
                    None,
                    30,
                    HistoryOptions { session: tr_core::session::Session::UsExtended, ..Default::default() },
                )
                .await
                .unwrap();
            assert!(!extended.is_empty());
            assert!(extended.iter().all(|bar| tf.bucket_start(bar.ts) == bar.ts));
            assert_ne!(chart_ts, extended.iter().map(|bar| bar.ts).collect::<Vec<_>>());
        }
    }

    #[tokio::test]
    async fn default_yield_curve_from_history() {
        let c = sample::SampleProvider::at(1_790_000_000, 1_000).yield_curves().await.unwrap();
        assert_eq!(c.current.points.len(), 11);
        assert!(c.month_ago.is_some() && c.year_ago.is_some());
        assert!(c.current.get("US10Y").is_some_and(|y| y > 0.0 && y < 20.0));
    }

    #[test]
    fn curve_picking() {
        let row = |d: i64, y: f64| YieldCurve {
            ts: d * 86_400,
            points: vec![CurvePoint { ticker: "US10Y".into(), tenor_years: 10.0, yield_pct: y }],
        };
        let c = pick_curves(vec![row(400, 1.0), row(1, 9.0), row(370, 2.0), row(760, 3.0), row(800, 4.0)]).unwrap();
        assert_eq!(c.current.get("US10Y"), Some(4.0));
        assert_eq!(c.month_ago.unwrap().get("US10Y"), Some(3.0));
        assert_eq!(c.year_ago.unwrap().get("US10Y"), Some(1.0)); // latest at or before day 435
    }

    #[test]
    fn clip_respects_end_and_limit() {
        let bars: Vec<Bar> = (0..10).map(|i| Bar::new(i, 1.0, 1.0, 1.0, 1.0, 1.0)).collect();
        let c = clip(bars, Some(8), 3);
        assert_eq!(c.iter().map(|b| b.ts).collect::<Vec<_>>(), vec![5, 6, 7]);
    }

    #[test]
    fn quote_change() {
        let q = Quote { symbol: "X".into(), last: 110.0, prev_close: 100.0, ts: 0, spark: vec![], crypto_source: None };
        assert_eq!(q.change(), 10.0);
        assert!((q.change_pct() - 10.0).abs() < 1e-9);
    }
    #[tokio::test]
    async fn crypto_failures_are_not_disguised_as_sample_quotes_books_or_options() {
        let provider = Fallback::new(Broken);
        for symbol in ["BTCUSD Curncy", "BINANCE:BTCUSDT Curncy", "BINANCE:BTCUSDT.P Curncy", "UNKNOWN:BTCUSD"] {
            assert!(provider.quotes(&[symbol.into()]).await.is_err(), "{symbol}");
            assert!(provider.order_book(symbol, 10).await.is_err(), "{symbol}");
            assert!(provider.options(symbol, None).await.is_err(), "{symbol}");
        }
        let mixed = provider.quotes(&["BTCUSD Curncy".into(), "AAPL US Equity".into()]).await.unwrap();
        assert_eq!(mixed.len(), 1);
        assert_eq!(mixed[0].symbol, "AAPL US Equity");
    }

    #[test]
    fn old_quotes_load_without_crypto_metadata_and_invalid_contracts_are_rejected() {
        let quote: Quote =
            serde_json::from_str(r#"{"symbol":"BTCUSD Curncy","last":1,"prev_close":1,"ts":0}"#).unwrap();
        assert!(quote.crypto_source.is_none());
        let mut json = serde_json::to_value(&quote).unwrap();
        json["crypto_source"] =
            serde_json::json!({"venue":"Binance","kind":"Spot","native_symbol":"BTCUSDT","base":"ETH","quote":"USD"});
        assert!(serde_json::from_value::<Quote>(json).is_err());
    }
}
