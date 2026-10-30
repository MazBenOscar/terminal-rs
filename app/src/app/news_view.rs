//! N1b: thin news presentation; only a pending read dwell schedules a repaint.
#[cfg(test)]
#[path = "news_tests.rs"]
mod tests;
use super::{NamedWatchlist, NewsPane};
use crate::{
    congress_view::Action,
    theme::{self, dp, mono, ui_font},
};
use egui::{RichText, Ui};
use serde::{Deserialize, Serialize};
use tr_data::{
    news_intelligence::{self as intelligence, Cluster, KeywordWatch},
    news_store::Scope,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    pub wire: bool,
    pub unread: bool,
    pub watch_hits: bool,
    pub sort: intelligence::Sort,
}
#[derive(Default)]
pub struct Interaction {
    pub selected: Option<String>,
    pub expanded: std::collections::BTreeSet<String>,
    pub dwell: Option<(String, std::time::Instant)>,
    pub anchor: Option<String>,
    pub snapshot: Vec<String>,
    pub scroll_to: Option<String>,
    pub focus_search: bool,
    pub search_open: bool,
    pub source_filter: String,
    pub keyword_add_open: bool,
    keyword_add_inline: bool,
    pub(super) source_cache: SourceCache,
    pub keyword_input: String,
    pub(super) sources: Vec<SourceStatus>,
    pub open_data_sources: bool,
    pub anchor_offset: f32,
    pub restore_anchor: bool,
    pub watches_dirty: Option<std::time::Instant>,
    pub save_watches: bool,
    pub pending_keys: Vec<egui::Key>,
    pub copy_digest: bool,
    pub digest_text: Option<String>,
    pub volumes: Vec<tr_data::news_store::local::Volume>,
    pub dismissed_volume: Option<i64>,
    pub flash: Flash,
    // A strip selection may legitimately be outside the table's date/filter range.
    pub external_selected: Option<String>,
}
// N1b: all history-sized work happens only on invalidation. Models, ranges,
// metadata and unread counts are reused by settled frames.
#[derive(Default)]
pub struct RowCache {
    key: Option<CacheKey>,
    rows: Vec<CachedRow>,
    ids: Vec<String>,
    unread: usize,
    pending_count: usize,
    empty_message: Option<String>,
    older_match: bool,
    rebuilds: usize,
    layout_expanded: std::collections::BTreeSet<String>,
    layout_width: f32,
    layout_dirty: bool,
    relevance_expiry: Option<i64>,
    flash: Vec<Cluster>,
    offsets: Vec<f32>,
}
#[derive(PartialEq)]
struct CacheKey {
    revision: u64,
    len: usize,
    pending_len: usize,
    filters: tr_data::news_store::Filters,
    display: Display,
    watches: Vec<KeywordWatch>,
    symbol: Option<String>,
    symbols: Vec<String>,
    watched: Vec<String>,
    flash_high: bool,
}
struct CachedRow {
    story: Cluster,
    model: RowModel,
    highlights: Vec<intelligence::WatchHit>,
    stripe: Option<u8>,
    height: f32,
}
impl RowCache {
    pub(super) fn selected(&self, id: &str) -> Option<&Cluster> {
        self.rows.iter().find(|r| r.story.member_ids.iter().any(|m| m == id)).map(|r| &r.story)
    }
    fn update(&mut self, n: &mut NewsPane, watches: &[KeywordWatch], watched: &[String], now: i64) {
        let symbols = n.query.as_ref().map(|q| q.symbols.as_slice()).unwrap_or_default();
        if self.key.as_ref().is_some_and(|k| {
            k.revision == n.item_revision
                && k.len == n.items.len()
                && k.pending_len == n.pending.len()
                && k.filters == n.filters
                && k.display == n.display
                && k.watches == watches
                && k.symbol == n.symbol
                && k.symbols == symbols
                && k.watched == watched
                && k.flash_high == n.flash_high
                && self.relevance_expiry.is_none_or(|expiry| now < expiry)
        }) {
            return;
        }
        if self.key.as_ref().is_some_and(|k| {
            k.filters != n.filters
                || k.display.wire != n.display.wire
                || k.display.unread != n.display.unread
                || k.display.watch_hits != n.display.watch_hits
                || k.watches != watches
                || k.symbol != n.symbol
                || k.symbols != symbols
                || k.watched != watched
        }) {
            n.read_in_view.clear();
        }
        self.flash = if n.flash_high { flash_stories(n, watches, now) } else { vec![] };
        let query = current_query(n, now);
        self.empty_message = n.empty_message(&query);
        let older_query = tr_data::news_store::Query { start: 0, ..current_query(n, now) };
        self.older_match =
            n.items.iter().any(|s| s.item.ts < query.start && tr_data::news_store::matches(s, &older_query));
        let items: Vec<_> = n.items.iter().filter(|s| tr_data::news_store::matches(s, &query)).cloned().collect();
        let mut rows: Vec<Cluster> = if n.display.wire {
            items.iter().take(intelligence::MAX_ITEMS).map(Cluster::wire).collect()
        } else {
            intelligence::clusters(&items)
        };
        intelligence::sort_clusters(&mut rows, n.display.sort, n.display.wire, now);
        // Re-evaluate the 24h boundary on the next event-driven frame, never a timer.
        self.relevance_expiry = (n.display.sort == intelligence::Sort::Relevance)
            .then(|| {
                rows.iter()
                    .filter_map(Cluster::newest_dated)
                    .map(|ts| ts.saturating_add(86401))
                    .filter(|at| *at > now)
                    .min()
            })
            .flatten();
        let linked = n.symbol.as_deref().filter(|_| n.filters.scope == Scope::Symbol);
        let heights: std::collections::BTreeMap<_, _> =
            self.rows.iter().flat_map(|r| r.story.member_ids.iter().map(move |id| (id.clone(), r.height))).collect();
        self.rows = rows
            .into_iter()
            .filter_map(|story| {
                let stripe = watches
                    .iter()
                    .find(|w| story.members.iter().any(|m| intelligence::watch_matches(m, &w.words)))
                    .map(|w| w.colour);
                if (n.display.unread
                    && !story.unread()
                    && !story.member_ids.iter().any(|id| n.read_in_view.contains(id)))
                    || (n.display.watch_hits && stripe.is_none())
                {
                    return None;
                }
                let height = story.member_ids.iter().filter_map(|id| heights.get(id)).copied().fold(dp(22.0), f32::max);
                Some(CachedRow {
                    model: row_model_watched(&story, linked, watched, now),
                    highlights: intelligence::watch_highlights(&story.lead.item.title, watches),
                    story,
                    stripe,
                    height,
                })
            })
            .collect();
        self.unread = self.rows.iter().filter(|r| r.model.unread).count();
        self.pending_count = n.new_count();
        self.ids = self.rows.iter().map(|r| r.model.id.clone()).collect();
        if self.ids != n.interaction.snapshot {
            let remap = |id: Option<&str>| {
                id.and_then(|id| self.rows.iter().find(|r| r.story.member_ids.iter().any(|m| m == id)))
                    .map(|r| r.model.id.clone())
            };
            n.interaction.selected = remap(n.interaction.selected.as_deref()).or_else(|| {
                n.interaction.selected.clone().filter(|id| n.interaction.external_selected.as_ref() == Some(id))
            });
            n.interaction.anchor = remap(n.interaction.anchor.as_deref());
            n.interaction.restore_anchor = n.interaction.anchor.is_some();
            n.interaction.snapshot = self.ids.clone();
        }
        // Any surviving member keeps an expansion alive even if the lead changes.
        let expanded: std::collections::BTreeSet<_> = self
            .rows
            .iter()
            .filter(|r| r.story.member_ids.iter().any(|id| n.interaction.expanded.contains(id)))
            .flat_map(|r| r.story.member_ids.iter().cloned())
            .collect();
        n.interaction.expanded = expanded;
        self.layout_dirty = true;
        self.rebuilds += 1;
        self.key = Some(CacheKey {
            revision: n.item_revision,
            len: n.items.len(),
            pending_len: n.pending.len(),
            filters: n.filters.clone(),
            display: n.display.clone(),
            watches: watches.to_vec(),
            symbol: n.symbol.clone(),
            symbols: symbols.to_vec(),
            watched: watched.to_vec(),
            flash_high: n.flash_high,
        });
    }
}
pub(super) fn owns_key(key: egui::Key, selected: bool, search: bool) -> bool {
    match key {
        egui::Key::ArrowDown | egui::Key::ArrowUp | egui::Key::Slash => true,
        egui::Key::Escape => selected || search,
        egui::Key::J
        | egui::Key::K
        | egui::Key::Enter
        | egui::Key::Space
        | egui::Key::C
        | egui::Key::U
        | egui::Key::R
        | egui::Key::W
        | egui::Key::S => selected,
        _ => false,
    }
}
// Exact modifiers (egui's consume_key also matches Shift variants). Remove
// repeat key events and their text together before type-anywhere sees them.
pub(super) fn capture_keys(input: &mut egui::InputState, selected: bool, search: bool) -> Vec<egui::Key> {
    let mut keys = vec![];
    let mut text_key = None;
    input.events.retain(|event| match event {
        egui::Event::Key { key, pressed, modifiers, .. } => {
            let owned = *modifiers == egui::Modifiers::NONE && owns_key(*key, selected, search);
            text_key = if owned && *pressed { Some(*key) } else { None };
            if owned && *pressed {
                keys.push(*key);
            }
            !owned
        }
        egui::Event::Text(text) => {
            !text_key.is_some_and(|key| key_text(key).is_some_and(|s| text.eq_ignore_ascii_case(s)))
        }
        _ => true,
    });
    keys
}
fn key_text(key: egui::Key) -> Option<&'static str> {
    match key {
        egui::Key::J => Some("j"),
        egui::Key::K => Some("k"),
        egui::Key::C => Some("c"),
        egui::Key::U => Some("u"),
        egui::Key::S => Some("s"),
        egui::Key::R => Some("r"),
        egui::Key::W => Some("w"),
        egui::Key::Slash => Some("/"),
        egui::Key::Space => Some(" "),
        _ => None,
    }
}
fn consume_exact(input: &mut egui::InputState, wanted: egui::Key) -> bool {
    let mut handled = false;
    input.events.retain(|event| {
        if matches!(event, egui::Event::Key { key, pressed: true, modifiers, .. } if *key == wanted && *modifiers == egui::Modifiers::NONE) {
            handled = true;
            false
        } else { true }
    });
    handled
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Next,
    Previous,
    Open,
    Expand,
    Chart,
    Save,
    Search,
    Escape,
    Unread,
}
#[derive(Debug, PartialEq)]
pub enum Effect {
    None,
    Open,
    Expand,
    Chart,
    Save,
    Search,
}
/// Pure reducer: IDs, not indices, survive refresh/reordering.
pub fn reduce(
    key: Key,
    ids: &[String],
    selected: &mut Option<String>,
    search: &mut String,
    unread: &mut bool,
) -> Effect {
    let current = selected.as_ref().and_then(|s| ids.iter().position(|id| id == s));
    match key {
        Key::Next | Key::Previous => {
            if !ids.is_empty() {
                let index = match (key, current) {
                    (Key::Next, Some(i)) => (i + 1).min(ids.len() - 1),
                    (Key::Previous, Some(i)) => i.saturating_sub(1),
                    _ => 0,
                };
                *selected = Some(ids[index].clone());
            }
            Effect::None
        }
        Key::Escape => {
            if !search.is_empty() {
                search.clear();
            } else {
                *selected = None;
            }
            Effect::None
        }
        Key::Unread => {
            *unread = !*unread;
            Effect::None
        }
        Key::Search => Effect::Search,
        Key::Open if current.is_some() => Effect::Open,
        Key::Expand if current.is_some() => Effect::Expand,
        Key::Chart if current.is_some() => Effect::Chart,
        Key::Save if current.is_some() => Effect::Save,
        _ => Effect::None,
    }
}
#[derive(Debug, PartialEq)]
pub struct RowModel {
    pub id: String,
    pub time: String,
    pub full_time: String,
    pub ticker: Option<String>,
    pub extra_tickers: Vec<String>,
    pub ticker_is_subject: bool,
    pub ticker_hover: String,
    pub about: String,
    pub headline: String,
    pub priority: intelligence::Priority,
    pub event: Option<intelligence::Event>,
    pub matched_phrase: Option<&'static str>,
    pub source_count: usize,
    pub meta: String,
    pub meta_steps: Vec<String>,
    pub unread: bool,
}
#[cfg(test)]
pub fn row_model(story: &Cluster, linked: Option<&str>, now: i64) -> RowModel {
    row_model_watched(story, linked, &[], now)
}
fn fallback_ticker<'a>(story: &'a Cluster, linked: Option<&str>, watched: &[String]) -> Option<&'a String> {
    story
        .tickers
        .iter()
        .find(|key| Some(key.as_str()) == linked)
        .or_else(|| story.tickers.iter().find(|key| watched.contains(key)))
        .or_else(|| story.tickers.first())
}
fn row_model_watched(story: &Cluster, linked: Option<&str>, watched: &[String], now: i64) -> RowModel {
    let (time, full_time) = if story.lead.inferred_ts {
        ("—".into(), "time unknown".into())
    } else {
        crate::feed_time::feed_time(story.newest, now, false)
    };
    let ticker_is_subject = !story.subjects.is_empty();
    let tickers: Vec<_> = if ticker_is_subject {
        // Subjects are ordered by first mention. Never promote a later subject
        // into the column merely because the first subject is linked.
        let preceding =
            linked.and_then(|key| story.subjects.iter().position(|s| s == key)).unwrap_or(story.subjects.len());
        story.subjects[..preceding].to_vec()
    } else {
        fallback_ticker(story, linked, watched).cloned().into_iter().collect()
    };
    let short = |key: &String| key.split_whitespace().next().unwrap_or(key).to_owned();
    let about = if story.subjects.is_empty() {
        "Not identified".into()
    } else {
        story.subjects.iter().map(short).collect::<Vec<_>>().join(" · ")
    };
    let returned: Vec<_> = story.tickers.iter().filter(|key| !story.subjects.contains(key)).map(short).collect();
    let about = format!(
        "About: {about}{}",
        if returned.is_empty() { String::new() } else { format!(" · Also returned for: {}", returned.join(" · ")) }
    );
    let ticker_hover = if ticker_is_subject {
        about.clone()
    } else {
        tickers
            .first()
            .map(|key| format!("Returned by {} for {}; not mentioned in the story", story.lead.provider, short(key)))
            .unwrap_or_default()
    };
    let source = story.lead.source.clone();
    let sources =
        if story.sources.len() > 1 { format!("{source} +{}", story.sources.len() - 1) } else { source.clone() };
    let tickers_meta = if tickers.len() > 1 { format!("{sources} · +{}", tickers.len() - 1) } else { sources.clone() };
    let mut meta = tickers_meta.clone();
    for tag in &story.tags {
        meta.push_str(" · ");
        meta.push_str(tag);
    }
    let meta_steps = vec![meta.clone(), tickers_meta, sources, source, String::new()];
    RowModel {
        id: story.lead.id.clone(),
        time,
        full_time,
        ticker: tickers.first().cloned(),
        extra_tickers: tickers.into_iter().skip(1).collect(),
        ticker_is_subject,
        ticker_hover,
        about,
        headline: story.lead.item.title.clone(),
        priority: story.priority(),
        event: story.classification().map(|c| c.1),
        matched_phrase: story.classification().map(|c| c.2),
        source_count: story.sources.len(),
        meta,
        meta_steps,
        unread: story.unread(),
    }
}
/// Below this width a filled square costs exactly the reclaimed glyph slot.
/// Full wording and the matched phrase remain available on hover.
fn pill_text(event: intelligence::Event, width: f32) -> &'static str {
    if width <= dp(600.0) { "" } else { event.label() }
}
fn pill_colours(priority: intelligence::Priority) -> (egui::Color32, egui::Color32) {
    if priority == intelligence::Priority::Urgent {
        let fill = theme::amber();
        let lum = |c: egui::Color32| 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
        let text = [theme::text(), theme::card()]
            .into_iter()
            .max_by(|a, b| (lum(*a) - lum(fill)).abs().total_cmp(&(lum(*b) - lum(fill)).abs()))
            .unwrap();
        (fill, text)
    } else {
        (theme::accent_wash(), theme::accent())
    }
}
#[derive(Debug, PartialEq)]
pub enum EmptyCause {
    Loading,
    NoKey,
    Filtered,
    NoHistory,
}
pub fn empty_cause(loading: bool, no_key: bool, filtered: bool) -> EmptyCause {
    if loading {
        EmptyCause::Loading
    } else if no_key {
        EmptyCause::NoKey
    } else if filtered {
        EmptyCause::Filtered
    } else {
        EmptyCause::NoHistory
    }
}
fn current_query(n: &NewsPane, now: i64) -> tr_data::news_store::Query {
    let days = n.filters.days.clamp(1, 90) as i64;
    let symbols = n
        .query
        .as_ref()
        .filter(|q| q.filters.scope == n.filters.scope && q.filters.watchlist == n.filters.watchlist)
        .map(|q| q.symbols.clone())
        .unwrap_or_else(|| if n.filters.scope == Scope::Symbol { n.symbol.iter().cloned().collect() } else { vec![] });
    tr_data::news_store::Query {
        symbols,
        filters: n.filters.clone(),
        start: if days == 1 { tr_data::news_store::day_start(now) } else { now - days * 86400 },
        ..Default::default()
    }
}
#[cfg(test)]
fn remap_anchor(id: Option<&str>, rows: &[Cluster]) -> Option<String> {
    id.and_then(|id| rows.iter().find(|s| s.member_ids.iter().any(|member| member == id))).map(|s| s.lead.id.clone())
}
fn chrome(ui: &mut Ui) {
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
    ui.style_mut().text_styles.insert(egui::TextStyle::Button, ui_font(10.5));
    ui.spacing_mut().button_padding = egui::vec2(dp(4.0), dp(2.0));
    ui.spacing_mut().interact_size.y = dp(20.0);
    ui.spacing_mut().item_spacing.x = dp(2.0);
}
fn watch_colour(index: u8) -> egui::Color32 {
    [theme::amber(), theme::cyan(), theme::accent(), theme::series(3), theme::series(4), theme::series(5)]
        [usize::from(index.min(5))]
}
fn mark_seen(n: &mut NewsPane, story: &Cluster) {
    let ids: std::collections::BTreeSet<_> = story.member_ids.iter().collect();
    let mut queued: std::collections::BTreeSet<_> = n.seen_queue.drain(..).collect();
    for item in n.items.iter_mut().chain(&mut n.pending) {
        if !item.seen && ids.contains(&item.id) {
            item.seen = true;
            n.read_in_view.insert(item.id.clone());
            n.local_seen.insert(item.id.clone());
            n.item_revision = n.item_revision.wrapping_add(1);
            queued.insert(item.id.clone());
        }
    }
    n.seen_queue = queued.into_iter().collect();
}
pub(super) fn toggle_saved(n: &mut NewsPane, story: &Cluster) {
    let saved = !story.members.iter().any(|member| member.bookmark);
    for item in n.items.iter_mut().chain(&mut n.pending) {
        if story.member_ids.contains(&item.id) {
            item.bookmark = saved;
            n.local_bookmarks.insert(item.id.clone(), saved);
        }
    }
    n.bookmark_queue.push((story.member_ids.clone(), saved));
    n.item_revision = n.item_revision.wrapping_add(1);
}
fn chart_ticker<'a>(story: &'a Cluster, linked: Option<&str>, watched: &[String]) -> Option<&'a String> {
    if story.subjects.is_empty() {
        fallback_ticker(story, linked, watched)
    } else {
        story.subjects.iter().find(|ticker| Some(ticker.as_str()) != linked).or_else(|| story.subjects.first())
    }
}
/// Readable story preview: a solid-edged card (the pane colour alone blends into charts),
/// headline first, then source/time/tickers, the summary at a reading width, and the host.
fn hover_card(title: &egui::Response, story: &Cluster, model: &RowModel) {
    let summary = story.lead.summary.trim();
    crate::feed_time::tooltip(title).show(|ui| {
        ui.spacing_mut().item_spacing.y = dp(4.0);
        ui.add(
            egui::Label::new(RichText::new(&story.lead.item.title).font(ui_font(13.5)).color(theme::text()).strong())
                .wrap(),
        );
        let meta = [story.sources.join(", "), model.full_time.clone(), model.about.clone()];
        ui.add(egui::Label::new(RichText::new(meta.join(" · ")).font(mono(10.5)).color(theme::muted())).wrap());
        if !summary.is_empty() {
            ui.add_space(dp(2.0));
            let clipped: String = summary.chars().take(600).collect();
            let clipped = if clipped.len() < summary.len() { format!("{}…", clipped.trim_end()) } else { clipped };
            ui.add(egui::Label::new(RichText::new(clipped).font(ui_font(12.5)).color(theme::text())).wrap());
        }
        if let Some(host) =
            reqwest::Url::parse(&story.lead.item.link).ok().and_then(|u| u.host_str().map(str::to_owned))
        {
            ui.add_space(dp(2.0));
            ui.label(
                RichText::new(format!("{} · click to open", host.trim_start_matches("www.")))
                    .font(ui_font(10.5))
                    .color(theme::muted()),
            );
        }
    });
}

