use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};
use tr_core::{Bar, Timeframe, crypto::CryptoInstrument, session::Session};
use tr_data::{MarketData, Quote, SearchHit};

use crate::{ToolResult, args::LiveArgs};

pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceBasis {
    Adjusted,
    Unadjusted,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default)]
pub struct History {
    pub bars: Vec<Bar>,
    pub price_basis: PriceBasis,
    pub session: Session,
    /// Authoritative source identity; do not infer a venue from an equity ticker.
    pub instrument: Option<CryptoInstrument>,
    // A5b-MULTI provenance is optional: absence must never be described as live.
    pub source: Option<String>,
    pub simulated: Option<bool>,
}

pub trait DataHost: Send + Sync {
    fn history<'a>(&'a self, symbol: &'a str, tf: Timeframe, end: Option<i64>, limit: usize)
    -> HostFuture<'a, History>;
    fn search<'a>(&'a self, query: &'a str) -> HostFuture<'a, Vec<SearchHit>>;
    fn quotes<'a>(&'a self, symbols: &'a [String]) -> HostFuture<'a, Vec<Quote>>;
}

/// `chart_id = None` means the host's pinned chart, not whichever pane currently has focus.
/// Hosts must check loaded/future drawing anchors, stale ids and confirmation/consent at
/// execution time, and report linked panes moved by set_view. Queue deadlines belong to the host.
pub trait LiveHost: Send + Sync {
    fn call_live<'a>(&'a self, name: &'a str, args: LiveArgs) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>>;
}

/// Shared edit validation for windowed and headless hosts. Session ownership is
/// deliberately not inferred from persisted `created_by` metadata.
pub mod edits {
    use std::collections::{HashMap, HashSet};

    use tr_chart::drawings::{Anchor, Author, Drawing, Tool};
    use tr_core::{
        Bar, Timeframe,
        indicators::{registry::Spec, study},
    };

    use crate::{
        ToolResult,
        args::{self, LiveArgs},
    };

    pub(super) fn timeframe_label(label: &str, timeframe: Timeframe) -> String {
        let sanitized = tr_chart::drawings::text::sanitize(label);
        let label = sanitized.trim();
        if label.is_empty() {
            return String::new();
        }
        let mut tokens = label.split(|c: char| !c.is_alphanumeric()).filter(|token| !token.is_empty()).peekable();
        let includes_timeframe = std::iter::from_fn(|| {
            let token = tokens.next()?;
            // Numeric M labels before "share(s)" mean a quantity, not minutes/months.
            let shares = token.ends_with(['m', 'M'])
                && tokens
                    .peek()
                    .is_some_and(|next| next.eq_ignore_ascii_case("shares") || next.eq_ignore_ascii_case("share"));
            Some(
                !shares
                    && Timeframe::ALL.iter().any(|tf| {
                        token.eq_ignore_ascii_case(&format!("{tf:?}")) || token.eq_ignore_ascii_case(tf.label())
                    }),
            )
        })
        .any(|matches| matches);
        if includes_timeframe {
            label.into()
        } else {
            let prefix = format!("{timeframe:?} ");
            // Reserve the prefix's characters; truncate only the agent's text.
            let text: String = label.chars().take(500 - prefix.chars().count()).collect();
            format!("{prefix}{text}")
        }
    }

    pub const MAX_CREATED_PER_TURN: usize = 20;
    pub const MAX_AGENT_OBJECTS: usize = 100;
    /// Live-host edit failures supplement the registry's existing invalid_args,
    /// tool_disabled and budget_exhausted errors. Codes are stable wire strings.
    pub const ERROR_CODES: &[&str] = &[
        "stale_id",
        "wrong_chart",
        "not_owned",
        "chart_not_found",
        "pin_changed",
        "turn_object_limit",
        "chart_object_limit",
        "analysis_mismatch",
        "analysis_drawing_limit",
        "data_loading_timeout",
        "provider_error",
    ];

    pub fn error(code: &str, message: &str) -> ToolResult {
        ToolResult::error(code, message, None)
    }

    pub fn chart_id(args: &LiveArgs) -> Option<&str> {
        match args {
            LiveArgs::ReadChart(a) => a.chart_id.as_deref(),
            LiveArgs::IndicatorValues(a) => a.chart_id.as_deref(),
            LiveArgs::ReadUserActions(a) => a.chart_id.as_deref(),
            LiveArgs::SetView(a) => a.chart_id.as_deref(),
            LiveArgs::CreateDrawing(a) => a.chart_id.as_deref(),
            LiveArgs::UpdateDrawing(a) => a.chart_id.as_deref(),
            LiveArgs::DeleteDrawing(a) => a.chart_id.as_deref(),
            LiveArgs::AddIndicator(a) => a.chart_id.as_deref(),
            LiveArgs::RemoveIndicator(a) => a.chart_id.as_deref(),
            LiveArgs::Compare(a) => a.chart_id.as_deref(),
            _ => None,
        }
    }

    /// Persisted object provenance, including comparisons restored from layouts.
    /// Missing legacy metadata is user-owned, never inferred from a symbol.
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    pub struct Comparison {
        pub symbol: String,
        #[serde(default)]
        pub created_by: Author,
        #[serde(default)]
        pub turn_id: Option<String>,
    }
    impl From<String> for Comparison {
        fn from(symbol: String) -> Self {
            Self { symbol, created_by: Author::User, turn_id: None }
        }
    }
    impl From<&str> for Comparison {
        fn from(symbol: &str) -> Self {
            symbol.to_owned().into()
        }
    }

