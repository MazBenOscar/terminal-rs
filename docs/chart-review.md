# Charting review checklist

This increment implements the charting priorities with the boundaries below. It
does not establish complete TC2000 compatibility. Existing layouts migrate to v5;
new templates use v2 and accept v1. Older builds reject these newer versions.

For local price-level alert creation, monitoring rules and verification steps, see
[Price alerts](price-alerts.md). Chart interaction controls are covered in
[Chart interaction fixes](chart-interaction-fixes.md).
For the measure tool, zones, rays, parallel channels and text notes, see
[Chart drawing tools](chart-drawing-tools.md).

## Output styles and fills

Open **Indicators…**, then expand **Output styles & band fill** on a study.
Each named output has visibility, optional color, stroke width, and original/
line/dots/steps/histogram choices. A custom histogram color applies to both signs;
without it, columns retain the theme's positive/negative colors. Stroke width
affects lines and markers; column widths follow bar spacing.

Multi-output studies offer a fill between two selected outputs, with color and
opacity. For Bollinger Bands, select Upper and Lower. Fills respect the study's
pane, scale and visual displacement, skip undefined intervals, and use bounded
screen meshes when zoomed out. Fills remain enabled independently of individual
output visibility. Hidden outputs still supply calculations to child studies.

Review: give Bollinger Upper and Lower different colors, enable a faint fill,
and change MACD's signal width and histogram color. Save/reload the layout and
a template. Move the band study into another pane and try an independent scale.

## Linked charts and navigation

Place two charts in the same color link group. In each chart's context menu,
open **Linked navigation** and enable timestamp cursor and/or visible date range
synchronization. These options are independent and default off.

Cursors map timestamps into each chart's bars, with no price-axis synchronization.
A missing intraday session does not carry the last cursor forward across the gap.
Date ranges map to the target chart's available history and viewport limits;
older history pages in when available. Provider history limits can prevent an
exact match. Timeframe changes preserve the viewed date range where possible.

Review: compare a daily and intraday chart, then two symbols with gaps. Toggle
linking off and verify navigation is independent. Switching a symbol or choosing
a quick range clears a pending historical range from the previous selection.

## Time zones, sessions and refresh

The chart context menu's **Time zone, session & refresh** provides UTC, New York,
London and Tokyo display zones. Intraday labels use IANA daylight-saving rules;
daily and larger labels keep their trading dates.

US equity/ETF charts can filter intraday bars to regular hours or extended hours.
The profiles apply recurring US cash-equity holidays, common short days and a
selected list of exceptional closures. This is not a complete historical exchange
calendar. Regular hours use 09:30–16:00 Eastern, ending at 13:00 on supported
short days. The extended profile uses available bars from 04:00–20:00 Eastern.
It does not change the provider request, supply missing pre/postmarket bars, or
implement each venue's short-day extended closing schedule.

With a US session selected, intraday VWAP, VWAP bands and pivots use New York
session dates. Higher-timeframe aggregation anchors intraday periods to the
selected session open and caps their end at the session close. The chart itself
retains the provider's bar intervals; selecting a session cannot repair a provider's
pre-aggregated four-hour candles.

Automatic history refresh is optional, with a 15–600 second interval. It retains
the displayed bars while fetching and reports failures. Refresh does not imply
real-time market-data entitlement; the LIVE badge still requires feed events.
Repeated refreshes do not create duplicate feeds or replay the cold-start cache.
Corporate-action adjustments and actual vendor identity within a provider route
are still not harmonized.

Review: test a liquid equity intraday, compare New York and UTC labels around a
DST boundary, and verify that regular/extended filters only use available bars.
Try refresh with the network unavailable and confirm the error is visible.

## Study timeframes

Within **Calculation & offsets**, choose a study timeframe equal to or larger
than the chart timeframe. Larger periods are aggregated from loaded chart bars.
The first aggregated period is omitted because it may begin before loaded data.
A value is aligned only onto chart bars whose opening timestamp is at or after
the aggregate's end, so an unfinished daily/weekly result is not exposed early.

Source-linked studies must select the same effective study timeframe. For
example, set both RSI and EMA(RSI) to daily when the chart is intraday. Different
timeframes across a calculation edge produce an explicit error. Pane sharing
does not require matching timeframes. Intrinsically displaced outputs cannot
use a different timeframe.

This version does not independently fetch native higher-timeframe history, infer
missing bars inside a period, or support studies on a finer timeframe than the
chart. Scroll left for more history when a long-period study lacks warmup.

Review: put a daily SMA on an hourly chart and verify it changes only after the
daily period completes. Test a same-timeframe parent/child pair and a deliberately
incompatible pair; the latter should show an error.

## Custom formulas

