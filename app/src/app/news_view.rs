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
    pub open_data_sources: bool,
}
pub(super) fn owns_key(key: egui::Key) -> bool {
    matches!(
        key,
        egui::Key::ArrowDown
            | egui::Key::ArrowUp
            | egui::Key::J
            | egui::Key::K
            | egui::Key::Enter
            | egui::Key::Space
            | egui::Key::C
            | egui::Key::Slash
            | egui::Key::Escape
            | egui::Key::U
    )
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Next,
    Previous,
    Open,
    Expand,
    Chart,
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
    pub headline: String,
    pub meta: String,
    pub unread: bool,
}
pub fn row_model(story: &Cluster, linked: Option<&str>, now: i64) -> RowModel {
    let (time, full_time) = crate::feed_time::feed_time(story.newest, now, false);
    let tickers: Vec<_> = story.tickers.iter().filter(|t| Some(t.as_str()) != linked).cloned().collect();
    let mut meta = story.lead.source.clone();
    if story.sources.len() > 1 {
        meta.push_str(&format!(" +{}", story.sources.len() - 1));
    }
    if tickers.len() > 1 {
        meta.push_str(&format!(" · +{}", tickers.len() - 1));
    }
    for tag in &story.tags {
        meta.push_str(" · ");
        meta.push_str(tag);
    }
    RowModel {
        id: story.lead.id.clone(),
        time,
        full_time,
        ticker: tickers.first().cloned(),
        extra_tickers: tickers.into_iter().skip(1).collect(),
        headline: story.lead.item.title.clone(),
        meta,
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
            queued.insert(item.id.clone());
        }
    }
    n.seen_queue = queued.into_iter().collect();
}
fn chart_ticker<'a>(story: &'a Cluster, linked: Option<&str>) -> Option<&'a String> {
    story.tickers.iter().find(|ticker| Some(ticker.as_str()) != linked).or_else(|| story.tickers.first())
}
fn open(ui: &Ui, story: &Cluster) {
    if !tr_data::news_store::normalize_link(&story.lead.item.link).is_empty() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(&story.lead.item.link));
    }
}
fn toggle_expanded(n: &mut NewsPane, story: &Cluster) {
    if !n.interaction.expanded.remove(&story.lead.id) {
        n.interaction.expanded.insert(story.lead.id.clone());
    }
    mark_seen(n, story);
}
fn chips(ui: &mut Ui, display: &mut Display) {
    ui.toggle_value(&mut display.watch_hits, "Watch hits");
    ui.toggle_value(&mut display.unread, "Unread").on_hover_text("Unread only (u)");
}
fn filters(ui: &mut Ui, n: &mut NewsPane, watches: &mut Vec<KeywordWatch>, narrow: bool) {
    if narrow {
        ui.horizontal(|ui| chips(ui, &mut n.display));
    }
    ui.label("Sources");
    let sources: std::collections::BTreeSet<_> = n
        .items
        .iter()
        .flat_map(|s| [s.source.clone(), s.provider.clone()])
        .chain(n.filters.sources.iter().cloned())
        .filter(|s| !s.is_empty())
        .collect();
    if ui.selectable_label(n.filters.sources.is_empty(), "All sources").clicked() {
        n.filters.sources.clear();
    }
    for source in sources {
        let selected = n.filters.sources.contains(&source);
        if ui.selectable_label(selected, &source).clicked() {
            if selected {
                n.filters.sources.retain(|s| s != &source);
            } else {
                n.filters.sources.push(source);
            }
        }
    }
    ui.add(
        egui::TextEdit::singleline(&mut n.filters.include).hint_text("Include words (any)").desired_width(dp(210.0)),
    );
    ui.add(egui::TextEdit::singleline(&mut n.filters.exclude).hint_text("Exclude words").desired_width(dp(210.0)));
    ui.label("Keyword watches · words or \"quoted phrases\"");
    let mut delete = None;
    for (i, watch) in watches.iter_mut().enumerate() {
        ui.push_id(i, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut watch.name).hint_text("Name").desired_width(dp(100.0)));
                if ui.small_button("Delete").clicked() {
                    delete = Some(i);
                }
            });
            ui.add(egui::TextEdit::singleline(&mut watch.words).hint_text("Words").desired_width(dp(210.0)));
            ui.horizontal(|ui| {
                for colour in 0..6 {
                    if ui
                        .selectable_label(watch.colour.min(5) == colour, RichText::new("●").color(watch_colour(colour)))
                        .on_hover_text(format!("Watch colour {}", colour + 1))
                        .clicked()
                    {
                        watch.colour = colour;
                    }
                }
            });
        });
    }
    if let Some(i) = delete {
        watches.remove(i);
    }
    if ui.small_button("Add watch").clicked() {
        watches.push(KeywordWatch::default());
    }
}

