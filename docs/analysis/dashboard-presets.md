# Dashboard presets — design notes

> Implementation spec: [dashboard-implementation-plan.md](dashboard-implementation-plan.md). Phase 1 ships seven presets; BRIEF gains its strip in phase 2; RESEARCH waits for the analysis schema.

The app already models dashboards as data: a `Dashboard { id, name, description, build }` in `app/src/dashboards.rs`, building a `LayoutDoc` from existing `PaneSettings`. Today there is exactly one preset (News desk) and twenty-one pane types:

```
Chart   Feed(congress)  Ticker   Profile  Politicians  Alerts
Watchlist  Scanner  Search  News  Story  Filings  OrderBook
Options  Portfolio  Curve  Wei  Agent  Help  Settings
```

So every dashboard below is buildable with what exists today — no new panes required. That constraint is a feature: the presets should be **views over the same panes**, not new features.

---

## Six rules

1. **One question per dashboard.** If you can't state it in a sentence, it's two dashboards.
2. **One primary pane, at least 55% of the area.** Everything else is context.
3. **A triage pane in the top-left.** The eye lands there first; it must answer "is anything happening?" without a click.
4. **Never more than five panes.** The sixth becomes a tab in an existing pane.
5. **One link colour per dashboard.** Links are workflow, not decoration — panes on the same link follow the same symbol.
6. **The six-second test.** Open it cold: if you can't tell whether today differs from yesterday in six seconds, the layout is wrong.

A corollary: don't build dashboards for weekly tasks. The command bar already handles one-off checks faster than switching layouts.

---

## The set

| id | Name | Answers | Panes | Primary |
|---|---|---|---|---|
| BRIEF | Morning brief | What changed overnight, what do I do at the open? | Wei, Curve, News, Chart | News |
| NAME | Single-name desk | Everything about the one symbol I'm working | Chart, Profile, OrderBook, Options, News | Chart |
| FLOW | Smart money | What are insiders and politicians doing? | Feed, Politicians, Filings | Feed |
| MACRO | Rates & data | What is the curve doing, what did the print mean? | Curve, Chart, News, Ticker | Curve |
| SCAN | Idea scan | What is screening today? | Scanner, Chart, News | Scanner |
| RISK | Risk & P&L | Am I about to get hurt? | Portfolio, Chart, Alerts | Portfolio |
| RESEARCH | Research desk | Read one story and interrogate it | News, Story, Agent | Story |
| WALL | Monitor wall | Show me the whole book at once | 6× Chart | Chart grid |

Build BRIEF, RESEARCH, NAME and FLOW first — they cover the open, the read, the trade and the edge.

---

## 1. Morning brief (BRIEF)

```
┌──────────────┬───────────────────────────┐
│ WEI          │ NEWS    flash on, 12h     │
│ global board │ market scope              │
├──────────────┼───────────────────────────┤
│ CURVE        │ CHART   SPY 1D            │
│ 2s10s        │         volume on         │
└──────────────┴───────────────────────────┘
      0.34              0.66
```

- **Wei** — one board for Asia, Europe and US futures; this is the overnight diff.
- **Curve** — the shape, not the level; it's the fastest read on the day's macro tone.
- **News** — scope All, wire off, flash high on, 12h window. Triage, not reading.
- **Chart** — SPY, 1D, volume on, congress overlay off (noise before the open).
- **Link** Orange across News and Chart, so clicking a headline moves the chart.
- **Headline tape** on.

Why it works: it is a *diff* view. WEI and Curve tell you what moved before you read a word; the news tells you why; the chart confirms.

## 2. Research desk (RESEARCH)

```
┌────────────┬──────────────────┐
│ NEWS       │ STORY            │
│ symbol or  ├──────────────────┤
│ market     │ AGENT            │
└────────────┴──────────────────┘
      0.38           0.62
```

- **News** — the queue you are working through.
- **Story** — full text, with Analyze.
- **Agent** — same link colour, so `Analyze` enqueues into the visible pane rather than the last one opened.

This is the dashboard that pays off the analysis prompt: the Story pane is the input, the Agent pane is the output, and the v2 note (verdict, invalidation, risk lines) is legible at 55 characters without scrolling.

## 3. Single-name desk (NAME)

```
┌────────┬──────────────────┬─────────────┐
│PROFILE │                  │ OPTIONS     │
├────────┤  CHART           │ greeks on   │
│ORDERBK │  1D + volume     ├─────────────┤
│depth 10│                  │ NEWS symbol │
└────────┴──────────────────┴─────────────┘
   0.22         0.48              0.30
```