fn open(ui: &Ui, story: &Cluster) {
    if !tr_data::news_store::normalize_link(&story.lead.item.link).is_empty() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(&story.lead.item.link));
    }
}
fn toggle_expanded(n: &mut NewsPane, story: &Cluster) {
    let expanded = story.member_ids.iter().any(|id| n.interaction.expanded.contains(id));
    for id in &story.member_ids {
        if expanded {
            n.interaction.expanded.remove(id);
        } else {
            n.interaction.expanded.insert(id.clone());
        }
    }
    mark_seen(n, story);
}
fn keyword_click(n: &mut NewsPane, count: usize) {
    if count == 0 {
        n.interaction.keyword_add_open = true;
        n.display.watch_hits = false;
    } else {
        n.display.watch_hits = !n.display.watch_hits;
    }
}
fn add_keyword(n: &mut NewsPane, watches: &mut Vec<KeywordWatch>) {
    let words = n.interaction.keyword_input.trim();
    if words.is_empty() {
        return;
    }
    watches.push(KeywordWatch { name: words.into(), words: words.into(), ..Default::default() });
    n.interaction.keyword_input.clear();
    n.interaction.keyword_add_open = false;
    n.interaction.watches_dirty = None;
    n.interaction.save_watches = true;
}
fn keyword_field(ui: &mut Ui, n: &mut NewsPane, watches: &mut Vec<KeywordWatch>, focus: bool) -> bool {
    let field = ui.add(
        egui::TextEdit::singleline(&mut n.interaction.keyword_input)
            .hint_text("Add keyword or \"quoted phrase\"")
            .desired_width(dp(230.0)),
    );
    if focus {
        field.request_focus();
    }
    if ui.input_mut(|i| consume_exact(i, egui::Key::Escape)) {
        field.surrender_focus();
        true
    } else if ui.input_mut(|i| consume_exact(i, egui::Key::Enter)) {
        add_keyword(n, watches);
        field.surrender_focus();
        true
    } else {
        false
    }
}
fn chips(ui: &mut Ui, n: &mut NewsPane, watches: &mut Vec<KeywordWatch>, unread: usize, inline: bool) -> bool {
    ui.toggle_value(&mut n.display.unread, format!("Unread {unread}")).on_hover_text("Unread only (u)");
    let chip = ui.selectable_label(n.display.watch_hits, format!("Keywords {}", watches.len()));
    if chip.clicked() {
        keyword_click(n, watches.len());
        if inline && n.interaction.keyword_add_open {
            n.interaction.keyword_add_inline = true;
            n.interaction.keyword_add_open = false;
        }
    }
    if !inline {
        let mut open = n.interaction.keyword_add_open;
        let mut close = false;
        egui::Popup::from_response(&chip)
            .open_bool(&mut open)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                close = keyword_field(ui, n, watches, chip.clicked());
                if close {
                    ui.close();
                }
            });
        n.interaction.keyword_add_open = open && !close;
    }
    chip.clicked()
}
/// Shared by the pane footer and Help, including the parallel Save story action.
pub(super) const NEWS_SHORTCUTS: &[(&str, &str)] = &[
    ("↓/j", "Next story"),
    ("↑/k", "Previous story"),
    ("Enter", "Open story"),
    ("Space", "Expand/collapse story"),
    ("C", "Chart story"),
    ("/", "Search"),
    ("U", "Unread only"),
    ("W", "Stories / Wire (selected row)"),
    ("S", "Save story"),
    ("R", "Newest / Relevance (selected row)"),
    ("Esc", "Clear search / selection"),
];

