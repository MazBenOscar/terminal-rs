# Chart interactions

## Auto and Logarithmic: 2026-10-21 follow-up

REA exercised the right-axis context menu: initial Auto + Regular changed to
Auto + Logarithmic, then manual + Logarithmic, then Auto + Logarithmic again.
Auto and the scale transform are independent. The menu describes Auto as fitting
data to screen; Regular, Percent, Indexed to 100 and Logarithmic are scale choices.
This is scale behavior, not an algorithmic signal-generation mode.

The DOM also exposes A / Toggle auto scale and L / Toggle log scale controls in
an area marked hidden. Hover reveal/direct button clicks, drag-to-manual behavior,
per-pane persistence and exact numeric margins were not verified. See S1/S2 and
the proposed independent Auto/Log controls in the
[focused toolbar/scale report](toolbar-scale-followup.md). terminal-rs already
has automatic/manual scaling and Log scale; the proposed gap is visible access
and explicit state, not the absence of those calculations.

## Observed

- **Chrome around the canvas** (`ev_5888cfc1`, accessibility tree): top toolbar (symbol button, interval
  button, chart-type button, Indicators, layouts, Alert, Replay, undo/redo); left vertical drawing toolbar;
  right price scale; bottom time scale; bottom bar with quick ranges; right sidebar (watchlist, details, news).
- **Legend / OHLC readout** top-left: `Symbol · interval · exchange`, then `O H L C`, absolute and % change,
  coloured by bar direction; a `Vol` row underneath; a collapse chevron for indicator legends.
- **Price scale**: right-aligned; a highlighted label for last price and a second label for the
  overnight/extended price; a dotted horizontal line across the pane at the last price.
- **Volume** is drawn as an overlay at the bottom of the main pane (not a separate pane) with
  up/down colouring at reduced opacity; its last value is labelled on the price scale.
- **Quick ranges** (bottom-left): `1D 5D 1M 3M 6M YTD 1Y 5Y All`. Accessible names reveal each range
  implies an interval: 1D→1m, 5D→5m, 1M→30m, 3M→1h, 6M→2h, YTD→1D, 1Y→1D, 5Y→1W, All→1M.
- **Bottom-right**: live clock with timezone (`02:56:25 UTC`), session toggle (`RTH`, appears on intraday),
  `ADJ` (dividend adjustment) toggle.
- **Time axis labels** adapt to zoom: month names on daily, `HH:MM` with day numbers at day boundaries on 5m
  (`ev_16ea0e50`, step w5).
- **Interval change re-scales immediately**: switching 1D→5 m redraws with a new axis and the last-price
  label recoloured by the latest bar's direction (`ev_16ea0e50`).

## Documented

- Mouse wheel zooms the time axis around the cursor; drag on the pane pans; drag on the price scale
  stretches price; drag on the time scale stretches time; double-click a scale resets it (auto-scale).
- Alt+R resets the chart view. Ctrl/Cmd+Up/Down zoom in/out; Left/Right arrows step the view by a bar.
- Crosshair modes: cross, dot, arrow; crosshair shows price label on the price scale and time label on the
  time scale; legend values follow the hovered bar.

## Unknown

- Wheel and drag gestures could not be issued by the scenario runner; Ctrl+Up and arrow presses produced no
  visible change headless (focus likely needs a pointer inside the pane).

## terminal-rs implications

- Keep the legend as a single dense line with OHLC + change, coloured by bar direction; values follow the
  crosshair, falling back to the last bar.
- Draw volume as an overlay in the bottom ~20% of the price pane by default.
- Last-price line + coloured price-scale tag; time-axis label density chosen from pixel spacing.
- Quick ranges bar with the same range→interval pairing; implemented as commands too (`5D <GO>`).
- Gestures: wheel = zoom about cursor, drag = pan, drag price axis = price stretch, double-click axis = auto,
  `Alt+R` reset, arrows pan by one bar, `Ctrl+↑/↓` zoom. All handled on the UI thread with O(visible) work;
  bar geometry is uploaded to the GPU once per data change, and pan/zoom only changes a uniform.
