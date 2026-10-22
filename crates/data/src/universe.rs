//! US-listed common stocks from Nasdaq Trader's daily symbol directory (`nasdaqlisted.txt`
//! for Nasdaq, `otherlisted.txt` for NYSE, NYSE American and the rest). ETFs, test issues,
//! warrants, units, rights, preferreds, notes and depositary receipts (ADRs) are excluded,
//! which approximates StockCharts' `[type = stock] and [country = us]`: listing venue, not
//! company domicile, decides "US".

use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const NASDAQ_LISTED: &str = "https://www.nasdaqtrader.com/dynamic/SymDir/nasdaqlisted.txt";
pub const OTHER_LISTED: &str = "https://www.nasdaqtrader.com/dynamic/SymDir/otherlisted.txt";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Venue {
    Nasdaq,
    Nyse,
    NyseAmerican,
    /// NYSE Arca, Cboe and other venues listing a handful of common stocks.
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Listing {
    pub symbol: String,
    pub name: String,
    pub venue: Venue,
}

impl Listing {
    /// Market key that always resolves as a US equity (`CL` is Colgate, not crude oil).
    pub fn key(&self) -> String {
        format!("{} US Equity", self.symbol)
    }
}

/// What a scan runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Universe {
    #[default]
    Watchlist,
    UsStocks,
    Nasdaq,
    /// NYSE and NYSE American.
    Nyse,
    BinanceSpot,
    CoinbaseSpot,
}

impl Universe {
    pub const ALL: [Self; 6] =
        [Self::Watchlist, Self::UsStocks, Self::Nasdaq, Self::Nyse, Self::BinanceSpot, Self::CoinbaseSpot];

    pub fn crypto_venue(self) -> Option<tr_core::crypto::CryptoVenue> {
        use tr_core::crypto::CryptoVenue;
        match self {
            Self::BinanceSpot => Some(CryptoVenue::Binance),
            Self::CoinbaseSpot => Some(CryptoVenue::Coinbase),
            _ => None,
        }
    }

    pub fn requires_bulk(self) -> bool {
        matches!(self, Self::UsStocks | Self::Nasdaq | Self::Nyse)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Watchlist => "Watchlist",
            Self::UsStocks => "All US stocks",
            Self::Nasdaq => "Nasdaq stocks",
            Self::Nyse => "NYSE stocks",
            Self::BinanceSpot => "Binance spot",
            Self::CoinbaseSpot => "Coinbase spot",
        }
    }

    pub fn includes(self, venue: Venue) -> bool {
        match self {
            Self::Watchlist | Self::BinanceSpot | Self::CoinbaseSpot => false,
            Self::UsStocks => true,
            Self::Nasdaq => venue == Venue::Nasdaq,
            Self::Nyse => matches!(venue, Venue::Nyse | Venue::NyseAmerican),
        }
    }
}

const EXCLUDED_WORDS: [&str; 12] = [
    "warrant",
    "warrants",
    "unit",
    "units",
    "right",
    "rights",
    "preferred",
    "depositary",
    "debenture",
    "debentures",
    "note",
    "notes",
];

/// Common stock: not an ETF or test issue, a plain ticker (an optional one-letter share class
/// such as `BRK.B`, but not `.U`/`.W`/`.R` units, warrants and rights), and no excluded word.
fn is_common_stock(symbol: &str, name: &str, etf: &str, test: &str) -> bool {
    let plain = match symbol.split_once('.') {
        None => !symbol.is_empty() && symbol.chars().all(|c| c.is_ascii_uppercase()),
        Some((base, class)) => {
            !base.is_empty()
                && base.chars().all(|c| c.is_ascii_uppercase())
                && class.len() == 1
                && class.chars().all(|c| c.is_ascii_uppercase() && !"UWR".contains(c))
        }
    };
    let lower = name.to_ascii_lowercase();
    let excluded = lower.contains('%')
        || lower.split(|c: char| !c.is_ascii_alphanumeric()).any(|word| EXCLUDED_WORDS.contains(&word));
    etf == "N" && test == "N" && plain && !excluded
}

/// Rows of a pipe-delimited directory file as (header → value) lookups, skipping the trailer.
fn rows(text: &str, required: &[&str]) -> Result<Vec<Vec<(String, String)>>> {
    let mut lines = text.lines();
    let header: Vec<String> = lines.next().context("empty symbol directory")?.split('|').map(str::to_string).collect();
    for column in required {
        if !header.iter().any(|h| h == column) {
            bail!("symbol directory format changed: no {column} column");
        }
    }
    Ok(lines
        .filter(|l| !l.trim().is_empty() && !l.starts_with("File Creation Time"))
        .map(|l| header.iter().cloned().zip(l.split('|').map(|v| v.trim().to_string())).collect())
        .collect())
}

fn field<'a>(row: &'a [(String, String)], name: &str) -> &'a str {
    row.iter().find(|(h, _)| h == name).map_or("", |(_, v)| v.as_str())
}

pub fn parse_nasdaq_listed(text: &str) -> Result<Vec<Listing>> {
    let rows = rows(text, &["Symbol", "Security Name", "Test Issue", "ETF"])?;
    Ok(rows
        .iter()
        .filter(|r| {
            is_common_stock(field(r, "Symbol"), field(r, "Security Name"), field(r, "ETF"), field(r, "Test Issue"))
        })
        .map(|r| Listing {
            symbol: field(r, "Symbol").into(),
            name: field(r, "Security Name").into(),
            venue: Venue::Nasdaq,
        })
        .collect())
}

