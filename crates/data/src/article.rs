//! Deterministic article extraction for publisher pages. No network, no provider data.
//!
//! The extractor is intentionally conservative: a page must look like a real article before
//! its text is offered to the analyzer. Every failure maps to a footer reason, and the caller
//! falls back to the headline and summary.

#[cfg(test)]
#[path = "article_tests.rs"]
mod tests;

use scraper::{ElementRef, Html, Selector};

/// The text cap for one analysis. Cuts at a sentence end and records truncation.
const TEXT_CAP: usize = 12_000;
const MIN_CHARS: usize = 600;
const MIN_PARAGRAPHS: usize = 3;

/// Tags removed before paragraph text is collected. figure and figcaption are both removed so
/// captions never masquerade as body paragraphs.
const DROPPED: [&str; 11] =
    ["script", "style", "noscript", "nav", "header", "footer", "aside", "form", "figure", "figcaption", "iframe"];

/// Wording that indicates a consent wall rather than an article.
const CONSENT: [&str; 11] = [
    "cookie",
    "consent",
    "subscribe",
    "subscriber",
    "sign in",
    "sign-in",
    "log in",
    "login",
    "accept all",
    "create an account",
    "privacy policy",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    NoText,
    Paywalled,
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoText => f.write_str("no article text"),
            Self::Paywalled => f.write_str("paywalled"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Article {
    pub text: String,
    /// Characters in the stored text, after the 12,000 character cap.
    pub chars: usize,
    pub truncated: bool,
    pub published_at: Option<i64>,
    /// The page asks not to be archived; use the text for this analysis but do not store it.
    /// Combine with the fetch response's X-Robots-Tag value, which this HTML-only extractor
    /// cannot see: `article.noarchive || fetched.noarchive`.
    pub noarchive: bool,
    pub paragraphs: usize,
}

/// Extract article text and metadata from one decoded HTML page. The URL is reserved for
/// resolving publisher-specific link policy; text extraction itself never touches it.
pub fn extract(html: &str, _url: &str) -> Result<Article, Reason> {
    check_markup_budget(html)?;
    let document = Html::parse_document(html);
    let selected = select_container(&document)?;
    let metadata = metadata(&document);
    if metadata.paywalled {
        return Err(Reason::Paywalled);
    }
    let selected = selected.ok_or(Reason::NoText)?;
    let paragraphs = selected.paragraphs;
    let text = paragraphs.join("\n\n");
    let chars = text.chars().count();
    if chars < MIN_CHARS || paragraphs.len() < MIN_PARAGRAPHS {
        return Err(Reason::NoText);
    }
    let links = selected.links;
    // Links must make up less than 0.35 of the text. Navigation is already removed above.
    if links * 100 >= chars * 35 {
        return Err(Reason::NoText);
    }
    if consent_wall(&text) {
        return Err(Reason::NoText);
    }
    let (text, truncated) = cap_text(&text, TEXT_CAP);
    Ok(Article {
        chars: text.chars().count(),
        text,
        truncated,
        published_at: metadata.published,
        noarchive: metadata.noarchive,
        paragraphs: paragraphs.len(),
    })
}

#[derive(Default)]
struct Metadata {
    published: Option<i64>,
    paywalled: bool,
    noarchive: bool,
}

fn metadata(document: &Html) -> Metadata {
    let mut metadata = Metadata::default();
    if let Ok(selector) = Selector::parse("meta") {
        for element in document.select(&selector) {
            let Some(name) = element.value().attr("name").or_else(|| element.value().attr("property")) else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            let content = element.value().attr("content").unwrap_or("");
            match name.as_str() {
                "article:published_time" => {
                    metadata.published = metadata.published.or_else(|| parse_date(content));
                }
                "robots" | "googlebot" if content.to_ascii_lowercase().contains("noarchive") => {
                    metadata.noarchive = true;
                }
                _ => {}
            }
        }
    }
    if let Ok(selector) = Selector::parse("script") {
        for script in document.select(&selector) {
            let is_json_ld = script.value().attr("type").is_some_and(|kind| {
                kind.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/ld+json")
            });
            if !is_json_ld {
                continue;
            }
            let body: String = script.text().collect();
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
                scan_json_ld(&value, &mut metadata);
            }
        }
    }
    if metadata.published.is_none()
        && let Ok(selector) = Selector::parse("time[datetime]")
    {
        for time in document.select(&selector) {
            if let Some(published) = time.value().attr("datetime").and_then(parse_date) {
                metadata.published = Some(published);
                break;
            }
        }
    }
    metadata
}

fn scan_json_ld(value: &serde_json::Value, metadata: &mut Metadata) {
    match value {
        serde_json::Value::Object(map) => {
            if metadata.published.is_none()
                && let Some(published) = map.get("datePublished").and_then(serde_json::Value::as_str)
            {
                metadata.published = parse_date(published);
            }
            if let Some(free) = map.get("isAccessibleForFree") {
                let closed = matches!(free, serde_json::Value::Bool(false))
                    || free.as_str().is_some_and(|value| value.eq_ignore_ascii_case("false"));
                if closed {
                    metadata.paywalled = true;
                }
            }
            for value in map.values() {
                scan_json_ld(value, metadata);
            }
        }
        serde_json::Value::Array(items) => {
            for value in items {
                scan_json_ld(value, metadata);
            }
        }
        _ => {}
    }
}

fn parse_date(value: &str) -> Option<i64> {
    let value = value.trim();
    chrono::DateTime::parse_from_rfc3339(value).ok().map(|at| at.timestamp()).or_else(|| {
        chrono::NaiveDate::parse_from_str(value.get(..10)?, "%Y-%m-%d")
            .ok()
            .and_then(|date| date.and_hms_opt(0, 0, 0))
            .map(|at| at.and_utc().timestamp())
    })
}

const MAX_NODES: usize = 50_000;
const MAX_DEPTH: usize = 256;

/// Refuse excessive markup before html5ever builds its open-element stack (which itself can
/// become expensive on deep input). This conservative lexical budget may reject malformed
/// pages; the DOM pass below enforces the same limits on the actual tree.
fn check_markup_budget(html: &str) -> Result<(), Reason> {
    let mut open = Vec::new();
    let mut tags = 0_usize;
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        rest = &rest[at + 1..];
        tags += 1;
        if tags > MAX_NODES {
            return Err(Reason::NoText);
        }
        if let Some(comment) = rest.strip_prefix("!--") {
            let Some(end) = comment.find("-->") else { break };
            rest = &comment[end + 3..];
            continue;
        }
        let mut quote = None;
        let end = rest.char_indices().find_map(|(i, ch)| {
            if let Some(q) = quote {
                if ch == q {
                    quote = None;
                }
            } else if matches!(ch, '\'' | '"') {
                quote = Some(ch);
            } else if ch == '>' {
                return Some(i);
            }
            None
        });
        let Some(end) = end else { break };
        let tag = &rest[..end];
        let name = tag.split_whitespace().next().unwrap_or("").trim_end_matches('/').to_ascii_lowercase();
        if let Some(closing) = name.strip_prefix('/') {
            // Unknown/misnested end tags cannot buy more nesting budget. In particular,
            // html5ever ignores </span> in <article></span>, so a mere depth decrement
            // here would permit an arbitrarily deep parser stack.
            if open.last().is_some_and(|name| name == closing) {
                open.pop();
            }
        } else if !tag.starts_with(['!', '?'])
            && !matches!(
                name.as_str(),
                "area"
                    | "base"
                    | "br"
                    | "col"
                    | "embed"
                    | "hr"
                    | "img"
                    | "input"
                    | "link"
                    | "meta"
                    | "param"
                    | "source"
                    | "track"
                    | "wbr"
            )
        {
            // HTML ignores the self-closing flag on non-void HTML elements.
            open.push(name);
            if open.len() > MAX_DEPTH {
                return Err(Reason::NoText);
            }
        }
        rest = &rest[end + 1..];
    }
    Ok(())
}

