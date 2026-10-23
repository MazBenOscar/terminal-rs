use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use tr_core::{AssetClass, Bar, Series, Timeframe, crypto::CryptoInstrument};

use crate::{
    DataHost, History, MAX_TEXT_BYTES, ToolResult, analytics,
    args::{Analyze, FindSymbol, ReadData},
};

pub(crate) fn iso(ts: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp(ts, 0).map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, false))
}

pub(crate) fn timeframe(value: Option<&str>) -> Result<Timeframe, ToolResult> {
    value
        .unwrap_or("1D")
        .parse()
        .map_err(|message: String| ToolResult::error("invalid_args", &message, Some("timeframe")))
}

async fn history(host: &dyn DataHost, args: &ReadData, now: i64) -> Result<(History, Timeframe), ToolResult> {
    let tf = timeframe(args.timeframe.as_deref())?;
    let limit = args.limit.unwrap_or(1000);
    let mut history = host
        .history(&args.symbol, tf, Some(args.to.unwrap_or(now).min(now)), limit)
        .await
        .map_err(|e| ToolResult::error("host_error", &e, None))?;
    if let Some(instrument) = &history.instrument {
        instrument.validate().map_err(|e| ToolResult::error("invalid_data", &e, Some("instrument")))?;
    }
    if history.bars.iter().any(|b| {
        ![b.open, b.high, b.low, b.close, b.volume].iter().all(|n| n.is_finite())
            || b.volume < 0.
            || b.low > b.high
            || b.open < b.low
            || b.open > b.high
            || b.close < b.low
            || b.close > b.high
            || !(-62_135_596_800..253_370_764_800).contains(&b.ts)
    }) {
        return Err(ToolResult::error("invalid_data", "History contains invalid OHLCV or timestamps", None));
    }
    let mut bars = Series::new(&args.symbol, tf, history.bars).bars;
    bars.retain(|b| args.from.is_none_or(|from| b.ts >= from) && b.ts <= args.to.unwrap_or(now).min(now));
    if bars.len() > limit {
        bars.drain(..bars.len() - limit);
    }
    history.bars = bars;
    Ok((history, tf))
}

pub(crate) async fn read_data(host: &dyn DataHost, args: ReadData, now: i64) -> ToolResult {
    let (history, tf) = match history(host, &args, now).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    ToolResult::json(summary(&history, &args, tf, now))
}

fn summary(history: &History, args: &ReadData, tf: Timeframe, now: i64) -> Value {
    let bars = &history.bars;
    let first = bars.first();
    let last = bars.last();
    let mut value = json!({
        "version":1,"symbol":history.instrument.as_ref().map_or_else(||args.symbol.clone(),CryptoInstrument::canonical_ticker),
        "requested_symbol":args.symbol,"timeframe":tf.label(),"instrument":history.instrument,
        "price_basis":history.price_basis,"session":history.session.label(),"timezone":"UTC",
        "count":bars.len(),"first":first.and_then(|b|iso(b.ts)),"last":last.and_then(|b|iso(b.ts)),
        "requested_range":{"from":args.from.and_then(iso),"to":args.to.and_then(iso),"limit":args.limit},
        "range_note":"Statistics cover returned bars only; provider availability and limit may bound the requested range.",
        "ohlcv":stats(bars),
        "returns":{"close_to_close_percent":first.zip(last).and_then(|(a,b)|percent(a.close,b.close)),
            "open_to_close_percent":first.zip(last).and_then(|(a,b)|percent(a.open,b.close))},
        "in_progress_last_bar":last.is_some_and(|b| now>=b.ts && now<history.session.interval(tf,b.ts).1),
        "bar_columns":["time","open","high","low","close","volume","in_progress"],
        "close_series_columns":["time","close"],"truncated":false
    });
    let (mut recent, mut points) = (bars.len().min(200), bars.len().min(300));
    loop {
        value["bars"] = bars[bars.len() - recent..]
            .iter()
            .map(|b| {
                json!([
                    iso(b.ts),
                    b.open,
                    b.high,
                    b.low,
                    b.close,
                    b.volume,
                    last.is_some_and(|last| last.ts == b.ts)
                        && now >= b.ts
                        && now < history.session.interval(tf, b.ts).1
                ])
            })
            .collect();
        value["close_series"] = analytics::lttb(bars, points).into_iter().map(|(t, v)| json!([iso(t), v])).collect();
        value["emitted"] = json!({"recent_bars":recent,"close_points":value["close_series"].as_array().unwrap().len()});
        if value.to_string().len() <= MAX_TEXT_BYTES {
            return value;
        }
        value["truncated"] = true.into();
        value["marker"] =
            "[truncated: recent bars and LTTB points reduced to fit 16 KiB; full-range statistics retained]".into();
        if recent <= 1 && points <= 2 {
            return value;
        }
        recent = (recent * 3 / 4).max(1).min(bars.len());
        points = (points * 3 / 4).max(2).min(bars.len());
    }
}

