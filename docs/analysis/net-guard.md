# N0: shared news/filings fetch guard

`tr_net::shared()` is used by Yahoo **news** and CoinDesk RSS, and by Settings → Data sources → Finnhub Save & Test. Quotes, bars, Congress and exchange traffic are unchanged. Finnhub news and EDGAR fetching remain later phases.

## Ledger and environment

Default ledger on this macOS account:

`/Users/bennyo/Library/Application Support/terminal-rs-net/ledger.sqlite3`

Generally: `$HOME/Library/Application Support/terminal-rs-net/ledger.sqlite3` on macOS; `$XDG_DATA_HOME/terminal-rs-net/ledger.sqlite3` elsewhere (fallback `$HOME/.local/share`). Only `TR_NET_LEDGER` overrides the production path. Supplied HOME, XDG_DATA_HOME and TR_NET_LEDGER must be nonempty absolute paths; invalid paths fail closed. `TR_DATA_DIR` never selects or resets it. A failed shared-guard initialization is retried on demand at most once per minute, with requests refused meanwhile.

An overridden `TR_DATA_DIR` is offline unless `TR_ALLOW_EXTERNAL` equals exactly `1`. The environment is captured when a guard is constructed. Tests inject that same environment lookup with repository-local temporary paths, so parallel tests never mutate process-global environment variables or open the user's ledger.

The ledger uses WAL and a five-second busy timeout. UTC-day request counts, pacing reservations and in-flight leases are checked/reserved in one SQLite IMMEDIATE transaction. The extra `inflight` and `transport_failures` tables preserve concurrency and transport-failure counting across independent guard instances/processes. `last_request.at` and lease deadlines use Unix milliseconds; cooldown deadlines use Unix seconds; `requests.day` is the UTC epoch day. Every reserved attempt consumes budget, including transport failures.

Each request is limited to 30 seconds, while its durable lease lasts 35 seconds for crash recovery. Cancellation drops only the lease owned by that request; a crashed process leaves a lease that expires. Pacing waits at most five seconds by default, or a bounded 30 seconds for Yahoo news so different queries can queue while panes show Loading. At most three redirects are followed, only to HTTPS hosts in the same policy group; every hop reserves budget and a lease. A rejected redirect names only the target host. There are no automatic request retries or provider-host fallbacks. Responses are streamed with an 8 MiB decoded-body limit. Callers' clients cannot override these guard policies.

The shared guard coalesces simultaneous identical GET requests by group and full URL. Successful 2xx bodies are cached in process for five minutes for CoinDesk and Yahoo news. SEC and Finnhub default to zero TTL, configurable with `set_cache_ttl` for later phases. Refusals/errors are delivered to current waiters but never reused as cached content. Cache hits recheck switches/identity and never access the ledger. Different URLs and groups remain independent.

A 403/429/503 pauses the entire logical group regardless of content type. Refusal/captcha body markers are checked only for HTML (text/html or a whitespace-prefixed HTML/doctype start), never explicit JSON/RSS/Atom/XML content types. Three consecutive transport failures also pause the group. Cooldown is the maximum of Retry-After and the escalating 6/12/24-hour floor, capped at seven days. Successful 2xx non-refused network responses reset strikes. Cooldown writes retry briefly under contention; if persistence fails, an in-process cooldown still prevents another attempt before the floor. Group aliases share counts, spacing, leases and cooldowns.

## Settings and privacy

Source switches, the global pause and validated SEC contact are in local `settings.json` under `data_sources`. A corrupt section disables all external sources and explains this in status, without discarding other app preferences. SEC identification requires an ASCII, header-safe contact and uses `terminal-rs/<version> (personal research; <email>)`. The per-group policy explicitly preserves Yahoo news's existing self-identifying `Mozilla/5.0 (compatible; terminal-rs)` string. New groups SEC, Finnhub and CoinDesk never use a browser-like identity. Changing switches configures the shared guard immediately. A switch does not cancel an already-started socket operation.

The Finnhub key uses `SecretStore::platform_for(data_dir, "finnhub")`: macOS Keychain, or the existing owner-only file backend when selected. Save & Test saves once and performs one guarded AAPL quote request (subject to guarded redirects), with no retry. The key is sent in a sensitive `X-Finnhub-Token` header, never in the query string. If offline/disabled/paused/budget-limited, the key remains saved and the UI explains that the test was refused. Worker replies contain only `mask(key)`, never the full stored key. Guard errors contain only closed, non-sensitive messages: no URLs, response bodies, contact email or tokens. `redact()` is available for diagnostic URLs containing token/apikey/api_key query parameters.

Status snapshots include switches, UTC-day usage/cap, last request, cooldown/reason, offline and missing SEC contact. The UI adds missing-key state. Status is requested by the worker only while the Data sources tab is rendered, at ten-second intervals; pending work schedules no repaint loop. Status refresh has its own pending flag and never toggles Save & Test's busy state. There is no background status poller.

## Verification coverage

Localhost-only fixtures cover independent instances sharing pacing/caps, atomic competing reservations, a bounded pacing wait, concurrency through body consumption, offline zero-connection refusal, unknown-host refusal, 403/429/503/captcha pauses, persistence/expiry/backoff, Retry-After, repeated transport failure, redirect refusal, body cap, identities, redaction, status and environment paths. App tests cover settings serialization, masked Finnhub file secrets/removal/permissions, source-state labels and pending-refresh repaint suppression.
