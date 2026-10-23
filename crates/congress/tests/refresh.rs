//! End-to-end refresh against fixture-backed sources: discovery, parsing,
//! roster joins, idempotency and tracked-politician alerts.

use anyhow::{Result, anyhow};
use chrono::NaiveDate;
use tokio::sync::Mutex;
use tr_congress::house::{self, IndexEntry};
use tr_congress::refresh::{RefreshConfig, Site, Source, refresh};
use tr_congress::roster::{self, Roster};
use tr_congress::senate::{self, ReportRef};
use tr_congress::{Chamber, Db, Party};

const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn read(p: &str) -> Vec<u8> {
    std::fs::read(format!("{FIX}/{p}")).unwrap()
}

struct Fixtures {
    senate_limit: usize,
}

impl Source for Fixtures {
    async fn roster_for(&self, site: Site) -> Result<Roster> {
        match site {
            Site::HouseRoster => {
                let h = String::from_utf8(read("roster/house_members.xml"))?;
                Ok(Roster::new(roster::parse_house_roster(&h)?, vec![]))
            }
            Site::Senate => {
                let s = String::from_utf8(read("roster/senators.xml"))?;
                Ok(Roster::new(vec![], roster::parse_senate_roster(&s)?))
            }
            Site::House => Ok(Roster::default()),
        }
    }
    async fn house_index(&self, _year: i32) -> Result<Vec<IndexEntry>> {
        house::parse_index_xml(&String::from_utf8(read("house/2026FD.xml"))?)
    }
    async fn house_pdf(&self, e: &IndexEntry) -> Result<Vec<u8>> {
        Ok(read(&format!("house/{}.pdf", e.doc_id)))
    }
    async fn senate_reports(&self, _since: Option<NaiveDate>) -> Result<Vec<ReportRef>> {
        let mut r = senate::parse_search_json(&String::from_utf8(read("senate/search.json"))?)?.reports;
        r.truncate(self.senate_limit);
        Ok(r)
    }
    async fn senate_ptr(&self, r: &ReportRef) -> Result<String> {
        let f = match r.id.as_str() {
            "0541be4f-4f96-4d84-8d6f-d349f218bb2a" => "senate/ptr_bonds.html",
            "6bf3b6f7-9e1b-499a-bd5a-990292ce2e72" => "senate/ptr_stocks.html",
            "9e2ff733-aeac-4ce8-872c-3d6b7913da88" => "senate/ptr_many.html",
            _ => return Err(anyhow!("offline: {}", r.id)),
        };
        Ok(String::from_utf8(read(f))?)
    }
}

fn cfg() -> RefreshConfig {
    RefreshConfig {
        house_years: vec![2026],
        house_since: None,
        senate_since: None,
        max_new_per_chamber: 100,
        concurrency: 2,
    }
}

#[tokio::test]
async fn refresh_ingests_both_chambers_and_alerts() {
    let db = Mutex::new(Db::open_in_memory().unwrap());
    let src = Fixtures { senate_limit: 4 };
    let r1 = refresh(&src, &db, &cfg()).await.unwrap();
    // 3 House PTRs (one scanned) + 3 Senate PTRs; one Senate fetch fails offline and is retried later.
    assert_eq!(r1.scanned, 1, "{:?}", r1.errors);
    assert!(r1.new_trades > 40);
    assert!(r1.errors.iter().any(|e| e.contains("offline")));
    {
        let db = db.lock().await;
        let aapl = db.trades_for_ticker("AAPL", 10).unwrap();
        assert_eq!(aapl[0].politician, "Mark Alford");
        assert_eq!(aapl[0].party, Party::Republican);
        assert_eq!(aapl[0].chamber, Chamber::House);
        let jpm = db.trades_for_ticker("JPM", 10).unwrap();
        assert_eq!(jpm[0].chamber, Chamber::Senate);
        let fetterman = db.search_politicians("Fetterman", 5).unwrap();
        assert_eq!(fetterman[0].politician.party, Party::Democrat);
        assert_eq!(fetterman[0].politician.state.as_deref(), Some("PA"));
        // Track a politician before the next run.
        let allen = db.search_politicians("Allen", 5).unwrap()[0].politician.id;
        db.set_tracked(allen, true).unwrap();
    }

    // Second run is idempotent: nothing new except the previously failed fetch.
    let before = db.lock().await.stats().unwrap();
    let r2 = refresh(&src, &db, &cfg()).await.unwrap();
    assert_eq!(r2.new_filings, 0);
    assert_eq!(db.lock().await.stats().unwrap().trades, before.trades);

    // A tracked politician's filing that appears later raises exactly one alert.
    db.lock().await.conn().execute("DELETE FROM filings WHERE id = 'house:20033751'", []).unwrap();
    let r3 = refresh(&src, &db, &cfg()).await.unwrap();
    assert_eq!(r3.new_filings, 1);
    assert_eq!(r3.alerts.len(), 1);
    assert!(r3.alerts[0].message.contains("FERG"), "{}", r3.alerts[0].message);
    assert_eq!(db.lock().await.alerts(true, 10).unwrap().len(), 1);
}
