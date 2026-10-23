# A0: app-server client foundation and offline spike

Implemented against the committed `codex-cli 0.163.0` experimental schema.
No Codex, sandbox-exec, login, or network command was run. No git command was run;
no commit, index, or branch operation was performed. Live go/no-go is still pending.

## Layout and public API

`crates/agent` is workspace package `tr-agent`, also available as a workspace
path dependency. The app does not depend on it yet.

| File | Responsibility |
|---|---|
| `src/lib.rs` | Backend-neutral `AgentBackend`, `AgentEvents`, `BackendState`, `AgentEvent`; public re-exports |
| `src/transport.rs` | Awaitable allocated request ids, notification subscriptions, concurrent server-request handlers, tripwire, terminal-state propagation |
| `src/codex.rs` | `CodexAppServer`, `LaunchOptions`, account/login, models, threads, turns, steering, interruption, event translation |
| `src/isolation.rs` | Private home/config, environment allowlist, argv/profile generation, version range, process-group supervision and stdio bridge |
| `src/transcript.rs` | Private raw JSONL recorder with flush barrier; JSONL/value/text/stream scrubbers |
| `src/schema.rs` | Committed-schema method, property, variant, and enum contracts (test-only) |
| `examples/codex_spike.rs` | Interactive headless spike, stub tools, generated PNG, canary, interrupt timing |
| `examples/scrub_transcript.rs` | Offline raw-to-scrubbed transcript conversion |

`AgentBackend` exposes async `detect`, `list_models`, `start_thread(ThreadOptions)`,
`send_turn(TurnInput)`, `interrupt`, and `events`. Its associated event stream
implements `AgentEvents`, so another backend need not construct a Codex stream.
The futures use Rust's return-position `impl Future`, without an async-trait crate.
`AgentEvent` has TextDelta, ReasoningDelta, ToolStarted, ToolFinished, Usage,
RateLimits, Done, Error. Wire parameter objects retain routing ids. `AgentEvent`
and `WireEvent` implement scrubbed Debug, including nested JSON and Error strings;
raw parameter values and raw transcripts still require explicit scrubbing before logging.

Use `CodexAppServer::launch(LaunchOptions, ToolHandler, Option<Transcript>)` for
the supervised, version-gated, initialized client. Missing/wrong-version clients
remain inspectable through `detect()`; connected clients report not-logged-in,
rate-limited, ready, or sticky unsafe. `new(Transport)` plus `initialize()` permits
fake transports and explicit lifecycle control. Every fresh client reads account
state; only ChatGPT accounts can list models/start threads/send turns. The public
account state excludes email and account identifiers.

Additional Codex APIs: `read_account`, `account_state`, `login_start`, `login_wait`,
`login_cancel`, `rate_limits`, `steer`, `is_unsafe`, `shutdown`. `Login` intentionally
has no Debug implementation. `Model` preserves the catalog's default effort,
filters ultra out of supported efforts, and exposes `safe_default_effort()` for a
non-ultra selection. `ReasoningOption::costly` marks max. Sending ultra is rejected.
All threads/turns explicitly use the default service tier.

### Bounds and shutdown

- Output queue: 128 packets; overflow terminates the connection and pending calls.
- Each notification subscriber has a bounded 128-event queue. At capacity,
  adjacent same-item TextDelta/ReasoningDelta events coalesce up to 64 KiB per
  merged delta. Other overflow evicts the oldest event and increments that
  subscriber's `dropped_events()` counter (also exposed on `Events`). UI lag never
  kills a healthy session. No subscriber means events are not retained. Security
  inspection always runs in the reader before delivery, regardless of subscribers.
- Tool concurrency: 16; excess calls fail closed. Handler deadline: 30 seconds.
  Interruption cancels pending handler futures; later UI queue adapters must honor
  cancellation by dropping their own queued work too.
