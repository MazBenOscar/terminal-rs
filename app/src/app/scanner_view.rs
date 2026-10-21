//! Watchlist and market-wide snapshots: editable conditions, named definitions and explicit
//! data status.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use egui::{Align2, Color32, RichText, Sense, Ui, pos2};
use tr_core::Timeframe;
use tr_core::scanner::{
    Comparison, ConditionGroup, ConditionItem, FormulaCondition, MAX_BARS_AGO, MAX_CONDITIONS, MAX_GROUP_DEPTH,
    MatchMode, ScanCondition, ScanDefinition, ScanOperand, ScanOutcome, Source,
};

use crate::congress_view::Action;
use crate::theme::{self, amber, dp, mono, muted, text, ui_font};
use crate::widgets::{self, Icon};
use tr_data::universe::Universe;

use crate::layouts::ScanSettings;
use crate::worker::{PaneId, Request, ScanRow, data_dir};

pub struct ScannerPane {
    pub definition: ScanDefinition,
    pub generation: u64,
    pub running: bool,
    pub rows: Vec<ScanRow>,
    pub completed: Option<chrono::DateTime<chrono::Utc>>,
    snapshot_symbols: Vec<String>,
    saved: Vec<ScanSettings>,
    notice: Option<String>,
    matches_only: bool,
    pub universe: Universe,
    /// Universe scan progress: symbols evaluated and total.
    pub progress: Option<(usize, usize)>,
    /// Stops the running universe scan.
    pub cancel: Option<Arc<AtomicBool>>,
    show_unavailable: bool,
    pub refresh: Option<u32>,
    previous_matches: HashSet<String>,
    previous_evaluated: HashSet<String>,
    baseline_ready: bool,
    entered: HashSet<String>,
    exited: Vec<String>,
    started: Option<chrono::DateTime<chrono::Utc>>,
    run_duration: chrono::Duration,
    universe_size: Option<usize>,
}

impl Default for ScannerPane {
    fn default() -> Self {
        Self::new(ScanDefinition::default())
    }
}

impl ScannerPane {
    pub fn new(definition: ScanDefinition) -> Self {
        Self::from_settings(definition.into())
    }

    pub fn from_settings(settings: ScanSettings) -> Self {
        let (saved, notice) = match load_saved() {
            Ok(saved) => (saved, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        Self {
            definition: settings.definition,
            generation: 0,
            running: false,
            rows: Vec::new(),
            completed: None,
            snapshot_symbols: Vec::new(),
            saved,
            notice,
            matches_only: true,
            universe: settings.universe,
            progress: None,
            cancel: None,
            show_unavailable: false,
            refresh: settings.refresh,
            previous_matches: HashSet::new(),
            previous_evaluated: HashSet::new(),
            baseline_ready: false,
            entered: HashSet::new(),
            exited: Vec::new(),
            started: None,
            run_duration: chrono::Duration::zero(),
            universe_size: None,
        }
    }

    fn invalidate(&mut self) {
        self.rows.clear();
        self.completed = None;
        self.previous_matches.clear();
        self.previous_evaluated.clear();
        self.baseline_ready = false;
        self.entered.clear();
        self.exited.clear();
        self.universe_size = None;
    }

    fn stop(&mut self) {
        self.refresh = None;
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn partial(&mut self, generation: u64, rows: Vec<ScanRow>) {
        if self.running && self.generation == generation {
            self.rows.extend(rows);
        }
    }

    pub fn finish(&mut self, generation: u64, rows: Vec<ScanRow>) {
        if self.generation != generation {
            return;
        }
        let completed = chrono::Utc::now();
        self.run_duration = self.started.map_or(chrono::Duration::zero(), |start| completed - start);
        let stopped = self.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed));
        self.entered.clear();
        self.exited.clear();
        if !stopped {
            let (matches, evaluated) = match_sets(&rows);
            if self.baseline_ready {
                (self.entered, self.exited) =
                    match_diff(&self.previous_matches, &self.previous_evaluated, &matches, &evaluated);
            }
            self.previous_matches = matches;
            self.previous_evaluated = evaluated;
            self.baseline_ready = true;
        }
        self.rows = rows;
        self.running = false;
        self.progress = None;
        self.cancel = None;
        self.completed = Some(completed);
    }

    pub fn set_progress(&mut self, done: usize, total: usize) {
        self.progress = Some((done, total));
        self.universe_size = Some(total);
        self.restrict_refresh();
    }

    fn restrict_refresh(&mut self) {
        // Until the directory arrives, conservatively treat a market universe as large.
        let size = if self.universe == Universe::Watchlist { 0 } else { self.universe_size.unwrap_or(1001) };
        if self.refresh.is_some_and(|minutes| !interval_allowed(size, minutes)) {
            self.refresh = Some(15);
        }
    }
}

fn due_at(
    completed: chrono::DateTime<chrono::Utc>,
    interval: u32,
    run_duration: chrono::Duration,
) -> chrono::DateTime<chrono::Utc> {
    let interval = chrono::Duration::minutes(i64::from(interval));
    completed + if run_duration > interval { interval.max(chrono::Duration::seconds(30)) } else { interval }
}

fn interval_allowed(symbols: usize, minutes: u32) -> bool {
    matches!(minutes, 1 | 5 | 15 | 60) && (symbols <= 1000 || minutes >= 15)
}

fn match_sets(rows: &[ScanRow]) -> (HashSet<String>, HashSet<String>) {
    let mut matches = HashSet::new();
    let mut evaluated = HashSet::new();
    for row in rows {
        if row.error.is_none()
            && let Some(evaluation) = &row.evaluation
        {
            if evaluation.outcome != ScanOutcome::Unavailable {
                evaluated.insert(row.symbol.clone());
            }
            if evaluation.outcome == ScanOutcome::Match {
                matches.insert(row.symbol.clone());
            }
        }
    }
    (matches, evaluated)
}

fn match_diff(
    previous: &HashSet<String>,
    previous_evaluated: &HashSet<String>,
    current: &HashSet<String>,
    evaluated: &HashSet<String>,
) -> (HashSet<String>, Vec<String>) {
    let entered = current.difference(previous).filter(|s| previous_evaluated.contains(*s)).cloned().collect();
    let mut exited: Vec<_> = previous.difference(current).filter(|s| evaluated.contains(*s)).cloned().collect();
    exited.sort();
    (entered, exited)
}

impl Drop for ScannerPane {
    /// A closed pane stops its universe scan instead of letting it run on unseen.
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

fn load_saved() -> Result<Vec<ScanSettings>, String> {
    load_saved_at(&data_dir())
}

fn load_saved_at(dir: &std::path::Path) -> Result<Vec<ScanSettings>, String> {
    match std::fs::read(dir.join("scans.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("Could not read saved scans: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("Could not open saved scans: {e}")),
    }
}

/// Merge against disk so saving from two scanner panes does not discard the other scan.
fn save_definition(definition: &ScanSettings) -> Result<Vec<ScanSettings>, String> {
    save_definition_at(definition, &data_dir())
}

fn save_definition_at(definition: &ScanSettings, dir: &std::path::Path) -> Result<Vec<ScanSettings>, String> {
    definition.definition.validate()?;
    let mut saved = load_saved_at(dir)?;
    saved.retain(|s| !s.definition.name.eq_ignore_ascii_case(definition.definition.name.trim()));
    let mut definition = definition.clone();
    definition.definition.name = definition.definition.name.trim().into();
    saved.push(definition);
    saved.sort_by(|a, b| a.definition.name.cmp(&b.definition.name));
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create scan folder: {e}"))?;
    let bytes = serde_json::to_vec_pretty(&saved).map_err(|e| format!("Could not encode scans: {e}"))?;
    let tmp = dir.join("scans.json.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("Could not save scans: {e}"))?;
    std::fs::rename(tmp, dir.join("scans.json")).map_err(|e| format!("Could not replace scans: {e}"))?;
    Ok(saved)
}

const OPERAND_KINDS: [&str; 8] = ["Price", "Volume", "Bar change %", "SMA", "EMA", "RSI", "ATR", "Value"];

fn operand_kind(operand: &ScanOperand) -> usize {
    match operand {
        ScanOperand::Price => 0,
        ScanOperand::Volume => 1,
        ScanOperand::ChangePercent => 2,
        ScanOperand::Sma { .. } => 3,
        ScanOperand::Ema { .. } => 4,
        ScanOperand::Rsi { .. } => 5,
        ScanOperand::Atr { .. } => 6,
        ScanOperand::Constant(_) => 7,
    }
}

/// ComboBox uses a nested horizontal UI, so reserve its minimum width before
/// entering it: egui cannot move an already-rendered child onto the next line.
fn wrap_control(ui: &mut Ui, width: f32) {
    if ui.layout().main_wrap() && ui.available_size_before_wrap().x < width {
        ui.end_row();
    }
}

fn operand_editor(ui: &mut Ui, operand: &mut ScanOperand, bars_ago: &mut usize, id: &str) {
    let mut kind = operand_kind(operand);
    let before = kind;
    wrap_control(ui, 95.0);
    egui::ComboBox::from_id_salt(id).width(95.0).selected_text(OPERAND_KINDS[kind]).show_ui(ui, |ui| {
        for (i, name) in OPERAND_KINDS.iter().enumerate() {
            ui.selectable_value(&mut kind, i, *name);
        }
    });
    if kind != before {
        *operand = match kind {
            0 => ScanOperand::Price,
            1 => ScanOperand::Volume,
            2 => ScanOperand::ChangePercent,
            3 => ScanOperand::Sma { period: 20, source: Source::Close },
            4 => ScanOperand::Ema { period: 20, source: Source::Close },
            5 => ScanOperand::Rsi { period: 14 },
            6 => ScanOperand::Atr { period: 14 },
            _ => ScanOperand::Constant(0.0),
        };
    }
    match operand {
        ScanOperand::Sma { period, source } | ScanOperand::Ema { period, source } => {
            wrap_control(ui, 70.0);
            egui::ComboBox::from_id_salt((id, "source")).width(70.0).selected_text(source.label()).show_ui(ui, |ui| {
                for s in Source::ALL {
                    ui.selectable_value(source, s, s.label());
                }
            });
            ui.add(egui::DragValue::new(period).range(1..=500).prefix("Period "));
        }
        ScanOperand::Rsi { period } | ScanOperand::Atr { period } => {
            ui.add(egui::DragValue::new(period).range(1..=500).prefix("Period "));
        }
        ScanOperand::Constant(value) => {
            ui.add(egui::DragValue::new(value).speed(0.1).max_decimals(4));
        }
        _ => {}
    }
    // A constant is the same on every bar.
    if matches!(operand, ScanOperand::Constant(_)) {
        *bars_ago = 0;
    } else {
        ui.add(egui::DragValue::new(bars_ago).range(0..=MAX_BARS_AGO).suffix(" bars ago"))
            .on_hover_text("0 = latest bar; 1 = the bar before it (yesterday on 1D)");
    }
}

/// A muted label and its control, kept together when the row wraps.
fn field(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(muted()));
        add(ui);
    });
    ui.add_space(dp(8.0));
}

