//! House Clerk financial disclosures (disclosures-clerk.house.gov).
//!
//! The Clerk publishes a yearly ZIP (`{year}FD.zip`) containing an XML index of
//! every filing. Periodic Transaction Reports have `FilingType` `P`; each links
//! to a PDF at `/public_disc/ptr-pdfs/{year}/{DocID}.pdf`. Electronically filed
//! PTRs are text PDFs that we parse; paper filings are scanned images with no
//! text layer and are recorded as such.

use std::io::{Cursor, Read};
use std::sync::LazyLock;

use anyhow::{Context, Result, anyhow};
use chrono::NaiveDate;
use regex::Regex;
use serde::Deserialize;

use crate::model::{AmountRange, ParsedTrade, TxType, normalize_ticker, parse_us_date};

pub const BASE: &str = "https://disclosures-clerk.house.gov";

pub fn index_zip_url(year: i32) -> String {
    format!("{BASE}/public_disc/financial-pdfs/{year}FD.zip")
}

pub fn ptr_pdf_url(year: i32, doc_id: &str) -> String {
    format!("{BASE}/public_disc/ptr-pdfs/{year}/{doc_id}.pdf")
}

/// One row of the yearly filing index.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IndexEntry {
    pub doc_id: String,
    pub prefix: String,
    pub first: String,
    pub last: String,
    pub suffix: String,
    pub filing_type: String,
    pub state_district: String,
    pub year: i32,
    pub filing_date: Option<NaiveDate>,
}

impl IndexEntry {
    pub fn is_ptr(&self) -> bool {
        self.filing_type == "P"
    }

    pub fn display_name(&self) -> String {
        let mut n = format!("{} {}", self.first.trim(), self.last.trim());
        if !self.suffix.trim().is_empty() {
            n.push(' ');
            n.push_str(self.suffix.trim());
        }
        n
    }

    pub fn pdf_url(&self) -> String {
        ptr_pdf_url(self.year, &self.doc_id)
    }

    /// Two-letter state from `StateDst` (e.g. `MO04` -> `MO`).
    pub fn state(&self) -> Option<String> {
        let s: String = self.state_district.chars().take(2).collect();
        (s.len() == 2 && s.chars().all(|c| c.is_ascii_uppercase())).then_some(s)
    }
}

#[derive(Deserialize)]
struct RawIndex {
    #[serde(rename = "Member", default)]
    members: Vec<RawMember>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawMember {
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    last: String,
    #[serde(default)]
    first: String,
    #[serde(default)]
    suffix: String,
    #[serde(default)]
    filing_type: String,
    #[serde(default)]
    state_dst: String,
    #[serde(default)]
    year: String,
    #[serde(default)]
    filing_date: String,
    #[serde(rename = "DocID", default)]
    doc_id: String,
}

/// Parse the XML index file found inside `{year}FD.zip`.
pub fn parse_index_xml(xml: &str) -> Result<Vec<IndexEntry>> {
    let xml = xml.trim_start_matches('\u{feff}');
    let raw: RawIndex = quick_xml::de::from_str(xml).context("parsing House FD index XML")?;
    Ok(raw
        .members
        .into_iter()
        .filter(|m| !m.doc_id.trim().is_empty())
        .map(|m| IndexEntry {
            doc_id: m.doc_id.trim().to_string(),
            prefix: m.prefix,
            first: m.first,
            last: m.last,
            suffix: m.suffix,
            filing_type: m.filing_type.trim().to_string(),
            state_district: m.state_dst.trim().to_string(),
            year: m.year.trim().parse().unwrap_or_default(),
            filing_date: parse_us_date(&m.filing_date),
        })
        .collect())
}

/// Extract and parse the XML index from the yearly ZIP archive.
pub fn parse_index_zip(bytes: &[u8]) -> Result<Vec<IndexEntry>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("opening House FD zip")?;
    let idx = (0..zip.len())
        .find(|&i| zip.by_index(i).is_ok_and(|f| f.name().is_ok_and(|n| n.to_ascii_lowercase().ends_with(".xml"))))
        .ok_or_else(|| anyhow!("no XML index in House FD zip"))?;
    let mut xml = String::new();
    zip.by_index(idx)?.read_to_string(&mut xml)?;
    parse_index_xml(&xml)
}

