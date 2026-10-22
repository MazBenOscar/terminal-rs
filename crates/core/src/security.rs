//! Securities across asset classes, addressed with Bloomberg-style market keys:
//! `AAPL US Equity`, `SPX Index`, `BTCUSD Curncy`, `EURUSD Curncy`, `US10Y Govt`, `CL1 Comdty`.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::catalog;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AssetClass {
    Equity,
    Etf,
    Index,
    Fx,
    Crypto,
    Govt,
    Cmdty,
}

/// The Bloomberg "yellow key" a security lives under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum YellowKey {
    Equity,
    Index,
    Curncy,
    Govt,
    Comdty,
}

impl YellowKey {
    pub const ALL: [YellowKey; 5] =
        [YellowKey::Equity, YellowKey::Index, YellowKey::Curncy, YellowKey::Govt, YellowKey::Comdty];

    /// Accepts `<CRNCY>`, `CURNCY`, `crncy`, `CMDTY`, `COMDTY`, `GOVT`, `INDEX`, `EQUITY`, `EQ`, `ETF`.
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim().trim_start_matches('<').trim_end_matches('>').to_ascii_uppercase();
        Some(match t.as_str() {
            "EQUITY" | "EQ" | "EQTY" | "ETF" => YellowKey::Equity,
            "INDEX" | "IDX" => YellowKey::Index,
            "CRNCY" | "CURNCY" | "CURRENCY" | "CCY" | "CRYPTO" | "FX" => YellowKey::Curncy,
            "GOVT" | "GOV" | "RATES" => YellowKey::Govt,
            "CMDTY" | "COMDTY" | "COMMODITY" | "FUT" => YellowKey::Comdty,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            YellowKey::Equity => "Equity",
            YellowKey::Index => "Index",
            YellowKey::Curncy => "Curncy",
            YellowKey::Govt => "Govt",
            YellowKey::Comdty => "Comdty",
        }
    }
}

impl AssetClass {
    pub fn yellow_key(self) -> YellowKey {
        match self {
            AssetClass::Equity | AssetClass::Etf => YellowKey::Equity,
            AssetClass::Index => YellowKey::Index,
            AssetClass::Fx | AssetClass::Crypto => YellowKey::Curncy,
            AssetClass::Govt => YellowKey::Govt,
            AssetClass::Cmdty => YellowKey::Comdty,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AssetClass::Equity => "Equity",
            AssetClass::Etf => "ETF",
            AssetClass::Index => "Index",
            AssetClass::Fx => "FX",
            AssetClass::Crypto => "Crypto",
            AssetClass::Govt => "Govt",
            AssetClass::Cmdty => "Comdty",
        }
    }

    /// Annualised volatility used for synthetic sample data.
    pub fn sample_vol(self) -> f64 {
        match self {
            AssetClass::Equity => 0.30,
            AssetClass::Etf | AssetClass::Index => 0.18,
            AssetClass::Fx => 0.08,
            AssetClass::Crypto => 0.60,
            AssetClass::Govt => 0.20,
            AssetClass::Cmdty => 0.30,
        }
    }

    /// Exchanges trade crypto around the clock; everything else has sessions.
    pub fn trades_24_7(self) -> bool {
        self == AssetClass::Crypto
    }
}

const FIAT: [&str; 26] = [
    "USD", "EUR", "JPY", "GBP", "CHF", "AUD", "CAD", "NZD", "SEK", "NOK", "DKK", "CNH", "CNY", "HKD", "SGD", "MXN",
    "BRL", "ZAR", "TRY", "INR", "KRW", "PLN", "HUF", "CZK", "ILS", "THB",
];

pub fn is_fiat(code: &str) -> bool {
    FIAT.contains(&code)
}

/// A resolved security.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Security {
    pub ticker: String,
    pub class: AssetClass,
}

impl Security {
    pub fn new(ticker: impl Into<String>, class: AssetClass) -> Self {
        Self { ticker: ticker.into().to_ascii_uppercase(), class }
    }

    pub fn equity(ticker: &str) -> Self {
        Self::new(ticker, AssetClass::Equity)
    }

