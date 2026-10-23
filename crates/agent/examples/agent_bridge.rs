//! Manager-run sample-data bridge. Never launched by offline tests.
use std::path::PathBuf;
use std::sync::Arc;

use std::time::Duration;
use tokio::sync::Mutex;

use anyhow::{Context, Result, bail};
use serde_json::json;
use tr_agent::bridge::{
    AgentBridge, BridgeEvent, BridgeFuture, BridgeOptions, BridgeTurn, ChartPin, IsolatedConnector, ToolCall,
    ToolExecutor,
};
use tr_agent::isolation::{PrivateHome, SessionMode, VersionRange};
use tr_agent::transcript::{StreamScrubber, Transcript, scrub_text, scrub_value};
use tr_agent::transport::WireEvent;
use tr_agent::{AgentBackend, LaunchOptions, TurnInput};
use tr_agent_tools::{
    DataHost, History, HostFuture, LiveHost, MarketDataHost, PriceBasis, Registry, ToolResult, ToolSet, args::LiveArgs,
};
use tr_core::Timeframe;
use tr_data::{Quote, SearchHit, sample::SampleProvider};

struct Args {
    home: PathBuf,
    model: String,
    prompt: String,
    transcript: PathBuf,
    interrupt: bool,
    verify_recovery: bool,
    optional_tools: bool,
    tools: ToolSet,
    undo_agent_turn: bool,
}
fn args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let (mut home, mut model, mut prompt, mut transcript) = (None, None, None, None);
    let mut interrupt = false;
    let mut verify_recovery = false;
    let mut optional_tools = false;
    let mut tools = ToolSet::ReadOnly;
    let mut undo_agent_turn = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--home" => home = Some(PathBuf::from(args.next().context("--home requires a directory")?)),
            "--model" => model = Some(args.next().context("--model requires an id")?),
            "--prompt" => prompt = Some(args.next().context("--prompt requires text")?),
            "--transcript" => {
                transcript = Some(PathBuf::from(args.next().context("--transcript requires a new file")?))
            }
            "--interrupt-after-first-delta" => interrupt = true,
            "--verify-recovery" => verify_recovery = true,
            "--optional-tools" => optional_tools = true,
            "--tools" => {
                tools = match args.next().as_deref() {
                    Some("edit") => ToolSet::Edit,
                    Some("read-only") => ToolSet::ReadOnly,
                    _ => bail!("--tools requires read-only or edit"),
                }
            }
            "--undo-agent-turn" => undo_agent_turn = true,
            _ => bail!(
                "usage: agent_bridge --home <dir> --model <id> --prompt <text> --transcript <new file> [--tools read-only|edit] [--undo-agent-turn] [--optional-tools] [--interrupt-after-first-delta | --verify-recovery]"
            ),
        }
    }
    if interrupt && verify_recovery {
        bail!("interrupt and recovery checks require separate runs")
    }
    Ok(Args {
        home: home.context("--home required")?,
        model: model.context("--model required")?,
        prompt: prompt.context("--prompt required")?,
        transcript: transcript.context("--transcript required")?,
        interrupt,
        verify_recovery,
        optional_tools,
        tools,
        undo_agent_turn,
    })
}
fn find_codex() -> Result<PathBuf> {
    for directory in std::env::split_paths(&std::env::var_os("PATH").context("PATH missing")?) {
        let binary = directory.join("codex");
        if binary.is_file() {
            return Ok(binary);
        }
    }
    bail!("Codex missing from PATH")
}

#[derive(Clone)]
struct SampleHost {
    data: Arc<dyn DataHost>,
    loaded: History,
    pin: ChartPin,
    state: Arc<Mutex<SampleState>>,
}

#[derive(Default)]
struct SampleState {
    session: tr_agent_tools::edits::Session,
    drawings: Vec<tr_chart::drawings::Drawing>,
    indicators: Vec<tr_core::indicators::registry::Spec>,
    comparisons: Vec<tr_agent_tools::edits::Comparison>,
    timeframe: Option<Timeframe>,
    loaded: Option<History>,
    range: Option<(i64, i64)>,
    events: Vec<serde_json::Value>,
    history: Vec<SampleEdit>,
}
struct SampleEdit {
    turn: String,
    before: Vec<tr_chart::drawings::Drawing>,
    after: Vec<tr_chart::drawings::Drawing>,
    studies_before: Vec<tr_core::indicators::registry::Spec>,
    studies_after: Vec<tr_core::indicators::registry::Spec>,
    comparisons_before: Vec<tr_agent_tools::edits::Comparison>,
    comparisons_after: Vec<tr_agent_tools::edits::Comparison>,
    loaded_before: Option<History>,
    timeframe_before: Option<Timeframe>,
    timeframe_after: Option<Timeframe>,
    range_before: Option<(i64, i64)>,
    range_after: Option<(i64, i64)>,
}

