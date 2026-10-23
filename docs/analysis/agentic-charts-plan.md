# Agentic charts implementation plan

Status: design approved for implementation, 2026-10-22. No code has been written yet.
Phases are numbered A0–A5 (renamed from P0–P5 so they do not collide with the
performance programme P1–P6). See "Integration review" at the end for prerequisites
and ordering against the existing backlog.

This plan adds an agent chat panel that reads and edits charts. It covers drawings,
indicators, comparisons, the view, screenshots and, later, watchlists and alerts.
The agent runs on the user's local Codex CLI and their ChatGPT subscription.
Other local agent CLIs can be added behind the same interface.

The design comes from four sources:

- A behavioural study of CoinMarketCap Agentic Charts.
- A read-only map of the integration points in terminal-rs.
- Inspection of `codex-cli 0.163.0` (`codex app-server generate-json-schema --experimental`).
- Three reviews: a Claude architect review, a Codex self-review and a final review by Claude Fable 5.1.

Each phase lands on its own feature branch, following the existing workflow.

## Reference behaviour

CoinMarketCap Agentic Charts pairs a TradingView chart with a chat panel.

**Request.** Each message is one POST to `/chatbot/v3/agentic-charts/run`, with this body:

```
{input, sessionId, runId,
 chartContext: {symbol, resolution, visibleRange}}
```

**Reply.** The response streams back as server-sent events:

- `START`
- `TOOL_START` / `TOOL_FINISH`
- `FRONTEND_CONTEXT_REQUIRED`
- `REPLY`
- `COMPLETE` / `FAIL`

**Where the tools run.** The model loop runs on the server, but chart tools run in the
browser. The server pauses with `FRONTEND_CONTEXT_REQUIRED`. The browser runs the
requested tools against the chart, then resumes the run with `frontendToolResults`.

**Tools.**

- `read_chart`, `read_data`, `read_user_actions`, `find_market_pairs`
- `analyze`: swings, trend, levels, volume_profile and volatility. These are computed
  deterministically, never by the model.
- `create`, `update` and `delete` for drawings, indicators, comparisons and marks. Every
  entity carries an id and `createdBy`.
- `set_view` and `screenshot`

**UX.** The chat shows a tool trace, a streamed reply, interrupt and retry, a message
queue, history, suggested prompts and a credit meter.

terminal-rs reproduces the behaviour, not the implementation. In a native app the
"browser" side is the UI thread, so the pause-and-resume dance becomes a channel
hand-off.

## Decisions

| Topic | Decision |
|---|---|
| Agent backend | Local Codex. Spawn `codex app-server` and speak JSON-RPC as JSON Lines over stdio from Rust. The Codex SDKs are TypeScript/Python only, so we don't use them. Billing goes through the user's ChatGPT login. |
| Codex tool path | `dynamicTools`, registered in `thread/start`. Codex calls them back with the `item/tool/call` server request. This requires `initialize.capabilities.experimentalApi: true`. MCP is the documented fallback. |
| Other backends | mimo (mimocode) over ACP in phase A4. mmx (MiniMax) is deferred: it is a stateless chat CLI billed by API key, with no effort setting. |
| Backend, model and effort pickers | Populated live. For Codex the source is `model/list` (paginated; each entry has `supportedReasoningEfforts` and `defaultReasoningEffort`). Effort is an open string, so it is never hardcoded. `ultra` is hidden because it triggers multi-agent behaviour. `max` shows a cost and latency warning. |
| Tool layer | One registry crate, `crates/agent-tools`, with `schema()` and `call()`. Thin adapters expose it as Codex dynamicTools, as an MCP stdio shim and as direct calls. |
| Read-only MCP server (existing backlog item) | Becomes the data-tool half of the registry: history, indicators, conditions, scans. It is served by a `terminal-rs mcp-stdio` shim that talks to the running app over a 0600 Unix socket. There is no HTTP listener. Crypto tools use C01 instrument identity. |
| TradingView MCP client | Used only as a naming and shape reference. It is excluded from the agent toolset. |
| Conversation persistence | App state, not `LayoutDoc`, because layouts are exportable. Threads are `ephemeral` unless resuming is enabled. |

