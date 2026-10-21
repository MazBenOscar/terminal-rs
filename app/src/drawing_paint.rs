//! Per-family drawing icon painters. Chart geometry is dispatched by `tr_chart::drawings`.
use crate::widgets::Icon;
use egui::{Color32, Painter, Rect, Shape, Stroke, StrokeKind};

use crate::{
    chart_view::{label_plate, label_text},
    theme::{self, mono, ui_font},
};
use egui::{Align2, pos2, vec2};
use tr_chart::drawings::{Drawing, Shape as DrawingShape, Tool};

type LabeledShape = (DrawingShape, Option<String>);
pub struct Paint<'a> {
    pub painter: &'a Painter,
    pub rect: Rect,
    pub stroke: Stroke,
    pub selected: bool,
}

pub fn paint(drawing: &Drawing, paint: &Paint<'_>, shapes: Vec<LabeledShape>) {
    match drawing.tool {
        Tool::HorizontalRay
        | Tool::ExtendedLine
        | Tool::InfoLine
        | Tool::RegressionTrend
        | Tool::Trendline
        | Tool::Ray
        | Tool::Horizontal
        | Tool::Vertical
        | Tool::Channel
        | Tool::Pitchfork => paint_lines(paint, shapes),
        Tool::Fib | Tool::FibExtension | Tool::FibChannel | Tool::FibTimeZone => paint_fib(paint, shapes),
        Tool::Measure
        | Tool::PriceRange
        | Tool::DateRange
        | Tool::DatePriceRange
        | Tool::LongPosition
        | Tool::ShortPosition => {
            paint_measure(paint, shapes, matches!(drawing.tool, Tool::LongPosition | Tool::ShortPosition))
        }
        Tool::Arrow | Tool::ArrowMarkUp | Tool::ArrowMarkDown => paint_arrows(paint, shapes, drawing.tool),
        Tool::AnchoredVwap | Tool::FixedRangeVolumeProfile => paint_volume(paint, shapes),
        Tool::Rectangle | Tool::Circle | Tool::Path => paint_shapes(paint, shapes),
        Tool::Brush => paint_brush(paint, shapes),
        Tool::Text | Tool::PriceNote | Tool::Callout | Tool::FlagMark => paint_text(paint, shapes, drawing.tool),
        Tool::Xabcd
        | Tool::Abcd
        | Tool::HeadAndShoulders
        | Tool::TrianglePattern
        | Tool::ElliottImpulse
        | Tool::ElliottCorrection => paint_patterns(paint, shapes),
    }
}

fn paint_patterns(p: &Paint<'_>, shapes: Vec<LabeledShape>) {
    let (labels, geometry): (Vec<_>, Vec<_>) = shapes.into_iter().partition(|(_, label)| label.is_some());
    primitives(p, geometry, RectStyle::Plain);
    for (shape, label) in labels {
        if let Some(label) = label
            && let DrawingShape::Segment(a, _) = shape
        {
            label_text(
                p.painter,
                pos2(a[0] as f32, a[1] as f32),
                Align2::CENTER_CENTER,
                label,
                mono(10.),
                p.stroke.color,
            );
        }
    }
}

fn paint_arrows(p: &Paint<'_>, shapes: Vec<LabeledShape>, tool: Tool) {
    if tool == Tool::Arrow {
        primitives(p, shapes, RectStyle::Plain);
        return;
    }
    let color = if tool == Tool::ArrowMarkUp { theme::up() } else { theme::down() };
    for (shape, _) in shapes {
        if let DrawingShape::Polygon(points) = shape {
            let points = points[..3].iter().map(|a| pos2(a[0] as f32, a[1] as f32)).collect();
            p.painter.add(Shape::convex_polygon(points, color, Stroke::new(p.stroke.width, color)));
        }
    }
}
fn paint_brush(p: &Paint<'_>, shapes: Vec<LabeledShape>) {
    primitives(p, shapes, RectStyle::Plain);
}
fn paint_lines(p: &Paint<'_>, shapes: Vec<LabeledShape>) {
    primitives(p, shapes, RectStyle::Plain);
}
fn paint_fib(p: &Paint<'_>, shapes: Vec<LabeledShape>) {
    primitives(p, shapes, RectStyle::Plain);
}
fn paint_shapes(p: &Paint<'_>, shapes: Vec<LabeledShape>) {
    primitives(p, shapes, RectStyle::Plain);
}
fn paint_measure(p: &Paint<'_>, shapes: Vec<LabeledShape>, risk_reward: bool) {
    primitives(p, shapes, if risk_reward { RectStyle::RiskReward } else { RectStyle::Plain });
}
fn paint_volume(p: &Paint<'_>, shapes: Vec<LabeledShape>) {
    for (shape, label) in shapes {
        let poc = label.as_deref() == Some("Point of control");
        let value_area = label.as_deref() == Some("Value area");
        let context = Paint {
            stroke: Stroke::new(
                if poc { p.stroke.width + 1.5 } else { p.stroke.width },
                if poc || value_area { p.stroke.color } else { p.stroke.color.gamma_multiply(0.45) },
            ),
            ..*p
        };
        primitives(&context, vec![(shape, if poc { label } else { None })], RectStyle::Plain);
    }
}
fn paint_text(p: &Paint<'_>, shapes: Vec<LabeledShape>, tool: Tool) {
    primitives(p, shapes, if tool == Tool::Callout { RectStyle::Callout } else { RectStyle::Text });
}

