# News feed catalogue and vocabulary (N5-D1)

Status: 2026-10-28 — candidate seed catalogue, vocabulary and registry shape for the feed registry tracked as
item **D1** in [news-module-improvements.md](news-module-improvements.md). This is a reference and a design
note, not an implementation. Nothing described here is enabled, and no endpoint in it has been verified
against a live host from this repository — see [Provisioning rules](#provisioning-rules).

## Purpose

The News pane currently has no feed catalogue. It has one provider (Finnhub), two fallbacks (Yahoo search,
CoinDesk RSS) and a fixed set of URLs hardcoded as constants. That shape has three costs:

1. **Coverage.** General market news rests on a single vendor's market category, and the fallback is a
   search for the literal phrase `"stock market"`. A market-wide wire does not exist.
2. **Visibility.** A feed that dies is invisible until it fails. There is no list of what should be arriving,
   so nothing can report that it is not.
3. **Control.** The guard's on/off switches are per vendor (`crates/net/src/lib.rs:117-131`), so adding feeds
   one vendor at a time would multiply Settings rows without telling the user anything.

This document defines the vocabulary and the candidate catalogue that would close those three gaps. It does
not decide the implementation; it decides the *shape* of the data so the implementation has something
concrete to build against.

## Vocabulary

Three new pieces of vocabulary. All three are feed-level and none of them replaces anything that exists
today.

### Feed category

A coarse desk label assigned to a feed, used for grouping and for the source filter. Chosen to sit
**above** the headline-derived topic tags rather than duplicate them.

| Category | Covers | Existing headline tag it overlaps |
|---|---|---|
| `MARKETS` | equities, FX, commodities, company news, money markets | — |
| `MACRO` | central banks, inflation, labour, growth, sovereign | `Macro` |
| `REGULATORY` | regulators, courts, official filings, policy | `Legal`, `FDA` |
| `GEOPOLITICS` | world affairs, conflict, diplomacy, elections | — |
| `CRYPTO` | digital assets, exchanges, on-chain | `Crypto` |
| `ENERGY` | oil, gas, power, nuclear, climate | — |
| `TECH` | technology, software, semiconductors, research | `Analyst` |
| `DEFENSE` | military, arms, defence policy, procurement | — |

`DEFENSE` has no seed feed in the catalogue. It exists because the headline classifier can produce it and a
feed added later may need it.

### Region

Where the publisher sits, not where the story happens. Used for source diversity and for trading-hour
reasoning, not for filtering by story geography.

`GLOBAL` (29 feeds) · `US` (12) · `INDIA` (4) · `ASIA` (3) · `EU` (3) · `UK` (1) · `MENA` (1)

### Tier

Publisher quality, 1 (best) to 4. It is a single field with three concrete jobs:

1. **Cluster lead selection.** When several outlets carry the same story, the lead article is the one from
   the lowest tier; ties break on recency. This is the mechanism that keeps a wire or a major outlet at the
   top of a clustered story instead of a blog that happened to publish first.
2. **Row badge.** A one-character quality marker per row: tier 1 `★`, tier 2 `●`, tier 3–4 `·`.
3. **Stored per article.** The tier of the feed that supplied a story is persisted with it, so a story keeps
   its provenance after the feed is disabled or removed from the catalogue.

### How this differs from what exists today

| Concept | Level | Set by | Status |
|---|---|---|---|
| `Scope` (`Symbol` / `Watchlist` / `All`) | pane | the user | exists — `crates/data/src/news_store.rs:89-95` |
| Topic tags (9, headline-derived, max 2) | story | the classifier | exists — `crates/data/src/news_intelligence.rs:970-991` |
| `items.category` (currently `general` or a vendor market label) | story | the transport | exists but too coarse to filter on |
| **Feed category** | feed | the catalogue | new |
| **Region** | feed | the catalogue | new |
| **Tier** | feed | the catalogue | new |

A story's feed category and its headline tags are independent axes: a `MARKETS` feed can yield a story
tagged `Earnings`, and a `GEOPOLITICS` feed can yield a story tagged `Analyst`. The existing
`items.category` column is the natural home for the feed category once a registry exists; today it holds
`general` or the vendor's own market label (`forex` / `crypto` / `merger`).

## Registry row shape

Proposed shape for one catalogue entry. The first six fields are catalogue data and ship with the app; the
rest are runtime state the registry maintains.

```rust
struct Feed {
    id: String,        // stable slug, unique; also the item's provider_id
    name: String,      // display name
    url: String,       // fetch URL
    publisher: String, // publisher as it should appear in the row and in the source filter
    category: String,  // one of the categories above
    region: String,    // one of the regions above
    tier: u8,          // 1..=4
    group: String,     // guard group; several feeds may share one group and one policy
    enabled: bool,     // user switch
    builtin: bool,     // shipped vs user-added
    // runtime state
    last_success_at: Option<i64>,
    last_item_at: Option<i64>,   // publication time of the newest item seen
    probe_state: Option<String>,  // last probe verdict
    paused_until: Option<i64>,    // set by the guard, shown in the UI
}
```

`id` doubles as the item's `provider_id`, so a story always knows which catalogue entry produced it even
after the entry is disabled. `group` is what makes the Settings UI usable: it lets forty publishers share
one guard policy and one switch, instead of producing forty Settings rows.

## The catalogue

53 candidate feeds. `★` marks tier 1, `●` tier 2, `·` tier 3–4.

### Tier 1 — wire services and regulators

| id | Name | URL | Publisher | Category | Region |
|---|---|---|---|---|---|
| `sec-press` | SEC Press Releases | `https://www.sec.gov/news/pressreleases.rss` | SEC | REGULATORY | US |
| `fed-press` | Federal Reserve | `https://www.federalreserve.gov/feeds/press_all.xml` | FEDERAL RESERVE | REGULATORY | US |
| `un-news` | UN News | `https://news.un.org/feed/subscribe/en/news/all/rss.xml` | UN | GEOPOLITICS | GLOBAL |
| `ecb-press` | ECB Press | `https://www.ecb.europa.eu/rss/press.html` | ECB | REGULATORY | EU |
| `boe-news` | Bank of England | `https://www.bankofengland.co.uk/rss/news` | BOE | REGULATORY | UK |

### Tier 2 — major financial media

| id | Name | URL | Publisher | Category | Region |
|---|---|---|---|---|---|
| `bloomberg-mkts` | Bloomberg Markets | `https://feeds.bloomberg.com/markets/news.rss` | BLOOMBERG | MARKETS | GLOBAL |
| `wsj-markets` | WSJ Markets | `https://feeds.a.dj.com/rss/RSSMarketsMain.xml` | WSJ | MARKETS | US |
| `wsj-world` | WSJ World | `https://feeds.a.dj.com/rss/RSSWorldNews.xml` | WSJ | GEOPOLITICS | GLOBAL |
| `marketwatch` | MarketWatch | `https://feeds.marketwatch.com/marketwatch/topstories/` | MARKETWATCH | MARKETS | US |
| `cnbc-finance` | CNBC Finance | `https://search.cnbc.com/rs/search/combinedcms/view.xml?partnerId=wrss01&id=100003114` | CNBC | MARKETS | US |
| `seekingalpha` | Seeking Alpha | `https://seekingalpha.com/market_currents.xml` | SEEKING ALPHA | MARKETS | US |
| `investing-news` | Investing.com | `https://www.investing.com/rss/news.rss` | INVESTING.COM | MARKETS | GLOBAL |
| `economist` | The Economist | `https://www.economist.com/finance-and-economics/rss.xml` | ECONOMIST | MACRO | GLOBAL |
| `fxstreet` | FXStreet | `https://www.fxstreet.com/rss/news` | FXSTREET | MARKETS | GLOBAL |
| `mining-com` | Mining.com | `https://www.mining.com/feed/` | MINING.COM | MARKETS | GLOBAL |
| `benzinga` | Benzinga | `https://www.benzinga.com/feed` | BENZINGA | MARKETS | US |

### Tier 2 — global news

| id | Name | URL | Publisher | Category | Region |
|---|---|---|---|---|---|
| `bbc-world` | BBC World | `http://feeds.bbci.co.uk/news/world/rss.xml` | BBC | GEOPOLITICS | GLOBAL |
| `bbc-business` | BBC Business | `http://feeds.bbci.co.uk/news/business/rss.xml` | BBC | MARKETS | GLOBAL |
| `aljazeera` | Al Jazeera | `https://www.aljazeera.com/xml/rss/all.xml` | AL JAZEERA | GEOPOLITICS | GLOBAL |
| `nyt-world` | NYT World | `https://rss.nytimes.com/services/xml/rss/nyt/World.xml` | NYT | GEOPOLITICS | GLOBAL |
| `guardian-world` | Guardian World | `https://www.theguardian.com/world/rss` | GUARDIAN | GEOPOLITICS | GLOBAL |
| `france24` | France 24 | `https://www.france24.com/en/rss` | FRANCE 24 | GEOPOLITICS | EU |
| `foreignpolicy` | Foreign Policy | `https://foreignpolicy.com/feed/` | FOREIGN POLICY | GEOPOLITICS | GLOBAL |
| `middle-east-eye` | Middle East Eye | `https://www.middleeasteye.net/rss` | MIDDLE EAST EYE | GEOPOLITICS | MENA |
| `scmp` | South China Morning Post | `https://www.scmp.com/rss/91/feed` | SCMP | GEOPOLITICS | ASIA |

### Tier 2 — crypto

| id | Name | URL | Publisher | Category | Region |
|---|---|---|---|---|---|
| `coindesk` | CoinDesk | `https://www.coindesk.com/arc/outboundfeeds/rss/` | COINDESK | CRYPTO | GLOBAL |
| `cointelegraph` | CoinTelegraph | `https://cointelegraph.com/rss` | COINTELEGRAPH | CRYPTO | GLOBAL |
| `theblock` | The Block | `https://www.theblock.co/rss.xml` | THE BLOCK | CRYPTO | GLOBAL |
| `decrypt` | Decrypt | `https://decrypt.co/feed` | DECRYPT | CRYPTO | GLOBAL |
| `finextra` | Finextra | `https://www.finextra.com/rss/headlines.aspx` | FINEXTRA | TECH | GLOBAL |

### Tier 2 — energy, tech and Europe

| id | Name | URL | Publisher | Category | Region |
|---|---|---|---|---|---|
| `oilprice` | OilPrice.com | `https://oilprice.com/rss/main` | OILPRICE | ENERGY | GLOBAL |
| `techcrunch` | TechCrunch | `https://techcrunch.com/feed/` | TECHCRUNCH | TECH | GLOBAL |
| `wired` | Wired | `https://www.wired.com/feed/rss` | WIRED | TECH | US |
| `cnbc-world` | CNBC World | `https://search.cnbc.com/rs/search/combinedcms/view.xml?partnerId=wrss01&id=100727362` | CNBC | MARKETS | GLOBAL |
| `cnbc-tech` | CNBC Technology | `https://search.cnbc.com/rs/search/combinedcms/view.xml?partnerId=wrss01&id=19854910` | CNBC | TECH | US |
| `dw-world` | Deutsche Welle | `https://rss.dw.com/rdf/rss-en-all` | DW | GEOPOLITICS | EU |

### Tier 2 — Asia and India

| id | Name | URL | Publisher | Category | Region |
|---|---|---|---|---|---|
| `nikkei-asia` | Nikkei Asia | `https://asia.nikkei.com/rss/feed/nar` | NIKKEI ASIA | MARKETS | ASIA |
| `hindu-biz` | The Hindu Business | `https://www.thehindu.com/business/feeder/default.rss` | THE HINDU | MARKETS | INDIA |
| `livemint` | LiveMint | `https://www.livemint.com/rss/markets` | LIVEMINT | MARKETS | INDIA |
| `et-markets` | Economic Times | `https://economictimes.indiatimes.com/rssfeedtopstories.cms` | ECONOMIC TIMES | MARKETS | INDIA |
| `moneycontrol` | MoneyControl | `https://www.moneycontrol.com/rss/latestnews.xml` | MONEYCONTROL | MARKETS | INDIA |
| `channel-news-asia` | CNA | `https://www.channelnewsasia.com/rssfeeds/8395986` | CNA | MARKETS | ASIA |

### Tier 3 — economic, macro and specialty (8)

| id | Name | URL | Publisher | Category | Region |
|---|---|---|---|---|---|
| `zero-hedge` | ZeroHedge | `https://feeds.feedburner.com/zerohedge/feed` | ZEROHEDGE ⚠ | MACRO | GLOBAL |
| `calculated-risk` | Calculated Risk | `https://feeds.feedburner.com/CalculatedRisk` | CALCULATED RISK | MACRO | US |
| `wolfstreet` | Wolf Street | `https://wolfstreet.com/feed/` | WOLF STREET | MACRO | US |
| `bellingcat` | Bellingcat | `https://www.bellingcat.com/feed/` | BELLINGCAT | GEOPOLITICS | GLOBAL |
| `arstechnica` | Ars Technica | `https://feeds.arstechnica.com/arstechnica/index` | ARS TECHNICA | TECH | GLOBAL |
| `theverge` | The Verge | `https://www.theverge.com/rss/index.xml` | THE VERGE | TECH | GLOBAL |
| `mit-tech` | MIT Tech Review | `https://www.technologyreview.com/feed/` | MIT TECH REVIEW | TECH | GLOBAL |
| `carbon-brief` | Carbon Brief | `https://www.carbonbrief.org/feed/` | CARBON BRIEF | ENERGY | GLOBAL |

⚠ ZeroHedge carries a **caution** marker — sensationalist editorial, frequent headline rewriting. It belongs
at tier 3 for that reason, and the marker should be visible in the row, not just in the catalogue.

### Tier 4 — blogs and aggregators (3)

| id | Name | URL | Publisher | Category | Region |
|---|---|---|---|---|---|
| `hackernews` | Hacker News | `https://hnrss.org/frontpage` | HACKER NEWS | TECH | GLOBAL |
| `abnormal-returns` | Abnormal Returns | `https://abnormalreturns.com/feed/` | ABNORMAL RETURNS | MARKETS | US |
| `marginal-rev` | Marginal Revolution | `https://marginalrevolution.com/feed` | MARGINAL REVOLUTION | MACRO | GLOBAL |

### Totals

| | Count |
|---|---|
| Tier 1 / 2 / 3 / 4 | 5 / 37 / 8 / 3 = **53** |
| By category | MARKETS 18, GEOPOLITICS 12, TECH 8, MACRO 5, CRYPTO 4, REGULATORY 4, ENERGY 2 |
| By region | GLOBAL 29, US 12, INDIA 4, ASIA 3, EU 3, UK 1, MENA 1 |
| Distinct publishers | 49 (CNBC ×3, BBC ×2, WSJ ×2) |
| Distinct hosts | ~45 — this is the number that decides the guard policy count |

## Endpoints to avoid

These were tried and do not work. Each one costs a request and a warning on every cycle if it is added
blind, so they are recorded here rather than left to be rediscovered.

| Endpoint | Symptom | Why |
|---|---|---|
| `feeds.reuters.com` | dead | Reuters discontinued public RSS in 2020; no replacement endpoint is published |
| AP Top News via `rsshub.app` | HTTP 403 | The public RSSHub instance answers every client with a Cloudflare HTML page, never an article |
| IMF News | HTML, not RSS | The endpoint serves an Akamai access-denied page |
| `defenseone.com/rss/` | HTTP 404 | Feed discontinued |
| `defensenews.com/rss/` | HTTP 404 | Feed discontinued |
| Kitco | no endpoint | No longer publishes RSS |

The three RSSHub-class and CDN-blocked endpoints are the important ones: they fail in a way that looks like a
feed with zero items rather than an error, so a naive client will store nothing and keep asking. The probe
in the next section exists to catch exactly this.

## Provisioning rules

The rules that make a user-added feed safe to keep. They apply to the built-in catalogue too, but user
feeds are where they matter most, because a user can point at anything.

1. **Probe before trust.** On add, fetch once and classify the payload: RSS 2.0, Atom, RSS 1.0/RDF, HTML,
   or not-a-feed. Report item count, newest-item age and payload size.
2. **Reject a stale feed.** If the newest item is more than seven days old, do not enable the feed — report
   the age and let the user decide. This is the single check that catches a "working" endpoint that serves
   a frozen archive.
3. **Reject a non-feed.** An HTML page that returns 200 is not a feed. Classification must be on the root
   element, not on the content type.
4. **No backfill.** A new feed starts empty and fills from its next cycle. Do not attempt to reconstruct
   history from a feed that does not offer a cursor.
5. **Conditional requests.** Store `ETag` / `Last-Modified` and send `If-None-Match` / `If-Modified-Since`;
   a 304 means the feed is alive and unchanged, which is cheap proof of liveness.
6. **One host, one policy.** Feeds are grouped by host for pacing, and a host that refuses is paused rather
   than retried.
7. **Never fetch a feed we cannot identify.** The User-Agent is `terminal-rs/<version> (personal research;
   <group>)`, matching the existing `UserAgent::Research` identity.

## Guard integration

The existing guard (`crates/net/src/lib.rs:68-101`) already models a host policy with `min_interval`,
`per_minute`, `daily_cap`, `concurrency: 1` and a `cache_ttl`. The catalogue needs three changes to it.

**Switch granularity becomes the group, not the vendor.** `Settings` currently has one boolean per vendor
(`finnhub`, `sec`, `yahoo_news`, `coindesk`, `house_disclosures`, `house_roster`, `bargo`,
`bargo_keyless`). Fifty-three feeds need nine groups instead:

| Group | Feeds | Suggested `min_interval` | `daily_cap` |
|---|---|---|---|
| `regulators` | 4 | 300 s | 400 |
| `global-official` | 1 | 300 s | 300 |
| `financial-media` | 14 | 900 s | 600 |
| `global-media` | 11 | 900 s | 600 |
| `asia-india` | 6 | 900 s | 300 |
| `crypto` | 5 | 900 s | 400 |
| `tech` | 6 | 3600 s | 400 |
| `macro-blogs` | 4 | 3600 s | 300 |
| `energy` | 2 | 3600 s | 200 |

Group membership, by feed id:

| Group | Members |
|---|---|
| `regulators` | `sec-press`, `fed-press`, `ecb-press`, `boe-news` |
| `global-official` | `un-news` |
| `financial-media` | `bloomberg-mkts`, `wsj-markets`, `wsj-world`, `marketwatch`, `cnbc-finance`, `cnbc-world`, `cnbc-tech`, `seekingalpha`, `investing-news`, `economist`, `fxstreet`, `mining-com`, `benzinga`, `abnormal-returns` |
| `global-media` | `bbc-world`, `bbc-business`, `aljazeera`, `nyt-world`, `guardian-world`, `france24`, `foreignpolicy`, `middle-east-eye`, `scmp`, `dw-world`, `bellingcat` |
| `asia-india` | `nikkei-asia`, `hindu-biz`, `livemint`, `et-markets`, `moneycontrol`, `channel-news-asia` |
| `crypto` | `coindesk`, `cointelegraph`, `theblock`, `decrypt`, `finextra` |
| `tech` | `techcrunch`, `wired`, `arstechnica`, `theverge`, `mit-tech`, `hackernews` |
| `macro-blogs` | `zero-hedge`, `calculated-risk`, `wolfstreet`, `marginal-rev` |
| `energy` | `oilprice`, `carbon-brief` |

The Settings Data sources tab then shows nine rows with a switch, requests-today against cap, and pause
state — which is what the tab was built for.

**Traffic arithmetic.** One request per feed per cycle at the suggested cadence is about **5,250 requests
per day** across ~45 hosts, i.e. roughly 115 requests per host per day. That is a small absolute number but
it is only acceptable because it is spread across hosts. The number that would break it is refreshing all
53 on a single short cycle, which is what a naive "fetch everything every 5 minutes" implementation does.

**Refresh cadence follows tier.** Tier 1 every 5 minutes, tier 2 every 15, tier 3 hourly, tier 4 hourly.
Tier 1 is the wire and the regulators, where being late is the whole cost.

**A dead feed must be visible, not silent.** `last_item_at` per feed is what makes "this feed stopped two
days ago" answerable. The pane status line should show the active feed count and the oldest active feed's
item age — item **D3** in the backlog.

## Rollout

Ordered so each step is useful on its own and the risky parts come first.

1. **Vocabulary and registry shape only.** Add the `Feed` struct, the category and region vocabularies, and
   the tier field; persist the feed's tier and category on ingest. No new hosts, no new traffic.
2. **Tier 1 seeds.** Add the five regulators/wires with group policies, probes and the `last_item_at`
   tracking. Five hosts, 1,440 requests/day, and the probe and pause machinery gets exercised for real.
3. **Settings rows and source filter.** Surface the groups as switchable rows with usage and pause state;
   let the pane's existing source filter (`Filters.sources`) drive off the registry rather than off the
   distinct strings it finds in the store.
4. **Tier 2 expansion.** Twelve financial-media feeds first, since MARKETS is the largest category and the
   weakest part of the current coverage.
5. **Cluster lead selection by tier.** Now that tier is stored per article, use it to pick the lead of a
   clustered story and to badge the row. This is where the tier stops being metadata and starts being
   signal.
6. **User-added feeds**, with the probe, the staleness rejection and the same group policies.
7. **Tier 3 and tier 4**, at hourly cadence.

## Open questions

- **Language.** Every feed in this catalogue is English-language. A `lang` field on the story, a language
  filter and translation are all out of scope today. If multi-language coverage is wanted, the catalogue
  needs non-English feeds and the story model needs a `lang` column before that is a filter and not a
  guess.
- **Licence and redistribution.** These are public RSS endpoints, but public is not the same as
  redistributable. The existing note in `docs/analysis/agentic-charts-plan.md` about vendor headlines and
  LLM passthrough applies here, and it gets sharper when an agent reads headlines through `read_news`.
  Per-feed licence metadata on the registry row would let the agent and the UI say what may be quoted.
- **Category versus tag.** The feed category is a desk label; the headline tags are the finer axis. Whether
  the pane should filter on the category (feed) or the tags (story) — or both — is a UI decision that
  should be made with the filter popover redesign in item **C3** of the backlog.
- **Attribution.** If a story appears in the pane, the publisher should be visible in the row. The row
  already shows the publisher; the question is whether a row may ever be summarised or quoted in the digest
  without it.
