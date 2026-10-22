# TradingView chart UX study (REA, website mode)

Behavioural research that informed terminal-rs's chart and terminal UX.
**Behaviour only**: no TradingView code, scripts, icons, fonts, images or other assets were copied
into this repository. Screenshots were inspected in a scratch directory and are not committed.

## Method

The original observations come from REA `capture_browser_scenario` runs against the public, logged-out
chart page `https://www.tradingview.com/chart/?symbol=NASDAQ%3AAAPL` in headless Chromium
(1600x900, dark scheme, UTC, service workers blocked). No account, login or paid feature was used.
The dated follow-up reports below describe their own viewport, target and runtime
or static-analysis scope; their evidence is separate from these original runs.

| Evidence ID | Scenario |
|---|---|
| `ev_5888cfc16a02f1a70be8005ee743a0dda259ead198d5c0b15d9423e0410e439c` | 9 s cold load; screenshot + accessibility tree; network, WebSocket, worker and console events |
| `ev_c8bc591b3895ddaddd80deb570ee5b49985d0bd8da9d15df4336c10b11bf0757` | Interval control, chart-type menu, Indicators dialog with incremental search |
| `ev_16ea0e50664767b2f12b97912372160f577d9129a0b8838c4d15b2386c96a22d` | Keyboard: type-to-search symbol, type-to-change interval, Alt+H, arrows, Ctrl+Up; drawing tool groups; layout selector |
| `ev_ae39d4c9…`, `ev_2021143a…` | Failed attempts (canvas click blocked by overlay canvases); kept for provenance |

Labels used in every note:

- **Observed** — seen in a capture above (cited by evidence ID prefix).
- **Documented** — publicly documented TradingView behaviour (help centre / widely published hotkey lists).
- **Inferred** — reasoned from observations, not directly seen.
- **Unknown** — could not be exercised (e.g. needs a mouse pointer position, login, or wheel/drag gestures,
  which the scenario runner does not provide).

## Notes

- [Crypto C01/C02 delivery](crypto-delivery.md) — implementation guide, source
  identity migration, exchange discovery/scanning, rolling metrics, validation
  results and remaining boundaries. This is a delivery record, not REA evidence.

- [Quick tools and Auto/Log follow-up](toolbar-scale-followup.md) — 2026-10-21
  REA website runtime tests of drawing favorites, reload, scale-state transitions
  and indicator access; includes explicit account/interaction limits.

- [TC2000 remaining gap backlog](tc2000-gap-backlog.md) — current implementation
  priorities reconciled against `personal/main` at `cfee48c`, including decisions
  on hold and parity/data limits.
- [TradingView Desktop investigation](tradingview-desktop.md) — separate
  2026-10-21 static REA inspection of the installed Windows application, its
  chart-linking/layout/notification adapters, evidence ledger and coverage limits.

- [TC2000 EasyScan](tc2000-easyscan.md) — separate static REA study of scan definitions,
  condition sets, snapshot/streaming query paths, and the scope of the independent Rust scanner.

- [interactions.md](interactions.md) — pan/zoom, crosshair, scales, legend, quick ranges
- [drawing-tools.md](drawing-tools.md) — tool taxonomy, hotkeys, magnet, fib levels
- [indicators.md](indicators.md) — dialog flow, defaults, panes
- [layouts-hotkeys.md](layouts-hotkeys.md) — multi-chart layouts, sync, symbol/interval entry, hotkey table
- [data-streaming.md](data-streaming.md) — WebSocket protocol shape, sessions, update cadence, paging

## Headline takeaways for terminal-rs

1. **Typing anywhere is a command.** A letter opens symbol search pre-filled with it; a digit opens an
   interval entry pre-filled with it (`5` → "5 minutes"), Enter applies. This maps directly onto a
   Bloomberg-style command line (`AAPL <GO>`, `5 <GO>`).
2. **Small initial window, page on demand.** The chart requests 300 bars, then streams deltas. Our
   provider trait fetches a bounded window and pages history when the viewport nears the left edge.
3. **Everything on the chart is a "series" or "study"** sharing one time index; studies are computed
   server-side and pushed as `du` deltas. We compute indicators locally (cheap at 1M bars) but keep the
   same "study attached to series" model.
4. **Layouts are a grid picker with 1–16 panes and per-dimension sync** (symbol / interval / crosshair /
   time / date range). Crosshair sync is on by default.
5. **Drawing tools are grouped, each with an Alt hotkey**, plus a global magnet (snap to OHLC).
