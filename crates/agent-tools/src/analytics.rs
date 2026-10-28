//! Deterministic analytics shared by data tools and future chart integrations.
use serde::Serialize;
use serde_json::{Value, json};
use tr_core::{
    Bar, Timeframe, chart_analytics,
    indicators::{self, Ohlcv, PivotKind},
    session::{Session, Zone},
};

use crate::{ToolResult, args::Mode};

/// Precision analytics v2. All thresholds are volatility-relative, never symbol-specific.
pub mod precision {
    use super::*;

    #[derive(Debug, Clone, Serialize)]
    pub struct Swing {
        pub index: usize,
        pub time: i64,
        pub price: f64,
        pub strength: &'static str,
        pub role: &'static str,
        pub prominence: f64,
        pub confirmed_at: i64,
    }

    fn atrs(bars: &[Bar]) -> Vec<f64> {
        let o = Ohlcv::new(bars);
        let ranges = indicators::true_range(&o);
        indicators::atr(&o, 14)
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let fallback = mean(&ranges[i.saturating_sub(13)..=i]);
                (if v.is_finite() { v } else { fallback }).max(bars[i].close.abs() * 1e-12)
            })
            .collect()
    }

    /// Minor = confirmed 2/2 fractal. Major = 5/5 AND a reversal of at least
    /// 1.5 ATR on BOTH sides to the nearest opposite minor swing.
    pub fn swings(bars: &[Bar]) -> Vec<Swing> {
        if bars.is_empty() {
            return vec![];
        }
        // The final candle is still forming and cannot confirm a swing.
        let confirmed = &bars[..bars.len() - 1];
        let o = Ohlcv::new(confirmed);
        let atr = atrs(bars);
        let (highs, lows) = indicators::fractals(&o, 2);
        let (major_highs, major_lows) = indicators::fractals(&o, 5);
        let mut out = Vec::new();
        for (values, opposite, major, role) in
            [(&highs, &lows, &major_highs, "high"), (&lows, &highs, &major_lows, "low")]
        {
            for (i, &price) in values.iter().enumerate().filter(|(_, p)| p.is_finite()) {
                let left = (0..i).rev().find(|&j| opposite[j].is_finite());
                let right = (i + 1..confirmed.len()).find(|&j| opposite[j].is_finite());
                let prominence = left
                    .zip(right)
                    .map_or(0., |(l, r)| (price - opposite[l]).abs().min((price - opposite[r]).abs()) / atr[i]);
                let is_major = major[i].is_finite() && prominence >= 1.5;
                let confirmation = if is_major { (i + 5).max(right.unwrap() + 2) } else { i + 2 };
                out.push(Swing {
                    index: i,
                    time: bars[i].ts,
                    price,
                    strength: if is_major { "major" } else { "minor" },
                    role,
                    prominence,
                    confirmed_at: bars[confirmation].ts,
                });
            }
        }
        out.sort_by_key(|s| (s.index, s.role));
        out
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct Zone {
        pub item_id: String,
        pub price: f64,
        pub zone_width: f64,
        pub low: f64,
        pub high: f64,
        pub strength: &'static str,
        /// Total of distinct swing bars and non-swing closes which respected the zone.
        pub touches: usize,
        pub swing_touches: usize,
        pub respected_closes: usize,
        pub first_touch_ts: i64,
        pub last_touch_ts: i64,
        pub last_touch_time: i64,
        pub role: &'static str,
        pub side: &'static str,
        pub in_play: bool,
        pub trimmed: bool,
        pub score: f64,
        pub recommended: bool,
    }

    fn role(low: f64, high: f64, close: f64, atr: f64) -> &'static str {
        if close - low >= 0.1 * atr && high - close >= 0.1 * atr {
            "in_play"
        } else if close < (low + high) / 2. {
            "resistance"
        } else {
            "support"
        }
    }

    pub fn levels(bars: &[Bar]) -> Vec<Zone> {
        if bars.is_empty() {
            return vec![];
        }
        let atr = atrs(bars);
        let last_atr = *atr.last().unwrap();
        let close = bars.last().unwrap().close;
        let mut points = swings(bars);
        points.sort_by(|a, b| a.price.total_cmp(&b.price).then(a.index.cmp(&b.index)));
        // Bounded diameter avoids single-link chains. 0.35 ATR is the same
        // touch uncertainty used for lines; full cluster diameter is 0.7 ATR.
        let tolerance = last_atr * 0.35;
        // Tradeable bands: about the width CMC/TradingView-style zones use on D1
        // (0.4–0.6 ATR); wider boxes stop being actionable levels.
        let cap = last_atr * 0.6;
        let mut groups: Vec<Vec<Swing>> = Vec::new();
        // High and low reactions have opposite body intervals. Cluster them
        // independently first; merging wick prices before consulting the bodies
        // erases valid polarity-flip zones in long histories.
        for role in ["high", "low"] {
            let mut side: Vec<Vec<Swing>> = Vec::new();
            for p in points.iter().filter(|p| p.role == role).cloned() {
                if let Some(g) = side.last_mut().filter(|g| p.price - g[0].price <= 2. * tolerance) {
                    g.push(p);
                } else {
                    side.push(vec![p]);
                }
            }
            groups.extend(side);
        }
        // Body-aware touch intervals (wick tip to the nearer body edge).
        // Median endpoints represent where reactions concentrate, rather than
        // allowing one long wick to determine the rectangle's height.
        let bounds = |g: &[Swing]| {
            let mut prices: Vec<_> = g.iter().map(|s| s.price).collect();
            prices.sort_by(f64::total_cmp);
            let median = prices[prices.len() / 2];
            let mut bodies: Vec<_> = g
                .iter()
                .map(|s| {
                    let b = bars[s.index];
                    if s.role == "low" { b.open.min(b.close) } else { b.open.max(b.close) }
                })
                .collect();
            bodies.sort_by(f64::total_cmp);
            // The IQR rejects isolated wick outliers. Bodies can bias the range
            // by one touch tolerance, not drag it across a whole large candle.
            let body = bodies[bodies.len() / 2].clamp(median - tolerance, median + tolerance);
            let low = prices[(prices.len() - 1) / 4].min(body) - tolerance / 2.;
            let high = prices[(prices.len() - 1) * 3 / 4].max(body) + tolerance / 2.;
            if high - low <= cap {
                (low, high)
            } else {
                // Trim to the densest observed swing window, with median tie-breaking.
                let (l, h, _) = capped_zone_bounds(&prices, median, tolerance / 2., cap);
                (l, h)
            }
        };
        // Merge overlapping rectangles before counting/ranking; recompute bounds
        // from all touch intervals, rather than inflating with repeated padding.
        let mut groups: Vec<_> = groups
            .into_iter()
            .map(|g| {
                let (low, high) = bounds(&g);
                (g, low, high)
            })
            .collect();
        groups.sort_by(|a, b| a.1.total_cmp(&b.1));
        let mut i = 0;
        while i + 1 < groups.len() {
            if groups[i].2 >= groups[i + 1].1 {
                let next = groups.remove(i + 1);
                let low = groups[i].1.min(next.1);
                let high = groups[i].2.max(next.2);
                groups[i].0.extend(next.0);
                // Bounds depend only on observed reactions, never the last close.
                let (l, h) = if high - low <= cap { (low, high) } else { bounds(&groups[i].0) };
                groups[i].1 = l;
                groups[i].2 = h;
                i = i.saturating_sub(1);
            } else {
                i += 1;
            }
        }
        let avg_volume = mean(&bars.iter().map(|b| b.volume).collect::<Vec<_>>()).max(1.);
        let mut zones = Vec::new();
        for (g, low, high) in groups {
            let trimmed = g.iter().any(|s| s.price < low || s.price > high);
            // A capped-out swing must not inflate the touch count or strength.
            let g: Vec<_> =
                g.into_iter().filter(|s| s.price >= low - tolerance && s.price <= high + tolerance).collect();
            if g.is_empty() {
                continue;
            }
            let mut indices: Vec<_> = g.iter().map(|s| s.index).collect();
            indices.sort_unstable();
            indices.dedup();
            let first = indices[0];
            let mut last = *indices.last().unwrap();
            let respected: Vec<_> = (first..bars.len())
                .filter(|i| {
                    if indices.binary_search(i).is_ok() {
                        return false;
                    }
                    let b = bars[*i];
                    // A close respects a touched zone only if it exits on the same
                    // side as the previous close; no credit for trading through it.
                    let prev = bars[i.saturating_sub(1)].close;
                    b.low <= high
                        && b.high >= low
                        && ((prev >= high && b.close >= high) || (prev <= low && b.close <= low))
                })
                .collect();
            if let Some(&r) = respected.last() {
                last = last.max(r);
            }
            let swing_touches = indices.len();
            let touches = swing_touches + respected.len();
            let prominence = g.iter().map(|s| s.prominence).fold(0., f64::max);
            let major = g.iter().any(|s| s.strength == "major");
            let volume = mean(&indices.iter().map(|&i| bars[i].volume).collect::<Vec<_>>()) / avg_volume;
            let side = role(low, high, close, last_atr);
            zones.push(Zone {
                item_id: String::new(),
                recommended: false,
                price: (low + high) / 2.,
                zone_width: high - low,
                low,
                high,
                strength: if major { "major" } else { "minor" },
                touches,
                swing_touches,
                respected_closes: respected.len(),
                first_touch_ts: bars[first].ts,
                last_touch_ts: bars[last].ts,
                last_touch_time: bars[last].ts,
                role: side,
                side,
                in_play: side == "in_play",
                trimmed,
                score: 2. * swing_touches as f64
                    + (1. + respected.len() as f64).ln()
                    + 2. * last as f64 / bars.len() as f64
                    + prominence.min(10.)
                    + (1. + volume).ln(),
            });
        }
        let distance = |z: &Zone| (z.low - close).max(close - z.high).max(0.);
        zones.sort_by(|a, b| {
            distance(a).total_cmp(&distance(b)).then(b.score.total_cmp(&a.score)).then(a.low.total_cmp(&b.low))
        });
        // Reserve nearest major each side, then strongest other major/minor
        // candidates independently, not three total near-price slots.
        let mut selected = Vec::new();
        for side in ["support", "resistance", "in_play"] {
            for strength in ["major", "minor"] {
                let candidates: Vec<_> = zones
                    .iter()
                    .enumerate()
                    .filter(|(_, z)| z.role == side && z.strength == strength)
                    .map(|(i, _)| i)
                    .collect();
                if side != "in_play"
                    && strength == "major"
                    && let Some(&i) = candidates.first()
                {
                    selected.push(i);
                }
                // Preserve nearby context at each strength; score breaks distance
                // ties above, and lets the drawing agent choose its strongest six.
                for i in candidates {
                    if selected.iter().filter(|&&j| zones[j].role == side && zones[j].strength == strength).count()
                        >= if side == "in_play" { 1 } else { 3 }
                    {
                        break;
                    }
                    if !selected.contains(&i) {
                        selected.push(i);
                    }
                }
            }
        }
        // Recommend exactly the host's six-zone allowance: in-play, nearest
        // major on each side, then highest score among the remaining candidates.
        let mut recommended: Vec<usize> = selected.iter().copied().filter(|&i| zones[i].in_play).collect();
        for side in ["support", "resistance"] {
            if let Some(&i) = selected.iter().find(|&&i| zones[i].role == side && zones[i].strength == "major") {
                recommended.push(i);
            }
        }
        let mut by_score = selected.clone();
        by_score.sort_by(|&a, &b| zones[b].score.total_cmp(&zones[a].score).then(a.cmp(&b)));
        for i in by_score {
            if recommended.len() >= 6 {
                break;
            }
            if !recommended.contains(&i) {
                recommended.push(i);
            }
        }
        for i in recommended {
            zones[i].recommended = true;
        }
        selected.sort_unstable();
        selected
            .into_iter()
            .enumerate()
            .map(|(id, i)| {
                let mut z = zones[i].clone();
                z.item_id = format!("zone-{id}");
                z
            })
            .collect()
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct Trendline {
        pub item_id: String,
        pub role: &'static str,
        pub anchors: Vec<Swing>,
        pub touches: Vec<Swing>,
        pub projected_price: f64,
        pub projected_price_plus_10: f64,
        pub broken: bool,
        pub kind: &'static str,
        pub score: f64,
        pub closes_beyond: usize,
        pub post_touch_closes_beyond: usize,
    }

    fn line_price(a: &Swing, b: &Swing, index: usize) -> f64 {
        a.price + (b.price - a.price) * (index as f64 - a.index as f64) / (b.index as f64 - a.index as f64)
    }
    fn beyond(close: f64, line: f64, tolerance: f64, support: bool) -> bool {
        if support { close < line - tolerance } else { close > line + tolerance }
    }
    fn validate_line(
        bars: &[Bar],
        atr: &[f64],
        points: &[Swing],
        a: &Swing,
        b: &Swing,
        support: bool,
    ) -> Option<Trendline> {
        if (support && b.price <= a.price) || (!support && b.price >= a.price) {
            return None;
        }
        let touches: Vec<_> = points
            .iter()
            .filter(|p| (p.price - line_price(a, b, p.index)).abs() <= 0.35 * atr[p.index])
            .cloned()
            .collect();
        if touches.len() < 3 {
            return None;
        }
        let first = touches.first().unwrap();
        let last = touches.last().unwrap();
        if last.index * 5 < bars.len() * 3 {
            return None;
        }
        // Re-anchor on the extreme exact swing points and revalidate ALL claimed
        // touches and closes against that final, drawn line (not the candidate).
        if touches.iter().any(|p| (p.price - line_price(first, last, p.index)).abs() > 0.35 * atr[p.index]) {
            return None;
        }
        if (support && last.price <= first.price) || (!support && last.price >= first.price) {
            return None;
        }
        let closes_beyond = (first.index..=last.index)
            .filter(|&i| beyond(bars[i].close, line_price(first, last, i), 0., support))
            .count();
        if closes_beyond > 2 {
            return None;
        }
        let end = bars.last().unwrap();
        let projected_price = line_price(first, last, bars.len() - 1);
        let post_touch_closes_beyond = (last.index + 1..bars.len())
            .filter(|&i| beyond(bars[i].close, line_price(first, last, i), 0.35 * atr[i], support))
            .count();
        let residual =
            touches.iter().map(|p| (p.price - line_price(first, last, p.index)).abs() / atr[p.index]).sum::<f64>()
                / touches.len() as f64;
        let broken =
            post_touch_closes_beyond > 1 || beyond(end.close, projected_price, 0.35 * atr[bars.len() - 1], support);
        // A 20%-of-window half-life makes old touched lines fade continuously.
        // Broken lines remain auditable but rank below comparable active lines.
        let recency = 2_f64.powf(-((bars.len() - 1 - last.index) as f64) / (bars.len() as f64 * 0.2));
        let score = (touches.len() as f64 * 2. + (last.index - first.index) as f64 / bars.len() as f64 * 3. - residual)
            * recency
            * if broken { 0.5 } else { 1. };
        Some(Trendline {
            item_id: String::new(),
            role: if support { "rising_support" } else { "falling_resistance" },
            anchors: vec![first.clone(), last.clone()],
            projected_price,
            projected_price_plus_10: line_price(first, last, bars.len() - 1 + 10),
            broken,
            kind: if !broken && touches.len() >= 4 { "solid" } else { "dashed" },
            score,
            touches,
            closes_beyond,
            post_touch_closes_beyond,
        })
    }
    pub fn trendlines(bars: &[Bar]) -> Vec<Trendline> {
        trendline_sets(bars).0
    }

    /// Historical lines are auditable but never mixed into the active list.
    pub fn trendline_sets(bars: &[Bar]) -> (Vec<Trendline>, Vec<Trendline>) {
        if bars.is_empty() {
            return (vec![], vec![]);
        }
        let atr = atrs(bars);
        let swings = swings(bars);
        let mut out = Vec::new();
        for (role, support) in [("low", true), ("high", false)] {
            let points: Vec<_> = swings.iter().filter(|s| s.role == role).cloned().collect();
            let mut candidates = Vec::new();
            for (i, a) in points.iter().enumerate() {
                for b in &points[i + 1..] {
                    if let Some(line) = validate_line(bars, &atr, &points, a, b, support) {
                        candidates.push(line);
                    }
                }
            }
            candidates.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then(a.anchors[0].index.cmp(&b.anchors[0].index))
                    .then(a.anchors[1].index.cmp(&b.anchors[1].index))
            });
            let mut selected: Vec<Trendline> = Vec::new();
            for line in candidates {
                let slope = |l: &Trendline| {
                    (l.anchors[1].price - l.anchors[0].price) / (l.anchors[1].index - l.anchors[0].index) as f64
                };
                if selected.iter().any(|s| {
                    let shared = s.touches.iter().filter(|t| line.touches.iter().any(|u| t.index == u.index)).count();
                    shared >= 2
                        || ((s.projected_price - line.projected_price).abs() < 0.5 * atr[bars.len() - 1]
                            && (slope(s) - slope(&line)).abs() * 10. < 0.5 * atr[bars.len() - 1])
                }) {
                    continue;
                }
                if selected.iter().filter(|s| s.broken == line.broken).count() < 2 {
                    selected.push(line);
                }
            }
            out.extend(selected);
        }
        let (mut active, mut historical): (Vec<_>, Vec<_>) = out.into_iter().partition(|l| !l.broken);
        for (i, line) in active.iter_mut().enumerate() {
            line.item_id = format!("line-{i}");
        }
        for (i, line) in historical.iter_mut().enumerate() {
            line.item_id = format!("historical-line-{i}");
        }
        (active, historical)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        fn point(i: usize, price: f64) -> Swing {
            Swing {
                index: i,
                time: i as i64 * 86400,
                price,
                strength: "minor",
                role: "low",
                prominence: 2.,
                confirmed_at: (i + 2) as i64 * 86400,
            }
        }
        fn rising() -> (Vec<Bar>, Vec<Swing>) {
            let bars = (0..50)
                .map(|i| {
                    let p = 100. + i as f64;
                    let low = if [5, 20, 35, 45].contains(&i) { p } else { p + 2. };
                    Bar::new(i as i64 * 86400, p + 4., p + 5., low, p + 4., 100.)
                })
                .collect();
            (bars, [5, 20, 35, 45].into_iter().map(|i| point(i, 100. + i as f64)).collect())
        }
        #[test]
        fn line_tolerance_exact_extreme_anchors_and_close_validation() {
            let (mut bars, mut points) = rising();
            let atr = vec![10.; bars.len()];
            points[1].price += 3.49; // within 0.35 ATR, not exactly on the line
            let l = validate_line(&bars, &atr, &points, &points[0], &points[3], true).unwrap();
            assert_eq!(l.kind, "solid");
            assert_eq!(l.anchors[0].price, 105.);
            assert_eq!(l.anchors[1].price, 145.);
            assert_eq!(l.projected_price, 149.);
            assert_eq!(l.projected_price_plus_10, 159.);
            points[1].price += 0.02;
            assert_eq!(validate_line(&bars, &atr, &points, &points[0], &points[3], true).unwrap().kind, "dashed");
            // Wicks do not break the line; closes do, even inside the touch tolerance.
            for i in [10, 11, 12] {
                bars[i].low = 0.;
                bars[i].close = 100. + i as f64 - 0.1;
            }
            assert!(validate_line(&bars, &atr, &points, &points[0], &points[3], true).is_none());
            bars[12].close = 120.;
            assert!(validate_line(&bars, &atr, &points, &points[0], &points[3], true).is_some());
            assert!(validate_line(&bars, &atr, &points, &points[3], &points[0], true).is_none());
        }
        #[test]
        fn line_rejects_stale_last_touch_wrong_slope_and_too_few_touches() {
            let (bars, points) = rising();
            let atr = vec![10.; bars.len()];
            assert!(validate_line(&bars, &atr, &points[..2], &points[0], &points[1], true).is_none());
            assert!(validate_line(&bars, &atr, &points, &points[0], &points[3], false).is_none());
            let stale: Vec<_> = [2, 10, 20].into_iter().map(|i| point(i, 100. + i as f64)).collect();
            assert!(validate_line(&bars, &atr, &stale, &stale[0], &stale[2], true).is_none());
        }
        #[test]
        fn post_touch_breaks_survive_recovery_and_one_breach_is_allowed() {
            let (mut bars, points) = rising();
            let atr = vec![10.; bars.len()];
            bars[46].close = 100.;
            let validate = |bars: &[Bar]| validate_line(bars, &atr, &points, &points[0], &points[3], true).unwrap();
            assert!(!validate(&bars).broken, "one tolerated historical breach, last close recovered");
            bars[47].close = 100.;
            let line = validate(&bars);
            assert!(line.broken, "two breaches break even after recovery");
            assert_eq!(line.post_touch_closes_beyond, 2);
            assert_eq!(line.kind, "dashed");
            bars[46].close = 150.;
            bars[47].close = 150.;
            bars[49].close = 100.;
            assert!(validate(&bars).broken, "a final-close breach always breaks");
            bars[49].close = 149. - 3.49;
            assert!(!validate(&bars).broken, "0.35 ATR is the breach tolerance");
        }

        #[test]
        fn plateaus_and_opposite_confirmation_exclude_the_forming_bar() {
            let bars: Vec<_> = [10., 11., 15., 15., 11., 10., 9., 10.]
                .into_iter()
                .enumerate()
                .map(|(i, p)| Bar::new(i as i64, p, p + 1., p - 1., p, 1.))
                .collect();
            let points = swings(&bars);
            assert!(points.iter().any(|s| s.role == "high" && s.index == 2));
            assert!(!points.iter().any(|s| s.index == 3));
            assert!(!points.iter().any(|s| s.index + 2 >= bars.len() - 1));
            let bars = super::super::tests::btc_fixture();
            let points = swings(&bars);
            for s in points.iter().filter(|s| s.strength == "major") {
                let opposite = points.iter().find(|o| o.index > s.index && o.role != s.role).unwrap();
                assert!(s.confirmed_at >= bars[opposite.index + 2].ts);
                assert!(s.confirmed_at < bars.last().unwrap().ts);
            }
        }

        #[test]
        fn recommendations_reserve_in_play_and_nearest_majors_then_score() {
            let bars = super::super::tests::btc_fixture();
            let zones = levels(&bars);
            assert_eq!(zones.iter().filter(|z| z.recommended).count(), 6);
            for side in ["support", "resistance"] {
                assert!(zones.iter().find(|z| z.role == side && z.strength == "major").unwrap().recommended);
            }
            assert_eq!(role(99., 101., 100., 2.), "in_play");
            assert_eq!(role(99., 101., 99.1, 2.), "resistance");
            assert_eq!(role(99., 101., 100.9, 2.), "support");
            assert!(zones.iter().filter(|z| z.in_play).all(|z| z.recommended));
            let reserved = |z: &Zone| {
                z.in_play
                    || ["support", "resistance"].iter().any(|&side| {
                        zones
                            .iter()
                            .find(|z| z.role == side && z.strength == "major")
                            .is_some_and(|r| r.item_id == z.item_id)
                    })
            };
            for z in zones.iter().filter(|z| z.recommended && !reserved(z)) {
                assert!(zones.iter().filter(|z| !z.recommended).all(|other| other.score <= z.score));
            }
        }

        #[test]
        fn clusters_are_scaled_capped_deduplicated_and_count_distinct_bars() {
            let bars = tr_core::sample::generate("PRECISION", Timeframe::D1, 400, 1_700_000_000);
            let zones = levels(&bars);
            let atr = atrs(&bars);
            assert!(!zones.is_empty());
            for z in &zones {
                assert!(z.low < z.high && z.high - z.low <= 0.6 * atr.last().unwrap() + 1e-9);
                assert_eq!(z.touches, z.swing_touches + z.respected_closes);
                assert!(z.first_touch_ts <= z.last_touch_ts);
                assert_eq!(z.role, role(z.low, z.high, bars.last().unwrap().close, *atr.last().unwrap()));
            }
            for (i, z) in zones.iter().enumerate() {
                for other in &zones[i + 1..] {
                    assert!(z.high < other.low || other.high < z.low, "overlapping zones");
                }
            }
            let scaled: Vec<_> = bars
                .iter()
                .map(|b| Bar::new(b.ts, b.open * 10., b.high * 10., b.low * 10., b.close * 10., b.volume))
                .collect();
            let other = levels(&scaled);
            assert_eq!(zones.len(), other.len());
            // Rank ties may order differently after rounding; compare as price-sorted sets.
            let by_low = |z: &[Zone]| {
                let mut v = z.to_vec();
                v.sort_by(|a, b| a.low.total_cmp(&b.low));
                v
            };
            for (a, b) in by_low(&zones).iter().zip(&by_low(&other)) {
                assert!((a.low * 10. - b.low).abs() <= 1e-9 * b.low.abs().max(1.), "{} vs {}", a.low * 10., b.low);
                assert_eq!(a.touches, b.touches);
            }
            assert!(levels(&[]).is_empty());
            assert!(trendlines(&[]).is_empty());
        }
        #[test]
        fn btc_exhaustive_reference_disagreement_is_not_a_ranking_artifact() {
            let bars = super::super::tests::btc_fixture();
            let atr = atrs(&bars);
            let lows: Vec<_> = swings(&bars).into_iter().filter(|s| s.role == "low").collect();
            let mut nearest: Option<f64> = None;
            for (i, a) in lows.iter().enumerate() {
                for b in &lows[i + 1..] {
                    if let Some(l) = validate_line(&bars, &atr, &lows, a, b, true) {
                        assert!((l.projected_price / 80900. - 1.).abs() > 0.01);
                        if nearest.is_none_or(|p| (l.projected_price - 80900.).abs() < (p - 80900.).abs()) {
                            nearest = Some(l.projected_price);
                        }
                    }
                }
            }
            assert!((nearest.unwrap() - 82284.89424242426).abs() < 1e-7);
        }
        #[test]
        fn strengths_require_both_fractal_confirmation_and_atr_prominence() {
            let bars = super::super::tests::btc_fixture();
            let atr = atrs(&bars);
            let (h, l) = indicators::fractals(&Ohlcv::new(&bars), 5);
            let points = swings(&bars);
            assert!(points.iter().any(|s| s.strength == "major"));
            assert!(points.iter().any(|s| s.strength == "minor"));
            for s in points.iter().filter(|s| s.strength == "major") {
                assert!(s.prominence >= 1.5 && atr[s.index] > 0.);
                assert!(if s.role == "low" { l[s.index].is_finite() } else { h[s.index].is_finite() });
                assert!(s.confirmed_at >= bars[s.index + 5].ts);
            }
        }
    }
}

