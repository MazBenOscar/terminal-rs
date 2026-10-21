//! Chart-local single-comparison picker with isolated, debounced provider search.
use crate::{
    theme::{self, ui_font},
    worker::{PaneId, Request, Worker},
};
use egui::{RichText, Ui};
use std::time::{Duration, Instant};
use tr_data::SearchHit;

#[derive(Default)]
pub struct Picker {
    pub open: bool,
    query: String,
    focus: bool,
    pending: Option<Instant>,
    requested: Option<String>,
    hits: Vec<SearchHit>,
    error: Option<String>,
    searching: bool,
}
pub fn parse_key(input: &str) -> Option<String> {
    let words: Vec<_> = input.split_whitespace().collect();
    let (security, used) = tr_core::Security::resolve_tokens(&words)?;
    (used == words.len()).then(|| security.key())
}
impl Picker {
    pub fn open(&mut self) {
        *self = Self { open: true, focus: true, ..Default::default() };
    }
    pub fn on_hits(&mut self, query: &str, result: Result<Vec<SearchHit>, String>) {
        if !self.open || self.query.trim() != query || self.requested.as_deref() != Some(query) {
            return;
        }
        self.searching = false;
        match result {
            Ok(hits) => {
                self.hits = hits;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        pane: PaneId,
        subject: &str,
        current: Option<&str>,
        worker: &Worker,
    ) -> Option<Option<String>> {
        if !self.open {
            return None;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.open = false;
            return None;
        }
        let mut open = true;
        let mut chosen = None;
        egui::Window::new(RichText::new("Compare symbol").font(ui_font(13.)))
            .id(egui::Id::new((pane, "compare-picker")))
            .open(&mut open)
            .default_width(theme::dp(360.))
            .constrain_to(ctx.content_rect().shrink(theme::dp(24.)))
            .show(ctx, |ui| {
                if let Some(current) = current {
                    ui.horizontal(|ui| {
                        ui.label(format!("Comparing {current}"));
                        if ui.button("Remove").clicked() {
                            chosen = Some(None);
                        }
                    });
                }
                let input = ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("Search name, ticker or market key")
                        .desired_width(f32::INFINITY)
                        .char_limit(100),
                );
                if self.focus {
                    input.request_focus();
                    self.focus = false;
                }
                if input.changed() {
                    self.pending = Some(Instant::now());
                    self.requested = None;
                    self.hits.clear();
                    self.error = None;
                    self.searching = false;
                }
                let key = parse_key(&self.query).filter(|key| key != subject);
                let enter =
                    input.has_focus() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
                if ui
                    .add_enabled(
                        key.is_some(),
                        egui::Button::new(
                            key.as_ref().map_or("Compare entered symbol".into(), |key| format!("Compare {key}")),
                        ),
                    )
                    .clicked()
                    || enter && key.is_some()
                {
                    chosen = Some(key);
                }
                if parse_key(&self.query).as_deref() == Some(subject) {
                    ui.label(RichText::new("Choose a different symbol from the chart.").color(theme::muted()));
                }
                ui.separator();
                let mut seen = std::collections::HashSet::new();
                egui::ScrollArea::vertical().max_height(theme::dp(260.)).show(ui, |ui| {
                    let local = if self.query.trim().is_empty() {
                        ["SPY", "QQQ", "DIA", "IWM", "SPX", "MSFT", "NVDA", "BTCUSD", "ETHUSD"]
                            .into_iter()
                            .filter_map(|ticker| tr_core::catalog::lookup(ticker, None))
                            .collect()
                    } else {
                        tr_core::catalog::search(self.query.trim(), 12)
                    };
                    for listing in local {
                        let key = listing.key();
                        if key != subject && seen.insert(key.clone()) {
                            result_button(ui, &key, listing.name, current, &mut chosen);
                        }
                    }
                    for hit in &self.hits {
                        if hit.key != subject && seen.insert(hit.key.clone()) {
                            result_button(ui, &hit.key, &hit.name, current, &mut chosen);
                        }
                    }
                });
                if self.searching {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Searching provider…");
                    });
                }
                if let Some(error) = &self.error {
                    ui.label(RichText::new(format!("Search unavailable: {error}")).color(theme::down()));
                }
                if !self.searching && self.requested.is_some() && seen.is_empty() && self.error.is_none() {
                    ui.label("No matches. You can enter a market key above.");
                }
                ui.label(
                    RichText::new("One comparison at a time · rebased at the visible left edge")
                        .small()
                        .color(theme::muted()),
                );
            });
        if let Some(at) = self.pending {
            if at.elapsed() >= Duration::from_millis(250) {
                self.pending = None;
                let query = self.query.trim().to_string();
                if query.len() >= 2 {
                    self.searching = true;
                    self.requested = Some(query.clone());
                    worker.send(Request::CompareSearch { pane, query });
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        }
        self.open = open && chosen.is_none();
        chosen
    }
}
fn result_button(ui: &mut Ui, key: &str, name: &str, current: Option<&str>, chosen: &mut Option<Option<String>>) {
    if ui.selectable_label(current == Some(key), format!("{key}  ·  {name}")).clicked() {
        *chosen = Some(Some(key.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn market_keys_are_fully_parsed_and_canonicalized() {
        assert_eq!(parse_key(" msft "), Some("MSFT US Equity".into()));
        assert_eq!(parse_key("BTC/USD CRNCY"), Some("BTCUSD Curncy".into()));
        assert!(parse_key("MSFT RSI").is_none());
        assert!(parse_key("").is_none());
        assert!(parse_key("5").is_none());
    }
    #[test]
    fn outdated_searches_cannot_replace_the_current_picker_state() {
        let mut picker = Picker::default();
        picker.open();
        picker.query = "MSFT".into();
        picker.requested = Some("MSFT".into());
        picker.searching = true;
        picker.on_hits("AAPL", Err("old search failed".into()));
        assert!(picker.searching);
        assert!(picker.error.is_none());
        picker.on_hits("MSFT", Err("offline".into()));
        assert!(!picker.searching);
        assert_eq!(picker.error.as_deref(), Some("offline"));
        picker.open = false;
        picker.on_hits("MSFT", Ok(Vec::new()));
        assert_eq!(picker.error.as_deref(), Some("offline"));
    }
}
