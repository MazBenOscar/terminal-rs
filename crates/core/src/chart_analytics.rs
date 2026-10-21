//! Causal bar-derived chart analytics; profiles are OHLCV estimates, not trade prints.
use crate::Bar;

/// ATR(14), rounded down to a 1/2/2.5/5 box; short histories use 1% of close.
pub fn auto_box_size(bars: &[Bar]) -> f64 {
    let fallback = bars.last().map_or(1., |b| b.close.abs() * 0.01);
    let atr = if bars.len() >= 15 {
        bars[bars.len() - 15..]
            .windows(2)
            .map(|pair| {
                let (prev, bar) = (&pair[0], &pair[1]);
                (bar.high - bar.low).max((bar.high - prev.close).abs()).max((bar.low - prev.close).abs())
            })
            .sum::<f64>()
            / 14.
    } else {
        fallback
    };
    let value = if atr.is_finite() && atr > 0. {
        atr
    } else if fallback.is_finite() && fallback > 0. {
        fallback
    } else {
        1.
    };
    let power = 10f64.powf(value.log10().floor());
    let unit = value / power;
    let nice = [5., 2.5, 2., 1.].into_iter().find(|n| *n <= unit).unwrap_or(1.);
    (nice * power).max(f64::MIN_POSITIVE)
}