enum RectStyle {
    Plain,
    RiskReward,
    Text,
    Callout,
}
fn primitives(context: &Paint<'_>, shapes: Vec<LabeledShape>, style: RectStyle) {
    let Paint { painter: p, rect, stroke, selected } = *context;
    let color = stroke.color;
    // Paint all geometry before labels so later lines and translucent fills cannot
    // cover a label's plate (notably info/regression lines and Fib levels).
    for (shape, label) in &shapes {
        match shape {
            DrawingShape::Segment(a, b) => {
                p.line_segment([pos2(a[0] as f32, a[1] as f32), pos2(b[0] as f32, b[1] as f32)], stroke);
            }
            DrawingShape::DashedSegment(a, b) => {
                let a = pos2(a[0] as f32, a[1] as f32);
                let b = pos2(b[0] as f32, b[1] as f32);
                p.extend(Shape::dashed_line(&[a, b], stroke, 4., 4.));
            }
            DrawingShape::HLine(y) => {
                p.hline(rect.x_range(), *y as f32, stroke);
            }
            DrawingShape::VLine(x) => {
                p.vline(*x as f32, rect.y_range(), stroke);
            }
            DrawingShape::Rect(a, b) => {
                let r = Rect::from_two_pos(pos2(a[0] as f32, a[1] as f32), pos2(b[0] as f32, b[1] as f32));
                if matches!(style, RectStyle::Text | RectStyle::Callout) {
                    if selected || matches!(style, RectStyle::Callout) {
                        p.rect_stroke(r, 2., stroke, StrokeKind::Inside);
                    }
                } else {
                    let fill = if matches!(style, RectStyle::RiskReward) {
                        if label.as_ref().is_some_and(|s| s.starts_with("Risk")) { theme::down() } else { theme::up() }
                    } else {
                        color
                    };
                    p.rect_filled(r, 0., fill.gamma_multiply(0.12));
                    p.rect_stroke(r, 0., stroke, StrokeKind::Inside);
                }
            }
            DrawingShape::Polygon(points) => {
                let points = points.iter().map(|p| pos2(p[0] as f32, p[1] as f32)).collect();
                p.add(Shape::convex_polygon(points, color.gamma_multiply(0.08), stroke));
            }
        }
    }
    // label_text uses label_plate too; wrapped labels below use it directly.
    for (shape, label) in shapes {
        match shape {
            DrawingShape::Segment(a, b) => {
                if let Some(label) = label {
                    label_text(
                        p,
                        if a == b {
                            pos2(a[0] as f32 - 4., a[1] as f32 - 1.)
                        } else {
                            pos2(a[0] as f32 + 3., a[1] as f32 - 1.)
                        },
                        if a == b { Align2::RIGHT_BOTTOM } else { Align2::LEFT_BOTTOM },
                        label,
                        mono(10.),
                        color,
                    );
                }
            }
            DrawingShape::HLine(y) => {
                if let Some(label) = label {
                    label_text(
                        p,
                        pos2(rect.right() - 4., y as f32 - 1.),
                        Align2::RIGHT_BOTTOM,
                        label,
                        mono(10.),
                        color,
                    );
                }
            }
            DrawingShape::VLine(x) => {
                if let Some(label) = label {
                    label_text(p, pos2(x as f32 + 3., rect.top() + 3.), Align2::LEFT_TOP, label, mono(10.), color);
                }
            }
            DrawingShape::Rect(a, b) => {
                let r = Rect::from_two_pos(pos2(a[0] as f32, a[1] as f32), pos2(b[0] as f32, b[1] as f32));
                if matches!(style, RectStyle::Text | RectStyle::Callout) {
                    let galley = p.layout(label.unwrap_or_default(), ui_font(11.5), color, 220.);
                    label_plate(p, Rect::from_min_size(r.min + vec2(4., 2.), galley.size()));
                    p.galley(r.min + vec2(4., 2.), galley, color);
                } else {
                    if let Some(label) = label {
                        let galley = p.layout(label, mono(11.), color, rect.width().min(330.));
                        let size = galley.size() + vec2(8., 4.);
                        let min = pos2(
                            r.left().clamp(rect.left(), (rect.right() - size.x).max(rect.left())),
                            (r.top() - size.y - 2.).max(rect.top()),
                        );
                        label_plate(p, Rect::from_min_size(min + vec2(4., 2.), galley.size()));
                        p.galley(min + vec2(4., 2.), galley, color);
                    }
                }
            }
            DrawingShape::DashedSegment(..) | DrawingShape::Polygon(_) => {}
        }
    }
}

