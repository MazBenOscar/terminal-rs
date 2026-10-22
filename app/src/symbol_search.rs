//! Concrete instrument discovery shared by watchlists and chart search.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use tr_core::AssetClass;
use tr_core::crypto::{CryptoInstrument, CryptoVenue};
use tr_data::SearchHit;

use crate::{theme, watch_sections};

const LIMIT: usize = 500;

pub fn same_key(a: &str, b: &str) -> bool {
    fn canonical(key: &str) -> String {
        tr_data::security(key).crypto_instrument().map_or_else(|_| key.into(), |c| c.security().key())
    }
    canonical(a) == canonical(b)
}
const CATEGORIES: [(&str, Option<AssetClass>); 8] = [
    ("All", None),
    ("Stocks", Some(AssetClass::Equity)),
    ("Funds", Some(AssetClass::Etf)),
    ("Futures", Some(AssetClass::Cmdty)),
    ("Forex", Some(AssetClass::Fx)),
    ("Crypto", Some(AssetClass::Crypto)),
    ("Indices", Some(AssetClass::Index)),
    ("Bonds", Some(AssetClass::Govt)),
];

#[derive(Clone)]
pub enum Target {
    Append,
    After(String),
    Section { position: usize, name: String },
}

impl Target {
    pub fn rebase(&mut self, old: &[watch_sections::Item], new: &[watch_sections::Item]) {
        use watch_sections::Item;
        if old == new {
            return;
        }
        let Self::Section { position, name } = self else {
            return;
        };
        let same = |item: &Item| matches!(item, Item::Section { name: n, .. } if n == name);
        let candidates: Vec<_> = new.iter().enumerate().filter(|(_, item)| same(item)).map(|(i, _)| i).collect();
        let index = if old.iter().filter(|item| same(item)).count() == 1 && candidates.len() == 1 {
            candidates.first().copied()
        } else if old.get(*position).is_some_and(same) {
            let original = &old[watch_sections::block(old, *position)];
            let matches: Vec<_> =
                candidates.into_iter().filter(|i| &new[watch_sections::block(new, *i)] == original).collect();
            if matches.len() == 1 { matches.first().copied() } else { None }
        } else {
            None
        };
        if let Some(index) = index {
            *position = index;
        } else {
            *self = Self::Append;
        }
    }

    pub fn insert(&mut self, items: &mut Vec<watch_sections::Item>, key: &str) -> bool {
        use watch_sections::Item;
        if items.iter().filter(|i| matches!(i, Item::Symbol(_))).count() >= 200
            || items.iter().any(|i| matches!(i, Item::Symbol(s) if same_key(s, key)))
        {
            return false;
        }
        let gap = match self {
            Self::Append => items.len(),
            Self::After(key) => items
                .iter()
                .position(|i| matches!(i, Item::Symbol(s) if same_key(s, key)))
                .map_or(items.len(), |i| i + 1),
            Self::Section { position, name } => {
                let same = |item: &Item| matches!(item, Item::Section { name: n, .. } if n == name);
                let index =
                    if items.get(*position).is_some_and(same) { Some(*position) } else { items.iter().position(same) };
                index.map_or(items.len(), |i| watch_sections::block(items, i).end)
            }
        };
        items.insert(gap, Item::Symbol(key.into()));
        *self = Self::After(key.into());
        true
    }
}

pub struct Dialog {
    pub target: Option<Target>,
    pub query: String,
    category: Option<AssetClass>,
    source: String,
    kind: String,
    network: Vec<SearchHit>,
    crypto: [Option<Result<Vec<CryptoInstrument>, String>>; 2],
    requested: bool,
    pending: Option<Instant>,
    autofocus: bool,
    selected: usize,
}

pub struct Outcome {
    pub key: Option<String>,
    pub close: bool,
}

fn venue_index(venue: CryptoVenue) -> usize {
    match venue {
        CryptoVenue::Binance => 0,
        CryptoVenue::Coinbase => 1,
    }
}

