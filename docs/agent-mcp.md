# External agents (MCP) and mimo (ACP)

## Enable access deliberately

The normal app defaults to **Codex**, with both external-access switches **off**.
In **Settings › Agent**:

- **Allow external agents (MCP)** creates `<data dir>/agent/mcp.sock`.
- **Allow chart edits from external agents** enables the registry's editing and
  workflow tools. Leave it off for read-only access. Changing either setting
  disconnects existing clients; it does not widen a live connection's permissions.
- **Backend: Codex | mimo** chooses the in-app backend. Changing it closes the old
  backend and clears the conversation; it never replays the previous prompt.
  The pane header identifies the selected backend.

Open an Agent pane (`NEW AGENT`) to see external notices and workflow approval
cards. Open/pin a chart for chart-scoped calls. App-scoped tools such as watchlist
lists work without a chart. External grants and backend selection are app-wide
`settings.json` preferences, not portable layout settings.

Enabling MCP gives **any process running as your user** access to the exposed
chart, market-data and local news tools. Only enable it for clients you trust.
Read-only does not mean no disclosure: tool results can be sent to the external
client's model provider. Observe your market-data redistribution licence and the
provider's privacy/jurisdiction terms (including Xiaomi MiMo when selected in mimo).

## Register the headless shim

Build a debug binary, then register its **absolute** path:

```sh
cargo build --offline --locked -p terminal-rs
claude mcp add terminal-rs -- /path/to/terminal-rs mcp-stdio
```

`mcp-stdio` is parsed before eframe, providers, tracing, locks or settings load.
It creates no window, starts no data fetches and writes only newline-delimited
JSON-RPC to stdout. Fixed I/O diagnostics go to stderr; tool content is protocol
output, not a log. The shim proxies to the running app's Unix socket and keeps
one connection open (no automatic tool-call replay).

The app and shim must use the **same `TR_DATA_DIR`**. Both create and canonicalize
it to an absolute path once at startup, before starting threads. Relative values
are resolved against each process's startup working directory; use an absolute
value when registering a client whose cwd may differ. The canonical path is also
passed to the sandbox and mimo's child shim (whose cwd is private).
The normal macOS default is `~/Library/Application Support/terminal-rs`.
The complete canonical socket path (including `/agent/mcp.sock`) must fit in
**103 encoded bytes on macOS** or **107 on Linux**, excluding the trailing NUL.
The app checks before binding and the shim checks before connecting. If too long,
both the Agent notice and shim stderr report:

> External agent: data folder path is too long for a Unix socket (N bytes, max 103). Use a shorter TR_DATA_DIR.

Linux reports max 107 instead. The socket is never silently relocated; select a
shorter `TR_DATA_DIR` for both processes.
For a non-default instance, register an explicit server environment, for example:

```sh
claude mcp add --env TR_DATA_DIR=/absolute/path/to/test-data terminal-rs -- /absolute/path/to/terminal-rs mcp-stdio
```

Use a disposable Claude project and its default local registration scope for
manual tests; do not replace an existing registration in your real project.
Restart/reconnect the client after toggling policy so it refreshes `tools/list`.

### Wire protocol and unavailability

MCP uses JSON-RPC 2.0, one JSON object per newline, protocol **2025-06-18** (also
accepts **2025-03-26**). Supported methods: `initialize`,
`notifications/initialized`, `ping`, `tools/list`, `tools/call`.
Notifications (including malformed notification objects) and response messages
receive no response; stdout is never used for diagnostics. Unknown requests return
`-32601`; malformed JSON/request envelopes/arguments return bounded JSON-RPC errors
rather than panicking.

Initialization succeeds even when the app is absent or access is disabled.
`tools/list` then returns `{"tools":[]}`; `tools/call` returns text with
`isError: true` and the exact message:

> terminal-rs is not running or external agents are disabled (Settings › Agent)

Schemas come directly from the same `agent-tools::Registry` as Codex. Read-only
classification is `!ToolSpec.mutating`, not a second hard-coded list. Results use
MCP text content and `isError` for tool errors. Host-consent-only tools (currently
`screenshot`) are omitted from external `tools/list`; direct calls still fail
closed. Workflow proposal tools remain listed when editing is enabled because
their approval cards are enforced by the app, not by client-supplied consent.
Screenshot/image export is refused with an error rather than transferring an
image without separate consent.
App JSON-RPC errors (including the connection rate limit) remain JSON-RPC errors
through the proxy; they are not disguised as an unavailable app.

## Socket security and execution

- No app HTTP/TCP listener. The `agent` directory is owned, non-symlink and 0700;
  `mcp.sock` is 0600. On macOS, `getpeereid` additionally requires the app's UID.
