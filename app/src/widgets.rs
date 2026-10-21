//! Native-feeling controls drawn with egui: segmented controls with a sliding pill,
//! toolbar icon buttons, rounded cards and hover washes.

use egui::{
    Align2, Color32, CornerRadius, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, epaint::RectShape, pos2,
    vec2,
};

use crate::anim;
use crate::theme::{self, ui_font};

/// macOS-style segmented control. Returns the index clicked this frame.
pub fn segmented(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    labels: &[&str],
    selected: Option<usize>,
    height: f32,
) -> Option<usize> {
    let (font, pad) = segment_style(height);
    let widths: Vec<f32> = labels
        .iter()
        .map(|l| ui.painter().layout_no_wrap((*l).to_string(), font.clone(), Color32::WHITE).size().x + pad)
        .collect();
    let total: f32 = widths.iter().sum::<f32>() + 4.0;
    let (rect, _) = ui.allocate_exact_size(vec2(total, height), Sense::hover());
    let id = ui.id().with(id_salt);
    let p = ui.painter_at(rect.expand(1.0));
    p.rect_filled(rect, theme::CONTROL_RADIUS, theme::control());
    let mut x = rect.left() + 2.0;
    let mut lefts = Vec::with_capacity(labels.len());
    for w in &widths {
        lefts.push(x);
        x += w;
    }
    // Sliding selection pill.
    if let Some(sel) = selected.filter(|&s| s < labels.len()) {
        let px = anim::spring(ui.ctx(), id.with("pill-x"), lefts[sel]);
        let pw = anim::spring(ui.ctx(), id.with("pill-w"), widths[sel]);
        let pill = Rect::from_min_size(pos2(px, rect.top() + 2.0), vec2(pw, height - 4.0));
        let fill = if theme::palette().dark { theme::control_hover() } else { Color32::WHITE };
        p.add(RectShape::filled(
            pill.translate(vec2(0.0, 0.5)),
            CornerRadius::same(theme::CONTROL_RADIUS - 1),
            Color32::from_black_alpha(25),
        ));
        p.rect_filled(pill, theme::CONTROL_RADIUS - 1, fill);
    }
    let mut clicked = None;
    for (i, label) in labels.iter().enumerate() {
        let r = Rect::from_min_size(pos2(lefts[i], rect.top()), vec2(widths[i], height));
        let resp = ui.interact(r, id.with(i), Sense::click());
        let active = selected == Some(i);
        let color = if active || resp.hovered() { theme::text() } else { theme::muted() };
        p.text(r.center(), Align2::CENTER_CENTER, *label, font.clone(), color);
        if resp.clicked() {
            clicked = Some(i);
        }
        // Hairline separators between unselected neighbours.
        if i > 0 && selected != Some(i) && selected != Some(i - 1) {
            p.vline(r.left(), (r.top() + 5.0)..=(r.bottom() - 5.0), Stroke::new(1.0, theme::border()));
        }
    }
    clicked
}

/// Font and per-segment padding: compact (toolbar-height) controls are tighter.
fn segment_style(height: f32) -> (egui::FontId, f32) {
    if height <= 20.0 { (ui_font(11.0), 12.0) } else { (ui_font(11.5), 16.0) }
}

/// Width [`segmented`] will take for `labels` at `height`.
pub fn segmented_width(ui: &Ui, labels: &[&str], height: f32) -> f32 {
    let (font, pad) = segment_style(height);
    labels
        .iter()
        .map(|l| ui.painter().layout_no_wrap((*l).to_string(), font.clone(), Color32::WHITE).size().x + pad)
        .sum::<f32>()
        + 4.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Sidebar,
    /// The sidebar glyph mirrored, for a sidebar on the right.
    SidebarRight,
    Refresh,
    Bell,
    Gauge,
    Search,
    Globe,
    Curve,
    People,
    List,
    Star,
    Grid,
    Chart,
    Plus,
    More,
    Gear,
    ArrowUp,
    Close,
    Play,
    Pause,
    StepBack,
    StepForward,
    Crosshair,
    Trendline,
    HorizontalRay,
    ExtendedLine,
    InfoLine,
    RegressionTrend,
    FibChannel,
    FibTimeZone,
    Ray,
    HorizontalLine,
    VerticalLine,
    FibRetracement,
    FibExtension,
    Channel,
    Pitchfork,
    Rectangle,
    Arrow,
    ArrowMarkUp,
    ArrowMarkDown,
    Brush,
    Circle,
    Path,
    PriceNote,
    Callout,
    FlagMark,
    Text,
    LongPosition,
    ShortPosition,
    Xabcd,
    Abcd,
    HeadAndShoulders,
    TrianglePattern,
    ElliottImpulse,
    ElliottCorrection,
    Measure,
    PriceRange,
    DateRange,
    DatePriceRange,
    AnchoredVwap,
    FixedRangeVolumeProfile,
    Magnet,
    Lock,
    Unlock,
    EyeOff,
    Eye,
    Trash,
    Grip,
    ChevronSmall,
}

