//! Company profile (DES): the company behind one symbol, from Finnhub
//! `/stock/profile2` through the shared request guard.

use std::time::{Duration, Instant};

use egui::{RichText, Ui};
use tr_data::finnhub::CompanyProfile;

use crate::congress_view::Action;
use crate::theme::{self, amber, mono, muted, ui_font};
use crate::worker::{PaneId, Request, Worker};

/// What the pane shows for its symbol, and why when there is no fresh profile.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum CompanyState {
    /// The one request for this symbol is on its way.
    #[default]
    Loading,
    /// `CompanyPane::profile` is inside its TTL.
    Ready,
    /// Settings › Data sources has no Finnhub key.
    NoKey,
    /// External data sources are off.
    SourcesOff,
    /// Finnhub covers US equities only.
    NotEquity,
    /// Finnhub has no profile for this symbol.
    Missing,
    /// The request was refused or failed; a stored profile stays on screen, stale.
    Failed(String),
}

/// One symbol's company profile. The worker answers once per symbol and a refusal
/// becomes a state, never a retry loop.
pub struct CompanyPane {
    pub symbol: String,
    /// The profile on screen with the time it was fetched, kept when a refresh fails.
    pub profile: Option<(CompanyProfile, i64)>,
    pub state: CompanyState,
    /// The symbol the one request was made for.
    requested: Option<String>,
    settled_since: Instant,
    /// Set by "Open Data sources"; the pane owner switches settings tabs.
    pub open_data_sources: bool,
}

impl CompanyPane {
    pub fn new(symbol: &str) -> Self {
        Self {
            symbol: symbol.into(),
            profile: None,
            state: CompanyState::Loading,
            requested: None,
            settled_since: Instant::now(),
            open_data_sources: false,
        }
    }

    /// Visible panes ask once after the symbol settles, or schedule the remaining wait.
    pub fn request(&mut self, pane: PaneId, worker: &Worker, now: Instant) -> Option<Duration> {
        if self.requested.as_deref() == Some(self.symbol.as_str()) {
            return None;
        }
        let remaining = Duration::from_millis(400).saturating_sub(now.duration_since(self.settled_since));
        if !remaining.is_zero() {
            return Some(remaining);
        }
        self.requested = Some(self.symbol.clone());
        self.state = CompanyState::Loading;
        worker.send(Request::CompanyProfile { pane, symbol: self.symbol.clone() });
        None
    }

    pub fn retry(&mut self) {
        self.requested = None;
        self.state = CompanyState::Loading;
    }

    /// Old data kept upright after a failed refresh.
    pub fn stale(&self) -> bool {
        self.profile.is_some() && self.state != CompanyState::Ready
    }
}

/// Finnhub reports market cap in millions of the listing currency: 3.12T USD.
fn market_cap(millions: f64, currency: &str) -> String {
    match compact(millions) {
        Some(value) if currency.is_empty() => value,
        Some(value) => format!("{value} {currency}"),
        None => "—".into(),
    }
}

/// Millions as a short count (15.5B), or a dash when there is no number.
fn shares_out(millions: f64) -> String {
    compact(millions).unwrap_or_else(|| "—".into())
}

fn compact(millions: f64) -> Option<String> {
    let (value, suffix) = match millions {
        m if m >= 1e6 => (m / 1e6, "T"),
        m if m >= 1e3 => (m / 1e3, "B"),
        m if m > 0.0 => (m, "M"),
        _ => return None,
    };
    let digits = format!("{value:.2}");
    Some(format!("{}{suffix}", digits.trim_end_matches('0').trim_end_matches('.')))
}

/// Finnhub's website field is free text; only real web links open.
fn website(url: &str) -> Option<String> {
    if !crate::filings_view::safe_url(url) {
        return None;
    }
    reqwest::Url::parse(url).ok().map(|url| url.to_string())
}

fn shown(value: &str) -> &str {
    if value.trim().is_empty() { "—" } else { value }
}

fn header(ui: &mut Ui, symbol: &str, profile: &CompanyProfile, stale: bool) {
    ui.horizontal_wrapped(|ui| {
        ui.add(egui::Label::new(RichText::new(shown(&profile.name)).font(ui_font(16.0)).strong()).wrap());
        ui.label(RichText::new(shown_ticker(symbol, profile)).font(mono(12.0)).color(amber()));
        if stale {
            ui.label(RichText::new("stale").font(mono(10.5)).color(muted()));
        }
    });
}

