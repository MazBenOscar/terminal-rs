use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::Bar;

/// Chart intervals from 1 minute to 1 month.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub enum Timeframe {
    M1,
    M2,
    M3,
    M5,
    M15,
    M30,
    H1,
    H2,
    H4,
    #[default]
    D1,
    W1,
    MN1,
    MN3,
}

impl Timeframe {
    pub const ALL: [Timeframe; 13] = [
        Timeframe::M1,
        Timeframe::M2,
        Timeframe::M3,
        Timeframe::M5,
        Timeframe::M15,
        Timeframe::M30,
        Timeframe::H1,
        Timeframe::H2,
        Timeframe::H4,
        Timeframe::D1,
        Timeframe::W1,
        Timeframe::MN1,
        Timeframe::MN3,
    ];

    /// Nominal duration in seconds (months count as 30 days).
    pub fn seconds(self) -> i64 {
        match self {
            Timeframe::M1 => 60,
            Timeframe::M2 => 120,
            Timeframe::M3 => 180,
            Timeframe::M5 => 300,
            Timeframe::M15 => 900,
            Timeframe::M30 => 1800,
            Timeframe::H1 => 3600,
            Timeframe::H2 => 7200,
            Timeframe::H4 => 14_400,
            Timeframe::D1 => 86_400,
            Timeframe::W1 => 604_800,
            Timeframe::MN1 => 2_592_000,
            Timeframe::MN3 => 7_776_000,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Timeframe::M1 => "1m",
            Timeframe::M2 => "2m",
            Timeframe::M3 => "3m",
            Timeframe::M5 => "5m",
            Timeframe::M15 => "15m",
            Timeframe::M30 => "30m",
            Timeframe::H1 => "1H",
            Timeframe::H2 => "2H",
            Timeframe::H4 => "4H",
            Timeframe::D1 => "1D",
            Timeframe::W1 => "1W",
            Timeframe::MN1 => "1M",
            Timeframe::MN3 => "3M",
        }
    }

    pub fn is_intraday(self) -> bool {
        self < Timeframe::D1
    }

    /// Start of the bucket containing `ts` (weeks start Monday, months on the 1st, UTC).
    pub fn bucket_start(self, ts: i64) -> i64 {
        match self {
            Timeframe::W1 => {
                let d = day_of(ts);
                let monday = d - Duration::days(d.weekday().num_days_from_monday() as i64);
                midnight(monday)
            }
            Timeframe::MN1 | Timeframe::MN3 => {
                let d = day_of(ts);
                midnight(
                    NaiveDate::from_ymd_opt(
                        d.year(),
                        if self == Self::MN3 { (d.month0() / 3) * 3 + 1 } else { d.month() },
                        1,
                    )
                    .expect("valid month start"),
                )
            }
            tf => ts.div_euclid(tf.seconds()) * tf.seconds(),
        }
    }

    /// Only regular exchange multi-hour bars use the session grid. Provider and
    /// extended-hours histories retain their UTC alignment.
    pub fn session_aligned(self, session: crate::session::Session) -> bool {
        session == crate::session::Session::UsRegular && matches!(self, Self::H2 | Self::H4)
    }

    pub fn resample_session(self, bars: &[Bar], session: crate::session::Session) -> Vec<Bar> {
        if !self.session_aligned(session) {
            return self.resample(bars);
        }
        let mut out: Vec<Bar> = Vec::new();
        for b in bars.iter().filter(|b| session.contains(b.ts)) {
            let start = crate::session::bar_interval(self, session, b.ts).0;
            match out.last_mut() {
                Some(cur) if cur.ts == start => {
                    cur.high = cur.high.max(b.high);
                    cur.low = cur.low.min(b.low);
                    cur.close = b.close;
                    cur.volume += b.volume;
                }
                _ => out.push(Bar { ts: start, ..*b }),
            }
        }
        out
    }

    /// New intervals use existing provider intervals, then calendar-aligned aggregation.
    pub fn source_interval(self) -> Option<(Self, usize)> {
        match self {
            Self::M2 => Some((Self::M1, 2)),
            Self::M3 => Some((Self::M1, 3)),
            Self::H2 => Some((Self::H1, 2)),
            Self::MN3 => Some((Self::MN1, 3)),
            _ => None,
        }
    }

