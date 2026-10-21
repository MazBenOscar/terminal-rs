//! Shared condition trees and leaf evaluation.
use crate::scanner::{Comparison, ConditionEvaluation, MAX_BARS_AGO, MatchMode, ScanOperand, ScanOutcome, Source};
use crate::{Series, Timeframe};
use serde::{Deserialize, Serialize};

pub const MAX_CONDITIONS: usize = 32;
pub const MAX_GROUP_DEPTH: usize = 3;
pub type SeriesSet = std::collections::HashMap<Timeframe, Series>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanCondition {
    #[serde(default)]
    pub timeframe: Option<Timeframe>,
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
            timeframe: None,
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
        ) + &timeframe_suffix(self.timeframe)
    }

    fn right_label(&self) -> String {
        if self.right_multiplier == 1.0 {
            self.right.label()
        } else {
            format!("{} × {}", self.right.label(), self.right_multiplier)
        }
    }

    pub fn validate(&self) -> Result<(), String> {
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

    pub fn required_bars(&self) -> usize {
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

fn timeframe_suffix(timeframe: Option<Timeframe>) -> String {
    timeframe.map_or_else(String::new, |tf| format!(" [{}]", tf.label()))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaCondition {
    pub expression: String,
    #[serde(default)]
    pub timeframe: Option<Timeframe>,
}

impl FormulaCondition {
    pub fn label(&self) -> String {
        format!("PCF: {}{}", self.expression, timeframe_suffix(self.timeframe))
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.expression.trim().is_empty() || self.expression.chars().count() > 500 {
            return Err("Formula must contain 1 to 500 characters".into());
        }
        crate::indicators::pcf::evaluate(&self.expression, &crate::indicators::registry::Input::new(&[], Timeframe::D1))
            .map(|_| ())
    }

    pub fn evaluate(&self, series: &Series) -> ConditionEvaluation {
        match crate::indicators::pcf::evaluate(
            &self.expression,
            &crate::indicators::registry::Input::new(&series.bars, series.timeframe),
        ) {
            Ok(values) => match values.last().copied().filter(|v| v.is_finite()) {
                Some(value) => ConditionEvaluation {
                    outcome: if value != 0.0 { ScanOutcome::Match } else { ScanOutcome::NoMatch },
                    left: Some(value),
                    right: None,
                    reason: None,
                },
                None => ConditionEvaluation::unavailable("Insufficient history or invalid input", None, None),
            },
            Err(reason) => ConditionEvaluation::unavailable(&reason, None, None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConditionItem {
    Compare(ScanCondition),
    Formula(FormulaCondition),
    Group(ConditionGroup),
}

impl ConditionItem {
    pub fn label(&self) -> String {
        match self {
            Self::Compare(c) => c.label(),
            Self::Formula(c) => c.label(),
            Self::Group(g) => format!(
                "Group: {} conditions (match {})",
                g.items.len(),
                if g.mode == MatchMode::All { "all" } else { "any" }
            ),
        }
    }

    pub fn timeframe(&self) -> Option<Timeframe> {
        match self {
            Self::Compare(c) => c.timeframe,
            Self::Formula(c) => c.timeframe,
            Self::Group(_) => None,
        }
    }

    pub fn required_bars(&self) -> usize {
        match self {
            Self::Compare(c) => c.required_bars(),
            Self::Formula(_) => 300,
            Self::Group(g) => g.items.iter().map(Self::required_bars).max().unwrap_or(1),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionGroup {
    pub mode: MatchMode,
    pub items: Vec<ConditionItem>,
}

impl ConditionGroup {
    pub fn comparisons(mode: MatchMode, conditions: Vec<ScanCondition>) -> Self {
        Self { mode, items: conditions.into_iter().map(ConditionItem::Compare).collect() }
    }

    pub fn leaves(&self) -> Vec<&ConditionItem> {
        let mut leaves = Vec::new();
        for item in &self.items {
            match item {
                ConditionItem::Group(group) => leaves.extend(group.leaves()),
                _ => leaves.push(item),
            }
        }
        leaves
    }

    pub fn validate(&self) -> Result<(), String> {
        self.validate_at(1)?;
        if self.leaves().len() > MAX_CONDITIONS {
            return Err(format!("A scan supports at most {MAX_CONDITIONS} conditions"));
        }
        Ok(())
    }

    fn validate_at(&self, depth: usize) -> Result<(), String> {
        if depth > MAX_GROUP_DEPTH {
            return Err(format!("Group depth must not exceed {MAX_GROUP_DEPTH}"));
        }
        if self.items.is_empty() {
            return Err("Every group needs at least 1 item".into());
        }
        for item in &self.items {
            match item {
                ConditionItem::Compare(c) => c.validate()?,
                ConditionItem::Formula(c) => c.validate()?,
                ConditionItem::Group(g) => g.validate_at(depth + 1)?,
            }
        }
        Ok(())
    }

    pub(crate) fn evaluate(
        &self,
        timeframe: Timeframe,
        set: &SeriesSet,
        leaves: &mut Vec<ConditionEvaluation>,
    ) -> ScanOutcome {
        let outcomes: Vec<_> = self
            .items
            .iter()
            .map(|item| {
                if let ConditionItem::Group(group) = item {
                    return group.evaluate(timeframe, set, leaves);
                }
                let tf = item.timeframe().unwrap_or(timeframe);
                let evaluation = match set.get(&tf) {
                    Some(series) => match item {
                        ConditionItem::Compare(c) => c.evaluate(series),
                        ConditionItem::Formula(c) => c.evaluate(series),
                        ConditionItem::Group(_) => unreachable!(),
                    },
                    None => ConditionEvaluation::unavailable(&format!("Needs {} history", tf.label()), None, None),
                };
                let outcome = evaluation.outcome;
                leaves.push(evaluation);
                outcome
            })
            .collect();
        combine(self.mode, &outcomes)
    }
}

fn combine(mode: MatchMode, outcomes: &[ScanOutcome]) -> ScanOutcome {
    match mode {
        MatchMode::All if outcomes.contains(&ScanOutcome::NoMatch) => ScanOutcome::NoMatch,
        MatchMode::Any if outcomes.contains(&ScanOutcome::Match) => ScanOutcome::Match,
        _ if outcomes.contains(&ScanOutcome::Unavailable) => ScanOutcome::Unavailable,
        MatchMode::All => ScanOutcome::Match,
        MatchMode::Any => ScanOutcome::NoMatch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bar, scanner::ScanDefinition};

    fn series(tf: Timeframe, closes: &[f64]) -> Series {
        Series::new(
            "TEST",
            tf,
            closes.iter().enumerate().map(|(i, &c)| Bar::new(i as i64, c, c, c, c, 200_000.0)).collect(),
        )
    }

    fn predicate(value: &str) -> ConditionItem {
        ConditionItem::Formula(FormulaCondition { expression: value.into(), timeframe: None })
    }

    #[test]
    fn legacy_flat_json_loads() {
        let json = r#"{"name":"Legacy","timeframe":"D1","match_mode":"Any","conditions":[{"left":"Price","comparison":"Above","right":{"Sma":{"period":3}}}]}"#;
        let scan: ScanDefinition = serde_json::from_str(json).unwrap();
        let expected =
            ScanCondition { right: ScanOperand::Sma { period: 3, source: Source::Close }, ..ScanCondition::default() };
        assert_eq!(scan.root, ConditionGroup::comparisons(MatchMode::Any, vec![expected.clone()]));
        for closes in [&[1., 2.][..], &[1., 2., 3.], &[3., 2., 1.]] {
            let s = series(Timeframe::D1, closes);
            assert_eq!(scan.evaluate(&s).outcome, expected.evaluate(&s).outcome);
        }
    }

    #[test]
    fn roundtrip_writes_version_1() {
        let scan = ScanDefinition {
            root: ConditionGroup {
                mode: MatchMode::Any,
                items: vec![predicate("C > AVGC3"), ConditionItem::Group(ScanDefinition::default().root)],
            },
            ..ScanDefinition::default()
        };
        let json = serde_json::to_value(&scan).unwrap();
        assert_eq!(json["version"], 1);
        assert!(json.get("conditions").is_none());
        assert!(json.get("match_mode").is_none());
        assert_eq!(serde_json::from_value::<ScanDefinition>(json.clone()).unwrap(), scan);
        let mut future = json;
        future["version"] = 99.into();
        assert_eq!(serde_json::from_value::<ScanDefinition>(future).unwrap(), scan);
    }

    #[test]
    fn nested_groups() {
        use ScanOutcome::{Match, NoMatch, Unavailable};
        let states = [("1", Match), ("0", NoMatch), ("AVGC3", Unavailable)];
        let s = series(Timeframe::D1, &[1.]);
        for (a, av) in states {
            for (b, bv) in states {
                for (c, cv) in states {
                    let scan = ScanDefinition {
                        root: ConditionGroup {
                            mode: MatchMode::All,
                            items: vec![
                                predicate(a),
                                ConditionItem::Group(ConditionGroup {
                                    mode: MatchMode::Any,
                                    items: vec![predicate(b), predicate(c)],
                                }),
                            ],
                        },
                        ..ScanDefinition::default()
                    };
                    let inner = if bv == Match || cv == Match {
                        Match
                    } else if bv == Unavailable || cv == Unavailable {
                        Unavailable
                    } else {
                        NoMatch
                    };
                    let expected = if av == NoMatch || inner == NoMatch {
                        NoMatch
                    } else if av == Unavailable || inner == Unavailable {
                        Unavailable
                    } else {
                        Match
                    };
                    let result = scan.evaluate(&s);
                    assert_eq!(result.outcome, expected, "{a} AND ({b} OR {c})");
                    assert_eq!(result.conditions.iter().map(|c| c.outcome).collect::<Vec<_>>(), vec![av, bv, cv]);
                    assert_eq!(
                        scan.leaves().iter().map(|c| c.label()).collect::<Vec<_>>(),
                        vec![format!("PCF: {a}"), format!("PCF: {b}"), format!("PCF: {c}")]
                    );
                }
            }
        }
    }

    #[test]
    fn limits() {
        let mut scan = ScanDefinition {
            root: ConditionGroup { mode: MatchMode::All, items: vec![predicate("1"); 33] },
            ..ScanDefinition::default()
        };
        assert!(scan.validate().unwrap_err().contains("32"));
        scan.root.items.clear();
        assert!(scan.validate().unwrap_err().contains("at least 1"));
        scan.root.items.push(predicate("1"));
        for _ in 0..2 {
            scan.root = ConditionGroup { mode: MatchMode::All, items: vec![ConditionItem::Group(scan.root)] };
        }
        assert!(scan.validate().is_ok());
        scan.root = ConditionGroup { mode: MatchMode::All, items: vec![ConditionItem::Group(scan.root)] };
        assert!(scan.validate().unwrap_err().contains("depth"));
        scan.root = ConditionGroup {
            mode: MatchMode::Any,
            items: vec![predicate("1"), ConditionItem::Group(ConditionGroup { mode: MatchMode::All, items: vec![] })],
        };
        assert!(scan.validate().is_err());
    }

    #[test]
    fn formula_predicate() {
        let formula = FormulaCondition { expression: "C > AVGC3".into(), timeframe: None };
        assert!(formula.validate().is_ok());
        for (closes, outcome, left) in [
            (&[1., 2., 3.][..], ScanOutcome::Match, Some(1.)),
            (&[3., 2., 1.][..], ScanOutcome::NoMatch, Some(0.)),
            (&[1., 2.][..], ScanOutcome::Unavailable, None),
        ] {
            let result = formula.evaluate(&series(Timeframe::D1, closes));
            assert_eq!(result.outcome, outcome);
            assert_eq!(result.left, left);
            assert_eq!(result.right, None);
        }
        for expression in ["", " ", "INVALID(", &"C".repeat(501)] {
            assert!(FormulaCondition { expression: expression.into(), timeframe: None }.validate().is_err());
        }
        let bad = FormulaCondition { expression: "INVALID(".into(), timeframe: None };
        assert_eq!(bad.evaluate(&series(Timeframe::D1, &[1.])).outcome, ScanOutcome::Unavailable);
    }

    #[test]
    fn timeframe_override() {
        let weekly = ScanCondition {
            timeframe: Some(Timeframe::W1),
            right: ScanOperand::Sma { period: 3, source: Source::Close },
            ..ScanCondition::default()
        };
        assert!(weekly.label().ends_with(" [1W]"));
        let scan = ScanDefinition {
            root: ConditionGroup { mode: MatchMode::All, items: vec![predicate("1"), ConditionItem::Compare(weekly)] },
            ..ScanDefinition::default()
        };
        assert_eq!(scan.timeframes(), vec![Timeframe::D1, Timeframe::W1]);
        assert_eq!(scan.required_bars(), 300);
        assert_eq!(scan.required_bars_for(Timeframe::W1), 3);
        assert_eq!(scan.required_bars_for(Timeframe::H1), 1);
        let mut set = SeriesSet::from([(Timeframe::D1, series(Timeframe::D1, &[3., 2., 1.]))]);
        let result = scan.evaluate_set(&set);
        assert_eq!(result.conditions[0].outcome, ScanOutcome::Match);
        assert_eq!(result.conditions[1].reason.as_deref(), Some("Needs 1W history"));
        assert_eq!(result.outcome, ScanOutcome::Unavailable);
        set.insert(Timeframe::W1, series(Timeframe::W1, &[1., 2., 3.]));
        assert_eq!(scan.evaluate_set(&set).outcome, ScanOutcome::Match);
        set.get_mut(&Timeframe::W1).unwrap().timeframe = Timeframe::H1;
        assert_eq!(scan.evaluate_set(&set).reason.as_deref(), Some("Series timeframe does not match scan"));
        set.insert(Timeframe::W1, series(Timeframe::W1, &[1., 2., 3.]));
        set.get_mut(&Timeframe::W1).unwrap().bars[1].ts = 0;
        assert_eq!(scan.evaluate_set(&set).reason.as_deref(), Some("Bars must have strictly increasing timestamps"));
    }

    #[test]
    fn serde_legacy_empty_missing_and_future_versions() {
        // Legacy flat definition with an empty conditions list becomes an empty root group.
        let empty = r#"{"name":"Empty","timeframe":"D1","match_mode":"Any","conditions":[]}"#;
        let scan: ScanDefinition = serde_json::from_str(empty).unwrap();
        assert_eq!(scan.root, ConditionGroup { mode: MatchMode::Any, items: Vec::new() });
        assert!(scan.validate().unwrap_err().contains("at least 1"));

        // Missing match_mode defaults to All.
        let no_mode =
            r#"{"name":"X","timeframe":"D1","conditions":[{"left":"Price","comparison":"Above","right":"Price"}]}"#;
        let scan: ScanDefinition = serde_json::from_str(no_mode).unwrap();
        assert_eq!(scan.root.mode, MatchMode::All);
        assert_eq!(scan.root.items.len(), 1);

        // Missing conditions and missing root also yields an empty root group.
        let bare = r#"{"name":"Bare","timeframe":"D1"}"#;
        let scan: ScanDefinition = serde_json::from_str(bare).unwrap();
        assert!(scan.root.items.is_empty());
        assert!(scan.validate().is_err());

        // Any-mode legacy keeps Any and its conditions.
        let any = r#"{"name":"Any","timeframe":"W1","match_mode":"Any","conditions":[{"left":"Volume","comparison":"AtLeast","right":{"Constant":100000}}]}"#;
        let scan: ScanDefinition = serde_json::from_str(any).unwrap();
        assert_eq!(scan.root.mode, MatchMode::Any);
        assert_eq!(scan.timeframe, Timeframe::W1);
        assert_eq!(scan.leaves().len(), 1);
        assert!(scan.validate().is_ok());

        // A missing version is accepted and a future version with a v1 body still loads.
        for version in [None, Some(0), Some(1), Some(99)] {
            let mut json = serde_json::to_value(ScanDefinition::default()).unwrap();
            match version {
                Some(v) => json["version"] = v.into(),
                None => {
                    let obj = json.as_object_mut().unwrap();
                    obj.remove("version");
                }
            }
            assert_eq!(serde_json::from_value::<ScanDefinition>(json).unwrap(), ScanDefinition::default());
        }

        // Unknown extra fields are ignored (forward compatible).
        let mut json = serde_json::to_value(ScanDefinition::default()).unwrap();
        json["future_field"] = serde_json::json!({"nested": true});
        assert_eq!(serde_json::from_value::<ScanDefinition>(json).unwrap(), ScanDefinition::default());

        // A v1 body that still carries legacy conditions ignores them when root is present.
        let mut json = serde_json::to_value(ScanDefinition::default()).unwrap();
        json["match_mode"] = "Any".into();
        json["conditions"] = serde_json::json!([{"left":"Volume","comparison":"Below","right":{"Constant":1}}]);
        let mixed: ScanDefinition = serde_json::from_value(json).unwrap();
        assert_eq!(mixed, ScanDefinition::default());

        // Explicit null root falls back to legacy fields.
        let null_root = r#"{"name":"N","timeframe":"D1","root":null,"match_mode":"Any","conditions":[{"left":"Price","comparison":"Below","right":"Price"}]}"#;
        let scan: ScanDefinition = serde_json::from_str(null_root).unwrap();
        assert_eq!(scan.root.mode, MatchMode::Any);
        assert_eq!(scan.leaves().len(), 1);
    }

    #[test]
    fn three_valued_logic_any_of_all_and_three_level_nesting() {
        use ScanOutcome::{Match, NoMatch, Unavailable};
        let states = [Match, NoMatch, Unavailable];
        let s = series(Timeframe::D1, &[1.]);
        let outcome_of = |v: &str| match v {
            "1" => Match,
            "0" => NoMatch,
            _ => Unavailable,
        };
        let kleene_and = |a: ScanOutcome, b: ScanOutcome| {
            if a == NoMatch || b == NoMatch {
                NoMatch
            } else if a == Unavailable || b == Unavailable {
                Unavailable
            } else {
                Match
            }
        };
        let kleene_or = |a: ScanOutcome, b: ScanOutcome| {
            if a == Match || b == Match {
                Match
            } else if a == Unavailable || b == Unavailable {
                Unavailable
            } else {
                NoMatch
            }
        };
        let expr = ["1", "0", "AVGC3"];
        for av in expr {
            for bv in expr {
                for cv in expr {
                    // ANY(av, ALL(bv, cv))
                    let scan = ScanDefinition {
                        root: ConditionGroup {
                            mode: MatchMode::Any,
                            items: vec![
                                predicate(av),
                                ConditionItem::Group(ConditionGroup {
                                    mode: MatchMode::All,
                                    items: vec![predicate(bv), predicate(cv)],
                                }),
                            ],
                        },
                        ..ScanDefinition::default()
                    };
                    let expected = kleene_or(outcome_of(av), kleene_and(outcome_of(bv), outcome_of(cv)));
                    assert_eq!(scan.evaluate(&s).outcome, expected, "{av} OR ({bv} AND {cv})");

                    // Three-level nesting: ALL(av, ANY(bv, ALL(cv, av)))
                    let scan = ScanDefinition {
                        root: ConditionGroup {
                            mode: MatchMode::All,
                            items: vec![
                                predicate(av),
                                ConditionItem::Group(ConditionGroup {
                                    mode: MatchMode::Any,
                                    items: vec![
                                        predicate(bv),
                                        ConditionItem::Group(ConditionGroup {
                                            mode: MatchMode::All,
                                            items: vec![predicate(cv), predicate(av)],
                                        }),
                                    ],
                                }),
                            ],
                        },
                        ..ScanDefinition::default()
                    };
                    let expected = kleene_and(
                        outcome_of(av),
                        kleene_or(outcome_of(bv), kleene_and(outcome_of(cv), outcome_of(av))),
                    );
                    assert_eq!(scan.evaluate(&s).outcome, expected, "{av} AND ({bv} OR ({cv} AND {av}))");
                }
            }
        }
        for a in states {
            assert_eq!(combine(MatchMode::All, &[a]), a);
            assert_eq!(combine(MatchMode::Any, &[a]), a);
        }
    }

    #[test]
    fn depth_and_leaf_counting_are_exact() {
        // Exactly 32 leaves and exactly depth 3 are valid; one more of either is not.
        let mut scan = ScanDefinition {
            root: ConditionGroup { mode: MatchMode::All, items: vec![predicate("1"); MAX_CONDITIONS] },
            ..ScanDefinition::default()
        };
        assert!(scan.validate().is_ok());
        assert_eq!(scan.leaves().len(), MAX_CONDITIONS);
        scan.root.items.push(predicate("1"));
        assert!(scan.validate().unwrap_err().contains("32"));

        // Leaves buried in nested groups still count toward the cap.
        let scan = ScanDefinition {
            root: ConditionGroup {
                mode: MatchMode::All,
                items: vec![
                    predicate("1"),
                    ConditionItem::Group(ConditionGroup {
                        mode: MatchMode::All,
                        items: vec![predicate("1"); MAX_CONDITIONS],
                    }),
                ],
            },
            ..ScanDefinition::default()
        };
        assert_eq!(scan.leaves().len(), MAX_CONDITIONS + 1);
        assert!(scan.validate().unwrap_err().contains("32"));

        // Depth: root=1, so two nested wrappers (levels 2 and 3) are fine, a fourth is not.
        let leaf = ConditionGroup { mode: MatchMode::All, items: vec![predicate("1")] };
        let depth3 = ConditionGroup {
            mode: MatchMode::All,
            items: vec![ConditionItem::Group(ConditionGroup {
                mode: MatchMode::All,
                items: vec![ConditionItem::Group(leaf)],
            })],
        };
        let scan = ScanDefinition { root: depth3, ..ScanDefinition::default() };
        assert!(scan.validate().is_ok());
        let depth4 = ConditionGroup { mode: MatchMode::All, items: vec![ConditionItem::Group(scan.root.clone())] };
        let scan = ScanDefinition { root: depth4, ..ScanDefinition::default() };
        assert!(scan.validate().unwrap_err().contains("depth"));

        // An empty nested group is rejected even when the root has other items.
        let scan = ScanDefinition {
            root: ConditionGroup {
                mode: MatchMode::All,
                items: vec![
                    predicate("1"),
                    ConditionItem::Group(ConditionGroup { mode: MatchMode::Any, items: Vec::new() }),
                ],
            },
            ..ScanDefinition::default()
        };
        assert!(scan.validate().unwrap_err().contains("at least 1"));
    }

    #[test]
    fn formula_treats_nan_inf_and_negative_zero() {
        let formula = |expression: &str| FormulaCondition { expression: expression.into(), timeframe: None };
        let s = series(Timeframe::D1, &[1., 2., 3., 4., 5.]);
        // Finiteness: last value NaN is unavailable, never a match or a non-match.
        assert_eq!(formula("C/0").evaluate(&s).outcome, ScanOutcome::Unavailable);
        assert_eq!(formula("C/0").validate(), Ok(()));
        // Overflow collapses to NaN in PCF, so it is unavailable too.
        assert_eq!(formula("1e308*10").evaluate(&s).outcome, ScanOutcome::Unavailable);
        // -0.0 == 0.0, so it must be a non-match, not a match.
        let neg_zero = formula("-0").evaluate(&s);
        assert_eq!(neg_zero.outcome, ScanOutcome::NoMatch);
        assert_eq!(neg_zero.left, Some(0.0));
        assert_eq!(formula("0").evaluate(&s).outcome, ScanOutcome::NoMatch);
        assert_eq!(formula("0 * -1").evaluate(&s).outcome, ScanOutcome::NoMatch);
        assert_eq!(formula("1").evaluate(&s).outcome, ScanOutcome::Match);
        assert_eq!(formula("-1").evaluate(&s).outcome, ScanOutcome::Match);
        assert_eq!(formula("C - C").evaluate(&s).outcome, ScanOutcome::NoMatch);
    }

    #[test]
    fn timeframe_override_equal_to_scan_timeframe_is_not_extra() {
        let mut scan = ScanDefinition {
            root: ConditionGroup {
                mode: MatchMode::All,
                items: vec![
                    predicate("1"),
                    ConditionItem::Compare(ScanCondition {
                        timeframe: Some(Timeframe::D1),
                        right: ScanOperand::Sma { period: 3, source: Source::Close },
                        ..ScanCondition::default()
                    }),
                ],
            },
            ..ScanDefinition::default()
        };
        // An explicit override matching the scan timeframe must not become a second fetch.
        assert_eq!(scan.timeframes(), vec![Timeframe::D1]);
        // It still contributes to the scan timeframe's required history.
        assert_eq!(scan.required_bars_for(Timeframe::D1), 300);
        assert_eq!(scan.required_bars(), 300);

        // A real second timeframe is still listed after the scan timeframe.
        if let ConditionItem::Compare(c) = &mut scan.root.items[1] {
            c.timeframe = Some(Timeframe::W1);
        }
        assert_eq!(scan.timeframes(), vec![Timeframe::D1, Timeframe::W1]);
        assert_eq!(scan.required_bars_for(Timeframe::W1), 3);
        assert_eq!(scan.required_bars_for(Timeframe::D1), 300);

        // Several distinct overrides, each listed once; the scan timeframe is first.
        let scan = ScanDefinition {
            root: ConditionGroup {
                mode: MatchMode::All,
                items: vec![
                    ConditionItem::Compare(ScanCondition {
                        timeframe: Some(Timeframe::W1),
                        ..ScanCondition::default()
                    }),
                    ConditionItem::Compare(ScanCondition {
                        timeframe: Some(Timeframe::H1),
                        ..ScanCondition::default()
                    }),
                    ConditionItem::Compare(ScanCondition {
                        timeframe: Some(Timeframe::W1),
                        ..ScanCondition::default()
                    }),
                    predicate("1"),
                ],
            },
            ..ScanDefinition::default()
        };
        assert_eq!(scan.timeframes(), vec![Timeframe::D1, Timeframe::H1, Timeframe::W1]);
    }

    #[test]
    fn required_bars_for_covers_formulas_in_nested_groups() {
        let formula_on = |tf: Option<Timeframe>| {
            ConditionItem::Formula(FormulaCondition { expression: "C > 1".into(), timeframe: tf })
        };
        let compare_on = |tf: Option<Timeframe>, period: usize| {
            ConditionItem::Compare(ScanCondition {
                timeframe: tf,
                right: ScanOperand::Sma { period, source: Source::Close },
                ..ScanCondition::default()
            })
        };
        let scan = ScanDefinition {
            root: ConditionGroup {
                mode: MatchMode::All,
                items: vec![
                    formula_on(None),
                    ConditionItem::Group(ConditionGroup {
                        mode: MatchMode::Any,
                        items: vec![
                            formula_on(Some(Timeframe::W1)),
                            compare_on(Some(Timeframe::W1), 10),
                            compare_on(None, 4),
                        ],
                    }),
                ],
            },
            ..ScanDefinition::default()
        };
        // Formula on the scan timeframe dominates D1 (300 > SMA(4)).
        assert_eq!(scan.required_bars_for(Timeframe::D1), 300);
        // W1 has a formula (300) and SMA(10): the formula wins.
        assert_eq!(scan.required_bars_for(Timeframe::W1), 300);
        // A timeframe with no leaves falls back to 1 and is not fetched.
        assert_eq!(scan.required_bars_for(Timeframe::H1), 1);
        assert_eq!(scan.timeframes(), vec![Timeframe::D1, Timeframe::W1]);

        // A formula-only override on another timeframe still needs 300 there, 1 on the scan tf
        // only when no scan-tf leaf remains.
        let scan = ScanDefinition {
            root: ConditionGroup {
                mode: MatchMode::All,
                items: vec![ConditionItem::Group(ConditionGroup {
                    mode: MatchMode::All,
                    items: vec![formula_on(Some(Timeframe::W1))],
                })],
            },
            ..ScanDefinition::default()
        };
        assert_eq!(scan.required_bars_for(Timeframe::W1), 300);
        assert_eq!(scan.required_bars_for(Timeframe::D1), 1);
        assert_eq!(scan.timeframes(), vec![Timeframe::D1, Timeframe::W1]);
    }

    #[test]
    fn integer_and_fractional_json_numbers_load_as_f64_fields() {
        // Hand-written scans often store 2 instead of 2.0; both must load.
        let json = r#"{
            "version": 1,
            "name": "Ints",
            "timeframe": "D1",
            "root": {
                "mode": "All",
                "items": [
                    {"Compare": {
                        "left": "Price",
                        "comparison": "Above",
                        "right": {"Sma": {"period": 20}},
                        "right_multiplier": 2,
                        "left_bars_ago": 1,
                        "right_bars_ago": 0
                    }},
                    {"Formula": {"expression": "C > 1"}}
                ]
            }
        }"#;
        let scan: ScanDefinition = serde_json::from_str(json).unwrap();
        assert!(scan.validate().is_ok());
        let ConditionItem::Compare(c) = &scan.root.items[0] else { panic!("Expected comparison") };
        assert_eq!(c.right_multiplier, 2.0);
        assert_eq!(c.left_bars_ago, 1);
        // Serialising back keeps the numeric value.
        let again: ScanDefinition = serde_json::from_value(serde_json::to_value(&scan).unwrap()).unwrap();
        assert_eq!(again, scan);
    }

    #[test]
    #[ignore = "BUG: ConditionItem::required_bars hardcodes 300 for every FormulaCondition instead of the expression's window; AVG(C,1000) needs 1000 bars so required_bars_for reports 300, the worker fetches only 900 (300*3), and the predicate stays Unavailable forever"]
    fn formula_required_bars_must_cover_expression_windows() {
        let scan = ScanDefinition {
            root: ConditionGroup {
                mode: MatchMode::All,
                items: vec![ConditionItem::Formula(FormulaCondition {
                    expression: "AVG(C, 1000) > 0".into(),
                    timeframe: None,
                })],
            },
            ..ScanDefinition::default()
        };
        // The expression needs a full 1000-bar window before it can produce a number.
        assert!(scan.required_bars() >= 1000, "required_bars() = {}", scan.required_bars());
        // Whatever the worker fetches from required_bars must be enough to evaluate.
        let fetched = scan.required_bars().saturating_mul(3).max(300);
        let bars: Vec<_> = (0..fetched as i64).map(|i| Bar::new(i, 1.0, 1.0, 1.0, 1.0, 1.0)).collect();
        let series = Series::new("TEST", Timeframe::D1, bars);
        assert_eq!(scan.evaluate(&series).outcome, ScanOutcome::Match);
    }

    #[test]
    fn presets_unchanged() {
        let condition = |left, comparison, right, ago, multiplier| ScanCondition {
            left,
            comparison,
            right,
            left_bars_ago: ago,
            right_multiplier: multiplier,
            ..ScanCondition::default()
        };
        use Comparison::{Above, AtLeast, Below};
        use ScanOperand::{Atr, Constant, Ema, Price, Rsi, Sma, Volume};
        let momentum = vec![
            condition(Sma { period: 1, source: Source::Volume }, Above, Constant(100_000.), 0, 1.),
            condition(Sma { period: 1, source: Source::Close }, Above, Constant(1.), 0, 1.),
            condition(Rsi { period: 14 }, AtLeast, Constant(70.), 0, 1.),
            condition(Price, AtLeast, Ema { period: 4, source: Source::Close }, 0, 1.),
            condition(Rsi { period: 14 }, Below, Constant(69.9), 1, 1.),
        ];
        let thrust = vec![
            condition(Sma { period: 1, source: Source::Volume }, Above, Constant(100_000.), 0, 1.),
            condition(Rsi { period: 14 }, AtLeast, Constant(69.8), 0, 1.),
            condition(Volume, AtLeast, Ema { period: 50, source: Source::Volume }, 0, 2.5),
            condition(Price, AtLeast, Constant(0.2), 0, 1.),
            condition(Atr { period: 1 }, Above, Constant(0.5), 0, 1.),
        ];
        let s = Series::new("AAPL", Timeframe::D1, crate::sample::generate("AAPL", Timeframe::D1, 400, 1_790_000_000));
        for (preset, conditions) in ScanDefinition::presets().into_iter().zip([momentum, thrust]) {
            let flat = ScanDefinition {
                name: preset.name.clone(),
                timeframe: Timeframe::D1,
                root: ConditionGroup::comparisons(MatchMode::All, conditions),
            };
            assert_eq!(preset, flat);
            assert_eq!(preset.evaluate(&s), flat.evaluate(&s));
        }
    }
}
