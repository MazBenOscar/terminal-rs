#[cfg(test)]
use std::process::Command;
#[cfg(test)]
use tr_alerts::delivery::{macos_notification_command, notification_text};
pub use tr_alerts::delivery::{notification_command, sound_command, spawn};

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_macos_payload(command: &Command, title: &str, body: &str) {
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "-e",
                "display notification (system attribute \"TR_NOTIFY_BODY\") with title (system attribute \"TR_NOTIFY_TITLE\")"
            ]
        );
        let env: std::collections::HashMap<_, _> = command.get_envs().collect();
        assert_eq!(env[std::ffi::OsStr::new("TR_NOTIFY_TITLE")], Some(std::ffi::OsStr::new(title)));
        assert_eq!(env[std::ffi::OsStr::new("TR_NOTIFY_BODY")], Some(std::ffi::OsStr::new(body)));
    }

    #[test]
    fn notification_payload_must_follow_end_of_options_marker() {
        // Raw command construction preserves even newlines as data, never options or code.
        for payload in
            ["-e beep", "-h", "-l JavaScript", "line1\nline2", "\" & do shell script \"bad", "'quoted' \\slashes"]
        {
            assert_macos_payload(&macos_notification_command(payload, payload), payload, payload);
            let Some(command) = notification_command(payload, payload) else { continue };
            let cleaned = notification_text(payload);
            if cfg!(target_os = "macos") {
                assert_macos_payload(&command, &cleaned, &cleaned);
            } else {
                let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
                assert_eq!(args, ["--", &cleaned, &cleaned]);
            }
        }
    }

    #[test]
    fn notification_text_is_bounded_and_strips_ascii_controls() {
        assert_eq!(notification_text("a\0\n\r\t\u{7f}b"), "ab");
        assert_eq!(notification_text(&"é".repeat(256)), "é".repeat(256));
        let long = "é".repeat(257);
        let capped = format!("{}…", "é".repeat(255));
        assert_eq!(notification_text(&long), capped);
        let Some(command) = notification_command(&long, &long) else { return };
        if cfg!(target_os = "macos") {
            assert_macos_payload(&command, &capped, &capped);
        } else {
            let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
            assert_eq!(args, ["--", &capped, &capped]);
        }
        if let Some(sound) = sound_command() {
            assert_eq!(sound.get_program(), "afplay");
            assert_eq!(sound.get_args().collect::<Vec<_>>(), ["/System/Library/Sounds/Glass.aiff"]);
        }
    }
}
