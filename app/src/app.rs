//! Top-level terminal: command line, docked panes, saved layouts, hotkeys and status bar.
//!
//! Submodules: `chrome` (toolbar, sidebar, status bar), `workspace` (dock rendering,
//! drag and drop, pop-out windows, link groups, saved layouts), `panels` (per-kind pane
//! content) and `settings_view` (preferences UI).

#[path = "agent_bridge.rs"]
pub mod agent_bridge;
mod agent_ui;
mod broker;
mod chrome;
mod columns_view;
pub(crate) mod data_sources;
mod demo;
mod demo_harness;
mod hotkeys_view;
mod options_view;
mod palette_view;
mod panels;
mod portfolio_view;
mod scanner_view;
mod settings_view;
mod theming;
mod trading;
mod watchlist_drag;
mod workspace;

pub use broker::{PosSort, questrade_handle};

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::{Color32, Event, Key, Modifiers, Rect, Ui, UiBuilder, pos2, vec2};
use serde::{Deserialize, Serialize};
use tr_chart::drawings::Tool;
use tr_congress::Trade;
use tr_congress::db::{Alert, PoliticianSummary};
use tr_data::{OrderBook, Quote, SearchHit, YieldCurves};

use crate::anim;
use crate::chart_view::ChartState;
use crate::command::{self, Command, Function};
use crate::congress_view::{Action, FeedFilter};
use crate::dock::{Node, PaneId};
use crate::layout::Layout;
use crate::layouts::{LayoutDoc, Link, PaneSettings, Store, WindowGeometry};
use crate::markets_view;
use crate::prefs::{self, Prefs};
use crate::theme;
use crate::worker::{Profile, Request, Response, Worker, data_dir};

use chrome::{SIDEBAR_W, STATUS_H, SidebarCmd};
use settings_view::SettingsTab;

type SaveWrite = (std::path::PathBuf, Vec<u8>);
type SaveResult = (SaveWrite, std::io::Result<()>);
type SaveWriter = std::sync::Arc<dyn Fn(&std::path::Path, &[u8]) -> std::io::Result<()> + Send + Sync>;

struct SaveState {
    written: HashMap<std::path::PathBuf, Vec<u8>>,
    job: Option<std::thread::JoinHandle<Vec<SaveResult>>>,
    writer: SaveWriter,
}

impl Default for SaveState {
    fn default() -> Self {
        Self { written: HashMap::new(), job: None, writer: std::sync::Arc::new(crate::layouts::write_atomic) }
    }
}

impl SaveState {
    fn changed(&self, writes: Vec<SaveWrite>) -> Vec<SaveWrite> {
        writes.into_iter().filter(|(path, bytes)| self.written.get(path) != Some(bytes)).collect()
    }

    fn write(writes: Vec<SaveWrite>, writer: &SaveWriter) -> Vec<SaveResult> {
        writes
            .into_iter()
            .map(|write| {
                let result = writer(&write.0, &write.1);
                (write, result)
            })
            .collect()
    }

    fn start(&mut self, writes: Vec<SaveWrite>) {
        let writes = self.changed(writes);
        if !writes.is_empty() {
            let writer = self.writer.clone();
            self.job = Some(std::thread::spawn(move || Self::write(writes, &writer)));
        }
    }
}

pub enum PaneKind {
    Chart(Box<ChartState>),
    Feed {
        filter: FeedFilter,
    },
    Ticker {
        symbol: String,
        trades: Vec<Trade>,
        loading: bool,
    },
    Profile {
        query: String,
        data: Option<Box<Profile>>,
        loading: bool,
    },
    Politicians,
    Alerts,
    Watchlist,
    Scanner(Box<scanner_view::ScannerPane>),
    Help,
    Wei,
    Curve,
    Search {
        query: String,
    },
    News(Box<NewsPane>),
    // N2
    Filings(crate::filings_view::Settings),
    OrderBook(Box<BookPane>),
    Options(Box<OptionsPane>),
    /// Brokerage accounts through SnapTrade (`PORT`, `ACCT`, `POS`, `ORD`, `ACTV`).
    Portfolio(Box<broker::PortfolioPane>),
    Settings,
    Agent(crate::agent_pane::Settings),
}

pub struct OptionsPane {
    pub symbol: String,
    /// Selected expiration (`None` = nearest).
    pub expiration: Option<i64>,
    pub chain: Option<Box<tr_data::OptionChain>>,
    /// Greeks per call and put, aligned with the chain's contracts.
    pub call_greeks: Vec<Option<tr_core::options::Greeks>>,
    pub put_greeks: Vec<Option<tr_core::options::Greeks>>,
    /// Risk-free rate used for the greeks.
    pub rate: f64,
    pub error: Option<String>,
    pub loading: bool,
    pub fetched: Option<Instant>,
    /// Strikes each side of the money (0 = all).
    pub strikes: usize,
    pub show_greeks: bool,
}

impl OptionsPane {
    pub fn new(symbol: &str, strikes: usize, show_greeks: bool) -> Self {
        Self {
            symbol: symbol.into(),
            expiration: None,
            chain: None,
            call_greeks: Vec::new(),
            put_greeks: Vec::new(),
            rate: 0.04,
            error: None,
            loading: true,
            fetched: None,
            strikes,
            show_greeks,
        }
    }

    /// Store a chain and derive greeks from each contract's implied volatility.
    pub fn set_chain(&mut self, chain: Box<tr_data::OptionChain>, rate: f64, now: i64) {
        // Expiry at the 16:00 New York close (≈ 20:00 UTC) of the expiration date.
        let t = ((chain.expiration + 20 * 3600 - now) as f64 / (365.0 * 86_400.0)).max(0.5 / 365.0);
        let g = |call: bool, c: &tr_data::OptionContract| {
            tr_core::options::black_scholes(call, chain.spot, c.strike, t, rate, chain.dividend_yield, c.iv?)
                .map(|(_, g)| g)
        };
        self.call_greeks = chain.calls.iter().map(|c| g(true, c)).collect();
        self.put_greeks = chain.puts.iter().map(|c| g(false, c)).collect();
        self.rate = rate;
        if self.expiration.is_none() || !chain.expirations.contains(&chain.expiration) {
            self.expiration = Some(chain.expiration);
        }
        self.chain = Some(chain);
        self.error = None;
        self.loading = false;
    }
}

// N1b
pub(crate) mod news_view;
pub struct NewsPane {
    pub symbol: Option<String>,
    pub items: Vec<tr_data::news_store::Story>,
    pub filters: tr_data::news_store::Filters,
    pub paused: bool,
    pub pending: Vec<tr_data::news_store::Story>,
    pub status: String,
    pub updated: Option<Instant>,
    pub error: Option<String>,
    pub loading: bool,
    pub fetched: Option<Instant>,
    pub query: Option<crate::worker::NewsView>,
    pub display: news_view::Display,
    pub interaction: news_view::Interaction,
    pub seen_queue: Vec<String>,
    // N1b: revisions invalidate presentation once, never by scanning on repaint.
    pub item_revision: u64,
    pub cache: news_view::RowCache,
    pub read_in_view: std::collections::BTreeSet<String>,
    pub local_seen: std::collections::BTreeSet<String>,
}
impl NewsPane {
    pub fn new(symbol: Option<String>) -> Self {
        Self {
            symbol,
            items: vec![],
            filters: Default::default(),
            paused: false,
            pending: vec![],
            status: "Showing saved headlines".into(),
            updated: None,
            error: None,
            loading: true,
            fetched: None,
            query: None,
            display: Default::default(),
            interaction: Default::default(),
            seen_queue: vec![],
            item_revision: 0,
            cache: Default::default(),
            read_in_view: Default::default(),
            local_seen: Default::default(),
        }
    }
    fn apply(&mut self, mut items: Vec<tr_data::news_store::Story>) {
        // Seen is monotonic. A reply already in flight before the seen write
        // cannot undo a user action, including across scope changes.
        for item in &mut items {
            item.seen |= self.local_seen.contains(&item.id);
        }
        self.item_revision = self.item_revision.wrapping_add(1);
        if self.paused {
            if !items.is_empty() {
                self.pending = items;
            }
        } else {
            if items.len() != self.items.len()
                || items.iter().zip(&self.items).any(|(a, b)| {
                    a.id != b.id
                        || a.item != b.item
                        || a.source != b.source
                        || a.tags != b.tags
                        || a.summary != b.summary
                })
            {
                self.read_in_view.clear();
            }
            // Keep the retained snapshot on empty queries (N1a).
            if !items.is_empty() {
                self.items = items;
            }
            self.pending.clear();
        }
    }
    fn empty_message(&self, query: &tr_data::news_store::Query) -> Option<String> {
        if self.items.iter().any(|story| tr_data::news_store::matches(story, query)) {
            return None;
        }
        let selection = if self.filters.scope == tr_data::news_store::Scope::Symbol {
            self.symbol.as_deref().unwrap_or("this selection")
        } else {
            "this selection"
        };
        Some(format!("No saved headlines for {selection} yet"))
    }
    fn new_count(&self) -> usize {
        self.pending
            .iter()
            .filter(|s| {
                let visible = self.query.as_ref().is_none_or(|view| {
                    let days = view.filters.days.clamp(1, 90) as i64;
                    let now = chrono::Utc::now().timestamp();
                    let query = tr_data::news_store::Query {
                        symbols: view.symbols.clone(),
                        filters: view.filters.clone(),
                        start: if days == 1 { tr_data::news_store::day_start(now) } else { now - days * 86400 },
                        ..Default::default()
                    };
                    tr_data::news_store::matches(s, &query)
                });
                visible && !self.items.iter().any(|old| old.id == s.id)
            })
            .count()
    }
}
// N1b

pub struct BookPane {
    pub symbol: String,
    pub depth: usize,
    /// Price bucket; 0 = raw levels.
    pub group: f64,
    pub book: Option<OrderBook>,
    pub error: Option<String>,
    pub fetched: Option<Instant>,
}

impl BookPane {
    pub fn new(symbol: &str, depth: usize, group: f64) -> Self {
        Self { symbol: symbol.into(), depth, group, book: None, error: None, fetched: None }
    }
}

impl PaneKind {
    /// Short tab title.
    fn tab_title(&self) -> String {
        let short = |s: &str| crate::security_names::ticker(&tr_data::security(s), false);
        match self {
            PaneKind::Chart(c) => short(&c.symbol),
            PaneKind::Feed { .. } => "Congress".into(),
            PaneKind::Ticker { symbol, .. } => format!("{} Congress", short(symbol)),
            PaneKind::Profile { query, data, .. } => {
                data.as_ref().map_or_else(|| query.clone(), |p| p.politician.name.clone())
            }
            PaneKind::Politicians => "Politicians".into(),
            PaneKind::Alerts => "Alerts".into(),
            PaneKind::Watchlist => "Watchlist".into(),
            PaneKind::Scanner(s) => format!("Scan: {}", s.definition.name),
            PaneKind::Help => "Help".into(),
            PaneKind::Wei => "World Markets".into(),
            PaneKind::Curve => "Yield Curve".into(),
            PaneKind::Search { query } if query.is_empty() => "Search".into(),
            PaneKind::Search { query } => format!("Search: {query}"),
            PaneKind::News(n) => match &n.symbol {
                Some(s) => format!("{} News", short(s)),
                None => "Top News".into(),
            },
            // N2
            PaneKind::Filings(f) => {
                f.symbol.as_deref().map_or_else(|| "Filings".into(), |s| format!("{} Filings", short(s)))
            }
            PaneKind::OrderBook(b) => format!("{} Depth", short(&b.symbol)),
            PaneKind::Options(o) => format!("{} Options", short(&o.symbol)),
            PaneKind::Portfolio(p) => p.title(),
            PaneKind::Settings => "Settings".into(),
            PaneKind::Agent(_) => "Agent".into(),
        }
    }

    /// The Bloomberg-style function code shown on the tab.
    fn code(&self) -> &'static str {
        match self {
            PaneKind::Chart(_) => "GP",
            PaneKind::Feed { .. } | PaneKind::Ticker { .. } => "CONG",
            PaneKind::Profile { .. } | PaneKind::Politicians => "POL",
            PaneKind::Alerts => "ALRT",
            PaneKind::Watchlist => "WL",
            PaneKind::Scanner(_) => "SCAN",
            PaneKind::Help => "HELP",
            PaneKind::Wei => "WEI",
            PaneKind::Curve => "GC",
            PaneKind::Search { .. } => "SECF",
            PaneKind::News(_) => "CN",
            // N2
            PaneKind::Filings(_) => "CF",
            PaneKind::OrderBook(_) => "OB",
            PaneKind::Options(_) => "OMON",
            PaneKind::Portfolio(p) => p.view.code(),
            PaneKind::Settings => "PDF",
            PaneKind::Agent(_) => "AGENT",
        }
    }

    /// The security this pane follows, for symbol-driven panels.
    fn symbol(&self) -> Option<&str> {
        match self {
            PaneKind::Chart(c) => Some(&c.symbol),
            PaneKind::Ticker { symbol, .. } => Some(symbol),
            PaneKind::News(n) => n.symbol.as_deref(),
            // N2
            PaneKind::Filings(f) => f.symbol.as_deref(),
            PaneKind::OrderBook(b) => Some(&b.symbol),
            PaneKind::Options(o) => Some(&o.symbol),
            _ => None,
        }
    }

    fn follows_symbol(&self) -> bool {
        matches!(
            self,
            PaneKind::Chart(_)
                | PaneKind::Ticker { .. }
                | PaneKind::News(_)
                // N2
                | PaneKind::Filings(_)
                | PaneKind::OrderBook(_)
                | PaneKind::Options(_)
        )
    }

    fn settings(&self) -> PaneSettings {
        match self {
            PaneKind::Chart(c) => PaneSettings::Chart {
                uid: c.uid,
                symbol: c.symbol.clone(),
                tf: c.tf,
                chart_type: c.chart_type,
                log: c.log,
                indicators: c.indicators.clone(),
                drawings: c.drawings.clone(),
                compare: c.compare.clone(),
                compare_axis: c.compare_axis,
                show_volume: c.show_volume,
                show_congress: c.show_congress,
                magnet: c.magnet,
                drawings_hidden: c.drawings_hidden,
                drawings_locked: c.drawings_locked,
                indicators_hidden: c.indicators_hidden,
                positions_orders_hidden: c.positions_orders_hidden,
                drawing_last_used: c.drawing_toolbar.last,
                navigation: c.navigation,
                data: c.saved_data(),
            },
            PaneKind::Feed { filter } => {
                PaneSettings::Feed((*filter != FeedFilter::default()).then(|| (*filter).into()))
            }
            PaneKind::Ticker { symbol, .. } => PaneSettings::Ticker(symbol.clone()),
            PaneKind::Profile { query, .. } => PaneSettings::Profile(query.clone()),
            PaneKind::Politicians => PaneSettings::Politicians,
            PaneKind::Alerts => PaneSettings::Alerts,
            PaneKind::Watchlist => PaneSettings::Watchlist,
            PaneKind::Scanner(s) => PaneSettings::Scanner(crate::layouts::ScanSettings {
                definition: s.definition.clone(),
                universe: s.universe,
                refresh: s.refresh,
                crypto_filter: s.crypto_filter.clone(),
            }),
            PaneKind::Help => PaneSettings::Help,
            PaneKind::Wei => PaneSettings::Wei,
            PaneKind::Curve => PaneSettings::Curve,
            PaneKind::Search { query } => PaneSettings::Search(query.clone()),
            // N1b
            PaneKind::News(n) => PaneSettings::News(
                (n.symbol.is_some() || n.filters != Default::default() || n.display != Default::default()).then(|| {
                    crate::layouts::NewsSettings {
                        symbol: n.symbol.clone(),
                        filters: n.filters.clone(),
                        display: n.display.clone(),
                    }
                }),
            ),
            // N1b
            // N2
            PaneKind::Filings(f) => PaneSettings::Filings(f.clone()),
            PaneKind::OrderBook(b) => {
                PaneSettings::OrderBook { symbol: b.symbol.clone(), depth: b.depth, group: b.group }
            }
            PaneKind::Settings => PaneSettings::Settings,
            PaneKind::Agent(settings) => PaneSettings::Agent(settings.clone()),
            PaneKind::Options(o) => {
                PaneSettings::Options { symbol: o.symbol.clone(), strikes: o.strikes, greeks: o.show_greeks }
            }
            PaneKind::Portfolio(p) => PaneSettings::Portfolio {
                view: p.view,
                account: p.account.clone(),
                symbol: p.symbol.clone(),
                sort: p.sort,
                open_only: p.open_only,
            },
        }
    }

    fn from_settings(p: PaneSettings) -> PaneKind {
        match p {
            PaneSettings::Chart {
                uid,
                symbol,
                tf,
                chart_type,
                log,
                indicators,
                drawings,
                compare,
                compare_axis,
                show_volume,
                show_congress,
                magnet,
                drawings_hidden,
                drawings_locked,
                indicators_hidden,
                positions_orders_hidden,
                drawing_last_used,
                navigation,
                data,
            } => {
                let mut c = ChartState::new(0, &symbol, tf);
                if uid != 0 {
                    c.uid = uid;
                }
                c.chart_type = chart_type;
                c.log = log;
                c.indicators = indicators;
                c.drawings = drawings;
                c.compare = compare;
                c.compare_axis = compare_axis;
                c.show_volume = show_volume;
                c.show_congress = show_congress;
                c.magnet = magnet;
                c.drawings_hidden = drawings_hidden;
                c.drawings_locked = drawings_locked;
                c.indicators_hidden = indicators_hidden;
                c.positions_orders_hidden = positions_orders_hidden;
                c.drawing_toolbar.restore_last(&drawing_last_used);
                c.navigation = navigation;
                c.data = data;
                c.restore_price_scale();
                PaneKind::Chart(Box::new(c))
            }
            PaneSettings::Feed(f) => PaneKind::Feed { filter: f.map(Into::into).unwrap_or_default() },
            PaneSettings::Ticker(s) => PaneKind::Ticker { symbol: s, trades: Vec::new(), loading: true },
            PaneSettings::Profile(q) => PaneKind::Profile { query: q, data: None, loading: true },
            PaneSettings::Politicians => PaneKind::Politicians,
            PaneSettings::Alerts => PaneKind::Alerts,
            PaneSettings::Watchlist => PaneKind::Watchlist,
            PaneSettings::Scanner(settings) => {
                PaneKind::Scanner(Box::new(scanner_view::ScannerPane::from_settings(settings)))
            }
            PaneSettings::Help => PaneKind::Help,
            PaneSettings::Wei => PaneKind::Wei,
            PaneSettings::Curve => PaneKind::Curve,
            PaneSettings::Search(q) => PaneKind::Search { query: q },
            // N1b
            PaneSettings::News(s) => {
                let saved = s.unwrap_or_default();
                let mut pane = NewsPane::new(saved.symbol);
                pane.filters = saved.filters;
                pane.display = saved.display;
                PaneKind::News(Box::new(pane))
            }
            // N1b
            // N2
            PaneSettings::Filings(f) => PaneKind::Filings(f),
            PaneSettings::OrderBook { symbol, depth, group } => {
                PaneKind::OrderBook(Box::new(BookPane::new(&symbol, depth, group)))
            }
            PaneSettings::Settings => PaneKind::Settings,
            PaneSettings::Options { symbol, strikes, greeks } => {
                PaneKind::Options(Box::new(OptionsPane::new(&symbol, strikes, greeks)))
            }
            PaneSettings::Agent(settings) => PaneKind::Agent(settings),
            PaneSettings::Portfolio { view, account, symbol, sort, open_only } => {
                PaneKind::Portfolio(Box::new(broker::PortfolioPane { view, account, symbol, sort, open_only }))
            }
        }
    }
}

pub struct Pane {
    pub id: PaneId,
    pub kind: PaneKind,
    pub link: Option<Link>,
    /// User-chosen tab title.
    pub title: Option<String>,
    /// Set while popped out into its own window.
    pub window: Option<WindowGeometry>,
}