    /// Aggregate bars of a finer timeframe into this timeframe.
    pub fn resample(self, bars: &[Bar]) -> Vec<Bar> {
        let mut out: Vec<Bar> = Vec::with_capacity(bars.len() / 2 + 1);
        for b in bars {
            let start = crate::session::bar_interval(self, crate::session::Session::Provider, b.ts).0;
            match out.last_mut() {
                Some(cur) if cur.ts == start => {
                    cur.high = cur.high.max(b.high);
                    cur.low = cur.low.min(b.low);
                    cur.close = b.close;
                    cur.volume += b.volume;
                }
                _ => out.push(Bar { ts: start, ..*b }),
            }
        }
        out
    }
}

fn day_of(ts: i64) -> NaiveDate {
    DateTime::<Utc>::from_timestamp(ts, 0).map(|d| d.date_naive()).unwrap_or_default()
}

fn midnight(d: NaiveDate) -> i64 {
    d.and_hms_opt(0, 0, 0).expect("midnight").and_utc().timestamp()
}

impl fmt::Display for Timeframe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for Timeframe {
    type Err = String;

    /// Accepts TradingView/Bloomberg-ish spellings: `1`, `1m`, `5`, `60`, `1h`, `4H`, `D`, `1D`, `W`, `M`, `1MO`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        // A bare or trailing uppercase `M` means month; lowercase `m` means minute.
        if t == "3M" || t.eq_ignore_ascii_case("3mo") || t.eq_ignore_ascii_case("quarter") {
            return Ok(Self::MN3);
        }
        if t == "M" || t == "1M" || t.eq_ignore_ascii_case("1mo") || t.eq_ignore_ascii_case("mo") {
            return Ok(Timeframe::MN1);
        }
        Ok(match t.to_ascii_lowercase().as_str() {
            "1" | "1m" | "1min" => Timeframe::M1,
            "2" | "2m" | "2min" => Self::M2,
            "3" | "3m" | "3min" => Self::M3,
            "120" | "2h" => Self::H2,
            "5" | "5m" | "5min" => Timeframe::M5,
            "15" | "15m" | "15min" => Timeframe::M15,
            "30" | "30m" | "30min" => Timeframe::M30,
            "60" | "1h" | "h" => Timeframe::H1,
            "240" | "4h" => Timeframe::H4,
            "d" | "1d" | "day" => Timeframe::D1,
            "w" | "1w" | "week" => Timeframe::W1,
            "month" => Timeframe::MN1,
            _ => return Err(format!("unknown timeframe '{s}'")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> i64 {
        DateTime::parse_from_rfc3339(s).unwrap().timestamp()
    }

    #[test]
    fn regular_multi_hour_boundaries_normal_dst_and_half_day() {
        use crate::session::Session;
        // Trading days immediately before and after each DST transition.
        for (day, hour) in [("2026-03-06", 14), ("2026-03-09", 13), ("2026-10-30", 13), ("2026-11-02", 14)] {
            let open = ts(&format!("{day}T{hour}:30:00Z"));
            for tf in [Timeframe::H2, Timeframe::H4] {
                for minute in [0, 29, 119, 120, 239, 240, 359, 360, 389] {
                    let t = open + minute * 60;
                    let start = open + (t - open) / tf.seconds() * tf.seconds();
                    assert_eq!(
                        Session::UsRegular.interval(tf, t),
                        (start, (start + tf.seconds()).min(open + 390 * 60))
                    );
                }
            }
        }
        let open = ts("2026-11-27T14:30:00Z");
        let close = ts("2026-11-27T18:00:00Z");
        assert_eq!(Session::UsRegular.interval(Timeframe::H2, close - 1), (open + 7200, close));
        assert_eq!(Session::UsRegular.interval(Timeframe::H4, close - 1), (open, close));
        assert!(!Session::UsRegular.contains(close));
    }

    #[test]
    fn regular_two_hour_from_half_hours_equals_manual_aggregation() {
        use crate::session::Session;
        let open = ts("2026-03-09T13:30:00Z");
        let bars: Vec<_> = (-1..14)
            .map(|i| Bar::new(open + i * 1800, i as f64, i as f64 + 2., i as f64 - 1., i as f64 + 1., 10.))
            .collect();
        let actual = Timeframe::H2.resample_session(&bars, Session::UsRegular);
        let expected = vec![
            Bar::new(open, 0., 5., -1., 4., 40.),
            Bar::new(open + 7200, 4., 9., 3., 8., 40.),
            Bar::new(open + 14400, 8., 13., 7., 12., 40.),
            Bar::new(open + 21600, 12., 14., 11., 13., 10.),
        ];
        assert_eq!(actual, expected);
        assert_eq!(actual[0].ts, open);
        let half_open = ts("2026-11-27T14:30:00Z");
        let half_bars: Vec<_> = (0..10).map(|i| Bar::new(half_open + i * 1800, 1., 1., 1., 1., 1.)).collect();
        assert_eq!(Timeframe::H4.resample_session(&half_bars, Session::UsRegular)[0].volume, 7.);
    }

    #[test]
    fn crypto_provider_and_extended_multi_hour_buckets_remain_utc() {
        use crate::session::Session;
        let bars = vec![Bar::new(ts("2026-03-09T13:30:00Z"), 1., 2., 0., 1., 10.)];
        for tf in [Timeframe::H2, Timeframe::H4] {
            for session in [Session::Provider, Session::UsExtended] {
                assert_eq!(tf.resample_session(&bars, session), tf.resample(&bars));
                assert_eq!(session.interval(tf, bars[0].ts).0, tf.bucket_start(bars[0].ts));
            }
        }
    }

    #[test]
    fn parse_spellings() {
        assert_eq!("2".parse::<Timeframe>().unwrap(), Timeframe::M2);
        assert_eq!("3m".parse::<Timeframe>().unwrap(), Timeframe::M3);
        assert_eq!("2H".parse::<Timeframe>().unwrap(), Timeframe::H2);
        assert_eq!("3M".parse::<Timeframe>().unwrap(), Timeframe::MN3);
        assert_eq!("1m".parse::<Timeframe>().unwrap(), Timeframe::M1);
        assert_eq!("1M".parse::<Timeframe>().unwrap(), Timeframe::MN1);
        assert_eq!("4H".parse::<Timeframe>().unwrap(), Timeframe::H4);
        assert_eq!("D".parse::<Timeframe>().unwrap(), Timeframe::D1);
        assert_eq!("240".parse::<Timeframe>().unwrap(), Timeframe::H4);
        assert!("7x".parse::<Timeframe>().is_err());
    }

    #[test]
    fn week_and_month_buckets() {
        let may = NaiveDate::from_ymd_opt(2026, 5, 12).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        let april = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        let july = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        assert_eq!(Timeframe::MN3.bucket_start(may), april);
        assert_eq!(crate::session::next_bucket(Timeframe::MN3, may), july);
        let quarter = crate::sample::generate("AAPL", Timeframe::MN3, 120, july);
        assert!(quarter.windows(2).all(|w| w[0].ts < w[1].ts));
        // 2026-10-07 is a Wednesday.
        let wed = NaiveDate::from_ymd_opt(2026, 10, 7).unwrap().and_hms_opt(15, 30, 0).unwrap().and_utc().timestamp();
        let mon = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        let first = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        assert_eq!(Timeframe::W1.bucket_start(wed), mon);
        assert_eq!(Timeframe::MN1.bucket_start(wed), first);
        assert_eq!(Timeframe::H1.bucket_start(wed), wed - 30 * 60);
    }

    #[test]
    fn resample_minutes_to_hour() {
        let bars: Vec<Bar> =
            (0..120).map(|i| Bar::new(i * 60, i as f64, i as f64 + 1.0, i as f64 - 1.0, i as f64 + 0.5, 1.0)).collect();
        let h = Timeframe::H1.resample(&bars);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0], Bar::new(0, 0.0, 60.0, -1.0, 59.5, 60.0));
        assert_eq!(h[1].ts, 3600);
        assert_eq!(h[1].open, 60.0);
    }
}
