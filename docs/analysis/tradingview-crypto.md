# TradingView crypto: REA findings and terminal-rs implementation plan

Investigated 2026-10-21. Rust baseline: `7de343b` on `personal/main`.
Research branch: `codex/crypto-mcp-analysis`. Windows packaging remains parked.
This is a bounded product/data investigation, not a numeric parity certification
or a complete audit of every TradingView crypto feature.

## Method and evidence types

REA `capture_browser_scenario` performed public, logged-out website scenarios in
temporary Chrome profiles: 1600×1000, en-US/UTC, blocked service workers, eight-second
initial waits and explicit settling waits after actions. Captures selected DOM,
accessibility and URL; no account credentials, trading actions or private account
data were accessed. Owned browsers were terminated after capture. This follows
the installed [desktop study](tradingview-desktop.md), but these are **website
runtime observations**, not evidence that Electron implements the calculations.

Labels: **Observed** = captured controls/state; **Documented** = official source;
**Source** = current Rust implementation; **Proposed** = our independent design;
**Unknown** = not established. Rendered values and counts are ephemeral and were
not validated as financial reference data. No target code/assets are copied here.

## Feature map

| Workflow | Captured behavior and limits | terminal-rs relationship |
|---|---|---|
| Coin discovery | **Observed C1:** aggregated `CRYPTO` coin rows, rank, market cap, circulating supply, volume/market-cap, categories, social dominance and technical rating. Separate Overview, Performance, Technicals, Valuation, Derivatives, Addresses, Transactions and Sentiment tabs. | Existing crypto overview/watchlist is useful but lacks a coin-metadata universe and these non-OHLCV datasets. Do not treat an aggregate coin as a tradable exchange contract. |
| CEX pair discovery | **Observed C2/C3:** separate exchange-qualified rows; filters for exchange, symbol type, base and quote currency. Symbol-type choices are Spot, Futures and Perpetual. Overview/Performance/Technicals tables. | Add concrete venue/product discovery and a crypto universe to the existing scanner. Do not rebuild its generic indicator/condition editor. |
| Derivatives dashboard | **Observed C4:** coin table has open interest, 24h OI change, OI/volume, funding and total/long/short liquidations. Some cells are unavailable rather than zero. | New typed metric feeds and time-aligned histories are needed. Current equity-option OI is not crypto-perpetual OI. Aggregation formulas and numerical outputs remain unverified. |
| DEX pools | **Observed C5/C6:** blockchain/exchange, liquidity, FDV, transaction/trader counts, unique buyer/seller filters and performance/technical tabs. Blockchain picker includes Solana, Ethereum, Base and others; symbol links retain exchange and pool-like identifiers. | A separate chain/token/pool identity and indexer source are needed. Token ticker alone cannot identify a pool or distinguish chains/wrapped assets. |
| Filter interaction | **Observed C7:** choosing Solana and dismissing with Esc completed, but the final table still contained multiple chains. | Selection/picker inventory is established; successful filter application and cancellation semantics are not. Do not use this probe as proof of filter-result correctness. |
| Legacy pairs screener | **Observed C8:** older `/crypto-screener/` mixes venue/pair forms and has a different table. The new CEX page was reached through the screener switcher. | Keep the old and new surfaces separate; do not infer one uniform API/field schema from their similar names. |
| Charting/alerts | Existing asset-general studies, drawings, comparisons, shared conditions and local alerts already apply to crypto OHLCV. This pass did not successfully add derivative studies to an authenticated chart or create TradingView alerts. | Reuse these delivered Rust features. Funding/OI alerts require typed metric operands, not pretending non-price observations are OHLCV prices. |
| Trading/order flow | Exchange execution, account positions, DOM interaction, tick footprints/CVD and liquidation maps were not exercised in this pass. | Treat execution and detailed order flow as separate capability/data projects, not verified TradingView reconstruction requirements. |