pub fn parse_other_listed(text: &str) -> Result<Vec<Listing>> {
    let rows = rows(text, &["ACT Symbol", "Security Name", "Exchange", "ETF", "Test Issue"])?;
    Ok(rows
        .iter()
        .filter(|r| {
            is_common_stock(field(r, "ACT Symbol"), field(r, "Security Name"), field(r, "ETF"), field(r, "Test Issue"))
        })
        .map(|r| Listing {
            symbol: field(r, "ACT Symbol").into(),
            name: field(r, "Security Name").into(),
            venue: match field(r, "Exchange") {
                "N" => Venue::Nyse,
                "A" => Venue::NyseAmerican,
                _ => Venue::Other,
            },
        })
        .collect())
}

/// Download both directory files.
pub async fn fetch() -> Result<Vec<Listing>> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).user_agent("terminal-rs").build()?;
    let get = async |url: &str| -> Result<String> {
        let resp = client.get(url).send().await.with_context(|| format!("downloading {url}"))?;
        if !resp.status().is_success() {
            bail!("{url}: HTTP {}", resp.status().as_u16());
        }
        Ok(resp.text().await?)
    };
    let mut listings = parse_nasdaq_listed(&get(NASDAQ_LISTED).await?)?;
    listings.extend(parse_other_listed(&get(OTHER_LISTED).await?)?);
    listings.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    listings.dedup_by(|a, b| a.symbol == b.symbol);
    Ok(listings)
}

/// The cached directory at `path` when younger than `max_age`, otherwise a fresh download
/// (falling back to an older cache if the download fails).
pub async fn load_or_fetch(path: &Path, max_age: Duration) -> Result<Vec<Listing>> {
    let cached = || -> Option<Vec<Listing>> { serde_json::from_slice(&std::fs::read(path).ok()?).ok() };
    let age =
        std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok());
    if age.is_some_and(|a| a < max_age)
        && let Some(listings) = cached()
    {
        return Ok(listings);
    }
    match fetch().await {
        Ok(listings) => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Err(e) = std::fs::write(path, serde_json::to_vec(&listings)?) {
                tracing::warn!("could not cache the symbol directory: {e}");
            }
            Ok(listings)
        }
        Err(e) => cached().filter(|l| !l.is_empty()).ok_or(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NASDAQ: &str =
        "Symbol|Security Name|Market Category|Test Issue|Financial Status|Round Lot Size|ETF|NextShares
AAPL|Apple Inc. - Common Stock|Q|N|N|100|N|N
AAAP|Pacer Barings CLO Market Flex ETF|G|N|N|100|Y|N
AACG|ATA Creativity Global - American Depositary Shares, each representing two common shares|S|N|D|100|N|N
ZXZZT|NASDAQ TEST STOCK|G|Y|N|100|N|N
ABLLW|Abacus Life, Inc. - Warrant|S|N|N|100|N|N
File Creation Time: 1020202615:41|||||||";

    const OTHER: &str = "ACT Symbol|Security Name|Exchange|CQS Symbol|ETF|Round Lot Size|Test Issue|NASDAQ Symbol
A|Agilent Technologies, Inc. Common Stock|N|A|N|100|N|A
AAC.U|Ares Acquisition Corporation III Units|N|AAC.U|N|100|N|AAC=
ABR$D|Arbor Realty Trust 6.375% Series D Preferred|N|ABRpD|N|100|N|ABR-D
BRK.B|Berkshire Hathaway Inc. Class B|N|BRK.B|N|100|N|BRK=B
IMO|Imperial Oil Limited Common Stock|A|IMO|N|100|N|IMO
SPY|SPDR S&P 500 ETF Trust|P|SPY|Y|100|N|SPY
File Creation Time: 1020202615:41||||||";

    #[test]
    fn keeps_only_us_common_stock() {
        let nasdaq = parse_nasdaq_listed(NASDAQ).unwrap();
        assert_eq!(nasdaq.iter().map(|l| l.symbol.as_str()).collect::<Vec<_>>(), ["AAPL"]);
        let other = parse_other_listed(OTHER).unwrap();
        assert_eq!(other.iter().map(|l| l.symbol.as_str()).collect::<Vec<_>>(), ["A", "BRK.B", "IMO"]);
        assert_eq!(other[2].venue, Venue::NyseAmerican);
        assert_eq!(other[1].key(), "BRK.B US Equity");
    }

    #[test]
    fn universes_select_venues() {
        assert!(Universe::UsStocks.includes(Venue::Other));
        assert!(Universe::Nyse.includes(Venue::NyseAmerican) && !Universe::Nyse.includes(Venue::Nasdaq));
        assert!(Universe::Nasdaq.includes(Venue::Nasdaq) && !Universe::Nasdaq.includes(Venue::Nyse));
        assert!(!Universe::Watchlist.includes(Venue::Nasdaq));
    }

    #[test]
    fn a_changed_format_is_an_error_not_an_empty_universe() {
        assert!(parse_nasdaq_listed("Ticker|Name\nAAPL|Apple").is_err());
    }
}
