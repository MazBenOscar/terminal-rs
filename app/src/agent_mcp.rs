//! External clients use exactly the in-app registry executor and UI queue.
use crate::agent_bridge::AppExecutor;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tr_agent::bridge::{ChartPin, ToolCall, ToolExecutor};
use tr_agent::mcp::AppSocket;
use tr_agent_tools::{CallBudget, Registry, ToolSet};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backend {
    #[default]
    Codex,
    #[serde(rename = "mimo")]
    Mimo,
}
impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Mimo => "mimo",
        }
    }
}
pub use tr_agent::acp::MimoProfile;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub allow_external: bool,
    pub allow_external_edits: bool,
    pub backend: Backend,
    pub mimo_profile: MimoProfile,
    pub mimo_load_plugins: bool,
}

impl Settings {
    pub fn session_tools(&self, codex: ToolSet) -> ToolSet {
        match self.backend {
            Backend::Codex => codex,
            Backend::Mimo if self.allow_external_edits => ToolSet::Workflows,
            Backend::Mimo => ToolSet::ReadOnly,
        }
    }
}

pub struct ExternalServer {
    // Drop the listener/connections before the shared executor/pin.
    _socket: AppSocket,
    pub pin: Arc<Mutex<Option<ChartPin>>>,
}
struct Connection {
    executor: AppExecutor,
    turn: String,
    pin: Option<ChartPin>,
    pinned: bool,
    text_bytes: usize,
    shared_pin: Arc<Mutex<Option<ChartPin>>>,
    budget: CallBudget,
    started: Instant,
    limit: usize,
    generation: usize,
}
impl Connection {
    fn result(&mut self, result: tr_agent_tools::ToolResult) -> serde_json::Value {
        let bytes: usize = result
            .content
            .iter()
            .map(|part| match part {
                tr_agent_tools::Content::Text(text) => text.len(),
                tr_agent_tools::Content::ImagePng(_) => 0,
            })
            .sum();
        if bytes > tr_agent::bridge::MAX_TURN_TEXT_BYTES.saturating_sub(self.text_bytes) {
            self.text_bytes = tr_agent::bridge::MAX_TURN_TEXT_BYTES;
            return tr_agent::mcp::tool_error("Per-turn tool text budget exhausted");
        }
        self.text_bytes += bytes;
        tr_agent::mcp::tool_result(result)
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.executor.cancel(&self.turn);
    }
}
impl ExternalServer {
    pub fn start(
        data: &std::path::Path,
        executor: AppExecutor,
        pin: Option<ChartPin>,
        edit: bool,
    ) -> anyhow::Result<Self> {
        let pin = Arc::new(Mutex::new(pin));
        let shared_pin = pin.clone();
        let registry = Arc::new(Registry::new(if edit { ToolSet::Workflows } else { ToolSet::ReadOnly }));
        let factory = Arc::new(move || {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let registry = registry.clone();
            let state = Arc::new(tokio::sync::Mutex::new(Connection {
                executor: executor.clone(),
                turn: format!("external-{id}:0"),
                pin: None,
                pinned: false,
                text_bytes: 0,
                shared_pin: shared_pin.clone(),
                budget: CallBudget::new(if edit { 16 } else { 8 }),
                started: Instant::now(),
                limit: if edit { 16 } else { 8 },
                generation: 0,
            }));
            let handler: tr_agent::transport::ToolHandler = Arc::new(move |request| {
                let registry = registry.clone();
                let state = state.clone();
                Box::pin(async move {
                    if request["method"] == "tools/list" {
                        let mut schemas = tr_agent::mcp::schemas(&registry);
                        schemas["tools"].as_array_mut().unwrap().push(serde_json::json!({
                            "name":"read_proposal_outcomes",
                            "description":"Read this connection's proposal decisions (also pending/expired/undone). Latest 32 by default; use proposal_id for an older proposal. Repeat safely; outcomes are not consumed.",
                            "inputSchema":{"type":"object","properties":{"proposal_id":{"type":"integer","minimum":1}},"additionalProperties":false},
                            "annotations":{"readOnlyHint":true,"destructiveHint":false}
                        }));
                        return Ok(schemas);
                    }
                    let Some(name) = request["params"]["name"].as_str() else {
                        return Ok(tr_agent::mcp::tool_error("Invalid tool name"));
                    };
                    let arguments = request["params"]
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}))
                        .to_string();
                    let mut state = state.lock().await;
                    if state.started.elapsed() >= Duration::from_secs(60) {
                        state.executor.cancel(&state.turn);
                        state.generation += 1;
                        state.turn =
                            format!("{}:{}", state.turn.split(':').next().unwrap_or("external"), state.generation);
                        state.started = Instant::now();
                        state.budget = CallBudget::new(state.limit);
                        state.pin = None;
                        state.pinned = false;
                        state.text_bytes = 0;
                    }
                    if !state.budget.take() {
                        return Ok(tr_agent::mcp::tool_error(
                            "Per-turn tool budget exhausted; external connection turns last 60 seconds",
                        ));
                    }
                    if name == "read_proposal_outcomes" {
                        #[derive(Deserialize)]
                        #[serde(deny_unknown_fields)]
                        struct Args {
                            proposal_id: Option<u64>,
                        }
                        let args = match serde_json::from_str::<Args>(&arguments) {
                            Ok(args) if args.proposal_id != Some(0) => args,
                            _ => return Ok(tr_agent::mcp::tool_error("Expected optional positive proposal_id")),
                        };
                        let result = state.executor.external_outcomes(&state.turn, args.proposal_id).await;
                        return Ok(state.result(result));
                    }
                    if let Err(error) = registry.validate_call(name, &arguments) {
                        return Ok(tr_agent::mcp::tool_result(error));
                    }
                    if state.text_bytes >= tr_agent::bridge::MAX_TURN_TEXT_BYTES {
                        return Ok(tr_agent::mcp::tool_error("Per-turn tool text budget exhausted"));
                    }
                    if !state.pinned {
                        let pin = state.shared_pin.lock().unwrap().clone();
                        state.pin = pin;
                        state.pinned = true;
                    }
                    // Application-scoped tools (watchlists/news/scans) work
                    // without a chart, just as they do in an in-app turn.
                    let pin = state.pin.clone().unwrap_or(ChartPin {
                        uid: 0,
                        symbol: String::new(),
                        timeframe: String::new(),
                        visible_range: None,
                    });
                    let result = state
                        .executor
                        .execute(
                            registry,
                            ToolCall { turn: state.turn.clone(), pin, name: name.into(), arguments, confirmed: false },
                        )
                        .await;
                    Ok(state.result(result))
                })
            });
            handler
        });
        let socket = AppSocket::bind(data, factory)?;
        Ok(Self { _socket: socket, pin })
    }
}
