//! Background worker. All network, SQLite, indicator and LOD-pyramid work runs on a
//! tokio runtime; the UI thread only sends [`Request`]s and drains [`Response`]s, so it
//! never blocks.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use tokio::sync::Mutex;
use tr_chart::gpu::{LineKind, LineRef};
use tr_chart::scene::{Overlay, SeriesGpu, line_ref, marker_ref};
use tr_congress::db::{Alert, PoliticianSummary};
use tr_congress::performance::{self, Performance, TradeReturn};
use tr_congress::refresh::{self, HttpSource, NewTradeAlert, RefreshConfig};
use tr_congress::{Db, Politician, Trade};
use tr_core::indicators::registry::{Input, Plot, Tone};
use tr_core::scanner::{ScanDefinition, ScanEvaluation};
use tr_core::{Bar, Security, Series, Timeframe};
use tr_data::scanstore::{Fetch, ScanStore};
use tr_data::universe::Universe;
use tr_data::{BarCache, Provider, Quote, SearchHit, YieldCurves};

use crate::command::IndicatorSpec;
use crate::theme;

pub type PaneId = u64;

/// Minutes between Congress disclosure checks (`TR_CONGRESS_REFRESH_MIN`; 0 disables).
pub const CONGRESS_REFRESH_MIN: u64 = 30;

/// Bounded, on-demand scanner universe. Larger universes need a bulk data provider.
pub const MAX_SCAN_SYMBOLS: usize = 200;

#[derive(Debug, Clone)]
pub struct ScanRow {
    pub symbol: String,
    pub evaluation: Option<ScanEvaluation>,
    /// Most recent evaluated bar's open time; providers may supply delayed data.
    pub ts: Option<i64>,
    pub simulated: bool,
    pub error: Option<String>,
}

/// Settings › Brokerage actions on the optional Questrade market-data source.
pub enum QuestradeAction {
    /// Save (or with `None`, forget) the refresh token, then test it.
    Save(Option<String>),
    Test,
}

pub enum Request {
    Questrade {
        handle: tr_data::questrade::Questrade,
        action: QuestradeAction,
    },
    Scan {
        pane: PaneId,
        generation: u64,
        definition: ScanDefinition,
        symbols: Vec<String>,
    },
    /// Scan a market-wide universe through the scanner's bar store; `cancel` stops it.
    ScanUniverse {
        pane: PaneId,
        generation: u64,
        definition: ScanDefinition,
        universe: Universe,
        cancel: Arc<std::sync::atomic::AtomicBool>,
    },
    /// Load the brokerage portfolio through SnapTrade (`None` = the simulated sample).
    Portfolio(Option<tr_broker::Credentials>),
    /// Register a SnapTrade user under the partner keys.
    BrokerRegister {
        creds: tr_broker::Credentials,
        user_id: String,
    },
    /// Generate a read-only Connection Portal link (optionally to reconnect a connection).
    BrokerPortal {
        creds: tr_broker::Credentials,
        dark: bool,
        reconnect: Option<String>,
    },
    /// Remove a brokerage connection at the user's explicit request.
    BrokerDisconnect {
        creds: tr_broker::Credentials,
        id: String,
    },
    /// Option chain for one expiration (`None` = nearest).
    Options {
        pane: PaneId,
        symbol: String,
        expiration: Option<i64>,
    },
    /// Headlines for a market key (`None` = general market news).
    News {
        pane: PaneId,
        symbol: Option<String>,
    },
    OrderBook {
        pane: PaneId,
        symbol: String,
        depth: usize,
    },
    History {
        pane: PaneId,
        generation: u64,
        symbol: String,
        tf: Timeframe,
        limit: usize,
    },
    /// Prepend older history to `existing`.
    MoreHistory {
        pane: PaneId,
        generation: u64,
        symbol: String,
        tf: Timeframe,
        existing: Arc<Vec<Bar>>,
        limit: usize,
    },
    Indicators {
        revision: u64,
        pane: PaneId,
        generation: u64,
        bars: Arc<Vec<Bar>>,
        tf: Timeframe,
        specs: Vec<IndicatorSpec>,
        /// Colours are baked into overlays, so the UI passes its current palette.
        palette: theme::Palette,
        session: tr_core::session::Session,
    },
    Compare {
        revision: u64,
        pane: PaneId,
        generation: u64,
        symbol: String,
        tf: Timeframe,
        base: Arc<Vec<Bar>>,
    },
    CompareSearch {
        pane: PaneId,
        query: String,
    },
    Heikin {
        pane: PaneId,
        generation: u64,
        bars: Arc<Vec<Bar>>,
    },
    Markers {
        pane: PaneId,
        symbol: String,
    },
    Quotes(Vec<String>),
    PriceAlertQuotes {
        revision: u64,
        symbols: Vec<String>,
    },
    PriceAlertRules {
        revision: u64,
        alerts: Vec<tr_core::price_alerts::Alert>,
    },
    Latest {
        limit: usize,
    },
    TickerTrades {
        pane: PaneId,
        symbol: String,
    },
    Profile {
        pane: PaneId,
        query: String,
    },
    SearchPoliticians(String),
    Track {
        query: String,
        on: bool,
    },
    Alerts,
    MarkAlertsSeen,
    RefreshCongress,
    /// A pane closed or changed: stop its live feed and drop its series.
    Forget(PaneId),
    YieldCurves,
    Search(String),
    /// Order entry (see `app::trading`); only sent once trading is enabled.
    Trade(TradeRequest),
}

/// A trading call through SnapTrade. Previews never place anything; placing takes a preview
/// the user confirmed.
pub enum TradeRequest {
    Preview {
        creds: tr_broker::Credentials,
        req: tr_broker::trading::OrderRequest,
        ticket: u64,
    },
    Place {
        creds: tr_broker::Credentials,
        preview: Box<tr_broker::trading::OrderPreview>,
        ticket: u64,
    },
    Cancel {
        creds: tr_broker::Credentials,
        account: String,
        order: String,
    },
    /// Connection Portal link requesting trading access.
    Portal {
        creds: tr_broker::Credentials,
        dark: bool,
    },
    /// Which brokerage connections can trade (brokerage support and connection access): one
    /// `/authorizations` request, however many accounts there are.
    Capabilities {
        creds: tr_broker::Credentials,
    },
}

pub enum TradeResponse {
    Preview {
        ticket: u64,
        result: Result<Box<tr_broker::trading::OrderPreview>, String>,
    },
    Placed {
        ticket: u64,
        preview: Box<tr_broker::trading::OrderPreview>,
        result: Result<Option<Box<tr_broker::Order>>, tr_broker::trading::PlaceError>,
    },
    Cancelled {
        account: String,
        order: String,
        result: Result<(), String>,
    },
    Portal(Result<String, String>),
    /// Capabilities by connection (SnapTrade authorization) id.
    Capabilities(Result<std::collections::HashMap<String, tr_broker::trading::Capability>, CapsError>),
}

/// Why trading access could not be read. A rate limit is reported apart from real errors so
/// the app can back off quietly instead of showing an error.
#[derive(Debug, Clone, PartialEq)]
pub struct CapsError {
    pub message: String,
    pub throttled: bool,
    pub retry_after: Option<Duration>,
}

pub struct BuiltIndicator {
    pub spec: IndicatorSpec,
    pub overlays: Vec<(String, Overlay)>,
    pub error: Option<String>,
    pub fill: Option<crate::chart_fill::Fill>,
    pub ports: Vec<String>,
    /// Transparent geometry keeps hidden band boundaries in autoscale without a legend entry.
    pub bounds: Vec<Overlay>,
}

pub struct Profile {
    pub politician: Politician,
    pub trades: Vec<Trade>,
    pub perf: Performance,
    pub returns: Vec<TradeReturn>,
    pub tracked: bool,
}

