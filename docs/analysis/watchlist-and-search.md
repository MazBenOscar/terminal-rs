# Watchlist sections, drag-and-drop and symbol search

Delivered on `personal/main` at `dc27b49` (2026-10-22), following user QA with
TradingView reference screenshots. This is a delivery record, not REA evidence.

## Sections

Watchlists can hold named, collapsible sections. A list stores its symbols in
display order plus section headers positioned over them
(`app/src/watch_sections.rs`), so quote, scan and column consumers still see a
plain symbol list and older settings files load unchanged; lists without sections
save exactly as before. Symbols added elsewhere join the end; removed symbols leave
their section. Sorting by a column is display-only and stays within each section.

- Header menu: rename, add section below, add symbol to section, remove section
  (its symbols join the section above), collapse or expand all.
- Symbol menu: add to any saved watchlist, add as an extra comparison on the
  focused chart, add a section above, add a symbol below, plus the existing
  trade, chart, Congress and remove items.
- Right-clicking empty list space offers add symbol, add section and
  collapse/expand all. Enter saves inline name entries and Escape cancels.
- Inline entries and drags remember the section or symbol they act on, not its row
  index; if the list changes underneath, they re-resolve it or cancel.

## Drag-and-drop

Rows and section headers drag (`app/src/app/watchlist_drag.rs`): the dragged row
follows the pointer, its slot closes and a gap opens at the target with animated
offsets, an accent line marks the drop, and the list auto-scrolls near its edges.
A header carries its symbols and only lands before another header or at the end,
so sections never split. Escape or releasing outside cancels; drags pause while a
column sort is active. Geometry comes from the rows' real rectangles.

## Details card

The bottom of the watchlist pane shows the selected (or first) symbol: name,
venue and type, last price with `USD` for dollar-priced instruments, change and
status (crypto "24/7 market", otherwise the quote time). It hides for empty
lists and short panes and can be turned off in the list's menu.

## Symbol search dialog

`app/src/symbol_search.rs` serves "Add symbol" (watchlist +, row, section and
empty-space menus) and "Symbol search" (SECF and the command bar's "Search all
symbols…"). It only offers concrete instruments from the built-in catalog, the
provider's search and the Binance and Coinbase spot catalogs, so a bare ticker is
never guessed into the wrong asset class.

- Fixed size; title-row close; search icon and clear button; tooltips; centered
  empty states; keycap footer hints.
- Ranking: exact ticker, ticker prefix, crypto base asset, name word prefix,
  substring; ties prefer USD, USDT, USDC, EUR and BTC quotes. Pair queries such as
  `xrp/usd`, `xrp usd`, `XRPUSDT` and `BINANCE:XRPUSDT`, and full keys such as
  `AAPL US Equity`, resolve to the right instruments.
- One row per market: sources quoting the same pair collapse into one row with
  venue badges and an expandable per-source list. The row adds the source chosen
  in the Sources filter, else the first in `SOURCE_ORDER` (Coinbase, Binance).
- Sources view with search, backed by a small registry
  (centralized/decentralized exchange, stock and futures exchange, built-in) so
  OKX or DEX venues are one entry each.
- Click or Enter adds and keeps the dialog open; Shift adds and closes. Duplicates
  are detected on canonical keys and show a remove action instead.

## Validation

gpt-6.1-sol workers built the features in parallel worktrees from a shared data
model; MiMo adversarial reviews confirmed 13 defects (stale entry and drag indices,
pair and key matching, duplicate keys), all fixed with regression tests. An
impeccable critique of the running dialog drove a polish pass (column alignment,
fixed action column, hierarchy, consistent names, drawn icons, sources tiles).
Final checks: formatting and strict Clippy clean, 954 tests passing, 7 network
tests ignored. Live exchange catalogs were exercised in the running app.

## Limits

- Economy, options and derivatives (perpetuals, futures on crypto venues) are not
  offered; there is no data source for them as symbols yet.
- Some provider short names arrive truncated.
- The details card and chart tab show raw crypto keys (`COINBASE:BTC-USD`); see
  F01 in the [gap backlog](tc2000-gap-backlog.md).
