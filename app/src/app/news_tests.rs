use super::*;

fn n7_story(id: &str, title: &str, ts: i64) -> tr_data::news_store::Story {
    let mut story = tr_data::news_store::Story::new(
        tr_data::NewsItem {
            title: title.into(),
            link: format!("https://example.test/{id}"),
            ts,
            tickers: vec!["ACME US Equity".into()],
            publisher: "Reuters".into(),
        },
        "Reuters",
    );
    story.id = id.into();
    story.category = "tech".into();
    story
}

#[test]
fn n7b_flash_pick_expiry_dismiss_and_newer_rearm() {
    let now = 100_000;
    let old = Cluster::wire(&n7_story("old", "Acme raises guidance", now - 30));
    let new = Cluster::wire(&n7_story("new", "Beta trading halted", now - 10));
    let mut flash = Flash::default();
    let stories = vec![old.clone(), new.clone()];
    assert_eq!(flash.pick(&stories, now).unwrap().lead.id, "new");
    assert!(flash.pick(&stories, now + 590).is_none(), "expiry must not fall back to an older flash");
    flash.dismiss(&new);
    assert!(flash.pick(&stories, now).is_none());
    assert!(flash.pick(&[old], now).is_none(), "filter changes cannot resurrect an older alert");
    let newer = Cluster::wire(&n7_story("newer", "Urgent: gamma factory closed", now + 1));
    assert_eq!(flash.pick(std::slice::from_ref(&newer), now + 1).unwrap().lead.id, "newer");
    flash.dismiss(&newer);
    let mut changed_lead = newer.clone();
    changed_lead.lead.id = "another-publisher".into();
    changed_lead.member_ids.push("another-publisher".into());
    assert!(flash.pick(&[changed_lead], now + 2).is_none(), "cluster lead changes must not undo dismissal");
}

#[test]
fn n7b_flash_arrival_is_monotonic_and_history_is_not_rearmed() {
    let now = 100_000;
    let historical = n7_story("history", "Acme raises guidance", now - 3600);
    let mut flash = Flash::default();
    flash.observe(std::slice::from_ref(&historical), now);
    assert!(flash.pick(&[Cluster::wire(&historical)], now).is_none());
    let delayed = n7_story("delayed", "Beta trading halt", now - 3000);
    flash.observe(std::slice::from_ref(&delayed), now + 1);
    let stories = [Cluster::wire(&delayed)];
    assert!(flash.pick(&stories, now + 600).is_some(), "new deliveries expire by arrival, not publication");
    flash.observe(&[delayed], now + 601);
    assert!(flash.pick(&stories, now + 601).is_none(), "repeat delivery cannot restart the clock");
}

#[test]
fn review_n7_flash_older_history_delivered_later_is_not_fresh() {
    let now = chrono::Utc::now().timestamp();
    let mut pane = NewsPane::new(None);
    pane.filters.scope = Scope::All;
    pane.filters.days = 1;
    pane.flash_high = true;
    // First delivery: today's window (or an empty store on first run).
    pane.apply(vec![n7_story("today", "Routine market report", now - 60)]);
    // The user widens the date range to 7 days; the worker returns older saved history.
    let old = n7_story("old-high", "Acme raises guidance", now - 3 * 86400);
    pane.filters.days = 7;
    pane.apply(vec![n7_story("today", "Routine market report", now - 60), old]);
    let stories = flash_stories(&pane, &[], now);
    assert!(stories.iter().any(|s| s.lead.id == "old-high"));
    assert!(
        pane.interaction.flash.pick(&stories, now).is_none(),
        "a three-day-old saved headline must not appear as a fresh FLASH"
    );
}

#[test]
fn flash_empty_first_load_and_older_navigation_never_restart_arrival() {
    let now = 1_000_000;
    let old = n7_story("old", "Acme raises guidance", now - 3 * 86400);
    let mut flash = Flash::default();
    flash.observe(&[], now);
    assert_eq!(flash.newest_seen, None);
    flash.observe(std::slice::from_ref(&old), now + 1);
    assert!(flash.pick(&[Cluster::wire(&old)], now + 1).is_none());
    let latest = n7_story("latest", "Routine market report", now - 10);
    flash.observe(&[latest], now + 2);
    // select_related applies a snapshot containing only older members.
    let other = n7_story("related-old", "Beta trading halted", now - 2 * 86400);
    flash.observe(std::slice::from_ref(&other), now + 3);
    assert!(flash.pick(&[Cluster::wire(&other)], now + 3).is_none());
    assert_eq!(flash.newest_seen, Some(now - 10));
}

#[test]
fn flash_retention_is_bounded_and_eviction_cannot_rearm_old_ids() {
    let now = 1_000_000;
    let items: Vec<_> = (0..2 * intelligence::MAX_ITEMS + 20)
        .map(|i| n7_story(&format!("bounded-{i}"), "Acme raises guidance", now))
        .collect();
    let mut flash = Flash::default();
    flash.observe(&items, now);
    assert_eq!(flash.arrivals.len(), 2 * intelligence::MAX_ITEMS);
    let mut cluster = Cluster::wire(&items[0]);
    cluster.member_ids = items.iter().map(|s| s.id.clone()).collect();
    flash.dismiss(&cluster);
    assert_eq!(flash.dismissed.len(), 2 * intelligence::MAX_ITEMS);
    flash.observe(&[], now + FLASH_SECONDS);
    assert!(flash.arrivals.is_empty());
    assert!(flash.dismissed.is_empty());
    flash.observe(&items, now + FLASH_SECONDS + 1);
    assert!(flash.pick(&[Cluster::wire(&items[0])], now + FLASH_SECONDS + 1).is_none());
}

#[test]
fn n7b_flash_obeys_every_pane_filter_except_date_and_sort() {
    let now = 1_000_000;
    let mut pane = NewsPane::new(Some("ACME US Equity".into()));
    pane.filters.days = 1;
    pane.items = vec![
        n7_story("high", "Acme raises guidance for quantum chips", now - 2 * 86400),
        n7_story("normal", "Routine market report", now),
    ];
    let watches = vec![KeywordWatch { words: "quantum".into(), ..Default::default() }];
    let allowed = |p: &NewsPane| flash_stories(p, &watches, now).iter().any(|s| s.lead.id == "high");
    assert!(allowed(&pane), "date is excluded and normal priority is not picked");
    assert_eq!(flash_stories(&pane, &watches, now).len(), 1);
    pane.filters.sources = vec!["Other publisher".into()];
    assert!(!allowed(&pane));
    pane.filters.sources = vec!["Reuters".into()];
    assert!(allowed(&pane));
    pane.filters.sources.clear();
    pane.filters.categories = vec!["macro".into()];
    assert!(!allowed(&pane));
    pane.filters.categories = vec!["tech".into()];
    assert!(allowed(&pane));
    pane.filters.categories.clear();
    pane.filters.include = "absent".into();
    assert!(!allowed(&pane));
    pane.filters.include = "quantum".into();
    assert!(allowed(&pane));
    pane.filters.include.clear();
    pane.filters.exclude = "quantum".into();
    assert!(!allowed(&pane));
    pane.filters.exclude.clear();
    pane.filters.search = "absent".into();
    assert!(!allowed(&pane));
    pane.filters.search.clear();
    pane.filters.saved = true;
    assert!(!allowed(&pane));
    pane.items[0].bookmark = true;
    assert!(allowed(&pane));
    pane.filters.saved = false;
    pane.display.unread = true;
    pane.items[0].seen = true;
    assert!(!allowed(&pane));
    pane.items[0].seen = false;
    assert!(allowed(&pane));
    pane.display.unread = false;
    pane.display.watch_hits = true;
    assert!(allowed(&pane));
    assert!(flash_stories(&pane, &[], now).is_empty());
    pane.display.watch_hits = false;
    pane.symbol = Some("OTHER US Equity".into());
    assert!(!allowed(&pane));
    pane.filters.scope = Scope::Watchlist;
    assert!(!allowed(&pane));
    pane.filters.scope = Scope::All;
    assert!(allowed(&pane));
    pane.items.clear();
    pane.pending = vec![n7_story("pending", "Acme raises guidance", now)];
    assert_eq!(flash_stories(&pane, &[], now)[0].lead.id, "pending", "frozen lists still expose flash arrivals");
}

#[test]
fn n7b_tape_window_priority_keywords_and_cluster_dedup() {
    let now = 100_000;
    let high = n7_story("high", "Acme raises guidance", now - 1);
    let mut duplicate = high.clone();
    duplicate.id = "duplicate".into();
    duplicate.source = "CNBC".into();
    let keyword = n7_story("keyword", "Quantum laboratory opens in Zurich", now - 2);
    let normal = n7_story("normal", "Routine weather bulletin", now);
    let old = n7_story("old", "Delta files bankruptcy", now - TAPE_SECONDS);
    let future = n7_story("future", "Fed cuts rates", now + 1);
    let mut inferred = n7_story("inferred", "CEO resigns", now);
    inferred.inferred_ts = true;
    let watches = [KeywordWatch { words: "quantum".into(), ..Default::default() }];
    let items = [high.clone(), duplicate, high, keyword, normal, old, future, inferred];
    let selected = tape_items(&items, &watches, now);
    assert_eq!(selected.len(), 3);
    assert_eq!(selected[0].member_ids.len(), 2, "same ids dedup before clustering publishers");
    assert_eq!(selected[1].lead.id, "keyword");
    assert_eq!(selected[2].lead.id, "normal", "newer ordinary headlines top up after emphasized entries");
    assert!(tape_emphasized(&selected[0], &watches));
    assert!(tape_emphasized(&selected[1], &watches));
    assert!(!tape_emphasized(&selected[2], &watches));
    let no_keywords = tape_items(&items, &[], now);
    assert_eq!(no_keywords.len(), 3);
    assert_eq!(no_keywords[1].lead.id, "normal");
}

#[test]
fn tape_top_up_is_bounded_and_newest_first_without_priority_or_keywords() {
    let now = 100_000;
    let items: Vec<_> =
        (0..25).map(|i| n7_story(&format!("ordinary-{i}"), &format!("Observatory{i}"), now - i)).collect();
    let selected = tape_items(&items, &[], now);
    assert_eq!(selected.len(), 20);
    for (i, story) in selected.iter().enumerate() {
        assert_eq!(story.lead.id, format!("ordinary-{i}"));
        assert!(!tape_emphasized(story, &[]));
    }
}
#[test]
fn n7b_tape_clock_caps_repaint_pauses_and_rotates_statically() {
    for pref in [false, true] {
        for os in [false, true] {
            for focused in [false, true] {
                assert_eq!(tape_static_mode(pref, os, focused), pref || os || !focused);
            }
        }
    }
    let mut clock = TapeClock::default();
    clock.step(0.0, false, false, 3);
    clock.step(1.0, false, false, 3);
    assert_eq!(clock.offset, 40.0);
    assert_eq!(clock.repaint_after(), Some(TAPE_FRAME));
    assert!(TAPE_FRAME >= std::time::Duration::from_millis(33));
    clock.step(2.0, true, false, 3);
    clock.step(20.0, true, false, 3);
    assert_eq!(clock.offset, 40.0);
    assert!(clock.repaint_after().is_none());
    clock.step(21.0, false, false, 3);
    clock.step(22.0, false, false, 3);
    assert_eq!(clock.offset, 80.0, "unhover must not catch up paused time");
    clock.step(30.0, false, true, 3);
    clock.step(37.999, false, true, 3);
    assert_eq!(clock.index, 0);
    clock.step(38.0, false, true, 3);
    assert_eq!(clock.index, 1);
    assert_eq!(clock.offset, 80.0);
    assert_eq!(clock.repaint_after(), Some(std::time::Duration::from_secs(8)));
    clock.step(54.0, false, true, 3);
    assert_eq!(clock.index, 0);
}

#[test]
fn n7b_old_json_defaults_and_preset_flags_roundtrip() {
    for raw in [r#"{}"#, r#"{"symbol":null,"filters":{},"display":{}}"#, r#""ACME US Equity""#] {
        let settings: crate::layouts::NewsSettings = serde_json::from_str(raw).unwrap();
        assert!(!settings.flash_high);
    }
    let prefs: crate::prefs::Prefs = serde_json::from_str("{}").unwrap();
    assert!(!prefs.headline_tape);
    assert!(!prefs.reduce_motion);
    let settings: crate::layouts::NewsSettings = serde_json::from_str(r#"{"flash_high":true}"#).unwrap();
    let kind = super::super::PaneKind::from_settings(PaneSettings::News(Some(settings)));
    let PaneSettings::News(Some(saved)) = kind.settings() else { panic!() };
    assert!(saved.flash_high);
    let mut prefs = prefs;
    prefs.headline_tape = true;
    prefs.reduce_motion = true;
    assert_eq!(serde_json::from_str::<crate::prefs::Prefs>(&serde_json::to_string(&prefs).unwrap()).unwrap(), prefs);
    assert!(
        super::super::palette_view::static_items()
            .iter()
            .any(|i| i.title == "Headline tape" && i.target == crate::palette::Target::HeadlineTape)
    );
}

#[test]
fn n7b_tape_routes_to_most_recent_news_activates_tab_and_retains_filtered_selection() {
    use super::super::{PaneKind, TerminalApp};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/test-data/n7b-routing");
    let mut app = TerminalApp::test_workspace(&root);
    let story = Cluster::wire(&n7_story("pick", "Acme raises guidance", 100_000));
    assert_eq!(app.route_tape_story(&story).as_deref(), Some("https://example.test/pick"));
    let mut unsafe_story = story.clone();
    unsafe_story.lead.item.link = "javascript:alert(1)".into();
    assert!(app.route_tape_story(&unsafe_story).is_none());
    let first = app.new_pane(PaneKind::News(Box::new(NewsPane::new(None))));
    let mut recent = NewsPane::new(None);
    recent.last_focused = Some(std::time::Instant::now());
    recent.filters.search = "not-matching".into();
    let second = app.new_pane(PaneKind::News(Box::new(recent)));
    let other = app.new_pane(PaneKind::Help);
    app.dock = crate::dock::Node::Tabs { panes: vec![first, second, other], active: 2 };
    app.focus = other;
    assert!(app.route_tape_story(&story).is_none());
    assert_eq!(app.focus, second);
    assert_eq!(app.dock.visible(), vec![second]);
    let PaneKind::News(n) = &mut app.panes[1].kind else { panic!() };
    assert_eq!(n.interaction.selected.as_deref(), Some("pick"));
    assert_eq!(n.filters.search, "not-matching");
    let mut cache = RowCache::default();
    cache.update(n, &[], &[], 100_000);
    assert!(cache.rows.is_empty());
    assert_eq!(n.interaction.selected.as_deref(), Some("pick"));
    assert!(n.items.iter().any(|s| s.id == "pick"));
    app.focus = first;
    app.route_tape_story(&story);
    assert_eq!(app.focus, first);
}

#[test]
fn n7b_flash_ui_narrow_hidden_click_selects_and_cross_dismisses() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    pane.flash_high = true;
    pane.items = vec![n7_story("flash", "Acme raises guidance", chrono::Utc::now().timestamp())];
    let (narrow, _) = ui_frame(&ctx, &mut pane, 340.0, vec![], true);
    assert!(!text_shapes(&narrow).iter().any(|(s, _)| s == "FLASH"));
    let (wide, _) = ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    let badge = text_shapes(&wide).iter().find(|(s, _)| s == "FLASH").unwrap().1 + egui::vec2(3.0, 3.0);
    let output = click_at(&ctx, &mut pane, badge);
    assert_eq!(pane.interaction.selected.as_deref(), Some("flash"));
    assert!(output.platform_output.commands.iter().all(|c| !matches!(c, egui::OutputCommand::OpenUrl(_))));
    let cross = text_shapes(&output).iter().find(|(s, _)| s == "×").unwrap().1 + egui::vec2(3.0, 3.0);
    click_at(&ctx, &mut pane, cross);
    let (after, _) = ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    assert!(!text_shapes(&after).iter().any(|(s, _)| s == "FLASH"));
}

