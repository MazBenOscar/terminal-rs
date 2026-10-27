use super::*;
const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/finnhub_news.json");
#[test]
fn parsing_mapping_and_incremental_dates() {
    let items = parse(FIXTURE, Some("AAPL US Equity")).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].item.tickers, vec!["AAPL US Equity", "MSFT US Equity"]);
    assert_eq!(items[0].provider_id, "101");
    assert_eq!(items[0].source, "Finnhub");
    assert_eq!(items[0].summary, "Revenue rises as services expand.");
    assert_eq!(items[0].item.link, "https://example.test/apple");
    assert!(parse(b"{bad", None).is_err());
    assert_eq!(symbol("AAPL US Equity"), Some("AAPL"));
    assert_eq!(symbol("BRK.B US Equity"), Some("BRK.B"));
    for key in
        ["BTCUSD Curncy", "SPX Index", "VOD LN Equity", "AAPL", " US Equity", "bad US Equity", "AAPL&token=x US Equity"]
    {
        assert_eq!(symbol(key), None);
    }
    let now = 1793016000;
    assert_eq!(from_date(None, now), "2026-10-19");
    assert_eq!(from_date(Some(now - 86400), now), "2026-10-25");
    assert_eq!(from_date(Some(now - 30 * 86400), now), "2026-09-26");
}
#[test]
fn cursor_only_advances_and_empty_never_erases() {
    let mut store = NewsStore::memory().unwrap();
    let items = parse(FIXTURE, None).unwrap();
    assert_eq!(cursor(&store, "general").unwrap(), 0);
    save_market(&mut store, "general", &items, 1793016000).unwrap();
    assert_eq!(cursor(&store, "general").unwrap(), 102);
    save_market(&mut store, "general", &[], 1793016100).unwrap();
    save_market(&mut store, "general", &items[..1], 1793016100).unwrap();
    assert_eq!(cursor(&store, "general").unwrap(), 102);
    assert_eq!(cursor(&store, "crypto").unwrap(), 0);
    let query = crate::news_store::Query {
        filters: crate::news_store::Filters { scope: crate::news_store::Scope::All, ..Default::default() },
        ..Default::default()
    };
    assert_eq!(store.query(&query).unwrap().len(), 2);
}
#[test]
fn schedule_visible_only_round_robin_and_refusal_backoff() {
    let mut scheduler = Schedule::default();
    let symbols = vec!["AAPL US Equity".into(), "MSFT US Equity".into(), "BTCUSD Curncy".into()];
    assert_eq!(scheduler.next(1000, false, &symbols), None);
    for category in CATEGORIES {
        assert_eq!(scheduler.next(1000, true, &symbols), Some(Job::Market(category)));
    }
    assert_eq!(scheduler.next(1020, true, &symbols), Some(Job::Company(symbols[0].clone())));
    assert_eq!(scheduler.next(1025, true, &symbols), Some(Job::Company(symbols[1].clone())));
    assert_eq!(scheduler.next(1030, true, &symbols), None);
    scheduler.refused(1030, &Refused::Budget);
    for now in 1030..4630 {
        assert_eq!(scheduler.next(now, true, &symbols), None);
    }
    assert!(scheduler.next(4630, true, &symbols).is_some());
    scheduler.refused(4630, &Refused::Paused { until: 20000 });
    assert_eq!(scheduler.next(19999, true, &symbols), None);
    assert!(scheduler.next(20000, true, &symbols).is_some());
}
#[tokio::test]
async fn guarded_localhost_company_market_header_cursor_and_budget() {
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data").join(format!(
        "finnhub-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap()).parse().unwrap();
    let server = tokio::spawn(async move {
        let mut requests = vec![];
        for _ in 0..3 {
            let (mut stream, _) =
                tokio::time::timeout(Duration::from_secs(10), listener.accept()).await.unwrap().unwrap();
            let mut bytes = vec![];
            loop {
                let mut buf = [0; 2048];
                let n = stream.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
                if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                FIXTURE.len()
            );
            stream.write_all(headers.as_bytes()).await.unwrap();
            stream.write_all(FIXTURE).await.unwrap();
        }
        requests
    });
    // Inject the equivalents of TR_NET_LEDGER=<repo-local temp>, TR_DATA_DIR=<temp>,
    // TR_ALLOW_EXTERNAL=1, without mutating parallel tests' process environment.
    let config = tr_net::Config {
        ledger: root.join("ledger.sqlite3"),
        data_dir: Some(root.clone()),
        allow_external: true,
        max_wait: Duration::from_secs(5),
    };
    let guard = Guard::for_fixture(config, tr_net::Settings::default(), base).unwrap();
    let client = Client::default();
    let mut store = NewsStore::open(root.join("news.sqlite3")).unwrap();
    let now = 1793016000;
    let items = client.company(&guard, "fixture-secret", "AAPL US Equity", None, now).await.unwrap();
    store.insert(&items, now).unwrap();
    assert_eq!(store.latest("AAPL US Equity").unwrap(), Some(now));
    client
        .company(&guard, "fixture-secret", "AAPL US Equity", store.latest("AAPL US Equity").unwrap(), now)
        .await
        .unwrap();
    store.set_meta("minId:general", "99").unwrap();
    let items = client.market(&guard, "fixture-secret", "general", cursor(&store, "general").unwrap()).await.unwrap();
    save_market(&mut store, "general", &items, now).unwrap();
    assert_eq!(cursor(&store, "general").unwrap(), 102);
    let requests = server.await.unwrap();
    assert!(requests[0].starts_with("GET /api/v1/company-news?symbol=AAPL&from=2026-10-19&to=2026-10-26 "));
    assert!(requests[1].starts_with("GET /api/v1/company-news?symbol=AAPL&from=2026-10-26&to=2026-10-26 "));
    assert!(requests[2].starts_with("GET /api/v1/news?category=general&minId=99 "));
    for request in requests {
        assert!(!request.lines().next().unwrap().contains("secret"));
        assert!(request.to_lowercase().contains("x-finnhub-token: fixture-secret"));
        assert!(!request.contains("Mozilla"));
    }
    let ledger = rusqlite::Connection::open(root.join("ledger.sqlite3")).unwrap();
    ledger.execute("UPDATE requests SET count=3000 WHERE \"group\"='finnhub'", []).unwrap();
    assert_eq!(client.market(&guard, "fixture-secret", "general", 102).await, Err(Refused::Budget));
    drop(ledger);
    drop(store);
    let reopened = NewsStore::open(root.join("news.sqlite3")).unwrap();
    assert_eq!(cursor(&reopened, "general").unwrap(), 102);
    drop(reopened);
    drop(guard);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn review_market_news_topic_categories_reach_default_pane() {
    // Finnhub's documented /news?category=general items carry topic categories, not "general".
    let body = br#"[{"category":"technology","datetime":1793016000,"headline":"Square surges","id":5085164,"image":"","related":"","source":"CNBC","summary":"s","url":"https://example.test/sq"},
                    {"category":"top news","datetime":1793015000,"headline":"Stocks open higher","id":5085165,"image":"","related":"","source":"Reuters","summary":"s","url":"https://example.test/top"}]"#;
    let mut store = NewsStore::memory().unwrap();
    let items = parse(body, None).unwrap();
    // The same URL may already be saved from company news: dedup must not hide it.
    store.insert(&parse(body, Some("AAPL US Equity")).unwrap(), 1793016000).unwrap();
    save_market(&mut store, "general", &items, 1793016000).unwrap();
    // The default pane: Symbol scope, nothing linked = general market news.
    let rows = store.query(&crate::news_store::Query::default()).unwrap();
    assert_eq!(rows.len(), 2, "general market headlines must show in an unlinked News pane");
    for category in CATEGORIES {
        let mut store = NewsStore::memory().unwrap();
        save_market(&mut store, category, &items, 1793016000).unwrap();
        let q = crate::news_store::Query {
            filters: crate::news_store::Filters { scope: crate::news_store::Scope::All, ..Default::default() },
            ..Default::default()
        };
        assert!(store.query(&q).unwrap().iter().all(|s| s.category == category));
    }
}

