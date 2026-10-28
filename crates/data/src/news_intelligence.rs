//! Deterministic, bounded story intelligence. No I/O or provider calls.
#[cfg(test)]
#[path = "news_intelligence_tests.rs"]
mod tests;
use crate::news_store::Story;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_ITEMS: usize = 2_000;

/// Bump whenever subject semantics change; stored rows are lazily re-derived.
pub const MATCHER_VERSION: &str = "n1b-subjects-3";

/// Pure token/phrase index. Construction is worker-only; matching never scans
/// company names. Share-class selection is independent of SEC row ordering.
#[derive(Default)]
pub struct NameIndex {
    names: std::collections::HashMap<String, Name>,
    lengths: std::collections::HashMap<String, Vec<usize>>,
    tickers: std::collections::HashMap<String, String>,
    bare: std::collections::HashSet<String>,
}
#[derive(Clone, Copy)]
enum Evidence {
    Sec,
    Strong,
    Weak,
    Crypto,
}
struct Name {
    key: String,
    evidence: Evidence,
}
fn common_words() -> &'static std::collections::HashSet<&'static str> {
    static WORDS: std::sync::LazyLock<std::collections::HashSet<&str>> = std::sync::LazyLock::new(|| {
        include_str!("common_name_words.txt")
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.starts_with('#'))
            .collect()
    });
    &WORDS
}
fn distinctive(name: &str) -> bool {
    name.split_whitespace()
        .any(|s| s.chars().count() >= 4 && !s.chars().all(|c| c.is_numeric()) && !common_words().contains(s))
}
fn market_word(word: &str) -> bool {
    matches!(
        word,
        "nasdaq"
            | "nyse"
            | "dow"
            | "russell"
            | "nikkei"
            | "ftse"
            | "dax"
            | "cboe"
            | "lse"
            | "xetra"
            | "tsx"
            | "asx"
            | "euronext"
            | "exchange"
    )
}
fn equity_key(ticker: &str) -> String {
    let ticker = ticker.replace('-', ".");
    tr_core::catalog::lookup(&ticker, None).map(|l| l.key()).unwrap_or_else(|| format!("{ticker} US Equity"))
}
fn common_ticker(ticker: &str, title: &str) -> bool {
    let class = ticker.rsplit_once(['-', '.']);
    let plain = class.map_or(ticker, |(base, _)| base);
    !title.to_ascii_lowercase().contains("preferred")
        && plain.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && class.is_none_or(|(_, suffix)| matches!(suffix, "A" | "B" | "C"))
        && !(plain.len() == 5 && plain.ends_with(['P', 'O', 'W', 'R', 'U', 'Z']))
}

