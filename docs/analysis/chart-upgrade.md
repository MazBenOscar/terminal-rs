# Chart compatibility checkpoint

This increment extends the existing REA-based inventory with documented calculation
choices, reusable local study setups and drawing editing. It does not establish
full numeric or interaction parity with TC2000. The earlier
[indicator inventory](tc2000-indicators.md) remains the shipped-artifact baseline.

## Calculation evidence

Official TC2000 references checked on 2026-10-20:

| Reference | Implemented convention |
| --- | --- |
| [RSI and Wilder RSI](https://help.tc2000.com/m/69404/l/747071-rsi-wilder-s-rsi) | Plain RSI uses rolling averages of gains/losses; optional SMA smooths its output. Wilder remains the saved default. |
| [Bollinger Bands](https://help.tc2000.com/m/69404/l/744931-bollinger-bands) | Selectable center average; population deviation stays about the arithmetic mean. |
| [Bollinger bandwidth](https://help.tc2000.com/m/69404/l/744935-bollinger-bandwidth) | Ratio and percent differ by a factor of 100. Existing charts keep percent. |
| [Bollinger %B](https://help.tc2000.com/m/69404/l/744914-bollinger-b) | Relative position between lower and upper bands; undefined division is selectable. |
| [ATR](https://help.tc2000.com/m/69404/l/744766-average-true-range-atr) | True range with selectable averaging; the TC2000 simple ATR convention is available while saved charts keep Wilder. |
| [MACD](https://help.tc2000.com/m/69404/l/745913-macd-macd-histogram) | Chosen average for fast, slow and signal calculations; histogram is their difference. |

Simple, exponential, front-weighted, Hull and Wilder options reuse terminal-rs's
existing kernels. Tests cover hand-calculated rolling windows, band dispersion,
units, ATR and MACD values, missing outputs, and causal source chains. They are
not comparisons against exported TC2000 output. Exact seeds, Hull rounding,
flat RSI and missing-bar policies remain reference-fixture work. Legacy defaults
use the original computation path and scanner formulas are unchanged.

Historical lag is applied before a study supplies child inputs. Visual displacement
is applied only when building plots. Intrinsically displaced studies still cannot
serve as scalar inputs. No general TC2000 PCF expression interpreter is included.

## Interaction and data scope

- Named templates save the study graph and chart style. Merge remaps identifiers
  transactionally. Invalid graphs and unsupported/corrupt template files report errors.
- Drawing anchors and bodies can be dragged; styles, extensions, symbol ownership
  and session-local undo/redo are supported. Layout v3 stores the new settings and
  accepts older layouts.
- Chart failures no longer generate substitute history. Cache keys include the
  configured route and exclude legacy ambiguous entries. This isolates sample and
  provider routes, but does not prove which real vendor supplied each bar inside a
  route or harmonize corporate-action adjustments.

## Remaining work

Per-output line/dot/histogram styling and band fills; synchronized cursor and
navigation across charts; independent study timeframes; exchange calendars,
session boundaries and adjusted source consistency; exported numeric reference
fixtures; proprietary formula and complete PCF compatibility. All current studies
still use the chart's symbol, timeframe and history.
