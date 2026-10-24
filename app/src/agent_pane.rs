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
use crate::theme::{self, dp};
use crate::widgets::{self, Icon};

pub use tr_agent::codex::ResponseStyle;

pub const STREAM_INTERVAL: Duration = Duration::from_millis(33);
pub const UNDO_UNAVAILABLE: &str = "undo for this agent turn is no longer available (layout changed/app restarted)";
const CHART_EDITS_HELP: &str = "The agent can add, change and remove its own drawings, indicators and comparisons on the pinned chart, and propose watchlist and alert changes for you to approve. Your objects are never changed without approval, and every agent edit can be undone.";
pub(crate) const INSTALLATION_URL: &str = "https://developers.openai.com/codex";
const CHECKING_STATUS: &str = "Checking Codex, your account and chart tools…";
const RETRY_HELP: &str = "Sends the same prompt again and starts a new answer.";
const INTERRUPTED: &str = "Stopped. This answer is incomplete.";
const PRIVACY_GROUPS: &[(&str, &str)] = &[
    (
        "What is sent",
        "Your prompts, the pinned chart’s data and tool results go to OpenAI under your ChatGPT account. Screenshots only with your consent; not available yet.",
    ),
    (
        "What the agent can change",
        "In Chart edits mode: its own drawings, indicators and comparisons on the pinned chart, plus watchlist and alert proposals that require your approval. Your objects are never changed without approval. Every agent edit can be undone.",
    ),
    (
        "What is stored on this Mac",
        "Codex keeps conversations for recovery for 7 days, up to the newest 50. Layouts store your choices only, never conversations or sign-in details.",
    ),
];

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
    pub response_style: ResponseStyle,
}
impl ToolsMode {
    fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read only",
            Self::Edit => "Chart edits",
        }
    }
}

impl Settings {
    pub fn tool_set(&self) -> ToolSet {
        self.tool_set_if_available(crate::agent_bridge::agent_edit_tools_available())
    }

    fn tool_set_if_available(&self, available: bool) -> ToolSet {
        if available && self.tools == ToolsMode::Edit { ToolSet::Workflows } else { ToolSet::ReadOnly }
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
            Self::Dormant => "Start",
            Self::Checking => "Checking…",
            Self::Missing | Self::Unsupported { .. } => "Check again",
            Self::SignedOut => "Sign in with ChatGPT",
            Self::SigningIn => "Cancel",
            Self::Ready => "Send",
            Self::RateLimited { .. } => "Check limits",
            Self::ToolsUnavailable { .. } => "Retry response",
            Self::Stopped { unsafe_backend: true, .. } => "Close",
            Self::Stopped { unsafe_backend: false, .. } => "Restart",
        }
    }
    pub fn title(&self) -> &'static str {
        match self {
            Self::Dormant => "Chart analyst",
            Self::Checking => "Checking the agent",
            Self::Missing => "Codex isn't installed",
            Self::Unsupported { .. } => "Codex version not supported",
            Self::SignedOut => "Sign in with ChatGPT",
            Self::SigningIn => "Finish signing in",
            Self::Ready => "Ready",
            Self::RateLimited { .. } => "Usage limit reached",
            Self::ToolsUnavailable { .. } => "Chart data unavailable",
            Self::Stopped { unsafe_backend: true, .. } => "Agent stopped for safety",
            Self::Stopped { .. } => "Agent disconnected",
        }
    }
    fn description(&self) -> String {
        match self {
            Self::Dormant => {
                "Answers questions about your pinned chart using your ChatGPT account. Chart tools run locally in a sandbox; prompts and chart data are sent to OpenAI."
                    .into()
            }
            Self::Checking => "Nothing is sent yet.".into(),
            Self::Missing => "Install the Codex CLI, then check again.".into(),
            Self::Unsupported { found } => {
                format!("Found {}; this build is tested with 0.163.0.", found.trim_start_matches("codex-cli "))
            }
            Self::SignedOut => "API-key accounts aren't supported.".into(),
            Self::SigningIn => "Complete ChatGPT sign-in in your browser, then return here.".into(),
            Self::Ready => String::new(),
            Self::RateLimited { reset } => format!("Resets {reset}. Queued prompts stay paused until you resume them."),
            Self::ToolsUnavailable { .. } => "No analysis was completed.".into(),
            Self::Stopped { unsafe_backend: true, .. } => {
                "A safety check failed, so this session was stopped. Close it and start a new one.".into()
            }
            Self::Stopped { .. } => "Restart to continue; nothing is resent. The queue stays paused.".into(),
        }
    }
    fn composer_placeholder(&self) -> &'static str {
        match self {
            Self::SignedOut => "Sign in to ask about this chart",
            Self::Missing | Self::Unsupported { .. } => "Install Codex to start",
            Self::Checking | Self::SigningIn => "Starting…",
            Self::RateLimited { .. } => "Usage limit reached",
            Self::Stopped { .. } => "Agent stopped",
            Self::Dormant => "Start the agent to ask about this chart",
            Self::Ready | Self::ToolsUnavailable { .. } => "Ask about the pinned chart",
        }
    }

    fn accepts_input(&self) -> bool {
        matches!(self, Self::Ready | Self::ToolsUnavailable { .. })
    }

    fn action(&self) -> Option<Action> {
        Some(match self {
            Self::Dormant | Self::Missing | Self::Unsupported { .. } | Self::Stopped { unsafe_backend: false, .. } => {
                Action::Check
            }
            Self::SignedOut => Action::SignIn,
            Self::SigningIn => Action::CancelLogin,
            Self::RateLimited { .. } => Action::RefreshLimits,
            Self::ToolsUnavailable { .. } => Action::Retry,
            Self::Stopped { unsafe_backend: true, .. } => Action::Close,
            Self::Ready | Self::Checking => return None,
        })
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
    // In-memory identity only; never rendered in the tool row or persisted to layouts.
    target_args: Value,
    pub summary: Option<String>,
    pub interrupted: bool,
    pub error: Option<String>,
}

/// Local conversation metadata, never part of Settings or a layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationTurn {
    pub id: Option<String>,
    pub prompt_index: usize,
    pub finished: bool,
    pub final_answer_succeeded: bool,
    pub made_edits: bool,
    pub chart_changes: Option<usize>,
    pub undo_available: bool,
    pub undo_summary: Option<String>,
    pub reverted_changes: Option<usize>,
    pub local_time: String,
    pub(crate) started_at: Instant,
    pub(crate) duration: Option<Duration>,
}

impl ConversationTurn {
    fn reverted_summary(&self) -> Option<&str> {
        let count = self.reverted_changes?;
        self.undo_summary.as_deref().filter(|summary| summary.starts_with(&tool_group_label(&[], Some(count))))
    }
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
pub(crate) fn local_time(timestamp: i64) -> String {
    let Some(time) = chrono::DateTime::from_timestamp(timestamp, 0) else { return "time unavailable".into() };
    let time = time.with_timezone(&chrono::Local);
    let zone = tr_macos::local_timezone_abbreviation(timestamp)
        // Some zones have numeric names. Omit those rather than showing a raw offset.
        .filter(|name| !name.is_empty() && name.chars().all(|c| c.is_alphabetic() || c == ' '))
        .or_else(|| (time.offset().local_minus_utc() == 0).then(|| "UTC".into()));
    format!("{}{}", time.format("%H:%M"), zone.map_or(String::new(), |name| format!(" {name}")))
}

pub(crate) fn reset_time_at(timestamp: Option<i64>, now: i64) -> String {
    let Some(time) = timestamp.and_then(|t| chrono::DateTime::from_timestamp(t, 0)) else {
        return "time unavailable".into();
    };
    let date = time.with_timezone(&chrono::Local).date_naive();
    let today = chrono::DateTime::from_timestamp(now, 0).map(|t| t.with_timezone(&chrono::Local).date_naive());
    let day = if Some(date) == today { String::new() } else { format!("{} ", date.format("%b %d")) };
    let minutes = ((time.timestamp() - now).max(0) + 59) / 60;
    let relative = if minutes == 0 {
        "now".into()
    } else if minutes < 90 {
        format!("in {minutes} minute{}", if minutes == 1 { "" } else { "s" })
    } else if minutes < 48 * 60 {
        format!("in {}h {}m", minutes / 60, minutes % 60)
    } else {
        format!("in {}d {}h", minutes / (24 * 60), minutes / 60 % 24)
    };
    format!("{relative} · {day}{}", local_time(time.timestamp()))
}

fn reset_time(timestamp: Option<i64>) -> String {
    reset_time_at(timestamp, chrono::Local::now().timestamp())
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

/// Projection of the pinned chart, refreshed from chart state rather than answer text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataBasis {
    pub sample: bool,
    pub last_bar: Option<i64>,
    pub latest_price_update: Option<i64>,
    pub incomplete: Option<String>,
}
impl DataBasis {
    pub fn from_chart(chart: &crate::chart_view::ChartState, sample: bool) -> Self {
        let bars = chart.source_bars();
        Self {
            sample,
            last_bar: bars.bars.last().map(|bar| bar.ts),
            latest_price_update: chart.price_update_time(),
            incomplete: bars.last_in_progress.then(|| match chart.tf.to_string().as_str() {
                "1W" => "Weekly".into(),
                "1D" => "Daily".into(),
                other => other.to_owned(),
            }),
        }
    }

    fn freshness(&self) -> Option<String> {
        self.latest_price_update
            .map(|ts| format!("as of {}", local_time(ts)))
            .or_else(|| self.last_bar.map(|ts| format!("bar opened {}", local_time(ts))))
    }
}

pub struct AgentPane {
    pub settings: Settings,
    pub workflows: crate::agent_bridge::Workflows,
    pub proposal_focus: HashMap<egui::Id, String>,
    /// Only buttons have an Enter action; other card controls only own Escape.
    pub proposal_buttons: HashMap<egui::Id, bool>,
    /// Pending proposal ids drawn last frame, per turn; persists across frames.
    proposal_seen: HashMap<String, Vec<u64>>,
    /// Pending ids drawn in both the previous and the current frame. Approve and
    /// Decline act only on these, so a proposal that arrives as the user clicks is
    /// never decided without having been on screen.
    pub proposal_shown: HashMap<String, Vec<u64>>,
    symbol_seen: HashMap<String, Vec<(u64, usize, String)>>,
    pub symbol_shown: HashMap<String, Vec<(u64, usize, String)>>,
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
    pub data_basis: DataBasis,
    pub queue_review: bool,
    pub focus_shortcut: String,
    active_turn: Option<usize>,
    header_undo: Option<String>,
    header_undo_available: bool,
    replies: HashMap<String, ReplyMessage>,
    reply_cache: HashMap<usize, Vec<agent_markdown::DisplayBlock>>,
    limits_value: Value,
    coalescer: Coalescer,
    queue_paused: bool,
    retryable: bool,
    notice_levels: HashMap<usize, Severity>,
    pub expanded_tools: std::collections::HashSet<usize>,
}
impl Default for AgentPane {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            workflows: Default::default(),
            proposal_focus: Default::default(),
            proposal_buttons: Default::default(),
            proposal_seen: Default::default(),
            proposal_shown: Default::default(),
            symbol_seen: Default::default(),
            symbol_shown: Default::default(),
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
            data_basis: DataBasis::default(),
            queue_review: false,
            focus_shortcut: "Cmd/Ctrl+L".into(),
            active_turn: None,
            header_undo: None,
            header_undo_available: false,
            replies: HashMap::new(),
            reply_cache: HashMap::new(),
            limits_value: Value::Null,
            coalescer: Coalescer::default(),
            queue_paused: false,
            retryable: false,
            notice_levels: HashMap::new(),
            expanded_tools: Default::default(),
        }
    }
}

