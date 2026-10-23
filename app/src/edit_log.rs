//! Bounded chart edit history, independent of conversation persistence.

use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use tr_chart::drawings::Author;

#[derive(Debug, Clone, Serialize)]
pub struct EditEvent {
    pub timestamp_ms: i64,
    pub chart: u64,
    pub author: Author,
    pub turn_id: Option<String>,
    pub kind: String,
    pub value: serde_json::Value,
}

#[derive(Default)]
pub struct EditLog {
    events: VecDeque<EditEvent>,
    pub(crate) observed: HashMap<u64, serde_json::Value>,
}

impl EditLog {
    pub fn record(
        &mut self,
        chart: u64,
        author: Author,
        turn_id: Option<String>,
        kind: &str,
        value: serde_json::Value,
    ) {
        if self.events.len() == 200 {
            self.events.pop_front();
        }
        self.events.push_back(EditEvent {
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            chart,
            author,
            turn_id,
            kind: kind.into(),
            value,
        });
    }

    pub(crate) fn observe_set(
        &mut self,
        chart: u64,
        author: Author,
        turn: Option<String>,
        kind: &str,
        before: &serde_json::Value,
        after: &serde_json::Value,
    ) {
        let empty = Vec::new();
        let before = before.as_array().unwrap_or(&empty);
        let after = after.as_array().unwrap_or(&empty);
        let identity = |v: &serde_json::Value| if kind == "indicator" { v["chart"]["id"].clone() } else { v.clone() };
        for old in before {
            match after.iter().find(|new| identity(new) == identity(old)) {
                None => self.record(chart, author, turn.clone(), &format!("{kind}_remove"), old.clone()),
                Some(new) if new != old => {
                    self.record(chart, author, turn.clone(), &format!("{kind}_edit"), new.clone())
                }
                _ => {}
            }
        }
        for new in after {
            if !before.iter().any(|old| identity(old) == identity(new)) {
                self.record(chart, author, turn.clone(), &format!("{kind}_add"), new.clone());
            }
        }
    }

    pub fn newest(&self, limit: usize, user_only: bool) -> Vec<&EditEvent> {
        self.events.iter().rev().filter(|event| !user_only || event.author == Author::User).take(limit).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_is_bounded_newest_first_and_excludes_agent_actions() {
        let mut log = EditLog::default();
        for index in 0..250 {
            log.record(1, Author::User, None, "range", index.into());
        }
        log.record(1, Author::Agent, Some("turn".into()), "symbol", "AAA".into());
        assert_eq!(log.newest(500, false).len(), 200);
        assert_eq!(
            log.newest(2, true).iter().map(|e| e.value.clone()).collect::<Vec<_>>(),
            vec![serde_json::json!(249), serde_json::json!(248)]
        );
        assert_eq!(log.newest(1, false)[0].turn_id.as_deref(), Some("turn"));
        assert!(log.newest(1, false)[0].timestamp_ms > 0);
        assert!(log.newest(0, true).is_empty());
    }

    #[test]
    fn set_observation_distinguishes_indicator_and_comparison_edits() {
        let mut log = EditLog::default();
        let before = serde_json::json!([{ "chart": { "id": 1 }, "params": [2] }]);
        let after = serde_json::json!([{ "chart": { "id": 1 }, "params": [3] }, { "chart": { "id": 2 } }]);
        log.observe_set(1, Author::User, None, "indicator", &before, &after);
        log.observe_set(1, Author::User, None, "indicator", &after, &serde_json::json!([]));
        log.observe_set(1, Author::User, None, "comparison", &serde_json::json!(["AAA"]), &serde_json::json!(["BBB"]));
        let kinds: Vec<_> = log.newest(20, true).iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "comparison_add",
                "comparison_remove",
                "indicator_remove",
                "indicator_remove",
                "indicator_add",
                "indicator_edit"
            ]
        );
    }
}
