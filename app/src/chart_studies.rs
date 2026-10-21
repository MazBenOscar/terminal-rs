use crate::theme::{self, mono, muted};
use egui::{Align2, Rect, Stroke, Ui, pos2, vec2};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tr_chart::{
    gpu::LineRef,
    scene::line_ref,
    viewport::{PriceScale, Viewport},
};
use tr_core::{Bar, Timeframe, session::Zone};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ProfileMode {
    #[default]
    Off,
    Visible,
    Session,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub anchor: Option<i64>,
    pub profile: ProfileMode,
    pub bins: usize,
    pub baseline: Option<f64>,
    pub renko_size: f64,
}
impl Default for Settings {
    fn default() -> Self {
        Self { anchor: None, profile: ProfileMode::Off, bins: 48, baseline: None, renko_size: 1. }
    }
}
#[derive(Default)]
pub struct Cache {
    bars: Option<Arc<Vec<Bar>>>,
    anchor: Option<i64>,
    pub vwap: Option<LineRef>,
}
impl Cache {
    pub fn update(&mut self, bars: &Arc<Vec<Bar>>, anchor: Option<i64>) {
        if self.bars.as_ref().is_some_and(|old| Arc::ptr_eq(old, bars)) && self.anchor == anchor {
            return;
        }
        self.bars = Some(bars.clone());
        self.anchor = anchor;
        self.vwap = anchor.map(|anchor| line_ref(&tr_core::chart_analytics::anchored_vwap(bars, anchor)));
    }
}
impl Settings {
    pub fn menu(&mut self, ui: &mut Ui, timestamp: Option<i64>, price: Option<f64>) {
        if ui.add_enabled(timestamp.is_some(), egui::Button::new("Anchor VWAP at clicked bar")).clicked() {
            self.anchor = timestamp;
            ui.close();
        }
        if ui.button("Clear anchored VWAP").clicked() {
            self.anchor = None;
        }
        for mode in [ProfileMode::Off, ProfileMode::Visible, ProfileMode::Session] {
            ui.selectable_value(
                &mut self.profile,
                mode,
                match mode {
                    ProfileMode::Off => "Volume profile off",
                    ProfileMode::Visible => "Visible-range profile",
                    ProfileMode::Session => "Latest session profile",
                },
            );
        }
        ui.add(egui::DragValue::new(&mut self.bins).range(8..=128).prefix("Bins "));
        if ui.add_enabled(price.is_some(), egui::Button::new("Set baseline to clicked price")).clicked() {
            self.baseline = price;
        }
        if ui.button("Reset baseline to visible start").clicked() {
            self.baseline = None;
        }
        ui.label("OHLCV profile estimate; exact volume-at-price requires trade data.");
    }
    pub fn paint_profile(
        &self,
        ui: &Ui,
        plot: Rect,
        v: &Viewport,
        bars: &[Bar],
        scope: (Timeframe, bool),
        scale: &PriceScale,
    ) {
        if self.profile == ProfileMode::Off {
            return;
        }
        let (tf, us) = scope;
        let (lo, hi) = v.visible_range(bars.len());
        let selected = if self.profile == ProfileMode::Visible {
            &bars[lo.min(bars.len())..hi.min(bars.len())]
        } else {
            let date = |b: &Bar| {
                if us && tf.is_intraday() { Zone::NewYork.local(b.ts).date() } else { Zone::Utc.local(b.ts).date() }
            };
            let Some(last) = bars.last() else {
                return;
            };
            let start = bars.partition_point(|b| date(b) < date(last));
            &bars[start..]
        };
        let p = ui.painter_at(plot);
        if let Some(profile) = tr_core::chart_analytics::volume_profile(selected, self.bins) {
            let max = profile.volume.iter().copied().fold(0., f64::max);
            for (i, volume) in profile.volume.iter().enumerate() {
                let price = profile.low + profile.step * i as f64;
                if scale.log && price <= 0. {
                    continue;
                }
                let y0 = plot.top() + scale.y_of(price) as f32;
                let y1 = plot.top() + scale.y_of(price + profile.step) as f32;
                let width = plot.width() * 0.22 * (*volume / max) as f32;
                let color = if i == profile.poc {
                    theme::amber()
                } else if i >= profile.value_low && i <= profile.value_high {
                    theme::series(2)
                } else {
                    muted()
                };
                p.rect_filled(
                    Rect::from_two_pos(pos2(plot.right() - width, y0), pos2(plot.right(), y1)),
                    0.,
                    color.gamma_multiply(0.23),
                );
                if i == profile.poc {
                    p.hline(plot.x_range(), (y0 + y1) / 2., Stroke::new(0.7, color));
                }
            }
            let label = if selected.iter().any(|b| !b.volume.is_finite() || b.volume < 0.) {
                "Partial OHLCV profile estimate · POC / 70% area"
            } else {
                "OHLCV profile estimate · POC / 70% value area"
            };
            p.text(plot.right_bottom() + vec2(-6., -18.), Align2::RIGHT_BOTTOM, label, mono(9.), muted());
        } else {
            p.text(
                plot.right_bottom() + vec2(-6., -18.),
                Align2::RIGHT_BOTTOM,
                "Volume profile unavailable",
                mono(9.),
                muted(),
            );
        }
    }
}
