# Drawing tools

## Quick drawing favorites: 2026-10-21 follow-up

REA runtime observation now verifies adding Trendline to favorites from its tool
menu, a separate floating favorites item, show/hide without clearing selection,
same-profile reload persistence, and removal of the last favorite. The floating
item is present in DOM but is not an accessible button, so the accessibility tree
alone underreports this UI. Clicking the item completed; creating a finished
drawing through it was not verified. Drag placement/reordering, custom presets
and cross-device persistence remain unknown.

For evidence IDs D1–D5, exact limits and terminal-rs acceptance criteria, see
[the focused toolbar/scale report](toolbar-scale-followup.md). A configurable
favorites strip is a proposed access improvement to the existing Rust drawing
tools; the older MVP list below is historical, not the current tool inventory.

## Observed (`ev_16ea0e50`, `ev_5888cfc1` accessibility tree)

Left toolbar groups (accessible names): Cursors (pressed by default), Trend tools, Gann and Fibonacci
tools, Patterns, Forecasting and measurement tools, Geometric shapes / Brush, Annotation tools (Text),
Icons, Measure, Zoom in, Magnets, Keep drawing, Lock drawings and indicators, Hide all drawings,
Remove objects.

**Trend tools** flyout: Trendline `Alt+T`, Ray, Info line, Extended line, Trend angle,
Horizontal line `Alt+H`, Horizontal ray `Alt+J`, Vertical line `Alt+V`, Crossline `Alt+C`;
Channels: Parallel channel, Regression trend, Flat top/bottom, Disjoint channel; Pitchforks: Pitchfork,
Schiff, Modified Schiff, Inside pitchfork.

**Gann and Fibonacci** flyout: Fib retracement `Alt+F`, Trend-based fib extension, Fib channel,
Fib time zone, Fib speed resistance fan, Trend-based fib time, Fib circles, Fib spiral, Fib speed
resistance arcs, Fib wedge, Pitchfan; Gann box, Gann square fixed, Gann square, Gann fan.

Magnet tooltip text: "Magnet mode snaps drawings placed near price bars to the closest OHLC value".

## Documented

- Two-point tools (trendline, fib) are created by click–click or press–drag–release; a horizontal line is
  one click. Esc cancels an in-progress drawing. After placing, the tool reverts to the cursor unless
  "Keep drawing" is on.
- Selected drawings show endpoint handles that can be dragged; dragging the body moves the whole drawing;
  Delete/Backspace removes the selection. Shift constrains angles.
- Fib retracement default levels: 0, 0.236, 0.382, 0.5, 0.618, 0.786, 1 (plus extensions 1.618, 2.618… off
  or optional), each level labelled with its ratio and price.

## Unknown

- `Alt+H` without a pointer position placed nothing visible headless; whether it anchors to the last price
  or the cursor could not be confirmed (Documented: cursor price).

## terminal-rs implications

- MVP tool set: trendline (`Alt+T`), horizontal line (`Alt+H`), vertical line (`Alt+V`),
  fib retracement (`Alt+F`). Same hotkeys, click–click creation, Esc cancels, Delete removes the selected
  drawing, handles to edit endpoints.
- Drawings are stored in **data coordinates (time, price)**, never pixels, so they survive pan/zoom and
  timeframe changes; persisted per symbol in SQLite.
- Magnet toggle on `Alt+M` (bare letters are reserved for the command line)
  snaps to the nearest O/H/L/C of the bar under the cursor.
