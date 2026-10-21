//! Chart pane: GPU-rendered price/volume/indicator panes with egui-drawn axes, legend,
//! crosshair, drawings and congressional trade markers.

use std::sync::Arc;

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2,
    vec2,
};
use tr_chart::drawings::{self, Anchor, Drawing, Shape, Tool};
use tr_chart::gpu::{ChartCallback, Frame, LineKind, LineRef, Reveal, Style};
use tr_chart::scale;
use tr_chart::scene::{self, ChartType, Overlay, PricePane, SeriesGpu};
use tr_chart::viewport::{PriceScale, Viewport};
use tr_congress::{Trade, TxType};
use tr_core::indicators::formula::{Average, BandwidthUnits, RsiMethod};
use tr_core::indicators::registry::{self, Category};
use tr_core::indicators::study::{self, Pane as StudyPane, Scale as StudyScale, Source as StudySource};
use tr_core::{AssetClass, Bar, Security, Timeframe};

use crate::chart_compare::Axis as CompareAxis;
use crate::command::{IndicatorSpec, Range};
use crate::congress_view::Action;
use crate::theme::{self, amber, amber_dim, mono, muted, text, ui_font};
use crate::widgets;
use crate::worker::{BuiltIndicator, PaneId, Request, Worker, initial_limit};

fn menu_section(ui: &mut Ui, title: &str) {
    ui.add_space(theme::dp(3.));
    ui.label(RichText::new(title).small().color(theme::muted()));
}

pub(crate) fn menu_note(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).small().color(theme::muted())).wrap());
}

pub(crate) fn label_plate(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect.expand2(vec2(2., 1.)), 2., theme::card().gamma_multiply(0.82));
}

pub(crate) fn label_text(
    painter: &egui::Painter,
    pos: Pos2,
    anchor: Align2,
    text: impl Into<String>,
    font: egui::FontId,
    color: Color32,
) {
    let galley = painter.layout_no_wrap(text.into(), font, color);
    let rect = anchor.anchor_size(pos, galley.size());
    label_plate(painter, rect);
    painter.galley(rect.min, galley, color);
}

const AXIS_W: f32 = 66.0;
const TIME_H: f32 = 16.0;
/// Fallback header row, used when the pane's tab strip has no room for the controls.
const HEADER_H: f32 = 24.0;
const RANGE_H: f32 = 18.0;
/// Height of the interval / chart-type segmented controls.
pub const CONTROL_H: f32 = 20.0;
const DEFAULT_VISIBLE: usize = 160;
/// Oscillator panes shown under the price pane. GPU frame ids are `id * 8 + 1 + k`, so a
/// chart has room for at most seven.
const MAX_SUB_PANES: usize = 6;

/// Length of the load-in sweep across the visible bars.
const REVEAL_SECS: f32 = 0.7;
/// Length of an eased jump (quick ranges, reset, keyboard zoom and pan).
const GLIDE_SECS: f32 = 0.32;
/// Time constant of the autoscale easing.
const SCALE_TAU: f64 = 0.07;

/// A programmatic viewport change in progress; `start` is set on the first frame shown.
#[derive(Debug, Clone, Copy)]
struct Glide {
    from: Viewport,
    to: Viewport,
    start: Option<f64>,
}

impl Glide {
    /// Pans linearly and zooms geometrically, so a zoom feels even all the way.
    fn at(&self, e: f64) -> Viewport {
        let (a, b) = (self.from.bar_px.ln(), self.to.bar_px.ln());
        Viewport {
            right: self.from.right + (self.to.right - self.from.right) * e,
            bar_px: (a + (b - a) * e).exp(),
            ..self.to
        }
    }
}

#[derive(Default)]
struct DrawState {
    tool: Option<Tool>,
    pending: Option<Anchor>,
    second: Option<Anchor>,
    note: Option<NoteDraft>,
    selected: Option<usize>,
    hover: Option<(usize, DrawHandle)>,
    drag: Option<DrawingDrag>,
}

#[derive(Clone, Copy)]
enum DrawHandle {
    A,
    B,
    C,
    Body,
}

struct NoteDraft {
    anchor: Anchor,
    existing: Option<(usize, Drawing)>,
    text: String,
}

struct DrawingDrag {
    index: usize,
    handle: DrawHandle,
    start: Anchor,
    original: Drawing,
}

pub struct ChartState {
    pub id: PaneId,
    pub symbol: String,
    pub tf: Timeframe,
    pub chart_type: ChartType,
    pub log: bool,
    pub magnet: bool,
    pub show_volume: bool,
    pub show_congress: bool,
    pub indicators: Vec<IndicatorSpec>,
    pub drawings: Vec<Drawing>,
    pub compare: Option<String>,
    pub navigation: crate::chart_navigation::Navigation,
    /// How a comparison is scaled against the main symbol.
    pub compare_axis: crate::chart_compare::Axis,
    pub data: crate::chart_data::ChartData,
    refresh_at: std::time::Instant,
    pub local_cursor: Option<i64>,
    pub shared_cursor: Option<i64>,
    pub range_event: Option<crate::chart_navigation::DateRange>,
    pending_dates: Option<crate::chart_navigation::DateRange>,

    bars: Arc<Vec<Bar>>,
    source_bars: Arc<Vec<Bar>>,
    view_revision: u64,
    view_source: Option<Arc<Vec<Bar>>>,
    view_config: (Option<i64>, Option<u64>),
    replay_cutoff: Option<i64>,
    replay_play: bool,
    replay_speed: usize,
    replay_reanchor: bool,
    replay_tick: std::time::Instant,
    gpu: Option<SeriesGpu>,
    ha_gpu: Option<(Arc<Vec<Bar>>, Option<SeriesGpu>)>,
    analytics: crate::chart_studies::Cache,
    context_timestamp: Option<i64>,
    generation: u64,
    viewport: Option<Viewport>,
    plot_width: f64,
    manual_scale: Option<PriceScale>,
    glide: Option<Glide>,
    /// Shown autoscale bounds (transformed) and whether they are log, easing to the target.
    shown_scale: Option<(f64, f64, bool)>,
    /// Renewed when a load's first bars arrive, which replays the load-in sweep.
    reveal_key: u64,
    /// Renewed when a load's first Congress markers arrive, which pops them in.
    markers_key: u64,
    loading: bool,
    cached_history: bool,
    paging: bool,
    exhausted: bool,
    error: Option<String>,
    built: Vec<BuiltIndicator>,
    compare_line: Option<LineRef>,
    comparisons: std::collections::HashMap<String, crate::chart_compare::Series>,
    compare_scale: f64,
    compare_base: Option<f64>,
    markers: Vec<Trade>,
    pending_range: Option<Range>,
    draw: DrawState,
    hover_marker: Option<usize>,
    /// Resolved security for `symbol`.
    sec: Security,
    last_live: Option<std::time::Instant>,
    last_indicator_req: Option<std::time::Instant>,
    indicator_revision: u64,
    /// App-level actions requested from this chart (context menu), drained by the app.
    pub actions: Vec<Action>,
    pub price_alerts: Vec<tr_core::price_alerts::Alert>,
    pub price_alert_actions: Vec<crate::price_alerts::Action>,
    pub account_overlay: crate::chart_account::Snapshot,
    pub export_notice: Option<Result<std::path::PathBuf, String>>,
    events: Vec<tr_data::ChartEvent>,
    events_range: Option<(i64, i64)>,
    events_notice: String,
    live_notice: Option<String>,
    context_price: Option<f64>,
    /// False while a pane is being dragged over the workspace: no crosshair or hover cards.
    pub hover_enabled: bool,
    /// The workspace drew this chart's controls in the pane's tab strip, so skip the
    /// header row and give the plot the space.
    pub controls_in_strip: bool,
    indicator_editor: bool,
    indicator_focus: Option<u32>,
    compare_picker: crate::chart_compare::Picker,
    compare_revision: u64,
    compare_error: Option<String>,
    compare_loading: bool,
    indicator_search: String,
    template_name: String,
    template_names: Option<Vec<String>>,
    template_selected: String,
    template_status: Option<Result<String, String>>,
    drawing_undo: Vec<Vec<Drawing>>,
    drawing_redo: Vec<Vec<Drawing>>,
}

impl ChartState {
    pub fn new(id: PaneId, symbol: &str, tf: Timeframe) -> Self {
        let mut indicators = crate::command::default_indicators();
        study::assign_ids(&mut indicators);
        Self {
            id,
            symbol: tr_data::security(symbol).key(),
            tf,
            chart_type: if tr_data::security(symbol).class == AssetClass::Govt {
                ChartType::Line
            } else {
                ChartType::Candles
            },
            log: false,
            magnet: false,
            show_volume: true,
            show_congress: true,
            indicators,
            drawings: Vec::new(),
            compare: None,
            compare_axis: Default::default(),
            navigation: Default::default(),
            data: Default::default(),
            refresh_at: std::time::Instant::now(),
            local_cursor: None,
            shared_cursor: None,
            range_event: None,
            pending_dates: None,
            bars: Arc::new(Vec::new()),
            source_bars: Arc::new(Vec::new()),
            view_revision: 0,
            view_source: None,
            view_config: (None, None),
            replay_cutoff: None,
            replay_play: false,
            replay_speed: 0,
            replay_reanchor: false,
            replay_tick: std::time::Instant::now(),
            gpu: None,
            ha_gpu: None,
            analytics: Default::default(),
            context_timestamp: None,
            generation: 0,
            viewport: None,
            plot_width: 800.,
            manual_scale: None,
            glide: None,
            shown_scale: None,
            reveal_key: 0,
            markers_key: 0,
            loading: false,
            cached_history: false,
            paging: false,
            exhausted: false,
            error: None,
            built: Vec::new(),
            compare_line: None,
            comparisons: Default::default(),
            compare_scale: 1.0,
            compare_base: None,
            markers: Vec::new(),
            pending_range: None,
            draw: DrawState::default(),
            hover_marker: None,
            sec: tr_data::security(symbol),
            last_live: None,
            last_indicator_req: None,
            indicator_revision: 0,
            actions: Vec::new(),
            price_alerts: Vec::new(),
            price_alert_actions: Vec::new(),
            account_overlay: Default::default(),
            export_notice: None,
            events: vec![],
            events_range: None,
            events_notice: String::new(),
            live_notice: None,
            context_price: None,
            hover_enabled: true,
            controls_in_strip: false,
            indicator_editor: false,
            indicator_focus: None,
            compare_picker: Default::default(),
            compare_revision: 0,
            compare_error: None,
            compare_loading: false,
            indicator_search: String::new(),
            template_name: String::new(),
            template_names: None,
            template_selected: String::new(),
            template_status: None,
            drawing_undo: Vec::new(),
            drawing_redo: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    fn effective_session(&self) -> tr_core::session::Session {
        if matches!(self.sec.class, AssetClass::Equity | AssetClass::Etf) {
            self.data.session
        } else {
            tr_core::session::Session::Provider
        }
    }

    fn effective_history(&self) -> tr_data::HistoryOptions {
        if matches!(self.sec.class, AssetClass::Equity | AssetClass::Etf) {
            self.data.history
        } else {
            Default::default()
        }
    }

    /// (Re)load history for the current symbol and timeframe.
    pub fn load(&mut self, w: &Worker) {
        if self.compare.is_none() && !self.data.comparisons.is_empty() {
            self.compare = Some(self.data.comparisons.remove(0));
        }
        let mut seen = std::collections::HashSet::new();
        seen.insert(self.symbol.clone());
        if let Some(primary) = &self.compare {
            seen.insert(primary.clone());
        }
        self.data.comparisons = self
            .data
            .comparisons
            .iter()
            .filter_map(|key| crate::chart_compare::parse_key(key))
            .filter(|key| seen.insert(key.clone()))
            .take(7)
            .collect();
        self.sync_compare_slots();
        if let Some(view) = self.viewport {
            self.plot_width = view.width_px;
        }
        self.refresh_at = std::time::Instant::now();
        self.local_cursor = None;
        self.shared_cursor = None;
        self.range_event = None;
        if self.draw.drag.is_some() {
            self.cancel();
        }
        self.draw = DrawState::default();
        self.adopt_legacy_drawings();
        study::assign_ids(&mut self.indicators);
        self.generation += 1;
        self.loading = true;
        self.cached_history = false;
        self.paging = false;
        self.exhausted = false;
        self.error = None;
        self.viewport = None;
        self.manual_scale = None;
        self.glide = None;
        self.compare_line = None;
        self.comparisons.clear();
        self.compare_error = None;
        self.compare_loading = false;
        self.markers.clear();
        self.events.clear();
        self.events_range = None;
        self.events_notice.clear();
        // Never show the previous symbol's bars under the new title.
        self.bars = Arc::new(Vec::new());
        self.source_bars = Arc::new(Vec::new());
        self.view_source = None;
        self.replay_cutoff = None;
        self.replay_play = false;
        self.replay_reanchor = false;
        self.gpu = None;
        self.built.clear();
        w.send(Request::History {
            pane: self.id,
            generation: self.generation,
            symbol: self.symbol.clone(),
            tf: self.tf,
            options: self.effective_history(),
            limit: initial_limit(self.tf),
        });
        self.last_live = None;
        self.live_notice = None;
        if self.sec.has_congress_trades() {
            w.send(Request::Markers { pane: self.id, symbol: self.symbol.clone() });
        }
    }

    pub fn set_symbol(&mut self, symbol: &str, w: &Worker) {
        self.pending_dates = None;
        self.adopt_legacy_drawings();
        let sec = tr_data::security(symbol);
        if sec.class == AssetClass::Govt {
            self.chart_type = ChartType::Line;
        } else if self.sec.class == AssetClass::Govt {
            self.chart_type = ChartType::Candles;
        }
        self.symbol = sec.key();
        if self.compare.as_deref() == Some(&self.symbol) {
            self.compare = None;
        }
        self.sec = sec;
        self.data.analytics.baseline = None;
        self.data.analytics.renko_size = 0.;
        self.draw = DrawState::default();
        self.load(w);
    }

    pub fn set_timeframe(&mut self, tf: Timeframe, w: &Worker) {
        if tf != self.tf {
            self.pending_dates =
                self.viewport.as_ref().and_then(|v| crate::chart_navigation::date_range(&self.bars, self.tf, v));
            self.tf = tf;
            self.load(w);
        }
    }

    pub fn set_range(&mut self, r: Range, w: &Worker) {
        self.pending_dates = None;
        self.pending_range = Some(r);
        if r.timeframe() != self.tf {
            self.tf = r.timeframe();
            self.load(w);
        } else {
            self.glide(Self::apply_range);
        }
    }

    /// Apply `change` to the view (on top of any glide's destination) and ease to it.
    fn glide(&mut self, change: impl FnOnce(&mut Self)) {
        let shown = self.viewport;
        if let Some(g) = self.glide.take() {
            self.viewport = Some(g.to);
        }
        change(self);
        if self.navigation.range {
            self.range_event =
                self.viewport.as_ref().and_then(|v| crate::chart_navigation::date_range(&self.bars, self.tf, v));
        }
        if let (Some(from), Some(to)) = (shown, self.viewport)
            && from != to
            && !crate::anim::reduced()
        {
            self.viewport = Some(from);
            self.glide = Some(Glide { from, to, start: None });
        }
    }

    fn apply_range(&mut self) {
        let Some(r) = self.pending_range.take() else { return };
        let (Some(v), Some(last)) = (self.viewport.as_mut(), self.bars.last()) else { return };
        let count = match r.span_secs(last.ts) {
            Some(span) => self.bars.len() - self.bars.partition_point(|b| b.ts < last.ts - span),
            None => self.bars.len(),
        };
        *v = Viewport::fit_last(self.bars.len(), count.max(2), v.width_px);
        if self.navigation.range {
            self.range_event = crate::chart_navigation::date_range(&self.bars, self.tf, v);
        }
        self.manual_scale = None;
    }

    pub fn toggle_indicator(&mut self, spec: IndicatorSpec, w: &Worker) {
        study::assign_ids(&mut self.indicators);
        if let Some(i) = self.indicators.iter().position(|s| s.code == spec.code && s.params == spec.params) {
            self.remove_study(i);
        } else {
            self.indicators.push(spec);
        }
        study::assign_ids(&mut self.indicators);
        self.request_indicators(w);
    }

    /// A removed source also removes its calculation descendants. Pane-only links
    /// survive in their own panes rather than dangling after the owner disappears.
    fn remove_study(&mut self, index: usize) {
        let Some(id) = self.indicators.get(index).and_then(|s| s.chart.as_ref()).map(|c| c.id) else { return };
        let mut removed = std::collections::HashSet::from([id]);
        loop {
            let before = removed.len();
            for spec in &self.indicators {
                if let Some(c) = &spec.chart
                    && let StudySource::Output { study, .. } = c.source
                    && removed.contains(&study)
                {
                    removed.insert(c.id);
                }
            }
            if removed.len() == before {
                break;
            }
        }
        self.indicators.retain(|s| s.chart.as_ref().is_none_or(|c| !removed.contains(&c.id)));
        for spec in &mut self.indicators {
            if let Some(c) = &mut spec.chart
                && matches!(c.pane, StudyPane::With(id) if removed.contains(&id))
            {
                c.pane = StudyPane::Own;
            }
        }
        self.built.retain(|b| b.spec.chart.as_ref().is_none_or(|c| !removed.contains(&c.id)));
    }

    /// Remove every instance of the indicator `code` (all moving averages of one kind).
    fn remove_indicator(&mut self, code: &str, w: &Worker) {
        study::assign_ids(&mut self.indicators);
        while let Some(index) = self.indicators.iter().position(|s| s.code == code) {
            self.remove_study(index);
        }
        self.request_indicators(w);
    }

    pub fn clear_indicators(&mut self) {
        self.indicators.clear();
        self.built.clear();
    }

    fn sync_compare_slots(&mut self) {
        let symbols: Vec<_> = self.compare.iter().chain(&self.data.comparisons).cloned().collect();
        crate::chart_compare::sync_slots(&mut self.data.compare_slots, &symbols);
    }

    fn compare_color(&self, symbol: &str) -> Color32 {
        theme::compare(crate::chart_compare::slot(&self.data.compare_slots, symbol))
    }

    fn primary_compare_color(&self) -> Color32 {
        self.compare_color(self.compare.as_deref().unwrap_or_default())
    }

    pub fn set_compare(&mut self, symbol: Option<String>, w: &Worker) {
        self.data.comparisons.clear();
        self.data.hidden_comparisons.clear();
        self.comparisons.clear();
        self.compare = symbol.and_then(|s| crate::chart_compare::parse_key(&s)).filter(|s| *s != self.symbol);
        self.compare_line = None;
        self.compare_error = None;
        self.compare_loading = false;
        self.request_compare(w);
    }

    pub fn start_tool(&mut self, tool: Tool) {
        self.cancel();
        self.draw = DrawState { tool: Some(tool), ..Default::default() };
    }

    pub fn jump_to(&mut self, ts: i64) {
        let half = (self.viewport.map_or(100., |v| v.visible_bars() / 2.) * self.tf.seconds() as f64) as i64;
        self.apply_linked_range(crate::chart_navigation::DateRange {
            start: ts.saturating_sub(half),
            end: ts.saturating_add(half.max(self.tf.seconds())),
        });
    }
    pub fn plot_scan(&mut self, w: &Worker) {
        let Some(scan) = &self.data.workflow.scan else {
            return;
        };
        let Ok(expression) = crate::chart_workflow::expression(scan) else {
            return;
        };
        let previous = self.indicators.iter().position(|s| s.chart.as_ref().is_some_and(|c| c.scan_plot));
        if previous.is_none() && self.indicators.len() >= 128 {
            return;
        }
        let mut spec = IndicatorSpec::new("PCF", &[]).unwrap();
        spec.chart = Some(study::Settings::new(0, &spec));
        let settings = spec.chart.as_mut().unwrap();
        settings.expression = Some(expression);
        settings.scan_plot = true;
        settings.pane = StudyPane::Own;
        if let Some(index) = previous {
            spec.chart.as_mut().unwrap().id = self.indicators[index].chart.as_ref().unwrap().id;
            self.indicators[index] = spec;
        } else {
            self.indicators.push(spec);
        }
        study::assign_ids(&mut self.indicators);
        self.request_indicators(w);
    }

    /// Esc: cancel an in-progress drawing, then deselect. Returns whether it consumed the key.
    pub fn cancel(&mut self) -> bool {
        if let Some(drag) = self.draw.drag.take() {
            if let Some(drawing) = self.drawings.get_mut(drag.index) {
                *drawing = drag.original;
            }
            self.drawing_undo.pop();
        }
        if self.draw.tool.is_some() || self.draw.selected.is_some() || self.draw.note.is_some() {
            self.draw = DrawState::default();
            true
        } else {
            false
        }
    }

    pub fn delete_selected(&mut self) -> bool {
        match self.draw.selected.take() {
            Some(i) if i < self.drawings.len() => {
                self.remember_drawings();
                self.drawings.remove(i);
                true
            }
            _ => false,
        }
    }

    fn adopt_legacy_drawings(&mut self) {
        for drawing in self
            .drawings
            .iter_mut()
            .chain(self.drawing_undo.iter_mut().flatten())
            .chain(self.drawing_redo.iter_mut().flatten())
        {
            if drawing.symbol.is_none() {
                drawing.symbol = Some(self.symbol.clone());
            }
        }
    }

    fn remember_drawings(&mut self) {
        if self.drawing_undo.len() >= 64 {
            self.drawing_undo.remove(0);
        }
        self.drawing_undo.push(self.drawings.clone());
        self.drawing_redo.clear();
    }

    pub fn undo_drawings(&mut self) {
        if let Some(previous) = self.drawing_undo.pop() {
            self.drawing_redo.push(std::mem::replace(&mut self.drawings, previous));
            self.draw = DrawState::default();
        }
    }

    pub fn redo_drawings(&mut self) {
        if let Some(next) = self.drawing_redo.pop() {
            self.drawing_undo.push(std::mem::replace(&mut self.drawings, next));
            self.draw = DrawState::default();
        }
    }

    pub fn reset_view(&mut self) {
        self.glide(|s| {
            if let Some(v) = s.viewport.as_mut() {
                *v = Viewport::fit_last(s.bars.len(), DEFAULT_VISIBLE, v.width_px);
            }
            s.manual_scale = None;
        });
    }

    pub fn pan_bars(&mut self, n: f64) {
        self.glide(|s| {
            if let Some(v) = s.viewport.as_mut() {
                v.pan_bars(n);
                v.clamp(s.bars.len());
            }
        });
    }

    pub fn zoom(&mut self, factor: f64) {
        self.glide(|s| {
            if let Some(v) = s.viewport.as_mut() {
                let anchor = v.width_px * 0.85;
                v.zoom(factor, anchor);
                v.clamp(s.bars.len());
            }
        });
    }

    /// Ease the shown autoscale range toward `target`, so the axis glides as bars scroll
    /// in rather than jumping. Snaps on new data and when switching log scale.
    fn ease_scale(&mut self, ctx: &egui::Context, target: PriceScale) -> PriceScale {
        let (a, b) = target.bounds_t();
        let (lo, hi) = match self.shown_scale {
            Some((lo, hi, log)) if log == target.log && !crate::anim::reduced() => {
                let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1) as f64;
                let k = 1.0 - (-dt / SCALE_TAU).exp();
                let (lo, hi) = (lo + (a - lo) * k, hi + (b - hi) * k);
                if (lo - a).abs() + (hi - b).abs() < (b - a).abs() * 1e-4 {
                    (a, b)
                } else {
                    ctx.request_repaint();
                    (lo, hi)
                }
            }
            _ => (a, b),
        };
        self.shown_scale = Some((lo, hi, target.log));
        let inv = |x: f64| if target.log { x.exp() } else { x };
        PriceScale { lo: inv(lo), hi: inv(hi), ..target }
    }

    fn request_indicators(&mut self, w: &Worker) {
        if !self.bars.is_empty() {
            self.indicator_revision += 1;
            w.send(Request::Indicators {
                revision: self.indicator_revision,
                pane: self.id,
                generation: self.generation,
                bars: self.bars.clone(),
                symbol: self.symbol.clone(),
                options: self.effective_history(),
                tf: self.tf,
                specs: self.indicators.clone(),
                palette: theme::palette(),
                session: self.effective_session(),
            });
        }
    }

    fn request_compare(&mut self, w: &Worker) {
        self.sync_compare_slots();
        self.comparisons.retain(|key, _| self.data.comparisons.contains(key));
        let symbols: Vec<_> = self.compare.iter().chain(self.data.comparisons.iter()).cloned().collect();
        if self.chart_type != ChartType::Renko && !symbols.is_empty() && !self.bars.is_empty() {
            self.compare_revision += 1;
            self.compare_loading = true;
            self.compare_error = None;
            if self.compare_line.as_ref().is_some_and(|l| l.pyramid.level_len(0) != self.bars.len()) {
                self.compare_line = None;
            }
            for sym in symbols {
                if self.compare.as_ref() != Some(&sym) {
                    let extra = self.comparisons.entry(sym.clone()).or_default();
                    extra.loading = true;
                    extra.error = None;
                    if extra.line.as_ref().is_some_and(|l| l.pyramid.level_len(0) != self.bars.len()) {
                        extra.line = None;
                    }
                }
                w.send(Request::Compare {
                    revision: self.compare_revision,
                    pane: self.id,
                    generation: self.generation,
                    symbol: sym,
                    tf: self.tf,
                    options: self.effective_history(),
                    base: self.bars.clone(),
                });
            }
        }
    }

    // ---- worker responses -------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn on_history(
        &mut self,
        generation: u64,
        bars: Arc<Vec<Bar>>,
        gpu: SeriesGpu,
        prepended: usize,
        from_cache: bool,
        exhausted: bool,
        live: bool,
        w: &Worker,
    ) {
        if generation != self.generation {
            return;
        }
        let session = self.effective_session();
        let filtered = session != tr_core::session::Session::Provider && self.tf.is_intraday();
        let (bars, gpu, prepended) = if filtered {
            let filtered = Arc::new(session.filter(&bars, self.tf));
            let prepended = if prepended > 0 {
                self.bars.first().map_or(0, |first| filtered.partition_point(|b| b.ts < first.ts))
            } else {
                0
            };
            let gpu = SeriesGpu::build(&filtered);
            (filtered, gpu, prepended)
        } else {
            (bars, gpu, prepended)
        };
        if live {
            self.last_live = Some(std::time::Instant::now());
        }
        self.source_bars = bars.clone();
        self.cached_history = from_cache;
        self.paging = false;
        self.exhausted = exhausted || self.exhausted && prepended == 0;
        if !from_cache {
            self.loading = false;
        }
        if self.replay_cutoff.is_some() || self.chart_type == ChartType::Renko {
            self.refresh_view(w);
            return;
        }
        self.view_source = Some(self.source_bars.clone());
        self.view_config = (None, None);
        self.apply_view(bars, gpu, prepended, live, w);
    }
    fn apply_view(&mut self, bars: Arc<Vec<Bar>>, gpu: SeriesGpu, prepended: usize, live: bool, w: &Worker) {
        let old_len = self.bars.len();
        let new_len = bars.len();
        if old_len == 0 && new_len > 0 {
            self.reveal_key = crate::anim::fresh_key();
            self.shown_scale = None;
        }
        // Keep a glide in progress on the same bars as the data shifts under it.
        if let Some(g) = self.glide.as_mut() {
            let shift = if prepended > 0 { prepended as f64 } else { new_len as f64 - old_len as f64 };
            g.from.right += shift;
            g.to.right += shift;
        }
        self.bars = bars;
        self.gpu = Some(gpu);
        self.error = None;
        match self.viewport.as_mut() {
            None => {
                self.viewport = Some(Viewport::fit_last(new_len, DEFAULT_VISIBLE, self.plot_width));
            }
            Some(v) if prepended > 0 => v.right += prepended as f64,
            Some(v) => {
                // A refresh replaced the data: keep the same distance from the latest bar.
                let from_end = old_len as f64 - v.right;
                v.right = new_len as f64 - from_end;
            }
        }
        self.apply_range();
        if let Some(range) = self.pending_dates {
            if let Some(v) = &mut self.viewport {
                crate::chart_navigation::apply(&self.bars, self.tf, range, v);
            }
            if !self.cached_history && (self.exhausted || self.bars.first().is_some_and(|bar| bar.ts <= range.start)) {
                self.pending_dates = None;
            }
        }
        // Live ticks only move the last bar: refresh indicators at most every 1.5 s and
        // leave the compare series alone.
        let due = self.last_indicator_req.is_none_or(|t| t.elapsed().as_millis() >= 1500);
        if !live || due {
            self.request_indicators(w);
            self.last_indicator_req = Some(std::time::Instant::now());
        }
        if !live || old_len != new_len {
            self.request_compare(w);
        }
    }

