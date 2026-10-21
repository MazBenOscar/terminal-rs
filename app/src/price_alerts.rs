//! Local price-alert storage, monitoring and controls shared by every chart/layout.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{RichText, Ui};
use tr_core::price_alerts::{Alert, Book, Direction, Event, Observation, Status};

use crate::theme::{amber, mono, muted, text};
use crate::worker::{Request, Worker};

pub const TOAST_PREFIX: &str = "\u{1e}price:";
const POLL: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub enum Action {
    Create { symbol: String, level: f64, direction: Direction },
    Edit(u64),
    Toggle(u64),
    Delete(u64),
    Manage,
}

struct Draft {
    id: Option<u64>,
    symbol: String,
    price: String,
    direction: Direction,
    simulated: bool,
    error: Option<String>,
}

pub struct State {
    pub book: Book,
    path: PathBuf,
    pub load_error: Option<String>,
    pub save_error: Option<String>,
    pub notice: Option<String>,
    dirty: bool,
    last_save: Option<Instant>,
    revision: u64,
    pending: Option<u64>,
    last_poll: Option<Instant>,
    pub simulated: bool,
    pub congress_tab: bool,
    history: bool,
    monitor: HashMap<String, String>,
    editor: Option<Draft>,
}

impl State {
    pub fn load(path: PathBuf, simulated: bool) -> Self {
        let loaded = match std::fs::read(&path) {
            Ok(bytes) if bytes.len() <= 4_194_304 => serde_json::from_slice::<Book>(&bytes)
                .map_err(|e| format!("Reading price alerts: {e}"))
                .and_then(|book| book.validate().map(|()| book)),
            Ok(_) => Err("Price-alert file exceeds 4 MiB".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Book::default()),
            Err(e) => Err(format!("Reading price alerts: {e}")),
        };
        let (book, load_error) = match loaded {
            Ok(book) => (book, None),
            Err(e) => (Book::default(), Some(e)),
        };
        Self {
            book,
            path,
            load_error,
            save_error: None,
            notice: None,
            dirty: false,
            last_save: None,
            revision: 0,
            pending: None,
            last_poll: None,
            simulated,
            congress_tab: false,
            history: false,
            monitor: HashMap::new(),
            editor: None,
        }
    }

    pub fn unread(&self) -> usize {
        self.book.events.iter().filter(|e| !e.seen).count()
    }

    pub fn save(&mut self) {
        if !self.dirty || self.load_error.is_some() {
            return;
        }
        self.last_save = Some(Instant::now());
        let result = self.book.validate().and_then(|()| {
            let bytes = serde_json::to_vec_pretty(&self.book).map_err(|e| e.to_string())?;
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            crate::layouts::write_atomic(&self.path, &bytes).map_err(|e| e.to_string())
        });
        match result {
            Ok(()) => {
                self.dirty = false;
                self.save_error = None;
            }
            Err(e) => {
                if self.save_error.as_ref() != Some(&e) {
                    self.notice = Some("Price alerts could not be saved. Open Alerts for details and retry; changes may be lost on exit.".into());
                }
                self.save_error = Some(e);
            }
        }
    }

    fn changed(&mut self, config: bool) {
        self.dirty = true;
        if config {
            self.revision = self.revision.wrapping_add(1);
            self.last_poll = None;
        }
        self.save();
    }

    pub fn periodic(&mut self, worker: &Worker) {
        if self.dirty && self.last_save.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
            self.save();
        }
        if self.load_error.is_some() || self.pending.is_some() || self.last_poll.is_some_and(|t| t.elapsed() < POLL) {
            return;
        }
        let symbols: Vec<_> = self
            .book
            .alerts
            .iter()
            .filter(|a| a.status == Status::Armed && a.simulated == self.simulated)
            .map(|a| a.symbol.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if !symbols.is_empty() {
            self.pending = Some(self.revision);
            self.last_poll = Some(Instant::now());
            worker.send(Request::PriceAlertQuotes { revision: self.revision, symbols });
        }
    }

