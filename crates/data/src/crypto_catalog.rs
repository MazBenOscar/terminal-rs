//! Validated, venue-specific spot discovery. Catalogs describe market-data
//! products and never imply that an account is entitled to trade them.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use tokio::sync::Mutex;
use tr_core::crypto::{CryptoInstrument, CryptoMarketKind, CryptoVenue, split_pair};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_PRODUCTS: usize = 30_000;
const TTL_SECONDS: i64 = 15 * 60;
const BINANCE_HOSTS: [&str; 2] = ["https://data-api.binance.vision", "https://api.binance.com"];
const COINBASE_HOSTS: [&str; 1] = ["https://api.exchange.coinbase.com"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    pub venue: CryptoVenue,
    pub fetched_at: i64,
    pub products: Vec<CryptoInstrument>,
}

/// Exchange-reported rolling 24-hour statistics. Volumes retain their native
/// base/quote units; an unavailable quote volume is never estimated from price.
#[derive(Debug, Clone, PartialEq)]
pub struct Rolling24h {
    pub instrument: CryptoInstrument,
    pub observed_at: i64,
    pub window_start: Option<i64>,
    pub window_end: Option<i64>,
    pub last: f64,
    pub change_percent: Option<f64>,
    pub base_volume: f64,
    pub quote_volume: Option<f64>,
}

impl Rolling24h {
    pub fn validate(&self) -> Result<()> {
        self.instrument.require_spot().map_err(anyhow::Error::msg)?;
        if !valid_time(self.observed_at)
            || [self.window_start, self.window_end].into_iter().flatten().any(|ts| !valid_time(ts))
            || self.window_start.is_some() != self.window_end.is_some()
        {
            bail!("invalid rolling statistics timestamps");
        }
        if let Some((start, end)) = self.window_start.zip(self.window_end) {
            // Freshness is an application policy. Allow small clock offsets and
            // rounding, but do not relabel an old exchange snapshot as current.
            if !(86_340..=86_460).contains(&end.saturating_sub(start))
                || end < self.observed_at.saturating_sub(120)
                || end > self.observed_at.saturating_add(30)
            {
                bail!("stale or invalid rolling 24-hour window");
            }
        }
        if !self.last.is_finite()
            || self.last <= 0.0
            || !self.base_volume.is_finite()
            || self.base_volume < 0.0
            || self.quote_volume.is_some_and(|volume| !volume.is_finite() || volume < 0.0)
            || self.change_percent.is_some_and(|change| !change.is_finite() || change < -100.0)
        {
            bail!("invalid rolling statistics values");
        }
        Ok(())
    }
}

fn valid_time(ts: i64) -> bool {
    ts > 0 && chrono::DateTime::from_timestamp(ts, 0).is_some()
}

fn number(value: &Value) -> Result<f64> {
    let number = match value {
        Value::String(text) => text.parse::<f64>().ok(),
        Value::Number(value) => value.as_f64(),
        _ => None,
    }
    .ok_or_else(|| anyhow!("invalid rolling statistics number"))?;
    if !number.is_finite() {
        bail!("non-finite rolling statistics number");
    }
    Ok(number)
}

fn optional_number(row: &Value, key: &str) -> Result<Option<f64>> {
    row.get(key).filter(|value| !value.is_null()).map(number).transpose()
}

fn milliseconds(row: &Value, key: &str) -> Result<Option<i64>> {
    row.get(key)
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_i64()
                .filter(|ms| *ms > 0)
                .map(|ms| ms / 1000)
                .ok_or_else(|| anyhow!("invalid rolling window {key}"))
        })
        .transpose()
}

/// Parse only the selected exchange's contract. Binance exposes exact rolling
/// boundaries and base/quote volume; Coinbase exposes base volume without
/// window timestamps or quote volume in this endpoint.
pub fn parse_rolling(instrument: &CryptoInstrument, body: &str, observed_at: i64) -> Result<Rolling24h> {
    if body.len() > MAX_BYTES {
        bail!("rolling statistics response exceeds size limit");
    }
    instrument.require_spot().map_err(anyhow::Error::msg)?;
    let row: Value = serde_json::from_str(body).context("parsing rolling crypto statistics")?;
    let (last, change, base, quote, start, end) = match instrument.venue {
        CryptoVenue::Binance => {
            if row["symbol"].as_str() != Some(instrument.native_symbol.as_str()) {
                bail!("rolling statistics product mismatch");
            }
            (
                number(&row["lastPrice"])?,
                optional_number(&row, "priceChangePercent")?,
                number(&row["volume"])?,
                optional_number(&row, "quoteVolume")?,
                milliseconds(&row, "openTime")?,
                milliseconds(&row, "closeTime")?,
            )
        }
        CryptoVenue::Coinbase => {
            let last = number(&row["last"])?;
            let open = optional_number(&row, "open")?;
            if open.is_some_and(|open| open < 0.0) {
                bail!("negative rolling opening price");
            }
            let change = open.filter(|open| *open > 0.0).map(|open| (last / open - 1.0) * 100.0);
            (last, change, number(&row["volume"])?, None, None, None)
        }
    };
    let statistics = Rolling24h {
        instrument: instrument.clone(),
        observed_at,
        window_start: start,
        window_end: end,
        last,
        change_percent: change,
        base_volume: base,
        quote_volume: quote,
    };
    statistics.validate()?;
    Ok(statistics)
}