## Isolation (critical)

A developer's `~/.codex/config.toml` may run with `sandbox_mode = "danger-full-access"`
and `approval_policy = "never"`, with browser and computer-use MCP servers attached.
Inheriting that would hand the chart agent a silent shell and a browser. The sandbox
settings alone do not fix this:

- `approvalPolicy: never` means Codex never asks; commands still run.
- `read-only` still allows reading the whole disk.
- No app-server method reports which tools the model can see.

Isolation is therefore built in three layers. All three are required.

### 1. Private Codex home

- Point `CODEX_HOME` at an app-owned directory.
- Sign in inside it with `account/login/start {type: "chatgpt"}`:
  - handle the `account/login/completed` notification and the cancel path;
  - handle the localhost callback port clashing with the user's own Codex;
  - enable `secret_auth_storage` so the token is stored in the Keychain.
- Private config:
  - `sandbox_mode = "read-only"`
  - `web_search = "disabled"`
  - `forced_login_method = "chatgpt"`
  - no `mcp_servers` and no `notify`
  - launch with `--strict-config`
- Disable every feature the agent doesn't need with `--disable`:

  | Group | Features |
  |---|---|
  | Shell | `shell_tool`, `unified_exec`, `unified_exec_tty` |
  | Images | `view_image`, `image_generation` |
  | Apps and plugins | `apps`, `plugins`, `remote_plugin` |
  | Browser and computer use | `browser_use`, `browser_use_external`, `computer_use`, `in_app_browser` |
  | Multi-agent | `multi_agent`, `multi_agent_v2` |
  | Hooks and skills | `hooks`, `skill_search`, `skill_mcp_dependency_install`, `tool_suggest` |
  | Other | `sleep_tool`, `realtime_conversation`, `worktrees`, `daemon_auto_start`, `code_mode_host` |

- Per-thread settings:
  - `environments: []`
  - `cwd` set to an empty app directory
  - `serviceTier: "default"`
  - `baseInstructions` replacing the coding persona
- Scrub the child's environment. An inherited `OPENAI_API_KEY` would silently move
  billing from the subscription to the API.
- Check that `account/read` reports a ChatGPT account at startup and after every reconnect.

### 2. Runtime tripwire

This is the fail-closed control.

On every `item/started`, the item type must be one of:

- `agentMessage`
- `reasoning`
- `userMessage`
- `dynamicToolCall`
- `contextCompaction`
- `plan`

Any other type triggers these steps:

1. Call `turn/interrupt`.
2. Kill the child.
3. Mark the backend unsafe.
4. Show the user why.

Other examples include `commandExecution`, `fileChange`, `mcpToolCall`, `webSearch`,
`imageView`, `imageGeneration` and `collabAgentToolCall`.

Also:

- **Canary turn.** Run one once per Codex version. It asks the agent to list its
  directory and run `ls ~`. It passes only if no disallowed item appears.
- **Approvals.** Decline every `*requestApproval` request. This is a backstop only.
- **Config check.** `config/read` with `includeLayers` verifies config, not tools.

### 3. OS sandbox

Launch app-server under a macOS `sandbox-exec` profile that allows only:

- reads of the private home, the codex binary and system libraries;
- exec of codex itself;
- outbound network on port 443.

This makes secrets such as `~/.ssh` unreadable even if a tool survives the first two layers.

### Version drift

- Check `codex --version` against a tested range at startup. Codex Desktop and
  `codex update` replace the binary.
- Commit a snapshot of the `--experimental` schema so protocol changes show up as diffs.
- Never enable analytics flags.
- Rotate the logs in the private home.

## Architecture

