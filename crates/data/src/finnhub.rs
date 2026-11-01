//! Incremental Finnhub news; credentials are sensitive headers, never URLs.
#[cfg(test)]
#[path = "finnhub_tests.rs"]
mod tests;
use crate::{
    NewsItem,
    news_store::{NewsStore, Story},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tr_net::{Guard, Refused};

pub const CATEGORIES: [&str; 4] = ["general", "forex", "crypto", "merger"];
pub fn symbol(key: &str) -> Option<&str> {
    let ticker = key.strip_suffix(" US Equity")?;
    (!ticker.is_empty()
        && ticker.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-')))
    .then_some(ticker)
}
pub fn from_date(latest: Option<i64>, now: i64) -> String {
    DateTime::from_timestamp(latest.unwrap_or(now - 7 * 86400).min(now), 0)
        .unwrap_or_default()
        .format("%Y-%m-%d")
        .to_string()
}
#[derive(Deserialize)]
struct Wire {
    datetime: i64,
    headline: Option<String>,
    id: u64,
    related: Option<String>,
    source: Option<String>,
    summary: Option<String>,
    url: Option<String>,
}
/// The company behind one US equity, from `/stock/profile2`. Fields Finnhub omits keep
/// their defaults; `logo` is ignored and never fetched.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompanyProfile {
    pub name: String,
    pub ticker: String,
    pub exchange: String,
    pub currency: String,
    pub country: String,
    pub industry: String,
    pub ipo: String,
    pub weburl: String,
    /// Market cap in millions of `currency`, as Finnhub reports it.
    pub market_cap: f64,
    /// Shares outstanding, millions.
    pub shares_out: f64,
}
#[derive(Deserialize)]
struct ProfileWire {
    name: Option<String>,
    ticker: Option<String>,
    exchange: Option<String>,
    currency: Option<String>,
    country: Option<String>,
    #[serde(rename = "finnhubIndustry")]
    industry: Option<String>,
    ipo: Option<String>,
    weburl: Option<String>,
    #[serde(rename = "marketCapitalization")]
    market_cap: Option<f64>,
    #[serde(rename = "shareOutstanding")]
    shares_out: Option<f64>,
}
impl ProfileWire {
    /// Finnhub answers `{}` (or nothing but ignored fields) for a symbol it does not know.
    fn empty(&self) -> bool {
        self.name.is_none()
            && self.ticker.is_none()
            && self.exchange.is_none()
            && self.currency.is_none()
            && self.country.is_none()
            && self.industry.is_none()
            && self.ipo.is_none()
            && self.weburl.is_none()
            && self.market_cap.is_none()
            && self.shares_out.is_none()
    }
}
/// A `/stock/profile2` body: `{}` is a miss, a partial object fills defaults, and
/// anything malformed is an error.
pub fn parse_profile(bytes: &[u8]) -> anyhow::Result<Option<CompanyProfile>> {
    let wire: ProfileWire = serde_json::from_slice(bytes)?;
    if wire.empty() {
        return Ok(None);
    }
    Ok(Some(CompanyProfile {
        name: wire.name.unwrap_or_default(),
        ticker: wire.ticker.unwrap_or_default(),
        exchange: wire.exchange.unwrap_or_default(),
        currency: wire.currency.unwrap_or_default(),
        country: wire.country.unwrap_or_default(),
        industry: wire.industry.unwrap_or_default(),
        ipo: wire.ipo.unwrap_or_default(),
        weburl: wire.weburl.unwrap_or_default(),
        market_cap: wire.market_cap.unwrap_or_default(),
        shares_out: wire.shares_out.unwrap_or_default(),
    }))
}
pub fn parse(bytes: &[u8], company: Option<&str>) -> anyhow::Result<Vec<Story>> {
    let items: Vec<serde_json::Value> = serde_json::from_slice(bytes)?;
    Ok(items
        .into_iter()
        .filter_map(|value| serde_json::from_value::<Wire>(value).ok())
        .filter(|w| w.datetime > 0 && w.headline.as_ref().is_some_and(|s| !s.trim().is_empty()))
        .map(|w| {
            // Resolve related tickers through the same local catalog as app search.
            // Unknown/foreign names are not fabricated into US equities.
            let mut tickers: Vec<_> = w
                .related
                .as_deref()
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .filter_map(|s| tr_core::catalog::lookup(s, None).map(|listing| listing.key()))
                .collect();
            if let Some(key) = company {
                tickers.push(key.into());
            }
            tickers.sort();
            tickers.dedup();
            let source = w.source.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "Finnhub".into());
            let mut story = Story::new(
                NewsItem {
                    title: w.headline.unwrap_or_default(),
                    publisher: source.clone(),
                    link: w.url.unwrap_or_default(),
                    ts: w.datetime,
                    tickers,
                },
                &source,
            );
            story.provider = "Finnhub".into();
            story.provider_id = w.id.to_string();
            story.summary = w.summary.unwrap_or_default();
            story.category = if company.is_some() { "company" } else { "general" }.into();
            story
        })
        .collect())
}
pub struct Client {
    client: reqwest::Client,
    base: String,
}
impl Default for Client {
    fn default() -> Self {
        Self { client: reqwest::Client::new(), base: "https://finnhub.io/api/v1".into() }
    }
}
impl Client {
    /// GET through the guard with the credential as a sensitive header, never a query.
    async fn get(&self, guard: &Guard, key: &str, path: &str, query: &[(&str, String)]) -> Result<Vec<u8>, Refused> {
        let mut token = reqwest::header::HeaderValue::from_str(key.trim()).map_err(|_| Refused::InvalidRequest)?;
        token.set_sensitive(true);
        let req = self.client.get(format!("{}{path}", self.base)).query(query).header("X-Finnhub-Token", token);
        guard.send("finnhub", req).await
    }
    async fn fetch(
        &self,
        guard: &Guard,
        key: &str,
        path: &str,
        query: &[(&str, String)],
        company: Option<&str>,
    ) -> Result<Vec<Story>, Refused> {
        let body = self.get(guard, key, path, query).await?;
        parse(&body, company).map_err(|_| Refused::InvalidRequest)
    }
    /// `/stock/profile2` for one symbol. `Ok(None)` is a symbol Finnhub has no profile
    /// for (or a key that is not an equity at all, which is never requested).
    pub async fn company_profile(
        &self,
        guard: &Guard,
        key: &str,
        market_key: &str,
    ) -> Result<Option<CompanyProfile>, Refused> {
        let Some(ticker) = symbol(market_key) else { return Ok(None) };
        let body = self.get(guard, key, "/stock/profile2", &[("symbol", ticker.to_string())]).await?;
        parse_profile(&body).map_err(|_| Refused::InvalidRequest)
    }
    pub async fn company(
        &self,
        guard: &Guard,
        key: &str,
        market_key: &str,
        latest: Option<i64>,
        now: i64,
    ) -> Result<Vec<Story>, Refused> {
        let Some(ticker) = symbol(market_key) else { return Ok(vec![]) };
        let to = DateTime::<Utc>::from_timestamp(now, 0).unwrap_or_default().format("%Y-%m-%d").to_string();
        self.fetch(
            guard,
            key,
            "/company-news",
            &[("symbol", ticker.into()), ("from", from_date(latest, now)), ("to", to)],
            Some(market_key),
        )
        .await
    }
    pub async fn market(&self, guard: &Guard, key: &str, category: &str, min_id: u64) -> Result<Vec<Story>, Refused> {
        if !CATEGORIES.contains(&category) {
            return Err(Refused::InvalidRequest);
        }
        let mut items = self
            .fetch(guard, key, "/news", &[("category", category.into()), ("minId", min_id.to_string())], None)
            .await?;
        for item in &mut items {
            item.category = category.into();
        }
        Ok(items)
    }
}
pub fn cursor(store: &NewsStore, category: &str) -> anyhow::Result<u64> {
    match store.meta(&format!("minId:{category}"))? {
        Some(value) => Ok(value.parse()?),
        None => Ok(0),
    }
}
/// Commit the cursor only after every item has been durably inserted.
pub fn save_market(store: &mut NewsStore, category: &str, items: &[Story], now: i64) -> anyhow::Result<()> {
    let mut items = items.to_vec();
    for item in &mut items {
        item.category = category.into();
    }
    store.insert(&items, now)?;
    let max =
        items.iter().filter_map(|s| s.provider_id.parse::<u64>().ok()).max().unwrap_or(0).max(cursor(store, category)?);
    store.set_meta(&format!("minId:{category}"), &max.to_string())
}

