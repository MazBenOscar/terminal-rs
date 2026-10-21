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
    pub const ALL: [Self; 22] = [
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
            Tool::Rectangle => "Rectangle / zone",
            Tool::Ray => "Ray",
            Tool::Channel => "Parallel channel",
            Tool::Text => "Text note",
            Tool::FibExtension => "Fib extension",
            Tool::Pitchfork => "Pitchfork",
            Tool::LongPosition => "Long risk/reward",
            Tool::ShortPosition => "Short risk/reward",
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
    /// Additional vertices for brush and path drawings, in data coordinates.
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "shapes::deserialize_vertices")]
    pub vertices: Vec<Anchor>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub arrow_end: bool,
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
            vertices: Vec::new(),
            arrow_end: false,
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
pub mod shapes;
pub mod text;

pub type Point = [f64; 2];
pub type LabeledShape = (Shape, Option<String>);

pub struct Geometry<'a> {
    pub drawing: &'a Drawing,
    pub points: [Point; 3],
    pub bounds: [f64; 4],
    pub price_y: &'a dyn Fn(f64) -> f64,
    pub measure_label: String,
    pub text_size: Point,
    pub project: &'a dyn Fn(Anchor) -> Point,
}

pub fn geometry(context: &Geometry<'_>) -> Vec<LabeledShape> {
    match context.drawing.tool {
        Tool::Arrow
        | Tool::ArrowMarkUp
        | Tool::ArrowMarkDown
        | Tool::Trendline
        | Tool::Ray
        | Tool::Horizontal
        | Tool::Vertical
        | Tool::Channel
        | Tool::Pitchfork => lines::geometry(context),
        Tool::Fib | Tool::FibExtension => fib::geometry(context),
        Tool::Measure | Tool::LongPosition | Tool::ShortPosition => measure::geometry(context),
        Tool::Rectangle | Tool::Circle | Tool::Path | Tool::Brush => shapes::geometry(context),
        Tool::Text | Tool::PriceNote | Tool::Callout | Tool::FlagMark => text::geometry(context),
    }
}

pub fn point_count(tool: Tool) -> usize {
    match tool {
        Tool::Arrow
        | Tool::ArrowMarkUp
        | Tool::ArrowMarkDown
        | Tool::Trendline
        | Tool::Ray
        | Tool::Horizontal
        | Tool::Vertical
        | Tool::Channel
        | Tool::Pitchfork => lines::point_count(tool),
        Tool::Fib | Tool::FibExtension => fib::point_count(tool),
        Tool::Measure | Tool::LongPosition | Tool::ShortPosition => measure::point_count(tool),
        Tool::Rectangle | Tool::Circle | Tool::Path | Tool::Brush => shapes::point_count(tool),
        Tool::Text | Tool::PriceNote | Tool::Callout | Tool::FlagMark => text::point_count(tool),
    }
}

pub fn handles(drawing: &Drawing) -> Vec<Anchor> {
    match drawing.tool {
        Tool::Arrow
        | Tool::ArrowMarkUp
        | Tool::ArrowMarkDown
        | Tool::Trendline
        | Tool::Ray
        | Tool::Horizontal
        | Tool::Vertical
        | Tool::Channel
        | Tool::Pitchfork => lines::handles(drawing),
        Tool::Fib | Tool::FibExtension => fib::handles(drawing),
        Tool::Measure | Tool::LongPosition | Tool::ShortPosition => measure::handles(drawing),
        Tool::Rectangle | Tool::Circle | Tool::Path | Tool::Brush => shapes::handles(drawing),
        Tool::Text | Tool::PriceNote | Tool::Callout | Tool::FlagMark => text::handles(drawing),
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
        Tool::Arrow
        | Tool::ArrowMarkUp
        | Tool::ArrowMarkDown
        | Tool::Trendline
        | Tool::Ray
        | Tool::Horizontal
        | Tool::Vertical
        | Tool::Channel
        | Tool::Pitchfork => lines::hit_distance(context, point),
        Tool::Fib | Tool::FibExtension => fib::hit_distance(context, point),
        Tool::Measure | Tool::LongPosition | Tool::ShortPosition => measure::hit_distance(context, point),
        Tool::Rectangle | Tool::Circle | Tool::Path | Tool::Brush => shapes::hit_distance(context, point),
        Tool::Text | Tool::PriceNote | Tool::Callout | Tool::FlagMark => text::hit_distance(context, point),
    }
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;

    #[test]
    fn family_dispatch_covers_every_tool_geometry_hits_and_handles() {
        for tool in Tool::ALL {
            let mut drawing = Drawing::new(tool, Anchor { ts: 1, price: 100. }, Anchor { ts: 2, price: 90. });
            drawing.c = Some(Anchor { ts: 3, price: 120. });
            if tool == Tool::ShortPosition {
                drawing.b.price = 120.;
                drawing.c.as_mut().unwrap().price = 90.;
            }
            drawing.text = "Note".into();
            let g = Geometry {
                drawing: &drawing,
                points: [[20., 100.], [60., 90.], [80., 120.]],
                bounds: [0., 0., 200., 200.],
                price_y: &|p| p,
                measure_label: "Measurement".into(),
                text_size: [40., 20.],
                project: &|a| [a.ts as f64, a.price],
            };
            let shapes = geometry(&g);
            assert!(!shapes.is_empty(), "{tool:?}");
            assert_eq!(handles(&drawing).len(), tool.points(), "{tool:?}");
            assert!(hit_distance(&g, [40., 100.]).is_finite(), "{tool:?}");
            assert!((1..=3).contains(&tool.points()));
        }
    }
}
