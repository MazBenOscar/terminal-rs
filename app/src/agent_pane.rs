//! One app-wide conversation; extra panes explicitly point to the first Agent pane.
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use egui::{RichText, Ui};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tr_agent::bridge::{BridgeEvent, Recovery};
use tr_agent::codex::Model;
use tr_agent::transcript::{scrub_text, scrub_value};
use tr_agent::transport::WireEvent;
use tr_agent_tools::ToolSet;

use crate::agent_markdown::{self, Block};

pub const STREAM_INTERVAL: Duration = Duration::from_millis(33);
pub const UNDO_UNAVAILABLE: &str = "undo for this agent turn is no longer available (layout changed/app restarted)";
const PRIVACY: &str = "Your prompts, the pinned chart's data and tool results leave this machine and go to OpenAI under your ChatGPT account. In Can edit charts mode the agent can add, change and remove its own drawings, indicators and comparisons on the pinned chart; they are badged Agent and undoable. It cannot edit your own or kept objects. Screenshots require separate consent; capture is currently unavailable. Codex stores local conversation rollouts for recovery (7 days / newest 50 bridge threads). Layouts contain choices only, never conversation or sign-in details.";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolsMode {
    #[default]
    ReadOnly,
    Edit,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub model: String,
    pub effort: String,
    pub tools: ToolsMode,
    pub pinned_chart: Option<u64>,
}
impl ToolsMode {
    fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read only",
            Self::Edit => "Can edit charts",
        }
    }
}

impl Settings {
    pub fn tool_set(&self) -> ToolSet {
        self.tool_set_if_available(crate::agent_bridge::agent_edit_tools_available())
    }

    fn tool_set_if_available(&self, available: bool) -> ToolSet {
        if available && self.tools == ToolsMode::Edit { ToolSet::Edit } else { ToolSet::ReadOnly }
    }

