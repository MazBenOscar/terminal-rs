# Price alerts: first version

Branch: `codex/price-alerts`, based on `personal/main` at `ca582cc` after the chart
interaction fixes and remote compare/legend polish. This increment adds local,
one-shot price-level alerts. Indicator crosses, drawn-line conditions, scanner
matches and server-side delivery remain future work.

## Create and manage

- Right-click inside a chart's **price pane** → **Create price alert here…**.
  The editor starts at the cursor's displayed price precision. It suggests crossing
  above a level above the current chart close, or below a level below that close.
- Choose **Crosses above** or **Crosses below**, adjust the price and create it.
  Alternatively, press **F4** / enter **ALRT** and select **New price alert…**.
  A symbol can be a ticker or a complete market key.
- Armed alerts appear as amber dashed levels on charts of that symbol. Disabled
  levels are muted; triggered levels disappear. Off-screen levels do not change
  chart autoscaling. Only levels for the current live/sample data mode are drawn.
- The chart context menu's **Price alerts** submenu provides edit, disable/rearm,
  delete and **Manage all alerts…**. The Alerts pane provides the same controls.
- **Price alerts → Triggered** holds the latest 500 events, including the level,
  observed price, source, price-data time and observation time. Events remain after
  deleting their alert. **Mark read** clears their unread badge.
- A new event produces an in-app **Price Alert** toast and updates the shared bell
  badge. Congress filing alerts remain under **Congress filings** with their own
  read controls. This version has no OS notification, sound, email or push delivery.

## Crossing and monitoring rules

Above means the preceding sampled price was strictly below the level and the new
sample is at or above it. Below is the reverse. Touching the level from the intended
side counts; a sampled jump across it counts. An alert fires once and becomes
**Triggered**. **Rearm**, or saving an edit, arms it again.

The first valid observation only establishes a baseline, even if already beyond
the level. Creating, editing, rearming and restarting never retroactively fire an
alert. Runtime baselines are not saved. Provider failures, stale/invalid data,
source-route changes and price-data gaps longer than two minutes establish a fresh
baseline instead of guessing a crossing. Out-of-order price-data timestamps are
ignored; updates within the same minute can cross a level.

The monitor runs independently of chart panes, tabs, layouts, chart timeframes,
chart refresh settings and brokerage account selection. It polls current minute
bar closes about every 15 seconds after a completed check, with at most four
concurrent symbol requests and a 20-second timeout per symbol. Slow providers can
extend this interval. Limits: 200 configured alerts across 32 distinct market keys.

**The app must remain running and awake.** Crossings between samples, during sleep,
offline periods or while the app is closed can be missed. No historical replay or
catch-up evaluation is performed. The supplied minute-bar timestamp is its opening
time, not a guaranteed exchange tick time. Up to 20 minutes of data age is accepted
to accommodate delayed providers; older observations and timestamps over 30 seconds
in the future are rejected. The Alerts pane displays price-data times or an explicit
waiting/unavailable state. Monitoring does not establish real-time data entitlement
or enforce a particular trading session; available provider bars determine coverage.

## Data and storage

`MarketData::alert_observation` uses uncached minute history with provenance. The
ordinary quote fallback is deliberately bypassed: a failed live provider cannot
produce an alert from sample prices. Configured Questrade/Alpaca stock sources stay
fixed for alert requests; unavailable data is reported rather than switching sources.
No chart-cache data or demo-account quote is used for evaluation.
Crypto provenance identifies the existing Binance/Coinbase route, not each
underlying venue; the provider may change venue within that route.

Explicit sample mode only evaluates alerts marked **Sample**. Live alerts pause
there, and sample alerts pause in live mode. The offline sample history is generated
at provider startup; its separate chart tick simulation is not the alert feed.
Use deterministic engine tests for crossing scenarios rather than expecting the
sample chart's moving last-price tag to trigger a sample alert.

Configuration, triggered state and history are atomically written to
`price-alerts.json` in the application data directory (`TR_DATA_DIR` overrides it).
Each change saves immediately. Failed writes show an in-app notice and a persistent
error in Alerts, retry every five seconds, and retry on exit. Unsaved changes can be
lost on exit, including a triggered state. Corrupt, oversized or unsupported files
are preserved and monitoring/editing stay blocked; close the app, repair or move the
file aside, then restart. This local store assumes one running app instance per data
directory; concurrent instances are not coordinated.

## Review checklist

1. Create above/below levels from a chart and from ALRT. Inspect the price and mode
   before saving; verify the line on another chart of the same symbol.
2. Edit, disable, rearm and delete through both the list and chart submenu. Confirm
   that no existing drawing or undo history changes.
3. Close all chart panes. Keep Alerts open and verify that monitoring continues.
4. On a configured live feed, let successive observations cross a nearby level.
   Verify one toast, one history record and one unread event; repeated observations
   must not fire it again until rearmed. Prices can be delayed.
5. Restart with armed and triggered alerts. Verify restoration without retrospective
   firing. Confirm that marking price history read does not mark Congress filings read.
6. Disconnect data access and verify visible unavailable/waiting state and recovery.
   Sample/live mode changes must never evaluate alerts against the other mode's data.

Automated coverage includes crossing boundaries, one-shot/rearm semantics, stale,
future, invalid, out-of-order and simulated observations, source changes and gaps,
history bounds, restart behavior, persistence failures/corrupt-file preservation,
in-flight configuration revisions, worker operation without charts, and pointer-driven
chart-menu/editor/list controls. Native sample-mode visual checks cover chart creation,
level display and the split-pane alert list; external live-provider delivery is not
verified by the offline suite.

Validation on this branch: 383 workspace tests passed, with 5 existing tests
ignored; formatting and `cargo clippy --workspace --all-targets -- -D warnings`
passed. Tests and the sample UI preview ran under WSL Ubuntu. The final data-source
selection adjustment also passed the data crate's test suite.
