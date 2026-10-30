# Dashboard presets — implementation plan

**Status: spec only.** No code in this tree has been changed. This document is written to be handed to a coding agent.

Design source: [dashboard-presets.md](dashboard-presets.md) and [dashboard-capability-map.md](dashboard-capability-map.md).

---

## 1. Scope

Add seven built-in presets to `app/src/dashboards.rs`, alongside the existing News desk. Every one is constructed from panes that already exist — no new pane kinds, no new data feeds.

Phase 1 ships seven presets. Phase 2 upgrades BRIEF. Phase 3 upgrades RESEARCH and is blocked on the analysis schema.

## 2. Decisions locked

1. **The order ticket stays app-level and single.** Do not add a `PaneSettings::Ticket` variant. NAME ships with the floating ticket overlaying the layout (`app/src/app/trading.rs`).
2. **No new pane kinds in phase 1.** Presets are `LayoutDoc` construction only.
3. **No economic calendar.** It is the only large gap and MACRO works without it.
4. **BRIEF ships v1 without the quote strip.** The strip and the breadth meter are phase 2.
5. **RESEARCH waits** for the analysis-schema work, so the parser and the prompt land together.
6. **Format compatibility.** Adding presets must not change the layout file format or break loading existing layouts.

## 3. API contract

Read this before writing code; do not re-derive it.

| Fact | Where |
|---|---|
| `LayoutDoc::preset(name, layout, Vec<PaneSettings>)` assigns pane ids 1..n in vector order | layouts.rs:472 |
| `SavedPane` carries `settings` (flattened), `link`, `title`, `window` | layouts.rs:301 |
| `Link` is Red, Orange, Green, Blue, Purple | layouts.rs:24 |
| `doc.focus: Option<PaneId>`, `PaneId = u64` | layouts.rs:342, dock.rs:12 |
| `Dashboard.headline_tape` toggles the app-level news tape | dashboards.rs:15 |
| `Node::leaf(id)`, `Node::split_with(dir, children, shares)`, `Node::Tabs { panes, active }` | dock.rs:120-134 |
| `Layout::Grid3x2` exists and `preset` builds that dock for you | layout.rs:6, dock.rs |
| `crate::layouts::chart_settings(symbol, defaults)` builds a Chart pane | layouts.rs:496 |
| `NewsSettings { symbol, filters, display, flash_high }` implements Default | layouts.rs:59 |
| `Scope` is Symbol, Watchlist, All — **there is no Market** | news_store.rs:137 |
| `Filters.include` matches **any** of its whitespace-separated whole words; `Filters.search` requires **all** terms | news_store.rs:253, 231 |
| `PaneSettings::Ticker` is congressional trades for a symbol, **not** a quote strip | app.rs:366 |
| `PaneSettings::Watchlist` is the global active watchlist, not scoped to the pane | layouts.rs:160 |
| `PaneSettings::Profile` takes a free-text query, not a market key | app.rs:572 |
| `PaneSettings::Portfolio { view, account, symbol, sort, open_only }` | layouts.rs:193 |
| `PortView` is Summary, Accounts, Positions, Orders, Activity | command.rs:18 |

Verify while implementing (do not guess): that `ScanSettings: Default`, that `FeedSettings: Default`, and what string `Profile` expects for an equity.

## 4. Phase 1 — seven presets

Target registry order in `ALL`:

| id | name | headline_tape | panes |
|---|---|---|---|
| NEWS | News desk *(existing)* | true | 3 |
| BRIEF | Morning brief | true | 4 |
| NAME | Single-name desk | false | 5 |
| FLOW | Smart money | false | 4 |
| MACRO | Rates & data | false | 3 |
| SCAN | Idea scan | false | 3 |
| RISK | Risk & P&L | true | 3 |
| WALL | Monitor wall | true | 6 |

### 4.1 WALL — Monitor wall

Six charts, no links, equal shares. `preset` already produces the 3x2 dock, so do not override `doc.dock`.

```rust
fn wall(defaults: &PanelDefaults) -> LayoutDoc {
    LayoutDoc::preset(
        "Monitor wall",
        Layout::Grid3x2,
        ["SPY US Equity", "QQQ US Equity", "IWM US Equity",
         "TLT US Equity", "CL1 Comdty", "BTCUSD Curncy"]
            .iter()
            .map(|s| crate::layouts::chart_settings(s, defaults))
            .collect(),
    )
}
```