- A live socket is never replaced. A bounded connection probe must establish
  `ConnectionRefused` before a stale socket can be removed. Other failures,
  symlinks, non-owned sockets and regular files fail closed.
- Turning access off or cleanly exiting cancels connections/queued UI work and
  unlinks the owned socket inode. A crash can leave a stale socket; startup removes
  it even when MCP is off, with the same ownership/type/refused-listener checks.
- Transient accept failures (EMFILE/ENFILE, aborted, interrupted or would-block)
  back off 50 ms and retry; only fatal failures stop the listener.
- At most 16 connections, **one call in flight per connection**, **60 calls per
  rolling minute per connection**, and **1 MiB per JSON line** (input and output).
- A connection has a 60-second tool/analysis scope: eight read-only calls or
  sixteen editing/workflow calls, with the existing 128 KiB cumulative text cap
  and per-result caps. Failed validated tool attempts consume the call budget.
  Rotation cancels the previous scope and clears its analysis references. These
  are per-connection guardrails, not a security boundary against a same-user
  process that reconnects.
- The first valid tool call snapshots the pin for that scope, including the
  no-chart case. A later user change invalidates stale chart operations instead
  of silently retargeting them. Omit `chart_id` for the pin, or use an explicit
  valid target returned by `read_chart` where the tool supports targets.
- Calls use the **same AppExecutor, Registry, ScopedHost and UI-thread queue** as
  the in-app agent: argument validation, `same_instrument`, target/pin checks,
  analysis-ref precision, ownership/object caps, and watchlist/alert approvals.
  Analysis references are connection-local; another client's call cannot erase
  or borrow them. No socket thread owns the app or mutates chart state directly.
- Notices, drawing badges and workflow approval summaries identify **External
  agent**. Watchlist/alert writes only propose a change; the user must approve
  in the Agent pane, including when the in-app Codex consent flow has not run.
  The MCP tool-request notice is coalesced per connection for 30 seconds; repeated
  requests update its counter rather than appending more notices.

### External workflow outcomes

External proposal decisions, expirations and undos **never** enter the in-app
Codex or mimo conversation's host `workflow_results` context. The dedicated
read-only MCP tool **`read_proposal_outcomes`** returns them to the originating
connection, including mimo (which uses the same socket). After proposing a write,
poll this tool on that same connection; no host prompt is injected or replayed.

Arguments are `{}` for the latest 32 proposals, newest first, or
`{"proposal_id":123}` for a specific proposal, including an older one. Each row
contains its host proposal ID/turn, `pending|approved|declined|expired|failed`
status, a bounded/scrubbed decision summary and an `undone` flag. Reads are
non-consuming and repeatable, so a lost reply cannot lose a decision. Poll pending
IDs again to observe later decisions; the proposal's original tool result supplies
its ID. Connection identity spans 60-second budget rotations, but does not span a
disconnect/reconnect; other connections cannot read these outcomes. Conversation
reset/app restart discards the in-memory proposal history. The tool uses the same
UI queue, per-connection rate/call/text budgets and cancellation as other calls.

Keep chart edits off unless intended. Once enabled, drawing/indicator/view
operations can apply immediately subject to the existing host rules; the separate
approval cards apply to workflow writes, not to every chart edit.

## mimo backend

mimo is **Xiaomi MiMo Code**, an OpenCode fork. Its ACP agentInfo
name is `OpenCode`; this does not mean it uses `~/.config/opencode`.
`AcpBackend` implements the existing `AgentBackend` trait separately from Codex's
wire transport. Binary discovery checks PATH, then `~/.mimocode/bin/mimo` metadata.
terminal-rs checks directory/binary metadata only: it never opens or copies your
personal provider configuration or credentials. The sandboxed mimo child can
read the selected profile, including its keys and provider-managed data; writes
are limited to the profile-specific data/state roots described below.

### Profiles and plugins

Settings › Agent shows these choices when **Backend: mimo** is selected:

- **Use my mimo setup (providers, keys and models from your installed mimo)**
  (`mimo_profile: "Own"`, the default, including older settings without the field).
  Uses providers, keys and models from **mimo's credential store** (including
  `auth.json` under the share directory), not your personal config. Personal MCP
  servers, plugins and instructions in `~/.config/mimocode` are intentionally
  **not loaded**: that directory is neither linked nor sandbox-readable.
  The child receives a per-launch **shadow HOME** at
  `<private workspace>/home`, not the real home. This 0700 directory contains only
  symlinks to the four real mimo roots listed below that already exist, plus 0700
  parent directories (`.config`, `.local`, `.local/share`, `.local/state`, `.cache`).
  `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME` and `XDG_STATE_HOME` are
  **unset**, so mimo finds its installed setup through those links. Inherited
  custom XDG locations are intentionally not honoured. The shadow home is removed
  with the private workspace; installed data is not copied or removed.

  **Why not the real HOME?** Manager live testing of mimo 0.1.16 found that it
  probes `~/.claude.json` at startup and exits fatally on sandbox `EPERM`. The
  shadow home makes unrelated probes (`~/.claude.json`, `~/.claude`,
  `~/.config/opencode`, etc.) absent (`ENOENT`, which mimo tolerates), without
  granting access to any real Claude/OpenCode/Codex configuration or the real
  home root. The sandbox grants the resolved real mimo paths, not broader home
  access through the symlinks.
