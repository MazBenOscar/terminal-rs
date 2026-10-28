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
    pub updated: Mutex<Option<Instant>>,
    pub key: Mutex<Option<String>>,
    pub memory_only: Mutex<bool>,
    published: Mutex<Vec<Published>>,
    pub local: Mutex<LocalState>,
}
/// All N4 reads are local. This service has no provider/guard handle and adds no
/// interests or requests to either source scheduler.
pub(super) struct LocalState {
    settings: tr_data::news_store::local::Notifications,
    watches: Vec<tr_data::news_intelligence::KeywordWatch>,
    watched: Vec<String>,
    session: i64,
    since: [i64; 5],
    volume_at: i64,
    volumes: Vec<tr_data::news_store::local::Volume>,
    history: Option<Vec<String>>,
}
impl Default for LocalState {
    fn default() -> Self {
        let session = chrono::Utc::now().timestamp();
        Self {
            settings: Default::default(),
            watches: vec![],
            watched: vec![],
            session,
            since: [session; 5],
            volume_at: 0,
            volumes: vec![],
            history: None,
        }
    }
}
impl LocalState {
    pub fn configure(
        &mut self,
        settings: tr_data::news_store::local::Notifications,
        watches: Vec<tr_data::news_intelligence::KeywordWatch>,
        watched: Vec<String>,
    ) {
        let now = chrono::Utc::now().timestamp();
        let flags =
            |s: &tr_data::news_store::local::Notifications| [s.filings, s.insiders, s.keywords, s.breaking, s.volume];
        for (i, (old, new)) in flags(&self.settings).into_iter().zip(flags(&settings)).enumerate() {
            if old != new {
                self.since[i] = now;
            }
        }
        if self.watches != watches {
            self.since[2] = now;
        }
        self.settings = settings;
        self.watches = watches;
        self.watched = watched;
    }
}

