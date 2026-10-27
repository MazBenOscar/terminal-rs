use tr_edgar::{store::Store, *};
const ATOM: &[u8] = include_bytes!("fixtures/current_8k.atom");
const TICKERS: &[u8] = include_bytes!("fixtures/company_tickers.json");
const APPLE: &[u8] = include_bytes!("fixtures/submissions_aapl.json");

#[test]
fn real_atom_iso_8859_titles_accessions_times_and_links() {
    let feed = parse_feed(ATOM).unwrap();
    assert_eq!(feed.len(), 40);
    let first = &feed[0];
    assert_eq!(first.cik, 1_526_243);
    assert_eq!(first.form, "8-K");
    assert_eq!(first.description, "PERPETUA RESOURCES CORP.");
    assert_eq!(first.accession, "0001104659-26-118875");
    assert!(first.url.ends_with("0001104659-26-118875-index.htm"));
    assert_eq!(chrono::DateTime::from_timestamp(first.filed, 0).unwrap().to_rfc3339(), "2026-10-26T21:30:28+00:00");
    assert!(feed.iter().all(|f| f.cik > 0 && f.accession.len() == 20 && f.filed > 0));
}
#[test]
fn declaration_only_decoding_preserves_body_mentions_and_windows_punctuation() {
    let source = String::from_utf8(ATOM.to_vec()).unwrap();
    let utf8 = source.replacen("ISO-8859-1", "UTF-8", 1).replacen("PERPETUA", "ISO-8859-1 É", 1);
    assert_eq!(parse_feed(utf8.as_bytes()).unwrap()[0].description, "ISO-8859-1 É RESOURCES CORP.");
    for encoding in ["iso-8859-1", "latin1", "WINDOWS-1252"] {
        let text = source.replacen("ISO-8859-1", encoding, 1).replacen("PERPETUA", "ISO-8859-1 X", 1);
        let mut bytes = text.into_bytes();
        let index = bytes.windows(3).position(|w| w == b" X ").unwrap() + 1;
        bytes[index] = if encoding == "WINDOWS-1252" { 0x80 } else { 0xc9 };
        let expected = if encoding == "WINDOWS-1252" { "€" } else { "É" };
        assert_eq!(parse_feed(&bytes).unwrap()[0].description, format!("ISO-8859-1 {expected} RESOURCES CORP."));
    }
    let no_declaration = utf8.split_once("?>").unwrap().1;
    assert_eq!(parse_feed(no_declaration.as_bytes()).unwrap()[0].description, "ISO-8859-1 É RESOURCES CORP.");
}
#[test]
fn form_four_roles_are_preserved_for_each_company() {
    let source = String::from_utf8(ATOM.to_vec()).unwrap();
    let issuer = source.replacen("8-K - PERPETUA", "4 - PERPETUA", 1).replacen("(Filer)", "(Issuer)", 1);
    let reporting = issuer.replacen("(Issuer)", "(Reporting)", 1);
    let mut rows =
        vec![parse_feed(issuer.as_bytes()).unwrap().remove(0), parse_feed(reporting.as_bytes()).unwrap().remove(0)];
    assert!(!rows[0].reporting_owner);
    assert!(rows[1].reporting_owner);
    rows[1].cik = 42;
    let mut store = Store::memory().unwrap();
    store.upsert(&rows).unwrap();
    assert_eq!(store.filings().unwrap().len(), 2);
    assert_eq!(store.filings().unwrap().iter().filter(|f| f.reporting_owner).count(), 1);
    let mut enrichment = rows[1].clone();
    enrichment.from_submissions = true;
    enrichment.reporting_owner = false; // submissions do not supply the feed's role
    enrichment.primary_doc = "ownership.xml".into();
    store.upsert(&[enrichment]).unwrap();
    assert_eq!(store.filings().unwrap().len(), 2);
    assert_eq!(store.filings().unwrap().iter().filter(|f| f.reporting_owner).count(), 1);
}
#[test]
fn accepted_order_and_date_only_fallback_and_stable_ticker() {
    let mut store = Store::memory().unwrap();
    let first = Filing {
        accession: "first".into(),
        cik: 1,
        ticker: "GOOG".into(),
        filed: 86400,
        accepted: Some(90000),
        ..Default::default()
    };
    let second = Filing {
        accession: "second".into(),
        cik: 1,
        ticker: "GOOGL".into(),
        filed: 86400,
        accepted: Some(91000),
        ..Default::default()
    };
    store.upsert(&[first.clone(), second]).unwrap();
    let mut replacement = first;
    replacement.ticker = "GOOGL".into();
    store.upsert(&[replacement]).unwrap();
    let rows = store.filings().unwrap();
    assert_eq!(rows[0].accession, "second");
    assert_eq!(rows[1].ticker, "GOOG");
    assert!(rows[0].time_label().contains(':'));
    assert_eq!(Filing { filed: 86400, ..Default::default() }.time_label(), "01-02");
}
#[test]
fn real_ticker_mapping_and_watched_cik_filtering() {
    let companies = parse_tickers(TICKERS).unwrap();
    assert_eq!(companies.len(), 300);
    let aapl = companies.iter().find(|c| c.ticker == "AAPL").unwrap();
    assert_eq!(aapl.cik, 320193);
    let watched =
        watched(&companies, &["AAPL US Equity".into(), "AAPL".into(), "BTCUSD Curncy".into(), "AAPL CA Equity".into()]);
    assert_eq!(watched.len(), 1);
    assert_eq!(watched[&320193], "AAPL");
    assert_eq!(ticker("BRK.B US Equity").as_deref(), Some("BRK-B"));
    assert!(ticker("AAPL CA Equity").is_none());
    let watched = watched_for_feed();
    let hits: Vec<_> = parse_feed(ATOM).unwrap().into_iter().filter(|f| watched.contains_key(&f.cik)).collect();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].cik, 1_526_243);
}
fn watched_for_feed() -> std::collections::BTreeMap<u64, String> {
    [(1_526_243, "PPTA".into())].into_iter().collect()
}
#[test]
fn real_submissions_arrays_metadata_windows_and_urls() {
    let rows = parse_submissions(APPLE, 320193, "AAPL", 0).unwrap();
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|f| f.cik == 320193 && f.ticker == "AAPL" && !f.primary_doc.is_empty()));
    let eight = rows.iter().find(|f| f.form == "8-K" && !f.items.is_empty()).unwrap();
    assert!(eight.accepted.is_some());
    assert!(eight.url.contains(&eight.accession.replace('-', "")));
    assert!(eight.url.ends_with(&eight.primary_doc));
    let now = rows.iter().map(|f| f.filed).max().unwrap() + 1;
    let month = parse_submissions(APPLE, 320193, "AAPL", now - 30 * 86400).unwrap();
    let quarter = parse_submissions(APPLE, 320193, "AAPL", now - 90 * 86400).unwrap();
    assert!(!month.is_empty() && quarter.len() > month.len());
    assert!(month.iter().all(|f| f.filed >= now - 30 * 86400));
    assert!(parse_submissions(APPLE, 1, "WRONG", 0).is_err());
    assert_eq!(
        filing_url(320193, "0000320193-26-000001", "aapl.htm"),
        "https://www.sec.gov/Archives/edgar/data/320193/000032019326000001/aapl.htm"
    );
    assert!(filing_url(320193, "0000320193-26-000001", "").ends_with("0000320193-26-000001-index.html"));
    assert!(filing_url(320193, "0000320193-26-000001", "../bad").ends_with("-index.html"));
    // Insider forms render through an XSL subdirectory; that relative path is kept.
    assert_eq!(
        filing_url(320193, "0001140361-26-040540", "xslF345X06/form4.xml"),
        "https://www.sec.gov/Archives/edgar/data/320193/000114036126040540/xslF345X06/form4.xml"
    );
    for bad in ["a/../b.xml", "/abs.xml", "a//b.xml", "a\\b.xml", "a/b.xml?x=1", "a/b#c", "a/./b"] {
        assert!(filing_url(320193, "0000320193-26-000001", bad).ends_with("-index.html"), "{bad}");
    }
}
#[test]
fn eight_k_decode_table_covers_all_requested_items_and_preserves_unknown() {
    let items = "1.01,1.02,1.03,2.01,2.02,2.03,2.05,2.06,3.01,3.02,4.01,4.02,5.01,5.02,5.03,5.07,7.01,8.01,9.01";
    let decoded = decode_items(items);
    assert!(!decoded.contains("Unknown"));
    for phrase in [
        "Material Definitive Agreement",
        "Bankruptcy",
        "Results of Operations",
        "Non-Reliance",
        "Departure/Appointment",
        "Shareholder Vote",
        "Reg FD",
        "Other Events",
        "Financial Statements and Exhibits",
    ] {
        assert!(decoded.contains(phrase));
    }
    assert_eq!(decode_items("99.99"), "Item 99.99");
    assert!(!decode_items("2.04,3.03,5.04,5.05,5.06,5.08,6.01,6.02,6.03,6.04,6.05").contains("Item "));
    assert!(decode_items("").is_empty());
}
#[test]
fn sqlite_upsert_dedupe_prune_and_metadata_cache() {
    let mut db = Store::memory().unwrap();
    let companies = parse_tickers(TICKERS).unwrap();
    db.replace_companies(&companies).unwrap();
    assert_eq!(db.companies().unwrap().len(), 300);
    let rows = parse_submissions(APPLE, 320193, "AAPL", 0).unwrap();
    db.upsert(&rows).unwrap();
    db.upsert(&rows).unwrap();
    assert_eq!(db.filings().unwrap().len(), rows.len());
    let old = rows[0].clone();
    db.upsert(&[Filing {
        accession: old.accession.clone(),
        cik: old.cik,
        ticker: old.ticker.clone(),
        form: old.form.clone(),
        filed: old.filed,
        ..Default::default()
    }])
    .unwrap();
    assert_eq!(
        db.filings().unwrap().iter().find(|f| f.accession == old.accession).unwrap().primary_doc,
        old.primary_doc
    );
    db.set("etag:map", "fixture-etag").unwrap();
    assert_eq!(db.get("etag:map").unwrap().as_deref(), Some("fixture-etag"));
    let now = rows.iter().map(|f| f.filed).max().unwrap();
    let expected = rows.iter().filter(|f| f.filed >= now - 90 * 86400).count();
    let removed = db.prune(now).unwrap();
    assert_eq!(removed, rows.len() - expected);
    assert_eq!(db.filings().unwrap().len(), expected);
}

#[test]
fn review_lowercase_latin1_declaration() {
    let feed = String::from_utf8(ATOM.to_vec()).unwrap().replacen("ISO-8859-1", "iso-8859-1", 1);
    let mut bytes = feed.into_bytes();
    let at = bytes.windows(9).position(|w| w == b"PERPETUA ").unwrap();
    bytes[at + 1] = 0xC9; // Latin-1 capital E with acute
    let parsed = parse_feed(&bytes).unwrap();
    assert_eq!(parsed.len(), 40);
    assert_eq!(parsed[0].description, "P\u{c9}RPETUA RESOURCES CORP.");
}