    pub fn select_defaults(&mut self, models: &[Model]) {
        let Some(model) = models.iter().find(|m| m.id == self.model).or_else(|| models.first()) else { return };
        self.model.clone_from(&model.id);
        if !model
            .supported_reasoning_efforts
            .iter()
            .any(|e| e.reasoning_effort == self.effort && e.reasoning_effort != "ultra")
        {
            self.effort = model
                .safe_default_effort()
                .filter(|default| {
                    model
                        .supported_reasoning_efforts
                        .iter()
                        .any(|e| e.reasoning_effort == *default && e.reasoning_effort != "ultra")
                })
                .or_else(|| {
                    model
                        .supported_reasoning_efforts
                        .iter()
                        .find(|e| e.reasoning_effort != "ultra")
                        .map(|e| e.reasoning_effort.as_str())
                })
                .unwrap_or_default()
                .into();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Onboarding {
    Dormant,
    Checking,
    Missing,
    Unsupported { found: String },
    SignedOut,
    SigningIn,
    Ready,
    RateLimited { reset: String },
    ToolsUnavailable { reason: String },
    Stopped { reason: String, unsafe_backend: bool },
}
impl Onboarding {
    pub fn primary(&self) -> &'static str {
        match self {
            Self::Dormant => "Start agent",
            Self::Checking => "Checking…",
            Self::Missing | Self::Unsupported { .. } => "Check again",
            Self::SignedOut => "Sign in with ChatGPT",
            Self::SigningIn => "Cancel",
            Self::Ready => "Send",
            Self::RateLimited { .. } => "Check limits",
            Self::ToolsUnavailable { .. } => "Retry",
            Self::Stopped { unsafe_backend: true, .. } => "Close agent",
            Self::Stopped { unsafe_backend: false, .. } => "Restart agent",
        }
    }
    fn description(&self) -> String {
        match self {
            Self::Dormant => "Use the isolated local Codex backend with your ChatGPT account.".into(),
            Self::Checking => "Checking isolated Codex, account and chart-tool readiness…".into(),
            Self::Missing => {
                "Codex is not installed. Install Codex CLI from developers.openai.com/codex/cli, then check again."
                    .into()
            }
            Self::Unsupported { found } => {
                format!("Unsupported Codex. Found: {found}. Tested: codex-cli 0.163.0. No agent was started.")
            }
            Self::SignedOut => "Sign in with your ChatGPT account. API-key accounts are not supported.".into(),
            Self::SigningIn => "Waiting for ChatGPT sign-in in your browser…".into(),
            Self::Ready => "Ready".into(),
            Self::RateLimited { reset } => format!("Rate-limited. Resets {reset}. Queued prompts are paused."),
            Self::ToolsUnavailable { reason } => format!("Chart tools unavailable: {reason}"),
            Self::Stopped { reason, unsafe_backend } => format!(
                "Agent {}: {reason}. No prompt will be replayed. {}",
                if *unsafe_backend { "unsafe" } else { "stopped" },
                if *unsafe_backend {
                    "Safety state cannot be cleared in this pane."
                } else {
                    "Use Restart agent to create a new isolated session; the queue stays paused."
                }
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub text: String,
    pub requires_tools: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trace {
    id: String,
    pub name: String,
    pub success: Option<bool>,
    pub duration_ms: Option<u64>,
    pub arguments: String,
    pub summary: Option<String>,
    pub interrupted: bool,
}

/// Local conversation metadata, never part of Settings or a layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationTurn {
    pub id: Option<String>,
    pub prompt_index: usize,
    pub finished: bool,
    pub made_edits: bool,
    pub undo_available: bool,
    pub undo_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderedBlock {
    Prompt(String),
    Reply(Vec<Block>),
    Tool(Trace),
    Notice(String),
}

#[derive(Default)]
pub struct Coalescer {
    last: Option<Instant>,
    dirty: bool,
}
impl Coalescer {
    pub fn changed(&mut self) {
        self.dirty = true;
    }
    pub fn due_in(&self, now: Instant, visible: bool) -> Option<Duration> {
        if !visible || !self.dirty {
            return None;
        }
        Some(
            self.last
                .map_or(Duration::ZERO, |last| STREAM_INTERVAL.saturating_sub(now.saturating_duration_since(last))),
        )
    }
    fn flush(&mut self, now: Instant) {
        self.dirty = false;
        self.last = Some(now);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub id: String,
    pub used: f32,
    pub reset: String,
    pub minutes: u64,
}
fn reset_time(timestamp: Option<i64>) -> String {
    timestamp
        .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
        .map(|t| t.with_timezone(&chrono::Local).format("%b %d %H:%M %Z").to_string())
        .unwrap_or_else(|| "time unavailable".into())
}
pub fn limits(value: &Value) -> Vec<Limit> {
    let snapshots = std::iter::once(("ordinary", &value["rateLimits"])).chain(
        value["rateLimitsByLimitId"].as_object().into_iter().flat_map(|m| m.iter().map(|(id, v)| (id.as_str(), v))),
    );
    let mut limits = Vec::new();
    for (id, snapshot) in snapshots {
        for key in ["primary", "secondary"] {
            let window = &snapshot[key];
            if let Some(used) = window["usedPercent"].as_f64().filter(|n| n.is_finite()) {
                let limit = Limit {
                    id: scrub_text(snapshot["limitId"].as_str().unwrap_or(id)).chars().take(80).collect(),
                    used: used.clamp(0.0, 100.0) as f32,
                    reset: reset_time(window["resetsAt"].as_i64()),
                    minutes: window["windowDurationMins"].as_u64().unwrap_or(0),
                };
                if !limits.contains(&limit) {
                    limits.push(limit);
                }
            }
        }
    }
    limits
}

/// Only the ordinary snapshot and an explicitly matching model limit govern use.
/// Unknown per-limit ids remain visible, but cannot disable an unrelated model.
pub fn blocking_reset(value: &Value, model: &str) -> Option<String> {
    let snapshots = std::iter::once(&value["rateLimits"]).chain(
        value["rateLimitsByLimitId"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.iter())
            .filter(|(id, snapshot)| {
                !model.is_empty()
                    && (id.as_str() == model
                        || snapshot["normalModelSlug"].as_str() == Some(model)
                        || snapshot["limitId"].as_str() == Some(model))
            })
            .map(|(_, snapshot)| snapshot),
    );
    let resets: Vec<_> = snapshots
        .flat_map(|snapshot| ["primary", "secondary"].map(|key| &snapshot[key]))
        .filter(|window| window["usedPercent"].as_f64().is_some_and(|used| used.is_finite() && used >= 100.0))
        .map(|window| reset_time(window["resetsAt"].as_i64()))
        .collect();
    if value["ordinaryUsageAllowed"] == false || !resets.is_empty() {
        Some(if resets.is_empty() { "time unavailable".into() } else { resets.join(", ") })
    } else {
        None
    }
}

struct ReplyMessage {
    index: usize,
    text: String,
    dirty: bool,
}

pub struct AgentPane {
    pub settings: Settings,
    pub state: Onboarding,
    pub models: Vec<Model>,
    pub limits: Vec<Limit>,
    pub blocks: Vec<RenderedBlock>,
    pub queue: VecDeque<Prompt>,
    pub last_prompt: Option<Prompt>,
    pub running: bool,
    pub input: String,
    pub disclosure: bool,
    pub acknowledged: bool,
    pub initialized: bool,
    pub active_chart: Option<u64>,
    pub session: Option<crate::agent_session::Session>,
    pub session_tools: Option<ToolSet>,
    pub turns: Vec<ConversationTurn>,
    active_turn: Option<usize>,
    header_undo: Option<String>,
    header_undo_available: bool,
    replies: HashMap<String, ReplyMessage>,
    reply_cache: HashMap<usize, Vec<agent_markdown::DisplayBlock>>,
    limits_value: Value,
    coalescer: Coalescer,
    queue_paused: bool,
}
impl Default for AgentPane {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            state: Onboarding::Dormant,
            models: Vec::new(),
            limits: Vec::new(),
            blocks: Vec::new(),
            queue: VecDeque::new(),
            last_prompt: None,
            running: false,
            input: String::new(),
            disclosure: true,
            acknowledged: false,
            initialized: false,
            active_chart: None,
            session: None,
            session_tools: None,
            turns: Vec::new(),
            active_turn: None,
            header_undo: None,
            header_undo_available: false,
            replies: HashMap::new(),
            reply_cache: HashMap::new(),
            limits_value: Value::Null,
            coalescer: Coalescer::default(),
            queue_paused: false,
        }
    }
}

pub enum Action {
    Check,
    SignIn,
    CancelLogin,
    ReopenLogin,
    RefreshLimits,
    Interrupt,
    Retry,
    Send(Prompt),
    SetTools(ToolsMode),
    UndoTurn(String),
    Pin,
    Close,
}

impl AgentPane {
    pub fn tool_set(&self) -> ToolSet {
        self.settings.tool_set()
    }

    pub fn can_change_tools(&self, mode: ToolsMode, available: bool) -> bool {
        !self.running
            && !matches!(
                self.state,
                Onboarding::Checking | Onboarding::SigningIn | Onboarding::Stopped { unsafe_backend: true, .. }
            )
            && (mode == ToolsMode::ReadOnly || available)
    }

    pub fn select_tools(&mut self, mode: ToolsMode, available: bool) -> bool {
        if self.settings.tools == mode || !self.can_change_tools(mode, available) {
            return false;
        }
        self.settings.tools = mode;
        self.queue_paused = true;
        self.notice(&format!(
            "Tools mode: {}. Applies to the next isolated session start; an open session will restart. Queue stays paused; no prompt is resent.",
            mode.label()
        ));
        true
    }

    /// Freeze the registry for this session. Every replacement uses normal onboarding.
    pub fn prepare_session_start(&mut self) -> Option<ToolSet> {
        if self.running || matches!(self.state, Onboarding::Stopped { unsafe_backend: true, .. }) {
            return None;
        }
        self.queue_paused |= self.session_tools.is_some();
        self.models.clear();
        self.session_tools = Some(self.tool_set());
        self.transition(Onboarding::Checking);
        self.session_tools
    }

    pub fn refresh_undo(&mut self, available: &[String], last: Option<&str>) {
        for i in 0..self.turns.len() {
            let end = self.turns.get(i + 1).map_or(self.blocks.len(), |turn| turn.prompt_index);
            let turn = &mut self.turns[i];
            let retained = turn.id.as_ref().is_some_and(|id| available.contains(id));
            turn.made_edits |= retained || self.blocks[turn.prompt_index + 1..end].iter().any(|block| {
                matches!(block, RenderedBlock::Tool(trace) if is_edit_tool(&trace.name) && trace.success == Some(true))
            });
            turn.undo_available = retained && turn.finished;
        }
        self.header_undo = last.map(str::to_owned).or_else(|| {
            self.turns.iter().rev().find(|turn| turn.finished && turn.made_edits).and_then(|turn| turn.id.clone())
        });
        self.header_undo_available = self.header_undo.as_ref().is_some_and(|id| available.contains(id));
    }

    pub fn undo_result(&mut self, id: &str, result: Result<crate::agent_bridge::UndoSummary, String>) {
        let text = match result {
            Ok(summary) => format!(
                "Undo: {} reverted · {} conflicts skipped · {} charts",
                summary.reverted,
                summary.skipped.len(),
                summary.charts
            ),
            Err(reason) => scrub_text(&reason),
        };
        if let Some(turn) = self.turns.iter_mut().find(|turn| turn.id.as_deref() == Some(id)) {
            turn.undo_summary = Some(text);
        } else {
            self.notice(&text);
        }
    }

    fn finish_turn(&mut self) {
        if let Some(index) = self.active_turn.take() {
            self.turns[index].finished = true;
        }
    }

    pub fn notice(&mut self, text: &str) {
        self.blocks.push(RenderedBlock::Notice(scrub_text(text)));
    }

    pub fn transition(&mut self, state: Onboarding) {
        if matches!(self.state, Onboarding::Stopped { unsafe_backend: true, .. }) {
            return;
        }
        if !matches!(state, Onboarding::Ready | Onboarding::SigningIn | Onboarding::Checking) {
            self.queue_paused = true;
        }
        if matches!(state, Onboarding::Stopped { .. }) {
            self.flush(Instant::now());
            self.running = false;
            self.interrupt_tools();
            self.finish_turn();
        }
        self.state = state;
    }

    pub fn interrupt(&mut self) {
        self.queue_paused = true;
        self.interrupt_tools();
    }

    pub fn interrupt_tools(&mut self) {
        for block in &mut self.blocks {
            if let RenderedBlock::Tool(trace) = block
                && trace.success.is_none()
            {
                trace.interrupted = true;
            }
        }
    }

    pub fn enqueue(&mut self, prompt: Prompt) {
        if !prompt.text.trim().is_empty() && self.queue.len() < 32 {
            // Conversation text is not a log: preserve whitespace and ordinary words.
            // While paused, new prompts append without releasing the existing queue.
            self.queue.push_back(prompt);
        }
    }

    pub fn next_prompt(&mut self) -> Option<Prompt> {
        if self.running || self.queue_paused || self.state != Onboarding::Ready {
            return None;
        }
        let prompt = self.queue.pop_front()?;
        self.start_prompt(prompt)
    }

    /// Explicit retry sends just that prompt; it does not release the paused FIFO.
    pub fn start_prompt(&mut self, prompt: Prompt) -> Option<Prompt> {
        if self.running || self.state != Onboarding::Ready || prompt.text.trim().is_empty() {
            return None;
        }
        self.flush(Instant::now());
        self.replies.clear();
        self.last_prompt = Some(prompt.clone());
        self.active_turn = Some(self.turns.len());
        self.turns.push(ConversationTurn {
            id: None,
            prompt_index: self.blocks.len(),
            finished: false,
            made_edits: false,
            undo_available: false,
            undo_summary: None,
        });
        self.blocks.push(RenderedBlock::Prompt(prompt.text.clone()));
        self.running = true;
        Some(prompt)
    }

    pub fn resume_queue(&mut self) {
        self.queue_paused = false;
    }

    pub fn set_limits(&mut self, value: &Value) {
        self.limits_value = crate::agent_session::visible_limits(value);
        self.limits = limits(value);
        let model = self
            .models
            .iter()
            .find(|m| m.id == self.settings.model)
            .map_or(self.settings.model.as_str(), |m| m.model.as_str());
        if let Some(reset) = blocking_reset(value, model) {
            self.transition(Onboarding::RateLimited { reset });
        } else if matches!(self.state, Onboarding::RateLimited { .. }) {
            self.transition(Onboarding::Ready);
        }
    }

    pub fn event(&mut self, event: BridgeEvent) {
        match event {
            BridgeEvent::Wire(WireEvent::Notification { method, params }) => self.wire(&method, &params),
            BridgeEvent::Wire(WireEvent::Error(reason)) => self.notice(&reason),
            BridgeEvent::Started { turn } => {
                if let Some(index) = self.active_turn {
                    self.turns[index].id.get_or_insert(turn);
                }
            }
            BridgeEvent::Completed { turn, status, .. } => {
                // Completion also supplies the canonical id if Started was absent.
                if let Some(index) = self.active_turn {
                    self.turns[index].id.get_or_insert(turn);
                }
                self.finish_turn();
                self.flush(Instant::now());
                self.running = false;
                if status != "completed" {
                    self.interrupt_tools();
                    self.queue_paused = true;
                    self.notice(&format!("Turn {status}. Reply is provisional; retry only on request."));
                }
            }
            BridgeEvent::ToolsUnavailable { reason } => {
                self.finish_turn();
                self.flush(Instant::now());
                self.running = false;
                self.notice(&format!("Tools unavailable: {reason}. Reply is provisional."));
                self.transition(Onboarding::ToolsUnavailable { reason: scrub_text(&reason) });
            }
            BridgeEvent::Recovered(recovery) => {
                self.finish_turn();
                self.flush(Instant::now());
                self.running = false;
                self.interrupt_tools();
                self.queue_paused = true;
                self.notice(&match recovery {
                    Recovery::Resumed => "Recovered agent session. Interrupted prompt was not resent; queue paused.".into(),
                    Recovery::HistoryLost { reason } => format!("Recovered with history lost: {reason}. Previous messages below are local only; nothing was resent."),
                });
            }
        }
    }

    fn message(&mut self, id: &str) -> &mut ReplyMessage {
        self.replies.entry(id.to_owned()).or_insert_with(|| {
            // Reserve the row immediately, preserving event order even if
            // a tool event arrives before the next 33 ms text flush.
            let index = self.blocks.len();
            self.blocks.push(RenderedBlock::Reply(Vec::new()));
            ReplyMessage { index, text: String::new(), dirty: false }
        })
    }

    pub fn wire(&mut self, method: &str, params: &Value) {
        match method {
            "item/started" | "item/completed" if params["item"]["type"] == "agentMessage" => {
                let item = &params["item"];
                let message = self.message(item["id"].as_str().unwrap_or_default());
                if let Some(text) = item["text"].as_str()
                    && (method == "item/completed" || message.text.is_empty())
                    && message.text != text
                {
                    message.text = text.to_owned();
                    message.dirty = true;
                    self.coalescer.changed();
                }
            }
            "item/agentMessage/delta" => {
                if let Some(delta) = params["delta"].as_str() {
                    let message = self.message(params["itemId"].as_str().unwrap_or_default());
                    message.text.push_str(delta);
                    message.dirty = true;
                    self.coalescer.changed();
                }
            }
            "item/completed" | "item/started" if params["item"]["type"] == "dynamicToolCall" => {
                let item = &params["item"];
                let name = item["tool"].as_str().unwrap_or("chart tool");
                let summary = item["editSummary"].as_str().map(short_label).or_else(|| edit_tool_summary(item));
                let trace = Trace {
                    id: scrub_text(item["id"].as_str().unwrap_or_default()),
                    name: scrub_text(name),
                    success: item["success"].as_bool(),
                    duration_ms: item["durationMs"].as_u64(),
                    arguments: if is_edit_tool(name) {
                        summary.clone().unwrap_or_default()
                    } else {
                        safe_arguments(&item["arguments"])
                    },
                    summary,
                    interrupted: false,
                };
                let start = self.active_turn.map_or(0, |index| self.turns[index].prompt_index + 1);
                if let Some(RenderedBlock::Tool(old)) = self.blocks[start..]
                    .iter_mut()
                    .rev()
                    .find(|b| matches!(b, RenderedBlock::Tool(old) if old.id == trace.id && old.success.is_none()))
                {
                    *old = trace;
                } else {
                    self.blocks.push(RenderedBlock::Tool(trace));
                }
            }
            "account/rateLimits/updated" => self.set_limits(params),
            "error" => {
                if let Some(reason) = params["error"]["message"].as_str() {
                    self.notice(reason);
                }
            }
            _ => {}
        }
    }

    fn flush(&mut self, now: Instant) {
        if !self.coalescer.dirty {
            return;
        }
        for message in self.replies.values_mut().filter(|message| message.dirty) {
            let blocks = agent_markdown::parse(&message.text);
            self.reply_cache.insert(message.index, agent_markdown::prepare(&blocks));
            self.blocks[message.index] = RenderedBlock::Reply(blocks);
            message.dirty = false;
        }
        self.coalescer.flush(now);
    }

    pub fn show(&mut self, ui: &mut Ui, pin_label: &str, pane_id: egui::Id) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some(delay) = self.coalescer.due_in(Instant::now(), true) {
            if delay.is_zero() {
                self.flush(Instant::now());
            } else {
                ui.ctx().request_repaint_after(delay);
            }
        }
        let old_model = self.settings.model.clone();
        self.header(ui, &mut actions);
        if old_model != self.settings.model {
            self.set_limits(&self.limits_value.clone());
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(pin_label);
            if ui.small_button("Pin active chart").clicked() {
                actions.push(Action::Pin);
            }
            if ui.small_button("Privacy info").clicked() {
                self.disclosure = !self.disclosure;
            }
        });
        if self.disclosure {
            ui.label(PRIVACY);
        }
        if !self.acknowledged {
            if ui.button("Continue").clicked() {
                self.acknowledged = true;
                self.disclosure = false;
            }
            ui.small("Not investment advice");
            return actions;
        }
        ui.label(self.state.description());
        for limit in &self.limits {
            let window = match limit.minutes {
                300 => "5-hour".into(),
                10080 => "Weekly".into(),
                n => format!("{n}-minute window"),
            };
            ui.add(
                egui::ProgressBar::new(limit.used / 100.0)
                    .text(format!("{} · {window}: {:.0}% used · resets {}", limit.id, limit.used, limit.reset)),
            );
        }
        if self.state == Onboarding::SigningIn && ui.small_button("Open sign-in page again").clicked() {
            actions.push(Action::ReopenLogin);
        }
        let height = (ui.available_height() - 150.0).max(40.0);
        egui::ScrollArea::vertical()
            .id_salt("conversation")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .max_height(height)
            .show(ui, |ui| {
                for (i, block) in self.blocks.iter().enumerate() {
                    match block {
                        RenderedBlock::Prompt(text) => {
                            ui.label(RichText::new(format!("You: {text}")).strong());
                            if let Some(turn) = self.turns.iter().find(|turn| turn.prompt_index == i) {
                                if turn.finished && turn.made_edits {
                                    let enabled = turn.undo_available && !self.running;
                                    if undo_button(ui, "Undo this turn", enabled, self.running).clicked()
                                        && let Some(id) = &turn.id
                                    {
                                        actions.push(Action::UndoTurn(id.clone()));
                                    }
                                }
                                if let Some(summary) = &turn.undo_summary {
                                    ui.label(RichText::new(summary).color(crate::theme::amber()));
                                }
                            }
                        }
                        RenderedBlock::Reply(_) => {
                            if let Some(cached) = self.reply_cache.get(&i) {
                                agent_markdown::show(ui, cached);
                            }
                        }
                        RenderedBlock::Notice(text) => {
                            ui.label(RichText::new(text).color(crate::theme::amber()));
                        }
                        RenderedBlock::Tool(trace) => {
                            let status = if trace.interrupted {
                                "interrupted"
                            } else {
                                match trace.success {
                                    Some(true) => "ok",
                                    Some(false) => "failed",
                                    None => "running",
                                }
                            };
                            let duration = trace.duration_ms.map(|ms| format!(" · {ms} ms")).unwrap_or_default();
                            let summary = trace.summary.as_ref().map(|s| format!(" · {s}")).unwrap_or_default();
                            egui::CollapsingHeader::new(format!("{} · {status}{duration}{summary}", trace.name))
                                .id_salt(("tool", i))
                                .show(ui, |ui| {
                                    ui.monospace(&trace.arguments);
                                });
                        }
                    }
                }
            });
        if self.blocks.is_empty() && self.state == Onboarding::Ready {
            ui.horizontal_wrapped(|ui| {
                for prompt in ["What's the trend?", "Find support and resistance", "Summarise the last 20 bars"] {
                    if ui.small_button(prompt).clicked() {
                        actions.push(Action::Send(Prompt { text: prompt.into(), requires_tools: true }));
                    }
                }
            });
        }
        ui.horizontal(|ui| {
            ui.label(format!("{} queued{}", self.queue.len(), if self.queue_paused { " · paused" } else { "" }));
            if !self.queue.is_empty() {
                if ui.small_button("Clear queue").clicked() {
                    self.queue.clear();
                }
                if self.queue_paused && ui.small_button("Resume queue").clicked() {
                    self.resume_queue();
                }
            }
            if self.last_prompt.is_some()
                && !self.running
                && (self.state == Onboarding::Ready || matches!(self.state, Onboarding::ToolsUnavailable { .. }))
                && ui.small_button("Retry last prompt").clicked()
            {
                actions.push(Action::Retry);
            }
            if self.running && ui.button("Interrupt (Esc)").clicked() {
                actions.push(Action::Interrupt);
            }
        });
        let input_id = input_id(pane_id);
        let focused = ui.memory(|m| m.has_focus(input_id));
        let enter =
            focused
                && ui.input_mut(|i| {
                    let plain = i.events.iter().any(|event| matches!(event,
                egui::Event::Key { key: egui::Key::Enter, pressed: true, modifiers, .. } if modifiers.is_none()
            ));
                    plain && i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                });
        // Esc is owned by app hotkeys (which also knows about the palette and
        // other text fields). Leave idle Esc for TextEdit to release its focus.
        ui.add(
            egui::TextEdit::multiline(&mut self.input)
                .id(input_id)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .hint_text("Ask the agent · Enter sends · Shift+Enter newline")
                .char_limit(16000),
        );
        let ready = self.state == Onboarding::Ready;
        let has_input = !self.input.trim().is_empty();
        let tools_unavailable = matches!(self.state, Onboarding::ToolsUnavailable { .. });
        let can_send = (ready || tools_unavailable) && has_input;
        if self.queue_paused {
            ui.small("New prompts append to the paused queue. Only Resume queue releases it.");
        }
        let clicked = ui
            .add_enabled(
                self.state != Onboarding::Checking && (!ready || has_input),
                egui::Button::new(if tools_unavailable && has_input { "Send" } else { self.state.primary() }),
            )
            .clicked();
        if clicked || (enter && can_send) {
            let action = match self.state {
                Onboarding::Dormant | Onboarding::Missing | Onboarding::Unsupported { .. } => Action::Check,
                Onboarding::SignedOut => Action::SignIn,
                Onboarding::SigningIn => Action::CancelLogin,
                Onboarding::Ready => {
                    Action::Send(Prompt { text: std::mem::take(&mut self.input), requires_tools: false })
                }
                Onboarding::RateLimited { .. } => Action::RefreshLimits,
                Onboarding::ToolsUnavailable { .. } if has_input => {
                    Action::Send(Prompt { text: std::mem::take(&mut self.input), requires_tools: false })
                }
                Onboarding::ToolsUnavailable { .. } => Action::Retry,
                Onboarding::Stopped { unsafe_backend: false, .. } => Action::Check,
                Onboarding::Stopped { unsafe_backend: true, .. } => Action::Close,
                Onboarding::Checking => return actions,
            };
            actions.push(action);
        }
        ui.small("Not investment advice");
        actions
    }

    fn header(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("agent-backend").selected_text("Codex").show_ui(ui, |ui| {
                ui.selectable_label(true, "Codex").on_hover_text("Local isolated backend");
            });
            let old = self.settings.model.clone();
            let label = self.models.iter().find(|m| m.id == old).map_or("Model", |m| m.display_name.as_str());
            egui::ComboBox::from_id_salt("agent-model").selected_text(label).show_ui(ui, |ui| {
                for model in &self.models {
                    ui.selectable_value(&mut self.settings.model, model.id.clone(), &model.display_name);
                }
            });
            if old != self.settings.model {
                self.settings.effort.clear();
            }
            self.settings.select_defaults(&self.models);
            if let Some(model) = self.models.iter().find(|m| m.id == self.settings.model) {
                egui::ComboBox::from_id_salt("agent-effort").selected_text(&self.settings.effort).show_ui(ui, |ui| {
                    for effort in &model.supported_reasoning_efforts {
                        if effort.reasoning_effort == "ultra" {
                            continue;
                        }
                        let label = effort_label(effort);
                        ui.selectable_value(&mut self.settings.effort, effort.reasoning_effort.clone(), label)
                            .on_hover_text(&effort.description);
                    }
                });
            }
            let available = crate::agent_bridge::agent_edit_tools_available();
            let mut selected = self.settings.tools;
            egui::ComboBox::from_id_salt("agent-tools")
                .selected_text(format!("Next session: {}", selected.label()))
                .show_ui(ui, |ui| {
                    for mode in [ToolsMode::ReadOnly, ToolsMode::Edit] {
                        ui.add_enabled_ui(self.can_change_tools(mode, available), |ui| {
                            ui.selectable_value(&mut selected, mode, mode.label());
                        })
                        .response
                        .on_disabled_hover_text(if mode == ToolsMode::Edit && !available {
                            "Chart-edit execution is not available in this build. Sessions use Read only."
                        } else {
                            "Wait for the turn or sign-in checks to finish. Unsafe sessions require an app restart."
                        });
                    }
                });
            if selected != self.settings.tools {
                actions.push(Action::SetTools(selected));
            }
            let current = match self.session_tools {
                Some(ToolSet::Edit) => "Can edit charts",
                Some(_) => "Read only",
                None => "Not started",
            };
            ui.strong(format!("Current mode: {current}"));
            if undo_button(ui, "Undo last agent turn", self.header_undo_available && !self.running, self.running)
                .clicked()
                && let Some(id) = &self.header_undo
            {
                actions.push(Action::UndoTurn(id.clone()));
            }
        });
        if self.models.iter().find(|m| m.id == self.settings.model).is_some_and(|m| {
            m.supported_reasoning_efforts
                .iter()
                .any(|e| e.reasoning_effort == self.settings.effort && (e.costly || e.reasoning_effort == "max"))
        }) {
            ui.small("Costly effort: higher subscription usage and latency.");
        }
    }
}

fn undo_button(ui: &mut Ui, label: &str, enabled: bool, running: bool) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(label))
        .on_hover_text("Undo the whole agent turn on all affected charts; preserve conflicting user changes.")
        .on_disabled_hover_text(if running { "Wait for the active turn to finish." } else { UNDO_UNAVAILABLE })
}

