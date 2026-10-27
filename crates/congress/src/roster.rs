//! Member rosters used to resolve party affiliation, which neither disclosure
//! system publishes. Sources are the official chamber lists:
//! - House Clerk: <https://clerk.house.gov/xml/lists/MemberData.xml>
//! - Senate: historical fixtures only; live senate.gov access is permanently disabled.
//!   Keep the parser for stored history and a future third-party source.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::model::{Chamber, Party};

pub const HOUSE_ROSTER_URL: &str = "https://clerk.house.gov/xml/lists/MemberData.xml";
pub const SENATE_ROSTER_URL: &str = "https://www.senate.gov/general/contact_information/senators_cfm.xml";

#[derive(Debug, Clone, PartialEq)]
pub struct RosterMember {
    pub chamber: Chamber,
    /// Canonical display name (House `official-name`, Senate first + last).
    pub name: String,
    pub first: String,
    pub last: String,
    pub party: Party,
    pub state: String,
    /// House only, e.g. `MO04` (`AK00` for at-large seats).
    pub state_district: Option<String>,
    pub bioguide: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Roster {
    pub house: Vec<RosterMember>,
    pub senate: Vec<RosterMember>,
    by_district: HashMap<String, usize>,
}

/// Lowercase, strip punctuation, honorifics and generational suffixes.
pub fn normalize_name(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || c == ',')
        .map(|w| w.chars().filter(|c| c.is_alphanumeric() || *c == '-').collect::<String>().to_lowercase())
        .filter(|w| {
            !w.is_empty() && !matches!(w.as_str(), "jr" | "sr" | "ii" | "iii" | "iv" | "hon" | "the" | "honorable")
        })
        .collect()
}

impl Roster {
    pub fn new(house: Vec<RosterMember>, senate: Vec<RosterMember>) -> Self {
        let by_district =
            house.iter().enumerate().filter_map(|(i, m)| m.state_district.clone().map(|d| (d, i))).collect();
        Self { house, senate, by_district }
    }

    pub fn is_empty(&self) -> bool {
        self.house.is_empty() && self.senate.is_empty()
    }

    /// House members are keyed by seat, which both the filing index and roster carry.
    pub fn house_by_district(&self, state_district: &str) -> Option<&RosterMember> {
        self.by_district.get(state_district.trim()).map(|&i| &self.house[i])
    }

    /// Match on last name, using the first name to break ties.
    pub fn find_by_name(&self, chamber: Chamber, first: &str, last: &str) -> Option<&RosterMember> {
        let pool = match chamber {
            Chamber::House => &self.house,
            Chamber::Senate => &self.senate,
        };
        let want_last = normalize_name(last);
        let want_first = normalize_name(first);
        let matches: Vec<&RosterMember> = pool
            .iter()
            .filter(|m| {
                let have = normalize_name(&m.last);
                !want_last.is_empty() && (have == want_last || have.last() == want_last.last())
            })
            .collect();
        match matches.as_slice() {
            [one] => Some(one),
            [] => None,
            many => many
                .iter()
                .find(|m| {
                    let f = normalize_name(&m.first);
                    want_first.iter().any(|w| f.contains(w))
                })
                .copied(),
        }
    }
}

#[derive(Deserialize)]
struct HouseDoc {
    members: HouseMembers,
}
#[derive(Deserialize)]
struct HouseMembers {
    #[serde(rename = "member", default)]
    member: Vec<HouseMember>,
}
#[derive(Deserialize)]
struct HouseMember {
    statedistrict: String,
    #[serde(rename = "member-info")]
    info: HouseInfo,
}
#[derive(Deserialize)]
struct HouseInfo {
    #[serde(default)]
    lastname: String,
    #[serde(default)]
    firstname: String,
    #[serde(default)]
    party: String,
    #[serde(rename = "bioguideID", default)]
    bioguide: String,
    #[serde(rename = "official-name", default)]
    official_name: String,
}

/// Parse the House Clerk `MemberData.xml`. Vacant seats are skipped.
pub fn parse_house_roster(xml: &str) -> Result<Vec<RosterMember>> {
    let doc: HouseDoc = quick_xml::de::from_str(xml).context("parsing House MemberData.xml")?;
    Ok(doc
        .members
        .member
        .into_iter()
        .filter(|m| !m.info.lastname.trim().is_empty())
        .map(|m| RosterMember {
            chamber: Chamber::House,
            name: if m.info.official_name.trim().is_empty() {
                format!("{} {}", m.info.firstname.trim(), m.info.lastname.trim())
            } else {
                m.info.official_name.trim().to_string()
            },
            first: m.info.firstname.trim().to_string(),
            last: m.info.lastname.trim().to_string(),
            party: Party::from_code(&m.info.party),
            state: m.statedistrict.chars().take(2).collect(),
            state_district: Some(m.statedistrict.trim().to_string()),
            bioguide: Some(m.info.bioguide).filter(|b| !b.is_empty()),
        })
        .collect())
}

#[derive(Deserialize)]
struct SenateDoc {
    #[serde(rename = "member", default)]
    member: Vec<SenateMember>,
}
#[derive(Deserialize)]
struct SenateMember {
    #[serde(default)]
    last_name: String,
    #[serde(default)]
    first_name: String,
    #[serde(default)]
    party: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    bioguide_id: String,
}

/// Parse senate.gov `senators_cfm.xml`.
pub fn parse_senate_roster(xml: &str) -> Result<Vec<RosterMember>> {
    let doc: SenateDoc = quick_xml::de::from_str(xml).context("parsing senators_cfm.xml")?;
    Ok(doc
        .member
        .into_iter()
        .map(|m| RosterMember {
            chamber: Chamber::Senate,
            name: format!("{} {}", m.first_name.trim(), m.last_name.trim()),
            first: m.first_name.trim().to_string(),
            last: m.last_name.trim().to_string(),
            party: Party::from_code(&m.party),
            state: m.state.trim().to_string(),
            state_district: None,
            bioguide: Some(m.bioguide_id.trim().to_string()).filter(|b| !b.is_empty()),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_normalization() {
        assert_eq!(normalize_name("McConnell, Jr."), vec!["mcconnell"]);
        assert_eq!(normalize_name("Justice II"), vec!["justice"]);
        assert_eq!(normalize_name("Hon. Mark Alford"), vec!["mark", "alford"]);
    }
}
