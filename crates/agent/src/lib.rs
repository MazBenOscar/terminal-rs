#[cfg(test)]
extern crate self as tr_agent;

// Compile the actual example adapter into the bridge's offline unit harness;
// the sealed Connector boundary remains unchanged in production and tests.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../examples/agent_bridge.rs"]
mod sample_driver_tests;

pub mod acp;
pub mod bridge;
pub mod codex;
pub mod isolation;
pub mod mcp;
pub mod transcript;
pub mod transport;

use std::future::Future;

use anyhow::Result;
use serde_json::Value;

pub use codex::{
    AccountState, CodexAppServer, DynamicTool, Events, LaunchOptions, Login, Model, ReasoningOption, ThreadOptions,
    TurnInput,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendState {
    Missing,
    WrongVersion,
    NotLoggedIn,
    RateLimited,
    Ready,
    Unsafe,
}

#[derive(Clone, PartialEq)]
pub enum AgentEvent {
    TextDelta(Value),
    ReasoningDelta(Value),
    ToolStarted(Value),
    ToolFinished(Value),
    Usage(Value),
    RateLimits(Value),
    Done(Value),
    Notice(String),
    Error(String),
}

impl std::fmt::Debug for AgentEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (name, value) = match self {
            Self::TextDelta(value) => ("TextDelta", value),
            Self::ReasoningDelta(value) => ("ReasoningDelta", value),
            Self::ToolStarted(value) => ("ToolStarted", value),
            Self::ToolFinished(value) => ("ToolFinished", value),
            Self::Usage(value) => ("Usage", value),
            Self::RateLimits(value) => ("RateLimits", value),
            Self::Done(value) => ("Done", value),
            Self::Notice(message) => return f.debug_tuple("Notice").field(&transcript::scrub_text(message)).finish(),
            Self::Error(message) => return f.debug_tuple("Error").field(&transcript::scrub_text(message)).finish(),
        };
        let mut safe = value.clone();
        transcript::scrub_value(&mut safe);
        f.debug_tuple(name).field(&safe).finish()
    }
}

/// Story-only outcomes; never insert analysis data into an ordinary conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnalysisError {
    ToolAttempt,
    NoAnswer,
    /// NoAnswer caused by the bounded reply collector, not a confirmed turn end.
    NoAnswerTooLong,
    Cancelled,
    TimedOut,
    NotReady,
}
impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ToolAttempt => "Agent tried to use a tool — analysis stopped for safety",
            Self::NoAnswer => "Model returned no answer",
            Self::NoAnswerTooLong => "Model returned no answer — reply too long",
            Self::Cancelled => "Cancelled",
            Self::TimedOut => "Analysis timed out — Retry",
            Self::NotReady => "Agent not ready",
        })
    }
}

/// Collect only final-answer text. Callers own cancellation/retirement even on
/// timeout; the 90 second budget includes session creation and turn startup.
pub(crate) async fn analysis_reply(events: &mut impl AgentEvents) -> Result<String, AnalysisError> {
    let mut chunks: Vec<(String, String)> = Vec::new();
    let mut bytes = 0usize;
    const MAX_REPLY: usize = 64 * 1024;
    loop {
        match events.recv().await {
            Some(AgentEvent::TextDelta(params)) => {
                let id = params["itemId"].as_str().unwrap_or_default();
                let final_text = params["finalText"].as_str();
                let text = final_text.unwrap_or_else(|| params["delta"].as_str().unwrap_or_default());
                if text.is_empty() && final_text.is_none() {
                    continue;
                }
                let old = chunks.iter().find(|(key, _)| key == id).map_or(0, |(_, text)| text.len());
                let separator = usize::from(!chunks.is_empty() && !chunks.iter().any(|(key, _)| key == id));
                let next = bytes
                    .saturating_sub(if final_text.is_some() { old } else { 0 })
                    .saturating_add(text.len())
                    .saturating_add(separator);
                if next > MAX_REPLY {
                    return Err(AnalysisError::NoAnswerTooLong);
                }
                bytes = next;
                let index = chunks.iter().position(|(key, _)| key == id).unwrap_or_else(|| {
                    chunks.push((id.into(), String::new()));
                    chunks.len() - 1
                });
                if let Some(text) = params["finalText"].as_str() {
                    chunks[index].1 = text.into();
                } else {
                    chunks[index].1.push_str(params["delta"].as_str().unwrap_or_default());
                }
            }
            Some(AgentEvent::ToolStarted(_) | AgentEvent::ToolFinished(_)) => return Err(AnalysisError::ToolAttempt),
            Some(AgentEvent::Done(params)) => {
                let text = chunks.into_iter().map(|(_, text)| text).collect::<Vec<_>>().join("\n");
                if matches!(params["turn"]["status"].as_str(), Some("interrupted" | "cancelled"))
                    || params["turn"]["stopReason"] == "cancelled"
                {
                    return Err(AnalysisError::Cancelled);
                }
                if params["turn"]["status"] == "failed" || text.trim().is_empty() {
                    return Err(AnalysisError::NoAnswer);
                }
                return Ok(text);
            }
            Some(AgentEvent::Error(reason)) => {
                return Err(if reason.contains("tool") || reason.contains("unsafe") {
                    AnalysisError::ToolAttempt
                } else {
                    AnalysisError::NotReady
                });
            }
            None => return Err(AnalysisError::NotReady),
            _ => {}
        }
    }
}