pub(crate) fn is_edit_tool(name: &str) -> bool {
    matches!(
        name,
        "create_drawing"
            | "update_drawing"
            | "delete_drawing"
            | "add_indicator"
            | "remove_indicator"
            | "set_view"
            | "compare"
    )
}

fn short_label(text: &str) -> String {
    scrub_text(text).chars().filter(|ch| !ch.is_control()).take(180).collect()
}

/// Whitelist a tiny result projection, never forward raw contentItems or anchor arrays.
pub(crate) fn edit_tool_summary(item: &Value) -> Option<String> {
    let name = item["tool"].as_str()?;
    if !is_edit_tool(name) {
        return None;
    }
    let args = &item["arguments"];
    let result = item["contentItems"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|content| {
            (content["type"] == "inputText")
                .then(|| content["text"].as_str().and_then(|text| serde_json::from_str::<Value>(text).ok()))
                .flatten()
        })
        .unwrap_or(Value::Null);
    let label = match name {
        "create_drawing" | "update_drawing" => match args["kind"].as_str().unwrap_or("drawing") {
            "horizontal" => args["anchors"][0]["price"]
                .as_f64()
                .filter(|p| p.is_finite())
                .map_or_else(|| "horizontal line".into(), |price| format!("horizontal line at {price:.2}")),
            "trend_line" => "trend line (2 anchors)".into(),
            "vertical" => "vertical line".into(),
            "ray" => "ray (2 anchors)".into(),
            "rectangle" => "rectangle (2 anchors)".into(),
            "text" => "text annotation".into(),
            _ => "drawing update".into(),
        },
        "delete_drawing" => "drawing removed".into(),
        "add_indicator" => {
            let code = args["code"].as_str().unwrap_or("indicator").to_uppercase();
            let params = args["params"]
                .as_array()
                .into_iter()
                .flatten()
                .take(4)
                .filter_map(|p| p.as_f64().filter(|p| p.is_finite()))
                .map(|p| p.to_string())
                .collect::<Vec<_>>();
            format!("{code} {}", params.join(", ")).trim().to_owned()
        }
        "remove_indicator" => "indicator removed".into(),
        "compare" => format!(
            "comparison {} · {}",
            result["symbol"].as_str().or_else(|| args["symbol"].as_str()).unwrap_or_default(),
            args["action"].as_str().unwrap_or_default()
        ),
        _ => format!("chart view {}", args["timeframe"].as_str().unwrap_or_default()).trim().to_owned(),
    };
    let id = if item["success"] == true {
        result.get("id").or_else(|| args.get("id")).or_else(|| result.get("uid"))
    } else {
        args.get("id")
    };
    let id = id.and_then(|id| id.as_str().map(str::to_owned).or_else(|| id.as_u64().map(|id| id.to_string())));
    Some(short_label(&match id {
        Some(id) => format!("#{id} · {label}"),
        None => label,
    }))
}

