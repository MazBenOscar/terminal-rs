use super::*;

#[test]
fn n7_analysis_cache_migrates_old_database_preserves_news_and_round_trips() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data/n7-analysis");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!("{}.sqlite3", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let now = chrono::Utc::now().timestamp();
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE items(id TEXT PRIMARY KEY,source TEXT NOT NULL,provider_id TEXT NOT NULL,
        url TEXT NOT NULL,headline TEXT NOT NULL,summary TEXT NOT NULL,published INTEGER NOT NULL,
        tickers TEXT NOT NULL,category TEXT NOT NULL,fetched INTEGER NOT NULL,seen INTEGER NOT NULL DEFAULT 0,
        bookmark INTEGER NOT NULL DEFAULT 0);",
    )
    .unwrap();
    db.execute("INSERT INTO items VALUES('old','Fixture','','https://fixture.invalid/old','Existing headline','summary',?1,'[]','general',?1,1,1)", [now]).unwrap();
    drop(db);
    let store = NewsStore::open(&path).unwrap();
    let rows = store
        .query(&Query { filters: Filters { scope: Scope::All, ..Default::default() }, ..Default::default() })
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].item.title, "Existing headline");
    assert!(rows[0].seen && rows[0].bookmark);
    assert!(store.story_analysis(&["lead".into(), "old".into()]).unwrap().is_none());
    let member = StoryAnalysis {
        story_id: "old".into(),
        model: "fixture-model".into(),
        created_at: now,
        json: "Prose fallback is cached verbatim".into(),
    };
    store.save_story_analysis(&member).unwrap();
    assert_eq!(store.story_analysis(&["lead".into(), "old".into()]).unwrap(), Some(member.clone()));
    let lead = StoryAnalysis { story_id: "lead".into(), json: "{\"sentiment\":0}".into(), ..member.clone() };
    store.save_story_analysis(&lead).unwrap();
    assert_eq!(store.story_analysis(&["lead".into(), "old".into()]).unwrap(), Some(lead.clone()));
    drop(store);
    let store = NewsStore::open(&path).unwrap();
    assert_eq!(store.story_analysis(&["lead".into()]).unwrap(), Some(lead));
    assert_eq!(store.story_analysis(&["old".into()]).unwrap(), Some(member));
    assert!(store.story_analysis(&["lead".into()]).unwrap().unwrap().v2_payload().is_none());
    let input = serde_json::json!({"input":"multi_source","summaries":2,"publishers":["A","B"],"authors":[]});
    for reply in ["{\"verdict\":{\"direction\":\"none\"}}", "Prose fallback\n\n preserved"] {
        let v2 = StoryAnalysis::v2("v2".into(), "fixture-v2".into(), now, reply.into(), input.clone());
        store.save_story_analysis(&v2).unwrap();
        let loaded = store.story_analysis(&["v2".into()]).unwrap().unwrap();
        assert_eq!(loaded, v2);
        assert_eq!(loaded.reply(), reply);
        assert_eq!(loaded.v2_payload(), Some((reply.to_owned(), input.clone())));
        assert_eq!(serde_json::from_str::<serde_json::Value>(&loaded.json).unwrap()["schema"], 2);
    }
    drop(store);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn n7_related_prefers_tickers_caps_eight_and_excludes_cluster_and_out_of_window() {
    use crate::news_intelligence::Cluster;
    let now = 10_000_000;
    let selected = story(
        "https://fixture.invalid/selected",
        "Acme reports quarterly earnings",
        now - 10,
        "ACME US Equity",
        "Fixture",
    );
    let cluster = Cluster::wire(&selected);
    let titles = [
        "Factory expansion in Japan",
        "Director resigns unexpectedly",
        "Dividend increases ten percent",
        "Bond issuance closes",
        "Patent dispute settled",
        "New smartphone introduced",
        "Retail store closures",
        "Cloud sales accelerate",
        "Union wage talks begin",
        "Cybersecurity breach investigated",
        "Warehouse construction starts",
        "Executive compensation approved",
    ];
    let mut items: Vec<_> = titles
        .iter()
        .enumerate()
        .map(|(i, title)| {
            story(
                &format!("https://fixture.invalid/related/{i}"),
                title,
                now - i as i64 * 40_000,
                "ACME US Equity",
                "Fixture",
            )
        })
        .collect();
    items.push(selected.clone());
    let mut member = selected.clone();
    member.id = "syndicated-member".into();
    items.push(member);
    items.push(story("https://fixture.invalid/old", "Old", now - 7 * 86400 - 1, "ACME US Equity", "Fixture"));
    items.push(story("https://fixture.invalid/future", "Future", now + 1, "ACME US Equity", "Fixture"));
    items.push(story("https://fixture.invalid/other", "Other category match", now, "OTHER US Equity", "Fixture"));
    let related = related_stories(&cluster, &items, now);
    assert_eq!(related.len(), 8);
    assert!(related.iter().all(|c| c.tickers.contains(&"ACME US Equity".into())
        && !c.member_ids.contains(&selected.id)
        && !c.member_ids.contains(&"syndicated-member".into())));
    assert!(related.windows(2).all(|w| w[0].newest >= w[1].newest));
    assert!(related.iter().all(|c| c.newest <= now && c.newest >= now - 7 * 86400));
    let edge = story("https://fixture.invalid/edge", "Boundary story", now - 7 * 86400, "ACME US Equity", "Fixture");
    assert_eq!(related_stories(&cluster, &[edge], now).len(), 1);
}

#[test]
fn n7_related_finds_ticker_beyond_the_first_market_page() {
    use crate::news_intelligence::{Cluster, MAX_ITEMS};
    let now = chrono::Utc::now().timestamp();
    let selected = story(
        "https://fixture.invalid/page-selected",
        "Chip supplier expands fabrication",
        now,
        "NVDA US Equity",
        "Fixture",
    );
    let mut items: Vec<_> = (0..=MAX_ITEMS)
        .map(|i| {
            story(
                &format!("https://fixture.invalid/page/{i}"),
                &format!("Unrelated shipping update {i}"),
                now - i as i64,
                "OTHER US Equity",
                "Fixture",
            )
        })
        .collect();
    let ticker = story(
        "https://fixture.invalid/page-ticker",
        "Graphics product launch",
        now - MAX_ITEMS as i64 - 10,
        "NVDA US Equity",
        "Fixture",
    );
    items.push(ticker.clone());
    let cluster = Cluster::wire(&selected);
    let related = related_stories(&cluster, &items, now);
    assert_eq!(related.len(), 1);
    assert_eq!(related[0].lead.id, ticker.id);
    let mut store = NewsStore::memory().unwrap();
    store.insert(&items, now).unwrap();
    let related = store.related_stories(&cluster, now).unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0].lead.id, ticker.id);
}