    /// Canonical market key, e.g. `AAPL US Equity`, `BTCUSD Curncy`.
    pub fn key(&self) -> String {
        match self.class {
            AssetClass::Equity | AssetClass::Etf => format!("{} US Equity", self.ticker),
            c => format!("{} {}", self.ticker, c.yellow_key().label()),
        }
    }

    /// Resolve user input such as `BTCUSD <CRNCY>`, `aapl us equity`, `US10Y GOVT`, `CL1`.
    /// Returns the security and how many input tokens it consumed.
    pub fn resolve_tokens(tokens: &[&str]) -> Option<(Self, usize)> {
        let (&head, rest) = tokens.split_first()?;
        if head.chars().any(char::is_control) {
            return None;
        }
        // `BTC/USD` is a pair; `BRK/B` is a share class.
        let up = head.trim().to_ascii_uppercase();
        if up.contains(':') {
            let instrument = crate::crypto::CryptoInstrument::parse(&up).ok()?;
            let mut used = 1;
            if rest.first().is_some_and(|word| word.eq_ignore_ascii_case("US")) {
                return None;
            }
            if let Some(key) = rest.first().and_then(|word| YellowKey::parse(word)) {
                if key != YellowKey::Curncy {
                    return None;
                }
                used += 1;
            }
            return Some((instrument.security(), used));
        }
        let ticker = match up.split_once('/') {
            Some((a, b)) if a.len() >= 3 && b.len() >= 3 => format!("{a}{b}"),
            Some((a, b)) => format!("{a}.{b}"),
            None => up,
        };
        if !looks_like_ticker(&ticker) {
            return None;
        }
        // Optional market/country word (`US`) then a yellow key.
        let mut used = 1;
        let mut key = None;
        for t in rest.iter().take(2) {
            if let Some(k) = YellowKey::parse(t) {
                key = Some(k);
                used += 1;
                break;
            }
            if t.eq_ignore_ascii_case("US") {
                used += 1;
                continue;
            }
            break;
        }
        if key.is_none() && used > 1 {
            used = 1; // a stray `US` without a yellow key belongs to the next function
        }
        Some((Self::classify(&ticker, key), used))
    }

    /// Parse a full key or bare ticker (e.g. persisted settings).
    pub fn parse(input: &str) -> Option<Self> {
        if input.len() > 256 || input.chars().any(char::is_control) {
            return None;
        }
        let tokens: Vec<&str> = input.split_whitespace().collect();
        let (security, consumed) = Self::resolve_tokens(&tokens)?;
        if tokens.first().is_some_and(|head| head.contains(':')) && consumed != tokens.len() {
            return None;
        }
        Some(security)
    }

    pub fn crypto_instrument(&self) -> Result<crate::crypto::CryptoInstrument, String> {
        crate::crypto::CryptoInstrument::from_security(self)
    }

    fn classify(ticker: &str, key: Option<YellowKey>) -> Self {
        let listed = catalog::lookup(ticker, key);
        if let Some(l) = listed {
            return Self::new(l.ticker, l.class);
        }
        let class = match key {
            Some(YellowKey::Index) => AssetClass::Index,
            Some(YellowKey::Govt) => AssetClass::Govt,
            Some(YellowKey::Comdty) => AssetClass::Cmdty,
            Some(YellowKey::Curncy) => {
                if ticker.len() == 6 && is_fiat(&ticker[..3]) && is_fiat(&ticker[3..]) {
                    AssetClass::Fx
                } else {
                    AssetClass::Crypto
                }
            }
            Some(YellowKey::Equity) => AssetClass::Equity,
            None if ticker.len() == 6 && is_fiat(&ticker[..3]) && is_fiat(&ticker[3..]) => AssetClass::Fx,
            None if ["USDT", "USDC", "FDUSD"].iter().any(|quote| ticker.ends_with(quote))
                && crate::crypto::split_pair(ticker).is_some() =>
            {
                AssetClass::Crypto
            }
            None => AssetClass::Equity,
        };
        Self::new(ticker, class)
    }

