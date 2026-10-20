//! Independent, provider-agnostic market scan conditions evaluated at the latest bar.
//!
//! The caller chooses the symbol universe and obtains historical data. This module
//! never substitutes sample data, decides data freshness, or fetches market prices.

use serde::{Deserialize, Serialize};

use crate::{Bar, Series, Timeframe, indicators};

pub const MAX_PERIOD: usize = 500;
pub const MAX_CONDITIONS: usize = 32;
pub const MAX_BARS_AGO: usize = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MatchMode {
    #[default]
    All,
    Any,
}

/// Bar field a moving average is computed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Source {
    #[default]
    Close,
    Volume,
}

impl Source {
    pub const ALL: [Self; 2] = [Self::Close, Self::Volume];

    pub fn label(self) -> &'static str {
        match self {
            Self::Close => "Close",
            Self::Volume => "Volume",
        }
    }

    /// The window's values, rejecting gaps and negative volume rather than averaging them away.
    fn values(self, bars: &[Bar]) -> Option<Vec<f64>> {
        let values: Vec<f64> = bars
            .iter()
            .map(|b| match self {
                Self::Close => b.close,
                Self::Volume => b.volume,
            })
            .collect();
        let valid = |v: &f64| v.is_finite() && (self == Self::Close || *v >= 0.0);
        values.iter().all(valid).then_some(values)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ScanOperand {
    Price,
    Volume,
    /// Percent change from the previous bar's close, not the previous session.
    ChangePercent,
    Sma {
        period: usize,
        /// Older saved scans have no source and average closes.
        #[serde(default)]
        source: Source,
    },
    /// Exponential moving average seeded with the SMA of its first `period` values.
    Ema {
        period: usize,
        #[serde(default)]
        source: Source,
    },
    /// Wilder average true range; ATR(1) is the latest bar's true range.
    Atr {
        period: usize,
    },
    /// Wilder RSI of closing prices, using the supplied history for its seed.
    Rsi {
        period: usize,
    },
    Constant(f64),
}

impl ScanOperand {
    pub fn label(&self) -> String {
        match self {
            Self::Price => "Close".into(),
            Self::Volume => "Volume".into(),
            Self::ChangePercent => "Change % (1 bar)".into(),
            Self::Sma { period, source: Source::Close } => format!("SMA({period})"),
            Self::Sma { period, source } => format!("SMA({period}, {})", source.label()),
            Self::Ema { period, source: Source::Close } => format!("EMA({period})"),
            Self::Ema { period, source } => format!("EMA({period}, {})", source.label()),
            Self::Atr { period } => format!("ATR({period})"),
            Self::Rsi { period } => format!("RSI({period})"),
            Self::Constant(value) => format!("{value}"),
        }
    }

    fn validate(self) -> Result<(), String> {
        match self {
            Self::Sma { period, .. } | Self::Ema { period, .. } | Self::Atr { period } | Self::Rsi { period }
                if !(1..=MAX_PERIOD).contains(&period) =>
            {
                Err(format!("Indicator period must be between 1 and {MAX_PERIOD}"))
            }
            Self::Constant(value) if !value.is_finite() => Err("Threshold must be finite".into()),
            _ => Ok(()),
        }
    }

    fn required_bars(self) -> usize {
        match self {
            Self::Sma { period, .. } | Self::Ema { period, .. } => period.clamp(1, MAX_PERIOD),
            // True range needs the previous close.
            Self::Atr { period } | Self::Rsi { period } => period.min(MAX_PERIOD).saturating_add(1),
            Self::ChangePercent => 2,
            _ => 1,
        }
    }

    fn value(self, series: &Series, index: usize) -> Option<f64> {
        let bars = &series.bars[..=index];
        if bars.len() < self.required_bars() {
            return None;
        }
        let value = match self {
            Self::Price => bars[index].close,
            Self::Volume => {
                let volume = bars[index].volume;
                if volume < 0.0 {
                    return None;
                }
                volume
            }
            Self::ChangePercent => {
                let previous = bars[index - 1].close;
                let current = bars[index].close;
                if !previous.is_finite() || previous <= 0.0 || !current.is_finite() {
                    return None;
                }
                (current / previous - 1.0) * 100.0
            }
            Self::Sma { period, source } => {
                // Only the current window is relevant; do not let an earlier gap
                // contaminate a complete, finite window.
                let values = source.values(&bars[bars.len() - period..])?;
                *indicators::sma(&values, period).last()?
            }
            Self::Ema { period, source } => {
                // Like RSI, the seed carries through all history. The indicator skips
                // non-finite inputs silently, so gaps are rejected here instead.
                let values = source.values(bars)?;
                *indicators::ema(&values, period).last()?
            }
            Self::Atr { period } => {
                // Wilder smoothing carries its seed, like RSI: reject any gap in the history.
                if bars.iter().any(|b| !(b.high.is_finite() && b.low.is_finite() && b.close.is_finite())) {
                    return None;
                }
                *indicators::atr(&indicators::Ohlcv::new(bars), period).last()?
            }
            Self::Rsi { period } => {
                let closes = bars.iter().map(|b| b.close).collect::<Vec<_>>();
                // Wilder smoothing depends on all its seeded history. Explicitly
                // reject gaps: floating-point max() in the indicator can mask NaNs.
                if closes.iter().any(|v| !v.is_finite()) {
                    return None;
                }
                // Reject arithmetic overflow before RSI's zero-loss branch can
                // turn an infinite gain into an apparently valid RSI of 100.
                closes.windows(2).try_fold(0.0_f64, |total, pair| {
                    let total = total + (pair[1] - pair[0]).abs();
                    total.is_finite().then_some(total)
                })?;
                *indicators::rsi(&closes, period).last()?
            }
            Self::Constant(value) => value,
        };
        value.is_finite().then_some(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Comparison {
    Above,
    Below,
    AtLeast,
    AtMost,
    CrossedAbove,
    CrossedBelow,
}

impl Comparison {
    pub const ALL: [Self; 6] =
        [Self::Above, Self::Below, Self::AtLeast, Self::AtMost, Self::CrossedAbove, Self::CrossedBelow];

    pub fn label(self) -> &'static str {
        match self {
            Self::Above => ">",
            Self::Below => "<",
            Self::AtLeast => ">=",
            Self::AtMost => "<=",
            Self::CrossedAbove => "crossed above",
            Self::CrossedBelow => "crossed below",
        }
    }

    fn is_cross(self) -> bool {
        matches!(self, Self::CrossedAbove | Self::CrossedBelow)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanCondition {
    pub left: ScanOperand,
    pub comparison: Comparison,
    pub right: ScanOperand,
    /// Evaluate the left operand this many bars before the latest (0 = latest, 1 = yesterday on 1D).
    #[serde(default)]
    pub left_bars_ago: usize,
    #[serde(default)]
    pub right_bars_ago: usize,
    /// Scales the right operand, e.g. `volume >= EMA(50, Volume) × 2.5`.
    #[serde(default = "one")]
    pub right_multiplier: f64,
}

fn one() -> f64 {
    1.0
}

impl Default for ScanCondition {
    fn default() -> Self {
        Self {
            left: ScanOperand::Price,
            comparison: Comparison::Above,
            right: ScanOperand::Sma { period: 20, source: Source::Close },
            left_bars_ago: 0,
            right_bars_ago: 0,
            right_multiplier: 1.0,
        }
    }
}

fn ago_label(label: String, bars_ago: usize) -> String {
    match bars_ago {
        0 => label,
        1 => format!("{label} [1 bar ago]"),
        n => format!("{label} [{n} bars ago]"),
    }
}

impl ScanCondition {
    pub fn label(&self) -> String {
        format!(
            "{} {} {}",
            ago_label(self.left.label(), self.left_bars_ago),
            self.comparison.label(),
            ago_label(self.right_label(), self.right_bars_ago)
        )
    }

    fn right_label(&self) -> String {
        if self.right_multiplier == 1.0 {
            self.right.label()
        } else {
            format!("{} × {}", self.right.label(), self.right_multiplier)
        }
    }

    fn validate(&self) -> Result<(), String> {
        self.left.validate()?;
        self.right.validate()?;
        if !self.right_multiplier.is_finite() {
            return Err("Multiplier must be finite".into());
        }
        if self.left_bars_ago.max(self.right_bars_ago) > MAX_BARS_AGO {
            return Err(format!("Bars ago must be between 0 and {MAX_BARS_AGO}"));
        }
        Ok(())
    }

    fn required_bars(&self) -> usize {
        let side = |operand: ScanOperand, ago: usize| operand.required_bars().saturating_add(ago);
        side(self.left, self.left_bars_ago).max(side(self.right, self.right_bars_ago))
            + usize::from(self.comparison.is_cross())
    }

    /// Both operand values for the bar at `index`, each shifted back by its side's bars-ago.
    ///
    /// `index` is the bar being evaluated: the latest bar, or the one before it when a
    /// crossover looks at its previous state. Return `None` for a side whose shifted bar
    /// would fall before the first bar of the series.
    fn values_at(&self, series: &Series, index: usize) -> (Option<f64>, Option<f64>) {
        let side = |operand: ScanOperand, bars_ago: usize| {
            index.checked_sub(bars_ago).and_then(|shifted| operand.value(series, shifted))
        };
        let right = side(self.right, self.right_bars_ago).map(|v| v * self.right_multiplier);
        (side(self.left, self.left_bars_ago), right.filter(|v| v.is_finite()))
    }

    fn evaluate(&self, series: &Series) -> ConditionEvaluation {
        let Some(index) = series.bars.len().checked_sub(1) else {
            return ConditionEvaluation::unavailable("No bars", None, None);
        };
        let (left, right) = self.values_at(series, index);
        let (Some(a), Some(b)) = (left, right) else {
            return ConditionEvaluation::unavailable("Insufficient history or invalid input", left, right);
        };
        let matched = match self.comparison {
            Comparison::Above => a > b,
            Comparison::Below => a < b,
            Comparison::AtLeast => a >= b,
            Comparison::AtMost => a <= b,
            Comparison::CrossedAbove | Comparison::CrossedBelow => {
                let previous = index.checked_sub(1).and_then(|i| match self.values_at(series, i) {
                    (Some(a), Some(b)) => Some((a, b)),
                    _ => None,
                });
                let Some((previous_a, previous_b)) = previous else {
                    return ConditionEvaluation::unavailable("Crossover needs a valid previous bar", left, right);
                };
                if self.comparison == Comparison::CrossedAbove {
                    previous_a <= previous_b && a > b
                } else {
                    previous_a >= previous_b && a < b
                }
            }
        };
        ConditionEvaluation {
            outcome: if matched { ScanOutcome::Match } else { ScanOutcome::NoMatch },
            left,
            right,
            reason: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanDefinition {
    pub name: String,
    pub timeframe: Timeframe,
    pub match_mode: MatchMode,
    pub conditions: Vec<ScanCondition>,
}

impl Default for ScanDefinition {
    fn default() -> Self {
        Self {
            name: "Price above SMA 20".into(),
            timeframe: Timeframe::D1,
            match_mode: MatchMode::All,
            conditions: vec![
                ScanCondition::default(),
                ScanCondition {
                    left: ScanOperand::Volume,
                    comparison: Comparison::AtLeast,
                    right: ScanOperand::Constant(100_000.0),
                    ..ScanCondition::default()
                },
            ],
        }
    }
}

impl ScanDefinition {
    /// Built-in scans offered alongside the user's saved library.
    pub fn presets() -> Vec<Self> {
        vec![Self::momentum_ignition(), Self::volume_thrust()]
    }

    /// The first day RSI(14) reaches overbought: at least 70 today after closing below 69.9
    /// yesterday, with the close holding its 4-day EMA, on stocks above $1 trading over
    /// 100,000 shares. StockCharts form: `[sma(1,daily volume) > 100000] and
    /// [sma(1,daily close) > 1] and [daily rsi(14) >= 70] and
    /// [daily close >= daily ema(4,daily close)] and [yesterday's daily rsi(14) < 69.9]`.
    /// Overbought momentum on a volume explosion: RSI(14) at least 69.8 on at least 2.5× the
    /// 50-day EMA of volume, with a true range over $0.50, on stocks of at least $0.20 trading
    /// over 100,000 shares. StockCharts form: `[sma(1,daily volume) > 100000] and
    /// [daily rsi(14) >= 69.8] and [daily volume >= daily ema(50,daily volume) * 2.5] and
    /// [daily close >= 0.2] and [ATR(1) > .50]`.
    pub fn volume_thrust() -> Self {
        let condition = |left, comparison, right| ScanCondition { left, comparison, right, ..ScanCondition::default() };
        Self {
            name: "Volume Thrust".into(),
            timeframe: Timeframe::D1,
            match_mode: MatchMode::All,
            conditions: vec![
                condition(
                    ScanOperand::Sma { period: 1, source: Source::Volume },
                    Comparison::Above,
                    ScanOperand::Constant(100_000.0),
                ),
                condition(ScanOperand::Rsi { period: 14 }, Comparison::AtLeast, ScanOperand::Constant(69.8)),
                ScanCondition {
                    right_multiplier: 2.5,
                    ..condition(
                        ScanOperand::Volume,
                        Comparison::AtLeast,
                        ScanOperand::Ema { period: 50, source: Source::Volume },
                    )
                },
                condition(ScanOperand::Price, Comparison::AtLeast, ScanOperand::Constant(0.2)),
                condition(ScanOperand::Atr { period: 1 }, Comparison::Above, ScanOperand::Constant(0.5)),
            ],
        }
    }

    pub fn momentum_ignition() -> Self {
        let condition = |left, comparison, right, left_bars_ago| ScanCondition {
            left,
            comparison,
            right,
            left_bars_ago,
            ..ScanCondition::default()
        };
        let rsi = ScanOperand::Rsi { period: 14 };
        Self {
            name: "Momentum Ignition".into(),
            timeframe: Timeframe::D1,
            match_mode: MatchMode::All,
            conditions: vec![
                condition(
                    ScanOperand::Sma { period: 1, source: Source::Volume },
                    Comparison::Above,
                    ScanOperand::Constant(100_000.0),
                    0,
                ),
                condition(
                    ScanOperand::Sma { period: 1, source: Source::Close },
                    Comparison::Above,
                    ScanOperand::Constant(1.0),
                    0,
                ),
                condition(rsi, Comparison::AtLeast, ScanOperand::Constant(70.0), 0),
                condition(
                    ScanOperand::Price,
                    Comparison::AtLeast,
                    ScanOperand::Ema { period: 4, source: Source::Close },
                    0,
                ),
                condition(rsi, Comparison::Below, ScanOperand::Constant(69.9), 1),
            ],
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.chars().count() > 100 {
            return Err("Scan name must contain 1 to 100 characters".into());
        }
        if self.conditions.is_empty() || self.conditions.len() > MAX_CONDITIONS {
            return Err(format!("A scan needs between 1 and {MAX_CONDITIONS} conditions"));
        }
        for condition in &self.conditions {
            condition.validate()?;
        }
        Ok(())
    }

    /// Minimum history needed for every condition. EMA and RSI may differ with longer
    /// histories because exponential smoothing preserves its initial seed.
    pub fn required_bars(&self) -> usize {
        self.conditions.iter().map(ScanCondition::required_bars).max().unwrap_or(1)
    }

    /// Evaluate the last supplied bar, including an in-progress bar if present.
    /// Three-valued logic: a known false settles ALL; a known true settles ANY;
    /// otherwise any unavailable condition makes the result unavailable.
    pub fn evaluate(&self, series: &Series) -> ScanEvaluation {
        let invalid = self.validate().err().or_else(|| {
            if series.timeframe != self.timeframe {
                Some("Series timeframe does not match scan".into())
            } else if series.bars.windows(2).any(|w| w[0].ts >= w[1].ts) {
                Some("Bars must have strictly increasing timestamps".into())
            } else {
                None
            }
        });
        if let Some(reason) = invalid {
            return ScanEvaluation { outcome: ScanOutcome::Unavailable, conditions: Vec::new(), reason: Some(reason) };
        }
        let conditions: Vec<_> = self.conditions.iter().map(|c| c.evaluate(series)).collect();
        let any_match = conditions.iter().any(|c| c.outcome == ScanOutcome::Match);
        let any_no_match = conditions.iter().any(|c| c.outcome == ScanOutcome::NoMatch);
        let any_unavailable = conditions.iter().any(|c| c.outcome == ScanOutcome::Unavailable);
        let outcome = match self.match_mode {
            MatchMode::All if any_no_match => ScanOutcome::NoMatch,
            MatchMode::Any if any_match => ScanOutcome::Match,
            _ if any_unavailable => ScanOutcome::Unavailable,
            MatchMode::All => ScanOutcome::Match,
            MatchMode::Any => ScanOutcome::NoMatch,
        };
        let reason = (outcome == ScanOutcome::Unavailable).then(|| "One or more conditions lack valid data".into());
        ScanEvaluation { outcome, conditions, reason }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanOutcome {
    Match,
    NoMatch,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConditionEvaluation {
    pub outcome: ScanOutcome,
    pub left: Option<f64>,
    pub right: Option<f64>,
    pub reason: Option<String>,
}

impl ConditionEvaluation {
    fn unavailable(reason: &str, left: Option<f64>, right: Option<f64>) -> Self {
        Self { outcome: ScanOutcome::Unavailable, left, right, reason: Some(reason.into()) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScanEvaluation {
    pub outcome: ScanOutcome,
    pub conditions: Vec<ConditionEvaluation>,
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Bar;

    fn series(closes: &[f64]) -> Series {
        Series::new(
            "TEST",
            Timeframe::D1,
            closes.iter().enumerate().map(|(i, &c)| Bar::new(i as i64, c, c, c, c, 200_000.0)).collect(),
        )
    }

    fn scan(left: ScanOperand, comparison: Comparison, right: ScanOperand) -> ScanDefinition {
        ScanDefinition {
            conditions: vec![ScanCondition { left, comparison, right, ..ScanCondition::default() }],
            ..ScanDefinition::default()
        }
    }

    #[test]
    fn warmup_is_unavailable_never_a_match() {
        let sma = scan(ScanOperand::Price, Comparison::Above, ScanOperand::Sma { period: 3, source: Source::Close });
        assert_eq!(sma.required_bars(), 3);
        assert_eq!(sma.evaluate(&series(&[1.0, 2.0])).outcome, ScanOutcome::Unavailable);
        assert_eq!(sma.evaluate(&series(&[1.0, 2.0, 3.0])).outcome, ScanOutcome::Match);
        let rsi = scan(ScanOperand::Rsi { period: 3 }, Comparison::Above, ScanOperand::Constant(50.0));
        assert_eq!(rsi.required_bars(), 4);
        assert_eq!(rsi.evaluate(&series(&[1.0, 2.0, 3.0])).outcome, ScanOutcome::Unavailable);
        let evaluated = rsi.evaluate(&series(&[1.0, 2.0, 3.0, 4.0]));
        assert_eq!(evaluated.outcome, ScanOutcome::Match);
        assert_eq!(evaluated.conditions[0].left, Some(100.0));
    }

    #[test]
    fn boundary_comparisons_and_change_percent() {
        let s = series(&[100.0, 125.0]);
        for (comparison, expected) in [
            (Comparison::Above, ScanOutcome::NoMatch),
            (Comparison::Below, ScanOutcome::NoMatch),
            (Comparison::AtLeast, ScanOutcome::Match),
            (Comparison::AtMost, ScanOutcome::Match),
        ] {
            assert_eq!(
                scan(ScanOperand::ChangePercent, comparison, ScanOperand::Constant(25.0)).evaluate(&s).outcome,
                expected
            );
        }
        assert_eq!(
            scan(ScanOperand::ChangePercent, Comparison::Above, ScanOperand::Constant(1.0))
                .evaluate(&series(&[0.0, 1.0]))
                .outcome,
            ScanOutcome::Unavailable
        );
    }

    #[test]
    fn crossovers_require_history_and_strict_current_cross() {
        let cross =
            scan(ScanOperand::Price, Comparison::CrossedAbove, ScanOperand::Sma { period: 2, source: Source::Close });
        assert_eq!(cross.required_bars(), 3);
        assert_eq!(cross.evaluate(&series(&[1.0, 2.0])).outcome, ScanOutcome::Unavailable);
        assert_eq!(cross.evaluate(&series(&[2.0, 1.0, 3.0])).outcome, ScanOutcome::Match);
        assert_eq!(cross.evaluate(&series(&[1.0, 2.0, 3.0])).outcome, ScanOutcome::NoMatch);
        let threshold = scan(ScanOperand::Price, Comparison::CrossedAbove, ScanOperand::Constant(2.0));
        assert_eq!(threshold.evaluate(&series(&[1.0, 2.0])).outcome, ScanOutcome::NoMatch);
        assert_eq!(threshold.evaluate(&series(&[2.0, 3.0])).outcome, ScanOutcome::Match);
        let down = scan(ScanOperand::Price, Comparison::CrossedBelow, ScanOperand::Constant(2.0));
        assert_eq!(down.evaluate(&series(&[2.0, 1.0])).outcome, ScanOutcome::Match);
    }

    #[test]
    fn three_valued_logic_preserves_known_answers() {
        let mut def = scan(ScanOperand::Price, Comparison::Above, ScanOperand::Constant(1.0));
        def.conditions.push(ScanCondition {
            left: ScanOperand::Price,
            comparison: Comparison::Above,
            right: ScanOperand::Sma { period: 20, source: Source::Close },
            ..ScanCondition::default()
        });
        def.match_mode = MatchMode::Any;
        assert_eq!(def.evaluate(&series(&[2.0])).outcome, ScanOutcome::Match);
        assert_eq!(def.evaluate(&series(&[0.0])).outcome, ScanOutcome::Unavailable);
        def.match_mode = MatchMode::All;
        assert_eq!(def.evaluate(&series(&[0.0])).outcome, ScanOutcome::NoMatch);
        assert_eq!(def.evaluate(&series(&[2.0])).outcome, ScanOutcome::Unavailable);
    }

    #[test]
    fn invalid_data_and_definitions_do_not_produce_matches() {
        let price = scan(ScanOperand::Price, Comparison::Above, ScanOperand::Constant(0.0));
        assert_eq!(price.evaluate(&series(&[])).outcome, ScanOutcome::Unavailable);
        assert_eq!(price.evaluate(&series(&[f64::INFINITY])).outcome, ScanOutcome::Unavailable);
        let rsi = scan(ScanOperand::Rsi { period: 2 }, Comparison::Above, ScanOperand::Constant(0.0));
        assert_eq!(rsi.evaluate(&series(&[1.0, f64::NAN, 2.0, 3.0])).outcome, ScanOutcome::Unavailable);
        assert_eq!(rsi.evaluate(&series(&[-1e308, 1e308, 1e308])).outcome, ScanOutcome::Unavailable);
        let invalid = scan(
            ScanOperand::Sma { period: usize::MAX, source: Source::Close },
            Comparison::Above,
            ScanOperand::Constant(0.0),
        );
        assert!(invalid.validate().is_err());
        assert_eq!(invalid.evaluate(&series(&[1.0])).outcome, ScanOutcome::Unavailable);
        assert!(
            scan(ScanOperand::Sma { period: 0, source: Source::Close }, Comparison::Above, ScanOperand::Constant(0.0))
                .validate()
                .is_err()
        );
        assert!(scan(ScanOperand::Price, Comparison::Above, ScanOperand::Constant(f64::NAN)).validate().is_err());
        let mut wrong_tf = series(&[3.0]);
        wrong_tf.timeframe = Timeframe::M1;
        assert_eq!(price.evaluate(&wrong_tf).outcome, ScanOutcome::Unavailable);
        let mut wrong_order = series(&[3.0, 4.0]);
        wrong_order.bars[1].ts = wrong_order.bars[0].ts;
        assert_eq!(price.evaluate(&wrong_order).outcome, ScanOutcome::Unavailable);
    }

    #[test]
    fn scans_round_trip_through_saved_json() {
        let definition = ScanDefinition::default();
        assert_eq!(
            serde_json::from_str::<ScanDefinition>(&serde_json::to_string(&definition).unwrap()).unwrap(),
            definition
        );
        let mut invalid = definition;
        invalid.conditions.clear();
        assert!(invalid.validate().is_err());
        invalid.conditions = vec![ScanCondition::default(); MAX_CONDITIONS + 1];
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn ema_and_volume_averages() {
        let ema = scan(ScanOperand::Price, Comparison::AtLeast, ScanOperand::Ema { period: 4, source: Source::Close });
        assert_eq!(ema.required_bars(), 4);
        assert_eq!(ema.evaluate(&series(&[1.0, 2.0, 3.0])).outcome, ScanOutcome::Unavailable);
        // Seed SMA(1..4) = 2.5, then 0.4 * 5 + 0.6 * 2.5 = 3.5.
        let evaluated = ema.evaluate(&series(&[1.0, 2.0, 3.0, 4.0, 5.0]));
        assert_eq!(evaluated.outcome, ScanOutcome::Match);
        assert_eq!(evaluated.conditions[0].right, Some(3.5));
        assert_eq!(ema.evaluate(&series(&[1.0, f64::NAN, 3.0, 4.0, 5.0])).outcome, ScanOutcome::Unavailable);

        let volumes = [50_000.0, 150_000.0, 250_000.0];
        let mut s = series(&[1.0, 1.0, 1.0]);
        s.bars.iter_mut().zip(volumes).for_each(|(b, v)| b.volume = v);
        let avg = scan(
            ScanOperand::Sma { period: 2, source: Source::Volume },
            Comparison::Above,
            ScanOperand::Constant(100_000.0),
        );
        assert_eq!(avg.evaluate(&s).conditions[0].left, Some(200_000.0));
        assert_eq!(avg.evaluate(&s).outcome, ScanOutcome::Match);
        s.bars[1].volume = -1.0;
        assert_eq!(avg.evaluate(&s).outcome, ScanOutcome::Unavailable);
    }

    #[test]
    fn bars_ago_reads_earlier_bars_and_needs_their_history() {
        let s = series(&[1.0, 2.0, 3.0]);
        let mut yesterday = scan(ScanOperand::Price, Comparison::Below, ScanOperand::Constant(2.5));
        assert_eq!(yesterday.evaluate(&s).outcome, ScanOutcome::NoMatch);
        yesterday.conditions[0].left_bars_ago = 1;
        assert_eq!(yesterday.required_bars(), 2);
        let evaluated = yesterday.evaluate(&s);
        assert_eq!((evaluated.outcome, evaluated.conditions[0].left), (ScanOutcome::Match, Some(2.0)));
        yesterday.conditions[0].left_bars_ago = 3;
        assert_eq!(yesterday.evaluate(&s).outcome, ScanOutcome::Unavailable);
        yesterday.conditions[0].left_bars_ago = MAX_BARS_AGO + 1;
        assert!(yesterday.validate().is_err());

        // Today's close against the close two bars earlier.
        let mut rising = scan(ScanOperand::Price, Comparison::Above, ScanOperand::Price);
        rising.conditions[0].right_bars_ago = 2;
        assert_eq!(rising.evaluate(&s).conditions[0].right, Some(1.0));
        assert_eq!(rising.evaluate(&s).outcome, ScanOutcome::Match);

        // A shifted crossover compares the shifted bar with the one before it.
        let mut cross = scan(ScanOperand::Price, Comparison::CrossedAbove, ScanOperand::Constant(1.5));
        cross.conditions[0].left_bars_ago = 1;
        assert_eq!(cross.required_bars(), 3);
        assert_eq!(cross.evaluate(&s).outcome, ScanOutcome::Match);
        assert_eq!(cross.conditions[0].label(), "Close [1 bar ago] crossed above 1.5");
    }

    #[test]
    fn atr_and_multiplier() {
        // Gap up: true range spans yesterday's close (10) to today's high (13).
        let mut s = series(&[10.0, 12.0]);
        s.bars[1].high = 13.0;
        s.bars[1].low = 11.5;
        let tr = scan(ScanOperand::Atr { period: 1 }, Comparison::Above, ScanOperand::Constant(0.5));
        assert_eq!(tr.required_bars(), 2);
        assert_eq!(tr.evaluate(&s).conditions[0].left, Some(3.0));
        assert_eq!(tr.evaluate(&series(&[10.0])).outcome, ScanOutcome::Unavailable);
        s.bars[0].low = f64::NAN;
        assert_eq!(tr.evaluate(&s).outcome, ScanOutcome::Unavailable);

        let mut doubled = scan(ScanOperand::Price, Comparison::AtLeast, ScanOperand::Price);
        doubled.conditions[0].right_bars_ago = 1;
        doubled.conditions[0].right_multiplier = 2.0;
        assert_eq!(doubled.conditions[0].label(), "Close >= Close × 2 [1 bar ago]");
        let evaluated = doubled.evaluate(&series(&[5.0, 10.0]));
        assert_eq!((evaluated.outcome, evaluated.conditions[0].right), (ScanOutcome::Match, Some(10.0)));
        assert_eq!(doubled.evaluate(&series(&[5.0, 9.9])).outcome, ScanOutcome::NoMatch);
        doubled.conditions[0].right_multiplier = f64::INFINITY;
        assert!(doubled.validate().is_err());
    }

    #[test]
    fn volume_thrust_preset() {
        let definition = ScanDefinition::volume_thrust();
        assert!(ScanDefinition::presets().contains(&definition));
        assert_eq!(definition.validate(), Ok(()));
        // 60 quiet, choppy days on 200,000 shares, then a $5 jump on 1,000,000 shares.
        let mut closes: Vec<f64> = (0..60).map(|i| if i % 2 == 0 { 20.0 } else { 20.2 }).collect();
        closes.push(25.0);
        let mut s = series(&closes);
        s.bars.last_mut().unwrap().volume = 1_000_000.0;
        let evaluated = definition.evaluate(&s);
        assert_eq!(evaluated.outcome, ScanOutcome::Match, "{evaluated:?}");
        // The same move on ordinary volume is not a thrust.
        s.bars.last_mut().unwrap().volume = 300_000.0;
        assert_eq!(definition.evaluate(&s).outcome, ScanOutcome::NoMatch);
    }

    #[test]
    fn older_saved_scans_still_load() {
        let json = r#"{"left":"Price","comparison":"Above","right":{"Sma":{"period":20}}}"#;
        assert_eq!(serde_json::from_str::<ScanCondition>(json).unwrap(), ScanCondition::default());
    }

    #[test]
    fn momentum_ignition_preset() {
        let definition = ScanDefinition::momentum_ignition();
        assert!(ScanDefinition::presets().contains(&definition));
        assert_eq!(definition.validate(), Ok(()));
        // A choppy month (RSI near 50), then one strong up day.
        let mut closes: Vec<f64> = (0..40).map(|i| if i % 2 == 0 { 50.0 } else { 51.0 }).collect();
        closes.push(70.0);
        let evaluated = definition.evaluate(&series(&closes));
        assert_eq!(evaluated.outcome, ScanOutcome::Match, "{evaluated:?}");
        assert!(evaluated.conditions[4].left.unwrap() < 69.9);
        // Already overbought yesterday: the breakout happened earlier, so no match today.
        closes.push(80.0);
        assert_eq!(definition.evaluate(&series(&closes)).outcome, ScanOutcome::NoMatch);
    }
}