impl Pane {
    fn display_title(&self) -> String {
        self.title.clone().unwrap_or_else(|| self.kind.tab_title())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NamedWatchlist {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub symbols: Vec<String>,
    /// Section headers over `symbols`; older files have none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<crate::watch_sections::WatchSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_turn: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub agent_entries: std::collections::BTreeMap<String, String>,
    /// Stable target identity once a workflow touches this list, not ownership.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchlistAddOutcome {
    pub added: bool,
    pub truncated: bool,
    pub message: String,
}

impl NamedWatchlist {
    pub fn new(name: String, symbols: Vec<String>) -> Self {
        Self {
            name,
            symbols,
            sections: Vec::new(),
            agent_turn: None,
            agent_entries: Default::default(),
            workflow_id: None,
        }
    }

    /// Rows in display order (section headers and symbols).
    pub fn items(&self) -> Vec<crate::watch_sections::Item> {
        crate::watch_sections::items(&self.symbols, &self.sections)
    }

    pub fn set_items(&mut self, items: &[crate::watch_sections::Item]) {
        let (symbols, sections) = crate::watch_sections::split(items);
        if symbols.iter().any(|s| !self.symbols.contains(s)) {
            self.agent_turn = None;
        }
        self.agent_entries.retain(|s, _| symbols.contains(s));
        (self.symbols, self.sections) = (symbols, sections);
    }
}

fn watchlist_name_key(name: &str) -> String {
    name.to_uppercase().to_lowercase()
}

pub(crate) fn valid_watchlist_name(name: &str) -> bool {
    (1..=40).contains(&name.trim().chars().count())
        && !name.chars().any(|c| {
            c.is_control()
                || matches!(c, '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
                    | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
        })
}

fn unique_watchlist_name(name: &str, lists: &[NamedWatchlist]) -> String {
    let mut candidate = name.to_owned();
    let mut n = 2;
    while lists.iter().any(|list| watchlist_name_key(&list.name) == watchlist_name_key(&candidate)) {
        let suffix = format!(" ({n})");
        candidate = format!("{}{}", name.chars().take(40 - suffix.chars().count()).collect::<String>(), suffix);
        n += 1;
    }
    candidate
}

pub(crate) fn sanitize_watchlists(lists: Vec<NamedWatchlist>) -> Vec<NamedWatchlist> {
    let mut out = Vec::new();
    for list in lists {
        let name = list.name.trim();
        if list.name.chars().any(char::is_control) || !valid_watchlist_name(name) {
            continue;
        }
        let name = unique_watchlist_name(name, &out);
        // Rows from the raw list keep each header beside the symbols that survive cleaning.
        let raw = crate::watch_sections::items(&list.symbols, &list.sections);
        let symbols = watchlist_append(
            &[],
            list.symbols
                .into_iter()
                .filter_map(|s| {
                    if s.chars().any(char::is_control) {
                        return None;
                    }
                    let tokens: Vec<_> = s.split_whitespace().collect();
                    tr_core::Security::resolve_tokens(&tokens)
                        .and_then(|(_, used)| (used == tokens.len()).then(|| watchlist_symbol_key(&s)))
                })
                .collect(),
        )
        .0;
        let clean = raw
            .into_iter()
            .filter_map(|item| match item {
                crate::watch_sections::Item::Symbol(s) => {
                    let tokens: Vec<_> = s.split_whitespace().collect();
                    tr_core::Security::resolve_tokens(&tokens)
                        .and_then(|(_, used)| (used == tokens.len()).then(|| watchlist_symbol_key(&s)))
                        .map(crate::watch_sections::Item::Symbol)
                }
                section => Some(section),
            })
            .collect::<Vec<_>>();
        let clean = crate::watch_sections::resync(&clean, &symbols);
        let (symbols, sections) = crate::watch_sections::split(&clean);
        let sections = crate::watch_sections::sanitize(&symbols, sections);
        let agent_entries = list.agent_entries.into_iter().filter(|(s, _)| symbols.contains(s)).collect();
        out.push(NamedWatchlist {
            name,
            symbols,
            sections,
            agent_turn: list.agent_turn,
            agent_entries,
            workflow_id: list.workflow_id,
        });
        if out.len() == 50 {
            break;
        }
    }
    if out.is_empty() {
        out.push(NamedWatchlist::new("Main".into(), Vec::new()));
    }
    out
}

/// App-wide state persisted in `settings.json` (layouts live in their own files).
#[derive(Serialize, Deserialize)]
struct Settings {
    #[serde(default, deserialize_with = "data_sources::deserialize_settings")]
    data_sources: tr_net::Settings,
    #[serde(default)]
    watchlist: Vec<String>,
    #[serde(default)]
    watchlists: Vec<NamedWatchlist>,
    #[serde(default)]
    active_watchlist: usize,
    #[serde(default = "default_true")]
    watch_details: bool,
    #[serde(default = "default_true")]
    sidebar: bool,
    /// Name of the layout to restore on launch.
    #[serde(default)]
    active_layout: Option<String>,
    // Pre-docking settings files stored a preset and a pane list here.
    #[serde(default, skip_serializing)]
    layout: Option<Layout>,
    #[serde(default, skip_serializing)]
    panes: Option<Vec<serde_json::Value>>,
}

impl Settings {
    fn sanitized_watchlists(&self) -> (Vec<NamedWatchlist>, usize) {
        let lists = if self.watchlists.is_empty() {
            vec![NamedWatchlist::new("Main".into(), self.watchlist.clone())]
        } else {
            self.watchlists.clone()
        };
        let lists = sanitize_watchlists(lists);
        let active = self.active_watchlist.min(lists.len() - 1);
        (lists, active)
    }
}

fn accept_columns<T>(revision: u64, current: u64, result: T) -> Option<T> {
    (revision == current).then_some(result)
}

fn finish_columns_reply<T>(in_flight: &mut Option<u64>, revision: u64, current: u64, result: T) -> Option<T> {
    let accepted = accept_columns(revision, current, result);
    if *in_flight == Some(revision) {
        *in_flight = None;
    }
    accepted
}

fn columns_refresh_due(in_flight: Option<u64>, requested: Option<Instant>, now: Instant) -> bool {
    in_flight.is_none() && requested.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(60))
}

fn watch_value_order(a: Option<f64>, b: Option<f64>, descending: bool) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => {
            if descending {
                b.total_cmp(&a)
            } else {
                a.total_cmp(&b)
            }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn default_true() -> bool {
    true
}

const DEFAULT_WATCHLIST: [&str; 11] = [
    "SPX Index",
    "AAPL US Equity",
    "NVDA US Equity",
    "SPY US Equity",
    "BTCUSD Curncy",
    "ETHUSD Curncy",
    "EURUSD Curncy",
    "USDJPY Curncy",
    "US10Y Govt",
    "CL1 Comdty",
    "GC1 Comdty",
];

/// Convert a pre-docking settings file into a layout document.
fn migrate_legacy(s: &Settings, defaults: &prefs::PanelDefaults) -> Option<LayoutDoc> {
    let panes: Vec<PaneSettings> = s
        .panes
        .as_ref()?
        .iter()
        .filter_map(|v| {
            // Unit variants were stored as bare strings ("Feed"); wrap them as {"Feed": null}.
            let v = match v {
                serde_json::Value::String(name) => serde_json::json!({ name.as_str(): null }),
                other => other.clone(),
            };
            serde_json::from_value(v).ok()
        })
        .collect();
    if panes.is_empty() {
        return None;
    }
    let _ = defaults;
    Some(LayoutDoc::preset("Default", s.layout.unwrap_or(Layout::Grid2x2), panes))
}

#[derive(Clone, Copy)]
enum WatchEntryKind {
    Section,
    Rename,
}

#[derive(Clone)]
struct WatchItemEntry {
    kind: WatchEntryKind,
    list: String,
    anchor: WatchEntryAnchor,
    focus: bool,
    text: String,
}

#[derive(Clone)]
enum WatchEntryAnchor {
    End,
    Symbol(String, bool),
    Section { name: String, occurrence: usize, after: bool },
}

impl WatchEntryAnchor {
    fn at(items: &[crate::watch_sections::Item], index: usize, after: bool) -> Self {
        use crate::watch_sections::Item;
        match items.get(index) {
            Some(Item::Symbol(key)) => Self::Symbol(key.clone(), after),
            Some(Item::Section { name, .. }) => Self::Section {
                name: name.clone(),
                occurrence: items[..index]
                    .iter()
                    .filter(|item| matches!(item, Item::Section { name: n, .. } if n == name))
                    .count(),
                after,
            },
            None => Self::End,
        }
    }

    fn resolve(&self, items: &[crate::watch_sections::Item]) -> Option<usize> {
        use crate::watch_sections::{self, Item};
        match self {
            Self::End => Some(items.len()),
            Self::Symbol(key, after) => items
                .iter()
                .position(|item| matches!(item, Item::Symbol(k) if k == key))
                .map(|i| i + usize::from(*after)),
            Self::Section { name, occurrence, after } => items
                .iter()
                .enumerate()
                .filter(|(_, item)| matches!(item, Item::Section { name: n, .. } if n == name))
                .nth(*occurrence)
                .map(|(i, _)| if *after { watch_sections::block(items, i).end } else { i }),
        }
    }
}

/// How long an alert toast stays up.
const TOAST_SECS: u64 = 10;

/// A pane drag from its tab or header: pending until the pointer moves past a small
/// threshold (so clicks still select tabs), then active until release.
#[derive(Debug, Clone, Copy)]
pub struct Drag {
    pub pane: PaneId,
    pub origin: egui::Pos2,
    pub active: bool,
}

pub struct TerminalApp {
    // N2
    edgar: crate::filings_view::State,
    drawing_ids: crate::drawing_history::SharedDrawingIds,
    pub agent_queue: crate::agent_queue::AgentToolQueue,
    pub pinned_agent_chart: Option<u64>,
    pub(crate) agent_pane: crate::agent_pane::AgentPane,
    agent_demo: Option<crate::agent_demo::Demo>,
    agent_turns: Vec<String>,
    agent_save_pending: bool,
    pub edit_log: crate::edit_log::EditLog,
    agent_job_chart: Option<u64>,
    worker: Worker,
    provider: String,
    panes: Vec<Pane>,
    dock: Node,
    focus: PaneId,
    next_id: PaneId,
    last_opened: Option<PaneId>,
    /// Preset last applied, for the toolbar picker (cleared by manual rearranging).
    preset: Option<Layout>,
    drag: Option<Drag>,
    dock_motion: workspace::DockMotion,
    /// Tab being renamed inline: pane and edit buffer.
    renaming_tab: Option<(PaneId, String)>,

    store: Store,
    layout_name: String,
    layout_names: Vec<String>,
    saves: SaveState,
    settings_file: std::path::PathBuf,
    /// Layout being renamed in the sidebar: old name and edit buffer.
    renaming_layout: Option<(String, String)>,

    prefs: Prefs,
    /// Built-in and custom themes, and the theme last applied to egui.
    themes: Vec<crate::themes::Theme>,
    theme_store: crate::themes::ThemeStore,
    applied_theme: Option<crate::themes::Theme>,
    /// Theme selected in the editor.
    editing_theme: String,
    /// Custom themes edited but not yet written (saved once editing pauses).
    dirty_themes: Vec<String>,
    themes_changed_at: Option<Instant>,
    renaming_theme: Option<(String, String)>,
    theme_io: String,
    /// The OS appearance, for System theme resolution in commands.
    system_theme: Option<egui::Theme>,
    /// The active layout carries its theme.
    layout_theme: bool,
    rebinding: Option<prefs::Action>,
    hotkey_filter: String,
    settings_tab: SettingsTab,
    data_sources: data_sources::State,
    io_path: String,

    curves: Option<Box<YieldCurves>>,
    last_wei: Option<Instant>,
    last_curve: Option<Instant>,
    wei_updated: Option<String>,
    /// Latest network search results and the query they answer.
    symbol_search: Option<crate::symbol_search::Dialog>,
    net_hits: (String, Vec<SearchHit>),
    /// Query typed in the command line awaiting a debounced network search.
    search_pending: Option<(String, Instant)>,
    suggestion: Option<usize>,

    cmd: String,
    cmd_history: Vec<String>,
    cmd_hist_pos: Option<usize>,
    focus_cmd: bool,

    status: String,
    toasts: Vec<(String, Instant)>,
    alert_toasts: Vec<(Instant, tr_core::price_alerts::Event)>,
    // N1a
    news_subscription: Option<(Vec<crate::worker::NewsView>, Vec<String>)>,
    // N1a
    watchlist_undo: Option<(Instant, Vec<String>, Vec<String>)>,

    latest: Vec<Trade>,
    politicians: Vec<PoliticianSummary>,
    pol_search: String,
    alerts: Vec<Alert>,
    price_alerts: crate::price_alerts::State,
    congress_status: String,
    /// When the last Congress ingest finished.
    congress_checked: Option<chrono::DateTime<chrono::Local>>,

    /// Active-list projection for panel, scanner and worker snapshots. Updated only by
    /// set_watchlist and switch_watchlist; persisted ownership lives in watchlists.
    watchlist: Vec<String>,
    watchlists: Vec<NamedWatchlist>,
    active_watchlist: usize,
    watchlist_entry: Option<(Option<String>, String)>,
    watch_item_entry: Option<WatchItemEntry>,
    watch_selected: Option<String>,
    watch_details: bool,
    watchlist_delete: bool,
    security_names: crate::security_names::Names,
    watch_columns: tr_core::columns::ColumnSet,
    columns_editor: Option<columns_view::ColumnsEditor>,
    column_cells: HashMap<String, Vec<tr_core::columns::Cell>>,
    columns_revision: u64,
    columns_computed_at: i64,
    columns_requested: Option<Instant>,
    columns_in_flight: Option<u64>,
    columns_snapshot: (Vec<String>, tr_core::columns::ColumnSet),
    watch_sort: Option<(usize, bool)>,
    watch_drag: Option<watchlist_drag::WatchDrag>,
    quotes: HashMap<String, Quote>,
    last_quotes: Option<Instant>,
    last_feed: Option<Instant>,
    last_save: Instant,
    started: Instant,
    smoke_done: bool,
    /// Frame-time readout (F12): smoothed CPU time of `ui()` and interval between frames.
    show_perf: bool,
    perf_cpu_ms: f64,
    perf_dt_ms: f64,
    last_frame: Option<Instant>,

    sidebar_open: bool,
    /// System accent colour (macOS) or the default blue.
    system_accent: Color32,
    last_accent_poll: Option<Instant>,
    window_styled: bool,
    updater_started: bool,
    /// The window is see-through behind the sidebar (macOS vibrancy).
    vibrancy: bool,
    /// Measured macOS title bar geometry (polled; changes with full screen).
    chrome: Option<tr_macos::Chrome>,
    last_chrome_poll: Option<Instant>,
    /// Brokerage integration (SnapTrade credentials and the loaded portfolio).
    broker: broker::BrokerState,
    /// Order ticket and trading dialogs.
    trading: trading::TradingState,
    /// The paper-trading demo account (selected by default).
    demo: demo::DemoState,
    /// ⌘K command palette.
    palette: crate::palette::Palette,
}

/// Resolve a run of words (`BTCUSD CRNCY AAPL US EQUITY MSFT`) into market keys.
fn keys_from_words(words: &[String]) -> Vec<String> {
    let tokens: Vec<&str> = words.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match tr_core::Security::resolve_tokens(&tokens[i..]) {
            Some((sec, used)) => {
                out.push(sec.key());
                i += used;
            }
            None => i += 1,
        }
    }
    out
}

/// Qualified keys are already identities; legacy bare tickers still resolve as before.
fn watchlist_symbol_key(symbol: &str) -> String {
    if symbol.contains(':') { symbol.to_owned() } else { tr_data::security(symbol).key() }
}

/// Each watchlist is capped at 200 symbols; appending preserves existing order.
fn watchlist_append(current: &[String], symbols: Vec<String>) -> (Vec<String>, usize, bool) {
    let mut next = current.to_vec();
    let mut truncated = false;
    for symbol in symbols {
        let key = watchlist_symbol_key(&symbol);
        if !next.iter().any(|existing| crate::symbol_search::same_key(existing, &key)) {
            if next.len() >= 200 {
                truncated = true;
            } else {
                next.push(key);
            }
        }
    }
    let added = next.len() - current.len();
    (next, added, truncated)
}

fn watchlist_restore(
    current: &[String],
    undo: &mut Option<(Instant, Vec<String>, Vec<String>)>,
) -> Option<Vec<String>> {
    undo.take().and_then(|(_, previous, expected)| (current == expected).then_some(previous))
}

fn watchlist_add_message(added: usize, truncated: bool) -> String {
    let mut message = if added == 0 && !truncated {
        "All matches are already in the watchlist".into()
    } else {
        format!("Added {added} symbols to the watchlist")
    };
    if truncated {
        message.push_str(" (truncated to the 200-symbol watchlist cap)");
    }
    message
}

fn settings_path() -> std::path::PathBuf {
    data_dir().join("settings.json")
}

fn prefs_path() -> std::path::PathBuf {
    data_dir().join("prefs.json")
}

impl TerminalApp {
    pub fn new(
        ctx: &egui::Context,
        worker: Worker,
        provider: String,
        questrade: Option<tr_data::questrade::Questrade>,
    ) -> Self {
        let system_accent =
            tr_macos::accent_color().map_or(theme::DEFAULT_ACCENT, |[r, g, b]| Color32::from_rgb(r, g, b));
        let mut prefs = Prefs::load(&prefs_path());
        let theme_store = crate::themes::ThemeStore::new(data_dir().join("themes"));
        let mut themes = theme_store.load_all();
        // Appearance overrides from older versions become a custom theme file.
        if let Some(mut t) = prefs.migrated_theme.take() {
            t.name = crate::themes::unique_name(&themes, &t.name);
            prefs.appearance.theme = t.name.clone();
            match theme_store.save(&t) {
                Ok(path) => t.source = Some(path),
                Err(e) => tracing::warn!("saving migrated theme: {e:#}"),
            }
            themes.push(t);
            let _ = prefs.save(&prefs_path());
        }
        if let Ok(v) = std::env::var("TR_THEME") {
            prefs.appearance.theme = match v.to_ascii_lowercase().as_str() {
                "light" => prefs.appearance.light_theme.clone(),
                "dark" => prefs.appearance.dark_theme.clone(),
                _ => crate::themes::find(&themes, &v).map_or(prefs.appearance.theme.clone(), |t| t.name.clone()),
            };
        }
        let settings: Option<Settings> =
            std::fs::read(settings_path()).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let mut broker = broker::BrokerState::load(provider.eq_ignore_ascii_case("sample"));
        broker.questrade = questrade;
        let (demo, demo_note) = demo::DemoState::load(demo::demo_path());
        let price_alerts = crate::price_alerts::State::load(
            data_dir().join("price-alerts.json"),
            provider.eq_ignore_ascii_case("sample"),
        );
        let mut app = Self {
            // N2
            edgar: Default::default(),
            drawing_ids: Default::default(),
            agent_queue: crate::agent_queue::AgentToolQueue::new(ctx.clone()),
            pinned_agent_chart: None,
            agent_pane: crate::agent_pane::AgentPane::default(),
            agent_demo: None,
            agent_turns: Vec::new(),
            agent_save_pending: false,
            edit_log: Default::default(),
            agent_job_chart: None,
            broker,
            trading: Default::default(),
            demo,
            worker,
            provider,
            panes: Vec::new(),
            dock: Node::default(),
            focus: 0,
            next_id: 1,
            last_opened: None,
            preset: None,
            drag: None,
            dock_motion: Default::default(),
            renaming_tab: None,
            store: Store::new(data_dir().join("layouts")),
            layout_name: "Default".into(),
            layout_names: Vec::new(),
            saves: SaveState::default(),
            settings_file: settings_path(),
            renaming_layout: None,
            editing_theme: prefs.appearance.theme.clone(),
            themes,
            theme_store,
            applied_theme: None,
            dirty_themes: Vec::new(),
            themes_changed_at: None,
            renaming_theme: None,
            theme_io: String::new(),
            system_theme: ctx.system_theme(),
            layout_theme: false,
            prefs,
            rebinding: None,
            hotkey_filter: String::new(),
            settings_tab: SettingsTab::Appearance,
            data_sources: data_sources::State::default(),
            io_path: String::new(),
            curves: None,
            last_wei: None,
            last_curve: None,
            wei_updated: None,
            symbol_search: None,
            net_hits: (String::new(), Vec::new()),
            search_pending: None,
            suggestion: None,
            cmd: String::new(),
            cmd_history: Vec::new(),
            cmd_hist_pos: None,
            focus_cmd: true,
            status: "Ready. Type a ticker and press <GO> (Enter). F1 for help.".into(),
            toasts: Vec::new(),
            alert_toasts: Vec::new(),
            // N1a
            news_subscription: None,
            // N1a
            watchlist_undo: None,
            latest: Vec::new(),
            politicians: Vec::new(),
            pol_search: String::new(),
            alerts: Vec::new(),
            price_alerts,
            congress_status: String::new(),
            congress_checked: None,
            watchlist: DEFAULT_WATCHLIST.map(String::from).to_vec(),
            watchlists: vec![NamedWatchlist::new("Main".into(), DEFAULT_WATCHLIST.map(String::from).to_vec())],
            active_watchlist: 0,
            watchlist_entry: None,
            watch_item_entry: None,
            watch_selected: None,
            watch_details: true,
            watchlist_delete: false,
            security_names: Default::default(),
            watch_columns: Default::default(),
            columns_editor: None,
            column_cells: HashMap::new(),
            columns_revision: 0,
            columns_computed_at: 0,
            columns_requested: None,
            columns_in_flight: None,
            columns_snapshot: Default::default(),
            watch_sort: None,
            watch_drag: None,
            quotes: HashMap::new(),
            last_quotes: None,
            last_feed: None,
            last_save: Instant::now(),
            started: Instant::now(),
            smoke_done: false,
            show_perf: std::env::var_os("TR_FPS").is_some(),
            perf_cpu_ms: 0.0,
            perf_dt_ms: 0.0,
            last_frame: None,
            sidebar_open: true,
            system_accent,
            last_accent_poll: None,
            window_styled: false,
            updater_started: false,
            vibrancy: false,
            chrome: None,
            last_chrome_poll: None,
            palette: crate::palette::Palette::load(&data_dir().join("palette.json")),
        };
        // Do not accidentally re-enable sources when another settings field prevents restoration.
        app.data_sources = data_sources::State::new(match std::fs::read(settings_path()) {
            Ok(bytes) => tr_net::Settings::from_document(&bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => tr_net::Settings::default(),
            Err(_) => tr_net::Settings::corrupt(),
        });
        let mut doc = None;
        if let Some(s) = &settings {
            app.data_sources = data_sources::State::new(s.data_sources.clone());
            (app.watchlists, app.active_watchlist) = s.sanitized_watchlists();
            app.watchlist = app.watchlists[app.active_watchlist].symbols.clone();
            app.sidebar_open = s.sidebar;
            app.watch_details = s.watch_details;
            if let Some(name) = &s.active_layout {
                match app.store.load(name) {
                    Ok(d) => doc = Some(d),
                    Err(e) => {
                        tracing::warn!("restoring layout {name:?}: {e:#}");
                        app.toasts.push((format!("Could not restore layout {name:?}: {e:#}"), Instant::now()));
                    }
                }
            }
            if doc.is_none() {
                doc = migrate_legacy(s, &app.prefs.panels);
            }
        }
        let doc = doc
            .or_else(|| {
                let name = app.store.list().into_iter().next()?;
                match app.store.load(&name) {
                    Ok(doc) => Some(doc),
                    Err(e) => {
                        if app.toasts.is_empty() {
                            app.toasts.push((format!("Could not restore layout {name:?}: {e:#}"), Instant::now()));
                        }
                        None
                    }
                }
            })
            .unwrap_or_else(|| LayoutDoc::default_workspace("Default", &app.prefs.panels));
        if let Ok(bytes) = std::fs::read(&app.settings_file) {
            app.saves.written.insert(app.settings_file.clone(), bytes);
        }
        app.apply_doc(doc);
        if !app.store.exists(&app.layout_name) {
            app.save_layout();
        }
        app.refresh_layout_names();
        app.worker.send(Request::Latest { limit: 2_000 });
        app.worker.send(Request::SearchPoliticians(String::new()));
        app.worker.send(Request::Alerts);
        if let Some(note) = demo_note {
            app.trade_toast(note, false);
        }
        if let Some(error) = &app.price_alerts.load_error {
            app.toasts.push((
                format!(
                    "{}Could not load price alerts: {error}. Open Alerts for recovery instructions.",
                    crate::price_alerts::TOAST_PREFIX
                ),
                Instant::now(),
            ));
        }
        app
    }

    fn settings_json(&self) -> Vec<u8> {
        let s = Settings {
            data_sources: self.data_sources.settings.clone(),
            watchlist: Vec::new(),
            watchlists: self.watchlists.clone(),
            active_watchlist: self.active_watchlist,
            watch_details: self.watch_details,
            sidebar: self.sidebar_open,
            active_layout: Some(self.layout_name.clone()),
            layout: None,
            panes: None,
        };
        serde_json::to_vec_pretty(&s).expect("settings serialize")
    }

    fn save_settings(&mut self) {
        self.finish_save();
        let write = (self.settings_file.clone(), self.settings_json());
        self.save_sync(vec![write]);
    }

    fn save_prefs(&mut self) {
        if self.agent_demo.is_some() {
            return;
        }
        if let Err(e) = self.prefs.save(&prefs_path()) {
            self.status = format!("Could not save preferences: {e}");
        }
    }

    fn switch_watchlist(&mut self, index: usize) {
        if index >= self.watchlists.len() {
            return;
        }
        if let Some((at, _, _)) = self.watchlist_undo.take() {
            self.toasts.retain(|(_, time)| *time != at);
        }
        self.watch_item_entry = None;
        self.watchlist_entry = None;
        self.watch_drag = None;
        if self.symbol_search.as_ref().is_some_and(|d| d.target.is_some()) {
            self.symbol_search = None;
        }
        self.active_watchlist = index;
        self.watchlist = self.watchlists[index].symbols.clone();
        self.watchlist_delete = false;
        self.last_quotes = None;
        // Even equal-symbol lists are a new selection: discard cells and invalidate
        // replies from the previous selection before requesting fresh columns.
        self.columns_revision = self.columns_revision.wrapping_add(1);
        self.column_cells.clear();
        self.columns_computed_at = 0;
        self.columns_requested = None;
        for pane in &mut self.panes {
            if let PaneKind::Scanner(scanner) = &mut pane.kind {
                scanner.watchlist_changed();
            }
        }
        self.refresh_columns();
    }

    fn create_watchlist(&mut self, name: String, symbols: Vec<String>) -> Result<(), String> {
        self.check_watchlist_name(&name, None)?;
        let name = name.trim().to_owned();
        if self.watchlists.len() >= 50 {
            return Err("At most 50 watchlists are allowed".into());
        }
        self.watchlists.push(NamedWatchlist::new(name, watchlist_append(&[], symbols).0));
        self.switch_watchlist(self.watchlists.len() - 1);
        Ok(())
    }

    fn check_watchlist_name(&self, name: &str, except: Option<usize>) -> Result<(), String> {
        if !valid_watchlist_name(name) {
            return Err("Names must contain 1–40 characters and no control or invisible format characters".into());
        }
        if self
            .watchlists
            .iter()
            .enumerate()
            .any(|(i, l)| Some(i) != except && watchlist_name_key(&l.name) == watchlist_name_key(name.trim()))
        {
            return Err("A watchlist with that name already exists".into());
        }
        Ok(())
    }

    fn duplicate_watchlist(&mut self) -> Result<(), String> {
        let mut copy = self.watchlists[self.active_watchlist].clone();
        copy.agent_turn = None;
        copy.agent_entries.clear();
        copy.workflow_id = None;
        copy.name = unique_watchlist_name(&copy.name, &self.watchlists);
        self.check_watchlist_name(&copy.name, None)?;
        if self.watchlists.len() >= 50 {
            return Err("At most 50 watchlists are allowed".into());
        }
        self.watchlists.push(copy);
        self.switch_watchlist(self.watchlists.len() - 1);
        Ok(())
    }

    fn apply_watch_item_entry(&mut self) -> Result<(), String> {
        let Some(entry) = self.watch_item_entry.clone() else { return Ok(()) };
        if entry.list != self.watchlists[self.active_watchlist].name {
            self.watch_item_entry = None;
            return Err("The watchlist being edited is no longer active".into());
        }
        let mut items = self.watchlists[self.active_watchlist].items();
        let index = match entry.anchor.resolve(&items) {
            Some(index) => index,
            None => {
                self.watch_item_entry = None;
                return Err("The entry target no longer exists".into());
            }
        };
        match entry.kind {
            WatchEntryKind::Section => crate::watch_sections::insert_section(&mut items, index, &entry.text)?,
            WatchEntryKind::Rename => crate::watch_sections::rename_section(&mut items, index, &entry.text)?,
        }
        self.set_watchlist_items(&items);
        self.watch_item_entry = None;
        Ok(())
    }

    fn apply_watchlist_entry(&mut self) -> Result<(), String> {
        let Some((target, name)) = self.watchlist_entry.clone() else {
            return Ok(());
        };
        match target {
            Some(key) => {
                let Some(index) = self.watchlists.iter().position(|l| watchlist_name_key(&l.name) == key) else {
                    self.watchlist_entry = None;
                    return Err("The watchlist being renamed no longer exists".into());
                };
                self.rename_watchlist(index, name)?;
            }
            None => self.create_watchlist(name, Vec::new())?,
        }
        self.watchlist_entry = None;
        Ok(())
    }

    fn rename_watchlist(&mut self, index: usize, name: String) -> Result<(), String> {
        if index >= self.watchlists.len() {
            return Err("The watchlist being renamed no longer exists".into());
        }
        self.check_watchlist_name(&name, Some(index))?;
        let name = name.trim().to_owned();
        self.watchlists[index].agent_turn = None;
        self.watchlists[index].name = name;
        Ok(())
    }

    fn delete_watchlist(&mut self, index: usize) -> Result<(), String> {
        if self.watchlists.len() == 1 {
            return Err("Cannot delete the last watchlist".into());
        }
        self.watchlists.remove(index);
        if index == self.active_watchlist {
            self.switch_watchlist(index.min(self.watchlists.len() - 1));
        } else if index < self.active_watchlist {
            self.active_watchlist -= 1;
        }
        Ok(())
    }

    /// Replace the active list's symbols. Sections keep the symbols that remain; new
    /// symbols join the end (the last section).
    fn set_watchlist(&mut self, symbols: Vec<String>) {
        let items = crate::watch_sections::resync(&self.watchlists[self.active_watchlist].items(), &symbols);
        self.set_watchlist_items(&items);
    }

    /// Store the active list's rows (sections and order). Membership or order changes
    /// refresh quotes and columns like any watchlist edit; section-only edits do not.
    pub(crate) fn set_watchlist_items(&mut self, items: &[crate::watch_sections::Item]) {
        if let Some(dialog) = &mut self.symbol_search
            && let Some(target) = &mut dialog.target
        {
            target.rebase(&self.watchlists[self.active_watchlist].items(), items);
        }
        self.watchlists[self.active_watchlist].set_items(items);
        let symbols = self.watchlists[self.active_watchlist].symbols.clone();
        if self.watchlist != symbols {
            if let Some((at, _, _)) = self.watchlist_undo.take() {
                self.toasts.retain(|(_, time)| *time != at);
            }
            self.watchlist = symbols;
            self.last_quotes = None;
            self.refresh_columns();
        }
    }

    /// A user drag claims the moved entries, not the rows displaced by them.
    fn move_watchlist_item(&mut self, from: usize, gap: usize) {
        let mut items = self.watchlists[self.active_watchlist].items();
        if from >= items.len() {
            return;
        }
        let moved = items[crate::watch_sections::block(&items, from)].to_vec();
        if crate::watch_sections::move_item(&mut items, from, gap) {
            let list = &mut self.watchlists[self.active_watchlist];
            list.agent_turn = None;
            for item in moved {
                if let crate::watch_sections::Item::Symbol(symbol) = item {
                    list.agent_entries.remove(&symbol);
                }
            }
            self.set_watchlist_items(&items);
        }
    }

    fn add_watchlist_symbols(&mut self, symbols: Vec<String>) {
        for symbol in &symbols {
            self.watchlists[self.active_watchlist].agent_entries.remove(&watchlist_symbol_key(symbol));
        }
        let (next, added, truncated) = watchlist_append(&self.watchlist, symbols);
        self.set_watchlist(next);
        let message = watchlist_add_message(added, truncated);
        self.status = message.clone();
        self.toasts.push((format!("Watchlist: {message}"), Instant::now()));
    }

    /// UI-independent single-symbol add for menus and agent tools. A missing section
    /// appends to the list's end; a section target is its ordinal and captured name.
    /// Duplicates and the cap are reported without moving an existing symbol.
    pub fn add_to_named_watchlist(
        &mut self,
        name: &str,
        symbol: String,
        section: Option<&crate::chart_view::WatchlistSectionTarget>,
    ) -> Result<WatchlistAddOutcome, String> {
        let index = self
            .watchlists
            .iter()
            .position(|list| list.name == name)
            .ok_or_else(|| format!("Watchlist {name} no longer exists"))?;
        self.watchlists[index].agent_entries.remove(&watchlist_symbol_key(&symbol));
        let list = &self.watchlists[index];
        let mut items = list.items();
        let gap = if let Some(section) = section {
            let header = items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.is_section())
                .nth(section.index)
                .filter(|(_, item)| {
                    matches!(item,
                    crate::watch_sections::Item::Section { name, .. } if name == &section.name)
                })
                .map(|(position, _)| position)
                .ok_or_else(|| format!("Section {} in {name} no longer exists", section.name))?;
            crate::watch_sections::block(&items, header).end
        } else {
            items.len()
        };
        let ticker = crate::security_names::ticker(&tr_data::security(&symbol), false);
        let (next, added, truncated) = watchlist_append(&list.symbols, vec![symbol]);
        if added != 0 {
            items.insert(gap, crate::watch_sections::Item::Symbol(next.last().unwrap().clone()));
            if index == self.active_watchlist {
                self.set_watchlist_items(&items);
            } else {
                self.watchlists[index].set_items(&items);
            }
        }
        let message = if index == self.active_watchlist {
            watchlist_add_message(added, truncated)
        } else {
            match (added, truncated) {
                (0, true) => format!("{name} is full (200 symbols); {ticker} was not added"),
                (0, false) => format!("{ticker} is already in {name}"),
                _ => format!("Added {ticker} to {name}"),
            }
        };
        Ok(WatchlistAddOutcome { added: added != 0, truncated, message })
    }

    fn add_watchlist_symbol(
        &mut self,
        name: &str,
        symbol: String,
        section: Option<&crate::chart_view::WatchlistSectionTarget>,
    ) {
        match self.add_to_named_watchlist(name, symbol, section) {
            Ok(outcome) => {
                self.status = outcome.message.clone();
                self.toasts.push((format!("Watchlist: {}", outcome.message), Instant::now()));
            }
            Err(error) => self.status = error,
        }
    }

    fn undo_watchlist(&mut self) {
        if let Some((at, _, _)) = &self.watchlist_undo {
            self.toasts.retain(|(_, time)| time != at);
        }
        if let Some(previous) = watchlist_restore(&self.watchlist, &mut self.watchlist_undo) {
            self.set_watchlist(previous);
            self.status = "Watchlist restored".into();
        }
    }

    fn run_actions(&mut self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Notice(message) => self.toasts.push((message, Instant::now())),
                // N1a: ticker chips target a linked chart, else the existing chart target (never just News).
                Action::OpenChart(s) => {
                    if self.idx(self.focus).is_some_and(|i| {
                        matches!(self.panes[i].kind, PaneKind::News(_))
                            && !self.panes[i].link.is_some_and(|link| {
                                self.panes.iter().any(|p| p.link == Some(link) && matches!(p.kind, PaneKind::Chart(_)))
                            })
                    }) {
                        self.focus = self.chart_target();
                    }
                    self.open_security(&s);
                }
                // N1a
                Action::ScanChart { symbol, symbols, definition } => {
                    self.open_security(&symbol);
                    self.with_chart(|c, w| {
                        c.open_scan_chart(&symbol, symbols, definition, w);
                    });
                }
                Action::OpenTicker(s) => {
                    self.open_panel(PaneKind::Ticker { symbol: s, trades: Vec::new(), loading: true })
                }
                Action::OpenProfile(q) => self.open_panel(PaneKind::Profile { query: q, data: None, loading: true }),
                // N3: an explicit Reload wakes the worker, without resetting durable cadence.
                Action::ReloadFilings => self.edgar.interest = None,
                // N3
                Action::AddToWatchlist(symbol) => {
                    let name = self.watchlists[self.active_watchlist].name.clone();
                    self.add_watchlist_symbol(&name, symbol, None);
                }
                Action::AddToNamedWatchlist { list, symbol } => self.add_watchlist_symbol(&list, symbol, None),
                Action::AddToWatchlistSection { list, symbol, section } => {
                    self.add_watchlist_symbol(&list, symbol, Some(&section));
                }
                Action::AddToCompare(symbol) => {
                    if self.panes.iter().any(|p| matches!(p.kind, PaneKind::Chart(_))) {
                        self.with_chart(|c, w| c.add_comparison(symbol, w));
                    } else {
                        self.status = "Open a chart to compare".into();
                    }
                }
                Action::SaveMatchesAsWatchlist(name, symbols) => {
                    let name = unique_watchlist_name(name.trim(), &self.watchlists);
                    let result = self.create_watchlist(name, symbols);
                    self.status = result.err().unwrap_or_else(|| "Matches saved as new watchlist".into());
                }
                Action::AddMatchesToWatchlist(symbols) => self.add_watchlist_symbols(symbols),
                Action::ReplaceWatchlistWithMatches(symbols) => {
                    if let Some((at, _, _)) = self.watchlist_undo.take() {
                        self.toasts.retain(|(_, time)| *time != at);
                    }
                    let previous = self.watchlist.clone();
                    let (next, _, truncated) = watchlist_append(&[], symbols);
                    let mut message = format!("Watchlist replaced with {} matches · Undo", next.len());
                    if truncated {
                        message.push_str(" (truncated to the 200-symbol watchlist cap)");
                    }
                    self.set_watchlist(next);
                    let at = Instant::now();
                    self.watchlist_undo = Some((at, previous, self.watchlist.clone()));
                    self.status = message.clone();
                    self.toasts.push((format!("Watchlist: {message}"), at));
                }
                Action::LinkedSymbol(link, s) => self.broadcast(link, &s, None),
                Action::Command(line) => match command::parse(&line) {
                    Some(cmd) => self.dispatch(cmd),
                    None => tracing::warn!("internal command did not parse: {line}"),
                },
                Action::Track(q, on) => {
                    self.worker.send(Request::Track { query: q.clone(), on });
                    // Refresh any open profile so the star updates.
                    for p in &self.panes {
                        if let PaneKind::Profile { query, .. } = &p.kind {
                            self.worker.send(Request::Profile { pane: p.id, query: query.clone() });
                        }
                    }
                }
            }
        }
    }