pub enum Response {
    Heikin {
        pane: PaneId,
        generation: u64,
        bars: Arc<Vec<Bar>>,
        gpu: SeriesGpu,
    },
    /// Outcome of a [`QuestradeAction`]: a status line or an error.
    Questrade(Result<String, String>),
    Scan {
        pane: PaneId,
        generation: u64,
        rows: Vec<ScanRow>,
    },
    /// Symbols evaluated so far in a universe scan.
    ScanProgress {
        pane: PaneId,
        generation: u64,
        done: usize,
        total: usize,
    },
    Portfolio(Result<Box<tr_broker::Portfolio>, String>),
    /// `(userId, userSecret)` for a newly registered SnapTrade user.
    BrokerRegistered(Result<(String, String), String>),
    BrokerPortal(Result<String, String>),
    /// The connection id removed, or why not.
    BrokerDisconnected(Result<String, String>),
    Options {
        pane: PaneId,
        symbol: String,
        chain: Result<Box<tr_data::OptionChain>, String>,
    },
    News {
        pane: PaneId,
        symbol: Option<String>,
        items: Result<Vec<tr_data::NewsItem>, String>,
    },
    OrderBook {
        pane: PaneId,
        symbol: String,
        book: Result<tr_data::OrderBook, String>,
    },
    History {
        pane: PaneId,
        generation: u64,
        symbol: String,
        tf: Timeframe,
        bars: Arc<Vec<Bar>>,
        gpu: SeriesGpu,
        /// Number of bars prepended (paging), 0 otherwise.
        prepended: usize,
        from_cache: bool,
        exhausted: bool,
        /// A live-feed update of the latest bar.
        live: bool,
    },
    HistoryError {
        pane: PaneId,
        generation: u64,
        error: String,
    },
    Indicators {
        revision: u64,
        pane: PaneId,
        generation: u64,
        built: Vec<BuiltIndicator>,
    },
    Compare {
        revision: u64,
        pane: PaneId,
        generation: u64,
        symbol: String,
        line: LineRef,
    },
    CompareError {
        pane: PaneId,
        generation: u64,
        revision: u64,
        symbol: String,
        error: String,
    },
    CompareSearch {
        pane: PaneId,
        query: String,
        result: Result<Vec<SearchHit>, String>,
    },
    Markers {
        pane: PaneId,
        symbol: String,
        trades: Vec<Trade>,
    },
    Quotes(Vec<Quote>),
    Latest(Vec<Trade>),
    TickerTrades {
        pane: PaneId,
        symbol: String,
        trades: Vec<Trade>,
    },
    Profile {
        pane: PaneId,
        profile: Option<Box<Profile>>,
        query: String,
    },
    Politicians(Vec<PoliticianSummary>),
    Alerts(Vec<Alert>),
    NewAlerts(Vec<NewTradeAlert>),
    PriceAlertQuotes {
        revision: u64,
        observations: Vec<(String, Result<tr_core::price_alerts::Observation, String>)>,
    },
    PriceAlertRules {
        revision: u64,
        observations: Vec<(u64, Result<tr_core::price_alerts::Observation, String>)>,
    },
    Status(String),
    CongressRefreshed(String),
    Curves(Box<YieldCurves>),
    SearchHits {
        query: String,
        hits: Vec<SearchHit>,
    },
    Trade(Box<TradeResponse>),
}

pub struct Worker {
    tx: tokio::sync::mpsc::UnboundedSender<Request>,
    pub rx: mpsc::Receiver<Response>,
    _rt: tokio::runtime::Runtime,
}

/// Bars per pane tagged with the load generation they belong to.
type SeriesMap = HashMap<PaneId, (u64, Arc<Vec<Bar>>, String)>;

struct Shared {
    provider: Provider,
    /// Authoritative bars per chart pane (history, paging and live ticks all merge here),
    /// tagged with the pane's load generation.
    series: std::sync::Mutex<SeriesMap>,
    cache: std::sync::Mutex<Option<BarCache>>,
    scan_store: std::sync::Mutex<Option<ScanStore>>,
    db: Mutex<Db>,
    out: mpsc::Sender<Response>,
    ctx: egui::Context,
}

impl Shared {
    fn send(&self, r: Response) {
        let _ = self.out.send(r);
        self.ctx.request_repaint();
    }
}

pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("TR_DATA_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/terminal-rs")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("terminal-rs")
    }
}

impl Worker {
    #[cfg(test)]
    pub fn inert() -> Self {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let (_, rx) = mpsc::channel();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        Self { tx, rx, _rt: rt }
    }

    pub fn spawn(ctx: egui::Context, provider: Provider) -> anyhow::Result<Self> {
        let dir = data_dir();
        std::fs::create_dir_all(&dir)?;
        let db = Db::open(dir.join("congress.sqlite3")).or_else(|e| {
            tracing::warn!("congress db unavailable ({e:#}); using in-memory store");
            Db::open_in_memory()
        })?;
        let cache =
            BarCache::open(dir.join("bars.sqlite3")).map_err(|e| tracing::warn!("bar cache unavailable: {e:#}")).ok();
        let scan_store = ScanStore::open(dir.join("scan_bars.sqlite3"))
            .map_err(|e| tracing::warn!("scan store unavailable: {e:#}"))
            .ok();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .thread_name("tr-worker")
            .build()?;
        let (tx, mut req_rx) = tokio::sync::mpsc::unbounded_channel::<Request>();
        let (out, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            provider,
            series: Default::default(),
            cache: std::sync::Mutex::new(cache),
            scan_store: std::sync::Mutex::new(scan_store),
            db: Mutex::new(db),
            out,
            ctx,
        });

        // Request dispatcher: every request runs as its own task.
        let s = shared.clone();
        rt.spawn(async move {
            while let Some(req) = req_rx.recv().await {
                let s = s.clone();
                tokio::spawn(async move { handle(&s, req).await });
            }
        });

        // Scheduled congressional disclosure refresh.
        let s = shared.clone();
        rt.spawn(async move {
            let minutes: u64 = std::env::var("TR_CONGRESS_REFRESH_MIN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(CONGRESS_REFRESH_MIN);
            if minutes == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
            let mut tick = tokio::time::interval(Duration::from_secs(minutes * 60));
            loop {
                tick.tick().await;
                congress_refresh(&s).await;
            }
        });
        Ok(Self { tx, rx, _rt: rt })
    }

    pub fn send(&self, req: Request) {
        let _ = self.tx.send(req);
    }
}

async fn congress_refresh(s: &Shared) {
    s.send(Response::Status("Refreshing congressional disclosures (Senate eFD + House Clerk)…".into()));
    let source = match HttpSource::new() {
        Ok(src) => src,
        Err(e) => return s.send(Response::Status(format!("congress refresh failed: {e:#}"))),
    };
    let cfg = RefreshConfig::incremental(&*s.db.lock().await);
    match refresh::refresh(&source, &s.db, &cfg).await {
        Ok(report) => {
            if !report.alerts.is_empty() {
                s.send(Response::NewAlerts(report.alerts.clone()));
            }
            s.send(Response::CongressRefreshed(report.summary()));
        }
        Err(e) => s.send(Response::Status(format!("congress refresh failed: {e:#}"))),
    }
}

fn overlay(values: &[f64], color: egui::Color32, kind: LineKind) -> Overlay {
    let line = if kind == LineKind::Dots { marker_ref(values) } else { line_ref(values) };
    Overlay { line, color: theme::rgba(color), kind, scale: 1.0, width: 1.3, histogram_color: None }
}

/// Compute every indicator on `bars`. The `i`-th indicator takes the `i`-th series colour;
/// its signal lines, outer bands and up/down lines take theme roles.
#[cfg(test)]
pub fn build_indicators(
    bars: &[Bar],
    tf: Timeframe,
    specs: &[IndicatorSpec],
    palette: &theme::Palette,
) -> Vec<BuiltIndicator> {
    build_indicators_session(bars, tf, specs, palette, tr_core::session::Session::Provider)
}