fn source(hit: &SearchHit) -> String {
    if hit.exchange.as_deref() == Some("Built-in") {
        return "Built-in".into();
    }
    if let Ok(c) = tr_data::security(&hit.key).crypto_instrument() {
        return match c.venue {
            CryptoVenue::Binance => "Binance",
            CryptoVenue::Coinbase => "Coinbase",
        }
        .into();
    }
    hit.exchange.clone().unwrap_or_else(|| "Built-in".into())
}

fn kind(class: AssetClass) -> &'static str {
    match class {
        AssetClass::Equity => "stock",
        AssetClass::Etf => "fund",
        AssetClass::Index => "index",
        AssetClass::Fx => "forex",
        AssetClass::Crypto => "spot crypto",
        AssetClass::Govt => "bond",
        AssetClass::Cmdty => "futures",
    }
}

/// Quote currencies people usually mean, most likely first; other quotes follow.
const MAJOR_QUOTES: [&str; 5] = ["USD", "USDT", "USDC", "EUR", "BTC"];

fn rank(hit: &SearchHit, query: &str) -> Option<(u8, u8, u8, usize)> {
    let sec = tr_data::security(&hit.key);
    let ticker = sec.ticker.to_uppercase();
    let crypto = sec.crypto_instrument().ok();
    let native = crypto.as_ref().map_or(ticker.as_str(), |c| c.native_symbol.as_str());
    let name = hit.name.to_uppercase();
    let rank = if query.is_empty() || ticker == query || native == query {
        0
    } else if native.starts_with(query) {
        1
    } else if crypto.as_ref().is_some_and(|c| c.base == query) {
        2
    } else if name.split_whitespace().any(|w| w.starts_with(query)) {
        3
    } else if ticker.contains(query) || name.contains(query) {
        4
    } else {
        return None;
    };
    // Within a rank, XRP/USD and XRP/USDT come before XRP/BRL or XRP/TRY.
    let quote = crypto
        .as_ref()
        .map_or(0, |c| MAJOR_QUOTES.iter().position(|q| *q == c.quote).unwrap_or(MAJOR_QUOTES.len()) as u8);
    Some((rank, u8::from(!matches!(hit.class, AssetClass::Equity | AssetClass::Crypto)), quote, native.len()))
}

impl Dialog {
    pub fn new(query: String, target: Option<Target>) -> Self {
        Self {
            target,
            query,
            category: None,
            source: String::new(),
            kind: String::new(),
            network: Vec::new(),
            crypto: [None, None],
            requested: false,
            pending: Some(Instant::now()),
            autofocus: true,
            selected: 0,
        }
    }

    pub fn network(&mut self, query: &str, hits: &[SearchHit]) {
        if query == self.query {
            self.network = hits
                .iter()
                .cloned()
                .map(|mut hit| {
                    hit.name = crate::security_names::sanitize(&hit.name);
                    hit
                })
                .collect();
        }
    }

    pub fn catalog(&mut self, venue: CryptoVenue, result: Result<Vec<CryptoInstrument>, String>) {
        self.crypto[venue_index(venue)] = Some(result);
    }

    pub fn requests(&mut self) -> Vec<crate::worker::Request> {
        use crate::worker::Request;
        let mut requests = Vec::new();
        if !self.requested && (self.category.is_none() || self.category == Some(AssetClass::Crypto)) {
            self.requested = true;
            requests
                .extend([Request::SymbolCatalog(CryptoVenue::Binance), Request::SymbolCatalog(CryptoVenue::Coinbase)]);
        }
        if self.pending.is_some_and(|at| at.elapsed() >= Duration::from_millis(250)) {
            self.pending = None;
            if !self.query.trim().is_empty() {
                requests.push(Request::Search(self.query.clone()));
            }
        }
        requests
    }

