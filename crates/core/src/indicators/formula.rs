//! Explicit chart formula conventions. Defaults preserve existing saved studies;
//! these choices implement documented equations, not certified vendor outputs.

use serde::{Deserialize, Serialize};

use super::registry::{Input, Output, Spec};
use super::{ema, hma, rma, rsi, sma, stdev, true_range, wma};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Average {
    Simple,
    Exponential,
    FrontWeighted,
    Hull,
    Wilder,
}

impl Average {
    pub const ALL: [Self; 5] = [Self::Simple, Self::Exponential, Self::FrontWeighted, Self::Hull, Self::Wilder];
    pub fn label(self) -> &'static str {
        match self {
            Self::Simple => "Simple",
            Self::Exponential => "Exponential",
            Self::FrontWeighted => "Front weighted",
            Self::Hull => "Hull",
            Self::Wilder => "Wilder",
        }
    }
    pub fn compute(self, values: &[f64], n: usize) -> Vec<f64> {
        match self {
            Self::Simple => sma(values, n),
            Self::Exponential => ema(values, n),
            Self::FrontWeighted => wma(values, n),
            Self::Hull => hma(values, n),
            Self::Wilder => rma(values, n),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RsiMethod {
    #[default]
    Wilder,
    Plain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BandwidthUnits {
    Ratio,
    #[default]
    Percent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Formula {
    pub average: Option<Average>,
    pub rsi_method: RsiMethod,
    pub rsi_smoothing: usize,
    pub bandwidth_units: BandwidthUnits,
    pub undefined_zero_division: bool,
    /// Value from this many bars ago; always causal and applied before consumers.
    pub bars_ago: usize,
}

impl Default for Formula {
    fn default() -> Self {
        Self {
            average: None,
            rsi_method: RsiMethod::Wilder,
            rsi_smoothing: 1,
            bandwidth_units: BandwidthUnits::Percent,
            undefined_zero_division: false,
            bars_ago: 0,
        }
    }
}

/// Plain RSI uses rolling simple averages of finite adjacent gain/loss pairs.
/// Flat windows retain the application's neutral-50 convention.
pub fn plain_rsi(values: &[f64], n: usize) -> Vec<f64> {
    let mut gains = vec![f64::NAN; values.len()];
    let mut losses = gains.clone();
    for i in 1..values.len() {
        let d = values[i] - values[i - 1];
        if d.is_finite() {
            gains[i] = d.max(0.);
            losses[i] = (-d).max(0.);
        }
    }
    sma(&gains, n)
        .into_iter()
        .zip(sma(&losses, n))
        .map(|(g, l)| {
            if !g.is_finite() || !l.is_finite() {
                f64::NAN
            } else if l == 0. {
                if g == 0. { 50. } else { 100. }
            } else {
                100. - 100. / (1. + g / l)
            }
        })
        .collect()
}

fn quotient(a: f64, b: f64, legacy: f64, undefined: bool) -> f64 {
    if !a.is_finite() || !b.is_finite() {
        f64::NAN
    } else if b == 0. {
        if undefined { f64::NAN } else { legacy }
    } else {
        a / b
    }
}

pub fn compute(spec: &Spec, input: &Input<'_>) -> Result<Vec<Output>, String> {
    let defaults = Formula::default();
    let f = spec.chart.as_ref().map(|c| &c.formula).unwrap_or(&defaults);
    if spec.chart.as_ref().is_some_and(|c| c.displacement.unsigned_abs() > 1000) {
        return Err("Plot displacement must be between -1000 and 1000 bars".into());
    }
    if f.bars_ago > 1000 || !(1..=1000).contains(&f.rsi_smoothing) {
        return Err("Offsets must be 0–1000 and RSI smoothing must be 1–1000".into());
    }
    if f.average.is_some() && !matches!(spec.code.as_str(), "BB" | "BBW" | "PCTB" | "ATR" | "MACD" | "ENV") {
        return Err("This indicator does not support an average-method override".into());
    }
    if f.rsi_method == RsiMethod::Wilder && f.rsi_smoothing != 1 {
        return Err("Use a child moving average to smooth Wilder RSI".into());
    }
    if *f == defaults {
        return Ok(spec.compute(input));
    }
    let mut outputs = spec.compute(input);
    let params = spec.def().map(|d| d.normalize(&spec.params)).unwrap_or_default();
    let values = &input.d.close;
    let n = params.first().copied().unwrap_or(1.) as usize;
    match spec.code.as_str() {
        "RSI" => {
            let result = match f.rsi_method {
                RsiMethod::Wilder => rsi(values, n),
                RsiMethod::Plain => plain_rsi(values, n),
            };
            outputs[0].values = if f.rsi_smoothing == 1 { result } else { sma(&result, f.rsi_smoothing) };
        }
        "BB" | "BBW" | "PCTB" => {
            let center = f.average.unwrap_or(Average::Simple).compute(values, n);
            // Dispersion always uses the arithmetic mean, even with an EMA/Hull center.
            let deviation = stdev(values, n);
            let width: Vec<_> = deviation.iter().map(|v| params[1] * v).collect();
            match spec.code.as_str() {
                "BB" => {
                    outputs[0].values = center.iter().zip(&width).map(|(c, w)| c + w).collect();
                    outputs[1].values = center.clone();
                    outputs[2].values = center.iter().zip(&width).map(|(c, w)| c - w).collect();
                }
                "BBW" => {
                    let factor = if f.bandwidth_units == BandwidthUnits::Percent { 100. } else { 1. };
                    outputs[0].values = width
                        .iter()
                        .zip(&center)
                        .map(|(w, c)| factor * quotient(2. * w, *c, 0., f.undefined_zero_division))
                        .collect();
                }
                _ => {
                    outputs[0].values = values
                        .iter()
                        .zip(&center)
                        .zip(&width)
                        .map(|((v, c), w)| quotient(v - c + w, 2. * w, 0.5, f.undefined_zero_division))
                        .collect()
                }
            }
        }
        "ATR" => outputs[0].values = f.average.unwrap_or(Average::Wilder).compute(&true_range(&input.d), n),
        "ENV" => {
            let center = f.average.unwrap_or(Average::Simple).compute(values, n);
            outputs[0].values = center.iter().map(|c| c * (1. + params[1] / 100.)).collect();
            outputs[1].values = center.clone();
            outputs[2].values = center.iter().map(|c| c * (1. - params[1] / 100.)).collect();
        }
        "MACD" if f.average.is_some() => {
            let average = f.average.unwrap();
            let fast = average.compute(values, n);
            let slow = average.compute(values, params[1] as usize);
            let macd: Vec<_> = fast.iter().zip(slow).map(|(a, b)| a - b).collect();
            let signal = average.compute(&macd, params[2] as usize);
            outputs[0].values = macd.clone();
            outputs[1].values = signal.clone();
            outputs[2].values = macd.iter().zip(signal).map(|(a, b)| a - b).collect();
        }
        _ => {}
    }
    if f.bars_ago > 0 {
        for output in &mut outputs {
            let mut lagged = vec![f64::NAN; output.values.len()];
            if f.bars_ago < lagged.len() {
                let count = lagged.len() - f.bars_ago;
                lagged[f.bars_ago..].copy_from_slice(&output.values[..count]);
            }
            output.values = lagged;
        }
    }
    Ok(outputs)
}

/// Visual displacement never feeds back into calculation sources. Positive shifts
/// plot to the right; negative shifts plot to the left and leave an undefined tail.
pub fn display_values(values: &[f64], displacement: i16) -> Vec<f64> {
    let shift = displacement.unsigned_abs() as usize;
    if displacement >= 0 {
        let mut out = vec![f64::NAN; shift];
        out.extend_from_slice(values);
        out
    } else {
        let mut out = vec![f64::NAN; values.len()];
        if shift < values.len() {
            out[..values.len() - shift].copy_from_slice(&values[shift..]);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::study;
    use super::*;
    use crate::{Bar, Timeframe};

    fn run(code: &str, params: &[f64], closes: &[f64], f: Formula) -> Vec<Output> {
        let bars: Vec<_> = closes.iter().enumerate().map(|(i, &c)| Bar::new(i as i64, c, c, c, c, 1.)).collect();
        let mut specs = vec![Spec::new(code, params).unwrap()];
        study::assign_ids(&mut specs);
        specs[0].chart.as_mut().unwrap().formula = f;
        compute(&specs[0], &Input::new(&bars, Timeframe::D1)).unwrap()
    }
    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn display_offsets_preserve_missing_values_and_never_wrap() {
        let values = [1., f64::NAN, 3., 4.];
        let right = display_values(&values, 2);
        assert_eq!(right.len(), 6);
        assert!(right[..2].iter().all(|v| v.is_nan()));
        assert_eq!(right[2], 1.);
        assert!(right[3].is_nan());
        assert_eq!(&right[4..], &[3., 4.]);
        let left = display_values(&values, -2);
        assert_eq!(&left[..2], &[3., 4.]);
        assert!(left[2..].iter().all(|v| v.is_nan()));
        assert!(display_values(&values, -20).iter().all(|v| v.is_nan()));
    }

    #[test]
    fn plain_rsi_and_optional_sma_match_hand_computed_windows() {
        let values = [1., 2., 1., 3., 2., 5.];
        let f = Formula { rsi_method: RsiMethod::Plain, ..Default::default() };
        let r = run("RSI", &[3.], &values, f.clone());
        assert!(r[0].values[..3].iter().all(|v| v.is_nan()));
        near(r[0].values[3], 75.);
        near(r[0].values[4], 50.);
        near(r[0].values[5], 250. / 3.);
        let r = run("RSI", &[3.], &values, Formula { rsi_smoothing: 2, ..f });
        near(r[0].values[4], 62.5);
        near(r[0].values[5], 200. / 3.);
    }

    #[test]
    fn bollinger_dispersion_is_independent_of_the_center_method() {
        let values = [1., 2., 4., 2., 8., 5.];
        let b = run("BB", &[3., 2.], &values, Formula { average: Some(Average::Exponential), ..Default::default() });
        near(b[1].values[3], 13. / 6.);
        near(b[0].values[3] - b[1].values[3], 4. * 2_f64.sqrt() / 3.);
        let w = run("BB", &[3., 2.], &values, Formula { average: Some(Average::FrontWeighted), ..Default::default() });
        near(w[1].values[2], 17. / 6.);
        near(w[0].values[2] - w[1].values[2], 2. * 14_f64.sqrt() / 3.);
    }

    #[test]
    fn bandwidth_ratio_and_percent_and_degenerate_windows_are_explicit() {
        let values = [1., 2., 4., 2., 8., 5.];
        let percent = run("BBW", &[3., 2.], &values, Formula::default());
        let ratio =
            run("BBW", &[3., 2.], &values, Formula { bandwidth_units: BandwidthUnits::Ratio, ..Default::default() });
        near(percent[0].values[3], 100. * ratio[0].values[3]);
        let flat = run("PCTB", &[3., 2.], &[1.; 5], Formula { undefined_zero_division: true, ..Default::default() });
        assert!(flat[0].values.iter().all(|v| v.is_nan()));
    }

    #[test]
    fn simple_macd_and_causal_offset_match_manual_values() {
        let values = [1., 4., 2., 8., 3., 9.];
        let f = Formula { average: Some(Average::Simple), ..Default::default() };
        let m = run("MACD", &[2., 3., 2.], &values, f.clone());
        near(m[0].values[2], 2. / 3.);
        near(m[0].values[3], 1. / 3.);
        near(m[1].values[3], 0.5);
        near(m[2].values[3], -1. / 6.);
        let lag = run("MACD", &[2., 3., 2.], &values, Formula { bars_ago: 1, ..f });
        near(lag[0].values[3], m[0].values[2]);
    }

    #[test]
    fn atr_methods_match_known_ranges() {
        let bars: Vec<_> = [2., 3., 5., 4., 7.]
            .iter()
            .enumerate()
            .map(|(i, &r)| Bar::new(i as i64, 100., 100. + r / 2., 100. - r / 2., 100., 1.))
            .collect();
        let mut specs = vec![Spec::new("ATR", &[3.]).unwrap()];
        study::assign_ids(&mut specs);
        specs[0].chart.as_mut().unwrap().formula.average = Some(Average::Simple);
        let result = compute(&specs[0], &Input::new(&bars, Timeframe::D1)).unwrap();
        near(result[0].values[2], 10. / 3.);
        near(result[0].values[4], 16. / 3.);
        specs[0].chart.as_mut().unwrap().formula.average = Some(Average::Wilder);
        let result = compute(&specs[0], &Input::new(&bars, Timeframe::D1)).unwrap();
        near(result[0].values[4], 127. / 27.);
    }
}
