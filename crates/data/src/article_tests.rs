use super::*;

fn sentence(times: usize) -> String {
    "Traders weighed the report and the outlook for the next quarter. ".repeat(times)
}

fn body(paragraphs: &[String]) -> String {
    paragraphs.iter().map(|paragraph| format!("<p>{paragraph}</p>")).collect()
}

fn page(inner: &str) -> String {
    format!("<html><body>{inner}</body></html>")
}

fn three(prefix: &str) -> Vec<String> {
    (0..3).map(|index| format!("{prefix} {index}. {}", sentence(4))).collect()
}

#[test]
fn prefers_article_then_main_then_the_densest_block() {
    let inside = three("Inside article");
    let html = page(&format!("<div><p>Outside marker. {}</p></div><article>{}</article>", sentence(8), body(&inside)));
    let article = extract(&html, "https://fixture.invalid/article").unwrap();
    assert!(article.text.contains("Inside article 0."));
    assert!(!article.text.contains("Outside marker."));

    let html = page(&format!("<div><p>Outside marker. {}</p></div><main>{}</main>", sentence(8), body(&inside)));
    let article = extract(&html, "https://fixture.invalid/main").unwrap();
    assert!(article.text.contains("Inside article 2."));
    assert!(!article.text.contains("Outside marker."));

    let dense = three("Dense block");
    let html = page(&format!("<div><p>Short block marker. {}</p></div><div>{}</div>", sentence(1), body(&dense)));
    let article = extract(&html, "https://fixture.invalid/densest").unwrap();
    assert!(article.text.contains("Dense block 0."));
    assert!(article.text.contains("Dense block 2."));
    assert!(!article.text.contains("Short block marker."));
}

#[test]
fn cookie_walls_paywall_teasers_and_link_farms_are_rejected() {
    let cookie = vec![
        "We use cookies and consent tools. Please accept all cookies to continue. Subscribe or sign in to read more. "
            .repeat(6);
        3
    ];
    let html = page(&format!("<article>{}</article>", body(&cookie)));
    assert_eq!(extract(&html, "https://fixture.invalid/cookies"), Err(Reason::NoText));

    let long = three("Paywalled story");
    let json = r#"{"@context":"https://schema.org","@type":"NewsArticle","datePublished":"2026-10-26T09:00:00Z","isAccessibleForFree":false}"#;
    let html = format!(
        "<html><head><script type=\"application/ld+json\">{json}</script></head><body><article>{}</article></body></html>",
        body(&long)
    );
    assert_eq!(extract(&html, "https://fixture.invalid/paywall"), Err(Reason::Paywalled));

    let farm: Vec<String> = (0..3)
        .map(|paragraph| {
            let links: String = (0..12)
                .map(|index| {
                    format!(
                        "<a href=\"/story/{paragraph}/{index}\">Read the full market report number {index} here</a> "
                    )
                })
                .collect();
            format!("<p>{links}</p>")
        })
        .collect();
    let html = page(&format!("<article>{}</article>", farm.join("")));
    assert_eq!(extract(&html, "https://fixture.invalid/links"), Err(Reason::NoText));
}

#[test]
fn caps_at_a_sentence_end_and_records_truncation() {
    let paragraphs: Vec<String> = (0..20).map(|index| format!("Section {index}. {}", sentence(15))).collect();
    let html = page(&format!("<article>{}</article>", body(&paragraphs)));
    let article = extract(&html, "https://fixture.invalid/long").unwrap();
    assert!(article.truncated);
    assert_eq!(article.chars, article.text.chars().count());
    assert!(article.chars <= TEXT_CAP);
    assert!(article.chars > TEXT_CAP / 2);
    assert!(article.text.ends_with('.'));
}

#[test]
fn reads_json_ld_meta_and_time_dates() {
    let expected = |value: &str| chrono::DateTime::parse_from_rfc3339(value).unwrap().timestamp();
    let paragraphs = three("Dated story");

    let json = r#"{"@context":"https://schema.org","@type":"NewsArticle","datePublished":"2026-10-26T09:00:00Z","isAccessibleForFree":true}"#;
    let html = format!(
        "<html><head><script type=\"application/ld+json\">{json}</script></head><body><article>{}</article></body></html>",
        body(&paragraphs)
    );
    assert_eq!(
        extract(&html, "https://fixture.invalid/json").unwrap().published_at,
        Some(expected("2026-10-26T09:00:00Z"))
    );

    let html = format!(
        "<html><head><meta property=\"article:published_time\" content=\"2026-10-25T12:00:00Z\"></head><body><article>{}</article></body></html>",
        body(&paragraphs)
    );
    assert_eq!(
        extract(&html, "https://fixture.invalid/meta").unwrap().published_at,
        Some(expected("2026-10-25T12:00:00Z"))
    );

    let html = format!(
        "<html><body><article><time datetime=\"2026-10-24T08:00:00Z\">Yesterday</time>{}</article></body></html>",
        body(&paragraphs)
    );
    assert_eq!(
        extract(&html, "https://fixture.invalid/time").unwrap().published_at,
        Some(expected("2026-10-24T08:00:00Z"))
    );
}

