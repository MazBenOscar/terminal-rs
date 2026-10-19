//! Rolling statistics: sums, extremes, dispersion, least-squares fits and ranks. Running
//! sums are recomputed exactly once per window length, so drift stays bounded over
//! millions of bars at O(1) amortised cost per bar.

use std::collections::VecDeque;

use super::NAN;

/// Calls `f(i, r, s, w, s2)` for every full window of `n` values ending at `i` that holds
/// only finite values, where, over `y - r` (`r` = the first finite value, for precision),
/// `s` = Σy, `w` = Σk·y with k = 1 for the oldest … n for the newest, and `s2` = Σy².
pub(super) fn windows(values: &[f64], n: usize, mut f: impl FnMut(usize, f64, f64, f64, f64)) {
    if n == 0 {
        return;
    }
    let r = values.iter().copied().find(|v| v.is_finite()).unwrap_or(0.0);
    let y = |i: usize| if values[i].is_finite() { values[i] - r } else { 0.0 };
    let (mut s, mut w, mut s2, mut bad) = (0.0, 0.0, 0.0, 0usize);
    for i in 0..values.len() {
        if !values[i].is_finite() {
            bad += 1;
        }
        let v = y(i);
        if i >= n {
            if !values[i - n].is_finite() {
                bad -= 1;
            }
            let o = y(i - n);
            w -= s;
            s -= o;
            s2 -= o * o;
            w += n as f64 * v;
        } else {
            w += (i + 1) as f64 * v;
        }
        s += v;
        s2 += v * v;
        if i + 1 >= n {
            if (i + 1) % n == 0 {
                (s, w, s2) = (0.0, 0.0, 0.0);
                for (k, j) in (i + 1 - n..=i).enumerate() {
                    let v = y(j);
                    s += v;
                    w += (k + 1) as f64 * v;
                    s2 += v * v;
                }
            }
            if bad == 0 {
                f(i, r, s, w, s2);
            }
        }
    }
}

/// Rolling sum of `n` values. Windows holding a non-finite value are NaN.
pub fn sum(values: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![NAN; values.len()];
    windows(values, n, |i, r, s, _, _| out[i] = s + r * n as f64);
    out
}

/// Population variance over `n` values.
pub fn variance(values: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![NAN; values.len()];
    let k = n as f64;
    windows(values, n, |i, _, s, _, s2| {
        let m = s / k;
        out[i] = (s2 / k - m * m).max(0.0);
    });
    out
}

/// Population standard deviation over `n` values.
pub fn stdev(values: &[f64], n: usize) -> Vec<f64> {
    variance(values, n).into_iter().map(f64::sqrt).collect()
}

/// Distance from the `n`-value mean in standard deviations (0 when flat).
pub fn zscore(values: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![NAN; values.len()];
    let k = n as f64;
    windows(values, n, |i, r, s, _, s2| {
        let m = s / k;
        let sd = (s2 / k - m * m).max(0.0).sqrt();
        out[i] = if sd > 0.0 { (values[i] - r - m) / sd } else { 0.0 };
    });
    out
}

/// Index of the highest (`max`) or lowest value in each window of `n` (the most recent one
/// on ties), or `usize::MAX` while the window is short or holds a non-finite value.
pub(super) fn extreme_at(values: &[f64], n: usize, max: bool) -> Vec<usize> {
    let mut out = vec![usize::MAX; values.len()];
    if n == 0 {
        return out;
    }
    let mut dq: VecDeque<usize> = VecDeque::new();
    let mut bad = 0usize;
    for (i, &v) in values.iter().enumerate() {
        if v.is_finite() {
            while let Some(&b) = dq.back() {
                if (max && values[b] <= v) || (!max && values[b] >= v) {
                    dq.pop_back();
                } else {
                    break;
                }
            }
            dq.push_back(i);
        } else {
            bad += 1;
        }
        if i >= n && !values[i - n].is_finite() {
            bad -= 1;
        }
        while dq.front().is_some_and(|&f| f + n <= i) {
            dq.pop_front();
        }
        if i + 1 >= n
            && bad == 0
            && let Some(&f) = dq.front()
        {
            out[i] = f;
        }
    }
    out
}

fn pick(values: &[f64], idx: Vec<usize>) -> Vec<f64> {
    idx.into_iter().map(|j| if j == usize::MAX { NAN } else { values[j] }).collect()
}

/// Highest value over the last `n` (monotonic deque, O(1) amortised).
pub fn highest(values: &[f64], n: usize) -> Vec<f64> {
    pick(values, extreme_at(values, n, true))
}

