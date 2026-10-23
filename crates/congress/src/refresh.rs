//! Incremental ingest: discover new PTRs on both chambers' official sites,
//! parse them, normalise into SQLite and raise alerts for tracked politicians.

use std::fmt;
use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate, Utc};
use tokio::sync::Mutex;
use tokio::time::Instant;
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
    configured.and_then(|v| v.parse().ok()).unwrap_or(if sample { 0 } else { 30 })
}

/// Where filings come from. [`HttpSource`] talks to the official sites; tests
/// substitute fixtures.
pub trait Source: Sync {
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
    /// House index years overlapping the discovery window.
    pub house_years: Vec<i32>,
    /// House discovery lower bound, independent of the Senate window.
    pub house_since: Option<NaiveDate>,
    /// Senate discovery lower bound; also sent to Senate search.
    pub senate_since: Option<NaiveDate>,
    /// Cap on new filings fetched per chamber per run (keeps first runs polite).
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
            house_years: (house_since.year()..=today.year()).collect(),
            house_since: Some(house_since),
            senate_since: Some(senate_since),
            max_new_per_chamber: 200,
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
                    self.chamber_counts[*i] != (0, 0)
                        || !self.paused.iter().any(|p| p.starts_with(&format!("{label} source")))
                })
                .map(|(i, label)| {
                    format!("{label}: {} new filings, {} trades", self.chamber_counts[i].0, self.chamber_counts[i].1)
                })
                .collect();
            parts.extend(self.paused.clone());
            if !self.parse_errors.is_empty() {
                parts.push(format!("{} parse errors", self.parse_errors.len()));
            }
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
        std::iter::once(counts).chain(self.notes.clone()).collect::<Vec<_>>().join(" · ")
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
    let mut blocked = Vec::new();
    for site in Site::ALL {
        let (until, _) = db.lock().await.source_cooldown(site.key())?;
        if until > Utc::now().timestamp() {
            blocked.push(site);
        }
    }
    let already_blocked = blocked.len();
    src.begin_run(&blocked).await;
    let mut house = Vec::new();
    let mut senate = Vec::new();
    for site in [Site::HouseRoster, Site::Senate] {
        if blocked.contains(&site) {
            incomplete_rosters.push(site);
            continue;
        }
        match failures.observe(site, src.roster_for(site).await, &mut report) {
            Ok(r) => {
                house.extend(r.house);
                senate.extend(r.senate);
                if site == Site::HouseRoster {
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
    for site in [Site::House, Site::Senate] {
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
                    db.reset_source(site.key())?;
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
    report.paused = paused_sources(&db)?;
    if blocked.len() > already_blocked {
        warn!("{}", report.paused.join(" · "));
    }
    // The legacy last_refresh_date is intentionally left untouched.
    db.set_meta("last_refresh_summary", &report.summary())?;
    info!("congress refresh: {}", report.summary());
    Ok(report)
}

async fn record_failure(
    db: &Mutex<Db>,
    report: &mut RefreshReport,
    site: Site,
    error: anyhow::Error,
    blocked: &mut Vec<Site>,
) -> Result<()> {
    if let Some(refusal) = error.downcast_ref::<SiteRefused>() {
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
    let mut todo: Vec<IndexEntry> = Vec::new();
    for &year in &cfg.house_years {
        let entries = match failures.observe(Site::House, src.house_index(year).await, report) {
            Ok(entries) => entries,
            Err(e) if e.is::<IndexNotPublished>() => {
                report.notes.push(format!("House {year} index not published yet; skipped"));
                continue;
            }
            Err(e) => return Err(e).with_context(|| format!("house index {year}")),
        };
        todo.extend(entries.into_iter().filter(|e| {
            e.is_ptr()
                && (retries.contains(&format!("house:{}", e.doc_id))
                    || (!known.contains(&format!("house:{}", e.doc_id))
                        && cfg.house_since.is_none_or(|since| e.filing_date.is_some_and(|d| d >= since))))
        }));
    }
    let mut seen = std::collections::HashSet::new();
    todo.retain(|entry| seen.insert(entry.doc_id.clone()));
    // Newest first so a capped run still gets the freshest filings.
    todo.sort_by_key(|e| std::cmp::Reverse(e.filing_date));
    let capped = todo.len() > cfg.max_new_per_chamber.min(200);
    report.capped |= capped;
    todo.truncate(cfg.max_new_per_chamber.min(200));
    debug!("house: {} new PTRs to fetch", todo.len());

    for entry in todo {
        let parsed = match failures.observe(Site::House, src.house_pdf(&entry).await, report) {
            Err(e) => {
                let id = format!("house:{}", entry.doc_id);
                if retries.contains(&id) {
                    db.lock().await.record_failed_retry(&id)?;
                }
                if e.is::<SiteRefused>() {
                    return Err(e);
                }
                // Network errors are not stored, so the filing is retried next run.
                report.errors.push(format!("house:{} fetch: {e:#}", entry.doc_id));
                continue;
            }
            Ok(bytes) => {
                let bytes_owned = bytes;
                match tokio::task::spawn_blocking(move || house::parse_ptr_pdf(&bytes_owned)).await? {
                    Ok(ptr) => Parsed {
                        record_paper: false,
                        status: FilingStatus::Parsed,
                        error: None,
                        filed: entry.filing_date.or(ptr.signed_date),
                        trades: ptr.trades,
                    },
                    Err(PdfError::Scanned) => Parsed {
                        record_paper: true,
                        status: FilingStatus::Scanned,
                        error: None,
                        filed: entry.filing_date,
                        trades: vec![],
                    },
                    Err(e) => Parsed {
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
                record_paper: true,
                status: FilingStatus::Scanned,
                error: None,
                filed: r.date_received,
                trades: vec![],
            },
            Ok(Some(html)) => match senate::parse_ptr_html(&html) {
                Ok(p) => Parsed {
                    record_paper: false,
                    status: FilingStatus::Parsed,
                    error: None,
                    filed: p.filed_date.or(r.date_received),
                    trades: p.trades,
                },
                Err(e) => Parsed {
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

#[derive(Default)]
struct PaceState {
    next: Option<Instant>,
    refused: bool,
    transport_failures: u8,
}

#[derive(Default)]
struct Pacer {
    state: Mutex<PaceState>,
}

impl Pacer {
    // The returned guard is held until the body is read, not just until headers arrive.
    async fn enter(&self, site: Site) -> Result<tokio::sync::MutexGuard<'_, PaceState>> {
        let mut state = self.state.lock().await;
        if state.refused {
            return Err(SiteRefused { site, retry_after: None }.into());
        }
        if let Some(next) = state.next {
            tokio::time::sleep_until(next).await;
        }
        state.next = Some(Instant::now() + Duration::from_secs(1));
        Ok(state)
    }
}

#[derive(Debug)]
struct IndexNotPublished;
impl fmt::Display for IndexNotPublished {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("House index not published yet")
    }
}
impl std::error::Error for IndexNotPublished {}

/// Validate download signatures, not Content-Type (which can be misleading).
fn check_download(site: Site, url: &str, status: u16, body: &[u8], retry: Option<&str>) -> Result<()> {
    let path = reqwest::Url::parse(url)?.path().to_ascii_lowercase();
    if site == Site::House
        && status == 404
        && [Utc::now().year(), Utc::now().year() + 1].iter().any(|year| path.ends_with(&format!("/{year}fd.zip")))
    {
        return Err(IndexNotPublished.into());
    }
    check_response(site, status, body, retry)?;
    if site == Site::House
        && (200..300).contains(&status)
        && ((path.ends_with(".pdf") && !body.starts_with(b"%PDF-"))
            || (path.ends_with(".zip")
                && ![b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08"].iter().any(|magic| body.starts_with(*magic))))
    {
        return Err(SiteRefused { site, retry_after: None }.into());
    }
    Ok(())
}

fn retry_after(value: &str, now: i64) -> Option<i64> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|s| now.saturating_add(s.min(i64::MAX as u64) as i64))
        .or_else(|| chrono::DateTime::parse_from_rfc2822(value).ok().map(|d| d.timestamp()))
        .map(|until| until.min(now.saturating_add(7 * 86400)))
}

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

fn home_csrf(home: &str) -> Result<String> {
    senate::csrf_from_form(home).ok_or_else(|| SiteRefused { site: Site::Senate, retry_after: None }.into())
}

#[cfg(test)]
type FixtureResponse = fn(&reqwest::Request) -> Result<(u16, Vec<u8>)>;

/// Live source backed by the official websites.
pub struct HttpSource {
    client: reqwest::Client,
    /// CSRF token of an agreed eFD session, established lazily.
    senate_csrf: Mutex<Option<String>>,
    pacers: [Pacer; 3],
    senate_rehandshake: std::sync::atomic::AtomicBool,
    site_base: Option<(Site, reqwest::Url)>,
    #[cfg(test)]
    fixture: Option<FixtureResponse>,
    #[cfg(test)]
    fixture_hits: std::sync::atomic::AtomicUsize,
}

impl HttpSource {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("terminal-rs/", env!("CARGO_PKG_VERSION"), " (personal STOCK Act disclosure reader)"))
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self {
            client,
            senate_csrf: Mutex::new(None),
            pacers: Default::default(),
            senate_rehandshake: Default::default(),
            site_base: None,
            #[cfg(test)]
            fixture: None,
            #[cfg(test)]
            fixture_hits: Default::default(),
        })
    }

    /// Route every request for one logical site to an offline fixture server.
    /// Redirects are still classified and paced using the logical site.
    pub fn with_site_base(site: Site, base: &str) -> Result<Self> {
        let mut source = Self::new()?;
        source.site_base = Some((site, reqwest::Url::parse(base)?));
        Ok(source)
    }

    async fn refuse(&self, site: Site) -> anyhow::Error {
        self.pacer(site).state.lock().await.refused = true;
        SiteRefused { site, retry_after: None }.into()
    }

    fn pacer(&self, site: Site) -> &Pacer {
        &self.pacers[site as usize]
    }

    /// All HTTP, including redirects and session handshakes, passes through this gate.
    async fn request(&self, builder: reqwest::RequestBuilder) -> Result<(Vec<u8>, String)> {
        let mut request = builder.build()?;
        for _ in 0..6 {
            let url = request.url().clone();
            let site = Site::for_url(url.as_str())?;
            let mut pace = self.pacer(site).enter(site).await?;
            let replay = request.try_clone();
            if let Some((mapped_site, base)) = &self.site_base {
                // A fake-base source must never fall through to a live host.
                anyhow::ensure!(*mapped_site == site, "unmapped site in offline source");
                let mut target = base.clone();
                target.set_path(url.path());
                target.set_query(url.query());
                *request.url_mut() = target;
            }
            let (status, retry, location, bytes) = match self.response(request, site).await {
                Ok(response) => {
                    pace.transport_failures = 0;
                    response
                }
                Err(e) => {
                    if is_transport(&e) {
                        pace.transport_failures += 1;
                        if pace.transport_failures >= 3 {
                            pace.refused = true;
                            return Err(SiteRefused { site, retry_after: None }.into());
                        }
                    } else {
                        pace.transport_failures = 0;
                    }
                    pace.refused |= e.is::<SiteRefused>();
                    return Err(e);
                }
            };
            let checked =
                check_download(site, url.as_str(), status.as_u16(), &bytes, retry.as_deref()).and_then(|()| {
                    if site == Site::Senate && url.path().contains("/search/home") && !status.is_redirection() {
                        home_csrf(&String::from_utf8_lossy(&bytes)).map(|_| ()).map_err(|_| {
                            SiteRefused {
                                site,
                                retry_after: retry.as_deref().and_then(|v| retry_after(v, Utc::now().timestamp())),
                            }
                            .into()
                        })
                    } else {
                        Ok(())
                    }
                });
            if let Err(e) = checked {
                pace.refused |= e.is::<SiteRefused>();
                return Err(e);
            }
            if status.is_redirection() {
                let next = url.join(location.as_deref().context("redirect without Location")?)?;
                request = if matches!(status.as_u16(), 307 | 308) {
                    let mut replay = replay.context("redirect with non-replayable body")?;
                    *replay.url_mut() = next;
                    replay
                } else {
                    self.client.get(next).header("Referer", url.as_str()).build()?
                };
                continue;
            }
            return Ok((bytes.to_vec(), url.to_string()));
        }
        bail!("too many Congress source redirects")
    }

    /// Raw transport seam: fixture tests exercise the real request gate and session
    /// logic without opening a socket or contacting any host.
    async fn response(
        &self,
        request: reqwest::Request,
        site: Site,
    ) -> Result<(reqwest::StatusCode, Option<String>, Option<String>, Vec<u8>)> {
        #[cfg(test)]
        if let Some(fixture) = self.fixture {
            self.fixture_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (status, bytes) = fixture(&request)?;
            return Ok((reqwest::StatusCode::from_u16(status)?, None, None, bytes));
        }
        let resp = self.client.execute(request).await?;
        let status = resp.status();
        let retry = resp.headers().get(reqwest::header::RETRY_AFTER).and_then(|h| h.to_str().ok()).map(str::to_owned);
        let location = resp.headers().get(reqwest::header::LOCATION).and_then(|h| h.to_str().ok()).map(str::to_owned);
        // Do not wait for a challenge body when the status already refuses us.
        if matches!(status.as_u16(), 403 | 429 | 503) {
            return Err(SiteRefused {
                site,
                retry_after: retry.as_deref().and_then(|v| retry_after(v, Utc::now().timestamp())),
            }
            .into());
        }
        Ok((status, retry, location, resp.bytes().await?.to_vec()))
    }

    async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        Ok(self.request(self.client.get(url)).await?.0)
    }

    /// Accept the eFD prohibition agreement and return the session CSRF token.
    async fn senate_session(&self, force: bool) -> Result<String> {
        let mut guard = self.senate_csrf.lock().await;
        if let (Some(tok), false) = (guard.as_ref(), force) {
            return Ok(tok.clone());
        }
        if force && self.senate_rehandshake.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(self.refuse(Site::Senate).await);
        }
        let home = self.get_bytes(senate::HOME_URL).await?;
        let form_tok = home_csrf(&String::from_utf8_lossy(&home))?;
        let (page, _) = self
            .request(
                self.client
                    .post(senate::HOME_URL)
                    .header("Referer", senate::HOME_URL)
                    .form(&[("prohibition_agreement", "1"), ("csrfmiddlewaretoken", form_tok.as_str())]),
            )
            .await?;
        // After agreeing, the search page carries a fresh form token tied to the csrftoken cookie.
        let page = String::from_utf8_lossy(&page);
        let tok = senate::csrf_from_form(&page).unwrap_or(form_tok);
        *guard = Some(tok.clone());
        Ok(tok)
    }

    async fn senate_page(&self, csrf: &str, start: u64, since: Option<NaiveDate>) -> Result<senate::SearchPage> {
        let (bytes, _) = self
            .request(
                self.client
                    .post(senate::DATA_URL)
                    .header("Referer", senate::SEARCH_URL)
                    .header("X-CSRFToken", csrf)
                    .form(&senate::search_form(csrf, start, 100, since)),
            )
            .await?;
        senate::parse_search_json(&String::from_utf8_lossy(&bytes))
    }
}

impl Source for HttpSource {
    async fn begin_run(&self, blocked: &[Site]) {
        for site in Site::ALL {
            let mut state = self.pacer(site).state.lock().await;
            state.refused = blocked.contains(&site);
            state.transport_failures = 0;
        }
        *self.senate_csrf.lock().await = None;
        self.senate_rehandshake.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    async fn roster_for(&self, site: Site) -> Result<Roster> {
        match site {
            Site::Senate => {
                let bytes = self.get_bytes(roster::SENATE_ROSTER_URL).await?;
                Ok(Roster::new(vec![], roster::parse_senate_roster(&String::from_utf8_lossy(&bytes))?))
            }
            Site::HouseRoster => {
                let bytes = self.get_bytes(roster::HOUSE_ROSTER_URL).await?;
                Ok(Roster::new(roster::parse_house_roster(&String::from_utf8_lossy(&bytes))?, vec![]))
            }
            Site::House => Ok(Roster::default()),
        }
    }

    async fn house_index(&self, year: i32) -> Result<Vec<IndexEntry>> {
        let zip = self.get_bytes(&house::index_zip_url(year)).await?;
        tokio::task::spawn_blocking(move || house::parse_index_zip(&zip)).await?
    }

    async fn house_pdf(&self, entry: &IndexEntry) -> Result<Vec<u8>> {
        self.get_bytes(&entry.pdf_url()).await
    }

    async fn senate_reports(&self, since: Option<NaiveDate>) -> Result<Vec<ReportRef>> {
        let mut csrf = self.senate_session(false).await?;
        let mut out = Vec::new();
        let mut start = 0;
        loop {
            let page = match self.senate_page(&csrf, start, since).await {
                Ok(p) => p,
                // Session expired: agree again once and retry.
                Err(e) if !e.is::<SiteRefused>() && !is_transport(&e) && (start == 0 || out.is_empty()) => {
                    csrf = self.senate_session(true).await?;
                    match self.senate_page(&csrf, start, since).await {
                        Ok(page) => page,
                        Err(e) if is_transport(&e) || e.is::<SiteRefused>() => return Err(e),
                        Err(_) => return Err(self.refuse(Site::Senate).await),
                    }
                }
                Err(e) => return Err(e),
            };
            let n = page.reports.len() as u64;
            out.extend(page.reports);
            start += n;
            if n == 0 || start >= page.total {
                break;
            }
        }
        Ok(out)
    }

    async fn senate_ptr(&self, report: &ReportRef) -> Result<String> {
        self.senate_session(false).await?;
        let (bytes, final_url) =
            self.request(self.client.get(&report.url).header("Referer", senate::SEARCH_URL)).await?;
        let valid_ptr = |bytes: Vec<u8>, url: &str| -> Option<String> {
            let text = String::from_utf8(bytes).ok()?;
            (!url.contains("/search/home") && senate::parse_ptr_html(&text).is_ok()).then_some(text)
        };
        if let Some(text) = valid_ptr(bytes, &final_url) {
            return Ok(text);
        }
        // The budget is shared with search and every PTR for the entire run.
        self.senate_session(true).await?;
        let (bytes, url) = self.request(self.client.get(&report.url).header("Referer", senate::SEARCH_URL)).await?;
        match valid_ptr(bytes, &url) {
            Some(text) => Ok(text),
            None => Err(self.refuse(Site::Senate).await),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};

    struct PtrProbe(HttpSource);

    impl Source for PtrProbe {
        async fn begin_run(&self, blocked: &[Site]) {
            self.0.begin_run(blocked).await;
        }
        async fn roster_for(&self, _: Site) -> Result<Roster> {
            Ok(Roster::default())
        }
        async fn house_index(&self, _: i32) -> Result<Vec<IndexEntry>> {
            Ok(vec![])
        }
        async fn house_pdf(&self, _: &IndexEntry) -> Result<Vec<u8>> {
            unreachable!()
        }
        async fn senate_reports(&self, since: Option<NaiveDate>) -> Result<Vec<ReportRef>> {
            let mut fake = Fake::new(None);
            fake.count = 200;
            fake.senate_reports(since).await
        }
        async fn senate_ptr(&self, report: &ReportRef) -> Result<String> {
            self.0.senate_ptr(report).await
        }
    }

    fn agreement() -> Vec<u8> {
        b"<html><form><input name=\"csrfmiddlewaretoken\" value=\"fixture-token\"></form></html>".to_vec()
    }

    fn challenge_response(request: &reqwest::Request) -> Result<(u16, Vec<u8>)> {
        Ok((
            200,
            if request.url().path().contains("/search/home") {
                agreement()
            } else {
                b"<html>Transaction Date: please enable cookies</html>".to_vec()
            },
        ))
    }

    #[tokio::test(start_paused = true)]
    async fn http_soft_block_costs_six_requests_for_two_hundred_ptrs_and_persists_pause() {
        for fixture in
            [challenge_response as FixtureResponse, (|_: &reqwest::Request| Ok((200, agreement()))) as FixtureResponse]
        {
            let mut http = HttpSource::new().unwrap();
            http.fixture = Some(fixture);
            let src = PtrProbe(http);
            let db = Mutex::new(Db::open_in_memory().unwrap());
            let r = refresh(&src, &db, &config()).await.unwrap();
            assert_eq!(src.0.fixture_hits.load(std::sync::atomic::Ordering::SeqCst), 6);
            assert!(r.paused[0].contains("Senate source paused"));
            assert_eq!(db.lock().await.source_cooldown(Site::Senate.key()).unwrap().1, 1);
            refresh(&src, &db, &config()).await.unwrap();
            assert_eq!(src.0.fixture_hits.load(std::sync::atomic::Ordering::SeqCst), 6);
            // Even a direct caller cannot continue the queue within the run.
            assert!(src.0.get_bytes(senate::HOME_URL).await.unwrap_err().is::<SiteRefused>());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn forced_handshake_budget_is_shared_by_search_and_ptrs_and_reset_per_run() {
        let mut http = HttpSource::new().unwrap();
        http.fixture = Some(challenge_response);
        for expected in [2, 4] {
            http.begin_run(&[]).await;
            http.senate_session(true).await.unwrap();
            assert!(http.senate_session(true).await.unwrap_err().is::<SiteRefused>());
            assert_eq!(http.fixture_hits.load(std::sync::atomic::Ordering::SeqCst), expected);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn house_html_downloads_refuse_and_close_the_http_gate() {
        for url in [
            house::index_zip_url(Utc::now().year()),
            "https://disclosures-clerk.house.gov/public_disc/ptr-pdfs/2026/123.pdf".into(),
        ] {
            let mut http = HttpSource::new().unwrap();
            http.fixture = Some(|_| Ok((200, b"<html>Sign in to continue</html>".to_vec())));
            assert!(http.get_bytes(&url).await.unwrap_err().is::<SiteRefused>());
            assert!(http.get_bytes(&url).await.unwrap_err().is::<SiteRefused>());
            assert_eq!(http.fixture_hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
        for (suffix, body) in [("pdf", b"random bytes".as_slice()), ("zip", b"not a zip".as_slice())] {
            assert!(
                check_download(
                    Site::House,
                    &format!("https://disclosures-clerk.house.gov/test.{suffix}"),
                    200,
                    body,
                    None
                )
                .unwrap_err()
                .is::<SiteRefused>()
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn unpublished_house_year_is_skipped_with_a_note_not_a_refusal() {
        let mut src = HttpSource::new().unwrap();
        src.fixture = Some(|_| Ok((404, b"Not Found".to_vec())));
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let mut cfg = config();
        cfg.house_years = vec![Utc::now().year() + 1];
        let mut report = RefreshReport::default();
        refresh_house(&src, &db, &cfg, &Roster::default(), &mut report, &mut TransportFailures::default())
            .await
            .unwrap();
        assert!(report.errors.is_empty());
        assert!(report.summary().contains("not published yet; skipped"));
        assert!(!src.pacer(Site::House).state.lock().await.refused);
        assert!(
            !check_download(Site::House, &house::index_zip_url(2020), 404, b"missing", None)
                .unwrap_err()
                .is::<IndexNotPublished>()
        );
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

    #[tokio::test(start_paused = true)]
    async fn http_transport_streak_stops_after_three_and_clears_next_run() {
        let mut http = HttpSource::new().unwrap();
        http.fixture = Some(|_| Err(anyhow::anyhow!("error sending request: connection reset by peer")));
        for run in 1..=2 {
            http.begin_run(&[]).await;
            for attempt in 1..=4 {
                let error = http.get_bytes(senate::HOME_URL).await.unwrap_err();
                assert_eq!(error.is::<SiteRefused>(), attempt >= 3);
            }
            assert_eq!(http.fixture_hits.load(std::sync::atomic::Ordering::SeqCst), 3 * run);
        }
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

    #[tokio::test(start_paused = true)]
    async fn pacer_spaces_starts_and_holds_single_in_flight_through_body() {
        let pacer = Arc::new(Pacer::default());
        let first = pacer.enter(Site::Senate).await.unwrap();
        let start = Instant::now();
        let p = pacer.clone();
        let waiting = tokio::spawn(async move {
            let _guard = p.enter(Site::Senate).await.unwrap();
            Instant::now()
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(!waiting.is_finished(), "a slow response must exclude the next request");
        // Other hosts do not wait on the Senate gate.
        let house = Pacer::default();
        let _house = house.enter(Site::House).await.unwrap();
        drop(first);
        let second = waiting.await.unwrap();
        assert!(second.duration_since(start) >= Duration::from_secs(2));
        let third = pacer.enter(Site::Senate).await.unwrap();
        assert!(Instant::now().duration_since(second) >= Duration::from_secs(1));
        drop(third);
        pacer.state.lock().await.refused = true;
        assert!(pacer.enter(Site::Senate).await.err().unwrap().is::<SiteRefused>());
    }

    #[tokio::test]
    async fn first_run_is_ninety_days_capped_at_two_hundred_and_backlog_drains() {
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let mut src = Fake::new(None);
        src.count = 250;
        let cfg = RefreshConfig::incremental(&*db.lock().await);
        let since = Utc::now().date_naive() - chrono::Duration::days(90);
        assert_eq!(cfg.senate_since, Some(since));
        assert_eq!(cfg.max_new_per_chamber, 200);
        assert_eq!(cfg.concurrency, 1);
        assert_eq!(cfg.house_years, (since.year()..=Utc::now().year()).collect::<Vec<_>>());
        let r = refresh(&src, &db, &cfg).await.unwrap();
        assert_eq!(r.new_filings, 400);
        assert_eq!(src.requests(Site::Senate, "ptr"), 200);
        assert_eq!(src.requests(Site::House, "pdf"), 200);
        assert_eq!(*src.since.lock().unwrap(), Some(since));
        {
            let seen = src.seen.lock().unwrap();
            let first = seen.iter().find(|(s, r)| *s == Site::Senate && r.starts_with("ptr")).unwrap();
            assert_eq!(first.1, "ptr:0");
        }
        assert!(r.capped);
        for key in ["last_refresh_date_house", "last_refresh_date_senate"] {
            assert!(db.lock().await.meta(key).unwrap().is_none());
        }
        let cfg = RefreshConfig::incremental(&*db.lock().await);
        assert_eq!(cfg.house_since, Some(since));
        assert_eq!(cfg.senate_since, Some(since));
        let r = refresh(&src, &db, &cfg).await.unwrap();
        assert_eq!(r.new_filings, 100);
        assert!(!r.capped);
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
        assert_eq!(cfg.house_years, vec![2026, 2027]);
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
}
