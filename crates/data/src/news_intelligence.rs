//! Deterministic, bounded story intelligence. No I/O or provider calls.
#[cfg(test)]
#[path = "news_intelligence_tests.rs"]
mod tests;
use crate::news_store::Story;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_ITEMS: usize = 2_000;
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
            if !item.item.tickers.is_empty()
                && out[i].members.iter().any(|member| {
                    !member.item.tickers.is_empty()
                        && !member.item.tickers.iter().any(|t| item.item.tickers.contains(t))
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
        if !cash
            && (shouting
                || word.len() == 1
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
        {
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
