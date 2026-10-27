//! Provider events plus explicit local imports (including earnings).
use egui::{Align2, Rect, Ui, pos2};
use tr_chart::viewport::Viewport;
use tr_core::Bar;
use tr_data::{ChartEvent, EventKind};

pub fn imported(path: &std::path::Path, symbol: &str, start: i64, end: i64) -> Result<Vec<ChartEvent>, String> {
    let bytes = match std::fs::read(path) {
        Ok(b) if b.len() <= 2_097_152 => b,
        Ok(_) => return Err("chart-events.json exceeds 2 MiB".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.to_string()),
    };
    let events: Vec<ChartEvent> = serde_json::from_slice(&bytes).map_err(|e| format!("chart-events.json: {e}"))?;
    if events.len() > 10_000
        || events.iter().any(|e| {
            e.ts <= 0 || e.detail.len() > 500 || e.source.is_empty() || e.source.len() > 200 || e.symbol.len() > 100
        })
    {
        return Err("Invalid/oversized corporate event import".into());
    }
    Ok(events.into_iter().filter(|e| e.symbol == symbol && e.ts >= start && e.ts <= end).collect())
}
// N2: pure store-to-chart mapping, no network on chart load.
pub fn filings(rows: &[tr_edgar::Filing], symbol: &str, start: i64, end: i64) -> Vec<ChartEvent> {
    if tr_edgar::ticker(symbol).is_none() {
        return vec![];
    }
    rows.iter()
        .filter_map(|f| {
            let ts = f.accepted.unwrap_or(f.filed);
            (f.matches_symbol(symbol) && !f.reporting_owner && f.chart_marker() && ts >= start && ts <= end).then(
                || ChartEvent {
                    symbol: symbol.into(),
                    ts,
                    kind: EventKind::Filing,
                    detail: f.detail(),
                    source: "SEC EDGAR · local store".into(),
                },
            )
        })
        .chain(insiders(rows, symbol, start, end))
        .collect()
}
// N3: code P/S lots only, from either table. Joint owners don't multiply chart markers.
pub fn insiders(rows: &[tr_edgar::Filing], symbol: &str, start: i64, end: i64) -> Vec<ChartEvent> {
    let mut events = vec![];
    let effective = tr_edgar::insider::effective_lots(rows, symbol);
    for f in rows.iter().filter(|f| !f.reporting_owner && f.matches_symbol(symbol)) {
        let lots: Vec<_> = effective
            .iter()
            .filter(|(filing, t)| filing.accession == f.accession && matches!(t.code.as_str(), "P" | "S"))
            .map(|(_, t)| (*t).clone())
            .collect();
        // Group by transaction date, so a filing with several dates marks the actual trade days.
        let dates: std::collections::BTreeSet<_> = lots.iter().map(|t| t.date.as_str()).collect();
        for date in dates {
            let Some(ts) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|d| d.and_utc().timestamp())
            else {
                continue;
            };
            if ts < start || ts > end {
                continue;
            }
            let daily: Vec<_> = lots.iter().filter(|t| t.date == date).cloned().collect();
            for g in tr_edgar::insider::aggregate(&daily) {
                let action = if g.code == "P" { "bought" } else { "sold" };
                events.push(ChartEvent {
                    symbol: symbol.into(),
                    ts,
                    kind: if g.code == "P" { EventKind::InsiderBuy } else { EventKind::InsiderSell },
                    detail: format!(
                        "{} {action} {}",
                        g.owner.display_name(),
                        g.value.map(tr_edgar::insider::value).unwrap_or_else(|| "(value unavailable)".into())
                    ),
                    source: "SEC EDGAR · local store".into(),
                });
            }
        }
    }
    events
}
fn bar_index(bars: &[Bar], tf: tr_core::Timeframe, timestamp: i64) -> Option<usize> {
    let bucket = |ts: i64| if tf.is_intraday() { ts.div_euclid(86_400) } else { tf.bucket_start(ts) };
    let target = bucket(timestamp);
    let i = bars.partition_point(|b| bucket(b.ts) < target);
    (i < bars.len() && bucket(bars[i].ts) == target).then_some(i)
}
fn appearance(kind: &EventKind) -> (&'static str, egui::Color32) {
    match kind {
        EventKind::Earnings => ("E", crate::theme::amber()),
        EventKind::Dividend => ("D", crate::theme::up()),
        EventKind::Split => ("S", crate::theme::cyan()),
        EventKind::Filing => ("F", crate::theme::accent()),
        EventKind::InsiderBuy => ("▲", crate::theme::up()),
        EventKind::InsiderSell => ("▼", crate::theme::down()),
    }
}
pub fn paint(
    ui: &mut Ui,
    rect: Rect,
    viewport: &Viewport,
    bars: &[Bar],
    tf: tr_core::Timeframe,
    events: &[ChartEvent],
) {
    let (a, b) = viewport.visible_range(bars.len());
    let mut stacked = std::collections::HashMap::<usize, usize>::new();
    for event in events {
        let Some(index) = bar_index(bars, tf, event.ts) else {
            continue;
        };
        if index < a || index >= b {
            continue;
        }
        let stack = stacked.entry(index).or_default();
        let center = pos2(
            rect.left() + viewport.x_center(index) as f32,
            rect.bottom() - crate::theme::dp(12.) - (*stack).min(5) as f32 * crate::theme::dp(19.),
        );
        *stack += 1;
        let (letter, color) = appearance(&event.kind);
        if !matches!(event.kind, EventKind::InsiderBuy | EventKind::InsiderSell) {
            ui.painter().circle_filled(center, crate::theme::dp(8.), crate::theme::card());
            ui.painter().circle_stroke(center, crate::theme::dp(8.), egui::Stroke::new(crate::theme::dp(1.), color));
        }
        ui.painter().text(center, Align2::CENTER_CENTER, letter, crate::theme::mono(10.), color);
        ui.interact(
            Rect::from_center_size(center, egui::vec2(crate::theme::dp(18.), crate::theme::dp(18.))),
            ui.id().with(("corp-event", event.ts, &event.detail)),
            egui::Sense::hover(),
        )
        .on_hover_text(format!(
            "{}\n{}\n{}",
            event.detail,
            crate::feed_time::feed_time(
                event.ts,
                chrono::Utc::now().timestamp(),
                matches!(event.kind, EventKind::InsiderBuy | EventKind::InsiderSell)
            )
            .1,
            event.source
        ));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn insider_markers_map_only_open_market_lots_from_store_at_transaction_date() {
        let d =
            tr_edgar::insider::parse(include_bytes!("../../crates/edgar/tests/fixtures/form4_sale_10b5_1.xml"), 320193)
                .unwrap();
        let ts =
            chrono::NaiveDate::from_ymd_opt(2026, 10, 13).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        let mut store = tr_edgar::store::Store::memory().unwrap();
        store
            .replace_companies(&[tr_edgar::Company { cik: 320193, ticker: "AAPL".into(), title: "Apple".into() }])
            .unwrap();
        let f = tr_edgar::Filing {
            cik: 320193,
            ticker: "AAPL".into(),
            accession: "sale".into(),
            form: "4".into(),
            filed: ts + 2 * 86400,
            accepted: Some(ts + 2 * 86400 + 3600),
            ..Default::default()
        };
        store.upsert(std::slice::from_ref(&f)).unwrap();
        store.save_insiders(&f, &d).unwrap();
        let rows = store.chart_filings("AAPL US Equity", ts, ts + 3600).unwrap();
        assert_eq!(rows.len(), 1);
        let e = filings(&rows, "AAPL US Equity", ts, ts + 3600);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].ts, ts);
        assert_eq!(e[0].kind, EventKind::InsiderSell);
        assert_eq!(e[0].detail, "Newstead J. sold $828K");
        assert_eq!(appearance(&EventKind::InsiderBuy), ("▲", crate::theme::up()));
        assert_eq!(appearance(&EventKind::InsiderSell), ("▼", crate::theme::down()));
        let mut rows = rows;
        rows[0].insiders[0].code = "P".into();
        assert_eq!(insiders(&rows, "AAPL", ts, ts + 3600)[0].kind, EventKind::InsiderBuy);
        for code in ["A", "M", "F", "G", "C", "J"] {
            rows[0].insiders[0].code = code.into();
            assert!(insiders(&rows, "AAPL", ts, ts + 3600).is_empty());
        }
        rows[0].insiders[0].code = "P".into();
        rows[0].insiders[0].derivative = true;
        assert_eq!(insiders(&rows, "AAPL", ts, ts + 3600)[0].kind, EventKind::InsiderBuy);
        rows[0].insiders[0].code = "M".into();
        assert!(insiders(&rows, "AAPL", ts, ts + 3600).is_empty());
        assert!(insiders(&rows, "MSFT", ts, ts + 3600).is_empty());
        assert!(insiders(&rows, "AAPL", ts + 1, ts + 3600).is_empty());
    }
    // N2
    #[test]
    fn filing_events_use_stored_acceptance_items_symbol_and_range() {
        let rows = vec![
            tr_edgar::Filing {
                ticker: "AAPL".into(),
                form: "8-K".into(),
                filed: 100,
                accepted: Some(150),
                items: "2.02,9.01".into(),
                ..Default::default()
            },
            tr_edgar::Filing { ticker: "AAPL".into(), form: "4".into(), filed: 100, ..Default::default() },
            tr_edgar::Filing { ticker: "MSFT".into(), form: "10-Q".into(), filed: 100, ..Default::default() },
        ];
        let events = filings(&rows, "AAPL US Equity", 140, 200);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].ts, 150);
        assert_eq!(events[0].kind, EventKind::Filing);
        assert!(events[0].detail.contains("2.02 Results of Operations"));
        assert!(filings(&rows, "BTCUSD Curncy", 0, 200).is_empty());
        assert!(filings(&rows, "AAPL US Equity", 0, 149).is_empty());
        for form in ["8-K/A", "10-Q", "10-K", "10-K/A"] {
            let row = tr_edgar::Filing { ticker: "AAPL".into(), form: form.into(), filed: 100, ..Default::default() };
            assert_eq!(filings(&[row], "AAPL US Equity", 0, 200).len(), 1);
        }
    }
    #[test]
    fn events_map_to_daily_and_aggregate_buckets_without_inventing_missing_days() {
        let monday = 1_792_368_000; // UTC Monday 2026-10-19
        let monday = tr_core::Timeframe::W1.bucket_start(monday);
        let bars = [Bar::new(monday, 1., 1., 1., 1., 1.)];
        assert_eq!(bar_index(&bars, tr_core::Timeframe::W1, monday + 3 * 86_400), Some(0));
        assert_eq!(bar_index(&bars, tr_core::Timeframe::D1, monday + 3 * 86_400), None);
        assert_eq!(bar_index(&bars, tr_core::Timeframe::MN1, monday + 3 * 86_400), Some(0));
    }
    #[test]
    fn imports_are_bounded_filtered_and_require_attribution() {
        let path = std::env::temp_dir().join(format!("chart-events-{}.json", std::process::id()));
        let event = ChartEvent {
            symbol: "AAPL US Equity".into(),
            ts: 100,
            kind: EventKind::Earnings,
            detail: "Reported earnings".into(),
            source: "Issuer release".into(),
        };
        std::fs::write(&path, serde_json::to_vec(&vec![event.clone()]).unwrap()).unwrap();
        assert_eq!(imported(&path, &event.symbol, 1, 200).unwrap(), vec![event]);
        assert!(imported(&path, "SPY US Equity", 1, 200).unwrap().is_empty());
        std::fs::write(&path, b"broken").unwrap();
        assert!(imported(&path, "AAPL US Equity", 1, 200).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
