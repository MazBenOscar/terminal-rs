# Agentic charts implementation plan

Status: design approved for implementation, 2026-10-22. No code has been written yet.
Phases are numbered A0–A5 (renamed from P0–P5 so they do not collide with the
performance programme P1–P6). See "Integration review" at the end for prerequisites
and ordering against the existing backlog.

This plan adds an agent chat panel that reads and edits charts. It covers drawings,
indicators, comparisons, the view, screenshots and, later, watchlists and alerts.
The agent runs on the user's local Codex CLI and their ChatGPT subscription.
Other local agent CLIs can be added behind the same interface.

The design comes from four sources:

- A behavioural study of CoinMarketCap Agentic Charts.
- A read-only map of the integration points in terminal-rs.
- Inspection of `codex-cli 0.163.0` (`codex app-server generate-json-schema --experimental`).
- Three reviews: a Claude architect review, a Codex self-review and a final review by Claude Fable 5.1.

Each phase lands on its own feature branch, following the existing workflow.

## A3E-FIX reviewer follow-up (2026-10-27)

This section supersedes the original A3e report below. All reads/edits stayed
inside `terminal-rs-precision`; no git command/state change, commit, release build,
app launch or network operation was used. The external scratchpad specification
was not opened because it lies outside the explicitly permitted directory; the
15-item task and this repository's plan were used. Reviewer tests and their hooks
were retained; all six A3e reviewer ignores were removed.

### Outcomes

| Item | Outcome |
|---|---|
| 1 | Post-last-touch through final-bar closes are checked at 0.35 ATR per bar. More than one breach, or a final-close breach, breaks the line even after recovery. Active `trendlines` excludes broken lines; separate `historical` contains only broken lines. |
| 2 | Score-first same-role deduplication rejects >=2 shared touches, or last projections <0.5 ATR apart with slopes whose next-ten-bar divergence is <0.5 ATR. |
| 3 | Bounds use observed swing/body reactions only; capped merges trim to dense touches, never to the latest close. In-play requires >=0.1 ATR interior clearance on both edges. The manager's **0.6 ATR cap is retained**. |
| 4 | Touch/close validation, final anchors, current projection and +10 projection all use bar indices, including irregular weekday histories. |
| 5 | Broken references enforce `(broken)` and dashed styling. Contradictory labels are replaced; harmless model context is preserved, adding the authoritative role when absent. Explicit historical references resolve exact anchors. |
| 6 | V2 ATR floor is relative only: abs(close) x 1e-12. Tiny-price regression now covers zone bounds/roles/touches/recommendations and active/historical line projections. |
| 7 | Core fractals accept equal highs/lows, choosing the first bar of a contiguous equal run; separated equal extrema remain distinct. |
| 8 | V2 swings exclude the still-forming final bar from confirmation. Major confirmation includes the opposite minor swing's +2 confirmation. V2 compatibility highs/lows use these same confirmed points. |
| 9 | Replacement is per kind and same symbol/timeframe; lines retain zones and zones retain lines. Real user edits, including moves/text/style, transfer ownership to User and protect drawings from replacement. |
| 10 | Host execution rejects symbol/timeframe mismatches with a clear re-run-analyze error, before applying drawings. |
| 11 | Host turn counters enforce <=6 referenced zones and <=2 referenced lines per role, across analyses/charts/calls; rejected attempts do not add drawings. |
| 12 | Manual-coordinate warnings require zone_role or support/resistance/trend label text. An ordinary unlabelled ray does not warn. |
| 13 | Live chart reconciliation clears notices when the target closes or its drawings become visible by any toolbar/tab path; stale hidden status updates while offscreen. No automatic reveal. |
| 14 | `recommended=true` marks up to six zones: in-play, nearest major each side, then score. Agent instructions use these flags and active-only lines. |
| 15 | Both unchanged Coinbase fixtures are reported below at default lookback: 393 D1 / 1000 4H bars. |

### Tests changed / added

- Enabled all six original `review_*` regressions in `review_a3e_tests.rs` and
  `review_a3e_registry_tests`; strengthened post-touch state/breach accounting,
  >=2-touch and projected/slope deduplication, weekday touch/current/+10 index
  consistency, tiny-price lines as well as zones, and broken label/style tests.
- Added `post_touch_breaks_survive_recovery_and_one_breach_is_allowed`,
  `plateaus_and_opposite_confirmation_exclude_the_forming_bar`,
  `recommendations_reserve_in_play_and_nearest_majors_then_score`, and
  `precision_h4_fixture_report`.
- Added core `fractals_accept_equal_extrema_and_select_the_first_plateau_bar`,
  host `analysis_refs_enforce_current_chart_and_per_turn_caps`, and pane
  `drawing_notice_reconciles_toolbar_tab_and_closed_pane_paths`.
- Updated existing per-kind replacement/object-cap, manual-ray warning,
  interleaved-user-edit ownership, D1 reference-role, and real D1/4H tool-contract
  tests. No fixture or golden/snapshot was changed. The unsupported 80,900
  reference test remains explicitly ignored, not reported as a successful match.

### Post-fix fixture tables

Both end at **2026-10-28 00:00 UTC**, close **80,628.95**. Tables show all returned
zones, not just the six recommended drawings. `S+C=T` is swing touches plus
respected closes. Prices are rounded here; cached coordinates keep full precision.
D1 has **no genuine in-play zone**: the former close-pinned band is support.
4H has one touch-derived, interior-clearance-qualified in-play zone.

