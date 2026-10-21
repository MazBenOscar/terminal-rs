# Chart studies

Open **Indicators…** in a chart's header, or right-click the chart and choose
**Indicators → Manage Indicators…**. This editor applies to that chart only.
Click or right-click a study's legend for show/hide, edit and remove actions.
Hidden studies remain as muted entries on the price pane. Edit opens the same
editor at that study; removal also removes its calculation descendants.
Use the search field and picker to add a study. Repeated additions create
independent instances, including multiple RSI, Bollinger and MACD configurations.
Commands still toggle the first matching code/parameter combination.

Each study has a stable number, numeric parameters, visibility and optional custom
color. **Move up** changes pane/legend order without changing calculation inputs.
Settings are saved automatically with the workspace layout. Layout format v4
retains the original calculations when migrating older labels and settings;
older application versions cannot read v4 layouts. Back up layouts before switching
back to an older application build.

## Inputs, panes and scales

These are separate choices:

- **Input** selects the values used for calculation. SMA, EMA, WMA, HMA, RMA,
  Bollinger Bands, bandwidth, %B, envelopes, RSI and MACD accept open/high/low/close, volume, typical price
  (HLC3), or another study's named output. **Original bars** preserves the study's
  original OHLCV definition. Additional close-based moving averages and statistics
  expose the same Input selector. OHLCV-only studies retain their original input;
  volume-weighted averages keep the chart's original volume weights.
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

## Calculation methods and offsets

Expand **Calculation & offsets** within a study:

- RSI offers Wilder and plain RSI. Plain RSI averages gains and losses over the
  rolling period and supports an optional SMA of the resulting RSI.
- ATR, MACD, envelopes and Bollinger studies offer simple, exponential, front-weighted,
  Hull and Wilder averages. Bollinger's selected average changes the center;
  its population standard deviation remains measured about the arithmetic mean.
- Bollinger bandwidth offers ratio and percentage units. Bandwidth and %B can
  leave division by zero undefined instead of using the legacy fallback value.
- **Bars ago** uses an earlier calculated value at the current bar. This causal
  lag is also passed to child studies.
- **Visual displacement** moves the plot left (negative) or right (positive).
  Children consume the undisplaced values, so a left-shifted parent cannot feed
  future values into another calculation. Positive displacement can extend into
  the right margin; pan to see it.

Existing saved setups keep Wilder RSI/ATR, a simple Bollinger basis, and bandwidth
in percent. The new methods are chart settings; scanner thresholds and calculations
remain unchanged. These choices follow the [documented conventions](analysis/chart-upgrade.md),
but exact TC2000 numeric parity still requires exported reference fixtures for
initialization, rounding and degenerate windows.

## Templates and drawings

In **Chart templates**, enter a name and select **Save setup**. Templates save
studies, source connections, panes, scales, formula options, chart type, log scale
and volume visibility in the data directory's `chart-templates` folder. They do
not save the symbol, timeframe or drawings. **Replace studies & style** restores
the setup; **Add studies** merges studies and remaps their source and pane links.
Saving the same valid name replaces that template. Corrupt or unsupported files
are preserved and reported as errors.

Drawings belong to the symbol on which they were created. Older unowned drawings
are assigned to their saved chart's symbol. Select and drag an endpoint to reshape
a drawing, or drag its body to move it. Whole-object movement on a logarithmic axis
preserves price ratios. Right-click **Draw** to edit the selected drawing's color,
width, trendline extensions, or visibility on all symbols. Delete removes the
selection; Ctrl/Cmd+Z undoes and Ctrl/Cmd+Shift+Z redoes drawing edits. This chart's
undo history is limited to 64 edits and lasts for the running session. Escape
cancels a drag. **Remove drawings on this symbol** preserves other symbols' drawings.

## History integrity and remaining limits

Chart history failures display an error and retain any already loaded history.
They never substitute generated bars; sample history requires explicit sample
mode. Cached history is labeled **cached**, and the cache is separated by configured
provider route. Older unqualified cache entries are ignored and fetched again.
A route can still choose among real upstream providers: the cache does not establish
exchange session, adjustment or actual vendor equivalence.

Per-output styles, band fills, linked navigation, timezone/session controls,
completed higher-timeframe studies and a bounded custom formula editor are now
available. See the [functional review checklist](chart-review.md) for controls,
examples and exact limitations. Complete formula compatibility, native independent
history, full exchange calendars and adjusted source consistency remain pending.

There are at most six additional panes. Hidden panes beyond the limit are noted
on the chart; their studies still calculate. Source chaining currently supports
the scalar consumers above. Intrinsically displaced output arrays (such as Ichimoku) are rejected as sources
until timestamp/offset alignment is implemented; ordinary named outputs retain
the chart's bar alignment. Studies share the chart's symbol and loaded history;
larger study timeframes derive completed periods from those bars.

Missing inputs remain undefined rather than becoming false signals: RSI blanks
the missing difference and the next adjacent difference, while MACD skips missing
closes and resumes its retained EMA state. Recursive studies can still change
slightly when more seed history is loaded. Errors are never substituted with a
different output port or a fabricated scalar input for OHLCV-only studies.

The [research inventory](analysis/tc2000-indicators.md) records the comparison
baseline and unresolved compatibility questions. Its generated registry snapshot
predates these changes and remains historical evidence, not current UI behavior.

See the [chart interaction review](chart-interaction-fixes.md) for the comparison
picker, selected-drawing deletion, header spacing and LIVE badge fixes.
