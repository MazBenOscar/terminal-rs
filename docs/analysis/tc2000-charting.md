# TC2000 charting comparison and implementation plan

Research date: 2026-10-20. terminal-rs baseline: `6150b4f`, `personal/main`.
Target: installed TC2000 `25.0.9511.21172`.

## Recommendation

Prioritize charting before additional scanner features. Keep the existing Rust
renderer and improve the chart model and interaction layer incrementally.
The first user-facing milestone should be **indicator editing and resizable
indicator panes**, preceded by chart-data and drawing-ownership corrections.
Additional indicators alone will not address the main workflow gaps.

The [indicator inventory and composition study](tc2000-indicators.md) expands
this report with the official catalog, installed calculator declarations,
the complete Rust registry, EMA/Bollinger/RSI/MACD analysis, and source/pane/scale
relationships. It supersedes milestone 1's UI-first scope: the indicator model
must support explicit source/output references and independent pane/scale
membership before the editor can expose those operations correctly.

Use separate branches from `personal/main` for each milestone, merging reviewed
work into that integration branch. Keep `master` tracking upstream. Changes
suitable for upstream should be isolated into focused patches based on upstream
`master`, without the personal scanner dependency or TC2000 artifacts.

## Evidence and coverage

REA inspected managed metadata and CIL call relationships without loading the
assemblies. These establish declarations and static calls, not observed product
behavior, exact formulas, or runtime reachability. Coverage is partial: metadata
issues include duplicate rows, unresolved calls, and no recovered MVID. Tokens
are coordinates for this build only.

Charting assembly SHA-256:
`e37d5f30ca71ec0ba8cba4c207b07058d93b729749dde5df41daf43fc8148d7c`.

| Ref | REA retained Evidence ID | Scope |
|---|---|---|
| C | `ev_b8470f0fd2ab706cfda0b2418cb924283fcb734e0b73fecc1bc51df0366ce946` | WebChart, LinePlot, indicator discovery |
| P | `ev_65e97e80a8201b7664c47af9fdcefa7bc366cc35235073bcefd5d198fab63bb3` | PlotBase, LinePlotEditor, templates |
| D | `ev_af008cc9b89af17b0398e123e59b66eb461f453a6500c21a2b92dfb19d18be1e` | Drawing manipulation, style, Pane, ScaleLayer |

Portable evidence is outside Git at the session scratch directory:
`C:\Users\mbakh\.codex\visualizations\2026\10\20\01a15a59-5636-72c0-9352-b7a20879886e\tc2000-analysis\chart-tools-evidence.json`.
It contains 57 records, approximately 129 MB. This document contains feature
descriptions and independent Rust design proposals, not proprietary code/assets.

Windows Computer Use restored the running TC2000 window and read its accessibility
tree, including its chart watermark and demo/delay status. Repeated captures
showed a black client area. No chart gestures, menus, or settings were exercised;
their visual behavior remains unverified. No orders were placed.

## Comparison

| Area | TC2000 static evidence | Current terminal-rs source | Gap / proposed priority |
|---|---|---|---|
| Navigation | WebChart declares zoom, pan, undo-zoom, date scaler/slider; `ZoomTo` calls `AllDateScaler.ZoomToX` (C) | Cursor-anchored wheel zoom, plot drag, axis drag, quick ranges, reset, history paging already exist | Preserve historical viewing context across timeframe changes; optional navigation history |
| Indicators | `LinePlot.ShowEditor` calls `LinePlotEditor.Show`; PlotBase exposes style/visibility/source controls (C, P) | Registry parameters and command-based changes; no per-instance style/visibility/editor in the chart model | First user-facing milestone |
| Panes/scales | Move-pane and move-scale actions open `overlayMovePane`; combine actions call a scale chooser; pane/scale transfer paths exist (C, P, D) | Fixed overlay/oscillator classification and uniform oscillator heights, capped at six panes | Resizing and ordering first; arbitrary placement/shared scales later |
| Drawings | Drawing base declares point/object movement, editor, colors, thickness, extension, group and `PlotOnAllSymbols`; editor/serialization calls present (D) | Four tools with placement, selection, deletion and magnet; fixed styles; no ownership field | Correct symbol ownership, then endpoint/body dragging, styles and undo |
| Templates | Save actions serialize chart XML and open save dialogs; load paths request saved/factory templates (C, P) | Whole-workspace layouts save chart settings | Named local chart templates independent of symbol and workspace |
| Multiple charts | Shared date-pointer creation/positioning and timeframe-link declarations (C) | Color groups propagate symbols only | Optional timestamp crosshair linking, then time/range linking |
| Data/session controls | Not established by this selected TC2000 evidence | Silent history fallback; crypto-only live feeds; UTC labels; no session selector | Correct source labeling/cache isolation before visual expansion |

TC2000 sharing defaults, modifier keys, exact pane-scale rules, template overwrite
semantics and navigation limits remain unknown. In particular, method names do
not prove that linked timeframes preserve a historical date range.

### Navigation and linked charts

In C, `WebChart.SetSharedDatePointer` calls `ShowSharedDatePointer`, which creates
a `SharedChartPointer` and calls `SetPosition`. `PushTimeFrameChanges` references
symbol linking, desired chart length and the timeframe-changed event.
`PanChartWithMouse` reads desired end date/bar counts, maps dates and coordinates,
and updates the desired end date. These support a date-aware chart model; exact
runtime synchronization rules need direct observation.