#[test]
fn n7b_tape_ui_placeholder_cache_expiry_and_no_requests() {
    use super::super::{PaneKind, TerminalApp};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/test-data/n7b-tape");
    let (worker, mut requests) = crate::worker::Worker::recording();
    let mut app = TerminalApp::isolated_workspace(&root, worker);
    app.prefs.headline_tape = true;
    app.prefs.reduce_motion = true;
    let ctx = egui::Context::default();
    let frame = |app: &mut TerminalApp, time| {
        let mut height = -1.0;
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                focused: false,
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 400.0))),
                ..Default::default()
            },
            |ui| {
                height = app.show_headline_tape(ui, ui.max_rect());
            },
        );
        output.append(ctx.run_ui(egui::RawInput { time: Some(time), focused: false, ..Default::default() }, |ui| {
            height = app.show_headline_tape(ui, ui.max_rect());
        }));
        output.textures_delta.clear();
        (height, output)
    };
    let (height, output) = frame(&mut app, 0.0);
    assert_eq!(height, dp(24.0));
    assert!(text_shapes(&output).iter().any(|(s, _)| s == "No headlines in the last 2 hours"));
    let mut news = NewsPane::new(None);
    news.filters.scope = Scope::All;
    let now = chrono::Utc::now().timestamp();
    news.items = vec![n7_story("tape", "Acme raises guidance", now - 10)];
    let id = app.new_pane(PaneKind::News(Box::new(news)));
    let (height, output) = frame(&mut app, 1.0);
    assert_eq!(height, dp(24.0));
    assert!(text_shapes(&output).iter().any(|(s, _)| s.contains("Acme raises guidance")));
    let delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
    assert!(delay >= TAPE_FRAME && delay <= std::time::Duration::from_secs(8));
    let galley = app.headline_tape.galleys[0].clone();
    frame(&mut app, 2.0);
    assert!(std::sync::Arc::ptr_eq(&galley, &app.headline_tape.galleys[0]), "settled frames reuse text layout");
    let index = app.idx(id).unwrap();
    let PaneKind::News(n) = &mut app.panes[index].kind else { panic!() };
    n.filters.scope = Scope::Symbol;
    let mut fresh_tape = Tape::default();
    fresh_tape.update(&app.panes, &[], now);
    assert!(fresh_tape.items.is_empty(), "symbol caches are not market caches");
    assert_eq!(frame(&mut app, 2.5).0, dp(24.0), "already cached market items survive scope changes");
    app.panes.clear();
    assert_eq!(frame(&mut app, 2.75).0, dp(24.0), "closing the last News pane retains the app-level tape");
    let cached = app.headline_tape.items[0].clone();
    assert_eq!(app.route_tape_story(&cached).as_deref(), Some("https://example.test/tape"));
    app.headline_tape.update(&app.panes, &[], now + TAPE_SECONDS);
    assert!(app.headline_tape.items.is_empty());
    assert_eq!(frame(&mut app, 3.0).0, dp(24.0), "expired tape retains a visible placeholder");
    // Worker data alone populates the tape without any News pane, high priority,
    // or keyword watches. Replacing it rebuilds galleys AND measured offsets.
    app.headline_tape.receive_market(vec![n7_story("ordinary", "Routine weather bulletin", now - 1)]);
    let (_, output) = frame(&mut app, 4.0);
    assert!(text_shapes(&output).iter().any(|(s, _)| s.contains("Routine weather bulletin")));
    let galley = app.headline_tape.galleys[0].clone();
    let offsets = app.headline_tape.offsets.clone();
    app.headline_tape.receive_market(vec![n7_story(
        "changed",
        "Unscheduled observatory bulletin from northern facilities",
        now,
    )]);
    let (_, output) = frame(&mut app, 5.0);
    assert!(!std::sync::Arc::ptr_eq(&galley, &app.headline_tape.galleys[0]));
    assert_ne!(offsets, app.headline_tape.offsets);
    assert!(text_shapes(&output).iter().any(|(s, _)| s.contains("Unscheduled observatory")));
    assert!(requests.try_recv().is_err(), "drawing strips must never fetch or subscribe");
}

#[test]
fn n6b_keyword_chip_reducer_adds_without_enabling_and_legacy_json_loads() {
    let mut pane = visible_pane();
    let mut watches = vec![];
    keyword_click(&mut pane, watches.len());
    assert!(pane.interaction.keyword_add_open);
    assert!(!pane.display.watch_hits);
    pane.interaction.keyword_input = "  \"rate cut\"  ".into();
    add_keyword(&mut pane, &mut watches);
    assert_eq!(watches.len(), 1);
    assert_eq!(watches[0].words, "\"rate cut\"");
    assert!(!pane.display.watch_hits);
    assert!(!pane.interaction.keyword_add_open);
    assert!(pane.interaction.save_watches);
    keyword_click(&mut pane, watches.len());
    assert!(pane.display.watch_hits);
    keyword_click(&mut pane, watches.len());
    assert!(!pane.display.watch_hits);
    pane.interaction.keyword_input = "   ".into();
    add_keyword(&mut pane, &mut watches);
    assert_eq!(watches.len(), 1, "ignore blank additions");
    let old: crate::layouts::NewsSettings =
        serde_json::from_str(r#"{"symbol":null,"display":{"watch_hits":true}}"#).unwrap();
    assert!(old.display.watch_hits);
    assert!(!old.display.wire);
    assert!(serde_json::to_string(&old).unwrap().contains("\"watch_hits\":true"));
}

#[test]
fn n6b_zero_keyword_popover_enter_saves_and_escape_cancels() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    let mut watches = vec![];
    let frame = |pane: &mut NewsPane, watches: &mut Vec<KeywordWatch>, events| {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 400.0))),
                events,
                ..Default::default()
            },
            |ui| {
                show(ui, pane, &[], watches, true, None, 0);
            },
        );
        output.textures_delta.clear();
        output
    };
    let output = frame(&mut pane, &mut watches, vec![]);
    let position = text_shapes(&output).iter().find(|(s, _)| s == "Keywords 0").unwrap().1 + egui::vec2(3.0, 3.0);
    let click = |pressed| egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(&mut pane, &mut watches, vec![egui::Event::PointerMoved(position), click(true)]);
    frame(&mut pane, &mut watches, vec![click(false)]);
    assert!(pane.interaction.keyword_add_open);
    assert!(!pane.display.watch_hits);
    frame(&mut pane, &mut watches, vec![]);
    frame(&mut pane, &mut watches, vec![egui::Event::Text("inflation".into())]);
    assert_eq!(pane.interaction.keyword_input, "inflation");
    frame(&mut pane, &mut watches, key_event(egui::Key::Enter));
    assert_eq!(watches.len(), 1);
    assert_eq!(watches[0].words, "inflation");
    assert!(!pane.display.watch_hits);
    assert!(pane.interaction.save_watches);
    watches.clear();
    frame(&mut pane, &mut watches, vec![egui::Event::PointerMoved(position), click(true)]);
    frame(&mut pane, &mut watches, vec![click(false)]);
    frame(&mut pane, &mut watches, key_event(egui::Key::Escape));
    assert!(!pane.interaction.keyword_add_open);
    assert!(watches.is_empty());
    assert!(!pane.display.watch_hits);
}

#[test]
fn n6b_empty_state_names_keywords_and_all_active_causes() {
    let mut pane = visible_pane();
    pane.display.watch_hits = true;
    assert_eq!(filtered_empty_text(&pane, 1), "No stories match your 1 keyword");
    assert_eq!(filtered_empty_text(&pane, 2), "No stories match your 2 keywords");
    pane.filters.search = "absent".into();
    pane.filters.sources = vec!["Reuters".into()];
    pane.filters.categories = vec!["tech".into()];
    pane.filters.saved = true;
    pane.filters.include = "earnings".into();
    pane.filters.exclude = "rumor".into();
    pane.display.unread = true;
    for keyword_filter in [true, false] {
        pane.display.watch_hits = keyword_filter;
        let text = filtered_empty_text(&pane, 2);
        for cause in [
            "Search: absent",
            "Sources: Reuters",
            "Categories: TECH",
            "Saved only",
            "Include any: earnings",
            "Exclude any: rumor",
            "Unread only",
        ] {
            assert!(text.contains(cause), "{text}");
        }
        assert!(!text.contains("days"), "do not blame date range for other filters");
    }
    pane.display.watch_hits = true;
    let ctx = egui::Context::default();
    let (output, _) = ui_frame(&ctx, &mut pane, 1800.0, vec![], false);
    assert!(text_shapes(&output).iter().any(|(s, _)| s.starts_with("No stories match your 0 keywords")));
    let clear = text_shapes(&output).into_iter().find(|(s, _)| s == "Clear filters").unwrap().1;
    click_at(&ctx, &mut pane, clear + egui::vec2(3.0, 3.0));
    assert!(!pane.display.watch_hits && !pane.display.unread);
    assert!(pane.filters.sources.is_empty() && pane.filters.categories.is_empty());
}

#[test]
fn n6b_status_counts_enabled_sources_pauses_and_latest_success() {
    let now = 100_000;
    let mut sources = vec![];
    for (name, status, updated) in [
        ("Finnhub", "OK", now - 500),
        ("Yahoo", "Quiet", now - 120),
        ("BBC", "Paused until 14:32", now - 600),
        ("SEC", "Off", now),
    ] {
        sources.push(SourceStatus {
            name: name.into(),
            status: status.into(),
            reason: None,
            newest: Some(now - 7200),
            updated: Some(updated),
        });
    }
    assert_eq!(aggregate_status(&sources, now), "3 sources · 1 paused · updated 2m ago");
    assert_eq!(sources[2].line(now), "BBC · Paused until 14:32 · newest 2h ago");
    sources[0].status = "Paused".into();
    assert_eq!(aggregate_status(&sources, now), "3 sources · 2 paused · updated 2m ago");
    assert_eq!(aggregate_status(&[], now), "0 sources · not updated yet");
    sources[0].status = "OK".into();
    sources[2].status = "Pending".into();
    assert_eq!(aggregate_status(&sources, now), "3 sources · 1 pending · updated 2m ago");
    sources[2].status = "Quiet".into();
    assert_eq!(aggregate_status(&sources, now), "3 sources · updated 2m ago");
}

#[test]
fn n6b_sources_are_publishers_only_without_changing_saved_provider_filters() {
    let mut pane = visible_pane();
    pane.items[0].source = "Reuters".into();
    pane.items[0].provider = "finnhub".into();
    pane.items[0].sources = vec!["Reuters".into(), "rss".into()];
    pane.items[0].category = "macro".into();
    pane.items[1].category = "crypto".into();
    pane.items[1].source = "CoinDesk".into();
    pane.items[1].provider = "CoinDesk".into();
    pane.filters.sources =
        vec!["finnhub".into(), "yahoo".into(), "rss".into(), "sec-press".into(), "Saved publisher".into()];
    let before = pane.filters.sources.clone();
    let sources = publisher_sources(&pane);
    for publisher in ["Reuters", "CoinDesk", "Saved publisher"] {
        assert!(sources.contains(publisher));
    }
    for provider in ["finnhub", "yahoo", "rss", "sec-press"] {
        assert!(!sources.contains(provider));
    }
    pane.filters.sources = vec!["finnhub".into()];
    assert!(tr_data::news_store::matches(&pane.items[0], &current_query(&pane, chrono::Utc::now().timestamp())));
    pane.filters.sources = before.clone();
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        filters(ui, &mut pane, &mut vec![], false, 0);
    });
    output.textures_delta.clear();
    assert_eq!(pane.filters.sources, before);
    let labels = text_shapes(&output);
    let sources_y = labels.iter().find(|(s, _)| s == "Sources").unwrap().1.y;
    for category in ["MACRO", "CRYPTO"] {
        assert!(labels.iter().any(|(s, p)| s == category && p.y > sources_y), "publisher category heading: {category}");
    }
}

