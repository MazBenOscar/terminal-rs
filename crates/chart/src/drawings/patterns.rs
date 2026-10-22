//! Ordered, labelled multi-point patterns share placement and geometry.
use super::{Anchor, Drawing, LabeledShape, Point, Shape, Tool};

pub fn point_count(tool: Tool) -> usize {
    match tool {
        Tool::Xabcd => 5,
        Tool::Abcd | Tool::TrianglePattern | Tool::ElliottCorrection => 4,
        Tool::HeadAndShoulders => 7,
        Tool::ElliottImpulse => 6,
        _ => 0,
    }
}

pub fn handles(d: &Drawing) -> Vec<Anchor> {
    d.pattern_points.clone()
}

#[derive(Debug, Clone, Default)]
pub struct Placement {
    pub points: Vec<Anchor>,
}
impl Placement {
    pub fn backspace(&mut self) {
        self.points.pop();
    }
    pub fn cancel(&mut self) {
        self.points.clear();
    }
    pub fn click(&mut self, tool: Tool, anchor: Anchor) -> Option<Drawing> {
        self.points.push(anchor);
        if self.points.len() != point_count(tool) {
            return None;
        }
        let [first, second, ..] = self.points.as_slice() else {
            return None;
        };
        let mut d = Drawing::new(tool, *first, *second);
        d.pattern_points = std::mem::take(&mut self.points);
        Some(d)
    }
}

/// Absolute price-leg ratios; zero-length denominators have no ratio.
pub fn ratios(tool: Tool, p: &[Anchor]) -> Vec<(&'static str, Option<f64>)> {
    let specs: &[(&str, usize, usize, usize, usize)] = match tool {
        Tool::Xabcd => &[("AB/XA", 1, 2, 0, 1), ("BC/AB", 2, 3, 1, 2), ("CD/BC", 3, 4, 2, 3), ("AD/XA", 1, 4, 0, 1)],
        Tool::Abcd => &[("BC/AB", 1, 2, 0, 1), ("CD/BC", 2, 3, 1, 2)],
        _ => &[],
    };
    specs
        .iter()
        .filter_map(|&(name, a, b, c, d)| {
            let numerator = (p.get(a)?.price - p.get(b)?.price).abs();
            let denominator = (p.get(c)?.price - p.get(d)?.price).abs();
            let ratio = numerator / denominator;
            Some((name, (denominator > 0. && ratio.is_finite()).then_some(ratio)))
        })
        .collect()
}

/// Screen y grows downward. Extrema use both neighbours; monotone/tied
/// points use their neighbour mean (ties go above) for a stable fallback.
pub fn label_above(points: &[Point], index: usize) -> bool {
    let Some(point) = points.get(index) else {
        return true;
    };
    let y = point[1];
    let neighbours: Vec<_> = [index.checked_sub(1), index.checked_add(1)]
        .into_iter()
        .flatten()
        .filter_map(|i| points.get(i).map(|point| point[1]))
        .collect();
    neighbours.is_empty() || y <= neighbours.iter().sum::<f64>() / neighbours.len() as f64
}

pub fn neckline(a: Point, b: Point, right: f64) -> Option<(Point, Point)> {
    let dx = b[0] - a[0];
    if dx.abs() < 1e-9 {
        return None;
    }
    let end = [right, a[1] + (right - a[0]) * (b[1] - a[1]) / dx];
    end.iter().all(|v| v.is_finite()).then_some((a, end))
}

fn intersection(a: Point, b: Point, c: Point, d: Point) -> Option<Point> {
    let u = [b[0] - a[0], b[1] - a[1]];
    let v = [d[0] - c[0], d[1] - c[1]];
    let cross = u[0] * v[1] - u[1] * v[0];
    if cross.abs() < 1e-9 {
        return None;
    }
    let t = ((c[0] - a[0]) * v[1] - (c[1] - a[1]) * v[0]) / cross;
    let result = [a[0] + t * u[0], a[1] + t * u[1]];
    result.iter().all(|v| v.is_finite()).then_some(result)
}

pub fn hit_distance(context: &super::Geometry<'_>, point: Point) -> f64 {
    super::distance_to_shapes(geometry(context.drawing, &context.pattern_points, context.bounds), point)
}