```
egui UI thread ── App::logic() drains AgentToolQueue ──┐
   │  Agent pane (PaneKind::Agent)                      │ oneshot replies
   ▼                                                     │
crates/agent ── AgentBackend trait ── CodexAppServer (stdio JSONL child)
   │                 └─ AcpBackend (phase A4)           ▲
   ▼                                                     │ item/tool/call
crates/agent-tools ── ToolRegistry ── data tools (worker / MarketData)
                                  └─ live tools (queued to UI thread)
```

### `AgentBackend`

The trait has these methods:

- `detect() -> BackendState`. States: missing, wrong version, not logged in, rate-limited, ready.
- `list_models()`
- `start_thread(opts)`
- `send_turn(model, effort, input, images, context)`
- `interrupt()`
- an event stream: `TextDelta`, `ReasoningDelta`, `ToolStarted`, `ToolFinished`, `Usage`,
  `RateLimits`, `Done`, `Error`

### Codex wire protocol

- JSON Lines with no `jsonrpc` field.
- Handshake:
  1. Send `initialize` with `clientInfo` and `capabilities.experimentalApi: true`.
  2. Send the `initialized` notification.
- Reply to `item/tool/call` using the JSON-RPC request id, not `callId`:

  ```
  {success, contentItems: [inputText | inputImage]}
  ```

- Sandbox spellings differ by method:
  - `thread/start`: `sandbox: "read-only"`
  - `turn/start`: `sandboxPolicy: {type: "readOnly", networkAccess: false}`

### Lifecycle

- Run one long-lived private child per app, never the user's daemon or proxy.
- Kill it by process group on exit.
- Restart it with backoff and fail any pending requests.
- Resume threads with `thread/resume`. Fetch history with `thread/turns/list` and `thread/items/list`.

### Per-turn context

Every `turn/start` carries `additionalContext` with:

- symbol
- timeframe
- visible date range
- pinned pane id

This is the equivalent of CMC's `chartContext`. Other turn controls:

- `outputSchema` (optional) returns a structured "actions taken" summary for the trace.
- `turn/steer` handles interjections while a turn is running.
- `turn/interrupt {threadId, turnId}` must also cancel queued UI tool calls. It does
  not roll back changes already applied.

### UI-thread marshalling

eframe 0.36 does not call `ui()` while the window is hidden. It calls only
`App::logic()`, which terminal-rs does not implement today.

- Live tools are queued and drained in a new `App::logic()`.
- Queue pushes and screenshot completion call `request_repaint`.
- The JSON-RPC reader never blocks on the UI.
- Deadlines are about 10 s for UI tools and about 30 s for data tools that fetch
  unloaded history. A timeout replies `success: false`.
- `screenshot` fails immediately while the window is hidden.

### Budgets

- `read_data` defaults to a summary: statistics, the last ~200 bars and LTTB downsampling.
- Each tool result is capped at about 16 KB, because Codex truncates silently.
- About 8 tool calls per turn. Every round trip uses subscription quota.
- The pane shows the 5-hour and weekly limits (`account/rateLimits/read`) and token
  usage (`thread/tokenUsage/updated`).
- Never use the fast or priority tiers.

## Tool registry

Every live tool takes `chart_id`, which defaults to the pinned pane. The pin must
survive layout changes, because pane ids change when panes are rearranged.

