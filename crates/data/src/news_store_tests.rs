use super::*;
fn story(link: &str, title: &str, ts: i64, ticker: &str, source: &str) -> Story {
    Story::new(
        NewsItem { title: title.into(), link: link.into(), ts, tickers: vec![ticker.into()], publisher: source.into() },
        source,
    )
}
#[test]
fn normalization_and_sha1_table() {
    for (raw, expected) in [
        ("HTTPS://Example.COM/story/#fragment", "https://example.com/story"),
        (
            "https://EXAMPLE.com/story/?utm_source=a&UTM_medium=b&guccounter=1&ncid=z&id=7#x",
            "https://example.com/story?id=7",
        ),
        ("http://example.com/", "http://example.com"),
        ("http://example.com:1234/?utm_campaign=x", "http://example.com:1234"),
        ("https://example.com/story?q=bank", "https://example.com/story?q=bank"),
        ("javascript:alert(1)", ""),
        ("file:///tmp/x", ""),
        ("not a link", ""),
    ] {
        assert_eq!(normalize_link(raw), expected, "{raw}");
    }
    assert_eq!(
        story("https://example.com/story/?utm_source=a", "A", 1, "AAPL US Equity", "Yahoo").id,
        story("https://EXAMPLE.com/story#x", "B", 2, "MSFT US Equity", "Finnhub").id
    );
    let fallback = story("javascript:bad", "abc", 1, "", "");
    let expected: String = Sha1::digest(b"|abc").iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(fallback.id, expected);
    let digest: String = Sha1::digest(b"abc").iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(digest, "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert!(fallback.item.link.is_empty());
}
#[test]
fn duplicate_sources_merge_tickers_without_replacing_headline() {
    let mut db = NewsStore::memory().unwrap();
    let a = story("https://a.test/1/", "Original", 200, "AAPL US Equity", "Yahoo");
    let b = story("https://A.test/1?utm_source=x", "Later", 300, "MSFT US Equity", "Finnhub");
    db.insert(&[a.clone(), b.clone(), b], 400).unwrap();
    let rows = db.query(&Query::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].item.title, a.item.title);
    assert_eq!(rows[0].item.tickers, vec!["AAPL US Equity", "MSFT US Equity"]);
    assert_eq!(db.latest("MSFT US Equity").unwrap(), Some(200));
}
#[test]
fn prune_exempts_bookmarks_and_startup_prunes() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!(
        "news-prune-{}-{}.sqlite3",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    let now = chrono::Utc::now().timestamp();
    {
        let mut db = NewsStore::open(&path).unwrap();
        db.insert(
            &[
                story("https://a/1", "Old", now - 91 * 86400, "A", "X"),
                story("https://a/2", "Kept", now - 91 * 86400, "B", "X"),
                story("https://a/3", "New", now, "C", "X"),
            ],
            now,
        )
        .unwrap();
        db.db.execute("UPDATE items SET bookmark=1 WHERE headline='Kept'", []).unwrap();
    }
    let db = NewsStore::open(&path).unwrap();
    let rows = db.query(&Query::default()).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].item.title, "New");
    assert_eq!(db.prune(now + 92 * 86400).unwrap(), 1);
    assert_eq!(db.query(&Query::default()).unwrap()[0].item.title, "Kept");
    drop(db);
    std::fs::remove_file(path).unwrap();
}
#[test]
fn scope_is_general_when_unlinked_and_empty_watchlist_is_not_all() {
    let mut db = NewsStore::memory().unwrap();
    let general = story("https://a/general", "Market news", 100, "", "Finnhub");
    let mut company = story("https://a/company", "Company news", 200, "AAPL US Equity", "Finnhub");
    company.category = "company".into();
    db.insert(&[general, company], 300).unwrap();
    let mut q = Query::default();
    assert_eq!(db.query(&q).unwrap().len(), 1);
    q.filters.scope = Scope::Watchlist;
    assert!(db.query(&q).unwrap().is_empty());
    q.symbols = vec!["AAPL US Equity".into()];
    assert_eq!(db.query(&q).unwrap()[0].item.title, "Company news");
    q.filters.scope = Scope::All;
    q.symbols.clear();
    assert_eq!(db.query(&q).unwrap().len(), 2);
}

