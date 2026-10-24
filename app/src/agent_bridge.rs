//! Registry adapters over the worker and the hidden-window-safe logic queue.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tr_agent::bridge::{AgentBridge, BridgeFuture, BridgeOptions, ChartPin, Connector, ToolCall, ToolExecutor};
use tr_agent_tools::{
    CallBudget, CallContext, DataHost, History, HostFuture, LiveHost, Registry, ToolResult, args::LiveArgs,
};
use tr_core::Timeframe;
use tr_data::{Quote, SearchHit};

use crate::agent_queue::{AgentJob, AgentReply, AgentToolQueue, AgentWaitJob, ChartTarget};
use crate::app::{PaneKind, TerminalApp};
use crate::worker::{AgentRequest, Request};

const UI_TIMEOUT: Duration = Duration::from_secs(10);
const DATA_TIMEOUT: Duration = Duration::from_secs(30);
const VIEW_LOAD_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ViewLoad {
    uid: u64,
    generation: u64,
    symbol: String,
    timeframe: Timeframe,
}

fn loading_timeout(mut view: Value) -> ToolResult {
    view["error"] = json!({
        "code":"data_loading_timeout",
        "message":"Data loading timed out; the timeframe is left switched. Data and the clamped range may finish loading later. Undo this agent turn to restore the previous view."
    });
    view["timeframe_left_switched"] = json!(true);
    let mut result = ToolResult::json(view);
    result.success = false;
    result
}
static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct AppExecutor {
    queue: AgentToolQueue,
    worker: mpsc::UnboundedSender<Request>,
    edits: Arc<Mutex<tr_agent_tools::edits::Session>>,
    expected: Arc<Mutex<HashMap<String, (String, String)>>>,
}

pub const fn agent_edit_tools_available() -> bool {
    true
}

#[derive(Debug, Default, serde::Serialize)]
pub struct UndoSummary {
    pub charts: usize,
    pub reverted: usize,
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalStatus {
    Pending,
    Approved,
    Declined,
    Expired,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Proposal {
    pub id: u64,
    pub turn: String,
    pub args: LiveArgs,
    pub summary: String,
    pub user_owned: bool,
    pub selected: bool,
    pub deadline: Instant,
    pub status: ProposalStatus,
    pub before: Value,
}

impl Proposal {
    pub fn ownership_label(&self) -> Option<&'static str> {
        self.user_owned.then(|| {
            if self.before["agent_created"].as_bool() == Some(true) || self.before["agent_turn"].as_str().is_some() {
                "Created by the agent in an earlier conversation"
            } else {
                "This alert was created by you"
            }
        })
    }
}

#[derive(Debug, Clone)]
pub enum WorkflowUndo {
    Alert { before: Option<tr_core::price_alerts::Alert>, after: Option<tr_core::price_alerts::Alert>, position: usize },
    Watchlist { before: Option<crate::app::NamedWatchlist>, after: Option<crate::app::NamedWatchlist> },
}

#[derive(Default)]
pub struct Workflows {
    pub proposals: Vec<Proposal>,
    pub undo: Vec<(String, WorkflowUndo)>,
    pub context: Vec<tr_agent::codex::WorkflowResult>,
    pub next_id: u64,
}
impl Workflows {
    pub fn expire(&mut self, now: Instant, turn: Option<&str>) {
        for proposal in &mut self.proposals {
            if proposal.status == ProposalStatus::Pending
                && (now >= proposal.deadline || turn.is_some_and(|t| t == proposal.turn))
            {
                proposal.status = ProposalStatus::Expired;
                self.context.push(tr_agent::codex::WorkflowResult::Confirmation {
                    proposal_id: proposal.id,
                    status: tr_agent::codex::WorkflowStatus::Expired,
                    summary: proposal.summary.clone(),
                });
            }
        }
    }
    pub fn reset(&mut self) {
        for proposal in &mut self.proposals {
            if proposal.status == ProposalStatus::Pending {
                proposal.status = ProposalStatus::Expired;
            }
        }
        self.context.clear();
    }
}

impl TerminalApp {
    fn execute_agent_edit(
        &mut self,
        session: &mut tr_agent_tools::edits::Session,
        expected: &mut HashMap<String, (String, String)>,
        call: &ToolCall,
        target: ChartTarget,
        args: LiveArgs,
    ) -> ToolResult {
        use tr_agent_tools::edits::{Chart, error};
        let Some(pinned) = self.agent_chart(ChartTarget::Uid(call.pin.uid)) else {
            return error("chart_not_found", "Pinned chart was closed");
        };
        if self.agent_chart_uid(ChartTarget::Pinned) != Some(call.pin.uid) {
            return error("pin_changed", "Chart pin changed during the turn; start a new turn");
        }
        let (symbol, timeframe) =
            expected.get(&call.turn).cloned().unwrap_or_else(|| (call.pin.symbol.clone(), call.pin.timeframe.clone()));
        if pinned.symbol != symbol || pinned.tf.to_string() != timeframe {
            return error("pin_changed", "Pinned chart symbol or timeframe changed during the turn");
        }
        for pane in &self.panes {
            if let PaneKind::Chart(chart) = &pane.kind {
                session.observe(&Chart {
                    uid: chart.uid,
                    symbol: &chart.symbol,
                    timeframe: chart.tf,
                    bars: chart.source_bars().bars,
                    drawings: &chart.drawings,
                    indicators: &chart.indicators,
                    comparisons: &chart.comparison_objects(),
                });
            }
        }
        let Some(selected) = self.agent_chart(target) else { return error("chart_not_found", "Chart was closed") };
        let uid = selected.uid;
        if matches!(args, LiveArgs::SetView(_)) && uid != call.pin.uid {
            return error("wrong_chart", "set_view only changes the pinned chart");
        }
        let comparisons = selected.comparison_objects();
        let chart = Chart {
            uid,
            symbol: &selected.symbol,
            timeframe: selected.tf,
            bars: selected.source_bars().bars,
            drawings: &selected.drawings,
            indicators: &selected.indicators,
            comparisons: &comparisons,
        };
        let plan = match session.plan(&chart, &call.turn, args) {
            Ok(plan) => plan,
            Err(error) => return error,
        };
        let pane = self
            .panes
            .iter_mut()
            .find_map(|pane| match &mut pane.kind {
                PaneKind::Chart(chart) if chart.uid == uid => Some(chart),
                _ => None,
            })
            .unwrap();
        let mut value = pane.apply_agent_plan(plan, &call.turn, &self.worker);
        if pane.finish_agent_view().is_some() {
            value = pane.agent_view_result();
        }
        if uid == call.pin.uid {
            expected.insert(call.turn.clone(), (pane.symbol.clone(), pane.tf.to_string()));
        }
        self.prune_agent_turns();
        if self.agent_turn_undo_available(&call.turn) && !self.agent_turns.contains(&call.turn) {
            self.agent_turns.push(call.turn.clone());
        }
        self.agent_save_pending = true;
        self.autosave();
        ToolResult::json(value)
    }

    /// Pane-facing availability: provenance survives saves, inverse edits do not.
    pub fn agent_turn_undo_available(&self, turn: &str) -> bool {
        self.agent_pane.workflows.undo.iter().any(|(owner, _)| owner == turn)
            || self.panes.iter().any(|pane| matches!(&pane.kind, PaneKind::Chart(chart) if chart.has_agent_turn(turn)))
    }

    pub fn undoable_agent_turns(&self) -> Vec<&str> {
        self.agent_turns.iter().filter(|turn| self.agent_turn_undo_available(turn)).map(String::as_str).collect()
    }

    pub fn last_agent_turn(&self) -> Option<&str> {
        self.agent_turns.iter().rev().find(|turn| self.agent_turn_undo_available(turn)).map(String::as_str)
    }

    /// The chart menu chooses this chart's latest turn, then undoes the WHOLE turn
    /// on all charts. Its label explicitly discloses that scope.
    pub fn last_agent_turn_for_chart(&self, uid: u64) -> Option<&str> {
        let chart = self.agent_chart(ChartTarget::Uid(uid))?;
        self.agent_turns.iter().rev().find(|turn| chart.has_agent_turn(turn)).map(String::as_str)
    }

    pub(crate) fn prune_agent_turns(&mut self) {
        let available: Vec<_> = self.undoable_agent_turns().into_iter().map(str::to_owned).collect();
        self.agent_turns = available;
    }

    /// Revert by identity, not by restoring a chart snapshot. Conflicting user
    /// changes (including Keep) are reported rather than overwritten.
    pub fn undo_agent_turn(&mut self, turn_id: &str) -> Result<UndoSummary, String> {
        self.agent_pane.workflows.expire(Instant::now(), Some(turn_id));
        self.prune_agent_turns();
        if !self.agent_turns.iter().any(|turn| turn == turn_id) {
            return Err("undo for this agent turn is no longer available (layout changed/app restarted)".into());
        }
        self.observe_edits(true);
        let mut summary = UndoSummary::default();
        for pane in &mut self.panes {
            if let PaneKind::Chart(chart) = &mut pane.kind {
                let drawings = chart.undo_turn(turn_id);
                let (reverted, skipped) = chart.undo_agent_state(turn_id, &self.worker);
                if drawings.reverted + reverted > 0 || !drawings.skipped.is_empty() || !skipped.is_empty() {
                    summary.charts += 1;
                }
                summary.reverted += drawings.reverted + reverted;
                summary.skipped.extend(drawings.skipped.iter().map(|id| id.to_string()));
                summary.skipped.extend(skipped);
            }
        }
        self.undo_workflows(turn_id, &mut summary);
        self.agent_turns.retain(|turn| turn != turn_id);
        self.observe_edits(true);
        self.edit_log.record(
            0,
            tr_chart::drawings::Author::User,
            Some(turn_id.into()),
            "undo_agent_turn",
            json!(summary),
        );
        self.agent_save_pending = true;
        self.autosave();
        Ok(summary)
    }
}
impl AppExecutor {
    pub fn new(app: &TerminalApp) -> Self {
        Self {
            queue: app.agent_queue.clone(),
            worker: app.worker.sender(),
            edits: Default::default(),
            expected: Default::default(),
        }
    }

    pub async fn start(mut self, connector: Arc<dyn Connector>, options: BridgeOptions) -> anyhow::Result<AgentBridge> {
        // A new bridge is a new ownership session, even if the UI reused a clone
        // of the executor. Reconnect/resume within that bridge retains ownership.
        self.edits = Default::default();
        self.expected = Default::default();
        let tools = options.tools;
        AgentBridge::start(connector, Arc::new(self), Registry::new(tools), options).await
    }

    pub fn pin(app: &TerminalApp) -> Option<ChartPin> {
        let chart = app.agent_chart(ChartTarget::Pinned)?;
        Some(ChartPin {
            uid: chart.uid,
            symbol: chart.symbol.clone(),
            timeframe: chart.tf.to_string(),
            visible_range: chart.visible_date_range().map(|range| (range.start, range.end)),
        })
    }
}
impl ToolExecutor for AppExecutor {
    fn execute(&self, registry: Arc<Registry>, call: ToolCall) -> BridgeFuture<'_, ToolResult> {
        Box::pin(async move {
            let host = ScopedHost { app: self.clone(), call };
            let mut budget = CallBudget::new(1);
            let mut ctx = CallContext::new(&host, &host, &mut budget);
            ctx.confirmed = host.call.confirmed;
            registry.call(&host.call.name, &host.call.arguments, &mut ctx).await
        })
    }
    fn cancel(&self, turn: &str) {
        self.queue.cancel(turn);
    }
}

