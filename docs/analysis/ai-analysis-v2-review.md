# AI analysis v2: review of the proposed prompt and colour plan

Status: 2026-10-30. This reviews [docs/prompts/ai-article-analysis.md](../prompts/ai-article-analysis.md)
against how terminal-rs works today (the N7 Story pane), and gives the colour roles for the
rendered note (impeccable colorize, Operate mode). It is a plan only; no code yet.

## 1. What carries over unchanged

The prompt is strong. These parts go in as written:

- **Verdict at the top**, made of direction, asset, horizon and conviction, with "Proves wrong if".
  This is the most useful change.
- **Asset-relative sentiment**, for example `-0.35 (EUR)`.
- **One fact, one place.** Risk lines are capped at 18 words.
- **The four risk categories**, plus a "Not addressed:" line.
- **Entities exclude the author and the outlet.**
- **The "What v2 refuses to add" list:** no invented tickers, price levels or historical percentiles.
- **The JSON variant.** We render the note ourselves, so we use JSON (see §3).

## 2. What has to change for terminal-rs

### 2.1 Input

**Problem.** We do not have the article body. The Story pane holds only:
- the headline
- the feed's summary (Finnhub and RSS give 1–2 sentences, sometimes nothing)
- publishers, time, tickers, category and event pill

We do not fetch articles. Publisher terms, paywalls, redirect links and the senate.gov lesson all argue
against it, and the guard has no general "fetch any article" host.

**What breaks.** The fixed counts would force the model to invent material:
- exactly 5 key points of 18–35 words
- a 60–100 word summary
- 6–8 hashtags
- 4–12 entities

**Change.** Every count becomes a maximum, and the output scales to the input:

| Input available | Summary | Key points | Tags / topics | Entities | Confidence cap |
|---|---|---|---|---|---|
| Headline only | omit | 0–2 | ≤4 | ≤4 | 70% |
| Headline + standfirst (normal case) | ≤60 words | ≤3 | ≤6 | ≤8 | 80% |
| Several publishers' standfirsts in the cluster | ≤80 words | ≤5 | ≤8 | ≤12 | 90% |

- The pane tells the model which row applies (`"input": "headline_only" | "standfirst" | "multi_source"`).
- The pane prints that basis under the note, for example "Based on headline + 2 summaries".
- An empty field is better than a padded one; the existing rule "If a figure is missing, omit it" covers it.
- **Cluster input is new.** It sends the standfirst of every publisher in the cluster (the ×N) as
  separate untrusted blocks. This is the cheapest legitimate way to get more substance than one summary.
- **Later and optional:** fetch the full article. It needs a guarded, per-click, user-visible "Fetch article
  text" with per-host spacing, and only for hosts the user allows. Not in this step.

### 2.2 Prompt delivery

- Our agent backends (mimo, Codex) take one user turn. There is no per-turn system message.
- Part A and part B are therefore sent as one turn:
  1. the instructions first
  2. then the story inside the existing escaped `<STORY_DATA>` block, which keeps the injection defence
     the review checked
- The URL is not sent as something to open. The publisher host is enough for "Never list the publisher".
- The turn stays **ReadOnly**.

### 2.3 Output and validation in Rust

- Ask for the JSON variant only.
- The parser:
  - clamps sentiment to −1..1, conviction to 1..10 and confidence to the cap above
  - enforces the enums for direction, urgency, levels and categories
  - truncates lists to their caps
  - drops any entity naming the publisher or author
- **Consistency check.** If the verdict says BEARISH but sentiment is positive, the note still shows, with
  a small "inconsistent" flag. It is never silently fixed.
- **Cache.**
  - Records carry `schema: 2`.
  - Older v1 cache entries show as "Older analysis · Re-analyze".

### 2.4 Chips

- **Hashtags and topics overlap.** Merge them into one **Topics** row, at most 8.
  - Each chip is a click-to-filter search in the linked News pane, which is the function the doc assumes
    hashtags have.
- **Entities:**
  - listed companies with a ticker become **ticker chips** that drive the linked chart (an existing Story
    pane behaviour)
  - other entities become search chips

