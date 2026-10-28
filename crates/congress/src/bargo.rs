//! Senate via Bargo. No disclosure portal URL is deserialized, stored or fetched.
use crate::db::NewPolitician;
use crate::refresh::NewTradeAlert;
use crate::{AmountRange, Chamber, Db, Party, TxType};
use anyhow::Result;
use chrono::{NaiveDate, Utc};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use sha1::{Digest, Sha1};
#[cfg(test)]
use std::time::Duration;
use tokio::sync::Mutex;

pub const CREDIT: &str = "Senate data: Bargo (bargo.ai)";
pub const CREDIT_URL: &str = "https://www.bargo.ai/free-apis/congress";
const BASE: &str = "https://www.bargo.ai/free-apis/congress/v1";
#[cfg(test)]
#[path = "bargo_tests.rs"]
mod tests;

/// API fields are nullable. Invalid individual rows are skipped, never guessed.
#[derive(Clone, Deserialize)]
pub struct BargoTrade {
    pub member: Option<String>,
    pub member_slug: Option<String>,
    pub chamber: Option<String>,
    pub state: Option<String>,
    pub ticker: Option<String>,
    pub asset: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub amount_low: Option<f64>,
    pub amount_high: Option<f64>,
    pub amount_range: Option<String>,
    pub transaction_date: Option<NaiveDate>,
    pub disclosure_date: Option<NaiveDate>,
}
#[derive(Deserialize)]
pub struct Page {
    pub trades: Vec<BargoTrade>,
}

pub fn request(path: &str, key: Option<&str>) -> reqwest::RequestBuilder {
    let request = reqwest::Client::new().get(format!("{BASE}/{path}"));
    match key.filter(|k| !k.is_empty()) {
        Some(key) => match reqwest::header::HeaderValue::from_str(key) {
            Ok(mut value) => {
                value.set_sensitive(true);
                request.header("X-Api-Key", value)
            }
            Err(_) => request.header("X-Api-Key", "\n"),
        },
        None => request,
    }
}
fn amount(value: Option<f64>) -> Option<u64> {
    value.filter(|v| v.is_finite() && *v >= 0.0 && *v < i64::MAX as f64 && v.fract() == 0.0).map(|v| v as u64)
}
fn name(value: &str) -> (String, String) {
    let words = |s: &str| {
        crate::roster::normalize_name(s)
            .into_iter()
            .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>())
            .collect::<Vec<_>>()
    };
    // Both roster "Last, First M." and eFD "First M. Last" reduce to first + last.
    // Never match on surname alone: two senators in a state may share one.
    if let Some((last, first)) = value.split_once(',') {
        let first = words(first);
        let last = words(last);
        if let (Some(first), Some(last)) = (first.first(), last.last()) {
            return (first.clone(), last.clone());
        }
    }
    let words = words(value);
    (words.first().cloned().unwrap_or_default(), words.last().cloned().unwrap_or_default())
}
impl BargoTrade {
    pub fn id(&self) -> Option<String> {
        let parts = [
            self.member_slug.as_deref()?,
            self.ticker.as_deref().unwrap_or(""),
            self.asset.as_deref().unwrap_or(""),
            self.kind.as_deref()?,
            &self.transaction_date?.to_string(),
            &amount(self.amount_low).map(|v| v.to_string()).unwrap_or_default(),
            &amount(self.amount_high).map(|v| v.to_string()).unwrap_or_default(),
            &self.disclosure_date?.to_string(),
        ];
        let digest = Sha1::digest(parts.join("|"));
        Some(format!("bargo:{}", digest.iter().map(|b| format!("{b:02x}")).collect::<String>()))
    }
}

