# TC2000 EasyScan: static feature map

Research date: 2026-10-20. Target: the installed TC2000 version
`25.0.9511.21172`. REA MCP inspected shipped managed assemblies without loading
or executing them. This note records declarations and call relationships, not
a reproduced UI session or a claim of algorithmic equivalence. No proprietary
implementation or assets are included in terminal-rs.

## Evidence and limits

The full REA bundle remains outside Git in the session's TC2000 analysis scratch
directory as `easyscan-evidence.json` (about 102 MB). Exact Evidence IDs below
refer to retained `inspect_managed_members` records. The Data assembly SHA-256
is `5e0b4d788b7a2f45a7bc79395b4c172be0d27d7efc5d878ea5182deda596932c`.
REA reports partial metadata: duplicate Module/Assembly rows, an invalid GUID
heap reference, and no recovered MVID. Tokens are coordinates in this artifact,
not stable cross-version identifiers. Some call targets are unresolved. Static
call presence does not establish that a branch executes or that a target is
reachable in the running product.

| Reference | Evidence ID | Scope |
|---|---|---|
| S | `ev_0ece1c8905ca24d316e88f9c682766801b6987f5036c71e023b17f52cd3465c7` | Data: SymbolScan, SymbolScanCondition, SymbolScanRunner |
| C | `ev_c044f7d607b07fe332aec6f3f337ec95c94d28493c9df6a49ea6813783ad0f2e` | Data: MQLScan, baseCondition, conditionGroupAnd/Or, ScanManager |
| A | `ev_438818e24474085ab113cd98bf8d1d4e9840d020351c707c1753dcff2c4892fa` | Data: AutoRefreshScanManager |
| R | `ev_1c2d013a1ba8b260ffe7e69d4c7490fc25f7964efe24735257077baa23edeaa4` | Data: asynchronous DoScan_Imp state machines |
| N | `ev_14490fc46483c43274d56521c8713381de15b6c9c99346262822fefcab7020c6` | Data: MQLSingleUseSocket and AutoRefreshScan |
| W | `ev_07a3fa0f72f6c009ba131207d28fe0974744602fc3f0fb98a7b7f90dcbd5227c` | WatchList declarations, references, and call anchors |
| T | `ev_4f5f2585c56c67720b64e03988aecd80b6dc597e2cfc8e005262d7462ce64627` | TMain declarations and references from the earlier pass |

Unfiltered Data and Charting member inspection exceeded REA's 64 MiB
representation budget; exact Data type selections succeeded. Charting's
single-type probe (`WBI.Web.Charting.Chart`) was unmatched. That probe does not
establish absence of chart integration.

## Feature map

### Universe and scan definition — partially answered

**Observed, static (S):** SymbolScan exposes a name, UID/ScanGUID, a list of
conditions, source watchlists (`ListsToScan`, `AddListToScan`, `RemoveListToScan`,
`ClearListsToScan`), and an override source list. `AddListToScan` calls
`IWatchList.get_UID` and `UpdateActiveSources`. `SymbolsToScan` reads source lists
and symbol BFCIDs. Subscription and source-change methods are also declared.

**Inference:** a saved scan combines a reusable condition definition with one or
more symbol sources, and its result can act as a dynamic watchlist. WatchList
references include ComboList and system/industry watchlists (W). Exact union,
intersection, duplicate-handling, and exclusion semantics remain **unknown**.

### Conditions and timeframes — partially answered

**Observed, static (W, C, S):** condition families include above/below,
greater/less than a value, crossing up/down, new high/low, PCF, and AND/OR groups.
SymbolScanCondition declares period, minimum/maximum values, first/second
sources, and conversion to an MQL condition. `baseCondition` declares timeframe,
bar offset, minimum bars, calculation eligibility/reasons, and calculation
methods. Group calculation methods call child `baseCondition.Calculate` and
construct boolean datasets. SymbolScan has timeframe mode, a shared timeframe,
target date, and `AllConditionTimeframesSame`.

**Inference:** conditions form a tree and can carry their own timeframe, with
scan-level overrides. Exact indicator formulas, boundary comparisons, PCF
grammar, missing-bar treatment, session adjustment, and root combination rules
are **unknown**; names alone do not prove these semantics.

### Snapshot execution and results — answered at call-relationship level

**Observed, static (S, R, N):**

1. `SymbolScan.RefreshSnapshot` constructs/cancels a SymbolScanRunner and calls
   `DoScan`. `MakeMQLScan` constructs MQLScan, supplies a target date, and calls
   each SymbolScanCondition's `MQLCondition`.
