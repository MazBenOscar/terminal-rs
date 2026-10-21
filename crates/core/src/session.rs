//! Explicit display zones and US cash-equity session conventions.
//! Recurring holidays are supported; this is not a complete historical exchange calendar.
use crate::{Bar, Timeframe};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc, Weekday};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Zone {
    #[default]
    Utc,
    NewYork,
    London,
    Tokyo,
}
impl Zone {
    pub const ALL: [Self; 4] = [Self::Utc, Self::NewYork, Self::London, Self::Tokyo];
    pub fn label(self) -> &'static str {
        match self {
            Self::Utc => "UTC",
            Self::NewYork => "New York",
            Self::London => "London",
            Self::Tokyo => "Tokyo",
        }
    }
    pub fn local(self, ts: i64) -> NaiveDateTime {
        let d = DateTime::<Utc>::from_timestamp(ts, 0).unwrap_or_default();
        match self {
            Self::Utc => d.naive_utc(),
            Self::NewYork => d.with_timezone(&chrono_tz::America::New_York).naive_local(),
            Self::London => d.with_timezone(&chrono_tz::Europe::London).naive_local(),
            Self::Tokyo => d.with_timezone(&chrono_tz::Asia::Tokyo).naive_local(),
        }
    }
    pub fn label_timestamp(self, ts: i64, tf: Timeframe) -> i64 {
        if tf.is_intraday() { self.local(ts).and_utc().timestamp() } else { ts }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Session {
    #[default]
    Provider,
    UsRegular,
    UsExtended,
}
impl Session {
    pub const ALL: [Self; 3] = [Self::Provider, Self::UsRegular, Self::UsExtended];
    pub fn label(self) -> &'static str {
        match self {
            Self::Provider => "Provider bars",
            Self::UsRegular => "US regular hours",
            Self::UsExtended => "US extended hours (available bars)",
        }
    }
    pub fn contains(self, ts: i64) -> bool {
        if self == Self::Provider {
            return true;
        }
        let dt = Zone::NewYork.local(ts);
        let date = dt.date();
        if closed(date) {
            return false;
        }
        let minute = dt.hour() * 60 + dt.minute();
        let (open, close) = if self == Self::UsRegular { (open_minute(date), close_minute(date)) } else { (240, 1200) };
        minute >= open && minute < close
    }
    pub fn filter(self, bars: &[Bar], tf: Timeframe) -> Vec<Bar> {
        bars.iter().filter(|b| !tf.is_intraday() || self.contains(b.ts)).copied().collect()
    }
    /// Half-open aggregation interval, anchored to the NY session open when selected.
    pub fn interval(self, tf: Timeframe, ts: i64) -> (i64, i64) {
        if self == Self::Provider {
            return (tf.bucket_start(ts), next_bucket(tf, ts));
        }
        let date = Zone::NewYork.local(ts).date();
        if tf.is_intraday() {
            let open = local_ts(date, if self == Self::UsRegular { open_minute(date) } else { 240 });
            let end = local_ts(date, if self == Self::UsRegular { close_minute(date) } else { 1200 });
            let start = open + (ts - open).div_euclid(tf.seconds()) * tf.seconds();
            (start, (start + tf.seconds()).min(end))
        } else if tf == Timeframe::D1 {
            (
                local_ts(date, if self == Self::UsRegular { open_minute(date) } else { 240 }),
                local_ts(date, if self == Self::UsRegular { close_minute(date) } else { 1200 }),
            )
        } else {
            // Weekly/monthly labels use calendar buckets; completion waits for the
            // next period, never assumes the last observed session was the final one.
            let calendar = date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
            let start = DateTime::from_timestamp(tf.bucket_start(calendar), 0).unwrap().date_naive();
            let end = DateTime::from_timestamp(next_bucket(tf, calendar), 0).unwrap().date_naive();
            (local_ts(start, 0), local_ts(end, 0))
        }
    }
}
fn local_ts(day: NaiveDate, minute: u32) -> i64 {
    let local = day.and_time(chrono::NaiveTime::MIN) + Duration::minutes(i64::from(minute));
    chrono_tz::America::New_York
        .from_local_datetime(&local)
        .earliest()
        .map_or_else(|| local.and_utc().timestamp(), |dt| dt.timestamp())
}
pub fn next_bucket(tf: Timeframe, ts: i64) -> i64 {
    let start = tf.bucket_start(ts);
    if matches!(tf, Timeframe::MN1 | Timeframe::MN3) {
        let d = DateTime::from_timestamp(start, 0).unwrap().date_naive();
        let month = d.month0() + if tf == Timeframe::MN3 { 3 } else { 1 };
        let (y, m) = (d.year() + (month / 12) as i32, month % 12 + 1);
        NaiveDate::from_ymd_opt(y, m, 1).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp()
    } else {
        start + tf.seconds()
    }
}
fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}
fn observed(d: NaiveDate) -> NaiveDate {
    match d.weekday() {
        Weekday::Sat => d - Duration::days(1),
        Weekday::Sun => d + Duration::days(1),
        _ => d,
    }
}
fn nth(y: i32, m: u32, weekday: Weekday, n: u32) -> NaiveDate {
    let first = date(y, m, 1);
    first
        + Duration::days(
            ((7 + weekday.num_days_from_monday() - first.weekday().num_days_from_monday()) % 7 + 7 * (n - 1)) as i64,
        )
}
fn easter(y: i32) -> NaiveDate {
    let a = y % 19;
    let b = y / 100;
    let c = y % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    date(y, ((h + l - 7 * m + 114) / 31) as u32, ((h + l - 7 * m + 114) % 31 + 1) as u32)
}
pub fn closed(d: NaiveDate) -> bool {
    if let Some(day) = crate::exchange_calendar::day(d) {
        return day.is_none();
    }
    if matches!(d.weekday(), Weekday::Sat | Weekday::Sun) {
        return true;
    }
    let y = d.year();
    let memorial = (date(y, 6, 1) - Duration::days(1)).week(Weekday::Mon).first_day();
    let new_year = date(y, 1, 1);
    let new_year = if new_year.weekday() == Weekday::Sun { new_year + Duration::days(1) } else { new_year };
    [
        new_year,
        nth(y, 1, Weekday::Mon, 3),
        nth(y, 2, Weekday::Mon, 3),
        easter(y) - Duration::days(2),
        memorial,
        observed(date(y, 7, 4)),
        nth(y, 9, Weekday::Mon, 1),
        nth(y, 11, Weekday::Thu, 4),
        observed(date(y, 12, 25)),
    ]
    .contains(&d)
        || y >= 2022 && d == observed(date(y, 6, 19))
        || [
            date(2001, 9, 11),
            date(2001, 9, 12),
            date(2001, 9, 13),
            date(2001, 9, 14),
            date(2004, 6, 11),
            date(2007, 1, 2),
            date(2012, 10, 29),
            date(2012, 10, 30),
            date(2018, 12, 5),
            date(2025, 1, 9),
        ]
        .contains(&d)
}
pub fn open_minute(d: NaiveDate) -> u32 {
    crate::exchange_calendar::day(d).flatten().map_or(570, |d| d.open)
}
pub fn close_minute(d: NaiveDate) -> u32 {
    if let Some(day) = crate::exchange_calendar::day(d).flatten() {
        return day.close;
    }
    let black_friday = nth(d.year(), 11, Weekday::Thu, 4) + Duration::days(1);
    if d == black_friday || d.month() == 12 && d.day() == 24 || d.month() == 7 && d.day() == 3 { 780 } else { 960 }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ts(s: &str) -> i64 {
        DateTime::parse_from_rfc3339(s).unwrap().timestamp()
    }
    #[test]
    fn local_conversion_does_not_panic_at_dst_transitions() {
        assert_eq!(local_ts(date(2026, 3, 8), 150), ts("2026-03-08T02:30:00Z"));
        assert_eq!(local_ts(date(2026, 11, 1), 90), ts("2026-11-01T05:30:00Z"));
    }
    #[test]
    fn dst_and_regular_session_edges() {
        assert!(Session::UsRegular.contains(ts("2026-03-06T14:30:00Z")));
        assert!(Session::UsRegular.contains(ts("2026-03-09T13:30:00Z")));
        assert!(!Session::UsRegular.contains(ts("2026-03-09T13:29:00Z")));
        assert!(!Session::UsRegular.contains(ts("2026-03-09T20:00:00Z")));
        assert_eq!(Zone::NewYork.local(ts("2026-03-09T13:30:00Z")).hour(), 9);
    }
    #[test]
    fn holidays_and_short_days() {
        for s in ["2026-01-01", "2026-04-03", "2026-05-25", "2026-06-19", "2026-07-03", "2026-11-26", "2026-12-25"] {
            assert!(closed(s.parse().unwrap()), "{s}");
        }
        assert!(!closed(date(2021, 12, 31)));
        assert_eq!(close_minute(date(2026, 11, 27)), 780);
        assert!(!Session::UsRegular.contains(ts("2026-11-27T18:00:00Z")));
    }
    #[test]
    fn aggregation_anchors_and_month_completion_are_calendar_based() {
        let t = ts("2026-03-09T14:00:00Z");
        assert_eq!(
            Session::UsRegular.interval(Timeframe::H4, t),
            (ts("2026-03-09T13:30:00Z"), ts("2026-03-09T17:30:00Z"))
        );
        assert_eq!(next_bucket(Timeframe::MN1, ts("2026-02-15T00:00:00Z")), ts("2026-03-01T00:00:00Z"));
    }
}