    pub fn on_quotes(&mut self, revision: u64, observations: Vec<(String, Result<Observation, String>)>) -> Vec<Event> {
        // Configuration changes invalidate requests already in flight. Clear only that request.
        if self.pending != Some(revision) {
            return vec![];
        }
        self.pending = None;
        if revision != self.revision {
            return vec![];
        }
        self.last_poll = Some(Instant::now());
        let now = chrono::Utc::now().timestamp();
        let mut events = Vec::new();
        for (symbol, result) in observations {
            match result {
                Ok(q) if q.symbol == symbol && q.simulated == self.simulated && q.valid_at(now) => {
                    self.monitor.insert(
                        symbol.clone(),
                        format!("{} · data {}", display_price(&symbol, q.price), timestamp(q.ts)),
                    );
                    events.extend(self.book.observe(q, now));
                }
                result => {
                    let error = match result {
                        Err(e) => format!("Unavailable · {e}"),
                        Ok(q) if q.simulated != self.simulated => "Waiting · data mode differs from alert".into(),
                        Ok(_) => "Waiting · stale or invalid price data".into(),
                    };
                    self.monitor.insert(symbol.clone(), error);
                    self.book.reset_symbol(&symbol);
                }
            }
        }
        if !events.is_empty() {
            self.changed(false);
        }
        events
    }

    pub fn action(&mut self, action: Action) {
        if self.load_error.is_some() {
            return;
        }
        match action {
            Action::Create { symbol, level, direction } => {
                self.editor = Some(Draft {
                    id: None,
                    symbol,
                    price: level.to_string(),
                    direction,
                    simulated: self.simulated,
                    error: None,
                })
            }
            Action::Edit(id) => {
                if let Some(a) = self.book.alerts.iter().find(|a| a.id == id) {
                    self.editor = Some(Draft {
                        id: Some(id),
                        symbol: a.symbol.clone(),
                        price: a.level.to_string(),
                        direction: a.direction,
                        simulated: a.simulated,
                        error: None,
                    });
                }
            }
            Action::Toggle(id) => {
                self.book.toggle(id);
                self.changed(true);
            }
            Action::Delete(id) => {
                self.book.delete(id);
                self.changed(true);
            }
            Action::Manage => {
                self.congress_tab = false;
                self.history = false;
            }
        }
    }

