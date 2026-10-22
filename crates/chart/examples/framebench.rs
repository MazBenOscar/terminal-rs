//! Interactive-style frame-time benchmark: a scripted zoom-out/pan/zoom-in session over
//! a 1M-bar series, rendered offscreen at 1600x900. Prints percentiles per phase.
//!
//! `cargo run --release -p tr-chart --example framebench [bars]`
//! `cargo run --release -p tr-chart --example framebench -- --live` compares live updates.

use std::time::{Duration, Instant};

use tr_chart::gpu::{Frame, LineKind, Style};
use tr_chart::headless::Headless;
use tr_chart::scene::{ChartType, Overlay, PricePane, SeriesGpu, line_ref, price_pane};
use tr_chart::viewport::Viewport;
use tr_core::{Timeframe, indicators, sample};

fn pct(v: &mut [Duration], p: f64) -> f64 {
    v.sort();
    v[((v.len() - 1) as f64 * p).round() as usize].as_secs_f64() * 1e3
}

fn live_frame(h: &mut Headless, series: &SeriesGpu) {
    let v = Viewport::fit_last(series.len(), 200, 1600.);
    let mut cmds = Vec::new();
    for chart_type in [ChartType::Candles, ChartType::Line] {
        cmds.extend(
            price_pane(&PricePane {
                series,
                chart_type,
                show_volume: true,
                overlays: &[],
                viewport: &v,
                height_px: 900.,
                log: false,
                manual_scale: None,
                style: Style::default(),
            })
            .cmds,
        );
    }
    h.render(&mut [Frame { pane: 1, frame_no: 0, cmds }]);
}

fn live_bench() {
    let mut h = Headless::new(1600, 900).expect("no wgpu adapter");
    println!("LIVE adapter: {} ({:?})", h.adapter_info.name, h.adapter_info.backend);
    let mut bars = sample::generate("LIVE", Timeframe::M1, 100_000, 1_790_000_000);
    let mut new = SeriesGpu::build(&bars);
    live_frame(&mut h, &new);
    let (mut old_times, mut new_times) = (Vec::new(), Vec::new());
    for tick in 1..=600 {
        let from = bars.len() - 1;
        let b = bars.last_mut().unwrap();
        b.close += if tick % 2 == 0 { 0.03 } else { -0.02 };
        b.high = b.high.max(b.close) + 0.01;
        b.low = b.low.min(b.close) - 0.01;
        b.volume += 1.;
        if tick % 60 == 0 {
            let mut b = *bars.last().unwrap();
            b.ts += 60;
            bars.push(b);
        }
        let t = Instant::now();
        let old = SeriesGpu::build(&bars);
        live_frame(&mut h, &old);
        old_times.push(t.elapsed());
        let t = Instant::now();
        if new.update_tail(&bars, from).is_none() {
            new = SeriesGpu::build(&bars);
        }
        live_frame(&mut h, &new);
        new_times.push(t.elapsed());
    }
    let fresh = SeriesGpu::build(&bars);
    for k in 0..fresh.bars.pyramid.read().expect("pyramid lock").levels() {
        assert_eq!(
            new.bars.pyramid.read().expect("pyramid lock").level(k),
            fresh.bars.pyramid.read().expect("pyramid lock").level(k)
        );
        let (a, b) = (
            new.closes.pyramid.read().expect("pyramid lock").offsets[k],
            fresh.closes.pyramid.read().expect("pyramid lock").offsets[k],
        );
        let n = fresh.closes.pyramid.read().expect("pyramid lock").level_len(k);
        assert_eq!(
            &new.closes.pyramid.read().expect("pyramid lock").data[a..a + n],
            &fresh.closes.pyramid.read().expect("pyramid lock").data[b..b + n]
        );
    }
    for (name, times) in [("OLD", &mut old_times), ("NEW", &mut new_times)] {
        println!(
            "LIVE {name}: p50 {:.3} ms, p95 {:.3} ms, p99 {:.3} ms",
            pct(times, 0.5),
            pct(times, 0.95),
            pct(times, 0.99)
        );
    }
    println!("LIVE equivalence: passed (100000 bars, 600 ticks, 10 appends)");
}

fn main() {
    if std::env::args().any(|arg| arg == "--live") {
        live_bench();
        return;
    }
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
    let mut h = Headless::new(1600, 900).expect("no wgpu adapter");
    println!("adapter: {} ({:?}, {:?})", h.adapter_info.name, h.adapter_info.device_type, h.adapter_info.backend);

    let t = Instant::now();
    let bars = sample::generate("BENCH", Timeframe::M1, n, 1_790_000_000);
    let gen_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let series = SeriesGpu::build(&bars);
    let overlays = [
        Overlay {
            line: line_ref(&indicators::ema(&closes, 20)),
            color: [0.0, 0.6, 1.0, 1.0],
            kind: LineKind::Line,
            scale: 1.0,
            width: 1.3,
            histogram_color: None,
        },
        Overlay {
            line: line_ref(&indicators::bollinger(&closes, 20, 2.0).upper),
            color: [0.8, 0.8, 0.8, 1.0],
            kind: LineKind::Line,
            scale: 1.0,
            width: 1.3,
            histogram_color: None,
        },
    ];
    let prep_ms = t.elapsed().as_secs_f64() * 1e3;
    println!("{n} bars: generate {gen_ms:.0} ms, pyramids+indicators {prep_ms:.0} ms");

    let mut v = Viewport::fit_last(n, 200, 1600.0);
    let mut render = |v: &Viewport| {
        let t0 = Instant::now();
        let out = price_pane(&PricePane {
            series: &series,
            chart_type: ChartType::Candles,
            show_volume: true,
            overlays: &overlays,
            viewport: v,
            height_px: 900.0,
            log: false,
            manual_scale: None,
            style: Style::default(),
        });
        let scene = t0.elapsed();
        let timing = h.render(&mut [Frame { pane: 1, frame_no: 0, cmds: out.cmds }]);
        (scene + timing.cpu, scene + timing.total)
    };
    // Warm-up uploads the pyramids once.
    let first = render(&v);
    println!("first frame (includes GPU upload): {:.1} ms", first.1.as_secs_f64() * 1e3);

    type Step = Box<dyn Fn(&mut Viewport, usize)>;
    let phases: [(&str, Step); 3] = [
        ("zoom out to all bars", Box::new(|v: &mut Viewport, _| v.zoom(0.97, 1500.0))),
        ("pan across history", Box::new(|v: &mut Viewport, i| v.pan_px(if i < 150 { 40.0 } else { -40.0 }))),
        ("zoom in to 1 bar/8px", Box::new(|v: &mut Viewport, _| v.zoom(1.035, 800.0))),
    ];
    println!("{:<24} {:>6} {:>10} {:>10} {:>10} {:>10}", "phase", "frames", "cpu p50", "cpu p99", "gpu p50", "gpu p99");
    for (name, step) in phases {
        let (mut cpu, mut total) = (Vec::new(), Vec::new());
        for i in 0..300 {
            step(&mut v, i);
            v.clamp(n);
            let (c, t) = render(&v);
            cpu.push(c);
            total.push(t);
        }
        println!(
            "{:<24} {:>6} {:>8.2}ms {:>8.2}ms {:>8.2}ms {:>8.2}ms",
            name,
            cpu.len(),
            pct(&mut cpu, 0.5),
            pct(&mut cpu, 0.99),
            pct(&mut total, 0.5),
            pct(&mut total, 0.99)
        );
    }
    println!("(120 fps budget = 8.33 ms; 'cpu' = scene build + command recording, 'gpu' = until GPU finished)");
}