### 4.2 FLOW — Smart money

- Panes: 1 Feed(Some(FeedSettings { tickers_only: true, .. })), 2 Politicians, 3 Filings(default), 4 Chart SPY.
- Dock: Row [0.60, 0.40]; right column [0.5, 0.5]; bottom-right is `Node::Tabs { panes: vec![3, 4], active: 0 }`.
- Links: Orange on 3 and 4 (a filing selects a symbol and the chart follows). 1 and 2 unlinked.
- Focus: 1.

```rust
fn flow(defaults: &PanelDefaults) -> LayoutDoc {
    let mut doc = LayoutDoc::preset(
        "Smart money",
        Layout::TwoCols, // fallback; the dock below is authoritative
        vec![
            PaneSettings::Feed(Some(FeedSettings { tickers_only: true, ..Default::default() })),
            PaneSettings::Politicians,
            PaneSettings::Filings(Default::default()),
            crate::layouts::chart_settings("SPY US Equity", defaults),
        ],
    );
    doc.dock = Node::split_with(
        Dir::Row,
        vec![
            Node::leaf(1),
            Node::split_with(
                Dir::Column,
                vec![Node::leaf(2), Node::Tabs { panes: vec![3, 4], active: 0 }],
                vec![0.5, 0.5],
            ),
        ],
        vec![0.60, 0.40],
    );
    doc.panes[2].link = Some(Link::Orange);
    doc.panes[3].link = Some(Link::Orange);
    doc.focus = Some(1);
    doc
}
```

### 4.3 MACRO — Rates & data

- Panes: 1 Curve, 2 Chart "US10Y Govt", 3 News.
- Dock: Row [0.52, 0.48]; right column [0.58, 0.42].
- News settings: `filters.scope = Scope::All`, `filters.include = "cpi fomc payrolls inflation"` (any-of, so the pane is quiet on quiet days — that is the point), `days = 3`, `flash_high = true`.
- Links: Orange on 2 and 3.
- Focus: 1.
- Do **not** use `PaneSettings::Ticker` here; it is a congress-trades pane.

### 4.4 SCAN — Idea scan

- Panes: 1 Scanner(ScanSettings::default()), 2 Chart "SPY US Equity", 3 News(Symbol scope).
- Dock: Row [0.52, 0.48]; right column [0.55, 0.45].
- Links: Orange on 2 and 3, so the selected row drives the chart and the news.
- Focus: 1.

### 4.5 RISK — Risk & P&L

- Panes: 1 Portfolio { view: Positions, account: None, symbol: None, sort: Pnl, open_only: true }, 2 Chart "SPY US Equity", 3 Alerts.
- Dock: Row [0.46, 0.54]; right column [0.62, 0.38].
- Links: Orange on 1 and 2.
- Focus: 1.

### 4.6 NAME — Single-name desk

- Panes: 1 Profile("AAPL"), 2 OrderBook { symbol: "AAPL US Equity", depth: 10, group: 0.0 }, 3 Chart "AAPL US Equity", 4 Options { symbol: "AAPL US Equity", strikes: 10, greeks: true }, 5 News(Some(NewsSettings { symbol: Some("AAPL US Equity"), filters: Scope::Symbol, .. })).
- Dock: Row [0.22, 0.48, 0.30]; left column [0.5, 0.5] holding 1 and 2; right column [0.55, 0.45] holding 4 and 5.
- Links: Orange on all five.
- Focus: 3.
- The ticket is **not** a pane here; it opens over the layout via `BUY`/`SELL`.

### 4.7 BRIEF — Morning brief (v1)

- Panes: 1 Wei, 2 Curve, 3 News(Scope::All, flash on, 12h), 4 Chart "SPY US Equity".
- Dock: Row [0.34, 0.66]; left column [0.55, 0.45]; right column [0.42, 0.58].
- Links: Orange on 3 and 4.
- Focus: 3 (the triage pane takes the eye first).
- Phase 2 adds the quote strip and breadth meter above this dock.