    fn execute(&mut self, input: &str) {
        let undo = input.trim().to_ascii_uppercase();
        if undo == "UNDO AGENT" || undo.starts_with("UNDO AGENT CHART ") {
            let turn = if let Some(uid) = undo.strip_prefix("UNDO AGENT CHART ") {
                uid.parse().ok().and_then(|uid| self.last_agent_turn_for_chart(uid)).map(str::to_owned)
            } else {
                self.last_agent_turn().map(str::to_owned)
            };
            self.status = match turn {
                Some(turn) => match self.undo_agent_turn(&turn) {
                    Ok(summary) => format!(
                        "Undid agent turn: {} operations, {} conflicts",
                        summary.reverted,
                        summary.skipped.len()
                    ),
                    Err(error) => error,
                },
                None => "No agent turn to undo".into(),
            };
            return;
        }
        let Some(cmd) = command::parse(input) else {
            self.status = command::parse_error(input);
            return;
        };
        self.cmd_history.push(input.trim().to_string());
        self.dispatch(cmd);
    }

    fn dispatch(&mut self, cmd: Command) {
        match cmd {
            Command::Symbol(s) => self.open_security(&s),
            Command::SymbolFunction(s, Function::Congress) => {
                self.open_panel(PaneKind::Ticker { symbol: s.clone(), trades: Vec::new(), loading: true });
                self.status = format!("{s}: congressional trades");
            }
            // N2
            Command::SymbolFunction(s, Function::Filings) => {
                if !self.retarget_feed(&s, false) {
                    self.open_panel(PaneKind::Filings(crate::filings_view::Settings::new(Some(s.clone()))));
                }
                self.status = format!("{s}: filings");
            }
            Command::SymbolFunction(s, Function::News) => {
                if !self.retarget_feed(&s, true) {
                    self.open_panel(PaneKind::News(Box::new(NewsPane::new(Some(s.clone())))));
                }
                self.status = format!("{s}: news");
            }
            Command::SymbolFunction(s, Function::OrderBook(depth)) => {
                let depth = depth.unwrap_or(self.prefs.panels.book_depth).clamp(5, 200);
                self.open_panel(PaneKind::OrderBook(Box::new(BookPane::new(&s, depth, 0.0))));
                self.status = format!("{s}: order book");
            }
            Command::SymbolFunction(s, Function::Options) => {
                self.open_panel(PaneKind::Options(Box::new(OptionsPane::new(&s, 12, true))));
                self.status = format!("{s}: option chain");
            }
            Command::SymbolFunction(s, Function::Portfolio(view, arg)) => self.portfolio_command(view, &arg, Some(s)),
            Command::SymbolFunction(s, Function::Trade(mut t)) => {
                t.symbol = Some(s);
                self.open_ticket(t, None);
            }
            Command::SymbolFunction(s, f @ (Function::NewPanel(_) | Function::Split { .. })) => {
                self.apply_with_symbol(f, Some(s));
            }
            Command::SymbolFunction(s, f) => {
                self.open_security(&s);
                self.apply(f);
            }
            Command::Function(f) => self.apply(f),
        }
    }

    fn apply(&mut self, f: Function) {
        self.apply_with_symbol(f, None);
    }

