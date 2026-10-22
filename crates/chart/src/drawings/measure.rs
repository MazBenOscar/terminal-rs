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

    fn geometry_ctx<'a>(d: &'a Drawing, measure_label: String) -> Geometry<'a> {
        Geometry {
            drawing: d,
            points: [[10., 40.], [80., 20.], [80., 20.]],
            pattern_points: Vec::new(),
            bounds: [0., 0., 100., 100.],
            price_y: &|p| p,
            measure_label,
            text_size: [0., 0.],
            closes: &[],
            project: &|a| [a.ts as f64, a.price],
        }
    }

    #[test]
    fn measurer_geometry_attaches_the_range_label_including_date_range_volume() {
        let bars = bars();
        for (tool, expect_volume) in [(Tool::PriceRange, false), (Tool::DateRange, true), (Tool::DatePriceRange, true)]
        {
            let d = drawing(tool);
            let label = range_label(&d, &bars, 2, 2);
            let shapes = geometry(&geometry_ctx(&d, label.clone()));
            assert_eq!(shapes.len(), 1, "{tool:?}");
            let attached = shapes[0].1.as_deref();
            assert_eq!(attached, Some(label.as_str()), "{tool:?} must paint its measure label");
            assert_eq!(attached.unwrap().contains("Volume"), expect_volume, "{tool:?}");
        }
    }

    #[test]
    fn bar_range_is_inclusive_and_handles_reversed_zero_width_and_off_data_anchors() {
        let bars = bars();
        let mut d = drawing(Tool::DatePriceRange);
        assert_eq!(bar_range(&bars, &d), 1..4);
        std::mem::swap(&mut d.a, &mut d.b);
        assert_eq!(bar_range(&bars, &d), 1..4, "p2 before p1 must not invert or drop bars");
        d.b = d.a;
        assert_eq!(bar_range(&bars, &d), 3..4, "zero-width range is the containing bar");
        d.a = Anchor { ts: i64::MIN, price: 1. };
        d.b = Anchor { ts: i64::MIN, price: 2. };
        assert_eq!(bar_range(&bars, &d), 0..1, "both anchors before first bar snap to bar 0");
        d.a = Anchor { ts: i64::MAX, price: 1. };
        d.b = Anchor { ts: i64::MAX, price: 2. };
        assert_eq!(bar_range(&bars, &d), 4..5, "both anchors after last bar snap to the last bar");
        d.a = Anchor { ts: i64::MIN, price: 1. };
        d.b = Anchor { ts: i64::MAX, price: 2. };
        assert_eq!(bar_range(&bars, &d), 0..5, "spanning the whole series includes every bar");
        assert!(bar_range(&[], &d).is_empty());
    }

    #[test]
    fn range_volume_skips_missing_negative_and_nonfinite_bars_but_keeps_gap_bars() {
        // ts 0, 86400 then a weekend-style gap to 259200: all three bars sit in the range.
        let bars = vec![
            Bar::new(0, 1., 1., 1., 1., 10.),
            Bar::new(86400, 1., 1., 1., 1., f64::NAN),
            Bar::new(172800, 1., 1., 1., 1., -5.),
            Bar::new(259200, 1., 1., 1., 1., f64::INFINITY),
            Bar::new(345600, 1., 1., 1., 1., 7.),
            Bar::new(432000, 1., 1., 1., 1., 3.),
        ];
        let mut d = Drawing::new(Tool::DateRange, Anchor { ts: 0, price: 1. }, Anchor { ts: 432000, price: 1. });
        assert_eq!(range_label(&d, &bars, 5, 2), "5 bars · 5d · Volume 20.00");
        // Reversed anchors and a range that only covers the gap neighbourhood.
        std::mem::swap(&mut d.a, &mut d.b);
        assert!(range_label(&d, &bars, 5, 2).ends_with("Volume 20.00"));
        d.a = Anchor { ts: 86400, price: 1. };
        d.b = Anchor { ts: 345600, price: 1. };
        assert_eq!(range_label(&d, &bars, 3, 2), "3 bars · 3d · Volume 7.00");
    }

    #[test]
    fn anchored_vwap_anchor_before_first_after_last_on_gap_and_zero_volume() {
        let daily = [
            Bar::new(86_400, 10., 10., 10., 10., 2.),
            Bar::new(172_800, 20., 20., 20., 20., 2.),
            Bar::new(345_600, 30., 30., 30., 30., 2.),
        ];
        let mut d = Drawing::new(Tool::AnchoredVwap, Anchor { ts: 0, price: 0. }, Anchor { ts: 0, price: 0. });
        // Before the first bar: whole series from bar 0.
        d.a.ts = 0;
        let values = anchored_vwap(&daily, &d);
        assert_eq!(values, vec![10., 15., 20.]);
        // On the weekend gap between bar 1 and bar 2: snap to the prior bar.
        d.a.ts = 250_000;
        let values = anchored_vwap(&daily, &d);
        assert!(values[0].is_nan());
        assert_eq!(&values[1..], &[20., 25.], "gap anchor includes the containing prior bar");
        // After the last bar: only the last bar is populated.
        d.a.ts = i64::MAX;
        let values = anchored_vwap(&daily, &d);
        assert!(values[..2].iter().all(|v| v.is_nan()));
        assert_eq!(values[2], 30.);
        // A zero-volume bar carries the running VWAP forward without inventing prints.
        let mut carry = daily;
        carry[1].volume = 0.;
        d.a.ts = 0;
        assert_eq!(anchored_vwap(&carry, &d), vec![10., 10., 20.]);
        // All-zero volume leaves the series undefined but still length-safe.
        let dry: Vec<_> = daily.iter().map(|b| Bar { volume: 0., ..*b }).collect();
        d.a.ts = 0;
        assert!(anchored_vwap(&dry, &d).iter().all(|v| v.is_nan()));
        assert!(anchored_vwap(&[], &d).is_empty());
    }

    #[test]
    fn fixed_profile_rejects_empty_zero_volume_and_invalid_bars_and_handles_degenerate_range() {
        let mut d =
            Drawing::new(Tool::FixedRangeVolumeProfile, Anchor { ts: 0, price: 0. }, Anchor { ts: 1, price: 1. });
        assert!(fixed_profile(&[], &d).is_none());
        assert!(fixed_profile(&[Bar::new(0, 1., 1., 1., 1., 0.)], &d).is_none(), "zero total volume");
        assert!(
            fixed_profile(&[Bar::new(0, 1., f64::NAN, 1., 1., 5.), Bar::new(1, 1., f64::NAN, f64::NAN, 1., 5.)], &d)
                .is_none(),
            "no finite high/low"
        );
        // Degenerate price range high == low still produces a bounded single-bin profile.
        let flat = [Bar::new(0, 5., 5., 5., 5., 12.)];
        d.a = Anchor { ts: 0, price: 5. };
        d.b = Anchor { ts: 0, price: 5. };
        let p = fixed_profile(&flat, &d).unwrap();
        assert_eq!(p.low, 5.);
        assert!(p.step > 0.);
        assert_eq!(p.volume.iter().sum::<f64>(), 12.);
        assert_eq!(p.poc, 0);
        assert_eq!((p.value_low, p.value_high), (0, 0));
        assert!(p.volume.len() == row_count(d.profile_rows));
    }

    #[test]
    fn poc_ties_are_deterministic_and_value_area_stays_inside_tiny_profiles() {
        // Four equal bins: POC must be stable across calls (highest bin on ties).
        let bars =
            [Bar::new(1, 0., 4., 0., 0., 40.), Bar::new(2, 0., 4., 0., 0., 40.), Bar::new(3, 0., 4., 0., 0., 40.)];
        let mut d =
            Drawing::new(Tool::FixedRangeVolumeProfile, Anchor { ts: 1, price: 0. }, Anchor { ts: 3, price: 4. });
        d.profile_rows = 4;
        let p = fixed_profile(&bars, &d).unwrap();
        assert_eq!(p.volume, vec![30., 30., 30., 30.]);
        assert_eq!(p.poc, 3, "ties resolve to the highest-price bin");
        assert!(p.value_low <= p.poc && p.poc <= p.value_high);
        assert!(p.value_high < p.volume.len());
        for _ in 0..3 {
            assert_eq!(fixed_profile(&bars, &d).unwrap().poc, p.poc);
        }
        // Single-bar, single-price profile: value area is the POC alone.
        d.profile_rows = 4;
        let tiny = [Bar::new(1, 2., 2., 2., 2., 9.)];
        d.a = Anchor { ts: 1, price: 2. };
        d.b = Anchor { ts: 1, price: 2. };
        let p = fixed_profile(&tiny, &d).unwrap();
        assert_eq!((p.poc, p.value_low, p.value_high), (0, 0, 0));
    }

    #[test]
    fn series_geometry_emits_finite_profile_rows_and_vwap_segments() {
        let bars = bars();
        let mut d = Drawing::new(
            Tool::FixedRangeVolumeProfile,
            Anchor { ts: 86400, price: 10. },
            Anchor { ts: 259200, price: 30. },
        );
        d.profile_rows = 8;
        let g = geometry_ctx(&d, String::new());
        let shapes = series_geometry(&g, &bars, &|i| i as f64 * 10.);
        assert!(shapes.len() > 1);
        for (shape, _) in &shapes {
            match shape {
                Shape::Rect(a, b) => {
                    assert!(a.iter().chain(b).all(|v| v.is_finite()));
                    assert!(a[0] <= b[0] + f64::EPSILON, "profile rows extend rightward from the range");
                }
                Shape::Segment(a, b) => assert!(a.iter().chain(b).all(|v| v.is_finite())),
                other => panic!("unexpected profile shape {other:?}"),
            }
        }
        let vwap = Drawing::new(Tool::AnchoredVwap, Anchor { ts: 86400, price: 10. }, Anchor { ts: 86400, price: 10. });
        let g = geometry_ctx(&vwap, String::new());
        let shapes = series_geometry(&g, &bars, &|i| i as f64 * 10.);
        assert!(!shapes.is_empty());
        assert!(shapes.iter().all(|(s, _)| match s {
            Shape::Segment(a, b) => a.iter().chain(b).all(|v| v.is_finite()),
            _ => false,
        }));
    }
}
