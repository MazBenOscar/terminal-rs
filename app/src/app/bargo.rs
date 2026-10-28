//! SENATE-BARGO: Settings and cached credentials. Replies never contain the raw key.
use super::TerminalApp;
use crate::worker::{Request, data_dir};
use egui::Ui;
use std::{
    sync::{OnceLock, RwLock},
    time::{Duration, Instant},
};
use tr_broker::secrets::{SecretStore, mask};

static KEY: OnceLock<RwLock<Option<String>>> = OnceLock::new();
pub(crate) fn key() -> Option<String> {
    KEY.get_or_init(|| RwLock::new(store().load_text().ok().flatten())).read().expect("Bargo key lock").clone()
}
fn store() -> SecretStore {
    SecretStore::platform_for(&data_dir(), "bargo")
}
pub enum Action {
    Status,
    Save(String),
    Remove,
}
pub struct Reply {
    masked: String,
    message: Option<String>,
    status: String,
    usage: String,
}
#[derive(Default)]
pub struct State {
    pub congress_status: String,
    input: String,
    masked: String,
    message: Option<String>,
    status: String,
    usage: String,
    pending: bool,
    refreshed: Option<Instant>,
}
impl State {
    pub fn receive(&mut self, reply: Reply) {
        self.masked = reply.masked;
        self.message = reply.message;
        self.status = reply.status;
        self.usage = reply.usage;
        self.pending = false;
        self.refreshed = Some(Instant::now());
    }
}
async fn save_test(store: &SecretStore, guard: Result<&tr_net::Guard, tr_net::Refused>, value: &str) -> String {
    if store.save_text(value.trim()).is_err() {
        return "Could not save Bargo key".into();
    }
    match guard {
        Ok(guard) => match guard.send("bargo", tr_congress::bargo::request("health", Some(value.trim()))).await {
            Ok(body) => match serde_json::from_slice::<serde_json::Value>(&body) {
                Ok(value) if value["status"] == "ok" => {
                    "Bargo key saved; health test passed (health does not validate keys)".into()
                }
                _ => "Bargo key saved; invalid health response".into(),
            },
            Err(e) => format!("Bargo key saved; test: {e}"),
        },
        Err(e) => format!("Bargo key saved; test: {e}"),
    }
}
pub async fn perform(action: Action, updated: Option<i64>) -> Reply {
    let mut message = None;
    match action {
        Action::Status => {}
        Action::Remove => {
            message = Some(if store().clear().is_ok() {
                let _ = key();
                *KEY.get().expect("loaded key").write().expect("Bargo key lock") = None;
                "Bargo key removed".into()
            } else {
                "Could not remove Bargo key".into()
            });
        }
        Action::Save(value) => {
            message = Some(save_test(&store(), tr_net::shared(), &value).await);
            let _ = key();
            *KEY.get().expect("loaded key").write().expect("Bargo key lock") = store().load_text().ok().flatten();
        }
    }
    let key = key();
    let keyed = key.as_ref().is_some_and(|k| !k.is_empty());
    let cap = if keyed { 40 } else { 10 };
    let (status, usage) = match tr_net::shared() {
        Ok(guard) => {
            guard.set_bargo_keyed(keyed);
            let status = tr_congress::bargo::status(guard, updated, keyed);
            let requests = guard
                .status()
                .ok()
                .and_then(|v| v.into_iter().find(|s| s.group == "bargo"))
                .map(|s| s.requests_today)
                .unwrap_or(0);
            let rows = guard
                .remaining("bargo")
                .ok()
                .flatten()
                .map(|r| {
                    format!(
                        "{} (reported {})",
                        r.rows.map(|n| n.to_string()).unwrap_or_else(|| "unknown".into()),
                        chrono::DateTime::from_timestamp(r.at, 0)
                            .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                            .unwrap_or_default()
                    )
                })
                .unwrap_or_else(|| "not reported".into());
            (status, format!("Requests today: {requests} / {cap} · rows remaining: {rows}"))
        }
        Err(e) => (format!("Senate via Bargo · {e}"), String::new()),
    };
    Reply { masked: key.as_deref().map(mask).unwrap_or_default(), message, status, usage }
}