#[derive(Default)]
struct NodeText {
    start: usize,
    end: usize,
    char_start: usize,
    chars: usize,
    paragraphs: usize,
    direct_score: usize,
    links: usize,
}

struct Selected {
    paragraphs: Vec<String>,
    links: usize,
}

/// One DFS, with no ancestor scans or per-candidate subtree scans. Visible text is normalized
/// once into a shared buffer; each node id memoizes its range, length and aggregate scores.
fn select_container(document: &Html) -> Result<Option<Selected>, Reason> {
    let mut memo = std::collections::HashMap::new();
    let mut events = vec![(document.tree.root(), false, 0_usize)];
    let mut frames: Vec<NodeText> = Vec::new();
    let mut candidates = [None; 4];
    let (mut visited, mut dropped_depth, mut chars) = (0, 0, 0_usize);
    let mut text = String::new();
    while let Some((node, exiting, depth)) = events.pop() {
        let element = ElementRef::wrap(node);
        let is_dropped = element.is_some_and(|e| DROPPED.contains(&e.value().name()));
        if !exiting {
            visited += 1;
            if visited > MAX_NODES || depth > MAX_DEPTH {
                return Err(Reason::NoText);
            }
            if is_dropped {
                dropped_depth += 1;
            }
            frames.push(NodeText { start: text.len(), char_start: chars, ..Default::default() });
            if dropped_depth == 0 {
                if let Some(value) = node.value().as_text() {
                    for ch in value.chars() {
                        let ch = if ch.is_whitespace() { ' ' } else { ch };
                        if ch != ' ' || !text.ends_with(' ') {
                            text.push(ch);
                            chars += 1;
                        }
                    }
                } else if element.is_some_and(|e| {
                    matches!(
                        e.value().name(),
                        "p" | "div" | "section" | "li" | "br" | "tr" | "td" | "h1" | "h2" | "h3" | "blockquote"
                    )
                }) && !text.ends_with(' ')
                {
                    text.push(' ');
                    chars += 1;
                }
            }
            events.push((node, true, depth));
            for child in node.children().rev() {
                events.push((child, false, depth + 1));
            }
        } else {
            let mut info = frames.pop().ok_or(Reason::NoText)?;
            info.end = text.len();
            info.chars = chars - info.char_start;
            let visible = dropped_depth == 0;
            if visible && let Some(element) = element {
                let slice = &text[info.start..info.end];
                info.chars = info.chars.saturating_sub(usize::from(slice.starts_with(' ')));
                info.chars = info.chars.saturating_sub(usize::from(slice.len() > 1 && slice.ends_with(' ')));
                let name = element.value().name();
                if name == "p" && info.chars > 0 {
                    info.paragraphs += 1;
                }
                if name == "a" {
                    info.links += info.chars;
                }
                let rank = if name == "article" {
                    Some(0)
                } else if element.value().attr("itemprop") == Some("articleBody") {
                    Some(1)
                } else if name == "main" {
                    Some(2)
                } else if matches!(name, "div" | "section" | "td" | "body") {
                    Some(3)
                } else {
                    None
                };
                if let Some(rank) = rank {
                    let score = if rank == 3 { info.direct_score } else { info.chars };
                    if score > 0 && info.paragraphs > 0 && candidates[rank].is_none_or(|(_, best)| score > best) {
                        candidates[rank] = Some((element, score));
                    }
                }
                if let Some(parent) = frames.last_mut() {
                    parent.paragraphs += info.paragraphs;
                    parent.links += info.links;
                    if name == "p" {
                        parent.direct_score += info.chars;
                    }
                }
                memo.insert(node.id(), info);
            }
            if is_dropped {
                dropped_depth -= 1;
            }
        }
    }
    let Some((container, _)) = candidates.into_iter().flatten().next() else {
        return Ok(None);
    };
    let links = memo.get(&container.id()).ok_or(Reason::NoText)?.links;
    let paragraphs = container
        .descendants()
        .filter_map(|node| {
            let element = ElementRef::wrap(node)?;
            if element.value().name() != "p" {
                return None;
            }
            let info = memo.get(&node.id())?;
            (info.chars > 0).then(|| text[info.start..info.end].trim().to_owned())
        })
        .collect();
    Ok(Some(Selected { paragraphs, links }))
}