    fn effective_box_size(&self) -> f64 {
        if self.data.analytics.renko_size > 0. {
            self.data.analytics.renko_size
        } else {
            let n = self
                .replay_cutoff
                .map_or(self.source_bars.len(), |cutoff| self.source_bars.partition_point(|b| b.ts <= cutoff));
            tr_core::chart_analytics::auto_box_size(&self.source_bars[..n])
        }
    }

    fn box_controls(&mut self, ui: &mut Ui) {
        let mut auto = self.data.analytics.renko_size <= 0.;
        let effective = self.effective_box_size();
        if ui.checkbox(&mut auto, "Auto box (ATR 14)").changed() {
            self.data.analytics.renko_size = if auto { 0. } else { effective };
        }
        let mut size = if auto { effective } else { self.data.analytics.renko_size };
        if ui.add_enabled(!auto, egui::DragValue::new(&mut size).range(0.000001..=1e9).speed(0.1)).changed() {
            self.data.analytics.renko_size = size;
        }
    }

    fn refresh_view(&mut self, w: &Worker) {
        let size = (self.chart_type == ChartType::Renko).then_some(self.effective_box_size());
        let config = (self.replay_cutoff, size.map(f64::to_bits));
        if self.source_bars.is_empty()
            || self.view_source.as_ref().is_some_and(|s| Arc::ptr_eq(s, &self.source_bars))
                && config == self.view_config
        {
            return;
        }
        if config == self.view_config
            && let Some(cutoff) = self.replay_cutoff
            && let Some(old) = &self.view_source
        {
            let n = self.source_bars.partition_point(|b| b.ts <= cutoff);
            let old_n = old.partition_point(|b| b.ts <= cutoff);
            if n == old_n && self.source_bars[..n] == old[..old_n] {
                self.view_source = Some(self.source_bars.clone());
                return;
            }
        }
        self.replay_reanchor |= config.0.is_some() != self.view_config.0.is_some();
        let reset = config.1 != self.view_config.1;
        self.view_source = Some(self.source_bars.clone());
        self.view_config = config;
        self.view_revision = self.view_revision.wrapping_add(1);
        self.indicator_revision = self.indicator_revision.wrapping_add(1);
        self.compare_revision = self.compare_revision.wrapping_add(1);
        if reset {
            self.built.clear();
            self.compare_line = None;
            self.comparisons.clear();
            self.bars = Arc::new(vec![]);
            self.gpu = None;
            self.ha_gpu = None;
            self.viewport = None;
            self.manual_scale = None;
            self.shown_scale = None;
            self.glide = None;
        }
        w.send(Request::ChartView {
            pane: self.id,
            generation: self.generation,
            revision: self.view_revision,
            source: self.source_bars.clone(),
            cutoff: self.replay_cutoff,
            renko: size,
        });
    }
    pub fn on_view(
        &mut self,
        generation: u64,
        revision: u64,
        source: Arc<Vec<Bar>>,
        result: Result<(Arc<Vec<Bar>>, SeriesGpu), String>,
        w: &Worker,
    ) {
        if generation != self.generation || revision != self.view_revision {
            return;
        }
        if !Arc::ptr_eq(&source, &self.source_bars) {
            let Some(cutoff) = self.replay_cutoff else { return };
            let n = self.source_bars.partition_point(|b| b.ts <= cutoff);
            let old_n = source.partition_point(|b| b.ts <= cutoff);
            // Live tails may change while an identical replay prefix is building.
            if n != old_n || self.source_bars[..n] != source[..old_n] {
                return;
            }
        }
        match result {
            Ok((bars, gpu)) => {
                let viewport = self.viewport;
                let old_len = self.bars.len();
                self.apply_view(bars, gpu, 0, false, w);
                if self.replay_reanchor {
                    self.glide = None;
                }
                if let Some(mut view) = viewport {
                    view = replay_viewport(view, old_len, self.bars.len(), self.replay_reanchor);
                    self.viewport = Some(view);
                }
                self.replay_reanchor = false;
            }
            Err(e) => self.error = Some(e),
        }
    }
    /// Consume replay shortcuts before ordinary chart navigation.
    pub fn replay_keys(&mut self, ctx: &egui::Context, w: &Worker) -> bool {
        let Some(cutoff) = self.replay_cutoff else { return false };
        if ctx.egui_wants_keyboard_input() {
            return false;
        }
        let pressed = |key| ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key));
        if pressed(egui::Key::Escape) {
            self.replay_cutoff = None;
            self.replay_play = false;
            self.refresh_view(w);
            return true;
        }
        let toggle = pressed(egui::Key::Space);
        if toggle {
            self.replay_play = !self.replay_play;
            self.replay_tick = std::time::Instant::now();
        }
        let back = pressed(egui::Key::ArrowLeft);
        let next = pressed(egui::Key::ArrowRight);
        if !self.source_bars.is_empty() && (back || next) {
            let mut index = self.source_bars.partition_point(|b| b.ts <= cutoff).saturating_sub(1);
            if back {
                index = index.saturating_sub(1);
            }
            if next {
                index = (index + 1).min(self.source_bars.len() - 1);
            }
            self.replay_cutoff = Some(self.source_bars[index].ts);
            self.refresh_view(w);
        }
        toggle || back || next
    }

    fn replay_controls(&mut self, ui: &mut Ui, w: &Worker) {
        let Some(cutoff) = self.replay_cutoff else {
            return;
        };
        let len = self.source_bars.len();
        if len == 0 {
            return;
        }
        let mut index = self.source_bars.partition_point(|b| b.ts <= cutoff).saturating_sub(1);
        ui.spacing_mut().item_spacing.x = theme::dp(4.);
        ui.label(RichText::new("REPLAY").font(mono(10.)).color(amber()));
        if widgets::icon_button(ui, widgets::Icon::StepBack, "Previous bar (←)", false).clicked() {
            index = index.saturating_sub(1);
        }
        let (icon, hint) = if self.replay_play {
            (widgets::Icon::Pause, "Pause (Space)")
        } else {
            (widgets::Icon::Play, "Play (Space)")
        };
        if widgets::icon_button(ui, icon, hint, self.replay_play).clicked() {
            self.replay_play = !self.replay_play;
            self.replay_tick = std::time::Instant::now();
        }
        if widgets::icon_button(ui, widgets::Icon::StepForward, "Next bar (→)", false).clicked() {
            index = (index + 1).min(len - 1);
        }
        if let Some(speed) = widgets::segmented(
            ui,
            (self.id, "replay-speed"),
            &["1×", "2×", "5×"],
            Some(self.replay_speed),
            theme::dp(22.),
        ) {
            self.replay_speed = speed;
            self.replay_tick = std::time::Instant::now();
        }
        if self.replay_play {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
            if self.replay_tick.elapsed().as_secs_f32() >= 1. / [1., 2., 5.][self.replay_speed] {
                index = (index + 1).min(len - 1);
                self.replay_tick = std::time::Instant::now();
            }
            if index == len - 1 {
                self.replay_play = false;
            }
        }
        let caption_width =
            ui.painter().layout_no_wrap(format!("Bar {len} / {len} · 2000-00-00"), ui_font(11.), text()).size().x;
        let slider_width = (ui.available_width() - caption_width - theme::dp(40.)).max(theme::dp(24.));
        ui.scope(|ui| {
            ui.spacing_mut().slider_width = slider_width;
            // Render after playback advances, using an explicitly linear bar-index range.
            let mut position = index as f64;
            ui.add(
                egui::Slider::new(&mut position, 0.0..=(len - 1) as f64).integer().logarithmic(false).show_value(false),
            );
            index = (position.round() as usize).min(len - 1);
        });
        let date = chrono::DateTime::from_timestamp(self.source_bars[index].ts, 0)
            .map(|date| date.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        ui.label(RichText::new(format!("Bar {} / {len} · {date}", index + 1)).font(ui_font(11.)));
        self.replay_cutoff = Some(self.source_bars[index].ts);
        if widgets::icon_button(ui, widgets::Icon::Close, "Exit replay (Esc)", false).clicked() {
            self.replay_cutoff = None;
            self.replay_play = false;
        }
        self.refresh_view(w);
    }
    pub fn on_history_error(&mut self, generation: u64, error: String) {
        if generation == self.generation {
            self.loading = false;
            self.paging = false;
            self.exhausted = true;
            self.error =
                Some(if self.bars.is_empty() { error } else { format!("{error} · previous history retained") });
        }
    }

    pub fn on_indicators(&mut self, generation: u64, revision: u64, built: Vec<BuiltIndicator>) {
        if generation == self.generation
            && revision == self.indicator_revision
            && built.iter().map(|b| &b.spec).eq(self.indicators.iter())
        {
            self.built = built;
        }
    }

    pub fn on_compare(&mut self, generation: u64, revision: u64, symbol: &str, line: LineRef) {
        if generation == self.generation
            && revision == self.compare_revision
            && self.data.comparisons.iter().any(|s| s == symbol)
        {
            let extra = self.comparisons.entry(symbol.to_string()).or_default();
            extra.line = Some(line);
            extra.loading = false;
            extra.error = None;
            return;
        }
        if generation == self.generation && revision == self.compare_revision && self.compare.as_deref() == Some(symbol)
        {
            self.compare_line = Some(line);
            self.compare_loading = false;
            self.compare_error = None;
        }
    }

    pub fn on_heikin(&mut self, generation: u64, bars: Arc<Vec<Bar>>, gpu: SeriesGpu) {
        if generation == self.generation && Arc::ptr_eq(&self.bars, &bars) {
            self.ha_gpu = Some((bars, Some(gpu)));
        }
    }
    pub fn on_live_status(&mut self, generation: u64, message: Option<String>) {
        if generation == self.generation {
            self.live_notice = message;
        }
    }
    pub fn on_events(
        &mut self,
        generation: u64,
        symbol: &str,
        range: (i64, i64),
        events: Vec<tr_data::ChartEvent>,
        notice: String,
    ) {
        if generation == self.generation && symbol == self.symbol && self.events_range == Some(range) {
            self.events = events;
            self.events_notice = notice;
        }
    }

    pub fn on_compare_error(&mut self, generation: u64, revision: u64, symbol: &str, error: String) {
        if generation == self.generation
            && revision == self.compare_revision
            && self.data.comparisons.iter().any(|s| s == symbol)
        {
            let extra = self.comparisons.entry(symbol.to_string()).or_default();
            extra.error = Some(error);
            extra.loading = false;
            return;
        }
        if generation == self.generation && revision == self.compare_revision && self.compare.as_deref() == Some(symbol)
        {
            self.compare_error = Some(error);
            self.compare_loading = false;
        }
    }
    pub fn on_compare_search(&mut self, query: &str, result: Result<Vec<tr_data::SearchHit>, String>) {
        self.compare_picker.on_hits(query, result);
    }

    pub fn apply_linked_range(&mut self, range: crate::chart_navigation::DateRange) {
        if self.bars.first().is_none_or(|b| range.start < b.ts) {
            self.pending_dates = Some(range);
        }
        if let Some(v) = &mut self.viewport {
            crate::chart_navigation::apply(&self.bars, self.tf, range, v);
            self.glide = None;
        } else {
            self.pending_dates = Some(range);
        }
    }

    pub fn on_markers(&mut self, symbol: &str, trades: Vec<Trade>) {
        if symbol == self.symbol {
            if self.markers.is_empty() && !trades.is_empty() {
                self.markers_key = crate::anim::fresh_key();
            }
            self.markers = trades;
        }
    }

    // ---- coordinate helpers -----------------------------------------------------------

    /// Fractional bar index for a timestamp (extrapolated past either end).
    fn index_of_ts(&self, ts: i64) -> f64 {
        let b = &self.bars;
        let (Some(first), Some(last)) = (b.first(), b.last()) else { return 0.0 };
        let step = self.tf.seconds() as f64;
        if ts > last.ts {
            return (b.len() - 1) as f64 + (ts - last.ts) as f64 / step + 0.5;
        }
        if ts < first.ts {
            return (ts - first.ts) as f64 / step + 0.5;
        }
        b.partition_point(|x| x.ts <= ts).saturating_sub(1) as f64 + 0.5
    }

    fn ts_of_index(&self, idx: f64) -> i64 {
        let b = &self.bars;
        let Some(last) = b.last() else { return 0 };
        let i = idx.floor();
        if i >= b.len() as f64 {
            return last.ts + ((i - (b.len() - 1) as f64) * self.tf.seconds() as f64) as i64;
        }
        if i < 0.0 {
            return b[0].ts + (i * self.tf.seconds() as f64) as i64;
        }
        b[i as usize].ts
    }

    // ---- rendering --------------------------------------------------------------------

    /// Draw the pane into `rect`. Returns true if the pane was clicked (focus request).
    /// Width of [`Self::controls`] (interval and chart-type segmented controls).
    pub fn controls_width(ui: &Ui) -> f32 {
        let tf: Vec<&str> = Timeframe::ALL.iter().map(|t| t.label()).collect();
        let ct: Vec<&str> = ChartType::ALL.iter().map(|c| c.label()).collect();
        widgets::segmented_width(ui, &tf, CONTROL_H)
            + widgets::segmented_width(ui, &ct, CONTROL_H)
            + Self::action_width(ui, "Indicators…")
            + Self::action_width(ui, "Compare…")
            + 24.
            + ui.spacing().item_spacing.x * 5.
    }

    fn action_width(ui: &Ui, label: &str) -> f32 {
        ui.painter().layout_no_wrap(label.into(), ui_font(11.), text()).size().x + ui.spacing().button_padding.x * 2.
    }

    fn chart_actions(&mut self, ui: &mut Ui) -> bool {
        let mut clicked = false;
        if ui
            .add_sized(
                vec2(Self::action_width(ui, "Indicators…"), CONTROL_H),
                egui::Button::new(RichText::new("Indicators…").font(ui_font(11.))),
            )
            .clicked()
        {
            self.indicator_editor = true;
            clicked = true;
        }
        if ui
            .add_sized(
                vec2(Self::action_width(ui, "Compare…"), CONTROL_H),
                egui::Button::new(RichText::new("Compare…").font(ui_font(11.))),
            )
            .on_hover_text("Choose or remove the comparison symbol")
            .clicked()
        {
            self.compare_picker.open();
            clicked = true;
        }
        clicked
    }

    /// Interval and chart-type controls, laid out left to right in `ui`. Returns whether
    /// one was clicked.
    pub fn controls(&mut self, ui: &mut Ui, w: &Worker) -> bool {
        let mut clicked = false;
        let tf_labels: Vec<&str> = Timeframe::ALL.iter().map(|t| t.label()).collect();
        let selected_tf = Timeframe::ALL.iter().position(|t| *t == self.tf);
        if let Some(i) = widgets::segmented(ui, (self.id, "tf"), &tf_labels, selected_tf, CONTROL_H) {
            self.set_timeframe(Timeframe::ALL[i], w);
            clicked = true;
        }
        ui.add_space(8.0);
        let ct_labels: Vec<&str> = ChartType::ALL.iter().map(|c| c.label()).collect();
        let selected_ct = ChartType::ALL.iter().position(|c| *c == self.chart_type);
        if let Some(i) = widgets::segmented(ui, (self.id, "ct"), &ct_labels, selected_ct, CONTROL_H) {
            self.chart_type = ChartType::ALL[i];
            clicked = true;
        }
        self.mode_badge(ui);
        ui.add_space(6.);
        clicked |= self.chart_actions(ui);
        clicked
    }

    fn mode_badge(&self, ui: &mut Ui) {
        let mode = if self.replay_cutoff.is_some() {
            Some(("REPLAY", "REPLAY · historical bars · live alerts independent"))
        } else if self.chart_type == ChartType::Renko {
            Some(("RENKO", "RENKO · synthetic bricks · one-box reversal"))
        } else if self.chart_type == ChartType::HeikinAshi {
            Some(("HA", "Heikin Ashi display · OHLC/studies/P&L use source bars"))
        } else {
            None
        };
        if let Some((label, hint)) = mode {
            egui::Frame::new()
                .stroke(Stroke::new(1., amber()))
                .corner_radius(theme::CONTROL_RADIUS)
                .inner_margin(egui::Margin::symmetric(4, 1))
                .show(ui, |ui| {
                    ui.label(RichText::new(label).font(mono(10.)).color(amber())).on_hover_text(hint);
                });
        }
    }

    pub fn show(&mut self, ui: &mut Ui, rect: Rect, focused: bool, w: &Worker) -> bool {
        self.refresh_view(w);
        if self.data.show_events
            && self.replay_cutoff.is_none()
            && self.chart_type != ChartType::Renko
            && let (Some(first), Some(last)) = (self.source_bars.first(), self.source_bars.last())
        {
            let range = (first.ts.div_euclid(86_400) * 86_400, (last.ts.div_euclid(86_400) + 1) * 86_400);
            if self.events_range != Some(range) {
                self.events_range = Some(range);
                self.events_notice = "Loading corporate events…".into();
                w.send(Request::ChartEvents {
                    pane: self.id,
                    generation: self.generation,
                    symbol: self.symbol.clone(),
                    start: range.0,
                    end: range.1,
                });
            }
        }
        self.local_cursor = None;
        if self.data.refresh_seconds > 0
            && !self.loading
            && self.refresh_at.elapsed().as_secs() >= u64::from(self.data.refresh_seconds.max(15))
        {
            self.refresh_at = std::time::Instant::now();
            self.loading = true;
            w.send(Request::History {
                pane: self.id,
                generation: self.generation,
                symbol: self.symbol.clone(),
                tf: self.tf,
                options: self.effective_history(),
                limit: initial_limit(self.tf).max(self.bars.len()),
            });
        }
        if self.data.refresh_seconds > 0 {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
        }
        study::assign_ids(&mut self.indicators);
        self.show_indicator_editor(ui.ctx(), w);
        self.show_note_editor(ui.ctx());
        self.compare_picker.chart = Some(rect);
        if let Some(chosen) = self.compare_picker.show(
            ui.ctx(),
            self.id,
            &self.symbol,
            self.compare.as_deref(),
            (&mut self.compare_axis, &mut self.data.comparisons, &mut self.data.hidden_comparisons),
            w,
        ) {
            if let Some(symbol) = chosen {
                self.data.hidden_comparisons.retain(|s| s != &symbol);
                if self.compare.is_none() {
                    self.set_compare(Some(symbol), w);
                } else if self.compare.as_ref() != Some(&symbol)
                    && !self.data.comparisons.contains(&symbol)
                    && self.data.comparisons.len() < 7
                {
                    self.data.hidden_comparisons.retain(|s| s != &symbol);
                    self.data.comparisons.push(symbol);
                    self.request_compare(w);
                }
            } else if self.data.comparisons.is_empty() {
                self.set_compare(None, w);
            } else {
                let next = self.data.comparisons.remove(0);
                self.compare_line = self.comparisons.remove(&next).and_then(|s| s.line);
                self.compare = Some(next);
                self.request_compare(w);
            }
        }
        if !self.data.comparisons.is_empty() && self.compare_axis == CompareAxis::Own {
            self.compare_axis = CompareAxis::Percent;
        }
        self.sync_compare_slots();
        self.comparisons.retain(|key, _| self.data.comparisons.contains(key));
        let painter = ui.painter_at(rect);
        // Sits under a tab strip inside a card: square top, rounded bottom.
        let r = theme::CARD_RADIUS.saturating_sub(1);
        painter.rect_filled(rect, CornerRadius { nw: 0, ne: 0, sw: r, se: r }, theme::card());
        let mut clicked = false;

        // Controls live in the tab strip when it has room; otherwise a slim header row.
        let controls_h = if self.controls_in_strip { 0.0 } else { HEADER_H };
        let replay_h = if self.replay_cutoff.is_some() { 32. } else { 0. };
        let header_h = controls_h + replay_h;
        if replay_h > 0. {
            let toolbar = Rect::from_min_size(
                pos2(rect.left() + 6., rect.top() + controls_h),
                vec2(rect.width() - 12., replay_h),
            );
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(toolbar)
                    .layout(egui::Layout::left_to_right(egui::Align::Center))
                    .id_salt((self.id, "replay")),
            );
            self.replay_controls(&mut child, w);
        }
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), controls_h));
        if controls_h > 0.0 {
            painter.hline(header.x_range(), header.bottom(), Stroke::new(1.0, theme::border()));
            let mut hui = ui.new_child(
                UiBuilder::new()
                    .max_rect(header.shrink2(vec2(8.0, 0.0)))
                    .layout(egui::Layout::left_to_right(egui::Align::Center))
                    .id_salt((self.id, "header")),
            );
            hui.spacing_mut().item_spacing.x = 6.0;
            hui.label(RichText::new(&self.sec.ticker).font(mono(12.0)).color(amber()));
            if hui.available_width() > Self::controls_width(&hui) {
                clicked |= self.controls(&mut hui, w);
            } else {
                // Narrow pane: the same controls as compact menus.
                hui.menu_button(RichText::new(self.tf.label()).font(ui_font(11.5)), |ui| {
                    for tf in Timeframe::ALL {
                        if ui.radio(self.tf == tf, tf.label()).clicked() {
                            self.set_timeframe(tf, w);
                            clicked = true;
                            ui.close();
                        }
                    }
                });
                hui.menu_button(RichText::new(self.chart_type.label()).font(ui_font(11.5)), |ui| {
                    for ct in ChartType::ALL {
                        if ui.radio(self.chart_type == ct, ct.label()).clicked() {
                            self.chart_type = ct;
                            clicked = true;
                            ui.close();
                        }
                    }
                });
                self.mode_badge(&mut hui);
                if hui.available_width()
                    > Self::action_width(&hui, "Indicators…") + Self::action_width(&hui, "Compare…") + 12.
                {
                    clicked |= self.chart_actions(&mut hui);
                } else {
                    hui.menu_button("Chart…", |ui| {
                        if ui.button("Indicators…").clicked() {
                            self.indicator_editor = true;
                            ui.close();
                        }
                        if ui.button("Compare symbol…").clicked() {
                            self.compare_picker.open();
                            ui.close();
                        }
                    });
                }
            }
        }

        // Bottom quick-range row.
        let range_row = Rect::from_min_max(pos2(rect.left(), rect.bottom() - RANGE_H), rect.max);
        let mut rx = range_row.left() + 6.0;
        for r in Range::ALL {
            let lr = Rect::from_min_size(pos2(rx, range_row.top()), vec2(r.label().len() as f32 * 7.4 + 8.0, RANGE_H));
            let resp = ui.interact(lr, ui.id().with((self.id, "range", r.label())), Sense::click());
            painter.text(
                lr.center(),
                Align2::CENTER_CENTER,
                r.label(),
                mono(11.0),
                if resp.hovered() { amber() } else { muted() },
            );
            if resp.clicked() {
                self.set_range(r, w);
                clicked = true;
            }
            rx = lr.right();
        }
        let status = match (&self.error, self.loading, self.paging) {
            (Some(e), _, _) => format!("ERR {e}"),
            (_, true, _) => "loading…".into(),
            (_, _, true) => "loading history…".into(),
            _ => format!(
                "{} bars{}{}{} · {}{}",
                self.bars.len(),
                if self.cached_history { " · cached" } else { "" },
                if self.log { "  LOG" } else { "" },
                if self.magnet { "  MAGNET" } else { "" },
                self.data.zone.label(),
                if self.data.session == tr_core::session::Session::Provider {
                    ""
                } else if self.data.session == tr_core::session::Session::UsRegular {
                    " · US RTH"
                } else {
                    " · US EXT"
                }
            ),
        };
        painter.text(
            pos2(range_row.right() - 6.0, range_row.center().y),
            Align2::RIGHT_CENTER,
            status,
            mono(10.5),
            muted(),
        );

        let body = Rect::from_min_max(pos2(rect.left(), rect.top() + header_h), pos2(rect.right(), range_row.top()));
        let own_axis = self.chart_type != ChartType::Renko
            && self.compare.as_ref().is_some_and(|s| !self.data.hidden_comparisons.contains(s))
            && self.compare_axis == CompareAxis::Own;
        // An own-axis comparison gets a left-hand price axis of the same width.
        let left_axis = if own_axis { AXIS_W } else { 0.0 };
        let plot_all = Rect::from_min_max(
            pos2(body.left() + left_axis, body.top()),
            pos2(body.right() - AXIS_W, body.bottom() - TIME_H),
        );
        self.plot_width = plot_all.width().max(1.) as f64;
        if plot_all.width() < 40.0 || plot_all.height() < 40.0 {
            return clicked;
        }

        if self.chart_type == ChartType::HeikinAshi
            && !self.bars.is_empty()
            && self.ha_gpu.as_ref().is_none_or(|(bars, _)| !Arc::ptr_eq(bars, &self.bars))
        {
            self.ha_gpu = Some((self.bars.clone(), None));
            w.send(Request::Heikin { pane: self.id, generation: self.generation, bars: self.bars.clone() });
        }
        let display = if self.chart_type == ChartType::HeikinAshi {
            self.ha_gpu.as_ref().and_then(|(_, gpu)| gpu.clone()).or_else(|| self.gpu.clone())
        } else {
            self.gpu.clone()
        };
        let Some(gpu) = display else {
            let msg = self.error.clone().unwrap_or_else(|| format!("Loading {} {}…", self.symbol, self.tf));
            painter.text(plot_all.center(), Align2::CENTER_CENTER, msg, mono(13.0), amber_dim());
            if self.error.is_some() && self.chart_type == ChartType::Renko {
                ui.scope_builder(
                    UiBuilder::new()
                        .max_rect(Rect::from_min_size(plot_all.center() + vec2(-230., 24.), vec2(460., 60.))),
                    |ui| {
                        ui.horizontal(|ui| {
                            self.box_controls(ui);
                            if ui.button("Back to candles").clicked() {
                                self.chart_type = ChartType::Candles;
                            }
                        });
                    },
                );
            }
            return clicked;
        };

        // Split plot height between the price pane and oscillator sub-panes.
        let built_len = self.built.len();
        let groups = study_groups(&self.built);
        let pane_groups: Vec<_> = groups.iter().filter(|(key, _)| key.is_some()).take(MAX_SUB_PANES).collect();
        let desired: Vec<f32> = pane_groups
            .iter()
            .map(|(key, members)| {
                self.indicators
                    .iter()
                    .find_map(|s| s.chart.as_ref().filter(|c| Some(c.id) == *key).map(|c| c.height))
                    .unwrap_or_else(|| self.built[members[0]].spec.chart.as_ref().map_or(120.0, |c| c.height))
                    .clamp(40.0, 600.0)
            })
            .collect();
        let total: f32 = desired.iter().sum();
        let height_factor = if total > 0.0 { (plot_all.height() * 0.6 / total).min(1.0) } else { 1.0 };
        let heights: Vec<_> = desired.iter().map(|h| h * height_factor).collect();
        let price_rect =
            Rect::from_min_max(plot_all.min, pos2(plot_all.right(), plot_all.bottom() - heights.iter().sum::<f32>()));

        let len = self.bars.len();
        if let Some(g) = self.glide.as_mut() {
            let now = ui.input(|i| i.time);
            let t = crate::anim::since(ui.ctx(), now - *g.start.get_or_insert(now), GLIDE_SECS);
            self.viewport = Some(g.at(crate::anim::ease_out(t) as f64));
            if t >= 1.0 {
                self.glide = None;
            }
        }
        let v =
            self.viewport.get_or_insert_with(|| Viewport::fit_last(len, DEFAULT_VISIBLE, price_rect.width() as f64));
        v.set_width(price_rect.width() as f64);

        // ---- input ----
        let plot_resp = ui.interact(plot_all, ui.id().with((self.id, "plot")), Sense::click_and_drag());
        let legend_bar = ui
            .input(|i| i.pointer.hover_pos())
            .and_then(|p| v.bar_at((p.x - price_rect.left()) as f64, len))
            .unwrap_or(len.saturating_sub(1));
        let mut legend_panes = Vec::new();
        let mut legend_top = price_rect.bottom();
        for ((_, members), height) in pane_groups.iter().zip(&heights) {
            let rect =
                Rect::from_min_max(pos2(plot_all.left(), legend_top), pos2(plot_all.right(), legend_top + height));
            legend_top += height;
            for &index in members {
                legend_panes.push((rect, index));
            }
        }
        let legend_rows =
            crate::chart_legend::rows(ui, &self.indicators, &self.built, price_rect, &legend_panes, legend_bar);
        let over_legend =
            ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| legend_rows.iter().any(|row| row.rect.contains(p)));
        let legend_action = crate::chart_legend::interact(ui, self.id, &legend_rows);
        // Register splitters after the plot so their drag target wins hit-testing,
        // including the first mouse-down when arriving from outside the chart.
        let mut divider_dragging = false;
        let mut top = price_rect.bottom();
        for ((key, _), height) in pane_groups.iter().zip(&heights) {
            let hit = Rect::from_min_max(pos2(plot_all.left(), top - 3.0), pos2(plot_all.right(), top + 3.0));
            let divider = ui.interact(hit, ui.id().with((self.id, "study-divider", key)), Sense::drag());
            if divider.hovered() || divider.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
            }
            if divider.dragged() {
                divider_dragging = true;
                if let Some(c) =
                    self.indicators.iter_mut().filter_map(|s| s.chart.as_mut()).find(|c| Some(c.id) == *key)
                {
                    c.height = (c.height - divider.drag_delta().y / height_factor.max(0.01)).clamp(40.0, 600.0);
                }
                self.request_indicators(w);
            }
            top += height;
        }
        let axis_rect =
            Rect::from_min_max(pos2(price_rect.right(), price_rect.top()), pos2(body.right(), price_rect.bottom()));
        let axis_resp = ui.interact(axis_rect, ui.id().with((self.id, "paxis")), Sense::click_and_drag());
        let time_rect =
            Rect::from_min_max(pos2(plot_all.left(), plot_all.bottom()), pos2(plot_all.right(), body.bottom()));
        let time_resp = ui.interact(time_rect, ui.id().with((self.id, "taxis")), Sense::click_and_drag());
        if plot_resp.clicked() || plot_resp.drag_started() || axis_resp.clicked() || time_resp.clicked() {
            clicked = true;
        }
        let hover = plot_resp.hover_pos().filter(|p| self.hover_enabled && !over_legend && plot_all.contains(*p));

        if plot_resp.hovered() && !over_legend || axis_resp.hovered() || time_resp.hovered() {
            let (scroll, zoom_delta) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            if scroll != egui::Vec2::ZERO || zoom_delta != 1.0 {
                self.glide = None;
            }
            let v = self.viewport.as_mut().expect("viewport");
            if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                let anchor = (p.x - price_rect.left()).clamp(0.0, price_rect.width()) as f64;
                if scroll.y != 0.0 {
                    v.zoom((scroll.y as f64 * 0.003).exp(), anchor);
                }
                if zoom_delta != 1.0 {
                    v.zoom(zoom_delta as f64, anchor);
                }
            }
            if scroll.x != 0.0 {
                v.pan_px(scroll.x as f64);
            }
        }
        if (self.draw.tool.is_none() && plot_resp.dragged()) || time_resp.dragged() {
            self.glide = None;
        }
        if self.draw.tool.is_none()
            && plot_resp.dragged()
            && !divider_dragging
            && !over_legend
            && self.draw.drag.is_none()
            && self.draw.hover.is_none()
        {
            let d = plot_resp.drag_delta();
            self.viewport.as_mut().expect("viewport").pan_px(d.x as f64);
            if let Some(s) = self.manual_scale.as_mut() {
                s.pan_px(d.y as f64);
            }
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
        if time_resp.dragged() {
            let v = self.viewport.as_mut().expect("viewport");
            let w_px = v.width_px;
            v.zoom((-time_resp.drag_delta().x as f64 * 0.005).exp(), w_px);
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }
        if time_resp.double_clicked() || plot_resp.double_clicked() && self.draw.tool.is_none() {
            self.reset_view();
        }
        let v = *self.viewport.as_ref().expect("viewport");
        let mut v = v;
        // Replay truncates the displayed prefix, not the user's zoom range.
        v.clamp(if self.replay_cutoff.is_some() { self.source_bars.len() } else { len });
        self.viewport = Some(v);

        let changed_by_user = plot_resp.dragged()
            || time_resp.dragged()
            || ui.input(|i| i.smooth_scroll_delta != egui::Vec2::ZERO || i.zoom_delta() != 1.) && plot_resp.hovered()
            || self.glide.is_some();
        if self.navigation.range && changed_by_user {
            self.range_event = crate::chart_navigation::date_range(&self.bars, self.tf, &v);
        }

        // Page older history when the left edge approaches loaded data.
        if self.replay_cutoff.is_none()
            && self.chart_type != ChartType::Renko
            && !self.paging
            && !self.loading
            && !self.exhausted
            && v.left() < v.visible_bars() * 0.3
        {
            self.paging = true;
            w.send(Request::MoreHistory {
                pane: self.id,
                generation: self.generation,
                symbol: self.symbol.clone(),
                tf: self.tf,
                options: self.effective_history(),
                existing: self.source_bars.clone(),
                limit: initial_limit(self.tf),
            });
        }

        // ---- price pane GPU frame ----
        let mut overlays: Vec<Overlay> = groups
            .iter()
            .filter(|(key, _)| key.is_none())
            .flat_map(|(_, members)| members.iter().map(|&i| &self.built[i]))
            .filter(|b| b.spec.chart.as_ref().is_none_or(|c| c.scale == StudyScale::Shared))
            .flat_map(|b| b.overlays.iter().map(|o| o.1.clone()).chain(b.bounds.iter().cloned()))
            .collect();
        // Percent mode labels prices relative to the main close at the visible left edge, where
        // the comparison is rebased too, so both series read as % change from the same bar.
        let mut percent_base = None;
        self.analytics.update(&self.bars, self.data.analytics.anchor);
        if let Some(vwap) = &self.analytics.vwap {
            let mut overlay = compare_overlay(vwap, 1.);
            overlay.color = theme::rgba(theme::text());
            overlays.push(overlay);
        }
        let (lo, hi) = v.visible_range(len);
        let primary_visible = self.chart_type != ChartType::Renko
            && self.compare.as_ref().is_some_and(|s| !self.data.hidden_comparisons.contains(s));
        let mut lines: Vec<LineRef> = self
            .data
            .comparisons
            .iter()
            .filter(|_| self.chart_type != ChartType::Renko)
            .filter(|s| !self.data.hidden_comparisons.contains(s))
            .filter_map(|s| self.comparisons.get(s).and_then(|s| s.line.clone()))
            .collect();
        if primary_visible && let Some(line) = &self.compare_line {
            lines.push(line.clone());
        }
        let anchor = crate::chart_compare::common_anchor(&gpu.closes, &lines, lo, hi);
        self.compare_base = anchor.map(|i| gpu.closes.pyramid.data[i][1] as f64);
        if matches!(self.compare_axis, CompareAxis::Percent | CompareAxis::Indexed) {
            percent_base = anchor.map(|i| gpu.closes.pyramid.data[i][1] as f64);
        }
        if let Some(c) = &self.compare_line {
            let scale =
                anchor.map(|i| gpu.closes.pyramid.data[i][1] as f64 / c.pyramid.data[i][1] as f64).unwrap_or(1.);
            self.compare_scale = scale;
            if self.compare_axis != CompareAxis::Own && primary_visible && anchor.is_some() {
                let mut overlay = compare_overlay(c, scale);
                overlay.color = theme::rgba(self.primary_compare_color());
                overlays.push(overlay);
            }
        }
        for key in &self.data.comparisons {
            if self.chart_type == ChartType::Renko || self.data.hidden_comparisons.contains(key) {
                continue;
            }
            if let Some(extra) = self.comparisons.get_mut(key)
                && let (Some(line), Some(at)) = (&extra.line, anchor)
            {
                extra.factor = gpu.closes.pyramid.data[at][1] as f64 / line.pyramid.data[at][1] as f64;
                let mut overlay = compare_overlay(line, extra.factor);
                overlay.color = theme::rgba(theme::compare(crate::chart_compare::slot(&self.data.compare_slots, key)));
                overlays.push(overlay);
            }
        }
        let reveal_t =
            crate::anim::progress(ui.ctx(), egui::Id::new(("chart-reveal", self.id)), self.reveal_key, REVEAL_SECS);
        let reveal = Reveal { opacity: ui.opacity(), ..Reveal::sweep(reveal_t, v.left(), v.visible_bars()) };
        let style = Style {
            baseline: self
                .data
                .analytics
                .baseline
                .unwrap_or_else(|| self.bars.get(v.visible_range(len).0).map_or(0., |b| b.close))
                as f32,
            up: theme::rgba(theme::candle_up()),
            down: theme::rgba(theme::candle_down()),
            color: theme::rgba(amber()),
            pixels_per_point: ui.ctx().pixels_per_point(),
            reveal,
            ..Style::default()
        };
        let mut pane = PricePane {
            series: &gpu,
            chart_type: self.chart_type,
            show_volume: self.show_volume,
            overlays: &overlays,
            viewport: &v,
            height_px: price_rect.height() as f64,
            log: self.log,
            manual_scale: self.manual_scale.map(|mut s| {
                s.height_px = price_rect.height() as f64;
                s
            }),
            style,
        };
        pane.manual_scale = Some(match pane.manual_scale {
            Some(s) => {
                let (a, b) = s.bounds_t();
                self.shown_scale = Some((a, b, s.log));
                s
            }
            None => self.ease_scale(ui.ctx(), scene::autoscale(&pane)),
        });
        let mut out = scene::price_pane(&pane);
        let mut scale = out.scale;
        scale.height_px = price_rect.height() as f64;
        if self.draw.tool.is_none() && !over_legend {
            if plot_resp.secondary_clicked() {
                self.context_timestamp = plot_resp
                    .interact_pointer_pos()
                    .filter(|p| price_rect.contains(*p))
                    .and_then(|p| v.bar_at((p.x - price_rect.left()) as f64, len))
                    .and_then(|i| self.bars.get(i))
                    .map(|b| b.ts);
                self.context_price = plot_resp
                    .interact_pointer_pos()
                    .filter(|p| price_rect.contains(*p))
                    .map(|p| {
                        let price = scale.price_at((p.y - price_rect.top()) as f64);
                        // Seed the editor at the same precision as the chart cursor.
                        format!("{:.*}", self.sec.decimals(price), price).parse().unwrap_or(price)
                    })
                    .filter(|v| v.is_finite());
            }
            plot_resp.context_menu(|ui| self.context_menu(ui, w));
        }
        if self.built.len() != built_len {
            ui.ctx().request_repaint();
            return clicked;
        }
        if axis_resp.dragged() {
            let mut s = self.manual_scale.unwrap_or(scale);
            s.stretch((axis_resp.drag_delta().y as f64 * -0.01).exp());
            self.manual_scale = Some(s);
            ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
        }
        if axis_resp.double_clicked() {
            self.manual_scale = None;
        }

        // Grid first (under the GPU geometry is not possible inside one pass, so draw it
        // faintly; candles are opaque on top visually).
        let price_painter = ui.painter_at(price_rect);
        let us = matches!(self.sec.class, AssetClass::Equity | AssetClass::Etf);
        self.data.context.shade(ui, price_rect, &v, &self.bars, self.tf, us);
        let tick_count = (price_rect.height() / 42.0).max(2.0) as usize;
        let (ticks, tick_labels): (Vec<f64>, Vec<String>) = match percent_base {
            Some(base) => {
                let pct = scale::price_ticks(percent(scale.lo, base), percent(scale.hi, base), tick_count);
                let step = if pct.len() > 1 { pct[1] - pct[0] } else { 1.0 };
                let dec = scale::decimals_for(step).min(2);
                pct.into_iter()
                    .filter(|p| *p > -100.0)
                    .map(|p| (base * (1.0 + p / 100.0), axis_value(p, dec, self.compare_axis)))
                    .unzip()
            }
            None => {
                let ticks = scale::price_ticks(scale.lo, scale.hi, tick_count);
                let step = if ticks.len() > 1 { ticks[1] - ticks[0] } else { 1.0 };
                let dec = scale::decimals_for(step).max(self.sec.decimals(scale.hi).min(3));
                let labels = ticks.iter().map(|t| scale::format_price(*t, dec)).collect();
                (ticks, labels)
            }
        };
        for t in &ticks {
            let y = price_rect.top() + scale.y_of(*t) as f32;
            price_painter.hline(price_rect.x_range(), y, Stroke::new(1.0, theme::grid()));
        }
        let frame_no = ui.ctx().cumulative_frame_nr();
        // Independent overlays use their own transform without changing price autoscale.
        let mut price_study_scales = Vec::new();
        if let Some((_, members)) = groups.iter().find(|(key, _)| key.is_none()) {
            for &index in members {
                let b = &self.built[index];
                if b.spec.chart.as_ref().is_some_and(|c| c.scale != StudyScale::Shared) {
                    let fixed = study_fixed_range(&b.spec);
                    let ovs: Vec<_> = b.overlays.iter().map(|o| o.1.clone()).chain(b.bounds.iter().cloned()).collect();
                    let separate = scene::sub_pane(&ovs, &v, price_rect.height() as f64, fixed, style);
                    out.cmds.extend(separate.cmds);
                    price_study_scales.push((price_rect, index, separate.scale));
                }
            }
        }
        let mut own_scale = None;
        if own_axis && let Some(c) = &self.compare_line {
            let mut overlay = compare_overlay(c, 1.0);
            overlay.color = theme::rgba(self.primary_compare_color());
            let separate = scene::sub_pane(&[overlay], &v, price_rect.height() as f64, None, style);
            out.cmds.extend(separate.cmds);
            own_scale = Some(separate.scale);
        }
        let frame = Frame { pane: self.id * 8, frame_no, cmds: out.cmds };
        if let Some((_, members)) = groups.iter().find(|(key, _)| key.is_none()) {
            for &index in members {
                let study_scale = price_study_scales.iter().find(|(_, i, _)| *i == index).map_or(scale, |(_, _, s)| *s);
                if let Some(fill) = &self.built[index].fill {
                    fill.paint(ui, price_rect, &v, &study_scale);
                }
            }
        }
        ui.painter().add(tr_chart::egui_wgpu::Callback::new_paint_callback(price_rect, ChartCallback(Arc::new(frame))));

        // Sub panes.
        let mut sub_rects = Vec::new();
        let mut top = price_rect.bottom();
        for (k, ((key, members), &height)) in pane_groups.iter().zip(&heights).enumerate() {
            let r = Rect::from_min_max(pos2(plot_all.left(), top), pos2(plot_all.right(), top + height));
            top += height;
            let b = self
                .built
                .iter()
                .find(|b| b.spec.chart.as_ref().is_some_and(|c| Some(c.id) == *key))
                .unwrap_or(&self.built[members[0]]);
            let def = b.spec.def();
            let ovs: Vec<Overlay> = members
                .iter()
                .filter(|&&i| self.built[i].spec.chart.as_ref().is_none_or(|c| c.scale == StudyScale::Shared))
                .flat_map(|&i| {
                    self.built[i].overlays.iter().map(|o| o.1.clone()).chain(self.built[i].bounds.iter().cloned())
                })
                .collect();
            let mut s = style;
            s.thickness_px = 1.3;
            let mut so = scene::sub_pane(&ovs, &v, r.height() as f64, study_fixed_range(&b.spec), s);
            for &bi in members {
                let b = &self.built[bi];
                let scale = if b.spec.chart.as_ref().is_some_and(|c| c.scale != StudyScale::Shared) {
                    let ovs: Vec<_> = b.overlays.iter().map(|o| o.1.clone()).chain(b.bounds.iter().cloned()).collect();
                    let separate = scene::sub_pane(&ovs, &v, r.height() as f64, study_fixed_range(&b.spec), s);
                    so.cmds.extend(separate.cmds);
                    separate.scale
                } else {
                    so.scale
                };
                sub_rects.push((r, bi, scale));
            }
            let p = ui.painter_at(r);
            p.hline(r.x_range(), r.top(), Stroke::new(1.0, theme::border()));
            for &lvl in def.map_or(&[][..], |d| d.levels) {
                let y = r.top() + so.scale.y_of(lvl) as f32;
                if r.y_range().contains(y) {
                    p.hline(r.x_range(), y, Stroke::new(1.0, muted().gamma_multiply(0.35)));
                }
            }
            let frame = Frame { pane: self.id * 8 + 1 + k as u64, frame_no, cmds: so.cmds };
            for (rect, index, scale) in sub_rects.iter().filter(|(rect, _, _)| *rect == r) {
                if let Some(fill) = &self.built[*index].fill {
                    fill.paint(ui, *rect, &v, scale);
                }
            }
            ui.painter().add(tr_chart::egui_wgpu::Callback::new_paint_callback(r, ChartCallback(Arc::new(frame))));
        }
        sub_rects.extend(price_study_scales);

        // ---- axes ----
        let axis_painter = ui.painter_at(Rect::from_min_max(pos2(plot_all.right(), body.top()), body.max));
        axis_painter.vline(plot_all.right(), body.y_range(), Stroke::new(1.0, theme::border()));
        let positive_prices = self.bars.last().is_some_and(|b| b.low > 0.0);
        for (t, label) in ticks.iter().zip(&tick_labels) {
            let y = price_rect.top() + scale.y_of(*t) as f32;
            // The volume headroom can dip below zero; don't label impossible prices.
            if positive_prices && *t < 0.0 {
                continue;
            }
            if y > price_rect.top() + 6.0 && y < price_rect.bottom() - 6.0 {
                axis_painter.text(pos2(plot_all.right() + 6.0, y), Align2::LEFT_CENTER, label, mono(11.0), muted());
            }
        }
        let left_painter = ui.painter_at(Rect::from_min_max(body.min, pos2(plot_all.left(), body.bottom())));
        if let Some(s) = own_scale {
            left_painter.vline(plot_all.left(), body.y_range(), Stroke::new(1.0, theme::border()));
            let ticks = scale::price_ticks(s.lo, s.hi, tick_count);
            let step = if ticks.len() > 1 { ticks[1] - ticks[0] } else { 1.0 };
            let dec = scale::decimals_for(step).min(4);
            for t in ticks {
                let y = price_rect.top() + s.y_of(t) as f32;
                if y > price_rect.top() + 6.0 && y < price_rect.bottom() - 6.0 {
                    left_painter.text(
                        pos2(plot_all.left() - 6.0, y),
                        Align2::RIGHT_CENTER,
                        scale::format_price(t, dec),
                        mono(11.0),
                        self.primary_compare_color().gamma_multiply(0.8),
                    );
                }
            }
        }
        for (at, (r, bi, s)) in sub_rects.iter().enumerate() {
            let row = sub_rects[..at].iter().filter(|(other, _, _)| other == r).count();
            let separate = self.built[*bi].spec.chart.as_ref().is_some_and(|c| c.scale != StudyScale::Shared);
            let color = self.built[*bi].overlays.first().map_or(muted(), |(_, o)| opaque(o));
            if row > 0 && !separate {
                continue;
            }
            for t in scale::price_ticks(s.lo, s.hi, 3) {
                let y = r.top() + s.y_of(t) as f32;
                if y > r.top() + 8.0 && y < r.bottom() - 4.0 {
                    let (painter, x, align) = if separate {
                        (ui.painter_at(*r), r.left() + 50.0 + row as f32 * 52.0, Align2::RIGHT_CENTER)
                    } else {
                        (axis_painter.clone(), plot_all.right() + 6.0, Align2::LEFT_CENTER)
                    };
                    painter.text(pos2(x, y), align, scale::format_value(t), mono(10.5), color);
                }
            }
        }
        // Time axis labels (sampled so 1M visible bars stays cheap).
        let (lo, hi) = v.visible_range(len);
        let time_painter = ui.painter_at(time_rect);
        time_painter.hline(time_rect.x_range(), time_rect.top(), Stroke::new(1.0, theme::border()));
        if hi > lo {
            let stride = ((hi - lo) / 600).max(1);
            let idxs: Vec<usize> = (lo..hi).step_by(stride).collect();
            let times: Vec<i64> =
                idxs.iter().map(|&i| self.data.zone.label_timestamp(self.bars[i].ts, self.tf)).collect();
            let gap = 90.0 / (v.bar_px * stride as f64);
            for (j, label) in scale::time_ticks(&times, 0, times.len(), gap, self.tf) {
                let x = price_rect.left() + v.x_center(idxs[j]) as f32;
                ui.painter_at(plot_all).vline(x, plot_all.y_range(), Stroke::new(1.0, theme::grid()));
                time_painter.text(pos2(x, time_rect.center().y), Align2::CENTER_CENTER, label, mono(10.5), muted());
            }
        }

        // Last price line + axis tag, fading in as the sweep reaches the last bar.
        let tag_fade = crate::anim::span(reveal_t, 0.8, 1.0);
        let mut right_tags = Vec::new();
        let mut fixed_tag = None;
        if let Some(last) = self.bars.last()
            && tag_fade > 0.0
        {
            if self.chart_type != ChartType::Renko && self.compare_axis != CompareAxis::Own && anchor.is_some() {
                for key in &self.data.comparisons {
                    if self.data.hidden_comparisons.contains(key) {
                        continue;
                    }
                    let Some(extra) = self.comparisons.get(key) else { continue };
                    let Some(raw) = extra.line.as_ref().and_then(last_finite) else { continue };
                    let rebased = raw * extra.factor;
                    let y = price_rect.top() + scale.y_of(rebased) as f32;
                    if !price_rect.y_range().contains(y) {
                        continue;
                    }
                    let decimals = scale::decimals_for(raw.abs() / 1000.0).clamp(2, 4);
                    let text = match percent_base {
                        Some(base) => axis_value(percent(rebased, base), 2, self.compare_axis),
                        None => scale::format_price(raw, decimals),
                    };
                    right_tags.push((y, self.compare_color(key).gamma_multiply(tag_fade), text));
                }
            }
            // The comparison's last value: on the right axis when it shares it, else on the left one.
            if let Some(c) = &self.compare_line
                && let Some(raw) = last_finite(c)
                && primary_visible
                && (own_axis || anchor.is_some())
            {
                let color = self.primary_compare_color().gamma_multiply(tag_fade);
                let ink = Color32::BLACK.gamma_multiply(tag_fade);
                let decimals = scale::decimals_for(raw.abs() / 1000.0).clamp(2, 4);
                match own_scale {
                    Some(s) => {
                        let y = price_rect.top() + s.y_of(raw) as f32;
                        if price_rect.y_range().contains(y) {
                            let tag = Rect::from_min_size(pos2(body.left() + 1.0, y - 8.0), vec2(AXIS_W - 2.0, 16.0));
                            left_painter.rect_filled(tag, 0.0, color);
                            left_painter.text(
                                tag.right_center() - vec2(5.0, 0.0),
                                Align2::RIGHT_CENTER,
                                scale::format_price(raw, decimals),
                                mono(11.0),
                                ink,
                            );
                        }
                    }
                    None => {
                        let rebased = raw * self.compare_scale;
                        let y = price_rect.top() + scale.y_of(rebased) as f32;
                        if price_rect.y_range().contains(y) {
                            let text = match percent_base {
                                Some(base) => axis_value(percent(rebased, base), 2, self.compare_axis),
                                None => scale::format_price(raw, decimals),
                            };
                            right_tags.push((y, color, text));
                        }
                    }
                }
            }
            let y = price_rect.top() + scale.y_of(last.close) as f32;
            if price_rect.y_range().contains(y) {
                let col = if last.is_up() { theme::up() } else { theme::down() };
                let mut xx = price_rect.left();
                while xx < price_rect.right() {
                    price_painter.hline(
                        xx..=(xx + 3.0).min(price_rect.right()),
                        y,
                        Stroke::new(1.0, col.gamma_multiply(0.6 * tag_fade)),
                    );
                    xx += 6.0;
                }
                let text = match percent_base {
                    Some(base) => axis_value(percent(last.close, base), 2, self.compare_axis),
                    None => scale::format_price(last.close, self.sec.decimals(last.close)),
                };
                fixed_tag = Some(right_tags.len());
                right_tags.push((y, col.gamma_multiply(tag_fade), text));
            }
        }

        let desired: Vec<_> = right_tags.iter().map(|(y, _, _)| (*y, 16.)).collect();
        let positions = spread_tags(&desired, fixed_tag, price_rect.top(), price_rect.bottom());
        for ((true_y, color, text), y) in right_tags.into_iter().zip(positions) {
            let tag = Rect::from_min_size(pos2(plot_all.right() + 1., y - 8.), vec2(AXIS_W - 2., 16.));
            if (y - true_y).abs() > 3. {
                let x = tag.left() + 2.;
                axis_painter.line_segment([pos2(x, true_y), pos2(x + 3., y)], Stroke::new(1., color));
            }
            axis_painter.rect_filled(tag, 0., color);
            axis_painter.text(
                tag.left_center() + vec2(5., 0.),
                Align2::LEFT_CENTER,
                text,
                mono(11.),
                Color32::BLACK.gamma_multiply(tag_fade),
            );
        }

        if self.replay_cutoff.is_none() && self.chart_type != ChartType::Renko {
            self.paint_price_alerts(ui, price_rect, &scale);
        }
        self.data.analytics.paint_profile(ui, price_rect, &v, &self.bars, (self.tf, us), &scale);
        self.data.context.paint(
            ui,
            price_rect,
            &scale,
            &self.bars,
            (
                self.tf,
                us,
                self.effective_session(),
                self.replay_cutoff.is_none() && self.chart_type != ChartType::Renko,
            ),
        );
        if self.data.show_events && self.replay_cutoff.is_none() && self.chart_type != ChartType::Renko {
            crate::chart_events::paint(ui, price_rect, &v, &self.bars, self.tf, &self.events);
        }
        if self.replay_cutoff.is_none()
            && self.chart_type != ChartType::Renko
            && (self.data.account.positions || self.data.account.orders)
        {
            self.account_overlay.paint(ui, price_rect, &scale, self.bars.last().map(|b| b.close));
        }

        // ---- drawings ----
        let drawing_response = if over_legend {
            ui.interact(Rect::NOTHING, ui.id().with((self.id, "legend-blocks-drawing")), Sense::hover())
        } else {
            plot_resp.clone()
        };
        if self.chart_type != ChartType::Renko && self.replay_cutoff.is_none() {
            self.paint_drawings(ui, price_rect, &v, &scale, hover, &drawing_response);
        }

        // ---- congress markers ----
        self.hover_marker = None;
        if self.show_congress && self.replay_cutoff.is_none() && self.chart_type != ChartType::Renko {
            self.paint_markers(ui, price_rect, &v, &scale, hover, &reveal);
        }

        // ---- crosshair + legend ----
        let local_bar = hover.and_then(|p| {
            let index = v.index_at((p.x - price_rect.left()) as f64);
            (index >= 0. && index < len as f64).then_some(index.floor() as usize)
        });
        self.local_cursor = local_bar.map(|i| self.bars[i].ts);
        if self.navigation.cursor && self.local_cursor.is_some() {
            ui.ctx().request_repaint();
        }
        let linked_bar = self
            .navigation
            .cursor
            .then_some(self.shared_cursor)
            .flatten()
            .and_then(|ts| crate::chart_navigation::index_at(&self.bars, self.tf, ts));
        let hovered_bar = local_bar.or(linked_bar);
        if hover.is_none()
            && let Some(i) = linked_bar
        {
            let x = price_rect.left() + v.x_center(i) as f32;
            if plot_all.x_range().contains(x) {
                dashed_v(&ui.painter_at(plot_all), x, plot_all.y_range(), Stroke::new(1., Color32::from_gray(110)));
            }
        }
        if let Some(p) = hover {
            let ch = Stroke::new(1.0, Color32::from_gray(110));
            let plot_painter = ui.painter_at(plot_all);
            let xi = hovered_bar.map(|i| price_rect.left() + v.x_center(i) as f32).unwrap_or(p.x);
            dashed_v(&plot_painter, xi, plot_all.y_range(), ch);
            dashed_h(&plot_painter, plot_all.x_range(), p.y, ch);
            if price_rect.contains(p) {
                let price = scale.price_at((p.y - price_rect.top()) as f64);
                let tag = Rect::from_min_size(pos2(plot_all.right() + 1.0, p.y - 8.0), vec2(AXIS_W - 2.0, 16.0));
                axis_painter.rect_filled(tag, 0.0, Color32::from_gray(70));
                let label = match percent_base {
                    Some(base) => axis_value(percent(price, base), 2, self.compare_axis),
                    None => scale::format_price(price, self.sec.decimals(price)),
                };
                axis_painter.text(tag.left_center() + vec2(5.0, 0.0), Align2::LEFT_CENTER, label, mono(11.0), text());
                if let Some(s) = own_scale {
                    let own = s.price_at((p.y - price_rect.top()) as f64);
                    let tag = Rect::from_min_size(pos2(body.left() + 1.0, p.y - 8.0), vec2(AXIS_W - 2.0, 16.0));
                    left_painter.rect_filled(tag, 0.0, Color32::from_gray(70));
                    let decimals = scale::decimals_for(own.abs() / 1000.0).clamp(2, 4);
                    left_painter.text(
                        tag.right_center() - vec2(5.0, 0.0),
                        Align2::RIGHT_CENTER,
                        scale::format_price(own, decimals),
                        mono(11.0),
                        text(),
                    );
                }
            }
            if let Some(i) = hovered_bar {
                let label = scale::crosshair_label(self.data.zone.label_timestamp(self.bars[i].ts, self.tf), self.tf);
                let gw = label.len() as f32 * 7.0 + 10.0;
                let tag = Rect::from_center_size(pos2(xi, time_rect.center().y), vec2(gw, TIME_H));
                time_painter.rect_filled(tag, 0.0, Color32::from_gray(70));
                time_painter.text(tag.center(), Align2::CENTER_CENTER, label, mono(10.5), text());
            }
            if self.draw.tool.is_some() {
                ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
            }
        }
        self.paint_legend(ui, price_rect, hovered_bar.unwrap_or(len.saturating_sub(1)), &legend_rows);
        if matches!(self.sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf) {
            clicked |= self.trade_buttons(ui, plot_all);
        }
        self.paint_live_badge(ui, price_rect);
        if let Some(action) = legend_action {
            self.apply_legend_action(action, w);
        }

        let _ = focused; // the surrounding card shows focus
        clicked
    }

    /// Buy / Sell buttons in the plot's top-right corner: they open the order ticket.
    fn apply_legend_action(&mut self, action: crate::chart_legend::Action, w: &Worker) {
        use crate::chart_legend::Action;
        let id = match action {
            Action::Visibility(id, _) | Action::Edit(id) | Action::Remove(id) => id,
        };
        let Some(index) = self.indicators.iter().position(|s| s.chart.as_ref().is_some_and(|c| c.id == id)) else {
            return;
        };
        match action {
            Action::Edit(_) => {
                self.indicator_editor = true;
                self.indicator_focus = Some(id);
            }
            Action::Visibility(_, visible) => {
                self.indicators[index].chart.as_mut().unwrap().visible = visible;
                self.request_indicators(w);
            }
            Action::Remove(_) => {
                self.remove_study(index);
                self.request_indicators(w);
            }
        }
    }

    fn paint_live_badge(&self, ui: &Ui, plot: Rect) {
        if self.replay_cutoff.is_none()
            && self.chart_type != ChartType::Renko
            && plot.width() > 180.
            && let Some(notice) = &self.live_notice
        {
            let painter = ui.painter_at(plot);
            let galley = painter.layout_no_wrap("STREAM WAITING".into(), ui_font(10.), amber());
            let badge = live_badge_rect(plot, galley.size().x);
            painter.rect_filled(badge, 2., theme::card());
            painter.galley(badge.left_center() + vec2(6., -galley.size().y / 2.), galley, amber());
            ui.interact(badge, ui.id().with((self.id, "stream-status")), Sense::hover()).on_hover_text(notice);
            return;
        }
        if self.replay_cutoff.is_some()
            || self.chart_type == ChartType::Renko
            || !self.last_live.is_some_and(|t| t.elapsed().as_secs() < 5)
            || plot.width() < 60.
            || plot.height() < 50.
        {
            return;
        }
        let painter = ui.painter_at(plot);
        let galley = painter.layout_no_wrap("LIVE".into(), ui_font(10.), theme::up());
        let badge = live_badge_rect(plot, galley.size().x);
        painter.rect_filled(badge, 2., theme::card());
        let pulse = 0.6 + 0.4 * ((ui.input(|i| i.time) * 3.).sin() as f32).abs();
        painter.circle_filled(badge.left_center() + vec2(6., 0.), 2.5, theme::up().gamma_multiply(pulse));
        painter.galley(badge.left_center() + vec2(12., -galley.size().y / 2.), galley, theme::up());
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }

    fn trade_buttons(&mut self, ui: &mut Ui, plot: Rect) -> bool {
        let h = 20.0;
        let mut x = plot.right() - 8.0;
        let mut clicked = false;
        for (label, cmd, color, tip) in
            [("SELL", "SELL", theme::down(), "Sell (⇧S)"), ("BUY", "BUY", theme::up(), "Buy (⇧B)")]
        {
            let g = ui.painter().layout_no_wrap(label.into(), mono(11.0), Color32::WHITE);
            let w = g.size().x + 18.0;
            let r = Rect::from_min_size(pos2(x - w, plot.top() + 6.0), vec2(w, h));
            x -= w + 6.0;
            let resp = ui.interact(r, ui.id().with((self.id, "trade", label)), Sense::click()).on_hover_text(tip);
            let fill = if resp.hovered() { color } else { color.gamma_multiply(0.8) };
            let p = ui.painter();
            p.rect_filled(r, h / 2.0, fill);
            p.galley(r.center() - g.size() / 2.0, g, Color32::WHITE);
            if resp.clicked() {
                self.actions.push(Action::Command(format!("{cmd} {}", self.symbol)));
                clicked = true;
            }
        }
        clicked
    }

    fn show_indicator_editor(&mut self, ctx: &egui::Context, w: &Worker) {
        if !self.indicator_editor {
            return;
        }
        let mut open = true;
        let focus = self.indicator_focus.take();
        let before = self.indicators.clone();
        let mut remove = None;
        let mut reorder = None;
        let mut save_template = false;
        let mut apply_template = None;
        if self.template_names.is_none() {
            match crate::chart_templates::names(&crate::chart_templates::directory()) {
                Ok(names) => self.template_names = Some(names),
                Err(error) => {
                    self.template_names = Some(Vec::new());
                    self.template_status = Some(Err(error));
                }
            }
        }
        let field_w = theme::dp(220.0);
        let content = ctx.content_rect();
        egui::Window::new(RichText::new(format!("Indicators · {}", self.sec.ticker)).font(ui_font(13.0)))
            .id(egui::Id::new((self.id, "indicator-editor")))
            .open(&mut open)
            .default_width(theme::dp(380.0))
            .default_height(theme::dp(560.0))
            .default_pos(content.left_top() + egui::vec2(theme::dp(72.0), theme::dp(72.0)))
            // Keep the title bar clear of the macOS window buttons.
            .constrain_to(content.with_min_y(content.top() + theme::dp(34.0)))
            .show(ctx, |ui| {
                egui::ComboBox::from_id_salt("add-study")
                    .selected_text("Add indicator…")
                    .width(ui.available_width())
                    .height(theme::dp(320.0))
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                    .show_ui(ui, |ui| {
                        let search = ui.add(
                            egui::TextEdit::singleline(&mut self.indicator_search)
                                .hint_text("Search by name or code")
                                .desired_width(f32::INFINITY),
                        );
                        if ui.memory(|m| m.focused().is_none()) {
                            search.request_focus();
                        }
                        ui.separator();
                        let query = self.indicator_search.to_ascii_lowercase();
                        let matches: Vec<_> = registry::all()
                            .iter()
                            .filter(|d| {
                                query.is_empty()
                                    || d.name.to_ascii_lowercase().contains(&query)
                                    || d.code.to_ascii_lowercase().contains(&query)
                            })
                            .collect();
                        if matches.is_empty() {
                            ui.label(RichText::new(format!("No indicator matches “{}”", self.indicator_search)).color(muted()));
                        }
                        let code_w = matches.iter().map(|d| d.code.len()).max().unwrap_or(0);
                        for def in matches {
                            let mut job = egui::text::LayoutJob::default();
                            job.append(&format!("{:<code_w$}  ", def.code), 0.0, egui::TextFormat::simple(mono(12.0), amber()));
                            job.append(def.name, 0.0, egui::TextFormat::simple(ui_font(13.0), text()));
                            if ui.add(egui::Button::selectable(false, job)).clicked() {
                                self.indicators.push(IndicatorSpec::of(def));
                                self.indicator_search.clear();
                                ui.close();
                            }
                        }
                    });
                ui.add_space(theme::dp(4.0));
                if before.is_empty() {
                    ui.add_space(theme::dp(16.0));
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("No indicators on this chart yet").color(text()));
                        ui.label(
                            RichText::new("Add one above, or apply a saved template below.").small().color(muted()),
                        );
                    });
                    ui.add_space(theme::dp(16.0));
                }
                let count = self.indicators.len();
                egui::ScrollArea::vertical()
                    .id_salt("study-list")
                    .max_height(theme::dp(470.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                    for (index, spec) in self.indicators.iter_mut().enumerate() {
                        let Some(c) = &mut spec.chart else { continue };
                        ui.push_id(c.id, |ui| {
                            let error = self
                                .built
                                .iter()
                                .find(|b| b.spec.chart.as_ref().is_some_and(|s| s.id == c.id))
                                .and_then(|b| b.error.clone());
                            let def = registry::find(&spec.code);
                            let default_rgb: [u8; 3] = theme::series(c.id.saturating_sub(1) as usize).to_array()[..3].try_into().unwrap();
                            let swatch = c.color.unwrap_or(default_rgb);
                            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
                                ui.ctx(),
                                ui.make_persistent_id("study"),
                                true,
                            );
                            if focus==Some(c.id) {
                                state.set_open(true);
                                ui.scroll_to_rect(Rect::from_min_size(ui.cursor().min,vec2(10.,24.)),Some(egui::Align::TOP));
                            }
                            state
                                .show_header(ui, |ui| {
                                    ui.checkbox(&mut c.visible, "").on_hover_text(
                                        "Show on chart. Hidden studies still feed indicators calculated from them.",
                                    );
                                    let dim = if c.visible { 1.0 } else { 0.45 };
                                    let (r, _) = ui.allocate_exact_size(
                                        egui::vec2(theme::dp(10.0), theme::dp(10.0)),
                                        egui::Sense::hover(),
                                    );
                                    let [red, green, blue] = swatch;
                                    ui.painter().rect_filled(r, 2.0, Color32::from_rgb(red, green, blue).gamma_multiply(dim));
                                    let code_color = if error.is_some() { theme::down() } else { amber() };
                                    ui.label(RichText::new(&spec.code).font(mono(12.0)).color(code_color.gamma_multiply(dim)));
                                    ui.label(RichText::new(format!("#{}", c.id)).small().color(muted().gamma_multiply(dim)));
                                    let name_w = (ui.available_width() - theme::dp(56.0)).max(0.0);
                                    ui.scope(|ui| {
                                        ui.set_max_width(name_w);
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(def.map_or("", |d| d.name)).color(muted().gamma_multiply(dim)),
                                            )
                                            .truncate(),
                                        );
                                    });
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if widgets::icon_button(
                                            ui,
                                            widgets::Icon::Close,
                                            "Remove. Also removes indicators calculated from this one.",
                                            false,
                                        )
                                        .clicked()
                                        {
                                            remove = Some(index);
                                        }
                                        ui.add_enabled_ui(index > 0, |ui| {
                                            if widgets::icon_button(ui, widgets::Icon::ArrowUp, "Move up", false).clicked() {
                                                reorder = Some(index);
                                            }
                                        });
                                    });
                                })
                                .body(|ui| {
                                    if let Some(error) = &error {
                                        ui.label(RichText::new(error).color(theme::down()));
                                    }
                                    egui::Grid::new("settings")
                                        .num_columns(2)
                                        .spacing([theme::dp(12.0), theme::dp(6.0)])
                                        .show(ui, |ui| {
                                        if let Some(def) = def {
                                            for (param, value) in def.params.iter().zip(&mut spec.params) {
                                                ui.label(RichText::new(param.name).color(muted()));
                                                let response = ui.add(
                                                    egui::DragValue::new(value)
                                                        .range(param.min..=param.max)
                                                        .speed(if param.int { 1.0 } else { 0.05 }),
                                                );
                                                if response.changed() && param.int {
                                                    *value = value.round();
                                                }
                                                ui.end_row();
                                            }
                                        }
                                        if study::accepts_source(&spec.code) {
                                            ui.label(RichText::new("Input").color(muted()))
                                                .on_hover_text("Calculation input; independent of display placement");
                                            egui::ComboBox::from_id_salt("source")
                                                .selected_text(study_source_label(&c.source, &before))
                                                .width(field_w)
                                                .show_ui(ui, |ui| {
                                                    for (label, source) in [
                                                        ("Original bars", StudySource::Bars),
                                                        ("Open", StudySource::Open),
                                                        ("High", StudySource::High),
                                                        ("Low", StudySource::Low),
                                                        ("Close", StudySource::Close),
                                                        ("Volume", StudySource::Volume),
                                                        ("Typical price (HLC3)", StudySource::Hlc3),
                                                    ] {
                                                        ui.selectable_value(&mut c.source, source, label);
                                                    }
                                                    for b in &self.built {
                                                        let Some(parent) = &b.spec.chart else { continue };
                                                        if !study::can_use_source(&before, c.id, parent.id) {
                                                            continue;
                                                        }
                                                        for name in &b.ports {
                                                            ui.selectable_value(
                                                                &mut c.source,
                                                                StudySource::Output { study: parent.id, output: name.clone() },
                                                                format!("{} #{} · {name}", b.spec.label(), parent.id),
                                                            );
                                                        }
                                                    }
                                                });
                                            ui.end_row();
                                        }
                                        ui.label(RichText::new("Pane").color(muted()));
                                        egui::ComboBox::from_id_salt("pane")
                                            .selected_text(match c.pane {
                                                StudyPane::Price => "Price pane".into(),
                                                StudyPane::Own => "Own pane".into(),
                                                StudyPane::With(id) => format!("With #{id}"),
                                            })
                                            .width(field_w)
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(&mut c.pane, StudyPane::Price, "Price pane");
                                                ui.selectable_value(&mut c.pane, StudyPane::Own, "Own pane");
                                                for (other_index, other) in before.iter().enumerate() {
                                                    if let Some(other_c) = &other.chart
                                                        && other_c.id != c.id
                                                        && study::pane_key(&before, other_index).is_ok_and(|key| key != Some(c.id))
                                                    {
                                                        ui.selectable_value(
                                                            &mut c.pane,
                                                            StudyPane::With(other_c.id),
                                                            format!("With {} #{}", other.label(), other_c.id),
                                                        );
                                                    }
                                                }
                                            });
                                        ui.end_row();
                                        ui.label(RichText::new("Scale").color(muted()));
                                        egui::ComboBox::from_id_salt("scale")
                                            .selected_text(match c.scale {
                                                StudyScale::Shared => "Share pane axis",
                                                StudyScale::Independent => "Independent axis",
                                                StudyScale::Fixed { .. } => "Fixed range",
                                            })
                                            .width(field_w)
                                            .show_ui(ui, |ui| {
                                                ui.selectable_value(&mut c.scale, StudyScale::Shared, "Share pane axis");
                                                ui.selectable_value(&mut c.scale, StudyScale::Independent, "Independent axis");
                                                if ui
                                                    .selectable_label(matches!(c.scale, StudyScale::Fixed { .. }), "Fixed range…")
                                                    .clicked()
                                                {
                                                    let (lo, hi) =
                                                        def.and_then(|d| d.range).unwrap_or((0.0, 100.0));
                                                    c.scale = StudyScale::Fixed { lo, hi };
                                                }
                                            });
                                        ui.end_row();
                                        if let StudyScale::Fixed { lo, hi } = &mut c.scale {
                                            ui.label(RichText::new("Range").color(muted()));
                                            ui.horizontal(|ui| {
                                                ui.add(egui::DragValue::new(lo));
                                                ui.label(RichText::new("to").color(muted()));
                                                ui.add(egui::DragValue::new(hi));
                                            });
                                            ui.end_row();
                                        }
                                        ui.label(RichText::new("Color").color(muted()));
                                        ui.horizontal(|ui| {
                                            let mut rgb = swatch;
                                            if ui.color_edit_button_srgb(&mut rgb).changed() {
                                                c.color = Some(rgb);
                                            }
                                            if c.color.is_some() {
                                                if ui.small_button("Reset").on_hover_text("Use the default series color").clicked() {
                                                    c.color = None;
                                                }
                                            } else {
                                                ui.label(RichText::new("Default").small().color(muted()));
                                            }
                                        });
                                        ui.end_row();
                                    });
                                    ui.collapsing("Calculation & offsets", |ui| {
                                        if spec.code == "PCF" {
                                            ui.label(RichText::new("Formula (documented PCF subset)").color(muted()));
                                            ui.add(
                                                egui::TextEdit::multiline(c.expression.get_or_insert_with(|| "C".into()))
                                                    .font(mono(12.0))
                                                    .desired_rows(3)
                                                    .desired_width(ui.available_width()),
                                            );
                                            ui.label(
                                                RichText::new("Example: C > AVGC20 AND V > 2 * AVG(V,50)").small().color(muted()),
                                            );
                                        }
                                        egui::Grid::new("calculation")
                                            .num_columns(2)
                                            .spacing([theme::dp(12.0), theme::dp(6.0)])
                                            .show(ui, |ui| {
                                            ui.label(RichText::new("Timeframe").color(muted()));
                                            egui::ComboBox::from_id_salt("study-timeframe")
                                                .width(field_w - theme::dp(20.0))
                                                .selected_text(c.timeframe.map_or("Chart timeframe", Timeframe::label))
                                                .show_ui(ui, |ui| {
                                                    ui.selectable_value(&mut c.timeframe, None, "Chart timeframe");
                                                    for tf in Timeframe::ALL {
                                                        ui.selectable_value(&mut c.timeframe, Some(tf), tf.label());
                                                    }
                                                });
                                            ui.end_row();
                                            if matches!(spec.code.as_str(), "BB" | "BBW" | "PCTB" | "ATR" | "MACD" | "ENV") {
                                                let original = match spec.code.as_str() {
                                                    "ATR" => "Original: Wilder",
                                                    "MACD" => "Original: Exponential",
                                                    _ => "Original: Simple",
                                                };
                                                ui.label(RichText::new("Average").color(muted()));
                                                egui::ComboBox::from_id_salt("average-method")
                                                    .width(field_w - theme::dp(20.0))
                                                    .selected_text(c.formula.average.map_or(original, Average::label))
                                                    .show_ui(ui, |ui| {
                                                        ui.selectable_value(&mut c.formula.average, None, original);
                                                        for average in Average::ALL {
                                                            ui.selectable_value(&mut c.formula.average, Some(average), average.label());
                                                        }
                                                    });
                                                ui.end_row();
                                            }
                                            if spec.code == "RSI" {
                                                ui.label(RichText::new("RSI method").color(muted()));
                                                ui.horizontal(|ui| {
                                                    ui.selectable_value(&mut c.formula.rsi_method, RsiMethod::Wilder, "Wilder");
                                                    ui.selectable_value(&mut c.formula.rsi_method, RsiMethod::Plain, "Plain");
                                                });
                                                ui.end_row();
                                                if c.formula.rsi_method == RsiMethod::Plain {
                                                    ui.label(RichText::new("Output SMA").color(muted()));
                                                    ui.add(egui::DragValue::new(&mut c.formula.rsi_smoothing).range(1..=1000));
                                                    ui.end_row();
                                                } else {
                                                    c.formula.rsi_smoothing = 1;
                                                }
                                            }
                                            if spec.code == "BBW" {
                                                ui.label(RichText::new("Bandwidth").color(muted()));
                                                ui.horizontal(|ui| {
                                                    ui.selectable_value(&mut c.formula.bandwidth_units, BandwidthUnits::Ratio, "Ratio");
                                                    ui.selectable_value(&mut c.formula.bandwidth_units, BandwidthUnits::Percent, "Percent");
                                                });
                                                ui.end_row();
                                            }
                                            ui.label(RichText::new("Bars ago").color(muted()));
                                            ui.add(egui::DragValue::new(&mut c.formula.bars_ago).range(0..=1000))
                                                .on_hover_text("Historical value used by this plot and its calculation children");
                                            ui.end_row();
                                            ui.label(RichText::new("Displacement").color(muted()));
                                            ui.add(egui::DragValue::new(&mut c.displacement).range(-1000..=1000)).on_hover_text(
                                                "Display only: positive moves right, negative moves left. Does not change child calculations.",
                                            );
                                            ui.end_row();
                                        });
                                        if matches!(spec.code.as_str(), "BBW" | "PCTB") {
                                            ui.checkbox(&mut c.formula.undefined_zero_division, "Undefined when denominator is zero");
                                        }
                                        if c.timeframe.is_some_and(|tf| tf != self.tf) {
                                            ui.label(
                                                RichText::new("Calculated from this interval's own history; each value appears when its period closes.")
                                                    .small()
                                                    .color(muted()),
                                            );
                                        }
                                    });
                                    let names = registry::Spec::new(&spec.code, &spec.params)
                                        .map(|s| {
                                            s.compute(&registry::Input::new(&[], self.tf))
                                                .into_iter()
                                                .map(|o| o.name.to_string())
                                                .collect::<Vec<_>>()
                                        })
                                        .unwrap_or_default();
                                    crate::chart_controls::output_styles(ui, c, &names);
                                });
                            if index + 1 < count {
                                ui.separator();
                            }
                        });
                    }
                });
                ui.separator();
                ui.collapsing("Chart templates", |ui| {
                    ui.horizontal(|ui| {
                        let save_w = theme::dp(96.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.template_name)
                                .hint_text("Template name")
                                .desired_width(ui.available_width() - save_w),
                        );
                        if ui
                            .add_enabled(!self.template_name.trim().is_empty(), egui::Button::new("Save current"))
                            .clicked()
                        {
                            save_template = true;
                        }
                    });
                    egui::ComboBox::from_id_salt("chart-template")
                        .selected_text(if self.template_selected.is_empty() {
                            "Choose a saved template…"
                        } else {
                            &self.template_selected
                        })
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            let names = self.template_names.as_deref().unwrap_or_default();
                            if names.is_empty() {
                                ui.label(RichText::new("No saved templates yet").color(muted()));
                            }
                            for name in names {
                                ui.selectable_value(&mut self.template_selected, name.clone(), name);
                            }
                        });
                    ui.add_enabled_ui(!self.template_selected.is_empty(), |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("Replace studies & style").on_hover_text("Keeps this symbol, timeframe and drawings").clicked() {
                                apply_template = Some(false);
                            }
                            if ui.button("Add studies").on_hover_text("Adds studies with remapped IDs; keeps existing style").clicked() {
                                apply_template = Some(true);
                            }
                        });
                    });
                    if let Some(status) = &self.template_status {
                        match status {
                            Ok(message) => {
                                ui.label(RichText::new(message).small().color(theme::up()));
                            }
                            Err(error) => {
                                ui.label(RichText::new(error).small().color(theme::down()));
                            }
                        }
                    }
                });
                ui.label(
                    RichText::new("Drag a chart pane divider to resize. Settings save with the workspace.")
                        .small()
                        .color(muted()),
                );
            });
        self.indicator_editor = open;
        if save_template {
            let name = self.template_name.trim().to_string();
            let mut template = crate::chart_templates::Template {
                version: 2,
                name: name.clone(),
                indicators: self.indicators.clone(),
                chart_type: self.chart_type,
                log: self.log,
                show_volume: self.show_volume,
                data: self.data.clone(),
            };
            self.template_status = Some(template.save(&crate::chart_templates::directory()).map(|()| {
                self.template_names = None;
                self.template_selected = name.clone();
                format!("Saved {name}")
            }));
        }
        if let Some(merge) = apply_template {
            self.template_status = Some(
                crate::chart_templates::Template::load(&crate::chart_templates::directory(), &self.template_selected)
                    .and_then(|template| {
                        if merge {
                            template.merge_into(&mut self.indicators)?;
                        } else {
                            self.indicators = template.indicators;
                            self.chart_type = template.chart_type;
                            self.log = template.log;
                            self.show_volume = template.show_volume;
                            let old_session = self.data.session;
                            self.data = template.data;
                            if old_session != self.data.session {
                                self.load(w);
                            }
                        }
                        Ok(format!("Applied {}", self.template_selected))
                    }),
            );
        }
        if let Some(index) = remove {
            self.remove_study(index);
        } else if let Some(index) = reorder {
            self.indicators.swap(index, index - 1);
        }
        study::assign_ids(&mut self.indicators);
        if self.indicators != before {
            self.request_indicators(w);
        }
    }

    fn paint_price_alerts(&self, ui: &Ui, plot: Rect, scale: &PriceScale) {
        use tr_core::price_alerts::Status;
        let painter = ui.painter_at(plot);
        for alert in self.price_alerts.iter().filter(|a| a.status != Status::Triggered && a.rule.is_none()) {
            if scale.log && alert.level <= 0. {
                continue;
            }
            let y = plot.top() + scale.y_of(alert.level) as f32;
            if !y.is_finite() || !plot.y_range().contains(y) {
                continue;
            }
            let color = if alert.status == Status::Armed { amber() } else { muted() };
            let mut x = plot.left();
            while x < plot.right() {
                painter.hline(x..=(x + 6.).min(plot.right()), y, Stroke::new(1., color.gamma_multiply(0.65)));
                x += 11.;
            }
            let label = format!(
                "Alert {} · {}",
                scale::format_price(alert.level, self.sec.decimals(alert.level)),
                crate::price_alerts::status(alert)
            );
            let galley = painter.layout_no_wrap(label, mono(10.5), color);
            let rect = Rect::from_min_size(
                pos2(
                    (plot.right() - galley.size().x - 8.).max(plot.left()),
                    (y - galley.size().y - 2.).max(plot.top()),
                ),
                galley.size() + vec2(6., 2.),
            );
            painter.rect_filled(rect, 0., theme::card());
            painter.galley(rect.min + vec2(3., 0.), galley, color);
        }
    }

    fn context_menu(&mut self, ui: &mut Ui, w: &Worker) {
        ui.set_min_width(theme::dp(190.));
        menu_section(ui, "View");
        self.view_menu(ui, w);
        ui.separator();
        menu_section(ui, "Analysis");
        self.analysis_menu(ui);
        ui.separator();
        menu_section(ui, "Alerts & account");
        self.alerts_menu(ui);
        ui.menu_button("Positions & orders", |ui| self.account_overlay.menu(ui, &mut self.data.account));
        ui.separator();
        menu_section(ui, "Tools");
        self.replay_export_menu(ui, w);
        self.navigation_menu(ui, w);
        self.data_navigation_menu(ui, w);
        ui.separator();
        if ui.button("Add to Watchlist").clicked() {
            self.actions.push(Action::AddToWatchlist(self.symbol.clone()));
            ui.close();
        }
        if self.sec.has_congress_trades() {
            ui.checkbox(&mut self.show_congress, "Congress Trade Markers");
            if ui.button("Who in Congress Traded This…").clicked() {
                self.actions.push(Action::OpenTicker(self.symbol.clone()));
                ui.close();
            }
        }
    }

    fn analysis_menu(&mut self, ui: &mut Ui) {
        ui.menu_button("Anchored VWAP & volume profile", |ui| {
            self.data.analytics.menu(ui, self.context_timestamp, self.context_price)
        });
        ui.menu_button("Session context", |ui| self.data.context.menu(ui));
        ui.menu_button("Corporate events", |ui| {
            ui.set_max_width(theme::dp(280.));
            ui.checkbox(&mut self.data.show_events, "Show event markers");
            if ui.button("Reload event data").clicked() {
                self.events_range = None;
            }
            ui.separator();
            menu_note(ui, "E earnings · D dividends · S splits; hover for details and source.");
            if self.events_notice.starts_with("Provider events unavailable:")
                || self.events_notice.contains("import error:")
            {
                ui.add(egui::Label::new(RichText::new(&self.events_notice).small().color(theme::down())).wrap());
            } else {
                menu_note(ui, &self.events_notice);
            }
            menu_note(ui, "Dividends/splits from Yahoo · earnings from chart-events.json");
            menu_note(ui, "Import attributed earnings in the app data directory.");
        });
    }

    fn navigation_menu(&mut self, ui: &mut Ui, w: &Worker) {
        ui.menu_button("Navigation", |ui| {
            ui.set_max_width(theme::dp(280.));
            if ui.add(egui::Button::new("Previous symbol").shortcut_text("↑")).clicked() {
                self.actions.push(Action::Command("PREV".into()));
                ui.close();
            }
            if ui.add(egui::Button::new("Next symbol").shortcut_text("↓")).clicked() {
                self.actions.push(Action::Command("NEXT".into()));
                ui.close();
            }
            if ui.button("Use watchlist for traversal").clicked() {
                *self.data.workflow = Default::default();
                ui.close();
            }
            if let Some(scan) = &self.data.workflow.scan {
                ui.label(
                    RichText::new(format!("Scan · {} · {} symbols", scan.name, self.data.workflow.symbols.len()))
                        .small()
                        .color(muted()),
                );
                for condition in &scan.conditions {
                    ui.label(RichText::new(condition.label()).font(mono(11.)).color(muted()));
                }
                if ui.button("Plot scan conditions").clicked() {
                    self.plot_scan(w);
                    ui.close();
                }
            }
            ui.separator();
            menu_note(ui, "Type DATE YYYY-MM-DD to jump to a date");
            menu_note(ui, "Provider history limits apply.");
            if self.data.workflow.scan.is_some() {
                menu_note(ui, "Plot: 1 = match; uses loaded bars, not the earlier scan snapshot.");
            }
        });
    }

    fn replay_export_menu(&mut self, ui: &mut Ui, w: &Worker) {
        ui.menu_button("Replay & export", |ui| {
            ui.set_max_width(theme::dp(280.));
            if ui.add_enabled(!self.source_bars.is_empty(), egui::Button::new("Replay from here…")).clicked() {
                self.replay_cutoff =
                    self.context_timestamp.or_else(|| self.source_bars.get(self.source_bars.len() / 2).map(|b| b.ts));
                self.replay_play = false;
                ui.ctx().request_repaint();
                ui.close();
            }
            ui.separator();
            for (label, image) in [("Export visible bars as CSV", false), ("Export chart as SVG", true)] {
                if ui.add_enabled(!self.bars.is_empty(), egui::Button::new(label)).clicked() {
                    let (a, b) = self.viewport.map_or((0, self.bars.len()), |v| v.visible_range(self.bars.len()));
                    w.send(Request::ExportChart {
                        pane: self.id,
                        symbol: self.symbol.clone(),
                        title: format!(
                            "{} {} · {}{}",
                            self.symbol,
                            self.tf,
                            self.chart_type.label(),
                            if self.replay_cutoff.is_some() { " · REPLAY" } else { "" }
                        ),
                        bars: Arc::new(if self.chart_type == ChartType::HeikinAshi {
                            tr_core::chart_analytics::heikin_ashi(&self.bars)[a..b].to_vec()
                        } else {
                            self.bars[a..b].to_vec()
                        }),
                        image,
                    });
                    self.export_notice = None;
                }
            }
            if let Some(result) = &self.export_notice {
                match result {
                    Ok(path) => {
                        let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
                        ui.add(egui::Label::new(format!("Saved {name}")).truncate())
                            .on_hover_text(path.display().to_string());
                        if ui.button("Open export folder").clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(format!(
                                "file://{}",
                                path.parent().unwrap().display()
                            )));
                        }
                    }
                    Err(e) => {
                        ui.colored_label(theme::down(), e);
                    }
                }
            }
            ui.separator();
            menu_note(ui, "Replay uses loaded bars; live alerts keep running.");
            menu_note(ui, "Hidden during replay: drawings, account levels, Congress markers.");
            menu_note(ui, "SVG: candles and axes only (no studies, drawings or UI panes).");
        });
        if self.chart_type == ChartType::Renko {
            menu_section(ui, "Renko box");
            self.box_controls(ui);
            ui.separator();
            menu_note(ui, "Close-only bricks, one-box reversal; studies use synthetic bricks.");
            menu_note(ui, "No comparisons or drawings on repeated brick timestamps.");
        }
    }

    fn alerts_menu(&mut self, ui: &mut Ui) {
        use crate::price_alerts::Action as AlertAction;
        if ui.add_enabled(self.context_price.is_some(), egui::Button::new("Create price alert here…")).clicked() {
            if let Some(level) = self.context_price {
                let direction = if self.bars.last().is_some_and(|bar| level < bar.close) {
                    tr_core::price_alerts::Direction::Below
                } else {
                    tr_core::price_alerts::Direction::Above
                };
                self.price_alert_actions.push(AlertAction::Create { symbol: self.symbol.clone(), level, direction });
            }
            ui.close();
        }
        ui.menu_button("Price alerts", |ui| {
            if ui.button("Indicator / formula alert…").clicked() {
                self.price_alert_actions.push(AlertAction::Condition {
                    symbol: self.symbol.clone(),
                    level: 70.,
                    rule: tr_core::price_alerts::Rule::Formula { expression: "WRSI(14)".into(), timeframe: self.tf },
                });
                ui.close();
            }
            if let Some(scan) = &self.data.workflow.scan
                && ui.button("Alert when this symbol matches scan…").clicked()
                && let Ok(expression) = crate::chart_workflow::expression(scan)
            {
                self.price_alert_actions.push(AlertAction::Condition {
                    symbol: self.symbol.clone(),
                    level: 0.5,
                    rule: tr_core::price_alerts::Rule::Formula { expression, timeframe: scan.timeframe },
                });
                ui.close();
            }
            if ui.button("Manage all alerts…").clicked() {
                self.price_alert_actions.push(AlertAction::Manage);
                ui.close();
            }
            for alert in &self.price_alerts {
                ui.separator();
                ui.label(format!(
                    "{} {} · {}",
                    alert.direction.label(),
                    alert.level,
                    crate::price_alerts::status(alert)
                ));
                ui.push_id(alert.id, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Edit…").clicked() {
                            self.price_alert_actions.push(AlertAction::Edit(alert.id));
                            ui.close();
                        }
                        if ui
                            .button(if alert.status == tr_core::price_alerts::Status::Armed {
                                "Disable"
                            } else {
                                "Rearm"
                            })
                            .clicked()
                        {
                            self.price_alert_actions.push(AlertAction::Toggle(alert.id));
                            ui.close();
                        }
                        if ui.button("Delete").clicked() {
                            self.price_alert_actions.push(AlertAction::Delete(alert.id));
                            ui.close();
                        }
                    });
                });
            }
        });
    }

    fn data_navigation_menu(&mut self, ui: &mut Ui, w: &Worker) {
        ui.menu_button("Time zone, session & refresh", |ui| {
            ui.set_max_width(theme::dp(280.));
            let before = self.data.clone();
            self.data.edit(ui, matches!(self.sec.class, AssetClass::Equity | AssetClass::Etf));
            if self.data.session != before.session || self.data.history != before.history {
                self.pending_dates =
                    self.viewport.as_ref().and_then(|v| crate::chart_navigation::date_range(&self.bars, self.tf, v));
                self.load(w);
            }
            if ui.button("Refresh history now").clicked() {
                self.pending_dates =
                    self.viewport.as_ref().and_then(|v| crate::chart_navigation::date_range(&self.bars, self.tf, v));
                self.load(w);
                ui.close();
            }
            ui.separator();
            menu_note(ui, "Session filters are separate from history requests; unsupported options show an error.");
            menu_note(ui, &tr_core::exchange_calendar::description());
            menu_note(ui, "History refresh does not establish a real-time subscription.");
            if let Some(notice) = &self.live_notice {
                menu_note(ui, notice);
            }
        });
        ui.menu_button("Linked navigation", |ui| {
            ui.set_max_width(theme::dp(280.));
            ui.checkbox(&mut self.navigation.cursor, "Synchronize timestamp cursor");
            ui.checkbox(&mut self.navigation.range, "Synchronize visible date range");
            ui.separator();
            menu_note(ui, "Uses this pane's color link group");
        });
    }

    fn view_menu(&mut self, ui: &mut Ui, w: &Worker) {
        use crate::price_alerts::Action as AlertAction;
        ui.menu_button("Chart type", |ui| {
            for ct in ChartType::ALL {
                if ui.radio(self.chart_type == ct, ct.label()).clicked() {
                    self.chart_type = ct;
                    ui.close();
                }
            }
        });
        ui.menu_button("Indicators", |ui| {
            ui.set_min_width(220.0);
            if ui.button("Manage indicators…").clicked() {
                self.indicator_editor = true;
                ui.close();
            }
            if ui.button("Search indicators…").clicked() {
                self.actions.push(Action::Command("IND".into()));
                ui.close();
            }
            if !self.indicators.is_empty() {
                ui.separator();
                ui.label(RichText::new("On this chart").small().color(muted()));
                for spec in self.indicators.clone() {
                    if ui.checkbox(&mut true, spec.label()).on_hover_text(spec.name()).clicked() {
                        self.toggle_indicator(spec, w);
                    }
                }
            }
            ui.separator();
            for cat in Category::ALL {
                let label = match cat {
                    Category::MovingAverages => "Moving averages",
                    Category::Bands => "Bands & channels",
                    _ => cat.label(),
                };
                ui.menu_button(label, |ui| {
                    ui.set_min_width(260.0);
                    egui::ScrollArea::vertical().max_height(440.0).show(ui, |ui| {
                        for def in registry::all().iter().filter(|d| d.category == cat) {
                            let on = self.indicators.iter().any(|s| s.code == def.code);
                            let hover = IndicatorSpec::of(def).label();
                            if ui.checkbox(&mut on.clone(), def.name).on_hover_text(hover).clicked() {
                                if on {
                                    self.remove_indicator(def.code, w);
                                } else {
                                    self.toggle_indicator(IndicatorSpec::of(def), w);
                                }
                            }
                        }
                    });
                });
            }
            ui.separator();
            if ui.button("Remove all").clicked() {
                self.clear_indicators();
                ui.close();
            }
        });
        ui.menu_button("Draw", |ui| {
            if ui
                .add_enabled(
                    self.draw.selected.is_some(),
                    egui::Button::new("Delete selected drawing").shortcut_text("Delete"),
                )
                .clicked()
            {
                self.delete_selected();
                ui.close();
            }
            let alt = if cfg!(target_os = "macos") { "⌥" } else { "Alt+" };
            for (tool, key) in
                [(Tool::Trendline, "T"), (Tool::Horizontal, "H"), (Tool::Vertical, "V"), (Tool::Fib, "F")]
            {
                if ui.add(egui::Button::new(tool.label()).shortcut_text(format!("{alt}{key}"))).clicked() {
                    self.start_tool(tool);
                    ui.close();
                }
            }
            for tool in [
                Tool::Measure,
                Tool::Rectangle,
                Tool::Ray,
                Tool::Channel,
                Tool::Text,
                Tool::FibExtension,
                Tool::Pitchfork,
                Tool::LongPosition,
                Tool::ShortPosition,
            ] {
                if ui.button(tool.label()).clicked() {
                    self.start_tool(tool);
                    ui.close();
                }
            }
            if ui.add_enabled(!self.drawing_undo.is_empty(), egui::Button::new("Undo drawing edit")).clicked() {
                self.undo_drawings();
                ui.close();
            }
            if ui.add_enabled(!self.drawing_redo.is_empty(), egui::Button::new("Redo drawing edit")).clicked() {
                self.redo_drawings();
                ui.close();
            }
            if let Some(index) = self.draw.selected.filter(|&i| i < self.drawings.len()) {
                ui.separator();
                let mut edited = self.drawings[index].clone();
                ui.label(format!("Selected {}", edited.tool.label()));
                if matches!(edited.tool, Tool::Trendline | Tool::Ray | Tool::Horizontal)
                    && ui.button("Alert on saved line…").clicked()
                {
                    let (a, b) = if edited.a.ts <= edited.b.ts { (edited.a, edited.b) } else { (edited.b, edited.a) };
                    let (start, end, first, last) = if edited.tool == Tool::Horizontal {
                        (a.ts, a.ts + 60, a.price, a.price)
                    } else {
                        (a.ts, b.ts, a.price, b.price)
                    };
                    self.price_alert_actions.push(AlertAction::Condition {
                        symbol: self.symbol.clone(),
                        level: 0.,
                        rule: tr_core::price_alerts::Rule::Line { start, end, first, last, logarithmic: self.log },
                    });
                    ui.close();
                }
                if edited.tool == Tool::Text && ui.button("Edit text…").clicked() {
                    self.draw.note = Some(NoteDraft {
                        anchor: edited.a,
                        text: edited.text.clone(),
                        existing: Some((index, edited.clone())),
                    });
                    ui.close();
                }
                ui.add(egui::DragValue::new(&mut edited.width).range(0.5..=6.0).speed(0.1).prefix("Width "));
                let mut custom = edited.color.is_some();
                if ui.checkbox(&mut custom, "Custom drawing color").changed() {
                    edited.color = custom.then_some(theme::cyan().to_array()[..3].try_into().unwrap());
                }
                if let Some(color) = &mut edited.color {
                    ui.color_edit_button_srgb(color);
                }
                if ui.checkbox(&mut edited.all_symbols, "Show on all symbols").changed() && !edited.all_symbols {
                    edited.symbol = Some(self.symbol.clone());
                }
                if edited.tool == Tool::Trendline {
                    ui.checkbox(&mut edited.extend_left, "Extend left");
                    ui.checkbox(&mut edited.extend_right, "Extend right");
                }
                if edited != self.drawings[index] {
                    self.remember_drawings();
                    self.drawings[index] = edited;
                }
            }
            if ui.button("Remove drawings on this symbol").clicked() {
                self.remember_drawings();
                self.drawings.retain(|d| !d.visible_on(&self.symbol));
                self.draw = DrawState::default();
                ui.close();
            }
        });
        if ui.button("Compare symbol…").clicked() {
            self.compare_picker.open();
            ui.close();
        }
        ui.checkbox(&mut self.log, "Log scale");
        ui.checkbox(&mut self.show_volume, "Volume");
        ui.checkbox(&mut self.magnet, "Magnet");
        if ui.button("Reset view").clicked() {
            self.reset_view();
            ui.close();
        }
    }

    fn paint_legend(&self, ui: &Ui, price_rect: Rect, i: usize, rows: &[crate::chart_legend::Row]) {
        let Some(b) = self.bars.get(i) else { return };
        let p = ui.painter_at(price_rect);
        let prev = if i > 0 { self.bars[i - 1].close } else { b.open };
        let chg = b.close - prev;
        let col = if chg >= 0.0 { theme::up() } else { theme::down() };
        let dec = self.sec.decimals(b.close);
        let mut x = price_rect.left() + 6.0;
        let y = price_rect.top() + 4.0;
        let put = |text: String, color: Color32, x: &mut f32| {
            let g = p.layout_no_wrap(text, mono(11.5), color);
            label_plate(&p, Rect::from_min_size(pos2(*x, y), g.size()));
            p.galley(pos2(*x, y), g.clone(), color);
            *x += g.size().x + 8.0;
        };
        put(format!("{} · {}", self.sec.ticker, self.tf), amber(), &mut x);
        for (k, val) in [("O", b.open), ("H", b.high), ("L", b.low), ("C", b.close)] {
            put(format!("{k}{val:.dec$}"), col, &mut x);
        }
        put(format!("{:+.dec$} ({:+.2}%)", chg, if prev != 0.0 { chg / prev * 100.0 } else { 0.0 }), col, &mut x);
        put(format!("Vol {}", scale::format_volume(b.volume)), muted(), &mut x);

        let groups = study_groups(&self.built);
        let mut ly = y + 16.0 + rows.iter().filter(|row| price_rect.contains(row.rect.center())).count() as f32 * 14.0;
        let hidden = groups.iter().filter(|(key, _)| key.is_some()).count().saturating_sub(MAX_SUB_PANES);
        if hidden > 0 {
            let note =
                format!("+{hidden} more oscillator{} (max {MAX_SUB_PANES} panes)", if hidden == 1 { "" } else { "s" });
            label_text(&p, pos2(price_rect.left() + 6.0, ly), Align2::LEFT_TOP, note, mono(11.0), muted());
            ly += 14.0;
        }
        for key in self.compare.iter().chain(&self.data.comparisons) {
            let ticker = key.split_whitespace().next().unwrap_or(key);
            let extra = self.comparisons.get(key);
            let (line, factor, loading, error) = if self.compare.as_ref() == Some(key) {
                (self.compare_line.as_ref(), self.compare_scale, self.compare_loading, self.compare_error.as_deref())
            } else {
                (
                    extra.and_then(|s| s.line.as_ref()),
                    extra.map_or(1., |s| s.factor),
                    extra.is_none_or(|s| s.loading),
                    extra.and_then(|s| s.error.as_deref()),
                )
            };
            let raw = line.and_then(|line| line.pyramid.data.get(i)).map(|p| p[1] as f64).filter(|v| v.is_finite());
            let state = if self.chart_type == ChartType::Renko {
                Some("unavailable on Renko")
            } else if self.data.hidden_comparisons.contains(key) {
                Some("hidden")
            } else if let Some(error) = error {
                Some(error)
            } else if loading {
                Some("loading…")
            } else {
                None
            };
            let label = if let Some(state) = state {
                format!("{ticker} · {state}")
            } else if let Some(raw) = raw {
                self.compare_base.map_or_else(
                    || format!("{ticker} {raw:.2}"),
                    |base| format!("{ticker} {raw:.2} {:+.2}%", percent(raw * factor, base)),
                )
            } else {
                format!("{ticker} · unavailable")
            };
            let pos = pos2(price_rect.left() + theme::dp(6.), ly);
            let size = p.layout_no_wrap(label.clone(), mono(11.), self.compare_color(key)).size();
            label_text(&p, pos, Align2::LEFT_TOP, label, mono(11.), self.compare_color(key));
            ui.interact(Rect::from_min_size(pos, size), ui.id().with(("compare-legend", key)), Sense::hover())
                .on_hover_text(format!("{key}\n% change from the visible left edge"));
            ly += 14.;
        }
        if self.data.analytics.anchor.is_some() {
            let value = self
                .analytics
                .vwap
                .as_ref()
                .and_then(|line| line.pyramid.data.get(i))
                .map(|p| p[1])
                .filter(|v| v.is_finite());
            label_text(
                &p,
                pos2(price_rect.left() + 6., ly),
                Align2::LEFT_TOP,
                value.map_or("Anchored VWAP · unavailable in loaded history".into(), |v| {
                    format!("Anchored VWAP {v:.2}")
                }),
                mono(10.),
                theme::text(),
            );
            ly += 14.;
        }
        if let Some(tool) = self.draw.tool {
            let msg = if self.draw.second.is_some() {
                "click third point / target (Esc cancels)"
            } else if self.draw.pending.is_some() {
                "click second point (Esc cancels)"
            } else {
                "click to place (Esc cancels)"
            };
            label_text(
                &p,
                pos2(price_rect.left() + 6.0, ly),
                Align2::LEFT_TOP,
                format!("{}: {msg}", tool.label()),
                mono(11.0),
                amber(),
            );
        }
        crate::chart_legend::paint(ui, rows);
    }

    fn show_note_editor(&mut self, ctx: &egui::Context) {
        let Some(draft) = &mut self.draw.note else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        egui::Window::new("Chart text note")
            .id(egui::Id::new((self.id, "drawing-note")))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(300.)
            .show(ctx, |ui| {
                ui.label("Note");
                ui.add(egui::TextEdit::multiline(&mut draft.text).char_limit(500).desired_rows(4).desired_width(285.));
                ui.label(
                    RichText::new("Up to 500 characters. Double-click the note to edit it.").small().color(muted()),
                );
                ui.horizontal(|ui| {
                    save = ui.add_enabled(!draft.text.trim().is_empty(), egui::Button::new("Save note")).clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if save {
            let draft = self.draw.note.take().unwrap();
            let note = draft.text.trim().to_string();
            if let Some((index, original)) = draft.existing {
                // A nonmodal editor must not update a different drawing after undo/delete.
                if self.drawings.get(index) == Some(&original) && original.text != note {
                    self.remember_drawings();
                    self.drawings[index].text = note;
                }
            } else {
                self.remember_drawings();
                let mut drawing = Drawing::new(Tool::Text, draft.anchor, draft.anchor);
                drawing.text = note;
                drawing.symbol = Some(self.symbol.clone());
                self.drawings.push(drawing);
                self.draw.selected = Some(self.drawings.len() - 1);
            }
        } else if cancel || !open {
            self.draw.note = None;
        }
    }

    fn drawing_shapes(
        &self,
        ui: &Ui,
        d: &Drawing,
        rect: Rect,
        v: &Viewport,
        s: &PriceScale,
    ) -> Vec<(Shape, Option<String>)> {
        if [Some(d.a), Some(d.b), d.c]
            .into_iter()
            .take(d.tool.points())
            .flatten()
            .any(|a| !a.price.is_finite() || (s.log && a.price <= 0.))
        {
            return vec![];
        }
        let px =
            |a: &Anchor| [rect.left() as f64 + v.x_of(self.index_of_ts(a.ts)), rect.top() as f64 + s.y_of(a.price)];
        match d.tool {
            Tool::Trendline | Tool::Ray => {
                let (mut a, mut b) = (px(&d.a), px(&d.b));
                if d.tool == Tool::Trendline && a[0] > b[0] {
                    std::mem::swap(&mut a, &mut b);
                }
                drawings::clip_line(
                    a,
                    b,
                    [rect.left() as f64, rect.top() as f64, rect.right() as f64, rect.bottom() as f64],
                    d.tool == Tool::Trendline && d.extend_left,
                    d.tool == Tool::Ray || d.extend_right,
                )
                .map(|(a, b)| vec![(Shape::Segment(a, b), None)])
                .unwrap_or_default()
            }
            Tool::Rectangle | Tool::Measure => {
                let label = if d.tool == Tool::Measure {
                    let bars = (self.index_of_ts(d.b.ts) - self.index_of_ts(d.a.ts)).abs().round() as usize;
                    Some(drawings::measure_label(d.a, d.b, bars, self.sec.decimals(d.a.price)))
                } else {
                    None
                };
                vec![(Shape::Rect(px(&d.a), px(&d.b)), label)]
            }
            Tool::Channel => {
                if let Some(c) = d.c {
                    vec![(Shape::Polygon(drawings::channel_polygon(px(&d.a), px(&d.b), px(&c))), None)]
                } else {
                    vec![(Shape::Segment(px(&d.a), px(&d.b)), None)]
                }
            }
            Tool::Pitchfork => {
                let (a, b) = (px(&d.a), px(&d.b));
                let c = px(&d.c.unwrap_or(d.b));
                let m = [(b[0] + c[0]) / 2., (b[1] + c[1]) / 2.];
                let delta = [m[0] - a[0], m[1] - a[1]];
                [a, b, c]
                    .into_iter()
                    .filter_map(|start| {
                        drawings::clip_line(
                            start,
                            [start[0] + delta[0], start[1] + delta[1]],
                            [rect.left() as f64, rect.top() as f64, rect.right() as f64, rect.bottom() as f64],
                            false,
                            true,
                        )
                        .map(|(a, b)| (Shape::Segment(a, b), None))
                    })
                    .collect()
            }
            Tool::LongPosition | Tool::ShortPosition => {
                if let Some((risk, reward, ratio)) = d.risk_reward() {
                    let a = px(&d.a);
                    let b = px(&d.b);
                    let c = px(&d.c.unwrap());
                    let right = b[0].max(c[0]).max(a[0] + 35.);
                    vec![
                        (Shape::Rect(a, [right, b[1]]), Some(format!("Risk {risk:.2} / unit"))),
                        (Shape::Rect(a, [right, c[1]]), Some(format!("Reward {reward:.2} / unit · R/R {ratio:.2}"))),
                    ]
                } else {
                    vec![(
                        Shape::Segment(px(&d.a), px(&d.b)),
                        Some("Set entry, stop, then target on the opposite side".into()),
                    )]
                }
            }
            Tool::Text => {
                let a = px(&d.a);
                let size = ui.painter().layout(d.text.clone(), ui_font(11.5), text(), 220.).size() + vec2(8., 4.);
                vec![(Shape::Rect(a, [a[0] + size.x as f64, a[1] + size.y as f64]), Some(d.text.clone()))]
            }
            Tool::Horizontal => vec![(Shape::HLine(px(&d.a)[1]), Some(format!("{:.2}", d.a.price)))],
            Tool::Vertical => vec![(Shape::VLine(px(&d.a)[0]), None)],
            Tool::Fib | Tool::FibExtension => {
                let (xa, xb) = (px(&d.a)[0], px(&d.b)[0]);
                let (x0, x1) = if d.tool == Tool::FibExtension {
                    (px(&d.c.unwrap_or(d.b))[0], rect.right() as f64)
                } else {
                    (xa.min(xb), xa.max(xb))
                };
                let mut shapes: Vec<_> = (if d.tool == Tool::Fib { d.fib_levels() } else { d.extension_levels() })
                    .into_iter()
                    .map(|(r, price)| {
                        let y = rect.top() as f64 + s.y_of(price);
                        (Shape::Segment([x0, y], [x1, y]), Some(format!("{r:.3} ({price:.2})")))
                    })
                    .collect();
                if d.tool == Tool::FibExtension {
                    shapes.extend([
                        (Shape::Segment(px(&d.a), px(&d.b)), None),
                        (Shape::Segment(px(&d.b), px(&d.c.unwrap_or(d.b))), None),
                    ]);
                }
                shapes
            }
        }
    }

    fn paint_drawings(
        &mut self,
        ui: &Ui,
        rect: Rect,
        v: &Viewport,
        s: &PriceScale,
        hover: Option<Pos2>,
        resp: &egui::Response,
    ) {
        let p = ui.painter_at(rect);
        let pointer_anchor = |this: &Self, pos: Pos2| -> Anchor {
            let pos = pos.clamp(rect.min, rect.max);
            let idx = v.index_at((pos.x - rect.left()) as f64);
            let mut price = s.price_at((pos.y - rect.top()) as f64);
            if this.magnet
                && let Some(b) = this.bars.get(idx.floor().max(0.0) as usize)
            {
                price = drawings::magnet(price, [b.open, b.high, b.low, b.close]);
            }
            Anchor { ts: this.ts_of_index(idx), price }
        };

        // Endpoint handles take precedence over whole-object hit testing. Keep
        // original anchors for absolute drag deltas, including on logarithmic axes.
        let hit = |this: &Self, pos: Pos2| -> Option<(usize, DrawHandle)> {
            if !rect.contains(pos) {
                return None;
            }
            for (index, drawing) in this.drawings.iter().enumerate().rev().filter(|(_, d)| d.visible_on(&this.symbol)) {
                let anchors =
                    [(Some(drawing.a), DrawHandle::A), (Some(drawing.b), DrawHandle::B), (drawing.c, DrawHandle::C)];
                for (anchor, handle) in anchors.into_iter().take(drawing.tool.points()) {
                    let Some(anchor) = anchor else {
                        continue;
                    };
                    let point = pos2(
                        rect.left() + v.x_of(this.index_of_ts(anchor.ts)) as f32,
                        rect.top() + s.y_of(anchor.price) as f32,
                    );
                    if point.is_finite() && point.distance(pos) < 8.0 {
                        return Some((index, handle));
                    }
                }
                if this
                    .drawing_shapes(ui, drawing, rect, v, s)
                    .iter()
                    .any(|(shape, _)| drawings::distance(shape, [pos.x as f64, pos.y as f64]) < 6.0)
                {
                    return Some((index, DrawHandle::Body));
                }
            }
            None
        };
        if self.draw.tool.is_none() && self.draw.drag.is_none() && self.draw.note.is_none() {
            self.draw.hover = hover.and_then(|pos| hit(self, pos));
        }
        if self.draw.tool.is_none()
            && self.draw.note.is_none()
            && resp.drag_started()
            && let Some(pos) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
            && let Some((index, handle)) = hit(self, pos)
        {
            self.remember_drawings();
            self.draw.drag = Some(DrawingDrag {
                index,
                handle,
                start: pointer_anchor(self, pos),
                original: self.drawings[index].clone(),
            });
            self.draw.selected = Some(index);
        }
        if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
            && self.draw.drag.is_some()
        {
            let anchor = pointer_anchor(self, pos);
            let drag = self.draw.drag.as_ref().unwrap();
            let mut edited = drag.original.clone();
            match drag.handle {
                DrawHandle::A => edited.a = anchor,
                DrawHandle::B => edited.b = anchor,
                DrawHandle::C => edited.c = Some(anchor),
                DrawHandle::Body => {
                    for point in
                        std::iter::once(&mut edited.a).chain(std::iter::once(&mut edited.b)).chain(edited.c.iter_mut())
                    {
                        point.ts = point.ts.saturating_add(anchor.ts.saturating_sub(drag.start.ts));
                        point.price = if s.log && drag.start.price > 0. {
                            point.price * anchor.price / drag.start.price
                        } else {
                            point.price + anchor.price - drag.start.price
                        };
                    }
                }
            }
            if edited.tool.points() == 1 {
                if matches!(drag.handle, DrawHandle::B) {
                    edited.a = edited.b;
                } else {
                    edited.b = edited.a;
                }
            }
            self.drawings[drag.index] = edited;
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        } else if self.draw.hover.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        if resp.drag_stopped() {
            self.draw.drag = None;
        }

        // Placement.
        if let (Some(tool), true, Some(pos)) =
            (self.draw.tool, resp.clicked(), resp.interact_pointer_pos().filter(|p| rect.contains(*p)))
        {
            let a = pointer_anchor(self, pos);
            if tool == Tool::Text {
                self.draw = DrawState {
                    note: Some(NoteDraft { anchor: a, existing: None, text: String::new() }),
                    ..Default::default()
                };
            } else if tool.points() == 1 {
                self.remember_drawings();
                let mut drawing = Drawing::new(tool, a, a);
                drawing.symbol = Some(self.symbol.clone());
                self.drawings.push(drawing);
                self.draw = DrawState::default();
            } else if tool.points() == 3
                && self.draw.second.is_none()
                && let Some(first) = self.draw.pending
            {
                let screen = pos2(
                    rect.left() + v.x_of(self.index_of_ts(first.ts)) as f32,
                    rect.top() + s.y_of(first.price) as f32,
                );
                if screen.distance(pos) >= 3. {
                    self.draw.second = Some(a);
                }
            } else if let Some(first) = self.draw.pending.take() {
                self.remember_drawings();
                let mut drawing = Drawing::new(tool, first, self.draw.second.unwrap_or(a));
                if tool.points() == 3 {
                    drawing.c = Some(a);
                }
                drawing.symbol = Some(self.symbol.clone());
                self.drawings.push(drawing);
                self.draw = DrawState::default();
                self.draw.selected = Some(self.drawings.len() - 1);
            } else {
                self.draw.pending = Some(a);
            }
        } else if self.draw.tool.is_none() && self.draw.note.is_none() && resp.clicked() {
            // Selection by proximity.
            self.draw.selected = resp.interact_pointer_pos().and_then(|pos| hit(self, pos).map(|(index, _)| index));
            if resp.double_clicked()
                && let Some(index) = self.draw.selected.filter(|&i| self.drawings[i].tool == Tool::Text)
            {
                let original = self.drawings[index].clone();
                self.draw.note = Some(NoteDraft {
                    anchor: original.a,
                    text: original.text.clone(),
                    existing: Some((index, original)),
                });
            }
        }

        let mut all: Vec<(Drawing, bool)> = self
            .drawings
            .iter()
            .cloned()
            .enumerate()
            .filter(|(_, d)| d.visible_on(&self.symbol))
            .map(|(i, d)| (d, self.draw.selected == Some(i)))
            .collect();
        if let (Some(tool), Some(first), Some(h)) = (self.draw.tool, self.draw.pending, hover)
            && rect.contains(h)
        {
            let mut preview = Drawing::new(tool, first, self.draw.second.unwrap_or_else(|| pointer_anchor(self, h)));
            if self.draw.second.is_some() {
                preview.c = Some(pointer_anchor(self, h));
            }
            all.push((preview, true));
        }
        for (d, selected) in all {
            let color =
                if selected { amber() } else { d.color.map_or(theme::cyan(), |[r, g, b]| Color32::from_rgb(r, g, b)) };
            let stroke = Stroke::new(if selected { d.width.clamp(2.0, 6.0) } else { d.width.clamp(0.5, 6.0) }, color);
            if selected {
                for anchor in [Some(d.a), Some(d.b), d.c].into_iter().take(d.tool.points()).flatten() {
                    let point = pos2(
                        rect.left() + v.x_of(self.index_of_ts(anchor.ts)) as f32,
                        rect.top() + s.y_of(anchor.price) as f32,
                    );
                    if point.is_finite() {
                        p.circle_filled(point, 3.5, amber());
                    }
                }
            }
            for (shape, label) in self.drawing_shapes(ui, &d, rect, v, s) {
                match shape {
                    Shape::Segment(a, b) => {
                        p.line_segment([pos2(a[0] as f32, a[1] as f32), pos2(b[0] as f32, b[1] as f32)], stroke);
                        if let Some(l) = label {
                            label_text(
                                &p,
                                pos2(a[0] as f32 + 3.0, a[1] as f32 - 1.0),
                                Align2::LEFT_BOTTOM,
                                l,
                                mono(10.0),
                                color,
                            );
                        }
                    }
                    Shape::HLine(y) => {
                        p.hline(rect.x_range(), y as f32, stroke);
                        if let Some(l) = label {
                            label_text(
                                &p,
                                pos2(rect.right() - 4.0, y as f32 - 1.0),
                                Align2::RIGHT_BOTTOM,
                                l,
                                mono(10.0),
                                color,
                            );
                        }
                    }
                    Shape::VLine(x) => {
                        p.vline(x as f32, rect.y_range(), stroke);
                    }
                    Shape::Rect(a, b) => {
                        let r = Rect::from_two_pos(pos2(a[0] as f32, a[1] as f32), pos2(b[0] as f32, b[1] as f32));
                        if d.tool == Tool::Text {
                            // The text gets its own plate below.
                            if selected {
                                p.rect_stroke(r, 2., stroke, StrokeKind::Inside);
                            }
                            let galley = p.layout(label.unwrap_or_default(), ui_font(11.5), color, 220.);
                            label_plate(&p, Rect::from_min_size(r.min + vec2(4., 2.), galley.size()));
                            p.galley(r.min + vec2(4., 2.), galley, color);
                        } else {
                            let fill = if matches!(d.tool, Tool::LongPosition | Tool::ShortPosition) {
                                if label.as_ref().is_some_and(|s| s.starts_with("Risk")) {
                                    theme::down()
                                } else {
                                    theme::up()
                                }
                            } else {
                                color
                            };
                            p.rect_filled(r, 0., fill.gamma_multiply(0.12));
                            p.rect_stroke(r, 0., stroke, StrokeKind::Inside);
                            if let Some(label) = label {
                                let galley = p.layout(label, mono(11.), color, rect.width().min(330.));
                                let size = galley.size() + vec2(8., 4.);
                                let min = pos2(
                                    r.left().clamp(rect.left(), (rect.right() - size.x).max(rect.left())),
                                    (r.top() - size.y - 2.).max(rect.top()),
                                );
                                label_plate(&p, Rect::from_min_size(min + vec2(4., 2.), galley.size()));
                                p.galley(min + vec2(4., 2.), galley, color);
                            }
                        }
                    }
                    Shape::Polygon(points) => {
                        let points: Vec<_> = points.iter().map(|p| pos2(p[0] as f32, p[1] as f32)).collect();
                        p.add(egui::Shape::convex_polygon(points, color.gamma_multiply(0.08), stroke));
                    }
                }
            }
        }
    }

    /// Congress trade markers. They pop in once the reveal sweep has passed their bar, and
    /// left to right when they arrive after it.
    fn paint_markers(
        &mut self,
        ui: &Ui,
        rect: Rect,
        v: &Viewport,
        s: &PriceScale,
        hover: Option<Pos2>,
        reveal: &Reveal,
    ) {
        if self.markers.is_empty() || self.bars.is_empty() {
            return;
        }
        let arrive = crate::anim::progress(ui.ctx(), egui::Id::new(("chart-markers", self.id)), self.markers_key, 0.6);
        let p = ui.painter_at(rect);
        let len = self.bars.len();
        let (lo, hi) = v.visible_range(len);
        // Stack multiple trades on the same bar.
        let mut stack: std::collections::HashMap<(usize, bool), u32> = std::collections::HashMap::new();
        let full_r = (v.bar_px as f32 * 0.45).clamp(4.0, 7.0);
        let mut best: Option<(f32, usize, Pos2)> = None;
        for (mi, t) in self.markers.iter().enumerate() {
            let Some(ts) = t.tx_date.and_hms_opt(12, 0, 0).map(|d| d.and_utc().timestamp()) else { continue };
            let idx = self.index_of_ts(ts).floor();
            if idx < lo as f64 || idx >= hi as f64 || idx < 0.0 || idx >= len as f64 {
                continue;
            }
            let i = idx as usize;
            let bar = self.bars[i];
            let sell = t.tx_type.is_sell();
            let n = stack.entry((i, sell)).or_insert(0);
            let x = rect.left() + v.x_center(i) as f32;
            let off = 6.0 + full_r + *n as f32 * (2.0 * full_r + 1.0);
            let swept = ((reveal.front - (i as f64 + 0.5)) / reveal.ramp as f64).clamp(0.0, 1.0) as f32;
            let lead = 0.5 * (x - rect.left()) / rect.width().max(1.0);
            let pop = crate::anim::ease_out_back(swept.min(crate::anim::span(arrive, lead, lead + 0.5)));
            if pop <= 0.01 {
                *n += 1;
                continue;
            }
            let r = full_r * pop;
            let y = if sell {
                rect.top() + s.y_of(bar.high) as f32 - off
            } else {
                rect.top() + s.y_of(bar.low) as f32 + off
            };
            *n += 1;
            let c = Pos2::new(x, y);
            let col = theme::party_color(t.party);
            p.circle_filled(c, r, col);
            if full_r >= 5.0 {
                let letter = match t.tx_type {
                    TxType::Purchase => "B",
                    TxType::Sale | TxType::SalePartial => "S",
                    _ => "E",
                };
                let fade = crate::anim::span(pop, 0.5, 1.0);
                p.text(c, Align2::CENTER_CENTER, letter, mono(full_r * 1.6), Color32::WHITE.gamma_multiply(fade));
            } else {
                p.circle_stroke(c, r, Stroke::new(1.0, Color32::WHITE));
            }
            if let Some(h) = hover {
                let d = h.distance(c);
                if d <= r + 3.0 && best.is_none_or(|b| d < b.0) {
                    best = Some((d, mi, c));
                }
            }
        }
        if let Some((_, mi, c)) = best {
            self.hover_marker = Some(mi);
            let t = &self.markers[mi];
            hover_card(ui, c, t);
        }
    }
}

fn live_badge_rect(plot: Rect, text_width: f32) -> Rect {
    Rect::from_min_size(pos2(plot.right() - text_width - 22., plot.top() + 30.), vec2(text_width + 16., 14.))
}

fn compare_overlay(line: &LineRef, scale: f64) -> Overlay {
    Overlay {
        line: line.clone(),
        color: theme::rgba(theme::compare(0)),
        kind: LineKind::Line,
        scale,
        width: 1.3,
        histogram_color: None,
    }
}

fn last_finite(line: &LineRef) -> Option<f64> {
    let data = &line.pyramid.data;
    (0..line.pyramid.level_len(0)).rev().find_map(|i| Some(data[i][1] as f64).filter(|x| x.is_finite()))
}

/// `price` as a % change from `base`.
fn percent(price: f64, base: f64) -> f64 {
    (price / base - 1.0) * 100.0
}

fn format_percent(p: f64, decimals: usize) -> String {
    if p.abs() < 0.5 * 10f64.powi(-(decimals as i32)) { "0%".into() } else { format!("{p:+.decimals$}%") }
}

fn axis_value(p: f64, decimals: usize, axis: CompareAxis) -> String {
    if axis == CompareAxis::Indexed { format!("{:.*}", decimals, 100. + p) } else { format_percent(p, decimals) }
}

fn study_source_label(source: &StudySource, specs: &[IndicatorSpec]) -> String {
    match source {
        StudySource::Bars => "Original bars".into(),
        StudySource::Open => "Open".into(),
        StudySource::High => "High".into(),
        StudySource::Low => "Low".into(),
        StudySource::Close => "Close".into(),
        StudySource::Volume => "Volume".into(),
        StudySource::Hlc3 => "Typical price (HLC3)".into(),
        StudySource::Output { study, output } => {
            let label = specs
                .iter()
                .find(|s| s.chart.as_ref().is_some_and(|c| c.id == *study))
                .map_or_else(|| "Missing indicator".into(), IndicatorSpec::label);
            format!("{label} #{study} · {output}")
        }
    }
}

fn study_fixed_range(spec: &IndicatorSpec) -> Option<(f64, f64)> {
    match spec.chart.as_ref().map(|c| c.scale) {
        Some(StudyScale::Fixed { lo, hi }) if lo.is_finite() && hi.is_finite() && lo < hi => Some((lo, hi)),
        _ => spec.def().and_then(|d| d.range),
    }
}

/// Group visible studies by stable pane identity, independent of calculation input.
fn study_groups(built: &[BuiltIndicator]) -> Vec<(Option<u32>, Vec<usize>)> {
    let specs: Vec<_> = built.iter().map(|b| b.spec.clone()).collect();
    let mut groups: Vec<(Option<u32>, Vec<usize>)> = Vec::new();
    for (i, spec) in specs.iter().enumerate() {
        if spec.chart.as_ref().is_some_and(|c| !c.visible) {
            continue;
        }
        let key = study::pane_key(&specs, i).unwrap_or(Some(i as u32 + 1));
        if let Some((_, members)) = groups.iter_mut().find(|(group, _)| *group == key) {
            members.push(i);
        } else {
            groups.push((key, vec![i]));
        }
    }
    groups
}

fn opaque(o: &Overlay) -> Color32 {
    let [r, g, b, _] = o.color.map(|c| (c.clamp(0.0, 1.0) * 255.0) as u8);
    Color32::from_rgb(r, g, b)
}

fn dashed_h(p: &egui::Painter, x: egui::Rangef, y: f32, s: Stroke) {
    let mut a = x.min;
    while a < x.max {
        p.hline(a..=(a + 4.0).min(x.max), y, s);
        a += 7.0;
    }
}

fn dashed_v(p: &egui::Painter, x: f32, y: egui::Rangef, s: Stroke) {
    let mut a = y.min;
    while a < y.max {
        p.vline(x, a..=(a + 4.0).min(y.max), s);
        a += 7.0;
    }
}

/// Hover card for a congressional trade marker, including the disclosure lag.
fn hover_card(ui: &Ui, at: Pos2, t: &Trade) {
    let lines = [
        (
            format!("{} ({}-{})", t.politician, t.party.code(), t.state.as_deref().unwrap_or("??")),
            theme::party_color(t.party),
        ),
        (format!("{} · {}", t.chamber.as_str(), t.owner.as_deref().unwrap_or("Self")), muted()),
        (
            format!(
                "{} {}  {}",
                match t.tx_type {
                    TxType::Purchase => "BUY",
                    TxType::Sale => "SELL",
                    TxType::SalePartial => "SELL (partial)",
                    TxType::Exchange => "EXCHANGE",
                    TxType::Other => "OTHER",
                },
                t.ticker.as_deref().unwrap_or("-"),
                t.amount.map(|a| a.to_string()).unwrap_or_else(|| "amount n/a".into())
            ),
            if t.tx_type.is_buy() { theme::up() } else { theme::down() },
        ),
        (format!("Traded  {}", t.tx_date), text()),
        (format!("Filed   {}", t.filed_date), text()),
        (
            format!("Disclosure lag {} days", t.disclosure_lag_days()),
            if t.disclosure_lag_days() > 45 { theme::down() } else { amber() },
        ),
    ];
    let w = lines.iter().map(|(l, _)| l.len()).max().unwrap_or(10) as f32 * 7.2 + 14.0;
    let h = lines.len() as f32 * 14.0 + 8.0;
    let screen = ui.ctx().content_rect();
    let mut min = at + vec2(12.0, 12.0);
    if min.x + w > screen.right() {
        min.x = at.x - w - 12.0;
    }
    if min.y + h > screen.bottom() {
        min.y = at.y - h - 12.0;
    }
    let card = Rect::from_min_size(min, vec2(w, h));
    let layer = egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("congress-card"));
    let p = ui.ctx().layer_painter(layer);
    p.rect_filled(card, 0.0, Color32::from_rgb(16, 16, 16));
    p.rect_stroke(card, 0.0, Stroke::new(1.0, amber_dim()), StrokeKind::Inside);
    for (k, (l, c)) in lines.iter().enumerate() {
        p.text(card.left_top() + vec2(7.0, 4.0 + k as f32 * 14.0), Align2::LEFT_TOP, l, mono(11.5), *c);
    }
}

/// Follow the newest bar without changing zoom. A paused manual pan is retained
/// while the new bar remains visible; start/exit always return to the right edge.
fn replay_viewport(mut view: Viewport, old_len: usize, new_len: usize, reanchor: bool) -> Viewport {
    if old_len == new_len && !reanchor {
        return view;
    }
    let newest = new_len as f64 - 0.5;
    if reanchor || view.right >= old_len as f64 - 1. || newest < view.left() || newest > view.right {
        view.right = new_len as f64 + 3.;
    }
    view
}

/// Return tag centres in input order, using a stable vertical sort and 1px gaps.
/// The fixed tag divides the sweep into two independent blocks. Bounds and gaps
/// are guaranteed when each block fits; otherwise fixed position and gaps win.
fn spread_tags(desired: &[(f32, f32)], fixed: Option<usize>, lo: f32, hi: f32) -> Vec<f32> {
    fn block(indices: &[usize], desired: &[(f32, f32)], out: &mut [f32], lo: f32, hi: f32) {
        let mut edge = lo;
        for &i in indices {
            let half = desired[i].1 / 2.;
            out[i] = desired[i].0.max(edge + half);
            edge = out[i] + half + 1.;
        }
        // Shift overflowing tags up, propagating only as far as necessary.
        edge = hi;
        for &i in indices.iter().rev() {
            let half = desired[i].1 / 2.;
            out[i] = out[i].min(edge - half);
            edge = out[i] - half - 1.;
        }
    }
    let mut indices: Vec<_> = (0..desired.len()).collect();
    indices.sort_by(|&a, &b| desired[a].0.total_cmp(&desired[b].0));
    let mut out = vec![0.; desired.len()];
    if let Some((position, i)) = fixed.and_then(|i| indices.iter().position(|&j| j == i).map(|p| (p, i))) {
        let (y, height) = desired[i];
        out[i] = y;
        block(&indices[..position], desired, &mut out, lo, y - height / 2. - 1.);
        block(&indices[position + 1..], desired, &mut out, y + height / 2. + 1., hi);
    } else {
        block(&indices, desired, &mut out, lo, hi);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tags_spread_stably_with_variable_heights_and_bounds() {
        for (desired, fixed) in [
            (vec![(80., 16.), (20., 16.), (50., 16.)], None),
            (vec![(90., 16.), (90., 20.), (91., 12.)], None),
            (vec![(2., 16.), (3., 20.), (4., 12.)], None),
            (vec![(50., 16.), (50., 16.), (50., 16.)], Some(1)),
            (vec![(45., 20.), (50., 16.), (51., 12.)], Some(1)),
            (vec![], None),
        ] {
            let out = spread_tags(&desired, fixed, 0., 100.);
            let mut sorted: Vec<_> = (0..desired.len()).collect();
            sorted.sort_by(|&a, &b| desired[a].0.total_cmp(&desired[b].0));
            for &i in &sorted {
                assert!(out[i] - desired[i].1 / 2. >= 0.);
                assert!(out[i] + desired[i].1 / 2. <= 100.);
            }
            for pair in sorted.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                assert!(out[b] - out[a] >= (desired[a].1 + desired[b].1) / 2. + 1.);
            }
            if let Some(i) = fixed {
                assert_eq!(out[i], desired[i].0);
            }
        }
        assert_eq!(spread_tags(&[(20., 16.), (60., 16.)], None, 0., 100.), vec![20., 60.]);
    }

    #[test]
    fn replay_viewport_follows_transitions_and_offscreen_bars_but_keeps_manual_pan() {
        let view = Viewport { right: 103., bar_px: 8., width_px: 400. };
        let start = replay_viewport(view, 100, 50, true);
        assert_eq!(start.right, 53.);
        assert_eq!(start.visible_bars(), view.visible_bars());
        assert_eq!(replay_viewport(start, 50, 51, false).right, 54.);
        assert_eq!(replay_viewport(start, 50, 10, false).right, 13.);
        assert_eq!(replay_viewport(start, 50, 100, true).right, 103.);
        let panned = Viewport { right: 70., ..view };
        assert_eq!(replay_viewport(panned, 100, 60, false), panned);
        assert_eq!(replay_viewport(panned, 100, 100, false), panned);
        assert_eq!(replay_viewport(panned, 100, 101, false).right, 104.);
        assert_eq!(replay_viewport(panned, 100, 10, false).right, 13.);
    }

    #[test]
    fn symbol_changes_keep_session_preferences_and_reset_prices() {
        let mut c = ChartState::new(1, "AAPL US Equity", Timeframe::D1);
        let w = Worker::inert();
        c.data.session = tr_core::session::Session::UsRegular;
        c.data.history.extended_hours = true;
        c.data.analytics.baseline = Some(150.);
        c.data.analytics.renko_size = 5.;
        c.set_symbol("BTCUSD Curncy", &w);
        assert_eq!(c.data.analytics.baseline, None);
        assert_eq!(c.data.analytics.renko_size, 0.);
        assert_eq!(c.effective_session(), tr_core::session::Session::Provider);
        assert_eq!(c.effective_history(), tr_data::HistoryOptions::default());
        c.set_symbol("AAPL US Equity", &w);
        assert_eq!(c.data.session, tr_core::session::Session::UsRegular);
        assert_eq!(c.effective_session(), tr_core::session::Session::UsRegular);
        assert!(c.data.history.extended_hours);
    }

    #[test]
    fn scan_plot_replaces_previous_expression() {
        let mut c = ChartState::new(1, "AAPL US Equity", Timeframe::D1);
        let w = Worker::inert();
        c.indicators.clear();
        c.data.workflow.scan = Some(Default::default());
        c.plot_scan(&w);
        let first = c.indicators[0].clone();
        c.data.workflow.scan.as_mut().unwrap().conditions[0].right_multiplier = 2.;
        c.plot_scan(&w);
        c.plot_scan(&w);
        assert_eq!(c.indicators.len(), 1);
        let settings = c.indicators[0].chart.as_ref().unwrap();
        assert!(settings.scan_plot);
        assert_eq!(settings.id, first.chart.as_ref().unwrap().id);
        assert_ne!(settings.expression, first.chart.as_ref().unwrap().expression);
    }

    #[test]
    fn replay_keeps_frame_and_viewport_across_ticks_and_steps() {
        let mut c = ChartState::new(1, "AAPL US Equity", Timeframe::D1);
        let w = Worker::inert();
        let source = Arc::new(tr_core::sample::generate("AAPL", Timeframe::D1, 100, 1_790_000_000));
        c.on_history(c.generation, source.clone(), SeriesGpu::build(&source), 0, false, false, false, &w);
        c.replay_cutoff = Some(source[49].ts);
        c.refresh_view(&w);
        let past = Arc::new(source[..50].to_vec());
        c.on_view(c.generation, c.view_revision, source.clone(), Ok((past.clone(), SeriesGpu::build(&past))), &w);
        let frame = c.bars.clone();
        let mut updated = source.as_ref().clone();
        updated[99].close += 1.;
        c.source_bars = Arc::new(updated);
        let revision = c.view_revision;
        c.refresh_view(&w);
        assert_eq!(c.view_revision, revision);
        assert!(Arc::ptr_eq(&frame, &c.bars));
        for (step, right) in [30., 54.].into_iter().enumerate() {
            c.viewport = Some(Viewport { right, bar_px: 8., width_px: 500. });
            c.replay_cutoff = Some(source[50 + step].ts);
            c.refresh_view(&w);
            assert!(Arc::ptr_eq(&frame, &c.bars) || c.bars.len() == 51);
            let next = Arc::new(source[..51 + step].to_vec());
            let reveal = c.reveal_key;
            c.on_view(c.generation, c.view_revision, source.clone(), Ok((next.clone(), SeriesGpu::build(&next))), &w);
            let view = c.viewport.unwrap();
            assert_eq!(view.bar_px, 8.);
            assert_eq!(view.right, if right == 30. { 54. } else { 55. });
            assert_eq!(c.reveal_key, reveal);
        }
        let mut changed = c.source_bars.as_ref().clone();
        changed[50].close += 1.;
        c.source_bars = Arc::new(changed);
        c.refresh_view(&w);
        assert!(c.view_revision > revision);
    }

    #[test]
    fn replay_retains_frame_and_rejects_older_views() {
        let mut c = ChartState::new(1, "AAPL US Equity", Timeframe::D1);
        let w = Worker::inert();
        let source = Arc::new(tr_core::sample::generate("AAPL", Timeframe::D1, 100, 1_790_000_000));
        c.on_history(c.generation, source.clone(), SeriesGpu::build(&source), 0, false, false, false, &w);
        assert_eq!(c.bars.len(), 100);
        c.replay_cutoff = Some(source[49].ts);
        c.refresh_view(&w);
        let old_revision = c.view_revision;
        assert_eq!(c.bars.len(), 100);
        c.replay_cutoff = Some(source[19].ts);
        c.refresh_view(&w);
        let past = Arc::new(source[..50].to_vec());
        c.on_view(c.generation, old_revision, source.clone(), Ok((past.clone(), SeriesGpu::build(&past))), &w);
        assert_eq!(c.bars.len(), 100);
        let past = Arc::new(source[..20].to_vec());
        c.on_view(c.generation, c.view_revision, source.clone(), Ok((past.clone(), SeriesGpu::build(&past))), &w);
        assert_eq!(c.bars.len(), 20);
        assert_eq!(c.source_bars.len(), 100);
        c.replay_cutoff = None;
        c.refresh_view(&w);
        c.on_view(c.generation, c.view_revision, source.clone(), Ok((source.clone(), SeriesGpu::build(&source))), &w);
        assert_eq!(c.bars.len(), 100);
    }

    #[test]
    fn short_replay_preserves_zoom_when_the_chart_is_drawn() {
        let ctx = egui::Context::default();
        let mut chart = sample_chart();
        let w = Worker::inert();
        let source = chart.bars.clone();
        chart.source_bars = source.clone();
        chart.viewport = Some(Viewport { right: 503., bar_px: 2., width_px: 700. });
        chart.replay_cutoff = Some(source[9].ts);
        chart.refresh_view(&w);
        let past = Arc::new(source[..10].to_vec());
        chart.on_view(chart.generation, chart.view_revision, source, Ok((past.clone(), SeriesGpu::build(&past))), &w);
        chart_frame(&ctx, &mut chart, vec![]);
        let view = chart.viewport.unwrap();
        assert_eq!(view.bar_px, 2.);
        assert_eq!(view.right, 13.);
        // A paused user pan survives subsequent frames with no cutoff movement.
        chart.viewport.as_mut().unwrap().right = 8.;
        chart_frame(&ctx, &mut chart, vec![]);
        assert_eq!(chart.viewport.unwrap().right, 8.);
    }

    #[test]
    fn replay_shortcuts_consume_navigation_only_during_replay() {
        let ctx = egui::Context::default();
        let worker = Worker::inert();
        let mut chart = sample_chart();
        chart.source_bars = chart.bars.clone();
        let press = |chart: &mut ChartState, key| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ui| {
                    let active = chart.replay_cutoff.is_some();
                    assert_eq!(chart.replay_keys(ui.ctx(), &worker), active);
                    assert_eq!(ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key)), !active);
                },
            );
            output.textures_delta.clear();
        };
        press(&mut chart, egui::Key::ArrowRight);
        chart.replay_cutoff = Some(chart.source_bars[0].ts);
        press(&mut chart, egui::Key::ArrowRight);
        assert_eq!(chart.replay_cutoff, Some(chart.source_bars[1].ts));
        press(&mut chart, egui::Key::ArrowLeft);
        assert_eq!(chart.replay_cutoff, Some(chart.source_bars[0].ts));
        press(&mut chart, egui::Key::Space);
        assert!(chart.replay_play);
        press(&mut chart, egui::Key::Escape);
        assert!(chart.replay_cutoff.is_none());
        assert!(!chart.replay_play);
    }

    #[test]
    fn comparison_slots_migrate_and_round_trip_with_chart_data() {
        let worker = Worker::inert();
        let mut chart = sample_chart();
        chart.data = serde_json::from_str(r#"{"comparisons":["SPY US Equity","QQQ US Equity"]}"#).unwrap();
        chart.load(&worker);
        assert_eq!(chart.data.compare_slots, vec![("SPY US Equity".into(), 0), ("QQQ US Equity".into(), 1)]);
        chart.compare = Some(chart.data.comparisons.remove(0));
        chart.sync_compare_slots();
        assert_eq!(crate::chart_compare::slot(&chart.data.compare_slots, "QQQ US Equity"), 1);
        let json = serde_json::to_string(&chart.data).unwrap();
        let restored: crate::chart_data::ChartData = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.compare_slots, chart.data.compare_slots);
    }

    #[test]
    fn replay_slider_knob_tracks_cutoff_across_the_full_source_history() {
        let ctx = egui::Context::default();
        let mut chart = sample_chart();
        chart.source_bars = chart.bars.clone();
        let len = chart.source_bars.len();
        let mut positions = Vec::new();
        for index in [0, len / 2, len - 1] {
            chart.replay_cutoff = Some(chart.source_bars[index].ts);
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900., 560.))),
                    ..Default::default()
                },
                |ui| {
                    ui.horizontal(|ui| chart.replay_controls(ui, &Worker::inert()));
                },
            );
            output.textures_delta.clear();
            let knob = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect) if rect.stroke.width > 0. => Some(rect.rect.center().x),
                    _ => None,
                })
                .expect("slider knob");
            positions.push(knob);
            assert_eq!(chart.replay_cutoff, Some(chart.source_bars[index].ts));
            assert!(output.shapes.iter().any(|shape| matches!(
                &shape.shape,
                egui::Shape::Text(text) if text.galley.job.text.starts_with(&format!("Bar {} / {len} · ", index + 1))
            )));
        }
        let fraction = (positions[1] - positions[0]) / (positions[2] - positions[0]);
        assert!((fraction - 0.5).abs() < 0.01, "knob positions: {positions:?}");
        assert!(positions[2] > positions[0], "flex slider must span the available history");
    }

    #[test]
    fn percent_axis_labels() {
        assert!((percent(110.0, 100.0) - 10.0).abs() < 1e-9);
        assert_eq!(format_percent(10.0, 1), "+10.0%");
        assert_eq!(format_percent(-2.5, 2), "-2.50%");
        assert_eq!(format_percent(0.001, 2), "0%");
    }

    fn ui_frame(ctx: &egui::Context, events: Vec<egui::Event>, mut draw: impl FnMut(&mut Ui)) -> Vec<(String, Rect)> {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900., 560.))),
                time: Some(ctx.input(|i| i.time) + 0.2),
                events,
                ..Default::default()
            },
            |ui| draw(ui),
        );
        let texts = output
            .shapes
            .iter()
            .filter_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    Some((text.galley.job.text.clone(), text.galley.rect.translate(text.pos.to_vec2())))
                } else {
                    None
                }
            })
            .collect();
        output.textures_delta.clear();
        texts
    }
    fn pointer_button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }
    fn sample_chart() -> ChartState {
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        chart.indicators = vec![IndicatorSpec::new("EMA", &[20.]).unwrap()];
        study::assign_ids(&mut chart.indicators);
        chart.bars = Arc::new(tr_core::sample::generate("AAPL", chart.tf, 500, 1_790_000_000));
        chart.gpu = Some(SeriesGpu::build(&chart.bars));
        chart.built = crate::worker::build_indicators(&chart.bars, chart.tf, &chart.indicators, &theme::palette());
        chart
    }
    fn chart_frame(ctx: &egui::Context, chart: &mut ChartState, events: Vec<egui::Event>) -> Vec<(String, Rect)> {
        ui_frame(ctx, events, |ui| {
            chart.show(ui, ui.max_rect(), true, &Worker::inert());
        })
    }
    fn click_chart(ctx: &egui::Context, chart: &mut ChartState, pos: Pos2) -> Vec<(String, Rect)> {
        chart_frame(ctx, chart, vec![egui::Event::PointerMoved(pos)]);
        chart_frame(ctx, chart, vec![pointer_button(pos, true)]);
        chart_frame(ctx, chart, vec![pointer_button(pos, false)]);
        chart_frame(ctx, chart, vec![])
    }

    fn drawing_frame(
        ctx: &egui::Context,
        chart: &mut ChartState,
        events: Vec<egui::Event>,
        log: bool,
    ) -> Vec<(String, Rect)> {
        ui_frame(ctx, events, |ui| {
            chart.show_note_editor(ui.ctx());
            let rect = Rect::from_min_size(Pos2::ZERO, vec2(800., 400.));
            let v = Viewport { right: 100., bar_px: 8., width_px: 800. };
            let scale = PriceScale::new(100., 200., log, 400.);
            let response = ui.interact(rect, egui::Id::new("expanded-drawing-test"), Sense::click_and_drag());
            chart.paint_drawings(ui, rect, &v, &scale, ui.input(|i| i.pointer.hover_pos()), &response);
        })
    }

    fn drawing_chart() -> ChartState {
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        chart.bars = Arc::new((0..100).map(|i| Bar::new(i * 86400, 150., 160., 140., 150., 1.)).collect());
        chart
    }

    fn click_drawing(ctx: &egui::Context, chart: &mut ChartState, at: Pos2, log: bool) -> Vec<(String, Rect)> {
        drawing_frame(ctx, chart, vec![egui::Event::PointerMoved(at)], log);
        drawing_frame(ctx, chart, vec![pointer_button(at, true)], log);
        drawing_frame(ctx, chart, vec![pointer_button(at, false)], log);
        drawing_frame(ctx, chart, vec![], log)
    }

    #[test]
    fn measure_zone_and_ray_place_select_delete_and_undo_without_losing_anchors() {
        for tool in [Tool::Measure, Tool::Rectangle, Tool::Ray] {
            let ctx = egui::Context::default();
            let mut chart = drawing_chart();
            chart.start_tool(tool);
            click_drawing(&ctx, &mut chart, pos2(320., 300.), false);
            assert!(chart.drawings.is_empty());
            let labels = click_drawing(&ctx, &mut chart, pos2(480., 100.), false);
            assert_eq!(chart.drawings.len(), 1);
            let drawing = chart.drawings[0].clone();
            assert_eq!(drawing.symbol, Some(chart.symbol.clone()));
            assert_eq!(drawing.a.price, 125.);
            assert_eq!(drawing.b.price, 175.);
            if tool == Tool::Measure {
                assert!(
                    labels
                        .iter()
                        .any(|(s, _)| s.contains("+50.00 (+40.00%)") && s.contains("20 bars") && s.contains("20d"))
                );
            }
            click_drawing(&ctx, &mut chart, pos2(400., 200.), false);
            assert_eq!(chart.draw.selected, Some(0));
            assert!(chart.delete_selected());
            assert!(chart.drawings.is_empty());
            chart.undo_drawings();
            assert_eq!(chart.drawings[0], drawing);
            chart.redo_drawings();
            assert!(chart.drawings.is_empty());
        }
    }

    #[test]
    fn channel_requires_three_points_and_width_edits_undo_on_log_scale() {
        let ctx = egui::Context::default();
        let mut chart = drawing_chart();
        chart.start_tool(Tool::Channel);
        click_drawing(&ctx, &mut chart, pos2(320., 300.), true);
        click_drawing(&ctx, &mut chart, pos2(480., 100.), true);
        assert!(chart.drawings.is_empty());
        assert!(chart.draw.second.is_some());
        click_drawing(&ctx, &mut chart, pos2(480., 200.), true);
        let original = chart.drawings[0].clone();
        assert!(original.c.is_some());
        let from = pos2(480., 200.);
        let to = pos2(480., 250.);
        drawing_frame(&ctx, &mut chart, vec![egui::Event::PointerMoved(from)], true);
        drawing_frame(&ctx, &mut chart, vec![pointer_button(from, true)], true);
        drawing_frame(&ctx, &mut chart, vec![egui::Event::PointerMoved(to)], true);
        drawing_frame(&ctx, &mut chart, vec![pointer_button(to, false)], true);
        assert_eq!(chart.drawings[0].a, original.a);
        assert_eq!(chart.drawings[0].b, original.b);
        assert_ne!(chart.drawings[0].c, original.c);
        chart.undo_drawings();
        assert_eq!(chart.drawings[0], original);
        // Moving the band translates its width anchor too, preserving log ratios.
        let from = pos2(425., 220.);
        let to = pos2(445., 240.);
        drawing_frame(&ctx, &mut chart, vec![egui::Event::PointerMoved(from)], true);
        drawing_frame(&ctx, &mut chart, vec![pointer_button(from, true)], true);
        drawing_frame(&ctx, &mut chart, vec![egui::Event::PointerMoved(to)], true);
        drawing_frame(&ctx, &mut chart, vec![pointer_button(to, false)], true);
        let moved = &chart.drawings[0];
        assert_ne!(moved.a, original.a);
        assert_ne!(moved.b, original.b);
        assert_ne!(moved.c, original.c);
        assert!((moved.c.unwrap().price / moved.a.price - original.c.unwrap().price / original.a.price).abs() < 1e-8);
        chart.undo_drawings();
        assert_eq!(chart.drawings[0], original);
        chart.start_tool(Tool::Channel);
        click_drawing(&ctx, &mut chart, pos2(200., 150.), true);
        assert!(chart.cancel());
        assert!(chart.draw.pending.is_none() && chart.draw.second.is_none());
        assert_eq!(chart.drawings.len(), 1);
    }

    #[test]
    fn note_creation_cancellation_editing_and_undo_are_transactional() {
        let ctx = egui::Context::default();
        let mut chart = drawing_chart();
        chart.start_tool(Tool::Text);
        click_drawing(&ctx, &mut chart, pos2(500., 300.), false);
        assert!(chart.drawings.is_empty());
        assert!(chart.draw.note.is_some());
        chart.cancel();
        assert!(chart.drawings.is_empty() && chart.drawing_undo.is_empty());
        chart.start_tool(Tool::Text);
        click_drawing(&ctx, &mut chart, pos2(500., 300.), false);
        chart.draw.note.as_mut().unwrap().text = "Watch support\nWait for volume".into();
        drawing_frame(&ctx, &mut chart, vec![], false);
        let labels = drawing_frame(&ctx, &mut chart, vec![], false);
        let save = labels.iter().find(|(s, _)| s == "Save note").unwrap().1.center();
        click_drawing(&ctx, &mut chart, save, false);
        assert_eq!(chart.drawings.len(), 1);
        let original = chart.drawings[0].clone();
        assert_eq!(original.text, "Watch support\nWait for volume");
        chart.draw.note =
            Some(NoteDraft { anchor: original.a, existing: Some((0, original.clone())), text: "Updated note".into() });
        drawing_frame(&ctx, &mut chart, vec![], false);
        let labels = drawing_frame(&ctx, &mut chart, vec![], false);
        let save = labels.iter().find(|(s, _)| s == "Save note").unwrap().1.center();
        click_drawing(&ctx, &mut chart, save, false);
        assert_eq!(chart.drawings[0].text, "Updated note");
        chart.undo_drawings();
        assert_eq!(chart.drawings[0], original);
        chart.undo_drawings();
        assert!(chart.drawings.is_empty());
    }
    #[test]
    fn advanced_drawings_place_three_points_and_keep_undo() {
        for tool in [Tool::FibExtension, Tool::Pitchfork, Tool::LongPosition, Tool::ShortPosition] {
            let ctx = egui::Context::default();
            let mut chart = drawing_chart();
            chart.start_tool(tool);
            click_drawing(&ctx, &mut chart, pos2(400., 200.), false);
            click_drawing(&ctx, &mut chart, pos2(500., if tool == Tool::ShortPosition { 100. } else { 300. }), false);
            assert!(chart.drawings.is_empty());
            click_drawing(&ctx, &mut chart, pos2(600., if tool == Tool::ShortPosition { 300. } else { 100. }), false);
            assert_eq!(chart.drawings.len(), 1);
            assert!(chart.drawings[0].c.is_some());
            if matches!(tool, Tool::LongPosition | Tool::ShortPosition) {
                assert_eq!(chart.drawings[0].risk_reward(), Some((25., 25., 1.)));
            }
            chart.undo_drawings();
            assert!(chart.drawings.is_empty());
            chart.redo_drawings();
            assert_eq!(chart.drawings.len(), 1);
        }
    }

    #[test]
    fn legend_clicks_hide_and_restore_without_placing_a_drawing_or_panning() {
        let ctx = egui::Context::default();
        let mut chart = sample_chart();
        chart.start_tool(Tool::Horizontal);
        let texts = chart_frame(&ctx, &mut chart, vec![]);
        let row = texts.iter().find(|(text, _)| text.starts_with("EMA 20")).unwrap().1.center();
        let right = chart.viewport.unwrap().right;
        let menu = click_chart(&ctx, &mut chart, row);
        let show = menu.iter().find(|(text, _)| text == "Show on chart").expect("legend opens study menu").1.center();
        click_chart(&ctx, &mut chart, show);
        assert!(!chart.indicators[0].chart.as_ref().unwrap().visible);
        assert_eq!(chart.viewport.unwrap().right, right);
        assert!(chart.drawings.is_empty());
        let texts = chart_frame(&ctx, &mut chart, vec![]);
        let hidden =
            texts.iter().find(|(text, _)| text.starts_with("EMA 20") && text.contains("hidden")).unwrap().1.center();
        let menu = click_chart(&ctx, &mut chart, hidden);
        let show = menu.iter().find(|(text, _)| text == "Show on chart").unwrap().1.center();
        click_chart(&ctx, &mut chart, show);
        assert!(chart.indicators[0].chart.as_ref().unwrap().visible);
        assert!(chart.drawings.is_empty());
    }

    #[test]
    fn oscillator_legend_can_open_its_indicator_editor() {
        let ctx = egui::Context::default();
        let mut chart = sample_chart();
        chart.indicators.push(IndicatorSpec::new("RSI", &[14.]).unwrap());
        study::assign_ids(&mut chart.indicators);
        chart.built = crate::worker::build_indicators(&chart.bars, chart.tf, &chart.indicators, &theme::palette());
        let texts = chart_frame(&ctx, &mut chart, vec![]);
        let oscillator = texts
            .iter()
            .find(|(text, _)| text.starts_with("RSI 14"))
            .expect("oscillator has a legend target")
            .1
            .center();
        let menu = click_chart(&ctx, &mut chart, oscillator);
        let edit = menu.iter().find(|(text, _)| text == "Edit indicator…").unwrap().1.center();
        // Inspect immediately after release, before the next frame consumes the focus request.
        chart_frame(&ctx, &mut chart, vec![egui::Event::PointerMoved(edit), pointer_button(edit, true)]);
        chart_frame(&ctx, &mut chart, vec![pointer_button(edit, false)]);
        assert!(chart.indicator_editor);
        assert_eq!(chart.indicator_focus, Some(2));
    }

    #[test]
    fn chart_price_context_creates_an_alert_with_the_clicked_level_and_direction() {
        let ctx = egui::Context::default();
        let mut chart = sample_chart();
        chart.manual_scale = Some(PriceScale::new(50., 250., false, 400.));
        chart_frame(&ctx, &mut chart, vec![]);
        let at = pos2(700., 300.);
        chart_frame(&ctx, &mut chart, vec![egui::Event::PointerMoved(at)]);
        for pressed in [true, false] {
            chart_frame(
                &ctx,
                &mut chart,
                vec![egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
        let texts = chart_frame(&ctx, &mut chart, vec![]);
        let level = chart.context_price.expect("Price under the context click");
        assert!(level > 50. && level < 250.);
        let create = texts.iter().find(|(s, _)| s == "Create price alert here…").unwrap().1.center();
        click_chart(&ctx, &mut chart, create);
        assert_eq!(chart.price_alert_actions.len(), 1);
        let crate::price_alerts::Action::Create { symbol, level: saved, direction } = &chart.price_alert_actions[0]
        else {
            panic!("Expected new alert");
        };
        assert_eq!(symbol, &chart.symbol);
        assert_eq!(*saved, level);
        let expected = if level < chart.bars.last().unwrap().close {
            tr_core::price_alerts::Direction::Below
        } else {
            tr_core::price_alerts::Direction::Above
        };
        assert_eq!(*direction, expected);
        assert!(chart.drawings.is_empty());
    }

    #[test]
    fn comparison_picker_selects_and_removes_a_symbol_without_a_command() {
        let ctx = egui::Context::default();
        let mut chart = sample_chart();
        chart.compare_picker.open();
        chart_frame(&ctx, &mut chart, vec![]);
        let texts = chart_frame(&ctx, &mut chart, vec![]);
        let candidate = texts
            .iter()
            .find(|(text, _)| text.starts_with("SPX Index  ·"))
            .expect("local catalog is available without provider search")
            .1
            .center();
        click_chart(&ctx, &mut chart, candidate);
        assert_eq!(chart.compare.as_deref(), Some("SPX Index"));
        assert!(chart.compare_loading);
        assert!(!chart.compare_picker.open);
        chart.compare_picker.open();
        chart_frame(&ctx, &mut chart, vec![]);
        let texts = chart_frame(&ctx, &mut chart, vec![]);
        let remove = texts.iter().find(|(text, _)| text == "Remove").unwrap().1.center();
        click_chart(&ctx, &mut chart, remove);
        assert!(chart.compare.is_none());
        assert!(chart.compare_line.is_none());
        assert!(!chart.compare_loading);
    }

    #[test]
    fn charts_without_loaded_comparisons_have_no_percent_base() {
        let ctx = egui::Context::default();
        for chart_type in [ChartType::Candles, ChartType::Renko] {
            for anchored in [false, true] {
                let mut chart = sample_chart();
                chart.chart_type = chart_type;
                chart.data.analytics.anchor = anchored.then_some(chart.bars[0].ts);
                chart_frame(&ctx, &mut chart, vec![]);
                assert!(chart.compare_base.is_none());
            }
        }
    }

    #[test]
    fn renko_comparison_legend_is_unavailable_even_with_stale_lines() {
        let ctx = egui::Context::default();
        let mut chart = sample_chart();
        chart.chart_type = ChartType::Renko;
        chart.compare = Some("SPY US Equity".into());
        chart.compare_loading = true;
        chart.data.comparisons.push("QQQ US Equity".into());
        let line = tr_chart::scene::line_ref(&vec![50.; chart.bars.len()]);
        chart.compare_line = Some(line.clone());
        chart.comparisons.insert(
            "QQQ US Equity".into(),
            crate::chart_compare::Series { line: Some(line), loading: true, ..Default::default() },
        );
        let labels = chart_frame(&ctx, &mut chart, vec![]);
        for ticker in ["SPY", "QQQ"] {
            assert!(labels.iter().any(|(text, _)| text == &format!("{ticker} · unavailable on Renko")));
        }
        assert!(chart.compare_base.is_none());
    }

    #[test]
    fn picker_adds_multiple_series_removes_individually_and_ignores_old_responses() {
        let ctx = egui::Context::default();
        let mut chart = sample_chart();
        for symbol in ["SPY US Equity", "QQQ US Equity", "DIA US Equity"] {
            chart.compare_picker.open();
            chart_frame(&ctx, &mut chart, vec![]);
            let labels = chart_frame(&ctx, &mut chart, vec![]);
            let at = labels.iter().find(|(s, _)| s.starts_with(&format!("{symbol}  ·"))).unwrap().1.center();
            click_chart(&ctx, &mut chart, at);
        }
        assert_eq!(chart.compare.as_deref(), Some("SPY US Equity"));
        assert_eq!(chart.data.comparisons, vec!["QQQ US Equity", "DIA US Equity"]);
        let line = tr_chart::scene::line_ref(&vec![50.; chart.bars.len()]);
        chart.on_compare(chart.generation, chart.compare_revision - 1, "QQQ US Equity", line.clone());
        assert!(chart.comparisons["QQQ US Equity"].line.is_none());
        for symbol in ["SPY US Equity", "QQQ US Equity", "DIA US Equity"] {
            chart.on_compare(chart.generation, chart.compare_revision, symbol, line.clone());
        }
        chart.compare_axis = CompareAxis::Indexed;
        chart_frame(&ctx, &mut chart, vec![]);
        assert!(chart.compare_base.is_some());
        assert_eq!(axis_value(10., 2, CompareAxis::Indexed), "110.00");
        chart.compare_picker.open();
        chart_frame(&ctx, &mut chart, vec![]);
        let labels = chart_frame(&ctx, &mut chart, vec![]);
        let at = labels.iter().find(|(s, _)| s == "Remove").unwrap().1.center();
        click_chart(&ctx, &mut chart, at);
        assert_eq!(chart.compare.as_deref(), Some("QQQ US Equity"));
        assert_eq!(chart.data.comparisons, vec!["DIA US Equity"]);
    }

    #[test]
    fn legend_actions_target_stable_instances_and_keep_the_existing_removal_policy() {
        let mut chart = sample_chart();
        chart.indicators.push(IndicatorSpec::new("EMA", &[50.]).unwrap());
        chart.indicators.push(IndicatorSpec::new("EMA", &[9.]).unwrap());
        study::assign_ids(&mut chart.indicators);
        chart.indicators[2].chart.as_mut().unwrap().source = StudySource::Output { study: 1, output: "EMA".into() };
        chart.indicators.swap(0, 1);
        let worker = Worker::inert();
        chart.apply_legend_action(crate::chart_legend::Action::Edit(1), &worker);
        assert_eq!(chart.indicator_focus, Some(1));
        assert!(chart.indicator_editor);
        chart.apply_legend_action(crate::chart_legend::Action::Remove(1), &worker);
        assert_eq!(chart.indicators.len(), 1);
        assert_eq!(chart.indicators[0].chart.as_ref().unwrap().id, 2);
    }

    #[test]
    fn context_menu_renders_sections_in_order() {
        let ctx = egui::Context::default();
        let mut chart = sample_chart();
        let worker = Worker::inert();
        let texts = ui_frame(&ctx, vec![], |ui| chart.context_menu(ui, &worker));
        let headers = ["View", "Analysis", "Alerts & account", "Tools"];
        let rendered: Vec<_> = texts.iter().filter(|(text, _)| headers.contains(&text.as_str())).collect();
        assert_eq!(rendered.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>(), headers);
        assert!(rendered.windows(2).all(|pair| pair[0].1.top() < pair[1].1.top()));
    }

    #[test]
    fn context_menu_deletes_only_the_selected_drawing_and_undo_restores_it() {
        let ctx = egui::Context::default();
        let mut chart = sample_chart();
        let drawing = Drawing::new(Tool::Horizontal, Anchor { ts: 0, price: 10. }, Anchor { ts: 0, price: 10. });
        chart.drawings = vec![drawing.clone(), drawing];
        chart.draw.selected = Some(1);
        let worker = Worker::inert();
        let texts = ui_frame(&ctx, vec![], |ui| chart.context_menu(ui, &worker));
        let at = texts.iter().find(|(text, _)| text == "Draw").unwrap().1.center();
        ui_frame(&ctx, vec![egui::Event::PointerMoved(at), pointer_button(at, true)], |ui| {
            chart.context_menu(ui, &worker)
        });
        ui_frame(&ctx, vec![pointer_button(at, false)], |ui| chart.context_menu(ui, &worker));
        let texts = ui_frame(&ctx, vec![], |ui| chart.context_menu(ui, &worker));
        let delete = texts
            .iter()
            .find(|(text, _)| text == "Delete selected drawing")
            .expect("selected deletion is exposed")
            .1
            .center();
        ui_frame(&ctx, vec![egui::Event::PointerMoved(delete), pointer_button(delete, true)], |ui| {
            chart.context_menu(ui, &worker)
        });
        ui_frame(&ctx, vec![pointer_button(delete, false)], |ui| chart.context_menu(ui, &worker));
        assert_eq!(chart.drawings.len(), 1);
        chart.undo_drawings();
        assert_eq!(chart.drawings.len(), 2);
    }

    #[test]
    fn header_controls_fit_their_measured_allocation_and_live_stays_out_of_the_axis() {
        let ctx = egui::Context::default();
        let mut chart = sample_chart();
        ui_frame(&ctx, vec![], |ui| {
            let width = ChartState::controls_width(ui);
            let used = ui
                .horizontal(|ui| {
                    chart.controls(ui, &Worker::inert());
                })
                .response
                .rect
                .width();
            assert!(used <= width, "controls use {used}, reserved {width}");
        });
        chart.last_live = Some(std::time::Instant::now());
        let texts = chart_frame(&ctx, &mut chart, vec![]);
        let live = texts.iter().find(|(text, _)| text == "LIVE").unwrap().1;
        assert!(live.right() <= 900. - AXIS_W);
        assert!(live.top() >= HEADER_H + 30.);
    }

    #[test]
    fn stale_comparison_results_and_errors_cannot_replace_a_new_selection() {
        let mut chart = sample_chart();
        let worker = Worker::inert();
        chart.set_compare(Some("MSFT".into()), &worker);
        let revision = chart.compare_revision;
        chart.set_compare(Some("NVDA".into()), &worker);
        chart.on_compare_error(chart.generation, revision, "MSFT US Equity", "old error".into());
        assert!(chart.compare_error.is_none());
        chart.on_compare(chart.generation, revision, "MSFT US Equity", tr_chart::scene::line_ref(&[1.]));
        assert!(chart.compare_line.is_none());
        chart.on_compare_error(chart.generation, chart.compare_revision, "NVDA US Equity", "offline".into());
        assert_eq!(chart.compare_error.as_deref(), Some("offline"));
        assert!(!chart.compare_loading);
        chart.set_compare(None, &worker);
        assert!(chart.compare.is_none());
        assert!(chart.compare_error.is_none());
    }

    #[test]
    fn advanced_studies_build_headless_frames_at_split_pane_width() {
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        chart.indicators = vec![
            IndicatorSpec::new("BB", &[]).unwrap(),
            IndicatorSpec::new("EMA", &[3.]).unwrap(),
            IndicatorSpec::new("PCF", &[]).unwrap(),
        ];
        study::assign_ids(&mut chart.indicators);
        chart.indicators[0].chart.as_mut().unwrap().fill =
            Some(study::BandFill { upper: "Upper".into(), lower: "Lower".into(), color: [80, 160, 180], opacity: 35 });
        chart.indicators[1].chart.as_mut().unwrap().timeframe = Some(Timeframe::W1);
        chart.indicators[2].chart.as_mut().unwrap().expression = Some("C > AVGC20".into());
        chart.bars = Arc::new(tr_core::sample::generate("AAPL", Timeframe::D1, 500, 1_790_000_000));
        chart.gpu = Some(SeriesGpu::build(&chart.bars));
        chart.built = crate::worker::build_indicators(&chart.bars, chart.tf, &chart.indicators, &theme::palette());
        assert!(chart.built.iter().all(|b| b.error.is_none()));
        let ctx = egui::Context::default();
        let worker = Worker::inert();
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(430., 600.))),
                ..Default::default()
            },
            |ui| {
                chart.show(ui, ui.max_rect(), true, &worker);
            },
        );
        assert!(
            out.shapes.iter().any(|s| matches!(&s.shape,egui::Shape::Mesh(mesh) if !mesh.vertices.is_empty())),
            "Band fill mesh is present"
        );
        out.textures_delta.clear();
    }

    #[test]
    fn timeframe_changes_preserve_the_historical_date_range() {
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        chart.bars = Arc::new(tr_core::sample::generate("AAPL", Timeframe::D1, 500, 1_790_000_000));
        chart.viewport = Some(Viewport { right: 250., bar_px: 4., width_px: 400. });
        let before = crate::chart_navigation::date_range(&chart.bars, chart.tf, &chart.viewport.unwrap()).unwrap();
        let worker = Worker::inert();
        chart.set_timeframe(Timeframe::W1, &worker);
        let bars = Arc::new(tr_core::sample::generate("AAPL", Timeframe::W1, 500, 1_790_000_000));
        chart.on_history(chart.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, false, false, &worker);
        let after = crate::chart_navigation::date_range(&chart.bars, chart.tf, &chart.viewport.unwrap()).unwrap();
        assert!((after.start - before.start).abs() <= Timeframe::W1.seconds());
        assert!((after.end - before.end).abs() <= Timeframe::W1.seconds());
        assert!(chart.pending_dates.is_none());
    }

    #[test]
    fn daily_chart_builds_lower_timeframe_ema_from_fetched_history() {
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        chart.bars = Arc::new((0..30).map(|i| Bar::new(i * 86400, 10., 10., 10., 10., 1.)).collect());
        chart.indicators = vec![IndicatorSpec::new("EMA", &[3.]).unwrap()];
        study::assign_ids(&mut chart.indicators);
        chart.indicators[0].chart.as_mut().unwrap().timeframe = Some(Timeframe::H1);
        let history = Arc::new(
            (0..720)
                .map(|i| {
                    let price = 100. + i as f64;
                    Bar::new(i * 3600, price, price, price, price, 1.)
                })
                .collect(),
        );
        let histories = study::StudyHistories::from([(Timeframe::H1, Ok(history))]);
        let worker = Worker::inert();
        chart.request_indicators(&worker);
        let built = crate::worker::build_indicators_session(
            &chart.bars,
            chart.tf,
            &chart.indicators,
            &theme::palette(),
            chart.effective_session(),
            &histories,
        );
        chart.on_indicators(chart.generation, chart.indicator_revision, built);
        assert_eq!(chart.built.len(), 1);
        assert!(chart.built[0].error.is_none());
        let extent = chart.built[0].overlays[0].1.line.pyramid.extent(0, 0, chart.bars.len()).unwrap();
        assert!(extent.0.is_finite() && extent.1.is_finite());
        assert!(extent.0 > 100., "EMA must use hourly history, not the daily chart prices");
    }

    #[test]
    fn a_newer_calculation_request_rejects_an_old_result_on_the_same_bars() {
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        chart.bars = Arc::new(tr_core::sample::generate("AAPL", Timeframe::D1, 300, 1_790_000_000));
        let old = crate::worker::build_indicators(&chart.bars, chart.tf, &chart.indicators, &theme::palette());
        let worker = Worker::inert();
        chart.request_indicators(&worker);
        let revision = chart.indicator_revision;
        chart.request_indicators(&worker);
        chart.on_indicators(chart.generation, revision, old);
        assert!(chart.built.is_empty());
    }

    #[test]
    fn drawing_body_drag_preserves_log_ratios_and_supports_undo() {
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        chart.bars = Arc::new((0..100).map(|i| Bar::new(i * 86400, 10., 10., 10., 10., 1.)).collect());
        let mut drawing = Drawing::new(
            Tool::Trendline,
            Anchor { ts: 20 * 86400, price: 10. },
            Anchor { ts: 60 * 86400, price: 100. },
        );
        drawing.symbol = Some(chart.symbol.clone());
        chart.drawings.push(drawing.clone());
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(800., 400.));
        let v = Viewport { right: 100., bar_px: 8., width_px: 800. };
        let scale = PriceScale::new(1., 1000., true, 400.);
        let start = pos2(v.x_of(40.) as f32, scale.y_of(1000_f64.sqrt()) as f32);
        let to = start + vec2(40., -30.);
        let frame = |chart: &mut ChartState, events| {
            let mut output =
                ctx.run_ui(egui::RawInput { screen_rect: Some(rect), events, ..Default::default() }, |ui| {
                    let response = ui.interact(rect, egui::Id::new("drawing-test"), Sense::click_and_drag());
                    chart.paint_drawings(ui, rect, &v, &scale, ui.input(|i| i.pointer.hover_pos()), &response);
                });
            output.textures_delta.clear();
        };
        frame(&mut chart, vec![]);
        frame(&mut chart, vec![egui::Event::PointerMoved(start)]);
        frame(
            &mut chart,
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        frame(&mut chart, vec![egui::Event::PointerMoved(to)]);
        frame(
            &mut chart,
            vec![egui::Event::PointerButton {
                pos: to,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let edited = chart.drawings[0].clone();
        assert_eq!(edited.a.ts - drawing.a.ts, 5 * 86400);
        assert!((edited.b.price / edited.a.price - 10.).abs() < 1e-9);
        assert!(edited.a.price > drawing.a.price);
        chart.undo_drawings();
        assert_eq!(chart.drawings[0], drawing);
        chart.redo_drawings();
        assert_eq!(chart.drawings[0], edited);
    }

    #[test]
    fn legacy_drawing_ownership_survives_symbol_changes_and_undo() {
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        chart.drawings.push(Drawing::new(Tool::Horizontal, Anchor { ts: 0, price: 10. }, Anchor { ts: 0, price: 10. }));
        chart.remember_drawings();
        chart.drawings[0].width = 3.;
        let owner = chart.symbol.clone();
        chart.set_symbol("MSFT", &Worker::inert());
        assert!(!chart.drawings[0].visible_on(&chart.symbol));
        chart.undo_drawings();
        assert_eq!(chart.drawings[0].symbol.as_deref(), Some(owner.as_str()));
        assert!(!chart.drawings[0].visible_on(&chart.symbol));
        assert!(chart.drawings[0].visible_on(&owner));
        chart.drawings[0].all_symbols = true;
        assert!(chart.drawings[0].visible_on(&chart.symbol));
    }

    #[test]
    fn pane_divider_drag_resizes_instead_of_panning() {
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        chart.indicators = vec![IndicatorSpec::new("RSI", &[]).unwrap()];
        study::assign_ids(&mut chart.indicators);
        chart.bars = Arc::new(tr_core::sample::generate("AAPL", Timeframe::D1, 500, 1_790_000_000));
        chart.gpu = Some(SeriesGpu::build(&chart.bars));
        chart.built = crate::worker::build_indicators(&chart.bars, chart.tf, &chart.indicators, &theme::palette());
        let worker = Worker::inert();
        let ctx = egui::Context::default();
        let frame = |chart: &mut ChartState, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900., 560.))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    chart.show(ui, ui.max_rect(), true, &worker);
                },
            );
            output.textures_delta.clear();
        };
        frame(&mut chart, vec![]);
        let right = chart.viewport.unwrap().right;
        let at = pos2(600., 560. - RANGE_H - TIME_H - 120.);
        frame(&mut chart, vec![egui::Event::PointerMoved(at)]);
        frame(
            &mut chart,
            vec![egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let to = at - vec2(0., 40.);
        frame(&mut chart, vec![egui::Event::PointerMoved(to)]);
        frame(
            &mut chart,
            vec![egui::Event::PointerButton {
                pos: to,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(chart.indicators[0].chart.as_ref().unwrap().height >= 150.);
        assert_eq!(chart.viewport.unwrap().right, right);
    }

    #[test]
    fn removing_a_source_removes_only_calculation_descendants() {
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        chart.indicators =
            ["RSI", "EMA", "BB", "MACD"].iter().map(|code| IndicatorSpec::new(code, &[]).unwrap()).collect();
        study::assign_ids(&mut chart.indicators);
        chart.indicators[1].chart.as_mut().unwrap().source = StudySource::Output { study: 1, output: "RSI".into() };
        chart.indicators[2].chart.as_mut().unwrap().source = StudySource::Output { study: 2, output: "EMA".into() };
        chart.indicators[3].chart.as_mut().unwrap().pane = StudyPane::With(1);
        chart.remove_study(0);
        assert_eq!(chart.indicators.len(), 1);
        assert_eq!(chart.indicators[0].code, "MACD");
        assert_eq!(chart.indicators[0].chart.as_ref().unwrap().pane, StudyPane::Own);
    }

    #[test]
    fn grouping_and_stale_results_respect_instance_settings() {
        let mut chart = ChartState::new(1, "AAPL", Timeframe::D1);
        chart.indicators =
            ["RSI", "EMA", "BB", "BB"].iter().map(|code| IndicatorSpec::new(code, &[]).unwrap()).collect();
        study::assign_ids(&mut chart.indicators);
        chart.indicators[1].chart.as_mut().unwrap().pane = StudyPane::With(1);
        chart.indicators[2].chart.as_mut().unwrap().visible = false;
        let bars = tr_core::sample::generate("AAPL", Timeframe::D1, 100, 1_790_000_000);
        let built = crate::worker::build_indicators(&bars, Timeframe::D1, &chart.indicators, &theme::palette());
        assert_eq!(study_groups(&built), vec![(Some(1), vec![0, 1]), (None, vec![3])]);
        chart.indicators[0].params[0] = 2.;
        chart.on_indicators(chart.generation, chart.indicator_revision, built);
        assert!(chart.built.is_empty(), "a superseded parameter response cannot replace current studies");
    }

    #[test]
    fn glide_pans_linearly_and_zooms_evenly() {
        let from = Viewport { right: 100.0, bar_px: 2.0, width_px: 800.0 };
        let to = Viewport { right: 200.0, bar_px: 8.0, width_px: 800.0 };
        let g = Glide { from, to, start: None };
        assert_eq!(g.at(0.0), from);
        let end = g.at(1.0);
        assert_eq!(end.right, to.right);
        assert!((end.bar_px - to.bar_px).abs() < 1e-9);
        let mid = g.at(0.5);
        assert_eq!(mid.right, 150.0);
        assert!((mid.bar_px - 4.0).abs() < 1e-9, "geometric midpoint");
    }

    #[test]
    fn view_changes_glide_from_what_is_shown() {
        let mut c = ChartState::new(1, "AAPL", Timeframe::D1);
        c.bars = Arc::new(tr_core::sample::generate("AAPL", Timeframe::D1, 500, 1_790_000_000));
        let shown = Viewport::fit_last(500, 100, 800.0);
        c.viewport = Some(shown);
        c.pan_bars(-20.0);
        assert_eq!(c.viewport, Some(shown), "stays where it is shown until the glide runs");
        assert_eq!(c.glide.map(|g| g.to.right), Some(shown.right - 20.0));
        // A second press adds to the destination and keeps gliding from the shown view.
        c.pan_bars(-20.0);
        let g = c.glide.expect("gliding");
        assert_eq!((g.from, g.to.right), (shown, shown.right - 40.0));
        c.reset_view();
        assert_eq!(c.glide.map(|g| g.to), Some(Viewport::fit_last(500, DEFAULT_VISIBLE, 800.0)));
    }
}
