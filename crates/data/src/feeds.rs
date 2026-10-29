//! Built-in N6 catalogue. Feed content is untrusted data, never instructions.
#[cfg(test)]
#[path = "feeds_tests.rs"]
mod tests;
use crate::{
    news_store::{NewsStore, Story},
    rss::parse_entries,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct Feed {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub publisher: &'static str,
    pub category: &'static str,
    pub region: &'static str,
    pub tier: u8,
    pub group: &'static str,
    pub cadence: Duration,
}
macro_rules! feed {
    ($id:literal,$name:literal,$url:literal,$publisher:literal,$category:literal,$region:literal,$tier:literal,$group:literal,$seconds:literal) => {
        Feed {
            id: $id,
            name: $name,
            url: $url,
            publisher: $publisher,
            category: $category,
            region: $region,
            tier: $tier,
            group: $group,
            cadence: Duration::from_secs($seconds),
        }
    };
}
pub const FEEDS: [Feed; 12] = [
    feed!(
        "fed-press",
        "Federal Reserve",
        "https://www.federalreserve.gov/feeds/press_all.xml",
        "FEDERAL RESERVE",
        "macro",
        "US",
        1,
        "regulators",
        900
    ),
    feed!(
        "sec-press",
        "SEC Press Releases",
        "https://www.sec.gov/news/pressreleases.rss",
        "SEC",
        "regulatory",
        "US",
        1,
        "sec",
        900
    ),
    feed!(
        "ecb-press",
        "ECB Press",
        "https://www.ecb.europa.eu/rss/press.html",
        "ECB",
        "macro",
        "EU",
        1,
        "regulators",
        900
    ),
    feed!(
        "boe-news",
        "Bank of England",
        "https://www.bankofengland.co.uk/rss/news",
        "BANK OF ENGLAND",
        "macro",
        "UK",
        1,
        "regulators",
        900
    ),
    feed!(
        "bbc-business",
        "BBC Business",
        "https://feeds.bbci.co.uk/news/business/rss.xml",
        "BBC",
        "markets",
        "GLOBAL",
        2,
        "global-media",
        900
    ),
    feed!(
        "bbc-world",
        "BBC World",
        "https://feeds.bbci.co.uk/news/world/rss.xml",
        "BBC",
        "geopolitics",
        "GLOBAL",
        2,
        "global-media",
        900
    ),
    feed!(
        "cnbc-top",
        "CNBC Top",
        "https://search.cnbc.com/rs/search/combinedcms/view.xml?partnerId=wrss01&id=100003114",
        "CNBC",
        "markets",
        "US",
        2,
        "global-media",
        900
    ),
    feed!(
        "cnbc-world",
        "CNBC World",
        "https://search.cnbc.com/rs/search/combinedcms/view.xml?partnerId=wrss01&id=100727362",
        "CNBC",
        "markets",
        "GLOBAL",
        2,
        "global-media",
        900
    ),
    feed!(
        "aljazeera",
        "Al Jazeera",
        "https://www.aljazeera.com/xml/rss/all.xml",
        "AL JAZEERA",
        "geopolitics",
        "GLOBAL",
        2,
        "global-media",
        900
    ),
    feed!("techcrunch", "TechCrunch", "https://techcrunch.com/feed/", "TECHCRUNCH", "tech", "GLOBAL", 2, "tech", 1800),
    feed!(
        "theverge",
        "The Verge",
        "https://www.theverge.com/rss/index.xml",
        "THE VERGE",
        "tech",
        "GLOBAL",
        3,
        "tech",
        1800
    ),
    feed!("hackernews", "Hacker News", "https://hnrss.org/frontpage", "HACKER NEWS", "tech", "GLOBAL", 4, "tech", 1800),
];
pub const CATEGORIES: [&str; 6] = ["markets", "macro", "regulatory", "geopolitics", "tech", "crypto"];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub id: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub last_success_at: Option<i64>,
    pub last_item_at: Option<i64>,
    pub last_status: String,
    pub items_today: u32,
    pub day: i64,
}
impl Feed {
    pub fn quiet(&self, state: &State, now: i64) -> bool {
        state.last_item_at.is_some_and(|at| {
            now - at > if matches!(self.group, "regulators" | "sec") { 30 * 86400 } else { 3 * 86400 }
        })
    }
    pub fn parse(&self, xml: &str) -> Result<Vec<Story>> {
        Ok(parse_entries(xml, self.publisher)?
            .into_iter()
            .map(|(mut item, _guid, summary)| {
                item.publisher = self.publisher.into();
                let mut story = Story::new(item, self.publisher);
                story.summary = summary;
                story.provider = "rss".into();
                story.provider_id = self.id.into();
                story.category = self.category.into();
                story
            })
            .collect())
    }
    pub async fn fetch(&self, guard: &tr_net::Guard, state: &State) -> Result<(Vec<Story>, tr_net::ResponseMetadata)> {
        let request = conditional_request(self, state);
        let (body, metadata) = guard.send_feed(self.group, self.id, request).await?;
        let stories = if metadata.status == 304 { vec![] } else { self.parse(std::str::from_utf8(&body)?)? };
        Ok((stories, metadata))
    }
}
pub fn conditional_request(feed: &Feed, state: &State) -> reqwest::RequestBuilder {
    let mut request = reqwest::Client::new().get(feed.url);
    if let Some(etag) = &state.etag {
        request = request.header("If-None-Match", etag);
    }
    if let Some(modified) = &state.last_modified {
        request = request.header("If-Modified-Since", modified);
    }
    request
}
/// One entry per feed, with at most one attempt per tick. Sharing hosts alternates
/// fairly: an unsent spacing refusal waits one cadence instead of spinning.
pub struct Schedule {
    due: [i64; 12],
    hosts: std::collections::BTreeMap<&'static str, i64>,
    next_slot: i64,
}
impl Schedule {
    pub fn new(now: i64) -> Self {
        Self { due: std::array::from_fn(|i| now + i as i64 * 5), hosts: Default::default(), next_slot: now }
    }
    pub fn next(&mut self, now: i64) -> Option<&'static Feed> {
        self.next_available(now, |_| true)
    }
    pub fn next_available(&mut self, now: i64, available: impl Fn(&Feed) -> bool) -> Option<&'static Feed> {
        if now < self.next_slot {
            return None;
        }
        let host = |i: usize| FEEDS[i].url.split('/').nth(2).expect("built-in host");
        let i = (0..FEEDS.len())
            .filter(|i| {
                available(&FEEDS[*i]) && self.due[*i] <= now && self.hosts.get(host(*i)).is_none_or(|at| now >= *at)
            })
            .min_by_key(|i| self.due[*i])?;
        self.due[i] = now + FEEDS[i].cadence.as_secs() as i64;
        self.hosts.insert(host(i), self.due[i]);
        self.next_slot = now + 5;
        Some(&FEEDS[i])
    }
}
impl NewsStore {
    pub fn feed_state(&self, id: &str) -> Result<State> {
        self.load_feed_state(id)
    }
}
