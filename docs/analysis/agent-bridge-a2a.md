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

## A2b agent pane

`NEW AGENT`, the + menu's Agent entry, and `AGENT` open the pane. There is one
background-owned session and one in-memory conversation per app. The **first Agent
pane** owns the UI; additional panes say "Agent already open" rather than starting
another child. Closing that pane makes the next Agent pane the owner, without
replaying anything. Closing every pane retains the conversation/session until app
exit. App exit closes the control channel and joins the owner after bridge cleanup.

### State machine and actions

The first open shows the privacy disclosure with **Continue**. A small Privacy
info button reopens it later; disclosure acknowledgement is not exported/persisted.
After acknowledgement each onboarding state has one primary action:

| State | Primary action | Transition |
|---|---|---|
| Dormant | Start agent | Checking, on explicit request only |
| Checking | Checking… (disabled) | Missing / Unsupported / SignedOut / RateLimited / Ready / Stopped |
| Missing | Check again | Checking; installation instructions are plain text, never an executed command |
| Unsupported | Check again | Checking; found version and tested `codex-cli 0.163.0` are shown |
| SignedOut (including non-ChatGPT account) | Sign in with ChatGPT | SigningIn |
| SigningIn | Cancel | SignedOut; successful completion continues readiness checks |
| Ready | Send | One active turn; later prompts join the FIFO queue |
| RateLimited | Check limits | Read limits again; Ready if quota permits |
| ToolsUnavailable | Retry | Explicitly retry the last prompt; no silent replay |
| Stopped / ordinary failure | Restart agent | New isolated session; queue stays paused |
| Stopped / unsafe | Close agent | Close the pane, retaining the sticky safety state |

SigningIn also offers the secondary **Open sign-in page again** action. Only the
URL returned by `account/login/start` goes through the existing app opener. It is
never drawn, logged or serialized. Login start/wait/cancel, account reads, model
listing, launches and bridge RPCs run on the owner thread. Login uses the existing
login-only sandbox, then waits for the supervisor's cleanup before starting the
normal bridge. Scrubbed login failures are inline notices. Ordinary RPC/recovery
failures offer Restart agent through that same path, with the queue paused. An
**unsafe** bridge has no API to reset its sticky safety state and offers only Close
agent. Restart the app after resolving that cause; neither Check nor a tools-mode
change can construct a replacement to evade a known safety refusal.

Recovery and history loss are inline notices. The active prompt is not resent,
and remaining queued prompts are paused. Resume queue or Retry last prompt is an
explicit user decision. Interrupt also pauses the remaining queue. Queue capacity
is 32 prompts; Clear queue drops waiting prompts, not the active turn. Ordinary
input uses `requires_tools = false`, even if its natural language mentions tools.
The three suggested data questions use `true`. There is no separate bridge
self-test API and no extra hidden/subscription-consuming turn is invented.

### Persistence, rendering and integration boundaries

`PaneSettings::Agent` contains only **model id, effort, tools mode, and a valid
pinned chart uid**. No conversation, queued prompts, login URL/id, account payload,
token, auth file or transcript enters layouts. Invalid/closed pins clear, and
layout replacement cannot attach an old uid to a replacement chart. Pin active
chart uses the last focused chart (otherwise the existing chart-selection rule).
Labels use `security_names::label`, including crypto's BASE/QUOTE and venue.

