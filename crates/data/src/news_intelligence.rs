//! Deterministic, bounded story intelligence. No I/O or provider calls.
#[cfg(test)]
#[path = "news_intelligence_tests.rs"]
mod tests;
use crate::news_store::Story;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_ITEMS: usize = 2_000;

/// Pure token/phrase index. Construction is worker-only; matching never scans
/// company names. SEC input order preserves its primary share class per CIK.
#[derive(Default)]
pub struct NameIndex {
    names: std::collections::HashMap<String, Name>,
    lengths: std::collections::HashMap<String, Vec<usize>>,
    tickers: std::collections::HashMap<String, String>,
}
struct Name {
    key: String,
    guarded: bool,
    crypto_context: bool,
}
/// Removing trailing legal/share-class decorations is deliberately iterative.
pub fn company_name(title: &str) -> String {
    let mut tokens = words(title);
    loop {
        let n = tokens.len();
        if n >= 2
            && (tokens[n - 2] == "class" && matches!(tokens[n - 1].as_str(), "a" | "b" | "c")
                || matches!((tokens[n - 2].as_str(), tokens[n - 1].as_str()), ("n", "v") | ("s", "a")))
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
            )
        }) {
            tokens.pop();
        } else {
            break;
        }
    }
    tokens.join(" ")
}
fn ambiguous_name(name: &str) -> bool {
    name.chars().count() <= 3
        || matches!(
            name,
            // Famous word-names are allowed, but only with evidence, not globally.
            "apple"
                | "amazon"
                | "meta"
                | "oracle"
                | "visa"
                | "target"
                | "ford"
                | "shell"
                | "block"
                | "gap"
                | "alphabet"
                | "arm"
                | "unity"
                | "toast"
                | "snap"
                | "zoom"
                | "uber"
                | "bloom"
                | "box"
                | "match"
                | "first"
                | "best"
                | "new"
                | "one"
                | "on"
                | "all"
                | "it"
                | "now"
                | "open"
                | "real"
                | "life"
                | "global"
                | "core"
                | "power"
                | "energy"
                | "future"
                | "next"
                | "shift"
                | "work"
                | "play"
                | "way"
                | "bright"
                | "root"
                | "lemonade"
                | "affirm"
                | "agora"
                | "catalyst"
                | "prime"
                | "eagle"
                | "star"
                | "sun"
                | "silver"
                | "gold"
                | "black"
                | "white"
                | "red"
                | "blue"
                | "green"
                | "ever"
                | "safe"
        )
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
        let mut primary = std::collections::HashMap::new();
        for (cik, ticker, title) in companies {
            // SEC also lists warrants, units and preferred shares. Prefer plain
            // US common tickers, preserving first-listed order within that tier.
            let common = ticker.len() <= 5
                && !ticker.contains(['-', '^', '/'])
                && !(ticker.len() == 5 && ticker.ends_with(['W', 'R', 'U']))
                && !title.to_ascii_lowercase().contains("preferred");
            let candidate = primary.entry(*cik).or_insert((ticker.as_str(), common));
            if common && !candidate.1 {
                *candidate = (ticker, true);
            }
        }
        for (cik, ticker, title) in companies {
            let key = format!("{} US Equity", primary[cik].0);
            index.tickers.insert(ticker.to_ascii_uppercase(), format!("{ticker} US Equity"));
            index.add(&company_name(title), &key, false);
        }
        for listing in tr_core::catalog::CATALOG.iter() {
            if matches!(listing.class, tr_core::security::AssetClass::Equity | tr_core::security::AssetClass::Etf) {
                index.tickers.entry(listing.ticker.into()).or_insert_with(|| listing.key());
                if listing.class == tr_core::security::AssetClass::Equity {
                    index.add(&company_name(listing.name), &listing.key(), false);
                }
            }
        }
        // Useful aliases not obtainable by stripping legal suffixes.
        for (alias, full) in [
            ("meta", "meta platforms"),
            ("facebook", "meta platforms"),
            ("google", "alphabet"),
            ("jpmorgan", "jpmorgan chase"),
            ("coinbase", "coinbase global"),
            ("palantir", "palantir technologies"),
            ("ford", "ford motor"),
        ] {
            if let Some(name) = index.names.get(full) {
                let key = name.key.clone();
                index.add(alias, &key, false);
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
            for alias in aliases.split_whitespace() {
                index.add(alias, &key, alias == "ripple");
                // Fixed crypto names take precedence over similarly named issuers.
                index.names.insert(
                    alias.into(),
                    Name { key: key.clone(), guarded: ambiguous_name(alias), crypto_context: alias == "ripple" },
                );
                if alias.len() <= 4 {
                    index.tickers.insert(alias.to_ascii_uppercase(), key.clone());
                }
            }
        }
        index
    }
    fn add(&mut self, alias: &str, key: &str, crypto_context: bool) {
        if alias.is_empty() {
            return;
        }
        self.names.entry(alias.into()).or_insert_with(|| Name {
            key: key.into(),
            guarded: ambiguous_name(alias),
            crypto_context,
        });
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
    /// Provider/queried associations only disambiguate a name actually present.
    pub fn subjects(&self, headline: &str, summary: &str, provider: &[String]) -> Vec<String> {
        let texts = [headline, summary];
        let spans = texts.map(text_spans);
        let mut explicit = std::collections::HashMap::new();
        for (part, tokens) in spans.iter().enumerate() {
            let text = texts[part];
            let letters: Vec<_> = text.chars().filter(|c| c.is_alphabetic()).collect();
            let shouting =
                !letters.is_empty() && letters.iter().filter(|c| c.is_uppercase()).count() * 100 >= letters.len() * 80;
            for (i, (range, _)) in tokens.iter().enumerate() {
                let raw = &text[range.clone()];
                let upper = raw.to_ascii_uppercase();
                let cash = text[..range.start].ends_with('$');
                let paren = text[..range.start].ends_with('(') && text[range.end..].starts_with(')');
                if !cash && tokens[i].1 == "meta" && tokens.get(i + 1).is_some_and(|(_, w)| w == "analysis") {
                    continue;
                }
                if let Some(key) = self.tickers.get(&upper)
                    && (cash || (raw == upper && (paren || bare_ticker_allowed(raw, shouting))))
                {
                    explicit.insert((part, i), key.clone());
                }
            }
        }
        let crypto = spans.iter().flatten().any(|(_, w)| {
            matches!(
                w.as_str(),
                "crypto"
                    | "cryptocurrency"
                    | "bitcoin"
                    | "ethereum"
                    | "blockchain"
                    | "token"
                    | "tokens"
                    | "xrp"
                    | "dogecoin"
                    | "solana"
            )
        });
        let mut result = vec![];
        for (part, tokens) in spans.iter().enumerate() {
            let mut i = 0;
            while i < tokens.len() {
                let mut found = explicit.get(&(part, i)).map(|key| (key.as_str(), 1));
                if let Some(lengths) = self.lengths.get(&tokens[i].1) {
                    for &len in lengths {
                        if i + len > tokens.len() {
                            continue;
                        }
                        let phrase = tokens[i..i + len].iter().map(|(_, w)| w.as_str()).collect::<Vec<_>>().join(" ");
                        let Some(name) = self.names.get(&phrase) else {
                            continue;
                        };
                        let next = tokens.get(i + len).map(|(_, w)| w.as_str()).unwrap_or("");
                        // Neither a hyphenated meta-analysis nor a possessive-like
                        // word fragment is a company mention.
                        if phrase == "meta" && next == "analysis" {
                            continue;
                        }
                        let context = matches!(
                            next,
                            "stock"
                                | "shares"
                                | "inc"
                                | "incorporated"
                                | "corp"
                                | "corporation"
                                | "ltd"
                                | "limited"
                                | "plc"
                                | "earnings"
                                | "ceo"
                        ) || (next == "s"
                            && texts[part][tokens[i + len - 1].0.end..].starts_with(['\'', '’']))
                            || provider.contains(&name.key)
                            || explicit.values().any(|key| key == &name.key);
                        if (!name.guarded || context) && (!name.crypto_context || crypto) {
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
    let letters: Vec<_> = text.chars().filter(|c| c.is_alphabetic()).collect();
    let shouting =
        !letters.is_empty() && letters.iter().filter(|c| c.is_uppercase()).count() * 100 >= letters.len() * 80;
    let mut result = provider.to_vec();
    for raw in text.split(|c: char| !c.is_alphanumeric() && c != '$') {
        let cash = raw.starts_with('$');
        let word = raw.trim_start_matches('$');
        let upper = word.to_ascii_uppercase();
        let word = if cash { upper.as_str() } else { word };
        if word.is_empty() || word.len() > 5 || !word.bytes().all(|b| b.is_ascii_uppercase()) {
            continue;
        }
        if !cash && !bare_ticker_allowed(word, shouting) {
            continue;
        }
        if let Some(listing) = tr_core::catalog::lookup(word, None) {
            result.push(listing.key());
        }
    }
    result.sort();
    result.dedup();
    result
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
