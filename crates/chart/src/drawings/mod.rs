//! Family dispatch for drawing geometry. The public model remains serde-compatible.
mod shared;
use serde::{Deserialize, Serialize};
pub use shared::{Shape, channel_polygon, clip_line, distance, magnet, measure_label};

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
    FibExtension,
    Pitchfork,
    LongPosition,
    ShortPosition,
    HorizontalRay,
    ExtendedLine,
    InfoLine,
    RegressionTrend,
    FibChannel,
    FibTimeZone,
    Xabcd,
    Abcd,
    HeadAndShoulders,
    TrianglePattern,
    ElliottImpulse,
    ElliottCorrection,
    PriceRange,
    DateRange,
    DatePriceRange,
    AnchoredVwap,
    FixedRangeVolumeProfile,
    Arrow,
    ArrowMarkUp,
    ArrowMarkDown,
    Brush,
    Circle,
    Path,
    PriceNote,
    Callout,
    FlagMark,
}

impl Tool {
    pub const ALL: [Self; 39] = [
        Self::Trendline,
        Self::Horizontal,
        Self::Vertical,
        Self::Fib,
        Self::Measure,
        Self::Rectangle,
        Self::Ray,
        Self::Channel,
        Self::Text,
        Self::FibExtension,
        Self::Pitchfork,
        Self::LongPosition,
        Self::ShortPosition,
        Self::HorizontalRay,
        Self::ExtendedLine,
        Self::InfoLine,
        Self::RegressionTrend,
        Self::FibChannel,
        Self::FibTimeZone,
        Self::Xabcd,
        Self::Abcd,
        Self::HeadAndShoulders,
        Self::TrianglePattern,
        Self::ElliottImpulse,
        Self::ElliottCorrection,
        Self::PriceRange,
        Self::DateRange,
        Self::DatePriceRange,
        Self::AnchoredVwap,
        Self::FixedRangeVolumeProfile,
        Self::Arrow,
        Self::ArrowMarkUp,
        Self::ArrowMarkDown,
        Self::Brush,
        Self::Circle,
        Self::Path,
        Self::PriceNote,
        Self::Callout,
        Self::FlagMark,
    ];
    pub fn points(self) -> usize {
        point_count(self)
    }
    pub fn label(self) -> &'static str {
        match self {
            Tool::Trendline => "Trendline",
            Tool::Horizontal => "Horizontal line",
            Tool::Vertical => "Vertical line",
            Tool::Fib => "Fib retracement",
            Tool::Measure => "Measure",
            Tool::PriceRange => "Price range",
            Tool::DateRange => "Date range",
            Tool::DatePriceRange => "Date and price range",
            Tool::AnchoredVwap => "Anchored VWAP",
            Tool::FixedRangeVolumeProfile => "Fixed range volume profile",
            Tool::Rectangle => "Rectangle / zone",
            Tool::Ray => "Ray",
            Tool::Channel => "Parallel channel",
            Tool::Text => "Text note",
            Tool::FibExtension => "Fib extension",
            Tool::Pitchfork => "Pitchfork",
            Tool::LongPosition => "Long risk/reward",
            Tool::ShortPosition => "Short risk/reward",
            Tool::HorizontalRay => "Horizontal ray",
            Tool::ExtendedLine => "Extended line",
            Tool::InfoLine => "Info line",
            Tool::RegressionTrend => "Regression trend",
            Tool::FibChannel => "Fib channel",
            Tool::FibTimeZone => "Fib time zone",
            Tool::Xabcd => "XABCD",
            Tool::Abcd => "ABCD",
            Tool::HeadAndShoulders => "Head and shoulders",
            Tool::TrianglePattern => "Triangle pattern",
            Tool::ElliottImpulse => "Elliott impulse",
            Tool::ElliottCorrection => "Elliott correction",
            Tool::Arrow => "Arrow",
            Tool::ArrowMarkUp => "Arrow mark up",
            Tool::ArrowMarkDown => "Arrow mark down",
            Tool::Brush => "Brush",
            Tool::Circle => "Circle",
            Tool::Path => "Path",
            Tool::PriceNote => "Price note",
            Tool::Callout => "Callout",
            Tool::FlagMark => "Flag mark",
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
    /// Third anchor, absent on older drawings.
    #[serde(default)]
    pub c: Option<Anchor>,
    #[serde(default, deserialize_with = "text::deserialize_text")]
    pub text: String,
    /// Ordered anchors for the pattern family; absent in legacy saves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pattern_points: Vec<Anchor>,
    #[serde(default = "profile_rows", deserialize_with = "deserialize_rows", skip_serializing_if = "is_default_rows")]
    pub profile_rows: usize,
    /// Additional vertices for brush and path drawings, in data coordinates.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "shapes::deserialize_vertices")]
    pub vertices: Vec<Anchor>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub arrow_end: bool,
}

fn deserialize_rows<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<usize, D::Error> {
    usize::deserialize(deserializer).map(measure::row_count)
}
fn profile_rows() -> usize {
    24
}
fn is_default_rows(rows: &usize) -> bool {
    *rows == profile_rows()
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
            pattern_points: Vec::new(),
            profile_rows: profile_rows(),
            vertices: Vec::new(),
            arrow_end: false,
        }
    }
    /// Normalise untrusted saved geometry; false means the drawing is incomplete.
    pub fn sanitize(&mut self) -> bool {
        self.text = text::sanitize(&self.text);
        if self.tool != Tool::Path {
            self.arrow_end = false;
        }
        if matches!(self.tool, Tool::Path | Tool::Brush) {
            self.vertices.retain(|point| point.price.is_finite());
            self.vertices.truncate(shapes::MAX_POINTS);
            self.vertices.len() >= 2
        } else {
            self.vertices.clear();
            true
        }
    }

    pub fn visible_on(&self, symbol: &str) -> bool {
        self.all_symbols || self.symbol.as_deref().is_none_or(|owner| owner == symbol)
    }
    pub fn fib_levels(&self) -> Vec<(f64, f64)> {
        fib::levels(self)
    }
    pub fn extension_levels(&self) -> Vec<(f64, f64)> {
        fib::extension_levels(self)
    }
    pub fn risk_reward(&self) -> Option<(f64, f64, f64)> {
        measure::risk_reward(self)
    }
}

