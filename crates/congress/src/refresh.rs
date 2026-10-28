//! Incremental official-site House ingest, guarded machine-wide by tr_net.
//! No live senate.gov requests. Senate's third-party ingest lives in [`crate::bargo`].

use std::fmt;
use std::future::Future;
#[cfg(test)]
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate, Utc};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::db::{Db, FilingRecord, FilingStatus, NewPolitician};
use crate::house::{self, IndexEntry, PdfError};
use crate::model::{Chamber, Party};
use crate::roster::{self, Roster};
use crate::senate::{self, ReportKind, ReportRef};

/// Senate's roster and eFD share a pause; the two House hosts are independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    Senate,
    House,
    HouseRoster,
}

impl Site {
    const ALL: [Self; 3] = [Self::Senate, Self::House, Self::HouseRoster];

    fn key(self) -> &'static str {
        match self {
            Self::Senate => "senate.gov",
            Self::House => "disclosures-clerk.house.gov",
            Self::HouseRoster => "clerk.house.gov",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Senate => "Senate",
            Self::House => "House",
            Self::HouseRoster => "House roster",
        }
    }

    fn for_url(url: &str) -> Result<Self> {
        match reqwest::Url::parse(url)?.host_str() {
            Some("www.senate.gov" | "senate.gov" | "efdsearch.senate.gov") => Ok(Self::Senate),
            Some("disclosures-clerk.house.gov") => Ok(Self::House),
            Some("clerk.house.gov") => Ok(Self::HouseRoster),
            _ => bail!("unsupported Congress source host"),
        }
    }
}

/// A refusal is not a transient parsing or transport failure. Sources must not retry it.
#[derive(Debug, Clone)]
pub struct SiteRefused {
    pub site: Site,
    /// Absolute UTC seconds, from either form of Retry-After.
    pub retry_after: Option<i64>,
}

impl fmt::Display for SiteRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: site refused requests", self.site.key())
    }
}

impl std::error::Error for SiteRefused {}

fn pause_text(site: Site, until: i64) -> String {
    let now = Utc::now();
    let bounded = until.clamp(now.timestamp(), now.timestamp() + 7 * 86400);
    let local = chrono::DateTime::from_timestamp(bounded, 0).unwrap_or(now).with_timezone(&chrono::Local);
    format!("{} source paused until {}: site refused requests", site.label(), local.format("%Y-%m-%d %H:%M %Z"))
}

/// Available at startup, even when automatic refresh is disabled.
pub fn paused_sources(db: &Db) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for site in Site::ALL {
        let (until, _) = db.source_cooldown(site.key())?;
        if until > Utc::now().timestamp() {
            out.push(pause_text(site, until));
        }
    }
    Ok(out)
}

/// Shared by the scheduler and the app's freshness display. An explicit zero disables.
pub fn refresh_minutes(provider: Option<&str>, configured: Option<&str>) -> u64 {
    let sample = provider.map(str::trim).filter(|v| !v.is_empty()).is_some_and(|v| v.eq_ignore_ascii_case("sample"));
    let default = if sample { 0 } else { 30 };
    let Some(raw) = configured.map(str::trim).filter(|v| !v.is_empty()) else { return default };
    if !raw.bytes().all(|b| b.is_ascii_digit()) {
        return default;
    }
    // Checked decimal parsing: even an overflowing numeric value is capped, never wrapped.
    let minutes =
        raw.bytes().try_fold(0_u64, |n, b| n.checked_mul(10)?.checked_add(u64::from(b - b'0'))).unwrap_or(u64::MAX);
    if minutes == 0 {
        return 0;
    }
    let bounded = minutes.clamp(15, 7 * 24 * 60);
    if bounded != minutes {
        warn!("TR_CONGRESS_REFRESH_MIN={raw} clamped to {bounded} minutes");
    }
    bounded
}

/// Where filings come from. [`HttpSource`] talks only to guarded House hosts;
/// fixture sources can still ingest both chambers without network access.
pub const SENATE_PAUSED: &str = "Senate: paused — needs a third-party source (senate.gov blocked this network)";

pub trait Source: Sync {
    /// Live HTTP has no Senate plan and delegates all pacing/refusals to tr_net.
    fn guarded_house_only(&self) -> bool {
        false
    }
    /// Install persisted pauses before any requests. Live sources also clear run-local refusals.
    fn begin_run(&self, _blocked: &[Site]) -> impl Future<Output = ()> + Send {
        async {}
    }
    /// Fetch only this site's roster, never both: the other host may be paused.
    fn roster_for(&self, site: Site) -> impl Future<Output = Result<Roster>> + Send;
    fn house_index(&self, year: i32) -> impl Future<Output = Result<Vec<IndexEntry>>> + Send;
    fn house_pdf(&self, entry: &IndexEntry) -> impl Future<Output = Result<Vec<u8>>> + Send;
    fn senate_reports(&self, since: Option<NaiveDate>) -> impl Future<Output = Result<Vec<ReportRef>>> + Send;
    fn senate_ptr(&self, report: &ReportRef) -> impl Future<Output = Result<String>> + Send;
}

#[derive(Debug, Clone)]
pub struct RefreshConfig {
    /// Index years for fixture imports. Live HTTP uses current year plus a once-per-year rollover check.
    pub house_years: Vec<i32>,
    /// House discovery lower bound, independent of the Senate window.
    pub house_since: Option<NaiveDate>,
    /// Senate discovery lower bound; also sent to Senate search.
    pub senate_since: Option<NaiveDate>,
    /// Cap per run; defaults to 50. Live House plans enforce a hard maximum of 50.
    pub max_new_per_chamber: usize,
    /// Retained for callers; official-site fetches are always sequential.
    pub concurrency: usize,
}

impl RefreshConfig {
    /// Defaults derived independently from each chamber's last complete discovery.
    pub fn incremental(db: &Db) -> Self {
        Self::incremental_at(db, Utc::now().date_naive())
    }

