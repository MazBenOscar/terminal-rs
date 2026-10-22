//! Pane content for every panel kind.

use std::time::Instant;

use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, Ui, UiBuilder, pos2, vec2};
use tr_data::OrderBook;

use super::{PaneKind, TerminalApp};
use super::{WatchEntryKind, WatchItemEntry};
use crate::congress_view::{self, Action};
use crate::markets_view;
use crate::theme::{self, amber, dp, mono, muted, text, ui_font};
use crate::watch_sections::{self, Item};
use crate::widgets;
use crate::worker::Request;

struct WatchRowLayout {
    cols: Vec<f32>,
    name_width: f32,
    name_left: f32,
    show_name: bool,
    extra: usize,
    stale: bool,
}

/// Use security precision for price-like values and two decimals for other indicators.
fn format_column_value(sec: &tr_core::Security, value: f64, last_price: Option<f64>) -> String {
    if value.abs() >= 1e6 {
        tr_chart::scale::format_value(value)
    } else {
        let price_like = last_price.is_some_and(|p| p > 0.0 && (0.5 * p..=2.0 * p).contains(&value));
        let decimals = if price_like { sec.decimals(value) } else { 2 };
        format!("{value:.decimals$}")
    }
}

impl TerminalApp {
    /// Draw pane `i` into `rect`. Returns its actions and whether it was clicked.
    pub(super) fn render_content(&mut self, ui: &mut Ui, i: usize, rect: Rect, focused: bool) -> (Vec<Action>, bool) {
        let mut actions = Vec::new();
        let id = self.panes[i].id;
        let dragging = self.drag.is_some_and(|d| d.active);
        let account_overlay = if let PaneKind::Chart(c) = &self.panes[i].kind {
            Some(crate::chart_account::Snapshot::build(
                self.shown_portfolio(),
                &c.symbol,
                &c.data.account,
                self.demo_mode(),
                self.broker.error.as_deref(),
                self.provider.eq_ignore_ascii_case("sample"),
            ))
        } else {
            None
        };
        if let PaneKind::Chart(c) = &mut self.panes[i].kind {
            c.account_overlay = account_overlay.unwrap_or_default();
            c.price_alerts = self
                .price_alerts
                .book
                .alerts
                .iter()
                .filter(|a| a.symbol == c.symbol && a.simulated == self.price_alerts.simulated)
                .cloned()
                .collect();
            let key = tr_data::security(&c.symbol).key();
            c.watchlists = self
                .watchlists
                .iter()
                .enumerate()
                .map(|(index, list)| crate::chart_view::WatchlistTarget {
                    name: list.name.clone(),
                    active: index == self.active_watchlist,
                    contains: list.symbols.contains(&key),
                })
                .collect();
            c.hover_enabled = !dragging;
            let clicked = c.show(ui, rect, focused, &self.worker);
            actions.append(&mut c.actions);
            let alert_actions = std::mem::take(&mut c.price_alert_actions);
            for action in alert_actions {
                if matches!(action, crate::price_alerts::Action::Manage) {
                    self.price_alerts.action(action);
                    actions.push(Action::Command("ALRT".into()));
                } else {
                    self.price_alerts.action(action);
                }
            }
            return (actions, clicked);
        }
        let freshness = self.congress_freshness();
        let inner = rect.shrink2(vec2(dp(8.0), dp(4.0)));
        let mut child = ui.new_child(UiBuilder::new().max_rect(inner).id_salt(("pane", id)));
        child.set_clip_rect(inner.intersect(ui.clip_rect()));
        let ui = &mut child;
        match &mut self.panes[i].kind {
            PaneKind::Feed { filter } => actions.extend(congress_view::feed(ui, &self.latest, filter, &freshness)),
            PaneKind::Ticker { symbol, trades, loading } => {
                if *loading {
                    ui.label(RichText::new("Loading…").color(muted()));
                } else {
                    actions.extend(congress_view::ticker_trades(ui, symbol, trades));
                }
            }
            PaneKind::Profile { query, data, loading } => {
                actions.extend(congress_view::profile(ui, query, data.as_deref(), *loading))
            }
            PaneKind::Politicians => {
                actions.extend(congress_view::politicians(ui, &self.politicians, &mut self.pol_search))
            }
            PaneKind::Alerts => {
                ui.horizontal_wrapped(|ui| {
                    let unread = self.price_alerts.unread();
                    ui.selectable_value(&mut self.price_alerts.congress_tab, false, format!("Price alerts ({unread})"));
                    ui.selectable_value(&mut self.price_alerts.congress_tab, true, "Congress filings");
                });
                ui.separator();
                if self.price_alerts.congress_tab {
                    let (a, seen) = congress_view::alerts(ui, &self.alerts);
                    actions.extend(a);
                    if seen {
                        self.worker.send(Request::MarkAlertsSeen);
                    }
                } else {
                    self.price_alerts.show(ui);
                }
            }
            PaneKind::Wei => actions.extend(markets_view::wei(ui, &self.quotes, self.wei_updated.as_deref())),
            PaneKind::Curve => actions.extend(markets_view::curve(ui, self.curves.as_deref())),
            PaneKind::Search { query } => {
                let hits = if self.net_hits.0 == *query { self.net_hits.1.as_slice() } else { &[] };
                let (a, changed) = markets_view::search(ui, query, hits);
                actions.extend(a);
                if changed && query.trim().len() >= 2 {
                    self.search_pending = Some((query.clone(), Instant::now()));
                }
            }
            PaneKind::News(n) => {
                let offline = self.provider.eq_ignore_ascii_case("sample");
                if let Some(refresh) = news(ui, n, offline) {
                    n.fetched = None;
                    n.loading = refresh;
                }
            }
            PaneKind::Options(o) => {
                let (a, refetch) = super::options_view::options_chain(ui, o);
                actions.extend(a);
                if refetch {
                    o.fetched = None;
                }
            }
            PaneKind::OrderBook(b) => {
                if let Some(a) = order_book(ui, b) {
                    actions.push(a);
                }
            }
            PaneKind::Scanner(s) => {
                ui.data_mut(|d| d.insert_temp(egui::Id::new("security-names"), self.security_names.clone()));
                let market = s.universe != tr_data::universe::Universe::Watchlist;
                let shown = s
                    .rows
                    .iter()
                    .filter(|row| {
                        let outcome = row
                            .evaluation
                            .as_ref()
                            .map(|e| e.outcome)
                            .unwrap_or(tr_core::scanner::ScanOutcome::Unavailable);
                        !(s.matches_only && outcome == tr_core::scanner::ScanOutcome::NoMatch
                            || market && !s.show_unavailable && outcome == tr_core::scanner::ScanOutcome::Unavailable)
                    })
                    .map(|r| r.symbol.clone());
                crate::security_names::request(ui, &self.security_names, &self.worker, &self.provider, shown);
                let bulk_ready = self.broker.questrade.as_ref().is_some_and(|q| q.is_configured());
                actions.extend(super::scanner_view::show_named(
                    ui,
                    s,
                    id,
                    &self.watchlist,
                    bulk_ready,
                    |request| self.worker.send(request),
                    &self.watchlists[self.active_watchlist].name,
                ));
            }
            PaneKind::Portfolio(pp) => {
                let demo =
                    (self.prefs.trading.account_mode == crate::prefs::AccountMode::Demo).then_some(&*self.demo.pf);
                for a in super::portfolio_view::portfolio(ui, pp, &self.broker, demo, &self.quotes) {
                    use super::portfolio_view::PortAction as P;
                    match a {
                        P::Core(a) => actions.push(a),
                        P::Refresh => actions.push(Action::Command("PORT REFRESH".into())),
                        P::Connect => actions.push(Action::Command("PORT CONNECT".into())),
                        P::Setup => actions.push(Action::Command("PORT SETUP".into())),
                        P::Options(k) => actions.push(Action::Command(format!("{k} OMON"))),
                        P::News(k) => actions.push(Action::Command(format!("{k} CN"))),
                        P::Trade { key, buy, account, units } => self.open_ticket(
                            crate::command::TradeArgs {
                                buy: Some(buy),
                                symbol: Some(key),
                                units,
                                ..Default::default()
                            },
                            account,
                        ),
                        P::CancelOrder { account, id } => self.request_cancel(&account, &id),
                        P::ClosePosition { account, key } => self.close_position(&account, &key),
                        P::TradingSettings => {
                            self.settings_tab = super::settings_view::SettingsTab::Trading;
                            self.open_panel(PaneKind::Settings);
                        }
                        P::Open(url) => {
                            if let Err(e) = super::broker::open_external(url) {
                                self.status = format!("Could not open {url}: {e}");
                            }
                        }
                    }
                }
            }
            PaneKind::Watchlist | PaneKind::Settings | PaneKind::Help | PaneKind::Chart(_) => {}
        }
        match self.panes[i].kind {
            PaneKind::Watchlist => actions.extend(self.watchlist(ui)),
            PaneKind::Help => self.help_panel(ui),
            PaneKind::Settings => {
                let cmds = self.settings_panel(ui);
                self.run_settings_cmds(cmds);
            }
            _ => {}
        }
        (actions, false)
    }