/// Explicit app keys, not inferred from ambiguous SEC names. `|` separates aliases.
/// Weak aliases are never promoted by possessives or title case alone.
const FAMOUS: &[(&str, &str, bool)] = &[
    ("Apple", "AAPL", false),
    ("Amazon|Amazon.com", "AMZN", false),
    ("Alphabet|Google", "GOOGL", false),
    ("Meta|Meta Platforms|Facebook", "META", false),
    ("Oracle", "ORCL", false),
    ("Netflix", "NFLX", false),
    ("Microsoft", "MSFT", false),
    ("Nvidia", "NVDA", false),
    ("Intel", "INTC", false),
    ("Cisco|Cisco Systems", "CSCO", false),
    ("Dell|Dell Technologies", "DELL", false),
    ("Verizon|Verizon Communications", "VZ", false),
    ("AT&T", "T", false),
    ("T-Mobile|T-Mobile US", "TMUS", false),
    ("Wells Fargo", "WFC", false),
    ("JPMorgan|J.P. Morgan|JPMorgan Chase", "JPM", false),
    ("Berkshire|Berkshire Hathaway", "BRK.B", false),
    ("Goldman Sachs", "GS", false),
    ("Morgan Stanley", "MS", false),
    ("Bank of America", "BAC", false),
    ("Citigroup|Citi", "C", false),
    ("Salesforce", "CRM", false),
    ("Adobe", "ADBE", false),
    ("IBM", "IBM", false),
    ("Walmart", "WMT", false),
    ("Costco", "COST", false),
    ("Disney", "DIS", false),
    ("Boeing", "BA", false),
    ("Nike", "NKE", false),
    ("Starbucks", "SBUX", false),
    ("McDonald's", "MCD", false),
    ("Coca-Cola", "KO", false),
    ("PepsiCo", "PEP", false),
    ("Pfizer", "PFE", false),
    ("Moderna", "MRNA", false),
    ("Eli Lilly|Lilly", "LLY", false),
    ("Novo Nordisk", "NVO", false),
    ("Exxon|ExxonMobil|Exxon Mobil", "XOM", false),
    ("Chevron", "CVX", false),
    ("Palantir|Palantir Technologies", "PLTR", false),
    ("AMD|Advanced Micro Devices", "AMD", false),
    ("Broadcom", "AVGO", false),
    ("Qualcomm", "QCOM", false),
    ("Micron", "MU", false),
    ("Arm|Arm Holdings", "ARM", true),
    ("Super Micro|Supermicro", "SMCI", false),
    ("Marvell", "MRVL", false),
    ("TSMC|Taiwan Semiconductor", "TSM", false),
    ("Uber", "UBER", false),
    ("Airbnb", "ABNB", false),
    ("Shopify", "SHOP", false),
    ("Spotify", "SPOT", false),
    ("PayPal", "PYPL", false),
    ("Block", "XYZ", true),
    ("Coinbase|Coinbase Global", "COIN", false),
    ("Robinhood", "HOOD", false),
    ("MicroStrategy", "MSTR", false),
    ("Strategy", "MSTR", true),
    ("Tesla", "TSLA", false),
    ("Ford|Ford Motor", "F", true),
    ("GM|General Motors", "GM", false),
    ("Rivian", "RIVN", false),
    ("Lucid", "LCID", true),
    ("Visa", "V", true),
    ("Mastercard", "MA", false),
    ("American Express|Amex", "AXP", false),
    ("Target", "TGT", true),
    ("Gap", "GAP", true),
    ("Shell", "SHEL", true),
    ("BP", "BP", false),
    ("Home Depot", "HD", false),
    ("Lowe's", "LOW", false),
    ("UnitedHealth", "UNH", false),
    ("CVS", "CVS", false),
    ("Johnson & Johnson|J&J", "JNJ", false),
    ("Merck", "MRK", false),
    ("AbbVie", "ABBV", false),
    ("Amgen", "AMGN", false),
    ("Gilead", "GILD", false),
    ("Snowflake", "SNOW", false),
    ("CrowdStrike", "CRWD", false),
    ("Datadog", "DDOG", false),
    ("ServiceNow", "NOW", false),
    ("Workday", "WDAY", false),
    ("Intuit", "INTU", false),
    ("Zoom", "ZM", false),
    ("Dropbox", "DBX", false),
    ("Pinterest", "PINS", false),
    ("Snap", "SNAP", true),
    ("Reddit", "RDDT", false),
    ("Roblox", "RBLX", false),
    ("Unity", "U", true),
    ("Sony", "SONY", false),
    ("Alibaba", "BABA", false),
    ("Baidu", "BIDU", false),
    ("JD.com", "JD", false),
    ("PDD|Temu", "PDD", false),
    ("Toyota", "TM", false),
    ("Honda", "HMC", false),
    ("SoFi", "SOFI", false),
    ("Nu|Nu Holdings", "NU", true),
    ("Carvana", "CVNA", false),
    ("Chewy", "CHWY", false),
    ("Etsy", "ETSY", false),
    ("eBay", "EBAY", false),
    ("Zillow", "Z", false),
    ("Expedia", "EXPE", false),
    ("Booking", "BKNG", false),
    ("Marriott", "MAR", false),
    ("Hilton", "HLT", false),
    ("Delta", "DAL", true),
    ("United Airlines", "UAL", false),
    ("American Airlines", "AAL", false),
    ("Southwest", "LUV", true),
    ("JetBlue", "JBLU", false),
    ("FedEx", "FDX", false),
    ("UPS", "UPS", false),
    ("Caterpillar", "CAT", false),
    ("Deere|John Deere", "DE", false),
    ("Lockheed Martin", "LMT", false),
    ("Northrop|Northrop Grumman", "NOC", false),
    ("Raytheon|RTX", "RTX", false),
    ("General Electric|GE", "GE", false),
    ("GE Vernova", "GEV", false),
    ("Honeywell", "HON", false),
    ("3M", "MMM", false),
    ("Dow Inc", "DOW", false),
    ("Bloom Energy", "BE", false),
    ("Constellation Energy", "CEG", false),
    ("Vistra", "VST", false),
    ("NextEra|NextEra Energy", "NEE", false),
    ("Duke Energy", "DUK", false),
    ("Southern Company", "SO", true),
    ("Rocket Lab", "RKLB", false),
    ("IonQ", "IONQ", false),
    ("Rigetti", "RGTI", false),
    ("D-Wave", "QBTS", false),
    ("SoundHound", "SOUN", false),
    ("BigBear.ai", "BBAI", false),
    ("CoreWeave", "CRWV", false),
    ("Nebius", "NBIS", false),
    ("IREN", "IREN", false),
    ("Oklo", "OKLO", false),
    ("NuScale", "SMR", false),
    ("Cameco", "CCJ", false),
    ("Centrus", "LEU", false),
    ("Strive", "ASST", true),
    ("AstraZeneca", "AZN", false),
    ("First Solar", "FSLR", false),
    ("First Majestic Silver", "AG", false),
];
/// Removing trailing legal/share-class decorations is deliberately iterative.
pub fn company_name(title: &str) -> String {
    // SEC uses slash-delimited state/country decorations, with or without a closing slash.
    let title = title.split('/').next().unwrap_or(title);
    let mut tokens = words(title);
    loop {
        let n = tokens.len();
        if n >= 2
            && (tokens[n - 2] == "class" && matches!(tokens[n - 1].as_str(), "a" | "b" | "c")
                || matches!((tokens[n - 2].as_str(), tokens[n - 1].as_str()), ("n", "v") | ("s", "a") | ("l", "p")))
        {
            tokens.truncate(n - 2);
        } else if tokens.last().is_some_and(|s| {
            matches!(
                s.as_str(),
                "inc"
                    | "incorporated"
                    | "corp"
                    | "corporation"
                    | "co"
                    | "company"
                    | "ltd"
                    | "limited"
                    | "plc"
                    | "holdings"
                    | "holding"
                    | "group"
                    | "nv"
                    | "sa"
                    | "ag"
                    | "se"
                    | "de"
                    | "com"
                    | "lp"
                    | "llc"
            )
        }) {
            tokens.pop();
        } else {
            break;
        }
    }
    tokens.join(" ")
}
fn text_spans(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut out = vec![];
    let mut start = None;
    for (i, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        if c.is_alphanumeric() {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            out.push((s..i, text[s..i].to_lowercase()));
        }
    }
    out
}
impl NameIndex {
    pub fn new(companies: &[(u64, String, String)]) -> Self {
        let mut index = Self::default();
        let mut primary: std::collections::HashMap<u64, &str> = std::collections::HashMap::new();
        let priority = |ticker: &str| {
            let key = equity_key(ticker);
            (!tr_core::catalog::CATALOG.iter().any(|l| l.key() == key), ticker.len(), ticker.to_owned())
        };
        for (cik, ticker, title) in companies {
            if common_ticker(ticker, title) {
                let candidate = primary.entry(*cik).or_insert(ticker);
                if priority(ticker) < priority(candidate) {
                    *candidate = ticker;
                }
            }
        }
        for (cik, ticker, title) in companies {
            index.tickers.insert(ticker.replace('-', "."), equity_key(ticker));
            if let Some(ticker) = primary.get(cik) {
                let alias = company_name(title);
                if distinctive(&alias)
                    && !alias.split_whitespace().any(market_word)
                    && alias.split_whitespace().next() != Some("samsung")
                {
                    index.add(&alias, &equity_key(ticker), Evidence::Sec);
                }
                if alias == "space exploration technologies" {
                    index.add("spacex", &equity_key(ticker), Evidence::Strong);
                }
            }
        }
        for listing in tr_core::catalog::CATALOG.iter() {
            if matches!(listing.class, tr_core::security::AssetClass::Equity | tr_core::security::AssetClass::Etf) {
                index.tickers.insert(listing.ticker.into(), listing.key());
                index.bare.insert(listing.ticker.into());
            }
        }
        for &(aliases, ticker, weak) in FAMOUS {
            let key = equity_key(ticker);
            index.tickers.insert(ticker.into(), key.clone());
            index.bare.insert(ticker.into());
            for alias in aliases.split('|') {
                index.add(&words(alias).join(" "), &key, if weak { Evidence::Weak } else { Evidence::Strong });
            }
        }
        for (aliases, ticker) in [
            ("bitcoin btc", "BTCUSD"),
            ("ethereum ether eth", "ETHUSD"),
            ("solana sol", "SOLUSD"),
            ("xrp ripple", "XRPUSD"),
            ("dogecoin doge", "DOGEUSD"),
            ("cardano ada", "ADAUSD"),
        ] {
            let key = tr_core::catalog::lookup(ticker, None).expect("catalog crypto").key();
            index.tickers.insert(ticker.into(), key.clone());
            index.bare.insert(ticker.into());
            for alias in aliases.split_whitespace() {
                index.add(alias, &key, Evidence::Crypto);
                // Short crypto tickers still count as explicit scope evidence. DOGE is
                // context-checked in the explicit pass as well as the name pass.
                if matches!(alias, "btc" | "eth" | "sol" | "xrp" | "ada" | "doge") {
                    index.bare.insert(alias.to_ascii_uppercase());
                } else {
                    index.bare.remove(&alias.to_ascii_uppercase());
                }
                index.tickers.insert(alias.to_ascii_uppercase(), key.clone());
            }
        }
        index
    }
    fn add(&mut self, alias: &str, key: &str, evidence: Evidence) {
        if alias.is_empty() {
            return;
        }
        self.names.insert(alias.into(), Name { key: key.into(), evidence });
        let mut tokens = alias.split_whitespace();
        let first = tokens.next().expect("nonempty alias");
        let len = 1 + tokens.count();
        let lengths = self.lengths.entry(first.into()).or_default();
        if !lengths.contains(&len) {
            lengths.push(len);
            lengths.sort_unstable_by(|a, b| b.cmp(a));
        }
    }
    /// Headline appearance order, then summary appearance order, deduplicated.
    /// Associations disambiguate a name actually present; watched keys only admit bare tickers.
    pub fn subjects(&self, headline: &str, summary: &str, provider: &[String]) -> Vec<String> {
        self.subjects_watched(headline, summary, provider, &[])
    }
    pub fn subjects_watched(
        &self,
        headline: &str,
        summary: &str,
        provider: &[String],
        watched: &[String],
    ) -> Vec<String> {
        self.match_subjects(headline, summary, provider, watched, true)
    }
    fn match_subjects(
        &self,
        headline: &str,
        summary: &str,
        provider: &[String],
        watched: &[String],
        by_name: bool,
    ) -> Vec<String> {
        let decoded = [decode_entities(headline), decode_entities(summary)];
        let texts = decoded.each_ref().map(String::as_str);
        let spans = texts.map(text_spans);
        let extra: std::collections::HashMap<_, _> = provider
            .iter()
            .chain(watched)
            .filter_map(|key| key.split_whitespace().next().map(|ticker| (ticker.to_owned(), key.clone())))
            .collect();
        // A cashtag is explicit evidence below. An unrelated dollar amount (e.g.
        // "DOGE cuts $10 billion") is not crypto context.
        let crypto = spans.iter().flatten().any(|(_, w)| {
            matches!(
                w.as_str(),
                "price"
                    | "prices"
                    | "coin"
                    | "coins"
                    | "crypto"
                    | "cryptocurrency"
                    | "bitcoin"
                    | "ethereum"
                    | "blockchain"
                    | "token"
                    | "tokens"
                    | "xrp"
                    | "dogecoin"
                    | "solana"
                    | "etf"
            )
        });
        let mut explicit = std::collections::HashMap::new();
        for (part, tokens) in spans.iter().enumerate() {
            let text = texts[part];
            let letters: Vec<_> = text.chars().filter(|c| c.is_alphabetic()).collect();
            let shouting =
                !letters.is_empty() && letters.iter().filter(|c| c.is_uppercase()).count() * 100 >= letters.len() * 80;
            for (i, (range, word)) in tokens.iter().enumerate() {
                if market_word(word) || excluded_form(text, tokens, i, 1) || dateline(text, range.start) {
                    continue;
                }
                let mut end = range.end;
                let mut len = 1;
                if let Some((next, _)) = tokens.get(i + 1)
                    && matches!(&text[end..next.start], "." | "-")
                    && next.end - next.start == 1
                {
                    end = next.end;
                    len = 2;
                }
                let raw = &text[range.start..end];
                let upper = raw.to_ascii_uppercase().replace('-', ".");
                let before = &text[..range.start];
                let cash = before.ends_with('$');
                let exchange = before.ends_with(':') && i > 0 && market_word(&tokens[i - 1].1);
                let parenthetical = before.ends_with('(') && text[end..].starts_with(')');
                let preceding_sec = parenthetical
                    && (i.saturating_sub(8)..i).any(|start| {
                        let phrase = tokens[start..i].iter().map(|t| t.1.as_str()).collect::<Vec<_>>().join(" ");
                        self.names.get(&phrase).is_some_and(|name| {
                            matches!(name.evidence, Evidence::Sec) && self.tickers.get(&upper) == Some(&name.key)
                        })
                    });
                let paren = parenthetical
                    && i > 0
                    && (text[tokens[i - 1].0.clone()].chars().any(char::is_uppercase) || preceding_sec);
                let bare =
                    (self.bare.contains(&upper) || extra.contains_key(&upper)) && bare_ticker_allowed(&upper, shouting);
                let crypto_alias = self.names.get(word).is_some_and(|n| matches!(n.evidence, Evidence::Crypto));
                if (cash
                    || exchange
                    || raw == raw.to_ascii_uppercase()
                        && (bare || paren && (upper.len() == 1 || bare_ticker_allowed(&upper, false))))
                    && (!crypto_alias || cash && word != "ether" || crypto_alias_allowed(word, raw, crypto || cash))
                    && let Some(key) = self.tickers.get(&upper).or_else(|| extra.get(&upper))
                {
                    explicit.insert((part, i), (key.clone(), len));
                }
            }
        }
        let mut result = vec![];
        for (part, tokens) in spans.iter().enumerate() {
            let text = texts[part];
            let mut i = 0;
            while i < tokens.len() {
                let mut found = explicit.get(&(part, i)).map(|(key, len)| (key.as_str(), *len));
                if by_name
                    && !dateline(text, tokens[i].0.start)
                    && let Some(lengths) = self.lengths.get(&tokens[i].1)
                {
                    for &len in lengths {
                        if i + len > tokens.len() || excluded_form(text, tokens, i, len) {
                            continue;
                        }
                        let phrase = tokens[i..i + len].iter().map(|(_, w)| w.as_str()).collect::<Vec<_>>().join(" ");
                        let Some(name) = self.names.get(&phrase) else { continue };
                        let raw = &text[tokens[i].0.start..tokens[i + len - 1].0.end];
                        // Punctuation-sensitive brand spelling must not collapse to English words.
                        if phrase == "at t" && raw != "AT&T" {
                            continue;
                        }
                        let capitalised = raw.chars().any(char::is_uppercase);
                        let context = stock_context(text, tokens, i, len)
                            || provider.contains(&name.key)
                            || explicit.values().any(|(key, _)| key == &name.key);
                        let allowed = match name.evidence {
                            Evidence::Sec => true,
                            Evidence::Strong => capitalised,
                            Evidence::Weak => capitalised && context,
                            Evidence::Crypto => crypto_alias_allowed(&phrase, raw, crypto),
                        };
                        if allowed {
                            found = Some((&name.key, len));
                            break;
                        }
                    }
                }
                if let Some((key, len)) = found {
                    if !result.iter().any(|s| s == key) {
                        result.push(key.to_owned());
                    }
                    i += len;
                } else {
                    i += 1;
                }
            }
        }
        result
    }
    /// Scope membership includes explicit symbols, never inferred company-name subjects.
    pub fn explicit_tickers(&self, text: &str, associated: &[String], watched: &[String]) -> Vec<String> {
        let mut result = associated.to_vec();
        result.extend(self.match_subjects(text, "", associated, watched, false));
        result.sort();
        result.dedup();
        result
    }
}