fn key_legend_width(ui: &Ui) -> f32 {
    NEWS_SHORTCUTS
        .iter()
        .map(|(key, action)| {
            ui.painter().layout_no_wrap((*key).into(), mono(10.0), theme::text()).size().x
                + ui.painter().layout_no_wrap((*action).into(), ui_font(10.5), theme::muted()).size().x
                + dp(24.0) // Key padding and inter-item spacing, including a trailing gap.
        })
        .sum()
}

fn key_legend(ui: &mut Ui) {
    // Only shown when the complete line fits. No scrolling/animation or repaint timer:
    // compact panes keep their two-line chrome; the same bindings remain in Help.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = dp(4.0);
        for (key, action) in NEWS_SHORTCUTS {
            let galley = ui.painter().layout_no_wrap((*key).into(), mono(10.0), theme::text());
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(galley.size().x + dp(8.0), dp(18.0)), egui::Sense::hover());
            ui.painter().rect_filled(rect, dp(4.0), theme::control());
            ui.painter().rect_stroke(
                rect,
                dp(4.0),
                egui::Stroke::new(dp(1.0), theme::border()),
                egui::StrokeKind::Inside,
            );
            ui.painter().galley(
                egui::pos2(rect.left() + dp(4.0), rect.center().y - galley.size().y / 2.0),
                galley,
                theme::text(),
            );
            ui.add(egui::Label::new(RichText::new(*action).font(ui_font(10.5)).color(theme::muted())).extend());
            ui.add_space(dp(4.0));
        }
    });
}

#[derive(Default)]
pub(super) struct SourceCache {
    pub key: Option<super::data_sources::NewsSourceKey>,
    pub summary: String,
    pub hover: String,
}
impl SourceCache {
    pub fn rebuild(&mut self, sources: &[SourceStatus], now: i64) {
        self.summary = aggregate_status(sources, now);
        self.hover = sources.iter().map(|s| s.line(now)).collect::<Vec<_>>().join("\n");
    }
}
#[derive(Clone, Debug)]
pub(super) struct SourceStatus {
    pub name: String,
    pub status: String,
    pub reason: Option<String>,
    pub newest: Option<i64>,
    pub updated: Option<i64>,
}
impl SourceStatus {
    fn enabled(&self) -> bool {
        self.status != "Off"
    }
    fn paused(&self) -> bool {
        self.enabled() && !matches!(self.status.as_str(), "OK" | "Quiet" | "Pending")
    }
    fn line(&self, now: i64) -> String {
        let age = self.newest.map(|at| source_age(at, now)).unwrap_or_else(|| "—".into());
        let reason = self.reason.as_deref().filter(|s| !s.is_empty()).map(|s| format!(" — {s}")).unwrap_or_default();
        format!("{} · {}{reason} · newest {age}", self.name, self.status)
    }
}
fn source_age(at: i64, now: i64) -> String {
    match (now - at).max(0) {
        0..60 => "now".into(),
        60..3600 => format!("{}m ago", (now - at) / 60),
        3600..86400 => format!("{}h ago", (now - at) / 3600),
        _ => format!("{}d ago", (now - at) / 86400),
    }
}
fn aggregate_status(sources: &[SourceStatus], now: i64) -> String {
    let count = sources.iter().filter(|s| s.enabled()).count();
    let paused = sources.iter().filter(|s| s.paused()).count();
    let updated = sources.iter().filter(|s| s.enabled()).filter_map(|s| s.updated).max();
    let age = updated.map(|at| format!("updated {}", source_age(at, now))).unwrap_or_else(|| "not updated yet".into());
    let pending = sources.iter().filter(|s| s.status == "Pending").count();
    let mut parts = vec![format!("{count} sources")];
    if paused > 0 {
        parts.push(format!("{paused} paused"));
    }
    if pending > 0 {
        parts.push(format!("{pending} pending"));
    }
    parts.push(age);
    parts.join(" · ")
}
pub(super) fn pane_source(n: &NewsPane) -> &'static str {
    if n.status.contains("CoinDesk") {
        "CoinDesk"
    } else if n.status.contains("Yahoo") {
        "Yahoo"
    } else {
        "Finnhub"
    }
}
fn source_details(ui: &mut Ui, n: &mut NewsPane, actions: &mut Vec<Action>) {
    ui.label(RichText::new(&n.interaction.source_cache.hover).font(mono(10.5)).color(theme::muted()));
    if ui.button("Open Data sources").clicked() {
        n.interaction.open_data_sources = true;
        actions.push(Action::Command("SETTINGS".into()));
        ui.close();
    }
}
fn source_matches(source: &str, filter: &str) -> bool {
    source.to_lowercase().contains(&filter.trim().to_lowercase())
}

fn active_filter_summary(n: &NewsPane) -> String {
    let mut parts = vec![];
    if n.filters.saved {
        parts.push("Saved only".into());
    }
    if !n.filters.search.trim().is_empty() {
        parts.push(format!("Search: {}", n.filters.search.trim()));
    }
    if n.display.sort == intelligence::Sort::Relevance {
        parts.push("Relevance".into());
    }
    if !n.filters.sources.is_empty() {
        parts.push(format!("Sources: {}", n.filters.sources.join(", ")));
    }
    if !n.filters.categories.is_empty() {
        parts.push(format!("Categories: {}", n.filters.categories.join(", ").to_uppercase()));
    }
    if !n.filters.include.trim().is_empty() {
        parts.push(format!("Include any: {}", n.filters.include.trim()));
    }
    if !n.filters.exclude.trim().is_empty() {
        parts.push(format!("Exclude any: {}", n.filters.exclude.trim()));
    }
    if n.display.unread {
        parts.push("Unread only".into());
    }
    if n.display.watch_hits {
        parts.push("Keywords".into());
    }
    parts.join(" · ")
}

