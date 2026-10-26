use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use tr_core::{AssetClass, Bar, Series, Timeframe, crypto::CryptoInstrument};

use crate::{
    DataHost, History, MAX_TEXT_BYTES, ToolResult, analytics,
    args::{Analyze, FindSymbol, ReadData},
};

// A5b-MULTI: deterministic read-only batch tools, isolated for the scanner merge.
pub(crate) mod multi {
    use super::*;
    use crate::args::{CompareSymbols, GetQuotes};
    use std::{collections::BTreeSet, future::poll_fn, task::Poll};
    use tr_core::indicators::{self, Ohlcv};

    fn echo(symbol: &str) -> String {
        crate::scrub_text(symbol).chars().take(64).collect()
    }
    // Keep the version and all per-symbol rows when verbose discovery hints or
    // provenance exceed the text budget. Numeric summaries are never downsampled.
    fn batch_result(mut value: Value) -> ToolResult {
        if value.to_string().len() > MAX_TEXT_BYTES {
            value["truncated"] = json!(true);
            value["marker"] =
                json!("[truncated: discovery hints, request echoes and source detail omitted to fit 16 KiB]");
            for row in value["symbols"].as_array_mut().into_iter().flatten() {
                let row = row.as_object_mut().unwrap();
                for key in ["hint", "hints", "message", "requested_symbol", "source"] {
                    row.remove(key);
                }
                if let Some(times) = row.get_mut("forming_bar_times").and_then(Value::as_array_mut)
                    && times.len() > 2
                {
                    let count = times.len();
                    times.drain(1..count - 1);
                    row.insert("forming_bars_excluded".into(), json!(count));
                }
            }
        }
        ToolResult::json(value)
    }

    pub(crate) async fn get_quotes(host: &dyn DataHost, args: GetQuotes, now: i64) -> ToolResult {
        let mut rows = Vec::new();
        let mut keys = Vec::new();
        let mut resolved = Vec::new();
        for symbol in &args.symbols {
            match resolve_workflow_symbol(host, symbol).await {
                Ok(key) => {
                    rows.push(json!({"requested_symbol":echo(symbol),"symbol":key}));
                    if !keys.contains(&key) {
                        keys.push(key.clone());
                    }
                    resolved.push(Some(key));
                }
                Err(error) => {
                    rows.push(error);
                    resolved.push(None);
                }
            }
        }
        // Exactly one quote request for a batch containing resolved instruments.
        let quotes = if keys.is_empty() { Ok(Vec::new()) } else { host.quotes(&keys).await };
        for (i, key) in resolved.iter().enumerate().filter_map(|(i, key)| key.as_ref().map(|key| (i, key))) {
            let row = &mut rows[i];
            match &quotes {
                Err(_) => row["error"] = json!("quotes_unavailable"),
                Ok(quotes) => {
                    let quote = quotes.iter().find(|q| tr_data::security(&q.symbol).key() == *key);
                    match quote {
                        None => row["error"] = json!("quote_unavailable"),
                        Some(q) if !q.last.is_finite() || q.last <= 0. || iso(q.ts).is_none() => {
                            row["error"] = json!("invalid_data")
                        }
                        Some(q) => {
                            row["last"] = json!(q.last);
                            let previous = q.prev_close.is_finite() && q.prev_close > 0.;
                            row["change"] = if previous { json!(q.last - q.prev_close) } else { Value::Null };
                            row["change_percent"] =
                                if previous { json!((q.last / q.prev_close - 1.) * 100.) } else { Value::Null };
                            row["as_of"] = json!(iso(q.ts));
                            row["source"] = json!(q.crypto_source);
                            row["freshness"] = json!(if q.ts > now {
                                "future_timestamp"
                            } else if now - q.ts <= 60 {
                                "recent_timestamp"
                            } else {
                                "stale_timestamp"
                            });
                            row["simulation"] = json!(match q.simulated {
                                Some(true) => "simulated",
                                Some(false) => "not_simulated",
                                None => "unknown",
                            });
                        }
                    }
                }
            }
        }
        let failed = rows.iter().filter(|r| !r["error"].is_null()).count();
        batch_result(json!({"version":1,"as_of":iso(now),"symbols":rows,"failed":failed,
            "warnings":["Quote timestamps indicate age, not a live-feed guarantee; simulation is unknown unless the provider supplies provenance."]})).capped()
    }

    struct Input {
        index: usize,
        key: String,
        history: History,
        forming: Vec<i64>,
    }

    fn common_times(inputs: &[Input]) -> BTreeSet<i64> {
        let mut times: BTreeSet<i64> =
            inputs.first().map(|i| i.history.bars.iter().map(|b| b.ts).collect()).unwrap_or_default();
        for input in inputs.iter().skip(1) {
            let available: BTreeSet<_> = input.history.bars.iter().map(|b| b.ts).collect();
            times = times.intersection(&available).copied().collect();
        }
        times
    }

