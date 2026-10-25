//! Deterministic analytics shared by data tools and future chart integrations.
use serde::Serialize;
use serde_json::{Value, json};
use tr_core::{
    Bar, Timeframe, chart_analytics,
    indicators::{self, Ohlcv, PivotKind},
    session::{Session, Zone},
};

use crate::{ToolResult, args::Mode};

pub(crate) fn finite(value: f64, field: &str) -> Result<f64, ToolResult> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ToolResult::error("invalid_data", "Computed number is not finite", Some(field)))
    }
}

/// Scale before summing so a representable mean does not need a representable sum.
pub(crate) fn mean(values: &[f64]) -> f64 {
    let scale = values.iter().map(|v| v.abs()).fold(0., f64::max);
    if scale == 0. { 0. } else { (values.iter().map(|v| v / scale).sum::<f64>() / values.len() as f64) * scale }
}

fn computed_last(values: &[f64], ready: bool, field: &str) -> Result<Option<f64>, ToolResult> {
    if ready { values.last().map(|&v| finite(v, field)).transpose() } else { Ok(None) }
}

#[derive(Debug, Clone, Serialize)]
pub struct SupportResistance {
    pub price: f64,
    pub zone_width: f64,
    pub low: f64,
    pub high: f64,
    pub touches: usize,
    pub last_touch_time: i64,
    pub score: f64,
    pub in_play: bool,
    pub trimmed: bool,
    pub side: &'static str,
}

/// Cluster confirmed two-sided fractal highs/lows within 0.35 ATR(14). Clusters have
/// bounded diameter (no single-link chaining). Touches count distinct swing bars;
/// ranking combines touch count, normalized recency and relative touched-bar volume.
pub fn support_resistance(bars: &[Bar], top: usize) -> Vec<SupportResistance> {
    if bars.is_empty() || top == 0 {
        return Vec::new();
    }
    let close = bars.last().unwrap().close;
    let o = Ohlcv::new(bars);
    let atr = last_finite(&indicators::atr(&o, 14))
        .unwrap_or_else(|| indicators::true_range(&o).iter().sum::<f64>() / bars.len() as f64);
    let tolerance = (atr * 0.35).max(bars.last().unwrap().close.abs() * 1e-8).max(1e-8);
    let (highs, lows) = indicators::fractals(&o, 2);
    let mut candidates: Vec<_> = highs
        .iter()
        .zip(&lows)
        .enumerate()
        .flat_map(|(i, (&h, &l))| [(h, i), (l, i)].into_iter().filter(|(p, _)| p.is_finite()))
        .collect();
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mean_volume = (bars.iter().map(|b| b.volume).sum::<f64>() / bars.len() as f64).max(1.);
    let mut out = Vec::new();
    let mut start = 0;
    while start < candidates.len() {
        let mut end = start + 1;
        while end < candidates.len() && candidates[end].0 - candidates[start].0 <= 2. * tolerance {
            end += 1;
        }
        let cluster = &candidates[start..end];
        let mut indices: Vec<_> = cluster.iter().map(|c| c.1).collect();
        indices.sort_unstable();
        indices.dedup();
        let last = *indices.last().unwrap();
        let volume = indices.iter().map(|&i| bars[i].volume).sum::<f64>() / indices.len() as f64;
        let touches = indices.len();
        let price = cluster.iter().map(|c| c.0).sum::<f64>() / cluster.len() as f64;
        // A skewed cluster need not be centered on its mean: retain every touch
        // while padding each side to at least the tolerance around price.
        let touch_prices: Vec<_> = cluster.iter().map(|c| c.0).collect();
        let cap = (1.5 * atr).max(2. * tolerance);
        let (low, high, trimmed) = capped_zone_bounds(&touch_prices, price, tolerance, cap);
        let zone_width = high - low;
        out.push(SupportResistance {
            price,
            zone_width,
            low,
            high,
            touches,
            last_touch_time: bars[last].ts,
            score: touches as f64 + last as f64 / bars.len() as f64 + (1. + volume / mean_volume).ln(),
            in_play: low <= close && close <= high,
            trimmed,
            side: if high < close {
                "support"
            } else if low > close {
                "resistance"
            } else {
                "in_play"
            },
        });
        start = end;
    }
    let distance = |zone: &SupportResistance| (zone.low - close).max(close - zone.high).max(0.);
    out.sort_by(|a, b| {
        distance(a).total_cmp(&distance(b)).then(b.score.total_cmp(&a.score)).then(a.price.total_cmp(&b.price))
    });
    // Reserve the nearest zone on each side before filling the remaining slots.
    let support = out.iter().position(|z| z.high < close);
    let resistance = out.iter().position(|z| z.low > close);
    let mut selected: Vec<_> = out.iter().enumerate().filter_map(|(i, z)| z.in_play.then_some(i)).take(3).collect();
    for i in [support, resistance].into_iter().flatten() {
        let on_side = |z: &SupportResistance| if out[i].high < close { z.low <= close } else { z.high >= close };
        if !selected.contains(&i) && selected.iter().filter(|&&j| on_side(&out[j])).count() < 3 {
            selected.push(i);
        }
    }
    let mut below = selected.iter().filter(|&&i| out[i].low <= close).count();
    let mut above = selected.iter().filter(|&&i| out[i].high >= close).count();
    for (i, zone) in out.iter().enumerate() {
        if selected.len() >= top.max(2) {
            break;
        }
        if selected.contains(&i) {
            continue;
        }
        if (zone.low <= close && below >= 3) || (zone.high >= close && above >= 3) {
            continue;
        }
        selected.push(i);
        below += usize::from(zone.low <= close);
        above += usize::from(zone.high >= close);
    }
    selected.sort_unstable();
    selected.into_iter().map(|i| out[i].clone()).collect()
}