fn toggle_category(categories: &mut Vec<String>, category: &str, multi: bool) {
    categories.retain(|c| c != "none"); // Legacy explicit-empty selection remains loadable.
    if !multi {
        if categories.as_slice() == [category] {
            categories.clear();
        } else {
            *categories = vec![category.into()];
        }
    } else if categories.iter().any(|c| c == category) {
        categories.retain(|c| c != category);
    } else {
        categories.push(category.into());
    }
}
pub(super) fn category_code(category: &str) -> &'static str {
    match tr_data::news_store::category_key(category) {
        "markets" => "MKT",
        "macro" => "MACRO",
        "regulatory" => "REG",
        "geopolitics" => "GEO",
        "tech" => "TECH",
        "crypto" => "CRYPTO",
        _ => "",
    }
}
fn publisher_sources(n: &NewsPane) -> std::collections::BTreeSet<String> {
    n.items
        .iter()
        .flat_map(|s| s.sources.iter().cloned().chain([s.source.clone()]))
        .chain(n.filters.sources.iter().cloned())
        .filter(|s| {
            !s.is_empty()
                && !matches!(s.to_ascii_lowercase().as_str(), "rss" | "finnhub" | "yahoo" | "yahoo-news")
                && !tr_data::feeds::FEEDS.iter().any(|f| f.id == s)
        })
        .collect()
}
fn filtered_empty_text(n: &NewsPane, keywords: usize) -> String {
    let summary = active_filter_summary(n);
    if n.display.watch_hits {
        let others = summary.split(" · ").filter(|s| *s != "Keywords").collect::<Vec<_>>().join(" · ");
        let noun = if keywords == 1 { "keyword" } else { "keywords" };
        let base = format!("No stories match your {keywords} {noun}");
        if others.is_empty() { base } else { format!("{base} · {others}") }
    } else if summary.is_empty() {
        if n.filters.days == 1 {
            "No stories match Today".into()
        } else {
            format!("No stories match the last {} days", n.filters.days)
        }
    } else {
        format!("No stories match: {summary}")
    }
}
fn filters(ui: &mut Ui, n: &mut NewsPane, watches: &mut Vec<KeywordWatch>, narrow: bool, unread: usize) {
    let mut changed = false;
    let mut finished = false;
    if narrow {
        let clicked = ui.horizontal(|ui| chips(ui, n, watches, unread, true)).inner;
        if n.interaction.keyword_add_inline && keyword_field(ui, n, watches, clicked) {
            n.interaction.keyword_add_inline = false;
        }
    }
    ui.horizontal(|ui| {
        ui.label("Sort (R)");
        ui.selectable_value(&mut n.display.sort, intelligence::Sort::Newest, "Newest");
        ui.selectable_value(&mut n.display.sort, intelligence::Sort::Relevance, "Relevance")
            .on_hover_text("Dated stories in the last 24h: priority, distinct sources, then time. Older or undated stories: newest first.");
    });
    ui.checkbox(&mut n.filters.saved, "Saved");
    ui.label("Category");
    ui.horizontal_wrapped(|ui| {
        if ui.selectable_label(n.filters.categories.is_empty(), "All").clicked() {
            n.filters.categories.clear();
        }
        for category in tr_data::feeds::CATEGORIES {
            let selected = n.filters.categories.iter().any(|c| c == category);
            if ui
                .selectable_label(selected, category.to_uppercase())
                .on_hover_text("Click to select only this category; Cmd-click (Ctrl on Linux) to add/remove")
                .clicked()
            {
                toggle_category(&mut n.filters.categories, category, ui.input(|i| i.modifiers.command));
            }
        }
    });
    ui.label("Sources");
    let sources = publisher_sources(n);
    ui.add(
        egui::TextEdit::singleline(&mut n.interaction.source_filter)
            .hint_text("Filter sources (contains, ignoring case)")
            .desired_width(dp(260.0)),
    );
    let mut matching: Vec<_> = sources
        .iter()
        .filter(|s| source_matches(s, &n.interaction.source_filter))
        .map(|source| {
            let category = n
                .items
                .iter()
                .find(|s| &s.source == source || s.sources.contains(source))
                .map(|s| tr_data::news_store::category_key(&s.category).to_string())
                .or_else(|| tr_data::feeds::FEEDS.iter().find(|f| f.publisher == source).map(|f| f.category.into()))
                .unwrap_or_default();
            (category, source)
        })
        .collect();
    matching.sort();
    ui.label(
        RichText::new(format!("{} of {} sources match", matching.len(), sources.len()))
            .font(mono(10.5))
            .color(theme::muted()),
    );
    if ui.selectable_label(n.filters.sources.is_empty(), "All sources").clicked() {
        n.filters.sources.clear();
    }
    // Pin active legacy filters above the publisher list, even when a publisher
    // search would hide them, so every saved selection can be removed individually.
    let legacy: Vec<_> = n.filters.sources.iter().filter(|s| !sources.contains(*s)).cloned().collect();
    for source in legacy {
        if ui.selectable_label(true, format!("{source} (legacy)")).clicked() {
            n.filters.sources.retain(|s| s != &source);
        }
    }
    egui::ScrollArea::vertical().id_salt("news-filter-sources").max_height(dp(140.0)).show(ui, |ui| {
        let mut previous = None;
        for (category, source) in matching {
            if previous.as_ref() != Some(&category) {
                ui.label(
                    RichText::new(if category.is_empty() {
                        "Other publishers".into()
                    } else {
                        category.to_uppercase()
                    })
                    .color(theme::muted())
                    .font(ui_font(10.5)),
                );
                previous = Some(category);
            }
            let selected = n.filters.sources.contains(source);
            if ui.selectable_label(selected, source).clicked() {
                if selected {
                    n.filters.sources.retain(|s| s != source);
                } else {
                    n.filters.sources.push(source.clone());
                }
            }
        }
    });
    ui.label("Include words (any)");
    ui.add(
        egui::TextEdit::singleline(&mut n.filters.include)
            .hint_text("Any whole word; case-insensitive, not a phrase")
            .desired_width(dp(260.0)),
    );
    ui.label("Exclude words");
    ui.add(
        egui::TextEdit::singleline(&mut n.filters.exclude)
            .hint_text("Reject any whole word; case-insensitive, not a phrase")
            .desired_width(dp(260.0)),
    );
    ui.add(
        egui::Label::new(
            RichText::new("Separate words with spaces or punctuation; quotes do not make phrases.")
                .font(ui_font(10.5))
                .color(theme::muted()),
        )
        .wrap(),
    );
    ui.label("Keywords · words or \"quoted phrases\"");
    let mut delete = None;
    for (i, watch) in watches.iter_mut().enumerate() {
        ui.push_id(i, |ui| {
            ui.horizontal(|ui| {
                let response =
                    ui.add(egui::TextEdit::singleline(&mut watch.name).hint_text("Name").desired_width(dp(100.0)));
                changed |= response.changed();
                finished |= response.lost_focus();
                if ui.small_button("Delete").clicked() {
                    delete = Some(i);
                }
            });
            let response =
                ui.add(egui::TextEdit::singleline(&mut watch.words).hint_text("Words").desired_width(dp(210.0)));
            changed |= response.changed();
            finished |= response.lost_focus();
            let response = ui.checkbox(&mut watch.notify, "Notify");
            changed |= response.changed();
            finished |= response.changed();
            ui.horizontal(|ui| {
                for colour in 0..6 {
                    let response = crate::feed_time::icon_button(
                        ui,
                        crate::widgets::Icon::Circle,
                        &format!("Keyword colour {}", colour + 1),
                    );
                    ui.painter().circle_filled(response.rect.center(), dp(3.), watch_colour(colour));
                    if watch.colour.min(5) == colour {
                        ui.painter().rect_stroke(
                            response.rect,
                            theme::CONTROL_RADIUS,
                            egui::Stroke::new(dp(1.), theme::accent()),
                            egui::StrokeKind::Inside,
                        );
                    }
                    if response.clicked() {
                        changed |= watch.colour != colour;
                        watch.colour = colour;
                    }
                }
            });
        });
    }
    if let Some(i) = delete {
        watches.remove(i);
        changed = true;
    }
    if ui.small_button("Add keyword").clicked() {
        watches.push(KeywordWatch::default());
        changed = true;
    }
    if changed {
        n.interaction.watches_dirty = Some(std::time::Instant::now());
    }
    if finished && n.interaction.watches_dirty.take().is_some() {
        n.interaction.save_watches = true;
    }
}

pub(super) fn window_visible(viewport: &egui::ViewportInfo) -> bool {
    viewport.minimized != Some(true) && viewport.visible() != Some(false)
}

fn current_volume(n: &NewsPane, now: i64) -> Option<&tr_data::news_store::local::Volume> {
    let symbols = n.query.as_ref().map(|q| q.symbols.as_slice()).unwrap_or_default();
    n.interaction
        .volumes
        .iter()
        .filter(|v| {
            v.hour == tr_data::news_store::local::local_hour(now)
                && v.hour != n.interaction.dismissed_volume.unwrap_or(-1)
                && match n.filters.scope {
                    Scope::Symbol => n.symbol.as_ref() == Some(&v.key),
                    Scope::Watchlist => v.key.starts_with("Topic: ") || symbols.contains(&v.key),
                    Scope::All => v.key.starts_with("Topic: "),
                }
        })
        .min_by_key(|v| v.key.starts_with("Topic: "))
}