struct PendingJob {
    queue: AgentToolQueue,
    id: String,
}
impl Drop for PendingJob {
    fn drop(&mut self) {
        self.queue.cancel_job(&self.id);
    }
}
struct ScopedHost {
    app: AppExecutor,
    call: ToolCall,
}
impl ScopedHost {
    async fn job(
        &self,
        chart: ChartTarget,
        run: impl FnOnce(&mut TerminalApp) -> AgentReply + Send + 'static,
    ) -> Result<Value, String> {
        let id = format!("agent-{}", NEXT_JOB.fetch_add(1, Ordering::Relaxed));
        let (reply, receive) = oneshot::channel();
        let pending = PendingJob { queue: self.app.queue.clone(), id: id.clone() };
        self.app.queue.push(AgentJob {
            id,
            turn: self.call.turn.clone(),
            chart,
            deadline: Instant::now() + UI_TIMEOUT,
            run: Box::new(run),
            reply,
        });
        let result = tokio::time::timeout(UI_TIMEOUT, receive)
            .await
            .map_err(|_| "UI tool timeout")?
            .map_err(|_| "UI tool cancelled")?;
        drop(pending);
        if result.success { Ok(result.value) } else { Err(result.value.to_string()) }
    }
    async fn wait_view(&self, load: ViewLoad, deadline: Instant) -> ToolResult {
        let id = format!("agent-view-{}", NEXT_JOB.fetch_add(1, Ordering::Relaxed));
        let (reply, receive) = oneshot::channel();
        let pending = PendingJob { queue: self.app.queue.clone(), id: id.clone() };
        let fallback = json!({"uid":load.uid,"symbol":load.symbol,"timeframe":load.timeframe.to_string(),
            "loaded_bars":0,"first_bar_time":null,"last_bar_time":null,"visible_range":null,
            "from_cache":null,"data_source":null,"linked_panes":[]});
        self.app.queue.push_wait(AgentWaitJob {
            id,
            turn: self.call.turn.clone(),
            deadline,
            check: Box::new(move |app, expired| {
                let result = if app.agent_chart_uid(ChartTarget::Pinned) != Some(load.uid) {
                    tr_agent_tools::edits::error("pin_changed", "Chart pin changed while loading data")
                } else if let Some(chart) = app.agent_chart(ChartTarget::Uid(load.uid)) {
                    if chart.agent_view_generation() != load.generation
                        || chart.symbol != load.symbol
                        || chart.tf != load.timeframe
                    {
                        tr_agent_tools::edits::error(
                            "pin_changed",
                            "Chart symbol or timeframe changed while loading data",
                        )
                    } else if !chart.agent_view_pending() && !chart.source_bars().bars.is_empty() {
                        // A response delivered on the deadline tick wins over expiry.
                        ToolResult::json(chart.agent_view_result())
                    } else if let Some(reason) = chart.agent_view_error() {
                        tr_agent::bridge::provider_error(reason)
                    } else if expired {
                        loading_timeout(chart.agent_view_result())
                    } else {
                        return None;
                    }
                } else {
                    tr_agent_tools::edits::error("chart_not_found", "Chart was closed while loading data")
                };
                Some(AgentReply { success: true, value: serde_json::to_value(result).unwrap() })
            }),
            reply,
        });
        let result = tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), receive).await;
        drop(pending);
        match result {
            Ok(Ok(reply)) => serde_json::from_value(reply.value)
                .unwrap_or_else(|_| ToolResult::error("host_error", "Invalid view reply", None)),
            Ok(Err(_)) => {
                ToolResult::error("host_error", "View load cancelled; partial timeframe edit may remain", None)
            }
            Err(_) => loading_timeout(fallback),
        }
    }

    fn target(&self, chart: &Option<String>) -> Result<ChartTarget, String> {
        chart.as_ref().map_or(Ok(ChartTarget::Uid(self.call.pin.uid)), |id| {
            id.parse().map(ChartTarget::Uid).map_err(|_| "chart_id must be a chart uid".into())
        })
    }
    async fn worker<T>(&self, request: Request, receive: oneshot::Receiver<Result<T, String>>) -> Result<T, String> {
        self.app.worker.send(request).map_err(|_| "worker stopped")?;
        tokio::time::timeout(DATA_TIMEOUT, receive)
            .await
            .map_err(|_| "data tool timeout")?
            .map_err(|_| "worker reply cancelled")?
    }
}
impl DataHost for ScopedHost {
    fn history<'a>(
        &'a self,
        symbol: &'a str,
        tf: Timeframe,
        end: Option<i64>,
        limit: usize,
    ) -> HostFuture<'a, History> {
        Box::pin(async move {
            let (expected_symbol, expected_tf) = self
                .app
                .expected
                .lock()
                .unwrap()
                .get(&self.call.turn)
                .cloned()
                .unwrap_or_else(|| (self.call.pin.symbol.clone(), self.call.pin.timeframe.clone()));
            if tr_data::security(symbol).key() == tr_data::security(&expected_symbol).key()
                && tf.to_string() == expected_tf
            {
                let uid = self.call.pin.uid;
                let value = self
                    .job(ChartTarget::Uid(uid), move |app| {
                        let Some(chart) = app.agent_chart(ChartTarget::Uid(uid)) else {
                            return AgentReply::error("chart not found");
                        };
                        if chart.symbol != expected_symbol || chart.tf != tf {
                            return AgentReply::error("pinned chart changed; retry turn");
                        }
                        let bars: Vec<_> = chart
                            .source_bars()
                            .bars
                            .iter()
                            .rev()
                            .filter(|b| end.is_none_or(|end| b.ts < end))
                            .take(limit)
                            .copied()
                            .collect();
                        AgentReply {
                            success: true,
                            value: json!({"bars":bars, "session":chart.read_chart().session,
                            "instrument":chart.source_instrument()}),
                        }
                    })
                    .await?;
                let mut bars: Vec<tr_core::Bar> =
                    serde_json::from_value(value["bars"].clone()).map_err(|e| e.to_string())?;
                bars.reverse();
                if !bars.is_empty() {
                    return Ok(History {
                        bars,
                        session: serde_json::from_value(value["session"].clone()).map_err(|e| e.to_string())?,
                        instrument: serde_json::from_value(value["instrument"].clone()).map_err(|e| e.to_string())?,
                        ..History::default()
                    });
                }
            }
            let (reply, receive) = oneshot::channel();
            self.worker(
                Request::Agent(AgentRequest::ToolHistory {
                    symbol: symbol.into(),
                    tf,
                    end,
                    limit,
                    options: tr_data::HistoryOptions::default(),
                    reply,
                }),
                receive,
            )
            .await
        })
    }
    fn search<'a>(&'a self, query: &'a str) -> HostFuture<'a, Vec<SearchHit>> {
        Box::pin(async move {
            let (reply, receive) = oneshot::channel();
            self.worker(Request::Agent(AgentRequest::Search { query: query.into(), reply }), receive).await
        })
    }
    fn quotes<'a>(&'a self, symbols: &'a [String]) -> HostFuture<'a, Vec<Quote>> {
        Box::pin(async move {
            let (reply, receive) = oneshot::channel();
            self.worker(Request::Agent(AgentRequest::Quotes { symbols: symbols.into(), reply }), receive).await
        })
    }
}
impl LiveHost for ScopedHost {
    fn call_live<'a>(&'a self, _: &'a str, args: LiveArgs) -> BridgeFuture<'a, ToolResult> {
        Box::pin(async move {
            if args.name().contains("watchlist") || args.name().contains("alert") {
                let turn = self.call.turn.clone();
                return match self
                    .job(ChartTarget::Application, move |app| {
                        let result = app.agent_workflow(&turn, args);
                        AgentReply { success: true, value: serde_json::to_value(result).unwrap() }
                    })
                    .await
                {
                    Ok(value) => serde_json::from_value(value)
                        .unwrap_or_else(|_| ToolResult::error("host_error", "Invalid workflow reply", None)),
                    Err(error) => ToolResult::error("host_error", &error, None),
                };
            }
            let chart = match &args {
                LiveArgs::ReadChart(a) => &a.chart_id,
                LiveArgs::IndicatorValues(a) => &a.chart_id,
                LiveArgs::ReadUserActions(a) => &a.chart_id,
                LiveArgs::Screenshot(_) => {
                    return ToolResult::error(
                        "window_hidden",
                        "Screenshot unavailable without a visible window and capture consent flow",
                        None,
                    );
                }
                _ => {
                    let target = match self.target(&tr_agent_tools::edits::chart_id(&args).map(str::to_owned)) {
                        Ok(target) => target,
                        Err(error) => return ToolResult::error("chart_not_found", &error, None),
                    };
                    let edits = self.app.edits.clone();
                    let expected = self.app.expected.clone();
                    let call = self.call.clone();
                    let is_view = matches!(args, LiveArgs::SetView(_));
                    // Absolute call deadline includes the initial UI queue wait.
                    let deadline = Instant::now() + VIEW_LOAD_TIMEOUT;
                    return match self
                        .job(target, move |app| {
                            let result = app.execute_agent_edit(
                                &mut edits.lock().unwrap(),
                                &mut expected.lock().unwrap(),
                                &call,
                                target,
                                args,
                            );
                            let load = app
                                .agent_chart(target)
                                .filter(|chart| is_view && result.success && chart.agent_view_pending())
                                .map(|chart| ViewLoad {
                                    uid: chart.uid,
                                    generation: chart.agent_view_generation(),
                                    symbol: chart.symbol.clone(),
                                    timeframe: chart.tf,
                                });
                            AgentReply { success: true, value: json!({"result":result,"load":load}) }
                        })
                        .await
                    {
                        Ok(value) => {
                            if let Ok(load) = serde_json::from_value::<ViewLoad>(value["load"].clone()) {
                                self.wait_view(load, deadline).await
                            } else {
                                serde_json::from_value(value["result"].clone())
                                    .unwrap_or_else(|_| ToolResult::error("host_error", "Invalid UI reply", None))
                            }
                        }
                        Err(error) => ToolResult::error(
                            if error.contains("chart not found") { "chart_not_found" } else { "host_error" },
                            &error,
                            None,
                        ),
                    };
                }
            };
            let target = match self.target(chart) {
                Ok(target) => target,
                Err(e) => return ToolResult::error("chart_not_found", &e, None),
            };
            let result = self.job(target, move |app| {
                let Some(selected) = app.agent_chart(target) else { return AgentReply::error("chart not found") };
                let uid = selected.uid;
                let value = match args {
                    LiveArgs::ReadChart(_) => {
                        let charts: Vec<_> = app.panes.iter().filter_map(|pane| match &pane.kind {
                            PaneKind::Chart(chart) => Some(json!({"uid":chart.uid,"symbol":chart.symbol,"timeframe":chart.tf,
                                "visible_range":chart.visible_date_range(),"loaded_bars":chart.source_bars().bars.len(),
                                "drawings":chart.drawings,"studies":chart.studies_tail(None, 0),"comparisons":chart.comparison_symbols(),"comparison_provenance":chart.comparison_objects(),
                                "undoable_agent_turns":app.undoable_agent_turns().into_iter().filter(|turn| chart.has_agent_turn(turn)).collect::<Vec<_>>()})),
                            _ => None,
                        }).collect();
                        json!({"pinned_uid":uid,"charts":charts})
                    }
                    LiveArgs::IndicatorValues(args) => {
                        let studies = selected.studies_tail(args.id.as_deref(), 200);
                        json!({"uid":uid,"tail":200,"studies":studies})
                    }
                    LiveArgs::ReadUserActions(args) => json!({"events":app.edit_log.newest(200,true).into_iter()
                        .filter(|event| event.chart == uid && args.since.is_none_or(|since| event.timestamp_ms / 1000 >= since)).collect::<Vec<_>>()}),
                    _ => return AgentReply::error("tool disabled"),
                };
                AgentReply { success: true, value }
            }).await;
            match result {
                Ok(value) => ToolResult::json(value),
                Err(error) => ToolResult::error("host_error", &error, None),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_agent_tools::Content;

    fn host(queue: AgentToolQueue) -> ScopedHost {
        let (worker, _) = mpsc::unbounded_channel();
        ScopedHost {
            app: AppExecutor { queue, worker, edits: Default::default(), expected: Default::default() },
            call: ToolCall {
                turn: "turn".into(),
                pin: ChartPin { uid: 41, symbol: "AAPL".into(), timeframe: "1D".into(), visible_range: None },
                name: "read_chart".into(),
                arguments: "{}".into(),
                confirmed: false,
            },
        }
    }

    #[test]
    fn workflow_reads_and_confirmed_calls_work_without_a_chart_but_never_auto_apply() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/workflow-global-host");
        let mut app = TerminalApp::test_workspace(&root);
        app.panes.clear();
        let executor = AppExecutor::new(&app);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            for (name, arguments, tools) in [
                ("list_watchlists", "{}", tr_agent_tools::ToolSet::ReadOnly),
                ("list_alerts", "{}", tr_agent_tools::ToolSet::ReadOnly),
                ("create_watchlist", r#"{"name":"Proposed"}"#, tr_agent_tools::ToolSet::Workflows),
            ] {
                let registry = Arc::new(Registry::new(tools));
                let call = ToolCall {
                    turn: "global:1".into(),
                    pin: ChartPin { uid: 0, symbol: String::new(), timeframe: String::new(), visible_range: None },
                    name: name.into(),
                    arguments: arguments.into(),
                    confirmed: true,
                };
                let mut result = executor.execute(registry, call);
                assert!(futures::poll!(result.as_mut()).is_pending());
                app.process_logic();
                let result = result.await;
                assert!(result.success, "{name}: {result:?}");
                if name == "create_watchlist" {
                    assert!(
                        matches!(&result.content[0], Content::Text(text) if text.contains("awaiting_user_confirmation"))
                    );
                }
            }
        });
        assert_eq!(app.watchlists.len(), 1, "confirmed=true must not bypass the card");
        app.confirm_workflows("global:1", true);
        assert_eq!(app.watchlists.len(), 2);
    }

    #[tokio::test]
    async fn dropping_executor_future_removes_queued_job_without_ui_drain() {
        let queue = AgentToolQueue::new(egui::Context::default());
        let host = host(queue.clone());
        let mut future = Box::pin(host.job(ChartTarget::Uid(41), |_| panic!("cancelled job must not run")));
        assert!(futures::poll!(future.as_mut()).is_pending());
        assert_eq!(queue.len(), 1);
        drop(future);
        assert_eq!(queue.len(), 0);
    }

    #[tokio::test]
    async fn queued_reply_uses_pinned_uid_deadline_and_oneshot() {
        let queue = AgentToolQueue::new(egui::Context::default());
        let host = host(queue.clone());
        let mut future = Box::pin(host.job(ChartTarget::Uid(41), |_| panic!("only logic executes jobs")));
        assert!(futures::poll!(future.as_mut()).is_pending());
        let job = queue.pop().unwrap();
        assert!(matches!(job.chart, ChartTarget::Uid(41)));
        assert_eq!(job.turn, "turn");
        assert!(job.deadline > Instant::now());
        assert!(job.deadline <= Instant::now() + UI_TIMEOUT);
        job.reply.send(AgentReply { success: true, value: json!({"uid":41}) }).unwrap();
        assert_eq!(future.await.unwrap()["uid"], 41);
    }

    #[tokio::test]
    async fn cancellation_and_hidden_screenshot_fail_without_running_ui_work() {
        let queue = AgentToolQueue::new(egui::Context::default());
        let host = host(queue.clone());
        let mut future = Box::pin(host.job(ChartTarget::Uid(41), |_| panic!("cancelled")));
        assert!(futures::poll!(future.as_mut()).is_pending());
        host.app.cancel("turn");
        assert!(future.await.is_err());
        let result = host
            .call_live("screenshot", LiveArgs::Screenshot(tr_agent_tools::args::Screenshot { chart_id: None }))
            .await;
        assert!(!result.success);
        assert!(matches!(&result.content[0],Content::Text(text) if text.contains("window_hidden")));
        assert_eq!(queue.len(), 0);
    }

    #[tokio::test]
    async fn worker_search_is_request_agent_with_direct_oneshot_reply() {
        let queue = AgentToolQueue::new(egui::Context::default());
        let mut host = host(queue);
        let (worker, mut receive) = mpsc::unbounded_channel();
        host.app.worker = worker;
        let future = host.search("AAPL");
        let server = async {
            let Request::Agent(AgentRequest::Search { query, reply }) = receive.recv().await.unwrap() else {
                panic!("agent search")
            };
            assert_eq!(query, "AAPL");
            reply.send(Ok(vec![])).unwrap();
        };
        let (result, ()) = tokio::join!(future, server);
        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unloaded_history_uses_worker_oneshot_with_source_metadata() {
        let mut host = host(AgentToolQueue::new(egui::Context::default()));
        let (worker, mut receive) = mpsc::unbounded_channel();
        host.app.worker = worker;
        let future = host.history("MSFT", Timeframe::D1, Some(200000), 10);
        let server = async {
            let Request::Agent(AgentRequest::ToolHistory { symbol, limit, reply, .. }) = receive.recv().await.unwrap()
            else {
                panic!("tool history")
            };
            assert_eq!(symbol, "MSFT");
            assert_eq!(limit, 10);
            reply
                .send(Ok(History {
                    bars: vec![tr_core::Bar::new(86400, 1., 2., 0., 1., 10.)],
                    session: tr_core::session::Session::UsRegular,
                    ..Default::default()
                }))
                .unwrap();
        };
        let (result, ()) = tokio::join!(future, server);
        let result = result.unwrap();
        assert_eq!(result.bars.len(), 1);
        assert_eq!(result.session, tr_core::session::Session::UsRegular);
    }

    fn edit_fixture(tag: &str) -> (TerminalApp, AppExecutor, ChartPin) {
        use crate::app::Pane;
        use crate::chart_view::ChartState;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(tag);
        let mut app = TerminalApp::test_workspace(&root);
        let mut chart = ChartState::new(1, "AAA", Timeframe::D1);
        let bars =
            Arc::new((1..=100).map(|n| tr_core::Bar::new(n * 86400, 10., 12., 8., 11., 100.)).collect::<Vec<_>>());
        chart.on_history(0, bars.clone(), tr_chart::scene::SeriesGpu::build(&bars), 0, false, true, false, &app.worker);
        app.panes.push(Pane { id: 1, kind: PaneKind::Chart(Box::new(chart)), link: None, title: None, window: None });
        app.focus = 1;
        app.observe_edits(false);
        let executor = AppExecutor::new(&app);
        let pin = AppExecutor::pin(&app).unwrap();
        (app, executor, pin)
    }

    fn result_value(result: &ToolResult) -> Value {
        let Content::Text(text) = &result.content[0] else { panic!("text result") };
        serde_json::from_str(text).unwrap()
    }

    async fn edit_call(
        app: &mut TerminalApp,
        executor: &AppExecutor,
        pin: &ChartPin,
        turn: &str,
        name: &str,
        arguments: Value,
    ) -> ToolResult {
        let mut future = executor.execute(
            Arc::new(Registry::new(tr_agent_tools::ToolSet::Edit)),
            ToolCall {
                turn: turn.into(),
                pin: pin.clone(),
                name: name.into(),
                arguments: arguments.to_string(),
                confirmed: false,
            },
        );
        if let std::task::Poll::Ready(result) = futures::poll!(future.as_mut()) {
            return result;
        }
        app.process_logic();
        future.await
    }

    fn create_args(kind: &str) -> Value {
        let anchors = if matches!(kind, "trend_line" | "ray" | "rectangle") {
            json!([{"time":86400,"price":10.},{"time":172800,"price":11.}])
        } else {
            json!([{"time":86400,"price":10.}])
        };
        json!({"symbol":"AAA","kind":kind,"anchors":anchors,"label":"support"})
    }

    #[test]
    fn all_edit_tools_record_agent_turn_and_return_ids_in_logic_without_ui() {
        use tr_chart::drawings::Author;
        let (mut app, executor, pin) = edit_fixture("a3-all-edits");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            for kind in ["horizontal", "vertical", "trend_line", "ray", "rectangle", "text"] {
                let result = edit_call(&mut app, &executor, &pin, "all", "create_drawing", create_args(kind)).await;
                assert!(result.success, "{result:?}");
                let id = result_value(&result)["id"].as_u64().unwrap();
                let drawing = app.agent_chart(ChartTarget::Pinned).unwrap().drawings.last().unwrap();
                assert_eq!((drawing.created_by, drawing.turn_id.as_deref()), (Author::Agent, Some("all")));
                assert!(drawing.agent_highlight());
                assert!(
                    edit_call(
                        &mut app,
                        &executor,
                        &pin,
                        "all",
                        "update_drawing",
                        json!({"symbol":"AAA","id":id,"label":"updated"})
                    )
                    .await
                    .success
                );
                assert!(
                    edit_call(&mut app, &executor, &pin, "all", "delete_drawing", json!({"symbol":"AAA","id":id}))
                        .await
                        .success
                );
            }
            let added =
                edit_call(&mut app, &executor, &pin, "all", "add_indicator", json!({"code":"rsi","params":[14]})).await;
            assert!(added.success);
            let id = result_value(&added)["id"].clone();
            let studies = app.agent_chart(ChartTarget::Pinned).unwrap().studies();
            let settings = studies.last().unwrap().spec.chart.as_ref().unwrap();
            assert_eq!((settings.created_by, settings.turn_id.as_deref()), (Author::Agent, Some("all")));
            assert!(studies.last().unwrap().agent_highlight);
            assert!(edit_call(&mut app, &executor, &pin, "all", "remove_indicator", json!({"id":id})).await.success);
            let view = edit_call(&mut app, &executor, &pin, "all", "set_view", json!({"from":0,"to":999999999})).await;
            assert!(view.success);
            assert_eq!(result_value(&view)["from"], 86400);
            assert_eq!(result_value(&view)["to"], 8640000);
            assert_eq!(app.agent_chart(ChartTarget::Pinned).unwrap().source_bars().bars.len(), 100);
            assert!(
                edit_call(&mut app, &executor, &pin, "all", "compare", json!({"symbol":"BBB","action":"add"}))
                    .await
                    .success
            );
            assert!(
                edit_call(&mut app, &executor, &pin, "all", "compare", json!({"symbol":"BBB","action":"remove"}))
                    .await
                    .success
            );
            for kind in [
                "drawing_add",
                "drawing_update",
                "drawing_clear",
                "indicator_add",
                "indicator_remove",
                "range",
                "comparison_add",
                "comparison_remove",
            ] {
                assert!(
                    app.edit_log.newest(200, false).iter().any(|event| event.kind == kind
                        && event.author == Author::Agent
                        && event.turn_id.as_deref() == Some("all")),
                    "missing {kind}"
                );
            }
            assert_eq!(app.last_agent_turn(), Some("all"));
            assert!(agent_edit_tools_available());
        });
    }

    #[test]
    fn stale_other_chart_user_owned_and_changed_pin_fail_structurally() {
        use tr_chart::drawings::{Anchor, Author, Drawing, Tool};
        let (mut app, executor, pin) = edit_fixture("a3-errors");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let result =
                edit_call(&mut app, &executor, &pin, "errors", "delete_drawing", json!({"symbol":"AAA","id":99999999}))
                    .await;
            assert!(!result.success);
            assert_eq!(result_value(&result)["error"]["code"], "stale_id");
            let anchor = Anchor { ts: 86400, price: 10. };
            let user = Drawing::new(Tool::Horizontal, anchor, anchor);
            let user_id = user.id;
            app.agent_chart_mut(ChartTarget::Pinned)
                .unwrap()
                .edit_drawings(Author::User, None, |drawings| drawings.push(user));
            let result = edit_call(
                &mut app,
                &executor,
                &pin,
                "errors",
                "update_drawing",
                json!({"symbol":"AAA","id":user_id,"label":"bad"}),
            )
            .await;
            assert_eq!(result_value(&result)["error"]["code"], "not_owned");
            // Persisted Agent provenance never grants ownership in a new executor.
            let added =
                edit_call(&mut app, &executor, &pin, "errors", "create_drawing", create_args("horizontal")).await;
            let id = result_value(&added)["id"].clone();
            let other_session = AppExecutor::new(&app);
            let result =
                edit_call(&mut app, &other_session, &pin, "errors", "delete_drawing", json!({"symbol":"AAA","id":id}))
                    .await;
            assert_eq!(result_value(&result)["error"]["code"], "not_owned");
            let mut other = crate::chart_view::ChartState::new(2, "AAA", Timeframe::D1);
            let foreign = Drawing::new(Tool::Horizontal, anchor, anchor);
            let foreign_id = foreign.id;
            other.drawings.push(foreign);
            let other_uid = other.uid;
            app.panes.push(crate::app::Pane {
                id: 2,
                kind: PaneKind::Chart(Box::new(other)),
                link: None,
                title: None,
                window: None,
            });
            let result = edit_call(
                &mut app,
                &executor,
                &pin,
                "errors",
                "delete_drawing",
                json!({"symbol":"AAA","id":foreign_id}),
            )
            .await;
            assert_eq!(result_value(&result)["error"]["code"], "wrong_chart");
            for args in [
                json!({"symbol":"AAA","kind":"horizontal","anchors":[{"time":1,"price":10}]}),
                json!({"symbol":"AAA","kind":"horizontal","anchors":[{"time":86400,"price":1e30}]}),
            ] {
                let result = edit_call(&mut app, &executor, &pin, "errors", "create_drawing", args).await;
                assert!(!result.success);
                assert_eq!(result_value(&result)["error"]["code"], "invalid_args");
            }
            let unknown =
                edit_call(&mut app, &executor, &pin, "errors", "add_indicator", json!({"code":"invented"})).await;
            assert_eq!(result_value(&unknown)["error"]["code"], "invalid_args");
            app.pinned_agent_chart = Some(other_uid);
            let result =
                edit_call(&mut app, &executor, &pin, "errors", "create_drawing", create_args("horizontal")).await;
            assert_eq!(result_value(&result)["error"]["code"], "pin_changed");
            app.panes.retain(|p| p.id != 1);
            let result =
                edit_call(&mut app, &executor, &pin, "errors", "create_drawing", create_args("horizontal")).await;
            assert!(!result.success);
            assert_eq!(result_value(&result)["error"]["code"], "chart_not_found");
        });
    }

    #[test]
    fn combined_object_limits_count_creations_not_current_turn_population() {
        let (mut app, executor, pin) = edit_fixture("a3-limits");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut id = Value::Null;
            for n in 0..20 {
                let result = if n % 2 == 0 {
                    edit_call(&mut app, &executor, &pin, "limited", "create_drawing", create_args("horizontal")).await
                } else {
                    edit_call(&mut app, &executor, &pin, "limited", "add_indicator", json!({"code":"RSI"})).await
                };
                assert!(result.success, "{result:?}");
                if n == 0 {
                    id = result_value(&result)["id"].clone();
                }
            }
            assert!(
                edit_call(&mut app, &executor, &pin, "limited", "delete_drawing", json!({"symbol":"AAA","id":id}))
                    .await
                    .success
            );
            let result =
                edit_call(&mut app, &executor, &pin, "limited", "create_drawing", create_args("horizontal")).await;
            assert_eq!(result_value(&result)["error"]["code"], "turn_object_limit");
            for n in 19..100 {
                assert!(
                    edit_call(
                        &mut app,
                        &executor,
                        &pin,
                        &format!("fill-{n}"),
                        "create_drawing",
                        create_args("horizontal")
                    )
                    .await
                    .success
                );
            }
            let result = edit_call(&mut app, &executor, &pin, "overflow", "add_indicator", json!({"code":"SMA"})).await;
            assert_eq!(result_value(&result)["error"]["code"], "chart_object_limit");
        });
    }

    #[test]
    fn undo_agent_turn_keeps_user_interleaving_and_restores_neighbour_order() {
        use tr_chart::drawings::Author;
        let (mut app, executor, pin) = edit_fixture("a3-undo");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut ids = Vec::new();
            for _ in 0..3 {
                let added =
                    edit_call(&mut app, &executor, &pin, "seed", "create_drawing", create_args("horizontal")).await;
                ids.push(result_value(&added)["id"].as_u64().unwrap());
            }
            assert!(
                edit_call(&mut app, &executor, &pin, "undo", "delete_drawing", json!({"symbol":"AAA","id":ids[1]}))
                    .await
                    .success
            );
            let chart = app.agent_chart_mut(ChartTarget::Pinned).unwrap();
            chart.edit_drawings(Author::User, None, |drawings| {
                drawings.remove(0);
                let mut user = drawings[0].clone();
                user.id = tr_chart::drawings::next_id();
                user.text = "user edit during turn".into();
                drawings.push(user);
            });
            let user_id = chart.drawings.last().unwrap().id;
            assert!(
                edit_call(
                    &mut app,
                    &executor,
                    &pin,
                    "undo",
                    "update_drawing",
                    json!({"symbol":"AAA","id":ids[2],"label":"agent change"})
                )
                .await
                .success
            );
            let added = edit_call(&mut app, &executor, &pin, "undo", "add_indicator", json!({"code":"RSI"})).await;
            assert!(added.success);
            let chart = app.agent_chart_mut(ChartTarget::Pinned).unwrap();
            chart.indicators.push(tr_core::indicators::registry::Spec::new("EMA", &[7.]).unwrap());
            tr_core::indicators::study::assign_ids(&mut chart.indicators);
            let report = app.undo_agent_turn("undo").unwrap();
            assert!(report.skipped.is_empty(), "{report:?}");
            assert_eq!(report.reverted, 3);
            let chart = app.agent_chart(ChartTarget::Pinned).unwrap();
            assert_eq!(chart.drawings.iter().map(|d| d.id).collect::<Vec<_>>(), [ids[1], ids[2], user_id]);
            assert_eq!(chart.drawings.last().unwrap().text, "user edit during turn");
            assert!(chart.indicators.iter().any(|s| s.code == "EMA"));
            assert!(!chart.indicators.iter().any(|s| s.code == "RSI"));
            assert!(app.undo_agent_turn("undo").is_err());
            assert_eq!(app.last_agent_turn(), Some("seed"));
        });
    }

    #[test]
    fn keep_transfers_ownership_and_readonly_never_queues_mutations() {
        let (mut app, executor, pin) = edit_fixture("a3-keep-readonly");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let added = edit_call(&mut app, &executor, &pin, "keep", "create_drawing", create_args("text")).await;
            let id = result_value(&added)["id"].as_u64().unwrap();
            app.agent_chart_mut(ChartTarget::Pinned).unwrap().keep_agent_drawing(id);
            let added = edit_call(&mut app, &executor, &pin, "keep", "add_indicator", json!({"code":"RSI"})).await;
            let study_id = result_value(&added)["id"].as_str().unwrap().split_once(':').unwrap().1.parse().unwrap();
            app.agent_chart_mut(ChartTarget::Pinned).unwrap().keep_agent_study(study_id);
            let denied =
                edit_call(&mut app, &executor, &pin, "keep", "delete_drawing", json!({"symbol":"AAA","id":id})).await;
            assert_eq!(result_value(&denied)["error"]["code"], "not_owned");
            let summary = app.undo_agent_turn("keep").unwrap();
            assert_eq!(summary.reverted, 0);
            assert_eq!(summary.skipped.len(), 2);
            let chart = app.agent_chart(ChartTarget::Pinned).unwrap();
            assert!(!chart.drawings[0].agent_highlight());
            assert!(!chart.indicators.last().unwrap().chart.as_ref().unwrap().agent_highlight());
            let result = executor
                .execute(
                    Arc::new(Registry::new(tr_agent_tools::ToolSet::ReadOnly)),
                    ToolCall {
                        turn: "read".into(),
                        pin,
                        name: "create_drawing".into(),
                        arguments: create_args("horizontal").to_string(),
                        confirmed: false,
                    },
                )
                .await;
            assert!(!result.success);
            assert_eq!(result_value(&result)["error"]["code"], "tool_disabled");
            assert_eq!(app.agent_queue.len(), 0);
        });
    }

    struct ViewFixture {
        app: TerminalApp,
        executor: AppExecutor,
        pin: ChartPin,
        requests: mpsc::UnboundedReceiver<Request>,
        out: std::sync::mpsc::Sender<crate::worker::Response>,
        weekly: Arc<Vec<tr_core::Bar>>,
        observed_requests: Vec<Request>,
    }

    fn view_bars(tf: Timeframe) -> Arc<Vec<tr_core::Bar>> {
        let start = if tf == Timeframe::W1 { 1_000_000 } else { 100_000_000 };
        Arc::new((0..1000).map(|n| tr_core::Bar::new(start + n * tf.seconds(), 20., 24., 16., 22., 100.)).collect())
    }

    impl ViewFixture {
        fn new(tag: &str) -> Self {
            let (mut app, _, _) = edit_fixture(tag);
            let (worker, requests) = crate::worker::Worker::recording();
            app.worker = worker;
            let (out, rx) = std::sync::mpsc::channel();
            app.worker.rx = rx;
            if let PaneKind::Chart(chart) = &mut app.panes[0].kind {
                chart.set_timeframe(Timeframe::W1, &app.worker);
                chart.navigation.range = true;
            }
            let weekly = view_bars(Timeframe::W1);
            let mut fixture = Self {
                executor: AppExecutor::new(&app),
                pin: AppExecutor::pin(&app).unwrap(),
                app,
                requests,
                out,
                weekly,
                observed_requests: Vec::new(),
            };
            fixture.deliver(fixture.weekly.clone(), false);
            fixture.app.process_logic();
            fixture.app.agent_chart_mut(ChartTarget::Pinned).unwrap().apply_linked_range(
                crate::chart_navigation::DateRange { start: fixture.weekly[200].ts, end: fixture.weekly[600].ts },
            );
            fixture.pin = AppExecutor::pin(&fixture.app).unwrap();
            fixture.app.observe_edits(false);
            fixture
        }

        fn call(&self, turn: &str, tf: &str) -> ToolCall {
            ToolCall {
                turn: turn.into(),
                pin: self.pin.clone(),
                name: "set_view".into(),
                arguments: json!({"timeframe":tf,"from":0,"to":2_000_000_000_i64}).to_string(),
                confirmed: false,
            }
        }

        fn deliver(&mut self, bars: Arc<Vec<tr_core::Bar>>, from_cache: bool) {
            let (pane, generation, symbol, tf) = loop {
                match self.requests.try_recv().expect("user-path history request") {
                    Request::History { pane, generation, symbol, tf, .. } => break (pane, generation, symbol, tf),
                    request => self.observed_requests.push(request),
                }
            };
            self.out
                .send(crate::worker::Response::History {
                    pane,
                    generation,
                    revision: 1,
                    symbol,
                    tf,
                    gpu: tr_chart::scene::SeriesGpu::build(&bars),
                    bars,
                    prepended: 0,
                    from_cache,
                    exhausted: true,
                    live: false,
                    crypto_source: None,
                })
                .unwrap();
        }

        fn session_history(&mut self) {
            while let Ok(request) = self.requests.try_recv() {
                if let Request::SessionHistory { history } = request {
                    let bars = Arc::new(history.session.filter(&history.source, history.tf));
                    self.out
                        .send(crate::worker::Response::SessionHistory {
                            history,
                            gpu: tr_chart::scene::SeriesGpu::build(&bars),
                            bars,
                        })
                        .unwrap();
                } else {
                    self.observed_requests.push(request);
                }
            }
            self.app.process_logic();
        }

        async fn switch(&mut self, turn: &str, tf: &str, cache: bool) -> ToolResult {
            let executor = self.executor.clone();
            let mut future =
                executor.execute(Arc::new(Registry::new(tr_agent_tools::ToolSet::Edit)), self.call(turn, tf));
            assert!(futures::poll!(future.as_mut()).is_pending());
            self.app.process_logic();
            assert!(futures::poll!(future.as_mut()).is_pending(), "must await the new bars");
            self.deliver(view_bars(tf.parse().unwrap()), cache);
            self.app.process_logic();
            self.session_history();
            future.await
        }
    }

    #[test]
    fn weekly_to_four_hour_waits_for_worker_and_draws_from_new_loaded_bars() {
        let mut f = ViewFixture::new("a3b-loaded-switch");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let before = f.app.agent_chart(ChartTarget::Pinned).unwrap().visible_date_range();
            let result = f.switch("switch", "4H", false).await;
            assert!(result.success, "{result:?}");
            let value = result_value(&result);
            let bars = f.app.agent_chart(ChartTarget::Pinned).unwrap().source_bars().bars.to_vec();
            let last = bars.last().unwrap();
            assert!(!bars.is_empty());
            assert_eq!(value["timeframe"], "4H");
            assert_eq!(value["loaded_bars"], bars.len());
            assert_eq!(value["first_bar_time"], bars[0].ts);
            assert_eq!(value["last_bar_time"], last.ts);
            assert_eq!(value["data_source"], "fresh_load");
            assert_eq!(value["from_cache"], false);
            assert_eq!(value["linked_panes"], json!([]));
            assert!(value["visible_range"]["start"].as_i64().unwrap() >= bars[0].ts);
            assert!(value["visible_range"]["end"].as_i64().unwrap() <= last.ts);
            assert!(f.app.edit_log.newest(200, false).iter().any(|event| event.kind == "timeframe"
                && event.author == tr_chart::drawings::Author::Agent
                && event.turn_id.as_deref() == Some("switch")));
            // Same canonical turn still carries its original weekly pin. read_data
            // must read the newly loaded chart, not independently fetch old bars.
            let read = edit_call(
                &mut f.app,
                &f.executor,
                &f.pin,
                "switch",
                "read_data",
                json!({"symbol":"AAA","timeframe":"4H","limit":2}),
            )
            .await;
            assert!(read.success, "{read:?}");
            assert_eq!(
                result_value(&read)["bars"][1][0],
                chrono::DateTime::from_timestamp(last.ts, 0).unwrap().to_rfc3339()
            );
            let draw = edit_call(
                &mut f.app,
                &f.executor,
                &f.pin,
                "switch",
                "create_drawing",
                json!({"symbol":"AAA","kind":"horizontal","anchors":[{"time":last.ts,"price":last.low}]}),
            )
            .await;
            assert!(draw.success, "{draw:?}");
            // This old weekly timestamp is outside the new H4 history.
            let rejected =
                edit_call(&mut f.app, &f.executor, &f.pin, "switch", "create_drawing", create_args("horizontal")).await;
            assert_eq!(result_value(&rejected)["error"]["code"], "invalid_args");
            f.observed_requests.extend(std::iter::from_fn(|| f.requests.try_recv().ok()));
            assert!(
                f.observed_requests
                    .iter()
                    .any(|request| matches!(request, Request::Indicators { tf: Timeframe::H4, .. }))
            );
            let report = f.app.undo_agent_turn("switch").unwrap();
            assert_eq!(report.reverted, 2);
            assert!(report.skipped.is_empty(), "{report:?}");
            assert_eq!(f.app.agent_chart(ChartTarget::Pinned).unwrap().tf, Timeframe::W1);
            assert!(
                f.app.agent_chart(ChartTarget::Pinned).unwrap().source_bars().bars.is_empty(),
                "undo reloads, never reinstalls a snapshot"
            );
            f.deliver(f.weekly.clone(), false);
            f.app.process_logic();
            assert_eq!(f.app.agent_chart(ChartTarget::Pinned).unwrap().visible_date_range(), before);
        });
    }

    #[test]
    fn switched_view_uses_cache_and_never_moves_a_linked_pane() {
        let mut f = ViewFixture::new("a3b-linked-cache");
        let mut other = crate::chart_view::ChartState::new(2, "AAA", Timeframe::W1);
        other.navigation.range = true;
        other.on_history(
            0,
            f.weekly.clone(),
            tr_chart::scene::SeriesGpu::build(&f.weekly),
            0,
            false,
            true,
            false,
            &f.app.worker,
        );
        let uid = other.uid;
        f.app.panes[0].link = Some(crate::layouts::Link::Blue);
        f.app.panes.push(crate::app::Pane {
            id: 2,
            kind: PaneKind::Chart(Box::new(other)),
            link: Some(crate::layouts::Link::Blue),
            title: None,
            window: None,
        });
        let before = serde_json::to_value(f.app.agent_chart(ChartTarget::Uid(uid)).unwrap().read_chart()).unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let result = f.switch("cache", "4H", true).await;
            assert!(result.success, "{result:?}");
            assert_eq!(result_value(&result)["from_cache"], true);
            assert_eq!(result_value(&result)["data_source"], "cache");
            let after = serde_json::to_value(f.app.agent_chart(ChartTarget::Uid(uid)).unwrap().read_chart()).unwrap();
            assert_eq!(before, after);
            assert!(f.app.agent_chart(ChartTarget::Pinned).unwrap().range_event.is_none());
        });
    }

    #[test]
    fn user_timeframe_or_symbol_change_after_switch_conflicts_with_undo() {
        for symbol in [false, true] {
            let mut f = ViewFixture::new(if symbol { "a3b-symbol-conflict" } else { "a3b-timeframe-conflict" });
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                assert!(f.switch("switch", "4H", false).await.success);
                if let PaneKind::Chart(chart) = &mut f.app.panes[0].kind {
                    if symbol {
                        chart.set_symbol("MSFT", &f.app.worker);
                    } else {
                        chart.set_timeframe(Timeframe::D1, &f.app.worker);
                        chart.set_timeframe(Timeframe::H4, &f.app.worker); // away and back is still a user edit
                    }
                }
                let view = AppExecutor::pin(&f.app).unwrap();
                let report = f.app.undo_agent_turn("switch").unwrap();
                assert_eq!(report.reverted, 0);
                assert_eq!(report.skipped, [format!("view:{}", f.pin.uid)]);
                assert_eq!(
                    serde_json::to_value(AppExecutor::pin(&f.app).unwrap()).unwrap(),
                    serde_json::to_value(view).unwrap()
                );
            });
        }
    }

    #[test]
    fn set_view_timeout_is_structured_and_retains_partial_switch_for_undo() {
        let mut f = ViewFixture::new("a3b-timeout");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut future =
                f.executor.execute(Arc::new(Registry::new(tr_agent_tools::ToolSet::Edit)), f.call("timeout", "4H"));
            assert!(futures::poll!(future.as_mut()).is_pending());
            f.app.process_logic();
            assert!(futures::poll!(future.as_mut()).is_pending());
            f.app.process_logic(); // an idle check must not complete the call
            assert!(futures::poll!(future.as_mut()).is_pending());
            f.app.agent_queue.expire_waits_for_test();
            f.app.process_logic();
            let result = future.await;
            assert!(!result.success);
            assert_eq!(result_value(&result)["error"]["code"], "data_loading_timeout");
            assert_eq!(result_value(&result)["timeframe"], "4H");
            assert_eq!(result_value(&result)["timeframe_left_switched"], true);
            assert!(f.app.agent_turn_undo_available("timeout"));
            // A late worker response still applies the range and refreshes the inverse.
            f.deliver(view_bars(Timeframe::H4), false);
            f.app.process_logic();
            f.session_history();
            let report = f.app.undo_agent_turn("timeout").unwrap();
            assert_eq!(report.reverted, 1);
            assert!(report.skipped.is_empty());
            f.deliver(f.weekly.clone(), false);
            f.app.process_logic();
            assert_eq!(AppExecutor::pin(&f.app).unwrap().visible_range, f.pin.visible_range);
        });
    }

    #[test]
    fn cancelled_pending_switch_can_undo_before_data_and_discards_stale_worker_reply() {
        let mut f = ViewFixture::new("a3b-cancel-load");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let executor = f.executor.clone();
            let mut future =
                executor.execute(Arc::new(Registry::new(tr_agent_tools::ToolSet::Edit)), f.call("cancel", "4H"));
            assert!(futures::poll!(future.as_mut()).is_pending());
            f.app.process_logic();
            assert!(futures::poll!(future.as_mut()).is_pending());
            executor.cancel("cancel");
            assert!(!future.await.success);
            let report = f.app.undo_agent_turn("cancel").unwrap();
            assert_eq!(report.reverted, 1);
            assert!(report.skipped.is_empty());
            // First request is the cancelled H4 generation, second is the undo's W1.
            f.deliver(view_bars(Timeframe::H4), false);
            f.app.process_logic();
            assert!(f.app.agent_chart(ChartTarget::Pinned).unwrap().source_bars().bars.is_empty());
            f.deliver(f.weekly.clone(), false);
            f.app.process_logic();
            assert_eq!(AppExecutor::pin(&f.app).unwrap().visible_range, f.pin.visible_range);
        });
    }

    #[test]
    fn view_deadline_also_completes_without_a_logic_tick() {
        let mut f = ViewFixture::new("a3b-no-tick-timeout");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let executor = f.executor.clone();
            let call = f.call("timeout", "4H");
            let mut future = executor.execute(Arc::new(Registry::new(tr_agent_tools::ToolSet::Edit)), call.clone());
            assert!(futures::poll!(future.as_mut()).is_pending());
            f.app.process_logic();
            drop(future);
            let chart = f.app.agent_chart(ChartTarget::Pinned).unwrap();
            let load = ViewLoad {
                uid: chart.uid,
                generation: chart.agent_view_generation(),
                symbol: chart.symbol.clone(),
                timeframe: chart.tf,
            };
            let host = ScopedHost { app: executor, call };
            let result = host.wait_view(load, Instant::now()).await;
            assert_eq!(result_value(&result)["error"]["code"], "data_loading_timeout");
            assert_eq!(result_value(&result)["timeframe"], "4H");
            assert_eq!(VIEW_LOAD_TIMEOUT, Duration::from_secs(20));
        });
    }

    #[test]
    fn several_timeframe_switches_in_one_turn_undo_to_the_original_range() {
        let mut f = ViewFixture::new("a3b-multiple-switches");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert!(f.switch("switches", "4H", false).await.success);
            assert!(f.switch("switches", "1D", false).await.success);
            let report = f.app.undo_agent_turn("switches").unwrap();
            assert_eq!(report.reverted, 2);
            assert!(report.skipped.is_empty(), "{report:?}");
            f.deliver(view_bars(Timeframe::H4), false); // intermediate inverse
            f.deliver(f.weekly.clone(), false); // final inverse
            f.app.process_logic();
            assert_eq!(AppExecutor::pin(&f.app).unwrap().timeframe, "1W");
            assert_eq!(AppExecutor::pin(&f.app).unwrap().visible_range, f.pin.visible_range);
        });
    }

    #[test]
    fn set_view_readonly_and_symbol_changes_are_still_rejected() {
        let (mut app, executor, pin) = edit_fixture("a3b-rejections");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let result = executor
                .execute(
                    Arc::new(Registry::new(tr_agent_tools::ToolSet::ReadOnly)),
                    ToolCall {
                        turn: "readonly".into(),
                        pin: pin.clone(),
                        name: "set_view".into(),
                        arguments: json!({"timeframe":"4H"}).to_string(),
                        confirmed: false,
                    },
                )
                .await;
            assert_eq!(result_value(&result)["error"]["code"], "tool_disabled");
            assert_eq!(app.agent_queue.len(), 0);
            let result =
                edit_call(&mut app, &executor, &pin, "symbol", "set_view", json!({"symbol":"MSFT","timeframe":"4H"}))
                    .await;
            assert_eq!(result_value(&result)["error"]["code"], "invalid_args");
            assert_eq!(
                serde_json::to_value(AppExecutor::pin(&app).unwrap()).unwrap(),
                serde_json::to_value(pin).unwrap()
            );
        });
    }

    #[test]
    fn range_only_view_preserves_later_user_navigation() {
        let (mut app, executor, pin) = edit_fixture("a3b-range-conflict");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert!(
                edit_call(&mut app, &executor, &pin, "range", "set_view", json!({"from":86400,"to":864000}))
                    .await
                    .success
            );
            app.agent_chart_mut(ChartTarget::Pinned)
                .unwrap()
                .apply_linked_range(crate::chart_navigation::DateRange { start: 864000, end: 4000000 });
            let before = app.agent_chart(ChartTarget::Pinned).unwrap().visible_date_range();
            app.execute("UNDO AGENT");
            assert_eq!(app.agent_chart(ChartTarget::Pinned).unwrap().visible_date_range(), before);
            assert!(app.status.contains("conflicts"));
        });
    }

    #[test]
    fn logic_only_autosave_coalesces_in_flight_agent_edits_and_undo() {
        let (mut app, executor, pin) = edit_fixture("a3-hidden-autosave");
        let initial_studies = app.agent_chart(ChartTarget::Pinned).unwrap().indicators.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let first = std::sync::atomic::AtomicBool::new(true);
        // No filesystem writes: hold the first ordinary autosave in flight until
        // two successful edits arrive through logic, without constructing a Ui.
        app.saves.writer = Arc::new(move |_, _| {
            if first.swap(false, Ordering::SeqCst) {
                started_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
            }
            Ok(())
        });
        app.autosave();
        started_rx.recv().unwrap();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert!(
                edit_call(&mut app, &executor, &pin, "save", "create_drawing", create_args("horizontal")).await.success
            );
            assert!(edit_call(&mut app, &executor, &pin, "save", "add_indicator", json!({"code":"RSI"})).await.success);
        });
        assert!(app.agent_save_pending);
        release_tx.send(()).unwrap();
        app.finish_save();
        eframe::App::logic(&mut app, &egui::Context::default(), &mut eframe::Frame::_new_kittest());
        assert!(!app.agent_save_pending);
        app.finish_save();
        let path = app.store.path(&app.layout_name);
        assert_eq!(app.saves.written[&path], app.current_doc().to_json());
        let saved = String::from_utf8(app.saves.written[&path].clone()).unwrap();
        assert!(saved.contains("Agent") && saved.contains("save"));
        let summary = app.undo_agent_turn("save").unwrap();
        assert_eq!(summary.reverted, 2);
        app.finish_save();
        assert_eq!(app.saves.written[&path], app.current_doc().to_json());
        let chart = app.agent_chart(ChartTarget::Pinned).unwrap();
        assert!(chart.drawings.is_empty());
        assert_eq!(chart.indicators, initial_studies);
        // Once caught up, another logic pass has no save job and no pending poll.
        eframe::App::logic(&mut app, &egui::Context::default(), &mut eframe::Frame::_new_kittest());
        assert!(app.saves.job.is_none());
        assert!(!app.agent_save_pending);
    }

    #[test]
    fn undo_latest_turn_does_not_revert_an_earlier_sessions_same_key() {
        let (mut app, executor, pin) = edit_fixture("a3-reused-turn");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            // Use the same allocator as the bridge. Injecting the identical
            // ToolCall.turn twice would describe ONE turn under the canonical-id
            // contract, not two sessions; bridge-level tests cover real dispatch.
            let first_turn = tr_agent::bridge::TurnIds::new().unwrap().allocate().unwrap();
            let next_turn = tr_agent::bridge::TurnIds::new().unwrap().allocate().unwrap();
            assert_ne!(first_turn, next_turn);
            let first =
                edit_call(&mut app, &executor, &pin, &first_turn, "create_drawing", create_args("horizontal")).await;
            let first = result_value(&first)["id"].as_u64().unwrap();
            let next_session = AppExecutor::new(&app);
            assert!(
                edit_call(&mut app, &next_session, &pin, &next_turn, "create_drawing", create_args("horizontal"))
                    .await
                    .success
            );
            assert_eq!(app.last_agent_turn(), Some(next_turn.as_str()));
            app.execute("UNDO AGENT");
            let left: Vec<_> = app.agent_chart(ChartTarget::Pinned).unwrap().drawings.iter().map(|d| d.id).collect();
            assert_eq!(left, [first], "{}", app.status);
        });
    }

    #[test]
    fn comparison_provenance_blocks_user_readds_and_undo_reports_conflicts_not_noops() {
        let (mut app, executor, pin) = edit_fixture("a3-comparison-conflicts");
        let worker = crate::worker::Worker::inert();
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            for (turn, readd) in [("readded", true), ("missing", false)] {
                assert!(
                    edit_call(&mut app, &executor, &pin, turn, "compare", json!({"symbol":"BBB","action":"add"}))
                        .await
                        .success
                );
                let chart = app.agent_chart_mut(ChartTarget::Pinned).unwrap();
                assert_eq!(chart.comparison_objects()[0].turn_id.as_deref(), Some(turn));
                chart.remove_comparison("BBB US Equity", &worker);
                if readd {
                    chart.add_comparison("BBB US Equity".into(), &worker);
                    let result = edit_call(
                        &mut app,
                        &executor,
                        &pin,
                        "denied",
                        "compare",
                        json!({"symbol":"BBB","action":"remove"}),
                    )
                    .await;
                    assert_eq!(result_value(&result)["error"]["code"], "not_owned");
                }
                let summary = app.undo_agent_turn(turn).unwrap();
                assert_eq!(summary.reverted, 0, "missing removals must not count");
                assert_eq!(summary.skipped.len(), 1);
                assert!(summary.skipped[0].starts_with("comparison:"));
                let chart = app.agent_chart_mut(ChartTarget::Pinned).unwrap();
                if readd {
                    assert_eq!(chart.comparison_objects()[0].created_by, tr_chart::drawings::Author::User);
                    chart.remove_comparison("BBB US Equity", &worker);
                } else {
                    assert!(chart.comparison_symbols().is_empty());
                }
            }
            assert!(
                edit_call(&mut app, &executor, &pin, "owned", "compare", json!({"symbol":"CCC","action":"add"}))
                    .await
                    .success
            );
            assert_eq!(app.undo_agent_turn("owned").unwrap().reverted, 1);
            assert!(app.agent_chart(ChartTarget::Pinned).unwrap().comparison_symbols().is_empty());
            assert!(
                edit_call(&mut app, &executor, &pin, "seed", "compare", json!({"symbol":"CCC","action":"add"}))
                    .await
                    .success
            );
            assert!(
                edit_call(&mut app, &executor, &pin, "removed", "compare", json!({"symbol":"CCC","action":"remove"}))
                    .await
                    .success
            );
            app.agent_chart_mut(ChartTarget::Pinned).unwrap().add_comparison("CCC US Equity".into(), &worker);
            let summary = app.undo_agent_turn("removed").unwrap();
            assert_eq!(summary.reverted, 0);
            assert_eq!(summary.skipped.len(), 1, "restoring a removal must also report user conflicts");
            assert_eq!(
                app.agent_chart(ChartTarget::Pinned).unwrap().comparison_objects()[0].created_by,
                tr_chart::drawings::Author::User
            );
        });
    }

    #[test]
    fn undo_availability_is_lost_on_layout_switch_and_restart_but_provenance_survives() {
        let (mut app, executor, pin) = edit_fixture("a3-unavailable");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert!(
                edit_call(&mut app, &executor, &pin, "before-layout", "create_drawing", create_args("text"))
                    .await
                    .success
            );
            assert!(
                edit_call(
                    &mut app,
                    &executor,
                    &pin,
                    "before-layout",
                    "compare",
                    json!({"symbol":"BBB","action":"add"})
                )
                .await
                .success
            );
        });
        assert!(app.agent_turn_undo_available("before-layout"));
        assert_eq!(app.undoable_agent_turns(), ["before-layout"]);
        let bytes = app.current_doc().to_json();
        let message = "undo for this agent turn is no longer available (layout changed/app restarted)";
        app.apply_doc(crate::layouts::LayoutDoc::from_json(&bytes).unwrap());
        assert!(app.agent_turns.is_empty());
        assert!(app.undoable_agent_turns().is_empty());
        assert_eq!(app.last_agent_turn(), None);
        assert_eq!(app.undo_agent_turn("before-layout").unwrap_err(), message);
        let chart = app.agent_chart(ChartTarget::Pinned).unwrap();
        assert_eq!(chart.drawings[0].turn_id.as_deref(), Some("before-layout"));
        assert_eq!(chart.comparison_objects()[0].turn_id.as_deref(), Some("before-layout"));
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/a3-unavailable-restart");
        let mut restarted = TerminalApp::test_workspace(&root);
        restarted.apply_doc(crate::layouts::LayoutDoc::from_json(&bytes).unwrap());
        assert!(!restarted.agent_turn_undo_available("before-layout"));
        assert!(restarted.undoable_agent_turns().is_empty());
        assert_eq!(restarted.undo_agent_turn("before-layout").unwrap_err(), message);
        let pin = AppExecutor::pin(&restarted).unwrap();
        let new_session = AppExecutor::new(&restarted);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let result = edit_call(
                &mut restarted,
                &new_session,
                &pin,
                "new",
                "compare",
                json!({"symbol":"BBB","action":"remove"}),
            )
            .await;
            assert_eq!(result_value(&result)["error"]["code"], "not_owned");
        });
    }

    #[test]
    fn chart_undo_selects_its_latest_turn_and_explicitly_undoes_the_whole_turn() {
        let (mut app, executor, pin) = edit_fixture("a3-chart-undo");
        let other = crate::chart_view::ChartState::new(2, "AAA", Timeframe::D1);
        let other_uid = other.uid;
        app.panes.push(crate::app::Pane {
            id: 2,
            kind: PaneKind::Chart(Box::new(other)),
            link: None,
            title: None,
            window: None,
        });
        let add = |uid: u64| json!({"code":"RSI","chart_id":uid.to_string()});
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert!(edit_call(&mut app, &executor, &pin, "chart-a", "add_indicator", add(pin.uid)).await.success);
            assert!(edit_call(&mut app, &executor, &pin, "chart-b", "add_indicator", add(other_uid)).await.success);
            assert_eq!(app.last_agent_turn(), Some("chart-b"));
            assert_eq!(app.last_agent_turn_for_chart(pin.uid), Some("chart-a"));
            assert_eq!(app.last_agent_turn_for_chart(other_uid), Some("chart-b"));
            assert!(!app.agent_chart(ChartTarget::Uid(pin.uid)).unwrap().has_agent_turn("chart-b"));
            // Same chart-targeted command emitted by the context menu. Focus is
            // deliberately elsewhere: it must not select the global latest turn.
            app.pinned_agent_chart = Some(pin.uid);
            app.focus = 2;
            app.execute(&format!("UNDO AGENT CHART {}", pin.uid));
            assert_eq!(app.last_agent_turn_for_chart(pin.uid), None);
            assert_eq!(app.last_agent_turn_for_chart(other_uid), Some("chart-b"));
            for uid in [pin.uid, other_uid] {
                assert!(edit_call(&mut app, &executor, &pin, "both", "add_indicator", add(uid)).await.success);
            }
            app.execute(&format!("UNDO AGENT CHART {}", pin.uid));
            assert!(app.status.contains("2 operations"), "{}", app.status);
            assert!(!app.agent_turn_undo_available("both"));
            assert_eq!(app.last_agent_turn_for_chart(other_uid), Some("chart-b"));
        });
    }

    #[test]
    fn huge_drawing_id_in_loaded_layout_does_not_exhaust_study_ids() {
        let (mut app, executor, pin) = edit_fixture("a3-huge-drawing");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert!(
                edit_call(&mut app, &executor, &pin, "drawing", "create_drawing", create_args("horizontal"))
                    .await
                    .success
            );
        });
        let mut doc = app.current_doc();
        for pane in &mut doc.panes {
            if let crate::layouts::PaneSettings::Chart { drawings, .. } = &mut pane.settings {
                drawings[0].id = (u32::MAX as u64) + 1_000_000;
            }
        }
        app.apply_doc(crate::layouts::LayoutDoc::from_json(&doc.to_json()).unwrap());
        assert!(app.agent_chart(ChartTarget::Pinned).unwrap().drawings[0].id > u32::MAX as u64);
        let pin = AppExecutor::pin(&app).unwrap();
        let executor = AppExecutor::new(&app);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let result = edit_call(&mut app, &executor, &pin, "study", "add_indicator", json!({"code":"RSI"})).await;
            assert!(result.success, "{result:?}");
            let id = result_value(&result)["id"].as_str().unwrap().split_once(':').unwrap().1.parse::<u32>().unwrap();
            assert_ne!(id, 0);
        });
    }

    #[test]
    fn set_view_with_explicit_matching_symbol_does_not_panic() {
        let (mut app, executor, pin) = edit_fixture("a3-view-symbol");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let result = edit_call(
                &mut app,
                &executor,
                &pin,
                "sym",
                "set_view",
                json!({"symbol":"aaa us equity","from":86400,"to":864000}),
            )
            .await;
            assert!(result.success, "{result:?}");
        });
    }

    #[test]
    fn provider_failure_completes_view_wait_immediately_and_remains_undoable() {
        let mut f = ViewFixture::new("a3b-provider-error");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let executor = f.executor.clone();
            let mut future =
                executor.execute(Arc::new(Registry::new(tr_agent_tools::ToolSet::Edit)), f.call("error", "4H"));
            assert!(futures::poll!(future.as_mut()).is_pending());
            f.app.process_logic();
            assert!(futures::poll!(future.as_mut()).is_pending());
            let chart = f.app.agent_chart(ChartTarget::Pinned).unwrap();
            let pane = chart.id;
            let generation = chart.agent_view_generation();
            // An error from a superseded request must not fail this wait.
            f.out
                .send(crate::worker::Response::HistoryError {
                    pane,
                    generation: generation - 1,
                    error: "stale provider failure".into(),
                })
                .unwrap();
            f.app.process_logic();
            assert!(futures::poll!(future.as_mut()).is_pending());
            f.out
                .send(crate::worker::Response::HistoryError {
                    pane,
                    generation,
                    error: format!("provider denied: Bearer private-token; {}", "🦀".repeat(400)),
                })
                .unwrap();
            f.app.process_logic();
            let std::task::Poll::Ready(result) = futures::poll!(future.as_mut()) else {
                panic!("provider error must finish on this tick, not wait for expiry")
            };
            assert!(!result.success);
            let value = result_value(&result);
            assert_eq!(value["error"]["code"], "provider_error");
            let message = value["error"]["message"].as_str().unwrap();
            assert!(message.contains("provider denied") && message.contains("[REDACTED]"));
            assert!(!message.contains("private-token"));
            assert!(message.chars().count() <= 300);
            let report = f.app.undo_agent_turn("error").unwrap();
            assert_eq!(report.reverted, 1);
            assert!(report.skipped.is_empty(), "{report:?}");
        });
    }

    #[test]
    fn view_completion_on_expiry_tick_wins_over_timeout() {
        let mut f = ViewFixture::new("a3b-deadline-completion");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let executor = f.executor.clone();
            let mut future =
                executor.execute(Arc::new(Registry::new(tr_agent_tools::ToolSet::Edit)), f.call("deadline", "1D"));
            assert!(futures::poll!(future.as_mut()).is_pending());
            f.app.process_logic();
            assert!(futures::poll!(future.as_mut()).is_pending());
            f.app.agent_queue.expire_waits_for_test();
            f.deliver(view_bars(Timeframe::D1), false);
            f.app.process_logic();
            let std::task::Poll::Ready(result) = futures::poll!(future.as_mut()) else {
                panic!("deadline tick must complete")
            };
            assert!(result.success, "{result:?}");
            assert_eq!(result_value(&result)["loaded_bars"], 1000);
        });
    }

    #[test]
    fn user_navigation_away_and_back_revokes_view_undo_even_for_identical_range() {
        for gesture in ["pan", "zoom", "range", "linked"] {
            let (mut app, executor, pin) = edit_fixture(&format!("a3b-touch-{gesture}"));
            let worker = crate::worker::Worker::inert();
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                assert!(
                    edit_call(&mut app, &executor, &pin, "view", "set_view", json!({"from":864000,"to":4000000}))
                        .await
                        .success
                );
                let chart = app.agent_chart_mut(ChartTarget::Pinned).unwrap();
                let range = chart.visible_date_range().unwrap();
                match gesture {
                    "pan" => {
                        chart.pan_bars(5.);
                        chart.pan_bars(-5.);
                    }
                    "zoom" => {
                        chart.zoom(1.25);
                        chart.zoom(0.8);
                    }
                    "range" => chart.set_range(crate::command::Range::Y1, &worker),
                    _ => chart.apply_linked_range(range),
                }
                // Restore the exact range without adding another user gesture;
                // each entry point above must independently revoke ownership.
                chart.edit_author = tr_chart::drawings::Author::Agent;
                chart.apply_linked_range(range);
                chart.edit_author = tr_chart::drawings::Author::User;
                assert_eq!(chart.visible_date_range(), Some(range));
                let report = app.undo_agent_turn("view").unwrap();
                assert_eq!(report.reverted, 0, "{gesture}: {report:?}");
                assert_eq!(report.skipped, [format!("view:{}", pin.uid)]);
            });
        }
    }

    #[test]
    fn successive_undos_and_a_new_switch_keep_the_buffered_logical_range() {
        let mut f = ViewFixture::new("a3b-buffered-range");
        let original_range = f.pin.visible_range;
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert!(f.switch("t1", "1D", false).await.success);
            f.pin = AppExecutor::pin(&f.app).unwrap();
            let daily_range = f.pin.visible_range;
            assert!(f.switch("t2", "4H", false).await.success);
            assert!(f.app.undo_agent_turn("t2").unwrap().skipped.is_empty());
            // Start another agent range change while the undo reload has no bars.
            f.pin = AppExecutor::pin(&f.app).unwrap();
            let executor = f.executor.clone();
            let mut future =
                executor.execute(Arc::new(Registry::new(tr_agent_tools::ToolSet::Edit)), f.call("t3", "1D"));
            assert!(futures::poll!(future.as_mut()).is_pending());
            f.app.process_logic();
            assert!(futures::poll!(future.as_mut()).is_pending());
            drop(future);
            assert!(f.app.undo_agent_turn("t3").unwrap().skipped.is_empty());
            // t3's inverse must retain t2's buffered daily window, not None.
            f.deliver(view_bars(Timeframe::D1), false);
            f.app.process_logic();
            assert_eq!(AppExecutor::pin(&f.app).unwrap().visible_range, daily_range);
            assert!(f.app.undo_agent_turn("t1").unwrap().skipped.is_empty());
            f.deliver(f.weekly.clone(), false);
            f.app.process_logic();
            assert_eq!(AppExecutor::pin(&f.app).unwrap().timeframe, "1W");
            assert_eq!(AppExecutor::pin(&f.app).unwrap().visible_range, original_range);
        });
    }

    #[test]
    fn undoing_a_newer_agent_view_does_not_erase_an_older_user_conflict() {
        let mut f = ViewFixture::new("a3b-monotonic-user-generation");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert!(f.switch("t1", "1D", false).await.success);
            let chart = f.app.agent_chart_mut(ChartTarget::Pinned).unwrap();
            let range = chart.visible_date_range().unwrap();
            chart.apply_linked_range(range); // Same range, but driven by the user.
            f.pin = AppExecutor::pin(&f.app).unwrap();
            assert!(f.switch("t2", "4H", false).await.success);
            let second = f.app.undo_agent_turn("t2").unwrap();
            assert_eq!(second.reverted, 1);
            assert!(second.skipped.is_empty());
            let first = f.app.undo_agent_turn("t1").unwrap();
            assert_eq!(first.reverted, 0);
            assert_eq!(first.skipped, [format!("view:{}", f.pin.uid)]);
        });
    }

    #[test]
    fn anchored_history_paging_does_not_revoke_view_undo() {
        let mut f = ViewFixture::new("a3b-paging-ownership");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert!(f.switch("paging", "1D", false).await.success);
            let chart = f.app.agent_chart(ChartTarget::Pinned).unwrap();
            let range = chart.visible_date_range();
            let (pane, generation, symbol) = (chart.id, chart.agent_view_generation(), chart.symbol.clone());
            let first = chart.source_bars().bars[0];
            let mut bars: Vec<_> = (1..=10).rev().map(|n| tr_core::Bar { ts: first.ts - n * 86400, ..first }).collect();
            bars.extend_from_slice(chart.source_bars().bars);
            let bars = Arc::new(bars);
            f.out
                .send(crate::worker::Response::History {
                    pane,
                    generation,
                    revision: 2,
                    symbol,
                    tf: Timeframe::D1,
                    gpu: tr_chart::scene::SeriesGpu::build(&bars),
                    bars,
                    prepended: 10,
                    from_cache: false,
                    exhausted: true,
                    live: false,
                    crypto_source: None,
                })
                .unwrap();
            f.app.process_logic();
            assert_eq!(f.app.agent_chart(ChartTarget::Pinned).unwrap().visible_date_range(), range);
            let report = f.app.undo_agent_turn("paging").unwrap();
            assert_eq!(report.reverted, 1);
            assert!(report.skipped.is_empty(), "{report:?}");
        });
    }

    // ---- A3B review tests ----

    #[test]
    fn review_live_bar_after_switch_does_not_revoke_undo() {
        let mut f = ViewFixture::new("a3b-review-live");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert!(f.switch("live", "1D", false).await.success);
            let users_before = f.app.edit_log.newest(200, true).len();
            let (pane, generation, len, last) = {
                let chart = f.app.agent_chart(ChartTarget::Pinned).unwrap();
                let bars = chart.source_bars().bars;
                (chart.id, chart.agent_view_generation(), bars.len(), *bars.last().unwrap())
            };
            let next = tr_core::Bar::new(last.ts + 86400, 20., 24., 16., 22., 100.);
            // Delivered through the worker path, exactly like a provider live tick.
            f.out
                .send(crate::worker::Response::LiveBars {
                    pane,
                    generation,
                    revision: 2,
                    from: len,
                    bars: vec![next],
                    len: len + 1,
                })
                .unwrap();
            f.app.process_logic();
            assert_eq!(f.app.agent_chart(ChartTarget::Pinned).unwrap().source_bars().bars.len(), len + 1);
            // No user action happened; only worker live data (no new User event).
            assert_eq!(f.app.edit_log.newest(200, true).len(), users_before);
            let report = f.app.undo_agent_turn("live").unwrap();
            assert!(report.skipped.is_empty(), "{report:?}");
            assert_eq!(report.reverted, 1);
            assert_eq!(f.app.agent_chart(ChartTarget::Pinned).unwrap().tf, Timeframe::W1);
        });
    }

    #[test]
    fn review_fresh_refresh_after_cached_completion_does_not_revoke_undo() {
        let mut f = ViewFixture::new("a3b-review-cache-fresh");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let result = f.switch("cache", "1D", true).await;
            assert!(result.success, "{result:?}");
            assert_eq!(result_value(&result)["from_cache"], true);
            // The provider refresh for the same generation arrives with one newer bar.
            let (pane, generation, symbol) = {
                let chart = f.app.agent_chart(ChartTarget::Pinned).unwrap();
                (chart.id, chart.agent_view_generation(), chart.symbol.clone())
            };
            let mut fresh = view_bars(Timeframe::D1).to_vec();
            let last = *fresh.last().unwrap();
            fresh.push(tr_core::Bar::new(last.ts + 86400, 20., 24., 16., 22., 100.));
            let fresh = Arc::new(fresh);
            f.out
                .send(crate::worker::Response::History {
                    pane,
                    generation,
                    revision: 2,
                    symbol,
                    tf: Timeframe::D1,
                    gpu: tr_chart::scene::SeriesGpu::build(&fresh),
                    bars: fresh.clone(),
                    prepended: 0,
                    from_cache: false,
                    exhausted: true,
                    live: false,
                    crypto_source: None,
                })
                .unwrap();
            f.app.process_logic();
            assert_eq!(f.app.agent_chart(ChartTarget::Pinned).unwrap().source_bars().bars.len(), fresh.len());
            let report = f.app.undo_agent_turn("cache").unwrap();
            assert!(report.skipped.is_empty(), "{report:?}");
            assert_eq!(report.reverted, 1);
        });
    }

    #[test]
    fn review_successive_turn_undos_before_reload_restore_original_view() {
        let mut f = ViewFixture::new("a3b-review-successive-undo");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert!(f.switch("t1", "1D", false).await.success);
            f.pin = AppExecutor::pin(&f.app).unwrap();
            assert!(f.switch("t2", "4H", false).await.success);
            let second = f.app.undo_agent_turn("t2").unwrap();
            assert_eq!(second.reverted, 1);
            assert!(second.skipped.is_empty(), "{second:?}");
            // The user clicks undo again before the 1D reload has answered.
            let first = f.app.undo_agent_turn("t1").unwrap();
            assert!(first.skipped.is_empty(), "{first:?}");
            assert_eq!(first.reverted, 1);
            assert_eq!(f.app.agent_chart(ChartTarget::Pinned).unwrap().tf, Timeframe::W1);
        });
    }

    #[test]
    fn review_user_timeframe_change_while_loading_fails_wait_and_conflicts_undo() {
        let mut f = ViewFixture::new("a3b-review-user-tf-loading");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let executor = f.executor.clone();
            let mut future =
                executor.execute(Arc::new(Registry::new(tr_agent_tools::ToolSet::Edit)), f.call("load", "4H"));
            assert!(futures::poll!(future.as_mut()).is_pending());
            f.app.process_logic();
            assert!(futures::poll!(future.as_mut()).is_pending());
            if let PaneKind::Chart(chart) = &mut f.app.panes[0].kind {
                chart.set_timeframe(Timeframe::D1, &f.app.worker);
            }
            // The stale 4H response must not satisfy the request.
            f.deliver(view_bars(Timeframe::H4), false);
            f.app.process_logic();
            let result = future.await;
            assert!(!result.success);
            assert_eq!(result_value(&result)["error"]["code"], "pin_changed");
            assert_eq!(f.app.agent_chart(ChartTarget::Pinned).unwrap().tf, Timeframe::D1);
            assert!(f.app.agent_chart(ChartTarget::Pinned).unwrap().source_bars().bars.is_empty());
            let report = f.app.undo_agent_turn("load").unwrap();
            assert_eq!(report.reverted, 0);
            assert_eq!(report.skipped, [format!("view:{}", f.pin.uid)]);
            assert_eq!(f.app.agent_chart(ChartTarget::Pinned).unwrap().tf, Timeframe::D1);
        });
    }

    #[test]
    fn review_parallel_set_views_in_one_turn_do_not_misattribute_and_undo_fully() {
        let mut f = ViewFixture::new("a3b-review-parallel");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let executor = f.executor.clone();
            let registry = Arc::new(Registry::new(tr_agent_tools::ToolSet::Edit));
            let mut a = executor.execute(registry.clone(), f.call("par", "4H"));
            let mut b = executor.execute(registry, f.call("par", "1D"));
            assert!(futures::poll!(a.as_mut()).is_pending());
            assert!(futures::poll!(b.as_mut()).is_pending());
            f.app.process_logic();
            assert!(futures::poll!(b.as_mut()).is_pending());
            // Both requests were sent; deliver both in order. Only the 1D generation is current.
            f.deliver(view_bars(Timeframe::H4), false);
            f.deliver(view_bars(Timeframe::D1), false);
            f.app.process_logic();
            let (a, b) = (a.await, b.await);
            assert!(!a.success, "superseded switch must not claim success: {a:?}");
            assert!(b.success, "{b:?}");
            assert_eq!(result_value(&b)["timeframe"], "1D");
            let report = f.app.undo_agent_turn("par").unwrap();
            assert_eq!(report.reverted, 2);
            assert!(report.skipped.is_empty(), "{report:?}");
            assert_eq!(f.app.agent_chart(ChartTarget::Pinned).unwrap().tf, Timeframe::W1);
        });
    }

    #[test]
    fn review_agent_switch_on_linked_pane_emits_no_range_event_after_render_state() {
        let mut f = ViewFixture::new("a3b-review-link-glide");
        f.app.panes[0].link = Some(crate::layouts::Link::Red);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            assert!(f.switch("link", "1D", false).await.success);
            let chart = f.app.agent_chart(ChartTarget::Pinned).unwrap();
            assert!(chart.range_event.is_none());
            let undo = f.app.undo_agent_turn("link").unwrap();
            assert_eq!(undo.reverted, 1);
            f.deliver(f.weekly.clone(), false);
            f.app.process_logic();
            assert!(f.app.agent_chart(ChartTarget::Pinned).unwrap().range_event.is_none());
        });
    }
}
