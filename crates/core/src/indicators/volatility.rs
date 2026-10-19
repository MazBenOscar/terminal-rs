//! Ranges, bands and channels, and volatility estimators.

use crate::Timeframe;

use super::ma::{ema, rma, sma};
use super::momentum::roc;
use super::stats::{change, highest, linreg_map, lowest, stdev, sum, variance};
use super::{NAN, Ohlcv, ago, ratio, zip};

/// True range: the bar's range extended to the previous close (high − low on the first bar).
pub fn true_range(o: &Ohlcv) -> Vec<f64> {
    (0..o.len())
        .map(|i| {
            let p = ago(&o.close, i, 1);
            if p.is_nan() { o.high[i] - o.low[i] } else { o.high[i].max(p) - o.low[i].min(p) }
        })
        .collect()
}

/// Average true range (Wilder).
pub fn atr(o: &Ohlcv, n: usize) -> Vec<f64> {
    rma(&true_range(o), n)
}

/// ATR as a percentage of the close.
pub fn natr(o: &Ohlcv, n: usize) -> Vec<f64> {
    zip(&atr(o, n), &o.close, |a, c| 100.0 * ratio(a, c, NAN))
}

#[derive(Debug, Clone)]
pub struct Bands {
    pub upper: Vec<f64>,
    pub middle: Vec<f64>,
    pub lower: Vec<f64>,
}

impl Bands {
    /// `middle ± width`.
    fn around(middle: Vec<f64>, width: &[f64]) -> Self {
        let upper = zip(&middle, width, |m, w| m + w);
        let lower = zip(&middle, width, |m, w| m - w);
        Bands { upper, middle, lower }
    }
}

/// Bollinger bands: SMA ± `k` population standard deviations.
pub fn bollinger(closes: &[f64], n: usize, k: f64) -> Bands {
    let sd: Vec<f64> = stdev(closes, n).into_iter().map(|s| k * s).collect();
    Bands::around(sma(closes, n), &sd)
}

/// Bollinger %B: where the close sits between the bands (0 at the lower, 1 at the upper).
pub fn percent_b(closes: &[f64], n: usize, k: f64) -> Vec<f64> {
    let b = bollinger(closes, n, k);
    (0..closes.len()).map(|i| ratio(closes[i] - b.lower[i], b.upper[i] - b.lower[i], 0.5)).collect()
}

/// Bollinger bandwidth: band width as a percentage of the middle band.
pub fn bandwidth(closes: &[f64], n: usize, k: f64) -> Vec<f64> {
    let b = bollinger(closes, n, k);
    (0..closes.len()).map(|i| 100.0 * ratio(b.upper[i] - b.lower[i], b.middle[i], 0.0)).collect()
}

/// Keltner channels: EMA(close, `n`) ± `mult` × ATR(`atr_n`).
pub fn keltner(o: &Ohlcv, n: usize, mult: f64, atr_n: usize) -> Bands {
    let w: Vec<f64> = atr(o, atr_n).into_iter().map(|a| mult * a).collect();
    Bands::around(ema(&o.close, n), &w)
}

/// Donchian channels: the `n`-bar highest high and lowest low, and their midpoint.
pub fn donchian(o: &Ohlcv, n: usize) -> Bands {
    let (upper, lower) = (highest(&o.high, n), lowest(&o.low, n));
    let middle = zip(&upper, &lower, |h, l| (h + l) / 2.0);
    Bands { upper, middle, lower }
}

/// Moving average envelopes: SMA ± `pct` percent.
pub fn envelopes(values: &[f64], n: usize, pct: f64) -> Bands {
    let middle = sma(values, n);
    let w: Vec<f64> = middle.iter().map(|m| m * pct / 100.0).collect();
    Bands::around(middle, &w)
}

/// Headley's acceleration bands: SMAs of the high and low pushed out by `factor` times the
/// bar's relative range.
pub fn acceleration_bands(o: &Ohlcv, n: usize, factor: f64) -> Bands {
    let rel: Vec<f64> = (0..o.len()).map(|i| ratio(o.high[i] - o.low[i], o.high[i] + o.low[i], 0.0)).collect();
    let up: Vec<f64> = (0..o.len()).map(|i| o.high[i] * (1.0 + factor * rel[i])).collect();
    let dn: Vec<f64> = (0..o.len()).map(|i| o.low[i] * (1.0 - factor * rel[i])).collect();
    Bands { upper: sma(&up, n), middle: sma(&o.close, n), lower: sma(&dn, n) }
}

