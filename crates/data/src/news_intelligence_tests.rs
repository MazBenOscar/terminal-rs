use super::*;
use crate::{
    NewsItem,
    news_store::{NewsStore, Query},
};
fn story(id: usize, title: &str, ts: i64, source: &str) -> Story {
    Story::new(
        NewsItem {
            title: title.into(),
            link: format!("https://fixture.test/{id}"),
            publisher: source.into(),
            ts,
            tickers: vec![],
        },
        source,
    )
}
fn subject_names() -> NameIndex {
    let companies = [
        (1, "FROG", "JFrog Ltd"),
        (2, "PLTR", "Palantir Technologies Inc."),
        (3, "TGT", "Target Corp"),
        (4, "SHEL", "Shell Plc"),
        (5, "XYZ", "Block Inc"),
        (6, "GAP", "Gap Inc"),
        (7, "F", "Ford Motor Co"),
        (8, "INTC", "Intel Corp"),
        (9, "ORCL", "Oracle Corp"),
        (10, "GOOGL", "Alphabet Inc. Class A"),
        (10, "GOOG", "Alphabet Inc. Class C"),
        (11, "AG", "First Majestic Silver Corp"),
        (12, "FSLR", "First Solar, Inc."),
        (13, "FIRST", "First Corp"),
        (14, "WXYZW", "Distinctive Holdings Inc"),
        (14, "DIST", "Distinctive Holdings Inc"),
    ]
    .map(|(cik, ticker, title)| (cik, ticker.into(), title.into()));
    NameIndex::new(&companies)
}
#[test]
fn subjects_real_headline_table_and_false_positive_guards() {
    let index = subject_names();
    // Expected first symbols, in textual order, never association order.
    for (headline, associated, expected) in [
        ("Can JFrog Stock Keep Its Lead Over Its Industry?", "MSFT", "FROG"),
        ("Investor Who Called the AI Boom Predicts a Bitcoin Crash", "NVDA", "BTCUSD"),
        ("Nvidia unveils a new AI chip", "MSFT", "NVDA"),
        ("Broadcom wins AI networking orders", "", "AVGO"),
        ("Microsoft expands cloud capacity", "", "MSFT"),
        ("Palantir Technologies raises outlook", "", "PLTR"),
        ("Palantir Technologies Inc. signs deal", "", "PLTR"),
        ("JFrog's cloud revenue accelerates", "", "FROG"),
        ("Apple stock gains after launch", "", "AAPL"),
        ("Apple shares gain in early trading", "", "AAPL"),
        ("Apple's new iPhone goes on sale", "", "AAPL"),
        ("Apple’s CEO talks tariffs", "", "AAPL"),
        ("Apple (AAPL) launches new iPhone", "", "AAPL"),
        ("Apple Inc unveils new iPhone", "", "AAPL"),
        ("Apple earnings top estimates", "", "AAPL"),
        ("Apple CEO visits China", "", "AAPL"),
        ("Apple launches new iPhone", "AAPL", "AAPL"),
        ("Amazon shares climb after earnings", "", "AMZN"),
        ("Amazon.com opens another warehouse", "AMZN", "AMZN"),
        ("Meta Platforms expands data centers", "", "META"),
        ("Meta stock rises on ad growth", "", "META"),
        ("Meta announces new AI tools", "META", "META"),
        ("Oracle earnings beat forecasts", "", "ORCL"),
        ("Visa shares rally as spending grows", "", "V"),
        ("Target stock slumps on weak outlook", "", "TGT"),
        ("Shell earnings fall on lower oil prices", "", "SHEL"),
        ("Block stock sets record", "", "XYZ"),
        ("Block sets record", "XYZ", "XYZ"),
        ("Gap CEO outlines turnaround", "", "GAP"),
        ("Ford Motor recalls vehicles", "", "F"),
        ("Intel wins chip subsidy", "", "INTC"),
        ("Alphabet's earnings beat estimates", "", "GOOGL"),
        ("Google unveils AI search features", "", "GOOGL"),
        ("First Solar raises revenue guidance", "", "FSLR"),
        ("First Majestic Silver expands mine", "", "AG"),
        ("Bitcoin and Ethereum surge", "", "BTCUSD ETHUSD"),
        ("Ether rallies after ETF approval", "", "ETHUSD"),
        ("BTC rises as ETH falls", "", "BTCUSD ETHUSD"),
        ("Solana outpaces Bitcoin", "", "SOLUSD BTCUSD"),
        ("SOL token jumps on crypto demand", "", "SOLUSD"),
        ("XRP rises after court ruling", "", "XRPUSD"),
        ("Ripple expands crypto payments", "", "XRPUSD"),
        ("Dogecoin and DOGE fans celebrate", "", "DOGEUSD"),
        ("Cardano rallies as ADA volumes rise", "", "ADAUSD"),
        ("$frog outpaces $NVDA", "MSFT", "FROG NVDA"),
        ("NVDA and MSFT rally on AI spending", "", "NVDA MSFT"),
        ("Tesla and Netflix report next week", "", "TSLA NFLX"),
        ("Berkshire Hathaway buys more shares", "", "BRK.B"),
        ("Fed misses inflation target", "", ""),
        ("Apple pie recipes", "MSFT", ""),
        ("Block sets record", "", ""),
        ("Shell game in Washington", "", ""),
        ("Visa rules for H-1B", "", ""),
        ("Gap between rich and poor", "", ""),
        ("Meta-analysis shows better outcomes", "", ""),
        ("Meta-analysis shows better outcomes", "META", ""),
        ("META-analysis shows better outcomes", "", ""),
        ("Ripple effects spread across markets", "", ""),
        ("RIPPLE effects spread across markets", "", ""),
        ("Palantir wins a defense contract", "", "PLTR"),
        ("Ford shares rise on EV demand", "", "F"),
        ("Nvidia-like demand boosts JFrog", "MSFT", "NVDA FROG"),
        ("$BTCUSD outpaces $ETHUSD", "", "BTCUSD ETHUSD"),
        ("Sol shines in a new astronomy exhibit", "", ""),
        ("Ada writes a book", "", ""),
        ("Fed CEO sees AI GDP CPI SEC FDA EPS growth", "", ""),
        ("Economists see a V-shaped recovery", "", ""),
        ("NVDA AND MSFT RALLY", "", ""),
        ("$NVDA AND MSFT RALLY", "", "NVDA"),
        ("A first look at inflation", "", ""),
    ] {
        let provider = if associated.is_empty() { vec![] } else { vec![format!("{associated} US Equity")] };
        let subjects = index.subjects(headline, "", &provider);
        let actual: Vec<_> = subjects.iter().map(|s| s.split_whitespace().next().unwrap()).collect();
        assert_eq!(actual, expected.split_whitespace().collect::<Vec<_>>(), "{headline} / {associated}");
    }
}
#[test]
fn names_normalize_legal_suffixes_primary_classes_and_order() {
    for (title, normalized) in [
        ("JFrog Ltd", "jfrog"),
        ("NVIDIA CORP", "nvidia"),
        ("Alphabet Inc.", "alphabet"),
        ("Meta Platforms", "meta platforms"),
        ("Acme Holdings Group Class B /DE/", "acme"),
        ("Acme & Co N.V.", "acme"),
        ("Acme S.A.", "acme"),
        ("Acme AG SE", "acme"),
        ("Acme Corporation Limited Plc", "acme"),
    ] {
        assert_eq!(company_name(title), normalized, "{title}");
    }
    let index = subject_names();
    assert_eq!(
        index.subjects("Nvidia buys JFrog", "Microsoft partners with JFrog and Bitcoin", &["MSFT US Equity".into()]),
        vec!["NVDA US Equity", "FROG US Equity", "MSFT US Equity", "BTCUSD Curncy"]
    );
    assert_eq!(index.subjects("Alphabet Inc. unveils AI", "", &[]), vec!["GOOGL US Equity"]);
    assert_eq!(index.subjects("Meta Platforms grows", "", &[]), vec!["META US Equity"]);
    assert_eq!(index.subjects("Block sets record", "$XYZ gains", &[]), vec!["XYZ US Equity"]);
    assert_eq!(index.subjects("Distinctive expands", "", &[]), vec!["DIST US Equity"]);
    assert!(index.subjects("A chip company gains", "", &["NVDA US Equity".into()]).is_empty());
    assert!(default_names().subjects("JFrog rises", "", &["MSFT US Equity".into()]).is_empty());
}
#[test]
fn ten_thousand_company_index_and_two_thousand_headlines_timing() {
    let mut companies: Vec<_> =
        (0..10_000).map(|i| (i, format!("Z{i}"), format!("Distinctive{i} Holdings Inc /DE/"))).collect();
    companies.push((20_000, "FROG".into(), "JFrog Ltd".into()));
    let start = std::time::Instant::now();
    let index = NameIndex::new(&companies);
    let indexing = start.elapsed();
    let headlines: Vec<_> =
        (0..2000).map(|i| format!("JFrog and Distinctive{i} shares rise as Bitcoin demand accelerates")).collect();
    let start = std::time::Instant::now();
    for headline in &headlines {
        assert_eq!(index.subjects(headline, "Nvidia reports earnings", &["MSFT US Equity".into()]).len(), 4);
    }
    let matching = start.elapsed();
    eprintln!("SUBJECT TIMINGS: index 10,001 companies {indexing:?}; match 2,000 headlines + summaries {matching:?}");
    assert!(matching < std::time::Duration::from_millis(300), "matching took {matching:?}");
}
#[test]
fn subject_disjointness_overrides_shared_finnhub_association() {
    let mut a = story(1, "JFrog beats quarterly earnings estimates", 100, "Reuters");
    let mut b = story(2, "Nvidia beats quarterly earnings estimates", 200, "Reuters");
    a.item.tickers = vec!["MSFT US Equity".into()];
    b.item.tickers = a.item.tickers.clone();
    a.subjects = vec!["FROG US Equity".into()];
    b.subjects = vec!["NVDA US Equity".into()];
    assert_eq!(clusters(&[a.clone(), b.clone()]).len(), 2);
    b.subjects = a.subjects.clone();
    b.item.tickers = vec!["AAPL US Equity".into()];
    assert_eq!(clusters(&[a, b]).len(), 1);
}
#[test]
fn cross_source_fixture_leads_union_and_order_stability() {
    let mut a = story(1, "Nvidia to acquire chip startup in major deal", 100, "Yahoo");
    a.item.tickers = vec!["NVDA US Equity".into()];
    let mut b = story(2, "Nvidia acquires chip startup: major deal announced", 200, "Reuters");
    // A secondary ticker may be unioned only with an overlapping company set.
    b.item.tickers = vec!["AMD US Equity".into(), "NVDA US Equity".into()];
    let c = story(3, "Nvidia chip startup deal announced", 250, "Reuters");
    let items = vec![a.clone(), b.clone(), c.clone()];
    let result = clusters(&items);
    assert_eq!(result.len(), 1);
    let cluster = &result[0];
    assert_eq!(cluster.lead.id, b.id);
    assert_eq!(cluster.member_ids.len(), 3);
    assert_eq!(cluster.sources, vec!["Reuters", "Yahoo"]);
    assert_eq!(cluster.tickers, vec!["AMD US Equity", "NVDA US Equity"]);
    assert!(cluster.tags.contains(&"M&A".into()));
    assert_eq!(cluster.newest, 250);
    assert_eq!(clusters(&[c, b, a]), result);
}
#[test]
fn jaccard_threshold_edges_empty_tokens_and_suffixes() {
    let set = |s| headline_tokens(s);
    assert!(similar(&set("alpha beta"), &set("alpha gamma delta"), false)); // 1/4
    assert!(!similar(&set("alpha beta"), &set("alpha gamma delta epsilon"), false)); // 1/5
    assert!(similar(&set("alpha beta"), &set("alpha gamma delta epsilon"), true));
    assert!(!similar(&set("alpha beta"), &set("alpha gamma delta epsilon zeta"), true));
    assert!(!similar(&set("the and"), &set("to in"), true));
    assert_eq!(set("The NVDA.US, earnings!"), set("NVDA earnings"));
}
#[test]
fn categories_and_entire_twenty_four_hour_span() {
    let a = story(1, "alpha beta", 100, "Yahoo");
    let mut b = story(2, "alpha gamma delta epsilon", 101, "Reuters");
    b.category = "crypto".into();
    assert_eq!(clusters(&[a.clone(), b.clone()]).len(), 2);
    b.category = "general".into();
    assert_eq!(clusters(&[a.clone(), b]).len(), 1);
    assert_eq!(clusters(&[a.clone(), story(3, "alpha beta", 86500, "Reuters")]).len(), 1);
    assert_eq!(clusters(&[a.clone(), story(3, "alpha beta", 86501, "Reuters")]).len(), 2);
    assert_eq!(
        clusters(&[a, story(2, "alpha beta", 86499, "Reuters"), story(3, "alpha beta", 86501, "Reuters")]).len(),
        2
    );
}
#[test]
fn two_thousand_items_use_index_and_bounded_snapshot() {
    let items: Vec<_> = (0..2200).map(|i| story(i, &format!("unique{i} topic{i}"), i as i64, "Yahoo")).collect();
    let start = std::time::Instant::now();
    let (result, comparisons) = cluster_stats(&items);
    assert_eq!(result.len(), MAX_ITEMS);
    assert_eq!(comparisons, 0, "disjoint headlines must not be pairwise scanned");
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
    let duplicates: Vec<_> = (0..2000).map(|i| story(i, "Nvidia acquires chip startup", i as i64, "Reuters")).collect();
    let (result, comparisons) = cluster_stats(&duplicates);
    assert_eq!(result.len(), 1);
    assert_eq!(comparisons, 1999);
}
#[test]
fn extraction_catalog_cashtags_denylist_and_shouting() {
    for (title, expected) in [
        ("$NVDA and $MSFT gain", vec!["MSFT US Equity", "NVDA US Equity"]),
        ("NVDA and AAPL gain", vec!["AAPL US Equity", "NVDA US Equity"]),
        ("CEO sees AI GDP CPI FED SEC FDA EPS Q1 Q2 Q3 Q4 IT ON ALL USA US ETF IPO", vec![]),
        ("NVDA AND MSFT RALLY", vec![]),
        ("$NVDA AND MSFT RALLY", vec!["NVDA US Equity"]),
        ("ZZZZZ reports results", vec![]),
        ("bank gains on acclaim", vec![]),
    ] {
        assert_eq!(extract_tickers(title, &[]), expected, "{title}");
    }
    assert_eq!(
        extract_tickers("NVDA beats estimates", &["NVDA US Equity".into(), "AAPL US Equity".into()]),
        vec!["AAPL US Equity", "NVDA US Equity"]
    );
}
#[test]
fn tags_whole_word_inflection_table() {
    for (headline, tags) in [
        ("Earnings and revenue", vec!["Earnings"]),
        ("Raises guidance and outlook", vec!["Guidance"]),
        ("acquire", vec!["M&A"]),
        ("acquires", vec!["M&A"]),
        ("acquired", vec!["M&A"]),
        ("acquisition", vec!["M&A"]),
        ("FDA approves trial", vec!["FDA"]),
        ("CPI inflation", vec!["Macro"]),
        ("Bitcoin blockchain", vec!["Crypto"]),
        ("Analysts upgraded", vec!["Analyst"]),
        ("Insiders buy", vec!["Insider"]),
        ("Lawsuits and bans", vec!["Legal"]),
        ("Bank banking banner banana", vec![]),
        ("profitable guidancebook trialsman", vec![]),
        ("earnings guidance acquisition FDA inflation", vec!["Earnings", "Guidance"]),
    ] {
        assert_eq!(topic_tags(headline), tags, "{headline}");
    }
}
#[test]
fn watch_phrases_boundaries_case_unicode_and_overlaps() {
    for (text, query, expected) in [
        ("Bank BAN banking", "ban", vec!["BAN"]),
        ("Nvidia AI chip deal", "nvidia \"AI chip\"", vec!["Nvidia", "AI chip"]),
        ("AI big chip", "\"AI chip\"", vec![]),
        ("Café earnings rise", "CAFÉ", vec!["Café"]),
        ("AI chip deal", "\"AI chip\" chip", vec!["AI chip"]),
        ("unrelated", "", vec![]),
        ("acquisition bank", "acquire", vec![]),
    ] {
        let matches: Vec<_> = watch_ranges(text, query).into_iter().map(|r| &text[r]).collect();
        assert_eq!(matches, expected, "{text} / {query}");
    }
}
#[test]
fn insert_enriches_and_seen_survives_reopen() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!(
        "news-seen-{}-{}.sqlite3",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    let now = chrono::Utc::now().timestamp();
    let s = story(1, "$NVDA acquires startup", now, "Reuters");
    {
        let mut db = NewsStore::open(&path).unwrap();
        db.insert(std::slice::from_ref(&s), now).unwrap();
        let rows = db.query(&Query::default()).unwrap();
        assert_eq!(rows[0].item.tickers, vec!["NVDA US Equity"]);
        assert_eq!(rows[0].tags, vec!["M&A"]);
        assert!(!rows[0].seen);
        db.mark_seen(std::slice::from_ref(&s.id)).unwrap();
        db.insert(std::slice::from_ref(&s), now).unwrap();
    }
    let db = NewsStore::open(&path).unwrap();
    assert!(db.query(&Query::default()).unwrap()[0].seen);
    drop(db);
    std::fs::remove_file(path).unwrap();
}
#[test]
fn shared_token_candidates_expire_instead_of_scanning_ninety_days() {
    let items: Vec<_> = (0..2000)
        .map(|i| story(i, &format!("common a{i} b{i} c{i} d{i} e{i} f{i}"), i as i64 * 3600, "Reuters"))
        .collect();
    let (clusters, comparisons) = cluster_stats(&items);
    assert_eq!(clusters.len(), 2000);
    assert!(
        comparisons <= 2000 * 24,
        "only representatives in the inclusive 24h window may be compared: {comparisons}"
    );
}
#[test]
fn overlapping_watches_resolve_stably_and_colours_are_clamped() {
    let watches = vec![
        KeywordWatch { name: "Phrase".into(), words: "\"AI chip\"".into(), colour: 1 },
        KeywordWatch { name: "Other".into(), words: "chip NVDA".into(), colour: 5 },
    ];
    let text = "AI chip and NVDA";
    let hits = watch_highlights(text, &watches);
    assert_eq!(hits, vec![WatchHit { range: 0..7, colour: 1 }, WatchHit { range: 12..16, colour: 5 }]);
    let stored: KeywordWatch =
        serde_json::from_str(r#"{"name":"Invalid colour","words":"NVDA","colour":255}"#).unwrap();
    assert_eq!(stored.colour, 5);
    assert_eq!(serde_json::from_str::<KeywordWatch>("{}").unwrap(), KeywordWatch::default());
    assert_eq!(extract_tickers("$nvda gains", &[]), vec!["NVDA US Equity"]);
    for word in [
        "A", "I", "CEO", "AI", "US", "USA", "ETF", "IPO", "GDP", "CPI", "FED", "SEC", "FDA", "EPS", "IT", "ON", "ALL",
        "CAN", "FOR",
    ] {
        assert!(extract_tickers(&format!("{word} announces a new product today"), &[]).is_empty(), "{word}");
    }
    assert_eq!(topic_tags("M&A talks"), vec!["M&A"]);
    assert!(topic_tags("Court trial gives approval to banker").is_empty());
}
#[test]
fn review_single_letter_v_is_not_visa() {
    for title in ["Economists see a V-shaped recovery in hiring", "Senate debates Title V of the funding bill"] {
        assert!(extract_tickers(title, &[]).is_empty(), "{title}: {:?}", extract_tickers(title, &[]));
    }
    assert_eq!(extract_tickers("$V beats estimates", &[]), vec!["V US Equity"]);
    for word in [
        "ALL", "ON", "IT", "NOW", "ARE", "CAN", "GO", "BIG", "REAL", "OPEN", "SO", "HAS", "AN", "AM", "BE", "BY", "AT",
        "DO", "MOD", "LOW", "KEY", "TRUE", "PLAY", "LIFE", "SEE", "FUN", "CASH", "WELL", "CAR", "FAST", "NICE", "BEST",
        "GOOD", "HOME", "ANY", "OUT", "PEAK", "TECH",
    ] {
        assert!(extract_tickers(&format!("{word} announces a new product today"), &[]).is_empty(), "{word}");
    }
}
#[test]
fn review_disjoint_ticker_headlines_do_not_cluster() {
    let mut apple = story(1, "Apple beats quarterly earnings estimates", 100, "Reuters");
    apple.item.tickers = vec!["AAPL US Equity".into()];
    let mut microsoft = story(2, "Microsoft beats quarterly earnings estimates", 200, "Reuters");
    microsoft.item.tickers = vec!["MSFT US Equity".into()];
    let bridge = story(3, "Companies beat quarterly earnings estimates", 150, "Yahoo");
    assert_eq!(clusters(&[apple.clone(), microsoft.clone()]).len(), 2);
    assert_eq!(clusters(&[apple, bridge, microsoft]).len(), 2, "ticker-less bridges must not join disjoint companies");
}
#[test]
fn review_tags_cover_sues_and_merge_inflections() {
    for (headline, tag) in [
        ("Musk sues OpenAI over contract", "Legal"),
        ("Shareholders vow to sue the board", "Legal"),
        ("Regional lenders agree to merge", "M&A"),
        ("Two miners merged in all-stock deal", "M&A"),
    ] {
        assert!(topic_tags(headline).contains(&tag.to_string()), "{headline}: {:?}", topic_tags(headline));
    }
}
