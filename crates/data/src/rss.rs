//! Minimal RSS 2.0 reader for headline feeds (title, link, pubDate). Used for crypto
//! news, which Yahoo's search endpoint does not index.

use anyhow::Result;

use crate::NewsItem;

/// CoinDesk's public feed.
pub const COINDESK: &str = "https://www.coindesk.com/arc/outboundfeeds/rss/";

/// One HTML pass strips markup and decodes entities, without interpreting the
/// decoded output again. Bound stored/displayed text, not bytes, for Unicode.
fn text(raw: &str, limit: usize) -> String {
    let html = scraper::Html::parse_fragment(raw);
    let mut plain = String::new();
    for node in html.tree.root().descendants() {
        if node.ancestors().any(|parent| {
            parent.value().as_element().is_some_and(|e| matches!(e.name(), "script" | "style" | "template"))
        }) {
            continue;
        }
        if let Some(text) = node.value().as_text() {
            plain.push_str(text);
        } else if node
            .value()
            .as_element()
            .is_some_and(|e| matches!(e.name(), "br" | "p" | "div" | "li" | "section" | "article" | "h1" | "h2" | "h3"))
        {
            plain.push(' ');
        }
    }
    let cleaned: String = plain.chars().filter(|c| !c.is_control() || c.is_whitespace()).collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(limit).collect()
}

/// Parse `<item>`s into headlines (newest first), tagging the feed's publisher.
pub fn parse_rss(xml: &str, publisher: &str) -> Result<Vec<NewsItem>> {
    Ok(parse_entries(xml, publisher)?.into_iter().map(|(item, _, _)| item).collect())
}
/// Preserve GUID and summary for the built-in registry without changing the
/// established NewsItem interface used by Yahoo and CoinDesk.
pub(crate) fn parse_entries(xml: &str, publisher: &str) -> Result<Vec<(NewsItem, String, String)>> {
    let feed = feed_rs::parser::parse(xml.as_bytes())?;
    let mut out = Vec::new();
    for entry in feed.entries {
        let Some(title) = entry.title else { continue };
        let link = entry
            .links
            .iter()
            .find(|l| {
                l.rel.as_deref().is_none_or(|r| r == "alternate")
                    && reqwest::Url::parse(&l.href)
                        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
            })
            .map(|l| l.href.clone())
            .unwrap_or_default();
        if link.is_empty() {
            continue;
        }
        let author = entry.authors.first().map(|a| a.name.as_str()).filter(|a| !a.is_empty());
        out.push((
            NewsItem {
                title: text(&title.content, 300),
                publisher: author.map_or_else(|| publisher.to_string(), |a| format!("{publisher} · {a}")),
                link,
                ts: entry.published.or(entry.updated).map_or(0, |d| d.timestamp()),
                tickers: Vec::new(),
            },
            entry.id,
            entry.summary.map(|s| text(&s.content, 600)).unwrap_or_default(),
        ));
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.0.ts));
    Ok(out)
}

