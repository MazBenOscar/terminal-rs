//! Watchlist and market-wide snapshots: editable conditions, named definitions and explicit
//! data status.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use egui::{RichText, Ui};
use tr_core::Timeframe;
use tr_core::scanner::{
    Comparison, MAX_BARS_AGO, MatchMode, ScanCondition, ScanDefinition, ScanOperand, ScanOutcome, Source,
};

use crate::congress_view::Action;
use crate::theme::{amber, mono, muted};
use tr_data::universe::Universe;

use crate::worker::{PaneId, Request, ScanRow, data_dir};

pub struct ScannerPane {
    pub definition: ScanDefinition,
    pub generation: u64,
    pub running: bool,
    pub rows: Vec<ScanRow>,
    pub completed: Option<chrono::DateTime<chrono::Utc>>,
    snapshot_symbols: Vec<String>,
    saved: Vec<ScanDefinition>,
    notice: Option<String>,
    matches_only: bool,
    pub universe: Universe,
    /// Universe scan progress: symbols evaluated and total.
    pub progress: Option<(usize, usize)>,
    /// Stops the running universe scan.
    pub cancel: Option<Arc<AtomicBool>>,
    show_unavailable: bool,
}

impl Default for ScannerPane {
    fn default() -> Self {
        Self::new(ScanDefinition::default())
    }
}