pub fn geometry(d: &Drawing, points: &[Point], bounds: [f64; 4]) -> Vec<LabeledShape> {
    let mut result: Vec<_> = points.windows(2).map(|p| (Shape::Segment(p[0], p[1]), None)).collect();
    let labels: &[&str] = match d.tool {
        Tool::Xabcd => &["X", "A", "B", "C", "D"],
        Tool::Abcd | Tool::TrianglePattern => &["A", "B", "C", "D"],
        Tool::HeadAndShoulders => &["", "LS", "", "H", "", "RS", ""],
        Tool::ElliottImpulse => &["", "1", "2", "3", "4", "5"],
        Tool::ElliottCorrection => &["0", "A", "B", "C"],
        _ => &[],
    };
    for (i, &point) in points.iter().enumerate() {
        if let Some(&label) = labels.get(i).filter(|s| !s.is_empty()) {
            let y = point[1] + if label_above(points, i) { -14. } else { 14. };
            result.push((Shape::Segment([point[0], y], [point[0], y]), Some(label.into())));
        }
    }
    if d.tool == Tool::Xabcd {
        for (a, b) in [(0, 2), (1, 3), (2, 4), (0, 4)] {
            if let (Some(&a), Some(&b)) = (points.get(a), points.get(b)) {
                result.push((Shape::DashedSegment(a, b), None));
            }
        }
    }
    if d.tool == Tool::HeadAndShoulders
        && let [_, _, a, _, b, ..] = points
    {
        let (a, b) = neckline(*a, *b, bounds[2]).unwrap_or((*a, *b));
        result.push((Shape::Segment(a, b), None));
    }
    if d.tool == Tool::TrianglePattern
        && let [a, _, c, ..] = points
    {
        result.push((Shape::Segment(*a, *c), None));
        if let [a, b, c, d] = points {
            result.push((Shape::Segment(*b, *d), None));
            if let Some(end) = intersection(*a, *c, *b, *d) {
                for start in [*a, *b] {
                    result.push((Shape::Segment(start, end), None));
                }
            }
        }
    }
    if let Some(p) = points.first() {
        for (i, (name, ratio)) in ratios(d.tool, &d.pattern_points).into_iter().enumerate() {
            let at = [p[0], bounds[1] + 14. + i as f64 * 14.];
            let label = ratio.map_or_else(|| format!("{name} —"), |r| format!("{name} {r:.3}"));
            result.push((Shape::Segment(at, at), Some(label)));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn anchors(prices: &[f64]) -> Vec<Anchor> {
        prices.iter().enumerate().map(|(i, &price)| Anchor { ts: i as i64, price }).collect()
    }
    #[test]
    fn pattern_dispatch_geometry_hits_and_all_handles() {
        for tool in Tool::ALL.into_iter().filter(|t| point_count(*t) > 0) {
            let mut placement = Placement::default();
            let mut drawing = None;
            for i in 0..tool.points() {
                drawing = placement.click(tool, Anchor { ts: i as i64, price: if i % 2 == 0 { 100. } else { 120. } });
            }
            let d = drawing.unwrap();
            let points: Vec<_> = d.pattern_points.iter().map(|a| [a.ts as f64 * 20., a.price]).collect();
            let context = super::super::Geometry {
                drawing: &d,
                points: [points[0], points[1], points[2]],
                pattern_points: points.clone(),
                bounds: [0., 0., 200., 200.],
                price_y: &|p| p,
                measure_label: String::new(),
                text_size: [0., 0.],
                closes: &[],
                project: &|a| [a.ts as f64 * 20., a.price],
            };
            assert_eq!(super::super::handles(&d).len(), tool.points());
            let shapes = super::super::geometry(&context);
            assert!(shapes.iter().any(|(s, _)| *s == Shape::Segment(points[0], points[1])));
            assert_eq!(super::super::hit_distance(&context, [10., 110.]), 0.);
            if tool == Tool::Xabcd {
                assert_eq!(shapes.iter().filter(|(s, _)| matches!(s, Shape::DashedSegment(..))).count(), 4);
                assert!(shapes.iter().any(|(_, label)| label.as_deref() == Some("AB/XA 1.000")));
            }
        }
    }
    #[test]
    fn triangle_trendlines_meet_and_parallel_lines_are_safe() {
        let mut d = Drawing::new(Tool::TrianglePattern, Anchor { ts: 0, price: 1. }, Anchor { ts: 1, price: 2. });
        d.pattern_points = anchors(&[0., 10., 2., 8.]);
        let p = [[0., 0.], [0., 10.], [2., 2.], [2., 8.]];
        let shapes = geometry(&d, &p, [0., 0., 100., 100.]);
        assert!(shapes.contains(&(Shape::Segment(p[0], [5., 5.]), None)));
        assert!(shapes.contains(&(Shape::Segment(p[1], [5., 5.]), None)));
        assert!(intersection([0., 0.], [1., 1.], [0., 2.], [1., 3.]).is_none());
    }
    #[test]
    fn ratio_math_exact() {
        let p = anchors(&[100., 120., 110., 115., 105.]);
        assert_eq!(
            ratios(Tool::Xabcd, &p),
            vec![("AB/XA", Some(0.5)), ("BC/AB", Some(0.5)), ("CD/BC", Some(2.)), ("AD/XA", Some(0.75))]
        );
        assert_eq!(ratios(Tool::Abcd, &p[..4]), vec![("BC/AB", Some(0.5)), ("CD/BC", Some(0.5))]);
        assert_eq!(ratios(Tool::Abcd, &anchors(&[1., 1., 2., 3.]))[0].1, None);
    }
    #[test]
    fn point_counts_and_backspace_escape_placement() {
        for (tool, count) in [
            (Tool::Xabcd, 5),
            (Tool::Abcd, 4),
            (Tool::HeadAndShoulders, 7),
            (Tool::TrianglePattern, 4),
            (Tool::ElliottImpulse, 6),
            (Tool::ElliottCorrection, 4),
        ] {
            assert_eq!(tool.points(), count);
            let mut placement = Placement::default();
            let a = Anchor { ts: 0, price: 1. };
            placement.backspace();
            assert!(placement.click(tool, a).is_none());
            placement.backspace();
            assert!(placement.points.is_empty());
            placement.click(tool, a);
            placement.cancel();
            assert!(placement.points.is_empty());
            for _ in 1..count {
                assert!(placement.click(tool, a).is_none());
            }
            let d = placement.click(tool, a).unwrap();
            assert_eq!(handles(&d).len(), count);
            assert!(placement.points.is_empty());
        }
    }
    #[test]
    fn label_placement_high_low_rule() {
        let p = [[0., 5.], [1., 1.], [2., 10.], [3., 2.]];
        assert!(label_above(&p, 1));
        assert!(!label_above(&p, 2));
        assert!(!label_above(&p, 0));
        assert!(label_above(&p, 3));
    }
    #[test]
    fn head_shoulders_neckline_extends_to_right_edge() {
        assert_eq!(neckline([10., 20.], [30., 30.], 100.), Some(([10., 20.], [100., 65.])));
        assert!(neckline([10., 20.], [10., 30.], 100.).is_none());
    }

    #[test]
    fn ratio_division_by_zero_and_non_finite_prices_yield_none() {
        let zero_leg = anchors(&[100., 100., 120., 130., 140.]);
        let r = ratios(Tool::Xabcd, &zero_leg);
        assert_eq!(r[0].1, None, "AB/XA with zero-length XA");
        assert_eq!(r[3].1, None, "AD/XA with zero-length XA");
        let nan = anchors(&[100., f64::NAN, 120., 130.]);
        assert!(ratios(Tool::Abcd, &nan).iter().all(|(_, r)| r.is_none()));
        // Short point lists drop incomplete legs instead of panicking.
        assert!(ratios(Tool::Xabcd, &anchors(&[1., 2.])).len() < 4);
    }

    #[test]
    fn label_above_ties_and_extrema_use_neighbour_mean() {
        // Screen y grows downward: lower y is a visual high.
        let tied = [[0., 5.], [1., 5.], [2., 5.]];
        assert!(label_above(&tied, 0), "endpoint tie goes above");
        assert!(label_above(&tied, 1), "interior tie goes above");
        assert!(label_above(&tied, 2));
        let equal_peak = [[0., 8.], [1., 2.], [2., 2.]];
        assert!(label_above(&equal_peak, 1), "shared high labels above");
        assert!(!label_above(&equal_peak, 0), "higher neighbour sits below");
        // Single-point and out-of-range indices default to above.
        assert!(label_above(&[[0., 1.]], 0));
        assert!(label_above(&[[0., 1.]], 99));
    }

    #[test]
    fn triangle_parallel_and_far_apex_geometry_stays_finite() {
        let mut d = Drawing::new(Tool::TrianglePattern, Anchor { ts: 0, price: 1. }, Anchor { ts: 1, price: 2. });
        // Nearly-parallel sides meet far to the right of the viewport.
        let p = [[0., 0.], [50., 0.1], [0., 10.], [50., 9.9]];
        d.pattern_points = anchors(&[0., 0.1, 10., 9.9]);
        let shapes = geometry(&d, &p, [0., 0., 100., 100.]);
        for (shape, label) in &shapes {
            match shape {
                Shape::Segment(a, b) | Shape::DashedSegment(a, b) => {
                    assert!(a.iter().chain(b.iter()).all(|v| v.is_finite()), "{label:?}");
                }
                _ => {}
            }
        }
        // Parallel sides never invent an apex (intersection is None).
        assert!(intersection([0., 0.], [10., 0.], [0., 10.], [10., 10.]).is_none());
        let parallel = [[0., 0.], [10., 0.], [0., 10.], [10., 10.]];
        let shapes = geometry(&d, &parallel, [0., 0., 100., 100.]);
        // 4 sides/labels + a–c and b–d only; no third pair of apex spokes.
        let stroke_count =
            shapes.iter().filter(|(s, label)| label.is_none() && matches!(s, Shape::Segment(a, b) if a != b)).count();
        assert_eq!(stroke_count, 5, "windows(3) + a–c + b–d");
    }

    #[test]
    fn placement_backspace_escape_mid_pattern_are_transactional() {
        let mut placement = Placement::default();
        for _ in 0..3 {
            assert!(placement.click(Tool::Xabcd, Anchor { ts: 1, price: 1. }).is_none());
        }
        assert_eq!(placement.points.len(), 3);
        placement.backspace();
        assert_eq!(placement.points.len(), 2);
        placement.cancel();
        assert!(placement.points.is_empty());
        // Completing still yields a full handle set after a mid-placement undo.
        for i in 0..3 {
            assert!(placement.click(Tool::Abcd, Anchor { ts: i, price: 1. }).is_none());
        }
        let d = placement.click(Tool::Abcd, Anchor { ts: 3, price: 1. }).unwrap();
        assert_eq!(handles(&d).len(), 4);
        assert!(placement.points.is_empty());
    }
}