pub(super) fn show(
    ui: &mut Ui,
    n: &mut NewsPane,
    lists: &[NamedWatchlist],
    watches: &mut Vec<KeywordWatch>,
    focused: bool,
    link: Option<crate::layouts::Link>,
    active_watchlist: usize,
) -> Vec<Action> {
    if focused {
        n.last_focused = Some(std::time::Instant::now());
    }
    if let Some(text) = n.interaction.digest_text.take() {
        ui.ctx().copy_text(text);
    }
    if let Some(v) = current_volume(n, chrono::Utc::now().timestamp()) {
        let hour = v.hour;
        let message = v.message();
        ui.horizontal(|ui| {
            ui.label(RichText::new(message).font(ui_font(10.5)).color(theme::muted()));
            if ui.small_button("Dismiss").clicked() {
                n.interaction.dismissed_volume = Some(hour);
            }
        });
    }
    let before = n.filters.clone();
    let mut actions = vec![];
    let now = chrono::Utc::now().timestamp();
    let narrow = ui.available_width() < dp(360.0);
    let relative = n
        .updated
        .map(|at| crate::feed_time::feed_time(now - at.elapsed().as_secs() as i64, now, false).0)
        .unwrap_or_else(|| "now".into());
    let update = if relative == "now" {
        "updated now".into()
    } else if relative.ends_with('m') {
        format!("updated {relative} ago")
    } else {
        format!("updated {relative}")
    };
    let pane_status = n.error.as_deref().unwrap_or(&n.status).replace("updated just now", &update);
    let status = n.interaction.source_cache.summary.clone();
    let status_hover = n.interaction.source_cache.hover.clone();
    let watchlist_name = lists
        .iter()
        .find(|l| Some(&l.name) == n.filters.watchlist.as_ref())
        .or_else(|| lists.get(active_watchlist))
        .map(|l| l.name.clone())
        .unwrap_or_else(|| "Watchlist".into());
    let watched: Vec<_> = lists.iter().flat_map(|list| list.symbols.iter().cloned()).collect();
    let mut cache = std::mem::take(&mut n.cache);
    cache.update(n, watches, &watched, now);
    let unread = cache.unread;
    ui.horizontal_top(|ui| {
        chrome(ui);
        // Reserve the overflow first, at the right edge, independent of status length.
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), dp(20.)),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let more = crate::feed_time::icon_button(ui, crate::widgets::Icon::More, "News options");
                egui::Popup::menu(&more).show(|ui| {
                    ui.add(egui::Label::new(&pane_status).wrap());
                    ui.add(egui::Label::new(&status).wrap());
                    source_details(ui, n, &mut actions);
                    ui.checkbox(&mut n.flash_high, "Flash high-priority");
                    ui.separator();
                    if ui
                        .button("Copy digest")
                        .on_hover_text("Copy a 24-hour digest from local headlines and filing metadata")
                        .clicked()
                    {
                        n.interaction.copy_digest = true;
                        ui.close();
                    }
                    if ui
                        .button("Reload saved headlines")
                        .on_hover_text("Re-read saved history; source pacing is unchanged")
                        .clicked()
                    {
                        n.fetched = None;
                        n.read_in_view.clear();
                        n.item_revision = n.item_revision.wrapping_add(1);
                        ui.close();
                    }
                    if ui
                        .button("Check for new headlines")
                        .on_hover_text(format!(
                            "Fetch now from {} (guard limits apply). RSS feeds keep their own 15–30 min timers.",
                            pane_source(n)
                        ))
                        .clicked()
                    {
                        n.refresh_source = true;
                        n.fetched = None;
                        ui.close();
                    }
                    if ui.button(if n.paused { "Unfreeze list" } else { "Freeze list" }).clicked() {
                        n.paused = !n.paused;
                        if !n.paused && !n.pending.is_empty() {
                            let incoming = std::mem::take(&mut n.pending);
                            n.apply(incoming);
                        }
                        ui.close();
                    }
                    ui.menu_button("About sources", |ui| {
                        ui.label("Finnhub free tier: personal, non-commercial use.");
                        ui.label("Yahoo: headlines and links; publishers own the content.");
                        ui.label("CoinDesk: headlines and links; publisher owns the content.");
                        ui.label("Built-in RSS feeds: headlines and links only; publishers own the content.");
                    });
                });
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), dp(20.)),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.selectable_value(&mut n.filters.scope, Scope::Symbol, "Symbol");
                        ui.selectable_value(&mut n.filters.scope, Scope::Watchlist, "Watchlist");
                        ui.selectable_value(&mut n.filters.scope, Scope::All, "Market");
                        if n.filters.scope == Scope::Watchlist {
                            egui::ComboBox::from_id_salt("news-watchlist")
                                .width(dp(70.0))
                                .wrap_mode(egui::TextWrapMode::Truncate)
                                .selected_text(&watchlist_name)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut n.filters.watchlist, None, "Active watchlist");
                                    for list in lists {
                                        ui.selectable_value(
                                            &mut n.filters.watchlist,
                                            Some(list.name.clone()),
                                            &list.name,
                                        );
                                    }
                                });
                        } else if n.filters.scope == Scope::Symbol {
                            if let Some(symbol) = &n.symbol {
                                ui.label(
                                    RichText::new(symbol.split_whitespace().next().unwrap_or(symbol))
                                        .font(mono(12.0))
                                        .color(theme::amber()),
                                );
                            }
                            if let Some(link) = link {
                                crate::feed_time::dot(ui, link.color());
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if n.paused
                                && ui
                                    .small_button(format!("{} new · Show", cache.pending_count))
                                    .on_hover_text("Updates paused in this pane; show new saved headlines")
                                    .clicked()
                            {
                                n.paused = false;
                                if !n.pending.is_empty() {
                                    let incoming = std::mem::take(&mut n.pending);
                                    n.apply(incoming);
                                }
                            }
                            ui.add(
                                egui::Label::new(RichText::new(&status).font(ui_font(10.5)).color(theme::muted()))
                                    .truncate(),
                            )
                            .on_hover_text(&status_hover);
                        });
                    },
                );
            },
        );
    });
    ui.horizontal(|ui| {
        chrome(ui);
        let ranges = [(1, "Today"), (7, "7D"), (30, "30D"), (90, "90D")];
        egui::ComboBox::from_id_salt("news-days")
            .width(dp(48.0))
            .selected_text(ranges.iter().find(|(days, _)| *days == n.filters.days).map_or("Today", |(_, label)| *label))
            .show_ui(ui, |ui| {
                for (days, label) in ranges {
                    ui.selectable_value(&mut n.filters.days, days, label);
                }
            });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = dp(0.0);
            ui.selectable_value(&mut n.display.wire, false, "Stories")
                .on_hover_text("Stories — duplicates merged; ×N = number of sources");
            ui.selectable_value(&mut n.display.wire, true, "Wire").on_hover_text("Wire — every headline as it arrived");
        });
        if !narrow {
            chips(ui, n, watches, unread, false);
        }
        // Count only filters hidden from the header: Unread and Keywords count
        // in narrow panes only, using the same rules as the wide header chips.
        let count = n.filters.sources.len()
            + usize::from(!n.filters.categories.is_empty())
            + usize::from(n.filters.saved)
            + usize::from(!n.filters.include.is_empty())
            + usize::from(!n.filters.exclude.is_empty())
            + usize::from(narrow && n.display.unread)
            + usize::from(narrow && n.display.watch_hits);
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            let filter_label = format!("Filters ({count})");
            let filter_width =
                ui.painter().layout_no_wrap(filter_label.clone(), ui_font(10.5), theme::text()).size().x + dp(12.0);
            // The Keywords chip appears at 360pt, but that does not guarantee
            // enough room for an editable search field beside Filters.
            let compact_search = narrow || ui.available_width() < filter_width + dp(60.0);
            if compact_search && !n.interaction.search_open && n.filters.search.is_empty() {
                if crate::feed_time::icon_button(ui, crate::widgets::Icon::Search, "Search (/)").clicked() {
                    n.interaction.search_open = true;
                    n.interaction.focus_search = true;
                }
            } else {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut n.filters.search)
                        .hint_text("Search")
                        // Leave room for the field margins and gap before Filters,
                        // including when search is explicitly opened in a compact pane.
                        .desired_width((ui.available_width() - filter_width - dp(12.0)).clamp(0.0, dp(210.0))),
                );
                if n.interaction.focus_search {
                    response.request_focus();
                    n.interaction.focus_search = false;
                }
                if focused && response.has_focus() && ui.input_mut(|i| consume_exact(i, egui::Key::Escape)) {
                    if n.filters.search.is_empty() {
                        n.interaction.selected = None;
                    } else {
                        n.filters.search.clear();
                    }
                    response.surrender_focus();
                }
                if response.lost_focus() && n.filters.search.is_empty() {
                    n.interaction.search_open = false;
                }
            }
            let button = ui.button(filter_label);
            let menu = egui::Popup::menu(&button)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show(|ui| filters(ui, n, watches, narrow, unread));
            if menu.is_none() || !narrow {
                n.interaction.keyword_add_inline = false;
            }
            if narrow {
                // The compact editor belongs only to this menu, never to a later wide popup.
                n.interaction.keyword_add_open = false;
            }
        });
    });
    cache.update(n, watches, &watched, now);
    if n.flash_high && !narrow {
        show_flash(ui, n, &cache.flash, now);
    }
    let summary = active_filter_summary(n);
    if !summary.is_empty() {
        ui.add(egui::Label::new(RichText::new(&summary).font(ui_font(10.5)).color(theme::muted())).truncate())
            .on_hover_text(summary);
    }
    let text_focused = ui.memory(|m| m.focused()).is_some_and(|id| egui::TextEdit::load_state(ui.ctx(), id).is_some());
    if focused
        && !text_focused
        && n.interaction.selected.is_some()
        && (n.interaction.pending_keys.contains(&egui::Key::W) || ui.input_mut(|i| consume_exact(i, egui::Key::W)))
    {
        n.display.wire = !n.display.wire;
        ui.ctx().request_repaint();
    }
    if focused
        && !text_focused
        && n.interaction.selected.is_some()
        && (n.interaction.pending_keys.contains(&egui::Key::R) || ui.input_mut(|i| consume_exact(i, egui::Key::R)))
    {
        n.display.sort = match n.display.sort {
            intelligence::Sort::Newest => intelligence::Sort::Relevance,
            intelligence::Sort::Relevance => intelligence::Sort::Newest,
        };
        ui.ctx().request_repaint(); // Input only; no sort timer.
    }
    cache.update(n, watches, &watched, now);
    let ids = &cache.ids;
    let rows = &cache.rows;
    if focused && !text_focused {
        let keys = [
            (egui::Key::ArrowDown, Key::Next),
            (egui::Key::J, Key::Next),
            (egui::Key::ArrowUp, Key::Previous),
            (egui::Key::K, Key::Previous),
            (egui::Key::Enter, Key::Open),
            (egui::Key::Space, Key::Expand),
            (egui::Key::C, Key::Chart),
            (egui::Key::S, Key::Save),
            (egui::Key::Slash, Key::Search),
            (egui::Key::Escape, Key::Escape),
            (egui::Key::U, Key::Unread),
        ];
        for (egui_key, key) in keys {
            if !owns_key(egui_key, n.interaction.selected.is_some(), !n.filters.search.is_empty()) {
                continue;
            }
            let queued = n.interaction.pending_keys.contains(&egui_key);
            if !queued && !ui.input_mut(|i| consume_exact(i, egui_key)) {
                continue;
            }
            ui.ctx().request_repaint(); // Input-driven state change, never an idle loop.
            let old = n.interaction.selected.clone();
            let effect = reduce(key, ids, &mut n.interaction.selected, &mut n.filters.search, &mut n.display.unread);
            if old != n.interaction.selected {
                n.interaction.dwell = n.interaction.selected.clone().map(|id| (id, std::time::Instant::now()));
                n.interaction.scroll_to = n.interaction.selected.clone();
            }
            if effect == Effect::Search {
                n.interaction.search_open = true;
                n.interaction.focus_search = true;
                ui.ctx().request_repaint();
            }
            if let Some(story) =
                rows.iter().map(|r| &r.story).find(|s| Some(&s.lead.id) == n.interaction.selected.as_ref())
            {
                match effect {
                    Effect::Open => {
                        open(ui, story);
                        mark_seen(n, story);
                    }
                    Effect::Expand => toggle_expanded(n, story),
                    Effect::Save => toggle_saved(n, story),
                    Effect::Chart => {
                        let linked = n.symbol.as_deref().filter(|_| n.filters.scope == Scope::Symbol);
                        if let Some(t) = chart_ticker(story, linked, &watched) {
                            actions.push(Action::OpenChart(t.clone()));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    n.interaction.pending_keys.clear();
    // A dwell is armed only by a user selection action, never by refresh.
    if let Some(at) = n.interaction.watches_dirty {
        let wait = std::time::Duration::from_millis(700).saturating_sub(at.elapsed());
        if !text_focused
            || wait.is_zero()
            || ui.input(|i| {
                i.events.iter().any(|e| {
                    matches!(
                        e,
                        egui::Event::Key { key: egui::Key::Enter, pressed: true, modifiers: egui::Modifiers::NONE, .. }
                    )
                })
            })
        {
            n.interaction.save_watches = true;
            n.interaction.watches_dirty = None;
        } else {
            ui.ctx().request_repaint_after(wait);
        }
    }
    if let Some((id, at)) = n.interaction.dwell.clone() {
        if focused && !text_focused && n.interaction.selected.as_ref() == Some(&id) {
            if let Some(story) = rows.iter().map(|r| &r.story).find(|s| s.lead.id == id && s.unread()) {
                let wait = std::time::Duration::from_secs(1).saturating_sub(at.elapsed());
                if wait.is_zero() {
                    mark_seen(n, story);
                    n.interaction.dwell = None;
                } else {
                    ui.ctx().request_repaint_after(wait);
                }
            } else {
                n.interaction.dwell = None;
            }
        } else {
            n.interaction.dwell = None;
        }
    }
    if rows.is_empty() {
        let filtered = n.filters.saved
            || !n.filters.sources.is_empty()
            || !n.filters.categories.is_empty()
            || !n.filters.include.is_empty()
            || !n.filters.exclude.is_empty()
            || !n.filters.search.is_empty()
            || n.display.unread
            || n.display.watch_hits
            || cache.older_match;
        let no_key = n.filters.scope == Scope::Symbol
            && n.symbol.is_some()
            && pane_status.contains("Company news needs a Finnhub key.");
        match empty_cause(n.loading, no_key, filtered) {
            EmptyCause::Loading => {
                ui.label(RichText::new("Reading saved headlines…").color(theme::muted()));
            }
            EmptyCause::NoKey => {
                ui.label(RichText::new("Company news needs a Finnhub key.").color(theme::muted()));
                if ui.small_button("Open Data sources").clicked() {
                    n.interaction.open_data_sources = true;
                    actions.push(Action::Command("SETTINGS".into()));
                }
            }
            EmptyCause::Filtered => {
                let message = filtered_empty_text(n, watches.len());
                ui.add(egui::Label::new(RichText::new(&message).color(theme::muted())).truncate())
                    .on_hover_text(message);
                if ui.small_button("Clear filters").clicked() {
                    let scope = n.filters.scope.clone();
                    let watchlist = n.filters.watchlist.clone();
                    n.filters =
                        tr_data::news_store::Filters { scope, watchlist, days: n.filters.days, ..Default::default() };
                    n.display.unread = false;
                    n.display.watch_hits = false;
                }
            }
            EmptyCause::NoHistory => {
                if let Some(message) = &cache.empty_message {
                    ui.add(egui::Label::new(RichText::new(message).color(theme::muted())).truncate())
                        .on_hover_text(message);
                }
                if ui.small_button("Reload").clicked() {
                    n.fetched = None;
                }
            }
        }
    }
    let linked = if n.filters.scope == Scope::Symbol { n.symbol.clone() } else { None };
    let scroll = n.interaction.scroll_to.take();
    let mut top = None;
    let restore = std::mem::take(&mut n.interaction.restore_anchor);
    let anchor = n.interaction.anchor.clone();
    let anchor_offset = n.interaction.anchor_offset;
    let expanded = !n.interaction.expanded.is_empty();
    if cache.layout_dirty
        || cache.layout_expanded != n.interaction.expanded
        || cache.layout_width != ui.available_width()
    {
        cache.offsets.clear();
        cache.offsets.push(0.0);
        for row in &mut cache.rows {
            if !n.interaction.expanded.contains(&row.model.id) {
                row.height = dp(22.0);
            } else if row.height == dp(22.0) {
                // Reserve an initial estimate even for an offscreen expansion;
                // visible rows replace it with their measured height below.
                row.height = dp(22.0
                    + 3.0 * 15.0
                    + 24.0
                    + if n.display.wire || row.story.members.len() == 1 {
                        0.0
                    } else {
                        22.0 * row.story.members.len() as f32
                    });
            }
            cache.offsets.push(cache.offsets.last().copied().unwrap_or_default() + row.height);
        }
        cache.layout_expanded.clone_from(&n.interaction.expanded);
        cache.layout_width = ui.available_width();
        cache.layout_dirty = false;
    }
    // Reserve a shared column only when the filtered Stories view needs it.
    let source_width =
        if !n.display.wire && cache.rows.iter().any(|row| row.model.source_count > 1) { dp(32.0) } else { dp(0.0) };
    let mut height_changed = false;
    let mut draw_row = |ui: &mut Ui, row: &mut CachedRow| {
        ui.spacing_mut().item_spacing.y = dp(0.0);
        let story = &row.story;
        let model = &row.model;
        let start = ui.next_widget_position().y;
        let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), dp(22.0)), egui::Sense::click());
        if n.interaction.selected.as_ref() == Some(&model.id) {
            ui.painter().rect_filled(rect, dp(0.0), theme::accent_wash());
            if focused {
                ui.painter().rect_stroke(
                    rect.shrink(dp(0.5)),
                    dp(0.),
                    egui::Stroke::new(dp(1.), theme::accent()),
                    egui::StrokeKind::Inside,
                );
            }
        } else if response.hovered() {
            ui.painter().rect_filled(rect, dp(0.0), theme::control_hover());
        }
        if let Some(colour) = row.stripe {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(
                    rect.min + egui::vec2(dp(1.0), dp(3.0)),
                    egui::vec2(dp(2.0), rect.height() - dp(6.0)),
                ),
                dp(0.0),
                watch_colour(colour),
            );
        }
        if model.unread {
            ui.painter().circle_filled(rect.left_center() + egui::vec2(dp(7.0), dp(0.0)), dp(2.0), theme::accent());
        }
        // Only unread/watch and (when needed) source strength remain before time.
        if !n.display.wire && model.source_count > 1 {
            ui.painter().text(
                egui::pos2(rect.left() + dp(12.0) + source_width, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                format!("×{}", model.source_count),
                mono(11.0),
                theme::muted(),
            );
        }
        let leading_width = dp(14.0) + source_width;
        let time_width = ui.fonts_mut(|f| f.glyph_width(&mono(11.0), '0')) * crate::feed_time::WIDTH_CHARS as f32;
        // A constant column keeps every headline starting at the same x, ticker or not.
        let ticker_width = time_width;
        let time_rect = egui::Rect::from_min_size(
            rect.min + egui::vec2(leading_width, dp(0.0)),
            egui::vec2(time_width, rect.height()),
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(time_rect).layout(egui::Layout::right_to_left(egui::Align::Center)),
            |ui| ui.add(egui::Label::new(RichText::new(&model.time).font(mono(11.0)).color(theme::muted())).truncate()),
        )
        .inner
        .on_hover_text(&model.full_time);
        let ticker_rect = egui::Rect::from_min_size(
            egui::pos2(time_rect.right() + dp(6.0), rect.top()),
            egui::vec2(ticker_width, rect.height()),
        );
        let mut child_clicked = false;
        if let Some(ticker) = &model.ticker {
            let r = ui
                .scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(ticker_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(ticker.split_whitespace().next().unwrap_or(ticker))
                                    .font(mono(11.0))
                                    .color(if model.ticker_is_subject { theme::amber() } else { theme::muted() }),
                            )
                            .truncate()
                            .sense(egui::Sense::click()),
                        )
                    },
                )
                .inner;
            let r = r.on_hover_text(&model.ticker_hover);
            if r.clicked() {
                actions.push(Action::OpenChart(ticker.clone()));
                child_clicked = true;
            }
        }
        let pill_width = if let Some(event) = model.event {
            let text = pill_text(event, rect.width());
            let (fill, ink) = pill_colours(model.priority);
            let galley = ui.painter().layout_no_wrap(text.into(), mono(9.0), ink);
            let width = if text.is_empty() { dp(10.0) } else { galley.size().x + dp(8.0) };
            let pill = egui::Rect::from_center_size(
                egui::pos2(ticker_rect.right() + dp(4.0) + width / 2.0, rect.center().y),
                egui::vec2(width, if text.is_empty() { dp(10.0) } else { galley.size().y + dp(4.0) }),
            );
            ui.painter().rect_filled(pill, dp(3.0), fill);
            if !text.is_empty() {
                ui.painter().galley(pill.center() - galley.size() / 2.0, galley, ink);
            }
            ui.interact(pill, ui.id().with((&model.id, "event")), egui::Sense::hover()).on_hover_text(format!(
                "{} · priority: {} · headline wording, not verified urgency · matched phrase: ‘{}’",
                event.label(),
                model.priority.label(),
                model.matched_phrase.unwrap_or_default()
            ));
            width + dp(4.0)
        } else {
            dp(0.0)
        };
        let headline_left = ticker_rect.right() + dp(4.0) + pill_width;
        let available = rect.right() - headline_left;
        let budget = crate::feed_time::tail_budget(rect.width(), available);
        let (meta_text, meta_width) = model
            .meta_steps
            .iter()
            .find_map(|text| {
                let code = category_code(&row.story.lead.category);
                let text = if n.filters.scope == Scope::All
                    && n.filters.categories.len() != 1
                    && !narrow
                    && !text.is_empty()
                    && !code.is_empty()
                {
                    format!("{code} · {text}")
                } else {
                    text.clone()
                };
                let width = ui.painter().layout_no_wrap(text.clone(), ui_font(10.5), theme::muted()).size().x;
                (width <= budget).then_some((text, width))
            })
            .expect("empty metadata always fits");
        let meta_rect = egui::Rect::from_min_max(
            egui::pos2((rect.right() - meta_width).max(ticker_rect.right()), rect.top()),
            rect.max,
        );
        let title_rect = egui::Rect::from_min_max(
            egui::pos2(headline_left, rect.top()),
            egui::pos2((meta_rect.left() - dp(6.0)).max(headline_left), rect.bottom()),
        );
        let mut job = egui::text::LayoutJob::default();
        let mut cursor = 0;
        let base = if model.unread { theme::text() } else { theme::muted() };
        for hit in &row.highlights {
            let range = hit.range.clone();
            job.append(
                &model.headline[cursor..range.start],
                dp(0.0),
                egui::TextFormat { font_id: ui_font(13.0), color: base, ..Default::default() },
            );
            let colour = watch_colour(hit.colour);
            job.append(
                &model.headline[range.clone()],
                dp(0.0),
                egui::TextFormat {
                    font_id: ui_font(13.0),
                    color: colour,
                    underline: egui::Stroke::new(dp(1.0), colour),
                    ..Default::default()
                },
            );
            cursor = range.end;
        }
        job.append(
            &model.headline[cursor..],
            dp(0.0),
            egui::TextFormat { font_id: ui_font(13.0), color: base, ..Default::default() },
        );
        // Real variable-font weight: one shaped run, never double-painted faux bold.
        if model.priority != intelligence::Priority::Normal {
            for section in &mut job.sections {
                section.format.coords = egui::epaint::text::VariationCoords::new([(b"wght", 680.0)]);
            }
        }
        // The hover card below replaces egui's elided-text tip, so only one popup ever shows.
        // Left-aligned in its column: `ui.put` would centre a headline shorter than the slot.
        let title = ui
            .scope_builder(
                egui::UiBuilder::new().max_rect(title_rect).layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    ui.add(egui::Label::new(job).truncate().show_tooltip_when_elided(false).sense(egui::Sense::click()))
                },
            )
            .inner;
        if title.clicked() {
            open(ui, story);
            mark_seen(n, story);
            child_clicked = true;
        }
        title.context_menu(|ui| {
            if ui
                .button(if story.members.iter().any(|member| member.bookmark) { "Unsave story" } else { "Save story" })
                .clicked()
            {
                toggle_saved(n, story);
                ui.close();
            }
        });
        hover_card(&title, story, model);
        let meta = ui
            .scope_builder(
                egui::UiBuilder::new().max_rect(meta_rect).layout(egui::Layout::right_to_left(egui::Align::Center)),
                |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(meta_text).font(ui_font(10.5)).color(theme::muted()))
                            .truncate()
                            .sense(egui::Sense::click()),
                    )
                },
            )
            .inner;
        if meta.clicked() {
            child_clicked = true;
            if model.extra_tickers.is_empty() {
                toggle_expanded(n, story);
            }
        }
        let meta = meta.on_hover_text(&model.about);
        if !model.extra_tickers.is_empty() {
            egui::Popup::menu(&meta).show(|ui| {
                for ticker in &model.extra_tickers {
                    if ui.button(ticker).clicked() {
                        actions.push(Action::OpenChart(ticker.clone()));
                        ui.close();
                    }
                }
            });
        }
        if response.clicked() && !child_clicked {
            toggle_expanded(n, story);
        }
        if n.interaction.expanded.contains(&model.id) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = dp(0.0);
                ui.add_space(time_width + ticker_width + leading_width + dp(10.0));
                ui.vertical(|ui| {
                    let mut summary = egui::text::LayoutJob::default();
                    summary.append(
                        &story.lead.summary,
                        dp(0.0),
                        egui::TextFormat { font_id: ui_font(12.0), color: theme::muted(), ..Default::default() },
                    );
                    summary.wrap.max_rows = 3;
                    ui.add(egui::Label::new(summary).wrap());
                    if !n.display.wire && story.members.len() > 1 {
                        for member in &story.members {
                            let (time, full) = if member.inferred_ts {
                                ("—".into(), "time unknown".into())
                            } else {
                                crate::feed_time::feed_time(member.item.ts, now, false)
                            };
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(time).font(mono(10.5)).color(theme::muted()))
                                    .on_hover_text(full);
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!("{} · {}", member.source, member.item.title))
                                            .font(ui_font(10.5))
                                            .color(theme::muted()),
                                    )
                                    .truncate(),
                                );
                            });
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui.small_button("Open").clicked() {
                            open(ui, story);
                            mark_seen(n, story);
                        }
                        if ui.small_button("Chart").clicked()
                            && let Some(t) = chart_ticker(story, linked.as_deref(), &watched)
                        {
                            actions.push(Action::OpenChart(t.clone()));
                        }
                        if ui
                            .small_button(if story.members.iter().any(|member| member.bookmark) {
                                "Unsave story"
                            } else {
                                "Save story"
                            })
                            .on_hover_text("Save/unsave story (S)")
                            .clicked()
                        {
                            toggle_saved(n, story);
                        }
                        if ui.small_button("Copy link").clicked() {
                            ui.ctx().copy_text(story.lead.item.link.clone());
                        }
                    });
                });
            });
        }
        // N7a: pointer selection publishes the same stable id as keyboard selection.
        if response.clicked() || child_clicked {
            n.interaction.selected = Some(model.id.clone());
            n.interaction.dwell = Some((model.id.clone(), std::time::Instant::now()));
        }
        let height = (ui.next_widget_position().y - start).max(dp(22.0));
        if top.is_none() && start + height > ui.clip_rect().top() {
            top = Some((model.id.clone(), start - ui.clip_rect().top()));
        }
        height_changed |= row.height != height;
        row.height = height;
    };
    // Collapsed rows use egui's fixed-height virtual list. Expanded rows retain
    // measured heights and virtualise by prefix offsets rather than lying about
    // their height to show_rows.
    let show_legend = ui.available_height() >= dp(160.0) && ui.available_width() >= key_legend_width(ui);
    let footer_height = if show_legend { dp(28.0) } else { 0.0 };
    let mut area = egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height((ui.available_height() - footer_height).max(0.0));
    let offsets = &cache.offsets;
    let row_y = |index: usize| if expanded { offsets[index] } else { index as f32 * dp(22.0) };
    if restore && let Some(index) = cache.ids.iter().position(|id| Some(id) == anchor.as_ref()) {
        area = area.vertical_scroll_offset((row_y(index) - anchor_offset).max(0.0));
    }
    let scroll_index = scroll.as_ref().and_then(|id| cache.ids.iter().position(|s| s == id));
    let scroll_selection = |ui: &mut Ui, origin: f32| {
        if let Some(index) = scroll_index {
            let rect = egui::Rect::from_min_size(
                egui::pos2(ui.min_rect().left(), origin + row_y(index)),
                egui::vec2(ui.available_width(), dp(22.0)),
            );
            // None means just bring an offscreen row into view, not pin it.
            ui.scroll_to_rect(rect, None);
        }
    };
    ui.spacing_mut().item_spacing.y = dp(0.0);
    // The two virtualisation paths create different intermediate UIs. Give
    // each row an absolute pane-local UI identity so clicks/focus remain stable
    // across expansion and when the visible range moves.
    let rows_id = ui.id().with("news-rows");
    let mut draw_virtual_row = |ui: &mut Ui, row: &mut CachedRow| {
        let rect = egui::Rect::from_min_max(ui.next_widget_position(), ui.max_rect().max);
        let mut child = egui::Ui::new(
            ui.ctx().clone(),
            rows_id.with(&row.model.id),
            egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child.set_clip_rect(ui.clip_rect());
        draw_row(&mut child, row);
        ui.advance_cursor_after_rect(child.min_rect());
    };
    if !expanded {
        area.show_rows(ui, dp(22.0), cache.rows.len(), |ui, range| {
            let origin = ui.next_widget_position().y - row_y(range.start);
            scroll_selection(ui, origin);
            for row in &mut cache.rows[range] {
                draw_virtual_row(ui, row);
            }
        });
    } else {
        area.show_viewport(ui, |ui, viewport| {
            let origin = ui.next_widget_position().y;
            scroll_selection(ui, origin);
            let total = *offsets.last().unwrap_or(&0.0);
            ui.set_min_height(total);
            let first = offsets.partition_point(|y| *y <= viewport.top()).saturating_sub(1).min(cache.rows.len());
            let end = offsets.partition_point(|y| *y < viewport.bottom()).min(cache.rows.len());
            ui.add_space(offsets[first]);
            for row in &mut cache.rows[first..end] {
                draw_virtual_row(ui, row);
            }
            ui.add_space((total - offsets[end]).max(0.0));
        });
    }
    if let Some((id, offset)) = top {
        n.interaction.anchor = Some(id);
        n.interaction.anchor_offset = offset;
    }
    if height_changed {
        cache.layout_dirty = true;
        // One layout correction, not an idle repaint loop.
        ui.ctx().request_repaint();
    }
    if show_legend {
        key_legend(ui);
    }
    n.cache = cache;
    if before != n.filters {
        n.fetched = None;
        n.query = None;
    }
    actions
}