#[derive(Default)]
pub(crate) struct CatalogCache {
    binance: Mutex<Option<Catalog>>,
    coinbase: Mutex<Option<Catalog>>,
}

impl CatalogCache {
    pub(crate) async fn load(&self, client: &reqwest::Client, venue: CryptoVenue) -> Result<Catalog> {
        let hosts = match venue {
            CryptoVenue::Binance => BINANCE_HOSTS.as_slice(),
            CryptoVenue::Coinbase => COINBASE_HOSTS.as_slice(),
        };
        self.load_from(client, venue, hosts, chrono::Utc::now().timestamp()).await
    }

    async fn load_from(
        &self,
        client: &reqwest::Client,
        venue: CryptoVenue,
        hosts: &[&str],
        now: i64,
    ) -> Result<Catalog> {
        let cache = match venue {
            CryptoVenue::Binance => &self.binance,
            CryptoVenue::Coinbase => &self.coinbase,
        };
        // Hold the per-venue lock through refresh: one in-flight request for
        // each exchange, with the other exchange free to refresh independently.
        let mut guard = cache.lock().await;
        if let Some(catalog) = guard.as_ref()
            && (0..TTL_SECONDS).contains(&now.saturating_sub(catalog.fetched_at))
        {
            return Ok(catalog.clone());
        }
        let products = tokio::time::timeout(Duration::from_secs(20), fetch(client, venue, hosts))
            .await
            .context("crypto catalog refresh timed out")??;
        let catalog = Catalog { venue, fetched_at: now, products };
        *guard = Some(catalog.clone());
        Ok(catalog)
    }
}

async fn fetch(client: &reqwest::Client, venue: CryptoVenue, hosts: &[&str]) -> Result<Vec<CryptoInstrument>> {
    let path = match venue {
        CryptoVenue::Binance => "/api/v3/exchangeInfo",
        CryptoVenue::Coinbase => "/products",
    };
    let mut last_error = anyhow!("no crypto catalog endpoint configured");
    for host in hosts {
        match async {
            let text = get_bounded(client, &format!("{host}{path}")).await?;
            parse_catalog(venue, &text)
        }
        .await
        {
            Ok(products) => return Ok(products),
            Err(error) => last_error = error,
        }
    }
    Err(last_error.context(format!("{} spot catalog unavailable", venue.label())))
}

async fn get_bounded(client: &reqwest::Client, url: &str) -> Result<String> {
    let mut response = client.get(url).send().await?;
    if !response.status().is_success() {
        bail!("crypto catalog HTTP {}", response.status());
    }
    if response.content_length().is_some_and(|size| size > MAX_BYTES as u64) {
        bail!("crypto catalog exceeds 32 MiB");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > MAX_BYTES {
            bail!("crypto catalog exceeds 32 MiB");
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).context("crypto catalog is not UTF-8")
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("invalid catalog {key}"))
}

fn flag(value: &Value, key: &str) -> Result<bool> {
    match value.get(key) {
        None => Ok(false),
        Some(value) => value.as_bool().ok_or_else(|| anyhow!("invalid catalog flag {key}")),
    }
}

