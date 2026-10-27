//! All XML variants are derived in memory from the manager-supplied real filings.
use tr_edgar::{Filing, insider::*, store::Store};
const SALE: &str = include_str!("fixtures/form4_sale_10b5_1.xml");
const EXERCISE: &str = include_str!("fixtures/form4_exercise_withholding.xml");
const CIK: u64 = 320193;
#[test]
fn real_sale_plan_issuer_owner_relationship_and_lot() {
    let d = parse(SALE.as_bytes(), CIK).unwrap();
    assert_eq!(d.issuer_cik, CIK);
    assert_eq!(d.symbol, "AAPL");
    assert!(!d.amendment);
    let t = &d.transactions[0];
    assert_eq!(t.owner.name, "Newstead Jennifer");
    assert_eq!(t.owner.cik, Some(1780525));
    assert!(t.owner.is_officer);
    assert!(!t.owner.is_director);
    assert!(!t.owner.is_ten_percent_owner);
    assert_eq!(t.owner.role(), "SVP, GC");
    assert_eq!(short_name(&t.owner.name), "Newstead J.");
    assert_eq!(t.code, "S");
    assert_eq!(t.date, "2026-10-13");
    assert_eq!(t.shares, Some(2399.));
    assert_eq!(t.price, Some(345.));
    assert_eq!(t.owned_after, Some(37194.));
    assert_eq!(t.acquired, Some(false));
    assert_eq!(t.direct, Some(true));
    assert!(t.plan_10b5_1);
    assert!(!t.derivative);
    assert!(parse(SALE.as_bytes(), 42).is_err());
}
#[test]
fn real_exercise_withholding_derivative_and_footnote_only_price() {
    let d = parse(EXERCISE.as_bytes(), CIK).unwrap();
    assert_eq!(d.transactions.len(), 3);
    let m = &d.transactions[0];
    let f = &d.transactions[1];
    let derivative = &d.transactions[2];
    assert_eq!(m.owner.role(), "COO");
    assert_eq!(m.shares, Some(99878.));
    assert_eq!(m.price, None);
    assert_eq!(m.acquired, Some(true));
    assert_eq!(f.code, "F");
    assert_eq!(f.shares, Some(52854.));
    assert_eq!(f.price, Some(330.32));
    assert!(derivative.derivative);
    assert_eq!(derivative.owned_after, Some(0.));
    assert!(!m.plan_10b5_1);
    assert_eq!(aggregate(&d.transactions[..2])[1].text(), "Tax withholding 52,854");
}
#[test]
fn purchase_director_ten_percent_joint_owners_and_amendment() {
    let source = SALE
        .replace("<documentType>4</documentType>", "<documentType>4/A</documentType>")
        .replace("<transactionCode>S", "<transactionCode>P")
        .replace("<isDirector>false", "<isDirector>1")
        .replace("<isTenPercentOwner>false", "<isTenPercentOwner>true");
    let owner = &source[source.find("<reportingOwner>").unwrap()
        ..source.find("</reportingOwner>").unwrap() + "</reportingOwner>".len()];
    let joint = source.replace(
        "</reportingOwner>",
        &format!(
            "</reportingOwner>{}",
            owner.replace("Newstead Jennifer", "Other Owner").replace("0001780525", "0000000042")
        ),
    );
    let d = parse(joint.as_bytes(), CIK).unwrap();
    assert!(d.amendment);
    assert_eq!(d.owners.len(), 2);
    assert_eq!(d.transactions.len(), 2);
    assert_eq!(d.transactions[0].code, "P");
    assert_eq!(d.owners[0].role(), "SVP, GC, Director, 10% owner");
    assert_eq!(d.owners[1].cik, Some(42));
}
#[test]
fn empty_missing_nonfinite_malformed_and_dtd_never_invent_prices() {
    for price in ["", "NaN", "inf", "-1", "<footnoteId id=\"F1\"/>"] {
        let source = SALE.replace("<value>345.00</value>", &format!("<value>{price}</value>"));
        assert_eq!(parse(source.as_bytes(), CIK).unwrap().transactions[0].price, None);
    }
    let missing = SALE
        .replace("<value>2399</value>", "")
        .replace("<value>D</value>", "")
        .replace("<rptOwnerCik>0001780525</rptOwnerCik>", "");
    let d = parse(missing.as_bytes(), CIK).unwrap();
    assert_eq!(d.transactions[0].shares, None);
    assert_eq!(d.transactions[0].direct, None);
    assert_eq!(d.owners[0].cik, None);
    for source in [
        "broken",
        "<ownershipDocument>",
        "<ownershipDocument></wrong>",
        "<!DOCTYPE ownershipDocument><ownershipDocument/>",
    ] {
        assert!(parse(source.as_bytes(), CIK).is_err(), "{source}");
    }
}
#[test]
fn aggregate_multiple_lots_uses_share_weighted_average_and_missing_values_propagate() {
    let lot = &SALE[SALE.find("<nonDerivativeTransaction>").unwrap()
        ..SALE.find("</nonDerivativeTransaction>").unwrap() + "</nonDerivativeTransaction>".len()];
    let source = SALE.replace(
        "</nonDerivativeTable>",
        &format!("{} </nonDerivativeTable>", lot.replace("2399", "101").replace("345.00", "350.00")),
    );
    let d = parse(source.as_bytes(), CIK).unwrap();
    let g = aggregate(&d.transactions).remove(0);
    assert_eq!(g.lots, 2);
    assert_eq!(g.shares, Some(2500.));
    assert_eq!(g.value, Some(863005.));
    assert!((g.average_price.unwrap() - 345.202).abs() < 0.00001);
    assert_eq!(g.text(), "Sold 2 lots 2,500 @ avg 345.20");
    let mut missing = d.transactions.clone();
    missing[1].price = None;
    let g = aggregate(&missing).remove(0);
    assert_eq!(g.average_price, None);
    assert_eq!(g.value, None);
}
#[test]
fn action_roles_values_and_raw_document_path() {
    for (code, label, color) in [
        ("P", "Bought", Direction::Up),
        ("S", "Sold", Direction::Down),
        ("A", "Award", Direction::Neutral),
        ("M", "Option exercise", Direction::Neutral),
        ("F", "Tax withholding", Direction::Neutral),
        ("G", "Gift", Direction::Neutral),
        ("C", "Conversion", Direction::Neutral),
        ("J", "Other", Direction::Neutral),
        ("?", "Other", Direction::Neutral),
    ] {
        assert_eq!(action(code), (label, color));
    }
    for (n, s) in [(815000., "$815K"), (17200000., "$17.2M"), (1100000000., "$1.1B")] {
        assert_eq!(value(n), s);
    }
    assert_eq!(Owner { is_director: true, ..Default::default() }.role(), "Director");
    assert_eq!(Owner { is_ten_percent_owner: true, ..Default::default() }.role(), "10% owner");
    assert_eq!(number(120000.), "120,000");
    assert_eq!(short_name("Khan Sabih"), "Khan S.");
    for (title, expected) in [
        ("Chief Executive Officer", "CEO"),
        ("Chief Financial Officer", "CFO"),
        ("Chief Operating Officer", "COO"),
        ("General Counsel", "GC"),
    ] {
        assert_eq!(Owner { is_officer: true, officer_title: title.into(), ..Default::default() }.role(), expected);
    }
    let mut f = Filing { cik: CIK, accession: "0001140361-26-040540".into(), ..Default::default() };
    for path in ["xslF345X06/form4.xml", "form4.xml"] {
        f.primary_doc = path.into();
        assert_eq!(xml_url(&f).unwrap(), "https://www.sec.gov/Archives/edgar/data/320193/000114036126040540/form4.xml");
    }
    for path in ["doc.htm", "", "../bad.xml", "xslF/a/../bad.xml", "/bad.xml", "a.xml?query"] {
        f.primary_doc = path.into();
        assert_eq!(xml_url(&f), None);
    }
}
#[test]
fn immutable_store_roundtrip_indexed_reader_empty_document_and_pruning() {
    let mut s = Store::memory().unwrap();
    let f = Filing {
        cik: CIK,
        accession: "sale".into(),
        ticker: "AAPL".into(),
        form: "4".into(),
        filed: 100,
        ..Default::default()
    };
    s.upsert(std::slice::from_ref(&f)).unwrap();
    let d = parse(SALE.as_bytes(), CIK).unwrap();
    s.save_insiders(&f, &d).unwrap();
    s.save_insiders(&f, &Document { issuer_cik: CIK, ..Default::default() }).unwrap();
    assert_eq!(s.insider_transactions("sale").unwrap(), d.transactions);
    assert_eq!(s.insiders_for_cik(CIK, "2026-10-01", "2026-10-30").unwrap(), d.transactions);
    assert!(s.insiders_for_cik(42, "2026-10-01", "2026-10-30").unwrap().is_empty());
    assert_eq!(s.filings().unwrap()[0].insiders, d.transactions);
    s.prune(100 + 91 * 86400).unwrap();
    assert!(!s.insider_complete("sale").unwrap());
    assert!(s.insider_transactions("sale").unwrap().is_empty());
    let mut f = f;
    f.accession = "empty".into();
    s.upsert(std::slice::from_ref(&f)).unwrap();
    s.save_insiders(&f, &Document { issuer_cik: CIK, ..Default::default() }).unwrap();
    assert!(s.insider_complete("empty").unwrap());
}