/// Trim padding first. If touches themselves exceed the cap, center the cap on
/// the densest window of touches (ties favor the median), rather than on outliers.
fn capped_zone_bounds(prices: &[f64], price: f64, tolerance: f64, cap: f64) -> (f64, f64, bool) {
    let low = prices[0].min(price - tolerance);
    let high = prices[prices.len() - 1].max(price + tolerance);
    if high - low <= cap {
        return (low, high, false);
    }
    let span = prices[prices.len() - 1] - prices[0];
    let center = if span <= cap {
        prices[prices.len() / 2]
    } else {
        let (start, end) = (0..prices.len())
            .map(|i| {
                let end = prices.partition_point(|p| *p <= prices[i] + cap);
                (i, end)
            })
            .max_by_key(|&(i, end)| (end - i, std::cmp::Reverse(i.abs_diff(prices.len() / 2))))
            .unwrap();
        prices[(start + end - 1) / 2]
    };
    let mut low = (center - cap / 2.).clamp(low, high - cap);
    if span <= cap {
        low = low.clamp(prices[prices.len() - 1] - cap, prices[0]);
    }
    let high = low + cap;
    let trimmed = prices.iter().any(|p| *p < low || *p > high);
    (low, high, trimmed)
}

/// Largest-triangle-three-buckets on time/close. Thresholds below two still retain
/// both endpoints for a nontrivial series. Empty buckets never fabricate points.
pub fn lttb(bars: &[Bar], threshold: usize) -> Vec<(i64, f64)> {
    let threshold = threshold.max(2);
    if bars.len() <= threshold {
        return bars.iter().map(|b| (b.ts, b.close)).collect();
    }
    let last = bars.len() - 1;
    let mut out = vec![(bars[0].ts, bars[0].close)];
    if threshold == 2 {
        out.push((bars[last].ts, bars[last].close));
        return out;
    }
    let bucket = (bars.len() - 2) as f64 / (threshold - 2) as f64;
    let mut a = 0;
    for i in 0..threshold - 2 {
        let avg_start = (((i + 1) as f64 * bucket).floor() as usize + 1).min(last);
        let avg_end = (((i + 2) as f64 * bucket).floor() as usize + 1).min(bars.len());
        let avg = &bars[avg_start..avg_end];
        let x = avg.iter().map(|b| (b.ts - bars[0].ts) as f64).sum::<f64>() / avg.len() as f64;
        let y = avg.iter().map(|b| b.close).sum::<f64>() / avg.len() as f64;
        let start = (i as f64 * bucket).floor() as usize + 1;
        let end = (((i + 1) as f64 * bucket).floor() as usize + 1).min(last);
        let ax = (bars[a].ts - bars[0].ts) as f64;
        a = (start..end)
            .max_by(|&j, &k| {
                let area = |index: usize| {
                    ((ax - x) * (bars[index].close - bars[a].close)
                        - (ax - (bars[index].ts - bars[0].ts) as f64) * (y - bars[a].close))
                        .abs()
                };
                area(j).total_cmp(&area(k)).then(k.cmp(&j))
            })
            .unwrap_or(start);
        out.push((bars[a].ts, bars[a].close));
    }
    out.push((bars[last].ts, bars[last].close));
    out
}

