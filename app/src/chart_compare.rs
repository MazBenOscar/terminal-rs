//! Chart-local single-comparison picker with isolated, debounced provider search.
use crate::{
    theme::{self, ui_font},
    worker::{PaneId, Request, Worker},
};
use egui::{RichText, Ui};
use std::time::{Duration, Instant};
use tr_data::SearchHit;

/// How a comparison is scaled against the chart's own symbol.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Axis {
    /// Both series rebased at the visible left edge; the axis reads % change.
    #[default]
    Percent,
    /// Rebased onto the main symbol's price axis.
    Price,
    /// The comparison in its own prices on a separate left-hand axis.
    Own,
    Indexed,
}

impl Axis {
    pub const ALL: [Self; 4] = [Self::Percent, Self::Indexed, Self::Price, Self::Own];

    pub fn label(self) -> &'static str {
        match self {
            Self::Percent => "% change",
            Self::Price => "Price axis",
            Self::Own => "Own axis",
            Self::Indexed => "Index 100",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Percent => "Both symbols rebased at the visible left edge; the axis shows % change",
            Self::Price => "The comparison rebased onto this chart's price axis",
            Self::Own => "The comparison in its own prices on a separate left axis",
            Self::Indexed => "All series indexed to 100 at the first common visible bar",
        }
    }
}

#[derive(Default)]
pub struct Series {
    pub line: Option<tr_chart::gpu::LineRef>,
    pub error: Option<String>,
    pub loading: bool,
    pub factor: f64,
}

/// Preserve identities across promotion and migrate old layouts in comparison order.
pub fn sync_slots(slots: &mut Vec<(String, u8)>, symbols: &[String]) {
    let mut used = [false; 8];
    slots.retain(|(key, slot)| {
        if !symbols.contains(key) || *slot >= 8 || used[*slot as usize] {
            return false;
        }
        used[*slot as usize] = true;
        true
    });
    for key in symbols {
        if !slots.iter().any(|(existing, _)| existing == key)
            && let Some(slot) = used.iter().position(|used| !used)
        {
            slots.push((key.clone(), slot as u8));
            used[slot] = true;
        }
    }
}

pub fn slot(slots: &[(String, u8)], symbol: &str) -> usize {
    slots.iter().find(|(key, _)| key == symbol).map_or(0, |(_, slot)| *slot as usize)
}