#[test]
fn n6b_selected_legacy_sources_are_visible_above_publishers_and_individually_removable() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    pane.items[0].source = "Reuters".into();
    pane.filters.sources = vec!["finnhub".into(), "rss".into(), "Reuters".into()];
    // Active legacy entries remain visible even when the publisher search hides them.
    pane.interaction.source_filter = "Reuters".into();
    let frame = |pane: &mut NewsPane, events| {
        let mut output = ctx.run_ui(
            egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 1000.0))),
                ..Default::default()
            },
            |ui| filters(ui, pane, &mut vec![], false, 0),
        );
        output.textures_delta.clear();
        output
    };
    let output = frame(&mut pane, vec![]);
    let labels = text_shapes(&output);
    let finnhub = labels.iter().find(|(s, _)| s == "finnhub (legacy)").unwrap().1;
    let rss = labels.iter().find(|(s, _)| s == "rss (legacy)").unwrap().1;
    let reuters = labels.iter().find(|(s, p)| s == "Reuters" && p.y > rss.y).unwrap().1;
    assert!(finnhub.y < reuters.y && rss.y < reuters.y);
    let position = finnhub + egui::vec2(3.0, 3.0);
    let click = |pressed| egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(&mut pane, vec![egui::Event::PointerMoved(position), click(true)]);
    frame(&mut pane, vec![click(false)]);
    assert_eq!(pane.filters.sources, ["rss", "Reuters"]);
}

#[test]
fn n6b_w_requires_selection_and_respects_pane_and_search_focus() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    assert!(pane.interaction.selected.is_none());
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::W), true);
    assert!(!pane.display.wire, "unselected W belongs to type-anywhere");
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::ArrowDown), true);
    assert!(pane.interaction.selected.is_some());
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::W), false);
    assert!(!pane.display.wire);
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::W), true);
    assert!(pane.display.wire);
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::W), true);
    assert!(!pane.display.wire);
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::Slash), true);
    ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    let mut typing = key_event(egui::Key::W);
    typing.push(egui::Event::Text("w".into()));
    ui_frame(&ctx, &mut pane, 600.0, typing, true);
    assert_eq!(pane.filters.search, "w");
    assert!(!pane.display.wire);
    assert!(NEWS_SHORTCUTS.contains(&("W", "Stories / Wire (selected row)")));
}

#[test]
fn n6b_filters_count_only_hidden_header_chips() {
    for width in [320.0, 359.0, 360.0, 600.0] {
        let ctx = egui::Context::default();
        let mut pane = visible_pane();
        pane.display.unread = true;
        pane.display.watch_hits = true;
        let (output, _) = ui_frame(&ctx, &mut pane, width, vec![], false);
        let labels = text_shapes(&output);
        let count = if width < 360.0 { 2 } else { 0 };
        assert!(labels.iter().any(|(s, _)| s == &format!("Filters ({count})")));
        assert_eq!(labels.iter().any(|(s, _)| s == "Unread 0"), width >= 360.0);
        assert_eq!(labels.iter().any(|(s, _)| s == "Keywords 0"), width >= 360.0);
    }
}

#[test]
fn n6b_category_meta_is_market_only_wide_and_mixed_category() {
    for (scope, width, single, visible) in [
        (Scope::All, 1800.0, false, true),
        (Scope::All, 1800.0, true, false),
        (Scope::All, 320.0, false, false),
        (Scope::Watchlist, 1800.0, false, false),
    ] {
        let ctx = egui::Context::default();
        let mut pane = visible_pane();
        pane.filters.scope = scope;
        for item in &mut pane.items {
            item.category = "tech".into();
            item.source = "Reuters".into();
        }
        if single {
            pane.filters.categories = vec!["tech".into()];
        }
        let (output, _) = ui_frame(&ctx, &mut pane, width, vec![], false);
        assert_eq!(text_shapes(&output).iter().any(|(s, _)| s.starts_with("TECH · Reuters")), visible);
    }
    for (category, code) in [
        ("general", "MKT"),
        ("markets", "MKT"),
        ("macro", "MACRO"),
        ("regulatory", "REG"),
        ("geopolitics", "GEO"),
        ("tech", "TECH"),
        ("crypto", "CRYPTO"),
    ] {
        assert_eq!(category_code(category), code);
    }
}

#[test]
fn n6b_selected_w_capture_respects_app_text_focus() {
    use super::super::{PaneKind, TerminalApp};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/test-data/news-wire-key");
    let (worker, _requests) = crate::worker::Worker::recording();
    let mut app = TerminalApp::isolated_workspace(&root, worker);
    let mut pane = visible_pane();
    pane.interaction.selected = Some(pane.items[0].id.clone());
    let id = app.new_pane(PaneKind::News(Box::new(pane)));
    app.focus = id;
    let ctx = egui::Context::default();
    let typing = || {
        let mut events = key_event(egui::Key::W);
        events.push(egui::Event::Text("w".into()));
        events
    };
    let mut output = ctx.run_ui(egui::RawInput { events: typing(), ..Default::default() }, |ui| app.hotkeys(ui.ctx()));
    output.textures_delta.clear();
    assert!(app.cmd.is_empty());
    let i = app.idx(id).unwrap();
    let PaneKind::News(n) = &mut app.panes[i].kind else { panic!() };
    assert_eq!(n.interaction.pending_keys, [egui::Key::W]);
    n.interaction.pending_keys.clear();
    let mut text = String::new();
    let input_id = egui::Id::new("n6b-wire-typing");
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        ui.add(egui::TextEdit::singleline(&mut text).id(input_id)).request_focus();
    });
    output.textures_delta.clear();
    let mut output = ctx.run_ui(egui::RawInput { events: typing(), ..Default::default() }, |ui| {
        app.hotkeys(ui.ctx());
        ui.add(egui::TextEdit::singleline(&mut text).id(input_id));
    });
    output.textures_delta.clear();
    assert_eq!(text, "w");
    assert!(app.cmd.is_empty());
    let PaneKind::News(n) = &app.panes[i].kind else { panic!() };
    assert!(n.interaction.pending_keys.is_empty());
}

#[test]
fn n5_feed_delivery_is_idempotent_even_with_reordered_replies() {
    let mut last = 0;
    assert!(super::super::accept_news_delivery(&mut last, 1));
    assert!(!super::super::accept_news_delivery(&mut last, 1));
    assert!(super::super::accept_news_delivery(&mut last, 3));
    assert!(!super::super::accept_news_delivery(&mut last, 2));
}
#[test]
fn n5_inferred_timestamp_is_honestly_labelled() {
    let mut item = stories()[0].clone();
    item.inferred_ts = true;
    let model = row_model(&Cluster::wire(&item), None, item.item.ts + 60);
    assert_eq!(model.full_time, "time unknown");
    assert_eq!(model.time, "—");
}

#[test]
fn n5_save_key_requires_selection_and_saved_filter_tracks_actions_and_stale_replies() {
    assert!(!owns_key(egui::Key::S, false, false));
    assert!(owns_key(egui::Key::S, true, false));
    let mut pane = NewsPane::new(None);
    pane.items = stories();
    let original = pane.items.clone();
    let cluster = Cluster::wire(&pane.items[0]);
    let mut selection = Some(cluster.lead.id.clone());
    assert_eq!(
        reduce(Key::Save, std::slice::from_ref(&cluster.lead.id), &mut selection, &mut String::new(), &mut false),
        Effect::Save
    );
    toggle_saved(&mut pane, &cluster);
    assert!(pane.items[0].bookmark);
    assert_eq!(pane.bookmark_queue, vec![(cluster.member_ids.clone(), true)]);
    pane.filters.saved = true;
    pane.apply(original.clone()); // a read issued before Save must not undo it
    let q = current_query(&pane, pane.items[0].item.ts);
    assert!(tr_data::news_store::matches(&pane.items[0], &q));
    assert!(!tr_data::news_store::matches(&pane.items[1], &q));
    let cluster = Cluster::wire(&pane.items[0]);
    toggle_saved(&mut pane, &cluster);
    assert!(!pane.items[0].bookmark);
    assert!(!pane.bookmark_queue.last().unwrap().1);
    pane.bookmark_reply(&cluster.member_ids, true, None); // older Save ack cannot undo queued Unsave
    assert!(!pane.items[0].bookmark);
    pane.bookmark_reply(&cluster.member_ids, false, None);
    assert!(pane.local_bookmarks.is_empty());
    pane.bookmark_reply(&cluster.member_ids, true, None); // another pane saved it
    assert!(pane.items[0].bookmark);
    let cluster = Cluster::wire(&pane.items[0]);
    toggle_saved(&mut pane, &cluster);
    pane.bookmark_reply(&cluster.member_ids, false, Some("Read-only store"));
    assert!(pane.items[0].bookmark, "failed Unsave restores saved state");
    assert_eq!(pane.error.as_deref(), Some("Read-only store"));
}
use crate::layouts::{LayoutDoc, PaneSettings};
fn stories() -> Vec<tr_data::news_store::Story> {
    tr_data::finnhub::parse(include_bytes!("../../../crates/data/tests/fixtures/finnhub_news.json"), None).unwrap()
}
#[test]
fn n5_history_and_source_refresh_issue_new_per_pane_generations() {
    use super::super::{PaneKind, TerminalApp};
    use crate::worker::{Request, Worker};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/test-data/news-generations");
    let (worker, mut requests) = Worker::recording();
    let mut app = TerminalApp::isolated_workspace(&root, worker);
    let id = app.new_pane(PaneKind::News(Box::new(NewsPane::new(None))));
    let i = app.idx(id).unwrap();
    app.news_periodic(&[i], false);
    let first = app.news_subscription.as_ref().unwrap().0[0].clone();
    while requests.try_recv().is_ok() {}
    let PaneKind::News(n) = &mut app.panes[i].kind else { panic!() };
    n.fetched = None;
    app.news_periodic(&[i], false);
    let second = app.news_subscription.as_ref().unwrap().0[0].clone();
    assert!(second.generation > first.generation);
    assert_eq!(second.filters, first.filters);
    assert!(matches!(requests.try_recv(), Ok(Request::NewsRead(view)) if view == second));
    while requests.try_recv().is_ok() {}
    let PaneKind::News(n) = &mut app.panes[i].kind else { panic!() };
    n.refresh_source = true;
    n.fetched = None;
    app.news_periodic(&[i], false);
    let third = app.news_subscription.as_ref().unwrap().0[0].clone();
    assert!(third.generation > second.generation);
    assert!(matches!(requests.try_recv(), Ok(Request::NewsRefresh(view)) if view == third));
    let other = app.new_pane(PaneKind::News(Box::new(NewsPane::new(None))));
    let j = app.idx(other).unwrap();
    app.news_periodic(&[i, j], false);
    let views = &app.news_subscription.as_ref().unwrap().0;
    assert_eq!(views[0].generation, third.generation);
    assert_eq!(views[1].generation, 1);
    let PaneKind::News(n) = &mut app.panes[i].kind else { panic!() };
    **n = NewsPane::new(None); // reopening News in the same pane must not reset its counter
    app.news_periodic(&[i, j], false);
    assert!(app.news_subscription.as_ref().unwrap().0[0].generation > third.generation);
}
#[test]
fn n4_volume_banner_is_current_hour_and_scope_specific() {
    use tr_data::news_store::local::{Volume, local_hour};
    let now = 20 * 86400 + 120;
    let hour = local_hour(now);
    let volume = |key: &str, at| Volume { key: key.into(), hour: at, count: 14, usual: 3., z: 11. };
    let mut pane = NewsPane::new(Some("NVDA US Equity".into()));
    pane.filters.scope = Scope::Symbol;
    pane.interaction.volumes =
        vec![volume("Topic: Earnings", hour), volume("AAPL US Equity", hour), volume("NVDA US Equity", hour - 3600)];
    assert!(current_volume(&pane, now).is_none());
    pane.interaction.volumes.push(volume("NVDA US Equity", hour));
    assert_eq!(current_volume(&pane, now).unwrap().message(), "Unusual volume: NVDA — 14 stories this hour (usual 3)");
    pane.interaction.dismissed_volume = Some(hour);
    assert!(current_volume(&pane, now).is_none());
    pane.interaction.dismissed_volume = None;
    pane.filters.scope = Scope::All;
    assert_eq!(current_volume(&pane, now).unwrap().key, "Topic: Earnings");
    pane.filters.scope = Scope::Watchlist;
    assert_eq!(current_volume(&pane, now).unwrap().key, "Topic: Earnings");
    assert!(current_volume(&pane, now + 3600).is_none());
}
#[test]
fn never_blank_and_pause_counts_unique_new_items() {
    let mut pane = NewsPane::new(None);
    let mut items = stories();
    pane.apply(vec![items[0].clone()]);
    pane.apply(vec![]);
    assert_eq!(pane.items.len(), 1);
    pane.error = Some("Offline".into());
    assert_eq!(pane.items.len(), 1);
    pane.paused = true;
    pane.apply(items.clone());
    pane.apply(items.clone());
    assert_eq!(pane.items.len(), 1);
    assert_eq!(pane.new_count(), 1);
    pane.apply(vec![]);
    assert_eq!(pane.new_count(), 1);
    pane.paused = false;
    pane.apply(std::mem::take(&mut items));
    assert_eq!(pane.items.len(), 2);
    assert_eq!(pane.new_count(), 0);
}
#[test]
fn scope_snapshots_default_active_watchlist_and_hidden_stop() {
    use super::super::{PaneKind, TerminalApp};
    use crate::worker::{Request, Worker};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/test-data/news-scopes");
    let (worker, mut requests) = Worker::recording();
    let mut app = TerminalApp::isolated_workspace(&root, worker);
    app.watchlists = vec![
        NamedWatchlist::new("Main".into(), vec!["AAPL US Equity".into()]),
        NamedWatchlist::new("Other".into(), vec!["MSFT US Equity".into()]),
    ];
    app.watchlist = app.watchlists[0].symbols.clone();
    let id = app.new_pane(PaneKind::News(Box::new(NewsPane::new(None))));
    let i = app.idx(id).unwrap();
    app.news_periodic(&[i], false);
    let (views, symbols) = app.news_subscription.as_ref().unwrap();
    assert!(views[0].symbols.is_empty(), "unlinked symbol means general market");
    assert_eq!(symbols.len(), 2, "scheduler sees all watchlists, not just active");
    while requests.try_recv().is_ok() {}
    app.news_periodic(&[i], false);
    assert!(requests.try_recv().is_err(), "unchanged frames send no requests");
    let PaneKind::News(n) = &mut app.panes[i].kind else { panic!() };
    n.filters.scope = Scope::Watchlist;
    app.news_periodic(&[i], false);
    assert_eq!(app.news_subscription.as_ref().unwrap().0[0].symbols, vec!["AAPL US Equity"]);
    let PaneKind::News(n) = &mut app.panes[i].kind else { panic!() };
    n.filters.watchlist = Some("Other".into());
    app.news_periodic(&[i], false);
    assert_eq!(app.news_subscription.as_ref().unwrap().0[0].symbols, vec!["MSFT US Equity"]);
    // Scope changes now explicitly reload history and enter the loading state.
    while requests.try_recv().is_ok() {}
    app.news_periodic(&[i], true);
    assert!(app.news_subscription.as_ref().unwrap().0.is_empty());
    assert!(matches!(requests.try_recv(), Ok(Request::NewsVisible { .. })));
}