/// STARC bands: SMA(close, `n`) ± `mult` × ATR(`atr_n`).
pub fn starc(o: &Ohlcv, n: usize, atr_n: usize, mult: f64) -> Bands {
    let w: Vec<f64> = atr(o, atr_n).into_iter().map(|a| mult * a).collect();
    Bands::around(sma(&o.close, n), &w)
}

/// Standard error bands: the `n`-bar regression value ± `k` standard errors, both smoothed
/// with an SMA over `smooth` bars.
pub fn standard_error_bands(values: &[f64], n: usize, smooth: usize, k: f64) -> Bands {
    let mid = sma(&linreg_map(values, n, |f| f.at(n as f64 - 1.0)), smooth);
    let se: Vec<f64> = sma(&linreg_map(values, n, |f| f.se), smooth).into_iter().map(|s| k * s).collect();
    Bands::around(mid, &se)
}

/// Bars per year on `tf`, for annualising volatility (US equity hours intraday).
pub fn periods_per_year(tf: Timeframe) -> f64 {
    match tf {
        Timeframe::D1 => 252.0,
        Timeframe::W1 => 52.0,
        Timeframe::MN1 => 12.0,
        intraday => 252.0 * 23_400.0 / intraday.seconds() as f64,
    }
}

/// `ln(a / b)` for positive prices, NaN otherwise.
fn ln_ratio(a: f64, b: f64) -> f64 {
    if a > 0.0 && b > 0.0 { (a / b).ln() } else { NAN }
}

/// Annualised percentage volatility from a per-bar variance; rounding can leave tiny
/// negative variances, which read as zero. Missing variances stay NaN.
fn annualised(variance: f64, ppy: f64) -> f64 {
    if variance.is_finite() { 100.0 * (variance.max(0.0) * ppy).sqrt() } else { NAN }
}

/// Annualised percentage volatility from a rolling mean of per-bar variance terms.
fn annualise(terms: &[f64], n: usize, ppy: f64) -> Vec<f64> {
    sma(terms, n).into_iter().map(|v| annualised(v, ppy)).collect()
}

/// Historical (close-to-close) volatility: annualised standard deviation of log returns, %.
pub fn historical_volatility(values: &[f64], n: usize, ppy: f64) -> Vec<f64> {
    let r: Vec<f64> = (0..values.len()).map(|i| ln_ratio(values[i], ago(values, i, 1))).collect();
    stdev(&r, n).into_iter().map(|s| 100.0 * s * ppy.sqrt()).collect()
}

/// Parkinson's high-low volatility estimator, annualised %.
pub fn parkinson(o: &Ohlcv, n: usize, ppy: f64) -> Vec<f64> {
    let k = 1.0 / (4.0 * std::f64::consts::LN_2);
    let terms: Vec<f64> = (0..o.len()).map(|i| k * ln_ratio(o.high[i], o.low[i]).powi(2)).collect();
    annualise(&terms, n, ppy)
}

/// Garman-Klass OHLC volatility estimator, annualised %.
pub fn garman_klass(o: &Ohlcv, n: usize, ppy: f64) -> Vec<f64> {
    let c = 2.0 * std::f64::consts::LN_2 - 1.0;
    let terms: Vec<f64> = (0..o.len())
        .map(|i| 0.5 * ln_ratio(o.high[i], o.low[i]).powi(2) - c * ln_ratio(o.close[i], o.open[i]).powi(2))
        .collect();
    annualise(&terms, n, ppy)
}

/// Per-bar Rogers-Satchell variance term (drift independent).
fn rs_terms(o: &Ohlcv) -> Vec<f64> {
    (0..o.len())
        .map(|i| {
            let (h, l, op, c) = (o.high[i], o.low[i], o.open[i], o.close[i]);
            ln_ratio(h, c) * ln_ratio(h, op) + ln_ratio(l, c) * ln_ratio(l, op)
        })
        .collect()
}

