//! Concrete instrument discovery shared by watchlists and chart search.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use tr_core::AssetClass;
use tr_core::crypto::{CryptoInstrument, CryptoVenue};
use tr_data::SearchHit;

use crate::{theme, watch_sections, widgets};
use theme::{dp, mono, ui_font};
use widgets::Icon;

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

/// Familiar asset and currency names; unknown codes remain literal.
fn asset_name(code: &str) -> &str {
    match code {
        "BTC" => "Bitcoin",
        "ETH" => "Ethereum",
        "SOL" => "Solana",
        "XRP" => "XRP",
        "ADA" => "Cardano",
        "DOGE" => "Dogecoin",
        "USDT" => "Tether",
        "USDC" => "USD Coin",
        "USD" => "US Dollar",
        "EUR" => "Euro",
        "GBP" => "British Pound",
        "JPY" => "Japanese Yen",
        "BRL" => "Brazilian Real",
        "TRY" => "Turkish Lira",
        "INR" => "Indian Rupee",
        "AUD" => "Australian Dollar",
        "CAD" => "Canadian Dollar",
        _ => code,
    }
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
    if hit.key.to_uppercase() == query {
        return Some((0, 0, 0, 0));
    }
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
    FuturesExchange,
    BuiltIn,
}

impl SourceKind {
    fn label(self) -> &'static str {
        match self {
            Self::CentralizedExchange => "Centralized exchange · spot crypto",
            Self::DecentralizedExchange => "Decentralized exchange · spot crypto",
            Self::StockExchange => "Stock exchange",
            Self::FuturesExchange => "Futures exchange",
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
            || Self::new(name, exchange_kind(name), theme::muted()),
            |(_, kind, color)| Self::new(name, *kind, *color),
        )
    }
}

/// Provider exchange names are free text; the futures venues are recognisable by name.
fn exchange_kind(name: &str) -> SourceKind {
    let upper = name.to_uppercase();
    let futures = ["MERCANTILE", "BOARD OF TRADE", "CME", "CBOT", "NYMEX", "COMEX", "ICE FUTURES"];
    if futures.iter().any(|f| upper.contains(f)) { SourceKind::FuturesExchange } else { SourceKind::StockExchange }
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

fn badge_sources(group: &Group) -> Vec<String> {
    // Badges identify crypto venues; stock and catalog rows already name their source.
    let mut names: Vec<_> = group
        .members
        .iter()
        .map(source)
        .filter(|name| {
            matches!(
                SourceInfo::from_name(name).kind,
                SourceKind::CentralizedExchange | SourceKind::DecentralizedExchange
            )
        })
        .collect();
    names.dedup();
    names
}

#[derive(Clone, Copy)]
struct VisibleRow {
    group: usize,
    member: Option<usize>,
}

struct RowColumns {
    ticker: egui::Rect,
    name: egui::Rect,
    label: egui::Rect,
    badges: egui::Rect,
    chevron: egui::Rect,
    action: egui::Rect,
}

impl RowColumns {
    fn new(row: egui::Rect, subrow: bool) -> Self {
        let slot =
            |left, width| egui::Rect::from_min_size(egui::pos2(left, row.top()), egui::vec2(width, row.height()));
        let action = slot(row.right() - dp(8.0 + 28.0), dp(28.0));
        let chevron = slot(action.left() - dp(22.0), dp(22.0));
        let badges = slot(chevron.left() - dp(90.0), dp(90.0));
        let label = slot(badges.left() - dp(150.0 + 8.0), dp(150.0));
        let ticker = slot(row.left() + dp(12.0) + if subrow { dp(24.0) } else { 0.0 }, dp(120.0));
        let name_left = ticker.right() + dp(12.0);
        let name = slot(name_left, (label.left() - dp(12.0) - name_left).max(0.0));
        Self { ticker, name, label, badges, chevron, action }
    }
}

fn elided(
    ui: &egui::Ui,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.into(), font, color);
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    ui.painter().layout_job(job)
}

