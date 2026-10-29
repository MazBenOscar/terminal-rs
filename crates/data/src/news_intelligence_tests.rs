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
#[test]
fn n5c_priority_headline_fixture_is_token_exact_and_case_insensitive() {
    use Priority::{High, Normal, Urgent};
    let fixtures = [
        ("BREAKING: Acme reports results", Urgent),
        (" urgent — central bank statement", Urgent),
        ("Flash: company update", Urgent),
        ("ALERT: exchange notice", Urgent),
        ("Live breaking NEWS from the desk", Urgent),
        ("Trading halted in Acme shares", High),
        ("Exchange announces trading halt", High),
        ("Stock halted pending news", High),
        ("Acme files for bankruptcy", High),
        ("Acme enters CHAPTER 11", High),
        ("Acme cuts guidance", High),
        ("Acme guidance cut after weak sales", High),
        ("Acme raises guidance", High),
        ("Acme guidance raise confirmed", High),
        ("Acme beats estimates", High),
        ("Acme misses estimates", High),
        ("Acme to acquire Beta", High),
        ("Acme signs merger agreement", High),
        ("Acme signs buyout agreement", High),
        ("Acme acquisition agreement signed", High),
        ("FDA approval for Acme drug", High),
        ("FDA rejects Acme application", High),
        ("FDA rejection hits Acme", High),
        ("FDA approves therapy", High),
        ("SEC charges Acme", High),
        ("Acme recalls vehicles", High),
        ("Acme CEO resigns", High),
        ("Acme CEO steps down", High),
        ("Analyst upgrades Acme to buy", High),
        ("Acme downgraded to underweight by broker", High),
        ("Acme dividend cut", High),
        ("Acme suspends dividend", High),
        ("Acme dividend suspended", High),
        ("Fed rate decision due", Normal),
        ("ECB cuts interest rates", High),
        ("Fed raises rates by 25 basis points", High),
        ("ECB raises interest rates", High),
        ("Acme upgrade to buy", High),
        ("Acme upgraded to outperform", High),
        ("UK economic growth downgraded to 1% in 2027", Normal),
        ("Dow wavers ahead of rate decision", Normal),
        ("Fed holds rates steady, signals patience", High),
        ("Acme raises outlook after strong quarter", High),
        ("Acme to cut jobs in restructuring", High),
        ("Acme downgrade to sell", High),
        ("Broker downgrades Acme to neutral", High),
        ("Broker upgrades Acme to overweight", High),
        ("Acme announces voluntary recall", High),
        ("Acme recalls 10000 cars", High),
        ("Acme recalls 500 units", High),
        ("Acme recalls defective products", High),
        ("Acme to buy Beta for $5 billion", High),
        ("Acme tops expectations", High),
        ("Acme beats expectations", High),
        ("Acme misses expectations", High),
        ("Acme CEO to step down", High),
        ("BREAKING - exchange notice", Urgent),
        ("urgent–exchange notice", Urgent),
        ("Flash | exchange notice", Urgent),
        ("Alert — exchange notice", Urgent),
        ("Breaking down the Fed's rate path", Normal),
        ("Flash PMI: activity slows", Normal),
        ("Urgent care chain plans IPO", Normal),
        ("BREAKING Acme opens factory", Normal),
        ("Breakingnewsletter explores savings", Normal),
        ("Analyst upgrade cycle preview", Normal),
        ("Acme downgraded software version", Normal),
        ("Acme upgrades security features", Normal),
        ("Voters debate recall of governor", Normal),
        ("Traders recall past rate cuts", Normal),
        ("Mortgage rate cut helps buyers", Normal),
        ("Shipping rate hike expected", Normal),
        ("Acme to buy new office furniture", Normal),
        ("New products improve alertness", Normal),
        ("Alert investors watch Acme", Normal),
        ("Flashy launch draws crowds", Normal),
        ("Urgently needed repairs completed", Normal),
        ("Groundbreaking research unveiled", Normal),
        ("Acme breaking ground on factory", Normal),
        ("Acme halting progress on new factory", Normal),
        ("Chapter 110 of company history", Normal),
        ("Bankruptcyproof portfolio ideas", Normal),
        ("Acme misses estimated delivery date", Normal),
        ("FDA approvalness is not a word", Normal),
        ("InSEC charges are disputed", Normal),
        ("CEO resignation rumors circulate", Normal),
        ("Dividend cutting tools on display", Normal),
        ("Rate decisions previewed", Normal),
        ("Acme publishes annual report", Normal),
        ("", Normal),
    ];
    assert!(fixtures.len() >= 30);
    for (headline, expected) in fixtures {
        assert_eq!(priority(headline), expected, "{headline}");
        assert_eq!(priority(&headline.to_uppercase()), expected, "upper: {headline}");
    }
}

