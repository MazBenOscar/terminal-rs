//! Manager-run sample-data bridge. Never launched by offline tests.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::json;
use tr_agent::bridge::{
    AgentBridge, BridgeEvent, BridgeFuture, BridgeOptions, BridgeTurn, ChartPin, HostExecutor, IsolatedConnector,
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
}
fn args() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let (mut home, mut model, mut prompt, mut transcript) = (None, None, None, None);
    let mut interrupt = false;
    let mut verify_recovery = false;
    let mut optional_tools = false;
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
            _ => bail!(
                "usage: agent_bridge --home <dir> --model <id> --prompt <text> --transcript <new file> [--optional-tools] [--interrupt-after-first-delta | --verify-recovery]"
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
    data: Arc<MarketDataHost<SampleProvider>>,
    loaded: History,
    pin: ChartPin,
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
            if symbol != self.pin.symbol || tf != Timeframe::D1 {
                return self.data.history(symbol, tf, end, limit).await;
            }
            let mut result = self.loaded.clone();
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
            match args {
                LiveArgs::ReadChart(_) => {
                    ToolResult::json(json!({"charts":[{"uid":self.pin.uid,"symbol":self.pin.symbol,
                    "timeframe":self.pin.timeframe,"source":"sample","loaded_bars":self.loaded.bars.len(),
                    "first_bar":self.loaded.bars.first(),"last_bar":self.loaded.bars.last(),"drawings":[],"comparisons":[]}]}))
                }
                LiveArgs::IndicatorValues(_) => ToolResult::json(
                    json!({"uid":self.pin.uid,"studies":[],"reason":"No studies attached to the headless chart"}),
                ),
                LiveArgs::ReadUserActions(_) => ToolResult::json(json!({"events":[]})),
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
    let options = BridgeOptions::new(home.cwd.to_string_lossy().into_owned());
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
    let host = SampleHost { data: Arc::new(data), loaded, pin: pin.clone() };
    let mut bridge = AgentBridge::start(
        connector,
        Arc::new(HostExecutor { data: host.clone(), live: host }),
        Registry::new(ToolSet::ReadOnly),
        options,
    )
    .await?;
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
                BridgeEvent::Completed { status, calls, interrupt_elapsed, .. } => {
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
mod tests {
    use super::*;
    use tr_agent_tools::{CallBudget, CallContext, Content};

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
}
