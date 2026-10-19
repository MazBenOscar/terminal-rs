//! Trend: directional movement, trailing stops, Ichimoku, Bill Williams' tools, zigzag and
//! trend-strength filters.

use super::ma::{rma, sma};
use super::stats::{change, displace, extreme_at, highest, linreg, lowest, sum};
use super::volatility::{atr, bollinger, true_range};
use super::{NAN, Ohlcv, ago, ratio, zip};

/// Wilder's directional movement index.
#[derive(Debug, Clone)]
pub struct Dmi {
    pub plus: Vec<f64>,
    pub minus: Vec<f64>,
    pub adx: Vec<f64>,
}

/// +DI and −DI over `n` bars and ADX smoothed over `adx_n`, all Wilder-smoothed.
pub fn dmi(o: &Ohlcv, n: usize, adx_n: usize) -> Dmi {
    let len = o.len();
    let (up, down) = (change(&o.high, 1), change(&o.low, 1));
    let plus_dm: Vec<f64> = (0..len)
        .map(|i| {
            if up[i].is_nan() {
                NAN
            } else if up[i] > -down[i] && up[i] > 0.0 {
                up[i]
            } else {
                0.0
            }
        })
        .collect();
    let minus_dm: Vec<f64> = (0..len)
        .map(|i| {
            if down[i].is_nan() {
                NAN
            } else if -down[i] > up[i] && -down[i] > 0.0 {
                -down[i]
            } else {
                0.0
            }
        })
        .collect();
    let mut tr = true_range(o);
    if let Some(t) = tr.first_mut() {
        *t = NAN;
    }
    let tr = rma(&tr, n);
    let plus = zip(&rma(&plus_dm, n), &tr, |d, t| 100.0 * ratio(d, t, 0.0));
    let minus = zip(&rma(&minus_dm, n), &tr, |d, t| 100.0 * ratio(d, t, 0.0));
    let dx: Vec<f64> = (0..len).map(|i| ratio((plus[i] - minus[i]).abs(), plus[i] + minus[i], 0.0)).collect();
    let adx = rma(&dx, adx_n).into_iter().map(|v| 100.0 * v).collect();
    Dmi { plus, minus, adx }
}

/// Aroon up and down: how recently the `n`-bar high and low were set, 0..100.
pub fn aroon(o: &Ohlcv, n: usize) -> (Vec<f64>, Vec<f64>) {
    let k = n as f64;
    let since = |idx: Vec<usize>| -> Vec<f64> {
        idx.into_iter()
            .enumerate()
            .map(|(i, j)| if j == usize::MAX { NAN } else { 100.0 * (k - (i - j) as f64) / k })
            .collect()
    };
    (since(extreme_at(&o.high, n + 1, true)), since(extreme_at(&o.low, n + 1, false)))
}

/// Vortex indicator: +VI and −VI over `n` bars.
pub fn vortex(o: &Ohlcv, n: usize) -> (Vec<f64>, Vec<f64>) {
    let vp: Vec<f64> = (0..o.len()).map(|i| (o.high[i] - ago(&o.low, i, 1)).abs()).collect();
    let vm: Vec<f64> = (0..o.len()).map(|i| (o.low[i] - ago(&o.high, i, 1)).abs()).collect();
    let tr = sum(&true_range(o), n);
    (zip(&sum(&vp, n), &tr, |v, t| ratio(v, t, 0.0)), zip(&sum(&vm, n), &tr, |v, t| ratio(v, t, 0.0)))
}

/// A stop-and-reverse level and, per bar, whether it trails an uptrend (below price).
#[derive(Debug, Clone)]
pub struct Trail {
    pub value: Vec<f64>,
    pub up: Vec<bool>,
}

impl Trail {
    fn new(len: usize) -> Self {
        Trail { value: vec![NAN; len], up: vec![false; len] }
    }

