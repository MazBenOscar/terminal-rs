//! Measurements and long/short risk-reward tools.
use super::{Geometry, LabeledShape};
use crate::drawings::{Drawing, Shape, Tool};

pub fn label(a: super::Anchor, b: super::Anchor, bar_intervals: usize, decimals: usize) -> String {
    let change = b.price - a.price;
    let pct = if a.price == 0. { "n/a".into() } else { format!("{:+.2}%", change / a.price.abs() * 100.) };
    let secs = b.ts.abs_diff(a.ts);
    let elapsed = if secs >= 86400 {
        format!("{}d {}h", secs / 86400, secs % 86400 / 3600)
    } else if secs >= 3600 {
        format!("{}h {}m", secs / 3600, secs % 3600 / 60)
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    };
    format!("{change:+.decimals$} ({pct}) · {bar_intervals} bars · {elapsed}")
}

pub fn point_count(tool: Tool) -> usize {
    if tool == Tool::Measure { 2 } else { 3 }
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
    if g.drawing.tool == Tool::Measure {
        return vec![(Shape::Rect(a, b), Some(g.measure_label.clone()))];
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
