# Chart audit delivery

For the reconciled remaining backlog at `cfee48c`, including the macOS QA merge,
evidence limits and decisions on hold, see [TC2000 gap backlog](analysis/tc2000-gap-backlog.md).

The working baseline incorporates price alerts, comparison scale modes and expanded
drawing tools through `03019b3`. The user requested continuing through the remaining
priorities without pausing for review after each increment.

Delivery sequence (completed increments are merged into `personal/main`; each keeps
its feature branch for review):

1. `codex/chart-account-overlays`: average costs, chart-mark P/L and open limit/stop orders.
2. `codex/chart-compare-scales`: several comparison symbols and indexed-to-100 scale.
3. `codex/chart-scan-workflow`: keyboard navigation, scanner traversal/conditions and
   last-price/session context.
4. `codex/chart-studies-types`: anchored VWAP, volume profile and additional chart types.
5. `codex/chart-advanced-drawings` and `codex/chart-alert-conditions`: tools and conditions.
6. `codex/chart-replay-exports`: replay, exports, Renko and timeframe extensions.
7. `codex/chart-event-data`: event/provider coverage and optional minute streaming;
   implement supported data paths and explicitly record evidence
   or credential limitations rather than inventing data or claiming full TC2000 parity.

## Account overlays

The chart context menu's **Positions and orders** controls visibility and account
selection. Cost levels and working limit/stop levels come from the currently selected
demo or brokerage mode. Accounts are kept separate. Options and other unsupported
instruments are excluded from underlying price axes; non-USD share cost bases are
excluded. Short quantities retain their sign. Filled/canceled orders are omitted.

P/L is explicitly **chart P/L**, computed from the latest loaded chart close, cost,
quantity and multiplier. It is not a guarantee of real-time brokerage valuation, and
does not include commissions, FX conversion or realized P/L. Stale/undated portfolio
snapshots and refresh failures are labeled. A chart with overlays enabled participates
in the normal read-only portfolio refresh schedule even if PORT is closed. No order
placement, cancellation or modification is performed by these overlays.

## Multiple comparisons and scales

The chart picker adds up to eight comparison symbols. Each extra row can be hidden
or removed; removing the original comparison promotes the next one. `CMP` retains
its original replace/clear behavior. All visible, successfully loaded comparison
lines use the first common positive finite bar in the visible range as their anchor.
There is no invented value before a symbol's available history. Loading and errors
are per symbol; generation/revision guards reject responses for older chart data.

The scale options include percent change, indexed-to-100, rebased price and the
existing single-comparison own axis. Multiple comparisons use a shared axis; own-axis
mode is unavailable with extra series. Legend values identify the symbol and its
return from the shared baseline. Hidden series do not affect autoscale or the anchor.
Series/visibility settings persist in layouts (format v6, accepting earlier versions).

## Chart and scanner workflow

`DATE YYYY-MM-DD` / `GOTO` centers a date and pages older history when available.
Up/Down or `PREV`/`NEXT` traverses the chart's scan-match queue, or the watchlist
when there is no queue. Traversal wraps. Existing `5` + Enter, left/right pan and
+/- zoom remain available. Text editors retain their keys while editing.

Clicking a matched scanner row captures that result list and scan definition, selects
the scan timeframe and adds a PCF condition plot (1 = match, 0 = no match). The chart
navigation menu exposes the definition and can re-add the plot or switch traversal
back to the watchlist. The plot is evaluated on the chart's loaded bars; it is not the
earlier scanner snapshot. Formula size limits and startup/missing-data conventions
still apply and errors are visible in the study legend. A fresh scan is required for
a current match list. Queues are capped at 5,000 symbols and persist with layouts.

Price/session context controls expose a bar-close countdown, previous loaded session
close, loaded-session high/low and shading of available US pre/post-market bars.
These are derived from loaded bars, not fabricated official exchange prices or extra
history. The countdown says Bar closed for an expired last bar. Context visibility
persists with the layout; exchange/session/provider limitations remain explicit.

## Studies and display types