/// Result of parsing one PTR PDF.
#[derive(Debug, Clone, PartialEq)]
pub struct HousePtr {
    pub filer_name: Option<String>,
    pub status: Option<String>,
    pub state_district: Option<String>,
    pub signed_date: Option<NaiveDate>,
    pub trades: Vec<ParsedTrade>,
}

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("scanned paper filing (no text layer)")]
    Scanned,
    #[error("pdf text extraction failed: {0}")]
    Extract(String),
}

/// Extract text from a PDF. `pdf-extract` can panic on malformed input, so
/// the call is isolated.
pub fn pdf_text(bytes: &[u8]) -> Result<String, PdfError> {
    let owned = bytes.to_vec();
    let res = std::panic::catch_unwind(move || pdf_extract::extract_text_from_mem(&owned));
    match res {
        Ok(Ok(t)) => Ok(t.replace('\0', "")),
        Ok(Err(e)) => Err(PdfError::Extract(e.to_string())),
        Err(_) => Err(PdfError::Extract("panic in pdf parser".into())),
    }
}

/// Parse a PTR PDF. Returns [`PdfError::Scanned`] for image-only paper filings.
pub fn parse_ptr_pdf(bytes: &[u8]) -> Result<HousePtr, PdfError> {
    let text = pdf_text(bytes)?;
    let meaningful = text.chars().filter(|c| c.is_alphanumeric()).count();
    if meaningful < 40 {
        return Err(PdfError::Scanned);
    }
    Ok(parse_ptr_text(&text))
}

static ANCHOR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|\s)(S \(partial\)|P|S|E)\s+(\d{2}/\d{2}/\d{4})\s+(\d{2}/\d{2}/\d{4})(.*)$").unwrap()
});
static ASSET_TYPE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([A-Z]{2})\]\s*$").unwrap());
static OWNER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(SP|JT|DC)\s+").unwrap());
static META: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:[A-Z]\s*){1,3}:").unwrap());
static PAREN_TICKER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(([A-Za-z0-9./ ]{1,10})\)").unwrap());
static EXCHANGE_TICKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:NYSE|NASDAQ|NYSEARCA|NYSEAMERICAN|ARCA|BATS|CBOE)[A-Z]*:\s*([A-Z]{1,5}(?:\.[A-Z])?)\b").unwrap()
});

const HEADER_PREFIXES: [&str; 7] = [
    "Filing ID #",
    "ID Owner Asset",
    "Type Date Notification",
    "Date Amount Cap.",
    "Gains >",
    "$200?",
    "Clerk of the House",
];

fn is_header(line: &str) -> bool {
    HEADER_PREFIXES.iter().any(|p| line.starts_with(p))
}

fn owner_label(code: &str) -> &'static str {
    match code {
        "SP" => "Spouse",
        "JT" => "Joint",
        "DC" => "Dependent Child",
        _ => "Self",
    }
}

fn field<'a>(lines: &'a [&str], key: &str) -> Option<&'a str> {
    lines.iter().find_map(|l| l.strip_prefix(key)).map(str::trim).filter(|s| !s.is_empty())
}

/// Extract a ticker from an asset description, e.g. `Apple Inc. (AAPL) [ST]`
/// or `... NYSEARCA: DIA [OT]`. CUSIPs in parentheses are ignored.
pub fn ticker_from_asset(asset: &str) -> Option<String> {
    PAREN_TICKER
        .captures_iter(asset)
        .filter_map(|c| normalize_ticker(&c[1]))
        .last()
        .or_else(|| EXCHANGE_TICKER.captures(asset).and_then(|c| normalize_ticker(&c[1])))
}