pub(crate) fn finite(value: f64, field: &str) -> Result<f64, ToolResult> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ToolResult::error("invalid_data", "Computed number is not finite", Some(field)))
    }
}

/// Scale before summing so a representable mean does not need a representable sum.
pub(crate) fn mean(values: &[f64]) -> f64 {
    let scale = values.iter().map(|v| v.abs()).fold(0., f64::max);
    if scale == 0. { 0. } else { (values.iter().map(|v| v / scale).sum::<f64>() / values.len() as f64) * scale }
}

fn computed_last(values: &[f64], ready: bool, field: &str) -> Result<Option<f64>, ToolResult> {
    if ready { values.last().map(|&v| finite(v, field)).transpose() } else { Ok(None) }
}

#[derive(Debug, Clone, Serialize)]
pub struct SupportResistance {
    pub price: f64,
    pub zone_width: f64,
    pub low: f64,
    pub high: f64,
    pub touches: usize,
    pub last_touch_time: i64,
    pub score: f64,
    pub in_play: bool,
    pub trimmed: bool,
    pub side: &'static str,
}

/// Cluster confirmed two-sided fractal highs/lows within 0.35 ATR(14). Clusters have
/// bounded diameter (no single-link chaining). Touches count distinct swing bars;
/// ranking combines touch count, normalized recency and relative touched-bar volume.
pub fn support_resistance(bars: &[Bar], top: usize) -> Vec<SupportResistance> {
    if bars.is_empty() || top == 0 {
        return Vec::new();
    }
    let close = bars.last().unwrap().close;
    let o = Ohlcv::new(bars);
    let atr = last_finite(&indicators::atr(&o, 14))
        .unwrap_or_else(|| indicators::true_range(&o).iter().sum::<f64>() / bars.len() as f64);
    let tolerance = (atr * 0.35).max(bars.last().unwrap().close.abs() * 1e-8).max(1e-8);
    let (highs, lows) = indicators::fractals(&o, 2);
    let mut candidates: Vec<_> = highs
        .iter()
        .zip(&lows)
        .enumerate()
        .flat_map(|(i, (&h, &l))| [(h, i), (l, i)].into_iter().filter(|(p, _)| p.is_finite()))
        .collect();
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mean_volume = (bars.iter().map(|b| b.volume).sum::<f64>() / bars.len() as f64).max(1.);
    let mut out = Vec::new();
    let mut start = 0;
    while start < candidates.len() {
        let mut end = start + 1;
        while end < candidates.len() && candidates[end].0 - candidates[start].0 <= 2. * tolerance {
            end += 1;
        }
        let cluster = &candidates[start..end];
        let mut indices: Vec<_> = cluster.iter().map(|c| c.1).collect();
        indices.sort_unstable();
        indices.dedup();
        let last = *indices.last().unwrap();
        let volume = indices.iter().map(|&i| bars[i].volume).sum::<f64>() / indices.len() as f64;
        let touches = indices.len();
        let price = cluster.iter().map(|c| c.0).sum::<f64>() / cluster.len() as f64;
        // A skewed cluster need not be centered on its mean: retain every touch
        // while padding each side to at least the tolerance around price.
        let touch_prices: Vec<_> = cluster.iter().map(|c| c.0).collect();
        let cap = (1.5 * atr).max(2. * tolerance);
        let (low, high, trimmed) = capped_zone_bounds(&touch_prices, price, tolerance, cap);
        let zone_width = high - low;
        out.push(SupportResistance {
            price,
            zone_width,
            low,
            high,
            touches,
            last_touch_time: bars[last].ts,
            score: touches as f64 + last as f64 / bars.len() as f64 + (1. + volume / mean_volume).ln(),
            in_play: low <= close && close <= high,
            trimmed,
            side: if high < close {
                "support"
            } else if low > close {
                "resistance"
            } else {
                "in_play"
            },
        });
        start = end;
    }
    let distance = |zone: &SupportResistance| (zone.low - close).max(close - zone.high).max(0.);
    out.sort_by(|a, b| {
        distance(a).total_cmp(&distance(b)).then(b.score.total_cmp(&a.score)).then(a.price.total_cmp(&b.price))
    });
    // Reserve the nearest zone on each side before filling the remaining slots.
    let support = out.iter().position(|z| z.high < close);
    let resistance = out.iter().position(|z| z.low > close);
    let mut selected: Vec<_> = out.iter().enumerate().filter_map(|(i, z)| z.in_play.then_some(i)).take(3).collect();
    for i in [support, resistance].into_iter().flatten() {
        let on_side = |z: &SupportResistance| if out[i].high < close { z.low <= close } else { z.high >= close };
        if !selected.contains(&i) && selected.iter().filter(|&&j| on_side(&out[j])).count() < 3 {
            selected.push(i);
        }
    }
    let mut below = selected.iter().filter(|&&i| out[i].low <= close).count();
    let mut above = selected.iter().filter(|&&i| out[i].high >= close).count();
    for (i, zone) in out.iter().enumerate() {
        if selected.len() >= top.max(2) {
            break;
        }
        if selected.contains(&i) {
            continue;
        }
        if (zone.low <= close && below >= 3) || (zone.high >= close && above >= 3) {
            continue;
        }
        selected.push(i);
        below += usize::from(zone.low <= close);
        above += usize::from(zone.high >= close);
    }
    selected.sort_unstable();
    selected.into_iter().map(|i| out[i].clone()).collect()
}

