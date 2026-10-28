# News module: review and enhancement backlog (N5)

Status: 2026-10-28 — code review of the shipped news module at `personal/main` (`fb3bd09`) and a ranked
backlog of enhancements. This is a review, not a user guide: the [README](../../README.md#features) describes
the current controls, and [news-plan.md](news-plan.md) is the build plan that produced what shipped. Every
claim below is anchored to a file:line at `fb3bd09`; re-verify before acting, the module is moving. Follows
DESIGN.md ("The Native Trading Desk"): dense rows, monospace data, colour for meaning only, theme roles and
`dp()` only.

## Scope and method

Read in full: `crates/data/src/news_store.rs`, `crates/data/src/news_intelligence.rs`, `crates/data/src/rss.rs`,
`crates/data/src/finnhub.rs`, `crates/data/src/yahoo.rs`, `crates/data/src/crypto.rs`, `crates/net/src/lib.rs`,
`app/src/worker/news.rs`, `app/src/app/news_view.rs`, and the news surface of `app/src/app.rs`,
`crates/agent-tools/src/{registry.rs,args.rs,host.rs}`. Cross-checked against the worker loop, the store
schema, the intelligence heuristics and the existing test suite (133 test functions across
`news_intelligence_tests.rs`, `news_store_tests.rs`, `app/src/worker/news.rs`, `app/src/app/news_tests.rs`).

No live host was contacted and no network request was issued; every finding comes from source at the named
commit. Where a claim is about runtime behaviour it is marked **[runtime]** and names the path to observe it.

## What is already strong — keep these

Do not regress these while making the changes below; several were expensive to get right.

| Area | What is right | Where |
|---|---|---|
| Network citizenship | Machine-wide SQLite ledger shared across processes and data dirs; per-host rate, concurrency and daily caps; Retry-After parsing; escalating cooldowns; offline-by-default when `TR_DATA_DIR` is set; honest `terminal-rs/<ver>` identity; per-source switches and a global kill switch | `crates/net/src/lib.rs`, `docs/analysis/net-guard.md` |
| Subject precision | SEC company titles + local catalogue + curated names, an acronym denylist, excluded forms ("Apple pie", "Amazon rainforest"), stock-context gating | `crates/data/src/news_intelligence.rs:16-532`, `:575-588`, `:839-969` |
| Honesty about matches | The UI says "Not identified", or "Returned by <source> for <ticker>; not mentioned in the story", instead of implying a match that is not in the text | `app/src/app/news_view.rs:330-347` |
| Keyword matching | Watch matching is token-exact, so "ban" cannot match "bank"; search is prefix-anchored for the same reason | `crates/data/src/news_intelligence.rs:1044-1095`, `crates/data/src/news_store.rs:162-169` |
| Never blank the view | A failed or empty fetch never overwrites stored headlines | `app/src/app.rs:235-264` |
| Scroll and selection identity | Scroll anchor and selection are keyed by item id, not index, and survive refresh, reorder and expansion | `app/src/app/news_view.rs:148-157`, `:251-289`, `:1266-1269` |
| Render cost | Rows are cached behind an explicit invalidation key; two virtualisation paths (fixed height vs measured offsets); no idle repaint loop — only a pending read dwell schedules a repaint | `app/src/app/news_view.rs:47-179`, `:1207-1274` |
| Empty states | Four distinct causes — loading, no key, filtered, no history — each with an action | `app/src/app/news_view.rs:373-390`, `:897-951` |
| Notification hygiene | Persisted claims, a 2-hour freshness window, a 6-per-10-minute desktop cap with "+N more in News" overflow, quiet hours that retain history without catch-up | `crates/data/src/news_store.rs:210-237`, `:838-883` |
| Agent boundaries | `read_news` / `read_filings` are read-only, local-only, schema-bounded, URL-host-only, and a test asserts the local path cannot construct a network request | `app/src/worker/news.rs:88-147`, `:1258-1264`, `crates/agent-tools/src/registry.rs:632-669` |

## A. Correctness and operational risk — P0

### A1. Substring matching in the RSS filter produces false headlines

`crates/data/src/rss.rs:39-45` filters headlines with `t.contains(&w.to_lowercase())`. A three-letter
crypto base therefore matches inside unrelated words: `ETH` matches "something", `SOL` matches "sold",
`ADA` matches "Canada". The caller is the crypto path
(`crates/data/src/crypto.rs:378`), so a user watching a coin can be shown headlines that never mention it.

Every other matcher in the module is token-exact, which makes this one an inconsistency rather than a design
choice.

**Fix:** reuse the tokenisation already implemented in `news_intelligence` and require a whole-token match,
including the simple inflections the rest of the module already handles. **Acceptance:** a regression test
asserts that "something" does not match `ETH`, that "sold" does not match `SOL`, that "Canada" does not match
`ADA`, and that a genuine "Ethereum sell-off" still matches `ETH`.

### A2. `notification_seen` grows without bound

`crates/data/src/news_store.rs:566` creates `notification_seen(kind, id)`; rows are inserted at `:796`, `:829`
and there is **no `DELETE` against it anywhere in the tree**. Every observed story writes a row, and because
the table also carries cluster-delivery de-duplication it cannot be dropped casually. `items`,
`news_fetches`, `notification_history` and `notification_slots` are all trimmed; this table is not.

**Fix:** give it a retention window tied to the item retention (90 days), or reduce it to a rolling key set
per kind. **Acceptance:** a test seeds rows older than the retention window, runs the prune, and asserts the
table no longer grows without bound while live de-duplication still works.

### A3. The 5-second worker tick does O(N) SQL and holds a mutex across it

`local_tick` runs every 5 seconds (`app/src/worker/news.rs:751`) and issues roughly two selects plus one
insert per story over up to 2000 rows (`:329-345`), plus `cluster_previously_delivered`, which scans the
`items` table per cluster (`crates/data/src/news_store.rs:800-822`). It holds the `news.local` mutex for its
entire body (`:184-257`) and takes `news.store` inside it; `news::read` takes `local` first (`:552`). A
slow tick therefore queues UI reads behind it. The tick runs **whether or not a news pane is visible**,
unlike the network path, which correctly stops when no pane wants data (`:762`).

**Fix:** gate the evaluation on (a) at least one visible pane or (b) at least one enabled notification type;
move the per-story work into one set-based statement rather than a statement per story; release
`news.local` before taking `news.store`, or move the claim check into the store so only one lock is held.

### A4. Blocking `rusqlite` inside `async` functions

`app/src/worker/news.rs:193-204` and `:571-579` call blocking SQLite APIs directly on the async runtime,
whose worker pool has four threads (`app/src/worker.rs:1182-1186`). A slow query blocks a runtime thread.

**Fix:** wrap store work in `spawn_blocking`, or give the store a dedicated blocking thread.

### A5. Status and update time are service-wide, so panes lie to each other

`app/src/worker/news.rs:18-21` holds `status` and `updated` as service-level mutexes. One symbol's failure
rewrites the status line of every news pane, including panes showing unrelated symbols.

**Fix:** move status and `updated` into the per-view record the worker already publishes, so each pane
reports its own source state.

### A6. Two responses carry no pane id

`Response::FeedNotices` (`app/src/worker.rs:716-720`) and `Response::NewsVolume`
(`app/src/worker.rs:720`) are broadcast. They are also re-sent on every read (`app/src/worker/news.rs:552`),
so a notice can be delivered twice.

**Fix:** give both a `PaneId` and deliver only to the pane that owns the condition, or make the delivery
idempotent on the client side.

### A7. Undated items are stored with `ts = 0` and are invisible

`crates/data/src/rss.rs:30` uses `.map_or(0, ...)` and `crates/data/src/yahoo.rs:424` uses
`.unwrap_or_default()`. A zero timestamp falls outside every window, so the story exists in the store but can
never be selected.

**Fix:** treat an undated item as "now" at ingest and record that the timestamp was inferred, or keep it
excluded but say so. **Acceptance:** a test asserts an undated item is either visible with a sane timestamp or
explicitly labelled.

### A8. No request generation counter

Stale replies are rejected by full `View` equality (`app/src/app.rs:2450-2453`). That catches a reply for a
superseded symbol or scope, but two concurrent reads of the *same* view are indistinguishable, and a late
reply for an old view re-inserts itself into the published map (`app/src/worker/news.rs:502`), which makes the
next legitimate read look changed and triggers an extra `NewsSaved`.

**Fix:** add a monotonically increasing request generation per pane and drop any reply whose generation is
older than the latest issued.

### A9. The pane cannot wake its source

The overflow menu's "Reload" (`app/src/app/news_view.rs:672-677`) only re-reads local storage; its tooltip
says so, but the label does not. There is no way to ask the source for a refresh, and `n.loading` has no
timeout if the worker dies.

**Fix:** see C4 — rename the action and add a real source refresh that clears the cadence for that view.

### A10. Dead bookmark state

`items.bookmark` exists in the schema (`crates/data/src/news_store.rs:558-599`) and is exempt from the prune
(`:702-704`), but nothing ever writes it. It is schema-only state that the UI cannot express.

**Fix:** either implement save/unsave with a row action and a pane filter, or drop the column and the prune
exemption.

## B. Feature coverage — P1

### B1. Rows carry no importance signal

Every row renders at the same weight: time, ticker, headline, and grey tags in the right margin. A user
scanning a hundred headlines has no way to tell a market-moving story from a routine one.

There is a model-free way to do this that costs no inference and no validation: classify **priority** from
the headline text (breaking / urgent / announcement patterns), and surface it as a compact marker plus a
source-count prominence for stories carried by several outlets. Both are deterministic and cheap.

**Fix:** add a `priority` classification and a `source_count` prominence to the row model; expose priority in
the Stories view and in the digest. **Acceptance:** a fixture set of headlines produces the expected
priorities, and the Stories view shows the number of distinct sources per cluster.

### B2. Chronological ordering only

Rows are ordered by time. There is no relevance ordering, so a routine item published later always outranks
an important item published earlier.

**Fix:** add a Newest / Relevance toggle. Relevance = priority first, then recency, using the B1
classification. **Acceptance:** a mixed-priority fixture sorts as expected under both orders.

### B3. The agent can read news but not act on it

`read_news` and `read_filings` are the only news tools (`crates/agent-tools/src/registry.rs:632-669`), and
both are read-only. `DataHost` has no `news()` method at all
(`crates/agent-tools/src/host.rs:35-39`), so any agent path that uses the data host rather than the live
bridge cannot see news whatsoever.

The agent also cannot create a keyword watch, cannot ask for a refresh, and cannot report feed health.

**Fix:** extend `DataHost` with `news()` and add these tools:

| Tool | Purpose | Mutation |
|---|---|---|
| `add_watch` | create a keyword watch (label, words, colour, notify) | yes |
| `list_watches` / `delete_watch` | list and remove watches | yes / yes |
| `refresh_news` | ask the source for a refresh now | yes |
| `get_news_status` | feeds configured, active sources, cached count, last update, paused hosts | no |

Give every list tool an explicit truncation contract rather than a bare array — for example
`"showing 20 of 87 matching headlines; raise limit (max 50) or narrow the scope"`. The current digest already
bounds output to 16 KiB (`app/src/agent_bridge.rs:1107-1132`); keep that and state it in the result.

### B4. The digest's markdown drops information the JSON keeps

`local::digest` (`crates/data/src/news_store.rs:374-419`) carries per-story sources, source counts and URL
hosts; the markdown renderer (`:421-505`) drops the per-story source and host. The markdown is what a person
copies to the clipboard (`app/src/worker.rs:2662-2685`), so the artefact people use is the weaker one.

**Fix:** keep source and host in the markdown rendering.

### B5. `read_news` scope semantics are not documented

`scope = market` returns filing and insider metadata for the whole watchlist
(`app/src/worker/news.rs:95`, `:130-131`), and `scope = watchlist` with no name silently uses the active
watchlist (`app/src/agent_ui.rs:64`). Neither is in the tool description
(`crates/agent-tools/src/registry.rs:662-664`).

**Fix:** state the behaviour in the description, and make an unnamed watchlist scope an explicit error rather
than a silent fallback.

### B6. The feed set is fixed at compile time

Feed URLs are constants (`crates/data/src/rss.rs:9`, `crates/data/src/finnhub.rs:84`, `:113`,
`crates/data/src/yahoo.rs:439`) and transport policies live in `crates/net/src/lib.rs:68-101`. There is no
catalogue, no tiers, no user-added feeds and no way to see which feeds exist or when each last returned
something.

**Fix:** see D1.

## C. UI/UX — P1

### C1. The pane's keyboard map is undiscoverable

The pane handles ten keys — `↑`/`↓` and `j`/`k`, `Enter`, `Space`, `C`, `/`, `U`, `Esc`
(`app/src/app/news_view.rs:806-818`) — and none of them appear in the pane. The hotkeys panel documents only
`N` and `AAPL CN` as the way to open the pane (`app/src/app/hotkeys_view.rs`, Panels section). A first-time
user has no way to learn that `Space` expands a story or `C` charts it.

**Fix:** add a one-line key legend to the pane footer — the pattern already exists in
`app/src/symbol_search.rs:413-431` — and add a News section to the hotkeys panel listing the pane's own
keys and the row actions. **Acceptance:** every key handled in `news_view.rs:806-818` appears in the legend.

### C2. A watched story loses its unread marker

`app/src/app/news_view.rs:1006-1017` draws the watch stripe **or** the unread dot, never both, so a story
that matches a keyword watch also loses the "you have not read this" signal.

**Fix:** draw the stripe and the dot independently.

### C3. The filters popover scales badly and is ambiguous

`app/src/app/news_view.rs:493-582` lists every source as a flat run of selectable labels with no search
box, and the two word fields are labelled "Include words (any)" and "Exclude words" with no indication of
whether the value is a single phrase or a list.

**Fix:** add a filter field above the source list, show the number of matching sources, and state the
matching rule in the hint text.

### C4. "Reload" does not reload

The button reads "Reload" and re-reads local storage only (`app/src/app/news_view.rs:672-677`); its tooltip
says "Reload saved history; source pacing is unchanged", which is accurate but contradicts the label. There is
no action that asks the source for fresh data.

**Fix:** rename it "Re-read history", and add a separate "Refresh source" that clears the cadence for that
view and issues a guarded request.

### C5. Every pane shows the same status line

Because status is service-wide (A5), a pane showing a healthy symbol can display a failure that belongs to
another symbol. Combined with the compact status formatting
(`app/src/feed_time.rs:81`), a user cannot tell which source is in trouble.

**Fix:** per-pane status, and include the failing scope in the message.

### C6. The Stories view does not lead with the story's strength

For a clustered story the number of distinct sources is the strongest available "this is a real event" cue,
but it is rendered in the right-hand meta column, competing with tags and the source name. The meta column
also degrades by dropping information in a fixed order (`app/src/app/news_view.rs:357`), so on a narrow pane
the source count may be the first thing lost.

**Fix:** in the Stories view, give source count its own leading column; keep the current ladder for the
trailing metadata.

### C7. The search field and the filters popover are hard to reach

Search is only visible as an icon when the pane is narrow (`app/src/app/news_view.rs:773-777`), and the
filters live behind a popup. On a wide pane both are present but the search field expands to the full
remaining width (`:779-783`), which pushes the filter button to the far edge.

**Fix:** give the search field a maximum width so the filter control stays adjacent, and show the active
filter summary in the collapsed state rather than only a count.

## D. Breadth — P2

### D1. A real feed registry

General market news currently rests on one API's market category, with a fallback that searches a single
phrase. That makes the Market scope the weakest part of the module, and it means a feed that dies is
invisible until it fails.

**Fix:** introduce a feed catalogue with:

- a built-in set of feeds across tiers (wires and regulators, major financial media, global news, sector
  and specialty, blogs), each with a category, region, source display name and tier;
- user-added feeds, stored locally, with an enable/disable switch;
- a probe that runs when a feed is added: classify the payload as RSS 2.0, Atom, RSS 1.0/RDF, HTML or
  not-a-feed; report item count, newest-item age and payload size; **reject feeds whose newest item is more
  than a week old** so dead feeds cannot be added silently;
- all requests routed through the existing guard with a per-host policy, and every feed shown with its tier,
  last success, last item age and pause state.

### D2. Optional sentiment, validated before it is shown

Sentiment is absent today, and that is a reasonable default: an unvalidated score is noise in front of a
professional user. The way to earn the right to show one is to validate it against stored history first —
score a sample of headlines, compare against subsequent price moves for the same subject, and only surface
the column if the correlation is real.

**Fix:** keep it out of the row until the validation exists. Build the validation harness first; it needs no
UI and no user-visible change.

### D3. Feed health surfaced in the pane

There is no per-feed health view. A user cannot answer "is this feed stale?" without leaving the pane.

**Fix:** show the active source count and the oldest active feed's last-item age in the status line, and list
paused hosts with their resume time — the information already exists in the guard's ledger.

## Backlog summary

| ID | Title | Area | Priority | Effort | Depends on |
|---|---|---|---|---|---|
| A1 | Token-exact RSS filtering | correctness | P0 | S | — |
| A2 | Prune `notification_seen` | correctness | P0 | S | — |
| A3 | Gate and batch the 5-second tick | performance | P0 | M | — |
| A4 | Move blocking SQLite off the runtime | performance | P0 | S | — |
| A5 | Per-pane status and update time | correctness | P0 | S | — |
| A6 | Pane id on `FeedNotices` / `NewsVolume` | correctness | P0 | S | A5 |
| A7 | Undated items | correctness | P0 | S | — |
| A8 | Request generation counter | correctness | P0 | S | — |
| A9/C4 | Real source refresh | feature | P0 | S | A8 |
| A10 | Bookmark state: implement or remove | feature | P1 | S | — |
| B1 | Priority and source-count signals | feature | P1 | M | — |
| B2 | Relevance sort | feature | P1 | S | B1 |
| B3 | Agent actions and `DataHost::news()` | feature | P1 | L | — |
| B4 | Keep source and host in the digest markdown | feature | P1 | S | — |
| B5 | Document `read_news` scope semantics | feature | P1 | S | — |
| C1 | Key legend and hotkeys entry | UX | P1 | S | — |
| C2 | Unread dot alongside the watch stripe | UX | P1 | S | — |
| C3 | Searchable source filter, clearer word fields | UX | P1 | M | — |
| C5 | Per-pane status line | UX | P1 | S | A5 |
| C6 | Lead with source count in Stories | UX | P1 | S | B1 |
| C7 | Bounded search field, filter summary | UX | P2 | S | — |
| D1 | Feed registry, user feeds, probe | breadth | P2 | L | A3, A4 |
| D2 | Sentiment validation harness | breadth | P2 | M | B1 |
| D3 | Feed health in the pane | breadth | P2 | M | D1 |

Effort: S ≈ under a day, M ≈ one to three days, L ≈ more than a week.

## Documentation drift to correct

These are small, but they make the plan document untrustworthy as a specification.

| Location | Says | Code |
|---|---|---|
| `docs/analysis/news-plan.md:3` | "N4 notifications, anomalies, digest and agent read_news/read_filings remain unshipped" | All four are implemented (`app/src/worker/news.rs:88-257`, `crates/agent-tools/src/registry.rs:662-669`) |
| `docs/analysis/news-plan.md:119` | "Claims are persisted before delivery" | Claims are committed after the handoff (`app/src/worker/news.rs:258-273`); the store comment at `crates/data/src/news_store.rs:836` says the opposite of the plan |
| `docs/analysis/news-plan.md:31` | "Hidden window … → no polling" | True for the network, but `local_tick` still evaluates every 5 seconds while hidden (`app/src/worker/news.rs:749-751`) |
| `docs/analysis/news-plan.md:121` | Digest carries "timestamps, source counts, tickers, filing metadata and insider totals" | True for the JSON; the markdown renderer drops per-story source and host (`crates/data/src/news_store.rs:421-505`) |
| `docs/analysis/news-filings-design.md:36` | "Reload …" reads as a source reload | It re-reads local storage only (`app/src/app/news_view.rs:672-677`) |
| `app/src/app.rs:3017` | "News every 5 minutes" | The worker loop ticks every 5 seconds; the source cadence is 5 minutes for the general category (`crates/data/src/finnhub.rs:185-248`) |

## Explicitly out of scope for this backlog

- Sentiment scoring in the row, until the validation harness in D2 exists and passes.
- Prediction markets, geolocation and threat labelling: no source in the current feed set supports them, and
  adding them would need new hosts, new guard policies and their own provenance decisions.
- Live or real-host validation of any change here: this review issued no network request, and the existing
  suite is fixture-based and offline by design.

## How to verify this backlog

Each item names the assertion that closes it. The suite to extend is
`crates/data/src/news_intelligence_tests.rs` (35 functions), `crates/data/src/news_store_tests.rs` (33),
`app/src/worker/news.rs` (25) and `app/src/app/news_tests.rs` (40) — all offline, all fixture-driven. Two
existing gaps are worth closing first because they are the ones that would catch a regression in this
work: a real-data audit table is currently a specification table rather than real data
(`crates/data/src/news_intelligence_tests.rs:716-718`, with the real audit `#[ignore]`d pending an external
data directory at `:892-893`), and RSS 1.0/RDF, non-UTF-8 charset handling and `content:encoded` bodies are
untested.
