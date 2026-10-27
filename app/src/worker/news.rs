//! N1a: one visible-only news scheduler; panes query saved history, never the network.
use super::*;
use tr_data::{
    finnhub::{self, Job, Schedule},
    news_store::{Filters, NewsStore, Query, Story},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub pane: PaneId,
    pub symbol: Option<String>,
    pub filters: Filters,
    pub symbols: Vec<String>,
}
#[derive(Default)]
pub(super) struct Service {
    pub views: Mutex<Vec<View>>,
    pub watchlist: Mutex<Vec<String>>,
    pub store: Mutex<Option<NewsStore>>,
    pub status: Mutex<String>,
    pub key: Mutex<Option<String>>,
    pub memory_only: Mutex<bool>,
    published: Mutex<Vec<Published>>,
}
struct Published {
    view: View,
    items: Result<Vec<Story>, String>,
    status: String,
}
fn changed(
    published: &mut Vec<Published>,
    view: &View,
    items: &Result<Vec<Story>, String>,
    status: &str,
    force: bool,
) -> (bool, bool) {
    let old = published.iter().find(|old| old.view == *view);
    let rows_changed = force || old.is_none_or(|old| old.items != *items);
    let status_changed = old.is_none_or(|old| old.status != status);
    published.retain(|old| old.view.pane != view.pane);
    published.push(Published { view: view.clone(), items: items.clone(), status: status.into() });
    (rows_changed, status_changed)
}
/// Preserve the bad file; failure to recover disk storage must never abort startup.
pub(super) fn open_store(path: &std::path::Path) -> (Option<NewsStore>, bool) {
    match NewsStore::open(path) {
        Ok(store) => return (Some(store), false),
        Err(error) => tracing::warn!("saved news unavailable: {error:#}"),
    }
    if path.exists() {
        let base = format!("{}.corrupt-{}", path.display(), chrono::Utc::now().timestamp());
        let mut backup = std::path::PathBuf::from(&base);
        let mut index = 0;
        while backup.exists() {
            index += 1;
            backup = format!("{base}-{index}").into();
        }
        if let Err(error) = std::fs::rename(path, &backup) {
            tracing::warn!("cannot preserve bad news store: {error}");
            return (NewsStore::memory().ok(), true);
        }
        // Keep associated SQLite journals with their database, never reuse them.
        for suffix in ["-wal", "-shm"] {
            let sidecar = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
            if sidecar.exists()
                && let Err(error) = std::fs::rename(&sidecar, format!("{}{suffix}", backup.display()))
            {
                tracing::warn!("cannot preserve news journal: {error}");
                return (NewsStore::memory().ok(), true);
            }
        }
    }
    match NewsStore::open(path) {
        Ok(store) => (Some(store), false),
        Err(error) => {
            tracing::warn!("fresh news store unavailable, using memory: {error:#}");
            (NewsStore::memory().ok(), true)
        }
    }
}
pub(super) async fn read(s: &Shared, view: View) {
    read_view(s, view, true).await;
}
async fn read_view(s: &Shared, view: View, force: bool) {
    let now = chrono::Utc::now().timestamp();
    let days = view.filters.days.clamp(1, 90) as i64;
    let start = if days == 1 { tr_data::news_store::day_start(now) } else { now - days * 86400 };
    let query = Query { symbols: view.symbols.clone(), filters: view.filters.clone(), start, ..Query::default() };
    let items = match s.news.store.lock().await.as_ref() {
        Some(store) => store.query(&query).map_err(|_| "News store unavailable".into()),
        None => Err("News store unavailable".into()),
    };
    let mut status = s.news.status.lock().await.clone();
    if status.is_empty() {
        status = "Showing saved headlines".into();
    }
    if *s.news.memory_only.lock().await {
        status = format!("Saved headlines unavailable · {status}");
    }
    let (rows_changed, status_changed) = changed(&mut *s.news.published.lock().await, &view, &items, &status, force);
    if rows_changed {
        s.send(Response::NewsSaved { view, items, status });
    } else if status_changed {
        // Status transitions (pause/offline/recovery) are separate from item replies.
        // No timestamp-only heartbeat or unchanged list is sent on the five-second tick.
        s.send(Response::NewsStatus { view, status });
    }
}
async fn publish(s: &Shared) {
    let views = s.news.views.lock().await.clone();
    for view in views {
        read_view(s, view, false).await;
    }
}
fn refusal_status(error: &tr_net::Refused) -> String {
    match error {
        tr_net::Refused::Paused { until } => {
            let time = chrono::DateTime::from_timestamp(*until, 0)
                .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
                .unwrap_or_default();
            format!("Finnhub paused until {time} — showing saved headlines")
        }
        _ => format!("Finnhub · {error} — showing saved headlines"),
    }
}
pub(super) fn use_fallback(enabled: bool, key: Option<&str>) -> bool {
    !enabled || key.is_none_or(|k| k.trim().is_empty())
}
#[derive(Default)]
struct FallbackState {
    fallback_due: i64,
    next_slot: i64,
    round: std::collections::VecDeque<String>,
    watch_due: std::collections::HashMap<String, i64>,
    pane_due: std::collections::HashMap<Option<String>, i64>,
    attempts: std::collections::VecDeque<i64>,
}
/// No per-symbol fanout for watchlists: one attempt/minute, one/symbol/hour.
/// Errors consume that symbol's slot, not the rest of the round.
fn fallback_targets(views: &[View], now: i64, state: &mut FallbackState) -> Vec<Option<String>> {
    use tr_data::news_store::Scope;
    let mut targets = vec![];
    // A rolling feature-wide ceiling also covers mixed market/symbol/watchlist panes.
    // Count attempts conservatively, even when the guard serves a cache hit.
    while state.attempts.front().is_some_and(|at| now - at >= 3600) {
        state.attempts.pop_front();
    }
    let mut watch: Vec<_> =
        views.iter().filter(|v| v.filters.scope == Scope::Watchlist).flat_map(|v| v.symbols.iter().cloned()).collect();
    watch.sort();
    watch.dedup();
    state.round.retain(|s| watch.contains(s));
    if state.round.is_empty() && now >= state.fallback_due {
        state.round = watch.into_iter().filter(|s| state.watch_due.get(s).is_none_or(|due| now >= *due)).collect();
    }
    if now >= state.next_slot
        && state.attempts.len() < 60
        && let Some(symbol) = state.round.pop_front()
    {
        state.watch_due.insert(symbol.clone(), now + 3600);
        state.next_slot = now + 60;
        targets.push(Some(symbol));
        state.attempts.push_back(now);
        // Delay the next round only after this one has completed.
        if state.round.is_empty() {
            state.fallback_due = now + 300;
        }
    }
    for view in views.iter().filter(|v| v.filters.scope != Scope::Watchlist) {
        let target = if view.filters.scope == Scope::All { None } else { view.symbol.clone() };
        if state.pane_due.get(&target).is_none_or(|due| now >= *due)
            && (targets.contains(&target) || state.attempts.len() < 60)
        {
            state.pane_due.insert(target.clone(), now + 300);
            if !targets.contains(&target) {
                targets.push(target);
                state.attempts.push_back(now);
            }
        }
    }
    targets
}
/// Unsupported company keys retain the existing guarded Yahoo/CoinDesk route,
/// even when Finnhub is configured. Never ask Finnhub for a fabricated US key.
fn fallback_views(views: &[View], fallback_all: bool) -> Vec<View> {
    use tr_data::news_store::Scope;
    views
        .iter()
        .filter_map(|view| {
            if fallback_all {
                return Some(view.clone());
            }
            let mut view = view.clone();
            match view.filters.scope {
                Scope::All => None,
                Scope::Symbol => view.symbol.as_deref().is_some_and(|s| finnhub::symbol(s).is_none()).then_some(view),
                Scope::Watchlist => {
                    view.symbols.retain(|s| finnhub::symbol(s).is_none());
                    (!view.symbols.is_empty()).then_some(view)
                }
            }
        })
        .collect()
}
fn fallback_items(items: Vec<tr_data::NewsItem>, target: Option<&str>) -> Vec<Story> {
    items
        .into_iter()
        .map(|mut item| {
            if let Some(symbol) = target {
                item.tickers.push(symbol.into());
            }
            let source = if item.publisher.starts_with("CoinDesk") { "CoinDesk" } else { "Yahoo" };
            let mut story = Story::new(item, source);
            story.category = if source == "CoinDesk" {
                "crypto"
            } else if target.is_some() {
                "company"
            } else {
                "general"
            }
            .into();
            story
        })
        .collect()
}
fn bounds(store: Option<&NewsStore>, job: &Job) -> anyhow::Result<(Option<i64>, u64)> {
    let store = store.ok_or_else(|| anyhow::anyhow!("News store unavailable"))?;
    match job {
        Job::Company(symbol) => Ok((store.latest(symbol)?, 0)),
        Job::Market(category) => Ok((None, finnhub::cursor(store, category)?)),
    }
}
pub(super) async fn run(s: Arc<Shared>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let client = finnhub::Client::default();
    let mut schedule = Schedule::default();
    let mut fallback = FallbackState::default();
    loop {
        interval.tick().await;
        let views = s.news.views.lock().await.clone();
        if views.is_empty() {
            continue;
        }
        let now = chrono::Utc::now().timestamp();
        let key = s.news.key.lock().await.clone();
        let guard = match tr_net::shared() {
            Ok(guard) => guard,
            Err(error) => {
                if now >= schedule.backoff_until {
                    schedule.refused(now, &error);
                    *s.news.status.lock().await = refusal_status(&error);
                    publish(&s).await;
                }
                continue;
            }
        };
        let enabled = guard
            .status()
            .ok()
            .and_then(|rows| rows.into_iter().find(|r| r.group == "finnhub"))
            .is_some_and(|r| r.enabled);
        let fallback_all = use_fallback(enabled, key.as_deref());
        let routed = fallback_views(&views, fallback_all);
        let targets = fallback_targets(&routed, now, &mut fallback);
        if !targets.is_empty() {
            let mut status = "Yahoo / CoinDesk · showing saved headlines".to_string();
            for target in targets {
                if s.news.views.lock().await.is_empty() {
                    break;
                }
                match s.provider.news(target.as_deref()).await {
                    Ok(items) => {
                        let stories = fallback_items(items, target.as_deref());
                        if let Some(store) = s.news.store.lock().await.as_mut() {
                            if store.insert(&stories, now).is_err() {
                                status = "News store write failed — showing saved headlines".into();
                            } else {
                                status = "Yahoo / CoinDesk · updated just now".into();
                            }
                        }
                    }
                    Err(_) => {
                        status = "Yahoo / CoinDesk unavailable — showing saved headlines".into();
                    }
                }
            }
            *s.news.status.lock().await = status;
            publish(&s).await;
        }
        if fallback_all {
            continue;
        }
        let symbols = s.news.watchlist.lock().await.clone();
        let priority: Vec<_> = views
            .iter()
            .filter(|v| v.filters.scope == tr_data::news_store::Scope::Symbol)
            .flat_map(|v| v.symbols.iter().cloned())
            .collect();
        let all_market = views.iter().any(|v| v.filters.scope == tr_data::news_store::Scope::All);
        let Some(job) = schedule.next_for_views(now, true, all_market, &symbols, &priority) else {
            continue;
        };
        let token = key.as_deref().unwrap_or_default();
        let saved_bounds = {
            let store = s.news.store.lock().await;
            bounds(store.as_ref(), &job)
        };
        let Ok((latest, cursor)) = saved_bounds else {
            // A broken store/cursor must not silently become a fresh history request.
            schedule.refused(now, &tr_net::Refused::Ledger);
            *s.news.status.lock().await = "News cursor unavailable — showing saved headlines".into();
            publish(&s).await;
            continue;
        };
        let result = match &job {
            Job::Company(symbol) => client.company(guard, token, symbol, latest, now).await,
            Job::Market(category) => client.market(guard, token, category, cursor).await,
        };
        let status = match result {
            Ok(items) => {
                let mut lock = s.news.store.lock().await;
                let saved = lock.as_mut().is_some_and(|store| match job {
                    Job::Market(category) => finnhub::save_market(store, category, &items, now).is_ok(),
                    Job::Company(_) => store.insert(&items, now).is_ok(),
                });
                if saved {
                    let count = symbols.iter().filter(|s| finnhub::symbol(s).is_some()).count();
                    let pacing = finnhub::company_status(count);
                    format!(
                        "Finnhub · updated just now{}",
                        if pacing.is_empty() { String::new() } else { format!(" · {pacing}") }
                    )
                } else {
                    "News store write failed — showing saved headlines".into()
                }
            }
            Err(error) => {
                schedule.refused(now, &error);
                refusal_status(&error)
            }
        };
        *s.news.status.lock().await = status;
        publish(&s).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn view(scope: tr_data::news_store::Scope, symbols: &[&str]) -> View {
        View {
            pane: 1,
            symbol: symbols.first().map(|s| (*s).into()),
            filters: Filters { scope, ..Default::default() },
            symbols: symbols.iter().map(|s| (*s).into()).collect(),
        }
    }
    #[test]
    fn review_fallback_watchlist_bounded_round_robin_and_error_isolation() {
        use tr_data::news_store::Scope;
        for count in [20, 150] {
            let symbols: Vec<_> = (0..count).map(|i| format!("S{i:03} US Equity")).collect();
            let views = vec![View { symbols: symbols.clone(), ..view(Scope::Watchlist, &[]) }];
            let mut state = FallbackState::default();
            let mut seen = std::collections::HashMap::new();
            let mut hourly = [0; 24];
            let mut first_round = vec![];
            for now in (0..86400).step_by(5) {
                let targets = fallback_targets(&views, now, &mut state);
                assert!(targets.len() <= 1, "watchlist must never fan out");
                for target in targets {
                    let symbol = target.unwrap();
                    if let Some(last) = seen.insert(symbol.clone(), now) {
                        assert!(now - last >= 3600);
                    }
                    hourly[(now / 3600) as usize] += 1;
                    if first_round.len() < count {
                        first_round.push(symbol);
                    }
                    // Simulate an error on the first symbol: selection, not success,
                    // owns pacing. The next symbol must still be attempted next minute.
                    if first_round.len() < count {
                        assert_eq!(state.fallback_due, 0);
                    }
                }
            }
            assert_eq!(first_round, symbols);
            assert!(hourly.iter().all(|n| *n <= 60));
            assert!(hourly.iter().sum::<usize>() <= if count == 20 { 480 } else { 1440 });
        }
    }
    #[test]
    fn review_mixed_panes_share_rolling_hour_fallback_ceiling() {
        use tr_data::news_store::Scope;
        let mut watch = view(Scope::Watchlist, &[]);
        watch.symbols = (0..150).map(|i| format!("S{i:03} US Equity")).collect();
        let views = vec![
            watch,
            view(Scope::All, &[]),
            view(Scope::Symbol, &["AAPL US Equity"]),
            view(Scope::Symbol, &["MSFT US Equity"]),
        ];
        let mut state = FallbackState::default();
        let mut requests = std::collections::VecDeque::new();
        let mut seen = std::collections::HashSet::new();
        for now in (0..86400).step_by(5) {
            while requests.front().is_some_and(|at| now - at >= 3600) {
                requests.pop_front();
            }
            for symbol in fallback_targets(&views, now, &mut state) {
                requests.push_back(now);
                if let Some(symbol) = symbol {
                    seen.insert(symbol);
                }
            }
            assert!(requests.len() <= 60);
        }
        assert!(views[0].symbols.iter().all(|s| seen.contains(s)), "mixed panes must not starve the watchlist round");
    }
    #[test]
    fn review_fallback_routing_and_old_pane_cadence_with_key() {
        use tr_data::news_store::Scope;
        let us = view(Scope::Symbol, &["AAPL US Equity"]);
        let crypto = view(Scope::Symbol, &["BTCUSD Curncy"]);
        let foreign = view(Scope::Symbol, &["VOD LN Equity"]);
        let all = view(Scope::All, &[]);
        let views = vec![us, crypto.clone(), foreign.clone(), all.clone()];
        assert_eq!(fallback_views(&views, false), vec![crypto, foreign]);
        assert_eq!(fallback_views(&views, true), views);
        let mut state = FallbackState::default();
        assert_eq!(fallback_targets(&[views[0].clone(), views[0].clone(), all.clone()], 0, &mut state).len(), 2);
        assert!(fallback_targets(&[views[0].clone(), all.clone()], 299, &mut state).is_empty());
        assert_eq!(fallback_targets(&[views[0].clone(), all], 300, &mut state).len(), 2);
    }
    #[test]
    fn review_publish_only_changed_items_and_status_transitions() {
        use tr_data::news_store::Scope;
        let view = view(Scope::Symbol, &["AAPL US Equity"]);
        let mut published = vec![];
        let empty = Ok(vec![]);
        assert_eq!(changed(&mut published, &view, &empty, "saved", false), (true, true));
        for _ in 0..100 {
            assert_eq!(changed(&mut published, &view, &empty, "saved", false), (false, false));
        }
        assert_eq!(changed(&mut published, &view, &empty, "paused", false), (false, true));
        assert_eq!(changed(&mut published, &view, &empty, "paused", true), (true, false));
        let rows =
            Ok(finnhub::parse(include_bytes!("../../../crates/data/tests/fixtures/finnhub_news.json"), None).unwrap());
        assert_eq!(changed(&mut published, &view, &rows, "saved", false), (true, true));
        assert_eq!(changed(&mut published, &view, &rows, "saved", false), (false, false));
    }
    #[test]
    fn missing_store_or_corrupt_cursor_never_requests_first_run_history() {
        assert!(bounds(None, &Job::Market("general")).is_err());
        let store = NewsStore::memory().unwrap();
        assert_eq!(bounds(Some(&store), &Job::Market("general")).unwrap(), (None, 0));
        store.set_meta("minId:general", "broken").unwrap();
        assert!(bounds(Some(&store), &Job::Market("general")).is_err());
    }
    #[test]
    fn missing_key_or_off_uses_fallback_only() {
        assert!(use_fallback(true, None));
        assert!(use_fallback(true, Some("  ")));
        assert!(use_fallback(false, Some("secret")));
        assert!(!use_fallback(true, Some("secret")));
        let items =
            tr_data::rss::parse_rss(include_str!("../../../crates/data/tests/fixtures/coindesk_rss.xml"), "CoinDesk")
                .unwrap();
        let stories = fallback_items(items, Some("BTCUSD Curncy"));
        let mut store = NewsStore::memory().unwrap();
        store.insert(&stories, 0).unwrap();
        let query = Query { symbols: vec!["BTCUSD Curncy".into()], ..Default::default() };
        let rows = store.query(&query).unwrap();
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|s| s.source == "CoinDesk" && s.category == "crypto"));
        store.insert(&fallback_items(vec![], Some("BTCUSD Curncy")), 1).unwrap();
        assert_eq!(store.query(&query).unwrap().len(), 5);
    }
}