pub fn common_anchor(
    main: &tr_chart::gpu::LineRef,
    others: &[tr_chart::gpu::LineRef],
    lo: usize,
    hi: usize,
) -> Option<usize> {
    if others.is_empty() {
        return None;
    }
    let valid = |line: &tr_chart::gpu::LineRef, i: usize| {
        let pyramid = line.pyramid.read().expect("pyramid lock");
        i < pyramid.level_len(0) && pyramid.data.get(i).is_some_and(|p| p[1].is_finite() && p[1] > 0.)
    };
    let len = main.pyramid.read().expect("pyramid lock").level_len(0);
    (lo..hi.min(len)).find(|&i| valid(main, i) && others.iter().all(|line| valid(line, i)))
}

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
    /// Anchor the window under the chart's Compare button on the first frame after opening.
    place: bool,
    /// The chart's rect, set before [`Picker::show`] each frame.
    pub chart: Option<egui::Rect>,
}
pub fn parse_key(input: &str) -> Option<String> {
    let words: Vec<_> = input.split_whitespace().collect();
    let (security, used) = tr_core::Security::resolve_tokens(&words)?;
    (used == words.len()).then(|| security.key())
}
impl Picker {
    pub fn open(&mut self) {
        *self = Self { open: true, focus: true, place: true, ..Default::default() };
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
        controls: (&mut Axis, &mut Vec<String>, &mut Vec<String>),
        worker: &Worker,
    ) -> Option<Option<String>> {
        let (axis, extra, hidden) = controls;
        if !self.open {
            return None;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.open = false;
            return None;
        }
        let mut open = true;
        let mut chosen = None;
        let width = theme::dp(360.);
        let mut window = egui::Window::new(RichText::new("Compare symbol").font(ui_font(13.)))
            .id(egui::Id::new((pane, "compare-picker")))
            .open(&mut open)
            .default_width(width)
            .constrain_to(ctx.content_rect().shrink(theme::dp(24.)));
        let chart = self.chart.unwrap_or_else(|| ctx.content_rect());
        if std::mem::take(&mut self.place) {
            // Top-right of the plot, under the header's Compare button.
            let x = (chart.right() - width - theme::dp(12.)).max(chart.left() + theme::dp(6.));
            window = window.current_pos(egui::pos2(x, chart.top() + theme::dp(52.)));
        }
        window.show(ctx, |ui| {
            if let Some(current) = current {
                ui.horizontal(|ui| {
                    ui.label(format!("Comparing {current}"));
                    let mut visible = !hidden.iter().any(|s| s == current);
                    if ui.checkbox(&mut visible, "Show").changed() {
                        hidden.retain(|s| s != current);
                        if !visible {
                            hidden.push(current.to_string());
                        }
                    }
                    if ui.button("Remove").clicked() {
                        chosen = Some(None);
                    }
                });
            }
            let mut remove = None;
            for (index, key) in extra.iter().enumerate() {
                ui.push_id(key, |ui| {
                    ui.horizontal(|ui| {
                        let mut visible = !hidden.contains(key);
                        if ui.checkbox(&mut visible, key).changed() {
                            hidden.retain(|s| s != key);
                            if !visible {
                                hidden.push(key.clone());
                            }
                        }
                        if ui.button("Remove").clicked() {
                            remove = Some(index);
                        }
                    })
                });
            }
            if let Some(index) = remove {
                let key = extra.remove(index);
                hidden.retain(|s| s != &key);
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new("Scale").color(theme::muted()));
                for option in Axis::ALL {
                    ui.add_enabled_ui(option != Axis::Own || extra.is_empty(), |ui| {
                        ui.selectable_value(axis, option, option.label()).on_hover_text(option.hint())
                    });
                }
            });
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
            let enter = input.has_focus() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            let mut results: Vec<(String, String)> = Vec::new();
            let local = if self.query.trim().is_empty() {
                ["SPY", "QQQ", "DIA", "IWM", "SPX", "MSFT", "NVDA", "BTCUSD", "ETHUSD"]
                    .into_iter()
                    .filter_map(|ticker| tr_core::catalog::lookup(ticker, None))
                    .collect()
            } else {
                tr_core::catalog::search(self.query.trim(), 12)
            };
            let found = local
                .into_iter()
                .map(|listing| (listing.key(), listing.name.to_string()))
                .chain(self.hits.iter().map(|hit| (hit.key.clone(), hit.name.clone())));
            for (key, name) in found {
                if key != subject && !results.iter().any(|(k, _)| *k == key) {
                    results.push((key, name));
                }
            }
            // A typed key is offered only when no result already is that key, after the results,
            // so a plain word such as "tesla" is not presented as the ticker TESLA.
            let typed = parse_key(&self.query).filter(|key| key != subject && !results.iter().any(|(k, _)| k == key));
            if enter {
                chosen = results.first().map(|(key, _)| Some(key.clone())).or_else(|| typed.clone().map(Some));
            }
            if parse_key(&self.query).as_deref() == Some(subject) {
                ui.label(RichText::new("Choose a different symbol from the chart.").color(theme::muted()));
            }
            ui.separator();
            if extra.len() >= 7 {
                ui.label("Eight comparisons selected. Remove one before adding another.");
            }
            egui::ScrollArea::vertical().max_height(theme::dp(260.)).show(ui, |ui| {
                for (key, name) in &results {
                    result_button(ui, key, name, current, &mut chosen);
                }
                if let Some(key) = &typed {
                    let label = RichText::new(format!("Use “{key}” as entered")).color(theme::muted());
                    if ui.add(egui::Button::selectable(false, label).truncate()).clicked() {
                        chosen = Some(Some(key.clone()));
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
            if !self.searching && self.requested.is_some() && results.is_empty() && self.error.is_none() {
                ui.label(
                    RichText::new("No matches. Enter a full market key, e.g. TSLA US Equity.").color(theme::muted()),
                );
            }
            ui.label(
                RichText::new(format!("Up to 8 comparisons · {}", axis.hint().to_lowercase()))
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
    let text = format!("{key}  ·  {name}");
    if ui.add(egui::Button::selectable(current == Some(key), text).truncate()).on_hover_text(name).clicked() {
        *chosen = Some(Some(key.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slots_survive_promotion_and_reuse_the_lowest_free_slot() {
        let mut slots = Vec::new();
        sync_slots(&mut slots, &["A".into(), "B".into()]);
        assert_eq!(slots, vec![("A".into(), 0), ("B".into(), 1)]);
        sync_slots(&mut slots, &["B".into()]);
        assert_eq!(slot(&slots, "B"), 1);
        sync_slots(&mut slots, &["B".into(), "C".into()]);
        assert_eq!(slot(&slots, "B"), 1);
        assert_eq!(slot(&slots, "C"), 0);
    }

    #[test]
    fn no_comparisons_have_no_anchor() {
        let main = tr_chart::scene::line_ref(&[100., 101., 102.]);
        assert_eq!(common_anchor(&main, &[], 0, 3), None);
    }

    #[test]
    fn all_visible_series_share_one_anchor_and_missing_overlap_stays_unavailable() {
        let line = tr_chart::scene::line_ref;
        let main = line(&[100., 101., 102., 103.]);
        let early = line(&[50., 51., 52., 53.]);
        let late = line(&[f64::NAN, f64::NAN, 10., 11.]);
        assert_eq!(common_anchor(&main, &[early.clone(), late], 0, 4), Some(2));
        assert_eq!(common_anchor(&main, &[early], 1, 4), Some(1));
        assert_eq!(common_anchor(&main, &[line(&[f64::NAN; 4])], 0, 4), None);
    }
    #[test]
    fn compare_axis_defaults_to_percent_and_round_trips() {
        assert_eq!(Axis::default(), Axis::Percent);
        for axis in Axis::ALL {
            let json = serde_json::to_string(&axis).unwrap();
            assert_eq!(serde_json::from_str::<Axis>(&json).unwrap(), axis);
        }
    }
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