#[test]
fn related_archive_scan_stops_at_four_pages() {
    use crate::news_intelligence::{Cluster, MAX_ITEMS};
    let now = chrono::Utc::now().timestamp();
    let selected = story(
        "https://fixture.invalid/cap-selected",
        "Chip supplier expands fabrication",
        now,
        "NVDA US Equity",
        "Fixture",
    );
    let mut items: Vec<_> = (0..4 * MAX_ITEMS)
        .map(|i| {
            story(
                &format!("https://fixture.invalid/cap/{i}"),
                &format!("Shipping report {i}"),
                now - i as i64,
                "OTHER US Equity",
                "Fixture",
            )
        })
        .collect();
    let outside = story(
        "https://fixture.invalid/cap-outside",
        "Graphics product launch",
        now - 4 * MAX_ITEMS as i64 - 1,
        "NVDA US Equity",
        "Fixture",
    );
    items.push(outside.clone());
    let mut store = NewsStore::memory().unwrap();
    store.insert(&items, now).unwrap();
    let related = store.related_stories(&Cluster::wire(&selected), now).unwrap();
    assert!(
        related.iter().all(|c| !c.member_ids.contains(&outside.id)),
        "the inspector must not walk the entire archive"
    );
}

#[test]
fn n7_related_falls_back_to_event_or_category_only_without_shared_tickers() {
    use crate::news_intelligence::Cluster;
    let now = 10_000_000;
    let mut selected =
        story("https://fixture.invalid/selected", "Acme beats estimates", now, "ACME US Equity", "Fixture");
    selected.category = "technology".into();
    let mut same_category =
        story("https://fixture.invalid/category", "New product preview", now - 1, "OTHER US Equity", "Fixture");
    same_category.category = "technology".into();
    let same_event =
        story("https://fixture.invalid/event", "Other beats estimates", now - 2, "OTHER US Equity", "Fixture");
    let mut unrelated =
        story("https://fixture.invalid/unrelated", "Sports results", now - 3, "OTHER US Equity", "Fixture");
    unrelated.category = "sports".into();
    let mut undated = same_category.clone();
    undated.id = "undated".into();
    undated.inferred_ts = true;
    let rows = related_stories(
        &Cluster::wire(&selected),
        &[same_category.clone(), same_event.clone(), unrelated, undated],
        now,
    );
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|c| c.member_ids.contains(&same_category.id)));
    assert!(rows.iter().any(|c| c.member_ids.contains(&same_event.id)));
}

#[test]
fn n5_claim_retention_prunes_old_rows_but_keeps_live_cluster_identity() {
    let now = chrono::Utc::now().timestamp();
    let mut store = NewsStore::memory().unwrap();
    let item =
        story("https://fixture.invalid/live", "NVDA earnings record revenue", now - 60, "NVDA US Equity", "Fixture");
    store.insert(std::slice::from_ref(&item), now).unwrap();
    store.claim_cluster(std::slice::from_ref(&item.id)).unwrap();
    store.claim_notification("keyword", "expired").unwrap();
    store.claim_notification("volume", "edge").unwrap();
    store
        .db
        .execute("UPDATE notification_seen_at SET at=?1 WHERE kind='keyword' AND id='expired'", [now - 91 * 86400])
        .unwrap();
    store
        .db
        .execute("UPDATE notification_seen_at SET at=?1 WHERE kind='volume' AND id='edge'", [now - 90 * 86400])
        .unwrap();
    store.prune(now).unwrap();
    assert!(!store.notification_claimed("keyword", "expired").unwrap());
    assert!(store.notification_claimed("volume", "edge").unwrap());
    assert!(store.notification_claimed("breaking-member", &item.id).unwrap());
    assert!(!store.claim_cluster(&[item.id.clone(), "late-member".into()]).unwrap());
    assert!(store.cluster_previously_delivered(&item).unwrap());
}

#[test]
fn n5_undated_rss_is_visible_at_ingest_time_and_flagged() {
    let now = chrono::Utc::now().timestamp();
    let items = crate::rss::parse_rss("<rss version=\"2.0\"><channel><title>Fixture</title><link>https://fixture.invalid</link><description>Fixture</description><item><title>Undated story</title><link>https://fixture.invalid/undated</link></item></channel></rss>", "Fixture").unwrap();
    assert_eq!(items.len(), 1);
    let mut store = NewsStore::memory().unwrap();
    store.insert(&[Story::new(items[0].clone(), "Fixture")], now).unwrap();
    let q = Query {
        start: now - 60,
        end: now,
        filters: Filters { scope: Scope::All, ..Default::default() },
        ..Default::default()
    };
    let rows = store.query(&q).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].item.ts, now);
    assert!(rows[0].inferred_ts);
    store.insert(&[Story::new(items[0].clone(), "Fixture")], now + 60).unwrap();
    assert_eq!(store.query(&q).unwrap()[0].item.ts, now, "re-import must not move inferred time");
    let yahoo = crate::yahoo::parse_news(
        r#"{"news":[{"title":"Yahoo undated","link":"https://fixture.invalid/yahoo-undated"}]}"#,
    )
    .unwrap();
    store.insert(&[Story::new(yahoo[0].clone(), "Yahoo")], now).unwrap();
    let rows = store.query(&q).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|story| story.inferred_ts && story.item.ts == now));
}

