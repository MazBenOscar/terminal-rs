//! Shapes and sampled strokes. Vertices remain in timestamp/price coordinates.
use super::{Anchor, Geometry, LabeledShape, Point, Shape, Tool};

pub const MAX_POINTS: usize = 2_000;
pub fn point_count(_tool: Tool) -> usize {
    2
}
pub fn handles(d: &super::Drawing) -> Vec<Anchor> {
    if matches!(d.tool, Tool::Path | Tool::Brush) && !d.vertices.is_empty() {
        d.vertices.clone()
    } else {
        super::anchor_handles(d, 2)
    }
}
pub fn hit_distance(g: &Geometry<'_>, p: Point) -> f64 {
    super::distance_to_shapes(geometry(g), p)
}
pub fn geometry(g: &Geometry<'_>) -> Vec<LabeledShape> {
    let [a, b, _] = g.points;
    match g.drawing.tool {
        Tool::Circle => {
            let radius = (b[0] - a[0]).hypot(b[1] - a[1]);
            let point = |i: usize| {
                let t = i as f64 * std::f64::consts::TAU / 96.;
                [a[0] + radius * t.cos(), a[1] + radius * t.sin()]
            };
            (0..96).map(|i| (Shape::Segment(point(i), point(i + 1)), None)).collect()
        }
        Tool::Path | Tool::Brush => {
            let points: Vec<_> = if g.drawing.vertices.is_empty() {
                vec![a, b]
            } else {
                g.drawing.vertices.iter().copied().map(g.project).collect()
            };
            let mut shapes: Vec<_> = points.windows(2).map(|p| (Shape::Segment(p[0], p[1]), None)).collect();
            if g.drawing.arrow_end
                && let [.., a, b] = points.as_slice()
            {
                shapes.extend(super::lines::arrowhead(*a, *b));
            }
            shapes
        }
        _ => vec![(Shape::Rect(a, b), None)],
    }
}

pub fn deserialize_vertices<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Vec<Anchor>, D::Error> {
    use serde::Deserialize;
    let mut points = Vec::<Anchor>::deserialize(de)?;
    points.retain(|p| p.price.is_finite());
    points.truncate(MAX_POINTS);
    Ok(points)
}

