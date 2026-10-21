//! Lines, channels and pitchforks.

#[derive(Debug, PartialEq)]
pub struct RegressionFit {
    pub intercept: f64,
    pub slope: f64,
    pub residual_sd: f64,
    pub r_squared: f64,
}

/// Population residual deviation; x is the zero-based chronological bar index.
pub fn regression_fit(closes: &[f64]) -> Option<RegressionFit> {
    if closes.is_empty() || closes.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let n = closes.len() as f64;
    let mx = (n - 1.) / 2.;
    let my = closes.iter().sum::<f64>() / n;
    let xx = (0..closes.len()).map(|i| (i as f64 - mx).powi(2)).sum::<f64>();
    let xy = closes.iter().enumerate().map(|(i, y)| (i as f64 - mx) * (y - my)).sum::<f64>();
    let slope = if xx == 0. { 0. } else { xy / xx };
    let intercept = my - slope * mx;
    let residual = closes.iter().enumerate().map(|(i, y)| (y - intercept - slope * i as f64).powi(2)).sum::<f64>();
    let total = closes.iter().map(|y| (y - my).powi(2)).sum::<f64>();
    Some(RegressionFit {
        intercept,
        slope,
        residual_sd: (residual / n).sqrt(),
        r_squared: if total == 0. { 1. } else { (1. - residual / total).clamp(0., 1.) },
    })
}

fn regression_geometry(g: &Geometry<'_>) -> Vec<LabeledShape> {
    let Some(fit) = regression_fit(g.closes) else {
        return vec![];
    };
    let [a, b, _] = g.points;
    let (x0, x1) = (a[0].min(b[0]), a[0].max(b[0]));
    let end = fit.intercept + fit.slope * (g.closes.len() - 1) as f64;
    let mut shapes: Vec<_> = [-2., 0., 2.]
        .into_iter()
        .filter_map(|level| {
            let offset = level * fit.residual_sd;
            clip_line(
                [x0, (g.price_y)(fit.intercept + offset)],
                [x1, (g.price_y)(end + offset)],
                g.bounds,
                false,
                false,
            )
            .map(|(a, b)| (Shape::Segment(a, b), None))
        })
        .collect();
    let p = [x1, (g.price_y)(end)];
    shapes.push((Shape::Rect(p, p), Some(format!("Slope/bar {:.4} · r² {:.4}", fit.slope, fit.r_squared))));
    shapes
}

use super::{Geometry, LabeledShape};
use crate::drawings::{Shape, Tool};

/// Clip a segment, ray or infinite line to the viewport.
pub fn clip_line(
    a: super::Point,
    b: super::Point,
    bounds: [f64; 4],
    extend_start: bool,
    extend_end: bool,
) -> Option<(super::Point, super::Point)> {
    if a.iter().chain(b.iter()).chain(bounds.iter()).any(|x| !x.is_finite()) {
        return None;
    }
    let (mut lo, mut hi) =
        (if extend_start { f64::NEG_INFINITY } else { 0. }, if extend_end { f64::INFINITY } else { 1. });
    let delta = [b[0] - a[0], b[1] - a[1]];
    if delta[0].abs() < 1e-10 && delta[1].abs() < 1e-10 {
        return (a[0] >= bounds[0] && a[0] <= bounds[2] && a[1] >= bounds[1] && a[1] <= bounds[3]).then_some((a, a));
    }
    for axis in 0..2 {
        if delta[axis].abs() < 1e-10 {
            if a[axis] < bounds[axis] || a[axis] > bounds[axis + 2] {
                return None;
            }
        } else {
            let t1 = (bounds[axis] - a[axis]) / delta[axis];
            let t2 = (bounds[axis + 2] - a[axis]) / delta[axis];
            lo = lo.max(t1.min(t2));
            hi = hi.min(t1.max(t2));
            if lo > hi {
                return None;
            }
        }
    }
    Some(([a[0] + lo * delta[0], a[1] + lo * delta[1]], [a[0] + hi * delta[0], a[1] + hi * delta[1]]))
}

/// Project the channel width in displayed coordinates, including logarithmic charts.
pub fn channel_polygon(a: super::Point, b: super::Point, c: super::Point) -> [super::Point; 4] {
    let delta = [b[0] - a[0], b[1] - a[1]];
    let len2 = delta[0].powi(2) + delta[1].powi(2);
    let t = if len2 <= 1e-10 { 0. } else { ((c[0] - a[0]) * delta[0] + (c[1] - a[1]) * delta[1]) / len2 };
    let offset = [c[0] - a[0] - t * delta[0], c[1] - a[1] - t * delta[1]];
    [a, b, [b[0] + offset[0], b[1] + offset[1]], [a[0] + offset[0], a[1] + offset[1]]]
}