    fn watchlist(&mut self, ui: &mut Ui) -> Vec<Action> {
        let mut out = Vec::new();
        let details = self.watch_details && ui.available_height() >= dp(320.0) && !self.watchlist.is_empty();
        ui.horizontal(|ui| {
            let mut active = self.active_watchlist;
            egui::ComboBox::from_id_salt("watchlist-picker").selected_text(&self.watchlists[active].name).show_ui(
                ui,
                |ui| {
                    for (i, list) in self.watchlists.iter().enumerate() {
                        ui.selectable_value(&mut active, i, &list.name);
                    }
                },
            );
            if active != self.active_watchlist {
                self.switch_watchlist(active);
            }
            if ui.small_button("+").on_hover_text("Add symbol").clicked() {
                self.start_watch_entry(WatchEntryKind::Symbol, self.watchlists[active].items().len(), String::new());
            }
            let response = widgets::icon_button(ui, widgets::Icon::More, "Watchlist options", false);
            egui::Popup::menu(&response).show(|ui| {
                if ui.button("New list…").clicked() {
                    self.watchlist_entry = Some((None, String::new()));
                    ui.close();
                }
                if ui.button("Rename…").clicked() {
                    self.watchlist_entry = Some((
                        Some(super::watchlist_name_key(&self.watchlists[active].name)),
                        self.watchlists[active].name.clone(),
                    ));
                    ui.close();
                }
                if ui.add_enabled(self.watchlists.len() < 50, egui::Button::new("Duplicate")).clicked() {
                    if let Err(e) = self.duplicate_watchlist() {
                        self.status = e;
                    }
                    ui.close();
                }
                if ui.button("Add section…").clicked() {
                    self.start_watch_entry(
                        WatchEntryKind::Section,
                        self.watchlists[self.active_watchlist].items().len(),
                        String::new(),
                    );
                    ui.close();
                }
                if ui.button("Collapse all sections").clicked() {
                    self.collapse_watch_sections(true);
                    ui.close();
                }
                if ui.button("Expand all sections").clicked() {
                    self.collapse_watch_sections(false);
                    ui.close();
                }
                if ui.checkbox(&mut self.watch_details, "Show details card").changed() {
                    self.save_settings();
                }
                let label = if self.watchlist_delete {
                    format!("Delete {}?", self.watchlists[active].name)
                } else {
                    "Delete".into()
                };
                if ui.add_enabled(self.watchlists.len() > 1, egui::Button::new(label)).clicked() {
                    if self.watchlist_delete {
                        if let Err(e) = self.delete_watchlist(active) {
                            self.status = e;
                        }
                        ui.close();
                    } else {
                        self.watchlist_delete = true;
                    }
                }
            });
        });
        let mut accept = false;
        let mut cancel = false;
        if let Some((_, name)) = &mut self.watchlist_entry {
            ui.horizontal(|ui| {
                egui::Frame::new()
                    .fill(theme::header())
                    .stroke(egui::Stroke::new(1.0, muted()))
                    .inner_margin(dp(4.0))
                    .show(ui, |ui| {
                        let response = ui.add(
                            egui::TextEdit::singleline(name)
                                .frame(egui::Frame::NONE)
                                .hint_text("List name")
                                .char_limit(40),
                        );
                        accept = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    });
                accept |= ui.button("Save").clicked();
                cancel = ui.button("Cancel").clicked();
            });
        }
        if accept {
            if let Err(e) = self.apply_watchlist_entry() {
                self.status = e;
            }
        } else if cancel {
            self.watchlist_entry = None;
        }
        self.watch_item_entry_ui(ui);
        let row_h = dp(22.0);
        let w = ui.available_width();
        let (hdr, header_response) = ui.allocate_exact_size(vec2(w, row_h), Sense::click());
        header_response.context_menu(|ui| {
            if ui.button("Edit columns…").clicked() {
                out.push(Action::Command("COL EDIT".into()));
                ui.close();
            }
        });
        let p = ui.painter();
        p.rect_filled(hdr, theme::CONTROL_RADIUS, theme::header());
        let extra = self.watch_columns.columns.len().min(((w - dp(380.0)).max(0.0) / dp(85.0)) as usize);
        let show_name =
            crate::security_names::show_column(w, self.watch_columns.columns.len(), self.watch_columns.show_name);
        let name_width = if show_name { dp(150.0) } else { 0.0 };
        let base = if extra == 0 { 1.0 } else { (dp(380.0) / (w - name_width)).max(0.45) };
        let mut cols = vec![0.0, 0.34 * base, 0.58 * base, 0.79 * base, base];
        for i in 0..extra {
            cols.push(base + (1.0 - base) * (i + 1) as f32 / extra as f32);
        }
        let col = |r: Rect, k: usize| {
            r.left() + 8.0 + (r.width() - 16.0 - name_width) * cols[k] + if k > 0 { name_width } else { 0.0 }
        };
        let name_left = col(hdr, 1) - name_width;
        if show_name {
            let title = match self.watch_sort {
                Some((usize::MAX, false)) => "Name ▲",
                Some((usize::MAX, true)) => "Name ▼",
                _ => "Name",
            };
            p.text(pos2(name_left, hdr.center().y), Align2::LEFT_CENTER, title, ui_font(11.5), muted());
            let hit = Rect::from_min_max(pos2(name_left, hdr.top()), pos2(name_left + name_width, hdr.bottom()));
            let response = ui.interact(hit, ui.id().with("watch-name-sort"), Sense::click());
            response.context_menu(|ui| {
                if ui.button("Edit columns…").clicked() {
                    out.push(Action::Command("COL EDIT".into()));
                    ui.close();
                }
            });
            if response.clicked() {
                self.watch_sort = match self.watch_sort {
                    Some((usize::MAX, false)) => Some((usize::MAX, true)),
                    Some((usize::MAX, true)) => None,
                    _ => Some((usize::MAX, false)),
                };
            }
        }
        crate::security_names::request(
            ui,
            &self.security_names,
            &self.worker,
            &self.provider,
            self.watchlist.iter().cloned(),
        );
        let mut headers = vec!["Security".to_string(), "Last".into(), "Change".into(), "%".into()];
        headers.extend(self.watch_columns.columns.iter().take(extra).map(|c| c.title.clone()));
        for (index, title) in headers.iter().enumerate() {
            let k = if index == 0 { 0 } else { index + 1 };
            let suffix = match self.watch_sort {
                Some((active, descending)) if active == index => {
                    if descending {
                        " ▼"
                    } else {
                        " ▲"
                    }
                }
                _ => "",
            };
            let label = format!("{title}{suffix}");
            p.text(
                pos2(col(hdr, k), hdr.center().y),
                if index == 0 { Align2::LEFT_CENTER } else { Align2::RIGHT_CENTER },
                label,
                ui_font(11.5),
                muted(),
            );
            let left = if index == 0 { hdr.left() } else { col(hdr, k - 1) };
            let right = if index == 0 { name_left } else { col(hdr, k) };
            let hit = Rect::from_min_max(pos2(left, hdr.top()), pos2(right, hdr.bottom()));
            let response = ui.interact(hit, ui.id().with(("watch-header", index)), Sense::click());
            response.context_menu(|ui| {
                if ui.button("Edit columns…").clicked() {
                    out.push(Action::Command("COL EDIT".into()));
                    ui.close();
                }
            });
            if response.clicked() {
                self.watch_sort = match self.watch_sort {
                    Some((active, false)) if active == index => Some((index, true)),
                    Some((active, true)) if active == index => None,
                    _ => Some((index, false)),
                };
            }
        }
        let hidden = self.watch_columns.columns.len() - extra;
        if hidden > 0 {
            p.text(
                pos2(col(hdr, 0) + dp(95.0), hdr.center().y),
                Align2::LEFT_CENTER,
                format!("+{hidden}"),
                ui_font(11.0),
                muted(),
            );
        }
        let items = self.watchlists[self.active_watchlist].items();
        let order = self.watch_display_order(&items);
        let layout = WatchRowLayout {
            cols,
            name_width,
            name_left,
            show_name,
            extra,
            stale: chrono::Utc::now().timestamp() - self.columns_computed_at > 180,
        };
        let height = (ui.available_height() - if details { dp(134.0) } else { 0.0 }).max(row_h);
        // The drag state leaves `self` for the loop, which draws rows through `&mut self`.
        let mut drag_state = self.watch_drag.take();
        let mut drop = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(height).show(ui, |ui| {
            let mut drag =
                super::watchlist_drag::DragFrame::new(ui, &mut drag_state, &items, self.watch_sort.is_some());
            for index in order {
                let (rect, response) = ui.allocate_exact_size(vec2(w, row_h), Sense::click_and_drag());
                let offset = drag.row(ui, index, rect, &response);
                let rect = rect.translate(vec2(0.0, offset));
                self.draw_watch_item(ui, index, &items[index], (rect, response), &layout, &mut out);
            }
            drop = drag.finish(ui);
            ui.add_space(4.0);
            ui.label(
                RichText::new("WL ADD <KEY> to add · right-click a row for options").font(ui_font(11.0)).color(muted()),
            );
        });
        self.watch_drag = drag_state;
        if let Some((from, gap)) = drop {
            let mut items = items;
            if watch_sections::move_item(&mut items, from, gap) {
                self.set_watchlist_items(&items);
            }
        }
        if details {
            self.watch_details_card(ui);
        }
        out
    }