    use tr_core::session::{Session, Zone};

    fn calendar_days(class: AssetClass) -> u32 {
        match class {
            AssetClass::Crypto => 365,
            AssetClass::Fx => 260,
            AssetClass::Equity | AssetClass::Etf | AssetClass::Index | AssetClass::Govt | AssetClass::Cmdty => 252,
        }
    }

    fn annual_bars(tf: Timeframe, class: AssetClass, session: Session) -> f64 {
        let days = f64::from(calendar_days(class));
        let hours = match (class, session) {
            (AssetClass::Crypto | AssetClass::Fx, _) => 24.,
            (_, Session::UsExtended) => 16.,
            _ => 6.5,
        };
        match tf {
            Timeframe::D1 => days,
            Timeframe::W1 => 52.,
            Timeframe::MN1 => 12.,
            Timeframe::MN3 => 4.,
            _ => days * (hours * 3600. / tf.seconds() as f64).ceil(),
        }
    }

    fn exchange_local(class: AssetClass) -> bool {
        matches!(class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index)
    }

    fn finished_end(tf: Timeframe, class: AssetClass, session: Session, ts: i64) -> i64 {
        if tf.is_intraday() {
            let end = ts + tf.seconds();
            if class != AssetClass::Crypto && session != Session::Provider && session.contains(ts) {
                end.min(session.interval(tf, ts).1)
            } else {
                end
            }
        } else if tf == Timeframe::D1 && exchange_local(class) {
            // Daily provider stamps name an exchange-local trading date. Use
            // its actual close (DST and half days included), not UTC midnight.
            let session = if session == Session::Provider { Session::UsRegular } else { session };
            session.interval(tf, ts).1
        } else {
            bar_end(session, tf, ts)
        }
    }

    fn alignment_key(tf: Timeframe, class: AssetClass, session: Session, ts: i64, mixed_crypto: bool) -> i64 {
        if tf.is_intraday() {
            let end = finished_end(tf, class, session, ts);
            // Start with the bucket containing end - 1s. If its crypto close
            // would be after the exchange close, use the preceding completed
            // bucket instead. No OHLC close from the future is ever consumed.
            let containing = tf.bucket_start(end - 1);
            if mixed_crypto && class != AssetClass::Crypto && containing + tf.seconds() > end {
                containing - tf.seconds()
            } else {
                containing
            }
        } else {
            let date = if exchange_local(class) { Zone::NewYork.local(ts).date() } else { Zone::Utc.local(ts).date() };
            tf.bucket_start(date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp())
        }
    }

    fn alignment_note(tf: Timeframe, days: u32) -> String {
        let calendar = match days {
            252 => "exchange/equity session calendar (252 trading days)",
            260 => "FX weekday session calendar (260 weekdays)",
            _ => "crypto UTC calendar (365 days)",
        };
        let rule = if tf.is_intraday() {
            "UTC interval buckets after provider-offset normalisation: target the bucket containing bar end - 1s; if that crypto bucket ends after the exchange bar, use the preceding completed crypto bucket (e.g. equity 14:30-15:30 H1 uses crypto 14:00-15:00, never 15:00-16:00). No future crypto closes."
        } else {
            "Calendar-period inner join: US equity/ETF/index America/New_York trading date, crypto and other instruments UTC date; weekly/monthly/quarterly use the corresponding period key, not exact provider timestamps."
        };
        format!("{rule} Common bars follow the most restrictive {calendar}; no interpolation or forward fill.")
    }
    fn returns(bars: &[Bar]) -> Vec<f64> {
        bars.windows(2).map(|w| w[1].close.ln() - w[0].close.ln()).collect()
    }
    fn centered_sum(a: &[f64], b: &[f64]) -> f64 {
        let ma = analytics::mean(a);
        let mb = analytics::mean(b);
        a.iter().zip(b).map(|(a, b)| (a - ma) * (b - mb)).sum()
    }
    fn metric_row(bars: &[Bar], base: &[Bar], annual: f64) -> Value {
        let n = bars.len();
        if n < 2 {
            return json!({"return_percent":null,"annualised_volatility_percent":null,"max_drawdown_percent":null,
                "atr14_percent":null,"last_finished_close":bars.last().map(|b| b.close),
                "relative_strength":null,"correlation":null,"beta":null,"correlation_reason":"insufficient_overlap",
                "beta_reason":"insufficient_overlap"});
        }
        let r = returns(bars);
        let b = returns(base);
        let variance = centered_sum(&r, &r);
        let bv = centered_sum(&b, &b);
        let cov = centered_sum(&r, &b);
        let reason = if r.len() < 20 {
            Some("insufficient_aligned_returns")
        } else if variance <= 1e-24 || bv <= 1e-24 {
            Some("zero_variance")
        } else {
            None
        };
        let mut peak = bars[0].close;
        let mut dd: f64 = 0.;
        for bar in bars {
            peak = peak.max(bar.close);
            dd = dd.min((bar.close / peak - 1.) * 100.);
        }
        let last = bars[n - 1].close;
        let ratios: Vec<_> = bars.iter().zip(base).map(|(a, b)| a.close / b.close).collect();
        let slope = if n < 20 {
            None
        } else {
            let tail = &ratios[n - 20..];
            let x: Vec<_> = (0..20).map(f64::from).collect();
            let v = centered_sum(&x, tail);
            Some(if v > 1e-12 {
                "positive"
            } else if v < -1e-12 {
                "negative"
            } else {
                "flat"
            })
        };
        let atr = if n < 14 {
            None
        } else {
            indicators::atr(&Ohlcv::new(bars), 14).last().copied().filter(|v| v.is_finite()).map(|v| v / last * 100.)
        };
        json!({"return_percent":(last / bars[0].close - 1.) * 100.,
            "annualised_volatility_percent":if r.len() >= 2 { Some((variance / (r.len()-1) as f64 * annual).sqrt() * 100.) } else { None },
            "max_drawdown_percent":dd,"atr14_percent":atr,"last_finished_close":last,
            "relative_strength":{"ratio_change_percent":(ratios[n-1] / ratios[0] - 1.) * 100.,"ratio_slope_sign_20":slope,
                "slope_reason":if n < 20 { Some("insufficient_aligned_bars") } else { None }},
            "correlation":if reason.is_none() { Some((cov / (variance * bv).sqrt()).clamp(-1.,1.)) } else { None },
            "correlation_reason":reason,
            "beta":if r.len() >= 20 && bv > 1e-24 { Some(cov / bv) } else { None },
            "beta_reason":if r.len() < 20 { Some("insufficient_aligned_returns") } else if bv <= 1e-24 { Some("zero_base_variance") } else { None }})
    }

