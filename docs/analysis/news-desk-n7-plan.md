# N7: News desk (Story pane, FLASH strip, headline tape, dashboard presets)

Status: 2026-10-29. Plan, approved scope. The user's choices:
- AI analysis runs **on click** and is cached per story.
- Extras in the first build: the Story pane plus the News desk preset, the bottom headline tape, the FLASH
  strip, and related stories.

The reference was a screenshot of another terminal with a headline list, an article detail panel, a flash
bar and a scrolling tape. We take the ideas only. The look follows DESIGN.md ("The Native Trading Desk":
dense rows, monospace data, colour for meaning only, theme roles and `dp()` only, every theme).

## N7a: Story pane (new `PaneKind::Story`, command `STORY`)

The Story pane is a companion to a News pane and shows the story selected there.

### How it follows a News pane

- It joins the existing colour-link system:
  - A News pane with a link colour publishes its selected story id to panes on the same link.
  - Today's link bus only carries symbols. Add a story-selection message alongside it, so nothing changes
    for charts.
- An unlinked Story pane follows the most recently focused News pane.
- With no selection, the empty state reads "Select a headline in a News pane". If no News pane exists, it
  offers to open one.

### Content, top to bottom

1. **Headline.** Full and wrapped.
2. **Story line.** Publisher, age, priority/event pill and category code, using the News row grammar.
3. **Summary.** Full, at reading width.
4. **Sources.** Every publisher carrying the clustered story (the ×N), each with time and link.
5. **Keywords matched**, if any.
6. **Tickers** as chips:
   - A click broadcasts the symbol on the pane's link colour, so a linked chart switches to it.
   - ⌥-click adds the ticker to the watchlist through the existing path.
7. **Actions:** Open, Copy link, Copy headline, Save story (the existing bookmark path), Analyze.
8. **Analysis.** See the next section.
9. **Related.**
   - Up to 8 stories from the last 7 days that share a ticker (or failing that the same event/category).
   - The selected cluster is excluded and the newest come first.
   - Clicking one selects it in the source News pane.

### Analyze

The user chose analysis on click: nothing runs automatically.

- **Trigger.**
  - Analyze builds a prompt and enqueues it on an Agent pane, using `agent_pane::enqueue`.
  - It picks the Agent pane on the same link, or else the most recent one. With no Agent pane it opens one
    as a tab next to the Story pane.
  - The turn runs **ReadOnly**, with no write tools, whatever the pane's current mode.
- **Untrusted content.** The headline, summary and publisher text are untrusted data. Put them in a
  clearly delimited block, with an instruction that the text is data and not instructions. No links are
  fetched unless the user asks in the Agent pane.
- **Reply format.** The prompt asks for a fixed, small JSON object:
  `sentiment` (-1..1), `confidence` (0..1), `urgency` (low|medium|high), `outlook` (short phrase),
  `key_points[]` (≤5), `risks[]` (≤3, each with a level), `tickers[]`.
- **Parsing.**
  - Parse tolerantly: take the first JSON object in the reply.
  - If parsing fails, show the reply text as plain prose and offer "Retry".
- **Display.**
  - The Story pane shows the parsed result as inspector rows: Sentiment, Confidence, Urgency, Outlook,
    then key points and risks.
  - Colour carries meaning only: up and down colours for sentiment, warn for high urgency.
  - The footer shows the model name and age, for example "MiniMax-M3.1-Flash · 3m ago", plus
    "Re-analyze".
- **Cache.**
  - New table `story_analysis(story_id, model, created_at, json)` in news.sqlite3. The migration is
    additive.
  - Re-selecting the story shows the cached result instantly.
  - Clusters key on the lead id and fall back through member ids.
- **States.**
  - Queued or Running, with Cancel through the agent's own cancel.
  - Agent not ready shows the agent's stopped text and a button that focuses the Agent pane.

### Persistence

- `PaneSettings::Story { link_follow: … }`, with serde defaults.
- Old layouts load unchanged.

## N7b: FLASH strip and headline tape

### FLASH strip, inside the News pane header

- **What it pins.** The newest **High** priority story in the pane's current scope, under the pane's
  filters minus the date range, for 10 minutes after arrival.
- **Look.** One line: a `FLASH` badge, the headline and the publisher. It uses the warn or error role,
  never decoration.
- **Interaction.**
  - Clicking the strip selects the story.
  - × dismisses that story; a newer one re-arms the strip.
- **Settings.**
  - Per-pane toggle in the ⋯ menu: "Flash high-priority", on by default in the News desk preset and off
    elsewhere.
  - Narrow panes hide the strip.

### Headline tape, an app-level bottom strip

- **Toggle.** View menu and command palette ("Headline tape"). Off by default; the News desk preset turns
  it on.
- **Content.** High-priority stories and keyword matches from the last 2 hours, from the market-scope news
  cache. Each entry is a publisher tag and the headline, separated by thin dividers.
- **Motion.**
  - Slow horizontal scroll, about 40 px/s. Hovering pauses it.
  - Use `request_repaint_after` at a capped rate, never a continuous 60 fps.
  - With reduce-motion set, or the app unfocused, the tape rotates statically every 8 s instead.
- **Click.** Clicking an entry focuses the most recent News pane and selects the story. If no News pane is
  open, it opens the URL.
- **Empty.** With nothing qualifying, the tape hides itself and takes no space.

## N7c: dashboard presets (the "News desk" first)

- **Registry.** A built-in registry of dashboards, `Dashboard { id, name, description, build(&PanelDefaults)
  -> LayoutDoc }`, reusing `LayoutDoc` and `dock::Node::split_with`.
- **News desk:**
  - Row split 0.64 / 0.36.
  - Left: a News pane in Market scope, Stories view, with Flash on.
  - Right: a column split 0.62 / 0.38, with the Story pane on top and below it a chart (SPY) on the same
    link colour.
  - The News, Story and chart panes share link Orange. Clicking a ticker in the Story pane therefore drives
    the chart.
  - The headline tape turns on.
- **Opening it.**
  - It opens as a **new saved workspace** named "News desk", or "News desk 2" if that name exists. The
    user's current workspace is never overwritten.
  - It is reachable from the Layouts menu ("Dashboards ›") and the palette (`DASH NEWS`, "News desk").
  - Edits are saved like any workspace, and the built-in template stays pristine.
- **Later presets** each need only a `build` function, plus any new pane they rely on:
  - **General:** chart, watchlist, news and agent.
  - **Markets:** indices and futures, macro news, sector heat map. The heat map is a new pane.
  - **Crypto:** crypto chart, order book, crypto news and scanner.
  - **Equity:** a chart linked to profile, filings, news and options.

## Delivery

- **Worker A** (`codex/news-n7a`): N7a and N7c (the Story pane, Analyze, related stories, the
  story-selection link message and the dashboard registry with News desk).
- **Worker B** (`codex/news-n7b`): the N7b FLASH strip and headline tape.
- **Overlap.** Both touch `news_view.rs` lightly. B owns the header strip and the tape. A owns the selection
  publish. Merge B first, then A.
- **Review.** An independent Opus review covers prompt-injection handling in Analyze, repaint cost of the
  tape, link-bus regressions and serde compatibility. Then a fix pass and full checks.
- **Test.** Tests use offline fixtures only. The user does the live test after the merge.