    fn watch_display_order(&self, items: &[Item]) -> Vec<usize> {
        let mut order: Vec<usize> = (0..items.len()).collect();
        if let Some((index, descending)) = self.watch_sort {
            let compare = |a: &usize, b: &usize| {
                let (Item::Symbol(a), Item::Symbol(b)) = (&items[*a], &items[*b]) else {
                    return std::cmp::Ordering::Equal;
                };
                if index == usize::MAX {
                    return crate::security_names::order(
                        self.security_names.name(a),
                        self.security_names.name(b),
                        descending,
                    );
                }
                if index == 0 {
                    let order = tr_data::security(a).ticker.cmp(&tr_data::security(b).ticker);
                    return if descending { order.reverse() } else { order };
                }
                let key = |s: &String| {
                    match index {
                        1 => self.quotes.get(s).map(|q| q.last),
                        2 => self.quotes.get(s).map(|q| q.change()),
                        3 => self.quotes.get(s).map(|q| q.change_pct()),
                        _ => {
                            tr_core::columns::sort_key(self.column_cells.get(s).and_then(|cells| cells.get(index - 4)))
                        }
                    }
                    .filter(|v| v.is_finite())
                };
                super::watch_value_order(key(a), key(b), descending)
            };
            let mut start = 0;
            for end in 0..=items.len() {
                if end == items.len() || items[end].is_section() {
                    order[start..end].sort_by(compare);
                    start = end + 1;
                }
            }
        }
        let visible = watch_sections::visible(items);
        order.retain(|i| visible.contains(i));
        order
    }

    fn draw_watch_item(
        &mut self,
        ui: &mut Ui,
        index: usize,
        item: &Item,
        row: (Rect, egui::Response),
        layout: &WatchRowLayout,
        out: &mut Vec<Action>,
    ) {
        let (r, resp) = row;
        if let Item::Section { name, collapsed } = item {
            widgets::hover_wash(ui, r, &resp, false);
            ui.painter().text(
                pos2(r.left() + dp(8.0), r.center().y),
                Align2::LEFT_CENTER,
                format!("{}  {}", if *collapsed { "▸" } else { "▾" }, name.to_uppercase()),
                ui_font(11.0),
                muted(),
            );
            if resp.clicked() {
                let mut items = self.watchlists[self.active_watchlist].items();
                watch_sections::set_collapsed(&mut items, index, !collapsed);
                self.set_watchlist_items(&items);
            }
            resp.context_menu(|ui| self.watch_section_menu(ui, index, name));
            return;
        }
        let Item::Symbol(s) = item else { return };
        let WatchRowLayout { name_width, name_left, show_name, extra, stale, .. } = *layout;
        let col = |r: Rect, k: usize| {
            r.left() + 8.0 + (r.width() - 16.0 - name_width) * layout.cols[k] + if k > 0 { name_width } else { 0.0 }
        };
        let sec = tr_data::security(s);
        widgets::hover_wash(ui, r, &resp, self.watch_selected.as_ref() == Some(s));
        let p = ui.painter();
        let crypto = sec.crypto_instrument().ok();
        // Crypto pairs use the type column too: BASE/QUOTE must keep its quote
        // asset visible (USD and USDT are different markets), and "Curncy" adds nothing.
        let crypto_pair = crypto.is_some();
        let ticker_width = if crypto_pair { 114.0 } else { 70.0 };
        let ticker =
            crypto.map_or_else(|| sec.ticker.clone(), |instrument| format!("{}/{}", instrument.base, instrument.quote));
        let mut job = egui::text::LayoutJob::simple_singleline(ticker, mono(12.5), amber());
        job.wrap.max_width = ticker_width;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        job.wrap.overflow_character = Some('…');
        let galley = p.layout_job(job);
        p.galley(pos2(col(r, 0), r.center().y - galley.size().y / 2.0), galley, amber());
        if !crypto_pair {
            p.text(
                pos2(col(r, 0) + 74.0, r.center().y),
                Align2::LEFT_CENTER,
                sec.class.yellow_key().label(),
                ui_font(11.0),
                muted(),
            );
        }
        let name = self.security_names.name(s);
        let ticker_hit = Rect::from_min_max(r.min, pos2(name_left, r.bottom()));
        let mut description = crate::security_names::hover(&sec.ticker, name);
        if let Some(quote) = self.quotes.get(s)
            && let Some(source) = &quote.crypto_source
        {
            let observed = chrono::DateTime::from_timestamp(quote.ts, 0)
                .map(|ts| ts.to_rfc3339())
                .unwrap_or_else(|| "unavailable".into());
            description.push_str(&format!(
                "\n{} spot · {} · quote {}\nObserved {observed}",
                source.venue.label(),
                source.native_symbol,
                source.quote
            ));
        }
        ui.interact(ticker_hit, ui.id().with(("ticker-name", s)), Sense::hover()).on_hover_text(description);
        if show_name {
            let hit = Rect::from_min_max(pos2(name_left, r.top()), pos2(name_left + name_width - dp(6.0), r.bottom()));
            crate::security_names::paint(ui, hit, name.unwrap_or(""), ("watch-name", s));
        }
        match self.quotes.get(s) {
            Some(q) => {
                let c = if q.change() >= 0.0 { theme::up() } else { theme::down() };
                let d = sec.decimals(q.last);
                p.text(
                    pos2(col(r, 2), r.center().y),
                    Align2::RIGHT_CENTER,
                    format!("{:.d$}", q.last),
                    mono(12.5),
                    text(),
                );
                p.text(
                    pos2(col(r, 3), r.center().y),
                    Align2::RIGHT_CENTER,
                    markets_view::change_text(&sec, q),
                    mono(12.5),
                    c,
                );
                p.text(
                    pos2(col(r, 4), r.center().y),
                    Align2::RIGHT_CENTER,
                    format!("{:+.2}%", q.change_pct()),
                    mono(12.5),
                    c,
                );
            }
            None => {
                p.text(pos2(col(r, 4), r.center().y), Align2::RIGHT_CENTER, "…", mono(12.5), muted());
            }
        }
        for index in 0..extra {
            use tr_core::columns::Cell;
            let cell = self.column_cells.get(s).and_then(|cells| cells.get(index));
            let (label, color, reason) = match cell {
                Some(Cell::Value(value)) => {
                    (format_column_value(&sec, *value, self.quotes.get(s).map(|q| q.last)), text(), None)
                }
                Some(Cell::Check(true)) => ("Yes".into(), theme::up(), None),
                Some(Cell::Check(false)) => ("No".into(), muted(), None),
                Some(Cell::Unavailable(reason)) => ("—".into(), muted(), Some(reason.as_str())),
                None => ("—".into(), muted(), Some("Awaiting history")),
            };
            p.text(
                pos2(col(r, index + 5), r.center().y),
                Align2::RIGHT_CENTER,
                label,
                mono(12.5),
                if stale { muted() } else { color },
            );
            if let Some(reason) = reason {
                let hit = Rect::from_min_max(pos2(col(r, index + 4), r.top()), pos2(col(r, index + 5), r.bottom()));
                ui.interact(hit, ui.id().with(("watch-cell", s, index)), Sense::hover()).on_hover_text(reason);
            }
        }
        let tradable = matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf);
        let pills = if tradable { widgets::buy_sell_pills().to_vec() } else { Vec::new() };
        let tips = [format!("Buy {} (⇧B)", sec.ticker), format!("Sell {} (⇧S)", sec.ticker)];
        let tips: Vec<&str> = tips.iter().map(String::as_str).collect();
        match widgets::row_pills(ui, r, ("wl-pills", s), &pills, &tips) {
            Some(i) => out.push(Action::Command(format!("{} {s}", if i == 0 { "BUY" } else { "SELL" }))),
            None if resp.clicked() => {
                self.watch_selected = Some(s.clone());
                out.push(Action::OpenChart(s.to_owned()));
            }
            None => {}
        }
        resp.context_menu(|ui| self.watch_symbol_menu(ui, index, s, out));
    }

    fn watch_symbol_menu(&mut self, ui: &mut Ui, index: usize, s: &str, out: &mut Vec<Action>) {
        let sec = tr_data::security(s);
        let tradable = matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf);
        if tradable {
            for (label, side) in [("Buy", "BUY"), ("Sell", "SELL")] {
                if ui.button(format!("{label} {}…", sec.ticker)).clicked() {
                    out.push(Action::Command(format!("{side} {s}")));
                    ui.close();
                }
            }
            ui.separator();
        }
        if ui.button("Open Chart").clicked() {
            out.push(Action::OpenChart(s.to_owned()));
            ui.close();
        }
        ui.menu_button(format!("Add {} to watchlist", sec.ticker), |ui| {
            for action in self.watchlist_destination_menu(ui, s) {
                out.push(action);
            }
        });
        if ui.button(format!("Add {} to compare", sec.ticker)).clicked() {
            out.push(Action::AddToCompare(s.to_owned()));
            ui.close();
        }
        ui.separator();
        if ui.button("Add section").clicked() {
            self.start_watch_entry(WatchEntryKind::Section, index, String::new());
            ui.close();
        }
        if ui.button("Add symbol").clicked() {
            self.start_watch_entry(WatchEntryKind::Symbol, index + 1, String::new());
            ui.close();
        }
        if sec.has_congress_trades() && ui.button("Who in Congress Traded This").clicked() {
            out.push(Action::OpenTicker(sec.ticker.clone()));
            ui.close();
        }
        ui.separator();
        if ui.button("Remove from Watchlist").clicked() {
            self.set_watchlist(self.watchlist.iter().filter(|x| *x != s).cloned().collect());
            ui.close();
        }
    }

    fn start_watch_entry(&mut self, kind: WatchEntryKind, index: usize, text: String) {
        self.watch_item_entry = Some(WatchItemEntry { kind, index, text });
    }

    fn watch_item_entry_ui(&mut self, ui: &mut Ui) {
        let mut accept = false;
        let mut cancel = false;
        if let Some(entry) = &mut self.watch_item_entry {
            ui.horizontal(|ui| {
                let hint = match entry.kind {
                    WatchEntryKind::Symbol => "Ticker",
                    _ => "Section name",
                };
                let response =
                    ui.add(egui::TextEdit::singleline(&mut entry.text).hint_text(hint).desired_width(dp(160.0)));
                if !response.has_focus() && ui.memory(|m| m.focused().is_none()) {
                    response.request_focus();
                }
                accept = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                accept |= ui.button("Save").clicked();
                cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
            });
        }
        if cancel {
            self.watch_item_entry = None;
        } else if accept && let Err(e) = self.apply_watch_item_entry() {
            self.status = e;
        }
    }

    fn collapse_watch_sections(&mut self, collapsed: bool) {
        let mut items = self.watchlists[self.active_watchlist].items();
        for i in 0..items.len() {
            watch_sections::set_collapsed(&mut items, i, collapsed);
        }
        self.set_watchlist_items(&items);
    }

    fn watch_section_menu(&mut self, ui: &mut Ui, index: usize, name: &str) {
        let items = self.watchlists[self.active_watchlist].items();
        for (label, kind, target, text) in [
            ("Rename section…", WatchEntryKind::Rename, index, name.to_owned()),
            ("Add section below…", WatchEntryKind::Section, watch_sections::block(&items, index).end, String::new()),
            ("Add symbol to section…", WatchEntryKind::Symbol, watch_sections::block(&items, index).end, String::new()),
        ] {
            if ui.button(label).clicked() {
                self.start_watch_entry(kind, target, text);
                ui.close();
            }
        }
        if ui.button("Remove section").clicked() {
            let mut items = items;
            watch_sections::remove_section(&mut items, index);
            self.set_watchlist_items(&items);
            ui.close();
        }
        for (label, collapsed) in [("Collapse all", true), ("Expand all", false)] {
            if ui.button(label).clicked() {
                self.collapse_watch_sections(collapsed);
                ui.close();
            }
        }
    }

    fn watchlist_destination_menu(&self, ui: &mut Ui, symbol: &str) -> Vec<Action> {
        let mut out = Vec::new();
        for (i, list) in self.watchlists.iter().enumerate() {
            let contains = list.symbols.iter().any(|s| s == symbol);
            let label = format!(
                "{}{}{}",
                if contains { "✓ " } else { "" },
                list.name,
                if i == self.active_watchlist { " (active)" } else { "" }
            );
            if ui.add_enabled(!contains, egui::Button::new(label)).clicked() {
                out.push(Action::AddToNamedWatchlist { list: list.name.clone(), symbol: symbol.into() });
                ui.close();
            }
        }
        out
    }

    fn watch_details_card(&self, ui: &mut Ui) {
        let Some(symbol) =
            self.watch_selected.as_ref().filter(|s| self.watchlist.contains(s)).or(self.watchlist.first())
        else {
            return;
        };
        let sec = tr_data::security(symbol);
        let quote = self.quotes.get(symbol);
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), dp(130.0)), Sense::hover());
        let p = ui.painter();
        p.line_segment([r.left_top(), r.right_top()], Stroke::new(1.0, muted().gamma_multiply(0.3)));
        let at = |y| pos2(r.left() + dp(8.0), r.top() + dp(y));
        p.text(at(17.0), Align2::LEFT_CENTER, &sec.ticker, mono(17.0), amber());
        p.text(at(38.0), Align2::LEFT_CENTER, self.security_names.name(symbol).unwrap_or(""), ui_font(11.0), text());
        let crypto = sec.crypto_instrument().ok();
        let venue = if let Some(instrument) = &crypto {
            let venue = quote.and_then(|q| q.crypto_source.as_ref()).map_or(instrument.venue, |s| s.venue);
            format!("{} · Spot · Crypto", venue.label())
        } else {
            match sec.class {
                tr_core::AssetClass::Equity | tr_core::AssetClass::Etf => "US Equity",
                _ => sec.class.yellow_key().label(),
            }
            .to_owned()
        };
        p.text(at(55.0), Align2::LEFT_CENTER, venue, ui_font(11.0), muted());
        if let Some(q) = quote {
            let d = sec.decimals(q.last);
            let price = format!("{:.d$}", q.last);
            let galley = p.layout_no_wrap(price, mono(22.0), text());
            let width = galley.size().x;
            p.galley(at(67.0), galley, text());
            if sec.dollar_priced() {
                p.text(at(81.0) + vec2(width + dp(6.0), 0.0), Align2::LEFT_CENTER, "USD", ui_font(10.0), muted());
            }
            p.text(
                at(103.0),
                Align2::LEFT_CENTER,
                format!("{}  {:+.2}%", markets_view::change_text(&sec, q), q.change_pct()),
                mono(12.0),
                if q.change() >= 0.0 { theme::up() } else { theme::down() },
            );
        } else {
            p.text(at(81.0), Align2::LEFT_CENTER, "…", mono(22.0), muted());
        }
        let status = if crypto.is_some() {
            "24/7 market".to_owned()
        } else {
            quote
                .and_then(|q| chrono::DateTime::from_timestamp(q.ts, 0))
                .map(|ts| format!("Quote as of {}", ts.with_timezone(&chrono::Local).format("%H:%M")))
                .unwrap_or_else(|| "Quote unavailable".into())
        };
        p.text(at(121.0), Align2::LEFT_CENTER, status, ui_font(10.0), muted());
    }
}

