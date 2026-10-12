//! Brokerage accounts through SnapTrade: credential lifecycle (Keychain / private file,
//! environment overrides), user registration, the read-only Connection Portal, portfolio
//! refresh scheduling and the `PORT`/`ACCT`/`POS`/`ORD`/`ACTV` commands. Read-only: nothing
//! here can place or change orders.

use std::time::{Duration, Instant};

use tr_broker::{Credentials, Portfolio, Position, SecretStore};

use super::{PaneKind, TerminalApp};
use crate::command::PortView;
use crate::worker::{Request, data_dir};

/// Portfolio reload interval while a portfolio pane is visible (SnapTrade caches holdings
/// for most plans, so polling faster gains nothing).
const REFRESH: Duration = Duration::from_secs(120);
/// Faster polling for a few minutes after opening the Connection Portal, so newly connected
/// accounts appear without a manual refresh.
const CONNECT_POLL: Duration = Duration::from_secs(20);
const CONNECT_WINDOW: Duration = Duration::from_secs(300);

/// State of one portfolio pane.
#[derive(Debug, Clone, PartialEq)]
pub struct PortfolioPane {
    pub view: PortView,
    /// Restrict to one account id.
    pub account: Option<String>,
    /// Restrict positions/orders/activity to one market key (`AAPL POS`).
    pub symbol: Option<String>,
    pub sort: PosSort,
    /// Orders: only open ones.
    pub open_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum PosSort {
    #[default]
    Value,
    Pnl,
    PnlPct,
    Symbol,
}

impl PortfolioPane {
    pub fn new(view: PortView) -> Self {
        Self { view, account: None, symbol: None, sort: PosSort::Value, open_only: false }
    }

    pub fn title(&self) -> String {
        match &self.symbol {
            Some(s) => format!("{} {}", tr_data::security(s).ticker, self.view.label()),
            None if self.view == PortView::Summary => "Portfolio".into(),
            None => self.view.label().into(),
        }
    }
}

/// Where the integration stands, for the setup screen and status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerMode {
    /// Offline sample data (`TR_PROVIDER=sample` or `TR_BROKER=sample`) without credentials.
    Simulated,
    /// Keys missing or incomplete: show the setup screen.
    Setup,
    /// Ready to read accounts.
    Live,
}

pub struct BrokerState {
    pub store: SecretStore,
    /// Effective credentials (stored values overridden by `SNAPTRADE_*`).
    pub creds: Credentials,
    /// Some fields come from the environment.
    pub from_env: bool,
    /// What is saved in the store (without environment overrides).
    pub saved: Credentials,
    /// Settings form buffer.
    pub form: Credentials,
    pub reveal: bool,
    pub portfolio: Option<Box<Portfolio>>,
    pub error: Option<String>,
    pub loading: bool,
    pub fetched: Option<Instant>,
    /// Feedback line in Settings › Brokerage: text and whether it is an error.
    pub message: Option<(String, bool)>,
    /// A request in flight from Settings ("Registering…").
    pub busy: Option<&'static str>,
    /// Last Connection Portal link, so it can be copied if no browser opened.
    pub portal_url: Option<String>,
    pub connecting_since: Option<Instant>,
    /// Sample portfolio requested (offline provider or `TR_BROKER=sample`).
    pub simulate: bool,
    pub last_quotes: Option<Instant>,
    /// "Test Connection" is waiting for a portfolio load.
    pub testing: bool,
}

impl BrokerState {
    pub fn load(offline: bool) -> Self {
        let store = SecretStore::platform(&data_dir());
        let mut message = None;
        let stored = match store.load() {
            Ok(c) => c.unwrap_or_default(),
            Err(e) => {
                tracing::warn!("SnapTrade credentials unavailable: {e:#}");
                message = Some((format!("Could not read saved credentials: {e:#}"), true));
                Credentials::default()
            }
        };
        let mut creds = stored.clone();
        let from_env = creds.apply_env(|k| std::env::var(k).ok());
        let simulate = offline || std::env::var("TR_BROKER").is_ok_and(|v| v.eq_ignore_ascii_case("sample"));
        Self {
            store,
            saved: stored,
            form: creds.clone(),
            creds,
            from_env,
            reveal: false,
            portfolio: None,
            error: None,
            loading: false,
            fetched: None,
            message,
            busy: None,
            portal_url: None,
            connecting_since: None,
            simulate,
            last_quotes: None,
            testing: false,
        }
    }