fn percent(from: f64, to: f64) -> Option<f64> {
    (from != 0.).then(|| (to / from - 1.) * 100.).filter(|v| v.is_finite())
}

fn stats(bars: &[Bar]) -> Value {
    if bars.is_empty() {
        return Value::Null;
    }
    let column = |get: fn(&Bar) -> f64| {
        let min = bars.iter().map(get).fold(f64::INFINITY, f64::min);
        let max = bars.iter().map(get).fold(f64::NEG_INFINITY, f64::max);
        let sum = bars.iter().map(get).sum::<f64>();
        json!({"min":min,"max":max,"mean":sum/bars.len() as f64,"sum":sum})
    };
    json!({"open":column(|b|b.open),"high":column(|b|b.high),"low":column(|b|b.low),
        "close":column(|b|b.close),"volume":column(|b|b.volume)})
}

pub(crate) async fn analyze(host: &dyn DataHost, args: Analyze, now: i64) -> ToolResult {
    let request = ReadData {
        symbol: args.symbol.clone(),
        timeframe: args.timeframe,
        from: args.from,
        to: args.to,
        limit: args.limit,
    };
    let (history, tf) = match history(host, &request, now).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let bars = &history.bars;
    if bars.is_empty() {
        return ToolResult::error("no_data", "No bars in the requested range", None);
    }
    let mut value = analytics::analyze(bars, tf, history.session, args.mode, args.top.unwrap_or(10));
    value["version"] = 1.into();
    value["symbol"] =
        history.instrument.as_ref().map_or_else(|| args.symbol.clone(), CryptoInstrument::canonical_ticker).into();
    value["timeframe"] = tf.label().into();
    value["bar_range"] = json!({"from":iso(bars[0].ts),"to":iso(bars.last().unwrap().ts),"count":bars.len()});
    value["price_basis"] = json!(history.price_basis);
    value["session"] = history.session.label().into();
    value["mode"] = json!(args.mode);
    while value.to_string().len() > MAX_TEXT_BYTES {
        value["truncated"] = true.into();
        value["marker"] = "[truncated: analysis detail reduced to fit 16 KiB; bar_range retained]".into();
        let mut reduced = false;
        for path in ["/highs", "/lows", "/support_resistance"] {
            if let Some(array) = value.pointer_mut(path).and_then(Value::as_array_mut)
                && array.len() > 1
            {
                let drop = array.len() / 2;
                if path == "/support_resistance" {
                    array.truncate(array.len() - drop);
                } else {
                    array.drain(..drop);
                }
                reduced = true;
            }
        }
        if let Some(array) = value.pointer_mut("/zigzag/series").and_then(Value::as_array_mut)
            && array.len() > 2
        {
            let last = array.last().unwrap().clone();
            *array = array.iter().step_by(2).cloned().collect();
            if array.last() != Some(&last) {
                array.push(last);
            }
            reduced = true;
        }
        if !reduced {
            break;
        }
    }
    ToolResult::json(value)
}

pub(crate) async fn find_symbol(host: &dyn DataHost, args: FindSymbol) -> ToolResult {
    let hits = match host.search(&args.query).await {
        Ok(hits) => hits,
        Err(e) => return ToolResult::error("host_error", &e, None),
    };
    let total = hits.len();
    let mut results = Vec::new();
    for hit in hits.into_iter().take(100) {
        let instrument = if hit.class == AssetClass::Crypto {
            let tokens: Vec<_> = hit.key.split_whitespace().collect();
            tr_core::Security::resolve_tokens(&tokens).and_then(|(security, used)| {
                (used == tokens.len()).then(|| CryptoInstrument::from_security(&security).ok()).flatten()
            })
        } else {
            None
        };
        let symbol = instrument.as_ref().map_or_else(|| hit.key.clone(), CryptoInstrument::canonical_ticker);
        let display_name =
            instrument.as_ref().map(|i| format!("{}/{}", i.base, i.quote)).unwrap_or_else(|| hit.key.clone());
        let result = json!({"symbol":symbol,"display_name":display_name,"name":hit.name,"class":hit.class,
            "exchange":hit.exchange,"instrument":instrument});
        results.push(result);
        if json!({"results":results}).to_string().len() > MAX_TEXT_BYTES - 512 {
            results.pop();
            break;
        }
    }
    let truncated = results.len() < total;
    ToolResult::json(json!({"results":results,"total":total,"truncated":truncated,
        "marker":if truncated {Some("[truncated: search results reduced to fit 16 KiB]")} else {None}}))
}
