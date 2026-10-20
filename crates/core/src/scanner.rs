//! Independent, provider-agnostic market scan conditions evaluated at the latest bar.
//!
//! The caller chooses the symbol universe and obtains historical data. This module
//! never substitutes sample data, decides data freshness, or fetches market prices.

use serde::{Deserialize, Serialize};

use crate::{Series, Timeframe, indicators};

pub const MAX_PERIOD: usize = 500;
pub const MAX_CONDITIONS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MatchMode {
    #[default]
    All,
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ScanOperand {
    Price,
    Volume,
    /// Percent change from the previous bar's close, not the previous session.
    ChangePercent,
    Sma {
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
            Self::Sma { period } => format!("SMA({period})"),
            Self::Rsi { period } => format!("RSI({period})"),
            Self::Constant(value) => format!("{value}"),
        }
    }

    fn validate(self) -> Result<(), String> {
        match self {
            Self::Sma { period } | Self::Rsi { period } if !(1..=MAX_PERIOD).contains(&period) => {
                Err(format!("Indicator period must be between 1 and {MAX_PERIOD}"))
            }
            Self::Constant(value) if !value.is_finite() => Err("Threshold must be finite".into()),
            _ => Ok(()),
        }
    }

    fn required_bars(self) -> usize {
        match self {
            Self::Sma { period } => period.clamp(1, MAX_PERIOD),
            Self::Rsi { period } => period.min(MAX_PERIOD).saturating_add(1),
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
            Self::Sma { period } => {
                // Only the current window is relevant; do not let an earlier gap
                // contaminate a complete, finite window.
                let closes: Vec<f64> = bars[bars.len() - period..].iter().map(|b| b.close).collect();
                if closes.iter().any(|v| !v.is_finite()) {
                    return None;
                }
                *indicators::sma(&closes, period).last()?
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
}

impl Default for ScanCondition {
    fn default() -> Self {
        Self { left: ScanOperand::Price, comparison: Comparison::Above, right: ScanOperand::Sma { period: 20 } }
    }
}

impl ScanCondition {
    pub fn label(&self) -> String {
        format!("{} {} {}", self.left.label(), self.comparison.label(), self.right.label())
    }

    fn evaluate(&self, series: &Series) -> ConditionEvaluation {
        let Some(index) = series.bars.len().checked_sub(1) else {
            return ConditionEvaluation::unavailable("No bars", None, None);
        };
        let left = self.left.value(series, index);
        let right = self.right.value(series, index);
        let (Some(a), Some(b)) = (left, right) else {
            return ConditionEvaluation::unavailable("Insufficient history or invalid input", left, right);
        };
        let matched = match self.comparison {
            Comparison::Above => a > b,
            Comparison::Below => a < b,
            Comparison::AtLeast => a >= b,
            Comparison::AtMost => a <= b,
            Comparison::CrossedAbove | Comparison::CrossedBelow => {
                let previous = index
                    .checked_sub(1)
                    .and_then(|i| Some((self.left.value(series, i)?, self.right.value(series, i)?)));
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
                },
            ],
        }
    }
}

impl ScanDefinition {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.chars().count() > 100 {
            return Err("Scan name must contain 1 to 100 characters".into());
        }
        if self.conditions.is_empty() || self.conditions.len() > MAX_CONDITIONS {
            return Err(format!("A scan needs between 1 and {MAX_CONDITIONS} conditions"));
        }
        for condition in &self.conditions {
            condition.left.validate()?;
            condition.right.validate()?;
        }
        Ok(())
    }

    /// Minimum history needed for every condition. RSI may differ with longer
    /// histories because Wilder smoothing preserves its initial seed.
    pub fn required_bars(&self) -> usize {
        self.conditions
            .iter()
            .map(|c| c.left.required_bars().max(c.right.required_bars()) + usize::from(c.comparison.is_cross()))
            .max()
            .unwrap_or(1)
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
        ScanDefinition { conditions: vec![ScanCondition { left, comparison, right }], ..ScanDefinition::default() }
    }

    #[test]
    fn warmup_is_unavailable_never_a_match() {
        let sma = scan(ScanOperand::Price, Comparison::Above, ScanOperand::Sma { period: 3 });
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
        let cross = scan(ScanOperand::Price, Comparison::CrossedAbove, ScanOperand::Sma { period: 2 });
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
            right: ScanOperand::Sma { period: 20 },
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
        let invalid = scan(ScanOperand::Sma { period: usize::MAX }, Comparison::Above, ScanOperand::Constant(0.0));
        assert!(invalid.validate().is_err());
        assert_eq!(invalid.evaluate(&series(&[1.0])).outcome, ScanOutcome::Unavailable);
        assert!(
            scan(ScanOperand::Sma { period: 0 }, Comparison::Above, ScanOperand::Constant(0.0)).validate().is_err()
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
}
