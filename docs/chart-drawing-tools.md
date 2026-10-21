# Chart drawing tools

Branch: `codex/chart-drawing-tools`, based on `personal/main` / `codex/price-alerts`
at `38f0eb6`. The baseline includes price alerts and the remote comparison-scale updates.

The chart's **right-click → Draw** menu and command palette offer these additions:

| Tool / command | Placement | Result |
| --- | --- | --- |
| Measure / `MEASURE`, `RULER` | Click start, then end | Price change, percentage change, bar intervals and elapsed calendar time |
| Rectangle / zone / `RECT`, `ZONE` | Click two opposite corners | Filled price/time zone |
| Ray / `RAY` | Click origin, then direction | A line extending from the origin through the second point to the plot boundary |
| Parallel channel / `CHANNEL` | Click baseline start, baseline end, then width | A filled parallel band with three editable anchor handles |
| Text note / `NOTE` | Click an anchor, enter text, Save note | A multiline chart annotation, up to 500 characters |

Existing trendlines, horizontal/vertical lines, Fibonacci retracement and their shortcuts
are unchanged. The new commands activate the focused chart's tool. A selected drawing
does not prevent placing another one. Esc cancels placement; unfinished channels and
unsaved new notes do not change saved drawings or add an undo step.

## Interaction

Click a drawing to select it. Filled zones, measures and channels can be selected from
their interior. Text notes can be selected within their text area. Drag handles to change
anchors, or drag the body to move the whole drawing. Channel movement includes its width
anchor; on logarithmic axes it preserves price ratios. Ray extension follows A → B, so
leftward and vertical rays work too. The channel's third point sets perpendicular width
in displayed coordinates; its parallelism follows the current linear/log transform.
Channels are finite bands between their baseline endpoints; extended channels are future work.

Double-click a note, or select it and choose **Draw → Edit text…**, to reopen its editor.
Save commits the edit; Cancel/Esc leaves the original unchanged. Deleting/undoing a note
while its editor is open cannot apply the draft to a different drawing. New notes are
added only on Save; an empty or whitespace-only note cannot be saved.

Use Delete, or **Draw → Delete Selected Drawing**, to remove the selection.
Ctrl/Cmd+Z and Ctrl/Cmd+Shift+Z undo and redo placement, movement, text edits and deletion.
Drawings retain the existing per-symbol ownership, optional all-symbol scope, color,
stroke width and magnet snapping. They remain timestamp/price anchored across pan, zoom,
timeframe and comparison-scale changes. New tools operate in the price pane, not indicator panes.

## Measurement semantics

The readout is end price minus start price. Percent is the change divided by the absolute
start price; it displays `n/a` if the start price is zero. Percent remains a price return
even when the comparison axis displays percent; the difference is not a percentage-point
change on that axis. Negative-price futures use the same absolute-denominator convention.

The bar count is the rounded distance between the two anchors in the chart's loaded bar
indices: it counts intervals, not both endpoint candles inclusively. It updates after a
timeframe change. Elapsed time is absolute calendar time between timestamp anchors,
including weekends and overnight gaps; it is not trading-session duration. Times are
shown in days/hours, hours/minutes or minutes/seconds. Reversing the anchors reverses
the price return while retaining the positive elapsed time and bar distance.

Measures are saved drawings, so they remain until deleted. Their readouts move with their
anchors, recalculate after edits, and are clipped to the price pane. Drawing levels and
notes do not expand price autoscaling. Nonpositive-price drawings are hidden on log scales.

## Persistence and compatibility

Saved layouts use **format v5**. Versions 1–4 still load; existing drawings default the
new optional channel-width and text fields. New drawings, channel width anchors, text,
symbol scope and style settings round-trip with the layout. Chart templates continue to
exclude drawings, and template versions are unchanged.

Older builds cannot read v5 layouts. Copy layouts before testing older branches, or use
a separate `TR_DATA_DIR` for review. This protects newer annotations from being silently
discarded by an older build.

## Review

1. Draw each tool using the Draw menu or its command. Check forward/reverse measures,
   zero-price handling where applicable, leftward/vertical rays and a three-click channel.
2. Drag endpoints, the channel width handle and each object's body. Check undo/redo and
   Delete. Cancel a partly placed channel and a new note; no drawing should be added.
3. Create and edit a multiline note. Verify Save, Cancel, Esc and double-click editing.
4. Pan, zoom, switch timeframes and toggle log/percent/own-axis comparison modes. Verify
   anchors remain on their intended timestamps and prices; measures report underlying
   price changes. Session gaps affect elapsed time differently from bar intervals.
5. Save/reopen the layout, then change symbols and return. Check ownership and style.

Automated checks cover ray clipping, degenerate geometry, channel parallelism and hit
testing, measurement math, actual pointer placement/selection/deletion, channel width and
body dragging on log scales, note creation/editing/cancellation, undo/redo, and layout
round-trips with legacy defaults. Sample-mode UI inspection covers all five tools on
a percent-scale chart. Fibonacci extensions, pitchforks, risk/reward tools and drawing
conditions for alerts remain later increments.

Validation: 392 workspace tests passed, with 5 existing network tests ignored.
Formatting and strict workspace Clippy checks passed. Tests and the sample UI
preview ran under WSL Ubuntu; the final restart check restored all five new tool
types from a saved v5 layout.