#[test]
fn queries_search_scope_dates_filters_and_paging() {
    let mut db = NewsStore::memory().unwrap();
    let mut a = story("https://a/1", "Bank earnings beat", 300, "AAPL US Equity", "Finnhub");
    a.summary = "Revenue grows".into();
    db.insert(
        &[
            a,
            story("https://a/2", "Ban proposed", 200, "MSFT US Equity", "Yahoo"),
            story("https://a/3", "Markets rise", 100, "MSFT US Equity", "Yahoo"),
        ],
        400,
    )
    .unwrap();
    let mut q = Query::default();
    assert_eq!(db.query(&q).unwrap().len(), 3);
    q.symbols = vec!["AAPL US Equity".into(), "MSFT US Equity".into()];
    assert_eq!(db.query(&q).unwrap().len(), 3);
    q.symbols = vec!["AAPL US Equity".into()];
    assert_eq!(db.query(&q).unwrap().len(), 1);
    q.symbols.clear();
    q.start = 150;
    q.end = 250;
    assert_eq!(db.query(&q).unwrap()[0].item.title, "Ban proposed");
    q.start = 0;
    q.end = i64::MAX;
    q.filters.include = "BAN".into();
    assert_eq!(db.query(&q).unwrap().len(), 1);
    q.filters.include.clear();
    q.filters.exclude = "ban".into();
    assert_eq!(db.query(&q).unwrap().len(), 2);
    q.filters.exclude.clear();
    q.filters.search = "REVENUE".into();
    assert_eq!(db.query(&q).unwrap().len(), 1);
    q.filters.search = "\" OR *".into();
    assert!(db.query(&q).unwrap().is_empty());
    q.filters.search.clear();
    q.filters.sources = vec!["Yahoo".into()];
    q.limit = 1;
    q.offset = 1;
    assert_eq!(db.query(&q).unwrap()[0].item.title, "Markets rise");
    q.offset = 0;
    q.limit = 20;
    db.fts = false;
    q.filters.search = "proposed".into();
    assert_eq!(db.query(&q).unwrap().len(), 1);
    assert!(!whole_word("banking bank", "ban"));
    assert!(whole_word("BANK profits", "bank"));
}

#[test]
fn review_store_search_matches_partial_words_like_view_filter() {
    let mut db = NewsStore::memory().unwrap();
    let s = story("https://a.test/e", "Apple reports earnings growth", 100, "AAPL US Equity", "Yahoo");
    db.insert(std::slice::from_ref(&s), 100).unwrap();
    let mut q = Query { filters: Filters { scope: Scope::All, ..Default::default() }, ..Default::default() };
    q.filters.search = "earn".into();
    assert!(matches(&s, &q), "the pane's own filter treats search as a substring");
    assert_eq!(db.query(&q).unwrap().len(), 1, "stored-history search must agree with the pane filter");
    for search in ["earn", "earn gro", "gro earn", "port", "\" OR *", "   ", "APP REP"] {
        q.filters.search = search.into();
        db.fts = true;
        let fts = db.query(&q).unwrap();
        assert_eq!(!fts.is_empty(), matches(&s, &q), "view: {search}");
        db.fts = false;
        assert_eq!(db.query(&q).unwrap(), fts, "LIKE: {search}");
    }
}