Add **PCF · Custom formula (PCF subset)** using the indicator picker. Edit its
expression under **Calculation & offsets**. The result plots as a normal study;
conditions use 1 and 0, and undefined results remain blank. An EMA or other
supported scalar consumer can use the Formula output.

Supported syntax is deliberately bounded:

| Syntax | Meaning |
| --- | --- |
| `O H L C V`, `OPEN HIGH LOW CLOSE VOLUME` | Price/volume fields |
| `C1`, `H5`, `REF(C, 1)` | Historical values; future references are rejected |
| `+ - * / ^`, parentheses | Arithmetic; exponentiation is right-associative |
| `= == <> != < <= > >=`, `AND OR NOT` | Comparisons and Boolean expressions |
| `AVG/SMA`, `XAVG/EMA`, `WAVG`, `HAVG`, `RMA` | Averages: `(expression, literal_period)` |
| `SUM MAX MIN STDDEV` | Rolling operations: `(expression, literal_period)` |
| `ABS SQRT LOG EXP` | Unary operations; LOG is natural logarithm |
| `IF/IIF(condition, yes, no)` | Conditional values |
| `CROSS(a, b)` | Strict upward crossing from at-or-below |
| `RSI(period[, output_sma[, offset]])` | Plain RSI, optional SMA and historical offset |
| `WRSI(period[, offset])` | Wilder RSI |
| `ATR(period[, offset])`, `WATR(period[, offset])` | Simple/Wilder average of true range |
| `AVGC20`, `XAVGC20.1`, `MAXH10`, `MINL10` | Compact field/window forms with optional offset |
| `RSI14.2.1`, `WRSI14.1` | Plain RSI with output SMA/offset; Wilder RSI with offset |

Examples:

```text
C > AVGC20 AND V > 2 * AVG(V, 50)
CROSS(XAVG(C, 12), XAVG(C, 26))
RSI(14, 2, 1)
(C - AVG(C, 20)) / STDDEV(C, 20)
```

Expressions are limited to 4096 bytes, 512 tokens, 128 expressions and 32 parser
nesting levels. Periods are literal integers 1–1000; REF/offsets also accept zero.
Invalid arithmetic yields an undefined value. Undefined operands propagate
through Boolean operations; exact TC2000 undefined-value policies remain
unverified. Unsupported functions/fields and malformed expressions produce
errors. There is no general script execution, fundamentals lookup, proprietary
formula reconstruction, complete PCF grammar or scanner integration in this version.

The [official RSI syntax](https://help.tc2000.com/m/69404/l/747071-rsi-wilder-s-rsi)
distinguishes plain RSI's output SMA from Wilder RSI's offset; both forms are
tested separately. Existing scanner RSI conventions remain unchanged.

## Numeric reference comparisons

Run:

```sh
cargo run -p tr-core --example chart_parity -- path/to/reference.json
```

Use [the fixture schema example](../crates/core/tests/fixtures/manual-chart-reference.json).
It contains hand-computed values and is explicitly not a TC2000 export. Supply
the exact OHLCV bars used by the reference, unique increasing UTC timestamps,
study settings/IDs, and expected values in bar order (`null` means undefined).
Record source, symbol, export date, adjustment policy and session description;
choose absolute and relative tolerances appropriate to the exported precision.
Expected values describe calculation outputs before visual displacement.

The JSON report counts comparisons and mismatches and includes the first 20
differences with timestamps. Exit codes are 0 for a match, 1 for differences, 2
for invalid data. A comparison with no finite value pairs is rejected.

Matched TC2000 exports are still required to certify initialization, Hull rounding,
flat/gapped/short histories and proprietary studies. The tool makes those checks
repeatable; it does not manufacture evidence of equivalence.

## Remaining parity work

Complete PCF and proprietary study formulas; exported TC2000 numeric fixtures;
native provider-backed independent study histories and mixed-timeframe source
edges; complete historical/venue calendars; requesting extended-hours history;
corporate-action/source consistency and entitled equity streaming. Band fills
are local to one study; cross-study fills and full drawing-tool coverage remain
outside this increment. UI polish can follow the functional review above.

## Verification on this machine

The workspace suite passed with 358 tests and 5 existing ignored tests. After
adding the final hidden-band autoscale regression, the app suite passed all 108
tests. Formatting and strict workspace/all-target Clippy passed under WSL.
The manual reference comparison reported zero differences. Headless egui checks
exercise a narrow split-pane chart with a band fill, weekly overlay and PCF pane.

A visible application inspection could not run: WSL's Wayland compositor was
unavailable (`NoCompositor`), and X11 reported `XOpenDisplayFailed`. These checks
do not substitute for reviewing the running UI on your machine.