pub(crate) fn last_finite(values: &[f64]) -> Option<f64> {
    values.last().copied().filter(|v| v.is_finite())
}

pub(crate) fn analyze(
    bars: &[Bar],
    tf: Timeframe,
    session: Session,
    mode: Mode,
    top: usize,
) -> Result<Value, ToolResult> {
    let o = Ohlcv::new(bars);
    let value = match mode {
        Mode::Swings => {
            let input = indicators::registry::Input::new(bars, tf).with_session(session);
            let fractals = indicators::registry::Spec::new("FRACTALS", &[]).unwrap().compute(&input);
            let zigzag = indicators::registry::Spec::new("ZIGZAG", &[]).unwrap().compute(&input);
            let points = |values: &[f64]| -> Vec<Value> {
                values
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.is_finite())
                    .map(|(i, v)| json!({"time":bars[i].ts,"price":v,"confirmed_at":bars.get(i+2).map(|b|b.ts)}))
                    .collect()
            };
            let line: Vec<_> = bars
                .iter()
                .zip(&zigzag[0].values)
                .filter_map(|(b, v)| v.is_finite().then_some(Bar { close: *v, ..*b }))
                .collect();
            json!({"fractal_periods":2,"highs":points(&fractals[0].values),"lows":points(&fractals[1].values),
                "zigzag":{"deviation_percent":5,"last_leg_repaints":true,"series":lttb(&line,300)}})
        }
        Mode::Trend => trend(bars, &o)?,
        Mode::Levels => {
            let session_key = |ts| {
                let ts = if session != Session::Provider && tf.is_intraday() {
                    Zone::NewYork.local(ts).and_utc().timestamp()
                } else {
                    ts
                };
                indicators::session_key(tf)(ts)
            };
            let ready = bars.windows(2).any(|w| session_key(w[0].ts) != session_key(w[1].ts));
            let mut pivots = serde_json::Map::new();
            for (name, values) in indicators::pivots(&o, session_key, PivotKind::Classic) {
                pivots.insert(name.into(), json!(computed_last(&values, ready, &format!("pivots.{name}"))?));
            }
            let levels = support_resistance(bars, top);
            for (i, level) in levels.iter().enumerate() {
                for (name, value) in [
                    ("price", level.price),
                    ("zone_width", level.zone_width),
                    ("low", level.low),
                    ("high", level.high),
                    ("score", level.score),
                ] {
                    finite(value, &format!("support_resistance[{i}].{name}"))?;
                }
            }
            json!({"pivots":pivots,"pivot_kind":"classic","pivot_session":"core timeframe session key",
                "support_resistance":levels})
        }
        Mode::VolumeProfile => {
            let profile = chart_analytics::volume_profile(bars, 48)
                .map(|p| {
                    finite(p.step, "profile.step")?;
                    for &volume in &p.volume {
                        finite(volume, "profile.volume")?;
                    }
                    finite(p.volume.iter().sum(), "profile.total_volume")?;
                    let poc = finite(p.low + (p.poc as f64 + 0.5) * p.step, "profile.poc")?;
                    let low = finite(p.low + p.value_low as f64 * p.step, "profile.value_area.low")?;
                    let high = finite(p.low + (p.value_high + 1) as f64 * p.step, "profile.value_area.high")?;
                    Ok(json!({"poc":poc,"value_area":{"low":low,"high":high,"fraction":0.7},
                    "bins":p.volume.len(),"estimated_from_ohlcv":true}))
                })
                .transpose()?;
            // Keep core's app behaviour unchanged. Normalizing both units here
            // avoids overflow in its typical-price sum and price*volume products.
            let price_scale = bars
                .iter()
                .flat_map(|b| [b.high.abs(), b.low.abs(), b.close.abs()])
                .fold(0., f64::max)
                .max(f64::MIN_POSITIVE);
            let volume_scale = bars.iter().map(|b| b.volume).fold(0., f64::max);
            let vwap = if volume_scale == 0. {
                None
            } else {
                let scaled: Vec<_> = bars
                    .iter()
                    .map(|b| Bar {
                        high: b.high / price_scale,
                        low: b.low / price_scale,
                        close: b.close / price_scale,
                        volume: b.volume / volume_scale,
                        ..*b
                    })
                    .collect();
                let values = chart_analytics::anchored_vwap(&scaled, bars[0].ts);
                Some(finite(values.last().unwrap() * price_scale, "anchored_vwap")?)
            };
            json!({"profile":profile,"anchored_vwap":vwap,"vwap_anchor":bars.first().map(|b|b.ts)})
        }
        Mode::Volatility => {
            let logs: Vec<_> = bars
                .windows(2)
                .filter(|w| w[0].close > 0. && w[1].close > 0.)
                .map(|w| w[1].close.ln() - w[0].close.ln())
                .collect();
            let realized = if logs.len() < 2 {
                None
            } else {
                let mean = mean(&logs);
                Some(finite(
                    (logs.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (logs.len() - 1) as f64).sqrt(),
                    "realized_volatility_per_bar",
                )?)
            };
            let mut ranges: Vec<_> =
                bars.iter().map(|b| finite(b.high - b.low, "range_percentiles")).collect::<Result<_, _>>()?;
            let atr = computed_last(&indicators::atr(&o, 14), bars.len() >= 14, "atr")?;
            ranges.sort_by(f64::total_cmp);
            let percentile =
                |q: f64| ranges.get(((ranges.len().saturating_sub(1)) as f64 * q).round() as usize).copied();
            json!({"atr_period":14,"atr":atr,"realized_volatility_per_bar":realized,
                "log_return_count":logs.len(),"range_percentiles":{"p10":percentile(0.1),"p50":percentile(0.5),"p90":percentile(0.9)}})
        }
    };
    Ok(value)
}