    fn apply_with_symbol(&mut self, f: Function, symbol: Option<String>) {
        match f {
            Function::Date(date) => match chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d") {
                Ok(date) => self.with_chart(|c, _| c.jump_to(date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp())),
                Err(_) => self.status = "Use DATE YYYY-MM-DD".into(),
            },
            Function::Step(delta) => {
                let watchlist = self.watchlist.clone();
                self.with_chart(|c, w| {
                    let symbols =
                        if c.data.workflow.symbols.is_empty() { &watchlist } else { &c.data.workflow.symbols };
                    if let Some(symbol) = crate::chart_workflow::next(symbols, &c.symbol, delta) {
                        c.set_symbol(&symbol, w);
                    }
                });
            }
            Function::Timeframe(tf) => self.with_chart(|c, w| c.set_timeframe(tf, w)),
            Function::ChartType(t) => self.with_chart(|c, _| c.chart_type = t),
            Function::RenkoBox(size) => self.with_chart(|c, _| {
                c.data.analytics.renko_size = size.unwrap_or(0.);
                c.chart_type = tr_chart::scene::ChartType::Renko;
            }),
            Function::Indicator(spec) => self.with_chart(|c, w| c.toggle_indicator(spec, w)),
            Function::IndicatorPicker(q) => self.palette.open_scoped(crate::palette::Kind::Indicator, &q),
            Function::ClearIndicators => self.with_chart(|c, _| c.clear_indicators()),
            Function::Compare(s) => self.with_chart(|c, w| c.set_compare(s, w)),
            Function::LogScale => self.with_chart(|c, _| c.toggle_log_scale()),
            Function::Range(r) => self.with_chart(|c, w| c.set_range(r, w)),
            Function::Draw(t) => self.with_chart(|c, _| c.start_tool(t)),
            Function::DrawingBar(shown) => {
                self.prefs.show_drawing_rail = shown;
                self.save_prefs();
            }
            Function::ClearDrawings => self.with_chart(|c, _| c.clear_drawings()),
            Function::Magnet => self.with_chart(|c, _| c.magnet = !c.magnet),
            Function::Congress => self.open_panel(PaneKind::Feed { filter: FeedFilter::default() }),
            Function::Politician(q) if q.trim().is_empty() => self.open_panel(PaneKind::Politicians),
            Function::Politician(q) => self.open_panel(PaneKind::Profile { query: q, data: None, loading: true }),
            Function::Track(q) => self.worker.send(Request::Track { query: q, on: true }),
            Function::Untrack(q) => self.worker.send(Request::Track { query: q, on: false }),
            Function::Alerts => {
                self.worker.send(Request::Alerts);
                self.open_panel(PaneKind::Alerts);
            }
            Function::Refresh => self.worker.send(Request::RefreshCongress),
            Function::Watchlist => self.open_panel(PaneKind::Watchlist),
            Function::Scanner => self.open_panel(PaneKind::Scanner(Box::default())),
            Function::ColumnAdd(mut column) => {
                if self.watch_columns.columns.len() >= tr_core::columns::MAX_COLUMNS {
                    self.status = "At most 8 columns are supported".into();
                } else if self.watch_columns.columns.iter().any(|c| c.title.eq_ignore_ascii_case(&column.title)) {
                    self.status = "A column with that title already exists".into();
                } else {
                    column.id = self.watch_columns.columns.iter().map(|c| c.id).max().unwrap_or(0).saturating_add(1);
                    self.watch_columns.columns.push(column);
                }
                self.refresh_columns();
            }
            Function::ColumnDelete(title) => {
                self.watch_columns.columns.retain(|c| !c.title.eq_ignore_ascii_case(&title));
                self.watch_sort = None;
                self.refresh_columns();
            }
            Function::ColumnEdit => {
                self.columns_editor = Some(columns_view::ColumnsEditor::new(&self.watch_columns));
            }
            Function::ColumnClear => {
                self.watch_columns.columns.clear();
                self.watch_sort = None;
                self.refresh_columns();
            }
            Function::WatchNew(name) => {
                let result = self.create_watchlist(name, Vec::new());
                self.status = result.err().unwrap_or_else(|| "Watchlist created".into());
            }
            Function::WatchLists => {
                self.status = self.watchlists.iter().map(|l| l.name.as_str()).collect::<Vec<_>>().join(", ")
            }
            Function::WatchUse(ref name) | Function::WatchDelete(ref name) | Function::WatchRename(ref name, _) => {
                if let Some(index) =
                    self.watchlists.iter().position(|l| watchlist_name_key(&l.name) == watchlist_name_key(name.trim()))
                {
                    let result = match f {
                        Function::WatchDelete(_) => self.delete_watchlist(index),
                        Function::WatchRename(_, new) => self.rename_watchlist(index, new),
                        _ => {
                            self.switch_watchlist(index);
                            Ok(())
                        }
                    };
                    self.status = result.err().unwrap_or_else(|| "Watchlist updated".into());
                } else {
                    self.status = format!("Watchlist not found: {name}");
                }
            }
            Function::WatchAdd(words) => self.add_watchlist_symbols(keys_from_words(&words)),
            Function::WatchRemove(words) => {
                let keys = keys_from_words(&words);
                let next = self.watchlist.iter().filter(|s| !keys.contains(s)).cloned().collect();
                self.set_watchlist(next);
            }
            Function::Layout(arg) => self.layout_command(&arg),
            Function::Layouts => {
                self.settings_tab = SettingsTab::Layouts;
                self.open_panel(PaneKind::Settings);
                self.status = format!("Layouts: {}", self.layout_names.join(" · "));
            }
            Function::NewPanel(spec) => {
                let kind = self.panel_from_spec(&spec, symbol.as_deref());
                match kind {
                    Some(kind) => {
                        let target = self.focused_visible();
                        let id = self.new_pane(kind);
                        self.dock_pane(id, target, crate::dock::Zone::Right);
                    }
                    None => self.status = format!("Unknown panel {spec:?}; try CHART, NEWS, BOOK, WEI, GC, CONG, WL"),
                }
            }
            Function::Split { down, panel } => match self.panel_from_spec(&panel, symbol.as_deref()) {
                Some(kind) => {
                    let target = self.focused_visible();
                    let id = self.new_pane(kind);
                    let zone = if down { crate::dock::Zone::Bottom } else { crate::dock::Zone::Right };
                    self.dock_pane(id, target, zone);
                }
                None => self.status = format!("Unknown panel {panel:?}"),
            },
            Function::ClosePane => self.close_pane(self.focus),
            Function::PopOut => self.pop_out(self.focus),
            Function::Link(link) => {
                let focus = self.focus;
                self.set_link(focus, link);
            }
            Function::News => self.open_panel(PaneKind::News(Box::new(NewsPane::new(self.focused_symbol())))),
            // N2
            Function::Filings => {
                self.open_panel(PaneKind::Filings(crate::filings_view::Settings::new(self.focused_symbol())))
            }
            Function::OrderBook(depth) => {
                let sym = self.focused_symbol().unwrap_or_else(|| "BTCUSD Curncy".into());
                let depth = depth.unwrap_or(self.prefs.panels.book_depth).clamp(5, 200);
                self.open_panel(PaneKind::OrderBook(Box::new(BookPane::new(&sym, depth, 0.0))));
            }
            Function::Settings => self.open_panel(PaneKind::Settings),
            Function::Update => {
                if !tr_macos::updater::check_for_updates() {
                    let url = format!("{}/latest", tr_macos::updater::RELEASES);
                    self.status = match broker::open_external(&url) {
                        Ok(()) => "Opened the latest release on GitHub".into(),
                        Err(e) => format!("Could not open {url}: {e}"),
                    };
                }
            }
            Function::Options => {
                let sym = self.optionable_symbol();
                self.open_panel(PaneKind::Options(Box::new(OptionsPane::new(&sym, 12, true))));
            }
            Function::Sidebar(arg) => {
                let side = &mut self.prefs.appearance.sidebar_side;
                match arg.as_str() {
                    "LEFT" => *side = prefs::SidebarSide::Left,
                    "RIGHT" => *side = prefs::SidebarSide::Right,
                    "SWAP" | "FLIP" | "OTHER" => *side = side.other(),
                    "SHOW" | "ON" => self.sidebar_open = true,
                    "HIDE" | "OFF" => self.sidebar_open = false,
                    _ => self.sidebar_open = !self.sidebar_open,
                }
                self.save_prefs();
            }
            Function::Theme(t) => {
                let system = self.system_theme;
                self.theme_command(&t, system);
            }
            Function::Title(t) => {
                let focus = self.focus;
                if let Some(p) = self.pane_mut(focus) {
                    p.title = (!t.trim().is_empty()).then(|| t.trim().to_string());
                }
            }
            Function::Help => self.open_panel(PaneKind::Help),
            Function::Agent => self.open_panel(PaneKind::Agent(crate::agent_pane::Settings::default())),
            Function::Wei => {
                self.last_wei = None;
                self.open_panel(PaneKind::Wei);
            }
            Function::Curve => {
                self.worker.send(Request::YieldCurves);
                self.last_curve = Some(Instant::now());
                self.open_panel(PaneKind::Curve);
            }
            Function::Search(q) => {
                self.symbol_search = Some(crate::symbol_search::Dialog::new(q, None));
            }
            Function::Portfolio(view, arg) => self.portfolio_command(view, &arg, None),
            Function::Trade(t) => self.open_ticket(t, None),
            Function::AccountMode(live) => {
                let mode = if live { prefs::AccountMode::Live } else { prefs::AccountMode::Demo };
                if mode == self.prefs.trading.account_mode {
                    self.status = format!("Already using the {} account", if live { "LIVE" } else { "DEMO" });
                }
                self.set_account_mode(mode, false);
            }
        }
    }

    // ---- worker responses ----------------------------------------------------------------

    fn chart_mut(&mut self, pane: PaneId) -> Option<&mut ChartState> {
        match self.panes.iter_mut().find(|p| p.id == pane).map(|p| &mut p.kind) {
            Some(PaneKind::Chart(c)) => {
                c.drawing_ids.attach(self.drawing_ids.clone());
                c.drawing_ids.repair(&mut c.drawings);
                Some(c)
            }
            _ => None,
        }
    }

    /// Resolve a persistent chart identity; a stale pin follows the active chart.
    pub fn agent_chart_uid(&self, target: crate::agent_queue::ChartTarget) -> Option<u64> {
        use crate::agent_queue::ChartTarget;
        let exists = |uid| self.panes.iter().any(|p| matches!(&p.kind, PaneKind::Chart(c) if c.uid == uid));
        match target {
            ChartTarget::Application => None,
            ChartTarget::Uid(uid) => exists(uid).then_some(uid),
            ChartTarget::Pinned => {
                self.pinned_agent_chart.filter(|uid| exists(*uid)).or_else(|| self.peek_chart().map(|c| c.uid))
            }
        }
    }

    pub fn charts(&self) -> Vec<crate::chart_view::ChartRead<'_>> {
        self.panes
            .iter()
            .filter_map(|p| match &p.kind {
                PaneKind::Chart(c) => Some(c.read_chart()),
                _ => None,
            })
            .collect()
    }

    pub fn agent_chart(&self, target: crate::agent_queue::ChartTarget) -> Option<&ChartState> {
        let uid = self.agent_chart_uid(target)?;
        self.panes.iter().find_map(|p| match &p.kind {
            PaneKind::Chart(c) if c.uid == uid => Some(c.as_ref()),
            _ => None,
        })
    }

    pub fn read_user_actions(&self, limit: usize) -> Vec<&crate::edit_log::EditEvent> {
        self.edit_log.newest(limit, true)
    }

    pub fn agent_worker(&self) -> tokio::sync::mpsc::UnboundedSender<Request> {
        self.worker.sender()
    }

    pub fn agent_chart_mut(&mut self, target: crate::agent_queue::ChartTarget) -> Option<&mut ChartState> {
        let uid = self.agent_chart_uid(target)?;
        let pane = self.panes.iter().find_map(|p| match &p.kind {
            PaneKind::Chart(c) if c.uid == uid => Some(p.id),
            _ => None,
        })?;
        self.chart_mut(pane)
    }

    /// The target resolved immediately before the current queued job started.
    pub fn job_chart_mut(&mut self) -> Option<&mut ChartState> {
        self.agent_chart_mut(crate::agent_queue::ChartTarget::Uid(self.agent_job_chart?))
    }

    fn observe_edits(&mut self, record: bool) {
        self.edit_log
            .observed
            .retain(|uid, _| self.panes.iter().any(|p| matches!(&p.kind, PaneKind::Chart(c) if c.uid == *uid)));
        for pane in &mut self.panes {
            let PaneKind::Chart(chart) = &mut pane.kind else { continue };
            chart.drawing_ids.attach(self.drawing_ids.clone());
            chart.drawing_ids.repair(&mut chart.drawings);
            chart.finish_drawing_edit();
            for edit in chart.drawing_events.drain(..) {
                for operation in edit.operations {
                    self.edit_log.record(
                        chart.uid,
                        edit.author,
                        edit.turn.clone(),
                        operation.kind(),
                        serde_json::json!({ "ids": operation.ids() }),
                    );
                }
            }
            let current = chart.edit_observation();
            if let Some(previous) = self.edit_log.observed.insert(chart.uid, current.clone())
                && record
            {
                for (key, kind) in [("indicators", "indicator"), ("comparisons", "comparison")] {
                    self.edit_log.observe_set(
                        chart.uid,
                        chart.edit_author,
                        chart.edit_turn.clone(),
                        kind,
                        &previous[key],
                        &current[key],
                    );
                }
                for (key, kind) in [("symbol", "symbol"), ("timeframe", "timeframe"), ("range", "range")] {
                    if current[key] != previous[key] {
                        self.edit_log.record(
                            chart.uid,
                            chart.edit_author,
                            chart.edit_turn.clone(),
                            kind,
                            current[key].clone(),
                        );
                    }
                }
            }
        }
    }

    fn process_logic(&mut self) {
        use crate::agent_queue::{AgentReply, MAX_JOB_TIME_PER_FRAME, MAX_JOBS_PER_FRAME};
        use tr_chart::drawings::Author;
        self.observe_edits(true);
        self.drain();
        // History/live changes are not user edits.
        self.observe_edits(false);
        for pane in &mut self.panes {
            if let PaneKind::Chart(chart) = &mut pane.kind
                && chart.finish_agent_view().is_some()
            {
                self.agent_save_pending = true;
            }
        }
        self.observe_edits(true);
        for pane in &mut self.panes {
            if let PaneKind::Chart(chart) = &mut pane.kind {
                chart.edit_author = Author::User;
                chart.edit_turn = None;
            }
        }
        self.agent_queue.expire();
        let started = Instant::now();
        let mut executed = 0;
        for _ in 0..self.agent_queue.len().min(MAX_JOBS_PER_FRAME) {
            if started.elapsed() >= MAX_JOB_TIME_PER_FRAME {
                break;
            }
            let Some(job) = self.agent_queue.pop() else { break };
            executed += 1;
            let reply = if job.deadline <= Instant::now() {
                AgentReply::error("timeout")
            } else if matches!(job.chart, crate::agent_queue::ChartTarget::Application) {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (job.run)(self)))
                    .unwrap_or_else(|_| AgentReply::error("job panicked; edits may be partial"))
            } else if let Some(uid) = self.agent_chart_uid(job.chart) {
                self.agent_job_chart = Some(uid);
                for pane in &mut self.panes {
                    if let PaneKind::Chart(c) = &mut pane.kind {
                        c.begin_agent_edit(&job.turn);
                    }
                }
                // Jobs own no queue lock. Unwinding may leave partial edits, but Rust's
                // exclusive borrow remains valid; report failure without promising rollback.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (job.run)(self)))
                    .unwrap_or_else(|_| AgentReply::error("job panicked; edits may be partial"));
                self.observe_edits(true);
                for pane in &mut self.panes {
                    if let PaneKind::Chart(c) = &mut pane.kind {
                        c.edit_author = Author::User;
                        c.edit_turn = None;
                    }
                }
                self.agent_job_chart = None;
                result
            } else {
                AgentReply::error("chart not found")
            };
            let _ = job.reply.send(reply);
        }
        let queue = self.agent_queue.clone();
        queue.check_waiting(self, started, MAX_JOBS_PER_FRAME - executed);
        self.agent_queue.expire();
        self.agent_queue.repaint_pending();
        if self.agent_save_pending {
            self.autosave();
        }
    }

    fn drain(&mut self) {
        while let Ok(r) = self.worker.rx.try_recv() {
            match r {
                // N2: shared store snapshot, never clear rows on a failed refresh.
                Response::Edgar { rows, status, updated, flags } => {
                    self.edgar.rows = rows;
                    self.edgar.status = status;
                    self.edgar.updated = updated;
                    self.edgar.flags = flags;
                }
                Response::WatchlistColumns { revision, cells, computed_at } => {
                    if let Some((cells, computed_at)) = finish_columns_reply(
                        &mut self.columns_in_flight,
                        revision,
                        self.columns_revision,
                        (cells, computed_at),
                    ) {
                        self.column_cells = cells;
                        self.columns_computed_at = computed_at;
                    }
                    self.refresh_columns();
                }
                Response::Scan { pane, generation, rows } => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane)
                        && let PaneKind::Scanner(s) = &mut p.kind
                        && s.generation == generation
                    {
                        s.finish(generation, rows);
                    }
                }
                Response::ScanPartial { pane, generation, rows } => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane)
                        && let PaneKind::Scanner(s) = &mut p.kind
                    {
                        s.partial(generation, rows);
                    }
                }
                Response::CryptoScanStats { pane, generation, symbol, result } => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane)
                        && let PaneKind::Scanner(s) = &mut p.kind
                    {
                        s.crypto_stats(generation, symbol, result);
                    }
                }
                Response::ScanUniverseInfo { pane, generation, available, selected, fetched_at } => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane)
                        && let PaneKind::Scanner(s) = &mut p.kind
                    {
                        s.catalog_info(generation, available, selected, fetched_at);
                    }
                }
                Response::ScanProgress { pane, generation, done, total } => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane)
                        && let PaneKind::Scanner(s) = &mut p.kind
                        && s.generation == generation
                    {
                        s.set_progress(done, total);
                    }
                }
                Response::History {
                    pane,
                    generation,
                    revision,
                    symbol,
                    tf,
                    bars,
                    gpu,
                    prepended,
                    from_cache,
                    exhausted,
                    live,
                    crypto_source,
                } => {
                    let worker = &self.worker;
                    if let Some(PaneKind::Chart(c)) = self.panes.iter_mut().find(|p| p.id == pane).map(|p| &mut p.kind)
                        && c.symbol == symbol
                        && c.tf == tf
                    {
                        c.on_crypto_source(generation, crypto_source);
                        c.on_history_revision(
                            generation, revision, bars, gpu, prepended, from_cache, exhausted, live, worker,
                        );
                    }
                }
                Response::SessionHistory { history, bars, gpu } => {
                    let worker = &self.worker;
                    if let Some(PaneKind::Chart(c)) =
                        self.panes.iter_mut().find(|p| p.id == history.pane).map(|p| &mut p.kind)
                    {
                        c.on_session_history(history, bars, gpu, worker);
                    }
                }
                Response::LiveBars { pane, generation, revision, from, bars, len } => {
                    let worker = &self.worker;
                    if let Some(PaneKind::Chart(c)) = self.panes.iter_mut().find(|p| p.id == pane).map(|p| &mut p.kind)
                    {
                        c.on_live_bars_revision(generation, revision, from, bars, len, worker);
                    }
                }
                Response::HistoryError { pane, generation, error } => {
                    // Errors while paging just mean "no older history"; only report failed loads.
                    let empty = self.chart_mut(pane).map(|c| {
                        c.on_history_error(generation, error.clone());
                        c.is_empty()
                    });
                    if empty == Some(true) {
                        self.status = format!("data error: {error}");
                    }
                }
                Response::Indicators { pane, generation, built, revision } => {
                    let worker = &self.worker;
                    if let Some(PaneKind::Chart(c)) = self.panes.iter_mut().find(|p| p.id == pane).map(|p| &mut p.kind)
                    {
                        c.on_indicators(generation, revision, built, worker);
                    }
                }
                Response::LiveStatus { pane, generation, message } => {
                    if let Some(c) = self.chart_mut(pane) {
                        c.on_live_status(generation, message);
                    }
                }
                Response::ChartEvents { pane, generation, symbol, start, end, events, notice } => {
                    if let Some(c) = self.chart_mut(pane) {
                        c.on_events(generation, &symbol, (start, end), events, notice);
                    }
                }
                Response::ExportChart { pane, result } => {
                    self.status = match &result {
                        Ok(path) => {
                            format!("Exported {}", path.file_name().unwrap_or(path.as_os_str()).to_string_lossy())
                        }
                        Err(err) => format!("Export failed: {err}"),
                    };
                    if let Some(c) = self.chart_mut(pane) {
                        c.export_notice = Some(result);
                    }
                }
                Response::ChartView { pane, generation, revision, source, result } => {
                    let worker = &self.worker;
                    if let Some(PaneKind::Chart(c)) = self.panes.iter_mut().find(|p| p.id == pane).map(|p| &mut p.kind)
                    {
                        c.on_view(generation, revision, source, result, worker);
                    }
                }
                Response::Heikin { pane, generation, bars, gpu } => {
                    if let Some(c) = self.chart_mut(pane) {
                        c.on_heikin(generation, bars, gpu);
                    }
                }
                Response::Compare { pane, generation, symbol, line, revision } => {
                    if let Some(c) = self.chart_mut(pane) {
                        c.on_compare(generation, revision, &symbol, line);
                    }
                }
                Response::CompareError { pane, generation, revision, symbol, error } => {
                    if let Some(c) = self.chart_mut(pane) {
                        c.on_compare_error(generation, revision, &symbol, error);
                    }
                }
                Response::CompareSearch { pane, query, result } => {
                    if let Some(c) = self.chart_mut(pane) {
                        c.on_compare_search(&query, result);
                    }
                }
                Response::Markers { pane, symbol, trades } => {
                    if let Some(c) = self.chart_mut(pane) {
                        c.on_markers(&symbol, trades);
                    }
                }
                Response::PriceAlertQuotes { revision, observations } => {
                    for event in self.price_alerts.on_quotes(revision, observations) {
                        self.alert_toast(event);
                    }
                }
                Response::PriceAlertRules { revision, observations } => {
                    for event in self.price_alerts.on_rules(revision, observations) {
                        self.alert_toast(event);
                    }
                }
                Response::Quotes(q) => {
                    if q.len() > 20 {
                        self.wei_updated = Some(format!("updated {}", chrono::Local::now().format("%H:%M:%S")));
                    }
                    let keys: Vec<String> = q.iter().map(|x| x.symbol.clone()).collect();
                    for x in q {
                        self.quotes.insert(x.symbol.clone(), x);
                    }
                    self.demo_on_quotes(&keys);
                }
                Response::Latest(t) => self.latest = t,
                Response::TickerTrades { pane, symbol, trades } => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane)
                        && let PaneKind::Ticker { symbol: s, trades: t, loading } = &mut p.kind
                        && *s == symbol
                    {
                        *t = trades;
                        *loading = false;
                    }
                }
                Response::Profile { pane, profile, query } => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane)
                        && let PaneKind::Profile { query: q, data, loading } = &mut p.kind
                        && *q == query
                    {
                        *data = profile;
                        *loading = false;
                    }
                }
                Response::Options { pane, symbol, chain } => {
                    // Greeks use the 3-month T-bill yield when the curve is loaded.
                    let rate = self.curves.as_ref().and_then(|c| c.current.get("US3M")).map_or(0.04, |y| y / 100.0);
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane)
                        && let PaneKind::Options(o) = &mut p.kind
                        && o.symbol == symbol
                    {
                        match chain {
                            Ok(c) => o.set_chain(c, rate, chrono::Utc::now().timestamp()),
                            Err(e) => {
                                o.error = Some(e);
                                o.loading = false;
                            }
                        }
                    }
                }
                // N1b: stale query replies cannot replace a newly selected scope.
                Response::NewsStatus { view, status, updated } => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == view.pane)
                        && let PaneKind::News(n) = &mut p.kind
                        && n.query.as_ref() == Some(&view)
                    {
                        n.updated = updated;
                        n.status = status;
                    }
                }
                Response::NewsSaved { view, items, status, updated } => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == view.pane)
                        && let PaneKind::News(n) = &mut p.kind
                        && n.query.as_ref() == Some(&view)
                    {
                        n.loading = false;
                        n.updated = updated;
                        n.status = status;
                        match items {
                            Ok(items) => {
                                n.apply(items);
                                n.error = None;
                            }
                            Err(e) => n.error = Some(e),
                        }
                    }
                }
                // N1b
                Response::OrderBook { pane, symbol, book } => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane)
                        && let PaneKind::OrderBook(b) = &mut p.kind
                        && b.symbol == symbol
                    {
                        match book {
                            Ok(book) => {
                                b.book = Some(book);
                                b.error = None;
                            }
                            Err(e) => b.error = Some(e),
                        }
                    }
                }
                Response::Portfolio(result) => {
                    let result = self.keep_simulated_trades(result);
                    self.on_portfolio(result)
                }
                Response::BrokerRegistered(result) => self.on_broker_registered(result),
                Response::DataSources(reply) => self.data_sources.receive(reply),
                Response::Questrade(result) => {
                    self.broker.questrade_busy = false;
                    self.broker.questrade_message = Some(match result {
                        Ok(m) => (m, false),
                        Err(e) => (e, true),
                    });
                }
                Response::BrokerPortal(result) => self.on_broker_portal(result),
                Response::BrokerDisconnected(result) => self.on_broker_disconnected(result),
                Response::Politicians(list) => self.politicians = list,
                Response::Alerts(a) => self.alerts = a,
                Response::NewAlerts(new) => {
                    for a in new {
                        tracing::info!("ALERT: {}", a.message);
                        self.toasts.push((a.message, Instant::now()));
                    }
                    self.worker.send(Request::Alerts);
                }
                Response::Status(s) => self.status = s,
                Response::Curves(c) => self.curves = Some(c),
                Response::SecurityNames(names) => self.security_names = names,
                Response::SymbolCatalog { venue, result } => {
                    if let Some(dialog) = &mut self.symbol_search {
                        dialog.catalog(venue, result);
                    }
                }
                Response::SearchHits { query, hits } => {
                    if let Some(dialog) = &mut self.symbol_search {
                        dialog.network(&query, &hits);
                    }
                    self.net_hits = (query, hits);
                }
                Response::Trade(r) => self.on_trade(*r),
                Response::CongressPaused(summary) => self.show_congress_pause(summary),
                Response::CongressRefreshed(summary) => {
                    self.congress_status = summary.to_string();
                    self.congress_checked = Some(chrono::Local::now());
                    self.status = if summary.contains("source paused until") {
                        summary
                    } else {
                        format!("Congress disclosures updated: {summary}")
                    };
                    self.refresh_congress_views();
                }
            }
        }
    }

    /// A persisted pause is display state, not evidence of a completed check.
    fn show_congress_pause(&mut self, summary: String) {
        self.congress_status = summary.clone();
        self.status = summary;
    }

    /// Freshness line for the Congress feed: last check, newest filing, next check.
    fn congress_freshness(&self) -> String {
        let newest = self.latest.iter().map(|t| t.filed_date).max();
        let minutes = crate::worker::congress_refresh_minutes() as i64;
        let mut parts = Vec::new();
        match self.congress_checked {
            Some(t) => {
                parts.push(format!("checked {}", t.format("%H:%M")));
                if minutes > 0 {
                    parts.push(format!("next {}", (t + chrono::Duration::minutes(minutes)).format("%H:%M")));
                }
            }
            None if self.congress_status.contains("source paused until") => {}
            None if minutes == 0 => parts.push("Congress refresh disabled".into()),
            None => parts.push("checking Senate eFD + House Clerk…".into()),
        }
        if let Some(d) = newest {
            parts.push(format!("newest filing {d}"));
        }
        if !self.congress_status.is_empty() {
            parts.push(self.congress_status.clone());
        }
        parts.join(" · ")
    }

    fn refresh_congress_views(&mut self) {
        self.worker.send(Request::Latest { limit: 2_000 });
        self.worker.send(Request::SearchPoliticians(String::new()));
        for p in &self.panes {
            match &p.kind {
                PaneKind::Ticker { symbol, .. } => {
                    self.worker.send(Request::TickerTrades { pane: p.id, symbol: symbol.clone() })
                }
                PaneKind::Chart(c) => self.worker.send(Request::Markers { pane: p.id, symbol: c.symbol.clone() }),
                _ => {}
            }
        }
        self.last_feed = Some(Instant::now());
    }

    // ---- input ---------------------------------------------------------------------------

    /// Global and chart hotkeys. Runs before the command line so it can claim keys; when
    /// the command line holds text, keys belong to it.
    fn hotkeys(&mut self, ctx: &egui::Context) {
        // A rebindable chord, checked before text-field ownership; never steals palette/rebinding keys.
        if self.rebinding.is_none() && !self.palette.open {
            let focus = ctx.input(|input| {
                input.events.iter().find_map(|event| match event {
                    Event::Key { key, modifiers, pressed: true, repeat: false, .. }
                        if self.prefs.hotkeys.action_for(*key, *modifiers) == Some(prefs::Action::FocusAgentInput) =>
                    {
                        Some((*key, *modifiers))
                    }
                    _ => None,
                })
            });
            if let Some((key, modifiers)) = focus
                && self.focus_agent_input(ctx)
            {
                ctx.input_mut(|input| input.consume_key(modifiers, key));
                return;
            }
        }
        if self.agent_escape(ctx) || (!self.palette.open && self.agent_captures_focus(ctx)) {
            return;
        }
        let mut presses: Vec<(Key, Modifiers)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::Key { key, pressed: true, repeat: false, modifiers, .. } if !prefs::is_modifier(*key) => {
                        Some((*key, *modifiers))
                    }
                    _ => None,
                })
                .collect()
        });
        // Settings is capturing a new binding: the next key press is it.
        if let Some(action) = self.rebinding {
            if let Some(&(key, m)) = presses.first() {
                ctx.input_mut(|i| i.consume_key(m, key));
                self.rebinding = None;
                if key != Key::Escape {
                    let chord = prefs::Chord::from_event(key, m);
                    let displaced = self.prefs.hotkeys.bind(action, chord);
                    self.status = match displaced {
                        Some(d) => {
                            format!("{} bound to {} (removed from {})", action.label(), chord.display(), d.label())
                        }
                        None => format!("{} bound to {}", action.label(), chord.display()),
                    };
                    self.save_prefs();
                }
            }
            return;
        }
        // ⌘K (Ctrl+K) opens and closes the palette from anywhere, even from a text field.
        if let Some(&(key, m)) =
            presses.iter().find(|(k, m)| self.prefs.hotkeys.action_for(*k, *m) == Some(prefs::Action::CommandPalette))
        {
            ctx.input_mut(|i| i.consume_key(m, key));
            self.palette.toggle();
            return;
        }
        // While it is open, the palette owns the keyboard.
        if self.palette.open {
            return;
        }
        let cmd_id = egui::Id::new("command-line");
        let focused = ctx.memory(|m| m.focused());
        let cmd_focused = focused == Some(cmd_id);
        let other_text_focused = focused.is_some() && !cmd_focused;
        if other_text_focused {
            return;
        }
        // N3: feed navigation wins over type-anywhere and unmodified desk hotkeys.
        if !cmd_focused
            && self.cmd.is_empty()
            && let Some(index) = self.idx(self.focus)
            && let PaneKind::Filings(settings) = &self.panes[index].kind
        {
            let handled: Vec<_> = [
                Key::ArrowUp,
                Key::ArrowDown,
                Key::J,
                Key::K,
                Key::Enter,
                Key::Space,
                Key::C,
                Key::Slash,
                Key::Escape,
                Key::U,
            ]
            .into_iter()
            .filter(|key| crate::filings_view::pane_uses_key(ctx, egui::Id::new(self.focus), settings, *key))
            .collect();
            let uses = |key| handled.contains(&key);
            // Include auto-repeat. Strip only Text paired with a consumed unmodified key;
            // leave Key events for the pane, and Shift+letters for type-anywhere.
            ctx.input_mut(|i| {
                let mut pane_text = false;
                i.events.retain(|e| match e {
                    Event::Key { key, modifiers, pressed: true, .. } => {
                        pane_text = *modifiers == Modifiers::NONE && uses(*key);
                        true
                    }
                    Event::Text(_) => !pane_text,
                    _ => true,
                });
            });
            presses.retain(|(key, modifiers)| *modifiers != Modifiers::NONE || !uses(*key));
        }
        // N3
        // N1b: consume only feed-owned events; unrelated chords and text still
        // run through this pass. Repeats must be removed before type-anywhere.
        if !cmd_focused
            && self.cmd.is_empty()
            && let Some(i) = self.idx(self.focus)
            && let PaneKind::News(n) = &mut self.panes[i].kind
        {
            let keys = ctx.input_mut(|input| {
                news_view::capture_keys(input, n.interaction.selected.is_some(), !n.filters.search.is_empty())
            });
            presses.retain(|(key, modifiers)| *modifiers != Modifiers::NONE || !keys.contains(key));
            n.interaction.pending_keys.extend(keys);
        }
        // N1b
        if !cmd_focused && self.cmd.is_empty() && self.focused_is_chart() {
            let mut handled = false;
            self.with_chart(|chart, worker| handled = chart.replay_keys(ctx, worker));
            if handled {
                return;
            }
        }
        // Chorded bindings (⌘, Ctrl, Alt) work even while the command line has text.
        let mut swallow_text = false;
        for &(key, m) in &presses {
            let Some(action) = self.prefs.hotkeys.action_for(key, m) else { continue };
            let chorded = m.command || m.ctrl || m.alt;
            if !chorded && !self.cmd.is_empty() {
                continue;
            }
            // ⇧B / ⇧S type letters: never while the command line is being typed into.
            let letter = !chorded && Self::types_text(key);
            if letter && cmd_focused {
                continue;
            }
            if !self.hotkey_applies(action) {
                continue;
            }
            ctx.input_mut(|i| i.consume_key(m, key));
            swallow_text |= letter;
            self.run_hotkey(action);
        }
        if !cmd_focused && !swallow_text {
            // Type-anywhere: capture keystrokes until the command line has focus.
            // Judge modifiers per key event: the Alt release can arrive in the same frame
            // as Alt+T, so the frame-level modifier state is unreliable.
            let typed: String = ctx.input(|i| {
                let mut chord = false;
                let mut out = String::new();
                for e in &i.events {
                    match e {
                        Event::Key { pressed: true, modifiers, .. } => {
                            chord = modifiers.alt || modifiers.ctrl || modifiers.command
                        }
                        Event::Text(t) if !chord => out.push_str(t),
                        _ => {}
                    }
                }
                out
            });
            if !typed.is_empty() {
                let starts = !self.cmd.is_empty()
                    || typed.chars().next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '^' || c == '?');
                if starts {
                    self.cmd.push_str(&typed.to_ascii_uppercase());
                    self.focus_cmd = true;
                }
            }
        }
        if !self.cmd.is_empty() || !self.focused_is_chart() {
            return;
        }
        let pressed = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        if pressed(Modifiers::NONE, Key::Escape) {
            self.with_chart(|c, _| {
                c.cancel();
            });
        }
        let backspace = pressed(Modifiers::NONE, Key::Backspace);
        if pressed(Modifiers::NONE, Key::Delete) || backspace {
            self.with_chart(|c, _| {
                if !backspace || !c.pattern_backspace() {
                    c.delete_selected();
                }
            });
        }
        if pressed(Modifiers { shift: true, ..Modifiers::COMMAND }, Key::Z) {
            self.with_chart(|c, _| c.redo_drawings());
        } else if pressed(Modifiers::COMMAND, Key::Z) {
            self.with_chart(|c, _| c.undo_drawings());
        }
        if pressed(Modifiers::NONE, Key::ArrowLeft) {
            self.with_chart(|c, _| c.pan_bars(-5.0));
        }
        if pressed(Modifiers::NONE, Key::ArrowRight) {
            self.with_chart(|c, _| c.pan_bars(5.0));
        }
        if pressed(Modifiers::NONE, Key::ArrowDown) {
            self.apply(Function::Step(1));
        }
        if pressed(Modifiers::NONE, Key::ArrowUp) {
            self.apply(Function::Step(-1));
        }
        if pressed(Modifiers::COMMAND, Key::ArrowUp)
            || pressed(Modifiers::NONE, Key::Plus)
            || pressed(Modifiers::NONE, Key::Equals)
        {
            self.with_chart(|c, _| c.zoom(1.25));
        }
        if pressed(Modifiers::COMMAND, Key::ArrowDown) || pressed(Modifiers::NONE, Key::Minus) {
            self.with_chart(|c, _| c.zoom(0.8));
        }
    }

    /// Keys that also produce text (a bare or shifted letter, digit or symbol).
    fn types_text(key: Key) -> bool {
        let n = key.name();
        n.len() == 1 || matches!(key, Key::Space | Key::Minus | Key::Plus | Key::Equals | Key::Period | Key::Comma)
    }

    fn hotkey_applies(&self, a: prefs::Action) -> bool {
        use prefs::Action as A;
        match a {
            A::DrawTrendline
            | A::DrawHorizontal
            | A::DrawVertical
            | A::DrawFib
            | A::ResetView
            | A::LogScale
            | A::Magnet => self.focused_is_chart(),
            a => a.layout_slot().is_none_or(|i| i < self.layout_names.len()),
        }
    }

    fn run_hotkey(&mut self, a: prefs::Action) {
        use prefs::Action as A;
        match a {
            A::Help => self.apply(Function::Help),
            A::Congress => self.apply(Function::Congress),
            A::Politicians => self.apply(Function::Politician(String::new())),
            A::Alerts => self.apply(Function::Alerts),
            A::Watchlist => self.apply(Function::Watchlist),
            A::Refresh => self.apply(Function::Refresh),
            A::FrameTiming => self.show_perf = !self.show_perf,
            A::Portfolio => self.apply(Function::Portfolio(command::PortView::Summary, String::new())),
            A::Positions => self.apply(Function::Portfolio(command::PortView::Positions, String::new())),
            A::Orders => self.apply(Function::Portfolio(command::PortView::Orders, String::new())),
            A::Buy => self.quick_trade(true),
            A::Sell => self.quick_trade(false),
            A::NextPane => self.cycle_focus(1),
            A::PrevPane => self.cycle_focus(-1),
            A::ToggleSidebar => self.sidebar_open = !self.sidebar_open,
            A::NextTheme => self.cycle_theme(1),
            A::SwapSidebarSide => self.apply(Function::Sidebar("SWAP".into())),
            A::SplitRight => self.apply(Function::Split { down: false, panel: String::new() }),
            A::SplitDown => self.apply(Function::Split { down: true, panel: String::new() }),
            A::NewTab => {
                let target = self.focused_visible();
                let kind = self.panel_from_spec("", None).expect("chart");
                let id = self.new_pane(kind);
                self.dock_pane(id, target, crate::dock::Zone::Center);
            }
            A::ClosePane => self.close_pane(self.focus),
            A::PopOut => self.pop_out(self.focus),
            A::SaveLayout => {
                self.save_layout();
                self.status = format!("Saved layout {:?}", self.layout_name);
            }
            A::Settings => self.apply(Function::Settings),
            A::CommandPalette => self.palette.toggle(),
            A::DrawTrendline => self.with_chart(|c, _| c.start_tool(Tool::Trendline)),
            A::DrawHorizontal => self.with_chart(|c, _| c.start_tool(Tool::Horizontal)),
            A::DrawVertical => self.with_chart(|c, _| c.start_tool(Tool::Vertical)),
            A::DrawFib => self.with_chart(|c, _| c.start_tool(Tool::Fib)),
            A::ResetView => self.with_chart(|c, _| c.reset_view()),
            A::LogScale => self.with_chart(|c, _| c.toggle_log_scale()),
            A::Magnet => self.with_chart(|c, _| c.magnet = !c.magnet),
            a => {
                if let Some(spec) = a.panel_spec() {
                    self.apply(Function::NewPanel(spec.into()));
                } else if let Some(name) = a.layout_slot().and_then(|i| self.layout_names.get(i)).cloned() {
                    self.switch_layout(&name);
                }
            }
        }
    }

    fn refresh_columns(&mut self) {
        let changed = self.columns_snapshot.0 != self.watchlist || self.columns_snapshot.1 != self.watch_columns;
        if changed {
            self.columns_revision = self.columns_revision.wrapping_add(1);
            self.columns_snapshot = (self.watchlist.clone(), self.watch_columns.clone());
            self.column_cells.clear();
            self.columns_computed_at = 0;
            self.columns_requested = None;
        }
        if !self.watch_columns.columns.is_empty()
            && columns_refresh_due(self.columns_in_flight, self.columns_requested, Instant::now())
        {
            self.columns_revision = self.columns_revision.wrapping_add(1);
            self.worker.send(Request::WatchlistColumns {
                revision: self.columns_revision,
                symbols: self.watchlist.clone(),
                columns: self.watch_columns.columns.clone(),
            });
            self.columns_requested = Some(Instant::now());
            self.columns_in_flight = Some(self.columns_revision);
        }
    }

    // N1a: the worker owns timing; UI sends only changed visibility/query snapshots.
    fn news_periodic(&mut self, visible: &[usize], hidden: bool) {
        use tr_data::news_store::Scope;
        let mut views = vec![];
        if !hidden {
            for &i in visible {
                let pane = self.panes[i].id;
                if let PaneKind::News(n) = &mut self.panes[i].kind {
                    let symbols = match n.filters.scope {
                        Scope::Symbol => n.symbol.iter().cloned().collect(),
                        Scope::Watchlist => self
                            .watchlists
                            .iter()
                            .find(|l| Some(&l.name) == n.filters.watchlist.as_ref())
                            .map(|l| l.symbols.clone())
                            .unwrap_or_else(|| self.watchlist.clone()),
                        Scope::All => vec![],
                    };
                    let view =
                        crate::worker::NewsView { pane, symbol: n.symbol.clone(), filters: n.filters.clone(), symbols };
                    if n.query.as_ref() != Some(&view) {
                        n.pending.clear();
                        n.loading = true;
                        n.fetched = None;
                    }
                    n.query = Some(view.clone());
                    if n.fetched.is_none() {
                        n.fetched = Some(Instant::now());
                        self.worker.send(Request::NewsRead(view.clone()));
                    }
                    views.push(view);
                }
            }
        }
        let mut symbols: Vec<_> = if views.is_empty() {
            vec![]
        } else {
            self.watchlists.iter().flat_map(|l| l.symbols.iter().cloned()).collect()
        };
        symbols.sort();
        symbols.dedup();
        // Linked visible panes are eligible even outside every watchlist, and go first.
        let mut linked: Vec<_> =
            views.iter().filter(|v| v.filters.scope == Scope::Symbol).flat_map(|v| v.symbols.iter().cloned()).collect();
        linked.sort();
        linked.dedup();
        symbols.retain(|s| !linked.contains(s));
        linked.extend(symbols);
        let next = (views, linked);
        if self.news_subscription.as_ref() != Some(&next) {
            self.worker.send(Request::NewsVisible { views: next.0.clone(), watchlist: next.1.clone() });
            self.news_subscription = Some(next);
        }
    }
    // N1a
    fn periodic(&mut self) {
        if self.agent_demo.is_some() {
            return;
        }
        self.refresh_columns();
        self.price_alerts.periodic(&self.worker);
        let visible = self.visible_panes();
        let shown = |f: &dyn Fn(&PaneKind) -> bool| visible.iter().any(|&i| f(&self.panes[i].kind));
        let visible_wl = self.sidebar_open || shown(&|k| matches!(k, PaneKind::Watchlist));
        if visible_wl && self.last_quotes.is_none_or(|t| t.elapsed() > Duration::from_secs(20)) {
            self.worker.send(Request::Quotes(self.watchlist.clone()));
            self.last_quotes = Some(Instant::now());
        }
        if shown(&|k| matches!(k, PaneKind::Wei)) && self.last_wei.is_none_or(|t| t.elapsed() > Duration::from_secs(20))
        {
            self.worker.send(Request::Quotes(markets_view::overview_keys()));
            self.last_wei = Some(Instant::now());
        }
        if shown(&|k| matches!(k, PaneKind::Curve))
            && self.last_curve.is_none_or(|t| t.elapsed() > Duration::from_secs(600))
        {
            self.worker.send(Request::YieldCurves);
            self.last_curve = Some(Instant::now());
        }
        // News every 5 minutes and order books every second, while visible.
        for &i in &visible {
            let p = &mut self.panes[i];
            match &mut p.kind {
                // N1a: no per-frame news polling; the worker scheduler owns refreshes.
                PaneKind::Options(o) if o.fetched.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) => {
                    o.fetched = Some(Instant::now());
                    self.worker.send(Request::Options {
                        pane: p.id,
                        symbol: o.symbol.clone(),
                        expiration: o.expiration,
                    });
                }
                PaneKind::OrderBook(b) if b.fetched.is_none_or(|t| t.elapsed() > Duration::from_millis(1000)) => {
                    b.fetched = Some(Instant::now());
                    self.worker.send(Request::OrderBook { pane: p.id, symbol: b.symbol.clone(), depth: b.depth });
                }
                _ => {}
            }
        }
        self.broker_periodic(&visible);
        if let Some((q, at)) = &self.search_pending
            && at.elapsed() > Duration::from_millis(300)
        {
            self.worker.send(Request::Search(q.clone()));
            self.search_pending = None;
        }
        if self.last_feed.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
            self.worker.send(Request::Latest { limit: 2_000 });
            self.last_feed = Some(Instant::now());
        }
        self.save_dirty_themes(false);
        if self.last_save.elapsed() > Duration::from_secs(10) {
            self.autosave();
            self.last_save = Instant::now();
        }
        let undo_at = self.watchlist_undo.as_ref().map(|(at, _, _)| *at);
        self.toasts.retain(|(_, t)| Some(*t) == undo_at || t.elapsed() < Duration::from_secs(TOAST_SECS));
        self.alert_toasts.retain(|(t, _)| t.elapsed() < Duration::from_secs(TOAST_SECS));
    }

    fn symbol_search_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.symbol_search.take() else {
            return;
        };
        for request in dialog.requests() {
            self.worker.send(request);
        }
        dialog.list_name.clone_from(&self.watchlists[self.active_watchlist].name);
        let outcome = dialog.show(ctx, &self.watchlist);
        if let Some(key) = outcome.key {
            if let Some(target) = &mut dialog.target {
                let mut items = self.watchlists[self.active_watchlist].items();
                if self.watchlist.iter().any(|s| crate::symbol_search::same_key(s, &key)) {
                    items.retain(|i| !matches!(i, crate::watch_sections::Item::Symbol(s) if crate::symbol_search::same_key(s, &key)));
                    self.set_watchlist_items(&items);
                } else if target.insert(&mut items, &key) {
                    self.set_watchlist_items(&items);
                } else {
                    self.status = "Watchlist is limited to 200 symbols".into();
                }
            } else {
                self.run_actions(vec![Action::OpenChart(key)]);
            }
        }
        if !outcome.close {
            self.symbol_search = Some(dialog);
        }
    }

    /// Compare snapshots every tick, including edits with no explicit dirty marking.
    fn autosave(&mut self) {
        if self.agent_demo.is_some() {
            self.agent_save_pending = false;
            return;
        }
        if self.saves.job.as_ref().is_some_and(|job| !job.is_finished()) {
            return;
        }
        self.finish_save();
        let mut writes = self.theme_writes();
        writes.push((self.store.path(&self.layout_name), self.current_doc().to_json()));
        writes.push((self.settings_file.clone(), self.settings_json()));
        self.saves.start(writes);
        self.agent_save_pending = false;
    }

    fn complete_save(&mut self, results: Vec<SaveResult>) {
        for ((path, bytes), result) in results {
            match result {
                Ok(()) => {
                    let mut is_theme = false;
                    for theme in &mut self.themes {
                        if !theme.builtin && self.theme_store.save_path(theme) == path {
                            is_theme = true;
                            theme.source = Some(path.clone());
                            if theme.to_toml().as_bytes() == bytes {
                                self.dirty_themes.retain(|name| name != &theme.name);
                            }
                        }
                    }
                    if !is_theme {
                        self.saves.written.insert(path, bytes);
                    }
                }
                Err(e) => {
                    tracing::warn!("saving {}: {e}", path.display());
                    self.status = format!("Could not save {}: {e}; will retry", path.display());
                }
            }
        }
        if self.dirty_themes.is_empty() {
            self.themes_changed_at = None;
        }
    }

    fn finish_save(&mut self) {
        if let Some(job) = self.saves.job.take() {
            match job.join() {
                Ok(results) => self.complete_save(results),
                Err(_) => self.status = "Could not autosave; will retry".into(),
            }
        }
    }

    fn save_sync(&mut self, writes: Vec<SaveWrite>) {
        if self.agent_demo.is_some() {
            return;
        }
        let writes = self.saves.changed(writes);
        let results = SaveState::write(writes, &self.saves.writer);
        self.complete_save(results);
    }

    fn exit_save(&mut self) {
        self.finish_save();
        let mut writes = self.theme_writes();
        writes.push((self.store.path(&self.layout_name), self.current_doc().to_json()));
        writes.push((self.settings_file.clone(), self.settings_json()));
        self.save_sync(writes);
    }
}

