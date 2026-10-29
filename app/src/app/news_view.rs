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
}
struct CachedRow {
    story: Cluster,
    model: RowModel,
    highlights: Vec<intelligence::WatchHit>,
    stripe: Option<u8>,
    height: f32,
}
impl RowCache {
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
            n.interaction.selected = remap(n.interaction.selected.as_deref());
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
        source_count: story.sources.len(),
        meta,
        meta_steps,
        unread: story.unread(),
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
fn toggle_saved(n: &mut NewsPane, story: &Cluster) {
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
        let meta = [story.lead.source.clone(), model.full_time.clone(), model.about.clone()];
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
fn chips(ui: &mut Ui, display: &mut Display, unread: usize) {
    ui.toggle_value(&mut display.unread, format!("Unread {unread}")).on_hover_text("Unread only (u)");
    ui.toggle_value(&mut display.watch_hits, "Watch hits");
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

fn source_matches(source: &str, filter: &str) -> bool {
    source.to_lowercase().contains(&filter.trim().to_lowercase())
}

fn active_filter_summary(n: &NewsPane) -> String {
    let mut parts = vec![];
    if n.display.sort == intelligence::Sort::Relevance {
        parts.push("Relevance".into());
    }
    if !n.filters.sources.is_empty() {
        parts.push(format!("Sources: {}", n.filters.sources.join(", ")));
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
        parts.push("Watch hits".into());
    }
    parts.join(" · ")
}

fn filters(ui: &mut Ui, n: &mut NewsPane, watches: &mut Vec<KeywordWatch>, narrow: bool, unread: usize) {
    let mut changed = false;
    let mut finished = false;
    if narrow {
        ui.horizontal(|ui| chips(ui, &mut n.display, unread));
    }
    ui.horizontal(|ui| {
        ui.label("Sort (R)");
        ui.selectable_value(&mut n.display.sort, intelligence::Sort::Newest, "Newest");
        ui.selectable_value(&mut n.display.sort, intelligence::Sort::Relevance, "Relevance")
            .on_hover_text("Dated stories in the last 24h: priority, distinct sources, then time. Older or undated stories: newest first.");
    });
    ui.checkbox(&mut n.filters.saved, "Saved");
    ui.label("Sources");
    let sources: std::collections::BTreeSet<_> = n
        .items
        .iter()
        .flat_map(|s| [s.source.clone(), s.provider.clone()])
        .chain(n.filters.sources.iter().cloned())
        .filter(|s| !s.is_empty())
        .collect();
    ui.add(
        egui::TextEdit::singleline(&mut n.interaction.source_filter)
            .hint_text("Filter sources (contains, ignoring case)")
            .desired_width(dp(260.0)),
    );
    let matching: Vec<_> = sources.iter().filter(|s| source_matches(s, &n.interaction.source_filter)).collect();
    ui.label(
        RichText::new(format!("{} of {} sources match", matching.len(), sources.len()))
            .font(mono(10.5))
            .color(theme::muted()),
    );
    if ui.selectable_label(n.filters.sources.is_empty(), "All sources").clicked() {
        n.filters.sources.clear();
    }
    egui::ScrollArea::vertical().id_salt("news-filter-sources").max_height(dp(140.0)).show(ui, |ui| {
        for source in matching {
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
    ui.label("Keyword watches · words or \"quoted phrases\"");
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
                        &format!("Watch colour {}", colour + 1),
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
    if ui.small_button("Add watch").clicked() {
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
    let status = n.error.as_deref().unwrap_or(&n.status).replace("updated just now", &update);
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
                    ui.add(egui::Label::new(&status).wrap());
                    ui.separator();
                    if ui
                        .button("Copy digest")
                        .on_hover_text("Copy a 24-hour digest from local headlines and filing metadata")
                        .clicked()
                    {
                        n.interaction.copy_digest = true;
                        ui.close();
                    }
                    if ui.button("Re-read history").on_hover_text("Re-read saved history; source pacing is unchanged").clicked() {
                        n.fetched = None;
                        n.read_in_view.clear();
                        n.item_revision = n.item_revision.wrapping_add(1);
                        ui.close();
                    }
                    if ui.button("Refresh source").on_hover_text("Request fresh headlines through the normal guard; respects pause, cooldown, offline and budgets").clicked() {
                        n.refresh_source = true;
                        n.fetched = None;
                        ui.close();
                    }
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut n.display.wire, false, "Stories");
                        ui.selectable_value(&mut n.display.wire, true, "Wire");
                    });
                    if ui.button(if n.paused { "Show new updates" } else { "Pause updates" }).clicked() {
                        n.paused = !n.paused;
                        if !n.paused && !n.pending.is_empty() {
                            let incoming = std::mem::take(&mut n.pending);
                            n.apply(incoming);
                        }
                        ui.close();
                    }
                    ui.menu_button("About this source", |ui| {
                        ui.label("Finnhub free tier: personal, non-commercial use.");
                        ui.label("Headlines and metadata only; links open the publisher.");
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
                                egui::Label::new(
                                    RichText::new(crate::feed_time::compact_status(&status))
                                        .font(ui_font(10.5))
                                        .color(theme::muted()),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&status);
                        });
                    },
                );
            },
        );
    });
    ui.horizontal(|ui| {
        chrome(ui);
        for (days, label) in [(1, "Today"), (7, "7D"), (30, "30D"), (90, "90D")] {
            ui.selectable_value(&mut n.filters.days, days, label);
        }
        ui.toggle_value(&mut n.display.unread, format!("Unread {unread}")).on_hover_text("Unread only (u)");
        if !narrow {
            ui.toggle_value(&mut n.display.watch_hits, "Watch hits");
        }
        let count = n.filters.sources.len()
            + usize::from(n.filters.saved)
            + usize::from(!n.filters.include.is_empty())
            + usize::from(!n.filters.exclude.is_empty())
            + usize::from(narrow && n.display.unread)
            + usize::from(narrow && n.display.watch_hits);
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            let filter_label = format!("Filters ({count})");
            let filter_width =
                ui.painter().layout_no_wrap(filter_label.clone(), ui_font(10.5), theme::text()).size().x + dp(12.0);
            // The Watch hits chip appears at 360pt, but that does not guarantee
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
            ui.menu_button(filter_label, |ui| filters(ui, n, watches, narrow, unread));
        });
    });
    let summary = active_filter_summary(n);
    if !summary.is_empty() {
        ui.add(egui::Label::new(RichText::new(&summary).font(ui_font(10.5)).color(theme::muted())).truncate())
            .on_hover_text(summary);
    }
    let text_focused = ui.memory(|m| m.focused()).is_some_and(|id| egui::TextEdit::load_state(ui.ctx(), id).is_some());
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
            || !n.filters.include.is_empty()
            || !n.filters.exclude.is_empty()
            || !n.filters.search.is_empty()
            || n.display.unread
            || n.display.watch_hits
            || cache.older_match;
        let no_key = n.filters.scope == Scope::Symbol
            && n.symbol.is_some()
            && status.contains("Company news needs a Finnhub key.");
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
                let selection = match n.filters.scope {
                    Scope::Symbol => n.symbol.as_deref().and_then(|s| s.split_whitespace().next()).unwrap_or("Market"),
                    Scope::Watchlist => &watchlist_name,
                    Scope::All => "Market",
                };
                let message = if n.filters.days == 1 {
                    format!("No headlines for {selection} today.")
                } else {
                    format!("No headlines for {selection} in the last {} days.", n.filters.days)
                };
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
        // Fixed leading slots: unread/watch, priority, then Stories source strength.
        // When needed, single-source rows leave the shared count slot blank.
        let priority_rect =
            egui::Rect::from_min_size(rect.min + egui::vec2(dp(12.0), dp(0.0)), egui::vec2(dp(14.0), rect.height()));
        let marker = match model.priority {
            intelligence::Priority::Urgent => Some(("!", theme::amber())), // attention role
            intelligence::Priority::High => Some(("+", theme::accent())),
            intelligence::Priority::Normal => None,
        };
        if let Some((text, colour)) = marker {
            ui.painter().text(priority_rect.center(), egui::Align2::CENTER_CENTER, text, mono(11.0), colour);
            ui.interact(priority_rect, ui.id().with("priority"), egui::Sense::hover()).on_hover_text(format!(
                "Priority: {} · headline patterns, not verified urgency",
                model.priority.label()
            ));
        }
        if !n.display.wire && model.source_count > 1 {
            ui.painter().text(
                egui::pos2(priority_rect.right() + source_width, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                format!("×{}", model.source_count),
                mono(11.0),
                theme::muted(),
            );
        }
        let leading_width = dp(28.0) + source_width;
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
        let available = rect.right() - ticker_rect.right() - dp(4.);
        let budget = crate::feed_time::tail_budget(rect.width(), available);
        let (meta_text, meta_width) = model
            .meta_steps
            .iter()
            .find_map(|text| {
                let width = ui.painter().layout_no_wrap(text.clone(), ui_font(10.5), theme::muted()).size().x;
                (width <= budget).then_some((text, width))
            })
            .expect("empty metadata always fits");
        let meta_rect = egui::Rect::from_min_max(
            egui::pos2((rect.right() - meta_width).max(ticker_rect.right()), rect.top()),
            rect.max,
        );
        let title_rect = egui::Rect::from_min_max(
            egui::pos2(ticker_rect.right() + dp(4.0), rect.top()),
            egui::pos2((meta_rect.left() - dp(6.0)).max(ticker_rect.right() + dp(4.0)), rect.bottom()),
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