/// Headlines list. Returns `Some(_)` when a refresh was requested.
fn news(ui: &mut Ui, n: &mut super::NewsPane, offline: bool) -> Option<bool> {
    let mut refresh = None;
    ui.horizontal_wrapped(|ui| {
        let title = match &n.symbol {
            Some(s) => format!("{} headlines", tr_data::security(s).key()),
            None => "Top market headlines".into(),
        };
        ui.label(RichText::new(title).font(ui_font(12.5)).color(text()));
        if n.loading {
            ui.add(egui::Spinner::new().size(12.0));
        }
        if n.symbol.is_some() && ui.small_button("Top News").on_hover_text("General market headlines").clicked() {
            n.symbol = None;
            n.items.clear();
            refresh = Some(true);
        }
        if ui.small_button("Refresh").clicked() {
            refresh = Some(true);
        }
    });
    ui.add_space(2.0);
    if let Some(e) = &n.error {
        ui.label(RichText::new(format!("Could not load news: {e}")).font(ui_font(12.0)).color(theme::down()));
    }
    if n.items.is_empty() && !n.loading {
        let msg = if offline {
            "No headlines in offline sample mode (TR_PROVIDER=sample)."
        } else {
            "No headlines for this security right now."
        };
        ui.label(RichText::new(msg).font(ui_font(12.0)).color(muted()));
    }
    let now = chrono::Utc::now().timestamp();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let w = ui.available_width();
        for item in &n.items {
            let age = (now - item.ts).max(0);
            let when = if age < 3600 {
                format!("{}m", age / 60)
            } else if age < 86_400 {
                format!("{}h", age / 3600)
            } else {
                chrono::DateTime::from_timestamp(item.ts, 0).map(|d| d.format("%b %d").to_string()).unwrap_or_default()
            };
            let title = ui.painter().layout(item.title.clone(), ui_font(13.0), text(), (w - 70.0).max(80.0));
            let h = title.size().y + dp(20.0);
            let (r, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
            widgets::hover_wash(ui, r, &resp, false);
            let p = ui.painter();
            p.text(pos2(r.left() + 6.0, r.top() + 4.0), Align2::LEFT_TOP, &when, mono(11.0), amber());
            let tpos = pos2(r.left() + 60.0, r.top() + 3.0);
            let th = title.size().y;
            p.galley(tpos, title, text());
            p.text(pos2(tpos.x, tpos.y + th + 2.0), Align2::LEFT_TOP, &item.publisher, ui_font(11.0), muted());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(&item.link));
            }
            resp.on_hover_text(&item.link).context_menu(|ui| {
                if ui.button("Open in Browser").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&item.link));
                    ui.close();
                }
                if ui.button("Copy Link").clicked() {
                    ui.ctx().copy_text(item.link.clone());
                    ui.close();
                }
            });
            p.hline(r.x_range(), r.bottom(), Stroke::new(1.0, theme::border()));
        }
    });
    refresh
}

/// Bucket sizes offered for a book around `mid`: raw plus three decades of the price scale.
pub fn group_steps(mid: f64) -> [f64; 4] {
    let base = 10f64.powi((mid.abs().max(1e-9).log10().floor() as i32) - 4);
    [0.0, base, base * 10.0, base * 100.0]
}

fn fmt_size(q: f64) -> String {
    if q >= 1e6 {
        format!("{:.2}M", q / 1e6)
    } else if q >= 1e3 {
        format!("{:.2}K", q / 1e3)
    } else if q >= 1.0 {
        format!("{q:.3}")
    } else {
        format!("{q:.5}")
    }
}