    pub(crate) async fn compare_symbols(host: &dyn DataHost, args: CompareSymbols, now: i64) -> ToolResult {
        let tf = match timeframe(Some(&args.timeframe)) {
            Ok(tf) => tf,
            Err(e) => return e,
        };
        let limit = args.lookback.unwrap_or(if args.from.is_some() { 1000 } else { 120 });
        let mut rows = Vec::new();
        let mut requests = Vec::new();
        for (index, symbol) in args.symbols.iter().enumerate() {
            match resolve_workflow_symbol(host, symbol).await {
                Err(error) => rows.push(error),
                Ok(key) => {
                    rows.push(json!({"requested_symbol":echo(symbol),"symbol":key}));
                    requests.push((
                        index,
                        ReadData {
                            symbol: key,
                            timeframe: Some(args.timeframe.clone()),
                            from: args.from,
                            to: args.to,
                            limit: Some(limit + 1),
                        },
                    ));
                }
            }
        }
        // Overscan before joining: 1.5 covers the crypto/trading-day ratio
        // (365/252) and modest gaps, bounded at 1500 plus one detection bar.
        let fetch_limit = if args.from.is_some() { limit } else { (limit * 3).div_ceil(2).min(1500) };
        for (_, request) in &mut requests {
            request.limit = Some(fetch_limit + 1);
        }
        let mixed_crypto = requests.iter().any(|(_, r)| tr_data::security(&r.symbol).class == AssetClass::Crypto)
            && requests.iter().any(|(_, r)| tr_data::security(&r.symbol).class != AssetClass::Crypto);
        let mut provider_truncated = false;
        let mut inputs = Vec::new();
        // Poll at most four history futures at once; no runtime or new dependency.
        // Batches deliberately preserve request order irrespective of completion order.
        for chunk in requests.chunks(4) {
            let mut pending: Vec<_> =
                chunk.iter().map(|(_, request)| Some(Box::pin(history(host, request, now)))).collect();
            let mut results: Vec<_> = (0..chunk.len()).map(|_| None).collect();
            poll_fn(|cx| {
                for (i, future) in pending.iter_mut().enumerate() {
                    if let Some(f) = future
                        && let Poll::Ready(result) = f.as_mut().poll(cx)
                    {
                        results[i] = Some(result);
                        *future = None;
                    }
                }
                if pending.iter().all(Option::is_none) { Poll::Ready(()) } else { Poll::Pending }
            })
            .await;
            for ((index, request), result) in chunk.iter().zip(results) {
                match result.unwrap() {
                    Err(_) => rows[*index]["error"] = json!("history_unavailable_or_invalid"),
                    Ok((mut h, _)) => {
                        if h.bars.iter().any(|b| b.close <= 0.) {
                            rows[*index]["error"] = json!("non_positive_close");
                            continue;
                        }
                        let class = tr_data::security(&request.symbol).class;
                        let forming: Vec<i64> = h
                            .bars
                            .iter()
                            .filter(|b| finished_end(tf, class, h.session, b.ts) > now)
                            .map(|b| b.ts)
                            .collect();
                        h.bars.retain(|b| finished_end(tf, class, h.session, b.ts) <= now);
                        provider_truncated |= args.from.is_some() && h.bars.len() > fetch_limit;
                        if h.bars.is_empty() {
                            rows[*index]["error"] = json!("no_finished_bars");
                            rows[*index]["forming_bar_times"] =
                                json!(forming.iter().filter_map(|ts| iso(*ts)).collect::<Vec<_>>());
                            continue;
                        }
                        for bar in &mut h.bars {
                            bar.ts = alignment_key(tf, class, h.session, bar.ts, mixed_crypto);
                        }
                        h.bars = Series::new(&request.symbol, tf, h.bars).bars;
                        inputs.push(Input { index: *index, key: request.symbol.clone(), history: h, forming });
                    }
                }
            }
        }
        let mut times = common_times(&inputs);
        while times.len() > limit {
            times.pop_first();
        }
        // The common series follows the most restrictive returned calendar and
        // session, irrespective of request order (including index/FX/futures).
        let calendar = inputs
            .iter()
            .map(|i| {
                let class = tr_data::security(&i.key).class;
                (calendar_days(class), annual_bars(tf, class, i.history.session))
            })
            .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)));
        let (days, annual) = calendar.unwrap_or((365, annual_bars(tf, AssetClass::Crypto, Session::Provider)));
        let aligned: Vec<Vec<Bar>> =
            inputs.iter().map(|i| i.history.bars.iter().filter(|b| times.contains(&b.ts)).copied().collect()).collect();
        let base_index = inputs.iter().position(|i| i.index == 0);
        let mut warnings = Vec::new();
        if inputs.len() < 2 || times.len() < 21 {
            warnings.push(
                "insufficient_overlap: at least 21 common finished bars are needed for correlation and beta".to_owned(),
            );
        }
        if base_index.is_none() {
            warnings.push(
                "base_unavailable: relative strength, correlation and beta are null; base was not substituted".into(),
            );
        }
        if provider_truncated {
            warnings.push("date_window_capped: at most 1000 finished bars per symbol".into());
        }
        let mut dropped = serde_json::Map::new();
        for (i, input) in inputs.iter().enumerate() {
            let row = &mut rows[input.index];
            let bars = &aligned[i];
            let mut metrics = metric_row(bars, base_index.map_or(bars, |base| &aligned[base]), annual);
            if base_index.is_none() {
                for field in ["relative_strength", "correlation", "beta"] {
                    metrics[field] = Value::Null;
                }
                metrics["correlation_reason"] = json!("base_unavailable");
                metrics["beta_reason"] = json!("base_unavailable");
            }
            if inputs.len() < 2 {
                row["error"] = json!("insufficient_valid_symbols");
            } else {
                row.as_object_mut().unwrap().extend(metrics.as_object().unwrap().clone());
            }
            let available = input
                .history
                .bars
                .iter()
                .filter(|b| times.first().zip(times.last()).is_some_and(|(from, to)| b.ts >= *from && b.ts <= *to))
                .count();
            let count = available.saturating_sub(bars.len());
            row["dropped_bars"] = json!(count);
            dropped.insert(input.key.clone(), json!(count));
            row["forming_bar_times"] = json!(input.forming.iter().filter_map(|ts| iso(*ts)).collect::<Vec<_>>());
            row["finished_bars_available"] = json!(available);
            row["price_basis"] = json!(input.history.price_basis);
            row["session"] = json!(input.history.session.label());
            row["source"] = json!(input.history.source);
            row["simulated"] = json!(input.history.simulated);
            if input.history.simulated != Some(false) {
                warnings.push(format!(
                    "{}: {}",
                    input.key,
                    if input.history.simulated == Some(true) {
                        "simulated data; not market observations"
                    } else {
                        "simulation provenance unavailable; not verified live"
                    }
                ));
            }
        }
        let mut rank: Vec<_> =
            rows.iter().filter_map(|r| Some((r["symbol"].as_str()?, r["return_percent"].as_f64()?))).collect();
        rank.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let rank: Vec<_> = rank
            .into_iter()
            .enumerate()
            .map(|(i, (symbol, r))| json!({"rank":i+1,"symbol":symbol,"return_percent":r}))
            .collect();
        let failed = rows.iter().filter(|r| !r["error"].is_null()).count();
        batch_result(json!({"version":1,"as_of":iso(now),"timeframe":tf.label(),"base":rows[0]["symbol"],
            "requested_window":{"lookback":args.lookback.or(args.from.is_none().then_some(120)),"from":args.from.and_then(iso),"to":args.to.and_then(iso)},
            "window":{"from":times.first().and_then(|t|iso(*t)),"to":times.last().and_then(|t|iso(*t))},
            "bars_used":times.len(),"finished_bars_only":true,"dropped_bars":dropped,"symbols":rows,"rank":rank,"failed":failed,
            "annualisation":{"bars_per_year":annual,"method":"Nominal session bars/year; sample standard deviation of aligned log returns. Gaps are single aligned returns, not interpolated."},
            "alignment_note":alignment_note(tf, days),
            "metric_notes":"Return and drawdown use aligned closes; drawdown is negative peak-to-trough. ATR(14) uses Wilder smoothing on aligned OHLC bars. Ratio slope is OLS over the last 20 aligned ratios. Correlation and beta require 20 aligned log returns.",
            "warnings":warnings})).capped()
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        fn input(index: usize, times: &[i64]) -> Input {
            Input {
                index,
                key: format!("EQ{index} US Equity"),
                history: History {
                    bars: times.iter().map(|t| Bar::new(*t, 10., 11., 9., 10., 1.)).collect(),
                    ..History::default()
                },
                forming: vec![],
            }
        }
        #[test]
        fn alignment_is_an_exact_order_independent_inner_join_without_fill() {
            let mut inputs = vec![input(0, &[1, 2, 4, 5]), input(1, &[2, 3, 4]), input(2, &[0, 2, 4, 6])];
            assert_eq!(common_times(&inputs), BTreeSet::from([2, 4]));
            inputs.reverse();
            assert_eq!(common_times(&inputs), BTreeSet::from([2, 4]));
            inputs.push(input(3, &[8]));
            assert!(common_times(&inputs).is_empty());
        }
        fn timestamp(text: &str) -> i64 {
            DateTime::parse_from_rfc3339(text).unwrap().timestamp()
        }
        #[test]
        fn calendar_keys_use_new_york_dates_dst_and_week_month_periods() {
            let key = |tf, class, text| alignment_key(tf, class, Session::Provider, timestamp(text), true);
            for (equity, crypto) in [
                ("2026-03-06T14:30:00Z", "2026-03-06T00:00:00Z"),
                ("2026-03-09T13:30:00Z", "2026-03-09T00:00:00Z"),
                // This UTC timestamp is still the prior exchange-local date.
                ("2026-03-10T00:30:00Z", "2026-03-09T00:00:00Z"),
            ] {
                assert_eq!(
                    key(Timeframe::D1, AssetClass::Equity, equity),
                    key(Timeframe::D1, AssetClass::Crypto, crypto)
                );
            }
            for (tf, exchange, crypto) in [
                (Timeframe::W1, "2026-03-13T13:30:00Z", "2026-03-09T00:00:00Z"),
                (Timeframe::MN1, "2026-03-31T13:30:00Z", "2026-03-01T00:00:00Z"),
                (Timeframe::MN3, "2026-03-31T13:30:00Z", "2026-01-01T00:00:00Z"),
            ] {
                assert_eq!(key(tf, AssetClass::Index, exchange), key(tf, AssetClass::Crypto, crypto));
            }
        }
        #[test]
        fn offset_hourly_bars_join_completed_crypto_without_future_closes() {
            let start = timestamp("2026-03-09T14:00:00Z");
            let mut equity = input(0, &[start + 1800, start + 5400]);
            let mut crypto = input(1, &[start, start + 3600, start + 7200]);
            crypto.key = "COINBASE:BTC-USD Curncy".into();
            let equity_end = start + 5400;
            for bar in &mut equity.history.bars {
                bar.ts = alignment_key(Timeframe::H1, AssetClass::Equity, Session::Provider, bar.ts, true);
            }
            for (i, bar) in crypto.history.bars.iter_mut().enumerate() {
                bar.close = [100., 110., 999.][i];
                bar.ts = alignment_key(Timeframe::H1, AssetClass::Crypto, Session::Provider, bar.ts, true);
            }
            let inputs = [equity, crypto];
            let times = common_times(&inputs);
            assert_eq!(times, BTreeSet::from([start, start + 3600]));
            let first_crypto = inputs[1].history.bars.iter().find(|b| b.ts == inputs[0].history.bars[0].ts).unwrap();
            assert_eq!(first_crypto.close, 100.);
            assert!(first_crypto.ts + Timeframe::H1.seconds() <= equity_end);
            for offset in [0, 1, 1799, 1800, 3599] {
                let end = start + offset + 3600;
                let key = alignment_key(Timeframe::H1, AssetClass::Equity, Session::Provider, start + offset, true);
                assert!(key + 3600 <= end, "no future crypto for provider offset {offset}");
            }
            let note = alignment_note(Timeframe::H1, 252);
            assert!(note.contains("bar end - 1s") && note.contains("preceding completed"));
        }
        #[test]
        fn insufficient_overlap_zero_variance_and_atr_warmup_are_explicit() {
            let empty = metric_row(&[], &[], 252.);
            assert_eq!(empty["correlation_reason"], "insufficient_overlap");
            let input = input(0, &(0..21).collect::<Vec<_>>());
            let bars = &input.history.bars;
            let flat = metric_row(bars, bars, 252.);
            assert_eq!(flat["correlation_reason"], "zero_variance");
            assert_eq!(flat["beta_reason"], "zero_base_variance");
            assert_eq!(flat["return_percent"], 0.);
            assert_eq!(flat["max_drawdown_percent"], 0.);
            assert_eq!(flat["atr14_percent"], 20.);
            let short = metric_row(&bars[..13], &bars[..13], 252.);
            assert!(short["atr14_percent"].is_null());
            assert_eq!(short["correlation_reason"], "insufficient_aligned_returns");
        }
        #[test]
        fn verbose_discovery_is_capped_without_losing_version_or_error_rows() {
            let rows = vec![json!({"symbol":"UNKNOWN","error":"unknown_symbol","hint":"🦀".repeat(4000)}); 25];
            let result = batch_result(json!({"version":1,"symbols":rows}));
            let crate::Content::Text(text) = &result.content[0] else { panic!("JSON result") };
            assert!(text.len() <= MAX_TEXT_BYTES);
            let value: Value = serde_json::from_str(text).unwrap();
            assert_eq!(value["version"], 1);
            assert_eq!(value["symbols"].as_array().unwrap().len(), 25);
            assert_eq!(value["truncated"], true);
            assert_eq!(value["symbols"][24]["error"], "unknown_symbol");
        }
        #[test]
        fn nominal_annualisation_uses_equity_calendar_for_a_mixed_join() {
            for class in [AssetClass::Equity, AssetClass::Etf, AssetClass::Index, AssetClass::Govt, AssetClass::Cmdty] {
                assert_eq!(annual_bars(Timeframe::D1, class, Session::Provider), 252.);
                assert_eq!(annual_bars(Timeframe::H1, class, Session::UsRegular), 252. * 7.);
                assert_eq!(annual_bars(Timeframe::H1, class, Session::UsExtended), 252. * 16.);
            }
            assert_eq!(annual_bars(Timeframe::D1, AssetClass::Fx, Session::Provider), 260.);
            assert_eq!(annual_bars(Timeframe::D1, AssetClass::Crypto, Session::Provider), 365.);
            assert_eq!(annual_bars(Timeframe::H1, AssetClass::Crypto, Session::Provider), 365. * 24.);
        }
    }
}
// End A5b-MULTI data tools.

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
            || !(crate::args::MIN_TIMESTAMP..=crate::args::MAX_TIMESTAMP).contains(&b.ts)
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
    match summary(&history, &args, tf, now) {
        Ok(value) => ToolResult::json(value),
        Err(error) => error,
    }
}