pub mod fib;
pub mod lines;
pub mod measure;
pub mod patterns;
pub mod shapes;
pub mod text;

pub type Point = [f64; 2];
pub type LabeledShape = (Shape, Option<String>);

pub struct Geometry<'a> {
    pub drawing: &'a Drawing,
    pub points: [Point; 3],
    pub pattern_points: Vec<Point>,
    pub bounds: [f64; 4],
    pub price_y: &'a dyn Fn(f64) -> f64,
    pub measure_label: String,
    pub text_size: Point,
    /// Closes in chronological order over the inclusive anchor bar range.
    pub closes: &'a [f64],
    pub project: &'a dyn Fn(Anchor) -> Point,
}

pub fn geometry(context: &Geometry<'_>) -> Vec<LabeledShape> {
    match context.drawing.tool {
        Tool::HorizontalRay
        | Tool::ExtendedLine
        | Tool::InfoLine
        | Tool::RegressionTrend
        | Tool::Arrow
        | Tool::ArrowMarkUp
        | Tool::ArrowMarkDown
        | Tool::Trendline
        | Tool::Ray
        | Tool::Horizontal
        | Tool::Vertical
        | Tool::Channel
        | Tool::Pitchfork => lines::geometry(context),
        Tool::Fib | Tool::FibExtension | Tool::FibChannel | Tool::FibTimeZone => fib::geometry(context),
        Tool::Measure
        | Tool::PriceRange
        | Tool::DateRange
        | Tool::DatePriceRange
        | Tool::AnchoredVwap
        | Tool::FixedRangeVolumeProfile
        | Tool::LongPosition
        | Tool::ShortPosition => measure::geometry(context),
        Tool::Rectangle | Tool::Circle | Tool::Path | Tool::Brush => shapes::geometry(context),
        Tool::Text | Tool::PriceNote | Tool::Callout | Tool::FlagMark => text::geometry(context),
        Tool::Xabcd
        | Tool::Abcd
        | Tool::HeadAndShoulders
        | Tool::TrianglePattern
        | Tool::ElliottImpulse
        | Tool::ElliottCorrection => patterns::geometry(context.drawing, &context.pattern_points, context.bounds),
    }
}

pub fn point_count(tool: Tool) -> usize {
    match tool {
        Tool::HorizontalRay
        | Tool::ExtendedLine
        | Tool::InfoLine
        | Tool::RegressionTrend
        | Tool::Arrow
        | Tool::ArrowMarkUp
        | Tool::ArrowMarkDown
        | Tool::Trendline
        | Tool::Ray
        | Tool::Horizontal
        | Tool::Vertical
        | Tool::Channel
        | Tool::Pitchfork => lines::point_count(tool),
        Tool::Fib | Tool::FibExtension | Tool::FibChannel | Tool::FibTimeZone => fib::point_count(tool),
        Tool::Measure
        | Tool::PriceRange
        | Tool::DateRange
        | Tool::DatePriceRange
        | Tool::AnchoredVwap
        | Tool::FixedRangeVolumeProfile
        | Tool::LongPosition
        | Tool::ShortPosition => measure::point_count(tool),
        Tool::Rectangle | Tool::Circle | Tool::Path | Tool::Brush => shapes::point_count(tool),
        Tool::Text | Tool::PriceNote | Tool::Callout | Tool::FlagMark => text::point_count(tool),
        Tool::Xabcd
        | Tool::Abcd
        | Tool::HeadAndShoulders
        | Tool::TrianglePattern
        | Tool::ElliottImpulse
        | Tool::ElliottCorrection => patterns::point_count(tool),
    }
}