    pub struct Chart<'a> {
        pub uid: u64,
        pub symbol: &'a str,
        pub timeframe: Timeframe,
        /// Current axis precision supplied by the chart renderer.
        pub price_decimals: usize,
        pub bars: &'a [Bar],
        pub drawings: &'a [Drawing],
        pub indicators: &'a [Spec],
        pub comparisons: &'a [Comparison],
    }

    pub enum Plan {
        Drawings(Vec<Drawing>, u64),
        Indicators(Vec<Spec>, String),
        View { symbol: String, timeframe: Timeframe, from: Option<i64>, to: Option<i64> },
        Compare(args::Compare),
    }

    #[derive(Default)]
    pub struct Session {
        owned: HashMap<(u64, String), String>,
        turns: HashSet<String>,
        known: HashMap<String, HashSet<u64>>,
        created: HashMap<String, usize>,
        study_ids: HashMap<u64, HashSet<u32>>,
        analysis_created: HashMap<(String, &'static str), usize>,
    }

    impl Session {
        pub fn observe(&mut self, chart: &Chart<'_>) {
            for settings in chart.indicators.iter().filter_map(|s| s.chart.as_ref()) {
                self.study_ids.entry(chart.uid).or_default().insert(settings.id);
            }
            for id in chart.drawings.iter().map(|d| format!("drawing:{}", d.id)).chain(
                chart
                    .indicators
                    .iter()
                    .filter_map(|s| s.chart.as_ref())
                    .map(|s| format!("study:{}:{}", chart.uid, s.id)),
            ) {
                self.known.entry(id).or_default().insert(chart.uid);
            }
        }

        fn own(&mut self, chart: u64, id: String, turn: &str) {
            self.turns.insert(turn.to_owned());
            self.owned.insert((chart, id.clone()), turn.to_owned());
            self.known.entry(id).or_default().insert(chart);
            *self.created.entry(turn.to_owned()).or_default() += 1;
        }

        fn can_create(&self, chart: &Chart<'_>, turn: &str) -> Result<(), ToolResult> {
            if self.created.get(turn).copied().unwrap_or_default() >= MAX_CREATED_PER_TURN {
                return Err(error(
                    "turn_object_limit",
                    "create_drawing/add_indicator/compare reached 20 new objects this turn; reuse update_drawing or stop creating until a new turn",
                ));
            }
            let count = chart.drawings.iter().filter(|d| d.created_by == Author::Agent).count()
                + chart
                    .indicators
                    .iter()
                    .filter_map(|s| s.chart.as_ref())
                    .filter(|s| s.created_by == Author::Agent)
                    .count()
                + chart.comparisons.iter().filter(|comparison| comparison.created_by == Author::Agent).count();
            if count >= MAX_AGENT_OBJECTS {
                return Err(error(
                    "chart_object_limit",
                    "chart reached 100 agent objects; update existing drawings or delete session-owned objects before creating more",
                ));
            }
            Ok(())
        }

        fn check_owner(&self, chart: u64, id: &str, author: Author, turn: Option<&str>) -> Result<(), ToolResult> {
            if author != Author::Agent
                || !turn.is_some_and(|turn| {
                    self.turns.contains(turn)
                        && self.owned.get(&(chart, id.to_owned())).is_some_and(|record| record == turn)
                })
            {
                return Err(error(
                    "not_owned",
                    "Only objects created by this agent session may be edited; Keep transfers ownership to the user",
                ));
            }
            Ok(())
        }

        fn missing(&self, chart: u64, id: &str) -> ToolResult {
            if self.known.get(id).is_some_and(|charts| !charts.contains(&chart)) {
                error("wrong_chart", "Object belongs to another chart")
            } else {
                error("stale_id", "Unknown or removed object id")
            }
        }

        pub fn plan(&mut self, chart: &Chart<'_>, turn: &str, args: LiveArgs) -> Result<Plan, ToolResult> {
            self.observe(chart);
            match args {
                LiveArgs::CreateDrawing(a) => {
                    if a.analysis_ref.is_some()
                        && tr_data::security(&a.symbol).key() != tr_data::security(chart.symbol).key()
                    {
                        return Err(error(
                            "analysis_mismatch",
                            "Analysis symbol differs from the current chart; re-run analyze for this chart",
                        ));
                    }
                    check_symbol(chart, &a.symbol)?;
                    let tool = match a.kind {
                        args::DrawingKind::Horizontal => Tool::Horizontal,
                        args::DrawingKind::Vertical => Tool::Vertical,
                        args::DrawingKind::TrendLine => Tool::Trendline,
                        args::DrawingKind::Ray => Tool::Ray,
                        args::DrawingKind::Rectangle => Tool::Rectangle,
                        args::DrawingKind::Text => Tool::Text,
                    };
                    if a.zone_role.is_some() && tool != Tool::Rectangle {
                        return Err(error(
                            "invalid_args",
                            "zone_role requires kind=rectangle; omit zone_role for lines or text",
                        ));
                    }
                    let source_timeframe = a
                        .source_timeframe
                        .as_deref()
                        .map(str::parse::<Timeframe>)
                        .transpose()
                        .map_err(|_| {
                            error("invalid_args", "source_timeframe must be a supported Timeframe (e.g. 1W, 4H)")
                        })?
                        .unwrap_or(chart.timeframe);
                    if a.analysis_ref.is_some() && source_timeframe != chart.timeframe {
                        return Err(error(
                            "analysis_mismatch",
                            "Analysis timeframe differs from the current chart; re-run analyze on the current timeframe",
                        ));
                    }
                    let points = drawing_anchors(chart, &a.anchors, tool, a.analysis_ref.is_some())?;
                    let analysis_kind = if a.zone_role.is_some() {
                        "zones"
                    } else if points.last().unwrap().price > points[0].price {
                        "rising_support"
                    } else {
                        "falling_resistance"
                    };
                    if a.analysis_ref.is_some() {
                        let limit = if analysis_kind == "zones" { 6 } else { 2 };
                        if self.analysis_created.get(&(turn.into(), analysis_kind)).copied().unwrap_or_default()
                            >= limit
                        {
                            return Err(error(
                                "analysis_drawing_limit",
                                "This turn has enough analysis drawings (6 zones and 2 lines per role maximum); please summarize the drawings already added",
                            ));
                        }
                    }
                    let mut drawing = Drawing::new(tool, points[0], *points.last().unwrap());
                    drawing.symbol = Some(chart.symbol.into());
                    drawing.all_symbols = a.all_symbols;
                    drawing.created_by = Author::Agent;
                    drawing.turn_id = Some(turn.into());
                    drawing.source_timeframe = Some(source_timeframe);
                    drawing.zone_role = a.zone_role;
                    drawing.extend_left = a.extend_left;
                    drawing.extend_right = a.extend_right;
                    drawing.dashed = a.dashed;
                    drawing.technical_analysis = a.analysis_ref.is_some();
                    drawing.text = timeframe_label(a.label.as_deref().unwrap_or_default(), source_timeframe);
                    drawing.auto_label = drawing.zone_role.is_some() && drawing.text.is_empty();
                    if drawing.auto_label {
                        drawing.text = zone_label(&drawing, source_timeframe, chart.price_decimals);
                    }
                    drawing.sanitize();
                    let id = drawing.id;
                    let mut next = chart.drawings.to_vec();
                    if a.analysis_ref.is_some() {
                        next.retain(|d| {
                            !(d.created_by == Author::Agent
                                && d.turn_id.as_deref() != Some(turn)
                                && d.symbol.as_deref() == Some(chart.symbol)
                                && d.source_timeframe == Some(source_timeframe)
                                && if tool == Tool::Rectangle {
                                    d.tool == Tool::Rectangle && (d.zone_role.is_some() || d.technical_analysis)
                                } else {
                                    matches!(d.tool, Tool::Ray | Tool::Trendline)
                                        && (d.technical_analysis
                                            || ["support", "resistance", "trend"]
                                                .iter()
                                                .any(|word| d.text.to_ascii_lowercase().contains(word)))
                                })
                        });
                    }
                    self.can_create(&Chart { drawings: &next, ..*chart }, turn)?;
                    next.push(drawing);
                    self.own(chart.uid, format!("drawing:{id}"), turn);
                    if a.analysis_ref.is_some() {
                        *self.analysis_created.entry((turn.into(), analysis_kind)).or_default() += 1;
                    }
                    Ok(Plan::Drawings(next, id))
                }
                LiveArgs::UpdateDrawing(a) => {
                    check_symbol(chart, &a.symbol)?;
                    let key = format!("drawing:{}", a.id);
                    let index = chart
                        .drawings
                        .iter()
                        .position(|d| d.id == a.id)
                        .ok_or_else(|| self.missing(chart.uid, &key))?;
                    let mut next = chart.drawings.to_vec();
                    let drawing = &mut next[index];
                    self.check_owner(chart.uid, &key, drawing.created_by, drawing.turn_id.as_deref())?;
                    if drawing.symbol.as_deref() != Some(chart.symbol) {
                        return Err(error("wrong_chart", "Drawing belongs to another symbol"));
                    }
                    let timeframe = drawing.source_timeframe.unwrap_or(chart.timeframe);
                    let anchors_changed = a.anchors.is_some();
                    if let Some(points) = a.anchors {
                        let points = drawing_anchors(chart, &points, drawing.tool, false)?;
                        drawing.a = points[0];
                        drawing.b = *points.last().unwrap();
                    }
                    if let Some(label) = a.label {
                        drawing.text = timeframe_label(&label, timeframe);
                        drawing.auto_label = false;
                    } else if drawing.auto_label && anchors_changed {
                        drawing.text = zone_label(drawing, timeframe, chart.price_decimals);
                    }
                    drawing.all_symbols = a.all_symbols;
                    drawing.sanitize();
                    Ok(Plan::Drawings(next, a.id))
                }
                LiveArgs::DeleteDrawing(a) => {
                    check_symbol(chart, &a.symbol)?;
                    let key = format!("drawing:{}", a.id);
                    let drawing =
                        chart.drawings.iter().find(|d| d.id == a.id).ok_or_else(|| self.missing(chart.uid, &key))?;
                    self.check_owner(chart.uid, &key, drawing.created_by, drawing.turn_id.as_deref())?;
                    if drawing.symbol.as_deref() != Some(chart.symbol) {
                        return Err(error("wrong_chart", "Drawing belongs to another symbol"));
                    }
                    Ok(Plan::Drawings(chart.drawings.iter().filter(|d| d.id != a.id).cloned().collect(), a.id))
                }
                LiveArgs::AddIndicator(a) => {
                    self.can_create(chart, turn)?;
                    let def = tr_core::indicators::registry::find(&a.code)
                        .ok_or_else(|| error("invalid_args", "Unknown indicator code"))?;
                    if a.params.len() > def.params.len() || a.params.iter().any(|p| !p.is_finite()) {
                        return Err(error("invalid_args", "Invalid indicator parameters"));
                    }
                    let mut spec = Spec::new(&a.code, &a.params).unwrap();
                    // Studies have their own chart-local u32 space. A large persisted
                    // drawing id must never poison study allocation. Retain observed
                    // and removed ids for this session to reject stale handles.
                    let used = self.study_ids.entry(chart.uid).or_default();
                    let next = used.iter().copied().max().unwrap_or(0).checked_add(1);
                    let id = next
                        .filter(|id| *id != 0)
                        .or_else(|| (1..=u32::MAX).find(|id| !used.contains(id)))
                        .ok_or_else(|| error("chart_object_limit", "Study id space exhausted"))?;
                    used.insert(id);
                    let mut settings = study::Settings::new(id, &spec);
                    settings.created_by = Author::Agent;
                    settings.turn_id = Some(turn.into());
                    spec.chart = Some(Box::new(settings));
                    let id = format!("{}:{id}", chart.uid);
                    let mut next = chart.indicators.to_vec();
                    next.push(spec);
                    self.own(chart.uid, format!("study:{id}"), turn);
                    Ok(Plan::Indicators(next, id))
                }
                LiveArgs::RemoveIndicator(a) => {
                    let key = format!("study:{}", a.id);
                    let id =
                        a.id.split_once(':')
                            .and_then(|(uid, id)| (uid == chart.uid.to_string()).then_some(id))
                            .and_then(|id| id.parse::<u32>().ok());
                    let index = chart
                        .indicators
                        .iter()
                        .position(|s| s.chart.as_ref().is_some_and(|s| Some(s.id) == id))
                        .ok_or_else(|| {
                            if a.id.split_once(':').is_some_and(|(uid, _)| uid != chart.uid.to_string()) {
                                error("wrong_chart", "Study belongs to another chart")
                            } else {
                                self.missing(chart.uid, &key)
                            }
                        })?;
                    let settings = chart.indicators[index].chart.as_ref().unwrap();
                    self.check_owner(chart.uid, &key, settings.created_by, settings.turn_id.as_deref())?;
                    // Never cascade a deletion into a user's calculation, pane or fill links.
                    if chart.indicators.iter().enumerate().any(|(i, s)| {
                        i != index
                            && s.chart.as_ref().is_some_and(|c| {
                                matches!(c.source, study::Source::Output { study, .. } if Some(study) == id)
                                    || matches!(c.pane, study::Pane::With(parent) if Some(parent) == id)
                                    || c.fill.as_ref().is_some_and(|f| {
                                        [f.upper.study, f.lower.study].iter().any(|p| p.is_some() && *p == id)
                                    })
                            })
                    }) {
                        return Err(error(
                            "not_owned",
                            "Study has dependents; remove their links in the user editor first",
                        ));
                    }
                    let mut next = chart.indicators.to_vec();
                    next.remove(index);
                    Ok(Plan::Indicators(next, a.id))
                }
                LiveArgs::SetView(a) => {
                    let timeframe = a
                        .timeframe
                        .as_deref()
                        .map(str::parse)
                        .transpose()
                        .map_err(|_| error("invalid_args", "Unknown timeframe"))?
                        .unwrap_or(chart.timeframe);
                    if a.symbol
                        .as_ref()
                        .is_some_and(|symbol| tr_data::security(symbol).key() != tr_data::security(chart.symbol).key())
                    {
                        return Err(error(
                            "invalid_args",
                            "set_view is limited to the loaded symbol; symbol changes are not allowed",
                        ));
                    }
                    // Do not clamp against the old timeframe. Hosts apply the range
                    // only after loading the requested timeframe's bars.
                    if a.from.zip(a.to).is_some_and(|(from, to)| from > to) {
                        return Err(error("invalid_args", "from must not exceed to"));
                    }
                    Ok(Plan::View {
                        symbol: a.symbol.map_or_else(|| chart.symbol.into(), |s| tr_data::security(&s).key()),
                        timeframe,
                        from: a.from,
                        to: a.to,
                    })
                }
                LiveArgs::Compare(mut a) => {
                    a.symbol = tr_data::security(&a.symbol).key();
                    let key = format!("comparison:{}", a.symbol);
                    match a.action {
                        args::CompareAction::Add => {
                            self.can_create(chart, turn)?;
                            if a.symbol == chart.symbol
                                || chart.comparisons.iter().any(|c| c.symbol == a.symbol)
                                || chart.comparisons.len() >= 8
                            {
                                return Err(error(
                                    "invalid_args",
                                    "Comparison already exists, matches base symbol or chart is full",
                                ));
                            }
                            self.own(chart.uid, key, turn);
                        }
                        args::CompareAction::Remove => {
                            let comparison = chart
                                .comparisons
                                .iter()
                                .find(|c| c.symbol == a.symbol)
                                .ok_or_else(|| self.missing(chart.uid, &key))?;
                            self.check_owner(chart.uid, &key, comparison.created_by, comparison.turn_id.as_deref())?;
                        }
                    }
                    Ok(Plan::Compare(a))
                }
                _ => Err(error("tool_disabled", "Not an edit tool")),
            }
        }
    }

    fn check_symbol(chart: &Chart<'_>, symbol: &str) -> Result<(), ToolResult> {
        if tr_data::security(symbol).key() != tr_data::security(chart.symbol).key() {
            return Err(error(
                "wrong_chart",
                &format!("symbol must match the loaded chart {}; call read_chart before retrying", chart.symbol),
            ));
        }
        Ok(())
    }

    fn zone_label(drawing: &Drawing, timeframe: Timeframe, decimals: usize) -> String {
        let Some(role) = drawing.zone_role else { return String::new() };
        let role = match role {
            args::ZoneRole::Support => "support",
            args::ZoneRole::Resistance => "resistance",
            args::ZoneRole::InPlay => "in play",
        };
        let low = tr_chart::scale::format_price(drawing.a.price.min(drawing.b.price), decimals);
        let high = tr_chart::scale::format_price(drawing.a.price.max(drawing.b.price), decimals);
        format!("{timeframe:?} {role} {low}–{high}")
    }

    fn drawing_anchors(
        chart: &Chart<'_>,
        points: &[args::Anchor],
        tool: Tool,
        preserve_timestamps: bool,
    ) -> Result<Vec<Anchor>, ToolResult> {
        let count = tool.points();
        if points.len() != count {
            return Err(error("invalid_args", &format!("anchors must contain exactly {count} points for {tool:?}")));
        }
        let (first, last) =
            chart.bars.first().zip(chart.bars.last()).ok_or_else(|| {
                error("invalid_args", "anchors require loaded bars; call read_chart after data loads")
            })?;
        let magnitude =
            chart.bars.iter().flat_map(|b| [b.low.abs(), b.high.abs()]).filter(|p| p.is_finite()).fold(1., f64::max);
        let bound = (magnitude * 100.).min(1e15);
        // Keep caller indices attached while normalizing chronological order.
        let mut points: Vec<_> = points.iter().enumerate().collect();
        if tool == Tool::Rectangle && points[0].1.time > points[1].1.time {
            points.swap(0, 1);
        }
        let mut out = Vec::new();
        for (position, &(i, p)) in points.iter().enumerate() {
            if !p.price.is_finite() || p.price.abs() > bound {
                return Err(error(
                    "invalid_args",
                    &format!("anchors[{i}].price must be finite and within -{bound}–{bound}; use analyze bounds"),
                ));
            }
            // Rectangle end extensions are harmless; clip them to the loaded last bar.
            let time = if tool == Tool::Rectangle && position == 1 && !preserve_timestamps {
                p.time.min(last.ts)
            } else {
                p.time
            };
            if time < first.ts || time > last.ts || time > chrono::Utc::now().timestamp() {
                let date = |ts| {
                    chrono::DateTime::from_timestamp(ts, 0).map(|t| t.to_rfc3339()).unwrap_or_else(|| ts.to_string())
                };
                return Err(error(
                    "invalid_args",
                    &format!(
                        "anchors[{i}].time must be within the loaded range {}–{} (Unix {}–{}); read_chart for valid anchors",
                        date(first.ts),
                        date(last.ts),
                        first.ts,
                        last.ts
                    ),
                ));
            }
            let ts = if tool == Tool::Rectangle && !preserve_timestamps {
                let index = chart.bars.partition_point(|bar| bar.ts < time);
                match (index.checked_sub(1).and_then(|i| chart.bars.get(i)), chart.bars.get(index)) {
                    (Some(a), Some(b)) => {
                        if time - a.ts <= b.ts - time {
                            a.ts
                        } else {
                            b.ts
                        }
                    }
                    (Some(a), None) => a.ts,
                    (_, Some(b)) => b.ts,
                    _ => unreachable!(),
                }
            } else {
                time
            };
            out.push(Anchor { ts, price: p.price });
        }
        if tool == Tool::Rectangle {
            if out[0].price == out[1].price {
                return Err(error(
                    "invalid_args",
                    "anchors[1].price must differ from anchors[0].price; use analyze low and high for a non-zero zone",
                ));
            }
            if out[0].price > out[1].price {
                let price = out[0].price;
                out[0].price = out[1].price;
                out[1].price = price;
            }
            if out[0].ts == out[1].ts {
                return Err(error(
                    "invalid_args",
                    "anchors times snap to the same bar; choose two distinct loaded bars from read_chart",
                ));
            }
        }
        Ok(out)
    }
}