2. The runner's asynchronous state machine is TypeDef `0x02000328`, nested under
   SymbolScanRunner (`0x02000326`). Its `MoveNext` is `0x060024b3`. Identically named
   state machines also exist for count/backtest runners; they must not be mixed.
3. That state machine calls `ISymbolScan.MakeMQLScan`, subscribes to
   `MQLScan.ResultsAreIn`, and calls `MQLSingleUseSocket.RunMQLQueryOnce`.
4. `RunMQLQueryOnce_Imp` calls `AddSnapshotScanDP` and `ConnectIt`.
5. `SymbolScanRunner.MQLScanIn` reads `MQLScan.Results`, removes its event
   subscription, and calls `FinishScan`. FinishScan resolves symbol IDs through
   SymbolManager and invokes ScanComplete.

**Inference:** this path sends a scan query through socket infrastructure and
receives matching symbol IDs asynchronously. It is evidence of a service-backed
query path, not proof that all TC2000 condition evaluation is remote. Endpoint,
wire protocol, server evaluator, service cadence, and latency are **unknown**.
The client also declares local calculation methods (S, C).

The runner exposes start/result counts, elapsed time, completion state, and
cancellation (S). WatchList declares progress controls, filter modes, scan tabs,
and scan status events (W); actual layout and interaction order are **unknown**.

### Refresh — partially answered

**Observed, static (S, A, N):** SymbolScan separates streaming and snapshot modes.
`DoManualRefresh` calls `RefreshTimeSpan` and `ReScan`; `ReScan` references both
snapshot refresh and auto-refresh management. Conditions/timeframe changes have
rescan paths. AutoRefreshScan.Start calls `AutoRefreshScanSocket.AddAutoItem` and
registers with AutoRefreshScanManager. The manager checks
`FSCServices.MaxAutoScanCount`, and exposes start/stop and lifecycle methods.

**Inference:** automatic refresh is a separately managed subscription with limits.
Exact refresh intervals, entitlement values, reconnect behavior, and market-wide
capacity are **unknown**. terminal-rs must choose its own explicit refresh policy.

### Persistence and chart integration — partially answered

**Observed, static (S, C, T, W):** scan/condition objects expose XML serialization
and deserialization, GUIDs, modified state, and source references. ScanManager
declares SaveScan/LoadScans; LoadScans_Imp calls SavedUserDataRequest.GetItemsWithData
and UserDataCache.GetUserDataItems. TMain declares an EasyScan picker and replace
dialog. SymbolScan declares ShowChartWithconditions and a ShowConditionsChart
event; WatchList exposes condition columns and filter controls.

**Inference:** named scans are reusable saved objects integrated into watchlists
and charts. Storage schema, location, synchronization guarantees, overwrite UI,
and precise chart-opening behavior are **unknown**.

## Independent first version in terminal-rs

These are product choices derived from the feature map, not recovered TC2000
algorithms or a compatibility implementation:

- Start with the current watchlist as a bounded symbol universe. Fetch data
  through terminal-rs providers and evaluate conditions locally in Rust.
- Use typed price, volume, previous-bar change, SMA, and RSI operands with numeric comparisons.
  Offer explicit all/any combination and a single timeframe first. Defer PCF,
  nested condition editors, market-wide scans, and historical backtests.
- Run work away from the UI thread, expose running/completed state, and open
  matching symbols through the existing chart selection path.
- Keep saved scan definitions separate from transient results. Save name,
  timeframe, combination, and conditions. Versioned schemas, multiple universe
  choices, progress events, cancellation and refresh options can follow later.
- Make manual refresh the baseline. Any periodic refresh must state its cadence,
  prevent overlapping jobs, and cancel/ignore obsolete results after edits.
- Treat insufficient history or failed data as unavailable, never a passing
  condition. Surface simulated data prominently and preserve provenance so a
  fallback cannot appear to be a real market match.
- Define comparison boundaries, indicator warmup, and stale data behavior in
  tests using synthetic bars. No numerical parity claim is justified by this
  static research.

The research is sufficient to justify this bounded first version. Full TC2000
behavioral parity remains unresolved and would require focused runtime/UI
observations and access to the corresponding data service behavior.

## Implemented workflow and validation

Open `SCAN` (also available in the sidebar, command palette and pane menus),
choose a timeframe, edit conditions, and select Match all or Match any. Run scan
captures the current watchlist, up to 200 symbols. The worker fetches four
histories at a time with a 15-second timeout per symbol, then evaluates locally.
Results retain the captured universe and warn if the watchlist subsequently
changes. There is no automatic refresh or cancellation in this version.

Operands are close price, volume, previous-bar percentage change, SMA and Wilder
RSI, compared to constants or other operands. Comparisons are strict above/below,
inclusive at-least/at-most, and crossings. Crossings require the previous bar on
the opposite side or equal and the latest bar strictly across. Indicators use
terminal-rs formulas, not recovered TC2000 formulas. The latest bar can still be
forming. Results display bar timestamps and age without claiming real-time data.