| TF | ID | Role / strength | Low–high | S+C=T | Score | Trimmed | Recommended |
|---|---|---|---|---|---:|---|---|
| D1 | zone-0 | support / major | 79,107.00–80,246.85 | 11+16=27 | 30.3611 | yes | yes |
| D1 | zone-1 | resistance / major | 81,880.08–83,019.92 | 8+5=13 | 22.9153 | yes | yes |
| D1 | zone-2 | support / minor | 77,791.22–78,456.14 | 3+10=13 | 12.8621 | no | no |
| D1 | zone-3 | resistance / major | 84,261.99–85,188.53 | 4+7=11 | 14.7542 | no | yes |
| D1 | zone-4 | support / major | 75,794.72–76,845.71 | 6+9=15 | 19.8107 | no | yes |
| D1 | zone-5 | resistance / major | 86,257.15–87,397.00 | 7+6=13 | 20.5853 | yes | yes |
| D1 | zone-6 | support / major | 73,741.53–74,881.38 | 6+5=11 | 18.7520 | yes | yes |
| D1 | zone-7 | resistance / minor | 87,400.72–88,065.64 | 1+9=10 | 7.0171 | no | no |
| D1 | zone-8 | support / minor | 72,364.13–73,503.98 | 2+2=4 | 9.8924 | no | no |
| D1 | zone-9 | support / minor | 70,020.00–70,941.65 | 3+11=14 | 12.9913 | no | no |
| D1 | zone-10 | resistance / minor | 90,814.55–91,479.47 | 1+0=1 | 4.8094 | no | no |
| D1 | zone-11 | resistance / minor | 110,889.53–111,554.45 | 1+0=1 | 3.8887 | no | no |
| 4H | zone-0 | in_play / major | 80,491.12–80,908.88 | 10+12=22 | 28.7093 | yes | yes |
| 4H | zone-1 | resistance / major | 80,927.48–81,202.49 | 3+10=13 | 14.9592 | no | yes |
| 4H | zone-2 | support / major | 79,789.09–80,206.85 | 7+14=21 | 22.4563 | yes | yes |
| 4H | zone-3 | resistance / major | 81,302.25–81,720.00 | 4+15=19 | 16.9328 | yes | no |
| 4H | zone-4 | support / major | 79,091.12–79,508.88 | 14+15=29 | 37.6140 | yes | yes |
| 4H | zone-5 | resistance / major | 81,919.62–82,337.37 | 7+13=20 | 22.6037 | yes | yes |
| 4H | zone-6 | support / minor | 78,334.44–78,747.85 | 6+18=24 | 18.6227 | no | no |
| 4H | zone-7 | support / major | 77,518.12–77,935.88 | 20+25=45 | 47.5790 | yes | yes |
| 4H | zone-8 | resistance / minor | 83,513.00–83,930.75 | 3+21=24 | 13.1805 | no | no |
| 4H | zone-9 | support / minor | 72,985.39–73,229.09 | 1+2=3 | 5.0417 | no | no |
| 4H | zone-10 | support / minor | 69,900.59–70,144.29 | 1+1=2 | 12.4076 | no | no |

**Active trendlines** (all broken=false):

| TF | ID / role | Exact anchors (UTC) | Touches / style | Last projection | +10 bars | Post-touch breaches |
|---|---|---|---|---:|---:|---:|
| D1 | line-0 / falling resistance | 2025-10-06 126,296.00 → 2026-10-21 84,856.07 | 5 / solid | 84,092.70 | 83,002.18 | 0 |
| 4H | line-0 / rising support | 2026-09-15 16:00 74,887.50 → 2026-10-27 08:00 79,365.93 | 6 / solid | 79,437.58 | 79,616.72 | 0 |
| 4H | line-1 / falling resistance | 2026-10-02 12:00 87,249.05 → 2026-10-21 16:00 84,856.07 | 5 / solid | 84,065.35 | 83,857.26 | 0 |
| 4H | line-2 / falling resistance | 2026-10-25 00:00 80,925.00 → 2026-10-26 12:00 80,710.04 | 3 / dashed | 80,495.08 | 80,256.24 | 1 |

**Historical only** (all broken=true, dashed, labelled `(broken)` if explicitly drawn):

| TF | ID / role | Exact anchors (UTC) | Touches | Last projection | +10 bars | Post-touch breaches |
|---|---|---|---:|---:|---:|---:|
| D1 | historical-line-0 / rising support | 2026-08-14 62,468.21 → 2026-10-18 80,564.57 | 3 | 83,348.63 | 86,132.68 | 6 |
| D1 | historical-line-1 / rising support | 2026-07-06 61,250.00 → 2026-07-24 63,641.00 | 5 | 76,393.00 | 77,721.33 | 19 |
| D1 | historical-line-2 / falling resistance | 2025-10-13 116,077.51 → 2026-09-14 79,591.17 | 6 | 74,813.20 | 73,727.29 | 41 |
| D1 | historical-line-3 / falling resistance | 2026-05-06 82,814.23 → 2026-09-03 82,283.00 | 5 | 82,039.52 | 81,995.25 | 22 |
| 4H | historical-line-0 / rising support | 2026-08-16 20:00 62,631.18 → 2026-10-20 12:00 81,196.01 | 6 | 83,349.15 | 83,827.62 | 32 |
| 4H | historical-line-1 / rising support | 2026-09-20 00:00 80,085.00 → 2026-10-06 04:00 85,100.01 | 8 | 91,872.86 | 92,389.87 | 127 |
| 4H | historical-line-2 / falling resistance | 2026-10-06 12:00 86,698.35 → 2026-10-18 08:00 82,183.56 | 4 | 78,495.42 | 77,859.54 | 48 |
| 4H | historical-line-3 / falling resistance | 2026-09-22 12:00 86,734.00 → 2026-10-01 04:00 84,368.26 | 5 | 77,043.56 | 76,588.61 | 150 |

Reproduce with `cargo test --offline --locked -p tr-agent-tools precision_ -- --nocapture`.
The original 76,393 D1 rising line has recovered above its projection, but is
historical because 19 post-touch closes breached it by >0.35 ATR. Recovery does
not turn a long-broken line back into current support.

### Follow-up verification

- `cargo fmt --all`: passed.
- `cargo clippy --offline --locked --workspace --all-targets -- -D warnings`: passed, no warnings.
- `TR_CONGRESS_REFRESH_MIN=0 cargo test --offline --locked --workspace`: passed,
  **1,923 passed / 0 failed / 9 ignored**, including doctests. The equivalent Cargo
  configuration form `cargo --config 'env.TR_CONGRESS_REFRESH_MIN="0"' test
  --offline --locked --workspace` was used because shell environment-prefix
  assignments require approval. All six targeted reviewer tests passed; none is
  ignored. The remaining ignores are the pre-existing unsupported 80,900
  reference assertion, seven external-network tests and one manual news audit.

Logs: `target/a3e-fix-workspace-tests.log`; fixture tables:
`target/a3e-fix-fixtures.log` (generated build artifacts, not source files).

## Original A3e implementation report (pre-fix, superseded; 2026-10-27)

Implemented offline in `terminal-rs-precision`; no commit, git-state command,
release build, application launch, or network operation was performed. The supplied
reference image was outside the permitted repository and was not opened. Both
Coinbase CSV fixtures are retained unchanged. N4's `read_news`/`read_filings`
implementation and its inline schema tests were not edited.

### Per-item outcome

1. **Swings:** confirmed 2/2 minor fractals; major requires both 5/5 fractal
   confirmation and at least 1.5 ATR(14) reversal prominence on both sides to the
   nearest opposite minor swing. Prices remain exact high/low observations.
2. **Levels v2:** versioned output with configurable history, body-aware robust
   ATR-scaled clusters, role/strength/score, separate swing and respected-close
   counts, first/last touch timestamps, trimming metadata, overlap deduplication,
   in-play inclusion and nearest-major reservations. Up to three major and three
   minor per outside side are returned; the agent is instructed to draw at most
   six zones. Explicit v1 keeps the legacy analytical format/algorithm.
3. **Trendlines:** deterministic same-role swing pairs, rising lows/falling highs,
   at least three swing touches within 0.35 ATR at each touch, no more than two
   closes beyond the final line between extreme touches, and a touch in the final
   40% of history. Revalidated exact extreme swing anchors, current/+10 projections,
   broken status, solid >=4/dashed 3, and up to two lines per role. This new mode is
   always v2, even if a caller supplies version=1.
