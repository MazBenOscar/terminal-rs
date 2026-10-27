# News and Filings panes: target design (N1b, N3)

Status: 2026-10-27, from an impeccable critique of the N1a News pane and the N2 Filings pane (live app, Terminal Dark). Planning target for the N1b and N3 builders; a polish pass follows once both land. Follows DESIGN.md ("The Native Trading Desk"): dense rows, monospace data, colour for meaning only, theme roles and `dp()` only.

## What the critique found

| # | Heuristic | Score | Key issue |
|---|---|---|---|
| 1 | Visibility of system status | 2 | Status lines are good, but empty states merge three causes (not tracked, still importing, filtered out) into one paragraph |
| 2 | Match with the real world | 2 | "4 · FORM 4" says nothing a trader needs (who, bought or sold, how much); "Read saved" is internal jargon; times shown in UTC |
| 3 | User control and freedom | 3 | Pause and filters are reversible; "Pause" does not say what it pauses |
| 4 | Consistency and standards | 1 | The two panes disagree on scope labels ("Symbol (linked)" vs "Symbol"), time formats (relative "1h" vs "10-22 22:32"), chip styles and row height; News does not show its symbol, Filings does |
| 5 | Error prevention | 3 | Unsafe links are rejected; no destructive actions |
| 6 | Recognition rather than recall | 2 | The linked symbol is invisible in News, so NVDA chips under an AAPL chart look wrong |
| 7 | Flexibility and efficiency | 1 | No keyboard navigation; every action needs the mouse in a keyboard-first terminal |
| 8 | Aesthetic and minimalist design | 2 | Chrome takes four rows before the first headline ("90D" wraps alone, search fields truncate); a separator under every row; the Finnhub licence footnote repeats in every pane |
| 9 | Error recovery | 2 | Refusals are explained, but the empty states offer no action ("add to watchlist", "show all forms") |
| 10 | Help and documentation | 3 | Tooltips carry the summary and link |

Strengths to keep: stored history never blanks; the honest source status line ("Finnhub · updated 2m ago", "Paused until …"); amber ticker chips that open the chart.

Priority issues, in order: (1) the inconsistent pane grammar, (2) low row density with heavy chrome, (3) Form 4 rows without content, (4) no keyboard path, (5) empty states without cause or action.

## Shared feed grammar (both panes)

### Header (one line, two at most)

```
[Symbol | Watchlist | Market]  AAPL ●                 Finnhub · 2m ago  ⋯
[Today 7D 30D 90D]  [type chips …]          [/ Search          ]  Filters (2)
```

- Line 1: the scope segmented control, then the effective symbol in amber mono with the link-group colour dot (Watchlist scope shows the list name instead), then, right-aligned, the status in muted 10.5pt and an overflow `⋯` menu. Labels are "Symbol", "Watchlist", "Market" in both panes (News keeps "Market" for All market).
- Line 2: date range segmented control, the pane's type chips, the search field (flexible width, hint "Search", `/` focuses it), and a `Filters (n)` button opening a popover for everything else (sources, include/exclude words, keyword watches, and N3's "Buys and sells only").
- Narrow panes (below about 360pt): line 2 collapses to the date range, a search icon and `Filters (n)`; chips move into the popover. Never wrap a single chip onto its own line.
- Rename "Read saved" to "Reload" and move it into `⋯`. "Pause" becomes "Pause updates"; while paused a "12 new · Show" button replaces it.
- The Finnhub licence note moves to Settings › Data sources and the `⋯` menu ("About this source"); it no longer repeats in every pane.

### Rows

One line per row by default, fixed columns aligned across rows:

```
gutter | time (6ch, right-aligned) | ticker (6ch) | main text (flex, truncate) | trailing meta (right-aligned)
```