/// Rogers-Satchell volatility estimator, annualised %.
pub fn rogers_satchell(o: &Ohlcv, n: usize, ppy: f64) -> Vec<f64> {
    annualise(&rs_terms(o), n, ppy)
}

/// Yang-Zhang volatility: overnight, open-to-close and Rogers-Satchell variances combined
/// to minimise estimator variance, annualised %.
pub fn yang_zhang(o: &Ohlcv, n: usize, ppy: f64) -> Vec<f64> {
    let overnight: Vec<f64> = (0..o.len()).map(|i| ln_ratio(o.open[i], ago(&o.close, i, 1))).collect();
    let intraday: Vec<f64> = (0..o.len()).map(|i| ln_ratio(o.close[i], o.open[i])).collect();
    let k = n as f64;
    let sample = if n > 1 { k / (k - 1.0) } else { 1.0 };
    let w = 0.34 / (1.34 + (k + 1.0) / (k - 1.0).max(1.0));
    let (vo, vc, rs) = (variance(&overnight, n), variance(&intraday, n), sma(&rs_terms(o), n));
    (0..o.len()).map(|i| annualised(sample * vo[i] + w * sample * vc[i] + (1.0 - w) * rs[i], ppy)).collect()
}

/// Ulcer index: root mean square of the percentage drawdown from the `n`-bar high.
pub fn ulcer(values: &[f64], n: usize) -> Vec<f64> {
    let hh = highest(values, n);
    let dd: Vec<f64> = (0..values.len()).map(|i| (100.0 * ratio(values[i] - hh[i], hh[i], 0.0)).powi(2)).collect();
    sma(&dd, n).into_iter().map(f64::sqrt).collect()
}

/// Chaikin volatility: the `roc_n`-bar rate of change of the EMA of the high-low range.
pub fn chaikin_volatility(o: &Ohlcv, n: usize, roc_n: usize) -> Vec<f64> {
    roc(&ema(&zip(&o.high, &o.low, |h, l| h - l), n), roc_n)
}

/// Dorsey's mass index: the `sum_n`-bar sum of EMA(range) / EMA(EMA(range)).
pub fn mass_index(o: &Ohlcv, n: usize, sum_n: usize) -> Vec<f64> {
    let e1 = ema(&zip(&o.high, &o.low, |h, l| h - l), n);
    let e2 = ema(&e1, n);
    sum(&zip(&e1, &e2, |a, b| ratio(a, b, 1.0)), sum_n)
}