| Tool | Kind | Built on | Rules |
|---|---|---|---|
| `read_chart` | live | `ChartState`, `chart_navigation::date_range` | List all chart panes; entities include ids |
| `read_data` | data | `ChartState` bars, `MarketData::history` | Mark the in-progress last bar; ISO timestamps with zone and session; adjusted vs unadjusted; venue-qualified crypto ids |
| `analyze` | data | `levels::pivots`, ZIGZAG / FRACTALS, `atr`, `chart_analytics::volume_profile`, `anchored_vwap`, a new support/resistance finder | Versioned output, including the bar range it was computed over |
| `indicator_values` | live | the chart's built study outputs | |
| `find_symbol` | data | `MarketData::search` | |
| `set_view` | live | `set_symbol`, `set_timeframe`, `apply_linked_range` | Report any linked panes that also moved |
| `create_drawing`, `update_drawing`, `delete_drawing` | live, mutating | `Drawing::new`, `sanitize` | Explicit `symbol` and `all_symbols = false`; reject anchors in the future or beyond loaded bars; a stale id returns a structured error |
| `add_indicator`, `remove_indicator` | live, mutating | `Spec::new`, `toggle_indicator`, `remove_study` | Validate against the indicator catalog |
| `compare` | live, mutating | `add_comparison`, `set_compare` | |
| `screenshot` | live | `capture_export` and `chart_export::crop_png`, returned as `inputImage` | One-time consent; a per-turn indicator |
| `read_user_actions` | live | a new edit event log | |
| watchlist tools | live, mutating | `create_watchlist`, `add_watchlist_symbols` | Confirmation required |
| alert tools | live, mutating | `price_alerts::Book::add`, `set_rule`, `save` | Confirmation required |

No broker, position or order tools in v1. Tool output is treated as data, because news,
symbol names, imported drawing labels and alert text are untrusted.

## Phases

### A0: Codex spike

- Branch: `codex/agent-spike`
- Time box: 2 days
- Throwaway and headless; nothing merges into the app.

Build a small Rust binary that does the following:

1. Spawns the seatbelt-wrapped private app-server with the isolation config above.
2. Runs `initialize` with `experimentalApi` and checks that `account/read` returns a ChatGPT account.
3. Starts a thread with two stub dynamic tools and `environments: []`:
   - `read_chart`, returning canned JSON;
   - `read_data`, returning canned bars.
4. Sends "describe the trend".
5. Replies to tool calls with text and with an `inputImage` PNG.

Acceptance criteria:

- At least one `item/tool/call` reaches our tool.
- Both reply types are accepted.
- `turn/completed` arrives with an `agentMessage`.
- The canary turn produces zero disallowed item types.
- `turn/interrupt` returns in under 1 s.
- The JSON Lines transcript is saved as a test fixture.
- Token and rate-limit deltas are logged.

Go/no-go: any disallowed item type stops the project until it is fixed.

### A1: tool registry and app prerequisites

- Branch: `codex/agent-tools`

Work:

1. **Drawing identity.** In `crates/chart/src/drawings/mod.rs` (`Drawing`), add
   `id: u64`, `created_by: Author { User, Agent }` and `turn_id: Option<String>`.
   Use serde defaults and assign ids on load.
2. **Undo by id.** Undo agent edits by id or by inverse operation, not by restoring a
   whole snapshot, so user edits made during a turn survive. Make `ClearDrawings` take
   an undo snapshot.
3. **Chart getters.** In `app/src/chart_view.rs` (`ChartState`), add read accessors for
   bars, the viewport and the visible `DateRange`.
4. **Worker requests.** In `app/src/worker.rs`, add agent request and response variants
   that aren't tied to a pane: history, quotes, search, indicator evaluation.
5. **The `logic()` drain.** In `app/src/app.rs`, add `fn logic` with the agent tool-queue
   drain, deadlines and repaint wake-ups.
6. **Registry crate.** Create `crates/agent-tools` with the registry, schemas and the data
   tools. Add the support/resistance level finder.
7. **Edit event log.** Record user drawing and view edits for `read_user_actions`.

Acceptance criteria:

- Old layout fixtures load.
- Tool calls complete with the window hidden (test).
- Tool schemas are covered by snapshot tests.
- The analysis tools have golden tests on fixed bar fixtures.

### A2: Agent pane on Codex (read-only)

- Branch: `codex/agent-pane`

Work:

1. **Backend crate.** Create `crates/agent` with the `AgentBackend` trait and the
   `CodexAppServer` client: process supervision, JSON Lines routing by thread, turn,
   item and request id, `detect()` states and login.