fn shown_ticker(symbol: &str, profile: &CompanyProfile) -> String {
    if profile.ticker.is_empty() {
        crate::security_names::ticker(&tr_data::security(symbol), false)
    } else {
        profile.ticker.clone()
    }
}

fn value(ui: &mut Ui, r: &theme::AnalysisRoles, text: &str) {
    ui.label(RichText::new(text).font(mono(12.0)).color(r.text));
}

fn inspector(ui: &mut Ui, profile: &CompanyProfile, r: &theme::AnalysisRoles) {
    use super::story_view::inspector_row;
    inspector_row(ui, "Exchange", r, |ui| value(ui, r, shown(&profile.exchange)));
    inspector_row(ui, "Industry", r, |ui| {
        ui.label(RichText::new(shown(&profile.industry)).font(ui_font(12.0)).color(r.text));
    });
    inspector_row(ui, "Country", r, |ui| value(ui, r, shown(&profile.country)));
    inspector_row(ui, "Market cap", r, |ui| value(ui, r, &market_cap(profile.market_cap, &profile.currency)));
    inspector_row(ui, "Shares out", r, |ui| value(ui, r, &shares_out(profile.shares_out)));
    inspector_row(ui, "IPO", r, |ui| value(ui, r, shown(&profile.ipo)));
    inspector_row(ui, "Website", r, |ui| match website(&profile.weburl) {
        Some(url) => {
            let host = reqwest::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_owned));
            ui.horizontal(|ui| {
                if let Some(host) = host {
                    ui.label(RichText::new(host.trim_start_matches("www.")).font(mono(12.0)).color(r.ticker));
                }
                if ui.button("Open").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
            });
        }
        None => {
            ui.label(RichText::new("—").font(mono(12.0)).color(r.secondary));
        }
    });
}

