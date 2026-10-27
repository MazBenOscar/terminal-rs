//! One serialized scheduler per worker. Cadences and validators survive restarts.
use crate::{parse_feed, parse_submissions, parse_tickers, store::Store, watched};
use anyhow::Result;
use std::{collections::BTreeMap, future::Future, time::Duration};
use tr_net::{Refused, ResponseMetadata};

const DAY: i64 = 86_400;
const CYCLE: i64 = 600;
const MAP: &str = "https://www.sec.gov/files/company_tickers.json";
#[derive(Clone, Debug, Default)]
pub struct Reply {
    pub body: Vec<u8>,
    pub metadata: ResponseMetadata,
}
/// Injection boundary: production has exactly one implementation, via the shared guard.
pub trait Transport: Send + Sync {
    fn check(&self) -> std::result::Result<(), Refused>;
    // Fixture transports may keep cadence locally; production always claims the shared ledger.
    fn claim_slot(&self, _key: &str, _interval: Duration) -> std::result::Result<bool, Refused> {
        Ok(true)
    }
    fn pending_allowed(&self) -> std::result::Result<bool, Refused> {
        Ok(true)
    }
    fn defer_slot(&self, _key: &str, _interval: Duration) -> std::result::Result<(), Refused> {
        Ok(())
    }
    fn increment_counter(&self, _key: &str, local_next: i64) -> std::result::Result<i64, Refused> {
        Ok(local_next)
    }
    fn clear_counter(&self, _key: &str) -> std::result::Result<(), Refused> {
        Ok(())
    }
    fn get(
        &self,
        url: &str,
        etag: Option<String>,
        modified: Option<String>,
    ) -> impl Future<Output = std::result::Result<Reply, Refused>> + Send;
}
pub struct Sec;
impl Transport for Sec {
    fn increment_counter(&self, key: &str, _local_next: i64) -> std::result::Result<i64, Refused> {
        tr_net::shared()?.increment_counter(key)
    }
    fn clear_counter(&self, key: &str) -> std::result::Result<(), Refused> {
        tr_net::shared()?.clear_counter(key)
    }
    fn defer_slot(&self, key: &str, interval: Duration) -> std::result::Result<(), Refused> {
        tr_net::shared()?.defer_slot(key, interval)
    }
    fn claim_slot(&self, key: &str, interval: Duration) -> std::result::Result<bool, Refused> {
        tr_net::shared()?.claim_slot(key, interval)
    }
    fn pending_allowed(&self) -> std::result::Result<bool, Refused> {
        let status = tr_net::shared()?.status()?.into_iter().find(|s| s.group == "sec").ok_or(Refused::Ledger)?;
        Ok(status.requests_today < status.cap * 80 / 100)
    }
    fn check(&self) -> std::result::Result<(), Refused> {
        tr_net::shared()?.check("sec")
    }
    async fn get(
        &self,
        url: &str,
        etag: Option<String>,
        modified: Option<String>,
    ) -> std::result::Result<Reply, Refused> {
        let guard = tr_net::shared()?;
        let mut request = reqwest::Client::new().get(url);
        if let Some(etag) = etag {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        if let Some(modified) = modified {
            request = request.header(reqwest::header::IF_MODIFIED_SINCE, modified);
        }
        let (body, metadata) = if url.starts_with("https://data.sec.gov/submissions/") {
            guard.send_with_metadata_budget("sec", request, 1200).await?
        } else {
            guard.send_with_metadata("sec", request).await?
        };
        Ok(Reply { body, metadata })
    }
}
pub fn refusal_label(reason: &Refused) -> String {
    match reason {
        Refused::NeedsContact => "Needs contact email (Settings › Data sources)".into(),
        Refused::Paused { until } => format!(
            "Paused until {}",
            chrono::DateTime::from_timestamp(*until, 0)
                .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| until.to_string())
        ),
        _ => reason.to_string(),
    }
}
pub struct Scheduler<T = Sec> {
    pub store: Store,
    transport: T,
    pub status: String,
}
impl<T: Transport> Scheduler<T> {
    pub fn new(store: Store, transport: T) -> Self {
        Self { store, transport, status: "SEC EDGAR · not updated yet".into() }
    }
    pub async fn tick(&mut self, now: i64, active: bool, symbols: &[String]) {
        if !active {
            return;
        }
        if self.store.prune(now).is_err() {
            self.status = "SEC EDGAR · store unavailable".into();
            return;
        }
        if symbols.is_empty() {
            self.status = "SEC EDGAR · only watchlist symbols are tracked".into();
            return;
        }
        if let Err(reason) = self.transport.check() {
            self.status = refusal_label(&reason);
            return;
        }
        match self.run(now, symbols).await {
            Ok(()) => {
                let updated = self.store.number("updated").unwrap_or(0);
                self.status = if updated == 0 {
                    "SEC EDGAR · not updated yet".into()
                } else {
                    format!("SEC EDGAR · updated {}m ago", (now - updated).max(0) / 60)
                };
                if let Ok(companies) = self.store.companies() {
                    let watched = watched(&companies, symbols);
                    if !self.transport.pending_allowed().unwrap_or(false) {
                        self.status.push_str(" · Company refresh deferred to reserve the feed budget");
                    }
                    let days = sweep_days(watched.len());
                    if days > 1 {
                        self.status
                            .push_str(&format!(" · Daily refresh every {days} days for {} companies", watched.len()));
                    }
                    let skipped: Vec<_> = watched
                        .keys()
                        .filter(|cik| self.store.number(&format!("skip:{cik}")).unwrap_or(0) > now)
                        .map(|cik| format!("CIK {cik}"))
                        .collect();
                    if !skipped.is_empty() {
                        self.status.push_str(&format!(
                            " · {} {} could not be read: {}",
                            skipped.len(),
                            if skipped.len() == 1 { "company" } else { "companies" },
                            skipped.join(", ")
                        ));
                    }
                }
            }
            Err(e) => {
                self.status = e
                    .downcast_ref::<Refused>()
                    .map(refusal_label)
                    .unwrap_or_else(|| "SEC EDGAR · refresh unavailable (stored filings retained)".into())
            }
        }
    }
    async fn fetch(&mut self, key: &str, url: &str) -> Result<Reply> {
        Ok(self
            .transport
            .get(url, self.store.get(&format!("etag:{key}"))?, self.store.get(&format!("modified:{key}"))?)
            .await?)
    }
    fn validators(&self, key: &str, metadata: &ResponseMetadata) -> Result<()> {
        // Called only after parsing and storage succeeded. Drop stale validators on a fresh response.
        for (name, value) in [("etag", &metadata.etag), ("modified", &metadata.last_modified)] {
            let key = format!("{name}:{key}");
            if let Some(value) = value {
                self.store.set(&key, value)?;
            } else if metadata.status != 304 {
                self.store.remove(&key)?;
            }
        }
        Ok(())
    }
    fn claim(&self, key: &str, seconds: i64) -> Result<bool> {
        Ok(self.transport.claim_slot(key, Duration::from_secs(seconds as u64))?)
    }
    async fn run(&mut self, now: i64, symbols: &[String]) -> Result<()> {
        if due(self.store.number("map_checked")?, now, DAY) {
            self.store.set("map_checked", now)?;
            if self.claim("sec:map", DAY)? {
                let reply = self.fetch("map", MAP).await?;
                if reply.metadata.status != 304 {
                    let companies = parse_tickers(&reply.body)?;
                    anyhow::ensure!(!companies.is_empty(), "Empty company map");
                    self.store.replace_companies(&companies)?;
                }
                self.validators("map", &reply.metadata)?;
            }
        }
        let watched = watched(&self.store.companies()?, symbols);
        if watched.is_empty() {
            return Ok(());
        }
        // Re-space when companies are added, without resetting a slot on every tick.
        let signature = watched.keys().map(u64::to_string).collect::<Vec<_>>().join(",");
        if let Some(previous) = self.store.get("watched")?
            && previous != signature
        {
            for cik in watched.keys() {
                if !previous.split(',').any(|s| s == cik.to_string()) {
                    self.store.set(&format!("new:{cik}"), 1)?;
                }
            }
            let soon = (self.store.number("last_import")? + 60).max(now);
            self.store.set("next_sweep", self.store.number("next_sweep")?.min(soon))?;
        }
        self.store.set("watched", signature)?;
        if due(self.store.number("feed_checked")?, now, CYCLE) {
            self.store.set("feed_checked", now)?;
            for form in ["8-K", "4"] {
                if !self.claim(&format!("sec:feed:{form}"), CYCLE)? {
                    continue;
                }
                let url = format!(
                    "https://www.sec.gov/cgi-bin/browse-edgar?action=getcurrent&type={form}&company=&dateb=&owner=include&start=0&count=100&output=atom"
                );
                let key = format!("feed:{form}");
                let reply = self.fetch(&key, &url).await?;
                if reply.metadata.status != 304 {
                    let mut hits = parse_feed(&reply.body)?;
                    hits.retain(|f| watched.contains_key(&f.cik));
                    for f in &mut hits {
                        f.ticker = watched[&f.cik].clone();
                        if !f.reporting_owner
                            && !self.store.enriched(f.cik, &f.accession)?
                            && self.store.number(&format!("pending_dropped:{}", f.cik))? == 0
                        {
                            let key = format!("pending:{}", f.cik);
                            let mut pending: Vec<String> =
                                self.store.get(&key)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
                            if !pending.contains(&f.accession) && pending.len() < 100 {
                                pending.push(f.accession.clone());
                            }
                            self.store.set(&key, serde_json::to_string(&pending)?)?;
                        }
                    }
                    self.store.upsert(&hits)?;
                }
                self.validators(&key, &reply.metadata)?;
            }
            self.store.set("updated", now)?;
        }
        // Least-recently-attempted order applies to pending work too.
        let mut order: Vec<_> = watched
            .iter()
            .map(|(&cik, ticker)| Ok((self.store.number(&format!("attempt:{cik}"))?, cik, ticker)))
            .collect::<Result<_>>()?;
        order.sort_by_key(|(last, cik, _)| (*last, *cik));
        for (_, cik, ticker) in order {
            if self.pending(cik)? && self.company_due(cik, now)? && self.transport.pending_allowed()? {
                self.attempt(cik, ticker, now, false).await?;
            }
        }
        self.sweep(&watched, now).await?;
        self.store.prune(now)?;
        Ok(())
    }
    fn pending(&self, cik: u64) -> Result<bool> {
        Ok(self.store.get(&format!("pending:{cik}"))?.is_some_and(|s| s != "0" && s != "[]")
            && self.store.number(&format!("pending_attempts:{cik}"))? < 3)
    }
    fn company_due(&self, cik: u64, now: i64) -> Result<bool> {
        Ok(due(self.store.number(&format!("attempt:{cik}"))?, now, CYCLE)
            && now >= self.store.number(&format!("retry:{cik}"))?
            && now >= self.store.number(&format!("skip:{cik}"))?)
    }
    async fn attempt(&mut self, cik: u64, ticker: &str, now: i64, daily: bool) -> Result<()> {
        // Keep all non-feed work below 80%, leaving enough room for both feeds through midnight.
        if !self.transport.pending_allowed()? {
            return Ok(());
        }
        if self.store.number(&format!("initialized:{cik}"))? == 0
            && (self.store.number("last_import")? + 60 > now || !self.claim("sec:import", 60)?)
        {
            return Ok(());
        }
        if !self.claim(&format!("sec:sub:{cik}"), CYCLE)? {
            return Ok(());
        }
        self.store.remove(&format!("new:{cik}"))?;
        let pending = self.pending(cik)?;
        if pending && !daily {
            let attempts = self.transport.increment_counter(
                &format!("sec:pending:{cik}"),
                self.store.number(&format!("pending_attempts:{cik}"))? + 1,
            )?;
            self.store.set(&format!("pending_attempts:{cik}"), attempts)?;
            if attempts > 3 {
                self.store.remove(&format!("pending:{cik}"))?;
                self.store.set(&format!("pending_dropped:{cik}"), 1)?;
                return Ok(());
            }
        }
        self.store.set(&format!("attempt:{cik}"), now)?;
        // Every attempt, including failures, consumes its sweep turn.
        if daily {
            self.store.set(&format!("swept:{cik}"), now)?;
        }
        self.store.set("last_import", now)?;
        let result = self.submissions(cik, ticker, now).await;
        if result.is_err() {
            let failures = self.transport.increment_counter(
                &format!("sec:failure:sub:{cik}"),
                self.store.number(&format!("failures:{cik}"))? + 1,
            )?;
            self.store.set(&format!("failures:{cik}"), failures)?;
            let backoff = [3600, 21600, 86400][(failures - 1).min(2) as usize];
            self.store.set(&format!("retry:{cik}"), now + backoff)?;
            self.transport.defer_slot(&format!("sec:sub:{cik}"), Duration::from_secs(backoff as u64))?;
            if daily {
                let failures = self.transport.increment_counter(
                    &format!("sec:failure:sweep:{cik}"),
                    self.store.number(&format!("daily_failures:{cik}"))? + 1,
                )?;
                self.store.set(&format!("daily_failures:{cik}"), failures)?;
                if failures >= 3 {
                    self.store.set(&format!("skip:{cik}"), now + 7 * DAY)?;
                    self.transport.defer_slot(&format!("sec:sub:{cik}"), Duration::from_secs(7 * DAY as u64))?;
                    self.transport.defer_slot(&format!("sec:sweep:{cik}"), Duration::from_secs(7 * DAY as u64))?;
                    self.store.set(&format!("daily_failures:{cik}"), 0)?;
                    self.transport.clear_counter(&format!("sec:failure:sweep:{cik}"))?;
                }
            }
        } else {
            self.transport.clear_counter(&format!("sec:failure:sub:{cik}"))?;
            self.transport.clear_counter(&format!("sec:failure:sweep:{cik}"))?;
            self.store.set(&format!("failures:{cik}"), 0)?;
            self.store.set(&format!("retry:{cik}"), 0)?;
            self.store.set(&format!("daily_failures:{cik}"), 0)?;
            // A successful enrichment also serves as today's sweep.
            self.store.set(&format!("swept:{cik}"), now)?;
        }
        if pending && self.store.number(&format!("pending_attempts:{cik}"))? >= 3 {
            self.store.remove(&format!("pending:{cik}"))?;
            self.store.set(&format!("pending_dropped:{cik}"), 1)?;
        }
        // Source-wide refusals stop the cycle; ordinary CIK errors must not starve other companies.
        if let Err(error) = result
            && matches!(
                error.downcast_ref::<Refused>(),
                Some(
                    Refused::Paused { .. }
                        | Refused::Budget
                        | Refused::Disabled
                        | Refused::Offline
                        | Refused::NeedsContact
                        | Refused::Ledger
                )
            )
        {
            return Err(error);
        }
        Ok(())
    }
    async fn submissions(&mut self, cik: u64, ticker: &str, now: i64) -> Result<()> {
        let key = format!("sub:{cik}");
        let reply = self.fetch(&key, &format!("https://data.sec.gov/submissions/CIK{cik:010}.json")).await?;
        if reply.metadata.status != 304 {
            // Persist only the parsed, tracked 90-day window, never the submissions body.
            let filings = parse_submissions(&reply.body, cik, ticker, now - 90 * DAY)?;
            self.store.upsert(&filings)?;
        }
        let days = if self.store.number(&format!("initialized:{cik}"))? == 0 { 30 } else { 90 };
        self.store.set(&format!("window:{cik}"), now - days * DAY)?;
        self.store.set(&format!("initialized:{cik}"), now)?;
        let pending_key = format!("pending:{cik}");
        if let Some(value) = self.store.get(&pending_key)? {
            let mut pending: Vec<String> = serde_json::from_str(&value).unwrap_or_default();
            pending.retain(|acc| !self.store.enriched(cik, acc).unwrap_or(false));
            if pending.is_empty() {
                self.store.remove(&pending_key)?;
                self.store.set(&format!("pending_attempts:{cik}"), 0)?;
            } else {
                self.store.set(&pending_key, serde_json::to_string(&pending)?)?;
            }
        }
        self.validators(&key, &reply.metadata)?;
        self.store.set("updated", now)?;
        Ok(())
    }
    async fn sweep(&mut self, watched: &BTreeMap<u64, String>, now: i64) -> Result<()> {
        let has_new = watched.keys().any(|cik| {
            self.store.number(&format!("new:{cik}")).unwrap_or(0) != 0
                && self.store.number(&format!("attempt:{cik}")).unwrap_or(0) == 0
        });
        if (self.store.number("next_sweep")? > now && !has_new) || self.store.number("last_import")? + 144 > now {
            return Ok(());
        }
        let period = sweep_days(watched.len()) * DAY;
        let spacing = (period / watched.len().max(1) as i64).max(144);
        self.store.set("next_sweep", now + spacing)?;
        let mut order: Vec<_> = watched
            .iter()
            .map(|(&cik, ticker)| Ok((self.store.number(&format!("attempt:{cik}"))?, cik, ticker)))
            .collect::<Result<_>>()?;
        order.sort_by_key(|(last, cik, _)| (*last, *cik));
        for (_, cik, ticker) in order {
            if due(self.store.number(&format!("swept:{cik}"))?, now, period)
                && self.company_due(cik, now)?
                && self.transport.pending_allowed()?
                && self.claim("sec:sweep", 144)?
                && self.claim(&format!("sec:sweep:{cik}"), period)?
            {
                self.transport.clear_counter(&format!("sec:pending:{cik}"))?;
                self.store.set(&format!("pending_dropped:{cik}"), 0)?;
                self.store.set(&format!("pending_attempts:{cik}"), 0)?;
                self.attempt(cik, ticker, now, true).await?;
                break;
            }
        }
        Ok(())
    }
}
fn sweep_days(companies: usize) -> i64 {
    companies.div_ceil(600).max(1) as i64
}
fn due(last: i64, now: i64, cadence: i64) -> bool {
    last == 0 || now - last >= cadence
}