2. **Agent pane.** Add a `PaneKind::Agent` variant in `app.rs`, a `PaneSettings::Agent`
   variant in `layouts.rs`, pane rendering in `app/panels.rs`, and a `NEW AGENT` panel
   in `app/workspace.rs`. The pane provides:
   - backend, model and effort pickers;
   - the streamed reply, coalesced to about 30 Hz;
   - markdown rendering (egui_commonmark or a minimal renderer);
   - tool trace rows;
   - interrupt and retry;
   - a message queue;
   - suggested prompts;
   - a chart pin;
   - a rate-limit meter;
   - a privacy disclosure of what leaves the machine;
   - "not investment advice".
3. **Keyboard focus.** The chat input captures keyboard focus so chart hotkeys don't fire.
4. **Tools.** Read tools, `additionalContext`, and screenshots with consent.

Acceptance criteria:

- CI replays the A0 transcript through a fake app-server, and the pane renders deterministically.
- Live, "what's the trend" cites real loaded bars.
- Each onboarding state shows one action.
- A killed app-server recovers through `thread/resume`.

### A3: agent edits the chart

- Branch: `codex/agent-edits`

Work:

- The drawing, indicator, comparison and `set_view` mutation tools.
- Highlight agent-made objects.
- "Undo agent turn".

Acceptance criteria:

- The agent draws a support level and a trend line, and adds an indicator.
- "Undo agent turn" keeps the user's own edits made during the turn.
- A stale id returns a structured error.

### A4: MCP shim and mimo

- Branch: `codex/agent-mcp-acp`

Work:

- A `terminal-rs mcp-stdio` subcommand using `rmcp`, bridging to the app over a 0600
  Unix socket. The read-only toolset is the default.
- An `AcpBackend` for `mimo acp`. `session/new` passes `mcpServers` and `cwd`.
- Lock mimo down with `--pure` and permissions denying bash, edit and webfetch.

Check before building:

- whether `mimo acp` is stdio-only (its help also lists `--port` and `--no-auth`);
- how it exposes model and effort (variant). If it doesn't, use `mimo serve`.

Acceptance criteria:

- Claude Code lists the tools through the shim.
- mimo completes the A2 prompt.
- mimo passes the item tripwire and the canary.

### A5: more tools

- Branch: `codex/agent-workflows`

Work:

- Watchlist and alert tools, with confirmation.
- Scanner and multi-chart tools.
- mmx stays deferred. If it is added later, call the MiniMax Messages API directly
  (this needs the reqwest `stream` feature) rather than a CLI per step.

## Testing and evaluation

- **Unit tests.** Golden tests for the analysis tools, snapshot tests for the schemas,
  and fixture tests for old layouts.
- **Protocol tests.** A fake app-server, and later a fake ACP agent, replaying recorded
  JSON Lines transcripts in CI.
- **Manual evaluation.** 10-15 prompts per backend with tool-trace assertions. These
  are not run in CI, because each run uses subscription quota.

## Risks and open items

- **Experimental APIs.** Codex app-server and dynamicTools are both experimental.
  Mitigations: pin the Codex version range, commit the schema snapshot, run the canary
  per version, and keep MCP as the fallback.
- **Data licences.** Sending vendor bars or news to OpenAI or MiniMax may breach
  redistribution clauses, especially for equities feeds. Audit each data provider's
  terms before release. MiniMax is China-hosted and needs separate disclosure.
- **Account terms.** Driving the user's own Codex login through app-server is the
  documented harness use. Never ship, proxy or share credentials.
- **Quota.** Every tool round trip is a model request. Without the budget and meter,
  the 5-hour window can be exhausted quickly.

## Integration references