/// Remove invisible controls before later prompt escaping. This is not HTML/XML escaping;
/// callers must still escape delimiters and label the result as untrusted article data.
pub fn sanitize_for_prompt(text: &str) -> String {
    text.chars()
        .filter(|&ch| {
            ch == '\n'
                || !matches!(ch,
        '\u{0000}'..='\u{001f}' | '\u{007f}'..='\u{009f}' |
        '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' |
        '\u{2060}'..='\u{2069}' | '\u{feff}')
        })
        .collect()
}

/// The text is "mainly" consent wording when at least three markers appear and they make up
/// at least five percent of the words. One or two mentions in a real article do not trip this.
fn consent_wall(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let words = lower.split_whitespace().count().max(1);
    let hits: usize = CONSENT.iter().map(|marker| lower.matches(marker).count()).sum();
    hits >= 3 && hits * 20 >= words
}

fn cap_text(text: &str, limit: usize) -> (String, bool) {
    if text.chars().count() <= limit {
        return (text.to_owned(), false);
    }
    let mut cut: String = text.chars().take(limit).collect();
    let boundary = cut.rfind(['.', '!', '?']).filter(|index| *index > limit / 2).or_else(|| cut.rfind(' '));
    if let Some(index) = boundary {
        cut.truncate(index + 1);
    }
    (cut.trim_end().to_owned(), true)
}