#[test]
fn review_today_uses_local_calendar_midnight() {
    use chrono::{Datelike, TimeZone, Timelike};
    for now in [1793016000, 1773000000, 1793500000] {
        let start = day_start(now);
        let local_now = chrono::Local.timestamp_opt(now, 0).unwrap();
        let local_start = chrono::Local.timestamp_opt(start, 0).unwrap();
        assert_eq!(local_start.date_naive(), local_now.date_naive());
        assert_eq!((local_start.hour(), local_start.minute(), local_start.second()), (0, 0, 0));
        assert_eq!(local_start.year(), local_now.year());
    }
}

#[test]
fn n1b_subject_column_migration_and_user_examples_preserve_scope_and_seen() {
    let now = chrono::Utc::now().timestamp();
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE items(id TEXT PRIMARY KEY,source TEXT NOT NULL,provider_id TEXT NOT NULL,url TEXT NOT NULL,headline TEXT NOT NULL,summary TEXT NOT NULL,published INTEGER NOT NULL,tickers TEXT NOT NULL,category TEXT NOT NULL,fetched INTEGER NOT NULL,seen INTEGER NOT NULL DEFAULT 0,bookmark INTEGER NOT NULL DEFAULT 0,tags TEXT NOT NULL DEFAULT '[]',intelligence INTEGER NOT NULL DEFAULT 1,provider TEXT NOT NULL DEFAULT 'Finnhub');").unwrap();
    for (id, title, ticker) in [
        ("frog", "Can JFrog Stock Keep Its Lead Over Its Industry?", "MSFT US Equity"),
        ("btc", "Investor Who Called the AI Boom Predicts a Bitcoin Crash", "NVDA US Equity"),
    ] {
        connection.execute("INSERT INTO items(id,source,provider_id,url,headline,summary,published,tickers,category,fetched,seen) VALUES(?1,'Publisher','7','',?2,'',?3,?4,'company',?3,1)",
            params![id, title, now, serde_json::to_string(&[ticker]).unwrap()]).unwrap();
    }
    let mut db = NewsStore::init(connection).unwrap();
    assert_eq!(
        db.db
            .query_row("SELECT COUNT(*) FROM items WHERE subjects='[]' AND intelligence=1", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    db.names = Some(crate::news_intelligence::NameIndex::new(&[(1, "FROG".into(), "JFrog Ltd".into())]));
    for (association, subject) in [("MSFT US Equity", "FROG US Equity"), ("NVDA US Equity", "BTCUSD Curncy")] {
        let rows = db.query(&Query { symbols: vec![association.into()], ..Default::default() }).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].subjects, vec![subject]);
        assert!(rows[0].item.tickers.contains(&association.into()));
        assert!(rows[0].item.tickers.contains(&subject.into()));
        assert!(rows[0].seen);
        assert_eq!(rows[0].provider, "Finnhub");
    }
    let q = Query { symbols: vec!["FROG US Equity".into()], ..Default::default() };
    assert_eq!(db.query(&q).unwrap().len(), 1, "extracted subjects join association membership");
    db.db.execute("UPDATE items SET headline='Unmentioned' WHERE id='frog'", []).unwrap();
    assert_eq!(db.query(&q).unwrap()[0].subjects, vec!["FROG US Equity"], "completed rows are not rescanned");
}
#[test]
fn subject_backfill_is_bounded_and_read_failures_are_optional() {
    let mut db = NewsStore::memory().unwrap();
    let items: Vec<_> = (0..2001)
        .map(|i| story(&format!("https://fixture.test/bound/{i}"), "Bitcoin gains", 100, "", "Reuters"))
        .collect();
    db.insert(&items, 101).unwrap();
    db.db.execute("UPDATE items SET intelligence=1,subjects='[]'", []).unwrap();
    db.query(&Query::default()).unwrap();
    assert_eq!(
        db.db.query_row("SELECT COUNT(*) FROM items WHERE intelligence=1", [], |r| r.get::<_, i64>(0)).unwrap(),
        1
    );
    db.query(&Query::default()).unwrap();
    assert_eq!(
        db.db.query_row("SELECT COUNT(*) FROM items WHERE intelligence=1", [], |r| r.get::<_, i64>(0)).unwrap(),
        0
    );
    db.db.execute("UPDATE items SET intelligence=1,subjects='[]'", []).unwrap();
    db.db.execute_batch("PRAGMA query_only=ON").unwrap();
    let rows = db.query(&Query::default()).unwrap();
    assert_eq!(rows.len(), 200);
    assert!(rows.iter().all(|s| s.subjects.is_empty()));
}
#[test]
fn worker_names_read_only_edgar_map_refresh_and_missing_file() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!(
        "subject-edgar-{}-{}.sqlite3",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    let mut db = NewsStore::memory().unwrap();
    db.refresh_names(&path).unwrap();
    assert!(!path.exists(), "optional read must not create an EDGAR database");
    let source = Connection::open(&path).unwrap();
    source.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE companies(ticker TEXT PRIMARY KEY,cik INTEGER,title TEXT); CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT);
        INSERT INTO companies VALUES('GOOGL',1,'Alphabet Inc. Class A'),('GOOG',1,'Alphabet Inc. Class C'),('FROG',2,'JFrog Ltd');").unwrap();
    db.refresh_names(&path).unwrap();
    assert!(db.edgar.as_ref().unwrap().db.execute("INSERT INTO companies VALUES('BAD',3,'Bad Corp')", []).is_err());
    let mut s =
        story("https://fixture.test/local", "JFrog partners with Alphabet Inc.", 100, "MSFT US Equity", "Reuters");
    s.summary = "Nvidia supplies chips".into();
    db.insert(&[s], 101).unwrap();
    let q = Query { symbols: vec!["MSFT US Equity".into()], ..Default::default() };
    assert_eq!(db.query(&q).unwrap()[0].subjects, vec!["FROG US Equity", "GOOGL US Equity", "NVDA US Equity"]);
    source.execute("INSERT INTO meta VALUES('filing','new')", []).unwrap();
    db.refresh_names(&path).unwrap();
    assert_eq!(
        db.db.query_row("SELECT intelligence FROM items", [], |r| r.get::<_, i64>(0)).unwrap(),
        2,
        "non-company commits do not rebuild"
    );
    source.execute("UPDATE companies SET ticker='FROG2' WHERE ticker='FROG'", []).unwrap();
    db.refresh_names(&path).unwrap();
    assert_eq!(db.db.query_row("SELECT intelligence FROM items", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    let rows = db.query(&q).unwrap();
    assert_eq!(rows[0].subjects[0], "FROG2 US Equity");
    assert!(rows[0].item.tickers.contains(&"FROG US Equity".into()), "association history is retained");
    drop(db);
    drop(source);
    std::fs::remove_file(path).unwrap();
}
#[test]
fn n1a_schema_lazy_backfill_precedes_symbol_filter_and_runs_once() {
    let now = chrono::Utc::now().timestamp();
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE items(id TEXT PRIMARY KEY,source TEXT NOT NULL,provider_id TEXT NOT NULL,url TEXT NOT NULL,headline TEXT NOT NULL,summary TEXT NOT NULL,published INTEGER NOT NULL,tickers TEXT NOT NULL,category TEXT NOT NULL,fetched INTEGER NOT NULL,seen INTEGER NOT NULL DEFAULT 0,bookmark INTEGER NOT NULL DEFAULT 0);").unwrap();
    connection.execute("INSERT INTO items VALUES('old','Reuters','','https://fixture.test/old','$NVDA acquired startup','',?1,'[]','general',?1,1,0)",[now]).unwrap();
    let db = NewsStore::init(connection).unwrap();
    let before: i64 = db.db.query_row("SELECT intelligence FROM items", [], |r| r.get(0)).unwrap();
    assert_eq!(before, 0, "opening must not synchronously enrich retained history");
    let q = Query { symbols: vec!["NVDA US Equity".into()], ..Default::default() };
    let rows = db.query(&q).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].tags, vec!["M&A"]);
    assert!(rows[0].seen);
    assert_eq!(db.db.query_row("SELECT intelligence FROM items", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    db.db.execute("UPDATE items SET headline='$MSFT new guidance'", []).unwrap();
    let again = db.query(&q).unwrap();
    assert_eq!(again[0].item.tickers, rows[0].item.tickers, "already migrated rows are not recomputed on each query");
    assert_eq!(again[0].tags, rows[0].tags);
}
#[test]
fn duplicate_keeps_tags_of_the_retained_headline() {
    let mut db = NewsStore::memory().unwrap();
    let a = story("https://fixture.test/a", "Earnings and guidance", 10, "", "Reuters");
    let b = story("https://fixture.test/a", "Later lawsuit", 20, "", "Yahoo");
    db.insert(&[a, b], 30).unwrap();
    let rows = db.query(&Query::default()).unwrap();
    assert_eq!(rows[0].tags, vec!["Earnings", "Guidance"]);
}
#[test]
fn review_refetch_upgrades_n1a_provider_as_source() {
    let now = chrono::Utc::now().timestamp();
    let connection = Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE items(id TEXT PRIMARY KEY,source TEXT NOT NULL,provider_id TEXT NOT NULL,url TEXT NOT NULL,headline TEXT NOT NULL,summary TEXT NOT NULL,published INTEGER NOT NULL,tickers TEXT NOT NULL,category TEXT NOT NULL,fetched INTEGER NOT NULL,seen INTEGER NOT NULL DEFAULT 0,bookmark INTEGER NOT NULL DEFAULT 0);").unwrap();
    let mut fresh = story("https://fixture.test/n1a", "Chipmaker wins contract", now, "", "Reuters");
    fresh.provider = "Finnhub".into();
    connection
        .execute(
            "INSERT INTO items VALUES(?1,'Finnhub','7','https://fixture.test/n1a','Chipmaker wins contract','',?2,'[]','general',?2,0,0)",
            params![fresh.id, now],
        )
        .unwrap();
    let mut db = NewsStore::init(connection).unwrap();
    db.insert(std::slice::from_ref(&fresh), now).unwrap();
    let rows = db
        .query(&Query { filters: Filters { scope: Scope::All, ..Default::default() }, ..Default::default() })
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].source.as_str(), rows[0].provider.as_str()), ("Reuters", "Finnhub"));
}

