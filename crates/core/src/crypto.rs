//! Explicit crypto identities. Legacy display keys retain their persisted form,
//! while resolution always chooses one exact venue, quote asset and market kind.

use serde::{Deserialize, Serialize};

use crate::{AssetClass, Security};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CryptoVenue {
    Binance,
    Coinbase,
}

impl CryptoVenue {
    pub fn label(self) -> &'static str {
        match self {
            Self::Binance => "BINANCE",
            Self::Coinbase => "COINBASE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CryptoMarketKind {
    Spot,
    Perpetual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CryptoInstrument {
    pub venue: CryptoVenue,
    pub native_symbol: String,
    pub base: String,
    pub quote: String,
    pub kind: CryptoMarketKind,
    #[serde(default)]
    pub settlement: Option<String>,
    /// Venue metadata only; absent until an authoritative catalog provides it.
    #[serde(default)]
    pub expiry: Option<i64>,
    #[serde(default)]
    pub contract_multiplier: Option<String>,
    #[serde(default)]
    pub price_increment: Option<String>,
    #[serde(default)]
    pub quantity_increment: Option<String>,
}

impl<'de> Deserialize<'de> for CryptoInstrument {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Input {
            venue: Option<CryptoVenue>,
            native_symbol: String,
            base: String,
            quote: String,
            kind: Option<CryptoMarketKind>,
            settlement: Option<String>,
            expiry: Option<i64>,
            contract_multiplier: Option<String>,
            price_increment: Option<String>,
            quantity_increment: Option<String>,
        }
        let input = Input::deserialize(deserializer)?;
        let instrument = Self {
            venue: input.venue.ok_or_else(|| serde::de::Error::custom("Missing crypto venue"))?,
            native_symbol: input.native_symbol,
            base: input.base,
            quote: input.quote,
            kind: input.kind.ok_or_else(|| serde::de::Error::custom("Missing crypto market kind"))?,
            settlement: input.settlement,
            expiry: input.expiry,
            contract_multiplier: input.contract_multiplier,
            price_increment: input.price_increment,
            quantity_increment: input.quantity_increment,
        };
        instrument.validate().map_err(serde::de::Error::custom)?;
        Ok(instrument)
    }
}

fn asset_valid(value: &str) -> bool {
    !value.is_empty() && value.len() <= 24 && value.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

impl CryptoInstrument {
    /// Parse canonical qualified tickers or legacy unqualified spot pairs.
    /// A legacy USD pair means Coinbase USD, never Binance USDT.
    pub fn parse(input: &str) -> Result<Self, String> {
        if input.is_empty()
            || input.len() > 80
            || input.bytes().any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return Err("Invalid crypto instrument key".into());
        }
        let upper = input.to_ascii_uppercase();
        let (venue, product, qualified) = if let Some((venue, product)) = upper.split_once(':') {
            let venue = match venue {
                "BINANCE" => CryptoVenue::Binance,
                "COINBASE" => CryptoVenue::Coinbase,
                _ => return Err("Unsupported crypto venue".into()),
            };
            (venue, product, true)
        } else {
            let venue = if split_pair(&upper).is_some_and(|(_, quote)| quote == "USD") {
                CryptoVenue::Coinbase
            } else {
                CryptoVenue::Binance
            };
            (venue, upper.as_str(), false)
        };
        let (product, kind) = match product.strip_suffix(".P") {
            Some(p) if qualified => (p, CryptoMarketKind::Perpetual),
            Some(_) => return Err("Derivative instruments require an explicit venue".into()),
            None => (product, CryptoMarketKind::Spot),
        };
        let (base, quote) = match venue {
            CryptoVenue::Coinbase if qualified => product.split_once('-').ok_or("Coinbase requires BASE-QUOTE")?,
            _ => split_pair(product).ok_or("Unsupported or malformed crypto pair")?,
        };
        let native_symbol = match venue {
            CryptoVenue::Binance => format!("{base}{quote}"),
            CryptoVenue::Coinbase => format!("{base}-{quote}"),
        };
        let instrument = Self {
            venue,
            native_symbol,
            base: base.into(),
            quote: quote.into(),
            kind,
            settlement: None,
            expiry: None,
            contract_multiplier: None,
            price_increment: None,
            quantity_increment: None,
        };
        instrument.validate()?;
        Ok(instrument)
    }

    pub fn from_security(security: &Security) -> Result<Self, String> {
        if security.class != AssetClass::Crypto {
            return Err("Security is not a crypto instrument".into());
        }
        Self::parse(&security.ticker)
    }

    /// Validate untrusted deserialized catalog metadata before using it.
    pub fn validate(&self) -> Result<(), String> {
        if !asset_valid(&self.base) || !asset_valid(&self.quote) || self.base == self.quote {
            return Err("Invalid crypto base or quote asset".into());
        }
        let expected = match self.venue {
            CryptoVenue::Binance => format!("{}{}", self.base, self.quote),
            CryptoVenue::Coinbase => format!("{}-{}", self.base, self.quote),
        };
        if self.native_symbol != expected || self.native_symbol.len() > 49 {
            return Err("Crypto product does not match base and quote".into());
        }
        if self.venue == CryptoVenue::Binance && split_pair(&self.native_symbol) != Some((&self.base, &self.quote)) {
            return Err("Unsupported or ambiguous Binance quote asset".into());
        }
        if self.settlement.as_ref().is_some_and(|s| !asset_valid(s)) {
            return Err("Invalid crypto settlement asset".into());
        }
        if self.kind == CryptoMarketKind::Spot
            && (self.settlement.is_some() || self.expiry.is_some() || self.contract_multiplier.is_some())
        {
            return Err("Spot instrument contains contract metadata".into());
        }
        if self.expiry.is_some() {
            return Err("Perpetual instruments cannot have an expiry".into());
        }
        for value in [&self.contract_multiplier, &self.price_increment, &self.quantity_increment].into_iter().flatten()
        {
            if value.len() > 40
                || !value.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                || !value.parse::<f64>().is_ok_and(|n| n.is_finite() && n > 0.0)
            {
                return Err("Invalid crypto precision or multiplier metadata".into());
            }
        }
        Ok(())
    }

    pub fn require_spot(&self) -> Result<(), String> {
        self.validate()?;
        if self.kind != CryptoMarketKind::Spot {
            return Err("Crypto derivatives are identified but not supported by the spot provider".into());
        }
        Ok(())
    }

    pub fn canonical_ticker(&self) -> String {
        format!(
            "{}:{}{}",
            self.venue.label(),
            self.native_symbol,
            if self.kind == CryptoMarketKind::Perpetual { ".P" } else { "" }
        )
    }

    pub fn security(&self) -> Security {
        Security::new(self.canonical_ticker(), AssetClass::Crypto)
    }
}

/// Known quote suffixes for compact pair notation. Discovery metadata must be
/// used for products whose quote cannot be inferred unambiguously.
pub fn split_pair(product: &str) -> Option<(&str, &str)> {
    [
        "FDUSD", "USDT", "USDC", "TUSD", "BUSD", "USD", "EUR", "GBP", "JPY", "AUD", "CAD", "TRY", "BRL", "BTC", "ETH",
        "BNB", "DAI",
    ]
    .into_iter()
    .find_map(|quote| {
        let base = product.strip_suffix(quote)?;
        (asset_valid(base) && base != quote).then_some((base, quote))
    })
}

/// Exact asset filters for a venue-owned crypto universe. Invalid filters fail
/// closed: sanitizing never removes invalid characters or truncates an asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CryptoUniverseFilter {
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub quote: String,
    #[serde(default = "default_universe_limit")]
    pub limit: usize,
    /// Zero-based offset in the deterministically ordered matching catalog.
    #[serde(default)]
    pub page: usize,
}

