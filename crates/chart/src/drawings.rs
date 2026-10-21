//! User drawings. Anchors are stored as (timestamp, price) so they survive pan, zoom and
//! timeframe changes; they are mapped to bar indices only when drawn.

use serde::{Deserialize, Serialize};

pub const FIB_LEVELS: [f64; 7] = [0.0, 0.236, 0.382, 0.5, 0.618, 0.786, 1.0];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub ts: i64,
    pub price: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tool {
    Trendline,
    Horizontal,
    Vertical,
    Fib,
    Measure,
    Rectangle,
    Ray,
    Channel,
    Text,
}

impl Tool {
    pub fn points(self) -> usize {
        match self {
            Tool::Horizontal | Tool::Vertical | Tool::Text => 1,
            Tool::Trendline | Tool::Fib | Tool::Measure | Tool::Rectangle | Tool::Ray => 2,
            Tool::Channel => 3,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tool::Trendline => "Trendline",
            Tool::Horizontal => "Horizontal line",
            Tool::Vertical => "Vertical line",
            Tool::Fib => "Fib retracement",
            Tool::Measure => "Measure",
            Tool::Rectangle => "Rectangle / zone",
            Tool::Ray => "Ray",
            Tool::Channel => "Parallel channel",
            Tool::Text => "Text note",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Drawing {
    pub tool: Tool,
    pub a: Anchor,
    pub b: Anchor,
    /// None is a legacy annotation, adopted by its saved chart's symbol on load.
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub all_symbols: bool,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default = "line_width")]
    pub width: f32,
    #[serde(default)]
    pub extend_left: bool,
    #[serde(default)]
    pub extend_right: bool,
    /// Width anchor for a parallel channel; absent on older drawings.
    #[serde(default)]
    pub c: Option<Anchor>,
    #[serde(default)]
    pub text: String,
}

fn line_width() -> f32 {
    1.3
}

impl Drawing {
    pub fn new(tool: Tool, a: Anchor, b: Anchor) -> Self {
        Self {
            tool,
            a,
            b,
            symbol: None,
            all_symbols: false,
            color: None,
            width: line_width(),
            extend_left: false,
            extend_right: false,
            c: None,
            text: String::new(),
        }
    }

    pub fn visible_on(&self, symbol: &str) -> bool {
        self.all_symbols || self.symbol.as_deref().is_none_or(|owner| owner == symbol)
    }

    /// Fib levels as `(ratio, price)`, measured from `b` (0) back to `a` (1), the
    /// usual retracement convention for a swing drawn from `a` to `b`.
    pub fn fib_levels(&self) -> Vec<(f64, f64)> {
        FIB_LEVELS.iter().map(|&r| (r, self.b.price + (self.a.price - self.b.price) * r)).collect()
    }
}

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

/// Clip a parametric segment/ray/line to a viewport. A ray follows A → B, including
/// leftward and vertical rays; a coincident pair remains a selectable point.
pub fn clip_line(
    a: [f64; 2],
    b: [f64; 2],
    bounds: [f64; 4],
    extend_start: bool,
    extend_end: bool,
) -> Option<([f64; 2], [f64; 2])> {
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

/// A parallel band with A → B as its baseline and C on the other edge. Projection
/// uses displayed coordinates so the band stays parallel on logarithmic charts.
pub fn channel_polygon(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> [[f64; 2]; 4] {
    let delta = [b[0] - a[0], b[1] - a[1]];
    let len2 = delta[0].powi(2) + delta[1].powi(2);
    let t = if len2 <= 1e-10 { 0. } else { ((c[0] - a[0]) * delta[0] + (c[1] - a[1]) * delta[1]) / len2 };
    let offset = [c[0] - a[0] - t * delta[0], c[1] - a[1] - t * delta[1]];
    [a, b, [b[0] + offset[0], b[1] + offset[1]], [a[0] + offset[0], a[1] + offset[1]]]
}

pub fn measure_label(a: Anchor, b: Anchor, bar_intervals: usize, decimals: usize) -> String {
    let change = b.price - a.price;
    let pct = if a.price == 0. { "n/a".into() } else { format!("{:+.2}%", change / a.price.abs() * 100.) };
    let secs = b.ts.abs_diff(a.ts);
    let elapsed = if secs >= 86400 {
        format!("{}d {}h", secs / 86400, secs % 86400 / 3600)
    } else if secs >= 3600 {
        format!("{}h {}m", secs / 3600, secs % 3600 / 60)
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    };
    format!("{change:+.decimals$} ({pct}) · {bar_intervals} bars · {elapsed}")
}

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
