# TradingView quick tools and price-scale follow-up

Date: 2026-10-21. This REA runtime follow-up addresses the drawing favorites
toolbar, indicator quick access, and Auto/Logarithmic controls raised after the
[installed desktop investigation](tradingview-desktop.md).

## Method and boundary

REA `capture_browser_scenario` launched installed Chrome (`155.0.8059.85` as
reported by the provider) against the public AAPL chart, using temporary empty
profiles, a 1600×1000 viewport, en-US/UTC, reduced motion and blocked service
workers. REA performed the declared clicks, menu selections, waits and reload.
DOM/accessibility snapshots support the state observations below. Network
content and account storage were not requested. Owned browser processes were
terminated after each scenario. There was no login, paid-feature use or trade.

This is **website runtime evidence**, not observation of the installed Electron
window. Desktop adapters and website controls must not be conflated. Independent
scenario profiles also mean persistence can only be claimed where a reload was
explicitly performed within the same scenario. Market prices were moving; these
captures are not deterministic price-output fixtures.

## Results

| Question | Result | Evidence and qualification |
|---|---|---|
| How are quick drawing tools chosen? | Answered | Open Trend tools and select Trendline's Add to favorites button. Its action changes to Remove from favorites and the left toolbar gains a pressed Show Favorite Drawing Tools Toolbar control. D1/D2. |
| Is there a separate quick toolbar? | Answered | After dismissing the menu, the DOM contains a floating toolbar with a Trendline item (`FavoriteToolbarLineToolTrendLine`). The item is a span, not an accessible button; the accessibility tree alone misses it. D2. |
| Can it be hidden without clearing favorites? | Answered | Hide/show changes the toolbar control's pressed state. The Trendline item returns after showing it again. D3. |
| Do favorites survive reload? | Partially answered | Trendline and the pressed toolbar control remain after navigation back to the same chart in the same temporary profile. D3. Cross-profile/account/device synchronization, ordering and drag-position persistence were not tested. |
| Can a favorite be removed? | Answered | Reopening Trend tools and choosing Remove from favorites removes the Trendline floating item; removing the only favorite also removes the favorites-toolbar control from this capture. D4. |
| Does quick selection create/edit the correct drawing? | Partially answered | Clicking the floating Trendline item completed in D5. A subsequent hide action timed out and later actions were cancelled. No completed canvas drawing or endpoint editing was verified. |
| Are indicator favorites exposed? | Partially answered | Indicator result DOM includes Add to favorites controls; searching Relative strength index returns the built-in RSI alongside related/community results. I1/I2. The indicator favorite click timed out; persistence and a one-click favorites dropdown were not established. |
| Can an indicator be added without an account? | Blocked in this scenario | Clicking the built-in RSI result completed, then an account prompt said a free account was required for access. I3. A subsequent Close menu click timed out behind that prompt. This establishes this session's gate, not a universal entitlement rule. No successful RSI attachment or numeric output is claimed. |
| What do Auto and Logarithmic mean here? | Answered at control-state level | The axis menu labels Auto as fitting data to screen. Regular, Percent, Indexed to 100 and Logarithmic are separate scale entries. S1. Auto is a range-fitting choice; Logarithmic is a scale transform, not an automatically generated trading signal. |
| Can Auto and Log be combined? | Answered | Initial menu: Auto checked, Regular checked, Logarithmic unchecked. Selecting Logarithmic keeps Auto checked. Turning Auto off keeps Logarithmic checked; turning Auto back on also preserves Logarithmic. S2. |
| Are quick A/L buttons present? | Partially answered | DOM contains A / Toggle auto scale and L / Toggle log scale controls in the price-axis mode-button area. That area was marked hidden in snapshots. Hover reveal, direct A/L clicks and keyboard equivalence were not verified; the menu actions above were exercised. |
| Does manual dragging disable Auto; are modes saved per pane? | Unresolved | The scenario action vocabulary did not exercise drag/hover/wheel gestures. Reload persistence of scale mode, per-pane ownership, nonpositive-price rules and exact fitting margins remain untested. |

The indicator picker also lists **Auto fib extension, Auto fib retracement,
Auto key levels (Beta), Auto pitchfork and Auto trendlines (Beta)** in I1. These
are automatically calculated study candidates, distinct from Auto scale. Only
their names were observed: algorithms, parameters, repainting, confirmation rules,
availability and plotted output were not recovered. If “algorithmic view” refers
to these studies, they need a separate numerical/behavioral investigation.

## terminal-rs implications and acceptance criteria

