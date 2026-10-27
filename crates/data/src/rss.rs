//! Minimal RSS 2.0 reader for headline feeds (title, link, pubDate). Used for crypto
//! news, which Yahoo's search endpoint does not index.

use anyhow::Result;

use crate::NewsItem;

/// CoinDesk's public feed.
pub const COINDESK: &str = "https://www.coindesk.com/arc/outboundfeeds/rss/";

fn text(raw: &str) -> String {
    let raw = raw.strip_prefix("<![CDATA[").and_then(|r| r.strip_suffix("]]>")).unwrap_or(raw);
    raw.trim().to_string()
}

/// Parse `<item>`s into headlines (newest first), tagging the feed's publisher.
pub fn parse_rss(xml: &str, publisher: &str) -> Result<Vec<NewsItem>> {
    let feed = feed_rs::parser::parse(xml.as_bytes())?;
    let mut out = Vec::new();
    for entry in feed.entries {
        let Some(title) = entry.title else { continue };
        let Some(link) = entry.links.iter().find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate")) else {
            continue;
        };
        let author = entry.authors.first().map(|a| a.name.as_str()).filter(|a| !a.is_empty());
        out.push(NewsItem {
            title: text(&title.content),
            publisher: author.map_or_else(|| publisher.to_string(), |a| format!("{publisher} · {a}")),
            link: link.href.clone(),
            ts: entry.published.or(entry.updated).map_or(0, |d| d.timestamp()),
            tickers: Vec::new(),
        });
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.ts));
    Ok(out)
}

/// Keep headlines mentioning any of `words` (case-insensitive); no match returns empty.
pub fn filter_relevant(items: Vec<NewsItem>, words: &[&str]) -> Vec<NewsItem> {
    let hit = |n: &NewsItem| {
        let t = n.title.to_lowercase();
        words.iter().any(|w| t.contains(&w.to_lowercase()))
    };
    items.into_iter().filter(hit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_coindesk_fixture() {
        let n = parse_rss(include_str!("../tests/fixtures/coindesk_rss.xml"), "CoinDesk").unwrap();
        assert_eq!(n.len(), 5);
        assert!(n.iter().all(|x| x.link.starts_with("https://www.coindesk.com/") && x.ts > 1_700_000_000));
        assert!(n.iter().all(|x| !x.title.contains("CDATA") && x.publisher.starts_with("CoinDesk")));
        assert!(n.windows(2).all(|w| w[0].ts >= w[1].ts));
        assert!(parse_rss("<html></html>", "x").is_err());
    }

    #[test]
    fn parses_atom_alternate_links_and_dates() {
        let items = parse_rss(include_str!("../tests/fixtures/news_atom.xml"), "Fixture").unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Bitcoin & Ether");
        assert_eq!(items[0].link, "https://example.test/story/1");
        assert!(items[0].publisher.contains("Fixture Author"));
        assert!(items[0].ts > items[1].ts);
        assert!(filter_relevant(items, &["solana"]).is_empty());
    }

    #[test]
    fn entities_and_filtering() {
        let xml = "<rss version=\"2.0\"><channel><item><title>Bitcoin &amp; Ether rally</title><link>https://a/1</link>\
                   <pubDate>Mon, 12 Oct 2026 12:09:09 +0000</pubDate></item><item><title>Fed holds</title>\
                   <link>https://a/2</link></item></channel></rss>";
        let n = parse_rss(xml, "T").unwrap();
        assert_eq!(n[0].title, "Bitcoin & Ether rally");
        assert_eq!(filter_relevant(n.clone(), &["bitcoin"]).len(), 1);
        assert!(filter_relevant(n, &["solana"]).is_empty());
    }
}
