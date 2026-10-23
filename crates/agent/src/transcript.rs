use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};

use anyhow::{Result, anyhow, bail};
use regex::Regex;
use serde_json::Value;

#[derive(Clone)]
pub struct Transcript {
    sender: SyncSender<Record>,
    failed: Arc<AtomicBool>,
}

enum Record {
    Line(String),
    Flush(tokio::sync::oneshot::Sender<bool>),
}

impl Transcript {
    /// Raw transcripts contain credentials. Store outside the repository, then scrub.
    pub fn create(path: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        let (sender, receiver) = sync_channel::<Record>(256);
        let failed = Arc::new(AtomicBool::new(false));
        let status = failed.clone();
        std::thread::spawn(move || {
            for record in receiver {
                let result = match record {
                    Record::Line(line) => writeln!(file, "{line}").and_then(|()| file.flush()),
                    Record::Flush(ack) => {
                        let result = file.sync_all();
                        let _ = ack.send(result.is_ok());
                        result
                    }
                };
                if result.is_err() {
                    status.store(true, Ordering::SeqCst);
                    break;
                }
            }
        });
        Ok(Self { sender, failed })
    }

    pub fn record(&self, value: &Value) -> Result<()> {
        if self.failed.load(Ordering::SeqCst) {
            bail!("transcript write failed");
        }
        self.sender
            .try_send(Record::Line(serde_json::to_string(value)?))
            .map_err(|_| anyhow!("transcript queue overflow or closed"))
    }

    pub async fn flush(&self) -> Result<()> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.sender.try_send(Record::Flush(sender)).map_err(|_| anyhow!("transcript queue overflow or closed"))?;
        if !tokio::time::timeout(std::time::Duration::from_secs(5), receiver).await?? {
            bail!("transcript flush failed");
        }
        Ok(())
    }
}

pub fn scrub_jsonl(input: impl BufRead, mut output: impl Write) -> Result<()> {
    for line in input.lines() {
        let mut value: Value = serde_json::from_str(&line?)?;
        scrub_value(&mut value);
        writeln!(output, "{value}")?;
    }
    Ok(())
}

pub fn scrub_value(value: &mut Value) {
    scrub_nested(value, false);
}

