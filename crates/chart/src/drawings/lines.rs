//! Lines, channels and pitchforks.
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

/// Original open arrowhead with a stable ten-pixel length, shortened on tiny arrows.
pub fn arrowhead(a: super::Point, b: super::Point) -> Vec<LabeledShape> {
    let delta = [b[0] - a[0], b[1] - a[1]];
    let length = delta[0].hypot(delta[1]);
    if length < 1e-8 {
        return vec![];
    }
    let u = [delta[0] / length, delta[1] / length];
    let size = length.min(10.);
    [-1., 1.]
        .into_iter()
        .map(|side| {
            (
                Shape::Segment(
                    b,
                    [b[0] - size * u[0] - side * size * 0.45 * u[1], b[1] - size * u[1] + side * size * 0.45 * u[0]],
                ),
                None,
            )
        })
        .collect()
}

/// The supplied point is the projected bar low (up) or high (down).
pub fn marker(a: super::Point, up: bool) -> [super::Point; 4] {
    let direction = if up { 1. } else { -1. };
    let tip = [a[0], a[1] + direction * 5.];
    [tip, [a[0] - 5., a[1] + direction * 14.], [a[0] + 5., a[1] + direction * 14.], tip]
}

pub fn point_count(tool: Tool) -> usize {
    match tool {
        Tool::Horizontal | Tool::Vertical | Tool::ArrowMarkUp | Tool::ArrowMarkDown => 1,
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
        Tool::Arrow => {
            let mut shapes = vec![(Shape::Segment(a, b), None)];
            shapes.extend(arrowhead(a, b));
            shapes
        }
        Tool::ArrowMarkUp | Tool::ArrowMarkDown => {
            vec![(Shape::Polygon(marker(a, d.tool == Tool::ArrowMarkUp)), None)]
        }
        Tool::Trendline | Tool::Ray => {
            if d.tool == Tool::Trendline && a[0] > b[0] {
                std::mem::swap(&mut a, &mut b);
            }
            clip_line(a, b, g.bounds, d.tool == Tool::Trendline && d.extend_left, d.tool == Tool::Ray || d.extend_right)
                .map(|(a, b)| vec![(Shape::Segment(a, b), None)])
                .unwrap_or_default()
        }
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
mod annotation_tests {
    use super::*;
    #[test]
    fn arrowhead_geometry() {
        let head = arrowhead([0., 0.], [20., 0.]);
        assert_eq!(head.len(), 2);
        assert_eq!(head[0].0, Shape::Segment([20., 0.], [10., -4.5]));
        assert_eq!(head[1].0, Shape::Segment([20., 0.], [10., 4.5]));
        assert!(arrowhead([1., 1.], [1., 1.]).is_empty());
        assert_eq!(arrowhead([0., 0.], [0., 20.])[0].0, Shape::Segment([0., 20.], [4.5, 10.]));
    }
    #[test]
    fn markers_are_below_low_and_above_high() {
        assert!(marker([30., 80.], true).iter().all(|p| p[1] > 80.));
        assert!(marker([30., 20.], false).iter().all(|p| p[1] < 20.));
    }
}