/// Small FIFO tombstone set: late protocol traffic stays denied without growing
/// session metadata for the lifetime of the application.
pub(crate) struct BoundedIds<T>(std::collections::VecDeque<T>);
impl<T> Default for BoundedIds<T> {
    fn default() -> Self {
        Self(Default::default())
    }
}
impl<T: PartialEq> BoundedIds<T> {
    pub(crate) fn insert(&mut self, id: T) {
        self.remove(&id);
        self.0.push_back(id);
        if self.0.len() > 64 {
            self.0.pop_front();
        }
    }
    pub(crate) fn contains<Q: PartialEq + ?Sized>(&self, id: &Q) -> bool
    where
        T: std::borrow::Borrow<Q>,
    {
        self.0.iter().any(|entry| entry.borrow() == id)
    }
    pub(crate) fn remove<Q: PartialEq + ?Sized>(&mut self, id: &Q) -> bool
    where
        T: std::borrow::Borrow<Q>,
    {
        if let Some(index) = self.0.iter().position(|entry| entry.borrow() == id) {
            self.0.remove(index);
            true
        } else {
            false
        }
    }
}

pub trait AgentEvents: Send {
    fn recv(&mut self) -> impl Future<Output = Option<AgentEvent>> + Send;
}

pub trait AgentBackend: Send + Sync {
    type Events: AgentEvents;

    fn detect(&self) -> impl Future<Output = Result<BackendState>> + Send;
    fn list_models(&self) -> impl Future<Output = Result<Vec<Model>>> + Send;
    fn start_thread(&self, opts: ThreadOptions) -> impl Future<Output = Result<String>> + Send;
    fn send_turn(&self, input: TurnInput) -> impl Future<Output = Result<String>> + Send;
    fn interrupt(&self) -> impl Future<Output = Result<()>> + Send;
    fn events(&self) -> Self::Events;
}

#[cfg(test)]
mod analysis_tests {
    use super::*;
    use serde_json::json;
    struct Fixture(std::collections::VecDeque<AgentEvent>);
    impl AgentEvents for Fixture {
        async fn recv(&mut self) -> Option<AgentEvent> {
            self.0.pop_front()
        }
    }
    #[tokio::test]
    async fn final_text_deduplicates_stream_and_empty_or_cancelled_has_real_reason() {
        let mut events = Fixture(
            vec![
                AgentEvent::TextDelta(json!({"itemId":"a","delta":"partial"})),
                AgentEvent::TextDelta(json!({"itemId":"a","finalText":"Final answer"})),
                AgentEvent::Done(json!({"turn":{"status":"completed"}})),
            ]
            .into(),
        );
        assert_eq!(analysis_reply(&mut events).await, Ok("Final answer".into()));
        for (status, error) in [
            ("completed", AnalysisError::NoAnswer),
            ("failed", AnalysisError::NoAnswer),
            ("cancelled", AnalysisError::Cancelled),
        ] {
            let mut events = Fixture(vec![AgentEvent::Done(json!({"turn":{"status":status}}))].into());
            assert_eq!(analysis_reply(&mut events).await, Err(error));
        }
    }
    #[tokio::test]
    async fn analysis_text_budget_bounds_deltas_finals_and_item_separators() {
        for updates in [
            vec![json!({"delta":"x".repeat(65537)})],
            vec![json!({"delta":"x".repeat(65536)}), json!({"delta":"y"})],
            vec![json!({"finalText":"x".repeat(65537)})],
            vec![json!({"itemId":"a","delta":"x".repeat(65536)}), json!({"itemId":"b","delta":"y"})],
        ] {
            let mut events = Fixture(updates.into_iter().map(AgentEvent::TextDelta).collect());
            assert_eq!(analysis_reply(&mut events).await, Err(AnalysisError::NoAnswerTooLong));
        }
        let mut events = Fixture(
            vec![
                AgentEvent::TextDelta(json!({"delta":"x".repeat(65536)})),
                AgentEvent::TextDelta(json!({"finalText":"Final"})),
                AgentEvent::Done(json!({"turn":{"status":"completed"}})),
            ]
            .into(),
        );
        assert_eq!(analysis_reply(&mut events).await, Ok("Final".into()));
        let mut events = Fixture(
            vec![
                AgentEvent::TextDelta(json!({"delta":"discard"})),
                AgentEvent::TextDelta(json!({"finalText":""})),
                AgentEvent::Done(json!({"turn":{"status":"completed"}})),
            ]
            .into(),
        );
        assert_eq!(analysis_reply(&mut events).await, Err(AnalysisError::NoAnswer));
        assert!(AnalysisError::NoAnswerTooLong.to_string().contains("reply too long"));
    }
    #[test]
    fn tombstones_keep_only_the_last_64_unique_ids() {
        let mut ids = BoundedIds::default();
        for id in 0..1000 {
            ids.insert(id);
        }
        assert_eq!(ids.0.len(), 64);
        assert!(!ids.contains(&935));
        assert!(ids.contains(&936));
        ids.insert(936);
        ids.insert(1000);
        assert!(ids.contains(&936));
        assert!(!ids.contains(&937));
    }
    #[test]
    fn error_copy_is_specific_and_actionable() {
        for (error, text) in [
            (AnalysisError::ToolAttempt, "Agent tried to use a tool — analysis stopped for safety"),
            (AnalysisError::NoAnswer, "Model returned no answer"),
            (AnalysisError::Cancelled, "Cancelled"),
            (AnalysisError::TimedOut, "Analysis timed out — Retry"),
            (AnalysisError::NotReady, "Agent not ready"),
        ] {
            assert_eq!(error.to_string(), text);
        }
    }
}

#[cfg(test)]
mod schema;
