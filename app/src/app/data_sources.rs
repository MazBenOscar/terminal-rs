//! Data-source preferences and worker operations. Raw keys never return to the UI.
use super::TerminalApp;
use crate::{
    theme::{self, dp, mono, ui_font},
    worker::{Request, data_dir},
};
use egui::{RichText, Ui};
use std::time::{Duration, Instant};
use tr_broker::secrets::{SecretStore, mask};

pub enum Action {
    Status,
    Save(String),
    Remove,
}
pub struct Reply {
    pub status: Result<Vec<tr_net::Status>, String>,
    pub masked_key: String,
    pub message: Option<Result<String, String>>,
    status_only: bool,
}
#[derive(Default)]
pub struct State {
    // SENATE-BARGO
    pub(super) bargo: super::bargo::State,
    // SENATE-BARGO END
    pub settings: tr_net::Settings,
    key_input: String,
    contact_input: String,
    masked_key: String,
    busy: bool,
    status_pending: bool,
    refreshed: Option<Instant>,
    status: Vec<tr_net::Status>,
    message: Option<Result<String, String>>,
}
pub(super) fn deserialize_settings<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<tr_net::Settings, D::Error> {
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    Ok(tr_net::Settings::from_section(value))
}
impl State {
    fn begin(&mut self, action: &Action) {
        if matches!(action, Action::Status) {
            self.status_pending = true;
        } else {
            self.busy = true;
        }
    }
    pub fn new(settings: tr_net::Settings) -> Self {
        Self { contact_input: settings.sec_contact.clone(), settings, ..Self::default() }
    }
    fn refresh_delay(&self, interval: Duration) -> Option<Duration> {
        (!self.busy && !self.status_pending)
            .then(|| self.refreshed.map_or(Duration::ZERO, |at| interval.saturating_sub(at.elapsed())))
    }
    pub fn receive(&mut self, reply: Reply) {
        if reply.status_only {
            self.status_pending = false;
        } else {
            self.busy = false;
        }
        self.refreshed = Some(Instant::now());
        self.masked_key = reply.masked_key;
        match reply.status {
            Ok(rows) => self.status = rows,
            Err(error) => self.message = Some(Err(error)),
        }
        if reply.message.is_some() {
            self.message = reply.message;
        }
    }
}