fn scrub_nested(value: &mut Value, identifying: bool) {
    match value {
        Value::Object(object) => {
            for (key, value) in object.iter_mut() {
                let key = key.to_ascii_lowercase().replace(['_', '-'], "");
                if ((key.contains("token") || key.contains("secret")) && value.is_string())
                    || (identifying && key == "id")
                    || key == "authorization"
                    || key.contains("cookie")
                    || key.contains("session")
                    || key.contains("password")
                    || key.contains("secret")
                    || key == "limitname"
                    || key == "servername"
                    || key == "hostname"
                    || key == "balance"
                    || key == "plantype"
                    || key.contains("email")
                    || key.contains("authurl")
                    || key.contains("apikey")
                    || key.contains("loginid")
                    || ((key.contains("account")
                        || key.contains("user")
                        || key.contains("org")
                        || key == "ratelimitid"
                        || key == "limitid")
                        && key.contains("id"))
                {
                    *value = Value::String("[REDACTED]".into());
                } else if key == "ratelimitsbylimitid" {
                    // Map keys can themselves be identifying quota names.
                    if let Value::Object(map) = value {
                        let entries = std::mem::take(map);
                        for (index, (_, mut entry)) in entries.into_iter().enumerate() {
                            scrub_nested(&mut entry, true);
                            map.insert(format!("redacted-{index}"), entry);
                        }
                    }
                } else {
                    let identifying = identifying
                        || key.contains("account")
                        || key.contains("user")
                        || key.contains("org")
                        || key.contains("ratelimit");
                    scrub_nested(value, identifying);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|value| scrub_nested(value, identifying)),
        Value::String(text) => *text = scrub_text(text),
        _ => {}
    }
}

pub fn scrub_text(text: &str) -> String {
    use std::sync::OnceLock;
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            r"(?i)[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9.-]+\.[a-z]{2,}",
            r#"(?i)https?://[^\s"<>]+\?[^\s"<>]+"#,
            r"\b[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b",
            r"(?i)\b(?:sk-|acct[-_]|account[-_]|user[-_]|org[-_]|rate[-_]limit[-_])[A-Za-z0-9_-]+",
            r"(?i)\b(?:Bearer|Basic)\s+[A-Za-z0-9._~+/-]+=*",
            r#"(?i)["']?(?:authorization|set-cookie|cookie|session(?:id)?|password|secret|api[_-]?key)["']?\s*[:=]\s*(?:"[^"\r\n]*"|'[^'\r\n]*'|[^\r\n,;}]+)"#,
            r"\b[A-Za-z0-9_-]{32,}\b",
        ]
        .iter()
        .map(|pattern| Regex::new(pattern).unwrap())
        .collect()
    });
    static HOME: OnceLock<Regex> = OnceLock::new();
    // Home-directory paths name the local user; keep the rest of the path for debugging.
    let home = HOME.get_or_init(|| Regex::new(r#"/(Users|home)/[^/\s"']+"#).unwrap());
    let text = home.replace_all(text, "/$1/[USER]");
    patterns.iter().fold(text.into_owned(), |text, pattern| pattern.replace_all(&text, "[REDACTED]").into_owned())
}

#[derive(Default)]
pub struct StreamScrubber {
    pending: String,
}

impl StreamScrubber {
    pub fn push(&mut self, delta: &str) -> Result<String> {
        self.pending.push_str(delta);
        if self.pending.len() > 1024 * 1024 {
            bail!("streaming line exceeds scrubber limit");
        }
        let Some(end) = self.pending.rfind('\n') else {
            return Ok(String::new());
        };
        let complete: String = self.pending.drain(..=end).collect();
        Ok(scrub_text(&complete))
    }

    pub fn finish(&mut self) -> String {
        scrub_text(&std::mem::take(&mut self.pending))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn streaming_scrubber_handles_secrets_split_across_deltas() {
        let mut scrubber = StreamScrubber::default();
        assert_eq!(scrubber.push("Contact trader@").unwrap(), "");
        assert_eq!(scrubber.push("example.com with Bearer ").unwrap(), "");
        let line = scrubber.push("secret-token\n").unwrap();
        assert!(!line.contains("trader"));
        assert!(!line.contains("secret-token"));
        assert_eq!(scrubber.finish(), "");
    }

    #[tokio::test]
    async fn recorder_writes_private_jsonl_and_flushes() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("transcript-{}.jsonl", std::process::id()));
        let transcript = Transcript::create(&path).unwrap();
        transcript.record(&json!({"id":1,"result":{"accessToken":"raw-secret"}})).unwrap();
        transcript.flush().await.unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("raw-secret"));
        assert_eq!(raw.lines().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        drop(transcript);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn scrubber_redacts_fixture_secrets() {
        let mut value = json!({
            "email":"trader@example.com", "chatgptAccountId":"opaque", "user_id":"opaque-user",
            "orgId":"opaque-org", "accessToken":"abc", "limitId":"quota-name",
            "authUrl":"https://login.example/?state=secret", "text":"trader@example.com user-123 org-456 sk-abcdef eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signaturelong",
            "rateLimitsByLimitId":{"private-quota":{"limitId":"private-quota","usedPercent":5}},
            "account":{"id":"nested-account"}, "tokenUsage":{"totalTokens":123},
            "serverName":"traders-macbook.local", "credits":{"balance":"1234.5"}, "planType":"pro", "cwd":"/Users/trader/Library/agent/empty", "path":"/home/trader/x"
        });
        scrub_value(&mut value);
        let text = value.to_string();
        for secret in [
            "trader@",
            "opaque",
            "quota-name",
            "state=secret",
            "user-123",
            "org-456",
            "sk-abcdef",
            "eyJ",
            "private-quota",
            "traders-macbook",
            "/Users/trader",
            "/home/trader",
            "1234.5",
            "\"pro\"",
        ] {
            assert!(!text.contains(secret), "{secret}");
        }
        assert!(text.contains("usedPercent"));
        assert!(!text.contains("nested-account"));
        assert_eq!(value["cwd"], "/Users/[USER]/Library/agent/empty");
        assert_eq!(value["tokenUsage"]["totalTokens"], 123);
        let mut output = Vec::new();
        scrub_jsonl(std::io::Cursor::new(format!("{value}\n")), &mut output).unwrap();
        assert!(serde_json::from_slice::<Value>(&output).is_ok());
    }
}
