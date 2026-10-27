//! Durable, provider-independent headline history. No network access.
#[cfg(test)]
#[path = "news_store_tests.rs"]
mod tests;
use crate::NewsItem;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Story {
    pub id: String,
    pub item: NewsItem,
    pub source: String,
    pub provider_id: String,
    /// Transport provider is separate from the publisher used for story leads.
    #[serde(default)]
    pub provider: String,
    pub summary: String,
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub seen: bool,
}
impl Story {
    pub fn new(item: NewsItem, source: &str) -> Self {
        let link = normalize_link(&item.link);
        let identity = if link.is_empty() { format!("{source}|{}", item.title) } else { link.clone() };
        let tags = crate::news_intelligence::topic_tags(&item.title);
        Self {
            id: Sha1::digest(identity.as_bytes()).iter().map(|b| format!("{b:02x}")).collect(),
            item: NewsItem { link, ..item },
            source: source.into(),
            provider_id: String::new(),
            provider: source.into(),
            summary: String::new(),
            category: "general".into(),
            tags,
            seen: false,
        }
    }
}

/// Only safe web links survive; tracking does not change a story's identity.
pub fn normalize_link(link: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(link) else { return String::new() };
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return String::new();
    }
    url.set_fragment(None);
    let pairs: Vec<_> = url
        .query_pairs()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            !k.starts_with("utm_") && !matches!(k.as_str(), "guccounter" | "ncid")
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.set_query(None);
    if !pairs.is_empty() {
        url.query_pairs_mut().extend_pairs(pairs);
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(&path);
    // URL serializers retain the authority's root slash; identity does not (including ports).
    let text = url.to_string();
    match text.split_once('?') {
        Some((base, query)) => format!("{}?{query}", base.trim_end_matches('/')),
        None => text.trim_end_matches('/').to_string(),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    #[default]
    Symbol,
    Watchlist,
    All,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filters {
    pub scope: Scope,
    pub watchlist: Option<String>,
    pub sources: Vec<String>,
    pub include: String,
    pub exclude: String,
    pub search: String,
    pub days: u32,
}
impl Default for Filters {
    fn default() -> Self {
        Self {
            scope: Scope::Symbol,
            watchlist: None,
            sources: vec![],
            include: String::new(),
            exclude: String::new(),
            search: String::new(),
            days: 7,
        }
    }
}
#[derive(Serialize, Deserialize)]
pub struct Query {
    pub symbols: Vec<String>,
    pub filters: Filters,
    pub start: i64,
    pub end: i64,
    pub limit: usize,
    pub offset: usize,
}
impl Default for Query {
    fn default() -> Self {
        Self { symbols: vec![], filters: Filters::default(), start: 0, end: i64::MAX, limit: 200, offset: 0 }
    }
}
/// Apply view filters to a retained snapshot without deleting it on an empty query.
pub fn matches(story: &Story, q: &Query) -> bool {
    matches_columns(
        &format!("{} {}", story.item.title, story.summary),
        &story.item.tickers,
        (&story.source, &story.provider),
        &story.category,
        story.item.ts,
        q,
    )
}
// N1b: SQL matching needs no identity hash, URL parsing or topic enrichment.
fn matches_columns(
    text: &str,
    tickers: &[String],
    provenance: (&str, &str),
    category: &str,
    ts: i64,
    q: &Query,
) -> bool {
    let f = &q.filters;
    ts >= q.start
        && ts <= q.end
        && !(f.scope == Scope::Watchlist && q.symbols.is_empty())
        && !(f.scope == Scope::Symbol && q.symbols.is_empty() && category != "general")
        && (q.symbols.is_empty() || q.symbols.iter().any(|key| tickers.contains(key)))
        && (f.sources.is_empty() || f.sources.iter().any(|s| s == provenance.0 || s == provenance.1))
        && (f.include.is_empty() || whole_word(text, &f.include))
        && (f.exclude.is_empty() || !whole_word(text, &f.exclude))
        && search_matches(text, &f.search)
}
/// Same token-prefix semantics in FTS, LIKE fallback and the retained view.
fn terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|s| !s.is_empty()).map(str::to_lowercase).collect()
}
fn search_matches(text: &str, search: &str) -> bool {
    if search.trim().is_empty() {
        return true;
    }
    let query = terms(search);
    let words = terms(text);
    !query.is_empty() && query.iter().all(|term| words.iter().any(|word| word.starts_with(term)))
}
/// Today means the user's local calendar day, including DST transitions.
pub fn day_start(now: i64) -> i64 {
    use chrono::TimeZone;
    let local = chrono::Local.timestamp_opt(now, 0).single().unwrap_or_else(chrono::Local::now);
    chrono::Local
        .from_local_datetime(&local.date_naive().and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map(|d| d.timestamp())
        .unwrap_or(now)
}
pub fn whole_word(text: &str, keywords: &str) -> bool {
    let words: Vec<_> =
        text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
    keywords
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .any(|k| words.iter().any(|w| w == &k.to_lowercase()))
}

pub struct NewsStore {
    db: Connection,
    fts: bool,
}
impl NewsStore {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }
    pub fn memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }
    fn init(db: Connection) -> Result<Self> {
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.create_scalar_function(
            "news_matches",
            7,
            rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| {
                let raw: String = ctx.get(6)?;
                let q: Query =
                    serde_json::from_str(&raw).map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?;
                let tickers: String = ctx.get(2)?;
                let provenance: String = ctx.get(3)?;
                let (source, provider) = provenance.split_once('\0').unwrap_or((&provenance, &provenance));
                let title: String = ctx.get(0)?;
                let summary: String = ctx.get(1)?;
                let category: String = ctx.get(4)?;
                Ok(matches_columns(
                    &format!("{title} {summary}"),
                    &serde_json::from_str::<Vec<String>>(&tickers).unwrap_or_default(),
                    (source, provider),
                    &category,
                    ctx.get(5)?,
                    &q,
                ))
            },
        )?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS items(id TEXT PRIMARY KEY, source TEXT NOT NULL, provider_id TEXT NOT NULL,
            url TEXT NOT NULL, headline TEXT NOT NULL, summary TEXT NOT NULL, published INTEGER NOT NULL,
            tickers TEXT NOT NULL, category TEXT NOT NULL, fetched INTEGER NOT NULL, seen INTEGER NOT NULL DEFAULT 0,
            bookmark INTEGER NOT NULL DEFAULT 0);
            CREATE INDEX IF NOT EXISTS news_published ON items(published DESC);
            CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        let columns: Vec<String> =
            db.prepare("PRAGMA table_info(items)")?.query_map([], |r| r.get(1))?.collect::<rusqlite::Result<_>>()?;
        if !columns.iter().any(|c| c == "tags") {
            db.execute("ALTER TABLE items ADD COLUMN tags TEXT NOT NULL DEFAULT '[]'", [])?;
        }
        if !columns.iter().any(|c| c == "intelligence") {
            db.execute("ALTER TABLE items ADD COLUMN intelligence INTEGER NOT NULL DEFAULT 0", [])?;
        }
        if !columns.iter().any(|c| c == "provider") {
            db.execute("ALTER TABLE items ADD COLUMN provider TEXT NOT NULL DEFAULT ''", [])?;
        }
        db.execute("CREATE INDEX IF NOT EXISTS news_backfill ON items(published DESC) WHERE intelligence=0", [])?;
        let fts = db
            .execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS news_fts USING fts5(id UNINDEXED, headline, summary);
            CREATE TRIGGER IF NOT EXISTS news_fts_insert AFTER INSERT ON items BEGIN
                INSERT INTO news_fts VALUES(new.id,new.headline,new.summary); END;
            CREATE TRIGGER IF NOT EXISTS news_fts_delete AFTER DELETE ON items BEGIN
                DELETE FROM news_fts WHERE id=old.id; END;
            INSERT INTO news_fts SELECT id,headline,summary FROM items WHERE id NOT IN (SELECT id FROM news_fts);",
            )
            .is_ok();
        let store = Self { db, fts };
        store.prune(chrono::Utc::now().timestamp())?;
        Ok(store)
    }
    pub fn prune(&self, now: i64) -> Result<usize> {
        Ok(self.db.execute("DELETE FROM items WHERE published < ?1 AND bookmark=0", [now - 90 * 86400])?)
    }
    pub fn insert(&mut self, stories: &[Story], now: i64) -> Result<()> {
        let tx = self.db.transaction()?;
        for story in stories {
            // N1a used the transport source for link-less identities. Keep that
            // fallback even though N1b displays the publisher separately.
            let identity_source = if story.provider.is_empty() { &story.source } else { &story.provider };
            let canonical = Story::new(story.item.clone(), identity_source);
            let id = canonical.id;
            tx.execute("INSERT OR IGNORE INTO items(id,source,provider_id,url,headline,summary,published,tickers,category,fetched,provider) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![id, story.source, story.provider_id, canonical.item.link, story.item.title, story.summary, story.item.ts, serde_json::to_string(&story.item.tickers)?, story.category, now, story.provider])?;
            let (raw, title): (String, String) =
                tx.query_row("SELECT tickers,headline FROM items WHERE id=?1", [&id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let mut tickers: Vec<String> = serde_json::from_str(&raw)?;
            tickers.extend(crate::news_intelligence::extract_tickers(&title, &[]));
            tickers.extend(crate::news_intelligence::extract_tickers(&story.item.title, &story.item.tickers));
            let tags = crate::news_intelligence::topic_tags(&title);
            tickers.sort();
            tickers.dedup();
            // A company/general duplicate must still reach the default market pane.
            // Preserve general membership if later fetched through a narrower endpoint.
            tx.execute(
                "UPDATE items SET tickers=?2, tags=?4, intelligence=1,
                provider=CASE WHEN provider='' THEN source ELSE provider END,
                source=CASE WHEN provider='' OR source=provider THEN ?5 ELSE source END, category=CASE
                WHEN category='general' OR ?3='general' THEN 'general'
                WHEN ?3 IN ('forex','crypto','merger') THEN ?3 ELSE category END WHERE id=?1",
                params![
                    id,
                    serde_json::to_string(&tickers)?,
                    story.category,
                    serde_json::to_string(&tags)?,
                    story.source
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self.db.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0)).optional()?)
    }
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO meta VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
    pub fn latest(&self, symbol: &str) -> Result<Option<i64>> {
        Ok(self.db.query_row(
            "SELECT MAX(published) FROM items,json_each(items.tickers) WHERE json_each.value=?1",
            [symbol],
            |r| r.get(0),
        )?)
    }
    pub fn mark_seen(&self, ids: &[String]) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        for id in ids {
            tx.execute("UPDATE items SET seen=1 WHERE id=?1", [id])?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Bounded lazy migration, executed only by the history worker, never by UI.
    fn backfill(&self, q: &Query) -> Result<()> {
        let mut stmt = self.db.prepare("SELECT id,headline,tickers FROM items WHERE intelligence=0 AND published>=?1 AND published<=?2 ORDER BY published DESC LIMIT 2000")?;
        let rows = stmt
            .query_map(params![q.start, q.end], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.is_empty() {
            return Ok(());
        }
        let tx = self.db.unchecked_transaction()?;
        for (id, title, raw) in rows {
            let tickers = crate::news_intelligence::extract_tickers(
                &title,
                &serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default(),
            );
            let tags = crate::news_intelligence::topic_tags(&title);
            tx.execute(
                "UPDATE items SET tickers=?2,tags=?3,intelligence=1,provider=CASE WHEN provider='' THEN source ELSE provider END WHERE id=?1",
                params![id, serde_json::to_string(&tickers)?, serde_json::to_string(&tags)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn query(&self, q: &Query) -> Result<Vec<Story>> {
        // Enrichment is optional: read-only disks and transient write failures
        // must never hide otherwise readable saved history.
        let _ = self.backfill(q);
        let mut sql = "SELECT id,source,provider_id,url,headline,summary,published,tickers,category,tags,seen,provider FROM items WHERE published>=?1 AND published<=?2".to_string();
        let search = if self.fts && !q.filters.search.trim().is_empty() {
            sql.push_str(" AND id IN (SELECT id FROM news_fts WHERE news_fts MATCH ?3)");
            let terms = terms(&q.filters.search);
            if terms.is_empty() {
                return Ok(vec![]);
            }
            terms.iter().map(|term| format!("\"{}\"*", term.replace('"', "\"\""))).collect::<Vec<_>>().join(" AND ")
        } else {
            sql.push_str(" AND (headline LIKE ?3 ESCAPE '\\' OR summary LIKE ?3 ESCAPE '\\')");
            // A broad LIKE candidate; news_matches applies identical per-term prefixes.
            let first = terms(&q.filters.search).into_iter().next().unwrap_or_default();
            format!("%{}%", first.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
        };
        sql.push_str(" AND news_matches(headline,summary,tickers,source||char(0)||provider,category,published,?4) ORDER BY published DESC,id LIMIT ?5 OFFSET ?6");
        let mut stmt = self.db.prepare(&sql)?;
        let rows = stmt.query_map(
            params![
                q.start,
                q.end,
                search,
                serde_json::to_string(q)?,
                q.limit.min(crate::news_intelligence::MAX_ITEMS) as i64,
                q.offset.min(i64::MAX as usize) as i64
            ],
            |r| {
                let raw: String = r.get(7)?;
                Ok(Story {
                    id: r.get(0)?,
                    source: r.get(1)?,
                    provider_id: r.get(2)?,
                    provider: r.get(11)?,
                    summary: r.get(5)?,
                    category: r.get(8)?,
                    tags: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
                    seen: r.get(10)?,
                    item: NewsItem {
                        link: r.get(3)?,
                        title: r.get(4)?,
                        publisher: r.get(1)?,
                        ts: r.get(6)?,
                        tickers: serde_json::from_str(&raw).unwrap_or_default(),
                    },
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}