/// Lowest value over the last `n`.
pub fn lowest(values: &[f64], n: usize) -> Vec<f64> {
    pick(values, extreme_at(values, n, false))
}

/// `values[i] - values[i - k]`.
pub fn change(values: &[f64], k: usize) -> Vec<f64> {
    (0..values.len()).map(|i| if i >= k { values[i] - values[i - k] } else { NAN }).collect()
}

/// Displace a series by `k` bars: forward (into the future, extending the output past the
/// last bar) when positive, back when negative.
pub fn displace(values: &[f64], k: isize) -> Vec<f64> {
    let len = values.len();
    if k >= 0 {
        let k = k as usize;
        let mut out = vec![NAN; len + k];
        out[k..].copy_from_slice(values);
        out
    } else {
        let k = k.unsigned_abs().min(len);
        let mut out = vec![NAN; len];
        out[..len - k].copy_from_slice(&values[k..]);
        out
    }
}

/// Least-squares line through one window, with x = 0 for the oldest value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub slope: f64,
    pub intercept: f64,
    /// Coefficient of determination (0 for a flat window).
    pub r2: f64,
    /// Standard error of the estimate, sqrt(SSE / (n - 2)).
    pub se: f64,
}

impl Fit {
    pub fn at(&self, x: f64) -> f64 {
        self.intercept + self.slope * x
    }
}

/// Map every rolling `n`-value least-squares fit through `f` (O(1) per bar).
pub fn linreg_map(values: &[f64], n: usize, f: impl Fn(Fit) -> f64) -> Vec<f64> {
    let mut out = vec![NAN; values.len()];
    let k = n as f64;
    let sx = k * (k - 1.0) / 2.0;
    let sxx = (k - 1.0) * k * (2.0 * k - 1.0) / 6.0;
    let den = k * sxx - sx * sx;
    windows(values, n, |i, r, s, w, s2| {
        let sxy = w - s;
        let slope = if den != 0.0 { (k * sxy - sx * s) / den } else { 0.0 };
        let b = (s - slope * sx) / k;
        let sse = (s2 - b * s - slope * sxy).max(0.0);
        let sst = s2 - s * s / k;
        let r2 = if sst > 1e-12 * s2.max(1e-300) { (1.0 - sse / sst).clamp(0.0, 1.0) } else { 0.0 };
        let se = if n > 2 { (sse / (k - 2.0)).sqrt() } else { 0.0 };
        out[i] = f(Fit { slope, intercept: b + r, r2, se });
    });
    out
}

/// Linear regression value at the newest bar (least squares moving average, LSMA).
pub fn linreg(values: &[f64], n: usize) -> Vec<f64> {
    linreg_map(values, n, |f| f.at(n as f64 - 1.0))
}

/// Slope of the rolling regression line, per bar.
pub fn linreg_slope(values: &[f64], n: usize) -> Vec<f64> {
    linreg_map(values, n, |f| f.slope)
}

/// R² of the rolling regression line.
pub fn r_squared(values: &[f64], n: usize) -> Vec<f64> {
    linreg_map(values, n, |f| f.r2)
}

/// Time series forecast: the regression line extended one bar ahead.
pub fn tsf(values: &[f64], n: usize) -> Vec<f64> {
    linreg_map(values, n, |f| f.at(n as f64))
}

/// Percentage of the previous `n` values less than or equal to the current one.
pub fn percent_rank(values: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![NAN; values.len()];
    if n == 0 {
        return out;
    }
    for i in n..values.len() {
        let v = values[i];
        let w = &values[i - n..i];
        if v.is_finite() && w.iter().all(|x| x.is_finite()) {
            out[i] = 100.0 * w.iter().filter(|&&x| x <= v).count() as f64 / n as f64;
        }
    }
    out
}