#[test]
fn ticker_chips_target_charts_and_link_changes_keep_saved_rows_and_filters() {
    use super::super::{PaneKind, TerminalApp};
    use crate::{chart_view::ChartState, layouts::Link, worker::Worker};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/test-data/news-tickers");
    let (worker, _requests) = Worker::recording();
    let mut app = TerminalApp::isolated_workspace(&root, worker);
    let chart = app.new_pane(PaneKind::Chart(Box::new(ChartState::new(0, "AAPL US Equity", tr_core::Timeframe::D1))));
    let news = app.new_pane(PaneKind::News(Box::new(NewsPane::new(None))));
    let ci = app.idx(chart).unwrap();
    let ni = app.idx(news).unwrap();
    app.panes[ni].link = Some(Link::Blue); // A link without a chart must still open a chart.
    app.focus = news;
    app.run_actions(vec![Action::OpenChart("MSFT US Equity".into())]);
    let PaneKind::Chart(c) = &app.panes[ci].kind else { panic!() };
    assert_eq!(c.symbol, "MSFT US Equity");
    app.panes[ci].link = Some(Link::Blue);
    let PaneKind::News(n) = &mut app.panes[ni].kind else { panic!() };
    n.items = stories();
    n.filters.include = "earnings".into();
    app.focus = news;
    app.run_actions(vec![Action::OpenChart("NVDA US Equity".into())]);
    let PaneKind::Chart(c) = &app.panes[ci].kind else { panic!() };
    assert_eq!(c.symbol, "NVDA US Equity");
    let PaneKind::News(n) = &app.panes[ni].kind else { panic!() };
    assert_eq!(n.symbol.as_deref(), Some("NVDA US Equity"));
    assert_eq!(n.items.len(), 2);
    assert_eq!(n.filters.include, "earnings");
}

#[test]
fn review_symbol_switch_empty_loading_and_pause_count_are_view_local() {
    use super::super::{PaneKind, TerminalApp};
    use crate::worker::Worker;
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/test-data/news-switch-empty");
    let (worker, _requests) = Worker::recording();
    let mut app = TerminalApp::isolated_workspace(&root, worker);
    let id = app.new_pane(PaneKind::News(Box::new(NewsPane::new(Some("AAPL US Equity".into())))));
    let i = app.idx(id).unwrap();
    app.news_periodic(&[i], false);
    let PaneKind::News(n) = &mut app.panes[i].kind else { panic!() };
    let mut saved = stories()[0].clone();
    saved.item.ts = chrono::Utc::now().timestamp();
    saved.item.tickers = vec!["AAPL US Equity".into()];
    n.items = vec![saved.clone()];
    n.paused = true;
    saved.id = "new".into();
    n.pending = vec![saved];
    assert_eq!(n.new_count(), 1);
    n.symbol = Some("TSLA US Equity".into());
    n.loading = false;
    app.news_periodic(&[i], false);
    let PaneKind::News(n) = &mut app.panes[i].kind else { panic!() };
    assert!(n.loading);
    assert!(n.pending.is_empty());
    assert_eq!(n.new_count(), 0);
    let q = tr_data::news_store::Query { symbols: vec!["TSLA US Equity".into()], ..Default::default() };
    assert_eq!(n.empty_message(&q).as_deref(), Some("No saved headlines for TSLA US Equity yet"));
    n.apply(vec![]);
    assert_eq!(n.empty_message(&q).as_deref(), Some("No saved headlines for TSLA US Equity yet"));
    assert_eq!(n.items.len(), 1, "retain old saved items without displaying the wrong symbol");
}

#[test]
fn review_visible_unfocused_window_keeps_polling() {
    let mut viewport = egui::ViewportInfo { focused: Some(false), minimized: Some(false), ..Default::default() };
    assert!(window_visible(&viewport), "focus loss must not suspend visible panes");
    viewport.minimized = Some(true);
    assert!(!window_visible(&viewport));
}

#[test]
fn old_layout_fixture_loads_and_new_filters_round_trip() {
    let mut doc = LayoutDoc::from_json(include_bytes!("../../tests/fixtures/news_layout_old.json")).unwrap();
    let PaneSettings::News(Some(saved)) = &mut doc.panes[0].settings else { panic!() };
    assert_eq!(saved.symbol.as_deref(), Some("AAPL US Equity"));
    assert_eq!(saved.filters.scope, Scope::Symbol);
    assert_eq!(saved.filters.days, 7);
    saved.filters.scope = Scope::Watchlist;
    saved.filters.watchlist = Some("Main".into());
    saved.filters.include = "earnings".into();
    saved.filters.exclude = "ban".into();
    saved.filters.search = "growth".into();
    saved.filters.sources = vec!["Finnhub".into()];
    saved.filters.days = 90;
    let restored = LayoutDoc::from_json(&doc.to_json()).unwrap();
    assert_eq!(restored, doc);
    assert!(matches!(doc.panes[1].settings, PaneSettings::News(None)));
}

#[test]
fn review_linked_symbol_outside_watchlists_is_scheduled() {
    use super::super::{PaneKind, TerminalApp};
    use crate::worker::Worker;
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/test-data/news-linked");
    let (worker, _requests) = Worker::recording();
    let mut app = TerminalApp::isolated_workspace(&root, worker);
    app.watchlists = vec![NamedWatchlist::new("Main".into(), vec!["AAPL US Equity".into()])];
    app.watchlist = app.watchlists[0].symbols.clone();
    let id = app.new_pane(PaneKind::News(Box::new(NewsPane::new(Some("TSLA US Equity".into())))));
    let i = app.idx(id).unwrap();
    app.news_periodic(&[i], false);
    let (_, symbols) = app.news_subscription.as_ref().unwrap();
    assert!(symbols.iter().any(|s| s == "TSLA US Equity"), "linked symbol missing from scheduler list: {symbols:?}");
}

#[test]
fn n1a_and_pre_n1a_layout_fixtures_default_to_stories() {
    for fixture in [
        include_bytes!("../../tests/fixtures/news_layout_old.json").as_slice(),
        include_bytes!("../../tests/fixtures/news_layout_n1a.json").as_slice(),
    ] {
        let doc = LayoutDoc::from_json(fixture).unwrap();
        let PaneSettings::News(Some(settings)) = &doc.panes[0].settings else { panic!() };
        assert_eq!(settings.display, Display::default());
        assert_eq!(settings.display.sort, intelligence::Sort::Newest);
        assert_eq!(settings.symbol.as_deref(), Some("AAPL US Equity"));
        assert_eq!(LayoutDoc::from_json(&doc.to_json()).unwrap(), doc);
    }
    let mut doc = LayoutDoc::from_json(include_bytes!("../../tests/fixtures/news_layout_n1a.json")).unwrap();
    let PaneSettings::News(Some(settings)) = &mut doc.panes[0].settings else { panic!() };
    settings.display = Display { wire: true, unread: true, watch_hits: true, sort: intelligence::Sort::Relevance };
    assert_eq!(LayoutDoc::from_json(&doc.to_json()).unwrap(), doc);
}
#[test]
fn row_columns_omit_linked_symbol_and_include_union_meta() {
    let mut stories = stories();
    stories[0].source = "Reuters".into();
    stories[0].item.tickers = vec!["AAPL US Equity".into(), "MSFT US Equity".into(), "NVDA US Equity".into()];
    stories[0].subjects = stories[0].item.tickers.clone();
    let mut cluster = Cluster::wire(&stories[0]);
    cluster.sources = vec!["Reuters".into(), "Yahoo".into(), "CNBC".into()];
    let model = row_model(&cluster, Some("AAPL US Equity"), cluster.newest + 12 * 60);
    assert_eq!(model.id, stories[0].id);
    assert_eq!(model.time, "12m");
    assert!(model.full_time.contains("UTC"));
    assert_eq!(model.ticker, None, "a later subject must not replace the linked first subject");
    assert_eq!(chart_ticker(&cluster, Some("AAPL US Equity"), &[]).map(String::as_str), Some("MSFT US Equity"));
    assert!(model.extra_tickers.is_empty());
    assert!(model.meta.starts_with("Reuters +2"));
    let preceding = row_model(&cluster, Some("MSFT US Equity"), cluster.newest);
    assert_eq!(preceding.ticker.as_deref(), Some("AAPL US Equity"));
    assert!(preceding.extra_tickers.is_empty());
    assert!(model.meta.contains("Earnings"));
    assert_eq!(&model.meta_steps[1..], &["Reuters +2", "Reuters +2", "Reuters", ""]);
    let unlinked = row_model(&cluster, None, cluster.newest);
    assert!(unlinked.meta_steps[0].contains("Earnings"));
    assert_eq!(&unlinked.meta_steps[1..], &["Reuters +2 · +2", "Reuters +2", "Reuters", ""]);
    assert!(model.unread);
    cluster.members[0].seen = true;
    assert!(!row_model(&cluster, None, cluster.newest).unread);
    let mut single = Cluster::wire(&tr_data::news_store::Story::new(
        tr_data::NewsItem {
            title: "Only AAPL".into(),
            publisher: "Reuters".into(),
            link: "https://fixture.test/single".into(),
            ts: 1,
            tickers: vec!["AAPL US Equity".into()],
        },
        "Reuters",
    ));
    single.subjects = vec!["AAPL US Equity".into()];
    assert_eq!(row_model(&single, Some("AAPL US Equity"), 2).ticker, None);
}
#[test]
fn subject_display_is_amber_association_fallback_is_muted_and_chart_target_agrees() {
    let mut s = tr_data::news_store::Story::new(
        tr_data::NewsItem {
            title: "Can JFrog Stock Keep Its Lead Over Its Industry?".into(),
            publisher: "Publisher".into(),
            link: "https://fixture.test/jfrog".into(),
            ts: 100,
            tickers: vec!["MSFT US Equity".into()],
        },
        "Publisher",
    );
    s.provider = "Finnhub".into();
    s.subjects = vec!["FROG US Equity".into()];
    let cluster = Cluster::wire(&s);
    for linked in [None, Some("MSFT US Equity")] {
        let model = row_model(&cluster, linked, 101);
        assert_eq!(model.ticker.as_deref(), Some("FROG US Equity"));
        assert!(model.ticker_is_subject);
        assert!(model.extra_tickers.is_empty());
        assert_eq!(model.about, "About: FROG · Also returned for: MSFT");
        assert_eq!(chart_ticker(&cluster, linked, &[]), model.ticker.as_ref());
    }
    s.subjects.clear();
    let cluster = Cluster::wire(&s);
    let model = row_model(&cluster, Some("MSFT US Equity"), 101);
    assert_eq!(model.ticker.as_deref(), Some("MSFT US Equity"));
    assert!(!model.ticker_is_subject);
    assert_eq!(model.ticker_hover, "Returned by Finnhub for MSFT; not mentioned in the story");
    assert_eq!(chart_ticker(&cluster, Some("MSFT US Equity"), &[]), model.ticker.as_ref());
    s.subjects = vec!["BTCUSD Curncy".into(), "FROG US Equity".into(), "NVDA US Equity".into()];
    s.item.tickers.push("NVDA US Equity".into());
    let cluster = Cluster::wire(&s);
    let model = row_model(&cluster, Some("NVDA US Equity"), 101);
    assert_eq!(model.ticker.as_deref(), Some("BTCUSD Curncy"));
    assert_eq!(model.extra_tickers, vec!["FROG US Equity"]);
    assert!(model.meta.contains(" · +1"));
    assert!(model.about.contains("About: BTCUSD · FROG · NVDA"));
}
#[test]
fn review_muted_fallback_prefers_linked_association() {
    let mut s = tr_data::news_store::Story::new(
        tr_data::NewsItem {
            title: "AI agents raise concerns for banks".into(),
            publisher: "Publisher".into(),
            link: "https://fixture.test/fallback".into(),
            ts: 100,
            // Store order is sorted, so an older AAPL association precedes MSFT.
            tickers: vec!["AAPL US Equity".into(), "MSFT US Equity".into()],
        },
        "Publisher",
    );
    s.provider = "Finnhub".into();
    let cluster = Cluster::wire(&s);
    let model = row_model(&cluster, Some("MSFT US Equity"), 101);
    assert_eq!(model.ticker.as_deref(), Some("MSFT US Equity"));
    assert_eq!(model.ticker_hover, "Returned by Finnhub for MSFT; not mentioned in the story");
}
#[test]
fn muted_fallback_prefers_watched_association_for_display_and_chart() {
    let s = tr_data::news_store::Story::new(
        tr_data::NewsItem {
            title: "Market commentary".into(),
            publisher: "Publisher".into(),
            link: "https://fixture.test/watched-fallback".into(),
            ts: 100,
            tickers: vec!["AAPL US Equity".into(), "MSFT US Equity".into(), "NVDA US Equity".into()],
        },
        "Publisher",
    );
    let cluster = Cluster::wire(&s);
    let watched = vec!["MSFT US Equity".into()];
    for (linked, expected) in [
        (None, "MSFT US Equity"),
        (Some("NVDA US Equity"), "NVDA US Equity"),
        (Some("UNKNOWN US Equity"), "MSFT US Equity"),
    ] {
        let model = row_model_watched(&cluster, linked, &watched, 101);
        assert_eq!(model.ticker.as_deref(), Some(expected));
        assert!(!model.ticker_is_subject);
        assert_eq!(chart_ticker(&cluster, linked, &watched), model.ticker.as_ref());
    }
}
#[test]
fn keyboard_reducer_boundaries_actions_escape_and_reordering() {
    let ids = vec!["a".into(), "b".into(), "c".into()];
    let mut selected = None;
    let mut search = String::new();
    let mut unread = false;
    assert_eq!(reduce(Key::Open, &ids, &mut selected, &mut search, &mut unread), Effect::None);
    reduce(Key::Previous, &ids, &mut selected, &mut search, &mut unread);
    assert_eq!(selected.as_deref(), Some("a"));
    reduce(Key::Next, &ids, &mut selected, &mut search, &mut unread);
    assert_eq!(selected.as_deref(), Some("b"));
    for (key, effect) in [
        (Key::Open, Effect::Open),
        (Key::Expand, Effect::Expand),
        (Key::Chart, Effect::Chart),
        (Key::Search, Effect::Search),
    ] {
        assert_eq!(reduce(key, &ids, &mut selected, &mut search, &mut unread), effect);
    }
    let reordered = vec!["new".into(), "b".into(), "a".into(), "c".into()];
    reduce(Key::Next, &reordered, &mut selected, &mut search, &mut unread);
    assert_eq!(selected.as_deref(), Some("a"));
    reduce(Key::Unread, &ids, &mut selected, &mut search, &mut unread);
    assert!(unread);
    search = "earnings".into();
    reduce(Key::Escape, &ids, &mut selected, &mut search, &mut unread);
    assert!(search.is_empty());
    assert!(selected.is_some());
    reduce(Key::Escape, &ids, &mut selected, &mut search, &mut unread);
    assert!(selected.is_none());
    reduce(Key::Next, &[], &mut selected, &mut search, &mut unread);
    assert!(selected.is_none());
    for key in [egui::Key::J, egui::Key::K, egui::Key::C, egui::Key::U, egui::Key::Space, egui::Key::Slash] {
        assert!(owns_key(key, true, false));
    }
    assert!(!owns_key(egui::Key::A, true, false));
    assert!(!owns_key(egui::Key::J, false, false));
    assert!(owns_key(egui::Key::ArrowDown, false, false));
}
#[test]
fn empty_causes_prioritize_loading_key_and_filters() {
    assert_eq!(empty_cause(true, true, true), EmptyCause::Loading);
    assert_eq!(empty_cause(false, true, true), EmptyCause::NoKey);
    assert_eq!(empty_cause(false, false, true), EmptyCause::Filtered);
    assert_eq!(empty_cause(false, false, false), EmptyCause::NoHistory);
}
#[test]
fn expansion_marks_every_member_seen_and_queues_one_write_per_item() {
    let mut pane = NewsPane::new(None);
    pane.items = stories();
    let mut cluster = Cluster::wire(&pane.items[0]);
    cluster.member_ids.push(pane.items[1].id.clone());
    toggle_expanded(&mut pane, &cluster);
    assert!(pane.items.iter().all(|s| s.seen));
    assert_eq!(pane.seen_queue.len(), 2);
    toggle_expanded(&mut pane, &cluster);
    assert!(pane.interaction.expanded.is_empty());
    assert_eq!(pane.seen_queue.len(), 2);
}
#[test]
fn watches_are_global_prefs_with_legacy_defaults_and_roundtrip() {
    let mut prefs: crate::prefs::Prefs = serde_json::from_str("{}").unwrap();
    assert!(prefs.keyword_watches.is_empty());
    prefs.keyword_watches.push(KeywordWatch {
        name: "Chips".into(),
        words: "NVDA \"AI chip\"".into(),
        colour: 5,
        notify: false,
    });
    assert_eq!(serde_json::from_str::<crate::prefs::Prefs>(&serde_json::to_string(&prefs).unwrap()).unwrap(), prefs);
}