fn heading(ui: &mut Ui, enabled: &mut bool) -> bool {
    ui.separator();
    ui.heading("Senate (Bargo)");
    let changed = ui.checkbox(enabled, "Enable Senate (Bargo)").changed();
    ui.hyperlink_to(tr_congress::bargo::CREDIT, tr_congress::bargo::CREDIT_URL);
    ui.label("Optional free key. Keyless: lower caps. Free data window: 3 months; no raw-record redistribution.");
    changed
}
pub(super) fn draw(app: &mut TerminalApp, ui: &mut Ui) -> bool {
    let changed = heading(ui, &mut app.data_sources.settings.bargo);
    let mut action = None;
    let state = &mut app.data_sources.bargo;
    ui.add_enabled_ui(!state.pending, |ui| {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut state.input).password(true).hint_text("Optional Bargo API key"));
            ui.label(if state.masked.is_empty() { "Keyless" } else { &state.masked });
            if ui.add_enabled(!state.input.trim().is_empty(), egui::Button::new("Save & Test")).clicked() {
                action = Some(Action::Save(std::mem::take(&mut state.input)));
            }
            if ui.add_enabled(!state.masked.is_empty(), egui::Button::new("Remove")).clicked() {
                action = Some(Action::Remove);
            }
        });
    });
    ui.label(&state.status);
    ui.label(&state.usage);
    if let Some(message) = &state.message {
        ui.label(message);
    }
    if !state.pending && state.refreshed.is_none_or(|t| t.elapsed() >= Duration::from_secs(10)) {
        action = action.or(Some(Action::Status));
    }
    if let Some(action) = action {
        state.pending = true;
        app.worker.send(Request::BargoSettings(action));
    }
    if !state.pending {
        ui.ctx().request_repaint_after(Duration::from_secs(10));
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn senate_data_source_row_credit_switch_and_serialization() {
        let ctx = egui::Context::default();
        let mut settings = tr_net::Settings::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            heading(ui, &mut settings.bargo);
        });
        output.textures_delta.clear();
        let texts: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                _ => None,
            })
            .collect();
        assert!(texts.contains(&"Senate (Bargo)"));
        assert!(texts.contains(&tr_congress::bargo::CREDIT));
        assert!(settings.enabled("bargo"));
        settings.bargo = false;
        let settings = tr_net::Settings::from_section(serde_json::to_value(settings).unwrap());
        assert!(!settings.enabled("bargo"));
    }
    #[tokio::test]
    async fn secret_file_is_masked_test_is_offline_and_errors_never_echo_key() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../target/test-data/bargo-secret-{}", std::process::id()));
        let store = SecretStore {
            backend: tr_broker::secrets::Backend::File(root.join("secrets/bargo.json")),
            account: "bargo",
        };
        let guard = tr_net::Guard::new(
            tr_net::Config {
                ledger: root.join("ledger.sqlite3"),
                data_dir: Some(root.join("instance")),
                allow_external: false,
                max_wait: Duration::ZERO,
            },
            tr_net::Settings::default(),
        )
        .unwrap();
        let key = "fixture-bargo-secret-1234";
        let message = save_test(&store, Ok(&guard), key).await;
        assert!(message.contains("Offline"));
        assert!(!message.contains(key));
        assert_eq!(store.load_text().unwrap().as_deref(), Some(key));
        let mut state = State::default();
        state.receive(Reply { masked: mask(key), message: Some(message), status: String::new(), usage: String::new() });
        assert!(!state.masked.contains(key));
        assert!(state.input.is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(root.join("secrets/bargo.json")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        store.clear().unwrap();
        assert!(store.load_text().unwrap().is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
