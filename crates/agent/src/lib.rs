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
mod schema;