- **Gutter (dp 6)**: the unread dot (system accent) or a keyword-watch stripe (2pt, the watch's colour). Nothing else.
- **Time**: `crate::feed_time::feed_time` (local time zone): "now", "12m" under an hour, "14:32" earlier today, "Oct 22" this year, "2025-10-22" older; date-only filings never show a clock. Hover shows the full timestamp with the UTC offset. Monospace, muted.
- **Ticker**: amber mono, plain text (no pill background) in the column; more tickers trail in the meta as "+2". In Symbol scope the linked symbol's own chip is omitted (it is in the header). Clicking a ticker opens it in the linked chart.
- **Main text**: primary text, 12–13pt system font, one line with ellipsis; the full text and summary appear on hover and in the expanded row.
- **Trailing meta**: muted 10.5pt, right-aligned: source, cluster count, tags, values.
- **Spacing and separators**: rows `dp(22)` at regular density; no separator lines; hover uses the `control-hover` wash, the keyboard-selected row the accent wash.
- **Read state**: unread rows keep primary text and the gutter dot; read rows dim the main text to `muted`. Opening, expanding or keyboard-selecting a row for 1 s marks it read.
- **Expanded row** (Space or a click on the row body, not the title link): up to three lines of summary or detail under the row, indented to the main-text column, and the row's actions ("Open", "Chart", "Copy link").

### Keyboard (pane focused, no text field focused)

`↑/↓` or `j/k` move the selection; `Enter` opens the link; `Space` expands; `c` opens the selected row's ticker in the linked chart; `/` focuses search; `Esc` clears search, then selection; `u` toggles "Unread only". The scroll position stays anchored on the selected or top item ID across refreshes.

### Empty and status states

One muted line plus one action, chosen by cause:

| Cause | Copy | Action |
|---|---|---|
| Symbol not tracked (Filings) | "AAPL is not on a watchlist. Filings are tracked for watchlist symbols only." | "Add to watchlist" |
| First import pending | "Importing filings: 3 of 8 companies (one a minute while visible)." | none |
| Filters exclude everything | "No 8-K for AAPL in the last 30 days." | "Show all forms" / "Clear filters" |
| No key (News, company news) | "Company news needs a Finnhub key." | "Open Data sources" |
| Paused or offline | the status line already says it; the list shows stored rows | none |

Warnings use `amber` text only when the user can act; otherwise `muted`.

## News rows (N1b)

```
• 12m  NVDA  Nvidia to invest $3B in Meta share offering as AI partnership …   Reuters +4 · M&A
  09:41 AMD   Did the market read Intel right?                                  Finnhub · Earnings
```

- **Story clusters**: one row per story, the lead headline from the best source; trailing meta "Reuters +4" (four more sources). Expanding lists the member headlines (time, source, headline), indented, muted. A "Stories / Wire" toggle in `⋯` (Stories by default).
- **Tickers**: provider tickers plus extracted ones (cashtags, then bare uppercase words validated against the symbol catalogue; "shouting" headlines skip bare words). First ticker in the column, the rest as "+N" in meta (hover lists them, click opens a small menu).
- **Topic tags**: at most two, from a fixed whole-word lexicon (Earnings, Guidance, M&A, FDA, Macro, Crypto, Analyst, Insider, Legal); muted text, no colour.
- **Keyword watches**: managed in the Filters popover (name, words, one colour from a fixed set of 6 theme-safe hues). Matching rows get the gutter stripe; the matched words render in that colour with an underline. A "Watch hits" chip filters to them.
- **Unread**: a per-item seen flag in the store; the header shows "8 unread" and `u` toggles "Unread only".
- No sentiment labels.

## Filing rows (N3)

```
  Oct 22  AAPL  4     Newstead J. (SVP, GC) · Sold 2,399 @ 339.62 · 10b5-1         $815K
  Oct 01  AAPL  4     Khan S. (COO) · Option exercise 99,878 · Tax withholding 52,854   —
  Oct 30  AAPL  8-K   Results of operations · Financial statements
  Oct 31  AAPL  10-K  Annual report
```

- Columns: time, ticker, **form** (mono, 5ch, muted text, e.g. "8-K", "10-Q", "4", "4/A"), main text, trailing **value** (mono, right-aligned).
- **8-K**: decoded items joined with " · "; amendments show "/A" in the form column.
- **Form 4**: "Owner (Role) · Action shares @ price", owner as "Last F." (hover gives the full name), role from the officer title, abbreviated to fit the column (CEO, CFO, COO, GC, Director, 10% owner).
  - Open-market purchase (code P): "Bought", in `up`; open-market sale (S): "Sold", in `down`. Only P and S get colour. The value column shows shares × price ($815K, $17.2M).
  - Every other code is neutral text with a plain name: A award, M option exercise, F tax withholding, G gift, C conversion, J other. The value column shows "—".
  - Several transactions in one filing are aggregated per code ("Sold 3 lots 120,000 @ avg 341.90"); expanding lists each lot with date, shares, price and shares owned after.
  - A 10b5-1 plan (`aff10b5One`) adds a muted "10b5-1" tag.
  - Reporting-owner filings (the watched company files as an owner) stay hidden by default.
- **Insider summary** (Symbol scope, Insider filter): one muted line above the rows, "Insiders 90D · 3 sells $42.1M · 0 buys", sums coloured by direction. A cluster badge "Cluster buy" (amber) when two or more insiders bought within 30 days.
- **Filter chips**: All · 8-K · 10-K/Q · Insider · 13D/G · Offerings · Proxy; the Filters popover adds "Buys and sells only" (hide A/M/F/G/C/J).
- **Chart markers**: open-market P/S only, small ▲ (up) / ▼ (down) markers in the event lane, hover "Newstead J. sold $815K"; the existing events toggle controls them.

## Out of scope here

Visual polish of spacing, density variants and light themes (the polish pass), desktop notifications (N4), sentiment.