- Tool results: text/image only, shape-checked. Text has a separate aggregate
  16 KiB UTF-8 budget; images have an aggregate 4 MiB decoded budget. Only exact
  `data:image/png;base64,` and `data:image/jpeg;base64,` URLs with valid canonical
  base64 are accepted. No image URL is fetched. Errors, timeouts, invocation/future
  panics, oversized or malformed results yield `success:false`; the turn continues.
  Aborting/timing out a handler also aborts its spawned future.
- Input lines: 1 MiB maximum. Requests time out after 30 seconds and remove their
  pending entry, including when the caller cancels its future.
- Transcript queue: 256 lines; overflow/write failure terminates transport rather
  than silently dropping records. Raw files are create-new, mode 0600. `flush()`
  provides an explicit persistence barrier. JSON values are recorded without
  redaction or direction envelopes; insignificant input whitespace is normalized.
- Standard-library child processes use dedicated bounded stdio bridges. Writes
  acknowledge actual child-stdin flush before a tripwire kills the process group.
  Child status and descendants are polled with a 10 ms sleep. Drop, shutdown,
  EOF, read/write errors and child exit kill the group, enumerate descendants with
  `/bin/ps -A -o pid=,ppid=,pgid=`, and kill tracked descendants even if they changed
  groups. Enumeration failures and survivors are Error findings. Home/config is
  rechecked after shutdown and again after child reaping. No helper shell or
  external kill command is used; `kill(2)` performs termination. Probes have a
  five-second deadline and cancellation cleanup too.

The first attempt to use Tokio process support required an uncached errno
package. The final implementation uses only Tokio io-util, existing workspace
crates, and the standard library; no third-party dependency was added.

## Plan versus schema

No contradictory method name or allowed item spelling was found. All six allowed
item variants exist exactly as written in the plan. The following shorthand in the
plan needed schema-specific interpretation:

1. `dynamicTools` entries must include `type: "function"` in addition to name,
   description and inputSchema. Bare function specifications are incomplete.
2. `additionalContext` is not an arbitrary chart-context object. Each opaque source
   key maps to `{kind, value}`, with value a string. This client wraps chart
   fragments as `{kind: "untrusted", value: "..."}`.
3. Declining approvals is not one universal response. Command/file requests use
   `{decision: "decline"}`. Permission requests use `{permissions: {}, scope:
   "turn"}`. Legacy approvals use `{decision: "denied"}`. All are violations too.
4. `initialized` has only a method property in its notification variant; the client
   does not attach an invented empty params field.
5. The tool-output union also permits inputAudio. The implementation deliberately
   restricts it to the plan's inputText/inputImage subset.
6. Usage availability has `ordinaryUsageAllowed`, which takes precedence over
   percentage-based estimates. Percentages are used conservatively only when that
   authoritative field is absent/null.

The task explicitly overrides the plan's throwaway-A0/A2-crate-creation wording:
this is reusable infrastructure now, not a throwaway binary. That is a scope
change, not a protocol disagreement.

## A0-FIX security design

### Inbound default-deny and approvals

The pinned schema defines the method universe. `ALLOWED_NOTIFICATIONS` is an exact,
reviewed subset, schema-tested: agent/reasoning/plan deltas; item started/completed;
thread and turn lifecycle; token usage; account/login/rate limits; errors/warnings;
and server-request resolution. There is no permissive prefix or unknown-method
fallback. In particular exec/process, file-change, MCP/server, hook, auto-review,
realtime and unknown methods trip immediately. `item/updated` is not in the pinned
schema and is therefore also a violation, even for an otherwise allowed item type.
All inbound objects, including responses and nested thread history, are inspected
for `item`/`items` payloads. Missing/malformed types and anything outside the six
allowed item types trip. Malformed envelopes also fail closed. A violation attempts
an interrupt, kills, marks sticky unsafe, and emits an Error naming the method.
Security reply/interrupt write failure does not replace the finding with a generic
queue/write error. Notification delivery is downstream of these checks.