/// Trim padding first. If touches themselves exceed the cap, center the cap on
/// the densest window of touches (ties favor the median), rather than on outliers.
fn capped_zone_bounds(prices: &[f64], price: f64, tolerance: f64, cap: f64) -> (f64, f64, bool) {
    let low = prices[0].min(price - tolerance);
    let high = prices[prices.len() - 1].max(price + tolerance);
    if high - low <= cap {
        return (low, high, false);
    }
    let span = prices[prices.len() - 1] - prices[0];
    let center = if span <= cap {
        prices[prices.len() / 2]
    } else {
        let (start, end) = (0..prices.len())
            .map(|i| {
                let end = prices.partition_point(|p| *p <= prices[i] + cap);
                (i, end)
            })
            .max_by_key(|&(i, end)| (end - i, std::cmp::Reverse(i.abs_diff(prices.len() / 2))))
            .unwrap();
        prices[(start + end - 1) / 2]
    };
    let mut low = (center - cap / 2.).clamp(low, high - cap);
    if span <= cap {
        low = low.clamp(prices[prices.len() - 1] - cap, prices[0]);
    }
    let high = low + cap;
    let trimmed = prices.iter().any(|p| *p < low || *p > high);
    (low, high, trimmed)
}

/// Largest-triangle-three-buckets on time/close. Thresholds below two still retain
/// both endpoints for a nontrivial series. Empty buckets never fabricate points.
pub fn lttb(bars: &[Bar], threshold: usize) -> Vec<(i64, f64)> {
    let threshold = threshold.max(2);
    if bars.len() <= threshold {
        return bars.iter().map(|b| (b.ts, b.close)).collect();
    }
    let last = bars.len() - 1;
    let mut out = vec![(bars[0].ts, bars[0].close)];
    if threshold == 2 {
        out.push((bars[last].ts, bars[last].close));
        return out;
    }
    let bucket = (bars.len() - 2) as f64 / (threshold - 2) as f64;
    let mut a = 0;
    for i in 0..threshold - 2 {
        let avg_start = (((i + 1) as f64 * bucket).floor() as usize + 1).min(last);
        let avg_end = (((i + 2) as f64 * bucket).floor() as usize + 1).min(bars.len());
        let avg = &bars[avg_start..avg_end];
        let x = avg.iter().map(|b| (b.ts - bars[0].ts) as f64).sum::<f64>() / avg.len() as f64;
        let y = avg.iter().map(|b| b.close).sum::<f64>() / avg.len() as f64;
        let start = (i as f64 * bucket).floor() as usize + 1;
        let end = (((i + 1) as f64 * bucket).floor() as usize + 1).min(last);
        let ax = (bars[a].ts - bars[0].ts) as f64;
        a = (start..end)
            .max_by(|&j, &k| {
                let area = |index: usize| {
                    ((ax - x) * (bars[index].close - bars[a].close)
                        - (ax - (bars[index].ts - bars[0].ts) as f64) * (y - bars[a].close))
                        .abs()
                };
                area(j).total_cmp(&area(k)).then(k.cmp(&j))
            })
            .unwrap_or(start);
        out.push((bars[a].ts, bars[a].close));
    }
    out.push((bars[last].ts, bars[last].close));
    out
}