fn row_text(ui: &egui::Ui, rect: egui::Rect, text: &str, font: egui::FontId, color: egui::Color32, right: bool) {
    let galley = elided(ui, text, font, color, rect.width());
    let x = if right { rect.right() - galley.size().x } else { rect.left() };
    ui.painter().galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley, color);
}

fn row_icon(ui: &mut egui::Ui, rect: egui::Rect, icon: Icon, tooltip: &str) -> egui::Response {
    let rect = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), rect.height().min(24.0)));
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
    widgets::icon_button(&mut child, icon, tooltip, false)
}

fn filter_label<'a>(value: &'a str, all: &'a str) -> &'a str {
    if value.is_empty() { all } else { value }
}

fn filter_button(ui: &mut egui::Ui, value: &str, all: &str, tooltip: &str) -> egui::Response {
    let label = filter_label(value, all);
    let galley = ui.painter().layout_no_wrap(label.into(), ui_font(12.0), theme::text());
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(galley.size().x + dp(38.0), dp(26.0)), egui::Sense::click());
    let fill = if !value.is_empty() {
        theme::accent_wash()
    } else if response.hovered() {
        theme::control_hover()
    } else {
        theme::control()
    };
    ui.painter().rect_filled(rect, theme::CONTROL_RADIUS, fill);
    ui.painter().galley(
        egui::pos2(rect.left() + dp(10.0), rect.center().y - galley.size().y / 2.0),
        galley,
        theme::text(),
    );
    widgets::draw_icon(
        ui.painter(),
        egui::Rect::from_center_size(
            egui::pos2(rect.right() - dp(12.0), rect.center().y),
            egui::vec2(dp(14.0), dp(14.0)),
        ),
        Icon::ChevronDown,
        theme::muted(),
    );
    response.on_hover_text(tooltip)
}

fn footer(ui: &mut egui::Ui, adding: bool) {
    let parts: &[(bool, &str)] = if adding {
        &[
            (true, "⇧"),
            (false, " + "),
            (true, "Click"),
            (false, " or "),
            (true, "⇧"),
            (false, " + "),
            (true, "Enter"),
            (false, " to add symbol and close dialog"),
        ]
    } else {
        &[
            (true, "Enter"),
            (false, " to open · "),
            (true, "⇧"),
            (false, " "),
            (true, "Enter"),
            (false, " to open and close"),
        ]
    };
    let widths: Vec<_> = parts
        .iter()
        .map(|(cap, text)| {
            ui.painter()
                .layout_no_wrap((*text).into(), if *cap { mono(10.0) } else { ui_font(11.0) }, theme::muted())
                .size()
                .x
                + if *cap { 8.0 } else { 0.0 }
        })
        .collect();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), dp(24.0)), egui::Sense::hover());
    let mut x = rect.center().x - widths.iter().sum::<f32>() / 2.0;
    for ((cap, text), width) in parts.iter().zip(widths) {
        let pos = egui::pos2(x, rect.center().y);
        if *cap {
            widgets::keycap(ui.painter(), pos, text);
        } else {
            ui.painter().text(pos, egui::Align2::LEFT_CENTER, text, ui_font(11.0), theme::muted());
        }
        x += width;
    }
    response.on_hover_text("↑/↓ select · → expand sources · ← collapse · Enter activates · Esc closes");
}