// N7b: strips share the existing news intelligence and UI snapshots. Neither
// path subscribes to news, queries the store, or starts a source refresh.
const FLASH_SECONDS: i64 = 600;
const TAPE_SECONDS: i64 = 2 * 3600;
const TAPE_FRAME: std::time::Duration = std::time::Duration::from_millis(50); // at most 20 Hz
const ROTATION_SECONDS: f64 = 8.0;

#[derive(Default)]
pub struct Flash {
    arrivals: std::collections::BTreeMap<String, i64>,
    initialized: bool,
    dismissed: std::collections::BTreeSet<String>,
    dismissed_at: Option<(i64, i64)>,
}
impl Flash {
    /// Hydrating saved history must not make old headlines fresh again. Once
    /// hydrated, a newly delivered id uses its actual UI arrival, not publication.
    pub(super) fn observe(&mut self, items: &[tr_data::news_store::Story], now: i64) {
        for story in items {
            let arrival = if self.initialized { now } else { story.item.ts.min(now) };
            self.arrivals.entry(story.id.clone()).or_insert(arrival);
        }
        self.initialized = true;
    }
    fn arrival(&self, story: &Cluster) -> i64 {
        story.members.iter().map(|s| self.arrivals.get(&s.id).copied().unwrap_or(s.item.ts)).max().unwrap_or(0)
    }
    fn pick<'a>(&self, stories: &'a [Cluster], now: i64) -> Option<&'a Cluster> {
        // Pick first, then suppress: dismissing the newest must not reveal an
        // older alert. Only a strictly newer arrival can re-arm the strip.
        let newest = stories.iter().max_by_key(|s| (self.arrival(s), s.newest, &s.lead.id))?;
        let arrival = self.arrival(newest);
        (arrival <= now
            && now < arrival.saturating_add(FLASH_SECONDS)
            && self.dismissed_at.is_none_or(|at| (arrival, newest.newest) > at)
            && !newest.member_ids.iter().any(|id| self.dismissed.contains(id)))
        .then_some(newest)
    }
    fn dismiss(&mut self, story: &Cluster) {
        self.dismissed_at =
            Some(self.dismissed_at.unwrap_or((i64::MIN, i64::MIN)).max((self.arrival(story), story.newest)));
        self.dismissed.extend(story.member_ids.iter().cloned());
    }
}