/// Bridge for workers using the existing market-data interface. Price basis is explicit:
/// MarketData::history itself does not expose it. Unknown is safer than assuming adjusted.
pub struct MarketDataHost<M> {
    pub market: M,
    pub price_basis: PriceBasis,
    pub session: Session,
}

impl<M: MarketData> DataHost for MarketDataHost<M> {
    fn history<'a>(
        &'a self,
        symbol: &'a str,
        tf: Timeframe,
        end: Option<i64>,
        limit: usize,
    ) -> HostFuture<'a, History> {
        Box::pin(async move {
            let bars = self.market.history(symbol, tf, end, limit).await.map_err(|e| e.to_string())?;
            Ok(History {
                bars,
                price_basis: self.price_basis,
                session: self.session,
                instrument: self.market.crypto_instrument(symbol),
                source: Some(self.market.name().into()),
                simulated: matches!(self.market.name(), "sample" | "offline").then_some(true),
            })
        })
    }

    fn search<'a>(&'a self, query: &'a str) -> HostFuture<'a, Vec<SearchHit>> {
        Box::pin(async move { self.market.search(query).await.map_err(|e| e.to_string()) })
    }

    fn quotes<'a>(&'a self, symbols: &'a [String]) -> HostFuture<'a, Vec<Quote>> {
        Box::pin(async move { self.market.quotes(symbols).await.map_err(|e| e.to_string()) })
    }
}