type TextSpan = (std::ops::Range<usize>, String);
fn stock_context(text: &str, tokens: &[TextSpan], i: usize, len: usize) -> bool {
    let quarter = |word: &str| matches!(word, "q1" | "q2" | "q3" | "q4");
    let context = |word: &str| {
        matches!(
            word,
            "stock"
                | "shares"
                | "inc"
                | "incorporated"
                | "corp"
                | "corporation"
                | "earnings"
                | "ceo"
                | "investors"
                | "rating"
                | "upgrade"
                | "upgrades"
                | "downgrade"
                | "downgrades"
        )
    };
    let before = i.checked_sub(1).map(|j| tokens[j].1.as_str()).unwrap_or("");
    let after = tokens.get(i + len).map(|t| t.1.as_str()).unwrap_or("");
    // "Quality Strategy Q3 2026 Portfolio Review": a quarter after the tail of a longer
    // capitalised title names a fund report, not the company.
    let inside_title = i.checked_sub(1).is_some_and(|j| text[tokens[j].0.clone()].starts_with(char::is_uppercase));
    context(before)
        || context(after)
        || quarter(after) && !inside_title
        || (after == "price" && tokens.get(i + len + 1).is_some_and(|t| t.1 == "target"))
        || (before == "target" && i >= 2 && tokens[i - 2].1 == "price")
}
fn crypto_alias_allowed(alias: &str, raw: &str, context: bool) -> bool {
    match alias {
        "doge" | "ripple" => context,
        "ether" => raw == "Ether" && context,
        "sol" | "ada" | "btc" | "eth" | "xrp" => raw == raw.to_ascii_uppercase(),
        _ => true,
    }
}
fn excluded_form(text: &str, tokens: &[TextSpan], i: usize, len: usize) -> bool {
    let word = tokens[i].1.as_str();
    let after = &text[tokens[i + len - 1].0.end..];
    let next = tokens.get(i + len).map(|t| t.1.as_str()).unwrap_or("");
    // Do not reinterpret an exchange prefix, or the protected non-company meanings.
    after.starts_with(':') && market_word(word)
        || word == "meta" && (text[tokens[i].0.end..].starts_with('-') || next == "analysis")
        || word == "apple" && next == "pie"
        || word == "amazon" && next == "rainforest"
        || word == "oracle" && next == "of" && tokens.get(i + len + 1).is_some_and(|t| t.1 == "omaha")
        || word == "dow" && tokens.get(i + 1).is_none_or(|t| t.1 != "inc")
        || word == "hang" && next == "seng"
        || i > 0 && matches!((tokens[i - 1].1.as_str(), word), ("hang", "seng") | ("dow", "jones"))
}
fn dateline(text: &str, pos: usize) -> bool {
    let Some(comma) = text.find(',') else { return false };
    if pos > comma {
        return false;
    }
    let prefix = &text[..comma];
    let date = text[comma + 1..].trim_start().to_ascii_lowercase();
    prefix.chars().any(char::is_uppercase)
        && !prefix.chars().any(char::is_lowercase)
        && ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"]
            .iter()
            .any(|month| date.starts_with(month))
}
/// Decode numeric and common HTML named entities before tokenisation, without I/O.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        if let Some(end) = rest.find(';').filter(|end| *end <= 16) {
            let entity = &rest[1..end];
            let decoded = match entity {
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "rsquo" => Some('’'),
                "lsquo" => Some('‘'),
                "rdquo" => Some('”'),
                "ldquo" => Some('“'),
                "nbsp" => Some(' '),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "ndash" => Some('–'),
                "mdash" => Some('—'),
                "hellip" => Some('…'),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .and_then(|s| u32::from_str_radix(s, 16).ok())
                    .and_then(char::from_u32)
                    .or_else(|| entity.strip_prefix('#').and_then(|s| s.parse().ok()).and_then(char::from_u32)),
            };
            if let Some(c) = decoded {
                out.push(c);
                rest = &rest[end + 1..];
                continue;
            }
        }
        out.push('&');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