Only schema-known methods with an id are classified as server requests. A method
plus `id:null` does not bypass notification validation. The sole accepted server
request is `item/tool/call`. Other requests get a decline/error and trip; unknown
methods with ids are still forbidden notifications (with a courtesy error reply).
`account/chatgptAuthTokens/refresh` belongs to external-host token auth, not our
Codex-managed ChatGPT OAuth flow, so it is not enabled. Approval-shaped messages
are rejected even without an id or with an unknown method spelling.

Both `thread/start` and `turn/start` pin `approvalPolicy: "untrusted"` and
`approvalsReviewer: "user"`. `untrusted` is the ask-before-untrusted/side-effecting
command policy, not `never` (which never asks) or model-discretionary `on-request`.
The schema's exact reviewer description is:

> Configures who approval requests are routed to for review. Examples include sandbox escapes, blocked network access, MCP approval prompts, and ARC escalations. Defaults to `user`. `auto_review` uses a carefully prompted subagent to gather relevant context and apply a risk-based decision framework before approving or denying the request. The legacy value `guardian_subagent` is accepted for compatibility.

**Schema limitation:** `AskForApproval` lists `untrusted`, `on-request`, `never`
and a granular object, but provides **no description text for those policy
values**. No missing description has been fabricated. The `TurnStartParams`
property says "Override the approval policy for this turn and subsequent turns."
Behavioral confirmation of `untrusted` is a mandatory live check; disabled shell
features, read-only sandboxing, declined approvals and tripwires remain independent
controls. We never rely on an approval policy as an auto-execution permission.

### Feature registry and exact launch argv

Final feature allowlist (adding any entry requires review):
`item_ids`, `steer`, `instant_interrupt`, `content_item_kinds`, `sqlite`,
`enable_request_compression`. (`secret_auth_storage` was dropped after the live run;
see "Live go/no-go".)

The committed `schema/codex-0.163.0-features.txt` has 175 rows. `feature_argv()`
walks names in lexical order and emits a separate `--disable NAME` pair,
including default-off/removed entries, for every name outside these six, which
retain their registry defaults. Thus all `in_app_*`, fast/ultrafast,
collaboration, goals, snapshots, guardian, browser, plugin, workspace, API-key,
MCP elicitation/search, retries and proxy fallback features are explicitly off.
There is never an `--analytics-default-enabled` argument.

Exact argv construction (the registry expansion above is literal, not a sample
subset; `app_server_argv` is regression-tested against every committed row):

```text
/usr/bin/sandbox-exec -p PROFILE CANONICAL_CODEX --strict-config
    [for each registry name in lexical order:
        --disable NAME if not allowlisted;
        --enable NAME if allowlisted and default-off]
    app-server
```

Before the server, that **same argv prefix**, scrubbed environment, private home,
working directory and session-specific Seatbelt profile run `--version` and then
`features list` instead of `app-server`. The binary is canonicalized once, and
that exact path is used for all three invocations. Version must be exactly
0.163.0 even if the caller supplies a wider `VersionRange`. The feature parser
requires the full registry, valid stages/booleans, no duplicates, no unapproved
enabled features, and all seven approved features enabled. A malformed, partial,
changed or unsafe listing prevents startup.

### Home, install read grant, network, cleanup and diagnostics

`PrivateHome::open` and startup/shutdown rechecks require our unchanged 0600
config, an owned 0700 home/empty cwd, no symlinks, and no unknown entries. Root
directories allowed: `empty`, `sessions`, `log`, `logs`. Root files allowed:
`config.toml`, `auth.json`, `version.json`, `models_cache.json`,
`.codex-global-state.json`, `state_5.sqlite` and its `-shm`/`-wal` companions,
`logs_2.sqlite` and its `-shm`/`-wal` companions. Below storage directories only
2-/4-digit date directories, `rollout-*.jsonl`, `codex-tui.log`,
`codex-app-server.log`, and `codex.log` are allowed. This is intentionally narrow:
AGENTS/override, rules, skills, hooks, prompts, plugins, MCP configuration and
other unknown entries refuse startup. Findings omit attacker-controlled filenames
and explain the rejected entry class. Unknown legitimate files require review,
not automatic learning of an expanded allowlist.