fn timeframe_editor(ui: &mut Ui, timeframe: &mut Option<Timeframe>) {
    let selected = match timeframe {
        Some(tf) => RichText::new(tf.label()),
        None => RichText::new("Scan timeframe").color(muted()),
    };
    wrap_control(ui, dp(110.0));
    egui::ComboBox::from_id_salt("condition-timeframe").width(dp(110.0)).selected_text(selected).show_ui(ui, |ui| {
        ui.selectable_value(timeframe, None, RichText::new("Scan timeframe").color(muted()));
        for tf in Timeframe::ALL {
            ui.selectable_value(timeframe, Some(tf), tf.label());
        }
    });
}

fn mode_editor(ui: &mut Ui, mode: &mut MatchMode) {
    wrap_control(ui, dp(90.0));
    egui::ComboBox::from_id_salt("scan-mode")
        .width(dp(90.0))
        .selected_text(if *mode == MatchMode::All { "Match all" } else { "Match any" })
        .show_ui(ui, |ui| {
            ui.selectable_value(mode, MatchMode::All, "Match all");
            ui.selectable_value(mode, MatchMode::Any, "Match any");
        });
}

fn compare_editor(ui: &mut Ui, condition: &mut ScanCondition, index: usize) {
    ui.label(RichText::new(format!("{}", index + 1)).font(mono(12.0)).color(muted()));
    operand_editor(ui, &mut condition.left, &mut condition.left_bars_ago, "left");
    wrap_control(ui, dp(110.0));
    egui::ComboBox::from_id_salt("comparison").width(dp(110.0)).selected_text(condition.comparison.label()).show_ui(
        ui,
        |ui| {
            for comparison in Comparison::ALL {
                ui.selectable_value(&mut condition.comparison, comparison, comparison.label());
            }
        },
    );
    operand_editor(ui, &mut condition.right, &mut condition.right_bars_ago, "right");
    // Scaling a constant is just a different constant.
    if matches!(condition.right, ScanOperand::Constant(_)) {
        condition.right_multiplier = 1.0;
    } else {
        ui.add(egui::DragValue::new(&mut condition.right_multiplier).speed(0.05).max_decimals(3).prefix("× "))
            .on_hover_text("Multiplies the right side, e.g. Volume >= EMA(50, Volume) × 2.5");
    }
    timeframe_editor(ui, &mut condition.timeframe);
}

/// Keep the remove column outside every line of wrapped condition controls.
fn condition_row(ui: &mut Ui, add: impl FnOnce(&mut Ui)) -> (egui::Rect, egui::Response) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
        // Allocate from the right before any controls can expand their child UI.
        let close = ui.allocate_ui_with_layout(
            egui::vec2(dp(22.0), dp(20.0)),
            egui::Layout::top_down(egui::Align::RIGHT),
            |ui| widgets::icon_button(ui, Icon::Close, "Remove condition", false),
        );
        let remaining = ui.available_rect_before_wrap().width().max(0.0);
        let content =
            ui.allocate_ui_with_layout(egui::vec2(remaining, 0.0), egui::Layout::top_down(egui::Align::LEFT), |ui| {
                ui.set_max_width(remaining);
                ui.horizontal_wrapped(|ui| {
                    ui.set_max_width(remaining);
                    add(ui);
                });
            });
        (content.response.rect, close.inner)
    })
    .inner
}

fn run_enabled(definition: &ScanDefinition, running: bool, data_ready: bool) -> bool {
    !running && data_ready && definition.validate().is_ok()
}

fn can_add_leaf(leaves: usize) -> bool {
    leaves < MAX_CONDITIONS
}

fn can_add_group(depth: usize) -> bool {
    depth < MAX_GROUP_DEPTH
}

/// Remove a child by index path, pruning any ancestors left empty except the root.
fn remove_at(group: &mut ConditionGroup, path: &[usize]) {
    let Some((&index, rest)) = path.split_first() else { return };
    if index >= group.items.len() {
        return;
    }
    if rest.is_empty() {
        group.items.remove(index);
    } else if let ConditionItem::Group(child) = &mut group.items[index] {
        remove_at(child, rest);
        if child.items.is_empty() {
            group.items.remove(index);
        }
    }
}