/// Original measure-family glyphs on a 12-unit grid.
fn measure_volume_icon(p: &egui::Painter, r: Rect, icon: Icon, color: Color32) {
    let unit = r.width().min(r.height()) / 12.;
    let pt = |x: f32, y: f32| r.center() + vec2((x - 6.) * unit, (y - 6.) * unit);
    let stroke = Stroke::new(theme::dp(1.4), color);
    if icon == Icon::AnchoredVwap {
        p.circle_stroke(pt(2., 9.), unit, stroke);
        p.add(Shape::line(vec![pt(2., 8.), pt(4., 5.), pt(6., 6.), pt(8., 3.), pt(11., 2.)], stroke));
    } else if icon == Icon::FixedRangeVolumeProfile {
        p.line_segment([pt(2., 1.), pt(2., 11.)], stroke);
        for (y, end) in [(2., 6.), (4., 9.), (6., 11.), (8., 7.), (10., 5.)] {
            p.line_segment([pt(2., y), pt(end, y)], stroke);
        }
    } else {
        if icon != Icon::DateRange {
            p.line_segment([pt(3., 1.), pt(3., 11.)], stroke);
            for y in [1., 11.] {
                p.line_segment([pt(1., y), pt(5., y)], stroke);
            }
        }
        if icon != Icon::PriceRange {
            p.line_segment([pt(1., 8.), pt(11., 8.)], stroke);
            for x in [1., 11.] {
                p.line_segment([pt(x, 6.), pt(x, 10.)], stroke);
            }
        }
        if icon == Icon::DatePriceRange {
            p.line_segment([pt(6., 2.), pt(10., 2.)], stroke);
            p.line_segment([pt(10., 2.), pt(10., 5.)], stroke);
        }
    }
}

// Original annotation glyphs, constructed on a twelve-unit grid.
fn annotation_point(r: egui::Rect, x: f32, y: f32) -> egui::Pos2 {
    r.center() + egui::vec2(x - 6., y - 6.) * (r.width().min(r.height()) / 16.)
}
fn arrow_icon(p: &egui::Painter, r: egui::Rect, icon: Icon, color: egui::Color32) {
    let pt = |x, y| annotation_point(r, x, y);
    let s = egui::Stroke::new(theme::dp(1.4), color);
    if icon == Icon::Arrow {
        p.line_segment([pt(1., 11.), pt(11., 1.)], s);
        p.add(egui::Shape::line(vec![pt(5., 1.), pt(11., 1.), pt(11., 7.)], s));
    } else {
        let (tip, base) = if icon == Icon::ArrowMarkUp { (2., 10.) } else { (10., 2.) };
        p.add(egui::Shape::convex_polygon(vec![pt(6., tip), pt(1., base), pt(11., base)], color, s));
    }
}
fn brush_icon(p: &egui::Painter, r: egui::Rect, color: egui::Color32) {
    let pt = |x, y| annotation_point(r, x, y);
    p.add(egui::Shape::line(
        vec![pt(1., 10.), pt(3., 3.), pt(5., 2.), pt(6., 8.), pt(8., 10.), pt(11., 4.)],
        egui::Stroke::new(theme::dp(1.4), color),
    ));
}
fn shape_icon(p: &egui::Painter, r: egui::Rect, icon: Icon, color: egui::Color32) {
    let pt = |x, y| annotation_point(r, x, y);
    let s = egui::Stroke::new(theme::dp(1.4), color);
    if icon == Icon::Circle {
        p.circle_stroke(pt(6., 6.), r.width().min(r.height()) * 5. / 16., s);
    } else {
        p.add(egui::Shape::line(vec![pt(1., 10.), pt(4., 2.), pt(8., 9.), pt(11., 3.)], s));
        for (x, y) in [(1., 10.), (4., 2.), (8., 9.), (11., 3.)] {
            p.circle_stroke(pt(x, y), 1.6 * r.width().min(r.height()) / 16., s);
        }
    }
}
fn note_icon(p: &egui::Painter, r: egui::Rect, icon: Icon, color: egui::Color32) {
    let pt = |x, y| annotation_point(r, x, y);
    let s = egui::Stroke::new(theme::dp(1.4), color);
    match icon {
        Icon::FlagMark => {
            p.line_segment([pt(3., 12.), pt(3., 1.)], s);
            p.add(egui::Shape::line(vec![pt(3., 1.), pt(11., 1.), pt(9., 6.), pt(3., 6.)], s));
        }
        Icon::PriceNote => {
            p.add(egui::Shape::line(vec![pt(1., 10.), pt(5., 4.), pt(11., 4.), pt(11., 9.), pt(5., 9.)], s));
            p.line_segment([pt(7., 6.), pt(9., 6.)], s);
        }
        _ => {
            p.add(egui::Shape::line(
                vec![pt(1., 11.), pt(4., 7.), pt(2., 7.), pt(2., 1.), pt(11., 1.), pt(11., 7.), pt(6., 7.)],
                s,
            ));
        }
    }
}

