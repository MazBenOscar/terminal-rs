# TradingView MCP and an independent terminal-rs interface

Investigated 2026-10-21; Rust baseline `7de343b`. Independent branch:
`codex/mcp-study`. Scope covers both connecting to
TradingView and exposing terminal-rs services to agents. No MCP configuration,
credentials, subscriptions, account content or application code were changed.

## Verified boundary and documented offering

REA captured the official [MCP documentation](https://www.tradingview.com/mcp/docs)
as Evidence `ev_0e0a7394b78c63e5924ad4bd3fcd482bf9d7b208f8e3f75bcbf1d331636cdb64`.
It advertises a beta
Streamable HTTP service at `https://mcp.tradingview.com/mcp`, OAuth 2.1, paid
Essential-or-higher access excluding trials, and approximately 100 calls/minute/user.

Its 35 documented tools span watchlists (8), market data (3), search (1), screener
(5), news (2), fundamentals/forecasts (3), documents (2), calendars (3), alerts (8).
Examples include `get_ohlcv`, `run_screener`, `get_screener_columns` and
`get_technicals_rating`. Documented caps: 5,000 bars, 1,000 scan rows, 50 batch symbols.

Alert creation is limited to simple price conditions; study/multiple conditions
and webhook creation are unsupported. Updates restart asynchronously and cannot
change symbol/condition/timeframe. No order-placement or chart-drawing tool appears
in this catalog; this is not an authenticated `tools/list` result. Even a
read-labelled operation deserves review: `get_active_watchlist` documents a
fallback that can activate/create a list.

A separate unauthenticated HTTP initialize probe returned **401** and a Bearer
protected-resource metadata URL. This confirms an authentication boundary, not
tool execution or account entitlement. The local probe log accompanies the REA
bundle; it is analyst HTTP evidence, not a REA capture. No authenticated tool calls
were made. Remote MCP and experimental browser WebMCP are distinct mechanisms.

The raw supporting files remain outside Git on the Windows research machine:

```text
C:/Users/mbakh/.codex/visualizations/2026/10/20/01a15a59-5636-72c0-9352-b7a20879886e/tradingview-desktop/crypto-mcp/
```

`crypto-mcp-rea-evidence.json` retains the REA capture;
`mcp-unauthenticated-probe.json` records the separate HTTP probe. Neither is
required to implement this bounded design on macOS. The
[crypto study](https://github.com/MazBenOscar/terminal-rs/blob/codex/crypto-study/docs/analysis/tradingview-crypto.md)
lives independently on `codex/crypto-study`; this branch does not include it.

## Path A: optional TradingView client — proposed design

This is our integration proposal, not a claim about recovered implementation.

Begin with an external agent client connected to the official service for a
small, manually verified research workflow. Building a full MCP client into the
terminal UI is unnecessary for that first test. An eventual Rust integration
would use a dedicated client adapter with OAuth discovery, refresh/revocation,
secure token storage, schema discovery, bounded retries and cancellation. Verify
current transport/auth requirements against the
[MCP authorization specification](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization).

For any data returned to terminal-rs, preserve symbol, resolved venue/market kind,
source, interval, as-of timestamp and missing/stale/error states. Keep remote
research data distinguishable from exchange-backed chart feeds. Validate column
discovery and representative results for the desired crypto market before
promising that coin, CEX and DEX screening all map identically through the client.
Do not use guessed private scan endpoints or a third-party service bearing a
similar name as substitutes for the official interface.

Acceptance: authenticated discovery works for the user's entitled account;
quote/history/scan examples preserve units and missing values; token expiry,
revocation, rate limiting and partial responses are handled; remote changes are
explicitly selected operations with observable results. A successful remote
response is not permission to redistribute or persist all data indefinitely;
confirm the intended usage with the provider before selecting a caching policy.

## Path B: local terminal-rs server — proposed design

Source inventory found no MCP server/client crate, dispatcher or protocol
integration in the Cargo workspace. `app/src/command.rs::Function` is a UI command
enum, not an external tool contract. Build a typed service API shared by GUI and
MCP, using existing `tr-core`/`tr-data` calculations rather than parsing UI text.
The official [Rust SDK](https://github.com/modelcontextprotocol/rust-sdk) provides
the `rmcp` implementation with Tokio integration; pin a compatible release and
check its MSRV/dependency requirements during implementation.

Proposed first binary/crate: `terminal-rs-mcp` / `crates/mcp` (not created here).
Use local stdio first: the client owns the subprocess, logs go to stderr, and
stdout carries only protocol messages. This follows the
[MCP transport contract](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports).
Add authenticated loopback HTTP only if an always-running/shared service is needed;
validate Origin and restrict binding, rather than expose an unauthenticated port.

Suggested first tools below are **our proposed names**, not TradingView tool names:

| Proposed operation | Reuse and output contract |
|---|---|
| `search_instruments`, `describe_instrument` | Catalog/provider discovery; concrete product identity, supported intervals and capabilities. Explicit ambiguity rather than guess a venue. |
| `get_quotes`, `get_history`, `get_feed_health` | Provider services, bounded counts, as-of time, source route, units and unavailable/stale reasons. No implicit synthetic fallback. |
| `evaluate_study`, `evaluate_condition` | Existing indicator/PCF/shared condition engine; bounded lookback/complexity, input-source provenance and completed-period alignment. PCF remains a pure supported expression language, not arbitrary code. |
| `run_scan`, `get_scan_result` | Existing scan definitions/universe logic; budget, cancellation, partial/complete state and per-symbol reasons. Do not duplicate scheduled scans. |
| `list_watchlists`, `list_alerts`, `get_layout_summary` | Read-only service snapshots with stable IDs/revisions; coordinate with named-watchlists and alerts-daemon work rather than accessing private GUI fields or racing persistence writes. |

Only later add separate mutation capabilities for watchlists, chart selection and
alerts. Route UI changes through typed app messaging; use stable object IDs,
revision checks and operation IDs so retries cannot duplicate effects. Chart
interaction needs a running-app bridge; a headless provider process alone cannot
change the visible chart. Standalone read-only calculation needs no running GUI.

Live orders, credential retrieval, arbitrary filesystem access and unrestricted
shell/code evaluation are outside the first server scope. Future execution must
preserve the application's preview/confirmation and broker capability checks.
Tool descriptions/annotations are not sufficient enforcement; the dispatcher and
service layer must enforce the selected capabilities. Treat loaded libraries,
layouts, settings and tool-result text as untrusted input with bounded schemas.

Acceptance tests should cover protocol initialization/discovery, input validation,
transport isolation, cancellation, partial failures, malicious persisted fields,
source/units round trips and computation agreement with the GUI's service calls.
Read operations must not create a watchlist, alter selection or write settings.
Replay requests must respect cutoffs; no future bars may leak into a scan result.

## Sequence and handoff

1. Coordinate crypto identity/provenance C01 with the separate crypto branch
   before exposing ambiguous crypto routes as precise agent tools. Existing
   crypto aliases can switch between Binance USDT and Coinbase USD products;
   require resolved venue, native product, market kind, quote/settlement units
   and source timestamps. General read-only tools can progress independently
   for existing unambiguous instruments and calculations.
2. Extract the smallest reusable service boundary; add read-only stdio tools for
   existing validated features. Proposed branch: `codex/mcp-readonly`.
3. Evaluate optional official-service access independently. Proposed branch:
   `codex/tradingview-mcp-client`; it need not block our own server.
4. Add mutations/GUI bridging only after stable IDs and ownership are settled with
   the named-watchlist/daemon branches. Keep execution a separate scope.

These branch names are suggestions. The current research branch contains
documentation only. Recheck latest `personal/main`, merge rather than rebase, keep
new persisted fields serde-defaulted, and follow existing theme/widgets and test
conventions. No full TradingView tool or chart parity milestone is implied.