Right-click a loaded price bar and use **Anchored VWAP and volume profile** to set
an anchor, choose visible-range/latest-session profiles, set the bin count or a
baseline price. VWAP uses HLC3 weighted by reported volume and becomes unavailable
when the anchor history or subsequent volume is missing. Profiles distribute each
bar's volume across its high/low bins, displaying POC and an approximately 70% value
area; partial data is labeled. These are estimates, not exchange volume-at-price.

Chart types now include Heikin Ashi, hollow up candles and baseline charts. Heikin
Ashi construction runs in the worker and keeps raw timestamps; source OHLC, studies,
alerts and account valuations still use actual source bars. Baseline lines/fills use
positive/negative colors relative to the chosen reference or visible start. Layouts
use v7 so older builds cannot silently discard the new style and study settings.

## Advanced drawings

`FIBEXT`, `PITCHFORK`, `LONGRR` and `SHORTRR` add three-point tools through the
same Draw menu/command palette. Fib extensions project A→B from C. Pitchforks use
the median from A through the B/C midpoint and two parallel rays, in displayed
coordinates. Risk/reward tools use entry, stop and target; they show per-unit risk,
reward and the ratio, and label invalid sides rather than displaying a fictitious
ratio. They are planning annotations and do not submit orders or size positions.
All use the existing selection, three-handle/body dragging, ownership and undo path.

## Condition alerts

The alert editor supports PCF formula values on a selected timeframe (for example
`WRSI(14)` crossing 70 or `C-XAVG(C,20)` crossing zero). A chart opened from a scan
can create a Boolean condition alert for that symbol. Selected trendlines, rays and
horizontal drawings can create an alert against a saved line snapshot. Sloping line
alerts extrapolate by elapsed time, including closed sessions; they are labeled
explicitly because chart drawings use bar-space geometry. Later drawing edits do not
change an already saved alert. Zero means price crosses the saved line.

Condition polling uses up to 1,500 uncached provider bars, including the forming bar,
with a fresh minute observation required for source/mode/age checks. Each rule has
its own baseline and one-shot history; edits, errors, source changes and long gaps
re-seed. Indicator values are labeled as values and never drawn as dollar levels.
The app must remain running; crossings between polls may be missed. Scan alerts
monitor selected symbols, not an unattended universe-wide rescan. Alert storage v2
accepts v1 files; keep a backup before opening with an older build.

## Replay, exports and intervals

The chart's **Replay and exports** menu starts replay at the clicked loaded bar
(or the middle of loaded history). The toolbar steps source bars, scrubs and plays
at one bar per second. Studies and comparisons rebuild from the cutoff; stale
responses are rejected. Drawings, account levels and Congress markers are hidden,
so saved annotations and present-day positions cannot reveal a future state.
Independent live alerts continue normally. Replay is transient and uses loaded
history; it is not a simulated execution or corporate-action point-in-time engine.

`RENKO` uses a fixed positive box size, close-only construction and one-box reversal.
Multiple bricks can share a source timestamp. Studies use these synthetic bricks;
volume is apportioned OHLCV volume. Comparisons, drawings and account/alert levels
are unavailable on brick charts. The box size is in the chart context menu and
persists. Construction is bounded at 100,000 bricks and errors ask for a larger box.

Exports save the **visible displayed bars** under the app data directory's `exports`
folder. CSV preserves OHLCV values and Unix UTC timestamps. SVG is a portable OHLC
price-chart image with axes and mode labeling, not a screenshot: it excludes
studies, drawings and other UI panes. Replay exports contain only displayed history;
Renko CSV contains synthetic bricks. Files use unique names and never overwrite.

Intervals now include 2m, 3m, 2H and calendar quarters (`3M` or `quarter`). They
aggregate supported base intervals rather than request unsupported provider codes.
Quarter boundaries and completion use actual calendar months. Crypto streaming is
disabled for these aggregated intervals; history refresh remains available. Provider
depth limits still apply, including the minute-history limit for 2m/3m. Layout v8
accepts older layouts and protects the new interval/type settings from old builds.

## Events and provider options

**Earnings, dividends and splits** in the chart context menu enables E/D/S markers.
Yahoo's reported dividend/split records are fetched independently of price bars;
errors and incomplete coverage are visible in the menu. Event markers map to the
displayed daily, weekly, monthly or quarterly bucket. Replay and Renko hide events.
A live Yahoo probe returned actual dividend dates/amounts; parser fixtures cover
dividends and split ratios. No automatic earnings coverage is claimed.