impl SampleState {
    fn undo(&mut self, turn: &str) -> Result<usize> {
        let mut reverted = 0;
        let (target, kept): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.history).into_iter().partition(|edit| edit.turn == turn);
        self.history = kept;
        for edit in target.into_iter().rev() {
            // The headless driver has no concurrent user input. Still invert by id
            // and expected value rather than replacing the whole chart.
            for after in &edit.after {
                let before = edit.before.iter().find(|d| d.id == after.id);
                if before == Some(after) {
                    continue;
                }
                if let Some(index) = self.drawings.iter().position(|d| d == after) {
                    reverted += 1;
                    match before {
                        Some(before) => self.drawings[index] = before.clone(),
                        None => {
                            self.drawings.remove(index);
                        }
                    }
                }
            }
            for (index, before) in
                edit.before.iter().enumerate().filter(|(_, d)| !edit.after.iter().any(|a| a.id == d.id))
            {
                if !self.drawings.iter().any(|d| d.id == before.id) {
                    let at = edit.before[index + 1..]
                        .iter()
                        .find_map(|d| self.drawings.iter().position(|n| n.id == d.id))
                        .or_else(|| {
                            edit.before[..index]
                                .iter()
                                .rev()
                                .find_map(|d| self.drawings.iter().position(|n| n.id == d.id).map(|i| i + 1))
                        })
                        .unwrap_or(self.drawings.len());
                    self.drawings.insert(at, before.clone());
                    reverted += 1;
                }
            }
            for after in &edit.studies_after {
                if !edit.studies_before.contains(after)
                    && let Some(index) = self.indicators.iter().position(|s| s == after)
                {
                    self.indicators.remove(index);
                    reverted += 1;
                }
            }
            for (index, before) in edit.studies_before.iter().enumerate() {
                if !edit.studies_after.contains(before)
                    && !self
                        .indicators
                        .iter()
                        .any(|s| s.chart.as_ref().map(|c| c.id) == before.chart.as_ref().map(|c| c.id))
                {
                    self.indicators.insert(index.min(self.indicators.len()), before.clone());
                    reverted += 1;
                }
            }
            for object in edit.comparisons_after.iter().filter(|s| !edit.comparisons_before.contains(s)) {
                if object.created_by == tr_chart::drawings::Author::Agent
                    && object.turn_id.as_deref() == Some(turn)
                    && let Some(index) = self.comparisons.iter().position(|c| c == object)
                {
                    self.comparisons.remove(index);
                    reverted += 1;
                }
            }
            for object in edit.comparisons_before.iter().filter(|s| !edit.comparisons_after.contains(s)) {
                if !self.comparisons.iter().any(|c| c.symbol == object.symbol) {
                    self.comparisons.push(object.clone());
                    reverted += 1;
                }
            }
            if self.timeframe == edit.timeframe_after {
                self.timeframe = edit.timeframe_before;
                self.loaded = edit.loaded_before;
            }
            if self.range == edit.range_after {
                self.range = edit.range_before;
            }
        }
        if reverted == 0 {
            bail!("undo did not revert any objects for agent turn {turn}");
        }
        Ok(reverted)
    }
}