Only the canonical binary's containing directory receives the install read grant;
`/`, `/Users`, home or any ancestor of home/private-home/.ssh/.codex/Documents
is refused. Relative/control/parent-traversal paths are rejected. Normal sessions
permit outbound TCP 443 only, with no bind/inbound grant. Separate `SessionMode::Login`
sessions add only loopback bind/inbound at the selected callback port and cannot
start threads/turns. Normal sessions reject login calls. The example's `--login`
mode exits after login; restart without it for agent work. The policy callback
port is configurable (`--callback-port`, or `SessionMode::Login`), default 1455.
The login schema/response exposes authUrl/loginId, not a dedicated callback port.

Named Mach lookups are individually allowed for DNS/Keychain/trust only. There is
no broad Mach or network allowance. Process-group kill is supplemented with
tracked descendant enumeration and direct PID kills; the tree computation is
covered by a fake ps table, including escaped groups. Home/process cleanup
findings are included in the terminal Error and rechecked by the reaping thread;
late findings also update the terminal state. Scrubbers redact Basic/Bearer auth,
cookie/session/set-cookie values of any length, password/secret/api-key fields,
and nested JSON; Debug uses those scrubbers instead of raw params.

## UNVERIFIED: manager live checks

No Codex, sandbox-exec, login, network command, or live process-cleanup experiment
was run. These items are **UNVERIFIED**, and a failure must stop the live run
rather than cause an exemption to be added:

- Actual CLI acceptance/semantics of `--strict-config`, `--disable`, `--enable`,
  including registry entries marked removed, and the exact features-list format.
  Startup verification now enforces the committed registry rather than trusting it.
- Approval `untrusted` behavioral semantics (the schema lacks policy descriptions),
  user routing, and ask/decline/trip behavior for every side-effecting attempt.
- (Resolved live: the Keychain is not reachable from the sandbox; see "Live go/no-go".)
  `cli_auth_credentials_store = "keyring"` and `[features] secret_auth_storage = true`
  actually using Keychain without credential-file fallback. The protocol schema
  establishes managed OAuth persistence but not filesystem names or config keys.
  `auth.json` is allowlisted as a possible credential/metadata file, not proof
  that token fallback is acceptable; inspect locally without logging credentials.
- Exact home artifacts: sessions/logs are implied by protocol concepts, sqlite by
  the registry/config schema, but exact database suffixes, cache/global-state
  filenames and dated rollout layout are not specified by the app-server schema.
  These narrow candidate names need installed-code/live confirmation. Unknown
  artifacts fail closed before reuse and are reported after each session.
- Seatbelt profile compilation, system/dyld resources, native binary installation,
  and the minimal lookup set. **Each lookup is individually UNVERIFIED**:
  `com.apple.dnssd.service` (mDNSResponder/DNS), `com.apple.SecurityServer`,
  `com.apple.securityd`, `com.apple.trustd`, `com.apple.trustd.agent`
  (Keychain/trust). Confirm DNS/TLS without a broad lookup/network exemption.
- Callback default 1455, IPv4/IPv6 loopback interpretation, conflicts, and whether
  the CLI supports selecting the same port. The configurable policy port does
  not claim to configure Codex's listener; if the actual listener differs the
  sandbox denies it. Do not widen to arbitrary loopback ports.
- `/bin/ps` visibility/format, descendant reparenting/setsid races, PID reuse
  between a table snapshot and kill, and zombie/survivor reporting. Tracking
  includes previously observed escaped descendants but cannot prove visibility
  of a child that forks/reparents entirely between snapshots. No claim of a
  race-free OS process containment primitive is made.
- Canonical executable/installation ownership and replacement races. The install
  directory must contain a native executable and must not overlap protected
  ancestors. Node/shell interpreter launchers remain denied. Private home must
  be outside Documents/.ssh/.codex; do not place it in this repository for live use.
