//! Watchlist and market-wide snapshots: editable conditions, named definitions and explicit
//! data status.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use egui::{Align2, Color32, RichText, Sense, Ui, pos2};
use tr_core::Timeframe;
use tr_core::scanner::{
    Comparison, MAX_BARS_AGO, MatchMode, ScanCondition, ScanDefinition, ScanOperand, ScanOutcome, Source,
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

/// A muted label and its control, kept together when the row wraps.
fn field(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(muted()));
        add(ui);
    });
    ui.add_space(dp(8.0));
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
    egui::ScrollArea::vertical().id_salt(("scanner", id)).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            let (title, subtitle) = if market {
                ("MARKET SCANNER", format!("{} · manual snapshot", pane.universe.label()))
            } else {
                ("WATCHLIST SCANNER", format!("{} symbols · manual snapshot", symbols.len()))
            };
            ui.label(RichText::new(title).font(mono(12.0)).color(amber()));
            ui.label(RichText::new(subtitle).font(mono(12.0)).color(muted()));
        });
        ui.label(
            RichText::new("Uses the latest provider bar, which may still be forming. No automatic refresh.")
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
            for (i, item) in pane.definition.root.items.iter_mut().enumerate() {
                let tr_core::scanner::ConditionItem::Compare(condition) = item else {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(item.label()).color(muted()));
                        if ui.button("Remove").clicked() {
                            remove = Some(i);
                        }
                    });
                    continue;
                };
                ui.push_id(i, |ui| {
                    egui::Frame::new()
                        .fill(theme::header())
                        .corner_radius(theme::CONTROL_RADIUS)
                        .inner_margin(egui::Margin::symmetric(6, 4))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            let close_w = dp(26.0);
                            let full = ui.available_width();
                            let top = ui.cursor().min;
                            ui.scope(|ui| {
                                ui.set_max_width((full - close_w).max(0.0));
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(RichText::new(format!("{}", i + 1)).font(mono(12.0)).color(muted()));
                                    operand_editor(ui, &mut condition.left, &mut condition.left_bars_ago, "left");
                                    egui::ComboBox::from_id_salt("comparison")
                                        .width(dp(110.0))
                                        .selected_text(condition.comparison.label())
                                        .show_ui(ui, |ui| {
                                            for comparison in Comparison::ALL {
                                                ui.selectable_value(
                                                    &mut condition.comparison,
                                                    comparison,
                                                    comparison.label(),
                                                );
                                            }
                                        });
                                    operand_editor(ui, &mut condition.right, &mut condition.right_bars_ago, "right");
                                    // Scaling a constant is just a different constant.
                                    if matches!(condition.right, ScanOperand::Constant(_)) {
                                        condition.right_multiplier = 1.0;
                                    } else {
                                        ui.add(
                                            egui::DragValue::new(&mut condition.right_multiplier)
                                                .speed(0.05)
                                                .max_decimals(3)
                                                .prefix("× "),
                                        )
                                        .on_hover_text(
                                            "Multiplies the right side, e.g. Volume >= EMA(50, Volume) × 2.5",
                                        );
                                    }
                                });
                            });
                            let close = egui::Rect::from_min_size(
                                pos2(top.x + full - close_w + dp(4.0), top.y),
                                egui::vec2(dp(22.0), dp(20.0)),
                            );
                            let remove_button =
                                |ui: &mut Ui| widgets::icon_button(ui, Icon::Close, "Remove condition", false);
                            if ui.put(close, remove_button).clicked() {
                                remove = Some(i);
                            }
                        });
                    ui.add_space(dp(2.0));
                });
            }
            if let Some(i) = remove {
                pane.definition.root.items.remove(i);
            }
            if pane.definition.root.items.is_empty() {
                ui.label(RichText::new("No conditions yet. Add one to describe what a match looks like.").color(muted()));
            }
            if pane.definition.leaves().len() < 32 && ui.button("Add condition").clicked() {
                pane.definition.root.items.push(tr_core::scanner::ConditionItem::Compare(ScanCondition::default()));
            }
        });
        if pane.definition != before {
            pane.invalidate();
        }
        let validation = pane.definition.validate().err();
        if let Some(error) = &validation {
            ui.label(RichText::new(error).color(theme::down()));
        }
        ui.add_space(dp(4.0));
        ui.separator();
        let can_run = !pane.running
            && validation.is_none()
            && if market { bulk_ready } else { !symbols.is_empty() && symbols.len() <= 200 };
        ui.horizontal_wrapped(|ui| {
            let run = egui::Button::new(RichText::new("Run scan").color(Color32::WHITE))
                .fill(theme::accent())
                .min_size(egui::vec2(dp(84.0), 0.0));
            if ui.add_enabled(can_run, run).clicked() {
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
                    });
                }
            }
            if ui.add_enabled(!pane.running && validation.is_none(), egui::Button::new("Save scan")).clicked() {
                match save_definition(&ScanSettings { definition: pane.definition.clone(), universe: pane.universe }) {
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
                            selected = Some(ScanSettings { definition: preset, universe: pane.universe });
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
                if ui.add_enabled(!stopping, egui::Button::new(if stopping { "Stopping…" } else { "Stop" })).clicked()
                    && let Some(cancel) = &pane.cancel
                {
                    cancel.store(true, Ordering::Relaxed);
                }
            });
            ui.label(RichText::new("Fresh snapshots are reused. Stale data, changed sources and larger lookbacks download full history; a market-wide refresh can take 10–15 minutes.").small().color(muted()));
        }
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
        if let Some(completed) = pane.completed {
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
        let mut definition = ScanSettings {
            definition: ScanDefinition { name: "First".into(), ..Default::default() },
            universe: Universe::Nasdaq,
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