    pub fn mode(&self) -> BrokerMode {
        if self.creds.is_ready() {
            BrokerMode::Live
        } else if self.simulate {
            BrokerMode::Simulated
        } else {
            BrokerMode::Setup
        }
    }

    /// The terminal's own quote for a position, when it maps onto a market key (options are
    /// never repriced from their underlying).
    pub fn live_price(&self, quotes: &std::collections::HashMap<String, tr_data::Quote>, p: &Position) -> Option<f64> {
        if p.kind == tr_broker::InstrumentKind::Option {
            return None;
        }
        quotes.get(&p.market_key()?).map(|q| q.last).filter(|x| x.is_finite() && *x > 0.0)
    }
}

/// Open a URL or path with the system handler.
pub fn open_external(target: &str) -> std::io::Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener).arg(target).spawn().map(|_| ())
}

/// A stable, non-identifying SnapTrade user id for this installation.
pub fn new_user_id() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mix = nanos ^ (u128::from(std::process::id()) << 64);
    format!("terminal-rs-{:016x}", (mix as u64) ^ ((mix >> 64) as u64))
}

impl TerminalApp {
    /// Request a portfolio load now.
    pub(super) fn refresh_portfolio(&mut self) {
        let b = &mut self.broker;
        let creds = match b.mode() {
            BrokerMode::Live => Some(b.creds.clone()),
            BrokerMode::Simulated => None,
            BrokerMode::Setup => return,
        };
        b.loading = true;
        b.fetched = Some(Instant::now());
        self.worker.send(Request::Portfolio(creds));
    }

    /// Periodic portfolio and quote refresh while a portfolio pane is visible.
    pub(super) fn broker_periodic(&mut self, visible: &[usize]) {
        let shown = visible.iter().any(|&i| matches!(self.panes[i].kind, PaneKind::Portfolio(_)));
        if !shown {
            return;
        }
        let b = &self.broker;
        let connecting = b.connecting_since.is_some_and(|t| t.elapsed() < CONNECT_WINDOW);
        let every = if connecting { CONNECT_POLL } else { REFRESH };
        if !b.loading && b.mode() != BrokerMode::Setup && b.fetched.is_none_or(|t| t.elapsed() > every) {
            self.refresh_portfolio();
        }
        // Reprice holdings with the terminal's own quotes.
        if let Some(pf) = &self.broker.portfolio
            && self.broker.last_quotes.is_none_or(|t| t.elapsed() > Duration::from_secs(20))
        {
            let mut keys: Vec<String> = pf
                .positions
                .iter()
                .filter(|p| p.kind != tr_broker::InstrumentKind::Option)
                .filter_map(Position::market_key)
                .collect();
            keys.sort();
            keys.dedup();
            if !keys.is_empty() {
                self.worker.send(Request::Quotes(keys));
            }
            self.broker.last_quotes = Some(Instant::now());
        }
    }

    pub(super) fn on_portfolio(&mut self, result: Result<Box<Portfolio>, String>) {
        let b = &mut self.broker;
        b.loading = false;
        if std::mem::take(&mut b.testing) {
            b.message = Some(match &result {
                Ok(pf) if pf.accounts.is_empty() => {
                    ("Credentials work. No brokerage connected yet: use Connect Brokerage.".into(), false)
                }
                Ok(pf) => (
                    format!(
                        "Connected: {} account{} at {}.",
                        pf.accounts.len(),
                        if pf.accounts.len() == 1 { "" } else { "s" },
                        {
                            let mut inst: Vec<&str> = pf.accounts.iter().map(|a| a.institution.as_str()).collect();
                            inst.sort();
                            inst.dedup();
                            inst.join(", ")
                        }
                    ),
                    false,
                ),
                Err(e) => (format!("Connection failed: {e}"), true),
            });
        }
        match result {
            Ok(pf) => {
                let n = pf.accounts.len();
                // New accounts after connecting: stop the fast polling.
                if b.connecting_since.is_some() && b.portfolio.as_ref().is_some_and(|old| n > old.accounts.len()) {
                    b.connecting_since = None;
                    self.status = format!("Brokerage connected: {n} account{}", if n == 1 { "" } else { "s" });
                }
                b.portfolio = Some(pf);
                b.error = None;
                b.last_quotes = None;
            }
            Err(e) => {
                tracing::warn!("portfolio: {e}");
                b.error = Some(e);
            }
        }
    }