pub fn default_names() -> &'static NameIndex {
    static NAMES: std::sync::LazyLock<NameIndex> = std::sync::LazyLock::new(|| NameIndex::new(&[]));
    &NAMES
}
pub fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|s| !s.is_empty()).map(str::to_lowercase).collect()
}
pub fn headline_tokens(text: &str) -> BTreeSet<String> {
    words(text)
        .into_iter()
        .filter(|s| {
            !matches!(
                s.as_str(),
                "a" | "an"
                    | "the"
                    | "and"
                    | "or"
                    | "of"
                    | "to"
                    | "in"
                    | "on"
                    | "for"
                    | "with"
                    | "as"
                    | "is"
                    | "at"
                    | "by"
                    | "from"
                    | "us"
                    | "equity"
                    | "ln"
                    | "hk"
                    | "cn"
                    | "jp"
                    | "curncy"
            )
        })
        .collect()
}
pub fn similar(a: &BTreeSet<String>, b: &BTreeSet<String>, same_category: bool) -> bool {
    let intersection = a.intersection(b).count();
    let union = a.len() + b.len() - intersection;
    union > 0 && intersection * 100 >= union * if same_category { 20 } else { 25 }
}
fn rank(source: &str) -> usize {
    [
        "Reuters",
        "Bloomberg",
        "Associated Press",
        "AP",
        "Financial Times",
        "The Wall Street Journal",
        "CNBC",
        "SEC",
        "Finnhub",
        "Yahoo",
        "CoinDesk",
    ]
    .iter()
    .position(|s| s.eq_ignore_ascii_case(source))
    .unwrap_or(100)
}
#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    pub lead: Story,
    pub member_ids: Vec<String>,
    pub members: Vec<Story>,
    pub sources: Vec<String>,
    pub tickers: Vec<String>,
    pub subjects: Vec<String>,
    pub tags: Vec<String>,
    pub newest: i64,
}
impl Cluster {
    pub fn wire(story: &Story) -> Self {
        Self {
            lead: story.clone(),
            member_ids: vec![story.id.clone()],
            members: vec![story.clone()],
            sources: vec![story.source.clone()],
            tickers: story.item.tickers.clone(),
            subjects: story.subjects.clone(),
            tags: story.tags.clone(),
            newest: story.item.ts,
        }
    }
    pub fn unread(&self) -> bool {
        self.members.iter().any(|s| !s.seen)
    }
}
/// Match saved delivery identity with the same time, headline and subject rules
/// as clustering, even when an old member is outside the bounded pane snapshot.
pub fn same_cluster(a: &Story, b: &Story) -> bool {
    if a.item.ts.abs_diff(b.item.ts) > 86400 {
        return false;
    }
    let aa = a.known_tickers();
    let bb = b.known_tickers();
    (aa.is_empty() || bb.is_empty() || aa.iter().any(|s| bb.contains(s)))
        && similar(&headline_tokens(&a.item.title), &headline_tokens(&b.item.title), a.category == b.category)
}

