//! Measurements, volume-based drawings and long/short risk-reward tools.
use tr_core::{
    Bar,
    chart_analytics::{self, Profile},
};

/// Inclusive containing-bar range, independent of anchor direction.
pub fn bar_range(bars: &[Bar], d: &Drawing) -> std::ops::Range<usize> {
    if bars.is_empty() {
        return 0..0;
    }
    let index = |ts| bars.partition_point(|b| b.ts <= ts).saturating_sub(1);
    let a = index(d.a.ts);
    let b = index(d.b.ts);
    a.min(b)..a.max(b) + 1
}

pub fn range_label(d: &Drawing, bars: &[Bar], intervals: usize, decimals: usize) -> String {
    let full = super::measure_label(d.a, d.b, intervals, decimals);
    if !matches!(d.tool, Tool::PriceRange | Tool::DateRange | Tool::DatePriceRange) {
        return full;
    }
    let (price, date) = full.split_once(" · ").unwrap();
    let volume: f64 = bars[bar_range(bars, d)]
        .iter()
        .filter_map(|b| (b.volume.is_finite() && b.volume >= 0.).then_some(b.volume))
        .fold(0., |sum, volume| sum + volume);
    match d.tool {
        Tool::PriceRange => price.to_owned(),
        Tool::DateRange => format!("{date} · Volume {volume:.2}"),
        Tool::DatePriceRange => format!("{full} · Volume {volume:.2}"),
        _ => full,
    }
}

pub fn row_count(rows: usize) -> usize {
    rows.clamp(4, 200)
}

pub fn anchored_vwap(bars: &[Bar], d: &Drawing) -> Vec<f64> {
    chart_analytics::anchored_vwap(bars, d.a.ts)
}

pub fn fixed_profile(bars: &[Bar], d: &Drawing) -> Option<Profile> {
    chart_analytics::volume_profile(&bars[bar_range(bars, d)], row_count(d.profile_rows))
}

/// Data-backed primitives share the same painting and hit-testing path.
pub fn series_geometry(g: &Geometry<'_>, bars: &[Bar], x: &dyn Fn(usize) -> f64) -> Vec<LabeledShape> {
    let d = g.drawing;
    if d.tool == Tool::AnchoredVwap {
        let values = anchored_vwap(bars, d);
        let mut shapes = vec![];
        for i in 1..values.len() {
            if values[i - 1].is_finite() && values[i].is_finite() {
                let a = [x(i - 1), (g.price_y)(values[i - 1])];
                let b = [x(i), (g.price_y)(values[i])];
                if a.into_iter().chain(b).all(f64::is_finite) {
                    shapes.push((Shape::Segment(a, b), None));
                }
            }
        }
        // A single populated bar still has a selectable mark.
        if shapes.is_empty()
            && let Some(i) = values.iter().position(|p| p.is_finite())
        {
            let y = (g.price_y)(values[i]);
            shapes.push((Shape::Segment([x(i), y], [x(i) + 3., y]), None));
        }
        return shapes;
    }
    let mut shapes = geometry(g);
    if let Some(profile) = fixed_profile(bars, d) {
        let range = bar_range(bars, d);
        let left = x(range.start);
        let width = (x(range.end - 1) - left).abs().max(40.);
        let max = profile.volume[profile.poc];
        for (i, volume) in profile.volume.iter().enumerate() {
            let bottom = (g.price_y)(profile.low + i as f64 * profile.step);
            let top = (g.price_y)(profile.low + (i + 1) as f64 * profile.step);
            if !top.is_finite() || !bottom.is_finite() {
                continue;
            }
            let label = if i == profile.poc {
                Some("Point of control".into())
            } else if (profile.value_low..=profile.value_high).contains(&i) {
                Some("Value area".into())
            } else {
                None
            };
            shapes.push((Shape::Rect([left, top], [left + width * volume / max, bottom]), label));
        }
    }
    shapes
}

use super::{Geometry, LabeledShape};
use crate::drawings::{Drawing, Shape, Tool};