## 5. Tests

Mirror `dashboards.rs:55`. Add one table-driven test over `ALL` plus targeted assertions for the two load-bearing presets.

```rust
#[test]
fn every_dashboard_builds_a_self_consistent_doc() {
    for d in ALL {
        let doc = (d.build)(&Default::default());
        assert!(!doc.panes.is_empty(), "{} has no panes", d.id);

        let ids: Vec<u64> = doc.panes.iter().map(|p| p.id).collect();
        assert_eq!(ids, (1..=doc.panes.len() as u64).collect::<Vec<_>>(), "{} ids", d.id);

        let placed = doc.dock.panes();
        assert_eq!(placed.len(), doc.panes.len(), "{} leaves a pane undocked", d.id);
        let unique: std::collections::HashSet<_> = placed.iter().copied().collect();
        assert_eq!(unique.len(), placed.len(), "{} docks a pane twice", d.id);

        assert_eq!(LayoutDoc::from_json(&doc.to_json()).unwrap(), doc, "{} round-trip", d.id);

        let mut repaired = doc.clone();
        repaired.repair();
        assert_eq!(repaired.panes.len(), doc.panes.len(), "{} repair dropped panes", d.id);
    }
}
```

Plus, in the style of the existing test:

- BRIEF: pane 3 is News with `Scope::All` and `flash_high`; panes 3 and 4 linked Orange; `focus == Some(3)`.
- NAME: five panes; all Orange; pane 3 is a Chart on "AAPL US Equity"; pane 4 is Options with `greeks`.
- WALL: six chart panes, `doc.dock.panes().len() == 6`, no links.

## 6. Phase 2 — BRIEF quote strip and breadth

### 6.1 Quote strip

- Add `quote_strip: bool` to `Dashboard` (a const struct; not serialized — verify before relying on that) and set it true for BRIEF.
- Render as a single row in the status bar (`chrome.rs::status_bar`, line 889) or a slim strip above it.
- Six keys: VIX Index, US10Y Govt, DXY Index, GC1 Comdty, CL1 Comdty, BTCUSD Curncy. Drop any key the active provider cannot resolve, per key — a missing key must render "—", never blank the strip.
- Reuse the existing quote-subscription path used by watchlists. Do not add a second polling loop.

### 6.2 Breadth meter

- Aggregate the **active watchlist** (not a new universe): advancers, decliners, unchanged from `self.quotes`.
- Render a 0-100 bar plus counts. `widgets.rs` has no meter primitive today, so this is a new small draw helper.
- With no quotes, render an em dash rather than 0/100.

## 7. Phase 3 — RESEARCH and analysis schema v2

Blocked on the prompt work; do not start until the analysis format is settled.

- Extend `story_view::Analysis` (story_view.rs:61) with optional fields: verdict, verdict_asset, horizon, conviction, invalidation, hashtags, topics, entities, not_addressed. Everything optional, so v1 replies still parse.
- Relax the validator: accept `urgency: "breaking"` and a four-category risk list (story_view.rs:103).
- Swap the analyze prompt (story_view.rs:126) for the JSON variant at the end of [the prompt doc](../prompts/ai-article-analysis.md), keeping the `<STORY_DATA>` delimiters and the read-only instruction.
- Renderer order: verdict and invalidation first, then the metric row, then key points, risks, then topics and entities (collapsed by default).
- A malformed reply must fall back to prose exactly as today (story_view.rs:1006).

## 8. Sequence and definition of done

1. Registry entries and the four simple presets: WALL, MACRO, SCAN, FLOW.
2. RISK, NAME, BRIEF.
3. Tests.
4. Phase 2 strip, then breadth.
5. Phase 3 schema.

Done when `cargo test -p terminal-rs dashboards` passes, every preset opens from the dashboard menu, and each one survives open → save → reopen.

## 9. Guardrails

- Do **not** touch `app/src/app/trading.rs`.
- Do **not** add `PaneSettings` variants in phase 1.
- Do **not** change the `Link` or `Scope` enums.
- Keep `FORMAT_VERSION` unchanged; presets do not alter the file format.
- No new dependencies.
- If a preset needs a pane setting that does not exist, stop and report rather than widening the enum.