    /// The level split into its uptrend and downtrend stretches (NaN elsewhere), so each
    /// side draws as its own broken line.
    pub fn split(&self) -> (Vec<f64>, Vec<f64>) {
        let side = |want: bool| -> Vec<f64> {
            self.value.iter().zip(&self.up).map(|(v, u)| if *u == want { *v } else { NAN }).collect()
        };
        (side(true), side(false))
    }
}

/// Wilder's parabolic SAR with acceleration `start`, step `inc` and cap `max`.
pub fn psar(o: &Ohlcv, start: f64, inc: f64, max: f64) -> Trail {
    let len = o.len();
    let mut t = Trail::new(len);
    if len < 2 {
        return t;
    }
    let (h, l, c) = (&o.high, &o.low, &o.close);
    let mut below = c[1] > c[0];
    let (mut ext, mut sar) = if below { (h[1], l[0]) } else { (l[1], h[0]) };
    let mut af = start;
    for i in 1..len {
        let mut fresh = i == 1;
        sar += af * (ext - sar);
        if below && sar > l[i] {
            (fresh, below, sar, ext, af) = (true, false, h[i].max(ext), l[i], start);
        } else if !below && sar < h[i] {
            (fresh, below, sar, ext, af) = (true, true, l[i].min(ext), h[i], start);
        }
        if !fresh && ((below && h[i] > ext) || (!below && l[i] < ext)) {
            ext = if below { h[i] } else { l[i] };
            af = (af + inc).min(max);
        }
        if below {
            sar = sar.min(l[i - 1]);
            if i > 1 {
                sar = sar.min(l[i - 2]);
            }
        } else {
            sar = sar.max(h[i - 1]);
            if i > 1 {
                sar = sar.max(h[i - 2]);
            }
        }
        t.value[i] = sar;
        t.up[i] = below;
    }
    t
}

/// SuperTrend: median price ± `factor` × ATR(`n`), ratcheting with the trend.
pub fn supertrend(o: &Ohlcv, n: usize, factor: f64) -> Trail {
    let (a, mid) = (atr(o, n), o.hl2());
    let mut t = Trail::new(o.len());
    let (mut lb_prev, mut ub_prev, mut was_up) = (NAN, NAN, None);
    for i in 0..o.len() {
        if a[i].is_nan() {
            continue;
        }
        let (mut lb, mut ub) = (mid[i] - factor * a[i], mid[i] + factor * a[i]);
        let pc = ago(&o.close, i, 1);
        if !lb_prev.is_nan() && !(lb > lb_prev || pc < lb_prev) {
            lb = lb_prev;
        }
        if !ub_prev.is_nan() && !(ub < ub_prev || pc > ub_prev) {
            ub = ub_prev;
        }
        let up = match was_up {
            None => false,
            Some(false) => o.close[i] > ub,
            Some(true) => o.close[i] >= lb,
        };
        t.value[i] = if up { lb } else { ub };
        t.up[i] = up;
        (lb_prev, ub_prev, was_up) = (lb, ub, Some(up));
    }
    t
}

/// Volatility stop: a close-based trailing stop `factor` × ATR(`n`) from the extreme close
/// of the current swing.
pub fn volatility_stop(o: &Ohlcv, n: usize, factor: f64) -> Trail {
    let (a, tr) = (atr(o, n), true_range(o));
    let c = &o.close;
    let mut t = Trail::new(c.len());
    let Some(&first) = c.first() else { return t };
    let (mut hi, mut lo, mut up, mut stop) = (first, first, true, 0.0f64);
    for i in 0..c.len() {
        let src = c[i];
        let m = if a[i].is_nan() { tr[i] } else { a[i] * factor };
        hi = hi.max(src);
        lo = lo.min(src);
        stop = if up { stop.max(hi - m) } else { stop.min(lo + m) };
        if stop.is_nan() {
            stop = src;
        }
        let now_up = src - stop >= 0.0;
        if now_up != up {
            (hi, lo) = (src, src);
            stop = if now_up { hi - m } else { lo + m };
        }
        up = now_up;
        t.value[i] = stop;
        t.up[i] = up;
    }
    t
}