pub fn label(a: super::Anchor, b: super::Anchor, bar_intervals: usize, decimals: usize) -> String {
    let change = b.price - a.price;
    let pct = if a.price == 0. { "n/a".into() } else { format!("{:+.2}%", change / a.price.abs() * 100.) };
    let secs = b.ts.abs_diff(a.ts);
    let elapsed = format_duration(secs);
    format!("{change:+.decimals$} ({pct}) · {bar_intervals} bars · {elapsed}")
}

fn format_duration(secs: u64) -> String {
    let (major, minor, major_unit, minor_unit) = if secs >= 86_400 {
        (secs / 86_400, secs % 86_400 / 3600, "d", "h")
    } else if secs >= 3600 {
        (secs / 3600, secs % 3600 / 60, "h", "m")
    } else {
        (secs / 60, secs % 60, "m", "s")
    };
    match (major, minor) {
        (0, _) => format!("{minor}{minor_unit}"),
        (_, 0) => format!("{major}{major_unit}"),
        _ => format!("{major}{major_unit} {minor}{minor_unit}"),
    }
}

pub fn point_count(tool: Tool) -> usize {
    match tool {
        Tool::AnchoredVwap => 1,
        Tool::LongPosition | Tool::ShortPosition => 3,
        _ => 2,
    }
}

pub fn risk_reward(d: &Drawing) -> Option<(f64, f64, f64)> {
    let c = d.c?;
    if d.tool == Tool::LongPosition && !(d.b.price < d.a.price && c.price > d.a.price) {
        return None;
    }
    if d.tool == Tool::ShortPosition && !(d.b.price > d.a.price && c.price < d.a.price) {
        return None;
    }
    let risk = (d.a.price - d.b.price).abs();
    let reward = (c.price - d.a.price).abs();
    (risk.is_finite() && reward.is_finite() && risk > 0.).then_some((risk, reward, reward / risk))
}

pub fn handles(d: &Drawing) -> Vec<super::Anchor> {
    super::anchor_handles(d, point_count(d.tool))
}
pub fn hit_distance(g: &Geometry<'_>, p: super::Point) -> f64 {
    super::distance_to_shapes(geometry(g), p)
}