fn supported_asset(asset: &str) -> bool {
    !asset.is_empty() && asset.len() <= 24 && asset.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

fn filter_increment(row: &Value, filter: &str, field: &str) -> Result<String> {
    let filters = row["filters"].as_array().ok_or_else(|| anyhow!("missing Binance filters"))?;
    let mut matches = filters.iter().filter(|value| value["filterType"].as_str() == Some(filter));
    let value = text(matches.next().ok_or_else(|| anyhow!("missing Binance {filter}"))?, field)?;
    if matches.next().is_some() {
        bail!("duplicate Binance {filter}");
    }
    Ok(value.into())
}

fn parse_product(venue: CryptoVenue, row: &Value) -> Result<Option<CryptoInstrument>> {
    let (base, quote, native, price, quantity) = match venue {
        CryptoVenue::Binance => {
            let status = text(row, "status")?;
            if status != "TRADING" {
                return Ok(None);
            }
            if !row
                .get("isSpotTradingAllowed")
                .and_then(Value::as_bool)
                .ok_or_else(|| anyhow!("missing spot permission"))?
            {
                return Ok(None);
            }
            let base = text(row, "baseAsset")?;
            let quote = text(row, "quoteAsset")?;
            let native = text(row, "symbol")?;
            if native != format!("{base}{quote}") {
                bail!("Binance product does not match its assets");
            }
            if !supported_asset(base) || !supported_asset(quote) || split_pair(native) != Some((base, quote)) {
                return Ok(None);
            }
            (
                base,
                quote,
                native,
                filter_increment(row, "PRICE_FILTER", "tickSize")?,
                filter_increment(row, "LOT_SIZE", "stepSize")?,
            )
        }
        CryptoVenue::Coinbase => {
            if text(row, "status")? != "online"
                || flag(row, "trading_disabled")?
                || flag(row, "cancel_only")?
                || flag(row, "auction_mode")?
            {
                return Ok(None);
            }
            let base = text(row, "base_currency")?;
            let quote = text(row, "quote_currency")?;
            let native = text(row, "id")?;
            if native != format!("{base}-{quote}") {
                bail!("Coinbase product does not match its assets");
            }
            if !supported_asset(base) || !supported_asset(quote) {
                return Ok(None);
            }
            // Limit-only and post-only products still supply analysis data.
            flag(row, "limit_only")?;
            flag(row, "post_only")?;
            (base, quote, native, text(row, "quote_increment")?.into(), text(row, "base_increment")?.into())
        }
    };
    let product = CryptoInstrument {
        venue,
        native_symbol: native.into(),
        base: base.into(),
        quote: quote.into(),
        kind: CryptoMarketKind::Spot,
        settlement: None,
        expiry: None,
        contract_multiplier: None,
        price_increment: Some(price),
        quantity_increment: Some(quantity),
    };
    product.validate().map_err(anyhow::Error::msg)?;
    Ok(Some(product))
}

/// Parse a complete authoritative response. Bad active metadata fails the
/// snapshot; offline and unsupported products are deliberately excluded.
pub fn parse_catalog(venue: CryptoVenue, body: &str) -> Result<Vec<CryptoInstrument>> {
    if body.len() > MAX_BYTES {
        bail!("crypto catalog exceeds 32 MiB");
    }
    let value: Value = serde_json::from_str(body).context("parsing crypto catalog")?;
    let rows = match venue {
        CryptoVenue::Binance => value.get("symbols"),
        CryptoVenue::Coinbase => Some(&value),
    }
    .and_then(Value::as_array)
    .ok_or_else(|| anyhow!("invalid crypto catalog root"))?;
    if rows.len() > MAX_PRODUCTS {
        bail!("crypto catalog exceeds 30000 products");
    }
    let mut products = BTreeMap::new();
    for row in rows {
        if let Some(product) = parse_product(venue, row)? {
            let key = product.canonical_ticker();
            if let Some(existing) = products.get(&key)
                && existing != &product
            {
                bail!("conflicting crypto catalog product {key}");
            }
            products.insert(key, product);
        }
    }
    if products.is_empty() {
        bail!("crypto catalog has no supported active spot products");
    }
    Ok(products.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    const BINANCE: &str = r#"{"symbols":[{"symbol":"BTCUSDT","baseAsset":"BTC","quoteAsset":"USDT","status":"TRADING","isSpotTradingAllowed":true,"permissions":[],"permissionSets":[["SPOT"]],"filters":[{"filterType":"PRICE_FILTER","tickSize":"0.01000000"},{"filterType":"LOT_SIZE","stepSize":"0.00001000"}]}]}"#;
    const COINBASE: &str = r#"[{"id":"BTC-USD","base_currency":"BTC","quote_currency":"USD","status":"online","quote_increment":"0.01","base_increment":"0.00000001","cancel_only":false,"post_only":true,"limit_only":true}]"#;

    #[test]
    fn rolling_statistics_preserve_window_and_native_volume_units() {
        let product = CryptoInstrument::parse("BINANCE:BTCUSDT").unwrap();
        let body = r#"{"symbol":"BTCUSDT","lastPrice":"110","priceChangePercent":"10","volume":"2","quoteVolume":"215","openTime":1000000,"closeTime":87400000}"#;
        let statistics = parse_rolling(&product, body, 87_401).unwrap();
        assert_eq!(statistics.base_volume, 2.0);
        assert_eq!(statistics.quote_volume, Some(215.0));
        assert_eq!(statistics.change_percent, Some(10.0));
        assert_eq!((statistics.window_start, statistics.window_end), (Some(1_000), Some(87_400)));
        let product = CryptoInstrument::parse("COINBASE:BTC-USD").unwrap();
        let statistics =
            parse_rolling(&product, r#"{"last":"110","open":"100","volume":"2","volume_30day":"9999"}"#, 87_401)
                .unwrap();
        assert!((statistics.change_percent.unwrap() - 10.0).abs() < 1e-10);
        assert_eq!(statistics.base_volume, 2.0);
        assert_eq!(statistics.quote_volume, None);
        assert_eq!((statistics.window_start, statistics.window_end), (None, None));
    }

    #[test]
    fn zero_volume_is_valid_but_unknown_change_is_not_zero() {
        let product = CryptoInstrument::parse("COINBASE:BTC-USD").unwrap();
        for open in ["0", "null"] {
            let body = format!(r#"{{"last":"100","open":{open},"volume":"0"}}"#);
            let statistics = parse_rolling(&product, &body, 1_000).unwrap();
            assert_eq!(statistics.base_volume, 0.0);
            assert_eq!(statistics.change_percent, None);
            assert_eq!(statistics.quote_volume, None);
        }
    }

    #[test]
    fn rolling_window_must_be_paired_recent_and_24_hours() {
        let product = CryptoInstrument::parse("BINANCE:BTCUSDT").unwrap();
        let body = r#"{"symbol":"BTCUSDT","lastPrice":"110","volume":"2","openTime":1000000,"closeTime":87400000}"#;
        let statistics = parse_rolling(&product, body, 87_401).unwrap();
        for offset in [-121, 31] {
            let mut changed = statistics.clone();
            let end = changed.observed_at + offset;
            changed.window_end = Some(end);
            changed.window_start = Some(end - 86_400);
            assert!(changed.validate().is_err());
        }
        let mut changed = statistics.clone();
        changed.window_start = None;
        assert!(changed.validate().is_err());
        let mut changed = statistics.clone();
        changed.window_end = None;
        assert!(changed.validate().is_err());
        for duration in [86_339, 86_461] {
            let mut changed = statistics.clone();
            changed.window_start = Some(changed.window_end.unwrap() - duration);
            assert!(changed.validate().is_err());
        }
    }

    #[test]
    fn rolling_statistics_fail_closed_on_invalid_values_products_or_windows() {
        let product = CryptoInstrument::parse("BINANCE:BTCUSDT").unwrap();
        for body in [
            r#"{"symbol":"ETHUSDT","lastPrice":"100","volume":"1"}"#,
            r#"{"symbol":"BTCUSDT","lastPrice":"NaN","volume":"1"}"#,
            r#"{"symbol":"BTCUSDT","lastPrice":"0","volume":"1"}"#,
            r#"{"symbol":"BTCUSDT","lastPrice":"100","volume":"-1"}"#,
            r#"{"symbol":"BTCUSDT","lastPrice":"100","volume":"1","quoteVolume":"-1"}"#,
            r#"{"symbol":"BTCUSDT","lastPrice":"100","volume":"1","openTime":2000000,"closeTime":1000000}"#,
            r#"{"symbol":"BTCUSDT","lastPrice":"100","volume":"1","priceChangePercent":"NaN"}"#,
        ] {
            assert!(parse_rolling(&product, body, 3_000).is_err());
        }
        let coinbase = CryptoInstrument::parse("COINBASE:BTC-USD").unwrap();
        assert!(parse_rolling(&coinbase, r#"{"last":"100","open":"-1","volume":"1"}"#, 1_000).is_err());
        assert!(parse_rolling(&coinbase, r#"{"last":"100","volume":null}"#, 1_000).is_err());
        assert!(parse_rolling(&coinbase, r#"{"last":"100","volume":"1"}"#, -1).is_err());
    }

    #[test]
    fn exact_spot_metadata_and_precision() {
        let binance = parse_catalog(CryptoVenue::Binance, BINANCE).unwrap();
        assert_eq!(binance[0].canonical_ticker(), "BINANCE:BTCUSDT");
        assert_eq!(binance[0].price_increment.as_deref(), Some("0.01000000"));
        let coinbase = parse_catalog(CryptoVenue::Coinbase, COINBASE).unwrap();
        assert_eq!(coinbase[0].canonical_ticker(), "COINBASE:BTC-USD");
        assert_eq!(coinbase[0].quantity_increment.as_deref(), Some("0.00000001"));
    }

    #[test]
    fn excludes_inactive_unsupported_and_noncontinuous_products() {
        for changed in [
            BINANCE.replace("TRADING", "HALT"),
            BINANCE.replace("\"isSpotTradingAllowed\":true", "\"isSpotTradingAllowed\":false"),
            BINANCE.replace("USDT", "XYZ"),
        ] {
            assert!(parse_catalog(CryptoVenue::Binance, &changed).is_err());
        }
        for key in ["cancel_only", "trading_disabled", "auction_mode"] {
            let mut value: Value = serde_json::from_str(COINBASE).unwrap();
            value[0][key] = Value::Bool(true);
            assert!(parse_catalog(CryptoVenue::Coinbase, &value.to_string()).is_err());
        }
    }

    #[test]
    fn rejects_malformed_and_conflicting_metadata() {
        for body in [
            "{}",
            "[]",
            "{\"symbols\":[{}]}",
            &BINANCE.replace("BTCUSDT", "ETHUSDT"),
            &BINANCE.replace("0.01000000", "NaN"),
        ] {
            assert!(parse_catalog(CryptoVenue::Binance, body).is_err());
        }
        let mut value: Value = serde_json::from_str(COINBASE).unwrap();
        let mut duplicate = value[0].clone();
        duplicate["quote_increment"] = Value::String("0.001".into());
        value.as_array_mut().unwrap().push(duplicate);
        assert!(parse_catalog(CryptoVenue::Coinbase, &value.to_string()).is_err());
        value[1] = value[0].clone();
        assert_eq!(parse_catalog(CryptoVenue::Coinbase, &value.to_string()).unwrap().len(), 1);
        assert!(parse_catalog(CryptoVenue::Coinbase, &" ".repeat(MAX_BYTES + 1)).is_err());
        assert!(
            parse_catalog(CryptoVenue::Coinbase, &serde_json::to_string(&vec![Value::Null; MAX_PRODUCTS + 1]).unwrap())
                .is_err()
        );
    }

    fn server(responses: Vec<(u16, String)>) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let thread = std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut request = [0; 4096];
                let _ = socket.read(&mut request).unwrap();
                write!(
                    socket,
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (host, thread)
    }

    #[tokio::test]
    async fn snapshot_ttl_single_flight_and_failed_refresh() {
        let cache = CatalogCache::default();
        let client = reqwest::Client::new();
        let (host, thread) = server(vec![(200, COINBASE.into()), (503, "unavailable".into())]);
        let hosts = [host.as_str()];
        let (first, simultaneous) = tokio::join!(
            cache.load_from(&client, CryptoVenue::Coinbase, &hosts, 1_000),
            cache.load_from(&client, CryptoVenue::Coinbase, &hosts, 1_000)
        );
        assert_eq!(first.unwrap(), simultaneous.unwrap());
        assert_eq!(cache.load_from(&client, CryptoVenue::Coinbase, &hosts, 1_899).await.unwrap().fetched_at, 1_000);
        assert!(cache.load_from(&client, CryptoVenue::Coinbase, &hosts, 1_900).await.is_err());
        thread.join().unwrap();
    }

    #[tokio::test]
    async fn mirror_failure_retries_only_provided_venue_and_preserves_error() {
        let client = reqwest::Client::new();
        let (bad, bad_thread) = server(vec![(451, "restricted".into())]);
        let (good, good_thread) = server(vec![(200, BINANCE.into())]);
        let products = fetch(&client, CryptoVenue::Binance, &[&bad, &good]).await.unwrap();
        assert!(products.iter().all(|product| product.venue == CryptoVenue::Binance));
        bad_thread.join().unwrap();
        good_thread.join().unwrap();
    }

    #[tokio::test]
    async fn declared_oversized_catalog_is_rejected_before_body_read() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = [0; 4096];
            let _ = socket.read(&mut request).unwrap();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", MAX_BYTES + 1)
                .unwrap();
        });
        let error = get_bounded(&reqwest::Client::new(), &url).await.unwrap_err();
        assert!(error.to_string().contains("32 MiB"));
        server.join().unwrap();
    }
}