#[test]
fn n5c_sort_recent_priorities_source_strength_and_old_history() {
    let now = 200_000;
    let make = |id, title, age| Cluster::wire(&story(id, title, now - age, "Reuters"));
    let mut rows = vec![
        make(1, "Routine", 1),
        make(2, "Acme beats estimates", 2),
        make(3, "URGENT: statement", 3),
        make(4, "Acme cuts dividend", 4),
        make(5, "BREAKING: stale", 86400 + 2),
        make(6, "Routine old", 86401),
        make(7, "Routine boundary", 86400),
    ];
    rows[3].sources.push("CNBC".into());
    let ids = |rows: &[Cluster]| rows.iter().map(|c| c.lead.id.clone()).collect::<Vec<_>>();
    let original = ids(&rows);
    let expected = |indices: &[usize]| indices.iter().map(|&i| original[i].clone()).collect::<Vec<_>>();
    sort_clusters(&mut rows, Sort::Newest, false, now);
    assert_eq!(ids(&rows), expected(&[0, 1, 2, 3, 6, 5, 4]));
    sort_clusters(&mut rows, Sort::Relevance, false, now);
    assert_eq!(ids(&rows), expected(&[2, 3, 1, 0, 6, 5, 4]));
    sort_clusters(&mut rows, Sort::Relevance, true, now);
    assert_eq!(ids(&rows), expected(&[2, 1, 3, 0, 6, 5, 4]));
    sort_clusters(&mut rows, Sort::Relevance, false, now + 86401);
    assert_eq!(ids(&rows), expected(&[0, 1, 2, 3, 6, 5, 4]));
}

#[test]
fn n5c_completed_acquisition_is_not_below_talks() {
    assert!(priority("Acme acquires Beta for $5 billion") >= priority("Acme in talks to acquire Beta"));
}

#[test]
fn n5c_relevance_requires_a_recent_dated_member_and_keeps_old_band_chronological() {
    let now = 200_000;
    let mut inferred = story(1, "URGENT: Acme results", now - 1, "Reuters");
    inferred.inferred_ts = true;
    let old = story(2, "Acme results", now - 86401, "CNBC");
    let dated = story(3, "Routine dated report", now - 20, "Reuters");
    let mut mixed = Cluster::wire(&inferred);
    mixed.members.push(old);
    let mut undated = inferred.clone();
    undated.id = "undated".into();
    undated.item.ts -= 1;
    for wire in [true, false] {
        let mut rows = vec![mixed.clone(), Cluster::wire(&undated), Cluster::wire(&dated)];
        sort_clusters(&mut rows, Sort::Relevance, wire, now);
        assert_eq!(rows.iter().map(|c| &c.lead.id).collect::<Vec<_>>(), vec![&dated.id, &inferred.id, &undated.id]);
        let mut recent = dated.clone();
        recent.item.ts = now - 30;
        rows[1].members.push(recent);
        sort_clusters(&mut rows, Sort::Relevance, wire, now);
        assert_eq!(rows[0].lead.id, inferred.id, "a genuine recent member establishes freshness");
    }
}

#[test]
fn n5c_cluster_priority_is_maximum_even_when_lead_is_normal() {
    let a = story(1, "Acme revenue results today", 100, "Reuters");
    let b = story(2, "BREAKING: Acme revenue results today", 101, "CNBC");
    let rows = clusters(&[a, b]);
    assert_eq!(rows.len(), 1);
    assert_eq!(priority(&rows[0].lead.item.title), Priority::Normal);
    assert_eq!(rows[0].priority(), Priority::Urgent);
    assert_eq!(rows[0].sources.len(), 2);
}

#[test]
fn review_priority_first_token_urgency_false_positives() {
    for headline in [
        "Breaking down the Fed's rate path for 2027",
        "Flash PMI: euro zone business activity slows in October",
        "Urgent care chain Acme files for IPO",
    ] {
        assert_ne!(priority(headline), Priority::Urgent, "{headline}");
    }
}

#[test]
fn review_priority_generic_word_false_positives() {
    for headline in [
        "Microsoft upgrades Windows security features",
        "Apple's iPhone upgrade cycle slows",
        "California governor faces recall election",
        "Traders recall the 1987 crash as volatility returns",
    ] {
        assert_eq!(priority(headline), Priority::Normal, "{headline}");
    }
}

#[test]
fn review_priority_misses_common_market_event_wording() {
    for headline in [
        "Microsoft acquires Activision for $69 billion",
        "Acme agrees to buy Beta for $5 billion",
        "Nasdaq halts trading in Acme shares",
        "Fed cuts rates by a quarter point",
        "Acme tops estimates as sales jump",
    ] {
        assert_eq!(priority(headline), Priority::High, "{headline}");
    }
}

