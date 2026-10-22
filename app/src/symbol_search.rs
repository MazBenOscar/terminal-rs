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
        let security = tr_data::security(key);
        security.crypto_instrument().map_or_else(|_| security.key(), |c| c.security().key())
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
    pub list_name: String,
    category: Option<AssetClass>,
    source: String,
    kind: String,
    network: Vec<SearchHit>,
    crypto: [Option<Result<Vec<CryptoInstrument>, String>>; 2],
    requested: bool,
    pending: Option<Instant>,
    autofocus: bool,
    selected: usize,
    expanded: HashSet<String>,
    sources_view: bool,
    source_query: String,
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
    let query = query.trim().to_uppercase();
    let tokens: Vec<_> = query.split(['/', '-', ':', ' ']).filter(|s| !s.is_empty()).collect();
    let venue = tokens.first().filter(|s| SOURCE_REGISTRY.iter().any(|(id, _, _)| id.eq_ignore_ascii_case(s)));
    if venue.is_some_and(|v| !source(hit).eq_ignore_ascii_case(v)) {
        return None;
    }
    let pair = tokens[usize::from(venue.is_some())..].concat();
    let query = query.as_str();
    let sec = tr_data::security(&hit.key);
    let ticker = sec.ticker.to_uppercase();
    let crypto = sec.crypto_instrument().ok();
    let query = if crypto.is_some() && venue.is_some() { pair.as_str() } else { query };
    let native = crypto.as_ref().map_or(ticker.as_str(), |c| c.native_symbol.as_str());
    let name = hit.name.to_uppercase();
    let pair_match = crypto.as_ref().is_some_and(|c| format!("{}{}", c.base, c.quote) == pair);
    let rank = if query.is_empty() || ticker == query || native == query || pair_match {
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

/// Preferred primary sources; unlisted sources follow alphabetically.
const SOURCE_ORDER: [&str; 2] = ["Coinbase", "Binance"];

#[derive(Clone, Copy)]
enum SourceKind {
    CentralizedExchange,
    #[expect(dead_code, reason = "Reserved for decentralized source registry entries")]
    DecentralizedExchange,
    StockExchange,
    BuiltIn,
}

impl SourceKind {
    fn label(self) -> &'static str {
        match self {
            Self::CentralizedExchange => "Centralized exchange · spot crypto",
            Self::DecentralizedExchange => "Decentralized exchange · spot crypto",
            Self::StockExchange => "US stock exchange",
            Self::BuiltIn => "Built-in catalog",
        }
    }
}

struct SourceInfo {
    id: String,
    name: String,
    kind: SourceKind,
    badge: (char, egui::Color32),
}

const SOURCE_REGISTRY: [(&str, SourceKind, egui::Color32); 3] = [
    ("Coinbase", SourceKind::CentralizedExchange, egui::Color32::from_rgb(65, 125, 255)),
    ("Binance", SourceKind::CentralizedExchange, egui::Color32::from_rgb(240, 185, 11)),
    ("Built-in", SourceKind::BuiltIn, egui::Color32::GRAY),
];

impl SourceInfo {
    fn new(name: &str, kind: SourceKind, color: egui::Color32) -> Self {
        Self { id: name.into(), name: name.into(), kind, badge: (name.chars().next().unwrap_or('?'), color) }
    }

    fn from_name(name: &str) -> Self {
        SOURCE_REGISTRY.iter().find(|(id, _, _)| *id == name).map_or_else(
            || Self::new(name, SourceKind::StockExchange, theme::muted()),
            |(_, kind, color)| Self::new(name, *kind, *color),
        )
    }
}

fn source_order(name: &str) -> (usize, String) {
    (SOURCE_ORDER.iter().position(|s| *s == name).unwrap_or(SOURCE_ORDER.len()), name.to_lowercase())
}

fn market_key(hit: &SearchHit) -> String {
    let security = tr_data::security(&hit.key);
    security.crypto_instrument().map_or_else(|_| security.key(), |c| format!("{}/{}", c.base, c.quote))
}

struct Group {
    id: String,
    members: Vec<SearchHit>,
}