// History intraday labels use core's bucket grid (only regular H2/H4 are
// session-aligned). Daily labels name a UTC calendar date, not the previous NY
// evening; use that date's session close, including early closes and DST.
fn bar_end(session: tr_core::session::Session, tf: Timeframe, ts: i64) -> i64 {
    use tr_core::session::{Session, bar_interval};
    if tf == Timeframe::D1 && session != Session::Provider {
        let midday = tf.bucket_start(ts) + 12 * 3600;
        session.interval(tf, midday).1
    } else {
        bar_interval(tf, session, ts).1
    }
}

fn summary(history: &History, args: &ReadData, tf: Timeframe, now: i64) -> Result<Value, ToolResult> {
    let bars = &history.bars;
    let first = bars.first();
    let last = bars.last();
    let ohlcv = stats(bars)?;
    let close_return = first
        .zip(last)
        .map(|(a, b)| percent(a.close, b.close, "returns.close_to_close_percent"))
        .transpose()?
        .flatten();
    let open_return =
        first.zip(last).map(|(a, b)| percent(a.open, b.close, "returns.open_to_close_percent")).transpose()?.flatten();
    let mut value = json!({
        "version":1,"symbol":history.instrument.as_ref().map_or_else(||args.symbol.clone(),CryptoInstrument::canonical_ticker),
        "requested_symbol":args.symbol,"timeframe":tf.label(),"instrument":history.instrument,
        "price_basis":history.price_basis,"session":history.session.label(),"timezone":"UTC",
        "count":bars.len(),"first":first.and_then(|b|iso(b.ts)),"last":last.and_then(|b|iso(b.ts)),
        "requested_range":{"from":args.from.and_then(iso),"to":args.to.and_then(iso),"limit":args.limit},
        "range_note":"Statistics cover returned bars only; provider availability and limit may bound the requested range.",
        "ohlcv":ohlcv,
        "returns":{"close_to_close_percent":close_return,"open_to_close_percent":open_return},
        "in_progress_last_bar":last.is_some_and(|b| now>=b.ts && now<bar_end(history.session,tf,b.ts)),
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
                    last.is_some_and(|last| last.ts == b.ts) && now >= b.ts && now < bar_end(history.session, tf, b.ts)
                ])
            })
            .collect();
        value["close_series"] = analytics::lttb(bars, points).into_iter().map(|(t, v)| json!([iso(t), v])).collect();
        value["emitted"] = json!({"recent_bars":recent,"close_points":value["close_series"].as_array().unwrap().len()});
        if value.to_string().len() <= MAX_TEXT_BYTES {
            return Ok(value);
        }
        value["truncated"] = true.into();
        value["marker"] =
            "[truncated: recent bars and LTTB points reduced to fit 16 KiB; full-range statistics retained]".into();
        if recent <= 1 && points <= 2 {
            return Ok(value);
        }
        recent = (recent * 3 / 4).max(1).min(bars.len());
        points = (points * 3 / 4).max(2).min(bars.len());
    }
}