pub(crate) fn last_finite(values: &[f64]) -> Option<f64> {
    values.last().copied().filter(|v| v.is_finite())
}

pub(crate) fn analyze(
    bars: &[Bar],
    tf: Timeframe,
    session: Session,
    mode: Mode,
    top: usize,
) -> Result<Value, ToolResult> {
    let o = Ohlcv::new(bars);
    let value = match mode {
        Mode::Swings => {
            let input = indicators::registry::Input::new(bars, tf).with_session(session);
            let fractals = indicators::registry::Spec::new("FRACTALS", &[]).unwrap().compute(&input);
            let zigzag = indicators::registry::Spec::new("ZIGZAG", &[]).unwrap().compute(&input);
            let points = |values: &[f64]| -> Vec<Value> {
                values
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.is_finite())
                    .map(|(i, v)| json!({"time":bars[i].ts,"price":v,"confirmed_at":bars.get(i+2).map(|b|b.ts)}))
                    .collect()
            };
            let line: Vec<_> = bars
                .iter()
                .zip(&zigzag[0].values)
                .filter_map(|(b, v)| v.is_finite().then_some(Bar { close: *v, ..*b }))
                .collect();
            json!({"fractal_periods":2,"highs":points(&fractals[0].values),"lows":points(&fractals[1].values),
                "zigzag":{"deviation_percent":5,"last_leg_repaints":true,"series":lttb(&line,300)}})
        }
        Mode::Trend => trend(bars, &o)?,
        Mode::Trendlines => {
            let (lines, historical) = precision::trendline_sets(bars);
            for line in lines.iter().chain(&historical) {
                for number in [line.projected_price, line.projected_price_plus_10, line.score] {
                    finite(number, "trendlines")?;
                }
                for touch in &line.touches {
                    finite(touch.prominence, "trendlines.touches.prominence")?;
                }
            }
            json!({"trendlines":lines,"historical":historical})
        }
        Mode::Levels => {
            let session_key = |ts| {
                let ts = if session != Session::Provider && tf.is_intraday() {
                    Zone::NewYork.local(ts).and_utc().timestamp()
                } else {
                    ts
                };
                indicators::session_key(tf)(ts)
            };
            let ready = bars.windows(2).any(|w| session_key(w[0].ts) != session_key(w[1].ts));
            let mut pivots = serde_json::Map::new();
            for (name, values) in indicators::pivots(&o, session_key, PivotKind::Classic) {
                pivots.insert(name.into(), json!(computed_last(&values, ready, &format!("pivots.{name}"))?));
            }
            let levels = support_resistance(bars, top);
            for (i, level) in levels.iter().enumerate() {
                for (name, value) in [
                    ("price", level.price),
                    ("zone_width", level.zone_width),
                    ("low", level.low),
                    ("high", level.high),
                    ("score", level.score),
                ] {
                    finite(value, &format!("support_resistance[{i}].{name}"))?;
                }
            }
            json!({"pivots":pivots,"pivot_kind":"classic","pivot_session":"core timeframe session key",
                "support_resistance":levels})
        }
        Mode::VolumeProfile => {
            let profile = chart_analytics::volume_profile(bars, 48)
                .map(|p| {
                    finite(p.step, "profile.step")?;
                    for &volume in &p.volume {
                        finite(volume, "profile.volume")?;
                    }
                    finite(p.volume.iter().sum(), "profile.total_volume")?;
                    let poc = finite(p.low + (p.poc as f64 + 0.5) * p.step, "profile.poc")?;
                    let low = finite(p.low + p.value_low as f64 * p.step, "profile.value_area.low")?;
                    let high = finite(p.low + (p.value_high + 1) as f64 * p.step, "profile.value_area.high")?;
                    Ok(json!({"poc":poc,"value_area":{"low":low,"high":high,"fraction":0.7},
                    "bins":p.volume.len(),"estimated_from_ohlcv":true}))
                })
                .transpose()?;
            // Keep core's app behaviour unchanged. Normalizing both units here
            // avoids overflow in its typical-price sum and price*volume products.
            let price_scale = bars
                .iter()
                .flat_map(|b| [b.high.abs(), b.low.abs(), b.close.abs()])
                .fold(0., f64::max)
                .max(f64::MIN_POSITIVE);
            let volume_scale = bars.iter().map(|b| b.volume).fold(0., f64::max);
            let vwap = if volume_scale == 0. {
                None
            } else {
                let scaled: Vec<_> = bars
                    .iter()
                    .map(|b| Bar {
                        high: b.high / price_scale,
                        low: b.low / price_scale,
                        close: b.close / price_scale,
                        volume: b.volume / volume_scale,
                        ..*b
                    })
                    .collect();
                let values = chart_analytics::anchored_vwap(&scaled, bars[0].ts);
                Some(finite(values.last().unwrap() * price_scale, "anchored_vwap")?)
            };
            json!({"profile":profile,"anchored_vwap":vwap,"vwap_anchor":bars.first().map(|b|b.ts)})
        }
        Mode::Volatility => {
            let logs: Vec<_> = bars
                .windows(2)
                .filter(|w| w[0].close > 0. && w[1].close > 0.)
                .map(|w| w[1].close.ln() - w[0].close.ln())
                .collect();
            let realized = if logs.len() < 2 {
                None
            } else {
                let mean = mean(&logs);
                Some(finite(
                    (logs.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (logs.len() - 1) as f64).sqrt(),
                    "realized_volatility_per_bar",
                )?)
            };
            let mut ranges: Vec<_> =
                bars.iter().map(|b| finite(b.high - b.low, "range_percentiles")).collect::<Result<_, _>>()?;
            let atr = computed_last(&indicators::atr(&o, 14), bars.len() >= 14, "atr")?;
            ranges.sort_by(f64::total_cmp);
            let percentile =
                |q: f64| ranges.get(((ranges.len().saturating_sub(1)) as f64 * q).round() as usize).copied();
            json!({"atr_period":14,"atr":atr,"realized_volatility_per_bar":realized,
                "log_return_count":logs.len(),"range_percentiles":{"p10":percentile(0.1),"p50":percentile(0.5),"p90":percentile(0.9)}})
        }
    };
    Ok(value)
}

