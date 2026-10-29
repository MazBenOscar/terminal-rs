# N6: first built-in RSS feeds (regulators, global media, tech)

Status: 2026-10-28 — plan, approved scope. Implements the first slice of item **D1** (and a light **D3**) from
[news-module-improvements.md](news-module-improvements.md), using the vocabulary in
[news-feed-catalogue.md](news-feed-catalogue.md). Everything else in the catalogue stays out of scope.

## Scope: 12 feeds in 3 groups

Each endpoint below was fetched once by hand on 2026-10-28 with an honest identity (one request each) and
returned a well-formed feed.

| id | Publisher | URL | Format notes (from the probe) | Category | Region | Tier | Group |
|---|---|---|---|---|---|---|---|
| `fed-press` | FEDERAL RESERVE | `https://www.federalreserve.gov/feeds/press_all.xml` | RSS 2.0, 20 items; `pubDate` and `link` wrapped in CDATA | MACRO | US | 1 | `regulators` |
| `sec-press` | SEC | `https://www.sec.gov/news/pressreleases.rss` | RSS 2.0, 25 items, a few per week | REGULATORY | US | 1 | `sec` (existing) |
| `ecb-press` | ECB | `https://www.ecb.europa.eu/rss/press.html` | RSS 2.0, 15 items, weekly cadence | MACRO | EU | 1 | `regulators` |
| `boe-news` | BANK OF ENGLAND | `https://www.bankofengland.co.uk/rss/news` | RSS 2.0, 50 items, weekly cadence | MACRO | UK | 1 | `regulators` |
| `bbc-business` | BBC | `https://feeds.bbci.co.uk/news/business/rss.xml` | RSS 2.0, 52 items, hourly | MARKETS | GLOBAL | 2 | `global-media` |
| `bbc-world` | BBC | `https://feeds.bbci.co.uk/news/world/rss.xml` | RSS 2.0, 27 items, hourly | GEOPOLITICS | GLOBAL | 2 | `global-media` |
| `cnbc-top` | CNBC | `https://search.cnbc.com/rs/search/combinedcms/view.xml?partnerId=wrss01&id=100003114` | RSS 2.0, 30 items, fresh | MARKETS | US | 2 | `global-media` |
| `cnbc-world` | CNBC | `https://search.cnbc.com/rs/search/combinedcms/view.xml?partnerId=wrss01&id=100727362` | RSS 2.0, 30 items, fresh | MARKETS | GLOBAL | 2 | `global-media` |
| `aljazeera` | AL JAZEERA | `https://www.aljazeera.com/xml/rss/all.xml` | RSS 2.0, 25 items, fresh | GEOPOLITICS | GLOBAL | 2 | `global-media` |
| `techcrunch` | TECHCRUNCH | `https://techcrunch.com/feed/` | RSS 2.0, 20 items | TECH | GLOBAL | 2 | `tech` |
| `theverge` | THE VERGE | `https://www.theverge.com/rss/index.xml` | **Atom**; the root element follows a line break after the XML declaration | TECH | GLOBAL | 3 | `tech` |
| `hackernews` | HACKER NEWS | `https://hnrss.org/frontpage` | RSS 2.0, 20 items (third-party mirror of HN) | TECH | GLOBAL | 4 | `tech` |

Rejected at probe time: CNBC Finance (`id=10000664`), because its newest item was 7 days old. Everything on
the catalogue's "avoid" list stays out (Investing.com, The Block, OilPrice, Seeking Alpha direct, Bloomberg,
WSJ, Economist, Nikkei, ZeroHedge, Reuters, AP via RSSHub, IMF, Kitco).

## Design

### 1. Registry (`crates/data/src/feeds.rs`, new)

- A static, built-in catalogue: `Feed { id, name, url, publisher, category, region, tier, group, cadence }`,
  following the catalogue's row shape.
- Runtime state lives in a small `feed_state` table in news.sqlite3. This is an additive migration,
  compatible with older builds, which ignore unknown tables. Per feed it holds:
  - `etag` and `last_modified`
  - `last_success_at`, `last_item_at` and `last_status`
  - `items_today`
- Ingested items get:
  - `provider = "rss"` and `provider_id = <feed id>`
  - `source = publisher`
  - `category = feed category` (lowercased, matching the existing column)
- Story ids hash the feed's link or guid, consistent with the CoinDesk path.
- The same story from BBC and CNBC still clusters through the existing story clustering.

### 2. Guard (`crates/net`)

Every request goes through `tr_net`. No fetch path may bypass it.

| Group | Hosts | Min interval per host | Daily cap per host | Notes |
|---|---|---|---|---|
| `regulators` | federalreserve.gov, ecb.europa.eu, bankofengland.co.uk | 15 min | 120 | `.gov`-class hosts: slow and polite |
| `sec` (existing) | sec.gov | existing SEC policy | shares the existing SEC budget | sends the existing SEC contact user agent |
| `global-media` | feeds.bbci.co.uk, search.cnbc.com, aljazeera.com | 15 min | 120 | |
| `tech` | techcrunch.com, theverge.com, hnrss.org | 30 min | 60 | |

