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

/// Story headers retain the date and an explicit age beyond twelve hours.
/// Unlike a feed row, an inferred timestamp must never look like a real time.
pub(crate) fn story_time(ts: i64, now: i64, inferred: bool) -> (String, String) {
    story_time_in(ts, now, inferred, &chrono::Local)
}
fn story_time_in<Tz: TimeZone>(ts: i64, now: i64, inferred: bool, tz: &Tz) -> (String, String)
where
    Tz::Offset: std::fmt::Display,
{
    if inferred {
        return ("time unknown".into(), "time unknown".into());
    }
    let (label, hover) = feed_time_in(ts, now, false, tz);
    let age = now.saturating_sub(ts);
    if age <= 12 * 3600 {
        return (label, hover);
    }
    let Some(time) = DateTime::from_timestamp(ts, 0) else { return (label, hover) };
    let time = time.with_timezone(tz);
    let age = if age >= 86400 { format!("{}d", age / 86400) } else { format!("{}h", age / 3600) };
    (format!("{} · {age} old", time.format("%b %d")), hover)
}

/// Feed controls are vector-drawn, density-scaled and input-driven (no animation timer).
pub(crate) fn icon_button(ui: &mut egui::Ui, icon: crate::widgets::Icon, label: &str) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(crate::theme::dp(22.), crate::theme::dp(20.)), egui::Sense::click());
    if response.hovered() || response.has_focus() {
        ui.painter().rect_filled(rect, crate::theme::CONTROL_RADIUS, crate::theme::control_hover());
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            crate::theme::CONTROL_RADIUS,
            egui::Stroke::new(crate::theme::dp(1.), crate::theme::accent()),
            egui::StrokeKind::Inside,
        );
    }
    crate::widgets::draw_icon(ui.painter(), rect.shrink(crate::theme::dp(4.)), icon, crate::theme::text());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label));
    response.on_hover_text(label)
}

pub(crate) fn dot(ui: &mut egui::Ui, colour: egui::Color32) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(crate::theme::dp(8.), crate::theme::dp(16.)), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), crate::theme::dp(2.5), colour);
    response
}

/// The main column owns the row. A short tail never leaves a fixed empty block.
pub(crate) fn tail_budget(row: f32, available: f32) -> f32 {
    (row * 0.30).min((available - row * 0.60 - crate::theme::dp(6.)).max(0.))
}

/// One actionable problem, otherwise source and age. Full diagnostic stays in the menu/tip.
pub(crate) fn compact_status(status: &str) -> String {
    let lower = status.to_ascii_lowercase();
    for (needle, label) in [
        ("offline", "Offline"),
        ("needs contact", "Needs contact"),
        ("needs a finnhub key", "Needs key"),
        ("unavailable", "Source unavailable"),
        ("reserve", "Budget reserved"),
    ] {
        if lower.contains(needle) {
            return label.into();
        }
    }
    if let Some(at) = lower.find("paused until") {
        return status[at..].split(" · ").next().unwrap_or(status).into();
    }
    status.split(" · ").take(2).collect::<Vec<_>>().join(" · ")
}

pub(crate) fn tooltip(response: &egui::Response) -> egui::Tooltip<'_> {
    let mut tooltip = egui::Tooltip::for_enabled(response);
    tooltip.popup = tooltip
        .popup
        .frame(
            egui::Frame::popup(&response.ctx.global_style())
                .fill(crate::theme::card())
                .stroke(egui::Stroke::new(crate::theme::dp(1.), crate::theme::muted()))
                .inner_margin(egui::Margin::symmetric(crate::theme::dp(10.) as i8, crate::theme::dp(8.) as i8)),
        )
        .width(crate::theme::dp(380.));
    tooltip
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    #[test]
    fn tails_leave_sixty_percent_for_main_text_when_fixed_columns_allow_it() {
        for row in [340., 600., 1200.] {
            for fixed in [60., 110., 180.] {
                let available = row - fixed;
                let tail = tail_budget(row, available);
                assert!(tail <= row * 0.30);
                assert!(tail >= 0.);
                if available >= row * 0.60 + crate::theme::dp(6.) {
                    assert!(available - tail - crate::theme::dp(6.) >= row * 0.60 - 0.001);
                } else {
                    assert_eq!(tail, 0.);
                }
            }
        }
    }

    #[test]
    fn compact_status_keeps_actionable_failures_not_backlog_prose() {
        assert_eq!(
            compact_status("SEC EDGAR · updated now · Reading insider forms: 216 left"),
            "SEC EDGAR · updated now"
        );
        assert_eq!(compact_status("SEC EDGAR · Offline (test instance)"), "Offline");
        assert_eq!(compact_status("SEC EDGAR · Needs contact email (Settings)"), "Needs contact");
        assert_eq!(compact_status("SEC EDGAR · waiting: reserved feed budget"), "Budget reserved");
    }

    // 2026-10-27 13:00 UTC = 09:00 EDT.
    const NOW: i64 = 1_793_106_000;

    #[test]
    fn story_header_age_and_hover_use_real_time_only() {
        for (hours, expected) in [(1, "12:00"), (12, "01:00"), (13, "Oct 27 · 13h old"), (120, "Oct 22 · 5d old")] {
            let (label, hover) = story_time_in(NOW - hours * 3600, NOW, false, &chrono::Utc);
            assert_eq!(label, expected);
            assert!(hover.contains("2026"));
            assert!(hover.contains("UTC+00:00"));
        }
        assert_eq!(story_time_in(NOW, NOW, true, &chrono::Utc), ("time unknown".into(), "time unknown".into()));
    }
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
