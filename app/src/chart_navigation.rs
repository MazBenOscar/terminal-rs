//! Timestamp-based chart navigation; no pixel or bar-index sharing across charts.
use serde::{Deserialize, Serialize};
use tr_chart::viewport::Viewport;
use tr_core::{Bar, Timeframe};

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Navigation {
    pub cursor: bool,
    pub range: bool,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DateRange {
    pub start: i64,
    pub end: i64,
}

pub fn index_at(bars: &[Bar], tf: Timeframe, ts: i64) -> Option<usize> {
    let i = bars.partition_point(|b| b.ts <= ts).checked_sub(1)?;
    // Never carry a Friday intraday pointer into a missing weekend session.
    (ts < if tf == Timeframe::MN1 {
        tr_core::session::next_bucket(tf, bars[i].ts)
    } else {
        bars[i].ts.saturating_add(tf.seconds())
    })
    .then_some(i)
}
fn time_at(bars: &[Bar], tf: Timeframe, i: f64) -> i64 {
    if i < 0. {
        return bars.first().map_or(0, |b| b.ts + (i * tf.seconds() as f64) as i64);
    }
    let idx = i.floor().max(0.) as usize;
    if let Some(bar) = bars.get(idx) {
        bar.ts
    } else {
        bars.last().map_or(0, |b| b.ts + ((i - bars.len() as f64 + 1.) * tf.seconds() as f64) as i64)
    }
}
pub fn date_range(bars: &[Bar], tf: Timeframe, v: &Viewport) -> Option<DateRange> {
    if bars.is_empty() {
        return None;
    }
    Some(DateRange {
        start: time_at(bars, tf, v.left()),
        end: time_at(bars, tf, v.right).max(time_at(bars, tf, v.left()) + tf.seconds()),
    })
}
pub fn apply(bars: &[Bar], tf: Timeframe, range: DateRange, v: &mut Viewport) {
    if bars.is_empty() {
        return;
    }
    let index = |ts| {
        if ts > bars.last().unwrap().ts {
            (bars.len() - 1) as f64 + (ts - bars.last().unwrap().ts) as f64 / tf.seconds() as f64
        } else {
            bars.partition_point(|b| b.ts < ts) as f64
        }
    };
    let (left, right) = (index(range.start), index(range.end));
    v.right = right;
    v.bar_px = v.width_px / (right - left).max(2.);
    v.clamp(bars.len());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pointers_use_timestamps_and_reject_session_gaps() {
        let bars = [Bar::new(1000, 1., 1., 1., 1., 1.), Bar::new(5000, 1., 1., 1., 1., 1.)];
        assert_eq!(index_at(&bars, Timeframe::M5, 1150), Some(0));
        assert_eq!(index_at(&bars, Timeframe::M5, 2000), None);
        assert_eq!(index_at(&bars, Timeframe::M5, 900), None);
    }
    #[test]
    fn date_ranges_map_to_other_bar_intervals() {
        let bars: Vec<_> = (0..100).map(|i| Bar::new(i * 300, 1., 1., 1., 1., 1.)).collect();
        let mut v = Viewport::fit_last(100, 50, 600.);
        apply(&bars, Timeframe::M5, DateRange { start: 3000, end: 9000 }, &mut v);
        assert_eq!(v.right, 30.);
        assert_eq!(v.visible_bars(), 20.);
    }
}