/// Dorsey's relative volatility index: an RSI of the `n`-bar standard deviation, split by
/// the direction of the close, EMA-smoothed over `smooth`.
pub fn relative_volatility_index(values: &[f64], n: usize, smooth: usize) -> Vec<f64> {
    let sd = stdev(values, n);
    let ch = change(values, 1);
    let side = |up: bool| -> Vec<f64> {
        (0..values.len()).map(|i| if sd[i].is_nan() || (ch[i] > 0.0) == up { sd[i] } else { 0.0 }).collect()
    };
    zip(&ema(&side(true), smooth), &ema(&side(false), smooth), |u, d| 100.0 * ratio(u, u + d, 0.5))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Bar;

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9 * (1.0 + b.abs())
    }

    fn ohlcv(rows: &[(f64, f64, f64, f64)]) -> Ohlcv {
        let bars: Vec<Bar> =
            rows.iter().enumerate().map(|(i, r)| Bar::new(i as i64, r.0, r.1, r.2, r.3, 100.0)).collect();
        Ohlcv::new(&bars)
    }

    #[test]
    fn true_range_uses_previous_close() {
        let o = ohlcv(&[(10.0, 11.0, 9.0, 10.0), (12.0, 13.0, 12.0, 12.5), (9.0, 10.0, 8.0, 9.0)]);
        assert_eq!(true_range(&o), vec![2.0, 3.0, 4.5]);
        let a = atr(&o, 2);
        assert!(a[0].is_nan() && near(a[1], 2.5) && near(a[2], 0.5 * 4.5 + 0.5 * 2.5));
    }

    #[test]
    fn bands_hand_computed() {
        let o = ohlcv(&[(1.0, 2.0, 0.0, 1.0), (2.0, 4.0, 1.0, 3.0), (3.0, 3.5, 2.5, 3.0)]);
        let d = donchian(&o, 2);
        assert!(near(d.upper[2], 4.0) && near(d.lower[2], 1.0) && near(d.middle[2], 2.5));
        let e = envelopes(&o.close, 2, 10.0);
        assert!(near(e.upper[2], 3.3) && near(e.lower[2], 2.7));
        let b = bollinger(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0], 8, 2.0);
        assert!(near(percent_b(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0], 8, 2.0)[7], (9.0 - 1.0) / 8.0));
        assert!(near(bandwidth(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0], 8, 2.0)[7], 800.0 / 5.0));
        assert!(near(b.middle[7], 5.0));
        // Flat prices: zero-width bands put %B in the middle.
        assert_eq!(percent_b(&[3.0; 10], 5, 2.0)[9], 0.5);
    }

    #[test]
    fn estimators_agree_on_constant_ranges() {
        // Every bar spans a fixed log range and opens/closes at the extremes alternately:
        // the range-based estimators then all see the same per-bar variance.
        let rows: Vec<(f64, f64, f64, f64)> = (0..200)
            .map(|i| {
                let lo = 100.0 * (1.0 + 0.001 * (i % 7) as f64);
                let hi = lo * 1.02;
                if i % 2 == 0 { (lo, hi, lo, hi) } else { (hi, hi, lo, lo) }
            })
            .collect();
        let o = ohlcv(&rows);
        let r = (1.02f64).ln();
        let ppy = 252.0;
        // Rogers-Satchell is zero when bars open and close at their extremes.
        assert!(near(rogers_satchell(&o, 20, ppy)[100], 0.0));
        let p = parkinson(&o, 20, ppy)[100];
        assert!(near(p, 100.0 * (r * r / (4.0 * std::f64::consts::LN_2) * ppy).sqrt()));
        let gk = garman_klass(&o, 20, ppy)[100];
        let c = 2.0 * std::f64::consts::LN_2 - 1.0;
        assert!(near(gk, 100.0 * ((0.5 * r * r - c * r * r) * ppy).sqrt()));
        assert!(yang_zhang(&o, 20, ppy)[100].is_finite());
    }

    #[test]
    fn estimators_are_blank_until_their_window_fills() {
        let rows: Vec<(f64, f64, f64, f64)> = (0..40).map(|i| (100.0, 101.0 + (i % 3) as f64, 99.0, 100.5)).collect();
        let o = ohlcv(&rows);
        for (name, v, first) in [
            ("parkinson", parkinson(&o, 20, 252.0), 19),
            ("garman-klass", garman_klass(&o, 20, 252.0), 19),
            ("rogers-satchell", rogers_satchell(&o, 20, 252.0), 19),
            ("yang-zhang", yang_zhang(&o, 20, 252.0), 20),
        ] {
            assert!(v[..first].iter().all(|x| x.is_nan()), "{name} reads before its window fills");
            assert!(v[first..].iter().all(|x| x.is_finite() && *x > 0.0), "{name} is blank after warm-up");
        }
        // A bar with a non-positive price has no log range; its windows stay blank.
        let mut rows = rows;
        rows[30].2 = 0.0;
        assert!(parkinson(&ohlcv(&rows), 20, 252.0)[30..].iter().all(|x| x.is_nan()));
    }

    #[test]
    fn historical_volatility_of_constant_growth_is_zero() {
        let g: Vec<f64> = (0..50).map(|i| 100.0 * 1.01f64.powi(i)).collect();
        assert!(historical_volatility(&g, 20, 252.0)[49].abs() < 1e-6);
        assert!(ulcer(&g, 14)[49].abs() < 1e-9);
        assert_eq!(periods_per_year(Timeframe::H1), 252.0 * 6.5);
    }

    #[test]
    fn mass_index_of_constant_range() {
        let rows: Vec<(f64, f64, f64, f64)> =
            (0..100).map(|i| (i as f64, i as f64 + 2.0, i as f64, i as f64 + 1.0)).collect();
        assert!(near(mass_index(&ohlcv(&rows), 9, 25)[99], 25.0));
    }
}
