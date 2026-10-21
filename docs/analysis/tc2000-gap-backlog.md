# TC2000-to-terminal-rs remaining gap backlog

Reviewed: 2026-10-21. Source baseline: `cfee48c` on `personal/main`, including the
merged `codex/chart-qa-fixes`. This is a reconciliation of the existing TC2000 REA
research with the current Rust implementation, not a new TC2000 runtime audit or
a claim of complete compatibility. TradingView desktop investigation is separate.

The user reports clean formatting/strict Clippy and **442 passing tests, 5 ignored**
at this baseline, plus macOS running-app QA and an independent code/UX review.
The older roadmap's 416-test total and failed WSL display attempt describe the
earlier `35924b9` delivery, not the current overall validation state. This document
does not independently rerun or certify those reported checks.

## Evidence and interpretation

- [EasyScan static feature map](tc2000-easyscan.md): installed TC2000
  `25.0.9511.21172`; refs S/C/W establish declarations for scan conditions,
  timeframe overrides, AND/OR groups, PCF, watchlist columns and chart integration;
  refs A/N establish automatic-refresh infrastructure. Static declarations and
  calls do not establish runtime availability, exact formula semantics, service
  cadence or account entitlements. Retained Evidence IDs are in that report.
- [Charting comparison](tc2000-charting.md), refs C/P/D: chart/source/pane/scale,
  template, drawing and linking infrastructure. Its `6150b4f` Rust comparisons are
  historical; do not treat its original missing-feature list as current.
- [Indicator inventory](tc2000-indicators.md) and linked JSON catalogs: bounded
  installed calculator discovery and documented indicator candidates, with explicit
  parity limits. Counts describe that research baseline, not a compatibility score.
- [Delivery roadmap](../chart-roadmap.md): shipped chart increments, provider paths
  and limitations; current source is authoritative when old descriptions differ.

Classification below distinguishes **implementation gap**, **unverified parity**,
**external data/validation**, **product decision** and **scope candidate**. A
candidate is a proposal requiring a further requirements/data audit, not a
confirmed absence of a TC2000-equivalent feature.

## Prioritized backlog

| ID / priority | Classification | Remaining work and completion evidence |
|---|---|---|
| T01 / first | Implementation gap | **Independent study histories.** Fetch each study's native interval and sufficient seed history, then align completed values to the chart. Current `multiframe::evaluate` aggregates loaded chart bars, rejects lower-than-chart study intervals and requires a source/child chain to share its study timeframe. Preserve session/source/adjustment provenance, no-future alignment, paging and replay cutoffs. Complete with sparse-history, missing-bar, source-change, calendar and replay tests. |
| T02 / first | Unverified parity, plus bounded implementation gaps | **Formula and indicator compatibility.** Expand the documented PCF subset deliberately; do not promise unrestricted TC2000 syntax. Use matched attributed TC2000 OHLCV/output exports for initialization, offsets, units, zero/flat data, missing bars and parameter combinations. A reference-fixture harness already exists in `indicators/parity.rs`; acquiring and validating real fixtures is pending, not creating the harness. Worden BOP/TSV and other proprietary mappings remain unverified despite matching names. Recheck historical inventory findings against today's code before changing formulas. |
| T03 / next foundation | Implementation gap | **Reusable condition definitions.** Introduce one versioned condition model reusable by scans, chart plots, alerts and columns, with nested ALL/ANY groups, per-condition timeframes/explicit overrides and bounded PCF predicates in the scanner. Current scanner has a flat condition list, a shared timeframe and typed operands; PCF chart/alert support and scan-to-PCF translation do not make PCF a general scanner operand. Preserve history requirements, eligibility/error reasons, source/missing-data policy and one calculation meaning across consumers. |
| T04 / next workflow | Implementation gap | **Custom watchlist columns and ranking.** Persist formula-value columns and condition checkmarks, sort/filter by evaluated results and save column sets. Current watchlist quote columns and scanner result tables are not a general reusable formula-column/ranking system. Define explicit stale/unavailable states and bounded shared computations. TC2000 watchlist/condition-column static evidence is in EasyScan ref W; exact runtime editing/ranking behavior still needs observation. |
| T05 / next workflow | Implementation gap, external capacity | **Continuously updating scans.** Add explicit refresh schedules, cancellation/backpressure, incremental results and entered/exited-match highlighting. Existing watchlist and market-universe snapshot scanning, progress/cancellation, saved scans, navigation and plotted conditions are already delivered. Current selected-symbol condition alerts are not unattended universe rescans. Provider rate limits and coverage determine practical capacity; do not infer TC2000's service cadence from its static auto-refresh classes. |
| T06 / next workflow | Implementation gap, product choices | **Alert lifecycle and delivery.** Add repeat/rearm/cooldown policy and optional sound/desktop delivery; decide whether and how monitoring continues while the app is closed. Price, PCF and scan-condition alerts already exist, but are local, sampled and one-shot. Drawing alerts snapshot geometry; following later drawing edits needs explicit identity/deletion semantics. Test missed-poll/source changes, duplicate suppression and restart behavior. |
| T07 / data hardening | External data/validation, implementation gap | **Production coverage.** Validate live Alpaca authentication/entitlements with the intended account; test reconnects and implement required backfill. Expand stock streaming beyond its current opt-in 1m/default-history path when supported. Supply attributed earnings coverage and full venue/calendar schedules; the local import paths already exist. Reconcile adjustment/session conventions between charts, scans, alerts and other consumers. Provider support must be verified individually; available settings do not certify end-to-end consistency. |
| T08 / chart depth | Implementation gap, product choices | **Chart fills and exports.** Current band fill connects named outputs within one study; general cross-study fills need alignment/unit/ownership rules. CSV and SVG exports already exist; SVG is a price-chart image, not a complete screenshot with studies, drawings and panes. Add full-chart export if desired. Intervals 2m/3m/2H/quarter already exist; a general arbitrary-interval editor remains a separate design scope. |
| T09 / profile quality | External data, unverified equivalence | **Actual volume at price.** Current visible/session profiles estimate distribution from OHLCV bars. Tick/trade or provider volume-at-price data and aggregation rules are required for a more exact profile; do not represent the existing estimate as exchange trade distribution. |

