# Dashboard plan: validation against the code, and the wave plan

Status: 2026-10-30. This checks [dashboard-implementation-plan.md](dashboard-implementation-plan.md) and
[dashboard-capability-map.md](dashboard-capability-map.md) against `personal/main` (read-only, with file:line
evidence). Where this file and the plan disagree, **this file wins**.

## Verdict

The plan suits terminal-rs: seven presets built from existing panes, a test over `ALL`, and the guardrails.
Most of the §3 API contract is accurate. The corrections below are required. The first five would break or
quietly degrade a preset.

## Corrections

| # | Plan says | Fact | Correction |
|---|---|---|---|
| 1 | NAME pane 1 is `Profile("AAPL")` | `Profile` is a **politician** profile (`POL`/`PROFILE`, congress_view). "AAPL" finds nothing. | Drop it. Use `Ticker("AAPL US Equity")` (congressional trades in AAPL, follows links) or `Filings` scoped to the symbol. A company-profile pane is a new gap, **G7**. |
| 2 | NAME News `filters: Scope::Symbol` | `filters` is a `Filters` struct, so this is a type error | `filters: Filters { scope: Scope::Symbol, ..Default::default() }` |
| 3 | SCAN `ScanSettings::default()` | No `Default` exists (layouts.rs:260) | `ScanSettings::from(ScanDefinition::default())` |
| 4 | BRIEF News "12h"; MACRO `days = 3` | `Filters.days` is in 1..90, and the UI only offers Today/7D/30D/90D. 12h can't be expressed, and 3 shows as "Today". | BRIEF `days: 1` (Today); MACRO `days: 7` |
| 5 | FLOW: Filings and Chart share one Tabs group | The chart hides behind the Filings tab, so following is invisible | Give the Chart its own slot: three slots on the right, or Feed and Politicians as tabs |
| 6 | `headline_tape: false` turns the tape off | It only turns the tape **on** (workspace.rs:1012). The preference persists. | Keep "on only". Presets marked `false` leave the user's choice unchanged. Document this. |
| 7 | RISK `open_only: true` | It only affects the Orders view | Drop it |
| 8 | NAME OrderBook for AAPL | Equity depth falls back to **simulated** sample data. Only crypto books are real. | Drop it from NAME (it fits a Crypto desk instead). NAME becomes Chart, Options, News and congressional trades in AAPL. |
| 9 | FLOW: "a filing selects a symbol and the chart follows" | Filings broadcast only on a ticker click, `C` or the Chart button, not on row selection. The default scope is Watchlist, and there is no "all filings" option. Politicians sends no symbol. | Keep the link, and reword the claim to "ticker click / C". Scope stays Watchlist. |
| 10 | MACRO/BRIEF: Chart and News linked | It works one way only (news → chart). A News pane in Scope::All ignores incoming symbols. | Fine as is. Don't claim chart → news. |
| 11 | SCAN: chart and news follow the selected row | The scanner works unlinked only through `chart_target` | Also link the Scanner Orange so it is deterministic |
| 12 | The status bar shows provider state | It shows hotkey hints and the status message | Fix the wording in the capability map |
| 13 | The quote strip reuses the watchlist poll, with no second loop | The watchlist path is itself a gated 20 s poll. **Wei already fetches all six strip keys.** | The strip reuses the Wei/overview poll, plus a gate in `periodic()` when no Wei pane is visible |

**Checked and correct:**
- Pane ids, `SavedPane`, `Link`, the `Node` API, `Grid3x2` and `repair()`.
- `FeedSettings::tickers_only`.
- `include` matches any of its terms and `search` requires all of them.
- `Ticker` = congressional trades.
- Portfolio fields and `PosSort::Pnl`.
- Every symbol key (SPY, QQQ, IWM, TLT, US10Y Govt, CL1/GC1 Comdty, DXY, VIX, BTCUSD) resolves to live data.

**Phase 3 (RESEARCH):** this is already delivered by **N7d**, the v2 analysis schema, parser and renderer
that is in progress. RESEARCH reduces to a preset of News, Story and Agent, built on N7d.

## Wave plan (starts after N7d merges)

### Wave A: parallel, 2 builders

- **A1, presets (one builder, `codex/dash-a1`).**
  - Builds WALL, FLOW, MACRO, SCAN, RISK, NAME, BRIEF (v1) and RESEARCH, with corrections 1–11 applied.
  - Adds the table-driven `ALL` test and targeted tests, plus an open → save → reopen test per preset.
  - One builder, because every preset lives in `dashboards.rs` and two would conflict.
- **A2, BRIEF strip and breadth (one builder, `codex/dash-a2`).** Works in `chrome.rs`, `widgets.rs` and
  `app.rs`.
  - **Quote strip.** It shows VIX, US10Y, DXY, GC1, CL1 and BTCUSD, and a missing key shows "—". It reuses
    the overview poll, with a gate in `periodic()`.
  - **Breadth meter** over the active watchlist. It shows a dash when there are no quotes, and gets a new
    small meter primitive in `widgets.rs`.
  - **Wiring.** A2 adds `Dashboard.quote_strip` with a default of false. A1 sets it to true for BRIEF when
    the branches are integrated.
  - **Merge order.** Merge A2 first, then A1.
- **Review.** An independent review per builder, then a fix pass and full checks. The user tests in the app,
  and then I merge locally.

### A2 implementation notes

- Chrome order, top to bottom: dock → quote strip → headline tape → status bar.
  Each strip reserves zero height when disabled. View and the palette expose **Quote strip**;
  the preference defaults off, and dashboard chrome flags are on-only. NEWS leaves it unchanged;
  A1 will enable the new `Dashboard.quote_strip` flag for BRIEF during integration.
- One existing 20-second overview gate requests the full Wei universe when Wei is visible,
  otherwise just the six strip keys when enabled. The active-watchlist gate is also enabled
  by the strip so breadth remains refreshed when the sidebar/watchlist panes are hidden.
- Breadth ignores missing, invalid, future-dated and at-least-five-minute-old observations.
  Its percentage is advancers divided by all eligible active-watchlist quotes (including unchanged).
  The bar uses an up-role fill over a neutral track; the remainder is not labelled as decliners.
- Text layouts are cached by quote revision, palette, mono font and density; watchlist changes
  and the earliest breadth expiry also invalidate them. Narrow windows horizontally scroll the
  six quotes while keeping breadth visible. No new feed, dependency or layout format is added.

### Wave B: one decision per item

| Item | Size | Notes |
|---|---|---|
| G7 company-profile pane (NAME) | medium | Finnhub `/stock/profile2` is on the free tier and uses the user's key, through the guard. It shows name, sector, market cap and links. It would replace the politician-profile slot in NAME. **Built (G7): `DES`/`COMPANY` pane, on-demand `/stock/profile2` through the guard with a 7-day profile cache; NAME keeps chart, options, news and this profile in front of congress trades.** |
| G5 empty-pane collapse | small | Changes the dock model, so it needs a deliberate design |
| G4 dockable order ticket | medium | Touches `trading.rs`, which the plan forbids in phase 1. Needs an explicit go-ahead. |
| G3 economic calendar | large | Needs a free event source through the guard. Most free calendars are scraped or premium, so research first. |
| Crypto desk | small | A crypto chart, the real crypto order book, crypto news and a scanner. This is the "Crypto dashboard" the user asked for earlier. |
