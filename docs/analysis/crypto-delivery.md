# Crypto delivery and validation

## C01 — instrument identity and provenance

Branch: `codex/crypto-instruments`, based on `personal/main` at `30b0a3c`.

Crypto requests identify an exchange, native product, base asset, quote asset and
market kind. Enter `COINBASE:BTC-USD` or `BINANCE:BTCUSDT` to select a source
explicitly. Legacy `BTCUSD Curncy` remains readable and resolves to Coinbase's
USD spot product; `BTCUSDT` resolves to Binance's USDT spot product. USD and USDT
are different markets. There is no automatic exchange or quote substitution.
Perpetual identities can be represented, but spot adapters reject them before
network requests. Contract precision is optional and never inferred from price.

Quotes include validated instrument metadata and observation time. Watchlist
hover text and chart status expose the resolved exchange/product. Histories,
studies, scans and alerts use versioned concrete provider-route cache keys so
older ambiguous caches cannot be reused. SVG exports include the resolved route;
the existing OHLCV CSV format remains unchanged and has no provenance columns.

Crypto failures cannot fall through to simulated quotes, books or option chains.
Quote batches deduplicate aliases, isolate failed products, cap concurrency and
request duration, and validate finite prices. A batch handles up to 256 concrete
products. Quote sparklines are currently empty to avoid per-product history
fanout; chart histories remain available. HTTP bodies, pagination and history
sizes are bounded. Book and candle parsers reject invalid observations.

Stock order entry rejects crypto at ticket creation and execution validation.
Account overlays require an explicit matching crypto venue and quote; an
unqualified broker holding cannot safely be matched to an exchange-specific chart.

Validation includes identity/serialization migration tests, cache/source isolation,
broker normalization and execution guards, chart generation handling, and loopback
HTTP tests for partial failures, malformed/oversize responses and alias reuse.
Full workspace formatting, strict Clippy and tests are required before commit.
These checks do not establish live exchange availability or desktop visual QA.

Remaining boundaries: reconnect recovery is C03; derivatives, crypto execution,
and volume-at-price data are separate work. Exchange access can be region-restricted.

## C02 — discovery and scanning (implementation follows C01)

Discovery must use exchange product metadata and retain C01 identities through
the existing condition evaluator and chart/watchlist navigation. Only active
spot products are eligible. Filters must be bounded and persisted compatibly;
result limits and unavailable data must be explicit. Do not rank products using
invented volume or compare quote-volume values in different currencies.

Reference API contracts:

- [Binance exchange information](https://developers.binance.com/en/docs/catalog/core-trading-spot-trading/api/rest-api/general)
- [Binance symbol filters](https://github.com/binance/binance-spot-api-docs/blob/master/filters.md)
- [Coinbase Exchange products](https://docs.cdp.coinbase.com/api-reference/exchange-api/rest-api/products/get-all-known-trading-pairs)

These are public metadata/data interfaces; discovery does not grant trading permissions.