pub fn tool_icon(tool: tr_chart::drawings::Tool) -> Icon {
    use tr_chart::drawings::Tool;
    match tool {
        Tool::Trendline => Icon::Trendline,
        Tool::HorizontalRay => Icon::HorizontalRay,
        Tool::ExtendedLine => Icon::ExtendedLine,
        Tool::InfoLine => Icon::InfoLine,
        Tool::RegressionTrend => Icon::RegressionTrend,
        Tool::FibChannel => Icon::FibChannel,
        Tool::FibTimeZone => Icon::FibTimeZone,
        Tool::Ray => Icon::Ray,
        Tool::Horizontal => Icon::HorizontalLine,
        Tool::Vertical => Icon::VerticalLine,
        Tool::Fib => Icon::FibRetracement,
        Tool::FibExtension => Icon::FibExtension,
        Tool::Channel => Icon::Channel,
        Tool::Pitchfork => Icon::Pitchfork,
        Tool::Rectangle => Icon::Rectangle,
        Tool::Text => Icon::Text,
        Tool::Arrow => Icon::Arrow,
        Tool::ArrowMarkUp => Icon::ArrowMarkUp,
        Tool::ArrowMarkDown => Icon::ArrowMarkDown,
        Tool::Brush => Icon::Brush,
        Tool::Circle => Icon::Circle,
        Tool::Path => Icon::Path,
        Tool::PriceNote => Icon::PriceNote,
        Tool::Callout => Icon::Callout,
        Tool::FlagMark => Icon::FlagMark,
        Tool::LongPosition => Icon::LongPosition,
        Tool::ShortPosition => Icon::ShortPosition,
        Tool::Measure => Icon::Measure,
        Tool::Xabcd => Icon::Xabcd,
        Tool::Abcd => Icon::Abcd,
        Tool::HeadAndShoulders => Icon::HeadAndShoulders,
        Tool::TrianglePattern => Icon::TrianglePattern,
        Tool::ElliottImpulse => Icon::ElliottImpulse,
        Tool::ElliottCorrection => Icon::ElliottCorrection,
        Tool::PriceRange => Icon::PriceRange,
        Tool::DateRange => Icon::DateRange,
        Tool::DatePriceRange => Icon::DatePriceRange,
        Tool::AnchoredVwap => Icon::AnchoredVwap,
        Tool::FixedRangeVolumeProfile => Icon::FixedRangeVolumeProfile,
    }
}

#[cfg(test)]
mod drawing_icon_tests {
    use super::*;

    #[test]
    fn every_tool_has_a_distinct_nonempty_line_icon() {
        let ctx = egui::Context::default();
        let mut signatures = Vec::new();
        for tool in tr_chart::drawings::Tool::ALL {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                let painter =
                    ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("icons")));
                draw_icon(&painter, Rect::from_min_size(Pos2::ZERO, vec2(24., 24.)), tool_icon(tool), theme::text());
            });
            assert!(!output.shapes.is_empty(), "{tool:?}");
            assert!(output.shapes.iter().any(|s| s.shape.visual_bounding_rect().is_positive()));
            output.textures_delta.clear();
            let signature = format!("{:?}", output.shapes);
            assert!(!signatures.contains(&signature), "Duplicate geometry for {tool:?}");
            signatures.push(signature);
        }
    }
}

/// Original pattern glyphs on a centred twelve-unit grid.
fn pattern_icon(p: &egui::Painter, r: Rect, icon: Icon, color: Color32) {
    let points: &[[f32; 2]] = match icon {
        Icon::Xabcd => &[[-6., 4.], [-3., -5.], [0., 2.], [3., -2.], [6., 5.]],
        Icon::Abcd => &[[-6., 5.], [-2., -4.], [2., 2.], [6., -5.]],
        Icon::HeadAndShoulders => &[[-6., 5.], [-4., -1.], [-2., 2.], [0., -6.], [2., 2.], [4., -1.], [6., 5.]],
        Icon::TrianglePattern => &[[-6., -5.], [-6., 5.], [6., 0.]],
        Icon::ElliottImpulse => &[[-6., 6.], [-4., 1.], [-2., 3.], [0., -3.], [3., -1.], [6., -6.]],
        Icon::ElliottCorrection => &[[-6., -5.], [-2., 3.], [2., -2.], [6., 5.]],
        _ => unreachable!("Non-pattern icon"),
    };
    let unit = r.width().min(r.height()) / 16.;
    let point = |a: [f32; 2]| r.center() + vec2(a[0] * unit, a[1] * unit);
    let stroke = Stroke::new(theme::dp(1.4), color);
    let radius = 1.6 * unit;
    // Stop each segment at the open anchors rather than drawing through their centres.
    for pair in points.windows(2) {
        let a = point(pair[0]);
        let b = point(pair[1]);
        let offset = (b - a).normalized() * radius;
        p.line_segment([a + offset, b - offset], stroke);
    }
    if icon == Icon::Xabcd {
        p.extend(Shape::dashed_line(&[point(points[0]), point(points[4])], stroke, 2. * unit, 2. * unit));
    }
    if icon == Icon::HeadAndShoulders {
        p.line_segment([point([-6., 3.]), point([6., 3.])], stroke);
    }
    if icon == Icon::TrianglePattern {
        p.line_segment([point(points[0]), point([6., 0.])], stroke);
        p.line_segment([point(points[1]), point([6., 0.])], stroke);
    }
    for &anchor in points {
        p.circle_stroke(point(anchor), radius, stroke);
    }
}