fn trend(bars: &[Bar], o: &Ohlcv) -> Result<Value, ToolResult> {
    if bars.len() < 2 {
        return Ok(
            json!({"direction":"insufficient_data","slope_per_bar":null,"r_squared":null,"higher_highs":0,"higher_lows":0}),
        );
    }
    let n = bars.len() as f64;
    let mx = (n - 1.) / 2.;
    let max_price = bars.iter().map(|b| b.close.abs()).fold(0., f64::max).max(f64::MIN_POSITIVE);
    // Power-of-two scaling preserves the original significands exactly.
    let scale = 2_f64.powi((max_price.log2().floor() as i32).clamp(-1022, 1023));
    let prices: Vec<_> = bars.iter().map(|b| b.close / scale).collect();
    let my = prices.iter().sum::<f64>() / n;
    let xx = (0..bars.len()).map(|i| (i as f64 - mx).powi(2)).sum::<f64>();
    let xy = prices.iter().enumerate().map(|(i, p)| (i as f64 - mx) * (p - my)).sum::<f64>();
    let yy = prices.iter().map(|p| (p - my).powi(2)).sum::<f64>();
    let slope = finite((xy / xx) * scale, "slope_per_bar")?;
    let r_squared = finite(if yy > 0. { (xy * xy / (xx * yy)).clamp(0., 1.) } else { 1. }, "r_squared")?;
    let (highs, lows) = indicators::fractals(o, 2);
    let higher = |v: Vec<f64>| {
        v.into_iter().filter(|v| v.is_finite()).collect::<Vec<_>>().windows(2).filter(|w| w[1] > w[0]).count()
    };
    Ok(
        json!({"direction":if slope.abs() <= (my.abs()*scale).max(1.)*1e-10 {"sideways"} else if slope>0. {"up"} else {"down"},
        "slope_per_bar":slope,"r_squared":r_squared,
        "higher_highs":higher(highs),"higher_lows":higher(lows)}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_output_exposes_zone_bounds_and_keeps_existing_fields() {
        let bars = tr_core::sample::generate("ZONES", Timeframe::D1, 400, 1_700_000_000);
        let value = analyze(&bars, Timeframe::D1, Session::Provider, Mode::Levels, 10).unwrap();
        let levels = value["support_resistance"].as_array().unwrap();
        assert!(!levels.is_empty());
        for level in levels {
            let price = level["price"].as_f64().unwrap();
            let width = level["zone_width"].as_f64().unwrap();
            assert!(width > 0.);
            let low = level["low"].as_f64().unwrap();
            let high = level["high"].as_f64().unwrap();
            assert!(low < price && price < high);
            assert_eq!(width, high - low);
            for field in ["touches", "last_touch_time", "score"] {
                assert!(level[field].is_number(), "{field}");
            }
        }
        assert!(support_resistance(&[], 10).is_empty());
    }

    #[test]
    fn lopsided_zone_contains_every_touch_and_tolerance_padding() {
        let bars: Vec<_> = (0..25)
            .map(|i| {
                let high = match i {
                    2 | 7 | 12 => 10.,
                    17 => 12.,
                    _ => 9.,
                };
                Bar::new(i * 86400, 8., high, 6., 8., 1.)
            })
            .collect();
        let levels = support_resistance(&bars, 10);
        let level = levels.iter().find(|level| level.price == 10.5).expect("10,10,10,12 cluster");
        assert_eq!(level.touches, 4);
        for touch in [10., 10., 10., 12.] {
            assert!((level.low..=level.high).contains(&touch));
        }
        let tolerance = last_finite(&indicators::atr(&Ohlcv::new(&bars), 14)).unwrap() * 0.35;
        assert!(level.low <= level.price - tolerance);
        assert!(level.high >= level.price + tolerance);
        assert_eq!(level.zone_width, level.high - level.low);
        assert!(level.score.is_finite());
    }

    #[test]
    fn zones_include_in_play_and_nearest_sides_in_distance_order_with_height_cap() {
        let mut bars = tr_core::sample::generate("ZONE-ORDER", Timeframe::W1, 500, 1_700_000_000);
        let original = support_resistance(&bars, 100);
        assert!(!original.is_empty());
        bars.last_mut().unwrap().close = original[0].price;
        let close = bars.last().unwrap().close;
        let zones = support_resistance(&bars, 100);
        assert!(zones.iter().any(|z| z.in_play && z.low <= close && close <= z.high));
        let distance = |z: &SupportResistance| (z.low - close).max(close - z.high).max(0.);
        assert!(zones.windows(2).all(|w| distance(&w[0]) <= distance(&w[1])));
        assert!(zones.iter().filter(|z| z.low <= close).count() <= 3);
        assert!(zones.iter().filter(|z| z.high >= close).count() <= 3);
        let atr = last_finite(&indicators::atr(&Ohlcv::new(&bars), 14)).unwrap();
        let tolerance = (atr * 0.35).max(close.abs() * 1e-8).max(1e-8);
        for z in &zones {
            assert!(z.high - z.low <= (1.5 * atr).max(2. * tolerance) + 1e-9);
        }
        let limited = support_resistance(&bars, 1);
        for side in ["support", "resistance"] {
            if let Some(nearest) = zones.iter().find(|z| z.side == side) {
                assert!(
                    limited.iter().any(|z| z.price == nearest.price),
                    "nearest {side} cannot be displaced by touch score or top"
                );
            }
        }
        assert!(limited.iter().any(|z| z.in_play));
    }

    #[test]
    fn capped_zones_trim_padding_first_then_prefer_dense_touches_over_outliers() {
        let (low, high, trimmed) = capped_zone_bounds(&[10., 10., 14.], 11.333333, 2., 4.);
        assert!(!trimmed);
        assert_eq!(high - low, 4.);
        assert!(low <= 10. && high >= 14.);
        let (low, high, trimmed) = capped_zone_bounds(&[0., 10., 10.1, 10.2, 100.], 26.06, 1., 3.);
        assert!(trimmed);
        assert_eq!(high - low, 3.);
        assert!(low <= 10. && high >= 10.2);
        assert!(low > 0. && high < 100.);
    }

    #[test]
    fn review_trimmed_only_when_touches_are_excluded() {
        let (low, high, trimmed) = capped_zone_bounds(&[10., 10., 14.], 11.333333, 2., 4.);
        assert!(low <= 10. && high >= 14., "all touches inside");
        assert!(!trimmed, "trimmed must report excluded touches, not removed padding");
    }

    #[test]
    fn lttb_keeps_endpoints_and_point_count() {
        let bars = tr_core::sample::generate("LTTB", Timeframe::M1, 1000, 1_700_000_000);
        for count in [2, 3, 17, 299, 300, 999, 1000, 1001] {
            let points = lttb(&bars, count);
            assert_eq!(points.len(), count.min(bars.len()));
            assert_eq!(points[0], (bars[0].ts, bars[0].close));
            assert_eq!(*points.last().unwrap(), (bars[999].ts, bars[999].close));
            assert!(points.windows(2).all(|w| w[0].0 < w[1].0));
        }
        assert!(lttb(&[], 300).is_empty());
        assert_eq!(lttb(&bars[..1], 300).len(), 1);
    }
}
