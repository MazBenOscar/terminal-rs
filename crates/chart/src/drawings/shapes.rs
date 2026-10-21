//! Rectangle geometry.
use super::{Geometry, LabeledShape};
use crate::drawings::Shape;
pub fn point_count() -> usize {
    2
}
pub fn handles(d: &super::Drawing) -> Vec<super::Anchor> {
    super::anchor_handles(d, point_count())
}
pub fn hit_distance(g: &Geometry<'_>, p: super::Point) -> f64 {
    super::distance_to_shapes(geometry(g), p)
}

pub fn geometry(g: &Geometry<'_>) -> Vec<LabeledShape> {
    vec![(Shape::Rect(g.points[0], g.points[1]), None)]
}
