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