    pub(super) fn on_broker_registered(&mut self, result: Result<(String, String), String>) {
        self.broker.busy = None;
        match result {
            Ok((id, secret)) => {
                self.broker.form.user_id = id;
                self.broker.form.user_secret = secret;
                // SnapTrade returns the secret only once: persist it immediately.
                self.save_broker_form("SnapTrade user registered and saved. Next: Connect Brokerage.");
            }
            Err(e) => self.broker.message = Some((format!("Registration failed: {e}"), true)),
        }
    }

    pub(super) fn on_broker_portal(&mut self, result: Result<String, String>) {
        self.broker.busy = None;
        match result {
            Ok(url) => {
                let opened = open_external(&url);
                self.broker.connecting_since = Some(Instant::now());
                self.broker.message = Some(match opened {
                    Ok(()) => ("Connection Portal opened in your browser (link valid 5 minutes, read-only access). Accounts appear here once connected.".into(), false),
                    Err(e) => (format!("Could not open a browser ({e}); copy the link below."), true),
                });
                self.status = "SnapTrade Connection Portal opened in the browser".into();
                self.broker.portal_url = Some(url);
            }
            Err(e) => self.broker.message = Some((format!("Could not open the Connection Portal: {e}"), true)),
        }
    }

    /// Persist the settings form and make it the effective credentials.
    pub(super) fn save_broker_form(&mut self, ok: &str) {
        let b = &mut self.broker;
        match b.store.save(&b.form) {
            Ok(()) => {
                b.saved = b.form.clone();
                let mut creds = b.form.clone();
                b.from_env = creds.apply_env(|k| std::env::var(k).ok());
                b.creds = creds;
                b.message = Some((ok.into(), false));
                b.portfolio = None;
                b.error = None;
                b.fetched = None;
            }
            Err(e) => b.message = Some((format!("Could not save credentials: {e:#}"), true)),
        }
    }

    pub(super) fn forget_broker_credentials(&mut self) {
        let b = &mut self.broker;
        match b.store.clear() {
            Ok(()) => {
                b.saved = Credentials::default();
                b.form = Credentials::default();
                let mut creds = Credentials::default();
                b.from_env = creds.apply_env(|k| std::env::var(k).ok());
                b.creds = creds;
                b.portfolio = None;
                b.error = None;
                b.fetched = None;
                b.portal_url = None;
                b.message = Some((
                    if b.from_env {
                        "Saved credentials removed (SNAPTRADE_* environment variables still apply).".into()
                    } else {
                        "Saved credentials removed from this computer.".into()
                    },
                    false,
                ));
            }
            Err(e) => b.message = Some((format!("Could not remove credentials: {e:#}"), true)),
        }
    }

    pub(super) fn register_snaptrade_user(&mut self) {
        let form = self.broker.form.clone();
        if !form.has_keys() {
            self.broker.message = Some(("Enter the client ID and consumer key first.".into(), true));
            return;
        }
        let user_id = if form.user_id.trim().is_empty() { new_user_id() } else { form.user_id.trim().to_string() };
        self.broker.busy = Some("Registering user…");
        self.broker.message = None;
        self.worker.send(Request::BrokerRegister { creds: form, user_id });
    }

    pub(super) fn connect_brokerage(&mut self) {
        if !self.broker.creds.is_ready() {
            self.broker.message =
                Some(("Save your SnapTrade keys and register a user before connecting a brokerage.".into(), true));
            self.settings_tab = super::settings_view::SettingsTab::Brokerage;
            self.open_panel(PaneKind::Settings);
            return;
        }
        let dark = self.applied_theme.as_ref().is_none_or(|t| t.dark);
        self.broker.busy = Some("Opening the Connection Portal…");
        self.broker.message = None;
        self.worker.send(Request::BrokerPortal { creds: self.broker.creds.clone(), dark });
    }