#[test]
fn drawing_labels_have_plates_above_geometry() {
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let painter = ui.ctx().layer_painter(egui::LayerId::background());
        let paint = Paint {
            painter: &painter,
            rect: Rect::from_min_size(pos2(0., 0.), vec2(500., 300.)),
            stroke: Stroke::new(1., Color32::WHITE),
            selected: false,
        };
        primitives(
            &paint,
            vec![
                (DrawingShape::Segment([20., 60.], [200., 80.]), Some("Info / regression / Fib".into())),
                (DrawingShape::HLine(100.), Some("Level".into())),
                (DrawingShape::VLine(150.), Some("Time zone".into())),
                (DrawingShape::Rect([20., 150.], [200., 200.]), Some("Range / profile".into())),
            ],
            RectStyle::Plain,
        );
        for style in [RectStyle::Text, RectStyle::Callout] {
            primitives(
                &paint,
                vec![(DrawingShape::Rect([220., 100.], [400., 150.]), Some("Price note / callout".into()))],
                style,
            );
        }
        paint_patterns(
            &paint,
            vec![
                (DrawingShape::Segment([250., 200.], [250., 200.]), Some("Point / ratio".into())),
                (DrawingShape::Segment([250., 200.], [350., 250.]), None),
            ],
        );
    });
    output.textures_delta.clear();
    let mut labels = 0;
    for pair in output.shapes.windows(2) {
        if let Shape::Text(text) = &pair[1].shape {
            let Shape::Rect(plate) = &pair[0].shape else {
                panic!("drawing text must be immediately preceded by its plate");
            };
            assert!(plate.fill.a() > 0);
            assert!(plate.rect.contains_rect(Rect::from_min_size(text.pos, text.galley.size())));
            labels += 1;
        }
    }
    assert_eq!(labels, 7);
}

struct Glyph {
    rect: Rect,
    stroke: Stroke,
    unit: f32,
}
impl Glyph {
    fn new(rect: Rect, color: Color32) -> Self {
        Self { rect, stroke: Stroke::new(1.4, color), unit: rect.height().min(rect.width()) / 16. }
    }
    fn point(&self, x: f32, y: f32) -> egui::Pos2 {
        self.rect.center() + egui::vec2(x * self.unit, y * self.unit)
    }
}