/// Shared condition editor; inherited timeframes use the caller's timeframe.
pub(crate) fn condition_group_editor(
    ui: &mut Ui,
    group: &mut ConditionGroup,
    scan_tf: Timeframe,
    id_salt: impl egui::AsIdSalt,
) {
    ui.push_id(id_salt, |ui| {
        ui.horizontal(|ui| {
            mode_editor(ui, &mut group.mode);
            ui.label(RichText::new(format!("Default timeframe: {}", scan_tf.label())).small().color(muted()));
        });
        let mut leaves = ScanDefinition { name: String::new(), timeframe: scan_tf, root: group.clone() }.leaves().len();
        let mut remove = None;
        group_editor(ui, group, 1, &mut leaves, &mut Vec::new(), &mut remove);
        if let Some(path) = remove {
            remove_at(group, &path);
        }
    });
}

fn group_editor(
    ui: &mut Ui,
    group: &mut ConditionGroup,
    depth: usize,
    leaves: &mut usize,
    path: &mut Vec<usize>,
    remove: &mut Option<Vec<usize>>,
) {
    for (index, item) in group.items.iter_mut().enumerate() {
        path.push(index);
        ui.push_id(index, |ui| {
            let frame = egui::Frame::new()
                .fill(theme::header())
                .corner_radius(theme::CONTROL_RADIUS)
                .inner_margin(egui::Margin::symmetric(6, 4));
            let response = frame.show(ui, |ui| {
                ui.set_width(ui.available_width());
                let (_, close) = condition_row(ui, |ui| match item {
                    ConditionItem::Compare(condition) => compare_editor(ui, condition, index),
                    ConditionItem::Formula(formula) => {
                        ui.add(
                            egui::TextEdit::singleline(&mut formula.expression)
                                .desired_width(dp(280.0))
                                .hint_text("PCF formula, e.g. C > AVGC20"),
                        );
                        timeframe_editor(ui, &mut formula.timeframe);
                    }
                    ConditionItem::Group(child) => {
                        ui.label(RichText::new("Group").color(muted()));
                        mode_editor(ui, &mut child.mode);
                    }
                });
                if close.clicked() {
                    *remove = Some(path.clone());
                }
                match item {
                    ConditionItem::Formula(formula) => {
                        if let Err(error) = formula.validate() {
                            ui.label(RichText::new(error).small().color(theme::down()));
                        }
                    }
                    ConditionItem::Group(child) => {
                        ui.scope(|ui| {
                            ui.spacing_mut().indent = dp(12.0);
                            ui.indent("group-children", |ui| {
                                group_editor(ui, child, depth + 1, leaves, path, remove);
                            });
                        });
                    }
                    ConditionItem::Compare(_) => {}
                }
            });
            if matches!(item, ConditionItem::Group(_)) {
                let rect = response.response.rect;
                ui.painter()
                    .line_segment([rect.left_top(), rect.left_bottom()], egui::Stroke::new(dp(1.0), theme::border()));
            }
            ui.add_space(dp(2.0));
        });
        path.pop();
    }
    if depth == 1 && group.items.is_empty() {
        ui.label(RichText::new("No conditions yet. Add one to describe what a match looks like.").color(muted()));
    }
    ui.horizontal_wrapped(|ui| {
        for (label, kind) in [("Add condition", 0), ("Add formula", 1), ("Add group", 2)] {
            if kind == 2 && !can_add_group(depth) {
                continue;
            }
            if ui
                .add_enabled(can_add_leaf(*leaves), egui::Button::new(label))
                .on_disabled_hover_text("A scan can have up to 32 conditions")
                .clicked()
            {
                let item = match kind {
                    0 => ConditionItem::Compare(ScanCondition::default()),
                    1 => ConditionItem::Formula(FormulaCondition { expression: String::new(), timeframe: None }),
                    _ => ConditionItem::Group(ConditionGroup::comparisons(
                        MatchMode::All,
                        vec![ScanCondition::default()],
                    )),
                };
                group.items.push(item);
                *leaves += 1;
            }
        }
    });
}

fn outcome_label(outcome: ScanOutcome) -> (&'static str, Color32) {
    match outcome {
        ScanOutcome::Match => ("Match", theme::up()),
        ScanOutcome::NoMatch => ("No match", muted()),
        ScanOutcome::Unavailable => ("Unavailable", theme::down()),
    }
}