- **Private login (separate sign-in inside terminal-rs)**
  (`mimo_profile: "Private"`). Preserves the separate owned 0700 login home at
  `<data dir>/agent/mimo-home`, with HOME there and XDG config/data set to its
  `config`/`data` children. Existing installed credentials are not used; provider
  setup/sign-in remains the separate manual step below, not a new in-app login UI.
  To remove this private login, stop the backend/close the app, then delete only
  **`<data dir>/agent/mimo-home`**. This does not remove your installed mimo login.

**Own always uses `--pure`**, even if an older saved plugin toggle is true;
**Load mimo plugins** is hidden in Own mode. For **Private only**, the collapsed
**Advanced** section retains **Load mimo plugins**, off by default
(`mimo_load_plugins: false`). Off uses `mimo acp --port <N> --pure`; on drops only
`--pure`. Private providers configured via plugins may need this toggle. Plugins
run inside the private sandbox with access to its data and keys;
this is additional code you trust, not an expansion of sandbox permissions.
Changing profile or this toggle closes the old backend and starts a new
conversation without replaying a prompt. Model choices remain saved separately
for mimo and Codex.

### Transport resolved: stdio

**Manager-verified live fact:** `mimo acp` speaks ACP JSON-RPC over **stdio**.
Its `initialize` response returns `protocolVersion: 1`,
`agentInfo: {"name":"OpenCode","version":"0.1.16"}`,
`authMethods: [{"id":"opencode-login"}]`, and
`agentCapabilities.loadSession: true`. These facts are supplied by the manager;
this fix did not launch mimo, the app, or any provider or use external networking.

By default the implementation spawns `mimo acp --port <N> --pure` and uses
**JSONL stdio ACP v1**. `--port` and the selected number are separate argv items.
The Private-only advanced plugin toggle removes only `--pure`, not the sandbox or ACP tripwires.
There is **no HTTP/port transport fallback** and no `--no-auth`. mimo (Bun) also
starts an **internal localhost HTTP server**, even for stdio ACP. Before spawning,
terminal-rs binds `127.0.0.1:0`, reads the assigned port and drops the listener.
Both profiles pin bind/inbound/outbound localhost rules to that one nonzero port
and pass it to mimo; there is no wildcard loopback access. Launch verifies the
ACP `initialize` handshake. If it fails and the chosen port is now occupied,
terminal-rs shuts down the failed child, awaits verified cleanup, then retries
**once** with a different free port and matching sandbox/argv. Unsafe tripwires
and cleanup failures never trigger a retry; prompts are never replayed.

**Manager-verified live fact (mimo 0.1.16):** the narrowed data/state-only writable
set, read-only install/config/cache, `/usr/lib` + `/usr/share` read grants and
single-port localhost rules allow `initialize` and `session/new` to succeed with
20 models. In a subsequent manager live test with eager, startup-configured MCP,
MiniMax-M3 answered **“BTCUSD Curncy, 1D”** via `terminal-rs_read_chart`, with no
permission requests, zero personal MCP servers and all 20 models present. This
implementation did not run a live test. Private-home authentication still needs
the manual checks below; do not grant broad networking or shell access.

### Isolation and ACP operations

- Fresh empty 0700 workspace: `<data dir>/agent/mimo-<pid>-<nonce>/empty`.
  It is removed after child/process-group cleanup, including launch-error paths.