pub enum Action {
    Check,
    SignIn,
    CancelLogin,
    ReopenLogin,
    InstallationInstructions,
    RefreshLimits,
    Interrupt,
    Retry,
    Send(Prompt),
    SetTools(ToolsMode),
    UndoTurn(String),
    /// `ids` are the pending proposals the user saw; others in the turn stay pending.
    Confirm {
        turn: String,
        approve: bool,
        ids: Vec<u64>,
    },
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
        if mode == ToolsMode::ReadOnly {
            self.workflows.reset();
        }
        self.queue_paused = true;
        self.notice(&format!("{} on restart. Queue stays paused; nothing is resent.", mode.label()));
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
        let reverted = result.as_ref().ok().map(|summary| summary.reverted);
        let text = match result {
            Ok(summary) => format!(
                "{} change{} reverted{}",
                summary.reverted,
                if summary.reverted == 1 { "" } else { "s" },
                if summary.skipped.is_empty() {
                    String::new()
                } else if summary.skipped.iter().any(|reason| reason.contains(' ')) {
                    format!(" · {} kept: {}", summary.skipped.len(), summary.skipped.join("; "))
                } else {
                    format!(" · {} kept because you edited them", summary.skipped.len())
                }
            ),
            Err(reason) => scrub_text(&reason),
        };
        if let Some(turn) = self.turns.iter_mut().find(|turn| turn.id.as_deref() == Some(id)) {
            turn.undo_summary = Some(text);
            if let Some(reverted) = reverted {
                turn.reverted_changes = Some(reverted);
            }
        } else {
            self.notice(&text);
        }
    }

    fn finish_turn(&mut self) {
        if let Some(index) = self.active_turn.take() {
            let turn = &mut self.turns[index];
            turn.duration = Some(turn.started_at.elapsed());
            turn.finished = true;
        }
    }

    pub fn notice(&mut self, text: &str) {
        self.notice_levels.insert(self.blocks.len(), notice_severity(text));
        self.blocks.push(RenderedBlock::Notice(scrub_text(text)));
    }

    pub fn error_notice(&mut self, text: &str) {
        self.notice(text);
        self.notice_levels.insert(self.blocks.len() - 1, Severity::Error);
    }

    pub fn transition(&mut self, state: Onboarding) {
        if matches!(self.state, Onboarding::Stopped { unsafe_backend: true, .. }) {
            return;
        }
        // Pause only when work is cut off; onboarding states before Ready have nothing to protect.
        if matches!(
            state,
            Onboarding::RateLimited { .. } | Onboarding::ToolsUnavailable { .. } | Onboarding::Stopped { .. }
        ) && (self.running || !self.queue.is_empty())
        {
            self.queue_paused = true;
        }
        if matches!(state, Onboarding::Stopped { .. }) {
            self.workflows.reset();
            self.flush(Instant::now());
            self.running = false;
            self.interrupt_tools();
            self.finish_turn();
        }
        self.state = state;
    }

    pub fn interrupt(&mut self) {
        self.retryable = true;
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
            // An empty, idle queue holds no stale prompts, so a pause has nothing left to guard.
            if self.queue.is_empty() && !self.running {
                self.queue_paused = false;
            }
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
            final_answer_succeeded: false,
            made_edits: false,
            chart_changes: None,
            undo_available: false,
            undo_summary: None,
            reverted_changes: None,
            local_time: local_time(chrono::Local::now().timestamp()),
            started_at: Instant::now(),
            duration: None,
        });
        self.retryable = false;
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
            BridgeEvent::Wire(WireEvent::Error(reason)) => self.error_notice(&reason),
            BridgeEvent::Started { turn } => {
                if let Some(index) = self.active_turn {
                    self.turns[index].id.get_or_insert(turn);
                }
            }
            BridgeEvent::Completed { turn, status, .. } => {
                // Completion also supplies the canonical id if Started was absent.
                if let Some(index) = self.active_turn {
                    self.turns[index].id.get_or_insert(turn);
                    let start = self.turns[index].prompt_index;
                    let last_failure = self.blocks[start..]
                        .iter()
                        .rposition(|block| matches!(block, RenderedBlock::Tool(t) if t.success == Some(false)))
                        .map(|i| start + i);
                    self.turns[index].final_answer_succeeded = status == "completed"
                        && self
                            .replies
                            .values()
                            .any(|reply| !reply.text.trim().is_empty() && last_failure.is_none_or(|i| reply.index > i));
                }
                self.finish_turn();
                self.flush(Instant::now());
                self.running = false;
                if status != "completed" {
                    self.retryable = true;
                    self.interrupt_tools();
                    self.queue_paused = true;
                    if matches!(status.as_str(), "interrupted" | "cancelled") {
                        self.notice(INTERRUPTED);
                    } else {
                        self.notice(&format!("Turn {status}. Answer is provisional; retry when you're ready."));
                    }
                }
            }
            BridgeEvent::ToolsUnavailable { reason } => {
                self.finish_turn();
                self.flush(Instant::now());
                self.running = false;
                self.retryable = true;
                self.transition(Onboarding::ToolsUnavailable { reason: scrub_text(&reason) });
            }
            BridgeEvent::Recovered(recovery) => {
                if matches!(&recovery, Recovery::HistoryLost { .. }) {
                    self.workflows.reset();
                }
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
                let summary = workflow_tool_target(name, &item["arguments"], &self.workflows.proposals).or_else(|| {
                    item["editSummary"].as_str().map(short_label).or_else(|| edit_tool_summary(item)).or_else(|| {
                        let args = &item["arguments"];
                        Some(short_label(
                            args["symbol"].as_str().or_else(|| args["query"].as_str()).unwrap_or("Pinned chart"),
                        ))
                    })
                });
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
                    target_args: normalized_args(&item["arguments"]),
                    summary,
                    interrupted: false,
                    error: tool_error(item),
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
                    self.error_notice(reason);
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
        if ui
            .memory(|m| m.focused())
            .is_some_and(|id| self.proposal_focus.contains_key(&id) && !self.proposal_buttons.contains_key(&id))
        {
            // egui checkboxes also accept Enter by default; cards reserve it for buttons.
            ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        }
        self.proposal_focus.clear();
        self.proposal_buttons.clear();
        egui::Frame::new().inner_margin(dp(12.0)).show(ui, |ui| self.show_content(ui, pin_label, pane_id)).inner
    }

    fn show_content(&mut self, ui: &mut Ui, pin_label: &str, pane_id: egui::Id) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some(delay) = self.coalescer.due_in(Instant::now(), true) {
            if delay.is_zero() {
                self.flush(Instant::now());
            } else {
                ui.ctx().request_repaint_after(delay);
            }
        }
        ui.spacing_mut().item_spacing = egui::vec2(dp(8.0), dp(8.0));
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
        ui.style_mut().visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, theme::accent());
        // Recompute relative reset copy on event-driven frames, without a countdown timer.
        if matches!(self.state, Onboarding::RateLimited { .. }) && !self.limits_value.is_null() {
            self.set_limits(&self.limits_value.clone());
        }
        let old_model = self.settings.model.clone();
        self.header(ui, pin_label, &mut actions);
        if old_model != self.settings.model {
            self.set_limits(&self.limits_value.clone());
        }
        ui.add_space(dp(6.0));

        // Reserve the measured composer height, rather than a fixed empty footer.
        let input_width = (ui.available_width() - self.composer_controls_width(ui) - dp(18.0)).max(dp(40.0));
        let input_text = if self.input.is_empty() { "Ask about the pinned chart" } else { &self.input };
        let input_job = agent_markdown::wrapped_job(input_text, input_width, theme::body_font(13.0), theme::text());
        let text_height = ui.fonts_mut(|f| f.layout_job(input_job)).size().y;
        let line_height = ui.fonts_mut(|f| f.row_height(&theme::body_font(13.0)));
        let input_height = text_height.clamp(line_height, line_height * 6.0);
        let queue_height =
            if self.queue.is_empty() { 0.0 } else { dp(28.0) + if self.queue_review { dp(104.0) } else { 0.0 } };
        let footer_height =
            if self.acknowledged { input_height.max(dp(24.0)) + dp(72.0) + queue_height } else { dp(24.0) };
        let height = (ui.available_height() - footer_height).max(dp(20.0));
        egui::ScrollArea::vertical()
            .id_salt(pane_id.with("conversation"))
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .max_height(height)
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width());
                if self.disclosure || !self.acknowledged {
                    agent_markdown::wrapped(ui, "Privacy", theme::text());
                    for (heading, body) in PRIVACY_GROUPS {
                        ui.label(RichText::new(*heading).font(theme::ui_font(10.5)).color(theme::text()));
                        agent_markdown::wrapped(ui, body, theme::text());
                    }
                    if !self.acknowledged && control(ui, "Continue").clicked() {
                        self.acknowledged = true;
                        self.disclosure = false;
                    }
                    ui.add_space(dp(6.0));
                }
                if !self.acknowledged {
                    return;
                }
                if self.blocks.is_empty() && self.state == Onboarding::Ready {
                    agent_markdown::wrapped(
                        ui,
                        "Read trends, levels and momentum on the pinned chart; Chart edits also lets you mark it up.",
                        readable(theme::muted()),
                    );
                    suggested_prompts(ui, self.settings.tools, &mut actions);
                }
                self.conversation(ui, &mut actions);
                if self.state != Onboarding::Ready {
                    if !self.blocks.is_empty() {
                        ui.add_space(dp(9.0));
                    }
                    self.onboarding(ui, &mut actions);
                }
                for limit in &self.limits {
                    let window = match limit.minutes {
                        300 => "5-hour".into(),
                        10080 => "Weekly".into(),
                        n => format!("{n}-minute"),
                    };
                    let source = if limit.id == "ordinary" { String::new() } else { format!("{} · ", limit.id) };
                    let usage = format!("{source}{window} limit {:.0}% used", limit.used);
                    let label = if matches!(self.state, Onboarding::RateLimited { .. }) {
                        usage
                    } else {
                        format!("{usage} · resets {}", limit.reset)
                    };
                    agent_markdown::wrapped(ui, &label, readable(theme::muted()));
                }
            });
        if self.acknowledged {
            self.queue_row(ui);
            self.composer(ui, pane_id, input_height, &mut actions);
        }
        ui.add(
            egui::Label::new(
                RichText::new("Not investment advice").font(theme::ui_font(10.5)).color(readable(theme::muted())),
            )
            .wrap(),
        );
        actions
    }

    fn onboarding(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        agent_markdown::show(ui, &[agent_markdown::DisplayBlock::Heading(2, self.state.title().into())]);
        let response = agent_markdown::wrapped(ui, &self.state.description(), theme::text());
        match &self.state {
            Onboarding::ToolsUnavailable { reason } | Onboarding::Stopped { reason, .. } => {
                response.on_hover_text(reason);
            }
            _ => {}
        }
        ui.add_space(dp(3.0));
        if matches!(self.state, Onboarding::Checking | Onboarding::SigningIn) {
            ui.horizontal_top(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(dp(14.0), dp(18.0)), egui::Sense::hover());
                // Static, drawn activity: progress follows events, never an idle animation timer.
                widgets::draw_icon(ui.painter(), rect, Icon::Running, readable(theme::muted()));
                agent_markdown::wrapped(ui, CHECKING_STATUS, theme::text());
            });
        }
        if self.state == Onboarding::Checking {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            let primary = ui.add(primary_button(self.state.primary()));
            let primary = if matches!(self.state, Onboarding::ToolsUnavailable { .. }) {
                primary.on_hover_text(RETRY_HELP)
            } else {
                primary
            };
            focus_ring(ui, &primary);
            if primary.clicked()
                && let Some(action) = self.state.action()
            {
                actions.push(action);
            }
            if matches!(self.state, Onboarding::Missing | Onboarding::Unsupported { .. })
                && control(ui, "Installation instructions")
                    .on_hover_text(format!("Open {INSTALLATION_URL} in your browser"))
                    .clicked()
            {
                actions.push(Action::InstallationInstructions);
            }
        });
        if self.state == Onboarding::SigningIn && control(ui, "Open sign-in page again").clicked() {
            actions.push(Action::ReopenLogin);
        }
    }

    fn conversation(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        // Expire before deriving headers as well as cards on the single deadline wake.
        self.workflows.expire(Instant::now(), None);
        for i in 0..self.blocks.len() {
            match &self.blocks[i] {
                RenderedBlock::Prompt(text) => {
                    if i > 0 {
                        // Two related-element gaps plus a hairline: 16–24 dp between turns.
                        ui.add(egui::Separator::default().spacing(dp(1.0)));
                    }
                    let turn = self.turns.iter().find(|t| t.prompt_index == i);
                    let time = turn.map_or("", |t| t.local_time.as_str());
                    prompt_header(ui, time);
                    agent_markdown::wrapped(ui, text, theme::text());
                    if let Some(turn) = turn
                        && let Some(summary) = &turn.undo_summary
                        && turn.reverted_summary() != Some(summary.as_str())
                    {
                        notice_row(ui, summary, notice_severity(summary));
                    }
                    let end = self.blocks[i + 1..]
                        .iter()
                        .position(|b| matches!(b, RenderedBlock::Prompt(_)))
                        .map_or(self.blocks.len(), |n| i + 1 + n);
                    self.tools(ui, i, i + 1..end, actions);
                    ui.add_space(dp(3.0));
                }
                RenderedBlock::Reply(blocks) => {
                    if self
                        .turns
                        .iter()
                        .rev()
                        .find(|turn| turn.prompt_index < i)
                        .is_some_and(|turn| turn.reverted_changes.is_some_and(|n| n > 0))
                    {
                        ui.label(RichText::new("Reverted").font(theme::ui_font(10.5)).color(readable(theme::muted())));
                    }
                    ui.push_id(("reply", i), |ui| {
                        if let Some(cached) = self.reply_cache.get(&i) {
                            agent_markdown::show(ui, cached);
                        } else {
                            agent_markdown::show(ui, &agent_markdown::prepare(blocks));
                        }
                    });
                    // Host-authored feedback follows the last answer block of this turn;
                    // the model's (possibly now stale) answer remains untouched.
                    let end = self.blocks[i + 1..]
                        .iter()
                        .position(|b| matches!(b, RenderedBlock::Prompt(_)))
                        .map_or(self.blocks.len(), |n| i + 1 + n);
                    if !self.blocks[i + 1..end].iter().any(|b| matches!(b, RenderedBlock::Reply(_)))
                        && let Some(line) = self
                            .turns
                            .iter()
                            .rev()
                            .find(|t| t.prompt_index < i)
                            .and_then(|t| t.id.as_deref())
                            .and_then(|id| self.workflows.decision_line(id))
                    {
                        ui.label(RichText::new(line).font(theme::mono(10.5)).color(readable(theme::muted())));
                    }
                }
                RenderedBlock::Notice(text) => {
                    // A stop has one block and one adjacent action; its diagnostic stays in the tooltip.
                    if !matches!(&self.state, Onboarding::Stopped { reason, .. } if reason == text) {
                        if text == INTERRUPTED || (text.starts_with("Turn ") && text.contains("Answer is provisional"))
                        {
                            ui.horizontal_wrapped(|ui| {
                                notice_row(ui, text, Severity::Warning);
                                if self.show_retry()
                                    && self.turns.last().is_some_and(|turn| i > turn.prompt_index)
                                    && ui.add(primary_button("Retry response")).on_hover_text(RETRY_HELP).clicked()
                                {
                                    actions.push(Action::Retry);
                                }
                            });
                        } else {
                            notice_row(
                                ui,
                                text,
                                self.notice_levels.get(&i).copied().unwrap_or_else(|| notice_severity(text)),
                            );
                        }
                    }
                }
                RenderedBlock::Tool(_) => {
                    // Tool blocks stay in event order in memory; one inspector row per turn in the UI.
                    if !self.blocks[..i].iter().any(|b| matches!(b, RenderedBlock::Prompt(_) | RenderedBlock::Tool(_)))
                    {
                        self.tools(ui, i, i..self.blocks.len(), actions);
                    }
                }
            }
        }
    }

    fn tools(&mut self, ui: &mut Ui, key: usize, range: std::ops::Range<usize>, actions: &mut Vec<Action>) {
        let traces: Vec<_> = self.blocks[range]
            .iter()
            .filter_map(|b| if let RenderedBlock::Tool(t) = b { Some(t) } else { None })
            .collect();
        let turn = self.turns.iter().find(|turn| turn.prompt_index == key);
        let turn_id = turn.and_then(|turn| turn.id.clone());
        if traces.is_empty() && !turn.is_some_and(|turn| turn.made_edits) {
            return;
        }
        let reverted = turn.and_then(|turn| turn.reverted_changes);
        let label = if traces.is_empty() && reverted.is_none() {
            "Chart changes".into()
        } else {
            turn.and_then(ConversationTurn::reverted_summary).map_or_else(
                || tool_group_label_outcome(&traces, reverted, turn.is_some_and(|t| t.final_answer_succeeded)),
                str::to_owned,
            )
        };
        let label = if reverted.is_none() {
            turn_id.as_deref().and_then(|id| self.workflows.turn_summary(id)).map_or(label.clone(), |summary| {
                let chart_edits: Vec<_> = traces.iter().copied().filter(|trace| is_edit_tool(&trace.name)).collect();
                let summary = if chart_edits.is_empty() {
                    summary
                } else {
                    format!("{} · {summary}", tool_group_summary_outcome(&chart_edits, None, false))
                };
                tool_step_suffix(summary, &traces)
            })
        } else {
            label
        };
        let label = tool_group_duration(label, turn);
        let recovered = recovered_failures(&traces);
        let retries =
            if traces.iter().filter(|trace| trace.success == Some(false)).count() == recovered { recovered } else { 0 };
        let label = retry_suffix(label, retries);
        let expanded = self.expanded_tools.contains(&key);
        let wash = ui.painter().add(egui::Shape::Noop);
        let response = ui
            .with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if let Some(turn) =
                    turn.filter(|turn| turn.finished && turn.made_edits && turn.reverted_changes.is_none())
                {
                    let changes = turn.chart_changes.unwrap_or_else(|| {
                        traces.iter().filter(|trace| is_edit_tool(&trace.name) && trace.success == Some(true)).count()
                    });
                    let label = if changes == 0 {
                        "Undo chart changes".into()
                    } else {
                        if turn_id.as_ref().is_some_and(|id| self.workflows.undo.iter().any(|(owner, _)| owner == id)) {
                            format!("Undo {changes} change{}", if changes == 1 { "" } else { "s" })
                        } else {
                            format!("Undo {changes} chart change{}", if changes == 1 { "" } else { "s" })
                        }
                    };
                    if undo_button(ui, &label, turn.undo_available && !self.running, self.running).clicked()
                        && let Some(id) = &turn.id
                    {
                        actions.push(Action::UndoTurn(id.clone()));
                    }
                }
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), 0.0),
                    egui::Layout::left_to_right(egui::Align::Min),
                    |ui| {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(dp(14.0), dp(18.0)), egui::Sense::hover());
                        widgets::draw_icon(
                            ui.painter(),
                            rect,
                            if expanded { Icon::ChevronDown } else { Icon::ChevronRight },
                            readable(theme::muted()),
                        );
                        if reverted.is_some()
                            || traces.iter().any(|trace| trace.success.is_none() && !trace.interrupted)
                        {
                            let (rect, _) =
                                ui.allocate_exact_size(egui::vec2(dp(14.0), dp(18.0)), egui::Sense::hover());
                            widgets::draw_icon(
                                ui.painter(),
                                rect,
                                if reverted.is_some() { Icon::Revert } else { Icon::Running },
                                readable(theme::muted()),
                            );
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new(&label).font(theme::ui_font(12.0)).color(readable(theme::muted())),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&label);
                    },
                )
                .response
            })
            .inner;
        let click = ui.interact(response.rect, ui.id().with(("tools", key)), egui::Sense::click());
        if click.hovered() {
            ui.painter()
                .set(wash, egui::epaint::RectShape::filled(click.rect, theme::CONTROL_RADIUS, theme::control()));
        }
        focus_ring(ui, &click);
        if click.clicked() {
            if expanded {
                self.expanded_tools.remove(&key);
            } else {
                self.expanded_tools.insert(key);
            }
        }
        if expanded {
            for trace in traces {
                tool_row_with_history(ui, trace, reverted.is_some_and(|n| n > 0));
            }
        }
        if let Some(id) = turn_id {
            self.confirmation_card(ui, &id, actions);
        }
    }

    fn confirmation_card(&mut self, ui: &mut Ui, turn: &str, actions: &mut Vec<Action>) {
        use crate::agent_bridge::ProposalStatus;
        let now = Instant::now();
        self.workflows.expire(now, None);
        let rows: Vec<_> =
            self.workflows.proposals.iter().enumerate().filter(|(_, p)| p.turn == turn).map(|(i, _)| i).collect();
        if rows.is_empty() {
            self.proposal_seen.remove(turn);
            self.proposal_shown.remove(turn);
            self.symbol_seen.remove(turn);
            self.symbol_shown.remove(turn);
            return;
        }
        let pending: Vec<u64> = rows
            .iter()
            .map(|&i| &self.workflows.proposals[i])
            .filter(|p| p.status == ProposalStatus::Pending)
            .map(|p| p.id)
            .collect();
        let seen = self.proposal_seen.insert(turn.into(), pending.clone()).unwrap_or_default();
        let symbols: Vec<_> = rows
            .iter()
            .flat_map(|&i| {
                let p = &self.workflows.proposals[i];
                p.symbols.iter().enumerate().map(move |(index, s)| (p.id, index, s.symbol.clone()))
            })
            .collect();
        let seen_symbols = self.symbol_seen.insert(turn.into(), symbols.clone()).unwrap_or_default();
        let shown_symbols: Vec<_> = symbols.into_iter().filter(|s| seen_symbols.contains(s)).collect();
        // Fail closed for the whole proposal if any sub-row is new. This also prevents
        // unticked, newly arrived symbols being implicitly declined by Approve selected.
        let shown: Vec<u64> = pending
            .into_iter()
            .filter(|id| {
                seen.contains(id)
                    && rows.iter().all(|&i| {
                        let p = &self.workflows.proposals[i];
                        p.id != *id
                            || p.symbols
                                .iter()
                                .enumerate()
                                .all(|(n, s)| shown_symbols.contains(&(p.id, n, s.symbol.clone())))
                    })
            })
            .collect();
        self.symbol_shown.insert(turn.into(), shown_symbols);
        self.proposal_shown.insert(turn.into(), shown.clone());
        let multi = rows.len() > 1 || rows.iter().any(|&i| !self.workflows.proposals[i].symbols.is_empty());
        let focused = ui.memory(|m| m.focused());
        let card_focused = focused.is_some_and(|id| self.proposal_focus.get(&id).is_some_and(|owner| owner == turn));
        if card_focused {
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                actions.push(Action::Confirm { turn: turn.into(), approve: false, ids: shown.clone() });
            }
            if focused.is_some_and(|id| !self.proposal_buttons.contains_key(&id)) {
                ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            }
        }
        ui.push_id(("workflow-card", turn), |ui| {
            egui::Frame::new().fill(theme::header()).inner_margin(dp(6.0)).show(ui, |ui| {
                for &index in &rows {
                    let proposal = &mut self.workflows.proposals[index];
                    ui.horizontal_wrapped(|ui| {
                        if multi && proposal.symbols.is_empty() && proposal.status == ProposalStatus::Pending {
                            let response = ui.checkbox(&mut proposal.selected, "");
                            self.proposal_focus.insert(response.id, turn.into());
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new(proposal.card_summary()).font(theme::ui_font(12.0)).color(theme::text()),
                            )
                            .wrap(),
                        );
                        let status = match &proposal.status {
                            ProposalStatus::Pending => "Awaiting approval",
                            ProposalStatus::Approved => "Approved",
                            ProposalStatus::Declined => "Declined",
                            ProposalStatus::Expired => "Expired",
                            ProposalStatus::Failed(reason) => reason.as_str(),
                        };
                        ui.label(RichText::new(status).font(theme::ui_font(10.5)).color(theme::muted()));
                    });
                    for (n, symbol) in proposal.symbols.iter_mut().enumerate() {
                        ui.push_id((proposal.id, n), |ui| {
                            ui.horizontal(|ui| {
                                ui.add_space(dp(12.0));
                                let label = crate::security_names::label(&symbol.symbol);
                                if proposal.status == ProposalStatus::Pending {
                                    let response = ui.checkbox(&mut symbol.selected, label);
                                    focus_ring(ui, &response);
                                    self.proposal_focus.insert(response.id, turn.into());
                                } else {
                                    ui.label(RichText::new(label).font(theme::ui_font(12.0)).color(theme::text()));
                                    if let Some(outcome) = &symbol.outcome {
                                        ui.label(
                                            RichText::new(outcome).font(theme::ui_font(10.5)).color(theme::muted()),
                                        );
                                    }
                                }
                            });
                        });
                    }
                    let note = match &proposal.args {
                        tr_agent_tools::args::LiveArgs::CreateAlert(args) => args.note.as_deref(),
                        _ => proposal.before["note"].as_str(),
                    };
                    if let Some(note) = note {
                        ui.add(
                            egui::Label::new(
                                RichText::new(note.replace(['\n', '\r'], " "))
                                    .font(theme::ui_font(10.5))
                                    .color(theme::muted()),
                            )
                            .truncate(),
                        )
                        .on_hover_text(note);
                    }
                    if let Some(label) = proposal.ownership_label() {
                        ui.label(RichText::new(label).font(theme::ui_font(10.5)).color(theme::muted()));
                    }
                    if proposal.status == ProposalStatus::Pending {
                        // One deadline wake, never a periodic/idle repaint.
                        ui.ctx().request_repaint_after(proposal.deadline.saturating_duration_since(now));
                    }
                }
                if rows.iter().any(|&i| self.workflows.proposals[i].status == ProposalStatus::Pending) {
                    ui.horizontal(|ui| {
                        let selected = rows.iter().any(|&i| {
                            let p = &self.workflows.proposals[i];
                            p.status == ProposalStatus::Pending && p.is_selected() && shown.contains(&p.id)
                        });
                        let approve = ui
                            .add_enabled(selected, primary_button(if multi { "Approve selected" } else { "Approve" }));
                        let decline = control(ui, if multi { "Decline all" } else { "Decline" });
                        self.proposal_focus.insert(approve.id, turn.into());
                        self.proposal_focus.insert(decline.id, turn.into());
                        if selected {
                            self.proposal_buttons.insert(approve.id, true);
                        }
                        self.proposal_buttons.insert(decline.id, false);
                        let focused = approve.has_focus() || decline.has_focus();
                        let enter = focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
                        let escape =
                            focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                        if (approve.clicked() || (enter && approve.has_focus())) && selected {
                            actions.push(Action::Confirm { turn: turn.into(), approve: true, ids: shown.clone() });
                        }
                        if decline.clicked() || (enter && decline.has_focus()) || escape {
                            actions.push(Action::Confirm { turn: turn.into(), approve: false, ids: shown.clone() });
                        }
                    });
                }
            });
        });
    }

    fn show_retry(&self) -> bool {
        self.last_prompt.is_some()
            && !self.running
            && (self.retryable || matches!(self.state, Onboarding::ToolsUnavailable { .. }))
            && (self.state == Onboarding::Ready || matches!(self.state, Onboarding::ToolsUnavailable { .. }))
    }

    fn queue_label(&self) -> String {
        format!("{} queued{}", self.queue.len(), if self.queue_paused { " · Paused" } else { "" })
    }

    fn queue_row(&mut self, ui: &mut Ui) {
        if self.queue.is_empty() {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(self.queue_label()).font(theme::ui_font(10.5)).color(readable(theme::muted())));
            if control(ui, if self.queue_review { "Hide queue" } else { "Review" }).clicked() {
                self.queue_review = !self.queue_review;
            }
            if self.queue_paused && control(ui, "Resume").clicked() {
                self.resume_queue();
            }
            if control(ui, "Clear").clicked() {
                self.queue.clear();
            }
        });
        if self.queue_review && !self.queue.is_empty() {
            egui::ScrollArea::vertical().id_salt("queue-review").max_height(dp(96.0)).show(ui, |ui| {
                let mut remove = None;
                for (index, prompt) in self.queue.iter().enumerate() {
                    ui.push_id(index, |ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            if control(ui, "Remove").on_hover_text("Remove this queued prompt").clicked() {
                                remove = Some(index);
                            }
                            ui.allocate_ui_with_layout(
                                egui::vec2(ui.available_width(), 0.0),
                                egui::Layout::left_to_right(egui::Align::Min),
                                |ui| {
                                    agent_markdown::wrapped(ui, &format!("{}.", index + 1), theme::text());
                                    agent_markdown::wrapped(ui, &prompt.text, theme::text());
                                },
                            );
                        });
                    });
                }
                if let Some(index) = remove {
                    self.queue.remove(index);
                }
            });
        }
    }

    fn composer_label(&self) -> &'static str {
        if self.running {
            "Stop"
        } else if self.state.accepts_input() && self.queue_paused && !self.queue.is_empty() {
            "Add to queue"
        } else {
            "Send"
        }
    }

    fn send_blocked_reason(&self) -> Option<&'static str> {
        Some(match self.state {
            Onboarding::Dormant => "Start the agent before sending a prompt.",
            Onboarding::Missing => "Install Codex, then check again before sending.",
            Onboarding::Unsupported { .. } => "Install a supported Codex version, then check again before sending.",
            Onboarding::SignedOut => "Sign in with ChatGPT before sending a prompt.",
            Onboarding::SigningIn => "Finish signing in before sending a prompt.",
            Onboarding::Checking => "Wait for Codex, account and chart-tool checks to finish.",
            Onboarding::RateLimited { .. } => "Wait for the usage limit to reset, then check limits.",
            Onboarding::Stopped { unsafe_backend: true, .. } => "Close this unsafe session and start a new one.",
            Onboarding::Stopped { .. } => "Restart the agent before sending a prompt.",
            _ if self.queue.len() >= 32 => "The queue is full. Remove a prompt or clear the queue first.",
            _ if self.input.trim().is_empty() => "Enter a prompt first.",
            _ => return None,
        })
    }

    fn composer_button(&self, ui: &mut Ui) -> egui::Response {
        let blocked = self.send_blocked_reason();
        let enabled = self.running || blocked.is_none();
        let button = if self.running {
            egui::Button::new(self.composer_label()).wrap()
        } else if enabled {
            primary_button(self.composer_label())
        } else {
            egui::Button::new(RichText::new(self.composer_label()).color(readable(theme::muted())))
                .fill(theme::control())
                .wrap()
        };
        let response = ui.add_enabled(enabled, button);
        focus_ring(ui, &response);
        response
            .on_hover_text(if self.running {
                "Interrupt (Esc)"
            } else if self.queue_paused && !self.queue.is_empty() {
                "Add this prompt to the queue. It will wait for Resume."
            } else {
                "Send (Enter)"
            })
            .on_disabled_hover_text(blocked.unwrap_or("Enter a prompt first."))
    }

    fn composer_controls_width(&self, ui: &Ui) -> f32 {
        ui.painter()
            .layout_no_wrap(self.composer_label().into(), egui::TextStyle::Button.resolve(ui.style()), theme::text())
            .size()
            .x
            .ceil()
            + ui.spacing().button_padding.x * 2.0
            + dp(8.0)
    }

    fn composer(&mut self, ui: &mut Ui, pane_id: egui::Id, height: f32, actions: &mut Vec<Action>) {
        let input_id = input_id(pane_id);
        let enter = ui.memory(|m| m.has_focus(input_id))
            && ui.input_mut(|i| {
                let plain = i.events.iter().any(|event| {
                    matches!(event,
                egui::Event::Key { key: egui::Key::Enter, pressed: true, modifiers, .. } if modifiers.is_none())
                });
                plain && i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
            });
        let accepts_input = self.state.accepts_input();
        let placeholder = self.state.composer_placeholder();
        let input_help = format!(
            "{} focuses agent input · Enter sends · Shift+Enter adds a line · Esc interrupts",
            self.focus_shortcut
        );
        let mut send = false;
        let mut field = egui::Frame::new()
            .fill(theme::control())
            .stroke(egui::Stroke::new(dp(1.0), theme::border()))
            .corner_radius(dp(6.0) as u8)
            .inner_margin(egui::Margin::symmetric(dp(8.0) as i8, dp(6.0) as i8))
            .begin(ui);
        field.content_ui.set_min_width(field.content_ui.available_width());
        field.content_ui.horizontal_top(|ui| {
            let width = (ui.available_width() - self.composer_controls_width(ui)).max(dp(40.0));
            ui.allocate_ui_with_layout(egui::vec2(width, height), egui::Layout::top_down(egui::Align::Min), |ui| {
                // Esc stays owned by app hotkeys; idle Esc releases TextEdit focus.
                egui::ScrollArea::vertical()
                    .id_salt(pane_id.with("input-scroll"))
                    .max_height(height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.add_enabled(
                            accepts_input,
                            egui::TextEdit::multiline(&mut self.input)
                                .id(input_id)
                                .font(theme::body_font(13.0))
                                .frame(egui::Frame::NONE)
                                .margin(egui::Vec2::ZERO)
                                .desired_rows(1)
                                .desired_width(ui.available_width())
                                .hint_text(RichText::new(placeholder).color(readable(theme::muted())))
                                .char_limit(16000),
                        )
                        .on_hover_text(&input_help)
                        .on_disabled_hover_text(&input_help);
                    });
            });
            if self.composer_button(ui).clicked() {
                if self.running {
                    actions.push(Action::Interrupt);
                } else {
                    send = true;
                }
            }
        });
        if ui.memory(|m| m.has_focus(input_id)) {
            field.frame.stroke.color = theme::accent();
        }
        field.end(ui);
        // Enter still appends to the queue during a running turn.
        if (send || enter) && self.send_blocked_reason().is_none() {
            actions.push(Action::Send(Prompt { text: std::mem::take(&mut self.input), requires_tools: false }));
        }
    }

    fn status(&self) -> &'static str {
        if self.running {
            return "Working";
        }
        match self.state {
            Onboarding::Ready if self.queue_paused && !self.queue.is_empty() => "Paused",
            Onboarding::Ready => "Ready",
            Onboarding::Dormant => "Idle",
            Onboarding::SignedOut => "Signed out",
            Onboarding::SigningIn => "Signing in",
            Onboarding::Checking => "Checking",
            Onboarding::Missing | Onboarding::Unsupported { .. } => "Unavailable",
            Onboarding::RateLimited { .. } => "Paused",
            Onboarding::ToolsUnavailable { .. } => "Unverified",
            Onboarding::Stopped { .. } => "Stopped",
        }
    }

    fn header(&mut self, ui: &mut Ui, pin_label: &str, actions: &mut Vec<Action>) {
        self.settings.select_defaults(&self.models);
        let model = self.models.iter().find(|m| m.id == self.settings.model);
        let model_label = format!(
            "{} · {}",
            model.map_or("Model", |m| m.display_name.as_str()),
            if self.settings.effort.is_empty() { "effort" } else { &self.settings.effort }
        );
        let pin_label = pin_label.trim_start_matches("Pinned: ");
        let width_of = |text: &str| {
            ui.painter().layout_no_wrap(text.into(), theme::ui_font(10.5), theme::text()).size().x + dp(12.0)
        };
        let model_width = width_of(&model_label) + dp(16.0);
        let mode_width = width_of("Read only") + width_of("Chart edits");
        let style_width = width_of(self.settings.response_style.label()) + dp(16.0);
        let status_width = width_of(self.status()) + dp(8.0);
        let more_width = dp(22.0);
        let total = model_width + mode_width + style_width + status_width + more_width + dp(32.0);
        let narrow = header_rows(ui.available_width(), total) == 2;
        let budget = (ui.available_width() - mode_width - status_width - more_width - dp(32.0)).max(dp(36.0));
        ui.horizontal_top(|ui| {
            self.model_picker(ui, &model_label, if narrow { model_width.min(budget) } else { model_width });
            self.mode_switch(ui, actions);
            if !narrow {
                self.style_switch(ui);
            }
            let severity = match self.state {
                Onboarding::Stopped { unsafe_backend: true, .. } => Severity::Error,
                Onboarding::RateLimited { .. } | Onboarding::ToolsUnavailable { .. } => Severity::Warning,
                _ if self.status() == "Paused" => Severity::Warning,
                _ => Severity::Info,
            };
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = dp(3.0);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(dp(5.0), dp(18.0)), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), dp(2.0), severity.color());
                ui.label(RichText::new(self.status()).font(theme::ui_font(10.5)).color(readable(theme::muted())));
            });
            self.overflow(ui, actions);
        });
        ui.horizontal_wrapped(|ui| {
            let pin_budget = (ui.available_width() - if narrow { style_width + dp(8.0) } else { 0.0 }).max(dp(40.0));
            if icon_control(
                ui,
                pin_label,
                compact_width(ui, pin_label).min(pin_budget) + dp(16.0),
                Icon::Pin,
                false,
                true,
            )
            .on_hover_text("Pin the active chart. Prompts and chart tools use this chart until you pin another.")
            .clicked()
            {
                actions.push(Action::Pin);
            }
            if narrow {
                self.style_switch(ui);
            }
        });
        if self.data_basis.sample || self.data_basis.last_bar.is_some() {
            ui.horizontal_wrapped(|ui| {
                if self.data_basis.sample {
                    egui::Frame::new()
                        .stroke(egui::Stroke::new(dp(1.0), theme::border()))
                        .corner_radius(theme::CONTROL_RADIUS)
                        .inner_margin(egui::Margin::symmetric(dp(6.0) as i8, dp(2.0) as i8))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("Sample data").font(theme::ui_font(10.5)).color(readable(theme::muted())),
                            );
                        });
                }
                if let Some(freshness) = self.data_basis.freshness() {
                    ui.label(RichText::new(freshness).font(theme::ui_font(10.5)).color(readable(theme::muted())));
                }
                if let Some(word) = &self.data_basis.incomplete {
                    ui.label(
                        RichText::new(format!("{word} bar incomplete"))
                            .font(theme::ui_font(10.5))
                            .color(readable(theme::muted())),
                    );
                }
            });
        }
    }

    fn model_picker(&mut self, ui: &mut Ui, label: &str, width: f32) {
        let response = ui
            .add_enabled_ui(!self.models.is_empty(), |ui| {
                icon_control(ui, label, width, Icon::ChevronDown, true, false)
            })
            .inner
            .on_hover_text("Model and reasoning effort; costly efforts use more of your subscription and take longer.")
            .on_disabled_hover_text("Start the agent to load available models and reasoning efforts.");
        egui::Popup::menu(&response).show(|ui| {
            let old = self.settings.model.clone();
            for model in &self.models {
                ui.selectable_value(&mut self.settings.model, model.id.clone(), &model.display_name);
            }
            if old != self.settings.model {
                self.settings.effort.clear();
            }
            self.settings.select_defaults(&self.models);
            if let Some(model) = self.models.iter().find(|m| m.id == self.settings.model) {
                ui.separator();
                ui.menu_button("Reasoning effort", |ui| {
                    for effort in &model.supported_reasoning_efforts {
                        if effort.reasoning_effort != "ultra" {
                            ui.selectable_value(
                                &mut self.settings.effort,
                                effort.reasoning_effort.clone(),
                                effort_label(effort),
                            )
                            .on_hover_text(format!(
                                "{}{}",
                                effort.description,
                                if effort.costly || effort.reasoning_effort == "max" {
                                    " Higher subscription usage and latency."
                                } else {
                                    ""
                                }
                            ));
                        }
                    }
                });
            }
        });
    }

    fn mode_switch(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let available = crate::agent_bridge::agent_edit_tools_available();
        let group = egui::Frame::new()
            .fill(theme::control())
            .stroke(egui::Stroke::new(dp(1.0), theme::border()))
            .corner_radius(theme::CONTROL_RADIUS)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for (mode, label) in [(ToolsMode::ReadOnly, "Read only"), (ToolsMode::Edit, "Chart edits")] {
                        let selected = self.settings.tools == mode;
                        let response = ui
                            .add_enabled_ui(self.can_change_tools(mode, available), |ui| {
                                ui.spacing_mut().button_padding = egui::vec2(dp(6.0), dp(3.0));
                                ui.add(
                                    egui::Button::new(RichText::new(label).font(theme::ui_font(10.5)))
                                        .corner_radius(0)
                                        .selected(selected),
                                )
                            })
                            .inner;
                        focus_ring(ui, &response);
                        let help = if mode == ToolsMode::Edit {
                            CHART_EDITS_HELP
                        } else {
                            "The agent can read the pinned chart, but cannot change chart objects."
                        };
                        let response = response.on_hover_text(help).on_disabled_hover_text(format!(
                            "{help}\n{}",
                            if mode == ToolsMode::Edit && !available {
                                "Chart-edit tools aren't available in this build."
                            } else {
                                "Wait for the current turn or account check. Unsafe sessions must be closed."
                            }
                        ));
                        if response.clicked() && !selected {
                            actions.push(Action::SetTools(mode));
                        }
                    }
                });
            });
        ui.painter().rect_stroke(
            group.response.rect,
            theme::CONTROL_RADIUS,
            egui::Stroke::new(dp(1.0), theme::border()),
            egui::StrokeKind::Inside,
        );
    }

    fn style_switch(&mut self, ui: &mut Ui) {
        let label = self.settings.response_style.label();
        let response =
            icon_control(ui, label, compact_width(ui, label) + dp(16.0), Icon::ChevronDown, true, false).on_hover_text(
                format!("{} Applies to the next prompt; no restart.", self.settings.response_style.instruction()),
            );
        egui::Popup::menu(&response).show(|ui| {
            for style in [ResponseStyle::Terse, ResponseStyle::Analyst, ResponseStyle::Detailed] {
                if ui
                    .selectable_value(&mut self.settings.response_style, style, style.label())
                    .on_hover_text(style.instruction())
                    .clicked()
                {
                    ui.close();
                }
            }
        });
    }

    fn overflow(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(dp(22.0), dp(20.0)), egui::Sense::click());
        if response.hovered() {
            ui.painter().rect_filled(rect, theme::CONTROL_RADIUS, theme::control_hover());
        }
        widgets::draw_icon(ui.painter(), rect.shrink(dp(4.0)), Icon::More, theme::text());
        focus_ring(ui, &response);
        let response = response.on_hover_text("Agent menu");
        egui::Popup::menu(&response).show(|ui| {
            ui.menu_button("Backend", |ui| {
                ui.selectable_label(true, "Codex").on_hover_text("Runs locally in a sandbox");
            });
            if control(ui, "Privacy info").clicked() {
                self.disclosure = !self.disclosure;
                ui.close();
            }
            if undo_button(ui, "Undo last agent turn", self.header_undo_available && !self.running, self.running)
                .clicked()
                && let Some(id) = &self.header_undo
            {
                actions.push(Action::UndoTurn(id.clone()));
                ui.close();
            }
            let restart = !self.running
                && !matches!(
                    self.state,
                    Onboarding::Checking | Onboarding::SigningIn | Onboarding::Stopped { unsafe_backend: true, .. }
                );
            if ui
                .add_enabled(restart, primary_button("Restart agent"))
                .on_disabled_hover_text("Finish the current operation, or close an unsafe session.")
                .clicked()
            {
                actions.push(Action::Check);
                ui.close();
            }
            if control(ui, "Close").clicked() {
                actions.push(Action::Close);
                ui.close();
            }
        });
    }
}

