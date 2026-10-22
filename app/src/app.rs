//! Top-level terminal: command line, docked panes, saved layouts, hotkeys and status bar.
//!
//! Submodules: `chrome` (toolbar, sidebar, status bar), `workspace` (dock rendering,
//! drag and drop, pop-out windows, link groups, saved layouts), `panels` (per-kind pane
//! content) and `settings_view` (preferences UI).

mod broker;
mod chrome;
mod columns_view;
mod demo;
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
use tr_data::{NewsItem, OrderBook, Quote, SearchHit, YieldCurves};

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
    OrderBook(Box<BookPane>),
    Options(Box<OptionsPane>),
    /// Brokerage accounts through SnapTrade (`PORT`, `ACCT`, `POS`, `ORD`, `ACTV`).
    Portfolio(Box<broker::PortfolioPane>),
    Settings,
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

pub struct NewsPane {
    /// `None` = general market headlines.
    pub symbol: Option<String>,
    pub items: Vec<NewsItem>,
    pub error: Option<String>,
    pub loading: bool,
    pub fetched: Option<Instant>,
}

impl NewsPane {
    pub fn new(symbol: Option<String>) -> Self {
        Self { symbol, items: Vec::new(), error: None, loading: true, fetched: None }
    }
}

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
        let short = |s: &str| tr_data::security(s).ticker;
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
            PaneKind::OrderBook(b) => format!("{} Depth", short(&b.symbol)),
            PaneKind::Options(o) => format!("{} Options", short(&o.symbol)),
            PaneKind::Portfolio(p) => p.title(),
            PaneKind::Settings => "Settings".into(),
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
            PaneKind::OrderBook(_) => "OB",
            PaneKind::Options(_) => "OMON",
            PaneKind::Portfolio(p) => p.view.code(),
            PaneKind::Settings => "PDF",
        }
    }

    /// The security this pane follows, for symbol-driven panels.
    fn symbol(&self) -> Option<&str> {
        match self {
            PaneKind::Chart(c) => Some(&c.symbol),
            PaneKind::Ticker { symbol, .. } => Some(symbol),
            PaneKind::News(n) => n.symbol.as_deref(),
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
                | PaneKind::OrderBook(_)
                | PaneKind::Options(_)
        )
    }

    fn settings(&self) -> PaneSettings {
        match self {
            PaneKind::Chart(c) => PaneSettings::Chart {
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
            PaneKind::News(n) => PaneSettings::News(n.symbol.clone()),
            PaneKind::OrderBook(b) => {
                PaneSettings::OrderBook { symbol: b.symbol.clone(), depth: b.depth, group: b.group }
            }
            PaneKind::Settings => PaneSettings::Settings,
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
            PaneSettings::News(s) => PaneKind::News(Box::new(NewsPane::new(s))),
            PaneSettings::OrderBook { symbol, depth, group } => {
                PaneKind::OrderBook(Box::new(BookPane::new(&symbol, depth, group)))
            }
            PaneSettings::Settings => PaneKind::Settings,
            PaneSettings::Options { symbol, strikes, greeks } => {
                PaneKind::Options(Box::new(OptionsPane::new(&symbol, strikes, greeks)))
            }
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
}

impl NamedWatchlist {
    pub fn new(name: String, symbols: Vec<String>) -> Self {
        Self { name, symbols, sections: Vec::new() }
    }

    /// Rows in display order (section headers and symbols).
    pub fn items(&self) -> Vec<crate::watch_sections::Item> {
        crate::watch_sections::items(&self.symbols, &self.sections)
    }

    pub fn set_items(&mut self, items: &[crate::watch_sections::Item]) {
        (self.symbols, self.sections) = crate::watch_sections::split(items);
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
                        .and_then(|(security, used)| (used == tokens.len()).then(|| security.key()))
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
                        .and_then(|(security, used)| (used == tokens.len()).then(|| security.key()))
                        .map(crate::watch_sections::Item::Symbol)
                }
                section => Some(section),
            })
            .collect::<Vec<_>>();
        let clean = crate::watch_sections::resync(&clean, &symbols);
        let (symbols, sections) = crate::watch_sections::split(&clean);
        let sections = crate::watch_sections::sanitize(&symbols, sections);
        out.push(NamedWatchlist { name, symbols, sections });
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
    last_saved: Vec<u8>,
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

/// Each watchlist is capped at 200 symbols; appending preserves existing order.
fn watchlist_append(current: &[String], symbols: Vec<String>) -> (Vec<String>, usize, bool) {
    let mut next = current.to_vec();
    let mut truncated = false;
    for symbol in symbols {
        let key = tr_data::security(&symbol).key();
        if !next.contains(&key) {
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
            last_saved: Vec::new(),
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
        let mut doc = None;
        if let Some(s) = &settings {
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

    fn save_settings(&self) {
        let s = Settings {
            watchlist: Vec::new(),
            watchlists: self.watchlists.clone(),
            active_watchlist: self.active_watchlist,
            watch_details: self.watch_details,
            sidebar: self.sidebar_open,
            active_layout: Some(self.layout_name.clone()),
            layout: None,
            panes: None,
        };
        if let Ok(json) = serde_json::to_vec_pretty(&s)
            && let Err(e) = crate::layouts::write_atomic(&settings_path(), &json)
        {
            tracing::warn!("saving settings: {e}");
        }
    }

    fn save_prefs(&mut self) {
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

    fn add_watchlist_symbols(&mut self, symbols: Vec<String>) {
        let (next, added, truncated) = watchlist_append(&self.watchlist, symbols);
        self.set_watchlist(next);
        let message = watchlist_add_message(added, truncated);
        self.status = message.clone();
        self.toasts.push((format!("Watchlist: {message}"), Instant::now()));
    }

    /// Add to any saved watchlist. The active list goes through the usual path (undo,
    /// quote refresh); another list is updated in place and saved with the settings.
    fn add_to_named_watchlist(&mut self, name: &str, symbol: String) {
        let Some(index) = self.watchlists.iter().position(|list| list.name == name) else {
            self.status = format!("Watchlist {name} no longer exists");
            return;
        };
        if index == self.active_watchlist {
            return self.add_watchlist_symbols(vec![symbol]);
        }
        let ticker = tr_data::security(&symbol).ticker;
        let (next, added, truncated) = watchlist_append(&self.watchlists[index].symbols, vec![symbol]);
        self.watchlists[index].symbols = next;
        let message = match (added, truncated) {
            (0, true) => format!("{name} is full (200 symbols); {ticker} was not added"),
            (0, false) => format!("{ticker} is already in {name}"),
            _ => format!("Added {ticker} to {name}"),
        };
        self.status = message.clone();
        self.toasts.push((format!("Watchlist: {message}"), Instant::now()));
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
                Action::OpenChart(s) => self.open_security(&s),
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
                Action::AddToWatchlist(s) => self.add_watchlist_symbols(vec![s]),
                Action::AddToNamedWatchlist { list, symbol } => self.add_to_named_watchlist(&list, symbol),
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
            Command::SymbolFunction(s, Function::News) => {
                self.open_panel(PaneKind::News(Box::new(NewsPane::new(Some(s.clone())))));
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
            Function::ClearDrawings => self.with_chart(|c, _| c.drawings.clear()),
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
            Some(PaneKind::Chart(c)) => Some(c),
            _ => None,
        }
    }

    fn drain(&mut self) {
        while let Ok(r) = self.worker.rx.try_recv() {
            match r {
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
                        c.on_history(generation, bars, gpu, prepended, from_cache, exhausted, live, worker);
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
                    if let Some(c) = self.chart_mut(pane) {
                        c.on_indicators(generation, revision, built);
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
                Response::News { pane, symbol, items } => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane)
                        && let PaneKind::News(n) = &mut p.kind
                        && n.symbol == symbol
                    {
                        n.loading = false;
                        match items {
                            Ok(items) => {
                                n.items = items;
                                n.error = None;
                            }
                            Err(e) => n.error = Some(e),
                        }
                    }
                }
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
                Response::CongressRefreshed(summary) => {
                    self.congress_status = summary.to_string();
                    self.congress_checked = Some(chrono::Local::now());
                    self.status = format!("Congress disclosures updated: {summary}");
                    self.refresh_congress_views();
                }
            }
        }
    }

    /// Freshness line for the Congress feed: last check, newest filing, next check.
    fn congress_freshness(&self) -> String {
        let newest = self.latest.iter().map(|t| t.filed_date).max();
        let minutes = std::env::var("TR_CONGRESS_REFRESH_MIN")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(crate::worker::CONGRESS_REFRESH_MIN as i64);
        let mut parts = Vec::new();
        match self.congress_checked {
            Some(t) => {
                parts.push(format!("checked {}", t.format("%H:%M")));
                if minutes > 0 {
                    parts.push(format!("next {}", (t + chrono::Duration::minutes(minutes)).format("%H:%M")));
                }
            }
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
        let presses: Vec<(Key, Modifiers)> = ctx.input(|i| {
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

    fn periodic(&mut self) {
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
                PaneKind::News(n) if n.fetched.is_none_or(|t| t.elapsed() > Duration::from_secs(300)) => {
                    n.fetched = Some(Instant::now());
                    self.worker.send(Request::News { pane: p.id, symbol: n.symbol.clone() });
                }
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

    /// Save the active layout (only when it changed) and app settings.
    fn autosave(&mut self) {
        self.save_dirty_themes(true);
        let json = self.current_doc().to_json();
        if json != self.last_saved {
            self.save_layout();
        }
        self.save_settings();
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

impl eframe::App for TerminalApp {
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
        } else if !self.updater_started {
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
        self.drain();
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
        if let Some(secs) = std::env::var("TR_SMOKE_SECS").ok().and_then(|v| v.parse::<f64>().ok()) {
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
        self.autosave();
        self.price_alerts.save();
        if self.demo.save_error.is_some() {
            self.demo.save();
        }
    }
}

#[cfg(test)]
impl TerminalApp {
    /// Inert workspace with all disk-backed dependencies confined to the fixture.
    fn test_workspace(root: &std::path::Path) -> Self {
        Self {
            broker: broker::BrokerState::with_store(
                tr_broker::secrets::SecretStore::file(root.join("secrets.json")),
                true,
            ),
            trading: Default::default(),
            demo: demo::DemoState::load(root.join("demo.json")).0,
            worker: Worker::inert(),
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
            last_saved: Vec::new(),
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
        assert!(app.status.contains("Added SOLUSD to Crypto"), "{}", app.status);
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
}
