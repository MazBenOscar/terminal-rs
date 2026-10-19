//! Renders real frames offscreen (any adapter, including software Vulkan) and checks pixels.

use tr_chart::gpu::{Frame, LineKind, Reveal, Style};
use tr_chart::headless::Headless;
use tr_chart::scene::{ChartType, Overlay, PricePane, SeriesGpu, line_ref, price_pane, sub_pane};
use tr_chart::viewport::Viewport;
use tr_core::{Timeframe, indicators, sample};

fn dump_ppm(path: &str, w: u32, h: u32, rgba: &[u8]) {
    let mut out = format!("P6 {w} {h} 255\n").into_bytes();
    out.extend(rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]));
    std::fs::write(path, out).unwrap();
}

fn count(px: &[u8], pred: impl Fn(&[u8]) -> bool) -> usize {
    px.as_chunks::<4>().0.iter().filter(|p| pred(p.as_slice())).count()
}

#[test]
fn renders_candles_volume_lines_and_hist() {
    let Some(mut h) = Headless::new(800, 400) else {
        eprintln!("no wgpu adapter; skipping");
        return;
    };
    eprintln!("adapter: {:?}", h.adapter_info.name);
    let bars = sample::generate("AAPL", Timeframe::D1, 2_000, 1_790_000_000);
    let series = SeriesGpu::build(&bars);
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let overlays = [Overlay {
        line: line_ref(&indicators::sma(&closes, 20)),
        color: [0.0, 0.6, 1.0, 1.0],
        kind: LineKind::Line,
        scale: 1.0,
    }];
    let v = Viewport::fit_last(bars.len(), 150, 800.0);
    for ct in ChartType::ALL {
        let out = price_pane(&PricePane {
            series: &series,
            chart_type: ct,
            show_volume: true,
            overlays: &overlays,
            viewport: &v,
            height_px: 400.0,
            log: false,
            manual_scale: None,
            style: Style::default(),
        });
        h.render(&mut [Frame { pane: 1, frame_no: 0, cmds: out.cmds }]);
        let px = h.read_pixels();
        if let Ok(dir) = std::env::var("TR_DUMP_DIR") {
            dump_ppm(&format!("{dir}/{ct:?}.ppm"), 800, 400, &px);
        }
        let green = count(&px, |p| p[1] > 150 && p[0] < 100);
        let red = count(&px, |p| p[0] > 200 && p[1] < 100);
        let blue = count(&px, |p| p[2] > 200 && p[0] < 50);
        eprintln!("{ct:?}: green {green} red {red} blue {blue}");
        assert!(blue > 300, "{ct:?}: SMA overlay not drawn");
        match ct {
            ChartType::Candles | ChartType::Bars => assert!(green > 500 && red > 500, "{ct:?}"),
            ChartType::Line | ChartType::Area => {
                let amber = count(&px, |p| p[0] > 200 && p[1] > 120 && p[1] < 200 && p[2] < 60);
                assert!(amber > 300, "{ct:?}: price line missing ({amber})");
            }
        }
    }

    // MACD histogram in a sub-pane.
    let m = indicators::macd(&closes, 12, 26, 9);
    let hist = [Overlay { line: line_ref(&m.histogram), color: [1.0; 4], kind: LineKind::Hist, scale: 1.0 }];
    let out = sub_pane(&hist, &v, 400.0, None, Style::default());
    h.render(&mut [Frame { pane: 2, frame_no: 0, cmds: out.cmds }]);
    let px = h.read_pixels();
    assert!(count(&px, |p| p[1] > 150 && p[0] < 100) > 200);
    assert!(count(&px, |p| p[0] > 200 && p[1] < 100) > 200);
}

#[test]
fn reveal_sweeps_in_from_the_left_and_fades() {
    let Some(mut h) = Headless::new(800, 400) else {
        eprintln!("no wgpu adapter; skipping");
        return;
    };
    let bars = sample::generate("AAPL", Timeframe::D1, 2_000, 1_790_000_000);
    let series = SeriesGpu::build(&bars);
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let overlays = [Overlay {
        line: line_ref(&indicators::sma(&closes, 20)),
        color: [0.0, 0.6, 1.0, 1.0],
        kind: LineKind::Line,
        scale: 1.0,
    }];
    let v = Viewport::fit_last(bars.len(), 150, 800.0);
    let mut draw = |reveal: Reveal, pane: u64| {
        let out = price_pane(&PricePane {
            series: &series,
            chart_type: ChartType::Candles,
            show_volume: true,
            overlays: &overlays,
            viewport: &v,
            height_px: 400.0,
            log: false,
            manual_scale: None,
            style: Style { reveal, ..Style::default() },
        });
        h.render(&mut [Frame { pane, frame_no: pane, cmds: out.cmds }]);
        h.read_pixels()
    };
    // Lit pixels (candles, volume or the SMA) in columns [x0, x1).
    let lit = |px: &[u8], x0: usize, x1: usize| {
        px.as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .filter(|(i, p)| (x0..x1).contains(&(i % 800)) && p[..3] != [0, 0, 0])
            .count()
    };

    let half = draw(Reveal::sweep(0.45, v.left(), v.visible_bars()), 1);
    if let Ok(dir) = std::env::var("TR_DUMP_DIR") {
        dump_ppm(&format!("{dir}/reveal.ppm"), 800, 400, &half);
    }
    let (left, right) = (lit(&half, 0, 250), lit(&half, 450, 800));
    eprintln!("half revealed: left {left} right {right}");
    assert!(left > 2_000, "bars behind the front are drawn ({left})");
    assert_eq!(right, 0, "nothing past the front, lines included");

    let full = draw(Reveal::DONE, 2);
    assert!(lit(&full, 450, 800) > 2_000);

    let faded = draw(Reveal { opacity: 0.0, ..Reveal::DONE }, 3);
    assert_eq!(lit(&faded, 0, 800), 0, "opacity 0 draws nothing");
}