/// Index only cluster representatives, not all historical item pairs. A candidate
/// must share a token and fit the entire cluster's 24-hour span. Input order never
/// affects leads or IDs; ties are broken by item ID.
pub fn clusters(items: &[Story]) -> Vec<Cluster> {
    cluster_stats(items).0
}
pub fn cluster_stats(items: &[Story]) -> (Vec<Cluster>, usize) {
    let mut sorted: Vec<_> = items.iter().collect();
    sorted.sort_by(|a, b| b.item.ts.cmp(&a.item.ts).then(a.id.cmp(&b.id)));
    sorted.truncate(MAX_ITEMS);
    sorted.sort_by(|a, b| a.item.ts.cmp(&b.item.ts).then(a.id.cmp(&b.id)));
    let mut out: Vec<Cluster> = vec![];
    let mut tokens: Vec<BTreeSet<String>> = vec![];
    let mut index: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut comparisons = 0;
    for item in sorted {
        let set = headline_tokens(&item.item.title);
        let candidates: BTreeSet<_> = set
            .iter()
            .filter_map(|t| index.get(t))
            .flat_map(|ids| {
                // Representatives are chronological: skip expired candidates without
                // scanning pairs across the retained 90-day history.
                let first = ids.partition_point(|&i| item.item.ts - out[i].members[0].item.ts > 86400);
                ids[first..].iter()
            })
            .copied()
            .collect();
        let found = candidates.into_iter().find(|&i| {
            if item.item.ts - out[i].members[0].item.ts > 86400 {
                return false;
            }
            // Compare every member, not just the union: a ticker-less bridge must
            // not join two companies with disjoint, known ticker sets.
            let subjects = item.known_tickers();
            if !subjects.is_empty()
                && out[i].members.iter().any(|member| {
                    let other = member.known_tickers();
                    !other.is_empty() && !other.iter().any(|t| subjects.contains(t))
                })
            {
                return false;
            }
            comparisons += 1;
            similar(&set, &tokens[i], item.category == out[i].members[0].category)
        });
        if let Some(i) = found {
            let c = &mut out[i];
            if (rank(&item.source), item.item.ts, &item.id) < (rank(&c.lead.source), c.lead.item.ts, &c.lead.id) {
                c.lead = item.clone();
            }
            c.member_ids.push(item.id.clone());
            c.members.push(item.clone());
            c.sources.push(item.source.clone());
            c.sources.sort();
            c.sources.dedup();
            for subject in &item.subjects {
                if !c.subjects.contains(subject) {
                    c.subjects.push(subject.clone());
                }
            }
            // The displayed headline's subjects always lead the cluster union.
            let mut subjects = c.lead.subjects.clone();
            for subject in &c.subjects {
                if !subjects.contains(subject) {
                    subjects.push(subject.clone());
                }
            }
            c.subjects = subjects;
            c.tickers.extend(item.item.tickers.clone());
            c.tickers.sort();
            c.tickers.dedup();
            c.tags.extend(item.tags.clone());
            c.tags.sort();
            c.tags.dedup();
            c.newest = item.item.ts;
        } else {
            let i = out.len();
            for t in &set {
                index.entry(t.clone()).or_default().push(i);
            }
            tokens.push(set);
            out.push(Cluster::wire(item));
        }
    }
    out.sort_by(|a, b| b.newest.cmp(&a.newest).then(a.lead.id.cmp(&b.lead.id)));
    (out, comparisons)
}