### 2.5 Model reliability

- mimo's default model is a fast tier. 15 rules plus a self-check is a lot for it.
- Before we ship:
  - run the new prompt on about 10 saved stories (macro print, single-name earnings, geopolitics,
    headline-only) through the user's mimo and Codex
  - compare the results side by side
- Keep the "Knobs" table as the tuning guide. If the fast tier fails the counts, shorten the rules rather
  than add more.

## 3. Colour plan (impeccable colorize, Operate mode)

**Strategy:** restrained.
- The note is mostly ink (`text`) and quiet labels (`muted`).
- Colour appears only where the reader must act or judge, and no note uses more than three hues:
  1. **direction**: `up` / `down`
  2. **attention**: amber
  3. **alarm**: a `down` fill, used only for the top urgency or risk levels
- Cyan stays reserved for live/info values. The system accent stays reserved for selection and focus.
- No new theme fields. The roles derive from the existing palette, so every built-in and user theme works.

### Roles

| Element | Role | Non-colour cue |
|---|---|---|
| Verdict direction (BULLISH / BEARISH / NEUTRAL / NO SIGNAL) | `up` / `down` / `text` / `muted`, semibold | ▲ ▼ ■ glyph and the word itself |
| Verdict asset | ticker convention (`amber`, mono) | uppercase mono |
| Horizon, conviction | `text`; conviction as a 10-cell meter in `text`/`grid` (intensity, not polarity, so no hue) | number "8/10" |
| **Proves wrong if** | label in **attention**, plus a 2 dp attention rule down the left edge; body in `text` | label word, position directly under the verdict |
| Sentiment score | `up`/`down` by sign; `(asset)` in `muted` | explicit sign |
| int, confidence | `muted` mono | labels |
| Outlook | `text` (the score already carries the polarity; no double coding) | — |
| Urgency LOW / MEDIUM | `muted` / `text` | word |
| Urgency HIGH | attention, outlined badge | badge outline |
| Urgency BREAKING / CRITICAL | **alarm**: filled `down` badge with card-coloured text | filled shape |
| Risk level | same ladder as urgency; category name in `text`; evidence line in `muted` | word and badge shape |
| Not addressed | `muted`, one line | the "Not addressed:" label |
| Inconsistent flag | attention, small | "inconsistent" word |
| Topic / entity chips | control fill, `text` ink; ticker chips use ticker amber | chip shape |
| Basis line ("Based on headline + 2 summaries"), model and age | `muted` | — |

### Theme guard (required)

Measured on the card background:

| Theme | `up` | `amber` | Note |
|---|---|---|---|
| Terminal Light | 3.8:1 | 4.3:1 | Both are below 4.5:1 for small text |
| Bloomberg | — | — | Body text *is* amber, so the attention role disappears |

So the roles go through one helper, `analysis_roles(palette)`:

- **Text roles** (direction words, the sentiment number, the attention label) must reach 4.5:1 on `card`.
  When a role is short, it darkens on light themes and lightens on dark themes along the same hue until
  it passes.
- **Badges and rules** must reach 3:1.
- **Attention** is `amber`, unless amber is within 1.5:1 of `text` (as in Bloomberg). Then attention
  falls back to `cyan`, and that theme's ticker amber is unchanged.
- A unit test walks **every built-in theme** and asserts these ratios. User themes get the same automatic
  adjustment at run time.

## 4. Proposed delivery (N7d)

- **Branch.** One worker, on a branch from `personal/main` after N7 merges. It builds:
  - the v2 prompt (adapted per §2)
  - the JSON parser and validation
  - the `schema: 2` cache
  - the Story pane renderer using §3's roles
  - topic and entity chips
  - the theme-guard helper with its all-themes test
- **Review.** An independent review focused on fabrication risks (count scaling and the confidence cap),
  injection, and contrast. Then a fix pass.
- **Model comparison.** Before merging, a side-by-side on about 10 saved stories through mimo and Codex.
  The user picks.
