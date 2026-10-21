//! Saved scan/watchlist traversal and chart-side scan explanations.
use serde::{Deserialize, Serialize};
use tr_core::scanner::{Comparison, MatchMode, ScanDefinition, ScanOperand, Source};

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
    let terms: Vec<_> = scan
        .conditions
        .iter()
        .map(|c| {
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
        })
        .collect();
    Ok(terms.join(if scan.match_mode == MatchMode::All { " AND " } else { " OR " }))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        scan.conditions[0].right_multiplier = 2.;
        scan.conditions[0].left_bars_ago = 3;
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
