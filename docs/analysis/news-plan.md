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
- Optional N0b: move Congress onto `tr_net` (its House fetch then also obeys the machine-wide ledger).

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

### N3: insider trades, Form 4 (`codex/edgar-insiders`)
- Parse Form 4 XML (owner, role, transaction code, shares, price, post-transaction holdings); only for watchlist CIKs found in the feed.
- Insider pane per symbol and watchlist, open-market buys highlighted (code P), clusters (≥ 2 insiders buying within 30 days).
- Chart markers next to Congress markers; alerts ("insider buy in a watchlist symbol") with optional macOS notification via `tr_alerts::delivery`.

### N4: polish and agent access (later)
- Desktop notifications for watchlist filings, breaking headlines and keyword watches (fresh items only, de-duplicated by ID).
- Headline-volume anomalies: hourly counts per category against a stored baseline (z-score), once at least a day of history exists.
- A deterministic digest (top headlines grouped by category, with sources and tickers) as the shape for the agent's news tool.
- Agent tools `read_news` / `read_filings` (read-only). Licence note: Finnhub's free tier is personal and non-commercial; EDGAR is public domain. Default the agent to headlines and filings metadata only, and say so in the pane's disclosure text.
- Optional free RSS: Fed and BLS releases, press-release wires (GlobeNewswire, PR Newswire), Cointelegraph/Decrypt.

## Order and parallelism

N0 first (everything depends on the guard). Then N1a and N2 in parallel, N1b after N1a worktrees (different crates and panes). N3 after N2 (shares the CIK map and feed). Each phase: worker build, Opus review with ignored failing tests, fix worker, my verification, a live check in a throwaway instance with `TR_ALLOW_EXTERNAL=1` under the shared ledger, and then a ping.

## Decisions (2026-10-26)

- Free sources only: Finnhub free tier (user's key) and SEC EDGAR; Yahoo and CoinDesk as fallbacks.
- `feed-rs` is approved as a new dependency.
- The SEC contact email is entered by the user in Settings and stored only in local settings, never in the repository.
- The News pane defaults to general market news when no symbol is linked.
- Headline retention: 90 days.
