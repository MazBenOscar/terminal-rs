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
    Cancelled,
    TimedOut,
    NotReady,
}
impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ToolAttempt => "Agent tried to use a tool — analysis stopped for safety",
            Self::NoAnswer => "Model returned no answer",
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
    loop {
        match events.recv().await {
            Some(AgentEvent::TextDelta(params)) => {
                let id = params["itemId"].as_str().unwrap_or_default();
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