    /// Display name from the catalog, if known.
    pub fn name(&self) -> Option<&'static str> {
        catalog::lookup(&self.ticker, Some(self.class.yellow_key())).filter(|l| l.class == self.class).map(|l| l.name)
    }

    /// Base/quote split for currency pairs (`BTCUSD` -> `BTC`, `USD`).
    pub fn pair(&self) -> Option<(&str, &str)> {
        if !matches!(self.class, AssetClass::Fx | AssetClass::Crypto) {
            return None;
        }
        if self.class == AssetClass::Crypto && self.ticker.contains(':') {
            self.crypto_instrument().ok()?;
            let (venue, product) = self.ticker.split_once(':')?;
            let product = product.strip_suffix(".P").unwrap_or(product);
            return match venue {
                "COINBASE" => product.split_once('-'),
                "BINANCE" => crate::crypto::split_pair(product),
                _ => None,
            };
        }
        for q in ["USDT", "USDC", "USD", "EUR", "GBP", "JPY", "BTC", "ETH"] {
            if let Some(base) = self.ticker.strip_suffix(q).filter(|b| !b.is_empty()) {
                return Some((base, q));
            }
        }
        None
    }

    /// Price decimals appropriate for the instrument and level.
    pub fn decimals(&self, price: f64) -> usize {
        match self.class {
            AssetClass::Govt => 3,
            AssetClass::Fx if self.ticker.ends_with("JPY") => 3,
            AssetClass::Fx => 4,
            _ => match price.abs() {
                p if p >= 1000.0 => 2,
                p if p >= 1.0 => 2,
                p if p >= 0.01 => 4,
                _ => 6,
            },
        }
    }

    /// Whether prices are quoted in US dollars, so they can carry a `$`. Indices (points),
    /// yields and non-USD quotes are not; Canadian listings trade in CAD.
    pub fn dollar_priced(&self) -> bool {
        match self.class {
            AssetClass::Equity | AssetClass::Etf => {
                ![".TO", ".V", ".CN", ".NE"].iter().any(|suffix| self.ticker.ends_with(suffix))
            }
            AssetClass::Fx => self.ticker.ends_with("USD"),
            AssetClass::Crypto => self.crypto_instrument().is_ok_and(|c| c.quote == "USD"),
            AssetClass::Cmdty => true,
            AssetClass::Index | AssetClass::Govt => false,
        }
    }

    /// Whether congressional (STOCK Act) disclosures can reference this security.
    pub fn has_congress_trades(&self) -> bool {
        matches!(self.class, AssetClass::Equity | AssetClass::Etf)
    }
}

impl fmt::Display for Security {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.key())
    }
}

