# A2a: headless bridge

The bridge carries the entire ReadOnly registry through the single direct `chart`
namespace. No pane, login UI, new third-party dependency, or relaxed isolation is
included. Implementation and tests are offline; the manager must run the live
checks below. The committed A0 fixture and schema snapshot are unchanged.

## API for A2b

- On the UI thread, obtain `AppExecutor::new(&app)` and
  `AppExecutor::pin(&app)`. The executor owns only cloned queue/worker senders;
  moving it to a background task does not move or lock the app.
- `AppExecutor::start(connector, BridgeOptions::new(private_cwd)).await` returns
  `tr_agent::bridge::AgentBridge` using `ToolSet::ReadOnly`.
- Production connector: `IsolatedConnector { launch: LaunchOptions, transcript }`.
  It uses the same `spawn_session` path as the proven spike on **every** connection.
  `Connector` is sealed: only `IsolatedConnector` and in-module `cfg(test)` duplex
  fakes can implement it. Neither public constructor accepts an unchecked launcher.
  The bridge installs its shared safety observer before initializing a connection.
- `bridge.send(BridgeTurn { input: TurnInput, pin, requires_tools,
  screenshot_consent }).await`; only one turn may run at a time. The host declares
  `requires_tools`, rather than guessing from natural language. The pin becomes
  untrusted `additionalContext.chart` JSON with uid, symbol, timeframe and range.
- Drain `bridge.next_event().await`: `Wire` (text/reasoning/tool/usage/rate-limit
  events with their original routing parameters), `Completed`, `ToolsUnavailable`,
  or `Recovered(Resumed | HistoryLost { reason })`. Treat streamed text as
  provisional until completion. Raw values require the existing scrubbers before
  logging; Debug of wire events is already scrubbed.
- `bridge.interrupt().await` cancels queued jobs and starts a monotonic clock before
  sending `turn/interrupt`. `Completed.interrupt_elapsed` is present only upon a
  matching `turn/completed` with status `interrupted`, not merely the RPC reply.
- `bridge.client()` exposes existing model/account/rate-limit APIs; `thread_id()`,
  `recover()` and `shutdown()` support lifecycle management. A2b should select
  between its control channel and `next_event` in one background owner task.

The transport-independent boundary is `ToolExecutor::execute(Arc<Registry>,
ToolCall)` plus cancellation. `HostExecutor` runs existing DataHost/LiveHost
implementations directly; the app executor runs live jobs through AgentToolQueue
and data requests through `Request::Agent` oneshots. Loaded pinned bars are read
in `App::logic`, not replaced with canned or independently fetched data. Worker
history retains source identity/session; price basis remains explicitly unknown
where the provider does not expose it. UI jobs keep the A1a 10-second deadline and
8 jobs / 4 ms drain; data requests have a 30-second deadline. Dropped or timed-out
executor futures remove their queued job, even without a UI drain.

## Readiness and first-turn proof

Before starting a real bridge, the existing private-home, sandbox, environment,
version, full feature-registry, skills and ChatGPT-account checks still apply.
Readiness additionally checks the actual private config for:

```toml
[features.code_mode]
direct_only_tool_namespaces = ["chart"]
```

Only `codex-cli 0.163.0` passes the pinned VersionRange. Both `code_mode` and
`code_mode_host` must be off; the existing full-registry verifier enforces this,
not a two-feature substitute. Function names, descriptions and input schemas are
copied unchanged from ToolSpec; the existing client sets `deferLoading: false`.
There is no protocol tool-inventory endpoint: static readiness is provisional,
then direct calls supply the runtime proof.

Exact schema-backed signals (no invented notification names):

| Notification | Fields inspected |
|---|---|
| `warning` | `message`, optional `threadId` |
| `configWarning` | `summary`, optional `details` |
| `error` | `error.message`, `threadId`, `turnId` |
| `turn/completed` | `turn.error.message`, `turn.status`, `turn.id` |

Case-insensitive message matching recognizes tool-router diagnostics (`tool router`
or `tool_router`) and turn errors naming tools (`error.message` or
`turn.error.message` containing `tool`). Other warnings, including the disabled
Code Mode advisory, are not tools-unavailable signals. These are text diagnostics,
not schema enums. Errors/warnings targeted at another thread/turn are not applied.
A completed tool-requiring turn without any enabled `chart` `item/tool/call`
becomes `ToolsUnavailable`, even if it produced fluent agent text. This guard runs
on subsequent turns too, including after reconnect. Conversational turns with
`requires_tools = false` can complete normally with no calls.