    fn candidates(&self) -> Vec<SearchHit> {
        let builtin: Vec<_> = tr_core::catalog::CATALOG
            .iter()
            .map(|l| SearchHit { key: l.key(), name: l.name.into(), class: l.class, exchange: Some("Built-in".into()) })
            .collect();
        let mut hits = self.network.clone();
        for products in self.crypto.iter().filter_map(|v| v.as_ref().and_then(|r| r.as_ref().ok())) {
            hits.extend(products.iter().map(|c| SearchHit {
                key: c.canonical_ticker(),
                name: format!("{} / {}", c.base, c.quote),
                class: AssetClass::Crypto,
                exchange: None,
            }));
        }
        hits.extend(builtin);
        for hit in &mut hits {
            if let Ok(c) = tr_data::security(&hit.key).crypto_instrument() {
                hit.key = c.security().key();
            }
        }
        let query = self.query.trim().to_uppercase();
        let mut seen = HashSet::new();
        hits.retain(|h| {
            seen.insert(h.key.clone()) && self.category.is_none_or(|c| h.class == c) && rank(h, &query).is_some()
        });
        hits.sort_by_cached_key(|h| (rank(h, &query), h.key.clone()));
        hits
    }

    #[cfg(test)]
    fn results(&self) -> Vec<SearchHit> {
        self.filtered(self.candidates())
    }

    fn filtered(&self, hits: Vec<SearchHit>) -> Vec<SearchHit> {
        hits.into_iter()
            .filter(|h| {
                (self.source.is_empty() || source(h) == self.source)
                    && (self.kind.is_empty() || kind(h.class) == self.kind)
            })
            .collect()
    }