pub(super) async fn local_read(
    s: &Shared,
    args: &tr_agent_tools::args::LiveArgs,
    symbols: &[String],
) -> Result<serde_json::Value, String> {
    use tr_data::news_store::local;
    let now = chrono::Utc::now().timestamp();
    let filing_symbols = if symbols.is_empty() { s.news.local.lock().await.watched.clone() } else { symbols.to_vec() };
    let edgar_path = data_dir().join("edgar.sqlite3");
    let filings = if edgar_path.exists() {
        tr_edgar::store::Store::open(edgar_path)
            .and_then(|db| db.local_filings(now - 90 * 86400, now))
            .map_err(|_| "Saved filings unavailable")?
    } else {
        vec![]
    };
    match args {
        tr_agent_tools::args::LiveArgs::ReadNews(a) => {
            let hours = a.hours.unwrap_or(24).clamp(1, 168);
            let start = now - i64::from(hours) * 3600;
            let scope = a.scope.as_deref().unwrap_or(if a.symbol.is_some() {
                "symbol"
            } else if a.watchlist.is_some() {
                "watchlist"
            } else {
                "market"
            });
            let query = Query {
                symbols: symbols.to_vec(),
                start,
                end: now,
                limit: tr_data::news_intelligence::MAX_ITEMS,
                filters: Filters {
                    scope: match scope {
                        "symbol" => tr_data::news_store::Scope::Symbol,
                        "watchlist" => tr_data::news_store::Scope::Watchlist,
                        _ => tr_data::news_store::Scope::All,
                    },
                    search: a.query.clone().unwrap_or_default(),
                    ..Default::default()
                },
                ..Default::default()
            };
            let store = s.news.store.lock().await;
            let items = store
                .as_ref()
                .ok_or("Saved headlines unavailable")?
                .query(&query)
                .map_err(|_| "Saved headlines unavailable")?;
            let metadata = serde_json::json!({"filings":tr_edgar::local_metadata(&filings,&filing_symbols,start,now,&[]),
                "insider_summary":tr_edgar::local_insider_summary(&filings,&filing_symbols,start,now)});
            serde_json::to_value(local::digest(
                &items,
                scope,
                start,
                now,
                a.limit.unwrap_or(20),
                a.include_summary,
                metadata,
            ))
            .map_err(|e| e.to_string())
        }
        tr_agent_tools::args::LiveArgs::ReadFilings(a) => Ok(serde_json::json!({"disclosure":local::DISCLOSURE,
            "filings":tr_edgar::local_metadata(&filings,symbols,now-i64::from(a.days.unwrap_or(30).clamp(1,90))*86400,now,&a.forms)})),
        _ => Err("Not a local news tool".into()),
    }
}
/// Claims every first observation, including disabled/non-matching stories. A
/// later retag, watch edit, or backfill cannot turn a saved item into an alert.
async fn local_tick(s: &Shared) -> anyhow::Result<()> {
    use chrono::Timelike;
    use tr_data::news_store::local;
    let now = chrono::Utc::now().timestamp();
    let mut state = s.news.local.lock().await;
    let store_guard = s.news.store.lock().await;
    let Some(store) = store_guard.as_ref() else { return Ok(()) };
    state.begin_session(store)?;
    let rows = store.query(&Query {
        start: now - 7200,
        end: now,
        limit: tr_data::news_intelligence::MAX_ITEMS,
        filters: Filters { scope: tr_data::news_store::Scope::All, ..Default::default() },
        ..Default::default()
    })?;
    let mut messages = state.news_alerts(store, &rows, now)?;
    let path = data_dir().join("edgar.sqlite3");
    if path.exists() {
        let db = tr_edgar::store::Store::open(path)?;
        let filings = db.filings()?;
        messages.extend(state.filing_alerts(&db, &filings, now)?);
    }
    if now - state.volume_at >= 300 {
        state.volume_at = now;
        let volumes = store.volume_anomalies(&state.watched, now)?;
        for v in &volumes {
            if store.claim_notification("volume", &format!("{}:{}", v.key, v.hour))?
                && state.settings.volume
                && v.hour >= state.since[4].max(state.session)
            {
                messages.push(format!("Unusual volume: {} {} stories this hour (usual {})", v.key, v.count, v.usual));
            }
        }
        if state.volumes != volumes {
            state.volumes = volumes.clone();
            s.send(Response::NewsVolume(volumes));
        }
    }
    let quiet = local::quiet(state.settings.quiet_start, state.settings.quiet_end, chrono::Local::now().hour());
    let (desktop, history) = store.deliver_notifications(&messages, now, quiet)?;
    if !desktop.is_empty() || state.history.as_ref() != Some(&history) {
        state.history = Some(history.clone());
        s.send(Response::FeedNotices { desktop, history });
    }
    Ok(())
}
impl LocalState {
    fn begin_session(&self, store: &NewsStore) -> anyhow::Result<()> {
        if self.history.is_none() {
            // Old in-app history and rate reservations survive, but a pending
            // overflow summary must not turn into restart catch-up delivery.
            store.set_meta("notification:overflow", "0")?;
        }
        Ok(())
    }
    fn news_alerts(&self, store: &NewsStore, rows: &[Story], now: i64) -> anyhow::Result<Vec<String>> {
        use tr_data::news_store::local;
        let state = self;
        let mut messages = vec![];
        for story in rows {
            let first = store.claim_notification("keyword", &story.id)?;
            if first
                && state.settings.keywords
                && local::fresh(story.item.ts, state.since[2], state.session, now)
                && state.watches.iter().any(|w| {
                    w.notify && !tr_data::news_intelligence::watch_ranges(&story.item.title, &w.words).is_empty()
                })
            {
                messages.push(format!("Keyword watch · {}", story.item.title));
            }
        }
        for c in tr_data::news_intelligence::clusters(rows) {
            // Earliest chronological member, not the publisher-ranked lead.
            let sources: std::collections::BTreeSet<_> =
                c.members.iter().filter(|m| m.item.ts >= now - 3600).map(|m| &m.source).collect();
            if sources.len() >= 4
                && store.claim_cluster(&c.member_ids)?
                && state.settings.breaking
                && !c.members.iter().any(|m| store.notification_blocked(&m.id).unwrap_or(true))
                && c.subjects.iter().any(|t| state.watched.contains(t))
                && c.members.iter().all(|m| local::fresh(m.item.ts, state.since[3], state.session, now))
            {
                messages.push(format!("Breaking · {} sources · {}", sources.len(), c.lead.item.title));
            }
        }
        Ok(messages)
    }
    fn filing_alerts(
        &self,
        db: &tr_edgar::store::Store,
        filings: &[tr_edgar::Filing],
        now: i64,
    ) -> anyhow::Result<Vec<String>> {
        use tr_data::news_store::local;
        let state = self;
        let mut messages = vec![];
        for f in filings {
            if !f.transient_symbol.is_empty() {
                db.claim_notification("filing", &f.accession)?;
                db.claim_notification("insider", &f.accession)?;
                continue;
            }
            if f.reporting_owner || (f.form.starts_with("8-K") && !f.from_submissions && f.items.is_empty()) {
                continue;
            }
            let watched = state.watched.iter().find(|s| f.matches_symbol(s));
            let first = db.claim_notification("filing", &f.accession)?;
            if first
                && state.settings.filings
                && watched.is_some()
                && local::fresh(f.time(), state.since[0], state.session, now)
                && matches!(
                    f.form.trim_end_matches("/A"),
                    "8-K" | "10-K" | "10-Q" | "SC 13D" | "SC 13G" | "13D" | "13G"
                )
            {
                messages.push(format!("{} · {}", f.ticker, f.detail()));
            }
            // Do not consume a pending XML accession until its local parse exists.
            if f.insider_parsed
                && db.claim_notification("insider", &f.accession)?
                && state.settings.insiders
                && local::fresh(f.time(), state.since[1], state.session, now)
                && let Some(symbol) = watched
                && let Some((value, cluster)) = tr_edgar::insider_buy(filings, f, symbol, state.settings.insider_min)
            {
                messages.push(format!(
                    "{} · Insider buy {}{}",
                    f.ticker,
                    tr_edgar::insider::value(value),
                    if cluster { " · Cluster buy" } else { "" }
                ));
            }
        }
        Ok(messages)
    }
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
pub(super) async fn mark_seen(s: &Shared, ids: Vec<String>) {
    if let Some(store) = s.news.store.lock().await.as_ref()
        && let Err(error) = store.mark_seen(&ids)
    {
        tracing::warn!("cannot save news read state: {error}");
    }
    publish(s).await;
}
pub(super) async fn read(s: &Shared, view: View) {
    s.send(Response::NewsVolume(s.news.local.lock().await.volumes.clone()));
    read_view(s, view, true).await;
}
async fn read_view(s: &Shared, view: View, force: bool) {
    let now = chrono::Utc::now().timestamp();
    let days = view.filters.days.clamp(1, 90) as i64;
    let start = if days == 1 { tr_data::news_store::day_start(now) } else { now - days * 86400 };
    let query = Query {
        symbols: view.symbols.clone(),
        filters: view.filters.clone(),
        start,
        limit: tr_data::news_intelligence::MAX_ITEMS,
        ..Query::default()
    };
    // N1b: hold the store lock through publication, so no pre-seen snapshot
    // can be sent after a completed seen write. UI also overlays local actions
    // on replies which were already in flight before that write.
    let watched = s.news.watchlist.lock().await.clone();
    let mut store_guard = s.news.store.lock().await;
    if let Some(store) = store_guard.as_mut() {
        let _ = store.set_watched(&watched);
        // Optional, read-only local SEC map; failures never fail saved reads.
        let _ = store.refresh_names(&data_dir().join("edgar.sqlite3"));
    }
    let items = match store_guard.as_ref() {
        Some(store) => store.query(&query).map_err(|_| "News store unavailable".into()),
        None => Err("News store unavailable".into()),
    };
    let mut status = s.news.status.lock().await.clone();
    if status.is_empty() {
        status = "Showing saved headlines".into();
    }
    if view.filters.scope == tr_data::news_store::Scope::Symbol
        && view.symbol.as_deref().is_some_and(|symbol| finnhub::symbol(symbol).is_some())
        && s.news.key.lock().await.as_deref().is_none_or(|key| key.trim().is_empty())
    {
        status.push_str(" · Company news needs a Finnhub key.");
    }
    if *s.news.memory_only.lock().await {
        status = format!("Saved headlines unavailable · {status}");
    }
    let (rows_changed, status_changed) = changed(&mut *s.news.published.lock().await, &view, &items, &status, force);
    let updated = *s.news.updated.lock().await;
    if rows_changed {
        s.send(Response::NewsSaved { view, items, status, updated });
    } else if status_changed {
        // Status transitions (pause/offline/recovery) are separate from item replies.
        // No timestamp-only heartbeat or unchanged list is sent on the five-second tick.
        s.send(Response::NewsStatus { view, status, updated });
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
            let time = crate::feed_time::feed_time(*until, chrono::Utc::now().timestamp(), false).0;
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
            let provider = if item.publisher.starts_with("CoinDesk") { "CoinDesk" } else { "Yahoo" };
            // RSS appends the author after the publisher. Authors are not sources.
            let publisher = item.publisher.split(" · ").next().unwrap_or_default().trim();
            let source = if publisher.is_empty() { provider.to_string() } else { publisher.to_string() };
            let mut story = Story::new(item, &source);
            story.provider = provider.into();
            story.category = if provider == "CoinDesk" {
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
        if let Err(error) = local_tick(&s).await {
            tracing::warn!("Local news notifications unavailable: {error}");
        }
        let watched = s.news.watchlist.lock().await.clone();
        if let Some(store) = s.news.store.lock().await.as_mut() {
            let _ = store.set_watched(&watched);
            let _ = store.refresh_names(&data_dir().join("edgar.sqlite3"));
        }
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
                            let initial = target
                                .as_deref()
                                .map(|t| store.latest(t).ok().flatten().is_none())
                                .unwrap_or_else(|| store.meta("notification:fallback-market").ok().flatten().is_none());
                            if initial && store.suppress_import(&stories).is_err() {
                                status = "News store write failed — showing saved headlines".into();
                                continue;
                            }
                            if store.insert(&stories, now).is_err() {
                                status = "News store write failed — showing saved headlines".into();
                            } else {
                                if target.is_none() {
                                    let _ = store.set_meta("notification:fallback-market", "1");
                                }
                                status = "Yahoo / CoinDesk · updated just now".into();
                                *s.news.updated.lock().await = Some(Instant::now());
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
                let saved = lock.as_mut().is_some_and(|store| {
                    let initial = match &job {
                        Job::Market(_) => cursor == 0,
                        Job::Company(_) => latest.is_none(),
                    };
                    if initial && store.suppress_import(&items).is_err() {
                        return false;
                    }
                    match job {
                        Job::Market(category) => finnhub::save_market(store, category, &items, now).is_ok(),
                        Job::Company(_) => store.insert(&items, now).is_ok(),
                    }
                });
                if saved {
                    *s.news.updated.lock().await = Some(Instant::now());
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
    fn n4_state() -> LocalState {
        LocalState {
            session: 100,
            since: [100; 5],
            settings: tr_data::news_store::local::Notifications {
                keywords: true,
                filings: true,
                insiders: true,
                breaking: true,
                ..Default::default()
            },
            watched: vec!["NVDA US Equity".into()],
            watches: vec![tr_data::news_intelligence::KeywordWatch {
                words: "earnings".into(),
                notify: true,
                ..Default::default()
            }],
            ..Default::default()
        }
    }
    fn n4_story(id: &str, ts: i64, source: &str) -> Story {
        let mut s = Story::new(
            tr_data::NewsItem {
                title: "NVDA earnings record revenue".into(),
                link: format!("https://fixture.invalid/{id}"),
                ts,
                publisher: source.into(),
                tickers: vec!["NVDA US Equity".into()],
            },
            source,
        );
        s.subjects = vec!["NVDA US Equity".into()];
        s
    }
    #[test]
    fn n4_notify_toggle_and_type_switches_never_retag_backlog() {
        let store = NewsStore::memory().unwrap();
        let mut state = n4_state();
        state.watches[0].notify = false;
        let a = n4_story("a", 150, "Fixture");
        assert!(state.news_alerts(&store, std::slice::from_ref(&a), 200).unwrap().is_empty());
        state.watches[0].notify = true;
        assert!(state.news_alerts(&store, &[a], 200).unwrap().is_empty());
        let b = n4_story("b", 151, "Fixture");
        assert_eq!(state.news_alerts(&store, std::slice::from_ref(&b), 200).unwrap().len(), 1);
        assert!(n4_state().news_alerts(&store, &[b], 200).unwrap().is_empty());
        state.settings.keywords = false;
        let c = n4_story("c", 152, "Fixture");
        assert!(state.news_alerts(&store, std::slice::from_ref(&c), 200).unwrap().is_empty());
        state.settings.keywords = true;
        assert!(state.news_alerts(&store, &[c], 200).unwrap().is_empty());
        for ts in [99, 100, 201, -8000] {
            assert!(
                state.news_alerts(&store, &[n4_story(&format!("time{ts}"), ts, "Fixture")], 200).unwrap().is_empty()
            );
        }
    }
    #[test]
    fn n4_first_import_and_restart_do_not_alert_even_with_recent_dates() {
        let mut store = NewsStore::memory().unwrap();
        let state = n4_state();
        let initial = n4_story("initial", 150, "Fixture");
        store.insert(std::slice::from_ref(&initial), 200).unwrap();
        assert!(state.news_alerts(&store, &[initial], 200).unwrap().is_empty());
        let restarted = LocalState { session: 200, ..n4_state() };
        assert!(
            restarted
                .news_alerts(&store, &[n4_story("unseen-before-restart", 199, "Fixture")], 201)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn n4_restart_drops_pending_overflow_but_preserves_rate_reservations_and_history() {
        let store = NewsStore::memory().unwrap();
        store.deliver_notifications(&vec!["event".into(); 6], 100, false).unwrap();
        assert!(store.deliver_notifications(&["overflow".into()], 101, false).unwrap().0.is_empty());
        n4_state().begin_session(&store).unwrap();
        let (desktop, history) = store.deliver_notifications(&[], 700, false).unwrap();
        assert!(desktop.is_empty());
        assert_eq!(history.len(), 7);
        // Restarting does not manufacture capacity within the rolling window.
        let other = NewsStore::memory().unwrap();
        other.deliver_notifications(&vec!["event".into(); 6], 100, false).unwrap();
        n4_state().begin_session(&other).unwrap();
        assert!(other.deliver_notifications(&["fresh".into()], 101, false).unwrap().0.is_empty());
    }
    #[test]
    fn n4_breaking_requires_four_sources_watched_subject_and_one_hour() {
        let store = NewsStore::memory().unwrap();
        let mut state = n4_state();
        state.settings.keywords = false;
        let mut rows = (0..4).map(|i| n4_story(&i.to_string(), 150 + i, &format!("Source {i}"))).collect::<Vec<_>>();
        assert!(state.news_alerts(&store, &rows[..3], 200).unwrap().is_empty());
        assert_eq!(state.news_alerts(&store, &rows, 200).unwrap().len(), 1);
        rows.push(n4_story("earlier", 140, "Better lead"));
        assert!(state.news_alerts(&store, &rows, 200).unwrap().is_empty());
        let other = NewsStore::memory().unwrap();
        state.watched.clear();
        assert!(state.news_alerts(&other, &rows, 200).unwrap().is_empty());
        state.watched = vec!["NVDA US Equity".into()];
        let late = NewsStore::memory().unwrap();
        assert!(state.news_alerts(&late, &rows, 4000).unwrap().is_empty());
    }
    #[test]
    fn n4_filings_respect_watches_types_once_and_decoded_items() {
        let db = tr_edgar::store::Store::memory().unwrap();
        let mut state = n4_state();
        let mut f = tr_edgar::Filing {
            accession: "fresh".into(),
            cik: 1,
            ticker: "NVDA".into(),
            form: "8-K".into(),
            items: "2.02".into(),
            filed: 150,
            ..Default::default()
        };
        let messages = state.filing_alerts(&db, &[f.clone()], 200).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("Results of Operations"));
        assert!(state.filing_alerts(&db, &[f.clone()], 200).unwrap().is_empty());
        state.settings.filings = false;
        f.accession = "disabled".into();
        assert!(state.filing_alerts(&db, &[f.clone()], 200).unwrap().is_empty());
        state.settings.filings = true;
        assert!(state.filing_alerts(&db, &[f.clone()], 200).unwrap().is_empty());
        f.accession = "once".into();
        f.transient_symbol = "NVDA".into();
        assert!(state.filing_alerts(&db, &[f.clone()], 200).unwrap().is_empty());
        f.transient_symbol.clear();
        assert!(state.filing_alerts(&db, &[f.clone()], 200).unwrap().is_empty());
        f.accession = "old".into();
        f.filed = 99;
        assert!(state.filing_alerts(&db, &[f.clone()], 200).unwrap().is_empty());
        f.accession = "owner".into();
        f.filed = 150;
        f.reporting_owner = true;
        assert!(state.filing_alerts(&db, &[f], 200).unwrap().is_empty());
    }
    #[test]
    fn n4_insider_toggle_never_replays_and_pending_local_xml_can_complete() {
        let db = tr_edgar::store::Store::memory().unwrap();
        let mut state = n4_state();
        let mut f = tr_edgar::Filing {
            accession: "disabled-buy".into(),
            cik: 1,
            ticker: "NVDA".into(),
            form: "4".into(),
            filed: 150,
            insider_parsed: true,
            insiders: vec![tr_edgar::insider::Transaction {
                owner: tr_edgar::insider::Owner { cik: Some(1), ..Default::default() },
                code: "P".into(),
                date: "2026-10-27".into(),
                shares: Some(1000.),
                price: Some(100.),
                ..Default::default()
            }],
            ..Default::default()
        };
        state.settings.insiders = false;
        assert!(state.filing_alerts(&db, &[f.clone()], 200).unwrap().is_empty());
        state.settings.insiders = true;
        assert!(state.filing_alerts(&db, &[f.clone()], 200).unwrap().is_empty());
        f.accession = "pending-buy".into();
        f.insider_parsed = false;
        assert!(state.filing_alerts(&db, &[f.clone()], 200).unwrap().is_empty());
        f.insider_parsed = true;
        let messages = state.filing_alerts(&db, &[f.clone()], 200).unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("Insider buy"));
        assert!(state.filing_alerts(&db, &[f], 200).unwrap().is_empty());
    }
    #[test]
    fn n4_local_paths_cannot_construct_network_requests() {
        let source = include_str!("news.rs").split("struct Published").next().unwrap();
        for forbidden in ["tr_net::", "s.provider", "scheduler::Sec", "Guard::request", "reqwest::Client"] {
            assert!(!source.contains(forbidden), "{forbidden}");
        }
    }

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