fn effort_label(effort: &tr_agent::codex::ReasoningOption) -> String {
    format!(
        "{}{}",
        effort.reasoning_effort,
        if effort.costly || effort.reasoning_effort == "max" { " · costly" } else { "" }
    )
}
pub fn input_id(pane: egui::Id) -> egui::Id {
    pane.with("agent-input")
}
pub fn captures_focus(focused: Option<egui::Id>, panes: impl IntoIterator<Item = egui::Id>) -> bool {
    panes.into_iter().any(|id| focused == Some(input_id(id)))
}
fn safe_arguments(value: &Value) -> String {
    fn strip(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.retain(|key, _| {
                    !["image", "bytes", "base64", "contentitems"]
                        .iter()
                        .any(|word| key.to_ascii_lowercase().contains(word))
                });
                map.values_mut().for_each(strip);
            }
            Value::Array(values) => values.iter_mut().for_each(strip),
            Value::String(text) if text.starts_with("data:image") || text.len() > 4096 => *text = "[omitted]".into(),
            _ => {}
        }
    }
    let mut value = value.clone();
    strip(&mut value);
    scrub_value(&mut value);
    value.to_string().chars().take(4096).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onboarding_has_one_primary_and_unsafe_is_sticky() {
        let mut pane = AgentPane::default();
        for state in [
            Onboarding::Checking,
            Onboarding::Missing,
            Onboarding::Unsupported { found: "0.1".into() },
            Onboarding::SignedOut,
            Onboarding::SigningIn,
            Onboarding::SignedOut,
            Onboarding::SigningIn,
            Onboarding::Ready,
            Onboarding::RateLimited { reset: "tomorrow".into() },
            Onboarding::Ready,
            Onboarding::ToolsUnavailable { reason: "router".into() },
            Onboarding::Ready,
            Onboarding::Stopped { reason: "exhausted".into(), unsafe_backend: false },
            Onboarding::Stopped { reason: "tripwire".into(), unsafe_backend: true },
        ] {
            pane.transition(state.clone());
            assert_eq!(pane.state, state);
            assert!(!pane.state.primary().is_empty());
        }
        pane.transition(Onboarding::Ready);
        assert!(matches!(pane.state, Onboarding::Stopped { unsafe_backend: true, .. }));
        assert_eq!(pane.state.primary(), "Close agent");
    }

    #[test]
    fn coalescing_has_no_idle_or_hidden_timer() {
        let mut c = Coalescer::default();
        let now = Instant::now();
        assert_eq!(c.due_in(now, true), None);
        c.changed();
        assert_eq!(c.due_in(now, false), None);
        assert_eq!(c.due_in(now, true), Some(Duration::ZERO));
        c.flush(now);
        assert_eq!(c.due_in(now + STREAM_INTERVAL, true), None);
        c.changed();
        assert_eq!(c.due_in(now + Duration::from_millis(10), true), Some(Duration::from_millis(23)));
        assert_eq!(c.due_in(now + STREAM_INTERVAL, true), Some(Duration::ZERO));
    }

    #[test]
    fn queue_is_fifo_and_recovery_never_resends() {
        let mut pane = AgentPane { state: Onboarding::Ready, ..Default::default() };
        for text in ["one", "two", "three"] {
            pane.enqueue(Prompt { text: text.into(), requires_tools: false });
        }
        assert_eq!(pane.next_prompt().unwrap().text, "one");
        assert!(pane.next_prompt().is_none());
        pane.event(BridgeEvent::Completed {
            turn: "t".into(),
            status: "completed".into(),
            calls: 0,
            interrupt_elapsed: None,
        });
        assert_eq!(pane.next_prompt().unwrap().text, "two");
        pane.event(BridgeEvent::Recovered(Recovery::Resumed));
        assert!(pane.next_prompt().is_none());
        pane.resume_queue();
        assert_eq!(pane.next_prompt().unwrap().text, "three");
        pane.queue.clear();
        assert_eq!(pane.last_prompt.unwrap().text, "three");
    }

    #[test]
    fn fake_models_default_first_and_costly_efforts() {
        let mut models: Vec<Model> = serde_json::from_value(serde_json::json!([
            {"id":"other","model":"other","displayName":"Other","defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"balanced"}]},
            {"id":"default","model":"default","displayName":"Default","isDefault":true,"defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low","description":"quick"},{"reasoningEffort":"max","description":"slow"}]}
        ])).unwrap();
        models.sort_by_key(|m| !m.is_default);
        let mut settings = Settings::default();
        settings.select_defaults(&models);
        assert_eq!((settings.model.as_str(), settings.effort.as_str()), ("default", "low"));
        assert_eq!(effort_label(&models[0].supported_reasoning_efforts[1]), "max · costly");
        settings.model = "other".into();
        settings.select_defaults(&models);
        assert_eq!(settings.effort, "medium");
    }

    #[test]
    fn settings_roundtrip_contains_only_choices_and_edit_requires_availability() {
        let settings =
            Settings { model: "m".into(), effort: "max".into(), tools: ToolsMode::Edit, pinned_chart: Some(41) };
        let value = serde_json::to_value(crate::layouts::PaneSettings::Agent(settings.clone())).unwrap();
        assert_eq!(value["Agent"].as_object().unwrap().len(), 4);
        assert_eq!(
            serde_json::from_value::<crate::layouts::PaneSettings>(value).unwrap(),
            crate::layouts::PaneSettings::Agent(settings.clone())
        );
        assert_eq!(settings.tool_set_if_available(false), ToolSet::ReadOnly);
        assert_eq!(settings.tool_set_if_available(true), ToolSet::Edit);
        assert_eq!(settings.tool_set(), ToolSet::Edit);
        assert_eq!(Settings::default().tool_set(), ToolSet::ReadOnly);
        assert!(serde_json::from_str::<Settings>("{\"token\":\"never\"}").is_err());
    }

    #[test]
    fn focus_capture_is_specific_to_agent_input() {
        let id = egui::Id::new("pane");
        assert!(captures_focus(Some(input_id(id)), [id]));
        assert!(!captures_focus(Some(id), [id]));
        assert!(!captures_focus(None, [id]));
    }

    #[test]
    fn trace_never_keeps_images_or_secret_arguments_and_limits_exclude_account_fields() {
        let args = safe_arguments(
            &serde_json::json!({"token":"secret","imageUrl":"data:image/png;base64,abc","nested":{"bytes":[1,2,3]},"symbol":"AAPL"}),
        );
        assert!(!args.contains("secret") && !args.contains("base64") && !args.contains("bytes"));
        let mut pane = AgentPane::default();
        pane.set_limits(&serde_json::json!({"ordinaryUsageAllowed":false,"rateLimits":{"planType":"never display","credits":{"balance":"secret"},"primary":{"usedPercent":100,"resetsAt":1793293790,"windowDurationMins":300}}}));
        assert!(matches!(pane.state, Onboarding::RateLimited { .. }));
        assert_eq!(pane.limits[0].used, 100.0);
    }

    #[test]
    fn enter_sends_chat_shift_enter_edits_and_pane_does_not_interrupt() {
        let mut pane = AgentPane { state: Onboarding::Ready, acknowledged: true, ..Default::default() };
        let ctx = egui::Context::default();
        let id = egui::Id::new("agent-keyboard-test");
        for (key, modifiers) in [
            (egui::Key::Enter, egui::Modifiers::NONE),
            (egui::Key::Enter, egui::Modifiers::SHIFT),
            (egui::Key::Escape, egui::Modifiers::NONE),
        ] {
            pane.input = "read my chart".into();
            pane.running = key == egui::Key::Escape;
            let mut actions = Vec::new();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 700.0))),
                    events: vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }],
                    ..Default::default()
                },
                |ui| {
                    ctx.memory_mut(|m| m.request_focus(input_id(id)));
                    actions.extend(pane.show(ui, "No chart pinned", id));
                },
            );
            output.textures_delta.clear();
            if key == egui::Key::Escape {
                // Interrupt belongs exclusively to app hotkeys, not pane rendering.
                assert!(!actions.iter().any(|a| matches!(a, Action::Interrupt)));
            } else if modifiers.shift {
                assert!(!actions.iter().any(|a| matches!(a, Action::Send(_))));
                assert!(pane.input.contains('\n'));
            } else {
                assert!(actions.iter().any(
                    |a| matches!(a, Action::Send(Prompt { text, requires_tools: false }) if text == "read my chart")
                ));
                assert!(pane.input.is_empty());
            }
        }
    }
}