pub fn lines(p: &Painter, r: Rect, icon: Icon, color: Color32) {
    let g = Glyph::new(r, color);
    let pt = |x, y| g.point(x, y);
    let s = g.stroke;
    match icon {
        Icon::HorizontalRay => {
            p.circle_stroke(pt(-5., 0.), g.unit, s);
            p.line_segment([pt(-4., 0.), pt(6., 0.)], s);
            p.line_segment([pt(6., -2.), pt(6., 2.)], s);
        }
        Icon::ExtendedLine => {
            p.line_segment([pt(-6., 6.), pt(6., -6.)], s);
            for (x, y) in [(-2., 2.), (2., -2.)] {
                p.circle_stroke(pt(x, y), g.unit, s);
            }
        }
        Icon::InfoLine => {
            p.line_segment([pt(-6., 5.), pt(2., -3.)], s);
            p.rect_stroke(Rect::from_min_max(pt(0., -6.), pt(6., -1.)), 0., s, StrokeKind::Middle);
            p.line_segment([pt(2., -3.5), pt(4., -3.5)], s);
        }
        Icon::RegressionTrend => {
            for offset in [-3., 0., 3.] {
                p.line_segment([pt(-6., 3. + offset), pt(6., -3. + offset)], s);
            }
            p.circle_stroke(pt(-2., 0.), g.unit, s);
            p.circle_stroke(pt(3., -1.), g.unit, s);
        }
        Icon::Trendline | Icon::Ray | Icon::Channel => {
            p.line_segment([pt(-4., 4.), pt(5., -5.)], s);
            p.circle_stroke(pt(-5., 5.), 1.4 * g.unit, s);
            if icon != Icon::Ray {
                p.circle_stroke(pt(5., -5.), 1.4 * g.unit, s);
            }
            if icon == Icon::Channel {
                p.line_segment([pt(-5., 0.), pt(0., -5.)], s);
            }
            if icon == Icon::Ray {
                p.line_segment([pt(5., -5.), pt(7., -7.)], s);
            }
        }
        Icon::HorizontalLine | Icon::VerticalLine => {
            let (a, b) =
                if icon == Icon::HorizontalLine { (pt(-6., 0.), pt(6., 0.)) } else { (pt(0., -6.), pt(0., 6.)) };
            p.line_segment([a, b], s);
            p.circle_stroke(r.center(), 1.4 * g.unit, s);
        }
        Icon::Pitchfork => {
            p.circle_stroke(pt(-5., 5.), 1.4 * g.unit, s);
            p.line_segment([pt(-4., 4.), pt(4., -4.)], s);
            p.add(Shape::line(vec![pt(-5., -1.), pt(-1., 1.), pt(1., 5.)], s));
            p.line_segment([pt(-5., -1.), pt(0., -6.)], s);
            p.line_segment([pt(1., 5.), pt(6., 0.)], s);
        }
        _ => unreachable!("Non-line icon"),
    }
}

pub fn fib(p: &Painter, r: Rect, icon: Icon, color: Color32) {
    let g = Glyph::new(r, color);
    let pt = |x, y| g.point(x, y);
    if icon == Icon::FibTimeZone {
        for x in [-6., -4., -2., 1., 6.] {
            p.line_segment([pt(x, -5.), pt(x, 5.)], g.stroke);
        }
        return;
    }
    if icon == Icon::FibChannel {
        for offset in [-3., -1., 0., 3.] {
            p.line_segment([pt(-6., 3. + offset), pt(6., -3. + offset)], g.stroke);
        }
        return;
    }
    let n = if icon == Icon::FibRetracement { 4 } else { 3 };
    for i in 0..n {
        let y = -5. + i as f32 * 3.;
        p.line_segment([pt(-5., y), pt(6. - i as f32 * 1.5, y)], g.stroke);
    }
    if icon == Icon::FibRetracement {
        p.circle_stroke(pt(-5., -5.), 1.4 * g.unit, g.stroke);
        p.circle_stroke(pt(1.5, 4.), 1.4 * g.unit, g.stroke);
    } else {
        p.line_segment([pt(-6., 5.), pt(-2., -5.)], g.stroke);
    }
}

pub fn shapes(p: &Painter, r: Rect, color: Color32) {
    let g = Glyph::new(r, color);
    p.rect_stroke(Rect::from_min_max(g.point(-5., -5.), g.point(5., 5.)), 0., g.stroke, StrokeKind::Middle);
    for (x, y) in [(-5., -5.), (5., -5.), (-5., 5.), (5., 5.)] {
        p.circle_stroke(g.point(x, y), 1.4 * g.unit, g.stroke);
    }
}

pub fn text(p: &Painter, r: Rect, color: Color32) {
    let g = Glyph::new(r, color);
    p.line_segment([g.point(-6., -5.), g.point(6., -5.)], g.stroke);
    p.line_segment([g.point(0., -5.), g.point(0., 6.)], g.stroke);
}

pub fn measure(p: &Painter, r: Rect, icon: Icon, color: Color32) {
    let g = Glyph::new(r, color);
    let pt = |x, y| g.point(x, y);
    if icon == Icon::Measure {
        p.add(Shape::line(vec![pt(-6., 3.), pt(3., -6.), pt(6., -3.), pt(-3., 6.), pt(-6., 3.)], g.stroke));
        for i in 0..3 {
            let x = -3. + i as f32 * 3.;
            p.line_segment([pt(x, -x - 3.), pt(x + 1.5, -x - 1.5)], g.stroke);
        }
    } else {
        let d = if icon == Icon::LongPosition { 1. } else { -1. };
        for (x, y, w, h) in [(-5., -5., 10., 6.), (-3., 1., 6., 4.)] {
            p.rect_stroke(Rect::from_two_pos(pt(x, y * d), pt(x + w, (y + h) * d)), 0., g.stroke, StrokeKind::Middle);
        }
        p.add(Shape::line(vec![pt(-2., -d), pt(0., -3. * d), pt(2., -d)], g.stroke));
    }
}