struct SampleExecutor(SampleHost);
impl ToolExecutor for SampleExecutor {
    fn execute(&self, registry: Arc<Registry>, call: ToolCall) -> BridgeFuture<'_, ToolResult> {
        Box::pin(async move {
            let live = TurnHost { host: &self.0, turn: &call.turn };
            let mut budget = tr_agent_tools::CallBudget::new(1);
            let mut ctx = tr_agent_tools::CallContext::new(&self.0, &live, &mut budget);
            ctx.confirmed = call.confirmed;
            registry.call(&call.name, &call.arguments, &mut ctx).await
        })
    }
}
struct TurnHost<'a> {
    host: &'a SampleHost,
    turn: &'a str,
}
impl LiveHost for TurnHost<'_> {
    fn call_live<'a>(&'a self, name: &'a str, args: LiveArgs) -> BridgeFuture<'a, ToolResult> {
        Box::pin(async move {
            use tr_agent_tools::edits::{Chart, Plan};
            if matches!(
                args,
                LiveArgs::ReadChart(_)
                    | LiveArgs::ReadUserActions(_)
                    | LiveArgs::IndicatorValues(_)
                    | LiveArgs::Screenshot(_)
            ) {
                return self.host.call_live(name, args).await;
            }
            if tr_agent_tools::edits::chart_id(&args).is_some_and(|id| id != self.host.pin.uid.to_string()) {
                return ToolResult::error("chart_not_found", "Unknown chart uid", None);
            }
            // One async lock covers planning, loading and application. Parallel
            // calls cannot plan against a stale timeframe or overwrite an inverse;
            // awaiting sample history never blocks an executor thread.
            let mut state = self.host.state.lock().await;
            let loaded_before = state.loaded.clone();
            let before = state.drawings.clone();
            let studies_before = state.indicators.clone();
            let range_before = state.range;
            let comparisons_before = state.comparisons.clone();
            let timeframe_before = state.timeframe;
            let timeframe = state.timeframe.unwrap_or(Timeframe::D1);
            let loaded = state.loaded.as_ref().unwrap_or(&self.host.loaded).clone();
            let bars = &loaded.bars;
            let chart = Chart {
                uid: self.host.pin.uid,
                symbol: &self.host.pin.symbol,
                timeframe,
                bars,
                drawings: &before,
                indicators: &studies_before,
                comparisons: &comparisons_before,
            };
            let plan = match state.session.plan(&chart, self.turn, args) {
                Ok(plan) => plan,
                Err(error) => return error,
            };
            let value = match plan {
                Plan::Drawings(next, id) => {
                    state.drawings = next;
                    json!({"id":id})
                }
                Plan::Indicators(next, id) => {
                    state.indicators = next;
                    json!({"id":id})
                }
                Plan::View { symbol, timeframe, from, to } => {
                    debug_assert_eq!(symbol, self.host.pin.symbol);
                    // Validate/plan before touching the provider. A wrong symbol,
                    // invalid range or unsupported timeframe must never fetch.
                    let fresh = if timeframe != chart.timeframe {
                        match tokio::time::timeout(
                            Duration::from_secs(20),
                            self.host.data.history(&symbol, timeframe, None, 1000),
                        )
                        .await
                        {
                            Ok(Ok(history)) => Some(history),
                            Ok(Err(reason)) => return tr_agent::bridge::provider_error(&reason),
                            Err(_) => {
                                return ToolResult::error(
                                    "data_loading_timeout",
                                    "Sample data loading timed out",
                                    None,
                                );
                            }
                        }
                    } else {
                        None
                    };
                    let from_cache = fresh.is_none();
                    if let Some(history) = fresh {
                        state.loaded = Some(history);
                    }
                    let history = state.loaded.as_ref().unwrap_or(&self.host.loaded);
                    let (Some(first), Some(last)) = (history.bars.first(), history.bars.last()) else {
                        return ToolResult::error("data_loading_timeout", "Sample chart has no bars", None);
                    };
                    let range = (
                        from.unwrap_or(first.ts).clamp(first.ts, last.ts),
                        to.unwrap_or(last.ts).clamp(first.ts, last.ts),
                    );
                    let metadata =
                        json!({"loaded_bars":history.bars.len(),"first_bar_time":first.ts,"last_bar_time":last.ts});
                    state.timeframe = Some(timeframe);
                    state.range = Some(range);
                    json!({"uid":self.host.pin.uid,"symbol":symbol,"timeframe":timeframe,
                        "loaded_bars":metadata["loaded_bars"],"first_bar_time":metadata["first_bar_time"],"last_bar_time":metadata["last_bar_time"],
                        "visible_range":{"start":range.0,"end":range.1},"from_cache":from_cache,
                        "data_source":if from_cache {"cache"} else {"fresh_load"},"linked_panes":[]})
                }
                Plan::Compare(args) => {
                    match args.action {
                        tr_agent_tools::args::CompareAction::Add => {
                            state.comparisons.push(tr_agent_tools::edits::Comparison {
                                symbol: args.symbol.clone(),
                                created_by: tr_chart::drawings::Author::Agent,
                                turn_id: Some(self.turn.into()),
                            })
                        }
                        tr_agent_tools::args::CompareAction::Remove => {
                            state.comparisons.retain(|s| s.symbol != args.symbol)
                        }
                    }
                    json!({"symbol":args.symbol,"comparisons":state.comparisons})
                }
            };
            let after = state.drawings.clone();
            let studies_after = state.indicators.clone();
            let range_after = state.range;
            let comparisons_after = state.comparisons.clone();
            let timeframe_after = state.timeframe;
            state.history.push(SampleEdit {
                turn: self.turn.into(),
                before,
                after,
                studies_before,
                studies_after,
                comparisons_before,
                comparisons_after,
                loaded_before,
                timeframe_before,
                timeframe_after,
                range_before,
                range_after,
            });
            state.events.push(json!({"author":"Agent","turn_id":self.turn,"kind":name,"value":value}));
            ToolResult::json(value)
        })
    }
}