**Fixture-specific distinction:** A0's successful trend turn itself emits this
exact advisory before both direct calls:

> Code Mode is unavailable because code-mode host is disabled. Code mode will fail closed; enable `features.code_mode_host` and install `codex-code-mode-host`.

That advisory is expected on every turn and describes the intentionally disabled
code execution path, not the direct chart tools. It is never itself a
`ToolsUnavailable` signal, whether or not a direct call follows. The separate
requires-tools/no-call guard still applies; tool-router errors and turn errors
naming tools are not suppressed by successful calls. The bridge never enables
Code Mode or its host in response to this message.

## Bounds, interruption and recovery

The shared registry CallBudget defaults to eight attempts per turn; rejected
namespace/name/arguments consume attempts too. Arguments are validated before the
executor and again by Registry::call. Failures, panics and executor timeouts return
`success: false`, with a short scrubbed text reason, rather than ending a healthy
session. Text is capped at **16 KiB per result**, aggregated across all its content
items, and **128 KiB per turn** (`MAX_TURN_TEXT_BYTES = 8 * MAX_TEXT_BYTES`), admitting
eight maximal text results. PNG bytes retain their independent **4 MiB per-result
and 4 MiB per-turn** caps. Failure text consumes remaining turn text allowance and
is also result-bounded (it may be empty if no bytes remain). Oversized single
results retain the existing per-call text/image failure reason. Only PNG data URLs
are emitted; no remote URL is fetched. Registry truncation remains explicit JSON.

Tool calls must match the current thread and acknowledged turn id. Calls racing
the start acknowledgement wait at most **100 ms** (`TURN_ACK_TOOL_WAIT`), then
validate against that same local turn; an unknown/mismatched id fails `stale_turn`
without invoking the executor or borrowing another turn's consent. Dropping or
failing `send` before its ack clears/cancels the pending turn and stops the entire
session, since the unknown server turn cannot be safely interrupted by id. No late
ack from that session can start work; explicitly recover before sending again.

An independent watchdog runs even if `next_event` is not being polled. The turn
fails and the session stops after **120 s** (`TURN_IDLE_TIMEOUT`) since its start
ack, last matching consumed notification, or accepted tool call; an interrupt
imposes a separate absolute **10 s** (`INTERRUPT_TIMEOUT`) completion deadline that
further events cannot extend. This includes an interrupt RPC that never acks.
The next event drain recovers (or returns the terminal failure); a new send cannot
remain stuck behind the failed turn.

Process death fails the current turn and cancels queued jobs. Recovery waits for
the supervisor's actual cleanup report (child wait, descendants, private-home
recheck), bounded by **10 s** (`CLEANUP_TIMEOUT`); it never substitutes a sleep for
cleanup evidence. Missing cleanup stops the bridge; cleanup findings taint it.
Exponential backoff starts at **250 ms**, doubles per consecutive attempt, and is
capped at **5 s** (`RECOVERY_INITIAL_BACKOFF`, `RECOVERY_MAX_BACKOFF`). There are at
most **3 consecutive attempts** (`MAX_RECOVERY_ATTEMPTS`), so actual waits are
250/500/1000 ms before terminal `agent stopped; restart the app/agent`. Cancellation
consumes an attempt; successful handshakes alone do not reset the count, only a
normally completed healthy turn does. Recovery reconnects through the original
isolation path, checks the account again, and sends
`thread/resume { threadId, cwd, baseInstructions, sandbox: "read-only",
approvalPolicy: "untrusted", approvalsReviewer: "user", serviceTier: "default",
runtimeWorkspaceRoots: [], excludeTurns: true }`. The schema has no dynamicTools
field on resume: Codex must restore the persisted tool definitions. Threads are
non-ephemeral so there is a rollout to resume. If resume is rejected, a fresh
thread registers the full registry and emits `HistoryLost`; the interrupted prompt
is never silently replayed. Tripwires and cleanup/isolation findings forbid
recovery. Unsafe state belongs to the bridge, shared with every transport before
its handshake, including a replacement whose handshake is cancelled or fails.
It is never cleared; `send` and `recover` refuse with
`bridge unsafe; restart the app/agent`. Live persisted-tool restoration remains
a manager check, and the post-resume guard catches unavailable tools.

### Privacy: persisted sessions and bounded retention

