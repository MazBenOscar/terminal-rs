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
        let family = tr_edgar::form_family(form);
        let f = family.as_str();
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
    #[cfg(test)]
    pub fn matches_at(&self, filing: &Filing, watched: &[String], now: i64) -> bool {
        self.matches_cached(filing, watched, now, &haystack(filing), &self.search.trim().to_ascii_lowercase())
    }
    fn matches_cached(&self, filing: &Filing, watched: &[String], now: i64, text: &str, query: &str) -> bool {
        self.matches(filing, watched)
            && filing.time()
                >= now
                    - i64::from(if filing.transient_symbol.is_empty() { self.days } else { self.days.min(30) }) * 86400
            && (!self.buys_sells_only
                || !Form::Insider.matches(&filing.form)
                || filing.insiders.iter().any(|t| {
                    matches!(t.code.as_str(), "P" | "S")
                        && match self.scope {
                            Scope::Symbol => self.symbol.as_deref().is_some_and(|s| t.matches_symbol(filing, s)),
                            Scope::Watchlist => watched.iter().any(|s| t.matches_symbol(filing, s)),
                        }
                }))
            && (query.is_empty() || text.contains(query))
    }
    pub fn display_ticker(&self, filing: &Filing, watched: &[String]) -> String {
        let display = if self.scope == Scope::Symbol {
            self.symbol.as_deref().filter(|s| filing.matches_symbol(s)).and_then(tr_edgar::ticker)
        } else {
            watched
                .iter()
                .filter(|s| {
                    filing.matches_symbol(s)
                        && (!self.buys_sells_only
                            || !Form::Insider.matches(&filing.form)
                            || filing
                                .insiders
                                .iter()
                                .any(|t| matches!(t.code.as_str(), "P" | "S") && t.matches_symbol(filing, s)))
                })
                .filter_map(|s| tr_edgar::ticker(s))
                .min()
        };
        display.unwrap_or_else(|| filing.ticker.clone())
    }
    pub fn matches(&self, filing: &Filing, watched: &[String]) -> bool {
        !filing.reporting_owner
            && if filing.transient_symbol.is_empty() {
                watched.iter().any(|s| filing.matches_symbol(s))
            } else {
                self.scope == Scope::Symbol && self.symbol.as_deref().is_some_and(|s| filing.matches_symbol(s))
            }
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
    pub flags: tr_edgar::scheduler::StatusFlags,
}
pub fn safe_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
    })
}
fn haystack(f: &Filing) -> String {
    format!(
        "{} {} {} {} {}",
        f.ticker,
        f.tickers.join(" "),
        f.form,
        f.detail(),
        f.insiders
            .iter()
            .map(|t| format!("{} {} {}", t.owner.name, t.security_title, t.issuer_symbol))
            .collect::<Vec<_>>()
            .join(" ")
    )
    .to_ascii_lowercase()
}
#[derive(Clone, Default)]
struct Runtime {
    cache_source: Option<Arc<Vec<Filing>>>,
    haystacks: Vec<String>,
    row_models: std::collections::BTreeMap<String, (String, String)>,
    unread: usize,
    cache_read_len: usize,
    cache_settings: Option<Settings>,
    cache_watched: Vec<String>,
    cache_minute: i64,
    filtered: Vec<usize>,
    summary: Summary,
    #[cfg(test)]
    cache_builds: usize,
    selected: Option<String>,
    expanded: std::collections::BTreeSet<String>,
    read: std::collections::BTreeSet<String>,
    selected_at: Option<std::time::Instant>,
    paused_rows: Option<Arc<Vec<Filing>>>,
    ids: Vec<String>,
    heights: std::collections::BTreeMap<String, f32>,
    anchor: Option<(String, f32)>,
}
impl Runtime {
    fn update(&mut self, settings: &Settings, state: &State, watched: &[String], now: i64) -> Arc<Vec<Filing>> {
        if settings.paused {
            if self.paused_rows.is_none() {
                self.paused_rows = Some(state.rows.clone());
            }
        } else {
            self.paused_rows = None;
        }
        let stored = self.paused_rows.clone().unwrap_or_else(|| state.rows.clone());
        let changed = self.cache_source.as_ref().is_none_or(|source| !Arc::ptr_eq(source, &stored));
        if changed {
            self.haystacks = stored.iter().map(haystack).collect();
            self.cache_source = Some(stored.clone());
        }
        let rebuild = changed
            || self.cache_settings.as_ref() != Some(settings)
            || self.cache_watched != watched
            || self.cache_minute != now / 60;
        if rebuild {
            let query = settings.search.trim().to_ascii_lowercase();
            self.filtered = stored
                .iter()
                .enumerate()
                .filter(|(index, f)| settings.matches_cached(f, watched, now, &self.haystacks[*index], &query))
                .map(|(index, _)| index)
                .collect();
            self.filtered
                .sort_by_key(|i| (std::cmp::Reverse(stored[*i].time()), std::cmp::Reverse(&stored[*i].accession)));
            let included: std::collections::BTreeSet<_> =
                self.filtered.iter().map(|i| stored[*i].accession.as_str()).collect();
            self.summary = summarize(&stored, settings.symbol.as_deref().unwrap_or(""), now, settings.days, |f| {
                included.contains(f.accession.as_str())
            });
            self.row_models = stored
                .iter()
                .map(|f| {
                    (
                        f.accession.clone(),
                        row_text_for(f, settings.buys_sells_only, Some(&settings.display_ticker(f, watched))),
                    )
                })
                .collect();
            self.cache_settings = Some(settings.clone());
            self.cache_watched = watched.to_vec();
            self.cache_minute = now / 60;
            #[cfg(test)]
            {
                self.cache_builds += 1;
            }
        }
        if rebuild || self.cache_read_len != self.read.len() {
            self.unread = self.filtered.iter().filter(|i| !self.read.contains(&stored[**i].accession)).count();
            self.cache_read_len = self.read.len();
        }
        stored
    }
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
        && !state
            .rows
            .iter()
            .any(|f| !f.transient_symbol.is_empty() && settings.symbol.as_deref().is_some_and(|s| f.matches_symbol(s)))
    {
        return EmptyCause::NotTracked;
    }
    if state.flags.unavailable {
        return EmptyCause::Offline;
    }
    let has_scope = state.rows.iter().any(|f| {
        !f.reporting_owner
            && watched.iter().any(|w| f.matches_symbol(w))
            && (settings.scope == Scope::Watchlist || settings.symbol.as_deref().is_some_and(|s| f.matches_symbol(s)))
    });
    if (state.rows.is_empty() && state.updated == 0) || (!has_scope && state.flags.importing) {
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
#[derive(Clone, Default, Debug, PartialEq)]
pub struct Summary {
    pub buys: usize,
    pub sells: usize,
    pub bought: f64,
    pub sold: f64,
    pub cluster: bool,
}
#[cfg(test)]
pub fn summary(rows: &[Filing], symbol: &str, now: i64) -> Summary {
    summarize(rows, symbol, now, 90, |_| true)
}
fn summarize(rows: &[Filing], symbol: &str, now: i64, days: u16, include: impl Fn(&Filing) -> bool) -> Summary {
    use std::collections::BTreeSet;
    let mut s = Summary::default();
    let mut buys = BTreeSet::new();
    let mut sells = BTreeSet::new();
    let mut purchases = vec![];
    for (f, t) in tr_edgar::insider::effective_lots(rows, symbol) {
        if !include(f) {
            continue;
        }
        let Some(date) = chrono::NaiveDate::parse_from_str(&t.date, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|d| d.and_utc().timestamp())
        else {
            continue;
        };
        if date < now - i64::from(days) * 86400 || date > now {
            continue;
        }
        let amount = t.shares.zip(t.price).map(|(a, b)| a * b).unwrap_or(0.);
        match t.code.as_str() {
            "P" => {
                buys.insert(&f.accession);
                s.bought += amount;
                // Buyer identities are gathered separately below, before joint-lot deduplication.
            }
            "S" => {
                sells.insert(&f.accession);
                s.sold += amount;
            }
            _ => {}
        }
    }
    s.buys = buys.len();
    s.sells = sells.len();
    for (f, t) in tr_edgar::insider::effective_transactions(rows, symbol) {
        if !include(f)
            || t.code != "P"
            || t.derivative
            || t.acquired == Some(false)
            || !t.shares.is_some_and(|v| v.is_finite() && v > 0.)
            || !t.price.is_none_or(|v| v.is_finite() && v > 0.)
        {
            continue;
        }
        let Some(date) = chrono::NaiveDate::parse_from_str(&t.date, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|d| d.and_utc().timestamp())
        else {
            continue;
        };
        if (now - i64::from(days) * 86400..=now).contains(&date) {
            purchases.push((date, f.cik, f.accession.as_str(), tr_edgar::insider::owner_id(&t.owner)));
        }
    }
    purchases.sort();
    // Any rolling 30-day span within the shown window, with the same shared
    // distinct-accession/beneficial-owner rule as worker alerts.
    let endings: BTreeSet<_> = purchases.iter().map(|(date, cik, _, _)| (*date, *cik)).collect();
    for (date, cik) in &endings {
        if tr_edgar::distinct_buyers(
            purchases
                .iter()
                .filter(|(d, c, _, _)| c == cik && *d <= *date && *d >= date - 30 * 86400)
                .map(|(_, _, accession, owner)| (*accession, owner.clone())),
        ) {
            s.cluster = true;
            break;
        }
    }
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
#[derive(Clone, Copy, Default)]
struct KeyboardUse {
    rows: bool,
    selected: bool,
}
/// Test hook: pretend the pane drew rows without a selection.
#[cfg(test)]
pub fn mark_unselected_for_test(ctx: &egui::Context, id: egui::Id) {
    ctx.data_mut(|d| d.insert_temp(id.with("filings-keys"), KeyboardUse { rows: true, selected: false }));
}
/// Test hook: pretend the pane drew rows with a selected row.
#[cfg(test)]
pub fn mark_selected_for_test(ctx: &egui::Context, id: egui::Id) {
    ctx.data_mut(|d| d.insert_temp(id.with("filings-keys"), KeyboardUse { rows: true, selected: true }));
}
pub fn pane_uses_key(ctx: &egui::Context, id: egui::Id, settings: &Settings, key: egui::Key) -> bool {
    let used = ctx.data_mut(|d| d.get_temp::<KeyboardUse>(id.with("filings-keys"))).unwrap_or_default();
    match key {
        egui::Key::ArrowUp | egui::Key::ArrowDown => used.rows,
        egui::Key::Enter | egui::Key::Space | egui::Key::C | egui::Key::J | egui::Key::K | egui::Key::U => {
            used.selected
        }
        egui::Key::Escape => used.selected || !settings.search.is_empty(),
        _ => owns_key(key),
    }
}
fn consume_exact(ui: &Ui, key: egui::Key) -> bool {
    ui.input_mut(|i| {
        let mut consumed = false;
        i.events.retain(|e| {
            let take = matches!(e, egui::Event::Key { key: k, pressed: true, modifiers, .. }
                if *k == key && *modifiers == egui::Modifiers::NONE);
            consumed |= take;
            !take
        });
        consumed
    })
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
#[cfg(test)]
pub fn row_text(f: &Filing, only_open_market: bool) -> (String, String) {
    row_text_for(f, only_open_market, None)
}
fn row_text_for(f: &Filing, only_open_market: bool, symbol: Option<&str>) -> (String, String) {
    use tr_edgar::insider::{aggregate, value};
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
                "No reported transactions".into()
            } else if f.description.is_empty() || f.description.eq_ignore_ascii_case("FORM 4") {
                "Form 4 · Statement of changes in ownership".into()
            } else {
                format!("Form 4 · {}", f.description)
            },
            String::new(),
        );
    }
    let has_common = f.insiders.iter().any(|t| !t.derivative);
    let lots: Vec<_> = f
        .insiders
        .iter()
        .filter(|t| {
            symbol.is_none_or(|s| t.matches_symbol(f, s))
                && (!t.derivative || !has_common || matches!(t.code.as_str(), "P" | "S"))
                && (!only_open_market || matches!(t.code.as_str(), "P" | "S"))
        })
        .cloned()
        .collect();
    let groups = aggregate(&lots);
    let mut text = vec![];
    let mut previous = None;
    let mut total = Some(0.);
    let mut has_value = false;
    let mut seen = std::collections::BTreeSet::new();
    for t in lots.iter().filter(|t| matches!(t.code.as_str(), "P" | "S")) {
        if seen.insert(tr_edgar::insider::lot_key(t)) {
            has_value = true;
            total = total.zip(t.shares.zip(t.price)).map(|(a, (shares, price))| a + shares * price);
        }
    }
    for g in &groups {
        if previous != Some(&g.owner) {
            let role = g.owner.role();
            text.push(format!(
                "{}{}",
                g.owner.display_name(),
                if role.is_empty() { String::new() } else { format!(" ({role})") }
            ));
            previous = Some(&g.owner);
        }
        text.push(g.text());
    }
    if !has_common {
        text.push("derivative".into());
    }
    if lots.iter().any(|t| t.plan_10b5_1) {
        text.push("10b5-1".into());
    }
    (text.join(" · "), total.filter(|_| has_value).map(value).unwrap_or_else(|| "—".into()))
}
fn detail_state(f: &Filing, state: &State, paused: bool, now: i64) -> String {
    if !matches!(f.form.as_str(), "4" | "4/A") || f.insider_parsed || !f.insiders.is_empty() {
        return String::new();
    }
    if f.insider_unavailable {
        return "details unavailable".into();
    }
    if let Some(until) = state.flags.paused_until {
        return chrono::DateTime::from_timestamp(until, 0)
            .map(|d| format!("paused until {}", d.with_timezone(&chrono::Local).format("%H:%M")))
            .unwrap_or_else(|| "paused".into());
    }
    if state.flags.unavailable {
        return "offline".into();
    }
    if paused {
        return "paused".into();
    }
    let active = state
        .interest
        .as_ref()
        .is_some_and(|(active, symbols)| *active && symbols.iter().any(|symbol| f.matches_symbol(symbol)));
    if active
        && state.flags.details_active
        && !state.flags.budget_deferred
        && f.insider_queued
        && f.insider_retry <= now
    {
        "reading"
    } else {
        "waiting"
    }
    .into()
}

/// Prose is one UI-font run; transaction figures, including their spaces and @,
/// are one mono run. Never switch fonts at every whitespace boundary.
fn row_job(text: &str, base: egui::Color32, insider: bool) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    for (i, part) in text.split(" · ").enumerate() {
        if i > 0 {
            job.append(
                " · ",
                dp(0.),
                egui::TextFormat { font_id: theme::ui_font(12.), color: base, ..Default::default() },
            );
        }
        let action = insider
            .then(|| {
                ["P", "S", "A", "M", "F", "G", "C", "J"].into_iter().find_map(|code| {
                    let word = tr_edgar::insider::action(code).0;
                    part.strip_prefix(word).filter(|tail| tail.starts_with(' ')).map(|tail| (word, code, tail))
                })
            })
            .flatten();
        if let Some((word, code, figures)) = action {
            job.append(
                word,
                dp(0.),
                egui::TextFormat {
                    font_id: theme::ui_font(12.),
                    color: if matches!(code, "P" | "S") { direction(code) } else { base },
                    ..Default::default()
                },
            );
            job.append(
                figures,
                dp(0.),
                egui::TextFormat { font_id: theme::mono(12.), color: base, ..Default::default() },
            );
        } else {
            job.append(
                part,
                dp(0.),
                egui::TextFormat { font_id: theme::ui_font(12.), color: base, ..Default::default() },
            );
        }
    }
    job
}

