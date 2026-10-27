use super::*;
use crate::layouts::{LayoutDoc, PaneSettings};
fn stories() -> Vec<tr_data::news_store::Story> {
    tr_data::finnhub::parse(include_bytes!("../../../crates/data/tests/fixtures/finnhub_news.json"), None).unwrap()
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
        assert_eq!(settings.symbol.as_deref(), Some("AAPL US Equity"));
        assert_eq!(LayoutDoc::from_json(&doc.to_json()).unwrap(), doc);
    }
    let mut doc = LayoutDoc::from_json(include_bytes!("../../tests/fixtures/news_layout_n1a.json")).unwrap();
    let PaneSettings::News(Some(settings)) = &mut doc.panes[0].settings else { panic!() };
    settings.display = Display { wire: true, unread: true, watch_hits: true };
    assert_eq!(LayoutDoc::from_json(&doc.to_json()).unwrap(), doc);
}
#[test]
fn row_columns_omit_linked_symbol_and_include_union_meta() {
    let mut stories = stories();
    stories[0].source = "Reuters".into();
    stories[0].item.tickers = vec!["AAPL US Equity".into(), "MSFT US Equity".into(), "NVDA US Equity".into()];
    let mut cluster = Cluster::wire(&stories[0]);
    cluster.sources = vec!["Reuters".into(), "Yahoo".into(), "CNBC".into()];
    let model = row_model(&cluster, Some("AAPL US Equity"), cluster.newest + 12 * 60);
    assert_eq!(model.id, stories[0].id);
    assert_eq!(model.time, "12m");
    assert!(model.full_time.contains("UTC"));
    assert_eq!(model.ticker.as_deref(), Some("MSFT US Equity"));
    assert_eq!(chart_ticker(&cluster, Some("AAPL US Equity")).map(String::as_str), Some("MSFT US Equity"));
    assert_eq!(model.extra_tickers, vec!["NVDA US Equity"]);
    assert!(model.meta.starts_with("Reuters +2 · +1"));
    assert!(model.meta.contains("Earnings"));
    assert!(model.unread);
    cluster.members[0].seen = true;
    assert!(!row_model(&cluster, None, cluster.newest).unread);
    let single = Cluster::wire(&tr_data::news_store::Story::new(
        tr_data::NewsItem {
            title: "Only AAPL".into(),
            publisher: "Reuters".into(),
            link: "https://fixture.test/single".into(),
            ts: 1,
            tickers: vec!["AAPL US Equity".into()],
        },
        "Reuters",
    ));
    assert_eq!(row_model(&single, Some("AAPL US Equity"), 2).ticker, None);
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
        assert!(owns_key(key));
    }
    assert!(!owns_key(egui::Key::A));
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
    prefs.keyword_watches.push(KeywordWatch { name: "Chips".into(), words: "NVDA \"AI chip\"".into(), colour: 5 });
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
        let today = labels.iter().find(|(s, _)| s == "Today").unwrap().1;
        let ninety = labels.iter().find(|(s, _)| s == "90D").unwrap().1;
        assert!((today.y - ninety.y).abs() < 1.0, "single date chips may not wrap");
        assert_eq!(labels.iter().any(|(s, _)| s == "Watch hits"), width >= 360.0);
        let positions: Vec<_> = pane
            .items
            .iter()
            .map(|story| labels.iter().find(|(text, _)| text == &story.item.title).unwrap().1.y)
            .collect();
        assert!(
            ((positions[0] - positions[1]).abs() - dp(22.0)).abs() < 1.0,
            "one headline per dp(22) row: {positions:?}"
        );
    }
}
#[test]
fn keyboard_dwell_marks_seen_once_and_text_fields_own_typing() {
    let ctx = egui::Context::default();
    let mut pane = visible_pane();
    ui_frame(&ctx, &mut pane, 600.0, vec![], true);
    ui_frame(&ctx, &mut pane, 600.0, key_event(egui::Key::J), true);
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
    let id = app.new_pane(PaneKind::News(Box::new(visible_pane())));
    app.focus = id;
    let ctx = egui::Context::default();
    let mut events = key_event(egui::Key::J);
    events.push(egui::Event::Text("j".into()));
    let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| app.hotkeys(ui.ctx()));
    output.textures_delta.clear();
    assert!(app.cmd.is_empty(), "News navigation must bypass type-anywhere");
}
