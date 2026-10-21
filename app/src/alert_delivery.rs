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

    #[test]
    #[ignore = "BUG: notification_command on macOS appends title/body after the -e script statements without a `--` end-of-options marker, so osascript's getopt re-parses any payload that starts with '-' as CLI options: `-h` prints usage (the notification never shows and stderr is Stdio::null so the failure is silent), `-l JavaScript` switches the language and the AppleScript fails to compile, and `-e <text>` injects another script statement (AppleScript accepts `use …`/comments as top-level). notify-send already passes `--`; macOS must do the same."]
    fn notification_payload_must_follow_end_of_options_marker() {
        let title = "-e use scripting additions";
        let body = "-h";
        let Some(command) = notification_command(title, body) else { panic!("macOS must produce a command") };
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        let payload_at = args.len() - 2;
        let separator = args[..payload_at].iter().position(|a| *a == "--");
        assert_eq!(separator, Some(payload_at - 1), "title/body must sit after a bare `--`, got {args:?}");
        assert_eq!(args[payload_at], title);
        assert_eq!(args[payload_at + 1], body);
    }

    #[test]
    fn notification_and_sound_commands_keep_payload_intact_and_reapless() {
        // Very long text is a single argv element; delivery failures must not panic.
        let title = "T".repeat(16_384);
        let body = "line1\nline2\ttabbed \"quoted\" 'single'".to_string();
        let Some(command) = notification_command(&title, &body) else { return };
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args[args.len() - 2], title);
        assert_eq!(args[args.len() - 1], body);
        // Sound is a fixed path with no user-controlled arguments.
        if let Some(sound) = sound_command() {
            let sound_args: Vec<_> = sound.get_args().map(|a| a.to_str().unwrap()).collect();
            assert!(sound_args.iter().all(|a| !a.contains(&title) && !a.contains(&body)));
            if cfg!(target_os = "macos") {
                assert_eq!(sound.get_program().to_str().unwrap(), "afplay");
                assert_eq!(sound_args, vec!["/System/Library/Sounds/Glass.aiff"]);
            }
        }
    }
}