/// Keep headlines mentioning any of `words` (case-insensitive); no match returns empty.
pub fn filter_relevant(items: Vec<NewsItem>, words: &[&str]) -> Vec<NewsItem> {
    let hit = |n: &NewsItem| {
        let tokens = crate::news_intelligence::words(&n.title);
        words.iter().any(|word| {
            let word = word.to_lowercase();
            let aliases: &[&str] = match word.as_str() {
                "eth" => &["eth", "ethereum", "ether"],
                "sol" => &["sol", "solana"],
                "ada" => &["ada", "cardano"],
                _ => &[&word],
            };
            // Possessives are already split by the shared tokenizer; admit only
            // a whole-token plural, never a prefix or an interior substring.
            aliases
                .iter()
                .any(|alias| tokens.iter().any(|token| token == alias || token.strip_suffix('s') == Some(*alias)))
        })
    };
    items.into_iter().filter(hit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn n6_plain_text_is_bounded_and_decoded_only_once() {
        assert_eq!(text("<p>A&nbsp; &amp; B</p><p>C\n D\u{7}</p><script>hidden</script>", 600), "A & B C D");
        assert_eq!(text("&amp;lt;b&amp;gt;", 600), "&lt;b&gt;");
        assert_eq!(text(&"é".repeat(700), 600).chars().count(), 600);
        let xml = format!(
            "<rss version='2.0'><channel><title>Fixture</title><item><title><![CDATA[<b>{}</b>]]></title><link>https://example.test/story</link><description><![CDATA[<div data-markup='needle'>Visible &amp; clear</div>]]></description></item></channel></rss>",
            "é".repeat(400)
        );
        let entries = parse_entries(&xml, "Fixture").unwrap();
        assert_eq!(entries[0].0.title.chars().count(), 300);
        assert_eq!(entries[0].2, "Visible & clear");
        assert!(!entries[0].2.contains("needle"));
        let atom = r#"<feed xmlns="http://www.w3.org/2005/Atom"><id>fixture</id><title>Fixture</title><updated>2026-10-28T12:00:00Z</updated><entry><id>story</id><title type="html">&lt;b&gt;Plain &amp;amp; title&lt;/b&gt;</title><link href="https://example.test/atom"/><summary type="html">&lt;p data-hidden="needle"&gt;Visible &amp;amp; clear&lt;/p&gt;</summary><updated>2026-10-28T12:00:00Z</updated></entry></feed>"#;
        let entries = parse_entries(atom, "Fixture").unwrap();
        assert_eq!(entries[0].0.title, "Plain & title");
        assert_eq!(entries[0].2, "Visible & clear");
    }
    #[test]
    fn n6_all_rss_providers_drop_unopenable_links_even_with_guids() {
        for publisher in ["Yahoo", "CoinDesk", "BBC"] {
            let mut xml = String::from("<rss version='2.0'><channel><title>Fixture</title>");
            for (i, link) in [
                "",
                "javascript:alert(1)",
                "file:///tmp/story",
                "/relative",
                "ftp://example.test/story",
                "https://example.test/story",
                "http://example.test/story",
            ]
            .iter()
            .enumerate()
            {
                xml.push_str(&format!("<item><title>Story {i}</title><guid isPermaLink='false'>opaque-{i}</guid><link>{link}</link></item>"));
            }
            xml.push_str("</channel></rss>");
            let items = parse_rss(&xml, publisher).unwrap();
            assert_eq!(items.len(), 2, "{publisher}");
            assert!(items.iter().all(|item| item.link.starts_with("http")));
        }
    }
    #[test]
    fn review_n5a_token_filter_keeps_legitimate_coin_mentions() {
        let hit = |title: &str, words: &[&str]| {
            filter_relevant(
                vec![NewsItem {
                    title: title.into(),
                    link: String::new(),
                    publisher: String::new(),
                    ts: 0,
                    tickers: vec![],
                }],
                words,
            )
            .len()
        };
        for (title, words) in [
            ("BTC/USD hits record", &["BTC", "Bitcoin"][..]),
            ("Bitcoin's halving explained", &["BTC", "Bitcoin"]),
            ("#Bitcoin-led rally lifts miners", &["BTC", "Bitcoin"]),
            ("BITCOIN ETFS see inflows", &["BTC", "Bitcoin"]),
            ("Ether slides as ETH-USD breaks support", &["ETH", "Ethereum"]),
            ("Solana’s validators upgrade", &["SOL", "Solana"]),
            ("Cardano founder speaks", &["ADA", "Cardano"]),
            ("XRP jumps 10%", &["XRP", "XRP"]),
            ("1INCH token unlock", &["1INCH", "1inch"]),
        ] {
            assert_eq!(hit(title, words), 1, "{title}");
        }
        for (title, words) in [
            ("Canadian banks rally", &["ADA", "Cardano"][..]),
            ("Insiders sold shares", &["SOL", "Solana"]),
            ("Ethics probe widens", &["ETH", "Ethereum"]),
        ] {
            assert_eq!(hit(title, words), 0, "{title}");
        }
    }
    #[test]
    fn token_exact_coin_filter() {
        for (title, word, count) in [
            ("something", "ETH", 0),
            ("sold", "SOL", 0),
            ("Canada", "ADA", 0),
            ("Ethereum sell-off", "ETH", 1),
            ("Ethereum’s rally", "ETH", 1),
            ("Bitcoins move", "bitcoin", 1),
        ] {
            let items = vec![NewsItem {
                title: title.into(),
                link: String::new(),
                publisher: String::new(),
                ts: 0,
                tickers: vec![],
            }];
            assert_eq!(filter_relevant(items, &[word]).len(), count, "{title}");
        }
    }
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
