//! Adversarial review tests for A3e (precision levels / trendlines).
use super::precision::*;
use super::tests::btc_fixture;
use tr_core::Bar;

fn atr_last(bars: &[Bar]) -> f64 {
    let o = tr_core::indicators::Ohlcv::new(bars);
    *tr_core::indicators::atr(&o, 14).last().unwrap()
}
fn line_at(l: &Trendline, index: usize) -> f64 {
    let (a, b) = (&l.anchors[0], &l.anchors[1]);
    a.price + (b.price - a.price) * (index as f64 - a.index as f64) / (b.index - a.index) as f64
}

#[test]
fn review_line_broken_after_last_touch() {
    let bars = btc_fixture();
    let atr = tr_core::indicators::atr(&tr_core::indicators::Ohlcv::new(&bars), 14);
    let (active, historical) = trendline_sets(&bars);
    assert!(!active.is_empty() && !historical.is_empty());
    for l in active.iter().chain(&historical) {
        let support = l.role == "rising_support";
        let beyond = |i: usize| {
            if support {
                bars[i].close < line_at(l, i) - 0.35 * atr[i]
            } else {
                bars[i].close > line_at(l, i) + 0.35 * atr[i]
            }
        };
        let count = (l.anchors[1].index + 1..bars.len()).filter(|&i| beyond(i)).count();
        assert_eq!(l.post_touch_closes_beyond, count);
        assert_eq!(l.broken, count > 1 || beyond(bars.len() - 1));
    }
    assert!(active.iter().all(|l| !l.broken));
    assert!(historical.iter().all(|l| l.broken && l.kind == "dashed"));
}

#[test]
fn review_near_duplicate_trendlines() {
    let (mut lines, historical) = trendline_sets(&btc_fixture());
    lines.extend(historical);
    for (i, x) in lines.iter().enumerate() {
        for y in &lines[i + 1..] {
            if x.role == y.role {
                let shared = x.touches.iter().filter(|t| y.touches.iter().any(|u| u.index == t.index)).count();
                assert!(shared < 2, "{} and {} share {shared} touches", x.item_id, y.item_id);
                let slope = |l: &Trendline| {
                    (l.anchors[1].price - l.anchors[0].price) / (l.anchors[1].index - l.anchors[0].index) as f64
                };
                let atr = atr_last(&btc_fixture());
                assert!(
                    (x.projected_price - y.projected_price).abs() >= 0.5 * atr
                        || (slope(x) - slope(y)).abs() * 10. >= 0.5 * atr
                );
            }
        }
    }
}

#[test]
fn review_in_play_edge_pinned_to_close() {
    let bars = btc_fixture();
    let close = bars.last().unwrap().close;
    for z in levels(&bars) {
        assert!(
            z.low != close && z.high != close,
            "{} {} {}–{} has an edge equal to close {close}",
            z.item_id,
            z.role,
            z.low,
            z.high
        );
    }
}

#[test]
fn review_plus10_projection_uses_last_gap() {
    // Equity-like D1 history: weekdays only, ending on a Monday (1970-01-01 was a Thursday).
    let mut bars: Vec<_> = btc_fixture().into_iter().filter(|b| !matches!((b.ts / 86400) % 7, 2 | 3)).collect();
    while (bars.last().unwrap().ts / 86400) % 7 != 4 {
        bars.pop();
    }
    let o = tr_core::indicators::Ohlcv::new(&bars);
    let ranges = tr_core::indicators::true_range(&o);
    let atrs: Vec<_> = tr_core::indicators::atr(&o, 14)
        .into_iter()
        .enumerate()
        .map(|(i, atr)| if atr.is_finite() { atr } else { super::mean(&ranges[i.saturating_sub(13)..=i]) })
        .collect();
    let atr = atr_last(&bars);
    let n = bars.len() as f64;
    let lines = trendlines(&bars);
    assert!(!lines.is_empty());
    for l in lines {
        let (a, b) = (&l.anchors[0], &l.anchors[1]);
        for touch in &l.touches {
            assert!((touch.price - line_at(&l, touch.index)).abs() <= 0.35 * atrs[touch.index]);
        }
        assert!((l.projected_price - line_at(&l, bars.len() - 1)).abs() <= 1e-10 * atr);
        // The chart's x axis is bar-index based, so "+10 bars" is index n-1+10.
        let plus10 = a.price + (b.price - a.price) * (n - 1. + 10. - a.index as f64) / (b.index - a.index) as f64;
        assert!(
            (l.projected_price_plus_10 - plus10).abs() <= 1e-10 * atr,
            "{}: plus_10 {} vs 10 bars ahead {} ({:.2} ATR)",
            l.item_id,
            l.projected_price_plus_10,
            plus10,
            (l.projected_price_plus_10 - plus10).abs() / atr
        );
    }
}

#[test]
fn review_tiny_price_scale_invariance() {
    let bars = btc_fixture();
    let k = 1e-12; // closes ~8e-8
    let tiny: Vec<_> =
        bars.iter().map(|b| Bar::new(b.ts, b.open * k, b.high * k, b.low * k, b.close * k, b.volume)).collect();
    let (a, b) = (levels(&bars), levels(&tiny));
    assert_eq!(a.len(), b.len(), "zone count changes with price scale");
    for (x, y) in a.iter().zip(&b) {
        assert!((x.low * k - y.low).abs() <= 1e-9 * y.low.abs(), "{} vs {}", x.low * k, y.low);
        assert!((x.high * k - y.high).abs() <= 1e-9 * y.high.abs());
        assert_eq!((x.role, x.touches, x.recommended), (y.role, y.touches, y.recommended));
    }
    let (active, historical) = trendline_sets(&bars);
    let (tiny_active, tiny_historical) = trendline_sets(&tiny);
    assert_eq!((active.len(), historical.len()), (tiny_active.len(), tiny_historical.len()));
    for (x, y) in active.iter().chain(&historical).zip(tiny_active.iter().chain(&tiny_historical)) {
        assert_eq!((x.role, x.broken, x.touches.len()), (y.role, y.broken, y.touches.len()));
        assert!((x.projected_price * k - y.projected_price).abs() <= 1e-9 * y.projected_price.abs());
    }
}