- Only intended tools usable, generated PNG and text delivered, token/rate-limit
  events received, canary passed, and interrupted completion in under one second.
  Confirm shell disabling prevents nested sandbox attempts. Current evidence is
  offline/fake only, not a successful live transcript.
- Scrubbing is defense-in-depth, not proof that arbitrary free text is anonymous.
  Manually review scrubbed fixtures. Login URL display is intentional; never log it.

Automatic reconnect/backoff, resume/history, UI cancellation adapters and log
rotation remain later integration work. No live certification is fabricated.

## Reviewer regression outcomes

All 11 `BUG:` ignores were removed; all 26 reviewer tests are active and pass.
No reviewer assertion was weakened to conceal a bug.

| Reviewer test (`tests/adversarial.rs`) | Outcome |
|---|---|
| `item_type_casing_missing_and_malformed_items_trip` | PASS, retained; all casing/missing-type variants deny |
| `forbidden_item_completed_without_started_trips` | FIXED; all item carriers inspected |
| `forbidden_activity_notifications_trip` | FIXED; exact notification allowlist |
| `item_started_with_id_still_trips` | FIXED; schema-based classification, including null ids |
| `approval_variants_are_declined_and_violate` | PASS, retained; legacy/future approval shapes deny |
| `mcp_elicitation_and_user_input_requests_are_violations` | FIXED; error plus tripwire |
| `unknown_server_requests_get_error_reply_and_do_not_hang` | EXPECTATION CORRECTED; now error/interrupt/sticky unsafe instead of continued usability, required by rules 2/3/10. Separate fake session per method. External-host auth refresh is not needed by managed OAuth. |
| `unknown_and_duplicate_response_ids_are_ignored` | PASS, retained |
| `oversized_line_without_newline_fails_closed` | PASS, retained |
| `invalid_utf8_and_partial_lines` | PASS, retained |
| `event_overflow_never_masks_a_later_tripwire` | PASS, retained; security stays reader-side |
| `shutdown_and_drop_kill_and_fail_pending` | PASS, retained |
| `tool_reply_keyed_by_request_id_not_call_id_and_bad_shapes_fail` | PASS, retained |
| `panicking_tool_handler_replies_failure` | FIXED; panic maps to success:false |
| `tool_image_must_be_data_url` | FIXED; strict MIME prefix/base64 validation |
| `realistic_chart_png_is_deliverable` | FIXED; independent decoded-image budget |
| `thread_start_pins_approval_policy_and_user_reviewer` | FIXED; thread and turn pins also unit/schema-tested |
| `sandbox_install_dir_cannot_cover_home_or_root` | FIXED; protected ancestor check |
| `sandbox_profile_escapes_hostile_paths` | PASS, retained |
| `environment_drops_secrets_proxies_and_loader_vars` | PASS, retained |
| `version_gate_rejects_prerelease_and_suffixes` | PASS, retained |
| `private_home_rejects_symlinks_group_writable_and_existing_dirs` | PASS, retained |
| `private_home_open_rejects_planted_instruction_and_rule_files` | FIXED; home allowlist/rechecks |
| `scrubber_covers_protocol_secrets` | PASS, retained |
| `stream_scrubber_handles_jwt_and_url_split_across_deltas` | PASS, retained |
| `scrubber_redacts_basic_auth_and_cookies` | FIXED; Basic auth and short sensitive fields |

Two older **unit** fixtures also needed rule-driven corrections: the former
kill-on-event-overflow assertion now checks coalescing, counted drops, health and
a later tripwire (additional E); the image round-trip fixture now uses valid
`AA==` rather than invalid one-character base64 `a` (rule 9). The old sandbox
profile test now requires no normal-session listener and a port-specific login
listener (additional C). Reviewer BUG-case expectations otherwise stayed intact.