/// Rolling median of `n` values (sorted window, O(n) per bar).
pub fn median(values: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![NAN; values.len()];
    if n == 0 {
        return out;
    }
    let mut sorted: Vec<f64> = Vec::with_capacity(n);
    let mut bad = 0usize;
    let pos = |s: &[f64], v: f64| s.partition_point(|x| *x < v);
    for i in 0..values.len() {
        let v = values[i];
        if v.is_finite() {
            let p = pos(&sorted, v);
            sorted.insert(p, v);
        } else {
            bad += 1;
        }
        if i >= n {
            let o = values[i - n];
            if o.is_finite() {
                let p = pos(&sorted, o);
                sorted.remove(p);
            } else {
                bad -= 1;
            }
        }
        if i + 1 >= n && bad == 0 {
            out[i] = if n % 2 == 1 { sorted[n / 2] } else { (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0 };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noisy(n: usize) -> Vec<f64> {
        (0..n).map(|i| 100.0 + ((i * 7919) % 101) as f64 * 0.37 - (i as f64 * 0.11).sin() * 9.0).collect()
    }

    fn near(a: f64, b: f64) -> bool {
        (a.is_nan() && b.is_nan()) || (a - b).abs() <= 1e-7 * (1.0 + b.abs())
    }

    #[test]
    fn rolling_sums_match_brute_force() {
        let xs = noisy(500);
        for n in [1, 2, 7, 20] {
            let s = sum(&xs, n);
            let var = variance(&xs, n);
            for i in 0..xs.len() {
                if i + 1 < n {
                    assert!(s[i].is_nan());
                    continue;
                }
                let w = &xs[i + 1 - n..=i];
                let m = w.iter().sum::<f64>() / n as f64;
                assert!(near(s[i], w.iter().sum()), "sum n={n} i={i}");
                let v = w.iter().map(|x| (x - m).powi(2)).sum::<f64>() / n as f64;
                assert!(near(var[i], v), "var n={n} i={i}: {} vs {v}", var[i]);
            }
        }
    }

    #[test]
    fn gaps_only_blank_the_windows_that_hold_them() {
        let mut xs = noisy(40);
        xs[10] = NAN;
        let s = sum(&xs, 5);
        assert!(s[9].is_finite());
        assert!((10..15).all(|i| s[i].is_nan()));
        assert!(near(s[15], xs[11..16].iter().sum()));
        assert!(highest(&xs, 5)[14].is_nan() && highest(&xs, 5)[15].is_finite());
    }

    #[test]
    fn extremes_match_brute_force() {
        let xs = noisy(400);
        for n in [1, 3, 14, 52] {
            let (h, l) = (highest(&xs, n), lowest(&xs, n));
            for i in n - 1..xs.len() {
                let w = &xs[i + 1 - n..=i];
                assert_eq!(h[i], w.iter().cloned().fold(f64::MIN, f64::max));
                assert_eq!(l[i], w.iter().cloned().fold(f64::MAX, f64::min));
            }
        }
        // Ties resolve to the most recent bar.
        assert_eq!(extreme_at(&[1.0, 3.0, 2.0, 3.0], 4, true)[3], 3);
    }

    #[test]
    fn regression_on_a_line_is_exact() {
        let xs: Vec<f64> = (0..50).map(|i| 3.0 + 0.5 * i as f64).collect();
        let lr = linreg(&xs, 10);
        let sl = linreg_slope(&xs, 10);
        let r2 = r_squared(&xs, 10);
        let f = tsf(&xs, 10);
        for i in 9..50 {
            assert!(near(lr[i], xs[i]), "{i}");
            assert!(near(sl[i], 0.5));
            assert!(near(r2[i], 1.0));
            assert!(near(f[i], xs[i] + 0.5));
        }
        // Noisy data: compare with a direct fit.
        let ys = noisy(120);
        let n = 25;
        let got = linreg_map(&ys, n, |f| f.slope + f.se);
        let i = 101;
        let w = &ys[i + 1 - n..=i];
        let k = n as f64;
        let mx = (k - 1.0) / 2.0;
        let my = w.iter().sum::<f64>() / k;
        let sxy: f64 = w.iter().enumerate().map(|(j, y)| (j as f64 - mx) * (y - my)).sum();
        let sxx: f64 = (0..n).map(|j| (j as f64 - mx).powi(2)).sum();
        let b = sxy / sxx;
        let a = my - b * mx;
        let sse: f64 = w.iter().enumerate().map(|(j, y)| (y - a - b * j as f64).powi(2)).sum();
        assert!(near(got[i], b + (sse / (k - 2.0)).sqrt()));
    }

    #[test]
    fn rank_median_and_shift() {
        let xs = [5.0, 1.0, 4.0, 2.0, 3.0, 6.0];
        assert_eq!(percent_rank(&xs, 4)[4], 50.0);
        assert_eq!(percent_rank(&xs, 4)[5], 100.0);
        assert_eq!(median(&xs, 3)[2], 4.0);
        assert_eq!(median(&xs, 4)[5], 3.5);
        assert_eq!(displace(&[1.0, 2.0], 2)[3], 2.0);
        let back = displace(&[1.0, 2.0, 3.0], -1);
        assert_eq!(&back[..2], &[2.0, 3.0]);
        assert!(back[2].is_nan());
        assert_eq!(change(&[1.0, 4.0, 9.0], 2)[2], 8.0);
        assert!(zscore(&[2.0; 10], 5)[9] == 0.0);
    }
}
