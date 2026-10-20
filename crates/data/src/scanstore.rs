//! The scanner's own bar store, separate from the chart cache: every symbol records when it
//! was fetched, from which source and whether the bars were simulated, so a market-wide scan
//! can reuse compatible fresh snapshots without losing provenance. Refreshes replace
//! the full fetched history rather than merging an unverified tail into older data.

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, NaiveTime, Utc, Weekday};
use rusqlite::{Connection, OptionalExtension, params};
use tr_core::{Bar, Timeframe};

pub struct ScanStore {
    conn: Connection,
}

/// When and from where a symbol's bars were last fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetch {
    pub at: i64,
    pub source: String,
    pub simulated: bool,
}

impl ScanStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS scan_bars (
                symbol TEXT NOT NULL, tf TEXT NOT NULL, ts INTEGER NOT NULL,
                o REAL NOT NULL, h REAL NOT NULL, l REAL NOT NULL, c REAL NOT NULL, v REAL NOT NULL,
                PRIMARY KEY (symbol, tf, ts)
            ) WITHOUT ROWID;
            CREATE TABLE IF NOT EXISTS scan_fetches (
                symbol TEXT NOT NULL, tf TEXT NOT NULL, fetched_at INTEGER NOT NULL,
                source TEXT NOT NULL, simulated INTEGER NOT NULL,
                PRIMARY KEY (symbol, tf)
            ) WITHOUT ROWID;",
        )?;
        Ok(Self { conn })
    }

    /// Atomically replace one complete fetched snapshot and its provenance. Never
    /// merge histories from different providers, simulation modes or coverage windows.
    pub fn store(&mut self, symbol: &str, tf: Timeframe, bars: &[Bar], fetch: &Fetch) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM scan_bars WHERE symbol = ?1 AND tf = ?2", params![symbol, tf.label()])?;
        {
            let mut st = tx.prepare_cached(
                "INSERT INTO scan_bars (symbol, tf, ts, o, h, l, c, v) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (symbol, tf, ts) DO UPDATE SET o = excluded.o, h = excluded.h, l = excluded.l,
                   c = excluded.c, v = excluded.v",
            )?;
            for b in bars {
                st.execute(params![symbol, tf.label(), b.ts, b.open, b.high, b.low, b.close, b.volume])?;
            }
            tx.execute(
                "INSERT INTO scan_fetches (symbol, tf, fetched_at, source, simulated) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (symbol, tf) DO UPDATE SET fetched_at = excluded.fetched_at,
                   source = excluded.source, simulated = excluded.simulated",
                params![symbol, tf.label(), fetch.at, fetch.source, fetch.simulated],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The most recent `limit` bars, ascending.
    pub fn load(&self, symbol: &str, tf: Timeframe, limit: usize) -> Result<Vec<Bar>> {
        let mut st = self.conn.prepare_cached(
            "SELECT ts, o, h, l, c, v FROM scan_bars WHERE symbol = ?1 AND tf = ?2 ORDER BY ts DESC LIMIT ?3",
        )?;
        let mut bars: Vec<Bar> = st
            .query_map(params![symbol, tf.label(), limit as i64], |r| {
                Ok(Bar::new(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        bars.reverse();
        Ok(bars)
    }

    pub fn fetched(&self, symbol: &str, tf: Timeframe) -> Result<Option<Fetch>> {
        Ok(self
            .conn
            .query_row(
                "SELECT fetched_at, source, simulated FROM scan_fetches WHERE symbol = ?1 AND tf = ?2",
                params![symbol, tf.label()],
                |r| Ok(Fetch { at: r.get(0)?, source: r.get(1)?, simulated: r.get(2)? }),
            )
            .optional()?)
    }
}

/// 21:15 UTC is after the 16:00 New York close in both EST and EDT.
const CLOSE_UTC: (u32, u32) = (21, 15);

/// The most recent weekday close boundary at or before `now`.
fn last_close(now: DateTime<Utc>) -> DateTime<Utc> {
    let close = NaiveTime::from_hms_opt(CLOSE_UTC.0, CLOSE_UTC.1, 0).expect("valid time");
    let mut day = now.date_naive();
    if now.time() < close {
        day -= Duration::days(1);
    }
    while matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
        day -= Duration::days(1);
    }
    day.and_time(close).and_utc()
}

/// 13:30 UTC is at or before the 09:30 New York open in both EDT and EST.
const OPEN_UTC: (u32, u32) = (13, 30);

/// Inside a (possibly) trading weekday session, conservatively wide for both EST and EDT.
fn in_session(now: DateTime<Utc>) -> bool {
    let t = now.time();
    let open = NaiveTime::from_hms_opt(OPEN_UTC.0, OPEN_UTC.1, 0).expect("valid time");
    let close = NaiveTime::from_hms_opt(CLOSE_UTC.0, CLOSE_UTC.1, 0).expect("valid time");
    !matches!(now.weekday(), Weekday::Sat | Weekday::Sun) && t >= open && t < close
}

/// Whether bars fetched at `fetched_at` still describe the market at `now`: during a session,
/// for 15 minutes (the last bar is still forming); otherwise when fetched after the latest
/// close, since nothing has traded since. Holidays only cost an unneeded refresh.
pub fn is_fresh(fetched_at: i64, now: i64) -> bool {
    let Some(now_dt) = DateTime::from_timestamp(now, 0) else { return false };
    if in_session(now_dt) {
        (0..15 * 60).contains(&(now - fetched_at))
    } else {
        fetched_at >= last_close(now_dt).timestamp()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> i64 {
        DateTime::parse_from_rfc3339(s).unwrap().timestamp()
    }

    #[test]
    fn stores_bars_with_their_provenance() {
        let mut store = ScanStore::open_in_memory().unwrap();
        assert_eq!(store.fetched("AAPL US Equity", Timeframe::D1).unwrap(), None);
        let bars: Vec<Bar> = (0..5).map(|i| Bar::new(i * 86_400, 1.0, 2.0, 0.5, 1.5, 10.0)).collect();
        let fetch = Fetch { at: 100, source: "questrade".into(), simulated: false };
        store.store("AAPL US Equity", Timeframe::D1, &bars, &fetch).unwrap();
        // A complete refreshed snapshot replaces the stored history.
        let fetch2 = Fetch { at: 200, ..fetch };
        let mut refreshed = bars.clone();
        refreshed[4] = Bar::new(4 * 86_400, 1.0, 3.0, 0.5, 2.5, 20.0);
        store.store("AAPL US Equity", Timeframe::D1, &refreshed, &fetch2).unwrap();
        let got = store.load("AAPL US Equity", Timeframe::D1, 3).unwrap();
        assert_eq!((got.len(), got[0].ts, got[2].close), (3, 2 * 86_400, 2.5));
        assert_eq!(store.fetched("AAPL US Equity", Timeframe::D1).unwrap(), Some(fetch2));
        assert!(store.load("AAPL US Equity", Timeframe::W1, 3).unwrap().is_empty());
    }

    #[test]
    fn freshness_follows_the_close() {
        // Tuesday 2026-10-20: fetched after the close stays fresh overnight, not into the session.
        assert!(is_fresh(at("2026-10-20T21:30:00Z"), at("2026-10-21T09:00:00Z")));
        assert!(!is_fresh(at("2026-10-20T21:30:00Z"), at("2026-10-21T15:00:00Z")));
        assert!(!is_fresh(at("2026-10-20T21:30:00Z"), at("2026-10-21T21:20:00Z")));
        // Friday's after-close fetch covers the weekend until Monday's open.
        assert!(is_fresh(at("2026-10-23T22:00:00Z"), at("2026-10-26T12:00:00Z")));
        assert!(!is_fresh(at("2026-10-23T22:00:00Z"), at("2026-10-26T14:00:00Z")));
        // During the session, 15 minutes.
        assert!(is_fresh(at("2026-10-21T15:00:00Z"), at("2026-10-21T15:10:00Z")));
        assert!(!is_fresh(at("2026-10-21T15:00:00Z"), at("2026-10-21T15:20:00Z")));
    }
}