Additional A–F coverage: registry-driven argv/parser and startup refusal rules;
shared sandboxed probe prefix/canonical path construction; separate network
profiles/session-mode guards; fake descendant trees; coalescing/drop counting
under lag with subsequent security processing; scrubbed nested Debug/errors.
New edge coverage also exercises malformed/updated/nested/response item carriers,
synchronous handler-invocation panic followed by a usable turn, independent 4 MiB
image/16 KiB text boundaries and invalid padding/MIME URLs, protected ancestors,
unknown home entries/post-session rechecks, and cleanup Error findings. Production
probes and OS cleanup are deliberately not executed by these offline tests.

## Tests

Agent tests use duplex streams, in-process state, committed fixtures or a fake
stdin writer. None launch Codex/sandbox-exec, run ps/kill experiments or use network.
Unit tests are at the ends of their files; reviewer integration tests remain in
`tests/adversarial.rs`. Foundation tests include:

- `codex::tests::handshake_order_and_chatgpt_requirement`
- `codex::tests::detect_reports_missing_wrong_version_login_quota_and_ready`
- `codex::tests::events_map_text_reasoning_tools_usage_limits_and_completion`
- `codex::tests::model_pagination_open_efforts_and_ultra_filter`
- `codex::tests::thread_turn_steer_interrupt_and_context`
- `codex::tests::login_completion_and_cancel`
- `transport::tests::routes_interleaved_responses_and_notifications`
- `transport::tests::tool_round_trip_text_and_image_uses_request_id`
- `transport::tests::tool_results_are_shape_checked_and_bounded`
- `transport::tests::approvals_declined_and_violate`
- `transport::tests::tripwire_kills_and_stays_unsafe`
- `transport::tests::interrupt_cancels_in_flight_tool_handlers`
- `transport::tests::crash_fails_pending_requests`
- `transport::tests::malformed_input_fails_pending_and_emits_error`
- `transport::tests::output_overflow_and_drop_kill_the_child`
- `transport::tests::event_overflow_coalesces_without_killing_and_counts_drops`
- `isolation::tests::environment_is_allowlisted_not_inherited`
- `isolation::tests::config_and_argv_disable_every_planned_feature`
- `isolation::tests::profile_denies_personal_secrets_and_documents`
- `isolation::tests::private_home_permissions_and_config_are_enforced`
- `isolation::tests::tripwire_flushes_child_stdin_before_group_kill`
- `isolation::tests::version_gate_rejects_unknown_and_out_of_range`
- `transcript::tests::scrubber_redacts_fixture_secrets`
- `transcript::tests::streaming_scrubber_handles_secrets_split_across_deltas`
- `transcript::tests::recorder_writes_private_jsonl_and_flushes`
- `schema::committed_schema_covers_every_wire_method_field_and_enum`
- Spike example: `tests::png_is_generated_in_process_with_valid_chunks`

### Verification

Offline flags enforce the task's no-network restriction.

| Command | Result |
|---|---|
| `cargo fmt --all` | PASS; also checked with `cargo fmt --all -- --check`; test counts N/A |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | PASS; no warnings; test counts N/A |
| `cargo test --workspace --offline` | PASS: 1,081 passed, 0 failed, 7 ignored across workspace unit/integration/doc-test targets; includes 34 agent unit + 26 reviewer tests, all active |
| `cargo test -p tr-agent --all-targets --offline` | PASS: 34 library + 26 reviewer + 1 spike-example test; 0 failed, 0 ignored |

The seven ignored workspace tests are pre-existing outside agent. All reviewer
BUG ignores were removed. An intermediate clippy run caught a type-complexity
alias, an MSRV-incompatible convenience method, and map-key iteration; all were
corrected before final verification. No lint suppression was added.

## Exact live commands for the manager

These are instructions, **not commands executed during implementation**. Start in
`/Users/bennyo/Documents/repos/terminal-rs-agent-spike`. Codex 0.163.0 must already
be installed as a native executable on PATH. Review the UNVERIFIED items first.