pub fn point_count(tool: Tool) -> usize {
    match tool {
        Tool::Horizontal | Tool::HorizontalRay | Tool::Vertical => 1,
        Tool::Channel | Tool::Pitchfork => 3,
        _ => 2,
    }
}

pub fn handles(d: &super::Drawing) -> Vec<super::Anchor> {
    super::anchor_handles(d, point_count(d.tool))
}
pub fn hit_distance(g: &Geometry<'_>, p: super::Point) -> f64 {
    super::distance_to_shapes(geometry(g), p)
}

pub fn geometry(g: &Geometry<'_>) -> Vec<LabeledShape> {
    let d = g.drawing;
    let [mut a, mut b, c] = g.points;
    match d.tool {
        Tool::Trendline | Tool::Ray => {
            if d.tool == Tool::Trendline && a[0] > b[0] {
                std::mem::swap(&mut a, &mut b);
            }
            clip_line(a, b, g.bounds, d.tool == Tool::Trendline && d.extend_left, d.tool == Tool::Ray || d.extend_right)
                .map(|(a, b)| vec![(Shape::Segment(a, b), None)])
                .unwrap_or_default()
        }
        Tool::ExtendedLine => {
            clip_line(a, b, g.bounds, true, true).map(|(a, b)| vec![(Shape::Segment(a, b), None)]).unwrap_or_default()
        }
        Tool::HorizontalRay => clip_line(a, [a[0] + 1., a[1]], g.bounds, false, true)
            .map(|(a, b)| vec![(Shape::Segment(a, b), None), (Shape::Segment(b, b), Some(format!("{:.2}", d.a.price)))])
            .unwrap_or_default(),
        Tool::InfoLine => vec![(Shape::Segment(a, b), None), (Shape::Rect(b, b), Some(g.measure_label.clone()))],
        Tool::RegressionTrend => regression_geometry(g),
        Tool::Horizontal => vec![(Shape::HLine(a[1]), Some(format!("{:.2}", d.a.price)))],
        Tool::Vertical => vec![(Shape::VLine(a[0]), None)],
        Tool::Channel => {
            vec![(if d.c.is_some() { Shape::Polygon(channel_polygon(a, b, c)) } else { Shape::Segment(a, b) }, None)]
        }
        Tool::Pitchfork => {
            let delta = [(b[0] + c[0]) / 2. - a[0], (b[1] + c[1]) / 2. - a[1]];
            [a, b, c]
                .into_iter()
                .filter_map(|start| {
                    clip_line(start, [start[0] + delta[0], start[1] + delta[1]], g.bounds, false, true)
                        .map(|(a, b)| (Shape::Segment(a, b), None))
                })
                .collect()
        }
        _ => unreachable!("Non-line tool dispatched to lines"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn regression_exact_fit_and_residual_bands() {
        assert_eq!(
            regression_fit(&[2., 4., 6.]),
            Some(RegressionFit { intercept: 2., slope: 2., residual_sd: 0., r_squared: 1. })
        );
        let fit = regression_fit(&[1., 0., 1., 2.]).unwrap();
        assert!((fit.intercept - 0.4).abs() < 1e-12);
        assert_eq!(fit.slope, 0.4);
        assert!((fit.residual_sd - 0.3_f64.sqrt()).abs() < 1e-12);
        assert!((fit.r_squared - 0.4).abs() < 1e-12);
        assert_eq!(regression_fit(&[]), None);
        assert_eq!(regression_fit(&[3., 3.]).unwrap().r_squared, 1.);
    }

    #[test]
    fn new_lines_clip_and_regression_recomputes_from_closes() {
        use crate::drawings::{Anchor, Drawing};
        let drawing = Drawing::new(Tool::HorizontalRay, Anchor { ts: 0, price: 20. }, Anchor { ts: 1, price: 40. });
        let mut g = Geometry {
            drawing: &drawing,
            points: [[20., 20.], [40., 40.], [40., 40.]],
            pattern_points: Vec::new(),
            bounds: [0., 0., 100., 100.],
            price_y: &|p| p,
            measure_label: String::new(),
            text_size: [0., 0.],
            closes: &[2., 4., 6.],
        };
        assert_eq!(geometry(&g)[0].0, Shape::Segment([20., 20.], [100., 20.]));
        assert_eq!(hit_distance(&g, [10., 20.]), 10.);
        let extended = Drawing { tool: Tool::ExtendedLine, ..drawing.clone() };
        g.drawing = &extended;
        assert_eq!(geometry(&g)[0].0, Shape::Segment([0., 0.], [100., 100.]));
        let regression = Drawing { tool: Tool::RegressionTrend, ..drawing.clone() };
        g.drawing = &regression;
        assert_eq!(geometry(&g)[1].0, Shape::Segment([20., 2.], [40., 6.]));
        g.closes = &[3., 6., 9.];
        assert_eq!(geometry(&g)[1].0, Shape::Segment([20., 3.], [40., 9.]));
    }
}
