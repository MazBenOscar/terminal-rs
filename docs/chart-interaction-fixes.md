# Chart interaction fixes

Branch: `codex/chart-interaction-fixes`, based on the combined chart/scanner
baseline `93e0eb1` integrated into `personal/main`.

## Changes and review steps

1. **LIVE placement:** the badge sits inside the price plot, below Buy/Sell,
   and is clipped out of the price-axis labels. Open a chart receiving feed
   updates and check the top ticks and last-price tag at narrow and wide sizes.
2. **Header spacing:** interval/type controls and Indicators/Compare buttons
   reserve their measured width. The tab strip leaves a separate gap before
   Add Panel. Narrow charts expose compact controls, with a Chart menu when
   both action buttons cannot fit.
3. **Interactive study legends:** click or right-click a price-overlay or
   oscillator legend to show/hide, edit, or remove that instance. Edit opens
   and scrolls the existing Indicators editor to the selected study. Hidden
   studies remain available as muted legend entries on the price pane so
   they can be restored. Legend clicks do not pan or place drawings.
4. **Selected drawing deletion:** right-click the chart, open Draw, and choose
   Delete Selected Drawing. It uses the same deletion and undo history as
   the Delete key. With nothing selected the menu item is disabled.
5. **Comparison picker:** open Compare in the chart header, its compact Chart
   menu, or the chart context menu. Choose a common symbol, search names/tickers
   in the local catalog and configured provider, or enter a full market key.
   Enter selects a valid typed key; Escape closes the picker. Remove clears
   the current comparison. Selecting the chart's own symbol is disabled.

The existing `CMP` command and saved comparison continue to work. Comparisons
still use a single symbol rebased at the visible left edge. Loading and errors
appear in the comparison legend. Search responses are routed to their own chart
picker, and superseded history results/errors cannot replace a newer selection.
Changing the main chart to its comparison symbol removes that self-comparison.

Visibility retains source calculations. Removing a study retains the existing
policy: calculation descendants are removed; studies only sharing its pane move
to their own panes. The legend menu explains the dependent-removal behavior.

The audit's Bollinger color issue is saved configuration. Its existing Color →
Reset control restores the default; this change does not rewrite user colors.
The existing `5` + Enter, left/right pan and +/- zoom shortcuts are preserved.

## Scope

This is the first delivery increment. Price alerts, additional drawing tools,
multiple comparisons/percent scales, account overlays and scanner/chart workflow
remain separate feature branches. This branch changes no layout format and does
not merge the interaction changes back into `personal/main` before review.

## Validation

369 workspace tests passed; 5 existing tests were ignored. Workspace formatting
and strict all-target Clippy passed under WSL. Pointer-event regressions exercise
legend menus on price/oscillator panes, hide/restore without drawing or panning,
selected-drawing deletion with undo, picker selection/removal, measured header
widths, LIVE/axis separation and stale comparison response rejection.

The running Vulkan app was inspected using an isolated sample-data folder at
wide and split-pane sizes. The header gap, plot-contained LIVE badges, legend
menu/editor transition, initial comparison choices and selecting a comparison
line were verified visually. Provider data availability remains source-dependent.

## Comparison scale

Branch `codex/chart-compare-scale` adds a **Scale** choice to the Compare window,
saved per chart with the layout (older layouts load as % change):

- **% change** (default): both symbols are rebased at the visible left edge, as
  before, and the price axis, crosshair tag and last-value tags read % change from
  that bar. Panning moves the base, so 0% is always the left edge.
- **Price axis**: the previous behaviour; the comparison is rebased onto the main
  symbol's prices, and its last-value tag shows its own price.
- **Own axis**: the comparison keeps its own prices, autoscaled independently on a
  left-hand axis with its own last-value and crosshair tags; the main symbol's
  autoscale ignores it.

The legend's "vs" figure is unchanged: the comparison's performance relative to the
main symbol at the hovered bar.