All/Any uses three-valued logic: unknown measurements cannot independently pass.
For All, a known false condition determines a non-match; for Any, a known true
condition determines a match. Otherwise unresolved measurements make the result
unavailable. Missing Yahoo volume and unsupported Treasury volume stay unavailable
for volume conditions; explicit reported zero remains valid. Provider errors are
not replaced with synthetic bars; explicit sample mode is labelled simulated.

Save scan stores a named definition in `scans.json` alongside the existing app
data. Saving under the same case-insensitive name replaces that definition.
Definitions also persist with workspace layouts; transient results do not.
Corrupt libraries are reported and preserved rather than overwritten. Clicking a
result uses the existing chart/link-group path.

Validated in Ubuntu WSL with a task-local Rust toolchain: `cargo test --workspace`
(292 passed, 5 network tests intentionally ignored), strict Clippy for core/data/app
with all targets, workspace formatting, and an eight-second native app smoke run
using an isolated sample-data scanner workspace (218 frames, successful exit).
Headless egui tests exercise narrow/wide empty, running and error-result states;
library and workspace tests cover persistence. Numerical/provider tests cover
warmup, crossovers, missing data and simulation provenance. This does not establish
live-provider or Windows-native UI parity.

## Second iteration: operands, presets and market-wide universes

Driven by StockCharts-style scans that the first version could not express, for example
`[sma(1,daily volume) > 100000] and [daily rsi(14) >= 70] and
[daily close >= daily ema(4,daily close)] and [yesterday's daily rsi(14) < 69.9]`:

- **Operands.** EMA joins SMA, and both take a source (close or volume), so
  `sma(1, volume)` and `ema(50, volume)` are expressible. ATR (Wilder) joins RSI; ATR(1) is the
  latest true range. Both reject gaps anywhere in their seeded history, like RSI.
- **Bars ago.** Each side of a condition may be read N bars back (0–250): `RSI(14) [1 bar ago]`.
  A shifted side needs that many more bars; a shift before the first bar is unavailable, not
  clamped. Crossovers compare the shifted bar with the one before it.
- **Multiplier.** The right side may be scaled (`Volume >= EMA(50, Volume) × 2.5`).
- **Compatibility.** New fields have serde defaults (close source, 0 bars ago, ×1), so scans
  saved by the first version load unchanged; a test pins the old JSON.
- **Presets.** Momentum Ignition (the scan above) and Volume Thrust
  (`[sma(1,volume) > 100000] and [rsi(14) >= 69.8] and [volume >= ema(50,volume) × 2.5] and
  [close >= 0.2] and [ATR(1) > 0.50]`) are built in, listed under PRESETS in Load scan, and
  each has a synthetic-bar test that matches the intended setup and rejects a near miss.

**Universes.** All US stocks, Nasdaq and NYSE (with NYSE American) come from Nasdaq Trader's
`nasdaqlisted.txt` and `otherlisted.txt`, cached for a day. Filters: ETF and test-issue flags,
plain tickers with an optional one-letter class (`BRK.B`, but not `.U`/`.W`/`.R`), and names
without warrant, unit, right, preferred, note, debenture, depositary or `%`. On 2026-10-20 this
kept 3,270 Nasdaq and 2,361 other listings (5,628 after de-duplication). Keys are written as
`<SYM> US Equity` so tickers such as `CL` resolve as stocks, not commodities. A changed file
header is an error rather than an empty universe.

**Scan store.** Universe scans read `scan_bars.sqlite3`, separate from the chart cache, which
records each symbol's fetch time, source and simulated flag. Stored bars are reused while fresh:
during a (conservatively wide) US session for 15 minutes, otherwise if fetched after the latest
weekday 21:15 UTC close boundary. Stale symbols fetch only their newest 10 bars; new ones fetch
full history. Fetch failures are per-symbol unavailable rows. Scans report progress every 25
symbols, can be stopped, and stop when their pane closes.

**Data source.** SnapTrade has no price history and warns against polling its quotes, so it
cannot feed a scanner. Questrade's personal API serves candles (2,000 per request) and symbol
search; with a token saved, stock scans use it exclusively, paced to ~16 requests/second.
Validated live: a full 5,628-symbol Momentum Ignition run completed from an empty store
(about half the universe was done four minutes in). Store reuse is covered by tests, not yet
timed live. Comparing match lists with
StockCharts or Finviz remains to be done; formula differences (RSI/EMA seeding, history depth,
adjusted prices) can legitimately produce small disagreements.
