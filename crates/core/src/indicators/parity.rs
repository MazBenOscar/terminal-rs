//! Reference comparison on identical OHLCV inputs. Reference provenance is mandatory.
use super::{
    registry::{Input, Spec},
    study,
};
use crate::{Bar, Timeframe, session::Session};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct Fixture {
    pub source: String,
    pub symbol: String,
    pub adjustments: String,
    pub session_description: String,
    pub exported_at: String,
    pub timeframe: Timeframe,
    #[serde(default)]
    pub session: Session,
    pub bars: Vec<Bar>,
    pub studies: Vec<Spec>,
    pub expected: Vec<Expected>,
    pub absolute_tolerance: f64,
    pub relative_tolerance: f64,
}
#[derive(Debug, Deserialize)]
pub struct Expected {
    pub study: u32,
    pub output: String,
    pub values: Vec<Option<f64>>,
}
#[derive(Debug, Serialize)]
pub struct Difference {
    pub study: u32,
    pub output: String,
    pub timestamp: i64,
    pub expected: Option<f64>,
    pub actual: Option<f64>,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub source: String,
    pub compared: usize,
    pub finite_pairs: usize,
    pub mismatches: usize,
    pub first_differences: Vec<Difference>,
}
pub fn compare(fixture: &mut Fixture) -> Result<Report, String> {
    if [&fixture.source, &fixture.symbol, &fixture.adjustments, &fixture.session_description, &fixture.exported_at]
        .iter()
        .any(|s| s.trim().is_empty())
    {
        return Err("Reference source, symbol, adjustments, session and export date are required".into());
    }
    if fixture.bars.is_empty() || fixture.expected.is_empty() {
        return Err("Reference bars and expected outputs must not be empty".into());
    }
    if fixture.bars.windows(2).any(|w| w[0].ts >= w[1].ts) {
        return Err("Reference timestamps must be unique and increasing".into());
    }
    if fixture.bars.iter().any(|b| ![b.open, b.high, b.low, b.close, b.volume].iter().all(|v| v.is_finite())) {
        return Err("Reference OHLCV values must be finite".into());
    }
    if [fixture.absolute_tolerance, fixture.relative_tolerance].iter().any(|n| !n.is_finite() || *n < 0.) {
        return Err("Tolerances must be finite and nonnegative".into());
    }
    study::assign_ids(&mut fixture.studies);
    let calculated =
        study::evaluate_with_session(&fixture.studies, &Input::new(&fixture.bars, fixture.timeframe), fixture.session);
    let mut report = Report {
        source: fixture.source.clone(),
        compared: 0,
        finite_pairs: 0,
        mismatches: 0,
        first_differences: Vec::new(),
    };
    for expected in &fixture.expected {
        let i = fixture
            .studies
            .iter()
            .position(|s| s.chart.as_ref().is_some_and(|c| c.id == expected.study))
            .ok_or("Unknown reference study ID")?;
        let outputs = calculated[i].as_ref().map_err(|e| format!("Study {}: {e}", expected.study))?;
        let output = outputs.iter().find(|o| o.name == expected.output).ok_or("Unknown reference output name")?;
        if output.values.len() != fixture.bars.len() || expected.values.len() != fixture.bars.len() {
            return Err("Every expected output must align with exactly the supplied bars".into());
        }
        for (j, (&wanted, &actual)) in expected.values.iter().zip(&output.values).enumerate() {
            report.compared += 1;
            let actual = actual.is_finite().then_some(actual);
            let matches = match (wanted, actual) {
                (Some(a), Some(b)) => {
                    report.finite_pairs += 1;
                    (a - b).abs() <= fixture.absolute_tolerance + fixture.relative_tolerance * a.abs()
                }
                (None, None) => true,
                _ => false,
            };
            if !matches {
                report.mismatches += 1;
                if report.first_differences.len() < 20 {
                    report.first_differences.push(Difference {
                        study: expected.study,
                        output: expected.output.clone(),
                        timestamp: fixture.bars[j].ts,
                        expected: wanted,
                        actual,
                    });
                }
            }
        }
    }
    if report.finite_pairs == 0 {
        return Err("Reference comparison has no finite value pairs".into());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Fixture {
        serde_json::from_str(include_str!("../../tests/fixtures/manual-chart-reference.json")).unwrap()
    }
    #[test]
    fn manual_fixture_matches_and_numeric_or_warmup_regressions_are_reported() {
        let mut f = fixture();
        let r = compare(&mut f).unwrap();
        assert_eq!(r.mismatches, 0);
        assert!(r.finite_pairs > 0);
        f.expected[0].values[1] = Some(123.);
        f.expected[0].values[0] = Some(0.);
        let r = compare(&mut f).unwrap();
        assert_eq!(r.mismatches, 2);
        assert_eq!(r.first_differences[0].timestamp, f.bars[0].ts);
    }
    #[test]
    fn missing_provenance_and_misaligned_reference_data_are_rejected() {
        let mut f = fixture();
        f.source.clear();
        assert!(compare(&mut f).is_err());
        let mut f = fixture();
        f.expected[0].values.pop();
        assert!(compare(&mut f).is_err());
    }
}