pub fn show(
    ui: &mut Ui,
    pane: &mut ScannerPane,
    id: PaneId,
    symbols: &[String],
    bulk_ready: bool,
    mut send: impl FnMut(Request),
) -> Vec<Action> {
    let mut actions = Vec::new();
    let market = pane.universe != Universe::Watchlist;
    pane.restrict_refresh();
    egui::ScrollArea::vertical().id_salt(("scanner", id)).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            let mode = if pane.refresh.is_some() { "auto-refresh" } else { "manual snapshot" };
            let (title, subtitle) = if market {
                ("MARKET SCANNER", format!("{} · {mode}", pane.universe.label()))
            } else {
                ("WATCHLIST SCANNER", format!("{} symbols · {mode}", symbols.len()))
            };
            ui.label(RichText::new(title).font(mono(12.0)).color(amber()));
            ui.label(RichText::new(subtitle).font(mono(12.0)).color(muted()));
        });
        ui.label(
            RichText::new("Uses the latest provider bar, which may still be forming.")
                .small()
                .color(muted()),
        );
        ui.add_space(dp(6.0));
        let before = pane.definition.clone();
        ui.add_enabled_ui(!pane.running, |ui| {
            ui.horizontal_wrapped(|ui| {
                field(ui, "Name", |ui| {
                    ui.add(egui::TextEdit::singleline(&mut pane.definition.name).desired_width(dp(160.0)).char_limit(80));
                });
                field(ui, "Timeframe", |ui| {
                    egui::ComboBox::from_id_salt("scan-timeframe")
                        .width(dp(64.0))
                        .selected_text(pane.definition.timeframe.label())
                        .show_ui(ui, |ui| {
                            for tf in Timeframe::ALL {
                                ui.selectable_value(&mut pane.definition.timeframe, tf, tf.label());
                            }
                        });
                });
                field(ui, "Universe", |ui| {
                    let before = pane.universe;
                    egui::ComboBox::from_id_salt("scan-universe")
                        .width(dp(120.0))
                        .selected_text(pane.universe.label())
                        .show_ui(ui, |ui| {
                            for universe in Universe::ALL {
                                let enabled = universe == Universe::Watchlist || bulk_ready;
                                ui.add_enabled_ui(enabled, |ui| {
                                    ui.selectable_value(&mut pane.universe, universe, universe.label())
                                        .on_disabled_hover_text(
                                            "Market-wide scans need a bulk data source: set up Questrade in Settings › Brokerage",
                                        );
                                });
                            }
                        });
                    if pane.universe != before {
                        pane.invalidate();
                    }
                });
            });
            ui.add_space(dp(6.0));
            ui.horizontal(|ui| {
                ui.label(RichText::new("Conditions").color(muted()));
                egui::ComboBox::from_id_salt("scan-mode")
                    .width(dp(90.0))
                    .selected_text(if pane.definition.root.mode == MatchMode::All { "Match all" } else { "Match any" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut pane.definition.root.mode, MatchMode::All, "Match all");
                        ui.selectable_value(&mut pane.definition.root.mode, MatchMode::Any, "Match any");
                    });
            });
            let mut remove = None;
            let mut leaves = pane.definition.leaves().len();
            group_editor(ui, &mut pane.definition.root, 1, &mut leaves, &mut Vec::new(), &mut remove);
            if let Some(path) = remove {
                remove_at(&mut pane.definition.root, &path);
            }
        });
        if pane.definition != before {
            pane.invalidate();
        }
        let validation = pane.definition.validate().err();
        if let Some(error) = &validation {
            ui.label(RichText::new(error).small().color(muted()));
        }
        ui.add_space(dp(4.0));
        ui.separator();
        let market = pane.universe != Universe::Watchlist;
        let can_run = run_enabled(
            &pane.definition,
            pane.running,
            if market { bulk_ready } else { !symbols.is_empty() && symbols.len() <= 200 },
        );
        ui.horizontal_wrapped(|ui| {
            let run = egui::Button::new(RichText::new("Run scan").color(Color32::WHITE))
                .fill(theme::accent())
                .min_size(egui::vec2(dp(84.0), 0.0));
            let mut run_response = ui.add_enabled(can_run, run);
            if let Some(error) = &validation {
                run_response = run_response.on_disabled_hover_text(error);
            }
            field(ui, "Refresh", |ui| {
                let label = pane.refresh.map_or_else(|| "Off".into(), |m| format!("{m} min"));
                egui::ComboBox::from_id_salt("scan-refresh").selected_text(label).show_ui(ui, |ui| {
                    ui.selectable_value(&mut pane.refresh, None, "Off");
                    let size = if market { pane.universe_size.unwrap_or(1001) } else { symbols.len() };
                    for minutes in [1, 5, 15, 60] {
                        ui.add_enabled_ui(interval_allowed(size, minutes), |ui| {
                            ui.selectable_value(&mut pane.refresh, Some(minutes), format!("{minutes} min"))
                                .on_disabled_hover_text("Large universes refresh at most every 15 minutes");
                        });
                    }
                });
            });
            pane.restrict_refresh();
            let now = chrono::Utc::now();
            let due = pane.refresh.map(|minutes| {
                pane.completed.map_or(now, |completed| due_at(completed, minutes, pane.run_duration))
            });
            if can_run && !pane.running && let Some(due) = due {
                ui.ctx().request_repaint_after((due - now).to_std().unwrap_or_default());
            }
            if run_response.clicked() || (can_run && due.is_some_and(|due| now >= due)) {
                // Never reuse a generation after restoring a layout or recreating a pane.
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                pane.generation = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if !market && pane.snapshot_symbols != symbols {
                    pane.invalidate();
                }
                pane.rows.clear();
                pane.entered.clear();
                pane.exited.clear();
                pane.started = Some(now);
                pane.running = true;
                pane.notice = None;
                let cancel = Arc::new(AtomicBool::new(false));
                pane.cancel = Some(cancel.clone());
                if market {
                    pane.snapshot_symbols.clear();
                    pane.progress = Some((0, 0));
                    send(Request::ScanUniverse {
                        pane: id,
                        generation: pane.generation,
                        definition: pane.definition.clone(),
                        universe: pane.universe,
                        cancel,
                    });
                } else {
                    pane.snapshot_symbols = symbols.to_vec();
                    send(Request::Scan {
                        pane: id,
                        generation: pane.generation,
                        definition: pane.definition.clone(),
                        symbols: symbols.to_vec(),
                        cancel,
                    });
                }
            }
            if ui.add_enabled(!pane.running && validation.is_none(), egui::Button::new("Save scan")).clicked() {
                match save_definition(&ScanSettings { definition: pane.definition.clone(), universe: pane.universe, refresh: pane.refresh }) {
                    Ok(saved) => {
                        pane.saved = saved;
                        pane.notice = Some(SAVED_NOTICE.into());
                    }
                    Err(e) => pane.notice = Some(e),
                }
            }
            let mut selected = None;
            ui.add_enabled_ui(!pane.running, |ui| {
                egui::ComboBox::from_id_salt("saved-scans").selected_text("Load scan").show_ui(ui, |ui| {
                    ui.label(RichText::new("Presets").small().color(muted()));
                    for preset in ScanDefinition::presets() {
                        if ui.selectable_label(false, &preset.name).clicked() {
                            selected = Some(ScanSettings { definition: preset, universe: pane.universe, refresh: pane.refresh });
                        }
                    }
                    ui.separator();
                    ui.label(RichText::new("Saved").small().color(muted()));
                    if pane.saved.is_empty() {
                        ui.label(RichText::new("No saved scans yet").color(muted()));
                    }
                    for saved in &pane.saved {
                        if ui.selectable_label(false, &saved.definition.name).clicked() {
                            selected = Some(saved.clone());
                        }
                    }
                });
            });
            if let Some(settings) = selected {
                pane.definition = settings.definition;
                pane.universe = settings.universe;
                pane.refresh = settings.refresh;
                pane.invalidate();
            }
            if pane.running && !market {
                ui.spinner();
                ui.label(
                    RichText::new(format!(
                        "Scanning {} symbols… up to 15 seconds per symbol, four at a time",
                        pane.snapshot_symbols.len()
                    ))
                    .color(muted()),
                );
            }
        });
        if pane.running && market {
            let (done, total) = pane.progress.unwrap_or((0, 0));
            ui.horizontal(|ui| {
                let fraction = if total == 0 { 0.0 } else { done as f32 / total as f32 };
                let text = if total == 0 { "Loading the US symbol directory…".to_string() } else { format!("{done} / {total}") };
                ui.add(egui::ProgressBar::new(fraction).text(text).desired_width(dp(260.0)));
                let stopping = pane.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed));
                if ui.add_enabled(!stopping, egui::Button::new(if stopping { "Stopping…" } else { "Stop" })).clicked() {
                    pane.stop();
                }
            });
            ui.label(RichText::new("Fresh snapshots are reused. Stale data, changed sources and larger lookbacks download full history; a market-wide refresh can take 10–15 minutes.").small().color(muted()));
        }
        if pane.running && !market && ui.button("Stop").clicked() {
            pane.stop();
        }
        let status = if let Some(minutes) = pane.refresh {
            let now = chrono::Utc::now();
            let remaining = pane.completed.map_or(0, |completed| {
                (due_at(completed, minutes, pane.run_duration) - now).num_seconds().max(0)
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
            format!("Auto-refresh every {minutes} min · next in {}:{:02}", remaining / 60, remaining % 60)
        } else {
            "Auto-refresh off".into()
        };
        ui.label(RichText::new(status).small().color(muted()));
        let warn = |ui: &mut Ui, message: &str| {
            ui.label(RichText::new(message).color(amber()));
        };
        if market && !bulk_ready {
            warn(ui, "Market-wide scans need a bulk data source: set up Questrade in Settings › Brokerage.");
        }
        if !market && symbols.is_empty() {
            warn(ui, "Your watchlist is empty. Add symbols with WL ADD <KEY>, then run the scan.");
        }
        if !market && symbols.len() > 200 {
            warn(ui, "This version supports up to 200 watchlist symbols. Reduce the list before running.");
        }
        if let Some(notice) = &pane.notice {
            let color = if notice == SAVED_NOTICE { theme::up() } else { theme::down() };
            ui.label(RichText::new(notice).color(color));
        }
        if let Some(completed) = pane.completed.filter(|_| !pane.running) {
            let count = |outcome| {
                pane.rows.iter().filter(|r| r.evaluation.as_ref().is_some_and(|e| e.outcome == outcome)).count()
            };
            let unavailable = pane
                .rows
                .iter()
                .filter(|r| r.error.is_some() || r.evaluation.as_ref().is_none_or(|e| e.outcome == ScanOutcome::Unavailable))
                .count();
            ui.add_space(dp(6.0));
            ui.horizontal_wrapped(|ui| {
                let matches = count(ScanOutcome::Match);
                ui.label(RichText::new(format!("{matches} matches")).color(if matches > 0 { theme::up() } else { text() }));
                ui.label(RichText::new(format!("· {} non-matches", count(ScanOutcome::NoMatch))).color(muted()));
                ui.label(
                    RichText::new(format!("· {unavailable} unavailable"))
                        .color(if unavailable > 0 { theme::down() } else { muted() }),
                );
                ui.add_space(dp(12.0));
                if market {
                    ui.checkbox(&mut pane.matches_only, "Matches only");
                    ui.checkbox(&mut pane.show_unavailable, "Show unavailable");
                } else {
                    ui.checkbox(&mut pane.matches_only, "Matches only (always show errors)");
                }
            });
            let scanned = if market { pane.rows.len() } else { pane.snapshot_symbols.len() };
            ui.label(
                RichText::new(format!(
                    "Scanned {scanned} symbols · finished {} UTC · click a row to chart it",
                    completed.format("%Y-%m-%d %H:%M:%S")
                ))
                .small()
                .color(muted()),
            );
            if !market && pane.snapshot_symbols != symbols {
                warn(ui, "The watchlist has changed since this snapshot. Run again to scan the current list.");
            }
            if pane.rows.iter().any(|r| r.simulated) {
                warn(ui, "SIMULATED DATA — these results use generated sample bars.");
            }
            ui.add_space(dp(4.0));
            if pane.baseline_ready && !pane.running {
                egui::CollapsingHeader::new(
                    RichText::new(format!("{} new · {} dropped", pane.entered.len(), pane.exited.len())).color(muted()),
                ).show(ui, |ui| {
                    for symbol in &pane.exited {
                        if ui.selectable_label(false, RichText::new(symbol).color(muted())).clicked() {
                            actions.push(Action::OpenChart(symbol.clone()));
                        }
                    }
                });
            }
            results_table(ui, pane, market, &mut actions);
        } else if pane.running && !pane.rows.is_empty() {
            results_table(ui, pane, market, &mut actions);
        } else if !pane.running {
            ui.add_space(dp(6.0));
            ui.label(RichText::new("Choose conditions and run a scan. Definitions are saved with the workspace; Save scan also adds them to your library.").color(muted()));
        }
    });
    actions
}