/// Gann HiLo activator: the SMA of lows under an uptrend, of highs over a downtrend; the
/// trend flips when the close crosses the other side's previous value.
pub fn gann_hilo(o: &Ohlcv, high_n: usize, low_n: usize) -> Trail {
    let (sh, sl) = (sma(&o.high, high_n), sma(&o.low, low_n));
    let mut t = Trail::new(o.len());
    let mut up = None;
    for i in 0..o.len() {
        if o.close[i] > ago(&sh, i, 1) {
            up = Some(true);
        } else if o.close[i] < ago(&sl, i, 1) {
            up = Some(false);
        }
        if let Some(u) = up {
            t.value[i] = if u { sl[i] } else { sh[i] };
            t.up[i] = u;
        }
    }
    t
}

#[derive(Debug, Clone)]
pub struct Ichimoku {
    pub conversion: Vec<f64>,
    pub base: Vec<f64>,
    /// Leading spans, displaced into the future (longer than the input).
    pub span_a: Vec<f64>,
    pub span_b: Vec<f64>,
    /// The close displaced into the past.
    pub lagging: Vec<f64>,
}

/// Ichimoku Kinko Hyo: Tenkan-sen, Kijun-sen, Senkou spans A and B and Chikou span.
pub fn ichimoku(o: &Ohlcv, conversion: usize, base: usize, span_b: usize, displacement: usize) -> Ichimoku {
    let mid = |n| zip(&highest(&o.high, n), &lowest(&o.low, n), |h, l| (h + l) / 2.0);
    let (conv, base) = (mid(conversion), mid(base));
    let shift = displacement.saturating_sub(1) as isize;
    let span_a = displace(&zip(&conv, &base, |a, b| (a + b) / 2.0), shift);
    let span_b = displace(&mid(span_b), shift);
    let lagging = displace(&o.close, -shift);
    Ichimoku { conversion: conv, base, span_a, span_b, lagging }
}

#[derive(Debug, Clone)]
pub struct Alligator {
    pub jaw: Vec<f64>,
    pub teeth: Vec<f64>,
    pub lips: Vec<f64>,
}

/// Bill Williams' alligator: smoothed (Wilder) median price over `lengths`, each displaced
/// forward by its `offsets`.
pub fn alligator(o: &Ohlcv, lengths: [usize; 3], offsets: [usize; 3]) -> Alligator {
    let hl2 = o.hl2();
    let line = |k: usize| displace(&rma(&hl2, lengths[k]), offsets[k] as isize);
    Alligator { jaw: line(0), teeth: line(1), lips: line(2) }
}

/// Gator oscillator: |jaw − teeth| above zero and −|teeth − lips| below, at each bar.
pub fn gator(o: &Ohlcv, lengths: [usize; 3], offsets: [usize; 3]) -> (Vec<f64>, Vec<f64>) {
    let a = alligator(o, lengths, offsets);
    let len = o.len();
    let upper = zip(&a.jaw[..len], &a.teeth[..len], |j, t| (j - t).abs());
    let lower = zip(&a.teeth[..len], &a.lips[..len], |t, l| -(t - l).abs());
    (upper, lower)
}

/// Bill Williams' fractals: bars whose high (low) is strictly beyond the `n` bars on each
/// side, marked at that high (low). A fractal is only known `n` bars later.
pub fn fractals(o: &Ohlcv, n: usize) -> (Vec<f64>, Vec<f64>) {
    let len = o.len();
    let (mut up, mut down) = (vec![NAN; len], vec![NAN; len]);
    for i in n..len.saturating_sub(n) {
        let mut around = (i - n..i).chain(i + 1..=i + n);
        if around.clone().all(|j| o.high[j] < o.high[i]) {
            up[i] = o.high[i];
        }
        if around.all(|j| o.low[j] > o.low[i]) {
            down[i] = o.low[i];
        }
    }
    (up, down)
}