4. **Drawing provenance:** turn-local `analysis_id`/`item_id` references override
   model coordinates, kind, label, source timeframe, extensions and dash style.
   Rectangles use first touch to last analyzed bar and really extend right in
   rendering; rays use exact swing endpoints. Analysis timestamps are not snapped
   to other chart bars. Unreferenced technical drawings receive a warning.
   Prior-turn agent S/R for the same symbol/timeframe, including legacy labeled
   rays, is replaced through undoable drawing plans; user/Keep objects, other
   symbols/timeframes and same-turn additions are retained. Replacement works at
   the 100-agent-object limit.
5. **Agent guidance:** read chart/data on the current timeframe, analyze both v2
   levels and lines, use references rather than estimate coordinates, reserve
   nearest majors/in-play, limit zones/lines, report touches, and describe broken
   lines as historical rather than active. Editing turns have 16 tool calls;
   read-only turns retain eight.
6. **Visibility:** successful drawing results disclose hidden/target visibility,
   chart UID and additions. The pane retains only a safe projection, aggregates
   additions and offers an explicit **Show drawings** button. Only that user
   action unhides drawings and activates the target tab; rendering the notice
   does not reveal anything automatically.
7. **Regression/verification:** unit, real D1/H4, geometry and app end-to-end tests
   pass. The requested 80,900 rising-line acceptance assertion is explicitly
   ignored because the real D1 fixture contradicts it; an exhaustive passing
   test proves the disagreement is not a ranking artifact. It is not reported as
   a successful reference match.

### Parameters and boundaries

Default lookback is 400 D1 bars, scaled by timeframe seconds and clamped to
20–1000 bars (H4 therefore caps at 1000). It is additionally bounded by the
requested/fetched history. ATR warm-up uses the observed mean true range, with a
small relative/absolute numerical floor. No BTC-specific constants occur in the
production algorithm.

Zones use 0.35 last-bar ATR touch uncertainty, initial same-role cluster diameter
0.7 ATR, IQR/median swing bounds biased toward the median nearer body edge, and a
1.2 ATR height cap. High/low reactions are clustered separately before overlap
merging to preserve polarity-flip levels. Cap-excluded distant swings do not
inflate counts or strength. Respected closes count non-swing bars that touch a
zone and close on the same outside side as the prior close; they are not a count
of every close in/near a zone. Total touches = distinct swing bars + respected
closes. Zone ranking is proximity-first, score as tie-break, with nearest-major
reservations; scores combine touches, recency, prominence and relative volume.

Line ranking combines touch count, span and normalized residual, with a
20%-window recency half-life and a 0.5 broken-line multiplier. The 0.35 ATR broken
threshold applies to the current close; the at-most-two historical close breaches
are strict, with no tolerance. Projections interpolate timestamps, matching the
rendered geometry; +10 uses the final observed bar interval.

The 16 KiB tool-result cap is retained. Oversized detail is explicitly marked and
reduced while retaining analysis metadata; line touch reduction retains exact
anchors and original touch count. Truncated results deliberately receive no
usable analysis reference: request a smaller history rather than silently draw
from an incomplete result. References expire across turns and after cache
rotation (32 entries).

### Actual Coinbase BTC-USD D1 output

393 bars, 2025-10-01 through 2026-10-28 UTC; final timestamp `1793145600`, close
**80,628.95**, last ATR(14) approximately **1,899.74469**. The following are all
returned analysis zones, not a proposal to draw all 11. Drawn rectangles end at
the last bar and extend right; last touch below is analytical metadata, not the
rectangle's end. Prices rounded to cents here; cached coordinates retain full
floating-point precision. `S+C=T` means swing touches + respected closes = total.

| ID | Role / strength | Low–high | S+C=T | First → last touch (UTC) | Score | Trimmed |
|---|---|---|---|---|---:|---|
| zone-0 | in-play / major | 78,525.63–80,628.95 | 11+13=24 | 2025-11-21 → 2026-10-28 | 30.1873 | no |
| zone-1 | resistance / major | 81,452.63–83,041.42 | 8+8=16 | 2026-05-06 → 2026-10-23 | 23.3208 | yes |
| zone-2 | support / major | 75,882.88–78,162.58 | 7+10=17 | 2026-04-17 → 2026-09-11 | 21.1814 | yes |
| zone-3 | resistance / major | 84,786.19–87,065.88 | 11+11=22 | 2025-12-15 → 2026-10-21 | 29.1847 | yes |
| zone-4 | support / major | 73,635.54–75,246.46 | 6+5=11 | 2026-03-04 → 2026-09-16 | 18.7571 | yes |
| zone-5 | resistance / minor | 87,400.72–88,730.55 | 1+6=7 | 2025-12-07 → 2026-01-28 | 6.6655 | no |
| zone-6 | support / major | 70,232.59–72,512.28 | 7+6=13 | 2026-02-08 → 2026-05-29 | 20.7768 | yes |
| zone-7 | resistance / major | 89,163.61–90,809.27 | 9+8=17 | 2025-12-11 → 2026-01-29 | 23.7134 | yes |
| zone-8 | support / minor | 68,288.62–69,015.73 | 2+9=11 | 2026-02-21 → 2026-08-20 | 9.8724 | yes |
| zone-9 | resistance / minor | 110,224.62–111,554.45 | 1+0=1 | 2025-11-02 → 2025-11-02 | 3.8887 | no |
| zone-10 | resistance / minor | 113,084.92–114,414.75 | 1+1=2 | 2025-10-21 → 2025-10-29 | 5.4336 | no |

| ID / role | Exact swing anchors (UTC date, price) | Touches / style | Current projection | +10 bars | Broken | Closes beyond |
|---|---|---|---:|---:|---|---:|
| line-0 / rising support | 2026-07-06 61,250.00 → 2026-07-24 63,641.00 | 5 / solid | 76,393.00 | 77,721.33 | no | 0 |
| line-1 / rising support | 2026-08-14 62,468.21 → 2026-10-18 80,564.57 | 3 / dashed | 83,348.63 | 86,132.68 | **yes** | 1 |
| line-2 / falling resistance | 2025-10-06 126,296.00 → 2026-10-21 84,856.07 | 5 / solid | 84,092.70 | 83,002.18 | no | 0 |
| line-3 / falling resistance | 2025-10-13 116,077.51 → 2026-10-21 84,856.07 | 5 / solid | 84,270.14 | 83,433.11 | no | 1 |

**Reference differences:** all four requested price bands overlap a returned zone
with the requested role and major classification where specified, but the
computed bounds are broader, not exact replications. In particular, reference
84,450–85,250 maps to 84,786.19–87,065.88; 82,150–82,900 to
81,452.63–83,041.42; 80,000–80,900 to 78,525.63–80,628.95; and
76,400–77,150 to 75,882.88–78,162.58. The test checks overlap/role, not endpoint
equality. The in-play close is at the upper boundary after overlap/cap trimming.