#[test]
fn backfill_write_failure_does_not_fail_saved_read() {
    let mut db = NewsStore::memory().unwrap();
    let item = story("https://fixture.test/readonly", "$NVDA guidance", 100, "", "Reuters");
    db.insert(&[item], 101).unwrap();
    db.db.execute("UPDATE items SET intelligence=0,provider=''", []).unwrap();
    db.db.execute_batch("PRAGMA query_only=ON").unwrap();
    let rows = db.query(&Query::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, "Reuters");
}

#[test]
fn linkless_refetch_keeps_n1a_transport_identity_and_seen() {
    let mut db = NewsStore::memory().unwrap();
    let old = story("", "Chipmaker wins contract", 100, "", "Finnhub");
    db.insert(std::slice::from_ref(&old), 101).unwrap();
    db.db.execute("UPDATE items SET provider='',intelligence=0", []).unwrap();
    db.mark_seen(std::slice::from_ref(&old.id)).unwrap();
    // Backfilling provenance must still allow a later publisher upgrade.
    assert_eq!(db.query(&Query::default()).unwrap()[0].provider, "Finnhub");
    let mut fresh = story("", "Chipmaker wins contract", 100, "", "Reuters");
    fresh.provider = "Finnhub".into();
    db.insert(&[fresh], 102).unwrap();
    let rows = db.query(&Query::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, old.id);
    assert!(rows[0].seen);
    assert_eq!((rows[0].source.as_str(), rows[0].provider.as_str()), ("Reuters", "Finnhub"));
}