fn prompt_header(ui: &mut Ui, time: &str) {
    // Explicit tracks keep the author at the left even when later markdown widens
    // a scroll area. Neither label can wrap or displace the other in narrow panes.
    let ink = readable(theme::muted());
    let author = ui.painter().layout_no_wrap("You".into(), theme::ui_font(10.5), ink);
    let time = ui.painter().layout_no_wrap(time.into(), theme::body_font(10.5), ink);
    let height = author.size().y.max(time.size().y);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
    ui.painter().galley(rect.min, author.clone(), ink);
    ui.painter().galley(rect.min + egui::vec2(dp(0.25), 0.0), author.clone(), ink);
    let time_x = (rect.right() - time.size().x).max(rect.left() + author.size().x + dp(8.0));
    ui.painter().galley(egui::pos2(time_x, rect.top()), time, ink);
}

fn header_rows(available: f32, required: f32) -> usize {
    if required <= available { 1 } else { 2 }
}

fn compact_width(ui: &Ui, label: &str) -> f32 {
    ui.painter().layout_no_wrap(label.into(), theme::ui_font(10.5), theme::text()).size().x + dp(12.0)
}
fn icon_control(ui: &mut Ui, label: &str, width: f32, icon: Icon, trailing: bool, outline: bool) -> egui::Response {
    let (rect, response) = ui
        .allocate_exact_size(egui::vec2(width.min(ui.available_width()).max(dp(24.0)), dp(22.0)), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, theme::CONTROL_RADIUS, theme::control_hover());
    }
    if outline {
        ui.painter().rect_stroke(
            rect,
            theme::CONTROL_RADIUS,
            egui::Stroke::new(dp(1.0), theme::border()),
            egui::StrokeKind::Inside,
        );
    }
    let icon_x = if trailing { rect.right() - dp(12.0) } else { rect.left() + dp(12.0) };
    let icon_rect = egui::Rect::from_center_size(egui::pos2(icon_x, rect.center().y), egui::vec2(dp(12.0), dp(12.0)));
    widgets::draw_icon(ui.painter(), icon_rect, icon, readable(theme::muted()));
    let mut job =
        agent_markdown::wrapped_job(label, (rect.width() - dp(28.0)).max(dp(1.0)), theme::ui_font(10.5), theme::text());
    job.wrap.max_rows = 1;
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let left = rect.left() + if trailing { dp(6.0) } else { dp(22.0) };
    ui.painter().galley(egui::pos2(left, rect.center().y - galley.size().y / 2.0), galley, theme::text());
    focus_ring(ui, &response);
    response
}
fn primary_button(label: &str) -> egui::Button<'_> {
    let ink = if contrast(theme::text(), theme::accent()) >= contrast(theme::card(), theme::accent()) {
        theme::text()
    } else {
        theme::card()
    };
    egui::Button::new(RichText::new(label).color(ink)).fill(theme::accent()).wrap()
}
fn control(ui: &mut Ui, label: &str) -> egui::Response {
    let response = ui.add(egui::Button::new(label).wrap());
    focus_ring(ui, &response);
    response
}
fn focus_ring(ui: &Ui, response: &egui::Response) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect,
            theme::CONTROL_RADIUS,
            egui::Stroke::new(1.0, theme::accent()),
            egui::StrokeKind::Inside,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Info,
    Warning,
    Error,
}
impl Severity {
    fn color(self) -> egui::Color32 {
        readable(match self {
            Self::Info => theme::muted(),
            Self::Warning => theme::amber_dim(),
            Self::Error => theme::down(),
        })
    }
    fn icon(self) -> Icon {
        match self {
            Self::Info => Icon::Info,
            Self::Warning => Icon::Warning,
            Self::Error => Icon::Close,
        }
    }
}
fn notice_severity(text: &str) -> Severity {
    let text = text.to_ascii_lowercase();
    if ["unsafe", "safety", "failed", "error", "could not", "unavailable", "no longer available", "ended"]
        .iter()
        .any(|s| text.contains(s))
    {
        Severity::Error
    } else if ["paused", "provisional", "interrupted", "history lost", "kept because"].iter().any(|s| text.contains(s))
    {
        Severity::Warning
    } else {
        Severity::Info
    }
}
fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
    let luminance = |c: egui::Color32| {
        let linear = |n: u8| {
            let v = f32::from(n) / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
    };
    let a = luminance(a);
    let b = luminance(b);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
pub(crate) fn readable(color: egui::Color32) -> egui::Color32 {
    // Theme-derived ink, not a bright ticker warning or an unreadable dim tint.
    (0..=20)
        .map(|i| widgets::lerp_color(color, theme::text(), i as f32 / 20.0))
        .find(|c| contrast(*c, theme::card()) >= 4.5)
        .unwrap_or(theme::text())
}
fn notice_row(ui: &mut Ui, text: &str, severity: Severity) {
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(dp(14.0), dp(18.0)), egui::Sense::hover());
        widgets::draw_icon(ui.painter(), rect, severity.icon(), severity.color());
        agent_markdown::wrapped(ui, text, severity.color());
    });
}
fn tool_label(name: &str) -> &str {
    match name {
        "read_chart" => "Read chart",
        "read_data" => "Read bars",
        "analyze" => "Analyze",
        "indicator_values" => "Indicator values",
        "find_symbol" => "Find symbol",
        "read_user_actions" => "Recent actions",
        "screenshot" => "Screenshot",
        "create_drawing" => "Draw",
        "update_drawing" => "Edit drawing",
        "delete_drawing" => "Delete drawing",
        "add_indicator" => "Add indicator",
        "remove_indicator" => "Remove indicator",
        "set_view" => "Change view",
        "compare" => "Compare",
        "list_watchlists" => "List watchlists",
        "list_alerts" => "List alerts",
        "create_watchlist" => "Propose watchlist",
        "add_watchlist_symbols" => "Propose adding symbols",
        "remove_watchlist_symbols" => "Propose removing symbols",
        "delete_watchlist" => "Propose deleting watchlist",
        "create_alert" => "Propose alert",
        "update_alert" => "Propose alert update",
        "delete_alert" => "Propose deleting alert",
        _ => "Chart tool",
    }
}

fn suggested_prompts(ui: &mut Ui, mode: ToolsMode, actions: &mut Vec<Action>) {
    for (heading, prompts, edits) in [
        ("Analyze", &["What's the trend?", "Find support and resistance", "Summarise the last 20 bars"][..], false),
        ("Find a setup", &["Where could a breakout confirm?", "What would invalidate a pullback setup?"][..], false),
        ("Add to chart", &["Mark support and resistance zones", "Add RSI 14"][..], true),
    ] {
        ui.add_space(dp(6.0));
        ui.label(RichText::new(heading).font(theme::ui_font(10.5)).color(readable(theme::muted())));
        for prompt in prompts {
            let enabled = !edits || mode == ToolsMode::Edit;
            let response = if enabled {
                control(ui, prompt)
            } else {
                ui.add_enabled(false, egui::Button::new(*prompt).wrap()).on_disabled_hover_text("Switch to Chart edits")
            };
            if response.clicked() {
                actions.push(Action::Send(Prompt { text: (*prompt).into(), requires_tools: true }));
            }
        }
    }
}

fn tool_group_duration(label: String, turn: Option<&ConversationTurn>) -> String {
    let Some(duration) = turn.filter(|turn| turn.finished).and_then(|turn| turn.duration) else { return label };
    let seconds = if duration < Duration::from_secs(1) {
        "<1s".into()
    } else {
        format!("{:.0}s", duration.as_secs_f64().round())
    };
    format!("{label} · {seconds}")
}