/// Multi-point glyphs share open anchors and scale-aware strokes.
fn anchored_tool_icon(p: &egui::Painter, r: Rect, icon: Icon, color: Color32) -> bool {
    let unit = r.width().min(r.height()) / 16.;
    let pt = |a: [f32; 2]| r.center() + vec2(a[0], a[1]) * unit;
    let stroke = Stroke::new(theme::dp(1.4), color);
    let anchors: &[[f32; 2]] = match icon {
        Icon::Trendline | Icon::Ray | Icon::Channel | Icon::RegressionTrend => &[[-5., 5.], [5., -5.]],
        Icon::ExtendedLine => &[[-2., 2.], [2., -2.]],
        Icon::InfoLine => &[[-5., 5.], [2., -2.]],
        Icon::Pitchfork => &[[-5., 5.], [-4., -2.], [2., 4.]],
        Icon::FibRetracement => &[[-5., 5.], [5., -5.]],
        Icon::FibExtension => &[[-5., 5.], [-1., -5.], [4., 2.]],
        Icon::FibChannel => &[[-5., 5.], [5., -2.], [-5., 0.]],
        Icon::FibTimeZone => &[[-5., 3.], [-2., 3.]],
        Icon::PriceRange => &[[-3., -5.], [-3., 5.]],
        Icon::DateRange => &[[-5., 2.], [5., 2.]],
        Icon::DatePriceRange => &[[-5., 5.], [5., -5.]],
        Icon::Path => &[[-5., 4.], [-2., -4.], [2., 3.], [5., -3.]],
        _ => return false,
    };
    let radius = 1.6 * unit;
    for pair in anchors.windows(2) {
        let a = pt(pair[0]);
        let b = pt(pair[1]);
        let offset = (b - a).normalized() * radius;
        p.line_segment([a + offset, b - offset], stroke);
    }
    let segment = |a, b| {
        p.line_segment([pt(a), pt(b)], stroke);
    };
    match icon {
        Icon::Ray | Icon::ExtendedLine => {
            segment([3., -3.], [6., -6.]);
            if icon == Icon::ExtendedLine {
                segment([-6., 6.], [-3., 3.]);
            }
        }
        Icon::Channel | Icon::RegressionTrend => {
            segment([-5., 0.], [0., -5.]);
            if icon == Icon::RegressionTrend {
                segment([0., 5.], [5., 0.]);
            }
        }
        Icon::InfoLine => {
            p.rect_stroke(Rect::from_min_max(pt([0., -6.]), pt([6., -2.])), 0., stroke, StrokeKind::Middle);
            segment([2., -4.], [4., -4.]);
        }
        Icon::Pitchfork => {
            for (a, b) in [([-4., -2.], [0., -6.]), ([2., 4.], [6., 0.]), ([-1., 1.], [5., -5.])] {
                segment(a, b);
            }
        }
        Icon::FibRetracement | Icon::FibExtension => {
            for y in [-5., -2., 1., 5.] {
                segment([-3., y], [6., y]);
            }
        }
        Icon::FibChannel => {
            for offset in [-4., -2., 2.] {
                segment([-5., 3. + offset], [5., -4. + offset]);
            }
        }
        Icon::FibTimeZone => {
            for x in [-5., -2., 1., 5.] {
                segment([x, -5.], [x, 1.]);
            }
        }
        Icon::PriceRange => {
            segment([-5., -5.], [-1., -5.]);
            segment([-5., 5.], [-1., 5.]);
        }
        Icon::DateRange => {
            segment([-5., 0.], [-5., 4.]);
            segment([5., 0.], [5., 4.]);
        }
        Icon::DatePriceRange => {
            segment([-5., 2.], [-5., -5.]);
            segment([-5., -5.], [2., -5.]);
            segment([-2., 5.], [5., 5.]);
            segment([5., 5.], [5., -2.]);
        }
        _ => {}
    }
    for &anchor in anchors {
        p.circle_stroke(pt(anchor), radius, stroke);
    }
    true
}

