# tr-agent-tools

Headless registry and deterministic data tools. No app or egui dependency.

## API

- `Registry::new(ToolSet::{ReadOnly, Edit, Workflows})`, `schemas()` and async
  `call(name, args_json, &mut CallContext)`.
- `ToolSpec`: name, description, JSON input schema, `ToolKind::{Data, Live}`,
  mutation and confirmation flags. Presets are cumulative: 7 read tools,
  14 chart/read tools, 21 tools including workflows. `set_view` is a mutation.
- `ToolResult { success, content }`, with `Content::{Text, ImagePng}`. Text is JSON,
  capped at 16 KiB **in total**, not per content item. Images are not text-capped.
- `CallBudget::default()` permits 8 attempts, including failed calls. Share the
  budget across concurrent/serial calls for a turn; create a fresh budget next turn.
- `CallContext` holds data/live hosts, the shared budget, `now` (Unix seconds), and
  a trusted `confirmed` flag. Confirmation is not accepted in tool arguments.
- `DataHost`: boxed Send futures for `history(symbol, timeframe, end, limit)`,
  `search(query)`, and `quotes(symbols)`. `History` carries source-reported price
  basis, session and optional authoritative `CryptoInstrument` alongside bars.
- `MarketDataHost<M: MarketData>` adapts existing providers. Configure the correct
  history price basis/session; use `PriceBasis::Unknown` when not established.
- `LiveHost::call_live(name, args::LiveArgs)` returns a boxed Send future. Optional
  `chart_id` means the pinned chart when absent. The app can enqueue the typed
  enum for its UI thread and resolve the future via a response channel.
- `args` exposes every argument struct, `Arguments::schema()`, and typed enums.
  Structs deny unknown fields; the registry also validates schema constraints.
- `analytics::{lttb, support_resistance, SupportResistance}` is reusable without
  a live chart. Inputs must be chronological finite OHLCV bars (as normalized and
  validated by the registry's data path).

Live execution remains the host's responsibility: stable pinned-pane resolution,
loaded-range checks and sanitization for drawings, stale entity ids, UI queue
cancellation/deadlines, linked-pane reporting, hidden-window screenshot failures,
and consent state. The registry rejects future drawing anchors early and checks
indicator codes/normalizes parameters against the core catalog before forwarding.
Confirmation-sensitive calls (including screenshots) fail unless `confirmed` is
set by trusted UI consent; hosts should recheck consent at execution time.

## Data semantics

`read_data` accepts `symbol`, optional `timeframe` (default `1D`), inclusive Unix
`from`/`to` bounds and `limit` (default 1,000, maximum 10,000). The provider request
is bounded, ending no later than `now`. Statistics and analysis cover the bars
actually returned, not a promise that the provider fulfilled the whole range.
Data tools never infer an authoritative crypto source from the requested symbol.
Search resolves crypto market keys using core instrument identity and exposes both
venue-qualified keys and human-readable pairs. Imported names/labels remain JSON
strings, not instructions.

Summary defaults target 200 recent bars and 300 full-range LTTB close points.
When that exceeds 16 KiB, the tool reduces detail, reports emitted counts and an
explicit truncation marker, retains full-range statistics, and preserves sampled
endpoints. Times in summaries and analysis envelopes are ISO 8601 UTC with an
explicit `+00:00` offset. Swing point times and S/R touch times are Unix seconds.
Last-bar progress is determined against the host-reported session's core interval.

`analyze` modes are `swings`, `trend`, `levels`, `volume_profile`, and `volatility`.
Every response has `version: 1`, symbol, timeframe, and the actual `bar_range`.
Fractals include their confirmation time; ZigZag identifies its repainting last
leg. Regression slope is price per observed bar, R² is 1 for a constant series,
and higher-high/low counts compare confirmed fractals. Volatility reports sample
standard deviation of valid log returns per bar (not implicitly annualized),
Wilder ATR(14), and nearest-index range percentiles. Short-window ATR is null until
warmup. Volume profiles are core OHLCV estimates with a 70% value area, plus VWAP
anchored at the range start. Core classic pivots require a previous core session.

The support/resistance finder lives in **this crate**, preserving the requested
new-crate-only scope. It clusters confirmed swing highs/lows with ATR-scaled,
bounded-diameter zones, then scores distinct swing-bar touches, recency and volume.
It is public for later reuse or relocation into core without app dependencies.

## Errors

Errors are JSON `{ "error": { "code", "message", "field"? } }` with
`success: false`. Registry/data codes:

- `invalid_args`: malformed JSON, unknown/missing fields or invalid values.
- `unknown_tool`, `tool_disabled`.
- `budget_exhausted`.
- `confirmation_required`.
- `host_error`: provider failure.
- `invalid_data`: invalid OHLCV, timestamp or source instrument metadata.
- `no_data`: analysis requested with no returned bars.

Live hosts may return additional structured codes such as `stale_id`,
`out_of_range`, `timeout`, or `window_hidden`. Oversized host text is wrapped in
valid JSON with a truncation marker and preview; built-in summaries/analyses
instead reduce their detail while retaining envelope metadata.

## Tests

Tests are offline. Schema snapshots live in `tests/snapshots`; seeded and handmade
bar fixtures and per-mode golden outputs live in `tests/fixtures`. Regenerate with:

```sh
UPDATE_SNAPSHOTS=1 cargo test -p tr-agent-tools
```

The generator is checked against committed fixtures; goldens round numbers to
8 decimal places to avoid insignificant platform-dependent libm differences.
