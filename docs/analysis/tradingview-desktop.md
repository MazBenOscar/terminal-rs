# TradingView Desktop: installed-application REA investigation

Inspected: 2026-10-21. This is a **static investigation of the installed Windows
application**, separate from the earlier [public website UX study](README.md).
It identifies desktop integration patterns worth evaluating for terminal-rs;
it does not establish TradingView chart-calculation or Pine compatibility.
The Rust comparison baseline is `cfee48c` on `personal/main`.

A subsequent [quick-tools and Auto/Log investigation](toolbar-scale-followup.md)
adds website runtime evidence for drawing favorites and independent scale modes,
plus indicator-access limits. It is separate from the static desktop pass below.

## Target and reproducibility

Windows package discovery located `TradingView.Desktop`, x64 package version
**3.4.1.8194**, at:

```text
C:/Program Files/WindowsApps/TradingView.Desktop_3.4.1.8194_x64__n534cwy3pjxzj
```

The internal application identifies itself as `tradingview-desktop`, version
`3.4.1`, with entry point `index.js`. These are different version fields, not
conflicting discoveries.

| Artifact | Bytes | SHA-256 |
|---|---:|---|
| `TradingView.exe` | 223,121,408 | `89eb7e0756e15d100711ea4917c1fd04db43dd31695581e5ede5905924c41f1f` |
| `resources/app.asar` | 38,119,804 | `a70a18bfd8f1db8d113993709d8294344545a29b3ccf7f7c3436667907023906` |
| `index.js` inside ASAR | 2,195,113 | `4f7c4e6d470d7f33c9b68dd23afcece3f1a16d6b7b2adc2744dc8748f54e97f3` |
| `index.js.map` inside ASAR | 8,793,884 | `acadbf9212fe5036d7b62bdc08accdf877cefee3e85411f69ed59b3bc23cbfa3` |

The ASAR inventory contains 237 file entries, including unpacked declarations.
REA classified 166 as relevant for the ASAR analyses below. These counts have
different scopes. The main source map contains 1,371 source entries, including
dependencies. We selected 32 desktop-integration TypeScript sources rather than
claiming to have inspected every entry.

Extraction read ASAR offsets and source-map `sourcesContent` as inert data.
Each selected text's hash was checked against its source-map entry. Nothing from
the target was executed. Raw sources, assets and the REA evidence bundle are
kept outside Git; only this independent report is committed.

The local evidence directory (a session scratch folder, kept off the repository)
contains `asar-inventory.json`, `expanded-map-provenance.json` (source ordinals,
paths and hashes), selected source texts, and
`tradingview-desktop-rea-evidence.json` (332,051,241 bytes; 129 retained records).
The export's envelope reports zero unknown records; that is **not** a statement
that the analyses have no semantic unknowns. See coverage below.

## Findings and their limits

**Observed** means present in packaged declarations or inspected source text.
**Inferred** means an architectural interpretation. **Unverified** means runtime
behavior, online services or calculations were not established by this pass.
Source paths below are TradingView source-map paths, not files in this repository.

