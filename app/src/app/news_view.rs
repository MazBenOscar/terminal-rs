//! N1a: stored-history news controls; no timers or idle repaints.
#[cfg(test)]
#[path = "news_tests.rs"]
mod tests;
use super::{NamedWatchlist, NewsPane};
use crate::{
    congress_view::Action,
    theme::{self, dp, mono, ui_font},
};
use egui::{RichText, Ui};
use tr_data::news_store::Scope;

pub(super) fn window_visible(viewport: &egui::ViewportInfo) -> bool {
    viewport.minimized != Some(true) && viewport.visible() != Some(false)
}

pub(super) fn show(ui: &mut Ui, n: &mut NewsPane, lists: &[NamedWatchlist]) -> Vec<Action> {
    let before = n.filters.clone();
    let mut actions = vec![];
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(&mut n.filters.scope, Scope::Symbol, "Symbol (linked)");
        ui.selectable_value(&mut n.filters.scope, Scope::Watchlist, "Watchlist");
        ui.selectable_value(&mut n.filters.scope, Scope::All, "All market");
        if n.filters.scope == Scope::Watchlist {
            egui::ComboBox::from_id_salt("news-watchlist")
                .selected_text(n.filters.watchlist.as_deref().unwrap_or("Active watchlist"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut n.filters.watchlist, None, "Active watchlist");
                    for list in lists {
                        ui.selectable_value(&mut n.filters.watchlist, Some(list.name.clone()), &list.name);
                    }
                });
        }
        if ui.selectable_label(n.paused, if n.paused { "Resume" } else { "Pause" }).clicked() {
            n.paused = !n.paused;
            if !n.paused && !n.pending.is_empty() {
                n.items = std::mem::take(&mut n.pending);
            }
        }
        if n.paused {
            ui.label(RichText::new(format!("{} new", n.new_count())).color(theme::amber()).font(mono(11.0)));
        }
        if ui
            .small_button("Read saved")
            .on_hover_text("Reload stored history; network refreshes are paced by the worker")
            .clicked()
        {
            n.fetched = None;
        }
    });
    ui.horizontal_wrapped(|ui| {
        let mut sources = vec!["Finnhub".to_string(), "Yahoo".into(), "CoinDesk".into()];
        sources.extend(n.items.iter().map(|s| s.source.clone()));
        sources.sort();
        sources.dedup();
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
        for (days, label) in [(1, "Today"), (7, "7D"), (30, "30D"), (90, "90D")] {
            ui.selectable_value(&mut n.filters.days, days, label);
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut n.filters.search)
                .hint_text("Search saved history")
                .desired_width(dp(150.0)),
        );
        ui.add(
            egui::TextEdit::singleline(&mut n.filters.include)
                .hint_text("Include words (any)")
                .desired_width(dp(125.0)),
        );
        ui.add(egui::TextEdit::singleline(&mut n.filters.exclude).hint_text("Exclude words").desired_width(dp(125.0)));
    });
    if before != n.filters {
        n.fetched = None;
        n.query = None;
    }
    let relative = n
        .updated
        .map(|at| {
            let seconds = at.elapsed().as_secs();
            if seconds < 60 {
                "updated just now".to_string()
            } else if seconds < 3600 {
                format!("updated {}m ago", seconds / 60)
            } else {
                format!("updated {}h ago", seconds / 3600)
            }
        })
        .unwrap_or_else(|| "updated just now".into());
    let status = n.error.as_deref().unwrap_or(&n.status).replace("updated just now", &relative);
    ui.add(egui::Label::new(RichText::new(&status).color(theme::muted()).font(ui_font(11.0))).truncate())
        .on_hover_text(&status);
    if n.loading {
        ui.label(RichText::new("Reading saved headlines…").color(theme::muted()));
    }
    let now = chrono::Utc::now().timestamp();
    let days = n.filters.days.clamp(1, 90) as i64;
    let query = tr_data::news_store::Query {
        symbols: n.query.as_ref().map(|q| q.symbols.clone()).unwrap_or_else(|| n.symbol.iter().cloned().collect()),
        filters: n.filters.clone(),
        start: if days == 1 { tr_data::news_store::day_start(now) } else { now - days * 86400 },
        ..Default::default()
    };
    if let Some(message) = n.empty_message(&query) {
        ui.label(RichText::new(message).color(theme::muted()));
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for story in &n.items {
            if !tr_data::news_store::matches(story, &query) {
                continue;
            }
            let item = &story.item;
            ui.horizontal_wrapped(|ui| {
                let age = (now - item.ts).max(0);
                let time = if age < 3600 {
                    format!("{}m", age / 60)
                } else if age < 86400 {
                    format!("{}h", age / 3600)
                } else {
                    chrono::DateTime::from_timestamp(item.ts, 0)
                        .map(|d| d.format("%b %d").to_string())
                        .unwrap_or_default()
                };
                ui.label(RichText::new(time).font(mono(11.0)).color(theme::muted()));
                // The store clears all non-http(s) links. Validate again at the browser boundary.
                let safe = !tr_data::news_store::normalize_link(&item.link).is_empty();
                let response = ui.add(
                    egui::Label::new(RichText::new(&item.title).font(ui_font(13.0)).color(theme::text()))
                        .sense(egui::Sense::click()),
                );
                if response.clicked() && safe {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&item.link));
                }
                response.on_hover_text(if story.summary.is_empty() { &item.link } else { &story.summary });
            });
            ui.horizontal_wrapped(|ui| {
                ui.add_space(dp(24.0));
                ui.label(RichText::new(&story.source).font(ui_font(10.5)).color(theme::muted()));
                for ticker in &item.tickers {
                    if ui
                        .small_button(
                            RichText::new(ticker.split_whitespace().next().unwrap_or(ticker))
                                .font(mono(10.5))
                                .color(theme::amber()),
                        )
                        .clicked()
                    {
                        actions.push(Action::OpenChart(ticker.clone()));
                    }
                }
            });
            ui.add_space(dp(2.0));
            ui.separator();
        }
    });
    ui.label(
        RichText::new("Finnhub free tier: personal, non-commercial use.").font(ui_font(10.0)).color(theme::muted()),
    );
    actions
}