impl Group {
    fn name(&self) -> &str {
        self.members.iter().max_by_key(|h| h.name.len()).map_or("", |h| h.name.as_str())
    }
}

#[derive(Clone, Copy)]
struct VisibleRow {
    group: usize,
    member: Option<usize>,
}

fn search_field(ui: &mut egui::Ui, text: &mut String, autofocus: &mut bool) -> bool {
    let mut changed = false;
    egui::Frame::new().fill(theme::header()).corner_radius(5).inner_margin(5.0).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("⌕").on_hover_text("Search symbols or names");
            let width = (ui.available_width() - 30.0).max(20.0);
            let field = ui.add_sized(
                [width, 24.0],
                egui::TextEdit::singleline(text).frame(egui::Frame::NONE).hint_text("Symbol or name"),
            );
            if *autofocus {
                field.request_focus();
                *autofocus = false;
            }
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                field.request_focus();
            }
            changed = field.changed();
            if !text.is_empty()
                && ui.small_button("⊗").on_hover_text("Clear search").clicked_by(egui::PointerButton::Primary)
            {
                text.clear();
                field.request_focus();
                changed = true;
            }
        });
    });
    changed
}

impl Dialog {
    pub fn new(query: String, target: Option<Target>) -> Self {
        Self {
            target,
            query,
            list_name: "list".into(),
            category: None,
            source: String::new(),
            kind: String::new(),
            network: Vec::new(),
            crypto: [None, None],
            requested: false,
            pending: Some(Instant::now()),
            autofocus: true,
            selected: 0,
            expanded: HashSet::new(),
            sources_view: false,
            source_query: String::new(),
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
        let slot = &mut self.crypto[venue_index(venue)];
        if let (Some(Ok(existing)), Ok(products)) = (&mut *slot, &result) {
            for product in products {
                if !existing.contains(product) {
                    existing.push(product.clone());
                }
            }
        } else {
            *slot = Some(result);
        }
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
            let security = tr_data::security(&hit.key);
            hit.key = security.crypto_instrument().map_or_else(|_| security.key(), |c| c.security().key());
        }
        let query = self.query.trim().to_uppercase();
        let mut unique: Vec<SearchHit> = Vec::new();
        let mut seen = std::collections::HashMap::new();
        for hit in hits {
            if let Some(&index) = seen.get(&hit.key) {
                let previous: &mut SearchHit = &mut unique[index];
                if hit.name.len() > previous.name.len() {
                    previous.name = hit.name;
                }
            } else {
                seen.insert(hit.key.clone(), unique.len());
                unique.push(hit);
            }
        }
        let mut hits = unique;
        hits.retain(|h| self.category.is_none_or(|c| h.class == c) && rank(h, &query).is_some());
        hits.sort_by_cached_key(|h| (rank(h, &query), h.key.clone()));
        hits
    }

    #[cfg(test)]
    fn results(&self) -> Vec<SearchHit> {
        self.filtered(self.candidates())
    }

    #[cfg(test)]
    fn filtered(&self, hits: Vec<SearchHit>) -> Vec<SearchHit> {
        hits.into_iter()
            .filter(|h| {
                (self.source.is_empty() || source(h) == self.source)
                    && (self.kind.is_empty() || kind(h.class) == self.kind)
            })
            .collect()
    }

    fn groups(&self) -> Vec<Group> {
        let mut groups: Vec<Group> = Vec::new();
        for hit in self.candidates().into_iter().filter(|h| self.kind.is_empty() || kind(h.class) == self.kind) {
            let id = market_key(&hit);
            if let Some(group) = groups.iter_mut().find(|g| g.id == id) {
                group.members.push(hit);
            } else {
                groups.push(Group { id, members: vec![hit] });
            }
        }
        // Candidates are already ranked; the first member gives each market its best rank.
        groups.retain(|g| self.source.is_empty() || g.members.iter().any(|h| source(h) == self.source));
        for group in &mut groups {
            group.members.sort_by_cached_key(|h| {
                let name = source(h);
                (self.source != name, source_order(&name), h.key.clone())
            });
        }
        groups
    }

