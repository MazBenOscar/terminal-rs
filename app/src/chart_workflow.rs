//! Saved scan/watchlist traversal and chart-side scan explanations.
use serde::{Deserialize, Serialize};
use tr_core::scanner::{Comparison, ConditionGroup, ConditionItem, MatchMode, ScanDefinition, ScanOperand, Source};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workflow {
    #[serde(deserialize_with = "bounded_symbols")]
    pub symbols: Vec<String>,
    pub scan: Option<ScanDefinition>,
}
fn bounded_symbols<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let mut symbols = Vec::<String>::deserialize(deserializer)?;
    symbols.truncate(5_000);
    Ok(symbols)
}
pub fn next(symbols: &[String], current: &str, delta: isize) -> Option<String> {
    if symbols.is_empty() {
        return None;
    }
    let index = symbols.iter().position(|s| s == current).map(|i| i as isize).unwrap_or(if delta > 0 { -1 } else { 0 });
    Some(symbols[(index + delta).rem_euclid(symbols.len() as isize) as usize].clone())
}
pub fn operand(operand: ScanOperand) -> String {
    match operand {
        ScanOperand::Price => "C".into(),
        ScanOperand::Volume => "V".into(),
        ScanOperand::ChangePercent => "100*(C/C1-1)".into(),
        ScanOperand::Constant(n) => n.to_string(),
        ScanOperand::Sma { period, source } => {
            format!("AVG({},{period})", if source == Source::Close { "C" } else { "V" })
        }
        ScanOperand::Ema { period, source } => {
            format!("XAVG({},{period})", if source == Source::Close { "C" } else { "V" })
        }
        ScanOperand::Atr { period } => format!("WATR({period})"),
        ScanOperand::Rsi { period } => format!("WRSI({period})"),
    }
}
pub fn expression(scan: &ScanDefinition) -> Result<String, String> {
    scan.validate()?;
    if scan.leaves().iter().any(|c| c.timeframe().is_some_and(|tf| tf != scan.timeframe)) {
        return Err("Conditions with their own timeframe can't be plotted yet".into());
    }
    Ok(group_expression(&scan.root))
}

fn group_expression(group: &ConditionGroup) -> String {
    let terms: Vec<_> = group
        .items
        .iter()
        .map(|item| match item {
            ConditionItem::Group(group) => format!("({})", group_expression(group)),
            ConditionItem::Formula(c) => format!("({})", c.expression),
            ConditionItem::Compare(c) => {
                let left = format!("REF(({}),{})", operand(c.left), c.left_bars_ago);
                let right = format!("REF(({}),{})*{}", operand(c.right), c.right_bars_ago, c.right_multiplier);
                match c.comparison {
                    Comparison::CrossedAbove => format!("CROSS(({left}),({right}))"),
                    Comparison::CrossedBelow => format!("CROSS(({right}),({left}))"),
                    other => format!(
                        "(({left}) {} ({right}))",
                        match other {
                            Comparison::Above => ">",
                            Comparison::Below => "<",
                            Comparison::AtLeast => ">=",
                            Comparison::AtMost => "<=",
                            _ => unreachable!(),
                        }
                    ),
                }
            }
        })
        .collect();
    terms.join(if group.mode == MatchMode::All { " AND " } else { " OR " })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_group_expression_has_parens() {
        use tr_core::scanner::FormulaCondition;
        let formula = |expression: &str| {
            ConditionItem::Formula(FormulaCondition { expression: expression.into(), timeframe: None })
        };
        let scan = ScanDefinition {
            root: ConditionGroup {
                mode: MatchMode::All,
                items: vec![
                    formula("C > 1"),
                    ConditionItem::Group(ConditionGroup {
                        mode: MatchMode::Any,
                        items: vec![formula("V > 2"), formula("C < 3")],
                    }),
                ],
            },
            ..ScanDefinition::default()
        };
        assert_eq!(expression(&scan).unwrap(), "(C > 1) AND ((V > 2) OR (C < 3))");
    }

    #[test]
    fn timeframe_override_cannot_be_plotted() {
        use tr_core::{
            Timeframe,
            scanner::{FormulaCondition, ScanCondition},
        };
        let mut scan = ScanDefinition::default();
        scan.root.items = vec![ConditionItem::Group(ConditionGroup {
            mode: MatchMode::All,
            items: vec![ConditionItem::Compare(ScanCondition {
                timeframe: Some(Timeframe::W1),
                ..ScanCondition::default()
            })],
        })];
        assert_eq!(expression(&scan).unwrap_err(), "Conditions with their own timeframe can't be plotted yet");
        scan.root.items = vec![ConditionItem::Formula(FormulaCondition {
            expression: "C > 1".into(),
            timeframe: Some(Timeframe::H1),
        })];
        assert_eq!(expression(&scan).unwrap_err(), "Conditions with their own timeframe can't be plotted yet");
        scan.root.items = vec![ConditionItem::Formula(FormulaCondition {
            expression: "C > 1".into(),
            timeframe: Some(Timeframe::D1),
        })];
        assert_eq!(expression(&scan).unwrap(), "(C > 1)");
    }

    #[test]
    fn restored_queue_is_capped() {
        let json = serde_json::json!({"symbols": (0..5_010).map(|i| i.to_string()).collect::<Vec<_>>()});
        let workflow: Workflow = serde_json::from_value(json).unwrap();
        assert_eq!(workflow.symbols.len(), 5_000);
        assert_eq!(workflow.symbols.last().unwrap(), "4999");
        assert!(serde_json::from_str::<Workflow>("{}").unwrap().symbols.is_empty());
    }
    #[test]
    fn traversal_wraps_and_expressions_preserve_offsets_and_scaling() {
        let symbols = vec!["A".into(), "B".into()];
        assert_eq!(next(&symbols, "B", 1), Some("A".into()));
        assert_eq!(next(&symbols, "A", -1), Some("B".into()));
        assert_eq!(next(&symbols, "missing", 1), Some("A".into()));
        assert!(next(&[], "A", 1).is_none());
        let mut scan = ScanDefinition::default();
        let ConditionItem::Compare(c) = &mut scan.root.items[0] else { panic!("Expected comparison") };
        c.right_multiplier = 2.;
        c.left_bars_ago = 3;
        let s = expression(&scan).unwrap();
        assert!(s.contains("REF((C),3)"));
        assert!(s.contains("*2"));
    }
    #[test]
    fn generated_scan_plots_agree_with_warm_finite_preset_evaluations() {
        let bars = tr_core::sample::generate("AAPL", tr_core::Timeframe::D1, 400, 1_790_000_000);
        let series = tr_core::Series::new("AAPL", tr_core::Timeframe::D1, bars.clone());
        for scan in ScanDefinition::presets() {
            let formula = expression(&scan).unwrap();
            let values = tr_core::indicators::pcf::evaluate(
                &formula,
                &tr_core::indicators::registry::Input::new(&bars, scan.timeframe),
            )
            .unwrap();
            match scan.evaluate(&series).outcome {
                tr_core::scanner::ScanOutcome::Match => assert_eq!(values.last(), Some(&1.)),
                tr_core::scanner::ScanOutcome::NoMatch => assert_eq!(values.last(), Some(&0.)),
                _ => {}
            }
        }
    }
}
