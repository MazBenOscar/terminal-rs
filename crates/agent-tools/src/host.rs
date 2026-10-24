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
                return Err(error("turn_object_limit", "At most 20 objects may be created per turn"));
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
                return Err(error("chart_object_limit", "At most 100 agent objects may exist per chart"));
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
                    self.can_create(chart, turn)?;
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
                        return Err(error("invalid_args", "zone_role is only supported for rectangles"));
                    }
                    let points = anchors(chart, &a.anchors, tool.points())?;
                    let mut drawing = Drawing::new(tool, points[0], *points.last().unwrap());
                    drawing.symbol = Some(chart.symbol.into());
                    drawing.all_symbols = a.all_symbols;
                    drawing.created_by = Author::Agent;
                    drawing.turn_id = Some(turn.into());
                    drawing.text = timeframe_label(a.label.as_deref().unwrap_or_default(), chart.timeframe);
                    drawing.zone_role = a.zone_role;
                    drawing.sanitize();
                    let id = drawing.id;
                    let mut next = chart.drawings.to_vec();
                    next.push(drawing);
                    self.own(chart.uid, format!("drawing:{id}"), turn);
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
                    if let Some(points) = a.anchors {
                        let points = anchors(chart, &points, drawing.tool.points())?;
                        drawing.a = points[0];
                        drawing.b = *points.last().unwrap();
                    }
                    if let Some(label) = a.label {
                        drawing.text = timeframe_label(&label, chart.timeframe);
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
            return Err(error("wrong_chart", "Explicit symbol does not match chart"));
        }
        Ok(())
    }

    fn anchors(chart: &Chart<'_>, points: &[args::Anchor], count: usize) -> Result<Vec<Anchor>, ToolResult> {
        let (first, last) = chart
            .bars
            .first()
            .zip(chart.bars.last())
            .ok_or_else(|| error("invalid_args", "Chart has no loaded data"))?;
        let magnitude =
            chart.bars.iter().flat_map(|b| [b.low.abs(), b.high.abs()]).filter(|p| p.is_finite()).fold(1., f64::max);
        let bound = (magnitude * 100.).min(1e15);
        if points.len() != count
            || points.iter().any(|p| {
                !p.price.is_finite()
                    || p.price.abs() > bound
                    || p.time < first.ts
                    || p.time > last.ts
                    || p.time > chrono::Utc::now().timestamp()
            })
        {
            return Err(error("invalid_args", "Invalid anchor count, price or timestamp outside loaded data"));
        }
        Ok(points.iter().map(|p| Anchor { ts: p.time, price: p.price }).collect())
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
    fn agent_labels_get_compact_timeframes_and_zone_roles_roundtrip() {
        let bars = [Bar::new(86400, 10., 12., 8., 11., 1.), Bar::new(172800, 10., 12., 8., 11., 1.)];
        let mut session = Session::default();
        for timeframe in Timeframe::ALL {
            let chart = Chart {
                uid: 1,
                symbol: "AAA US Equity",
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
    fn update_drawing_label_keeps_timeframe_prefix() {
        let bars = [Bar::new(86400, 10., 12., 8., 11., 1.), Bar::new(172800, 10., 12., 8., 11., 1.)];
        let chart = Chart {
            uid: 1,
            symbol: "AAA US Equity",
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
    fn lowercase_timeframe_in_agent_label_is_not_double_prefixed() {
        let bars = [Bar::new(86400, 10., 12., 8., 11., 1.), Bar::new(172800, 10., 12., 8., 11., 1.)];
        let chart = Chart {
            uid: 1,
            symbol: "AAA US Equity",
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