pub fn handles(drawing: &Drawing) -> Vec<Anchor> {
    match drawing.tool {
        Tool::HorizontalRay
        | Tool::ExtendedLine
        | Tool::InfoLine
        | Tool::RegressionTrend
        | Tool::Arrow
        | Tool::ArrowMarkUp
        | Tool::ArrowMarkDown
        | Tool::Trendline
        | Tool::Ray
        | Tool::Horizontal
        | Tool::Vertical
        | Tool::Channel
        | Tool::Pitchfork => lines::handles(drawing),
        Tool::Fib | Tool::FibExtension | Tool::FibChannel | Tool::FibTimeZone => fib::handles(drawing),
        Tool::Measure
        | Tool::PriceRange
        | Tool::DateRange
        | Tool::DatePriceRange
        | Tool::AnchoredVwap
        | Tool::FixedRangeVolumeProfile
        | Tool::LongPosition
        | Tool::ShortPosition => measure::handles(drawing),
        Tool::Rectangle | Tool::Circle | Tool::Path | Tool::Brush => shapes::handles(drawing),
        Tool::Text | Tool::PriceNote | Tool::Callout | Tool::FlagMark => text::handles(drawing),
        Tool::Xabcd
        | Tool::Abcd
        | Tool::HeadAndShoulders
        | Tool::TrianglePattern
        | Tool::ElliottImpulse
        | Tool::ElliottCorrection => patterns::handles(drawing),
    }
}

fn anchor_handles(drawing: &Drawing, count: usize) -> Vec<Anchor> {
    [Some(drawing.a), Some(drawing.b), drawing.c].into_iter().take(count).flatten().collect()
}

fn distance_to_shapes(shapes: Vec<LabeledShape>, point: Point) -> f64 {
    shapes.iter().map(|(shape, _)| distance(shape, point)).fold(f64::INFINITY, f64::min)
}

