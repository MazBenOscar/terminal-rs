# Performance audit (2026-10-22)

A read-only audit of the release build and the source at about `dc27b49`: one
measured session of the running app plus four parallel code reviews. Claims marked
✔︎ were re-checked against the source; other cost figures are estimates from
reading code, not profiling. No code was changed by the audit. Line references
are approximate and will drift; the named functions are authoritative.

## Measured

- Idle: 0–0.4% CPU, 10–17 threads, about 100–105 MB resident memory.
- No leak: memory moved between 100 and 105 MB over 90 seconds and returned.
- The app never fully sleeps; it wakes on a fixed repaint interval (finding 4).

## Already efficient

GPU-drawn candles and lines with level-of-detail data (1M-bar charts stay
responsive); indicators computed off the UI thread with stale results dropped;
one shared background pool; quotes batched 20 symbols per request; efficient
database setup; streamed (not polled) crypto and Alpaca prices; Congress and
options lists draw only visible rows; most refreshes run only for visible panels.

## Findings, ranked by impact

| # | Issue | Where | Impact | Suggested change |
|---|---|---|---|---|
| 1 | Each live tick rebuilds the whole chart: every 250 ms with new data it copies all bars, rebuilds both chart structures over the full history and re-uploads them to the GPU for a one-bar change ✔︎ | `worker.rs` `start_live`; `gpu.rs` series upload | High; the main memory churn | Send the changed bars and patch GPU buffers in place |
| 2 | Indicators recompute over the full history on live ticks (every 1.5 s) and each line is re-uploaded | `chart_view.rs` indicator requests; `worker.rs` `build_indicators_session` | High with many studies | Recompute recent bars only, reuse unchanged lines, compute independent studies in parallel |
| 3 | Session filtering runs on the UI thread: with a custom intraday session every tick filters all bars and rebuilds GPU data before the frame draws ✔︎ | `chart_view.rs` session filter | High in that mode | Move it into the worker |
| 4 | Work never pauses when idle or hidden: repaint every 500 ms ✔︎; no minimised/unfocused check; quotes, order book, alerts, Congress feed and autosave keep running | `app.rs` update loop and refresh timers | Medium; battery and network | When unfocused, repaint every 5–10 s and pause all but alerts; repaint the clock only when its text changes |
| 5 | Autosave on the UI thread every 10 s serialises the whole layout to compare it and rewrites `settings.json` even when unchanged ✔︎ | `app.rs` `autosave`, `save_settings` | Medium; disk writes can stall a frame | Save only on change, write on a background thread |
| 6 | Scanner tables lay out and format every row every frame, re-sort, and auto-refresh redraws every second | `scanner_view.rs` results table | Medium with large scans | Draw visible rows only, filter first, cache the sort; same later for watchlist and positions |
| 7 | Alerts check each symbol separately every 15 s and re-download up to 1,500 bars per rule check | `crates/alerts/src/lib.rs` | Medium; the largest steady network cost | Batch price alerts through quotes, cache bars and fetch only new ones, check rules at their own timeframe |
| 8 | Overlays: hover rebuilds every drawing's geometry per mouse move; anchored VWAP and volume profile recompute every frame; extended-hours shading draws one rectangle per bar ✔︎; drawings are copied each frame | `chart_view.rs` hit-testing; `measure.rs`; `chart_context.rs` | Medium on busy charts | Bounding-box pre-check, cache overlays, merge shading runs, draw without copying |
| 9 | A repaint for every worker message, including a status clear on every live bar | `worker.rs` response send | Low–medium | Send status only on change, coalesce repaints |
| 10 | Comparison symbols re-download full history whenever a new bar arrives (up to 8) | `chart_view.rs` compare requests; `worker.rs` | Low–medium | Fetch and append only new bars |

## Startup, build and memory

- Startup opens three databases, starts the worker and reads about eight settings
  and state files sequentially before the first frame; opening databases in the
  background with a placeholder frame would make launch feel faster.
- macOS defaults to 4x MSAA ✔︎, estimated at about 100 MB of GPU memory on a Retina
  window; 2x would roughly halve it (`TR_MSAA` already overrides).
- Release builds keep debug information; a distribution profile with full LTO,
  one codegen unit and stripped symbols should be smaller and faster.
- Two TLS crypto libraries (ring and aws-lc-rs) and two root-certificate copies are
  compiled in; unifying would save build time and 1–2 MB but needs security testing.
- Smaller items: the bar cache never evicts; some HTTP clients are recreated per
  refresh (Congress); no exponential retry on errors or rate limits; Congress
  database queries block a background thread.

## Programme

| ID | Scope | Findings |
|---|---|---|
| P1 | Incremental live updates: changed-bar messages, in-place GPU patching, incremental studies, session filtering in the worker, status/repaint coalescing | 1, 2, 3, 9 (and 10) |
| P2 | Idle and focus throttling; change-only background autosave | 4, 5 |
| P3 | Virtualized scanner, watchlist and positions tables with cached sorting | 6 |
| P4 | Batched alert checks with cached bars | 7 |
| P5 | Overlay caching and background startup | 8, startup |
| P6 | Distribution build profile and 2x MSAA default | build |

Measure before and after each step with a repeatable scenario: a live chart with
about 100k bars and five studies, plus a large scanner run; the repo's
`framebench` example covers part of that.

## P2 results (2026-10-22)

P2 was built (branch `codex/perf-idle`, `debcf6a`) and measured against
`personal/main` on the same machine and layout, 60 s windows, release builds:

| Condition | `personal/main` | P2 as built | P2 with immediate rate-limited repaints |
|---|---|---|---|
| Window behind another app | 0.01 s CPU | 0.44 s | 0.02 s |
| Foreground idle, live AAVE chart (back-to-back A/B) | 1.36–1.49 s | — | 1.75–1.83 s |

- macOS already throttles a background window: `personal/main` is near zero there,
  so finding 4's background cost does not reproduce on macOS.
- P2's first build regressed the background case: a 250 ms *timer* repaint
  (`request_repaint_after`) wakes and paints the window even while macOS throttles
  it (profiled with `sample`: full UI passes and indicator rebuilds while hidden).
  Immediate, rate-limited `request_repaint` calls restored near-zero CPU.
- Foreground idle cost is dominated by live data handling (findings 1 and 2: full
  rebuilds and indicator recomputation per tick), not repaint cadence; it varies
  roughly fivefold with the charted market's activity (0.26 s on a quiet chart,
  about 1.4 s on a busy one).

Decision: ship only P2's autosave part (change-only writes off the UI thread,
branch `codex/autosave`); shelve the repaint/polling policy; prioritise P1. Use a
busy live chart at foreground idle as the P1 benchmark, measured A/B back to back.

## P1a results (2026-10-22)

P1a (incremental live bars: `Response::LiveBars`, in-place pyramid patching,
dirty-range GPU uploads, status coalescing) was merged from `codex/perf-live`.

| Measure | Before | P1a |
|---|---|---|
| `framebench --live`, 100k bars, per tick p50 / p99 | 5.9 / 15.0 ms | 0.60 / 1.25 ms |
| App, `TR_PROVIDER=sample`, AAVE 1D (6,000 bars, 5 studies), foreground idle, CPU per 60 s | 4.61 / 4.83 s | 4.28 / 4.34 s |
| Resident memory in the same run | 150–167 MB | 133–143 MB |

The in-app gain (about 8% CPU, about 20 MB) is smaller than the per-tick gain
because a 6,000-bar chart is cheap to rebuild and the remaining cost is indicator
recomputation and painting; it grows with history length. Indicator work (finding
2) and session filtering (finding 3) are P1b.

## P1b results (2026-10-22)

`cargo run --release -p tr-chart --example framebench -- --studies` now compares
full evaluation plus fresh line pyramids against checkpoint resumption plus tail
patches. The deterministic workload has 100,000 bars, EMA 50, EMA 100, SMA 200,
RSI 14, MACD and BB 20, and 100 last-bar edits including 10 appends.

| Study refresh | p50 | p95 | p99 |
|---|---:|---:|---:|
| OLD | 4.239 ms | 4.821 ms | 5.285 ms |
| NEW | 0.007 ms | 0.011 ms | 0.019 ms |

Every output is checked bit-for-bit (NaNs compare equal) after every refresh.
These are CPU calculation/geometry numbers, not GPU frame times or an in-app A/B.

- EMA, SMA, Wilder RSI, MACD, Bollinger Bands, ATR and session-anchored VWAP
  retain the state before the mutable last bar. Earlier changes restart the
  calculation; History replacements and revision gaps invalidate the cache.
  Scalar OHLC/HLC3/volume inputs are supported for the close-based kernels,
  including SMA/EMA volume averages (there is no separate VMA registry entry).
- Study lines, markers, histogram lines and transparent bounds keep their keys
  and patch changed f32 entries. Generation, specs, palette and session are part
  of the cache identity. Native studies also resume in independent mixed batches.
- Other study codes, non-default formula settings, multi-timeframe studies, PCF,
  displacement, fills and pane links use full evaluation.
  Dependency-bearing batches retain the original graph evaluator. Independent
  full-evaluation batches use up to four scoped threads for histories of at least
  4,096 bars. Fill point arrays and validity prefixes remain fresh builds; they
  are not line pyramids.
- Indicators and comparisons share the existing UI Arc instead of copying the
  Vec on every request. Live patches apply immediately: `Arc::make_mut` copies
  once if a worker still holds the snapshot, never waiting for provider I/O.
  Indicator fetch parameters are computed before awaiting; the full series is
  retained for calculation and dropped before response delivery. Without
  multi-timeframe histories there are no network awaits. Comparison tail requests
  copy only changed base timestamps into a retained timestamp cache and drop the
  chart Arc before fetching; cached timestamps also support full-fetch fallback.
  Full comparison requests may retain the Arc across I/O. Tests check request Arc
  identity, immediate live edits/appends with blocked requests, revision acceptance
  and supersession, and no Vec copy without an outstanding reader.
- Comparison requests keep their existing cadence, fetch a small overlapping
  tail, and resume alignment from the affected timestamp. Missing overlap,
  changed history counts, prepends, backfills and identity changes use a full
  fetch. A failed tail merge can require one additional full fetch at that same
  cadence; no new polling was added.
- Full custom-session History filtering, filtered prepend counting and filtered
  SeriesGpu construction run on the blocking worker. Delivery retains raw
  `live_source` data and preserves the viewport; live tail filtering is unchanged.
  Only SessionHistory delivery queues live ticks: its blocking closure performs
  session filtering and fresh pyramid construction, with no network, shared-lock
  acquisition or other work to wait on. The pending revision gates delivery, so
  no read lease is needed.

Verification: formatting and strict workspace Clippy passed; workspace tests
passed with 1,011 passed and 7 ignored. New tests cover checkpoint resumption and
invalidation, missing values and scalar sources, cache identities and exact dirty
ranges, shared-request ownership, comparison request limits and fallbacks,
parallel calculation parity, and worker session filtering/viewport preservation.
