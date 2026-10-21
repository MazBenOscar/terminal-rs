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
fn bar_index(bars: &[Bar], tf: tr_core::Timeframe, timestamp: i64) -> Option<usize> {
    let bucket = |ts: i64| if tf.is_intraday() { ts.div_euclid(86_400) } else { tf.bucket_start(ts) };
    let target = bucket(timestamp);
    let i = bars.partition_point(|b| bucket(b.ts) < target);
    (i < bars.len() && bucket(bars[i].ts) == target).then_some(i)
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
        let center =
            pos2(rect.left() + viewport.x_center(index) as f32, rect.bottom() - 12. - (*stack).min(5) as f32 * 19.);
        *stack += 1;
        let (letter, color) = match event.kind {
            EventKind::Earnings => ("E", crate::theme::amber()),
            EventKind::Dividend => ("D", crate::theme::up()),
            EventKind::Split => ("S", crate::theme::cyan()),
        };
        ui.painter().circle_filled(center, 8., crate::theme::card());
        ui.painter().circle_stroke(center, 8., egui::Stroke::new(1., color));
        ui.painter().text(center, Align2::CENTER_CENTER, letter, crate::theme::mono(10.), color);
        ui.interact(
            Rect::from_center_size(center, egui::vec2(18., 18.)),
            ui.id().with(("corp-event", event.ts, &event.detail)),
            egui::Sense::hover(),
        )
        .on_hover_text(format!(
            "{}\n{}\n{}",
            event.detail,
            chrono::DateTime::from_timestamp(event.ts, 0).map(|d| d.to_rfc3339()).unwrap_or_default(),
            event.source
        ));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