#[cfg(test)]
mod tests {
    use super::edits::*;
    use crate::{
        Content,
        args::{self, LiveArgs},
    };
    use tr_core::{Bar, Timeframe};

    #[test]
    fn precision_replaces_only_prior_agent_sr_same_symbol_and_timeframe() {
        use tr_chart::drawings::{Anchor, Author, Drawing, Tool};
        let bars = [Bar::new(86400, 10., 12., 8., 11., 1.), Bar::new(172800, 10., 12., 8., 11., 1.)];
        let mut old = Drawing::new(Tool::Rectangle, Anchor { ts: 86400, price: 8. }, Anchor { ts: 172800, price: 12. });
        old.created_by = Author::Agent;
        old.turn_id = Some("previous".into());
        old.symbol = Some("AAA US Equity".into());
        old.source_timeframe = Some(Timeframe::D1);
        old.zone_role = Some(args::ZoneRole::Support);
        let mut kept = old.clone();
        kept.id = tr_chart::drawings::next_id();
        kept.created_by = Author::User;
        let mut h4 = old.clone();
        h4.id = tr_chart::drawings::next_id();
        h4.source_timeframe = Some(Timeframe::H4);
        let mut other = old.clone();
        other.id = tr_chart::drawings::next_id();
        other.symbol = Some("BBB US Equity".into());
        let mut drawings = vec![old.clone(), kept.clone(), h4.clone(), other.clone()];
        // A full chart must permit replacement; legacy labeled rays predate the marker.
        for _ in 0..96 {
            let mut legacy = old.clone();
            legacy.id = tr_chart::drawings::next_id();
            legacy.tool = Tool::Ray;
            legacy.zone_role = None;
            legacy.text = "D1 rising support".into();
            drawings.push(legacy);
        }
        let chart = Chart {
            uid: 1,
            symbol: "AAA US Equity",
            timeframe: Timeframe::D1,
            price_decimals: 2,
            bars: &bars,
            drawings: &drawings,
            indicators: &[],
            comparisons: &[],
        };
        let mut a = zone_args("support");
        a.analysis_ref = Some(args::AnalysisRef { analysis_id: "test".into(), item_id: "zone-0".into() });
        a.extend_right = true;
        a.anchors[0].time += 3600; // Analysis timestamps are exact, not snapped to chart bars.
        let mut session = Session::default();
        let Plan::Drawings(next, id) = session.plan(&chart, "new", LiveArgs::CreateDrawing(a.clone())).unwrap() else {
            panic!()
        };
        assert!(!next.iter().any(|d| d.id == old.id));
        assert_eq!(next.len(), 100, "zone replacement must retain prior trendlines, even at the object limit");
        for d in [kept, h4, other] {
            assert!(next.contains(&d));
        }
        assert!(next.iter().any(|d| d.id == id && d.extend_right && d.technical_analysis && d.a.ts == 90000));
        let Plan::Drawings(next2, _) =
            session.plan(&Chart { drawings: &next, ..chart }, "new", LiveArgs::CreateDrawing(a)).unwrap()
        else {
            panic!()
        };
        assert!(next2.iter().any(|d| d.id == id), "same-turn drawings must not replace one another");
        let mut line = zone_args("rising support");
        line.kind = args::DrawingKind::Ray;
        line.zone_role = None;
        line.analysis_ref = Some(args::AnalysisRef { analysis_id: "test".into(), item_id: "line-0".into() });
        let Plan::Drawings(next3, _) =
            session.plan(&Chart { drawings: &next2, ..chart }, "lines", LiveArgs::CreateDrawing(line)).unwrap()
        else {
            panic!()
        };
        assert!(next3.iter().any(|d| d.id == id), "line replacement preserves prior zones");
        assert_eq!(next3.iter().filter(|d| d.tool == Tool::Ray).count(), 1, "legacy lines are replaced by lines only");
    }

    #[test]
    fn analysis_refs_enforce_current_chart_and_per_turn_caps() {
        let bars = [Bar::new(86400, 10., 12., 8., 11., 1.), Bar::new(172800, 10., 12., 8., 11., 1.)];
        let chart = Chart {
            uid: 1,
            symbol: "AAA US Equity",
            timeframe: Timeframe::D1,
            price_decimals: 2,
            bars: &bars,
            drawings: &[],
            indicators: &[],
            comparisons: &[],
        };
        let mut session = Session::default();
        let mut zone = zone_args("support");
        zone.analysis_ref = Some(args::AnalysisRef { analysis_id: "cached".into(), item_id: "zone-0".into() });
        zone.source_timeframe = Some("4H".into());
        let error = session.plan(&chart, "turn", LiveArgs::CreateDrawing(zone.clone())).err().unwrap();
        assert!(format!("{error:?}").contains("re-run analyze"));
        zone.source_timeframe = Some("1D".into());
        zone.symbol = "BBB".into();
        let error = session.plan(&chart, "turn", LiveArgs::CreateDrawing(zone.clone())).err().unwrap();
        assert!(format!("{error:?}").contains("re-run analyze"));
        zone.symbol = "AAA".into();
        let mut drawings = vec![];
        for _ in 0..6 {
            let Plan::Drawings(next, _) = session
                .plan(&Chart { drawings: &drawings, ..chart }, "turn", LiveArgs::CreateDrawing(zone.clone()))
                .unwrap()
            else {
                panic!()
            };
            drawings = next;
        }
        assert!(session.plan(&chart, "turn", LiveArgs::CreateDrawing(zone.clone())).is_err());
        for rising in [true, false] {
            let mut line = zone.clone();
            line.kind = args::DrawingKind::Ray;
            line.zone_role = None;
            if !rising {
                line.anchors[0].price = 12.;
                line.anchors[1].price = 8.;
            }
            for _ in 0..2 {
                assert!(session.plan(&chart, "turn", LiveArgs::CreateDrawing(line.clone())).is_ok());
            }
            assert!(session.plan(&chart, "turn", LiveArgs::CreateDrawing(line)).is_err());
        }
        assert!(session.plan(&chart, "next", LiveArgs::CreateDrawing(zone)).is_ok());
    }