#[test]
fn noarchive_meta_is_flagged_and_scripts_navigation_and_footers_are_removed() {
    let paragraphs = three("Clean story");
    let html = format!(
        "<html><head><meta name=\"robots\" content=\"noindex, nofollow, noarchive\"></head><body><article><script>secret script</script><style>.hidden{{}}</style><nav>menu navigation</nav><footer>footer text</footer>{}</article></body></html>",
        body(&paragraphs)
    );
    let article = extract(&html, "https://fixture.invalid/clean").unwrap();
    assert!(article.noarchive);
    for removed in ["secret script", "menu navigation", "footer text", ".hidden"] {
        assert!(!article.text.contains(removed), "{removed} leaked into the article text");
    }
    assert!(article.text.contains("Clean story 1."));

    let html = page(&format!("<article>{}</article>", body(&paragraphs)));
    assert!(!extract(&html, "https://fixture.invalid/plain").unwrap().noarchive);
}

#[test]
fn short_pages_and_pages_without_three_paragraphs_are_rejected() {
    let short: Vec<String> = (0..3).map(|index| format!("Short {index}. Tiny line.")).collect();
    let html = page(&format!("<article>{}</article>", body(&short)));
    assert_eq!(extract(&html, "https://fixture.invalid/short"), Err(Reason::NoText));

    let two = vec![sentence(12), sentence(12)];
    let html = page(&format!("<article>{}</article>", body(&two)));
    assert_eq!(extract(&html, "https://fixture.invalid/two"), Err(Reason::NoText));
}

#[test]
fn ordinary_links_stay_below_the_link_ratio() {
    let paragraphs: Vec<String> = (0..3)
        .map(|index| {
            format!(
                "Paragraph {index}. {} A short <a href=\"/source\">source note</a> follows. {}",
                sentence(4),
                sentence(4)
            )
        })
        .collect();
    let html = page(&format!("<article>{}</article>", body(&paragraphs)));
    let article = extract(&html, "https://fixture.invalid/ratio").unwrap();
    assert!(article.text.contains("source note"));
}

#[test]
fn n8_two_megabytes_of_nesting_is_bounded() {
    let mut html = "<article><p>x</p>".repeat((2 * 1024 * 1024) / 17);
    html.extend(std::iter::repeat_n(' ', 2 * 1024 * 1024 - html.len()));
    assert_eq!(html.len(), 2 * 1024 * 1024);
    let started = std::time::Instant::now();
    assert_eq!(extract(&html, "https://fixture.invalid/nested"), Err(Reason::NoText));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn n8_node_and_actual_tree_depth_budgets_refuse_text() {
    let wide = format!("<article>{}</article>", "<p>x</p>".repeat(MAX_NODES));
    assert_eq!(extract(&wide, "https://fixture.invalid/wide"), Err(Reason::NoText));
    // HTML ignores non-void self-closing flags and unmatched closing tags. Both must be
    // stopped before parsing, rather than relying only on a post-parse tree depth limit.
    for tag in ["<div/>", "<article></span>"] {
        let deep = tag.repeat(MAX_DEPTH + 1);
        assert_eq!(check_markup_budget(&deep), Err(Reason::NoText));
        assert_eq!(extract(&deep, "https://fixture.invalid/deep"), Err(Reason::NoText));
    }
    // This input fits the lexical tag budget but exceeds the actual DOM node budget.
    let wide = "<div>x<br>y</div>".repeat(14_000);
    assert!(check_markup_budget(&wide).is_ok());
    assert!(matches!(select_container(&Html::parse_document(&wide)), Err(Reason::NoText)));
}

#[test]
fn n8_dropped_subtrees_and_nested_candidates_share_visible_text_ranges() {
    let html = page(&format!(
        "<article><article>{}</article><aside><article>{}</article></aside></article>",
        body(&three("Visible")),
        body(&three("Invisible"))
    ));
    let article = extract(&html, "https://fixture.invalid/nested").unwrap();
    assert!(article.text.contains("Visible"));
    assert!(!article.text.contains("Invisible"));
    assert_eq!(article.paragraphs, 3);
}

#[test]
fn n8_prompt_sanitizer_removes_controls_and_format_characters_not_delimiters() {
    let mut input = String::new();
    for ch in (0..=0x9f).filter_map(char::from_u32) {
        input.push(ch);
    }
    for range in [0x200b..=0x200f, 0x202a..=0x202e, 0x2060..=0x2069, 0xfeff..=0xfeff] {
        for ch in range.filter_map(char::from_u32) {
            input.push(ch);
        }
    }
    input.push_str("Café 中文 🙂 <ARTICLE_BODY>\n");
    let expected: String = (' '..='~').collect();
    assert_eq!(sanitize_for_prompt(&input), format!("\n{expected}Café 中文 🙂 <ARTICLE_BODY>\n"));
    assert_eq!(sanitize_for_prompt(""), "");
    assert_eq!(sanitize_for_prompt("a\tb\rc\u{0085}d"), "abcd");
}

#[test]
fn review_n8_extract_is_bounded_on_nested_adversarial_html() {
    // ~9 KB (the 2 MB cap allows ~100x more): nested <article> blocks each holding one paragraph.
    let depth = 500;
    let mut html = String::from("<html><body>");
    for _ in 0..depth {
        html.push_str("<article><p>x</p>");
    }
    html.push_str("</body></html>");
    let started = std::time::Instant::now();
    let _ = extract(&html, "https://fixture.invalid/nested");
    let elapsed = started.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(2), "extract took {elapsed:?} on {} bytes", html.len());
}