    fn incremental_at(db: &Db, today: NaiveDate) -> Self {
        let first_run = today - chrono::Duration::days(90);
        let windows = (|| -> Result<_> {
            db.migrate_refresh_dates()?;
            let since = |chamber, key| -> Result<NaiveDate> {
                let last: Option<NaiveDate> =
                    if db.has_filings(chamber)? { db.meta(key)?.and_then(|s| s.parse().ok()) } else { None };
                // Re-scan a week back: filings can appear in the index late.
                Ok(last.map(|d| d - chrono::Duration::days(7)).unwrap_or(first_run))
            };
            Ok((since(Chamber::House, "last_refresh_date_house")?, since(Chamber::Senate, "last_refresh_date_senate")?))
        })();
        let (house_since, senate_since) = windows.unwrap_or_else(|e| {
            warn!("discovery state unavailable; using first-run windows: {e:#}");
            (first_run, first_run)
        });
        Self {
            house_years: vec![today.year()],
            house_since: Some(house_since),
            senate_since: Some(senate_since),
            max_new_per_chamber: 50,
            concurrency: 1,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewTradeAlert {
    pub politician_id: i64,
    pub politician: String,
    pub filing_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct RefreshReport {
    pub new_filings: usize,
    pub new_trades: usize,
    pub scanned: usize,
    pub errors: Vec<String>,
    pub alerts: Vec<NewTradeAlert>,
    pub paused: Vec<String>,
    pub capped: bool,
    pub parse_errors: Vec<String>,
    pub notes: Vec<String>,
    /// House and Senate filing/trade counts, including work before a refusal.
    pub chamber_counts: [(usize, usize); 2],
}

impl RefreshReport {
    pub fn summary(&self) -> String {
        if !self.paused.is_empty() {
            let mut parts: Vec<String> = ["House", "Senate"]
                .iter()
                .enumerate()
                .filter(|(i, label)| {
                    (**label != "Senate" || !self.notes.iter().any(|note| note == SENATE_PAUSED))
                        && (self.chamber_counts[*i] != (0, 0)
                            || !self.paused.iter().any(|p| p.starts_with(&format!("{label} source"))))
                })
                .map(|(i, label)| {
                    format!("{label}: {} new filings, {} trades", self.chamber_counts[i].0, self.chamber_counts[i].1)
                })
                .collect();
            parts.extend(self.paused.clone());
            if !self.parse_errors.is_empty() {
                parts.push(format!("{} parse errors", self.parse_errors.len()));
            }
            parts.extend(self.errors.clone());
            parts.extend(self.notes.clone());
            return parts.join(" · ");
        }
        let counts = format!(
            "{} new filings, {} trades, {} scanned/paper, {} errors, {} alerts",
            self.new_filings,
            self.new_trades,
            self.scanned,
            self.errors.len() + self.parse_errors.len(),
            self.alerts.len()
        );
        std::iter::once(counts).chain(self.errors.clone()).chain(self.notes.clone()).collect::<Vec<_>>().join(" · ")
    }
}

fn is_transport(error: &anyhow::Error) -> bool {
    error.chain().any(|e| {
        e.downcast_ref::<reqwest::Error>()
            .is_some_and(|e| e.is_timeout() || e.is_connect() || e.is_request() || e.is_body())
    }) || ["connection reset", "connection closed", "connection refused", "timeout", "timed out", "connect error"]
        .iter()
        .any(|s| format!("{error:#}").to_ascii_lowercase().contains(s))
}

#[derive(Default)]
struct TransportFailures([u8; 3]);

impl TransportFailures {
    fn observe<T>(&mut self, site: Site, result: Result<T>, report: &mut RefreshReport) -> Result<T> {
        // Guard refusals (including transport streaks) already have machine-wide state.
        if result.as_ref().is_err_and(|e| e.is::<tr_net::Refused>()) {
            return result;
        }
        let count = &mut self.0[site as usize];
        if let Err(e) = &result
            && is_transport(e)
        {
            *count += 1;
            if *count >= 3 {
                report.errors.push(format!("{}: {e:#}", site.label()));
                return Err(SiteRefused { site, retry_after: None }.into());
            }
            return result;
        }
        *count = 0;
        result
    }
}

struct Parsed {
    /// Fetch errors are persisted like parse errors, but reported separately.
    fetch_failed: bool,
    record_paper: bool,
    status: FilingStatus,
    error: Option<String>,
    filed: Option<NaiveDate>,
    trades: Vec<crate::model::ParsedTrade>,
}

/// Run one refresh cycle against `src`, writing into `db`.
pub async fn refresh<S: Source>(src: &S, db: &Mutex<Db>, cfg: &RefreshConfig) -> Result<RefreshReport> {
    // Also migrate for callers supplying an explicit config, before any filings are added.
    db.lock().await.migrate_refresh_dates()?;
    let today = Utc::now().date_naive();
    let mut report = RefreshReport::default();
    let mut failures = TransportFailures::default();
    let mut incomplete_rosters = Vec::new();
    let live = src.guarded_house_only();
    if live {
        report.notes.push(SENATE_PAUSED.into());
    }
    let mut blocked = Vec::new();
    for site in Site::ALL.into_iter().filter(|_| !live) {
        let (until, _) = db.lock().await.source_cooldown(site.key())?;
        if until > Utc::now().timestamp() {
            blocked.push(site);
        }
    }
    let already_blocked = blocked.len();
    src.begin_run(&blocked).await;
    let mut house = Vec::new();
    let mut senate = Vec::new();
    for site in [Site::HouseRoster, Site::Senate].into_iter().filter(|s| !live || *s != Site::Senate) {
        if blocked.contains(&site) {
            incomplete_rosters.push(site);
            continue;
        }
        match failures.observe(site, src.roster_for(site).await, &mut report) {
            Ok(r) => {
                house.extend(r.house);
                senate.extend(r.senate);
                if site == Site::HouseRoster && !live {
                    db.lock().await.reset_source(site.key())?;
                }
            }
            Err(e) => {
                incomplete_rosters.push(site);
                record_failure(db, &mut report, site, e, &mut blocked).await?;
            }
        }
    }
    let roster = Roster::new(house, senate);

    if !roster.is_empty() {
        let members: Vec<_> = roster
            .house
            .iter()
            .chain(&roster.senate)
            .filter_map(|m| m.bioguide.clone().map(|b| (b, m.name.clone(), m.party, Some(m.state.clone()))))
            .collect();
        match db.lock().await.apply_roster(&members) {
            Ok(n) if n > 0 => info!("merged {n} duplicate politician rows via roster"),
            Ok(_) => {}
            Err(e) => {
                incomplete_rosters.extend([Site::HouseRoster, Site::Senate]);
                report.errors.push(format!("roster sync failed: {e:#}"));
            }
        }
    }
    for site in [Site::House, Site::Senate].into_iter().filter(|s| !live || *s != Site::Senate) {
        if blocked.contains(&site) {
            continue;
        }
        let errors_before = report.errors.len();
        let result = match site {
            Site::House => refresh_house(src, db, cfg, &roster, &mut report, &mut failures).await,
            Site::Senate => refresh_senate(src, db, cfg, &roster, &mut report, &mut failures).await,
            Site::HouseRoster => unreachable!(),
        };
        match result {
            Ok(capped) => {
                let roster_site = if site == Site::House { Site::HouseRoster } else { Site::Senate };
                if report.errors.len() == errors_before && !incomplete_rosters.contains(&roster_site) {
                    let db = db.lock().await;
                    if !live {
                        db.reset_source(site.key())?;
                    }
                    if !capped {
                        let key =
                            if site == Site::House { "last_refresh_date_house" } else { "last_refresh_date_senate" };
                        db.set_meta(key, &today.to_string())?;
                    }
                }
            }
            Err(e) => record_failure(db, &mut report, site, e, &mut blocked).await?,
        }
    }

    let db = db.lock().await;
    if !live {
        report.paused = paused_sources(&db)?;
    }
    if blocked.len() > already_blocked {
        warn!("{}", report.paused.join(" · "));
    }
    // The legacy last_refresh_date is intentionally left untouched.
    db.set_meta("last_refresh_summary", &report.summary())?;
    info!("congress refresh: {}", report.summary());
    Ok(report)
}

/// Ordinary HTTP / body errors belong to one filing. All other guard failures
/// fail closed for the run; pauses themselves are imposed by the guard.
fn source_wide_failure(error: &anyhow::Error) -> bool {
    if error.is::<SiteRefused>() {
        return true;
    }
    match error.downcast_ref::<tr_net::Refused>() {
        Some(tr_net::Refused::Http(status)) => matches!(status, 403 | 429 | 503),
        Some(tr_net::Refused::BodyTooLarge) | None => false,
        Some(_) => true,
    }
}

async fn record_failure(
    db: &Mutex<Db>,
    report: &mut RefreshReport,
    site: Site,
    error: anyhow::Error,
    blocked: &mut Vec<Site>,
) -> Result<()> {
    if let Some(refusal) = error.downcast_ref::<tr_net::Refused>() {
        match refusal {
            tr_net::Refused::Paused { until } => report.paused.push(pause_text(site, *until)),
            _ => report.errors.push(format!("{}: {refusal}", site.label())),
        }
        if source_wide_failure(&error) {
            blocked.push(site);
        }
    } else if let Some(refusal) = error.downcast_ref::<SiteRefused>() {
        if !blocked.contains(&refusal.site) {
            db.lock().await.refuse_source(refusal.site.key(), Utc::now().timestamp(), refusal.retry_after)?;
            blocked.push(refusal.site);
        }
    } else {
        warn!("{} refresh failed: {error:#}", site.label());
        report.errors.push(format!("{}: {error:#}", site.label()));
    }
    Ok(())
}

async fn store(
    db: &Mutex<Db>,
    report: &mut RefreshReport,
    politician: NewPolitician,
    record: impl FnOnce(i64) -> FilingRecord,
    parsed: Parsed,
) -> Result<()> {
    let mut db = db.lock().await;
    let pid = db.upsert_politician(&politician)?;
    let mut rec = record(pid);
    rec.paper = parsed.record_paper;
    rec.status = parsed.status;
    rec.error = parsed.error.clone();
    if let Some(f) = parsed.filed {
        rec.filed_date = f;
    }
    let n = db.insert_filing(&rec, &parsed.trades)?;
    report.new_filings += 1;
    report.new_trades += n;
    let chamber = usize::from(rec.chamber == Chamber::Senate);
    report.chamber_counts[chamber].0 += 1;
    report.chamber_counts[chamber].1 += n;
    match parsed.status {
        FilingStatus::Scanned => report.scanned += 1,
        FilingStatus::Error if parsed.fetch_failed => report.errors.push(format!(
            "{}: 1 filing failed ({})",
            rec.chamber.as_str(),
            parsed.error.unwrap_or_default()
        )),
        FilingStatus::Error => report.parse_errors.push(format!("{}: {}", rec.id, parsed.error.unwrap_or_default())),
        FilingStatus::Parsed => {}
    }
    if db.is_tracked(pid)? && parsed.status != FilingStatus::Error {
        let tickers: Vec<String> = {
            let mut t: Vec<String> = parsed
                .trades
                .iter()
                .map(|t| {
                    let what = t.ticker.clone().unwrap_or_else(|| t.asset_name.chars().take(18).collect());
                    format!("{} {what}", t.tx_type.marker())
                })
                .collect();
            t.dedup();
            t.truncate(6);
            t
        };
        let message = if parsed.trades.is_empty() {
            format!(
                "{} filed a new PTR ({}) on {}",
                politician.name,
                if rec.paper { "paper" } else { "scanned" },
                rec.filed_date
            )
        } else {
            format!("{} filed {} trade(s): {}", politician.name, parsed.trades.len(), tickers.join(", "))
        };
        if db.insert_alert(pid, &rec.id, &message)?.is_some() {
            report.alerts.push(NewTradeAlert {
                politician_id: pid,
                politician: politician.name.clone(),
                filing_id: rec.id.clone(),
                message,
            });
        }
    }
    Ok(())
}

async fn refresh_house<S: Source>(
    src: &S,
    db: &Mutex<Db>,
    cfg: &RefreshConfig,
    roster: &Roster,
    report: &mut RefreshReport,
    failures: &mut TransportFailures,
) -> Result<bool> {
    let known = db.lock().await.known_filing_ids(Chamber::House)?;
    let retries = db.lock().await.retry_filing_ids(Chamber::House)?;
    let live = src.guarded_house_only();
    let current_year = Utc::now().year();
    let years = if live { vec![current_year, current_year - 1] } else { cfg.house_years.clone() };
    let rollover_key = format!("house:rollover:{current_year}");
    let baseline_key = format!("house:skipped-initial:{current_year}");
    let skipped = db.lock().await.meta(&baseline_key)?;
    let mut todo: Vec<IndexEntry> = Vec::new();
    for &year in &years {
        let rollover = live && year == current_year - 1;
        let saved = if rollover { db.lock().await.meta(&rollover_key)? } else { None };
        let entries = if let Some(json) = saved {
            serde_json::from_str(&json).context("House rollover snapshot")?
        } else {
            let mut entries = match failures.observe(Site::House, src.house_index(year).await, report) {
                Ok(entries) => entries,
                Err(e) if e.is::<IndexNotPublished>() => {
                    report.notes.push(format!("House {year} index not published yet; skipped"));
                    vec![]
                }
                Err(e) => return Err(e).with_context(|| format!("house index {year}")),
            };
            if rollover {
                // One guarded prior-year index check per calendar year. Retain only a
                // bounded recent snapshot so capped work and retries survive restart
                // without fetching that index again or opening a historical backfill.
                entries.retain(|e| {
                    e.is_ptr() && cfg.house_since.is_none_or(|since| e.filing_date.is_some_and(|d| d >= since))
                });
                entries.sort_by_key(|e| std::cmp::Reverse(e.filing_date));
                entries.truncate(50);
                db.lock().await.set_meta(&rollover_key, &serde_json::to_string(&entries)?)?;
            }
            entries
        };
        todo.extend(entries.into_iter().filter(|e| {
            e.is_ptr()
                && (retries.contains(&format!("house:{}", e.doc_id))
                    || (!known.contains(&format!("house:{}", e.doc_id))
                        && ((live && skipped.is_none())
                            || cfg.house_since.is_none_or(|since| e.filing_date.is_some_and(|d| d >= since)))))
        }));
    }
    let mut seen = std::collections::HashSet::new();
    todo.retain(|entry| seen.insert(entry.doc_id.clone()));
    // Newest first so a capped run still gets the freshest filings.
    todo.sort_by_key(|e| std::cmp::Reverse(e.filing_date));
    let limit = cfg.max_new_per_chamber.min(if live { 50 } else { 200 });
    if live {
        // Persist the omitted initial index, before downloading anything. Future runs
        // must not drain that historical backlog, even after cancellation/restart.
        let omitted: std::collections::HashSet<String> = match skipped {
            Some(json) => serde_json::from_str(&json).context("House initial baseline")?,
            None => {
                let omitted: std::collections::HashSet<_> = todo.iter().skip(limit).map(|e| e.doc_id.clone()).collect();
                db.lock().await.set_meta(&baseline_key, &serde_json::to_string(&omitted)?)?;
                omitted
            }
        };
        todo.retain(|e| !omitted.contains(&e.doc_id));
    }
    let capped = todo.len() > limit;
    report.capped |= capped;
    todo.truncate(limit);
    debug!("house: {} new PTRs to fetch", todo.len());

    for entry in todo {
        let mut stop = None;
        let parsed = match failures.observe(Site::House, src.house_pdf(&entry).await, report) {
            Err(e) => {
                let id = format!("house:{}", entry.doc_id);
                let message = format!("{e:#}");
                let source_wide = source_wide_failure(&e);
                if retries.contains(&id) {
                    // Charge before any early return, including guard HTTP / pause errors.
                    db.lock().await.record_failed_retry(&id)?;
                    report.errors.push(format!("House: 1 filing failed ({message})"));
                    if source_wide {
                        return Err(e);
                    }
                    continue;
                }
                if source_wide {
                    stop = Some(e);
                }
                // Persist initial fetch failures too: otherwise they bypass retry budgets.
                Parsed {
                    fetch_failed: true,
                    record_paper: false,
                    status: FilingStatus::Error,
                    error: Some(message),
                    filed: entry.filing_date,
                    trades: vec![],
                }
            }
            Ok(bytes) => {
                let bytes_owned = bytes;
                match tokio::task::spawn_blocking(move || house::parse_ptr_pdf(&bytes_owned)).await? {
                    Ok(ptr) => Parsed {
                        fetch_failed: false,
                        record_paper: false,
                        status: FilingStatus::Parsed,
                        error: None,
                        filed: entry.filing_date.or(ptr.signed_date),
                        trades: ptr.trades,
                    },
                    Err(PdfError::Scanned) => Parsed {
                        fetch_failed: false,
                        record_paper: true,
                        status: FilingStatus::Scanned,
                        error: None,
                        filed: entry.filing_date,
                        trades: vec![],
                    },
                    Err(e) => Parsed {
                        fetch_failed: false,
                        record_paper: false,
                        status: FilingStatus::Error,
                        error: Some(e.to_string()),
                        filed: entry.filing_date,
                        trades: vec![],
                    },
                }
            }
        };
        let member = roster
            .house_by_district(&entry.state_district)
            .filter(|m| roster::normalize_name(&m.last).last() == roster::normalize_name(&entry.last).last())
            .or_else(|| roster.find_by_name(Chamber::House, &entry.first, &entry.last));
        let politician = NewPolitician {
            // The filing index has typos ("Scott Scott Franklin"); prefer the roster's name.
            name: member.map(|m| m.name.clone()).unwrap_or_else(|| entry.display_name()),
            chamber: Chamber::House,
            party: member.map(|m| m.party).unwrap_or(Party::Unknown),
            state: entry.state(),
            district: Some(entry.state_district.clone()).filter(|s| !s.is_empty()),
            bioguide: member.and_then(|m| m.bioguide.clone()),
        };
        let filed = entry.filing_date.unwrap_or_else(|| Utc::now().date_naive());
        let url = entry.pdf_url();
        let id = format!("house:{}", entry.doc_id);
        store(
            db,
            report,
            politician,
            |pid| FilingRecord {
                id,
                chamber: Chamber::House,
                politician_id: pid,
                filed_date: filed,
                url,
                paper: false,
                status: FilingStatus::Parsed,
                error: None,
                is_amendment: false,
            },
            parsed,
        )
        .await?;
        if let Some(error) = stop {
            return Err(error);
        }
    }
    Ok(capped)
}

async fn refresh_senate<S: Source>(
    src: &S,
    db: &Mutex<Db>,
    cfg: &RefreshConfig,
    roster: &Roster,
    report: &mut RefreshReport,
    failures: &mut TransportFailures,
) -> Result<bool> {
    let known = db.lock().await.known_filing_ids(Chamber::Senate)?;
    let retries = db.lock().await.retry_filing_ids(Chamber::Senate)?;
    let mut todo: Vec<ReportRef> = failures
        .observe(Site::Senate, src.senate_reports(cfg.senate_since).await, report)?
        .into_iter()
        .filter(|r| {
            retries.contains(&format!("senate:{}", r.id))
                || (!known.contains(&format!("senate:{}", r.id))
                    && cfg.senate_since.is_none_or(|since| r.date_received.is_some_and(|d| d >= since)))
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    todo.retain(|report| seen.insert(report.id.clone()));
    todo.sort_by_key(|r| std::cmp::Reverse(r.date_received));
    let capped = todo.len() > cfg.max_new_per_chamber.min(200);
    report.capped |= capped;
    todo.truncate(cfg.max_new_per_chamber.min(200));
    debug!("senate: {} new PTRs to fetch", todo.len());

    for r in todo {
        let html = match r.kind {
            ReportKind::Electronic => src.senate_ptr(&r).await.map(Some),
            ReportKind::Paper => Ok(None),
        };
        let parsed = match failures.observe(Site::Senate, html, report) {
            Err(e) => {
                let id = format!("senate:{}", r.id);
                if retries.contains(&id) {
                    db.lock().await.record_failed_retry(&id)?;
                }
                if e.is::<SiteRefused>() {
                    return Err(e);
                }
                report.errors.push(format!("senate:{} fetch: {e:#}", r.id));
                continue;
            }
            Ok(None) => Parsed {
                fetch_failed: false,
                record_paper: true,
                status: FilingStatus::Scanned,
                error: None,
                filed: r.date_received,
                trades: vec![],
            },
            Ok(Some(html)) => match senate::parse_ptr_html(&html) {
                Ok(p) => Parsed {
                    fetch_failed: false,
                    record_paper: false,
                    status: FilingStatus::Parsed,
                    error: None,
                    filed: p.filed_date.or(r.date_received),
                    trades: p.trades,
                },
                Err(e) => Parsed {
                    fetch_failed: false,
                    record_paper: false,
                    status: FilingStatus::Error,
                    error: Some(format!("{e:#}")),
                    filed: r.date_received,
                    trades: vec![],
                },
            },
        };
        let member = roster.find_by_name(Chamber::Senate, &r.first, &r.last);
        let politician = NewPolitician {
            name: member.map(|m| m.name.clone()).unwrap_or_else(|| r.display_name()),
            chamber: Chamber::Senate,
            party: member.map(|m| m.party).unwrap_or(Party::Unknown),
            state: member.map(|m| m.state.clone()),
            district: None,
            bioguide: member.and_then(|m| m.bioguide.clone()),
        };
        let filed = r.date_received.unwrap_or_else(|| Utc::now().date_naive());
        let (id, url, amend) = (format!("senate:{}", r.id), r.url.clone(), r.is_amendment());
        store(
            db,
            report,
            politician,
            |pid| FilingRecord {
                id,
                chamber: Chamber::Senate,
                politician_id: pid,
                filed_date: filed,
                url,
                paper: false,
                status: FilingStatus::Parsed,
                error: None,
                is_amendment: amend,
            },
            parsed,
        )
        .await?;
    }
    Ok(capped)
}

#[derive(Debug)]
struct IndexNotPublished;
impl fmt::Display for IndexNotPublished {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("House index not published yet")
    }
}
impl std::error::Error for IndexNotPublished {}

#[cfg(test)]
fn retry_after(value: &str, now: i64) -> Option<i64> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|s| now.saturating_add(s.min(i64::MAX as u64) as i64))
        .or_else(|| chrono::DateTime::parse_from_rfc2822(value).ok().map(|d| d.timestamp()))
        .map(|until| until.min(now.saturating_add(7 * 86400)))
}

#[cfg(test)]
fn check_response(site: Site, status: u16, body: &[u8], retry: Option<&str>) -> Result<()> {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    if matches!(status, 403 | 429 | 503)
        || text.contains("potential security risk")
        || text.contains("sorry, a potential")
        || text.contains("access denied")
        || text.contains("verify you are human")
        || text.contains("checking your browser")
        || text.contains("captcha")
    {
        return Err(
            SiteRefused { site, retry_after: retry.and_then(|v| retry_after(v, Utc::now().timestamp())) }.into()
        );
    }
    if status >= 400 {
        bail!("{}: HTTP {status}", site.key());
    }
    Ok(())
}

#[cfg(test)]
fn home_csrf(home: &str) -> Result<String> {
    senate::csrf_from_form(home).ok_or_else(|| SiteRefused { site: Site::Senate, retry_after: None }.into())
}

/// Live House-only source. No Senate URL or session is constructed here.
/// The injected guard is used only by localhost tests; production always uses shared().
pub struct HttpSource {
    client: reqwest::Client,
    guard: Option<std::sync::Arc<tr_net::Guard>>,
    index: Mutex<Option<(i32, Vec<IndexEntry>, tr_net::ResponseMetadata)>>,
    #[cfg(test)]
    planned_urls: std::sync::Mutex<Vec<String>>,
}

impl HttpSource {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::new(),
            guard: None,
            index: Mutex::new(None),
            #[cfg(test)]
            planned_urls: Default::default(),
        })
    }