pub fn extract_tickers(text: &str, provider: &[String]) -> Vec<String> {
    default_names().explicit_tickers(text, provider, &[])
}
fn bare_ticker_allowed(word: &str, shouting: bool) -> bool {
    !(shouting
        || word.len() <= 1
        || matches!(
            word,
            "A" | "I"
                | "CEO"
                | "CFO"
                | "COO"
                | "AI"
                | "US"
                | "USA"
                | "UK"
                | "EU"
                | "ETF"
                | "IPO"
                | "GDP"
                | "CPI"
                | "FED"
                | "SEC"
                | "FDA"
                | "EPS"
                | "IT"
                | "ON"
                | "ALL"
                | "BE"
                | "AT"
                | "OR"
                | "ARE"
                | "FOR"
                | "NEW"
                | "NOW"
                | "INC"
                | "TV"
                | "DD"
                | "AM"
                | "PM"
                | "IN"
                | "IS"
                | "AN"
                | "AS"
                | "THE"
                | "AND"
                | "OF"
                | "TO"
                | "BY"
                | "FROM"
                | "WITH"
                | "OUR"
                | "YOU"
                | "YOUR"
                | "HAS"
                | "HAD"
                | "MAY"
                | "CAN"
                | "WILL"
                | "WAS"
                | "WERE"
                | "WHAT"
                | "WHEN"
                | "HOW"
                | "WHO"
                | "WHY"
                | "ITS"
                | "ONE"
                | "TWO"
                | "OUT"
                | "UP"
                | "DOWN"
                | "BUY"
                | "SELL"
                | "HOLD"
                | "GO"
                | "BIG"
                | "REAL"
                | "OPEN"
                | "SO"
                | "DO"
                | "MOD"
                | "LOW"
                | "KEY"
                | "TRUE"
                | "PLAY"
                | "LIFE"
                | "SEE"
                | "FUN"
                | "CASH"
                | "WELL"
                | "CAR"
                | "FAST"
                | "NICE"
                | "BEST"
                | "GOOD"
                | "HOME"
                | "ANY"
                | "PEAK"
                | "TECH"
                | "AP"
                | "CTO"
                | "ET"
                | "PT"
                | "LNG"
                | "OLED"
                | "OCC"
                | "WTI"
                | "MA"
                | "MW"
                | "HR"
                | "FT"
                | "CD"
                | "PC"
                | "EV"
                | "PCE"
                | "PPI"
                | "FOMC"
                | "UN"
                | "Q1"
                | "Q2"
                | "Q3"
                | "Q4"
                | "IQ"
                | "NRC"
                | "SMR"
                | "LSE"
                | "BC"
                | "NYC"
                | "SAN"
                | "LA"
                | "DC"
        ))
}
pub fn topic_tags(text: &str) -> Vec<String> {
    let tokens = words(text);
    [
        ("Earnings", "earnings revenue revenues profit profits quarterly"),
        ("Guidance", "guidance outlook forecasts forecast forecasting forecasted"),
        ("M&A", "acquire acquires acquired acquiring acquisition acquisitions merge merges merged merging merger mergers takeover takeovers buyout buyouts"),
        ("FDA", "fda clinical drug drugs"),
        ("Macro", "inflation gdp cpi payroll payrolls rates fed recession"),
        ("Crypto", "bitcoin crypto cryptocurrency ethereum blockchain"),
        ("Analyst", "analyst analysts upgrade upgrades upgraded upgrading downgrade downgrades downgraded downgrading"),
        ("Insider", "insider insiders"),
        ("Legal", "sue sues sued suing lawsuit lawsuits litigation antitrust ban bans banned banning"),
    ]
    .into_iter()
    .filter(|(tag, lexicon)| {
        (*tag == "M&A" && tokens.windows(2).any(|pair| pair[0] == "m" && pair[1] == "a"))
            || lexicon.split_whitespace().any(|w| tokens.iter().any(|t| t == w))
    })
    .take(2)
    .map(|(tag, _)| tag.into())
    .collect()
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeywordWatch {
    pub name: String,
    pub words: String,
    #[serde(deserialize_with = "colour_index")]
    pub colour: u8,
    /// Desktop delivery is opt-in independently of highlighting.
    pub notify: bool,
}
fn colour_index<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
    Ok(u8::deserialize(d)?.min(5))
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchHit {
    pub range: std::ops::Range<usize>,
    pub colour: u8,
}
/// All matching watches highlight their words; the first watch wins overlaps.
pub fn watch_highlights(text: &str, watches: &[KeywordWatch]) -> Vec<WatchHit> {
    let ranges: Vec<_> = watches
        .iter()
        .flat_map(|watch| {
            watch_ranges(text, &watch.words)
                .into_iter()
                .map(move |range| WatchHit { range, colour: watch.colour.min(5) })
        })
        .collect();
    let mut boundaries: Vec<_> = ranges.iter().flat_map(|hit| [hit.range.start, hit.range.end]).collect();
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut hits: Vec<WatchHit> = vec![];
    for span in boundaries.windows(2) {
        if let Some(hit) = ranges.iter().find(|hit| hit.range.start <= span[0] && hit.range.end >= span[1]) {
            if let Some(last) = hits.last_mut()
                && last.colour == hit.colour
                && last.range.end == span[0]
            {
                last.range.end = span[1];
            } else {
                hits.push(WatchHit { range: span[0]..span[1], colour: hit.colour });
            }
        }
    }
    hits
}
/// Keyword watches intentionally cover the headline only, in both pane and
/// notification matching. Summaries are displayed as unhighlighted prose.
pub fn watch_matches(story: &Story, query: &str) -> bool {
    !watch_ranges(&story.item.title, query).is_empty()
}
/// Byte ranges preserve the original headline for layout, including Unicode.
pub fn watch_ranges(text: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    let mut patterns = vec![];
    let mut phrase = String::new();
    let mut quoted = false;
    for c in query.chars().chain(std::iter::once(' ')) {
        if c == '"' {
            if quoted && !phrase.is_empty() {
                patterns.push(words(&phrase));
                phrase.clear();
            }
            quoted = !quoted;
        } else if (c.is_whitespace() || c == ',') && !quoted {
            if !phrase.is_empty() {
                patterns.push(words(&phrase));
                phrase.clear();
            }
        } else {
            phrase.push(c);
        }
    }
    if !phrase.is_empty() {
        patterns.push(words(&phrase));
    }
    let mut spans = vec![];
    let mut start = None;
    for (i, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        if c.is_alphanumeric() {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            spans.push((s..i, text[s..i].to_lowercase()));
        }
    }
    let mut ranges = vec![];
    for pattern in patterns.into_iter().filter(|p| !p.is_empty()) {
        for window in spans.windows(pattern.len()) {
            if window.iter().zip(&pattern).all(|((_, word), p)| word == p) {
                ranges.push(window[0].0.start..window[window.len() - 1].0.end);
            }
        }
    }
    ranges.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<std::ops::Range<usize>> = vec![];
    for range in ranges {
        if let Some(last) = merged.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}
