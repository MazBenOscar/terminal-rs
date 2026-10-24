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
    (u64) => { json!({"type":"integer", "minimum":0, "maximum":9007199254740991_u64,
        "description":"Drawing ids are issued below 2^53 for exact JSON client representation"}) };
    (bool) => { json!({"type":"boolean", "default":false}) };
    (Timeframe) => { json!({"type":"string", "enum":["1m","2m","3m","5m","15m","30m","1H","2H","4H","1D","1W","1M","3M"], "default":"1D"}) };
    (Mode) => { json!({"type":"string", "enum":["swings","trend","levels","volume_profile","volatility"]}) };
    (ZoneRole) => { json!({"type":"string", "enum":["support","resistance"]}) };
    (DrawingKind) => { json!({"type":"string", "enum":["horizontal","vertical","trend_line","ray","rectangle","text"]}) };
    (CompareAction) => { json!({"type":"string", "enum":["add","remove"]}) };
    (AlertRule) => { json!({"type":"string", "enum":["above","below","crossing"]}) };
    (Anchors) => { json!({"type":"array", "items":Anchor::schema(), "minItems":1, "maxItems":8}) };
    (Symbols) => { json!({"type":"array", "items":field_schema!(String), "minItems":1, "maxItems":200}) };
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
    Crossing,
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
    zone_role: Option<ZoneRole> => ZoneRole
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
args!(CreateWatchlist { name: String => String } { chart_id: Option<String> => String });
args!(AddWatchlistSymbols { id: String => String, symbols: Vec<String> => Symbols } { chart_id: Option<String> => String });
args!(RemoveWatchlistSymbols { id: String => String, symbols: Vec<String> => Symbols } { chart_id: Option<String> => String });
args!(DeleteWatchlist { id: String => String } { chart_id: Option<String> => String });
args!(CreateAlert { symbol: String => String, rule: AlertRule => AlertRule, price: f64 => f64 } {
    chart_id: Option<String> => String, label: Option<String> => String
});
args!(UpdateAlert { id: String => String, rule: AlertRule => AlertRule, price: f64 => f64 } {
    chart_id: Option<String> => String, label: Option<String> => String
});
args!(DeleteAlert { id: String => String } { chart_id: Option<String> => String });

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
    CreateWatchlist(CreateWatchlist) => "create_watchlist", AddWatchlistSymbols(AddWatchlistSymbols) => "add_watchlist_symbols",
    RemoveWatchlistSymbols(RemoveWatchlistSymbols) => "remove_watchlist_symbols", DeleteWatchlist(DeleteWatchlist) => "delete_watchlist",
    CreateAlert(CreateAlert) => "create_alert", UpdateAlert(UpdateAlert) => "update_alert", DeleteAlert(DeleteAlert) => "delete_alert",
);
