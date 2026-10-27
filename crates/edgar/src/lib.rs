//! Watchlist-only SEC EDGAR metadata. No history pages and no market-wide backfill.
pub mod scheduler;
pub mod store;

use anyhow::{Result, bail};
use chrono::{DateTime, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Company {
    pub cik: u64,
    pub ticker: String,
    pub title: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filing {
    pub accession: String,
    pub cik: u64,
    pub ticker: String,
    /// All share classes of this registrant, populated from the company map on read.
    #[serde(default)]
    pub tickers: Vec<String>,
    /// A Form 4 feed hit for a reporting owner, not the watched issuer.
    #[serde(default)]
    pub reporting_owner: bool,
    #[serde(default)]
    pub from_submissions: bool,
    pub form: String,
    pub filed: i64,
    pub accepted: Option<i64>,
    pub report_date: String,
    pub items: String,
    pub primary_doc: String,
    pub description: String,
    pub url: String,
}
impl Filing {
    pub fn matches_symbol(&self, symbol: &str) -> bool {
        ticker(symbol).is_some_and(|t| t == self.ticker || self.tickers.contains(&t))
    }
    pub fn time(&self) -> i64 {
        self.accepted.unwrap_or(self.filed)
    }
    pub fn time_label(&self) -> String {
        chrono::DateTime::from_timestamp(self.time(), 0)
            .map(|d| d.format(if self.accepted.is_some() { "%m-%d %H:%M" } else { "%m-%d" }).to_string())
            .unwrap_or_default()
    }
    pub fn detail(&self) -> String {
        if self.form.starts_with("8-K") && !self.items.is_empty() {
            format!("{} · {}", self.form, decode_items(&self.items))
        } else {
            format!("{} · {}", self.form, self.description)
        }
    }
    pub fn chart_marker(&self) -> bool {
        matches!(self.form.as_str(), "8-K" | "8-K/A" | "10-Q" | "10-Q/A" | "10-K" | "10-K/A")
    }
}
/// Only US equity keys or plain tickers can map to SEC registrants.
pub fn ticker(symbol: &str) -> Option<String> {
    let words: Vec<_> = symbol.split_whitespace().collect();
    match words.as_slice() {
        [t] | [t, "US", "Equity"] => Some(t.to_ascii_uppercase().replace('.', "-")),
        _ => None,
    }
}
pub fn parse_tickers(bytes: &[u8]) -> Result<Vec<Company>> {
    #[derive(Deserialize)]
    struct Entry {
        cik_str: u64,
        ticker: String,
        title: String,
    }
    let entries: BTreeMap<String, Entry> = serde_json::from_slice(bytes)?;
    Ok(entries
        .into_values()
        .map(|e| Company { cik: e.cik_str, ticker: e.ticker.to_ascii_uppercase(), title: e.title })
        .collect())
}
pub fn watched(companies: &[Company], symbols: &[String]) -> BTreeMap<u64, String> {
    let tickers: BTreeSet<_> = symbols.iter().filter_map(|s| ticker(s)).collect();
    companies.iter().filter(|c| tickers.contains(&c.ticker)).map(|c| (c.cik, c.ticker.clone())).collect()
}
fn accession(value: &str) -> Option<String> {
    value.as_bytes().windows(20).find_map(|s| {
        (s[10] == b'-' && s[13] == b'-' && s.iter().enumerate().all(|(i, c)| i == 10 || i == 13 || c.is_ascii_digit()))
            .then(|| String::from_utf8_lossy(s).into_owned())
    })
}
pub fn filing_url(cik: u64, acc: &str, primary: &str) -> String {
    let compact = acc.replace('-', "");
    let document = if primary.is_empty() || primary.contains(['/', '\\', '?', '#']) || primary == "." || primary == ".."
    {
        format!("{acc}-index.html")
    } else {
        primary.to_owned()
    };
    format!("https://www.sec.gov/Archives/edgar/data/{cik}/{compact}/{document}")
}
pub fn parse_feed(bytes: &[u8]) -> Result<Vec<Filing>> {
    // Only the XML declaration determines decoding; body mentions are never rewritten.
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let declaration = if bytes.get(..5).is_some_and(|s| s.eq_ignore_ascii_case(b"<?xml")) {
        bytes.windows(2).position(|w| w == b"?>").map(|end| &bytes[..end])
    } else {
        None
    };
    let encoding = declaration.and_then(|d| {
        let lower = std::str::from_utf8(d).ok()?.to_ascii_lowercase();
        let at = lower.find("encoding")? + 8;
        let tail = lower[at..].trim_start().strip_prefix('=')?.trim_start();
        let quote = tail.chars().next()?;
        if quote != '\'' && quote != '"' {
            return None;
        }
        let end = tail[1..].find(quote)? + 1;
        let start = d.len() - tail.len() + 1;
        Some((start, start + end - 1, tail[1..end].to_owned()))
    });
    let text = if let Some((start, end, name)) =
        encoding.filter(|(_, _, n)| matches!(n.as_str(), "iso-8859-1" | "latin1" | "windows-1252"))
    {
        let decode = |part: &[u8]| {
            part.iter()
                .map(|&b| {
                    if name == "windows-1252" && (0x80..=0x9f).contains(&b) {
                        const CP: [u32; 32] = [
                            0x20ac, 0x81, 0x201a, 0x192, 0x201e, 0x2026, 0x2020, 0x2021, 0x2c6, 0x2030, 0x160, 0x2039,
                            0x152, 0x8d, 0x17d, 0x8f, 0x90, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014,
                            0x2dc, 0x2122, 0x161, 0x203a, 0x153, 0x9d, 0x17e, 0x178,
                        ];
                        char::from_u32(CP[(b - 0x80) as usize]).unwrap()
                    } else {
                        char::from(b)
                    }
                })
                .collect::<String>()
        };
        format!("{}UTF-8{}", decode(&bytes[..start]), decode(&bytes[end..]))
    } else {
        String::from_utf8(bytes.to_vec())?
    };
    let feed = feed_rs::parser::parse(text.as_bytes())?;
    Ok(feed
        .entries
        .into_iter()
        .filter_map(|entry| {
            let title = entry.title?.content;
            let (form, company) = title.split_once(" - ")?;
            let end = company.rfind(')')?;
            let start = company[..end].rfind('(')?;
            // A trailing (Filer)/(Reporting) follows the numeric CIK in real titles.
            let (company_name, cik) = if let Ok(cik) = company[start + 1..end].parse::<u64>() {
                (company[..start].trim(), cik)
            } else {
                let end = company[..start].rfind(')')?;
                let begin = company[..end].rfind('(')?;
                (company[..begin].trim(), company[begin + 1..end].parse::<u64>().ok()?)
            };
            let link = entry
                .links
                .iter()
                .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
                .map(|l| l.href.clone())
                .unwrap_or_default();
            let acc = accession(&entry.id).or_else(|| accession(&link))?;
            Some(Filing {
                accession: acc.clone(),
                cik,
                form: form.to_owned(),
                reporting_owner: company[end + 1..].contains("(Reporting)")
                    || company[start + 1..end].eq_ignore_ascii_case("Reporting"),
                accepted: entry.updated.or(entry.published).map(|d| d.timestamp()),
                filed: entry.updated.or(entry.published)?.timestamp(),
                description: company_name.to_owned(),
                url: if link.starts_with("https://www.sec.gov/") { link } else { filing_url(cik, &acc, "") },
                ..Default::default()
            })
        })
        .collect())
}
fn date(value: &str) -> Option<i64> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()?.and_hms_opt(0, 0, 0).map(|d| d.and_utc().timestamp())
}
fn tracked(form: &str) -> bool {
    let f = form.strip_suffix("/A").unwrap_or(form);
    matches!(f, "8-K" | "10-Q" | "10-K" | "S-1" | "SC 13D" | "SC 13G" | "3" | "4" | "5" | "DEF 14A")
        || f.starts_with("424B")
}
pub fn parse_submissions(bytes: &[u8], cik: u64, ticker: &str, since: i64) -> Result<Vec<Filing>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Recent {
        accession_number: Vec<String>,
        filing_date: Vec<String>,
        form: Vec<String>,
        #[serde(default)]
        report_date: Vec<String>,
        #[serde(default)]
        acceptance_date_time: Vec<String>,
        #[serde(default)]
        items: Vec<String>,
        #[serde(default)]
        primary_document: Vec<String>,
        #[serde(default)]
        primary_doc_description: Vec<String>,
    }
    #[derive(Deserialize)]
    struct Filings {
        recent: Recent,
    }
    #[derive(Deserialize)]
    struct Submissions {
        cik: serde_json::Value,
        filings: Filings,
    }
    let s: Submissions = serde_json::from_slice(bytes)?;
    let returned = s.cik.as_u64().or_else(|| s.cik.as_str()?.parse().ok());
    if returned != Some(cik) {
        bail!("Mismatched submissions CIK");
    }
    let r = s.filings.recent;
    if r.accession_number.len() != r.form.len() || r.form.len() != r.filing_date.len() {
        bail!("Misaligned recent arrays");
    }
    let value = |v: &[String], i| v.get(i).cloned().unwrap_or_default();
    Ok(r.accession_number
        .iter()
        .enumerate()
        .filter_map(|(i, acc)| {
            let filed = date(&r.filing_date[i])?;
            if filed < since || !tracked(&r.form[i]) || accession(acc).as_deref() != Some(acc) {
                return None;
            }
            let primary = value(&r.primary_document, i);
            Some(Filing {
                accession: acc.clone(),
                cik,
                ticker: ticker.into(),
                form: r.form[i].clone(),
                filed,
                accepted: r
                    .acceptance_date_time
                    .get(i)
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.timestamp()),
                report_date: value(&r.report_date, i),
                items: value(&r.items, i),
                url: filing_url(cik, acc, &primary),
                primary_doc: primary,
                description: value(&r.primary_doc_description, i),
                from_submissions: true,
                ..Default::default()
            })
        })
        .collect())
}
pub fn decode_items(items: &str) -> String {
    items
        .split([',', ';', ' '])
        .filter(|s| !s.is_empty())
        .map(|code| {
            let description = match code {
                "1.01" => "Entry into a Material Definitive Agreement",
                "1.02" => "Termination of a Material Definitive Agreement",
                "1.03" => "Bankruptcy or Receivership",
                "2.01" => "Completion of Acquisition or Disposition of Assets",
                "2.02" => "Results of Operations and Financial Condition",
                "2.03" => "Creation of a Direct Financial Obligation",
                "2.04" => "Triggering Events That Accelerate or Increase a Direct Financial Obligation",
                "3.03" => "Material Modification to Rights of Security Holders",
                "5.04" => "Temporary Suspension of Trading Under Employee Benefit Plans",
                "5.05" => "Amendments to or Waivers of the Code of Ethics",
                "5.06" => "Change in Shell Company Status",
                "5.08" => "Shareholder Director Nominations",
                "6.01" => "ABS Informational and Computational Material",
                "6.02" => "Change of Servicer or Trustee",
                "6.03" => "Change in Credit Enhancement or Other External Support",
                "6.04" => "Failure to Make a Required Distribution",
                "6.05" => "Securities Act Updating Disclosure",
                "2.05" => "Costs Associated with Exit or Disposal Activities",
                "2.06" => "Material Impairments",
                "3.01" => "Notice of Delisting or Failure to Satisfy a Continued Listing Rule",
                "3.02" => "Unregistered Sales of Equity Securities",
                "4.01" => "Changes in Registrant’s Certifying Accountant",
                "4.02" => "Non-Reliance on Previously Issued Financial Statements",
                "5.01" => "Changes in Control of Registrant",
                "5.02" => "Departure/Appointment of Officers",
                "5.03" => "Amendments to Articles of Incorporation or Bylaws",
                "5.07" => "Shareholder Vote",
                "7.01" => "Reg FD Disclosure",
                "8.01" => "Other Events",
                "9.01" => "Financial Statements and Exhibits",
                _ => return format!("Item {code}"),
            };
            format!("{code} {description}")
        })
        .collect::<Vec<_>>()
        .join(" · ")
}