impl DataHost for SampleHost {
    fn history<'a>(
        &'a self,
        symbol: &'a str,
        tf: Timeframe,
        end: Option<i64>,
        limit: usize,
    ) -> HostFuture<'a, History> {
        Box::pin(async move {
            let loaded = {
                let state = self.state.lock().await;
                (symbol == self.pin.symbol && tf == state.timeframe.unwrap_or(Timeframe::D1))
                    .then(|| state.loaded.as_ref().unwrap_or(&self.loaded).clone())
            };
            let Some(mut result) = loaded else {
                return self.data.history(symbol, tf, end, limit).await;
            };
            result.bars.retain(|bar| end.is_none_or(|end| bar.ts < end));
            let skip = result.bars.len().saturating_sub(limit);
            result.bars.drain(..skip);
            Ok(result)
        })
    }
    fn search<'a>(&'a self, query: &'a str) -> HostFuture<'a, Vec<SearchHit>> {
        // Explicit, offline sample catalogue. SampleProvider synthesizes arbitrary
        // histories but its default MarketData::search returns an empty list.
        Box::pin(async move {
            let query = query.trim().to_ascii_lowercase();
            Ok([
                ("AAPL US Equity", "Apple"),
                ("MSFT US Equity", "Microsoft"),
                ("NVDA US Equity", "NVIDIA"),
                ("SPY US Equity", "SPDR S&P 500 ETF"),
            ]
            .into_iter()
            .filter(|(key, name)| {
                key.to_ascii_lowercase().contains(&query) || name.to_ascii_lowercase().contains(&query)
            })
            .map(|(key, name)| SearchHit {
                key: key.into(),
                name: name.into(),
                class: tr_data::security(key).class,
                exchange: Some("sample".into()),
            })
            .collect())
        })
    }
    fn quotes<'a>(&'a self, symbols: &'a [String]) -> HostFuture<'a, Vec<Quote>> {
        self.data.quotes(symbols)
    }
}
impl LiveHost for SampleHost {
    fn call_live<'a>(&'a self, _: &'a str, args: LiveArgs) -> BridgeFuture<'a, ToolResult> {
        Box::pin(async move {
            let chart = match &args {
                LiveArgs::ReadChart(a) => &a.chart_id,
                LiveArgs::IndicatorValues(a) => &a.chart_id,
                LiveArgs::ReadUserActions(a) => &a.chart_id,
                LiveArgs::Screenshot(_) => {
                    return ToolResult::error(
                        "window_hidden",
                        "Headless sample has no visible window or screenshot consent",
                        None,
                    );
                }
                _ => return ToolResult::error("tool_disabled", "Read-only sample host", None),
            };
            if chart.as_ref().is_some_and(|id| id != &self.pin.uid.to_string()) {
                return ToolResult::error("chart_not_found", "Unknown chart uid", Some("chart_id"));
            }
            let state = self.state.lock().await;
            let timeframe = state.timeframe.unwrap_or(Timeframe::D1);
            let bars = &state.loaded.as_ref().unwrap_or(&self.loaded).bars;
            match args {
                LiveArgs::ReadChart(_) => {
                    let studies: Vec<_> = state
                        .indicators
                        .iter()
                        .map(|spec| {
                            json!({
                                "tool_id":format!("{}:{}",self.pin.uid,spec.chart.as_ref().unwrap().id),
                                "spec":spec,
                                "agent_highlight":spec.chart.as_ref().unwrap().agent_highlight()
                            })
                        })
                        .collect();
                    ToolResult::json(json!({"charts":[{"uid":self.pin.uid,"symbol":self.pin.symbol,
                    "timeframe":timeframe,"source":"sample","loaded_bars":bars.len(),
                    "first_bar":bars.first(),"last_bar":bars.last(),"drawings":state.drawings,"studies":studies,"comparisons":state.comparisons,"visible_range":state.range}]}))
                }
                LiveArgs::IndicatorValues(_) => ToolResult::json(
                    json!({"uid":self.pin.uid,"studies":state.indicators.iter().map(|spec| json!({"tool_id":format!("{}:{}",self.pin.uid,spec.chart.as_ref().unwrap().id),"spec":spec,"agent_highlight":spec.chart.as_ref().unwrap().agent_highlight(),"values":spec.compute(&tr_core::indicators::registry::Input::new(bars, timeframe)).iter().map(|output| json!({"name":output.name,"last":output.values.last().filter(|v| v.is_finite())})).collect::<Vec<_>>()})).collect::<Vec<_>>()}),
                ),
                LiveArgs::ReadUserActions(_) => ToolResult::json(
                    json!({"events":state.events.iter().filter(|event| event["author"] == "User").collect::<Vec<_>>()}),
                ),
                _ => unreachable!("read-only live tool"),
            }
        })
    }
}

