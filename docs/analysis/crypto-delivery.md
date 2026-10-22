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
Full workspace formatting and strict Clippy passed; 797 tests passed, with five
existing network tests ignored. Commit: `7abce7d`. These checks do not establish
desktop visual QA.

Remaining boundaries: reconnect recovery is C03; derivatives, crypto execution,
and volume-at-price data are separate work. Exchange access can be region-restricted.

## C02 — discovery and scanning

Branch: `codex/crypto-discovery`, stacked on C01. It contains both deliveries;
merge C01 before C02, or merge C02 to include both. Neither branch has been pushed
as part of this task.

Open a Scanner pane (`SCAN`) and choose **Binance spot** or **Coinbase spot** in
Universe. An equity bulk-data account is not needed. Enter optional exact Base
and Quote filters (for example BTC and USDT for Binance, BTC and USD for Coinbase),
set Max products and Page, choose existing technical/formula conditions and run.
Uncheck Matches only to inspect non-matches. Rows open the exact product's chart;
matches can be added to a named watchlist or stepped through with the existing
scan/chart workflow. Save scan and saved layouts retain filters and pagination.

Both public APIs return a complete product response, so pagination is local over
a frozen, alphabetically sorted, deduplicated snapshot. Each page scans at most
200 products; the caption shows supported eligible count, selected count, page
and catalog timestamp. This is not a liquidity ranking. Changing base, quote or
page size resets pagination. A new page/run may see a refreshed catalog.

Catalogs accept supported active spot products only, with exchange-reported tick
and lot increments. Offline, cancel-only and auction products are excluded, as
are unsupported asset identifiers and ambiguous Binance product encodings. This
is explicit partial coverage of the exchange, not every listed contract. No
futures/perpetual or aggregate coin/DEX universe is implied. Delistings disappear
after catalog refresh; earlier failures remain unavailable rather than fabricated
matches. Directory failures remain visible even with unavailable rows hidden.

Catalogs use a per-exchange, single-flight, 15-minute memory cache. A failed refresh
does not return stale success. Bodies are capped at 32 MiB and 30,000 products,
and refresh has a 20-second deadline. Scans share four history slots across panes,
emit partial results, preserve per-product failures and cancel pending requests.
Crypto bar caches expire after 60 seconds around the clock, including weekends;
equity-session cache behavior is unchanged. Scheduled scans reuse the existing
no-overlap and overrun-backoff logic.

Result columns show rolling 24-hour change and volumes in each product's native
base and quote units. Sorting by 24h change keeps missing/stale values last.
Binance supplies both volumes and rolling window bounds. Coinbase supplies base
volume and opening/last prices; percentage is derived from those prices, while
quote volume and exact window boundaries remain unavailable. Never substitute
last price times base volume for traded quote volume. Zero volume is valid;
missing data is shown as unavailable. These statistics are distinct from UTC
daily OHLCV conditions. They are informational columns, not new condition operands.

Metric requests share a four-request semaphore with a 20-second deadline. Typed
observations validate price, finite values, source/product identity and timestamps;
declared windows must be 24 hours ±60 seconds and end near observation time
(−120/+30 seconds, an application validation policy). UI metrics become stale
after 60 seconds. Each run obtains fresh metrics; missing statistics do not
invalidate independently available OHLCV scan results. Region/network restrictions
remain visible errors, with no other-exchange or simulated substitution.

### Verification

Core/data/worker/UI tests cover legacy settings, invalid metadata, sorting,
pagination, source changes, unavailable products, cancellation, weekend freshness,
scheduled scans and narrow/wide headless egui rendering. Final checks passed:
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`
and `cargo test --workspace` (820 passed, seven network tests ignored). The local
WSL checks used `--offline` for Cargo dependency resolution; the two explicitly
selected public-network tests below were also run successfully.

Public network smoke checks succeeded on this Windows machine through WSL:
Binance returned 1,246 supported active products and Coinbase 515 at test time.
Both supplied exact-source BTC quotes, hourly history and rolling statistics.
A separate worker integration test ran each BTC universe through discovery,
history/cache, condition evaluation and metric publication successfully. These
counts describe this session, not worldwide availability. Explicit network tests
remain ignored in the default suite so offline CI is deterministic:

```text
cargo test -p tr-data live_catalog_quote_history_keep_exact_venue -- --ignored --nocapture
cargo test -p terminal-rs live_crypto_universes_evaluate_exact_products_and_publish_metrics -- --ignored --nocapture
```

The new paths have headless UI coverage; a running-app visual review on the target
desktop is still needed. C03 streaming/reconnect recovery, derivatives metrics,
coin capitalization/categories, execution and order-flow data remain separate work.

Reference API contracts:

- [Binance exchange information](https://developers.binance.com/en/docs/catalog/core-trading-spot-trading/api/rest-api/general)
- [Binance symbol filters](https://github.com/binance/binance-spot-api-docs/blob/master/filters.md)
- [Coinbase Exchange products](https://docs.cdp.coinbase.com/api-reference/exchange-api/rest-api/products/get-all-known-trading-pairs)

These are public metadata/data interfaces; discovery does not grant trading permissions.
