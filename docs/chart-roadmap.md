# Remaining chart delivery

The working baseline incorporates price alerts, comparison scale modes and expanded
drawing tools through `03019b3`. The user requested continuing through the remaining
priorities without pausing for review after each increment.

Delivery sequence (completed increments are merged into `personal/main`; each keeps
its feature branch for review):

1. `codex/chart-account-overlays`: average costs, chart-mark P/L and open limit/stop orders.
2. `codex/chart-compare-scales`: several comparison symbols and indexed-to-100 scale.
3. `codex/chart-scan-workflow`: keyboard navigation, scanner traversal/conditions and
   last-price/session context.
4. Chart studies/types: anchored VWAP, volume profile and additional chart types.
5. Drawing/alert extensions: remaining advanced drawing tools and reusable conditions.
6. Replay, exports and timeframe extensions.
7. Event/provider coverage: implement supported data paths; explicitly record evidence
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
