//! Watchlist snapshots: editable conditions, named definitions and explicit data status.

use egui::{RichText, Ui};
use tr_core::Timeframe;
use tr_core::scanner::{Comparison, MatchMode, ScanCondition, ScanDefinition, ScanOperand, ScanOutcome};

use crate::congress_view::Action;
use crate::theme::{amber, mono, muted};
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
        }
    }

    fn invalidate(&mut self) {
        self.rows.clear();
        self.completed = None;
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

fn operand_kind(operand: &ScanOperand) -> usize {
    match operand {
        ScanOperand::Price => 0,
        ScanOperand::Volume => 1,
        ScanOperand::ChangePercent => 2,
        ScanOperand::Sma { .. } => 3,
        ScanOperand::Rsi { .. } => 4,
        ScanOperand::Constant(_) => 5,
    }
}

fn operand_editor(ui: &mut Ui, operand: &mut ScanOperand, id: &str) {
    let mut kind = operand_kind(operand);
    let before = kind;
    egui::ComboBox::from_id_salt(id)
        .width(95.0)
        .selected_text(["Price", "Volume", "Bar change %", "SMA", "RSI", "Value"][kind])
        .show_ui(ui, |ui| {
            for (i, name) in ["Price", "Volume", "Bar change %", "SMA", "RSI", "Value"].iter().enumerate() {
                ui.selectable_value(&mut kind, i, *name);
            }
        });
    if kind != before {
        *operand = match kind {
            0 => ScanOperand::Price,
            1 => ScanOperand::Volume,
            2 => ScanOperand::ChangePercent,
            3 => ScanOperand::Sma { period: 20 },
            4 => ScanOperand::Rsi { period: 14 },
            _ => ScanOperand::Constant(0.0),
        };
    }
    match operand {
        ScanOperand::Sma { period } | ScanOperand::Rsi { period } => {
            ui.add(egui::DragValue::new(period).range(1..=500).prefix("Period "));
        }
        ScanOperand::Constant(value) => {
            ui.add(egui::DragValue::new(value).speed(0.1).max_decimals(4));
        }
        _ => {}
    }
}

pub fn show(
    ui: &mut Ui,
    pane: &mut ScannerPane,
    id: PaneId,
    symbols: &[String],
    mut send: impl FnMut(Request),
) -> Vec<Action> {
    let mut actions = Vec::new();
    egui::ScrollArea::vertical().id_salt(("scanner", id)).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("WATCHLIST SCANNER").font(mono(12.0)).color(amber()));
            ui.label(format!("{} symbols · manual snapshot", symbols.len()));
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
                        operand_editor(ui, &mut condition.left, "left");
                        egui::ComboBox::from_id_salt("comparison").width(115.0)
                            .selected_text(condition.comparison.label()).show_ui(ui, |ui| {
                            for comparison in Comparison::ALL {
                                ui.selectable_value(&mut condition.comparison, comparison, comparison.label());
                            }
                        });
                        operand_editor(ui, &mut condition.right, "right");
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
        let can_run = !pane.running && validation.is_none() && !symbols.is_empty() && symbols.len() <= 200;
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(can_run, egui::Button::new("Run scan")).clicked() {
                // Never reuse a generation after restoring a layout or recreating a pane.
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                pane.generation = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                pane.invalidate();
                pane.running = true;
                pane.snapshot_symbols = symbols.to_vec();
                pane.notice = None;
                send(Request::Scan {
                    pane: id, generation: pane.generation, definition: pane.definition.clone(), symbols: symbols.to_vec(),
                });
            }
            if ui.add_enabled(!pane.running && validation.is_none(), egui::Button::new("Save scan")).clicked() {
                match save_definition(&pane.definition) {
                    Ok(saved) => { pane.saved = saved; pane.notice = Some("Scan saved to scans.json".into()); }
                    Err(e) => pane.notice = Some(e),
                }
            }
            let mut selected = None;
            ui.add_enabled_ui(!pane.running, |ui| {
                egui::ComboBox::from_id_salt("saved-scans").selected_text("Load saved scan").show_ui(ui, |ui| {
                    if pane.saved.is_empty() { ui.label("No saved scans yet"); }
                    for (i, saved) in pane.saved.iter().enumerate() {
                        if ui.selectable_label(false, &saved.name).clicked() { selected = Some(i); }
                    }
                });
            });
            if let Some(i) = selected { pane.definition = pane.saved[i].clone(); pane.invalidate(); }
            if pane.running { ui.spinner(); ui.label(format!("Scanning {} symbols… up to 15 seconds per symbol, four at a time", pane.snapshot_symbols.len())); }
        });
        if symbols.is_empty() { ui.label("Your watchlist is empty. Add symbols with WL ADD <KEY>, then run the scan."); }
        if symbols.len() > 200 { ui.label("This version supports up to 200 watchlist symbols. Reduce the list before running."); }
        if let Some(notice) = &pane.notice { ui.label(notice); }
        if let Some(completed) = pane.completed {
            let count = |outcome| pane.rows.iter().filter(|r| r.evaluation.as_ref().is_some_and(|e| e.outcome == outcome)).count();
            let unavailable = pane.rows.iter().filter(|r| r.error.is_some() || r.evaluation.as_ref().is_none_or(|e| e.outcome == ScanOutcome::Unavailable)).count();
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{} matches · {} non-matches · {} unavailable", count(ScanOutcome::Match), count(ScanOutcome::NoMatch), unavailable));
                ui.checkbox(&mut pane.matches_only, "Matches only (always show errors)");
            });
            ui.label(RichText::new(format!("Scanned {} symbols · finished {} UTC · click a symbol to chart it", pane.snapshot_symbols.len(), completed.format("%Y-%m-%d %H:%M:%S"))).small().color(muted()));
            if pane.snapshot_symbols != symbols {
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
                        if ui.link(&row.symbol).clicked() { actions.push(Action::OpenChart(row.symbol.clone())); }
                        ui.label(match outcome { ScanOutcome::Match => "Match", ScanOutcome::NoMatch => "No match", ScanOutcome::Unavailable => "Unavailable" });
                        ui.label(if row.simulated { "Simulated" } else if row.error.is_some() { "Unavailable" } else { "Provider snapshot" });
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
                            show(ui, &mut pane, 1, &["AAPL US Equity".into()], |_| {
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