fn ui_frame(
    ctx: &egui::Context,
    pane: &mut NewsPane,
    width: f32,
    events: Vec<egui::Event>,
    focused: bool,
) -> (egui::FullOutput, f32) {
    let mut measured = 0.0;
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 400.0))),
            events,
            ..Default::default()
        },
        |ui| {
            show(ui, pane, &[], &mut vec![], focused, None, 0);
            measured = ui.min_rect().width();
        },
    );
    output.textures_delta.clear();
    (output, measured)
}
fn key_event(key: egui::Key) -> Vec<egui::Event> {
    vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]
}
fn text_shapes(output: &egui::FullOutput) -> Vec<(String, egui::Pos2)> {
    output
        .shapes
        .iter()
        .filter_map(|shape| {
            if let egui::Shape::Text(text) = &shape.shape {
                Some((text.galley.job.text.clone(), text.pos))
            } else {
                None
            }
        })
        .collect()
}
#[test]
fn news_key_legend_covers_owned_keys_and_save_in_footer_and_help() {
    for key in egui::Key::ALL {
        if owns_key(*key, true, true) {
            let label = match key {
                egui::Key::ArrowDown => "↓",
                egui::Key::ArrowUp => "↑",
                egui::Key::Slash => "/",
                egui::Key::Escape => "Esc",
                _ => key.name(),
            };
            assert!(
                NEWS_SHORTCUTS.iter().any(|(keys, _)| keys.to_lowercase().contains(&label.to_lowercase())),
                "missing legend entry: {key:?}"
            );
        }
    }
    assert!(NEWS_SHORTCUTS.contains(&("S", "Save story")));
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    let (output, _) = ui_frame(&ctx, &mut pane, 1800.0, vec![], false);
    let labels = text_shapes(&output);
    for (key, action) in NEWS_SHORTCUTS {
        assert!(labels.iter().any(|(s, p)| s == key && p.y > 300.0), "footer key: {key}");
        assert!(labels.iter().any(|(s, _)| s == action), "footer action: {action}");
    }
    let short = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1800.0, 120.0))),
            ..Default::default()
        },
        |ui| {
            show(ui, &mut pane, &[], &mut vec![], false, None, 0);
        },
    );
    assert!(!text_shapes(&short).iter().any(|(s, _)| s == "Next story"), "short panes must hide the key legend");
    // Help uses the same table; keep its section and row actions discoverable too.
    let help = include_str!("hotkeys_view.rs");
    assert!(help.contains("section(ui, \"News\")"));
    assert!(help.contains("super::news_view::NEWS_SHORTCUTS"));
    assert!(help.contains("Copy link, Save/Unsave (S)"));
}

#[test]
fn unread_dot_and_watch_stripe_are_painted_independently() {
    for watched in [false, true] {
        for unread in [false, true] {
            let ctx = egui::Context::default();
            let mut pane = visible_pane();
            pane.items.truncate(1);
            pane.items[0].seen = !unread;
            let word = pane.items[0].item.title.clone();
            let mut watches = if watched { vec![KeywordWatch { words: word, ..Default::default() }] } else { vec![] };
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                show(ui, &mut pane, &[], &mut watches, false, None, 0);
            });
            let title_y = text_shapes(&output).iter().find(|(s, _)| s == &pane.items[0].item.title).unwrap().1.y;
            let in_row = |y: f32| (y - title_y).abs() < dp(15.0);
            let dots = output
                .shapes
                .iter()
                .filter(|s| {
                    matches!(&s.shape,
                        egui::Shape::Circle(c) if c.radius == dp(2.0) && c.center.x < dp(15.0) && in_row(c.center.y)
                    )
                })
                .count();
            let stripes = output.shapes.iter().filter(|s| matches!(&s.shape,
                egui::Shape::Rect(r) if r.rect.width() == dp(2.0) && r.rect.left() < dp(15.0) && in_row(r.rect.center().y)
            )).count();
            assert_eq!(dots, usize::from(unread), "watched={watched}, unread={unread}");
            assert_eq!(stripes, usize::from(watched), "watched={watched}, unread={unread}");
            output.textures_delta.clear();
        }
    }
}

#[test]
fn source_filter_is_case_insensitive_counts_matches_and_preserves_selection() {
    assert!(source_matches("Reuters", "  EUT  "));
    assert!(!source_matches("Reuters", "Bloomberg"));
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    pane.filters.sources = vec!["Reuters".into(), "Bloomberg".into()];
    pane.interaction.source_filter = "eUt".into();
    let source_count = publisher_sources(&pane).len();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        ui.set_max_width(dp(300.0));
        filters(ui, &mut pane, &mut vec![], false, 0);
    });
    let labels = text_shapes(&output);
    let count = format!("1 of {source_count} sources match");
    assert!(labels.iter().any(|(s, _)| s == &count));
    assert!(labels.iter().any(|(s, _)| s == "Reuters"));
    assert!(!labels.iter().any(|(s, _)| s == "Bloomberg"));
    assert!(labels.iter().any(|(s, _)| s == "Include words (any)"));
    assert!(labels.iter().any(|(s, _)| s.contains("Any whole word; case-insensitive, not a phrase")));
    assert!(labels.iter().any(|(s, _)| s.contains("Reject any whole word; case-insensitive, not a phrase")));
    assert_eq!(pane.filters.sources, ["Reuters", "Bloomberg"]);
    output.textures_delta.clear();
}

#[test]
fn search_is_bounded_filters_adjacent_and_collapsed_summary_is_visible() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    let (output, _) = ui_frame(&ctx, &mut pane, 1400.0, vec![], false);
    let labels = text_shapes(&output);
    let search = labels.iter().find(|(s, _)| s == "Search").unwrap().1;
    let filters = labels.iter().find(|(s, _)| s == "Filters (0)").unwrap().1;
    assert!(filters.x > search.x && filters.x - search.x <= dp(235.0), "{search:?} → {filters:?}");
    pane.filters.sources = vec!["Reuters".into()];
    pane.filters.include = "earnings guidance".into();
    pane.filters.exclude = "rumor".into();
    pane.display.unread = true;
    pane.display.watch_hits = true;
    let expected = active_filter_summary(&pane);
    for value in ["Reuters", "Include any: earnings guidance", "Exclude any: rumor", "Unread only", "Keywords"] {
        assert!(expected.contains(value));
    }
    for width in [320.0, 1400.0] {
        let (output, measured) = ui_frame(&ctx, &mut pane, width, vec![], false);
        assert!(measured <= width + 1.0);
        assert!(text_shapes(&output).iter().any(|(s, _)| s == &expected));
    }
}

#[test]
fn n6_categories_toggle_filter_persist_per_pane_and_settle_at_360pt() {
    let old: crate::layouts::NewsSettings =
        serde_json::from_str(r#"{"symbol":null,"filters":{"scope":"All"}}"#).unwrap();
    assert!(old.filters.categories.is_empty());
    let mut doc = LayoutDoc::from_json(include_bytes!("../../tests/fixtures/news_layout_n1a.json")).unwrap();
    let PaneSettings::News(Some(settings)) = &mut doc.panes[0].settings else { panic!() };
    settings.filters.categories = vec!["tech".into()];
    doc.panes[1].settings = PaneSettings::News(Some(old));
    let loaded = LayoutDoc::from_json(&doc.to_json()).unwrap();
    assert_eq!(loaded, doc);
    let mut categories = vec![];
    toggle_category(&mut categories, "tech", false);
    assert_eq!(categories, ["tech"]);
    toggle_category(&mut categories, "macro", false);
    assert_eq!(categories, ["macro"]);
    toggle_category(&mut categories, "tech", true);
    assert_eq!(categories, ["macro", "tech"]);
    toggle_category(&mut categories, "macro", true);
    assert_eq!(categories, ["tech"]);
    toggle_category(&mut categories, "tech", true);
    assert!(categories.is_empty());
    toggle_category(&mut categories, "tech", false);
    toggle_category(&mut categories, "tech", false);
    assert!(categories.is_empty(), "last selected chip returns to All");
    toggle_category(&mut categories, "tech", false);
    assert_eq!(categories, ["tech"]);
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    pane.items[0].category = "tech".into();
    pane.items[1].category = "markets".into();
    pane.filters.categories = categories;
    for _ in 0..4 {
        ui_frame(&ctx, &mut pane, 360.0, vec![], false);
    }
    let rebuilds = pane.cache.rebuilds;
    let (output, measured) = ui_frame(&ctx, &mut pane, 360.0, vec![], false);
    assert!(measured <= 361.0);
    assert_eq!(pane.cache.rebuilds, rebuilds);
    assert_eq!(pane.cache.ids.len(), 1);
    assert!(output.viewport_output[&egui::ViewportId::ROOT].repaint_delay > std::time::Duration::from_secs(60));
    assert!(active_filter_summary(&pane).contains("TECH"));
}
#[test]
fn n5c_old_display_json_defaults_newest_and_pane_sorts_persist_independently() {
    let old: crate::layouts::NewsSettings =
        serde_json::from_str(r#"{"symbol":null,"display":{"wire":true,"unread":true,"watch_hits":false}}"#).unwrap();
    assert_eq!(old.display.sort, intelligence::Sort::Newest);
    assert!(old.display.wire && old.display.unread);
    let mut doc = LayoutDoc::from_json(include_bytes!("../../tests/fixtures/news_layout_n1a.json")).unwrap();
    let PaneSettings::News(Some(settings)) = &mut doc.panes[0].settings else { panic!() };
    settings.display.sort = intelligence::Sort::Relevance;
    doc.panes[1].settings = PaneSettings::News(Some(old));
    let loaded = LayoutDoc::from_json(&doc.to_json()).unwrap();
    assert_eq!(loaded, doc);
    for (index, sort) in [(0, intelligence::Sort::Relevance), (1, intelligence::Sort::Newest)] {
        let PaneSettings::News(Some(settings)) = &loaded.panes[index].settings else { panic!() };
        assert_eq!(settings.display.sort, sort);
    }
}

#[test]
fn n5c_relevance_settles_without_idle_repaint_at_narrow_widths() {
    for width in [320.0, 359.0, 360.0, 600.0, 1800.0] {
        let ctx = egui::Context::default();
        let mut pane = visible_pane();
        pane.display.sort = intelligence::Sort::Relevance;
        for _ in 0..4 {
            ui_frame(&ctx, &mut pane, width, vec![], false);
        }
        let rebuilds = pane.cache.rebuilds;
        let (output, measured) = ui_frame(&ctx, &mut pane, width, vec![], false);
        assert!(measured <= width + 1.0, "relevance chrome overflow at {width}");
        assert_eq!(pane.cache.rebuilds, rebuilds);
        assert!(output.viewport_output[&egui::ViewportId::ROOT].repaint_delay > std::time::Duration::from_secs(60));
    }
}

#[test]
fn n5c_sort_toggle_preserves_selection_anchor_and_read_in_view() {
    let now = 200_000;
    let mut pane = visible_pane();
    pane.filters.days = 90;
    pane.items[0].item.title = "Routine alpha bulletin".into();
    pane.items[0].item.ts = now - 1;
    pane.items[1].item.title = "BREAKING: Beta statement".into();
    pane.items[1].item.ts = now - 60;
    let mut cache = RowCache::default();
    cache.update(&mut pane, &[], &[], now);
    let id = cache.ids[0].clone();
    let initial = cache.ids.clone();
    pane.interaction.selected = Some(id.clone());
    pane.interaction.anchor = Some(id.clone());
    pane.interaction.anchor_offset = -dp(5.0);
    pane.read_in_view.insert(id.clone());
    pane.display.sort = intelligence::Sort::Relevance;
    cache.update(&mut pane, &[], &[], now);
    assert_eq!(cache.ids, initial.into_iter().rev().collect::<Vec<_>>());
    assert_eq!(pane.interaction.selected.as_ref(), Some(&id));
    assert_eq!(pane.interaction.anchor.as_ref(), Some(&id));
    assert_eq!(pane.interaction.anchor_offset, -dp(5.0));
    assert!(pane.interaction.restore_anchor);
    assert!(pane.read_in_view.contains(&id));
    // Age crossing invalidates once on an event-driven frame, not every idle frame.
    let rebuilds = cache.rebuilds;
    cache.update(&mut pane, &[], &[], now);
    assert_eq!(cache.rebuilds, rebuilds);
    cache.update(&mut pane, &[], &[], now + 86401);
    assert_eq!(cache.ids[0], id);
    assert_eq!(cache.rebuilds, rebuilds + 1);
    // Saved and search filters still precede ranking.
    pane.filters.saved = true;
    pane.items[0].bookmark = true;
    pane.filters.search = "routine".into();
    cache.update(&mut pane, &[], &[], now);
    assert_eq!(cache.ids, vec![id]);
}

#[test]
fn n5c_r_toggles_only_with_selection_and_does_not_take_search_typing() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    assert!(!owns_key(egui::Key::R, false, false));
    assert!(owns_key(egui::Key::R, true, false));
    assert_eq!(key_text(egui::Key::R), Some("r"));
    ui_frame(&ctx, &mut pane, 360.0, key_event(egui::Key::R), true);
    assert_eq!(pane.display.sort, intelligence::Sort::Newest);
    pane.interaction.selected = Some(pane.items[0].id.clone());
    ui_frame(&ctx, &mut pane, 360.0, key_event(egui::Key::R), true);
    assert_eq!(pane.display.sort, intelligence::Sort::Relevance);
    ui_frame(&ctx, &mut pane, 360.0, key_event(egui::Key::R), true);
    assert_eq!(pane.display.sort, intelligence::Sort::Newest);
    ui_frame(&ctx, &mut pane, 360.0, key_event(egui::Key::Slash), true);
    ui_frame(&ctx, &mut pane, 360.0, vec![], true);
    ui_frame(&ctx, &mut pane, 360.0, key_event(egui::Key::R), true);
    assert_eq!(pane.display.sort, intelligence::Sort::Newest);
}

#[test]
fn review_unselected_r_reaches_type_anywhere() {
    use super::super::{PaneKind, TerminalApp};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/test-data/news-review-r-ownership");
    let (worker, _requests) = crate::worker::Worker::recording();
    let mut app = TerminalApp::isolated_workspace(&root, worker);
    let id = app.new_pane(PaneKind::News(Box::new(visible_pane())));
    app.focus = id;
    let ctx = egui::Context::default();
    let mut events = key_event(egui::Key::R);
    events.push(egui::Event::Text("r".into()));
    let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| app.hotkeys(ui.ctx()));
    output.textures_delta.clear();
    assert_eq!(app.cmd, "R", "unselected letters belong to type-anywhere");
    for (key, text) in [(egui::Key::I, "i"), (egui::Key::V, "v"), (egui::Key::N, "n")] {
        let mut events = key_event(key);
        events.push(egui::Event::Text(text.into()));
        let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| app.hotkeys(ui.ctx()));
        output.textures_delta.clear();
    }
    assert_eq!(app.cmd, "RIVN");
}