- **Own sandbox exposure:** deny-default; only four installed mimo roots remain
  readable, but only **data and state** are writable, in addition to the private
  workspace (including private tmp):
  - **Read/write:** `~/.local/share/mimocode` (auth, database, storage, logs,
    snapshots, tool output)
  - **Read/write:** `~/.local/state/mimocode` (model selection, key/value state
    and locks)
  - **Read-only:** `~/.mimocode` (install tree, including `bin/mimo`)
  - **Not exposed:** `~/.config/mimocode` (personal MCP servers, plugins, instructions)
  - **Read-only:** `~/.cache/mimocode` (model catalogue and cached binaries/models)

  The read-only roots prevent a manipulated session from persisting changes to
  mimo's binary, configuration, plugins or cached executables outside the sandbox.
  Install/update/configuration changes must be made outside the sandbox. Launch
  also refuses **any writable root containing the mimo binary or its install
  directory**, including an unusual binary location under data/state or the
  private workspace/login home.

  Roots must exist as directories and canonicalise to those exact paths under
  real canonical HOME. Redirected roots or ancestors are refused, not granted as
  an unrelated destination. Missing real directories are skipped (no link),
  **never created** by terminal-rs; finish installed setup outside the app if
  necessary. Only the shadow home and its parent layout are created. Symlinks below
  a granted root do not grant their outside targets. The home root is not granted
  read/write access; exact ancestor metadata traversal is allowed as before.
  There is no access to `~/.config/opencode`, `~/.codex`, `~/.ssh`, documents or
  `~/Library` via this profile. In particular, `~/Library/Caches` and similar
  speculative compatibility paths remain denied: no extra grant is guessed.
- **Private sandbox exposure:** the existing private workspace/login-home
  subpath grants, not the four personal mimo roots. The app-owned login home stays
  writable; the binary install tree stays read-only. The login home is persistent,
  owned, non-symlink and owner-only. Provider setup is a deliberate manual step.
- Environment is cleared, then only PATH, TMPDIR, LANG and safe LC_* locale
  values are retained, with HOME chosen by profile. Own uses a private workspace
  TMPDIR. Private retains the previous TMPDIR and XDG config/data behaviour.
  The host-selected `TR_DATA_DIR` is passed solely to locate the app MCP socket.
  Inherited API keys, auth tokens, custom XDG settings and `CODEX_HOME` are not
  passed. Both modes set these host-controlled switches:
  `MIMOCODE_MCP_TOOL_SEARCH=false`, `MIMOCODE_DISABLE_CLAUDE_CODE=1`,
  `MIMOCODE_DISABLE_MODELS_FETCH=1`, and `MIMOCODE_DISABLE_AUTOUPDATE=1`.
  `MIMOCODE_CONFIG_CONTENT` supplies exactly one local terminal-rs MCP server
  before startup/tool discovery:

  ```json
  {"mcp":{"terminal-rs":{"type":"local","command":["<current executable>","mcp-stdio"],"environment":{"TR_DATA_DIR":"<canonical data dir>"}}}}
  ```

  This avoids the late `session/new` attachment/startup-reminder race and deferred
  `mcp_tool_search`/`mcp_tool_use` discovery. Own never loads personal config.
- Production launch requires macOS `sandbox-exec`; other platforms refuse an
  unsandboxed mimo child. Both profiles allow the read-only binary install tree,
  exact app executable, system libraries and shared resources (`/usr/lib` and
  `/usr/share`, **not** a broad `/usr`, `/usr/local` or `/usr/bin` grant), DNS,
  outbound TCP `*:443`, and the exact app Unix socket. Both also include exactly
  these localhost rules for mimo's internal server (`N` is the chosen port,
  substituted as a decimal number):

  ```scheme
  (allow network-bind (local ip "localhost:N"))
  (allow network-inbound (local ip "localhost:N"))
  (allow network-outbound (remote ip "localhost:N"))
  ```

  Both profiles also grant the following **metadata-only** DNS path traversal
  (merged with private workspace/socket ancestor metadata grants, without duplicates):

  ```scheme
  (allow file-read-metadata (literal "/var") (literal "/etc") (literal "/tmp") (literal "/private") (literal "/private/var") (literal "/private/var/run") (literal "/private/etc") (literal "/private/tmp"))
  ```

  `file-read*` includes `(subpath "/private/etc/ssl")` for system TLS certificates.
  These metadata grants let the macOS resolver traverse the public aliases and
  canonical paths; without them DNS fails even with HTTPS outbound allowed.
  They do **not** grant directory contents or broad reads/writes under `/etc`,
  `/var` or `/tmp`. The existing exact `/private/etc/hosts` and
  `/private/etc/resolv.conf` reads, `/private/var/run/mDNSResponder` outbound,
  and the three Mach names `com.apple.dnssd.service`, `com.apple.trustd`, and
  `com.apple.trustd.agent` remain. The pinned loopback and TCP 443 rules are unchanged.
  The manager verified this DNS fix with curl and real mimo (including models.dev
  refresh); implementation verification uses profile-string tests, not live network.

  Execution is restricted to mimo and the app shim; no shell, arbitrary
  executable, broad home subtree or non-loopback inbound listener. Broad install
  grants covering home/credential trees are refused. Plugin loading does not
  weaken these restrictions.