/// Favourite glyph uses the same geometry in its quiet and selected states.
pub fn favorite_star(p: &egui::Painter, r: Rect, filled: bool) {
    let color = if filled { theme::accent() } else { theme::muted() };
    let unit = r.width().min(r.height()) / 16.;
    let points: Vec<_> = (0..10)
        .map(|i| {
            let angle = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.;
            let radius = if i % 2 == 0 { 6. } else { 2.6 };
            r.center() + vec2(angle.cos(), angle.sin()) * radius * unit
        })
        .collect();
    if filled {
        // Five convex wedges fill the concave star without invalid convex tessellation.
        for i in (0..10).step_by(2) {
            p.add(Shape::convex_polygon(
                vec![r.center(), points[(i + 9) % 10], points[i], points[(i + 1) % 10]],
                color,
                Stroke::NONE,
            ));
        }
    } else {
        let mut outline = points;
        outline.push(outline[0]);
        p.add(Shape::line(outline, Stroke::new(theme::dp(1.4), color)));
    }
}

/// Draw a simple line icon centred in `r`.
pub fn draw_icon(p: &egui::Painter, r: Rect, icon: Icon, color: Color32) {
    // Tool glyphs use a twelve-unit grid; leave room for the open anchor rings.
    let is_tool = tr_chart::drawings::Tool::ALL.iter().any(|&tool| tool_icon(tool) == icon);
    let r = if is_tool { Rect::from_center_size(r.center(), r.size() * (16. / 15.5)) } else { r };
    if anchored_tool_icon(p, r, icon, color) {
        return;
    }
    let s = Stroke::new(theme::dp(1.4), color);
    let c = r.center();
    let u = r.height().min(r.width()) / 16.0;
    let pt = |x: f32, y: f32| pos2(c.x + x * u, c.y + y * u);
    match icon {
        Icon::Crosshair => {
            for d in [-1., 1.] {
                p.line_segment([pt(d * 2., 0.), pt(d * 6., 0.)], s);
                p.line_segment([pt(0., d * 2.), pt(0., d * 6.)], s);
            }
        }
        Icon::HorizontalRay
        | Icon::ExtendedLine
        | Icon::InfoLine
        | Icon::RegressionTrend
        | Icon::Trendline
        | Icon::Ray
        | Icon::Channel
        | Icon::HorizontalLine
        | Icon::VerticalLine
        | Icon::Pitchfork => crate::drawing_paint::lines(p, r, icon, color),
        Icon::FibRetracement | Icon::FibExtension | Icon::FibChannel | Icon::FibTimeZone => {
            crate::drawing_paint::fib(p, r, icon, color)
        }
        Icon::Arrow | Icon::ArrowMarkUp | Icon::ArrowMarkDown => arrow_icon(p, r, icon, color),
        Icon::Brush => brush_icon(p, r, color),
        Icon::Circle | Icon::Path => shape_icon(p, r, icon, color),
        Icon::PriceNote | Icon::Callout | Icon::FlagMark => note_icon(p, r, icon, color),
        Icon::Rectangle => crate::drawing_paint::shapes(p, r, color),
        Icon::Text => crate::drawing_paint::text(p, r, color),
        Icon::LongPosition | Icon::ShortPosition | Icon::Measure => crate::drawing_paint::measure(p, r, icon, color),
        Icon::Xabcd
        | Icon::Abcd
        | Icon::HeadAndShoulders
        | Icon::TrianglePattern
        | Icon::ElliottImpulse
        | Icon::ElliottCorrection => pattern_icon(p, r, icon, color),
        Icon::PriceRange
        | Icon::DateRange
        | Icon::DatePriceRange
        | Icon::AnchoredVwap
        | Icon::FixedRangeVolumeProfile => measure_volume_icon(p, r, icon, color),
        Icon::Magnet => {
            p.add(Shape::line(vec![pt(-5., -5.), pt(-5., 2.), pt(-3., 5.), pt(3., 5.), pt(5., 2.), pt(5., -5.)], s));
            p.line_segment([pt(-5., -2.), pt(-2., -2.)], s);
            p.line_segment([pt(2., -2.), pt(5., -2.)], s);
        }
        Icon::Lock | Icon::Unlock => {
            p.rect_stroke(Rect::from_min_max(pt(-5., -1.), pt(5., 6.)), 1.5 * u, s, StrokeKind::Middle);
            let offset = if icon == Icon::Unlock { 4. } else { 0. };
            p.add(Shape::line(
                vec![
                    pt(-3. + offset, -1.),
                    pt(-3. + offset, -4.),
                    pt(-1. + offset, -6.),
                    pt(1. + offset, -6.),
                    pt(3. + offset, -4.),
                    pt(3. + offset, -1.),
                ],
                s,
            ));
            p.circle_stroke(pt(0., 2.), u, s);
            p.line_segment([pt(0., 3.), pt(0., 4.5)], s);
        }
        Icon::Eye | Icon::EyeOff => {
            p.add(Shape::line(
                vec![pt(-7., 0.), pt(-3., -4.), pt(3., -4.), pt(7., 0.), pt(3., 4.), pt(-3., 4.), pt(-7., 0.)],
                s,
            ));
            p.circle_stroke(c, 2. * u, s);
            if icon == Icon::EyeOff {
                p.line_segment([pt(-6., -6.), pt(6., 6.)], s);
            }
        }
        Icon::Trash => {
            p.add(Shape::line(vec![pt(-4., -3.), pt(-3., 6.), pt(3., 6.), pt(4., -3.)], s));
            p.line_segment([pt(-6., -4.), pt(6., -4.)], s);
            p.line_segment([pt(-2., -6.), pt(2., -6.)], s);
            for x in [-1.5, 1.5] {
                p.line_segment([pt(x, -1.), pt(x, 4.)], s);
            }
        }
        Icon::Grip => {
            for x in [-2., 2.] {
                for y in [-4., 0., 4.] {
                    p.circle_filled(pt(x, y), u, color);
                }
            }
        }
        Icon::ChevronSmall => {
            p.add(Shape::convex_polygon(vec![pt(-3., 3.), pt(3., -3.), pt(3., 3.)], color, Stroke::NONE));
        }
        Icon::Sidebar | Icon::SidebarRight => {
            let b = Rect::from_center_size(c, vec2(14.0 * u, 11.0 * u));
            p.rect_stroke(b, 2.0, s, StrokeKind::Middle);
            let x = if icon == Icon::Sidebar { b.left() + 4.5 * u } else { b.right() - 4.5 * u };
            p.vline(x, b.y_range(), s);
        }
        Icon::Refresh => {
            let pts: Vec<Pos2> = (0..=20)
                .map(|i| {
                    let a = 0.6 + i as f32 / 20.0 * 4.8;
                    pt(5.5 * a.cos(), 5.5 * a.sin())
                })
                .collect();
            let end = *pts.last().unwrap();
            p.add(Shape::line(pts, s));
            p.line_segment([end, end + vec2(2.5 * u, -u)], s);
            p.line_segment([end, end + vec2(0.5 * u, -3.0 * u)], s);
        }
        Icon::Bell => {
            let pts: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = std::f32::consts::PI * (1.0 + i as f32 / 16.0);
                    pt(4.5 * a.cos(), -1.0 + 4.5 * a.sin())
                })
                .collect();
            p.add(Shape::line(pts, s));
            p.line_segment([pt(-4.5, -1.0), pt(-5.5, 3.5)], s);
            p.line_segment([pt(4.5, -1.0), pt(5.5, 3.5)], s);
            p.line_segment([pt(-6.0, 3.5), pt(6.0, 3.5)], s);
            p.circle_filled(pt(0.0, 5.5), 1.2 * u, color);
        }
        Icon::Gauge => {
            let pts: Vec<Pos2> = (0..=18)
                .map(|i| {
                    let a = std::f32::consts::PI * (1.0 + i as f32 / 18.0);
                    pt(6.0 * a.cos(), 2.5 + 6.0 * a.sin())
                })
                .collect();
            p.add(Shape::line(pts, s));
            p.line_segment([pt(0.0, 2.5), pt(3.0, -1.5)], s);
        }
        Icon::Search => {
            p.circle_stroke(pt(-1.5, -1.5), 4.5 * u, s);
            p.line_segment([pt(1.8, 1.8), pt(5.5, 5.5)], Stroke::new(1.8, color));
        }
        Icon::Globe => {
            p.circle_stroke(c, 6.0 * u, s);
            p.line_segment([pt(-6.0, 0.0), pt(6.0, 0.0)], s);
            let pts: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = std::f32::consts::PI * (-0.5 + i as f32 / 16.0);
                    pt(2.6 * a.cos(), 6.0 * a.sin())
                })
                .collect();
            p.add(Shape::line(pts.clone(), s));
            p.add(Shape::line(pts.iter().map(|q| pos2(2.0 * c.x - q.x, q.y)).collect(), s));
        }
        Icon::Curve => {
            let pts: Vec<Pos2> = (0..=12)
                .map(|i| {
                    let x = -6.0 + i as f32;
                    pt(x, 4.0 - 7.5 * (1.0 - (-(x + 6.0) / 4.0).exp()))
                })
                .collect();
            p.add(Shape::line(pts, s));
        }
        Icon::People => {
            p.circle_stroke(pt(-2.0, -2.5), 2.5 * u, s);
            p.circle_stroke(pt(3.5, -1.5), 2.0 * u, s);
            p.add(Shape::line(vec![pt(-6.5, 5.5), pt(-5.0, 1.5), pt(1.0, 1.5), pt(2.5, 5.5)], s));
        }
        Icon::List => {
            for y in [-4.0, 0.0, 4.0] {
                p.circle_filled(pt(-5.0, y), 1.0 * u, color);
                p.line_segment([pt(-2.5, y), pt(6.0, y)], s);
            }
        }
        Icon::Grid => {
            for (x, y) in [(-3.5, -3.5), (3.5, -3.5), (-3.5, 3.5), (3.5, 3.5)] {
                p.rect_stroke(Rect::from_center_size(pt(x, y), vec2(5.0 * u, 5.0 * u)), 1.5, s, StrokeKind::Middle);
            }
        }
        Icon::Chart => {
            p.add(Shape::line(vec![pt(-6.0, 4.0), pt(-2.0, -1.0), pt(1.0, 2.0), pt(6.0, -4.5)], s));
        }
        Icon::Plus => {
            p.line_segment([pt(-5.0, 0.0), pt(5.0, 0.0)], s);
            p.line_segment([pt(0.0, -5.0), pt(0.0, 5.0)], s);
        }
        Icon::More => {
            for x in [-4.5, 0.0, 4.5] {
                p.circle_filled(pt(x, 0.0), 1.3 * u, color);
            }
        }
        Icon::Gear => {
            p.circle_stroke(c, 2.5 * u, s);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                p.line_segment(
                    [pt(4.2 * a.cos(), 4.2 * a.sin()), pt(6.2 * a.cos(), 6.2 * a.sin())],
                    Stroke::new(1.8, color),
                );
            }
            p.circle_stroke(c, 4.6 * u, s);
        }
        Icon::ArrowUp => {
            p.line_segment([pt(0.0, -5.0), pt(0.0, 5.0)], s);
            p.add(Shape::line(vec![pt(-4.5, -0.5), pt(0.0, -5.0), pt(4.5, -0.5)], s));
        }
        Icon::Play => {
            p.add(Shape::convex_polygon(vec![pt(-3., -5.), pt(5., 0.), pt(-3., 5.)], color, Stroke::NONE));
        }
        Icon::Pause => {
            p.line_segment([pt(-2.5, -5.), pt(-2.5, 5.)], Stroke::new(2.8, color));
            p.line_segment([pt(2.5, -5.), pt(2.5, 5.)], Stroke::new(2.8, color));
        }
        Icon::StepBack | Icon::StepForward => {
            let d = if icon == Icon::StepBack { -1. } else { 1. };
            p.add(Shape::convex_polygon(vec![pt(-4. * d, -5.), pt(3. * d, 0.), pt(-4. * d, 5.)], color, Stroke::NONE));
            p.line_segment([pt(5. * d, -5.), pt(5. * d, 5.)], s);
        }
        Icon::Close => {
            p.line_segment([pt(-4.5, -4.5), pt(4.5, 4.5)], s);
            p.line_segment([pt(4.5, -4.5), pt(-4.5, 4.5)], s);
        }
        Icon::Star => {
            let pts: Vec<Pos2> = (0..=10)
                .map(|i| {
                    let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
                    let rad = if i % 2 == 0 { 6.0 } else { 2.6 };
                    pt(rad * a.cos(), rad * a.sin())
                })
                .collect();
            p.add(Shape::line(pts, s));
        }
    }
}