    /// The Alerts pane keeps Congress filings intact on a neighboring tab.
    pub fn show(&mut self, ui: &mut Ui) {
        if let Some(error) = &self.load_error {
            ui.label(RichText::new(error).color(crate::theme::down()));
            ui.label("The original file was preserved. Close the app, repair or move price-alerts.json, then restart.");
            return;
        }
        ui.horizontal_wrapped(|ui| {
            let unread = self.unread();
            if ui.button("New price alert…").clicked() {
                self.editor = Some(Draft {
                    id: None,
                    symbol: String::new(),
                    price: String::new(),
                    direction: Direction::Above,
                    simulated: self.simulated,
                    error: None,
                });
            }
            ui.selectable_value(&mut self.history, false, "Levels");
            ui.selectable_value(&mut self.history, true, format!("Triggered ({unread})"));
            if self.history && ui.button("Mark read").clicked() {
                for event in &mut self.book.events {
                    event.seen = true;
                }
                self.changed(false);
            }
        });
        ui.label(
            RichText::new(if self.simulated {
                "Sample mode · simulated alerts only · app must stay running"
            } else {
                "Polls about every 15 seconds while app runs · prices may be delayed"
            })
            .color(muted()),
        );
        ui.label(RichText::new("One shot per crossing; the first valid observation sets the baseline.").color(muted()));
        if let Some(error) = &self.save_error {
            ui.label(
                RichText::new(format!("Not saved: {error}. Retrying; changes may be lost on exit."))
                    .color(crate::theme::down()),
            );
            if ui.button("Retry save").clicked() {
                self.save();
            }
        }
        ui.separator();
        let mut action = None;
        egui::ScrollArea::vertical().id_salt("price-alert-list").auto_shrink([false, false]).show(ui, |ui| {
            if self.history {
                if self.book.events.is_empty() {
                    ui.label("No price alerts have triggered.");
                }
                for event in self.book.events.iter().rev() {
                    ui.label(RichText::new(event_message(event)).color(if event.seen { muted() } else { amber() }));
                    ui.label(
                        RichText::new(format!(
                            "Observed {} · price data {} · {}",
                            timestamp(event.observed_at),
                            timestamp(event.quote_ts),
                            event.source
                        ))
                        .small()
                        .color(muted()),
                    );
                    ui.separator();
                }
            } else {
                if self.book.alerts.is_empty() {
                    ui.label("No price alerts yet.");
                    ui.label(
                        RichText::new("Right-click a chart price level to create one, or choose New price alert.")
                            .color(muted()),
                    );
                }
                for a in &self.book.alerts {
                    ui.push_id(a.id, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new(format!("{} · {} {}", a.symbol, a.direction.label(), a.level))
                                    .font(mono(11.5))
                                    .color(text()),
                            );
                            ui.label(RichText::new(status(a)).color(if a.status == Status::Armed {
                                amber()
                            } else {
                                muted()
                            }));
                        });
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Edit…").clicked() {
                                action = Some(Action::Edit(a.id));
                            }
                            if ui.button(if a.status == Status::Armed { "Disable" } else { "Rearm" }).clicked() {
                                action = Some(Action::Toggle(a.id));
                            }
                            if ui.button("Delete").clicked() {
                                action = Some(Action::Delete(a.id));
                            }
                            if a.simulated != self.simulated {
                                ui.label(RichText::new("Paused · different data mode").color(muted()));
                            } else if a.status == Status::Armed {
                                ui.label(
                                    RichText::new(
                                        self.monitor
                                            .get(&a.symbol)
                                            .map(String::as_str)
                                            .unwrap_or("Waiting for price data…"),
                                    )
                                    .small()
                                    .color(muted()),
                                );
                            }
                        });
                        ui.separator();
                    });
                }
            }
        });
        if let Some(action) = action {
            self.action(action);
        }
    }

    pub fn editor(&mut self, ctx: &egui::Context) {
        let Some(draft) = &mut self.editor else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        egui::Window::new(if draft.id.is_some() { "Edit price alert" } else { "Create price alert" })
            .id(egui::Id::new("price-alert-editor")).open(&mut open).collapsible(false).resizable(false)
            .default_width(340.).show(ctx, |ui| {
                ui.label("Symbol");
                ui.add_enabled(draft.id.is_none(), egui::TextEdit::singleline(&mut draft.symbol).hint_text("AAPL or BTCUSD Curncy").char_limit(100).desired_width(f32::INFINITY));
                ui.add_space(6.);
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("price-alert-direction").selected_text(draft.direction.label()).show_ui(ui, |ui| {
                        ui.selectable_value(&mut draft.direction, Direction::Above, Direction::Above.label());
                        ui.selectable_value(&mut draft.direction, Direction::Below, Direction::Below.label());
                    });
                    ui.add(egui::TextEdit::singleline(&mut draft.price).hint_text("Price").char_limit(30).desired_width(135.));
                });
                ui.add_space(8.);
                ui.label(RichText::new("Fires once, then stops. Saving rearms the alert.").color(muted()));
                ui.label(RichText::new("Polls about every 15 seconds while the app runs; slow providers delay checks. Crossings between checks can be missed.").small().color(muted()));
                if draft.simulated { ui.label(RichText::new("Sample alert · simulated data only").color(amber())); }
                if let Some(error) = &draft.error { ui.label(RichText::new(error).color(crate::theme::down())); }
                ui.horizontal(|ui| {
                    save = ui.button(if draft.id.is_some() { "Save and rearm" } else { "Create alert" }).clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if save {
            let result = (|| {
                let level: f64 = draft.price.trim().parse().map_err(|_| "Enter a valid price".to_string())?;
                if let Some(id) = draft.id {
                    self.book.edit(id, level, draft.direction)
                } else {
                    let symbol = crate::chart_compare::parse_key(&draft.symbol).ok_or("Choose a valid symbol")?;
                    self.book.add(symbol, level, draft.direction, draft.simulated).map(|_| ())
                }
            })();
            match result {
                Ok(()) => {
                    self.editor = None;
                    self.changed(true);
                }
                Err(e) => draft.error = Some(e),
            }
        } else if cancel || !open {
            self.editor = None;
        }
    }
}

pub fn status(a: &Alert) -> &'static str {
    match (a.status, a.simulated) {
        (Status::Armed, false) => "Armed",
        (Status::Armed, true) => "Sample · armed",
        (Status::Disabled, _) => "Disabled",
        (Status::Triggered, _) => "Triggered",
    }
}

