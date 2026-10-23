---
name: terminal-rs
description: A calm, native trading desk around dense, keyboard-driven market data.
colors:
  window: "#1E1E20"
  panel: "#0C0C0D"
  panel-header: "#1C1C1E"
  border: "#FFFFFF16"
  grid: "#222224"
  text: "#ECECEE"
  muted: "#8E8E93"
  amber: "#FFB000"
  amber-dim: "#AA7600"
  info-cyan: "#64D2FF"
  up: "#30D158"
  down: "#FF453A"
  control: "#FFFFFF12"
  control-hover: "#FFFFFF1E"
  system-accent: "#0A84FF"
typography:
  heading:
    fontFamily: "SF Pro (system UI), Helvetica"
    fontSize: "16pt"
  body:
    fontFamily: "SF Pro (system UI), Helvetica"
    fontSize: "13pt"
  label:
    fontFamily: "SF Pro (system UI), Helvetica"
    fontSize: "10.5pt"
  numeric:
    fontFamily: "SF Mono (system mono)"
    fontSize: "12pt"
rounded:
  card: "10px"
  menu: "8px"
  control: "6px"
spacing:
  item-x: "6px"
  item-y: "3px"
  button-x: "8px"
  button-y: "3px"
  control-height: "20px"
components:
  pane-card:
    backgroundColor: "{colors.panel}"
    rounded: "{rounded.card}"
  pane-header:
    backgroundColor: "{colors.panel-header}"
    textColor: "{colors.text}"
  control:
    backgroundColor: "{colors.control}"
    textColor: "{colors.text}"
    rounded: "{rounded.control}"
    padding: "3px 8px"
    height: "20px"
  control-hover:
    backgroundColor: "{colors.control-hover}"
  ticker:
    textColor: "{colors.amber}"
    typography: "{typography.numeric}"
---

# Design System: terminal-rs

## Overview

**Creative North Star: "The Native Trading Desk"**

terminal-rs is a professional's desk, not a dashboard: calm, native macOS chrome (system fonts, the system accent, rounded cards, vibrancy behind the sidebar) wrapped around dense, Bloomberg-grade data set in monospace. The chrome recedes so numbers lead. Everything is reachable from the keyboard, and the interface answers instantly.

Density is a feature: tiled panes, tight rows and small type carry a lot of information per pixel, with exact alignment of monospace figures. Colour is functional, never decorative: amber for tickers and the user's focus, green and red for moves, cyan for live/info values, the system accent for selection and interactive focus. Every surface must hold up in every theme (Terminal Dark and Light, Bloomberg amber, Nord, Solarized, Monokai and user themes) and at every density.

The system rejects three looks outright: a consumer chat app (big bubbles, avatars, emoji, playful styling), crypto-casino UI (neon, glow, gradients, gamified effects) and the generic web dashboard (card-heavy SaaS layouts with large padding and oversized headings).

**Key Characteristics:**
- Native macOS chrome, Bloomberg-dense content.
- Monospace for every number, ticker and table; system UI font for chrome.
- Colour carries meaning only (direction, focus, live, selection).
- Keyboard-first: command line, hotkeys, palette.
- Theme- and density-agnostic: roles, never hard-coded colours.

## Colors

A near-black data surface under quiet neutral chrome, with amber as the terminal's signature and semantic green/red/cyan doing the rest. Values below are the default Terminal Dark theme; every theme maps the same roles.

### Primary
- **Ticker Amber** (`amber`): tickers, the focused symbol, key figures the eye should land on, the terminal's identity colour. Dimmed amber (`amber-dim`) for secondary emphasis and inactive amber states.

### Secondary
- **Live Cyan** (`info-cyan`): live or informational values (live quotes repricing holdings, info notes).
- **System Accent** (`system-accent`, from macOS when set): selection, focus rings, the text cursor, hyperlinks and active controls; a translucent wash of it marks selection.

### Tertiary
- **Market Green / Market Red** (`up`, `down`): price direction, P&L, buy/sell. Never used for anything but direction or success/failure.

### Neutral
- **Desk Charcoal** (`window`): the window behind the cards.
- **Data Black** (`panel`): pane (card) background where data lives.
- **Header Graphite** (`panel-header`): pane headers and faint fills.
- **Hairline** (`border`, white at 9%): card and control outlines; `grid` for chart and table grid lines.
- **Primary Text** (`text`) and **Secondary Grey** (`muted`): content and labels/metadata.
- **Control Tints** (`control`, `control-hover`): translucent fills for buttons and segmented controls.