    fn visible_rows(&self, groups: &[Group]) -> Vec<VisibleRow> {
        let mut rows = Vec::new();
        for (index, group) in groups.iter().take(LIMIT).enumerate() {
            rows.push(VisibleRow { group: index, member: None });
            if self.expanded.contains(&group.id) {
                rows.extend((0..group.members.len()).map(|member| VisibleRow { group: index, member: Some(member) }));
            }
        }
        rows
    }

    fn sources(&self) -> Vec<SourceInfo> {
        let mut names: Vec<_> = SOURCE_REGISTRY.iter().map(|(id, _, _)| (*id).to_owned()).collect();
        names.extend(self.network.iter().map(source));
        names.sort_by_cached_key(|name| source_order(name));
        names.dedup();
        names.iter().map(|name| SourceInfo::from_name(name)).collect()
    }

    fn source_choices(&self) -> Vec<SourceInfo> {
        let query = self.source_query.trim().to_lowercase();
        self.sources()
            .into_iter()
            .filter(|s| s.name.to_lowercase().contains(&query) || s.kind.label().to_lowercase().contains(&query))
            .collect()
    }

    fn choose_source(&mut self, id: String) {
        self.source = id;
        self.sources_view = false;
        self.selected = 0;
        self.autofocus = true;
    }

