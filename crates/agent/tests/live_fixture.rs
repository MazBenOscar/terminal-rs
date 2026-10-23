//! Scrubbed transcript of the A0 live run (trend turn, canary, interrupt) against
//! Codex 0.163.0 signed in with ChatGPT. Guards against committing unscrubbed data.

use serde_json::Value;
use tr_agent::transcript::scrub_value;

const FIXTURE: &str = include_str!("fixtures/codex-0.163.0-live-trend-canary.jsonl");

fn lines() -> Vec<Value> {
    FIXTURE.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

#[test]
fn fixture_is_already_scrubbed() {
    for value in lines() {
        let mut again = value.clone();
        scrub_value(&mut again);
        assert_eq!(again, value, "fixture line changes when scrubbed again");
    }
}

#[test]
fn fixture_shows_direct_chart_tools_and_no_side_effects() {
    let lines = lines();
    let methods: Vec<&str> = lines.iter().filter_map(|line| line["method"].as_str()).collect();
    assert_eq!(methods.iter().filter(|method| **method == "item/tool/call").count(), 2);
    for line in &lines {
        assert_eq!(line["params"]["namespace"].as_str().unwrap_or("chart"), "chart");
        let kind = line["params"]["item"]["type"].as_str().unwrap_or_default();
        assert!(
            !matches!(kind, "commandExecution" | "fileChange" | "mcpToolCall" | "webSearch" | "imageGeneration"),
            "side-effecting item {kind}"
        );
        let method = line["method"].as_str().unwrap_or_default();
        assert!(!method.contains("requestApproval"), "approval request {method}");
    }
}