fn trend(bars: &[Bar], o: &Ohlcv) -> Result<Value, ToolResult> {
    if bars.len() < 2 {
        return Ok(
            json!({"direction":"insufficient_data","slope_per_bar":null,"r_squared":null,"higher_highs":0,"higher_lows":0}),
        );
    }
    let n = bars.len() as f64;
    let mx = (n - 1.) / 2.;
    let max_price = bars.iter().map(|b| b.close.abs()).fold(0., f64::max).max(f64::MIN_POSITIVE);
    // Power-of-two scaling preserves the original significands exactly.
    let scale = 2_f64.powi((max_price.log2().floor() as i32).clamp(-1022, 1023));
    let prices: Vec<_> = bars.iter().map(|b| b.close / scale).collect();
    let my = prices.iter().sum::<f64>() / n;
    let xx = (0..bars.len()).map(|i| (i as f64 - mx).powi(2)).sum::<f64>();
    let xy = prices.iter().enumerate().map(|(i, p)| (i as f64 - mx) * (p - my)).sum::<f64>();
    let yy = prices.iter().map(|p| (p - my).powi(2)).sum::<f64>();
    let slope = finite((xy / xx) * scale, "slope_per_bar")?;
    let r_squared = finite(if yy > 0. { (xy * xy / (xx * yy)).clamp(0., 1.) } else { 1. }, "r_squared")?;
    let (highs, lows) = indicators::fractals(o, 2);
    let higher = |v: Vec<f64>| {
        v.into_iter().filter(|v| v.is_finite()).collect::<Vec<_>>().windows(2).filter(|w| w[1] > w[0]).count()
    };
    Ok(
        json!({"direction":if slope.abs() <= (my.abs()*scale).max(1.)*1e-10 {"sideways"} else if slope>0. {"up"} else {"down"},
        "slope_per_bar":slope,"r_squared":r_squared,
        "higher_highs":higher(highs),"higher_lows":higher(lows)}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn btc_fixture() -> Vec<Bar> {
        parse_fixture(include_str!("../tests/fixtures/btcusd_coinbase_1D.csv"))
    }

    fn parse_fixture(csv: &str) -> Vec<Bar> {
        csv.lines()
            .skip(1)
            .filter(|l| !l.is_empty())
            .map(|l| {
                let p: Vec<_> = l.split(',').collect();
                Bar::new(
                    p[0].parse().unwrap(),
                    p[1].parse().unwrap(),
                    p[2].parse().unwrap(),
                    p[3].parse().unwrap(),
                    p[4].parse().unwrap(),
                    p[5].parse().unwrap(),
                )
            })
            .collect()
    }

    fn print_fixture_tables(tf: &str, bars: &[Bar]) {
        let date = |ts| chrono::DateTime::from_timestamp(ts, 0).unwrap().format("%Y-%m-%d %H:%M").to_string();
        println!(
            "FIXTURE {tf} bars={} last={} close={:.2}",
            bars.len(),
            date(bars.last().unwrap().ts),
            bars.last().unwrap().close
        );
        for z in precision::levels(bars) {
            println!(
                "FIXTURE_ZONE | {tf} | {} | {} / {} | {:.2}–{:.2} | {}+{}={} | {:.4} | {} | {} |",
                z.item_id,
                z.role,
                z.strength,
                z.low,
                z.high,
                z.swing_touches,
                z.respected_closes,
                z.touches,
                z.score,
                z.trimmed,
                z.recommended
            );
        }
        let (active, historical) = precision::trendline_sets(bars);
        for (state, lines) in [("active", active), ("historical", historical)] {
            for l in lines {
                println!(
                    "FIXTURE_LINE | {tf} | {state} | {} | {} | {} {:.2} → {} {:.2} | {} / {} | {:.2} | {:.2} | {} |",
                    l.item_id,
                    l.role,
                    date(l.anchors[0].time),
                    l.anchors[0].price,
                    date(l.anchors[1].time),
                    l.anchors[1].price,
                    l.touches.len(),
                    l.kind,
                    l.projected_price,
                    l.projected_price_plus_10,
                    l.post_touch_closes_beyond
                );
            }
        }
    }

    #[test]
    fn precision_h4_fixture_report() {
        let all = parse_fixture(include_str!("../tests/fixtures/btcusd_coinbase_4H.csv"));
        let bars = &all[all.len().saturating_sub(1000)..];
        print_fixture_tables("4H", bars);
        let zones = precision::levels(bars);
        assert_eq!(zones.iter().filter(|z| z.recommended).count(), zones.len().min(6));
        let atr = *indicators::atr(&Ohlcv::new(bars), 14).last().unwrap();
        let close = bars.last().unwrap().close;
        let in_play = zones.iter().find(|z| z.in_play).unwrap();
        assert!(in_play.recommended && close - in_play.low >= 0.1 * atr && in_play.high - close >= 0.1 * atr);
        let value = analyze(bars, Timeframe::H4, Session::Provider, Mode::Trendlines, 10).unwrap();
        assert!(value["trendlines"].as_array().unwrap().iter().all(|l| l["broken"] == false));
        assert!(value["historical"].as_array().unwrap().iter().all(|l| l["broken"] == true));
    }

    #[test]
    fn precision_btc_fixture_report_and_reference_comparison() {
        let bars = btc_fixture();
        let zones = precision::levels(&bars);
        let lines = precision::trendlines(&bars);
        println!(
            "PRECISION last={} close={} bars={} range={}..{}",
            bars.last().unwrap().ts,
            bars.last().unwrap().close,
            bars.len(),
            bars[0].ts,
            bars.last().unwrap().ts
        );
        print_fixture_tables("D1", &bars);
        let date = |ts| chrono::DateTime::from_timestamp(ts, 0).unwrap().format("%Y-%m-%d").to_string();
        for z in &zones {
            println!(
                "ZONE_TABLE | {} | {} / {} | {:.2}–{:.2} | {}+{}={} | {} → {} | {:.4} | {} |",
                z.item_id,
                z.role,
                z.strength,
                z.low,
                z.high,
                z.swing_touches,
                z.respected_closes,
                z.touches,
                date(z.first_touch_ts),
                date(z.last_touch_ts),
                z.score,
                z.trimmed
            );
        }
        for l in &lines {
            println!(
                "LINE_TABLE | {} | {} | {} {:.2} → {} {:.2} | {} / {} | {:.2} | {:.2} | {} | {} |",
                l.item_id,
                l.role,
                date(l.anchors[0].time),
                l.anchors[0].price,
                date(l.anchors[1].time),
                l.anchors[1].price,
                l.touches.len(),
                l.kind,
                l.projected_price,
                l.projected_price_plus_10,
                l.broken,
                l.closes_beyond
            );
        }
        for (low, high, side, major) in [
            (84450., 85250., "resistance", true),
            (82150., 82900., "resistance", false),
            // Touch-derived capped bounds are below the close, not an in-play
            // band fabricated by pinning its high edge to the current price.
            (80000., 80900., "support", false),
            (76400., 77150., "support", true),
        ] {
            assert!(
                zones.iter().any(|z| z.low <= high * 1.006
                    && z.high >= low * 0.994
                    && z.role == side
                    && (!major || z.strength == "major")),
                "reference {low}–{high} {side} missing"
            );
        }
        let swings = precision::swings(&bars);
        for line in &lines {
            assert!(
                line.anchors
                    .iter()
                    .all(|a| swings.iter().any(|s| s.time == a.time && s.price == a.price && s.role == a.role))
            );
        }
        assert_eq!(serde_json::to_value(&zones).unwrap(), serde_json::to_value(precision::levels(&bars)).unwrap());
        for z in &zones {
            assert_eq!(z.touches, z.swing_touches + z.respected_closes);
        }
    }

    #[test]
    #[ignore = "Reference differs from Coinbase D1: no valid 3-touch rising line projects within 1% of 80,900; nearest candidate 82,284.894242, broken. See A3e report."]
    fn precision_reference_rising_support_80900() {
        let lines = precision::trendlines(&btc_fixture());
        assert!(lines.iter().any(|l| l.role == "rising_support" && (l.projected_price / 80900. - 1.).abs() <= 0.01));
    }

    #[test]
    fn level_output_exposes_zone_bounds_and_keeps_existing_fields() {
        let bars = tr_core::sample::generate("ZONES", Timeframe::D1, 400, 1_700_000_000);
        let value = analyze(&bars, Timeframe::D1, Session::Provider, Mode::Levels, 10).unwrap();
        let levels = value["support_resistance"].as_array().unwrap();
        assert!(!levels.is_empty());
        for level in levels {
            let price = level["price"].as_f64().unwrap();
            let width = level["zone_width"].as_f64().unwrap();
            assert!(width > 0.);
            let low = level["low"].as_f64().unwrap();
            let high = level["high"].as_f64().unwrap();
            assert!(low < price && price < high);
            assert_eq!(width, high - low);
            for field in ["touches", "last_touch_time", "score"] {
                assert!(level[field].is_number(), "{field}");
            }
        }
        assert!(support_resistance(&[], 10).is_empty());
    }

    #[test]
    fn lopsided_zone_contains_every_touch_and_tolerance_padding() {
        let bars: Vec<_> = (0..25)
            .map(|i| {
                let high = match i {
                    2 | 7 | 12 => 10.,
                    17 => 12.,
                    _ => 9.,
                };
                Bar::new(i * 86400, 8., high, 6., 8., 1.)
            })
            .collect();
        let levels = support_resistance(&bars, 10);
        let level = levels.iter().find(|level| level.price == 10.5).expect("10,10,10,12 cluster");
        assert_eq!(level.touches, 4);
        for touch in [10., 10., 10., 12.] {
            assert!((level.low..=level.high).contains(&touch));
        }
        let tolerance = last_finite(&indicators::atr(&Ohlcv::new(&bars), 14)).unwrap() * 0.35;
        assert!(level.low <= level.price - tolerance);
        assert!(level.high >= level.price + tolerance);
        assert_eq!(level.zone_width, level.high - level.low);
        assert!(level.score.is_finite());
    }

    #[test]
    fn zones_include_in_play_and_nearest_sides_in_distance_order_with_height_cap() {
        let mut bars = tr_core::sample::generate("ZONE-ORDER", Timeframe::W1, 500, 1_700_000_000);
        let original = support_resistance(&bars, 100);
        assert!(!original.is_empty());
        bars.last_mut().unwrap().close = original[0].price;
        let close = bars.last().unwrap().close;
        let zones = support_resistance(&bars, 100);
        assert!(zones.iter().any(|z| z.in_play && z.low <= close && close <= z.high));
        let distance = |z: &SupportResistance| (z.low - close).max(close - z.high).max(0.);
        assert!(zones.windows(2).all(|w| distance(&w[0]) <= distance(&w[1])));
        assert!(zones.iter().filter(|z| z.low <= close).count() <= 3);
        assert!(zones.iter().filter(|z| z.high >= close).count() <= 3);
        let atr = last_finite(&indicators::atr(&Ohlcv::new(&bars), 14)).unwrap();
        let tolerance = (atr * 0.35).max(close.abs() * 1e-8).max(1e-8);
        for z in &zones {
            assert!(z.high - z.low <= (1.5 * atr).max(2. * tolerance) + 1e-9);
        }
        let limited = support_resistance(&bars, 1);
        for side in ["support", "resistance"] {
            if let Some(nearest) = zones.iter().find(|z| z.side == side) {
                assert!(
                    limited.iter().any(|z| z.price == nearest.price),
                    "nearest {side} cannot be displaced by touch score or top"
                );
            }
        }
        assert!(limited.iter().any(|z| z.in_play));
    }

    #[test]
    fn capped_zones_trim_padding_first_then_prefer_dense_touches_over_outliers() {
        let (low, high, trimmed) = capped_zone_bounds(&[10., 10., 14.], 11.333333, 2., 4.);
        assert!(!trimmed);
        assert_eq!(high - low, 4.);
        assert!(low <= 10. && high >= 14.);
        let (low, high, trimmed) = capped_zone_bounds(&[0., 10., 10.1, 10.2, 100.], 26.06, 1., 3.);
        assert!(trimmed);
        assert_eq!(high - low, 3.);
        assert!(low <= 10. && high >= 10.2);
        assert!(low > 0. && high < 100.);
    }

    #[test]
    fn review_trimmed_only_when_touches_are_excluded() {
        let (low, high, trimmed) = capped_zone_bounds(&[10., 10., 14.], 11.333333, 2., 4.);
        assert!(low <= 10. && high >= 14., "all touches inside");
        assert!(!trimmed, "trimmed must report excluded touches, not removed padding");
    }

    #[test]
    fn lttb_keeps_endpoints_and_point_count() {
        let bars = tr_core::sample::generate("LTTB", Timeframe::M1, 1000, 1_700_000_000);
        for count in [2, 3, 17, 299, 300, 999, 1000, 1001] {
            let points = lttb(&bars, count);
            assert_eq!(points.len(), count.min(bars.len()));
            assert_eq!(points[0], (bars[0].ts, bars[0].close));
            assert_eq!(*points.last().unwrap(), (bars[999].ts, bars[999].close));
            assert!(points.windows(2).all(|w| w[0].0 < w[1].0));
        }
        assert!(lttb(&[], 300).is_empty());
        assert_eq!(lttb(&bars[..1], 300).len(), 1);
    }
}

#[cfg(test)]
#[path = "review_a3e_tests.rs"]
mod review_a3e_tests;