#[test]
fn n5c_row_signals_are_aligned_and_source_count_leads_on_light_and_dark() {
    let old_palette = theme::palette();
    for palette in [crate::themes::terminal_dark(), crate::themes::terminal_light()] {
        theme::set_palette(palette.palette(theme::DEFAULT_ACCENT));
        let ctx = egui::Context::default();
        let mut pane = visible_pane();
        pane.items[0].item.title = "Routine alpha bulletin".into();
        pane.items[1].item.title = "Beta beats estimates".into();
        let mut breaking = pane.items[0].clone();
        breaking.id = "breaking".into();
        breaking.item.title = "FLASH: Gamma launch".into();
        breaking.source = "Reuters".into();
        pane.items.push(breaking.clone());
        for (id, source) in [("two", "CNBC"), ("three", "Bloomberg"), ("duplicate", "CNBC")] {
            let mut member = breaking.clone();
            member.id = id.into();
            member.source = source.into();
            pane.items.push(member);
        }
        let (output, measured) = ui_frame(&ctx, &mut pane, 360.0, vec![], false);
        assert!(measured <= 361.0);
        let labels = text_shapes(&output);
        let x = |text: &str| labels.iter().find(|(s, _)| s == text).unwrap().1.x;
        // The square + gap uses precisely the removed 14dp marker slot.
        assert_eq!(x("Routine alpha bulletin") + dp(14.0), x("Beta beats estimates"));
        assert_eq!(x("Beta beats estimates"), x("FLASH: Gamma launch"));
        assert!(x("×3") < x("FLASH: Gamma launch"));
        assert!(!labels.iter().any(|(s, _)| s == "×1" || s == "×4"));
        assert!(!labels.iter().any(|(s, _)| s == "!" || s == "+"));
        for priority in [intelligence::Priority::Urgent, intelligence::Priority::High] {
            let (fill, _) = pill_colours(priority);
            assert!(output.shapes.iter().any(|s| matches!(&s.shape,
                egui::Shape::Rect(r) if r.fill == fill && r.rect.size() == egui::vec2(dp(10.0), dp(10.0)))));
        }
        for headline in ["Beta beats estimates", "FLASH: Gamma launch"] {
            assert!(output.shapes.iter().any(|s| matches!(&s.shape,
                egui::Shape::Text(t) if t.galley.job.text == headline
                    && t.galley.job.sections.iter().all(|section| section.format.coords
                        == egui::epaint::text::VariationCoords::new([(b"wght", 680.0)])))));
        }
        let (wide, _) = ui_frame(&ctx, &mut pane, 900.0, vec![], false);
        let wide_labels = text_shapes(&wide);
        for (label, priority) in
            [("URGENT", intelligence::Priority::Urgent), ("EARNINGS", intelligence::Priority::High)]
        {
            let (_, ink) = pill_colours(priority);
            assert!(wide_labels.iter().any(|(text, _)| text == label));
            assert!(wide.shapes.iter().any(|s| matches!(&s.shape,
                egui::Shape::Text(t) if t.galley.job.text == label && t.fallback_color == ink)));
        }
        let lum = |c: egui::Color32| 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
        let (fill, ink) = pill_colours(intelligence::Priority::Urgent);
        assert!((lum(fill) - lum(ink)).abs() > 100.0, "attention pill must contrast in both themes");
        assert_eq!(pill_colours(intelligence::Priority::High), (theme::accent_wash(), theme::accent()));
        pane.display.wire = true;
        let (wire, _) = ui_frame(&ctx, &mut pane, 360.0, vec![], false);
        assert!(!text_shapes(&wire).iter().any(|(s, _)| s.starts_with('×')));
    }
    theme::set_palette(old_palette);
}

#[test]
fn polish_pill_text_covers_every_event_and_collapses_without_headline_width_loss() {
    use intelligence::Event::*;
    for (event, label) in [
        (Urgent, "URGENT"),
        (MergerAcquisition, "M&A"),
        (Earnings, "EARNINGS"),
        (Guidance, "GUIDANCE"),
        (Rating, "RATING"),
        (Fed, "FED"),
        (Halt, "HALT"),
        (Bankruptcy, "BANKRUPTCY"),
        (Fda, "FDA"),
        (Legal, "LEGAL"),
        (Recall, "RECALL"),
        (Dividend, "DIVIDEND"),
        (Leadership, "CEO"),
        (Jobs, "JOBS"),
    ] {
        assert_eq!(pill_text(event, dp(900.0)), label);
        for width in [320.0, 359.0, 360.0, 600.0] {
            assert_eq!(pill_text(event, dp(width)), "");
        }
    }
}

#[test]
fn n5c_source_column_is_reserved_only_for_visible_multi_source_stories() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    pane.items[0].item.title = "Routine alpha bulletin".into();
    pane.items[1].item.title = "Beta beats estimates".into();
    let headline_x = |output: &egui::FullOutput| {
        text_shapes(output).into_iter().find(|(text, _)| text == "Routine alpha bulletin").unwrap().1.x
    };
    let (single, _) = ui_frame(&ctx, &mut pane, 360.0, vec![], false);
    pane.display.wire = true;
    let (wire, _) = ui_frame(&ctx, &mut pane, 360.0, vec![], false);
    assert_eq!(headline_x(&single), headline_x(&wire), "no empty source column for single-source Stories");
    pane.display.wire = false;
    let mut duplicate = pane.items[1].clone();
    duplicate.id = "another-source".into();
    duplicate.source = "Another source".into();
    pane.items.push(duplicate);
    let (multi, _) = ui_frame(&ctx, &mut pane, 360.0, vec![], false);
    assert_eq!(headline_x(&multi), headline_x(&single) + dp(32.0));
    pane.filters.search = "alpha".into();
    let (filtered, _) = ui_frame(&ctx, &mut pane, 360.0, vec![], false);
    assert_eq!(headline_x(&filtered), headline_x(&single), "filtered-out clusters must not reserve the column");
}

fn visible_pane() -> NewsPane {
    let mut pane = NewsPane::new(None);
    pane.loading = false;
    pane.items = stories();
    pane.filters.scope = Scope::All;
    for story in &mut pane.items {
        story.item.ts = chrono::Utc::now().timestamp();
    }
    pane
}
#[test]
fn header_scope_labels_and_vector_overflow_survive_340pt() {
    for scope in [Scope::All, Scope::Symbol, Scope::Watchlist] {
        let ctx = egui::Context::default();
        let mut pane = visible_pane();
        pane.symbol = Some("AAPL US Equity".into());
        pane.filters.scope = scope.clone();
        pane.status = "Finnhub · updated now · A very long diagnostic with 216 pending rows".into();
        let lists =
            [NamedWatchlist { name: "Core list".into(), symbols: vec!["AAPL US Equity".into()], ..Default::default() }];
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(340., 400.))),
                ..Default::default()
            },
            |ui| {
                show(ui, &mut pane, &lists, &mut vec![], true, None, 0);
            },
        );
        let labels = text_shapes(&output);
        let header: Vec<_> = labels.iter().filter(|(_, p)| p.y < dp(25.)).map(|(s, _)| s.as_str()).collect();
        assert_eq!(header.contains(&"AAPL"), scope == Scope::Symbol, "{header:?}");
        assert_eq!(header.contains(&"Core list"), scope == Scope::Watchlist, "{header:?}");
        assert!(!labels.iter().any(|(s, _)| s.starts_with("Unread ")), "narrow pane keeps Unread in Filters");
        assert!(labels.iter().any(|(s, p)| s == "Stories" && p.y >= dp(20.)));
        let dots = output
            .shapes
            .iter()
            .filter(|shape| {
                matches!(&shape.shape,
                    egui::Shape::Circle(c) if c.center.x > 310. && c.center.x < 340. && c.center.y < dp(25.)
                )
            })
            .count();
        assert_eq!(dots, 3, "overflow must remain painted at the right edge");
        output.textures_delta.clear();
    }
}

#[test]
fn feed_chrome_stays_two_lines_and_rows_are_dense_at_narrow_widths() {
    for width in [320.0, 359.0, 360.0, 600.0] {
        let ctx = egui::Context::default();
        let mut pane = visible_pane();
        let (output, measured) = ui_frame(&ctx, &mut pane, width, vec![], false);
        assert!(measured <= width + 1.0, "chrome overflow at {width}: {measured}");
        let labels = text_shapes(&output);
        assert!(
            !labels.iter().any(|(s, _)| s.contains("non-commercial")),
            "licence belongs only in About and Settings"
        );
        assert!(!labels.iter().any(|(s, _)| s == "Next story"), "compact panes must hide the key legend");
        let today = labels.iter().find(|(s, _)| s == "7D").unwrap().1;
        let stories = labels.iter().find(|(s, _)| s == "Stories").unwrap().1;
        assert!((today.y - stories.y).abs() < dp(3.0), "date dropdown and view mode share a row");
        assert!(!labels.iter().any(|(s, _)| s == "90D"), "date options belong in the dropdown");
        assert_eq!(labels.iter().any(|(s, _)| s == "Keywords 0"), width >= 360.0);
        assert_eq!(labels.iter().any(|(s, _)| s.starts_with("Unread ")), width >= 360.0);
        let positions: Vec<_> = pane
            .items
            .iter()
            .map(|story| labels.iter().find(|(text, _)| text == &story.item.title).unwrap().1.y)
            .collect();
        assert!(
            positions.iter().copied().fold(f32::INFINITY, f32::min) < dp(60.0),
            "headlines must follow two chrome lines: {positions:?}"
        );
        assert!(
            ((positions[0] - positions[1]).abs() - dp(22.0)).abs() < 1.0,
            "one headline per dp(22) row: {positions:?}"
        );
    }
}
#[test]
fn open_search_stays_bounded_at_chrome_breakpoints() {
    for width in [320.0, 359.0, 360.0, 600.0] {
        for search in ["", "earnings"] {
            let ctx = egui::Context::default();
            let mut pane = visible_pane();
            pane.interaction.search_open = true;
            pane.filters.search = search.into();
            let (_, measured) = ui_frame(&ctx, &mut pane, width, vec![], false);
            assert!(measured <= width + 1.0, "open search overflow at {width}: {measured}");
        }
    }
}

