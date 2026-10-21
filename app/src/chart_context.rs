//! Context derived only from loaded bars, never fabricated exchange prices.
use crate::theme::{self, mono, muted};
use egui::{Align2, Rect, Stroke, Ui, pos2, vec2};
use serde::{Deserialize, Serialize};
use tr_chart::{
    scale,
    viewport::{PriceScale, Viewport},
};
use tr_core::{
    Bar, Timeframe,
    session::{Session, Zone},
};
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub countdown: bool,
    pub previous: bool,
    pub range: bool,
    pub extended: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self { countdown: true, previous: true, range: false, extended: true }
    }
}
pub fn session_levels(bars: &[Bar], tf: Timeframe, us: bool) -> Option<(Option<f64>, f64, f64)> {
    let date = |bar: &Bar| {
        if us && tf.is_intraday() { Zone::NewYork.local(bar.ts).date() } else { Zone::Utc.local(bar.ts).date() }
    };
    let day = date(bars.last()?);
    let previous = bars
        .iter()
        .rev()
        .find(|b| date(b) < day && (!us || !tf.is_intraday() || Session::UsRegular.contains(b.ts)))
        .map(|b| b.close);
    let mut high = f64::NEG_INFINITY;
    let mut low = f64::INFINITY;
    for bar in bars.iter().rev().take_while(|b| date(b) == day) {
        high = high.max(bar.high);
        low = low.min(bar.low);
    }
    Some((previous, high, low))
}
impl Settings {
    pub fn menu(&mut self, ui: &mut Ui) {
        ui.set_max_width(theme::dp(280.));
        ui.checkbox(&mut self.countdown, "Countdown to bar close");
        ui.checkbox(&mut self.previous, "Previous loaded session close");
        ui.checkbox(&mut self.range, "Loaded session high / low");
        ui.checkbox(&mut self.extended, "Shade available US pre/post-market bars");
        ui.separator();
        crate::chart_view::menu_note(ui, "From loaded bars; missing extended hours are not filled in.");
    }
    pub fn shade(&self, ui: &Ui, plot: Rect, v: &Viewport, bars: &[Bar], tf: Timeframe, us: bool) {
        if !self.extended || !tf.is_intraday() || !us {
            return;
        }
        let (lo, hi) = v.visible_range(bars.len());
        let p = ui.painter_at(plot);
        for (i, b) in bars.iter().enumerate().take(hi).skip(lo) {
            if Session::UsExtended.contains(b.ts) && !Session::UsRegular.contains(b.ts) {
                let x = plot.left() + v.x_of(i as f64) as f32;
                p.rect_filled(
                    Rect::from_min_size(pos2(x, plot.top()), vec2(v.bar_px as f32, plot.height())),
                    0.,
                    theme::series(3).gamma_multiply(0.07),
                );
            }
        }
    }
    pub fn paint(&self, ui: &Ui, plot: Rect, s: &PriceScale, bars: &[Bar], scope: (Timeframe, bool, Session, bool)) {
        let (tf, us, session, live_time_bars) = scope;
        let p = ui.painter_at(plot);
        if let Some((previous, high, low)) = session_levels(bars, tf, us) {
            let mut levels = Vec::new();
            if self.previous
                && let Some(v) = previous
            {
                levels.push(("Prior close", v));
            }
            if self.range {
                levels.extend([("Session high", high), ("Session low", low)]);
            }
            let mut labels = Vec::new();
            for (name, price) in levels {
                if !price.is_finite() || (s.log && price <= 0.) {
                    continue;
                }
                let y = plot.top() + s.y_of(price) as f32;
                if plot.y_range().contains(y) {
                    p.hline(plot.x_range(), y, Stroke::new(0.5, muted().gamma_multiply(0.6)));
                    labels.push((y, name, price));
                }
            }
            let mut occupied = Vec::new();
            for (y, name, price) in labels {
                let y = label_y(y - 2., &mut occupied);
                crate::chart_view::label_text(
                    &p,
                    pos2(plot.right() - 6., y),
                    Align2::RIGHT_BOTTOM,
                    format!("{name} {}", scale::format_price(price, 2)),
                    mono(9.5),
                    muted(),
                );
            }
        }
        if self.countdown
            && live_time_bars
            && let Some(last) = bars.last()
        {
            let now = chrono::Utc::now().timestamp();
            let end = session.interval(tf, last.ts).1;
            let label = if now < last.ts {
                "Future bar".into()
            } else if now >= end {
                "Bar closed".into()
            } else {
                let n = end - now;
                format!("Closes in {:02}:{:02}:{:02}", n / 3600, n % 3600 / 60, n % 60)
            };
            crate::chart_view::label_text(
                &p,
                plot.right_bottom() + vec2(-6., -4.),
                Align2::RIGHT_BOTTOM,
                label,
                mono(10.),
                muted(),
            );
            ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
        }
    }
}
fn label_y(mut y: f32, occupied: &mut Vec<f32>) -> f32 {
    while let Some(other) = occupied.iter().find(|other| (y - **other).abs() < 13.) {
        y = other + 13.;
    }
    occupied.push(y);
    y
}

#[cfg(test)]
mod tests {
    #[test]
    fn level_labels_nudge_down_past_all_collisions() {
        let mut occupied = Vec::new();
        assert_eq!(super::label_y(40., &mut occupied), 40.);
        assert_eq!(super::label_y(66., &mut occupied), 66.);
        assert_eq!(super::label_y(41., &mut occupied), 53.);
        assert_eq!(super::label_y(35., &mut occupied), 79.);
        assert_eq!(super::label_y(10., &mut occupied), 10.);
    }

    use super::*;
    #[test]
    fn countdown_only_renders_for_live_time_bars() {
        let ctx = egui::Context::default();
        let bars = [Bar::new(86400, 10., 12., 9., 11., 1.)];
        for live in [false, true] {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                let plot = Rect::from_min_size(pos2(0., 0.), vec2(500., 300.));
                Settings::default().paint(
                    ui,
                    plot,
                    &PriceScale::new(9., 13., false, 300.),
                    &bars,
                    (Timeframe::D1, false, Session::Provider, live),
                );
            });
            output.textures_delta.clear();
            let closed = output.shapes.iter().any(|shape| {
                matches!(
                    &shape.shape, egui::Shape::Text(text) if text.galley.job.text == "Bar closed"
                )
            });
            assert_eq!(closed, live);
        }
    }

    #[test]
    fn levels_use_loaded_dates_and_do_not_reuse_todays_close_as_previous() {
        let b = vec![
            Bar::new(86400, 10., 12., 9., 11., 1.),
            Bar::new(172800, 11., 15., 10., 14., 1.),
            Bar::new(176400, 14., 16., 13., 15., 1.),
        ];
        assert_eq!(session_levels(&b, Timeframe::H1, false), Some((Some(11.), 16., 10.)));
        assert!(session_levels(&[], Timeframe::D1, false).is_none());
    }
}