```sh
install -d -m 700 "$HOME/Library/Application Support/terminal-rs-agent-spike"

cargo run --offline -p tr-agent --example codex_spike -- \
  --home "$HOME/Library/Application Support/terminal-rs-agent-spike" \
  --login \
  --transcript "$HOME/Library/Application Support/terminal-rs-agent-spike/raw-login.jsonl"
```

Open the printed URL locally; Enter cancels the login. Do not redirect stdout to a
committable log during login. After login, reuse the same home without --login:

```sh
cargo run --offline -p tr-agent --example codex_spike -- \
  --home "$HOME/Library/Application Support/terminal-rs-agent-spike" \
  --transcript "$HOME/Library/Application Support/terminal-rs-agent-spike/raw-session.jsonl" \
  --canary

cargo run --offline -p tr-agent --example scrub_transcript -- \
  "$HOME/Library/Application Support/terminal-rs-agent-spike/raw-session.jsonl" \
  "$HOME/Library/Application Support/terminal-rs-agent-spike/scrubbed-session.jsonl"
```

Transcript outputs must not already exist (create-new). Review the scrubbed file
locally before copying it into the repository as a fixture. A tripwire, missing
text/image calls, failed/unexpected completion, or interrupt latency >= 1 second
makes the spike exit nonzero. Canary PASS is printed only after its completed turn
without an observed violation. A canary is not a tool-inventory API or a substitute
for validating all three isolation layers.

## Live preflight without login (2026-10-23, codex-cli 0.163.0, macOS arm64)

Run by the manager against a fresh private home under
`~/Library/Application Support/terminal-rs-agent-spike`; no account, no network turn.
Result: sandboxed version and feature probes, initialize, skills check and account
read all pass; the client stops at "ChatGPT login required" as designed. Changes
made from what the live run showed:

- **Sandbox profile.** Codex needs to read the root directory entry (`literal "/"`),
  the metadata of each ancestor of the private home (to canonicalize `CODEX_HOME`),
  `/etc/codex/requirements.toml` and `managed_config.toml` (admin policy; absent on
  this machine), and managed preferences (`user-preference-read`, `cfprefsd`
  mach services and read-only `apple.cfprefs.*` shared memory; preference reads are
  limited to the `com.openai.codex` domain), plus `opendirectoryd.libinfo`
  (user lookup) and `notification_center`. The install-directory subpath grant was
  removed: only the `codex` binary itself is readable (the directory also holds
  `codex-code-mode-host`). `diagnosticd` and `logd` stay denied.
- **Features.** Retired features (`removed` stage) always report enabled and are
  ignored by the check. `unified_exec` cannot be disabled by `--disable` or
  `-c features.unified_exec=false` in 0.163.0; it is the only named exception
  (`UNDISABLEABLE_FEATURES`), mitigated by the disabled shell tool, the exec-only-codex
  sandbox and the tripwire, and re-proved by the canary turn. Retired features that
  still report enabled are an explicit reviewed list (`RETIRED_ENABLED_FEATURES`). `codex features` rejects
  `--strict-config`, so that probe omits it.
- **Remote control.** Codex sends `remoteControl/status/changed` at startup. It is
  accepted only with status `disabled`; any other status trips.
- **Skills.** Codex installs five bundled system skills into `skills/.system` on
  every start and, with the real `HOME`, discovers every user-scope skill in the
  user's home (about 90 on this machine, including mail and computer-use skills).
  Fixes: the child gets a private, empty `HOME` (`<private home>/home`), the config
  disables each bundled skill with `[[skills.config]]`, the home validator accepts
  only a link-free `skills/.system` tree, and `initialize` calls `skills/list` and
  fails closed unless all five bundled skills are reported, all disabled, with no
  discovery errors and nothing else enabled. `skip_host_skill_discovery` had no effect.
- **Home entries.** Codex also creates `installation_id`, `goals_1`, `memories_1`
  and `queue_1` SQLite files (memories and goals stay disabled) and `tmp/arg0-macos`
  links back to its own binary; these exact shapes are accepted.