There is **no qualifying rising line within 1% of 80,900**, even when every low
pair is inspected before ranking. The nearest qualifying candidate joins
2026-09-15 **74,887.50** to 2026-10-18 **80,564.57**, has three touches, and
projects to **82,284.894242** (+1,384.89 / +1.712% versus 80,900), already broken.
It is not one of the final two ranked rising lines. The earlier major low is
2026-09-02 **76,219.18**, not a mid-September 76,600 low. October 24/25/26 lows
are **79,107.00 / 79,732.29 / 79,255.00**, not three independent confirmed rising
D1 swing lows. Neither reference rising-line shape is reproduced; thresholds
were not relaxed to fabricate it.

Reproduce the tables without launching the app:

```sh
cargo test --offline --locked -p tr-agent-tools precision_btc_fixture_report_and_reference_comparison -- --nocapture
```

### Added tests and changed files

Added tests cover fractal/prominence strength, scaled/capped/deduplicated zones
and distinct touches, line tolerance/exact anchors/strict close validation,
staleness/slope/minimum-touch rejection, exhaustive BTC reference disagreement,
D1 tables/reference overlaps, the explicit ignored 80,900 assertion, analysis
reference overrides/symbol mismatch/expiry/manual warnings, same-symbol/timeframe
replacement at the object cap (including legacy rays and exact timestamps),
rectangle viewport extension/hit testing, dashed ray labels/serialization,
real D1/H4 version/lookback/result-cap contracts, app cached drawings/replacement/
undo/hidden disclosure, and privacy-safe notice aggregation/user-button action.
Legacy golden calls now explicitly request v1; deterministic-content comparisons
ignore unique per-call analysis IDs; existing schema snapshots were refreshed.

Changed files (all relative to repository root):

- `crates/agent-tools/src/{analytics,args,data,host,lib,registry}.rs`
- `crates/agent-tools/tests/tools.rs`
- `crates/agent-tools/tests/snapshots/{read_only,edit,workflows}.json`
- `crates/agent/src/bridge.rs`
- `crates/chart/src/drawings/{mod,lines,shapes}.rs`
- `app/src/{agent_bridge,agent_pane,agent_session,chart_view,drawing_paint}.rs`
- `app/src/app/agent_ui.rs`
- `docs/analysis/agentic-charts-plan.md`

### Final verification

- `cargo fmt --all`: **passed**.
- `cargo clippy --offline --locked --workspace --all-targets -- -D warnings`:
  **passed**, no warnings.
- `TR_CONGRESS_REFRESH_MIN=0 cargo test --offline --locked --workspace`:
  **passed: 1,910 passed, 0 failed, 9 ignored**, including doctests. Executed
  equivalently using `cargo test --config 'env.TR_CONGRESS_REFRESH_MIN="0"'
  --offline --locked --workspace -- --quiet` because the shell environment-prefix
  form required approval. The nine ignores comprise the explicit A3e reference
  mismatch, seven pre-existing network-dependent tests, and one pre-existing
  manual news-label audit. The ignored A3e assertion is an unmet reference
  expectation, not a passing test.

| Suite group | Passed | Ignored |
|---|---:|---:|
| app | 961 | 1 |
| tr-agent (unit + integration + doctest) | 114 | 0 |
| tr-agent-tools | 116 | 1 |
| tr-alerts | 15 | 0 |
| tr-broker | 47 | 0 |
| tr-chart | 85 | 0 |
| tr-congress | 65 | 0 |
| tr-core | 224 | 0 |
| tr-data | 173 | 7 |
| tr-edgar | 66 | 0 |
| tr-macos | 2 | 0 |
| tr-net | 42 | 0 |
| **Total** | **1,910** | **9** |

## Reference behaviour

CoinMarketCap Agentic Charts pairs a TradingView chart with a chat panel.

**Request.** Each message is one POST to `/chatbot/v3/agentic-charts/run`, with this body:

```
{input, sessionId, runId,
 chartContext: {symbol, resolution, visibleRange}}
```

**Reply.** The response streams back as server-sent events:

- `START`
- `TOOL_START` / `TOOL_FINISH`
- `FRONTEND_CONTEXT_REQUIRED`
- `REPLY`
- `COMPLETE` / `FAIL`

**Where the tools run.** The model loop runs on the server, but chart tools run in the
browser. The server pauses with `FRONTEND_CONTEXT_REQUIRED`. The browser runs the
requested tools against the chart, then resumes the run with `frontendToolResults`.

**Tools.**

- `read_chart`, `read_data`, `read_user_actions`, `find_market_pairs`
- `analyze`: swings, trend, levels, volume_profile and volatility. These are computed
  deterministically, never by the model.
- `create`, `update` and `delete` for drawings, indicators, comparisons and marks. Every
  entity carries an id and `createdBy`.
- `set_view` and `screenshot`

**UX.** The chat shows a tool trace, a streamed reply, interrupt and retry, a message
queue, history, suggested prompts and a credit meter.

terminal-rs reproduces the behaviour, not the implementation. In a native app the
"browser" side is the UI thread, so the pause-and-resume dance becomes a channel
hand-off.

## Decisions

| Topic | Decision |
|---|---|
| Agent backend | Local Codex. Spawn `codex app-server` and speak JSON-RPC as JSON Lines over stdio from Rust. The Codex SDKs are TypeScript/Python only, so we don't use them. Billing goes through the user's ChatGPT login. |
| Codex tool path | `dynamicTools`, registered in `thread/start`. Codex calls them back with the `item/tool/call` server request. This requires `initialize.capabilities.experimentalApi: true`. MCP is the documented fallback. |
| Other backends | mimo (mimocode) over ACP in phase A4. mmx (MiniMax) is deferred: it is a stateless chat CLI billed by API key, with no effort setting. |
| Backend, model and effort pickers | Populated live. For Codex the source is `model/list` (paginated; each entry has `supportedReasoningEfforts` and `defaultReasoningEffort`). Effort is an open string, so it is never hardcoded. `ultra` is hidden because it triggers multi-agent behaviour. `max` shows a cost and latency warning. |
| Tool layer | One registry crate, `crates/agent-tools`, with `schema()` and `call()`. Thin adapters expose it as Codex dynamicTools, as an MCP stdio shim and as direct calls. |
| Read-only MCP server (existing backlog item) | Becomes the data-tool half of the registry: history, indicators, conditions, scans. It is served by a `terminal-rs mcp-stdio` shim that talks to the running app over a 0600 Unix socket. There is no HTTP listener. Crypto tools use C01 instrument identity. |
| TradingView MCP client | Used only as a naming and shape reference. It is excluded from the agent toolset. |
| Conversation persistence | App state, not `LayoutDoc`, because layouts are exportable. Threads are `ephemeral` unless resuming is enabled. |

## Isolation (critical)

