//! Higher-timeframe studies derived from available chart bars, aligned on completion.
use super::{
    registry::{Input, Output, Spec},
    study::{self, Source},
};
use crate::{Bar, Timeframe, session::Session};
use std::collections::{HashMap, HashSet};

type Results = Vec<Result<Vec<Output>, String>>;
fn timeframe(spec: &Spec, base: Timeframe) -> Timeframe {
    spec.chart.as_ref().and_then(|c| c.timeframe).unwrap_or(base)
}

pub(super) fn evaluate(specs: &[Spec], input: &Input<'_>, session: Session) -> Results {
    if !specs.iter().any(|s| timeframe(s, input.tf) != input.tf) {
        return study::evaluate_native(specs, input);
    }
    let base = study::evaluate_native(specs, input);
    let mut cache: HashMap<Timeframe, (Results, Vec<i64>)> = HashMap::new();
    specs
        .iter()
        .enumerate()
        .map(|(i, spec)| {
            let tf = timeframe(spec, input.tf);
            let mut at = i;
            let mut seen = HashSet::new();
            while seen.insert(at) {
                if timeframe(&specs[at], input.tf) != tf {
                    return Err("Source and child must use the same study timeframe".into());
                }
                match specs[at].chart.as_ref().map(|c| &c.source) {
                    Some(Source::Output { study, .. }) => {
                        let Some(parent) = specs.iter().position(|s| s.chart.as_ref().is_some_and(|c| c.id == *study))
                        else {
                            break;
                        };
                        at = parent;
                    }
                    _ => break,
                }
            }
            if tf == input.tf {
                return base[i].clone();
            }
            if tf < input.tf {
                return Err("A study timeframe must be at least the chart timeframe".into());
            }
            let (results, ends) = cache.entry(tf).or_insert_with(|| {
                let aggregation = if input.tf.is_intraday() { session } else { Session::Provider };
                let (bars, ends) = aggregate(input.bars, tf, aggregation);
                (study::evaluate_native(specs, &Input::new(&bars, tf).with_session(session)), ends)
            });
            let mut outputs = results[i].clone()?;
            for output in &mut outputs {
                if output.values.len() != ends.len() {
                    return Err("Intrinsically displaced outputs cannot use a different timeframe".into());
                }
                output.values = input
                    .bars
                    .iter()
                    .map(|bar| {
                        ends.partition_point(|end| *end <= bar.ts).checked_sub(1).map_or(f64::NAN, |j| output.values[j])
                    })
                    .collect();
            }
            Ok(outputs)
        })
        .collect()
}

fn aggregate(bars: &[Bar], tf: Timeframe, session: Session) -> (Vec<Bar>, Vec<i64>) {
    let mut out: Vec<Bar> = Vec::new();
    let mut ends = Vec::new();
    for bar in bars.iter().filter(|b| session.contains(b.ts)) {
        let (start, end) = session.interval(tf, bar.ts);
        if let Some(last) = out.last_mut()
            && last.ts == start
        {
            last.high = last.high.max(bar.high);
            last.low = last.low.min(bar.low);
            last.close = bar.close;
            last.volume += bar.volume;
        } else {
            out.push(Bar { ts: start, ..*bar });
            ends.push(end);
        }
    }
    // The first bucket may start before loaded history; omit it conservatively.
    if !out.is_empty() {
        out.remove(0);
        ends.remove(0);
    }
    (out, ends)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn higher_timeframe_values_never_appear_before_the_bucket_closes() {
        let bars: Vec<_> = (0..20).map(|i| Bar::new(i * 60, i as f64, i as f64, i as f64, i as f64, 1.)).collect();
        let mut specs = vec![Spec::new("SMA", &[1.]).unwrap()];
        study::assign_ids(&mut specs);
        specs[0].chart.as_mut().unwrap().timeframe = Some(Timeframe::M5);
        let full = evaluate(&specs, &Input::new(&bars, Timeframe::M1), Session::Provider)[0].clone().unwrap();
        assert!(full[0].values[..10].iter().all(|v| v.is_nan()));
        assert_eq!(full[0].values[10], 9.);
        assert_eq!(full[0].values[15], 14.);
        for n in 1..bars.len() {
            let prefix =
                evaluate(&specs, &Input::new(&bars[..n], Timeframe::M1), Session::Provider)[0].clone().unwrap();
            for (a, b) in prefix[0].values.iter().zip(&full[0].values) {
                assert!(a == b || a.is_nan() && b.is_nan());
            }
        }
    }
    #[test]
    fn cross_timeframe_dependencies_are_explicit_errors() {
        let mut specs = vec![Spec::new("RSI", &[]).unwrap(), Spec::new("EMA", &[]).unwrap()];
        study::assign_ids(&mut specs);
        specs[0].chart.as_mut().unwrap().timeframe = Some(Timeframe::D1);
        specs[1].chart.as_mut().unwrap().source = Source::Output { study: 1, output: "RSI".into() };
        assert!(
            evaluate(&specs, &Input::new(&[], Timeframe::H1), Session::Provider)[1]
                .as_ref()
                .unwrap_err()
                .contains("same study timeframe")
        );
    }
}