#[test]
fn review_schedule_paces_within_daily_budget() {
    let symbols: Vec<String> = (0..150).map(|i| format!("S{i} US Equity")).collect();
    let mut scheduler = Schedule::default();
    let mut jobs = 0;
    // The worker ticks every 5 s and runs at most one job per tick.
    for now in (0..86_400).step_by(5) {
        if scheduler.next(now, true, &symbols).is_some() {
            jobs += 1;
        }
    }
    assert!(jobs <= 2600, "{jobs} Finnhub requests scheduled in one day");
}

#[test]
fn review_daily_planner_categories_visibility_priority_and_busy_retry() {
    assert_eq!(company_interval(20), 1800);
    assert_eq!(company_interval(150), 6480);
    assert_eq!(company_status(150), "Company news every 2 h for 150 symbols");
    for count in [20, 150] {
        for all in [false, true] {
            let symbols: Vec<_> = (0..count).map(|i| format!("S{i} US Equity")).collect();
            let mut scheduler = Schedule::default();
            let mut categories = [0; 4];
            let mut companies = 0;
            let mut last = std::collections::HashMap::new();
            for now in (0..86400).step_by(5) {
                match scheduler.next_for_views(now, true, all, &symbols, &symbols[..1]) {
                    Some(Job::Market(category)) => {
                        categories[CATEGORIES.iter().position(|c| *c == category).unwrap()] += 1
                    }
                    Some(Job::Company(symbol)) => {
                        if let Some(at) = last.insert(symbol, now) {
                            assert!(now - at >= company_interval(count));
                        }
                        companies += 1;
                    }
                    None => {}
                }
            }
            assert_eq!(categories, if all { [288, 96, 96, 96] } else { [288, 0, 0, 0] });
            assert_eq!(companies, if count == 20 { 960 } else { 2000 });
            assert!(companies + categories.iter().sum::<usize>() <= 2600);
        }
    }
    let symbols = vec!["AAPL US Equity".into(), "TSLA US Equity".into()];
    let mut scheduler = Schedule::default();
    assert_eq!(scheduler.next_for_views(0, true, false, &symbols, &symbols[1..]), Some(Job::Market("general")));
    assert_eq!(
        scheduler.next_for_views(5, true, false, &symbols, &symbols[1..]),
        Some(Job::Company(symbols[1].clone()))
    );
    scheduler.refused(5, &Refused::Busy);
    assert_eq!(scheduler.next_for_views(7, true, false, &symbols, &symbols[1..]), None);
    assert_eq!(
        scheduler.next_for_views(8, true, false, &symbols, &symbols[1..]),
        Some(Job::Company(symbols[1].clone()))
    );
}

#[test]
fn review_parse_bad_items_null_strings_dates_and_catalog_chips() {
    let body = br#"[
        {"id":1,"datetime":1793016000,"headline":"Valid","source":null,"summary":null,"url":null,"related":"AAPL,MSFT,VOD.L,UNKNOWNFOREIGN,7203.T"},
        {"id":2,"datetime":1793016000,"headline":null},
        {"id":3,"datetime":1793016000},
        {"id":4,"datetime":0,"headline":"bad date"},
        {"id":5,"datetime":-1,"headline":"bad date"},
        {"id":6,"datetime":1793016000,"headline":"Also valid"},
        {"id":7,"datetime":1793016000,"headline":42}
    ]"#;
    let items = parse(body, None).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].item.tickers, vec!["AAPL US Equity", "MSFT US Equity"]);
    assert!(items[1].item.tickers.is_empty());
    assert!(items[0].summary.is_empty());
    assert_eq!(parse(body, Some("TSLA US Equity")).unwrap()[1].item.tickers, vec!["TSLA US Equity"]);
}