/// Iterative Ramer–Douglas–Peucker in the chart's projected pixel metric.
/// If the simplified stroke exceeds the storage cap, resample it preserving both endpoints.
pub fn simplify(points: &[Anchor], project: &dyn Fn(Anchor) -> Point) -> Vec<Anchor> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let screen: Vec<_> = points.iter().copied().map(project).collect();
    let mut keep = vec![false; points.len()];
    if let Some(first) = keep.first_mut() {
        *first = true;
    }
    if let Some(last) = keep.last_mut() {
        *last = true;
    }
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((start, end)) = stack.pop() {
        let (Some(&a), Some(&b)) = (screen.get(start), screen.get(end)) else {
            continue;
        };
        let segment = Shape::Segment(a, b);
        let farthest = (start + 1..end)
            .filter_map(|i| screen.get(i).map(|&point| (i, super::distance(&segment, point))))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, d)) = farthest
            && d > 0.5
            && let Some(point) = keep.get_mut(i)
        {
            *point = true;
            stack.push((start, i));
            stack.push((i, end));
        }
    }
    let simplified: Vec<_> = points.iter().copied().zip(keep).filter_map(|(p, keep)| keep.then_some(p)).collect();
    if simplified.len() > MAX_POINTS {
        (0..MAX_POINTS).filter_map(|i| simplified.get(i * (simplified.len() - 1) / (MAX_POINTS - 1)).copied()).collect()
    } else {
        simplified
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn circle_radius_uses_both_screen_coordinates() {
        let a = Anchor { ts: 0, price: 100. };
        let drawing = super::super::Drawing::new(Tool::Circle, a, a);
        let g = Geometry {
            drawing: &drawing,
            points: [[20., 30.], [23., 34.], [0., 0.]],
            bounds: [0., 0., 100., 100.],
            price_y: &|p| p,
            measure_label: String::new(),
            text_size: [0., 0.],
            project: &|a| [a.ts as f64, a.price],
            pattern_points: Vec::new(),
            closes: &[],
        };
        for (shape, _) in geometry(&g) {
            let Shape::Segment(a, b) = shape else {
                panic!("Expected circle segment");
            };
            for p in [a, b] {
                assert!(((p[0] - 20.).hypot(p[1] - 30.) - 5.).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn rdp_tolerance_is_in_projected_pixels() {
        let points = [Anchor { ts: 0, price: 0. }, Anchor { ts: 1, price: 0.4 }, Anchor { ts: 2, price: 0. }];
        assert_eq!(simplify(&points, &|a| [a.ts as f64, a.price]).len(), 2);
        assert_eq!(simplify(&points, &|a| [a.ts as f64, a.price * 2.]).len(), 3);
    }

    #[test]
    fn rdp_simplification_and_cap() {
        let project = |a: Anchor| [a.ts as f64, a.price];
        let line: Vec<_> = (0..100).map(|ts| Anchor { ts, price: ts as f64 }).collect();
        assert_eq!(simplify(&line, &project), vec![line[0], line[99]]);
        let bend = [Anchor { ts: 0, price: 0. }, Anchor { ts: 1, price: 2. }, Anchor { ts: 2, price: 0. }];
        assert_eq!(simplify(&bend, &project), bend);
        let noisy: Vec<_> = (0..10_000).map(|ts| Anchor { ts, price: (ts % 2) as f64 * 10. }).collect();
        let result = simplify(&noisy, &project);
        assert!(result.len() <= MAX_POINTS);
        assert_eq!(result.first(), noisy.first());
        assert_eq!(result.last(), noisy.last());
    }

    #[test]
    fn simplify_collapses_duplicate_runs_and_never_drops_endpoints_at_the_cap() {
        let project = |a: Anchor| [a.ts as f64, a.price];
        let flat: Vec<_> = (0..50).map(|_| Anchor { ts: 0, price: 0. }).collect();
        assert_eq!(simplify(&flat, &project), vec![flat[0], flat[49]]);
        let twins = [Anchor { ts: 0, price: 1. }, Anchor { ts: 0, price: 1. }, Anchor { ts: 0, price: 1. }];
        assert_eq!(simplify(&twins, &project).len(), 2);
        // Exactly at the cap: returned unchanged and still size-safe.
        let exact: Vec<_> = (0..MAX_POINTS).map(|ts| Anchor { ts: ts as i64, price: ts as f64 }).collect();
        assert_eq!(simplify(&exact, &project), vec![exact[0], exact[MAX_POINTS - 1]]);
        // One past the cap with a real zigzag so RDP cannot collapse it.
        let zig: Vec<_> =
            (0..MAX_POINTS + 1).map(|ts| Anchor { ts: ts as i64, price: (ts % 2) as f64 * 20. }).collect();
        let capped = simplify(&zig, &project);
        assert!(capped.len() <= MAX_POINTS);
        assert_eq!(capped.first(), zig.first());
        assert_eq!(capped.last(), zig.last());
    }

    #[test]
    fn circle_zero_radius_and_path_strokes_with_few_vertices_stay_selectable() {
        fn ctx<'a>(drawing: &'a super::super::Drawing) -> Geometry<'a> {
            Geometry {
                drawing,
                points: [[20., 30.], [20., 30.], [0., 0.]],
                bounds: [0., 0., 100., 100.],
                price_y: &|p| p,
                measure_label: String::new(),
                text_size: [0., 0.],
                project: &|a| [a.ts as f64, a.price],
                pattern_points: Vec::new(),
                closes: &[],
            }
        }
        let a = Anchor { ts: 0, price: 100. };
        let circle = super::super::Drawing::new(Tool::Circle, a, a);
        let g = ctx(&circle);
        assert_eq!(geometry(&g).len(), 96);
        assert!(hit_distance(&g, [20., 30.]).abs() < 1e-9, "zero-radius circle hits at its centre");
        assert!(hit_distance(&g, [50., 50.]).is_finite());

        let mut path = super::super::Drawing::new(Tool::Path, a, a);
        path.vertices.clear();
        assert_eq!(geometry(&ctx(&path)).len(), 1, "empty vertices fall back to the anchors");
        path.vertices = vec![a];
        // One vertex cannot form a stroke (windows(2) is empty); the state is rejected on save.
        assert_eq!(geometry(&ctx(&path)).len(), 0);
        path.vertices = vec![a, Anchor { ts: 1, price: 110. }];
        path.arrow_end = true;
        let g = ctx(&path);
        let shapes = geometry(&g);
        assert!(shapes.len() >= 3, "two-vertex stroke plus arrowhead");
        assert!(hit_distance(&g, [0.5, 105.]).is_finite());
    }
}
