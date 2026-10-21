# Chart studies

Open **Indicators…** in a chart's header, or right-click the chart and choose
**Indicators → Manage Indicators…**. This editor applies to that chart only.
Use the search field and picker to add a study. Repeated additions create
independent instances, including multiple RSI, Bollinger and MACD configurations.
Commands still toggle the first matching code/parameter combination.

Each study has a stable number, numeric parameters, visibility and optional custom
color. **Move up** changes pane/legend order without changing calculation inputs.
Settings are saved automatically with the workspace layout. Layout format v2
retains the original calculations when migrating v1 labels and older enum values;
older application versions cannot read v2 layouts.

## Inputs, panes and scales

These are separate choices:

- **Input** selects the values used for calculation. SMA, EMA, WMA, HMA, RMA,
  Bollinger Bands, RSI and MACD accept open/high/low/close, volume, typical price
  (HLC3), or another study's named output. **Original bars** preserves the study's
  original OHLCV definition. Other studies currently keep that original input.
- **Pane** chooses Price, Own, or another study's pane. Several studies can occupy
  the same pane. Drag its top divider to resize an oscillator pane; price keeps
  at least 40% of the chart's plotting space.
- **Scale** chooses the pane's shared axis, an independent axis, or a fixed numeric
  range. Independent axes use colored labels inside the plot on the left; shared
  axes appear on the right. Use separate scales for incompatible units such as
  volume and RSI. A fixed scale requires minimum smaller than maximum.

Hiding a study removes its plot but keeps its calculations available to children.
Removing a study also removes every indicator calculated from it. Indicators
which only shared its pane move into their own panes. The Remove button's tooltip
explains this behavior. Invalid imported source/pane links and unavailable output
ports appear as errors in the editor; source choices that create cycles are omitted.

## Example: price overlays and an RSI signal

1. Add EMA20, EMA50 and BB20/2. Keep Input on Original bars, Pane on Price, and
   Scale on Share pane axis.
2. Add RSI14 in its own pane. Its standard range is 0–100.
3. Add EMA9. Select the RSI study's **RSI** output as its Input. Set Pane to
   **With RSI** and share that pane's axis. This computes an EMA of RSI; merely
   moving an ordinary EMA into the RSI pane would not change its calculation.
4. Add MACD12/26/9 in its own pane. Its line, signal and histogram display together.

## First-version limits and conventions

This implements the chart study model, not complete TC2000 formula parity.
RSI and ATR retain Wilder smoothing; BB retains its SMA basis and population
deviation; BBW retains percentage units. Existing saved values and scanner
thresholds are not silently reinterpreted. Alternative RSI/ATR methods, configurable
Bollinger center methods, per-output styles, band fills, independent study
timeframes and a reusable template library remain future work.

There are at most six additional panes. Hidden panes beyond the limit are noted
on the chart; their studies still calculate. Source chaining currently supports
the scalar consumers above. Displaced output arrays are rejected as sources
until timestamp/offset alignment is implemented; ordinary named outputs retain
the chart's bar alignment. All studies use the chart's symbol/timeframe/history.

Missing inputs remain undefined rather than becoming false signals: RSI blanks
the missing difference and the next adjacent difference, while MACD skips missing
closes and resumes its retained EMA state. Recursive studies can still change
slightly when more seed history is loaded. Errors are never substituted with a
different output port or a fabricated scalar input for OHLCV-only studies.

The [research inventory](analysis/tc2000-indicators.md) records the comparison
baseline and unresolved compatibility questions. Its generated registry snapshot
predates these changes and remains historical evidence, not current UI behavior.