const FINNHUB_QUOTE: &str = "https://finnhub.io/api/v1/quote";
fn finnhub_request(key: &str) -> reqwest::RequestBuilder {
    let token = reqwest::header::HeaderValue::from_str(key).map(|mut value| {
        value.set_sensitive(true);
        value
    });
    let request = reqwest::Client::new().get(FINNHUB_QUOTE).query(&[("symbol", "AAPL")]);
    match token {
        Ok(value) => request.header("X-Finnhub-Token", value),
        // Preserve RequestBuilder's fail-closed invalid-header behavior without formatting the secret.
        Err(_) => request.header("X-Finnhub-Token", "\n"),
    }
}
pub async fn perform(action: Action, cached_key: Option<&str>) -> Reply {
    if matches!(&action, Action::Status) {
        return Reply {
            status_only: true,
            status: tr_net::shared().and_then(|g| g.status()).map_err(|e| e.to_string()),
            masked_key: cached_key.map(mask).unwrap_or_default(),
            message: None,
        };
    }
    let store = SecretStore::platform_for(&data_dir(), "finnhub");
    perform_with_store(action, &store, tr_net::shared()).await
}
async fn perform_with_store(
    action: Action,
    store: &SecretStore,
    guard: Result<&tr_net::Guard, tr_net::Refused>,
) -> Reply {
    let status_only = matches!(&action, Action::Status);
    let message = match action {
        Action::Status => None,
        Action::Remove => Some(
            store.clear().map(|()| "Finnhub key removed".into()).map_err(|_| "Could not remove Finnhub key".into()),
        ),
        Action::Save(key) => Some(
            async {
                store.save_text(key.trim()).map_err(|_| "Could not save Finnhub key".to_string())?;
                let guard = guard.as_ref().map_err(|e| e.to_string())?;
                let request = finnhub_request(key.trim());
                let body = guard
                    .send("finnhub", request)
                    .await
                    .map_err(|e| format!("Key saved; test {}: {e}", tr_net::redact(FINNHUB_QUOTE)))?;
                let quote: serde_json::Value =
                    serde_json::from_slice(&body).map_err(|_| "Key saved; invalid quote response".to_string())?;
                if quote["c"].as_f64().is_none_or(|price| !price.is_finite() || price <= 0.0) {
                    return Err("Key saved; no quote returned".into());
                }
                Ok("Finnhub key saved and tested".into())
            }
            .await,
        ),
    };
    let (masked_key, load_error) = match store.load_text() {
        Ok(key) => (key.as_deref().map(mask).unwrap_or_default(), None),
        Err(_) => (String::new(), Some(Err("Could not read Finnhub key".into()))),
    };
    Reply {
        status_only,
        status: guard.and_then(|g| g.status()).map_err(|e| e.to_string()),
        masked_key,
        message: message.or(load_error),
    }
}
fn timestamp(value: i64) -> String {
    chrono::DateTime::from_timestamp(value, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "—".into())
}
fn state_text(status: &tr_net::Status, has_key: bool) -> String {
    if status.offline {
        "Offline (test instance)".into()
    } else if !status.enabled {
        status.reason.as_deref().filter(|s| s.starts_with("Corrupt data_sources")).unwrap_or("Off").into()
    } else if let Some(until) = status.paused_until {
        format!("Paused until {}", timestamp(until))
    } else if status.needs_contact {
        "Needs contact email".into()
    } else if status.group == "finnhub" && !has_key {
        "Needs key".into()
    } else {
        "On".into()
    }
}
fn source_name(group: &str) -> &str {
    match group {
        "finnhub" => "Finnhub",
        "sec" => "SEC EDGAR",
        "yahoo-news" => "Yahoo news",
        "coindesk" => "CoinDesk",
        "house-disclosures" => "House disclosures",
        "house-roster" => "House roster",
        // SENATE-BARGO
        "bargo" => "Senate (Bargo)",
        // SENATE-BARGO END
        _ => "Unknown source",
    }
}
// SENATE-BARGO
#[cfg(test)]
const SENATE_ROW: &str = "Senate (Bargo)";
// SENATE-BARGO END
fn section(ui: &mut Ui, title: &str) {
    ui.add_space(dp(10.0));
    ui.label(RichText::new(title).font(ui_font(13.5)).color(theme::text()));
    ui.separator();
}
impl TerminalApp {
    fn source_action(&mut self, action: Action) {
        self.data_sources.begin(&action);
        self.worker.send(Request::DataSources { action });
    }
    pub(super) fn data_sources_tab(&mut self, ui: &mut Ui) {
        section(ui, "External news and filings");
        let mut changed =
            ui.checkbox(&mut self.data_sources.settings.pause_all, "Pause all external news and filings").changed();
        ui.horizontal_wrapped(|ui| {
            for (label, enabled) in [
                ("Finnhub", &mut self.data_sources.settings.finnhub),
                ("SEC EDGAR", &mut self.data_sources.settings.sec),
                ("Yahoo news", &mut self.data_sources.settings.yahoo_news),
                ("CoinDesk", &mut self.data_sources.settings.coindesk),
                ("House disclosures", &mut self.data_sources.settings.house_disclosures),
                ("House roster", &mut self.data_sources.settings.house_roster),
            ] {
                changed |= ui.checkbox(enabled, label).changed();
            }
        });
        section(ui, "Finnhub API key");
        ui.label(
            RichText::new("Personal, non-commercial use. Save & Test makes one guarded AAPL quote request.")
                .font(ui_font(12.0))
                .color(theme::muted()),
        );
        let busy = self.data_sources.busy;
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.data_sources.key_input)
                        .password(true)
                        .hint_text("Paste API key")
                        .font(mono(12.0))
                        .desired_width(dp(300.0)),
                );
                if !self.data_sources.masked_key.is_empty() {
                    ui.label(RichText::new(&self.data_sources.masked_key).font(mono(12.0)).color(theme::muted()));
                }
            });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.data_sources.key_input.trim().is_empty(), egui::Button::new("Save & Test"))
                    .clicked()
                {
                    let key = std::mem::take(&mut self.data_sources.key_input);
                    self.source_action(Action::Save(key));
                }
                if ui.add_enabled(!self.data_sources.masked_key.is_empty(), egui::Button::new("Remove")).clicked() {
                    self.source_action(Action::Remove);
                }
            });
        });
        section(ui, "SEC EDGAR contact");
        ui.label(
            RichText::new("Required for SEC identification. Stored locally in settings, not in the secret store.")
                .font(ui_font(12.0))
                .color(theme::muted()),
        );
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.data_sources.contact_input)
                    .hint_text("Contact email")
                    .font(ui_font(12.0))
                    .desired_width(dp(300.0)),
            );
            let email = self.data_sources.contact_input.trim();
            let valid = email.is_empty() || tr_net::valid_email(email);
            if ui.add_enabled(valid, egui::Button::new("Save contact")).clicked() {
                self.data_sources.settings.sec_contact = email.to_owned();
                changed = true;
            }
            if !valid {
                ui.label(RichText::new("Enter a valid email").color(theme::down()).font(ui_font(12.0)));
            }
        });
        // SENATE-BARGO
        changed |= super::bargo::draw(self, ui);
        // SENATE-BARGO END
        if changed {
            // Explicit user edits replace a rejected section with validated preferences.
            self.data_sources.settings.settings_error = false;
            // Apply switches immediately, even while a Save & Test is running.
            tr_net::configure_shared(self.data_sources.settings.clone());
            self.data_sources.refreshed = None;
            self.save_settings();
        }
        if let Some(message) = &self.data_sources.message {
            let (text, color) = match message {
                Ok(text) => (text, theme::up()),
                Err(text) => (text, theme::down()),
            };
            ui.label(RichText::new(text).color(color).font(ui_font(12.0)));
        }
        section(ui, "Shared machine-wide usage");
        egui::Grid::new("data-source-status").spacing(egui::vec2(dp(12.0), dp(4.0))).show(ui, |ui| {
            for heading in ["Source", "State", "Today / cap", "Last request"] {
                ui.label(RichText::new(heading).font(ui_font(12.0)).color(theme::muted()));
            }
            ui.end_row();
            for status in &self.data_sources.status {
                // SENATE-BARGO: the dedicated row includes key mode and provider row allowance.
                if status.group == "bargo" {
                    continue;
                }
                // SENATE-BARGO END
                let name = source_name(status.group);
                ui.label(RichText::new(name).font(ui_font(12.0)).color(theme::text()));
                ui.label(
                    RichText::new(state_text(status, !self.data_sources.masked_key.is_empty()))
                        .font(mono(11.0))
                        .color(theme::text()),
                )
                .on_hover_text(status.reason.as_deref().unwrap_or(""));
                ui.label(
                    RichText::new(format!("{} / {}", status.requests_today, status.cap))
                        .font(mono(11.0))
                        .color(theme::text()),
                );
                ui.label(
                    RichText::new(status.last_request.map(|t| timestamp(t / 1000)).unwrap_or_else(|| "—".into()))
                        .font(mono(11.0))
                        .color(theme::muted()),
                );
                ui.end_row();
            }
            // SENATE-BARGO: the former disabled Senate row is replaced above.
        });
        if !self.data_sources.settings.bargo {
            ui.label(RichText::new(tr_congress::refresh::SENATE_PAUSED).font(ui_font(12.0)).color(theme::muted()));
        }
        // SENATE-BARGO END
        let interval = Duration::from_secs(10);
        if self.data_sources.refresh_delay(interval) == Some(Duration::ZERO) {
            self.source_action(Action::Status);
        }
        // This function is only drawn for the active tab. No worker polling or idle repaint loop.
        if let Some(delay) = self.data_sources.refresh_delay(interval) {
            ui.ctx().request_repaint_after(delay);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refresh_is_bounded_and_never_spins_while_pending() {
        let interval = Duration::from_secs(10);
        let mut state = State::default();
        assert_eq!(state.refresh_delay(interval), Some(Duration::ZERO));
        state.busy = true;
        assert_eq!(state.refresh_delay(interval), None);
        state.refreshed = Some(Instant::now() - interval);
        assert_eq!(state.refresh_delay(interval), None);
        state.receive(Reply { status: Ok(Vec::new()), masked_key: String::new(), message: None, status_only: false });
        assert!(state.refresh_delay(interval).unwrap() > Duration::from_secs(9));
    }
    #[test]
    fn status_refresh_never_toggles_save_busy() {
        let mut state = State::default();
        state.begin(&Action::Status);
        assert!(state.status_pending);
        assert!(!state.busy);
        assert_eq!(state.refresh_delay(Duration::from_secs(10)), None);
        state.begin(&Action::Save("fixture".into()));
        state.receive(Reply { status: Ok(Vec::new()), masked_key: String::new(), message: None, status_only: true });
        assert!(state.busy, "a status reply must not finish Save & Test");
        assert!(!state.status_pending);
        state.receive(Reply {
            status: Ok(Vec::new()),
            masked_key: String::new(),
            message: Some(Ok("saved".into())),
            status_only: false,
        });
        assert!(!state.busy);
    }
    #[test]
    fn finnhub_token_is_header_only_and_sensitive() {
        let key = "fixture-finnhub-secret";
        let request = finnhub_request(key).build().unwrap();
        assert_eq!(request.headers()["X-Finnhub-Token"], key);
        assert!(request.headers()["X-Finnhub-Token"].is_sensitive());
        assert_eq!(request.url().query(), Some("symbol=AAPL"));
        assert!(!tr_net::redact(request.url().as_str()).contains(key));
        assert!(finnhub_request("bad\nkey").build().is_err());
    }
    #[test]
    fn corrupt_section_does_not_reset_app_preferences_or_enable_sources() {
        let settings: super::super::Settings =
            serde_json::from_str(r#"{"data_sources":{"finnhub":"yes"},"sidebar":false}"#).unwrap();
        assert!(!settings.sidebar);
        assert!(settings.data_sources.settings_error);
        assert!(tr_net::GROUPS.iter().all(|g| !settings.data_sources.enabled(g)));
    }
    #[test]
    fn congress_rows_have_switches_caps_pause_and_permanent_senate_explanation() {
        assert_eq!(source_name("house-disclosures"), "House disclosures");
        assert_eq!(source_name("house-roster"), "House roster");
        // SENATE-BARGO
        assert_eq!(SENATE_ROW, "Senate (Bargo)");
        assert_eq!(source_name("bargo"), SENATE_ROW);
        // SENATE-BARGO END
        assert_eq!(
            tr_congress::refresh::SENATE_PAUSED,
            "Senate: paused — needs a third-party source (senate.gov blocked this network)"
        );
        let mut settings = tr_net::Settings::default();
        for (group, cap) in [("house-disclosures", 400), ("house-roster", 20)] {
            assert!(settings.enabled(group));
            let policy = tr_net::policies().into_iter().find(|p| p.group == group).unwrap();
            assert_eq!(policy.daily_cap, cap);
            let mut status = tr_net::Status {
                group: policy.group,
                enabled: true,
                requests_today: 7,
                cap,
                last_request: None,
                paused_until: None,
                reason: None,
                offline: false,
                needs_contact: false,
            };
            assert_eq!(state_text(&status, false), "On");
            status.paused_until = Some(12345);
            assert!(state_text(&status, false).starts_with("Paused until "));
            status.enabled = false;
            assert_eq!(state_text(&status, false), "Off");
        }
        settings.house_disclosures = false;
        settings.house_roster = false;
        assert!(!settings.enabled("house-disclosures") && !settings.enabled("house-roster"));
        settings.house_disclosures = true;
        settings.house_roster = true;
        settings.pause_all = true;
        assert!(!settings.enabled("house-disclosures") && !settings.enabled("house-roster"));
        assert!(!settings.enabled("senate"));
        let roundtrip: tr_net::Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(roundtrip.house_disclosures && roundtrip.house_roster && roundtrip.pause_all);
    }
    #[test]
    fn status_text_covers_every_source_state() {
        let mut status = tr_net::Status {
            group: "finnhub",
            enabled: true,
            requests_today: 0,
            cap: 3000,
            last_request: None,
            paused_until: None,
            reason: None,
            offline: false,
            needs_contact: false,
        };
        assert_eq!(state_text(&status, false), "Needs key");
        assert_eq!(state_text(&status, true), "On");
        status.group = "sec";
        status.needs_contact = true;
        assert_eq!(state_text(&status, false), "Needs contact email");
        status.paused_until = Some(1);
        assert!(state_text(&status, false).starts_with("Paused until "));
        status.enabled = false;
        assert_eq!(state_text(&status, false), "Off");
        status.offline = true;
        assert_eq!(state_text(&status, false), "Offline (test instance)");
    }
    #[tokio::test]
    async fn finnhub_file_secret_is_masked_and_removed() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../target/test-data/finnhub-settings-{}", std::process::id()));
        let store = SecretStore {
            backend: tr_broker::secrets::Backend::File(root.join("secrets/finnhub.json")),
            account: "finnhub",
        };
        let key = "fixture-finnhub-secret-1234";
        // Disabled settings guarantee no sockets, independent of the test runner environment.
        let settings = tr_net::Settings { finnhub: false, ..Default::default() };
        let guard = tr_net::Guard::new(
            tr_net::Config {
                ledger: root.join("ledger.sqlite3"),
                data_dir: Some(root.join("instance")),
                allow_external: false,
                max_wait: Duration::ZERO,
            },
            settings.clone(),
        )
        .unwrap();
        let reply = perform_with_store(Action::Save(key.into()), &store, Ok(&guard)).await;
        assert_eq!(store.load_text().unwrap().as_deref(), Some(key));
        assert_eq!(reply.masked_key, mask(key));
        assert!(!reply.masked_key.contains(key));
        assert!(reply.message.as_ref().unwrap().as_ref().unwrap_err().contains("Offline"));
        let mut ui_state = State::new(settings);
        ui_state.receive(reply);
        assert_eq!(ui_state.masked_key, mask(key));
        assert!(ui_state.key_input.is_empty());
        assert!(!ui_state.message.as_ref().unwrap().as_ref().unwrap_err().contains(key));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.join("secrets/finnhub.json")).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let reply = perform_with_store(Action::Remove, &store, Ok(&guard)).await;
        assert!(reply.masked_key.is_empty());
        assert!(store.load_text().unwrap().is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