/// Zigzag through swing highs and lows that reverse by at least `deviation` percent,
/// interpolated linearly between pivots. The last leg runs to the latest extreme and
/// repaints until the reversal is confirmed.
pub fn zigzag(o: &Ohlcv, deviation: f64) -> Vec<f64> {
    let len = o.len();
    let mut out = vec![NAN; len];
    if len == 0 {
        return out;
    }
    let (h, l) = (&o.high, &o.low);
    let (up_by, down_by) = (1.0 + deviation / 100.0, 1.0 - deviation / 100.0);
    let mut pivots: Vec<(usize, f64)> = Vec::new();
    let (mut dir, mut hi, mut lo) = (0i8, 0usize, 0usize);
    for i in 1..len {
        match dir {
            1 if h[i] >= h[hi] => hi = i,
            1 if l[i] <= h[hi] * down_by => {
                pivots.push((hi, h[hi]));
                (dir, lo) = (-1, i);
            }
            -1 if l[i] <= l[lo] => lo = i,
            -1 if h[i] >= l[lo] * up_by => {
                pivots.push((lo, l[lo]));
                (dir, hi) = (1, i);
            }
            0 => {
                if h[i] > h[hi] {
                    hi = i;
                }
                if l[i] < l[lo] {
                    lo = i;
                }
                if hi > lo && h[hi] >= l[lo] * up_by {
                    pivots.push((lo, l[lo]));
                    dir = 1;
                } else if lo > hi && l[lo] <= h[hi] * down_by {
                    pivots.push((hi, h[hi]));
                    dir = -1;
                }
            }
            _ => {}
        }
    }
    match dir {
        1 => pivots.push((hi, h[hi])),
        -1 => pivots.push((lo, l[lo])),
        _ => {}
    }
    for w in pivots.windows(2) {
        let ((i0, v0), (i1, v1)) = (w[0], w[1]);
        for (k, slot) in out.iter_mut().enumerate().take(i1 + 1).skip(i0) {
            *slot = v0 + (v1 - v0) * (k - i0) as f64 / (i1 - i0).max(1) as f64;
        }
    }
    out
}

/// Choppiness index: 100 × log10(ΣTR / (highest high − lowest low)) / log10(n). High values
/// mean a sideways market, low values a trend.
pub fn choppiness(o: &Ohlcv, n: usize) -> Vec<f64> {
    let tr = sum(&true_range(o), n);
    let (hh, ll) = (highest(&o.high, n), lowest(&o.low, n));
    let k = (n as f64).log10();
    (0..o.len()).map(|i| 100.0 * ratio(tr[i], hh[i] - ll[i], n as f64).log10() / k).collect()
}

/// Vertical horizontal filter: the `n`-bar close range over the sum of bar-to-bar moves.
pub fn vhf(values: &[f64], n: usize) -> Vec<f64> {
    let (hh, ll) = (highest(values, n), lowest(values, n));
    let path = sum(&change(values, 1).into_iter().map(f64::abs).collect::<Vec<_>>(), n);
    (0..values.len()).map(|i| ratio(hh[i] - ll[i], path[i], 0.0)).collect()
}

/// Random walk index: the `n`-bar move against the range a random walk would cover.
pub fn rwi(o: &Ohlcv, n: usize) -> (Vec<f64>, Vec<f64>) {
    let a = atr(o, n);
    let scale = (n as f64).sqrt();
    let high = (0..o.len()).map(|i| ratio(o.high[i] - ago(&o.low, i, n), a[i] * scale, 0.0)).collect();
    let low = (0..o.len()).map(|i| ratio(ago(&o.high, i, n) - o.low[i], a[i] * scale, 0.0)).collect();
    (high, low)
}