| Area | Observed evidence | Interpretation and remaining uncertainty |
|---|---|---|
| Desktop/web boundary | Configuration names `https://www.tradingview.com/` as the default host and `chart/` as the chart path. Preloads, browser-view/tab wrappers, navigation rules and page API adapters are packaged. | **Inferred:** Electron hosts online chart pages alongside desktop services. This selected pass does not establish where every calculation runs, nor that the package contains no chart engine. |
| Chart linking | `src/linking/linkable.ts` defines symbol, interval, time and date-range channels. `linking-platform-api-injects.ts` observes page changes and dispatches setters for active-chart symbol, resolution, time and visible range. It also has a screener symbol subscription. | **Observed static contract.** Runtime propagation, loop prevention and screener availability need observation. Default declarations enable symbol and disable other channels; effective user defaults were not read. |
| Link resilience | `tv-tab-linking-agent.ts` buffers updates while the platform proxy is unavailable and gates notifications by focus/available channels; a 500 ms debounce is declared. | A useful pattern for startup/reload/focus races. The declaration does not measure runtime latency or establish a complete renderer-to-main IPC path. |
| Layout state | `layout-platform-api-injects.ts` reads per-chart symbols/resolutions and symbol/interval sync settings. `tv-tab.ts` contains layout restoration/retry and save/close/reload paths. | **Observed static restoration machinery.** Persistence correctness after a crash, migration behavior, cloud/local precedence and multi-window edge cases remain untested. |
| Local persistence | Session-storage modules contain in-memory, on-disk and LevelDB implementations. Packaged configuration declares a 300,000 ms autosave interval. | Backend implementations and a declared default are established; active storage selection, effective autosave cadence and actual account settings are not. No user profile was read. |
| Actionable notifications | `notification/alert-notifier.ts` uses Electron notifications and a sound-player service, handles notification clicks, brings the relevant window forward and changes the active chart/tab symbol. Sound/volume and stop-sound paths exist. | Strong evidence of desktop delivery/navigation integration. It does not establish cloud alert evaluation, app-closed delivery, delivery reliability, subscriptions or entitlements. |
| Native integration | The package declares a `tradingview` protocol, web URI handlers and a disabled-by-default startup task. The inventory includes `classic_level.node`, `keytar.node` and `native/tvdbridge.node`. | Declarations and file presence only. Native add-on behavior, exports, credential handling and actual startup state were not analyzed. |

Electron fuse inspection also identified a PE x86-64 slice and a version-1 fuse
wire with nine entries. Run-as-Node and Node CLI inspection switches were enabled;
cookie encryption, embedded ASAR integrity validation and ASAR-only loading were
disabled in that wire. These are static byte observations, not findings of an
exploitable vulnerability or proof of runtime access.

## Implications for terminal-rs

These are candidates supported by this investigation, not a new list of missing
features inferred from product reputation. See the [TC2000 gap backlog](tc2000-gap-backlog.md)
for the consolidated implementation priorities.

1. **Extend alert delivery and lifecycle (T06).** Add optional sound and desktop
   notifications whose action selects the alert's symbol, chart and relevant
   condition. Define acknowledgement, repeat/rearm, cooldown and restart behavior.
   Existing local price/condition alerts provide the foundation. Monitoring while
   the app is closed remains an explicit product/architecture decision.
2. **Verify linked-chart recovery.** terminal-rs already links charts. Use delayed
   loading, symbol changes during loading, focus changes, restoration and partial
   data availability as acceptance scenarios. Evaluate buffering and channel
   ownership before adding more linking controls.
3. **Audit workspace restoration.** terminal-rs already saves layouts/templates.
   Verify per-pane symbol/interval restoration, sync settings, interrupted saves,
   unsaved-change handling and reload behavior before declaring another feature
   missing. Desktop tab/window lifecycle deserves a focused workflow audit.
4. **Keep calculation parity evidence separate.** This desktop pass supplies no
   numerical reference outputs for EMA, RSI, Bollinger Bands or proprietary
   studies. Independent study histories (T01), attributed formula fixtures (T02)
   and shared scan/chart/alert conditions (T03) remain the stronger foundations
   for TC2000 parity; the desktop shell evidence does not supersede them.

The account-overlay default and 2H/4H session-alignment decisions remain on hold,
as requested. No application behavior was changed by this investigation.

## REA evidence ledger

Full identifiers are retained so these notes can be matched to the local bundle.
Parent evidence identifies the retained analysis used by views and traces.