/// Pure scheduler state, shared by one worker loop (never a task per pane).
#[derive(Default)]
pub struct Schedule {
    pub backoff_until: i64,
    market_due: [i64; 4],
    category: usize,
    companies: std::collections::HashMap<String, i64>,
    round: usize,
    company_day: i64,
    company_count: usize,
    last_job: Option<Job>,
    retry: Option<Job>,
}
/// Budget 2,000 company calls/day, leaving 576 market calls and cap headroom.
pub fn company_interval(symbols: usize) -> i64 {
    1800.max((symbols as i64 * 86400 + 1999) / 2000)
}
pub fn company_status(symbols: usize) -> String {
    let seconds = company_interval(symbols);
    if seconds <= 1800 {
        return String::new();
    }
    format!("Company news every {} h for {symbols} symbols", (seconds + 3599) / 3600)
}
#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    Market(&'static str),
    Company(String),
}
impl Schedule {
    /// Clear view cadence only; refusal backoff, daily budgets and guard state survive.
    pub fn refresh(&mut self, symbols: &[String], market: bool) {
        for symbol in symbols {
            self.companies.remove(symbol);
        }
        if market {
            self.market_due = [0; 4];
        }
    }
    pub fn next(&mut self, now: i64, visible: bool, symbols: &[String]) -> Option<Job> {
        self.next_for_views(now, visible, true, symbols, &[])
    }
    pub fn next_for_views(
        &mut self,
        now: i64,
        visible: bool,
        all_market: bool,
        symbols: &[String],
        priority: &[String],
    ) -> Option<Job> {
        if !visible || now < self.backoff_until {
            return None;
        }
        if let Some(job) = self.retry.take() {
            let eligible = match &job {
                Job::Market(category) => *category == "general" || all_market,
                Job::Company(symbol) => symbols.contains(symbol),
            };
            if eligible {
                self.last_job = Some(job.clone());
                return Some(job);
            }
        }
        for _ in CATEGORIES {
            let index = self.category;
            self.category = (self.category + 1) % CATEGORIES.len();
            if index != 0 && !all_market {
                continue;
            }
            if now >= self.market_due[index] {
                self.market_due[index] = now + if index == 0 { 300 } else { 900 };
                let job = Job::Market(CATEGORIES[index]);
                self.last_job = Some(job.clone());
                return Some(job);
            }
        }
        let day = now.div_euclid(86400);
        if self.company_day != day {
            self.company_day = day;
            self.company_count = 0;
        }
        if self.company_count >= 2000 {
            return None;
        }
        let count = symbols.iter().filter(|s| symbol(s).is_some()).count();
        let interval = company_interval(count);
        let mut candidates = priority.to_vec();
        for _ in 0..symbols.len() {
            candidates.push(symbols[self.round % symbols.len()].clone());
            self.round = self.round.wrapping_add(1);
        }
        for key in candidates {
            if symbol(&key).is_some() && self.companies.get(&key).is_none_or(|at| now - at >= interval) {
                self.companies.insert(key.clone(), now);
                self.company_count += 1;
                // Advance the next round past this symbol, not past the entire list.
                if let Some(index) = symbols.iter().position(|s| s == &key) {
                    self.round = index + 1;
                }
                let job = Job::Company(key);
                self.last_job = Some(job.clone());
                return Some(job);
            }
        }
        None
    }
    pub fn refused(&mut self, now: i64, error: &Refused) {
        self.backoff_until = match error {
            Refused::Paused { until } => (*until).max(now + 300),
            Refused::Budget => now + 3600,
            Refused::Busy => {
                self.retry = self.last_job.take();
                now + 3
            }
            _ => now + 300,
        };
    }
}