    fn guard(&self) -> Result<&tr_net::Guard, tr_net::Refused> {
        match self.guard.as_deref() {
            Some(guard) => Ok(guard),
            None => tr_net::shared(),
        }
    }

    /// A test guard must have a loopback-only transport (Guard::for_fixture).
    #[cfg(test)]
    fn with_guard(guard: std::sync::Arc<tr_net::Guard>) -> Self {
        Self {
            client: reqwest::Client::new(),
            guard: Some(guard),
            index: Mutex::new(None),
            planned_urls: Default::default(),
        }
    }

    fn request(&self, url: &str) -> Result<(&'static str, reqwest::RequestBuilder)> {
        // Classify before constructing a RequestBuilder. Unknown and Senate URLs cannot escape.
        let group = match Site::for_url(url)? {
            Site::House => "house-disclosures",
            Site::HouseRoster => "house-roster",
            Site::Senate => return Err(tr_net::Refused::Disabled.into()),
        };
        #[cfg(test)]
        self.planned_urls.lock().unwrap().push(url.to_owned());
        Ok((group, self.client.get(url)))
    }

    async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let (group, request) = self.request(url)?;
        Ok(self.guard()?.send(group, request).await?)
    }
}

impl Source for HttpSource {
    fn guarded_house_only(&self) -> bool {
        true
    }