    #[test]
    fn agent_labels_get_compact_timeframes_and_zone_roles_roundtrip() {
        let bars = [Bar::new(86400, 10., 12., 8., 11., 1.), Bar::new(172800, 10., 12., 8., 11., 1.)];
        let mut session = Session::default();
        for timeframe in Timeframe::ALL {
            let chart = Chart {
                uid: 1,
                symbol: "AAA US Equity",
                price_decimals: 2,
                timeframe,
                bars: &bars,
                drawings: &[],
                indicators: &[],
                comparisons: &[],
            };
            for (label, expected) in [
                (Some("Support"), format!("{timeframe:?} Support")),
                (Some("W1 Support"), "W1 Support".into()),
                (Some("[H4] Resistance"), "[H4] Resistance".into()),
                (Some("1W Support"), "1W Support".into()),
                (Some("Support W1"), "Support W1".into()),
                (None, String::new()),
            ] {
                for role in ["support", "resistance"] {
                    let args = serde_json::from_value(serde_json::json!({
                        "symbol":"AAA", "kind":"rectangle", "label":label, "zone_role":role,
                        "anchors":[{"time":86400,"price":8.},{"time":172800,"price":12.}]
                    }))
                    .unwrap();
                    let Plan::Drawings(drawings, _) = session
                        .plan(&chart, &format!("{timeframe:?}-{label:?}-{role}"), LiveArgs::CreateDrawing(args))
                        .unwrap()
                    else {
                        panic!("drawing")
                    };
                    let drawing = &drawings[0];
                    let expected =
                        if label.is_none() { format!("{timeframe:?} {role} 8.00–12.00") } else { expected.clone() };
                    assert_eq!(drawing.text, expected);
                    assert_eq!(drawing.created_by, tr_chart::drawings::Author::Agent);
                    assert!(drawing.color.is_none());
                    assert_eq!(serde_json::to_value(drawing.zone_role).unwrap(), role);
                    let saved = serde_json::to_value(drawing).unwrap();
                    assert_eq!(serde_json::from_value::<tr_chart::drawings::Drawing>(saved).unwrap(), *drawing);
                }
            }
        }
        let mut user = tr_chart::drawings::Drawing::new(
            tr_chart::drawings::Tool::Text,
            tr_chart::drawings::Anchor { ts: 86400, price: 10. },
            tr_chart::drawings::Anchor { ts: 86400, price: 10. },
        );
        user.text = "My label".into();
        user.sanitize();
        assert_eq!(user.text, "My label", "user drawings never enter the agent label path");
        assert!(user.zone_role.is_none());
    }

    fn zone_args(label: &str) -> args::CreateDrawing {
        serde_json::from_value(serde_json::json!({
            "symbol":"AAA", "kind":"rectangle", "label":label, "zone_role":"support",
            "anchors":[{"time":86400,"price":8.},{"time":172800,"price":12.}]
        }))
        .unwrap()
    }

    #[test]
    fn review_auto_zone_label_follows_anchor_update_after_precision_change() {
        let bars = [Bar::new(86400, 10., 12., 8., 11., 1.), Bar::new(172800, 10., 12., 8., 11., 1.)];
        let chart = Chart {
            uid: 1,
            symbol: "AAA US Equity",
            price_decimals: 4,
            timeframe: Timeframe::H4,
            bars: &bars,
            drawings: &[],
            indicators: &[],
            comparisons: &[],
        };
        let mut session = Session::default();
        let Plan::Drawings(created, id) = session.plan(&chart, "turn", LiveArgs::CreateDrawing(zone_args(""))).unwrap()
        else {
            panic!()
        };
        assert_eq!(created[0].text, "H4 support 8.0000–12.0000");
        assert!(created[0].auto_label);
        let mut saved = serde_json::to_value(&created[0]).unwrap();
        assert_eq!(saved["auto_label"], true);
        let restored: tr_chart::drawings::Drawing = serde_json::from_value(saved.clone()).unwrap();
        assert!(restored.auto_label);
        saved.as_object_mut().unwrap().remove("auto_label");
        let legacy: tr_chart::drawings::Drawing = serde_json::from_value(saved).unwrap();
        assert!(!legacy.auto_label);
        assert!(serde_json::to_value(legacy).unwrap().get("auto_label").is_none());
        // User zooms in: the axis now shows five decimals.
        let chart = Chart { drawings: &created, price_decimals: 5, ..chart };
        let update = args::UpdateDrawing {
            symbol: "AAA".into(),
            id,
            chart_id: None,
            all_symbols: false,
            anchors: Some(vec![args::Anchor { time: 86400, price: 9. }, args::Anchor { time: 172800, price: 11. }]),
            label: None,
        };
        let Plan::Drawings(updated, _) = session.plan(&chart, "turn", LiveArgs::UpdateDrawing(update.clone())).unwrap()
        else {
            panic!()
        };
        assert_eq!(updated[0].text, "H4 support 9.00000–11.00000");
        assert!(updated[0].auto_label);
        // Even text that looks exactly like generated text becomes explicitly agent-owned.
        let Plan::Drawings(custom, _) = session
            .plan(
                &chart,
                "turn",
                LiveArgs::UpdateDrawing(args::UpdateDrawing {
                    label: Some(created[0].text.clone()),
                    anchors: None,
                    ..update.clone()
                }),
            )
            .unwrap()
        else {
            panic!()
        };
        assert!(!custom[0].auto_label);
        let chart = Chart { drawings: &custom, ..chart };
        let Plan::Drawings(moved, _) = session.plan(&chart, "turn", LiveArgs::UpdateDrawing(update)).unwrap() else {
            panic!()
        };
        assert_eq!(moved[0].text, custom[0].text);
        let Plan::Drawings(custom_created, _) =
            session.plan(&chart, "turn", LiveArgs::CreateDrawing(zone_args("My support"))).unwrap()
        else {
            panic!()
        };
        assert!(!custom_created.last().unwrap().auto_label);
    }

    #[test]
    fn review_rectangle_rejection_names_the_agents_own_anchor_after_swap() {
        let bars = [Bar::new(86400, 10., 12., 8., 11., 1.), Bar::new(172800, 10., 12., 8., 11., 1.)];
        let chart = Chart {
            uid: 1,
            symbol: "AAA US Equity",
            price_decimals: 2,
            timeframe: Timeframe::H4,
            bars: &bars,
            drawings: &[],
            indicators: &[],
            comparisons: &[],
        };
        let mut args = zone_args("");
        // anchors[1] is the bad one (before the loaded range).
        args.anchors = vec![args::Anchor { time: 172800, price: 8. }, args::Anchor { time: 10, price: 12. }];
        let result = Session::default().plan(&chart, "turn", LiveArgs::CreateDrawing(args.clone())).err().unwrap();
        assert!(
            matches!(&result.content[0], Content::Text(text) if text.contains("anchors[1].time")),
            "{:?}",
            result.content
        );
        args.anchors[1].time = 86400;
        args.anchors[0].price = 1e16;
        let result = Session::default().plan(&chart, "turn", LiveArgs::CreateDrawing(args)).err().unwrap();
        assert!(matches!(&result.content[0], Content::Text(text) if text.contains("anchors[0].price")));
    }

