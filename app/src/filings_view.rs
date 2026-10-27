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
            Self::Reports => "10-Q/10-K",
            Self::Insider => "Insider 3/4/5",
            Self::Ownership => "Ownership 13D/G",
            Self::Offerings => "Offerings S-1/424B",
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub symbol: Option<String>,
    pub scope: Scope,
    pub form: Form,
}
impl Settings {
    pub fn new(symbol: Option<String>) -> Self {
        Self { scope: if symbol.is_some() { Scope::Symbol } else { Scope::Watchlist }, symbol, form: Form::All }
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
pub fn show(ui: &mut Ui, settings: &mut Settings, state: &State, watched: &[String]) {
    ui.spacing_mut().item_spacing = egui::vec2(dp(6.), dp(3.));
    ui.horizontal(|ui| {
        ui.selectable_value(&mut settings.scope, Scope::Symbol, "Symbol");
        ui.selectable_value(&mut settings.scope, Scope::Watchlist, "Watchlist");
        if let Some(symbol) = &settings.symbol {
            ui.label(RichText::new(symbol).font(theme::mono(11.)).color(theme::amber()));
        }
    });
    ui.horizontal_wrapped(|ui| {
        for form in Form::ALL {
            ui.selectable_value(&mut settings.form, form, form.label());
        }
    });
    let status = if state.status.starts_with("SEC EDGAR · updated") {
        let suffix = state.status.split_once("m ago").map(|(_, s)| s).unwrap_or("");
        format!("SEC EDGAR · updated {}m ago{suffix}", (chrono::Utc::now().timestamp() - state.updated).max(0) / 60)
    } else if state.status.is_empty() {
        "SEC EDGAR · waiting for worker".into()
    } else {
        state.status.clone()
    };
    ui.label(RichText::new(status).font(theme::mono(10.)).color(theme::muted()));
    ui.separator();
    let mut rows: Vec<_> = state.rows.iter().filter(|f| settings.matches(f, watched)).collect();
    rows.sort_by_key(|f| (std::cmp::Reverse(f.time()), std::cmp::Reverse(&f.accession)));
    if rows.is_empty() {
        ui.label(RichText::new("No matching filings. Only symbols on your watchlists (any list) are tracked; add a US equity to a watchlist to start. First import covers the last 30 days; subsequent sweeps retain 90 days.").font(theme::ui_font(11.)).color(theme::muted()));
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, dp(24.), rows.len(), |ui, range| {
        for i in range {
            let f = rows[i];
            let filed = f.time_label();
            let display = settings.display_ticker(f, watched);
            ui.horizontal(|ui| {
                ui.label(RichText::new(filed).font(theme::mono(10.)).color(theme::muted()));
                ui.label(RichText::new(&display).font(theme::mono(11.)).color(theme::amber()));
                let detail = f.detail();
                let response = ui.add(
                    egui::Button::new(RichText::new(&detail).font(theme::mono(11.)).color(theme::text())).frame(false),
                );
                let link = f.link();
                if response.clicked() && safe_url(&link) {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&link));
                }
                response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(format!("{detail}\n{}\n{link}", f.accession));
            });
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;
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
        let settings = Settings { symbol: Some("AAPL US Equity".into()), scope: Scope::Symbol, form: Form::Current };
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
        let state = State { status: "Offline (test instance)".into(), ..Default::default() };
        let mut delay = std::time::Duration::ZERO;
        for frame in 0..4 {
            let mut output = ctx.run_ui(egui::RawInput { time: Some(frame as f64), ..Default::default() }, |ui| {
                show(ui, &mut settings, &state, &[]);
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
        let watched = vec!["AAPL US Equity".into()];
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