/// One atomic page import. Seen keys also cover matches kept from old eFD history.
pub fn ingest(db: &mut Db, rows: &[BargoTrade], initial: bool, today: NaiveDate) -> Result<Vec<NewTradeAlert>> {
    let tx = db.conn().unchecked_transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS bargo_seen (id TEXT PRIMARY KEY, seen_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS bargo_members (slug TEXT PRIMARY KEY, politician_id INTEGER NOT NULL REFERENCES politicians(id));
        CREATE TABLE IF NOT EXISTS bargo_records (filing_id TEXT PRIMARY KEY, member_slug TEXT NOT NULL);")?;
    let politicians = {
        let mut statement = tx.prepare("SELECT id,name,state FROM politicians WHERE chamber='Senate'")?;
        statement
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut identities: std::collections::HashMap<_, _> = politicians
        .into_iter()
        .map(|(id, member, state)| ((name(&member), state.map(|s| s.to_ascii_uppercase())), id))
        .collect();
    let mut alerts = Vec::new();
    for row in rows {
        if !row.chamber.as_deref().is_some_and(|c| c.eq_ignore_ascii_case("senate")) {
            continue;
        }
        let (Some(id), Some(member), Some(tx_date), Some(filed)) = (
            row.id(),
            row.member.as_deref().filter(|s| !s.trim().is_empty()),
            row.transaction_date,
            row.disclosure_date,
        ) else {
            continue;
        };
        if tx.query_row("SELECT EXISTS(SELECT 1 FROM bargo_seen WHERE id=?1)", [&id], |r| r.get::<_, bool>(0))? {
            continue;
        }
        let kind = TxType::parse(row.kind.as_deref().unwrap_or(""));
        if kind == TxType::Other {
            continue;
        }
        let state = row.state.as_deref().map(str::trim).filter(|s| s.len() == 2).map(str::to_ascii_uppercase);
        let identity = (name(member), state.clone());
        let slug = row.member_slug.as_deref().unwrap_or_default();
        let previous: Option<i64> =
            tx.query_row("SELECT politician_id FROM bargo_members WHERE slug=?1", [slug], |r| r.get(0)).optional()?;
        let pid = match previous.or_else(|| identities.get(&identity).copied()) {
            Some(id) => id,
            None => {
                let id = db.upsert_politician(&NewPolitician {
                    name: member.trim().into(),
                    chamber: Chamber::Senate,
                    party: Party::Unknown,
                    state,
                    district: None,
                    bioguide: None,
                })?;
                identities.insert(identity, id);
                id
            }
        };
        tx.execute("INSERT OR IGNORE INTO bargo_members VALUES(?1,?2)", params![slug, pid])?;
        let ticker = row.ticker.as_deref().and_then(crate::model::normalize_ticker);
        let asset = row.asset.clone().unwrap_or_default();
        let range = row
            .amount_range
            .as_deref()
            .and_then(AmountRange::parse)
            .or_else(|| amount(row.amount_low).map(|min| AmountRange { min, max: amount(row.amount_high) }));
        let duplicate = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM trades WHERE politician_id=?1 AND filing_id NOT LIKE 'bargo:%'
             AND tx_date=?2 AND (tx_type=?3 OR (?4 AND tx_type IN ('S','S (partial)')))
             AND amount_min IS ?5 AND amount_max IS ?6
             AND ((?7 IS NOT NULL AND ticker=?7) OR (?8<>'' AND lower(trim(asset_name))=lower(trim(?8)))))",
            params![
                pid,
                tx_date.to_string(),
                kind.code(),
                kind.is_sell(),
                range.map(|a| a.min as i64),
                range.and_then(|a| a.max).map(|v| v as i64),
                ticker,
                asset
            ],
            |r| r.get::<_, bool>(0),
        )?;
        tx.execute("INSERT INTO bargo_seen VALUES(?1,?2)", params![id, Utc::now().timestamp()])?;
        if duplicate {
            continue;
        }
        // Keep the full spec key in bargo_seen, but an amendment/re-date updates the original
        // Bargo trade instead of creating a second trade or alert. Legacy imports have no slug
        // map yet: bootstrap it only through the already first+last+state matched politician.
        let amended: Option<String> = tx.query_row(
            "SELECT t.filing_id FROM trades t LEFT JOIN bargo_records b ON b.filing_id=t.filing_id
             WHERE t.filing_id LIKE 'bargo:%' AND (b.member_slug=?1 OR (b.member_slug IS NULL AND t.politician_id=?2))
             AND t.tx_date=?3 AND t.tx_type=?4 AND t.amount_min IS ?5 AND t.amount_max IS ?6
             AND ((?7 IS NOT NULL AND t.ticker=?7) OR (?7 IS NULL AND ?8<>'' AND lower(trim(t.asset_name))=lower(trim(?8)))) LIMIT 1",
            params![slug, pid, tx_date.to_string(), kind.code(), range.map(|a| a.min as i64),
                range.and_then(|a| a.max).map(|v| v as i64), ticker, asset], |r| r.get(0),
        ).optional()?;
        if let Some(existing) = amended {
            tx.execute("UPDATE trades SET filed_date=?2 WHERE filing_id=?1", params![existing, filed.to_string()])?;
            tx.execute("UPDATE filings SET filed_date=?2 WHERE id=?1", params![existing, filed.to_string()])?;
            tx.execute("INSERT OR IGNORE INTO bargo_records VALUES(?1,?2)", params![existing, slug])?;
            continue;
        }
        tx.execute("INSERT OR IGNORE INTO bargo_records VALUES(?1,?2)", params![id, slug])?;
        // Write directly on the enclosing transaction; no nested insert_filing transaction.
        tx.execute("INSERT OR IGNORE INTO filings (id,chamber,politician_id,filed_date,url,kind,status,fetched_at,first_attempt_at,source)
            VALUES(?1,'Senate',?2,?3,?4,'electronic','parsed',?5,?5,'Bargo')",
            params![id, pid, filed.to_string(), CREDIT_URL, Utc::now().timestamp()])?;
        tx.execute("INSERT OR IGNORE INTO trades (filing_id,row_idx,politician_id,ticker,asset_name,owner,tx_type,amount_min,amount_max,tx_date,filed_date,comment)
            VALUES(?1,0,?2,?3,?4,NULL,?5,?6,?7,?8,?9,?10)",
            params![id, pid, ticker, asset, kind.code(), range.map(|a| a.min as i64), range.and_then(|a| a.max).map(|v| v as i64),
                tx_date.to_string(), filed.to_string(), row.amount_range])?;
        if !initial && (0..=2).contains(&(today - filed).num_days()) && db.is_tracked(pid)? {
            let message = format!("{member}: new Senate trade via Bargo");
            if db.insert_alert(pid, &id, &message)?.is_some() {
                alerts.push(NewTradeAlert { politician_id: pid, politician: member.into(), filing_id: id, message });
            }
        }
    }
    tx.commit()?;
    Ok(alerts)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Run {
    Sweep,
    Poll,
}
pub fn schedule(guard: &tr_net::Guard, initial: bool) -> Result<Option<Run>, tr_net::Refused> {
    Ok(guard.claim_bargo_run(initial)?.map(|sweep| if sweep { Run::Sweep } else { Run::Poll }))
}

#[derive(Default, Debug)]
pub struct SyncReport {
    pub alerts: Vec<NewTradeAlert>,
    pub problem: Option<String>,
}
pub async fn sync(guard: &tr_net::Guard, db: &Mutex<Db>, key: Option<&str>) -> Result<SyncReport> {
    guard.set_bargo_key(key);
    let initial = db.lock().await.meta("bargo_initialized")?.is_none();
    let initial_sweep = initial || db.lock().await.meta("bargo_initial_complete")?.is_none();
    let Some(run) = schedule(guard, initial_sweep)? else { return Ok(SyncReport::default()) };
    // Mark before transport: a failed/partial initial sweep is never treated as fresh alerts.
    // Suppress alerts for the entire first successful sweep even after interruption.
    db.lock().await.set_meta("bargo_initialized", "1")?;
    let today = Utc::now().date_naive();
    let from = today - chrono::Duration::days(if run == Run::Sweep { guard.bargo_sweep_days()? } else { 7 });
    let keyed = key.is_some_and(|k| !k.is_empty());
    let default_limit = match (run, keyed) {
        (Run::Sweep, true) => 250,
        (Run::Sweep, false) | (Run::Poll, true) => 100,
        (Run::Poll, false) => 50,
    };
    // Keep the page width fixed (page-number pagination). In particular, shrink page ONE
    // to the remaining allowance, rather than silently burning a daily slot without a send.
    let limit = if run == Run::Sweep { default_limit.min(guard.bargo_allowance(key)? as usize) } else { default_limit };
    let mut report = SyncReport::default();
    let mut complete = false;
    for page in 0..40 {
        if limit == 0 || (guard.bargo_allowance(key)? as usize) < limit {
            report.problem = Some("Senate sweep deferred: daily row allowance used".into());
            break;
        }
        let req = request("trades", key).query(&[
            ("chamber", "senate".to_string()),
            ("from", from.to_string()),
            ("limit", limit.to_string()),
            ("page", page.to_string()),
        ]);
        let body = match guard.send_with_metadata_budget("bargo", req, if keyed { 40 } else { 10 }).await {
            Ok((body, _)) => body,
            Err(tr_net::Refused::Budget) => {
                report.problem = Some("Senate sweep deferred: daily row allowance used".into());
                break;
            }
            Err(e) => {
                report.problem = Some(e.to_string());
                break;
            }
        };
        let page: Page = match serde_json::from_slice::<Page>(&body) {
            Ok(page) if page.trades.len() <= limit => page,
            _ => {
                report.problem = Some("Invalid Bargo response".into());
                break;
            }
        };
        let count = page.trades.len();
        report.alerts.extend(ingest(&mut *db.lock().await, &page.trades, initial_sweep, today)?);
        db.lock().await.set_meta("bargo_updated", &Utc::now().timestamp().to_string())?;
        if run == Run::Poll || count < limit {
            complete = true;
            break;
        }
    }
    if run == Run::Sweep {
        if complete {
            db.lock().await.set_meta("bargo_initial_complete", "1")?;
            guard.finish_bargo_sweep(None)?;
        } else {
            let reason = report.problem.get_or_insert_with(|| "Senate sweep deferred: page bound reached".into());
            guard.finish_bargo_sweep(Some(reason))?;
        }
    }
    Ok(report)
}

fn timestamp(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default()
}
pub fn status(guard: &tr_net::Guard, updated: Option<i64>, keyed: bool) -> String {
    if let Some(status) = guard
        .status()
        .ok()
        .and_then(|rows| rows.into_iter().find(|s| s.group == "bargo"))
        .filter(|s| s.enabled && s.reason.as_deref().is_some_and(|r| r.contains("401")))
    {
        return format!(
            "Senate via Bargo · key error · paused until {}",
            timestamp(status.paused_until.unwrap_or_default())
        );
    }
    if let Err(error) = guard.check("bargo") {
        return match error {
            tr_net::Refused::Disabled => crate::refresh::SENATE_PAUSED.into(),
            tr_net::Refused::Paused { until } => format!("Senate via Bargo · paused until {}", timestamp(until)),
            tr_net::Refused::Budget => format!(
                "Senate via Bargo · paused until {} (request cap)",
                timestamp((Utc::now().timestamp() / 86400 + 1) * 86400)
            ),
            error => format!("Senate via Bargo · {error}"),
        };
    }
    if let Ok(Some(reason)) = guard.bargo_sweep_reason() {
        return reason;
    }
    let age = updated
        .map(|at| format!("updated {}h ago", (Utc::now().timestamp() - at).max(0) / 3600))
        .unwrap_or_else(|| "waiting for daily sweep".into());
    format!(
        "Senate via Bargo · {age}{}",
        if keyed { "" } else { " · needs key for full history (free window: 3 months)" }
    )
}