/// Apply the native macOS window chrome (unified toolbar, inset traffic lights, sidebar
/// vibrancy). Returns whether the window is now translucent.
#[cfg(target_os = "macos")]
fn style_native_window(frame: &eframe::Frame, vibrancy: bool) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let vibrancy = vibrancy && std::env::var("TR_VIBRANCY").map_or(true, |v| v != "0");
    let Ok(handle) = frame.window_handle() else { return false };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return false };
    // SAFETY: called on the main thread (eframe runs `ui` there) with a live NSView.
    let ok = unsafe { tr_macos::style_window(h.ns_view.as_ptr(), vibrancy) };
    tracing::info!("macOS window chrome applied: {ok} (vibrancy {vibrancy})");
    ok && vibrancy
}

#[cfg(not(target_os = "macos"))]
fn style_native_window(_frame: &eframe::Frame, _vibrancy: bool) -> bool {
    false
}

/// The main window's live title bar geometry (macOS).
#[cfg(target_os = "macos")]
fn native_chrome(frame: &eframe::Frame) -> Option<tr_macos::Chrome> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = frame.window_handle().ok()?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
    // SAFETY: main thread, live NSView owned by the window.
    unsafe { tr_macos::chrome_metrics(h.ns_view.as_ptr()) }
}

#[cfg(not(target_os = "macos"))]
fn native_chrome(_frame: &eframe::Frame) -> Option<tr_macos::Chrome> {
    None
}

// N2: called on native visibility events too, not by an idle repaint loop.
#[cfg(target_os = "macos")]
fn edgar_window_visible(frame: &eframe::Frame) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = frame.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return false;
    };
    // SAFETY: eframe logic is on the main thread with a live NSView.
    unsafe { tr_macos::window_visible(h.ns_view.as_ptr()).unwrap_or(false) }
}
// N2
#[cfg(not(target_os = "macos"))]
fn edgar_window_visible(_frame: &eframe::Frame) -> bool {
    true
}

/// eframe is built without its `links` feature, so `Context::open_url` is otherwise a no-op.
/// Hand every requested http(s) URL to the system opener, like the layout folder action.
fn open_requested_urls(ctx: &egui::Context) {
    for url in take_open_urls(ctx) {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let _ = std::process::Command::new(opener).arg(&url).spawn();
    }
}

/// Remove this frame's open-URL requests, keeping only http(s) links.
fn take_open_urls(ctx: &egui::Context) -> Vec<String> {
    let urls = ctx.output_mut(|o| {
        let mut urls = Vec::new();
        o.commands.retain(|c| match c {
            egui::OutputCommand::OpenUrl(open) => {
                urls.push(open.url.clone());
                false
            }
            _ => true,
        });
        urls
    });
    urls.into_iter().filter(|u| external_url(u)).collect()
}

fn external_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://")) && !url.chars().any(char::is_control)
}