A developer's `~/.codex/config.toml` may run with `sandbox_mode = "danger-full-access"`
and `approval_policy = "never"`, with browser and computer-use MCP servers attached.
Inheriting that would hand the chart agent a silent shell and a browser. The sandbox
settings alone do not fix this:

- `approvalPolicy: never` means Codex never asks; commands still run.
- `read-only` still allows reading the whole disk.
- No app-server method reports which tools the model can see.

Isolation is therefore built in three layers. All three are required.

### 1. Private Codex home

- Point `CODEX_HOME` at an app-owned directory.
- Sign in inside it with `account/login/start {type: "chatgpt"}`:
  - handle the `account/login/completed` notification and the cancel path;
  - handle the localhost callback port clashing with the user's own Codex;
  - enable `secret_auth_storage` so the token is stored in the Keychain.
- Private config:
  - `sandbox_mode = "read-only"`
  - `web_search = "disabled"`
  - `forced_login_method = "chatgpt"`
  - no `mcp_servers` and no `notify`
  - launch with `--strict-config`
- Disable every feature the agent doesn't need with `--disable`:

  | Group | Features |
  |---|---|
  | Shell | `shell_tool`, `unified_exec`, `unified_exec_tty` |
  | Images | `view_image`, `image_generation` |
  | Apps and plugins | `apps`, `plugins`, `remote_plugin` |
  | Browser and computer use | `browser_use`, `browser_use_external`, `computer_use`, `in_app_browser` |
  | Multi-agent | `multi_agent`, `multi_agent_v2` |
  | Hooks and skills | `hooks`, `skill_search`, `skill_mcp_dependency_install`, `tool_suggest` |
  | Other | `sleep_tool`, `realtime_conversation`, `worktrees`, `daemon_auto_start`, `code_mode_host` |

- Per-thread settings:
  - `environments: []`
  - `cwd` set to an empty app directory
  - `serviceTier: "default"`
  - `baseInstructions` replacing the coding persona
- Scrub the child's environment. An inherited `OPENAI_API_KEY` would silently move
  billing from the subscription to the API.
- Check that `account/read` reports a ChatGPT account at startup and after every reconnect.

### 2. Runtime tripwire

This is the fail-closed control.

On every `item/started`, the item type must be one of:

- `agentMessage`
- `reasoning`
- `userMessage`
- `dynamicToolCall`
- `contextCompaction`
- `plan`

Any other type triggers these steps:

1. Call `turn/interrupt`.
2. Kill the child.
3. Mark the backend unsafe.
4. Show the user why.

Other examples include `commandExecution`, `fileChange`, `mcpToolCall`, `webSearch`,
`imageView`, `imageGeneration` and `collabAgentToolCall`.

Also:

- **Canary turn.** Run one once per Codex version. It asks the agent to list its
  directory and run `ls ~`. It passes only if no disallowed item appears.
- **Approvals.** Decline every `*requestApproval` request. This is a backstop only.
- **Config check.** `config/read` with `includeLayers` verifies config, not tools.

### 3. OS sandbox

Launch app-server under a macOS `sandbox-exec` profile that allows only:

- reads of the private home, the codex binary and system libraries;
- exec of codex itself;
- outbound network on port 443.

This makes secrets such as `~/.ssh` unreadable even if a tool survives the first two layers.

### Version drift

- Check `codex --version` against a tested range at startup. Codex Desktop and
  `codex update` replace the binary.
- Commit a snapshot of the `--experimental` schema so protocol changes show up as diffs.
- Never enable analytics flags.
- Rotate the logs in the private home.

## Architecture

```
egui UI thread ── App::logic() drains AgentToolQueue ──┐
   │  Agent pane (PaneKind::Agent)                      │ oneshot replies
   ▼                                                     │
crates/agent ── AgentBackend trait ── CodexAppServer (stdio JSONL child)
   │                 └─ AcpBackend (phase A4)           ▲
   ▼                                                     │ item/tool/call
crates/agent-tools ── ToolRegistry ── data tools (worker / MarketData)
                                  └─ live tools (queued to UI thread)
```

### `AgentBackend`

The trait has these methods:

- `detect() -> BackendState`. States: missing, wrong version, not logged in, rate-limited, ready.
- `list_models()`
- `start_thread(opts)`
- `send_turn(model, effort, input, images, context)`
- `interrupt()`
- an event stream: `TextDelta`, `ReasoningDelta`, `ToolStarted`, `ToolFinished`, `Usage`,
  `RateLimits`, `Done`, `Error`

### Codex wire protocol

- JSON Lines with no `jsonrpc` field.
- Handshake:
  1. Send `initialize` with `clientInfo` and `capabilities.experimentalApi: true`.
  2. Send the `initialized` notification.
- Reply to `item/tool/call` using the JSON-RPC request id, not `callId`:

  ```
  {success, contentItems: [inputText | inputImage]}
  ```

- Sandbox spellings differ by method:
  - `thread/start`: `sandbox: "read-only"`
  - `turn/start`: `sandboxPolicy: {type: "readOnly", networkAccess: false}`

### Lifecycle

- Run one long-lived private child per app, never the user's daemon or proxy.
- Kill it by process group on exit.
- Restart it with backoff and fail any pending requests.
- Resume threads with `thread/resume`. Fetch history with `thread/turns/list` and `thread/items/list`.

### Per-turn context

Every `turn/start` carries `additionalContext` with:

- symbol
- timeframe
- visible date range
- pinned pane id

This is the equivalent of CMC's `chartContext`. Other turn controls:

- `outputSchema` (optional) returns a structured "actions taken" summary for the trace.
- `turn/steer` handles interjections while a turn is running.
- `turn/interrupt {threadId, turnId}` must also cancel queued UI tool calls. It does
  not roll back changes already applied.

### UI-thread marshalling

eframe 0.36 does not call `ui()` while the window is hidden. It calls only
`App::logic()`, which terminal-rs does not implement today.

- Live tools are queued and drained in a new `App::logic()`.
- Queue pushes and screenshot completion call `request_repaint`.
- The JSON-RPC reader never blocks on the UI.
- Deadlines are about 10 s for UI tools and about 30 s for data tools that fetch
  unloaded history. A timeout replies `success: false`.
- `screenshot` fails immediately while the window is hidden.

### Budgets

- `read_data` defaults to a summary: statistics, the last ~200 bars and LTTB downsampling.
- Each tool result is capped at about 16 KB, because Codex truncates silently.
- About 8 tool calls per turn. Every round trip uses subscription quota.
- The pane shows the 5-hour and weekly limits (`account/rateLimits/read`) and token
  usage (`thread/tokenUsage/updated`).
- Never use the fast or priority tiers.

## Tool registry

Every live tool takes `chart_id`, which defaults to the pinned pane. The pin must
survive layout changes, because pane ids change when panes are rearranged.

