//! N1a: one visible-only news scheduler; panes query saved history, never the network.
use super::*;
use tr_data::{
    finnhub::{self, Job, Schedule},
    news_store::{Filters, NewsStore, Query, Story},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub pane: PaneId,
    pub generation: u64,
    pub symbol: Option<String>,
    pub filters: Filters,
    pub symbols: Vec<String>,
}
#[derive(Default)]
pub(super) struct Service {
    pub views: Mutex<Vec<View>>,
    pub watchlist: Mutex<Vec<String>>,
    pub store: Mutex<Option<NewsStore>>,
    refresh: Mutex<Vec<View>>,
    wake: tokio::sync::Notify,
    generations: Mutex<std::collections::BTreeMap<PaneId, u64>>,
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
    prune_at: i64,
    volumes: Vec<tr_data::news_store::local::Volume>,
    history: Option<Vec<String>>,
    delivery: u64,
    edgar: Option<tr_edgar::store::Store>,
    edgar_at: i64,
    volume_symbols: Vec<String>,
    names: Option<Vec<(u64, String, String)>>,
    names_dirty: bool,
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
            prune_at: 0,
            volumes: vec![],
            history: None,
            delivery: 0,
            edgar: None,
            edgar_at: 0,
            volume_symbols: vec![],
            names: None,
            names_dirty: false,
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
            if old != new || self.settings.enabled != settings.enabled {
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

async fn blocking<T: Send + 'static>(s: &Arc<Shared>, work: impl FnOnce(&Shared) -> T + Send + 'static) -> T {
    let s = Arc::clone(s);
    tokio::task::spawn_blocking(move || work(&s)).await.expect("news blocking worker panicked")
}

pub(super) async fn local_read(
    s: &Arc<Shared>,
    args: &tr_agent_tools::args::LiveArgs,
    symbols: &[String],
) -> Result<serde_json::Value, String> {
    use tr_data::news_store::local;
    let now = chrono::Utc::now().timestamp();
    let filing_symbols = if symbols.is_empty() { s.news.local.lock().await.watched.clone() } else { symbols.to_vec() };
    let filings = blocking(s, move |s| s.news.local.blocking_lock().saved_filings(now))
        .await
        .map_err(|_| "Saved filings unavailable")?;
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
            let items = blocking(s, move |s| {
                s.news
                    .store
                    .blocking_lock()
                    .as_ref()
                    .ok_or("Saved headlines unavailable")?
                    .query(&query)
                    .map_err(|_| "Saved headlines unavailable")
            })
            .await?;
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
/// Pending claims are committed only after the complete batch is handed to
/// delivery. Observation/suppression markers are separate from delivery claims.
struct Notice {
    body: String,
    edgar: bool,
    claims: Vec<(String, String)>,
}
impl Notice {
    fn news(body: String, kind: &str, id: &str) -> Self {
        Self { body, edgar: false, claims: vec![(kind.into(), id.into())] }
    }
    fn commit(&self, news: Option<&NewsStore>, edgar: Option<&tr_edgar::store::Store>) -> anyhow::Result<()> {
        for (kind, id) in &self.claims {
            if self.edgar {
                edgar.ok_or_else(|| anyhow::anyhow!("EDGAR claims unavailable"))?.claim_notification(kind, id)?;
            } else {
                news.ok_or_else(|| anyhow::anyhow!("News claims unavailable"))?.claim_notification(kind, id)?;
            }
        }
        Ok(())
    }
}
fn volume_symbols(watched: &[String], views: &[View]) -> Vec<String> {
    let mut symbols = watched.to_vec();
    symbols.extend(
        views.iter().filter(|v| v.filters.scope == tr_data::news_store::Scope::Symbol).filter_map(|v| v.symbol.clone()),
    );
    symbols.sort();
    symbols.dedup();
    symbols
}
fn tick_enabled(visible: bool, settings: &tr_data::news_store::local::Notifications) -> bool {
    visible
        || (settings.enabled
            && (settings.filings || settings.insiders || settings.keywords || settings.breaking || settings.volume))
}
async fn local_tick(s: &Arc<Shared>) -> anyhow::Result<()> {
    let visible = !s.news.views.lock().await.is_empty();
    if !tick_enabled(visible, &s.news.local.lock().await.settings) {
        return Ok(());
    }
    blocking(s, local_tick_blocking).await
}
fn local_tick_blocking(s: &Shared) -> anyhow::Result<()> {
    let mut state = {
        let mut local = s.news.local.blocking_lock();
        // Keep a cheap read/config projection available while the evaluator owns
        // connections and does SQL. Configuration may change during evaluation.
        let projection = LocalState {
            settings: local.settings.clone(),
            watches: local.watches.clone(),
            watched: local.watched.clone(),
            session: local.session,
            since: local.since,
            volumes: local.volumes.clone(),
            history: local.history.clone(),
            delivery: local.delivery,
            ..Default::default()
        };
        std::mem::replace(&mut *local, projection)
    };
    let result = evaluate_local(s, &mut state);
    let mut local = s.news.local.blocking_lock();
    state.settings = local.settings.clone();
    state.watches = local.watches.clone();
    state.watched = local.watched.clone();
    state.since = local.since;
    *local = state;
    result
}
fn evaluate_local(s: &Shared, state: &mut LocalState) -> anyhow::Result<()> {
    use chrono::Timelike;
    use tr_data::news_store::local;
    let now = chrono::Utc::now().timestamp();
    let views = s.news.views.blocking_lock().clone();
    let watched = s.news.watchlist.blocking_lock().clone();
    // No local mutex is held during news or EDGAR database work.
    let mut guard = s.news.store.blocking_lock();
    if let Some(store) = guard.as_mut() {
        let _ = store.set_watched(&watched);
    }
    let mut notices = vec![];
    // Accumulate errors, not early returns: already collected messages still go
    // through delivery even if EDGAR, volume aggregation, or a later row fails.
    let news_result = {
        (|| -> anyhow::Result<()> {
            let Some(store) = guard.as_ref() else { return Ok(()) };
            state.begin_session(store)?;
            if now - state.prune_at >= 86400 {
                store.prune(now)?;
                state.prune_at = now;
            }
            let rows = store.query(&Query {
                // Cluster identity must retain earlier members, not reset at 2h.
                start: now - 86400,
                end: now,
                limit: tr_data::news_intelligence::MAX_ITEMS,
                filters: Filters { scope: tr_data::news_store::Scope::All, ..Default::default() },
                ..Default::default()
            })?;
            state.collect_news(store, &rows, now, &mut notices)?;
            let symbols = volume_symbols(&state.watched, &views);
            if now - state.volume_at >= 300 || symbols != state.volume_symbols {
                let volumes = store.volume_anomalies(&symbols, now)?;
                state.volume_at = now;
                state.volume_symbols = symbols;
                for v in &volumes {
                    let id = format!("{}:{}", v.key, v.hour);
                    if state.settings.enabled
                        && state.settings.volume
                        && (v.key.starts_with("Topic: ") || state.watched.contains(&v.key))
                        && v.hour >= state.since[4].max(state.session)
                        && !store.notification_claimed("volume", &id)?
                    {
                        notices.push(Notice::news(v.message(), "volume", &id));
                    }
                }
                if state.volumes != volumes {
                    state.volumes = volumes.clone();
                    for view in &views {
                        s.send(Response::NewsVolume { pane: view.pane, volumes: volumes.clone() });
                    }
                }
            }
            Ok(())
        })()
    };
    drop(guard);
    let edgar_result = state.collect_edgar(now, &mut notices);
    let mut guard = s.news.store.blocking_lock();
    let store = guard.as_mut().ok_or_else(|| anyhow::anyhow!("News delivery unavailable"))?;
    let names_result = if state.names_dirty {
        let result = store.install_names(state.names.as_deref().unwrap_or_default());
        if result.is_ok() {
            state.names_dirty = false;
        }
        result
    } else {
        Ok(())
    };
    let quiet = !state.settings.enabled
        || local::quiet(state.settings.quiet_start, state.settings.quiet_end, chrono::Local::now().hour());
    let mut handed = false;
    let delivery_result = handoff(&notices, store, now, quiet, |desktop, history| {
        handed = true;
        if !desktop.is_empty() || state.history.as_ref() != Some(&history) {
            state.history = Some(history.clone());
            state.delivery += 1;
            s.send(Response::FeedNotices { delivery: state.delivery, desktop, history });
        }
        Ok(())
    });
    drop(guard);
    let mut claims_result = Ok(());
    if handed {
        for notice in notices.iter().filter(|n| n.edgar) {
            claims_result = claims_result.and(notice.commit(None, state.edgar.as_ref()));
        }
    }
    delivery_result.and(claims_result).and(news_result).and(edgar_result).and(names_result)
}
fn handoff(
    notices: &[Notice],
    store: &NewsStore,
    now: i64,
    quiet: bool,
    deliver: impl FnOnce(Vec<String>, Vec<String>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let messages = notices.iter().map(|n| n.body.clone()).collect::<Vec<_>>();
    let (desktop, history) = store.deliver_notifications(&messages, now, quiet)?;
    deliver(desktop, history)?;
    let mut claims = std::collections::BTreeMap::<&str, Vec<String>>::new();
    for notice in notices.iter().filter(|n| !n.edgar) {
        for (kind, id) in &notice.claims {
            claims.entry(kind).or_default().push(id.clone());
        }
    }
    for (kind, ids) in claims {
        store.claim_notifications(kind, &ids)?;
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
    fn ensure_edgar(&mut self) -> anyhow::Result<()> {
        let path = data_dir().join("edgar.sqlite3");
        if self.edgar.is_none() && path.exists() {
            self.edgar = Some(tr_edgar::store::Store::open(path)?);
        }
        Ok(())
    }
    fn saved_filings(&mut self, now: i64) -> anyhow::Result<Vec<tr_edgar::Filing>> {
        self.ensure_edgar()?;
        self.edgar.as_ref().map(|db| db.local_filings(now - 90 * 86400, now)).unwrap_or_else(|| Ok(vec![]))
    }
    fn collect_edgar(&mut self, now: i64, notices: &mut Vec<Notice>) -> anyhow::Result<()> {
        // At most one snapshot evaluation per minute, including disabled types.
        if now - self.edgar_at < 60 {
            return Ok(());
        }
        self.edgar_at = now;
        self.ensure_edgar()?;
        if let Some(db) = self.edgar.as_ref() {
            let names = db.companies()?.into_iter().map(|c| (c.cik, c.ticker, c.title)).collect::<Vec<_>>();
            if self.names.as_ref() != Some(&names) {
                self.names = Some(names);
                self.names_dirty = true;
            }
            self.collect_filings(db, &db.filings()?, now, notices)?;
        }
        Ok(())
    }
    #[cfg(test)]
    fn news_alerts(&self, store: &NewsStore, rows: &[Story], now: i64) -> anyhow::Result<Vec<String>> {
        let mut notices = vec![];
        self.collect_news(store, rows, now, &mut notices)?;
        let messages = notices.iter().map(|n| n.body.clone()).collect(); // test delivery sink
        for notice in notices {
            notice.commit(Some(store), None)?;
        }
        Ok(messages)
    }
    fn collect_news(
        &self,
        store: &NewsStore,
        rows: &[Story],
        now: i64,
        notices: &mut Vec<Notice>,
    ) -> anyhow::Result<()> {
        use tr_data::news_store::local;
        let claims = store.notification_claims(&rows.iter().map(|story| story.id.clone()).collect::<Vec<_>>())?;
        let claimed = |kind: &str, id: &str| claims.contains(&(kind.into(), id.into()));
        let start = rows.iter().map(|story| story.item.ts).min().unwrap_or(now).saturating_sub(86400);
        let end = rows.iter().map(|story| story.item.ts).max().unwrap_or(now).saturating_add(86400);
        let delivered = store.delivered_members(start, end)?;
        let mut observed = vec![];
        let mut propagated = vec![];
        for story in rows {
            if claimed("keyword", &story.id) || claimed("keyword-observed", &story.id) {
                continue;
            }
            // Pane highlighting intentionally matches titles only, not summaries.
            let eligible = self.settings.enabled
                && self.settings.keywords
                && local::fresh(story.item.ts, self.since[2], self.session, now)
                && self.watches.iter().any(|w| w.notify && tr_data::news_intelligence::watch_matches(story, &w.words));
            if eligible {
                notices.push(Notice::news(format!("Keyword watch · {}", story.item.title), "keyword", &story.id));
            } else {
                observed.push(story.id.clone());
            }
        }
        for c in tr_data::news_intelligence::clusters(rows) {
            if c.member_ids.iter().any(|id| claimed("breaking-member", id)) {
                // Propagate an existing cluster identity through merges and lead changes.
                propagated.extend(c.member_ids.clone());
                continue;
            }
            let sources: std::collections::BTreeSet<_> =
                c.members.iter().filter(|m| m.item.ts >= now - 3600).map(|m| &m.source).collect();
            let blocked = c.members.iter().any(|m| claimed("breaking-block", &m.id));
            if sources.len() >= 4
                && self.settings.enabled
                && self.settings.breaking
                && !blocked
                && c.subjects.iter().any(|t| self.watched.contains(t))
                && c.members.iter().all(|m| local::fresh(m.item.ts, self.since[3], self.session, now))
            {
                if delivered.iter().any(|old| tr_data::news_intelligence::same_cluster(old, &c.lead)) {
                    propagated.extend(c.member_ids.clone());
                    continue;
                }
                notices.push(Notice {
                    body: format!("Breaking · {} sources · {}", sources.len(), c.lead.item.title),
                    edgar: false,
                    claims: c.member_ids.iter().map(|id| ("breaking-member".into(), id.clone())).collect(),
                });
            }
        }
        store.claim_notifications("keyword-observed", &observed)?;
        store.claim_notifications("breaking-member", &propagated)?;
        Ok(())
    }
    #[cfg(test)]
    fn filing_alerts(
        &self,
        db: &tr_edgar::store::Store,
        filings: &[tr_edgar::Filing],
        now: i64,
    ) -> anyhow::Result<Vec<String>> {
        let mut notices = vec![];
        self.collect_filings(db, filings, now, &mut notices)?;
        let messages = notices.iter().map(|n| n.body.clone()).collect(); // test delivery sink
        for notice in notices {
            notice.commit(None, Some(db))?;
        }
        Ok(messages)
    }
    fn collect_filings(
        &self,
        db: &tr_edgar::store::Store,
        filings: &[tr_edgar::Filing],
        now: i64,
        notices: &mut Vec<Notice>,
    ) -> anyhow::Result<()> {
        use tr_data::news_store::local;
        // Include seeded/legacy accession claims when deriving restatement identity.
        let mut claimed_lots = std::collections::BTreeSet::new();
        for old in filings {
            if db.notification_claimed("insider", &old.accession)? {
                claimed_lots.extend(old.insiders.iter().map(|t| tr_edgar::insider::restatement_key(old, t)));
            }
        }
        for f in filings {
            if !f.transient_symbol.is_empty() {
                db.claim_notification("filing-observed", &f.accession)?;
                db.claim_notification("insider-observed", &f.accession)?;
                continue;
            }
            let family = tr_edgar::form_family(&f.form);
            if f.reporting_owner || (family == "8-K" && !f.from_submissions && f.items.is_empty()) {
                continue;
            }
            let watched = self.watched.iter().find(|s| f.matches_symbol(s));
            if !db.notification_claimed("filing", &f.accession)?
                && !db.notification_claimed("filing-observed", &f.accession)?
            {
                if self.settings.enabled
                    && self.settings.filings
                    && watched.is_some()
                    && local::fresh(f.time(), self.since[0], self.session, now)
                    && matches!(family.as_str(), "8-K" | "10-K" | "10-Q" | "SC 13D" | "SC 13G")
                {
                    notices.push(Notice {
                        body: format!("{} · {}", f.ticker, f.detail()),
                        edgar: true,
                        claims: vec![("filing".into(), f.accession.clone())],
                    });
                } else {
                    db.claim_notification("filing-observed", &f.accession)?;
                }
            }
            // Pending XML must remain eligible until its parse completes.
            if !f.insider_parsed
                || db.notification_claimed("insider", &f.accession)?
                || db.notification_claimed("insider-observed", &f.accession)?
            {
                continue;
            }
            let keys: std::collections::BTreeSet<_> = f
                .insiders
                .iter()
                .filter(|t| t.code == "P" && !t.derivative)
                .map(|t| tr_edgar::insider::restatement_key(f, t))
                .collect();
            let mut superseded_claimed = false;
            for key in &keys {
                superseded_claimed |= db.notification_claimed("insider-lot", key)?
                    || notices.iter().any(|n| n.edgar && n.claims.contains(&("insider-lot".into(), key.clone())));
                superseded_claimed |= claimed_lots.contains(key);
            }
            let buy = watched.and_then(|symbol| tr_edgar::insider_buy(filings, f, symbol, self.settings.insider_min));
            if self.settings.enabled
                && self.settings.insiders
                && !superseded_claimed
                && local::fresh(f.time(), self.since[1], self.session, now)
                && let Some((value, cluster)) = buy
            {
                let mut claims = vec![("insider".into(), f.accession.clone())];
                claims.extend(keys.into_iter().map(|key| ("insider-lot".into(), key)));
                notices.push(Notice {
                    body: format!(
                        "{} · Insider buy {}{}",
                        f.ticker,
                        tr_edgar::insider::value(value),
                        if cluster { " · Cluster buy" } else { "" }
                    ),
                    edgar: true,
                    claims,
                });
            } else {
                db.claim_notification("insider-observed", &f.accession)?;
            }
        }
        Ok(())
    }
}

struct Published {
    view: View,
    items: Result<Vec<Story>, String>,
    status: String,
    updated: Option<Instant>,
}
fn changed(
    published: &mut Vec<Published>,
    view: &View,
    items: &Result<Vec<Story>, String>,
    status: &str,
    force: bool,
) -> (bool, bool) {
    if published.iter().any(|old| old.view.pane == view.pane && old.view.generation > view.generation) {
        return (false, false);
    }
    let old = published.iter().find(|old| old.view == *view);
    let updated = old.and_then(|old| old.updated);
    let rows_changed = force || old.is_none_or(|old| old.items != *items);
    let status_changed = old.is_none_or(|old| old.status != status);
    published.retain(|old| old.view.pane != view.pane);
    published.push(Published { view: view.clone(), items: items.clone(), status: status.into(), updated });
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
pub(super) async fn mark_seen(s: &Arc<Shared>, ids: Vec<String>) {
    blocking(s, move |s| {
        if let Some(store) = s.news.store.blocking_lock().as_ref()
            && let Err(error) = store.mark_seen(&ids)
        {
            tracing::warn!("cannot save news read state: {error}");
        }
    })
    .await;
    publish(s).await;
}
pub(super) async fn bookmark(s: &Arc<Shared>, ids: Vec<String>, saved: bool) {
    let write_ids = ids.clone();
    let result = blocking(s, move |s| -> anyhow::Result<()> {
        s.news
            .store
            .blocking_lock()
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("News store unavailable"))?
            .set_bookmark(&write_ids, saved)
    })
    .await;
    let failed = result.is_err();
    s.send(Response::NewsBookmark {
        ids,
        saved,
        error: result.err().map(|error| format!("Cannot save story: {error}")),
    });
    if !failed {
        publish(s).await;
    }
}
pub(super) async fn refresh(s: &Arc<Shared>, view: View) {
    read(s, view.clone()).await;
    source_status(
        s,
        std::slice::from_ref(&view),
        "Refresh requested — normal guard applies; pause/cooldown/budgets remain in force".into(),
        false,
    )
    .await;
    s.news.refresh.lock().await.push(view);
    s.news.wake.notify_one();
}
pub(super) async fn read(s: &Arc<Shared>, view: View) {
    let volumes = s.news.local.lock().await.volumes.clone();
    s.send(Response::NewsVolume { pane: view.pane, volumes });
    read_view(s, view, true).await;
}
async fn read_view(s: &Arc<Shared>, view: View, force: bool) {
    {
        let mut generations = s.news.generations.lock().await;
        let latest = generations.entry(view.pane).or_default();
        if view.generation < *latest {
            return;
        }
        *latest = view.generation;
    }
    blocking(s, move |s| read_view_blocking(s, view, force)).await;
}
fn read_view_blocking(s: &Shared, view: View, force: bool) {
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
    let watched = s.news.watchlist.blocking_lock().clone();
    let mut store_guard = s.news.store.blocking_lock();
    if let Some(store) = store_guard.as_mut() {
        let _ = store.set_watched(&watched);
        // Company names arrive from the minute-paced local EDGAR reader; no
        // EDGAR connection or queries are made under this store mutex.
    }
    let items = match store_guard.as_ref() {
        Some(store) => store.query(&query).map_err(|_| "News store unavailable".into()),
        None => Err("News store unavailable".into()),
    };
    let generations = s.news.generations.blocking_lock();
    if generations.get(&view.pane).is_some_and(|latest| *latest > view.generation) {
        return;
    }
    let mut published = s.news.published.blocking_lock();
    let old = published.iter().find(|old| {
        old.view.pane == view.pane
            && old.view.symbol == view.symbol
            && old.view.filters.scope == view.filters.scope
            && old.view.symbols == view.symbols
    });
    let mut status = old.map(|old| old.status.clone()).unwrap_or_else(|| "Showing saved headlines".into());
    let updated = old.and_then(|old| old.updated);
    status = status
        .trim_start_matches("Saved headlines unavailable · ")
        .split(" · Company news needs a Finnhub key.")
        .next()
        .unwrap_or_default()
        .to_string();
    if view.filters.scope == tr_data::news_store::Scope::Symbol
        && view.symbol.as_deref().is_some_and(|symbol| finnhub::symbol(symbol).is_some())
        && s.news.key.blocking_lock().as_deref().is_none_or(|key| key.trim().is_empty())
    {
        status.push_str(" · Company news needs a Finnhub key.");
    }
    if *s.news.memory_only.blocking_lock() {
        status = format!("Saved headlines unavailable · {status}");
    }
    let (rows_changed, status_changed) = changed(&mut published, &view, &items, &status, force);
    if let Some(record) = published.iter_mut().find(|old| old.view == view) {
        record.updated = updated;
    }
    if rows_changed {
        s.send(Response::NewsSaved { view, items, status, updated });
    } else if status_changed {
        // Status transitions (pause/offline/recovery) are separate from item replies.
        // No timestamp-only heartbeat or unchanged list is sent on the five-second tick.
        s.send(Response::NewsStatus { view, status, updated });
    }
}
pub(super) async fn publish(s: &Arc<Shared>) {
    let views = s.news.views.lock().await.clone();
    for view in views {
        read_view(s, view, false).await;
    }
}
async fn source_status(s: &Shared, views: &[View], status: String, success: bool) {
    let mut published = s.news.published.lock().await;
    for record in published.iter_mut().filter(|record| views.contains(&record.view)) {
        let scope = record.view.symbol.as_deref().unwrap_or(match record.view.filters.scope {
            tr_data::news_store::Scope::Watchlist => "Watchlist",
            _ => "Market",
        });
        record.status = format!("{status} · {scope}");
        if success {
            record.updated = Some(Instant::now());
        }
        s.send(Response::NewsStatus {
            view: record.view.clone(),
            status: record.status.clone(),
            updated: record.updated,
        });
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
#[derive(Default)]
struct WarningThrottle(std::collections::BTreeMap<String, i64>);
impl WarningThrottle {
    fn allow(&mut self, error: &str, now: i64) -> bool {
        self.0.retain(|_, at| now - *at < 600);
        if self.0.contains_key(error) {
            false
        } else {
            self.0.insert(error.into(), now);
            true
        }
    }
}
pub(super) async fn run(s: Arc<Shared>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let client = finnhub::Client::default();
    let mut schedule = Schedule::default();
    let mut fallback = FallbackState::default();
    let mut warnings = WarningThrottle::default();
    loop {
        tokio::select! { _ = interval.tick() => {}, _ = s.news.wake.notified() => {} }
        let refreshes = std::mem::take(&mut *s.news.refresh.lock().await);
        for view in refreshes {
            if schedule.backoff_until > chrono::Utc::now().timestamp() {
                let until =
                    crate::feed_time::feed_time(schedule.backoff_until, chrono::Utc::now().timestamp(), false).0;
                source_status(
                    &s,
                    std::slice::from_ref(&view),
                    format!("Refresh deferred · Finnhub paused until {until} — guard cooldown/budgets unchanged"),
                    false,
                )
                .await;
            }
            schedule.refresh(
                &view.symbols,
                view.filters.scope == tr_data::news_store::Scope::All
                    || (view.filters.scope == tr_data::news_store::Scope::Symbol && view.symbol.is_none()),
            );
            let target = if view.filters.scope == tr_data::news_store::Scope::All { None } else { view.symbol.clone() };
            fallback.pane_due.remove(&target);
            for symbol in &view.symbols {
                fallback.watch_due.remove(symbol);
            }
            fallback.fallback_due = 0;
        }
        if let Err(error) = local_tick(&s).await {
            let message = format!("{error:#}");
            if warnings.allow(&message, chrono::Utc::now().timestamp()) {
                tracing::warn!("Local news notifications unavailable: {message}");
            }
        }
        let views = s.news.views.lock().await.clone();
        if views.is_empty() {
            continue;
        }
        let now = chrono::Utc::now().timestamp();
        let key = s.news.key.lock().await.clone();
        let guard = match blocking(&s, |_| tr_net::shared()).await {
            Ok(guard) => guard,
            Err(error) => {
                if now >= schedule.backoff_until {
                    schedule.refused(now, &error);
                    source_status(&s, &views, refusal_status(&error), false).await;
                    publish(&s).await;
                }
                continue;
            }
        };
        let enabled = blocking(&s, move |_| {
            guard
                .status()
                .ok()
                .and_then(|rows| rows.into_iter().find(|r| r.group == "finnhub"))
                .is_some_and(|r| r.enabled)
        })
        .await;
        let fallback_all = use_fallback(enabled, key.as_deref());
        let routed = fallback_views(&views, fallback_all);
        let targets = fallback_targets(&routed, now, &mut fallback);
        if !targets.is_empty() {
            for target in targets {
                let affected: Vec<_> = views
                    .iter()
                    .filter(|view| match view.filters.scope {
                        tr_data::news_store::Scope::All => target.is_none(),
                        tr_data::news_store::Scope::Symbol if view.symbol.is_none() => target.is_none(),
                        _ => target.as_ref().is_some_and(|target| view.symbols.contains(target)),
                    })
                    .cloned()
                    .collect();
                let mut success = false;
                if s.news.views.lock().await.is_empty() {
                    break;
                }
                let status = match s.provider.news(target.as_deref()).await {
                    Ok(items) => {
                        let stories = fallback_items(items, target.as_deref());
                        success = blocking(&s, move |s| -> anyhow::Result<()> {
                            let mut guard = s.news.store.blocking_lock();
                            let store = guard.as_mut().ok_or_else(|| anyhow::anyhow!("News store unavailable"))?;
                            let initial = match target.as_deref() {
                                Some(t) => store.latest(t)?.is_none(),
                                None => store.meta("notification:fallback-market")?.is_none(),
                            };
                            if initial {
                                store.suppress_import(&stories)?;
                            }
                            store.insert(&stories, now)?;
                            store.prune(now)?;
                            if target.is_none() {
                                store.set_meta("notification:fallback-market", "1")?;
                            }
                            Ok(())
                        })
                        .await
                        .is_ok();
                        if success {
                            "Yahoo / CoinDesk · updated just now"
                        } else {
                            "News store write failed — showing saved headlines"
                        }
                        .into()
                    }
                    Err(error) => format!("Yahoo / CoinDesk unavailable · {error} — showing saved headlines"),
                };
                source_status(&s, &affected, status, success).await;
            }
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
        let bounds_job = job.clone();
        let saved_bounds = blocking(&s, move |s| bounds(s.news.store.blocking_lock().as_ref(), &bounds_job)).await;
        let affected: Vec<_> = views
            .iter()
            .filter(|view| match &job {
                Job::Market(category) => {
                    view.filters.scope == tr_data::news_store::Scope::All
                        || (*category == "general"
                            && view.filters.scope == tr_data::news_store::Scope::Symbol
                            && view.symbol.is_none())
                }
                Job::Company(symbol) => view.symbols.contains(symbol),
            })
            .cloned()
            .collect();
        let Ok((latest, cursor)) = saved_bounds else {
            // A broken store/cursor must not silently become a fresh history request.
            schedule.refused(now, &tr_net::Refused::Ledger);
            source_status(&s, &affected, "News cursor unavailable — showing saved headlines".into(), false).await;
            publish(&s).await;
            continue;
        };
        let result = match &job {
            Job::Company(symbol) => client.company(guard, token, symbol, latest, now).await,
            Job::Market(category) => client.market(guard, token, category, cursor).await,
        };
        let mut success = false;
        let status = match result {
            Ok(items) => {
                let saved = blocking(&s, move |s| {
                    s.news.store.blocking_lock().as_mut().is_some_and(|store| {
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
                    })
                })
                .await;
                if saved {
                    success = true;
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
        source_status(&s, &affected, status, success).await;
        publish(&s).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_shared() -> (Arc<Shared>, mpsc::Receiver<Response>) {
        super::super::tests::backfill_shared(Arc::new(tr_data::sample::SampleProvider::at(1_790_000_000, 100)))
    }
    #[test]
    fn n5_tick_is_gated_on_visibility_or_enabled_notification_types() {
        let mut settings = tr_data::news_store::local::Notifications::default();
        assert!(!tick_enabled(false, &settings));
        assert!(tick_enabled(true, &settings));
        settings.enabled = false;
        settings.keywords = true;
        assert!(!tick_enabled(false, &settings));
        settings.enabled = true;
        assert!(tick_enabled(false, &settings));
        for kind in 0..5 {
            let mut settings = tr_data::news_store::local::Notifications { enabled: true, ..Default::default() };
            match kind {
                0 => settings.filings = true,
                1 => settings.insiders = true,
                2 => settings.keywords = true,
                3 => settings.breaking = true,
                _ => settings.volume = true,
            }
            assert!(tick_enabled(false, &settings));
        }
    }
    #[tokio::test]
    async fn n5_hidden_disabled_tick_does_not_open_stores_and_blocking_work_leaves_runtime() {
        let (s, _) = fixture_shared();
        local_tick(&s).await.unwrap(); // No store: this can succeed only if gated.
        let runtime_thread = std::thread::current().id();
        let worker_thread = blocking(&s, |_| std::thread::current().id()).await;
        assert_ne!(runtime_thread, worker_thread);
    }
    #[tokio::test]
    async fn n5_status_update_time_and_volume_are_per_pane_and_old_generations_drop() {
        use tr_data::news_store::Scope;
        let (s, rx) = fixture_shared();
        *s.news.store.lock().await = Some(NewsStore::memory().unwrap());
        let mut a = view(Scope::Symbol, &["AAPL US Equity"]);
        let mut b = view(Scope::Symbol, &["MSFT US Equity"]);
        a.generation = 2;
        b.pane = 2;
        read(&s, a.clone()).await;
        read(&s, b.clone()).await;
        source_status(&s, std::slice::from_ref(&a), "A source failed".into(), false).await;
        source_status(&s, std::slice::from_ref(&b), "B updated".into(), true).await;
        {
            let records = s.news.published.lock().await;
            let a = records.iter().find(|r| r.view.pane == 1).unwrap();
            let b = records.iter().find(|r| r.view.pane == 2).unwrap();
            assert!(a.status.contains("A source failed") && a.status.contains("AAPL"));
            assert!(a.updated.is_none());
            assert!(b.status.contains("B updated") && b.status.contains("MSFT"));
            assert!(b.updated.is_some());
        }
        let replies = rx.try_iter().collect::<Vec<_>>();
        assert_eq!(replies.iter().filter(|r| matches!(r, Response::NewsVolume { pane: 1, .. })).count(), 1);
        assert_eq!(replies.iter().filter(|r| matches!(r, Response::NewsVolume { pane: 2, .. })).count(), 1);
        assert!(
            !replies.iter().any(|r| matches!(r, Response::FeedNotices { .. })),
            "reads never replay desktop notices"
        );
        a.generation = 1;
        read_view(&s, a, true).await;
        assert!(rx.try_recv().is_err());
        assert_eq!(s.news.published.lock().await.iter().find(|r| r.view.pane == 1).unwrap().view.generation, 2);
        b.generation = 2;
        refresh(&s, b.clone()).await;
        assert_eq!(*s.news.refresh.lock().await, vec![b]);
    }
    #[test]
    fn n5_tick_batches_story_claims_and_cluster_lookup() {
        let store = NewsStore::memory().unwrap();
        let mut state = n4_state();
        state.settings.breaking = false;
        let rows = (0..2000).map(|i| n4_story(&format!("batch-{i}"), 150, "Fixture")).collect::<Vec<_>>();
        let mut notices = vec![];
        state.collect_news(&store, &rows, 200, &mut notices).unwrap();
        assert_eq!(notices.len(), 2000);
        handoff(&notices, &store, 200, false, |desktop, history| {
            assert_eq!(desktop.len(), 6);
            assert_eq!(history.len(), 200);
            Ok(())
        })
        .unwrap();
        assert!(state.news_alerts(&store, &rows, 201).unwrap().is_empty());
        let source = include_str!("news.rs");
        let collect = source.split("    fn collect_news(").nth(1).unwrap().split("    #[cfg(test)]").next().unwrap();
        for per_story in
            ["notification_claimed(", "claim_notification(", "cluster_previously_delivered(", "notification_blocked("]
        {
            assert!(!collect.contains(per_story), "{per_story}");
        }
    }
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
    fn n4_partial_failure_hands_off_collected_messages_before_returning_error() {
        let store = NewsStore::memory().unwrap();
        let state = n4_state();
        let story = n4_story("partial", 150, "Fixture");
        let mut notices = vec![];
        state.collect_news(&store, std::slice::from_ref(&story), 200, &mut notices).unwrap();
        assert!(!store.notification_claimed("keyword", &story.id).unwrap());
        // A later EDGAR/volume step failed after news collection.
        let later: anyhow::Result<()> = Err(anyhow::anyhow!("later step failed"));
        let mut delivered = vec![];
        let result = handoff(&notices, &store, 200, false, |desktop, history| {
            assert_eq!(desktop.len(), 1);
            delivered = history;
            Ok(())
        })
        .and(later);
        assert!(result.is_err());
        assert_eq!(delivered.len(), 1);
        assert!(store.notification_claimed("keyword", &story.id).unwrap());
        assert!(state.news_alerts(&store, &[story], 201).unwrap().is_empty());
    }
    #[test]
    fn n4_rejected_delivery_leaves_candidates_unclaimed_and_retryable() {
        let store = NewsStore::memory().unwrap();
        let story = n4_story("rejected", 150, "Fixture");
        let state = n4_state();
        let mut notices = vec![];
        state.collect_news(&store, std::slice::from_ref(&story), 200, &mut notices).unwrap();
        assert!(handoff(&notices, &store, 200, false, |_, _| Err(anyhow::anyhow!("sink unavailable"))).is_err());
        assert!(!store.notification_claimed("keyword", &story.id).unwrap());
        assert_eq!(state.news_alerts(&store, &[story], 201).unwrap().len(), 1);
    }
    #[test]
    fn n4_keyword_scope_matches_pane_title_highlighting_not_summary() {
        let store = NewsStore::memory().unwrap();
        let state = n4_state();
        let mut story = n4_story("summary-only", 150, "Fixture");
        story.item.title = "NVDA launches new product".into();
        story.summary = "Earnings beat estimates".into();
        assert!(!tr_data::news_intelligence::watch_matches(&story, "earnings"));
        assert!(tr_data::news_intelligence::watch_highlights(&story.item.title, &state.watches).is_empty());
        assert!(state.news_alerts(&store, &[story], 200).unwrap().is_empty());
        let title = n4_story("title-hit", 150, "Fixture");
        assert!(!tr_data::news_intelligence::watch_highlights(&title.item.title, &state.watches).is_empty());
        assert_eq!(state.news_alerts(&store, &[title], 200).unwrap().len(), 1);
    }
    #[test]
    fn n4_master_switch_suppresses_all_feed_types_without_replaying() {
        let store = NewsStore::memory().unwrap();
        let db = tr_edgar::store::Store::memory().unwrap();
        let mut state = n4_state();
        state.settings.enabled = false;
        let story = n4_story("master", 150, "Fixture");
        let filing = review_buy("master-buy", "4", 150, 100.);
        assert!(state.news_alerts(&store, std::slice::from_ref(&story), 200).unwrap().is_empty());
        assert!(state.filing_alerts(&db, std::slice::from_ref(&filing), 200).unwrap().is_empty());
        state.settings.enabled = true;
        assert!(state.news_alerts(&store, &[story], 200).unwrap().is_empty());
        assert!(state.filing_alerts(&db, &[filing], 200).unwrap().is_empty());
    }
    #[test]
    fn n4_symbol_volume_interests_include_unwatched_linked_symbols_on_worker() {
        use tr_data::news_store::Scope;
        let watched = vec!["NVDA US Equity".into()];
        let views = [
            view(Scope::Symbol, &["AAPL US Equity"]),
            view(Scope::Symbol, &["AAPL US Equity"]),
            view(Scope::Watchlist, &["MSFT US Equity"]),
        ];
        assert_eq!(volume_symbols(&watched, &views), vec!["AAPL US Equity", "NVDA US Equity"]);
        assert_eq!(volume_symbols(&[], &views), vec!["AAPL US Equity"]);
    }
    #[test]
    fn n4_warning_throttle_is_per_error_and_ten_minutes() {
        let mut throttle = WarningThrottle::default();
        assert!(throttle.allow("locked", 100));
        assert!(!throttle.allow("locked", 699));
        assert!(throttle.allow("unavailable", 699));
        assert!(throttle.allow("locked", 700));
        assert!(!throttle.allow("unavailable", 701));
    }
    #[test]
    fn n4_breaking_saved_member_identity_survives_two_hours_and_snapshot_eviction() {
        let mut store = NewsStore::memory().unwrap();
        store.insert(&[n4_story("seed", 1, "Seed")], 1).unwrap();
        let mut state = n4_state();
        state.settings.keywords = false;
        let old = (0..4).map(|i| n4_story(&format!("old-{i}"), 150 + i, &format!("Source {i}"))).collect::<Vec<_>>();
        store.insert(&old, 200).unwrap();
        let old = store
            .query(&Query { limit: 100, ..Default::default() })
            .unwrap()
            .into_iter()
            .filter(|s| s.item.ts >= 100)
            .collect::<Vec<_>>();
        assert_eq!(state.news_alerts(&store, &old, 200).unwrap().len(), 1);
        let new = (0..4).map(|i| n4_story(&format!("new-{i}"), 8150 + i, &format!("Source {i}"))).collect::<Vec<_>>();
        // The new bounded snapshot has no old member IDs at all.
        assert!(state.news_alerts(&store, &new, 8200).unwrap().is_empty());
        assert!(new.iter().all(|s| store.notification_claimed("breaking-member", &s.id).unwrap()));
    }
    #[test]
    fn n4_edgar_evaluation_reuses_handle_and_runs_at_most_once_per_minute() {
        let mut state = n4_state();
        let mut db = tr_edgar::store::Store::memory().unwrap();
        db.upsert(&[review_buy("seed", "4", 1, 1.)]).unwrap();
        let buy = review_buy("minute-buy", "4", 150, 100.);
        db.upsert(std::slice::from_ref(&buy)).unwrap();
        db.save_insiders(
            &buy,
            &tr_edgar::insider::Document { issuer_cik: 1, transactions: buy.insiders.clone(), ..Default::default() },
        )
        .unwrap();
        state.edgar = Some(db);
        let mut pending = vec![];
        state.collect_edgar(200, &mut pending).unwrap();
        assert_eq!(pending.len(), 1);
        let next = review_buy("later-owner", "4", 210, 100.);
        state.edgar.as_mut().unwrap().upsert(std::slice::from_ref(&next)).unwrap();
        state
            .edgar
            .as_mut()
            .unwrap()
            .save_insiders(
                &next,
                &tr_edgar::insider::Document {
                    issuer_cik: 1,
                    transactions: next.insiders.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
        pending.clear();
        state.collect_edgar(205, &mut pending).unwrap();
        assert!(pending.is_empty());
        assert_eq!(state.edgar_at, 200);
        state.collect_edgar(260, &mut pending).unwrap();
        assert_eq!(state.edgar_at, 260);
        assert!(!pending.is_empty());
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
    fn review_buy(accession: &str, form: &str, filed: i64, price: f64) -> tr_edgar::Filing {
        tr_edgar::Filing {
            accession: accession.into(),
            cik: 1,
            ticker: "NVDA".into(),
            form: form.into(),
            filed,
            insider_parsed: true,
            insiders: vec![tr_edgar::insider::Transaction {
                owner: tr_edgar::insider::Owner { cik: Some(7), ..Default::default() },
                code: "P".into(),
                date: "2026-10-27".into(),
                shares: Some(2000.),
                price: Some(price),
                ..Default::default()
            }],
            ..Default::default()
        }
    }
    #[test]
    fn review_insider_amendment_is_not_double_notified() {
        let db = tr_edgar::store::Store::memory().unwrap();
        let state = n4_state();
        let original = review_buy("original", "4", 150, 100.);
        assert_eq!(state.filing_alerts(&db, std::slice::from_ref(&original), 200).unwrap().len(), 1);
        // Same owner/date/code/shares, corrected price, filed 10 s later.
        let amended = review_buy("amended", "4/A", 160, 101.);
        assert!(state.filing_alerts(&db, &[original, amended], 210).unwrap().is_empty());
    }
    #[test]
    fn review_schedule_13g_filings_alert_for_watched_issuers() {
        let db = tr_edgar::store::Store::memory().unwrap();
        let state = n4_state();
        let f = tr_edgar::Filing {
            accession: "schedule".into(),
            cik: 1,
            ticker: "NVDA".into(),
            form: "SCHEDULE 13G".into(),
            filed: 150,
            from_submissions: true,
            ..Default::default()
        };
        assert_eq!(state.filing_alerts(&db, &[f], 200).unwrap().len(), 1);
    }
    #[test]
    fn review_local_tick_does_not_reopen_and_reread_edgar_every_tick() {
        let source = include_str!("news.rs");
        let body = source.split("async fn local_tick").nth(1).unwrap().split("impl LocalState").next().unwrap();
        assert!(!body.contains("Store::open("), "edgar store opened on every 5 s tick");
        assert!(!body.contains("db.filings()"), "every windowed filing and the full companies map reread every 5 s");
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
            generation: 1,
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