fn build_indicators_session(
    bars: &[Bar],
    tf: Timeframe,
    specs: &[IndicatorSpec],
    palette: &theme::Palette,
    session: tr_core::session::Session,
) -> Vec<BuiltIndicator> {
    let input = Input::new(bars, tf);
    let colors = palette.series.len();
    let calculated = tr_core::indicators::study::evaluate_with_session(specs, &input, session);
    specs
        .iter()
        .zip(calculated)
        .enumerate()
        .map(|(i, (spec, result))| {
            let i = spec.chart.as_ref().map_or(i, |c| c.id.saturating_sub(1) as usize);
            let main = spec
                .chart
                .as_ref()
                .and_then(|c| c.color)
                .map_or(palette.series[i % colors], |[r, g, b]| egui::Color32::from_rgb(r, g, b));
            let error = result.as_ref().err().cloned();
            let mut outputs = result.unwrap_or_default();
            let ports = outputs.iter().map(|o| o.name.to_string()).collect();
            let fill = spec.chart.as_ref().and_then(|c| crate::chart_fill::Fill::build(c, &outputs));
            let bounds = outputs
                .iter()
                .filter(|o| {
                    spec.chart.as_ref().is_some_and(|c| {
                        c.fill.as_ref().is_some_and(|fill| o.name == fill.upper || o.name == fill.lower)
                            && c.outputs.get(o.name).is_some_and(|style| !style.visible)
                    })
                })
                .map(|o| {
                    overlay(
                        &tr_core::indicators::formula::display_values(
                            &o.values,
                            spec.chart.as_ref().map_or(0, |c| c.displacement),
                        ),
                        egui::Color32::TRANSPARENT,
                        LineKind::Line,
                    )
                })
                .collect();
            // Columns first, so lines and markers draw over them.
            outputs.sort_by_key(|o| {
                let custom = spec.chart.as_ref().and_then(|c| c.outputs.get(o.name));
                !custom
                    .and_then(|c| c.plot)
                    .map_or(o.plot == Plot::Hist, |p| p == tr_core::indicators::study::PlotStyle::Histogram)
            });
            let overlays = outputs
                .into_iter()
                .filter_map(|o| {
                    use tr_core::indicators::study::PlotStyle;
                    let custom = spec.chart.as_ref().and_then(|c| c.outputs.get(o.name));
                    if custom.is_some_and(|c| !c.visible) {
                        return None;
                    }
                    let color = match o.tone {
                        Tone::Main => main,
                        Tone::Signal if main == palette.amber => palette.cyan,
                        Tone::Signal => palette.amber,
                        Tone::Dim => main.gamma_multiply(0.6),
                        Tone::Up => palette.up,
                        Tone::Down => palette.down,
                        Tone::Series(k) => palette.series[(i + 1 + k as usize) % colors],
                    };
                    let color =
                        custom.and_then(|c| c.color).map_or(color, |[r, g, b]| egui::Color32::from_rgb(r, g, b));
                    let kind = match custom.and_then(|c| c.plot) {
                        Some(PlotStyle::Line) => LineKind::Line,
                        Some(PlotStyle::Dots) => LineKind::Dots,
                        Some(PlotStyle::Steps) => LineKind::Steps,
                        Some(PlotStyle::Histogram) => LineKind::Hist,
                        None => match o.plot {
                            Plot::Line => LineKind::Line,
                            Plot::Hist => LineKind::Hist,
                            Plot::Dots => LineKind::Dots,
                            Plot::Steps => LineKind::Steps,
                        },
                    };
                    let values = tr_core::indicators::formula::display_values(
                        &o.values,
                        spec.chart.as_ref().map_or(0, |c| c.displacement),
                    );
                    let mut overlay = overlay(&values, color, kind);
                    overlay.width = custom.map_or(1.3, |c| c.width);
                    overlay.histogram_color = custom.and_then(|c| c.color).map(|_| theme::rgba(color));
                    Some((o.name.to_string(), overlay))
                })
                .collect();
            BuiltIndicator { spec: spec.clone(), overlays, error, fill, ports, bounds }
        })
        .collect()
}

/// Align `other`'s closes to `base` timestamps (last close at or before each bar). The
/// chart rebases the result to the first visible bar every frame.
pub fn align_compare(base: &[Bar], other: &[Bar]) -> Vec<f64> {
    let mut out = vec![f64::NAN; base.len()];
    let mut j = 0;
    for (i, b) in base.iter().enumerate() {
        while j + 1 < other.len() && other[j + 1].ts <= b.ts {
            j += 1;
        }
        if let Some(o) = other.get(j).filter(|o| o.ts <= b.ts) {
            out[i] = o.close;
        }
    }
    out
}

fn default_limit(tf: Timeframe) -> usize {
    match tf {
        Timeframe::M1 => 5_000,
        Timeframe::M5 | Timeframe::M15 | Timeframe::M30 => 5_000,
        Timeframe::H1 | Timeframe::H4 => 4_000,
        Timeframe::D1 => 6_000,
        Timeframe::W1 | Timeframe::MN1 => 2_000,
    }
}

