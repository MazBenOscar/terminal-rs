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
// A5b-MULTI: bounded, read-only batch arguments (kept separate from scanner tools).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetQuotes {
    pub symbols: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompareSymbols {
    pub symbols: Vec<String>,
    pub timeframe: String,
    #[serde(default)]
    pub lookback: Option<usize>,
    #[serde(default)]
    pub from: Option<i64>,
    #[serde(default)]
    pub to: Option<i64>,
}
impl Arguments for GetQuotes {
    fn schema() -> Value {
        json!({"type":"object","properties":{"symbols":{"type":"array","items":field_schema!(String),
            "minItems":1,"maxItems":25}},"required":["symbols"],"additionalProperties":false})
    }
}
impl Arguments for CompareSymbols {
    fn schema() -> Value {
        json!({"type":"object","properties":{
            "symbols":{"type":"array","items":field_schema!(String),"minItems":2,"maxItems":8,
                "description":"First symbol is the base."},
            "timeframe":field_schema!(Timeframe),
            "lookback":{"type":["integer","null"],"minimum":1,"maximum":1000,"default":120,
                "description":"Finished bars per symbol; omit when using from/to. Date ranges are capped at 1000 bars per symbol."},
            "from":optional_schema::<Option<i64>>(field_schema!(Timestamp)),
            "to":optional_schema::<Option<i64>>(field_schema!(Timestamp))},
            "required":["symbols","timeframe"],"additionalProperties":false})
    }
}
// End A5b-MULTI arguments.

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