### Source anchors at `cfee48c`

- T01: `crates/core/src/indicators/multiframe.rs`, `evaluate`; and
  `app/src/worker.rs`, `Request::Indicators` / `build_indicators_session`.
- T02: `crates/core/src/indicators/pcf.rs`, bounded parser/evaluator;
  `crates/core/src/indicators/parity.rs`, attributed reference fixtures.
- T03/T05: `crates/core/src/scanner.rs`, `ScanOperand`, `MatchMode`,
  `ScanDefinition`; `app/src/app/scanner_view.rs`; `app/src/chart_workflow.rs`.
- T04: `app/src/app/panels.rs`, `watchlist`.
- T06: `crates/core/src/price_alerts.rs`, `Rule` / `Book::observe_target`;
  `app/src/price_alerts.rs`; roadmap's Condition alerts section.
- T07/T09: roadmap's Events and provider options, Exchange calendars, Optional
  equity minute streaming, and Studies and display types sections.
- T08: `app/src/chart_fill.rs`, `Fill::build`; roadmap's Replay, exports and
  intervals section. These references are symbols, not stable line numbers.

## Decisions explicitly on hold

Do not change these without checking with the user:

1. **Account-overlay default:** `chart_account::Settings::default` currently
   enables overlays, so visible charts can trigger broker polling. Whether this
   should be opt-in remains the user's product decision.
2. **2H/4H equity aggregation:** `timeframe.rs` buckets on whole UTC hours. With
   the RTH filter the opening hour can drop out. Decide whether bars should align
   with the 09:30 exchange open, including short sessions/DST, before changing
   grouping, historical values, caches or downstream calculations.

## Scope candidates requiring a separate audit

- Historical scan evaluation/backtesting, with point-in-time universe,
  fundamentals, corporate actions and survivorship policy. Existing replay is
  not a backtesting/execution engine. EasyScan has static count/backtest runner
  declarations, but their full behavior was not recovered.
- Fundamental screening, sector/breadth workflows and richer options screening:
  establish the desired instruments, metrics, entitlements and existing Rust
  capabilities before scheduling. The original indicator inventory explicitly
  excluded complete fundamentals and Worden T2 breadth coverage.
- Additional drawing-management operations and navigation history: inventory the
  specific workflow before treating a generic category as a confirmed gap.

## Already delivered; do not reopen as missing

Indicator editing/chaining, panes/scales, templates, linked charts, session/timezone
settings, price alerts, scan-to-chart plots/traversal, account/order overlays,
eight-symbol comparison and percent/index modes, advanced drawings, anchored VWAP,
OHLCV profiles, Heikin Ashi/hollow/baseline/Renko, replay, CSV/SVG exports, extra
fixed intervals, event/import plumbing and optional Alpaca minute streaming are
implemented. Their specific limits above remain valid.

The macOS QA merge additionally fixes the replay crash and view preservation,
default comparison axis, comparison colors/tags, label overlap/plates, countdown
placement, Renko recovery/auto sizing, session-setting preservation, HA exports,
scan-plot replacement, anchor containment, stream auth backoff, calendar boundary/
DST checks, event-fetch throttling and replay/menu/legend controls. This is the
current baseline, not work to repeat from the older stacked feature branches.

## Implementation sequence and branch policy

Start with `codex/chart-study-history`, then `codex/shared-conditions`, based on
the latest `personal/main`; these are proposed branch names, not branches created
by this document. Follow with columns/ranking and automated scanning/alert policy.
Keep data validation/fixtures moving alongside the code. Merge `personal/main`
into an older feature branch before continuing it; merge rather than rebase.
Keep upstream `master` separate. No full parity milestone is complete until its
behavior and numeric reference evidence are supplied and pass documented checks.