Rust source: `app/src/chart_view.rs:170` resets viewport/manual scale during load;
`:213` changes timeframe through load; `:430` initializes the last 160 bars.
`crates/chart/src/viewport.rs` maps indexed bars to screen coordinates.
`app/src/app/workspace.rs:496` broadcasts symbols, while chart crosshair state
and rendering remain local (`chart_view.rs:683`, `:938`).

### Indicators, panes and templates

`IndicatorSpec` (`crates/core/src/indicators/registry.rs:241`) holds indicator code
and parameters. Placement follows registry classification. Pane geometry
(`app/src/chart_view.rs:647`) allocates uniform oscillator heights; there are no
persisted per-indicator appearance controls or resize ratios in this model.
Workspace persistence (`app/src/layouts.rs:59`) already stores indicators,
drawings and chart settings, providing a migration starting point.

### Drawings and data correctness

`Drawing` (`crates/chart/src/drawings.rs:41`) has tool and two anchors only.
`ChartState.set_symbol` (`app/src/chart_view.rs:200`) resets interaction state
but retains drawings, so the same annotations appear on subsequent securities.
This needs an explicit ownership policy and backward-compatible persistence.

`Fallback.history` (`crates/data/src/lib.rs:337`) returns sample history when an
initial provider request fails. Older-history paging correctly returns errors
instead of splicing samples. However, initial results are cached unconditionally
(`app/src/worker.rs:593`), and the bar cache lacks provider/provenance fields
(`crates/data/src/cache.rs:31`). The global source chip labels non-sample
providers "Live" (`app/src/app/chrome.rs:104`, `:284`), despite possible fallback.
The per-chart LIVE badge separately requires a recent feed event.

Only crypto routes to live feeds (`crates/data/src/markets.rs:191`). Equity
charts otherwise update through history loads. Yahoo excludes pre/postmarket
(`crates/data/src/yahoo.rs:51`); H4 uses fixed UTC aggregation rather than
exchange-session anchoring (`:173`, `crates/core/src/timeframe.rs:70`). Time
labels use UTC without a selectable display zone (`crates/chart/src/scale.rs:87`).
Corporate-action adjustments also vary by provider. These are verified Rust
limitations, not claims that the selected TC2000 evidence proves an alternative.

## Delivery sequence

### 0. Chart correctness — `codex/chart-data-integrity`

Make initial chart provider failure explicit; synthetic data must require an
explicit demo/sample selection. Partition cached history by source and invalidate
legacy ambiguous cache entries. Show actual data status: sample, cached,
historical/delayed where known, live only with supporting feed evidence.
Do not imply a provider supplies real-time equities merely because it is configured.

Acceptance: a failed live-provider request cannot display/cache synthetic bars
under that provider; changing providers cannot reuse another source's history;
offline errors and explicitly chosen demo charts remain usable. Test failure,
cache isolation, legacy cache handling, and stale async responses.

### 1. Indicator controls and panes — `codex/chart-indicator-controls`

Updated prerequisite: implement `codex/chart-indicator-model` as specified in
the [composition study](tc2000-indicators.md#revised-first-implementation-milestone).
This includes source chaining, repeated instances, calculation-method/unit
compatibility and missing-value handling before the following UI increment.

Introduce stable indicator-instance IDs and persisted instance settings, separate
from indicator calculation parameters. Keep calculation reuse possible when
only appearance changes. Provide a chart-local indicator list with parameter
editing, visibility, color and removal, plus draggable pane dividers and pane
ordering. Preserve existing registry defaults on migration.

First version keeps each oscillator in its own pane and price overlays on the
price pane. Arbitrary source chaining and shared/separate scale assignment can
follow after the model supports reliable instance identity.

Acceptance: two moving averages remain independently editable; hiding preserves
settings; changing appearance does not reload history; resizing panes preserves
time alignment, enforces minimum heights and survives layout save/load; old
layouts load with defaults. Verify invalid parameters, narrow panes, multiple
instances and indicator recalculation while editing.

### 2. Drawing editing — `codex/chart-drawing-editor`

Store drawings by canonical security identity; migrate legacy drawings to the
chart's saved symbol. Add endpoint and whole-object dragging, selected-object
style controls, and undo/redo for create/move/edit/delete. Keep timestamps and
prices as anchors so drawings survive pan/zoom/timeframe changes. Define an
explicit option for drawings shared across securities.

Acceptance: switching A to B to A restores A's annotations without showing them
on B; drag editing works at linear/log scales and with magnet enabled; undo and
save/load preserve identity, geometry and style. Drawing drag must not also pan.

### 3. Reusable templates — `codex/chart-templates`

Save named local chart-style/indicator/pane configurations. Apply a template to
the current security, with an explicit replace/merge choice. Keep symbol-specific
drawings outside templates. Version the format and validate imports.

### 4. Linked chart navigation — `codex/chart-linking`

Add optional timestamp crosshair synchronization within existing link groups.
Map timestamps into each chart's bars rather than copying pixel/bar indices;
handle missing sessions and different timeframes explicitly. Then preserve the
viewed date range during timeframe changes and offer optional range linking.

### 5. Sessions and refresh — `codex/chart-sessions`

Add timezone/session display controls and consistent aggregation policies, then
provider-supported equity refresh with truthful stale/delayed status. This work
depends on available market data and entitlement information, not just rendering.

## Validation and status

This pass changed documentation only. Source paths and static calls were
reviewed; no Rust tests were rerun because no runtime code changed. The
implementation milestones above are proposed and have not been built. Assess
each increment with focused behavioral tests, workspace formatting/lint/tests,
and visual interaction checks before merging into `personal/main`.
