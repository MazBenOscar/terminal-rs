//! Registry adapters over the worker and the hidden-window-safe logic queue.
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tr_agent::bridge::{AgentBridge, BridgeFuture, BridgeOptions, ChartPin, Connector, ToolCall, ToolExecutor};
use tr_agent_tools::{
    CallBudget, CallContext, DataHost, History, HostFuture, LiveHost, Registry, ToolResult, ToolSet, args::LiveArgs,
};
use tr_core::Timeframe;
use tr_data::{Quote, SearchHit};

use crate::agent_queue::{AgentJob, AgentReply, AgentToolQueue, ChartTarget};
use crate::app::{PaneKind, TerminalApp};
use crate::worker::{AgentRequest, Request};

const UI_TIMEOUT: Duration = Duration::from_secs(10);
const DATA_TIMEOUT: Duration = Duration::from_secs(30);
static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct AppExecutor {
    queue: AgentToolQueue,
    worker: mpsc::UnboundedSender<Request>,
}
impl AppExecutor {
    pub fn new(app: &TerminalApp) -> Self {
        Self { queue: app.agent_queue.clone(), worker: app.worker.sender() }
    }

    pub async fn start(self, connector: Arc<dyn Connector>, options: BridgeOptions) -> anyhow::Result<AgentBridge> {
        AgentBridge::start(connector, Arc::new(self), Registry::new(ToolSet::ReadOnly), options).await
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
            if tr_data::security(symbol).key() == tr_data::security(&self.call.pin.symbol).key()
                && tf.to_string() == self.call.pin.timeframe
            {
                let uid = self.call.pin.uid;
                let expected_symbol = self.call.pin.symbol.clone();
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
                _ => return ToolResult::error("tool_disabled", "Only read tools are enabled", None),
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
                                "drawings":chart.drawings,"comparisons":chart.comparison_symbols()})),
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
            app: AppExecutor { queue, worker },
            call: ToolCall {
                turn: "turn".into(),
                pin: ChartPin { uid: 41, symbol: "AAPL".into(), timeframe: "1D".into(), visible_range: None },
                name: "read_chart".into(),
                arguments: "{}".into(),
                confirmed: false,
            },
        }
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
}