Models come from `model/list`, with `isDefault` first and per-model supported
efforts; `ultra` remains excluded and `max`/costly efforts carry a usage/latency
warning. The backend picker currently contains Codex only. **Read only** is the
default. **Can edit charts** is selectable only when
`agent_edit_tools_available()` is true. `Settings::tool_set` / `AgentPane::tool_set`
feed the background session's `BridgeOptions.tools`; only an explicit persisted
Edit choice enables `ToolSet::Edit`. The header distinguishes **Next session**
(the saved choice) from **Current mode** (the session's frozen registry).

Changing tools mode while idle confirms the change with an inline notice and
restarts an open session through the same login-only cleanup and normal isolated
launch path as Start/Restart agent. It never changes a registry mid-turn. The
picker is disabled during turns, sign-in/checking, and sticky unsafe stops; the
app action enforces the same guards. Queued prompts stay paused across restart,
models are fetched again, and nothing is resent. Choosing before Start changes
only the next start's options and does not launch a child. A restored layout choice
also cannot mutate the existing session's registry. A new bridge starts a new A3
ownership session; reconnect/resume within a bridge retains ownership.

The privacy disclosure explains that Edit can add, change and remove the agent's
own drawings, indicators and comparisons on the pinned chart. Objects are badged
Agent and undoable; user-owned and kept objects are not granted to the agent.
Screenshots remain refused by the existing executor: this build has no capture
consent flow, sends `screenshot_consent = false`, and never claims to send images.

The tiny markdown parser renders paragraphs, star-delimited bold/italic, inline
code, fenced blocks, headings, bullet/numbered lists, and pipe tables as monospace
grids. Links are visible text only; they do not open automatically or execute HTML.
Read tool rows contain name, success/failure, duration and collapsible scrubbed
arguments. Edit rows instead contain a bounded, scrubbed object id and short label,
for example `#42 · horizontal line at 227.12`, `#43 · trend line (2 anchors)` or
`#7:4 · RSI 14`. The session projects only that edit summary from returned text;
raw result content, image bytes and anchor arrays never reach the pane inbox.
Comparison rows use the symbol as their object identity. The rate-limit projection
contains only percent, window duration and reset time, never credit balances,
plan names or account identifiers.

`BridgeEvent::Started { turn }` attaches the canonical A3 id to the local user
prompt's conversation turn, never a Codex wire id. The successful send ack also
projects Started so partial edits retain an undo key even without a subsequent
wire start notification. Completion supplies the same key as a fallback. These
ids, turn metadata, and undo summaries stay in memory, outside layout settings.

Each finished turn with edits (successful edit-tool rows or retained A3 inverse
state) shows **Undo this turn**. The header offers **Undo last agent turn**, using
A3's latest undoable turn, not the last read/chat message. Both emit the same app
`Action::UndoTurn` and call `TerminalApp::undo_agent_turn` on the UI thread. Undo
covers the whole turn on all affected charts, preserving conflicting user edits.
The result appears inline on that prompt as reverted / conflicts skipped / chart
counts. Interrupted or failed turns can also undo their partial edits. No undo is
enabled during an active turn. Availability is refreshed from A3 before rendering
and after undo; exhausted/pruned history or layout replacement disables the action
with the exact A3 tooltip:
`undo for this agent turn is no longer available (layout changed/app restarted)`.
App restarts retain neither conversation nor inverse state; saved Agent provenance
never reconstructs undo permission.

`App::logic` drains the session inbox as well as the existing A1a live-tool queue.
Only rendered events wake the pane; reasoning/advisory/unused wire events do not.
Text deltas are merged in the inbox and markdown updates use a 33 ms dirty-only
coalescer, with no idle timer. Hidden tabs/minimized windows do not request stream
repaints. As a conservative macOS hide policy, streaming repaint requests also stop
when **all native viewports are unfocused**; focusing the app shows accumulated
text. This avoids a timer on macOS application hide, not just minimization. Tool
queue wakeups/deadlines remain the unchanged A1a mechanism. No polling loop was
added. Enter sends, Shift+Enter inserts a newline, and Esc interrupts; while the
input owns focus, chart keys and even the global command-palette shortcut are
blocked before dispatch.

The only backend changes are additive metadata (`Model.is_default`, scrubbed found
version on `WrongVersion`) and public access to the existing cleanup wait. Version,
isolation, skill inventory, tripwire, readiness, budgets, scrubbers, and the sealed
Connector have not been relaxed. The pane's offline fixture test uses the same
in-memory duplex transport/Codex client path as the existing fake app-server tests;
it replays the committed A0 first turn into a deterministic tool/reply block list.
It does not add an unchecked Connector or launch a real binary.

### Exact live checks for the manager (not run by A2b)

Run the sample app from this worktree, with the tested Codex CLI on the app's PATH.
Use a dedicated manager data root so existing layouts/sign-in are not changed:

```sh
TR_DATA_DIR="$PWD/target/a2b-manager" TR_PROVIDER=sample cargo run --offline -p terminal-rs
```

1. Run `NEW CHART`, then `NEW AGENT`. Read the disclosure, Continue, Start agent.
   With Codex absent expect Missing / Check again; with an unsupported version
   expect the actual found version versus `0.163.0`, with no normal agent turn.
   Do not enable tools/features or bypass the sandbox to make this pass.
2. On a fresh **app-owned** private home, verify SignedOut has one Sign in with
   ChatGPT action. Sign in, Cancel once, then sign in again. Verify reopening uses
   the same current login URL, login progress remains responsive, and a failed/
   stale login supplies only a scrubbed inline reason. Do not inspect/copy auth
   files or use the user's normal Codex home.
3. Pin the active sample chart. Verify **Current mode: Read only**, then click
   **What's the trend?** (or send `What is the trend on the pinned chart? Read the
   chart and loaded bars, cite timestamps and prices, and label sample data.`).
   Verify read_chart/read_data rows and a data-backed answer, with no chart edits.
   Expand read arguments: no image bytes, account data or secrets. Send `Hello`:
   a no-tool reply must not become ToolsUnavailable. Check quota percentages/reset
   times, no plan/credits balance, and visible Not investment advice.
4. With no turn running, select **Next session: Can edit charts**. Verify the inline
   restart notice, responsive Checking/login readiness, **Current mode: Can edit
   charts** after readiness, and a paused queue with no resent prompt. Send exactly:
   `Read the pinned chart and read_data with limit 20. Using only loaded bar
   timestamps and prices, create one horizontal support line at a recent low, one
   trend_line with two loaded anchors, and add_indicator RSI with params [14]. Use
   at most five tool calls. Report each returned object id and label sample data.`
   Since mode changes pause the queue, click **Resume queue** after enqueueing it.
   Verify two Agent-badged drawings and RSI, plus edit rows showing returned ids and
   short labels, never full anchor arrays or raw result payloads. During the turn,
   verify the tools picker and undo buttons are disabled.
5. Add a user drawing. On that completed edit turn click **Undo this turn** and
   verify the inline reverted / conflicts skipped counts, Agent objects removed,
   the user drawing preserved, and that turn's action now disabled. Repeat the
   prompt explicitly and use the header's **Undo last agent turn**; it must select
   the edit turn even if a later read-only question/chat completed. Optionally Keep
   an Agent drawing/study before undo: the kept object survives and conflicts
   skipped increases. Reload a layout while retaining the local conversation:
   old undo actions disable with the exact layout-changed/app-restarted tooltip.
6. Ask `Read the chart and write a long, detailed bar-by-bar explanation of every
   loaded bar.` After the first delta, queue two labelled prompts, then click
   **Interrupt (Esc)** or press Esc with the Agent input focused. Verify provisional
   interrupted text, paused queue, Clear queue behavior, and no automatic retry.
   Use Retry last prompt only explicitly. Switch back to Read only while idle:
   another normal isolated restart is required and the queue must remain paused.
7. Switch model and effort. Verify supported/default efforts, costly warning,
   default-first model ordering and Codex-only backend picker. Screenshots still
   refuse; prompt text is not capture consent. Privacy info mentions own-object
   editing, Agent badges and undo.
8. Focus the input and type chart hotkeys and the command-palette chord: chart,
   command bar and palette must not act. Check Enter versus Shift+Enter. Open
   another Agent pane: it must show the already-open explanation and no new child.
9. Close the pinned chart: pin clears. Save/export/reload the layout: only the four
   choices above survive; no conversation or sign-in information is exported.
10. During a long reply, kill **only this app's** sandboxed app-server in Activity
   Monitor. Verify Recovered or history-lost notice, no resend, and paused queue.
   A subsequent explicitly requested data question must still satisfy tool proof.
   For a safety failure, verify Close agent only, never an in-pane safety reset.
11. Observe Activity Monitor / existing performance readout while streaming, idle,
   on a hidden Agent tab, minimized, and with the app hidden. Expect roughly 30 Hz
   text updates only while foreground-visible, no agent timer while idle/hidden,
   and accumulated text on return. Close the app and verify its child is reaped.

These are manager-only live acceptance checks; no login, real Codex execution,
real agent home access, network test, or CPU/latency measurement was performed by
this implementation worker.

### A23-WIRE verification (2026-10-23)

| Command | Result |
|---|---|
| `cargo fmt --all` | PASS; test counts N/A |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS; zero warnings; test counts N/A |
| `cargo test --workspace` | PASS: **1,300 passed, 0 failed, 7 pre-existing network tests ignored**, including the sealed-Connector compile-fail doctest |

Eight added offline regressions cover availability-gated/persisted mode choices,
next-start registry freezing and paused queues, canonical Started-to-prompt ids,
partial-edit undo, actual clicks on both undo buttons, inline summary rendering,
real A3 undo with a kept-object conflict, layout invalidation, projected edit ids/
labels without raw coordinates/results, and ReadOnly session options through the
duplex transport and app executor (all seven edit tools rejected before UI work).
The focused `cargo test -p terminal-rs agent_ --offline` run passes 79 tests, with
0 failed and 0 ignored. The shared inert workspace fixture is exposed only under
`cfg(test)` for the session regression; no unchecked Connector was added.

No new crate, real Codex launch, real agent-home access, or live/network acceptance
check was used. The seven network-dependent ignores remain unchanged. A3 ownership,
limits, isolation, tripwire, scrubbers, caps and readiness are unchanged. Git state
was not changed; the work is uncommitted.

## A3 agent edits

The app executor and sample driver now execute the Edit preset through the same
registry and shared edit validation. App mutations use the existing 8-job / 4-ms
logic drain. Isolation, readiness, tripwire, scrubbing, result caps and the
eight-attempt bridge turn budget are unchanged. A2b now exposes the Edit choice
through its persisted tools picker and new isolated session starts (A23-WIRE);
A3 remains the source of ownership, mutation bounds and undo availability.

### Ownership, identity and bounds

- **Session ownership is mandatory.** An agent may update/delete only drawings,
  studies and comparisons created by its current executor/bridge session. Saved
  `created_by: Agent` metadata is provenance, not permission. The object must also
  carry one of this session's creation turns and match its own kind/id/chart record.
  Reused IDs from layouts or templates do not transfer ownership. Applying a chart
  template or study preset makes imported studies/comparisons User-owned and clears
  their turn provenance (templates exclude drawings). Starting a new bridge
  creates a fresh ownership session; reconnect/resume inside it retains ownership.
  Enabling Edit is not consent to edit a user's existing objects. This version
  exposes no grant for user-owned objects and therefore always refuses them.
- Drawings retain A1a's globally issued numeric IDs. Study tool IDs are opaque
  strings `chart_uid:study_id`; read_chart/indicator_values expose these as
  `tool_id` in the app. Study allocation uses each chart's free u32 space, independent
  of drawing IDs; IDs observed/issued in the ownership session are not recycled.
  Even a loaded u32::MAX study ID or huge drawing ID does not exhaust that space.
  Unknown/removed IDs fail; an ID known on another chart fails as `wrong_chart`.
- A canonical turn ID is `<32 lowercase random hex session nonce>:<decimal sequence>`.
  Each bridge allocates a fresh 128-bit namespace (SQLite's OS-seeded PRNG, in memory)
  across bridges, sessions and restarts; reconnects keep it. `send()`, Started/Completed,
  ToolCall, provenance, edit log and undo APIs all use this ID. The separately held
  Codex wire ID is only for protocol validation/interrupts, never a host undo key.
- Agent drawings support every registered kind: horizontal (support/resistance
  level), vertical, trend_line, ray, rectangle and text. The explicit symbol must
  match the target chart. `all_symbols` remains the schema's explicit opt-in.
  Anchors must be finite, have the tool's required point count, lie inside loaded
  timestamps and not in the future. Absolute prices must not exceed 100 times
  the loaded absolute price magnitude (minimum magnitude 1), capped at 1e15.
- At most **20 creations per turn**, cumulatively (deleting/undoing does not refund
  this allowance), and **100 live agent objects per chart**. The chart count includes
  Agent drawings/studies/comparisons loaded from disk or created by another session.
  All three share the allowances. The existing
  bridge call budget remains stricter for a normal single turn; comparisons also
  retain the chart's existing eight-comparison maximum.
- Indicators use the existing core catalogue, parameter normalization, built-cache
  refresh and study-removal path. Removing a study with any calculation, pane or
  fill dependents fails rather than cascading into a user's studies.
- **A3B:** `set_view` can switch the pinned chart to **any supported timeframe,
  finer or coarser**, including weekly → `4H`. The symbol must remain the loaded
  symbol; an explicit different symbol or another chart target is rejected.
  The app calls the same `ChartState::set_timeframe` → `load` path as user changes:
  worker history requests, caches, session filtering, transformed views, study
  recomputation and ordinary autosave. It never resamples weekly bars into fake
  finer bars. A changed/closed pin or user symbol/timeframe change during a turn
  still fails instead of retargeting edits.
- Completion is asynchronous: the UI queue starts the switch under its existing
  **10-second** initial-job deadline; a pending oneshot job is checked on each
  logic tick after worker responses. The **20-second absolute set_view deadline**
  includes that initial queue wait and remains inside the bridge's existing
  per-call deadline and turn budget (the wait is not another tool attempt).
  No UI-thread waiting, busy loop or idle repaint timer is added; worker delivery
  wakes logic, and the executor timeout bounds the result even without a tick.
  Cache bars may satisfy the load without waiting for a provider refresh. Range
  endpoints are applied **after** the new displayed/source bars are available and
  clamped to their timestamps. Empty loads time out rather than claiming success;
  provider failures immediately return `provider_error` with a scrubbed reason
  capped at 300 characters. Completion is checked before expiry on each tick, so
  data delivered on the deadline tick succeeds. `data_loading_timeout` explicitly
  says the timeframe is **left switched**;
  late data may still finish the clamped range, and the partial edit remains undoable.
- Results include `timeframe`, `loaded_bars`, `first_bar_time`, `last_bar_time`
  (Unix seconds), `visible_range`, `from_cache` and `data_source` (`cache` or
  `fresh_load`). After switching, call `read_data` **with the new timeframe** before
  choosing drawing anchors/prices; pinned reads use those actual loaded bars,
  including when the turn's original pin named the old timeframe.
- **Linked panes are the sole exception to user-switch behavior:** an agent never
  moves other panes, even those linked for timeframe/range navigation. No linked
  range event is broadcast; results retain `linked_panes: []`.
- Drawings, studies and comparisons persist `created_by` / `turn_id`. Missing legacy
  comparison metadata means User. User remove/re-add creates a new User comparison,
  even with the same symbol; the agent cannot remove it. Drawing/study Agent badges are
  derived from `created_by`, not from labels or edit-log text. Drawing-list and
  selected-drawing menus and study legends/tooltips expose provenance, **Keep**
  and Delete/Remove. Keep converts the object to User ownership and prevents
  subsequent agent mutation and turn undo from removing it.

Structured failures have `success: false` and JSON
`{"error":{"code":"...","message":"...","field":"..."}}` (field is optional).
Existing registry codes `invalid_args`, `tool_disabled`, `budget_exhausted`,
`confirmation_required` and host timeout/cancellation behavior are retained. A3
adds the live-host codes listed in `tr_agent_tools::edits::ERROR_CODES`:

| Code | Meaning |
|---|---|
| `stale_id` | Unknown or removed object |
| `wrong_chart` | Another chart/symbol owns the requested ID |
| `not_owned` | User-owned, kept, previous-session object, or study with dependents |
| `chart_not_found` | Target or original pin was closed |
| `pin_changed` | Pin, pinned symbol or timeframe changed during the turn |
| `turn_object_limit` | Twenty creations already admitted for this turn |
| `chart_object_limit` | One hundred live agent objects already exist |
| `data_loading_timeout` | New bars unavailable by the 20-second set_view deadline; timeframe left switched and partial edit undoable |
| `provider_error` | Provider rejected/failed a view history load; scrubbed reason returned immediately, not as a timeout |

### A2b APIs and undo

```rust
// Default remains ReadOnly. Opt in before starting a NEW bridge/thread:
let mut options = BridgeOptions::new(private_cwd);
options.tools = tr_agent_tools::ToolSet::Edit;
let bridge = AppExecutor::new(&app).start(connector, options).await?;

// UI-thread APIs; no pane dependency:
agent_bridge::agent_edit_tools_available() -> bool // true
app.last_agent_turn() -> Option<&str>
app.last_agent_turn_for_chart(uid: u64) -> Option<&str>
app.undoable_agent_turns() -> Vec<&str>
app.agent_turn_undo_available(turn_id: &str) -> bool
app.undo_agent_turn(turn_id: &str) -> Result<agent_bridge::UndoSummary, String>
// UndoSummary { charts: usize, reverted: usize, skipped: Vec<String> }
```

Do not call start on the UI thread; as in A2a, obtain the executor/pin there and
move only the executor and options to the background owner task. ReadOnly still
rejects every mutation before queueing it. The normal layout autosave serializes
these same chart drawing/study/view fields; no separate agent save format exists.
Successful edits and turn undo request autosave even without `ui()`. If a writer
is already running, logic coalesces a fresh snapshot after it finishes. Only that
pending-save interval requests timed wakeups; an idle agent adds no polling.

The command bar accepts **`UNDO AGENT`** for the global latest undoable turn. The
chart context menu offers **Undo latest agent turn touching this chart (all charts)**:
it selects the latest turn with retained undo state on THAT chart, independent of
focus or pin, then undoes the **whole turn on all affected charts**. Its internal
command is `UNDO AGENT CHART <uid>`. The all-chart scope is explicit in the label.
Undo uses A1a's inverse drawing operations by ID and neighbouring z-order, not a
whole-chart rollback. Studies are inverted by instance ID and neighbours; view
rollback checks user-navigation ownership rather than exact visible ranges. Comparison undo checks the
expected author/creation turn, preserves user re-adds, and reports missing or
conflicting objects in `skipped`; no-op removals are never counted as reverted.
Interleaved user edits, Keep, user-created studies and changed view state survive.
A3B timeframe switches and their deferred range completion are logged with
`Author::Agent` and the canonical `turn_id`. View inverses keep symbol, timeframe,
logical range and a user-navigation generation, **not a bars snapshot**. Undo restores
the previous timeframe through the same loading path and buffers the old visible
range until its bars arrive; the undo API reports the accepted reload immediately.
That buffered range also counts as the logical range for successive undos and new
agent edits before reload. It skips/reports `view:<uid>` conflicts if the user
changed timeframe or symbol (including switching away and back), panned, zoomed,
issued a range command, or drove linked range navigation from another pane.
Live bars, cache-then-refresh and paging that preserves the anchor do not advance
the user generation or revoke undo ownership. Undo does not reset that generation.
A timeout/interruption/provider failure keeps the partial switch undoable, and
late completion updates its inverse. The headless driver validates/plans before
fetching and holds one async state lock across planning, loading and application
so parallel calls cannot race their history or inverse state.
Undo history is session-local (drawing history retains A1a's existing 64-step
bound); layouts save provenance, not undo stacks. Layout replacement clears the
undoable list. Availability APIs (also exposed per-chart by read_chart) exclude
turns with no retained inverse state. An unavailable turn returns
`undo for this agent turn is no longer available (layout changed/app restarted)`
instead of a successful zero-operation report.

### Exact A3 live commands (manager only; not run during implementation)

Run from the repository root with the manager's already signed-in **app-owned**
private home, never the user's Codex home. Transcript filenames must be new.
The driver defaults to `--tools read-only`; `--tools edit` is explicit opt-in.
It prints resulting drawings, indicators, comparisons, timeframe, range and edit
events as JSON at turn completion. `--undo-agent-turn` then performs host-side undo of that completed
turn using its canonical ID and prints the resulting chart as a second JSON record.
It exits nonzero unless at least one object was actually reverted (a view-only or
no-op undo is not proof of object undo). The offline driver regression runs the
actual sample adapter through the sealed fake-connector bridge, not hand-built
ToolCalls with an invented wire ID.

```sh
TR_PROVIDER=sample cargo run --offline -p tr-agent --example agent_bridge -- \
  --home "$HOME/Library/Application Support/terminal-rs-agent-spike" \
  --model gpt-6.1-sol --tools edit \
  --prompt "Read the pinned chart and read_data with limit 20. Using only loaded bar timestamps and prices, create one horizontal support level at a recent low, one trend_line with two loaded anchors, and add_indicator RSI 14. Use at most five tool calls. Report each returned object id and explain that these are sample bars." \
  --transcript "$HOME/Library/Application Support/terminal-rs-agent-spike/raw-a3-edits-1.jsonl"

TR_PROVIDER=sample cargo run --offline -p tr-agent --example agent_bridge -- \
  --home "$HOME/Library/Application Support/terminal-rs-agent-spike" \
  --model gpt-6.1-sol --tools edit --undo-agent-turn \
  --prompt "Read the pinned chart and read_data with limit 20. Create a horizontal support level and a two-anchor trend_line using loaded timestamps/prices, then add_indicator RSI with params [14]. Use at most five tool calls; return the ids. The host will undo this turn after completion." \
  --transcript "$HOME/Library/Application Support/terminal-rs-agent-spike/raw-a3-undo-1.jsonl"

# A3B: SampleProvider generates actual finer-timeframe sample history (no network).
TR_PROVIDER=sample cargo run --offline -p tr-agent --example agent_bridge -- \
  --home "$HOME/Library/Application Support/terminal-rs-agent-spike" \
  --model gpt-6.1-sol --tools edit --undo-agent-turn \
  --prompt "First set_view to 1W. Then switch to 4h and draw support and resistance: use set_view timeframe 4H, read_data on 4H with limit 20, then create two horizontal lines using the returned loaded bar lows/highs. Report loaded bar timestamps and returned ids, label sample data, and use at most six calls. The host will undo this turn after completion." \
  --transcript "$HOME/Library/Application Support/terminal-rs-agent-spike/raw-a3b-4h-1.jsonl"
```

In the app, follow the exact A2b in-app checks above: sign in through the pane,
prove a Read only trend answer, switch to Can edit charts and wait for the isolated
restart, explicitly resume the queued support-line + trend-line + RSI prompt, then
use **Undo this turn** or **Undo last agent turn**. Both actions call the same A3
API as **`UNDO AGENT`** and report reverted/conflicts skipped inline. Add/move a user
drawing during the agent turn and check its z-order survives. Keep an Agent
object before undo to check conflict preservation. Also interrupt a streaming long
answer and confirm the queue pauses. Layout reload disables old turn actions;
app restart has no conversation or undo stacks to restore. No ownership grant,
creation cap, readiness guard or screenshot consent policy changes with the toggle.
These live checks remain manager-run; offline regressions use no real child or network.

### A3 verification (2026-10-23; original implementation, historical)

| Command | Result |
|---|---|
| `cargo fmt --all` | PASS; test counts N/A |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | PASS, zero warnings; test counts N/A |
| `cargo test --workspace --offline -- --quiet` | PASS: 1,234 passed, 0 failed, 7 pre-existing ignored; includes the sealed-connector compile-fail doctest |
| `cargo test -p tr-agent --examples --offline -- --quiet` | PASS: 3 passed, 0 failed, 0 ignored; includes both headless bridge tests |

A3 adds seven app-executor integration tests and four shared-validation tests,
plus the headless edit/undo example test. Coverage includes all drawing kinds,
CRUD, session ownership, structured failures, combined limits, Keep/highlights,
interleaved undo/z-order, loaded-data view clamping/resampling, changed pins and
closed charts, and logic-only autosave coalescing with an in-flight writer.
No real Codex process or manager home was accessed; live commands above remain
manager-run checks, not an A3 live verification claim.

### A3-FIX verification (2026-10-23)

All five reviewer `BUG` ignores are removed. The app's reused-turn regression now
uses the same canonical allocator as the bridge: injecting the identical
`bridge-1` ToolCall ID for two sessions contradicts the new identity contract
(the same ID denotes the same turn). Bridge tests independently verify matching
send/Started/tool/completion IDs and uniqueness across bridges. Comparison fixtures
now supply real author/turn metadata; the ordinary-study reservation fixture uses
a chart-local ID rather than depending on the unrelated drawing counter.

Additional regressions cover drawing/study ownership after ID reuse, comparison
remove/re-add and undo conflicts (including missing/no-op removals), persisted
comparison provenance, layout/restart unavailability, chart-specific turn
selection with whole-turn undo, template ownership transfer, and huge drawing /
maximum-u32 study IDs. The driver regression exercises the actual example adapter
through the in-module sealed fake connector; production Connector sealing is intact.

| Command | Result |
|---|---|
| `cargo fmt --all` | PASS; test counts N/A |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS, zero warnings; test counts N/A |
A2b verification:

| `cargo test --workspace` | PASS: **1,239 passed, 0 failed, 7 pre-existing ignored**, including the sealed-Connector compile-fail doctest |

The focused `cargo test -p terminal-rs agent_` run passed 33 tests, with 0 failed
and 0 ignored. A2b adds 16 offline tests covering markdown, state transitions,
settings, pickers, rate-limit/privacy projection, queue/recovery, dirty timers,
keyboard/focus guards, login cancel/reopen/failure and committed A0 fixture replay.
The seven existing ignored tests require public-provider network availability;
none was enabled. No dependency was added and no live acceptance check was run.


A3 verification:

| `cargo test --workspace` | PASS: 1,249 passed, 0 failed, 7 pre-existing network tests ignored; includes compile-fail doctest |
| `cargo test -p tr-agent --examples` | PASS: 2 passed, 0 failed, 0 ignored; bridge driver regression now runs in the library suite |

No new crates or network tests; no real Codex execution, manager-home access, or
changes to isolation/tripwire/scrubber/caps/readiness.


### A3B verification (2026-10-23)

| Command | Result |
|---|---|
| `cargo fmt --all` | PASS; test counts N/A |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS; zero warnings; test counts N/A |
| `cargo test --workspace` | PASS: **1,312 passed, 0 failed, 7 pre-existing ignored**, including the sealed-Connector compile-fail doctest |
| `cargo test -p terminal-rs agent_bridge::tests -- --test-threads=1` | PASS: 26 passed, 0 failed, 0 ignored |
| `cargo test -p tr-agent --example agent_bridge` | PASS: 2 passed, 0 failed, 0 ignored |

Offline regressions cover weekly → 4H worker/session-history delivery and bar
metadata, pinned read_data/new-bar drawing anchors, study requests, cache results,
linked-pane isolation, tick and no-tick timeouts, cancellation/late delivery,
undo before/after loading, multiple switches in one turn, timeframe/symbol user
conflicts (including away-and-back), range conflicts, and ReadOnly/symbol-change
refusals. The sealed bridge driver's RSI assertion now checks the calculation on
freshly loaded weekly bars, not the old insufficient resampled daily seed.

An intermediate workspace run hit the unrelated existing Coinbase heartbeat
reconnect timing assertion; its isolated rerun and the subsequent complete suite
passed without changing or ignoring that test. No new crate, network test, real
Codex execution, real agent-home access, safety/ownership/limit relaxation, commit,
or git-state change was made. Live A3B commands/checks above remain manager-only.

## A2a verification (historical)

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

## In-app acceptance (2026-10-23, release build of `feature/agentic-charts`, user-driven)

Run by the user in the real app (live data, Coinbase BTC/USD) with `gpt-6.1-sol`:

| Check | Result |
|---|---|
| Sign in with ChatGPT from the pane | pass |
| Read-only "What's the trend?" on a weekly chart | pass: cites real weekly bars, flags the forming bar |
| Edit mode: support line, trend line and RSI 14 | pass: objects drawn and badged |
| Undo last agent turn | pass: all three removed |
| Interrupt with Esc mid-reply | pass: stops at once, reply marked provisional |
| Chat focus vs chart hotkeys; palette Esc | pass |
| Edits on a 4h chart after a manual switch | pass: levels from 4h bars |

Bugs found and fixed during the session: the queue was paused while signed out, so
the first prompt after sign-in never sent; turns without a pin carried an empty
chart, so `read_chart` returned "chart not found" (sending now pins the last
focused chart).

Gap found at that acceptance run: `set_view` only allowed the loaded symbol and
an equal or coarser timeframe, so the agent asked the user to switch weekly to 4h
manually. **A3B implements this follow-up** with asynchronous finer/coarser loading
and the same undo/provenance rules, as documented above. The manager should rerun
that prompt on a weekly pinned chart, verify 4H bar metadata and data-backed lines,
then undo to the weekly range. Also change the timeframe manually before undo and
verify a reported conflict. These new live checks have not been run by the worker.