/// Two decimals, with K/M/B for volumes and other large values.
fn compact(v: f64) -> String {
    let a = v.abs();
    if a >= 1e9 {
        format!("{:.2}B", v / 1e9)
    } else if a >= 1e6 {
        format!("{:.2}M", v / 1e6)
    } else if a >= 1e5 {
        format!("{:.1}K", v / 1e3)
    } else {
        format!("{v:.2}")
    }
}

const SAVED_NOTICE: &str = "Scan saved to scans.json";

/// Results drawn like the watchlist: a header band, hover-washed rows, click to chart.
fn results_table(ui: &mut Ui, pane: &ScannerPane, market: bool, actions: &mut Vec<Action>) {
    let row_h = dp(22.0);
    let w = ui.available_width();
    // Left edges: security, result, last bar, bar age, details.
    let x = [8.0, dp(100.0), dp(196.0), dp(320.0), dp(392.0)];
    let (hdr, _) = ui.allocate_exact_size(egui::vec2(w, row_h), Sense::hover());
    let p = ui.painter_at(hdr);
    p.rect_filled(hdr, theme::CONTROL_RADIUS, theme::header());
    for (k, title) in ["Security", "Result", "Last bar (UTC)", "Bar age", "Details"].into_iter().enumerate() {
        p.text(pos2(hdr.left() + x[k], hdr.center().y), Align2::LEFT_CENTER, title, ui_font(11.5), muted());
    }
    let mut shown = 0;
    for row in &pane.rows {
        let outcome = row.evaluation.as_ref().map(|e| e.outcome).unwrap_or(ScanOutcome::Unavailable);
        if pane.matches_only && outcome == ScanOutcome::NoMatch {
            continue;
        }
        if market && !pane.show_unavailable && outcome == ScanOutcome::Unavailable {
            continue;
        }
        shown += 1;
        let (r, resp) = ui.allocate_exact_size(egui::vec2(w, row_h), Sense::click());
        widgets::hover_wash(ui, r, &resp, false);
        let p = ui.painter_at(r);
        let y = r.center().y;
        let ticker = tr_data::security(&row.symbol).ticker;
        p.text(pos2(r.left() + x[0], y), Align2::LEFT_CENTER, &ticker, mono(12.5), amber());
        let (label, color) = outcome_label(outcome);
        p.text(pos2(r.left() + x[1], y), Align2::LEFT_CENTER, label, ui_font(12.0), color);
        if pane.entered.contains(&row.symbol) {
            p.text(pos2(r.left() + x[1] + dp(60.0), y), Align2::LEFT_CENTER, "New", ui_font(10.0), theme::up());
        }
        let ts = row.ts.and_then(|ts| chrono::DateTime::from_timestamp(ts, 0));
        let last = ts.map_or_else(|| "—".into(), |ts| ts.format("%Y-%m-%d %H:%M").to_string());
        p.text(pos2(r.left() + x[2], y), Align2::LEFT_CENTER, last, mono(12.0), text());
        let age = row.ts.map_or_else(
            || "—".into(),
            |ts| {
                let minutes = chrono::Utc::now().timestamp().saturating_sub(ts).max(0) / 60;
                if minutes >= 1440 {
                    format!("{}d {}h", minutes / 1440, minutes % 1440 / 60)
                } else {
                    format!("{}h {}m", minutes / 60, minutes % 60)
                }
            },
        );
        p.text(pos2(r.left() + x[3], y), Align2::LEFT_CENTER, age, mono(12.0), muted());
        let detail =
            row.error.clone().or_else(|| row.evaluation.as_ref().and_then(|e| e.reason.clone())).unwrap_or_else(|| {
                row.evaluation
                    .as_ref()
                    .map(|e| {
                        e.conditions
                            .iter()
                            .zip(pane.definition.leaves())
                            .map(|(c, leaf)| {
                                let value = |v: Option<f64>| v.map_or_else(|| "—".into(), compact);
                                format!("{}: {} / {}", leaf.label(), value(c.left), value(c.right))
                            })
                            .collect::<Vec<_>>()
                            .join("   ")
                    })
                    .unwrap_or_default()
            });
        let detail_color = if row.error.is_some() { theme::down() } else { muted() };
        p.text(pos2(r.left() + x[4], y), Align2::LEFT_CENTER, &detail, mono(12.0), detail_color);
        let source = if row.simulated {
            "Simulated data"
        } else if row.error.is_some() {
            "Data unavailable"
        } else if market {
            "Stored bars"
        } else {
            "Provider snapshot"
        };
        let resp = resp.on_hover_text(format!(
            "{} · {source}\n{detail}\nClick to chart. Bar age is the age of the bar's opening timestamp, not a freshness guarantee.",
            row.symbol
        ));
        if resp.clicked() {
            if outcome == ScanOutcome::Match {
                actions.push(Action::ScanChart {
                    symbol: row.symbol.clone(),
                    symbols: pane
                        .rows
                        .iter()
                        .filter(|r| r.evaluation.as_ref().is_some_and(|e| e.outcome == ScanOutcome::Match))
                        .map(|r| tr_data::security(&r.symbol).key())
                        .take(5000)
                        .collect(),
                    definition: pane.definition.clone(),
                });
            } else {
                actions.push(Action::OpenChart(row.symbol.clone()));
            }
        }
    }
    if shown == 0 {
        ui.add_space(dp(6.0));
        ui.label(RichText::new("No rows to show with the current filters.").color(muted()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_due_time_waits_after_completion_with_backpressure() {
        let completed = chrono::DateTime::from_timestamp(1_000, 0).unwrap();
        assert_eq!(due_at(completed, 5, chrono::Duration::seconds(10)), completed + chrono::Duration::minutes(5));
        assert_eq!(due_at(completed, 1, chrono::Duration::minutes(10)), completed + chrono::Duration::minutes(1));
        assert_eq!(due_at(completed, 0, chrono::Duration::seconds(10)), completed + chrono::Duration::seconds(30));
    }

    #[test]
    #[ignore = "BUG: due_at's overrun arm `interval.max(chrono::Duration::seconds(30))` is a no-op for every supported refresh interval — 1|5|15|60 minutes are all >= 60s so max() equals interval and due is always `completed + interval`. run_duration is only read by that dead comparison, so a run that overruns its interval is rescheduled exactly like a short one (waiting another full interval on top of the overrun) and the 30-second floor the arm encodes (exercised only by the impossible interval=0 case) never applies."]
    fn refresh_due_time_applies_backpressure_when_a_run_overruns_its_interval() {
        let completed = chrono::DateTime::from_timestamp(1_000, 0).unwrap();
        // Overrun: a 30-minute run against a 5-minute interval has already blown the
        // cadence; the next run must not wait yet another full interval.
        assert_eq!(
            due_at(completed, 5, chrono::Duration::minutes(30)),
            completed + chrono::Duration::seconds(30),
            "overrun runs must re-arm after the 30s floor, not a full interval later"
        );
        // A short run keeps the requested cadence.
        assert_eq!(due_at(completed, 5, chrono::Duration::seconds(10)), completed + chrono::Duration::minutes(5));
    }

    #[test]
    fn match_diff_requires_symbol_presence_in_both_universes() {
        let set = |symbols: &[&str]| symbols.iter().map(|s| (*s).to_string()).collect::<HashSet<_>>();
        // VANISHED dropped out of the universe entirely; UNAVAILABLE is unevaluated now.
        let previous = set(&["VANISHED", "STAYED", "UNAVAILABLE"]);
        let previous_evaluated = set(&["VANISHED", "STAYED", "UNAVAILABLE"]);
        let current = set(&["STAYED", "BRAND_NEW"]);
        let evaluated = set(&["STAYED", "BRAND_NEW"]);
        let (entered, exited) = match_diff(&previous, &previous_evaluated, &current, &evaluated);
        // BRAND_NEW was never evaluated before, so it cannot be claimed as "entered".
        assert!(entered.is_empty());
        // VANISHED is gone and UNAVAILABLE is not evaluated now: neither is "dropped".
        assert!(exited.is_empty());
    }

    #[test]
    fn match_changes_require_available_evaluations_in_both_runs() {
        let set = |symbols: &[&str]| symbols.iter().map(|s| (*s).to_string()).collect::<HashSet<_>>();
        let previous = set(&["KEEP", "DROP", "UNAVAILABLE"]);
        let previous_evaluated = set(&["KEEP", "DROP", "UNAVAILABLE", "ENTER"]);
        let current = set(&["KEEP", "ENTER", "FIRST"]);
        let evaluated = set(&["KEEP", "DROP", "ENTER", "FIRST"]);
        let (entered, exited) = match_diff(&previous, &previous_evaluated, &current, &evaluated);
        assert_eq!(entered, set(&["ENTER"]));
        assert_eq!(exited, vec!["DROP"]);
    }

    #[test]
    fn large_universes_restrict_short_refresh_intervals() {
        for minutes in [1, 5, 15, 60] {
            assert!(interval_allowed(1000, minutes));
            assert_eq!(interval_allowed(1001, minutes), minutes >= 15);
        }
        assert!(!interval_allowed(100, 2));
        let mut pane = ScannerPane::default();
        pane.universe = Universe::Nasdaq;
        pane.refresh = Some(1);
        pane.set_progress(0, 2000);
        assert_eq!(pane.refresh, Some(15));
    }

    #[test]
    fn stop_disables_auto_refresh_and_signals_cancellation() {
        let mut pane = ScannerPane::default();
        pane.refresh = Some(5);
        pane.running = true;
        let cancel = Arc::new(AtomicBool::new(false));
        pane.cancel = Some(cancel.clone());
        pane.stop();
        assert_eq!(pane.refresh, None);
        assert!(cancel.load(Ordering::Relaxed));
        assert!(pane.running);
    }

    fn unavailable_row(symbol: &str) -> ScanRow {
        ScanRow {
            symbol: symbol.into(),
            evaluation: None,
            ts: None,
            simulated: false,
            error: Some("Unavailable".into()),
        }
    }

    #[test]
    fn partial_rows_append_then_final_replaces_them() {
        let mut pane = ScannerPane::default();
        pane.generation = 42;
        pane.running = true;
        pane.partial(42, vec![unavailable_row("A")]);
        pane.partial(42, vec![unavailable_row("B")]);
        assert_eq!(pane.rows.iter().map(|r| r.symbol.as_str()).collect::<Vec<_>>(), vec!["A", "B"]);
        pane.finish(42, vec![unavailable_row("FINAL")]);
        assert_eq!(pane.rows.len(), 1);
        assert_eq!(pane.rows[0].symbol, "FINAL");
        assert!(!pane.running);
        assert!(pane.completed.is_some());
        pane.partial(42, vec![unavailable_row("LATE")]);
        assert_eq!(pane.rows.len(), 1);
    }

    #[test]
    fn completed_runs_track_changes_and_invalidate_resets_baseline() {
        let row = |symbol: &str, outcome| ScanRow {
            symbol: symbol.into(),
            evaluation: Some(tr_core::scanner::ScanEvaluation { outcome, conditions: Vec::new(), reason: None }),
            ts: None,
            simulated: false,
            error: None,
        };
        let mut pane = ScannerPane::default();
        pane.finish(
            0,
            vec![row("A", ScanOutcome::Match), row("B", ScanOutcome::NoMatch), row("C", ScanOutcome::Match)],
        );
        assert!(pane.entered.is_empty());
        assert!(pane.exited.is_empty());
        pane.running = true;
        pane.partial(0, vec![row("B", ScanOutcome::Match)]);
        assert!(!pane.previous_matches.contains("B"));
        pane.finish(0, vec![row("A", ScanOutcome::NoMatch), row("B", ScanOutcome::Match), unavailable_row("C")]);
        assert_eq!(pane.entered, HashSet::from(["B".into()]));
        assert_eq!(pane.exited, vec!["A"]);
        pane.invalidate();
        assert!(!pane.baseline_ready);
        assert!(pane.previous_matches.is_empty());
        pane.finish(0, vec![row("A", ScanOutcome::Match)]);
        assert!(pane.entered.is_empty());
        assert!(pane.exited.is_empty());
    }

    #[test]
    fn auto_refresh_waits_until_due_and_never_overlaps() {
        let ctx = egui::Context::default();
        let mut pane = ScannerPane::default();
        pane.refresh = Some(1);
        pane.completed = Some(chrono::Utc::now());
        let mut requests = 0;
        for state in 0..3 {
            if state == 1 {
                pane.completed = Some(chrono::Utc::now() - chrono::Duration::minutes(2));
            }
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    show(ui, &mut pane, 1, &["AAPL US Equity".into()], false, |_| requests += 1);
                });
            });
            output.textures_delta.clear();
            assert_eq!(requests, usize::from(state > 0));
        }
        assert!(pane.running);
    }

    #[test]
    fn stale_generation_partials_are_dropped() {
        let mut pane = ScannerPane::default();
        pane.generation = 42;
        pane.running = true;
        pane.partial(41, vec![unavailable_row("STALE")]);
        assert!(pane.rows.is_empty());
        pane.finish(41, vec![unavailable_row("STALE")]);
        assert!(pane.rows.is_empty());
        assert!(pane.running);
    }

    #[test]
    fn scan_settings_without_refresh_load_off() {
        let legacy = serde_json::to_string(&ScanDefinition::default()).unwrap();
        let settings: ScanSettings = serde_json::from_str(&legacy).unwrap();
        assert_eq!(settings.refresh, None);
        let settings = ScanSettings { refresh: Some(15), ..settings };
        assert_eq!(serde_json::from_str::<ScanSettings>(&serde_json::to_string(&settings).unwrap()).unwrap(), settings);
    }

    #[test]
    fn nested_group_editing_keeps_ids_unique() {
        let compare = ConditionItem::Compare(ScanCondition::default());
        let formula = ConditionItem::Formula(FormulaCondition {
            expression: "C > AVGC20".into(),
            timeframe: Some(Timeframe::W1),
        });
        let definition = ScanDefinition {
            root: ConditionGroup {
                mode: MatchMode::All,
                items: vec![
                    compare.clone(),
                    ConditionItem::Group(ConditionGroup {
                        mode: MatchMode::Any,
                        items: vec![compare, formula.clone()],
                    }),
                    formula,
                ],
            },
            ..Default::default()
        };
        let mut pane = ScannerPane::new(definition.clone());
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                show(ui, &mut pane, 1, &["AAPL US Equity".into()], false, |_| {
                    panic!("No scan requested during render")
                });
            });
        });
        assert!(!output.shapes.is_empty());
        output.textures_delta.clear();
        assert_eq!(pane.definition, definition);
    }

    #[test]
    fn remove_at_tolerates_stale_and_malformed_paths() {
        let compare = ConditionItem::Compare(ScanCondition::default());
        let formula = |n: &str| ConditionItem::Formula(FormulaCondition { expression: n.into(), timeframe: None });
        let mut root = ConditionGroup {
            mode: MatchMode::All,
            items: vec![
                compare.clone(),
                ConditionItem::Group(ConditionGroup { mode: MatchMode::Any, items: vec![formula("A"), formula("B")] }),
                formula("C"),
            ],
        };
        // Empty path is a no-op.
        remove_at(&mut root, &[]);
        assert_eq!(root.items.len(), 3);
        // Out-of-range leaf index is a no-op.
        remove_at(&mut root, &[9]);
        assert_eq!(root.items.len(), 3);
        // Path into a non-group leaf is a no-op (item 0 is a Compare).
        remove_at(&mut root, &[0, 0]);
        assert_eq!(root.items.len(), 3);
        // Out-of-range index inside a valid group is a no-op.
        remove_at(&mut root, &[1, 9]);
        assert_eq!(root.items.len(), 3);
        // Deep path that runs off the tree is a no-op.
        remove_at(&mut root, &[1, 0, 0, 0]);
        assert_eq!(root.items.len(), 3);
        // Valid removal of a nested leaf leaves the group standing.
        remove_at(&mut root, &[1, 0]);
        let ConditionItem::Group(child) = &root.items[1] else { panic!("Expected group") };
        assert_eq!(child.items.len(), 1);
        // Removing the last nested leaf prunes the now-empty group.
        remove_at(&mut root, &[1, 0]);
        assert_eq!(root.items.len(), 2);
        // Stale path after that pruning must not touch the new neighbours.
        remove_at(&mut root, &[1, 0]);
        assert_eq!(root.items.len(), 2);
        remove_at(&mut root, &[0]);
        remove_at(&mut root, &[0]);
        assert!(root.items.is_empty());
    }

    #[test]
    fn sibling_group_ids_do_not_collide() {
        // Two sibling groups and a nested grandchild all render ComboBoxes that share
        // fixed salts ("scan-mode", "condition-timeframe", "comparison", "left"/"right").
        // push_id(index) must keep their persistent ids distinct, and editing one
        // group must not rewrite the other.
        let compare =
            |tf: Option<Timeframe>| ConditionItem::Compare(ScanCondition { timeframe: tf, ..ScanCondition::default() });
        let definition = ScanDefinition {
            root: ConditionGroup {
                mode: MatchMode::All,
                items: vec![
                    ConditionItem::Group(ConditionGroup {
                        mode: MatchMode::Any,
                        items: vec![compare(Some(Timeframe::W1)), compare(None)],
                    }),
                    ConditionItem::Group(ConditionGroup {
                        mode: MatchMode::All,
                        items: vec![
                            compare(Some(Timeframe::H1)),
                            ConditionItem::Group(ConditionGroup {
                                mode: MatchMode::Any,
                                items: vec![ConditionItem::Formula(FormulaCondition {
                                    expression: "C > 1".into(),
                                    timeframe: Some(Timeframe::W1),
                                })],
                            }),
                        ],
                    }),
                ],
            },
            ..Default::default()
        };
        assert!(definition.validate().is_ok());
        let mut pane = ScannerPane::new(definition.clone());
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                show(ui, &mut pane, 1, &["AAPL US Equity".into()], false, |_| {
                    panic!("No scan requested during render")
                });
            });
        });
        assert!(!output.shapes.is_empty());
        output.textures_delta.clear();
        // Rendering alone must never mutate the definition.
        assert_eq!(pane.definition, definition);

        // Distinct sibling-group leaves report distinct labels and paths, so a
        // remove aimed at one group cannot land on the other.
        let mut root = pane.definition.root.clone();
        remove_at(&mut root, &[0, 0]);
        assert_eq!(root.items.len(), 2);
        let ConditionItem::Group(first) = &root.items[0] else { panic!("Expected group") };
        assert_eq!(first.items.len(), 1);
        let ConditionItem::Group(second) = &root.items[1] else { panic!("Expected group") };
        assert_eq!(second.items.len(), 2);
    }

    #[test]
    fn leaf_budget_counts_group_defaults_on_add() {
        // The editor increments the shared leaf counter once per Add, and a new
        // group contributes exactly one default comparison.
        assert!(can_add_leaf(MAX_CONDITIONS - 1));
        assert!(!can_add_leaf(MAX_CONDITIONS));
        // Depth gate: a new group lands at depth+1, so the button is only offered
        // while depth < MAX_GROUP_DEPTH.
        for depth in 1..=MAX_GROUP_DEPTH + 1 {
            assert_eq!(can_add_group(depth), depth < MAX_GROUP_DEPTH, "depth {depth}");
        }
    }

    #[test]
    fn nested_definition_survives_scan_library_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "tr-scan-nested-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let definition = ScanDefinition {
            name: "Nested".into(),
            timeframe: Timeframe::W1,
            root: ConditionGroup {
                mode: MatchMode::Any,
                items: vec![
                    ConditionItem::Compare(ScanCondition {
                        timeframe: Some(Timeframe::H1),
                        left_bars_ago: 2,
                        right_bars_ago: 3,
                        right_multiplier: 2.5,
                        ..ScanCondition::default()
                    }),
                    ConditionItem::Formula(FormulaCondition {
                        expression: "C > AVGC20".into(),
                        timeframe: Some(Timeframe::W1),
                    }),
                    ConditionItem::Group(ConditionGroup {
                        mode: MatchMode::All,
                        items: vec![
                            ConditionItem::Compare(ScanCondition { right_multiplier: 0.5, ..ScanCondition::default() }),
                            ConditionItem::Group(ConditionGroup {
                                mode: MatchMode::Any,
                                items: vec![ConditionItem::Formula(FormulaCondition {
                                    expression: "V > 1000".into(),
                                    timeframe: None,
                                })],
                            }),
                        ],
                    }),
                ],
            },
        };
        assert!(definition.validate().is_ok());
        let settings = ScanSettings { definition: definition.clone(), universe: Universe::Nasdaq, refresh: None };
        let saved = save_definition_at(&settings, &dir).unwrap();
        assert_eq!(saved.len(), 1);
        let loaded = load_saved_at(&dir).unwrap();
        assert_eq!(loaded[0].definition, definition);
        assert_eq!(loaded[0].universe, Universe::Nasdaq);
        // JSON on disk is the version 1 form with root, not the legacy flat form.
        let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("scans.json")).unwrap()).unwrap();
        assert_eq!(raw[0]["version"], 1);
        assert!(raw[0].get("conditions").is_none());
        assert_eq!(raw[0]["root"]["items"][1]["Formula"]["expression"], "C > AVGC20");
        assert_eq!(raw[0]["root"]["items"][0]["Compare"]["right_multiplier"], 2.5);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn removing_last_child_removes_nested_group() {
        let compare = ConditionItem::Compare(ScanCondition::default());
        let mut root = ConditionGroup {
            mode: MatchMode::All,
            items: vec![
                compare.clone(),
                ConditionItem::Group(ConditionGroup {
                    mode: MatchMode::Any,
                    items: vec![
                        compare.clone(),
                        ConditionItem::Group(ConditionGroup { mode: MatchMode::All, items: vec![compare.clone()] }),
                    ],
                }),
                compare.clone(),
            ],
        };
        remove_at(&mut root, &[1, 1, 0]);
        assert_eq!(root.items.len(), 3);
        let ConditionItem::Group(child) = &root.items[1] else { panic!("Expected group") };
        assert_eq!(child.items, vec![compare.clone()]);
        remove_at(&mut root, &[1, 0]);
        assert_eq!(root.items, vec![compare.clone(), compare]);
        remove_at(&mut root, &[0]);
        remove_at(&mut root, &[0]);
        assert!(root.items.is_empty());
    }

    #[test]
    fn add_group_respects_depth_and_leaf_limits() {
        assert!(can_add_group(1));
        assert!(can_add_group(MAX_GROUP_DEPTH - 1));
        assert!(!can_add_group(MAX_GROUP_DEPTH));
        assert!(!can_add_group(MAX_GROUP_DEPTH + 1));
        assert!(can_add_leaf(MAX_CONDITIONS - 1));
        assert!(!can_add_leaf(MAX_CONDITIONS));
        assert!(!can_add_leaf(MAX_CONDITIONS + 1));
    }

    #[test]
    fn run_is_blocked_by_invalid_definition() {
        let mut definition = ScanDefinition::default();
        assert!(run_enabled(&definition, false, true));
        assert!(!run_enabled(&definition, true, true));
        assert!(!run_enabled(&definition, false, false));
        for item in [
            ConditionItem::Group(ConditionGroup { mode: MatchMode::All, items: Vec::new() }),
            ConditionItem::Formula(FormulaCondition { expression: "C >".into(), timeframe: None }),
        ] {
            definition.root.items = vec![item];
            assert!(definition.validate().is_err());
            assert!(!run_enabled(&definition, false, true));
        }
    }

    #[test]
    fn narrow_condition_rows_reserve_the_remove_column_on_every_line() {
        for width in [280.0, 360.0, 530.0, 900.0] {
            let mut items = [
                ConditionItem::Compare(ScanCondition { right: ScanOperand::Constant(50.0), ..Default::default() }),
                ConditionItem::Compare(ScanCondition {
                    right: ScanOperand::Sma { period: 20, source: Source::Close },
                    right_multiplier: 2.5,
                    timeframe: Some(Timeframe::W1),
                    ..Default::default()
                }),
                ConditionItem::Formula(FormulaCondition {
                    expression: "C > AVGC20".into(),
                    timeframe: Some(Timeframe::H1),
                }),
            ];
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 1200.0))),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        for (index, item) in items.iter_mut().enumerate() {
                            ui.push_id(index, |ui| {
                                let available = ui.available_rect_before_wrap();
                                let row =
                                    egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 4)).show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        condition_row(ui, |ui| match item {
                                            ConditionItem::Compare(condition) => compare_editor(ui, condition, index),
                                            ConditionItem::Formula(formula) => {
                                                ui.add(
                                                    egui::TextEdit::singleline(&mut formula.expression)
                                                        .desired_width(dp(280.0)),
                                                );
                                                timeframe_editor(ui, &mut formula.timeframe);
                                            }
                                            ConditionItem::Group(_) => unreachable!(),
                                        })
                                    });
                                let (content, close) = row.inner;
                                let frame = row.response.rect;
                                assert!(close.rect.is_positive(), "width {width}, row {index}: empty close button");
                                assert!(
                                    frame.contains_rect(close.rect),
                                    "width {width}, row {index}: frame {frame:?}, close {:?}",
                                    close.rect
                                );
                                assert!(
                                    available.contains_rect(close.rect),
                                    "Close button must remain inside the pane"
                                );
                                assert!(ui.clip_rect().contains_rect(close.rect), "Close button must not be clipped");
                                assert!(
                                    !content.intersects(close.rect),
                                    "width {width}, row {index}: content {content:?}, close {:?}",
                                    close.rect
                                );
                                assert!(content.right() < close.rect.left());
                                if width <= 530.0 && matches!(item, ConditionItem::Compare(_)) {
                                    assert!(content.height() > close.rect.height(), "The Compare controls must wrap");
                                }
                            });
                        }
                    });
                },
            );
            output.textures_delta.clear();
        }
    }

    #[test]
    fn scanner_ui_renders_empty_running_and_result_states() {
        let ctx = egui::Context::default();
        let mut pane = ScannerPane {
            definition: ScanDefinition::default(),
            generation: 0,
            running: false,
            rows: Vec::new(),
            completed: None,
            snapshot_symbols: Vec::new(),
            saved: Vec::new(),
            notice: None,
            matches_only: true,
            universe: Universe::Watchlist,
            progress: None,
            cancel: None,
            show_unavailable: false,
            refresh: None,
            previous_matches: HashSet::new(),
            previous_evaluated: HashSet::new(),
            baseline_ready: false,
            entered: HashSet::new(),
            exited: Vec::new(),
            started: None,
            run_duration: chrono::Duration::zero(),
            universe_size: None,
        };
        // Full pane layout, including narrow docks, without a network worker or GPU.
        for width in [280.0, 900.0] {
            for state in 0..3 {
                pane.running = state == 1;
                if state == 2 {
                    pane.completed = Some(chrono::Utc::now());
                    pane.rows = vec![ScanRow {
                        symbol: "AAPL US Equity".into(),
                        evaluation: None,
                        ts: None,
                        simulated: false,
                        error: Some("Provider unavailable".into()),
                    }];
                }
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 600.0))),
                        ..Default::default()
                    },
                    |ui| {
                        egui::CentralPanel::default().show(ui, |ui| {
                            show(ui, &mut pane, 1, &["AAPL US Equity".into()], false, |_| {
                                panic!("No scan requested during render")
                            });
                        });
                    },
                );
                assert!(!output.shapes.is_empty());
                // This headless pass intentionally has no renderer to upload textures.
                output.textures_delta.clear();
            }
        }
    }

    #[test]
    fn scan_library_merges_updates_and_preserves_corrupt_files() {
        let dir = std::env::temp_dir().join(format!(
            "tr-scan-library-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let mut definition = ScanSettings {
            definition: ScanDefinition { name: "First".into(), ..Default::default() },
            universe: Universe::Nasdaq,
            refresh: None,
        };
        assert_eq!(save_definition_at(&definition, &dir).unwrap().len(), 1);
        definition.definition.name = "Second".into();
        definition.universe = Universe::Nyse;
        assert_eq!(save_definition_at(&definition, &dir).unwrap().len(), 2);
        definition.definition.name = " first ".into();
        definition.definition.timeframe = Timeframe::W1;
        let saved = save_definition_at(&definition, &dir).unwrap();
        assert_eq!(saved.len(), 2);
        assert!(saved.iter().any(|s| s.definition.name == "first"
            && s.definition.timeframe == Timeframe::W1
            && s.universe == Universe::Nyse));
        assert_eq!(load_saved_at(&dir).unwrap(), saved);
        std::fs::write(dir.join("scans.json"), b"broken JSON").unwrap();
        assert!(save_definition_at(&definition, &dir).is_err());
        assert_eq!(std::fs::read(dir.join("scans.json")).unwrap(), b"broken JSON");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn large_values_are_compact() {
        assert_eq!(compact(339.5), "339.50");
        assert_eq!(compact(99_999.0), "99999.00");
        assert_eq!(compact(100_000.0), "100.0K");
        assert_eq!(compact(35_851_382.0), "35.85M");
        assert_eq!(compact(-2.5e9), "-2.50B");
    }

    #[test]
    fn legacy_scan_libraries_default_to_watchlist() {
        let legacy = serde_json::to_string(&vec![ScanDefinition::default()]).unwrap();
        let restored: Vec<ScanSettings> = serde_json::from_str(&legacy).unwrap();
        assert_eq!(restored[0].universe, Universe::Watchlist);
        assert_eq!(restored[0].definition, ScanDefinition::default());
    }
}