    async fn roster_for(&self, site: Site) -> Result<Roster> {
        match site {
            Site::Senate => Err(tr_net::Refused::Disabled.into()),
            Site::HouseRoster => {
                // The guard caches successful roster bodies for a day in process;
                // independent processes still share the 20/day ledger ceiling.
                let bytes = self.get_bytes(roster::HOUSE_ROSTER_URL).await?;
                Ok(Roster::new(roster::parse_house_roster(&String::from_utf8_lossy(&bytes))?, vec![]))
            }
            Site::House => Ok(Roster::default()),
        }
    }

    async fn house_index(&self, year: i32) -> Result<Vec<IndexEntry>> {
        // The previous year is allowed only for the bounded annual rollover plan.
        let current = Utc::now().year();
        anyhow::ensure!((current - 1..=current).contains(&year), "Live House discovery is current/previous-year only");
        let mut cached = self.index.lock().await;
        let (_, mut request) = self.request(&house::index_zip_url(year))?;
        if let Some((_, _, meta)) = cached.as_ref().filter(|(y, _, _)| *y == year) {
            if let Some(etag) = &meta.etag {
                request = request.header("If-None-Match", etag);
            }
            if let Some(modified) = &meta.last_modified {
                request = request.header("If-Modified-Since", modified);
            }
        }
        let (zip, meta) = match self.guard()?.send_with_metadata("house-disclosures", request).await {
            Err(tr_net::Refused::Http(404)) => return Err(IndexNotPublished.into()),
            result => result?,
        };
        if meta.status == 304 {
            return cached
                .as_ref()
                .filter(|(y, _, _)| *y == year)
                .map(|(_, entries, _)| entries.clone())
                .context("House index 304 without cached entries");
        }
        let entries = tokio::task::spawn_blocking(move || house::parse_index_zip(&zip)).await??;
        *cached = Some((year, entries.clone(), meta));
        Ok(entries)
    }

    async fn house_pdf(&self, entry: &IndexEntry) -> Result<Vec<u8>> {
        let current = Utc::now().year();
        anyhow::ensure!(
            (current - 1..=current).contains(&entry.year),
            "Live House PDFs are current/previous-year only"
        );
        self.get_bytes(&entry.pdf_url()).await
    }

    async fn senate_reports(&self, _: Option<NaiveDate>) -> Result<Vec<ReportRef>> {
        Err(tr_net::Refused::Disabled.into())
    }