pub(super) fn window_visible(viewport: &egui::ViewportInfo) -> bool {
    viewport.minimized != Some(true) && viewport.visible() != Some(false)
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
    let unread_query = current_query(n, now);
    let unread = n.items.iter().filter(|s| !s.seen && tr_data::news_store::matches(s, &unread_query)).count();
    ui.horizontal(|ui| {
        chrome(ui);
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
                        ui.selectable_value(&mut n.filters.watchlist, Some(list.name.clone()), &list.name);
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
                ui.label(RichText::new("●").color(link.color()));
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.menu_button("⋯", |ui| {
                if ui.button("Reload").on_hover_text("Reload saved history; source pacing is unchanged").clicked() {
                    n.fetched = None;
                    ui.close();
                }
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut n.display.wire, false, "Stories");
                    ui.selectable_value(&mut n.display.wire, true, "Wire");
                });
                if !n.paused && ui.button("Pause updates").clicked() {
                    n.paused = true;
                    ui.close();
                }
                ui.menu_button("About this source", |ui| {
                    ui.label("Finnhub free tier: personal, non-commercial use.");
                    ui.label("Headlines and metadata only; links open the publisher.");
                });
            });
            if n.paused
                && ui
                    .small_button(format!("{} new · Show", n.new_count()))
                    .on_hover_text("Updates paused in this pane; show new saved headlines")
                    .clicked()
            {
                n.paused = false;
                if !n.pending.is_empty() {
                    n.items = std::mem::take(&mut n.pending);
                }
            }
            ui.add(
                egui::Label::new(
                    RichText::new(format!("{unread} unread · {status}")).font(mono(10.5)).color(theme::muted()),
                )
                .truncate(),
            )
            .on_hover_text(&status);
        });
    });
    ui.horizontal(|ui| {
        chrome(ui);
        for (days, label) in [(1, "Today"), (7, "7D"), (30, "30D"), (90, "90D")] {
            ui.selectable_value(&mut n.filters.days, days, label);
        }
        if !narrow {
            chips(ui, &mut n.display);
        }
        let count = n.filters.sources.len()
            + usize::from(!n.filters.include.is_empty())
            + usize::from(!n.filters.exclude.is_empty())
            + watches.len()
            + usize::from(n.display.unread)
            + usize::from(n.display.watch_hits);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.menu_button(format!("Filters ({count})"), |ui| filters(ui, n, watches, narrow));
            if narrow && !n.interaction.search_open && n.filters.search.is_empty() {
                if ui.small_button("⌕").on_hover_text("Search (/)").clicked() {
                    n.interaction.search_open = true;
                    n.interaction.focus_search = true;
                }
            } else {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut n.filters.search)
                        .hint_text("Search")
                        .desired_width(ui.available_width().max(dp(30.0))),
                );
                if n.interaction.focus_search {
                    response.request_focus();
                    n.interaction.focus_search = false;
                }
                if response.lost_focus() && n.filters.search.is_empty() {
                    n.interaction.search_open = false;
                }
            }
        });
    });
    let query = current_query(n, now);
    let items: Vec<_> = n.items.iter().filter(|s| tr_data::news_store::matches(s, &query)).cloned().collect();
    let mut rows = if n.display.wire {
        items.iter().take(intelligence::MAX_ITEMS).map(Cluster::wire).collect()
    } else {
        intelligence::clusters(&items)
    };
    rows.retain(|s| {
        (!n.display.unread || s.unread())
            && (!n.display.watch_hits
                || s.members
                    .iter()
                    .any(|m| watches.iter().any(|w| !intelligence::watch_ranges(&m.item.title, &w.words).is_empty())))
    });
    let ids: Vec<_> = rows.iter().map(|s| s.lead.id.clone()).collect();
    if ids != n.interaction.snapshot {
        n.interaction.selected = remap_anchor(n.interaction.selected.as_deref(), &rows);
        n.interaction.scroll_to =
            n.interaction.selected.clone().or_else(|| remap_anchor(n.interaction.anchor.as_deref(), &rows));
        n.interaction.snapshot = ids.clone();
    }
    let text_focused = ui.memory(|m| m.focused()).is_some_and(|id| egui::TextEdit::load_state(ui.ctx(), id).is_some());
    if focused && !text_focused {
        let keys = [
            (egui::Key::ArrowDown, Key::Next),
            (egui::Key::J, Key::Next),
            (egui::Key::ArrowUp, Key::Previous),
            (egui::Key::K, Key::Previous),
            (egui::Key::Enter, Key::Open),
            (egui::Key::Space, Key::Expand),
            (egui::Key::C, Key::Chart),
            (egui::Key::Slash, Key::Search),
            (egui::Key::Escape, Key::Escape),
            (egui::Key::U, Key::Unread),
        ];
        for (egui_key, key) in keys {
            if !ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui_key)) {
                continue;
            }
            ui.ctx().request_repaint(); // Input-driven state change, never an idle loop.
            let old = n.interaction.selected.clone();
            let effect = reduce(key, &ids, &mut n.interaction.selected, &mut n.filters.search, &mut n.display.unread);
            if old != n.interaction.selected {
                n.interaction.dwell = n.interaction.selected.clone().map(|id| (id, std::time::Instant::now()));
                n.interaction.scroll_to = n.interaction.selected.clone();
            }
            if effect == Effect::Search {
                n.interaction.search_open = true;
                n.interaction.focus_search = true;
                ui.ctx().request_repaint();
            }
            if let Some(story) = rows.iter().find(|s| Some(&s.lead.id) == n.interaction.selected.as_ref()) {
                match effect {
                    Effect::Open => {
                        open(ui, story);
                        mark_seen(n, story);
                    }
                    Effect::Expand => toggle_expanded(n, story),
                    Effect::Chart => {
                        let linked = n.symbol.as_deref().filter(|_| n.filters.scope == Scope::Symbol);
                        if let Some(t) = chart_ticker(story, linked) {
                            actions.push(Action::OpenChart(t.clone()));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    if focused
        && !text_focused
        && n.interaction.dwell.is_none()
        && let Some(story) = rows.iter().find(|s| Some(&s.lead.id) == n.interaction.selected.as_ref() && s.unread())
    {
        n.interaction.dwell = Some((story.lead.id.clone(), std::time::Instant::now()));
    }
    if let Some((id, at)) = n.interaction.dwell.clone() {
        if focused && !text_focused && n.interaction.selected.as_ref() == Some(&id) {
            if let Some(story) = rows.iter().find(|s| s.lead.id == id && s.unread()) {
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
        let filtered = !n.filters.sources.is_empty()
            || !n.filters.include.is_empty()
            || !n.filters.exclude.is_empty()
            || !n.filters.search.is_empty()
            || n.display.unread
            || n.display.watch_hits
            || n.items.iter().any(|s| {
                s.item.ts < query.start
                    && tr_data::news_store::matches(
                        s,
                        &tr_data::news_store::Query { start: 0, ..current_query(n, now) },
                    )
            });
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
                    n.filters = tr_data::news_store::Filters { scope, watchlist, days: 90, ..Default::default() };
                    n.display.unread = false;
                    n.display.watch_hits = false;
                }
            }
            EmptyCause::NoHistory => {
                if let Some(message) = n.empty_message(&query) {
                    ui.add(egui::Label::new(RichText::new(&message).color(theme::muted())).truncate())
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
    egui::ScrollArea::vertical().auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
        ui.spacing_mut().item_spacing.y = dp(0.0);
        let origin = ui.next_widget_position().y;
        for story in &rows {
            let model = row_model(story, linked.as_deref(), now);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), dp(22.0)), egui::Sense::click());
            if top.is_none() && rect.bottom() - origin >= viewport.top() {
                top = Some(model.id.clone());
            }
            if scroll.as_ref() == Some(&model.id) {
                ui.scroll_to_rect(rect, Some(egui::Align::Min));
            }
            if n.interaction.selected.as_ref() == Some(&model.id) {
                ui.painter().rect_filled(rect, dp(0.0), theme::accent_wash());
            } else if response.hovered() {
                ui.painter().rect_filled(rect, dp(0.0), theme::control_hover());
            }
            let watch = watches.iter().find(|w| {
                story.members.iter().any(|m| !intelligence::watch_ranges(&m.item.title, &w.words).is_empty())
            });
            if let Some(w) = watch {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        rect.min + egui::vec2(dp(1.0), dp(3.0)),
                        egui::vec2(dp(2.0), rect.height() - dp(6.0)),
                    ),
                    dp(0.0),
                    watch_colour(w.colour),
                );
            } else if model.unread {
                ui.painter().circle_filled(rect.left_center() + egui::vec2(dp(3.0), dp(0.0)), dp(2.0), theme::accent());
            }
            let time_width = ui.fonts_mut(|f| f.glyph_width(&mono(11.0), '0')) * crate::feed_time::WIDTH_CHARS as f32;
            let ticker_width = time_width;
            let time_rect = egui::Rect::from_min_size(
                rect.min + egui::vec2(dp(6.0), dp(0.0)),
                egui::vec2(time_width, rect.height()),
            );
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(time_rect).layout(egui::Layout::right_to_left(egui::Align::Center)),
                |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(&model.time).font(mono(11.0)).color(theme::muted())).truncate(),
                    )
                },
            )
            .inner
            .on_hover_text(&model.full_time);
            let ticker_rect = egui::Rect::from_min_size(
                egui::pos2(time_rect.right() + dp(6.0), rect.top()),
                egui::vec2(ticker_width, rect.height()),
            );
            let mut child_clicked = false;
            if let Some(ticker) = &model.ticker {
                let r = ui.put(
                    ticker_rect,
                    egui::Label::new(
                        RichText::new(ticker.split_whitespace().next().unwrap_or(ticker))
                            .font(mono(11.0))
                            .color(theme::amber()),
                    )
                    .truncate()
                    .sense(egui::Sense::click()),
                );
                if r.clicked() {
                    actions.push(Action::OpenChart(ticker.clone()));
                    child_clicked = true;
                }
            }
            let meta_width = (rect.width() * 0.30).min(dp(230.0));
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
            let highlights = intelligence::watch_highlights(&model.headline, watches);
            let base = if model.unread { theme::text() } else { theme::muted() };
            for hit in highlights {
                let range = hit.range;
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
            let title = ui.put(title_rect, egui::Label::new(job).truncate().sense(egui::Sense::click()));
            if title.clicked() {
                open(ui, story);
                mark_seen(n, story);
                child_clicked = true;
            }
            title.on_hover_text(format!("{}\n{}\n{}", model.headline, story.lead.summary, story.lead.item.link));
            let meta = ui
                .scope_builder(
                    egui::UiBuilder::new().max_rect(meta_rect).layout(egui::Layout::right_to_left(egui::Align::Center)),
                    |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&model.meta).font(ui_font(10.5)).color(theme::muted()))
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
            let meta = meta.on_hover_text(story.tickers.join(", "));
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
                    ui.add_space(time_width + ticker_width + dp(16.0));
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
                                let (time, full) = crate::feed_time::feed_time(member.item.ts, now, false);
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
                                && let Some(t) = chart_ticker(story, linked.as_deref())
                            {
                                actions.push(Action::OpenChart(t.clone()));
                            }
                            if ui.small_button("Copy link").clicked() {
                                ui.ctx().copy_text(story.lead.item.link.clone());
                            }
                        });
                    });
                });
            }
        }
    });
    n.interaction.anchor = top;
    if before != n.filters {
        n.fetched = None;
        n.query = None;
    }
    actions
}
