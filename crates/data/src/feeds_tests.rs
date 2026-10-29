use super::*;
use crate::{
    news_intelligence::{Event, Priority, classify_with_phrase, clusters},
    news_store::{Filters, Query, Scope, matches},
};
const FED: &str = include_str!("../tests/fixtures/n6_fed.xml");
const ATOM: &str = include_str!("../tests/fixtures/n6_verge.xml");
const RSS: &str = include_str!("../tests/fixtures/n6_rss.xml");
fn market() -> Query {
    Query { filters: Filters { scope: Scope::All, ..Default::default() }, ..Default::default() }
}
fn metadata() -> tr_net::ResponseMetadata {
    tr_net::ResponseMetadata {
        status: 200,
        etag: Some("\"fixture-v1\"".into()),
        last_modified: Some("Wed, 28 Oct 2026 12:00:00 GMT".into()),
    }
}
#[tokio::test]
async fn n6_each_disabled_feed_and_group_stops_at_guard_with_zero_ledger_usage() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../target/test-data/n6-disabled-{}", std::process::id()));
    let mut settings = tr_net::Settings { sec_contact: "fixture@example.test".into(), ..Default::default() };
    for feed in FEEDS {
        settings.disabled_feeds.insert(feed.id.into());
    }
    let guard = tr_net::Guard::for_fixture(
        tr_net::Config {
            ledger: root.join("ledger.sqlite3"),
            data_dir: None,
            allow_external: false,
            max_wait: Duration::ZERO,
        },
        settings,
        "http://127.0.0.1:9/".parse().unwrap(),
    )
    .unwrap();
    for feed in FEEDS {
        let error = feed.fetch(&guard, &State::default()).await.unwrap_err();
        assert_eq!(error.downcast_ref::<tr_net::Refused>(), Some(&tr_net::Refused::Disabled));
    }
    guard.configure(tr_net::Settings {
        regulators: false,
        global_media: false,
        tech: false,
        sec: false,
        ..Default::default()
    });
    for feed in FEEDS {
        let error = feed.fetch(&guard, &State::default()).await.unwrap_err();
        assert_eq!(error.downcast_ref::<tr_net::Refused>(), Some(&tr_net::Refused::Disabled));
    }
    assert!(guard.status().unwrap().iter().all(|s| s.requests_today == 0));
}
#[tokio::test]
async fn n6_fetch_ingest_and_conditional_304_end_to_end_with_loopback_fixture() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/test-data/n6-http-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap();
    let server = tokio::spawn(async move {
        let mut requests = vec![];
        for i in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![];
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            requests.push(String::from_utf8(request).unwrap().to_ascii_lowercase());
            let response = if i == 0 {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"fixture-v1\"\r\nLast-Modified: Wed, 28 Oct 2026 12:00:00 GMT\r\nConnection: close\r\n\r\n{FED}",
                    FED.len()
                )
            } else {
                "HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
            };
            socket.write_all(response.as_bytes()).await.unwrap();
        }
        requests
    });
    let ledger = root.join("ledger.sqlite3");
    let guard = tr_net::Guard::for_fixture(
        tr_net::Config { ledger: ledger.clone(), data_dir: None, allow_external: false, max_wait: Duration::ZERO },
        tr_net::Settings::default(),
        base,
    )
    .unwrap();
    let feed = &FEEDS[0];
    let mut store = NewsStore::open(root.join("news.sqlite3")).unwrap();
    let (stories, metadata) = feed.fetch(&guard, &store.feed_state(feed.id).unwrap()).await.unwrap();
    store.save_feed(feed, &stories, &metadata, 1_793_188_800).unwrap();
    drop(store);
    // Restarting retains both conditional validators.
    let mut store = NewsStore::open(root.join("news.sqlite3")).unwrap();
    rusqlite::Connection::open(ledger).unwrap().execute("UPDATE last_request SET at=0", []).unwrap();
    let (unchanged, metadata) = feed.fetch(&guard, &store.feed_state(feed.id).unwrap()).await.unwrap();
    assert!(unchanged.is_empty());
    assert_eq!(metadata.status, 304);
    store.save_feed(feed, &unchanged, &metadata, 1_793_188_801).unwrap();
    assert_eq!(store.query(&market()).unwrap().len(), 2);
    let requests = server.await.unwrap();
    assert!(requests[1].contains("if-none-match: \"fixture-v1\""));
    assert!(requests[1].contains("if-modified-since: wed, 28 oct 2026 12:00:00 gmt"));
}
#[test]
fn n6_all_twelve_shapes_metadata_cdata_and_atom_dates() {
    let mut ids = std::collections::BTreeSet::new();
    for feed in FEEDS {
        assert!(ids.insert(feed.id));
        let fixture = match feed.id {
            "fed-press" => FED,
            "theverge" => ATOM,
            _ => RSS,
        };
        let stories = feed.parse(fixture).unwrap();
        assert!(!stories.is_empty(), "{}", feed.id);
        assert!(stories[0].item.ts > 1_790_000_000, "{}", feed.id);
        for story in &stories {
            assert_eq!(story.provider, "rss");
            assert_eq!(story.provider_id, feed.id);
            assert_eq!(story.category, feed.category);
            assert_eq!(story.source, feed.publisher);
            assert_eq!(story.item.publisher, feed.publisher);
            assert!(!story.item.title.contains("CDATA"));
            assert!(story.item.link.starts_with("https://example.test/"));
        }
        let url = reqwest::Url::parse(feed.url).unwrap();
        assert!(tr_net::policies().iter().any(|p| p.group == feed.group && Some(p.host) == url.host_str()));
    }
    assert_eq!(FEEDS[0].parse(FED).unwrap()[0].item.ts, 1_793_188_800);
    assert!(ATOM.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<feed"));
    for phrase in ["Synthetic FOMC statement holds fixture rate", "Synthetic monetary policy decision"] {
        let (priority, event, _) = classify_with_phrase(phrase).unwrap();
        assert_eq!((priority, event), (Priority::High, Event::Fed));
    }
}
#[test]
fn n6_validators_roundtrip_idempotency_provenance_and_categories() {
    let mut store = NewsStore::memory().unwrap();
    let bbc = &FEEDS[4];
    let cnbc = &FEEDS[6];
    let stories = bbc.parse(RSS).unwrap();
    store.save_feed(bbc, &stories, &metadata(), 1_793_188_800).unwrap();
    store.save_feed(bbc, &stories, &metadata(), 1_793_188_801).unwrap();
    let state = store.feed_state(bbc.id).unwrap();
    assert_eq!(state.items_today, 1);
    let request = conditional_request(bbc, &state).build().unwrap();
    assert_eq!(request.headers()["If-None-Match"], "\"fixture-v1\"");
    assert_eq!(request.headers()["If-Modified-Since"], "Wed, 28 Oct 2026 12:00:00 GMT");
    store.save_feed(bbc, &[], &tr_net::ResponseMetadata { status: 304, ..Default::default() }, 1_793_188_802).unwrap();
    let unchanged = store.feed_state(bbc.id).unwrap();
    assert_eq!(unchanged.etag, state.etag);
    assert_eq!(unchanged.last_modified, state.last_modified);
    assert_eq!(unchanged.last_item_at, state.last_item_at);
    assert_eq!(unchanged.last_success_at, Some(1_793_188_802));
    assert_eq!(unchanged.last_status, "OK");
    store.save_feed(cnbc, &cnbc.parse(RSS).unwrap(), &metadata(), 1_793_188_803).unwrap();
    let rows = store.query(&market()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].provider_id, "bbc-business");
    assert_eq!(rows[0].category, "markets");
    assert_eq!(rows[0].sources, ["BBC", "CNBC"]);
    assert_eq!(clusters(&rows)[0].sources, ["BBC", "CNBC"]);
    let mut query = market();
    query.filters.sources = vec!["CNBC".into()];
    assert_eq!(store.query(&query).unwrap().len(), 1);
    assert!(matches(&rows[0], &query));
    query.filters.categories = vec!["tech".into()];
    assert!(store.query(&query).unwrap().is_empty());
    assert!(!matches(&rows[0], &query));
    query.filters.categories = vec!["markets".into()];
    assert_eq!(store.query(&query).unwrap().len(), 1);
    let persisted: Filters = serde_json::from_str(&serde_json::to_string(&query.filters).unwrap()).unwrap();
    assert_eq!(persisted.categories, ["markets"]);
    let legacy: Filters = serde_json::from_str(r#"{"scope":"All","sources":[],"days":7}"#).unwrap();
    assert!(legacy.categories.is_empty());
    assert!(matches(&rows[0], &Query { filters: legacy, ..market() }));
}
#[test]
fn n6_category_filters_sql_and_snapshot_agree_and_scopes_use_subjects_only() {
    let mut store = NewsStore::memory().unwrap();
    for (i, category) in CATEGORIES.iter().enumerate() {
        let xml = RSS.replace("example.test/report", &format!("example.test/report-{i}"));
        let mut stories = FEEDS[4].parse(&xml).unwrap();
        stories[0].category = (*category).into();
        store.insert(&stories, 1_793_188_800).unwrap();
    }
    let rows = store.query(&market()).unwrap();
    assert_eq!(rows.len(), 6);
    for category in CATEGORIES {
        let mut q = market();
        q.filters.categories = vec![category.into()];
        assert_eq!(store.query(&q).unwrap().len(), 1);
        assert_eq!(rows.iter().filter(|s| matches(s, &q)).count(), 1);
    }
    let mut symbol = market();
    symbol.symbols = vec!["NVDA US Equity".into()];
    symbol.filters.scope = Scope::Symbol;
    assert!(store.query(&symbol).unwrap().is_empty());
    let mut associated = FEEDS[9].parse(ATOM).unwrap();
    associated[0].item.title = "$NVDA shares rise after synthetic processor announcement".into();
    store.insert(&associated, 1_793_188_801).unwrap();
    assert_eq!(store.query(&symbol).unwrap().len(), 1);
    symbol.filters.scope = Scope::Watchlist;
    assert_eq!(store.query(&symbol).unwrap().len(), 1);
}
#[test]
fn n6_undated_and_guid_identity_stay_stable() {
    let mut store = NewsStore::memory().unwrap();
    let feed = &FEEDS[0];
    let stories = feed.parse(FED).unwrap();
    store.save_feed(feed, &stories, &metadata(), 1_793_188_900).unwrap();
    store.save_feed(feed, &stories, &metadata(), 1_793_189_900).unwrap();
    let rows = store.query(&market()).unwrap();
    let undated = rows.iter().find(|s| s.item.title.contains("undated")).unwrap();
    assert!(undated.inferred_ts);
    assert_eq!(undated.item.ts, 1_793_188_900);
    let xml = "<rss version=\"2.0\"><channel><title>Fixture</title><item><title>First title</title><link>https://example.test/guid-story</link><guid isPermaLink=\"false\">opaque-fixture-guid</guid></item></channel></rss>";
    let first = feed.parse(xml).unwrap();
    let second = feed.parse(&xml.replace("First title", "Corrected title")).unwrap();
    assert_eq!(first[0].id, second[0].id);
    store.insert(&first, 1_793_188_900).unwrap();
    store.insert(&second, 1_793_189_900).unwrap();
    assert_eq!(store.query(&market()).unwrap().len(), 3);
}
#[test]
fn n6_schedule_staggers_hosts_without_starving_shared_feeds_and_quiet_is_neutral() {
    let mut schedule = Schedule::new(0);
    let mut last = std::collections::BTreeMap::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut last_tick = None;
    for now in 0..=3600 {
        if let Some(feed) = schedule.next(now) {
            seen.insert(feed.id);
            if let Some(at) = last_tick {
                assert!(now - at >= 5);
            }
            last_tick = Some(now);
            let host = reqwest::Url::parse(feed.url).unwrap().host_str().unwrap().to_string();
            if let Some(at) = last.insert(host, now) {
                assert!(now - at >= feed.cadence.as_secs() as i64);
            }
        }
    }
    assert_eq!(seen.len(), 12);
    let state = State { last_item_at: Some(100), ..Default::default() };
    assert!(!FEEDS[2].quiet(&state, 100 + 30 * 86400));
    assert!(FEEDS[2].quiet(&state, 101 + 30 * 86400));
    assert!(!FEEDS[9].quiet(&state, 100 + 3 * 86400));
    assert!(FEEDS[9].quiet(&state, 101 + 3 * 86400));
}
#[test]
fn n6_migration_is_additive_and_legacy_sql_can_still_open_and_write() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!(
        "n6-legacy-{}-{}.sqlite3",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    let legacy = rusqlite::Connection::open(&path).unwrap();
    legacy.execute_batch("CREATE TABLE items(id TEXT PRIMARY KEY, source TEXT NOT NULL, provider_id TEXT NOT NULL,url TEXT NOT NULL,headline TEXT NOT NULL,summary TEXT NOT NULL,published INTEGER NOT NULL,tickers TEXT NOT NULL,category TEXT NOT NULL,fetched INTEGER NOT NULL,seen INTEGER NOT NULL DEFAULT 0,bookmark INTEGER NOT NULL DEFAULT 0);").unwrap();
    let store = NewsStore::open(&path).unwrap();
    assert_eq!(store.feed_states().unwrap().len(), 12);
    legacy.execute("INSERT INTO items(id,source,provider_id,url,headline,summary,published,tickers,category,fetched) VALUES('old','Legacy','','https://example.test/old','Fixture','',?1,'[]','general',?1)",[chrono::Utc::now().timestamp()]).unwrap();
    assert_eq!(store.query(&market()).unwrap().len(), 1);
    assert_eq!(legacy.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0)).unwrap(), 0);
    drop(store);
    drop(legacy);
    // Opening again backfills legacy provenance without changing old item columns.
    let reopened = NewsStore::open(&path).unwrap();
    assert_eq!(reopened.query(&market()).unwrap()[0].sources, ["Legacy"]);
}
#[test]
fn review_feed_summary_markup_is_reduced_to_text() {
    // The N6 fixture's description is CDATA HTML, as real TechCrunch / HN feeds are.
    let stories = FEEDS[4].parse(RSS).unwrap();
    assert_eq!(stories[0].summary, "Invented summary only.");
    let xml = RSS.replace(
        "Invented summary only.",
        "<span data-hidden='markuponly'>Invented &amp; readable summary only.</span>",
    );
    let stories = FEEDS[4].parse(&xml).unwrap();
    assert_eq!(stories[0].summary, "Invented & readable summary only.");
    let mut store = NewsStore::memory().unwrap();
    store.insert(&stories, 1_793_188_800).unwrap();
    let mut query = market();
    query.filters.search = "markuponly".into();
    assert!(!matches(&stories[0], &query));
    assert!(store.query(&query).unwrap().is_empty());
    query.filters.search = "readable".into();
    assert!(matches(&stories[0], &query));
    assert_eq!(store.query(&query).unwrap().len(), 1);
}
#[test]
fn n6_feed_catalogue_rejects_guid_only_and_unsafe_links() {
    for feed in FEEDS {
        for link in ["", "javascript:alert(1)", "file:///tmp/story", "/relative"] {
            let xml = format!(
                "<rss version='2.0'><channel><title>Fixture</title><item><title>Unopenable</title><guid isPermaLink='false'>opaque-fixture-guid</guid><link>{link}</link></item></channel></rss>"
            );
            assert!(feed.parse(&xml).unwrap().is_empty(), "{}: {link}", feed.id);
        }
    }
}
