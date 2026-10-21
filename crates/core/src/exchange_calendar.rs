//! Optional attributed exchange schedule. Missing dates inside explicit coverage are
//! closed; outside it, callers retain the documented built-in approximation.
use chrono::{NaiveDate, TimeZone};
use serde::Deserialize;
use std::{collections::BTreeMap, sync::OnceLock};
#[derive(Debug, Deserialize)]
pub struct Day {
    pub date: NaiveDate,
    pub open: u32,
    pub close: u32,
}
#[derive(Debug, Deserialize)]
struct File {
    start: NaiveDate,
    end: NaiveDate,
    source: String,
    sessions: Vec<Day>,
}
pub struct Calendar {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub source: String,
    sessions: BTreeMap<NaiveDate, Day>,
}
impl Calendar {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 8_388_608 {
            return Err("Exchange schedule exceeds 8 MiB".into());
        }
        let file: File = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if file.start > file.end
            || (file.end - file.start).num_days() > 150_000
            || file.source.is_empty()
            || file.source.len() > 500
            || file.sessions.len() > 110_000
        {
            return Err("Invalid exchange calendar coverage/source".into());
        }
        let mut sessions = BTreeMap::new();
        for day in file.sessions {
            if day.date < file.start
                || day.date > file.end
                || day.open >= day.close
                || day.close >= 1440
                || sessions.contains_key(&day.date)
                || [day.open, day.close].iter().any(|minute| {
                    let local = day.date.and_hms_opt(minute / 60, minute % 60, 0).unwrap();
                    chrono_tz::America::New_York.from_local_datetime(&local).single().is_none()
                })
            {
                return Err("Invalid/duplicate exchange session".into());
            }
            sessions.insert(day.date, day);
        }
        Ok(Self { start: file.start, end: file.end, source: file.source, sessions })
    }
    pub fn day(&self, date: NaiveDate) -> Option<Option<&Day>> {
        (date >= self.start && date <= self.end).then(|| self.sessions.get(&date))
    }
}
fn loaded() -> &'static Result<Option<Calendar>, String> {
    static CALENDAR: OnceLock<Result<Option<Calendar>, String>> = OnceLock::new();
    CALENDAR.get_or_init(|| {
        let Some(path) = std::env::var_os("TR_US_CALENDAR") else {
            return Ok(None);
        };
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        Calendar::parse(&bytes).map(Some)
    })
}
pub fn day(date: NaiveDate) -> Option<Option<&'static Day>> {
    loaded().as_ref().ok()?.as_ref()?.day(date)
}
pub fn description() -> String {
    match loaded() {
        Ok(Some(c)) => {
            format!("US schedule {} to {} · {}; built-in approximation outside coverage", c.start, c.end, c.source)
        }
        Ok(None) => "US calendar: recurring holidays and selected closures; full historical schedule not loaded".into(),
        Err(e) => format!("US calendar import failed: {e}; using built-in approximation"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_dst_gaps_and_ambiguous_times() {
        for (date, open, close) in
            [("2026-03-08", 150, 240), ("2026-03-08", 60, 150), ("2026-11-01", 90, 240), ("2026-01-01", 570, 570)]
        {
            let json = serde_json::json!({"start":date,"end":date,"source":"test","sessions":[{"date":date,"open":open,"close":close}]});
            assert!(Calendar::parse(&serde_json::to_vec(&json).unwrap()).is_err());
        }
    }
    #[test]
    fn explicit_coverage_handles_closures_late_opens_and_missing_coverage() {
        let json = br#"{"start":"2001-09-10","end":"2001-09-17","source":"test schedule","sessions":[{"date":"2001-09-10","open":570,"close":960},{"date":"2001-09-17","open":600,"close":960}]}"#;
        let calendar = Calendar::parse(json).unwrap();
        assert!(calendar.day("2001-09-11".parse().unwrap()).unwrap().is_none());
        assert_eq!(calendar.day("2001-09-17".parse().unwrap()).unwrap().unwrap().open, 600);
        assert!(calendar.day("2001-09-18".parse().unwrap()).is_none());
        assert!(Calendar::parse(br#"{"start":"2026-01-01","end":"2026-01-02","source":"x","sessions":[{"date":"2026-01-01","open":960,"close":570}]}"#).is_err());
    }
}
