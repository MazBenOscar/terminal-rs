//! Fibonacci retracements and extensions.
use super::{Geometry, LabeledShape};
use crate::drawings::{Drawing, FIB_LEVELS, Shape, Tool};

pub fn point_count(tool: Tool) -> usize {
    if tool == Tool::Fib { 2 } else { 3 }
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
