# N8: article text for Analyze, fetched safely

Status: 2026-10-31. This reviews the proposed article-fetch plan against `personal/main` and turns it into a
spec plus an agent prompt. Where this file and the proposal disagree, **this file wins**.

## Verdict

The approach is right on three counts:
- fetch in Rust when the user clicks Analyze, never when a story arrives
- the model never touches the network, and the analysis process stays without tools
- if extraction fails, fall back to the summary and say so in the footer

The proposal's claims about the code hold, with the corrections below.

| Proposal says | Code says | Consequence |
|---|---|---|
| Redirects are disabled and checked again at each hop; 30 s timeout | True (`crates/net/src/lib.rs:493-495`, hop loop near `:1300`) | Reuse the hop loop's pattern |
| Need a decompression cap | The guard reads the body with `chunk()` after gzip decoding and enforces `BODY_CAP` on the decoded bytes | Already covered if the new client reads the same way. Keep a separate 2 MB cap for publisher pages. |
| "The existing cookie jar must not be reused" | The guard client has **no** cookie store. Only `crates/data/src/yahoo.rs:30` enables one. | Build a separate client with no cookie store. Never reuse the Yahoo client. |
| "Resolve the host yourself, then connect to the validated IP" | — | Do it with a custom `reqwest::dns::Resolve` that drops any address that isn't public. reqwest connects only to the addresses the resolver returns, and the check runs again on every redirect hop. That avoids a separate resolve-then-pin step and the window in between. |
| Not mentioned | `reqwest` reads `HTTP(S)_PROXY` from the environment by default, and a proxy does its own DNS lookup, which would skip the address check | **`.no_proxy()` is required** |
| Not mentioned | The guard denies `*.senate.gov` (`lib.rs:85`), and the user keeps an avoid list | Publisher fetches go through the same deny check, plus a paywalled-host skip list (below) |
| Not mentioned | Test instances must keep guarded sources offline unless `TR_ALLOW_EXTERNAL=1` | Publisher fetches follow the same gate |
| Write it up as phase 4 of the dashboard plan | It's news work and touches different files | Its own track, **N8**, running alongside dashboard wave A |

## Scope

### 1. Fetch policy

New module `crates/net/src/publisher.rs`, with its own client.

**Client settings:**
- `redirect(Policy::none())` and `retry(never())`
- `no_proxy()`
- no cookie store and no default auth headers
- the fixed research user agent the guard already uses for news
- connect timeout 4 s, total deadline 8 s
- rustls; `https` preferred

**URL checks, run on the first URL and on every redirect hop (at most 5 hops):**
- the scheme is `http` or `https`
- no username or password in the URL
- the port is absent, 80 or 443
- the host is a domain name, not an IP literal
- the host isn't on the deny list (senate.gov) or the skip list

**Addresses the resolver rejects:**
- loopback, private (10/8, 172.16/12, 192.168/16) and link-local (169.254/16, including 169.254.169.254)
- CGNAT 100.64/10, `0.0.0.0/8`, broadcast, multicast, and the documentation and benchmark ranges
- IPv6 loopback, unspecified, ULA `fc00::/7`, link-local `fe80::/10` and multicast
- IPv4-mapped and NAT64 (`64:ff9b::/96`) addresses whose embedded IPv4 address is rejected

If every address for a host is rejected, the request is refused.

**Response checks:**
- 2xx only
- `Content-Type` is `text/html` or `application/xhtml+xml`
- the decoded body is streamed with a 2 MB cap

**Budget.** The guard ledger tracks:
- the `publisher` group: 60 a day and at most 6 a minute
- each host: 1 request per 10 s (`claim_slot("publisher:<host>")`)
- a cooldown for the host on 403, 429 or 451, honouring `Retry-After`, with no automatic retry

**Trigger.** One fetch per explicit Analyze or Re-analyze. Background prefetch is not allowed.

**robots.txt.** Fetch it once per host every 24 h, through the same client and budget, and honour `Disallow` for
our user agent and `*`. When robots.txt says no, or the request fails, use the summary only and give that
reason in the footer.

### 2. Skip list (paywalls and the avoid list)