- **Diagnostics.** App-server stderr goes to `log/codex-app-server.log` (0600) and
  failed probes report a scrubbed stderr excerpt.

Still to verify at login: Keychain storage with the private `HOME`, the login
callback port, DNS and TLS under the 443-only policy. (Done below.)

## Live go/no-go with ChatGPT sign-in (2026-10-23, codex-cli 0.163.0, macOS arm64)

**Result: GO.** Signed in with ChatGPT (Pro plan) through the sandboxed app server,
then ran `codex_spike --canary` on `gpt-6.1-sol` (also tried `gpt-5.6-sol`):

| Check | Result |
|---|---|
| Sign-in through the localhost callback (port 1455) | pass |
| DNS and TLS to `auth.openai.com` and the model backend under the sandbox | pass after two profile fixes |
| Credential storage | `auth.json`, owner-only, in the private home (Keychain not usable, see below) |
| "Describe the trend": streamed text plus text and image tool replies | pass, 2 direct tool calls |
| Canary ("list your directory and run `ls ~`") | pass: refused; only user, agent and chart-tool items; no approval requests |
| Interrupt to turn completion | 0.001 s (the interrupt lands before generation; measure mid-stream in A2) |
| Private `HOME` and working directory | still empty |
| Remote control | reported `disabled` |

Scrubbed transcript: `crates/agent/tests/fixtures/codex-0.163.0-live-trend-canary.jsonl`,
guarded by `tests/live_fixture.rs` (re-scrubbing must not change it; no
side-effecting items or approval requests; tool calls only in the `chart` namespace).

Changes made from what the live run showed:

- **DNS and networking in the sandbox.** Name resolution needs the resolver socket
  (`network-outbound` to `/private/var/run/mDNSResponder`) and metadata of the `/var`
  link; Codex's HTTP stack also needs metadata (not contents) of its install
  directory. Found by bisecting with Codex's own unauthenticated device-code request
  under the same profile. Login failures now report Codex's (scrubbed) reason.
- **Credentials: file, not Keychain.** With the private `HOME` there is no default
  Keychain ("A default keychain could not be found"), and even broad Keychain access
  under the sandbox was denied authorization. Opening the user's login Keychain to
  the sandboxed process would be a much larger grant, so the config now uses
  `cli_auth_credentials_store = "file"` with `secret_auth_storage` off (it also needs
  the Keychain). Codex documents both stores; `auth.json` is treated like a password:
  the validator refuses startup unless it is a regular, non-link, owner-only (0600)
  file inside the 0700 private home, and it never enters transcripts or the repo.
- **Code Mode.** The model catalog marks every current model `tool_mode =
  "code_mode_only"`: tools are reached by model-written JavaScript run in the
  `codex-code-mode-host` process, which stays disabled. With it off the model saw no
  usable tools. Fix without running model-written code: app tools are sent as one
  `chart` namespace and the config sets `[features.code_mode] direct_only_tool_namespaces
  = ["chart"]`, which exposes them as direct function tools. Startup verification
  still requires `code_mode` and `code_mode_host` off. Tools are sent with
  `deferLoading: false` (tool search stays disabled).
- **Notifications and home entries.** `turn/moderationMetadata` (moderation verdict on
  the prompt) is allowlisted and ignored. Codex also writes `.sqlite-maintenance.lock`
  (empty) and `.sandbox_migration` (`v1`); both are accepted.
  `deprecationNotice`s come from `--disable` of retired feature names and are harmless.
- **Scrubber.** Live data showed home-directory paths, the machine name
  (`serverName`), credit balance and plan type surviving; these are now redacted
  (`/Users/[USER]/...`).
- **Sign-in UX for A2.** Take the sign-in URL from the `account/login/start` reply,
  never from terminal output: a stale link produces "State mismatch". The spike gained
  `--model <id>` for the model picker.
