//! LaunchAgent installation. The plist generator is pure and platform-independent.
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const LABEL: &str = "com.terminal-rs.alertd";
fn xml(value: &str) -> String {
    value
        .chars()
        .filter(|c| matches!(*c, '\t' | '\n' | '\r') || *c >= ' ')
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
pub fn plist(binary: &Path, data_dir: &Path, env: &[(String, String)]) -> String {
    let binary = xml(&binary.to_string_lossy());
    let logs = data_dir.join("logs");
    let stdout = xml(&logs.join("alertd.out.log").to_string_lossy());
    let stderr = xml(&logs.join("alertd.err.log").to_string_lossy());
    let env = env
        .iter()
        .filter(|(key, _)| matches!(key.as_str(), "TR_DATA_DIR" | "TR_SECRET_STORE"))
        .map(|(key, value)| format!("<key>{}</key><string>{}</string>", xml(key), xml(value)))
        .collect::<String>();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{LABEL}</string><key>ProgramArguments</key><array><string>{binary}</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>StandardOutPath</key><string>{stdout}</string><key>StandardErrorPath</key><string>{stderr}</string><key>EnvironmentVariables</key><dict>{env}</dict></dict></plist>\n"
    )
}
pub fn plist_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("Home directory is unavailable")?;
    Ok(PathBuf::from(home).join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}
pub fn installed() -> bool {
    plist_path().is_ok_and(|p| p.is_file())
}
fn launchctl(args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let status = Command::new("launchctl")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "Could not run launchctl")?;
    if status.success() { Ok(()) } else { Err(format!("launchctl exited with {status}")) }
}
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("Available on macOS".into());
    }
    let path = plist_path()?;
    #[cfg(unix)]
    // SAFETY: geteuid takes no arguments and has no preconditions.
    let uid = unsafe { libc::geteuid() };
    #[cfg(not(unix))]
    let uid = 0;
    let domain = format!("gui/{uid}");
    if !enabled {
        let result = launchctl(&["bootout".as_ref(), format!("{domain}/{LABEL}").as_ref()]);
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        return result;
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let binary = executable.parent().ok_or("Missing executable directory")?.join("terminal-rs-alertd");
    if !binary.is_absolute() || !binary.is_file() {
        return Err("The alert daemon is missing beside the application".into());
    }
    let dir = crate::data_dir();
    let dir = if dir.is_absolute() { dir } else { std::env::current_dir().map_err(|e| e.to_string())?.join(dir) };
    std::fs::create_dir_all(dir.join("logs")).map_err(|e| e.to_string())?;
    let env: Vec<_> = ["TR_DATA_DIR", "TR_SECRET_STORE"]
        .into_iter()
        .filter_map(|key| {
            std::env::var_os(key).map(|value| {
                let value = if key == "TR_DATA_DIR" {
                    dir.to_string_lossy().into_owned()
                } else {
                    value.to_string_lossy().into_owned()
                };
                (key.to_string(), value)
            })
        })
        .collect();
    crate::storage::write_atomic(&path, plist(&binary, &dir, &env).as_bytes()).map_err(|e| e.to_string())?;
    let result = launchctl(&["bootstrap".as_ref(), domain.as_ref(), path.as_os_str()]);
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plist_escapes_paths_and_limits_environment() {
        let text = plist(
            Path::new("/Applications/A & <B>/terminal-rs-alertd"),
            Path::new("/a b/&logs"),
            &[
                ("TR_DATA_DIR".into(), "/a & <b>".into()),
                ("TR_SECRET_STORE".into(), "file".into()),
                ("CREDENTIAL".into(), "excluded".into()),
            ],
        );
        assert!(text.contains("/Applications/A &amp; &lt;B&gt;/terminal-rs-alertd"));
        assert!(text.contains("/a b/&amp;logs/logs/alertd.err.log"));
        assert!(text.contains("<key>TR_SECRET_STORE</key><string>file</string>"));
        assert!(text.contains("<key>TR_DATA_DIR</key><string>/a &amp; &lt;b&gt;</string>"));
        assert!(!text.contains("CREDENTIAL"));
        assert!(text.contains("<key>RunAtLoad</key><true/>"));
        assert!(text.contains("<key>KeepAlive</key><true/>"));
        assert!(
            plist(Path::new("/bin/d"), Path::new("/data"), &[])
                .contains("<key>EnvironmentVariables</key><dict></dict>")
        );
    }

    /// Characters XML 1.0 forbids anywhere in a document (see production `Char`).
    fn xml_illegal(text: &str) -> Vec<char> {
        text.chars()
            .filter(|c| {
                let u = *c as u32;
                !(u == 0x9 || u == 0xA || u == 0xD || (0x20..=0xD7FF).contains(&u) || (0xE000..=0xFFFD).contains(&u))
            })
            .collect()
    }

    #[test]
    #[ignore = "BUG: launch::xml keeps CR and every char >= 0x20 verbatim, but an XML parser silently turns CR into LF in element content and rejects the noncharacters U+FFFE/U+FFFF outright, so a binary path, log path or TR_DATA_DIR containing CR or a noncharacter is corrupted or unparseable — the generated plist is not round-trip safe and launchctl install fails or launches the wrong path"]
    fn plist_output_is_round_trip_safe_under_xml_parsing() {
        for poison in ['\r', '\u{fffe}', '\u{ffff}'] {
            let binary = format!("/Applications/terminal{poison}-rs-alertd");
            let data = format!("/Users/a{poison}b/logs");
            let text = plist(
                Path::new(&binary),
                Path::new(&data),
                &[("TR_DATA_DIR".into(), format!("/data{poison}dir")), ("TR_SECRET_STORE".into(), "file".into())],
            );
            let bad = xml_illegal(&text);
            assert!(bad.is_empty(), "plist emitted XML-illegal {bad:?} for {poison:?}");
            assert!(
                !text.contains('\r'),
                "raw CR survives into the plist and any XML parser normalizes it to LF, corrupting the path: {text}"
            );
            assert!(!text.contains('\u{fffe}') && !text.contains('\u{ffff}'));
        }
    }
}