fn default_universe_limit() -> usize {
    200
}

impl Default for CryptoUniverseFilter {
    fn default() -> Self {
        Self { base: String::new(), quote: String::new(), limit: default_universe_limit(), page: 0 }
    }
}

impl CryptoUniverseFilter {
    pub const MAX_LIMIT: usize = 200;

    /// Normalize ordinary surrounding spaces and ASCII case. Preserve invalid
    /// values so validation can report them instead of widening the universe.
    pub fn sanitize(&mut self) {
        for asset in [&mut self.base, &mut self.quote] {
            if asset.len() <= 80 && !asset.chars().any(char::is_control) {
                *asset = asset.trim_matches(' ').to_ascii_uppercase();
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if !self.base.is_empty() && !asset_valid(&self.base) {
            return Err("Crypto base filter must be an ASCII asset code of at most 24 characters".into());
        }
        if !self.quote.is_empty() && !asset_valid(&self.quote) {
            return Err("Crypto quote filter must be an ASCII asset code of at most 24 characters".into());
        }
        if self.limit == 0 || self.limit > Self::MAX_LIMIT {
            return Err(format!("Crypto universe limit must be between 1 and {}", Self::MAX_LIMIT));
        }
        if self.page.checked_mul(self.limit).is_none_or(|offset| offset >= 30_000) {
            return Err("Crypto universe page is outside the supported catalog range".into());
        }
        Ok(())
    }

    pub fn matches(&self, instrument: &CryptoInstrument) -> bool {
        self.validate().is_ok()
            && instrument.require_spot().is_ok()
            && (self.base.is_empty() || self.base == instrument.base)
            && (self.quote.is_empty() || self.quote == instrument.quote)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn universe_filters_default_and_normalize_without_changing_assets() {
        let filter: CryptoUniverseFilter = serde_json::from_str("{}").unwrap();
        assert_eq!(filter, CryptoUniverseFilter::default());
        assert_eq!(filter.limit, 200);
        let mut filter = CryptoUniverseFilter { base: " btc ".into(), quote: " usd ".into(), limit: 10, page: 0 };
        filter.sanitize();
        assert_eq!(filter.base, "BTC");
        assert_eq!(filter.quote, "USD");
        assert!(filter.validate().is_ok());
        assert!(filter.matches(&CryptoInstrument::parse("COINBASE:BTC-USD").unwrap()));
        assert!(!filter.matches(&CryptoInstrument::parse("BINANCE:BTCUSDT").unwrap()));
        assert!(!filter.matches(&CryptoInstrument::parse("COINBASE:ETH-USD").unwrap()));
        let filter = CryptoUniverseFilter::default();
        assert!(!filter.matches(&CryptoInstrument::parse("BINANCE:BTCUSDT.P").unwrap()));
    }

    #[test]
    fn invalid_universe_filters_fail_closed_after_sanitizing() {
        let instrument = CryptoInstrument::parse("COINBASE:BTC-USD").unwrap();
        for value in ["A".repeat(25), " ".repeat(100_000), "BTC/USD".into(), "\n".into(), "B TC".into(), "é".into()] {
            let mut filter = CryptoUniverseFilter { base: value.clone(), ..Default::default() };
            filter.sanitize();
            assert!(filter.validate().is_err(), "{value:?}");
            assert!(!filter.matches(&instrument));
            let mut filter = CryptoUniverseFilter { quote: value, ..Default::default() };
            filter.sanitize();
            assert!(filter.validate().is_err());
            assert!(!filter.matches(&instrument));
        }
        for limit in [0, 201, usize::MAX] {
            let mut filter = CryptoUniverseFilter { limit, ..Default::default() };
            filter.sanitize();
            assert!(filter.validate().is_err());
            assert!(!filter.matches(&instrument));
        }
        for (page, limit, valid) in
            [(149, 200, true), (150, 200, false), (29_999, 1, true), (30_000, 1, false), (usize::MAX, 200, false)]
        {
            let filter = CryptoUniverseFilter { page, limit, ..Default::default() };
            assert_eq!(filter.validate().is_ok(), valid);
        }
    }

    #[test]
    fn legacy_pairs_resolve_exact_quote_assets() {
        let usd = CryptoInstrument::parse("BTCUSD").unwrap();
        let usdt = CryptoInstrument::parse("BTCUSDT").unwrap();
        assert_eq!(usd.canonical_ticker(), "COINBASE:BTC-USD");
        assert_eq!(usdt.canonical_ticker(), "BINANCE:BTCUSDT");
        assert_ne!(usd.quote, usdt.quote);
    }

    #[test]
    fn qualified_keys_preserve_venue_and_kind() {
        for input in ["BINANCE:BTCUSDT", "COINBASE:BTC-USDT", "BINANCE:BTCUSDT.P"] {
            let instrument = CryptoInstrument::parse(input).unwrap();
            assert_eq!(instrument.canonical_ticker(), input);
            assert_eq!(CryptoInstrument::from_security(&instrument.security()).unwrap(), instrument);
        }
        assert!(CryptoInstrument::parse("BINANCE:BTCUSDT.P").unwrap().require_spot().is_err());
    }

    #[test]
    fn malformed_and_untrusted_metadata_are_rejected() {
        for input in [
            "OTHER:BTCUSD",
            "BINANCE:BTC-USDT",
            "COINBASE:BTCUSD",
            "BTCUSD.P",
            "BINANCE:BTCUSDT.F",
            "BINANCE:BTCUSDT\n",
            "BINANCE:BTC:USD",
            "COINBASE:BTC--USD",
            "BINANCE:BTCBTC",
        ] {
            assert!(CryptoInstrument::parse(input).is_err(), "{input}");
        }
        let mut instrument = CryptoInstrument::parse("COINBASE:ETH-USD").unwrap();
        instrument.price_increment = Some("NaN".into());
        assert!(instrument.validate().is_err());
    }

    #[test]
    fn untrusted_persisted_identity_is_validated() {
        assert!(serde_json::from_str::<CryptoInstrument>("{}").is_err());
        let valid = CryptoInstrument::parse("BINANCE:ETHUSDT").unwrap();
        let mut json = serde_json::to_value(&valid).unwrap();
        assert_eq!(serde_json::from_value::<CryptoInstrument>(json.clone()).unwrap(), valid);
        json["native_symbol"] = serde_json::json!("BTCUSDT");
        assert!(serde_json::from_value::<CryptoInstrument>(json).is_err());
        let mut unknown_quote = valid;
        unknown_quote.quote = "UNRECOGNIZED".into();
        unknown_quote.native_symbol = "ETHUNRECOGNIZED".into();
        assert!(unknown_quote.validate().is_err());
    }
}