#[test]
fn legend_and_filter_summary_settle_without_idle_repaints_at_all_widths() {
    for width in [320.0, 359.0, 360.0, 600.0, 1800.0] {
        for summary in [false, true] {
            let ctx = egui::Context::default();
            let mut pane = visible_pane();
            if summary {
                pane.filters.exclude = "rumor".into();
            }
            for _ in 0..4 {
                ui_frame(&ctx, &mut pane, width, vec![], false);
            }
            let (output, measured) = ui_frame(&ctx, &mut pane, width, vec![], false);
            assert!(measured <= width + 1.0, "overflow at {width}, summary={summary}: {measured}");
            assert!(
                output.viewport_output[&egui::ViewportId::ROOT].repaint_delay > std::time::Duration::from_secs(60),
                "idle repaint at {width}, summary={summary}"
            );
            let labels = text_shapes(&output);
            assert_eq!(labels.iter().any(|(s, _)| s == "Next story"), width == 1800.0);
            assert_eq!(labels.iter().any(|(s, _)| s == "Exclude any: rumor"), summary);
        }
    }
}

#[test]
fn keyboard_dwell_marks_seen_once_and_text_fields_own_typing() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::ArrowDown), true);
    let id = pane.interaction.selected.clone().unwrap();
    assert!(pane.items.iter().any(|s| s.id == id && !s.seen));
    pane.interaction.dwell = Some((id.clone(), std::time::Instant::now() - std::time::Duration::from_millis(1100)));
    ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    assert!(pane.items.iter().any(|s| s.id == id && s.seen));
    assert_eq!(pane.seen_queue, vec![id]);
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::Slash), true);
    ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    assert!(ctx.memory(|m| m.focused()).is_some());
    let old = pane.interaction.selected.clone();
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::J), true);
    assert_eq!(pane.interaction.selected, old, "j belongs to a focused search field");
}
#[test]
fn settled_feed_has_no_idle_repaint_and_lost_focus_cancels_dwell() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    for _ in 0..4 {
        ui_frame(&ctx, &mut pane, 600.0, vec![], false);
    }
    let (output, _) = ui_frame(&ctx, &mut pane, 600.0, vec![], false);
    assert!(output.viewport_output[&egui::ViewportId::ROOT].repaint_delay > std::time::Duration::from_secs(60));
    let id = pane.items[0].id.clone();
    pane.interaction.selected = Some(id.clone());
    pane.interaction.dwell = Some((id, std::time::Instant::now() - std::time::Duration::from_secs(2)));
    ui_frame(&ctx, &mut pane, 600.0, vec![], false);
    assert!(pane.seen_queue.is_empty());
    assert!(pane.interaction.dwell.is_none());
}
#[test]
fn anchor_follows_member_when_a_better_source_becomes_lead() {
    let mut items = stories();
    let old = items[0].id.clone();
    let mut better = items[0].clone();
    better.id = "better-source".into();
    better.source = "Reuters".into();
    better.item.ts += 1;
    items.push(better);
    let clustered = intelligence::clusters(&items);
    assert_eq!(remap_anchor(Some(&old), &clustered).as_deref(), Some("better-source"));
    assert_eq!(remap_anchor(Some("gone"), &clustered), None);
}
fn click_at(ctx: &egui::Context, pane: &mut NewsPane, position: egui::Pos2) -> egui::FullOutput {
    ui_frame(
        ctx,
        pane,
        600.0,
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
        true,
    );
    ui_frame(
        ctx,
        pane,
        600.0,
        vec![egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
        true,
    )
    .0
}
#[test]
fn row_body_expands_but_headline_opens_a_safe_link() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    let title = pane.items[0].item.title.clone();
    let id = pane.items[0].id.clone();
    let (initial, _) = ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    let title_pos = text_shapes(&initial).into_iter().find(|(text, _)| text == &title).unwrap().1;
    let body = click_at(&ctx, &mut pane, egui::pos2(3.0, title_pos.y + 5.0));
    assert!(pane.interaction.expanded.contains(&id));
    assert!(pane.items.iter().find(|s| s.id == id).unwrap().seen);
    let position = text_shapes(&body).into_iter().find(|(text, _)| text == &title).unwrap().1 + egui::vec2(8.0, 5.0);
    let opened = click_at(&ctx, &mut pane, position);
    assert!(
        opened
            .platform_output
            .commands
            .iter()
            .any(|c| matches!(c,egui::OutputCommand::OpenUrl(url) if url.url.starts_with("https://example.test/")))
    );
    assert!(pane.interaction.expanded.contains(&id), "title clicks must not also toggle the row body");
    pane.items.iter_mut().find(|s| s.id == id).unwrap().item.link = "javascript:alert(1)".into();
    pane.item_revision += 1;
    let unsafe_open = click_at(&ctx, &mut pane, position);
    assert!(!unsafe_open.platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::OpenUrl(_))));
}
#[test]
fn paused_pane_exposes_new_count_show_action_without_a_footer() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    pane.paused = true;
    let mut incoming = pane.items[0].clone();
    incoming.id = "new-pending".into();
    pane.pending = vec![incoming];
    let (output, _) = ui_frame(&ctx, &mut pane, 600.0, vec![], false);
    let labels = text_shapes(&output);
    let show = labels.iter().find(|(text, _)| text == "1 new · Show").unwrap().1;
    click_at(&ctx, &mut pane, show + egui::vec2(5.0, 5.0));
    assert!(!pane.paused);
    assert_eq!(pane.items[0].id, "new-pending");
    assert!(pane.pending.is_empty());
}
#[test]
fn narrow_search_stays_visible_and_feed_keys_do_not_type_into_command_line() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    ui_frame(&ctx, &mut pane, 320.0, vec![], true);
    ui_frame(&ctx, &mut pane, 320.0, key_event(egui::Key::Slash), true);
    for _ in 0..3 {
        ui_frame(&ctx, &mut pane, 320.0, vec![], true);
    }
    assert!(pane.interaction.search_open);
    assert!(ctx.memory(|m| m.focused()).is_some());
    ui_frame(&ctx, &mut pane, 320.0, vec![egui::Event::Text("chip".into())], true);
    assert_eq!(pane.filters.search, "chip");

    use super::super::{PaneKind, TerminalApp};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/test-data/news-key-ownership");
    let (worker, _requests) = crate::worker::Worker::recording();
    let mut app = TerminalApp::isolated_workspace(&root, worker);
    let mut news = visible_pane();
    news.interaction.selected = Some(news.items[0].id.clone());
    let id = app.new_pane(PaneKind::News(Box::new(news)));
    app.focus = id;
    let ctx = egui::Context::default();
    let mut events = key_event(egui::Key::J);
    events.push(egui::Event::Text("j".into()));
    let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| app.hotkeys(ui.ctx()));
    output.textures_delta.clear();
    assert!(app.cmd.is_empty(), "News navigation must bypass type-anywhere");
}
fn many_stories(count: usize) -> Vec<tr_data::news_store::Story> {
    let base = stories()[0].clone();
    let now = chrono::Utc::now().timestamp();
    let words = ["chip", "rally", "bank", "oil", "deal", "fed", "court", "drug", "growth", "sales", "probe", "cut"];
    (0..count)
        .map(|i| {
            let mut story = base.clone();
            story.id = format!("review-{i}");
            story.item.title =
                format!("Company{i} {} {} update{i} report{}", words[i % 12], words[(i / 12) % 12], i % 7);
            story.item.link = format!("https://example.test/review/{i}");
            story.item.ts = now - (i as i64) * 3800;
            story
        })
        .collect()
}
#[test]
fn review_settled_frame_does_not_recluster_stored_history() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    pane.items = many_stories(2_000);
    pane.filters.days = 90;
    for _ in 0..3 {
        ui_frame(&ctx, &mut pane, 900.0, vec![], false);
    }
    let rebuilds = pane.cache.rebuilds;
    let start = std::time::Instant::now();
    for _ in 0..5 {
        ui_frame(&ctx, &mut pane, 900.0, vec![], false);
    }
    let frame = start.elapsed() / 5;
    eprintln!("N1b debug settled frame, 2,000 items: {frame:?}");
    assert_eq!(pane.cache.rebuilds, rebuilds, "settled frames must not rebuild rows");
    let start = std::time::Instant::now();
    for _ in 0..5 {
        std::hint::black_box(intelligence::clusters(&pane.items));
    }
    let clustering = start.elapsed() / 5;
    // A settled frame with unchanged items must reuse the clustered rows and draw only
    // the visible rows, so it costs far less than one clustering pass over the history.
    assert!(frame * 2 < clustering, "settled frame {frame:?} vs one clustering pass {clustering:?}");
}
#[test]
fn review_unread_only_keeps_the_row_being_read() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    pane.display.unread = true;
    ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::ArrowDown), true);
    let id = pane.interaction.selected.clone().unwrap();
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::Space), true);
    let (output, _) = ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    assert!(pane.interaction.expanded.contains(&id));
    assert_eq!(pane.interaction.selected.as_ref(), Some(&id), "the row being read keeps the selection");
    let summary = pane.items.iter().find(|s| s.id == id).unwrap().summary.clone();
    assert!(
        text_shapes(&output).iter().any(|(text, _)| text == &summary),
        "Space must show the expanded summary, not hide the row"
    );
}
#[test]
fn review_keyboard_selection_does_not_scroll_visible_rows() {
    let setup = || {
        let mut pane = visible_pane();
        pane.items = many_stories(60);
        pane.filters.days = 90;
        pane.display.wire = true;
        pane
    };
    let first_row_y = |output: &egui::FullOutput, title: &str| {
        text_shapes(output).into_iter().find(|(text, _)| text == title).map(|(_, pos)| pos.y)
    };
    let title = setup().items[0].item.title.clone();
    let ctx = egui::Context::default();
    let mut pane = setup();
    ui_frame(&ctx, &mut pane, 900.0, vec![], true);
    let (initial, _) = ui_frame(&ctx, &mut pane, 900.0, vec![], true);
    let before = first_row_y(&initial, &title);
    assert!(before.is_some());
    for _ in 0..3 {
        ui_frame(&ctx, &mut pane, 900.0, key_event(egui::Key::ArrowDown), true);
    }
    assert_eq!(pane.interaction.selected.as_deref(), Some(pane.items[2].id.as_str()));
    let mut output = None;
    for _ in 0..30 {
        output = Some(ui_frame(&ctx, &mut pane, 900.0, vec![], true).0);
    }
    assert_eq!(
        first_row_y(&output.unwrap(), &title),
        before,
        "selecting the third visible row must not scroll the first row out of view"
    );
}
#[test]
fn review_filters_count_ignores_defined_watches() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    let mut watches = vec![
        KeywordWatch { name: "Chips".into(), words: "chip".into(), colour: 0, notify: false },
        KeywordWatch { name: "Fed".into(), words: "fed".into(), colour: 1, notify: false },
    ];
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 400.0))),
            ..Default::default()
        },
        |ui| {
            show(ui, &mut pane, &[], &mut watches, false, None, 0);
        },
    );
    output.textures_delta.clear();
    let labels = text_shapes(&output);
    assert!(
        labels.iter().any(|(text, _)| text == "Filters (0)"),
        "{:?}",
        labels.iter().map(|l| &l.0).collect::<Vec<_>>()
    );
}

#[test]
fn cache_invalidates_for_filters_watches_display_and_seen_only_when_changed() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    let mut watches = vec![];
    let frame = |pane: &mut NewsPane, watches: &mut Vec<KeywordWatch>| {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 400.0))),
                ..Default::default()
            },
            |ui| {
                show(ui, pane, &[], watches, false, None, 0);
            },
        );
        output.textures_delta.clear();
    };
    frame(&mut pane, &mut watches);
    let first = pane.cache.rebuilds;
    frame(&mut pane, &mut watches);
    assert_eq!(pane.cache.rebuilds, first);
    watches.push(KeywordWatch { name: "Earnings".into(), words: "earnings".into(), colour: 2, notify: false });
    frame(&mut pane, &mut watches);
    assert_eq!(pane.cache.rebuilds, first + 1);
    pane.display.watch_hits = true;
    frame(&mut pane, &mut watches);
    assert_eq!(pane.cache.unread, 1);
    pane.filters.search = "notpresent".into();
    frame(&mut pane, &mut watches);
    assert!(pane.cache.rows.is_empty());
}

#[test]
fn unread_counts_displayed_clusters_and_watch_hits() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    let mut duplicate = pane.items[0].clone();
    duplicate.id = "duplicate".into();
    pane.items.push(duplicate);
    ui_frame(&ctx, &mut pane, 600.0, vec![], false);
    assert_eq!(pane.cache.unread, 2, "three unread items form two story rows");
    pane.display.wire = true;
    ui_frame(&ctx, &mut pane, 600.0, vec![], false);
    assert_eq!(pane.cache.unread, 3);
    pane.display.watch_hits = true;
    let mut watches = vec![KeywordWatch { name: "No hits".into(), words: "absent".into(), colour: 0, notify: false }];
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        show(ui, &mut pane, &[], &mut watches, false, None, 0);
    });
    output.textures_delta.clear();
    assert_eq!(pane.cache.unread, 0);
}

#[test]
fn unread_read_row_survives_seen_reply_but_not_filter_change() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    pane.display.unread = true;
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::ArrowDown), true);
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::Space), true);
    let id = pane.interaction.selected.clone().unwrap();
    // A stale worker reply cannot undo the local read, nor hide the expansion.
    let mut stale = pane.items.clone();
    stale.iter_mut().for_each(|s| s.seen = false);
    pane.apply(stale);
    ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    assert!(pane.items.iter().find(|s| s.id == id).unwrap().seen);
    assert_eq!(pane.interaction.selected.as_ref(), Some(&id));
    assert!(pane.interaction.expanded.contains(&id));
    pane.filters.days = 30;
    ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    assert!(!pane.cache.ids.contains(&id));
    assert!(pane.interaction.selected.is_none());
    assert!(pane.interaction.expanded.is_empty());
}

#[test]
fn expanded_cluster_survives_lead_change_and_refresh_never_rearms_dwell() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::ArrowDown), true);
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::Space), true);
    let old = pane.interaction.selected.clone().unwrap();
    pane.interaction.dwell = None;
    let mut better = pane.items.iter().find(|s| s.id == old).unwrap().clone();
    better.id = "new-lead".into();
    better.source = "Reuters".into();
    better.seen = false;
    better.item.ts += 1;
    let mut incoming = pane.items.clone();
    incoming.push(better);
    pane.interaction.expanded.insert("gone".into());
    pane.apply(incoming);
    ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    assert_eq!(pane.interaction.selected.as_deref(), Some("new-lead"));
    assert!(pane.interaction.expanded.contains("new-lead"));
    assert!(pane.interaction.expanded.contains(&old));
    assert!(!pane.interaction.expanded.contains("gone"));
    assert!(pane.interaction.dwell.is_none());
    assert!(!pane.items.iter().find(|s| s.id == "new-lead").unwrap().seen);
}