#[test]
fn n5_bookmarks_persist_filter_and_are_exempt_from_prune() {
    let now = chrono::Utc::now().timestamp();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data/n5-bookmarks");
    std::fs::create_dir_all(&root).unwrap();
    let path =
        root.join(format!("{}-{}.sqlite3", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap()));
    let mut store = NewsStore::open(&path).unwrap();
    let saved = story("https://fixture.invalid/saved", "Saved story", now - 91 * 86400, "", "Fixture");
    let other = story("https://fixture.invalid/other", "Other story", now - 91 * 86400, "", "Fixture");
    store.insert(&[saved.clone(), other], now).unwrap();
    store.set_bookmark(std::slice::from_ref(&saved.id), true).unwrap();
    assert_eq!(store.prune(now).unwrap(), 1);
    let q = Query { filters: Filters { saved: true, scope: Scope::All, ..Default::default() }, ..Default::default() };
    let rows = store.query(&q).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].bookmark && matches(&rows[0], &q));
    drop(store);
    let store = NewsStore::open(&path).unwrap();
    assert_eq!(store.query(&q).unwrap().len(), 1, "saved old story survives reopen and startup prune");
    store.set_bookmark(std::slice::from_ref(&saved.id), false).unwrap();
    assert!(store.query(&q).unwrap().is_empty());
    assert_eq!(store.prune(now).unwrap(), 1);
    drop(store);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn n4_freshness_is_strict_and_never_replays_a_restart_or_enabled_backlog() {
    use local::fresh;
    assert!(fresh(101, 100, 90, 102));
    for (published, enabled, session, now) in
        [(100, 100, 90, 102), (101, 100, 102, 103), (101, 102, 90, 103), (1, 0, 0, 7202), (104, 100, 90, 103)]
    {
        assert!(!fresh(published, enabled, session, now));
    }
    assert!(fresh(101, 100, 90, 7301));
}
#[test]
fn n4_quiet_hours_wrap_midnight_without_catchup() {
    for hour in 0..24 {
        assert_eq!(local::quiet(22, 7, hour), !(7..22).contains(&hour));
    }
    assert!(local::quiet(9, 17, 9));
    assert!(!local::quiet(9, 17, 17));
    assert!(!local::quiet(0, 0, 0));
    let store = NewsStore::memory().unwrap();
    let (desktop, history) = store.deliver_notifications(&["quiet".into()], 100, true).unwrap();
    assert!(desktop.is_empty());
    assert_eq!(history, ["quiet"]);
    assert!(store.deliver_notifications(&[], 101, false).unwrap().0.is_empty());
}
#[test]
fn n4_global_rate_limit_includes_overflow_summaries() {
    let store = NewsStore::memory().unwrap();
    let messages = (0..10).map(|i| format!("event {i}")).collect::<Vec<_>>();
    let (desktop, history) = store.deliver_notifications(&messages, 100, false).unwrap();
    assert_eq!(desktop.len(), 6);
    assert_eq!(desktop[5], "+5 more in News");
    assert_eq!(history.len(), 10);
    assert!(store.deliver_notifications(&["another".into()], 699, false).unwrap().0.is_empty());
    assert_eq!(store.deliver_notifications(&[], 700, false).unwrap().0, ["+1 more in News"]);
    assert_eq!(store.deliver_notifications(&messages[..5], 701, false).unwrap().0.len(), 5);
    assert!(store.deliver_notifications(&messages[..1], 702, false).unwrap().0.is_empty());
}
#[test]
fn n4_journal_deduplicates_restart_and_cluster_lead_changes() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data/n4-journal");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!(
        "{}.sqlite3",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    {
        let store = NewsStore::open(&path).unwrap();
        assert!(store.claim_notification("keyword", "item").unwrap());
        assert!(store.claim_cluster(&["a".into(), "b".into(), "c".into(), "d".into()]).unwrap());
        store.deliver_notifications(&vec!["event".into(); 6], 100, false).unwrap();
    }
    {
        let store = NewsStore::open(&path).unwrap();
        assert!(!store.claim_notification("keyword", "item").unwrap());
        assert!(!store.claim_cluster(&["earlier".into(), "d".into(), "new".into()]).unwrap());
        assert!(store.deliver_notifications(&["next".into()], 101, false).unwrap().0.is_empty());
        assert_eq!(store.deliver_notifications(&[], 700, false).unwrap().0, ["+1 more in News"]);
    }
    std::fs::remove_file(path).unwrap();
}
#[test]
fn n4_first_import_and_retag_are_not_new_alert_identities() {
    let mut store = NewsStore::memory().unwrap();
    let mut item = story("https://fixture.invalid/initial", "NVDA earnings", 101, "NVDA US Equity", "Fixture");
    store.insert(&[item.clone()], 102).unwrap();
    assert!(!store.claim_notification("keyword", &item.id).unwrap());
    assert!(store.notification_blocked(&item.id).unwrap());
    item.tags = vec!["M&A".into()];
    item.subjects = vec!["MSFT US Equity".into()];
    store.insert(&[item.clone()], 103).unwrap();
    assert!(!store.claim_notification("keyword", &item.id).unwrap());
}
#[test]
fn n4_volume_requires_three_same_hour_days_and_both_thresholds() {
    let hour = 20 * 86400;
    let history = (1..=14).map(|d| (hour - d * 86400, 3)).collect::<Vec<_>>();
    let v = local::volume_flag("NVDA", hour, 14, &history, 0).unwrap();
    assert_eq!(v.usual, 3.);
    assert_eq!(v.z, 11.);
    assert!(local::volume_flag("NVDA", hour, 14, &history, hour - 2 * 86400).is_none());
    assert!(local::volume_flag("NVDA", hour, 4, &history, 0).is_none());
    assert!(local::volume_flag("NVDA", hour, 5, &history, 0).is_none());
    assert!(local::volume_flag("NVDA", hour, 6, &history, hour - 3 * 86400).is_some());
    let mut noisy = (1..=14).map(|d| (hour - d * 86400, if d % 2 == 0 { 0 } else { 10 })).collect::<Vec<_>>();
    assert!(local::volume_flag("NVDA", hour, 14, &noisy, 0).is_none());
    noisy.push((hour - 15 * 86400, 10000));
    assert!(local::volume_flag("NVDA", hour, 14, &noisy, 0).is_none());
}
#[test]
fn n4_volume_compares_local_wall_hours_across_fall_dst_change() {
    use chrono::Timelike;
    let ts = |s| chrono::DateTime::parse_from_rfc3339(s).unwrap().timestamp();
    let transition = ts("2026-11-01T06:00:00Z");
    // America/New_York's actual 2026 fall transition, without mutating process TZ.
    let wall = |at| {
        let offset = chrono::FixedOffset::west_opt(if at >= transition { 5 * 3600 } else { 4 * 3600 }).unwrap();
        let t = chrono::DateTime::from_timestamp(at, 0).unwrap().with_timezone(&offset);
        (t.date_naive(), t.hour())
    };
    let now = ts("2026-11-03T09:00:00-05:00");
    let history =
        ["2026-10-31T09:00:00-04:00", "2026-11-01T09:00:00-05:00", "2026-11-02T09:00:00-05:00"].map(|s| (ts(s), 3));
    let flag = local::volume_flag_with("NVDA", now, 14, &history, 0, wall).unwrap();
    assert_eq!(flag.usual, 3.);
    assert_eq!(flag.z, 11.);
    let wrong_hour = history.map(|(at, n)| (at - 3600, n));
    assert!(local::volume_flag_with("NVDA", now, 14, &wrong_hour, 0, wall).is_none());
    // Both occurrences of the repeated 01:00 are one distinct local day.
    let repeated = [(ts("2026-11-01T01:00:00-04:00"), 3), (ts("2026-11-01T01:00:00-05:00"), 3)];
    assert!(local::volume_flag_with("NVDA", ts("2026-11-03T01:00:00-05:00"), 14, &repeated, 0, wall).is_none());
}
#[test]
fn n4_volume_coverage_requires_actual_importer_days_not_backfill_dates() {
    let mut store = NewsStore::memory().unwrap();
    let hour = 20 * 86400;
    let mut items = vec![];
    for day in 0..=3 {
        for i in 0..if day == 0 { 14 } else { 3 } {
            items.push(story(
                &format!("https://fixture.invalid/backfill-{day}-{i}"),
                "NVDA earnings",
                hour - day * 86400 + i,
                "NVDA US Equity",
                "Fixture",
            ));
        }
    }
    store.insert(&items, hour + 120).unwrap();
    assert!(store.volume_anomalies(&["NVDA US Equity".into()], hour + 120).unwrap().is_empty());
}
#[test]
fn n4_inactive_hours_are_not_zero_and_associations_are_not_subjects() {
    let mut store = NewsStore::memory().unwrap();
    let hour = 20 * 86400;
    for day in (0..=14).rev() {
        let count = if day == 0 { 14 } else { 3 };
        let items = (0..count)
            .map(|i| {
                story(
                    &format!("https://fixture.invalid/activity-{day}-{i}"),
                    "NVDA earnings",
                    hour - day * 86400 + i,
                    "MSFT US Equity",
                    "Fixture",
                )
            })
            .collect::<Vec<_>>();
        // Historical days 4–14 were only imported at another hour. They must
        // not lower the same-hour baseline by contributing zeroes.
        store.insert(&items, hour - day * 86400 + if day > 3 { 3600 } else { 120 }).unwrap();
    }
    let volumes = store.volume_anomalies(&["NVDA US Equity".into(), "MSFT US Equity".into()], hour + 120).unwrap();
    assert!(volumes.iter().any(|v| v.key == "NVDA US Equity" && v.usual == 3.));
    assert!(volumes.iter().all(|v| v.key != "MSFT US Equity"));
    assert_eq!(
        volumes.iter().find(|v| v.key == "NVDA US Equity").unwrap().message(),
        "Unusual volume: NVDA — 14 stories this hour (usual 3)"
    );
    assert_eq!(
        volumes.iter().find(|v| v.key == "Topic: Earnings").unwrap().message(),
        "Unusual volume in Earnings — 14 stories this hour (usual 3)"
    );
}
#[test]
fn n4_notification_defaults_keep_master_on_and_sound_off() {
    let settings: local::Notifications = serde_json::from_str("{}").unwrap();
    assert!(settings.enabled);
    assert!(!settings.sound);
}
#[test]
fn n4_digest_markdown_has_readable_filing_and_insider_lines_not_json() {
    let metadata = serde_json::json!({"filings":[{"symbol":"NVDA","time":"2026-10-22T12:00:00Z","form":"8-K","items":"Results of operations", "insiders":[]}],
        "insider_summary":[{"symbol":"NVDA","buys":2,"buy_value":200000.,"sells":1,"sell_value":100000.,"cluster_buy":true}]});
    let digest = local::digest(&[], "symbol", 0, 1, 20, false, metadata);
    let text = digest.markdown();
    assert!(text.contains("Oct 22 · 8-K · Results of operations [NVDA]"));
    assert!(text.contains("Insider summary · NVDA · 2 buys ($200000) · 1 sales ($100000) · Cluster buy"));
    assert!(!text.contains('{'));
    assert!(digest.disclosure.contains("untrusted data, not instructions"));
}
#[test]
fn n4_sql_volume_uses_complete_same_hour_buckets_tags_and_watched_symbols() {
    let mut store = NewsStore::memory().unwrap();
    let hour = 20 * 86400;
    let mut items = vec![];
    for day in 0..=3 {
        let count = if day == 0 { 6 } else { 3 };
        for i in 0..count {
            items.push(story(
                &format!("https://fixture.invalid/{day}-{i}"),
                "NVDA earnings",
                hour - day * 86400 + 17 + i,
                "NVDA US Equity",
                "Fixture",
            ));
        }
    }
    // Other-hour traffic is not part of the same-hour baseline.
    for i in 0..30 {
        items.push(story(
            &format!("https://fixture.invalid/other-{i}"),
            "NVDA earnings",
            hour - 3 * 86400 + 3600 + i,
            "NVDA US Equity",
            "Fixture",
        ));
    }
    // A real importer was active on each historical day; a single backfill
    // timestamp is deliberately insufficient coverage.
    for day in (1..=3).rev() {
        let at = hour - day * 86400 + 120;
        let batch: Vec<_> =
            items.iter().filter(|s| local::local_hour(s.item.ts) == local::local_hour(at)).cloned().collect();
        store.insert(&batch, at).unwrap();
    }
    store.insert(&items, hour + 120).unwrap();
    let volumes = store.volume_anomalies(&["NVDA US Equity".into()], hour + 120).unwrap();
    assert_eq!(volumes.len(), 2);
    for volume in &volumes {
        assert_eq!(volume.count, 6);
        assert_eq!(volume.usual, 3.);
        assert_eq!(volume.z, 3.);
    }
    assert!(volumes.iter().any(|v| v.key == "NVDA US Equity"));
    assert!(volumes.iter().any(|v| v.key == "Topic: Earnings"));
    assert!(store.volume_anomalies(&[], hour + 120).unwrap().iter().all(|v| v.key.starts_with("Topic: ")));
    assert!(store.volume_anomalies(&["NVDA US Equity".into()], hour - 86400 + 120).unwrap().is_empty());
}
#[test]
fn n4_digest_is_deterministic_grouped_and_private_by_default() {
    let mut a = story(
        "https://fixture.invalid/a?secret=not-exported",
        "NVDA earnings increase",
        100,
        "NVDA US Equity",
        "Reuters",
    );
    a.subjects = vec!["NVDA US Equity".into()];
    a.tags = vec!["Earnings".into()];
    a.summary = "é".repeat(500);
    let mut b = story("https://fixture.invalid/b", "Bank acquisition announced", 101, "JPM US Equity", "Fixture");
    b.subjects = vec!["JPM US Equity".into()];
    b.tags = vec!["M&A".into()];
    let metadata = serde_json::json!([{"form":"8-K","items":"2.02 Results of Operations"}]);
    let first = local::digest(&[b.clone(), a.clone()], "watchlist", 0, 200, 50, false, metadata.clone());
    let second = local::digest(&[a.clone(), b.clone()], "watchlist", 0, 200, 50, false, metadata.clone());
    assert_eq!(first, second);
    assert_eq!(first.groups[0].topic, "Earnings");
    assert_eq!(first.groups[0].subject, "NVDA US Equity");
    let text = serde_json::to_string(&first).unwrap();
    assert!(!text.contains("\"summary\":"));
    assert!(!text.contains("not-exported"));
    assert_eq!(first.groups[0].stories[0].url_host, "fixture.invalid");
    let markdown = first.markdown();
    assert!(markdown.contains("8-K"));
    for group in &first.groups {
        for story in &group.stories {
            let line = markdown.lines().find(|line| line.contains(&story.headline)).unwrap();
            assert!(line.contains(&story.url_host));
            for source in &story.sources {
                assert!(line.contains(source), "per-story source missing: {line}");
            }
        }
    }
    assert!(!markdown.contains("not-exported"));
    assert!(!markdown.contains("https://"));
    assert!(!markdown.contains("é"));
    let full = local::digest(&[a, b], "watchlist", 0, 200, 1, true, metadata);
    assert_eq!(full.groups.iter().map(|g| g.stories.len()).sum::<usize>(), 1);
    let summary = local::digest(&[second_story_for_summary()], "market", 0, 200, 50, true, serde_json::json!([]));
    assert_eq!(summary.groups[0].stories[0].summary.as_ref().unwrap().chars().count(), 300);
}
#[test]
fn n5c_digest_exports_cluster_priority_in_json_and_markdown() {
    let a = story("https://fixture.invalid/a", "Acme revenue results today", 100, "", "Reuters");
    let b = story("https://fixture.invalid/b", "BREAKING: Acme revenue results today", 101, "", "CNBC");
    let digest = local::digest(&[a, b], "market", 0, 200, 50, false, serde_json::json!([]));
    let json = serde_json::to_value(&digest).unwrap();
    assert_eq!(json["groups"][0]["stories"][0]["priority"], "urgent");
    assert_eq!(json["groups"][0]["stories"][0]["event"], "URGENT");
    assert!(digest.markdown().contains("event: URGENT"));
    assert_eq!(json["groups"][0]["stories"][0]["source_count"], 2);
    assert!(digest.markdown().contains("priority: urgent"));
    assert_eq!(digest.groups[0].stories[0].headline, "Acme revenue results today");
    let mut legacy = json["groups"][0]["stories"][0].clone();
    legacy.as_object_mut().unwrap().remove("priority");
    legacy.as_object_mut().unwrap().remove("event");
    let legacy: local::DigestStory = serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.priority, crate::news_intelligence::Priority::Normal);
    assert_eq!(legacy.event, None);
}
#[test]
fn n5c_digest_markdown_omits_only_normal_priority() {
    let stories = [
        story("https://fixture.invalid/routine", "Routine bulletin", 100, "", "Reuters"),
        story("https://fixture.invalid/high", "Acme beats estimates", 101, "", "CNBC"),
        story("https://fixture.invalid/urgent", "URGENT: exchange notice", 102, "", "Bloomberg"),
    ];
    let digest = local::digest(&stories, "market", 0, 200, 50, false, serde_json::json!([]));
    let markdown = digest.markdown();
    assert!(!markdown.contains("priority: normal"));
    assert!(markdown.contains("priority: high · event: EARNINGS"));
    assert!(markdown.contains("priority: urgent · event: URGENT"));
    assert!(!markdown.lines().find(|line| line.contains("Routine bulletin")).unwrap().contains("event:"));
    let json = serde_json::to_value(&digest).unwrap();
    assert!(
        json["groups"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["stories"].as_array().unwrap())
            .any(|s| s["priority"] == "normal")
    );
}
fn second_story_for_summary() -> Story {
    let mut s = story("https://fixture.invalid/s", "Summary fixture", 100, "", "Fixture");
    s.summary = "é".repeat(500);
    s
}
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
fn polish_short_search_terms_and_exact_tickers_agree_in_all_paths() {
    let mut db = NewsStore::memory().unwrap();
    let mut items = vec![
        story("https://fixture.test/ge", "GE Vernova to Acquire Trench Group", 100, "", "Fixture"),
        story("https://fixture.test/ticker", "Industrial outlook", 101, "ge US Equity", "Fixture"),
        story("https://fixture.test/gev", "Power equipment update", 102, "GEV", "Fixture"),
        story("https://fixture.test/get", "Investors get ready", 103, "", "Fixture"),
        story("https://fixture.test/generally", "Markets generally rise", 104, "", "Fixture"),
        story("https://fixture.test/german", "German factories recover", 105, "", "Fixture"),
        story("https://fixture.test/crypto", "Digital asset update", 106, "BTCUSD", "Fixture"),
        story("https://fixture.test/summary", "Summary only fixture", 107, "", "Fixture"),
    ];
    items[7].summary = "GE orders improve".into();
    db.insert(&items, 110).unwrap();
    let mut q = Query { filters: Filters { scope: Scope::All, ..Default::default() }, ..Default::default() };
    for (search, expected) in [
        ("ge", vec![7, 1, 0]),
        ("GE", vec![7, 1, 0]),
        ("verno", vec![0]),
        ("gev", vec![2]),
        ("btcusd", vec![6]),
        ("btc", vec![]),
        ("ge verno", vec![0]),
        ("ge industrial", vec![1]),
        ("ge get", vec![]),
        ("get", vec![3]),
        ("germ", vec![5]),
        ("gener", vec![4]),
        ("g", vec![]),
        ("ge%_", vec![7, 1, 0]),
        ("\" OR 1=1 --", vec![]),
    ] {
        q.filters.search = search.into();
        db.fts = true;
        let fts = db.query(&q).unwrap();
        let expected: Vec<_> = expected.into_iter().map(|i| items[i].id.clone()).collect();
        assert_eq!(fts.iter().map(|s| s.id.clone()).collect::<Vec<_>>(), expected, "FTS: {search}");
        db.fts = false;
        assert_eq!(db.query(&q).unwrap(), fts, "fallback: {search}");
        for item in &items {
            assert_eq!(matches(item, &q), expected.contains(&item.id), "retained: {search}: {}", item.item.title);
        }
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
        assert!(!rows[0].item.tickers.contains(&subject.into()), "subjects must not expand scope");
        assert!(rows[0].seen);
        assert_eq!(rows[0].provider, "Finnhub");
    }
    let q = Query { symbols: vec!["FROG US Equity".into()], ..Default::default() };
    assert!(db.query(&q).unwrap().is_empty(), "inferred subjects never join association membership");
    let q = Query { symbols: vec!["MSFT US Equity".into()], ..Default::default() };
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
    let cached_companies = db.edgar.as_ref().unwrap().companies.as_ptr();
    source.execute("INSERT INTO meta VALUES('filing','new')", []).unwrap();
    db.refresh_names(&path).unwrap();
    assert_eq!(
        db.edgar.as_ref().unwrap().companies.as_ptr(),
        cached_companies,
        "filing commits do not reload companies"
    );
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
    assert_eq!(rows[0].item.tickers, vec!["MSFT US Equity"], "names never contaminate association history");
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
#[test]
fn review_read_only_store_still_keeps_edgar_name_index() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!(
        "review-ro-edgar-{}-{}.sqlite3",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    let source = Connection::open(&path).unwrap();
    source
        .execute_batch(
            "CREATE TABLE companies(ticker TEXT PRIMARY KEY,cik INTEGER,title TEXT);
            INSERT INTO companies VALUES('FROG',2,'JFrog Ltd');",
        )
        .unwrap();
    let mut db = NewsStore::memory().unwrap();
    db.db.execute_batch("PRAGMA query_only=ON").unwrap();
    let _ = db.refresh_names(&path);
    let found = db.names.as_ref().map(|n| n.subjects("JFrog rises", "", &[]));
    drop(db);
    drop(source);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(found, Some(vec!["FROG US Equity".to_string()]));
}

#[test]
fn matcher_version_invalidates_old_subjects_without_expanding_associations() {
    let mut db = NewsStore::memory().unwrap();
    let now = chrono::Utc::now().timestamp();
    let s = story("https://fixture.test/version", "Alphabet and Amazon expand", now, "MSFT US Equity", "Reuters");
    db.insert(&[s], now).unwrap();
    db.db.execute("UPDATE items SET subjects='[\"HERE US Equity\"]'", []).unwrap();
    db.set_meta("subject_matcher", "old-matcher").unwrap();
    // Re-open the same connection as a startup migration, retaining all columns.
    let db = NewsStore::init(db.db).unwrap();
    assert_eq!(db.meta("subject_matcher").unwrap().as_deref(), Some(crate::news_intelligence::MATCHER_VERSION));
    assert_eq!(db.db.query_row("SELECT intelligence FROM items", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    let rows = db.query(&Query::default()).unwrap();
    assert_eq!(rows[0].subjects, ["GOOGL US Equity", "AMZN US Equity"]);
    assert_eq!(rows[0].item.tickers, ["MSFT US Equity"]);
}
#[test]
fn read_only_query_never_displays_stale_subjects_after_invalidation() {
    let mut db = NewsStore::memory().unwrap();
    let s = story("https://fixture.test/stale", "Alphabet expands", 100, "MSFT US Equity", "Reuters");
    db.insert(&[s], 101).unwrap();
    db.db.execute("UPDATE items SET subjects='[\"HERE US Equity\"]',intelligence=1", []).unwrap();
    db.db.execute_batch("PRAGMA query_only=ON").unwrap();
    assert!(db.query(&Query::default()).unwrap()[0].subjects.is_empty(), "failed backfill must mute old subjects");
    // Simulate a newly installed SEC index whose invalidation transaction failed.
    db.pending_names = Some("new-index".into());
    let rows = db.query(&Query::default()).unwrap();
    assert_eq!(rows[0].subjects, ["GOOGL US Equity"]);
    assert_eq!(rows[0].item.tickers, ["MSFT US Equity"]);
}
#[test]
fn stored_subjects_and_watchlist_evidence_never_expand_name_based_scope() {
    let mut db = NewsStore::memory().unwrap();
    db.names = Some(crate::news_intelligence::NameIndex::new(&[(1, "FROG".into(), "JFrog Ltd".into())]));
    let name =
        story("https://fixture.test/subjects-only", "JFrog and Alphabet expand", 100, "MSFT US Equity", "Reuters");
    let bare = story("https://fixture.test/watched-bare", "FROG expands", 100, "MSFT US Equity", "Reuters");
    db.insert(&[name, bare], 101).unwrap();
    db.set_watched(&["FROG US Equity".into()]).unwrap();
    let all = db.query(&Query::default()).unwrap();
    assert!(all.iter().all(|s| s.subjects.contains(&"FROG US Equity".into())));
    assert_eq!(
        db.query(&Query { symbols: vec!["FROG US Equity".into()], ..Default::default() }).unwrap().len(),
        1,
        "only the explicitly written watched ticker joins scope"
    );
    let by_name = all.iter().find(|s| s.item.title.starts_with("JFrog")).unwrap();
    assert_eq!(by_name.item.tickers, ["MSFT US Equity"]);
    db.set_watched(&[]).unwrap();
    let all = db.query(&Query::default()).unwrap();
    // An admitted explicit ticker is retained as an association; names still never leak.
    assert_eq!(all.iter().find(|s| s.item.title.starts_with("JFrog")).unwrap().item.tickers, ["MSFT US Equity"]);
}

#[test]
fn review_volume_single_old_item_fakes_three_days_of_history() {
    let mut store = NewsStore::memory().unwrap();
    let hour = 20 * 86400;
    // First import: one stale story (10 days old) plus a busy current hour.
    let mut items =
        vec![story("https://fixture.invalid/old", "Old headline", hour - 10 * 86400, "AAPL US Equity", "Fixture")];
    for i in 0..5 {
        items.push(story(
            &format!("https://fixture.invalid/now-{i}"),
            &format!("Company {i} quarterly earnings"),
            hour + 60 + i,
            "MSFT US Equity",
            &format!("Source {i}"),
        ));
    }
    store.insert(&items, hour + 120).unwrap();
    // No same-hour history exists for "Topic: Earnings"; the plan requires >= 3 days of it.
    assert!(store.volume_anomalies(&[], hour + 120).unwrap().is_empty());
}

// ---- N5a production-readiness review ----
fn review_n5a_old_db(tag: &str) -> std::path::PathBuf {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-data/review-n5a");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!(
        "{tag}-{}-{}.sqlite3",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap()
    ));
    let now = chrono::Utc::now().timestamp();
    let db = rusqlite::Connection::open(&path).unwrap();
    // Exact pre-N5a schema (6ca1d5a~1 init + its column migrations).
    db.execute_batch(
        "PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS items(id TEXT PRIMARY KEY, source TEXT NOT NULL, provider_id TEXT NOT NULL,
        url TEXT NOT NULL, headline TEXT NOT NULL, summary TEXT NOT NULL, published INTEGER NOT NULL,
        tickers TEXT NOT NULL, category TEXT NOT NULL, fetched INTEGER NOT NULL, seen INTEGER NOT NULL DEFAULT 0,
        bookmark INTEGER NOT NULL DEFAULT 0);
        CREATE INDEX IF NOT EXISTS news_published ON items(published DESC);
        CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS notification_seen(kind TEXT NOT NULL,id TEXT NOT NULL,PRIMARY KEY(kind,id));
        CREATE TABLE IF NOT EXISTS notification_history(at INTEGER NOT NULL,body TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS notification_slots(at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS news_fetches(at INTEGER PRIMARY KEY);
        ALTER TABLE items ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE items ADD COLUMN intelligence INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE items ADD COLUMN provider TEXT NOT NULL DEFAULT '';
        ALTER TABLE items ADD COLUMN subjects TEXT NOT NULL DEFAULT '[]';
        CREATE VIRTUAL TABLE IF NOT EXISTS news_fts USING fts5(id UNINDEXED, headline, summary);
        CREATE TRIGGER IF NOT EXISTS news_fts_insert AFTER INSERT ON items BEGIN
            INSERT INTO news_fts VALUES(new.id,new.headline,new.summary); END;
        CREATE TRIGGER IF NOT EXISTS news_fts_delete AFTER DELETE ON items BEGIN
            DELETE FROM news_fts WHERE id=old.id; END;",
    )
    .unwrap();
    for (id, published, fetched) in
        [("live", now - 600, now - 590), ("week", now - 7 * 86400, now - 7 * 86400 + 5), ("undated", 0, now - 60)]
    {
        db.execute(
            "INSERT INTO items(id,source,provider_id,url,headline,summary,published,tickers,category,fetched,provider)
             VALUES(?1,'Fixture','',?2,?3,'',?4,'[\"NVDA US Equity\"]','general',?5,'Fixture')",
            rusqlite::params![
                id,
                format!("https://fixture.invalid/{id}"),
                format!("Headline {id} Nvidia"),
                published,
                fetched
            ],
        )
        .unwrap();
    }
    for (kind, id) in [
        ("keyword", "live"),
        ("keyword-observed", "week"),
        ("breaking-member", "live"),
        ("breaking-block", "week"),
        ("volume", "NVDA US Equity:123"),
        ("keyword", "already-pruned-item"),
    ] {
        db.execute("INSERT INTO notification_seen VALUES(?1,?2)", [kind, id]).unwrap();
    }
    drop(db);
    path
}

#[test]
fn review_n5a_migration_from_pre_n5a_db_keeps_rows_and_claims_and_is_idempotent() {
    let path = review_n5a_old_db("migrate");
    let now = chrono::Utc::now().timestamp();
    for round in 0..3 {
        let store = NewsStore::open(&path).unwrap_or_else(|e| panic!("open round {round}: {e}"));
        let items: i64 = store.db.query_row("SELECT count(*) FROM items", [], |r| r.get(0)).unwrap();
        assert_eq!(items, 3, "round {round}: no item lost");
        let claims: i64 = store.db.query_row("SELECT count(*) FROM notification_seen", [], |r| r.get(0)).unwrap();
        assert_eq!(claims, 6, "round {round}: no claim lost");
        let zero: i64 =
            store.db.query_row("SELECT count(*) FROM notification_seen_at WHERE at<=0", [], |r| r.get(0)).unwrap();
        assert_eq!(zero, 0, "round {round}: every migrated claim has a timestamp");
        let stamped: i64 = store
            .db
            .query_row(
                "SELECT count(*) FROM notification_seen JOIN notification_seen_at USING(kind,id) WHERE at>=?1",
                [now],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stamped, 6, "unknown ages must be backfilled at migration time");
        assert!(store.notification_claimed("keyword", "live").unwrap());
        assert!(store.notification_claimed("volume", "NVDA US Equity:123").unwrap());
        assert!(!store.claim_cluster(&["live".into(), "new-member".into()]).unwrap(), "live cluster identity kept");
        let (published, inferred): (i64, bool) = store
            .db
            .query_row("SELECT published,inferred_ts FROM items WHERE id='undated'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(published, now - 60, "undated row adopts its fetch time");
        assert!(inferred);
        let q = Query {
            filters: Filters { scope: Scope::All, search: "Nvidia".into(), ..Default::default() },
            ..Default::default()
        };
        assert_eq!(store.query(&q).unwrap().len(), 3, "round {round}: FTS still finds migrated rows");
        // A later prune must not drop any of these young claims.
        store.prune(now).unwrap();
        // remove the claim the test itself added so counts stay stable across rounds
        store.db.execute("DELETE FROM notification_seen WHERE id='new-member'", []).unwrap();
    }
    let _ = std::fs::remove_file(&path);
}

/// The user runs several worktree builds against one data dir. An older binary's
/// positional `INSERT ... VALUES(?1,?2)` into notification_seen stops working once
/// N5a adds the `at` column, so the old build can never record a claim again.
#[test]
fn review_n5a_pre_n5a_build_can_still_claim_after_migration() {
    let path = review_n5a_old_db("downgrade");
    drop(NewsStore::open(&path).unwrap());
    let db = rusqlite::Connection::open(&path).unwrap();
    // Verbatim statement used by the pre-N5a claim_notification / claim_cluster.
    let result = db.execute("INSERT OR IGNORE INTO notification_seen VALUES(?1,?2)", ["keyword", "fresh-story"]);
    drop(db);
    assert!(result.is_ok(), "pre-N5a claim insert fails after migration: {result:?}");
    let now = chrono::Utc::now().timestamp();
    let store = NewsStore::open(&path).unwrap();
    assert!(store.notification_claimed("keyword", "fresh-story").unwrap());
    let at: i64 = store
        .db
        .query_row("SELECT at FROM notification_seen_at WHERE kind='keyword' AND id='fresh-story'", [], |r| r.get(0))
        .unwrap();
    assert!(at >= now, "old-build claims receive a migration-time age on next open");
    let columns: i64 =
        store.db.query_row("SELECT count(*) FROM pragma_table_info('notification_seen')", [], |r| r.get(0)).unwrap();
    assert_eq!(columns, 2, "keep the original claim schema");
    drop(store);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn n5a_dev_at_column_is_ignored_and_side_table_controls_pruning() {
    let path = review_n5a_old_db("dev-at");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "ALTER TABLE notification_seen ADD COLUMN at INTEGER NOT NULL DEFAULT 0;
        CREATE INDEX notification_seen_at ON notification_seen(at);
        UPDATE notification_seen SET at=1;",
    )
    .unwrap();
    drop(db);
    let now = chrono::Utc::now().timestamp();
    let store = NewsStore::open(&path).unwrap();
    assert!(store.notification_claimed("keyword", "live").unwrap(), "legacy column age is not authoritative");
    assert!(store.claim_notification("keyword", "dev-new").unwrap());
    let at: i64 =
        store.db.query_row("SELECT at FROM notification_seen_at WHERE id='dev-new'", [], |r| r.get(0)).unwrap();
    assert!(at >= now);
    store.db.execute("UPDATE notification_seen_at SET at=?1 WHERE id='dev-new'", [now - 91 * 86400]).unwrap();
    store.prune(now).unwrap();
    assert!(!store.notification_claimed("keyword", "dev-new").unwrap());
    let remaining: i64 =
        store.db.query_row("SELECT count(*) FROM notification_seen_at WHERE id='dev-new'", [], |r| r.get(0)).unwrap();
    assert_eq!(remaining, 0);
    drop(store);
    assert!(NewsStore::open(&path).unwrap().notification_claimed("keyword", "live").unwrap());
    let _ = std::fs::remove_file(path);
}

#[test]
fn n5a_failed_migration_rolls_back_all_schema_steps() {
    let path = review_n5a_old_db("rollback");
    let db = Connection::open(&path).unwrap();
    // Force a later migration step to fail after the item ALTERs have run.
    db.execute_batch("DROP TABLE news_fetches; CREATE TABLE notification_seen_at(bad TEXT);").unwrap();
    assert!(NewsStore::open(&path).is_err());
    let inferred: i64 = db
        .query_row("SELECT count(*) FROM pragma_table_info('items') WHERE name='inferred_ts'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(inferred, 0, "failed migration must not leave partial item ALTERs");
    let tables: i64 =
        db.query_row("SELECT count(*) FROM sqlite_master WHERE name='news_fetches'", [], |r| r.get(0)).unwrap();
    assert_eq!(tables, 0, "initial CREATE steps roll back too");
    db.execute_batch("DROP TABLE notification_seen_at;").unwrap();
    drop(db);
    assert!(NewsStore::open(&path).is_ok(), "retry after fixing the obstruction succeeds");
    let _ = std::fs::remove_file(path);
}

/// A7 + A2 together: an undated item gets ts = first ingest. After 90 days both the
/// row and its keyword claim are pruned (claim `at` == first-ingest wall clock). If the
/// feed still carries the item, the next ingest gives it ts = now again, so it is
/// fresh and unclaimed, and the keyword watch fires a second time for the same story.
#[test]
fn review_n5a_pruned_undated_story_is_not_fresh_again_on_reingest() {
    let now = chrono::Utc::now().timestamp();
    let first = now - 91 * 86400;
    let mut store = NewsStore::memory().unwrap();
    let undated = story("https://fixture.invalid/evergreen", "NVDA earnings explainer", 0, "NVDA US Equity", "Fixture");
    store.insert(&[story("https://fixture.invalid/seed", "seed", first - 10, "", "Fixture")], first).unwrap();
    store.insert(std::slice::from_ref(&undated), first).unwrap();
    // Delivered at first ingest (claim stamped with that day's wall clock).
    assert!(store.claim_notification("keyword", &undated.id).unwrap());
    store.db.execute("UPDATE notification_seen_at SET at=?1", [first]).unwrap();
    // Other live history exists, so this is not a first-import suppression.
    store.insert(&[story("https://fixture.invalid/recent", "recent", now - 600, "", "Fixture")], now - 600).unwrap();
    store.prune(now).unwrap();
    store.insert(std::slice::from_ref(&undated), now).unwrap();
    let published: i64 =
        store.db.query_row("SELECT published FROM items WHERE id=?1", [&undated.id], |r| r.get(0)).unwrap();
    let session = now - 3600;
    let reingested = store.query(&Query::default()).unwrap().into_iter().find(|s| s.id == undated.id).unwrap();
    assert!(reingested.inferred_ts);
    assert!(!store.notification_claimed("keyword", &undated.id).unwrap(), "old claim really was pruned");
    assert_eq!(store.latest("NVDA US Equity").unwrap(), None, "inferred time must not advance the Finnhub cursor");
    let would_notify = local::fresh_story(&reingested, session, session, now + 1)
        && !store.notification_claimed("keyword", &undated.id).unwrap();
    assert!(!would_notify, "same undated story is fresh and unclaimed again (published={published}, now={now})");
}

#[test]
fn n8_keyed_story_body_refusals_never_create_rows() {
    let store = NewsStore::memory().unwrap();
    for key in [
        "token",
        "apikey",
        "api-key",
        "api_key",
        "access_token",
        "x-finnhub-token",
        "x-amz-signature",
        "x-amz-credential",
        "signature",
        "sig",
        "client_secret",
        "auth",
        "key",
        "secret",
        "password",
    ] {
        let url = format!("https://fixture.invalid/story?{}=must-not-persist", key.to_ascii_uppercase());
        for status in ["ok", "publisher not fetched (provider key)"] {
            let mut body = StoryBody::of(&url, 1_800_000_000, status);
            body.text = "must not persist".into();
            store.save_story_body(&body).unwrap();
        }
    }
    let refused = StoryBody::of(
        "https://fixture.invalid/redacted",
        1_800_000_000,
        &tr_net::publisher::Reason::KeyedLink.to_string(),
    );
    store.save_story_body(&refused).unwrap();
    let count: i64 = store.db.query_row("SELECT count(*) FROM story_body", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 0, "even negative-cache URLs must not retain credentials");
}

#[test]
fn n8_noarchive_flag_overrides_ok_and_removes_previously_stored_text() {
    let store = NewsStore::memory().unwrap();
    for (html_flag, header_flag) in [(true, false), (false, true), (true, true)] {
        let mut body = StoryBody::of("https://fixture.invalid/story", 1_800_000_000, "ok");
        body.text = "must not persist".into();
        body.chars = 16;
        body.truncated = true;
        body.etag = Some("private-validator".into());
        store.save_story_body(&body).unwrap();
        body.noarchive = html_flag || header_flag;
        store.save_story_body(&body).unwrap();
        let cached = store.story_body(&body.url, body.fetched_at + 1).unwrap().unwrap();
        assert!(cached.noarchive);
        assert_eq!(cached.status, "noarchive");
        assert!(cached.text.is_empty());
        assert_eq!(cached.chars, 0);
        assert!(!cached.truncated);
        assert!(cached.etag.is_none());
    }
}

#[test]
fn n8_story_body_round_trips_and_failure_rows_expire_after_a_day() {
    let now = 1_800_000_000;
    let store = NewsStore::memory().unwrap();
    let mut body = StoryBody::of("https://fixture.invalid/story?utm_source=feed", now, "ok");
    body.published_at = Some(now - 3_600);
    body.chars = 42;
    body.truncated = true;
    body.etag = Some("\"fixture\"".into());
    body.text = "Article body".into();
    store.save_story_body(&body).unwrap();

    let cached = store.story_body("https://fixture.invalid/story", now + 10).unwrap().unwrap();
    assert_eq!(cached.url, "https://fixture.invalid/story");
    assert_eq!(cached.text, "Article body");
    assert_eq!(cached.chars, 42);
    assert!(cached.truncated);
    assert_eq!(cached.etag.as_deref(), Some("\"fixture\""));
    assert_eq!(cached.published_at, Some(now - 3_600));

    let failure = StoryBody::of("https://fixture.invalid/failed", now, "no article text");
    store.save_story_body(&failure).unwrap();
    assert!(store.story_body("https://fixture.invalid/failed", now + 3_600).unwrap().is_some());
    assert!(store.story_body("https://fixture.invalid/failed", now + STORY_BODY_NEGATIVE_TTL + 1).unwrap().is_none());
}

#[test]
fn n8_story_body_never_stores_noarchive_text() {
    let now = 1_800_000_000;
    let store = NewsStore::memory().unwrap();
    let mut body = StoryBody::of("https://fixture.invalid/noarchive", now, "noarchive");
    body.text = "must not persist".into();
    body.chars = 16;
    store.save_story_body(&body).unwrap();

    let cached = store.story_body("https://fixture.invalid/noarchive", now + 60).unwrap().unwrap();
    assert_eq!(cached.status, "noarchive");
    assert_eq!(cached.text, "");
    assert_eq!(cached.chars, 0);
}

#[test]
fn n8_story_body_failure_rows_never_store_text() {
    let now = 1_800_000_000;
    let store = NewsStore::memory().unwrap();
    let mut body = StoryBody::of("https://fixture.invalid/paywalled", now, "paywalled");
    body.text = "teaser".into();
    body.chars = 6;
    body.truncated = true;
    store.save_story_body(&body).unwrap();

    let cached = store.story_body("https://fixture.invalid/paywalled", now + 60).unwrap().unwrap();
    assert_eq!(cached.status, "paywalled");
    assert_eq!(cached.text, "");
    assert_eq!(cached.chars, 0);
    assert!(!cached.truncated);
}

#[test]
fn n8_story_body_prune_matches_ninety_day_headline_retention() {
    let now = 1_800_000_000;
    let store = NewsStore::memory().unwrap();
    let old = StoryBody::of("https://fixture.invalid/old-body", now - STORY_BODY_RETENTION - 1, "ok");
    let fresh = StoryBody::of("https://fixture.invalid/fresh-body", now - STORY_BODY_RETENTION + 1, "ok");
    store.save_story_body(&old).unwrap();
    store.save_story_body(&fresh).unwrap();
    store.prune(now).unwrap();

    assert!(store.story_body("https://fixture.invalid/old-body", now).unwrap().is_none());
    assert!(store.story_body("https://fixture.invalid/fresh-body", now).unwrap().is_some());
}
