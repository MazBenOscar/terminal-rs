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
    fn decrement_counter(&self, _key: &str) -> std::result::Result<(), Refused> {
        Ok(())
    }
    fn release_slot(&self, _key: &str) -> std::result::Result<(), Refused> {
        Ok(())
    }
    fn reserve_counter(&self, key: &str, next: i64, limit: i64) -> std::result::Result<bool, Refused> {
        if self.increment_counter(key, next)? <= limit {
            Ok(true)
        } else {
            self.decrement_counter(key)?;
            Ok(false)
        }
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
    fn reserve_counter(&self, key: &str, _next: i64, limit: i64) -> std::result::Result<bool, Refused> {
        // Production charges the allowance at each actual guarded HTTP hop, not here.
        Ok(tr_net::shared()?.counter(key)? < limit)
    }
    fn decrement_counter(&self, key: &str) -> std::result::Result<(), Refused> {
        if key.starts_with("sec:f4:day:") {
            // Never refund an HTTP request that did reach the server (e.g. a 403 pause).
            Ok(())
        } else {
            tr_net::shared()?.decrement_counter(key)
        }
    }
    fn release_slot(&self, key: &str) -> std::result::Result<(), Refused> {
        tr_net::shared()?.release_slot(key)
    }
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
        let (body, metadata) = if url.starts_with("https://www.sec.gov/Archives/") {
            guard.send_with_consumer_budget("sec", request, 1200, "sec:f4:day:", 300).await?
        } else if url.starts_with("https://data.sec.gov/submissions/") {
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
/// Machine-readable snapshot status. Presentation text is never used to infer empty-state causes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatusFlags {
    pub unavailable: bool,
    pub importing: bool,
    pub budget_deferred: bool,
}
pub struct Scheduler<T = Sec> {
    pub store: Store,
    transport: T,
    pub status: String,
    pub flags: StatusFlags,
}
impl<T: Transport> Scheduler<T> {
    pub fn new(store: Store, transport: T) -> Self {
        Self { store, transport, status: "SEC EDGAR · not updated yet".into(), flags: StatusFlags::default() }
    }
    pub async fn tick(&mut self, now: i64, active: bool, symbols: &[String]) {
        if !active {
            return;
        }
        self.flags = StatusFlags::default();
        if self.store.prune(now).is_err() {
            self.flags.unavailable = true;
            self.status = "SEC EDGAR · store unavailable".into();
            return;
        }
        if symbols.is_empty() {
            self.status = "SEC EDGAR · only watchlist symbols are tracked".into();
            return;
        }
        if let Err(reason) = self.transport.check() {
            self.flags.budget_deferred = matches!(reason, Refused::Budget | Refused::Busy);
            self.flags.unavailable = !self.flags.budget_deferred;
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
                        self.flags.budget_deferred = true;
                        self.status.push_str(" · Company refresh deferred to reserve the feed budget");
                    }
                    let imported = watched
                        .keys()
                        .filter(|cik| self.store.number(&format!("initialized:{cik}")).unwrap_or(0) > 0)
                        .count();
                    self.flags.importing = imported < watched.len();
                    if self.flags.importing {
                        self.status.push_str(&format!(
                            " · Importing filings: {imported} of {} companies (one a minute while visible).",
                            watched.len()
                        ));
                    }
                    self.backlog_status(&watched, now);
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
                self.flags.budget_deferred =
                    matches!(e.downcast_ref::<Refused>(), Some(Refused::Budget | Refused::Busy));
                self.flags.unavailable = !self.flags.budget_deferred;
                self.status = e
                    .downcast_ref::<Refused>()
                    .map(refusal_label)
                    .unwrap_or_else(|| "SEC EDGAR · refresh unavailable (stored filings retained)".into());
                if e.downcast_ref::<Refused>().is_none()
                    && let Ok(companies) = self.store.companies()
                {
                    self.backlog_status(&watched(&companies, symbols), now);
                }
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
        self.insiders(&watched, now).await?;
        self.store.prune(now)?;
        Ok(())
    }
    fn insider_backlog(&self, watched: &BTreeMap<u64, String>) -> Result<Vec<(crate::Filing, i64)>> {
        Ok(self
            .store
            .insider_backlog()?
            .into_iter()
            .filter(|(f, _)| watched.contains_key(&f.cik) && crate::insider::xml_url(f).is_some())
            .collect())
    }
    fn backlog_status(&mut self, watched: &BTreeMap<u64, String>, now: i64) {
        if let Ok(rows) = self.insider_backlog(watched) {
            let waiting = rows.iter().filter(|(_, retry)| *retry > now).count();
            let eligible = rows.len() - waiting;
            if !rows.is_empty() {
                self.status.push_str(&format!(" · Reading insider forms: {eligible} left"));
                if waiting > 0 {
                    self.status.push_str(&format!(" ({waiting} waiting to retry)"));
                }
            }
        }
    }
    /// One document per timer turn (deliberately below the four-per-minute ceiling).
    /// No sleeping worker or catch-up bursts; all pacing/ownership also lives in the shared ledger.
    async fn insiders(&mut self, watched: &BTreeMap<u64, String>, now: i64) -> Result<()> {
        if !self.transport.pending_allowed()? {
            return Ok(());
        }
        let candidates = self.insider_backlog(watched)?;
        for (f, retry) in candidates {
            let key = format!("sec:f4:{}", f.accession);
            let attempts_key = format!("f4:attempts:{}", f.accession);
            let attempts = self.store.number(&attempts_key)?;
            if retry > now {
                continue;
            }
            // Never reserve an accession when pacing or the daily ceiling prevents a request.
            let day = chrono::DateTime::from_timestamp(now, 0)
                .ok_or_else(|| anyhow::anyhow!("Invalid clock"))?
                .format("%Y%m%d")
                .to_string();
            let day_key = format!("sec:f4:day:{day}");
            if self.store.number(&day_key)? >= 300 {
                return Ok(());
            }
            if !self.claim("sec:f4:pace", 15)? {
                return Ok(());
            }
            let slot = if attempts == 0 { key.clone() } else { format!("{key}:attempt:{}", attempts + 1) };
            if !self.claim(&slot, 30 * DAY)? {
                if attempts == 0 {
                    // Another instance owns this immutable document. No local details are
                    // available; don't advertise an endless backlog or retry its XML.
                    self.store.set(&attempts_key, 3)?;
                }
                continue;
            }
            if attempts == 0 && self.transport.increment_counter(&format!("{key}:initial"), 1)? > 1 {
                // A 30-day lease expiring must not make an already-read form fresh again
                // in a throwaway data directory. This durable tombstone is never cleared.
                self.store.set(&attempts_key, 3)?;
                return Ok(());
            }
            let attempts = self.transport.increment_counter(&format!("{key}:attempts"), attempts + 1)?;
            self.store.set(&attempts_key, attempts)?;
            if attempts > 3 {
                continue;
            }
            // Fixtures reserve only after a successful claim, then refund unsent refusals.
            // Sec preflights here; its guard charges the real counter atomically per HTTP
            // hop at send time. Never refund that counter for a sent response (403, etc.).
            if !self.transport.reserve_counter(&day_key, self.store.number(&day_key)? + 1, 300)? {
                self.refund_form(&key, &slot, &attempts_key, attempts - 1)?;
                return Ok(());
            }
            self.store.set(&day_key, self.store.number(&day_key)? + 1)?;
            let retry_key = format!("f4:retry:{}", f.accession);
            // Write ahead of the await: a cancelled request still consumes its attempt and
            // backs off for an hour. The worker normally lets the whole tick finish.
            self.store.set(&retry_key, now + 3600)?;
            let result = self.transport.get(&crate::insider::xml_url(&f).unwrap(), None, None).await;
            match result {
                Ok(reply) => {
                    match crate::insider::parse_document(&reply.body) {
                        Ok(document) => self.store.save_insiders(&f, &document)?,
                        Err(_) => self.store.set(&format!("f4:terminal:{}", f.accession), 1)?,
                    }
                    // Immutable parse/decode/mismatch results are terminal, not source failures.
                    self.store.remove(&retry_key)?;
                }
                Err(Refused::Http(_) | Refused::Transport) => {
                    let backoff = [3600, 21600, 86400][(attempts - 1).min(2) as usize];
                    self.store.set(&retry_key, now + backoff)?;
                }
                Err(reason) => {
                    self.store.remove(&retry_key)?;
                    self.refund_form(&key, &slot, &attempts_key, attempts - 1)?;
                    self.transport.decrement_counter(&day_key)?;
                    self.store.set(&day_key, (self.store.number(&day_key)? - 1).max(0))?;
                    return Err(reason.into());
                }
            }
            return Ok(());
        }
        Ok(())
    }
    fn refund_form(&self, key: &str, slot: &str, attempts_key: &str, previous: i64) -> Result<()> {
        self.transport.decrement_counter(&format!("{key}:attempts"))?;
        if previous == 0 {
            self.transport.decrement_counter(&format!("{key}:initial"))?;
        }
        self.transport.release_slot(slot)?;
        self.store.set(attempts_key, previous)?;
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
        // Never-attempted companies (including every company on a fresh install, where no previous
        // watched signature exists) get their first import a minute apart, not one sweep slot apart.
        // Once attempted, a failing company follows its backoff and the regular sweep instead.
        let has_new = watched.keys().any(|&cik| {
            let number = |key: String| self.store.number(&key).unwrap_or(0);
            number(format!("attempt:{cik}")) == 0
                && number(format!("pending_dropped:{cik}")) == 0
                && self.company_due(cik, now).unwrap_or(false)
        });
        let gap = if has_new { 60 } else { 144 };
        if (self.store.number("next_sweep")? > now && !has_new) || self.store.number("last_import")? + gap > now {
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
                && self.claim("sec:sweep", gap)?
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