    #[test]
    fn update_drawing_label_keeps_timeframe_prefix() {
        let bars = [Bar::new(86400, 10., 12., 8., 11., 1.), Bar::new(172800, 10., 12., 8., 11., 1.)];
        let chart = Chart {
            uid: 1,
            symbol: "AAA US Equity",
            price_decimals: 2,
            timeframe: Timeframe::W1,
            bars: &bars,
            drawings: &[],
            indicators: &[],
            comparisons: &[],
        };
        let mut session = Session::default();
        let Plan::Drawings(drawings, id) =
            session.plan(&chart, "turn", LiveArgs::CreateDrawing(zone_args("Support"))).unwrap()
        else {
            panic!("drawing")
        };
        assert_eq!(drawings[0].text, "W1 Support");
        let chart = Chart { drawings: &drawings, ..chart };
        let Plan::Drawings(updated, _) = session
            .plan(
                &chart,
                "turn",
                LiveArgs::UpdateDrawing(args::UpdateDrawing {
                    symbol: "AAA".into(),
                    id,
                    chart_id: None,
                    all_symbols: false,
                    anchors: None,
                    label: Some("Support retest".into()),
                }),
            )
            .unwrap()
        else {
            panic!("drawing")
        };
        assert_eq!(updated[0].text, "W1 Support retest");
        let chart = Chart { drawings: &updated, ..chart };
        let Plan::Drawings(repeated, _) = session
            .plan(
                &chart,
                "turn",
                LiveArgs::UpdateDrawing(args::UpdateDrawing {
                    symbol: "AAA".into(),
                    id,
                    chart_id: None,
                    all_symbols: false,
                    anchors: None,
                    label: Some(updated[0].text.clone()),
                }),
            )
            .unwrap()
        else {
            panic!("drawing")
        };
        assert_eq!(repeated[0].text, "W1 Support retest");
    }

    #[test]
    fn source_timeframe_defaults_precise_zone_labels_and_idempotent_updates() {
        let bars = [Bar::new(86400, 10., 12., 8., 11., 1.), Bar::new(172800, 10., 12., 8., 11., 1.)];
        for decimals in [0, 2, 5] {
            for label in [None, Some("\u{0}  "), Some("Demand")] {
                let mut session = Session::default();
                let chart = Chart {
                    uid: 1,
                    symbol: "AAA US Equity",
                    timeframe: Timeframe::H4,
                    price_decimals: decimals,
                    bars: &bars,
                    drawings: &[],
                    indicators: &[],
                    comparisons: &[],
                };
                let mut args = zone_args("");
                args.label = label.map(str::to_owned);
                args.source_timeframe = Some("1W".into());
                let Plan::Drawings(drawings, id) = session.plan(&chart, "turn", LiveArgs::CreateDrawing(args)).unwrap()
                else {
                    panic!()
                };
                let expected = if label == Some("Demand") {
                    "W1 Demand".into()
                } else {
                    format!("W1 support {:.*}–{:.*}", decimals, 8., decimals, 12.)
                };
                assert_eq!(drawings[0].text, expected);
                assert_eq!(drawings[0].source_timeframe, Some(Timeframe::W1));
                let mut current = drawings;
                for _ in 0..2 {
                    let chart = Chart { drawings: &current, timeframe: Timeframe::D1, ..chart };
                    let args = args::UpdateDrawing {
                        symbol: "AAA".into(),
                        id,
                        chart_id: None,
                        all_symbols: false,
                        anchors: None,
                        label: Some(current[0].text.clone()),
                    };
                    let Plan::Drawings(updated, _) =
                        session.plan(&chart, "turn", LiveArgs::UpdateDrawing(args)).unwrap()
                    else {
                        panic!()
                    };
                    assert_eq!(updated[0].text, expected);
                    assert!(!updated[0].auto_label, "agent-supplied text clears automatic labeling");
                    current = updated;
                }
                if label != Some("Demand") {
                    let chart = Chart { drawings: &current, ..chart };
                    let args = args::UpdateDrawing {
                        symbol: "AAA".into(),
                        id,
                        chart_id: None,
                        all_symbols: false,
                        anchors: Some(vec![
                            args::Anchor { time: 86400, price: 9. },
                            args::Anchor { time: 172800, price: 13. },
                        ]),
                        label: None,
                    };
                    let Plan::Drawings(updated, _) =
                        session.plan(&chart, "turn", LiveArgs::UpdateDrawing(args)).unwrap()
                    else {
                        panic!()
                    };
                    assert_eq!(updated[0].text, expected, "explicit text stays unchanged on anchor-only updates");
                    current = updated;
                }
                let chart = Chart { drawings: &current, ..chart };
                let args = args::UpdateDrawing {
                    symbol: "AAA".into(),
                    id,
                    chart_id: None,
                    all_symbols: false,
                    anchors: None,
                    label: Some("Retest".into()),
                };
                let Plan::Drawings(updated, _) = session.plan(&chart, "turn", LiveArgs::UpdateDrawing(args)).unwrap()
                else {
                    panic!()
                };
                assert_eq!(updated[0].text, "W1 Retest");
            }
        }
    }

    #[test]
    fn rectangles_normalize_harmless_anchors_and_report_each_geometry_rejection() {
        let bars = [
            Bar::new(86400, 10., 12., 8., 11., 1.),
            Bar::new(172800, 10., 12., 8., 11., 1.),
            Bar::new(259200, 10., 12., 8., 11., 1.),
        ];
        let chart = Chart {
            uid: 1,
            symbol: "AAA US Equity",
            timeframe: Timeframe::H4,
            price_decimals: 2,
            bars: &bars,
            drawings: &[],
            indicators: &[],
            comparisons: &[],
        };
        let mut session = Session::default();
        for (times, prices, expected) in [
            ([172800, 86400], [12., 8.], [86400, 172800]),
            ([90000, 170000], [12., 8.], [86400, 172800]),
            ([86400, i64::MAX], [8., 12.], [86400, 259200]),
        ] {
            let mut args = zone_args("");
            args.anchors = (0..2).map(|i| args::Anchor { time: times[i], price: prices[i] }).collect();
            let Plan::Drawings(drawings, _) =
                session.plan(&chart, "normalized", LiveArgs::CreateDrawing(args)).unwrap()
            else {
                panic!()
            };
            assert_eq!([drawings[0].a.ts, drawings[0].b.ts], expected);
            assert_eq!([drawings[0].a.price, drawings[0].b.price], [8., 12.]);
            assert_eq!(drawings[0].text, "H4 support 8.00–12.00");
        }
        for (times, prices, field) in [
            ([0, 172800], [8., 12.], "anchors[0].time must be within the loaded range"),
            ([0, 1], [8., 12.], "loaded range"),
            ([86400, 172800], [8., 8.], "anchors[1].price must differ"),
            ([86400, 86401], [8., 12.], "same bar"),
            ([86400, 172800], [f64::NAN, 12.], "anchors[0].price must be finite"),
            ([86400, 172800], [8., f64::INFINITY], "anchors[1].price must be finite"),
            ([86400, 172800], [8., 1201.], "within -1200–1200"),
        ] {
            let mut args = zone_args("");
            args.anchors = (0..2).map(|i| args::Anchor { time: times[i], price: prices[i] }).collect();
            let result = session.plan(&chart, "invalid", LiveArgs::CreateDrawing(args)).err().expect("reject");
            assert!(
                matches!(&result.content[0], Content::Text(text) if text.contains(field)),
                "{field}: {:?}",
                result.content
            );
        }
        let mut args = zone_args("");
        args.anchors.pop();
        let result = session.plan(&chart, "invalid", LiveArgs::CreateDrawing(args)).err().unwrap();
        assert!(matches!(&result.content[0], Content::Text(text) if text.contains("exactly 2")));
        let result = session
            .plan(&Chart { bars: &[], ..chart }, "invalid", LiveArgs::CreateDrawing(zone_args("")))
            .err()
            .unwrap();
        assert!(matches!(&result.content[0], Content::Text(text) if text.contains("require loaded bars")));
        let mut args = zone_args("");
        args.kind = args::DrawingKind::TrendLine;
        let result = session.plan(&chart, "invalid", LiveArgs::CreateDrawing(args)).err().unwrap();
        assert!(
            matches!(&result.content[0], Content::Text(text) if text.contains("zone_role requires kind=rectangle"))
        );
        let mut args = zone_args("");
        args.source_timeframe = Some("invalid".into());
        let result = session.plan(&chart, "invalid", LiveArgs::CreateDrawing(args)).err().unwrap();
        assert!(matches!(&result.content[0], Content::Text(text) if text.contains("source_timeframe")));
        let mut args = zone_args("");
        args.symbol = "BBB".into();
        let result = session.plan(&chart, "invalid", LiveArgs::CreateDrawing(args)).err().unwrap();
        assert!(
            matches!(&result.content[0], Content::Text(text) if text.contains("symbol must match the loaded chart AAA"))
        );
    }

