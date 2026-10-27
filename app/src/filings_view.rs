//! CF: stored watchlist filings. Rendering never starts a fetch or schedules a repaint.
use crate::theme::{self, dp};
use egui::{RichText, Ui};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tr_edgar::Filing;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    Symbol,
    #[default]
    Watchlist,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Form {
    #[default]
    All,
    Current,
    Reports,
    Insider,
    Ownership,
    Offerings,
    Proxy,
}
impl Form {
    pub const ALL: [Self; 7] =
        [Self::All, Self::Current, Self::Reports, Self::Insider, Self::Ownership, Self::Offerings, Self::Proxy];
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Current => "8-K",
            Self::Reports => "10-K/Q",
            Self::Insider => "Insider",
            Self::Ownership => "13D/G",
            Self::Offerings => "Offerings",
            Self::Proxy => "Proxy",
        }
    }
    pub fn matches(self, form: &str) -> bool {
        let f = form.strip_suffix("/A").unwrap_or(form);
        match self {
            Self::All => true,
            Self::Current => f == "8-K",
            Self::Reports => matches!(f, "10-Q" | "10-K"),
            Self::Insider => matches!(f, "3" | "4" | "5"),
            Self::Ownership => matches!(f, "SC 13D" | "SC 13G"),
            Self::Offerings => f == "S-1" || f.starts_with("424B"),
            Self::Proxy => f == "DEF 14A",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub symbol: Option<String>,
    pub scope: Scope,
    pub form: Form,
    #[serde(default = "default_days")]
    pub days: u16,
    pub search: String,
    pub buys_sells_only: bool,
    pub unread_only: bool,
    pub paused: bool,
}
fn default_days() -> u16 {
    30
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            symbol: None,
            scope: Scope::Watchlist,
            form: Form::All,
            days: 30,
            search: String::new(),
            buys_sells_only: false,
            unread_only: false,
            paused: false,
        }
    }
}
impl Settings {
    pub fn new(symbol: Option<String>) -> Self {
        Self { scope: if symbol.is_some() { Scope::Symbol } else { Scope::Watchlist }, symbol, ..Self::default() }
    }
    pub fn matches_at(&self, filing: &Filing, watched: &[String], now: i64) -> bool {
        let query = self.search.trim().to_ascii_lowercase();
        self.matches(filing, watched)
            && filing.time() >= now - i64::from(self.days) * 86400
            && (!self.buys_sells_only || filing.insiders.iter().any(|t| matches!(t.code.as_str(), "P" | "S")))
            && (query.is_empty()
                || format!(
                    "{} {} {} {} {}",
                    filing.ticker,
                    filing.tickers.join(" "),
                    filing.form,
                    filing.detail(),
                    filing.insiders.iter().map(|t| t.owner.name.as_str()).collect::<Vec<_>>().join(" ")
                )
                .to_ascii_lowercase()
                .contains(&query))
    }
    pub fn display_ticker(&self, filing: &Filing, watched: &[String]) -> String {
        let display = if self.scope == Scope::Symbol {
            self.symbol.as_deref().filter(|s| filing.matches_symbol(s)).and_then(tr_edgar::ticker)
        } else {
            watched.iter().filter(|s| filing.matches_symbol(s)).filter_map(|s| tr_edgar::ticker(s)).min()
        };
        display.unwrap_or_else(|| filing.ticker.clone())
    }
    pub fn matches(&self, filing: &Filing, watched: &[String]) -> bool {
        !filing.reporting_owner
            && watched.iter().any(|s| filing.matches_symbol(s))
            && self.form.matches(&filing.form)
            && (self.scope == Scope::Watchlist || self.symbol.as_deref().is_some_and(|s| filing.matches_symbol(s)))
    }
}
#[derive(Default)]
pub struct State {
    pub interest: Option<(bool, Vec<String>)>,
    pub rows: Arc<Vec<Filing>>,
    pub status: String,
    pub updated: i64,
}
pub fn safe_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
    })
}
#[derive(Clone, Default)]
struct Runtime {
    selected: Option<String>,
    expanded: std::collections::BTreeSet<String>,
    read: std::collections::BTreeSet<String>,
    selected_at: Option<std::time::Instant>,
    paused_rows: Option<Arc<Vec<Filing>>>,
    ids: Vec<String>,
    heights: std::collections::BTreeMap<String, f32>,
    anchor: Option<(String, f32)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmptyCause {
    NotTracked,
    Importing,
    Filtered,
    NoHistory,
    Offline,
}
pub fn empty_cause(settings: &Settings, state: &State, watched: &[String]) -> EmptyCause {
    if settings.scope == Scope::Symbol
        && settings.symbol.as_ref().is_some_and(|s| !watched.iter().any(|w| tr_edgar::ticker(w) == tr_edgar::ticker(s)))
    {
        return EmptyCause::NotTracked;
    }
    if ["Offline", "Paused", "disabled", "Needs contact", "budget"].iter().any(|s| state.status.contains(s)) {
        return EmptyCause::Offline;
    }
    let has_scope = state.rows.iter().any(|f| {
        !f.reporting_owner
            && watched.iter().any(|w| f.matches_symbol(w))
            && (settings.scope == Scope::Watchlist || settings.symbol.as_deref().is_some_and(|s| f.matches_symbol(s)))
    });
    if (state.rows.is_empty() && state.updated == 0) || (!has_scope && state.status.contains("Importing filings:")) {
        return EmptyCause::Importing;
    }
    if settings.form != Form::All
        || settings.buys_sells_only
        || !settings.search.is_empty()
        || state.rows.iter().any(|f| settings.matches(f, watched))
    {
        EmptyCause::Filtered
    } else {
        EmptyCause::NoHistory
    }
}
#[derive(Default, Debug, PartialEq)]
pub struct Summary {
    pub buys: usize,
    pub sells: usize,
    pub bought: f64,
    pub sold: f64,
    pub cluster: bool,
}
pub fn summary(rows: &[Filing], symbol: &str, now: i64) -> Summary {
    let mut s = Summary::default();
    let mut owners = std::collections::BTreeSet::new();
    for f in rows.iter().filter(|f| !f.reporting_owner && f.matches_symbol(symbol)) {
        let first = f.insiders.first().map(|t| &t.owner);
        for t in &f.insiders {
            let Some(date) = chrono::NaiveDate::parse_from_str(&t.date, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|d| d.and_utc().timestamp())
            else {
                continue;
            };
            if date < now - 90 * 86400 || date > now {
                continue;
            }
            if t.code == "P" && date >= now - 30 * 86400 {
                owners.insert(t.owner.cik.map_or_else(|| t.owner.name.to_ascii_lowercase(), |c| c.to_string()));
            }
            // Joint owners describe the same economic lots; don't multiply their value.
            if Some(&t.owner) != first {
                continue;
            }
            let amount = t.shares.zip(t.price).map(|(a, b)| a * b).unwrap_or(0.);
            match t.code.as_str() {
                "P" => {
                    s.buys += 1;
                    s.bought += amount;
                }
                "S" => {
                    s.sells += 1;
                    s.sold += amount;
                }
                _ => {}
            }
        }
    }
    s.cluster = owners.len() >= 2;
    s
}
fn text_has_focus(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.focused()).is_some_and(|id| egui::TextEdit::load_state(ctx, id).is_some())
}
pub fn owns_key(key: egui::Key) -> bool {
    matches!(
        key,
        egui::Key::ArrowUp
            | egui::Key::ArrowDown
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
pub fn move_selection(selected: Option<&str>, ids: &[&str], down: bool) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    let index = selected.and_then(|id| ids.iter().position(|s| *s == id));
    let next = match index {
        None => {
            if down {
                0
            } else {
                ids.len() - 1
            }
        }
        Some(i) => {
            if down {
                (i + 1).min(ids.len() - 1)
            } else {
                i.saturating_sub(1)
            }
        }
    };
    Some(ids[next].to_owned())
}
fn direction(code: &str) -> egui::Color32 {
    match tr_edgar::insider::action(code).1 {
        tr_edgar::insider::Direction::Up => theme::up(),
        tr_edgar::insider::Direction::Down => theme::down(),
        _ => theme::text(),
    }
}
pub fn row_text(f: &Filing, only_open_market: bool) -> (String, String) {
    use tr_edgar::insider::{aggregate, short_name, value};
    if !matches!(f.form.as_str(), "4" | "4/A") {
        return (
            if f.form.starts_with("8-K") {
                f.items
                    .split([',', ';', ' '])
                    .filter(|s| !s.is_empty())
                    .map(|code| match code {
                        "2.02" => "Results of operations".into(),
                        "9.01" => "Financial statements".into(),
                        _ => tr_edgar::decode_items(code)
                            .split_once(' ')
                            .map(|(_, s)| s.to_owned())
                            .unwrap_or_else(|| code.into()),
                    })
                    .collect::<Vec<String>>()
                    .join(" · ")
            } else {
                match f.form.trim_end_matches("/A") {
                    "10-K" => "Annual report".into(),
                    "10-Q" => "Quarterly report".into(),
                    _ => f.description.clone(),
                }
            },
            String::new(),
        );
    }
    if f.insiders.is_empty() {
        return (
            if f.insider_parsed {
                "No reported transactions"
            } else if f.insider_unavailable {
                "details unavailable"
            } else {
                "Reading insider form…"
            }
            .into(),
            "—".into(),
        );
    }
    let has_common = f.insiders.iter().any(|t| !t.derivative);
    let lots: Vec<_> = f
        .insiders
        .iter()
        .filter(|t| {
            (!t.derivative || !has_common || matches!(t.code.as_str(), "P" | "S"))
                && (!only_open_market || matches!(t.code.as_str(), "P" | "S"))
        })
        .cloned()
        .collect();
    let groups = aggregate(&lots);
    let mut text = vec![];
    let mut previous = None;
    let mut total = Some(0.);
    let first = groups.first().map(|g| &g.owner);
    for g in &groups {
        if previous != Some(&g.owner) {
            text.push(format!("{} ({})", short_name(&g.owner.name), g.owner.role()));
            previous = Some(&g.owner);
        }
        text.push(g.text());
        if Some(&g.owner) == first {
            total = total.zip(g.value).map(|(a, b)| a + b);
        }
    }
    if !has_common {
        text.push("derivative".into());
    }
    if lots.iter().any(|t| t.plan_10b5_1) {
        text.push("10b5-1".into());
    }
    (text.join(" · "), total.filter(|_| !groups.is_empty()).map(value).unwrap_or_else(|| "—".into()))
}
fn open(ui: &Ui, f: &Filing) {
    let link = f.link();
    if safe_url(&link) {
        ui.ctx().open_url(egui::OpenUrl::new_tab(link));
    }
}
/// Context supplied by the pane owner, not guessed from widget focus.
pub struct PaneContext<'a> {
    pub focused: bool,
    pub link: Option<crate::layouts::Link>,
    pub watchlist_name: &'a str,
}
fn chart_action(ticker: &str, link: Option<crate::layouts::Link>) -> crate::congress_view::Action {
    let key = format!("{ticker} US Equity");
    link.map_or_else(
        || crate::congress_view::Action::OpenChart(key.clone()),
        |l| crate::congress_view::Action::LinkedSymbol(l, key.clone()),
    )
}
pub fn show(
    ui: &mut Ui,
    settings: &mut Settings,
    state: &State,
    watched: &[String],
    context: PaneContext<'_>,
) -> Vec<crate::congress_view::Action> {
    let mut actions = vec![];
    let now = chrono::Utc::now().timestamp();
    let runtime_id = ui.id().with("filings-runtime");
    let mut runtime = ui.data_mut(|d| d.get_temp::<Runtime>(runtime_id)).unwrap_or_default();
    let mut focus_search = false;
    ui.spacing_mut().item_spacing = egui::vec2(dp(6.), dp(3.));
    ui.horizontal(|ui| {
        ui.selectable_value(&mut settings.scope, Scope::Symbol, "Symbol");
        ui.selectable_value(&mut settings.scope, Scope::Watchlist, "Watchlist");
        let symbol = if settings.scope == Scope::Symbol {
            settings.symbol.as_deref().and_then(tr_edgar::ticker).unwrap_or_default()
        } else {
            context.watchlist_name.into()
        };
        ui.label(RichText::new(symbol).font(theme::mono(11.)).color(theme::amber()));
        if let Some(link) = context.link {
            let color = match link {
                crate::layouts::Link::Red => theme::down(),
                crate::layouts::Link::Orange => theme::amber(),
                crate::layouts::Link::Green => theme::up(),
                crate::layouts::Link::Blue => theme::accent(),
                crate::layouts::Link::Purple => theme::series(4),
            };
            ui.label(RichText::new("●").color(color)).on_hover_text(format!("{} link group", link.label()));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.menu_button("⋯", |ui| {
                ui.checkbox(&mut settings.paused, "Pause updates");
                if ui.button("Reload").clicked() {
                    runtime.paused_rows = None;
                    settings.paused = false;
                    actions.push(crate::congress_view::Action::ReloadFilings);
                    ui.close();
                }
                ui.label(
                    RichText::new("SEC EDGAR · public filings. Watchlist symbols only; no market-wide data.")
                        .color(theme::muted()),
                );
            });
            if settings.paused
                && let Some(saved) = &runtime.paused_rows
            {
                let old: std::collections::BTreeSet<_> = saved.iter().map(|f| f.accession.as_str()).collect();
                let new = state.rows.iter().filter(|f| !old.contains(f.accession.as_str())).count();
                if new > 0 && ui.button(format!("{new} new · Show")).clicked() {
                    settings.paused = false;
                    runtime.paused_rows = None;
                }
            }
            let status = if settings.paused {
                "Updates paused".into()
            } else if state.status.starts_with("SEC EDGAR · updated") {
                let suffix = state
                    .status
                    .split_once(" · ")
                    .and_then(|(_, s)| s.split_once(" · "))
                    .map(|(_, s)| format!(" · {s}"))
                    .unwrap_or_default();
                let (age, _) = crate::feed_time::feed_time(state.updated, now, false);
                format!("SEC EDGAR · {age}{suffix}")
            } else if state.status.is_empty() {
                "SEC EDGAR · waiting for worker".into()
            } else {
                state.status.clone()
            };
            ui.add(egui::Label::new(RichText::new(status).font(theme::ui_font(10.5)).color(theme::muted())).truncate());
        });
    });
    // Fold before controls would clip, including when larger theme fonts need more room.
    let chips_width = Form::ALL
        .iter()
        .map(|f| ui.painter().layout_no_wrap(f.label().into(), theme::ui_font(10.5), theme::text()).size().x + dp(14.))
        .sum::<f32>();
    let narrow = ui.available_width() < dp(360.).max(chips_width + dp(230.));
    let search_id = ui.id().with("filings-search");
    // Keyboard input is processed before the search field, so '/' can focus it this frame.
    if context.focused && !text_has_focus(ui.ctx()) {
        focus_search = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Slash));
    }
    ui.horizontal(|ui| {
        ui.style_mut().override_font_id = Some(theme::ui_font(10.5));
        ui.spacing_mut().button_padding = egui::vec2(dp(4.), dp(2.));
        ui.spacing_mut().item_spacing.x = dp(3.);
        for days in [7, 30, 90] {
            ui.selectable_value(&mut settings.days, days, format!("{days}D"));
        }
        if !narrow {
            for form in Form::ALL {
                ui.selectable_value(&mut settings.form, form, form.label());
            }
        }
        if narrow {
            let response = ui.button("⌕").on_hover_text("Search (/)");
            if focus_search && !response.clicked() {
                egui::Popup::toggle_id(ui.ctx(), egui::Popup::default_response_id(&response));
            }
            if response.clicked() {
                focus_search = true;
            }
            egui::Popup::menu(&response).show(|ui| {
                let search = ui.add(egui::TextEdit::singleline(&mut settings.search).id(search_id).hint_text("Search"));
                if focus_search {
                    search.request_focus();
                }
            });
        } else {
            let response = ui.add(
                egui::TextEdit::singleline(&mut settings.search)
                    .id(search_id)
                    .desired_width((ui.available_width() - dp(90.)).max(dp(40.)))
                    .hint_text("Search"),
            );
            if focus_search {
                response.request_focus();
            }
        }
        let n = usize::from(settings.buys_sells_only) + usize::from(settings.unread_only);
        ui.menu_button(format!("Filters ({n})"), |ui| {
            if narrow {
                for form in Form::ALL {
                    ui.selectable_value(&mut settings.form, form, form.label());
                }
                let response =
                    ui.add(egui::TextEdit::singleline(&mut settings.search).id(search_id).hint_text("Search"));
                if focus_search {
                    response.request_focus();
                }
            }
            ui.checkbox(&mut settings.buys_sells_only, "Buys and sells only");
            ui.checkbox(&mut settings.unread_only, "Unread only");
        });
    });
    if focus_search && narrow {
        ui.memory_mut(|m| m.request_focus(search_id));
    }
    if settings.paused {
        if runtime.paused_rows.is_none() {
            runtime.paused_rows = Some(state.rows.clone());
        }
    } else {
        runtime.paused_rows = None;
    }
    let stored = runtime.paused_rows.clone().unwrap_or_else(|| state.rows.clone());
    if settings.scope == Scope::Symbol && settings.form == Form::Insider {
        let s = summary(&stored, settings.symbol.as_deref().unwrap_or(""), now);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Insiders 90D").font(theme::mono(10.)).color(theme::muted()));
            ui.label(
                RichText::new(format!("· {} sells {}", s.sells, tr_edgar::insider::value(s.sold)))
                    .font(theme::mono(10.))
                    .color(theme::down()),
            );
            ui.label(
                RichText::new(format!("· {} buys {}", s.buys, tr_edgar::insider::value(s.bought)))
                    .font(theme::mono(10.))
                    .color(theme::up()),
            );
            if s.cluster {
                ui.label(RichText::new("Cluster buy").color(theme::amber()));
            }
        });
    }
    let mut rows: Vec<_> = stored
        .iter()
        .filter(|f| {
            settings.matches_at(f, watched, now) && (!settings.unread_only || !runtime.read.contains(&f.accession))
        })
        .collect();
    rows.sort_by_key(|f| (std::cmp::Reverse(f.time()), std::cmp::Reverse(&f.accession)));
    let mut scroll_selected = false;
    if context.focused && !text_has_focus(ui.ctx()) {
        let pressed = |key| ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key));
        let down = pressed(egui::Key::ArrowDown) || pressed(egui::Key::J);
        let up = pressed(egui::Key::ArrowUp) || pressed(egui::Key::K);
        if down || up {
            runtime.selected = move_selection(
                runtime.selected.as_deref(),
                &rows.iter().map(|f| f.accession.as_str()).collect::<Vec<_>>(),
                down,
            );
            runtime.selected_at = Some(std::time::Instant::now());
            scroll_selected = true;
        }
        if pressed(egui::Key::Escape) {
            if !settings.search.is_empty() {
                settings.search.clear();
            } else {
                runtime.selected = None;
            }
        }
        if pressed(egui::Key::U) {
            settings.unread_only = !settings.unread_only;
        }
        if let Some(f) = rows.iter().find(|f| Some(&f.accession) == runtime.selected.as_ref()) {
            if pressed(egui::Key::Enter) {
                open(ui, f);
                runtime.read.insert(f.accession.clone());
            }
            if pressed(egui::Key::Space) {
                if !runtime.expanded.remove(&f.accession) {
                    runtime.expanded.insert(f.accession.clone());
                }
                runtime.read.insert(f.accession.clone());
            }
            if pressed(egui::Key::C) {
                actions.push(chart_action(&settings.display_ticker(f, watched), context.link));
            }
        }
    } else if context.focused
        && ui.memory(|m| m.has_focus(search_id))
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        settings.search.clear();
        ui.memory_mut(|m| m.surrender_focus(search_id));
    }
    if runtime.selected_at.is_some_and(|t| t.elapsed() >= std::time::Duration::from_secs(1))
        && let Some(id) = &runtime.selected
    {
        runtime.read.insert(id.clone());
    }
    if rows.is_empty() {
        let cause = empty_cause(settings, state, watched);
        let ticker = settings.symbol.as_deref().and_then(tr_edgar::ticker).unwrap_or_else(|| "this watchlist".into());
        let text = match cause {
            EmptyCause::NotTracked => {
                format!("{ticker} is not on a watchlist. Filings are tracked for watchlist symbols only.")
            }
            EmptyCause::Importing => state
                .status
                .find("Importing filings:")
                .map(|i| state.status[i..].to_owned())
                .unwrap_or_else(|| "Importing filings (one company a minute while visible).".into()),
            EmptyCause::Filtered => {
                format!("No {} for {ticker} in the last {} days.", settings.form.label(), settings.days)
            }
            EmptyCause::NoHistory => "No stored filings in this window.".into(),
            EmptyCause::Offline => "Stored filings retained while updates are unavailable.".into(),
        };
        ui.label(RichText::new(text).font(theme::ui_font(11.)).color(theme::muted()));
        if cause == EmptyCause::NotTracked
            && ui.button("Add to watchlist").clicked()
            && let Some(s) = &settings.symbol
        {
            actions.push(crate::congress_view::Action::AddToWatchlist(s.clone()));
        }
        let clear_all = settings.form == Form::All;
        if cause == EmptyCause::Filtered
            && ui.button(if clear_all { "Clear filters" } else { "Show all forms" }).clicked()
        {
            if clear_all {
                settings.days = 90;
            }
            settings.form = Form::All;
            settings.search.clear();
            settings.buys_sells_only = false;
            settings.unread_only = false;
        }
    }
    let ids: Vec<_> = rows.iter().map(|f| f.accession.clone()).collect();
    let mut scroll = egui::ScrollArea::vertical().id_salt("filings-rows").auto_shrink([false, false]);
    if ids != runtime.ids
        && let Some((id, intra)) = &runtime.anchor
    {
        let offset = ids
            .iter()
            .position(|s| s == id)
            .map(|index| {
                ids[..index].iter().map(|s| runtime.heights.get(s).copied().unwrap_or(dp(25.))).sum::<f32>() + intra
            })
            .unwrap_or(0.);
        scroll = scroll.vertical_scroll_offset(offset);
    }
    let output = scroll.show_viewport(ui, |ui, viewport| {
        let content_top = ui.cursor().min.y;
        let mut positions = vec![];
        for f in rows {
            let selected = runtime.selected.as_ref() == Some(&f.accession);
            // Keep long 90-day feeds cheap: only visible lots lay out text and hover widgets.
            // Cached expanded heights also preserve the top accession across refreshes.
            let y = ui.cursor().min.y - content_top;
            let estimated = if runtime.expanded.contains(&f.accession) {
                runtime.heights.get(&f.accession).copied().unwrap_or(dp(115.))
            } else {
                dp(22.) + ui.spacing().item_spacing.y
            };
            if !(selected && scroll_selected) && (y + estimated < viewport.top() || y > viewport.bottom()) {
                ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), (estimated - ui.spacing().item_spacing.y).max(0.)),
                    egui::Sense::hover(),
                );
                positions.push((f.accession.clone(), y, ui.cursor().min.y - content_top - y));
                continue;
            }
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), dp(22.)), egui::Sense::click());
            if selected || response.hovered() {
                ui.painter().rect_filled(
                    rect,
                    dp(2.),
                    if selected { theme::accent().linear_multiply(0.16) } else { theme::control_hover() },
                );
            }
            if selected && scroll_selected {
                ui.scroll_to_rect(rect, Some(egui::Align::Center));
            }
            if !runtime.read.contains(&f.accession) {
                ui.painter().circle_filled(egui::pos2(rect.left() + dp(3.), rect.center().y), dp(1.5), theme::accent());
            }
            let (time, full_time) = crate::feed_time::feed_time(f.time(), now, f.accepted.is_none());
            let ticker = settings.display_ticker(f, watched);
            let (text, value) = row_text(f, settings.buys_sells_only);
            let time_width = ui
                .painter()
                .layout_no_wrap("0".repeat(crate::feed_time::WIDTH_CHARS), theme::mono(10.), theme::muted())
                .size()
                .x;
            let ticker_width = ui.painter().layout_no_wrap("000000".into(), theme::mono(11.), theme::amber()).size().x;
            let form_width = ui.painter().layout_no_wrap("00000".into(), theme::mono(10.), theme::muted()).size().x;
            let time_right = rect.left() + dp(6.) + time_width;
            let ticker_left = time_right + dp(6.);
            let form_left = ticker_left + if settings.scope == Scope::Symbol { 0. } else { ticker_width + dp(6.) };
            let main_left = form_left + form_width + dp(6.);
            let painter = ui.painter();
            painter.text(
                egui::pos2(time_right, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                time,
                theme::mono(10.),
                theme::muted(),
            );
            let time_rect = egui::Rect::from_min_max(rect.min, egui::pos2(time_right, rect.bottom()));
            ui.interact(time_rect, ui.id().with((&f.accession, "time")), egui::Sense::hover()).on_hover_text(full_time);
            if settings.scope != Scope::Symbol {
                let r =
                    egui::Rect::from_min_max(egui::pos2(ticker_left, rect.top()), egui::pos2(form_left, rect.bottom()));
                ui.painter().text(
                    egui::pos2(ticker_left, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    ticker,
                    theme::mono(11.),
                    theme::amber(),
                );
                if ui.interact(r, ui.id().with((&f.accession, "ticker")), egui::Sense::click()).clicked() {
                    actions.push(chart_action(&settings.display_ticker(f, watched), context.link));
                }
            }
            ui.painter().text(
                egui::pos2(form_left, rect.center().y),
                egui::Align2::LEFT_CENTER,
                &f.form,
                theme::mono(10.),
                theme::muted(),
            );
            let value_width = if value.is_empty() { 0. } else { dp(76.) };
            let main_rect = egui::Rect::from_min_max(
                egui::pos2(main_left, rect.top()),
                egui::pos2((rect.right() - value_width).max(main_left), rect.bottom()),
            );
            let mut job = egui::text::LayoutJob::default();
            // Only action words P/S carry direction; owner/role/plan stay neutral.
            let mut remaining = text.as_str();
            while !remaining.is_empty() {
                let next = [("Bought", "P"), ("Sold", "S")]
                    .into_iter()
                    .filter(|(_, code)| {
                        matches!(f.form.as_str(), "4" | "4/A") && f.insiders.iter().any(|t| t.code == *code)
                    })
                    .filter_map(|(word, code)| {
                        let at = if remaining.starts_with(&format!("{word} ")) {
                            Some(0)
                        } else {
                            remaining.find(&format!(" · {word} ")).map(|i| i + " · ".len())
                        };
                        at.map(|i| (i, word, code))
                    })
                    .min_by_key(|(i, _, _)| *i);
                let (piece, code, rest) = if let Some((i, word, code)) = next {
                    if i > 0 {
                        (&remaining[..i], "", &remaining[i..])
                    } else {
                        (&remaining[..word.len()], code, &remaining[word.len()..])
                    }
                } else {
                    (remaining, "", "")
                };
                let color = if code.is_empty() {
                    if runtime.read.contains(&f.accession) { theme::muted() } else { theme::text() }
                } else {
                    direction(code)
                };
                for token in piece.split_inclusive(char::is_whitespace) {
                    let font_id =
                        if token.chars().any(|c| c.is_ascii_digit()) { theme::mono(12.) } else { theme::ui_font(12.) };
                    job.append(token, 0., egui::TextFormat { font_id, color, ..Default::default() });
                }
                remaining = rest;
            }
            job.wrap.max_width = main_rect.width();
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
            ui.painter().with_clip_rect(main_rect).galley(
                egui::pos2(main_left, rect.center().y - galley.size().y / 2.),
                galley,
                theme::text(),
            );
            ui.painter().text(
                egui::pos2(rect.right(), rect.center().y),
                egui::Align2::RIGHT_CENTER,
                value,
                theme::mono(10.),
                theme::muted(),
            );
            let title = ui.interact(main_rect, ui.id().with((&f.accession, "title")), egui::Sense::click());
            if title.clicked() {
                open(ui, f);
                runtime.read.insert(f.accession.clone());
            }
            if response.clicked() && !title.clicked() {
                runtime.selected = Some(f.accession.clone());
                runtime.selected_at = Some(std::time::Instant::now());
                if !runtime.expanded.remove(&f.accession) {
                    runtime.expanded.insert(f.accession.clone());
                }
                runtime.read.insert(f.accession.clone());
            }
            title.on_hover_text(format!(
                "{text}\n{}\n{}\n{}",
                f.insiders
                    .iter()
                    .map(|t| t.owner.name.as_str())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", "),
                f.accession,
                f.link()
            ));
            if runtime.expanded.contains(&f.accession) {
                ui.horizontal(|ui| {
                    ui.add_space(main_left - rect.left());
                    ui.vertical(|ui| {
                        egui::ScrollArea::vertical().id_salt((&f.accession, "lots")).max_height(dp(66.)).show(
                            ui,
                            |ui| {
                                if f.insiders.is_empty() {
                                    ui.label(RichText::new(&text).font(theme::ui_font(11.)).color(theme::muted()));
                                }
                                for t in &f.insiders {
                                    let date = chrono::NaiveDate::parse_from_str(&t.date, "%Y-%m-%d")
                                        .ok()
                                        .and_then(|d| d.and_hms_opt(0, 0, 0))
                                        .map(|d| crate::feed_time::feed_time(d.and_utc().timestamp(), now, true).0)
                                        .unwrap_or_else(|| "—".into());
                                    ui.label(
                                        RichText::new(format!(
                                            "{date} · {} · {} {} @ {} · owned {}{}",
                                            tr_edgar::insider::short_name(&t.owner.name),
                                            tr_edgar::insider::action(&t.code).0,
                                            t.shares.map(tr_edgar::insider::number).unwrap_or_else(|| "—".into()),
                                            t.price.map(|p| format!("{p:.2}")).unwrap_or_else(|| "—".into()),
                                            t.owned_after.map(tr_edgar::insider::number).unwrap_or_else(|| "—".into()),
                                            if t.derivative { " · derivative" } else { "" }
                                        ))
                                        .font(theme::mono(10.))
                                        .color(theme::muted()),
                                    );
                                }
                            },
                        );
                        ui.horizontal(|ui| {
                            if ui.small_button("Open").clicked() {
                                open(ui, f);
                            }
                            if ui.small_button("Chart").clicked() {
                                actions.push(chart_action(&settings.display_ticker(f, watched), context.link));
                            }
                            if ui.small_button("Copy link").clicked() {
                                ui.ctx().copy_text(f.link());
                            }
                        });
                    });
                });
            }
            positions.push((f.accession.clone(), rect.top() - content_top, ui.cursor().min.y - rect.top()));
        }
        positions
    });
    runtime.anchor = output
        .inner
        .iter()
        .find(|(_, y, height)| y + height > output.state.offset.y)
        .map(|(id, y, _)| (id.clone(), output.state.offset.y - y));
    for (id, _, height) in output.inner {
        runtime.heights.insert(id, height);
    }
    runtime.ids = ids;
    ui.data_mut(|d| d.insert_temp(runtime_id, runtime));
    actions
}
#[cfg(test)]
mod tests {
    use super::*;
    fn insider_fixture() -> Filing {
        Filing {
            ticker: "AAPL".into(),
            cik: 320193,
            form: "4".into(),
            accession: "sale".into(),
            primary_doc: "xslF345X06/form4.xml".into(),
            filed: chrono::Utc::now().timestamp(),
            insiders: tr_edgar::insider::parse(
                include_bytes!("../../crates/edgar/tests/fixtures/form4_sale_10b5_1.xml"),
                320193,
            )
            .unwrap()
            .transactions,
            ..Default::default()
        }
    }
    #[test]
    fn current_n2_layout_fixture_loads_with_30_day_defaults() {
        let doc =
            crate::layouts::LayoutDoc::from_json(include_bytes!("../tests/fixtures/filings_n2_layout.json")).unwrap();
        let crate::layouts::PaneSettings::Filings(s) = &doc.panes[0].settings else {
            panic!("Filings layout");
        };
        assert_eq!(s.days, 30);
        assert_eq!(s.form, Form::Insider);
        assert!(!s.buys_sells_only);
        assert!(!s.paused);
        assert!(s.search.is_empty());
        assert_eq!(crate::layouts::LayoutDoc::from_json(&doc.to_json()).unwrap(), doc);
    }
    #[test]
    fn row_grammar_real_prices_neutral_exercise_and_code_colours() {
        let f = insider_fixture();
        assert_eq!(
            row_text(&f, false),
            ("Newstead J. (SVP, GC) · Sold 2,399 @ 345.00 · 10b5-1".into(), "$828K".into())
        );
        let mut exercise = f.clone();
        exercise.insiders = tr_edgar::insider::parse(
            include_bytes!("../../crates/edgar/tests/fixtures/form4_exercise_withholding.xml"),
            320193,
        )
        .unwrap()
        .transactions;
        assert_eq!(
            row_text(&exercise, false),
            ("Khan S. (COO) · Option exercise 99,878 · Tax withholding 52,854".into(), "—".into())
        );
        assert_eq!(direction("P"), theme::up());
        assert_eq!(direction("S"), theme::down());
        for code in ["A", "M", "F", "G", "C", "J"] {
            assert_eq!(direction(code), theme::text());
        }
        let eight = Filing { form: "8-K/A".into(), items: "2.02,9.01".into(), ..Default::default() };
        assert_eq!(row_text(&eight, false).0, "Results of operations · Financial statements");
        let mut empty = f.clone();
        empty.insiders.clear();
        empty.insider_unavailable = true;
        assert_eq!(row_text(&empty, false).0, "details unavailable");
        empty.insider_unavailable = false;
        empty.insider_parsed = true;
        assert_eq!(row_text(&empty, false).0, "No reported transactions");
        exercise.insiders.retain(|t| t.derivative);
        assert!(row_text(&exercise, false).0.contains("Option exercise 99,878 · derivative"));
    }
    #[test]
    fn date_search_open_market_filters_and_empty_causes_are_distinct() {
        let now = chrono::Utc::now().timestamp();
        let f = insider_fixture();
        let watched: Vec<String> = vec!["AAPL US Equity".into()];
        let mut s = Settings::new(Some(watched[0].clone()));
        s.form = Form::Insider;
        s.buys_sells_only = true;
        for search in ["newstead", "AAPL", "4"] {
            s.search = search.into();
            assert!(s.matches_at(&f, &watched, now));
        }
        s.search = "missing".into();
        assert!(!s.matches_at(&f, &watched, now));
        s.search.clear();
        let mut old = f.clone();
        old.filed = now - 31 * 86400;
        assert!(!s.matches_at(&old, &watched, now));
        s.days = 90;
        assert!(s.matches_at(&old, &watched, now));
        let mut neutral = f.clone();
        neutral.insiders[0].code = "F".into();
        assert!(!s.matches_at(&neutral, &watched, now));
        let state = State::default();
        assert_eq!(empty_cause(&s, &state, &[]), EmptyCause::NotTracked);
        assert_eq!(empty_cause(&s, &state, &watched), EmptyCause::Importing);
        let state = State { status: "Offline (test instance)".into(), ..Default::default() };
        assert_eq!(empty_cause(&s, &state, &watched), EmptyCause::Offline);
        let state = State { rows: Arc::new(vec![f]), updated: now, ..Default::default() };
        assert_eq!(empty_cause(&s, &state, &watched), EmptyCause::Filtered);
    }
    #[test]
    fn summary_sums_open_market_only_and_cluster_requires_distinct_recent_owners() {
        let now =
            chrono::NaiveDate::from_ymd_opt(2026, 10, 27).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        let mut sale = insider_fixture();
        sale.insiders[0].code = "P".into();
        let mut second = sale.clone();
        second.accession = "another".into();
        second.insiders[0].owner.name = "Other Owner".into();
        second.insiders[0].owner.cik = Some(42);
        let rows = vec![sale.clone(), second.clone()];
        let s = summary(&rows, "AAPL US Equity", now);
        assert_eq!(s.buys, 2);
        assert_eq!(s.bought, 2. * 2399. * 345.);
        assert_eq!(s.sells, 0);
        assert!(s.cluster);
        second.insiders[0].owner = sale.insiders[0].owner.clone();
        assert!(!summary(&[sale.clone(), second.clone()], "AAPL", now).cluster);
        second.insiders[0].owner.cik = Some(42);
        second.insiders[0].date = "2026-08-01".into();
        assert!(!summary(&[sale.clone(), second.clone()], "AAPL", now).cluster);
        second.insiders[0].date = "2026-10-13".into();
        second.insiders[0].derivative = true;
        assert_eq!(summary(&[sale.clone(), second.clone()], "AAPL", now).buys, 2);
        second.insiders[0].code = "M".into();
        assert_eq!(summary(&[sale, second], "AAPL", now).buys, 1);
    }
    #[test]
    fn keyboard_navigation_is_stable_by_id_and_clamped() {
        let ids = ["new", "old"];
        assert_eq!(move_selection(None, &ids, true).as_deref(), Some("new"));
        assert_eq!(move_selection(Some("new"), &ids, true).as_deref(), Some("old"));
        assert_eq!(move_selection(Some("old"), &ids, true).as_deref(), Some("old"));
        assert_eq!(move_selection(Some("new"), &ids, false).as_deref(), Some("new"));
        assert_eq!(move_selection(Some("old"), &[], true), None);
        for key in [
            egui::Key::J,
            egui::Key::K,
            egui::Key::Enter,
            egui::Key::Space,
            egui::Key::C,
            egui::Key::Slash,
            egui::Key::Escape,
            egui::Key::U,
        ] {
            assert!(owns_key(key));
        }
        assert!(!owns_key(egui::Key::A));
    }
    #[test]
    fn focused_keyboard_opens_expands_charts_and_respects_text_focus() {
        let ctx = egui::Context::default();
        let mut settings = Settings::new(Some("AAPL US Equity".into()));
        let first = insider_fixture();
        let mut second = first.clone();
        second.accession = "second".into();
        second.filed -= 1;
        let state = State { rows: Arc::new(vec![first, second]), ..Default::default() };
        let watched: Vec<String> = vec!["AAPL US Equity".into()];
        let mut render = |key: Option<egui::Key>, focused: bool| {
            let mut actions = vec![];
            let mut runtime = Runtime::default();
            let events = key
                .map(|key| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                })
                .into_iter()
                .collect();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600., 900.))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    actions = show(
                        ui,
                        &mut settings,
                        &state,
                        &watched,
                        PaneContext { focused, link: Some(crate::layouts::Link::Orange), watchlist_name: "Main" },
                    );
                    runtime = ui.data_mut(|d| d.get_temp::<Runtime>(ui.id().with("filings-runtime"))).unwrap();
                },
            );
            output.textures_delta.clear();
            (output, actions, runtime)
        };
        render(None, true);
        assert_eq!(render(Some(egui::Key::J), false).2.selected, None);
        assert_eq!(render(Some(egui::Key::J), true).2.selected.as_deref(), Some("sale"));
        assert!(render(Some(egui::Key::Space), true).2.expanded.contains("sale"));
        assert_eq!(
            render(Some(egui::Key::C), true).1,
            vec![crate::congress_view::Action::LinkedSymbol(crate::layouts::Link::Orange, "AAPL US Equity".into())]
        );
        assert!(
            render(Some(egui::Key::Enter), true)
                .0
                .platform_output
                .commands
                .iter()
                .any(|c| matches!(c, egui::OutputCommand::OpenUrl(_)))
        );
        assert_eq!(render(Some(egui::Key::J), true).2.selected.as_deref(), Some("second"));
        render(Some(egui::Key::Slash), true);
        assert_eq!(render(Some(egui::Key::K), true).2.selected.as_deref(), Some("second"));
    }
    #[test]
    fn filters_scope_amendments_and_only_watched_symbols() {
        let mut s = Settings::new(Some("AAPL US Equity".into()));
        let mut f = Filing { ticker: "AAPL".into(), form: "8-K/A".into(), ..Default::default() };
        let watched = vec!["AAPL US Equity".into(), "MSFT US Equity".into()];
        assert!(s.matches(&f, &watched));
        assert!(!s.matches(&f, &[]));
        f.ticker = "MSFT".into();
        assert!(!s.matches(&f, &watched));
        s.scope = Scope::Watchlist;
        assert!(s.matches(&f, &watched));
        for (filter, form) in [
            (Form::Reports, "10-Q/A"),
            (Form::Insider, "4"),
            (Form::Ownership, "SC 13G/A"),
            (Form::Offerings, "424B5"),
            (Form::Proxy, "DEF 14A"),
        ] {
            assert!(filter.matches(form));
            assert!(!filter.matches("8-K"));
        }
    }
    #[test]
    fn layout_defaults_and_roundtrip_preserve_legacy_news() {
        let old: crate::layouts::PaneSettings = serde_json::from_str(r#"{"News":"AAPL US Equity"}"#).unwrap();
        assert!(matches!(old, crate::layouts::PaneSettings::News(_)));
        let p: crate::layouts::PaneSettings = serde_json::from_str(r#"{"Filings":{}}"#).unwrap();
        assert_eq!(p, crate::layouts::PaneSettings::Filings(Settings::default()));
        let settings = Settings {
            symbol: Some("AAPL US Equity".into()),
            scope: Scope::Symbol,
            form: Form::Current,
            ..Settings::default()
        };
        let p = crate::layouts::PaneSettings::Filings(settings);
        assert_eq!(
            serde_json::from_slice::<crate::layouts::PaneSettings>(&serde_json::to_vec(&p).unwrap()).unwrap(),
            p
        );
    }
    #[test]
    fn commands_resolve_cf_filings_and_symbol_function() {
        use crate::command::{Command, Function, parse};
        for command in ["CF", "FILINGS"] {
            assert_eq!(parse(command), Some(Command::Function(Function::Filings)));
        }
        assert_eq!(parse("AAPL CF"), Some(Command::SymbolFunction("AAPL US Equity".into(), Function::Filings)));
    }
    #[test]
    fn idle_render_does_not_schedule_repaints() {
        let ctx = egui::Context::default();
        let mut settings = Settings::default();
        let state = State {
            status: "Offline (test instance)".into(),
            rows: Arc::new(vec![insider_fixture()]),
            ..Default::default()
        };
        let mut delay = std::time::Duration::ZERO;
        for frame in 0..4 {
            let mut output = ctx.run_ui(egui::RawInput { time: Some(frame as f64), ..Default::default() }, |ui| {
                show(
                    ui,
                    &mut settings,
                    &state,
                    &["AAPL US Equity".into()],
                    PaneContext { focused: false, link: None, watchlist_name: "All watchlists" },
                );
            });
            delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
            output.textures_delta.clear();
        }
        assert_eq!(delay, std::time::Duration::MAX);
    }
    #[test]
    fn review_share_classes_of_one_cik_both_see_filings() {
        // GOOG and GOOGL are one registrant (CIK 1652044); scheduler::watched keeps only the last ticker.
        let companies = vec![
            tr_edgar::Company { cik: 1652044, ticker: "GOOG".into(), title: "Alphabet".into() },
            tr_edgar::Company { cik: 1652044, ticker: "GOOGL".into(), title: "Alphabet".into() },
        ];
        let watched_symbols: Vec<String> = vec!["GOOG US Equity".into(), "GOOGL US Equity".into()];
        let map = tr_edgar::watched(&companies, &watched_symbols);
        let filing = Filing { cik: 1652044, ticker: map[&1652044].clone(), form: "8-K".into(), ..Default::default() };
        // Match the production boundary: readers attach all share classes from the persisted CIK map.
        let mut store = tr_edgar::store::Store::memory().unwrap();
        store.replace_companies(&companies).unwrap();
        store.upsert(&[filing]).unwrap();
        let filing = store.filings().unwrap().remove(0);
        for symbol in ["GOOG US Equity", "GOOGL US Equity"] {
            let settings = Settings::new(Some(symbol.into()));
            assert!(settings.matches(&filing, &watched_symbols), "{symbol} Symbol scope misses its issuer's 8-K");
            assert_eq!(settings.display_ticker(&filing, &watched_symbols), tr_edgar::ticker(symbol).unwrap());
            assert_eq!(store.chart_filings(symbol, 0, 1).unwrap().len(), 1);
            assert_eq!(
                crate::chart_events::filings(std::slice::from_ref(&filing), symbol, 0, 1).len(),
                1,
                "{symbol} chart"
            );
        }
    }
    #[test]
    fn reporting_owner_entries_stay_out_of_the_default_filings_pane() {
        let settings = Settings::new(Some("AAPL US Equity".into()));
        let mut filing = Filing { ticker: "AAPL".into(), form: "4".into(), ..Default::default() };
        let watched: Vec<String> = vec!["AAPL US Equity".into()];
        assert!(settings.matches(&filing, &watched));
        filing.reporting_owner = true;
        assert!(!settings.matches(&filing, &watched));
    }
    #[test]
    fn external_links_are_http_only() {
        assert!(safe_url("https://www.sec.gov/Archives/filing.htm"));
        for bad in ["javascript:alert(1)", "file:///tmp/filing", "/relative", "https://user:pass@sec.gov/"] {
            assert!(!safe_url(bad));
        }
    }
}