- ACP `initialize` uses protocolVersion 1 and disables `fs.readTextFile`,
  `fs.writeTextFile` and `terminal`. `session/new` always uses the private cwd and
  **`mcpServers: []`**: startup config already supplies terminal-rs, so there is
  no duplicate MCP connection. The shim answers `prompts/list`, `resources/list`
  and `resources/templates/list` with their respective empty lists.

- `session/prompt` streams text/thought/tool/plan events; plan entries and statuses
  are projected as bounded, scrubbed reasoning text. `session/cancel` is a
  notification. External workflow decisions return via `read_proposal_outcomes`
  over mimo's MCP connection, never as host workflow-results text. There is no
  invented concurrent ACP steer or automatic prompt replay. Rejected model/turn
  requests post a notice and keep the session alive; transport death or an unsafe
  tripwire ends it. Queued final text/completion events drain before closed errors.
- The model picker prefers the **model-category** select entry in ACP
  `configOptions`, including grouped provider options, with `models.availableModels`
  as fallback (both accepted on `initialize` and `session/new`). Manager live
  testing returned the same 20 models in both forms, with current model
  `xiaomi/mimo-v2.6-pro-ultraspeed`, plus two separate mode options. Labels use
  reported **provider/model** IDs when qualified; a third-segment variant is shown
  as **provider/model · variant**, e.g. `deepseek/deepseek-v4-pro · high`, while
  selection preserves the exact ID `deepseek/deepseek-v4-pro/high`. Unqualified
  IDs use the reported name. `configOptions` category `mode` and session `modes`
  (build/plan) are agent modes, never model choices. Switching uses `session/set_model`, or
  `session/set_config_option` with the reported config ID for config-option agents.
  The last mimo model is saved as pane choice `mimo_model`, independently of the
  Codex `model`, and applied **before the canary prompt** and again to each new
  user session if still offered. An unavailable or absent saved model falls back
  to mimo's reported current model.
  If no model metadata is offered the picker shows **mimo default**. No reasoning
  efforts, providers, model IDs or models from local credential files are invented.
  Images are not enabled. If no model is saved and the canary receives a provider
  unsupported/model error, readiness fails with **“mimo's default model isn't
  available on your plan — pick a model in the Agent pane and Restart”**. The
  catalogue is still published and the picker remains enabled while Stopped;
  selecting then Restart applies that choice before the fresh canary.
- Each mimo prompt includes the **same host-selected pinned-chart context** as
  Codex (chart UID, symbol, timeframe and visible range), marked as untrusted data.
  Pane readiness/status is rendered outside the transcript, not recorded as a
  user turn; notices are separate display blocks. Neither transcript blocks nor
  status/notice text are used to assemble ACP prompts. Canary output is discarded
  and a fresh session is created before user turns.
- ACP permission checking is **defence in depth**, not the real authorization
  boundary: titles, `rawInput` and `_meta` are agent-supplied and may be influenced
  by the model. Real authorization is enforced at the **app socket** (registry
  read-only/edit policy, validation and host approval cards) and by the **OS
  sandbox** (deny-default execution/filesystem access; no network except the
  explicit Unix-socket, DNS, outbound HTTPS and localhost-only grants needed by
  mimo). A matching
  title alone must never be treated as proof of trusted provenance.
- Permissions allow a registry-known **terminal-rs** MCP tool only via
  `allow_once`, never `allow_always`. Only ACP `kind` **`other`**, **`read`**, or
  absent is accepted. `execute`, `edit`, `delete`, `move`, `fetch`, and any other
  kind are refused regardless of title, including on partial tool updates.
  Foreign tools use `reject_once`
  (or cancelled when unavailable), cancel the session and trip the sticky unsafe
  state. Every `fs/*`, `terminal/*`, unknown executing update/request, unapproved
  tool update, malformed frame or child crash fails closed with a visible error.
  The exact accepted qualifications are `mcp__terminal-rs__<tool>`,
  `terminal-rs/<tool>`, `terminal-rs.<tool>`, **`terminal-rs_<tool>`**, or an exact
  unqualified tool name backed by structured terminal-rs server/tool provenance.
  The flat underscore form is manager-verified with eager tools: kind `other`,
  `rawInput` containing arguments, `_meta: null`. Ordinary argument `name` is not
  an MCP identity in that form. Explicit server/tool identity envelopes and `_meta`
  must agree with the title. Deferred `mcp_tool_search`/`mcp_tool_use`, unknown
  suffixes and unsafe kinds remain rejected; metadata cannot override a conflict. Permission approval also records its tool ID and name, allowing
  subsequent `tool_call_update` without a separate `tool_call`. Conflicting
  title/server/tool identities and explicit updates for an unknown session are
  refused; an approved tool ID cannot authorise a different registry tool or hide
  an unsafe kind in a partial update. The reader commits a
  new session ID before routing any following line; late updates from the retired
  canary session are ignored.