The official [CEX guide](https://www.tradingview.com/support/solutions/43000746110-cex-screener-discover-crypto-trading-pairs-centralized/)
documents saved screens, column sets, technical filters and refresh settings.
The [DEX guide](https://www.tradingview.com/support/solutions/43000744465-dex-screener-a-way-to-decentralize-your-trades-and-investments/)
distinguishes pool analysis from centralized pair analysis. Neither saved-state
round trips nor refresh cadence were tested here. The official
[open-interest guide](https://www.tradingview.com/support/solutions/43000762388-understanding-crypto-open-interest/)
distinguishes aggregated coin OI from exchange-specific contract OI, including
base/quote/contracts unit differences. Its aggregation is not recovered code.

## Existing Rust foundation and exact source anchors

- `crates/core/src/security.rs`: `Security` stores ticker/class; `AssetClass::Crypto`,
  `pair()` and `trades_24_7()` exist. `crates/core/src/catalog.rs` contains 11
  hardcoded crypto listings. Explicitly classified pairs can exceed that catalog;
  this is not exchange-wide product discovery.
- `crates/data/src/crypto.rs`: `CryptoProvider`, `binance_history`,
  `coinbase_history`, `binance_ws`, `coinbase_ws` supply public spot history,
  quotes and live updates. `binance_symbol` maps BTCUSD to BTCUSDT;
  `coinbase_product` maps to BTC-USD and aliases USDT to USD. These are different
  quote assets and potentially different prices, despite convenient legacy names.
- `crates/data/src/markets.rs`: `scan_history` routes crypto to public history;
  `scan_cache_key` uses the combined `binance+coinbase` identity. Ordinary bars
  do not preserve the resolved exchange/product identity. `search` delegates to
  Yahoo; `crates/data/src/universe.rs` lacks an exchange-crypto universe.
- `CryptoProvider::live` currently sends partial bars, not the existing
  `LiveUpdate::Reconnected` signal used for Alpaca. Crypto reconnect backfill is
  missing; Coinbase starts an aggregate from observed ticker messages. Derived
  intervals have no corresponding live route in `Markets::live`.
- `CryptoProvider::order_book` parses Binance/Coinbase REST snapshots. The ladder
  in `app/src/app/panels.rs::order_book` already shows spread/levels/cumulative
  depth; visible books poll every second. It is not a synchronized delta feed,
  and `OrderBook` lacks an explicit age/timestamp contract.
- `crates/core/src/conditions.rs`, `scanner.rs`, `columns.rs`, `price_alerts.rs`
  provide shared OHLCV/PCF scanning, ranking and condition alerts. Scheduled scans,
  repeat/cooldown delivery and chart traversal are delivered foundations.
- `crates/core/src/chart_analytics.rs::volume_profile` distributes OHLCV volume
  across bar ranges; it is an estimate, not retained trade volume at price (T09).
- `crates/broker/src/model.rs::InstrumentKind::Crypto` and `chart_account.rs`
  support portfolio/overlay display. `app/src/app/trading.rs::Ticket::market_key`
  and demo resolution use Stock; quick-trade/portfolio actions restrict
  Equity/Etf. Crypto display support is not a finished spot/perpetual execution
  implementation.

## Proposed implementation sequence

These are design scopes, not branches created or application changes made here.
Start each from the latest `personal/main`; merge, never rebase older branches.

| ID / proposed branch | Concrete deliverable and acceptance criteria | Dependencies / complexity |
|---|---|---|
| C01 `codex/crypto-instruments` | Canonical instrument ID plus venue, native product, base/quote, market kind, settlement/expiry and optional multiplier/tick/lot metadata. Separate aggregate coin IDs from exchange products. Preserve resolved source, timestamps and quote units through histories/cache keys/alerts/layouts. Migrate legacy symbols explicitly; no silent USD/USDT or venue substitution. Test same ticker across venues, spot/perp isolation, old layouts and missing/invalid metadata. | First foundation; medium, cross-cutting. New persisted fields need defaults and bounded validation. Recheck impact on study-history caches and broker keys. |
| C02 `codex/crypto-discovery` | Paginated one-venue product catalog, venue/kind/base/quote filters, bounded crypto scan universe, 24h metrics and result-to-chart navigation. Reuse shared conditions, columns and scheduled scans. Distinguish rolling 24h from UTC daily bars; report unavailable/stale metrics and partial universe coverage. Test sorting, cancel/retry, delistings and source changes. | C01. Medium. Coin capitalization/supply/category sources can be added separately; they are not in exchange OHLCV. |
| C03 `codex/crypto-feed-recovery` | Reconnect event, bounded missed-bar recovery, seeded current candles, deduplication and cancellation. Preserve the chart viewport and avoid double-counting corrected bars. Test out-of-order messages, gaps, current-bar handover and resampled intervals. | C01; reuse Alpaca recovery patterns without duplicating its implementation. Medium. |
| C04 `codex/crypto-derivatives` | Start with one permitted venue: contract catalog, mark/index price, funding interval/history/next payment and OI series. Keep observed, predicted and realized funding distinct. Carry unit/multiplier/as-of/coverage; align only known values to chart periods. Then extend typed condition operands for chart/scanner/alert parity. Test units, late observations, stale data and settlement changes. | C01/C03; high. New external series model/provider contract, not another local formula alone. |
| C05 `codex/crypto-market-overview` | Coin metadata and heatmap/ranking over an explicitly defined universe. Optional dominance/breadth require a published denominator and history convention. Add DEX pool analysis only after selecting a permitted pool/chain data source; identity must include chain/address/pool. | C01/C02 plus external metadata/indexer coverage. Medium for coin overview; high for DEX. No numerical TradingView-equivalence claim. |
| C06 `codex/crypto-orderflow` | Replace snapshots with provider-correct book synchronization, feed age and stale recovery. Add retained, identified trades before trade-based profile/footprint/CVD. Reconcile sequence/gap handling separately per provider; reconnect must rebuild state. Test deletes/duplicates/overflow and quote/base units. | C01/C03 and T09 coordination. High. Trade coverage and aggressor classification are provider-specific; no complete liquidation heatmap can be inferred from OHLCV. |
| C07 separate execution scope | Crypto demo semantics and capability checks first; then venue-supported preview/confirm execution with precision/minimum notional/fees, order flags and position accounting. Perpetual margin/leverage/funding need additional contracts. | Later; high. Do not route crypto tickets through Stock or include live execution in a read-only MCP milestone. |

For provider design, Binance's official
[derivatives market-data catalog](https://developers.binance.com/en/docs/catalog/core-trading-derivatives-trading-usd-s-m-futures/api/rest-api/market-data)
lists funding, mark/index, OI and trade endpoints; their coverage, retention and
availability must be tested before choosing an adapter. Coinbase's
[channel documentation](https://docs.cdp.coinbase.com/exchange/websocket-feed/channels)
provides product metadata, heartbeat and book/trade contracts. A provider API
listing does not prove availability from the user's region/account. An old
Binance liquidation-document URL redirected to a generic landing page during
research, so that link supplies no verified liquidation-stream guarantees.

## MCP connection and shared architecture

See [MCP findings and independent design](tradingview-mcp.md). Both paths are
useful: optional access to the official remote service, and a local terminal-rs
MCP server backed by our own provider/condition services. Neither requires
reconstructing TradingView's private screener endpoints. Keep provider origins
and capability differences explicit; MCP polling is not the chart tick feed.

The recommended first implementation is **C01 instrument identity/provenance**,
then C02 discovery and C03 recovery. A bounded read-only MCP service can be built
alongside this work for existing resolved instruments. Derivatives metrics follow
once their data contracts are established. The existing named-watchlist, session
bar, study-fill, alerts-daemon and drawing-category branches are in progress;
coordinate with their owners rather than duplicate them. T02 numerical formula
parity and T09 trade-level data remain separate open work.

## Evidence ledger and portable handoff

| Ref | Evidence ID | Scope |
|---|---|---|
| C1 | `ev_e1c5593c2975a8e20f779541f81ebd796e7c009b7b5fe6b7dfba3866eb175654` | Coin screener baseline |
| C2 | `ev_cc743dbf40eda3d65566afc0c028d382675249fbfe56a189df6e0226500a16b5` | Switch from coins to new CEX screener |
| C3 | `ev_6955d66ff605194ec237e59a8a25e01ed63fde05b67832f4ddbf0074055c3404` | CEX market-kind picker |
| C4 | `ev_ed51a4b4b5d312a2984e0affd805b066aed6b0771c570c43ea26a2eb97507892` | Derivatives tab/columns |
| C5 | `ev_6b4f227ab1dd02d4b1105be8e321af6ed43838fcebd80a5c15716958f7a394c1` | DEX baseline |
| C6 | `ev_f2d3b777931c324706a8d277c1234ed4d6b6fcdad712eaa8037b2f19f74683e4` | Blockchain choices |
| C7 | `ev_6fb0b6417274f4b1eddf7540e8bf7b69143855901d0a15b5158a3f77457a3718` | Solana selection/Esc; filtered results not established |
| C8 | `ev_0f3d9ed61f723ecc172dbe9c7680942990cf19c6a1a85e717a6cf4cec0e6774e` | Legacy pair screener |
| M1 | `ev_0e0a7394b78c63e5924ad4bd3fcd482bf9d7b208f8e3f75bcbf1d331636cdb64` | Public MCP documentation capture |

Complete selected artifacts were retained for these captures; that means the
requested snapshots completed, not that whole tables, product behavior or data
calculations were exhaustively validated. Paid/account features, saved screens,
automated trading and numerical aggregation remain unverified.

The 10-record, 5,326,740-byte REA bundle is outside Git at:

```text
C:/Users/mbakh/.codex/visualizations/2026/10/20/01a15a59-5636-72c0-9352-b7a20879886e/tradingview-desktop/crypto-mcp/crypto-mcp-rea-evidence.json
```

This report and its companion MCP design are sufficient for the bounded
implementation tasks on macOS. Raw captures are optional supporting evidence;
exact source/aggregation parity still needs attributed numerical fixtures.