    /// `PORT`, `ACCT`, `POS`, `ORD`, `ACTV` with an optional subcommand, optionally for a
    /// security (`AAPL POS`).
    pub(super) fn portfolio_command(&mut self, view: PortView, arg: &str, symbol: Option<String>) {
        match arg.trim() {
            "REFRESH" | "SYNC" | "RELOAD" => {
                if self.broker.mode() == BrokerMode::Setup {
                    self.status = "Brokerage not set up: PORT SETUP".into();
                } else {
                    self.refresh_portfolio();
                    self.status = "Reloading brokerage accounts…".into();
                }
                return;
            }
            "CONNECT" | "LINK" | "ADD" => return self.connect_brokerage(),
            "SETUP" | "SETTINGS" | "KEYS" | "CONFIG" => {
                self.settings_tab = super::settings_view::SettingsTab::Brokerage;
                return self.open_panel(PaneKind::Settings);
            }
            _ => {}
        }
        // Like other panels, this replaces the targeted portfolio pane or opens one.
        let mut pane = PortfolioPane::new(view);
        pane.symbol = symbol;
        self.open_panel(PaneKind::Portfolio(Box::new(pane)));
        if self.broker.portfolio.is_none() && !self.broker.loading {
            self.refresh_portfolio();
        }
    }
}