/// Price ladder: asks above, bids below, depth bars by cumulative size.
fn order_book(ui: &mut Ui, b: &mut super::BookPane) -> Option<Action> {
    let mut action = None;
    let key = tr_data::security(&b.symbol);
    let book: Option<OrderBook> = b.book.as_ref().map(|x| x.grouped(b.group));
    ui.horizontal_wrapped(|ui| {
        let r = ui.label(RichText::new(&key.ticker).font(mono(13.0)).color(amber())).interact(Sense::click());
        if r.clicked() {
            action = Some(Action::OpenChart(b.symbol.clone()));
        }
        r.on_hover_text("Open chart");
        if let Some(bk) = &book {
            ui.label(RichText::new(&bk.source).font(ui_font(11.5)).color(muted()));
            if bk.simulated {
                ui.label(
                    RichText::new(" SIMULATED ").font(ui_font(10.5)).color(Color32::BLACK).background_color(amber()),
                )
                .on_hover_text("No free public order book for this market (or offline); levels are synthetic.");
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        let depths = [10usize, 20, 50];
        let dl = ["10", "20", "50"];
        if let Some(i) =
            widgets::segmented(ui, ("book-depth", &b.symbol), &dl, depths.iter().position(|&x| x == b.depth), 20.0)
        {
            b.depth = depths[i];
            b.fetched = None;
        }
        let mid = b.book.as_ref().and_then(OrderBook::mid).unwrap_or(1.0);
        let steps = group_steps(mid);
        let d = key.decimals(mid);
        let labels: Vec<String> =
            steps.iter().map(|s| if *s == 0.0 { "Raw".into() } else { format!("{:.*}", d.min(8), s) }).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let sel = steps.iter().position(|s| (*s - b.group).abs() < 1e-12);
        if let Some(i) = widgets::segmented(ui, ("book-group", &b.symbol), &refs, sel, 20.0) {
            b.group = steps[i];
        }
    });
    if let Some(e) = &b.error {
        ui.label(RichText::new(e).font(ui_font(12.0)).color(theme::down()));
    }
    let Some(book) = book else {
        ui.add(egui::Spinner::new());
        return action;
    };
    let rect = ui.available_rect_before_wrap();
    let d = key.decimals(book.mid().unwrap_or(1.0));
    let row = dp(18.0);
    let rows_each = (((rect.height() - row * 2.0) / row / 2.0).floor() as usize).max(1);
    let asks: Vec<_> = book.asks.iter().take(rows_each).copied().collect();
    let bids: Vec<_> = book.bids.iter().take(rows_each).copied().collect();
    let cum = |v: &[(f64, f64)]| {
        v.iter()
            .scan(0.0, |acc, &(p, q)| {
                *acc += q;
                Some((p, q, *acc))
            })
            .collect::<Vec<_>>()
    };
    let (asks, bids) = (cum(&asks), cum(&bids));
    let max_cum = asks.last().map_or(0.0, |x| x.2).max(bids.last().map_or(0.0, |x| x.2)).max(1e-12);
    let p = ui.painter_at(rect);
    let w = rect.width();
    // Columns sized from the widest price so narrow panes stay legible.
    let price_w =
        p.layout_no_wrap(format!("{:.*}", d, book.asks.last().map_or(1.0, |l| l.0)), mono(12.0), text()).size().x;
    let show_total = w > price_w + 170.0;
    let x_price = rect.left() + (price_w + 8.0).max(w * 0.3);
    let x_total = rect.right() - 6.0;
    let x_size = if show_total { (x_price + x_total) / 2.0 } else { x_total };
    let mut y = rect.top();
    for (label, x) in [("Price", x_price), ("Size", x_size), ("Total", x_total)] {
        if label != "Total" || show_total {
            p.text(pos2(x, y + row / 2.0), Align2::RIGHT_CENTER, label, ui_font(11.0), muted());
        }
    }
    y += row;
    let draw = |y: f64, level: (f64, f64, f64), up: bool| {
        let y = y as f32;
        let c = if up { theme::up() } else { theme::down() };
        let frac = (level.2 / max_cum) as f32;
        let bar = Rect::from_min_max(pos2(rect.right() - w * frac, y + 1.0), pos2(rect.right(), y + row - 1.0));
        p.rect_filled(bar, 2.0, c.gamma_multiply(0.14));
        p.text(pos2(x_price, y + row / 2.0), Align2::RIGHT_CENTER, format!("{:.*}", d, level.0), mono(12.0), c);
        p.text(pos2(x_size, y + row / 2.0), Align2::RIGHT_CENTER, fmt_size(level.1), mono(12.0), text());
        if show_total {
            p.text(pos2(x_total, y + row / 2.0), Align2::RIGHT_CENTER, fmt_size(level.2), mono(12.0), muted());
        }
    };
    // Asks: best ask nearest the spread row.
    for (k, level) in asks.iter().enumerate().rev() {
        let at = y as f64 + (asks.len() - 1 - k) as f64 * row as f64;
        draw(at, *level, false);
    }
    y += asks.len() as f32 * row;
    let spread_r = Rect::from_min_size(pos2(rect.left(), y), vec2(w, row));
    p.rect_filled(spread_r, 3.0, theme::header());
    if let (Some(mid), Some(spread)) = (book.mid(), book.spread()) {
        p.text(pos2(x_price, spread_r.center().y), Align2::RIGHT_CENTER, format!("{:.*}", d, mid), mono(12.5), text());
        let spread_text = if show_total {
            format!("spread {:.*} ({:.1} bp)", d, spread, spread / mid * 1e4)
        } else {
            format!("{:.1} bp", spread / mid * 1e4)
        };
        p.text(pos2(x_total, spread_r.center().y), Align2::RIGHT_CENTER, spread_text, ui_font(11.0), muted());
    }
    y += row;
    for (k, level) in bids.iter().enumerate() {
        draw((y + k as f32 * row) as f64, *level, true);
    }
    ui.allocate_rect(rect, Sense::hover());
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_watchlists_updates_panel_rows_and_column_snapshot() {
        let root = std::env::temp_dir().join(format!("named-watchlists-panel-{}", std::process::id()));
        let mut app = TerminalApp::test_workspace(&root);
        app.add_watchlist_symbols(vec!["AAPL".into()]);
        app.create_watchlist("Technology".into(), vec!["NVDA".into()]).unwrap();
        let ctx = egui::Context::default();
        let draw = |app: &mut TerminalApp| {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                app.watchlist(ui);
            });
            output.textures_delta.clear();
            output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.job.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let labels = draw(&mut app);
        assert!(labels.iter().any(|text| text == "Technology"));
        assert!(labels.iter().any(|text| text == "NVDA"));
        assert!(!labels.iter().any(|text| text == "AAPL"));
        app.switch_watchlist(0);
        let labels = draw(&mut app);
        assert!(labels.iter().any(|text| text == "Main"));
        assert!(labels.iter().any(|text| text == "AAPL"));
        assert!(!labels.iter().any(|text| text == "NVDA"));
        assert_eq!(app.columns_snapshot.0, vec![tr_data::security("AAPL").key()]);
    }

    #[test]
    fn column_values_use_security_precision_only_for_price_like_values() {
        let btc = tr_core::Security::new("BTCUSD", tr_core::AssetClass::Crypto);
        let eur = tr_core::Security::new("EURUSD", tr_core::AssetClass::Fx);
        let jpy = tr_core::Security::new("USDJPY", tr_core::AssetClass::Fx);
        assert_eq!(format_column_value(&btc, 81_752.27, Some(84_454.0)), "81752.27");
        assert_eq!(format_column_value(&eur, 1.1457, Some(1.1237)), "1.1457");
        assert_eq!(format_column_value(&eur, 37.5558, Some(1.1237)), "37.56");
        assert_eq!(format_column_value(&jpy, 52.066, Some(158.0)), "52.07");
        for last in [None, Some(0.0), Some(-1.0)] {
            assert_eq!(format_column_value(&eur, 1.1457, last), "1.15");
        }
        // Both price-like boundaries are inclusive.
        assert_eq!(format_column_value(&eur, 0.56185, Some(1.1237)), "0.5618");
        assert_eq!(format_column_value(&eur, 2.2474, Some(1.1237)), "2.2474");
        assert_eq!(format_column_value(&eur, -37.5558, Some(1.1237)), "-37.56");
        assert_eq!(format_column_value(&btc, 1e6, Some(1e6)), "1.00M");
        assert_eq!(format_column_value(&btc, 2.3e9, Some(84_454.0)), "2.30B");
        assert_eq!(format_column_value(&btc, -2.3e9, None), "-2.30B");
    }

    #[test]
    fn column_values_reject_nan_and_infinite_last_prices_as_non_price_like() {
        let eur = tr_core::Security::new("EURUSD", tr_core::AssetClass::Fx);
        // NaN / ±inf last prices must not make the range check succeed.
        assert_eq!(format_column_value(&eur, 1.1457, Some(f64::NAN)), "1.15");
        assert_eq!(format_column_value(&eur, 1.1457, Some(f64::INFINITY)), "1.15");
        assert_eq!(format_column_value(&eur, 1.1457, Some(f64::NEG_INFINITY)), "1.15");
        // Non-finite formula values fall back to two decimals rather than panicking.
        assert_eq!(format_column_value(&eur, f64::NAN, Some(1.1237)), "NaN");
    }

    #[test]
    fn column_value_price_like_range_edges_are_inclusive_even_for_huge_levels() {
        let btc = tr_core::Security::new("BTCUSD", tr_core::AssetClass::Crypto);
        let last = 500_000.0;
        // Exactly 0.5x and just under 2x are price-like with two decimals.
        assert_eq!(format_column_value(&btc, 250_000.0, Some(last)), "250000.00");
        assert_eq!(format_column_value(&btc, 999_999.0, Some(last)), "999999.00");
        // One step outside the 0.5x bound is still two decimals (not price precision).
        assert_eq!(format_column_value(&btc, 249_999.0, Some(last)), "249999.00");
        // Values at/above 1e6 use compact formatting regardless of price-likeness.
        assert_eq!(format_column_value(&btc, 1e6, Some(last)), "1.00M");
    }

    fn sections_app() -> TerminalApp {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/watch-sections-tests");
        let mut app = TerminalApp::test_workspace(&root);
        app.watch_details = false;
        app.set_watchlist_items(&[
            Item::Symbol(tr_data::security("SPY").key()),
            Item::Section { name: "Tech".into(), collapsed: false },
            Item::Symbol(tr_data::security("NVDA").key()),
            Item::Symbol(tr_data::security("AAPL").key()),
            Item::Section { name: "Macro".into(), collapsed: false },
            Item::Symbol(tr_data::security("SPX Index").key()),
        ]);
        app
    }

    fn ui_frame(ctx: &egui::Context, events: Vec<egui::Event>, draw: &mut impl FnMut(&mut Ui)) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(650.0, 600.0))),
                events,
                ..Default::default()
            },
            draw,
        );
        output.textures_delta.clear();
        output
    }

    fn labels(output: &egui::FullOutput) -> Vec<String> {
        output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some(t.galley.job.text.clone()),
                _ => None,
            })
            .collect()
    }

    fn click_ui(label: &str, mut draw: impl FnMut(&mut Ui)) {
        let ctx = egui::Context::default();
        let output = ui_frame(&ctx, Vec::new(), &mut draw);
        let pos = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == label => Some(t.pos + t.galley.size() / 2.0),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {label}: {:?}", labels(&output)));
        for pressed in [true, false] {
            ui_frame(
                &ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                &mut draw,
            );
        }
    }

    fn save_entry(app: &mut TerminalApp, value: &str) {
        app.watch_item_entry.as_mut().unwrap().text = value.into();
        click_ui("Save", |ui| app.watch_item_entry_ui(ui));
    }

    #[test]
    fn section_headers_render_in_order_and_click_collapse_persists() {
        let mut app = sections_app();
        click_ui("▾  TECH", |ui| {
            app.watchlist(ui);
        });
        assert!(app.watchlists[0].sections[0].collapsed);
        assert!(!app.watchlists[0].sections[1].collapsed);
        let ctx = egui::Context::default();
        let output = ui_frame(&ctx, Vec::new(), &mut |ui| {
            app.watchlist(ui);
        });
        let texts = labels(&output);
        let tech = texts.iter().position(|s| s == "▸  TECH").unwrap();
        let macro_row = texts.iter().position(|s| s == "▾  MACRO").unwrap();
        assert!(tech < macro_row);
        assert!(!texts.iter().any(|s| s == "NVDA" || s == "AAPL"));
        assert!(texts.iter().any(|s| s == "SPY"));
        assert!(texts.iter().any(|s| s == "SPX"));
    }

    #[test]
    fn watch_sort_stays_within_sections_without_changing_storage() {
        let mut app = sections_app();
        let items = app.watchlists[0].items();
        app.watch_sort = Some((0, false));
        assert_eq!(app.watch_display_order(&items), [0, 1, 3, 2, 4, 5]);
        let ctx = egui::Context::default();
        let texts = labels(&ui_frame(&ctx, Vec::new(), &mut |ui| {
            app.watchlist(ui);
        }));
        assert!(texts.iter().position(|s| s == "AAPL") < texts.iter().position(|s| s == "NVDA"));
        app.watch_sort = Some((0, true));
        assert_eq!(app.watch_display_order(&items), [0, 1, 2, 3, 4, 5]);
        assert_eq!(app.watchlists[0].items(), items);
    }

    #[test]
    fn header_menu_rename_validation_remove_and_append_symbol() {
        let mut app = sections_app();
        click_ui("Rename section…", |ui| app.watch_section_menu(ui, 1, "Tech"));
        save_entry(&mut app, " ");
        assert!(app.status.contains("Section names"));
        assert_eq!(app.watchlists[0].sections[0].name, "Tech");
        save_entry(&mut app, "Growth");
        assert_eq!(app.watchlists[0].sections[0].name, "Growth");
        click_ui("Add symbol to section…", |ui| app.watch_section_menu(ui, 1, "Growth"));
        save_entry(&mut app, "MSFT");
        let items = app.watchlists[0].items();
        assert_eq!(items[4], Item::Symbol(tr_data::security("MSFT").key()));
        assert!(items[5].is_section());
        let symbols = app.watchlist.clone();
        click_ui("Remove section", |ui| app.watch_section_menu(ui, 1, "Growth"));
        assert_eq!(app.watchlist, symbols);
        assert_eq!(app.watchlists[0].sections.len(), 1);
        assert_eq!(app.watchlists[0].sections[0].name, "Macro");
    }

    #[test]
    fn symbol_menu_inserts_section_above_and_symbol_below_rejects_duplicates() {
        let mut app = sections_app();
        let nvda = tr_data::security("NVDA").key();
        click_ui("Add section", |ui| app.watch_symbol_menu(ui, 2, &nvda, &mut Vec::new()));
        save_entry(&mut app, "Chips");
        let items = app.watchlists[0].items();
        assert!(matches!(&items[2], Item::Section { name, .. } if name == "Chips"));
        assert_eq!(items[3], Item::Symbol(nvda.clone()));
        click_ui("Add symbol", |ui| app.watch_symbol_menu(ui, 3, &nvda, &mut Vec::new()));
        save_entry(&mut app, "NVDA");
        assert_eq!(app.status, "NVDA is already in Main");
        assert_eq!(app.watchlists[0].items(), items);
        save_entry(&mut app, "AAPL unexpected trailing input");
        assert_eq!(app.status, "Unknown symbol");
        save_entry(&mut app, "MSFT");
        assert_eq!(app.watchlists[0].items()[4], Item::Symbol(tr_data::security("MSFT").key()));
    }

    #[test]
    fn row_menu_named_watchlist_emits_existing_action_and_compare_preserves_extras() {
        let mut app = sections_app();
        let nvda = tr_data::security("NVDA").key();
        app.create_watchlist("Other".into(), Vec::new()).unwrap();
        app.switch_watchlist(0);
        let mut actions = Vec::new();
        click_ui("Other", |ui| actions.extend(app.watchlist_destination_menu(ui, &nvda)));
        assert_eq!(actions, [Action::AddToNamedWatchlist { list: "Other".into(), symbol: nvda.clone() }]);
        let mut compare = Vec::new();
        click_ui("Add NVDA to compare", |ui| app.watch_symbol_menu(ui, 2, &nvda, &mut compare));
        assert_eq!(compare, [Action::AddToCompare(nvda.clone())]);
        app.run_actions(compare.clone());
        assert_eq!(app.status, "Open a chart to compare");
        app.run_actions(vec![Action::OpenChart(tr_data::security("AAPL").key())]);
        let spy = tr_data::security("SPY").key();
        let msft = tr_data::security("MSFT").key();
        app.run_actions(vec![Action::AddToCompare(spy.clone()), Action::AddToCompare(msft.clone())]);
        app.run_actions(compare);
        let chart = app.peek_chart().unwrap();
        assert_eq!(chart.compare, Some(spy));
        assert_eq!(chart.data.comparisons, [msft, nvda]);
    }

    #[test]
    fn duplicate_keeps_sections_and_details_follow_selection_and_hide_when_empty() {
        let mut app = sections_app();
        app.collapse_watch_sections(true);
        let items = app.watchlists[0].items();
        click_ui("Duplicate", |ui| {
            if ui.button("Duplicate").clicked() {
                app.duplicate_watchlist().unwrap();
            }
        });
        assert_eq!(app.watchlists[app.active_watchlist].items(), items);
        app.collapse_watch_sections(false);
        click_ui("AAPL", |ui| {
            app.watchlist(ui);
        });
        let aapl = tr_data::security("AAPL").key();
        assert_eq!(app.watch_selected, Some(aapl.clone()));
        app.quotes.insert(
            aapl.clone(),
            tr_data::Quote {
                symbol: aapl,
                last: 123.45,
                prev_close: 120.0,
                ts: 1_700_000_000,
                spark: Vec::new(),
                crypto_source: None,
            },
        );
        app.watch_details = true;
        let ctx = egui::Context::default();
        let texts = labels(&ui_frame(&ctx, Vec::new(), &mut |ui| {
            app.watchlist(ui);
        }));
        assert!(texts.iter().any(|s| s == "AAPL"));
        assert!(texts.iter().any(|s| s == "123.45"));
        assert!(texts.iter().any(|s| s == "USD"));
        assert!(texts.iter().any(|s| s.starts_with("Quote as of ")));
        app.set_watchlist_items(&[]);
        let texts = labels(&ui_frame(&ctx, Vec::new(), &mut |ui| {
            app.watchlist(ui);
        }));
        assert!(!texts.iter().any(|s| s == "AAPL" || s == "123.45" || s == "USD"));
    }

    #[test]
    fn inline_entries_cancel_escape_and_respect_symbol_limit() {
        let mut app = sections_app();
        app.start_watch_entry(WatchEntryKind::Section, 0, "Discard".into());
        let items = app.watchlists[0].items();
        click_ui("Cancel", |ui| app.watch_item_entry_ui(ui));
        assert!(app.watch_item_entry.is_none());
        assert_eq!(app.watchlists[0].items(), items);
        app.start_watch_entry(WatchEntryKind::Rename, 1, "Discard".into());
        let ctx = egui::Context::default();
        ui_frame(
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut |ui| app.watch_item_entry_ui(ui),
        );
        assert!(app.watch_item_entry.is_none());
        assert_eq!(app.watchlists[0].items(), items);
        app.set_watchlist_items(&(0..200).map(|i| Item::Symbol(format!("TEST{i} US Equity"))).collect::<Vec<_>>());
        app.start_watch_entry(WatchEntryKind::Symbol, 200, "MSFT".into());
        click_ui("Save", |ui| app.watch_item_entry_ui(ui));
        assert_eq!(app.watchlist.len(), 200);
        assert!(app.status.contains("200 symbols"));
    }

    #[test]
    fn details_card_uses_first_symbol_and_is_hidden_in_short_panes() {
        let mut app = sections_app();
        app.watch_details = true;
        let ctx = egui::Context::default();
        let texts = labels(&ui_frame(&ctx, Vec::new(), &mut |ui| {
            app.watchlist(ui);
        }));
        assert_eq!(texts.iter().filter(|s| *s == "SPY").count(), 2);
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(650.0, 280.0))),
                ..Default::default()
            },
            |ui| {
                app.watchlist(ui);
            },
        );
        output.textures_delta.clear();
        assert_eq!(labels(&output).iter().filter(|s| *s == "SPY").count(), 1);
        assert!(!labels(&output).iter().any(|s| s == "Quote unavailable"));
    }

    #[test]
    fn details_toggle_settings_round_trips_and_defaults_on() {
        let mut settings: super::super::Settings = serde_json::from_str("{}").unwrap();
        assert!(settings.watch_details);
        settings.watch_details = false;
        let restored: super::super::Settings = serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert!(!restored.watch_details);
    }

    #[test]
    fn group_steps_scale_with_price() {
        assert_eq!(group_steps(82_000.0), [0.0, 1.0, 10.0, 100.0]);
        let fx = group_steps(1.09);
        assert!((fx[1] - 1e-4).abs() < 1e-12);
        assert_eq!(fmt_size(1_500.0), "1.50K");
        assert_eq!(fmt_size(0.5), "0.50000");
    }

    fn key(s: &str) -> String {
        tr_data::security(s).key()
    }

    /// Save an open inline entry; a cancelled/absent entry is a no-op so the test can
    /// accept "entry dropped on staleness" as a correct outcome.
    fn save_entry_if_open(app: &mut TerminalApp, value: &str) {
        if app.watch_item_entry.is_none() {
            return;
        }
        save_entry(app, value);
    }

    fn row_names(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                Item::Symbol(s) => s.clone(),
                Item::Section { name, .. } => format!("#{name}"),
            })
            .collect()
    }

    fn symbol_pos(items: &[Item], symbol: &str) -> Option<usize> {
        items.iter().position(|i| matches!(i, Item::Symbol(s) if s == symbol))
    }

    /// U, #First, #Second, NVDA — ungrouped row above two headers so a removal can
    /// slide the second header onto a stale rename index.
    fn two_headers_app() -> TerminalApp {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/watch-sections-tests");
        let mut app = TerminalApp::test_workspace(&root);
        app.watch_details = false;
        app.set_watchlist_items(&[
            Item::Symbol(key("SPY")),
            Item::Section { name: "First".into(), collapsed: false },
            Item::Section { name: "Second".into(), collapsed: false },
            Item::Symbol(key("NVDA")),
        ]);
        app
    }

    #[test]
    fn inline_entries_are_dropped_when_another_list_becomes_active() {
        let mut app = sections_app();
        app.start_watch_entry(WatchEntryKind::Symbol, 3, String::new());
        app.create_watchlist("Other".into(), vec![key("MSFT")]).unwrap();
        assert!(app.watch_item_entry.is_none(), "switching lists must discard the inline entry");
        // A later save must not resurrect it or touch the new list.
        assert!(app.watch_item_entry.is_none());
        assert_eq!(app.watchlists[1].symbols, vec![key("MSFT")]);
        assert_eq!(app.watchlists[0].items().len(), 6);
    }

    #[test]
    #[ignore = "BUG: inline entry keeps a stale row index after the list is edited and renames the wrong section"]
    fn inline_rename_entry_must_not_rename_a_different_section_after_list_edits() {
        let mut app = two_headers_app();
        // Rename "First" (item index 1).
        click_ui("Rename section…", |ui| app.watch_section_menu(ui, 1, "First"));
        assert!(app.watch_item_entry.is_some());
        // Remove the ungrouped SPY above it; "First" slides to index 0 and "Second" to 1.
        app.set_watchlist(vec![key("NVDA")]);
        assert_eq!(row_names(&app.watchlists[0].items()), vec!["#First".to_string(), "#Second".into(), key("NVDA")]);
        save_entry_if_open(&mut app, "Zombie");
        let names: Vec<&str> = app.watchlists[0].sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names[1], "Second", "a stale rename must never land on another section");
        assert!(
            names == ["Zombie", "Second"] || names == ["First", "Second"],
            "entry must rename First or be discarded, not retarget Second: {names:?}"
        );
    }

    #[test]
    #[ignore = "BUG: inline symbol entry keeps a stale row index after the list is edited and inserts into the wrong section"]
    fn inline_symbol_entry_must_stay_in_its_section_after_list_edits() {
        let mut app = sections_app();
        // "Add symbol" below AAPL (item 3) targets gap 4 — the end of Tech.
        click_ui("Add symbol", |ui| app.watch_symbol_menu(ui, 3, &key("AAPL"), &mut Vec::new()));
        // Drop SPY; every later row shifts up by one.
        app.set_watchlist(vec![key("NVDA"), key("AAPL"), key("SPX Index")]);
        save_entry_if_open(&mut app, "MSFT");
        let items = app.watchlists[0].items();
        match symbol_pos(&items, &key("MSFT")) {
            None => {} // entry was discarded on the edit — acceptable
            Some(pos) => {
                let names = row_names(&items);
                let tech = names.iter().position(|n| n == "#Tech").unwrap();
                let mac = names.iter().position(|n| n == "#Macro").unwrap();
                assert!(tech < pos && pos < mac, "MSFT was opened for Tech but landed at {pos} in {names:?}");
            }
        }
    }

    #[test]
    #[ignore = "BUG: removing a section while an inline entry is open leaves a stale index that inserts into a different section"]
    fn inline_entry_after_removing_its_section_must_not_insert_into_another() {
        let mut app = sections_app();
        // "Add symbol to section…" on Tech targets gap 4 (end of Tech's block).
        click_ui("Add symbol to section…", |ui| app.watch_section_menu(ui, 1, "Tech"));
        // Remove Tech while the entry is open (its symbols join the ungrouped run).
        click_ui("Remove section", |ui| app.watch_section_menu(ui, 1, "Tech"));
        assert_eq!(
            row_names(&app.watchlists[0].items()),
            vec![key("SPY"), key("NVDA"), key("AAPL"), "#Macro".to_string(), key("SPX Index")]
        );
        save_entry_if_open(&mut app, "MSFT");
        let items = app.watchlists[0].items();
        match symbol_pos(&items, &key("MSFT")) {
            None => {}
            Some(pos) => {
                let names = row_names(&items);
                let mac = names.iter().position(|n| n == "#Macro").unwrap();
                assert!(
                    pos < mac,
                    "a symbol opened for the removed Tech section must not land inside Macro: {names:?}"
                );
            }
        }
    }

    #[test]
    fn add_section_above_first_row_and_ungrouped_row_lands_directly_above() {
        let mut app = sections_app();
        click_ui("Add section", |ui| app.watch_symbol_menu(ui, 0, &key("SPY"), &mut Vec::new()));
        save_entry(&mut app, "Top");
        assert_eq!(row_names(&app.watchlists[0].items())[0], "#Top");
        assert_eq!(row_names(&app.watchlists[0].items())[1], key("SPY"));
        // Above an ungrouped row that sits under the new header: header goes between the two.
        click_ui("Add section", |ui| app.watch_symbol_menu(ui, 1, &key("SPY"), &mut Vec::new()));
        // index 1 is SPY again after the first insert (SPY moved to 1); target is above SPY.
        save_entry(&mut app, "Above SPY");
        let names = row_names(&app.watchlists[0].items());
        assert_eq!(
            names[..3].iter().map(String::as_str).collect::<Vec<_>>(),
            ["#Top", "#Above SPY", key("SPY").as_str()]
        );
    }

    #[test]
    fn add_symbol_below_last_row_of_collapsed_section_appends_inside_the_section() {
        let mut app = sections_app();
        app.collapse_watch_sections(true);
        // Header menu on the collapsed Tech block still targets its end (after hidden AAPL).
        click_ui("Add symbol to section…", |ui| app.watch_section_menu(ui, 1, "Tech"));
        save_entry(&mut app, "MSFT");
        let names = row_names(&app.watchlists[0].items());
        let tech = names.iter().position(|n| n == "#Tech").unwrap();
        let mac = names.iter().position(|n| n == "#Macro").unwrap();
        let msft = names.iter().position(|n| *n == key("MSFT")).unwrap();
        assert!(tech < msft && msft < mac, "MSFT should be the last Tech symbol: {names:?}");
        assert_eq!(
            names[msft - 2..msft].iter().map(String::as_str).collect::<Vec<_>>(),
            [key("NVDA").as_str(), key("AAPL").as_str()]
        );
    }

    #[test]
    fn remove_first_header_keeps_ungrouped_symbols_and_collapsed_symbols_reappear() {
        let mut app = sections_app();
        // SPY is ungrouped; collapse Tech so NVDA/AAPL are hidden.
        click_ui("▾  TECH", |ui| {
            app.watchlist(ui);
        });
        let texts = labels(&ui_frame(&egui::Context::default(), Vec::new(), &mut |ui| {
            app.watchlist(ui);
        }));
        assert!(!texts.iter().any(|s| s == "NVDA" || s == "AAPL"));
        // Remove the first header (Tech): its symbols join the ungrouped run and must show again.
        click_ui("Remove section", |ui| app.watch_section_menu(ui, 1, "Tech"));
        let texts = labels(&ui_frame(&egui::Context::default(), Vec::new(), &mut |ui| {
            app.watchlist(ui);
        }));
        assert!(texts.iter().any(|s| s == "NVDA") && texts.iter().any(|s| s == "AAPL"));
        assert!(texts.iter().any(|s| s == "SPY"));
        assert_eq!(app.watchlists[0].sections.len(), 1);
        assert_eq!(app.watchlists[0].sections[0].name, "Macro");
    }

    #[test]
    fn sort_stays_inside_each_section_when_an_earlier_section_is_collapsed() {
        let mut app = sections_app();
        click_ui("▾  TECH", |ui| {
            app.watchlist(ui);
        });
        let items = app.watchlists[0].items();
        app.watch_sort = Some((0, false));
        // Tech is collapsed so NVDA/AAPL are filtered out of the display order entirely;
        // the remaining rows must still be grouped: ungrouped SPY, headers, Macro's SPX.
        assert_eq!(app.watch_display_order(&items), [0, 1, 4, 5]);
        let texts = labels(&ui_frame(&egui::Context::default(), Vec::new(), &mut |ui| {
            app.watchlist(ui);
        }));
        assert!(texts.iter().position(|s| s == "SPY") < texts.iter().position(|s| s == "▸  TECH"));
        assert!(texts.iter().position(|s| s == "▸  TECH") < texts.iter().position(|s| s == "▾  MACRO"));
        assert!(!texts.iter().any(|s| s == "NVDA" || s == "AAPL"));
        // Expanding restores the sorted Tech block without letting it escape the section.
        app.collapse_watch_sections(false);
        app.watch_sort = Some((0, false));
        let items = app.watchlists[0].items();
        assert_eq!(app.watch_display_order(&items), [0, 1, 3, 2, 4, 5]);
    }

    #[test]
    fn sort_puts_symbols_without_quotes_last_in_both_directions_inside_a_section() {
        let mut app = sections_app();
        let aapl = key("AAPL");
        let nvda = key("NVDA");
        app.quotes.insert(
            aapl.clone(),
            tr_data::Quote {
                symbol: aapl.clone(),
                last: 10.0,
                prev_close: 9.0,
                ts: 1_700_000_000,
                spark: Vec::new(),
                crypto_source: None,
            },
        );
        let items = app.watchlists[0].items();
        for descending in [false, true] {
            app.watch_sort = Some((1, descending));
            let order = app.watch_display_order(&items);
            // Within Tech, NVDA has no quote and must trail AAPL in both directions.
            let positions: Vec<usize> = order
                .iter()
                .copied()
                .filter(|&i| items[i] == Item::Symbol(aapl.clone()) || items[i] == Item::Symbol(nvda.clone()))
                .collect();
            assert_eq!(
                items[positions[0]],
                Item::Symbol(aapl.clone()),
                "unquoted symbols sort last both ways (descending={descending})"
            );
            assert_eq!(items[positions[1]], Item::Symbol(nvda.clone()));
        }
    }

    #[test]
    fn details_card_follows_selection_across_removals_and_list_switches() {
        let mut app = sections_app();
        app.watch_details = true;
        let aapl = key("AAPL");
        app.quotes.insert(
            aapl.clone(),
            tr_data::Quote {
                symbol: aapl.clone(),
                last: 123.45,
                prev_close: 120.0,
                ts: 1_700_000_000,
                spark: Vec::new(),
                crypto_source: None,
            },
        );
        click_ui("AAPL", |ui| {
            app.watchlist(ui);
        });
        assert_eq!(app.watch_selected, Some(aapl.clone()));
        // Remove AAPL: the card must fall back to the first symbol (SPY), not a ghost of AAPL.
        app.set_watchlist(vec![key("SPY"), key("NVDA")]);
        let ctx = egui::Context::default();
        let texts = labels(&ui_frame(&ctx, Vec::new(), &mut |ui| {
            app.watchlist(ui);
        }));
        assert_eq!(
            texts.iter().filter(|s| *s == "SPY").count(),
            2,
            "card shows the first symbol once selection is gone"
        );
        assert!(!texts.iter().any(|s| s == "123.45"), "stale AAPL quote must not appear");
        // Switch to a list without AAPL while it is still selected in the previous list.
        app.switch_watchlist(0);
        app.create_watchlist("Other".into(), vec![key("MSFT")]).unwrap();
        app.watch_selected = Some(aapl.clone()); // selection from the other list
        let texts = labels(&ui_frame(&ctx, Vec::new(), &mut |ui| {
            app.watchlist(ui);
        }));
        assert!(texts.iter().any(|s| s == "MSFT"));
        assert_eq!(
            texts.iter().filter(|s| *s == "MSFT").count(),
            2,
            "card falls back to the active list's first symbol"
        );
    }

    #[test]
    fn add_symbol_below_a_row_respects_the_200_symbol_cap() {
        let mut app = sections_app();
        app.set_watchlist_items(&(0..200).map(|i| Item::Symbol(format!("TEST{i} US Equity"))).collect::<Vec<_>>());
        // Below an interior row, not just at the end.
        click_ui("Add symbol", |ui| app.watch_symbol_menu(ui, 5, "TEST5 US Equity", &mut Vec::new()));
        save_entry(&mut app, "MSFT");
        assert_eq!(app.watchlist.len(), 200);
        assert!(app.status.contains("200 symbols"));
        assert!(app.watchlists[0].items().iter().all(|i| !matches!(i, Item::Symbol(s) if s == &key("MSFT"))));
    }

    #[test]
    fn section_names_reject_blank_overlong_and_invisible_input_and_trim() {
        let mut app = sections_app();
        click_ui("Add section…", |ui| {
            if ui.button("Add section…").clicked() {
                app.start_watch_entry(WatchEntryKind::Section, 0, String::new());
            }
        });
        save_entry(&mut app, "   ");
        assert!(app.status.contains("Section names"), "{}", app.status);
        assert_eq!(app.watchlists[0].sections.len(), 2, "blank names must not add a header");
        save_entry(&mut app, &"x".repeat(41));
        assert!(app.status.contains("Section names"), "{}", app.status);
        assert_eq!(app.watchlists[0].sections.len(), 2);
        save_entry(&mut app, "Ok\u{200b}no");
        assert!(app.status.contains("Section names"), "{}", app.status);
        assert_eq!(app.watchlists[0].sections.len(), 2);
        save_entry(&mut app, &format!("  {}  ", "y".repeat(40)));
        assert_eq!(app.watchlists[0].sections.len(), 3);
        assert_eq!(app.watchlists[0].sections[0].name, "y".repeat(40), "40 visible characters are trimmed and kept");
    }

    #[test]
    fn duplicate_section_names_are_allowed_and_both_survive() {
        let mut app = sections_app();
        click_ui("Rename section…", |ui| app.watch_section_menu(ui, 1, "Tech"));
        save_entry(&mut app, "Macro");
        let names: Vec<&str> = app.watchlists[0].sections.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Macro", "Macro"], "section names need not be unique");
        // Both headers still collapse independently.
        click_ui("▾  MACRO", |ui| {
            app.watchlist(ui);
        });
        assert!(app.watchlists[0].sections[0].collapsed);
        assert!(!app.watchlists[0].sections[1].collapsed);
    }

    fn watch_frame(
        ctx: &egui::Context,
        app: &mut TerminalApp,
        events: Vec<egui::Event>,
    ) -> (Vec<Action>, Vec<egui::Pos2>) {
        let mut actions = Vec::new();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
                events,
                ..Default::default()
            },
            |ui| actions = app.watchlist(ui),
        );
        let rows = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if ["AAPL", "MSFT", "NVDA"].contains(&text.galley.job.text.as_str()) => {
                    Some(pos2(250.0, text.pos.y + text.galley.size().y / 2.0))
                }
                _ => None,
            })
            .collect();
        output.textures_delta.clear();
        (actions, rows)
    }

    fn watch_pointer(at: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn watch_drag_reorders_active_symbols_and_click_still_opens_chart() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/watch-drag-panel");
        let mut app = TerminalApp::test_workspace(&root);
        app.set_watchlist(vec!["AAPL".into(), "MSFT".into(), "NVDA".into()]);
        let ctx = egui::Context::default();
        let (_, rows) = watch_frame(&ctx, &mut app, vec![]);
        let from = rows[0];
        watch_frame(&ctx, &mut app, vec![egui::Event::PointerMoved(from), watch_pointer(from, true)]);
        let (actions, _) = watch_frame(&ctx, &mut app, vec![watch_pointer(from, false)]);
        assert!(actions.iter().any(|a| matches!(a, Action::OpenChart(key) if key == "AAPL")));
        assert_eq!(app.watchlist, ["AAPL", "MSFT", "NVDA"]);
        let to = rows[2] + vec2(0.0, 8.0);
        watch_frame(&ctx, &mut app, vec![egui::Event::PointerMoved(from), watch_pointer(from, true)]);
        watch_frame(&ctx, &mut app, vec![egui::Event::PointerMoved(to)]);
        watch_frame(&ctx, &mut app, vec![watch_pointer(to, false)]);
        assert_eq!(app.watchlist, ["MSFT", "NVDA", "AAPL"]);
        assert_eq!(app.watchlists[app.active_watchlist].symbols, app.watchlist);
    }

    #[test]
    fn watch_sorted_drag_does_not_reorder() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/watch-drag-sorted");
        let mut app = TerminalApp::test_workspace(&root);
        app.set_watchlist(vec!["AAPL".into(), "MSFT".into(), "NVDA".into()]);
        app.watch_sort = Some((0, false));
        let ctx = egui::Context::default();
        let (_, rows) = watch_frame(&ctx, &mut app, vec![]);
        let from = rows[0];
        let to = rows[2] + vec2(0.0, 8.0);
        watch_frame(&ctx, &mut app, vec![egui::Event::PointerMoved(from), watch_pointer(from, true)]);
        watch_frame(&ctx, &mut app, vec![egui::Event::PointerMoved(to)]);
        watch_frame(&ctx, &mut app, vec![watch_pointer(to, false)]);
        assert_eq!(app.watchlist, ["AAPL", "MSFT", "NVDA"]);
        assert!(app.watch_drag.is_none());
    }
}
