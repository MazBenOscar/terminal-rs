//! Typed arguments and their wire schemas. Unknown fields fail closed at every level.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
pub use tr_chart::drawings::ZoneRole;

pub trait Arguments {
    fn schema() -> Value;
}

pub(crate) const MIN_TIMESTAMP: i64 = -62_135_596_800;
pub(crate) const MAX_TIMESTAMP: i64 = 253_370_764_799;

// Derive nullability from the Rust field type, not from whether it has a default.
trait OptionalArgument {
    const NULLABLE: bool;
}
impl<T> OptionalArgument for Option<T> {
    const NULLABLE: bool = true;
}
impl<T> OptionalArgument for Vec<T> {
    const NULLABLE: bool = false;
}
impl OptionalArgument for bool {
    const NULLABLE: bool = false;
}
fn optional_schema<T: OptionalArgument>(mut schema: Value) -> Value {
    if T::NULLABLE {
        schema["type"] = json!([schema["type"], "null"]);
        if let Some(choices) = schema.get_mut("enum").and_then(Value::as_array_mut) {
            choices.push(Value::Null);
        }
    }
    schema
}

macro_rules! field_schema {
    (String) => { json!({"type":"string", "minLength":1, "maxLength":2048, "pattern":"\\S"}) };
    (f64) => { json!({"type":"number"}) };
    (AlertLevel) => { json!({"type":"number", "minimum":0, "maximum":1e15}) };
    (u64) => { json!({"type":"integer", "minimum":0, "maximum":9007199254740991_u64,
        "description":"Drawing ids are issued below 2^53 for exact JSON client representation"}) };
    (bool) => { json!({"type":"boolean", "default":false}) };
    (SourceTimeframe) => { json!({"type":"string", "enum":["1m","2m","3m","5m","15m","30m","1H","2H","4H","1D","1W","1M","3M"], "description":"Analysis timeframe; falls back to the chart timeframe when omitted."}) };
    (Timeframe) => { json!({"type":"string", "enum":["1m","2m","3m","5m","15m","30m","1H","2H","4H","1D","1W","1M","3M"], "default":"1D"}) };
    (Mode) => { json!({"type":"string", "enum":["swings","trend","levels","volume_profile","volatility"]}) };
    (ZoneRole) => { json!({"type":"string", "enum":["support","resistance"]}) };
    (DrawingKind) => { json!({"type":"string", "enum":["horizontal","vertical","trend_line","ray","rectangle","text"]}) };
    (CompareAction) => { json!({"type":"string", "enum":["add","remove"]}) };
    (AlertRule) => { json!({"type":"string", "enum":["above","below"]}) };
    (AlertKind) => { json!({"type":"string", "enum":["price","close"]}) };
    (Repeat) => { json!({"type":"string", "enum":["once","rearm"], "description":"rearm uses a 60 second cooldown"}) };
    (Name) => { json!({"type":"string", "minLength":1, "maxLength":40, "pattern":"\\S"}) };
    (Anchors) => { json!({"type":"array", "items":Anchor::schema(), "minItems":1, "maxItems":8}) };
    (Symbols) => { json!({"type":"array", "items":field_schema!(String), "minItems":1, "maxItems":20}) };
    (Params) => { json!({"type":"array", "items":{"type":"number"}}) };
    (Timestamp) => { json!({"type":"integer", "description":"Unix seconds UTC",
        "minimum":MIN_TIMESTAMP, "maximum":MAX_TIMESTAMP}) };
    (Limit) => { json!({"type":"integer", "minimum":1, "maximum":10000, "default":1000}) };
    (Top) => { json!({"type":"integer", "minimum":1, "maximum":100, "default":10}) };
}

