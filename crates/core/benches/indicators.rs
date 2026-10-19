use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tr_core::indicators::registry::{self, Input};
use tr_core::{Timeframe, indicators, sample};

fn bench(c: &mut Criterion) {
    let bars = sample::generate("BENCH", Timeframe::M1, 1_000_000, 1_790_000_000);
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let mut g = c.benchmark_group("indicators_1M_bars");
    g.sample_size(20);
    g.bench_function("sma20", |b| b.iter(|| indicators::sma(black_box(&closes), 20)));
    g.bench_function("ema20", |b| b.iter(|| indicators::ema(black_box(&closes), 20)));
    g.bench_function("rsi14", |b| b.iter(|| indicators::rsi(black_box(&closes), 14)));
    g.bench_function("macd", |b| b.iter(|| indicators::macd(black_box(&closes), 12, 26, 9)));
    g.bench_function("bollinger", |b| b.iter(|| indicators::bollinger(black_box(&closes), 20, 2.0)));
    g.bench_function("vwap", |b| b.iter(|| indicators::vwap(black_box(&bars))));
    g.finish();

    // The whole catalogue at default parameters, as the chart worker computes it.
    let input = Input::new(&bars[..100_000], Timeframe::M1);
    let mut g = c.benchmark_group("catalog_100k_bars");
    g.sample_size(10);
    g.bench_function("every_indicator", |b| {
        b.iter(|| registry::all().iter().map(|d| d.run(black_box(&input), &d.defaults()).len()).sum::<usize>())
    });
    for code in ["ICHIMOKU", "MAMA", "SUPERTREND", "ZIGZAG", "PIVOT", "YZ"] {
        let d = registry::find(code).expect(code);
        g.bench_function(code, |b| b.iter(|| d.run(black_box(&input), &d.defaults())));
    }
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
