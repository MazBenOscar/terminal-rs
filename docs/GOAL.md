# Goal: terminal-rs, a Bloomberg-style trading terminal with congressional trades

Build a fast, keyboard-driven market terminal: TradingView-level charting with a Bloomberg Terminal look (black background, amber/green monospace, dense tiled panels, a command line like `AAPL <GO>`).

## Research (REA)
Use REA on tradingview.com (website mode) to study chart UX: interactions, drawing tools, indicators, layouts, hotkeys, data streaming. Notes go in docs/analysis/. Study behaviour only; never copy proprietary code or assets.

## Stack
Pure Rust, fully native, no web frontend: wgpu for GPU chart rendering plus a fast immediate-mode UI (egui or custom), tokio for data ingest, rusqlite storage. It must be extremely snappy: sub-second cold start, 120 fps pan and zoom over 1M+ bars, and nothing that blocks the UI thread. Targets macOS first, then Linux.

## Features
1. Command-line navigation, tiling multi-panel layout, watchlists, hotkeys.
2. Candles, volume, line and area charts; 1m to 1M timeframes; SMA, EMA, VWAP, RSI, MACD, Bollinger; trendline, horizontal and fib drawings; symbol compare.
3. Market data behind a pluggable provider trait. Start with a free source (Alpaca free tier or Yahoo history), keys in env vars, and sample data when there is no key.
4. Congress trades (key feature): live STOCK Act ingest from House Clerk PTR filings (disclosures-clerk.house.gov), including PDF/XML parsing. Senate refresh is permanently paused pending a third-party source: senate.gov blocked this network and all its hosts are denied before transport. Preserve Senate parsers, fixtures and stored trade display. Normalise politician, party, chamber, ticker, buy/sell, amount range, transaction date and filed date into SQLite.
   - Panels: latest trades feed; politician profile (history, estimated performance); per-ticker list of who in Congress traded it.
   - Chart overlay: B/S markers at transaction dates coloured by party, with a hover card and the disclosure lag.
   - Official-source etiquette (N0b): all House requests use `tr_net::shared()` and its machine-wide ledger, with one in flight and ≥3 s spacing per group; disclosures have a 400/day cap and roster 20/day (420 combined, including redirects/failures). Live discovery requests only the current-year index and ≤50 newest initial PDFs; omitted initial history stays omitted on later runs. Index validators/304 preserve incremental behaviour. Refusals/challenges, invalid PDF/ZIP signatures and transport streaks pause the group for 6/12/24 hours (or longer via Retry-After, up to seven days), shared across instances/data dirs. Settings → Data sources exposes House switches/usage/pauses, a disabled Senate explanation and global pause. Scheduled cadence uses `claim_slot("congress:refresh", interval)` (30 min default); manual refresh still obeys the guard. `TR_DATA_DIR` instances are offline unless `TR_ALLOW_EXTERNAL=1`. `TR_PROVIDER=sample` disables Congress refresh unless the interval is explicitly set; `TR_CONGRESS_REFRESH_MIN=0` disables it.
5. Alerts when a tracked politician files a new trade.

## Quality
Tests for the parsers and indicators, frame-time benchmarks, and a README noting it was built from reverse-engineering research. Make Conventional Commits as you go.

## Progress
- [x] Command line, docking workspace (splits, tabs, drag and drop, pop-outs), saved layouts, link groups, rebindable hotkeys.
- [x] wgpu charts: candles/bars/line/area, 1m–1M, SMA/EMA/VWAP/RSI/MACD/Bollinger, drawings, compare, log scale.
- [x] Indicator library: 151 studies across moving averages, bands, momentum, trend, volatility, volume, statistics and pivots, from one registry driving the command line, `IND` picker, chart menu, help, panes/levels and saved layouts.
- [x] Pluggable data providers (Yahoo, Binance/Coinbase, sample mode); multi-asset (equities, FX, crypto, rates, commodities, indices).
- [x] Congress House-only guarded live ingest (PDF/XML); Senate parsers/fixtures and stored history retained, live Senate permanently paused pending a third-party source. Feed, politician profiles, per-ticker panel, chart markers with hover card, tracking alerts and freshness indicator.
- [x] Native macOS look (unified toolbar, vibrancy sidebar on either side, SF fonts), options chains (OMON) with greeks.
- [x] Theme system: built-in themes (Terminal Dark/Light, Bloomberg, High Contrast, Solarized, Nord, Monokai), live editor in Settings, custom themes as TOML files with TOML/JSON import/export, `THEME` command, ⌘⌥T cycling, optional theme per layout.
- [x] Brokerage accounts through SnapTrade (read-only): setup screen, Keychain credential storage, Connection Portal, `PORT`/`ACCT`/`POS`/`ORD`/`ACTV` panes with live-repriced P&L.
- [x] Tests for parsers, indicators, layouts, prefs, themes and the SnapTrade client; frame-time benchmarks; README.
