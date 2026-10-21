//! User drawings. Anchors are stored as (timestamp, price) so they survive pan, zoom and
//! timeframe changes; they are mapped to bar indices only when drawn.

// Shared screen primitives retained here for compatibility with the original tests.
#[cfg(test)]
use super::{Anchor, Drawing, Tool};

/// Screen-space geometry of a drawing, produced by the chart for hit testing.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Segment([f64; 2], [f64; 2]),
    HLine(f64),
    VLine(f64),
    Rect([f64; 2], [f64; 2]),
    Polygon([[f64; 2]; 4]),
}

/// Distance in pixels from point `p` to a shape.
pub fn distance(shape: &Shape, p: [f64; 2]) -> f64 {
    match shape {
        Shape::HLine(y) => (p[1] - y).abs(),
        Shape::VLine(x) => (p[0] - x).abs(),
        Shape::Rect(a, b) => {
            let dx = (a[0].min(b[0]) - p[0]).max(0.).max(p[0] - a[0].max(b[0]));
            let dy = (a[1].min(b[1]) - p[1]).max(0.).max(p[1] - a[1].max(b[1]));
            dx.hypot(dy)
        }
        Shape::Polygon(points) => {
            let mut positive = false;
            let mut negative = false;
            let mut area = 0.;
            let mut nearest = f64::INFINITY;
            for i in 0..4 {
                let (a, b) = (points[i], points[(i + 1) % 4]);
                let cross = (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
                positive |= cross > 1e-8;
                negative |= cross < -1e-8;
                area += a[0] * b[1] - b[0] * a[1];
                nearest = nearest.min(distance(&Shape::Segment(a, b), p));
            }
            if area.abs() > 1e-8 && !(positive && negative) { 0. } else { nearest }
        }
        Shape::Segment(a, b) => {
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len2 = dx * dx + dy * dy;
            let t = if len2 == 0.0 { 0.0 } else { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0) };
            let (cx, cy) = (a[0] + t * dx, a[1] + t * dy);
            ((p[0] - cx).powi(2) + (p[1] - cy).powi(2)).sqrt()
        }
    }
}

pub use super::lines::{channel_polygon, clip_line};

pub use super::measure::label as measure_label;

/// Snap `price` to the nearest of a bar's O/H/L/C (magnet mode).
pub fn magnet(price: f64, ohlc: [f64; 4]) -> f64 {
    ohlc.into_iter().min_by(|a, b| (a - price).abs().total_cmp(&(b - price).abs())).unwrap_or(price)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rays_clip_in_the_drawn_direction_including_left_vertical_and_offscreen_origins() {
        let r = [0., 0., 100., 100.];
        assert_eq!(clip_line([50., 50.], [25., 50.], r, false, true), Some(([50., 50.], [0., 50.])));
        assert_eq!(clip_line([50., 50.], [50., 60.], r, false, true), Some(([50., 50.], [50., 100.])));
        assert_eq!(clip_line([-10., 50.], [10., 50.], r, false, true), Some(([0., 50.], [100., 50.])));
        assert_eq!(clip_line([-10., 50.], [-20., 50.], r, false, true), None);
        assert_eq!(clip_line([50., 50.], [50., 50.], r, false, true), Some(([50., 50.], [50., 50.])));
        assert_eq!(clip_line([50., 50.], [50., 60.], r, true, true), Some(([50., 0.], [50., 100.])));
        assert!(clip_line([f64::NAN, 0.], [50., 50.], r, false, true).is_none());
    }

    #[test]
    fn channels_are_parallel_and_area_selection_handles_reversed_or_degenerate_bands() {
        for (a, b, c) in [
            ([10., 10.], [90., 50.], [20., 60.]),
            ([90., 50.], [10., 10.], [20., 60.]),
            ([50., 10.], [50., 80.], [80., 40.]),
        ] {
            let points = channel_polygon(a, b, c);
            let edge = [points[2][0] - points[3][0], points[2][1] - points[3][1]];
            assert!((edge[0] - (b[0] - a[0])).abs() < 1e-8);
            assert!((edge[1] - (b[1] - a[1])).abs() < 1e-8);
            let mid = [(points[0][0] + points[2][0]) / 2., (points[0][1] + points[2][1]) / 2.];
            assert_eq!(distance(&Shape::Polygon(points), mid), 0.);
            assert!(distance(&Shape::Polygon(points), [-100., -100.]) > 1.);
        }
        assert!(distance(&Shape::Polygon(channel_polygon([0., 0.], [10., 0.], [5., 0.])), [5., 20.]) > 19.);
        assert_eq!(distance(&Shape::Rect([20., 20.], [10., 10.]), [15., 15.]), 0.);
    }

    #[test]
    fn measurement_reports_signed_price_percent_intervals_and_calendar_time() {
        let a = Anchor { ts: 100, price: 100. };
        let b = Anchor { ts: 86500, price: 110. };
        assert_eq!(measure_label(a, b, 12, 2), "+10.00 (+10.00%) · 12 bars · 1d 0h");
        assert_eq!(measure_label(b, a, 12, 2), "-10.00 (-9.09%) · 12 bars · 1d 0h");
        assert!(measure_label(Anchor { price: 0., ..a }, b, 0, 2).contains("(n/a)"));
        assert!(measure_label(Anchor { price: -100., ..a }, Anchor { price: -90., ..b }, 1, 2).contains("(+10.00%)"));
    }

    #[test]
    fn fib_levels_retrace_from_b() {
        let d = Drawing::new(Tool::Fib, Anchor { ts: 0, price: 100.0 }, Anchor { ts: 1, price: 200.0 });
        let lv = d.fib_levels();
        assert_eq!(lv[0], (0.0, 200.0));
        assert_eq!(lv[3], (0.5, 150.0));
        assert!((lv[4].1 - 138.2).abs() < 1e-9);
        assert_eq!(lv[6], (1.0, 100.0));
    }
    #[test]
    fn extensions_project_the_swing_from_c_and_risk_rewards_reject_wrong_sides() {
        let mut d = Drawing::new(Tool::FibExtension, Anchor { ts: 1, price: 100. }, Anchor { ts: 2, price: 120. });
        d.c = Some(Anchor { ts: 3, price: 110. });
        assert_eq!(d.extension_levels()[2], (1., 130.));
        d.tool = Tool::LongPosition;
        d.a.price = 110.;
        d.b.price = 100.;
        d.c.as_mut().unwrap().price = 130.;
        assert_eq!(d.risk_reward(), Some((10., 20., 2.)));
        d.b.price = 120.;
        assert!(d.risk_reward().is_none());
        d.tool = Tool::ShortPosition;
        d.c.as_mut().unwrap().price = 90.;
        assert_eq!(d.risk_reward(), Some((10., 20., 2.)));
    }

    #[test]
    fn hit_distances() {
        assert_eq!(distance(&Shape::Segment([0.0, 0.0], [10.0, 0.0]), [5.0, 3.0]), 3.0);
        assert_eq!(distance(&Shape::Segment([0.0, 0.0], [10.0, 0.0]), [13.0, 4.0]), 5.0);
        assert_eq!(distance(&Shape::HLine(50.0), [999.0, 47.0]), 3.0);
        assert_eq!(distance(&Shape::VLine(10.0), [12.0, 0.0]), 2.0);
    }

    #[test]
    fn magnet_snaps() {
        assert_eq!(magnet(10.4, [10.0, 12.0, 9.0, 11.0]), 10.0);
        assert_eq!(magnet(11.7, [10.0, 12.0, 9.0, 11.0]), 12.0);
    }
}