fn normalized_args(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(fields.iter().map(|(k, v)| (k.clone(), normalized_args(v))).collect()),
        Value::Array(values) => Value::Array(values.iter().map(normalized_args).collect()),
        Value::String(text) => Value::String(text.trim().into()),
        _ => value.clone(),
    }
}

fn workflow_tool_target(name: &str, args: &Value, proposals: &[crate::agent_bridge::Proposal]) -> Option<String> {
    let text = match name {
        "list_watchlists" => "All watchlists".into(),
        "list_alerts" => "All alerts".into(),
        "create_watchlist" => args["name"].as_str().unwrap_or("Watchlist").trim().to_owned(),
        "add_watchlist_symbols" | "remove_watchlist_symbols" | "delete_watchlist" => {
            args["watchlist"].as_str().unwrap_or("Watchlist").trim().to_owned()
        }
        "create_alert" | "update_alert" | "delete_alert" => {
            let before = proposals.iter().rev().find_map(|p| {
                use tr_agent_tools::args::LiveArgs;
                let id = match &p.args {
                    LiveArgs::UpdateAlert(a) => a.id,
                    LiveArgs::DeleteAlert(a) => a.id,
                    _ => return None,
                };
                (args["id"].as_u64() == Some(id)).then_some(&p.before)
            });
            let key = args["symbol"].as_str().or_else(|| before.and_then(|b| b["symbol"].as_str()));
            let symbol = key
                .map(crate::security_names::label)
                .unwrap_or_else(|| args["id"].as_u64().map_or_else(|| "Alert".into(), |id| format!("Alert #{id}")));
            if let Some(level) = args["level"].as_f64().or_else(|| before.and_then(|b| b["level"].as_f64())) {
                let level = crate::price_alerts::display_price(key.unwrap_or(""), level);
                if symbol.chars().count() + level.chars().count() < 45 {
                    format!("{symbol} · {level}")
                } else {
                    symbol
                }
            } else {
                symbol
            }
        }
        _ => return None,
    };
    Some(short_label(&text))
}

fn same_tool_target(failed: &Trace, later: &Trace) -> bool {
    if failed.name != later.name {
        return false;
    }
    let (a, b) = (&failed.target_args, &later.target_args);
    if !a.is_object() || !b.is_object() {
        return false;
    }
    match failed.name.as_str() {
        "create_watchlist" | "add_watchlist_symbols" | "remove_watchlist_symbols" | "delete_watchlist" => {
            let key = if failed.name == "create_watchlist" { "name" } else { "watchlist" };
            return a[key].as_str().zip(b[key].as_str()).is_some_and(|(a, b)| {
                a.trim().to_uppercase().to_lowercase() == b.trim().to_uppercase().to_lowercase()
            });
        }
        "update_alert" | "delete_alert" => return !a["id"].is_null() && a["id"] == b["id"],
        "create_alert" => {
            return a["symbol"]
                .as_str()
                .zip(b["symbol"].as_str())
                .is_some_and(|(a, b)| crate::symbol_search::same_key(a, b))
                && a["level"].as_f64().is_some()
                && a["level"].as_f64() == b["level"].as_f64()
                && a["direction"] == b["direction"];
        }
        _ => {}
    }
    for scope in ["symbol", "chart_id"] {
        if !a[scope].is_null() && !b[scope].is_null() && a[scope] != b[scope] {
            return false;
        }
    }
    match failed.name.as_str() {
        "create_drawing" => {
            // An explicit label can identify the intended drawing even when its geometry changes.
            if a["label"].as_str().is_some_and(|label| !label.is_empty() && a["label"] == b["label"]) {
                return true;
            }
            if a["kind"].is_null()
                || a["kind"] != b["kind"]
                || a["zone_role"] != b["zone_role"]
                || a["side"] != b["side"]
            {
                return false;
            }
            let range = |args: &Value| -> Option<(f64, f64)> {
                let prices: Option<Vec<_>> = args["anchors"].as_array()?.iter().map(|p| p["price"].as_f64()).collect();
                let prices = prices?;
                if prices.is_empty() || prices.iter().any(|p| !p.is_finite()) {
                    return None;
                }
                Some((
                    prices.iter().copied().fold(f64::INFINITY, f64::min),
                    prices.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                ))
            };
            range(a).zip(range(b)).is_some_and(|((al, ah), (bl, bh))| al <= bh && bl <= ah)
        }
        "update_drawing" | "delete_drawing" => !a["id"].is_null() && a["id"] == b["id"],
        _ => a == b,
    }
}

fn recovered_failures(traces: &[&Trace]) -> usize {
    traces
        .iter()
        .enumerate()
        .filter(|(i, trace)| {
            trace.success == Some(false)
                && traces[i + 1..].iter().any(|later| later.success == Some(true) && same_tool_target(trace, later))
        })
        .count()
}

fn retry_suffix(label: String, retries: usize) -> String {
    if retries == 0 { label } else { format!("{label} · {retries} retr{}", if retries == 1 { "y" } else { "ies" }) }
}

fn tool_group_label(traces: &[&Trace], reverted: Option<usize>) -> String {
    tool_group_label_outcome(traces, reverted, false)
}

fn tool_group_label_outcome(traces: &[&Trace], reverted: Option<usize>, final_answer: bool) -> String {
    let summary = tool_group_summary_outcome(traces, reverted, final_answer);
    if reverted.is_some() || traces.iter().any(|trace| trace.success.is_none() && !trace.interrupted) {
        summary
    } else {
        tool_step_suffix(summary, traces)
    }
}

fn tool_group_summary_outcome(traces: &[&Trace], reverted: Option<usize>, final_answer: bool) -> String {
    if let Some(n) = reverted {
        return format!("{n} change{} reverted", if n == 1 { "" } else { "s" });
    }
    if let Some(trace) = traces.iter().rev().find(|trace| trace.success.is_none() && !trace.interrupted) {
        return match trace.name.as_str() {
            "read_chart" => "Reading chart…".into(),
            "read_data" => "Reading bars…".into(),
            "analyze" => "Analyzing…".into(),
            "create_drawing" => "Drawing…".into(),
            _ => format!("{}…", tool_label(&trace.name)),
        };
    }
    if traces.iter().any(|trace| trace.interrupted) {
        "Stopped before finishing".into()
    } else if traces.iter().any(|trace| trace.success == Some(false))
        && !traces.iter().any(|trace| trace.success == Some(true))
    {
        "Chart tools failed".into()
    } else {
        let edits: Vec<_> =
            traces.iter().filter(|trace| is_edit_tool(&trace.name) && trace.success == Some(true)).collect();
        let workflow_changes = traces
            .iter()
            .filter(|trace| {
                trace.success == Some(true)
                    && matches!(
                        trace.name.as_str(),
                        "create_watchlist"
                            | "add_watchlist_symbols"
                            | "remove_watchlist_symbols"
                            | "delete_watchlist"
                            | "create_alert"
                            | "update_alert"
                            | "delete_alert"
                    )
            })
            .count();
        if workflow_changes > 0 {
            format!("Awaiting approval · {workflow_changes} change{}", if workflow_changes == 1 { "" } else { "s" })
        } else if edits.is_empty() {
            if final_answer && !traces.iter().any(|trace| trace.success == Some(true)) {
                "Answered".into()
            } else if traces.iter().any(|trace| {
                trace.success == Some(true) && matches!(trace.name.as_str(), "list_watchlists" | "list_alerts")
            }) {
                "Read watchlists and alerts".into()
            } else if traces.iter().any(|trace| trace.success == Some(true) && trace.name == "analyze") {
                "Analyzed chart".into()
            } else {
                "Read chart data".into()
            }
        } else {
            let lines = edits
                .iter()
                .filter(|trace| {
                    trace.name == "create_drawing"
                        && trace.summary.as_deref().is_some_and(|s| s.to_ascii_lowercase().contains("line"))
                })
                .count();
            let zones = edits
                .iter()
                .filter(|trace| {
                    trace.name == "create_drawing"
                        && trace.target_args["kind"] == "rectangle"
                        && !trace.target_args["zone_role"].is_null()
                })
                .count();
            let rectangles = edits
                .iter()
                .filter(|trace| {
                    trace.name == "create_drawing"
                        && trace.target_args["kind"] == "rectangle"
                        && trace.target_args["zone_role"].is_null()
                })
                .count();
            let indicators: Vec<_> = edits.iter().filter(|trace| trace.name == "add_indicator").collect();
            if lines + zones + rectangles + indicators.len() == edits.len() && indicators.len() <= 1 {
                let mut parts = Vec::new();
                if zones > 0 {
                    parts.push(format!("Drew {zones} zone{}", if zones == 1 { "" } else { "s" }));
                }
                if rectangles > 0 {
                    parts.push(format!(
                        "{}{rectangles} rectangle{}",
                        if parts.is_empty() { "Drew " } else { "" },
                        if rectangles == 1 { "" } else { "s" }
                    ));
                }
                if lines > 0 {
                    parts.push(format!(
                        "{}{lines} line{}",
                        if parts.is_empty() { "Drew " } else { "" },
                        if lines == 1 { "" } else { "s" }
                    ));
                }
                if let Some(trace) = indicators.first() {
                    let object = main_tool_label(trace.summary.as_deref().unwrap_or("indicator"));
                    parts.push(format!("{} {object}", if parts.is_empty() { "Added" } else { "added" }));
                }
                parts.join(" and ")
            } else {
                "Chart edit tools finished".into()
            }
        }
    }
}

fn tool_step_suffix(summary: String, traces: &[&Trace]) -> String {
    let label = format!("{summary} · {} step{}", traces.len(), if traces.len() == 1 { "" } else { "s" });
    let failures = traces.iter().filter(|trace| trace.success == Some(false)).count() - recovered_failures(traces);
    if failures > 0 { format!("{label} · {failures} failed") } else { label }
}

fn tool_status(trace: &Trace) -> &'static str {
    if trace.interrupted {
        "interrupted"
    } else {
        match trace.success {
            Some(true) => "ok",
            Some(false) => "failed",
            None => "running",
        }
    }
}

#[cfg(test)]
fn tool_row(ui: &mut Ui, trace: &Trace) {
    tool_row_with_history(ui, trace, false);
}

fn tool_row_with_history(ui: &mut Ui, trace: &Trace, reverted: bool) {
    let status = tool_status(trace);
    let duration = trace.duration_ms.map(|ms| format!("{ms} ms")).unwrap_or_else(|| "—".into());
    let duration = ui.painter().layout_no_wrap(duration, theme::mono(10.5), readable(theme::muted()));
    let object = main_tool_label(trace.summary.as_deref().unwrap_or(""));
    let label = if object.is_empty() {
        tool_label(&trace.name).to_owned()
    } else {
        format!("{} · {object}", tool_label(&trace.name))
    };
    let width = ui.available_width();
    let mut job = agent_markdown::wrapped_job(
        &label,
        (width - duration.size().x - dp(32.0)).max(dp(1.0)),
        theme::ui_font(12.0),
        theme::text(),
    );
    job.wrap.max_rows = 1;
    let label = ui.fonts_mut(|f| f.layout_job(job));
    let height = dp(20.0).max(label.size().y).max(duration.size().y);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let icon = egui::Rect::from_center_size(
        egui::pos2(rect.left() + dp(7.0), rect.center().y),
        egui::vec2(dp(12.0), dp(12.0)),
    );
    match status {
        "ok" => {
            let stroke = egui::Stroke::new(dp(1.0), readable(if reverted { theme::muted() } else { theme::up() }));
            ui.painter()
                .add(egui::Shape::line(vec![icon.left_center(), icon.center_bottom(), icon.right_top()], stroke));
        }
        "failed" => widgets::draw_icon(ui.painter(), icon, Icon::Close, Severity::Error.color()),
        "interrupted" => widgets::draw_icon(ui.painter(), icon, Icon::Pause, Severity::Warning.color()),
        _ => {
            // A static clock, not a spinner: tool status never adds idle repaints.
            let stroke = egui::Stroke::new(dp(1.0), readable(theme::muted()));
            ui.painter().circle_stroke(icon.center(), dp(5.0), stroke);
            ui.painter().line_segment([icon.center_top(), icon.center()], stroke);
            ui.painter().line_segment([icon.center(), icon.right_center()], stroke);
        }
    }
    ui.painter().galley(
        egui::pos2(rect.left() + dp(20.0), rect.center().y - label.size().y / 2.0),
        label,
        theme::text(),
    );
    ui.painter().galley(
        egui::pos2(rect.right() - duration.size().x, rect.center().y - duration.size().y / 2.0),
        duration,
        theme::text(),
    );
    if trace.success == Some(false)
        && let Some(error) = &trace.error
    {
        ui.indent(("tool-error", &trace.id), |ui| agent_markdown::wrapped(ui, error, readable(theme::muted())));
    }
    response.on_hover_text(format!(
        "{} · {status}\n{}\nCall: {}\n{}",
        trace.name,
        trace.summary.as_deref().unwrap_or(""),
        trace.id,
        trace.arguments,
    ));
}

pub(crate) fn tool_error(item: &Value) -> Option<String> {
    if item["success"] != false {
        return None;
    }
    let mut messages = Vec::new();
    if let Some(message) = item["error"]["message"].as_str().or_else(|| item["error"].as_str()) {
        messages.push(short_label(message));
    }
    for content in item["contentItems"].as_array().into_iter().flatten() {
        if let Some(text) = content["text"].as_str() {
            let value = serde_json::from_str::<Value>(text).ok();
            let message = value.as_ref().and_then(|v| v["error"]["message"].as_str());
            if let Some(message) = message {
                messages.push(short_label(message));
            } else if value.is_none() {
                messages.push(short_label(text));
            }
        }
    }
    messages.dedup();
    (!messages.is_empty()).then(|| messages.join(" · "))
}

fn main_tool_label(summary: &str) -> String {
    let label =
        if summary.starts_with('#') { summary.split_once(" · ").map_or("", |(_, label)| label) } else { summary };
    let mut chars = label.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().collect::<String>() + chars.as_str())
}

pub(crate) fn price_label(price: f64) -> String {
    let text = format!("{price:.2}");
    let (whole, fraction) = text.split_once('.').unwrap_or((&text, "00"));
    let mut result = String::new();
    for (i, ch) in whole.chars().enumerate() {
        if i > 0
            && (whole.len() - i).is_multiple_of(3)
            && ch.is_ascii_digit()
            && whole.as_bytes()[i - 1].is_ascii_digit()
        {
            result.push(',');
        }
        result.push(ch);
    }
    format!("{result}.{fraction}")
}