    async fn senate_ptr(&self, _: &ReportRef) -> Result<String> {
        Err(tr_net::Refused::Disabled.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};

    struct LocalServer {
        base: reqwest::Url,
        seen: Arc<StdMutex<Vec<String>>>,
        stop: Arc<std::sync::atomic::AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
    }
    impl LocalServer {
        fn new(index_status: u16) -> Self {
            Self::respond(move |request| {
                if request.contains("MemberData.xml") {
                    (200, String::new(), include_bytes!("../tests/fixtures/roster/house_members.xml").to_vec())
                } else {
                    (index_status, String::new(), b"not published".to_vec())
                }
            })
        }
        fn respond(handler: impl Fn(&str) -> (u16, String, Vec<u8>) + Send + 'static) -> Self {
            use std::io::{Read, Write};
            use std::sync::atomic::Ordering;
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let base = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
            let seen = Arc::new(StdMutex::new(Vec::new()));
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (requests, stopped) = (seen.clone(), stop.clone());
            let worker = std::thread::spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            // macOS: accepted sockets inherit the listener's non-blocking mode.
                            stream.set_nonblocking(false).unwrap();
                            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                            let mut bytes = Vec::new();
                            let mut buf = [0; 8192];
                            while !bytes.ends_with(b"\r\n\r\n") {
                                match stream.read(&mut buf) {
                                    Ok(0) | Err(_) => break,
                                    Ok(n) => bytes.extend_from_slice(&buf[..n]),
                                }
                            }
                            let request = String::from_utf8_lossy(&bytes).into_owned();
                            let (status, headers, body) = handler(&request);
                            requests.lock().unwrap().push(request);
                            let _ = write!(
                                stream,
                                "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(&body);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2))
                        }
                        Err(e) => panic!("fixture: {e}"),
                    }
                }
            });
            Self { base, seen, stop, worker: Some(worker) }
        }
        fn source(&self, path: &std::path::Path, offline: bool) -> HttpSource {
            let guard = tr_net::Guard::for_fixture(
                tr_net::Config {
                    ledger: path.to_owned(),
                    data_dir: Some(path.with_extension("instance")),
                    allow_external: !offline,
                    max_wait: Duration::ZERO,
                },
                tr_net::Settings::default(),
                self.base.clone(),
            )
            .unwrap();
            HttpSource::with_guard(Arc::new(guard))
        }
    }
    impl Drop for LocalServer {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            self.worker.take().unwrap().join().unwrap();
        }
    }
    fn ledger_file() -> TestDbFile {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        TestDbFile(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "guard-test-{}-{}.sqlite3",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )))
    }

    #[tokio::test]
    async fn http_all_plans_are_house_only_and_offline_constructs_no_senate_requests() {
        for offline in [false, true] {
            let server = LocalServer::new(404);
            let ledger = ledger_file();
            let source = server.source(&ledger.0, offline);
            let db = Mutex::new(Db::open_in_memory().unwrap());
            for years in [vec![], vec![1999], vec![Utc::now().year()], vec![1999, Utc::now().year(), 2099]] {
                let mut cfg = config();
                cfg.house_years = years;
                cfg.max_new_per_chamber = usize::MAX;
                let report = refresh(&source, &db, &cfg).await.unwrap();
                assert!(report.summary().contains(SENATE_PAUSED));
                assert!(report.paused.is_empty(), "only actual cooldowns are pauses: {:?}", report.paused);
                assert!(!report.errors.is_empty(), "HTTP 404 / Offline / Busy must be reported as errors");
            }
            assert!(source.roster_for(Site::Senate).await.unwrap_err().is::<tr_net::Refused>());
            assert!(source.senate_reports(None).await.unwrap_err().is::<tr_net::Refused>());
            let report = Fake::new(None).senate_reports(None).await.unwrap().remove(0);
            assert!(source.senate_ptr(&report).await.unwrap_err().is::<tr_net::Refused>());
            for url in
                [senate::HOME_URL, roster::SENATE_ROSTER_URL, "https://senate.gov/", "https://future.senate.gov/"]
            {
                assert!(source.get_bytes(url).await.is_err());
            }
            assert!(source.house_index(1999).await.is_err());
            let urls = source.planned_urls.lock().unwrap().clone();
            assert!(!urls.is_empty());
            for url in urls.iter() {
                let url = reqwest::Url::parse(url).unwrap();
                assert!(matches!(url.host_str(), Some("clerk.house.gov" | "disclosures-clerk.house.gov")));
                if url.path().ends_with("FD.zip") {
                    assert!(
                        [Utc::now().year(), Utc::now().year() - 1]
                            .iter()
                            .any(|year| url.path().contains(&year.to_string()))
                    );
                }
            }
            let sent = server.seen.lock().unwrap().clone();
            if offline {
                assert!(sent.is_empty(), "test data dir must never open a socket");
            } else {
                assert_eq!(sent.len(), 2, "one roster and one index; later immediate runs are paced");
                assert!(sent.iter().all(|r| r.contains("terminal-rs/") && !r.contains("Mozilla")));
                let status = source.guard().unwrap().status().unwrap();
                assert_eq!(status.iter().find(|s| s.group == "house-disclosures").unwrap().requests_today, 1);
                assert_eq!(status.iter().find(|s| s.group == "house-roster").unwrap().requests_today, 1);
            }
            // Live sources neither read nor write the obsolete data-dir cooldowns.
            assert_eq!(db.lock().await.source_cooldown(Site::House.key()).unwrap(), (0, 0));
        }
    }

    #[tokio::test]
    async fn http_refusal_is_durable_across_sources_and_new_data_dirs() {
        let server = LocalServer::new(429);
        let ledger = ledger_file();
        let first = server.source(&ledger.0, false);
        let report = refresh(&first, &Mutex::new(Db::open_in_memory().unwrap()), &config()).await.unwrap();
        assert!(
            report.summary().contains("House source paused until"),
            "{}; hits={:?}",
            report.summary(),
            server.seen.lock().unwrap()
        );
        let second = server.source(&ledger.0, false);
        let report = refresh(&second, &Mutex::new(Db::open_in_memory().unwrap()), &config()).await.unwrap();
        assert!(
            report.summary().contains("House source paused until"),
            "{}; hits={:?}",
            report.summary(),
            server.seen.lock().unwrap()
        );
        assert_eq!(server.seen.lock().unwrap().len(), 2, "only first roster + refused index");
    }

    #[tokio::test]
    async fn http_index_validators_and_pdf_use_the_guard() {
        use std::io::Write;
        let year = Utc::now().year();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file(format!("{year}FD.xml"), zip::write::SimpleFileOptions::default()).unwrap();
        let xml = include_str!("../tests/fixtures/house/2026FD.xml").replace("2026", &year.to_string());
        zip.write_all(xml.as_bytes()).unwrap();
        let zip = zip.finish().unwrap().into_inner();
        let server = LocalServer::respond(move |request| {
            if request.contains(".pdf ") {
                (200, String::new(), include_bytes!("../tests/fixtures/house/8221321.pdf").to_vec())
            } else if request.to_ascii_lowercase().contains("if-none-match: \"index-v1\"") {
                (304, String::new(), vec![])
            } else {
                (200, "ETag: \"index-v1\"\r\nLast-Modified: Tue, 27 Oct 2026 00:00:00 GMT\r\n".into(), zip.clone())
            }
        });
        let ledger = ledger_file();
        let source = server.source(&ledger.0, false);
        let entries = source.house_index(year).await.unwrap();
        assert!(!entries.is_empty());
        let age = || {
            rusqlite::Connection::open(&ledger.0).unwrap().execute("UPDATE last_request SET at=0", []).unwrap();
        };
        age();
        assert_eq!(source.house_index(year).await.unwrap(), entries);
        age();
        let entry = entries.iter().find(|e| e.is_ptr()).unwrap();
        assert!(source.house_pdf(entry).await.unwrap().starts_with(b"%PDF-"));
        let sent = server.seen.lock().unwrap();
        assert_eq!(sent.len(), 3);
        assert!(sent[1].to_ascii_lowercase().contains("if-none-match: \"index-v1\""));
        assert!(sent[1].to_ascii_lowercase().contains("if-modified-since:"));
        assert!(sent[2].contains(".pdf "));
        assert!(sent.iter().all(|r| r.contains("terminal-rs/") && !r.contains("Mozilla")));
        assert_eq!(
            source
                .guard()
                .unwrap()
                .status()
                .unwrap()
                .iter()
                .find(|s| s.group == "house-disclosures")
                .unwrap()
                .requests_today,
            3
        );
    }

    #[tokio::test]
    async fn stored_senate_trades_remain_visible_while_live_senate_is_paused() {
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let mut fixtures = Fake::new(None);
        fixtures.count = 1;
        refresh(&fixtures, &db, &config()).await.unwrap();
        let before =
            db.lock().await.latest_trades(1000).unwrap().into_iter().filter(|t| t.chamber == Chamber::Senate).count();
        assert!(before > 0);
        let mut live = Fake::new(None);
        live.count = 0;
        let report = refresh(&HousePlan(live), &db, &config()).await.unwrap();
        assert!(report.summary().contains(SENATE_PAUSED));
        let after =
            db.lock().await.latest_trades(1000).unwrap().into_iter().filter(|t| t.chamber == Chamber::Senate).count();
        assert_eq!(after, before);
    }

    /// Same production plan, fixture bodies only: a new data directory never drains history.
    struct HousePlan(Fake);
    impl Source for HousePlan {
        fn guarded_house_only(&self) -> bool {
            true
        }
        async fn roster_for(&self, site: Site) -> Result<Roster> {
            assert_eq!(site, Site::HouseRoster);
            self.0.roster_for(site).await
        }
        async fn house_index(&self, year: i32) -> Result<Vec<IndexEntry>> {
            let current = Utc::now().year();
            assert!((current - 1..=current).contains(&year));
            self.0.house_index(year).await
        }
        async fn house_pdf(&self, entry: &IndexEntry) -> Result<Vec<u8>> {
            self.0.house_pdf(entry).await
        }
        async fn senate_reports(&self, _: Option<NaiveDate>) -> Result<Vec<ReportRef>> {
            panic!("Senate in live plan")
        }
        async fn senate_ptr(&self, _: &ReportRef) -> Result<String> {
            panic!("Senate in live plan")
        }
    }
    #[tokio::test]
    async fn fresh_live_plan_is_fifty_newest_current_year_and_never_drains_initial_history() {
        let file = ledger_file();
        let mut fake = Fake::new(None);
        fake.count = 250;
        let src = HousePlan(fake);
        {
            let mut db = Db::open(&file.0).unwrap();
            seed_filing(&mut db, Chamber::Senate, "senate:stored", Utc::now().date_naive());
            let db = Mutex::new(db);
            let mut cfg = config();
            cfg.house_years = vec![1999, Utc::now().year()];
            cfg.max_new_per_chamber = usize::MAX;
            let r = refresh(&src, &db, &cfg).await.unwrap();
            assert_eq!(r.new_filings, 50);
            assert_eq!(src.0.requests(Site::House, "pdf"), 50);
            assert!(r.summary().contains(SENATE_PAUSED));
            assert_eq!(db.lock().await.known_filing_ids(Chamber::Senate).unwrap().len(), 1);
            let seen = src.0.seen.lock().unwrap();
            assert_eq!(seen.iter().find(|(site, r)| *site == Site::House && r.starts_with("pdf")).unwrap().1, "pdf:0");
        }
        let db = Mutex::new(Db::open(&file.0).unwrap());
        for _ in 0..2 {
            let cfg = RefreshConfig::incremental(&*db.lock().await);
            let r = refresh(&src, &db, &cfg).await.unwrap();
            assert_eq!(r.new_filings, 0, "omitted initial history must stay omitted after reopen");
        }
        assert_eq!(src.0.requests(Site::House, "pdf"), 50);
        assert_eq!(src.0.requests(Site::Senate, ""), 0);
    }

    #[tokio::test]
    async fn parse_failures_retry_daily_at_most_three_times_and_expire_after_seven_days() {
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let mut src = Fake::new(None);
        src.bad_pdf = true;
        src.count = 1;
        for attempt in 1..=3 {
            let r = refresh(&src, &db, &config()).await.unwrap();
            assert!(r.errors.is_empty());
            assert_eq!(r.parse_errors.len(), 1);
            assert_eq!(src.requests(Site::House, "pdf"), attempt);
            let db = db.lock().await;
            assert!(db.known_filing_ids(Chamber::House).unwrap().contains("house:0"));
            assert!(db.retry_filing_ids(Chamber::House).unwrap().is_empty());
            let count: i64 = db
                .conn()
                .query_row("SELECT attempt_count FROM filings WHERE id = 'house:0'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, attempt as i64);
            db.conn().execute("UPDATE filings SET fetched_at = fetched_at - 86400 WHERE status = 'error'", []).unwrap();
        }
        refresh(&src, &db, &config()).await.unwrap();
        assert_eq!(src.requests(Site::House, "pdf"), 3);
        {
            let db = db.lock().await;
            db.conn().execute("UPDATE filings SET attempt_count = 1, first_attempt_at = first_attempt_at - 8 * 86400 WHERE status = 'error'", []).unwrap();
            assert!(db.retry_filing_ids(Chamber::House).unwrap().is_empty());
        }
        refresh(&src, &db, &config()).await.unwrap();
        assert_eq!(src.requests(Site::House, "pdf"), 3);
    }

    #[tokio::test]
    async fn stored_error_download_failures_also_consume_the_daily_and_total_retry_budget() {
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let mut src = Fake::new(None);
        src.bad_pdf = true;
        src.count = 1;
        refresh(&src, &db, &config()).await.unwrap();
        src.failure = Some((Site::House, "pdf"));
        src.status = 500;
        for expected in 2..=3 {
            db.lock()
                .await
                .conn()
                .execute("UPDATE filings SET fetched_at = fetched_at - 86400 WHERE status = 'error'", [])
                .unwrap();
            refresh(&src, &db, &config()).await.unwrap();
            refresh(&src, &db, &config()).await.unwrap();
            assert_eq!(src.requests(Site::House, "pdf"), expected);
        }
        db.lock()
            .await
            .conn()
            .execute("UPDATE filings SET fetched_at = fetched_at - 86400 WHERE status = 'error'", [])
            .unwrap();
        refresh(&src, &db, &config()).await.unwrap();
        assert_eq!(src.requests(Site::House, "pdf"), 3);
    }

    #[test]
    fn paused_summary_keeps_other_chamber_counts_and_parse_errors() {
        let report = RefreshReport {
            chamber_counts: [(3, 12), (0, 0)],
            paused: vec![pause_text(Site::Senate, Utc::now().timestamp() + 3600)],
            parse_errors: vec!["malformed filing".into()],
            ..Default::default()
        };
        let summary = report.summary();
        assert!(summary.starts_with("House: 3 new filings, 12 trades · Senate source paused until"));
        assert!(summary.ends_with("1 parse errors"));
    }

    #[test]
    fn huge_retry_after_values_are_bounded_in_parser_storage_and_display() {
        let now = Utc::now().timestamp();
        assert_eq!(retry_after(&u64::MAX.to_string(), now), Some(now + 7 * 86400));
        assert_eq!(retry_after("Fri, 31 Dec 9999 23:59:59 GMT", now), Some(now + 7 * 86400));
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.refuse_source(Site::Senate.key(), now, Some(i64::MAX)).unwrap(), now + 7 * 86400);
        assert!(!pause_text(Site::Senate, i64::MAX).contains("1970"));
    }

    #[test]
    fn transport_streak_is_per_site_and_success_breaks_it() {
        let mut failures = TransportFailures::default();
        let mut report = RefreshReport::default();
        for message in ["connection reset by peer", "operation timed out", "connect error"] {
            assert!(
                !failures
                    .observe::<()>(Site::Senate, Err(anyhow::anyhow!(message)), &mut report)
                    .unwrap_err()
                    .is::<SiteRefused>()
            );
            failures.observe(Site::Senate, Ok(()), &mut report).unwrap();
        }
        for site in [Site::Senate, Site::House] {
            for attempt in 1..=3 {
                let e = failures
                    .observe::<()>(site, Err(anyhow::anyhow!("connection reset by peer")), &mut report)
                    .unwrap_err();
                assert_eq!(e.is::<SiteRefused>(), attempt == 3);
            }
        }
    }

    struct Fake {
        failure: Option<(Site, &'static str)>,
        status: u16,
        retry: Option<&'static str>,
        count: usize,
        bad_pdf: bool,
        seen: StdMutex<Vec<(Site, String)>>,
        since: StdMutex<Option<NaiveDate>>,
    }

    impl Fake {
        fn new(failure: Option<(Site, &'static str)>) -> Self {
            Self {
                failure,
                status: 403,
                retry: None,
                count: 3,
                bad_pdf: false,
                seen: Default::default(),
                since: Default::default(),
            }
        }

        fn fetch(&self, site: Site, stage: &str, id: &str) -> Result<()> {
            self.seen.lock().unwrap().push((site, format!("{stage}:{id}")));
            if self.failure == Some((site, stage)) {
                if self.status == 0 {
                    home_csrf("<html>A challenge without a token</html>")?;
                } else {
                    check_response(site, self.status, b"", self.retry)?;
                }
            }
            Ok(())
        }

        fn requests(&self, site: Site, stage: &str) -> usize {
            self.seen.lock().unwrap().iter().filter(|(s, r)| *s == site && r.starts_with(stage)).count()
        }
    }

    impl Source for Fake {
        async fn roster_for(&self, site: Site) -> Result<Roster> {
            self.fetch(site, "roster", "")?;
            Ok(Roster::default())
        }

        async fn house_index(&self, year: i32) -> Result<Vec<IndexEntry>> {
            self.fetch(Site::House, "index", "")?;
            if year != Utc::now().year() {
                return Ok(vec![]);
            }
            Ok((0..self.count)
                .map(|i| IndexEntry {
                    doc_id: format!("{i}"),
                    prefix: String::new(),
                    first: "Test".into(),
                    last: "House".into(),
                    suffix: String::new(),
                    filing_type: "P".into(),
                    state_district: "CA01".into(),
                    year,
                    filing_date: Some(Utc::now().date_naive() - chrono::Duration::days((i % 91) as i64)),
                })
                .collect())
        }

        async fn house_pdf(&self, entry: &IndexEntry) -> Result<Vec<u8>> {
            self.fetch(Site::House, "pdf", &entry.doc_id)?;
            if self.bad_pdf {
                return Ok(b"%PDF-1.4 truncated garbage".to_vec());
            }
            Ok(include_bytes!("../tests/fixtures/house/8221321.pdf").to_vec())
        }

        async fn senate_reports(&self, since: Option<NaiveDate>) -> Result<Vec<ReportRef>> {
            *self.since.lock().unwrap() = since;
            self.fetch(Site::Senate, "search", "")?;
            Ok((0..self.count)
                .map(|i| ReportRef {
                    id: format!("{i}"),
                    kind: ReportKind::Electronic,
                    first: "Test".into(),
                    last: "Senator".into(),
                    office: String::new(),
                    title: "PTR".into(),
                    url: format!("https://efdsearch.senate.gov/{i}"),
                    date_received: Some(Utc::now().date_naive() - chrono::Duration::days((i % 91) as i64)),
                })
                .collect())
        }

        async fn senate_ptr(&self, r: &ReportRef) -> Result<String> {
            self.fetch(Site::Senate, "ptr", &r.id)?;
            Ok(include_str!("../tests/fixtures/senate/ptr_stocks.html").into())
        }
    }

    fn config() -> RefreshConfig {
        RefreshConfig {
            house_years: vec![Utc::now().year()],
            house_since: None,
            senate_since: None,
            max_new_per_chamber: 200,
            concurrency: 4,
        }
    }

    #[tokio::test]
    async fn refusal_stops_site_after_one_request_and_persists_pause() {
        for site in [Site::Senate, Site::House, Site::HouseRoster] {
            for status in [403, 429, 503] {
                let stage = if site == Site::House { "index" } else { "roster" };
                let mut src = Fake::new(Some((site, stage)));
                src.status = status;
                let db = Mutex::new(Db::open_in_memory().unwrap());
                let before = Utc::now().timestamp();
                let r = refresh(&src, &db, &config()).await.unwrap();
                assert_eq!(src.requests(site, ""), 1);
                let (until, strikes) = db.lock().await.source_cooldown(site.key()).unwrap();
                assert_eq!(strikes, 1);
                assert!(until >= before + 6 * 3600);
                assert!(r.summary().contains("source paused until"));
                assert!(!r.summary().contains("HTTP"));
                assert!(r.errors.is_empty());
                // The other chamber is still usable.
                assert!(r.new_filings >= 3);
            }
        }
    }

    #[tokio::test]
    async fn refusal_abandons_ptr_and_pdf_queues() {
        for (site, stage) in [(Site::Senate, "ptr"), (Site::House, "pdf")] {
            let src = Fake::new(Some((site, stage)));
            let db = Mutex::new(Db::open_in_memory().unwrap());
            refresh(&src, &db, &config()).await.unwrap();
            assert_eq!(src.requests(site, stage), 1);
            assert_eq!(db.lock().await.source_cooldown(site.key()).unwrap().1, 1);
        }
    }

    #[tokio::test]
    async fn missing_csrf_challenge_is_a_refusal() {
        let mut src = Fake::new(Some((Site::Senate, "search")));
        src.status = 0;
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let r = refresh(&src, &db, &config()).await.unwrap();
        assert_eq!(src.requests(Site::Senate, "search"), 1);
        assert_eq!(src.requests(Site::Senate, "ptr"), 0);
        assert!(r.summary().contains("Senate source paused until"));
        assert_eq!(db.lock().await.source_cooldown(Site::Senate.key()).unwrap().1, 1);
    }

    #[test]
    fn firewall_bodies_and_retry_after_are_classified() {
        for body in [
            "Sorry, a potential security risk was detected in your submitted request",
            "Verify you are human",
            "Access Denied",
            "Checking your browser",
            "captcha",
        ] {
            assert!(check_response(Site::Senate, 200, body.as_bytes(), None).unwrap_err().is::<SiteRefused>());
        }
        assert!(!check_response(Site::Senate, 500, b"ordinary error", None).unwrap_err().is::<SiteRefused>());
        let now = 1_700_000_000;
        assert_eq!(retry_after("7200", now), Some(now + 7200));
        assert_eq!(retry_after("Wed, 21 Oct 2015 07:28:00 GMT", now), Some(1_445_412_480));
        assert_eq!(retry_after("invalid", now), None);
        let err = check_response(Site::Senate, 429, b"", Some("90000")).unwrap_err();
        assert!(err.downcast_ref::<SiteRefused>().unwrap().retry_after.unwrap() >= Utc::now().timestamp() + 89999);
    }

    #[tokio::test]
    async fn cooldown_escalates_caps_honours_retry_after_and_resets_on_success() {
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let now = Utc::now().timestamp();
        {
            let db = db.lock().await;
            for hours in [6, 12, 24, 24] {
                assert_eq!(db.refuse_source(Site::Senate.key(), now, None).unwrap(), now + hours * 3600);
            }
            assert_eq!(db.source_cooldown(Site::Senate.key()).unwrap().1, 4);
            db.reset_source(Site::Senate.key()).unwrap();
            assert_eq!(db.refuse_source(Site::Senate.key(), now, Some(now + 40 * 3600)).unwrap(), now + 40 * 3600);
            db.reset_source(Site::Senate.key()).unwrap();
            assert_eq!(db.refuse_source(Site::Senate.key(), now, Some(now + 1)).unwrap(), now + 6 * 3600);
            // Expire the block without discarding its strike, as happens with elapsed time.
            db.conn().execute("UPDATE source_cooldowns SET blocked_until = 0", []).unwrap();
        }
        let src = Fake::new(None);
        refresh(&src, &db, &config()).await.unwrap();
        assert_eq!(db.lock().await.source_cooldown(Site::Senate.key()).unwrap(), (0, 0));
        assert_eq!(db.lock().await.refuse_source(Site::Senate.key(), now, None).unwrap(), now + 6 * 3600);
    }

    #[tokio::test]
    async fn retry_after_is_persisted_from_source_refusal() {
        let mut src = Fake::new(Some((Site::Senate, "search")));
        src.status = 429;
        src.retry = Some("172800");
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let before = Utc::now().timestamp();
        refresh(&src, &db, &config()).await.unwrap();
        assert!(db.lock().await.source_cooldown(Site::Senate.key()).unwrap().0 >= before + 172800);
    }

    struct TestDbFile(std::path::PathBuf);

    impl Drop for TestDbFile {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }

    #[tokio::test]
    async fn blocked_sites_are_skipped_after_database_reopen() {
        // Keep test files inside the repository, including SQLite's WAL sidecars.
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "cooldown-test-{}-{}.sqlite3",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let cleanup = TestDbFile(path);
        let path = &cleanup.0;
        {
            let db = Mutex::new(Db::open(path).unwrap());
            let src = Fake::new(Some((Site::Senate, "roster")));
            refresh(&src, &db, &config()).await.unwrap();
        }
        {
            let db = Mutex::new(Db::open(path).unwrap());
            assert!(paused_sources(&*db.lock().await).unwrap()[0].contains("Senate source paused until"));
            let src = Fake::new(None);
            refresh(&src, &db, &config()).await.unwrap();
            assert_eq!(src.requests(Site::Senate, ""), 0);
            assert!(src.requests(Site::House, "index") > 0);
        }
    }

    #[tokio::test]
    async fn default_config_is_current_year_and_fifty_while_fixture_history_stays_importable() {
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let mut src = Fake::new(None);
        src.count = 250;
        let cfg = RefreshConfig::incremental(&*db.lock().await);
        let since = Utc::now().date_naive() - chrono::Duration::days(90);
        assert_eq!(cfg.senate_since, Some(since));
        assert_eq!(cfg.max_new_per_chamber, 50);
        assert_eq!(cfg.concurrency, 1);
        assert_eq!(cfg.house_years, vec![Utc::now().year()]);
        // Fixtures can still import both chambers for parser/history regression coverage.
        // Live HttpSource additionally omits the initial backlog permanently.
        for run in 1..=5 {
            let cfg = RefreshConfig::incremental(&*db.lock().await);
            let r = refresh(&src, &db, &cfg).await.unwrap();
            assert_eq!(r.new_filings, 100);
            assert_eq!(r.capped, run < 5);
            assert_eq!(src.requests(Site::House, "pdf"), run * 50);
            assert_eq!(src.requests(Site::Senate, "ptr"), run * 50);
        }
        for key in ["last_refresh_date_house", "last_refresh_date_senate"] {
            assert_eq!(db.lock().await.meta(key).unwrap(), Some(Utc::now().date_naive().to_string()));
        }
        assert!(db.lock().await.meta("last_refresh_date").unwrap().is_none());
    }

    #[tokio::test]
    async fn discovery_filters_both_chambers_to_window_and_enforces_hard_cap() {
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let mut src = Fake::new(None);
        src.count = 250;
        let mut cfg = config();
        cfg.max_new_per_chamber = 2_000;
        cfg.house_since = Some(Utc::now().date_naive());
        cfg.senate_since = Some(Utc::now().date_naive());
        let r = refresh(&src, &db, &cfg).await.unwrap();
        assert_eq!(r.new_filings, 6); // offsets 0, 91, 182 in each chamber
        let db = Mutex::new(Db::open_in_memory().unwrap());
        cfg.house_since = None;
        cfg.senate_since = None;
        let r = refresh(&src, &db, &cfg).await.unwrap();
        assert_eq!(r.new_filings, 400);
    }

    fn seed_filing(db: &mut Db, chamber: Chamber, id: &str, filed_date: NaiveDate) {
        let politician_id = db
            .upsert_politician(&NewPolitician {
                name: "Stored Member".into(),
                chamber,
                party: Party::Unknown,
                state: None,
                district: None,
                bioguide: None,
            })
            .unwrap();
        db.insert_filing(
            &FilingRecord {
                id: id.into(),
                chamber,
                politician_id,
                filed_date,
                url: "offline fixture".into(),
                paper: true,
                status: FilingStatus::Scanned,
                error: None,
                is_amendment: false,
            },
            &[],
        )
        .unwrap();
    }

    #[test]
    fn legacy_house_only_database_gets_independent_windows_and_idempotent_migration() {
        let mut db = Db::open_in_memory().unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 10, 23).unwrap();
        let oldest = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        let span = (today - oldest).num_days();
        for i in 0..950 {
            seed_filing(
                &mut db,
                Chamber::House,
                &format!("house:{i}"),
                oldest + chrono::Duration::days(i * span / 949),
            );
        }
        db.set_meta("last_refresh_date", "2026-10-23").unwrap();
        let cfg = RefreshConfig::incremental_at(&db, today);
        assert_eq!(db.stats().unwrap().filings, 950);
        assert!(!db.has_filings(Chamber::Senate).unwrap());
        assert_eq!(cfg.house_since, Some(NaiveDate::from_ymd_opt(2026, 10, 16).unwrap()));
        assert_eq!(cfg.senate_since, Some(today - chrono::Duration::days(90)));
        assert_eq!(db.meta("last_refresh_date_house").unwrap().as_deref(), Some("2026-10-23"));
        assert!(db.meta("last_refresh_date_senate").unwrap().is_none());

        // A partially backfilled Senate must not inherit the legacy date on later runs.
        seed_filing(&mut db, Chamber::Senate, "senate:partial", today);
        db.set_meta("last_refresh_date_house", "2026-10-24").unwrap();
        for _ in 0..2 {
            let cfg = RefreshConfig::incremental_at(&db, today);
            assert_eq!(cfg.house_since, Some(NaiveDate::from_ymd_opt(2026, 10, 17).unwrap()));
            assert_eq!(cfg.senate_since, Some(today - chrono::Duration::days(90)));
            assert!(db.meta("last_refresh_date_senate").unwrap().is_none());
            assert_eq!(db.meta("last_refresh_date").unwrap().as_deref(), Some("2026-10-23"));
        }
    }

    #[test]
    fn populated_senate_can_migrate_and_house_years_use_only_house_window() {
        let mut db = Db::open_in_memory().unwrap();
        let today = NaiveDate::from_ymd_opt(2027, 2, 1).unwrap();
        seed_filing(&mut db, Chamber::Senate, "senate:stored", today);
        db.set_meta("last_refresh_date", "2027-01-31").unwrap();
        let cfg = RefreshConfig::incremental_at(&db, today);
        assert_eq!(cfg.senate_since, Some(NaiveDate::from_ymd_opt(2027, 1, 24).unwrap()));
        assert!(db.meta("last_refresh_date_house").unwrap().is_none());
        assert_eq!(cfg.house_years, vec![2027]);
        seed_filing(&mut db, Chamber::House, "house:stored", today);
        db.set_meta("last_refresh_date_house", "2027-01-31").unwrap();
        db.set_meta("last_refresh_date_senate", "2026-12-31").unwrap();
        let cfg = RefreshConfig::incremental_at(&db, today);
        assert_eq!(cfg.house_years, vec![2027]);
        assert_eq!(cfg.house_since, Some(NaiveDate::from_ymd_opt(2027, 1, 24).unwrap()));
        assert_eq!(cfg.senate_since, Some(NaiveDate::from_ymd_opt(2026, 12, 24).unwrap()));
    }

    #[tokio::test]
    async fn senate_pause_does_not_stop_house_date_advancing() {
        let today = Utc::now().date_naive();
        let old = today - chrono::Duration::days(30);
        for persisted_pause in [false, true] {
            let mut db = Db::open_in_memory().unwrap();
            seed_filing(&mut db, Chamber::House, "house:stored", old);
            db.set_meta("last_refresh_date", &old.to_string()).unwrap();
            let cfg = RefreshConfig::incremental(&db);
            if persisted_pause {
                db.refuse_source(Site::Senate.key(), Utc::now().timestamp(), None).unwrap();
            }
            let db = Mutex::new(db);
            let src = Fake::new(Some((Site::Senate, "roster")));
            let r = refresh(&src, &db, &cfg).await.unwrap();
            assert!(r.errors.is_empty());
            assert_eq!(r.paused.len(), 1);
            assert_eq!(src.requests(Site::Senate, ""), usize::from(!persisted_pause));
            let db = db.lock().await;
            assert_eq!(db.meta("last_refresh_date_house").unwrap(), Some(today.to_string()));
            assert!(db.meta("last_refresh_date_senate").unwrap().is_none());
            assert_eq!(db.meta("last_refresh_date").unwrap(), Some(old.to_string()));
            let next = RefreshConfig::incremental(&db);
            assert_eq!(next.house_since, Some(today - chrono::Duration::days(7)));
            assert_eq!(next.senate_since, Some(today - chrono::Duration::days(90)));
        }
    }

    #[tokio::test]
    async fn capped_chamber_keeps_its_window_without_holding_back_other_chamber() {
        let today = Utc::now().date_naive();
        let old = today - chrono::Duration::days(30);
        for capped in [Chamber::House, Chamber::Senate] {
            let mut db = Db::open_in_memory().unwrap();
            for chamber in [Chamber::House, Chamber::Senate] {
                let prefix = if chamber == Chamber::House { "house" } else { "senate" };
                seed_filing(&mut db, chamber, &format!("{prefix}:stored"), old);
                if chamber != capped {
                    for i in 0..3 {
                        seed_filing(&mut db, chamber, &format!("{prefix}:{i}"), today);
                    }
                }
            }
            db.set_meta("last_refresh_date", &old.to_string()).unwrap();
            let mut cfg = RefreshConfig::incremental(&db);
            cfg.max_new_per_chamber = 2;
            let db = Mutex::new(db);
            let src = Fake::new(None);
            let r = refresh(&src, &db, &cfg).await.unwrap();
            assert!(r.capped);
            assert!(r.errors.is_empty());
            assert_eq!(r.new_filings, 2);
            let db = db.lock().await;
            let next = RefreshConfig::incremental(&db);
            for (chamber, key, since) in [
                (Chamber::House, "last_refresh_date_house", next.house_since),
                (Chamber::Senate, "last_refresh_date_senate", next.senate_since),
            ] {
                let expected = if chamber == capped { old } else { today };
                assert_eq!(db.meta(key).unwrap(), Some(expected.to_string()));
                assert_eq!(since, Some(expected - chrono::Duration::days(7)));
            }
        }
    }

    #[tokio::test]
    async fn chamber_errors_keep_own_date_but_allow_other_chamber_to_advance() {
        let old = Utc::now().date_naive() - chrono::Duration::days(30);
        for (site, stage) in [
            (Site::Senate, "roster"),
            (Site::Senate, "search"),
            (Site::Senate, "ptr"),
            (Site::HouseRoster, "roster"),
            (Site::House, "index"),
            (Site::House, "pdf"),
        ] {
            let db = Db::open_in_memory().unwrap();
            db.set_meta("last_refresh_date_house", &old.to_string()).unwrap();
            db.set_meta("last_refresh_date_senate", &old.to_string()).unwrap();
            let db = Mutex::new(db);
            let mut src = Fake::new(Some((site, stage)));
            src.status = 500;
            let r = refresh(&src, &db, &config()).await.unwrap();
            assert!(!r.errors.is_empty());
            let db = db.lock().await;
            let (failed_key, successful_key) = if site == Site::Senate {
                ("last_refresh_date_senate", "last_refresh_date_house")
            } else {
                ("last_refresh_date_house", "last_refresh_date_senate")
            };
            assert_eq!(db.meta(failed_key).unwrap(), Some(old.to_string()));
            assert_eq!(db.meta(successful_key).unwrap(), Some(Utc::now().date_naive().to_string()));
        }
    }

    #[tokio::test]
    async fn chamber_discovery_filters_use_separate_lower_bounds() {
        for house_recent in [false, true] {
            let db = Mutex::new(Db::open_in_memory().unwrap());
            let src = Fake::new(None);
            let mut cfg = config();
            let today = Some(Utc::now().date_naive());
            cfg.house_since = if house_recent { today } else { None };
            cfg.senate_since = if house_recent { None } else { today };
            refresh(&src, &db, &cfg).await.unwrap();
            assert_eq!(src.requests(Site::House, "pdf"), if house_recent { 1 } else { 3 });
            assert_eq!(src.requests(Site::Senate, "ptr"), if house_recent { 3 } else { 1 });
        }
    }

    #[test]
    fn senate_hosts_share_a_gate_but_house_hosts_do_not() {
        assert_eq!(Site::for_url(roster::SENATE_ROSTER_URL).unwrap(), Site::Senate);
        assert_eq!(Site::for_url(senate::HOME_URL).unwrap(), Site::Senate);
        assert_eq!(Site::for_url(roster::HOUSE_ROSTER_URL).unwrap(), Site::HouseRoster);
        assert_eq!(Site::for_url(&house::index_zip_url(2026)).unwrap(), Site::House);
    }

    #[test]
    fn empty_database_keeps_ninety_day_window_even_after_an_empty_check() {
        let db = Db::open_in_memory().unwrap();
        let today = Utc::now().date_naive();
        db.set_meta("last_refresh_date", &today.to_string()).unwrap();
        for _ in 0..2 {
            let cfg = RefreshConfig::incremental(&db);
            let since = Some(today - chrono::Duration::days(90));
            assert_eq!(cfg.house_since, since);
            assert_eq!(cfg.senate_since, since);
            // Even a completed empty check must not shorten the first nonempty discovery.
            db.set_meta("last_refresh_date_house", &today.to_string()).unwrap();
            db.set_meta("last_refresh_date_senate", &today.to_string()).unwrap();
        }
    }

    #[tokio::test]
    async fn skipped_sites_keep_their_deadline_and_strikes_without_requests() {
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let now = Utc::now().timestamp();
        for site in Site::ALL {
            db.lock().await.refuse_source(site.key(), now, None).unwrap();
        }
        let src = Fake::new(None);
        let r = refresh(&src, &db, &config()).await.unwrap();
        assert!(src.seen.lock().unwrap().is_empty());
        assert_eq!(r.paused.len(), 3);
        for site in Site::ALL {
            assert_eq!(db.lock().await.source_cooldown(site.key()).unwrap(), (now + 6 * 3600, 1));
        }
    }

    #[test]
    fn sample_provider_disables_refresh_unless_explicitly_enabled() {
        assert_eq!(refresh_minutes(Some(" sample \t"), None), 0);
        assert_eq!(refresh_minutes(Some("  SaMpLe  "), None), 0);
        assert_eq!(refresh_minutes(Some(" \t"), None), 30);
        assert_eq!(refresh_minutes(Some("sample"), None), 0);
        assert_eq!(refresh_minutes(Some("sample"), Some("15")), 15);
        assert_eq!(refresh_minutes(Some("sample"), Some("0")), 0);
        assert_eq!(refresh_minutes(None, Some("0")), 0);
        assert_eq!(refresh_minutes(None, None), 30);
    }

    #[test]
    fn cadence_clamps_checked_values_and_preserves_zero() {
        for value in ["1", "7", "14", " 14 "] {
            assert_eq!(refresh_minutes(None, Some(value)), 15);
        }
        for value in ["10081", "18446744073709551615", "18446744073709551616", "999999999999999999999999999999999"] {
            assert_eq!(refresh_minutes(None, Some(value)), 10080);
        }
        for value in ["-1", "garbage", ""] {
            assert_eq!(refresh_minutes(None, Some(value)), 30);
        }
        assert_eq!(refresh_minutes(None, Some("0")), 0);
        assert_eq!(refresh_minutes(None, Some("15")), 15);
        assert_eq!(refresh_minutes(None, Some("10080")), 10080);
    }

    struct PdfFailure {
        plan: HousePlan,
        error: tr_net::Refused,
    }
    impl Source for PdfFailure {
        fn guarded_house_only(&self) -> bool {
            true
        }
        async fn roster_for(&self, site: Site) -> Result<Roster> {
            self.plan.roster_for(site).await
        }
        async fn house_index(&self, year: i32) -> Result<Vec<IndexEntry>> {
            self.plan.house_index(year).await
        }
        async fn house_pdf(&self, entry: &IndexEntry) -> Result<Vec<u8>> {
            let bytes = self.plan.house_pdf(entry).await?;
            if entry.doc_id == "0" {
                return Err(self.error.clone().into());
            }
            Ok(bytes)
        }
        async fn senate_reports(&self, _: Option<NaiveDate>) -> Result<Vec<ReportRef>> {
            panic!("Senate")
        }
        async fn senate_ptr(&self, _: &ReportRef) -> Result<String> {
            panic!("Senate")
        }
    }

    #[tokio::test]
    async fn all_per_pdf_guard_errors_continue_and_initial_failures_are_retry_bounded() {
        for error in [
            tr_net::Refused::Http(404),
            tr_net::Refused::Http(500),
            tr_net::Refused::Http(502),
            tr_net::Refused::BodyTooLarge,
        ] {
            let mut fake = Fake::new(None);
            fake.count = 3;
            let source = PdfFailure { plan: HousePlan(fake), error };
            let db = Mutex::new(Db::open_in_memory().unwrap());
            let first = refresh(&source, &db, &config()).await.unwrap();
            assert_eq!(source.plan.0.requests(Site::House, "pdf"), 3);
            assert!(first.paused.is_empty());
            assert_eq!(first.errors.len(), 1);
            assert!(first.parse_errors.is_empty(), "fetch errors must not be double-counted as parse errors");
            assert!(first.summary().contains("House: 1 filing failed"));
            for attempt in 1..=3 {
                let report = refresh(&source, &db, &config()).await.unwrap();
                assert!(report.paused.is_empty());
                let count: i64 = db
                    .lock()
                    .await
                    .conn()
                    .query_row("SELECT attempt_count FROM filings WHERE id='house:0'", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(count, attempt);
                db.lock()
                    .await
                    .conn()
                    .execute("UPDATE filings SET fetched_at=fetched_at-86400 WHERE id='house:0'", [])
                    .unwrap();
                refresh(&source, &db, &config()).await.unwrap();
            }
            assert_eq!(source.plan.0.requests(Site::House, "pdf"), 5, "two successes plus three failed attempts");
        }
    }

    #[tokio::test]
    async fn source_wide_guard_refusals_stop_but_only_real_pauses_are_paused() {
        for error in [
            tr_net::Refused::Budget,
            tr_net::Refused::Busy,
            tr_net::Refused::Disabled,
            tr_net::Refused::Offline,
            tr_net::Refused::NeedsContact,
            tr_net::Refused::Ledger,
            tr_net::Refused::Transport,
            tr_net::Refused::Paused { until: Utc::now().timestamp() + 3600 },
            tr_net::Refused::Http(403),
            tr_net::Refused::Http(429),
            tr_net::Refused::Http(503),
        ] {
            let paused = matches!(error, tr_net::Refused::Paused { .. });
            let mut fake = Fake::new(None);
            fake.count = 3;
            let source = PdfFailure { plan: HousePlan(fake), error };
            let db = Mutex::new(Db::open_in_memory().unwrap());
            let report = refresh(&source, &db, &config()).await.unwrap();
            assert_eq!(source.plan.0.requests(Site::House, "pdf"), 1);
            assert_eq!(report.paused.len(), usize::from(paused));
            assert!(!report.errors.is_empty());
            assert!(db.lock().await.known_filing_ids(Chamber::House).unwrap().contains("house:0"));
        }
    }

    #[tokio::test]
    async fn annual_rollover_fetches_prior_index_once_through_guard_and_survives_reopen() {
        use std::io::Write;
        let year = Utc::now().year();
        let prior = year - 1;
        let index = |xml: &str| {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            zip.start_file("index.xml", zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
            zip.finish().unwrap().into_inner()
        };
        let current_zip = index("<FinancialDisclosure></FinancialDisclosure>");
        let prior_zip = index(&format!(
            "<FinancialDisclosure><Member><Prefix/><Last>Late</Last><First>Test</First><Suffix/><FilingType>P</FilingType><StateDst>CA01</StateDst><Year>{prior}</Year><FilingDate>12/31/{prior}</FilingDate><DocID>late</DocID></Member></FinancialDisclosure>"
        ));
        let server = LocalServer::respond(move |request| {
            let body = if request.contains("MemberData.xml") {
                include_bytes!("../tests/fixtures/roster/house_members.xml").to_vec()
            } else if request.contains("/late.pdf ") {
                include_bytes!("../tests/fixtures/house/8221321.pdf").to_vec()
            } else if request.contains(&format!("/{prior}FD.zip ")) {
                prior_zip.clone()
            } else {
                current_zip.clone()
            };
            (200, String::new(), body)
        });
        let ledger = ledger_file();
        let data = ledger_file();
        for _ in 0..2 {
            let guard = tr_net::Guard::for_fixture(
                tr_net::Config {
                    ledger: ledger.0.clone(),
                    data_dir: Some(ledger.0.with_extension("instance")),
                    allow_external: true,
                    max_wait: Duration::from_secs(30),
                },
                tr_net::Settings::default(),
                server.base.clone(),
            )
            .unwrap();
            let source = HttpSource::with_guard(Arc::new(guard));
            let db = Mutex::new(Db::open(&data.0).unwrap());
            let mut cfg = config();
            cfg.house_since = NaiveDate::from_ymd_opt(prior, 12, 25);
            cfg.house_years = vec![1999, year + 1]; // ignored by live discovery
            let report = refresh(&source, &db, &cfg).await.unwrap();
            assert!(report.errors.is_empty(), "{}", report.summary());
            assert!(db.lock().await.known_filing_ids(Chamber::House).unwrap().contains("house:late"));
        }
        let seen = server.seen.lock().unwrap();
        assert_eq!(seen.iter().filter(|r| r.contains(&format!("/{prior}FD.zip "))).count(), 1);
        assert_eq!(seen.iter().filter(|r| r.contains(&format!("/{year}FD.zip "))).count(), 2);
        assert_eq!(seen.iter().filter(|r| r.contains(&format!("/ptr-pdfs/{prior}/late.pdf "))).count(), 1);
        assert!(seen.iter().all(|r| !r.contains("senate.gov") && !r.contains("1999FD")));
    }

    // ---- N0b adversarial review (enabled). ----

    /// Fixture House server: current-year index of three PTRs, roster, and PDFs; `fail`
    /// answers one doc ID's PDF with `status`. The guard paces at its real 3 s spacing.
    fn review_source(fail: &'static str, status: u16) -> (LocalServer, TestDbFile, HttpSource) {
        use std::io::Write;
        let year = Utc::now().year();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file(format!("{year}FD.xml"), zip::write::SimpleFileOptions::default()).unwrap();
        let xml = include_str!("../tests/fixtures/house/2026FD.xml").replace("2026", &year.to_string());
        zip.write_all(xml.as_bytes()).unwrap();
        let zip = zip.finish().unwrap().into_inner();
        let server = LocalServer::respond(move |request| {
            let pdf = |bytes: &[u8]| (200, String::new(), bytes.to_vec());
            if request.contains("MemberData.xml") {
                pdf(include_bytes!("../tests/fixtures/roster/house_members.xml"))
            } else if request.contains(&format!("/{fail}.pdf ")) {
                (status, String::new(), b"Not Found".to_vec())
            } else if request.contains("/8221321.pdf ") {
                pdf(include_bytes!("../tests/fixtures/house/8221321.pdf"))
            } else if request.contains("/20033751.pdf ") {
                pdf(include_bytes!("../tests/fixtures/house/20033751.pdf"))
            } else if request.contains("/20034201.pdf ") {
                pdf(include_bytes!("../tests/fixtures/house/20034201.pdf"))
            } else {
                (200, String::new(), zip.clone())
            }
        });
        let ledger = ledger_file();
        let guard = tr_net::Guard::for_fixture(
            tr_net::Config {
                ledger: ledger.0.clone(),
                data_dir: Some(ledger.0.with_extension("instance")),
                allow_external: true,
                max_wait: Duration::from_secs(10),
            },
            tr_net::Settings::default(),
            server.base.clone(),
        )
        .unwrap();
        (server, ledger, HttpSource::with_guard(Arc::new(guard)))
    }

    #[tokio::test]
    async fn review_pdf_http_error_aborts_whole_house_run() {
        let (server, _ledger, source) = review_source("20034201", 404);
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let report = refresh(&source, &db, &config()).await.unwrap();
        let pdfs = server.seen.lock().unwrap().iter().filter(|r| r.contains(".pdf ")).count();
        assert_eq!(pdfs, 3, "{}", report.summary());
        assert!(report.new_filings >= 2, "the two available PDFs must still ingest: {}", report.summary());
    }

    #[tokio::test]
    async fn review_guard_http_error_skips_retry_budget() {
        let (server, _ledger, source) = review_source("20034201", 500);
        let year = Utc::now().year();
        let mut db = Db::open_in_memory().unwrap();
        seed_filing(&mut db, Chamber::House, "house:20034201", Utc::now().date_naive());
        let old = Utc::now().timestamp() - 2 * 86400;
        db.conn()
            .execute(
                "UPDATE filings SET status='error', attempt_count=1, fetched_at=?1, first_attempt_at=?1 WHERE id='house:20034201'",
                [old],
            )
            .unwrap();
        db.set_meta(&format!("house:skipped-initial:{year}"), r#"["8221321","20033751"]"#).unwrap();
        let db = Mutex::new(db);
        let report = refresh(&source, &db, &config()).await.unwrap();
        assert_eq!(
            server.seen.lock().unwrap().iter().filter(|r| r.contains("/20034201.pdf ")).count(),
            1,
            "{}",
            report.summary()
        );
        let attempts: i64 = db
            .lock()
            .await
            .conn()
            .query_row("SELECT attempt_count FROM filings WHERE id='house:20034201'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(attempts, 2, "a failed retry download must consume the daily/total retry budget");
    }
}