| Tool | Kind | Built on | Rules |
|---|---|---|---|
| `read_chart` | live | `ChartState`, `chart_navigation::date_range` | List all chart panes; entities include ids |
| `read_data` | data | `ChartState` bars, `MarketData::history` | Mark the in-progress last bar; ISO timestamps with zone and session; adjusted vs unadjusted; venue-qualified crypto ids |
| `analyze` | data | `levels::pivots`, ZIGZAG / FRACTALS, `atr`, `chart_analytics::volume_profile`, `anchored_vwap`, a new support/resistance finder | Versioned output, including the bar range it was computed over |
| `indicator_values` | live | the chart's built study outputs | |
| `find_symbol` | data | `MarketData::search` | |
| `set_view` | live | `set_symbol`, `set_timeframe`, `apply_linked_range` | Report any linked panes that also moved |
| `create_drawing`, `update_drawing`, `delete_drawing` | live, mutating | `Drawing::new`, `sanitize` | Explicit `symbol` and `all_symbols = false`; reject anchors in the future or beyond loaded bars; a stale id returns a structured error |
| `add_indicator`, `remove_indicator` | live, mutating | `Spec::new`, `toggle_indicator`, `remove_study` | Validate against the indicator catalog |
| `compare` | live, mutating | `add_comparison`, `set_compare` | |
| `screenshot` | live | `capture_export` and `chart_export::crop_png`, returned as `inputImage` | One-time consent; a per-turn indicator |
| `read_user_actions` | live | a new edit event log | |
| watchlist tools | live, mutating | `create_watchlist`, `add_watchlist_symbols` | Confirmation required |
| alert tools | live, mutating | `price_alerts::Book::add`, `set_rule`, `save` | Confirmation required |

No broker, position or order tools in v1. Tool output is treated as data, because news,
symbol names, imported drawing labels and alert text are untrusted.

## Phases

### A0: Codex spike

- Branch: `codex/agent-spike`
- Time box: 2 days
- Throwaway and headless; nothing merges into the app.

Build a small Rust binary that does the following:

1. Spawns the seatbelt-wrapped private app-server with the isolation config above.
2. Runs `initialize` with `experimentalApi` and checks that `account/read` returns a ChatGPT account.
3. Starts a thread with two stub dynamic tools and `environments: []`:
   - `read_chart`, returning canned JSON;
   - `read_data`, returning canned bars.
4. Sends "describe the trend".
5. Replies to tool calls with text and with an `inputImage` PNG.

Acceptance criteria:

- At least one `item/tool/call` reaches our tool.
- Both reply types are accepted.
- `turn/completed` arrives with an `agentMessage`.
- The canary turn produces zero disallowed item types.
- `turn/interrupt` returns in under 1 s.
- The JSON Lines transcript is saved as a test fixture.
- Token and rate-limit deltas are logged.

Go/no-go: any disallowed item type stops the project until it is fixed.

### A1: tool registry and app prerequisites

- Branch: `codex/agent-tools`

Work:

1. **Drawing identity.** In `crates/chart/src/drawings/mod.rs` (`Drawing`), add
   `id: u64`, `created_by: Author { User, Agent }` and `turn_id: Option<String>`.
   Use serde defaults and assign ids on load.
2. **Undo by id.** Undo agent edits by id or by inverse operation, not by restoring a
   whole snapshot, so user edits made during a turn survive. Make `ClearDrawings` take
   an undo snapshot.
3. **Chart getters.** In `app/src/chart_view.rs` (`ChartState`), add read accessors for
   bars, the viewport and the visible `DateRange`.
4. **Worker requests.** In `app/src/worker.rs`, add agent request and response variants
   that aren't tied to a pane: history, quotes, search, indicator evaluation.
5. **The `logic()` drain.** In `app/src/app.rs`, add `fn logic` with the agent tool-queue
   drain, deadlines and repaint wake-ups.
6. **Registry crate.** Create `crates/agent-tools` with the registry, schemas and the data
   tools. Add the support/resistance level finder.
7. **Edit event log.** Record user drawing and view edits for `read_user_actions`.

Acceptance criteria:

- Old layout fixtures load.
- Tool calls complete with the window hidden (test).
- Tool schemas are covered by snapshot tests.
- The analysis tools have golden tests on fixed bar fixtures.

### A2: Agent pane on Codex (read-only)

Split after A0 (2026-10-23). The A0 spike already delivered the backend crate
(`crates/agent`: `AgentBackend`, `CodexAppServer`, isolation, tripwire, scrubber), and
the live run changed what A2 has to carry (see "Status after A0" below). A2a proves
the agent loop headless before any UI is built on it; A3 depends on A2a, not on the
pane, so A2b and A3 run in parallel.

#### A2a: headless bridge

- Branch: `codex/agent-bridge`

Work:

1. **Bridge.** Connect `tr-agent` to the A1b registry (`tr-agent-tools`, every tool in
   the single `chart` namespace that the config routes directly past Code Mode) and to
   the A1a queue (`Request::Agent`, budgets, chart uids and pin). No UI.
2. **Version and tool self-test.** Refuse any Codex version outside the tested range,
   and before reporting ready, verify that the tools are offered as direct function
   tools (the `code_mode_only` + `direct_only_tool_namespaces` behaviour is
   undocumented and could change on upgrade).
3. **Interrupt and recovery.** Interrupt mid-stream (after the first delta), and
   recover a killed app-server through `thread/resume`.
4. **Headless driver.** An example or test harness that runs the bridge against the
   sample provider, so live checks need no window.

Acceptance criteria:

- Live, read-only preset on sample data: "what's the trend" cites real loaded bars;
  all read tools reachable; text and image budgets enforced.
- Interrupt after streaming starts completes in under 1 s.
- Codex version outside the range, or tools not offered directly: refuses to start
  with a clear reason.
- CI replays the committed A0 transcript through a fake app-server.

#### A2b: agent pane

- Branch: `codex/agent-pane`

Work:

1. **Agent pane.** Add a `PaneKind::Agent` variant in `app.rs`, a `PaneSettings::Agent`
   variant in `layouts.rs`, pane rendering in `app/panels.rs`, and a `NEW AGENT` panel
   in `app/workspace.rs`. The pane provides:
   - backend, model and effort pickers (from `model/list`: per-model efforts, default,
     and "costly" tiers);
   - sign-in: open the URL from the `account/login/start` reply (never from terminal
     output; a stale link gives "State mismatch"), show login progress and the scrubbed
     failure reason;
   - the streamed reply, coalesced to about 30 Hz;
   - markdown rendering (egui_commonmark or a minimal renderer);
   - tool trace rows;
   - interrupt and retry;
   - a message queue;
   - suggested prompts;
   - a chart pin;
   - a rate-limit meter (`account/rateLimits/read` and updates);
   - a privacy disclosure of what leaves the machine;
   - "not investment advice".
2. **Keyboard focus.** The chat input captures keyboard focus so chart hotkeys don't fire.
3. **Context.** `additionalContext`, and screenshots with consent.

Acceptance criteria:

- The pane renders the A0 transcript deterministically through the fake app-server.
- Each onboarding state (not installed, unsupported version, signed out, signing in,
  ready, rate-limited) shows one action.