fn undo_button(ui: &mut Ui, label: &str, enabled: bool, running: bool) -> egui::Response {
    let response = if label.starts_with("Undo ") && label.contains("chart change") {
        ui.scope(|ui| {
            ui.spacing_mut().button_padding = egui::vec2(dp(6.0), dp(3.0));
            ui.add_enabled(enabled, egui::Button::new(RichText::new(label).font(theme::ui_font(10.5))).frame(false))
        })
        .inner
    } else {
        ui.add_enabled(enabled, egui::Button::new(label))
    };
    focus_ring(ui, &response);
    response
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
                .map_or_else(|| "Horizontal line".into(), |price| format!("Horizontal line at {}", price_label(price))),
            "trend_line" => "Trend line".into(),
            "vertical" => "vertical line".into(),
            "ray" => "ray (2 anchors)".into(),
            "rectangle" if args["zone_role"].as_str().is_some() => {
                format!("{} zone", args["zone_role"].as_str().unwrap())
            }
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
mod polish_tests {
    use super::*;

    fn texts(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
        fn collect(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Text(t) => out.push((t.galley.text().into(), t.galley.rect.translate(t.pos.to_vec2()))),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, out)),
                _ => {}
            }
        }
        let mut result = Vec::new();
        for shape in &output.shapes {
            collect(&shape.shape, &mut result);
        }
        result
    }

    fn render(
        ctx: &egui::Context,
        width: f32,
        events: Vec<egui::Event>,
        mut draw: impl FnMut(&mut Ui),
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 900.0))),
                events,
                ..Default::default()
            },
            |ui| draw(ui),
        );
        output.textures_delta.clear();
        output
    }

    fn pointer(pos: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn immediate_tooltips(ctx: &egui::Context) {
        ctx.all_styles_mut(|style| {
            style.interaction.tooltip_delay = 0.0;
            style.interaction.tooltip_grace_time = 0.0;
        });
    }

    fn workflow_trace(name: &str, args: Value, success: bool) -> Trace {
        let mut pane = AgentPane::default();
        pane.wire("item/completed", &serde_json::json!({"item":{"id":"workflow","type":"dynamicToolCall","tool":name,"arguments":args,"success":success}}));
        let RenderedBlock::Tool(trace) = pane.blocks.remove(0) else { panic!("trace") };
        trace
    }

    #[test]
    fn workflow_retry_identity_recovers_the_reported_create_add_find_add_sequence() {
        use serde_json::json;
        let calls = [
            workflow_trace("create_watchlist", json!({"name":"Agent Test"}), true),
            workflow_trace(
                "add_watchlist_symbols",
                json!({"watchlist":"Agent Test","symbols":["AAPL","MSFT","BTC-USD"]}),
                false,
            ),
            workflow_trace("find_symbol", json!({"query":"BTC-USD"}), true),
            workflow_trace(
                "add_watchlist_symbols",
                json!({"watchlist":"aGENT tEST","symbols":["AAPL US Equity","MSFT US Equity","COINBASE:BTC-USD"]}),
                true,
            ),
        ];
        let traces: Vec<_> = calls.iter().collect();
        assert_eq!(recovered_failures(&traces), 1);
        let label = retry_suffix(tool_group_label(&traces, None), recovered_failures(&traces));
        assert!(label.ends_with(" · 4 steps · 1 retry"), "{label}");
        assert!(!label.contains("failed"));
        assert_eq!(calls[1].summary.as_deref(), Some("Agent Test"));
        assert_eq!(calls[1].success, Some(false), "failed detail row is retained");
        assert_eq!(recovered_failures(&[&calls[3], &calls[1]]), 0, "only later success recovers");
        let other = workflow_trace("add_watchlist_symbols", json!({"watchlist":"Other","symbols":["AAPL"]}), true);
        assert_eq!(recovered_failures(&[&calls[1], &other]), 0);
        let remove =
            workflow_trace("remove_watchlist_symbols", json!({"watchlist":"Agent Test","symbols":["AAPL"]}), true);
        assert_eq!(recovered_failures(&[&calls[1], &remove]), 0, "same target but different tool is not a retry");
    }

    #[test]
    fn workflow_retry_identity_matches_each_workflow_target_not_changed_payloads() {
        use serde_json::json;
        for tool in ["create_watchlist", "add_watchlist_symbols", "remove_watchlist_symbols", "delete_watchlist"] {
            let key = if tool == "create_watchlist" { "name" } else { "watchlist" };
            let a = workflow_trace(tool, json!({key:" Agent Test ","symbols":["BTC-USD"]}), false);
            let b = workflow_trace(tool, json!({key:"agent test","symbols":["COINBASE:BTC-USD"]}), true);
            assert!(same_tool_target(&a, &b), "{tool}");
        }
        for tool in ["update_alert", "delete_alert"] {
            let a = workflow_trace(tool, json!({"id":7,"level":10}), false);
            let b = workflow_trace(tool, json!({"id":7,"level":20}), true);
            let other = workflow_trace(tool, json!({"id":8,"level":20}), true);
            assert!(same_tool_target(&a, &b));
            assert!(!same_tool_target(&a, &other));
        }
    }

    #[test]
    fn create_alert_retry_requires_same_symbol_level_and_direction() {
        use serde_json::json;
        let a =
            workflow_trace("create_alert", json!({"symbol":"BINANCE:XRPUSDT","level":10,"direction":"above"}), false);
        let canonical = tr_data::security("BINANCE:XRPUSDT").key();
        let b = workflow_trace("create_alert", json!({"symbol":canonical,"level":10.0,"direction":"above"}), true);
        assert!(same_tool_target(&a, &b));
        assert_eq!(recovered_failures(&[&a, &b]), 1);
        for args in [
            json!({"symbol":canonical,"level":20,"direction":"above"}),
            json!({"symbol":canonical,"level":10,"direction":"below"}),
            json!({"symbol":"AAPL US Equity","level":10,"direction":"above"}),
        ] {
            let other = workflow_trace("create_alert", args, true);
            assert!(!same_tool_target(&a, &other));
            assert_eq!(recovered_failures(&[&a, &other]), 0);
            assert!(tool_group_label(&[&a, &other], None).ends_with(" · 1 failed"));
        }
    }

    #[test]
    fn review_workflow_tool_target_is_scrubbed_and_capped_like_other_tool_summaries() {
        use serde_json::json;
        let name = format!("me@example.com\nsecond line {}", "x".repeat(400));
        for (tool, key) in [("create_watchlist", "name"), ("add_watchlist_symbols", "watchlist")] {
            let trace = workflow_trace(tool, json!({key: name}), false);
            let summary = trace.summary.unwrap();
            assert!(!summary.contains("me@example.com"), "{tool}: model-supplied text must be scrubbed");
            assert!(!summary.contains('\n'), "{tool}: control characters must be stripped");
            assert!(summary.chars().count() <= 180, "{tool}: summary must be capped");
        }
    }

    #[test]
    fn workflow_tool_rows_show_targets_and_leave_chart_tool_labels_unchanged() {
        use serde_json::json;
        for (tool, args, expected) in [
            ("create_watchlist", json!({"name":"Agent Test"}), "Agent Test"),
            ("add_watchlist_symbols", json!({"watchlist":"Agent Test"}), "Agent Test"),
            ("remove_watchlist_symbols", json!({"watchlist":"Agent Test"}), "Agent Test"),
            ("delete_watchlist", json!({"watchlist":"Agent Test"}), "Agent Test"),
            ("list_watchlists", json!({}), "All watchlists"),
            ("list_alerts", json!({"symbol":"AAPL US Equity"}), "All alerts"),
            ("create_alert", json!({"symbol":"COINBASE:BTC-USD"}), "BTC/USD · Coinbase"),
            ("read_chart", json!({}), "Pinned chart"),
        ] {
            let trace = workflow_trace(tool, args, true);
            assert_eq!(trace.summary.as_deref(), Some(expected), "{tool}");
            let ctx = egui::Context::default();
            let output = render(&ctx, dp(600.0), vec![], |ui| tool_row(ui, &trace));
            assert!(texts(&output).iter().any(|(s, _)| s == &format!("{} · {expected}", tool_label(tool))), "{tool}");
        }
        let mut p = card_pane().workflows.proposals.remove(0);
        p.before = json!({"symbol":"AAPL US Equity"});
        for tool in ["update_alert", "delete_alert"] {
            p.args = if tool == "update_alert" {
                tr_agent_tools::args::LiveArgs::UpdateAlert(
                    serde_json::from_value(json!({"id":7,"level":123})).unwrap(),
                )
            } else {
                tr_agent_tools::args::LiveArgs::DeleteAlert(serde_json::from_value(json!({"id":7})).unwrap())
            };
            assert_eq!(workflow_tool_target(tool, &json!({"id":7}), &[p.clone()]).as_deref(), Some("AAPL US Equity"));
        }
        let label = workflow_tool_target("create_alert", &json!({"symbol":"AAPL US Equity","level":100}), &[]).unwrap();
        assert!(label.starts_with("AAPL US Equity · 100"), "{label}");
    }

    #[test]
    fn symbol_subrows_are_plain_text_focusable_space_only_and_all_unticked_disables_approve() {
        let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("proposal").unwrap(), 1);
        pane.workflows.proposals.remove(0);
        let ctx = egui::Context::default();
        let mut actions = Vec::new();
        for _ in 0..2 {
            render(&ctx, dp(600.0), vec![], |ui| {
                actions = pane.show(ui, "Pinned", egui::Id::new("symbols"));
            });
        }
        assert!(actions.is_empty());
        let output = render(&ctx, dp(600.0), vec![], |ui| {
            actions = pane.show(ui, "Pinned", egui::Id::new("symbols"));
        });
        let text = texts(&output);
        assert!(text.iter().any(|(s, _)| s == "Add to Agent Test"));
        assert!(text.iter().any(|(s, _)| s == "AAPL US Equity"));
        assert!(text.iter().any(|(s, _)| s == "BTC/USD · Coinbase"));
        assert!(text.iter().any(|(s, _)| s == "Approve selected"));
        let boxes: Vec<_> =
            pane.proposal_focus.keys().filter(|id| !pane.proposal_buttons.contains_key(id)).copied().collect();
        assert_eq!(boxes.len(), 3);
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let before: Vec<_> = pane.workflows.proposals[0].symbols.iter().map(|s| s.selected).collect();
        ctx.memory_mut(|m| m.request_focus(boxes[0]));
        render(&ctx, dp(600.0), vec![key(egui::Key::Enter)], |ui| {
            actions = pane.show(ui, "Pinned", egui::Id::new("symbols"));
        });
        assert!(actions.is_empty(), "Enter on sub-row cannot approve");
        assert_eq!(
            pane.workflows.proposals[0].symbols.iter().map(|s| s.selected).collect::<Vec<_>>(),
            before,
            "Enter does not toggle"
        );
        ctx.memory_mut(|m| m.request_focus(boxes[0]));
        render(&ctx, dp(600.0), vec![key(egui::Key::Space)], |ui| {
            actions = pane.show(ui, "Pinned", egui::Id::new("symbols"));
        });
        assert_eq!(
            pane.workflows.proposals[0].symbols.iter().map(|s| s.selected).zip(before).filter(|(a, b)| a != b).count(),
            1
        );
        assert!(actions.is_empty());
        for s in &mut pane.workflows.proposals[0].symbols {
            s.selected = false;
        }
        render(&ctx, dp(600.0), vec![], |ui| {
            actions = pane.show(ui, "Pinned", egui::Id::new("symbols"));
        });
        assert!(!pane.proposal_buttons.values().any(|approve| *approve), "no enabled approval button");
    }

    #[test]
    fn symbol_subrow_guard_requires_previous_frame_even_for_new_unticked_rows() {
        use crate::agent_bridge::SymbolChoice;
        let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("proposal").unwrap(), 1);
        let turn = crate::agent_demo::TURN;
        let ctx = egui::Context::default();
        let mut actions = Vec::new();
        render(&ctx, dp(600.0), vec![], |ui| pane.confirmation_card(ui, turn, &mut actions));
        assert!(pane.proposal_shown[turn].is_empty());
        render(&ctx, dp(600.0), vec![], |ui| pane.confirmation_card(ui, turn, &mut actions));
        assert_eq!(pane.proposal_shown[turn], [1, 2]);
        pane.workflows.proposals[1].symbols.push(SymbolChoice {
            symbol: "NVDA US Equity".into(),
            selected: false,
            outcome: None,
        });
        render(&ctx, dp(600.0), vec![], |ui| pane.confirmation_card(ui, turn, &mut actions));
        assert_eq!(pane.proposal_shown[turn], [1], "new sub-row blocks approval AND implicit decline of its proposal");
        assert!(!pane.symbol_shown[turn].iter().any(|(_, _, s)| s == "NVDA US Equity"));
        render(&ctx, dp(600.0), vec![], |ui| pane.confirmation_card(ui, turn, &mut actions));
        assert_eq!(pane.proposal_shown[turn], [1, 2]);
        assert!(pane.symbol_shown[turn].iter().any(|(_, _, s)| s == "NVDA US Equity"));
    }

    #[test]
    fn workflow_outcome_header_truncates_with_tooltip_and_host_line_does_not_rewrite_answer() {
        check_workflow_outcome_header(false);
    }

    #[test]
    fn mixed_chart_and_workflow_header_keeps_both_summaries_and_full_tooltip() {
        check_workflow_outcome_header(true);
    }

    fn check_workflow_outcome_header(with_chart_edit: bool) {
        use crate::agent_bridge::ProposalStatus;
        let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("proposal").unwrap(), 1);
        let turn = crate::agent_demo::TURN;
        pane.turns[0].duration = Some(Duration::from_secs(57));
        for p in &mut pane.workflows.proposals {
            p.status = ProposalStatus::Approved;
            p.decided_at = Some(crate::agent_demo::SAMPLE_NOW);
            for s in &mut p.symbols {
                s.outcome = Some(if s.selected { "Added" } else { "Declined" }.into());
            }
        }
        if with_chart_edit {
            crate::agent_demo::tool(&mut pane, "chart-line", "create_drawing", Some(true), Some("Horizontal line"));
        }
        let summary = pane.workflows.turn_summary(turn).unwrap();
        let line = pane.workflows.decision_line(turn).unwrap();
        let ctx = egui::Context::default();
        immediate_tooltips(&ctx);
        let mut actions = Vec::new();
        let output = render(&ctx, dp(360.0), vec![], |ui| pane.conversation(ui, &mut actions));
        let text = texts(&output);
        let expected = if with_chart_edit {
            format!("Drew 1 line · {summary} · 3 steps · 57s")
        } else {
            format!("{summary} · 2 steps · 57s")
        };
        let (_, rect) = text.iter().find(|(s, _)| s == &expected).expect("outcome header");
        assert!(rect.height() < dp(24.0));
        assert_eq!(text.iter().filter(|(s, _)| s == &line).count(), 1, "one host decision line under the answer");
        assert!(text.iter().any(|(s, _)| s.contains("awaiting your confirmation")), "model text stays unchanged");
        let hover = rect.center();
        for _ in 0..2 {
            render(&ctx, dp(360.0), vec![egui::Event::PointerMoved(hover)], |ui| pane.conversation(ui, &mut actions));
        }
        let output =
            render(&ctx, dp(360.0), vec![egui::Event::PointerMoved(hover)], |ui| pane.conversation(ui, &mut actions));
        assert!(
            texts(&output).iter().filter(|(s, _)| s == &expected).count() >= 2,
            "full outcome text is also in tooltip"
        );
        assert!(actions.is_empty());
    }

    fn card_pane() -> AgentPane {
        use crate::agent_bridge::{Proposal, ProposalStatus};
        let mut pane = AgentPane::default();
        pane.workflows.proposals.push(Proposal {
            id: 1,
            turn: "card:1".into(),
            args: tr_agent_tools::args::LiveArgs::CreateAlert(tr_agent_tools::args::CreateAlert {
                symbol: "AAPL US Equity".into(),
                kind: tr_agent_tools::args::AlertKind::Price,
                direction: tr_agent_tools::args::AlertRule::Above,
                level: 100.,
                timeframe: None,
                repeat: None,
                note: Some(
                    "A long note about the alert\nwith its complete explanation visible only in the tooltip. "
                        .repeat(8),
                ),
            }),
            symbols: Vec::new(),
            decided_at: None,
            summary: "Create [Actual **Name**](https://example.invalid)".into(),
            user_owned: false,
            selected: true,
            deadline: Instant::now() + Duration::from_secs(600),
            status: ProposalStatus::Pending,
            before: Value::Null,
        });
        pane
    }

    #[test]
    fn confirmation_card_uses_plain_summary_and_muted_single_line_note_with_full_tooltip() {
        let mut pane = card_pane();
        let ctx = egui::Context::default();
        immediate_tooltips(&ctx);
        let mut actions = Vec::new();
        render(&ctx, dp(360.0), vec![], |ui| pane.confirmation_card(ui, "card:1", &mut actions));
        let output = render(&ctx, dp(360.0), vec![], |ui| pane.confirmation_card(ui, "card:1", &mut actions));
        let text = texts(&output);
        assert!(
            text.iter().any(|(text, _)| text == &pane.workflows.proposals[0].summary),
            "summary must not parse Markdown"
        );
        let (_, rect) = text.iter().find(|(text, _)| text.starts_with("A long note")).expect("note is on the card");
        assert!(rect.height() < dp(24.0), "note must occupy one line: {rect:?}");
        fn note_color(shape: &egui::Shape) -> Option<egui::Color32> {
            match shape {
                egui::Shape::Text(text) if text.galley.text().starts_with("A long note") => {
                    Some(text.galley.job.sections[0].format.color)
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(note_color),
                _ => None,
            }
        }
        assert_eq!(output.shapes.iter().find_map(|shape| note_color(&shape.shape)), Some(theme::muted()));
        let hover = rect.center();
        let note = match &pane.workflows.proposals[0].args {
            tr_agent_tools::args::LiveArgs::CreateAlert(args) => args.note.clone().unwrap(),
            _ => unreachable!(),
        };
        render(&ctx, dp(360.0), vec![egui::Event::PointerMoved(hover)], |ui| {
            pane.confirmation_card(ui, "card:1", &mut actions)
        });
        let output = render(&ctx, dp(360.0), vec![egui::Event::PointerMoved(hover)], |ui| {
            pane.confirmation_card(ui, "card:1", &mut actions)
        });
        assert!(texts(&output).iter().any(|(text, _)| text == &note), "tooltip keeps the full note");
        assert!(actions.is_empty());
    }

    #[test]
    fn confirmation_card_never_decides_a_proposal_that_arrived_as_the_user_clicked() {
        use crate::agent_bridge::ProposalStatus;
        let mut pane = card_pane();
        let ctx = egui::Context::default();
        let mut actions = Vec::new();
        render(&ctx, dp(600.0), vec![], |ui| pane.confirmation_card(ui, "card:1", &mut actions));
        render(&ctx, dp(600.0), vec![], |ui| pane.confirmation_card(ui, "card:1", &mut actions));
        assert_eq!(pane.proposal_shown["card:1"], vec![1]);
        // A second proposal lands in the same frame as the click: it is drawn, but the
        // user cannot have seen it, so it must not be part of the decision.
        let mut late = pane.workflows.proposals[0].clone();
        late.id = 2;
        late.status = ProposalStatus::Pending;
        pane.workflows.proposals.push(late);
        render(&ctx, dp(600.0), vec![], |ui| pane.confirmation_card(ui, "card:1", &mut actions));
        assert_eq!(pane.proposal_shown["card:1"], vec![1]);
        render(&ctx, dp(600.0), vec![], |ui| pane.confirmation_card(ui, "card:1", &mut actions));
        assert_eq!(pane.proposal_shown["card:1"], vec![1, 2]);
    }

    #[test]
    fn confirmation_card_enter_only_acts_on_focused_button_without_app_hotkeys() {
        for target in ["approve", "decline", "checkbox"] {
            let mut pane = card_pane();
            let mut second = pane.workflows.proposals[0].clone();
            second.id = 2;
            pane.workflows.proposals.push(second);
            let ctx = egui::Context::default();
            let mut actions = Vec::new();
            // Two frames: a proposal is decidable only once it has been on screen.
            render(&ctx, dp(600.0), vec![], |ui| pane.confirmation_card(ui, "card:1", &mut actions));
            render(&ctx, dp(600.0), vec![], |ui| pane.confirmation_card(ui, "card:1", &mut actions));
            let id = *pane
                .proposal_focus
                .keys()
                .find(|id| match target {
                    "approve" => pane.proposal_buttons.get(id) == Some(&true),
                    "decline" => pane.proposal_buttons.get(id) == Some(&false),
                    _ => !pane.proposal_buttons.contains_key(id),
                })
                .unwrap();
            ctx.memory_mut(|m| m.request_focus(id));
            let key = |key| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            render(&ctx, dp(600.0), vec![key(egui::Key::Enter)], |ui| {
                pane.confirmation_card(ui, "card:1", &mut actions)
            });
            if target == "checkbox" {
                assert!(actions.is_empty());
            } else {
                assert_eq!(actions.len(), 1, "{target}");
                assert!(matches!(&actions[0], Action::Confirm { approve, .. } if *approve == (target == "approve")));
            }
            actions.clear();
            ctx.memory_mut(|m| m.request_focus(id));
            render(&ctx, dp(600.0), vec![key(egui::Key::Escape)], |ui| {
                ctx.memory_mut(|m| m.request_focus(id));
                pane.confirmation_card(ui, "card:1", &mut actions)
            });
            assert_eq!(actions.len(), 1, "{target}: Escape emits one action");
            assert!(matches!(actions.as_slice(), [Action::Confirm { approve: false, .. }]), "{target}");
        }
    }

    #[test]
    fn unavailable_send_is_neutral_in_every_blocked_state_with_reason_and_no_hover_or_action() {
        for theme_name in ["Terminal Dark", "Terminal Light", "Bloomberg"] {
            for state in [
                "dormant",
                "missing",
                "unsupported",
                "signed-out",
                "signing-in",
                "checking",
                "rate-limited",
                "stopped-unsafe",
                "stopped-recoverable",
                "empty-ready",
                "tools-unavailable",
            ] {
                for input in ["", "   ", "Question"] {
                    let ctx = egui::Context::default();
                    let themes = crate::themes::builtins();
                    theme::configure(
                        &ctx,
                        themes.iter().find(|t| t.name == theme_name).unwrap(),
                        theme::DEFAULT_ACCENT,
                    );
                    immediate_tooltips(&ctx);
                    let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse(state).unwrap(), 1);
                    pane.input = input.into();
                    let blocked = pane.send_blocked_reason();
                    let mut rect = egui::Rect::NOTHING;
                    let mut clicked = false;
                    let mut enabled = false;
                    let output = render(&ctx, 360.0, vec![], |ui| {
                        let response = pane.composer_button(ui);
                        rect = response.rect;
                        enabled = response.enabled();
                    });
                    assert_eq!(enabled, blocked.is_none(), "{state} {input:?}");
                    assert_eq!(
                        output
                            .shapes
                            .iter()
                            .any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == theme::accent())),
                        blocked.is_none()
                    );
                    let Some(reason) = blocked else { continue };
                    assert!(output.shapes.iter().any(|s| matches!(&s.shape,
                        egui::Shape::Text(t) if t.galley.text() == "Send" && t.galley.job.sections[0].format.color == readable(theme::muted())
                    )), "disabled ink: {state}");
                    let fill = |output: &egui::FullOutput| {
                        output
                            .shapes
                            .iter()
                            .find_map(|s| match &s.shape {
                                egui::Shape::Rect(r) if r.rect == rect => Some(r.fill),
                                _ => None,
                            })
                            .unwrap()
                    };
                    let original_fill = fill(&output);
                    let mut tooltip_seen = false;
                    for _ in 0..4 {
                        let output = render(&ctx, 360.0, vec![egui::Event::PointerMoved(rect.center())], |ui| {
                            clicked |= pane.composer_button(ui).clicked();
                        });
                        assert_eq!(fill(&output), original_fill, "disabled hover {state}");
                        tooltip_seen |= texts(&output).iter().any(|(s, _)| s == reason);
                    }
                    assert!(tooltip_seen, "missing disabled tooltip: {state}");
                    for pressed in [true, false] {
                        render(&ctx, 360.0, pointer(rect.center(), pressed), |ui| {
                            clicked |= pane.composer_button(ui).clicked();
                        });
                    }
                    assert!(!clicked);
                }
            }
        }
        let mut full = crate::agent_demo::pane(crate::agent_demo::State::parse("empty-ready").unwrap(), 1);
        full.input = "Question".into();
        for _ in 0..32 {
            full.enqueue(Prompt { text: "queued".into(), requires_tools: false });
        }
        assert!(full.send_blocked_reason().unwrap().contains("queue is full"));
    }

    #[test]
    fn paused_composer_adds_to_queue_waits_for_resume_and_measures_the_longer_label() {
        for width in [360.0, 450.0, 660.0] {
            let ctx = egui::Context::default();
            immediate_tooltips(&ctx);
            let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("queue").unwrap(), 1);
            let original = pane.queue.clone();
            let mut actions = Vec::new();
            let mut draw = |pane: &mut AgentPane, events| {
                render(&ctx, width, events, |ui| {
                    actions = Vec::new();
                    pane.composer(ui, egui::Id::new("paused-composer"), dp(48.0), &mut actions);
                })
            };
            draw(&mut pane, vec![]);
            let output = draw(&mut pane, vec![]);
            let text = texts(&output);
            let button = text.iter().find(|(s, _)| s == "Add to queue").unwrap().1;
            assert!(button.right() <= width && !text.iter().any(|(s, _)| s == "Send"));
            let mut tooltip_seen = false;
            for _ in 0..4 {
                let output = draw(&mut pane, vec![egui::Event::PointerMoved(button.center())]);
                tooltip_seen |=
                    texts(&output).iter().any(|(s, _)| s == "Add this prompt to the queue. It will wait for Resume.");
            }
            assert!(tooltip_seen);
            for pressed in [true, false] {
                draw(&mut pane, pointer(button.center(), pressed));
            }
            let prompt =
                actions.into_iter().find_map(|a| if let Action::Send(p) = a { Some(p) } else { None }).unwrap();
            pane.enqueue(prompt);
            assert_eq!(pane.queue.len(), original.len() + 1);
            assert_eq!(pane.queue[0], original[0]);
            assert!(pane.next_prompt().is_none());
            pane.resume_queue();
            assert_eq!(pane.composer_label(), "Send");
            assert_eq!(pane.next_prompt(), Some(original[0].clone()));
        }
    }

    #[test]
    fn privacy_has_three_small_headings_body_ink_and_the_existing_continue_action() {
        for name in ["Terminal Dark", "Terminal Light", "Bloomberg"] {
            for width in [360.0, 450.0, 660.0] {
                let ctx = egui::Context::default();
                let themes = crate::themes::builtins();
                theme::configure(&ctx, themes.iter().find(|t| t.name == name).unwrap(), theme::DEFAULT_ACCENT);
                let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("privacy").unwrap(), 1);
                let draw = |pane: &mut AgentPane, events| {
                    render(&ctx, width, events, |ui| {
                        assert!(pane.show(ui, "BTC/USD · 1W", egui::Id::new("privacy-test")).is_empty());
                    })
                };
                draw(&mut pane, vec![]);
                let output = draw(&mut pane, vec![]);
                let text = texts(&output);
                let mut previous = 0.0;
                for (heading, body) in PRIVACY_GROUPS {
                    let heading_rect = text.iter().find(|(s, _)| s == heading).unwrap().1;
                    let body_rect = text.iter().find(|(s, _)| s == body).unwrap().1;
                    assert!(heading_rect.top() >= previous && body_rect.top() >= heading_rect.bottom());
                    assert!(body_rect.right() <= width);
                    previous = body_rect.bottom();
                    for shape in &output.shapes {
                        if let egui::Shape::Text(t) = &shape.shape {
                            if t.galley.text() == *body {
                                assert!(
                                    t.galley.job.sections.iter().all(|s| s.format.color == theme::text()
                                        && s.format.font_id == theme::body_font(13.0))
                                );
                            } else if t.galley.text() == *heading {
                                assert_eq!(t.galley.job.sections[0].format.font_id, theme::ui_font(10.5));
                            }
                        }
                    }
                }
                let pos = text.iter().find(|(s, _)| s == "Continue").unwrap().1.center();
                for pressed in [true, false] {
                    draw(&mut pane, pointer(pos, pressed));
                }
                assert!(pane.acknowledged && !pane.disclosure);
            }
        }
        assert_eq!(
            PRIVACY_GROUPS.iter().map(|(h, _)| *h).collect::<Vec<_>>(),
            ["What is sent", "What the agent can change", "What is stored on this Mac"]
        );
    }

    #[test]
    fn installation_instructions_are_secondary_adjacent_and_only_emit_on_click() {
        for state in ["missing", "unsupported"] {
            let ctx = egui::Context::default();
            let pane = crate::agent_demo::pane(crate::agent_demo::State::parse(state).unwrap(), 1);
            let mut actions = Vec::new();
            render(&ctx, 660.0, vec![], |ui| pane.onboarding(ui, &mut actions));
            assert!(actions.is_empty());
            let output = render(&ctx, 660.0, vec![], |ui| pane.onboarding(ui, &mut actions));
            let text = texts(&output);
            let check = text.iter().find(|(s, _)| s == "Check again").unwrap().1;
            let help = text.iter().find(|(s, _)| s == "Installation instructions").unwrap().1;
            assert!(help.left() > check.right() && (help.center().y - check.center().y).abs() < dp(1.0));
            assert!(!output.shapes.iter().any(
                |s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == theme::accent() && r.rect.contains_rect(help))
            ));
            for pressed in [true, false] {
                render(&ctx, 660.0, pointer(help.center(), pressed), |ui| pane.onboarding(ui, &mut actions));
            }
            assert_eq!(actions.len(), 1);
            assert!(matches!(actions[0], Action::InstallationInstructions));
        }
    }

    #[test]
    fn checking_and_signing_in_use_static_drawn_status_and_keep_login_actions() {
        for state in ["checking", "signing-in"] {
            let ctx = egui::Context::default();
            let pane = crate::agent_demo::pane(crate::agent_demo::State::parse(state).unwrap(), 1);
            let mut actions = Vec::new();
            let output = render(&ctx, 360.0, vec![], |ui| pane.onboarding(ui, &mut actions));
            let text = texts(&output);
            assert_eq!(text.iter().filter(|(s, _)| s == CHECKING_STATUS).count(), 1);
            assert!(!text.iter().any(|(s, _)| s == "Checking…"));
            assert!(
                output
                    .shapes
                    .iter()
                    .any(|s| matches!(&s.shape, egui::Shape::Circle(c) if c.stroke.color == readable(theme::muted())))
            );
            assert!(actions.is_empty());
            assert_eq!(pane.coalescer.due_in(Instant::now(), true), None);
            if state == "checking" {
                assert!(
                    !output
                        .shapes
                        .iter()
                        .any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == theme::accent()))
                );
            } else {
                for (label, reopen) in [("Cancel", false), ("Open sign-in page again", true)] {
                    let output = render(&ctx, 360.0, vec![], |ui| pane.onboarding(ui, &mut actions));
                    let pos = texts(&output).iter().find(|(s, _)| s == label).unwrap().1.center();
                    actions.clear();
                    for pressed in [true, false] {
                        render(&ctx, 360.0, pointer(pos, pressed), |ui| pane.onboarding(ui, &mut actions));
                    }
                    assert_eq!(actions.len(), 1);
                    assert!(matches!(
                        (&actions[0], reopen),
                        (Action::ReopenLogin, true) | (Action::CancelLogin, false)
                    ));
                }
            }
        }
    }

    #[test]
    fn chart_edits_mode_uses_the_same_name_at_all_widths_and_in_restart_notices() {
        for width in [360.0, 450.0, 660.0] {
            let ctx = egui::Context::default();
            immediate_tooltips(&ctx);
            let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("empty-ready").unwrap(), 1);
            let output = render(&ctx, width, vec![], |ui| pane.mode_switch(ui, &mut Vec::new()));
            let text = texts(&output);
            let pos = text.iter().find(|(s, _)| s == "Chart edits").unwrap().1.center();
            assert!(text.iter().any(|(s, _)| s == "Read only"));
            let mut seen = false;
            for _ in 0..4 {
                let output = render(&ctx, width, vec![egui::Event::PointerMoved(pos)], |ui| {
                    pane.mode_switch(ui, &mut Vec::new())
                });
                seen |= texts(&output).iter().any(|(s, _)| s == CHART_EDITS_HELP);
            }
            assert!(seen);
            assert!(pane.select_tools(ToolsMode::Edit, true));
            assert!(
                matches!(pane.blocks.last(), Some(RenderedBlock::Notice(s)) if s == "Chart edits on restart. Queue stays paused; nothing is resent.")
            );
        }
    }

    #[test]
    fn every_user_turn_keeps_you_at_the_left_across_demo_states_widths_and_long_markdown() {
        for state in crate::agent_demo::STATES {
            for width in [240.0, 360.0, 450.0, 660.0] {
                let ctx = egui::Context::default();
                let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse(state).unwrap(), 1);
                // Add a second turn after a long answer to exercise both author rows,
                // including fixtures that have no conversation to start with.
                pane.running = false;
                pane.state = Onboarding::Ready;
                pane.start_prompt(Prompt { text: "A long user prompt ".repeat(8), requires_tools: false });
                pane.blocks.push(RenderedBlock::Reply(agent_markdown::parse(&crate::agent_demo::MARKDOWN.repeat(3))));
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 20000.0))),
                        ..Default::default()
                    },
                    |ui| pane.conversation(ui, &mut Vec::new()),
                );
                output.textures_delta.clear();
                let text = texts(&output);
                let prompts = text
                    .iter()
                    .filter(|(s, _)| pane.blocks.iter().any(|b| matches!(b, RenderedBlock::Prompt(p) if p == s)))
                    .collect::<Vec<_>>();
                let authors = text.iter().filter(|(s, _)| s == "You").collect::<Vec<_>>();
                assert_eq!(prompts.len(), pane.turns.len(), "{state} {width}");
                // Two fractional ink passes per author, not a second visible label.
                assert_eq!(authors.len(), pane.turns.len() * 2, "{state} {width}");
                for (prompt, author) in prompts.iter().zip(authors.iter().step_by(2)) {
                    assert!((author.1.left() - prompt.1.left()).abs() < dp(1.0), "{state} {width}");
                    assert!(author.1.bottom() <= prompt.1.top());
                    assert!(author.1.left() >= 0.0 && author.1.right() <= width);
                }
            }
        }
    }

    #[test]
    fn data_basis_uses_source_bars_provider_flag_and_timeframe_progress() {
        use std::sync::Arc;
        use tr_core::{Bar, Timeframe};
        for (tf, word) in [(Timeframe::W1, "Weekly"), (Timeframe::D1, "Daily"), (Timeframe::H4, "4H")] {
            let mut chart = crate::chart_view::ChartState::new(1, "BTCUSD Crypto", tf);
            assert_eq!(DataBasis::from_chart(&chart, false), DataBasis::default());
            let now = chrono::Utc::now().timestamp();
            let ts = tf.bucket_start(now);
            let bars = Arc::new(vec![Bar::new(ts, 1., 2., 1., 2., 3.)]);
            chart.on_history(
                0,
                bars.clone(),
                tr_chart::scene::SeriesGpu::build(&bars),
                0,
                false,
                false,
                false,
                &crate::worker::Worker::inert(),
            );
            let basis = DataBasis::from_chart(&chart, true);
            assert!(basis.sample);
            assert_eq!(basis.last_bar, Some(ts));
            assert_eq!(basis.incomplete.as_deref(), Some(word));
            assert_eq!(basis.freshness(), Some(format!("bar opened {}", local_time(ts))));
            let worker = crate::worker::Worker::inert();
            let mut updated = bars.as_ref().clone();
            updated[0].close = 1.5;
            chart.on_live_bars(0, 0, updated, 1, &worker);
            let live_basis = DataBasis::from_chart(&chart, false);
            let received = chart.price_update_time().expect("accepted live update");
            assert!(received >= now);
            assert_eq!(live_basis.latest_price_update, Some(received));
            assert_eq!(live_basis.freshness(), Some(format!("as of {}", local_time(received))));
            chart.on_live_bars(1, 0, vec![], 0, &worker);
            assert_eq!(chart.price_update_time(), Some(received), "stale generation cannot refresh freshness");
            assert!(!DataBasis::from_chart(&chart, false).sample);
            let old = Arc::new(vec![Bar::new(ts - 2 * tf.seconds(), 1., 2., 1., 2., 3.)]);
            chart.on_history(
                0,
                old.clone(),
                tr_chart::scene::SeriesGpu::build(&old),
                0,
                false,
                false,
                false,
                &crate::worker::Worker::inert(),
            );
            assert!(DataBasis::from_chart(&chart, true).incomplete.is_none());
        }
    }

    #[test]
    fn reverted_turn_has_historical_label_muted_checks_and_no_redo() {
        let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("answer").unwrap(), 1);
        crate::agent_demo::tool(&mut pane, "line", "create_drawing", Some(true), Some("Horizontal line"));
        pane.undo_result(
            crate::agent_demo::TURN,
            Ok(crate::agent_bridge::UndoSummary { charts: 1, reverted: 1, skipped: vec![] }),
        );
        pane.expanded_tools.insert(0);
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| pane.conversation(ui, &mut Vec::new()));
        output.textures_delta.clear();
        let text = texts(&output);
        for label in ["1 change reverted · 7s", "Reverted"] {
            assert_eq!(text.iter().filter(|(s, _)| s == label).count(), 1, "{text:?}");
        }
        assert!(!text.iter().any(|(s, _)| s.starts_with("Undid")));
        assert!(!text.iter().any(|(s, _)| s.contains("Redo") || s.starts_with("Undo ")));
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Path(path) if path.points.len() == 3 && path.stroke.color == egui::epaint::ColorMode::Solid(readable(theme::muted())))));
        assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Path(path) if path.stroke.color == egui::epaint::ColorMode::Solid(readable(theme::up())))));
        pane.undo_result(crate::agent_demo::TURN, Err("unavailable".into()));
        assert_eq!(pane.turns[0].reverted_changes, Some(1));
    }

    #[test]
    fn grouped_suggestions_gate_only_chart_edits_with_disabled_tooltip() {
        for mode in [ToolsMode::ReadOnly, ToolsMode::Edit] {
            let ctx = egui::Context::default();
            immediate_tooltips(&ctx);
            let mut actions = Vec::new();
            let mut draw = |events| render(&ctx, 450.0, events, |ui| suggested_prompts(ui, mode, &mut actions));
            draw(vec![]);
            let output = draw(vec![]);
            let text = texts(&output);
            let mut previous = 0.0;
            for heading in ["Analyze", "Find a setup", "Add to chart"] {
                let rect = text.iter().find(|(s, _)| s == heading).unwrap().1;
                assert!(rect.top() >= previous);
                previous = rect.bottom();
            }
            for prompt in [
                "What's the trend?",
                "Where could a breakout confirm?",
                "Mark support and resistance zones",
                "Add RSI 14",
            ] {
                let rect = text.iter().find(|(s, _)| s == prompt).unwrap().1;
                let mut tooltip_seen = false;
                // Egui suppresses tooltips briefly after a click; advance input frames, not wall time.
                for _ in 0..90 {
                    let output = draw(vec![egui::Event::PointerMoved(rect.center())]);
                    tooltip_seen |= texts(&output).iter().any(|(s, _)| s == "Switch to Chart edits");
                }
                let enabled =
                    mode == ToolsMode::Edit || !["Mark support and resistance zones", "Add RSI 14"].contains(&prompt);
                if !enabled {
                    assert!(tooltip_seen, "{prompt}: {rect:?}");
                }
                for pressed in [true, false] {
                    draw(pointer(rect.center(), pressed));
                }
            }
            let sent: Vec<_> = actions
                .iter()
                .filter_map(|a| if let Action::Send(p) = a { Some(p.text.as_str()) } else { None })
                .collect();
            assert_eq!(sent.len(), if mode == ToolsMode::Edit { 4 } else { 2 }, "{mode:?}: {sent:?}");
            assert!(sent.contains(&"What's the trend?") && sent.contains(&"Where could a breakout confirm?"));
        }
    }

    #[test]
    fn finished_tool_group_uses_frozen_total_turn_time_not_tool_sum() {
        let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("answer").unwrap(), 1);
        let turn = &mut pane.turns[0];
        let label = "Analyzed chart · 3 steps".to_owned();
        assert_eq!(tool_group_duration(label.clone(), Some(turn)), "Analyzed chart · 3 steps · 7s");
        for (millis, expected) in
            [(0, "<1s"), (999, "<1s"), (1000, "1s"), (1499, "1s"), (1500, "2s"), (7499, "7s"), (7500, "8s")]
        {
            turn.duration = Some(Duration::from_millis(millis));
            assert_eq!(tool_group_duration(label.clone(), Some(turn)), format!("{label} · {expected}"));
        }
        turn.finished = false;
        assert_eq!(tool_group_duration(label.clone(), Some(turn)), label);
        assert_eq!(pane.coalescer.due_in(Instant::now(), true), None);

        let mut pane = AgentPane { state: Onboarding::Ready, ..Default::default() };
        pane.start_prompt(Prompt { text: "Read chart".into(), requires_tools: true });
        pane.turns[0].started_at = Instant::now() - Duration::from_secs(7);
        pane.event(BridgeEvent::Completed {
            turn: "duration".into(),
            status: "completed".into(),
            calls: 0,
            interrupt_elapsed: None,
        });
        let measured = pane.turns[0].duration.unwrap();
        assert!(measured >= Duration::from_secs(7));
        pane.finish_turn();
        assert_eq!(pane.turns[0].duration, Some(measured), "completion freezes the duration");
    }

    #[test]
    fn tool_group_progress_is_event_driven_short_and_truthful() {
        let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("streaming").unwrap(), 1);
        let label = |pane: &AgentPane| {
            tool_group_label(
                &pane
                    .blocks
                    .iter()
                    .filter_map(|b| if let RenderedBlock::Tool(t) = b { Some(t) } else { None })
                    .collect::<Vec<_>>(),
                None,
            )
        };
        assert_eq!(label(&pane), "Analyzing…");
        crate::agent_demo::tool(&mut pane, "analyze", "analyze", Some(true), None);
        assert_eq!(label(&pane), "Analyzed chart · 3 steps");
        for (name, expected) in
            [("read_chart", "Reading chart…"), ("read_data", "Reading bars…"), ("create_drawing", "Drawing…")]
        {
            let mut running = AgentPane::default();
            crate::agent_demo::tool(&mut running, "running", name, None, None);
            assert_eq!(label(&running), expected);
            assert_eq!(running.coalescer.due_in(Instant::now(), true), None, "static running icons add no timer");
        }
    }

    #[test]
    fn recovered_zone_headers_keep_failed_rows_and_muted_actionable_errors() {
        let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("edits").unwrap(), 1);
        pane.wire("item/completed", &serde_json::json!({"item": {
            "type":"dynamicToolCall", "id":"failed", "tool":"create_drawing", "success":false,
            "arguments":{"symbol":crate::agent_demo::SYMBOL,"kind":"rectangle","zone_role":"support",
                "anchors":[{"time":86400,"price":8.},{"time":172800,"price":12.}]},
            "contentItems":[{"type":"inputText","text":"{\"error\":{\"message\":\"anchors[1].price must differ from anchors[0].price\"}}"}]
        }}));
        crate::agent_demo::tool(&mut pane, "retry", "create_drawing", Some(true), Some("Support zone"));
        crate::agent_demo::tool(&mut pane, "second", "create_drawing", Some(true), Some("Resistance zone"));
        crate::agent_demo::complete(&mut pane, "completed");
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| pane.conversation(ui, &mut Vec::new()));
        output.textures_delta.clear();
        let text = texts(&output);
        assert!(text.iter().any(|(s, _)| s == "Drew 2 zones · 3 steps · 7s · 1 retry"));
        assert!(text.iter().any(|(s, _)| s == "anchors[1].price must differ from anchors[0].price"));
        let traces: Vec<_> =
            pane.blocks.iter().filter_map(|b| if let RenderedBlock::Tool(t) = b { Some(t) } else { None }).collect();
        assert_eq!(traces[0].success, Some(false));
        assert_eq!(recovered_failures(&traces), 1);
        let mut line = (*traces[2]).clone();
        line.summary = Some("Horizontal line".into());
        line.target_args["kind"] = "horizontal".into();
        line.target_args["zone_role"] = Value::Null;
        assert_eq!(tool_group_label(&[traces[1], &line], None), "Drew 1 zone and 1 line · 2 steps");
        assert_eq!(
            tool_group_label(&[traces[1], traces[0]], None),
            "Drew 1 zone · 2 steps · 1 failed",
            "an earlier success cannot recover a later failure"
        );
        assert_eq!(tool_group_label_outcome(&[traces[0]], None, true), "Chart tools failed · 1 step · 1 failed");
        assert_eq!(retry_suffix("Finished".into(), 2), "Finished · 2 retries");
        assert_eq!(recovered_failures(&[traces[0], traces[0], traces[1]]), 2);
        let mut other = (*traces[1]).clone();
        other.name = "analyze".into();
        assert_eq!(recovered_failures(&[traces[0], &other]), 0);
    }

    #[test]
    fn final_answer_completion_does_not_recover_a_tool_failure() {
        for (status, answer, recovered) in
            [("completed", true, true), ("failed", true, false), ("completed", false, false)]
        {
            let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("edits").unwrap(), 1);
            crate::agent_demo::tool(&mut pane, "failed", "analyze", Some(false), None);
            if answer {
                pane.wire("item/completed", &serde_json::json!({"item":{"type":"agentMessage", "id":"answer", "text":"Confirmed results only."}}));
            }
            crate::agent_demo::complete(&mut pane, status);
            assert_eq!(pane.turns[0].final_answer_succeeded, recovered);
            let traces: Vec<_> = pane
                .blocks
                .iter()
                .filter_map(|b| if let RenderedBlock::Tool(t) = b { Some(t) } else { None })
                .collect();
            assert_eq!(recovered_failures(&traces), 0);
            assert_eq!(tool_group_label_outcome(&traces, None, recovered), "Chart tools failed · 1 step · 1 failed");
        }
    }

    fn review_trace(id: &str, success: bool, summary: &str) -> Trace {
        Trace {
            id: id.into(),
            name: "create_drawing".into(),
            success: Some(success),
            duration_ms: Some(10),
            arguments: String::new(),
            target_args: serde_json::json!({"kind":"rectangle", "zone_role":if summary.contains("support") { Some("support") } else if summary.contains("resistance") { Some("resistance") } else { None },
                "anchors":[{"time":86400,"price":8.},{"time":172800,"price":12.}]}),
            summary: Some(summary.into()),
            interrupted: false,
            error: (!success).then(|| "anchors[1].time must be within the loaded range".into()),
        }
    }

    #[test]
    fn review_different_drawing_success_is_not_a_retry() {
        let failed = review_trace("r", false, "resistance zone");
        let other = review_trace("s", true, "#7 · support zone");
        assert_eq!(recovered_failures(&[&failed, &other]), 0);
        assert_eq!(tool_group_label(&[&failed, &other], None), "Drew 1 zone · 2 steps · 1 failed");
    }

    #[test]
    fn retries_require_the_same_drawing_identity_or_normalized_tool_arguments() {
        let failed = review_trace("failed", false, "support zone");
        let mut later = review_trace("later", true, "support zone");
        assert_eq!(recovered_failures(&[&failed, &later]), 1);
        later.target_args["anchors"] = serde_json::json!([{"price":11.},{"price":15.}]);
        assert_eq!(recovered_failures(&[&failed, &later]), 1, "overlapping ranges");
        later.target_args["anchors"] = serde_json::json!([{"price":20.},{"price":25.}]);
        assert_eq!(recovered_failures(&[&failed, &later]), 0, "different price target");
        let mut labeled = failed.clone();
        labeled.target_args["label"] = "weekly demand".into();
        later.target_args["label"] = "weekly demand".into();
        assert_eq!(recovered_failures(&[&labeled, &later]), 1, "same explicit label");
        for (field, value) in [("kind", "horizontal"), ("zone_role", "resistance"), ("side", "sell")] {
            let mut other = later.clone();
            other.target_args["anchors"] = failed.target_args["anchors"].clone();
            other.target_args[field] = value.into();
            assert_eq!(recovered_failures(&[&failed, &other]), 0, "{field}");
        }
        assert_eq!(recovered_failures(&[&later, &labeled]), 0, "success must be later");
        for tool in ["update_drawing", "delete_drawing"] {
            let mut failed = failed.clone();
            failed.name = tool.into();
            failed.target_args = serde_json::json!({"id":7,"label":"before"});
            let mut later = failed.clone();
            later.success = Some(true);
            later.target_args["label"] = "after".into();
            assert_eq!(recovered_failures(&[&failed, &later]), 1);
            later.target_args["id"] = 8.into();
            assert_eq!(recovered_failures(&[&failed, &later]), 0);
        }
        let mut failed = failed;
        failed.name = "analyze".into();
        failed.target_args = normalized_args(
            &serde_json::from_str::<Value>(r#"{ "mode": "trend", "symbol": " AAA ", "options": {"limit":20} }"#)
                .unwrap(),
        );
        let mut later = failed.clone();
        later.success = Some(true);
        later.target_args = normalized_args(&serde_json::json!({"options":{"limit":20},"symbol":"AAA","mode":"trend"}));
        assert_eq!(recovered_failures(&[&failed, &later]), 1);
        later.target_args["options"]["limit"] = 30.into();
        assert_eq!(recovered_failures(&[&failed, &later]), 0);
    }

    #[test]
    fn partially_retried_failures_keep_success_summary_and_only_failed_suffix() {
        let failed = review_trace("support-failed", false, "support zone");
        let unresolved = review_trace("resistance-failed", false, "resistance zone");
        let success = review_trace("support-success", true, "support zone");
        let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("edits").unwrap(), 1);
        pane.blocks.extend([failed, unresolved.clone(), success].map(RenderedBlock::Tool));
        crate::agent_demo::complete(&mut pane, "completed");
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| pane.conversation(ui, &mut Vec::new()));
        output.textures_delta.clear();
        let text = texts(&output);
        assert!(text.iter().any(|(s, _)| s.starts_with("Drew 1 zone · 3 steps · 1 failed")));
        assert!(!text.iter().any(|(s, _)| s.contains("1 retry")));
        let recovered = Trace { id: "resistance-success".into(), success: Some(true), error: None, ..unresolved };
        pane.blocks.push(RenderedBlock::Tool(recovered));
        let mut output = ctx.run_ui(Default::default(), |ui| pane.conversation(ui, &mut Vec::new()));
        output.textures_delta.clear();
        let text = texts(&output);
        assert!(text.iter().any(|(s, _)| s == "Drew 2 zones · 4 steps · 7s · 2 retries"));
        assert!(!text.iter().any(|(s, _)| s.contains("1 failed")));
    }

    #[test]
    fn review_plain_rectangle_is_not_a_zone() {
        let rectangle = review_trace("p", true, "#9 · rectangle (2 anchors)");
        assert_eq!(tool_group_label(&[&rectangle], None), "Drew 1 rectangle · 1 step");
        assert_eq!(tool_group_label(&[&rectangle, &rectangle], None), "Drew 2 rectangles · 2 steps");
        let zone = review_trace("zone", true, "support zone");
        assert_eq!(tool_group_label(&[&zone, &rectangle], None), "Drew 1 zone and 1 rectangle · 2 steps");
    }

    #[test]
    fn compact_reset_thresholds_include_local_date_only_when_needed() {
        let now = crate::agent_demo::SAMPLE_NOW;
        for (minutes, expected) in [
            (1, "in 1 minute"),
            (42, "in 42 minutes"),
            (89, "in 89 minutes"),
            (90, "in 1h 30m"),
            (320, "in 5h 20m"),
            (2879, "in 47h 59m"),
            (2880, "in 2d 0h"),
            (9420, "in 6d 13h"),
        ] {
            let reset = reset_time_at(Some(now + minutes * 60), now);
            assert!(reset.starts_with(&format!("{expected} · ")), "{reset}");
            assert!(reset.ends_with(&local_time(now + minutes * 60)));
            if minutes >= 2880 {
                let date = chrono::DateTime::from_timestamp(now + minutes * 60, 0)
                    .unwrap()
                    .with_timezone(&chrono::Local)
                    .format("%b %d")
                    .to_string();
                assert!(reset.contains(&date), "{reset}");
            }
        }
        assert!(reset_time_at(Some(now - 1), now).starts_with("now · "));
        let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("rate-limited").unwrap(), 1);
        pane.limits[0] =
            Limit { id: "weekly".into(), used: 25., reset: reset_time_at(Some(now + 9420 * 60), now), minutes: 10080 };
        pane.limits_value = Value::Null;
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            pane.show(ui, "BTC/USD · 4H", egui::Id::new("long-reset"));
        });
        output.textures_delta.clear();
        assert!(texts(&output).iter().any(|(s, _)| s == "weekly · Weekly limit 25% used"));
        // When not blocked, meters may still expose their own reset windows.
        pane.state = Onboarding::Ready;
        let mut output = ctx.run_ui(Default::default(), |ui| {
            pane.show(ui, "BTC/USD · 4H", egui::Id::new("long-reset-ready"));
        });
        output.textures_delta.clear();
        assert!(texts(&output).iter().any(|(s, _)| s.contains("Weekly limit 25% used · resets in 6d 13h")));
    }

    #[test]
    fn state_specific_placeholders_disable_input_and_primary_actions_use_accent() {
        for (state, expected) in [
            (Onboarding::SignedOut, "Sign in to ask about this chart"),
            (Onboarding::Missing, "Install Codex to start"),
            (Onboarding::Unsupported { found: "old".into() }, "Install Codex to start"),
            (Onboarding::Checking, "Starting…"),
            (Onboarding::SigningIn, "Starting…"),
            (Onboarding::RateLimited { reset: "later".into() }, "Usage limit reached"),
            (Onboarding::Stopped { reason: "stopped".into(), unsafe_backend: false }, "Agent stopped"),
        ] {
            assert!(!state.accepts_input());
            assert_eq!(state.composer_placeholder(), expected);
        }
        for name in ["signed-out", "empty-ready", "interrupted", "stopped-recoverable"] {
            let ctx = egui::Context::default();
            let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse(name).unwrap(), 1);
            if name == "empty-ready" {
                pane.input = "trend?".into();
            }
            let mut output = ctx.run_ui(Default::default(), |ui| {
                pane.show(ui, "BTC/USD · 1W", egui::Id::new("primary"));
            });
            output.textures_delta.clear();
            assert!(
                output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == theme::accent())),
                "{name}"
            );
        }
        assert_eq!(RETRY_HELP, "Sends the same prompt again and starts a new answer.");
        assert_eq!(
            CHART_EDITS_HELP,
            "The agent can add, change and remove its own drawings, indicators and comparisons on the pinned chart, and propose watchlist and alert changes for you to approve. Your objects are never changed without approval, and every agent edit can be undone."
        );
    }

    #[test]
    fn clearing_rate_limit_never_resumes_queue_and_failed_turn_keeps_retry() {
        let mut pane = AgentPane { state: Onboarding::Ready, ..Default::default() };
        pane.enqueue(Prompt { text: "queued".into(), requires_tools: true });
        pane.set_limits(&serde_json::json!({"rateLimits":{"primary":{"usedPercent":100,"resetsAt":crate::agent_demo::SAMPLE_NOW + 42 * 60}}}));
        assert!(pane.queue_paused);
        pane.set_limits(&serde_json::json!({"rateLimits":{"primary":{"usedPercent":0}}}));
        assert_eq!(pane.state, Onboarding::Ready);
        assert!(pane.next_prompt().is_none());
        pane.resume_queue();
        assert_eq!(pane.next_prompt().unwrap().text, "queued");
        crate::agent_demo::complete(&mut pane, "failed");
        assert!(pane.show_retry());
        assert!(matches!(pane.blocks.last(), Some(RenderedBlock::Notice(text)) if text.starts_with("Turn failed.")));
    }

    #[test]
    fn queue_review_remove_resume_and_clear_are_explicit_and_hide_when_empty() {
        fn frame(pane: &mut AgentPane, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
            let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| pane.queue_row(ui));
            output.textures_delta.clear();
            output
        }
        fn click(pane: &mut AgentPane, ctx: &egui::Context, label: &str) {
            frame(pane, ctx, vec![]);
            let output = frame(pane, ctx, vec![]);
            let pos = texts(&output).iter().find(|(s, _)| s == label).unwrap().1.center();
            for pressed in [true, false] {
                frame(
                    pane,
                    ctx,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
        }
        let ctx = egui::Context::default();
        let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("queue").unwrap(), 1);
        assert!(pane.queue_review);
        let first = pane.queue[0].text.clone();
        let output = frame(&mut pane, &ctx, vec![]);
        let text = texts(&output);
        let rect = |label: &str| text.iter().find(|(s, _)| s == label).unwrap().1;
        assert!(rect("1.").left() < rect(&first).left());
        assert!(rect(&first).right() < rect("Remove").left());
        assert!((rect(&first).left() - rect(&pane.queue[1].text).left()).abs() < dp(1.0));
        assert!(rect("1.").left() == rect("2.").left());
        assert!(text.iter().any(|(s, _)| s == "Hide queue"));
        click(&mut pane, &ctx, "Hide queue");
        let output = frame(&mut pane, &ctx, vec![]);
        assert!(texts(&output).iter().any(|(s, _)| s == "Review"));
        assert!(!texts(&output).iter().any(|(s, _)| s == &first));
        click(&mut pane, &ctx, "Review");
        click(&mut pane, &ctx, "Remove");
        assert_eq!(pane.queue.len(), 1);
        assert!(!pane.queue.iter().any(|prompt| prompt.text == first));
        assert!(pane.queue_paused);
        click(&mut pane, &ctx, "Resume");
        assert!(!pane.queue_paused);
        let output = frame(&mut pane, &ctx, vec![]);
        assert!(!texts(&output).iter().any(|(s, _)| s == "Resume"));
        click(&mut pane, &ctx, "Clear");
        assert!(pane.queue.is_empty());
        assert!(texts(&frame(&mut pane, &ctx, vec![])).is_empty());
    }

    #[test]
    fn interrupted_retry_is_adjacent_and_emits_retry_not_resume() {
        let ctx = egui::Context::default();
        let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("interrupted").unwrap(), 1);
        let mut actions = Vec::new();
        let mut render = |pane: &mut AgentPane, events: Vec<egui::Event>| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(660., 800.))),
                    ..Default::default()
                },
                |ui| {
                    actions = pane.show(ui, "BTC/USD · 1W", egui::Id::new("retry-adjacent"));
                },
            );
            output.textures_delta.clear();
            output
        };
        render(&mut pane, vec![]);
        let output = render(&mut pane, vec![]);
        let text = texts(&output);
        let notice = text.iter().find(|(s, _)| s == INTERRUPTED).unwrap().1;
        let retry = text.iter().find(|(s, _)| s == "Retry response").unwrap().1;
        assert_eq!(text.iter().filter(|(s, _)| s == "Retry response").count(), 1);
        assert!((notice.center().y - retry.center().y).abs() <= dp(5.0));
        assert!(retry.left() >= notice.right());
        let pos = retry.center();
        for pressed in [true, false] {
            render(
                &mut pane,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert!(actions.iter().any(|action| matches!(action, Action::Retry)));
        assert!(pane.queue_paused);
        let original = pane.last_prompt.clone().unwrap();
        let turn_count = pane.turns.len();
        assert_eq!(pane.start_prompt(original.clone()), Some(original));
        assert_eq!(pane.turns.len(), turn_count + 1);
        assert!(pane.queue_paused);
    }

    #[test]
    fn user_prompt_has_small_you_label_strong_ink_hairline_and_no_caret_rule() {
        let ctx = egui::Context::default();
        let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("answer").unwrap(), 1);
        pane.start_prompt(Prompt { text: "Second question".into(), requires_tools: true });
        let mut output = ctx.run_ui(Default::default(), |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(dp(8.0), dp(8.0));
            pane.conversation(ui, &mut Vec::new());
        });
        output.textures_delta.clear();
        let text = texts(&output);
        let previous = text.iter().find(|(s, _)| s.starts_with("These are synthetic sample bars")).unwrap().1;
        let next_you = text.iter().filter(|(s, _)| s == "You").map(|(_, rect)| rect.top()).fold(0.0_f32, f32::max);
        let gap = next_you - previous.bottom();
        assert!((dp(16.0)..=dp(24.0)).contains(&gap), "turn gap {gap}");
        let you = text.iter().find(|(s, _)| s == "You").unwrap().1;
        let prompt = text.iter().find(|(s, _)| s.starts_with("What's the weekly")).unwrap().1;
        assert!(you.bottom() <= prompt.top());
        let question = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == "Second question" => Some(t),
                _ => None,
            })
            .unwrap();
        assert_eq!(question.galley.job.sections[0].format.color, theme::text());
        assert!(output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::LineSegment { points, .. } if points[0].y == points[1].y && points[0].y > prompt.bottom())));
        assert!(!output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::LineSegment { points, .. } if points[0].x == prompt.left() && points[1].x == prompt.left() && points[0].y <= prompt.top() && points[1].y >= prompt.bottom())));
    }

    #[test]
    fn response_style_defaults_and_all_choices_persist_without_conversation() {
        assert_eq!(Settings::default().response_style, ResponseStyle::Analyst);
        assert_eq!(serde_json::from_str::<Settings>("{}").unwrap().response_style, ResponseStyle::Analyst);
        for (serialized, expected) in [
            ("Terse", ResponseStyle::Terse),
            ("Brief", ResponseStyle::Terse),
            ("Analyst", ResponseStyle::Analyst),
            ("Standard", ResponseStyle::Analyst),
        ] {
            let settings: Settings = serde_json::from_value(serde_json::json!({"response_style": serialized})).unwrap();
            assert_eq!(settings.response_style, expected);
            assert_eq!(serde_json::to_value(&settings).unwrap()["response_style"], expected.label());
        }
        assert!(
            ResponseStyle::Terse
                .instruction()
                .contains("Lead with numbers; use minimal prose, key levels and timestamps")
        );
        assert!(
            ResponseStyle::Analyst.instruction().contains("brief plain English with key numbers and a bottom line")
        );
        assert!(ResponseStyle::Detailed.instruction().contains("reasoning and concepts"));
        for style in [ResponseStyle::Terse, ResponseStyle::Analyst, ResponseStyle::Detailed] {
            let settings = Settings { response_style: style, ..Default::default() };
            let json = serde_json::to_string(&crate::layouts::PaneSettings::Agent(settings.clone())).unwrap();
            let saved: crate::layouts::PaneSettings = serde_json::from_str(&json).unwrap();
            assert_eq!(saved, crate::layouts::PaneSettings::Agent(settings));
        }
    }

    #[test]
    fn notices_use_meaning_only_ink_with_readable_contrast_in_every_theme() {
        for (text, severity) in [
            ("3 changes reverted", Severity::Info),
            ("Read only on restart", Severity::Info),
            ("Queue paused", Severity::Warning),
            ("Turn interrupted. Answer is provisional", Severity::Warning),
            ("Recovered with history lost", Severity::Warning),
            ("Safety check failed", Severity::Error),
            ("Could not open sign-in", Severity::Error),
            (UNDO_UNAVAILABLE, Severity::Error),
        ] {
            assert_eq!(notice_severity(text), severity, "{text}");
        }
        let mut pane = AgentPane::default();
        pane.event(BridgeEvent::Wire(WireEvent::Error("opaque diagnostic".into())));
        assert_eq!(pane.notice_levels.get(&0), Some(&Severity::Error));
        let original = theme::palette();
        for t in crate::themes::builtins() {
            theme::set_palette(t.palette(theme::DEFAULT_ACCENT));
            for ink in [theme::text(), readable(theme::muted())] {
                assert!(contrast(ink, theme::card()) >= 4.5, "{} user/tool/status ink", t.name);
            }
            for severity in [Severity::Info, Severity::Warning, Severity::Error] {
                assert!(contrast(severity.color(), theme::card()) >= 4.5, "{} {severity:?}", t.name);
            }
        }
        theme::set_palette(original);
    }

    #[test]
    fn composer_queue_and_retry_visibility_follows_turn_outcome() {
        let mut pane = AgentPane { state: Onboarding::Ready, ..Default::default() };
        assert!(pane.queue.is_empty());
        assert!(!pane.show_retry());
        pane.start_prompt(Prompt { text: "trend".into(), requires_tools: true });
        assert!(!pane.show_retry());
        pane.event(BridgeEvent::Completed {
            turn: "one".into(),
            status: "completed".into(),
            calls: 0,
            interrupt_elapsed: None,
        });
        assert!(!pane.show_retry());
        pane.start_prompt(pane.last_prompt.clone().unwrap());
        pane.interrupt();
        pane.event(BridgeEvent::Completed {
            turn: "two".into(),
            status: "interrupted".into(),
            calls: 0,
            interrupt_elapsed: None,
        });
        assert!(pane.show_retry());
        pane.enqueue(Prompt { text: "next".into(), requires_tools: false });
        assert_eq!(pane.queue.len(), 1);
        pane.state = Onboarding::ToolsUnavailable { reason: "test".into() };
        assert!(pane.show_retry());
        pane.state = Onboarding::SignedOut;
        assert!(!pane.show_retry());
    }

    #[test]
    fn onboarding_copy_and_primary_action_are_one_adjacent_block() {
        for (state, title, copy, primary) in [
            (
                Onboarding::Dormant,
                "Chart analyst",
                "Chart tools run locally in a sandbox; prompts and chart data are sent to OpenAI.",
                "Start",
            ),
            (Onboarding::Missing, "Codex isn't installed", "Install the Codex CLI", "Check again"),
            (
                Onboarding::Unsupported { found: "0.160.0".into() },
                "Codex version not supported",
                "Found 0.160.0; this build is tested with 0.163.0.",
                "Check again",
            ),
            (
                Onboarding::SignedOut,
                "Sign in with ChatGPT",
                "API-key accounts aren't supported.",
                "Sign in with ChatGPT",
            ),
            (Onboarding::SigningIn, "Finish signing in", "your browser", "Cancel"),
            (Onboarding::RateLimited { reset: "18:00 UTC".into() }, "Usage limit reached", "18:00 UTC", "Check limits"),
            (
                Onboarding::ToolsUnavailable { reason: "router".into() },
                "Chart data unavailable",
                "No analysis was completed.",
                "Retry response",
            ),
            (
                Onboarding::Stopped { reason: "tripwire".into(), unsafe_backend: true },
                "Agent stopped for safety",
                "Close it and start a new one.",
                "Close",
            ),
            (
                Onboarding::Stopped { reason: "lost".into(), unsafe_backend: false },
                "Agent disconnected",
                "nothing is resent",
                "Restart",
            ),
        ] {
            let ctx = egui::Context::default();
            let pane = AgentPane { state, ..Default::default() };
            assert_eq!(pane.state.title(), title);
            assert!(pane.state.description().contains(copy));
            let mut output = ctx.run_ui(Default::default(), |ui| pane.onboarding(ui, &mut Vec::new()));
            output.textures_delta.clear();
            let text = texts(&output);
            let body = text.iter().find(|(s, _)| *s == pane.state.description()).unwrap().1;
            let action = text.iter().rfind(|(s, _)| s == primary).unwrap().1;
            let last_line = if pane.state == Onboarding::SigningIn {
                text.iter().find(|(s, _)| s == CHECKING_STATUS).unwrap().1
            } else {
                body
            };
            assert!(action.top() >= last_line.bottom());
            assert!(action.top() - last_line.bottom() <= dp(16.0), "{title}");
            if pane.state == Onboarding::Dormant {
                assert_eq!(
                    pane.state.description(),
                    "Answers questions about your pinned chart using your ChatGPT account. Chart tools run locally in a sandbox; prompts and chart data are sent to OpenAI."
                );
            }
            if pane.state == Onboarding::SigningIn {
                assert!(text.iter().any(|(s, _)| s == "Open sign-in page again"));
            }
            output.textures_delta.clear();
        }
    }

    #[test]
    fn header_controls_and_data_context_wrap_without_clipping_in_desk_themes() {
        for name in ["Terminal Dark", "Terminal Light", "Bloomberg"] {
            for width in [360.0, 450.0, 660.0] {
                let ctx = egui::Context::default();
                let themes = crate::themes::builtins();
                let t = themes.iter().find(|t| t.name == name).unwrap();
                theme::configure(&ctx, t, theme::DEFAULT_ACCENT);
                let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("empty-ready").unwrap(), 1);
                let mut bottom = 0.0;
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 700.0))),
                        ..Default::default()
                    },
                    |ui| {
                        pane.header(ui, "BTC/USD · 1W", &mut Vec::new());
                        bottom = ui.min_rect().bottom();
                    },
                );
                output.textures_delta.clear();
                let text = texts(&output);
                assert!(bottom <= dp(108.0), "{name} {width}: {bottom} {text:?}");
                for label in [
                    "Read only",
                    "Chart edits",
                    "BTC/USD · 1W",
                    "Standard",
                    "Ready",
                    "Sample data",
                    "Weekly bar incomplete",
                ] {
                    let rect = text
                        .iter()
                        .find(|(s, _)| s == label)
                        .unwrap_or_else(|| panic!("{name} {width}: missing {label}: {text:?}"))
                        .1;
                    assert!(rect.right() <= width && rect.left() >= 0.0, "{name} {width}: {label} {rect:?}");
                }
                assert!(!text.iter().any(|(s, _)| ["Brief", "Detailed", "Terse", "Analyst"].contains(&s.as_str())));
                for (_, rect) in &text {
                    assert!(rect.right() <= width + 0.5, "{name} {width}: {text:?}");
                }
                output.textures_delta.clear();
            }
        }
        assert_eq!(header_rows(360.0, 600.0), 2);
        assert_eq!(header_rows(660.0, 600.0), 1);
    }

    #[test]
    fn context_chip_has_outline_hover_and_emits_pin_while_dropdowns_have_chevrons() {
        let ctx = egui::Context::default();
        let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("empty-ready").unwrap(), 1);
        let mut actions = Vec::new();
        let mut frame = |pane: &mut AgentPane, events: Vec<egui::Event>| {
            let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
                pane.header(ui, "BTC/USD · 1W", &mut actions);
            });
            output.textures_delta.clear();
            output
        };
        frame(&mut pane, vec![]);
        let output = frame(&mut pane, vec![]);
        let chip = texts(&output).iter().find(|(s, _)| s == "BTC/USD · 1W").unwrap().1;
        assert!(output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.rect.contains_rect(chip) && r.stroke.color == theme::border())));
        // Model and style both paint line-path chevrons, not text glyphs.
        assert!(
            output.shapes.iter().filter(|s| matches!(&s.shape, egui::Shape::Path(p) if p.points.len() == 3)).count()
                >= 2
        );
        let pos = chip.center();
        let hover = frame(&mut pane, vec![egui::Event::PointerMoved(pos)]);
        assert!(hover.shapes.iter().any(
            |s| matches!(&s.shape, egui::Shape::Rect(r) if r.rect.contains(pos) && r.fill == theme::control_hover())
        ));
        for pressed in [true, false] {
            frame(
                &mut pane,
                vec![egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
        assert!(actions.iter().any(|action| matches!(action, Action::Pin)));
    }

    #[test]
    fn tool_inspector_expands_on_click_and_keeps_object_ids_in_tooltips() {
        fn frame(pane: &mut AgentPane, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(450.0, 700.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    pane.show(ui, "BTC/USD · 1W", egui::Id::new("tool-inspector"));
                },
            );
            output.textures_delta.clear();
            output
        }
        let ctx = egui::Context::default();
        let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("answer").unwrap(), 1);
        crate::agent_demo::tool(
            &mut pane,
            "line",
            "create_drawing",
            Some(true),
            Some("#1:3 · Horizontal line at 27,534.92"),
        );
        frame(&mut pane, &ctx, vec![]);
        let output = frame(&mut pane, &ctx, vec![]);
        let text = texts(&output);
        assert!(!text.iter().any(|(s, _)| s.contains("Horizontal line")));
        let pos = text.iter().find(|(s, _)| s == "Drew 1 line · 4 steps · 7s").unwrap().1.center();
        let click = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&mut pane, &ctx, vec![egui::Event::PointerMoved(pos), click(true)]);
        frame(&mut pane, &ctx, vec![click(false)]);
        let output = frame(&mut pane, &ctx, vec![]);
        assert!(pane.expanded_tools.contains(&0));
        let text = texts(&output);
        assert!(text.iter().any(|(s, _)| s == "Draw · Horizontal line at 27,534.92"));
        assert!(text.iter().any(|(s, _)| s == "42 ms"));
        assert!(!text.iter().any(|(s, _)| s.contains("#1:3") || s == "Details" || s.contains("chart_id")));
        assert!(text.iter().any(|(s, _)| s == "Drew 1 line · 4 steps · 7s"));
        assert_eq!(price_label(27534.92), "27,534.92");
        assert_eq!(main_tool_label("#1:3 · RSI 14"), "RSI 14");
    }

    #[test]
    fn tool_rows_use_human_verbs_status_icons_and_aligned_mono_durations() {
        let names = [
            ("read_chart", "Read chart"),
            ("read_data", "Read bars"),
            ("analyze", "Analyze"),
            ("indicator_values", "Indicator values"),
            ("find_symbol", "Find symbol"),
            ("read_user_actions", "Recent actions"),
            ("screenshot", "Screenshot"),
            ("create_drawing", "Draw"),
            ("update_drawing", "Edit drawing"),
            ("delete_drawing", "Delete drawing"),
            ("add_indicator", "Add indicator"),
            ("remove_indicator", "Remove indicator"),
            ("set_view", "Change view"),
            ("compare", "Compare"),
        ];
        for (name, expected) in names {
            assert_eq!(tool_label(name), expected);
        }
        for width in [360.0, 450.0, 660.0] {
            let ctx = egui::Context::default();
            ctx.all_styles_mut(|style| {
                style.interaction.tooltip_delay = 0.0;
                style.interaction.tooltip_grace_time = 0.0;
            });
            let mut row_rect = egui::Rect::NOTHING;
            let trace = Trace {
                id: "internal-call".into(),
                name: "create_drawing".into(),
                success: Some(true),
                duration_ms: Some(42),
                arguments: "safe projected arguments".into(),
                target_args: Value::Null,
                summary: Some("#1:3 · Horizontal line at 27,534.92".into()),
                interrupted: false,
                error: None,
            };
            for (success, interrupted, expected) in [
                (Some(true), false, "ok"),
                (Some(false), false, "failed"),
                (None, false, "running"),
                (None, true, "interrupted"),
            ] {
                let trace = Trace { success, interrupted, ..trace.clone() };
                assert_eq!(tool_status(&trace), expected);
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 700.0))),
                        ..Default::default()
                    },
                    |ui| {
                        let start = ui.next_widget_position();
                        tool_row(ui, &trace);
                        row_rect = egui::Rect::from_min_max(start, ui.min_rect().max);
                    },
                );
                output.textures_delta.clear();
                let text = texts(&output);
                let label = text.iter().find(|(s, _)| s.starts_with("Draw · ")).unwrap();
                let duration = text.iter().find(|(s, _)| s == "42 ms").unwrap();
                assert!(label.1.right() + dp(6.0) <= duration.1.left());
                assert!((duration.1.right() - width).abs() < 0.1);
                assert!(row_rect.height() <= dp(24.0));
                assert!(!text.iter().any(|(s, _)| s.contains("#1:3") || s.contains("internal-call") || s == "Details"));
                assert!(output.shapes.iter().any(|s| !matches!(s.shape, egui::Shape::Text(_) | egui::Shape::Noop)));
                let duration = output
                    .shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::Shape::Text(t) if t.galley.text() == "42 ms" => Some(t),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(duration.galley.job.sections[0].format.font_id.family, egui::FontFamily::Monospace);
            }
            // Hover anywhere on the row exposes diagnostics without a second disclosure.
            let pos = row_rect.center();
            for _ in 0..3 {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 700.0))),
                        events: vec![egui::Event::PointerMoved(pos)],
                        ..Default::default()
                    },
                    |ui| tool_row(ui, &trace),
                );
                output.textures_delta.clear();
                if texts(&output).iter().any(|(s, _)| {
                    s.contains("internal-call") && s.contains("#1:3") && s.contains("safe projected arguments")
                }) {
                    row_rect = egui::Rect::NOTHING;
                    break;
                }
            }
            assert_eq!(row_rect, egui::Rect::NOTHING, "missing row tooltip at {width}");
        }
    }

    #[test]
    fn queue_row_is_above_composer_and_retry_stays_with_interruption() {
        for width in [360.0, 450.0, 660.0] {
            let ctx = egui::Context::default();
            let mut pane =
                AgentPane { state: Onboarding::Ready, acknowledged: true, disclosure: false, ..Default::default() };
            pane.start_prompt(Prompt { text: "trend".into(), requires_tools: true });
            pane.enqueue(Prompt { text: "next".into(), requires_tools: false });
            pane.event(BridgeEvent::Completed {
                turn: "failed".into(),
                status: "failed".into(),
                calls: 0,
                interrupt_elapsed: None,
            });
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 700.0))),
                    ..Default::default()
                },
                |ui| {
                    pane.show(ui, "BTC/USD · 1W", egui::Id::new("queue-composer"));
                },
            );
            output.textures_delta.clear();
            let text = texts(&output);
            let rect =
                |label: &str| text.iter().find(|(s, _)| s == label).unwrap_or_else(|| panic!("missing {label}")).1;
            let send = rect("Add to queue");
            for label in ["1 queued · Paused", "Review", "Resume", "Clear"] {
                assert!(rect(label).bottom() < send.top());
                assert!(rect(label).right() <= width);
            }
            assert!(rect("Retry response").top() < rect("1 queued · Paused").top());
            assert!(rect("Not investment advice").bottom() <= 700.0);
            assert!(!text.iter().any(|(s, _)| s == "Retry"));
        }
    }

    #[test]
    fn composer_is_a_padded_focus_bordered_field_that_grows_to_six_lines() {
        for name in ["Terminal Dark", "Terminal Light", "Bloomberg"] {
            for width in [360.0, 450.0, 660.0] {
                let ctx = egui::Context::default();
                let themes = crate::themes::builtins();
                theme::configure(&ctx, themes.iter().find(|t| t.name == name).unwrap(), theme::DEFAULT_ACCENT);
                let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("empty-ready").unwrap(), 1);
                let id = egui::Id::new("field-test");
                let mut heights = Vec::new();
                for (lines, focused) in [(0, false), (3, true), (12, true)] {
                    pane.input = (0..lines).map(|_| "Sample line").collect::<Vec<_>>().join("\n");
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 700.0))),
                            ..Default::default()
                        },
                        |ui| {
                            if focused {
                                ui.memory_mut(|m| m.request_focus(input_id(id)));
                            }
                            pane.show(ui, "BTC/USD · 1W", id);
                        },
                    );
                    output.textures_delta.clear();
                    let text = texts(&output);
                    let send = text.iter().find(|(s, _)| s == "Send").unwrap().1;
                    let field = output
                        .shapes
                        .iter()
                        .find_map(|s| match &s.shape {
                            egui::Shape::Rect(r)
                                if r.fill == theme::control()
                                    && r.stroke.width == dp(1.0)
                                    && r.rect.contains_rect(send) =>
                            {
                                Some(r)
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| panic!("missing composer field: {name} {width}"));
                    assert_eq!(field.stroke.color, if focused { theme::accent() } else { theme::border() });
                    assert_eq!(field.corner_radius, egui::CornerRadius::same(dp(6.0) as u8));
                    let text = texts(&output);
                    let send = text.iter().find(|(s, _)| s == "Send").unwrap().1;
                    assert!(field.rect.contains_rect(send));
                    assert!(field.rect.right() - send.right() >= dp(8.0));
                    assert!(field.rect.right() <= width - dp(12.0) && width - field.rect.right() <= dp(13.0));
                    let line_height = ctx.fonts_mut(|fonts| fonts.row_height(&theme::body_font(13.0)));
                    assert!(field.rect.height() <= line_height * 6.0 + dp(18.0));
                    if lines == 0 {
                        let hint = text.iter().find(|(s, _)| s == "Ask about the pinned chart").unwrap().1;
                        assert!(hint.left() - field.rect.left() >= dp(8.0));
                        assert!(hint.right() <= send.left());
                    }
                    let disclaimer = text.iter().find(|(s, _)| s == "Not investment advice").unwrap().1;
                    assert!(
                        disclaimer.bottom() <= 700.0,
                        "{name} {width} {lines} lines: field {:?}, disclaimer {disclaimer:?}",
                        field.rect
                    );
                    heights.push(field.rect.height());
                }
                assert!(heights[0] < heights[1] && heights[1] < heights[2], "{name} {width}: {heights:?}");
            }
        }
    }

    #[test]
    fn undo_is_at_tool_results_right_edge_below_prompt_and_proportional_time() {
        for width in [360.0, 450.0, 660.0] {
            let ctx = egui::Context::default();
            let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("edits").unwrap(), 1);
            for (id, name, summary) in [
                ("one", "create_drawing", "Horizontal line"),
                ("two", "create_drawing", "Trend line"),
                ("three", "add_indicator", "RSI 14"),
            ] {
                crate::agent_demo::tool(&mut pane, id, name, Some(true), Some(summary));
            }
            crate::agent_demo::complete(&mut pane, "completed");
            pane.refresh_undo(&[crate::agent_demo::TURN.into()], Some(crate::agent_demo::TURN));
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 700.0))),
                    ..Default::default()
                },
                |ui| {
                    pane.show(ui, "BTC/USD · 1W", egui::Id::new("inline-undo"));
                },
            );
            output.textures_delta.clear();
            let text = texts(&output);
            let undo = text.iter().find(|(s, _)| s == "Undo 3 chart changes").unwrap().1;
            let prompt = text.iter().find(|(s, _)| s.starts_with("Mark support")).unwrap().1;
            let time = text.iter().find(|(s, _)| s == &pane.turns[0].local_time).unwrap().1;
            let tools = text.iter().find(|(s, _)| s == "Drew 2 lines and added RSI 14 · 3 steps · 7s").unwrap().1;
            assert!(undo.top() >= prompt.bottom());
            assert!(time.bottom() <= prompt.top());
            assert!(tools.right() <= undo.left());
            assert!((tools.top() - undo.top()).abs() <= dp(5.0));
            assert!(
                undo.right() <= width && width - undo.right() <= dp(24.0),
                "{width}: {undo:?}, time {time:?}, prompt {prompt:?}"
            );
            let time = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(t) if t.galley.text() == pane.turns[0].local_time => Some(t),
                    _ => None,
                })
                .unwrap();
            assert_eq!(time.galley.job.sections[0].format.font_id, theme::body_font(10.5));
        }
    }

    #[test]
    fn local_times_and_rate_limit_copy_never_show_raw_offsets() {
        let timestamp = crate::agent_demo::SAMPLE_NOW;
        let time = local_time(timestamp);
        assert!(!time.contains('+') && !time.contains('-'), "{time}");
        let reset = reset_time_at(Some(timestamp + 42 * 60), timestamp);
        assert!(reset.starts_with("in 42 minutes · "), "{reset}");
        assert!(reset.ends_with(&local_time(timestamp + 42 * 60)));
        assert!(!reset.contains("AM") && !reset.contains("PM"));
        let clock = &time[..5];
        assert_eq!(clock.as_bytes()[2], b':');
        assert!(clock[..2].parse::<u8>().unwrap() < 24);
        assert!(!reset.contains('+'));
        assert_eq!(reset_time_at(None, timestamp), "time unavailable");
        assert_eq!(local_time(i64::MAX), "time unavailable");
        let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("rate-limited").unwrap(), 1);
        pane.limits[0].reset = "in 42 minutes · 20:42 EDT".into();
        pane.state = Onboarding::RateLimited { reset: "in 42 minutes · 20:42 EDT".into() };
        assert_eq!(
            pane.state.description(),
            "Resets in 42 minutes · 20:42 EDT. Queued prompts stay paused until you resume them."
        );
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            pane.show(ui, "Pinned", egui::Id::new("times"));
        });
        output.textures_delta.clear();
        let text = texts(&output);
        assert!(text.iter().any(|(s, _)| s == "5-hour limit 100% used"));
        assert_eq!(text.iter().filter(|(s, _)| s.contains("20:42 EDT")).count(), 1);
        assert_eq!(text.iter().filter(|(s, _)| s.contains("in 42 minutes")).count(), 1);
    }

    #[test]
    fn conversation_wraps_and_composer_stays_visible_at_review_widths() {
        for name in ["Terminal Dark", "Terminal Light", "Bloomberg"] {
            for width in [360.0, 450.0, 660.0] {
                let ctx = egui::Context::default();
                let themes = crate::themes::builtins();
                theme::configure(&ctx, themes.iter().find(|t| t.name == name).unwrap(), theme::DEFAULT_ACCENT);
                let mut pane = crate::agent_demo::pane(crate::agent_demo::State::parse("answer").unwrap(), 1);
                pane.notice(&"unbroken".repeat(100));
                pane.input = "line\n".repeat(20);
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 700.0))),
                        ..Default::default()
                    },
                    |ui| {
                        pane.show(ui, "BTC/USD · 1W", egui::Id::new("wrap-test"));
                    },
                );
                output.textures_delta.clear();
                let text = texts(&output);
                for (label, rect) in &text {
                    assert!(rect.right() <= width + 1.0 && rect.left() >= -1.0, "{name} {width}: {label} {rect:?}");
                }
                let disclaimer = text.iter().find(|(s, _)| s == "Not investment advice").unwrap().1;
                assert!(disclaimer.bottom() <= 700.0, "composer offscreen: {disclaimer:?}");
                assert!(!text.iter().any(|(s, _)| s.contains("0 queued") || s == "Retry"));
            }
        }
    }
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
        assert_eq!(pane.state.primary(), "Close");
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
        let settings = Settings {
            model: "m".into(),
            effort: "max".into(),
            tools: ToolsMode::Edit,
            pinned_chart: Some(41),
            response_style: ResponseStyle::Detailed,
        };
        let value = serde_json::to_value(crate::layouts::PaneSettings::Agent(settings.clone())).unwrap();
        assert_eq!(value["Agent"].as_object().unwrap().len(), 5);
        assert_eq!(
            serde_json::from_value::<crate::layouts::PaneSettings>(value).unwrap(),
            crate::layouts::PaneSettings::Agent(settings.clone())
        );
        assert_eq!(settings.tool_set_if_available(false), ToolSet::ReadOnly);
        assert_eq!(settings.tool_set_if_available(true), ToolSet::Workflows);
        assert_eq!(settings.tool_set(), ToolSet::Workflows);
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
        // Nothing was queued or running, so the rate limit had nothing to pause.
        assert!(!pane.queue_paused);
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
        assert_eq!(pane.prepare_session_start(), Some(ToolSet::Workflows));
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
        for label in ["Undo 1 chart change", "Undo last agent turn"] {
            let mut pane = AgentPane { state: Onboarding::Ready, acknowledged: true, ..Default::default() };
            completed_edit(&mut pane, "canonical");
            pane.refresh_undo(&["canonical".into()], Some("canonical"));
            let ctx = egui::Context::default();
            // Let egui settle wrapping/layout before pointing at the button.
            frame(&mut pane, &ctx, vec![]);
            let (_, mut output) = frame(&mut pane, &ctx, vec![]);
            if label == "Undo last agent turn" {
                let menu = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Circle(c) if c.radius > 0.5 && c.radius < 1.5 => Some(c.center),
                        _ => None,
                    })
                    .expect("drawn overflow menu");
                let click_menu = |pressed| egui::Event::PointerButton {
                    pos: menu,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                };
                frame(&mut pane, &ctx, vec![egui::Event::PointerMoved(menu), click_menu(true)]);
                frame(&mut pane, &ctx, vec![click_menu(false)]);
                (_, output) = frame(&mut pane, &ctx, vec![]);
            }
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
            text_position(
                &output,
                &tool_group_duration(
                    "2 changes reverted · 1 kept because you edited them".into(),
                    Some(&pane.turns[0]),
                ),
            );
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

    #[test]
    fn prompt_after_signing_in_from_signed_out_is_sent() {
        let mut pane = AgentPane::default();
        for state in [
            Onboarding::Checking,
            Onboarding::SignedOut,
            Onboarding::SigningIn,
            Onboarding::Checking,
            Onboarding::Ready,
        ] {
            pane.transition(state);
        }
        pane.enqueue(Prompt { text: "What's the trend?".into(), requires_tools: true });
        assert_eq!(pane.next_prompt().map(|p| p.text).as_deref(), Some("What's the trend?"));
    }

    #[test]
    fn stale_paused_prompts_still_wait_for_explicit_resume() {
        let mut pane = AgentPane { state: Onboarding::Ready, ..Default::default() };
        pane.enqueue(Prompt { text: "stale".into(), requires_tools: false });
        pane.interrupt();
        pane.enqueue(Prompt { text: "new".into(), requires_tools: false });
        assert!(pane.next_prompt().is_none(), "a non-empty paused queue must not resume implicitly");
    }
}
