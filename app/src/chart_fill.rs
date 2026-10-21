//! Bounded screen-space band meshes. Invalid intervals never bridge a data gap.
use egui::{Color32, Mesh, Pos2, Rect, Ui, pos2};
use std::sync::Arc;
use tr_chart::viewport::{PriceScale, Viewport};
use tr_core::indicators::registry::Output;
use tr_core::indicators::{formula::display_values, study::Settings};

pub type OutputLookup<'a> = std::collections::HashMap<u32, (&'a [Output], i16)>;

pub struct Fill {
    points: Arc<Vec<[f64; 2]>>,
    invalid: Vec<usize>,
    color: Color32,
}
impl Fill {
    pub fn build(settings: &Settings, outputs: &[Output], lookup: &OutputLookup<'_>) -> Option<Self> {
        let fill = settings.fill.as_ref()?;
        let values = |edge: &tr_core::indicators::study::FillEdge| {
            let (source, displacement) = match edge.study {
                Some(id) => *lookup.get(&id)?,
                None => (outputs, settings.displacement),
            };
            let output = source.iter().find(|o| o.name == edge.output)?;
            Some(display_values(&output.values, displacement))
        };
        let points: Vec<_> = values(&fill.upper)?.into_iter().zip(values(&fill.lower)?).map(|(a, b)| [a, b]).collect();
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
    fn ema_bollinger_fill_matches_per_bar_values_and_splits_nan_gaps() {
        use tr_core::{
            Bar, Timeframe,
            indicators::{
                registry::{Input, Spec},
                study::{self, BandFill, FillEdge},
            },
        };
        let bars: Vec<_> = (0..40).map(|i| Bar::new(i, i as f64, i as f64 + 2., i as f64 - 1., i as f64, 1.)).collect();
        let mut specs = vec![Spec::new("EMA", &[2.]).unwrap(), Spec::new("BB", &[3., 2.]).unwrap()];
        study::assign_ids(&mut specs);
        let settings = specs[0].chart.as_mut().unwrap();
        settings.fill = Some(BandFill {
            upper: "EMA".into(),
            lower: FillEdge { study: Some(2), output: "Lower".into() },
            color: [1, 2, 3],
            opacity: 35,
        });
        let mut outputs = study::evaluate(&specs, &Input::new(&bars, Timeframe::D1));
        let lower = outputs[1].as_mut().unwrap().iter_mut().find(|o| o.name == "Lower").unwrap();
        lower.values[20] = f64::NAN;
        let upper = outputs[0].as_ref().unwrap();
        let lower = outputs[1].as_ref().unwrap();
        let lookup = OutputLookup::from([(2, (lower.as_slice(), 0))]);
        let fill = Fill::build(specs[0].chart.as_ref().unwrap(), upper, &lookup).unwrap();
        let lower = &lower.iter().find(|o| o.name == "Lower").unwrap().values;
        for (i, point) in fill.points.iter().enumerate() {
            for (a, b) in point.iter().zip([upper[0].values[i], lower[i]]) {
                assert!(*a == b || a.is_nan() && b.is_nan());
            }
        }
        assert!(!fill.valid(19, 20));
        assert!(!fill.valid(20, 21));
        assert!(!fill.valid(19, 21));
        assert!(fill.valid(21, 22));
        assert!(Fill::build(specs[0].chart.as_ref().unwrap(), upper, &OutputLookup::new()).is_none());
    }

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
