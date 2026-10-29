//! Offline HTTP mocks only; the transport is pinned to loopback by Server::attach.
use super::*;
const FED: &str = "https://www.federalreserve.gov/feeds/press_all.xml";
const ECB: &str = "https://www.ecb.europa.eu/rss/press.html";
const BBC: &str = "https://feeds.bbci.co.uk/news/business/rss.xml";
const SEC: &str = "https://www.sec.gov/news/pressreleases.rss";
fn get(url: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new().get(url)
}
fn count(guard: &Guard) -> u32 {
    guard.db().unwrap().query_row("SELECT coalesce(sum(count),0) FROM requests", [], |r| r.get(0)).unwrap()
}
#[test]
fn n6_exact_hosts_policies_and_root_detection() {
    let new: Vec<_> = policies().into_iter().filter(|p| host_budget(p.group)).collect();
    assert_eq!(
        new.iter().map(|p| p.host).collect::<Vec<_>>(),
        [
            "www.federalreserve.gov",
            "www.ecb.europa.eu",
            "www.bankofengland.co.uk",
            "feeds.bbci.co.uk",
            "search.cnbc.com",
            "www.aljazeera.com",
            "techcrunch.com",
            "www.theverge.com",
            "hnrss.org"
        ]
    );
    for p in new {
        assert_eq!(p.min_interval, Duration::from_secs(if p.group == "tech" { 1800 } else { 900 }));
        assert_eq!(p.daily_cap, if p.group == "tech" { 60 } else { 120 });
        assert!(matches!(p.user_agent, UserAgent::Research));
    }
    for root in [
        "<rss/>",
        "<!DOCTYPE rss SYSTEM 'https://example.test/rss.dtd'><rss/>",
        "<!DOCTYPE rss [<!ENTITY fixture 'a > b'>]><rss/>",
        "<?xml version='1.0'?><!-- fixture --><!DOCTYPE feed><atom:feed xmlns:atom='http://www.w3.org/2005/Atom'/>",
        "<atom:feed/>",
        "<?xml version=\"1.0\"?>\n<feed xmlns=\"http://www.w3.org/2005/Atom\"/>",
        "\u{feff}\n<!-- fixture -->\n<rdf:RDF/>",
    ] {
        assert!(feed_signature(root.as_bytes()), "{root}");
    }
    for root in ["<html><rss/></html>", "<rssish/>", "<root/>", "", "<!-- missing end", "<!DOCTYPE html><html/>"] {
        assert!(!feed_signature(root.as_bytes()), "{root}");
    }
    let temp = Temp::new();
    let guard = temp.guard(false);
    for url in [
        "https://federalreserve.gov/feed",
        "https://other.federalreserve.gov/feed",
        "https://www.senate.gov/feed",
        "https://www.reuters.com/feed",
    ] {
        assert!(guard.policy("regulators", &url.parse().unwrap()).is_err());
    }
}
#[test]
fn n6_unknown_ledger_never_invents_permanent_strikes() {
    let temp = Temp::new();
    let guard = temp.guard(false);
    // Simulate an unreadable ledger without touching anything outside this fixture.
    let db = guard.db().unwrap();
    db.execute("ALTER TABLE cooldowns RENAME TO unavailable", []).unwrap();
    for _ in 0..4 {
        let Refused::Paused { until } = guard.pause("www.federalreserve.gov", "unreadable", None).unwrap() else {
            panic!()
        };
        assert!(until <= Utc::now().timestamp() + 604800);
    }
}
#[tokio::test]
async fn n6_sec_press_uses_legacy_cooldowns_and_success_resets_edgar() {
    let temp = Temp::new();
    let server = Server::new(200, "", "<rss/>");
    let guard = server.attach(temp.guard(false));
    guard.configure(Settings { sec_contact: "fixture@example.test".into(), ..Default::default() });
    for _ in 0..4 {
        let Refused::Paused { until } = guard.pause("sec", "HTTP 403", None).unwrap() else { panic!() };
        assert!(until <= Utc::now().timestamp() + 604800);
    }
    guard.db().unwrap().execute("UPDATE cooldowns SET until=0", []).unwrap();
    guard.send_feed("sec", "sec-press", get(SEC)).await.unwrap();
    assert_eq!(
        guard
            .db()
            .unwrap()
            .query_row("SELECT count(*) FROM cooldowns WHERE \"group\"='sec'", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        0
    );
    assert!(!guard.cooldowns.read().unwrap().contains_key("sec"));
}
#[tokio::test]
async fn n6_data_dir_explicit_override_allows_loopback_feed_fixture() {
    let temp = Temp::new();
    let server = Server::new(200, "", "<rss/>");
    let config = Config::from_lookup(|key| match key {
        "TR_NET_LEDGER" => Some(temp.0.join("override.sqlite3").into_os_string()),
        "TR_DATA_DIR" => Some(temp.0.join("instance").into_os_string()),
        "TR_ALLOW_EXTERNAL" => Some("1".into()),
        _ => None,
    })
    .unwrap();
    let guard = server.attach(Guard::new(config, Settings::default()).unwrap());
    guard.send_feed("regulators", "fed-press", get(FED)).await.unwrap();
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
    assert_eq!(count(&guard), 1);
}
#[tokio::test]
async fn n6_host_spacing_caps_and_shared_sec_budget() {
    let temp = Temp::new();
    let server = Server::new(200, "", "<rss/>");
    let guard = server.attach(temp.guard(false));
    guard.send_feed("regulators", "fed-press", get(FED)).await.unwrap();
    assert_eq!(guard.send_feed("regulators", "fed-press", get(FED)).await.unwrap_err(), Refused::Busy);
    guard.send_feed("regulators", "ecb-press", get(ECB)).await.unwrap();
    guard.send_feed("global-media", "bbc-business", get(BBC)).await.unwrap();
    assert_eq!(
        guard
            .send_feed("global-media", "bbc-world", get("https://feeds.bbci.co.uk/news/world/rss.xml"))
            .await
            .unwrap_err(),
        Refused::Busy
    );
    guard.db().unwrap().execute("UPDATE requests SET count=120 WHERE \"group\"='www.federalreserve.gov'", []).unwrap();
    age(&guard, "www.federalreserve.gov");
    assert_eq!(guard.send_feed("regulators", "fed-press", get(FED)).await.unwrap_err(), Refused::Budget);
    let status = guard.status().unwrap().into_iter().find(|s| s.group == "regulators").unwrap();
    assert_eq!((status.requests_today, status.cap), (121, 360));
    guard.configure(Settings { sec_contact: "fixture@example.test".into(), ..Default::default() });
    guard.send_feed("sec", "sec-press", get(SEC)).await.unwrap();
    assert_eq!(
        guard.send("sec", get("https://data.sec.gov/submissions/fixture.json")).await.unwrap_err(),
        Refused::Busy
    );
    let rows = server.requests.lock().unwrap();
    assert!(rows[..3].iter().all(|r| r.contains("personal research;") && !r.contains("fixture@example.test")));
    assert!(rows.last().unwrap().contains("fixture@example.test"));
}
#[tokio::test]
async fn n6_feed_root_wins_over_mime_and_block_words_in_article_text() {
    let temp = Temp::new();
    let body = "<?xml version=\"1.0\"?>\n<feed xmlns=\"http://www.w3.org/2005/Atom\"><title>Synthetic captcha research bulletin</title></feed>";
    let server = Server::new(200, "Content-Type: text/html\r\n", body);
    let guard = server.attach(temp.guard(false));
    assert_eq!(guard.send_feed("regulators", "fed-press", get(FED)).await.unwrap().0, body.as_bytes());
    assert!(guard.host_status("regulators", FED).unwrap().paused_until.is_none());
}
#[tokio::test]
async fn n6_conditional_headers_and_304_success() {
    let temp = Temp::new();
    let server = Server::sequence(
        &[(200, "ETag: \"fixture\"\r\nLast-Modified: Wed, 28 Oct 2026 12:00:00 GMT\r\n", "<rss/>"), (304, "", "")],
        Duration::ZERO,
    );
    let guard = server.attach(temp.guard(false));
    let (_, first) = guard.send_feed("regulators", "fed-press", get(FED)).await.unwrap();
    age(&guard, "www.federalreserve.gov");
    let (body, second) = guard
        .send_feed(
            "regulators",
            "fed-press",
            get(FED)
                .header("If-None-Match", first.etag.unwrap())
                .header("If-Modified-Since", first.last_modified.unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(second.status, 304);
    assert!(body.is_empty());
    assert_eq!(count(&guard), 2);
    assert!(guard.host_status("regulators", FED).unwrap().paused_until.is_none());
    let rows = server.requests.lock().unwrap();
    let headers = rows[1].to_ascii_lowercase();
    assert!(headers.contains("if-none-match: \"fixture\""));
    assert!(headers.contains("if-modified-since: wed, 28 oct 2026 12:00:00 gmt"));
}
#[tokio::test]
async fn n6_denials_and_non_feed_roots_pause_without_retry() {
    for (code, body, reason) in [
        (403, "", "HTTP 403"),
        (429, "", "HTTP 429"),
        (451, "", "HTTP 451"),
        (200, "<html>ordinary page</html>", "Not a feed"),
        (200, "<root><rss/></root>", "Not a feed"),
    ] {
        let temp = Temp::new();
        // A misleading XML MIME must never exempt an HTML root.
        let server = Server::new(code, "Content-Type: application/rss+xml\r\n", body);
        let guard = server.attach(temp.guard(false));
        for _ in 0..2 {
            assert!(matches!(guard.send_feed("regulators", "fed-press", get(FED)).await, Err(Refused::Paused { .. })));
        }
        assert_eq!(server.hits.load(Ordering::SeqCst), 1);
        assert_eq!(count(&guard), 1);
        assert!(guard.host_status("regulators", FED).unwrap().reason.unwrap().contains(reason));
        assert!(guard.host_status("regulators", ECB).unwrap().paused_until.is_none());
    }
}
#[tokio::test]
async fn n6_three_strikes_survive_success_restart_until_manual_resume() {
    let temp = Temp::new();
    let server = Server::sequence(&[(403, "", ""), (200, "", "<rss/>"), (429, "", ""), (451, "", "")], Duration::ZERO);
    let guard = server.attach(temp.guard(false));
    for code in [403, 200, 429, 451] {
        // Advance only expiring cooldowns; never bypass the permanent third strike.
        guard.db().unwrap().execute("UPDATE cooldowns SET until=0 WHERE strikes<3", []).unwrap();
        age(&guard, "www.federalreserve.gov");
        let result = guard.send_feed("regulators", "fed-press", get(FED)).await;
        if code == 200 {
            result.unwrap();
        } else {
            assert!(matches!(result, Err(Refused::Paused { .. })));
        }
    }
    drop(guard);
    let guard = server.attach(temp.guard(false));
    assert_eq!(guard.host_status("regulators", FED).unwrap().paused_until, Some(253_402_300_799));
    assert!(matches!(guard.send_feed("regulators", "fed-press", get(FED)).await, Err(Refused::Paused { .. })));
    assert_eq!(server.hits.load(Ordering::SeqCst), 4);
    guard.resume("regulators").unwrap();
    assert_eq!(count(&guard), 4);
    assert!(guard.host_status("regulators", FED).unwrap().paused_until.is_none());
    assert_eq!(guard.send_feed("regulators", "fed-press", get(FED)).await.unwrap_err(), Refused::Busy);
}
#[tokio::test]
async fn n6_disabled_feed_group_and_strict_data_dir_offline_never_charge_ledger() {
    let temp = Temp::new();
    let server = Server::new(200, "", "<rss/>");
    let guard = server.attach(temp.guard(false));
    let old = Settings::from_document(br#"{"data_sources":{"sec":true}}"#);
    assert!(old.regulators && old.global_media && old.tech && old.disabled_feeds.is_empty());
    let mut settings = old;
    settings.disabled_feeds.insert("fed-press".into());
    let roundtrip: Settings = serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
    assert!(roundtrip.disabled_feeds.contains("fed-press"));
    guard.configure(roundtrip);
    assert_eq!(guard.send_feed("regulators", "fed-press", get(FED)).await.unwrap_err(), Refused::Disabled);
    guard.configure(Settings { regulators: false, ..Default::default() });
    assert_eq!(guard.send_feed("regulators", "ecb-press", get(ECB)).await.unwrap_err(), Refused::Disabled);
    assert_eq!(count(&guard), 0);
    let mut offline = Config::from_lookup(|key| match key {
        "TR_NET_LEDGER" => Some(temp.0.join("offline.sqlite3").into_os_string()),
        "TR_DATA_DIR" => Some(temp.0.join("offline-instance").into_os_string()),
        _ => None,
    })
    .unwrap();
    offline.max_wait = Duration::ZERO;
    let guard = server.attach(Guard::new(offline, Settings::default()).unwrap());
    for (group, id, url) in [
        ("regulators", "fed-press", FED),
        ("global-media", "bbc-business", BBC),
        ("sec", "sec-press", SEC),
        ("tech", "hackernews", "https://hnrss.org/frontpage"),
    ] {
        assert_eq!(guard.send_feed(group, id, get(url)).await.unwrap_err(), Refused::Offline);
    }
    assert_eq!(count(&guard), 0);
    assert_eq!(server.hits.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn review_sec_filings_success_still_resets_strikes_and_never_pauses_forever() {
    let temp = Temp::new();
    let server = Server::sequence(
        &[(403, "", ""), (200, "", "{}"), (403, "", ""), (200, "", "{}"), (403, "", "")],
        Duration::ZERO,
    );
    let guard = server.attach(temp.guard(false));
    guard.configure(Settings { sec_contact: "fixture@example.test".into(), ..Default::default() });
    let filings = "https://data.sec.gov/submissions/fixture.json";
    for code in [403, 200, 403, 200, 403] {
        // Simulate each ordinary cooldown expiring before the next scheduled EDGAR request.
        guard.db().unwrap().execute("UPDATE cooldowns SET until=0", []).unwrap();
        age(&guard, "sec");
        let result = guard.send("sec", get(filings)).await;
        if code == 200 {
            result.unwrap();
        } else {
            assert!(matches!(result, Err(Refused::Paused { .. })));
        }
    }
    // Pre-N6: each success deleted the SEC cooldown row, so every incident was a fresh 6 h strike.
    let strikes: u32 =
        guard.db().unwrap().query_row("SELECT strikes FROM cooldowns WHERE \"group\"='sec'", [], |r| r.get(0)).unwrap();
    assert_eq!(strikes, 1);
    let until = guard.status().unwrap().into_iter().find(|s| s.group == "sec").unwrap().paused_until.unwrap();
    assert!(until < Utc::now().timestamp() + 604_800, "SEC filings paused until year 9999");
}
