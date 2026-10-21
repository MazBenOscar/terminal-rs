//! Bounded screen-space band meshes. Invalid intervals never bridge a data gap.
use egui::{Color32, Mesh, Pos2, Rect, Ui, pos2};
use std::sync::Arc;
use tr_chart::viewport::{PriceScale, Viewport};
use tr_core::indicators::registry::Output;
use tr_core::indicators::{formula::display_values, study::Settings};

pub struct Fill {
    points: Arc<Vec<[f64; 2]>>,
    invalid: Vec<usize>,
    color: Color32,
}
impl Fill {
    pub fn build(settings: &Settings, outputs: &[Output]) -> Option<Self> {
        let fill = settings.fill.as_ref()?;
        let upper = outputs.iter().find(|o| o.name == fill.upper)?;
        let lower = outputs.iter().find(|o| o.name == fill.lower)?;
        let points: Vec<_> = display_values(&upper.values, settings.displacement)
            .into_iter()
            .zip(display_values(&lower.values, settings.displacement))
            .map(|(a, b)| [a, b])
            .collect();
        let mut invalid = vec![0];
        for [a, b] in &points {
            invalid.push(invalid.last().copied().unwrap() + usize::from(!a.is_finite() || !b.is_finite()));
        }
        let [r, g, b] = fill.color;
        Some(Self { points: Arc::new(points), invalid, color: Color32::from_rgba_unmultiplied(r, g, b, fill.opacity) })
    }
    fn valid(&self, a: usize, b: usize) -> bool {
        self.invalid[b + 1] == self.invalid[a]
    }
    pub fn paint(&self, ui: &Ui, rect: Rect, view: &Viewport, scale: &PriceScale) {
        let (lo, hi) = view.visible_range(self.points.len());
        if hi <= lo + 1 {
            return;
        }
        let stride = (hi - lo).div_ceil(3000).max(1);
        let mut mesh = Mesh::default();
        for a in (lo..hi - 1).step_by(stride) {
            let b = (a + stride).min(hi - 1);
            if !self.valid(a, b) {
                continue;
            }
            let [u1, l1] = self.points[a];
            let [u2, l2] = self.points[b];
            if scale.log && [u1, l1, u2, l2].iter().any(|v| *v <= 0.) {
                continue;
            }
            let point = |i, price| pos2(rect.left() + view.x_center(i) as f32, rect.top() + scale.y_of(price) as f32);
            let base = mesh.vertices.len() as u32;
            for pos in [point(a, u1), point(b, u2), point(b, l2), point(a, l1)] {
                mesh.colored_vertex(Pos2::new(pos.x, pos.y.clamp(rect.top() - 1., rect.bottom() + 1.)), self.color);
            }
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base, base + 2, base + 3);
        }
        ui.painter_at(rect).add(egui::Shape::mesh(mesh));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gaps_are_not_bridged_when_zoomed_out() {
        let f = Fill {
            points: Arc::new(vec![[1., 0.], [f64::NAN, 0.], [2., 1.], [3., 1.]]),
            invalid: vec![0, 0, 1, 1, 1],
            color: Color32::WHITE,
        };
        assert!(!f.valid(0, 2));
        assert!(f.valid(2, 3));
    }
}