/// Bars requested on first load; `TR_HISTORY_LIMIT` overrides (e.g. 1000000 with sample data).
pub fn initial_limit(tf: Timeframe) -> usize {
    std::env::var("TR_HISTORY_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| default_limit(tf))
}

async fn handle(s: &Arc<Shared>, req: Request) {
    match req {
        Request::Questrade { handle, action } => {
            let test = async |h: &tr_data::questrade::Questrade| {
                let bar = h.test("AAPL").await?;
                let day = chrono::DateTime::from_timestamp(bar.ts, 0)
                    .map_or_else(String::new, |d| d.format("%Y-%m-%d").to_string());
                anyhow::Ok(format!("Questrade connected · AAPL {day} close {:.2}", bar.close))
            };
            let result = match action {
                QuestradeAction::Save(None) => {
                    handle.set_refresh_token(None).await.map(|()| "Questrade token removed".to_string())
                }
                QuestradeAction::Save(token) => match handle.set_refresh_token(token).await {
                    Ok(()) => test(&handle).await,
                    Err(e) => Err(e.context("saving the Questrade token")),
                },
                QuestradeAction::Test => test(&handle).await,
            };
            s.send(Response::Questrade(result.map_err(|e| format!("{e:#}"))));
        }
        Request::ScanUniverse { pane, generation, definition, universe, cancel } => {
            let rows = scan_universe(s, pane, generation, &definition, universe, &cancel).await;
            s.send(Response::Scan { pane, generation, rows });
        }
        Request::Scan { pane, generation, definition, symbols } => {
            let rows = scan(s.provider.clone(), definition, symbols, Duration::from_secs(15)).await;
            s.send(Response::Scan { pane, generation, rows });
        }
        Request::History { pane, generation, symbol, tf, limit } => {
            let refreshing = current_series(s, pane, generation).is_some_and(|bars| !bars.is_empty());
            let source = chart_cache_key(s.provider.as_ref(), &symbol);
            if !claim_series(s, pane, generation, &source) {
                if current_series(s, pane, generation).is_some() {
                    s.send(Response::HistoryError {
                        pane,
                        generation,
                        error: "Data route changed; reload the chart".into(),
                    });
                }
                return; // a newer load for this pane is already running
            }
            // Paint from the disk cache immediately, then refresh from the network.
            let cached = if refreshing {
                None
            } else {
                s.cache.lock().ok().and_then(|c| c.as_ref().and_then(|c| c.load(&source, tf, limit).ok()))
            };
            if let Some(bars) = cached.filter(|b| !b.is_empty()) {
                let bars = Arc::new(bars);
                let gpu = build_gpu(bars.clone()).await;
                s.send(Response::History {
                    pane,
                    generation,
                    symbol: symbol.clone(),
                    tf,
                    bars,
                    gpu,
                    prepended: 0,
                    from_cache: true,
                    exhausted: false,
                    live: false,
                });
            }
            match s.provider.history(&symbol, tf, None, limit).await {
                Ok(bars) if !bars.is_empty() => {
                    if chart_cache_key(s.provider.as_ref(), &symbol) != source {
                        s.send(Response::HistoryError {
                            pane,
                            generation,
                            error: "Data route changed; reload the chart".into(),
                        });
                        return;
                    }
                    if let Ok(mut c) = s.cache.lock()
                        && let Some(c) = c.as_mut()
                    {
                        let _ = c.store(&source, tf, &bars);
                    }
                    let exhausted = bars.len() < limit / 2;
                    let bars = Arc::new(bars);
                    if !store_series(s, pane, generation, bars.clone()) {
                        return; // superseded while loading
                    }
                    let gpu = build_gpu(bars.clone()).await;
                    s.send(Response::History {
                        pane,
                        generation,
                        symbol: symbol.clone(),
                        tf,
                        bars,
                        gpu,
                        prepended: 0,
                        from_cache: false,
                        exhausted,
                        live: false,
                    });
                    if !refreshing {
                        start_live(s.clone(), pane, generation, symbol, tf);
                    }
                }
                Ok(_) => s.send(Response::HistoryError { pane, generation, error: format!("no data for {symbol}") }),
                Err(e) => s.send(Response::HistoryError { pane, generation, error: format!("{e:#}") }),
            }
        }
        Request::MoreHistory { pane, generation, symbol, tf, existing, limit } => {
            let source = chart_cache_key(s.provider.as_ref(), &symbol);
            let previous = s
                .series
                .lock()
                .expect("series lock")
                .get(&pane)
                .filter(|(g, _, _)| *g == generation)
                .map(|(_, _, source)| source.clone());
            if previous.as_ref().is_some_and(|previous| previous != &source) {
                s.send(Response::HistoryError {
                    pane,
                    generation,
                    error: "Data route changed; reload before paging".into(),
                });
                return;
            }
            // Page from the worker's copy (it may hold newer live bars than the UI's).
            let base = current_series(s, pane, generation).filter(|b| !b.is_empty()).unwrap_or(existing);
            let Some(first) = base.first().copied() else { return };
            match s.provider.history(&symbol, tf, Some(first.ts), limit).await {
                Ok(older) => {
                    if chart_cache_key(s.provider.as_ref(), &symbol) != source {
                        s.send(Response::HistoryError {
                            pane,
                            generation,
                            error: "Data route changed while paging; reload the chart".into(),
                        });
                        return;
                    }
                    let older: Vec<Bar> = older.into_iter().filter(|b| b.ts < first.ts).collect();
                    let prepended = older.len();
                    let bars = {
                        let mut map = s.series.lock().expect("series lock");
                        let Some((g, cur, _)) = map.get_mut(&pane).filter(|(g, _, _)| *g == generation) else { return };
                        let _ = g;
                        let mut merged = older;
                        merged.extend_from_slice(cur);
                        *cur = Arc::new(merged);
                        cur.clone()
                    };
                    let gpu = build_gpu(bars.clone()).await;
                    s.send(Response::History {
                        pane,
                        generation,
                        symbol,
                        tf,
                        bars,
                        gpu,
                        prepended,
                        from_cache: false,
                        exhausted: prepended == 0,
                        live: false,
                    });
                }
                Err(e) => s.send(Response::HistoryError { pane, generation, error: format!("{e:#}") }),
            }
        }
        Request::Indicators { pane, generation, bars, tf, specs, palette, session, revision } => {
            let built =
                tokio::task::spawn_blocking(move || build_indicators_session(&bars, tf, &specs, &palette, session))
                    .await
                    .unwrap_or_default();
            s.send(Response::Indicators { pane, generation, built, revision });
        }
        Request::Compare { pane, generation, symbol, tf, base, revision } => {
            let limit = base.len().max(100);
            match s.provider.history(&symbol, tf, None, limit).await {
                Ok(other) => {
                    let line = tokio::task::spawn_blocking(move || {
                        let values = align_compare(&base, &other);
                        values.iter().any(|v| v.is_finite()).then(|| line_ref(&values))
                    })
                    .await;
                    if let Ok(Some(line)) = line {
                        s.send(Response::Compare { pane, generation, symbol, line, revision });
                    } else {
                        s.send(Response::CompareError {
                            pane,
                            generation,
                            revision,
                            symbol,
                            error: "No overlapping comparison history is available".into(),
                        });
                    }
                }
                Err(e) => {
                    s.send(Response::CompareError { pane, generation, revision, symbol, error: format!("{e:#}") })
                }
            }
        }
        Request::CompareSearch { pane, query } => {
            let result = s.provider.search(&query).await.map_err(|e| format!("{e:#}"));
            s.send(Response::CompareSearch { pane, query, result });
        }
        Request::Markers { pane, symbol } => {
            let sec = tr_data::security(&symbol);
            let trades = if sec.has_congress_trades() {
                s.db.lock().await.trades_for_ticker(&sec.ticker, 2_000).unwrap_or_default()
            } else {
                Vec::new()
            };
            s.send(Response::Markers { pane, symbol, trades });
        }
        Request::PriceAlertQuotes { revision, symbols } => {
            use futures::{StreamExt, stream};
            let observations =
                stream::iter(symbols)
                    .map(|symbol| async move {
                        let result =
                            match tokio::time::timeout(Duration::from_secs(20), s.provider.alert_observation(&symbol))
                                .await
                            {
                                Ok(result) => result.map_err(|e| format!("{e:#}")),
                                Err(_) => Err("Price request timed out; will retry".into()),
                            };
                        (symbol, result)
                    })
                    .buffer_unordered(4)
                    .collect()
                    .await;
            s.send(Response::PriceAlertQuotes { revision, observations });
        }
        Request::PriceAlertRules { revision, alerts } => {
            use futures::{StreamExt, stream};
            let observations = stream::iter(alerts)
                .map(|alert| async move {
                    let result = tokio::time::timeout(Duration::from_secs(20), async {
                        let rule = alert.rule.as_ref().ok_or("Missing condition")?;
                        let mut q = s.provider.alert_observation(&alert.symbol).await.map_err(|e| e.to_string())?;
                        if !q.valid_at(chrono::Utc::now().timestamp()) {
                            return Err("Stale price observation".into());
                        }
                        let tf = match rule {
                            tr_core::price_alerts::Rule::Formula { timeframe, .. } => *timeframe,
                            _ => Timeframe::M1,
                        };
                        let history =
                            s.provider.scan_history(&alert.symbol, tf, 1500).await.map_err(|e| e.to_string())?;
                        if history.simulated != q.simulated {
                            return Err("Condition and price data modes differ".into());
                        }
                        let last = history.bars.last().ok_or("No condition history")?;
                        if last.ts > q.ts + 30 || q.ts.saturating_sub(last.ts) > tf.seconds() + 120 {
                            return Err("Condition history is stale relative to price".into());
                        }
                        q.price = rule.value(&history.bars)?;
                        q.source = format!("{} · {}", q.source, rule.label().chars().take(200).collect::<String>());
                        Ok(q)
                    })
                    .await
                    .unwrap_or_else(|_| Err("Condition request timed out".into()));
                    (alert.id, result)
                })
                .buffer_unordered(4)
                .collect()
                .await;
            s.send(Response::PriceAlertRules { revision, observations });
        }
        Request::Heikin { pane, generation, bars } => {
            let input = bars.clone();
            if let Ok(gpu) =
                tokio::task::spawn_blocking(move || SeriesGpu::build(&tr_core::chart_analytics::heikin_ashi(&input)))
                    .await
            {
                s.send(Response::Heikin { pane, generation, bars, gpu });
            }
        }
        Request::Quotes(symbols) => match s.provider.quotes(&symbols).await {
            Ok(q) => s.send(Response::Quotes(q)),
            Err(e) => s.send(Response::Status(format!("quotes: {e:#}"))),
        },
        Request::Latest { limit } => {
            let t = s.db.lock().await.latest_trades(limit).unwrap_or_default();
            s.send(Response::Latest(t));
        }
        Request::TickerTrades { pane, symbol } => {
            let ticker = Security::parse(&symbol).map(|x| x.ticker).unwrap_or_else(|| symbol.clone());
            let trades = s.db.lock().await.trades_for_ticker(&ticker, 1_000).unwrap_or_default();
            s.send(Response::TickerTrades { pane, symbol, trades });
        }
        Request::Profile { pane, query } => {
            let profile = profile(s, &query).await;
            s.send(Response::Profile { pane, profile: profile.map(Box::new), query });
        }
        Request::SearchPoliticians(q) => {
            let r = s.db.lock().await.search_politicians(&q, 500).unwrap_or_default();
            s.send(Response::Politicians(r));
        }
        Request::Track { query, on } => {
            let db = s.db.lock().await;
            let hit = resolve_politician(&db, &query);
            match hit {
                Some(p) => {
                    let _ = db.set_tracked(p.id, on);
                    let verb = if on { "Tracking" } else { "Stopped tracking" };
                    s.send(Response::Status(format!("{verb} {} — alerts fire when they file a new PTR", p.name)));
                }
                None => s.send(Response::Status(format!("No politician matches '{query}' yet (run REFRESH)"))),
            }
            let r = db.search_politicians("", 500).unwrap_or_default();
            s.send(Response::Politicians(r));
        }
        Request::Alerts => {
            let a = s.db.lock().await.alerts(false, 200).unwrap_or_default();
            s.send(Response::Alerts(a));
        }
        Request::MarkAlertsSeen => {
            let db = s.db.lock().await;
            let _ = db.mark_alerts_seen();
            s.send(Response::Alerts(db.alerts(false, 200).unwrap_or_default()));
        }
        Request::RefreshCongress => congress_refresh(s).await,
        Request::Forget(pane) => {
            s.series.lock().expect("series lock").remove(&pane);
        }
        Request::YieldCurves => match s.provider.yield_curves().await {
            Ok(c) => s.send(Response::Curves(Box::new(c))),
            Err(e) => s.send(Response::Status(format!("yield curve: {e:#}"))),
        },
        Request::Portfolio(creds) => {
            let result = match creds {
                None => Ok(tr_broker::sample::portfolio()),
                Some(c) => match tr_broker::SnapTrade::new(c) {
                    Ok(client) => client.portfolio().await,
                    Err(e) => Err(e),
                },
            };
            s.send(Response::Portfolio(result.map(Box::new).map_err(|e| format!("{e:#}"))));
        }
        Request::BrokerRegister { creds, user_id } => {
            let result = match tr_broker::SnapTrade::new(creds) {
                Ok(client) => client.register_user(&user_id).await,
                Err(e) => Err(e),
            };
            s.send(Response::BrokerRegistered(result.map_err(|e| format!("{e:#}"))));
        }
        Request::BrokerPortal { creds, dark, reconnect } => {
            let result = match tr_broker::SnapTrade::new(creds) {
                Ok(client) => client.connection_portal_url(dark, reconnect.as_deref()).await,
                Err(e) => Err(e),
            };
            s.send(Response::BrokerPortal(result.map_err(|e| format!("{e:#}"))));
        }
        Request::BrokerDisconnect { creds, id } => {
            let result = match tr_broker::SnapTrade::new(creds) {
                Ok(client) => client.delete_connection(&id).await,
                Err(e) => Err(e),
            };
            s.send(Response::BrokerDisconnected(result.map(|()| id).map_err(|e| format!("{e:#}"))));
        }
        Request::Options { pane, symbol, expiration } => {
            let chain = s.provider.options(&symbol, expiration).await.map(Box::new).map_err(|e| format!("{e:#}"));
            s.send(Response::Options { pane, symbol, chain });
        }
        Request::News { pane, symbol } => {
            let items = s.provider.news(symbol.as_deref()).await.map_err(|e| format!("{e:#}"));
            s.send(Response::News { pane, symbol, items });
        }
        Request::OrderBook { pane, symbol, depth } => {
            let book = s.provider.order_book(&symbol, depth).await.map_err(|e| format!("{e:#}"));
            s.send(Response::OrderBook { pane, symbol, book });
        }
        Request::Search(query) => {
            let hits = s.provider.search(&query).await.unwrap_or_default();
            s.send(Response::SearchHits { query, hits });
        }
        Request::Trade(t) => s.send(Response::Trade(Box::new(trade(t).await))),
    }
}

/// Fetch a snapshot directly from the provider, independently of chart caches/live feeds.
/// Input order is retained even when fetches complete out of order.
async fn scan(provider: Provider, definition: ScanDefinition, symbols: Vec<String>, timeout: Duration) -> Vec<ScanRow> {
    use futures::StreamExt;
    let mut seen = std::collections::HashSet::new();
    let symbols: Vec<String> = symbols
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && seen.insert(s.to_ascii_uppercase()))
        .collect();
    let invalid = definition.validate().err().or_else(|| {
        (symbols.len() > MAX_SCAN_SYMBOLS)
            .then(|| format!("Scan limit is {MAX_SCAN_SYMBOLS} unique symbols; narrow the universe"))
    });
    if let Some(error) = invalid {
        return symbols
            .into_iter()
            .map(|symbol| ScanRow { symbol, evaluation: None, ts: None, simulated: false, error: Some(error.clone()) })
            .collect();
    }
    // Extra history stabilizes Wilder RSI; indicator evaluation still checks warm-up.
    let limit = definition.required_bars().saturating_mul(3).max(300);
    let fetches = symbols.into_iter().map(|symbol| {
        let provider = provider.clone();
        let definition = &definition;
        async move {
            let fetched =
                tokio::time::timeout(timeout, provider.scan_history(&symbol, definition.timeframe, limit)).await;
            let result = match fetched {
                Ok(r) => r.map_err(|e| format!("{e:#}")),
                Err(_) => Err(format!("History request timed out after {} seconds", timeout.as_secs())),
            };
            match result {
                Ok(history) => {
                    let series = Series::new(symbol.clone(), definition.timeframe, history.bars);
                    let ts = series.bars.last().map(|b| b.ts);
                    ScanRow {
                        symbol,
                        evaluation: Some(definition.evaluate(&series)),
                        ts,
                        simulated: history.simulated,
                        error: None,
                    }
                }
                Err(error) => ScanRow { symbol, evaluation: None, ts: None, simulated: false, error: Some(error) },
            }
        }
    });
    futures::stream::iter(fetches).buffered(4).collect().await
}