/// Fixed-box, close-only Renko with a one-box reversal. Timestamps are source times;
/// multiple bricks may share one time. Volume is apportioned, not tick-volume.
pub fn renko(bars: &[Bar], size: f64) -> Result<Vec<Bar>, String> {
    if !size.is_finite() || size <= 0. {
        return Err("Renko brick size must be positive".into());
    }
    let Some(first) = bars.first() else {
        return Ok(vec![]);
    };
    if !first.close.is_finite() {
        return Err("Invalid initial Renko price".into());
    }
    let mut level = first.close;
    let mut volume = 0.;
    let mut out = Vec::new();
    for bar in bars {
        if !bar.close.is_finite() {
            return Err("Invalid Renko source price".into());
        }
        volume += bar.volume;
        let count = ((bar.close - level).abs() / size).floor();
        if count > (100_000usize.saturating_sub(out.len())) as f64 {
            return Err("Renko exceeds 100,000 bricks; increase brick size".into());
        }
        let n = count as usize;
        let step = if bar.close >= level { size } else { -size };
        for _ in 0..n {
            let next = level + step;
            out.push(Bar::new(bar.ts, level, level.max(next), level.min(next), next, volume / n as f64));
            level = next;
        }
        if n > 0 {
            volume = 0.;
        }
    }
    Ok(out)
}
pub fn anchored_vwap(bars: &[Bar], anchor: i64) -> Vec<f64> {
    let mut out = vec![f64::NAN; bars.len()];
    let start = bars.partition_point(|b| b.ts <= anchor).saturating_sub(1);
    let (mut pv, mut volume) = (0., 0.);
    for (i, b) in bars.iter().enumerate().skip(start) {
        if !b.volume.is_finite() || b.volume < 0. || ![b.high, b.low, b.close].iter().all(|p| p.is_finite()) {
            break;
        }
        volume += b.volume;
        pv += (b.high + b.low + b.close) / 3. * b.volume;
        if volume > 0. {
            out[i] = pv / volume;
        }
    }
    out
}
#[derive(Debug, Clone)]
pub struct Profile {
    pub low: f64,
    pub step: f64,
    pub volume: Vec<f64>,
    pub poc: usize,
    pub value_low: usize,
    pub value_high: usize,
}
pub fn volume_profile(bars: &[Bar], bins: usize) -> Option<Profile> {
    let bins = bins.clamp(8, 128);
    let mut low = f64::INFINITY;
    let mut high = f64::NEG_INFINITY;
    for b in bars {
        if b.low.is_finite() && b.high.is_finite() && b.high >= b.low {
            low = low.min(b.low);
            high = high.max(b.high);
        }
    }
    if !low.is_finite() || !high.is_finite() {
        return None;
    }
    let step = ((high - low) / bins as f64).max(low.abs() * 1e-8).max(1e-8);
    let mut changes = vec![0.; bins + 1];
    for b in bars {
        if !b.volume.is_finite() || b.volume < 0. || !b.low.is_finite() || !b.high.is_finite() || b.high < b.low {
            continue;
        }
        let lo = (((b.low - low) / step).floor().max(0.) as usize).min(bins - 1);
        let hi = (((b.high - low) / step).floor().max(0.) as usize).min(bins - 1);
        let amount = b.volume / (hi - lo + 1) as f64;
        changes[lo] += amount;
        changes[hi + 1] -= amount;
    }
    let mut running = 0.;
    let volume: Vec<_> = changes[..bins]
        .iter()
        .map(|v| {
            running += v;
            running.max(0.)
        })
        .collect();
    let total: f64 = volume.iter().sum();
    if total <= 0. {
        return None;
    }
    let poc = volume.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?.0;
    let (mut a, mut b, mut sum) = (poc, poc, volume[poc]);
    while sum < total * 0.7 && (a > 0 || b + 1 < bins) {
        if b + 1 < bins && (a == 0 || volume[b + 1] >= volume[a - 1]) {
            b += 1;
            sum += volume[b];
        } else {
            a -= 1;
            sum += volume[a];
        }
    }
    Some(Profile { low, step, volume, poc, value_low: a, value_high: b })
}
pub fn heikin_ashi(bars: &[Bar]) -> Vec<Bar> {
    let mut previous: Option<(f64, f64)> = None;
    bars.iter()
        .map(|b| {
            let close = (b.open + b.high + b.low + b.close) / 4.;
            let open = previous.map_or((b.open + b.close) / 2., |(o, c)| (o + c) / 2.);
            let result = Bar::new(b.ts, open, b.high.max(open).max(close), b.low.min(open).min(close), close, b.volume);
            previous = (open.is_finite() && close.is_finite()).then_some((open, close));
            result
        })
        .collect()
}
#[cfg(test)]
mod tests {
    #[test]
    fn automatic_box_sizes() {
        let btc: Vec<_> = (0..15).map(|ts| crate::Bar::new(ts, 60_000., 61_400., 58_600., 60_000., 1.)).collect();
        assert_eq!(super::auto_box_size(&btc), 2500.);
        let small: Vec<_> = (0..15).map(|ts| crate::Bar::new(ts, 5., 5.15, 4.85, 5., 1.)).collect();
        assert_eq!(super::auto_box_size(&small), 0.25);
        assert_eq!(super::auto_box_size(&btc[..1]), 500.);
        assert_eq!(super::auto_box_size(&small[..1]), 0.05);
        assert_eq!(super::auto_box_size(&[]), 1.);
        assert!(super::auto_box_size(&[crate::Bar::new(0, 0., 0., 0., 0., 0.)]) > 0.);
    }
    use super::*;
    #[test]
    fn renko_builds_multiple_bricks_without_future_values_and_bounds_work() {
        let b = [
            Bar::new(1, 100., 100., 100., 100., 10.),
            Bar::new(2, 100., 126., 99., 125., 20.),
            Bar::new(3, 125., 125., 99., 100., 30.),
        ];
        let bricks = renko(&b[..2], 10.).unwrap();
        assert_eq!(bricks.len(), 2);
        assert_eq!(bricks[0].close, 110.);
        assert_eq!(bricks[1].close, 120.);
        assert!(bricks.iter().all(|b| b.ts == 2));
        assert_eq!(bricks.iter().map(|b| b.volume).sum::<f64>(), 30.);
        let full = renko(&b, 10.).unwrap();
        assert_eq!(full.len(), 4);
        assert_eq!(full[3].close, 100.);
        assert_eq!(full[..2], bricks);
        assert!(renko(&b, 0.).is_err());
        assert!(renko(&b, 0.00000001).is_err());
    }
    #[test]
    fn anchored_vwap_snaps_to_containing_bar_and_never_substitutes_missing_volume() {
        let b = [
            Bar::new(1, 10., 10., 10., 10., 2.),
            Bar::new(2, 20., 20., 20., 20., 1.),
            Bar::new(3, 30., 30., 30., 30., 0.),
        ];
        assert_eq!(anchored_vwap(&b, 1), vec![10., 40. / 3., 40. / 3.]);
        assert!(anchored_vwap(&b, 2)[0].is_nan());
        assert_eq!(anchored_vwap(&b, 2)[1], 20.);
        assert_eq!(anchored_vwap(&b, 0), anchored_vwap(&b, 1));
        let daily = [Bar::new(86_400, 10., 10., 10., 10., 1.), Bar::new(172_800, 20., 20., 20., 20., 1.)];
        for anchor in [172_800, 180_000] {
            let values = anchored_vwap(&daily, anchor);
            assert!(values[0].is_nan());
            assert_eq!(values[1], 20.);
        }
        assert_eq!(anchored_vwap(&daily, 0), vec![10., 15.]);
        let mut broken = b;
        broken[1].volume = f64::NAN;
        assert!(anchored_vwap(&broken, 1)[2].is_nan());
    }
    #[test]
    fn profiles_conserve_available_volume_and_identify_value_area() {
        let bars = [Bar::new(0, 10., 20., 10., 15., 100.), Bar::new(1, 15., 15., 15., 15., 50.)];
        let p = volume_profile(&bars, 16).unwrap();
        assert!((p.volume.iter().sum::<f64>() - 150.).abs() < 1e-8);
        assert!(p.value_low <= p.poc && p.value_high >= p.poc);
        assert!(volume_profile(&[Bar { volume: f64::NAN, ..bars[0] }], 16).is_none());
    }
    #[test]
    fn heikin_ashi_uses_recursive_open_and_keeps_time_and_volume() {
        let b = [Bar::new(1, 10., 14., 8., 12., 50.), Bar::new(2, 12., 16., 11., 15., 60.)];
        let ha = heikin_ashi(&b);
        assert_eq!(ha[0].open, 11.);
        assert_eq!(ha[0].close, 11.);
        assert_eq!(ha[1].open, 11.);
        assert_eq!(ha[1].close, 13.5);
        assert_eq!(ha[1].ts, 2);
        assert_eq!(ha[1].volume, 60.);
    }
}
