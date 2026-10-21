//! Optional platform delivery. Alert text is always passed as data, never code.
use std::process::{Command, Stdio};

pub fn notification_command(title: &str, body: &str) -> Option<Command> {
    if cfg!(target_os = "macos") {
        let mut command = Command::new("osascript");
        command.args([
            "-e",
            "on run argv",
            "-e",
            "display notification (item 2 of argv) with title (item 1 of argv)",
            "-e",
            "end run",
            title,
            body,
        ]);
        Some(command)
    } else if cfg!(target_os = "linux") {
        let mut command = Command::new("notify-send");
        command.args(["--", title, body]);
        Some(command)
    } else {
        None
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_text_is_passed_as_raw_arguments() {
        let title = "Alert: AAPL \" & do shell script \"bad";
        let body = "Raw body\nwith 'quotes' and \\slashes";
        let Some(command) = notification_command(title, body) else { return };
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(&args[args.len() - 2..], &[title, body]);
        for script in &args[..args.len() - 2] {
            assert!(!script.contains(title));
            assert!(!script.contains(body));
        }
        if cfg!(target_os = "macos") {
            assert_eq!(
                &args[..6],
                &[
                    "-e",
                    "on run argv",
                    "-e",
                    "display notification (item 2 of argv) with title (item 1 of argv)",
                    "-e",
                    "end run"
                ]
            );
        } else {
            assert_eq!(args[0], "--");
        }
    }
}