pub fn looks_like_ticker(s: &str) -> bool {
    let core = s.trim_start_matches('^');
    !core.is_empty()
        && core.len() <= 14
        && core.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '='))
        && core.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> Security {
        Security::parse(s).unwrap()
    }

    #[test]
    fn market_keys() {
        assert_eq!(r("BTCUSD <CRNCY>"), Security::new("BTCUSD", AssetClass::Crypto));
        assert_eq!(r("EURUSD <CRNCY>"), Security::new("EURUSD", AssetClass::Fx));
        assert_eq!(r("US10Y <GOVT>"), Security::new("US10Y", AssetClass::Govt));
        assert_eq!(r("CL1 <CMDTY>"), Security::new("CL1", AssetClass::Cmdty));
        assert_eq!(r("SPX <INDEX>"), Security::new("SPX", AssetClass::Index));
        assert_eq!(r("AAPL <EQUITY>"), Security::new("AAPL", AssetClass::Equity));
        assert_eq!(r("aapl us equity"), Security::new("AAPL", AssetClass::Equity));
        assert_eq!(r("CL1 Comdty").key(), "CL1 Comdty");
        assert_eq!(r("SPY").class, AssetClass::Etf);
    }

    #[test]
    fn bare_tickers_use_catalog_then_heuristics() {
        assert_eq!(r("BTCUSD").class, AssetClass::Crypto);
        assert_eq!(r("EURUSD").class, AssetClass::Fx);
        assert_eq!(r("AUDNZD").class, AssetClass::Fx);
        assert_eq!(r("SPX").class, AssetClass::Index);
        assert_eq!(r("US10Y").class, AssetClass::Govt);
        assert_eq!(r("ZZZQ").class, AssetClass::Equity);
        assert_eq!(r("PEPEUSD CRNCY").class, AssetClass::Crypto);
        assert_eq!(r("btc/usd").ticker, "BTCUSD");
        assert_eq!(r("brk/b").ticker, "BRK.B");
    }

    #[test]
    fn tokens_consumed() {
        let t = ["AAPL", "US", "EQUITY", "RSI"];
        assert_eq!(Security::resolve_tokens(&t).unwrap().1, 3);
        let t = ["AAPL", "RSI"];
        assert_eq!(Security::resolve_tokens(&t).unwrap().1, 1);
        let t = ["AAPL", "US", "CONG"];
        assert_eq!(Security::resolve_tokens(&t).unwrap().1, 1);
    }

    #[test]
    fn keys_and_helpers() {
        assert_eq!(Security::new("BTCUSD", AssetClass::Crypto).key(), "BTCUSD Curncy");
        assert_eq!(Security::new("US10Y", AssetClass::Govt).key(), "US10Y Govt");
        assert_eq!(Security::new("BTCUSDT", AssetClass::Crypto).pair(), Some(("BTC", "USDT")));
        assert_eq!(Security::new("USDJPY", AssetClass::Fx).decimals(150.0), 3);
        assert!(!Security::new("BTCUSD", AssetClass::Crypto).has_congress_trades());
        assert_eq!(r("BTCUSD Curncy").name(), Some("Bitcoin / US Dollar"));
    }

    #[test]
    fn qualified_crypto_round_trips_without_equity_fallback() {
        for ticker in ["BINANCE:BTCUSDT", "COINBASE:BTC-USDT", "BINANCE:BTCUSDT.P"] {
            let security = r(ticker);
            assert_eq!(security.class, AssetClass::Crypto);
            assert_eq!(r(&security.key()), security);
            assert_eq!(security.pair(), Some(("BTC", "USDT")));
            assert_eq!(Security::resolve_tokens(&[ticker, "CURNCY", "GP"]).unwrap().1, 2);
        }
        for input in [
            "OTHER:BTCUSD",
            "BINANCE:BTC-USDT",
            "COINBASE:BTCUSD",
            "BINANCE:BTCUSDT EQUITY",
            "BINANCE:BTCUSDT US Equity",
            "BINANCE:BTCUSDT CURNCY OB",
            "BINANCE:BTCUSDT unexpected",
        ] {
            assert!(Security::parse(input).is_none(), "{input}");
        }
        assert_eq!(Security::resolve_tokens(&["BINANCE:BTCUSDT", "OB"]).unwrap().1, 1);
        assert!(Security::resolve_tokens(&["BINANCE:BTCUSDT", "US", "Equity"]).is_none());
        assert!(Security::resolve_tokens(&["BINANCE:BTCUSDT", "Index"]).is_none());
    }

    #[test]
    fn legacy_security_persistence_is_unchanged() {
        let value = r("BTCUSD Curncy");
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(json, r#"{"ticker":"BTCUSD","class":"Crypto"}"#);
        assert_eq!(serde_json::from_str::<Security>(&json).unwrap(), value);
        assert_eq!(value.key(), "BTCUSD Curncy");
        assert_eq!(value.crypto_instrument().unwrap().canonical_ticker(), "COINBASE:BTC-USD");
        assert_eq!(r("BTCUSDT").crypto_instrument().unwrap().canonical_ticker(), "BINANCE:BTCUSDT");
    }

    #[test]
    fn dollar_priced_follows_the_quote_currency() {
        for usd in ["AAPL", "BRK.B US Equity", "EURUSD Curncy", "BTCUSD Curncy", "COINBASE:ETH-USD", "CL1 Comdty"] {
            assert!(r(usd).dollar_priced(), "{usd}");
        }
        for other in ["BMO.TO US Equity", "USDJPY Curncy", "BTCUSDT", "SPX Index", "US10Y Govt"] {
            assert!(!r(other).dollar_priced(), "{other}");
        }
    }
}
