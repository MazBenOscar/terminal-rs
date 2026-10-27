//! Shared timestamp column for feed panes (News, Filings): one grammar, local time.
use chrono::{DateTime, Datelike, TimeZone};

/// Fixed width of the time column, in monospace characters ("Oct 22", "14:32", "59m").
pub(crate) const WIDTH_CHARS: usize = 6;

/// The row label and its hover text for `ts`, in the user's local time zone.
/// `date_only` marks timestamps that carry no time of day (a filing date without an
/// acceptance time); those never show a clock or a relative age.
pub(crate) fn feed_time(ts: i64, now: i64, date_only: bool) -> (String, String) {
    feed_time_in(ts, now, date_only, &chrono::Local)
}

pub(crate) fn feed_time_in<Tz: TimeZone>(ts: i64, now: i64, date_only: bool, tz: &Tz) -> (String, String)
where
    Tz::Offset: std::fmt::Display,
{
    let (Some(time), Some(current)) = (DateTime::from_timestamp(ts, 0), DateTime::from_timestamp(now, 0)) else {
        return ("—".into(), "time unavailable".into());
    };
    // SEC date-only fields are calendar dates encoded at UTC midnight, not instants.
    // Preserve their day in western time zones instead of displaying the previous date.
    let date = time.date_naive();
    let (time, current) = (time.with_timezone(tz), current.with_timezone(tz));
    if date_only {
        let label = if date.year() == current.year() {
            date.format("%b %d").to_string()
        } else {
            date.format("%b'%y").to_string()
        };
        return (label, date.format("%a %b %d %Y (date only)").to_string());
    }
    let age = now - ts;
    let label = if (0..60).contains(&age) {
        "now".to_string()
    } else if (60..3600).contains(&age) {
        format!("{}m", age / 60)
    } else if time.date_naive() == current.date_naive() {
        time.format("%H:%M").to_string()
    } else if time.year() == current.year() {
        time.format("%b %d").to_string()
    } else {
        time.format("%b'%y").to_string()
    };
    (label, time.format("%a %b %d %Y %H:%M (UTC%:z)").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    // 2026-10-27 13:00 UTC = 09:00 EDT.
    const NOW: i64 = 1_793_106_000;

    #[test]
    fn labels_follow_age_then_local_day_then_year() {
        let edt = FixedOffset::west_opt(4 * 3600).unwrap();
        let label = |ts| feed_time_in(ts, NOW, false, &edt).0;
        assert_eq!(label(NOW - 20), "now");
        assert_eq!(label(NOW - 12 * 60), "12m");
        assert_eq!(label(NOW - 2 * 3600), "07:00");
        // 22:32 UTC on Oct 22 is 18:32 EDT the same day: shown as a date, never as UTC.
        assert_eq!(label(1_792_708_320), "Oct 22");
        assert_eq!(label(NOW - 400 * 86400), "Sep'25");
        for date_only in [false, true] {
            let (label, hover) = feed_time_in(NOW - 400 * 86400, NOW, date_only, &edt);
            assert_eq!(label, "Sep'25");
            assert!(label.chars().count() <= WIDTH_CHARS);
            assert!(hover.contains("2025"));
        }
        assert_eq!(feed_time_in(NOW + 30, NOW, false, &edt).0, "09:00");
    }

    #[test]
    fn local_midnight_decides_today_and_tooltips_name_the_offset() {
        let edt = FixedOffset::west_opt(4 * 3600).unwrap();
        // 03:30 UTC Oct 27 is 23:30 EDT Oct 26: yesterday locally.
        let (label, full) = feed_time_in(1_793_071_800, NOW, false, &edt);
        assert_eq!(label, "Oct 26");
        assert_eq!(full, "Mon Oct 26 2026 23:30 (UTC-04:00)");
    }

    #[test]
    fn date_only_calendar_day_does_not_shift_west_of_utc() {
        let west = FixedOffset::west_opt(7 * 3600).unwrap();
        let midnight =
            chrono::NaiveDate::from_ymd_opt(2026, 10, 22).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        assert_eq!(feed_time_in(midnight, NOW, true, &west).0, "Oct 22");
    }
    #[test]
    fn date_only_filings_never_show_a_clock() {
        let utc = FixedOffset::east_opt(0).unwrap();
        let (label, full) = feed_time_in(NOW - 3600, NOW, true, &utc);
        assert_eq!(label, "Oct 27");
        assert!(full.ends_with("(date only)"));
        assert!(label.chars().count() <= WIDTH_CHARS);
    }
}