/// Borderless toolbar button with an animated hover/press wash.
pub fn icon_button(ui: &mut Ui, icon: Icon, tooltip: &str, active: bool) -> Response {
    // Fill the space given (toolbars pass compact rects), up to a 28×24 button.
    let size = vec2(ui.available_width().clamp(18.0, 28.0), ui.available_height().clamp(18.0, 24.0));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let t = anim::spring_bool(ui.ctx(), resp.id.with("hover"), resp.hovered() || active);
    if t > 0.01 {
        let base = if active { theme::accent_wash() } else { theme::control_hover() };
        ui.painter().rect_filled(rect, theme::CONTROL_RADIUS, base.gamma_multiply(t));
    }
    let color = if active { theme::accent() } else { theme::text().gamma_multiply(0.85) };
    draw_icon(ui.painter(), rect.shrink(4.0), icon, color);
    resp.on_hover_text(tooltip)
}

/// Rounded card with soft shadow; the border tints with the accent colour when focused.
/// Returns the inner content rect.
pub fn card(ui: &Ui, rect: Rect, focus: f32) -> Rect {
    card_on(ui.painter(), rect, focus)
}

/// [`card`] drawn with an explicit painter (e.g. a foreground layer).
pub fn card_on(p: &egui::Painter, rect: Rect, focus: f32) -> Rect {
    let radius = CornerRadius::same(theme::CARD_RADIUS);
    let shadow_alpha = if theme::palette().dark { 90 } else { 28 };
    for (grow, a) in [(3.0, shadow_alpha / 3), (1.5, shadow_alpha / 2)] {
        p.add(RectShape::filled(rect.expand(grow).translate(vec2(0.0, 1.5)), radius, Color32::from_black_alpha(a)));
    }
    p.rect_filled(rect, radius, theme::card());
    let border = lerp_color(theme::border(), theme::accent(), focus);
    p.rect_stroke(rect, radius, Stroke::new(1.0 + focus * 0.5, border), StrokeKind::Inside);
    rect.shrink(1.0)
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

/// A selection highlight that glides between list rows. Create it before the rows (it
/// keeps a place under them) and call [`Glider::on`] with the selected row.
pub struct Glider {
    id: egui::Id,
    slot: egui::layers::ShapeIdx,
    /// Top of the list; positions are kept relative to it so scrolling does not lag.
    origin: f32,
}

impl Glider {
    pub fn new(ui: &Ui, id: egui::Id) -> Self {
        Self { id, slot: ui.painter().add(Shape::Noop), origin: ui.cursor().top() }
    }

    pub fn on(&self, ui: &Ui, row: Rect) {
        let ctx = ui.ctx();
        // A list that was just opened shows its selection in place.
        let frame = ctx.cumulative_frame_nr();
        let seen = self.id.with("frame");
        if ctx.data(|d| d.get_temp::<u64>(seen)).is_none_or(|f| f + 1 < frame) {
            ctx.data_mut(|d| d.remove::<anim::Spring>(self.id));
        }
        ctx.data_mut(|d| d.insert_temp(seen, frame));
        let y = anim::spring_with(ctx, self.id, row.top() - self.origin, 0.22, 0.86);
        let r = row.translate(vec2(0.0, self.origin + y - row.top()));
        ui.painter().set(self.slot, RectShape::filled(r, theme::CONTROL_RADIUS, theme::accent_wash()));
    }
}

/// Animated hover wash for list rows.
pub fn hover_wash(ui: &Ui, rect: Rect, resp: &Response, selected: bool) {
    let t = anim::spring_bool(ui.ctx(), resp.id.with("wash"), resp.hovered());
    if selected {
        ui.painter().rect_filled(rect, theme::CONTROL_RADIUS, theme::accent_wash());
    } else if t > 0.01 {
        ui.painter().rect_filled(rect, theme::CONTROL_RADIUS, theme::control().gamma_multiply(t * 1.6));
    }
}

/// A small action pill: label, fill and label colour.
pub type Pill<'a> = (&'a str, Color32, Color32);