#[cfg(test)]
mod fix_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prompts_preserve_whitespace_and_retry_does_not_release_paused_fifo() {
        let mut pane = AgentPane { state: Onboarding::Ready, ..Default::default() };
        pane.enqueue(Prompt { text: " \n user-defined account-level question \n ".into(), requires_tools: false });
        let prompt = pane.next_prompt().unwrap();
        assert_eq!(prompt.text, " \n user-defined account-level question \n ");
        assert_eq!(pane.blocks[0], RenderedBlock::Prompt(prompt.text.clone()));
        pane.enqueue(Prompt { text: "stale".into(), requires_tools: false });
        pane.event(BridgeEvent::Recovered(Recovery::Resumed));
        pane.start_prompt(prompt).unwrap();
        pane.event(BridgeEvent::Completed {
            turn: "retry".into(),
            status: "completed".into(),
            calls: 0,
            interrupt_elapsed: None,
        });
        assert!(pane.next_prompt().is_none());
        pane.resume_queue();
        assert_eq!(pane.next_prompt().unwrap().text, "stale");
    }

    #[test]
    fn selected_model_limit_blocks_but_other_limits_are_visible_only() {
        let mut pane = AgentPane { state: Onboarding::Ready, ..Default::default() };
        let value = json!({"ordinaryUsageAllowed":true,
            "rateLimits":{"primary":{"usedPercent":3}},
            "rateLimitsByLimitId":{"selected":{"primary":{"usedPercent":100}}, "other":{"primary":{"usedPercent":100}}},
            "accountId":"do not retain", "credits":{"balance":"do not retain"}});
        pane.settings.model = "selected".into();
        pane.set_limits(&value);
        assert!(matches!(pane.state, Onboarding::RateLimited { .. }));
        assert_eq!(pane.limits.len(), 3);
        assert!(pane.limits.iter().any(|l| l.id == "other"));
        assert!(!pane.limits_value.to_string().contains("do not retain"));
        pane.settings.model = "unlimited-model".into();
        pane.set_limits(&value);
        assert_eq!(pane.state, Onboarding::Ready);
        assert!(pane.queue_paused);
        pane.set_limits(&json!({"ordinaryUsageAllowed":false}));
        assert!(matches!(pane.state, Onboarding::RateLimited { .. }));
    }

    #[test]
    fn message_rows_keep_event_order_and_only_changed_messages_are_reparsed() {
        let mut pane = AgentPane::default();
        pane.wire("item/started", &json!({"item":{"type":"agentMessage","id":"a","text":""}}));
        pane.wire("item/agentMessage/delta", &json!({"itemId":"a","delta":"**Reading**."}));
        pane.wire("item/started", &json!({"item":{"type":"dynamicToolCall","id":"tool","tool":"read_chart"}}));
        pane.wire("item/agentMessage/delta", &json!({"itemId":"b","delta":"Trend is up."}));
        pane.flush(Instant::now());
        assert!(matches!(&pane.blocks[..], [RenderedBlock::Reply(_), RenderedBlock::Tool(_), RenderedBlock::Reply(_)]));
        let first = pane.reply_cache[&0].as_ptr();
        let second = pane.reply_cache[&2].as_ptr();
        pane.flush(Instant::now());
        assert_eq!(first, pane.reply_cache[&0].as_ptr());
        assert_eq!(second, pane.reply_cache[&2].as_ptr());
        pane.wire(
            "item/completed",
            &json!({"item":{"type":"agentMessage","id":"b","text":"Trend is up. user-defined."}}),
        );
        pane.flush(Instant::now());
        assert_eq!(first, pane.reply_cache[&0].as_ptr());
        assert_eq!(pane.blocks.len(), 3);
        assert_eq!(pane.blocks[2], RenderedBlock::Reply(vec![Block::Paragraph("Trend is up. user-defined.".into())]));
        pane.wire("item/completed", &json!({"item":{"type":"agentMessage","id":"c","text":"No deltas needed."}}));
        pane.flush(Instant::now());
        assert_eq!(pane.blocks[3], RenderedBlock::Reply(vec![Block::Paragraph("No deltas needed.".into())]));
    }

    #[test]
    fn interrupt_and_recovery_mark_unfinished_tools_and_pause_the_queue() {
        for recover in [false, true] {
            let mut pane = AgentPane { state: Onboarding::Ready, running: true, ..Default::default() };
            pane.wire("item/started", &json!({"item":{"type":"dynamicToolCall","id":"pending","tool":"read_chart"}}));
            pane.wire(
                "item/completed",
                &json!({"item":{"type":"dynamicToolCall","id":"done","tool":"read_data","success":true}}),
            );
            if recover {
                pane.event(BridgeEvent::Recovered(Recovery::Resumed));
            } else {
                pane.interrupt();
            }
            assert!(matches!(&pane.blocks[0], RenderedBlock::Tool(trace) if trace.interrupted));
            assert!(matches!(&pane.blocks[1], RenderedBlock::Tool(trace) if !trace.interrupted));
            assert!(pane.queue_paused);
        }
    }

    #[test]
    fn tools_unavailable_accepts_a_new_prompt_without_retrying_the_previous_one() {
        let mut pane = AgentPane {
            state: Onboarding::ToolsUnavailable { reason: "router".into() },
            acknowledged: true,
            input: "A new user-defined question".into(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let id = egui::Id::new("tools-unavailable-input");
        let mut actions = Vec::new();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 700.0))),
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                ctx.memory_mut(|m| m.request_focus(input_id(id)));
                actions.extend(pane.show(ui, "No chart pinned", id));
            },
        );
        output.textures_delta.clear();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::Send(Prompt { text, .. }) if text == "A new user-defined question"))
        );
        assert!(!actions.iter().any(|a| matches!(a, Action::Retry)));
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mode_switch_is_gated_never_mid_turn_and_restart_keeps_queue_paused() {
        let mut pane = AgentPane { state: Onboarding::Ready, ..Default::default() };
        pane.enqueue(Prompt { text: "queued".into(), requires_tools: false });
        assert!(!pane.can_change_tools(ToolsMode::Edit, false));
        assert!(!pane.select_tools(ToolsMode::Edit, false));
        assert!(pane.can_change_tools(ToolsMode::Edit, true));
        pane.running = true;
        assert!(!pane.select_tools(ToolsMode::Edit, true));
        assert_eq!(pane.settings.tools, ToolsMode::ReadOnly);
        assert_eq!(pane.prepare_session_start(), None);
        pane.running = false;
        assert_eq!(pane.prepare_session_start(), Some(ToolSet::ReadOnly));
        pane.transition(Onboarding::Ready);
        assert!(pane.select_tools(ToolsMode::Edit, true));
        assert_eq!(pane.session_tools, Some(ToolSet::ReadOnly), "selection must not mutate a running registry");
        assert!(
            matches!(pane.blocks.last(), Some(RenderedBlock::Notice(text)) if text.contains("Queue stays paused") && text.contains("restart"))
        );
        assert_eq!(pane.prepare_session_start(), Some(ToolSet::Edit));
        pane.transition(Onboarding::Ready);
        assert!(pane.next_prompt().is_none());
        pane.resume_queue();
        assert_eq!(pane.next_prompt().unwrap().text, "queued");
        pane.running = false;
        pane.transition(Onboarding::Stopped { reason: "tripwire".into(), unsafe_backend: true });
        assert!(!pane.select_tools(ToolsMode::ReadOnly, true));
        assert_eq!(pane.prepare_session_start(), None);
    }

    fn completed_edit(pane: &mut AgentPane, id: &str) {
        pane.start_prompt(Prompt { text: "support".into(), requires_tools: false }).unwrap();
        pane.event(BridgeEvent::Started { turn: id.into() });
        pane.wire(
            "item/completed",
            &json!({"turnId":"wire-id-not-an-undo-key", "item":{
                "id":"call", "type":"dynamicToolCall", "tool":"create_drawing", "success":true,
                "arguments":{"kind":"horizontal", "anchors":[{"time":86400, "price":227.12345}]},
                "contentItems":[{"type":"inputText","text":"{\"id\":42}"}]
            }}),
        );
        pane.event(BridgeEvent::Completed {
            turn: id.into(),
            status: "completed".into(),
            calls: 1,
            interrupt_elapsed: None,
        });
    }

    #[test]
    fn started_attaches_canonical_id_to_each_prompt_and_not_the_wire_id() {
        let mut pane = AgentPane { state: Onboarding::Ready, ..Default::default() };
        let mut ids = tr_agent::bridge::TurnIds::new().unwrap();
        let first = ids.allocate().unwrap();
        let second = ids.allocate().unwrap();
        completed_edit(&mut pane, &first);
        completed_edit(&mut pane, &second);
        assert_eq!(
            pane.turns.iter().map(|t| t.id.as_deref()).collect::<Vec<_>>(),
            [Some(first.as_str()), Some(second.as_str())]
        );
        for turn in &pane.turns {
            assert!(turn.finished);
            assert!(matches!(&pane.blocks[turn.prompt_index], RenderedBlock::Prompt(text) if text == "support"));
        }
        pane.refresh_undo(&[first.clone(), second.clone()], Some(&second));
        assert!(pane.turns.iter().all(|t| t.made_edits && t.undo_available));
        pane.refresh_undo(&[], None);
        assert!(pane.turns.iter().all(|t| t.made_edits && !t.undo_available));
        assert_eq!(pane.header_undo.as_deref(), Some(second.as_str()));
        assert!(!pane.header_undo_available);
    }

    #[test]
    fn undo_availability_alone_detects_edits_without_tool_notifications() {
        let mut pane = AgentPane { state: Onboarding::Ready, ..Default::default() };
        pane.start_prompt(Prompt { text: "RSI".into(), requires_tools: false }).unwrap();
        pane.event(BridgeEvent::Started { turn: "canonical".into() });
        pane.refresh_undo(&["canonical".into()], Some("canonical"));
        assert!(pane.turns[0].made_edits);
        assert!(!pane.turns[0].undo_available, "active turn cannot be undone");
        pane.event(BridgeEvent::Completed {
            turn: "canonical".into(),
            status: "interrupted".into(),
            calls: 1,
            interrupt_elapsed: None,
        });
        pane.refresh_undo(&["canonical".into()], Some("canonical"));
        assert!(pane.turns[0].undo_available, "partial edits remain undoable");
    }

    fn frame(pane: &mut AgentPane, ctx: &egui::Context, events: Vec<egui::Event>) -> (Vec<Action>, egui::FullOutput) {
        let mut actions = Vec::new();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))),
                events,
                ..Default::default()
            },
            |ui| {
                actions.extend(pane.show(ui, "Pinned", egui::Id::new("undo-test")));
            },
        );
        output.textures_delta.clear();
        (actions, output)
    }

    fn text_position(output: &egui::FullOutput, text: &str) -> egui::Pos2 {
        fn find(shape: &egui::epaint::Shape, text: &str) -> Option<egui::Pos2> {
            match shape {
                egui::epaint::Shape::Text(s) if s.galley.text() == text => {
                    Some(s.pos + s.galley.rect.center().to_vec2())
                }
                egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|s| find(s, text)),
                _ => None,
            }
        }
        output.shapes.iter().find_map(|s| find(&s.shape, text)).unwrap_or_else(|| panic!("missing UI text: {text}"))
    }

    #[test]
    fn both_undo_buttons_emit_app_actions_and_summary_stays_inline_when_disabled() {
        for label in ["Undo this turn", "Undo last agent turn"] {
            let mut pane = AgentPane { state: Onboarding::Ready, acknowledged: true, ..Default::default() };
            completed_edit(&mut pane, "canonical");
            pane.refresh_undo(&["canonical".into()], Some("canonical"));
            let ctx = egui::Context::default();
            // Let egui settle wrapping/layout before pointing at the button.
            frame(&mut pane, &ctx, vec![]);
            let (_, output) = frame(&mut pane, &ctx, vec![]);
            let pos = text_position(&output, label);
            let click = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            frame(&mut pane, &ctx, vec![egui::Event::PointerMoved(pos), click(true)]);
            let (actions, _) = frame(&mut pane, &ctx, vec![click(false)]);
            assert!(actions.iter().any(|a| matches!(a, Action::UndoTurn(id) if id == "canonical")), "{label}");
            pane.undo_result(
                "canonical",
                Ok(crate::agent_bridge::UndoSummary { charts: 1, reverted: 2, skipped: vec!["kept".into()] }),
            );
            pane.refresh_undo(&[], None);
            let (_, output) = frame(&mut pane, &ctx, vec![]);
            text_position(&output, "Undo: 2 reverted · 1 conflicts skipped · 1 charts");
            frame(&mut pane, &ctx, vec![click(true)]);
            let (actions, _) = frame(&mut pane, &ctx, vec![click(false)]);
            assert!(!actions.iter().any(|a| matches!(a, Action::UndoTurn(_))));
            assert!(!pane.turns[0].undo_available);
            assert_eq!(
                UNDO_UNAVAILABLE,
                "undo for this agent turn is no longer available (layout changed/app restarted)"
            );
        }
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    use serde_json::json;

    fn delta(pane: &mut AgentPane, text: &str) {
        pane.event(BridgeEvent::Wire(WireEvent::Notification {
            method: "item/agentMessage/delta".into(),
            params: json!({"itemId": "msg", "delta": text}),
        }));
    }

    #[test]
    fn enqueue_does_not_rewrite_ordinary_prompt_text() {
        let mut pane = AgentPane { state: Onboarding::Ready, ..Default::default() };
        let text = "Is this user-friendly for an account-level review of 0x6B175474E89094C44Da98b954EedeAC495271d0F?";
        pane.enqueue(Prompt { text: text.into(), requires_tools: false });
        assert_eq!(pane.next_prompt().unwrap().text, text);
    }

    #[test]
    fn reply_text_is_not_redacted_for_ordinary_words() {
        let mut pane = AgentPane { state: Onboarding::Ready, ..Default::default() };
        delta(&mut pane, "Use a user-defined stop at the org-wide 200-day average.");
        pane.flush(Instant::now());
        let Some(RenderedBlock::Reply(blocks)) = pane.blocks.last() else { panic!("no reply") };
        assert_eq!(
            blocks,
            &vec![agent_markdown::Block::Paragraph("Use a user-defined stop at the org-wide 200-day average.".into())]
        );
    }

    #[test]
    fn preamble_and_final_answer_stay_separate_messages() {
        let mut pane = AgentPane { state: Onboarding::Ready, ..Default::default() };
        pane.enqueue(Prompt { text: "trend?".into(), requires_tools: true });
        pane.next_prompt().unwrap();
        delta(&mut pane, "Reading the chart.");
        pane.flush(Instant::now());
        pane.event(BridgeEvent::Wire(WireEvent::Notification {
            method: "item/completed".into(),
            params: json!({"item":{"type":"dynamicToolCall","id":"call_1","tool":"read_chart","arguments":{},"success":true,"durationMs":1}}),
        }));
        pane.event(BridgeEvent::Wire(WireEvent::Notification {
            method: "item/agentMessage/delta".into(),
            params: json!({"itemId": "msg-2", "delta": "The trend is up."}),
        }));
        pane.event(BridgeEvent::Completed {
            turn: "t".into(),
            status: "completed".into(),
            calls: 1,
            interrupt_elapsed: None,
        });
        let kinds: Vec<_> = pane
            .blocks
            .iter()
            .map(|b| match b {
                RenderedBlock::Prompt(_) => "prompt".to_owned(),
                RenderedBlock::Tool(_) => "tool".to_owned(),
                RenderedBlock::Notice(_) => "notice".to_owned(),
                RenderedBlock::Reply(blocks) => format!("{blocks:?}"),
            })
            .collect();
        // The final answer must follow the tool row, and not be "Reading the chart.The trend is up."
        assert_eq!(kinds.last().map(String::as_str), Some(r#"[Paragraph("The trend is up.")]"#), "{kinds:?}");
    }

    #[test]
    fn escape_when_idle_releases_input_focus() {
        let mut pane = AgentPane { state: Onboarding::Ready, acknowledged: true, ..Default::default() };
        let ctx = egui::Context::default();
        let id = egui::Id::new("agent-escape-focus");
        for (events, focus) in [
            (vec![], true),
            (
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                false,
            ),
        ] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 700.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    if focus {
                        ctx.memory_mut(|m| m.request_focus(input_id(id)));
                    }
                    pane.show(ui, "No chart pinned", id);
                },
            );
            output.textures_delta.clear();
        }
        assert!(!ctx.memory(|m| m.has_focus(input_id(id))), "Esc did not release the chat input");
    }

    #[test]
    fn other_limit_id_at_100_percent_does_not_block_ordinary_usage() {
        let mut pane = AgentPane { state: Onboarding::Ready, ..Default::default() };
        pane.set_limits(&json!({
            "ordinaryUsageAllowed": true,
            "rateLimits": {"primary": {"usedPercent": 3, "resetsAt": 1793293790, "windowDurationMins": 10080}},
            "rateLimitsByLimitId": {
                "other-model": {"primary": {"usedPercent": 100, "resetsAt": 1793293790, "windowDurationMins": 300}}
            }
        }));
        assert_eq!(pane.state, Onboarding::Ready);
    }
}