| Area | Location |
|---|---|
| Command dispatcher | `app/src/app.rs`, `fn execute` (about line 1381) |
| UI entry point | `app/src/app.rs`, `fn ui` (line 2411) |
| Chart state | `app/src/chart_view.rs`, `ChartState` (line 321) |
| Drawings | `crates/chart/src/drawings/mod.rs`, `Drawing` (line 148) |
| Indicator registry | `crates/core/src/indicators/registry.rs`, `Spec` (line 256) |
| Worker | `app/src/worker.rs`: `Request` (line 77), `Response` (line 336), `Shared::send` (about line 605) |
| Data provider | `crates/data/src/lib.rs`, `MarketData` (line 278) |
| Secrets | `crates/broker/src/secrets.rs`, `SecretStore` |
| Price alerts | `crates/core/src/price_alerts.rs`, `Book` |
| Chart export | `app/src/chart_export.rs`, `crop_png` |

## Integration review (2026-10-22, against `personal/main` at `a057b35`)

**Verified.** Every function named in the tool registry and integration
references exists (`capture_export`, `crop_png`, `apply_linked_range`,
`add_comparison`, `set_compare`, `toggle_indicator`, `remove_study`,
`create_watchlist`, `add_watchlist_symbols`, `date_range`, `levels::pivots`,
`volume_profile`, `anchored_vwap`, `Drawing::sanitize`). `App::logic` is not
implemented and `Drawing` has no `id`, as the plan states. `codex-cli 0.163.0`
and `mimo` are installed. Line numbers in the references table will drift; the
names are authoritative.

**Corrections and additions.**

1. **Hidden-window drain is a prerequisite, not just the tool queue.** Worker
   responses (history, live bars, indicator results) are applied only in
   `TerminalApp::drain`, which is called from `ui()`. While the window is hidden,
   a `set_view` followed by `read_data` would wait for history that never reaches
   the chart and time out. A1 must move `drain()` into `logic()` (check that the
   chart apply paths need no `Ui`; GPU upload already happens at paint time) and
   re-measure hidden-window CPU, since the P2 measurements showed that work while
   hidden is easy to regress.
2. **Sandbox profile details for A0.** Codex applies its own Seatbelt profile to
   commands it runs; nesting `sandbox-exec` fails. With the shell tools disabled
   this should not arise, but A0 must confirm it. The profile must also allow the
   loopback listener for the ChatGPT login callback, Keychain access for
   `secret_auth_storage`, and writes inside the private home (sessions, logs).
3. **Recorded transcripts are fixtures in git.** Scrub account ids, emails,
   tokens and rate-limit identifiers from the A0 transcript before committing it,
   in line with the repository's no-secrets rule.
4. **Repaint cost while streaming.** The P1b profile shows that egui frame
   building dominates foreground CPU. Stream coalescing at about 30 Hz must apply
   only while a turn is streaming, with no timer repaints when idle; measure CPU
   during a streamed reply in A2.
5. **Undo by id replaces today's snapshot undo** (`ChartState::undo_drawings`).
6. **The read-only MCP server** referred to as an existing backlog item was not
   tracked in the TC2000 backlog; it is tracked there now as part of A4.
7. **Login is interactive.** A0 needs the user to sign in once inside the private
   Codex home (browser flow). Credentials never pass through chat or the repo.

**Prerequisites from the existing backlog.**

| Item | Needed before | Why |
|---|---|---|
| F01 display names for crypto keys | A2 | The agent and the user should see the same instrument names (`BTC/USD`, not `COINBASE:BTC-USD`) |
| F02 add-to-named-watchlist from every menu | A5 | Watchlist tools need the same "add to a specific list" path |
| T02 formula parity (remote agent) | Not blocking | Improves trust in `indicator_values` |
| P3–P6 performance | Not blocking | Deferred behind A0–A3; repaint work (note 4) may be pulled in if A2 measurements need it |

**Order.** A0 (go/no-go) → A1 (split into A1a app prerequisites: `logic()`
drain, drawing ids and undo by id, chart getters, edit log; and A1b the
`agent-tools` crate, data tools, level finder and golden tests; these can run as
parallel workers once the tool schema types are agreed) → A2 → A3 → A5 → A4.
A5 (watchlist and alert tools inside the app) is placed before A4 (MCP shim and
mimo backend) because the in-app agent is the priority; A4 can move earlier if
external clients are wanted sooner.
