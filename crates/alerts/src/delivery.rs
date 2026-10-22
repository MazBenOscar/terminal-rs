//! Platform delivery with alert text passed as data, never executable script.
use std::process::{Command, Stdio};

pub fn notification_command(title: &str, body: &str) -> Option<Command> {
    let title = notification_text(title);
    let body = notification_text(body);
    if cfg!(target_os = "macos") {
        Some(macos_notification_command(&title, &body))
    } else if cfg!(target_os = "linux") {
        let mut command = Command::new("notify-send");
        command.args(["--", &title, &body]);
        Some(command)
    } else {
        None
    }
}
pub fn notification_text(text: &str) -> String {
    let mut chars = text.chars().filter(|c| !c.is_ascii_control());
    let mut result: String = chars.by_ref().take(256).collect();
    if chars.next().is_some() {
        result.pop();
        result.push('…');
    }
    result
}
pub fn macos_notification_command(title: &str, body: &str) -> Command {
    let mut command = Command::new("osascript");
    command.args([
        "-e",
        "display notification (system attribute \"TR_NOTIFY_BODY\") with title (system attribute \"TR_NOTIFY_TITLE\")",
    ]);
    command.env("TR_NOTIFY_TITLE", title).env("TR_NOTIFY_BODY", body);
    command
}
pub fn sound_command() -> Option<Command> {
    if cfg!(target_os = "macos") {
        let mut command = Command::new("afplay");
        command.arg("/System/Library/Sounds/Glass.aiff");
        Some(command)
    } else {
        None
    }
}
pub fn spawn(command: Option<Command>, alert_id: u64) {
    let Some(mut command) = command else { return };
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    std::thread::spawn(move || match command.spawn() {
        Ok(mut child) => match child.wait() {
            Ok(status) if status.success() => {}
            _ => tracing::warn!(alert_id, "Alert delivery failed"),
        },
        Err(_) => tracing::warn!(alert_id, "Alert delivery failed"),
    });
}