/// The company profile for the pane's symbol: header, inspector rows and the state copy.
pub(super) fn show(ui: &mut Ui, id: PaneId, pane: &mut CompanyPane) -> Vec<Action> {
    let mut actions = Vec::new();
    let roles = theme::analysis_roles(&theme::palette());
    egui::ScrollArea::vertical().id_salt(("company", id)).auto_shrink([false, false]).show(ui, |ui| {
        if let Some((profile, _)) = &pane.profile {
            header(ui, &pane.symbol, profile, pane.stale());
        }
        match &pane.state {
            CompanyState::Ready => {
                if let Some((_, fetched_at)) = &pane.profile
                    && chrono::Utc::now().timestamp().saturating_sub(*fetched_at) > 86400
                    && let Some(date) = chrono::DateTime::from_timestamp(*fetched_at, 0)
                {
                    ui.label(
                        RichText::new(format!("Profile from {}", date.format("%Y-%m-%d")))
                            .font(mono(10.5))
                            .color(muted()),
                    );
                }
            }
            CompanyState::Loading => {
                ui.label(RichText::new("Loading…").color(muted()));
            }
            CompanyState::NoKey => {
                ui.label(RichText::new("Company profile needs a Finnhub key.").color(muted()));
                if ui.small_button("Open Data sources").clicked() {
                    pane.open_data_sources = true;
                    actions.push(Action::Command("SETTINGS".into()));
                }
            }
            CompanyState::SourcesOff => {
                ui.label(RichText::new("External sources are off.").color(muted()));
                if ui.small_button("Open Data sources").clicked() {
                    pane.open_data_sources = true;
                    actions.push(Action::Command("SETTINGS".into()));
                }
            }
            CompanyState::NotEquity => {
                ui.label(RichText::new("Company profiles cover US equities only.").color(muted()));
            }
            CompanyState::Missing => {
                ui.label(RichText::new("No profile found.").color(muted()));
            }
            CompanyState::Failed(error) => {
                ui.label(RichText::new(format!("Could not load the profile · {error}")).color(muted()));
                if ui.small_button("Retry").clicked() {
                    pane.retry();
                    ui.ctx().request_repaint();
                }
            }
        }
        if let Some((profile, _)) = &pane.profile {
            inspector(ui, profile, &roles);
        }
    });
    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> CompanyProfile {
        CompanyProfile {
            name: "Apple Inc.".into(),
            ticker: "AAPL".into(),
            exchange: "NASDAQ NMS - GLOBAL MARKET".into(),
            currency: "USD".into(),
            country: "US".into(),
            industry: "Technology".into(),
            ipo: "1980-12-12".into(),
            weburl: "https://www.apple.com".into(),
            market_cap: 3_120_000.0,
            shares_out: 15_500.0,
        }
    }

    fn text_of(pane: &mut CompanyPane) -> Vec<String> {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 4000.0))),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            show(ui, 1, pane);
        });
        let mut text = vec![];
        for shape in &out.shapes {
            rendered_text(&shape.shape, &mut text);
        }
        out.textures_delta.clear();
        text.into_iter().map(|(text, _)| text).collect()
    }

    fn rendered_text(shape: &egui::epaint::Shape, text: &mut Vec<(String, egui::Pos2)>) {
        match shape {
            egui::epaint::Shape::Text(s) => {
                text.push((s.galley.text().to_owned(), s.pos + s.galley.rect.center().to_vec2()))
            }
            egui::epaint::Shape::Vec(shapes) => {
                for s in shapes {
                    rendered_text(s, text);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn market_cap_is_compact_in_millions_of_the_currency() {
        assert_eq!(market_cap(3_120_000.0, "USD"), "3.12T USD");
        assert_eq!(market_cap(2_500_000.0, "EUR"), "2.5T EUR");
        assert_eq!(market_cap(45_200.0, "USD"), "45.2B USD");
        assert_eq!(market_cap(812.0, "USD"), "812M USD");
        assert_eq!(market_cap(1_234.5, ""), "1.23B");
        for empty in [0.0, -1.0, f64::NAN] {
            assert_eq!(market_cap(empty, "USD"), "—");
        }
        assert_eq!(shares_out(15_500.0), "15.5B");
        assert_eq!(shares_out(62.4), "62.4M");
        assert_eq!(shares_out(0.0), "—");
    }

    #[test]
    fn website_only_reaches_http_links() {
        assert_eq!(website("https://www.apple.com").as_deref(), Some("https://www.apple.com/"));
        assert_eq!(website(" http://example.test/ir ").as_deref(), Some("http://example.test/ir"));
        for rejected in [
            "",
            "   ",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
            "ftp://example.test",
            "https://user:pass@evil.test/",
            "https:example.test",
            "https:/example.test",
            "https://example.test/\npath",
            "https://example.test/\tpath",
        ] {
            assert_eq!(website(rejected), None, "{rejected:?}");
        }
        // The row shows no action at all when the field is not a web link.
        let mut pane = CompanyPane::new("AAPL US Equity");
        pane.profile = Some((CompanyProfile { weburl: "javascript:alert(1)".into(), ..profile() }, 0));
        pane.state = CompanyState::Ready;
        let text = text_of(&mut pane);
        assert!(!text.iter().any(|t| t == "Open"), "{text:?}");
    }

    #[test]
    fn each_state_shows_its_own_copy() {
        let mut pane = CompanyPane::new("AAPL US Equity");
        assert_eq!(pane.state, CompanyState::Loading);
        assert!(text_of(&mut pane).iter().any(|t| t.contains("Loading")));
        pane.state = CompanyState::NoKey;
        let text = text_of(&mut pane);
        assert!(text.iter().any(|t| t.contains("Company profile needs a Finnhub key.")), "{text:?}");
        assert!(text.iter().any(|t| t.contains("Open Data sources")), "{text:?}");
        pane.state = CompanyState::SourcesOff;
        let text = text_of(&mut pane);
        assert!(text.iter().any(|t| t.contains("External sources are off.")), "{text:?}");
        assert!(text.iter().any(|t| t.contains("Open Data sources")), "{text:?}");
        pane.state = CompanyState::NotEquity;
        let text = text_of(&mut pane);
        assert!(text.iter().any(|t| t.contains("Company profiles cover US equities only.")), "{text:?}");
        pane.state = CompanyState::Missing;
        let text = text_of(&mut pane);
        assert!(text.iter().any(|t| t.contains("No profile found.")), "{text:?}");
        pane.state = CompanyState::Failed("Rate limited · Request budget unavailable".into());
        let text = text_of(&mut pane);
        assert!(text.iter().any(|t| t.contains("Could not load the profile · Rate limited")), "{text:?}");
    }

    #[test]
    fn a_failed_refresh_keeps_the_stale_profile_on_screen() {
        let mut pane = CompanyPane::new("AAPL US Equity");
        pane.profile = Some((profile(), 1_793_016_000));
        pane.state = CompanyState::Failed("HTTP 500".into());
        assert!(pane.stale());
        let text = text_of(&mut pane);
        assert!(text.iter().any(|t| t.contains("Could not load the profile · HTTP 500")), "{text:?}");
        assert!(text.iter().any(|t| t.contains("stale")), "{text:?}");
        assert!(text.iter().any(|t| t.contains("Apple Inc.")), "{text:?}");
        assert!(text.iter().any(|t| t.contains("3.12T USD")), "{text:?}");
        for label in ["Exchange", "Industry", "Country", "Market cap", "Shares out", "IPO", "Website"] {
            assert!(text.iter().any(|t| t == label), "{label} missing from {text:?}");
        }
        pane.state = CompanyState::Ready;
        assert!(!pane.stale());
        let text = text_of(&mut pane);
        assert!(!text.iter().any(|t| t.contains("stale")), "{text:?}");
        assert!(text.iter().any(|t| t.contains("Apple Inc.")), "{text:?}");
        assert!(text.iter().any(|t| t.contains("AAPL")), "{text:?}");
    }

    #[test]
    fn visible_requests_debounce_and_failures_wait_for_retry() {
        let (worker, mut requests) = Worker::recording();
        let mut pane = CompanyPane::new("AAPL US Equity");
        let start = pane.settled_since;
        assert_eq!(pane.request(1, &worker, start), Some(Duration::from_millis(400)));
        assert_eq!(pane.request(1, &worker, start + Duration::from_millis(399)), Some(Duration::from_millis(1)));
        assert!(requests.try_recv().is_err());
        assert_eq!(pane.request(1, &worker, start + Duration::from_millis(400)), None);
        assert!(
            matches!(requests.try_recv(), Ok(Request::CompanyProfile { pane: 1, symbol }) if symbol == pane.symbol)
        );
        pane.state = CompanyState::Failed("HTTP 500".into());
        pane.profile = Some((profile(), 0));
        assert_eq!(pane.request(1, &worker, start + Duration::from_secs(60)), None);
        assert!(requests.try_recv().is_err());
        pane.retry();
        assert_eq!(pane.request(1, &worker, start + Duration::from_secs(60)), None);
        assert!(matches!(requests.try_recv(), Ok(Request::CompanyProfile { pane: 1, .. })));
        assert_eq!(pane.state, CompanyState::Loading);
        assert!(pane.profile.is_some(), "retry keeps the stored profile on screen");
        pane.request(1, &worker, start + Duration::from_secs(61));
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn retry_button_clears_the_request_without_discarding_the_profile() {
        let mut pane = CompanyPane::new("AAPL US Equity");
        pane.requested = Some(pane.symbol.clone());
        pane.profile = Some((profile(), 0));
        pane.state = CompanyState::Failed("HTTP 500".into());
        let ctx = egui::Context::default();
        let frame = |pane: &mut CompanyPane, events| {
            let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
                show(ui, 1, pane);
            });
            output.textures_delta.clear();
            output
        };
        let output = frame(&mut pane, vec![]);
        let mut text = Vec::new();
        for shape in &output.shapes {
            rendered_text(&shape.shape, &mut text);
        }
        let position = text.iter().find(|(s, _)| s == "Retry").unwrap().1;
        let click = |pressed| egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&mut pane, vec![egui::Event::PointerMoved(position), click(true)]);
        frame(&mut pane, vec![click(false)]);
        assert_eq!(pane.requested, None);
        assert_eq!(pane.state, CompanyState::Loading);
        assert!(pane.profile.is_some());
    }

    #[test]
    fn ready_profiles_show_the_fetch_date_only_when_older_than_a_day() {
        let mut pane = CompanyPane::new("AAPL US Equity");
        pane.profile = Some((profile(), 0));
        pane.state = CompanyState::Ready;
        let text = text_of(&mut pane);
        assert!(text.iter().any(|t| t == "Profile from 1970-01-01"), "{text:?}");
        pane.profile = Some((profile(), chrono::Utc::now().timestamp()));
        let text = text_of(&mut pane);
        assert!(!text.iter().any(|t| t.starts_with("Profile from")), "{text:?}");
    }

    #[test]
    fn review_g7_failed_state_offers_a_retry() {
        let mut pane = CompanyPane::new("AAPL US Equity");
        pane.state = CompanyState::Failed("Rate limited · Source busy (spacing wait exceeded)".into());
        let text = text_of(&mut pane);
        assert!(text.iter().any(|t| t.contains("Retry")), "{text:?}");
    }

    #[test]
    fn review_g7_website_opens_the_parsed_url_not_the_raw_text() {
        // WHATWG parsing reads the backslash as a path separator, so the validated (and
        // displayed) host is evil.test; the raw text goes to `open` unnormalised.
        assert_eq!(website(r"https://evil.test\@apple.com").as_deref(), Some("https://evil.test/@apple.com"));
    }
}