pub fn geometry(g: &Geometry<'_>) -> Vec<LabeledShape> {
    let [a, b, c] = g.points;
    let [left, top, right, bottom] = g.bounds;
    match g.drawing.tool {
        Tool::Measure | Tool::DatePriceRange => return vec![(Shape::Rect(a, b), Some(g.measure_label.clone()))],
        Tool::PriceRange => return vec![(Shape::Rect([left, a[1]], [right, b[1]]), Some(g.measure_label.clone()))],
        Tool::DateRange => return vec![(Shape::Rect([a[0], top], [b[0], bottom]), Some(g.measure_label.clone()))],
        // Data-backed geometry replaces these anchor guides in the app.
        Tool::AnchoredVwap => return vec![(Shape::Segment(a, [right, a[1]]), None)],
        Tool::FixedRangeVolumeProfile => return vec![(Shape::Rect([a[0], top], [b[0], bottom]), None)],
        _ => {}
    }
    if let Some((risk, reward, ratio)) = risk_reward(g.drawing) {
        let right = b[0].max(c[0]).max(a[0] + 35.);
        vec![
            (Shape::Rect(a, [right, b[1]]), Some(format!("Risk {risk:.2} / unit"))),
            (Shape::Rect(a, [right, c[1]]), Some(format!("Reward {reward:.2} / unit · R/R {ratio:.2}"))),
        ]
    } else {
        vec![(Shape::Segment(a, b), Some("Set entry, stop, then target on the opposite side".into()))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawings::Anchor;

    fn drawing(tool: Tool) -> Drawing {
        Drawing::new(tool, Anchor { ts: 86400, price: 100. }, Anchor { ts: 259200, price: 110. })
    }
    fn bars() -> Vec<Bar> {
        vec![
            Bar::new(0, 1., 1., 1., 1., 999.),
            Bar::new(86400, 10., 10., 10., 10., 10.),
            Bar::new(172800, 20., 20., 20., 20., 20.),
            Bar::new(259200, 30., 30., 30., 30., 30.),
            Bar::new(345600, 40., 40., 40., 40., 999.),
        ]
    }

    #[test]
    fn durations_omit_zero_parts_and_preserve_mixed_units() {
        for (secs, expected) in [
            (0, "0s"),
            (45, "45s"),
            (60, "1m"),
            (61, "1m 1s"),
            (3 * 3600, "3h"),
            (3 * 3600 + 15 * 60, "3h 15m"),
            (86_400, "1d"),
            (350 * 86_400, "350d"),
            (2 * 86_400 + 3 * 3600, "2d 3h"),
        ] {
            assert_eq!(format_duration(secs), expected);
            let a = Anchor { ts: 0, price: 100. };
            let b = Anchor { ts: secs as i64, price: 110. };
            assert!(label(a, b, 1, 2).ends_with(expected));
            assert!(label(b, a, 1, 2).ends_with(expected));
        }
    }

    #[test]
    fn range_labels_delta_percent_bars_elapsed_and_inclusive_volume() {
        let mut d = drawing(Tool::DatePriceRange);
        assert_eq!(range_label(&d, &bars(), 2, 2), "+10.00 (+10.00%) · 2 bars · 2d · Volume 60.00");
        d.tool = Tool::PriceRange;
        assert_eq!(range_label(&d, &bars(), 2, 2), "+10.00 (+10.00%)");
        d.tool = Tool::DateRange;
        std::mem::swap(&mut d.a, &mut d.b);
        assert_eq!(range_label(&d, &bars(), 2, 2), "2 bars · 2d · Volume 60.00");
        d.tool = Tool::DatePriceRange;
        assert!(range_label(&d, &bars(), 2, 2).starts_with("-10.00 (-9.09%)"));
        d.a.price = 0.;
        assert!(range_label(&d, &bars(), 2, 2).contains("n/a"));
        d.b = d.a;
        assert_eq!(bar_range(&bars(), &d).len(), 1);
        assert!(range_label(&d, &[], 0, 2).ends_with("Volume 0.00"));
    }

    #[test]
    fn drawing_vwap_matches_analytics_from_editable_anchor() {
        let mut d = drawing(Tool::AnchoredVwap);
        for ts in [0, 86400, 180000, 259200] {
            d.a.ts = ts;
            let actual = anchored_vwap(&bars(), &d);
            let expected = chart_analytics::anchored_vwap(&bars(), ts);
            assert!(actual.iter().zip(expected).all(|(a, b)| *a == b || (a.is_nan() && b.is_nan())));
            assert_eq!(handles(&d), vec![d.a]);
        }
    }

    #[test]
    fn fixed_profile_poc_value_area_and_row_bounds() {
        let bars = vec![
            Bar::new(1, 0., 4., 0., 2., 40.),
            Bar::new(2, 1., 1., 1., 1., 60.),
            Bar::new(3, 100., 100., 100., 100., 9999.),
        ];
        let mut d =
            Drawing::new(Tool::FixedRangeVolumeProfile, Anchor { ts: 1, price: 0. }, Anchor { ts: 2, price: 4. });
        assert_eq!(d.profile_rows, 24);
        d.profile_rows = 4;
        let p = fixed_profile(&bars, &d).unwrap();
        assert_eq!(p.volume, vec![10., 70., 10., 10.]);
        assert_eq!((p.poc, p.value_low, p.value_high), (1, 1, 1));
        for (requested, expected) in [(0, 4), (3, 4), (4, 4), (24, 24), (200, 200), (201, 200), (usize::MAX, 200)] {
            d.profile_rows = requested;
            assert_eq!(row_count(requested), expected);
            assert_eq!(fixed_profile(&bars, &d).unwrap().volume.len(), expected);
        }
        std::mem::swap(&mut d.a, &mut d.b);
        assert_eq!(fixed_profile(&bars, &d).unwrap().poc, 50);
    }
}