Non-ephemeral rollouts under `<private home>/sessions` contain prompts, chart/tool
results and assistant text, and are needed for resume. At bridge start, **before
spawning**, retention removes bridge-created threads older than **7 days**
(`SESSION_RETENTION_AGE`, measured from creation), or outside the **newest 50**
(`SESSION_RETENTION_THREADS`). Threads started during the current run are pruned
on the next start, not out from under a running agent.

A private 0600 `sessions/rollout-bridge-retention.jsonl` ledger records only the
UUID and creation time of threads this bridge starts (including history-loss
fallbacks). This filename is already accepted by the session validator; no new
validator exception was added. Pruning validates the whole home first and only
unlinks validator-accepted `rollout-*.jsonl` files ending in those expired UUIDs,
under the sessions tree. It refuses symbolic links and hard-linked files; it does
not traverse links or delete unrelated log/auth/config/lock files. Untracked
legacy/spike sessions are deliberately not claimed as bridge-owned and are left
untouched: review those separately rather than inferring ownership from age.

Matching history/index rows are removed from the existing `thread_history_1.sqlite`
and `state_5.sqlite` databases, never by deleting database files or retained
threads. The existing workspace SQLite dependency is reused (no new crate).
Tables with `thread_id` and the `threads.id` parent are pruned with bound UUID
parameters, secure-delete, vacuum and WAL truncation. Unknown schemas or cleanup
errors refuse startup instead of claiming successful retention. Offline tests
cover age/count bounds, retained and untracked histories, idempotence, and link/
unknown-entry refusal before any deletion. Live schema compatibility still needs
the manager's isolated run; no real home/database was inspected here.

The two older text-cap tests were corrected: a second half-sized text result now
succeeds, while the independent image-turn-cap assertion remains unchanged. The
aggregate test additionally proves eight full 16 KiB results fit and split content
items cannot evade the single-result cap. The older tripwire test also permits
`send` itself to report unsafe when the forbidden item races the start ack, rather
than requiring it to succeed before `next_event` reports the same violation. The
four review regressions retain their assertions and no longer have BUG ignores.

## Offline evidence and boundaries

Duplex fake app-server tests replay the committed first A0 turn through the real
transport and bridge. Its frozen `image=true` arguments are deliberately rejected
by the new registry schemas rather than adding a spike-only field to ToolSpec.
Separate scripts reach all seven ReadOnly tools with valid A1b arguments, including
an image reply, and cover invalid namespace/name/arguments, exhaustion, oversized
results, timeout/panic, mid-stream interrupt timing, death/resume/fallback,
readiness/version failures, tools-unavailable signals and unsafe non-recovery.
App tests verify queue cancellation/deadlines/oneshots and loaded-bar reads from
logic alone. Driver tests use SampleProvider without a child process or network.

The headless driver loads 1,000 actual SampleProvider daily bars for
`AAPL US Equity`; it never chooses a credential-bearing provider. Its documented
offline search catalogue is **AAPL/Apple, MSFT/Microsoft, NVDA/NVIDIA, SPY/SPDR S&P
500 ETF**, using canonical `US Equity` keys, case-insensitive key/name substrings
and exchange `sample`. SampleProvider itself has no search catalogue. The driver
test asserts that `find_symbol "AAPL"` returns Apple, not merely success with zero
hits. Empty indicator and edit lists are real headless state, not invented study values. Screenshot is
registered but fails for lack of consent/visible capture. The app executor also
fails screenshot closed: visible-window consent/capture wiring belongs to A2b.
PNG transport and independent image budgets are covered offline, not claimed as a
new live screenshot proof. No live subscription, latency or recovery claim is
made by these tests.

## Exact live commands (manager only)

Run from the repository root (the `codex/agent-bridge` worktree). Reuse the
manager's already signed-in private spike home below, or sign into a new app-owned
home with the existing spike login flow. Do not use `~/.codex`. Raw transcript
paths must be new and outside the validated `codex-agent` child directory. These
commands have **not** been executed during implementation.