- **Conditional requests.** Send `If-None-Match` / `If-Modified-Since`. A 304 counts as success: the feed is
  alive and unchanged, and no body is downloaded.
- **Pause, never retry hard.**
  - HTTP 403, 429 or 451, or a 200 whose body is HTML or not a feed (classified on the root element, not the
    content type), pauses that host through the existing cooldown and strike machinery.
  - The pause reason is shown in Settings.
  - Three strikes pause the host until a manual re-enable.
- **Offline whenever `TR_DATA_DIR` is set**, as today. Tests and demo instances never touch these hosts.
- **User agent:** the existing `UserAgent::Research` identity plus the group name. Only the SEC request
  carries the SEC contact.
- **Volume:** about 12 hosts × 96 requests/day at most, and most of those come back 304. That is far below
  any host's tolerance, and far below the catalogue's 5,250/day estimate for all 53 feeds.

### 3. Fetch and ingest (`app/src/worker/news.rs`)

- One scheduler entry per feed, staggered so hosts are never hit in the same second, and gated by the
  N5a tick.
- Blocking SQLite runs on `spawn_blocking`, following the A4 rule.
- Parse with the existing `parse_rss`, which already handles RSS and Atom. Add fixtures for:
  - Fed-style CDATA dates and links
  - the Verge-style Atom root after a line break

  The fixtures are synthetic, mirroring each feed's structure, never copied article text.
- Undated items follow the N5a `inferred_ts` rule.
- Feed items take part in the existing priority and event classifier, keyword watches and notifications
  exactly like Finnhub items. Add the phrases `fomc statement` and `monetary policy decision` (FED, High)
  so central-bank decisions stand out.

### 4. Where the stories appear

- **Market scope:** all enabled feeds.
- **Symbol and Watchlist scopes:** feed stories appear only through the existing ticker and subject
  matching, as Finnhub general news does today.
- **Filters popover:** the source list already lists publishers. Add a **Category** row of chips (MARKETS,
  MACRO, REGULATORY, GEOPOLITICS, TECH, CRYPTO) so you can, for example, hide TECH in one pane and show only
  TECH in another. This is per pane and persisted, with a serde default that means "all".

### 5. Settings › Data sources

- One row per group (Regulators, Global media, Tech), each with:
  - an on/off switch
  - requests today against the cap
  - the pause state and reason, with Resume
- Expanding a group shows one checkbox per feed, with:
  - last success
  - newest item age
  - the status: OK, Quiet, Paused or Not a feed
- Quiet means no new item for a while: more than 30 days for regulators, more than 3 days for media and
  tech. Quiet is shown in grey, not as an error, because the ECB and BoE often go a week without a release.
- **Defaults:** all three groups are on, matching the user's choice. Each feed can be switched off
  individually.
- The SEC press feed shows under the existing SEC row as a sub-checkbox.

### 6. Not in N6

- User-added feeds and the probe UI
- The other 41 catalogue feeds
- The tier row badge and tier-based cluster lead
- Language support
- Licence metadata

All of these are tracked in the catalogue's rollout. The tier is stored on the feed so the later steps need
no migration.

## Acceptance

1. **Fixture parsing.** Each of the 12 feeds' shapes parses. The CDATA date becomes a real timestamp, not
   0, and the Atom root after a line break is read.
2. **Guard.**
   - Per-host spacing and caps are enforced.
   - 304 is a success.
   - 403/429 or an HTML body pauses the host and records the reason; no immediate retry happens.
   - `TR_DATA_DIR` set means zero network.
3. **Conditional GET.** The ETag and Last-Modified round-trip.
4. **Ingest.**
   - `provider_id` is the feed id and `category` is the feed category.
   - Re-ingesting is idempotent.
   - The same link from two feeds becomes one story with two sources in Stories.
5. **Settings.** The group switch and per-feed checkboxes persist, and a disabled feed is never fetched
   (asserted through the guard ledger in tests).
6. **Filters.** The category chips filter as expected, and old pane JSON loads with all categories visible.
7. **Live check (manual, by the user).** After the merge, the Market view shows BBC, CNBC, Al Jazeera,
   TechCrunch, The Verge and HN stories within about 15–30 minutes. The Fed's FOMC statement appears with a
   FED label. Settings shows every feed OK, or Quiet for regulators.

## Delivery

- **Branch:** `codex/news-n6`, branched from `personal/main` after N5c merges.
- **Build:** one builder (the registry, guard, ingest and Settings share files, so splitting them would
  conflict).
- **Review:** an independent Opus review focused on guard bypass, pause behaviour and conditional GET.
- **Fixes:** fix pass, then a full workspace test run.
- **Before the first live launch:** back up the user's data.
- **Roll-out:** the first live run enables only `regulators` for one cycle, I check the ledger, and then the
  remaining groups are switched on.