#[test]
fn refresh_preserves_top_row_screen_offset_and_offscreen_keys_scroll() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    pane.items = many_stories(60);
    pane.filters.days = 90;
    pane.display.wire = true;
    for _ in 0..28 {
        ui_frame(&ctx, &mut pane, 900.0, key_event(egui::Key::ArrowDown), true);
    }
    for _ in 0..60 {
        ui_frame(&ctx, &mut pane, 900.0, vec![], true);
    }
    let selected = pane.interaction.selected.clone().unwrap();
    let (before, _) = ui_frame(&ctx, &mut pane, 900.0, vec![], true);
    let selected_title = pane.items.iter().find(|s| s.id == selected).unwrap().item.title.clone();
    assert!(text_shapes(&before).iter().any(|(s, _)| s == &selected_title), "offscreen navigation must reveal its row");
    let anchor = pane.interaction.anchor.clone().unwrap();
    let title = pane.items.iter().find(|s| s.id == anchor).unwrap().item.title.clone();
    let before_y = text_shapes(&before).into_iter().find(|(s, _)| s == &title).unwrap().1.y;
    let mut incoming = pane.items.clone();
    let mut fresh = incoming[0].clone();
    fresh.id = "fresh".into();
    fresh.item.title = "Fresh unrelated article".into();
    fresh.item.ts += 1;
    incoming.insert(0, fresh);
    pane.apply(incoming);
    let (after, _) = ui_frame(&ctx, &mut pane, 900.0, vec![], true);
    let after_y = text_shapes(&after).into_iter().find(|(s, _)| s == &title).unwrap().1.y;
    assert!((after_y - before_y).abs() < 1.0, "anchor moved from {before_y} to {after_y}");
}

#[test]
fn escape_clears_focused_search_then_selection_and_releases_letters() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::ArrowDown), true);
    let selected = pane.interaction.selected.clone();
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::Slash), true);
    ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    // Keep whichever equal-time fixture sorts first visible. Short prefixes no longer match.
    let query = pane
        .items
        .iter()
        .find(|story| Some(&story.id) == selected.as_ref())
        .unwrap()
        .item
        .title
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    ui_frame(&ctx, &mut pane, 600.0, vec![egui::Event::Text(query.clone())], true);
    assert_eq!(pane.filters.search, query);
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::Escape), true);
    assert!(pane.filters.search.is_empty());
    assert_eq!(pane.interaction.selected, selected);
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::Escape), true);
    assert!(pane.interaction.selected.is_none());
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::J), true);
    assert!(pane.interaction.selected.is_none());
}

#[test]
fn clear_filters_preserves_date_range() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    pane.filters.days = 30;
    pane.filters.search = "notpresent".into();
    let (output, _) = ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    let position = text_shapes(&output).into_iter().find(|(s, _)| s == "Clear filters").unwrap().1;
    click_at(&ctx, &mut pane, position + egui::vec2(3.0, 3.0));
    assert_eq!(pane.filters.days, 30);
    assert!(pane.filters.search.is_empty());
}

#[test]
fn feed_key_capture_is_exact_and_swallows_repeat_text_only_for_owned_keys() {
    let ctx = egui::Context::default();
    let mut events = key_event(egui::Key::J);
    events.push(egui::Event::Text("j".into()));
    events.push(egui::Event::Key {
        key: egui::Key::J,
        physical_key: None,
        pressed: true,
        repeat: true,
        modifiers: egui::Modifiers::NONE,
    });
    events.push(egui::Event::Text("j".into()));
    events.push(egui::Event::Key {
        key: egui::Key::K,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::SHIFT,
    });
    events.push(egui::Event::Text("K".into()));
    let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
        let keys = ui.input_mut(|i| capture_keys(i, true, false));
        assert_eq!(keys, vec![egui::Key::J, egui::Key::J]);
        assert_eq!(
            ui.input(|i| i
                .events
                .iter()
                .filter_map(|e| if let egui::Event::Text(t) = e { Some(t.as_str()) } else { None })
                .collect::<String>()),
            "K"
        );
    });
    output.textures_delta.clear();
    for key in [egui::Key::J, egui::Key::K, egui::Key::C, egui::Key::U, egui::Key::Enter, egui::Key::Space] {
        assert!(!owns_key(key, false, false));
    }
}

#[test]
fn command_line_receives_unselected_letters_and_blocks_feed_keys_when_nonempty() {
    use super::super::{PaneKind, TerminalApp};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/test-data/news-command-ownership");
    let (worker, _requests) = crate::worker::Worker::recording();
    let mut app = TerminalApp::isolated_workspace(&root, worker);
    let id = app.new_pane(PaneKind::News(Box::new(visible_pane())));
    app.focus = id;
    let ctx = egui::Context::default();
    let mut events = key_event(egui::Key::J);
    events.push(egui::Event::Text("j".into()));
    let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| app.hotkeys(ui.ctx()));
    output.textures_delta.clear();
    assert_eq!(app.cmd, "J");
    let mut output = ctx
        .run_ui(egui::RawInput { events: key_event(egui::Key::ArrowDown), ..Default::default() }, |ui| {
            app.hotkeys(ui.ctx())
        });
    output.textures_delta.clear();
    let i = app.idx(id).unwrap();
    let PaneKind::News(n) = &app.panes[i].kind else { panic!() };
    assert!(n.interaction.pending_keys.is_empty());
}

#[test]
fn watch_edits_wait_for_debounce_enter_or_focus_loss() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    let mut buffer = String::new();
    let input_id = egui::Id::new("watch-save-test");
    let mut frame = |pane: &mut NewsPane, focused: bool, events| {
        let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
            let response = ui.add(egui::TextEdit::singleline(&mut buffer).id(input_id));
            if focused {
                response.request_focus();
            } else {
                response.surrender_focus();
            }
            show(ui, pane, &[], &mut vec![], false, None, 0);
        });
        output.textures_delta.clear();
    };
    pane.interaction.watches_dirty = Some(std::time::Instant::now());
    frame(&mut pane, true, vec![]);
    assert!(!pane.interaction.save_watches, "do not save on each keystroke");
    assert!(pane.interaction.watches_dirty.is_some());
    pane.interaction.watches_dirty = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
    frame(&mut pane, true, vec![]);
    assert!(std::mem::take(&mut pane.interaction.save_watches));
    assert!(pane.interaction.watches_dirty.is_none());
    pane.interaction.watches_dirty = Some(std::time::Instant::now());
    frame(&mut pane, true, key_event(egui::Key::Enter));
    assert!(std::mem::take(&mut pane.interaction.save_watches));
    pane.interaction.watches_dirty = Some(std::time::Instant::now());
    frame(&mut pane, false, vec![]);
    assert!(pane.interaction.save_watches);
}

#[test]
fn expanded_virtual_rows_use_measured_heights_and_settle_without_repaint() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    pane.items = many_stories(60);
    pane.filters.days = 90;
    pane.display.wire = true;
    ui_frame(&ctx, &mut pane, 900.0, key_event(egui::Key::ArrowDown), true);
    ui_frame(&ctx, &mut pane, 900.0, key_event(egui::Key::Space), true);
    // Allow egui's transient scrollbar animation to finish as well.
    for _ in 0..30 {
        ui_frame(&ctx, &mut pane, 900.0, vec![], false);
    }
    let (output, _) = ui_frame(&ctx, &mut pane, 900.0, vec![], false);
    let labels = text_shapes(&output);
    let first_y = labels.iter().find(|(s, _)| s == &pane.items[0].item.title).unwrap().1.y;
    let second_y = labels.iter().find(|(s, _)| s == &pane.items[1].item.title).unwrap().1.y;
    assert!((second_y - first_y - pane.cache.rows[0].height).abs() < 1.0);
    assert!(pane.cache.rows[0].height > dp(22.0));
    assert!(labels.len() < 160, "only visible rows should be laid out");
    assert!(output.viewport_output[&egui::ViewportId::ROOT].repaint_delay > std::time::Duration::from_secs(60));
    let rebuilds = pane.cache.rebuilds;
    ui_frame(&ctx, &mut pane, 900.0, vec![], false);
    assert_eq!(pane.cache.rebuilds, rebuilds);
}

#[test]
fn review_n6b_unselected_w_reaches_type_anywhere() {
    use super::super::{PaneKind, TerminalApp};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/test-data/news-review-w-ownership");
    let (worker, _requests) = crate::worker::Worker::recording();
    let mut app = TerminalApp::isolated_workspace(&root, worker);
    let id = app.new_pane(PaneKind::News(Box::new(visible_pane())));
    app.focus = id;
    let ctx = egui::Context::default();
    for (key, text) in [(egui::Key::W, "w"), (egui::Key::M, "m"), (egui::Key::T, "t")] {
        let mut events = key_event(key);
        events.push(egui::Event::Text(text.into()));
        let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| app.hotkeys(ui.ctx()));
        output.textures_delta.clear();
    }
    assert_eq!(
        app.cmd, "WMT",
        "unselected letters belong to type-anywhere (see review_unselected_r_reaches_type_anywhere)"
    );
}

fn n6b_click(ctx: &egui::Context, pane: &mut NewsPane, width: f32, position: egui::Pos2) -> egui::FullOutput {
    let button = |pressed| egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    ui_frame(ctx, pane, width, vec![egui::Event::PointerMoved(position), button(true)], true);
    ui_frame(ctx, pane, width, vec![button(false)], true);
    ui_frame(ctx, pane, width, vec![], true).0
}

#[test]
fn review_n6b_keyword_popover_survives_click_in_its_field() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    let (output, _) = ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    let chip = text_shapes(&output).iter().find(|(s, _)| s == "Keywords 0").unwrap().1 + egui::vec2(3.0, 3.0);
    let output = n6b_click(&ctx, &mut pane, 600.0, chip);
    assert!(pane.interaction.keyword_add_open);
    let field =
        text_shapes(&output).iter().find(|(s, _)| s.starts_with("Add keyword or")).expect("popover field visible").1
            + egui::vec2(3.0, 3.0);
    n6b_click(&ctx, &mut pane, 600.0, field);
    assert!(pane.interaction.keyword_add_open, "clicking inside the popover's own field must not dismiss it");
}

#[test]
fn n6b_narrow_keyword_editor_stays_inline_and_does_not_reopen() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    let (output, _) = ui_frame(&ctx, &mut pane, 320.0, vec![], true);
    let filters =
        text_shapes(&output).iter().find(|(s, _)| s.starts_with("Filters (")).unwrap().1 + egui::vec2(3.0, 3.0);
    let output = n6b_click(&ctx, &mut pane, 320.0, filters);
    let chip = text_shapes(&output).iter().find(|(s, _)| s == "Keywords 0").unwrap().1 + egui::vec2(3.0, 3.0);
    let output = n6b_click(&ctx, &mut pane, 320.0, chip);
    let field =
        text_shapes(&output).iter().find(|(s, _)| s.starts_with("Add keyword or")).unwrap().1 + egui::vec2(3.0, 3.0);
    let output = n6b_click(&ctx, &mut pane, 320.0, field);
    assert!(text_shapes(&output).iter().any(|(s, _)| s == "Sort (R)"), "Filters stays open");
    assert!(pane.interaction.keyword_add_inline);
    assert!(!pane.interaction.keyword_add_open, "inline state must not leak into a wide popup");
    // This menu is taller than the viewport, so close with Escape rather than
    // assuming that any particular viewport coordinate is outside it.
    ui_frame(&ctx, &mut pane, 320.0, key_event(egui::Key::Escape), true);
    ui_frame(&ctx, &mut pane, 320.0, key_event(egui::Key::Escape), true);
    ui_frame(&ctx, &mut pane, 320.0, vec![], true);
    assert!(!pane.interaction.keyword_add_inline);
    let output = n6b_click(&ctx, &mut pane, 320.0, filters);
    assert!(!text_shapes(&output).iter().any(|(s, _)| s.starts_with("Add keyword or")));
    let (output, _) = ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    assert!(!pane.interaction.keyword_add_open);
    assert!(!text_shapes(&output).iter().any(|(s, _)| s.starts_with("Add keyword or")));
}

#[test]
fn n6b_overflow_starts_with_the_panes_own_status_reason() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    pane.error = Some("Finnhub rate-limit: specific failure reason".into());
    super::super::data_sources::State::default().update_news_sources(&mut pane, 100_000);
    ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    // The overflow icon is reserved at the top right, independent of the summary width.
    let output = n6b_click(&ctx, &mut pane, 600.0, egui::pos2(590.0, 10.0));
    let labels = text_shapes(&output);
    let reason = labels
        .iter()
        .find(|(s, _)| s == "Finnhub rate-limit: specific failure reason")
        .expect("overflow restores the detailed pane reason")
        .1;
    let summary = labels
        .iter()
        .filter(|(s, p)| s.starts_with("15 sources") && p.y > reason.y)
        .min_by(|a, b| a.1.y.total_cmp(&b.1.y))
        .expect("aggregate follows the pane reason")
        .1;
    assert!(reason.y < summary.y);
}

#[test]
fn review_n6b_narrow_zero_keyword_chip_shows_add_field() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    let (output, _) = ui_frame(&ctx, &mut pane, 320.0, vec![], true);
    let filters =
        text_shapes(&output).iter().find(|(s, _)| s.starts_with("Filters (")).unwrap().1 + egui::vec2(3.0, 3.0);
    let output = n6b_click(&ctx, &mut pane, 320.0, filters);
    let chip =
        text_shapes(&output).iter().find(|(s, _)| s == "Keywords 0").expect("chip in Filters").1 + egui::vec2(3.0, 3.0);
    let output = n6b_click(&ctx, &mut pane, 320.0, chip);
    assert!(
        text_shapes(&output).iter().any(|(s, _)| s.starts_with("Add keyword or")),
        "0-keyword click in a narrow pane must show the add field (open flag: {})",
        pane.interaction.keyword_add_open
    );
}