fn flash_stories(n: &NewsPane, watches: &[KeywordWatch], now: i64) -> Vec<Cluster> {
    let mut query = current_query(n, now);
    query.start = 0; // all pane filters, deliberately excluding the date range
    query.end = i64::MAX;
    let mut items = std::collections::BTreeMap::new();
    for item in n.items.iter().chain(&n.pending).filter(|s| tr_data::news_store::matches(s, &query)) {
        items.insert(item.id.clone(), item.clone());
    }
    intelligence::clusters(&items.into_values().collect::<Vec<_>>())
        .into_iter()
        .filter(|story| {
            story.priority() >= intelligence::Priority::High
                && (!n.display.unread || story.unread())
                && (!n.display.watch_hits
                    || story.members.iter().any(|s| watches.iter().any(|w| intelligence::watch_matches(s, &w.words))))
        })
        .collect()
}

/// Selecting from a strip never opens the URL or resets pane filters. Retain the
/// selected members when the tape routes into a pane with a different snapshot.
pub(super) fn select_strip_story(n: &mut NewsPane, story: &Cluster) {
    for member in &story.members {
        if !n.items.iter().chain(&n.pending).any(|s| s.id == member.id) {
            n.items.push(member.clone());
            n.item_revision = n.item_revision.wrapping_add(1);
        }
    }
    let id = story.lead.id.clone();
    n.interaction.external_selected = Some(id.clone());
    n.interaction.selected = Some(id.clone());
    n.interaction.scroll_to = Some(id.clone());
    n.interaction.dwell = Some((id, std::time::Instant::now()));
}

fn show_flash(ui: &mut Ui, n: &mut NewsPane, stories: &[Cluster], now: i64) {
    let Some(story) = n.interaction.flash.pick(stories, now) else { return };
    let expiry = n.interaction.flash.arrival(story).saturating_add(FLASH_SECONDS);
    ui.ctx().request_repaint_after(std::time::Duration::from_secs((expiry - now).max(1) as u64));
    ui.horizontal(|ui| {
        chrome(ui);
        let badge = ui.add(
            egui::Label::new(RichText::new("FLASH").font(mono(10.5)).strong().color(theme::warn()))
                .sense(egui::Sense::click()),
        );
        if badge.clicked() {
            select_strip_story(n, story);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("×").on_hover_text("Dismiss this flash story").clicked() {
                n.interaction.flash.dismiss(story);
            }
            let source_width = ui
                .painter()
                .layout_no_wrap(story.lead.source.clone(), mono(10.5), theme::muted())
                .size()
                .x
                .min(dp(120.0));
            let source = ui.add_sized(
                egui::vec2(source_width, dp(22.0)),
                egui::Label::new(RichText::new(&story.lead.source).font(mono(10.5)).color(theme::muted()))
                    .truncate()
                    .sense(egui::Sense::click()),
            );
            if source.clicked() {
                select_strip_story(n, story);
            }
            let response = ui
                .add_sized(
                    egui::vec2(ui.available_width(), dp(22.0)),
                    egui::Label::new(RichText::new(&story.lead.item.title).font(ui_font(12.0)).color(theme::text()))
                        .truncate()
                        .sense(egui::Sense::click()),
                )
                .on_hover_text(format!("{} · {}", story.lead.item.title, story.lead.source));
            if response.clicked() {
                select_strip_story(n, story);
            }
        });
    });
}

