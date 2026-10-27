use crate::{Company, Filing};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, time::Duration};

pub struct Store {
    db: Connection,
    #[cfg(test)]
    read_queries: std::cell::Cell<usize>,
}
impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }
    pub fn memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }
    fn init(db: Connection) -> Result<Self> {
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS filings(accession TEXT PRIMARY KEY, cik INTEGER NOT NULL, ticker TEXT NOT NULL,
                form TEXT NOT NULL, filed INTEGER NOT NULL, accepted INTEGER, report_date TEXT NOT NULL, items TEXT NOT NULL,
                primary_doc TEXT NOT NULL, description TEXT NOT NULL, url TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS filings_symbol ON filings(ticker,filed);
            CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS companies(ticker TEXT PRIMARY KEY, cik INTEGER NOT NULL, title TEXT NOT NULL);")?;
        // Migrate the N2 store once: one accession may appear for an issuer and a reporting owner.
        db.execute_batch("BEGIN IMMEDIATE")?;
        let has_role = db
            .prepare("PRAGMA table_info(filings)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|c| c == "reporting_owner");
        if !has_role {
            db.execute_batch("ALTER TABLE filings RENAME TO filings_legacy;
                CREATE TABLE filings(accession TEXT NOT NULL, cik INTEGER NOT NULL, ticker TEXT NOT NULL,
                    form TEXT NOT NULL, filed INTEGER NOT NULL, accepted INTEGER, report_date TEXT NOT NULL, items TEXT NOT NULL,
                    primary_doc TEXT NOT NULL, description TEXT NOT NULL, url TEXT NOT NULL, reporting_owner INTEGER NOT NULL DEFAULT 0, from_submissions INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY(cik,accession,reporting_owner));
                INSERT INTO filings SELECT *,0,primary_doc!='' FROM filings_legacy;
                DROP TABLE filings_legacy;")?;
        }
        let has_provenance = db
            .prepare("PRAGMA table_info(filings)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|c| c == "from_submissions");
        if !has_provenance {
            db.execute_batch("ALTER TABLE filings ADD COLUMN from_submissions INTEGER NOT NULL DEFAULT 0; UPDATE filings SET from_submissions=1 WHERE primary_doc!='';")?;
        }
        db.execute_batch(
            "CREATE INDEX IF NOT EXISTS filings_cik_time ON filings(cik,coalesce(accepted,filed));
            DELETE FROM meta WHERE key LIKE 'body:%'; COMMIT;",
        )?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS insider_transactions(
            accession TEXT NOT NULL, seq INTEGER NOT NULL, cik INTEGER NOT NULL,
            owner TEXT NOT NULL, owner_cik INTEGER, role TEXT NOT NULL, code TEXT NOT NULL,
            date TEXT NOT NULL, shares REAL, price REAL, acquired INTEGER, owned_after REAL,
            direct INTEGER, derivative INTEGER NOT NULL, plan_10b5_1 INTEGER NOT NULL,
            owner_json TEXT NOT NULL, PRIMARY KEY(accession,seq));
            CREATE INDEX IF NOT EXISTS insider_cik_date ON insider_transactions(cik,date);
            CREATE TABLE IF NOT EXISTS insider_documents(accession TEXT PRIMARY KEY,cik INTEGER NOT NULL);",
        )?;
        let has_json = db
            .prepare("PRAGMA table_info(insider_transactions)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|c| c == "transaction_json");
        if !has_json {
            db.execute_batch("ALTER TABLE insider_transactions ADD COLUMN transaction_json TEXT;")?;
        }
        Ok(Self {
            db,
            #[cfg(test)]
            read_queries: Default::default(),
        })
    }
    fn read(&self, sql: &str) -> rusqlite::Result<rusqlite::Statement<'_>> {
        #[cfg(test)]
        self.read_queries.set(self.read_queries.get() + 1);
        self.db.prepare(sql)
    }
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.read("SELECT value FROM meta WHERE key=?1")?.query_row([key], |r| r.get(0)).optional()?)
    }
    pub fn number(&self, key: &str) -> Result<i64> {
        Ok(self.get(key)?.and_then(|s| s.parse().ok()).unwrap_or(0))
    }
    pub fn set(&self, key: &str, value: impl ToString) -> Result<()> {
        self.db.execute(
            "INSERT INTO meta VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value.to_string()],
        )?;
        Ok(())
    }
    pub fn remove(&self, key: &str) -> Result<()> {
        self.db.execute("DELETE FROM meta WHERE key=?1", [key])?;
        Ok(())
    }
    pub fn companies(&self) -> Result<Vec<Company>> {
        Ok(self
            .read("SELECT cik,ticker,title FROM companies ORDER BY ticker")?
            .query_map([], |r| Ok(Company { cik: r.get::<_, i64>(0)? as u64, ticker: r.get(1)?, title: r.get(2)? }))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn replace_companies(&mut self, companies: &[Company]) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM companies", [])?;
        for c in companies {
            tx.execute("INSERT INTO companies VALUES(?1,?2,?3)", params![c.ticker, c.cik as i64, c.title])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn contains(&self, acc: &str) -> Result<bool> {
        Ok(self.db.query_row("SELECT 1 FROM filings WHERE accession=?1", [acc], |_| Ok(())).optional()?.is_some())
    }
    /// A feed placeholder must never erase richer submissions metadata.
    pub fn upsert(&mut self, filings: &[Filing]) -> Result<()> {
        let tx = self.db.transaction()?;
        for f in filings {
            // Submissions do not label ownership roles. Preserve a known Reporting feed role
            // instead of manufacturing an Issuer row for the very same company/accession.
            let reporting_owner = if f.from_submissions && matches!(f.form.as_str(), "4" | "4/A") {
                tx.query_row("SELECT reporting_owner FROM filings WHERE cik=?1 AND accession=?2 ORDER BY reporting_owner LIMIT 1", params![f.cik as i64, f.accession], |r| r.get::<_,bool>(0)).optional()?.unwrap_or(f.reporting_owner)
            } else {
                f.reporting_owner
            };
            tx.execute(
                "INSERT INTO filings VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                ON CONFLICT(cik,accession,reporting_owner) DO UPDATE SET
                from_submissions=max(excluded.from_submissions,filings.from_submissions),
                filed=CASE WHEN excluded.from_submissions THEN excluded.filed ELSE filings.filed END,
                accepted=CASE WHEN excluded.from_submissions THEN coalesce(excluded.accepted,filings.accepted) ELSE coalesce(filings.accepted,excluded.accepted) END,
                report_date=CASE WHEN excluded.report_date!='' THEN excluded.report_date ELSE filings.report_date END,
                items=CASE WHEN excluded.items!='' THEN excluded.items ELSE filings.items END,
                primary_doc=CASE WHEN excluded.primary_doc!='' THEN excluded.primary_doc ELSE filings.primary_doc END,
                description=CASE WHEN excluded.primary_doc!='' THEN excluded.description ELSE filings.description END,
                url=CASE WHEN excluded.primary_doc!='' THEN excluded.url ELSE filings.url END",
                params![
                    f.accession,
                    f.cik as i64,
                    f.ticker,
                    f.form,
                    f.filed,
                    f.accepted,
                    f.report_date,
                    f.items,
                    f.primary_doc,
                    f.description,
                    f.url,
                    reporting_owner,
                    f.from_submissions
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Filing> {
        Ok(Filing {
            accession: r.get(0)?,
            cik: r.get::<_, i64>(1)? as u64,
            ticker: r.get(2)?,
            form: r.get(3)?,
            filed: r.get(4)?,
            accepted: r.get(5)?,
            report_date: r.get(6)?,
            items: r.get(7)?,
            primary_doc: r.get(8)?,
            description: r.get(9)?,
            url: r.get(10)?,
            reporting_owner: r.get(11)?,
            from_submissions: r.get(12)?,
            tickers: vec![],
            insiders: vec![],
            insider_unavailable: false,
            insider_parsed: false,
        })
    }
    fn with_tickers(&self, mut rows: Vec<Filing>) -> Result<Vec<Filing>> {
        use std::collections::BTreeMap;
        let mut tickers: BTreeMap<u64, Vec<String>> = BTreeMap::new();
        for c in self.companies()? {
            tickers.entry(c.cik).or_default().push(c.ticker);
        }
        let documents: BTreeMap<String, u64> = self
            .read("SELECT accession,cik FROM insider_documents")?
            .query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?
            .collect::<rusqlite::Result<_>>()?;
        let meta: BTreeMap<String, i64> = self
            .read("SELECT key,CAST(value AS INTEGER) FROM meta WHERE key LIKE 'f4:%'")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut lots: BTreeMap<(String, u64), Vec<crate::insider::Transaction>> = BTreeMap::new();
        // One batch query for the whole snapshot, including empty documents via the map above.
        let accessions = serde_json::to_string(&rows.iter().map(|f| f.accession.as_str()).collect::<Vec<_>>())?;
        let mut query = self.read("SELECT owner_json,code,date,shares,price,acquired,owned_after,direct,derivative,plan_10b5_1,transaction_json,accession,cik FROM insider_transactions WHERE accession IN (SELECT value FROM json_each(?1)) ORDER BY accession,seq")?;
        for entry in query.query_map([accessions], |r| {
            Ok((r.get::<_, String>(11)?, r.get::<_, i64>(12)? as u64, Self::transaction_row(r)?))
        })? {
            let (accession, cik, t) = entry?;
            lots.entry((accession, cik)).or_default().push(t);
        }
        for row in &mut rows {
            row.tickers = tickers.get(&row.cik).cloned().unwrap_or_default();
            row.insiders = lots.get(&(row.accession.clone(), row.cik)).cloned().unwrap_or_default();
            row.insider_parsed = documents.get(&row.accession) == Some(&row.cik);
            row.reporting_owner |= documents.get(&row.accession).is_some_and(|cik| *cik != row.cik);
            row.insider_unavailable = !row.insider_parsed
                && (meta.get(&format!("f4:attempts:{}", row.accession)).copied().unwrap_or(0) >= 3
                    || meta.contains_key(&format!("f4:terminal:{}", row.accession))
                    || (matches!(row.form.as_str(), "4" | "4/A")
                        && !row.primary_doc.is_empty()
                        && crate::insider::xml_url(row).is_none()));
        }
        Ok(rows)
    }
    /// Single SQL backlog query; never hydrate lots or scan the company list for candidates.
    pub fn insider_backlog(&self) -> Result<Vec<(Filing, i64)>> {
        Ok(self
            .read(
                "SELECT filings.*,CAST(coalesce(retry.value,'0') AS INTEGER) FROM filings
            LEFT JOIN meta retry ON retry.key='f4:retry:'||accession
            WHERE reporting_owner=0 AND form IN ('4','4/A') AND primary_doc!=''
            AND filed>=CAST(coalesce((SELECT value FROM meta WHERE key='window:'||filings.cik),'0') AS INTEGER)
            AND NOT EXISTS(SELECT 1 FROM insider_documents d WHERE d.accession=filings.accession)
            AND NOT EXISTS(SELECT 1 FROM meta WHERE key='f4:terminal:'||accession)
            AND CAST(coalesce((SELECT value FROM meta WHERE key='f4:attempts:'||accession),'0') AS INTEGER)<3
            ORDER BY coalesce(accepted,filed) DESC,accession DESC",
            )?
            .query_map([], |r| Ok((Self::row(r)?, r.get(13)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn filings(&self) -> Result<Vec<Filing>> {
        let rows = self.read("SELECT * FROM filings WHERE filed>=CAST(coalesce((SELECT value FROM meta WHERE key='window:'||filings.cik),'0') AS INTEGER) ORDER BY coalesce(accepted,filed) DESC,accession DESC")?.query_map([], Self::row)?.collect::<rusqlite::Result<_>>()?;
        self.with_tickers(rows)
    }
    /// Uses the CIK/time index; chart loads never scan the entire filings store.
    pub fn chart_filings(&self, symbol: &str, start: i64, end: i64) -> Result<Vec<Filing>> {
        let Some(ticker) = crate::ticker(symbol) else {
            return Ok(vec![]);
        };
        let cik: Option<i64> =
            self.db.query_row("SELECT cik FROM companies WHERE ticker=?1", [&ticker], |r| r.get(0)).optional()?;
        let Some(cik) = cik else {
            return Ok(vec![]);
        };
        let mut rows: Vec<Filing> = self.read("SELECT * FROM filings WHERE cik=?1 AND coalesce(accepted,filed) BETWEEN ?2 AND ?3 AND reporting_owner=0 AND filed>=CAST(coalesce((SELECT value FROM meta WHERE key='window:'||filings.cik),'0') AS INTEGER) ORDER BY coalesce(accepted,filed)")?.query_map(params![cik,start,end], Self::row)?.collect::<rusqlite::Result<_>>()?;
        // N3: trade dates can precede acceptance. Use the insider CIK/date index as a
        // second path rather than incorrectly restricting trades to filing-time range.
        let dates = chrono::DateTime::from_timestamp(start, 0).zip(chrono::DateTime::from_timestamp(end, 0));
        if let Some((a, b)) = dates {
            let accessions = self
                .db
                .prepare("SELECT DISTINCT accession FROM insider_transactions WHERE cik=?1 AND date BETWEEN ?2 AND ?3")?
                .query_map(params![cik, a.format("%Y-%m-%d").to_string(), b.format("%Y-%m-%d").to_string()], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for accession in accessions {
                if rows.iter().any(|f| f.accession == accession) {
                    continue;
                }
                if let Some(f) = self.db.query_row("SELECT * FROM filings WHERE cik=?1 AND accession=?2 AND reporting_owner=0 AND filed>=CAST(coalesce((SELECT value FROM meta WHERE key='window:'||filings.cik),'0') AS INTEGER)",params![cik,accession],Self::row).optional()? { rows.push(f); }
            }
        }
        self.with_tickers(rows)
    }
    /// An empty, successfully parsed form is complete too. Never refetch immutable forms.
    pub fn insider_complete(&self, accession: &str) -> Result<bool> {
        Ok(self
            .db
            .query_row("SELECT 1 FROM insider_documents WHERE accession=?1", [accession], |_| Ok(()))
            .optional()?
            .is_some())
    }
    pub fn save_insiders(&mut self, f: &Filing, document: &crate::insider::Document) -> Result<()> {
        // The fetching row may be a reporting owner. Persist once, under the XML issuer.
        let issuer_cik = document.issuer_cik;
        let tx = self.db.transaction()?;
        if tx
            .execute("INSERT OR IGNORE INTO insider_documents VALUES(?1,?2)", params![f.accession, issuer_cik as i64])?
            == 0
        {
            return Ok(());
        }
        tx.execute(
            "UPDATE OR IGNORE filings SET reporting_owner=1 WHERE accession=?1 AND cik!=?2",
            params![f.accession, issuer_cik as i64],
        )?;
        tx.execute(
            "DELETE FROM filings WHERE accession=?1 AND cik!=?2 AND reporting_owner=0",
            params![f.accession, issuer_cik as i64],
        )?;
        for (seq, t) in document.transactions.iter().enumerate() {
            tx.execute(
                "INSERT INTO insider_transactions VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                params![
                    f.accession,
                    seq as i64,
                    issuer_cik as i64,
                    t.owner.name,
                    t.owner.cik.map(|c| c as i64),
                    t.owner.role(),
                    t.code,
                    t.date,
                    t.shares,
                    t.price,
                    t.acquired,
                    t.owned_after,
                    t.direct,
                    t.derivative,
                    t.plan_10b5_1,
                    serde_json::to_string(&t.owner)?,
                    serde_json::to_string(t)?
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    fn transaction_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<crate::insider::Transaction> {
        if let Some(json) = r.get::<_, Option<String>>(10)? {
            return serde_json::from_str(&json)
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(10, rusqlite::types::Type::Text, Box::new(e)));
        }
        let owner: String = r.get(0)?;
        Ok(crate::insider::Transaction {
            owner: serde_json::from_str(&owner).unwrap_or_default(),
            code: r.get(1)?,
            date: r.get(2)?,
            shares: r.get(3)?,
            price: r.get(4)?,
            acquired: r.get(5)?,
            owned_after: r.get(6)?,
            direct: r.get(7)?,
            derivative: r.get(8)?,
            plan_10b5_1: r.get(9)?,
            ..Default::default()
        })
    }
    pub fn insider_transactions(&self, accession: &str) -> Result<Vec<crate::insider::Transaction>> {
        Ok(self.read("SELECT owner_json,code,date,shares,price,acquired,owned_after,direct,derivative,plan_10b5_1,transaction_json FROM insider_transactions WHERE accession=?1 ORDER BY seq")?
            .query_map([accession], Self::transaction_row)?.collect::<rusqlite::Result<_>>()?)
    }
    /// Date-indexed, store-only reader for analytics. Pane/chart rows also carry these lots.
    pub fn insiders_for_cik(&self, cik: u64, start: &str, end: &str) -> Result<Vec<crate::insider::Transaction>> {
        let accessions = self
            .read("SELECT DISTINCT accession FROM insider_transactions WHERE cik=?1 AND date BETWEEN ?2 AND ?3")?
            .query_map(params![cik as i64, start, end], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut rows = vec![];
        for accession in accessions {
            rows.extend(
                self.insider_transactions(&accession)?
                    .into_iter()
                    .filter(|t| t.date.as_str() >= start && t.date.as_str() <= end),
            );
        }
        Ok(rows)
    }
    pub fn enriched(&self, cik: u64, accession: &str) -> Result<bool> {
        Ok(self
            .db
            .query_row(
                "SELECT 1 FROM filings WHERE cik=?1 AND accession=?2 AND reporting_owner=0 AND from_submissions=1",
                params![cik as i64, accession],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
    pub fn prune(&self, now: i64) -> Result<usize> {
        let removed = self.db.execute("DELETE FROM filings WHERE filed<?1", [now - 90 * 86400])?;
        self.db.execute_batch("DELETE FROM insider_transactions WHERE NOT EXISTS(SELECT 1 FROM filings WHERE filings.accession=insider_transactions.accession);
            DELETE FROM insider_documents WHERE NOT EXISTS(SELECT 1 FROM filings WHERE filings.accession=insider_documents.accession);")?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chart_queries_use_cik_time_index_and_all_share_classes() {
        let mut store = Store::memory().unwrap();
        store
            .replace_companies(&[
                Company { cik: 1, ticker: "BRK-A".into(), title: String::new() },
                Company { cik: 1, ticker: "BRK-B".into(), title: String::new() },
                Company { cik: 2, ticker: "OTHER".into(), title: String::new() },
            ])
            .unwrap();
        store
            .upsert(&[
                Filing {
                    cik: 1,
                    ticker: "BRK-A".into(),
                    accession: "one".into(),
                    filed: 1,
                    accepted: Some(100),
                    form: "8-K".into(),
                    ..Default::default()
                },
                Filing {
                    cik: 2,
                    ticker: "OTHER".into(),
                    accession: "two".into(),
                    filed: 1,
                    accepted: Some(100),
                    form: "8-K".into(),
                    ..Default::default()
                },
            ])
            .unwrap();
        for symbol in ["BRK.A US Equity", "BRK-B US Equity"] {
            let rows = store.chart_filings(symbol, 90, 110).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].cik, 1);
            assert!(rows[0].matches_symbol(symbol));
            assert!(store.chart_filings(symbol, 0, 99).unwrap().is_empty());
        }
        let plans = store.db.prepare("EXPLAIN QUERY PLAN SELECT * FROM filings WHERE cik=?1 AND coalesce(accepted,filed) BETWEEN ?2 AND ?3 AND reporting_owner=0").unwrap().query_map(params![1,90,110],|r|r.get::<_,String>(3)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
        assert!(plans.iter().any(|s| s.contains("filings_cik_time")), "{plans:?}");
    }
    #[test]
    fn snapshot_300_companies_9000_rows_uses_five_queries_and_one_for_backlog() {
        let mut store = Store::memory().unwrap();
        let companies: Vec<_> =
            (1..=300).map(|cik| Company { cik, ticker: format!("T{cik}"), title: String::new() }).collect();
        store.replace_companies(&companies).unwrap();
        let rows: Vec<_> = (0..9000)
            .map(|i| Filing {
                accession: format!("acc-{i}"),
                cik: i % 300 + 1,
                ticker: format!("T{}", i % 300 + 1),
                form: "4".into(),
                filed: 100,
                primary_doc: "form4.xml".into(),
                ..Default::default()
            })
            .collect();
        store.upsert(&rows).unwrap();
        for f in &rows {
            store
                .save_insiders(
                    f,
                    &crate::insider::Document {
                        issuer_cik: f.cik,
                        transactions: vec![crate::insider::Transaction {
                            shares: Some(10.),
                            price: Some(20.),
                            code: "P".into(),
                            security_title: "Common stock".into(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        store.read_queries.set(0);
        let snapshot = store.filings().unwrap();
        assert_eq!(snapshot.len(), 9000);
        assert!(snapshot.iter().all(|f| f.insiders.len() == 1 && f.insider_parsed && f.tickers.len() == 1));
        assert_eq!(store.read_queries.get(), 5, "snapshot must batch all row dependencies");
        store.read_queries.set(0);
        assert!(store.insider_backlog().unwrap().is_empty());
        assert_eq!(store.read_queries.get(), 1);
    }
    #[test]
    fn legacy_store_migrates_and_discards_whole_submissions_bodies() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE filings(accession TEXT PRIMARY KEY,cik INTEGER NOT NULL,ticker TEXT NOT NULL,form TEXT NOT NULL,filed INTEGER NOT NULL,accepted INTEGER,report_date TEXT NOT NULL,items TEXT NOT NULL,primary_doc TEXT NOT NULL,description TEXT NOT NULL,url TEXT NOT NULL);
            INSERT INTO filings VALUES('acc',1,'A','8-K',100,NULL,'','','doc.htm','','');
            CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            INSERT INTO meta VALUES('body:1','large legacy body');
            INSERT INTO meta VALUES('etag:sub:1','validator');").unwrap();
        let mut store = Store::init(db).unwrap();
        assert_eq!(store.get("body:1").unwrap(), None);
        assert_eq!(store.get("etag:sub:1").unwrap().as_deref(), Some("validator"));
        assert!(store.enriched(1, "acc").unwrap());
        let mut reporting = store.filings().unwrap().remove(0);
        reporting.cik = 2;
        reporting.reporting_owner = true;
        store.upsert(&[reporting]).unwrap();
        assert_eq!(store.filings().unwrap().len(), 2);
    }
}
