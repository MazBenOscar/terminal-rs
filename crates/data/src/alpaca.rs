//! Alpaca Market Data v2 (free tier uses the IEX feed).
//! Keys come from `APCA_API_KEY_ID` / `APCA_API_SECRET_KEY`.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tr_core::{Bar, Timeframe};

use crate::MarketData;

const BASE: &str = "https://data.alpaca.markets/v2/stocks";

pub struct AlpacaProvider {
    client: reqwest::Client,
    key: String,
    secret: String,
    feed: String,
    stream: std::sync::Arc<crate::alpaca_stream::Hub>,
    streaming: bool,
}

impl AlpacaProvider {
    pub fn new(key: String, secret: String, feed: String) -> Result<Self> {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build()?;
        let streaming = std::env::var("TR_ALPACA_STREAM").is_ok_and(|s| s == "1");
        Ok(Self { client, key, secret, feed, stream: Default::default(), streaming })
    }
    pub fn streaming_enabled(&self) -> bool {
        self.streaming
    }
    pub fn feed_name(&self) -> &str {
        &self.feed
    }
}

fn validate_options(tf: Timeframe, options: crate::HistoryOptions) -> Result<()> {
    if options.extended_hours && !tf.is_intraday() {
        bail!("Alpaca history does not support extended hours for non-intraday bars");
    }
    // Intraday bars include pre/post-market trades; do not apply a session filter.
    Ok(())
}

fn timeframe(tf: Timeframe) -> &'static str {
    match tf {
        Timeframe::M2 | Timeframe::M3 => unreachable!("resampled interval"),
        Timeframe::H2 | Timeframe::MN3 => unreachable!("resampled interval"),
        Timeframe::M1 => "1Min",
        Timeframe::M5 => "5Min",
        Timeframe::M15 => "15Min",
        Timeframe::M30 => "30Min",
        Timeframe::H1 => "1Hour",
        Timeframe::H4 => "4Hour",
        Timeframe::D1 => "1Day",
        Timeframe::W1 => "1Week",
        Timeframe::MN1 => "1Month",
    }
}

#[derive(Deserialize)]
struct BarsPage {
    #[serde(default)]
    bars: Option<Vec<RawBar>>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct RawBar {
    t: DateTime<Utc>,
    o: f64,
    h: f64,
    l: f64,
    c: f64,
    v: f64,
}

/// Parse one page of `/v2/stocks/{symbol}/bars`.
pub fn parse_bars_page(json: &str) -> Result<(Vec<Bar>, Option<String>)> {
    let page: BarsPage = serde_json::from_str(json).context("parsing Alpaca bars")?;
    let bars =
        page.bars.unwrap_or_default().into_iter().map(|b| Bar::new(b.t.timestamp(), b.o, b.h, b.l, b.c, b.v)).collect();
    Ok((bars, page.next_page_token.filter(|t| !t.is_empty())))
}

#[async_trait]
impl MarketData for AlpacaProvider {
    fn live(&self, symbol: &str, tf: Timeframe) -> Option<crate::LiveFeed> {
        if self.streaming && tf == Timeframe::M1 {
            self.stream.subscribe(symbol, self.key.clone(), self.secret.clone(), self.feed.clone())
        } else {
            None
        }
    }
    fn name(&self) -> &str {
        "alpaca"
    }

    async fn history(&self, symbol: &str, tf: Timeframe, end: Option<i64>, limit: usize) -> Result<Vec<Bar>> {
        self.chart_history(symbol, tf, end, limit, crate::HistoryOptions::default()).await
    }
    async fn chart_history(
        &self,
        symbol: &str,
        tf: Timeframe,
        end: Option<i64>,
        limit: usize,
        options: crate::HistoryOptions,
    ) -> Result<Vec<Bar>> {
        validate_options(tf, options)?;
        if let Some((base, factor)) = tf.source_interval() {
            let bars =
                self.chart_history(symbol, base, end, limit.saturating_add(1).saturating_mul(factor), options).await?;
            return Ok(crate::clip(tf.resample(&bars), end, limit));
        }
        // Alpaca free data has a 15 minute delay for the latest bars on the SIP feed;
        // IEX is real-time. Walk backwards from `end` with sort=desc.
        let end_ts = end.unwrap_or_else(|| Utc::now().timestamp());
        let end_dt = DateTime::<Utc>::from_timestamp(end_ts - 1, 0).unwrap_or_else(Utc::now);
        let mut out: Vec<Bar> = Vec::with_capacity(limit);
        let mut token: Option<String> = None;
        while out.len() < limit {
            let mut q: Vec<(&str, String)> = vec![
                ("timeframe", timeframe(tf).into()),
                ("end", end_dt.to_rfc3339()),
                ("start", "2000-01-01T00:00:00Z".into()),
                ("limit", (limit - out.len()).min(10_000).to_string()),
                ("adjustment", if options.price_basis == crate::PriceBasis::Raw { "raw" } else { "split" }.into()),
                ("feed", self.feed.clone()),
                ("sort", "desc".into()),
            ];
            if let Some(t) = &token {
                q.push(("page_token", t.clone()));
            }
            let resp = self
                .client
                .get(format!("{BASE}/{symbol}/bars"))
                .header("APCA-API-KEY-ID", &self.key)
                .header("APCA-API-SECRET-KEY", &self.secret)
                .query(&q)
                .send()
                .await?;
            if !resp.status().is_success() {
                bail!("alpaca {symbol}: HTTP {}", resp.status());
            }
            let (bars, next) = parse_bars_page(&resp.text().await?)?;
            let n = bars.len();
            out.extend(bars);
            token = next;
            if token.is_none() || n == 0 {
                break;
            }
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
    fn extended_hours_requires_intraday_bars() {
        let options = crate::HistoryOptions { extended_hours: true, ..Default::default() };
        assert!(validate_options(Timeframe::M1, options).is_ok());
        assert!(validate_options(Timeframe::H4, options).is_ok());
        assert!(
            validate_options(Timeframe::D1, options)
                .unwrap_err()
                .to_string()
                .contains("Alpaca history does not support")
        );
        assert!(validate_options(Timeframe::D1, crate::HistoryOptions::default()).is_ok());
    }
    #[test]
    fn parses_fixture() {
        let (bars, next) = parse_bars_page(include_str!("../tests/fixtures/alpaca_bars.json")).unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[1].close, 336.64);
        assert_eq!(bars[0].ts, 1_791_432_000);
        assert!(next.is_none());
        let (empty, _) = parse_bars_page(r#"{"bars":null,"next_page_token":null}"#).unwrap();
        assert!(empty.is_empty());
    }
}
