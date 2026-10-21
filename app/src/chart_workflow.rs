//! Saved scan/watchlist traversal and chart-side scan explanations.
use serde::{Deserialize, Serialize};
use tr_core::scanner::{Comparison, ConditionGroup, ConditionItem, MatchMode, ScanDefinition, ScanOperand, Source};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workflow {
    #[serde(deserialize_with = "bounded_symbols")]
    pub symbols: Vec<String>,
    pub scan: Option<ScanDefinition>,
    pub plot_conditions: bool,
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
        ScanOperand::Volume => "IF(V>=0,V,0/0)".into(),
        ScanOperand::ChangePercent => "100*(C/IF(C1>0,C1,0)-1)".into(),
        ScanOperand::Constant(n) => n.to_string(),
        ScanOperand::Sma { period, source } => {
            // Check the entire current window, not just its latest volume.
            if source == Source::Close {
                format!("AVG(C,{period})")
            } else {
                format!("IF(MIN(V,{period})>=0,AVG(V,{period}),0/0)")
            }
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
    fn legacy_workflow_defaults_plot_conditions_to_false_and_round_trips() {
        let old = r#"{"symbols":["AAPL US Equity"],"scan":null}"#;
        let mut workflow: Workflow = serde_json::from_str(old).unwrap();
        assert!(!workflow.plot_conditions);
        assert!(!serde_json::from_str::<Workflow>("{}").unwrap().plot_conditions);
        workflow.plot_conditions = true;
        let saved = serde_json::to_string(&workflow).unwrap();
        assert_eq!(serde_json::from_str::<Workflow>(&saved).unwrap(), workflow);
    }

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
    fn pcf_parenthesises_mixed_and_or_nesting() {
        use tr_core::scanner::FormulaCondition;
        let formula = |expression: &str| {
            ConditionItem::Formula(FormulaCondition { expression: expression.into(), timeframe: None })
        };
        // ANY(ALL(a,b), ALL(c,d)) must not collapse into a flat OR of four terms.
        let scan = ScanDefinition {
            root: ConditionGroup {
                mode: MatchMode::Any,
                items: vec![
                    ConditionItem::Group(ConditionGroup {
                        mode: MatchMode::All,
                        items: vec![formula("C > 1"), formula("C > 2")],
                    }),
                    ConditionItem::Group(ConditionGroup {
                        mode: MatchMode::All,
                        items: vec![formula("C > 3"), formula("C > 4")],
                    }),
                ],
            },
            ..ScanDefinition::default()
        };
        assert_eq!(expression(&scan).unwrap(), "((C > 1) AND (C > 2)) OR ((C > 3) AND (C > 4))");

        // ALL(ANY(a,b), ANY(c,d))
        let scan = ScanDefinition {
            root: ConditionGroup {
                mode: MatchMode::All,
                items: vec![
                    ConditionItem::Group(ConditionGroup {
                        mode: MatchMode::Any,
                        items: vec![formula("C > 1"), formula("C > 2")],
                    }),
                    ConditionItem::Group(ConditionGroup {
                        mode: MatchMode::Any,
                        items: vec![formula("C > 3"), formula("C > 4")],
                    }),
                ],
            },
            ..ScanDefinition::default()
        };
        assert_eq!(expression(&scan).unwrap(), "((C > 1) OR (C > 2)) AND ((C > 3) OR (C > 4))");

        // Three-level: ALL(a, ANY(b, ALL(c, d)))
        let scan = ScanDefinition {
            root: ConditionGroup {
                mode: MatchMode::All,
                items: vec![
                    formula("C > 1"),
                    ConditionItem::Group(ConditionGroup {
                        mode: MatchMode::Any,
                        items: vec![
                            formula("C > 2"),
                            ConditionItem::Group(ConditionGroup {
                                mode: MatchMode::All,
                                items: vec![formula("C > 3"), formula("C > 4")],
                            }),
                        ],
                    }),
                ],
            },
            ..ScanDefinition::default()
        };
        assert_eq!(expression(&scan).unwrap(), "(C > 1) AND ((C > 2) OR ((C > 3) AND (C > 4)))");

        // A formula that already contains OR is still isolated by its own parens.
        let scan = ScanDefinition {
            root: ConditionGroup { mode: MatchMode::All, items: vec![formula("C > 1 OR C > 2"), formula("C > 3")] },
            ..ScanDefinition::default()
        };
        assert_eq!(expression(&scan).unwrap(), "(C > 1 OR C > 2) AND (C > 3)");
    }

    #[test]
    fn pcf_maps_crosses_bars_ago_and_multipliers() {
        use tr_core::{
            Timeframe,
            scanner::{FormulaCondition, ScanCondition},
        };
        let mut scan = ScanDefinition::default();
        scan.root.items = vec![ConditionItem::Compare(ScanCondition {
            left_bars_ago: 1,
            right_bars_ago: 2,
            right_multiplier: 2.5,
            ..ScanCondition::default()
        })];
        let s = expression(&scan).unwrap();
        assert!(s.contains("REF((C),1)"), "{s}");
        assert!(s.contains("REF((AVG(C,20)),2)*2.5"), "{s}");

        // CrossedAbove maps to CROSS(left, right); CrossedBelow flips the arguments.
        if let ConditionItem::Compare(c) = &mut scan.root.items[0] {
            c.comparison = tr_core::scanner::Comparison::CrossedAbove;
            c.left_bars_ago = 0;
            c.right_bars_ago = 0;
            c.right_multiplier = 1.0;
        }
        let above = expression(&scan).unwrap();
        assert!(
            above.contains("CROSS((REF((C),0)),(REF((AVG(C,20)),0)*1))")
                || above.contains("CROSS((REF((C),0)), (REF((AVG(C,20)),0)*1))"),
            "{above}"
        );
        if let ConditionItem::Compare(c) = &mut scan.root.items[0] {
            c.comparison = tr_core::scanner::Comparison::CrossedBelow;
        }
        let below = expression(&scan).unwrap();
        assert!(
            below.contains("CROSS((REF((AVG(C,20)),0)*1),(REF((C),0)))")
                || below.contains("CROSS((REF((AVG(C,20)),0)*1), (REF((C),0)))"),
            "{below}"
        );

        // Nested group inside a cross-bearing scan still parenthesises.
        scan.root.items.push(ConditionItem::Group(ConditionGroup {
            mode: MatchMode::Any,
            items: vec![
                ConditionItem::Formula(FormulaCondition { expression: "C > 1".into(), timeframe: None }),
                ConditionItem::Formula(FormulaCondition { expression: "C > 2".into(), timeframe: None }),
            ],
        }));
        if let ConditionItem::Compare(c) = &mut scan.root.items[0] {
            c.comparison = tr_core::scanner::Comparison::CrossedAbove;
        }
        let nested = expression(&scan).unwrap();
        assert!(nested.ends_with(" AND ((C > 1) OR (C > 2))"), "{nested}");

        // Same-timeframe overrides are plottable; other timeframes are not.
        scan.root.items = vec![ConditionItem::Formula(FormulaCondition {
            expression: "C > 1".into(),
            timeframe: Some(Timeframe::D1),
        })];
        assert_eq!(expression(&scan).unwrap(), "(C > 1)");
    }

    #[test]
    fn generated_crosses_agree_with_model_evaluation() {
        use tr_core::scanner::{Comparison, ScanCondition, ScanOutcome};
        // Build a short series where each side's cross state is known at the last bar.
        let closes = [1.0, 3.0, 2.0, 4.0, 3.0, 5.0];
        let bars: Vec<_> =
            closes.iter().enumerate().map(|(i, &c)| tr_core::Bar::new(i as i64, c, c, c, c, 1_000.0)).collect();
        let series = tr_core::Series::new("TEST", tr_core::Timeframe::D1, bars.clone());
        for (comparison, left_ago, right_ago, mult) in [
            (Comparison::CrossedAbove, 0, 0, 1.0),
            (Comparison::CrossedBelow, 0, 0, 1.0),
            (Comparison::CrossedAbove, 1, 0, 1.0),
            (Comparison::CrossedBelow, 0, 1, 1.0),
            (Comparison::CrossedAbove, 0, 0, 2.0),
            (Comparison::CrossedBelow, 1, 1, 0.5),
        ] {
            let mut scan = ScanDefinition::default();
            scan.root.items = vec![ConditionItem::Compare(ScanCondition {
                left: tr_core::scanner::ScanOperand::Price,
                comparison,
                right: tr_core::scanner::ScanOperand::Price,
                left_bars_ago: left_ago,
                right_bars_ago: right_ago,
                right_multiplier: mult,
                ..ScanCondition::default()
            })];
            let formula = expression(&scan).unwrap();
            let values = tr_core::indicators::pcf::evaluate(
                &formula,
                &tr_core::indicators::registry::Input::new(&bars, scan.timeframe),
            )
            .unwrap();
            let last = values.last().copied().filter(|v| v.is_finite());
            match scan.evaluate(&series).outcome {
                ScanOutcome::Match => {
                    assert_eq!(last, Some(1.0), "{comparison:?} ago=({left_ago},{right_ago}) x{mult}")
                }
                ScanOutcome::NoMatch => {
                    assert_eq!(last, Some(0.0), "{comparison:?} ago=({left_ago},{right_ago}) x{mult}")
                }
                ScanOutcome::Unavailable => {}
            }
        }
    }

    #[test]
    fn generated_formulas_parse_for_odd_multipliers_and_constants() {
        use tr_core::scanner::{Comparison, ScanCondition, ScanOperand};
        for mult in [0.0, -0.0, -1.5, 2.5, 1e-8, 1e8, 0.1] {
            let mut scan = ScanDefinition::default();
            scan.root.items = vec![ConditionItem::Compare(ScanCondition {
                left: ScanOperand::Price,
                comparison: Comparison::Above,
                right: ScanOperand::Sma { period: 3, source: tr_core::scanner::Source::Close },
                right_multiplier: mult,
                ..ScanCondition::default()
            })];
            let formula = expression(&scan).unwrap();
            let bars: Vec<_> = (0..10)
                .map(|i| tr_core::Bar::new(i, 1.0 + i as f64, 2.0 + i as f64, i as f64, 1.0 + i as f64, 10.0))
                .collect();
            let values = tr_core::indicators::pcf::evaluate(
                &formula,
                &tr_core::indicators::registry::Input::new(&bars, scan.timeframe),
            )
            .expect(&formula);
            assert_eq!(values.len(), bars.len(), "{formula}");
        }
        // Negative and fractional constants stay parseable inside REF and comparisons.
        for constant in [-2.5, 0.0, -0.0, 1e-8] {
            let mut scan = ScanDefinition::default();
            scan.root.items = vec![ConditionItem::Compare(ScanCondition {
                left: ScanOperand::Price,
                comparison: Comparison::Above,
                right: ScanOperand::Constant(constant),
                ..ScanCondition::default()
            })];
            let formula = expression(&scan).unwrap();
            let bars: Vec<_> = (0..5).map(|i| tr_core::Bar::new(i, 1.0, 1.0, 1.0, 1.0, 1.0)).collect();
            tr_core::indicators::pcf::evaluate(
                &formula,
                &tr_core::indicators::registry::Input::new(&bars, scan.timeframe),
            )
            .unwrap_or_else(|e| panic!("{formula}: {e}"));
        }
    }

    #[test]
    fn change_percent_translation_rejects_non_positive_previous_close() {
        use tr_core::scanner::{Comparison, ScanCondition, ScanOperand, ScanOutcome};
        let mut scan = ScanDefinition::default();
        scan.root.items = vec![ConditionItem::Compare(ScanCondition {
            left: ScanOperand::ChangePercent,
            comparison: Comparison::Above,
            right: ScanOperand::Constant(0.0),
            ..ScanCondition::default()
        })];
        // Previous close of the last bar is negative: the model refuses to evaluate.
        let bars: Vec<_> = [10.0, -1.0, 5.0]
            .iter()
            .enumerate()
            .map(|(i, &c)| tr_core::Bar::new(i as i64, c, c, c, c, 1_000.0))
            .collect();
        let series = tr_core::Series::new("TEST", tr_core::Timeframe::D1, bars.clone());
        assert_eq!(scan.evaluate(&series).outcome, ScanOutcome::Unavailable);
        // The plotted PCF must agree and stay undefined rather than answering.
        let formula = expression(&scan).unwrap();
        let values = tr_core::indicators::pcf::evaluate(
            &formula,
            &tr_core::indicators::registry::Input::new(&bars, scan.timeframe),
        )
        .unwrap();
        let last = values.last().copied().filter(|v| v.is_finite());
        assert_eq!(last, None, "PCF produced {last:?} where the scan is unavailable");
    }

    #[test]
    fn translated_operands_preserve_unavailable_inputs() {
        let bars: Vec<_> = [10., 0., 5., -1., 6., 7.]
            .iter()
            .enumerate()
            .map(|(i, &c)| tr_core::Bar::new(i as i64, c, c, c, c, if i == 2 { -1. } else { 10. }))
            .collect();
        let input = tr_core::indicators::registry::Input::new(&bars, tr_core::Timeframe::D1);
        let change = tr_core::indicators::pcf::evaluate(&operand(ScanOperand::ChangePercent), &input).unwrap();
        assert!(change[0].is_nan());
        assert!(change[2].is_nan());
        assert!(change[4].is_nan());
        assert!(change[5].is_finite());
        let volume = tr_core::indicators::pcf::evaluate(&operand(ScanOperand::Volume), &input).unwrap();
        assert!(volume[2].is_nan());
        assert_eq!(volume[3], 10.);
        let average = tr_core::indicators::pcf::evaluate(
            &operand(ScanOperand::Sma { period: 2, source: Source::Volume }),
            &input,
        )
        .unwrap();
        assert!(average[2].is_nan());
        assert!(average[3].is_nan());
        assert_eq!(average[4], 10.);
    }

    #[test]
    fn change_percent_model_matches_pcf_on_valid_prices() {
        use tr_core::scanner::{Comparison, ScanCondition, ScanOperand, ScanOutcome};
        let closes = [10.0, 11.0, 10.5, 12.0, 11.0];
        let bars: Vec<_> =
            closes.iter().enumerate().map(|(i, &c)| tr_core::Bar::new(i as i64, c, c, c, c, 1_000.0)).collect();
        let series = tr_core::Series::new("TEST", tr_core::Timeframe::D1, bars.clone());
        let mut scan = ScanDefinition::default();
        scan.root.items = vec![ConditionItem::Compare(ScanCondition {
            left: ScanOperand::ChangePercent,
            comparison: Comparison::Above,
            right: ScanOperand::Constant(0.0),
            ..ScanCondition::default()
        })];
        let formula = expression(&scan).unwrap();
        let values = tr_core::indicators::pcf::evaluate(
            &formula,
            &tr_core::indicators::registry::Input::new(&bars, scan.timeframe),
        )
        .unwrap();
        let last = values.last().copied().filter(|v| v.is_finite());
        match scan.evaluate(&series).outcome {
            ScanOutcome::Match => assert_eq!(last, Some(1.0)),
            ScanOutcome::NoMatch => assert_eq!(last, Some(0.0)),
            ScanOutcome::Unavailable => {}
        }
        // Positive previous closes are required by both the model and translated PCF.
        // Lock the model side so a negative close can never become a match.
        let mut neg = bars.clone();
        neg[3].close = -1.0;
        neg[3].open = -1.0;
        neg[3].high = -1.0;
        neg[3].low = -1.0;
        let series = tr_core::Series::new("TEST", tr_core::Timeframe::D1, neg.clone());
        // Previous close of the last bar is neg[3] which is negative.
        assert_eq!(scan.evaluate(&series).outcome, ScanOutcome::Unavailable);
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