/// Buy / Sell / Close pills over the right end of a table row while the pointer is on it,
/// on a backdrop so they never overlap the row's text. Returns the index clicked.
pub fn row_pills(
    ui: &mut Ui,
    row: Rect,
    id: impl std::hash::Hash + std::fmt::Debug,
    pills: &[Pill<'_>],
    tips: &[&str],
) -> Option<usize> {
    if !ui.rect_contains_pointer(row) || pills.is_empty() {
        return None;
    }
    let h = (row.height() - 6.0).clamp(14.0, 18.0);
    let font = ui_font(10.5);
    let galleys: Vec<_> =
        pills.iter().map(|(l, _, c)| ui.painter().layout_no_wrap((*l).to_string(), font.clone(), *c)).collect();
    let total: f32 = galleys.iter().map(|g| g.size().x + 14.0).sum::<f32>() + 4.0 * (pills.len() - 1) as f32;
    let strip = Rect::from_min_max(
        pos2(row.right() - total - 12.0, row.top() + 1.0),
        pos2(row.right() - 1.0, row.bottom() - 1.0),
    );
    ui.painter().rect_filled(strip, theme::CONTROL_RADIUS, theme::card());
    let mut x = strip.left() + 6.0;
    let mut clicked = None;
    let base = ui.id().with(id);
    for (i, ((_, fill, _), g)) in pills.iter().zip(galleys).enumerate() {
        let w = g.size().x + 14.0;
        let r = Rect::from_min_size(pos2(x, row.center().y - h / 2.0), vec2(w, h));
        x += w + 4.0;
        let mut resp = ui.interact(r, base.with(i), Sense::click());
        let fill = if resp.hovered() { *fill } else { fill.gamma_multiply(0.82) };
        ui.painter().rect_filled(r, h / 2.0, fill);
        ui.painter().galley(r.center() - g.size() / 2.0, g, Color32::WHITE);
        if let Some(t) = tips.get(i) {
            resp = resp.on_hover_text(*t);
        }
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    clicked
}

/// The usual Buy / Sell pills.
pub fn buy_sell_pills() -> [Pill<'static>; 2] {
    [("B", theme::up(), Color32::WHITE), ("S", theme::down(), Color32::WHITE)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_lerp() {
        assert_eq!(lerp_color(Color32::BLACK, Color32::WHITE, 0.0), Color32::BLACK);
        assert_eq!(lerp_color(Color32::BLACK, Color32::WHITE, 1.0), Color32::WHITE);
        assert_eq!(lerp_color(Color32::BLACK, Color32::WHITE, 2.0), Color32::WHITE);
    }

    #[test]
    fn segmented_reports_layout_without_panicking() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let r = segmented(ui, "tf", &["1m", "5m", "1D"], Some(2), 22.0);
            assert!(r.is_none());
            let _ = icon_button(ui, Icon::Sidebar, "Sidebar", false);
        });
        out.textures_delta.clear();
    }
}