- **Profile** — what the company is; the thing you forget under time pressure.
- **OrderBook** — depth 10, grouping on for liquid names.
- **Chart** — the primary; volume on, positions overlay on.
- **Options** — strikes ±10, greeks on: this is the fastest read on positioning.
- **News** — symbol scope, so it follows the link.
- All four right-hand panes share one link.

Why it works: it removes the tab-churn of a normal session — you stop navigating and start reading.

## 4. Smart money (FLOW)

```
┌───────────────────────┬──────────────┐
│ FEED  congress        │ POLITICIANS  │
│ largest pane          ├──────────────┤
│                       │ FILINGS      │
└───────────────────────┴──────────────┘
         0.60                0.40
```

- **Feed** — congress disclosures, unfiltered; the status bar already reports "0 new filings, 0 trades", so this dashboard is where that counter earns its keep.
- **Politicians** — the filter/roster view; selecting a name narrows the feed.
- **Filings** — the corporate counterpart, tabbed with a Chart so a disclosure can be checked against price in one click.

Why it works: it is the one dashboard a generic terminal can't copy without the data, and the disclosure feed is useless without a person and a filing cross-reference next to it.

## 5. Rates & data (MACRO)

```
┌──────────────┬───────────────┐
│ CURVE        │ CHART UST10Y  │
├──────────────┼───────────────┤
│ NEWS  macro  │ TICKER  VIX   │
│ CPI FOMC NFP │               │
└──────────────┴───────────────┘
      0.55            0.45
```

- News scope filters to the three keywords that matter (CPI, FOMC, payrolls), so the pane is silent on quiet days — which is itself the signal.
- Open this at 08:30, not all day.

## 6. Idea scan (SCAN)

```
┌──────────────┬───────────────┐
│ SCANNER      │ CHART         │
│ results      ├───────────────┤
│              │ NEWS          │
└──────────────┴───────────────┘
      0.52            0.48
```

- Scanner is the primary and should keep its result count visible; the chart and news follow the selected row.

## 7. Risk & P&L (RISK)

```
┌──────────────┬────────────────┐
│ PORTFOLIO    │ CHART + POSITIONS│
│ open only    ├────────────────┤
│              │ ALERTS         │
└──────────────┴────────────────┘
      0.46            0.54
```

- Portfolio sorted by P&L, open positions only — the closed book is history, not risk.
- The chart carries the position overlay; alerts underneath so a stop is never off-screen.

## 8. Monitor wall (WALL)

```
┌────────┬────────┬────────┐
│ CHART  │ CHART  │ CHART  │
├────────┼────────┼────────┤
│ CHART  │ CHART  │ CHART  │
└────────┴────────┴────────┘
```

- Six charts, one timeframe, no link (or they all follow each other, which defeats the purpose).
- For a second monitor. Headline tape on, since there is no news pane.

---

## Variants, not dashboards

- **Earnings season** — don't build it. It is NAME with a Watchlist of reporters as a fourth pane, or SCAN with an earnings filter.
- **Crypto / FX / commodities** — same layouts, different symbols. That is a link colour or a saved workspace, not eight presets.
- **"Everything" dashboard** — twelve panes, none readable. The failure mode every terminal hits.

## What to avoid

- A preset that duplicates the default 2×2 grid.
- More than one primary visual per dashboard: two charts side by side means neither is being read.
- Presets that only work with live data — every one of these should degrade to a sensible empty state, or the demo mode looks broken.

---

## Implementation notes

The registry is a `const ALL: &[Dashboard]`; adding one is a function plus an entry:

```rust
fn brief(defaults: &PanelDefaults) -> LayoutDoc {
    let mut doc = LayoutDoc::preset(
        "Morning brief",
        Layout::MainRight2,
        vec![
            PaneSettings::Wei,
            PaneSettings::Curve,
            PaneSettings::News(Some(NewsSettings { flash_high: true, ..Default::default() })),
            crate::layouts::chart_settings("SPY US Equity", defaults),
        ],
    );
    doc.dock = Node::split_with(
        Dir::Row,
        vec![
            Node::split_with(Dir::Column, vec![Node::leaf(1), Node::leaf(2)], vec![0.55, 0.45]),
            Node::split_with(Dir::Column, vec![Node::leaf(3), Node::leaf(4)], vec![0.42, 0.58]),
        ],
        vec![0.34, 0.66],
    );
    for pane in &mut doc.panes { pane.link = Some(Link::Orange); }
    doc.focus = Some(3);
    doc
}
```

The preset name in `LayoutDoc::preset` is only a fallback — `doc.dock` is what gets used — so a new layout id is never required to add a dashboard.

Tests to mirror the existing one in `dashboards.rs`: pane order, split shares, every pane on the intended link, focus on the primary, and a JSON round-trip of the built doc.
