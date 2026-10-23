# Product

<!-- impeccable:product-schema 1 -->

## Platform

desktop (native Rust/egui application; macOS is the primary target, Linux builds also ship. Not web, iOS or Android.)

## Users

- Primary: the author, a hands-on trader and investor who uses terminal-rs every day for charting, scanning, research and portfolio monitoring.
- Secondary: terminal-rs is open source, so it should be a polished, clean product that a professional trader or investor would find valuable when they adopt it.

## Product Purpose

A fast, keyboard-driven market terminal: TradingView-class GPU charting with a Bloomberg Terminal working style (dense tiled panes, monospace numbers, a command line such as `AAPL <GO>`), across stocks, ETFs, indices, FX, crypto, US Treasuries, futures and options. It adds congressional trade tracking from the official STOCK Act disclosure systems, the user's own brokerage accounts (read-only by default), scanning, alerts and saved layouts.

The agent pane brings a chart-aware AI analyst into the terminal. Success: the user gets trustworthy answers grounded in the data actually loaded on their charts, and can hand the agent chart work, without leaving the terminal or giving up control.

## Positioning

- One local terminal that combines charting, multi-asset data, congressional disclosures and the user's own accounts, operated from the keyboard.
- The agent runs locally through Codex, signed in with the user's own ChatGPT account, isolated in a sandbox with no shell, file or web access; it reads and edits charts only through the terminal's own tools, and every agent edit is badged and undoable.

## Operating Context

- Used during live markets and for longer research sessions, often with many panes open at once; the agent sits in a dockable pane beside charts, watchlists and scanners.
- Keyboard-first: command line, hotkeys and the command palette; typing in the agent's input must never trigger chart hotkeys.
- Many themes (Bloomberg amber, Terminal Light, Nord, Solarized, Monokai and user themes) and density settings; every surface must work in all of them.

## Capabilities and Constraints

Agent pane jobs, all in scope:
- Quick chart reads: short, glanceable answers (trend, levels, what changed).
- Chart markup: drawing levels, trend lines and indicators, and switching timeframes, on the pinned chart, with per-turn undo.
- Deeper research: longer back-and-forth analysis across symbols and timeframes.
- Workflow automation (planned): watchlists, alerts and scans on the user's behalf.

Agent response style is a user preference with three options: terse terminal style (number-first, minimal prose), concise analyst (brief plain English with the key numbers and a bottom line; the default), and detailed teacher (explains reasoning and concepts).

Constraints:
- Native egui UI; no web views. No new third-party crates without a reason.
- Read-only tools by default; chart edits are opt-in, limited, badged and undoable.
- Privacy: prompts, the pinned chart's data and tool results go to OpenAI under the user's ChatGPT account; screenshots only with consent. Credentials never appear in the UI, logs or exports.
- Answers are analysis, not investment advice.

## Brand Commitments

- Name: terminal-rs.
- Bloomberg Terminal working style and TradingView-class charting are the stated references for behaviour; no Bloomberg or TradingView code, icons, fonts or other assets are used.

## Evidence on Hand

- Screenshots of the terminal in several themes: `docs/img/`.
- UX research and design rationale: `docs/analysis/`.
- In-app acceptance of the agent pane (2026-10-23): `docs/analysis/agent-bridge-a2a.md`.
- No user testimonials, adoption figures or performance claims exist; do not invent them.

## Product Principles

1. Grounded, not generic: every answer is tied to data actually loaded in the terminal, with timestamps and prices, and says when data is sample, partial or unfinished.
2. The user stays in control: read-only by default, opt-in edits, visible provenance, one-step undo, interrupt at any time.
3. Fast and dense like a professional terminal: keyboard first, glanceable, no wasted motion.
4. Private and contained: the agent only touches the terminal through its tools.
5. Polished enough to share: open source, held to the standard a professional trader expects.