/// Trend intensity index: the share of positive deviations from SMA(`ma_n`) over the last
/// `n` bars, 0..100.
pub fn tii(values: &[f64], ma_n: usize, n: usize) -> Vec<f64> {
    let dev = zip(values, &sma(values, ma_n), |c, m| c - m);
    let pos: Vec<f64> = dev.iter().map(|d| if d.is_nan() { NAN } else { d.max(0.0) }).collect();
    let neg: Vec<f64> = dev.iter().map(|d| if d.is_nan() { NAN } else { (-d).max(0.0) }).collect();
    zip(&sum(&pos, n), &sum(&neg, n), |p, q| 100.0 * ratio(p, p + q, 0.5))
}

/// Chande Kroll stop: ATR-offset extremes over `p` bars, then their extremes over `q`.
/// Returns (stop for shorts, stop for longs).
pub fn chande_kroll(o: &Ohlcv, p: usize, x: f64, q: usize) -> (Vec<f64>, Vec<f64>) {
    let a = atr(o, p);
    let first_high = zip(&highest(&o.high, p), &a, |h, a| h - x * a);
    let first_low = zip(&lowest(&o.low, p), &a, |l, a| l + x * a);
    (highest(&first_high, q), lowest(&first_low, q))
}

/// Chandelier exits: `mult` × ATR(`n`) below the `n`-bar high (longs) and above the low
/// (shorts).
pub fn chandelier(o: &Ohlcv, n: usize, mult: f64) -> (Vec<f64>, Vec<f64>) {
    let a = atr(o, n);
    (zip(&highest(&o.high, n), &a, |h, a| h - mult * a), zip(&lowest(&o.low, n), &a, |l, a| l + mult * a))
}

#[derive(Debug, Clone)]
pub struct Squeeze {
    /// Linear-regression momentum of the close against its channel midpoint.
    pub momentum: Vec<f64>,
    /// Bollinger bands inside the Keltner channels.
    pub on: Vec<bool>,
    /// Bollinger bands outside the Keltner channels.
    pub off: Vec<bool>,
}