    #[test]
    fn lowercase_timeframe_in_agent_label_is_not_double_prefixed() {
        let bars = [Bar::new(86400, 10., 12., 8., 11., 1.), Bar::new(172800, 10., 12., 8., 11., 1.)];
        let chart = Chart {
            uid: 1,
            symbol: "AAA US Equity",
            price_decimals: 2,
            timeframe: Timeframe::H4,
            bars: &bars,
            drawings: &[],
            indicators: &[],
            comparisons: &[],
        };
        let mut session = Session::default();
        for label in ["4h support", "h4 support", "1w support"] {
            let Plan::Drawings(drawings, _) =
                session.plan(&chart, label, LiveArgs::CreateDrawing(zone_args(label))).unwrap()
            else {
                panic!("drawing")
            };
            assert_eq!(drawings[0].text, label);
        }
    }

    #[test]
    fn timeframe_tokens_are_case_insensitive_standalone_and_not_share_quantities() {
        for tf in Timeframe::ALL {
            for token in [format!("{tf:?}"), tf.label().into()] {
                for token in [token.to_ascii_lowercase(), token.to_ascii_uppercase()] {
                    let label = format!("Support [{token}], retest");
                    assert_eq!(timeframe_label(&label, Timeframe::W1), label);
                    let embedded = format!("Support x{token}x");
                    assert_eq!(timeframe_label(&embedded, Timeframe::W1), format!("W1 {embedded}"));
                }
            }
        }
        for label in ["1M shares", "1m SHARES", "3M shares sold", "1M, shares", "1Mshare", "xH4", "H4x"] {
            assert_eq!(timeframe_label(label, Timeframe::W1), format!("W1 {label}"));
        }
        assert_eq!(timeframe_label("1M shares at [h4] support", Timeframe::W1), "1M shares at [h4] support");
    }

    #[test]
    fn long_agent_labels_reserve_prefix_within_sanitize_cap() {
        for timeframe in Timeframe::ALL {
            let prefix = format!("{timeframe:?} ");
            let label = timeframe_label(&format!("\u{0}{}", "界".repeat(600)), timeframe);
            assert_eq!(label.chars().count(), 500);
            assert_eq!(label, format!("{prefix}{}", "界".repeat(500 - prefix.chars().count())));
            assert_eq!(tr_chart::drawings::text::sanitize(&label), label);
            assert_eq!(timeframe_label(&label, timeframe), label);
        }
        assert_eq!(timeframe_label(" \u{0} ", Timeframe::W1), "");
    }