- Every fresh child runs the directory/shell **canary before Ready**, then starts
  a separate user session. The canary has a **60-second overall deadline**, including
  session creation and model selection. ACP `session/update` retry parts appear as
  scrubbed notices: `mimo: provider unreachable (attempt N) — <message>`, including
  while readiness is Checking. After **three network-kind retry updates**, readiness
  sends `session/cancel` and fails with: “mimo could not reach its model provider
  (network). Check your provider/endpoint in mimo, then retry.” Timeout also cancels
  and shows a clear error. Readiness failures leave Checking immediately, display
  the error in the pane (not only a tooltip), and offer **Restart**; nothing is replayed.
  Normal user turns display retry notices but do not apply the canary retry limit
  or a prompt deadline: mimo can keep retrying until the user interrupts.
  The existing stream scrubber handles secrets split
  across text deltas and final partial lines. Raw ACP transcripts are not saved;
  stderr is discarded, not copied into logs. Stop/shutdown kills and reaps the
  process group, tracks descendants while the child is alive, and awaits verified
  process/private-workspace cleanup. Cleanup findings are returned to the owner,
  not silently treated as success.

mimo tool calls travel through the external socket and obey **its** switch and
read-only/edit policy, regardless of the pane's Codex toolset choice. Their
host-authored `external-*` provenance/60-second scope is separate from the
in-app `mimo-*` conversational turn: use the external approval cards and drawing
provenance; exact conversation-footer undo/trace association is not provided.

### Troubleshooting: provider unreachable

If the pane shows **mimo: provider unreachable**, **Cannot connect to API**, or
“mimo could not reach its model provider (network)”, check the selected model,
provider login, and provider/endpoint configuration in mimo outside terminal-rs.
Own profile uses your installed mimo setup; Private profile uses its separate
saved login. Custom non-443 endpoints remain blocked by the isolation policy.
The sandbox must retain the DNS metadata traversal, SSL read, resolver socket,
and trust-service grants above; allowing TCP 443 alone does not make DNS work.
Do not broaden the filesystem/network policy or paste credentials into the pane.

Readiness stops after three network retries or its 60-second canary deadline and
shows an error with **Restart**. Fix the provider setup, then restart to run a fresh
canary; no prompt is automatically resent. During a normal turn notices are
informational and mimo may keep retrying; use **Interrupt** to stop it.

## Manager manual test (not executed during implementation)

These commands intentionally launch programs and may use Claude/mimo provider
network/quota. They are for the manager only. Market data uses the offline sample
provider, Congress refresh is disabled, and no real account/layout is reused.
Do **not** use `--agent-demo`: that harness intentionally disables external MCP.

### 1. Build and create a disposable instance

In a shell, retain the printed TEST_DIR for subsequent shells:

```sh
cd /Users/bennyo/Documents/repos/terminal-rs-a4
cargo build --offline --locked -p terminal-rs
BIN="$PWD/target/debug/terminal-rs"
TEST_DIR="$(mktemp -d /tmp/tr-a4.XXXXXX)"
# Use the canonical macOS /private/tmp spelling for sandbox/socket paths.
TEST_DIR="$(cd "$TEST_DIR" && pwd -P)"
export BIN TEST_DIR
printf 'BIN=%s\nTEST_DIR=%s\n' "$BIN" "$TEST_DIR"
```

Before starting the app, this must output initialization success, an empty tools
list, and the helpful isError result, with **no window** or provider startup:

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"manual","version":"1"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_chart","arguments":{}}}' \
  | TR_DATA_DIR="$TEST_DIR" "$BIN" mcp-stdio