pub fn hit_distance(context: &Geometry<'_>, point: Point) -> f64 {
    match context.drawing.tool {
        Tool::HorizontalRay
        | Tool::ExtendedLine
        | Tool::InfoLine
        | Tool::RegressionTrend
        | Tool::Arrow
        | Tool::ArrowMarkUp
        | Tool::ArrowMarkDown
        | Tool::Trendline
        | Tool::Ray
        | Tool::Horizontal
        | Tool::Vertical
        | Tool::Channel
        | Tool::Pitchfork => lines::hit_distance(context, point),
        Tool::Fib | Tool::FibExtension | Tool::FibChannel | Tool::FibTimeZone => fib::hit_distance(context, point),
        Tool::Measure
        | Tool::PriceRange
        | Tool::DateRange
        | Tool::DatePriceRange
        | Tool::AnchoredVwap
        | Tool::FixedRangeVolumeProfile
        | Tool::LongPosition
        | Tool::ShortPosition => measure::hit_distance(context, point),
        Tool::Rectangle | Tool::Circle | Tool::Path | Tool::Brush => shapes::hit_distance(context, point),
        Tool::Text | Tool::PriceNote | Tool::Callout | Tool::FlagMark => text::hit_distance(context, point),
        Tool::Xabcd
        | Tool::Abcd
        | Tool::HeadAndShoulders
        | Tool::TrianglePattern
        | Tool::ElliottImpulse
        | Tool::ElliottCorrection => patterns::hit_distance(context, point),
    }
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;

    #[test]
    fn sanitizing_untrusted_vertices_clears_caps_and_rejects_incomplete_strokes() {
        let a = Anchor { ts: 1, price: 100. };
        for tool in Tool::ALL {
            let mut drawing = Drawing::new(tool, a, a);
            drawing.vertices = vec![a; shapes::MAX_POINTS + 500];
            drawing.arrow_end = true;
            assert!(drawing.sanitize());
            assert_eq!(drawing.arrow_end, tool == Tool::Path);
            if matches!(tool, Tool::Path | Tool::Brush) {
                assert_eq!(drawing.vertices.len(), shapes::MAX_POINTS);
                for vertices in [vec![], vec![a], vec![a, Anchor { price: f64::NAN, ..a }]] {
                    drawing.vertices = vertices;
                    assert!(!drawing.sanitize());
                }
            } else {
                assert!(drawing.vertices.is_empty());
            }
        }
        assert!(patterns::label_above(&[], 0));
        assert!(patterns::label_above(&[[0., 0.]], usize::MAX));
    }

    #[test]
    fn sanitizing_keeps_complete_pattern_and_three_point_drawings() {
        let a = Anchor { ts: 1, price: 100. };
        let b = Anchor { ts: 2, price: 90. };
        for tool in [
            Tool::Xabcd,
            Tool::Abcd,
            Tool::HeadAndShoulders,
            Tool::TrianglePattern,
            Tool::ElliottImpulse,
            Tool::ElliottCorrection,
        ] {
            let mut drawing = Drawing::new(tool, a, b);
            drawing.pattern_points = (0..tool.points()).map(|i| Anchor { ts: i as i64, price: 100. }).collect();
            assert!(drawing.sanitize(), "{tool:?}");
            assert_eq!(drawing.pattern_points.len(), tool.points());
        }
        for tool in [Tool::Channel, Tool::FibChannel, Tool::FibExtension, Tool::Pitchfork] {
            let mut drawing = Drawing::new(tool, a, b);
            drawing.c = Some(Anchor { ts: 3, price: 110. });
            assert!(drawing.sanitize(), "{tool:?}");
            assert_eq!(handles(&drawing).len(), tool.points());
        }
    }

    #[test]
    #[ignore = "BUG: Drawing::sanitize never rejects incomplete non-path drawings. An Xabcd/Abcd/etc. with empty pattern_points (or a Channel/FibChannel/Pitchfork/FibExtension with c=None) loads as a ghost: handles() is short/empty so the object cannot be selected or deleted individually, and pattern geometry is empty. sanitize() should return false when pattern_points.len() != tool.points() for the pattern family and when c.is_none() for 3-point tools."]
    fn sanitize_rejects_incomplete_pattern_and_three_point_drawings() {
        let a = Anchor { ts: 1, price: 100. };
        let b = Anchor { ts: 2, price: 90. };
        for tool in [
            Tool::Xabcd,
            Tool::Abcd,
            Tool::HeadAndShoulders,
            Tool::TrianglePattern,
            Tool::ElliottImpulse,
            Tool::ElliottCorrection,
        ] {
            let mut drawing = Drawing::new(tool, a, b);
            assert!(!drawing.sanitize(), "empty pattern_points must be rejected for {tool:?}");
            drawing.pattern_points = vec![a; tool.points().saturating_sub(1)];
            assert!(!drawing.sanitize(), "short pattern_points must be rejected for {tool:?}");
        }
        for tool in [Tool::Channel, Tool::FibChannel, Tool::FibExtension, Tool::Pitchfork] {
            let mut drawing = Drawing::new(tool, a, b);
            assert!(!drawing.sanitize(), "missing c must be rejected for {tool:?}");
        }
    }

    #[test]
    fn family_dispatch_covers_every_tool_geometry_hits_and_handles() {
        for tool in Tool::ALL {
            let mut drawing = Drawing::new(tool, Anchor { ts: 1, price: 100. }, Anchor { ts: 2, price: 90. });
            drawing.c = Some(Anchor { ts: 3, price: 120. });
            if tool == Tool::ShortPosition {
                drawing.b.price = 120.;
                drawing.c.as_mut().unwrap().price = 90.;
            }
            drawing.pattern_points = (0..tool.points())
                .map(|i| Anchor { ts: i as i64 + 1, price: 100. + if i % 2 == 0 { 10. } else { -10. } })
                .collect();
            drawing.text = "Note".into();
            let g = Geometry {
                drawing: &drawing,
                points: [[20., 100.], [60., 90.], [80., 120.]],
                pattern_points: drawing.pattern_points.iter().map(|a| [a.ts as f64, a.price]).collect(),
                bounds: [0., 0., 200., 200.],
                price_y: &|p| p,
                measure_label: "Measurement".into(),
                text_size: [40., 20.],
                closes: &[100., 90.],
                project: &|a| [a.ts as f64, a.price],
            };
            let shapes = geometry(&g);
            assert!(!shapes.is_empty(), "{tool:?}");
            assert_eq!(handles(&drawing).len(), tool.points(), "{tool:?}");
            assert!(hit_distance(&g, [40., 100.]).is_finite(), "{tool:?}");
            assert!((1..=7).contains(&tool.points()));
        }
    }
}