/// Reuse a fresh snapshot only when its provider and requested coverage match.
/// Fetch a full replacement otherwise: a fixed-size tail cannot prove continuity
/// after an absence or satisfy a larger indicator lookback.
async fn universe_history(s: &Shared, symbol: &str, tf: Timeframe, limit: usize) -> Result<(Vec<Bar>, bool), String> {
    let now = chrono::Utc::now().timestamp();
    let source = s.provider.scan_cache_key(symbol);
    let (stored, fetched) = {
        let store = s.scan_store.lock().expect("scan store");
        let store = store.as_ref().ok_or("The scan store could not be opened")?;
        let err = |e: anyhow::Error| format!("scan store: {e:#}");
        (store.load(symbol, tf, limit).map_err(err)?, store.fetched(symbol, tf).map_err(err)?)
    };
    if let Some(f) = &fetched
        && stored.len() >= limit
        && f.source == source
        && tr_data::scanstore::is_fresh(f.at, now)
    {
        return Ok((stored, f.simulated));
    }
    let history = tokio::time::timeout(Duration::from_secs(20), s.provider.scan_history(symbol, tf, limit))
        .await
        .map_err(|_| "History request timed out after 20 seconds".to_string())?
        .map_err(|e| format!("{e:#}"))?;
    if history.bars.is_empty() {
        return Err(format!("No history for {symbol}"));
    }
    if s.provider.scan_cache_key(symbol) != source {
        return Err("The scan data source changed during the request; run the scan again".into());
    }
    let fetch = Fetch { at: now, source, simulated: history.simulated };
    let mut store = s.scan_store.lock().expect("scan store");
    let store = store.as_mut().ok_or("The scan store could not be opened")?;
    store.store(symbol, tf, &history.bars, &fetch).map_err(|e| format!("scan store: {e:#}"))?;
    Ok((store.load(symbol, tf, limit).map_err(|e| format!("scan store: {e:#}"))?, history.simulated))
}

async fn scan_universe(
    s: &Shared,
    pane: PaneId,
    generation: u64,
    definition: &ScanDefinition,
    universe: Universe,
    cancel: &std::sync::atomic::AtomicBool,
) -> Vec<ScanRow> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use futures::StreamExt;
    let failed = |error: String| {
        vec![ScanRow {
            symbol: universe.label().into(),
            evaluation: None,
            ts: None,
            simulated: false,
            error: Some(error),
        }]
    };
    if let Err(e) = definition.validate() {
        return failed(e);
    }
    let listings =
        match tr_data::universe::load_or_fetch(&data_dir().join("universe.json"), Duration::from_secs(86_400)).await {
            Ok(l) => l,
            Err(e) => return failed(format!("Could not load the US symbol directory: {e:#}")),
        };
    let symbols: Vec<String> = listings.iter().filter(|l| universe.includes(l.venue)).map(|l| l.key()).collect();
    let total = symbols.len();
    let done = AtomicUsize::new(0);
    s.send(Response::ScanProgress { pane, generation, done: 0, total });
    let limit = definition.required_bars().saturating_mul(3).max(300);
    futures::stream::iter(symbols.into_iter().map(|symbol| {
        let done = &done;
        async move {
            let row = if cancel.load(Ordering::Relaxed) {
                ScanRow { symbol, evaluation: None, ts: None, simulated: false, error: Some("Stopped".into()) }
            } else {
                match universe_history(s, &symbol, definition.timeframe, limit).await {
                    Ok((bars, simulated)) => {
                        let series = Series::new(symbol.clone(), definition.timeframe, bars);
                        let ts = series.bars.last().map(|b| b.ts);
                        ScanRow { symbol, evaluation: Some(definition.evaluate(&series)), ts, simulated, error: None }
                    }
                    Err(error) => ScanRow { symbol, evaluation: None, ts: None, simulated: false, error: Some(error) },
                }
            };
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_multiple_of(25) || n == total {
                s.send(Response::ScanProgress { pane, generation, done: n, total });
            }
            row
        }
    }))
    .buffered(4)
    .collect()
    .await
}