fn tape_items(items: &[tr_data::news_store::Story], watches: &[KeywordWatch], now: i64) -> Vec<Cluster> {
    let mut unique = std::collections::BTreeMap::new();
    for story in items.iter().filter(|s| !s.inferred_ts && s.item.ts > now - TAPE_SECONDS && s.item.ts <= now) {
        unique.insert(story.id.clone(), story.clone());
    }
    let mut stories = intelligence::clusters(&unique.into_values().collect::<Vec<_>>());
    stories.retain(|s| {
        s.priority() >= intelligence::Priority::High
            || s.members.iter().any(|s| watches.iter().any(|w| intelligence::watch_matches(s, &w.words)))
    });
    stories.sort_by(|a, b| b.newest.cmp(&a.newest).then(a.lead.id.cmp(&b.lead.id)));
    stories
}

fn tape_static_mode(preference: bool, os: bool, focused: bool) -> bool {
    preference || os || !focused
}

#[derive(Default)]
struct TapeClock {
    last: Option<f64>,
    paused: bool,
    static_mode: bool,
    offset: f64,
    rotation: f64,
    index: usize,
}
impl TapeClock {
    fn step(&mut self, time: f64, paused: bool, static_mode: bool, count: usize) {
        let elapsed = self.last.replace(time).map_or(0.0, |last| (time - last).max(0.0));
        if !paused && !self.paused && self.static_mode == static_mode {
            if static_mode {
                self.rotation += elapsed;
                self.index = (self.index + (self.rotation / ROTATION_SECONDS) as usize) % count.max(1);
                self.rotation %= ROTATION_SECONDS;
            } else {
                self.offset += elapsed * 40.0; // logical pixels/second, independent of frame rate
            }
        }
        self.paused = paused;
        self.static_mode = static_mode;
        self.index %= count.max(1);
    }
    fn repaint_after(&self) -> Option<std::time::Duration> {
        if self.paused {
            None
        } else if self.static_mode {
            Some(std::time::Duration::from_secs_f64((ROTATION_SECONDS - self.rotation).max(TAPE_FRAME.as_secs_f64())))
        } else {
            Some(TAPE_FRAME)
        }
    }
}

#[derive(Default)]
pub(super) struct Tape {
    key: Vec<(crate::dock::PaneId, u64, usize, usize)>,
    watches: Vec<KeywordWatch>,
    items: Vec<Cluster>,
    // App-level retention makes the URL fallback usable after the last News
    // pane closes. This is only a bounded copy of already-delivered UI data.
    market_items: Vec<tr_data::news_store::Story>,
    expiry: Option<i64>,
    clock: TapeClock,
    text_key: Option<(theme::Palette, egui::FontId, egui::FontId, f32)>,
    galleys: Vec<std::sync::Arc<egui::Galley>>,
    offsets: Vec<f32>,
}
impl Tape {
    fn update(&mut self, panes: &[super::Pane], watches: &[KeywordWatch], now: i64) {
        // Only market-scope snapshots: symbol/watchlist news does not silently
        // turn into a market subscription when this preference is enabled.
        let market = || {
            panes.iter().filter_map(|p| match &p.kind {
                super::PaneKind::News(n)
                    if n.filters.scope == Scope::All
                        && n.query.as_ref().is_none_or(|q| q.filters.scope == Scope::All) =>
                {
                    Some((p.id, n))
                }
                _ => None,
            })
        };
        let key: Vec<_> = market().map(|(id, n)| (id, n.item_revision, n.items.len(), n.pending.len())).collect();
        if key == self.key && self.watches == watches && self.expiry.is_none_or(|at| now < at) {
            return;
        }
        let mut unique: std::collections::BTreeMap<_, _> =
            std::mem::take(&mut self.market_items).into_iter().map(|s| (s.id.clone(), s)).collect();
        for item in market().flat_map(|(_, n)| n.items.iter().chain(&n.pending)) {
            unique.insert(item.id.clone(), item.clone());
        }
        self.market_items = unique
            .into_values()
            .filter(|s| !s.inferred_ts && s.item.ts > now - TAPE_SECONDS && s.item.ts <= now)
            .collect();
        self.market_items.sort_by(|a, b| b.item.ts.cmp(&a.item.ts).then(a.id.cmp(&b.id)));
        self.market_items.truncate(intelligence::MAX_ITEMS);
        self.items = tape_items(&self.market_items, watches, now);
        self.text_key = None;
        // Rebuild when any member ages out, including members that contribute
        // priority or keyword hits but are not the cluster lead.
        self.expiry = self.market_items.iter().map(|s| s.item.ts + TAPE_SECONDS).min();
        self.key = key;
        self.watches = watches.to_vec();
    }
}

impl super::TerminalApp {
    pub(super) fn toggle_headline_tape(&mut self) {
        self.prefs.headline_tape = !self.prefs.headline_tape;
        self.headline_tape.clock = TapeClock::default();
        self.save_prefs();
    }

    /// Return a safe URL only when no News pane exists. Selections use stable
    /// story ids; focus activates background tabs without creating a new pane.
    pub(super) fn route_tape_story(&mut self, story: &Cluster) -> Option<String> {
        let target = self
            .panes
            .iter()
            .enumerate()
            .filter_map(|(i, p)| match &p.kind {
                super::PaneKind::News(n) => Some((i, (p.id == self.focus, n.last_focused))),
                _ => None,
            })
            .max_by_key(|(_, rank)| *rank)
            .map(|(i, _)| i);
        if let Some(i) = target {
            let id = self.panes[i].id;
            if let super::PaneKind::News(n) = &mut self.panes[i].kind {
                select_strip_story(n, story);
                n.last_focused = Some(std::time::Instant::now());
            }
            self.focus = id;
            self.dock.activate(id);
            None
        } else {
            let url = tr_data::news_store::normalize_link(&story.lead.item.link);
            (!url.is_empty()).then_some(url)
        }
    }

    /// Paint just above the status bar; zero height means zero reserved space.
    pub(super) fn show_headline_tape(&mut self, ui: &mut Ui, available: egui::Rect) -> f32 {
        if !self.prefs.headline_tape {
            self.headline_tape.clock = TapeClock::default();
            return dp(0.0);
        }
        let now = chrono::Utc::now().timestamp();
        self.headline_tape.update(&self.panes, &self.prefs.keyword_watches, now);
        if self.headline_tape.items.is_empty() {
            self.headline_tape.clock = TapeClock::default();
            return dp(0.0);
        }
        let rect = egui::Rect::from_min_max(egui::pos2(available.left(), available.bottom() - dp(24.0)), available.max);
        let ctx = ui.ctx().clone();
        let (time, focused, hovered, visible) = ctx.input(|i| {
            (i.time, i.focused, i.pointer.hover_pos().is_some_and(|p| rect.contains(p)), window_visible(i.viewport()))
        });
        let tape = &mut self.headline_tape;
        let static_mode = tape_static_mode(self.prefs.reduce_motion, crate::anim::reduced(), focused);
        tape.clock.step(time, hovered || !visible, static_mode, tape.items.len());
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        painter.rect_filled(rect, dp(0.0), theme::header());
        painter.hline(rect.x_range(), rect.top(), egui::Stroke::new(dp(1.0), theme::border()));
        let text_key = (theme::palette(), mono(10.5), ui_font(12.0), theme::density());
        if tape.text_key.as_ref() != Some(&text_key) {
            tape.galleys = tape
                .items
                .iter()
                .map(|item| {
                    let mut job = egui::text::LayoutJob::default();
                    job.append(
                        &format!("{}  ", item.lead.source),
                        dp(0.0),
                        egui::TextFormat { font_id: mono(10.5), color: theme::muted(), ..Default::default() },
                    );
                    job.append(
                        &item.lead.item.title,
                        dp(0.0),
                        egui::TextFormat { font_id: ui_font(12.0), color: theme::text(), ..Default::default() },
                    );
                    painter.layout_job(job)
                })
                .collect();
            tape.offsets.clear();
            tape.offsets.push(dp(0.0));
            for galley in &tape.galleys {
                tape.offsets.push(tape.offsets.last().copied().unwrap_or_default() + galley.size().x + dp(24.0));
            }
            tape.text_key = Some(text_key);
        }
        let mut picked = None;
        let mut draw = |index: usize, cycle: usize, x: f32, width: f32, galley: std::sync::Arc<egui::Galley>| {
            let item = &tape.items[index];
            let entry = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(width, rect.height()));
            let hit = entry.intersect(rect);
            if hit.is_positive() {
                let response = ui
                    .interact(hit, ui.id().with(("headline-tape", &item.lead.id, cycle)), egui::Sense::click())
                    .on_hover_text(format!("{} · {}", item.lead.source, item.lead.item.title));
                if response.hovered() {
                    painter.rect_filled(hit, dp(0.0), theme::control_hover());
                }
                painter.galley(egui::pos2(x + dp(8.0), rect.center().y - galley.size().y / 2.0), galley, theme::text());
                painter.vline(
                    x + width - dp(1.0),
                    rect.y_range().shrink(dp(5.0)),
                    egui::Stroke::new(dp(1.0), theme::border()),
                );
                if response.clicked() {
                    picked = Some(item.clone());
                }
            }
        };
        if static_mode {
            let index = tape.clock.index;
            draw(index, 0, rect.left(), rect.width(), tape.galleys[index].clone());
        } else {
            let total = *tape.offsets.last().unwrap_or(&dp(0.0));
            let cycle_width = total.max(rect.width()).max(dp(1.0));
            tape.clock.offset %= f64::from(cycle_width);
            // Repeat seamlessly. A short tape leaves a calm gap; stable ids
            // distinguish the two visible copies at the wrap boundary.
            for cycle in 0..2 {
                let origin = rect.left() - tape.clock.offset as f32 + cycle as f32 * cycle_width;
                let first = tape.offsets.partition_point(|x| origin + x <= rect.left()).saturating_sub(1);
                let end = tape.offsets.partition_point(|x| origin + x < rect.right()).min(tape.items.len());
                for index in first.min(end)..end {
                    draw(
                        index,
                        cycle,
                        origin + tape.offsets[index],
                        tape.offsets[index + 1] - tape.offsets[index],
                        tape.galleys[index].clone(),
                    );
                }
            }
        }
        if visible {
            if let Some(wait) = tape.clock.repaint_after() {
                ctx.request_repaint_after(wait);
            }
            if let Some(expiry) = tape.expiry {
                ctx.request_repaint_after(std::time::Duration::from_secs((expiry - now).max(1) as u64));
            }
        }
        if let Some(story) = picked {
            if let Some(url) = self.route_tape_story(&story) {
                ctx.open_url(egui::OpenUrl::new_tab(url));
            } else if let Some(pane) = self.panes.iter().find(|p| p.id == self.focus && p.window.is_some()) {
                ctx.send_viewport_cmd_to(
                    egui::ViewportId::from_hash_of(("pane-window", pane.id)),
                    egui::ViewportCommand::Focus,
                );
            }
        }
        rect.height()
    }
}
