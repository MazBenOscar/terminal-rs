# Dashboard capability map

> Implementation spec derived from this map: [dashboard-implementation-plan.md](dashboard-implementation-plan.md).

Static read of the tree at the current revision — no code changed. The question: can the proposed dashboard views be built from what already exists, and where are the gaps?

## Method

- `app/src/layouts.rs:119` — the `PaneSettings` enum is the complete set of things a dashboard can contain.
- `app/src/app/panels.rs:105-318` — the render dispatch, one arm per pane kind.
- `app/src/dashboards.rs:9` — `Dashboard { id, name, description, build, headline_tape }`, with `ALL` a const slice.
- `app/src/dock.rs:27` — `Node::Tabs` and `Node::Split`, which is the only layout algebra available.
- `app/src/layout.rs:4` — six layout presets up to `Grid3x2`.

## Pane inventory

Every pane a dashboard can hold, and where it renders:

| Pane | Renderer | Evidence |
|---|---|---|
| Chart | `chart_view.rs` | panels.rs:56, 68 |
| Ticker | `panels.rs` | panels.rs:108 |
| Profile | `panels.rs` | panels.rs:115 |
| Politicians | `congress_view.rs` | panels.rs:118 |
| Alerts | `price_alerts.rs` | panels.rs:121 |
| Wei (World Markets) | `markets_view.rs:100` | panels.rs:169 |
| Curve (UST par) | `markets_view.rs:154` | panels.rs:170 |
| Search | `symbol_search` | panels.rs:171 |
| Filings | `filings_view.rs` | panels.rs:181 |
| Story | `story_view.rs:422` | panels.rs:198 |
| News | `news_view.rs` | panels.rs:202 |
| Options | `options_view.rs` | panels.rs:237 |
| OrderBook | `panels.rs` | panels.rs:244 |
| Scanner | `scanner_view.rs` | panels.rs:249 |
| Portfolio | `portfolio_view.rs:293` | panels.rs:277 |
| Watchlist | `panels.rs` | panels.rs:315 |
| Agent | `agent_pane.rs` | panels.rs:317 |
| Feed (congress) | `congress_view.rs` | panels.rs:105 |
| Help, Settings | `panels.rs` | panels.rs:316 |

Note what is **not** in the enum: no order ticket, no economic calendar, no breadth/participation panel, no quote strip, no news "badge" pane.

## Dashboard × capability matrix

| Dashboard | Panes it needs | Status |
|---|---|---|
| **MACRO** | Curve, Chart, News, Ticker | **Ready** |
| **SCAN** | Scanner, Chart, News | **Ready** |
| **RISK** | Portfolio, Chart+overlay, Alerts | **Ready** |
| **FLOW** | Feed, Politicians, Filings | **Ready** |
| **WALL / BOARD** | 6× Chart, `Grid3x2`, tabs | **Ready** |
| **BRIEF** | Wei, Curve, News, Chart | **Partial** — breadth read and cross-asset strip missing |
| **NAME** | Chart, Profile, OrderBook, Options, News | **Partial** — no dockable order ticket |
| **RESEARCH** | News, Story, Agent | **Partial** — analysis renderer is the v1 schema |

Five of the eight are buildable today without touching a pane. The three partials each need one addition, listed below.

## The gaps

### G1 · Breadth / participation read (BRIEF)

- **Exists:** live quotes in `self.quotes: HashMap<String, Quote>`, a watchlist and scanner universes.
- **Missing:** any aggregation into advancers/decliners or a 0-100 score, and any meter widget — `widgets.rs` has `segmented`, `card`, `row_pills`, `lerp_color` and no stat/meter primitive.
- **Size:** small. One aggregate over a subscribed universe plus a bar draw.

### G2 · Cross-asset quote strip (BRIEF)

- **Exists:** the symbol model handles every member — `SPX Index`, `US10Y Govt`, `CL1 Comdty`, `GC1 Comdty`, `BTCUSD Curncy` (`crates/data/src/lib.rs:2`), and quotes already stream per key.
- **Missing:** any price tape. `headline_tape` (`news_view.rs:2208`) is news only; the status bar (`chrome.rs:889`) is provider/connection state.
- **Size:** small, if the strip lives in the status bar or as a one-row pane.

### G3 · Economic calendar (MACRO)

- **Exists:** nothing. `calendar` matches in the tree are `exchange_calendar` (trading sessions) and SEC date handling.
- **Missing:** an events data source and a pane.
- **Size:** large — new feed, new storage, new render. Recommend leaving MACRO without it for v1.

### G4 · Dockable order ticket (NAME)

- **Exists:** a full ticket — `app/src/app/trading.rs:1` "the order ticket (BUY, SELL, TRADE, ⇧B / ⇧S, Buy/Sell buttons on charts …)". Paper and live, Snaptrade previews, capability cache.
- **Missing:** a `PaneSettings` variant. The ticket is app-level state opened by command, so it can be *overlaid* on a dashboard but not *docked into* its layout.
- **Size:** medium — the ticket's state would have to move from `App` into the pane.

### G5 · Empty-state collapse

- **Exists:** `Node::Tabs`, and `dock.rs:305` already removes empty groups and collapses single-child splits on structural edits.
- **Missing:** any per-pane hiding. A dashboard whose Filings pane has no rows keeps its full allotment.
- **Size:** small — but it changes the dock model, so it should be deliberate.

### G6 · v2 analysis renderer (RESEARCH)

This is the one that matters for the prompt work.

- **Exists:** Analyze enqueues a read-only prompt into a linked Agent pane (`story_view.rs:300`, `:262`), and the Story pane parses the reply (`story_view.rs:61-104`) and renders Sentiment, Confidence, Urgency, Outlook, key points and risks (`:677-697`).
- **The schema it parses** (`story_view.rs:126`): `sentiment`, `confidence`, `urgency` in low/medium/high, `outlook`, `key_points` (max 5), `risks` (max 3, level + risk), `tickers`. Anything else in the object is dropped.
- **Missing:** `verdict`, `invalidation`, `hashtags`, `topics`, `entities`, `sentiment_subject`, and the four-category risk taxonomy. The renderer would reject `urgency: "breaking"` outright (`:103`).
- **Also missing vs the screenshots:** the article badges (BREAKING / BEARISH / MAJOR / Impact / Threat). The only deterministic classifier in the tree is the news priority/event tagger in `crates/agent-tools/src/registry.rs:671`, which assigns priority and event from headline wording and has no sentiment badge.
- **Size:** small to medium — extend the struct, the validator, and the five render calls.

## What this means

- **Build the five ready dashboards first.** MACRO, SCAN, RISK, FLOW and WALL/BOARD are pure `LayoutDoc` construction: a `build` fn per dashboard, a row in `ALL`, and a test mirroring `dashboards.rs:55`.
- **BRIEF and NAME are one small addition each** (G1/G2 and G4). Neither blocks a v1: BRIEF ships fine as Wei/Curve/News/Chart, and NAME ships fine with the ticket opened by `BUY` over the layout.
- **RESEARCH needs the parser before the prompt matters.** Until G6 lands, the v2 note is prose the Story pane ignores — it will render as an agent reply, not as inspector rows. That is the real coupling between this dashboard work and the analysis prompt.
- **MACRO's calendar (G3) is the only large gap** and the only one I'd defer.

## Suggested order

1. WALL/BOARD, FLOW, RISK, SCAN, MACRO — no new code paths.
2. BRIEF (G2 strip, then G1 breadth).
3. NAME (G4 ticket pane).
4. RESEARCH (G6), paired with the v2 prompt so the parser and the prompt version land together.
5. G5 and G3 when they earn it.
