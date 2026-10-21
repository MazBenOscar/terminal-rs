//! Text note geometry.
use super::{Geometry, LabeledShape};
use crate::drawings::Shape;
pub fn point_count() -> usize {
    1
}
pub fn handles(d: &super::Drawing) -> Vec<super::Anchor> {
    super::anchor_handles(d, point_count())
}
pub fn hit_distance(g: &Geometry<'_>, p: super::Point) -> f64 {
    super::distance_to_shapes(geometry(g), p)
}

pub fn geometry(g: &Geometry<'_>) -> Vec<LabeledShape> {
    let a = g.points[0];
    vec![(Shape::Rect(a, [a[0] + g.text_size[0], a[1] + g.text_size[1]]), Some(g.drawing.text.clone()))]
}
