//! Frame-time benchmarks: build draw commands and render 1M-bar charts offscreen.
//! Run with `cargo bench -p tr-chart --bench frame`.

use criterion::{Criterion, criterion_group, criterion_main};
use tr_chart::gpu::{Frame, LineKind, Style};
use tr_chart::headless::Headless;
use tr_chart::scene::{ChartType, Overlay, PricePane, SeriesGpu, line_ref, price_pane};
use tr_chart::viewport::Viewport;
use tr_core::{Timeframe, indicators, sample};

fn bench(c: &mut Criterion) {
    let Some(mut h) = Headless::new(1600, 900) else {
        eprintln!("no wgpu adapter; skipping frame benchmarks");
        return;
    };
    eprintln!("adapter: {} ({:?})", h.adapter_info.name, h.adapter_info.device_type);
    let n = 1_000_000;
    let bars = sample::generate("BENCH", Timeframe::M1, n, 1_790_000_000);
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
            line: line_ref(&indicators::sma(&closes, 50)),
            color: [1.0, 0.0, 1.0, 1.0],
            kind: LineKind::Line,
            scale: 1.0,
            width: 1.3,
            histogram_color: None,
        },
    ];
    let mut g = c.benchmark_group("frame_1M_bars_1600x900");
    g.sample_size(50);
    for (name, bar_px) in [("all_1M_visible", 1600.0 / n as f64), ("10k_visible", 0.16), ("200_visible", 8.0)] {
        let mut v = Viewport { right: n as f64, bar_px, width_px: 1600.0 };
        let mut step = 0.0;
        g.bench_function(name, |b| {
            b.iter(|| {
                // Pan a little every frame so nothing is cached between iterations.
                step += 1.0;
                v.pan_px(if (step as i64 / 100) % 2 == 0 { 3.0 } else { -3.0 });
                let out = price_pane(&PricePane {
                    series: &series,
                    chart_type: ChartType::Candles,
                    show_volume: true,
                    overlays: &overlays,
                    viewport: &v,
                    height_px: 900.0,
                    log: false,
                    manual_scale: None,
                    style: Style::default(),
                });
                h.render(&mut [Frame { pane: 1, frame_no: 0, cmds: out.cmds }])
            })
        });
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
