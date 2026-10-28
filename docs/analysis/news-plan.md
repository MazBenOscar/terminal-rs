# News, filings and insider trades: implementation plan

Status: approved 2026-10-26. Sources: Finnhub free tier (user's key) and SEC EDGAR. Yahoo and CoinDesk stay as fallbacks. No paid sources.

## Lessons from senate.gov (non-negotiable rules)

senate.gov blocked the user's IP permanently. The cause was not one aggressive request but many independent instances: every throwaway test launch had a fresh `TR_DATA_DIR`, so each one ran a full first-time backfill, and each kept its own pacing and cooldown state, so none knew the others were fetching.

Every new external source follows these rules:

1. **One machine-wide ledger.** Pacing, budgets and cooldowns live in one SQLite file outside `TR_DATA_DIR` (`~/Library/Application Support/terminal-rs-net/ledger.sqlite3`), shared by every app instance, the alert daemon and tests that opt in. A throwaway data dir cannot reset it.
2. **Offline by default when the data dir is overridden.** Any instance with `TR_DATA_DIR` set (test, demo, A/B) makes no request to an external news/filings host unless `TR_ALLOW_EXTERNAL=1` is set explicitly. Tests never touch the network (fixtures only).
3. **Incremental only.** No market-wide or history backfill. First run fetches a bounded window for symbols on the user's watchlists only. Later runs use cursors (`minId`, `from` dates, `If-Modified-Since`/ETag).
4. **Budgets far below published limits.** Each host has a per-second rate, a concurrency of 1 and a daily cap, all well below the provider's limit.
5. **Stop on the first sign of refusal.** 403/429/503, captcha or "access denied" bodies, or repeated transport failures pause that host (Retry-After or 6 h → 12 h → 24 h, up to 7 days), persisted in the ledger and shown in the UI. No retries inside a pause, no fallback host for the same provider.
6. **Honest identification.** SEC requires a User-Agent with contact details. The contact email is a Settings field the user fills in; until it is set, EDGAR stays off. No browser UA spoofing anywhere.
7. **Visible and switchable.** A Data sources settings tab shows each source's state (on/off, last fetch, requests today vs cap, pause until) with a per-source off switch and a global kill switch.

## Budgets

| Host | Published limit | Our rate | Concurrency | Daily cap | Poll cadence |
|---|---|---|---|---|---|
| finnhub.io (free) | ~60 calls/min | ≤ 20/min | 1 | 3,000 | general every 5 min; forex/crypto/merger every 15 min only with All-market visible; company interval max(30 min, ceil(US symbols × 24 h / 2,000)), max 2,000 company jobs/day |
| sec.gov / data.sec.gov | 10 req/s | ≤ 1 req/s | 1 | 1,500 | latest-filings Atom feed every 10 min; per-company JSON only for new hits |
| query1.finance.yahoo.com | unpublished | unchanged, but via the guard | 1 | 1,000 | fallback only |
| coindesk.com RSS | unpublished | 1 per 5 min | 1 | 300 | crypto only |
| disclosures-clerk.house.gov | unpublished | ≤ 1 per 3 s | 1 | 400 | current-year index every 30 min; ≤50 new PDFs/run |
| clerk.house.gov | unpublished | ≤ 1 per 3 s | 1 | 20 | roster cached in process for 24 h |
| senate.gov and all subdomains | permanently blocked on this network | disabled | 0 | 0 | needs a third-party source |

Hidden window or no visible news/filings pane → no polling (fixes the perf-audit finding for Congress too).

## Architecture

- **`crates/net` (new, `tr_net`)**: generic `Guard` extracted from `crates/congress/src/refresh.rs` (pacer, soft-block checks, Retry-After parsing, transport-failure counting) plus the machine-wide ledger (`requests(host, day, count)`, `cooldowns(host, until, strikes)`), cross-process via SQLite WAL with a busy timeout. One shared reqwest client per host profile. `Guard::request(host, req)` is the only way out to the network for new sources.
- **News store**: `news.sqlite3` in the data dir. `items(id, source, url, url_hash, headline, headline_norm, published, tickers, category, summary)`; de-duplicated by URL hash, then by normalised headline within 24 h (keep the earliest, merge tickers and sources). FTS5 index on headline + summary if the bundled SQLite has it (else LIKE). 90-day retention.
- **Sources** write into the store; panes read from the store. A refresh never blocks the UI, and offline/paused shows the stored history with a clear status line.
- **Secrets**: Finnhub key in the existing secret store (`tr_broker::secrets`, new account `"finnhub"`), entered in Settings with Save & Test (one cheap call), masked display, never logged.

## Phases

### N0: fetch guard and Data sources settings (`codex/net-guard`)
- `tr_net` crate: guard, ledger, rules 1, 2, 4, 5, 7.
- Settings → Data sources tab: Finnhub key (Save & Test / Remove), SEC contact email, per-source switches, global kill switch, live status table.
- Route Yahoo news and CoinDesk RSS through the guard.
- Tests: ledger shared across two guard instances with different data dirs; `TR_DATA_DIR` without `TR_ALLOW_EXTERNAL` refuses; budget exhaustion; pause persistence and expiry; soft-block detection fixtures.

### N0b: Congress refresh safety — done (2026-10-27)

- **Senate is structurally off.** `HttpSource` has no Senate search, agreement, roster or PTR request implementation. All live refresh plans omit Senate, including explicit historical plans. `tr_net` permanently returns `Refused::Disabled` for `senate.gov` and every subdomain, including redirects, before transport and regardless of switches or future policies. Senate parsers, fixture ingestion and stored history remain available. The pane/status says: “Senate: paused — needs a third-party source (senate.gov blocked this network)”.
- **House uses the machine-wide guard exclusively.** `house-disclosures` (disclosures-clerk.house.gov) has ≥3 s spacing, concurrency 1 and 400/day; `house-roster` (clerk.house.gov) has ≥3 s spacing, concurrency 1 and 20/day. Honest `terminal-rs/<version> (personal research; <group>)` identities, refusal markers/statuses, transport streaks and escalating 6/12/24-hour cooldowns are shared across data directories/processes. Invalid PDF/ZIP signatures also pause the disclosure group in the ledger. The old Congress pacer is removed; live refresh ignores the obsolete per-data-dir cooldown table.
- **Bounded, incremental live discovery.** Only the current-year index is requested. At most 50 newest initial, unseen PTR PDFs are eligible; omitted initial IDs are persisted before downloading, so subsequent runs/restarts do not drain the old index. Stored House/Senate history is not removed. Later refreshes fetch newly discovered filings (plus existing bounded parse-error retries), at most 50/run. Reused sources retain parsed index entries and send ETag/Last-Modified validators; 304 reuses those entries. Successful roster bodies are cached for a day within the shared guard.
- **Cadence and switches.** Scheduled runs claim `congress:refresh` in the shared ledger for the configured interval. Default live cadence remains 30 minutes; sample defaults to off and explicit zero still disables refresh. Manual REFRESH bypasses cadence only, not budgets/spacing/cooldowns/offline. Settings → Data sources has House disclosures and House roster switches/usage/pause rows and a permanently disabled Senate explanation. Global pause covers both House groups. `TR_DATA_DIR` instances are offline unless `TR_ALLOW_EXTERNAL=1` exactly; re-enabling the production cadence needs no environment override.
- **Worst-case traffic:** 400 disclosure requests + 20 roster requests = **420 House HTTP attempts per UTC day machine-wide**, including redirects and failed sends. Fresh-directory or manual-refresh churn cannot raise these ceilings; Senate remains **0**. A first live run has at most one index plus 50 PDFs and one roster before redirects, paced by the guard rather than sent in a burst.
- **Regression coverage:** localhost-only injected Guards and repository-local temporary ledgers; no Senate request construction in live plans/direct entry points, permanent host/redirect deny-list, House index/PDF/roster guard usage and identity, index validators/304, shared pacing/slot/cooldown with expiry, refusal persistence across fresh stores/sources, offline zero sockets, 50-newest first import without later history drain, stored Senate trade visibility, and Data sources/status labels and switches.

Verification: `cargo fmt --all` and `cargo clippy --offline --locked --workspace --all-targets -- -D warnings` passed. The offline locked workspace tests passed with `TR_CONGRESS_REFRESH_MIN=0` forced through Cargo's `--config env.TR_CONGRESS_REFRESH_MIN.value=\"0\"` / `force=true` settings: **1,827 passed, 8 ignored**. All Congress HTTP tests use only `127.0.0.1` fakes; temporary ledgers stay inside this repository. No app/live-host run or release build was performed.

### N1a: news store and Finnhub (`codex/news-feed`)
- Finnhub company news (watchlist symbols, incremental `from`) and market news (general, forex, crypto, merger; `minId` cursor).
- Store with stable IDs: SHA-1 of the normalised link (fragment stripped, trailing slash trimmed, http(s) only), falling back to source + headline. Exact duplicates are dropped on insert.
- Never blank on refresh: keep the stored list until new items arrive; a failed or empty fetch never overwrites stored items.
- Replace the hand-written RSS parser with `feed-rs` (RSS and Atom; EDGAR's feed is Atom). Fix `filter_relevant`, which returns every item when nothing matches.
- Yahoo/CoinDesk as fallbacks when Finnhub is off. Default scope with no linked symbol: general market news.
- News pane: scope selector (symbol / watchlist / all), source filter, include/exclude keywords, search over history, date range, pause, ticker chips that open a chart. Layout saves scope and filters.
- Tests: Finnhub fixtures, cursor handling, ID normalisation, never-blank rules, filters, pane scope.

#### N1a review fixes (2026-10-26)
- Store the requested market category, not Finnhub's topic label. Resolve related ticker chips only through the local symbol catalogue; skip individual malformed items and nonpositive timestamps.
- Plan at most 2,576 Finnhub jobs/day with All-market visible (288 general + 288 other categories + 2,000 company). Twenty US symbols plan 1,536/day; 150 plan 2,576/day. Without All-market, these are 1,248 and 2,288/day. Visible linked symbols join the scheduler with priority; non-US/crypto symbol panes retain the guarded Yahoo/CoinDesk path even with a Finnhub key.
- Fallback watchlists select at most one symbol/minute and one attempt/symbol/hour, sharing one round across visible panes. Completion, not round start, sets the next-round delay. An error consumes only that symbol's slot. Symbol/market panes retain a five-minute cadence, with shared requests cached by the guard. All fallback selections additionally share a rolling 60/hour ceiling; Yahoo's machine-wide 1,000/day cap still applies. Watchlist-only bounds: 480 selections/day for 20 symbols; at most 1,440 for 150 (at most 1,000 Yahoo network requests after the guard).
- Preserve a bad database as `news.sqlite3.corrupt-<unix>` (and preserve its journals), reopen fresh, otherwise use an in-memory store and show “Saved headlines unavailable”.
- Today starts at local midnight; FTS and LIKE use matching per-term prefixes; SQL applies filters before LIMIT/OFFSET. Retain existing headlines but explain empty selections, keep pause counts view-local, and show relative update times. Suppress unchanged item replies; status transitions are separate and are not a heartbeat.
- Window visibility/minimization gates polling, never focus. Load the Finnhub secret at startup and on Settings saves/removals only. Spacing refusal is `Busy`, retried after Finnhub's three-second spacing rather than an hour-long budget backoff.

### N1b: story intelligence and pane polish (`codex/news-stories`)
Ideas taken from FinceptTerminal's news module (ideas only; no code is copied):
- Story clusters: Jaccard similarity of headline tokens (0.25, relaxed to 0.20 within a category) inside 24 h; one row per story with "N sources", lead item from the best source, and a rising/stable/falling badge. A wire/stories toggle.
- Ticker extraction from headline text (cashtags and bare uppercase words), validated against our own symbol catalogue; "shouting" headlines skip bare words.
- Keyword topic tags (earnings, M&A, FDA, macro, crypto) with whole-word matching and simple inflections (never substring: "ban" must not match "bank").
- Keyword watches: user keyword sets with a colour stripe on matching rows.
- Unread markers, keyboard navigation (↑/↓, Enter, /), scroll anchored on the item ID across refreshes, relative times.
- Not adopted: browser UA spoofing, lexicon sentiment labels (too noisy to show a professional user until validated against stored history), Python NLP, geolocation and threat labels, prediction markets.

### N2: SEC filings (`codex/edgar-filings`)
- Ticker ↔ CIK map from `company_tickers.json` (once a day, conditional GET).
- Latest-filings Atom feed (8-K, 10-Q, 10-K, S-1, 13D/G, 4) every 10 min; keep only CIKs on the user's watchlists; fetch `submissions/CIK##########.json` only for those, conditionally.
- Filings pane (CF-style): per symbol or watchlist, form filter, 8-K item codes decoded ("2.02 Results of Operations"), opens the filing in the browser. 8-Ks also appear in the News pane as a source.
- Chart: filing markers via the existing generic `ChartEvent` path (new `EventKind::Filing`).

#### N2-FIX implementation notes (2026-10-26)

- Cadence is claimed atomically in the machine-wide `tr_net` ledger: `sec:feed:8-K` and `sec:feed:4` (600 s), `sec:map` (24 h), `sec:sweep:<cik>` (at least 24 h), and `sec:sub:<cik>` (600 s). Generic slot deferrals and durable counters keep failure escalation and pending limits from resetting in a fresh data directory.
- Failed CIKs consume their round-robin turn. Retry backoff is 1 h → 6 h → 24 h; three consecutive failed daily turns suspend that CIK for seven days, with an explicit pane status. Pending accessions remain pending until submissions contain them, with at most three pending requests per sweep cycle across instances.
- Newly added CIKs bypass stale local sweep scheduling, but not pacing. First imports are separated by at least 60 s; sweep requests also share a machine-wide 144 s turn, limiting sweeps to 600 requests/day. Above 600 companies the refresh period is `ceil(companies / 600)` days, displayed in the pane.
- The 1,500/day SEC cap remains machine-wide. All submissions requests stop at 1,200 requests today (80%), atomically in the guard's reservation transaction, reserving room for feeds. Feed requests keep the full cap. Redirect hops count individually and obey both ceilings.
- `send_with_metadata` returns the body and its own validators together, including coalesced and redirected responses. Validators are saved only after successful processing. Only parsed, tracked filings within 90 days are stored; legacy whole-body submissions cache entries are deleted. The first import displays 30 days; later refreshes can expose the already-parsed 90-day cache after a 304.
- Filing identity is `(CIK, accession, issuer/reporting-owner role)`. The stored ticker is stable, while pane and chart readers resolve all share classes through the company map. Pane timestamps use acceptance time when available and otherwise show only the filing date. Reporting-owner Form 4 hits are retained but hidden from the default issuer pane.
- Chart loads reuse a cached store connection and query the charted CIK through the CIK/acceptance-or-filing-time index. XML legacy decoding is restricted to the declaration; body text is not rewritten.

Budget examples (continuous visible polling, direct responses): the two feeds cost 288/day, the map at most 1/day, and an ordinary daily sweep costs 50/day or 300/day: **339/day for 50 companies; 589/day for 300** before pending enrichment. A conservative sweep-cycle allowance of three pending fetches per company adds 150 or 900, giving **489** or **1,489**. A UTC calendar day can straddle a pending-counter reset at a daily sweep: for a fixed machine-wide set of N companies, allow up to six pending requests plus one sweep per company across the two partial cycles. This gives a conservative direct-response calendar-day bound of `min(289 + 7*N, 1489)`: **639/day for 50 companies; 1,489/day for 300**. Watchlist churn, additional instances with different watched sets, or redirect hops are not covered by that fixed-set estimate: the unconditional machine-wide maximum is **1,500/day for either size**. With direct responses, the 1,200 submissions cutoff plus at most 288 remaining feed requests and one map request gives a tighter traffic ceiling of **1,489/day**, independent of watchlist size. These are request bounds, not promises that every company refresh will complete when budget is deferred.

### N3: insider trades, Form 4 (`codex/edgar-insiders`)
- Parse Form 4 XML (owner, role, transaction code, shares, price, post-transaction holdings); only for watchlist CIKs found in the feed.
- Insider pane per symbol and watchlist, open-market buys highlighted (code P), clusters (≥ 2 insiders buying within 30 days).
- Chart markers next to Congress markers; alerts ("insider buy in a watchlist symbol") with optional macOS notification via `tr_alerts::delivery`.

#### N3-FIX implementation notes (2026-10-27)

- Form 4 XML is parsed once per accession and attributed to the XML issuer CIK, even if the first candidate came from a watched reporting owner. Owner-side rows stay hidden. Malformed/decode failures are terminal after one fetch; only HTTP/transport failures use the three-attempt 1 h / 6 h / 24 h schedule. The one-hour deadline is persisted before awaiting a request, so cancellation cannot immediately refetch it. Interest changes never cancel an in-flight scheduler tick.
- Shared accession claims precede the daily allowance check; contended claims consume no allowance. Source-wide refusals refund the per-accession attempt and release its claim, then stop the cycle. In production the guard atomically charges the Form 4 counter with its HTTP send reservation, not the scheduler preflight. Unsent refusals cost zero; responses that trigger a host pause still count as sent, and so does every redirect hop (using the UTC day at that hop). The 15-second Form 4 pace is ledger-only; there is no `f4:last` wall-clock gate. The machine-wide ceiling is **300 Form 4 HTTP requests/day**, including retries and redirects, for **both 8 and 300 watched companies**. Permanent XML/issuer failures no longer multiply traffic threefold.
- With direct responses, adding those 300 XML requests to the N2 fixed-set calendar-day bound yields `min(589 + 7*N, 1489)`: **645 total SEC requests/day for 8 companies; 1,489 for 300**. These are conservative bounds, not a guarantee all queued work completes. The Form 4 300/day ceiling holds even with redirects; the shared 1,200 enrichment cutoff also remains in force. Redirects or watchlist churn elsewhere still have the unconditional all-SEC ceiling of **1,500/day**, at either watchlist size and across data directories/instances.
- Stored snapshots are published at startup regardless of interest. Backlog lookup is one SQL query; snapshot hydration is five queries (one batched lot read), with a CIK-to-tickers map rather than per-row company scans. A fixture test asserts that bound for 300 companies / 9,000 populated rows. The pane keeps its runtime behind an Arc and caches search text and summary/filter work by snapshot identity and settings.
- One shared economic-lot projection deduplicates joint reporting owners and lets a 4/A supersede the original issuer/owner/date/code/shares match in summary, clusters and markers. Counts are filings, not lots; clusters use any rolling 30-day span in the shown window. Stored lots retain the issuer trading symbol, security title and document-local lot ID; known class symbols restrict rows/markers to that class, otherwise CIK classes are the fallback.
- Empty-state causes use snapshot flags, not status-text substring matches. Pane navigation uses exact unmodified keys, including repeats, and leaves unusable Escape/Enter/arrows alone. Prior-year feed timestamps use the six-character `Sep'25` grammar.

### N4: notifications and local agent access
- Implemented local-store-only feed evaluation for watched issuer filings, qualifying Form 4 purchases / distinct-owner clusters, per-watch opt-in keywords and optional four-source breaking clusters. All notification types default off. Settings live under Panels > Alerts; the separate News / Filings ALRT tab keeps up to 200 local events without changing the price-alert Book model.
- Claims are persisted before delivery, independently of UI read flags. First imports and Load once rows (including later promotion) are suppressed. Per-type enabling and session watermarks require publication after both and within two hours. Restart discards pending overflow delivery but preserves history and rate reservations. Feed-wide reservations cap desktop delivery at six per ten minutes, including `+N more in News` summaries. Local quiet hours retain history without catch-up.
- Hourly SQL counts cover topic tags and watched symbols. The worker evaluates at most once per five minutes against fourteen trailing same-hour buckets, requiring at least three covered days, count >= 5 and robust z >= 3 (median / MAD, scale floor one). Missing buckets after the first saved hour count as zero; this is storage coverage, not proof the importer ran continuously. Scoped News panes show a muted per-hour-dismissable banner; desktop volume alerts are opt-in.
- A pure deterministic digest groups by primary topic then primary subject (retaining all tags/subjects on each story), with timestamps, source counts, tickers, filing metadata and insider totals. News > Copy digest uses the same builder as `read_news`. Economic totals deduplicate joint lots; cluster identities retain all amendment-aware owners.
- `read_news` / `read_filings` are available in the default read-only agent set and route through application-target local worker jobs, without provider symbol discovery, chart pinning or on-demand fetches. Strict schemas cap news windows at 168 hours / 50 stories, filings at 90 days / 50 filings, and each filing at 50 saved Form 4 lines. Summaries are omitted unless explicitly requested, then limited to 300 Unicode characters; URLs expose only their host. Structured results are bounded to 16 KiB and preserved through session projection and expanded trace rendering. Descriptions/results disclose `headlines and filing metadata from local storage`; the pane also explains Finnhub personal/non-commercial use and SEC public-domain provenance.
- Delivery uses the existing `tr_alerts` notification command path. It has no click callback, so selecting the exact local row and guaranteed application activation remain unsupported. Feed monitoring requires the app worker; alertd is unchanged and does not fetch feeds. Native notification activation and daemon feed evaluation are deferred rather than adding platform services or daemon source interests in this phase.
- Optional RSS (Fed/BLS, press-release wires and additional crypto feeds) is explicitly out of scope: N4 adds no sources, request construction, dependencies or guard policies. No live/real-host app check is performed for this phase; fixtures and loopback tests only.

### SENATE-BARGO (2026-10-27)

- Senate now uses the optional-free-key Bargo Congress API (`www.bargo.ai`), never senate.gov;
  the permanent direct/redirect Senate host prohibition remains. Existing eFD history is retained.
  The bounded 60-day transaction-date sweep is an explicit Senate-only exception to the watchlist
  rule above, necessary for disclosures up to ~45 days late. Machine-wide slots: `bargo:sweep`
  (24 h), `bargo:poll` (3 h; first 10-day page only, visible window). A first store runs only a sweep.
- Guard: ≥30 s, ≤2/minute, concurrency 1, ≤40 HTTP attempts/day keyed or 10 keyless, including
  Settings health tests and failed sends. Durable row reservations cap returned trade rows at
  1,000/day keyed or 100/day keyless even without headers. Last reported request/row balances
  and UTC timestamps are persisted; requests below 5 or rows below 200 keyed / 20 keyless stop
  the run. Full sweep pages require enough remaining allowance; light pages may shrink.
- 401 key-error / 403 / 429 / 5xx / HTML challenge => persisted pause, no retry loop. 429 lasts
  until at least next UTC midnight. Overridden data dirs stay offline without exact opt-in.
- Secret account `bargo`, masked Save & Test (`/health`, availability rather than key validation),
  sensitive X-Api-Key header only. Free API covers three months, not unlimited key-enabled history.
  `/members` has no party and repeats trade state, so it is not fetched. Owner remains unknown.
- Stable SHA-1 row identities, normalized-name+state politician matching against DB roster history,
  eFD economic-match dedupe with durable seen markers; existing eFD rows win. Only first-seen
  disclosures within two days can alert, never the initial sweep. No filing_portal data is retained.
- Visible linked credit in Settings, Congress panes and charts: “Senate data: Bargo (bargo.ai)”.
  Bargo's terms prohibit raw-record redistribution. Tests use real fixtures and localhost only.
- Verification: `cargo fmt --all` and locked offline workspace Clippy with `-D warnings` pass.
  Locked offline workspace tests (Congress refresh forced to zero through Cargo environment config)
  pass with **1,890 passed, 8 ignored**. Added 20 Bargo regressions (11 Congress, 6 guard, 3 app),
  including rendered credit/browser URL and settings row, header-only secret handling, quota/cap
  persistence, first-sweep/poll claims across instances, eFD dedupe, refusal pauses, zero-socket
  offline mode and preservation of fresh alerts before a later-page refusal. No live-host launch,
  network opt-in, release build or git-state change was performed.

## Order and parallelism

N0 first (everything depends on the guard). Then N1a and N2 in parallel, N1b after N1a worktrees (different crates and panes). N3 after N2 (shares the CIK map and feed). Each phase: worker build, Opus review with ignored failing tests, fix worker, my verification, a live check in a throwaway instance with `TR_ALLOW_EXTERNAL=1` under the shared ledger, and then a ping.

## Decisions (2026-10-26)

- Free sources only: Finnhub free tier (user's key) and SEC EDGAR; Yahoo and CoinDesk as fallbacks.
- `feed-rs` is approved as a new dependency.
- The SEC contact email is entered by the user in Settings and stored only in local settings, never in the repository.
- The News pane defaults to general market news when no symbol is linked.
- Headline retention: 90 days.