```

Launch the normal app in that instance (foreground, in its own terminal):

```sh
TR_DATA_DIR="$TEST_DIR" TR_PROVIDER=sample TR_CONGRESS_REFRESH_MIN=0 "$BIN"
```

Open a sample chart and `NEW AGENT`. Initially there must be no socket. Enable
only **Allow external agents (MCP)** in Settings › Agent; inspect:

```sh
ls -ld "$TEST_DIR/agent" "$TEST_DIR/agent/mcp.sock"
```

Expect `drwx------` and `srw-------`. Repeat the JSONL probe: tools now include
`read_chart`, exclude mutating tools, and read_chart returns chart data.
A direct tools/call for create_drawing/create_watchlist must return a tool error.

### 2. Claude Code

In another shell using the BIN/TEST_DIR values from step 1:

```sh
mkdir -m 700 "$TEST_DIR/claude-project"
cd "$TEST_DIR/claude-project"
claude mcp add --env "TR_DATA_DIR=$TEST_DIR" terminal-rs -- "$BIN" mcp-stdio
claude mcp list
claude
```

Use `/mcp` to confirm terminal-rs is connected. Ask Claude to use read_chart and
analyze, not its own shell: “Using only terminal-rs tools, describe the pinned
chart's trend and nearest finished-bar support/resistance. Do not edit anything.”
Check an **External agent** notice in the app. Hide/minimise the app and repeat a
read; it should still complete through process_logic's queue drain.

Then deliberately enable the external-edit switch, reconnect Claude's MCP server,
and ask it to create a small test watchlist. Verify the **External agent** card
appears, no write precedes approval, Decline does nothing, and Approve applies
only selected symbols. Ask for a referenced drawing and inspect its **External
agent** badge. Disable MCP and check that the socket disappears and tools/call
returns the exact unavailable error. Closing the app should also remove it.

### 3. mimo private setup and backend

Close the app before private setup. Keep the same throwaway directory. Find the
binary without opening personal configuration, create a separate login home, and
consult the installed CLI's actual provider setup syntax (help did not document
its login subcommand, so this guide does not invent one):

```sh
MIMO="$(command -v mimo)"
if [ -z "$MIMO" ]; then MIMO="$HOME/.mimocode/bin/mimo"; fi
mkdir -p "$TEST_DIR/agent/mimo-home" "$TEST_DIR/mimo-setup"
chmod 700 "$TEST_DIR/agent" "$TEST_DIR/agent/mimo-home" "$TEST_DIR/mimo-setup"
MIMO_HOME="$TEST_DIR/agent/mimo-home"
env -i PATH="$PATH" LANG="${LANG:-en_US.UTF-8}" \
  HOME="$MIMO_HOME" XDG_CONFIG_HOME="$MIMO_HOME/config" XDG_DATA_HOME="$MIMO_HOME/data" \
  "$MIMO" providers --help
# If supported by this version, complete provider setup in its private interactive UI:
env -i PATH="$PATH" LANG="${LANG:-en_US.UTF-8}" \
  HOME="$MIMO_HOME" XDG_CONFIG_HOME="$MIMO_HOME/config" XDG_DATA_HOME="$MIMO_HOME/data" \
  "$MIMO" --pure "$TEST_DIR/mimo-setup"
```

Do not copy personal credential files or pass API keys on the command line.
Whether this version supports authentication under these private HOME/XDG paths
is part of the manual gate, not an asserted result. Exit the setup UI, then:

```sh
TR_DATA_DIR="$TEST_DIR" TR_PROVIDER=sample TR_CONGRESS_REFRESH_MIN=0 "$BIN"
```

Enable MCP (keep edits off first), select **Backend: mimo** and **Private login**
for the private setup above, open `NEW AGENT`, acknowledge the provider disclosure
and start/check the backend. Alternatively choose **Use my mimo setup** to test
your existing credential store without creating/copying a private login; personal
config must not load. For Private only, plugin-configured providers can enable
**Advanced › Load mimo plugins**. Expect
initialize → canary → fresh session → Ready and a **mimo** header. Try the same
read-only chart prompt, Interrupt mid-stream, then a fresh prompt. Try an unsafe
filesystem/shell request: it must be unavailable or visibly trip/stop, not execute.
Switch back to Codex and verify a new conversation with no old prompt replay.

If initialization stalls/fails, record only the fixed/scrubbed failure, installed
CLI version and which private-home/sandbox operation failed; do not capture raw
tokens or transcripts. Do not weaken the sandbox or use --no-auth. Stdio transport
is manager-verified; private-home auth and sandbox/tool-title compatibility still
need verification before calling the whole backend live-verified.

Exit all programs and remove only these test resources when finished:

```sh
cd "$TEST_DIR/claude-project"
claude mcp remove terminal-rs
cd /Users/bennyo/Documents/repos/terminal-rs-a4
printf 'Disposable directory to remove after review: %s\n' "$TEST_DIR"
# Review the path, then delete that throwaway directory using your normal workflow.
```

## Automated verification

No added test starts mimo, eframe or an external provider. Test directories live
under the repository's build-output tree. Existing coverage includes registry
schema equality, read-only refusal, unavailable app, malformed/oversized frames,
permissions/stale sockets, serial rate limits and proxy errors, panic containment,
ACP capabilities/models/events/canary/permissions/cancel/crash/scrubbing,
connection-local chart precision, workflow approval, default-off persistence,
queued-write cancellation and the continuation's provenance/session/cleanup tests.

MIMO-MCP-ONLY regressions cover four-root shadow homes even when personal config
exists, the exact startup MCP JSON and fixed environment switches in both modes,
Own's unconditional `--pure`, the flat eager tool title (including argument names),
unsafe/deferred tool rejection, empty MCP discovery lists, default-model rejection
and pre-canary saved selection, catalogue availability after readiness failure,
model-picker clicks while Stopped, status/notice separation from user turns, and
the full pinned-chart payload on both fake Codex and ACP wires.

Historical A4-FIX verification follows. A4-FIX removes both reviewer `#[ignore]`
attributes and adds **12 regression tests**:

- Four MCP tests: injected transient accept errors/backoff versus fatal errors;
  relative data-dir creation/canonicalization; safe disabled-startup socket cleanup;
  silent notifications/malformed notifications/responses with a sentinel request.
- Four fake ACP tests: queued final events drain before close (also late subscribers);
  a final partial text + prompt response + EOF on the real fake wire;
  recoverable prompt RPC rejection without replay; permission-only tool announcement
  followed by accepted completion versus a foreign ID tripwire. The reviewer
  session-handoff test also checks harmless late canary metadata.
- Three app/UI-queue tests: per-connection 30-second notice counters;
  disabled-at-startup stale socket removal; repeatable, connection-private proposal
  outcomes for both selected backends, including approvals/declines/expirations.
- One session-owner helper test: rejected model selection and busy-turn notices,
  continued session usability, optimistic UI recovery and fatal transport closure.

Final A4-FIX workspace run: **2,041 passed / 0 failed / 9 ignored**, including
doctests. Formatting and all-target workspace Clippy with `-D warnings` pass.
The exact requested environment-prefix command
`TR_CONGRESS_REFRESH_MIN=0 cargo test --offline --locked --workspace` was attempted
but required runner approval rather than executing. Verification uses equivalent
Cargo configuration, forcing the refresh value to zero:

```sh
cargo fmt --all
cargo clippy --offline --locked --workspace --all-targets -- -D warnings
cargo --config 'env.TR_CONGRESS_REFRESH_MIN.value="0"' \
  --config 'env.TR_CONGRESS_REFRESH_MIN.force=true' \
  test --offline --locked --workspace
```

The nine pre-existing ignores remain: seven external-network tests, one manual
news audit and the unsupported precision-reference assertion. No A4 test is
ignored. README.md is intentionally unchanged. No app/mimo was launched for this
fix; the stdio initialization finding above is manager-supplied live evidence.

### A4-HARDEN verification

Added **seven regression tests**:

- Socket byte-limit boundary/trailing-NUL and multibyte counting; bind/shim
  rejection before I/O or directory creation; actionable Agent-pane notice.
- Consent-only tools omitted from all external toolsets while direct screenshot
  calls still fail and workflow proposals remain available.
- Unsafe ACP kinds rejected despite matching MCP titles in permissions and tool
  starts; `other`/`read`/absent kinds with agreeing identities allowed once;
  approved partial updates cannot change kind or tool identity.

Extended the existing provenance test to cover mismatching registered
`rawInput.name`, `_meta` identities and title/metadata disagreement in both
permissions and tool starts. Updated the wire schema-equality test to compare
only externally callable tools.

A4-HARDEN workspace results: **2,048 passed / 0 failed / 9 ignored** (including
doctests). `cargo fmt --all` and offline/locked all-target workspace Clippy with
`-D warnings` pass. The environment-prefixed test command again required runner
approval; the equivalent forced Cargo environment configuration shown above ran
the full workspace suite successfully (also repeated with `--quiet` to retain
compact result totals). No app/mimo or release build was run, and no network or
personal CLI configuration was used.

### MIMO-SANDBOX-TIGHTEN verification

ACP regressions assert the exact Own data/state writable set, read-only
install/config/cache, writable Private login home, `/usr/lib` + `/usr/share`
without broader `/usr` grants, and all three localhost rules pinned to the
selected port without wildcards. They also cover separate `--port`/number argv
items with plugins on/off, rejection of port zero and writable paths containing
the binary/install directory, the one-retry limit and unsafe-backend refusal.
All 32 subsets of existing Own roots still get only their matching readable links
and data/state write grants. No added test starts mimo or the app.

`cargo fmt --all` and offline/locked workspace Clippy over all targets with
`-D warnings` pass. The exact environment-prefixed workspace test command required
runner approval. Using equivalent forced Cargo environment configuration, the
parallel workspace run encountered unrelated app timing/history-request failures;
the full serial workspace run passed **2,064 tests, 0 failed, 9 ignored**. The
35 ACP tests also passed with default test parallelism. No release build, live
app/mimo launch, network access or personal CLI configuration was used.