/// TTM-style squeeze momentum: Bollinger(`bb_n`, `bb_mult`) against Keltner(`kc_n`,
/// `kc_mult`, SMA of true range), with linear-regression momentum.
pub fn squeeze(o: &Ohlcv, bb_n: usize, bb_mult: f64, kc_n: usize, kc_mult: f64) -> Squeeze {
    let bb = bollinger(&o.close, bb_n, bb_mult);
    let (ma, range) = (sma(&o.close, kc_n), sma(&true_range(o), kc_n));
    let len = o.len();
    let (mut on, mut off) = (vec![false; len], vec![false; len]);
    for i in 0..len {
        let (ku, kl) = (ma[i] + kc_mult * range[i], ma[i] - kc_mult * range[i]);
        on[i] = bb.lower[i] > kl && bb.upper[i] < ku;
        off[i] = bb.lower[i] < kl && bb.upper[i] > ku;
    }
    let (hh, ll) = (highest(&o.high, kc_n), lowest(&o.low, kc_n));
    let rel: Vec<f64> = (0..len).map(|i| o.close[i] - ((hh[i] + ll[i]) / 2.0 + ma[i]) / 2.0).collect();
    Squeeze { momentum: linreg(&rel, kc_n), on, off }
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

    fn line(n: usize, slope: f64) -> Ohlcv {
        let rows: Vec<(f64, f64, f64, f64)> =
            (0..n).map(|i| 100.0 + slope * i as f64).map(|c| (c - slope / 2.0, c + 0.5, c - 0.5, c)).collect();
        ohlcv(&rows)
    }

    #[test]
    fn dmi_in_a_steady_uptrend() {
        let d = dmi(&line(100, 1.0), 14, 14);
        assert!(d.plus[13].is_nan() && d.plus[14].is_finite());
        assert!(d.minus[99] == 0.0 && d.plus[99] > 0.0);
        assert!(near(d.adx[99], 100.0));
    }

    #[test]
    fn aroon_counts_bars_since_extremes() {
        let o = line(30, 1.0);
        let (up, down) = aroon(&o, 10);
        assert!(up[9].is_nan());
        assert_eq!((up[20], down[20]), (100.0, 0.0));
    }

    #[test]
    fn trails_follow_the_trend() {
        let o = line(80, 1.0);
        for t in
            [psar(&o, 0.02, 0.02, 0.2), supertrend(&o, 10, 3.0), volatility_stop(&o, 20, 2.0), gann_hilo(&o, 13, 21)]
        {
            assert!(t.up[79], "should trail an uptrend");
            assert!(t.value[79] < o.low[79]);
            let (u, d) = t.split();
            assert!(u[79].is_finite() && d[79].is_nan());
        }
        let falling = line(80, -1.0);
        let st = supertrend(&falling, 10, 3.0);
        assert!(!st.up[79] && st.value[79] > falling.high[79]);
    }

    #[test]
    fn psar_flips_on_reversal() {
        let mut rows: Vec<(f64, f64, f64, f64)> =
            (0..20).map(|i| 100.0 + i as f64).map(|c| (c, c + 0.5, c - 0.5, c)).collect();
        rows.extend((0..20).map(|i| 119.0 - 2.0 * i as f64).map(|c| (c, c + 0.5, c - 0.5, c)));
        let p = psar(&ohlcv(&rows), 0.02, 0.02, 0.2);
        assert!(p.up[15] && !p.up[39]);
        assert!(p.value[39] > rows[39].1);
    }

    #[test]
    fn ichimoku_displacement() {
        let o = line(100, 1.0);
        let k = ichimoku(&o, 9, 26, 52, 26);
        assert_eq!(k.span_a.len(), 125);
        assert!(near(k.span_a[124], (k.conversion[99] + k.base[99]) / 2.0));
        assert!(near(k.lagging[0], o.close[25]));
        assert!(k.lagging[99].is_nan());
        let a = alligator(&o, [13, 8, 5], [8, 5, 3]);
        assert_eq!(a.jaw.len(), 108);
        let (up, low) = gator(&o, [13, 8, 5], [8, 5, 3]);
        assert!(up[99] > 0.0 && low[99] < 0.0);
    }

    #[test]
    fn fractals_and_zigzag_find_swings() {
        // Up 10%, down 10%, up again.
        let path = [100.0, 104.0, 110.0, 105.0, 99.0, 103.0, 108.0];
        let rows: Vec<(f64, f64, f64, f64)> = path.iter().map(|&c| (c, c, c, c)).collect();
        let o = ohlcv(&rows);
        let (up, down) = fractals(&o, 2);
        assert_eq!(up[2], 110.0);
        assert_eq!(down[4], 99.0);
        assert!(up[3].is_nan());
        let z = zigzag(&o, 5.0);
        assert_eq!((z[0], z[2], z[4], z[6]), (100.0, 110.0, 99.0, 108.0));
        assert!(near(z[1], 105.0) && near(z[3], 104.5));
    }

    #[test]
    fn trend_filters() {
        let o = line(100, 1.0);
        assert!(near(vhf(&o.close, 28)[99], 27.0 / 28.0));
        assert!(near(tii(&o.close, 60, 30)[99], 100.0));
        let c = choppiness(&o, 14)[99];
        assert!(c > 0.0 && c < 50.0, "trending market is not choppy: {c}");
        let (hi, lo) = rwi(&o, 14);
        assert!(hi[99] > lo[99]);
        let (sp_short, sp_long) = chande_kroll(&o, 10, 1.0, 9);
        assert!(sp_short[99].is_finite() && sp_long[99].is_finite());
        let (long_exit, short_exit) = chandelier(&o, 22, 3.0);
        assert!(near(long_exit[99], o.high[99] - 3.0 * atr(&o, 22)[99]));
        assert!(near(short_exit[99], lowest(&o.low, 22)[99] + 3.0 * atr(&o, 22)[99]));
        let s = squeeze(&o, 20, 2.0, 20, 1.5);
        assert!(s.momentum[99] > 0.0);
    }
}