/// Parse the reading-order text of an electronic PTR.
pub fn parse_ptr_text(text: &str) -> HousePtr {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let filer_name = field(&lines, "Name:").map(|n| n.trim_start_matches("Hon.").trim().to_string());
    let status = field(&lines, "Status:").map(str::to_string);
    let state_district = field(&lines, "State/District:").map(str::to_string);
    let signed_date = field(&lines, "Digitally Signed:").and_then(parse_us_date);

    let mut trades: Vec<ParsedTrade> = Vec::new();
    let mut pending: Vec<&str> = Vec::new();
    // Inside a metadata block (F S / S O / D / L / C) until the next blank line.
    let mut in_meta = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if is_header(line) {
            continue;
        }
        if line.is_empty() || line.starts_with('*') {
            pending.clear();
            in_meta = false;
            continue;
        }
        if META.is_match(line) {
            pending.clear();
            in_meta = true;
            if let (Some(desc), Some(last)) = (line.strip_prefix("D:"), trades.last_mut()) {
                last.comment = Some(desc.trim().to_string());
            } else if let (Some(desc), Some(last)) = (line.strip_prefix("C:"), trades.last_mut()) {
                last.comment.get_or_insert_with(String::new).push_str(desc.trim());
            }
            continue;
        }
        if in_meta {
            continue;
        }
        let Some(cap) = ANCHOR.captures(line) else {
            pending.push(line);
            continue;
        };
        let head = &line[..cap.get(0).unwrap().start()];
        let mut asset = pending.join(" ");
        if !head.trim().is_empty() {
            if !asset.is_empty() {
                asset.push(' ');
            }
            asset.push_str(head.trim());
        }
        pending.clear();

        let mut amount_text = cap[4].trim().to_string();
        let incomplete = amount_text.is_empty() || amount_text.ends_with('-') || amount_text.ends_with("Over");
        if incomplete && i < lines.len() && lines[i].starts_with('$') {
            amount_text.push(' ');
            amount_text.push_str(lines[i]);
            i += 1;
        }

        let asset_type = ASSET_TYPE.captures(&asset).map(|c| c[1].to_string());
        let mut name = ASSET_TYPE.replace(&asset, "").trim().to_string();
        let owner = OWNER.captures(&name).map(|c| owner_label(&c[1]).to_string());
        if owner.is_some() {
            name = OWNER.replace(&name, "").to_string();
        }
        let ticker = ticker_from_asset(&name);
        let (Some(tx_date), notification_date) = (parse_us_date(&cap[2]), parse_us_date(&cap[3])) else {
            continue;
        };
        trades.push(ParsedTrade {
            row: trades.len() as u32 + 1,
            owner: owner.or_else(|| Some("Self".into())),
            ticker,
            asset_name: name,
            asset_type,
            tx_type: TxType::parse(&cap[1]),
            tx_date,
            notification_date,
            amount: AmountRange::parse(&amount_text),
            comment: None,
        });
    }

    HousePtr { filer_name, status, state_district, signed_date, trades }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_handles_partial_sales() {
        let t = "Hdr\n\nAmazon.com, Inc. - Common Stock\n(AMZN) [ST] S (partial) 03/16/2026 03/16/2026 $1,001 - $15,000\n\nF S: New\nD: note\n";
        let p = parse_ptr_text(t);
        assert_eq!(p.trades.len(), 1);
        let tr = &p.trades[0];
        assert_eq!(tr.ticker.as_deref(), Some("AMZN"));
        assert_eq!(tr.tx_type, TxType::SalePartial);
        assert_eq!(tr.asset_type.as_deref(), Some("ST"));
        assert_eq!(tr.asset_name, "Amazon.com, Inc. - Common Stock (AMZN)");
        assert_eq!(tr.comment.as_deref(), Some("note"));
    }

    #[test]
    fn wrapped_amount_and_owner() {
        let t = "\nSP U.S. Treasury Note due 2/28/2029\n[GS] P 12/15/2025 12/16/2025 Spouse/DC Over\n$1,000,000\n\nF S: New\n";
        let p = parse_ptr_text(t);
        let tr = &p.trades[0];
        assert_eq!(tr.owner.as_deref(), Some("Spouse"));
        assert_eq!(tr.ticker, None);
        assert_eq!(tr.amount, Some(AmountRange { min: 1_000_001, max: None }));
        assert_eq!(tr.asset_name, "U.S. Treasury Note due 2/28/2029");
    }

    #[test]
    fn exchange_prefixed_ticker_and_cusip() {
        assert_eq!(
            ticker_from_asset("DIA - SPDR Dow Jones Indust Avg ETF Trust NYSEARCA: DIA").as_deref(),
            Some("DIA")
        );
        assert_eq!(ticker_from_asset("US Treasury 01/31/28 (91282CGH8)"), None);
        assert_eq!(ticker_from_asset("Berkshire Hathaway Inc. New Common Stock (BRK.B)").as_deref(), Some("BRK.B"));
    }
}