Source review at `cfee48c` finds drawing selections and Log scale/Reset view in
`app/src/chart_view.rs`, plus manual-scale state and automatic fitting. The scoped
review/search of `app/src` did not identify a configurable favorites toolbar.
Indicator picking, study editing and templates already exist. These proposals
improve access to existing functionality rather than reopen delivered features:

1. **Drawing favorites strip:** select existing tools from a compact toolbar;
   favorite/unfavorite from the full picker, show/hide without clearing the list,
   show the active tool, and provide accessible names and shortcut tooltips.
   Persist tool IDs/order/visibility and gracefully ignore unavailable IDs.
   Reordering and docking are terminal-rs design choices, not verified TradingView
   behavior in this pass. Validate reload, narrow panes and Esc cancellation.
2. **Explicit Auto and Log controls:** expose independent visible states using the
   existing scale machinery. Preserve Auto when changing transform and preserve
   transform when returning to Auto. Keep vertical fitting distinct from resetting
   the time viewport. Decide/test manual-axis interaction, per-pane persistence,
   replay, comparisons and nonpositive log inputs before implementing.
3. **Indicator quick access:** evaluate favorite studies and saved parameter
   presets using the existing registry/templates. Favorite type, configured study
   instance and multi-study template need distinct meanings. TradingView's account
   and automation limits above cannot serve as observed acceptance behavior.
4. **Automated drawing studies:** inventory current Rust overlap and define
   pivot/confirmation rules and reference outputs before scheduling auto Fib,
   trendline or pattern calculations. A matching product label is not parity.

No application code was changed. Existing holds on account-overlay defaults and
2H/4H equity-session alignment remain in force.

## Evidence ledger

| Ref | Evidence ID | Scope |
|---|---|---|
| B | `ev_b11867cddbcf989ceaf8319ce6c5e8425d8b12ada47345b02823a9acd9e2e61d` | Baseline DOM, accessibility and screenshot |
| D1 | `ev_be9325e1b8ffd24cd108d7868ce91cbc9f2d1c42f55f27b7cec92dc2dedc9636` | Trend tools menu and favorite actions |
| D2 | `ev_802cb5616a5f81b0150814d03a99cd000854f9853794c605104309d73391af59` | Favoriting and settled floating toolbar |
| D3 | `ev_7f0283b2e611aa6cef7e4904619e3319386f7f8be3ee6061b615af4d1c8a3271` | Hide/show and same-profile reload |
| D4 | `ev_da6f504e7b655c4051e85df06402dc47f927d87a7c38da4293f2ec47d2ef8a22` | Remove last drawing favorite |
| D5 | `ev_f13ed45fbdfa257347c6f114ce4d17ca785b499c8ff66c4c43e3bd9dde7e94ba` | Quick-item click; subsequent timeout/cancelled actions |
| S1 | `ev_5269e11475b785fe0427ee9d6452b920b12768b991c943794c9e8cd911591f73` | Settled right-axis context menu |
| S2 | `ev_891d13350369710430e5828be587d06ef45a4155b01b8962c62864b1fd5eb579` | Log on, Auto off/on; menu checkmarks after each change |
| I1 | `ev_b6593f576ddcf62acf7d05e1d44022e6e6e9d87d43922e4bcfdf2f5f7d4661f9` | Indicator picker and automatic-study labels |
| I2 | `ev_16f0ca2f31c8ee0bbefbe18e90ccf0550256cf8c4f00134ebb2590096bc919fc` | RSI search/results and favorite controls in DOM |
| I3 | `ev_fd00f91cb5c66efee6c98da504601cd8b5f55f022942bd7a3a9096f4be8a720a` | RSI click/account gate; subsequent close failure |
| I-failed | `ev_359a2835f0d790cd2522033bb15cced42a3ec6b1f34691ffed86d8112b8e1c65`, `ev_c457dc4ee54c652a649ae6b4ddb5b75a7445e6be5fc41b566ee1c3855a627940` | Indicator favorite click timeouts; later actions cancelled |

S2's complete MCP response exceeded the transport budget; its retained record
was exported and the exact DOM checkmarks inspected locally, without rerunning
the scenario. Early snapshots taken immediately after clicks sometimes preceded
menu rendering; settled snapshots are used for claims. Tool-input validation
errors did not execute scenarios. Failed/cancelled steps provide no positive
behavioral evidence.

The raw evidence bundle is retained outside Git alongside the earlier desktop
evidence, as `toolbar-scale-rea-evidence-final.json` in the scratch directory
documented in [the desktop report](tradingview-desktop.md). No target code,
DOM assets or screenshots are added to the repository.