    fn show_sources(&mut self, ui: &mut egui::Ui) {
        if search_field(ui, &mut self.source_query, &mut self.autofocus) {
            self.selected = 0;
        }
        egui::ScrollArea::vertical().id_salt("source-list").auto_shrink([false, false]).show(ui, |ui| {
            let mut choices = self.source_choices();
            if "all sources".contains(&self.source_query.trim().to_lowercase()) {
                choices.insert(0, SourceInfo::new("All sources", SourceKind::BuiltIn, theme::muted()));
                choices[0].id.clear();
            }
            let column_width = (ui.available_width() - 12.0) / 2.0;
            egui::Grid::new("sources-grid").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
                for (index, info) in choices.iter().enumerate() {
                    let mut text = egui::text::LayoutJob::default();
                    text.append(
                        &format!("{}\n", info.name),
                        0.0,
                        egui::TextFormat {
                            font_id: egui::FontId::proportional(14.0),
                            color: ui.visuals().text_color(),
                            ..Default::default()
                        },
                    );
                    if !info.id.is_empty() {
                        text.append(
                            info.kind.label(),
                            0.0,
                            egui::TextFormat {
                                font_id: egui::FontId::proportional(11.0),
                                color: theme::muted(),
                                ..Default::default()
                            },
                        );
                    }
                    if ui
                        .add_sized([column_width, 52.0], egui::Button::selectable(self.source == info.id, text))
                        .on_hover_text(format!("Show symbols from {}", info.name))
                        .clicked()
                    {
                        self.choose_source(info.id.clone());
                    }
                    if index % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
        });
    }

    fn keyboard(&mut self, ui: &egui::Ui, groups: &[Group], out: &mut Outcome) -> Vec<VisibleRow> {
        let mut rows = self.visible_rows(groups);
        self.selected = self.selected.min(rows.len().saturating_sub(1));
        ui.input(|i| {
            if i.key_pressed(egui::Key::ArrowDown) {
                self.selected = (self.selected + 1).min(rows.len().saturating_sub(1));
            }
            if i.key_pressed(egui::Key::ArrowUp) { self.selected = self.selected.saturating_sub(1); }
            if let Some(row) = rows.get(self.selected) {
                let group = &groups[row.group];
                if i.key_pressed(egui::Key::ArrowRight) && group.members.len() > 1 {
                    self.expanded.insert(group.id.clone());
                }
                if i.key_pressed(egui::Key::ArrowLeft) {
                    self.expanded.remove(&group.id);
                    self.selected = rows.iter().position(|r| r.group == row.group).unwrap_or(0);
                }
                if i.key_pressed(egui::Key::Enter) {
                    out.key = Some(group.members[row.member.unwrap_or(0)].key.clone());
                    out.close |= i.modifiers.shift || i.events.iter().any(|event| matches!(event, egui::Event::Key { key: egui::Key::Enter, modifiers, .. } if modifiers.shift));
                }
            }
        });
        rows = self.visible_rows(groups);
        rows
    }

    fn show_results(&mut self, ui: &mut egui::Ui, present: &[String], out: &mut Outcome) {
        if search_field(ui, &mut self.query, &mut self.autofocus) {
            self.network.clear();
            self.pending = Some(Instant::now());
            self.selected = 0;
            self.expanded.clear();
        }
        ui.horizontal_wrapped(|ui| {
            for (label, class) in CATEGORIES {
                let tooltip = if class.is_none() { "Show all categories".into() } else { format!("Show only {label}") };
                if ui.selectable_label(self.category == class, label).on_hover_text(tooltip).clicked() {
                    self.category = class;
                    self.source.clear();
                    self.kind.clear();
                    self.selected = 0;
                }
            }
        });
        ui.horizontal(|ui| {
            if ui
                .button(if self.source.is_empty() { "All sources" } else { &self.source })
                .on_hover_text("Filter by source")
                .clicked()
            {
                self.sources_view = true;
                self.source_query.clear();
                self.autofocus = true;
            }
            let mut types: Vec<_> = self.candidates().iter().map(|h| kind(h.class)).collect();
            types.sort();
            types.dedup();
            egui::ComboBox::from_id_salt("symbol-type")
                .selected_text(if self.kind.is_empty() { "All types" } else { &self.kind })
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_value(&mut self.kind, String::new(), "All types")
                        .on_hover_text("Show all types")
                        .clicked()
                    {
                        self.selected = 0;
                    }
                    for t in types {
                        if ui
                            .selectable_value(&mut self.kind, t.into(), t)
                            .on_hover_text(format!("Show only {t}"))
                            .clicked()
                        {
                            self.selected = 0;
                        }
                    }
                })
                .response
                .on_hover_text("Filter by instrument type");
        });
        let groups = self.groups();
        let rows = self.keyboard(ui, &groups, out);
        let footer_height = if groups.len() > LIMIT { 60.0 } else { 38.0 };
        let height = (ui.available_height() - footer_height).max(0.0);
        egui::ScrollArea::vertical().id_salt("symbol-results").auto_shrink([false, false]).max_height(height).show(
            ui,
            |ui| {
                ui.set_min_height(height);
                let top = ui.cursor().top();
                for (i, label) in ["Binance", "Coinbase"].iter().enumerate() {
                    match &self.crypto[i] {
                        None if self.requested => {
                            ui.colored_label(theme::muted(), format!("Loading {label}…"));
                        }
                        Some(Err(e)) => {
                            ui.colored_label(theme::muted(), format!("{label}: {e}"));
                        }
                        _ => {}
                    }
                }
                if rows.is_empty() {
                    let text = if self.query.trim().is_empty() {
                        "Type a symbol or name".into()
                    } else {
                        format!("No symbols match “{}”", self.query.trim())
                    };
                    let remaining = (height - (ui.cursor().top() - top)).max(0.0);
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(ui.available_width(), remaining), egui::Sense::hover());
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        text,
                        egui::TextStyle::Body.resolve(ui.style()),
                        theme::muted(),
                    );
                }
                for (index, row) in rows.iter().enumerate() {
                    self.show_row(ui, &groups[row.group], *row, index, present, out);
                }
            },
        );
        if groups.len() > LIMIT {
            ui.label(format!("Showing first {LIMIT} — refine your search"));
        }
        ui.separator();
        ui.small(if self.target.is_some() {
            "⇧ + Click or ⇧ + Enter to add symbol and close dialog"
        } else {
            "Enter to open · ⇧ Enter to open and close"
        })
        .on_hover_text("↑/↓ select · → expand sources · ← collapse · Enter activates · Esc closes");
    }

    fn show_row(
        &mut self,
        ui: &mut egui::Ui,
        group: &Group,
        row: VisibleRow,
        index: usize,
        present: &[String],
        out: &mut Outcome,
    ) {
        let hit = &group.members[row.member.unwrap_or(0)];
        let sec = tr_data::security(&hit.key);
        let ticker = sec.crypto_instrument().map_or(sec.ticker.clone(), |c| format!("{}/{}", c.base, c.quote));
        let remove = self.target.is_some() && present.iter().any(|s| same_key(s, &hit.key));
        let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 42.0), egui::Sense::click());
        if index == self.selected || response.hovered() {
            ui.painter().rect_filled(rect, 4.0, theme::header());
        }
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt((&group.id, row.member))
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let mut clicked = false;
        if row.member.is_some() {
            child.add_space(18.0);
        }
        clicked |= child
            .add_sized(
                [130.0, 36.0],
                egui::Button::new(egui::RichText::new(&ticker).monospace().color(theme::amber())).frame(false),
            )
            .clicked();
        let name_width = (child.available_width() - 300.0).max(30.0);
        clicked |= child
            .add_sized(
                [name_width, 36.0],
                egui::Label::new(
                    egui::RichText::new(if row.member.is_none() { group.name() } else { &hit.name })
                        .color(theme::muted()),
                )
                .truncate()
                .sense(egui::Sense::click()),
            )
            .clicked();
        clicked |= child
            .add(
                egui::Label::new(egui::RichText::new(format!("{} · {}", kind(hit.class), source(hit))).small())
                    .truncate()
                    .sense(egui::Sense::click()),
            )
            .on_hover_text(source(hit))
            .clicked();
        if row.member.is_none() {
            let mut names: Vec<_> = group.members.iter().map(source).collect();
            names.dedup();
            for name in names.iter().take(3) {
                let info = SourceInfo::from_name(name);
                let (badge, response) = child.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                child.painter().circle_filled(badge.center(), 9.0, info.badge.1);
                child.painter().text(
                    badge.center(),
                    egui::Align2::CENTER_CENTER,
                    info.badge.0,
                    egui::FontId::proportional(11.0),
                    egui::Color32::BLACK,
                );
                response.on_hover_text(info.name);
            }
            if names.len() > 3 {
                child.small(format!("+{}", names.len() - 3)).on_hover_text(names.join(", "));
            }
        }
        if row.member.is_none() && group.members.len() > 1 {
            let expanded = self.expanded.contains(&group.id);
            if child
                .small_button(if expanded { "⌄" } else { "›" })
                .on_hover_text(format!("Show all {} sources", group.members.len()))
                .clicked()
            {
                self.selected = index;
                if expanded {
                    self.expanded.remove(&group.id);
                } else {
                    self.expanded.insert(group.id.clone());
                }
            }
        }
        let list = &self.list_name;
        clicked |= child
            .button(if remove { "🗑" } else { "+" })
            .on_hover_text(if remove {
                format!("Remove {ticker} from {list}")
            } else {
                format!("Add {ticker} from {}", source(hit))
            })
            .clicked();
        if clicked || response.clicked() {
            self.selected = index;
            out.key = Some(hit.key.clone());
            out.close |= ui.input(|i| {
                i.modifiers.shift
                    || i.events
                        .iter()
                        .any(|event| matches!(event, egui::Event::PointerButton { modifiers, .. } if modifiers.shift))
            });
        }
        if index == self.selected
            && ui.input(|i| {
                i.key_pressed(egui::Key::ArrowDown)
                    || i.key_pressed(egui::Key::ArrowUp)
                    || i.key_pressed(egui::Key::ArrowLeft)
            })
        {
            response.scroll_to_me(Some(egui::Align::Center));
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, present: &[String]) -> Outcome {
        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let mut out = Outcome { key: None, close: escape && !self.sources_view };
        if escape && self.sources_view {
            self.sources_view = false;
            self.autofocus = true;
        }
        let screen = ctx.content_rect();
        let size = egui::vec2(760.0_f32.min(screen.width() * 0.92), 640.0_f32.min(screen.height() * 0.92))
            - egui::vec2(32.0, 32.0);
        egui::Window::new("Symbol search")
            .id(egui::Id::new("symbol-search"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .title_bar(false)
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
                ui.set_min_size(size);
                ui.set_max_size(size);
                ui.horizontal(|ui| {
                    if self.sources_view && ui.button("‹").on_hover_text("Back").clicked() {
                        self.sources_view = false;
                        self.autofocus = true;
                    }
                    ui.heading(if self.sources_view {
                        "Sources"
                    } else if self.target.is_some() {
                        "Add symbol"
                    } else {
                        "Symbol search"
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        out.close |= ui.button("×").on_hover_text("Close (Esc)").clicked();
                    });
                });
                if self.sources_view {
                    self.show_sources(ui);
                } else {
                    self.show_results(ui, present, &mut out);
                }
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
                egui::Shape::Text(t) if t.galley.job.text == "XRP / US Dollar" => Some(t.pos + t.galley.size() / 2.0),
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
    fn slash_pair_query_surfaces_crypto_pairs() {
        let cases: [(&str, &str); 9] = [
            ("xrp/usd", "COINBASE:XRP-USD"),
            ("XRP/USD", "COINBASE:XRP-USD"),
            ("XRP/USDT", "BINANCE:XRPUSDT"),
            ("btc/usd", "COINBASE:BTC-USD"),
            ("XRP-USD", "COINBASE:XRP-USD"),
            ("XRPUSDT", "BINANCE:XRPUSDT"),
            ("BINANCE:XRPUSDT", "BINANCE:XRPUSDT"),
            ("xrp usd", "COINBASE:XRP-USD"),
            ("coinbase:xrp/usd", "COINBASE:XRP-USD"),
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
    fn same_key_canonicalizes_bare_lowercase_and_full_market_keys() {
        assert!(same_key("MSFT", "MSFT US Equity"));
        assert!(same_key("msft us equity", "MSFT US Equity"));
        assert!(same_key("aapl", "AAPL US Equity"));
        assert!(same_key("SPX", "SPX Index"));
        assert!(same_key("BINANCE:XRPUSDT", "XRPUSDT Curncy"));
        assert!(!same_key("MSFT", "AAPL US Equity"));
    }

    struct Snapshot {
        outcome: Outcome,
        texts: Vec<(String, egui::Rect)>,
        rect: egui::Rect,
    }

    fn snapshot(dialog: &mut Dialog, ctx: &egui::Context, events: Vec<egui::Event>, present: &[String]) -> Snapshot {
        let mut outcome = Outcome { key: None, close: false };
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
        let texts = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => {
                    Some((t.galley.job.text.clone(), egui::Rect::from_min_size(t.pos, t.galley.size())))
                }
                _ => None,
            })
            .collect();
        let rect = ctx.memory(|m| m.area_rect(egui::Id::new("symbol-search"))).unwrap_or(egui::Rect::NOTHING);
        output.textures_delta.clear();
        Snapshot { outcome, texts, rect }
    }

    fn text_rect(snapshot: &Snapshot, text: &str) -> egui::Rect {
        snapshot.texts.iter().find(|(s, _)| s == text).unwrap_or_else(|| panic!("Missing {text:?}")).1
    }

    fn click(dialog: &mut Dialog, ctx: &egui::Context, pos: egui::Pos2) -> Snapshot {
        let event = |pressed| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        snapshot(dialog, ctx, event(true), &[]);
        snapshot(dialog, ctx, event(false), &[])
    }

    fn btc() -> Dialog {
        let mut dialog = Dialog::new("BTC".into(), Some(Target::Append));
        dialog.category = Some(AssetClass::Crypto);
        dialog.catalog(CryptoVenue::Binance, Ok(vec![CryptoInstrument::parse("BINANCE:BTCUSDT").unwrap()]));
        dialog.catalog(
            CryptoVenue::Coinbase,
            Ok(vec![
                CryptoInstrument::parse("COINBASE:BTC-USD").unwrap(),
                CryptoInstrument::parse("COINBASE:BTC-USDT").unwrap(),
            ]),
        );
        dialog
    }

    #[test]
    fn fixed_dialog_size_across_zero_three_and_four_hundred_results() {
        let ctx = egui::Context::default();
        let mut dialog = Dialog::new("ZZNONE".into(), None);
        snapshot(&mut dialog, &ctx, vec![], &[]);
        let mut sizes = Vec::new();
        for count in [0, 3, 400] {
            dialog.query = format!("ZZTEST{count}X");
            dialog.network = (0..count).map(|i| hit(&format!("{}{}", dialog.query, i), "Test")).collect();
            assert_eq!(dialog.results().len(), count);
            snapshot(&mut dialog, &ctx, vec![], &[]);
            sizes.push(snapshot(&mut dialog, &ctx, vec![], &[]).rect.size());
        }
        assert_eq!(sizes[0], sizes[1]);
        assert_eq!(sizes[0], sizes[2]);
        assert!((sizes[0].x - 760.0).abs() < 3.0, "{sizes:?}");
        assert!((sizes[0].y - 640.0).abs() < 3.0, "{sizes:?}");
    }

    #[test]
    fn title_close_is_right_aligned_on_title_row_and_closes() {
        let ctx = egui::Context::default();
        let mut dialog = btc();
        snapshot(&mut dialog, &ctx, vec![], &[]);
        let view = snapshot(&mut dialog, &ctx, vec![], &[]);
        let title = text_rect(&view, "Add symbol");
        let close = text_rect(&view, "×");
        assert!(close.left() > title.right());
        assert!((close.center().y - title.center().y).abs() < 5.0);
        assert!(click(&mut dialog, &ctx, close.center()).outcome.close);
    }

    #[test]
    fn clear_button_only_with_text_resets_query_highlight_and_focus() {
        let ctx = egui::Context::default();
        let mut dialog = btc();
        snapshot(&mut dialog, &ctx, vec![], &[]);
        let view = snapshot(&mut dialog, &ctx, vec![], &[]);
        dialog.selected = 1;
        click(&mut dialog, &ctx, text_rect(&view, "⊗").center());
        assert!(dialog.query.is_empty());
        assert_eq!(dialog.selected, 0);
        assert!(ctx.memory(|m| m.focused().is_some()));
        let view = snapshot(&mut dialog, &ctx, vec![], &[]);
        assert!(!view.texts.iter().any(|(s, _)| s == "⊗"));
    }

    #[test]
    fn btc_markets_group_sources_and_choose_preferred_or_filtered_primary() {
        let mut dialog = btc();
        let groups = dialog.groups();
        assert_eq!(groups.len(), 2);
        let usd = groups.iter().find(|g| g.id == "BTC/USD").unwrap();
        assert_eq!(usd.members.len(), 1);
        assert_eq!(source(&usd.members[0]), "Coinbase");
        assert_eq!(usd.name(), "Bitcoin / US Dollar");
        let usdt = groups.iter().find(|g| g.id == "BTC/USDT").unwrap();
        assert_eq!(usdt.members.len(), 2);
        assert_eq!(source(&usdt.members[0]), SOURCE_ORDER[0]);
        dialog.source = "Binance".into();
        let groups = dialog.groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(source(&groups[0].members[0]), "Binance");
        assert_eq!(groups[0].members.len(), 2);
    }

    #[test]
    fn right_and_chevron_expand_left_collapses_and_subrows_add_exact_keys() {
        let ctx = egui::Context::default();
        let mut dialog = btc();
        dialog.query = "BTC/USDT".into();
        frame(&mut dialog, &ctx, None, false, &[]);
        frame(&mut dialog, &ctx, Some(egui::Key::ArrowRight), false, &[]);
        assert_eq!(dialog.visible_rows(&dialog.groups()).len(), 3);
        for member in 0..2 {
            dialog.selected = member + 1;
            frame(&mut dialog, &ctx, None, false, &[]);
            let (out, _) = frame(&mut dialog, &ctx, Some(egui::Key::Enter), false, &[]);
            let groups = dialog.groups();
            assert_eq!(
                groups.len(),
                1,
                "query: {:?}, groups: {:?}",
                dialog.query,
                groups.iter().map(|g| &g.id).collect::<Vec<_>>()
            );
            assert_eq!(out.key, Some(groups[0].members[member].key.clone()));
        }
        frame(&mut dialog, &ctx, Some(egui::Key::ArrowLeft), false, &[]);
        assert_eq!(dialog.selected, 0);
        assert_eq!(dialog.visible_rows(&dialog.groups()).len(), 1);
        let view = snapshot(&mut dialog, &ctx, vec![], &[]);
        let out = click(&mut dialog, &ctx, text_rect(&view, "›").center()).outcome;
        assert!(out.key.is_none());
        assert_eq!(dialog.visible_rows(&dialog.groups()).len(), 3);
        frame(&mut dialog, &ctx, Some(egui::Key::ArrowDown), false, &[]);
        assert_eq!(dialog.selected, 1);
        frame(&mut dialog, &ctx, Some(egui::Key::ArrowUp), false, &[]);
        assert_eq!(dialog.selected, 0);
    }

    #[test]
    fn group_trash_requires_primary_exact_key_and_subrows_track_each_key() {
        let ctx = egui::Context::default();
        let mut dialog = btc();
        dialog.query = "BTC/USDT".into();
        let groups = dialog.groups();
        let primary = groups[0].members[0].key.clone();
        let secondary = groups[0].members[1].key.clone();
        snapshot(&mut dialog, &ctx, vec![], &[]);
        let view = snapshot(&mut dialog, &ctx, vec![], std::slice::from_ref(&secondary));
        assert!(!view.texts.iter().any(|(s, _)| s == "🗑"));
        let view = snapshot(&mut dialog, &ctx, vec![], std::slice::from_ref(&primary));
        assert_eq!(view.texts.iter().filter(|(s, _)| s == "🗑").count(), 1);
        dialog.expanded.insert(groups[0].id.clone());
        let view = snapshot(&mut dialog, &ctx, vec![], &[secondary]);
        assert_eq!(view.texts.iter().filter(|(s, _)| s == "🗑").count(), 1);
        let view = snapshot(&mut dialog, &ctx, vec![], &[primary]);
        assert_eq!(view.texts.iter().filter(|(s, _)| s == "🗑").count(), 2);
    }

    #[test]
    fn sources_search_choice_and_escape_return_before_closing() {
        let ctx = egui::Context::default();
        let mut dialog = btc();
        snapshot(&mut dialog, &ctx, vec![], &[]);
        let view = snapshot(&mut dialog, &ctx, vec![], &[]);
        click(&mut dialog, &ctx, text_rect(&view, "All sources").center());
        assert!(dialog.sources_view);
        dialog.source_query = "coin".into();
        assert_eq!(dialog.source_choices().len(), 1);
        assert_eq!(dialog.source_choices()[0].id, "Coinbase");
        let view = snapshot(&mut dialog, &ctx, vec![], &[]);
        let choice = view.texts.iter().find(|(s, _)| s.starts_with("Coinbase\n")).unwrap().1;
        click(&mut dialog, &ctx, choice.center());
        assert_eq!(dialog.source, "Coinbase");
        assert!(!dialog.sources_view);
        dialog.sources_view = true;
        let (out, _) = frame(&mut dialog, &ctx, Some(egui::Key::Escape), false, &[]);
        assert!(!out.close);
        assert!(!dialog.sources_view);
        frame(&mut dialog, &ctx, None, false, &[]);
        assert!(frame(&mut dialog, &ctx, Some(egui::Key::Escape), false, &[]).0.close);
    }

    #[test]
    fn empty_states_are_centered_in_results_region() {
        let ctx = egui::Context::default();
        let mut dialog = Dialog::new(String::new(), None);
        dialog.source = "Not loaded".into();
        snapshot(&mut dialog, &ctx, vec![], &[]);
        let view = snapshot(&mut dialog, &ctx, vec![], &[]);
        let empty = text_rect(&view, "Type a symbol or name");
        assert!((empty.center().x - view.rect.center().x).abs() < 15.0);
        dialog.query = "NOSUCHPAIR".into();
        let view = snapshot(&mut dialog, &ctx, vec![], &[]);
        text_rect(&view, "No symbols match “NOSUCHPAIR”");
    }
}