```sh
TR_PROVIDER=sample cargo run --offline -p tr-agent --example agent_bridge -- \
  --home "$HOME/Library/Application Support/terminal-rs-agent-spike" \
  --model gpt-6.1-sol \
  --prompt "What is the trend on the pinned chart? Use read_chart and read_data, then analyze mode trend. Cite loaded bar timestamps and closing prices; distinguish sample data from live market data." \
  --transcript "$HOME/Library/Application Support/terminal-rs-agent-spike/raw-a2a-trend-1.jsonl"

TR_PROVIDER=sample cargo run --offline -p tr-agent --example agent_bridge -- \
  --home "$HOME/Library/Application Support/terminal-rs-agent-spike" \
  --model gpt-6.1-sol \
  --prompt "Exercise each chart tool once: read_chart; find_symbol query AAPL; read_data for AAPL US Equity with limit 20; analyze for that symbol with mode trend and limit 20; indicator_values; read_user_actions; screenshot. Report any consent or headless screenshot failure honestly. Use at most seven calls." \
  --transcript "$HOME/Library/Application Support/terminal-rs-agent-spike/raw-a2a-all-reads-1.jsonl"

TR_PROVIDER=sample cargo run --offline -p tr-agent --example agent_bridge -- \
  --home "$HOME/Library/Application Support/terminal-rs-agent-spike" \
  --model gpt-6.1-sol \
  --prompt "Read the chart and bars, then describe every loaded bar in a long detailed answer." \
  --transcript "$HOME/Library/Application Support/terminal-rs-agent-spike/raw-a2a-interrupt-1.jsonl" \
  --interrupt-after-first-delta

TR_PROVIDER=sample cargo run --offline -p tr-agent --example agent_bridge -- \
  --home "$HOME/Library/Application Support/terminal-rs-agent-spike" \
  --model gpt-6.1-sol \
  --prompt "Read the pinned chart and write a very long bar-by-bar discussion of the loaded data." \
  --transcript "$HOME/Library/Application Support/terminal-rs-agent-spike/raw-a2a-recovery-1.jsonl" \
  --verify-recovery

cargo run --offline -p tr-agent --example scrub_transcript -- \
  "$HOME/Library/Application Support/terminal-rs-agent-spike/raw-a2a-trend-1.jsonl" \
  "$HOME/Library/Application Support/terminal-rs-agent-spike/scrubbed-a2a-trend-1.jsonl"
```

The driver exits nonzero for tools unavailable, unexpected completion, timeout, or
interrupt latency >= 1 s. Interrupt is sent only after the first agent-message
delta. For a live crash check, kill only this driver's sandboxed app-server from
Activity Monitor during the long prompt, not the user's unrelated Codex process.
The driver reports the recovery result rather than replaying the turn. With
`--verify-recovery`, it requires `Resumed` and sends a separate, explicitly requested
read_chart/read_data self-test; history loss, a second crash, or no observed crash
exits nonzero. Without that flag, it exits on recovery and leaves resend policy to
the caller. A2b can continue using the same bridge after `Recovered`.
`--optional-tools` sends the prompt with `requires_tools = false` (plain chat), so a
turn that completes without tool calls passes instead of reporting tools unavailable.
A2b should send ordinary chat with `requires_tools = false` and reserve `true` for
prompts that cannot be answered without chart data (and the first-turn self-test).

### Live results (2026-10-23, gpt-6.1-sol, sample provider)

| Check | Result |
|---|---|
| Trend question | `read_chart`, `read_data`, `analyze`; cites loaded bars and labels sample data |
| Every ReadOnly tool | 7/7 reached in the `chart` namespace; `screenshot` refused without consent |
| Two default `read_data` calls in one turn | both succeed (per-result 16 KiB, per-turn 128 KiB) |
| Plain chat (`--optional-tools`) | completes with no tool calls |
| Interrupt after first delta | 0.004 s, status `interrupted` |
| Kill app-server mid-stream | `Resumed`, no replay; self-test turn completes with 2 calls |
| Private `HOME` | still empty |
Review scrubbed transcripts locally before sharing; never copy raw login/account
files or enable tools/features to work around a failure.

## Verification

All commands use offline dependency resolution; no real Codex process, credentials
or network turn is used in tests.

| Command | Result |
|---|---|
| `cargo fmt --all` | PASS; test counts N/A |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | PASS, zero warnings; test counts N/A |
| `cargo test --workspace --offline -- --quiet` | PASS: 1,215 passed (including the sealed-connector compile-fail doctest), 0 failed, 7 pre-existing ignored |
| `cargo test -p tr-agent --all-targets --offline -- --quiet` | PASS: 94 passed, 0 failed, 0 ignored; includes both example tests |

A pre-existing `crates/agent/target/` contains leftover private-home test fixture
directories, not required build inputs. Successful tests clean up their own new
fixtures; the old directories were left untouched for manager cleanup. The build
uses the workspace-root `target/`. No real Codex binary, real credential/home, or
live service was used. Existing manager validator fixes and reviewer tests remain.