- Live: first full in-app agent test (sign-in, trend question, interrupt, model switch).

#### A2c: agent pane design polish (impeccable)

- Branch: `codex/agent-pane-polish`
- Requested by the user after in-app acceptance (2026-10-23): the pane works but does
  not look or feel finished.

Work, following the impeccable skill's workflow (Operate mode: precision and
restraint, patterns traders already know):

1. **Context.** The repo has no `PRODUCT.md` or `DESIGN.md`. Run `impeccable init`
   (the user supplies product intent) and `impeccable document` to derive the design
   system from the shipped terminal (tokens, type scale, spacing, colours, density),
   so the pane is polished in the terminal's own visual language, not a generic chat
   style.
2. **Critique.** Capture the pane in each state at the sizes it ships at (narrow
   side pane, half width, full; light/dark if themed): onboarding states, sign-in,
   empty with suggested prompts, streaming, tool rows, edit mode with undo, errors,
   rate limit. Run `impeccable critique` against them and reference agent side panels.
3. **Refine** with `polish`, `layout`, `typeset` and `clarify`: header (model,
   effort, tools mode, pin) hierarchy; message layout (user versus agent, spacing,
   markdown typography, tables, code); tool trace rows (compact, collapsible,
   status); notices (one consistent style instead of raw sentences such as "Queue
   stays paused; no prompt is resent"); input area (multiline, send/stop, queue);
   undo affordances; empty state; copy throughout.
4. **Constraints.** egui only, no new crates; keep every A2b/A3 behaviour, keyboard
   rule, privacy rule and test; no idle repaints; verify by looking at the rendered
   pane (screenshots before and after) rather than by code alone.

Acceptance criteria:

- Before/after screenshots of every state reviewed with the user.
- The user signs off on the look in the real app.

### A3: agent edits the chart

- Branch: `codex/agent-edits`

Work:

- The drawing, indicator, comparison and `set_view` mutation tools.
- Highlight agent-made objects.
- "Undo agent turn".

Acceptance criteria:

- The agent draws a support level and a trend line, and adds an indicator.
- "Undo agent turn" keeps the user's own edits made during the turn.
- A stale id returns a structured error.

### A4: MCP shim and mimo

- Branch: `codex/agent-mcp-acp`

Work:

- A `terminal-rs mcp-stdio` subcommand using `rmcp`, bridging to the app over a 0600
  Unix socket. The read-only toolset is the default.
- An `AcpBackend` for `mimo acp`. `session/new` passes `mcpServers` and `cwd`.
- Lock mimo down with `--pure` and permissions denying bash, edit and webfetch.

Check before building:

- whether `mimo acp` is stdio-only (its help also lists `--port` and `--no-auth`);
- how it exposes model and effort (variant). If it doesn't, use `mimo serve`.

Acceptance criteria:

- Claude Code lists the tools through the shim.
- mimo completes the A2 prompt.
- mimo passes the item tripwire and the canary.

### A5: more tools

A hands-on comparison with CoinMarketCap Agentic Charts (2026-10-23) showed that its
agent cannot create alerts or change a watchlist: it states the rule and stops. A5
therefore leads with confirmed workflow tools, and a small A3c carries the
answer-shape lessons from the same comparison.

**A5a: watchlist and alert tools** (`codex/agent-workflows`)

- Seven workflow tools plus `list_alerts` and `list_watchlists`. They are enabled in
  Chart edits mode; Read only gets the two list tools only.
- Every mutating call becomes a proposal. An inline card offers Approve or Decline.
  Nothing is applied without a click, and proposals expire after 10 minutes.
- Close alerts fire only on a finished bar of the chosen timeframe, never on the
  forming bar.
- Turn undo also reverts approved workflow changes when it is safe, and reports
  anything it had to keep.
- Agent-created alerts carry an "Agent" provenance marker.

**A3c: answer shape** (`codex/agent-answers`)

- Answers open with a verdict, then evidence with the finished bar kept separate from
  the forming bar, then the invalidation level.
- Support and resistance are drawn as zones. Agent labels carry a timeframe prefix
  (W1, H4).
- The agent asks a clarifying question when the request is ambiguous.
- The turn footer shows how long the turn took. Suggested prompts are grouped.

**A3d: zone labels and honest status** (merged with A5a)

- Zones carry host-built labels from the analysis timeframe ("W1 support 79,400–81,900").
  Support and resistance always include the zone price is in and the nearest zone on
  each side, capped in height by ATR.
- Tool headers count a failure as retried only when the same target later succeeds.
- Watchlist proposals list each symbol with its own checkbox. Workflow tools accept
  `find_symbol` results unchanged and refuse ambiguous tickers with hints.

**A5b: scanner and multi-symbol tools.** All read-only, in every mode.

- A5b-scan (`codex/agent-scanner`): `list_scans`, `read_scan_results` (results already
  in an open Scanner pane) and `run_scan` (a saved scan or a strict agent definition
  converted to the core `ScanDefinition`). Watchlist and crypto universes use the
  existing scan paths and limits. US market-wide universes are cache-only: the agent
  evaluates only symbols with fresh bars in the scan store and never fetches listings
  or bars. One agent scan runs at a time, with a 60-second timeout. Tool rows offer
  "Open in Scanner" for the user.
- A5b-multi (`codex/agent-multi`): `get_quotes` (up to 25 symbols) and
  `compare_symbols` (2–8 symbols on aligned finished bars: return, volatility, drawdown,
  ATR %, relative strength, correlation and beta to the first symbol).
- Moving results into a watchlist reuses `add_watchlist_symbols` and its approval card.
- mmx stays deferred. If it is added later, call the MiniMax Messages API directly
  (this needs the reqwest `stream` feature) rather than a CLI per step.

## Status after A5 (2026-10-26)

- A5a, A3c and A3d merged into `personal/main` (`b2f5c23`): watchlist and alert
  proposals with per-symbol approval, outcome headers and decision lines; the symbol
  resolver accepts `find_symbol` results and refuses ambiguous tickers.
- A5b merged into `personal/main` (`20bc8f4`): `list_scans`, `read_scan_results` and
  `run_scan`; US market-wide scans are cache-only, one scan at a time, with a 60 s
  deadline and a `run_scan` executor timeout of deadline + 15 s. Agent concurrency is 2.
- Multi-symbol tools shipped with A5b: `get_quotes` (≤25) and `compare_symbols` (2–8),
  aligned by NY trading date for exchange-listed symbols, UTC date for crypto, and
  the close-containing bucket for intraday. Annualisation uses 252/260/365.
- Follow-ups shipped with A5b: saved-scan name prompt (preset names reserved, Replace
  confirmation); session projection keeps scan results so tool rows and Open in Scanner
  work; press-drag-release placement for two-point drawings.
- Next: A4 (MCP shim and mimo) is the only remaining phase.

## Testing and evaluation

- **Unit tests.** Golden tests for the analysis tools, snapshot tests for the schemas,
  and fixture tests for old layouts.
- **Protocol tests.** A fake app-server, and later a fake ACP agent, replaying recorded
  JSON Lines transcripts in CI.
- **Manual evaluation.** 10-15 prompts per backend with tool-trace assertions. These
  are not run in CI, because each run uses subscription quota.

## Risks and open items

- **Senate data.** senate.gov permanently blocked the user's IP (2026-10-26) after
  repeated fresh-data-dir test launches. Senate data needs a third-party source.
  Every new external source must use shared pacing, a machine-wide request ledger
  and offline-by-default test instances.
- **Experimental APIs.** Codex app-server and dynamicTools are both experimental.
  Mitigations: pin the Codex version range, commit the schema snapshot, run the canary
  per version, and keep MCP as the fallback.
- **Data licences.** Sending vendor bars or news to OpenAI or MiniMax may breach
  redistribution clauses, especially for equities feeds. Audit each data provider's
  terms before release. MiniMax is China-hosted and needs separate disclosure.
- **Account terms.** Driving the user's own Codex login through app-server is the
  documented harness use. Never ship, proxy or share credentials.
- **Quota.** Every tool round trip is a model request. Without the budget and meter,
  the 5-hour window can be exhausted quickly.

## Integration references

| Area | Location |
|---|---|
| Command dispatcher | `app/src/app.rs`, `fn execute` (about line 1381) |
| UI entry point | `app/src/app.rs`, `fn ui` (line 2411) |
| Chart state | `app/src/chart_view.rs`, `ChartState` (line 321) |
| Drawings | `crates/chart/src/drawings/mod.rs`, `Drawing` (line 148) |
| Indicator registry | `crates/core/src/indicators/registry.rs`, `Spec` (line 256) |
| Worker | `app/src/worker.rs`: `Request` (line 77), `Response` (line 336), `Shared::send` (about line 605) |
| Data provider | `crates/data/src/lib.rs`, `MarketData` (line 278) |
| Secrets | `crates/broker/src/secrets.rs`, `SecretStore` |
| Price alerts | `crates/core/src/price_alerts.rs`, `Book` |
| Chart export | `app/src/chart_export.rs`, `crop_png` |

## Integration review (2026-10-22, against `personal/main` at `a057b35`)

**Verified.** Every function named in the tool registry and integration
references exists (`capture_export`, `crop_png`, `apply_linked_range`,
`add_comparison`, `set_compare`, `toggle_indicator`, `remove_study`,
`create_watchlist`, `add_watchlist_symbols`, `date_range`, `levels::pivots`,
`volume_profile`, `anchored_vwap`, `Drawing::sanitize`). `App::logic` is not
implemented and `Drawing` has no `id`, as the plan states. `codex-cli 0.163.0`
and `mimo` are installed. Line numbers in the references table will drift; the
names are authoritative.

**Corrections and additions.**

1. **Hidden-window drain is a prerequisite, not just the tool queue.** Worker
   responses (history, live bars, indicator results) are applied only in
   `TerminalApp::drain`, which is called from `ui()`. While the window is hidden,
   a `set_view` followed by `read_data` would wait for history that never reaches
   the chart and time out. A1 must move `drain()` into `logic()` (check that the
   chart apply paths need no `Ui`; GPU upload already happens at paint time) and
   re-measure hidden-window CPU, since the P2 measurements showed that work while
   hidden is easy to regress.
2. **Sandbox profile details for A0.** Codex applies its own Seatbelt profile to
   commands it runs; nesting `sandbox-exec` fails. With the shell tools disabled
   this should not arise, but A0 must confirm it. The profile must also allow the
   loopback listener for the ChatGPT login callback, Keychain access for
   `secret_auth_storage`, and writes inside the private home (sessions, logs).
3. **Recorded transcripts are fixtures in git.** Scrub account ids, emails,
   tokens and rate-limit identifiers from the A0 transcript before committing it,
   in line with the repository's no-secrets rule.
4. **Repaint cost while streaming.** The P1b profile shows that egui frame
   building dominates foreground CPU. Stream coalescing at about 30 Hz must apply
   only while a turn is streaming, with no timer repaints when idle; measure CPU
   during a streamed reply in A2.
5. **Undo by id replaces today's snapshot undo** (`ChartState::undo_drawings`).
6. **The read-only MCP server** referred to as an existing backlog item was not
   tracked in the TC2000 backlog; it is tracked there now as part of A4.
7. **Login is interactive.** A0 needs the user to sign in once inside the private
   Codex home (browser flow). Credentials never pass through chat or the repo.

**Prerequisites from the existing backlog.**

| Item | Needed before | Why |
|---|---|---|
| F01 display names for crypto keys | A2 | The agent and the user should see the same instrument names (`BTC/USD`, not `COINBASE:BTC-USD`) |
| F02 add-to-named-watchlist from every menu | A5 | Watchlist tools need the same "add to a specific list" path |
| T02 formula parity (remote agent) | Not blocking | Improves trust in `indicator_values` |
| P3–P6 performance | Not blocking | Deferred behind A0–A3; repaint work (note 4) may be pulled in if A2 measurements need it |

**Order.** A0 (go/no-go) → A1 (split into A1a app prerequisites: `logic()`
drain, drawing ids and undo by id, chart getters, edit log; and A1b the
`agent-tools` crate, data tools, level finder and golden tests; these can run as
parallel workers once the tool schema types are agreed) → A2a (headless bridge)
→ A2b (pane) ∥ A3 (edits) → A3b (agent timeframe switching) → A2c (pane design
polish with impeccable) → A5 → A4, with F02 running in parallel before A5.
A5 (watchlist and alert tools inside the app) is placed before A4 (MCP shim and
mimo backend) because the in-app agent is the priority; A4 can move earlier if
external clients are wanted sooner.

## Status after A0 (2026-10-23)

A0 passed the live go/no-go (report: [agent-spike-a0-report.md](agent-spike-a0-report.md))
and is merged into `feature/agentic-charts` with A1a, A1b and F01. Findings that change
later phases:

- **Code Mode.** Every current model is `code_mode_only`. App tools must stay in the one
  `chart` namespace routed by `[features.code_mode] direct_only_tool_namespaces`; Code
  Mode and its host stay disabled, so no model-written code runs. Only two canned tools
  have been proven this way; A2a proves the full registry.
- **Credentials.** The token is an owner-only `auth.json` in the private home, not the
  Keychain (unreachable from the sandbox). Startup refuses a readable or linked file.
- **Sandbox.** DNS needs the mDNSResponder socket and `/var` metadata; networking needs
  metadata of the Codex install directory.
- **Version drift** is now an A2a acceptance criterion (pinned range plus a tool
  self-test), not only a risk.
- **Interrupt** must be measured mid-stream; the A0 figure (0.001 s) landed before
  generation started.
- **Sign-in** must use the URL from the `account/login/start` reply.