fn hover_card(response: &egui::Response, f: &Filing, text: &str, state_tag: &str, full_time: &str) {
    crate::feed_time::tooltip(response).show(|ui| {
        ui.spacing_mut().item_spacing.y = dp(4.);
        let owners = f
            .insiders
            .iter()
            .map(|t| t.owner.name.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        ui.add(
            egui::Label::new(
                RichText::new(if owners.is_empty() { text } else { &owners })
                    .font(theme::ui_font(13.5))
                    .color(theme::text())
                    .strong(),
            )
            .wrap(),
        );
        if !owners.is_empty() {
            ui.add(egui::Label::new(row_job(text, theme::text(), true)).wrap());
        }
        ui.add(
            egui::Label::new(
                RichText::new(format!("{} · {} · {}", f.form, full_time, f.accession))
                    .font(theme::mono(10.5))
                    .color(theme::muted()),
            )
            .wrap(),
        );
        if !state_tag.is_empty() {
            ui.label(RichText::new(state_tag).font(theme::ui_font(10.5)).color(theme::muted()));
        }
        for t in &f.insiders {
            let lot = format!(
                "{} · {} · {} {} @ {} · owned {}",
                t.owner.name,
                t.date,
                tr_edgar::insider::action(&t.code).0,
                t.shares.map(tr_edgar::insider::number).unwrap_or_else(|| "—".into()),
                t.price.map(|p| format!("{p:.2}")).unwrap_or_else(|| "—".into()),
                t.owned_after.map(tr_edgar::insider::number).unwrap_or_else(|| "—".into())
            );
            ui.add(egui::Label::new(row_job(&lot, theme::muted(), true)).wrap());
        }
        if let Some(host) = reqwest::Url::parse(&f.link()).ok().and_then(|u| u.host_str().map(str::to_owned)) {
            ui.add_space(dp(2.));
            ui.label(
                RichText::new(format!("{} · click to open", host.trim_start_matches("www.")))
                    .font(theme::ui_font(10.5))
                    .color(theme::muted()),
            );
        }
    });
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
    pub keyboard_id: egui::Id,
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
fn untracked_actions(ui: &mut Ui, settings: &mut Settings, actions: &mut Vec<crate::congress_view::Action>) {
    ui.horizontal(|ui| {
        if ui.button("Add to watchlist").clicked()
            && let Some(s) = &settings.symbol
        {
            actions.push(crate::congress_view::Action::AddToWatchlist(s.clone()));
        }
        if ui
            .button("Load once")
            .on_hover_text(
                "Last 30 days, without tracking or insider document reads. At most once per company per 24 hours.",
            )
            .clicked()
            && let Some(s) = &settings.symbol
        {
            settings.days = 30;
            actions.push(crate::congress_view::Action::LoadFilingsOnce(s.clone()));
        }
    });
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
    // egui temp reads clone their value: store an Arc, not the growing Runtime collections.
    let runtime_handle =
        ui.data_mut(|d| d.get_temp::<Arc<std::sync::Mutex<Runtime>>>(runtime_id)).unwrap_or_else(|| {
            let handle = Arc::new(std::sync::Mutex::new(Runtime::default()));
            ui.data_mut(|d| d.insert_temp(runtime_id, handle.clone()));
            handle
        });
    let mut runtime_guard = runtime_handle.lock().expect("filings runtime");
    let runtime = &mut *runtime_guard;
    runtime.update(settings, state, watched, now);
    let mut focus_search = false;
    ui.spacing_mut().item_spacing = egui::vec2(dp(6.), dp(3.));
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
    ui.horizontal(|ui| {
        ui.style_mut().override_font_id = Some(theme::ui_font(10.5));
        ui.spacing_mut().button_padding = egui::vec2(dp(4.), dp(2.));
        ui.spacing_mut().item_spacing.x = dp(2.);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), dp(20.)),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let more = crate::feed_time::icon_button(ui, crate::widgets::Icon::More, "Filings options");
                egui::Popup::menu(&more).show(|ui| {
                    ui.add(egui::Label::new(&status).wrap());
                    ui.separator();
                    if settings.scope == Scope::Symbol
                        && settings
                            .symbol
                            .as_ref()
                            .is_some_and(|s| !watched.iter().any(|w| tr_edgar::ticker(w) == tr_edgar::ticker(s)))
                    {
                        untracked_actions(ui, settings, &mut actions);
                    }
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
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), dp(20.)),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.selectable_value(&mut settings.scope, Scope::Symbol, "Symbol");
                        ui.selectable_value(&mut settings.scope, Scope::Watchlist, "Watchlist");
                        let symbol = if settings.scope == Scope::Symbol {
                            settings.symbol.as_deref().and_then(tr_edgar::ticker).unwrap_or_default()
                        } else {
                            context.watchlist_name.into()
                        };
                        let label_width = ui
                            .painter()
                            .layout_no_wrap(symbol.clone(), theme::mono(11.), theme::amber())
                            .size()
                            .x
                            .min(dp(100.))
                            .min(ui.available_width() * 0.55);
                        ui.add_sized(
                            egui::vec2(label_width, dp(20.)),
                            egui::Label::new(RichText::new(&symbol).font(theme::mono(11.)).color(theme::amber()))
                                .truncate(),
                        )
                        .on_hover_text(symbol);
                        if let Some(link) = context.link.filter(|_| settings.scope == Scope::Symbol) {
                            let color = match link {
                                crate::layouts::Link::Red => theme::down(),
                                crate::layouts::Link::Orange => theme::amber(),
                                crate::layouts::Link::Green => theme::up(),
                                crate::layouts::Link::Blue => theme::accent(),
                                crate::layouts::Link::Purple => theme::series(4),
                            };
                            crate::feed_time::dot(ui, color).on_hover_text(format!("{} link group", link.label()));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if settings.paused
                                && let Some(saved) = &runtime.paused_rows
                            {
                                let old: std::collections::BTreeSet<_> =
                                    saved.iter().map(|f| f.accession.as_str()).collect();
                                let new = state.rows.iter().filter(|f| !old.contains(f.accession.as_str())).count();
                                if new > 0 && ui.button(format!("{new} new · Show")).clicked() {
                                    settings.paused = false;
                                    runtime.paused_rows = None;
                                }
                            }
                            ui.add(
                                egui::Label::new(
                                    RichText::new(crate::feed_time::compact_status(&status))
                                        .font(theme::ui_font(10.5))
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
    // Fold before controls would clip, including when larger theme fonts need more room.
    let chips_width = Form::ALL
        .iter()
        .map(|f| ui.painter().layout_no_wrap(f.label().into(), theme::ui_font(10.5), theme::text()).size().x + dp(14.))
        .sum::<f32>();
    let narrow = ui.available_width() < dp(360.).max(chips_width + dp(230.));
    let search_id = ui.id().with("filings-search");
    // Keyboard input is processed before the search field, so '/' can focus it this frame.
    if context.focused && !text_has_focus(ui.ctx()) {
        focus_search = consume_exact(ui, egui::Key::Slash);
    }
    ui.horizontal(|ui| {
        ui.style_mut().override_font_id = Some(theme::ui_font(10.5));
        ui.spacing_mut().button_padding = egui::vec2(dp(4.), dp(2.));
        ui.spacing_mut().item_spacing.x = dp(3.);
        for days in [7, 30, 90] {
            ui.selectable_value(&mut settings.days, days, format!("{days}D"));
        }
        ui.toggle_value(&mut settings.unread_only, format!("Unread {}", runtime.unread))
            .on_hover_text("Unread only (u)");
        if !narrow {
            for form in Form::ALL {
                ui.selectable_value(&mut settings.form, form, form.label());
            }
        }
        if narrow {
            let response = crate::feed_time::icon_button(ui, crate::widgets::Icon::Search, "Search (/)");
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
    let stored = runtime.update(settings, state, watched, now);
    if settings.scope == Scope::Symbol && settings.form == Form::Insider {
        let s = &runtime.summary;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Insiders {}D", settings.days)).font(theme::mono(10.)).color(theme::muted()),
            );
            ui.label(
                RichText::new(format!(
                    "· {} {} {}",
                    s.sells,
                    if s.sells == 1 { "sale" } else { "sales" },
                    tr_edgar::insider::value(s.sold)
                ))
                .font(theme::mono(10.))
                .color(if s.sells == 0 { theme::muted() } else { theme::down() }),
            );
            ui.label(
                RichText::new(format!(
                    "· {} {} {}",
                    s.buys,
                    if s.buys == 1 { "buy" } else { "buys" },
                    tr_edgar::insider::value(s.bought)
                ))
                .font(theme::mono(10.))
                .color(if s.buys == 0 { theme::muted() } else { theme::up() }),
            );
            if s.cluster {
                ui.label(RichText::new("Cluster buy").color(theme::amber()));
            }
        });
    }
    let rows: Vec<_> = runtime
        .filtered
        .iter()
        .map(|i| &stored[*i])
        .filter(|f| !settings.unread_only || !runtime.read.contains(&f.accession))
        .collect();
    if runtime.selected.as_ref().is_some_and(|id| !rows.iter().any(|f| &f.accession == id)) {
        runtime.selected = None;
    }
    let has_rows = !rows.is_empty();
    let mut scroll_selected = false;
    if context.focused && !text_has_focus(ui.ctx()) {
        let pressed = |key| consume_exact(ui, key);
        // Letters act only while a row is selected, so type-anywhere keeps them otherwise.
        let selected = runtime.selected.is_some();
        let down = (!rows.is_empty() && pressed(egui::Key::ArrowDown)) || (selected && pressed(egui::Key::J));
        let up = (!rows.is_empty() && pressed(egui::Key::ArrowUp)) || (selected && pressed(egui::Key::K));
        if down || up {
            runtime.selected = move_selection(
                runtime.selected.as_deref(),
                &rows.iter().map(|f| f.accession.as_str()).collect::<Vec<_>>(),
                down,
            );
            runtime.selected_at = Some(std::time::Instant::now());
            scroll_selected = true;
        }
        if (!settings.search.is_empty() || runtime.selected.is_some()) && pressed(egui::Key::Escape) {
            if !settings.search.is_empty() {
                settings.search.clear();
            } else {
                runtime.selected = None;
            }
        }
        if selected && pressed(egui::Key::U) {
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
        && !settings.search.is_empty()
        && consume_exact(ui, egui::Key::Escape)
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
        let ticker = if settings.scope == Scope::Watchlist {
            context.watchlist_name.to_owned()
        } else {
            settings.symbol.as_deref().and_then(tr_edgar::ticker).unwrap_or_else(|| "this symbol".into())
        };
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
                if settings.form == Form::All {
                    format!("No filings for {ticker} match these filters.")
                } else {
                    format!("No {} for {ticker} in the last {} days.", settings.form.label(), settings.days)
                }
            }
            EmptyCause::NoHistory => format!("No stored filings for {ticker} in this window."),
            EmptyCause::Offline => "Stored filings retained while updates are unavailable.".into(),
        };
        ui.label(RichText::new(text).font(theme::ui_font(11.)).color(theme::muted()));
        if cause == EmptyCause::NotTracked {
            untracked_actions(ui, settings, &mut actions);
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
                    if selected { theme::accent_wash() } else { theme::control_hover() },
                );
            }
            if selected && context.focused {
                ui.painter().rect_stroke(
                    rect.shrink(dp(0.5)),
                    dp(2.),
                    egui::Stroke::new(dp(1.), theme::accent()),
                    egui::StrokeKind::Inside,
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
            let (text, value) = &runtime.row_models[&f.accession];
            let state_tag = detail_state(f, state, settings.paused, now);
            let time_width = ui
                .painter()
                .layout_no_wrap("0".repeat(crate::feed_time::WIDTH_CHARS), theme::mono(10.), theme::muted())
                .size()
                .x;
            let ticker_width = ui.painter().layout_no_wrap("000000".into(), theme::mono(11.), theme::amber()).size().x;
            let form_width = ui.painter().layout_no_wrap("00000".into(), theme::mono(10.), theme::muted()).size().x;
            let time_right = rect.left() + dp(10.) + time_width;
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
            ui.interact(time_rect, ui.id().with((&f.accession, "time")), egui::Sense::hover())
                .on_hover_text(&full_time);
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
            let budget = crate::feed_time::tail_budget(rect.width(), rect.right() - main_left);
            let tail = if state_tag.is_empty() { value.as_str() } else { state_tag.as_str() };
            let tail_font = if state_tag.is_empty() { theme::mono(10.) } else { theme::ui_font(10.5) };
            let measured = ui.painter().layout_no_wrap(tail.into(), tail_font.clone(), theme::muted()).size().x;
            let cap = ui.painter().layout_no_wrap("0".repeat(9), theme::mono(10.), theme::muted()).size().x;
            let value_width = if state_tag.is_empty() { measured.min(cap) } else { measured }.min(budget);
            let value_gap = if value_width > 0. { dp(6.) } else { 0. };
            let main_rect = egui::Rect::from_min_max(
                egui::pos2(main_left, rect.top()),
                egui::pos2((rect.right() - value_width - value_gap).max(main_left), rect.bottom()),
            );
            let base = if runtime.read.contains(&f.accession) || (!state_tag.is_empty() && f.insiders.is_empty()) {
                theme::muted()
            } else {
                theme::text()
            };
            let mut job = row_job(text, base, matches!(f.form.as_str(), "4" | "4/A"));
            job.wrap.max_width = main_rect.width();
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
            ui.painter().with_clip_rect(main_rect).galley(
                egui::pos2(main_left, rect.center().y - galley.size().y / 2.),
                galley,
                theme::text(),
            );
            if value_width > 0. {
                let tail_rect = egui::Rect::from_min_max(egui::pos2(rect.right() - value_width, rect.top()), rect.max);
                let mut tail_job = egui::text::LayoutJob::simple_singleline(tail.into(), tail_font, theme::muted());
                tail_job.wrap.max_width = value_width;
                tail_job.wrap.max_rows = 1;
                let galley = ui.painter().layout_job(tail_job);
                ui.painter().with_clip_rect(tail_rect).galley(
                    egui::pos2(rect.right() - galley.size().x, rect.center().y - galley.size().y / 2.),
                    galley,
                    theme::muted(),
                );
            }
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
            hover_card(&title, f, text, &state_tag, &full_time);
            if runtime.expanded.contains(&f.accession) {
                ui.horizontal(|ui| {
                    ui.add_space(main_left - rect.left());
                    ui.vertical(|ui| {
                        egui::ScrollArea::vertical().id_salt((&f.accession, "lots")).max_height(dp(66.)).show(
                            ui,
                            |ui| {
                                if f.insiders.is_empty() {
                                    ui.label(RichText::new(text).font(theme::ui_font(11.)).color(theme::muted()));
                                }
                                for t in f
                                    .insiders
                                    .iter()
                                    .filter(|t| t.matches_symbol(f, &settings.display_ticker(f, watched)))
                                {
                                    let date = chrono::NaiveDate::parse_from_str(&t.date, "%Y-%m-%d")
                                        .ok()
                                        .and_then(|d| d.and_hms_opt(0, 0, 0))
                                        .map(|d| crate::feed_time::feed_time(d.and_utc().timestamp(), now, true).0)
                                        .unwrap_or_else(|| "—".into());
                                    ui.label(
                                        RichText::new(format!(
                                            "{date} · {} · {} {} @ {} · owned {}{}",
                                            t.owner.display_name(),
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
    ui.ctx().data_mut(|d| {
        d.insert_temp(
            context.keyboard_id.with("filings-keys"),
            KeyboardUse { rows: has_rows, selected: runtime.selected.is_some() },
        )
    });
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
    fn form4_tags_only_report_reading_for_an_active_queued_accession() {
        let mut f = Filing { ticker: "AAPL".into(), form: "4".into(), insider_queued: true, ..Default::default() };
        let mut state = State {
            interest: Some((true, vec!["AAPL US Equity".into()])),
            flags: tr_edgar::scheduler::StatusFlags { details_active: true, ..Default::default() },
            ..Default::default()
        };
        assert_eq!(detail_state(&f, &state, false, 100), "reading");
        f.insider_queued = false;
        assert_eq!(detail_state(&f, &state, false, 100), "waiting");
        f.insider_queued = true;
        f.insider_retry = 101;
        assert_eq!(detail_state(&f, &state, false, 100), "waiting");
        f.insider_retry = 0;
        assert_eq!(detail_state(&f, &state, true, 100), "paused");
        state.flags.unavailable = true;
        assert_eq!(detail_state(&f, &state, false, 100), "offline");
        state.flags.paused_until = Some(200);
        let local = chrono::DateTime::from_timestamp(200, 0).unwrap().with_timezone(&chrono::Local);
        assert_eq!(detail_state(&f, &state, false, 100), format!("paused until {}", local.format("%H:%M")));
        f.insider_unavailable = true;
        assert_eq!(detail_state(&f, &state, false, 100), "details unavailable");
        f.insider_parsed = true;
        assert!(detail_state(&f, &state, false, 100).is_empty());
    }

    #[test]
    fn owner_prose_and_directional_actions_use_ui_font_figures_use_mono() {
        let text = "Newstead J. (SVP, GC) · Sold 2,399 @ 345.00 · 10b5-1";
        let job = row_job(text, theme::text(), true);
        assert_eq!(job.text, text);
        let section = |s: &str| {
            job.sections
                .iter()
                .find(|part| job.text[part.byte_range.start.0..part.byte_range.end.0].starts_with(s))
                .unwrap()
        };
        assert_eq!(section("Newstead J. (SVP, GC)").format.font_id.family, egui::FontFamily::Proportional);
        assert_eq!(section("Sold").format.color, theme::down());
        assert_eq!(section(" 2,399 @ 345.00").format.font_id.family, egui::FontFamily::Monospace);
        let job = row_job("Owner · Bought 10 @ 20.00", theme::text(), true);
        assert!(
            job.sections
                .iter()
                .any(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0] == "Bought"
                    && s.format.color == theme::up())
        );
    }

    #[test]
    fn one_shot_rows_are_untracked_symbol_only_and_unread_is_ready_on_first_frame() {
        let now = chrono::Utc::now().timestamp();
        let row = Filing {
            accession: "once".into(),
            ticker: "AAPL".into(),
            form: "8-K".into(),
            filed: now,
            transient_symbol: "AAPL".into(),
            ..Default::default()
        };
        let state = State { rows: Arc::new(vec![row.clone()]), ..Default::default() };
        let mut settings = Settings::new(Some("AAPL US Equity".into()));
        assert!(settings.matches_at(&row, &[], now));
        assert_ne!(empty_cause(&settings, &state, &[]), EmptyCause::NotTracked);
        let mut runtime = Runtime::default();
        runtime.update(&settings, &state, &[], now);
        assert_eq!(runtime.unread, 1);
        runtime.read.insert("once".into());
        runtime.update(&settings, &state, &[], now);
        assert_eq!(runtime.unread, 0);
        assert_eq!(runtime.cache_builds, 1);
        settings.scope = Scope::Watchlist;
        assert!(!settings.matches_at(&row, &["AAPL US Equity".into()], now));
        settings.scope = Scope::Symbol;
        settings.symbol = Some("MSFT US Equity".into());
        assert!(!settings.matches_at(&row, &[], now));
        settings.symbol = Some("AAPL US Equity".into());
        settings.days = 90;
        assert!(!settings.matches_at(&Filing { filed: now - 31 * 86400, ..row }, &[], now));
    }

    #[test]
    fn narrow_header_preserves_overflow_unread_and_separated_row_columns() {
        for scope in [Scope::Symbol, Scope::Watchlist] {
            let ctx = egui::Context::default();
            let row = Filing {
                accession: "current".into(),
                ticker: "AAPL".into(),
                form: "8-K".into(),
                filed: chrono::Utc::now().timestamp(),
                items: "2.02,9.01".into(),
                ..Default::default()
            };
            let state = State {
                rows: Arc::new(vec![row]),
                status: "SEC EDGAR · updated now · Reading insider forms: 216 left".into(),
                ..Default::default()
            };
            let mut settings = Settings::new(Some("AAPL US Equity".into()));
            settings.scope = scope;
            let mut measured = 0.;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(340., 400.))),
                    ..Default::default()
                },
                |ui| {
                    show(
                        ui,
                        &mut settings,
                        &state,
                        &["AAPL US Equity".into()],
                        PaneContext {
                            focused: true,
                            keyboard_id: egui::Id::new("test"),
                            link: None,
                            watchlist_name: "Retirement with a long name",
                        },
                    );
                    measured = ui.min_rect().width();
                },
            );
            assert!(measured <= 341., "header must not grow the pane: {measured}");
            let text: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some(t),
                    _ => None,
                })
                .collect();
            assert!(text.iter().any(|t| t.galley.job.text == "Unread 1" && t.pos.y >= dp(20.)));
            let form = text.iter().find(|t| t.galley.job.text == "8-K").unwrap();
            let main = text.iter().find(|t| t.galley.job.text.starts_with("Results of operations")).unwrap();
            assert!(main.pos.x >= form.pos.x + form.galley.size().x + dp(6.));
            if scope == Scope::Watchlist {
                let ticker = text.iter().find(|t| t.galley.job.text == "AAPL").unwrap();
                assert!(form.pos.x >= ticker.pos.x + ticker.galley.size().x + dp(6.));
            }
            let dots = output
                .shapes
                .iter()
                .filter(|s| {
                    matches!(&s.shape, egui::Shape::Circle(c)
                if c.center.x > 310. && c.center.x < 340. && c.center.y < dp(25.))
                })
                .count();
            assert_eq!(dots, 3);
            output.textures_delta.clear();
        }
    }

    #[test]
    fn form4_hover_card_has_solid_edges_full_owner_and_transaction_lots() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| {
            s.interaction.tooltip_delay = 0.;
            s.interaction.tooltip_grace_time = 0.;
        });
        let filing = insider_fixture();
        let text = row_text(&filing, false).0;
        let mut found_lot = false;
        let mut found_edge = false;
        for frame in 0..4 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600., 400.))),
                    time: Some(frame as f64),
                    events: vec![egui::Event::PointerMoved(egui::pos2(50., 10.))],
                    ..Default::default()
                },
                |ui| {
                    let (_, response) = ui.allocate_exact_size(egui::vec2(200., 22.), egui::Sense::hover());
                    hover_card(&response, &filing, &text, "", "27 Oct 2026 12:00 UTC");
                },
            );
            found_lot |= output.shapes.iter().any(|s| {
                matches!(&s.shape, egui::Shape::Text(t)
                if t.galley.job.text.contains("Sold 2,399 @ 345.00 · owned"))
            });
            fn solid_edge(shape: &egui::Shape) -> bool {
                match shape {
                    egui::Shape::Vec(shapes) => shapes.iter().any(solid_edge),
                    egui::Shape::Rect(r) => {
                        r.fill == theme::card() && r.stroke.color == theme::muted() && r.stroke.width == dp(1.)
                    }
                    _ => false,
                }
            }
            found_edge |= output.shapes.iter().any(|s| solid_edge(&s.shape));
            output.textures_delta.clear();
        }
        assert!(found_lot, "hover card must include the individual transaction");
        assert!(found_edge, "hover card must have a solid visible edge");
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
        assert!(row_text(&empty, false).0.starts_with("Form 4 · "));
        assert_eq!(detail_state(&empty, &State::default(), false, 0), "details unavailable");
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
        let state = State {
            status: "Offline (test instance)".into(),
            flags: tr_edgar::scheduler::StatusFlags { unavailable: true, ..Default::default() },
            ..Default::default()
        };
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
        sale.insiders[0].acquired = Some(true);
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
                        PaneContext {
                            focused,
                            keyboard_id: egui::Id::new("test"),
                            link: Some(crate::layouts::Link::Orange),
                            watchlist_name: "Main",
                        },
                    );
                    runtime = ui
                        .data_mut(|d| d.get_temp::<Arc<std::sync::Mutex<Runtime>>>(ui.id().with("filings-runtime")))
                        .unwrap()
                        .lock()
                        .unwrap()
                        .clone();
                },
            );
            output.textures_delta.clear();
            (output, actions, runtime)
        };
        render(None, true);
        assert_eq!(render(Some(egui::Key::J), false).2.selected, None);
        // No selection: letters belong to type-anywhere; the arrow key starts selection.
        assert_eq!(render(Some(egui::Key::J), true).2.selected, None);
        assert_eq!(render(Some(egui::Key::ArrowDown), true).2.selected.as_deref(), Some("sale"));
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
            (Form::Ownership, "SCHEDULE 13G"),
            (Form::Ownership, "SCHEDULE 13D/A"),
            (Form::Ownership, "schedule 13g/a"),
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
                    PaneContext {
                        focused: false,
                        keyboard_id: egui::Id::new("test"),
                        link: None,
                        watchlist_name: "All watchlists",
                    },
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
    // ---- Review (N3): failing tests for defects found in production-readiness review ----
    fn review_now() -> i64 {
        chrono::NaiveDate::from_ymd_opt(2026, 10, 27).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp()
    }
    #[test]
    fn review_sale_with_exercise_shows_sale_value() {
        let mut f = insider_fixture();
        let mut exercise = f.insiders[0].clone();
        exercise.code = "M".into();
        exercise.price = None;
        exercise.acquired = Some(true);
        f.insiders.insert(0, exercise);
        let (text, value) = row_text(&f, false);
        assert!(text.contains("Sold 2,399 @ 345.00"), "{text}");
        assert_eq!(value, "$828K");
    }
    #[test]
    fn review_joint_filers_single_purchase_is_not_cluster() {
        let mut buy = insider_fixture();
        buy.insiders[0].code = "P".into();
        let mut joint = buy.insiders[0].clone();
        joint.owner.name = "Newstead Family Trust".into();
        joint.owner.cik = Some(99);
        buy.insiders.push(joint);
        let s = summary(&[buy], "AAPL US Equity", review_now());
        assert_eq!(s.buys, 1);
        assert!(!s.cluster, "one joint filing is one insider purchase, not a cluster");
    }
    #[test]
    fn review_amendment_does_not_double_count_summary() {
        let sale = insider_fixture();
        let mut amendment = sale.clone();
        amendment.accession = "amendment".into();
        amendment.form = "4/A".into();
        let s = summary(&[sale, amendment], "AAPL US Equity", review_now());
        assert_eq!(s.sells, 1);
        assert_eq!(s.sold, 2399. * 345.);
    }
    #[test]
    fn review_budget_deferral_status_is_not_offline_empty_state() {
        let watched: Vec<String> = vec!["AAPL US Equity".into()];
        let mut s = Settings::new(Some(watched[0].clone()));
        s.form = Form::Current;
        let state = State {
            rows: Arc::new(vec![insider_fixture()]),
            updated: review_now(),
            status: "SEC EDGAR · updated · Company refresh deferred to reserve the feed budget".into(),
            ..Default::default()
        };
        assert_eq!(empty_cause(&s, &state, &watched), EmptyCause::Filtered);
    }
    #[test]
    fn review_buys_sells_only_keeps_non_insider_forms() {
        let now = chrono::Utc::now().timestamp();
        let watched: Vec<String> = vec!["AAPL US Equity".into()];
        let mut s = Settings::new(Some(watched[0].clone()));
        s.buys_sells_only = true;
        let eight = Filing {
            ticker: "AAPL".into(),
            form: "8-K".into(),
            items: "2.02".into(),
            filed: now,
            ..Default::default()
        };
        assert!(s.matches_at(&eight, &watched, now));
    }
    #[test]
    fn summary_counts_filings_not_lots_and_finds_older_rolling_clusters() {
        let mut first = insider_fixture();
        first.insiders[0].code = "P".into();
        first.insiders[0].acquired = Some(true);
        first.insiders[0].date = "2026-08-10".into();
        let mut lot = first.insiders[0].clone();
        lot.lot_id = Some(1);
        first.insiders.push(lot);
        let mut second = first.clone();
        second.accession = "second".into();
        second.insiders.truncate(1);
        second.insiders[0].date = "2026-08-25".into();
        second.insiders[0].owner.cik = Some(99);
        let rows = [first, second];
        let s = summary(&rows, "AAPL", review_now());
        assert_eq!(s.buys, 2, "three lots, two filings");
        assert_eq!(s.bought, 3. * 2399. * 345.);
        assert!(s.cluster, "older purchases 15 days apart still cluster inside 90D");
        assert!(!summarize(&rows, "AAPL", review_now(), 30, |_| true).cluster);
    }
    #[test]
    fn share_class_lots_roundtrip_and_filter_rows_summary_and_amended_markers() {
        let mut f = insider_fixture();
        f.tickers = vec!["AAPL".into(), "AAPL-B".into()];
        let mut second = f.insiders[0].clone();
        second.issuer_symbol = "AAPL-B".into();
        second.security_title = "Class B common".into();
        second.shares = Some(10.);
        second.lot_id = Some(1);
        f.insiders.push(second);
        let mut amendment = f.clone();
        amendment.form = "4/A".into();
        amendment.accession = "amendment".into();
        amendment.insiders[0].price = Some(350.);
        let mut store = tr_edgar::store::Store::memory().unwrap();
        store
            .replace_companies(&[
                tr_edgar::Company { cik: f.cik, ticker: "AAPL".into(), title: String::new() },
                tr_edgar::Company { cik: f.cik, ticker: "AAPL-B".into(), title: String::new() },
            ])
            .unwrap();
        for row in [&f, &amendment] {
            store.upsert(std::slice::from_ref(row)).unwrap();
            store
                .save_insiders(
                    row,
                    &tr_edgar::insider::Document {
                        issuer_cik: row.cik,
                        transactions: row.insiders.clone(),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        let rows = store.filings().unwrap();
        assert_eq!(rows[0].insiders, f.insiders);
        assert_eq!(row_text_for(&f, false, Some("AAPL-B")).1, "$3K");
        assert_eq!(summary(&rows, "AAPL-B", review_now()).sold, 3450.);
        assert_eq!(summary(&rows, "AAPL", review_now()).sold, 2399. * 350.);
        for symbol in ["AAPL", "AAPL-B"] {
            let events = crate::chart_events::insiders(&rows, symbol, 0, review_now());
            assert_eq!(events.len(), 1, "amendment must replace its original on {symbol}");
        }
        let mut mixed = f.clone();
        mixed.insiders[0].code = "M".into();
        let watched = vec!["AAPL".into(), "AAPL-B".into()];
        let mut settings = Settings { buys_sells_only: true, ..Settings::new(Some("AAPL".into())) };
        assert!(!settings.matches_at(&mixed, &watched, review_now()), "neutral-only class is hidden");
        settings.symbol = Some("AAPL-B".into());
        assert!(settings.matches_at(&mixed, &watched, review_now()));
        settings.scope = Scope::Watchlist;
        assert!(settings.matches_at(&mixed, &watched, review_now()));
        assert_eq!(settings.display_ticker(&mixed, &watched), "AAPL-B");
        assert!(!settings.matches_at(&mixed, &["AAPL".into()], review_now()));
        f.insiders.truncate(1);
        assert!(!f.matches_symbol("AAPL-B"));
        f.insiders[0].issuer_symbol.clear();
        assert!(f.matches_symbol("AAPL-B"));
        f.insiders[0].issuer_symbol = "UNKNOWN".into();
        assert!(f.matches_symbol("AAPL-B"), "unknown XML symbols fall back to CIK classes");
    }
    #[test]
    fn empty_roles_are_omitted_and_non_ps_insider_forms_only_are_hidden() {
        let mut f = insider_fixture();
        f.insiders[0].owner = tr_edgar::insider::Owner { name: "Smith & Co".into(), ..Default::default() };
        assert!(row_text(&f, false).0.starts_with("Smith & Co · Sold"));
        let s = Settings { buys_sells_only: true, ..Settings::default() };
        let watched = vec!["AAPL".into()];
        for form in ["3", "4", "5", "4/A"] {
            f.form = form.into();
            for code in ["A", "M", "F", "G", "C", "J", "X", "D"] {
                f.insiders[0].code = code.into();
                assert!(!s.matches_at(&f, &watched, chrono::Utc::now().timestamp()));
            }
        }
        for form in ["8-K", "10-K", "SC 13D", "DEF 14A", "S-1"] {
            f.form = form.into();
            assert!(s.matches_at(&f, &watched, chrono::Utc::now().timestamp()));
        }
    }
    #[test]
    fn pane_leaves_unused_escape_enter_arrows_and_shift_keys_in_input() {
        let ctx = egui::Context::default();
        let mut settings = Settings::default();
        let state = State { updated: review_now(), ..Default::default() };
        let events: Vec<_> = [
            egui::Key::Escape,
            egui::Key::Enter,
            egui::Key::ArrowUp,
            egui::Key::ArrowDown,
            egui::Key::J,
            egui::Key::U,
            egui::Key::Slash,
        ]
        .into_iter()
        .map(|key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: if matches!(key, egui::Key::J | egui::Key::U | egui::Key::Slash) {
                egui::Modifiers::SHIFT
            } else {
                egui::Modifiers::NONE
            },
        })
        .collect();
        let mut remaining = vec![];
        let mut output = ctx.run_ui(egui::RawInput { events: events.clone(), ..Default::default() }, |ui| {
            show(
                ui,
                &mut settings,
                &state,
                &[],
                PaneContext { focused: true, keyboard_id: egui::Id::new("test"), link: None, watchlist_name: "Main" },
            );
            remaining = ui.input(|i| i.events.clone());
        });
        output.textures_delta.clear();
        assert_eq!(remaining, events);
        assert!(!settings.unread_only);
    }
    #[test]
    fn paused_startup_rows_and_runtime_cache_survive_idle_frames_and_invalidate_on_filters() {
        let ctx = egui::Context::default();
        let mut settings = Settings { paused: true, ..Settings::new(Some("AAPL".into())) };
        settings.form = Form::Insider;
        let state = State { rows: Arc::new(vec![insider_fixture()]), updated: review_now(), ..Default::default() };
        let mut handles = vec![];
        let mut buffers = vec![];
        for frame in 0..4 {
            if frame == 3 {
                settings.search = "missing".into();
            }
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                show(
                    ui,
                    &mut settings,
                    &state,
                    &["AAPL".into()],
                    PaneContext {
                        focused: false,
                        keyboard_id: egui::Id::new("test"),
                        link: None,
                        watchlist_name: "Main",
                    },
                );
                let handle = ui
                    .data_mut(|d| d.get_temp::<Arc<std::sync::Mutex<Runtime>>>(ui.id().with("filings-runtime")))
                    .unwrap();
                {
                    let runtime = handle.lock().unwrap();
                    assert_eq!(runtime.paused_rows.as_ref().unwrap().len(), 1);
                    assert_eq!(runtime.filtered.len(), usize::from(frame < 3));
                    assert_eq!(runtime.cache_builds, if frame < 3 { 1 } else { 2 });
                    buffers.push(runtime.haystacks[0].as_ptr());
                }
                handles.push(handle);
            });
            output.textures_delta.clear();
        }
        assert!(handles.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])));
        assert!(buffers.windows(2).all(|w| w[0] == w[1]), "haystacks are snapshot-, not frame-keyed");
    }
    #[test]
    fn all_chip_empty_copy_names_symbol_or_watchlist() {
        let ctx = egui::Context::default();
        let mut settings = Settings::new(Some("AAPL".into()));
        settings.search = "nothing".into();
        let state = State { rows: Arc::new(vec![insider_fixture()]), updated: review_now(), ..Default::default() };
        for (scope, expected) in [
            (Scope::Symbol, "No filings for AAPL match these filters."),
            (Scope::Watchlist, "No filings for Retirement match these filters."),
        ] {
            settings.scope = scope;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600., 900.))),
                    ..Default::default()
                },
                |ui| {
                    show(
                        ui,
                        &mut settings,
                        &state,
                        &["AAPL".into()],
                        PaneContext {
                            focused: false,
                            keyboard_id: egui::Id::new("test"),
                            link: None,
                            watchlist_name: "Retirement",
                        },
                    );
                },
            );
            assert!(
                output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.job.text == expected)),
                "missing {expected}"
            );
            output.textures_delta.clear();
        }
    }
    #[test]
    fn external_links_are_http_only() {
        assert!(safe_url("https://www.sec.gov/Archives/filing.htm"));
        for bad in ["javascript:alert(1)", "file:///tmp/filing", "/relative", "https://user:pass@sec.gov/"] {
            assert!(!safe_url(bad));
        }
    }
}