#[test]
fn review_relevance_inferred_time_does_not_lead_recent_band() {
    let now = 200_000;
    let mut undated = story(1, "BREAKING: Acme old repost", now - 10, "Reuters");
    undated.inferred_ts = true;
    let dated = story(2, "Acme beats estimates", now - 20, "CNBC");
    let mut rows = vec![Cluster::wire(&undated), Cluster::wire(&dated)];
    sort_clusters(&mut rows, Sort::Relevance, true, now);
    assert_eq!(rows[0].lead.id, dated.id, "inferred freshness must not rank as a recent breaking event");
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
        ("Ford Motor recalls vehicles", "", ""), // Ford is WEAK under the new rules.
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
        KeywordWatch { name: "Phrase".into(), words: "\"AI chip\"".into(), colour: 1, notify: false },
        KeywordWatch { name: "Other".into(), words: "chip NVDA".into(), colour: 5, notify: false },
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
/// Real rows from the SEC company map (edgar.sqlite3 `companies`, rowid order).
fn review_sec_names() -> NameIndex {
    let companies = [
        (1045810, "NVDA", "NVIDIA CORP"),
        (320193, "AAPL", "Apple Inc."),
        (789019, "MSFT", "MICROSOFT CORP"),
        (1652044, "GOOGL", "Alphabet Inc."),
        (1018724, "AMZN", "AMAZON COM INC"),
        (19617, "JPM", "JPMORGAN CHASE & CO"),
        (1932770, "HERE", "Here Group Ltd"),
        (1564708, "NWSA", "NEWS CORP"),
        (1845338, "MNDY", "monday.com Ltd."),
        (1688126, "CRCW", "Crypto Co"),
        (1530950, "POST", "Post Holdings, Inc."),
        (1050446, "MSTR", "Strategy Inc"),
        (2020027, "CLIK", "Click Holdings Ltd."),
        (1989930, "FGL", "Founder Group Ltd"),
        (1045942, "TRCK", "Track Group, Inc."),
        (1800227, "PPLI", "People Inc"),
        (1872195, "BLSH", "Bullish"),
        (1786352, "BILL", "BILL Holdings, Inc."),
        (318833, "TISI", "TEAM INC"),
        (726854, "CHCO", "CITY HOLDING CO"),
        (1120193, "NDAQ", "NASDAQ, INC."),
        (1527762, "CD", "Chaince Digital Holdings Inc."),
        (1998056, "PC", "Premium Catering (Holdings) Ltd"),
        (1276187, "ET", "Energy Transfer LP"),
        (891478, "SAN", "Banco Santander, S.A."),
        (1577526, "AI", "C3.ai, Inc."),
        (749251, "IT", "GARTNER INC"),
        (23795, "CTO", "CTO Realty Growth, Inc."),
        (3570, "LNG", "Cheniere Energy, Inc."),
        (1067983, "BRK-A", "BERKSHIRE HATHAWAY INC"),
        (1067983, "BRK-B", "BERKSHIRE HATHAWAY INC"),
        (763901, "BPOPO", "POPULAR, INC."),
        (763901, "BPOP", "POPULAR, INC."),
        (72971, "WFC", "WELLS FARGO & COMPANY/MN"),
        (1091667, "CHTR", "CHARTER COMMUNICATIONS, INC. /MO/"),
        (1973239, "ARM", "ARM HOLDINGS PLC /UK"),
        (1086600, "ARLP", "ALLIANCE RESOURCE PARTNERS LP"),
        (1181412, "SPCX", "SPACE EXPLORATION TECHNOLOGIES CORP"),
        (1162283, "MGHL", "MORGAN GROUP HOLDING CO"),
        (732717, "T", "AT&T INC."),
        (1283699, "TMUS", "T-Mobile US, Inc."),
        (732712, "VZ", "VERIZON COMMUNICATIONS INC"),
        (858877, "CSCO", "CISCO SYSTEMS, INC."),
        (1571996, "DELL", "Dell Technologies Inc."),
        (37996, "F", "FORD MOTOR CO"),
        (999991, "PAYD", "Paid Inc"),
    ]
    .map(|(cik, ticker, title)| (cik, ticker.into(), title.into()));
    NameIndex::new(&companies)
}
fn review_short(subjects: Vec<String>) -> Vec<String> {
    subjects.iter().map(|s| s.split_whitespace().next().unwrap().to_owned()).collect()
}
#[test]
fn review_common_word_company_names_need_evidence() {
    let index = review_sec_names();
    // Real headlines/summaries from the store; ~40 false subjects per 100 items come from this class.
    for (headline, summary, expected) in [
        ("Investor Who Called the AI Boom Predicts a Bitcoin Crash", "", "BTCUSD"),
        ("Here's Why Nvidia Stock Rallied", "", "NVDA"),
        ("Crypto could outperform AI stocks: Wall Street veteran", "", ""),
        ("Stocks Inch Higher After Monday's Anthropic Rally", "", ""),
        ("Bill Gates Says Robots Will Build Houses", "", ""),
        ("Still Bullish Into Year-End", "", ""),
        (
            "Microsoft offers buyouts",
            "Click here for the latest news. People familiar with the strategy say the team will post updates in the city on Monday.",
            "MSFT",
        ),
        ("GRO Advisory Founder Announces Collective", "", ""),
        ("Economists track the labor differential", "", ""),
    ] {
        let actual = review_short(index.subjects(headline, summary, &["NVDA US Equity".into()]));
        assert_eq!(actual, expected.split_whitespace().collect::<Vec<_>>(), "{headline} / {summary}");
    }
}
#[test]
fn review_exchange_names_and_prefixes_are_not_subjects() {
    let index = review_sec_names();
    for (headline, summary, expected) in [
        ("S&P 500, Nasdaq advance as oil eases ahead of Fed decision", "", ""),
        ("Janus Henderson launch tokenized Nasdaq-100 fund", "", ""),
        (
            "Scotiabank maintains Microsoft",
            "Analyst maintains Microsoft (NASDAQ:MSFT) with a Sector Outperform.",
            "MSFT",
        ),
        (
            "AstraZeneca offers compelling risk-reward",
            "AstraZeneca PLC (LSE:AZN, NYSE:AZN) offers ... Phase III results",
            "AZN",
        ),
        ("Apple delivery tracking", "Apple Inc (NASDAQ:AAPL, XETRA:APC) is expected to deliver", "AAPL"),
    ] {
        let actual = review_short(index.subjects(headline, summary, &[]));
        assert_eq!(actual, expected.split_whitespace().collect::<Vec<_>>(), "{headline} / {summary}");
    }
}
#[test]
fn review_bare_acronyms_are_not_sec_tickers() {
    let index = review_sec_names();
    for (headline, summary) in [
        ("Best CD rates today, Tuesday, October 27, 2026", ""),
        ("PC Sales Just Crashed 21% as AI Eats the World's Memory Chips", ""),
        ("Consumer confidence due", "It's released at 10 am ET on the last Tuesday of the month."),
        ("Mintlify launches enterprise offering", "SAN FRANCISCO, October 27, 2026--Mintlify, the AI-native platform"),
        ("Musk warns on capex", "Musk warned that the artificial intelligence (AI) infrastructure boom could slow."),
        ("Former CTO steps up to CEO role", ""),
        ("Qatar extends force majeure on LNG cargoes", ""),
        ("Information Technology (IT) Resilience Orchestration Market", ""),
    ] {
        assert!(
            index.subjects(headline, summary, &[]).is_empty(),
            "{headline} / {summary}: {:?}",
            index.subjects(headline, summary, &[])
        );
    }
    // Real tickers in ordinary positions still work.
    // Rule 3 explicitly curates AMD, independently of the SEC ticker list.
    assert_eq!(review_short(index.subjects("AMD, NVDA and AAPL lead gains", "", &[])), ["AMD", "NVDA", "AAPL"]);
}
#[test]
fn review_state_and_partnership_suffixes_are_stripped() {
    for (title, normalized) in [
        ("WELLS FARGO & COMPANY/MN", "wells fargo"),
        ("CHARTER COMMUNICATIONS, INC. /MO/", "charter communications"),
        ("Energy Transfer LP", "energy transfer"),
        ("ALLIANCE RESOURCE PARTNERS LP", "alliance resource partners"),
    ] {
        assert_eq!(company_name(title), normalized, "{title}");
    }
    let index = review_sec_names();
    assert_eq!(review_short(index.subjects("Wells Fargo rejects tokenized shares", "", &[])), ["WFC"]);
    // All three tokens are common words: rule 1 forbids SEC-name-only evidence.
    assert!(index.subjects("Alliance Resource Partners, L.P. Q3 2026 Earnings Call", "", &[]).is_empty());
    assert_eq!(review_short(index.subjects("Alliance Resource Partners (ARLP) Q3 Earnings", "", &[])), ["ARLP"]);
}
#[test]
fn review_share_class_prefers_catalogue_and_common_stock() {
    let index = review_sec_names();
    assert_eq!(index.subjects("Berkshire Hathaway buys more shares", "", &[]), vec!["BRK.B US Equity"]);
    assert!(index.subjects("Popular, Inc. reports record earnings", "", &[]).is_empty());
    // Exercise preferred/common choice with a distinctive SEC name, not a word-like name.
    let index =
        NameIndex::new(&[(1, "BPOPO".into(), "Popularez Inc".into()), (1, "BPOP".into(), "Popularez Inc".into())]);
    assert_eq!(index.subjects("Popularez reports earnings", "", &[]), vec!["BPOP US Equity"]);
}
#[test]
fn review_common_brand_forms_resolve_to_the_right_company() {
    let index = review_sec_names();
    assert_eq!(review_short(index.subjects("SpaceX Jumps 10% as AI Trade Returns", "", &[])), ["SPCX"]);
    assert_eq!(review_short(index.subjects("Rockefeller Adds J.P. Morgan Team in Houston", "", &[])), ["JPM"]);
    assert!(!index.subjects("Analysts look at T-Mobile's subscriber growth", "", &[]).contains(&"T US Equity".into()));
    assert_eq!(review_short(index.subjects("AT&T and Verizon slide on results", "", &[])), ["T", "VZ"]);
    assert_eq!(
        review_short(index.subjects("Dell, Lenovo, Cisco sign on to AMD blueprint", "", &[])),
        ["DELL", "CSCO", "AMD"]
    );
}
#[test]
fn review_people_possessives_are_not_company_evidence() {
    let index = review_sec_names();
    assert!(index.subjects("Harrison Ford's new film tops the box office", "", &[]).is_empty());
}
#[test]
fn review_famous_word_names_in_company_lists() {
    let index = review_sec_names();
    assert_eq!(
        review_short(index.subjects(
            "Nvidia, Apple, Alphabet, Microsoft, and Amazon Now Make Up 30.8% of VOO",
            "",
            &["NVDA US Equity".into()]
        )),
        ["NVDA", "AAPL", "GOOGL", "MSFT", "AMZN"]
    );
}
#[test]
fn review_doge_and_ether_need_crypto_context() {
    let index = review_sec_names();
    assert!(index.subjects("DOGE cuts federal contracts again", "", &[]).is_empty());
    assert!(index.subjects("Musk says DOGE savings were overstated", "", &[]).is_empty());
    assert!(index.subjects("Ad budgets vanish into the ether", "", &[]).is_empty());
}

#[test]
fn sec_names_require_distinctive_tokens_and_choose_longest_phrase() {
    let index = NameIndex::new(&[
        (1, "LONG".into(), "Xylophorics Networks Inc".into()),
        (2, "SHORT".into(), "Xylophorics Inc".into()),
        (3, "WORD".into(), "People News Group Inc".into()),
        (4, "DIGIT".into(), "12345 Holdings Inc".into()),
        (5, "AXO".into(), "AXO Inc".into()),
    ]);
    assert_eq!(
        index.subjects("xylophorics networks and xylophorics expand", "", &[]),
        ["LONG US Equity", "SHORT US Equity"]
    );
    assert!(index.subjects("People News Group stock earnings CEO", "", &["WORD US Equity".into()]).is_empty());
    assert!(index.subjects("12345 and AXO expand", "", &[]).is_empty());
    assert!(index.subjects("xylophoricsnetwork expands", "", &[]).is_empty());
    assert_eq!(index.subjects("$WORD and $AXO expand", "", &[]), ["WORD US Equity", "AXO US Equity"]);
    assert_eq!(default_names().explicit_tickers("BTC and ETH rally", &[], &[]), ["BTCUSD Curncy", "ETHUSD Curncy"]);
    assert!(default_names().explicit_tickers("DOGE cuts contracts", &[], &[]).is_empty());
}
#[test]
fn watched_symbols_admit_bare_tickers_not_weak_name_evidence() {
    let index = NameIndex::new(&[(1, "FROG".into(), "JFrog Ltd".into())]);
    assert!(index.subjects("FROG announces results", "", &[]).is_empty());
    assert_eq!(
        review_short(index.subjects_watched("FROG announces results", "", &[], &["FROG US Equity".into()])),
        ["FROG"]
    );
    assert_eq!(review_short(index.subjects("FROG announces results", "", &["FROG US Equity".into()])), ["FROG"]);
    assert!(index.subjects_watched("Target practice resumes", "", &[], &["TGT US Equity".into()]).is_empty());
    assert!(index.subjects_watched("AI tools improve", "", &[], &["AI US Equity".into()]).is_empty());
}
#[test]
fn explicit_tickers_parentheses_classes_and_separate_scope() {
    let index = review_sec_names();
    for (text, expected) in [
        ("Visa (V) announces results", "V"),
        ("Arm (ARM) expands", "ARM"),
        ("Here (HERE) announces results", "HERE"),
        ("$HERE announces results", "HERE"),
        ("NASDAQ:NVDA gains", "NVDA"),
        ("NYSE:BRK-B gains", "BRK.B"),
        ("$BRK.B gains", "BRK.B"),
        ("Berkshire (BRK.B) gains", "BRK.B"),
        ("Standalone (HERE) gains", "HERE"),
        ("standalone (HERE) gains", ""),
        ("HERE gains", ""),
        ("at t and At T sell products", ""),
        ("Samsung announces devices", ""),
        ("$ether rises", ""),
        ("$ETHER rises", ""),
        ("Ether price rises", "ETHUSD"),
        ("ether price rises", ""),
        ("$DOGE rises", "DOGEUSD"),
        ("$Ether rises", "ETHUSD"),
        ("DOGE cuts $10 billion in federal contracts", ""),
        ("Dogecoin rises", "DOGEUSD"),
        ("SOL rises", "SOLUSD"),
        ("Sol shines", ""),
        ("ADA rises", "ADAUSD"),
        ("Ada writes", ""),
    ] {
        assert_eq!(
            review_short(index.subjects(text, "", &[])),
            expected.split_whitespace().collect::<Vec<_>>(),
            "{text}"
        );
    }
    assert!(index.explicit_tickers("Alphabet joins Nvidia and Bitcoin", &[], &[]).is_empty());
    assert_eq!(index.explicit_tickers("Nvidia (NVDA) joins Alphabet", &[], &[]), ["NVDA US Equity"]);
    assert_eq!(review_short(index.subjects("jfrog (FROG) expands", "", &[])), Vec::<String>::new());
    let index = NameIndex::new(&[(1, "FROG".into(), "JFrog Ltd".into())]);
    assert_eq!(index.explicit_tickers("jfrog (FROG) expands", &[], &[]), ["FROG US Equity"]);
}
#[test]
fn summaries_decode_html_entities_before_matching() {
    let index = review_sec_names();
    assert_eq!(
        review_short(index.subjects(
            "Microsoft expands",
            "AT&amp;T, McDonald&#39;s, Lowe&rsquo;s and J&amp;J grow; &#36;NVDA gains; &quot;Alphabet&quot; reports",
            &[]
        )),
        ["MSFT", "T", "MCD", "LOW", "JNJ", "NVDA", "GOOGL"]
    );
    assert_eq!(
        review_short(index.subjects("", "Here&#39;s why people paid for Monday&#x2019;s news and strategy", &[])),
        Vec::<String>::new()
    );
    assert_eq!(decode_entities("a &unknown; b &amp; c &#x27; &#8217;"), "a &unknown; b & c ' ’");
}
#[test]
fn all_curated_aliases_use_explicit_keys_and_weak_evidence() {
    let index = NameIndex::new(&[]);
    for &(aliases, ticker, weak) in FAMOUS {
        for alias in aliases.split('|') {
            let headline = if weak { format!("{alias} stock gains") } else { format!("{alias} announces results") };
            assert_eq!(index.subjects(&headline, "", &[]).first(), Some(&equity_key(ticker)), "{headline}");
            if weak {
                // Uppercase abbreviations are themselves explicitly allowed tickers.
                if alias != ticker {
                    assert!(index.subjects(&format!("{alias} appears today"), "", &[]).is_empty(), "{alias}");
                }
                for headline in [format!("stock {alias}"), format!("price target {alias}"), format!("{alias} Q3")] {
                    assert!(index.subjects(&headline, "", &[]).contains(&equity_key(ticker)), "{headline}");
                }
            }
        }
    }
    assert!(index.subjects("Meta-Platforms discusses meta-analysis", "", &["META US Equity".into()]).is_empty());
}
#[test]
fn sec_share_selection_is_order_independent_and_never_uses_derivatives() {
    let companies = [
        (1, "XYLPP", "Xylophorics Inc"),
        (1, "XYLPO", "Xylophorics Inc"),
        (1, "XYLPW", "Xylophorics Inc"),
        (1, "XYLPR", "Xylophorics Inc"),
        (1, "XYLPU", "Xylophorics Inc"),
        (1, "XYLPZ", "Xylophorics Inc"),
        (1, "XYL-PA", "Xylophorics Inc"),
        (1, "XYLP", "Xylophorics Inc"),
        (1, "XYL", "Xylophorics Inc"),
        (2, "PREF-PB", "Preforica Inc"),
        (3, "BRK-A", "Berkshire Hathaway Inc"),
        (3, "BRK-B", "Berkshire Hathaway Inc"),
    ]
    .map(|(cik, ticker, title)| (cik, ticker.into(), title.into()));
    for offset in 0..companies.len() {
        let mut reordered = companies.to_vec();
        reordered.rotate_left(offset);
        for rows in [reordered.clone(), reordered.into_iter().rev().collect()] {
            let index = NameIndex::new(&rows);
            assert_eq!(index.subjects("Xylophorics expands", "", &[]), ["XYL US Equity"]);
            assert!(index.subjects("Preforica expands", "", &[]).is_empty());
            assert_eq!(index.subjects("Berkshire Hathaway expands", "", &[]), ["BRK.B US Equity"]);
        }
    }
}

// Labels checked against the task's rules. The reviewer fixtures supply some real
// headlines; other cases are specification examples. These are NOT represented as
// 120 headlines sampled from the inaccessible external audit files.
const LABELLED_CASES: [(&str, &str); 120] = [
    ("Here's Why Nvidia Stock Rallied", "NVDA"),
    ("Crypto could outperform AI stocks: Wall Street veteran", ""),
    ("Stocks Inch Higher After Monday's Anthropic Rally", ""),
    ("Bill Gates Says Robots Will Build Houses", ""),
    ("Still Bullish Into Year-End", ""),
    ("GRO Advisory Founder Announces Collective", ""),
    ("Economists track the labor differential", ""),
    ("S&P 500, Nasdaq advance as oil eases ahead of Fed decision", ""),
    ("Janus Henderson launch tokenized Nasdaq-100 fund", ""),
    ("Best CD rates today, Tuesday, October 27, 2026", ""),
    ("PC Sales Just Crashed 21% as AI Eats the World's Memory Chips", ""),
    ("Former CTO steps up to CEO role", ""),
    ("Qatar extends force majeure on LNG cargoes", ""),
    ("Information Technology (IT) Resilience Orchestration Market", ""),
    ("Harrison Ford's new film tops the box office", ""),
    ("DOGE cuts federal contracts again", ""),
    ("Musk says DOGE savings were overstated", ""),
    ("Ad budgets vanish into the ether", ""),
    ("Click here for the latest news", ""),
    ("People familiar with the strategy will post updates on Monday", ""),
    ("Workers were paid after the shutdown", ""),
    ("Apple pie recipes", ""),
    ("Amazon rainforest faces drought", ""),
    ("Oracle of Omaha speaks at meeting", ""),
    ("Meta-analysis shows better outcomes", ""),
    ("META-analysis shows better outcomes", ""),
    ("Meta-data describes the sample", ""),
    ("Nasdaq, NYSE, Dow Jones and Russell climb", ""),
    ("Nikkei, FTSE, DAX and Hang Seng fall", ""),
    ("CBOE, LSE, Xetra, TSX, ASX and Euronext announce hours", ""),
    ("Visa rules for H-1B", ""),
    ("Target practice resumes", ""),
    ("Gap between rich and poor", ""),
    ("Shell game in Washington", ""),
    ("Block sets record", ""),
    ("Strategy changes overnight", ""),
    ("Arm of the government announces findings", ""),
    ("Lucid dreams inspire artists", ""),
    ("Snap decisions prove costly", ""),
    ("Unity among the allies", ""),
    ("Delta variant remains in circulation", ""),
    ("Southwest winds bring rain", ""),
    ("Strive to improve public schools", ""),
    ("Economists see a V-shaped recovery", ""),
    ("Fed CEO sees AI GDP CPI SEC FDA EPS growth", ""),
    ("AP CTO CFO ET PT LNG OLED OCC WTI MA MW HR FT CD PC IPO ETF EV reports", ""),
    ("PCE PPI FOMC US UK EU UN AM PM Q1 Q2 Q3 Q4 IQ NRC SMR BC NYC SAN LA DC reports", ""),
    ("apple amazon alphabet meta oracle remain lowercase", ""),
    ("Alphabet expands its cloud business", "GOOGL"),
    ("Amazon opens another warehouse", "AMZN"),
    ("Meta announces new AI tools", "META"),
    ("Oracle wins cloud contract", "ORCL"),
    ("Wells Fargo rejects tokenized shares", "WFC"),
    ("Cisco signs chip agreement", "CSCO"),
    ("Dell announces new servers", "DELL"),
    ("Verizon slides on results", "VZ"),
    ("Visa shares rally as spending grows", "V"),
    ("Berkshire Hathaway buys more shares", "BRK.B"),
    ("Rockefeller Adds J.P. Morgan Team in Houston", "JPM"),
    ("JPMorgan Chase reports results", "JPM"),
    ("Apple launches new iPhone", "AAPL"),
    ("Google unveils AI search features", "GOOGL"),
    ("Facebook changes its platform", "META"),
    ("Microsoft expands cloud capacity", "MSFT"),
    ("Nvidia unveils a new AI chip", "NVDA"),
    ("Intel wins chip subsidy", "INTC"),
    ("Netflix raises subscription prices", "NFLX"),
    ("AT&T and Verizon slide on results", "T VZ"),
    ("T-Mobile adds subscribers", "TMUS"),
    ("Goldman Sachs raises outlook", "GS"),
    ("Morgan Stanley expands wealth division", "MS"),
    ("Bank of America reports results", "BAC"),
    ("Citigroup and Citi announce restructuring", "C"),
    ("Salesforce buys software startup", "CRM"),
    ("Adobe launches creative tools", "ADBE"),
    ("IBM announces quantum advance", "IBM"),
    ("Walmart and Costco add stores", "WMT COST"),
    ("Disney and Boeing announce results", "DIS BA"),
    ("Nike and Starbucks expand overseas", "NKE SBUX"),
    ("McDonald's raises prices", "MCD"),
    ("Coca-Cola and PepsiCo report sales", "KO PEP"),
    ("Pfizer and Moderna present vaccine data", "PFE MRNA"),
    ("Eli Lilly and Novo Nordisk report trials", "LLY NVO"),
    ("ExxonMobil and Chevron add production", "XOM CVX"),
    ("Palantir signs defense contract", "PLTR"),
    ("AMD and Broadcom win AI orders", "AMD AVGO"),
    ("Qualcomm and Micron report earnings", "QCOM MU"),
    ("Arm shares gain after earnings", "ARM"),
    ("Supermicro and Super Micro announce servers", "SMCI"),
    ("Marvell and TSMC partner on chips", "MRVL TSM"),
    ("Uber and Airbnb expand services", "UBER ABNB"),
    ("Shopify and Spotify release updates", "SHOP SPOT"),
    ("PayPal and Coinbase report earnings", "PYPL COIN"),
    ("Block stock sets record", "XYZ"),
    ("MicroStrategy announces purchase", "MSTR"),
    ("Strategy CEO outlines plan", "MSTR"),
    ("Tesla and Rivian release vehicles", "TSLA RIVN"),
    ("Ford shares rise on EV demand", "F"),
    ("Lucid earnings beat forecasts", "LCID"),
    ("Target investors welcome results", "TGT"),
    ("Gap rating upgraded", "GAP"),
    ("Shell price target rises", "SHEL"),
    ("Snowflake and CrowdStrike expand partnerships", "SNOW CRWD"),
    ("Datadog and ServiceNow report sales", "DDOG NOW"),
    ("Reddit and Roblox add users", "RDDT RBLX"),
    ("Sony and Alibaba release results", "SONY BABA"),
    ("Toyota and Honda expand factories", "TM HMC"),
    ("Rocket Lab and IonQ announce contracts", "RKLB IONQ"),
    ("CoreWeave and Nebius expand data centers", "CRWV NBIS"),
    ("Investor Who Called the AI Boom Predicts a Bitcoin Crash", "BTCUSD"),
    ("Bitcoin and Ethereum surge", "BTCUSD ETHUSD"),
    ("Solana outpaces Bitcoin", "SOLUSD BTCUSD"),
    ("XRP and Cardano rally", "XRPUSD ADAUSD"),
    ("DOGE token price climbs", "DOGEUSD"),
    ("Ether rallies after crypto demand", "ETHUSD"),
    ("$NVDA outpaces $MSFT", "NVDA MSFT"),
    ("Apple (AAPL) launches new iPhone", "AAPL"),
    ("Microsoft (NASDAQ:MSFT) raises outlook", "MSFT"),
    ("SpaceX Jumps 10% as AI Trade Returns", "SPCX"),
    ("Dell, Lenovo, Cisco sign on to AMD blueprint", "DELL CSCO AMD"),
];

#[derive(Default)]
struct AuditCounts {
    items: usize,
    false_subjects: usize,
    false_first: usize,
    misses: usize,
    expected: usize,
}
impl AuditCounts {
    fn add(&mut self, actual: &[String], expected: &[String]) {
        self.items += 1;
        self.expected += expected.len();
        self.false_subjects += actual.iter().filter(|s| !expected.contains(s)).count();
        self.false_first += usize::from(actual.first().is_some_and(|s| !expected.contains(s)));
        self.misses += expected.iter().filter(|s| !actual.contains(s)).count();
    }
    fn print(&self, label: &str) {
        let per100 = |n| n as f64 * 100.0 / self.items.max(1) as f64;
        eprintln!(
            "{label}: {} items; false subjects/100={:.2}; items with false first subject={} ({:.2}/100); misses/100={:.2}; found={}/{}",
            self.items,
            per100(self.false_subjects),
            self.false_first,
            per100(self.false_first),
            per100(self.misses),
            self.expected - self.misses,
            self.expected
        );
    }
}
#[test]
fn labelled_120_specification_regressions() {
    let index = review_sec_names();
    let mut counts = AuditCounts::default();
    for (headline, expected) in LABELLED_CASES {
        let expected: Vec<String> = expected.split_whitespace().map(str::to_owned).collect();
        let actual = review_short(index.subjects(headline, "", &[]));
        counts.add(&actual, &expected);
        assert_eq!(actual, expected, "{headline}");
    }
    counts.print("SPECIFICATION TABLE (not a real-data audit)");
    assert_eq!(counts.false_subjects, 0);
    assert!(counts.misses * 10 <= counts.expected);
}

/// Manual, read-only audit. TR_NEWS_AUDIT_DIR must contain news.sqlite3,
/// edgar.sqlite3, and labels.jsonl. Each JSONL row is hand-labelled:
/// {"headline":"...","expected":["NVDA","BTCUSD"]}
/// Labels must refer to exact stored headlines; provider associations are NOT labels.
/// Run: cargo test --offline --locked -p tr-data real_data_subject_audit -- --ignored --nocapture
#[test]
#[ignore = "manual read-only audit; requires TR_NEWS_AUDIT_DIR and hand-labelled labels.jsonl"]
fn real_data_subject_audit() {
    #[derive(Deserialize)]
    struct Label {
        headline: String,
        expected: Vec<String>,
    }
    let dir = std::path::PathBuf::from(std::env::var("TR_NEWS_AUDIT_DIR").expect(
        "set TR_NEWS_AUDIT_DIR to the directory containing the two read-only audit database copies and labels.jsonl",
    ));
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
    let news =
        rusqlite::Connection::open_with_flags(dir.join("news.sqlite3"), flags).expect("read-only news audit copy");
    let edgar =
        rusqlite::Connection::open_with_flags(dir.join("edgar.sqlite3"), flags).expect("read-only SEC audit copy");
    let companies = edgar
        .prepare("SELECT cik,ticker,title FROM companies")
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    let index = NameIndex::new(&companies);
    let raw = std::fs::read_to_string(dir.join("labels.jsonl"))
        .expect("hand-labelled labels.jsonl (never infer truth from stored subjects)");
    let mut counts = AuditCounts::default();
    let mut labelled = std::collections::HashSet::new();
    for line in raw.lines().filter(|s| !s.trim().is_empty()) {
        let label: Label = serde_json::from_str(line).expect("valid JSONL label");
        assert!(labelled.insert(label.headline.clone()), "duplicate headline label: {}", label.headline);
        assert_eq!(
            label.expected.iter().collect::<std::collections::HashSet<_>>().len(),
            label.expected.len(),
            "duplicate expected ticker"
        );
        let (summary, associated): (String, String) = news
            .query_row(
                "SELECT summary,tickers FROM items WHERE headline=?1 ORDER BY published DESC LIMIT 1",
                [&label.headline],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("label headline exists in real news copy");
        let associated: Vec<String> = serde_json::from_str(&associated).unwrap();
        let actual = review_short(index.subjects(&label.headline, &summary, &associated));
        if actual.iter().any(|s| !label.expected.contains(s)) || label.expected.iter().any(|s| !actual.contains(s)) {
            eprintln!("AUDIT mismatch: {}\n  expected={:?}; actual={actual:?}", label.headline, label.expected);
        }
        counts.add(&actual, &label.expected);
    }
    assert!(counts.items >= 120, "audit requires at least 120 hand-labelled stored headlines");
    counts.print("REAL-DATA AUDIT");
    assert_eq!(counts.false_subjects, 0, "precision target");
    assert!(counts.misses * 10 <= counts.expected, "recall target >=90%");
}

#[test]
fn fund_titles_and_bank_brands_are_not_company_subjects() {
    let index = NameIndex::new(&[
        (1050446, "MSTR".into(), "Strategy Inc".into()),
        (62234, "MCS".into(), "MARCUS CORP".into()),
        (886982, "GS".into(), "GOLDMAN SACHS GROUP INC".into()),
    ]);
    let short = |s: Vec<String>| s.iter().map(|t| t.split_whitespace().next().unwrap().to_owned()).collect::<Vec<_>>();
    assert!(short(index.subjects("Mar Vista U.S. Quality Strategy Q3 2026 Portfolio Review", "", &[])).is_empty());
    assert_eq!(short(index.subjects("Strategy Q3 earnings miss as bitcoin slides", "", &[])), ["MSTR", "BTCUSD"]);
    assert_eq!(short(index.subjects("Best CD rates today", "Marcus by Goldman Sachs on its 12-month CD", &[])), ["GS"]);
}