macro_rules! args {
    ($name:ident { $( $field:ident : $ty:ty => $schema:ident ),* $(,)? } { $( $optional:ident : $opt_ty:ty => $opt_schema:ident ),* $(,)? }) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            $(pub $field: $ty,)*
            $(#[serde(default)] pub $optional: $opt_ty,)*
        }
        impl Arguments for $name {
            fn schema() -> Value {
                #[allow(unused_mut)] // Empty argument objects deliberately have no properties.
                let mut properties = serde_json::Map::new();
                $(properties.insert(stringify!($field).into(), field_schema!($schema));)*
                $(properties.insert(stringify!($optional).into(), optional_schema::<$opt_ty>(field_schema!($opt_schema)));)*
                let required: Vec<&str> = vec![$(stringify!($field)),*];
                json!({"type":"object", "properties":properties, "required":required, "additionalProperties":false})
            }
        }
    };
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Swings,
    Trend,
    Levels,
    VolumeProfile,
    Volatility,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrawingKind {
    Horizontal,
    Vertical,
    TrendLine,
    Ray,
    Rectangle,
    Text,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareAction {
    Add,
    Remove,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertRule {
    Above,
    Below,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    Price,
    Close,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertRepeat {
    Once,
    Rearm,
}

args!(Anchor { time: i64 => Timestamp, price: f64 => f64 } {});
args!(ReadData { symbol: String => String } {
    timeframe: Option<String> => Timeframe, from: Option<i64> => Timestamp,
    to: Option<i64> => Timestamp, limit: Option<usize> => Limit
});
args!(Analyze { symbol: String => String, mode: Mode => Mode } {
    timeframe: Option<String> => Timeframe, from: Option<i64> => Timestamp,
    to: Option<i64> => Timestamp, limit: Option<usize> => Limit, top: Option<usize> => Top
});
args!(FindSymbol { query: String => String } {});
args!(ReadChart {} { chart_id: Option<String> => String });
args!(IndicatorValues {} { chart_id: Option<String> => String, id: Option<String> => String });
args!(SetView {} {
    chart_id: Option<String> => String, symbol: Option<String> => String,
    timeframe: Option<String> => Timeframe, from: Option<i64> => Timestamp, to: Option<i64> => Timestamp
});
args!(CreateDrawing { symbol: String => String, kind: DrawingKind => DrawingKind, anchors: Vec<Anchor> => Anchors } {
    chart_id: Option<String> => String, all_symbols: bool => bool, label: Option<String> => String,
    zone_role: Option<ZoneRole> => ZoneRole, source_timeframe: Option<String> => SourceTimeframe
});
args!(UpdateDrawing { symbol: String => String, id: u64 => u64 } {
    chart_id: Option<String> => String, all_symbols: bool => bool, anchors: Option<Vec<Anchor>> => Anchors,
    label: Option<String> => String
});
args!(DeleteDrawing { symbol: String => String, id: u64 => u64 } {
    chart_id: Option<String> => String, all_symbols: bool => bool
});
args!(AddIndicator { code: String => String } {
    chart_id: Option<String> => String, params: Vec<f64> => Params
});
args!(RemoveIndicator { id: String => String } { chart_id: Option<String> => String });
args!(Compare { symbol: String => String, action: CompareAction => CompareAction } { chart_id: Option<String> => String });
args!(Screenshot {} { chart_id: Option<String> => String });
args!(ReadUserActions {} { chart_id: Option<String> => String, since: Option<i64> => Timestamp });
args!(ListWatchlists {} {});
args!(ListAlerts {} { symbol: Option<String> => String });
args!(CreateWatchlist { name: String => Name } {});
args!(AddWatchlistSymbols { watchlist: String => Name, symbols: Vec<String> => Symbols } {});
args!(RemoveWatchlistSymbols { watchlist: String => Name, symbols: Vec<String> => Symbols } {});
args!(DeleteWatchlist { watchlist: String => Name } {});
args!(CreateAlert { symbol: String => String, kind: AlertKind => AlertKind, direction: AlertRule => AlertRule, level: f64 => AlertLevel } {
    timeframe: Option<String> => Timeframe, repeat: Option<AlertRepeat> => Repeat, note: Option<String> => String
});
args!(UpdateAlert { id: u64 => u64 } {
    level: Option<f64> => AlertLevel, direction: Option<AlertRule> => AlertRule, repeat: Option<AlertRepeat> => Repeat
});
args!(DeleteAlert { id: u64 => u64 } {});

// A5b-SCAN: scanner wire types and schemas (kept separate from multi-symbol tools).
pub mod scanner {
    use super::*;
    // A5b-SCAN: shared by worker deadlines and the bridge's run_scan-only timeout.
    pub const SCAN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);
    use tr_core::scanner::{
        Comparison, ConditionGroup, ConditionItem, MatchMode, ScanCondition, ScanDefinition, ScanOperand, Source,
    };

    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Universe {
        Watchlist,
        UsStocks,
        Nasdaq,
        Nyse,
        BinanceSpot,
        CoinbaseSpot,
    }
    impl Universe {
        pub fn core(self) -> tr_data::universe::Universe {
            use tr_data::universe::Universe as U;
            match self {
                Self::Watchlist => U::Watchlist,
                Self::UsStocks => U::UsStocks,
                Self::Nasdaq => U::Nasdaq,
                Self::Nyse => U::Nyse,
                Self::BinanceSpot => U::BinanceSpot,
                Self::CoinbaseSpot => U::CoinbaseSpot,
            }
        }
    }
    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Mode {
        All,
        Any,
    }
    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Op {
        Above,
        Below,
        AtLeast,
        AtMost,
        CrossedAbove,
        CrossedBelow,
    }
    #[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
    #[serde(rename_all = "snake_case")]
    pub enum Field {
        #[default]
        Close,
        Volume,
    }
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Average {
        pub period: usize,
        #[serde(default)]
        pub source: Field,
    }
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Period {
        pub period: usize,
    }
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case", deny_unknown_fields)]
    pub enum Indicator {
        Sma(Average),
        Ema(Average),
        Atr(Period),
        Rsi(Period),
    }
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Basic {
        Price,
        Volume,
        ChangePercent,
    }
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(untagged)]
    pub enum Operand {
        Basic(Basic),
        Indicator(Indicator),
        Constant(f64),
    }
    impl Operand {
        pub fn core(&self) -> ScanOperand {
            let source = |f: Field| match f {
                Field::Close => Source::Close,
                Field::Volume => Source::Volume,
            };
            match self {
                Self::Basic(Basic::Price) => ScanOperand::Price,
                Self::Basic(Basic::Volume) => ScanOperand::Volume,
                Self::Basic(Basic::ChangePercent) => ScanOperand::ChangePercent,
                Self::Constant(v) => ScanOperand::Constant(*v),
                Self::Indicator(Indicator::Sma(a)) => ScanOperand::Sma { period: a.period, source: source(a.source) },
                Self::Indicator(Indicator::Ema(a)) => ScanOperand::Ema { period: a.period, source: source(a.source) },
                Self::Indicator(Indicator::Atr(a)) => ScanOperand::Atr { period: a.period },
                Self::Indicator(Indicator::Rsi(a)) => ScanOperand::Rsi { period: a.period },
            }
        }
    }
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Condition {
        pub left: Operand,
        pub op: Op,
        pub right: Operand,
        #[serde(default)]
        pub bars_ago: usize,
    }
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Group {
        pub r#match: Mode,
        pub conditions: Vec<Condition>,
        #[serde(default)]
        pub groups: Vec<Group>,
    }
    impl Group {
        fn core(&self) -> ConditionGroup {
            let mut items: Vec<_> = self
                .conditions
                .iter()
                .map(|c| {
                    ConditionItem::Compare(ScanCondition {
                        left: c.left.core(),
                        right: c.right.core(),
                        comparison: match c.op {
                            Op::Above => Comparison::Above,
                            Op::Below => Comparison::Below,
                            Op::AtLeast => Comparison::AtLeast,
                            Op::AtMost => Comparison::AtMost,
                            Op::CrossedAbove => Comparison::CrossedAbove,
                            Op::CrossedBelow => Comparison::CrossedBelow,
                        },
                        left_bars_ago: c.bars_ago,
                        right_bars_ago: c.bars_ago,
                        ..ScanCondition::default()
                    })
                })
                .collect();
            items.extend(self.groups.iter().map(|g| ConditionItem::Group(g.core())));
            ConditionGroup {
                mode: match self.r#match {
                    Mode::All => MatchMode::All,
                    Mode::Any => MatchMode::Any,
                },
                items,
            }
        }
    }
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Definition {
        pub timeframe: String,
        pub r#match: Mode,
        pub conditions: Vec<Condition>,
        #[serde(default)]
        pub groups: Vec<Group>,
    }
    impl Definition {
        pub fn core(&self) -> Result<ScanDefinition, String> {
            let definition = ScanDefinition {
                name: "Agent scan".into(),
                timeframe: self.timeframe.parse().map_err(|_| "Unsupported scan timeframe")?,
                root: Group { r#match: self.r#match, conditions: self.conditions.clone(), groups: self.groups.clone() }
                    .core(),
            };
            definition.validate()?;
            Ok(definition)
        }
    }
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct ListScans {}
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct ReadScanResults {
        pub pane_id: Option<u64>,
        pub name: Option<String>,
        #[serde(default)]
        pub include: ScanInclude,
    }
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum ScanInclude {
        #[default]
        Matched,
        All,
    }
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct RunScan {
        pub saved: Option<String>,
        pub definition: Option<Definition>,
        pub universe: Universe,
        pub watchlist: Option<String>,
        pub sort_by: Option<Operand>,
        pub limit: Option<usize>,
    }
    impl RunScan {
        pub fn validate(&self) -> Result<(), String> {
            if self.saved.is_some() == self.definition.is_some() {
                return Err("Supply exactly one of saved or definition".into());
            }
            if self.watchlist.is_some() && !matches!(self.universe, Universe::Watchlist) {
                return Err("watchlist is only valid for universe=watchlist".into());
            }
            if self.limit.is_some_and(|n| !(1..=50).contains(&n)) {
                return Err("limit must be between 1 and 50".into());
            }
            if let Some(d) = &self.definition {
                d.core()?;
            }
            if let Some(o) = &self.sort_by {
                let d = ScanDefinition {
                    root: ConditionGroup::comparisons(
                        MatchMode::All,
                        vec![ScanCondition { left: o.core(), right: ScanOperand::Constant(0.), ..Default::default() }],
                    ),
                    ..Default::default()
                };
                d.validate()?;
            }
            Ok(())
        }
    }
    fn object(properties: Value, required: &[&str]) -> Value {
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
    }
    fn operand() -> Value {
        let mut choices =
            vec![json!({"type":"string","enum":["price","volume","change_percent"]}), json!({"type":"number"})];
        for name in ["sma", "ema", "atr", "rsi"] {
            let mut fields = json!({"period":{"type":"integer","minimum":1,"maximum":500}});
            if matches!(name, "sma" | "ema") {
                fields["source"] = json!({"type":"string","enum":["close","volume"],"default":"close"});
            }
            choices.push(object(json!({name:object(fields,&["period"])}), &[name]));
        }
        json!({"anyOf":choices})
    }
    fn group(depth: usize) -> Value {
        let condition = object(
            json!({"left":operand(),"op":{"type":"string","enum":["above","below","at_least","at_most","crossed_above","crossed_below"]},"right":operand(),"bars_ago":{"type":"integer","minimum":0,"maximum":250,"default":0}}),
            &["left", "op", "right"],
        );
        let mut fields = json!({"match":{"type":"string","enum":["all","any"]},"conditions":{"type":"array","items":condition,"maxItems":32}});
        if depth < 3 {
            fields["groups"] = json!({"type":"array","items":group(depth+1),"maxItems":32});
        } else {
            fields["groups"] = json!({"type":"array","items":{"type":"null"},"maxItems":0});
        }
        object(fields, &["match", "conditions"])
    }
    impl Arguments for ListScans {
        fn schema() -> Value {
            object(json!({}), &[])
        }
    }
    impl Arguments for ReadScanResults {
        fn schema() -> Value {
            object(
                json!({"pane_id":{"type":["integer","null"],"minimum":0},"name":{"type":["string","null"]},"include":{"type":"string","enum":["matched","all"],"default":"matched"}}),
                &[],
            )
        }
    }
    impl Arguments for RunScan {
        fn schema() -> Value {
            let mut definition = group(1);
            definition["properties"]["timeframe"] = field_schema!(Timeframe);
            definition["required"] = json!(["timeframe", "match", "conditions"]);
            object(
                json!({"saved":{"type":["string","null"]},"definition":{"anyOf":[definition,{"type":"null"}]},"universe":{"type":"string","enum":["watchlist","us_stocks","nasdaq","nyse","binance_spot","coinbase_spot"]},"watchlist":{"type":["string","null"]},"sort_by":{"anyOf":[operand(),{"type":"null"}]},"limit":{"type":["integer","null"],"minimum":1,"maximum":50,"default":50}}),
                &["universe"],
            )
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        fn definition() -> Value {
            json!({"timeframe":"1D","match":"all","conditions":[{"left":"price","op":"above","right":{"sma":{"period":20,"source":"close"}}}]})
        }
        #[test]
        fn scanner_definition_maps_every_operand_and_comparator_to_core() {
            let operands = vec![
                json!("price"),
                json!("volume"),
                json!("change_percent"),
                json!({"sma":{"period":20,"source":"volume"}}),
                json!({"ema":{"period":5}}),
                json!({"atr":{"period":14}}),
                json!({"rsi":{"period":14}}),
                json!(12.5),
            ];
            for operand in operands {
                for op in ["above", "below", "at_least", "at_most", "crossed_above", "crossed_below"] {
                    let mut input = definition();
                    input["conditions"][0] = json!({"left":operand,"op":op,"right":0,"bars_ago":2});
                    let typed: Definition = serde_json::from_value(input).unwrap();
                    let core = typed.core().unwrap();
                    assert_eq!(core.timeframe, tr_core::Timeframe::D1);
                    assert!(core.required_bars() >= 3);
                    let ConditionItem::Compare(c) = &core.root.items[0] else { panic!() };
                    assert_eq!(c.left, typed.conditions[0].left.core());
                    assert_eq!((c.left_bars_ago, c.right_bars_ago), (2, 2));
                }
            }
        }
        #[test]
        fn scanner_definition_is_strict_and_core_limits_are_authoritative() {
            for bad in
                [json!("unknown_operand"), json!({"sma":{"period":20,"unknown":true}}), json!({"unknown":{"period":2}})]
            {
                let mut value = definition();
                value["conditions"][0]["left"] = bad;
                assert!(serde_json::from_value::<Definition>(value).is_err());
            }
            for path in [0, 1, 2] {
                let mut value = definition();
                match path {
                    0 => value["surprise"] = json!(true),
                    1 => value["conditions"][0]["surprise"] = json!(true),
                    _ => value["groups"] = json!([{"match":"all","conditions":[],"surprise":1}]),
                }
                assert!(serde_json::from_value::<Definition>(value).is_err());
            }
            let mut value = definition();
            value["conditions"][0]["right"] = json!({"sma":{"period":0}});
            assert!(serde_json::from_value::<Definition>(value).unwrap().core().unwrap_err().contains("period"));
            let mut value = definition();
            value["conditions"] = json!(vec![value["conditions"][0].clone(); 33]);
            assert!(serde_json::from_value::<Definition>(value).unwrap().core().unwrap_err().contains("32"));
            let mut group = json!({"match":"any","conditions":[{"left":"price","op":"above","right":0}]});
            for _ in 0..3 {
                group = json!({"match":"all","conditions":[],"groups":[group]});
            }
            group["timeframe"] = json!("1D");
            assert!(serde_json::from_value::<Definition>(group).unwrap().core().unwrap_err().contains("depth"));
        }
        #[test]
        fn scanner_read_defaults_to_matched_and_accepts_only_explicit_all_or_matched() {
            assert_eq!(serde_json::from_value::<ReadScanResults>(json!({})).unwrap().include, ScanInclude::Matched);
            for (text, expected) in [("all", ScanInclude::All), ("matched", ScanInclude::Matched)] {
                assert_eq!(
                    serde_json::from_value::<ReadScanResults>(json!({"include":text})).unwrap().include,
                    expected
                );
            }
            for value in [json!({"include":"no_match"}), json!({"include":null}), json!({"include":true})] {
                assert!(serde_json::from_value::<ReadScanResults>(value).is_err());
            }
        }
        #[test]
        fn scanner_run_requires_one_definition_valid_limit_and_watchlist_scope() {
            for value in [
                json!({"universe":"watchlist"}),
                json!({"universe":"watchlist","saved":"x","definition":definition()}),
                json!({"universe":"nasdaq","saved":"x","watchlist":"Main"}),
                json!({"universe":"watchlist","saved":"x","limit":51}),
            ] {
                assert!(serde_json::from_value::<RunScan>(value).unwrap().validate().is_err());
            }
        }
    }
}
pub use scanner::{ListScans, ReadScanResults, RunScan};
// End A5b-SCAN wire block.

macro_rules! live_args {
    ($($variant:ident($ty:ident) => $name:literal),* $(,)?) => {
        #[derive(Debug, Clone, Serialize)]
        #[serde(tag = "tool", content = "args")]
        pub enum LiveArgs { $(#[serde(rename = $name)] $variant($ty)),* }
        impl LiveArgs {
            pub fn name(&self) -> &'static str {
                match self { $(Self::$variant(_) => $name),* }
            }
            pub(crate) fn parse(name: &str, value: Value) -> Result<Self, serde_json::Error> {
                match name { $($name => serde_json::from_value(value).map(Self::$variant),)* _ => unreachable!("registered live tool") }
            }
            pub(crate) fn schemas() -> Vec<(&'static str, Value)> { vec![$(($name, $ty::schema())),*] }
        }
    };
}

live_args!(
    // A5b-SCAN live tools.
    ListScans(ListScans) => "list_scans", ReadScanResults(ReadScanResults) => "read_scan_results", RunScan(RunScan) => "run_scan",
    ReadChart(ReadChart) => "read_chart", IndicatorValues(IndicatorValues) => "indicator_values",
    SetView(SetView) => "set_view", CreateDrawing(CreateDrawing) => "create_drawing",
    UpdateDrawing(UpdateDrawing) => "update_drawing", DeleteDrawing(DeleteDrawing) => "delete_drawing",
    AddIndicator(AddIndicator) => "add_indicator", RemoveIndicator(RemoveIndicator) => "remove_indicator",
    Compare(Compare) => "compare", Screenshot(Screenshot) => "screenshot", ReadUserActions(ReadUserActions) => "read_user_actions",
    ListWatchlists(ListWatchlists) => "list_watchlists", ListAlerts(ListAlerts) => "list_alerts",
    CreateWatchlist(CreateWatchlist) => "create_watchlist", AddWatchlistSymbols(AddWatchlistSymbols) => "add_watchlist_symbols",
    RemoveWatchlistSymbols(RemoveWatchlistSymbols) => "remove_watchlist_symbols", DeleteWatchlist(DeleteWatchlist) => "delete_watchlist",
    CreateAlert(CreateAlert) => "create_alert", UpdateAlert(UpdateAlert) => "update_alert", DeleteAlert(DeleteAlert) => "delete_alert",
);
