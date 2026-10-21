//! Per-user background alert agent; launchd owns its lifetime.
#[tokio::main]
async fn main() {
    let dir = tr_alerts::data_dir();
    let mut monitor = tr_alerts::Monitor::default();
    loop {
        // Reload rotating credentials after every handoff from the desktop app.
        if let Ok(events) = monitor.cycle_with(&dir, || tr_data::from_env_with(tr_alerts::questrade_handle())).await {
            let sound = std::fs::read(dir.join("prefs.json"))
                .ok()
                .filter(|bytes| bytes.len() <= 4_194_304)
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .and_then(|prefs| prefs.get("alert_sound").and_then(|value| value.as_bool()))
                .unwrap_or(false);
            for event in events {
                let body = format!("{} {} {:.4}", event.alert.symbol, event.alert.direction.label(), event.price);
                tr_alerts::delivery::spawn(
                    tr_alerts::delivery::notification_command("Price alert", &body),
                    event.alert.id,
                );
                if sound {
                    tr_alerts::delivery::spawn(tr_alerts::delivery::sound_command(), event.alert.id);
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}