fn search_field(ui: &mut egui::Ui, text: &mut String, autofocus: &mut bool, hint: &str) -> bool {
    let mut changed = false;
    egui::Frame::new()
        .fill(theme::header())
        .corner_radius(theme::CONTROL_RADIUS)
        .inner_margin(egui::Margin::symmetric(dp(5.0) as i8, dp(4.0) as i8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // Drawn icons: the UI font has no magnifier or circled-x glyphs.
                let (icon, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                crate::widgets::draw_icon(ui.painter(), icon, crate::widgets::Icon::Search, theme::muted());
                let width = (ui.available_width() - 30.0).max(20.0);
                let field = ui.add_sized(
                    [width, dp(24.0)],
                    egui::TextEdit::singleline(text).frame(egui::Frame::NONE).hint_text(hint),
                );
                if *autofocus {
                    field.request_focus();
                    *autofocus = false;
                }
                if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    field.request_focus();
                }
                changed = field.changed();
                let clear = (!text.is_empty())
                    .then(|| crate::widgets::icon_button(ui, crate::widgets::Icon::Close, "Clear search", false));
                #[cfg(test)]
                if let Some(clear) = &clear {
                    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("symbol-search-clear"), clear.rect));
                }
                if clear.is_some_and(|clear| clear.clicked_by(egui::PointerButton::Primary)) {
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
        // A pasted market key ("AAPL US Equity", "SPX Index") names one instrument exactly,
        // which free-text matching cannot find.
        let tokens: Vec<&str> = self.query.split_whitespace().collect();
        if tokens.len() > 1
            && let Some((security, used)) = tr_core::Security::resolve_tokens(&tokens)
            && used == tokens.len()
        {
            hits.push(SearchHit {
                key: security.key(),
                name: security.ticker.clone(),
                class: security.class,
                exchange: None,
            });
        }
        for hit in &mut hits {
            let security = tr_data::security(&hit.key);
            hit.key = security.crypto_instrument().map_or_else(
                |_| security.key(),
                |c| {
                    hit.name = format!("{} / {}", asset_name(&c.base), asset_name(&c.quote));
                    c.security().key()
                },
            );
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
        if search_field(ui, &mut self.source_query, &mut self.autofocus, "Search sources") {
            self.selected = 0;
        }
        egui::ScrollArea::vertical().id_salt("source-list").auto_shrink([false, false]).show(ui, |ui| {
            let mut choices = self.source_choices();
            if "all sources".contains(&self.source_query.trim().to_lowercase()) {
                choices.insert(0, SourceInfo::new("All sources", SourceKind::BuiltIn, theme::muted()));
                choices[0].id.clear();
            }
            let column_width = (ui.available_width() - 12.0) / 2.0;
            egui::Grid::new("sources-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                for (index, info) in choices.iter().enumerate() {
                    let (rect, response) = ui.allocate_exact_size(egui::vec2(column_width, 48.0), egui::Sense::click());
                    let selected = self.source == info.id;
                    widgets::hover_wash(ui, rect, &response, selected);
                    let painter = ui.painter_at(rect);
                    let badge = egui::pos2(rect.left() + 22.0, rect.center().y);
                    if !info.id.is_empty() {
                        painter.circle_filled(badge, 12.0, info.badge.1);
                        painter.text(badge, egui::Align2::CENTER_CENTER, info.badge.0, ui_font(11.0), theme::card());
                    }
                    let left = rect.left() + if info.id.is_empty() { 12.0 } else { 44.0 };
                    let (name_y, kind_y) = if info.id.is_empty() {
                        (rect.center().y, None)
                    } else {
                        (rect.center().y - 8.0, Some(rect.center().y + 9.0))
                    };
                    painter.text(
                        egui::pos2(left, name_y),
                        egui::Align2::LEFT_CENTER,
                        &info.name,
                        ui_font(13.5),
                        theme::text(),
                    );
                    if let Some(y) = kind_y {
                        painter.text(
                            egui::pos2(left, y),
                            egui::Align2::LEFT_CENTER,
                            info.kind.label(),
                            ui_font(11.0),
                            theme::muted(),
                        );
                    }
                    let tip = if info.id.is_empty() {
                        "Show symbols from every source".to_owned()
                    } else {
                        format!("Show symbols from {}", info.name)
                    };
                    if response.on_hover_text(tip).clicked() {
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
        if search_field(ui, &mut self.query, &mut self.autofocus, "Symbol or name") {
            self.network.clear();
            self.pending = Some(Instant::now());
            self.selected = 0;
            self.expanded.clear();
        }
        ui.add_space(dp(10.0));
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
        ui.add_space(dp(6.0));
        ui.horizontal(|ui| {
            if filter_button(ui, &self.source, "All sources", "Filter by source").clicked() {
                self.sources_view = true;
                self.source_query.clear();
                self.autofocus = true;
            }
            let mut types: Vec<_> = self.candidates().iter().map(|h| kind(h.class)).collect();
            types.sort();
            types.dedup();
            let response = filter_button(ui, &self.kind, "All types", "Filter by type");
            egui::Popup::menu(&response).show(|ui| {
                if ui
                    .selectable_value(&mut self.kind, String::new(), "All types")
                    .on_hover_text("Show all types")
                    .clicked()
                {
                    self.selected = 0;
                    ui.close();
                }
                for t in types {
                    if ui
                        .selectable_value(&mut self.kind, t.into(), t)
                        .on_hover_text(format!("Show only {t}"))
                        .clicked()
                    {
                        self.selected = 0;
                        ui.close();
                    }
                }
            });
        });
        ui.add_space(dp(10.0));
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
        ui.add_space(dp(8.0));
        ui.separator();
        footer(ui, self.target.is_some());
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
        let subrow = row.member.is_some();
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), dp(if subrow { 30.0 } else { 36.0 })),
            egui::Sense::click(),
        );
        widgets::hover_wash(ui, rect, &response, index == self.selected);
        let columns = RowColumns::new(rect, subrow);
        let ticker_color = if !subrow {
            theme::amber()
        } else if index == self.selected || response.hovered() {
            theme::text()
        } else {
            theme::muted()
        };
        row_text(ui, columns.ticker, &ticker, mono(if subrow { 12.0 } else { 12.5 }), ticker_color, false);
        row_text(ui, columns.name, if subrow { &hit.name } else { group.name() }, ui_font(12.5), theme::text(), false);
        row_text(
            ui,
            columns.label,
            &format!("{} · {}", kind(hit.class), source(hit)),
            ui_font(11.0),
            theme::muted(),
            true,
        );
        if !subrow {
            let names = badge_sources(group);
            let extra = (names.len() > 3).then(|| format!("+{}", names.len() - 3));
            let extra_width = extra.as_ref().map_or(0.0, |s| {
                ui.painter().layout_no_wrap(s.clone(), ui_font(11.0), theme::muted()).size().x + dp(4.0)
            });
            let count = names.len().min(3);
            let mut x = columns.badges.right() - count as f32 * dp(22.0) + dp(4.0) - extra_width;
            for name in names.iter().take(3) {
                let info = SourceInfo::from_name(name);
                let badge = egui::Rect::from_center_size(
                    egui::pos2(x + dp(9.0), rect.center().y),
                    egui::vec2(dp(18.0), dp(18.0)),
                );
                ui.painter().circle_filled(badge.center(), dp(9.0), info.badge.1);
                ui.painter().text(
                    badge.center(),
                    egui::Align2::CENTER_CENTER,
                    info.badge.0,
                    ui_font(11.0),
                    egui::Color32::BLACK,
                );
                ui.interact(badge, response.id.with(name), egui::Sense::hover()).on_hover_text(info.name);
                x += dp(22.0);
            }
            if let Some(extra) = extra {
                ui.painter().text(
                    egui::pos2(columns.badges.right(), rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    extra,
                    ui_font(11.0),
                    theme::muted(),
                );
            }
        }
        if !subrow && group.members.len() > 1 {
            let expanded = self.expanded.contains(&group.id);
            let tooltip =
                if expanded { "Hide sources".into() } else { format!("Show all {} sources", group.members.len()) };
            let chevron =
                row_icon(ui, columns.chevron, if expanded { Icon::ChevronDown } else { Icon::ChevronRight }, &tooltip);
            #[cfg(test)]
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("symbol-chevron", index)), chevron.rect));
            if chevron.clicked() {
                self.selected = index;
                if expanded {
                    self.expanded.remove(&group.id);
                } else {
                    self.expanded.insert(group.id.clone());
                }
            }
        }
        let tooltip = if remove {
            format!("Remove {ticker} from {}", self.list_name)
        } else {
            format!("Add {ticker} from {}", source(hit))
        };
        let action = row_icon(ui, columns.action, if remove { Icon::Trash } else { Icon::Plus }, &tooltip);
        #[cfg(test)]
        ui.ctx().data_mut(|d| {
            d.insert_temp(egui::Id::new(("symbol-action", index)), action.rect);
            d.insert_temp(egui::Id::new(("symbol-remove", index)), remove);
        });
        let clicked = action.clicked();
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
            .frame(
                egui::Frame::new()
                    .fill(theme::card())
                    .corner_radius(12)
                    .inner_margin(egui::Margin {
                        left: dp(16.0) as i8,
                        right: dp(16.0) as i8,
                        top: dp(14.0) as i8,
                        bottom: dp(18.0) as i8,
                    })
                    .shadow(egui::Shadow {
                        offset: [0, 8],
                        blur: 24,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(90),
                    }),
            )
            .show(ctx, |ui| {
                ui.set_min_size(size);
                ui.set_max_size(size);
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.horizontal(|ui| {
                    if self.sources_view && widgets::icon_button(ui, Icon::ChevronLeft, "Back", false).clicked() {
                        self.sources_view = false;
                        self.autofocus = true;
                    }
                    ui.label(
                        egui::RichText::new(if self.sources_view {
                            "Sources"
                        } else if self.target.is_some() {
                            "Add symbol"
                        } else {
                            "Symbol search"
                        })
                        .font(ui_font(16.0))
                        .strong()
                        .color(theme::text()),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let close = widgets::icon_button(ui, Icon::Close, "Close (Esc)", false);
                        #[cfg(test)]
                        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("symbol-search-close"), close.rect));
                        out.close |= close.clicked();
                    });
                });
                ui.add_space(dp(10.0));
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
        frame(&mut dialog, &ctx, None, false, &present);
        assert_eq!(rendered_removals(&dialog, &ctx), 1);
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
        let close = ctx.data(|d| d.get_temp::<egui::Rect>(egui::Id::new("symbol-search-close"))).unwrap();
        assert!(close.left() > title.right());
        assert!((close.center().y - title.center().y).abs() < 5.0);
        assert!(click(&mut dialog, &ctx, close.center()).outcome.close);
    }

    #[test]
    fn clear_button_only_with_text_resets_query_highlight_and_focus() {
        let ctx = egui::Context::default();
        let mut dialog = btc();
        snapshot(&mut dialog, &ctx, vec![], &[]);
        snapshot(&mut dialog, &ctx, vec![], &[]);
        dialog.selected = 1;
        let clear = ctx.data(|d| d.get_temp::<egui::Rect>(egui::Id::new("symbol-search-clear"))).expect("clear button");
        click(&mut dialog, &ctx, clear.center());
        assert!(dialog.query.is_empty());
        assert_eq!(dialog.selected, 0);
        assert!(ctx.memory(|m| m.focused().is_some()));
        ctx.data_mut(|d| d.remove::<egui::Rect>(egui::Id::new("symbol-search-clear")));
        snapshot(&mut dialog, &ctx, vec![], &[]);
        assert!(
            ctx.data(|d| d.get_temp::<egui::Rect>(egui::Id::new("symbol-search-clear"))).is_none(),
            "no clear button without text"
        );
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
        snapshot(&mut dialog, &ctx, vec![], &[]);
        let chevron = ctx.data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("symbol-chevron", 0_usize)))).unwrap();
        let out = click(&mut dialog, &ctx, chevron.center()).outcome;
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
        snapshot(&mut dialog, &ctx, vec![], std::slice::from_ref(&secondary));
        assert_eq!(rendered_removals(&dialog, &ctx), 0);
        snapshot(&mut dialog, &ctx, vec![], std::slice::from_ref(&primary));
        assert_eq!(rendered_removals(&dialog, &ctx), 1);
        dialog.expanded.insert(groups[0].id.clone());
        snapshot(&mut dialog, &ctx, vec![], &[secondary]);
        assert_eq!(rendered_removals(&dialog, &ctx), 1);
        snapshot(&mut dialog, &ctx, vec![], &[primary]);
        assert_eq!(rendered_removals(&dialog, &ctx), 2);
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
        let choice = view.texts.iter().find(|(s, _)| s == "Coinbase").unwrap().1;
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

    #[test]
    fn space_pair_query_surfaces_crypto_pairs() {
        for (query, expect) in [("xrp usd", "COINBASE:XRP-USD"), ("XRP USDT", "BINANCE:XRPUSDT")] {
            let mut dialog = Dialog::new(query.into(), Some(Target::Append));
            for (venue, key) in [(CryptoVenue::Binance, "BINANCE:XRPUSDT"), (CryptoVenue::Coinbase, "COINBASE:XRP-USD")]
            {
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

    fn rendered_removals(dialog: &Dialog, ctx: &egui::Context) -> usize {
        (0..dialog.visible_rows(&dialog.groups()).len())
            .filter(|index| ctx.data(|d| d.get_temp::<bool>(egui::Id::new(("symbol-remove", *index)))).unwrap_or(false))
            .count()
    }

    #[test]
    fn action_and_ticker_columns_align_across_source_counts_and_chevrons() {
        let ctx = egui::Context::default();
        let mut dialog = btc();
        let pair = |key: &str| SearchHit {
            key: tr_data::security(key).key(),
            name: "Bitcoin / Tether".into(),
            class: AssetClass::Crypto,
            exchange: None,
        };
        let mut groups = [
            Group { id: "one".into(), members: vec![hit("A", "Alpha")] },
            Group { id: "three".into(), members: vec![hit("B", "Beta"), hit("C", "Gamma"), hit("D", "Delta")] },
            Group { id: "venues".into(), members: vec![pair("COINBASE:BTC-USDT"), pair("BINANCE:BTCUSDT")] },
        ];
        groups[1].members[1].exchange = Some("NYSE".into());
        groups[1].members[2].exchange = Some("LSE".into());
        assert!(badge_sources(&groups[0]).is_empty() && badge_sources(&groups[1]).is_empty());
        assert_eq!(badge_sources(&groups[2]).len(), 2, "crypto venues keep their badges");
        let mut actions = Vec::new();
        let mut tickers = Vec::new();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            for (index, group) in groups.iter().enumerate() {
                let mut out = Outcome { key: None, close: false };
                dialog.show_row(ui, group, VisibleRow { group: index, member: None }, index, &[], &mut out);
                actions.push(ctx.data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("symbol-action", index)))).unwrap());
            }
            let mut out = Outcome { key: None, close: false };
            dialog.show_row(
                ui,
                &groups[1],
                VisibleRow { group: 1, member: Some(1) },
                2,
                &[groups[1].members[1].key.clone()],
                &mut out,
            );
            actions.push(ctx.data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("symbol-action", 2_usize)))).unwrap());
            dialog.show_row(ui, &groups[2], VisibleRow { group: 2, member: None }, 3, &[], &mut out);
            actions.push(ctx.data(|d| d.get_temp::<egui::Rect>(egui::Id::new(("symbol-action", 3_usize)))).unwrap());
        });
        for shape in &output.shapes {
            if let egui::Shape::Text(t) = &shape.shape
                && ["A", "B"].contains(&t.galley.job.text.as_str())
                // Venue badges draw letters too; tickers live in the left column.
                && t.pos.x < dp(60.0)
            {
                tickers.push(t.pos.x);
            }
        }
        output.textures_delta.clear();
        assert!(actions.iter().all(|rect| rect.left() == actions[0].left()));
        assert_eq!(tickers.len(), 2);
        assert_eq!(tickers[0], tickers[1]);
    }

    #[test]
    fn long_name_is_elided_before_right_cluster() {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let row = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(728.0, 36.0));
            let columns = RowColumns::new(row, false);
            let name = "Grayscale Bitcoin Mini Trust (BTC) with a very long descriptive name";
            let galley = elided(ui, name, ui_font(12.5), theme::text(), columns.name.width());
            assert!(galley.elided);
            assert_eq!(galley.rows[0].glyphs.last().unwrap().chr, '…');
            assert!(columns.name.left() + galley.size().x <= columns.label.left());
            assert_eq!(columns.ticker.left(), row.left() + dp(12.0));
        });
        output.textures_delta.clear();
    }

    #[test]
    fn only_crypto_venues_get_badges() {
        let mut member = hit("A", "Alpha");
        member.exchange = Some("Built-in".into());
        let mut group = Group { id: "A".into(), members: vec![member] };
        assert!(badge_sources(&group).is_empty());
        let ctx = egui::Context::default();
        let mut dialog = btc();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            dialog.show_row(
                ui,
                &group,
                VisibleRow { group: 0, member: None },
                0,
                &[],
                &mut Outcome { key: None, close: false },
            );
        });
        assert!(!output.shapes.iter().any(|s| matches!(s.shape, egui::Shape::Circle(_))));
        assert!(
            output
                .shapes
                .iter()
                .any(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.job.text == "stock · Built-in"))
        );
        output.textures_delta.clear();
        group.members.push(hit("B", "Beta"));
        assert!(badge_sources(&group).is_empty(), "stock rows name their exchange in the label");
        group.members.push(SearchHit {
            key: tr_data::security("BINANCE:BTCUSDT").key(),
            name: "Bitcoin / Tether".into(),
            class: AssetClass::Crypto,
            exchange: None,
        });
        assert_eq!(badge_sources(&group), ["Binance"]);
    }

    #[test]
    fn filters_render_active_values_and_footer_renders_keycaps() {
        let ctx = egui::Context::default();
        let mut dialog = btc();
        dialog.source = "Binance".into();
        dialog.kind = "spot crypto".into();
        frame(&mut dialog, &ctx, None, false, &[]);
        let (_, labels) = frame(&mut dialog, &ctx, None, false, &[]);
        for value in ["Binance", "spot crypto", "⇧", "Click", "Enter"] {
            assert!(labels.iter().any(|label| label == value), "Missing {value}");
        }
        assert!(!labels.iter().any(|label| label == "All sources" || label == "All types"));
    }

    #[test]
    fn type_filter_popup_selects_value_and_closes() {
        let ctx = egui::Context::default();
        let mut dialog = btc();
        snapshot(&mut dialog, &ctx, vec![], &[]);
        let view = snapshot(&mut dialog, &ctx, vec![], &[]);
        click(&mut dialog, &ctx, text_rect(&view, "All types").center());
        let view = snapshot(&mut dialog, &ctx, vec![], &[]);
        let choice = text_rect(&view, "spot crypto");
        dialog.selected = 1;
        click(&mut dialog, &ctx, choice.center());
        assert_eq!(dialog.kind, "spot crypto");
        assert_eq!(dialog.selected, 0);
        let view = snapshot(&mut dialog, &ctx, vec![], &[]);
        assert!(!view.texts.iter().any(|(text, _)| text == "All types"));
    }

    #[test]
    fn crypto_names_use_known_assets_and_literal_unknown_codes() {
        let mut dialog = btc();
        dialog.catalog(CryptoVenue::Binance, Ok(vec![CryptoInstrument::parse("BINANCE:PEPEUSDT").unwrap()]));
        dialog.query.clear();
        let hits = dialog.candidates();
        assert!(hits.iter().any(|h| h.name == "Bitcoin / Tether"));
        assert!(hits.iter().any(|h| h.name == "PEPE / Tether"));
        assert_eq!(asset_name("CAD"), "Canadian Dollar");
    }

    #[test]
    fn full_market_key_query_finds_the_instrument() {
        for query in ["AAPL US Equity", "msft us equity", "SPX Index"] {
            let dialog = Dialog::new(query.into(), Some(Target::Append));
            let hits = dialog.results();
            assert!(
                !hits.is_empty(),
                "{query:?} must resolve to its instrument, got {:?}",
                hits.iter().map(|h| &h.key).collect::<Vec<_>>()
            );
        }
    }
}