| Ref | Operation / scope | Evidence ID |
|---|---|---|
| F | Electron fuse inspection | `ev_23e1d68d6cbe608f1639ea8dd09233da96604853d2a8c05f8bc67d7932ed857e` |
| M | ASAR main-entry summary | `ev_5a2ee2c10404865b508bc09092852f6a7a187f4c73a2187524c919209f275225` |
| M-parent | Main-entry retained analysis | `ev_da77b6ad4f7282bae7d66dc1a9ce8c0aef0211acc123dceb3669070f90d5270c` |
| P | Package identity facts view | `ev_bb3010eb5f4d4ebccb7eb305cdc332ca41b3d6f11c394c07156671d4632fead4` |
| S | Preload/UI/config summary | `ev_3780b1a208dbae42019829b6db8bf46d3aaf3ebff79246a488e04ce67cf9785f` |
| S-parent | Preload/UI/config retained analysis | `ev_46ba89f8373f466423cce9f0abe955a2c02123a8b65c41983958b9eb1c992ebc` |
| D1 | Initial 20-source summary | `ev_845232dea928b907f3af7aa982e52a713ccebe45973320400336756f7b2155e5` |
| D1-parent | Initial selected-source retained analysis | `ev_f35256ed10f215a03e40f975f25d872df3e9c2c13d5aa4565e40e56db9312e94` |
| L | Linking module outgoing trace on D1-parent | `ev_6c03d2b2e18e42b4ff12a7108bc029a0b4a48ae72cff855051d0e71a241b45a1` |
| D2 | Expanded 32-source summary | `ev_d841a75ddcd617ab2744a466fc013809ea32afe2621a9c361ec9901fe72c6b59` |
| D2-parent | Expanded selected-source retained analysis | `ev_2484f8c8d8c1c4a8c417d36b5f927db294e0bc0123cd73250131959754995b05` |
| V | Six-module selected facts view on D2-parent | `ev_24c5a8d350eb3b00de5f14cfc9c166bdbaff62af2a0d7359ddb6e511f82bb633` |

Coverage boundaries:

- **M:** three selected paths (`package.json`, `index.js`, `index.js.map`),
  10,989,436 text bytes; one parsed JS file, 590,597 AST nodes and 908 bundled
  modules. Zero parse failures. Application graph partial; semantic recovery
  truncated. A zero BrowserWindow count in this pass is not proof of absence.
- **S:** 14 selected paths covering configuration, three preloads and selected
  window/menu/browser-API bundles/maps; six parsed JS files, 25,940 AST nodes.
  Zero parse failures. Four context-bridge observations and 62 IPC operations
  (30 literal, 32 dynamic) were recovered; complete renderer/main pairing was
  not established. Graph partial; semantic recovery truncated. Inline HTML
  scripts were not analyzed.
- **D1:** 20 selected source-map texts; 26,525 AST nodes, zero parse failures.
  The linking trace has 11 nodes/11 edges, zero terminal paths, nine observed and
  21 inferred facts. It is not a complete end-to-end execution trace.
- **D2:** expanded to 32 texts to include linking and storage dependencies;
  236,215 text bytes, 31,663 AST nodes, zero parse failures. Graph partial,
  semantic status unknown, 6,931 semantic unknowns. No hard truncation/omitted
  identities were reported in this pass; that does not remove unresolved calls,
  data flow or external dependencies. V exhausted its 122-row selected view,
  not the entire application's facts.

## Unresolved questions and next evidence

No running `TradingView.exe` process was found at discovery. We did not launch
TradingView, attach a browser debugger, log in, inspect user credentials/profile
data or exercise paid features. This pass therefore leaves these questions open:

- Chart/indicator calculation location, numerical semantics, Pine execution and
  custom-script editing: require separate targeted code/runtime or attributed
  output evidence; not recoverable from these desktop adapters alone.
- Notification clicks, sound, window focus and link propagation in practice:
  require a controlled running-app scenario and an explicit test alert.
- Workspace recovery, multiple-window synchronization and save conflicts:
  require controlled fixtures and restart/reload scenarios.
- Market-data coverage, broker/order behavior, scanners, fundamentals and plan
  limits: require feature-specific observation and source/entitlement verification.

The existing website study remains useful for publicly observed chart interaction
patterns. Its captures and this installed desktop analysis have different targets,
dates and coverage; neither should be presented as an exhaustive product audit.