impl TerminalApp {
    /// Settings › Brokerage: SnapTrade keys, user registration, the Connection Portal and
    /// where credentials are kept.
    pub(super) fn brokerage_tab(&mut self, ui: &mut egui::Ui) {
        use egui::RichText;

        use crate::theme::{self, dp, mono, muted, text, ui_font};
        let section = |ui: &mut egui::Ui, title: &str| {
            ui.add_space(dp(10.0));
            ui.label(RichText::new(title).font(ui_font(13.5)).color(text()));
            ui.separator();
        };
        let note = |ui: &mut egui::Ui, s: &str| {
            ui.label(RichText::new(s).font(ui_font(11.5)).color(muted()));
        };

        section(ui, "Brokerage accounts (SnapTrade)");
        let status = match self.broker.mode() {
            BrokerMode::Live => match self.broker.portfolio.as_deref() {
                Some(pf) if !pf.simulated => format!(
                    "Connected · {} account{}{}",
                    pf.accounts.len(),
                    if pf.accounts.len() == 1 { "" } else { "s" },
                    pf.fetched.map_or(String::new(), |t| format!(
                        " · synced {}",
                        t.with_timezone(&chrono::Local).format("%H:%M")
                    ))
                ),
                _ if self.broker.loading => "Connecting…".into(),
                _ => "Keys saved".into(),
            },
            BrokerMode::Simulated => "Not configured · showing the simulated sample portfolio".into(),
            BrokerMode::Setup => "Not configured".into(),
        };
        ui.label(RichText::new(status).font(ui_font(12.5)).color(theme::accent()));
        note(
            ui,
            "Read-only: terminal-rs reads accounts, balances, positions, orders and activity. It cannot place, \
             change or cancel orders, and the Connection Portal requests read-only access.",
        );
        note(
            ui,
            &format!(
                "Credentials are stored in {}; never in the repository or in layouts.",
                self.broker.store.describe()
            ),
        );
        if self.broker.from_env {
            ui.label(
                RichText::new("SNAPTRADE_* environment variables are set and override the saved values below.")
                    .font(ui_font(11.5))
                    .color(theme::amber()),
            );
        }

        section(ui, "1 · API keys");
        let reveal = self.broker.reveal;
        let field = |ui: &mut egui::Ui, label: &str, value: &mut String, secret: bool, hint: &str| {
            ui.horizontal(|ui| {
                ui.add_sized([130.0, 20.0], egui::Label::new(RichText::new(label).font(ui_font(12.5))));
                ui.add(
                    egui::TextEdit::singleline(value)
                        .password(secret && !reveal)
                        .hint_text(hint)
                        .font(mono(12.0))
                        .desired_width(330.0),
                );
            });
        };
        let form = &mut self.broker.form;
        field(ui, "Client ID", &mut form.client_id, false, "e.g. MY-APP-ID");
        field(ui, "Consumer key", &mut form.consumer_key, true, "secret");
        ui.horizontal(|ui| {
            ui.add_space(134.0);
            ui.checkbox(&mut self.broker.reveal, "Show secrets");
            if ui.link("Get keys from the SnapTrade dashboard").clicked()
                && let Err(e) = open_external(tr_broker::snaptrade::DASHBOARD_URL)
            {
                self.status = format!("Could not open the browser: {e}");
            }
        });

        section(ui, "2 · SnapTrade user");
        note(
            ui,
            "Commercial keys: register a user once (the secret is shown only at registration and is saved automatically). Personal keys: leave both empty.",
        );
        let form = &mut self.broker.form;
        field(ui, "User ID", &mut form.user_id, false, "registered automatically");
        field(ui, "User secret", &mut form.user_secret, true, "secret");
        let busy = self.broker.busy.is_some();
        ui.horizontal(|ui| {
            ui.add_space(134.0);
            let can_register = self.broker.form.has_keys() && self.broker.form.user_secret.trim().is_empty() && !busy;
            if ui
                .add_enabled(can_register, egui::Button::new("Register User"))
                .on_hover_text("Create a SnapTrade user for these keys and save its secret")
                .clicked()
            {
                self.register_snaptrade_user();
            }
        });

        section(ui, "3 · Save and connect");
        let dirty = self.broker.form != self.broker.saved;
        ui.horizontal_wrapped(|ui| {
            let ready = self.broker.form.is_ready();
            if ui.add_enabled(ready && dirty && !busy, egui::Button::new("Save")).clicked() {
                self.save_broker_form("Credentials saved.");
                self.refresh_portfolio();
            }
            let live = self.broker.creds.is_ready();
            if ui.add_enabled(live && !busy && !self.broker.loading, egui::Button::new("Test Connection")).clicked() {
                self.broker.testing = true;
                self.broker.message = None;
                self.refresh_portfolio();
            }
            if ui
                .add_enabled(live && !busy, egui::Button::new(RichText::new("Connect Brokerage…").strong()))
                .on_hover_text("Open SnapTrade's Connection Portal in your browser (read-only)")
                .clicked()
            {
                self.connect_brokerage();
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Forget Credentials"))
                .on_hover_text("Remove the saved keys and user from this computer")
                .clicked()
            {
                self.forget_broker_credentials();
            }
            if let Some(what) = self.broker.busy {
                ui.add(egui::Spinner::new().size(12.0));
                ui.label(RichText::new(what).font(ui_font(11.5)).color(muted()));
            }
        });
        if let Some((m, err)) = &self.broker.message {
            ui.label(RichText::new(m).font(ui_font(12.0)).color(if *err { theme::down() } else { theme::up() }));
        }
        if let Some(url) = self.broker.portal_url.clone() {
            ui.horizontal(|ui| {
                note(ui, "Portal link (expires in 5 minutes):");
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(url.clone());
                }
                if ui.small_button("Open").clicked() {
                    let _ = open_external(&url);
                }
            });
        }

        section(ui, "Using it");
        note(
            ui,
            "PORT summary · ACCT accounts and balances · POS positions · ORD orders · ACTV activity · AAPL POS one security · PORT REFRESH · PORT CONNECT",
        );
        note(
            ui,
            "Click a position to chart it; prices in cyan are live terminal quotes. Data comes from your broker through SnapTrade and may be delayed.",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_ids_are_unique_and_anonymous() {
        let a = new_user_id();
        std::thread::sleep(Duration::from_millis(1));
        let b = new_user_id();
        assert!(a.starts_with("terminal-rs-") && a.len() == "terminal-rs-".len() + 16);
        assert_ne!(a, b);
    }

    #[test]
    fn pane_titles() {
        assert_eq!(PortfolioPane::new(PortView::Summary).title(), "Portfolio");
        assert_eq!(PortfolioPane::new(PortView::Orders).title(), "Orders");
        let mut p = PortfolioPane::new(PortView::Positions);
        p.symbol = Some("AAPL US Equity".into());
        assert_eq!(p.title(), "AAPL Positions");
    }
}