impl ScannerPane {
    pub fn new(definition: ScanDefinition) -> Self {
        let (saved, notice) = match load_saved() {
            Ok(saved) => (saved, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        Self {
            definition,
            generation: 0,
            running: false,
            rows: Vec::new(),
            completed: None,
            snapshot_symbols: Vec::new(),
            saved,
            notice,
            matches_only: true,
            universe: Universe::Watchlist,
            progress: None,
            cancel: None,
            show_unavailable: false,
        }
    }

    fn invalidate(&mut self) {
        self.rows.clear();
        self.completed = None;
    }
}

impl Drop for ScannerPane {
    /// A closed pane stops its universe scan instead of letting it run on unseen.
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

fn load_saved() -> Result<Vec<ScanDefinition>, String> {
    load_saved_at(&data_dir())
}

fn load_saved_at(dir: &std::path::Path) -> Result<Vec<ScanDefinition>, String> {
    match std::fs::read(dir.join("scans.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("Could not read saved scans: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("Could not open saved scans: {e}")),
    }
}

/// Merge against disk so saving from two scanner panes does not discard the other scan.
fn save_definition(definition: &ScanDefinition) -> Result<Vec<ScanDefinition>, String> {
    save_definition_at(definition, &data_dir())
}

fn save_definition_at(definition: &ScanDefinition, dir: &std::path::Path) -> Result<Vec<ScanDefinition>, String> {
    definition.validate()?;
    let mut saved = load_saved_at(dir)?;
    saved.retain(|s| !s.name.eq_ignore_ascii_case(definition.name.trim()));
    let mut definition = definition.clone();
    definition.name = definition.name.trim().into();
    saved.push(definition);
    saved.sort_by(|a, b| a.name.cmp(&b.name));
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

fn operand_editor(ui: &mut Ui, operand: &mut ScanOperand, bars_ago: &mut usize, id: &str) {
    let mut kind = operand_kind(operand);
    let before = kind;
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
    egui::ScrollArea::vertical().id_salt(("scanner", id)).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            if market {
                ui.label(RichText::new("MARKET SCANNER").font(mono(12.0)).color(amber()));
                ui.label(format!("{} · manual snapshot", pane.universe.label()));
            } else {
                ui.label(RichText::new("WATCHLIST SCANNER").font(mono(12.0)).color(amber()));
                ui.label(format!("{} symbols · manual snapshot", symbols.len()));
            }
        });
        ui.label(RichText::new("Uses the latest provider bar, which may still be forming. No automatic refresh.").small().color(muted()));
        let before = pane.definition.clone();
        ui.add_enabled_ui(!pane.running, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut pane.definition.name).desired_width(160.0).char_limit(80));
                ui.label("Timeframe");
                egui::ComboBox::from_id_salt("scan-timeframe")
                    .selected_text(pane.definition.timeframe.label()).show_ui(ui, |ui| {
                    for tf in Timeframe::ALL {
                        ui.selectable_value(&mut pane.definition.timeframe, tf, tf.label());
                    }
                });
                ui.label("Universe");
                let before = pane.universe;
                egui::ComboBox::from_id_salt("scan-universe").selected_text(pane.universe.label()).show_ui(ui, |ui| {
                    for universe in Universe::ALL {
                        let enabled = universe == Universe::Watchlist || bulk_ready;
                        ui.add_enabled_ui(enabled, |ui| {
                            ui.selectable_value(&mut pane.universe, universe, universe.label())
                                .on_disabled_hover_text("Market-wide scans need a bulk data source: set up Questrade in Settings › Brokerage");
                        });
                    }
                });
                if pane.universe != before {
                    pane.invalidate();
                }
                egui::ComboBox::from_id_salt("scan-mode")
                    .selected_text(if pane.definition.match_mode == MatchMode::All { "Match all" } else { "Match any" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut pane.definition.match_mode, MatchMode::All, "Match all");
                        ui.selectable_value(&mut pane.definition.match_mode, MatchMode::Any, "Match any");
                    });
            });
            let mut remove = None;
            for (i, condition) in pane.definition.conditions.iter_mut().enumerate() {
                ui.push_id(i, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        operand_editor(ui, &mut condition.left, &mut condition.left_bars_ago, "left");
                        egui::ComboBox::from_id_salt("comparison").width(115.0)
                            .selected_text(condition.comparison.label()).show_ui(ui, |ui| {
                            for comparison in Comparison::ALL {
                                ui.selectable_value(&mut condition.comparison, comparison, comparison.label());
                            }
                        });
                        operand_editor(ui, &mut condition.right, &mut condition.right_bars_ago, "right");
                        // Scaling a constant is just a different constant.
                        if matches!(condition.right, ScanOperand::Constant(_)) {
                            condition.right_multiplier = 1.0;
                        } else {
                            ui.add(egui::DragValue::new(&mut condition.right_multiplier).speed(0.05).max_decimals(3).prefix("× "))
                                .on_hover_text("Multiplies the right side, e.g. Volume >= EMA(50, Volume) × 2.5");
                        }
                        if ui.small_button("Remove").clicked() { remove = Some(i); }
                    });
                });
            }
            if let Some(i) = remove { pane.definition.conditions.remove(i); }
            if pane.definition.conditions.len() < 32 && ui.button("Add condition").clicked() {
                pane.definition.conditions.push(ScanCondition::default());
            }
        });
        if pane.definition != before { pane.invalidate(); }
        ui.separator();
        let validation = pane.definition.validate().err();
        if let Some(error) = &validation { ui.label(error); }
        let can_run = !pane.running
            && validation.is_none()
            && if market { bulk_ready } else { !symbols.is_empty() && symbols.len() <= 200 };
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(can_run, egui::Button::new("Run scan")).clicked() {
                // Never reuse a generation after restoring a layout or recreating a pane.
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                pane.generation = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                pane.invalidate();
                pane.running = true;
                pane.notice = None;
                if market {
                    let cancel = Arc::new(AtomicBool::new(false));
                    pane.cancel = Some(cancel.clone());
                    pane.snapshot_symbols.clear();
                    pane.progress = Some((0, 0));
                    send(Request::ScanUniverse {
                        pane: id, generation: pane.generation, definition: pane.definition.clone(), universe: pane.universe, cancel,
                    });
                } else {
                    pane.snapshot_symbols = symbols.to_vec();
                    send(Request::Scan {
                        pane: id, generation: pane.generation, definition: pane.definition.clone(), symbols: symbols.to_vec(),
                    });
                }
            }
            if ui.add_enabled(!pane.running && validation.is_none(), egui::Button::new("Save scan")).clicked() {
                match save_definition(&pane.definition) {
                    Ok(saved) => { pane.saved = saved; pane.notice = Some("Scan saved to scans.json".into()); }
                    Err(e) => pane.notice = Some(e),
                }
            }
            let mut selected = None;
            ui.add_enabled_ui(!pane.running, |ui| {
                egui::ComboBox::from_id_salt("saved-scans").selected_text("Load scan").show_ui(ui, |ui| {
                    ui.label(RichText::new("PRESETS").small().color(muted()));
                    for preset in ScanDefinition::presets() {
                        if ui.selectable_label(false, &preset.name).clicked() { selected = Some(preset); }
                    }
                    ui.separator();
                    ui.label(RichText::new("SAVED").small().color(muted()));
                    if pane.saved.is_empty() { ui.label("No saved scans yet"); }
                    for saved in &pane.saved {
                        if ui.selectable_label(false, &saved.name).clicked() { selected = Some(saved.clone()); }
                    }
                });
            });
            if let Some(definition) = selected { pane.definition = definition; pane.invalidate(); }
            if pane.running && !market { ui.spinner(); ui.label(format!("Scanning {} symbols… up to 15 seconds per symbol, four at a time", pane.snapshot_symbols.len())); }
        });
        if pane.running && market {
            let (done, total) = pane.progress.unwrap_or((0, 0));
            ui.horizontal(|ui| {
                let fraction = if total == 0 { 0.0 } else { done as f32 / total as f32 };
                let text = if total == 0 { "Loading the US symbol directory…".to_string() } else { format!("{done} / {total}") };
                ui.add(egui::ProgressBar::new(fraction).text(text).desired_width(260.0));
                let stopping = pane.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed));
                if ui.add_enabled(!stopping, egui::Button::new(if stopping { "Stopping…" } else { "Stop" })).clicked()
                    && let Some(cancel) = &pane.cancel
                {
                    cancel.store(true, Ordering::Relaxed);
                }
            });
            ui.label(RichText::new("The first run downloads full history for every stock (about 10–15 minutes); later runs reuse stored bars and fetch only the newest.").small().color(muted()));
        }
        if market && !bulk_ready { ui.label("Market-wide scans need a bulk data source: set up Questrade in Settings › Brokerage."); }
        if !market && symbols.is_empty() { ui.label("Your watchlist is empty. Add symbols with WL ADD <KEY>, then run the scan."); }
        if !market && symbols.len() > 200 { ui.label("This version supports up to 200 watchlist symbols. Reduce the list before running."); }
        if let Some(notice) = &pane.notice { ui.label(notice); }
        if let Some(completed) = pane.completed {
            let count = |outcome| pane.rows.iter().filter(|r| r.evaluation.as_ref().is_some_and(|e| e.outcome == outcome)).count();
            let unavailable = pane.rows.iter().filter(|r| r.error.is_some() || r.evaluation.as_ref().is_none_or(|e| e.outcome == ScanOutcome::Unavailable)).count();
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{} matches · {} non-matches · {} unavailable", count(ScanOutcome::Match), count(ScanOutcome::NoMatch), unavailable));
                if market {
                    ui.checkbox(&mut pane.matches_only, "Matches only");
                    ui.checkbox(&mut pane.show_unavailable, "Show unavailable");
                } else {
                    ui.checkbox(&mut pane.matches_only, "Matches only (always show errors)");
                }
            });
            let scanned = if market { pane.rows.len() } else { pane.snapshot_symbols.len() };
            ui.label(RichText::new(format!("Scanned {scanned} symbols · finished {} UTC · click a symbol to chart it", completed.format("%Y-%m-%d %H:%M:%S"))).small().color(muted()));
            if !market && pane.snapshot_symbols != symbols {
                ui.label("The watchlist has changed since this snapshot. Run again to scan the current list.");
            }
            if pane.rows.iter().any(|r| r.simulated) { ui.label(RichText::new("SIMULATED DATA — these results use generated sample bars.").color(amber())); }
            egui::ScrollArea::horizontal().id_salt("scan-results").show(ui, |ui| {
                egui::Grid::new("scan-grid").striped(true).min_col_width(90.0).show(ui, |ui| {
                    for title in ["Security", "Result", "Data", "Last bar (UTC)", "Bar age", "Details"] { ui.strong(title); }
                    ui.end_row();
                    for row in &pane.rows {
                        let outcome = row.evaluation.as_ref().map(|e| e.outcome).unwrap_or(ScanOutcome::Unavailable);
                        if pane.matches_only && outcome == ScanOutcome::NoMatch { continue; }
                        if market && !pane.show_unavailable && outcome == ScanOutcome::Unavailable { continue; }
                        if ui.link(&row.symbol).clicked() { actions.push(Action::OpenChart(row.symbol.clone())); }
                        ui.label(match outcome { ScanOutcome::Match => "Match", ScanOutcome::NoMatch => "No match", ScanOutcome::Unavailable => "Unavailable" });
                        ui.label(if row.simulated { "Simulated" } else if row.error.is_some() { "Unavailable" } else if market { "Stored bars" } else { "Provider snapshot" });
                        let ts = row.ts.and_then(|ts| chrono::DateTime::from_timestamp(ts, 0));
                        ui.label(ts.map_or_else(|| "—".into(), |ts| ts.format("%Y-%m-%d %H:%M").to_string()));
                        ui.label(row.ts.map_or_else(|| "—".into(), |ts| {
                            let minutes = chrono::Utc::now().timestamp().saturating_sub(ts).max(0) / 60;
                            if minutes >= 1440 { format!("{}d {}h", minutes / 1440, minutes % 1440 / 60) }
                            else { format!("{}h {}m", minutes / 60, minutes % 60) }
                        })).on_hover_text("Age of the bar's opening timestamp, not a freshness guarantee. Closed markets and longer timeframes naturally have older bars.");
                        let detail = row.error.clone().or_else(|| row.evaluation.as_ref().and_then(|e| e.reason.clone()))
                            .unwrap_or_else(|| row.evaluation.as_ref().map(|e| e.conditions.iter().enumerate().map(|(i,c)| {
                                let value = |v: Option<f64>| v.map_or_else(|| "—".into(), |v| format!("{v:.2}"));
                                format!("{}: {} / {}", i+1, value(c.left), value(c.right))
                            }).collect::<Vec<_>>().join("; ")).unwrap_or_default());
                        ui.label(detail);
                        ui.end_row();
                    }
                });
            });
        } else if !pane.running {
            ui.label(RichText::new("Choose conditions and run a scan. Definitions are saved with the workspace; Save scan also adds them to your library.").color(muted()));
        }
    });
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut definition = ScanDefinition { name: "First".into(), ..Default::default() };
        assert_eq!(save_definition_at(&definition, &dir).unwrap().len(), 1);
        definition.name = "Second".into();
        assert_eq!(save_definition_at(&definition, &dir).unwrap().len(), 2);
        definition.name = " first ".into();
        definition.timeframe = Timeframe::W1;
        let saved = save_definition_at(&definition, &dir).unwrap();
        assert_eq!(saved.len(), 2);
        assert!(saved.iter().any(|s| s.name == "first" && s.timeframe == Timeframe::W1));
        std::fs::write(dir.join("scans.json"), b"broken JSON").unwrap();
        assert!(save_definition_at(&definition, &dir).is_err());
        assert_eq!(std::fs::read(dir.join("scans.json")).unwrap(), b"broken JSON");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