    pub fn show(&mut self, ctx: &egui::Context, present: &[String]) -> Outcome {
        let mut out = Outcome { key: None, close: ctx.input(|i| i.key_pressed(egui::Key::Escape)) };
        let screen = ctx.content_rect();
        let size = egui::vec2(720.0_f32.min(screen.width() - 24.0), 620.0_f32.min(screen.height() - 24.0));
        egui::Window::new(if self.target.is_some() { "Add symbol" } else { "Symbol search" })
            .id(egui::Id::new("symbol-search"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .fixed_size(size)
            .collapsible(false)
            .resizable(false)
            .frame(egui::Frame::new().fill(theme::card()).corner_radius(12).inner_margin(16.0).shadow(egui::Shadow {
                offset: [0, 8],
                blur: 24,
                spread: 0,
                color: egui::Color32::from_black_alpha(90),
            }))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let field = ui.add_sized(
                        [ui.available_width() - 32.0, 28.0],
                        egui::TextEdit::singleline(&mut self.query).hint_text("Symbol or name"),
                    );
                    if self.autofocus {
                        field.request_focus();
                        self.autofocus = false;
                    }
                    if field.changed() {
                        self.network.clear();
                        self.pending = Some(Instant::now());
                        self.selected = 0;
                    }
                    out.close |= ui.button("×").clicked();
                });
                ui.horizontal_wrapped(|ui| {
                    for (label, class) in CATEGORIES {
                        if ui.selectable_label(self.category == class, label).clicked() {
                            self.category = class;
                            self.source.clear();
                            self.kind.clear();
                            self.selected = 0;
                        }
                    }
                });
                let hits = self.candidates();
                let mut sources: Vec<_> = hits.iter().map(source).collect();
                sources.sort();
                sources.dedup();
                let mut types: Vec<_> = hits.iter().map(|h| kind(h.class)).collect();
                types.sort();
                types.dedup();
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("symbol-source")
                        .selected_text(if self.source.is_empty() { "All sources" } else { &self.source })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.source, String::new(), "All sources");
                            for s in sources {
                                ui.selectable_value(&mut self.source, s.clone(), s);
                            }
                        });
                    egui::ComboBox::from_id_salt("symbol-type")
                        .selected_text(if self.kind.is_empty() { "All types" } else { &self.kind })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.kind, String::new(), "All types");
                            for t in types {
                                ui.selectable_value(&mut self.kind, t.into(), t);
                            }
                        });
                });
                for (i, label) in ["Binance", "Coinbase"].iter().enumerate() {
                    match &self.crypto[i] {
                        None if self.requested => {
                            ui.label(format!("Loading {label}…"));
                        }
                        Some(Err(e)) => {
                            ui.colored_label(theme::muted(), format!("{label}: {e}"));
                        }
                        _ => {}
                    }
                }
                let hits = self.filtered(hits);
                let count = hits.len().min(LIMIT);
                self.selected = self.selected.min(count.saturating_sub(1));
                ui.input(|i| {
                    if i.key_pressed(egui::Key::ArrowDown) {
                        self.selected = (self.selected + 1).min(count.saturating_sub(1));
                    }
                    if i.key_pressed(egui::Key::ArrowUp) {
                        self.selected = self.selected.saturating_sub(1);
                    }
                    if i.key_pressed(egui::Key::Enter) && count > 0 {
                        out.key = Some(hits[self.selected].key.clone());
                        out.close |= i.modifiers.shift || i.events.iter().any(|event| matches!(event, egui::Event::Key { key: egui::Key::Enter, modifiers, .. } if modifiers.shift));
                    }
                });
                let mut scroll = egui::ScrollArea::vertical().max_height((size.y - 220.0).max(80.0));
                if ui.input(|i| i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::ArrowUp)) {
                    scroll = scroll.vertical_scroll_offset(self.selected as f32 * (42.0 + ui.spacing().item_spacing.y));
                }
                scroll.show_rows(ui, 42.0, count, |ui, range| {
                    for index in range {
                        let h = &hits[index];
                        let sec = tr_data::security(&h.key);
                        let ticker =
                            sec.crypto_instrument().map_or(sec.ticker.clone(), |c| format!("{}/{}", c.base, c.quote));
                        let remove = self.target.is_some() && present.iter().any(|s| same_key(s, &h.key));
                        let (rect, row_response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 42.0), egui::Sense::click());
                        if index == self.selected || row_response.hovered() { ui.painter().rect_filled(rect, 4.0, theme::header()); }
                        let mut row_ui = ui.new_child(egui::UiBuilder::new().id_salt(&h.key).max_rect(rect));
                        let response = row_ui.horizontal(|ui| {
                            let row = ui.add_sized(
                                [150.0, 36.0],
                                egui::Button::selectable(
                                    index == self.selected,
                                    egui::RichText::new(ticker).monospace().color(theme::amber()),
                                ),
                            );
                            let details = ui.vertical(|ui| {
                                ui.set_width((ui.available_width() - 42.0).max(40.0));
                                let name = ui.add(egui::Label::new(egui::RichText::new(&h.name).color(theme::muted())).truncate().sense(egui::Sense::click()));
                                let meta = ui.add(egui::Label::new(egui::RichText::new(format!("{} · {}", kind(h.class), source(h))).small()).sense(egui::Sense::click()));
                                name.clicked() || meta.clicked()
                            });
                            let action = ui.button(if remove { "🗑" } else { "+" }).on_hover_text(if remove {
                                "Remove from list"
                            } else {
                                "Add"
                            });
                            row.clicked() || action.clicked() || details.inner
                        });
                        if response.inner || row_response.clicked() {
                            self.selected = index;
                            out.key = Some(h.key.clone());
                            out.close |= ui.input(|i| i.modifiers.shift || i.events.iter().any(|event| matches!(event, egui::Event::PointerButton { modifiers, .. } if modifiers.shift)));
                        }
                    }
                });
                if hits.len() > LIMIT {
                    ui.label(format!("Showing first {LIMIT} — refine your search"));
                }
                ui.separator();
                ui.small(if self.target.is_some() {
                    "⇧ + Click or ⇧ + Enter to add symbol and close dialog"
                } else {
                    "Enter to open · ⇧ Enter to open and close"
                });
            });
        ctx.request_repaint_after(Duration::from_millis(250));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use watch_sections::Item;

    fn hit(ticker: &str, name: &str) -> SearchHit {
        SearchHit {
            key: format!("{ticker} US Equity"),
            name: name.into(),
            class: AssetClass::Equity,
            exchange: Some("NASDAQ".into()),
        }
    }

    fn xrp() -> Dialog {
        let mut dialog = Dialog::new("XRP".into(), Some(Target::Append));
        for (venue, key) in [(CryptoVenue::Binance, "BINANCE:XRPUSDT"), (CryptoVenue::Coinbase, "COINBASE:XRP-USD")] {
            dialog.catalog(venue, Ok(vec![CryptoInstrument::parse(key).unwrap()]));
        }
        dialog.network("XRP", &[hit("OTHER", "XRP Corporation")]);
        dialog
    }

    fn frame(
        dialog: &mut Dialog,
        ctx: &egui::Context,
        key: Option<egui::Key>,
        shift: bool,
        present: &[String],
    ) -> (Outcome, Vec<String>) {
        let mut outcome = Outcome { key: None, close: false };
        let modifiers = egui::Modifiers { shift, ..Default::default() };
        let events = key
            .map(|key| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers })
            .into_iter()
            .collect();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0))),
                events,
                ..Default::default()
            },
            |ui| {
                outcome = dialog.show(ui.ctx(), present);
            },
        );
        output.textures_delta.clear();
        let labels = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some(t.galley.job.text.clone()),
                _ => None,
            })
            .collect();
        (outcome, labels)
    }

    #[test]
    fn xrp_pairs_precede_name_match_and_exact_ticker_wins() {
        let mut dialog = xrp();
        let hits = dialog.results();
        let equity = hits.iter().position(|h| h.key == "OTHER US Equity").unwrap();
        for venue in ["BINANCE:XRPUSDT", "COINBASE:XRP-USD"] {
            assert!(hits.iter().position(|h| h.key.starts_with(venue)).unwrap() < equity);
        }
        dialog.network("XRP", &[hit("XRP", "Exact ticker")]);
        assert_eq!(dialog.results()[0].key, "XRP US Equity");
        assert!(rank(&hit("XRPABC", "Prefix"), "XRP") < rank(&hit("AXRP", "Substring"), "XRP"));
    }

    #[test]
    fn major_quote_pairs_rank_before_other_quotes_for_the_same_base() {
        let pair = |ticker: &str| SearchHit {
            key: tr_data::security(ticker).key(),
            name: ticker.into(),
            class: AssetClass::Crypto,
            exchange: None,
        };
        let mut hits: Vec<_> =
            ["BINANCE:XRPBRL", "BINANCE:XRPTRY", "COINBASE:XRP-USD", "BINANCE:XRPUSDC", "BINANCE:XRPUSDT"]
                .into_iter()
                .map(pair)
                .collect();
        hits.sort_by_cached_key(|h| (rank(h, "XRP"), h.key.clone()));
        let order: Vec<String> = hits.iter().map(|h| tr_data::security(&h.key).ticker).collect();
        assert_eq!(
            order,
            ["COINBASE:XRP-USD", "BINANCE:XRPUSDT", "BINANCE:XRPUSDC", "BINANCE:XRPBRL", "BINANCE:XRPTRY"]
        );
    }

    #[test]
    fn category_source_type_filters_and_canonical_dedupe() {
        let mut dialog = xrp();
        let hits = dialog.results();
        assert_eq!(hits.iter().filter(|h| h.key.starts_with("COINBASE:XRP-USD")).count(), 1);
        dialog.category = Some(AssetClass::Equity);
        assert!(dialog.results().iter().all(|h| h.class == AssetClass::Equity));
        dialog.category = Some(AssetClass::Crypto);
        dialog.source = "Binance".into();
        assert_eq!(dialog.results().len(), 1);
        dialog.kind = "stock".into();
        assert!(dialog.results().is_empty());
        dialog.kind = "spot crypto".into();
        assert_eq!(dialog.results().len(), 1);
        assert!(same_key("BINANCE:XRPUSDT", "BINANCE:XRPUSDT Curncy"));
    }

    #[test]
    fn stale_network_is_ignored_and_catalog_failure_is_inline() {
        let mut dialog = xrp();
        dialog.network("OLD", &[hit("XRP", "Stale")]);
        assert!(!dialog.results().iter().any(|h| h.name == "Stale"));
        dialog.catalog(CryptoVenue::Binance, Err("region-restricted".into()));
        assert!(dialog.results().iter().any(|h| h.key.starts_with("COINBASE:XRP-USD")));
        let ctx = egui::Context::default();
        frame(&mut dialog, &ctx, None, false, &[]);
        let (_, labels) = frame(&mut dialog, &ctx, None, false, &[]);
        assert!(labels.iter().any(|s| s.contains("Binance: region-restricted")));
    }

    #[test]
    fn discovery_requests_are_once_per_dialog_and_search_is_debounced() {
        let mut dialog = Dialog::new("XRP".into(), None);
        assert_eq!(dialog.requests().len(), 2);
        assert!(dialog.requests().is_empty());
        dialog.pending = Some(Instant::now() - Duration::from_millis(300));
        assert!(matches!(&dialog.requests()[..], [crate::worker::Request::Search(q)] if q == "XRP"));
        assert!(dialog.requests().is_empty());
    }

    #[test]
    fn dialog_enter_shift_enter_escape_and_remove_action() {
        let ctx = egui::Context::default();
        let mut dialog = xrp();
        frame(&mut dialog, &ctx, None, false, &[]);
        let (add, _) = frame(&mut dialog, &ctx, Some(egui::Key::Enter), false, &[]);
        assert!(add.key.is_some());
        assert!(!add.close);
        let present = vec![add.key.unwrap()];
        let (_, labels) = frame(&mut dialog, &ctx, None, false, &present);
        assert!(labels.iter().any(|s| s == "🗑"));
        frame(&mut dialog, &ctx, None, false, &present);
        let (remove, _) = frame(&mut dialog, &ctx, Some(egui::Key::Enter), true, &present);
        assert_eq!(remove.key.as_ref(), present.first());
        assert!(remove.close);
        let (escape, _) = frame(&mut dialog, &ctx, Some(egui::Key::Escape), false, &[]);
        assert!(escape.close);
    }

    #[test]
    fn cap_note_is_rendered_for_more_than_500_hits() {
        let mut dialog = Dialog::new("ZZTEST".into(), None);
        dialog.network("ZZTEST", &(0..600).map(|i| hit(&format!("ZZTEST{i}"), "Test")).collect::<Vec<_>>());
        let ctx = egui::Context::default();
        frame(&mut dialog, &ctx, None, false, &[]);
        let (_, labels) = frame(&mut dialog, &ctx, None, false, &[]);
        assert_eq!(dialog.results().len(), 600);
        assert!(labels.iter().any(|s| s == "Showing first 500 — refine your search"));
    }

    fn section(name: &str) -> Item {
        Item::Section { name: name.into(), collapsed: false }
    }

    #[test]
    fn insertion_targets_append_row_section_and_keep_add_order() {
        let original = vec![section("Tech"), Item::Symbol("A".into()), section("Other"), Item::Symbol("B".into())];
        let mut items = original.clone();
        Target::Append.insert(&mut items, "C");
        assert_eq!(items.last(), Some(&Item::Symbol("C".into())));
        let mut items = original.clone();
        let mut target = Target::After("A".into());
        assert!(target.insert(&mut items, "C"));
        assert!(target.insert(&mut items, "D"));
        assert_eq!(&items[1..4], &[Item::Symbol("A".into()), Item::Symbol("C".into()), Item::Symbol("D".into())]);
        let mut items = original;
        let mut target = Target::Section { position: 0, name: "Tech".into() };
        assert!(target.insert(&mut items, "C"));
        assert_eq!(items[2], Item::Symbol("C".into()));
        assert_eq!(items[3], section("Other"));
    }

    #[test]
    fn removed_anchors_append_and_moved_section_is_resolved_again() {
        let mut items = vec![section("Other"), Item::Symbol("B".into())];
        Target::After("Gone".into()).insert(&mut items, "C");
        assert_eq!(items.last(), Some(&Item::Symbol("C".into())));
        Target::Section { position: 0, name: "Gone".into() }.insert(&mut items, "D");
        assert_eq!(items.last(), Some(&Item::Symbol("D".into())));
        let mut target = Target::Section { position: 0, name: "Other".into() };
        items.insert(0, Item::Symbol("Before".into()));
        items.push(section("Next"));
        target.insert(&mut items, "E");
        assert_eq!(items[items.len() - 2], Item::Symbol("E".into()));
    }

    #[test]
    fn duplicate_and_200_cap_do_not_advance_target() {
        let mut items = (0..200).map(|i| Item::Symbol(format!("S{i}"))).collect::<Vec<_>>();
        let mut target = Target::After("S0".into());
        assert!(!target.insert(&mut items, "New"));
        items.pop();
        assert!(!target.insert(&mut items, "S0"));
        assert!(target.insert(&mut items, "New"));
        assert_eq!(items[1], Item::Symbol("New".into()));
    }

    #[test]
    fn section_rebase_does_not_retarget_a_removed_duplicate_name() {
        let old = vec![section("Same"), Item::Symbol("A".into()), section("Same"), Item::Symbol("B".into())];
        let mut new = vec![Item::Symbol("A".into()), section("Same"), Item::Symbol("B".into()), section("Last")];
        let mut target = Target::Section { position: 0, name: "Same".into() };
        target.rebase(&old, &new);
        target.insert(&mut new, "C");
        assert_eq!(new.last(), Some(&Item::Symbol("C".into())));
    }

    #[test]
    fn shift_clicking_result_name_acts_and_closes() {
        let mut dialog = xrp();
        let ctx = egui::Context::default();
        frame(&mut dialog, &ctx, None, false, &[]);
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0))),
                ..Default::default()
            },
            |ui| {
                dialog.show(ui.ctx(), &[]);
            },
        );
        let pos = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == "XRP / USD" => Some(t.pos + t.galley.size() / 2.0),
                _ => None,
            })
            .unwrap();
        output.textures_delta.clear();
        let mut outcome = Outcome { key: None, close: false };
        for pressed in [true, false] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0))),
                    events: vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::SHIFT,
                        },
                    ],
                    ..Default::default()
                },
                |ui| {
                    outcome = dialog.show(ui.ctx(), &[]);
                },
            );
            output.textures_delta.clear();
        }
        assert!(outcome.key.unwrap().starts_with("COINBASE:XRP-USD"));
        assert!(outcome.close);
    }

    fn has_key(hits: &[SearchHit], prefix: &str) -> bool {
        hits.iter().any(|h| h.key.starts_with(prefix))
    }

    #[test]
    #[ignore = "BUG: pair query 'xrp/usd' matches neither Coinbase XRP-USD nor Binance XRPUSDT"]
    fn slash_pair_query_surfaces_crypto_pairs() {
        let cases: [(&str, &str); 4] = [
            ("xrp/usd", "COINBASE:XRP-USD"),
            ("XRP/USD", "COINBASE:XRP-USD"),
            ("XRP/USDT", "BINANCE:XRPUSDT"),
            ("btc/usd", "COINBASE:BTC-USD"),
        ];
        for (query, expect) in cases {
            let mut dialog = Dialog::new(query.into(), Some(Target::Append));
            for (venue, key) in [
                (CryptoVenue::Binance, "BINANCE:XRPUSDT"),
                (CryptoVenue::Coinbase, "COINBASE:XRP-USD"),
                (CryptoVenue::Binance, "BINANCE:BTCUSDT"),
                (CryptoVenue::Coinbase, "COINBASE:BTC-USD"),
            ] {
                dialog.catalog(venue, Ok(vec![CryptoInstrument::parse(key).unwrap()]));
            }
            let hits = dialog.results();
            assert!(
                has_key(&hits, expect),
                "{query:?} must surface {expect}: {:?}",
                hits.iter().map(|h| &h.key).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    #[ignore = "BUG: same_key does not canonicalize non-crypto keys, so bare/lowercase forms duplicate"]
    fn same_key_canonicalizes_bare_lowercase_and_full_market_keys() {
        assert!(same_key("MSFT", "MSFT US Equity"));
        assert!(same_key("msft us equity", "MSFT US Equity"));
        assert!(same_key("aapl", "AAPL US Equity"));
        assert!(same_key("SPX", "SPX Index"));
        assert!(same_key("BINANCE:XRPUSDT", "XRPUSDT Curncy"));
        assert!(!same_key("MSFT", "AAPL US Equity"));
    }
}