pub fn event_message(e: &Event) -> String {
    format!(
        "{}{} {} {} · observed {}",
        if e.alert.simulated { "Sample · " } else { "" },
        e.alert.symbol,
        e.alert.direction.label(),
        e.alert.level,
        display_price(&e.alert.symbol, e.price)
    )
}

fn display_price(symbol: &str, price: f64) -> String {
    let sec = tr_data::security(symbol);
    tr_chart::scale::format_price(price, sec.decimals(price))
}

fn timestamp(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%b %d %H:%M:%S %Z").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        let dir = std::env::temp_dir().join(format!(
            "price-alerts-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        State::load(dir.join("price-alerts.json"), false)
    }
    fn q(price: f64) -> Observation {
        Observation {
            symbol: "AAPL US Equity".into(),
            price,
            ts: chrono::Utc::now().timestamp(),
            source: "test".into(),
            simulated: false,
        }
    }
    fn reply(state: &mut State, price: f64) -> Vec<Event> {
        state.pending = Some(state.revision);
        state.on_quotes(state.revision, vec![("AAPL US Equity".into(), Ok(q(price)))])
    }

    #[test]
    fn triggers_persist_with_unread_history_and_restart_never_reuses_a_baseline() {
        let mut s = state();
        s.book.add("AAPL US Equity".into(), 100., Direction::Above, false).unwrap();
        s.changed(true);
        assert!(reply(&mut s, 99.).is_empty());
        let events = reply(&mut s, 101.);
        assert_eq!(events.len(), 1);
        assert_eq!(s.unread(), 1);
        assert!(event_message(&events[0]).contains("Crosses above 100"));
        let mut restored = State::load(s.path.clone(), false);
        assert!(restored.load_error.is_none());
        assert_eq!(restored.book.alerts[0].status, Status::Triggered);
        assert_eq!(restored.unread(), 1);
        restored.action(Action::Toggle(1));
        assert!(reply(&mut restored, 101.).is_empty());
        restored.book.events[0].seen = true;
        restored.changed(false);
        assert_eq!(State::load(s.path.clone(), false).unread(), 0);
        std::fs::remove_dir_all(s.path.parent().unwrap()).unwrap();
    }

    #[test]
    fn edited_configuration_ignores_inflight_results_and_errors_reset_monitoring() {
        let mut s = state();
        s.book.add("AAPL US Equity".into(), 100., Direction::Above, false).unwrap();
        reply(&mut s, 99.);
        let old = s.revision;
        s.pending = Some(old);
        s.book.edit(1, 105., Direction::Above).unwrap();
        s.changed(true);
        assert!(s.on_quotes(old, vec![("AAPL US Equity".into(), Ok(q(110.)))]).is_empty());
        assert!(s.pending.is_none());
        reply(&mut s, 99.);
        s.pending = Some(s.revision);
        s.on_quotes(s.revision, vec![("AAPL US Equity".into(), Err("offline".into()))]);
        assert!(s.monitor["AAPL US Equity"].contains("offline"));
        assert!(reply(&mut s, 110.).is_empty());
        std::fs::remove_dir_all(s.path.parent().unwrap()).unwrap();
    }

    #[test]
    fn corrupt_or_future_version_file_is_preserved_and_never_overwritten() {
        for bytes in [b"broken".as_slice(), br#"{"version":999,"alerts":[],"events":[],"next_id":1}"#.as_slice()] {
            let mut s = state();
            std::fs::create_dir_all(s.path.parent().unwrap()).unwrap();
            std::fs::write(&s.path, bytes).unwrap();
            s = State::load(s.path.clone(), false);
            assert!(s.load_error.is_some());
            s.action(Action::Create { symbol: "AAPL US Equity".into(), level: 100., direction: Direction::Above });
            assert!(s.editor.is_none());
            s.dirty = true;
            s.save();
            assert_eq!(std::fs::read(&s.path).unwrap(), bytes);
            std::fs::remove_dir_all(s.path.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn failed_saves_are_visible_and_can_be_retried() {
        let mut s = state();
        let path = s.path.clone();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let blocker = path.with_extension("blocker");
        std::fs::write(&blocker, b"occupied").unwrap();
        s.path = blocker.join("price-alerts.json");
        s.book.add("AAPL US Equity".into(), 100., Direction::Above, false).unwrap();
        s.changed(true);
        assert!(s.save_error.is_some() && s.dirty);
        s.path = path;
        s.save();
        assert!(s.save_error.is_none() && !s.dirty);
        assert_eq!(State::load(s.path.clone(), false).book.alerts.len(), 1);
        std::fs::remove_dir_all(s.path.parent().unwrap()).unwrap();
    }

    #[test]
    fn wrong_mode_stale_or_mismatched_prices_cannot_trigger() {
        let mut s = state();
        s.book.add("AAPL US Equity".into(), 100., Direction::Above, false).unwrap();
        for variant in 0..3 {
            reply(&mut s, 99.);
            let mut observation = q(101.);
            match variant {
                0 => observation.simulated = true,
                1 => observation.ts -= 86_400,
                _ => observation.symbol = "MSFT US Equity".into(),
            }
            s.pending = Some(s.revision);
            assert!(s.on_quotes(s.revision, vec![("AAPL US Equity".into(), Ok(observation))]).is_empty());
            assert!(reply(&mut s, 101.).is_empty());
        }
        assert!(s.book.events.is_empty());
    }

    fn ui_frame(ctx: &egui::Context, s: &mut State, events: Vec<egui::Event>) -> Vec<(String, egui::Rect)> {
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(700., 500.))),
                time: Some(ctx.input(|i| i.time) + 0.2),
                events,
                ..Default::default()
            },
            |ui| {
                s.show(ui);
                s.editor(ui.ctx());
            },
        );
        let text = out
            .shapes
            .iter()
            .filter_map(|shape| {
                if let egui::Shape::Text(t) = &shape.shape {
                    Some((t.galley.job.text.clone(), t.galley.rect.translate(t.pos.to_vec2())))
                } else {
                    None
                }
            })
            .collect();
        out.textures_delta.clear();
        text
    }

    fn click(ctx: &egui::Context, s: &mut State, label: &str) {
        ui_frame(ctx, s, vec![]);
        let labels = ui_frame(ctx, s, vec![]);
        let at = labels
            .iter()
            .find(|(text, _)| text == label)
            .unwrap_or_else(|| panic!("Missing {label}: {labels:?}"))
            .1
            .center();
        ui_frame(ctx, s, vec![egui::Event::PointerMoved(at)]);
        for pressed in [true, false] {
            ui_frame(
                ctx,
                s,
                vec![egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
    }

    #[test]
    fn alert_editor_and_list_controls_create_validate_edit_disable_and_delete() {
        let ctx = egui::Context::default();
        let mut s = state();
        click(&ctx, &mut s, "New price alert…");
        s.editor.as_mut().unwrap().symbol = "AAPL".into();
        s.editor.as_mut().unwrap().price = "not a price".into();
        click(&ctx, &mut s, "Create alert");
        assert!(s.book.alerts.is_empty());
        assert!(s.editor.as_ref().unwrap().error.is_some());
        s.editor.as_mut().unwrap().price = "100.125".into();
        click(&ctx, &mut s, "Create alert");
        assert!(s.editor.is_none());
        assert_eq!(s.book.alerts[0].symbol, "AAPL US Equity");
        assert_eq!(s.book.alerts[0].level, 100.125);
        click(&ctx, &mut s, "Edit…");
        s.editor.as_mut().unwrap().price = "99.5".into();
        s.editor.as_mut().unwrap().direction = Direction::Below;
        click(&ctx, &mut s, "Save and rearm");
        assert_eq!(s.book.alerts[0].level, 99.5);
        assert_eq!(s.book.alerts[0].direction, Direction::Below);
        click(&ctx, &mut s, "Disable");
        assert_eq!(s.book.alerts[0].status, Status::Disabled);
        click(&ctx, &mut s, "Rearm");
        assert_eq!(s.book.alerts[0].status, Status::Armed);
        click(&ctx, &mut s, "Delete");
        assert!(s.book.alerts.is_empty());
        std::fs::remove_dir_all(s.path.parent().unwrap()).unwrap();
    }
}
