//! Pane content for every panel kind.

use std::time::Instant;

use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, Ui, UiBuilder, pos2, vec2};
use tr_data::OrderBook;

use super::{PaneKind, TerminalApp};
use crate::congress_view::{self, Action};
use crate::markets_view;
use crate::theme::{self, amber, dp, mono, muted, text, ui_font};
use crate::widgets;
use crate::worker::Request;

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
                let bulk_ready = self.broker.questrade.as_ref().is_some_and(|q| q.is_configured());
                actions.extend(super::scanner_view::show(ui, s, id, &self.watchlist, bulk_ready, |request| {
                    self.worker.send(request)
                }));
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
        let row_h = dp(22.0);
        let w = ui.available_width();
        let (hdr, _) = ui.allocate_exact_size(vec2(w, row_h), Sense::hover());
        let p = ui.painter();
        p.rect_filled(hdr, theme::CONTROL_RADIUS, theme::header());
        let extra = self.watch_columns.columns.len().min(((w - dp(380.0)).max(0.0) / dp(85.0)) as usize);
        let base = if extra == 0 { 1.0 } else { (dp(380.0) / w).max(0.45) };
        let mut cols = vec![0.0, 0.34 * base, 0.58 * base, 0.79 * base, base];
        for i in 0..extra {
            cols.push(base + (1.0 - base) * (i + 1) as f32 / extra as f32);
        }
        let col = |r: Rect, k: usize| r.left() + 8.0 + (r.width() - 16.0) * cols[k];
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
            let right = if index == 0 { col(hdr, 1) } else { col(hdr, k) };
            let hit = Rect::from_min_max(pos2(left, hdr.top()), pos2(right, hdr.bottom()));
            if ui.interact(hit, ui.id().with(("watch-header", index)), Sense::click()).clicked() {
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
        let mut symbols = self.watchlist.clone();
        if let Some((index, descending)) = self.watch_sort {
            symbols.sort_by(|a, b| {
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
            });
        }
        let stale = chrono::Utc::now().timestamp() - self.columns_computed_at > 180;
        let mut remove = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for s in &symbols {
                let sec = tr_data::security(s);
                let (r, resp) = ui.allocate_exact_size(vec2(w, row_h), Sense::click());
                widgets::hover_wash(ui, r, &resp, false);
                let p = ui.painter();
                p.text(pos2(col(r, 0), r.center().y), Align2::LEFT_CENTER, &sec.ticker, mono(12.5), amber());
                p.text(
                    pos2(col(r, 0) + 74.0, r.center().y),
                    Align2::LEFT_CENTER,
                    sec.class.yellow_key().label(),
                    ui_font(11.0),
                    muted(),
                );
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
                        Some(Cell::Value(value)) => (tr_chart::scale::format_value(*value), text(), None),
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
                        let hit =
                            Rect::from_min_max(pos2(col(r, index + 4), r.top()), pos2(col(r, index + 5), r.bottom()));
                        ui.interact(hit, ui.id().with(("watch-cell", s, index)), Sense::hover()).on_hover_text(reason);
                    }
                }
                let tradable = matches!(sec.class, tr_core::AssetClass::Equity | tr_core::AssetClass::Etf);
                let pills = if tradable { widgets::buy_sell_pills().to_vec() } else { Vec::new() };
                let tips = [format!("Buy {} (⇧B)", sec.ticker), format!("Sell {} (⇧S)", sec.ticker)];
                let tips: Vec<&str> = tips.iter().map(String::as_str).collect();
                match widgets::row_pills(ui, r, ("wl-pills", s), &pills, &tips) {
                    Some(i) => out.push(Action::Command(format!("{} {s}", if i == 0 { "BUY" } else { "SELL" }))),
                    None if resp.clicked() => out.push(Action::OpenChart(s.clone())),
                    None => {}
                }
                resp.context_menu(|ui| {
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
                        out.push(Action::OpenChart(s.clone()));
                        ui.close();
                    }
                    if sec.has_congress_trades() && ui.button("Who in Congress Traded This").clicked() {
                        out.push(Action::OpenTicker(sec.ticker.clone()));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Remove from Watchlist").clicked() {
                        remove = Some(s.clone());
                        ui.close();
                    }
                });
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new("WL ADD <KEY> to add · right-click a row for options").font(ui_font(11.0)).color(muted()),
            );
        });
        if let Some(s) = remove {
            self.watchlist.retain(|x| *x != s);
        }
        out
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
    fn group_steps_scale_with_price() {
        assert_eq!(group_steps(82_000.0), [0.0, 1.0, 10.0, 100.0]);
        let fx = group_steps(1.09);
        assert!((fx[1] - 1e-4).abs() < 1e-12);
        assert_eq!(fmt_size(1_500.0), "1.50K");
        assert_eq!(fmt_size(0.5), "0.50000");
    }
}