### Named Rules
**The Meaning-Only Colour Rule.** Colour signals state (direction, focus, live, selection, warning), never decoration. If a colour does not answer "what does this mean?", use a neutral.

**The Roles-Not-Values Rule.** UI code reads palette roles (`theme::amber()`, `theme::muted()`), never literal colours, so every theme and the light/dark switch work automatically.

## Typography

**Display Font:** none; the terminal has no display type.
**Body Font:** SF Pro (system UI font, Helvetica fallback), optionally switched to monospace by the theme.
**Label/Mono Font:** SF Mono (or the theme's chosen mono font) for every number, ticker, price, timestamp and table.

**Character:** a quiet native sans for chrome paired with a precise mono for data; the pairing says "Mac app" and "terminal" at once.

### Hierarchy
- **Heading** (16pt): pane-level titles and dialog headings only; rare.
- **Body** (13pt): chrome text, buttons, menus, prose.
- **Label / Small** (10.5pt): metadata, captions, secondary labels.
- **Numeric** (mono, 12pt): figures, tickers, tables, code.
All sizes scale with the theme's font scale (0.7 to 1.6).

### Named Rules
**The Mono Numbers Rule.** Any number a user might compare (price, change, volume, timestamp, level) is set in monospace so columns align.

## Layout

Tiled, dockable panes on a desk background: each pane is a card with a header strip and a dense content area. Spacing comes from a small scale multiplied by the theme's density (compact, regular, comfortable) through `dp()`: item spacing 6×3, button padding 8×3, control height 20 (regular density). Rows are tight; padding is never generous for its own sake. Panes must work from a narrow side column to full width.

### Named Rules
**The Density Rule.** Size layout in density-scaled points (`dp`) and theme fonts; never fixed pixel gaps that ignore the density preference.

## Elevation & Depth

Flat by default. Depth comes from tonal layering (desk, then card, then header strip) and hairline borders, not shadows. Shadows exist only on floating layers: windows and popups/menus (soft, larger in dark themes). Pane focus is shown by the card treatment, not by lifting.

### Named Rules
**The Flat Desk Rule.** Panes and in-pane elements carry no shadows; only true floating layers (popups, menus, dialogs) cast one.

## Shapes

Softly rounded native forms: cards at 10px, menus at 8px, controls (buttons, inputs, segmented controls) at 6px. Pills are used sparingly for compact status or buy/sell chips. No sharp-cornered or fully circular controls beyond icons.

## Components

### Buttons and controls
- **Shape:** gently rounded (6px).
- **Default:** translucent control tint with primary text, no border; hover lifts the tint (`control-hover`); active/selected uses the accent wash.
- **Icon buttons:** drawn vector icons with a tooltip; active state uses the accent.
- **Segmented controls:** a shared control strip for small mutually exclusive choices.

### Pane card and header
- **Card:** data-black surface, 10px corners, hairline border, focus shown by the card treatment.
- **Header:** graphite strip with the pane title and compact controls aligned right.

### Inputs / Fields
- **Style:** control tint background, 6px corners; the text cursor and focus use the system accent (2px cursor).

### Rows and tables
- **Style:** dense rows with a hover wash, monospace figures right-aligned, amber tickers, green/red changes; selected rows use the accent wash.

### Pills
- **Style:** small rounded chips for compact status (for example buy/sell), coloured by meaning only.

## Do's and Don'ts

### Do:
- **Do** read colours, fonts and spacing from the theme (`theme::*`, `dp()`), so every theme and density works.
- **Do** set every number, ticker and timestamp in monospace and align columns.
- **Do** keep chrome quiet and let data lead; use amber, cyan, green and red only for their meanings.
- **Do** keep everything keyboard-reachable and responsive instantly.

### Don't:
- **Don't** make anything look like a consumer chat app: no large rounded bubbles, avatars, emoji or playful styling.
- **Don't** use neon, glow, gradients, confetti or gamified effects.
- **Don't** build card-heavy SaaS layouts with large padding and oversized headings.
- **Don't** add shadows to panes or in-pane elements.
- **Don't** hard-code colours or pixel spacing that bypass the theme and density.
