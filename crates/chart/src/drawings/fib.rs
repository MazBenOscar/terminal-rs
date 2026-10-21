//! Fibonacci retracements, extensions, channels and time zones.

pub const TIME_LEVELS: [u32; 11] = [0, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89];

pub fn time_zone_positions(start: f64, end: f64) -> Vec<(u32, f64)> {
    TIME_LEVELS.into_iter().map(|level| (level, start + f64::from(level) * (end - start))).collect()
}

pub fn channel_levels(a: super::Point, b: super::Point, c: super::Point) -> Vec<(f64, super::Point, super::Point)> {
    let polygon = super::channel_polygon(a, b, c);
    let offset = [polygon[3][0] - a[0], polygon[3][1] - a[1]];
    FIB_LEVELS
        .into_iter()
        .map(|level| {
            (
                level,
                [a[0] + level * offset[0], a[1] + level * offset[1]],
                [b[0] + level * offset[0], b[1] + level * offset[1]],
            )
        })
        .collect()
}

use super::{Geometry, LabeledShape};
use crate::drawings::{Drawing, FIB_LEVELS, Shape, Tool};

pub fn point_count(tool: Tool) -> usize {
    if matches!(tool, Tool::Fib | Tool::FibTimeZone) { 2 } else { 3 }
}

pub fn levels(d: &Drawing) -> Vec<(f64, f64)> {
    FIB_LEVELS.iter().map(|&r| (r, d.b.price + (d.a.price - d.b.price) * r)).collect()
}

pub fn extension_levels(d: &Drawing) -> Vec<(f64, f64)> {
    let c = d.c.unwrap_or(d.b);
    [0., 0.618, 1., 1.272, 1.618, 2., 2.618].into_iter().map(|r| (r, c.price + (d.b.price - d.a.price) * r)).collect()
}

pub fn handles(d: &Drawing) -> Vec<super::Anchor> {
    super::anchor_handles(d, point_count(d.tool))
}
pub fn hit_distance(g: &Geometry<'_>, p: super::Point) -> f64 {
    super::distance_to_shapes(geometry(g), p)
}

pub fn geometry(g: &Geometry<'_>) -> Vec<LabeledShape> {
    let d = g.drawing;
    let [a, b, c] = g.points;
    if d.tool == Tool::FibTimeZone {
        return time_zone_positions(a[0], b[0])
            .into_iter()
            .filter(|(_, x)| *x >= g.bounds[0] && *x <= g.bounds[2])
            .map(|(level, x)| (Shape::VLine(x), Some(level.to_string())))
            .collect();
    }
    if d.tool == Tool::FibChannel {
        return channel_levels(a, b, c)
            .into_iter()
            .filter_map(|(level, a, b)| {
                super::clip_line(a, b, g.bounds, false, false)
                    .map(|(a, b)| (Shape::Segment(a, b), Some(format!("{level:.3}"))))
            })
            .collect();
    }
    let (x0, x1) = if d.tool == Tool::FibExtension { (c[0], g.bounds[2]) } else { (a[0].min(b[0]), a[0].max(b[0])) };
    let mut shapes: Vec<_> = (if d.tool == Tool::Fib { levels(d) } else { extension_levels(d) })
        .into_iter()
        .map(|(r, price)| {
            let y = (g.price_y)(price);
            (Shape::Segment([x0, y], [x1, y]), Some(format!("{r:.3} ({price:.2})")))
        })
        .collect();
    if d.tool == Tool::FibExtension {
        shapes.extend([(Shape::Segment(a, b), None), (Shape::Segment(b, c), None)]);
    }
    shapes
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_time_zone_offsets() {
        assert_eq!(
            time_zone_positions(10., 12.).iter().map(|(_, x)| *x).collect::<Vec<_>>(),
            [10., 12., 14., 16., 20., 26., 36., 52., 78., 120., 188.]
        );
        assert_eq!(time_zone_positions(10., 8.)[10], (89, -168.));
    }
    #[test]
    fn exact_channel_level_positions() {
        let levels = channel_levels([0., 0.], [10., 0.], [5., 100.]);
        for ((level, a, b), expected) in levels.iter().zip([0., 23.6, 38.2, 50., 61.8, 78.6, 100.]) {
            assert!((a[1] - expected).abs() < 1e-12, "{level}");
            assert_eq!(a[0], 0.);
            assert_eq!(b[0], 10.);
            assert_eq!(a[1], b[1]);
        }
        assert_eq!(levels[3], (0.5, [0., 50.], [10., 50.]));
    }
}