- **Never fetch:** bloomberg.com, wsj.com, economist.com, nikkei.com / asia.nikkei.com, ft.com,
  seekingalpha.com, investing.com, theblock.co, oilprice.com, zerohedge.com, reuters.com, apnews.com
  (the user's avoid list plus the big paywalls). Analyze falls back to the summary with the footer reason
  "publisher not fetched".
- **Aggregator redirect links**, such as Finnhub `finnhub.io/api/news?id=…` and Google News:
  - follow the redirect only through the hop checks
  - never attach a provider key
  - when the link carries a provider key or token, don't fetch at all

### 3. Extractor

New module `crates/data/src/article.rs`. It uses `scraper`, which the project already depends on.

**Steps:**
1. Remove `script`, `style`, `noscript`, `nav`, `header`, `footer`, `aside`, `form`, `figure`/`figcaption` and
   `iframe`.
2. Prefer `<article>`, then `[itemprop=articleBody]`, then `<main>`. Otherwise use the block with the most
   `<p>` text.
3. Join the paragraphs and collapse whitespace.
4. Cap the text at 12,000 characters, cutting at a sentence end, and record whether it was truncated.

**Checks that the result is an article.** All must pass:
- at least 600 characters and at least 3 paragraphs
- links make up less than 0.35 of the text
- the text doesn't mainly consist of cookie, consent, subscribe or sign-in wording

If any check fails, fall back to the summary with the footer reason "no article text".

**Metadata to extract:**
- the article's own publish date, from `article:published_time`, `datePublished` in JSON-LD, or `<time datetime>`
- `isAccessibleForFree: false` in JSON-LD, which means the text is a paywall teaser: use the summary only and
  give the reason "paywalled"
- `noarchive` in the robots meta tag or `X-Robots-Tag`, which means the text is used for this analysis but not
  stored

### 4. Republished-story detection

This covers the timing problem from the user's review: a five-day-old story stamped 09:25.

If the article's publish date is more than 12 h older than the feed time:
- the Story header shows the **article** date, for example "Oct 26 · 5d old · republished"
- the prompt's `age_hours` uses the article date

### 5. Storage

New table `story_body`, added with the existing `ALTER`/`CREATE IF NOT EXISTS` migration pattern:
- **Columns:** `url_hash` PK, `url`, `fetched_at`, `published_at`, `chars`, `truncated`, `etag`, `text`, `status`
- **Retention:** 90 days, matching headlines
- **Not stored:** pages marked `noarchive`, or text that failed the article checks (the failure status is still
  recorded, so the same URL isn't fetched again for 24 h)

### 6. Prompt and footer

**Prompt input:**
- a new basis, `article`, with these limits: summary ≤100 words, key points ≤5, topics ≤8, entities ≤12,
  confidence cap 0.90
- the article text goes in its own `<ARTICLE_BODY>` block, escaped the same way as `STORY_DATA` (`<` and `>`
  become `<` and `>`), and is labelled as untrusted data
- the prompt still says: do not fetch, do not use tools

**Footer wording:**
- "Based on article text · 4,120 chars · mimo-v2.6-flash · now"
- "Based on headline + 1 summary · article not fetched: <reason>"

**Settings** gets a toggle, "Fetch article text when analyzing", on by default, with the help line "One
request to the publisher per Analyze click."

## Acceptance criteria

1. **Unit tests for the resolver filter**, covering every rejected range, including IPv4-mapped and NAT64
   addresses.
2. **Redirect to a private address is refused.** Test with a local fixture server: a public-looking hostname
   that the test resolver maps to a public test address redirects to `127.0.0.1` and then `169.254.169.254`,
   and both are refused. No socket to a private address is ever opened. Assert this through the resolver
   hook.
3. **URL check tests:** a URL with userinfo, port 8080, an IP literal, a `file:`/`data:`/`ftp:` scheme, a
   senate.gov host, or a skip-list host is refused before any I/O.
4. **Response check tests:** a body over the cap, a non-HTML `Content-Type`, a 3xx with a missing or relative
   `Location`, and a 6th redirect hop are all refused.
5. **Proxy test:** with `HTTPS_PROXY` set in the test, the publisher client still connects directly (`no_proxy`).
6. **Extractor fixtures** (synthetic HTML only, never copied article text):
   - an article tag, a main tag, and a densest block
   - a cookie wall, a paywall JSON-LD teaser and a link farm (all rejected)
   - a 12k truncation, a JSON-LD date, and a meta-tag date
7. **Budget tests:** a second fetch to the same host within 10 s is deferred, the daily cap is enforced, and a
   429 response sets a cooldown.
8. **Prompt tests:** the `article` basis limits apply, `ARTICLE_BODY` is escaped, and the footer shows each
   fallback reason.
9. **No fetching outside an Analyze click:** nothing fetches on story arrival, when a story is selected, or in
   a test instance without `TR_ALLOW_EXTERNAL=1`.
10. **Full checks pass:** fmt, clippy `-D warnings`, the full workspace tests, and `--offline --locked`.

---

## Agent prompt

Paste the block below into the builder agent as written. It assumes a worktree on a new branch `codex/news-n8`
from `personal/main`, after N7d merges.

```text
TASK N8: article text for Analyze, fetched safely.
Work ONLY in the worktree you were started in (branch codex/news-n8). Do not open files outside it; never open ~/.mimo, ~/.config, ~/.local, ~/.codex, ~/.claude. Do not commit, push, or change git state. NO NETWORK: every test uses local fixture servers or a test resolver; never contact a real publisher. Never build --release; never run the app.
Spec: docs/analysis/article-fetch-plan.md (this file wins over any other doc). Read it fully first, then read crates/net/src/lib.rs (Guard, policies(), the redirect hop loop, BODY_CAP, claim_slot/cooldowns, the senate.gov deny), crates/data/src/rss.rs (scraper usage), crates/data/src/news_store.rs (migrations), app/src/app/analysis_v2.rs (prompt(), Input basis, caps, footer basis text), app/src/app/story_view.rs (Analyze flow, footer, header date), and the settings struct for data sources.
PHASING: this branch starts before N7d (analysis v2) is merged, so app/src/app/analysis_v2.rs may not exist yet. Do steps 1-3 and their tests now (crates/net, crates/data, migrations) and expose a clean API for step 4: e.g. `publisher::fetch_article(url) -> Result<Fetched, Reason>` and `article::extract(html, url) -> Result<Article, Reason>`. If analysis_v2.rs is absent, stop after step 3 and say so in the report; step 4 is done after personal/main is merged into this branch.
Build, in this order:
1. crates/net/src/publisher.rs: separate reqwest client (no redirects, no retry, no_proxy, no cookie store, 4 s connect / 8 s total), custom dns::Resolve that drops non-public addresses (full list in spec §1), URL validation on every hop (≤5), content-type and 2 MB decoded-body caps, deny list + skip list (§2), publisher budget via the guard ledger (group daily 60, 6/min, per-host 10 s slot, cooldown on 403/429/451 honouring Retry-After), robots.txt check cached 24 h, TR_ALLOW_EXTERNAL gate identical to the other guarded sources.
2. crates/data/src/article.rs: extractor + looks-like-an-article checks + metadata (published date, isAccessibleForFree, noarchive), per spec §3.
3. story_body table + 90-day retention + 24 h negative cache (§5).
4. Analyze path: on Analyze/Re-analyze only, fetch (off the UI thread, through the existing worker), then build the prompt with basis "article" and an escaped <ARTICLE_BODY> block, or fall back with a reason. Footer and header wording and republished detection per §4 and §6. Settings toggle "Fetch article text when analyzing" (default on).
5. Every acceptance criterion in the spec as a test. Fixtures are synthetic HTML only.
Rules: no new dependencies except what reqwest/tokio/std already provide (ask in the report if you believe one is unavoidable). Keep secrets out of logs and errors; never attach provider keys to publisher requests. Match surrounding code style and comment density.
Verify: cargo fmt --all; cargo clippy --offline --locked --workspace --all-targets -- -D warnings; TR_CONGRESS_REFRESH_MIN=0 cargo test --offline --locked --workspace.
REPORT: per section outcome, files changed, each acceptance criterion → test name, any spec deviations with reasons, three cargo results.
```

The builder's work then gets an independent security review focused on SSRF, the redirect chain, the proxy
and resolver path, and prompt injection through the article text. After a fix pass, the user tests Analyze on
an open article, a paywalled one, and a republished one.