async fn run() -> Result<()> {
    let args = args()?;
    if std::env::var("TR_PROVIDER").is_ok_and(|value| !value.eq_ignore_ascii_case("sample")) {
        bail!("headless driver supports TR_PROVIDER=sample only");
    }
    let parent = args.home.canonicalize().context("--home must already exist and be app-owned")?;
    let home =
        if parent.join("codex-agent").exists() { PrivateHome::open(&parent)? } else { PrivateHome::create(&parent)? };
    let mut options = BridgeOptions::new(home.cwd.to_string_lossy().into_owned());
    options.tools = args.tools;
    let transcript = Transcript::create(&args.transcript)?;
    let connector = Arc::new(IsolatedConnector {
        launch: LaunchOptions {
            binary: find_codex()?,
            home,
            user_home: std::env::var_os("HOME").context("HOME missing")?.into(),
            versions: VersionRange::default(),
            session_mode: SessionMode::Normal,
        },
        transcript: Some(transcript.clone()),
    });
    let data = MarketDataHost {
        market: SampleProvider::default(),
        price_basis: PriceBasis::Unknown,
        session: Default::default(),
    };
    let loaded = data.history("AAPL US Equity", Timeframe::D1, None, 1000).await.map_err(anyhow::Error::msg)?;
    let pin = ChartPin {
        uid: 1,
        symbol: "AAPL US Equity".into(),
        timeframe: "1D".into(),
        visible_range: loaded.bars.first().zip(loaded.bars.last()).map(|(a, b)| (a.ts, b.ts)),
    };
    let state = Arc::new(Mutex::new(SampleState::default()));
    let host = SampleHost { data: Arc::new(data), loaded, pin: pin.clone(), state: state.clone() };
    let mut bridge =
        AgentBridge::start(connector, Arc::new(SampleExecutor(host)), Registry::new(options.tools), options).await?;
    let result = async {
        let models = bridge.client().list_models().await?;
        let model = models.iter().find(|model| model.model == args.model).context("unknown --model")?;
        bridge
            .send(BridgeTurn {
                input: TurnInput {
                    model: model.model.clone(),
                    effort: model.safe_default_effort().context("no safe effort")?.into(),
                    text: args.prompt,
                    ..Default::default()
                },
                pin: pin.clone(),
                requires_tools: !args.optional_tools,
                screenshot_consent: false,
            })
            .await?;
        let mut interrupted = false;
        let mut recovered = false;
        let mut scrubber = StreamScrubber::default();
        loop {
            match bridge.next_event().await? {
                BridgeEvent::Wire(WireEvent::Notification { method, mut params }) => match method.as_str() {
                    "item/agentMessage/delta" => {
                        print!("{}", scrubber.push(params["delta"].as_str().unwrap_or_default())?);
                        if args.interrupt && !interrupted {
                            bridge.interrupt().await?;
                            interrupted = true;
                        }
                    }
                    "thread/tokenUsage/updated" | "account/rateLimits/updated" => {
                        scrub_value(&mut params);
                        eprintln!("{method}: {params}");
                    }
                    "item/started" if params["item"]["type"] == "dynamicToolCall" => {
                        scrub_value(&mut params);
                        eprintln!("tool: {params}");
                    }
                    _ => {}
                },
                BridgeEvent::Completed { turn, status, calls, interrupt_elapsed } => {
                    let mut state = state.lock().await;
                    println!(
                        "{}",
                        json!({"drawings":state.drawings,"indicators":state.indicators,"comparisons":state.comparisons,"timeframe":state.timeframe.unwrap_or(Timeframe::D1),"range":state.range,"events":state.events})
                    );
                    if args.undo_agent_turn {
                        let reverted = state.undo(&turn)?;
                        eprintln!("undid {turn}: {reverted} object operations");
                        println!(
                            "{}",
                            json!({"undo_agent_turn":turn,"drawings":state.drawings,"indicators":state.indicators,"comparisons":state.comparisons,"timeframe":state.timeframe.unwrap_or(Timeframe::D1),"range":state.range})
                        );
                    }
                    println!("{}", scrubber.finish());
                    eprintln!("status={status} calls={calls}");
                    if args.verify_recovery && !recovered {
                        bail!(
                            "no server death observed; repeat and kill only this driver's child during the long prompt"
                        )
                    }
                    if args.interrupt {
                        let elapsed = interrupt_elapsed.context("no measured mid-stream interrupted completion")?;
                        eprintln!("interrupt to interrupted completion: {:.3} s", elapsed.as_secs_f64());
                        if elapsed >= Duration::from_secs(1) {
                            bail!("interrupt >= 1 s")
                        }
                    } else if status != "completed" || (calls == 0 && !args.optional_tools) {
                        bail!("turn did not complete with chart calls")
                    }
                    return Ok(());
                }
                BridgeEvent::ToolsUnavailable { reason } => bail!("tools unavailable: {reason}"),
                BridgeEvent::Recovered(recovery) => {
                    eprintln!("server restarted: {recovery:?}; original prompt was not replayed");
                    if !args.verify_recovery || recovered {
                        bail!("restart requires an explicit new send")
                    }
                    if matches!(recovery, tr_agent::bridge::Recovery::HistoryLost { .. }) {
                        bail!("history lost; recovery self-test requires a resumed thread")
                    }
                    recovered = true;
                    println!("{}", std::mem::take(&mut scrubber).finish());
                    eprintln!("Sending the requested recovery self-test as a separate turn.");
                    bridge
                        .send(BridgeTurn {
                            input: TurnInput {
                                model: model.model.clone(),
                                effort: model.safe_default_effort().context("no safe effort")?.into(),
                                text: "Recovery self-test: call read_chart and read_data for the pinned chart, \
                                       then report the returned bar range. Do not repeat the interrupted prompt."
                                    .into(),
                                ..Default::default()
                            },
                            pin: pin.clone(),
                            requires_tools: true,
                            screenshot_consent: false,
                        })
                        .await?;
                }
                // Transport errors are consumed by next_event (recover or Err), never forwarded.
                BridgeEvent::Started { turn } => eprintln!("started agent turn {turn}"),
                BridgeEvent::Wire(_) => {}
            }
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(180), result).await;
    bridge.shutdown();
    transcript.flush().await?;
    result?
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{}", scrub_text(&error.to_string()));
        std::process::exit(1);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tr_agent_tools::{CallBudget, CallContext, Content};

    type HistoryRequest = (Timeframe, tokio::sync::oneshot::Sender<Result<History, String>>);

    struct ControlledData(tokio::sync::mpsc::UnboundedSender<HistoryRequest>);
    impl DataHost for ControlledData {
        fn history<'a>(&'a self, _: &'a str, tf: Timeframe, _: Option<i64>, _: usize) -> HostFuture<'a, History> {
            Box::pin(async move {
                let (send, receive) = tokio::sync::oneshot::channel();
                self.0.send((tf, send)).unwrap();
                receive.await.unwrap()
            })
        }
        fn search<'a>(&'a self, _: &'a str) -> HostFuture<'a, Vec<SearchHit>> {
            Box::pin(async { Ok(vec![]) })
        }
        fn quotes<'a>(&'a self, _: &'a [String]) -> HostFuture<'a, Vec<Quote>> {
            Box::pin(async { Ok(vec![]) })
        }
    }

    async fn controlled_host() -> (SampleHost, tokio::sync::mpsc::UnboundedReceiver<HistoryRequest>) {
        let data = MarketDataHost {
            market: SampleProvider::at(1_790_000_000, 1000),
            price_basis: PriceBasis::Unknown,
            session: Default::default(),
        };
        let loaded = data.history("AAPL US Equity", Timeframe::D1, None, 40).await.unwrap();
        let (send, receive) = tokio::sync::mpsc::unbounded_channel();
        (
            SampleHost {
                data: Arc::new(ControlledData(send)),
                loaded,
                pin: ChartPin { uid: 1, symbol: "AAPL US Equity".into(), timeframe: "1D".into(), visible_range: None },
                state: Default::default(),
            },
            receive,
        )
    }

    fn value(result: &ToolResult) -> serde_json::Value {
        let Content::Text(text) = &result.content[0] else { panic!("text") };
        serde_json::from_str(text).unwrap()
    }

    #[tokio::test]
    async fn sample_invalid_view_is_planned_before_fetching_history() {
        let (host, mut requests) = controlled_host().await;
        let live = TurnHost { host: &host, turn: "invalid" };
        for args in [
            json!({"symbol":"MSFT","timeframe":"4H"}),
            json!({"timeframe":"not-a-timeframe"}),
            json!({"timeframe":"4H","from":20,"to":10}),
        ] {
            let result = live.call_live("set_view", LiveArgs::SetView(serde_json::from_value(args).unwrap())).await;
            assert!(!result.success);
            assert_eq!(value(&result)["error"]["code"], "invalid_args");
            assert!(requests.try_recv().is_err(), "invalid views must not fetch history");
        }
        let state = host.state.lock().await;
        assert!(state.history.is_empty());
        assert!(state.loaded.is_none());
    }

    #[tokio::test]
    async fn sample_provider_failure_is_scrubbed_and_not_a_loading_timeout() {
        let (host, mut requests) = controlled_host().await;
        let live = TurnHost { host: &host, turn: "error" };
        let args = LiveArgs::SetView(serde_json::from_value(json!({"timeframe":"4H"})).unwrap());
        let call = live.call_live("set_view", args);
        let provider = async {
            let (tf, reply) = requests.recv().await.unwrap();
            assert_eq!(tf, Timeframe::H4);
            reply.send(Err(format!("provider unavailable: Bearer private-token; {}", "🦀".repeat(400)))).unwrap();
        };
        let (result, ()) = tokio::join!(call, provider);
        assert!(!result.success);
        let result = value(&result);
        assert_eq!(result["error"]["code"], "provider_error");
        let message = result["error"]["message"].as_str().unwrap();
        assert!(message.contains("provider unavailable") && message.contains("[REDACTED]"));
        assert!(!message.contains("private-token"));
        assert!(message.chars().count() <= 300);
        let state = host.state.lock().await;
        assert!(state.history.is_empty());
        assert!(state.loaded.is_none());
    }

    #[tokio::test]
    async fn sample_parallel_views_serialize_planning_loading_and_inverses() {
        let (host, mut requests) = controlled_host().await;
        let first_host = host.clone();
        let first = tokio::spawn(async move {
            TurnHost { host: &first_host, turn: "parallel" }
                .call_live("set_view", LiveArgs::SetView(serde_json::from_value(json!({"timeframe":"1W"})).unwrap()))
                .await
        });
        let (tf, reply) = requests.recv().await.unwrap();
        assert_eq!(tf, Timeframe::W1);
        let second_host = host.clone();
        let second = tokio::spawn(async move {
            TurnHost { host: &second_host, turn: "parallel" }
                .call_live("set_view", LiveArgs::SetView(serde_json::from_value(json!({"timeframe":"4H"})).unwrap()))
                .await
        });
        tokio::task::yield_now().await;
        assert!(requests.try_recv().is_err(), "the second call must wait for the first application");
        reply.send(Ok(host.loaded.clone())).unwrap();
        assert!(first.await.unwrap().success);
        let (tf, reply) = requests.recv().await.unwrap();
        assert_eq!(tf, Timeframe::H4);
        reply.send(Ok(host.loaded.clone())).unwrap();
        assert!(second.await.unwrap().success);
        let mut state = host.state.lock().await;
        assert_eq!(state.history.len(), 2);
        assert_eq!(state.history[1].timeframe_before, Some(Timeframe::W1));
        assert!(state.history[1].loaded_before.is_some());
        assert_eq!(state.timeframe, Some(Timeframe::H4));
        // No object edits were made, but even an all-view undo must restore the
        // chain. The driver's existing no-object report does not affect state.
        let _ = state.undo("parallel");
        assert_eq!(state.timeframe, None);
        assert_eq!(state.range, None);
        assert!(state.loaded.is_none());
    }

    #[tokio::test]
    async fn sample_driver_tools_reference_loaded_bars_without_network_or_window() {
        let data = MarketDataHost {
            market: SampleProvider::at(1_790_000_000, 1000),
            price_basis: PriceBasis::Unknown,
            session: Default::default(),
        };
        let loaded = data.history("AAPL US Equity", Timeframe::D1, None, 40).await.unwrap();
        assert!(!loaded.bars.is_empty());
        let last = loaded.bars.last().unwrap().close;
        let host = SampleHost {
            data: Arc::new(data),
            loaded,
            pin: ChartPin { uid: 1, symbol: "AAPL US Equity".into(), timeframe: "1D".into(), visible_range: None },
            state: Default::default(),
        };
        let registry = Registry::new(ToolSet::ReadOnly);
        let mut budget = CallBudget::default();
        let mut ctx = CallContext::new(&host, &host, &mut budget);
        for (name, args) in [
            ("read_chart", "{}"),
            ("read_data", r#"{"symbol":"AAPL US Equity","limit":10}"#),
            ("analyze", r#"{"symbol":"AAPL US Equity","mode":"trend"}"#),
            ("find_symbol", r#"{"query":"AAPL"}"#),
            ("indicator_values", "{}"),
            ("read_user_actions", "{}"),
        ] {
            let result = registry.call(name, args, &mut ctx).await;
            assert!(result.success, "{result:?}");
            if name == "find_symbol" {
                let Content::Text(text) = &result.content[0] else { panic!("text") };
                assert!(text.contains("AAPL US Equity") && text.contains("Apple"), "{text}");
                assert_eq!(host.search("mIcRoSoFt").await.unwrap()[0].key, "MSFT US Equity");
                assert!(host.search("no-such-symbol").await.unwrap().is_empty());
            }
            if name == "read_data" {
                let Content::Text(text) = &result.content[0] else { panic!("text") };
                let value: serde_json::Value = serde_json::from_str(text).unwrap();
                assert_eq!(value["count"], 10);
                assert_eq!(value["bars"][9][4], last);
            }
        }
        ctx.confirmed = true;
        assert!(!registry.call("screenshot", "{}", &mut ctx).await.success);
    }

    #[tokio::test]
    async fn sample_weekly_to_four_hour_switch_reads_and_draws_the_generated_bars() {
        let data = MarketDataHost {
            market: SampleProvider::at(1_790_000_000, 1000),
            price_basis: PriceBasis::Unknown,
            session: Default::default(),
        };
        let loaded = data.history("AAPL US Equity", Timeframe::W1, None, 1000).await.unwrap();
        let range = loaded.bars.first().zip(loaded.bars.last()).map(|(a, b)| (a.ts, b.ts));
        let host = SampleHost {
            data: Arc::new(data),
            loaded,
            pin: ChartPin { uid: 1, symbol: "AAPL US Equity".into(), timeframe: "1W".into(), visible_range: range },
            state: Arc::new(Mutex::new(SampleState { timeframe: Some(Timeframe::W1), range, ..Default::default() })),
        };
        let live = TurnHost { host: &host, turn: "switch" };
        let registry = Registry::new(ToolSet::Edit);
        let mut budget = CallBudget::default();
        let mut ctx = CallContext::new(&host, &live, &mut budget);
        let switched = registry.call("set_view", r#"{"timeframe":"4H","from":0,"to":2000000000}"#, &mut ctx).await;
        assert!(switched.success, "{switched:?}");
        let Content::Text(text) = &switched.content[0] else { panic!("text") };
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(value["loaded_bars"], 1000);
        assert_eq!(value["from_cache"], false);
        assert_eq!(value["linked_panes"], json!([]));
        let bars = host.history("AAPL US Equity", Timeframe::H4, None, 1000).await.unwrap().bars;
        assert_eq!(value["first_bar_time"], bars[0].ts);
        assert_eq!(value["last_bar_time"], bars[999].ts);
        let read =
            registry.call("read_data", r#"{"symbol":"AAPL US Equity","timeframe":"4H","limit":2}"#, &mut ctx).await;
        assert!(read.success);
        let Content::Text(text) = &read.content[0] else { panic!("text") };
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(value["bars"][1][4], bars[999].close);
        let draw = json!({"symbol":"AAPL US Equity","kind":"horizontal","anchors":[{"time":bars[999].ts,"price":bars[999].low}]});
        assert!(registry.call("create_drawing", &draw.to_string(), &mut ctx).await.success);
        {
            let mut state = host.state.lock().await;
            assert_eq!(state.drawings[0].a.ts, bars[999].ts);
            assert_eq!(state.undo("switch").unwrap(), 1);
            assert_eq!(state.timeframe, Some(Timeframe::W1));
            assert_eq!(state.range, range);
        }
        assert_eq!(host.history("AAPL US Equity", Timeframe::W1, None, 1000).await.unwrap().bars, host.loaded.bars);
    }

    /// Invoked from the library's sealed-connector bridge harness, not with
    /// hand-built ToolCalls. Also compiled in the standalone example test target.
    #[allow(dead_code)]
    pub(crate) async fn sample_edit_bridge_roundtrip(
        make_connector: impl FnOnce(Vec<serde_json::Value>) -> Arc<dyn tr_agent::bridge::Connector>,
    ) {
        let data = MarketDataHost {
            market: SampleProvider::at(1_790_000_000, 1000),
            price_basis: PriceBasis::Unknown,
            session: Default::default(),
        };
        let loaded = data.history("AAPL US Equity", Timeframe::D1, None, 40).await.unwrap();
        let first = loaded.bars.first().unwrap();
        let last = loaded.bars.last().unwrap();
        let anchors = json!([{"time":first.ts,"price":first.close},{"time":last.ts,"price":last.close}]);
        let horizontal =
            json!({"symbol":"AAPL US Equity","kind":"horizontal","anchors":[{"time":first.ts,"price":first.close}]});
        let state = Arc::new(Mutex::new(SampleState::default()));
        let pin = ChartPin { uid: 1, symbol: "AAPL US Equity".into(), timeframe: "1D".into(), visible_range: None };
        let executor =
            SampleExecutor(SampleHost { data: Arc::new(data), loaded, pin: pin.clone(), state: state.clone() });
        let calls = [
            ("create_drawing", horizontal),
            ("create_drawing", json!({"symbol":"AAPL US Equity","kind":"trend_line","anchors":anchors})),
            ("add_indicator", json!({"code":"RSI"})),
            ("set_view", json!({"timeframe":"1W","from":0,"to":2000000000})),
            ("compare", json!({"symbol":"MSFT","action":"add"})),
            ("read_chart", json!({})),
            ("indicator_values", json!({})),
        ]
        .into_iter()
        .enumerate()
        .map(|(id, (name, arguments))| {
            json!({
                "id":900 + id,"method":"item/tool/call","params":{
                    "threadId":"thread","turnId":"turn","callId":format!("call-{id}"),
                    "namespace":"chart","tool":name,"arguments":arguments
                }
            })
        })
        .collect();
        let mut options = BridgeOptions::new("empty".into());
        options.tools = ToolSet::Edit;
        let mut bridge =
            AgentBridge::start(make_connector(calls), Arc::new(executor), Registry::new(ToolSet::Edit), options)
                .await
                .unwrap();
        let sent = bridge
            .send(BridgeTurn {
                input: TurnInput {
                    model: "fake".into(),
                    effort: "low".into(),
                    text: "edit".into(),
                    ..Default::default()
                },
                pin,
                requires_tools: true,
                screenshot_consent: false,
            })
            .await
            .unwrap();
        let completed = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match bridge.next_event().await.unwrap() {
                    BridgeEvent::Completed { turn, calls, .. } => {
                        assert_eq!(calls, 7);
                        break turn;
                    }
                    BridgeEvent::Wire(_) | BridgeEvent::Started { .. } => {}
                    other => panic!("unexpected bridge event: {other:?}"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(sent, completed);
        {
            let state = state.lock().await;
            assert_eq!(state.drawings.len(), 2);
            assert_eq!(state.indicators.len(), 1);
            assert!(state.drawings.iter().all(|d| d.created_by == tr_chart::drawings::Author::Agent
                && d.turn_id.as_deref() == Some(completed.as_str())));
            assert!(state.events.iter().all(|event| event["author"] == "Agent" && event["turn_id"] == completed));
            let settings = state.indicators[0].chart.as_ref().unwrap();
            assert_eq!(settings.created_by, tr_chart::drawings::Author::Agent);
            assert_eq!(settings.turn_id.as_deref(), Some(completed.as_str()));
        }
        {
            let mut state = state.lock().await;
            assert_eq!(state.comparisons[0].turn_id.as_deref(), Some(completed.as_str()));
            assert!(state.undo("unknown-turn").is_err());
            assert_eq!(state.undo(&completed).unwrap(), 4);
            assert!(state.undo(&completed).is_err(), "a no-op must fail the driver");
        }
        let state = state.lock().await;
        assert!(state.drawings.is_empty());
        assert!(state.indicators.is_empty());
        assert_eq!(state.range, None);
        assert_eq!(state.timeframe, None);
    }
}
