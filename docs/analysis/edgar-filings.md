# N2: SEC EDGAR filings

Status: 2026-10-27 — historical N2 implementation snapshot; N2-FIX/N3 supersede some details below. Current code stores parsed 90-day metadata, not whole submissions bodies; cadence is machine-wide, validators remain in the local store, interest changes do not cancel in-flight ticks, and Form 4 parsing plus Load once are implemented. News integration and N4 notifications/agent access remain unshipped. See [implementation notes](news-plan.md#n2-fix-implementation-notes-2026-10-26) and the [README](../../README.md#features) for current behaviour.

## Boundaries

- `tr-edgar` owns parsing, SQLite metadata and a serialized scheduler. Production requests use only `tr_net::shared()?.send("sec", request)`; the shared SEC profile supplies contact identification, cross-host pacing and the daily budget. No history pages or market-wide backfill.
- `company_tickers.json` is conditionally checked at most once per 24 hours. The two latest feeds are **8-K and 4**, per the task's bounded two-request cycle (not the earlier plan's wider feed list).
- Feed entries are filtered by watched CIK before insertion/enrichment. Both feeds hitting the same company cause only one conditional submissions request per ten minutes. Pending enrichment survives restarts.
- Daily sweep slots are spaced by `86400 / watched_CIK_count`, with a one-minute minimum. A tick visits at most one sweep slot; missed slots are never caught up in a burst. Shared guard budgets remain the hard ceiling for very large lists.
- First company import uses only the last 30 days of `filings.recent`. Subsequent imports use 90 days. The returned recent JSON is cached locally so a later 304 can fill the wider window without fetching history pages.
- Cadence timestamps, ETags, Last-Modified values, cached submissions bodies, initialization and sweep markers are in `edgar.sqlite3` meta. Retention is 90 days; feed placeholders never erase submissions metadata.

## Worker and UI

- A tokio timer owns the scheduler; UI frames publish watchlist/visibility interest only when it changes. Closing/hiding the pane cancels an in-progress cycle. Native macOS visibility includes hidden, minimized and occluded windows. Polling does not require focus.
- Existing history is published before a guarded refresh. Unchanged data/status does not request a repaint; relative status age is formatted only when the UI actually renders. Refusals show stored rows and a clear contact/offline/pause/budget/disabled status.
- `CF`, `FILINGS` and symbol-qualified commands open the Filings pane. It follows symbol link groups and persists symbol/scope/form filters with serde defaults, leaving old News layouts unchanged. Scope includes all user watchlists, not only the active list. External links are HTTP(S) only.
- Chart events read the local WAL store only, mapping 8-K, 10-Q and 10-K (including amendments) to `EventKind::Filing`; acceptance time is preferred, with filed time as fallback. Existing `show_events` behavior and hover details apply.
- UI additions use theme roles and density-scaled spacing. No Form 4 XML parsing, News integration or notifications are included.

## Guard extension

The guard's byte-returning `send` API is preserved. `response_metadata(url)` exposes status/ETag/Last-Modified immediately after a serialized request; 304 is successful with an empty body. `check("sec")` preflights identity, pause and budget without reserving an attempt or opening a socket. Response metadata is bounded in memory; durable validators belong to the EDGAR store.

## Verification

- Exact manager-supplied fixtures: ISO-8859-1 Atom with 40 entries, Apple's real recent arrays, and 300-entry company map. Fixtures remain unchanged.
- Parser/mapping/watchlist filtering/accession/URL/item decode/store dedupe and prune tests.
- 127.0.0.1 scheduler fakes: two feed requests/cycle, one enrichment/CIK, sweep spacing/no catch-up, conditional 304s, 30-to-90-day cache reuse and disk restart persistence. Offline/NeedsContact/Paused/Budget/Disabled preflight makes zero requests.
- Guard-local fake verifies 304 headers, SEC identity and attempt accounting. Pane tests cover filters, commands, old/new layouts, safe links and no idle repaint. Chart mapping is tested without a fetch.
- All Cargo checks are run offline. In this shell, prefixing `TR_CONGRESS_REFRESH_MIN=0` requires approval; the equivalent `cargo test --offline --workspace --config 'env.TR_CONGRESS_REFRESH_MIN="0"'` supplies that value to every test without changing process-global test environment or launching the app.