async fn trade(t: TradeRequest) -> TradeResponse {
    let err = |e: anyhow::Error| format!("{e:#}");
    let client = |creds| tr_broker::SnapTrade::new(creds).map_err(err);
    match t {
        TradeRequest::Preview { creds, req, ticket } => {
            let result = match client(creds) {
                Ok(c) => c.order_impact(&req).await.map(Box::new).map_err(err),
                Err(e) => Err(e),
            };
            TradeResponse::Preview { ticket, result }
        }
        TradeRequest::Place { creds, preview, ticket } => {
            let result = match client(creds) {
                Ok(c) => c.place_previewed(&preview).await.map(|o| o.map(Box::new)),
                Err(e) => Err(tr_broker::trading::PlaceError::NotSent(e)),
            };
            TradeResponse::Placed { ticket, preview, result }
        }
        TradeRequest::Cancel { creds, account, order } => {
            let result = match client(creds) {
                Ok(c) => c.cancel_order(&account, &order).await.map_err(err),
                Err(e) => Err(e),
            };
            TradeResponse::Cancelled { account, order, result }
        }
        TradeRequest::Portal { creds, dark } => TradeResponse::Portal(match client(creds) {
            Ok(c) => c.trading_portal_url(dark).await.map_err(err),
            Err(e) => Err(e),
        }),
        TradeRequest::Capabilities { creds } => TradeResponse::Capabilities(match client(creds) {
            Ok(c) => c.connection_capabilities().await.map_err(|e| {
                let throttle = tr_broker::snaptrade::throttle(&e);
                CapsError { message: err(e), throttled: throttle.is_some(), retry_after: throttle.flatten() }
            }),
            Err(message) => Err(CapsError { message, throttled: false, retry_after: None }),
        }),
    }
}

/// Register a load for `generation`; generations only move forward, since request
/// tasks run concurrently and may start out of order.
fn chart_cache_key(provider: &dyn tr_data::MarketData, symbol: &str) -> String {
    // Versioned namespace excludes old, ambiguous caches (including legacy samples).
    let route = provider.scan_cache_key(symbol);
    format!("chart-v2:{}:{route}:{symbol}", route.len())
}

fn claim_series(s: &Shared, pane: PaneId, generation: u64, source: &str) -> bool {
    let mut map = s.series.lock().expect("series lock");
    match map.get(&pane) {
        Some((g, _, _)) if *g > generation => false,
        Some((g, _, route)) if *g == generation => route == source,
        _ => {
            map.insert(pane, (generation, Arc::new(Vec::new()), source.to_string()));
            true
        }
    }
}

/// Store freshly loaded bars for a pane if `generation` is still the newest.
fn store_series(s: &Shared, pane: PaneId, generation: u64, bars: Arc<Vec<Bar>>) -> bool {
    let mut map = s.series.lock().expect("series lock");
    match map.get(&pane) {
        Some((g, _, _)) if *g > generation => false,
        _ => {
            let source = map.get(&pane).map(|(_, _, source)| source.clone()).unwrap_or_default();
            map.insert(pane, (generation, bars, source));
            true
        }
    }
}

fn current_series(s: &Shared, pane: PaneId, generation: u64) -> Option<Arc<Vec<Bar>>> {
    s.series.lock().expect("series lock").get(&pane).filter(|(g, _, _)| *g == generation).map(|(_, b, _)| b.clone())
}

/// Merge the provider's live feed (if any) into the pane's series, publishing at most
/// four updates a second. Stops when the pane reloads, closes or the feed ends.
fn start_live(s: Arc<Shared>, pane: PaneId, generation: u64, symbol: String, tf: Timeframe) {
    let Some(mut feed) = s.provider.live(&symbol, tf) else { return };
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let mut dirty = false;
        loop {
            tokio::select! {
                bar = feed.recv() => {
                    let Some(bar) = bar else { break };
                    let mut map = s.series.lock().expect("series lock");
                    match map.get_mut(&pane) {
                        Some((g, bars, source)) if *g == generation && *source == chart_cache_key(s.provider.as_ref(),&symbol)
                            => dirty |= Series::apply_live(Arc::make_mut(bars), bar),
                        _ => break,
                    }
                }
                _ = tick.tick() => {
                    if !dirty {
                        if current_series(&s, pane, generation).is_none() {
                            break;
                        }
                        continue;
                    }
                    dirty = false;
                    let Some(bars) = current_series(&s, pane, generation) else { break };
                    let gpu = build_gpu(bars.clone()).await;
                    s.send(Response::History {
                        pane,
                        generation,
                        symbol: symbol.clone(),
                        tf,
                        bars,
                        gpu,
                        prepended: 0,
                        from_cache: false,
                        exhausted: false,
                        live: true,
                    });
                }
            }
        }
        tracing::debug!("live feed for pane {pane} ({symbol}) stopped");
    });
}

async fn build_gpu(bars: Arc<Vec<Bar>>) -> SeriesGpu {
    tokio::task::spawn_blocking(move || SeriesGpu::build(&bars)).await.expect("pyramid build task")
}

fn resolve_politician(db: &Db, query: &str) -> Option<Politician> {
    if let Ok(id) = query.trim().parse::<i64>() {
        return db.politician(id).ok().flatten();
    }
    // Prefer an exact last-name hit, then substring matches by activity.
    let all = db.search_politicians(query.trim(), 50).ok()?;
    let q = query.trim().to_ascii_lowercase();
    all.iter()
        .find(|p| p.politician.name.to_ascii_lowercase().split_whitespace().last() == Some(q.as_str()))
        .or(all.first())
        .map(|p| p.politician.clone())
}

async fn profile(s: &Shared, query: &str) -> Option<Profile> {
    let (politician, trades, tracked) = {
        let db = s.db.lock().await;
        let p = resolve_politician(&db, query)?;
        let trades = db.trades_for_politician(p.id).ok()?;
        let tracked = db.is_tracked(p.id).unwrap_or(false);
        (p, trades, tracked)
    };
    // Daily closes for each traded ticker (bounded) to estimate performance.
    let mut tickers: Vec<String> = trades.iter().filter_map(|t| t.ticker.clone()).collect();
    tickers.sort();
    tickers.dedup();
    tickers.truncate(40);
    let oldest = trades.iter().map(|t| t.tx_date).min();
    let days = oldest.map(|d| (chrono::Utc::now().date_naive() - d).num_days().max(30) as usize + 10).unwrap_or(400);
    let mut prices: std::collections::HashMap<String, Vec<Bar>> = std::collections::HashMap::new();
    let provider = s.provider.clone();
    let fetches = tickers.into_iter().map(|t| {
        let provider = provider.clone();
        async move {
            let r = provider.history(&format!("{t} US Equity"), Timeframe::D1, None, days.min(4000)).await;
            (t, r)
        }
    });
    for (t, r) in futures_join(fetches).await {
        if let Ok(b) = r {
            prices.insert(t, b);
        }
    }
    let (perf, returns) = performance::estimate(
        &trades,
        |tk, date| {
            let bars = prices.get(tk)?;
            let ts = date.and_hms_opt(23, 59, 59)?.and_utc().timestamp();
            let i = bars.partition_point(|b| b.ts <= ts).checked_sub(1)?;
            Some(bars[i].close)
        },
        |tk| prices.get(tk).and_then(|b| b.last()).map(|b| b.close),
    );
    Some(Profile { politician, trades, perf, returns, tracked })
}

