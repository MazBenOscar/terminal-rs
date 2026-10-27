//! Form 4 ownership documents and presentation-neutral transaction summaries.
use anyhow::{Result, bail, ensure};
use quick_xml::{Reader, events::Event};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Owner {
    pub name: String,
    pub cik: Option<u64>,
    pub is_director: bool,
    pub is_officer: bool,
    pub is_ten_percent_owner: bool,
    pub officer_title: String,
}
impl Owner {
    pub fn role(&self) -> String {
        let mut roles = vec![];
        if self.is_officer && !self.officer_title.is_empty() {
            let title = self.officer_title.to_ascii_uppercase();
            for (short, long) in [
                ("CEO", "CHIEF EXECUTIVE"),
                ("CFO", "CHIEF FINANCIAL"),
                ("COO", "CHIEF OPERATING"),
                ("SVP", "SENIOR VICE PRESIDENT"),
                ("GC", "GENERAL COUNSEL"),
            ] {
                if title.contains(short) || title.contains(long) {
                    roles.push(short.to_owned());
                }
            }
            if roles.is_empty() {
                roles.push(self.officer_title.clone());
            }
        }
        if self.is_director {
            roles.push("Director".into());
        }
        if self.is_ten_percent_owner {
            roles.push("10% owner".into());
        }
        roles.join(", ")
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Transaction {
    pub owner: Owner,
    pub code: String,
    /// ISO date; absent dates remain absent rather than using the filing timestamp.
    pub date: String,
    pub shares: Option<f64>,
    pub price: Option<f64>,
    pub acquired: Option<bool>,
    pub owned_after: Option<f64>,
    pub direct: Option<bool>,
    pub derivative: bool,
    pub plan_10b5_1: bool,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    pub issuer_cik: u64,
    pub symbol: String,
    pub amendment: bool,
    pub owners: Vec<Owner>,
    pub transactions: Vec<Transaction>,
}
// A bounded DOM built with the already-locked XML parser. No DTD/entity expansion.
#[derive(Default)]
struct Node {
    name: String,
    text: String,
    children: Vec<Node>,
}
impl Node {
    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|n| n.name == name)
    }
    fn find(&self, name: &str) -> Option<&Node> {
        self.child(name).or_else(|| self.children.iter().find_map(|n| n.find(name)))
    }
    fn value(&self, name: &str) -> String {
        self.find(name).map(|n| n.child("value").unwrap_or(n).text.trim().to_owned()).unwrap_or_default()
    }
}
pub fn parse(bytes: &[u8], expected_cik: u64) -> Result<Document> {
    ensure!(bytes.len() <= 8 * 1024 * 1024, "Oversized ownership document");
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut stack = vec![Node::default()];
    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                ensure!(stack.len() < 64, "Ownership XML nesting limit");
                stack.push(Node { name: String::from_utf8(e.local_name().as_ref().to_vec())?, ..Default::default() });
            }
            Event::End(_) => {
                ensure!(stack.len() > 1, "Unbalanced ownership XML");
                let node = stack.pop().unwrap();
                stack.last_mut().unwrap().children.push(node);
            }
            Event::Text(e) => stack.last_mut().unwrap().text.push_str(&e.decode()?),
            Event::CData(e) => stack.last_mut().unwrap().text.push_str(&e.decode()?),
            Event::GeneralRef(e) => {
                let reference = e.decode()?;
                let text = quick_xml::escape::unescape(&format!("&{reference};"))?.into_owned();
                stack.last_mut().unwrap().text.push_str(&text);
            }
            Event::DocType(_) => bail!("DTD not allowed in ownership document"),
            Event::Eof => break,
            _ => {}
        }
    }
    ensure!(stack.len() == 1, "Incomplete ownership XML");
    let root = stack.pop().unwrap();
    let root = root.child("ownershipDocument").ok_or_else(|| anyhow::anyhow!("Not an ownership document"))?;
    let issuer_cik = root.value("issuerCik").parse::<u64>()?;
    ensure!(issuer_cik == expected_cik, "Mismatched ownership issuer CIK");
    let form = root.value("documentType");
    ensure!(matches!(form.as_str(), "4" | "4/A"), "Not Form 4");
    let flag = |s: String| s == "1" || s.eq_ignore_ascii_case("true");
    let owners: Vec<_> = root
        .children
        .iter()
        .filter(|n| n.name == "reportingOwner")
        .map(|n| Owner {
            name: n.value("rptOwnerName"),
            cik: n.value("rptOwnerCik").parse().ok(),
            is_director: flag(n.value("isDirector")),
            is_officer: flag(n.value("isOfficer")),
            is_ten_percent_owner: flag(n.value("isTenPercentOwner")),
            officer_title: n.value("officerTitle"),
        })
        .collect();
    let plan = flag(root.value("aff10b5One"));
    let mut transactions = vec![];
    for table in &root.children {
        for n in &table.children {
            if !matches!(n.name.as_str(), "nonDerivativeTransaction" | "derivativeTransaction") {
                continue;
            }
            let numeric = |name| n.value(name).parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.);
            let optional_flag = |name, yes, no| match n.value(name).as_str() {
                s if s == yes => Some(true),
                s if s == no => Some(false),
                _ => None,
            };
            let date = n.value("transactionDate");
            let date = if chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").is_ok() { date } else { String::new() };
            // The document reports joint owners, not separate copies of the economic lot.
            // Keep each owner for display/cluster identity; summaries deduplicate joint lots.
            for owner in &owners {
                transactions.push(Transaction {
                    owner: owner.clone(),
                    code: n.value("transactionCode"),
                    date: date.clone(),
                    shares: numeric("transactionShares"),
                    price: numeric("transactionPricePerShare"),
                    acquired: optional_flag("transactionAcquiredDisposedCode", "A", "D"),
                    owned_after: numeric("sharesOwnedFollowingTransaction"),
                    direct: optional_flag("directOrIndirectOwnership", "D", "I"),
                    derivative: n.name == "derivativeTransaction",
                    plan_10b5_1: plan,
                });
            }
        }
    }
    Ok(Document {
        issuer_cik,
        symbol: root.value("issuerTradingSymbol"),
        amendment: form == "4/A",
        owners,
        transactions,
    })
}
/// Strip only SEC's leading renderer directory. Reject unsafe/non-XML paths.
pub fn xml_url(f: &crate::Filing) -> Option<String> {
    let mut path = f.primary_doc.as_str();
    if let Some((first, rest)) = path.split_once('/')
        && first.starts_with("xsl")
    {
        path = rest;
    }
    if !path.to_ascii_lowercase().ends_with(".xml")
        || !path.split('/').all(|s| {
            !s.is_empty()
                && s != "."
                && s != ".."
                && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        })
    {
        return None;
    }
    Some(crate::filing_url(f.cik, &f.accession, path))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Neutral,
}
pub fn action(code: &str) -> (&'static str, Direction) {
    match code {
        "P" => ("Bought", Direction::Up),
        "S" => ("Sold", Direction::Down),
        "A" => ("Award", Direction::Neutral),
        "M" => ("Option exercise", Direction::Neutral),
        "F" => ("Tax withholding", Direction::Neutral),
        "G" => ("Gift", Direction::Neutral),
        "C" => ("Conversion", Direction::Neutral),
        _ => ("Other", Direction::Neutral),
    }
}
pub fn short_name(name: &str) -> String {
    let mut words = name.split_whitespace();
    let last = words.next().unwrap_or("");
    words.next().and_then(|s| s.chars().next()).map_or_else(|| last.to_owned(), |c| format!("{last} {c}."))
}
pub fn number(n: f64) -> String {
    let s = if n.fract().abs() < 0.000001 { format!("{n:.0}") } else { format!("{n:.2}") };
    let (whole, fraction) = s.split_once('.').map_or((s.as_str(), None), |(w, f)| (w, Some(f)));
    let mut result = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(c);
    }
    if let Some(f) = fraction {
        result.push('.');
        result.push_str(f);
    }
    result
}
pub fn value(n: f64) -> String {
    if n >= 1e9 {
        format!("${:.1}B", n / 1e9)
    } else if n >= 1e6 {
        format!("${:.1}M", n / 1e6)
    } else if n >= 1e3 {
        format!("${:.0}K", n / 1e3)
    } else {
        format!("${n:.0}")
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Aggregate {
    pub owner: Owner,
    pub code: String,
    pub derivative: bool,
    pub lots: usize,
    pub shares: Option<f64>,
    pub average_price: Option<f64>,
    pub value: Option<f64>,
    pub plan: bool,
}
impl Aggregate {
    pub fn text(&self) -> String {
        let (label, _) = action(&self.code);
        let lots = if self.lots > 1 { format!("{} lots ", self.lots) } else { String::new() };
        let shares = self.shares.map(number).unwrap_or_else(|| "—".into());
        let price = self
            .average_price
            .filter(|_| matches!(self.code.as_str(), "P" | "S"))
            .map(|p| format!(" @ {}{p:.2}", if self.lots > 1 { "avg " } else { "" }))
            .unwrap_or_default();
        format!("{label} {lots}{shares}{price}")
    }
}
pub fn aggregate(rows: &[Transaction]) -> Vec<Aggregate> {
    let mut groups: Vec<(Aggregate, bool)> = vec![];
    for t in rows {
        let found =
            groups.iter().position(|(g, _)| g.owner == t.owner && g.code == t.code && g.derivative == t.derivative);
        let i = found.unwrap_or_else(|| {
            groups.push((
                Aggregate {
                    owner: t.owner.clone(),
                    code: t.code.clone(),
                    derivative: t.derivative,
                    lots: 0,
                    shares: Some(0.),
                    average_price: None,
                    value: Some(0.),
                    plan: false,
                },
                true,
            ));
            groups.len() - 1
        });
        let (g, complete) = &mut groups[i];
        g.lots += 1;
        g.plan |= t.plan_10b5_1;
        g.shares = g.shares.zip(t.shares).map(|(a, b)| a + b);
        g.value = g.value.zip(t.shares.zip(t.price)).map(|(v, (s, p))| v + s * p);
        *complete &= t.price.is_some();
    }
    groups
        .into_iter()
        .map(|(mut g, complete)| {
            g.average_price =
                if complete { g.value.zip(g.shares).filter(|(_, s)| *s > 0.).map(|(v, s)| v / s) } else { None };
            if !matches!(g.code.as_str(), "P" | "S") {
                g.value = None;
            }
            g
        })
        .collect()
}