#[cfg(test)]
mod review {
    #[test]
    fn review_corrupt_news_store_does_not_abort_worker_spawn() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../target/test-data/news-corrupt-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("news.sqlite3");
        let bad = b"this is not a sqlite database, just garbage bytes padding padding";
        std::fs::write(&path, bad).unwrap();
        assert!(tr_data::news_store::NewsStore::open(&path).is_err());
        let (store, memory_only) = super::open_store(&path);
        assert!(store.is_some());
        assert!(!memory_only);
        let backup = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.file_name().unwrap().to_string_lossy().starts_with("news.sqlite3.corrupt-"))
            .unwrap();
        assert_eq!(std::fs::read(backup).unwrap(), bad);
        assert!(store.unwrap().query(&tr_data::news_store::Query::default()).unwrap().is_empty());
        // A non-directory parent forces both disk attempts to fail, even as root.
        let blocker = root.join("blocker");
        std::fs::write(&blocker, b"keep me").unwrap();
        let (store, memory_only) = super::open_store(&blocker.join("news.sqlite3"));
        assert!(memory_only);
        assert!(store.is_some());
        assert_eq!(std::fs::read(blocker).unwrap(), b"keep me");
        // Spawn must call the recovery path, not propagate the open failure.
        let spawn = include_str!("../worker.rs");
        assert!(
            !spawn.contains("NewsStore::open(dir.join(\"news.sqlite3\"))?;"),
            "Worker::spawn aborts on a news store error"
        );
    }
}