impl eframe::App for TerminalApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.agent_logic(ctx);
        self.process_logic();
        // N3: publish unpaused visible interest only on changes, never per-frame HTTP.
        let active = self.agent_demo.is_none()
            && edgar_window_visible(frame)
            && ctx.input(|i| i.viewport().minimized != Some(true))
            && self.visible_panes().iter().any(|&i| matches!(&self.panes[i].kind, PaneKind::Filings(f) if !f.paused));
        // N3
        let mut symbols: Vec<_> = self.watchlists.iter().flat_map(|l| l.symbols.iter().cloned()).collect();
        symbols.sort();
        symbols.dedup();
        let interest = (active, symbols);
        if self.edgar.interest.as_ref() != Some(&interest) {
            self.worker.send(Request::EdgarInterest { active, symbols: interest.1.clone() });
            self.edgar.interest = Some(interest);
        }

        self.populate_agent_demo(ctx);
        // An edit arriving during an in-flight save still needs a fresh snapshot
        // while hidden. Poll only until that snapshot can start; never tick idle.
        if self.agent_save_pending {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        if self.vibrancy { [0.0; 4] } else { visuals.window_fill.to_normalized_gamma_f32() }
    }

    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let frame_start = Instant::now();
        if let Some(prev) = self.last_frame.replace(frame_start) {
            let dt = prev.elapsed().as_secs_f64() * 1e3;
            self.perf_dt_ms = if self.perf_dt_ms == 0.0 { dt } else { self.perf_dt_ms * 0.9 + dt * 0.1 };
        }
        let ctx = ui.ctx().clone();
        if !self.window_styled {
            self.window_styled = true;
            self.vibrancy = style_native_window(frame, self.prefs.appearance.vibrancy);
        } else if !self.updater_started && self.agent_demo.is_none() {
            // From the second frame, so loading Sparkle doesn't delay the first.
            self.updater_started = true;
            tr_macos::updater::start();
        }
        if self.last_chrome_poll.is_none_or(|t| t.elapsed() > Duration::from_millis(750)) {
            let c = native_chrome(frame);
            if c != self.chrome {
                tracing::info!("window chrome: {c:?}");
                self.chrome = c;
            }
            self.last_chrome_poll = Some(Instant::now());
        }
        if self.last_accent_poll.is_none_or(|t| t.elapsed() > Duration::from_secs(3)) {
            if let Some([r, g, b]) = tr_macos::accent_color() {
                self.system_accent = Color32::from_rgb(r, g, b);
            }
            anim::set_reduced(tr_macos::reduce_motion());
            self.last_accent_poll = Some(Instant::now());
        }
        self.system_theme = ctx.system_theme();
        let ti = self.active_theme_index(&ctx);
        if self.applied_theme.as_ref() != Some(&self.themes[ti]) {
            theme::configure(&ctx, &self.themes[ti], self.system_accent);
            self.applied_theme = Some(self.themes[ti].clone());
        }
        theme::refresh(&ctx, &self.themes[ti], self.system_accent);
        // N1a
        // Focus is not visibility: visible background windows must keep polling.
        let news_hidden = ctx.input(|i| !i.raw.viewports.values().any(news_view::window_visible));
        self.news_periodic(&self.visible_panes(), news_hidden);
        // N1a
        self.periodic();
        self.sync_chart_navigation();

        let screen = ui.max_rect();
        let open = anim::spring_with(
            &ctx,
            egui::Id::new("sidebar-open"),
            if self.sidebar_open { 1.0 } else { 0.0 },
            0.32,
            0.9,
        );
        let sb_w = (SIDEBAR_W * open).round();
        let right_side = self.prefs.appearance.sidebar_side == prefs::SidebarSide::Right;
        let main = if right_side {
            Rect::from_min_max(screen.min, pos2(screen.right() - sb_w, screen.bottom()))
        } else {
            Rect::from_min_max(pos2(screen.left() + sb_w, screen.top()), screen.max)
        };
        ui.painter().rect_filled(main, 0.0, theme::window());
        let toolbar = Rect::from_min_size(main.min, vec2(main.width(), self.toolbar_h()));
        let status = Rect::from_min_max(pos2(main.left(), main.bottom() - STATUS_H), main.max);
        let gap = theme::dp(6.0);
        let area = Rect::from_min_max(
            pos2(main.left() + gap, toolbar.bottom() + gap),
            pos2(main.right() - gap, status.top() - gap * 0.5),
        );

        if self.symbol_search.is_none() {
            self.hotkeys(&ctx);
        }
        self.command_palette(&ctx);
        let mut sidebar_cmds = Vec::new();
        if sb_w >= 1.0 {
            // The sidebar slides in from its edge.
            let x = if right_side { screen.right() - sb_w } else { screen.left() + sb_w - SIDEBAR_W };
            let full = Rect::from_min_size(pos2(x, screen.top()), vec2(SIDEBAR_W, screen.height()));
            let mut child = ui.new_child(UiBuilder::new().max_rect(full).id_salt("sidebar-root"));
            let visible = if right_side {
                Rect::from_min_max(pos2(screen.right() - sb_w, screen.top()), screen.max)
            } else {
                Rect::from_min_max(screen.min, pos2(screen.left() + sb_w, screen.bottom()))
            };
            child.set_clip_rect(visible);
            sidebar_cmds = self.sidebar(&mut child, full);
        }
        // The traffic lights sit at the window's left edge: clear them unless a left
        // sidebar already covers that corner.
        let inset = if right_side { self.traffic_inset() } else { (self.traffic_inset() - sb_w).max(0.0) };
        self.toolbar(ui, toolbar, inset);
        self.status_bar(ui, status);

        let mut actions = Vec::new();
        for c in sidebar_cmds {
            match c {
                SidebarCmd::Action(a) => actions.push(a),
                SidebarCmd::Function(f) => self.apply(f),
                SidebarCmd::Layout(op) => self.layout_op(op),
            }
        }
        prefs::set_drawing_favorites(&ctx, (self.prefs.drawing_favorites.clone(), self.prefs.show_drawing_favorites));
        prefs::set_drawing_toolbar(&ctx, (self.prefs.show_drawing_rail, self.prefs.drawing_favorites_pos));
        prefs::set_study_favorites(&ctx, self.prefs.study_favorites.clone());
        actions.extend(self.render_dock(ui, area, gap));
        actions.extend(self.render_windows(&ctx));
        let (favorites, shown) = prefs::drawing_favorites(&ctx);
        if favorites != self.prefs.drawing_favorites || shown != self.prefs.show_drawing_favorites {
            self.prefs.drawing_favorites = favorites;
            self.prefs.show_drawing_favorites = shown;
            self.save_prefs();
        }
        let (rail, position) = prefs::drawing_toolbar(&ctx);
        if rail != self.prefs.show_drawing_rail || position != self.prefs.drawing_favorites_pos {
            self.prefs.show_drawing_rail = rail;
            self.prefs.drawing_favorites_pos = position;
            self.save_prefs();
        }
        let study_favorites = prefs::study_favorites(&ctx);
        if study_favorites != self.prefs.study_favorites {
            self.prefs.study_favorites = study_favorites;
            self.save_prefs();
        }
        self.run_actions(actions);
        self.symbol_search_ui(&ctx);
        if let Some(editor) = &mut self.columns_editor {
            let (applied, open) = editor.show(&ctx);
            if let Some(set) = applied {
                self.watch_columns = set;
                self.refresh_columns();
            }
            if !open {
                self.columns_editor = None;
            }
        }
        self.trading_windows(&ctx);
        self.price_alerts.editor(&ctx);
        if let Some(notice) = self.price_alerts.notice.take() {
            self.toasts.push((format!("{}{notice}", crate::price_alerts::TOAST_PREFIX), Instant::now()));
        }
        self.toasts(&ctx);

        // TR_SMOKE_SECS=N: render for N seconds, log frame stats and quit (CI smoke test).
        if self.agent_demo.is_none()
            && let Some(secs) = std::env::var("TR_SMOKE_SECS").ok().and_then(|v| v.parse::<f64>().ok())
        {
            ctx.request_repaint();
            if !self.smoke_done && self.started.elapsed().as_secs_f64() > secs {
                self.smoke_done = true;
                tracing::info!(
                    "smoke: {} frames in {:.1}s, ui cpu {:.2} ms/frame, panes {}, layout {:?}",
                    ctx.cumulative_frame_nr(),
                    self.started.elapsed().as_secs_f64(),
                    self.perf_cpu_ms,
                    self.panes.len(),
                    self.layout_name
                );
                self.autosave();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        self.observe_edits(true);
        open_requested_urls(&ctx);
        self.capture_agent_demo(&ctx, screen);
        let cpu = frame_start.elapsed().as_secs_f64() * 1e3;
        self.perf_cpu_ms = self.perf_cpu_ms * 0.9 + cpu * 0.1;
        if self.show_perf {
            // Measure continuously while the readout is visible.
            ctx.request_repaint();
        }
        // Keep the clock ticking and pick up worker results without spinning.
        ctx.request_repaint_after(Duration::from_millis(500));
        if self.started.elapsed() < Duration::from_secs(2) {
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self) {
        if self.agent_demo.is_some() {
            return;
        }
        // Drop signals shutdown and waits at most 1.5 s; any remaining cleanup
        // stays with the detached owner instead of blocking the UI's exit/save.
        self.agent_pane.workflows.reset();
        self.agent_pane.session = None;
        self.exit_save();
        self.price_alerts.save();
        if self.demo.save_error.is_some() {
            self.demo.save();
        }
    }
}

impl TerminalApp {
    /// Inert workspace with all disk-backed dependencies confined to the fixture.
    #[cfg(test)]
    pub(crate) fn test_workspace(root: &std::path::Path) -> Self {
        Self::isolated_workspace(root, Worker::inert())
    }

    /// Shared by the offline review harness and session-free workspace tests.
    fn isolated_workspace(root: &std::path::Path, worker: Worker) -> Self {
        Self {
            // N2
            edgar: Default::default(),
            drawing_ids: Default::default(),
            agent_queue: crate::agent_queue::AgentToolQueue::new(egui::Context::default()),
            pinned_agent_chart: None,
            agent_pane: crate::agent_pane::AgentPane::default(),
            agent_demo: None,
            agent_turns: Vec::new(),
            agent_save_pending: false,
            edit_log: Default::default(),
            agent_job_chart: None,
            broker: broker::BrokerState::with_store(
                tr_broker::secrets::SecretStore::file(root.join("secrets.json")),
                true,
            ),
            trading: Default::default(),
            demo: demo::DemoState::load(root.join("demo.json")).0,
            worker,
            provider: "sample".into(),
            panes: Vec::new(),
            dock: Node::default(),
            focus: 0,
            next_id: 1,
            last_opened: None,
            preset: None,
            drag: None,
            dock_motion: Default::default(),
            renaming_tab: None,
            store: Store::new(root.join("layouts")),
            layout_name: "Default".into(),
            layout_names: Vec::new(),
            saves: SaveState::default(),
            settings_file: root.join("settings.json"),
            renaming_layout: None,
            editing_theme: String::new(),
            themes: Vec::new(),
            theme_store: crate::themes::ThemeStore::new(root.join("themes")),
            applied_theme: None,
            dirty_themes: Vec::new(),
            themes_changed_at: None,
            renaming_theme: None,
            theme_io: String::new(),
            system_theme: None,
            layout_theme: false,
            prefs: Prefs::default(),
            rebinding: None,
            hotkey_filter: String::new(),
            settings_tab: SettingsTab::Appearance,
            data_sources: data_sources::State::default(),
            io_path: String::new(),
            curves: None,
            last_wei: None,
            last_curve: None,
            wei_updated: None,
            symbol_search: None,
            net_hits: (String::new(), Vec::new()),
            search_pending: None,
            suggestion: None,
            cmd: String::new(),
            cmd_history: Vec::new(),
            cmd_hist_pos: None,
            focus_cmd: true,
            status: String::new(),
            toasts: Vec::new(),
            alert_toasts: Vec::new(),
            // N1a
            news_subscription: None,
            // N1a
            watchlist_undo: None,
            columns_in_flight: None,
            latest: Vec::new(),
            politicians: Vec::new(),
            pol_search: String::new(),
            alerts: Vec::new(),
            price_alerts: crate::price_alerts::State::load(root.join("price-alerts.json"), true),
            congress_status: String::new(),
            congress_checked: None,
            watchlist: Vec::new(),
            watchlists: vec![NamedWatchlist::new("Main".into(), Vec::new())],
            active_watchlist: 0,
            watchlist_entry: None,
            watch_item_entry: None,
            watch_selected: None,
            watch_details: true,
            watchlist_delete: false,
            security_names: Default::default(),
            watch_columns: Default::default(),
            columns_editor: None,
            column_cells: HashMap::new(),
            columns_revision: 0,
            columns_computed_at: 0,
            columns_requested: None,
            columns_snapshot: Default::default(),
            watch_sort: None,
            watch_drag: None,
            quotes: HashMap::new(),
            last_quotes: None,
            last_feed: None,
            last_save: Instant::now(),
            started: Instant::now(),
            smoke_done: false,
            show_perf: false,
            perf_cpu_ms: 0.,
            perf_dt_ms: 0.,
            last_frame: None,
            sidebar_open: true,
            system_accent: theme::DEFAULT_ACCENT,
            last_accent_poll: None,
            window_styled: false,
            updater_started: false,
            vibrancy: false,
            chrome: None,
            last_chrome_poll: None,
            palette: crate::palette::Palette::load(&root.join("palette.json")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::Timeframe;

    #[test]
    fn link_clicks_reach_the_system_opener_http_only() {
        // eframe's own opener is compiled out; the app must drain OpenUrl itself.
        let ctx = egui::Context::default();
        let form4 = "https://www.sec.gov/Archives/edgar/data/320193/x/xslF345X06/form4.xml";
        ctx.open_url(egui::OpenUrl::new_tab(form4));
        ctx.open_url(egui::OpenUrl::new_tab("file:///etc/passwd"));
        ctx.open_url(egui::OpenUrl::new_tab("javascript:alert(1)"));
        ctx.copy_text("kept".into());
        assert_eq!(take_open_urls(&ctx), vec![form4.to_string()]);
        assert!(take_open_urls(&ctx).is_empty());
        assert!(ctx.output(|o| o.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(_)))));
    }

    #[test]
    fn bare_ticker_from_a_feed_pane_goes_to_the_chart() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/feed-ticker-routing");
        let mut app = TerminalApp::test_workspace(&root);
        let chart_symbol = |app: &TerminalApp| {
            app.panes.iter().find_map(|p| match &p.kind {
                PaneKind::Chart(c) => Some(c.symbol.clone()),
                _ => None,
            })
        };
        for (open, feed_symbol) in [("AAPL N", "news"), ("AAPL CF", "filings")] {
            app.execute(open);
            let feed = app.focus;
            let symbol_of =
                |app: &TerminalApp| app.idx(feed).and_then(|i| app.panes[i].kind.symbol().map(str::to_owned));
            let before = symbol_of(&app);
            app.execute("JPM");
            assert_eq!(chart_symbol(&app).as_deref(), Some("JPM US Equity"), "{feed_symbol}: chart takes the ticker");
            assert_eq!(symbol_of(&app), before, "{feed_symbol}: unlinked feed pane keeps its symbol");
            assert!(matches!(app.panes[app.idx(app.focus).unwrap()].kind, PaneKind::Chart(_)));
        }
    }

    #[test]
    fn ticker_news_and_filings_retarget_the_existing_pane_and_keep_filters() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/feed-retarget");
        let mut app = TerminalApp::test_workspace(&root);
        app.execute("AAPL N");
        let news = app.focus;
        let index = app.idx(news).unwrap();
        if let PaneKind::News(n) = &mut app.panes[index].kind {
            n.filters.search = "earnings".into();
            n.filters.scope = tr_data::news_store::Scope::All;
        }
        let count = app.panes.len();
        app.execute("MSFT N");
        assert_eq!(app.panes.len(), count, "no duplicate News pane");
        assert_eq!(app.focus, news);
        let PaneKind::News(n) = &app.panes[app.idx(news).unwrap()].kind else { panic!("news pane replaced") };
        assert_eq!(n.symbol.as_deref(), Some("MSFT US Equity"));
        assert_eq!(n.filters.search, "earnings");
        assert_eq!(n.filters.scope, tr_data::news_store::Scope::Symbol);
        app.execute("AAPL CF");
        let filings = app.focus;
        let count = app.panes.len();
        app.execute("MSFT CF");
        assert_eq!((app.panes.len(), app.focus), (count, filings));
        assert_eq!(app.panes[app.idx(filings).unwrap()].kind.symbol(), Some("MSFT US Equity"));
    }

    type SaveLog = std::sync::Arc<std::sync::Mutex<Vec<(SaveWrite, std::thread::ThreadId)>>>;

    fn autosave_fixture(tag: &str) -> (TerminalApp, SaveLog) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(tag);
        let mut app = TerminalApp::test_workspace(&root);
        let log = SaveLog::default();
        let writes = log.clone();
        app.saves.writer = std::sync::Arc::new(move |path, bytes| {
            writes.lock().unwrap().push(((path.to_owned(), bytes.to_vec()), std::thread::current().id()));
            Ok(())
        });
        app.autosave();
        app.finish_save();
        log.lock().unwrap().clear();
        (app, log)
    }

    #[test]
    fn startup_congress_pause_does_not_claim_a_check() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/congress-pause-display");
        let mut app = TerminalApp::test_workspace(&root);
        let pause = "Senate source paused until 2026-10-24: site refused requests";
        app.show_congress_pause(pause.into());
        assert!(app.congress_checked.is_none());
        assert_eq!(app.status, pause);
        let freshness = app.congress_freshness();
        assert!(freshness.contains(pause));
        assert!(!freshness.contains("checked") && !freshness.contains("checking"));
        let previous = chrono::Local::now() - chrono::Duration::hours(1);
        app.congress_checked = Some(previous);
        app.show_congress_pause(pause.into());
        assert_eq!(app.congress_checked, Some(previous));
    }

    #[test]
    fn autosave_unchanged_state_performs_no_write() {
        let (mut app, log) = autosave_fixture("autosave-unchanged");
        app.autosave();
        assert!(app.saves.job.is_none());
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn data_source_settings_persist_in_app_settings() {
        let (mut app, log) = autosave_fixture("data-source-settings");
        let sources = tr_net::Settings {
            pause_all: true,
            finnhub: false,
            sec: false,
            yahoo_news: false,
            coindesk: false,
            sec_contact: "research@example.test".into(),
            ..Default::default()
        };
        app.data_sources.settings = sources.clone();
        app.save_settings();
        let writes = log.lock().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0.0, app.settings_file);
        let restored: Settings = serde_json::from_slice(&writes[0].0.1).unwrap();
        assert!(restored.data_sources == sources);
        let legacy: Settings = serde_json::from_str(r#"{"watchlist":[]}"#).unwrap();
        assert!(legacy.data_sources == tr_net::Settings::default());
    }

    #[test]
    fn autosave_settings_change_writes_only_settings_once() {
        let (mut app, log) = autosave_fixture("autosave-settings");
        app.watch_details = !app.watch_details;
        app.autosave();
        app.finish_save();
        app.autosave();
        assert!(app.saves.job.is_none());
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0.0, app.settings_file);
        assert_eq!(log[0].0.1, app.settings_json());
    }

    #[test]
    fn autosave_unmarked_layout_change_writes_only_layout_once() {
        let (mut app, log) = autosave_fixture("autosave-layout");
        app.layout_theme = true;
        app.autosave();
        app.finish_save();
        app.autosave();
        assert!(app.saves.job.is_none());
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0.0, app.store.path(&app.layout_name));
        assert_eq!(log[0].0.1, app.current_doc().to_json());
    }

    #[test]
    fn autosave_writer_runs_off_the_ui_thread() {
        let (mut app, log) = autosave_fixture("autosave-thread");
        app.watch_details = !app.watch_details;
        app.autosave();
        app.finish_save();
        assert_eq!(log.lock().unwrap().len(), 1);
        assert_ne!(log.lock().unwrap()[0].1, std::thread::current().id());
    }

    #[test]
    fn autosave_failed_file_retries_without_rewriting_successful_file() {
        let (mut app, log) = autosave_fixture("autosave-retry");
        let previous = app.saves.written[&app.settings_file].clone();
        let settings = app.settings_file.clone();
        let writes = log.clone();
        let mut_fail = std::sync::atomic::AtomicBool::new(true);
        app.saves.writer = std::sync::Arc::new(move |path, bytes| {
            writes.lock().unwrap().push(((path.to_owned(), bytes.to_vec()), std::thread::current().id()));
            if path == settings && mut_fail.swap(false, std::sync::atomic::Ordering::SeqCst) {
                Err(std::io::Error::other("injected failure"))
            } else {
                Ok(())
            }
        });
        app.watch_details = !app.watch_details;
        app.layout_theme = true;
        app.autosave();
        app.finish_save();
        assert_eq!(app.saves.written[&app.settings_file], previous);
        assert!(app.status.contains("injected failure"));
        app.autosave();
        app.finish_save();
        app.autosave();
        assert!(app.saves.job.is_none());
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(log.iter().filter(|w| w.0.0 == app.settings_file).count(), 2);
        assert_eq!(app.saves.written[&app.settings_file], app.settings_json());
    }

    #[test]
    fn autosave_exit_waits_for_in_flight_job_and_saves_latest_synchronously() {
        let (mut app, log) = autosave_fixture("autosave-exit");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = std::sync::Mutex::new(release_rx);
        let first = std::sync::atomic::AtomicBool::new(true);
        let writes = log.clone();
        app.saves.writer = std::sync::Arc::new(move |path, bytes| {
            if first.swap(false, std::sync::atomic::Ordering::SeqCst) {
                started_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            }
            writes.lock().unwrap().push(((path.to_owned(), bytes.to_vec()), std::thread::current().id()));
            Ok(())
        });
        app.watch_details = !app.watch_details;
        app.autosave();
        started_rx.recv().unwrap();
        app.watch_details = !app.watch_details;
        app.layout_theme = true;
        app.autosave();
        assert!(log.lock().unwrap().is_empty());
        let release = std::thread::spawn(move || release_tx.send(()).unwrap());
        app.exit_save();
        release.join().unwrap();
        assert!(app.saves.job.is_none());
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 3);
        assert_ne!(log[0].1, std::thread::current().id());
        assert!(log[1..].iter().all(|w| w.1 == std::thread::current().id()));
        assert_eq!(app.saves.written[&app.settings_file], app.settings_json());
        assert_eq!(app.saves.written[&app.store.path(&app.layout_name)], app.current_doc().to_json());
    }

    #[test]
    fn named_watchlists_legacy_migration_and_settings_round_trip() {
        let old: Settings =
            serde_json::from_str(r#"{"watchlist":["AAPL","NVDA","aapl"],"active_watchlist":999}"#).unwrap();
        let (lists, active) = old.sanitized_watchlists();
        assert_eq!(active, 0);
        assert_eq!(lists[0].name, "Main");
        assert_eq!(lists[0].symbols, vec![tr_data::security("AAPL").key(), tr_data::security("NVDA").key()]);
        let mut settings = old;
        settings.watchlists = lists.clone();
        settings.watchlists.push(NamedWatchlist::new("Second".into(), Vec::new()));
        settings.active_watchlist = 1;
        let restored: Settings = serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(restored.sanitized_watchlists(), (settings.watchlists, 1));
    }

    #[test]
    fn named_watchlists_layout_legacy_sanitize_and_round_trip() {
        let mut doc = LayoutDoc::default_workspace("Watchlists", &prefs::PanelDefaults::default());
        let mut value = serde_json::to_value(&doc).unwrap();
        value["watchlist"] = serde_json::json!(["NVDA", "AAPL", "NVDA"]);
        let legacy = LayoutDoc::from_json(&serde_json::to_vec(&value).unwrap()).unwrap();
        let lists = legacy.watchlists.unwrap();
        assert_eq!(lists[0].name, "Main");
        assert_eq!(lists[0].symbols.len(), 2);
        doc.watchlists = Some(vec![
            NamedWatchlist::new("Main".into(), vec!["AAPL".into()]),
            NamedWatchlist::new("MAIN".into(), vec!["NVDA".into()]),
        ]);
        doc.active_watchlist = 999;
        let loaded = LayoutDoc::from_json(&doc.to_json()).unwrap();
        assert_eq!(loaded.active_watchlist, 1);
        assert_eq!(loaded.watchlists.as_ref().unwrap()[1].name, "MAIN (2)");
        assert_eq!(LayoutDoc::from_json(&loaded.to_json()).unwrap(), loaded);
        let root = std::env::temp_dir().join(format!("named-watchlists-layout-{}", std::process::id()));
        let mut app = TerminalApp::test_workspace(&root);
        app.apply_doc(loaded);
        assert_eq!(app.active_watchlist, 1);
        assert_eq!(app.watchlist, vec![tr_data::security("NVDA").key()]);
        assert_eq!(app.current_doc().watchlists, Some(app.watchlists.clone()));
    }

    #[test]
    fn adding_to_a_named_watchlist_leaves_the_active_list_alone() {
        let root = std::env::temp_dir().join(format!("named-watchlist-add-{}", std::process::id()));
        let mut app = TerminalApp::test_workspace(&root);
        app.create_watchlist("Crypto".into(), Vec::new()).unwrap();
        app.create_watchlist("Tech".into(), vec!["NVDA".into()]).unwrap();
        let active = app.active_watchlist;
        let before = app.watchlist.clone();
        let crypto = app.watchlists.iter().position(|l| l.name == "Crypto").unwrap();
        app.run_actions(vec![Action::AddToNamedWatchlist { list: "Crypto".into(), symbol: "SOLUSD Curncy".into() }]);
        assert_eq!(app.watchlists[crypto].symbols, vec![tr_data::security("SOLUSD Curncy").key()]);
        assert_eq!((app.active_watchlist, &app.watchlist), (active, &before));
        assert!(app.status.contains("Added SOL/USD to Crypto"), "{}", app.status);
        app.run_actions(vec![Action::AddToNamedWatchlist { list: "Crypto".into(), symbol: "SOLUSD Curncy".into() }]);
        assert_eq!(app.watchlists[crypto].symbols.len(), 1);
        assert!(app.status.contains("already in Crypto"), "{}", app.status);
        app.run_actions(vec![Action::AddToNamedWatchlist { list: "Gone".into(), symbol: "AAPL".into() }]);
        assert!(app.status.contains("no longer exists"));
        let tech = app.watchlists[active].name.clone();
        app.run_actions(vec![Action::AddToNamedWatchlist { list: tech, symbol: "AAPL".into() }]);
        assert!(app.watchlist.contains(&tr_data::security("AAPL").key()), "the active list uses the usual path");
        assert_eq!(app.watchlists[active].symbols, app.watchlist);
    }

    #[test]
    fn named_watchlists_sanitize_names_symbols_and_limits() {
        let list = |name: &str| {
            NamedWatchlist::new(
                name.into(),
                vec!["aapl".into(), "NVDA".into(), "AAPL".into(), "".into(), "bad!".into(), "AAPL garbage".into()],
            )
        };
        let mut input =
            vec![list(""), list("\nMain"), list(&"x".repeat(41)), list(" Main "), list("main"), list("MAIN")];
        input.push(NamedWatchlist::new("Full".into(), (0..205).map(|n| format!("QA{n}")).collect()));
        let out = sanitize_watchlists(input);
        assert_eq!(out.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["Main", "main (2)", "MAIN (3)", "Full"]);
        assert_eq!(out[0].symbols.len(), 2);
        assert_eq!(out[3].symbols.len(), 200);
        let out = sanitize_watchlists((0..60).map(|_| list(&"é".repeat(40))).collect());
        assert_eq!(out.len(), 50);
        assert!(out.iter().all(|l| valid_watchlist_name(&l.name)));
        assert_eq!(sanitize_watchlists(Vec::new())[0].name, "Main");
    }

    #[test]
    fn named_watchlists_commands_switch_refresh_duplicate_delete_and_undo() {
        let root = std::env::temp_dir().join(format!("named-watchlists-{}", std::process::id()));
        let mut app = TerminalApp::test_workspace(&root);
        let run = |app: &mut TerminalApp, text: &str| {
            let Some(command::Command::Function(f)) = command::parse(text) else {
                panic!("Command did not parse: {text}");
            };
            app.apply(f);
        };
        run(&mut app, "WL ADD AAPL");
        let main = app.watchlist.clone();
        run(&mut app, "WL NEW Growth stocks");
        assert_eq!(app.watchlists[1].name, "Growth stocks");
        assert!(app.watchlist.is_empty());
        run(&mut app, "WL ADD NVDA");
        let revision = app.columns_revision;
        app.panes.push(Pane { id: 1, kind: PaneKind::Scanner(Box::default()), link: None, title: None, window: None });
        run(&mut app, "WL USE main");
        assert_eq!(app.watchlist, main);
        assert!(app.columns_revision > revision);
        assert_eq!(app.columns_snapshot.0, main);
        let PaneKind::Scanner(scanner) = &app.panes[0].kind else { panic!() };
        assert_eq!(scanner.universe, tr_data::universe::Universe::Watchlist);
        assert_eq!(scanner.generation, 1);
        run(&mut app, "WL RENAME Growth stocks TO Technology");
        assert_eq!(app.watchlists[1].name, "Technology");
        assert!(app.create_watchlist("technology".into(), Vec::new()).is_err());
        assert!(app.rename_watchlist(1, " ".into()).is_err());
        let duplicate = unique_watchlist_name("Main", &app.watchlists);
        app.create_watchlist(duplicate, main.clone()).unwrap();
        assert_eq!(app.watchlists[2].name, "Main (2)");
        app.run_actions(vec![Action::ReplaceWatchlistWithMatches(vec!["TSLA".into()])]);
        app.switch_watchlist(0);
        app.undo_watchlist();
        assert_eq!(app.watchlist, main);
        run(&mut app, "WL DEL Technology");
        run(&mut app, "WL DEL Main (2)");
        run(&mut app, "WL DEL Main");
        assert_eq!(app.watchlists.len(), 1);
        assert!(app.status.contains("last watchlist"));
        run(&mut app, "WL LISTS");
        assert_eq!(app.status, "Main");
    }

    #[test]
    fn named_watchlists_save_matches_preserves_order_and_caps() {
        let root = std::env::temp_dir().join(format!("named-watchlists-save-{}", std::process::id()));
        let mut app = TerminalApp::test_workspace(&root);
        let mut matches = vec!["NVDA".into(), "AAPL".into(), "NVDA".into()];
        matches.extend((0..205).map(|n| format!("QA{n}")));
        app.run_actions(vec![Action::SaveMatchesAsWatchlist("Matches".into(), matches)]);
        assert_eq!(app.active_watchlist, 1);
        assert_eq!(app.watchlist.len(), 200);
        assert_eq!(&app.watchlist[..2], &[tr_data::security("NVDA").key(), tr_data::security("AAPL").key()]);
        assert!(app.watchlists[0].symbols.is_empty());
        for i in 2..50 {
            app.create_watchlist(format!("List {i}"), Vec::new()).unwrap();
        }
        assert!(app.create_watchlist("Overflow".into(), Vec::new()).is_err());
    }

    #[test]
    fn watchlist_sections_survive_edits_sanitizing_and_old_files() {
        use crate::watch_sections::{Item, items};
        let root = std::env::temp_dir().join(format!("watch-sections-{}", std::process::id()));
        let mut app = TerminalApp::test_workspace(&root);
        let key = |s: &str| tr_data::security(s).key();
        let rows = vec![
            Item::Section { name: "Macro".into(), collapsed: true },
            Item::Symbol(key("SPX Index")),
            Item::Symbol(key("AAPL")),
            Item::Section { name: "Empty".into(), collapsed: false },
            Item::Section { name: "Crypto".into(), collapsed: false },
            Item::Symbol(key("BTCUSD Curncy")),
        ];
        app.set_watchlist_items(&rows);
        assert_eq!(app.watchlist, [key("SPX Index"), key("AAPL"), key("BTCUSD Curncy")]);
        app.set_watchlist(vec![key("BTCUSD Curncy"), key("SPX Index"), key("NVDA")]);
        let list = &app.watchlists[app.active_watchlist];
        assert_eq!(
            list.items(),
            [&rows[..2], &rows[3..], &[Item::Symbol(key("NVDA"))]].concat(),
            "removed symbols leave their section, new ones join the end"
        );
        let mut dirty = list.clone();
        dirty.symbols.insert(1, "not a ticker!".into());
        dirty.sections[2].at += 1;
        let clean = sanitize_watchlists(vec![dirty]);
        assert_eq!(clean[0].items(), list.items(), "cleaning a symbol keeps headers in place");
        let json = serde_json::to_string(&clean[0]).unwrap();
        assert_eq!(&serde_json::from_str::<NamedWatchlist>(&json).unwrap(), &clean[0]);
        let old: NamedWatchlist = serde_json::from_str(r#"{"name":"Main","symbols":["AAPL US Equity"]}"#).unwrap();
        assert!(old.sections.is_empty());
        assert!(!serde_json::to_string(&old).unwrap().contains("sections"), "unsectioned lists save as before");
        assert_eq!(items(&old.symbols, &old.sections), [Item::Symbol(key("AAPL"))]);
    }

    #[test]
    fn watchlist_add_replace_dedupe_order_cap_and_undo() {
        let key = |s: &str| tr_data::security(s).key();
        let original = vec![key("MSFT"), key("AAPL")];
        let matches = vec!["NVDA".into(), "AAPL".into(), "NVDA".into(), "TSLA".into()];
        let (added, count, truncated) = watchlist_append(&original, matches.clone());
        assert_eq!(added, vec![key("MSFT"), key("AAPL"), key("NVDA"), key("TSLA")]);
        assert_eq!((count, truncated), (2, false));
        let (replacement, _, _) = watchlist_append(&[], matches);
        assert_eq!(replacement, vec![key("NVDA"), key("AAPL"), key("TSLA")]);
        let mut undo = Some((Instant::now(), original.clone(), replacement.clone()));
        assert_eq!(watchlist_restore(&replacement, &mut undo), Some(original.clone()));
        assert!(undo.is_none());
        let mut undo = Some((Instant::now(), original.clone(), replacement));
        assert_eq!(watchlist_restore(&added, &mut undo), None);
        let (full, count, truncated) = watchlist_append(&[], (0..205).map(|n| format!("QA{n}")).collect());
        assert_eq!((full.len(), count, truncated), (200, 200, true));
        assert_eq!(
            watchlist_add_message(count, truncated),
            "Added 200 symbols to the watchlist (truncated to the 200-symbol watchlist cap)"
        );
        assert_eq!(watchlist_append(&original, original.clone()).1, 0);
        assert_eq!(watchlist_add_message(0, false), "All matches are already in the watchlist");
        // The row action uses the same single-symbol append path as WL ADD.
        let (row, count, _) = watchlist_append(&original, vec!["NVDA".into()]);
        assert_eq!(count, 1);
        assert_eq!(row.last(), Some(&key("NVDA")));
    }

    #[test]
    fn slow_column_reply_is_accepted_before_overdue_refresh() {
        let requested = Instant::now();
        let now = requested + Duration::from_secs(61);
        let mut in_flight = Some(4);
        assert!(!columns_refresh_due(in_flight, Some(requested), now));
        let cells = finish_columns_reply(&mut in_flight, 4, 4, "slow cells");
        assert_eq!(cells, Some("slow cells"));
        assert_eq!(in_flight, None);
        assert!(columns_refresh_due(in_flight, Some(requested), now));
        in_flight = Some(5);
        assert!(!columns_refresh_due(in_flight, Some(now), now));
        assert_eq!(finish_columns_reply(&mut in_flight, 4, 5, "stale"), None);
        assert_eq!(in_flight, Some(5));
        assert_eq!(finish_columns_reply(&mut in_flight, 5, 6, "changed inputs"), None);
        assert_eq!(in_flight, None);
        assert!(columns_refresh_due(in_flight, None, now));
    }

    #[test]
    fn stale_column_revision_is_dropped() {
        assert_eq!(accept_columns(3, 4, "old cells"), None);
        assert_eq!(accept_columns(4, 4, "new cells"), Some("new cells"));
    }

    #[test]
    fn watch_sort_unavailable_last_both_directions() {
        use tr_core::columns::{Cell, sort_key};
        let original =
            vec![Cell::Value(2.), Cell::Unavailable("No history".into()), Cell::Check(false), Cell::Check(true)];
        for descending in [false, true] {
            let mut view = original.clone();
            view.sort_by(|a, b| watch_value_order(sort_key(Some(a)), sort_key(Some(b)), descending));
            assert!(matches!(view.last(), Some(Cell::Unavailable(_))));
            assert_eq!(sort_key(view.first()), Some(if descending { 2. } else { 0. }));
            assert_eq!(watch_value_order(None, Some(1.), descending), std::cmp::Ordering::Greater);
        }
        assert_eq!(original[0], Cell::Value(2.));
    }

    #[test]
    fn legacy_settings_migrate_to_a_layout() {
        let old = r#"{"layout":"MainRight2","watchlist":["AAPL","BTCUSD Curncy"],"sidebar":false,
            "panes":[{"Chart":{"symbol":"NVDA","tf":"D1","chart_type":"Candles","log":false,"indicators":[],
            "drawings":[],"compare":null}},"Feed","Wei"]}"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert!(!s.sidebar);
        let doc = migrate_legacy(&s, &prefs::PanelDefaults::default()).unwrap();
        assert_eq!(doc.panes.len(), 3);
        assert_eq!(doc.dock.visible(), vec![1, 2, 3]);
        assert!(matches!(&doc.panes[0].settings, PaneSettings::Chart { symbol, .. } if symbol == "NVDA"));
        assert_eq!(doc.panes[1].settings, PaneSettings::Feed(None));
        // New-format files carry no legacy fields and are not migrated.
        let new: Settings = serde_json::from_str(r#"{"watchlist":[],"active_layout":"Desk"}"#).unwrap();
        assert!(migrate_legacy(&new, &prefs::PanelDefaults::default()).is_none());
        assert_eq!(new.active_layout.as_deref(), Some("Desk"));
    }

    #[test]
    fn pane_settings_round_trip_through_kinds() {
        let d = prefs::PanelDefaults::default();
        for s in [
            crate::layouts::chart_settings("BTCUSD Curncy", &d),
            PaneSettings::Feed(Some(crate::layouts::FeedSettings { buys: Some(true), ..Default::default() })),
            PaneSettings::News(Some("AAPL US Equity".into())),
            PaneSettings::News(None),
            PaneSettings::OrderBook { symbol: "ETHUSD Curncy".into(), depth: 50, group: 1.0 },
            PaneSettings::Search("gold".into()),
            PaneSettings::Settings,
            PaneSettings::Wei,
        ] {
            assert_eq!(PaneKind::from_settings(s.clone()).settings(), s);
        }
    }

    #[test]
    fn chart_layout_persists_plot_conditions_and_defaults_legacy_field() {
        let mut chart = ChartState::new(1, "AAPL", tr_core::Timeframe::D1);
        chart.data.workflow.plot_conditions = true;
        let before = PaneKind::Chart(Box::new(chart)).settings();
        let mut saved = serde_json::to_value(&before).unwrap();
        let restored = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(PaneKind::from_settings(restored).settings(), before);
        saved["Chart"]["data"]["workflow"].as_object_mut().unwrap().remove("plot_conditions");
        let restored = serde_json::from_value(saved).unwrap();
        let PaneKind::Chart(chart) = PaneKind::from_settings(restored) else {
            panic!("Expected chart");
        };
        assert!(!chart.data.workflow.plot_conditions);
    }

    #[test]
    fn advanced_chart_settings_survive_layout_serialization() {
        use tr_core::Timeframe;
        let mut chart = ChartState::new(1, "AAPL", Timeframe::H1);
        chart.navigation = crate::chart_navigation::Navigation { cursor: true, range: true };
        chart.data = crate::chart_data::ChartData {
            account: Default::default(),
            zone: tr_core::session::Zone::NewYork,
            session: tr_core::session::Session::UsRegular,
            refresh_seconds: 60,
            comparisons: vec![],
            hidden_comparisons: vec![],
            ..Default::default()
        };
        chart.indicators = vec![crate::command::IndicatorSpec::new("PCF", &[]).unwrap()];
        tr_core::indicators::study::assign_ids(&mut chart.indicators);
        let settings = chart.indicators[0].chart.as_mut().unwrap();
        settings.expression = Some("CROSS(C, AVGC20)".into());
        settings.timeframe = Some(Timeframe::D1);
        settings.outputs.insert(
            "Formula".into(),
            tr_core::indicators::study::OutputStyle { color: Some([12, 34, 56]), width: 2.5, ..Default::default() },
        );
        let before = PaneKind::Chart(Box::new(chart)).settings();
        let restored = serde_json::from_str(&serde_json::to_string(&before).unwrap()).unwrap();
        assert_eq!(PaneKind::from_settings(restored).settings(), before);
    }

    #[test]
    fn watchlist_names_must_treat_case_fold_equivalent_names_as_duplicates() {
        let list = |name: &str| NamedWatchlist::new(name.into(), Vec::new());
        // Case folding maps ß to "ss", the long s to "s" and the final sigma to "σ".
        let out = sanitize_watchlists(vec![list("SS"), list("ß")]);
        assert_eq!(out.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["SS", "ß (2)"]);
        let out = sanitize_watchlists(vec![list("s"), list("ſ")]);
        assert_eq!(out.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["s", "ſ (2)"]);
        let out = sanitize_watchlists(vec![list("σ"), list("ς")]);
        assert_eq!(out.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["σ", "ς (2)"]);
        let root = std::env::temp_dir().join(format!("named-watchlists-casefold-{}", std::process::id()));
        let mut app = TerminalApp::test_workspace(&root);
        app.create_watchlist("SS".into(), Vec::new()).unwrap();
        app.create_watchlist("Other".into(), Vec::new()).unwrap();
        assert!(app.create_watchlist("ß".into(), Vec::new()).is_err());
        assert!(app.rename_watchlist(2, "ß".into()).is_err());
        app.execute("WL USE ß");
        assert_eq!(app.active_watchlist, 1);
    }

    #[test]
    fn watchlist_names_must_reject_zero_width_and_bidi_characters() {
        let root = std::env::temp_dir().join(format!("named-watchlists-format-{}", std::process::id()));
        let mut app = TerminalApp::test_workspace(&root);
        for code in [0xAD, 0xFEFF]
            .into_iter()
            .chain(0x200B..=0x200F)
            .chain(0x202A..=0x202E)
            .chain(0x2060..=0x2064)
            .chain(0x2066..=0x2069)
            .chain([0, 0x7F])
        {
            let name = format!("Main{}", char::from_u32(code).unwrap());
            assert!(app.create_watchlist(name.clone(), Vec::new()).is_err());
            assert!(app.rename_watchlist(0, name.clone()).is_err());
            assert!(!valid_watchlist_name(&name));
        }
        assert!(app.create_watchlist("   ".into(), Vec::new()).is_err());
        app.create_watchlist("  Trimmed  ".into(), Vec::new()).unwrap();
        assert_eq!(app.watchlists[1].name, "Trimmed");
        let list = |name: &str| NamedWatchlist::new(name.into(), Vec::new());
        let invisible = |c: char| matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}');
        let out = sanitize_watchlists(vec![list("\u{200B}"), list("Main\u{200B}"), list("Main"), list("\u{202E}niam")]);
        assert!(
            out.iter().all(|l| !l.name.chars().any(invisible) && !l.name.trim().is_empty()),
            "invisible characters and blank names must not survive sanitisation: {:?}",
            out.iter().map(|l| &l.name).collect::<Vec<_>>()
        );
        // A name that differs from an existing one only by zero-width characters is a duplicate.
        let out = sanitize_watchlists(vec![list("Main\u{200B}"), list("Main")]);
        let visible: Vec<String> =
            out.iter().map(|l| l.name.chars().filter(|c| !invisible(*c)).collect::<String>()).collect();
        assert!(
            visible.iter().filter(|n| n.as_str() == "Main").count() <= 1,
            "names that differ only by zero-width characters must collide: {visible:?}"
        );
    }

    #[test]
    fn save_matches_as_watchlist_must_uniquify_colliding_names() {
        let root = std::env::temp_dir().join(format!("named-watchlists-save-collision-{}", std::process::id()));
        let mut app = TerminalApp::test_workspace(&root);
        app.run_actions(vec![Action::SaveMatchesAsWatchlist("Momentum 2026-10-21".into(), vec!["AAPL".into()])]);
        assert_eq!(app.watchlists.len(), 2);
        assert_eq!(app.watchlist, vec![tr_data::security("AAPL").key()]);
        app.run_actions(vec![Action::SaveMatchesAsWatchlist("Momentum 2026-10-21".into(), vec!["NVDA".into()])]);
        assert_eq!(app.watchlists.len(), 3, "a colliding save-as-new name must be uniquified, not dropped");
        assert_eq!(app.watchlist, vec![tr_data::security("NVDA").key()]);
        assert!(app.watchlists[1].name != app.watchlists[2].name);
    }

    #[test]
    fn undo_watchlist_is_discarded_across_list_switches_and_cannot_land_on_another_list() {
        let root = std::env::temp_dir().join(format!("named-watchlists-undo-switch-{}", std::process::id()));
        let mut app = TerminalApp::test_workspace(&root);
        app.add_watchlist_symbols(vec!["AAPL".into()]);
        let main = app.watchlist.clone();
        app.create_watchlist("Second".into(), vec!["NVDA".into()]).unwrap();
        app.run_actions(vec![Action::ReplaceWatchlistWithMatches(vec!["TSLA".into()])]);
        assert_ne!(app.watchlist, main);
        app.switch_watchlist(0);
        assert!(app.watchlist_undo.is_none(), "switching lists must discard the pending undo");
        app.undo_watchlist();
        assert_eq!(app.watchlist, main, "undo after a switch must not touch the newly active list");
        assert_eq!(app.watchlists[1].symbols, vec![tr_data::security("TSLA").key()]);
    }

    #[test]
    fn deleting_the_active_watchlist_keeps_the_active_index_in_range() {
        let root = std::env::temp_dir().join(format!("named-watchlists-delete-active-{}", std::process::id()));
        let mut app = TerminalApp::test_workspace(&root);
        for name in ["A", "B", "C"] {
            app.create_watchlist(name.into(), vec![name.to_string()]).unwrap();
        }
        assert_eq!(app.active_watchlist, 3);
        app.delete_watchlist(3).unwrap();
        assert!(app.active_watchlist < app.watchlists.len());
        assert_eq!(app.watchlist, app.watchlists[app.active_watchlist].symbols);
        app.switch_watchlist(app.watchlists.len() - 1);
        app.delete_watchlist(app.watchlists.len() - 1).unwrap();
        assert!(app.active_watchlist < app.watchlists.len());
        assert_eq!(app.watchlist, app.watchlists[app.active_watchlist].symbols);
    }

    #[test]
    fn switching_watchlists_invalidates_column_replies_for_the_previous_list() {
        let root = std::env::temp_dir().join(format!("named-watchlists-columns-switch-{}", std::process::id()));
        let mut app = TerminalApp::test_workspace(&root);
        app.create_watchlist("B".into(), vec!["NVDA".into()]).unwrap();
        app.switch_watchlist(0);
        let revision = app.columns_revision;
        app.column_cells.insert("AAPL".into(), Vec::new());
        app.switch_watchlist(1);
        assert!(app.columns_revision > revision, "the reply revision must move with the list");
        assert!(app.column_cells.is_empty(), "cells from the previous list must not survive the switch");
        assert_eq!(accept_columns(revision, app.columns_revision, "old cells"), None);
    }

    #[test]
    fn stale_rename_entry_must_not_rename_the_wrong_list_or_panic() {
        let root = std::env::temp_dir().join(format!("named-watchlists-stale-rename-{}", std::process::id()));
        let mut app = TerminalApp::test_workspace(&root);
        for name in ["A", "B", "C"] {
            app.create_watchlist(name.into(), Vec::new()).unwrap();
        }
        // The rename entry is open for "B" at index 2; deleting "Main" shifts B to index 1.
        app.watchlist_entry = Some((Some(watchlist_name_key("B")), "Renamed B".into()));
        app.delete_watchlist(0).unwrap();
        assert_eq!(app.watchlists[1].name, "B");
        // Apply by name, not the old index: C must remain untouched.
        app.apply_watchlist_entry().unwrap();
        assert_eq!(app.watchlists[1].name, "Renamed B");
        assert_eq!(app.watchlists[2].name, "C");
        app.watchlist_entry = Some((Some(watchlist_name_key("Renamed B")), "Zombie".into()));
        app.delete_watchlist(1).unwrap();
        assert!(app.apply_watchlist_entry().is_err());
        assert!(app.rename_watchlist(app.watchlists.len(), "Zombie".into()).is_err());
        assert!(app.watchlists.iter().all(|l| l.name != "Zombie"), "the wrong list was renamed");
    }

    #[test]
    fn duplicate_watchlist_copy_is_independent_of_the_original() {
        let key = |s: &str| tr_data::security(s).key();
        let root = std::env::temp_dir().join(format!("watch-sections-duplicate-{}", std::process::id()));
        let mut app = TerminalApp::test_workspace(&root);
        use crate::watch_sections::Item;
        app.set_watchlist_items(&[
            Item::Symbol(key("SPY")),
            Item::Section { name: "Tech".into(), collapsed: true },
            Item::Symbol(key("NVDA")),
            Item::Section { name: "Macro".into(), collapsed: false },
        ]);
        let original = app.watchlists[0].clone();
        app.duplicate_watchlist().unwrap();
        assert_eq!(app.active_watchlist, 1);
        assert_eq!(app.watchlists[1].items(), original.items());
        // Edit the copy: add a symbol, rename a section, expand Tech.
        app.set_watchlist_items(&[
            Item::Symbol(key("SPY")),
            Item::Section { name: "Growth".into(), collapsed: false },
            Item::Symbol(key("NVDA")),
            Item::Symbol(key("AAPL")),
            Item::Section { name: "Macro".into(), collapsed: false },
        ]);
        assert_eq!(app.watchlists[0], original, "editing the duplicate must not touch the original");
        assert_ne!(app.watchlists[1].sections[0].name, "Tech");
    }

    #[test]
    fn add_to_named_watchlist_preserves_the_target_lists_sections() {
        let key = |s: &str| tr_data::security(s).key();
        let root = std::env::temp_dir().join(format!("watch-sections-named-add-{}", std::process::id()));
        let mut app = TerminalApp::test_workspace(&root);
        use crate::watch_sections::Item;
        app.create_watchlist("Tech".into(), Vec::new()).unwrap();
        app.set_watchlist_items(&[
            Item::Section { name: "Chips".into(), collapsed: false },
            Item::Symbol(key("NVDA")),
            Item::Section { name: "Software".into(), collapsed: true },
        ]);
        let tech = app.active_watchlist;
        app.create_watchlist("Main2".into(), Vec::new()).unwrap();
        app.run_actions(vec![Action::AddToNamedWatchlist { list: "Tech".into(), symbol: key("AAPL") }]);
        let rows = app.watchlists[tech].items();
        assert!(rows.iter().any(|i| matches!(i, Item::Section { name, collapsed: true } if name == "Software")));
        assert_eq!(rows.iter().filter(|i| matches!(i, Item::Symbol(_))).count(), 2);
        // The new symbol joins the end of the last section rather than stripping headers.
        assert!(matches!(rows.last().unwrap(), Item::Symbol(s) if s == &key("AAPL")));
        assert_eq!(app.watchlists[tech].sections.len(), 2);
    }

    fn agent_fixture(tag: &str) -> TerminalApp {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(tag);
        let mut app = TerminalApp::test_workspace(&root);
        app.panes.push(Pane {
            id: 1,
            kind: PaneKind::Chart(Box::new(ChartState::new(1, "AAA", Timeframe::D1))),
            link: None,
            title: None,
            window: None,
        });
        app.focus = 1;
        app.next_id = 2;
        app.dock = Node::leaf(1);
        app
    }

    fn logic_only(app: &mut TerminalApp) {
        eframe::App::logic(app, &egui::Context::default(), &mut eframe::Frame::_new_kittest());
    }

    #[test]
    fn logic_alone_applies_history_and_live_bars() {
        use tr_chart::scene::SeriesGpu;
        use tr_core::Bar;
        let mut app = agent_fixture("agent-hidden-history");
        let (out, rx) = std::sync::mpsc::channel();
        app.worker.rx = rx;
        let bars = std::sync::Arc::new(vec![Bar::new(86400, 1., 2., 0., 1., 10.)]);
        out.send(Response::History {
            pane: 1,
            generation: 0,
            revision: 1,
            symbol: tr_data::security("AAA").key(),
            tf: Timeframe::D1,
            bars: bars.clone(),
            gpu: SeriesGpu::build(&bars),
            prepended: 0,
            from_cache: false,
            exhausted: false,
            live: false,
            crypto_source: None,
        })
        .unwrap();
        logic_only(&mut app);
        assert_eq!(app.chart_mut(1).unwrap().displayed_bars().bars, bars.as_slice());
        let live = Bar::new(172800, 2., 3., 1., 2.5, 20.);
        out.send(Response::LiveBars { pane: 1, generation: 0, revision: 2, from: 1, bars: vec![live], len: 2 })
            .unwrap();
        logic_only(&mut app);
        let chart = app.chart_mut(1).unwrap();
        assert_eq!(chart.displayed_bars().bars, &[bars[0], live]);
        assert_eq!(chart.source_bars().bars, &[bars[0], live]);
        assert!(app.edit_log.newest(200, true).is_empty(), "worker updates are not user edits");
    }

    #[test]
    fn logic_queue_delivers_replies_expires_cancels_and_contains_panics() {
        use crate::agent_queue::{AgentJob, AgentReply, ChartTarget};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let mut app = agent_fixture("agent-queue");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut replies = Vec::new();
        for (id, turn, expired) in
            [("ok", "a", false), ("expired", "a", true), ("cancel", "b", false), ("panic", "a", false)]
        {
            let (reply, rx) = tokio::sync::oneshot::channel();
            replies.push(rx);
            let calls = calls.clone();
            app.agent_queue.push(AgentJob {
                id: id.into(),
                turn: turn.into(),
                chart: ChartTarget::Pinned,
                deadline: if expired {
                    Instant::now() - Duration::from_secs(1)
                } else {
                    Instant::now() + Duration::from_secs(10)
                },
                run: Box::new(move |app| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    assert_ne!(id, "panic", "injected job panic");
                    let chart = app.job_chart_mut().unwrap();
                    chart.log = true;
                    AgentReply { success: true, value: serde_json::json!({ "uid": chart.uid }) }
                }),
                reply,
            });
        }
        app.agent_queue.cancel("b");
        logic_only(&mut app);
        let replies: Vec<_> = replies.iter_mut().map(|rx| rx.try_recv().unwrap()).collect();
        assert!(replies[0].success);
        assert_eq!(replies[1].value["error"], "timeout");
        assert_eq!(replies[2].value["error"], "cancelled");
        assert!(!replies[3].success);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(app.chart_mut(1).unwrap().log);
    }

    #[test]
    fn queue_push_wakes_context_and_cancel_reaches_jobs_not_yet_started() {
        use crate::agent_queue::{AgentJob, AgentReply, AgentToolQueue, ChartTarget};
        let mut app = agent_fixture("agent-queue-wake");
        let ctx = egui::Context::default();
        let woke = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wake = woke.clone();
        ctx.set_request_repaint_callback(move |_| {
            wake.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        app.agent_queue = AgentToolQueue::new(ctx);
        let mut receivers = Vec::new();
        for turn in ["first", "second"] {
            let (reply, rx) = tokio::sync::oneshot::channel();
            receivers.push(rx);
            let queue = app.agent_queue.clone();
            std::thread::spawn(move || {
                queue.push(AgentJob {
                    id: turn.into(),
                    turn: turn.into(),
                    chart: ChartTarget::Pinned,
                    deadline: Instant::now() + Duration::from_secs(10),
                    run: Box::new(|app| {
                        app.agent_queue.cancel("second");
                        AgentReply { success: true, value: serde_json::Value::Null }
                    }),
                    reply,
                })
            })
            .join()
            .unwrap();
        }
        assert!(woke.load(std::sync::atomic::Ordering::SeqCst));
        logic_only(&mut app);
        assert!(receivers[0].try_recv().unwrap().success);
        assert_eq!(receivers[1].try_recv().unwrap().value["error"], "cancelled");
    }

    #[test]
    fn chart_uids_survive_rearrangement_but_layout_replacement_clears_pin() {
        use crate::agent_queue::ChartTarget;
        let mut app = agent_fixture("agent-chart-identity");
        let uid = app.chart_mut(1).unwrap().uid;
        app.pinned_agent_chart = Some(uid);
        let mut duplicate = ChartState::new(0, "BBB", Timeframe::D1);
        duplicate.uid = uid;
        let second = app.new_pane(PaneKind::Chart(Box::new(duplicate)));
        let second_uid = app.chart_mut(second).unwrap().uid;
        assert_ne!(second_uid, uid);
        app.dock_pane(second, Some(1), crate::dock::Zone::Right);
        app.panes[0].window = Some(WindowGeometry { x: 10., y: 20., w: 800., h: 600. });
        assert_eq!(app.agent_chart_uid(ChartTarget::Pinned), Some(uid));
        let saved = app.current_doc();
        let loaded = LayoutDoc::from_json(&saved.to_json()).unwrap();
        app.apply_doc(loaded);
        assert_eq!(app.pinned_agent_chart, None);
        let active_uid = app.peek_chart().unwrap().uid;
        assert_ne!(active_uid, uid);
        assert_ne!(active_uid, second_uid);
        assert_eq!(app.agent_chart_uid(ChartTarget::Pinned), Some(active_uid));
        assert!(app.agent_chart_mut(ChartTarget::Uid(uid)).is_none());
        assert!(app.agent_chart_mut(ChartTarget::Uid(second_uid)).is_none());
        app.pinned_agent_chart = Some(u64::MAX);
        assert_eq!(app.agent_chart_uid(ChartTarget::Pinned), Some(active_uid));
        assert!(app.agent_chart_mut(ChartTarget::Uid(u64::MAX)).is_none());
    }

    #[test]
    fn a1a_hidden_drain_then_paint_matches_old_ui_time_drain_after_resize() {
        use tr_chart::scene::SeriesGpu;
        use tr_core::Bar;
        let mut hidden = agent_fixture("a1a-hidden-parity");
        let mut old = agent_fixture("a1a-ui-drain-parity");
        old.chart_mut(1).unwrap().uid = hidden.chart_mut(1).unwrap().uid;
        let (hidden_tx, hidden_rx) = std::sync::mpsc::channel();
        let (old_tx, old_rx) = std::sync::mpsc::channel();
        hidden.worker.rx = hidden_rx;
        old.worker.rx = old_rx;
        let bars =
            std::sync::Arc::new((0..100).map(|i| Bar::new(i * 86400, 100., 110., 90., 105., 10.)).collect::<Vec<_>>());
        for tx in [&hidden_tx, &old_tx] {
            tx.send(Response::History {
                pane: 1,
                generation: 0,
                revision: 1,
                symbol: tr_data::security("AAA").key(),
                tf: Timeframe::D1,
                bars: bars.clone(),
                gpu: SeriesGpu::build(&bars),
                prepended: 0,
                from_cache: false,
                exhausted: false,
                live: false,
                crypto_source: None,
            })
            .unwrap();
            tx.send(Response::LiveBars {
                pane: 1,
                generation: 0,
                revision: 2,
                from: 100,
                bars: vec![Bar::new(100 * 86400, 105., 115., 95., 110., 20.)],
                len: 101,
            })
            .unwrap();
        }
        logic_only(&mut hidden); // No Ui, layout, input or viewport measurement exists yet.
        assert_eq!(hidden.chart_mut(1).unwrap().displayed_bars().bars.len(), 101);
        assert!(hidden.read_user_actions(200).is_empty());
        let hidden_ctx = egui::Context::default();
        let old_ctx = egui::Context::default();
        for width in [900., 1500., 1000.] {
            for (ctx, app, drain_in_ui) in [(&hidden_ctx, &mut hidden, false), (&old_ctx, &mut old, true)] {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 600.))),
                        ..Default::default()
                    },
                    |ui| {
                        if drain_in_ui {
                            app.drain();
                        }
                        let worker = &app.worker;
                        let PaneKind::Chart(chart) = &mut app.panes[0].kind else { panic!("chart") };
                        chart.show(ui, ui.max_rect(), true, worker);
                    },
                );
                output.textures_delta.clear();
            }
            assert_eq!(
                serde_json::to_value(hidden.chart_mut(1).unwrap().read_chart()).unwrap(),
                serde_json::to_value(old.chart_mut(1).unwrap().read_chart()).unwrap(),
                "width {width}"
            );
        }
    }

    #[test]
    fn a1a_logic_applies_responses_before_jobs_and_later_user_input_invalidates_old_generation() {
        use crate::agent_queue::{AgentJob, AgentReply, ChartTarget};
        use tr_chart::scene::SeriesGpu;
        use tr_core::Bar;
        let mut app = agent_fixture("a1a-frame-order");
        let (tx, rx) = std::sync::mpsc::channel();
        app.worker.rx = rx;
        let bars = std::sync::Arc::new(vec![Bar::new(86400, 1., 2., 0., 1., 10.)]);
        let send_history = || {
            tx.send(Response::History {
                pane: 1,
                generation: 0,
                revision: 1,
                symbol: tr_data::security("AAA").key(),
                tf: Timeframe::D1,
                bars: bars.clone(),
                gpu: SeriesGpu::build(&bars),
                prepended: 0,
                from_cache: false,
                exhausted: false,
                live: false,
                crypto_source: None,
            })
            .unwrap()
        };
        send_history();
        let (reply, mut receive) = tokio::sync::oneshot::channel();
        app.agent_queue.push(AgentJob {
            id: "read".into(),
            turn: "t".into(),
            chart: ChartTarget::Pinned,
            deadline: Instant::now() + Duration::from_secs(60),
            reply,
            run: Box::new(|app| AgentReply {
                success: true,
                value: serde_json::json!(app.job_chart_mut().unwrap().displayed_bars().bars.len()),
            }),
        });
        logic_only(&mut app);
        assert_eq!(receive.try_recv().unwrap().value, serde_json::json!(1));
        // The same frame's command input is processed after logic, as in ui().
        app.execute("MSFT");
        assert_eq!(app.chart_mut(1).unwrap().symbol, "MSFT US Equity");
        send_history(); // Late response for the previous generation/symbol.
        logic_only(&mut app);
        assert!(app.chart_mut(1).unwrap().displayed_bars().bars.is_empty());
        assert!(app.read_user_actions(200).iter().any(|e| e.kind == "symbol"));
    }

    #[test]
    fn a1a_cancel_during_run_cancels_pending_siblings_without_rolling_back_started_job() {
        use crate::agent_queue::{AgentJob, AgentReply, ChartTarget};
        let mut app = agent_fixture("a1a-cancel-running");
        let mut receivers = Vec::new();
        for index in 0..2 {
            let (reply, receive) = tokio::sync::oneshot::channel();
            receivers.push(receive);
            app.agent_queue.push(AgentJob {
                id: index.to_string(),
                turn: "same-turn".into(),
                chart: ChartTarget::Pinned,
                deadline: Instant::now() + Duration::from_secs(60),
                reply,
                run: Box::new(move |app| {
                    assert_eq!(index, 0, "pending sibling must never run");
                    app.job_chart_mut().unwrap().log = true;
                    app.agent_queue.cancel("same-turn");
                    AgentReply { success: true, value: serde_json::Value::Null }
                }),
            });
        }
        logic_only(&mut app);
        assert!(receivers[0].try_recv().unwrap().success);
        assert_eq!(receivers[1].try_recv().unwrap().value["error"], "cancelled");
        assert!(app.chart_mut(1).unwrap().log);
        assert_eq!(app.agent_queue.len(), 0);
    }

    #[test]
    fn a1a_idle_logic_does_not_request_repaints() {
        let ctx = egui::Context::default();
        let mut app = agent_fixture("a1a-idle-logic");
        app.agent_queue = crate::agent_queue::AgentToolQueue::new(ctx.clone());
        let wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = wakes.clone();
        ctx.set_request_repaint_callback(move |_| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        for _ in 0..100 {
            eframe::App::logic(&mut app, &ctx, &mut eframe::Frame::_new_kittest());
        }
        assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(app.read_user_actions(200).is_empty());
    }

    #[test]
    fn a1a_bug_layout_uid_collision_retargets_pin() {
        use crate::agent_queue::ChartTarget;
        let mut app = agent_fixture("a1a-pin-layout-collision");
        let old_uid = app.chart_mut(1).unwrap().uid;
        app.pinned_agent_chart = Some(old_uid);
        let mut unrelated = app.current_doc();
        unrelated.name = "Unrelated imported layout".into();
        let PaneSettings::Chart { symbol, .. } = &mut unrelated.panes[0].settings else { panic!("chart") };
        *symbol = "MSFT US Equity".into(); // Imported uid happens to collide with the old pin.
        let mut active = unrelated.panes[0].clone();
        active.id = 2;
        let PaneSettings::Chart { uid, symbol, .. } = &mut active.settings else { panic!("chart") };
        *uid = tr_chart::drawings::next_id();
        *symbol = "NVDA US Equity".into();
        let active_uid = *uid;
        unrelated.panes.push(active);
        unrelated.dock = Node::leaf(2);
        unrelated.focus = Some(2);
        app.apply_doc(unrelated);
        assert_eq!(
            app.agent_chart_uid(ChartTarget::Pinned),
            Some(active_uid),
            "closing the pinned chart must fall back to active, not alias another layout's uid"
        );
    }

    #[test]
    fn a1a_queue_boundary_dropped_reply_nested_push_and_panic_edit_history() {
        use crate::agent_queue::{AgentJob, AgentReply, ChartTarget};
        use tr_chart::drawings::{Anchor, Author, Drawing, Tool};
        let mut app = agent_fixture("a1a-queue-boundaries");
        let (expired_reply, mut expired) = tokio::sync::oneshot::channel();
        app.agent_queue.push(AgentJob {
            id: "boundary".into(),
            turn: "expired".into(),
            chart: ChartTarget::Pinned,
            deadline: Instant::now(),
            reply: expired_reply,
            run: Box::new(|_| panic!("a deadline equal to now must not execute")),
        });
        let (reply, receive) = tokio::sync::oneshot::channel();
        drop(receive);
        app.agent_queue.push(AgentJob {
            id: "dropped-receiver".into(),
            turn: "partial".into(),
            chart: ChartTarget::Pinned,
            deadline: Instant::now() + Duration::from_secs(60),
            reply,
            run: Box::new(|app| {
                let a = Anchor { ts: 86400, price: 100. };
                app.job_chart_mut().unwrap().drawings.push(Drawing::new(Tool::Horizontal, a, a));
                let (reply, receive) = tokio::sync::oneshot::channel();
                drop(receive);
                app.agent_queue.push(AgentJob {
                    id: "nested".into(),
                    turn: "nested".into(),
                    chart: ChartTarget::Pinned,
                    deadline: Instant::now() + Duration::from_secs(60),
                    reply,
                    run: Box::new(|app| {
                        app.job_chart_mut().unwrap().log = true;
                        AgentReply { success: true, value: serde_json::Value::Null }
                    }),
                });
                panic!("after a complete drawing mutation");
            }),
        });
        logic_only(&mut app);
        assert_eq!(expired.try_recv().unwrap().value["error"], "timeout");
        assert_eq!(app.agent_queue.len(), 1, "new jobs wait for the next frame");
        assert!(!app.chart_mut(1).unwrap().log);
        let chart = app.chart_mut(1).unwrap();
        assert_eq!(chart.drawings[0].created_by, Author::Agent);
        assert_eq!(chart.edit_author, Author::User, "panic must restore mutation attribution");
        assert_eq!(chart.undo_turn("partial").reverted, 1);
        assert!(chart.drawings.is_empty());
        logic_only(&mut app);
        assert!(app.chart_mut(1).unwrap().log);
    }

    #[test]
    fn a1a_layout_reload_preserves_ids_but_not_session_undo_history() {
        use tr_chart::drawings::{Anchor, Author, Drawing, Tool};
        let mut app = agent_fixture("a1a-history-reload");
        let uid = app.chart_mut(1).unwrap().uid;
        app.chart_mut(1).unwrap().edit_drawings(Author::Agent, Some("t".into()), |d| {
            let a = Anchor { ts: 86400, price: 100. };
            let mut drawing = Drawing::new(Tool::Horizontal, a, a);
            drawing.symbol = Some("AAA".into());
            d.push(drawing);
        });
        let before = app.chart_mut(1).unwrap().drawings.clone();
        let bytes = app.current_doc().to_json();
        app.apply_doc(LayoutDoc::from_json(&bytes).unwrap());
        assert!(app.agent_chart_mut(crate::agent_queue::ChartTarget::Uid(uid)).is_none());
        let chart = app.agent_chart_mut(crate::agent_queue::ChartTarget::Pinned).unwrap();
        assert_ne!(chart.uid, uid, "loading replaces the live chart, not its drawings' identities");
        assert_eq!(chart.drawings, before);
        // LayoutDoc never persisted snapshot undo either. Attribution persists,
        // but undo_turn, like ordinary undo, is scoped to this loaded session.
        assert_eq!(chart.undo_turn("t").reverted, 0);
        chart.undo_drawings();
        assert_eq!(chart.drawings, before);
    }

    #[test]
    fn a1a_bug_agent_backlog_has_no_frame_budget() {
        use crate::agent_queue::{AgentJob, AgentReply, ChartTarget};
        let mut app = agent_fixture("a1a-queue-budget");
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for index in 0..4096 {
            let (reply, receive) = tokio::sync::oneshot::channel();
            drop(receive);
            let calls = calls.clone();
            app.agent_queue.push(AgentJob {
                id: index.to_string(),
                turn: "t".into(),
                chart: ChartTarget::Pinned,
                deadline: Instant::now() + Duration::from_secs(60),
                reply,
                run: Box::new(move |_| {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    AgentReply { success: true, value: serde_json::Value::Null }
                }),
            });
        }
        logic_only(&mut app);
        assert!(
            calls.load(std::sync::atomic::Ordering::SeqCst) < 4096,
            "a large backlog must yield to rendering/input, not execute every job in one frame"
        );
        assert!(app.agent_queue.len() > 0);
    }

    #[test]
    fn a1a_bug_layout_load_records_user_actions() {
        let mut app = agent_fixture("a1a-layout-log");
        logic_only(&mut app);
        assert!(app.read_user_actions(200).is_empty());
        let mut doc = app.current_doc();
        let PaneSettings::Chart { symbol, .. } = &mut doc.panes[0].settings else { panic!("chart") };
        *symbol = "MSFT US Equity".into();
        app.apply_doc(doc);
        logic_only(&mut app);
        assert!(app.read_user_actions(200).is_empty(), "loading a layout is not a user chart edit");
    }

    #[test]
    fn a1a_bug_cross_chart_insertion_duplicates_drawing_id() {
        use tr_chart::drawings::{Anchor, Author, Drawing, Tool};
        let mut app = agent_fixture("a1a-cross-chart-ids");
        app.chart_mut(1).unwrap().edit_drawings(Author::User, None, |d| {
            let a = Anchor { ts: 86400, price: 100. };
            let mut drawing = Drawing::new(Tool::Horizontal, a, a);
            drawing.symbol = Some("AAA".into());
            drawing.all_symbols = true;
            d.push(drawing);
        });
        let copied = app.chart_mut(1).unwrap().drawings[0].clone();
        let second = app.new_pane(PaneKind::Chart(Box::new(ChartState::new(0, "BBB", Timeframe::D1))));
        app.chart_mut(second).unwrap().edit_drawings(Author::User, None, |d| d.push(copied.clone()));
        assert_ne!(
            app.chart_mut(second).unwrap().drawings[0].id,
            copied.id,
            "two independently editable drawings cannot share an id"
        );
    }

    #[test]
    fn agent_budget_expires_jobs_beyond_budget_and_preserves_pending_deadlines() {
        use crate::agent_queue::{AgentJob, AgentReply, ChartTarget, MAX_JOBS_PER_FRAME};
        let mut app = agent_fixture("agent-budget-expiry");
        let deadline = Instant::now() + Duration::from_secs(60);
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for index in 0..MAX_JOBS_PER_FRAME * 3 {
            let (reply, receive) = tokio::sync::oneshot::channel();
            drop(receive);
            let calls = calls.clone();
            app.agent_queue.push(AgentJob {
                id: index.to_string(),
                turn: "t".into(),
                chart: ChartTarget::Pinned,
                deadline,
                reply,
                run: Box::new(move |_| {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    AgentReply { success: true, value: serde_json::Value::Null }
                }),
            });
        }
        let (reply, mut expired) = tokio::sync::oneshot::channel();
        app.agent_queue.push(AgentJob {
            id: "expired-at-back".into(),
            turn: "t".into(),
            chart: ChartTarget::Pinned,
            deadline: Instant::now(),
            reply,
            run: Box::new(|_| panic!("expired jobs cannot run")),
        });
        logic_only(&mut app);
        assert_eq!(expired.try_recv().unwrap().value["error"], "timeout");
        let completed = calls.load(std::sync::atomic::Ordering::SeqCst);
        assert!((1..=MAX_JOBS_PER_FRAME).contains(&completed));
        assert_eq!(app.agent_queue.len(), MAX_JOBS_PER_FRAME * 3 - completed);
        // Inspect without changing order or renewing any deadline.
        for index in completed..MAX_JOBS_PER_FRAME * 3 {
            let job = app.agent_queue.pop().unwrap();
            assert_eq!(job.id, index.to_string());
            assert_eq!(job.deadline, deadline);
            app.agent_queue.push(job);
        }
        while app.agent_queue.len() > 0 {
            logic_only(&mut app);
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), MAX_JOBS_PER_FRAME * 3);
    }

    #[test]
    fn agent_time_budget_yields_and_expires_jobs_that_age_during_execution() {
        use crate::agent_queue::{AgentJob, AgentReply, ChartTarget, MAX_JOB_TIME_PER_FRAME};
        let mut app = agent_fixture("agent-time-budget");
        let mut replies = Vec::new();
        for index in 0..3 {
            let (reply, receive) = tokio::sync::oneshot::channel();
            replies.push(receive);
            app.agent_queue.push(AgentJob {
                id: index.to_string(),
                turn: "t".into(),
                chart: ChartTarget::Pinned,
                deadline: Instant::now() + if index == 2 { Duration::from_millis(1) } else { Duration::from_secs(60) },
                reply,
                run: Box::new(move |_| {
                    assert_ne!(index, 2, "job must expire even beyond the work budget");
                    if index == 0 {
                        std::thread::sleep(MAX_JOB_TIME_PER_FRAME + Duration::from_millis(1));
                    }
                    AgentReply { success: true, value: serde_json::Value::Null }
                }),
            });
        }
        logic_only(&mut app);
        assert!(replies[0].try_recv().unwrap().success);
        assert!(matches!(replies[1].try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
        assert_eq!(replies[2].try_recv().unwrap().value["error"], "timeout");
        assert_eq!(app.agent_queue.len(), 1);
        logic_only(&mut app);
        assert!(replies[1].try_recv().unwrap().success);
    }

    #[test]
    fn closing_or_replacing_a_pin_never_rebinds_it_to_a_reused_uid() {
        use crate::agent_queue::ChartTarget;
        for replace in [false, true] {
            let mut app = agent_fixture(if replace { "pin-replaced" } else { "pin-closed" });
            let uid = app.chart_mut(1).unwrap().uid;
            let active = app.new_pane(PaneKind::Chart(Box::new(ChartState::new(0, "BBB", Timeframe::D1))));
            app.dock_pane(active, Some(1), crate::dock::Zone::Right);
            let active_uid = app.chart_mut(active).unwrap().uid;
            app.pinned_agent_chart = Some(uid);
            if replace {
                app.replace_pane(1, PaneKind::Help);
            } else {
                app.close_pane(1);
            }
            assert_eq!(app.pinned_agent_chart, None);
            let mut unrelated = ChartState::new(0, "CCC", Timeframe::D1);
            unrelated.uid = uid;
            let later = app.new_pane(PaneKind::Chart(Box::new(unrelated)));
            assert_eq!(app.chart_mut(later).unwrap().uid, uid);
            assert_eq!(app.agent_chart_uid(ChartTarget::Pinned), Some(active_uid));
        }
    }

    #[test]
    fn clear_drawings_command_is_undoable_and_agent_edits_are_not_user_actions() {
        use crate::agent_queue::{AgentJob, AgentReply, ChartTarget};
        use tr_chart::drawings::{Anchor, Author, Drawing, Tool};
        let mut app = agent_fixture("agent-edit-events");
        logic_only(&mut app);
        let anchor = Anchor { ts: 86400, price: 1. };
        app.chart_mut(1).unwrap().edit_drawings(Author::User, None, |drawings| {
            drawings.push(Drawing::new(Tool::Horizontal, anchor, anchor))
        });
        app.apply(Function::ClearDrawings);
        assert!(app.chart_mut(1).unwrap().drawings.is_empty());
        app.chart_mut(1).unwrap().undo_drawings();
        assert_eq!(app.chart_mut(1).unwrap().drawings.len(), 1);
        logic_only(&mut app);
        let user_count = app.edit_log.newest(200, true).len();
        let (reply, mut rx) = tokio::sync::oneshot::channel();
        app.agent_queue.push(AgentJob {
            id: "edit".into(),
            turn: "turn".into(),
            chart: ChartTarget::Pinned,
            deadline: Instant::now() + Duration::from_secs(10),
            reply,
            run: Box::new(|app| {
                app.job_chart_mut().unwrap().drawings.push(Drawing::new(
                    Tool::Horizontal,
                    Anchor { ts: 86400, price: 2. },
                    Anchor { ts: 86400, price: 2. },
                ));
                AgentReply { success: true, value: serde_json::Value::Null }
            }),
        });
        logic_only(&mut app);
        assert!(rx.try_recv().unwrap().success);
        assert_eq!(app.edit_log.newest(200, true).len(), user_count);
        assert_eq!(app.edit_log.newest(1, false)[0].author, Author::Agent);
        assert_eq!(app.chart_mut(1).unwrap().drawings[1].created_by, Author::Agent);
        assert_eq!(app.chart_mut(1).unwrap().drawings[1].turn_id.as_deref(), Some("turn"));
    }

    #[test]
    fn named_add_selects_section_preserves_keys_and_reports_duplicates_without_moving() {
        use crate::chart_view::WatchlistSectionTarget;
        use crate::watch_sections::Item;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/f02-section-add");
        let mut app = TerminalApp::test_workspace(&root);
        app.create_watchlist("Crypto".into(), Vec::new()).unwrap();
        app.set_watchlist_items(&[
            Item::Section { name: "Spot".into(), collapsed: true },
            Item::Section { name: "Spot".into(), collapsed: false },
        ]);
        let crypto = app.active_watchlist;
        app.switch_watchlist(0);
        let before = app.watchlist.clone();
        let section = WatchlistSectionTarget { index: 0, name: "Spot".into() };
        let key = "COINBASE:BTC-USD";
        let outcome = app.add_to_named_watchlist("Crypto", key.into(), Some(&section)).unwrap();
        assert!(outcome.added && !outcome.truncated);
        assert_eq!(outcome.message, "Added BTC/USD to Crypto");
        assert_eq!(app.watchlists[crypto].symbols, [key]);
        assert_eq!(app.watchlists[crypto].items()[1], Item::Symbol(key.into()));
        assert_eq!(app.watchlists[crypto].sections[1].at, 1);
        assert!(app.watchlists[crypto].sections[0].collapsed);
        assert_eq!(app.active_watchlist, 0);
        assert_eq!(app.watchlist, before);
        assert!(app.status.is_empty() && app.toasts.is_empty(), "headless adds do not emit UI notices");

        let rows = app.watchlists[crypto].items();
        let other_section = WatchlistSectionTarget { index: 1, name: "Spot".into() };
        let duplicate = app.add_to_named_watchlist("Crypto", key.into(), Some(&other_section)).unwrap();
        assert!(!duplicate.added && !duplicate.truncated);
        assert_eq!(duplicate.message, "BTC/USD is already in Crypto");
        let alias = app.add_to_named_watchlist("Crypto", format!("{key} Curncy"), None).unwrap();
        assert!(!alias.added && !alias.truncated);
        assert_eq!(alias.message, duplicate.message);
        assert_eq!(app.watchlists[crypto].items(), rows);
        let saved: Vec<NamedWatchlist> = serde_json::from_slice(&serde_json::to_vec(&app.watchlists).unwrap()).unwrap();
        assert_eq!(sanitize_watchlists(saved), app.watchlists, "saving and loading preserves qualified keys");
        app.run_actions(vec![Action::AddToWatchlistSection {
            list: "Crypto".into(),
            symbol: "BINANCE:BTCUSDT".into(),
            section: other_section,
        }]);
        assert_eq!(app.watchlists[crypto].symbols, [key, "BINANCE:BTCUSDT"]);
        assert_eq!(app.watchlists[crypto].items()[3], Item::Symbol("BINANCE:BTCUSDT".into()));
        assert_eq!(app.status, "Added BTC/USDT to Crypto");
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(app.toasts[0].0, "Watchlist: Added BTC/USDT to Crypto");
    }

    #[test]
    fn named_add_rejects_missing_lists_and_stale_sections_without_mutation() {
        use crate::chart_view::WatchlistSectionTarget;
        use crate::watch_sections::Item;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/f02-stale-add");
        let mut app = TerminalApp::test_workspace(&root);
        let before = app.watchlists.clone();
        assert_eq!(
            app.add_to_named_watchlist("Gone", "COINBASE:BTC-USD".into(), None),
            Err("Watchlist Gone no longer exists".into())
        );
        assert_eq!(app.watchlists, before);
        app.create_watchlist("Other".into(), Vec::new()).unwrap();
        let section = WatchlistSectionTarget { index: 0, name: "Deleted".into() };
        app.set_watchlist_items(&[Item::Section { name: "Replacement".into(), collapsed: false }]);
        let before = app.watchlists.clone();
        assert_eq!(
            app.add_to_named_watchlist("Other", "AAPL".into(), Some(&section)),
            Err("Section Deleted in Other no longer exists".into())
        );
        assert_eq!(app.watchlists, before);
        app.delete_watchlist(1).unwrap();
        let before = app.watchlists.clone();
        app.run_actions(vec![Action::AddToNamedWatchlist { list: "Other".into(), symbol: "AAPL".into() }]);
        assert_eq!(app.status, "Watchlist Other no longer exists");
        assert_eq!(app.watchlists, before);
        assert!(app.toasts.is_empty());
    }

    #[test]
    fn named_add_active_path_refreshes_and_keeps_existing_duplicate_and_cap_messages() {
        use crate::chart_view::WatchlistSectionTarget;
        use crate::watch_sections::Item;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/f02-active-add");
        let mut app = TerminalApp::test_workspace(&root);
        app.set_watchlist_items(&[
            Item::Section { name: "First".into(), collapsed: false },
            Item::Section { name: "Last".into(), collapsed: true },
        ]);
        let revision = app.columns_revision;
        let section = WatchlistSectionTarget { index: 0, name: "First".into() };
        app.run_actions(vec![Action::AddToWatchlistSection {
            list: "Main".into(),
            symbol: "COINBASE:BTC-USD".into(),
            section,
        }]);
        assert_eq!(app.watchlist, ["COINBASE:BTC-USD"]);
        assert_eq!(app.watchlists[0].symbols, app.watchlist);
        assert!(app.columns_revision > revision);
        assert_eq!(app.status, "Added 1 symbols to the watchlist");
        assert_eq!(app.watchlists[0].sections[1].at, 1);
        app.run_actions(vec![Action::AddToWatchlist("COINBASE:BTC-USD".into())]);
        assert_eq!(app.status, "All matches are already in the watchlist");
        assert_eq!(app.watchlist.len(), 1);
        let full = (0..200).map(|i| tr_data::security(&format!("QA{i}")).key()).collect();
        app.create_watchlist("Full".into(), full).unwrap();
        let outcome = app.add_to_named_watchlist("Full", "COINBASE:BTC-USD".into(), None).unwrap();
        assert!(!outcome.added && outcome.truncated);
        assert_eq!(outcome.message, "Added 0 symbols to the watchlist (truncated to the 200-symbol watchlist cap)");
        app.switch_watchlist(0);
        let outcome = app.add_to_named_watchlist("Full", "COINBASE:BTC-USD".into(), None).unwrap();
        assert!(!outcome.added && outcome.truncated);
        assert_eq!(outcome.message, "Full is full (200 symbols); BTC/USD was not added");
        assert_eq!(app.watchlists[1].symbols.len(), 200);
    }

    #[test]
    fn bridge_executor_reads_loaded_bars_and_live_tools_from_logic_without_ui() {
        use std::sync::Arc;
        use tr_agent::bridge::{ToolCall, ToolExecutor};
        use tr_agent_tools::{Content, Registry, ToolSet};
        let mut app = agent_fixture("bridge-hidden-logic");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (out, rx) = std::sync::mpsc::channel();
            app.worker.rx = rx;
            let bars = Arc::new(vec![
                tr_core::Bar::new(86400, 1., 2., 0., 1., 10.),
                tr_core::Bar::new(172800, 2., 3., 1., 2.5, 20.),
            ]);
            out.send(Response::History {
                pane: 1,
                generation: 0,
                revision: 1,
                symbol: tr_data::security("AAA").key(),
                tf: Timeframe::D1,
                bars: bars.clone(),
                gpu: tr_chart::scene::SeriesGpu::build(&bars),
                prepended: 0,
                from_cache: false,
                exhausted: false,
                live: false,
                crypto_source: None,
            })
            .unwrap();
            logic_only(&mut app);
            let executor = agent_bridge::AppExecutor::new(&app);
            let pin = agent_bridge::AppExecutor::pin(&app).unwrap();
            let registry = Arc::new(Registry::new(ToolSet::ReadOnly));
            for (name, arguments) in [
                ("read_chart", "{}"),
                ("read_data", r#"{"symbol":"AAA","limit":10}"#),
                ("indicator_values", "{}"),
                ("read_user_actions", "{}"),
            ] {
                let call = ToolCall {
                    turn: "turn".into(),
                    pin: pin.clone(),
                    name: name.into(),
                    arguments: arguments.into(),
                    confirmed: false,
                };
                let mut future = Box::pin(executor.execute(registry.clone(), call));
                let polled = futures::poll!(future.as_mut());
                assert!(polled.is_pending(), "{name}: {polled:?}; pin={pin:?}");
                logic_only(&mut app);
                let result = future.await;
                assert!(result.success, "{name}: {result:?}");
                if name == "read_data" {
                    let Content::Text(text) = &result.content[0] else { panic!("text") };
                    let value: serde_json::Value = serde_json::from_str(text).unwrap();
                    assert_eq!(value["count"], 2);
                    assert_eq!(value["bars"][1][4], 2.5);
                }
            }
        });
    }
}