    #[test]
    fn direct_hosts_reject_nonfinite_coordinates_and_wrong_update_anchor_count() {
        let bars = [Bar::new(86400, 10., 12., 8., 11., 1.), Bar::new(172800, 10., 12., 8., 11., 1.)];
        let chart = Chart {
            uid: 1,
            symbol: "AAA US Equity",
            price_decimals: 2,
            timeframe: Timeframe::D1,
            bars: &bars,
            drawings: &[],
            indicators: &[],
            comparisons: &[],
        };
        let mut session = Session::default();
        for price in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e100] {
            let result = session.plan(
                &chart,
                "turn",
                LiveArgs::CreateDrawing(args::CreateDrawing {
                    symbol: "AAA".into(),
                    kind: args::DrawingKind::Horizontal,
                    anchors: vec![args::Anchor { time: 86400, price }],
                    chart_id: None,
                    all_symbols: false,
                    label: None,
                    zone_role: None,
                    source_timeframe: None,
                    analysis_ref: None,
                    extend_left: false,
                    extend_right: false,
                    dashed: false,
                }),
            );
            let Err(result) = result else { panic!("invalid coordinates succeeded") };
            assert!(!result.success);
            assert!(matches!(&result.content[0], Content::Text(text) if text.contains("invalid_args")));
        }
        let plan = session
            .plan(
                &chart,
                "turn",
                LiveArgs::CreateDrawing(args::CreateDrawing {
                    symbol: "AAA".into(),
                    kind: args::DrawingKind::TrendLine,
                    anchors: vec![args::Anchor { time: 86400, price: 10. }, args::Anchor { time: 172800, price: 11. }],
                    chart_id: None,
                    all_symbols: false,
                    label: None,
                    zone_role: None,
                    source_timeframe: None,
                    analysis_ref: None,
                    extend_left: false,
                    extend_right: false,
                    dashed: false,
                }),
            )
            .unwrap();
        let Plan::Drawings(drawings, id) = plan else { panic!("drawing") };
        let chart = Chart { drawings: &drawings, ..chart };
        let result = session.plan(
            &chart,
            "turn",
            LiveArgs::UpdateDrawing(args::UpdateDrawing {
                symbol: "AAA".into(),
                id,
                chart_id: None,
                all_symbols: false,
                anchors: Some(vec![args::Anchor { time: 86400, price: 10. }]),
                label: None,
            }),
        );
        assert!(result.is_err());
        assert!(drawings[0].agent_highlight());
        let mut kept = drawings[0].clone();
        kept.created_by = tr_chart::drawings::Author::User;
        assert!(!kept.agent_highlight());
    }

    #[test]
    fn study_ids_are_chart_qualified_and_removed_ids_are_never_reused() {
        let mut session = Session::default();
        let chart = Chart {
            uid: 8,
            symbol: "AAA US Equity",
            price_decimals: 2,
            timeframe: Timeframe::D1,
            bars: &[],
            drawings: &[],
            indicators: &[],
            comparisons: &[],
        };
        let add = || LiveArgs::AddIndicator(args::AddIndicator { code: "RSI".into(), chart_id: None, params: vec![] });
        let Plan::Indicators(studies, first) = session.plan(&chart, "turn", add()).unwrap() else { panic!("study") };
        let settings = studies[0].chart.as_ref().unwrap();
        assert!(settings.agent_highlight());
        assert_eq!(settings.turn_id.as_deref(), Some("turn"));
        let chart = Chart { indicators: &studies, ..chart };
        assert!(
            session
                .plan(
                    &chart,
                    "turn",
                    LiveArgs::RemoveIndicator(args::RemoveIndicator {
                        id: format!("9:{}", settings.id),
                        chart_id: None
                    })
                )
                .is_err()
        );
        let Plan::Indicators(empty, _) = session
            .plan(
                &chart,
                "turn",
                LiveArgs::RemoveIndicator(args::RemoveIndicator { id: first.clone(), chart_id: None }),
            )
            .unwrap()
        else {
            panic!("study")
        };
        let chart = Chart { indicators: &empty, ..chart };
        let Plan::Indicators(_, second) = session.plan(&chart, "turn", add()).unwrap() else { panic!("study") };
        assert_ne!(first, second);
        assert!(
            session
                .plan(&chart, "turn", LiveArgs::RemoveIndicator(args::RemoveIndicator { id: first, chart_id: None }))
                .is_err()
        );
    }

    #[test]
    fn live_owned_comparisons_share_the_chart_object_limit() {
        let mut session = Session::default();
        let chart = Chart {
            uid: 1,
            symbol: "AAA US Equity",
            price_decimals: 2,
            timeframe: Timeframe::D1,
            bars: &[],
            drawings: &[],
            indicators: &[],
            comparisons: &[],
        };
        session
            .plan(
                &chart,
                "turn",
                LiveArgs::Compare(args::Compare {
                    symbol: "MSFT".into(),
                    action: args::CompareAction::Add,
                    chart_id: None,
                }),
            )
            .unwrap();
        let drawings: Vec<_> = (0..99)
            .map(|_| {
                let anchor = tr_chart::drawings::Anchor { ts: 86400, price: 10. };
                let mut drawing =
                    tr_chart::drawings::Drawing::new(tr_chart::drawings::Tool::Horizontal, anchor, anchor);
                drawing.created_by = tr_chart::drawings::Author::Agent;
                drawing
            })
            .collect();
        let comparisons = [Comparison {
            symbol: "MSFT US Equity".into(),
            created_by: tr_chart::drawings::Author::Agent,
            turn_id: Some("turn".into()),
        }];
        let chart = Chart { drawings: &drawings, comparisons: &comparisons, ..chart };
        let add = || LiveArgs::AddIndicator(args::AddIndicator { code: "RSI".into(), chart_id: None, params: vec![] });
        let Err(result) = session.plan(&chart, "next", add()) else { panic!("chart limit") };
        assert!(matches!(&result.content[0], Content::Text(text) if text.contains("chart_object_limit")));
        let chart = Chart { comparisons: &[], ..chart };
        assert!(session.plan(&chart, "next", add()).is_ok());
    }

    #[test]
    fn observed_ordinary_study_ids_are_reserved_after_removal() {
        let mut session = Session::default();
        let mut spec = tr_core::indicators::registry::Spec::new("RSI", &[]).unwrap();
        let observed = 10_000;
        spec.chart = Some(Box::new(tr_core::indicators::study::Settings::new(observed, &spec)));
        let studies = [spec];
        let chart = Chart {
            uid: 1,
            symbol: "AAA US Equity",
            price_decimals: 2,
            timeframe: Timeframe::D1,
            bars: &[],
            drawings: &[],
            indicators: &studies,
            comparisons: &[],
        };
        session.observe(&chart);
        let chart = Chart { indicators: &[], ..chart };
        let Plan::Indicators(studies, _) = session
            .plan(
                &chart,
                "turn",
                LiveArgs::AddIndicator(args::AddIndicator { code: "RSI".into(), chart_id: None, params: vec![] }),
            )
            .unwrap()
        else {
            panic!("study")
        };
        assert!(studies[0].chart.as_ref().unwrap().id > observed);
    }

    #[test]
    fn study_allocation_uses_free_space_even_with_an_observed_maximum_u32_id() {
        let mut session = Session::default();
        let mut spec = tr_core::indicators::registry::Spec::new("SMA", &[]).unwrap();
        spec.chart = Some(Box::new(tr_core::indicators::study::Settings::new(u32::MAX, &spec)));
        let studies = [spec];
        let chart = Chart {
            uid: 42,
            symbol: "AAA US Equity",
            price_decimals: 2,
            timeframe: Timeframe::D1,
            bars: &[],
            drawings: &[],
            indicators: &studies,
            comparisons: &[],
        };
        let Plan::Indicators(studies, _) = session
            .plan(
                &chart,
                "turn",
                LiveArgs::AddIndicator(args::AddIndicator { code: "RSI".into(), chart_id: None, params: vec![] }),
            )
            .unwrap()
        else {
            panic!("study")
        };
        assert_eq!(studies[1].chart.as_ref().unwrap().id, 1);
    }

    #[test]
    fn drawing_ownership_requires_the_original_turn_and_chart_record() {
        use tr_chart::drawings::Author;
        let bars = [Bar::new(86400, 10., 12., 8., 11., 1.)];
        let chart = Chart {
            uid: 1,
            symbol: "AAA US Equity",
            price_decimals: 2,
            timeframe: Timeframe::D1,
            bars: &bars,
            drawings: &[],
            indicators: &[],
            comparisons: &[],
        };
        let mut session = Session::default();
        let Plan::Drawings(drawings, id) = session
            .plan(
                &chart,
                "current",
                LiveArgs::CreateDrawing(args::CreateDrawing {
                    symbol: "AAA".into(),
                    kind: args::DrawingKind::Horizontal,
                    anchors: vec![args::Anchor { time: 86400, price: 10. }],
                    chart_id: None,
                    all_symbols: false,
                    label: None,
                    zone_role: None,
                    source_timeframe: None,
                    analysis_ref: None,
                    extend_left: false,
                    extend_right: false,
                    dashed: false,
                }),
            )
            .unwrap()
        else {
            panic!("drawing")
        };
        for turn in [Some("previous-session"), None] {
            let mut replaced = drawings.clone();
            replaced[0].turn_id = turn.map(str::to_owned);
            let chart = Chart { drawings: &replaced, ..chart };
            assert!(
                session
                    .plan(
                        &chart,
                        "next",
                        LiveArgs::DeleteDrawing(args::DeleteDrawing {
                            symbol: "AAA".into(),
                            id,
                            chart_id: None,
                            all_symbols: false
                        })
                    )
                    .is_err()
            );
        }
        let foreign = Chart { uid: 2, drawings: &drawings, ..chart };
        assert!(
            session
                .plan(
                    &foreign,
                    "next",
                    LiveArgs::DeleteDrawing(args::DeleteDrawing {
                        symbol: "AAA".into(),
                        id,
                        chart_id: None,
                        all_symbols: false
                    })
                )
                .is_err()
        );
        assert_eq!(drawings[0].created_by, Author::Agent);
    }

    #[test]
    fn agent_cannot_remove_user_readded_comparison() {
        let mut session = Session::default();
        let chart = Chart {
            uid: 1,
            symbol: "AAA US Equity",
            price_decimals: 2,
            timeframe: Timeframe::D1,
            bars: &[],
            drawings: &[],
            indicators: &[],
            comparisons: &[],
        };
        let compare = |action| LiveArgs::Compare(args::Compare { symbol: "MSFT".into(), action, chart_id: None });
        session.plan(&chart, "t1", compare(args::CompareAction::Add)).unwrap();
        // User removes the agent comparison, then adds MSFT again themselves.
        let observed: [Comparison; 0] = [];
        session.observe(&Chart { comparisons: &observed, ..chart });
        let user = [Comparison::from("MSFT US Equity")];
        let chart = Chart { comparisons: &user, ..chart };
        assert!(session.plan(&chart, "t2", compare(args::CompareAction::Remove)).is_err());
    }

    #[test]
    fn ownership_is_not_transferred_to_a_different_object_with_a_reused_id() {
        let bars = [Bar::new(86400, 10., 12., 8., 11., 1.)];
        let mut session = Session::default();
        let chart = Chart {
            uid: 1,
            symbol: "AAA US Equity",
            price_decimals: 2,
            timeframe: Timeframe::D1,
            bars: &bars,
            drawings: &[],
            indicators: &[],
            comparisons: &[],
        };
        let Plan::Indicators(studies, id) = session
            .plan(
                &chart,
                "t1",
                LiveArgs::AddIndicator(args::AddIndicator { code: "RSI".into(), chart_id: None, params: vec![] }),
            )
            .unwrap()
        else {
            panic!("study")
        };
        // The user replaces the chart's studies with a template/layout saved in a
        // previous session whose Agent study happens to carry the same id.
        let mut old = tr_core::indicators::registry::Spec::new("SMA", &[]).unwrap();
        let mut settings = tr_core::indicators::study::Settings::new(studies[0].chart.as_ref().unwrap().id, &old);
        settings.created_by = tr_chart::drawings::Author::Agent;
        settings.turn_id = Some("previous-session".into());
        old.chart = Some(Box::new(settings));
        let replaced = [old];
        let chart = Chart { indicators: &replaced, ..chart };
        let result =
            session.plan(&chart, "t2", LiveArgs::RemoveIndicator(args::RemoveIndicator { id, chart_id: None }));
        assert!(result.is_err(), "previous-session Agent study was removable");
    }
}
