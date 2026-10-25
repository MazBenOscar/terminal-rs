use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_TEXT_BYTES: usize = 16 * 1024;
const TRUNCATION_MARKER: &str = "[truncated: tool text exceeds 16 KiB]";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Content {
    Text(String),
    ImagePng(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub content: Vec<Content>,
}

impl ToolResult {
    pub fn json(value: Value) -> Self {
        Self { success: true, content: vec![Content::Text(value.to_string())] }.capped()
    }

    pub fn error(code: &str, message: &str, field: Option<&str>) -> Self {
        let bounded = |text: &str, max: usize| {
            if text.len() <= max {
                text.to_owned()
            } else {
                let mut end = max;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{} [truncated]", &text[..end])
            }
        };
        let mut error = json!({"code":bounded(code,128),"message":bounded(message,2048)});
        if let Some(field) = field {
            error["field"] = bounded(field, 512).into();
        }
        let mut result = Self::json(json!({"error":error}));
        result.success = false;
        result
    }

    /// Cap total text, not PNG bytes. Even malformed host text is wrapped as JSON data.
    pub fn capped(mut self) -> Self {
        let mut texts = Vec::new();
        let mut bytes = 0;
        for item in &mut self.content {
            if let Content::Text(text) = item {
                if serde_json::from_str::<Value>(text).is_err() {
                    *text = json!({"text":text}).to_string();
                }
                bytes += text.len();
                texts.push(text.clone());
            }
        }
        if bytes <= MAX_TEXT_BYTES {
            return self;
        }
        let source = texts.join("\n");
        let mut end = source.len().min(MAX_TEXT_BYTES / 2);
        loop {
            while !source.is_char_boundary(end) {
                end -= 1;
            }
            let text =
                json!({"truncated":true,"marker":TRUNCATION_MARKER,"original_bytes":bytes,"preview":&source[..end]})
                    .to_string();
            if text.len() <= MAX_TEXT_BYTES {
                self.content.retain(|item| !matches!(item, Content::Text(_)));
                self.content.insert(0, Content::Text(text));
                return self;
            }
            end /= 2;
        }
    }
}

/// Shared redaction for tool diagnostics and agent transcripts.
pub fn scrub_text(text: &str) -> String {
    use regex::Regex;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_cap_is_valid_json_even_for_unicode_and_escapes() {
        for source in ["🦀\"\n".repeat(20_000), "\0".repeat(30_000)] {
            let result = ToolResult {
                success: true,
                content: vec![Content::Text(source.clone()), Content::Text(source), Content::ImagePng(vec![1, 2])],
            }
            .capped();
            let mut bytes = 0;
            for content in &result.content {
                if let Content::Text(text) = content {
                    bytes += text.len();
                    let value: Value = serde_json::from_str(text).unwrap();
                    assert_eq!(value["truncated"], true);
                    assert!(value["marker"].as_str().unwrap().contains("truncated"));
                }
            }
            assert!(bytes <= MAX_TEXT_BYTES);
            assert!(matches!(result.content.last(), Some(Content::ImagePng(_))));
        }
    }
}
