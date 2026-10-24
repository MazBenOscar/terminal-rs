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
and consent state. The registry rejects future non-rectangle create anchors early;
rectangle and update anchors are validated/normalized against loaded data by the host.
It checks indicator codes/normalizes parameters against the core catalog before forwarding.
Screenshots require trusted UI consent. Workflow calls enqueue proposals; they do not
consume the screenshot consent flag or bypass user approval.

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
Zones are ordered by distance from the latest close, with at most three on each
side. The nearest support, resistance and in-play clusters are reserved even if
`top` is small (`top: 0` returns none). A containing zone has `in_play: true`;
`side` distinguishes support, resistance and in-play. Scores remain available but
no longer determine selection. Height is capped at max(1.5 ATR(14), 2 tolerance),
trimming padding toward the dense center first, retaining all touches if possible;
`trimmed: true` reports clipping. No zone is fabricated if no confirmed cluster exists.

## Agent rectangle validation and normalization

This is the rejection-path inventory for `create_drawing` rectangles:

- Registry/transport: disabled/unknown tool, exhausted shared call budget (failed
  attempts count), malformed JSON or >64 KiB payload; missing/unknown fields,
  wrong types, invalid enums (`kind`, `zone_role`, `source_timeframe`), timestamps
  outside the schema's calendar range, anchors outside the 1–8 schema item range,
  and optional label strings that are blank or longer than 2048 characters.
  Field paths and permitted values/ranges are returned. Omit `label` to use the
  host default. Label sanitization itself does **not** reject: control characters
  are removed and drawing text is truncated to 500 characters, reserving the TF prefix.
- Live adapter/queue: invalid or missing chart id, closed pinned/target chart,
  changed pin/symbol/timeframe, cancelled/disconnected job or UI deadline, or an
  invalid internal UI reply. These are execution/transport failures, not geometry
  failures. They do not prove an edit succeeded; read the chart before a new attempt.
- Session limits: 20 creations per turn or 100 agent objects on a chart. Reuse
  `update_drawing`, remove session-owned objects, or stop until a new turn as applicable.
- Session arguments: explicit symbol must match the loaded chart; `zone_role`
  requires `kind: rectangle`; direct hosts also validate `source_timeframe`.
- Geometry: exactly two anchors and loaded bars are required. Prices must be
  finite within ±min(100 × maximum loaded price magnitude, 1e15). Equal prices
  are rejected with a non-zero-zone correction. Inverted prices and time order
  are normalized, not rejected. End extensions are clamped to the last loaded bar.
  The resulting start/end must be within loaded bars and not in the future;
  errors include the anchor field and ISO/Unix loaded range. Times between bars
  snap to the nearest bar (ties choose earlier). Times snapping to the same bar
  are rejected; choose two distinct loaded bars. Non-rectangle geometry retains
  its prior timestamp semantics.

There is no extra rectangle rejection in `Drawing::sanitize`: this two-anchor
geometry is complete. The host creates `W1 support 72883–76892`-style labels for
unlabelled zones, using the chart-supplied axis precision and `source_timeframe`
from analysis (chart timeframe only when absent). The source is persisted for
idempotent label updates and survives chart timeframe changes.

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