/// Run futures concurrently, at most 6 in flight (keeps data providers happy).
async fn futures_join<F, T>(futs: impl Iterator<Item = F>) -> Vec<T>
where
    F: std::future::Future<Output = T>,
{
    use futures::StreamExt;
    futures::stream::iter(futs).buffered(6).collect().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_band_outlines_still_contribute_to_fill_autoscale() {
        use tr_core::indicators::study::{self, BandFill, OutputStyle};
        let bars: Vec<_> = (0..30)
            .map(|i| {
                let c = if i % 2 == 0 { 1. } else { 10. };
                Bar::new(i * 86400, c, c, c, c, 1.)
            })
            .collect();
        let mut specs = vec![IndicatorSpec::new("BB", &[3., 2.]).unwrap()];
        study::assign_ids(&mut specs);
        let settings = specs[0].chart.as_mut().unwrap();
        settings.fill = Some(BandFill { upper: "Upper".into(), lower: "Lower".into(), color: [1, 2, 3], opacity: 30 });
        for port in ["Upper", "Basis", "Lower"] {
            settings.outputs.insert(port.into(), OutputStyle { visible: false, ..Default::default() });
        }
        let built = build_indicators(&bars, Timeframe::D1, &specs, &theme::palette());
        assert!(built[0].overlays.is_empty());
        assert_eq!(built[0].bounds.len(), 2);
        let pane = tr_chart::scene::sub_pane(
            &built[0].bounds,
            &tr_chart::viewport::Viewport::fit_last(30, 20, 600.),
            200.,
            None,
            tr_chart::gpu::Style::default(),
        );
        assert!(pane.scale.lo < -4. && pane.scale.hi > 12.);
    }

    #[test]
    fn automatic_refresh_reports_a_changed_route_instead_of_staying_loading() {
        let (out, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            provider: Arc::new(tr_data::sample::SampleProvider::at(1_790_000_000, 100)),
            series: Default::default(),
            cache: std::sync::Mutex::new(None),
            scan_store: std::sync::Mutex::new(None),
            db: Mutex::new(Db::open_in_memory().unwrap()),
            out,
            ctx: egui::Context::default(),
        });
        assert!(claim_series(&shared, 1, 2, "previous-provider"));
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        runtime.block_on(handle(
            &shared,
            Request::History { pane: 1, generation: 2, symbol: "AAPL US Equity".into(), tf: Timeframe::D1, limit: 100 },
        ));
        assert!(
            matches!(rx.try_recv().unwrap(),Response::HistoryError {generation:2,error,..} if error.contains("route changed"))
        );
    }

    #[test]
    fn output_styles_preserve_hidden_sources_and_reach_gpu_uniforms() {
        use tr_core::indicators::study::{self, OutputStyle, Source};
        let bars = tr_core::sample::generate("AAPL", Timeframe::D1, 300, 1_790_000_000);
        let mut specs = vec![IndicatorSpec::new("MACD", &[]).unwrap(), IndicatorSpec::new("EMA", &[3.]).unwrap()];
        study::assign_ids(&mut specs);
        specs[0]
            .chart
            .as_mut()
            .unwrap()
            .outputs
            .insert("Signal".into(), OutputStyle { visible: false, ..Default::default() });
        specs[0]
            .chart
            .as_mut()
            .unwrap()
            .outputs
            .insert("Histogram".into(), OutputStyle { color: Some([255, 0, 0]), width: 3., ..Default::default() });
        specs[1].chart.as_mut().unwrap().source = Source::Output { study: 1, output: "Signal".into() };
        let built = build_indicators(&bars, Timeframe::D1, &specs, &theme::palette());
        assert!(built[0].ports.contains(&"Signal".into()));
        assert!(!built[0].overlays.iter().any(|(name, _)| name == "Signal"));
        assert!(built[1].error.is_none());
        let hist = &built[0].overlays.iter().find(|(name, _)| name == "Histogram").unwrap().1;
        assert_eq!(hist.width, 3.);
        let out = tr_chart::scene::sub_pane(
            std::slice::from_ref(hist),
            &tr_chart::viewport::Viewport::fit_last(300, 100, 800.),
            200.,
            None,
            tr_chart::gpu::Style::default(),
        );
        let tr_chart::gpu::DrawCmd::Line { uniforms, .. } = &out.cmds[0] else { panic!("Expected histogram") };
        assert_eq!(uniforms.up, [1., 0., 0., 1.]);
        assert_eq!(uniforms.down, [1., 0., 0., 1.]);
    }

    #[test]
    fn chart_cache_does_not_reuse_legacy_or_other_provider_history() {
        let provider = tr_data::sample::SampleProvider::at(1_790_000_000, 100);
        let key = chart_cache_key(&provider, "AAPL US Equity");
        let mut cache = BarCache::open_in_memory().unwrap();
        let bars = [Bar::new(100, 10., 11., 9., 10., 1.)];
        cache.store("AAPL US Equity", Timeframe::D1, &bars).unwrap();
        cache.store("chart-v2:5:yahoo:AAPL US Equity", Timeframe::D1, &bars).unwrap();
        assert!(cache.load(&key, Timeframe::D1, 100).unwrap().is_empty());
        cache.store(&key, Timeframe::D1, &bars).unwrap();
        assert_eq!(cache.load(&key, Timeframe::D1, 100).unwrap(), bars);
        assert!(cache.load(&chart_cache_key(&provider, "MSFT US Equity"), Timeframe::D1, 100).unwrap().is_empty());
    }

    #[test]
    fn scan_deduplicates_and_labels_explicit_samples() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let provider: Provider = Arc::new(tr_data::sample::SampleProvider::at(1_790_000_000, 400));
        let rows = rt.block_on(scan(
            provider,
            ScanDefinition::default(),
            vec![" AAPL ".into(), "aapl".into(), "".into(), "MSFT".into()],
            Duration::from_secs(1),
        ));
        assert_eq!(rows.iter().map(|r| r.symbol.as_str()).collect::<Vec<_>>(), vec!["AAPL", "MSFT"]);
        assert!(rows.iter().all(|r| r.simulated && r.ts.is_some() && r.evaluation.is_some() && r.error.is_none()));
    }

    #[test]
    fn alert_worker_preserves_symbol_revision_and_sample_provenance_without_chart_panes() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let (out, rx) = mpsc::channel();
        let s = Arc::new(Shared {
            provider: Arc::new(tr_data::sample::SampleProvider::at(1_790_000_000, 20)),
            series: Default::default(),
            cache: std::sync::Mutex::new(None),
            scan_store: std::sync::Mutex::new(None),
            db: Mutex::new(Db::open_in_memory().unwrap()),
            out,
            ctx: egui::Context::default(),
        });
        rt.block_on(handle(
            &s,
            Request::PriceAlertQuotes { revision: 42, symbols: vec!["AAPL US Equity".into(), "BTCUSD Curncy".into()] },
        ));
        let Response::PriceAlertQuotes { revision, observations } = rx.recv().unwrap() else {
            panic!("Expected alert observations");
        };
        assert_eq!(revision, 42);
        assert_eq!(observations.len(), 2);
        for (symbol, result) in observations {
            let q = result.unwrap();
            assert_eq!(q.symbol, symbol);
            assert!(q.simulated && q.price.is_finite());
        }
        assert!(s.series.try_lock().unwrap().is_empty());
    }

    #[test]
    fn scan_rejects_oversized_universe() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let provider: Provider = Arc::new(tr_data::sample::SampleProvider::at(0, 0));
        let rows = rt.block_on(scan(
            provider,
            ScanDefinition::default(),
            (0..=MAX_SCAN_SYMBOLS).map(|i| format!("SYMBOL{i}")).collect(),
            Duration::from_secs(1),
        ));
        assert_eq!(rows.len(), MAX_SCAN_SYMBOLS + 1);
        assert!(rows.iter().all(|r| r.evaluation.is_none() && r.error.as_ref().unwrap().contains("Scan limit")));
    }

    #[test]
    fn compare_alignment_carries_last_close() {
        let base: Vec<Bar> = (0..5).map(|i| Bar::new(i * 10, 100.0, 100.0, 100.0, 100.0 + i as f64, 1.0)).collect();
        let other: Vec<Bar> = vec![Bar::new(5, 0.0, 0.0, 0.0, 50.0, 1.0), Bar::new(25, 0.0, 0.0, 0.0, 55.0, 1.0)];
        let a = align_compare(&base, &other);
        assert!(a[0].is_nan());
        assert_eq!(&a[1..], &[50.0, 50.0, 55.0, 55.0]);
    }

    #[test]
    fn indicators_build_expected_overlays() {
        let bars = tr_core::sample::generate("X", Timeframe::D1, 300, 1_790_000_000);
        let palette = crate::themes::terminal_light().palette(theme::DEFAULT_ACCENT);
        let specs: Vec<IndicatorSpec> =
            ["BB", "MACD", "VWAP", "PSAR", "PIVOT"].iter().map(|c| IndicatorSpec::new(c, &[]).unwrap()).collect();
        let built = build_indicators(&bars, Timeframe::D1, &specs, &palette);
        assert_eq!(built[0].overlays.len(), 3);
        // The histogram draws first, under the MACD and signal lines.
        assert_eq!(built[1].overlays[0].1.kind, LineKind::Hist);
        assert_ne!(built[1].overlays[2].1.color, built[1].overlays[1].1.color, "signal stands out from MACD");
        assert_eq!(built[2].overlays.len(), 1);
        assert!(built[3].overlays.iter().all(|(_, o)| o.kind == LineKind::Dots));
        assert_eq!(built[3].overlays[0].1.color, theme::rgba(palette.up));
        // Every pair of bars holding a marker keeps one when zoomed out.
        let up = &built[3].overlays[0].1.line.pyramid;
        let pairs = up.data[..up.offsets[1]].chunks(2).filter(|c| c.iter().any(|p| p[1].is_finite())).count();
        assert_eq!(up.data[up.offsets[1]..up.offsets[2]].iter().filter(|p| p[1].is_finite()).count(), pairs);
        assert!(built[4].overlays.iter().all(|(_, o)| o.kind == LineKind::Steps));
    }

    #[test]
    fn universe_history_stores_reuses_and_labels_samples() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let (out, _rx) = mpsc::channel();
        let s = Shared {
            provider: Arc::new(tr_data::sample::SampleProvider::at(1_790_000_000, 400)),
            series: Default::default(),
            cache: std::sync::Mutex::new(None),
            scan_store: std::sync::Mutex::new(Some(ScanStore::open_in_memory().unwrap())),
            db: Mutex::new(Db::open_in_memory().unwrap()),
            out,
            ctx: egui::Context::default(),
        };
        let (bars, simulated) = rt.block_on(universe_history(&s, "AAPL US Equity", Timeframe::D1, 300)).unwrap();
        assert!(simulated);
        assert_eq!(bars.len(), 300);
        let fetch = s.scan_store.lock().unwrap().as_ref().unwrap().fetched("AAPL US Equity", Timeframe::D1).unwrap();
        assert!(fetch.is_some_and(|f| f.simulated && f.source == s.provider.name()));
        // Fresh bars come back from the store with their provenance intact.
        let (again, simulated) = rt.block_on(universe_history(&s, "AAPL US Equity", Timeframe::D1, 300)).unwrap();
        assert!(simulated);
        assert_eq!(again.last().map(|b| b.ts), bars.last().map(|b| b.ts));
    }

    #[test]
    fn universe_cache_refetches_for_source_changes_gaps_and_larger_lookbacks() {
        use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
        struct Fixture {
            simulated: AtomicBool,
            latest: AtomicI64,
            empty: AtomicBool,
            requests: std::sync::Mutex<Vec<usize>>,
        }
        #[async_trait::async_trait]
        impl tr_data::MarketData for Fixture {
            fn name(&self) -> &str {
                "fixture-router"
            }
            fn scan_cache_key(&self, _: &str) -> String {
                if self.simulated.load(Ordering::Relaxed) { "sample" } else { "live" }.into()
            }
            async fn history(&self, _: &str, tf: Timeframe, _: Option<i64>, limit: usize) -> anyhow::Result<Vec<Bar>> {
                self.requests.lock().unwrap().push(limit);
                if self.empty.load(Ordering::Relaxed) {
                    return Ok(Vec::new());
                }
                let end = self.latest.load(Ordering::Relaxed);
                let price = if self.simulated.load(Ordering::Relaxed) { 10.0 } else { 100.0 };
                Ok((0..limit)
                    .map(|i| {
                        Bar::new((end - (limit - 1 - i) as i64) * tf.seconds(), price, price, price, price, 1000.0)
                    })
                    .collect())
            }
            async fn scan_history(
                &self,
                symbol: &str,
                tf: Timeframe,
                limit: usize,
            ) -> anyhow::Result<tr_data::ScanHistory> {
                Ok(tr_data::ScanHistory {
                    bars: self.history(symbol, tf, None, limit).await?,
                    simulated: self.simulated.load(Ordering::Relaxed),
                })
            }
        }
        let fixture = Arc::new(Fixture {
            simulated: AtomicBool::new(true),
            latest: AtomicI64::new(1000),
            empty: AtomicBool::new(false),
            requests: Default::default(),
        });
        let (out, _rx) = mpsc::channel();
        let s = Shared {
            provider: fixture.clone(),
            series: Default::default(),
            cache: std::sync::Mutex::new(None),
            scan_store: std::sync::Mutex::new(Some(ScanStore::open_in_memory().unwrap())),
            db: Mutex::new(Db::open_in_memory().unwrap()),
            out,
            ctx: egui::Context::default(),
        };
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let tf = Timeframe::D1;
        let symbol = "AAPL US Equity";
        let get = |limit| rt.block_on(universe_history(&s, symbol, tf, limit));
        assert!(get(300).unwrap().1);
        assert!(get(300).unwrap().1);
        assert_eq!(*fixture.requests.lock().unwrap(), [300], "Fresh compatible snapshots are reused");

        fixture.simulated.store(false, Ordering::Relaxed);
        let (real, simulated) = get(300).unwrap();
        assert!(!simulated);
        assert!(real.iter().all(|bar| bar.close == 100.0), "No sample bars survive a source switch");
        assert_eq!(get(500).unwrap().0.len(), 500, "A larger lookback must be fetched even while fresh");

        let expire = || {
            let mut guard = s.scan_store.lock().unwrap();
            let store = guard.as_mut().unwrap();
            let bars = store.load(symbol, tf, 500).unwrap();
            store.store(symbol, tf, &bars, &Fetch { at: 0, source: "live".into(), simulated: false }).unwrap();
        };
        expire();
        fixture.latest.store(5000, Ordering::Relaxed);
        let (refreshed, simulated) = get(500).unwrap();
        assert!(!simulated);
        assert_eq!(refreshed.first().unwrap().ts, 4501 * tf.seconds());
        assert!(refreshed.windows(2).all(|bars| bars[1].ts - bars[0].ts == tf.seconds()));
        assert_eq!(*fixture.requests.lock().unwrap(), [300, 300, 500, 500]);

        expire();
        fixture.empty.store(true, Ordering::Relaxed);
        assert!(get(500).unwrap_err().contains("No history"));
        assert_eq!(
            s.scan_store.lock().unwrap().as_ref().unwrap().fetched(symbol, tf).unwrap().unwrap().at,
            0,
            "Empty refresh must not mark old bars fresh"
        );
    }

    #[test]
    fn series_generations_only_move_forward() {
        let (out, _rx) = mpsc::channel();
        let s = Shared {
            provider: std::sync::Arc::new(tr_data::sample::SampleProvider::at(0, 10)),
            series: Default::default(),
            cache: std::sync::Mutex::new(None),
            scan_store: std::sync::Mutex::new(None),
            db: Mutex::new(Db::open_in_memory().unwrap()),
            out,
            ctx: egui::Context::default(),
        };
        // The newer load's task starts first; the older one must not take over.
        assert!(claim_series(&s, 1, 2, "test-source"));
        assert!(!claim_series(&s, 1, 1, "test-source"));
        assert!(!claim_series(&s, 1, 2, "different-source"));
        assert!(!store_series(&s, 1, 1, Arc::new(vec![])));
        assert!(store_series(&s, 1, 2, Arc::new(vec![Bar::new(0, 1.0, 1.0, 1.0, 1.0, 1.0)])));
        assert_eq!(current_series(&s, 1, 2).unwrap().len(), 1);
        assert!(current_series(&s, 1, 1).is_none());
    }

    #[test]
    fn join_all_runs_everything() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let out = rt.block_on(futures_join((0..20).map(|i| async move {
            tokio::time::sleep(Duration::from_millis(1)).await;
            i * 2
        })));
        assert_eq!(out, (0..20).map(|i| i * 2).collect::<Vec<_>>());
    }
}
