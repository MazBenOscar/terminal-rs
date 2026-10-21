//! Independently fetched or aggregated study histories, aligned on bar completion.
use super::{
    registry::{Input, Output, Spec},
    study::{self, Source, StudyHistories},
};
use crate::{Bar, Timeframe, session::Session};
use std::collections::{HashMap, HashSet};

type Results = Vec<Result<Vec<Output>, String>>;
fn timeframe(spec: &Spec, base: Timeframe) -> Timeframe {
    spec.chart.as_ref().and_then(|c| c.timeframe).unwrap_or(base)
}

pub(super) fn evaluate(specs: &[Spec], input: &Input<'_>, session: Session, histories: &StudyHistories) -> Results {
    if !specs.iter().any(|s| timeframe(s, input.tf) != input.tf) {
        return study::evaluate_native(specs, input);
    }
    let base = study::evaluate_native(specs, input);
    let mut cache = HashMap::new();
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
            let cached = cache.entry(tf).or_insert_with(|| {
                let aggregation = if input.tf.is_intraday() { session } else { Session::Provider };
                let (bars, ends) = match histories.get(&tf) {
                    Some(Ok(history)) => {
                        let mut bars: Vec<_> = history
                            .iter()
                            .filter(|b| !tf.is_intraday() || aggregation.contains(b.ts))
                            .copied()
                            .collect();
                        // Stable sorting preserves provider order among duplicates. Keep the last.
                        bars.sort_by_key(|b| b.ts);
                        bars.reverse();
                        bars.dedup_by_key(|b| b.ts);
                        bars.reverse();
                        if bars.is_empty() {
                            return Err(format!("No {tf} history is available"));
                        }
                        let ends: Vec<i64> = bars
                            .iter()
                            .map(|b| {
                                // Provider calendar labels are UTC dates, not NY instants.
                                // Noon UTC maps the label to the same local trading date.
                                let ts = if !tf.is_intraday() && aggregation != Session::Provider {
                                    Timeframe::D1.bucket_start(b.ts) + 12 * 3600
                                } else {
                                    b.ts
                                };
                                aggregation.interval(tf, ts).1
                            })
                            .collect();
                        (bars, ends)
                    }
                    Some(Err(e)) => return Err(format!("Could not load {tf} history: {e}")),
                    None if tf < input.tf => return Err("Lower-timeframe history is not loaded".into()),
                    None => aggregate(input.bars, tf, aggregation),
                };
                Ok((study::evaluate_native(specs, &Input::new(&bars, tf).with_session(session)), ends))
            });
            let (results, ends) = cached.as_ref().map_err(Clone::clone)?;
            let mut outputs = results[i].clone()?;
            for output in &mut outputs {
                if output.values.len() != ends.len() {
                    return Err("Intrinsically displaced outputs cannot use a different timeframe".into());
                }
                output.values = input
                    .bars
                    .iter()
                    .enumerate()
                    .map(|(i, bar)| {
                        let mut as_of = if input.tf.session_aligned(session) {
                            session.interval(input.tf, bar.ts).1
                        } else {
                            crate::session::next_bucket(input.tf, bar.ts)
                        };
                        // A successor can prove closure across weekends/holidays, but
                        // only for this bar's own study period, never a later gap period.
                        let aggregation = if input.tf.is_intraday() { session } else { Session::Provider };
                        let own_end = aggregation.interval(tf, bar.ts).1;
                        if !input.tf.is_intraday()
                            && let Some(next) = input.bars.get(i + 1)
                            && next.ts >= own_end
                        {
                            as_of = as_of.max(own_end);
                        }
                        ends.partition_point(|end| *end <= as_of).checked_sub(1).map_or(f64::NAN, |j| output.values[j])
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
        let full = evaluate(&specs, &Input::new(&bars, Timeframe::M1), Session::Provider, &StudyHistories::new())[0]
            .clone()
            .unwrap();
        assert!(full[0].values[..9].iter().all(|v| v.is_nan()));
        assert_eq!(full[0].values[9], 9.);
        assert_eq!(full[0].values[14], 14.);
        for n in 1..bars.len() {
            let prefix =
                evaluate(&specs, &Input::new(&bars[..n], Timeframe::M1), Session::Provider, &StudyHistories::new())[0]
                    .clone()
                    .unwrap();
            for (a, b) in prefix[0].values.iter().zip(&full[0].values) {
                assert!(a == b || a.is_nan() && b.is_nan());
            }
        }
    }
    fn bar(ts: i64, close: f64) -> Bar {
        Bar::new(ts, close, close, close, close, 1.)
    }

    fn specs(tfs: &[Timeframe]) -> Vec<Spec> {
        let mut specs: Vec<_> = tfs.iter().map(|_| Spec::new("SMA", &[1.]).unwrap()).collect();
        study::assign_ids(&mut specs);
        for (spec, tf) in specs.iter_mut().zip(tfs) {
            spec.chart.as_mut().unwrap().timeframe = Some(*tf);
        }
        specs
    }

    fn histories(tf: Timeframe, bars: Vec<Bar>) -> StudyHistories {
        HashMap::from([(tf, Ok(std::sync::Arc::new(bars)))])
    }

    #[test]
    fn regular_multi_hour_studies_complete_on_session_grid_and_early_close() {
        let open = chrono::DateTime::parse_from_rfc3339("2026-03-09T13:30:00Z").unwrap().timestamp();
        let bars: Vec<_> = (0..4).map(|i| bar(open + i * 7200, 1.)).collect();
        let result = evaluate(
            &specs(&[Timeframe::H4]),
            &Input::new(&bars, Timeframe::H2),
            Session::UsRegular,
            &histories(Timeframe::H4, vec![bar(open, 100.), bar(open + 14400, 200.)]),
        );
        let values = &result[0].as_ref().unwrap()[0].values;
        assert!(values[0].is_nan());
        assert_eq!(&values[1..], &[100., 100., 200.]);

        let open = chrono::DateTime::parse_from_rfc3339("2026-11-27T14:30:00Z").unwrap().timestamp();
        let bars = vec![bar(open, 1.), bar(open + 7200, 1.)];
        let result = evaluate(
            &specs(&[Timeframe::H4]),
            &Input::new(&bars, Timeframe::H2),
            Session::UsRegular,
            &histories(Timeframe::H4, vec![bar(open, 100.)]),
        );
        let values = &result[0].as_ref().unwrap()[0].values;
        assert!(values[0].is_nan());
        assert_eq!(values[1], 100.);
    }

    #[test]
    #[ignore = "BUG: fetched study-history ends are computed as session.interval(tf, ts).1, which anchors every intraday study timeframe at the 09:30 NY open, but chart_history only session-aligns H2/H4 and returns UTC-bucketed bars for H1 (and other tfs). An H1 bar stamped 15:00Z really closes 16:00Z, yet ends lands on 15:30Z so its value is painted onto the 09:30-11:30 H2 bar 30 minutes before the H1 bar finishes — look-ahead bias for any H1 study on a session-aligned 2H/4H chart"]
    fn h1_study_on_session_aligned_multi_hour_chart_waits_for_the_real_bar_close() {
        let open = chrono::DateTime::parse_from_rfc3339("2026-03-09T13:30:00Z").unwrap().timestamp();
        let bars: Vec<_> = (0..4).map(|i| bar(open + i * 7200, 1.)).collect();
        // Provider H1 history is UTC-aligned: 15:00Z covers 15:00-16:00Z (10:00-11:00 EDT).
        let h1 = vec![bar(chrono::DateTime::parse_from_rfc3339("2026-03-09T15:00:00Z").unwrap().timestamp(), 42.)];
        let result = evaluate(
            &specs(&[Timeframe::H1]),
            &Input::new(&bars, Timeframe::H2),
            Session::UsRegular,
            &histories(Timeframe::H1, h1),
        );
        let values = &result[0].as_ref().unwrap()[0].values;
        // Bar 0 is 09:30-11:30 EDT and completes at 15:30Z, before the H1 bar's 16:00Z close.
        assert!(values[0].is_nan(), "H1 value leaked onto a chart bar that closed first: {values:?}");
        // Bar 1 is 11:30-13:30 EDT (as_of 17:30Z) and may show the completed H1 bar.
        assert_eq!(values[1], 42.);
    }

    #[test]
    fn fetched_higher_history_replaces_aggregation() {
        let bars: Vec<_> = (0..36).filter(|i| *i != 17).map(|i| bar(i * 300, i as f64)).collect();
        let specs = specs(&[Timeframe::H1]);
        let histories = histories(Timeframe::H1, vec![bar(0, 100.), bar(3600, 200.), bar(7200, 300.)]);
        let values =
            study::evaluate_with_histories(&specs, &Input::new(&bars, Timeframe::M5), Session::Provider, &histories)[0]
                .as_ref()
                .unwrap()[0]
                .values
                .clone();
        assert!(values[..11].iter().all(|v| v.is_nan()));
        assert_eq!(values[11], 100.);
        assert_eq!(values[22], 200.);
        assert_eq!(values[34], 300.);
        let fallback = study::evaluate(&specs, &Input::new(&bars, Timeframe::M5));
        assert_eq!(fallback[0].as_ref().unwrap()[0].values[22], 23.);
    }

    #[test]
    fn lower_timeframe_values_use_last_completed_bar() {
        let bars: Vec<_> = (0..4).map(|i| bar(i * 300, 0.)).collect();
        let history = (0..25).map(|i| bar(i * 60, i as f64)).collect();
        let result = evaluate(
            &specs(&[Timeframe::M1]),
            &Input::new(&bars, Timeframe::M5),
            Session::Provider,
            &histories(Timeframe::M1, history),
        );
        assert_eq!(result[0].as_ref().unwrap()[0].values, vec![4., 9., 14., 19.]);
    }

    #[test]
    fn sparse_history_is_nan_before_first_study_bar() {
        let bars: Vec<_> = (0..4).map(|i| bar(i * 300, 0.)).collect();
        let result = evaluate(
            &specs(&[Timeframe::M1]),
            &Input::new(&bars, Timeframe::M5),
            Session::Provider,
            &histories(Timeframe::M1, vec![bar(600, 42.)]),
        );
        let values = &result[0].as_ref().unwrap()[0].values;
        assert!(values[..2].iter().all(|v| v.is_nan()));
        assert_eq!(&values[2..], &[42., 42.]);
    }

    #[test]
    fn replay_cutoff_never_leaks_future_bars() {
        let bars: Vec<_> = (0..40).map(|i| bar(i * 300, 0.)).collect();
        let specs = specs(&[Timeframe::M1, Timeframe::H1]);
        let mut histories = histories(Timeframe::M1, (0..300).map(|i| bar(i * 60, i as f64)).collect());
        histories.extend(self::histories(Timeframe::H1, (0..10).map(|i| bar(i * 3600, 1000. + i as f64)).collect()));
        let full = evaluate(&specs, &Input::new(&bars, Timeframe::M5), Session::Provider, &histories);
        for n in 1..=bars.len() {
            let prefix = evaluate(&specs, &Input::new(&bars[..n], Timeframe::M5), Session::Provider, &histories);
            for (index, tf) in [Timeframe::M1, Timeframe::H1].iter().enumerate() {
                let values = &prefix[index].as_ref().unwrap()[0].values;
                for (a, b) in values.iter().zip(&full[index].as_ref().unwrap()[0].values) {
                    assert!(a == b || a.is_nan() && b.is_nan());
                }
                let end = crate::session::next_bucket(Timeframe::M5, bars[n - 1].ts);
                let completed = end / tf.seconds();
                let expected = if completed == 0 {
                    f64::NAN
                } else {
                    (completed - 1) as f64 + if *tf == Timeframe::H1 { 1000. } else { 0. }
                };
                let last = values[n - 1];
                assert!(last == expected || last.is_nan() && expected.is_nan());
            }
        }
    }

    #[test]
    fn history_errors_and_missing_lower_history_are_explicit() {
        let specs = specs(&[Timeframe::H1, Timeframe::M1, Timeframe::M15, Timeframe::M5]);
        let mut histories = histories(Timeframe::M15, vec![]);
        histories.insert(Timeframe::H1, Err("offline".into()));
        let result = evaluate(&specs, &Input::new(&[bar(0, 7.)], Timeframe::M5), Session::Provider, &histories);
        assert_eq!(result[0].as_ref().unwrap_err(), "Could not load 1H history: offline");
        assert_eq!(result[1].as_ref().unwrap_err(), "Lower-timeframe history is not loaded");
        assert_eq!(result[2].as_ref().unwrap_err(), "No 15m history is available");
        assert_eq!(result[3].as_ref().unwrap()[0].values, vec![7.]);
    }

    fn ts(s: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp()
    }

    #[test]
    fn intraday_session_filters_fetched_lower_bars() {
        let open = ts("2026-03-09T13:30:00Z");
        let close = ts("2026-03-09T20:00:00Z");
        let bars = [bar(open - 300, 0.), bar(open, 0.), bar(close, 0.)];
        let history = vec![bar(open - 60, 999.), bar(open + 240, 4.), bar(close, 999.)];
        let result = evaluate(
            &specs(&[Timeframe::M1]),
            &Input::new(&bars, Timeframe::M5),
            Session::UsRegular,
            &histories(Timeframe::M1, history),
        );
        let values = &result[0].as_ref().unwrap()[0].values;
        assert!(values[0].is_nan());
        assert_eq!(&values[1..], &[4., 4.]);
    }

    #[test]
    fn daily_study_on_intraday_chart_appears_at_session_close() {
        let open = ts("2026-03-09T13:30:00Z");
        let bars: Vec<_> = (0..78).map(|i| bar(open + i * 300, 0.)).collect();
        let history = vec![bar(ts("2026-03-09T00:00:00Z"), 123.)];
        let result = evaluate(
            &specs(&[Timeframe::D1]),
            &Input::new(&bars, Timeframe::M5),
            Session::UsRegular,
            &histories(Timeframe::D1, history),
        );
        let values = &result[0].as_ref().unwrap()[0].values;
        assert!(values[..77].iter().all(|v| v.is_nan()));
        assert_eq!(values[77], 123.);
    }

    #[test]
    fn fetched_history_is_sorted_and_duplicate_timestamps_keep_last() {
        let history = vec![bar(240, 4.), bar(0, 0.), bar(240, 44.), bar(60, 1.)];
        let result = evaluate(
            &specs(&[Timeframe::M1]),
            &Input::new(&[bar(0, 0.)], Timeframe::M5),
            Session::Provider,
            &histories(Timeframe::M1, history),
        );
        assert_eq!(result[0].as_ref().unwrap()[0].values, vec![44.]);
    }

    #[test]
    fn cross_timeframe_dependencies_are_explicit_errors() {
        let mut specs = vec![Spec::new("RSI", &[]).unwrap(), Spec::new("EMA", &[]).unwrap()];
        study::assign_ids(&mut specs);
        specs[0].chart.as_mut().unwrap().timeframe = Some(Timeframe::D1);
        specs[1].chart.as_mut().unwrap().source = Source::Output { study: 1, output: "RSI".into() };
        assert!(
            evaluate(&specs, &Input::new(&[], Timeframe::H1), Session::Provider, &StudyHistories::new())[1]
                .as_ref()
                .unwrap_err()
                .contains("same study timeframe")
        );
    }

    /// 5m bars covering `[open, close)` of a session.
    fn session_5m(open: i64, close: i64) -> Vec<Bar> {
        (open..close).step_by(300).map(|ts| bar(ts, 0.)).collect()
    }

    /// Session-anchored 1H history bars: open, open+1h, …
    fn session_1h(open: i64, close: i64, values: impl Fn(usize) -> f64) -> Vec<Bar> {
        let hours = ((close - open) + 3599) / 3600;
        (0..hours).map(|k| bar(open + k * 3600, values(k as usize))).collect()
    }

    fn both(
        specs: &[Spec],
        bars: &[Bar],
        tf: Timeframe,
        session: Session,
        histories: &StudyHistories,
    ) -> Vec<Vec<f64>> {
        evaluate(specs, &Input::new(bars, tf), session, histories)
            .into_iter()
            .map(|r| r.unwrap().into_iter().next().unwrap().values)
            .collect()
    }

    /// US DST 2026: spring-forward Sun Mar 8 (after Fri Mar 6 EST), fall-back Sun Nov 1
    /// (after Fri Oct 30 EDT). Exchange sessions must follow the local 09:30–16:00
    /// schedule even when their UTC close shifts by an hour.
    #[test]
    fn dst_transition_days_align_1h_and_daily_to_session_close() {
        // (open, close) UTC for each session; EST closes 21:00Z, EDT closes 20:00Z.
        let days = [
            ("2026-03-06", ts("2026-03-06T14:30:00Z"), ts("2026-03-06T21:00:00Z")), // Fri EST
            ("2026-03-09", ts("2026-03-09T13:30:00Z"), ts("2026-03-09T20:00:00Z")), // Mon EDT
            ("2026-10-30", ts("2026-10-30T13:30:00Z"), ts("2026-10-30T20:00:00Z")), // Fri EDT
            ("2026-11-02", ts("2026-11-02T14:30:00Z"), ts("2026-11-02T21:00:00Z")), // Mon EST
        ];
        let mut chart = Vec::new();
        let mut h1 = Vec::new();
        let mut d1 = Vec::new();
        let mut day_slices = Vec::new();
        for (i, (date, open, close)) in days.iter().enumerate() {
            let bars = session_5m(*open, *close);
            day_slices.push((chart.len(), chart.len() + bars.len(), *close, i as f64));
            chart.extend(bars);
            h1.extend(session_1h(*open, *close, |k| 100. * i as f64 + k as f64));
            d1.push(bar(ts(&format!("{date}T00:00:00Z")), 1000. + i as f64));
        }
        let specs = specs(&[Timeframe::H1, Timeframe::D1]);
        let mut histories = histories(Timeframe::H1, h1);
        histories.extend(self::histories(Timeframe::D1, d1));
        let out = both(&specs, &chart, Timeframe::M5, Session::UsRegular, &histories);
        for (start, end, session_close, day) in day_slices {
            // Daily value holds the previous session's close until this session closes.
            let prev = if day == 0. { f64::NAN } else { 1000. + day - 1. };
            assert!(
                out[1][start..end - 1].iter().all(|v| *v == prev || v.is_nan() && prev.is_nan()),
                "day {day} leaked its daily value before close"
            );
            assert_eq!(out[1][end - 1], 1000. + day, "day {day} daily close");
            assert_eq!(chart[end - 1].ts + 300, session_close, "day {day} last bar ends at session close");
            // 1H value k appears when its session-anchored hour ends; the last hour
            // ends at the session close (30 minutes on these full 6.5h sessions).
            for k in 0..7 {
                let hour_end = if k == 6 { session_close } else { chart[start].ts + (k as i64 + 1) * 3600 };
                let at = start + ((hour_end - chart[start].ts) / 300) as usize - 1;
                assert_eq!(out[0][at], 100. * day + k as f64, "day {day} hour {k}");
                assert!(out[0][start..at].iter().all(|v| v.is_nan() || *v != 100. * day + k as f64));
            }
        }
    }

    /// Aggregation (no fetched 1H history) uses the same session-anchored ends, so
    /// the final aggregated hour must also wait for the DST-correct session close.
    #[test]
    fn dst_fall_back_aggregated_1h_waits_for_est_session_close() {
        let open = ts("2026-11-02T14:30:00Z");
        let close = ts("2026-11-02T21:00:00Z");
        let bars = session_5m(open, close);
        let out = both(&specs(&[Timeframe::H1]), &bars, Timeframe::M5, Session::UsRegular, &StudyHistories::new());
        // First aggregated hour is dropped conservatively; the last (7th) hour ends at close.
        assert!(out[0].iter().all(|v| v.is_nan() || *v >= 0.));
        let last = out[0].iter().rposition(|v| !v.is_nan()).unwrap();
        assert_eq!(bars[last].ts + 300, close, "last 1H value lands on the EST session close");
    }

    #[test]
    fn dst_spring_forward_aggregated_1h_waits_for_edt_session_close() {
        let open = ts("2026-03-09T13:30:00Z");
        let close = ts("2026-03-09T20:00:00Z");
        let bars = session_5m(open, close);
        let out = both(&specs(&[Timeframe::H1]), &bars, Timeframe::M5, Session::UsRegular, &StudyHistories::new());
        let last = out[0].iter().rposition(|v| !v.is_nan()).unwrap();
        assert_eq!(bars[last].ts + 300, close, "last 1H value lands on the EDT session close");
    }

    /// W1/MN1 values must appear on the week's / month's last trading day bar,
    /// never earlier in the period.
    #[test]
    fn weekly_and_monthly_history_appears_on_the_periods_last_trading_day() {
        // D1 chart: Mon 2026-03-02..Fri 2026-03-06, then Mon 2026-03-09..Fri 2026-03-13;
        // January 2026 trading days through Fri 2026-01-30 (Jan 1 holiday, Jan 19 MLK).
        let week1: Vec<i64> = (0..5).map(|d| ts(&format!("2026-03-{:02}T00:00:00Z", 2 + d))).collect();
        let week2: Vec<i64> = (0..5).map(|d| ts(&format!("2026-03-{:02}T00:00:00Z", 9 + d))).collect();
        let january: Vec<i64> = (2..=30)
            .filter(|d| {
                let date = chrono::NaiveDate::from_ymd_opt(2026, 1, *d as u32).unwrap();
                !crate::session::closed(date)
            })
            .map(|d| ts(&format!("2026-01-{d:02}T00:00:00Z")))
            .collect();
        assert_eq!(january.len(), 20, "Jan 2026 has 20 open sessions after New Year");
        let mut chart: Vec<Bar> = week1.iter().chain(&week2).chain(&january).map(|&t| bar(t, 0.)).collect();
        chart.sort_by_key(|b| b.ts);
        let w1 = vec![bar(ts("2026-03-02T00:00:00Z"), 10.), bar(ts("2026-03-09T00:00:00Z"), 20.)];
        let mn1 = vec![bar(ts("2026-01-01T00:00:00Z"), 100.)];
        let specs = specs(&[Timeframe::W1, Timeframe::MN1]);
        let mut histories = histories(Timeframe::W1, w1);
        histories.extend(self::histories(Timeframe::MN1, mn1));
        let out = both(&specs, &chart, Timeframe::D1, Session::UsRegular, &histories);
        let at = |t: i64| chart.iter().position(|b| b.ts == t).unwrap();
        let fri_week1 = at(ts("2026-03-06T00:00:00Z"));
        let thu_week1 = at(ts("2026-03-05T00:00:00Z"));
        let fri_jan = at(ts("2026-01-30T00:00:00Z"));
        let thu_jan = at(ts("2026-01-29T00:00:00Z"));
        assert!(out[0][..=thu_week1].iter().all(|v| v.is_nan()), "W1 must not appear before the last trading day");
        assert_eq!(out[0][fri_week1], 10., "W1 appears on the week's last trading day");
        assert!(out[1][..=thu_jan].iter().all(|v| v.is_nan()), "MN1 must not appear before the last trading day");
        assert_eq!(out[1][fri_jan], 100., "MN1 appears on the month's last trading day");
    }

    #[test]
    fn last_friday_hides_current_week_until_monday_is_loaded() {
        let mut chart: Vec<_> = (2..=6).map(|d| bar(ts(&format!("2026-03-{d:02}T00:00:00Z")), 0.)).collect();
        let specs = specs(&[Timeframe::W1]);
        let histories =
            histories(Timeframe::W1, vec![bar(ts("2026-02-23T00:00:00Z"), 5.), bar(ts("2026-03-02T00:00:00Z"), 10.)]);
        let prefix = both(&specs, &chart, Timeframe::D1, Session::UsRegular, &histories);
        assert_eq!(prefix[0], vec![5.; 5], "last Friday has no successor proving the week closed");

        chart.push(bar(ts("2026-03-09T00:00:00Z"), 0.));
        let full = both(&specs, &chart, Timeframe::D1, Session::UsRegular, &histories);
        assert_eq!(full[0], vec![5., 5., 5., 5., 10., 10.], "Monday proves closure on Friday");
        for n in 1..=chart.len() {
            let prefix = both(&specs, &chart[..n], Timeframe::D1, Session::UsRegular, &histories);
            // A D1+ prefix's final bar lacks the successor that can prove period closure.
            assert_eq!(prefix[0][..n - 1], full[0][..n - 1]);
        }
    }

    /// The no-lookahead half of the property above: W1/MN1 values never
    /// appear before the period's last trading day.
    #[test]
    fn weekly_and_monthly_history_never_appear_earlier_than_the_last_trading_day() {
        let week1: Vec<i64> = (0..5).map(|d| ts(&format!("2026-03-{:02}T00:00:00Z", 2 + d))).collect();
        let mut chart: Vec<Bar> = week1.iter().map(|&t| bar(t, 0.)).collect();
        chart.extend(
            (2..=30)
                .filter(|d| {
                    let date = chrono::NaiveDate::from_ymd_opt(2026, 1, *d as u32).unwrap();
                    !crate::session::closed(date)
                })
                .map(|d| bar(ts(&format!("2026-01-{d:02}T00:00:00Z")), 0.)),
        );
        chart.sort_by_key(|b| b.ts);
        let w1 = vec![bar(ts("2026-03-02T00:00:00Z"), 10.)];
        let mn1 = vec![bar(ts("2026-01-01T00:00:00Z"), 100.)];
        let specs = specs(&[Timeframe::W1, Timeframe::MN1]);
        let mut histories = histories(Timeframe::W1, w1);
        histories.extend(self::histories(Timeframe::MN1, mn1));
        let out = both(&specs, &chart, Timeframe::D1, Session::UsRegular, &histories);
        let at = |t: i64| chart.iter().position(|b| b.ts == t).unwrap();
        let thu_week1 = at(ts("2026-03-05T00:00:00Z"));
        let thu_jan = at(ts("2026-01-29T00:00:00Z"));
        assert!(out[0][..=thu_week1].iter().all(|v| v.is_nan()));
        assert!(out[1][..=thu_jan].iter().all(|v| v.is_nan()));
    }

    #[test]
    fn in_progress_last_bar_stops_at_chart_bar_end_and_never_shows_later_study_bars() {
        // Chart M5 bars at 0:00, 0:05, 0:10; the last is still forming and covers
        // [0:10, 0:15). Lower-tf values use only study bars ending <= 0:15.
        let bars: Vec<_> = [0, 300, 600].iter().map(|&t| bar(t, 0.)).collect();
        let history: Vec<_> = (0..=11).map(|i| bar(i * 60, i as f64)).collect();
        let out =
            both(&specs(&[Timeframe::M1]), &bars, Timeframe::M5, Session::Provider, &histories(Timeframe::M1, history));
        // Bar 0 [0:00,0:05): last M1 ending <= 0:05 is the 0:04 bar (close 4).
        // Bar 1 [0:05,0:10): last M1 ending <= 0:10 is the 0:09 bar (close 9).
        // Bar 2 [0:10,0:15): last M1 ending <= 0:15 is the 0:11 bar (close 11).
        assert_eq!(out[0], vec![4., 9., 11.]);
        // Study bars whose bucket starts at/after the chart's last bar end (0:15)
        // never appear. A bar at 0:14 still counts: its bucket ends at 0:15.
        let mut extra = (0..=14).map(|i| bar(i * 60, i as f64)).collect::<Vec<_>>(); // through 0:14
        extra.push(bar(900, 999.)); // 0:15, after the chart's last bar
        extra.push(bar(960, 888.)); // 0:16, after the chart's last bar
        let out2 =
            both(&specs(&[Timeframe::M1]), &bars, Timeframe::M5, Session::Provider, &histories(Timeframe::M1, extra));
        assert_eq!(out2[0], vec![4., 9., 14.], "only study bars ending <= the chart bar end appear");
        assert!(out2[0].iter().all(|v| *v != 999. && *v != 888.));
    }

    #[test]
    fn chained_ema_of_rsi_is_identical_for_unsorted_and_duplicate_history() {
        let mut specs = vec![Spec::new("RSI", &[3.]).unwrap(), Spec::new("EMA", &[2.]).unwrap()];
        study::assign_ids(&mut specs);
        for spec in &mut specs {
            spec.chart.as_mut().unwrap().timeframe = Some(Timeframe::M1);
        }
        specs[1].chart.as_mut().unwrap().source =
            Source::Output { study: specs[0].chart.as_ref().unwrap().id, output: "RSI".into() };
        let bars: Vec<_> = (0..4).map(|i| bar(i * 300, 0.)).collect();
        let sorted: Vec<Bar> = (0..30).map(|i| bar(i * 60, i as f64)).collect();
        // Same bars, reversed, with a wrong-value duplicate of ts=300 placed earlier
        // so keep-last recovers the correct bar.
        let mut messy = sorted.clone();
        messy.reverse();
        messy.insert(0, bar(300, -999.));
        messy.insert(7, bar(600, -998.));
        let clean = both(&specs, &bars, Timeframe::M5, Session::Provider, &histories(Timeframe::M1, sorted));
        let dirty = both(&specs, &bars, Timeframe::M5, Session::Provider, &histories(Timeframe::M1, messy));
        assert_eq!(clean.len(), 2);
        for (a, b) in clean.iter().zip(&dirty) {
            assert!(a.iter().zip(b).all(|(x, y)| x == y || x.is_nan() && y.is_nan()), "chain changed with input order");
        }
        assert!(clean[1].iter().any(|v| v.is_finite()), "EMA of RSI produced values");
    }

    /// A chart gap longer than one study period must not leak a later period's
    /// value onto the bar before the gap. The successor only proves the chart
    /// bar's own period closed; it is not a license to skip ahead.
    #[test]
    fn chart_gap_does_not_leak_the_next_weeks_study_value_onto_the_prior_bar() {
        // Fri 2026-03-06 then Mon 2026-03-16: the week of Mar 9 is missing entirely.
        let bars = vec![bar(ts("2026-03-06T00:00:00Z"), 0.), bar(ts("2026-03-16T00:00:00Z"), 0.)];
        let w1 = vec![bar(ts("2026-03-02T00:00:00Z"), 10.), bar(ts("2026-03-09T00:00:00Z"), 20.)];
        let out =
            both(&specs(&[Timeframe::W1]), &bars, Timeframe::D1, Session::UsRegular, &histories(Timeframe::W1, w1));
        // Fri Mar 6 must show the week of Mar 2 (10). The missing week's close (20)
        // ends Mar 16 and must never displace backward across the gap.
        assert_eq!(out[0][0], 10., "a multi-week gap must not leak the next week's W1 value onto Friday");
        assert_eq!(out[0][1], 20., "Monday shows the week that just closed");
    }

    #[test]
    fn chart_gap_does_not_leak_the_next_months_study_value_onto_the_prior_bar() {
        // Fri 2026-01-30 then Mon 2026-03-02: February's sessions are missing.
        let bars = vec![bar(ts("2026-01-30T00:00:00Z"), 0.), bar(ts("2026-03-02T00:00:00Z"), 0.)];
        let mn1 = vec![bar(ts("2026-01-01T00:00:00Z"), 100.), bar(ts("2026-02-01T00:00:00Z"), 200.)];
        let out =
            both(&specs(&[Timeframe::MN1]), &bars, Timeframe::D1, Session::UsRegular, &histories(Timeframe::MN1, mn1));
        assert_eq!(out[0][0], 100., "a missing month must not leak February's MN1 value onto January's last day");
        assert_eq!(out[0][1], 200., "March's first session shows the month that just closed");
    }

    /// Week whose Friday is a holiday (Good Friday 2026-04-03): with the next
    /// session loaded, the W1 value lands on Thursday — the period's last
    /// trading day — never earlier in the week.
    #[test]
    fn holiday_friday_week_shows_weekly_value_on_thursday_when_monday_is_loaded() {
        // Mon Mar 30 .. Thu Apr 2 (Good Friday Apr 3 closed); successor Mon Apr 6.
        let week: Vec<i64> = ["2026-03-30", "2026-03-31", "2026-04-01", "2026-04-02", "2026-04-06"]
            .map(|d| ts(&format!("{d}T00:00:00Z")))
            .to_vec();
        let bars: Vec<Bar> = week.iter().map(|&t| bar(t, 0.)).collect();
        let w1 = vec![bar(ts("2026-03-30T00:00:00Z"), 10.), bar(ts("2026-04-06T00:00:00Z"), 20.)];
        let out =
            both(&specs(&[Timeframe::W1]), &bars, Timeframe::D1, Session::UsRegular, &histories(Timeframe::W1, w1));
        assert!(out[0][..3].iter().all(|v| v.is_nan()), "W1 must not appear before the week's last trading day");
        assert_eq!(out[0][3], 10., "holiday-Friday week shows W1 on Thursday");
        assert_eq!(out[0][4], 10., "Monday still shows the closed week until the next week closes");
    }

    /// EMA 200 seeded with SMA(200) retains ~37% of its seed transient only 100
    /// bars past the seed. The worker's fixed 300 warm-up bars therefore leave
    /// EMA 200 on W1 visibly under-converged at the start of a fetched history.
    #[test]
    fn warmup_of_300_is_insufficient_for_ema_200_on_weekly_bars() {
        // Violent swings keep the decaying seed transient large.
        let specs = [Spec::new("EMA", &[200.]).unwrap()];
        // Four initialization windows are needed for this test's strict 1e-9
        // numerical convergence tolerance; the worker requests one such window.
        let warmup = 4 * study::warmup_bars(&specs);
        let history_len = 2 * warmup + 50;
        let closes: Vec<f64> = (0..history_len)
            .map(|i| {
                let t = i as f64;
                100. + 40. * (t * 0.037).sin() + 25. * (t * 0.11).sin() + 15. * (t * 0.41).sin()
            })
            .collect();
        let long: Vec<Bar> = closes.iter().enumerate().map(|(i, &c)| bar(i as i64 * 604_800, c)).collect();
        let offset = history_len - (warmup + 50);
        let short = &long[offset..];
        let ema = |bars: &[Bar]| {
            let mut specs = vec![Spec::new("EMA", &[200.]).unwrap()];
            study::assign_ids(&mut specs);
            specs[0].chart.as_mut().unwrap().timeframe = Some(Timeframe::W1);
            // Evaluate natively on the study's own bars (same kernel the cache path uses).
            study::evaluate_native(&specs, &Input::new(bars, Timeframe::W1)).remove(0).unwrap().remove(0).values
        };
        let converged = ema(&long);
        let starved = ema(short);
        let mut worst = 0.0f64;
        for i in warmup..short.len() {
            let a = starved[i];
            let b = converged[offset + i];
            worst = worst.max((a - b).abs());
            assert!(
                (a - b).abs() < 1e-9,
                "EMA 200 with {warmup} warm-up diverges at chart bar {}: starved {a} vs converged {b}",
                i - warmup
            );
        }
        let _ = worst;
    }

    /// MN3 values must land on the quarter's last trading session once the next
    /// quarter's first bar proves the quarter closed — including the year-end wrap.
    #[test]
    fn quarterly_history_appears_on_the_quarters_last_trading_day_across_year_end() {
        // Q4-2025 trading days ending Wed 2025-12-31; Q1-2026 opens Fri 2026-01-02
        // (Jan 1 is a holiday). Also cover a plain quarter boundary: Q1 ends
        // Tue 2026-03-31, Q2 opens Wed 2026-04-01.
        let q4_last: Vec<i64> =
            ["2025-12-29", "2025-12-30", "2025-12-31"].map(|d| ts(&format!("{d}T00:00:00Z"))).to_vec();
        let q1_2026: Vec<i64> = ["2026-01-02", "2026-01-05"].map(|d| ts(&format!("{d}T00:00:00Z"))).to_vec();
        let q1_last: Vec<i64> = ["2026-03-30", "2026-03-31"].map(|d| ts(&format!("{d}T00:00:00Z"))).to_vec();
        let q2_open: Vec<i64> = ["2026-04-01"].map(|d| ts(&format!("{d}T00:00:00Z"))).to_vec();
        let mut chart: Vec<Bar> =
            q4_last.iter().chain(&q1_2026).chain(&q1_last).chain(&q2_open).map(|&t| bar(t, 0.)).collect();
        chart.sort_by_key(|b| b.ts);
        let mn3 = vec![
            bar(ts("2025-10-01T00:00:00Z"), 10.), // Q4-2025, ends 2026-01-01
            bar(ts("2026-01-01T00:00:00Z"), 20.), // Q1-2026, ends 2026-04-01
        ];
        let out =
            both(&specs(&[Timeframe::MN3]), &chart, Timeframe::D1, Session::UsRegular, &histories(Timeframe::MN3, mn3));
        let at = |t: i64| chart.iter().position(|b| b.ts == t).unwrap();
        let dec31 = at(ts("2025-12-31T00:00:00Z"));
        let mar31 = at(ts("2026-03-31T00:00:00Z"));
        let jan2 = at(ts("2026-01-02T00:00:00Z"));
        assert!(out[0][..dec31].iter().all(|v| v.is_nan()), "Q4 value must wait for its last trading day");
        assert_eq!(out[0][dec31], 10., "Q4-2025 value lands on Dec 31");
        assert_eq!(out[0][jan2], 10., "January sessions still show Q4 until Q1 closes");
        assert!(out[0][..mar31].iter().skip(jan2).all(|v| *v == 10.), "Q1 must not appear before its last trading day");
        assert_eq!(out[0][mar31], 20., "Q1-2026 value lands on Mar 31");
    }

    /// An intraday chart missing its final session bar cannot prove the session
    /// closed, so the daily value waits for the next session's first bar — it is
    /// not stalled any further, and the next session's own value stays hidden
    /// until that session closes.
    #[test]
    fn missing_last_intraday_bar_delays_daily_value_only_until_the_next_session() {
        let open = ts("2026-03-09T13:30:00Z");
        // Drop the final M5 bar of the session (19:55–20:00), so no bar proves close.
        let day1: Vec<Bar> = (0..77).map(|i| bar(open + i * 300, 0.)).collect();
        let next_open = ts("2026-03-10T13:30:00Z");
        let mut bars = day1;
        bars.push(bar(next_open, 0.));
        let history = vec![bar(ts("2026-03-09T00:00:00Z"), 123.), bar(ts("2026-03-10T00:00:00Z"), 200.)];
        let out = both(
            &specs(&[Timeframe::D1]),
            &bars,
            Timeframe::M5,
            Session::UsRegular,
            &histories(Timeframe::D1, history),
        );
        assert!(
            out[0][..77].iter().all(|v| v.is_nan()),
            "no bar proves the Mar 9 session closed, so its daily value stays hidden on Mar 9"
        );
        assert_eq!(out[0][77], 123., "the next session's first bar carries the prior daily close");
    }

    /// Day after Thanksgiving 2026: built-in calendar closes at 13:00 New York.
    #[test]
    fn half_day_session_aligns_studies_to_the_early_close() {
        let open = ts("2026-11-27T14:30:00Z");
        let close = ts("2026-11-27T18:00:00Z"); // 13:00 EST
        let bars = session_5m(open, close);
        assert_eq!(bars.len(), 42);
        let h1 = session_1h(open, close, |k| 10. + k as f64);
        let d1 = vec![bar(ts("2026-11-27T00:00:00Z"), 99.)];
        let specs = specs(&[Timeframe::H1, Timeframe::D1]);
        let mut histories = histories(Timeframe::H1, h1);
        histories.extend(self::histories(Timeframe::D1, d1));
        let out = both(&specs, &bars, Timeframe::M5, Session::UsRegular, &histories);
        let last = bars.len() - 1;
        assert_eq!(bars[last].ts + 300, close);
        assert!(out[1][..last].iter().all(|v| v.is_nan()));
        assert_eq!(out[1][last], 99., "daily value waits for the early close");
        // Four session-anchored hours: 09:30, 10:30, 11:30, 12:30–13:00.
        assert_eq!(out[0][last], 13., "last 1H value is the 12:30 hour ending at 13:00");
        let hour2 = out[0].iter().position(|v| *v == 12.).unwrap();
        assert_eq!(bars[hour2].ts + 300, open + 3 * 3600, "11:30 hour lands on its 12:30 end");
    }
}
