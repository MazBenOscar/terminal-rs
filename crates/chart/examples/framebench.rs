//! Interactive-style frame-time benchmark: a scripted zoom-out/pan/zoom-in session over
//! a 1M-bar series, rendered offscreen at 1600x900. Prints percentiles per phase.
//!
//! `cargo run --release -p tr-chart --example framebench [bars]`

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

fn main() {
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