fn percent(from: f64, to: f64, field: &str) -> Result<Option<f64>, ToolResult> {
    if from == 0. {
        return Ok(None);
    }
    analytics::finite((to / from - 1.) * 100., field).map(Some)
}

fn stats(bars: &[Bar]) -> Result<Value, ToolResult> {
    if bars.is_empty() {
        return Ok(Value::Null);
    }
    let mut columns = serde_json::Map::new();
    for (name, get) in [
        ("open", (|b: &Bar| b.open) as fn(&Bar) -> f64),
        ("high", |b: &Bar| b.high),
        ("low", |b: &Bar| b.low),
        ("close", |b: &Bar| b.close),
        ("volume", |b: &Bar| b.volume),
    ] {
        let min = bars.iter().map(get).fold(f64::INFINITY, f64::min);
        let max = bars.iter().map(get).fold(f64::NEG_INFINITY, f64::max);
        let mean = analytics::mean(&bars.iter().map(get).collect::<Vec<_>>());
        analytics::finite(mean, &format!("ohlcv.{name}.mean"))?;
        let sum = analytics::finite(bars.iter().map(get).sum::<f64>(), &format!("ohlcv.{name}.sum"))?;
        columns.insert(name.into(), json!({"min":min,"max":max,"mean":mean,"sum":sum}));
    }
    Ok(Value::Object(columns))
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
    let mut value = match analytics::analyze(bars, tf, history.session, args.mode, args.top.unwrap_or(10)) {
        Ok(value) => value,
        Err(error) => return error,
    };
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

/// Agent-only input normalization: never changes chart parsing or crypto venue routing.
fn symbol_query(input: &str) -> Result<String, String> {
    let input = input.trim();
    // Full crypto keys returned by workflow resolution may include the yellow key.
    if input.starts_with("COINBASE:") || input.starts_with("BINANCE:") {
        return Ok(input.into());
    }
    if let Some((venue, ticker)) = input.split_once(':')
        && !venue.is_empty()
        && venue.bytes().all(|c| c.is_ascii_uppercase())
        && !ticker.chars().any(char::is_whitespace)
    {
        let suffix = match venue {
            "NASDAQ" | "NYSE" | "AMEX" | "NYSEARCA" | "ARCA" | "BATS" => Some(""),
            // Existing Canadian listing keys use Yahoo's .TO/.V tickers.
            "TSX" => Some(".TO"),
            "TSXV" => Some(".V"),
            _ => None,
        };
        if let Some(suffix) = suffix
            && tr_core::security::looks_like_ticker(&ticker.to_ascii_uppercase())
        {
            return Ok(format!("{}{suffix} US Equity", ticker.to_ascii_uppercase()));
        }
        return Err("Use the symbol returned by find_symbol, e.g. \"NVDA US Equity\"".into());
    }
    // A colon in free text must not trigger the pair/venue shorthand either.
    if input.contains(':') {
        return Ok(input.into());
    }
    let words: Vec<_> = input.split_whitespace().collect();
    if let [pair, venue] = words.as_slice()
        && matches!(venue.to_ascii_uppercase().as_str(), "COINBASE" | "BINANCE")
    {
        let product =
            if venue.eq_ignore_ascii_case("COINBASE") { pair.replace('/', "-") } else { pair.replace(['/', '-'], "") };
        return Ok(format!("{}:{product}", venue.to_ascii_uppercase()));
    }
    Ok(input.into())
}

/// Unlike Security::parse, require the entire input to be a well-formed instrument.
fn workflow_security(input: &str) -> Option<tr_core::Security> {
    let words: Vec<_> = input.split_whitespace().collect();
    let (mut security, used) = tr_core::Security::resolve_tokens(&words)?;
    if used != words.len() {
        return None;
    }
    if words.len() == 1 && input.contains(['/', '-']) && security.class != AssetClass::Fx {
        let compact = input.replace(['/', '-'], "");
        if let Ok(instrument) = CryptoInstrument::parse(&compact) {
            security = instrument.security();
        }
    }
    if security.class == AssetClass::Crypto {
        security = security.crypto_instrument().ok()?.security();
    }
    Some(security)
}

fn search_instrument(hit: &tr_data::SearchHit) -> Option<CryptoInstrument> {
    if hit.class != AssetClass::Crypto {
        return None;
    }
    let tokens: Vec<_> = hit.key.split_whitespace().collect();
    tr_core::Security::resolve_tokens(&tokens).and_then(|(security, used)| {
        (used == tokens.len()).then(|| CryptoInstrument::from_security(&security).ok()).flatten()
    })
}

fn find_symbol_value(hit: &tr_data::SearchHit) -> String {
    search_instrument(hit).map_or_else(|| hit.key.clone(), |i| i.canonical_ticker())
}

/// Resolve with at most two searches, retaining discovery candidates for actionable errors.
pub(crate) async fn resolve_workflow_symbol(host: &dyn DataHost, input: &str) -> Result<String, serde_json::Value> {
    let echo: String = crate::scrub_text(input).chars().take(64).collect();
    let query = symbol_query(input)
        .map_err(|message| json!({"symbol":echo,"error":"invalid_symbol","message":crate::scrub_text(&message)}))?;
    let security = workflow_security(&query);
    let bare = query.split_whitespace().count() == 1 && !query.contains(':');
    let fallback = security.as_ref().map(|s| s.crypto_instrument().map_or_else(|_| s.ticker.clone(), |i| i.base));
    let mut candidates = Vec::new();
    let mut search_failed = false;
    for search in std::iter::once(query.as_str()).chain(fallback.as_deref().filter(|s| !s.eq_ignore_ascii_case(&query)))
    {
        match host.search(search).await {
            Ok(hits) => {
                if let Some(target) = &security {
                    // Bare tickers are not fully qualified keys: even an exact default
                    // equity key must not win over another listing or a crypto base.
                    let mut matches = Vec::new();
                    for hit in &hits {
                        if let Some(resolved) = workflow_security(&hit.key)
                            && (resolved.key() == target.key()
                                || bare
                                    && (resolved.ticker == target.ticker
                                        || search_instrument(hit).is_some_and(|i| i.base.eq_ignore_ascii_case(&query))))
                            && !matches.iter().any(|(key, _)| key == &resolved.key())
                        {
                            matches.push((resolved.key(), find_symbol_value(hit)));
                        }
                    }
                    if matches.len() == 1 {
                        return Ok(matches.remove(0).0);
                    }
                    if matches.len() > 1 {
                        let hints: Vec<_> =
                            matches.iter().take(5).map(|(_, symbol)| crate::scrub_text(symbol)).collect();
                        return Err(json!({"symbol":echo,"error":"ambiguous_symbol","hints":hints,
                            "hint":crate::scrub_text(&format!("Choose a symbol returned by find_symbol: {}", hints.join(", ")))}));
                    }
                }
                candidates.extend(hits);
            }
            Err(_) => search_failed = true,
        }
    }
    let mut error = json!({"symbol":echo,"error":if search_failed {"search_unavailable"} else {"unknown_symbol"}});
    if search_failed {
        error["message"] = json!("Symbol search failed; try again");
    }
    if let Some(hit) = candidates.first() {
        let symbol = find_symbol_value(hit);
        error["hint"] = json!(crate::scrub_text(&format!("did you mean \"{symbol}\"")));
    }
    Err(error)
}

pub(crate) async fn find_symbol(host: &dyn DataHost, args: FindSymbol) -> ToolResult {
    let query = match symbol_query(&args.query) {
        Ok(query) => query,
        Err(message) => return ToolResult::error("invalid_symbol", &message, Some("query")),
    };
    let search = if args.query.contains(':') && query != args.query.trim() && !query.contains(':') {
        workflow_security(&query).map_or(query.clone(), |s| s.ticker)
    } else {
        query
    };
    let hits = match host.search(&search).await {
        Ok(hits) => hits,
        Err(_) => return ToolResult::error("search_unavailable", "Symbol search failed; try again", None),
    };
    let total = hits.len();
    let mut results = Vec::new();
    for hit in hits.into_iter().take(100) {
        let instrument = search_instrument(&hit);
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