Attributed local events, including earnings, can be loaded from `chart-events.json`
in the app data directory. The file is a JSON array, at most 2 MiB/10,000 events:

```json
[{"symbol":"AAPL US Equity","ts":1792497600,"kind":"Earnings",
  "detail":"Illustrative earnings annotation; replace with verified data",
  "source":"Example schema only"}]
```

Kinds are `Earnings`, `Dividend` and `Split`; timestamps are Unix UTC seconds.
Use **Reload event data** after changing the file. These are annotations, not a
point-in-time fundamental database, and no missing event is silently fabricated.

**Time zone, session & refresh** now separates session filtering from requesting
extended-hours history. Yahoo passes `includePrePost=true` when requested. Alpaca
offers raw or split-adjusted OHLC through its `adjustment` parameter. Unsupported
provider/asset options produce an error rather than silently changing the source.
History/paging/comparisons use the same options; caches have separate namespaces.
Ordinary quotes/alerts retain their independent source convention. Sample-mode
options do not create real corporate actions or exchange sessions. Layout **v9**
protects these source settings and accepts earlier layouts.

## Exchange calendars

Set `TR_US_CALENDAR` to an attributed JSON schedule and restart to replace the
built-in approximation within explicit coverage. An example schema (not an
authoritative schedule):

```json
{"start":"2026-10-19","end":"2026-10-20","source":"Example only",
 "sessions":[{"date":"2026-10-19","open":570,"close":960},
             {"date":"2026-10-20","open":570,"close":960}]}
```

Times are minutes after midnight in New York. List **every open session** in the
coverage range; absent dates inside it are closed. This supports exceptional
closures, late openings and short days. Invalid files produce an explicit warning;
outside coverage the built-in holiday approximation still applies. The menu shows
coverage and attribution. A full historical/venue schedule has not been supplied
or independently certified, so complete exchange-calendar parity remains open.

## Optional equity minute streaming

Set `TR_ALPACA_STREAM=1` alongside the existing `APCA_API_KEY_ID`,
`APCA_API_SECRET_KEY` and `APCA_DATA_FEED` (`iex`, `sip` or `delayed_sip`). Streaming
is opt-in, limited to **1m stock charts with default history options**, and shares
one authenticated connection across up to 64 distinct chart symbols. It subscribes
to minute bars and updated bars, handles reconnect/subscription changes and shows
waiting/error status. Full snapshots replace corrected timestamps; partial crypto
updates retain their existing merge semantics. There is no tick-by-tick stock feed.

With streaming enabled, Alpaca chart history does not fall back to Yahoo and its
cache namespace is distinct. Reconnect/consumer gaps require a history refresh;
there is no fabricated backfill. Credentials are never included in status/log
messages. A local WebSocket server verifies authentication, multi-symbol subscription
and routing. Live authentication, account entitlement and exchange coverage were
not tested. Protocol references: [Alpaca streaming](https://docs.alpaca.markets/us/docs/streaming-market-data)
and [stock bars](https://docs.alpaca.markets/us/docs/real-time-stock-pricing-data).

## Delivery and remaining external validation

The audit's local chart increments are implemented on stacked feature branches;
`personal/main` collects them for testing. This is not a claim of complete TC2000
parity. Remaining external work is verified earnings/calendar coverage, entitled
live-feed testing, matched TC2000 numeric exports/proprietary formulas, and review
of the visible app on a functioning graphics session. Native independent study
histories and unrestricted PCF syntax remain separate larger parity work.

The visible-app smoke attempt on this machine failed with Wayland `NoCompositor`;
both Wayland and X11 socket directories were empty. GPU render and headless egui
checks remain useful, but they do not certify the final desktop appearance.

Final workspace verification: **416 tests passed, 5 existing network tests ignored**,
including all four offscreen GPU render tests. `cargo fmt --all -- --check` and
strict workspace/all-target Clippy passed under WSL. The local stream fixture and
headless interaction tests passed. The UI source detector returned no findings;
its coverage of native Rust/egui code is limited and does not replace visible QA.
