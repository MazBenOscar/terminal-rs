//! Background owner for login, bridge RPCs and event-driven UI wakeups.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::sync::{mpsc, watch};
use tr_agent::bridge::{AgentBridge, BridgeEvent, BridgeOptions, BridgeTurn, Connector, IsolatedConnector};
use tr_agent::codex::{AccountState, Login, Model};
use tr_agent::isolation::{PrivateHome, SessionMode, VersionRange, WrongVersion};
use tr_agent::transcript::scrub_text;
use tr_agent::{AgentBackend, CodexAppServer, LaunchOptions};

use crate::agent_bridge::AppExecutor;
use crate::agent_pane::{Onboarding, STREAM_INTERVAL};

pub enum Command {
    SignIn,
    CancelLogin,
    ReopenLogin,
    RefreshLimits,
    SelectModel(String),
    Turn(Box<BridgeTurn>),
    Analysis { id: crate::agent_pane::PromptId, input: tr_agent::TurnInput },
    WorkflowResults(Vec<tr_agent::codex::WorkflowResult>),
    Interrupt,
}
// The URL is deliberately neither Debug nor serializable and is never stored in pane state.
pub enum Event {
    State(Onboarding),
    LoginUrl(String),
    Models(Vec<Model>),
    Limits(serde_json::Value),
    Bridge(BridgeEvent),
    Analysis { id: crate::agent_pane::PromptId, result: std::result::Result<String, tr_agent::AnalysisError> },
    Notice(String),
}
struct Inbox {
    events: Mutex<Vec<Event>>,
    visible: AtomicBool,
    ctx: egui::Context,
    last_text_wake: Mutex<Option<Instant>>,
}
impl Inbox {
    fn push(&self, event: Event) {
        let text = matches!(&event, Event::Bridge(BridgeEvent::Wire(tr_agent::transport::WireEvent::Notification { method, .. })) if method == "item/agentMessage/delta");
        let mut events = self.events.lock().unwrap();
        if text
            && let Some(Event::Bridge(BridgeEvent::Wire(tr_agent::transport::WireEvent::Notification {
                method,
                params,
            }))) = events.last_mut()
            && method == "item/agentMessage/delta"
            && let Event::Bridge(BridgeEvent::Wire(tr_agent::transport::WireEvent::Notification {
                params: next, ..
            })) = &event
            && params["itemId"] == next["itemId"]
            && params["turnId"] == next["turnId"]
            && params["threadId"] == next["threadId"]
        {
            let delta = next["delta"].as_str().unwrap_or_default();
            if let Some(serde_json::Value::String(joined)) = params.get_mut("delta") {
                joined.push_str(delta);
            } else {
                params["delta"] = delta.into();
            }
        } else {
            events.push(event);
        }
        drop(events);
        if !self.visible.load(Ordering::Relaxed) {
            return;
        }
        if text {
            let now = Instant::now();
            let mut last = self.last_text_wake.lock().unwrap();
            let delay =
                last.map_or(Duration::ZERO, |last| STREAM_INTERVAL.saturating_sub(now.saturating_duration_since(last)));
            if delay.is_zero() {
                *last = Some(now);
                self.ctx.request_repaint();
            } else {
                self.ctx.request_repaint_after(delay);
            }
        } else {
            self.ctx.request_repaint();
        }
    }
    fn state(&self, state: Onboarding) {
        self.push(Event::State(state));
    }
    fn limits(&self, value: &serde_json::Value) {
        self.push(Event::Limits(visible_limits(value)));
    }
}

pub(crate) fn visible_limits(value: &serde_json::Value) -> serde_json::Value {
    // Keep only limits/availability, never account fields, credits or login data.
    // The pane evaluates these against its currently selected model.
    let mut visible = serde_json::json!({"rateLimits": visible_snapshot(&value["rateLimits"])});
    if let Some(map) = value["rateLimitsByLimitId"].as_object() {
        visible["rateLimitsByLimitId"] =
            map.iter().map(|(id, snapshot)| (id.clone(), visible_snapshot(snapshot))).collect();
    }
    if let Some(allowed) = value["ordinaryUsageAllowed"].as_bool() {
        visible["ordinaryUsageAllowed"] = allowed.into();
    }
    visible
}

fn visible_snapshot(snapshot: &serde_json::Value) -> serde_json::Value {
    let mut visible = serde_json::json!({});
    for key in ["primary", "secondary"] {
        for field in ["usedPercent", "resetsAt", "windowDurationMins"] {
            if let Some(number) = snapshot[key][field].as_number() {
                visible[key][field] = number.clone().into();
            }
        }
    }
    for key in ["limitId", "normalModelSlug"] {
        if let Some(text) = snapshot[key].as_str() {
            visible[key] = text.into();
        }
    }
    visible
}

// A replacement owner may wait here off the UI thread while a detached owner
// finishes cleanup. Never open the same private home concurrently on Restart.
static OWNER_LOCK: Mutex<()> = Mutex::new(());

pub struct Session {
    sender: Option<mpsc::UnboundedSender<Command>>,
    inbox: Arc<Inbox>,
    shutdown: watch::Sender<()>,
    finished: std::sync::mpsc::Receiver<()>,
    owner: Option<std::thread::JoinHandle<()>>,
}
fn finish_owner(owner: std::thread::JoinHandle<()>, finished: &std::sync::mpsc::Receiver<()>, timeout: Duration) {
    if finished.recv_timeout(timeout).is_ok() || owner.is_finished() {
        let _ = owner.join();
    }
    // Otherwise detach, not abort: the owner retains its runtime and completes
    // shutdown/reaping/private-home checks. Quit trades waiting for that report
    // for bounded UI latency; process exit may precede the final cleanup report.
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.shutdown.send(());
        self.sender.take();
        if let Some(owner) = self.owner.take() {
            finish_owner(owner, &self.finished, Duration::from_millis(1500));
        }
    }
}
impl Session {
    /// Command-only fixture: never creates a runtime, process, or private home.
    #[cfg(test)]
    pub(crate) fn test_channel() -> (Self, mpsc::UnboundedReceiver<Command>) {
        let (sender, receive) = mpsc::unbounded_channel();
        let (shutdown, _) = watch::channel(());
        let (_, finished) = std::sync::mpsc::channel();
        (
            Self {
                sender: Some(sender),
                inbox: Arc::new(Inbox {
                    events: Mutex::new(Vec::new()),
                    visible: AtomicBool::new(true),
                    ctx: egui::Context::default(),
                    last_text_wake: Mutex::new(None),
                }),
                shutdown,
                finished,
                owner: None,
            },
            receive,
        )
    }

    pub fn start(
        ctx: egui::Context,
        executor: AppExecutor,
        parent: PathBuf,
        tools: tr_agent_tools::ToolSet,
        config: crate::agent_mcp::Settings,
        mimo_model: String,
    ) -> Self {
        let (sender, receive) = mpsc::unbounded_channel();
        let (shutdown, stopping) = watch::channel(());
        let (finished, completion) = std::sync::mpsc::channel();
        let inbox = Arc::new(Inbox {
            events: Mutex::new(Vec::new()),
            visible: AtomicBool::new(true),
            ctx,
            last_text_wake: Mutex::new(None),
        });
        let background = inbox.clone();
        let owner = std::thread::spawn(move || {
            let _owner = OWNER_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let run = || -> Result<()> {
                if stopping.has_changed().unwrap_or(true) {
                    return Ok(());
                }
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                if config.backend == crate::agent_mcp::Backend::Mimo {
                    runtime.block_on(owner_acp(receive, stopping, &background, parent, tools, config, mimo_model))
                } else {
                    runtime.block_on(owner(receive, stopping, &background, executor, parent, tools))
                }
            };
            if let Err(error) = run() {
                background.state(failure_state(error));
            }
            let _ = finished.send(());
        });
        Self { sender: Some(sender), inbox, shutdown, finished: completion, owner: Some(owner) }
    }
    pub fn send(&self, command: Command) -> bool {
        self.sender.as_ref().is_some_and(|sender| sender.send(command).is_ok())
    }
    pub fn drain(&self, visible: bool) -> Vec<Event> {
        self.inbox.visible.store(visible, Ordering::Relaxed);
        std::mem::take(&mut *self.inbox.events.lock().unwrap())
    }
}

fn find_binary(search: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    search.into_iter().map(|directory| directory.join("codex")).find_map(|path| {
        let path = path.canonicalize().ok()?;
        let metadata = path.metadata().ok()?;
        if !metadata.is_file() {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return None;
            }
        }
        Some(path)
    })
}
fn binary() -> Option<PathBuf> {
    // Finder does not inherit an interactive shell PATH. Never source profiles.
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut search: Vec<_> = std::env::split_paths(&path).collect();
    search.extend([PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")]);
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        search.extend([home.join(".local/bin"), home.join(".npm-global/bin")]);
    }
    find_binary(search)
}
fn connector(parent: &std::path::Path, binary: PathBuf, mode: SessionMode) -> Result<Arc<IsolatedConnector>> {
    let home =
        if parent.join("codex-agent").exists() { PrivateHome::open(parent)? } else { PrivateHome::create(parent)? };
    Ok(Arc::new(IsolatedConnector {
        launch: LaunchOptions {
            binary,
            home,
            user_home: std::env::var_os("HOME").map(PathBuf::from).context("HOME is unavailable")?,
            versions: VersionRange::default(),
            session_mode: mode,
        },
        transcript: None,
    }))
}
async fn cleanup(client: &CodexAppServer) -> Result<()> {
    client.shutdown();
    tokio::time::timeout(Duration::from_secs(10), client.wait_cleanup())
        .await
        .context("agent cleanup timed out; restart the app")??;
    Ok(())
}
async fn onboarding(
    receive: &mut mpsc::UnboundedReceiver<Command>,
    inbox: &Inbox,
    client: &CodexAppServer,
) -> Result<bool> {
    let mut login: Option<Login> = None;
    loop {
        if login.is_none() && client.read_account().await? == AccountState::ChatGpt {
            let value = client.rate_limits().await?;
            inbox.limits(&value);
            if let Some(reset) = crate::agent_pane::blocking_reset(&value, "") {
                inbox.state(Onboarding::RateLimited { reset });
            } else {
                return Ok(true);
            }
        } else if login.is_none() {
            inbox.state(Onboarding::SignedOut);
        }
        let command = if let Some(pending) = login.as_mut() {
            tokio::select! {
                result = client.login_wait_for_completion(pending) => {
                    login = None;
                    match result {
                        Ok(()) => inbox.state(Onboarding::Checking),
                        Err(error) => {
                            if client.is_unsafe() { return Err(error); }
                            inbox.push(Event::Notice(scrub_text(&error.to_string())));
                            inbox.state(Onboarding::SignedOut);
                        }
                    }
                    continue;
                }
                command = receive.recv() => command,
            }
        } else {
            receive.recv().await
        };
        match command {
            Some(Command::SignIn) if login.is_none() => match client.login_start().await {
                Ok(pending) => {
                    inbox.state(Onboarding::SigningIn);
                    inbox.push(Event::LoginUrl(pending.auth_url.clone()));
                    login = Some(pending);
                }
                Err(error) => return Err(error),
            },
            Some(Command::CancelLogin) => {
                if let Some(pending) = login.take()
                    && let Err(error) = client.login_cancel(&pending).await
                {
                    // Completion may have already removed the login id. Account
                    // state, not the cancel RPC, is authoritative in that race.
                    if client.read_account().await? == AccountState::ChatGpt {
                        inbox.state(Onboarding::Checking);
                    } else {
                        inbox.push(Event::Notice(scrub_text(&error.to_string())));
                        inbox.state(Onboarding::SignedOut);
                    }
                } else {
                    inbox.state(Onboarding::SignedOut);
                }
            }
            Some(Command::ReopenLogin) => {
                if let Some(pending) = &login {
                    inbox.push(Event::LoginUrl(pending.auth_url.clone()));
                }
            }
            Some(Command::RefreshLimits) => {}
            Some(_) => {}
            None => {
                if let Some(pending) = login.take() {
                    let _ = client.login_cancel(&pending).await;
                }
                return Ok(false);
            }
        }
    }
}

fn failure_state(error: anyhow::Error) -> Onboarding {
    let unsafe_backend = error.is::<tr_agent::bridge::UnsafeBackend>();
    if !unsafe_backend && let Some(version) = error.downcast_ref::<WrongVersion>() {
        Onboarding::Unsupported { found: version.found.clone() }
    } else {
        Onboarding::Stopped { reason: scrub_text(&format!("{error:#}")).chars().take(400).collect(), unsafe_backend }
    }
}

fn version_at_path(error: anyhow::Error, binary: &std::path::Path) -> anyhow::Error {
    if let Some(version) = error.downcast_ref::<WrongVersion>() {
        let found = scrub_text(&format!("{} at {}", version.found, binary.display()));
        error.context(WrongVersion { found })
    } else {
        error
    }
}

fn classify(result: Result<()>, unsafe_backend: bool) -> Result<()> {
    if unsafe_backend {
        result
            .and(Err(anyhow::anyhow!("backend reported an unsafe safety state")))
            .context(tr_agent::bridge::UnsafeBackend)
    } else {
        result
    }
}

pub(crate) fn bridge_options(cwd: String, tools: tr_agent_tools::ToolSet) -> BridgeOptions {
    let mut options = BridgeOptions::new(cwd);
    if matches!(tools, tr_agent_tools::ToolSet::Edit | tr_agent_tools::ToolSet::Workflows)
        && crate::agent_bridge::agent_edit_tools_available()
    {
        options.tools = tools;
    }
    options
}

async fn owner(
    mut receive: mpsc::UnboundedReceiver<Command>,
    mut stopping: watch::Receiver<()>,
    inbox: &Inbox,
    executor: AppExecutor,
    parent: PathBuf,
    tools: tr_agent_tools::ToolSet,
) -> Result<()> {
    let Some(binary) = binary() else {
        inbox.state(Onboarding::Missing);
        return Ok(());
    };
    let result = async {
        // Login mode permits only account RPCs and the localhost callback, never a turn.
        let login_connector = connector(&parent, binary.clone(), SessionMode::Login { callback_port: 1455 })?;
        let client = login_connector
            .connect(Arc::new(|_| Box::pin(async { bail!("tools unavailable during sign-in") })))
            .await?;
        let ready = tokio::select! {
            biased;
            _ = stopping.changed() => Ok(false),
            result = async {
                client.initialize().await?;
                onboarding(&mut receive, inbox, &client).await
            } => result,
        };
        let clean = cleanup(&client).await;
        let proceed = ready.as_ref().is_ok_and(|ready| *ready);
        classify(clean.and(ready.map(|_| ())), client.is_unsafe())?;
        if !proceed {
            return Ok(());
        }
        let connector = connector(&parent, binary.clone(), SessionMode::Normal)?;
        let cwd = connector.launch.home.cwd.to_string_lossy().into_owned();
        let mut bridge = executor.start(connector, bridge_options(cwd, tools)).await?;
        let result = tokio::select! {
            biased;
            _ = stopping.changed() => Ok(()),
            result = run_bridge(&mut receive, inbox, &mut bridge) => result,
        };
        bridge.shutdown();
        let clean = cleanup(bridge.client()).await;
        classify(clean.and(result), bridge.is_unsafe())
    }
    .await;
    result.map_err(|error| version_at_path(error, &binary))
}

async fn owner_acp(
    mut receive: mpsc::UnboundedReceiver<Command>,
    mut stopping: watch::Receiver<()>,
    inbox: &Inbox,
    parent: PathBuf,
    tools: tr_agent_tools::ToolSet,
    config: crate::agent_mcp::Settings,
    mimo_model: String,
) -> Result<()> {
    use tr_agent::acp::AcpBackend;
    use tr_agent::transport::WireEvent;
    use tr_agent::{AgentEvent, AgentEvents, ThreadOptions};
    if AcpBackend::find_binary().is_none() {
        inbox.state(Onboarding::Stopped { reason: AcpBackend::missing_message().into(), unsafe_backend: false });
        return Ok(());
    }
    let client = AcpBackend::launch(&parent, config.mimo_profile, config.mimo_load_plugins).await?;
    let run = async {
        client.remember_model(mimo_model);
        acp_check(&client, inbox, client.canary()).await?;
        let registry = tr_agent_tools::Registry::new(tools);
        let instructions = tr_agent::bridge::session_instructions(&bridge_options(String::new(), tools), &registry)
            .replace(
                "Never use shell, filesystem, web, MCP, code execution or delegation.",
                "Use only terminal-rs MCP tools; never use shell, filesystem, web, code execution or delegation.",
            );
        client
            .start_thread(ThreadOptions {
                cwd: String::new(),
                base_instructions: instructions,
                ephemeral: true,
                dynamic_tools: tr_agent::bridge::dynamic_tools(&registry),
            })
            .await?;
        inbox.push(Event::Models(client.list_models().await?));
        inbox.state(Onboarding::Ready);
        let mut events = client.events();
        let mut calls = 0;
        let mut interrupted = false;
        let mut workflow_results = Vec::new();
        let mut deferred = std::collections::VecDeque::new();
        loop {
            tokio::select! {
                event = events.recv() => {
                    let Some(event) = event else { bail!("mimo ACP event stream closed"); };
                    let packet = match event {
                        AgentEvent::TextDelta(params) => Some(("item/agentMessage/delta",params)),
                        AgentEvent::ReasoningDelta(params) => Some(("item/reasoning/textDelta",params)),
                        AgentEvent::ToolStarted(params) => { calls += 1; Some(("item/started",params)) },
                        AgentEvent::ToolFinished(params) => Some(("item/completed",params)),
                        AgentEvent::Done(params) => {
                            if let Some(reason) = params["error"].as_str() {
                                inbox.push(Event::Notice(scrub_text(reason)));
                            }
                            inbox.push(Event::Bridge(BridgeEvent::Completed { turn:params["turnId"].as_str().unwrap_or_default().into(),
                                status:if interrupted {"interrupted"} else {params["turn"]["status"].as_str().unwrap_or("completed")}.into(), calls, interrupt_elapsed:None }));
                            calls = 0; interrupted = false; None
                        }
                        AgentEvent::Notice(message) => { inbox.push(Event::Notice(message)); None },
                        AgentEvent::Error(reason) => bail!("{reason}"),
                        AgentEvent::Usage(_) | AgentEvent::RateLimits(_) => None,
                    };
                    if let Some((method,params)) = packet {
                        let event = BridgeEvent::Wire(WireEvent::Notification {method:method.into(),params});
                        if let Some(event) = project(event) { inbox.push(Event::Bridge(event)); }
                    }
                }
                command = queued_command(&mut receive, &mut deferred) => match command {
                    Some(Command::Analysis { id, input }) => {
                        let (result, _) = isolated_analysis(client.analyze(input), client.interrupt_analysis(), &mut receive, &mut deferred).await;
                        // The dedicated child is stopped on every unconfirmed
                        // ending (including a dropped analyze future). The main
                        // backend and its subscriptions remain untouched.
                        inbox.push(Event::Analysis { id, result });
                        if client.is_unsafe() {
                            client.shutdown();
                            dropped_analysis_actions(inbox, &mut deferred);
                            bail!("Main backend stopped for safety");
                        }
                    }
                    Some(Command::Turn(mut turn)) => {
                        workflow_results.append(&mut turn.input.workflow_results);
                        turn.input.workflow_results = workflow_results.clone();
                        match acp_send_turn(&client, inbox, *turn).await? {
                            Some(id) => {
                                workflow_results.clear();
                                inbox.push(Event::Bridge(BridgeEvent::Started {turn:id}));
                            }
                            None => continue,
                        }
                    }
                    Some(Command::Interrupt) => { interrupted = true; client.interrupt().await?; }
                    Some(Command::SelectModel(model)) => {
                        if let Err(error) = client.select_model(&model).await {
                            inbox.push(Event::Notice(scrub_text(&error.to_string())));
                        }
                    }
                    Some(Command::RefreshLimits) => inbox.state(Onboarding::Ready),
                    Some(Command::WorkflowResults(mut results)) => {
                        workflow_results.append(&mut results);
                        inbox.push(Event::Notice("Workflow outcomes will accompany your next mimo prompt; no prompt was replayed.".into()));
                    }
                    Some(_) => {}, None => return Ok(()),
                }
            }
        }
    };
    let result = tokio::select! { biased; _ = stopping.changed() => Ok(()), result = run => result };
    // Leave Checking immediately, even while bounded child cleanup is still running.
    report_acp_failure(inbox, &result, client.is_unsafe());
    client.shutdown();
    tokio::time::timeout(Duration::from_secs(10), client.wait_cleanup())
        .await
        .context("mimo cleanup timed out; restart the app")??;
    classify(result, client.is_unsafe())
}

fn report_acp_failure(inbox: &Inbox, result: &Result<()>, unsafe_backend: bool) {
    if let Err(error) = result {
        inbox.state(Onboarding::Stopped {
            reason: scrub_text(&format!("{error:#}")).chars().take(400).collect(),
            unsafe_backend,
        });
    }
}

// Consume canary traffic while it runs, not only after Ready. Canary text/tools
// stay out of the conversation; provider notices and failures must be visible.
async fn acp_check(
    client: &tr_agent::acp::AcpBackend,
    inbox: &Inbox,
    check: impl std::future::Future<Output = Result<()>>,
) -> Result<()> {
    use tr_agent::AgentEvents;
    inbox.state(Onboarding::Checking);
    let mut events = client.events();
    tokio::pin!(check);
    let result = loop {
        tokio::select! {
            biased;
            event = events.recv() => match event {
                Some(tr_agent::AgentEvent::Notice(message)) => inbox.push(Event::Notice(message)),
                Some(tr_agent::AgentEvent::Error(reason)) => break Err(anyhow::anyhow!(reason)),
                None => break Err(anyhow::anyhow!("mimo ACP event stream closed during readiness")),
                Some(_) => {},
            },
            result = &mut check => break result,
        }
    };
    // session/new supplies the catalogue even when the provider rejects the
    // default model. Keep the picker usable while Stopped so Restart can use it.
    inbox.push(Event::Models(client.list_models().await?));
    report_acp_failure(inbox, &result, client.is_unsafe());
    result
}

async fn acp_send_turn(
    client: &tr_agent::acp::AcpBackend,
    inbox: &Inbox,
    mut turn: BridgeTurn,
) -> Result<Option<String>> {
    turn.include_chart_context()?;
    match client.send_turn(turn.input).await {
        Ok(id) => Ok(Some(id)),
        Err(error) if client.is_closed() || client.is_unsafe() => Err(error),
        Err(error) => {
            inbox.push(Event::Notice(scrub_text(&error.to_string())));
            // Clear the optimistic UI running state and pause queued prompts,
            // just like Codex recovery, without restarting or replaying a turn.
            inbox.push(Event::Bridge(BridgeEvent::Recovered(tr_agent::bridge::Recovery::Resumed)));
            Ok(None)
        }
    }
}

async fn deliver_workflow_results(
    client: &CodexAppServer,
    undelivered: &mut Vec<tr_agent::codex::WorkflowResult>,
    results: Vec<tr_agent::codex::WorkflowResult>,
) {
    if !results.is_empty() && client.workflow_results(&results).await.is_err() {
        undelivered.extend(results);
    }
}

async fn queued_command(
    receive: &mut mpsc::UnboundedReceiver<Command>,
    deferred: &mut std::collections::VecDeque<Command>,
) -> Option<Command> {
    if let Some(command) = deferred.pop_front() { Some(command) } else { receive.recv().await }
}

/// The same FIFO owns both kinds of work. Non-interrupt commands arriving
/// during analysis are deferred, not lost or steered into the isolated thread.
async fn isolated_analysis(
    run: impl std::future::Future<Output = std::result::Result<String, tr_agent::AnalysisError>>,
    cancel: impl std::future::Future<Output = ()>,
    receive: &mut mpsc::UnboundedReceiver<Command>,
    deferred: &mut std::collections::VecDeque<Command>,
) -> (std::result::Result<String, tr_agent::AnalysisError>, bool) {
    use tr_agent::AnalysisError;
    tokio::pin!(run);
    tokio::pin!(cancel);
    let mut cancelling = false;
    let mut cancel_sent = false;
    let mut receive_open = true;
    let deadline = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            biased;
            result = &mut run => {
                let confirmed = !matches!(result, Err(AnalysisError::TimedOut | AnalysisError::NotReady | AnalysisError::NoAnswerTooLong));
                let result = if cancelling && matches!(result, Ok(_) | Err(AnalysisError::NoAnswer)) {
                    Err(AnalysisError::Cancelled)
                } else { result };
                return (result, confirmed);
            },
            () = &mut cancel, if cancelling && !cancel_sent => cancel_sent = true,
            () = &mut deadline, if cancelling => return (Err(AnalysisError::Cancelled), false),
            command = receive.recv(), if receive_open => match command {
                Some(Command::Interrupt) | None => {
                    if command.is_none() { receive_open = false; }
                    if !cancelling {
                        cancelling = true;
                        deadline.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(3));
                    }
                },
                Some(command) => deferred.push_back(command),
            }
        }
    }
}

fn dropped_analysis_actions(inbox: &Inbox, deferred: &mut std::collections::VecDeque<Command>) {
    if deferred.is_empty() {
        return;
    }
    let actions = deferred
        .drain(..)
        .map(|command| match command {
            Command::Turn(_) => "prompt",
            Command::Analysis { .. } => "story analysis",
            Command::WorkflowResults(_) => "workflow outcomes",
            Command::SelectModel(_) => "model selection",
            Command::RefreshLimits => "limits refresh",
            _ => "agent action",
        })
        .collect::<Vec<_>>()
        .join(", ");
    inbox.push(Event::Notice(format!(
        "Backend stopped for safety; deferred actions dropped: {actions}. Nothing was replayed."
    )));
}

async fn run_bridge(
    receive: &mut mpsc::UnboundedReceiver<Command>,
    inbox: &Inbox,
    bridge: &mut AgentBridge,
) -> Result<()> {
    let mut models = bridge.client().list_models().await?;
    if models.is_empty() {
        bail!("Codex returned no supported models")
    }
    models.sort_by_key(|m| !m.is_default);
    inbox.push(Event::Models(models));
    inbox.state(Onboarding::Ready);
    inbox.limits(&bridge.client().rate_limits().await?);
    // The owner serializes steers and turn starts. Only failed/no-active-turn
    // deliveries carry over; a successful steer is never repeated next turn.
    let mut undelivered = Vec::new();
    let mut deferred = std::collections::VecDeque::new();
    loop {
        tokio::select! {
            event = bridge.next_event() => {
                let event = event?;
                if let Some(event) = project(event) { inbox.push(Event::Bridge(event)); }
            }
            command = queued_command(receive, &mut deferred) => match command {
                Some(Command::Analysis { id, input }) => {
                    let (result, confirmed) = isolated_analysis(bridge.client().analyze(input), bridge.client().interrupt_analysis(), receive, &mut deferred).await;
                    if !confirmed || bridge.is_unsafe() {
                        bridge.shutdown();
                        dropped_analysis_actions(inbox, &mut deferred);
                        inbox.push(Event::Analysis { id, result });
                        bail!("Analysis session retired; backend stopped for safety");
                    }
                    inbox.push(Event::Analysis { id, result });
                }
                Some(Command::Turn(mut turn)) => {
                    undelivered.append(&mut turn.input.workflow_results);
                    turn.input.workflow_results = undelivered.clone();
                    match bridge.send(*turn).await {
                        // The ack already returns the canonical key. Do not depend on
                        // a later wire turn/started notification to retain partial edits.
                        Ok(turn) => {
                            undelivered.clear();
                            inbox.push(Event::Bridge(BridgeEvent::Started { turn }));
                        }
                        Err(error) => {
                            inbox.push(Event::Notice(scrub_text(&error.to_string())));
                            // Recovery is bridge-controlled; it never replays the failed turn.
                            let recovery = bridge.recover().await?;
                            inbox.push(Event::Bridge(BridgeEvent::Recovered(recovery)));
                        }
                    }
                }
                Some(Command::WorkflowResults(results)) => {
                    deliver_workflow_results(bridge.client(), &mut undelivered, results).await;
                }
                Some(Command::Interrupt) => { if let Err(error) = bridge.interrupt().await { inbox.push(Event::Notice(scrub_text(&error.to_string()))); } }
                Some(Command::RefreshLimits) => { inbox.state(Onboarding::Ready); inbox.limits(&bridge.client().rate_limits().await?); }
                Some(_) => {}
                None => return Ok(()),
            }
        }
    }
}

pub(crate) fn project(event: BridgeEvent) -> Option<BridgeEvent> {
    match event {
        BridgeEvent::Wire(tr_agent::transport::WireEvent::Notification { method, mut params }) => {
            // Only visible text, tool metadata, limits and failures wake the pane.
            match method.as_str() {
                "item/started" | "item/completed" if params["item"]["type"] == "dynamicToolCall" => {
                    // A5b-MULTI: retain only the bounded numeric projection, not raw results.
                    let local = crate::agent_pane::local_tool_projection(&params["item"]);
                    let scan = crate::agent_pane::scan_tool_projection(&params["item"]);
                    let multi = crate::agent_pane::multi_tool_projection(&params["item"]);
                    let visibility = crate::agent_pane::drawing_visibility_projection(&params["item"]);
                    let summary = crate::agent_pane::edit_tool_summary(&params["item"]);
                    let error = crate::agent_pane::tool_error(&params["item"]);
                    if let Some(map) = params["item"].as_object_mut() {
                        map.retain(|key, _| {
                            matches!(key.as_str(), "id" | "type" | "tool" | "arguments" | "success" | "durationMs")
                        });
                        if let Some(local) = local {
                            map.insert("localRead".into(), local);
                        }
                        if let Some(scan) = scan {
                            map.insert("scanRead".into(), scan);
                        }
                        if let Some(multi) = multi {
                            map.insert("multiRead".into(), multi);
                        }
                        if let Some(visibility) = visibility {
                            map.insert("drawingVisibility".into(), visibility);
                        }
                        if let Some(summary) = summary {
                            map.remove("arguments");
                            map.insert("editSummary".into(), summary.into());
                        }
                        if let Some(error) = error {
                            map.insert("error".into(), error.into());
                        }
                    }
                }
                "item/started" | "item/completed" if params["item"]["type"] == "agentMessage" => {
                    if let Some(map) = params["item"].as_object_mut() {
                        map.retain(|key, _| matches!(key.as_str(), "id" | "type" | "text"));
                    }
                }
                "account/rateLimits/updated" => {
                    params = visible_limits(&params);
                }
                "item/agentMessage/delta" | "error" => {}
                _ => return None,
            }
            Some(BridgeEvent::Wire(tr_agent::transport::WireEvent::Notification { method, params }))
        }
        other => Some(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tr_agent::transport::{Transport, WireEvent};

    #[tokio::test(start_paused = true)]
    async fn isolated_owner_defers_workflow_and_model_commands_and_cancels_without_replay() {
        let (send, mut receive) = mpsc::unbounded_channel();
        send.send(Command::WorkflowResults(vec![tr_agent::codex::WorkflowResult::Undo {
            turn: "main".into(),
            reverted: 1,
            kept: vec![],
        }]))
        .unwrap();
        send.send(Command::SelectModel("next-model".into())).unwrap();
        send.send(Command::Interrupt).unwrap();
        let mut deferred = std::collections::VecDeque::new();
        let result = isolated_analysis(std::future::pending(), async {}, &mut receive, &mut deferred).await;
        assert_eq!(result, (Err(tr_agent::AnalysisError::Cancelled), false));
        assert!(matches!(deferred.pop_front(), Some(Command::WorkflowResults(results)) if results.len() == 1));
        assert!(matches!(deferred.pop_front(), Some(Command::SelectModel(model)) if model == "next-model"));
        assert!(deferred.is_empty());
        assert!(receive.try_recv().is_err());
    }

    #[tokio::test]
    async fn confirmed_analysis_end_replays_deferred_actions_before_new_commands() {
        for outcome in [
            Ok("answer".into()),
            Err(tr_agent::AnalysisError::Cancelled),
            Err(tr_agent::AnalysisError::NoAnswer),
            Err(tr_agent::AnalysisError::ToolAttempt),
        ] {
            let (send, mut receive) = mpsc::unbounded_channel();
            send.send(Command::WorkflowResults(vec![])).unwrap();
            send.send(Command::SelectModel("deferred".into())).unwrap();
            send.send(Command::Interrupt).unwrap();
            let (end, ended) = tokio::sync::oneshot::channel();
            let mut deferred = std::collections::VecDeque::new();
            let expected = outcome.clone();
            let (result, confirmed) = isolated_analysis(
                async { ended.await.unwrap() },
                async {
                    end.send(outcome).unwrap();
                },
                &mut receive,
                &mut deferred,
            )
            .await;
            assert!(confirmed, "a confirmed cancellation or refused tool never stops main");
            assert_eq!(
                result,
                if matches!(expected, Ok(_) | Err(tr_agent::AnalysisError::NoAnswer)) {
                    Err(tr_agent::AnalysisError::Cancelled)
                } else {
                    expected
                }
            );
            send.send(Command::RefreshLimits).unwrap();
            assert!(matches!(queued_command(&mut receive, &mut deferred).await, Some(Command::WorkflowResults(_))));
            assert!(
                matches!(queued_command(&mut receive, &mut deferred).await, Some(Command::SelectModel(model)) if model == "deferred")
            );
            assert!(matches!(queued_command(&mut receive, &mut deferred).await, Some(Command::RefreshLimits)));
            assert!(deferred.is_empty());
        }
    }

    #[test]
    fn stopped_analysis_reports_which_deferred_actions_were_dropped() {
        let inbox = Inbox {
            events: Mutex::new(Vec::new()),
            visible: AtomicBool::new(false),
            ctx: egui::Context::default(),
            last_text_wake: Mutex::new(None),
        };
        let mut deferred = std::collections::VecDeque::from([
            Command::WorkflowResults(vec![]),
            Command::SelectModel("sensitive-model-value".into()),
            Command::RefreshLimits,
        ]);
        dropped_analysis_actions(&inbox, &mut deferred);
        assert!(deferred.is_empty());
        let posted = inbox.events.lock().unwrap();
        assert!(
            matches!(&posted[..], [Event::Notice(text)] if text.contains("workflow outcomes, model selection, limits refresh") && text.contains("Nothing was replayed") && !text.contains("sensitive-model-value"))
        );
    }

    #[tokio::test]
    async fn mimo_echo_and_segmented_stream_render_one_prompt_and_two_replies() {
        use crate::agent_pane::{AgentPane, Prompt, RenderedBlock};
        use tr_agent::{AgentEvent, AgentEvents};
        let (client_io, server_io) = tokio::io::duplex(65536);
        let (read, write) = tokio::io::split(client_io);
        let client = tr_agent::acp::AcpBackend::connect(
            read,
            write,
            PathBuf::from("/empty"),
            PathBuf::from("/app"),
            Arc::new(|| {}),
        );
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server_io);
            let mut read = BufReader::new(read);
            loop {
                let line = tr_agent::mcp::read_line(&mut read).await.unwrap().unwrap();
                let request: serde_json::Value = serde_json::from_slice(&line).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "initialize" => json!({"protocolVersion":1}),
                    "session/new" => json!({"sessionId":"session"}),
                    "session/prompt" => {
                        for update in [
                            json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"What's the trend?"}}),
                            json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Keep these separated."}}),
                            json!({"sessionUpdate":"tool_call","toolCallId":"read","title":"terminal-rs_read_chart","kind":"other"}),
                            json!({"sessionUpdate":"tool_call_update","toolCallId":"read","status":"completed"}),
                            json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Verdict: up\n80,799.51"}}),
                        ] {
                            tr_agent::mcp::write_line(&mut write, &json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session","update":update}})).await.unwrap();
                        }
                        json!({"stopReason":"end_turn"})
                    }
                    _ => panic!("unexpected ACP request"),
                };
                tr_agent::mcp::write_line(&mut write, &tr_agent::mcp::success(request["id"].clone(), result))
                    .await
                    .unwrap();
                if request["method"] == "session/prompt" {
                    // Keep the fake child alive through Done consumption.
                    std::future::pending::<()>().await;
                }
            }
        });
        client
            .start_thread(tr_agent::ThreadOptions {
                cwd: String::new(),
                base_instructions: String::new(),
                ephemeral: true,
                dynamic_tools: vec![],
            })
            .await
            .unwrap();
        let mut events = client.events();
        let mut pane = AgentPane::default();
        pane.transition(Onboarding::Ready);
        pane.start_prompt(Prompt { id: Default::default(), text: "What's the trend?".into(), requires_tools: false })
            .unwrap();
        let turn = client
            .send_turn(tr_agent::TurnInput { text: "What's the trend?".into(), ..Default::default() })
            .await
            .unwrap();
        pane.event(BridgeEvent::Started { turn: turn.clone() });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let (method, params) = match events.recv().await.unwrap() {
                    AgentEvent::TextDelta(v) => ("item/agentMessage/delta", v),
                    AgentEvent::ToolStarted(v) => ("item/started", v),
                    AgentEvent::ToolFinished(v) => ("item/completed", v),
                    AgentEvent::Done(_) => break,
                    e => panic!("unexpected event: {e:?}"),
                };
                pane.event(
                    project(BridgeEvent::Wire(WireEvent::Notification { method: method.into(), params })).unwrap(),
                );
            }
        })
        .await
        .unwrap();
        pane.event(BridgeEvent::Completed { turn, status: "completed".into(), calls: 1, interrupt_elapsed: None });
        assert_eq!(pane.turns.len(), 1);
        assert_eq!(pane.blocks.iter().filter(|b| matches!(b, RenderedBlock::Prompt(_))).count(), 1);
        let replies: Vec<_> = pane
            .blocks
            .iter()
            .filter_map(|b| if let RenderedBlock::Reply(blocks) = b { Some(blocks.clone()) } else { None })
            .collect();
        assert_eq!(
            replies,
            [
                crate::agent_markdown::parse("Keep these separated."),
                crate::agent_markdown::parse("Verdict: up\n80,799.51")
            ]
        );
        assert!(matches!(&pane.blocks[2], RenderedBlock::Tool(_)));
        pane.backend = "mimo";
        pane.acknowledged = true;
        pane.disclosure = false;
        for width in [240.0, 360.0, 660.0] {
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 900.0))),
                    ..Default::default()
                },
                |ui| {
                    pane.show(ui, "No chart pinned", egui::Id::new("mimo-test"));
                },
            );
            let mut rows = Vec::new();
            fn collect(shape: &egui::Shape, rows: &mut Vec<(String, egui::Rect)>) {
                match shape {
                    egui::Shape::Text(t)
                        if ["What's the trend?", "Keep these separated.", "Verdict: up 80,799.51"]
                            .contains(&t.galley.text()) =>
                    {
                        rows.push((t.galley.text().into(), t.galley.rect.translate(t.pos.to_vec2())));
                    }
                    egui::Shape::Vec(shapes) => {
                        for s in shapes {
                            collect(s, rows);
                        }
                    }
                    _ => {}
                }
            }
            for shape in &output.shapes {
                collect(&shape.shape, &mut rows);
            }
            output.textures_delta.clear();
            assert_eq!(
                rows.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>(),
                ["What's the trend?", "Keep these separated.", "Verdict: up 80,799.51"]
            );
            assert!(
                rows.windows(2).all(|pair| pair[0].1.bottom() <= pair[1].1.top()),
                "message rectangles must not overlap"
            );
        }
        server.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn acp_readiness_notices_and_failures_leave_pane_checking_with_restart() {
        for failure in ["network", "timeout", "model"] {
            let network = failure == "network";
            let (client_io, server_io) = tokio::io::duplex(65536);
            let (read, write) = tokio::io::split(client_io);
            let client = tr_agent::acp::AcpBackend::connect(
                read,
                write,
                PathBuf::from("/empty"),
                PathBuf::from("/app"),
                Arc::new(|| {}),
            );
            let inbox = Inbox {
                events: Mutex::new(Vec::new()),
                visible: AtomicBool::new(false),
                ctx: egui::Context::default(),
                last_text_wake: Mutex::new(None),
            };
            if failure != "model" {
                client.remember_model("saved".into());
            }
            let server = async {
                let (read, mut write) = tokio::io::split(server_io);
                let mut read = BufReader::new(read);
                let mut attempt = 0;
                loop {
                    let line = tr_agent::mcp::read_line(&mut read).await.unwrap().unwrap();
                    let request: serde_json::Value = serde_json::from_slice(&line).unwrap();
                    let result = match request["method"].as_str().unwrap() {
                        "initialize" => json!({"protocolVersion":1}),
                        "session/new" => {
                            attempt += 1;
                            json!({"sessionId":format!("canary-{attempt}"),"models":{"currentModelId":"current",
                                "availableModels":[{"modelId":"current"},{"modelId":"saved"}]}})
                        }
                        "session/set_model" => {
                            assert_eq!(request["params"]["modelId"], "saved");
                            json!({})
                        }
                        "session/prompt" => {
                            if failure == "model" {
                                tr_agent::mcp::write_line(
                                    &mut write,
                                    &tr_agent::mcp::error(request["id"].clone(), -32603, "Not supported model"),
                                )
                                .await
                                .unwrap();
                            }
                            if network {
                                for provider_attempt in 1..=3 {
                                    tr_agent::mcp::write_line(
                                        &mut write,
                                        &json!({"jsonrpc":"2.0","method":"session/update",
                                        "params":{"sessionId":format!("canary-{attempt}"),"update":{"sessionUpdate":"agent_message_chunk",
                                            "content":{"type":"retry","attempt":provider_attempt,"kind":"network",
                                                "message":"Cannot connect to API; Bearer fixture-secret"}}}}),
                                    )
                                    .await
                                    .unwrap();
                                }
                            }
                            continue;
                        }
                        "session/cancel" => {
                            if failure == "model" || attempt == 2 {
                                break;
                            }
                            continue;
                        }
                        _ => panic!("unexpected ACP request"),
                    };
                    tr_agent::mcp::write_line(&mut write, &tr_agent::mcp::success(request["id"].clone(), result))
                        .await
                        .unwrap();
                }
            };
            let (result, ()) = tokio::join!(acp_check(&client, &inbox, client.canary()), server);
            let error = result.unwrap_err().to_string();
            assert!(
                error.contains(match failure {
                    "network" => "couldn't reach its model provider for the startup check (network; tried twice)",
                    "model" => "mimo's default model isn't available on your plan — pick a model in the Agent pane and Restart",
                    _ => "didn't answer its startup check within 2 minutes (tried twice)",
                }),
                "{error}"
            );
            let posted = std::mem::take(&mut *inbox.events.lock().unwrap());
            assert_eq!(
                posted.iter().filter(|e| matches!(e, Event::Notice(_))).count(),
                match failure {
                    "network" => 8, // Two progress notices plus three network notices per attempt.
                    "timeout" => 2,
                    _ => 1,
                }
            );
            assert_eq!(
                posted
                    .iter()
                    .any(|e| matches!(e, Event::Notice(message) if message == "Checking mimo… (attempt 2 of 2)")),
                failure != "model"
            );
            let mut pane = crate::agent_pane::AgentPane::default();
            for event in posted {
                match event {
                    Event::State(state) => pane.transition(state),
                    Event::Notice(message) => {
                        assert!(!message.contains("fixture-secret"));
                        assert_eq!(pane.state, Onboarding::Checking, "notice must arrive before readiness fails");
                        pane.notice(&message);
                    }
                    Event::Models(models) => {
                        assert_eq!(models.len(), 2);
                        pane.models = models;
                    }
                    _ => panic!("unexpected readiness event"),
                }
            }
            assert!(matches!(&pane.state, Onboarding::Stopped { reason, unsafe_backend: false } if reason == &error));
            assert_eq!(pane.state.primary(), "Restart");
        }
    }

    #[tokio::test]
    async fn acp_turn_rejection_posts_notice_and_allows_later_turns() {
        use tr_agent::acp::AcpBackend;
        let (client_io, server_io) = tokio::io::duplex(65536);
        let (read, write) = tokio::io::split(client_io);
        let client = AcpBackend::connect(read, write, PathBuf::from("/empty"), PathBuf::from("/app"), Arc::new(|| {}));
        let inbox = Inbox {
            events: Mutex::new(Vec::new()),
            visible: AtomicBool::new(false),
            ctx: egui::Context::default(),
            last_text_wake: Mutex::new(None),
        };
        let (prompt_seen, seen) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut prompt_seen = Some(prompt_seen);
            let (read, mut write) = tokio::io::split(server_io);
            let mut read = BufReader::new(read);
            let mut rejected = false;
            loop {
                let Some(line) = tr_agent::mcp::read_line(&mut read).await.unwrap() else {
                    return;
                };
                let request: serde_json::Value = serde_json::from_slice(&line).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "initialize" => json!({"protocolVersion":1}),
                    "session/new" => json!({"sessionId":"test","models":{"availableModels":[{"modelId":"model"}]}}),
                    "session/set_model" if !rejected => {
                        rejected = true;
                        tr_agent::mcp::write_line(
                            &mut write,
                            &tr_agent::mcp::error(request["id"].clone(), -32602, "rejected"),
                        )
                        .await
                        .unwrap();
                        continue;
                    }
                    "session/set_model" => json!({}),
                    "session/prompt" => {
                        let text = request["params"]["prompt"][0]["text"].as_str().unwrap();
                        let context =
                            text.split("Chart context (untrusted data): ").nth(1).unwrap().lines().next().unwrap();
                        let context: serde_json::Value = serde_json::from_str(context).unwrap();
                        let pin: serde_json::Value = serde_json::from_str(context["chart"].as_str().unwrap()).unwrap();
                        assert_eq!(
                            pin,
                            json!({"uid":42,"symbol":"BTCUSD Curncy","timeframe":"1D","visible_range":[100,200]})
                        );
                        assert!(text.ends_with("User: next"));
                        assert!(!text.contains("Checking mimo") && !text.contains("provider unreachable"));
                        prompt_seen.take().unwrap().send(()).unwrap();
                        // Leave the turn running until shutdown for the busy-turn assertion.
                        continue;
                    }
                    _ => panic!("unexpected ACP request"),
                };
                tr_agent::mcp::write_line(&mut write, &tr_agent::mcp::success(request["id"].clone(), result))
                    .await
                    .unwrap();
            }
        });
        client
            .start_thread(tr_agent::ThreadOptions {
                cwd: String::new(),
                base_instructions: String::new(),
                ephemeral: true,
                dynamic_tools: vec![],
            })
            .await
            .unwrap();
        let input = || BridgeTurn {
            input: tr_agent::TurnInput { model: "model".into(), text: "next".into(), ..Default::default() },
            pin: tr_agent::bridge::ChartPin {
                uid: 42,
                symbol: "BTCUSD Curncy".into(),
                timeframe: "1D".into(),
                visible_range: Some((100, 200)),
            },
            requires_tools: false,
            screenshot_consent: false,
        };
        assert!(acp_send_turn(&client, &inbox, input()).await.unwrap().is_none());
        assert!(!client.is_closed());
        assert!(acp_send_turn(&client, &inbox, input()).await.unwrap().is_some());
        tokio::time::timeout(Duration::from_secs(2), seen).await.unwrap().unwrap();
        assert!(acp_send_turn(&client, &inbox, input()).await.unwrap().is_none());
        let posted = std::mem::take(&mut *inbox.events.lock().unwrap());
        assert_eq!(posted.iter().filter(|e| matches!(e, Event::Notice(_))).count(), 2);
        assert_eq!(posted.iter().filter(|e| matches!(e, Event::Bridge(BridgeEvent::Recovered(_)))).count(), 2);
        let mut pane = crate::agent_pane::AgentPane::default();
        pane.running = true;
        for event in posted {
            match event {
                Event::Notice(reason) => pane.error_notice(&reason),
                Event::Bridge(event) => pane.event(event),
                _ => panic!("a recoverable failure must not stop onboarding"),
            }
        }
        assert!(!pane.running, "a rejected start must clear optimistic UI state");
        client.shutdown();
        assert!(acp_send_turn(&client, &inbox, input()).await.is_err());
        server.abort();
    }

    #[test]
    fn review_bridge_executor_timeout_outlasts_scan_deadline() {
        for tools in [tr_agent_tools::ToolSet::ReadOnly, tr_agent_tools::ToolSet::Workflows] {
            let options = bridge_options(String::new(), tools);
            assert_eq!(options.executor_timeout, std::time::Duration::from_secs(30));
            assert_eq!(options.executor_timeout_for("read_scan_results"), options.executor_timeout);
            assert_eq!(options.executor_timeout_for("analyze"), options.executor_timeout);
            assert_eq!(
                options.executor_timeout_for("run_scan"),
                tr_agent_tools::args::scanner::SCAN_DEADLINE + std::time::Duration::from_secs(15)
            );
        }
    }

    #[test]
    fn review_mid_turn_approval_is_delivered_once() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/review-steer-duplicate");
        let mut app = crate::app::TerminalApp::test_workspace(&root);
        let (sender, mut receive) = mpsc::unbounded_channel();
        let (shutdown, _stopping) = watch::channel(());
        let (_done, finished) = std::sync::mpsc::channel();
        let inbox = Arc::new(Inbox {
            events: Mutex::new(Vec::new()),
            visible: AtomicBool::new(true),
            ctx: egui::Context::default(),
            last_text_wake: Mutex::new(None),
        });
        app.agent_pane.session = Some(Session { sender: Some(sender), inbox, shutdown, finished, owner: None });
        app.agent_pane.running = true;
        app.agent_workflow(
            "steer:1",
            tr_agent_tools::args::LiveArgs::CreateWatchlist(tr_agent_tools::args::CreateWatchlist {
                name: "Crypto".into(),
            }),
        );
        app.confirm_workflows("steer:1", true);
        let mut steered = 0;
        while let Ok(command) = receive.try_recv() {
            if let Command::WorkflowResults(results) = command {
                steered += results.len();
            }
        }
        assert_eq!(steered, 1, "approval goes to the active turn via turn/steer");
        assert!(
            app.agent_pane.workflows.context.is_empty(),
            "already-steered outcome is queued again as next-turn context: {:?}",
            app.agent_pane.workflows.context
        );
        app.confirm_workflows("steer:1", true);
        assert!(receive.try_recv().is_err(), "a repeated approval must not send an empty steer");
    }

    #[tokio::test]
    async fn workflow_delivery_carries_only_failed_or_inactive_steers_and_never_sends_empty() {
        use tr_agent::codex::{WorkflowResult, WorkflowStatus};
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (read, write) = tokio::io::split(client_io);
        let client = CodexAppServer::new(Transport::connect(
            read,
            write,
            Arc::new(|_| Box::pin(async { Ok(json!({})) })),
            Arc::new(|| {}),
            None,
        ));
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server_io);
            let mut read = BufReader::new(read).lines();
            let mut starts = 0;
            let mut steers = 0;
            while let Some(line) = read.next_line().await.unwrap() {
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let method = request["method"].as_str().unwrap();
                let result = match method {
                    "initialized" => continue,
                    "initialize" => json!({}),
                    "skills/list" => {
                        json!({"data":[{"skills":tr_agent::codex::BUNDLED_SKILLS.iter().map(|name| json!({"name":name,"enabled":false})).collect::<Vec<_>>()}]})
                    }
                    "account/read" => json!({"account":{"type":"chatgpt"}}),
                    "thread/start" => json!({"thread":{"id":"thread"}}),
                    "turn/start" => {
                        starts += 1;
                        let outcomes = request["params"]["additionalContext"].get("application_workflow_results");
                        assert_eq!(outcomes.is_some(), starts < 3, "only undelivered outcomes enter start context");
                        if let Some(outcomes) = outcomes {
                            let text = outcomes["value"].as_str().unwrap();
                            assert!(text.contains(if starts == 1 { "inactive outcome" } else { "failed outcome" }));
                            assert!(!text.contains("delivered outcome"));
                        }
                        json!({"turn":{"id":format!("turn-{starts}")}})
                    }
                    "turn/steer" => {
                        steers += 1;
                        let text = request["params"]["additionalContext"]["application_workflow_results"]["value"]
                            .as_str()
                            .unwrap();
                        assert!(text.contains(if steers == 1 { "failed outcome" } else { "delivered outcome" }));
                        if steers == 1 {
                            write.write_all(format!("{}\n", json!({"id":request["id"],"error":{"code":-32602,"message":"turn already completed"}})).as_bytes()).await.unwrap();
                            continue;
                        }
                        json!({"turnId":"turn-2"})
                    }
                    _ => panic!("unexpected method {method}"),
                };
                write.write_all(format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes()).await.unwrap();
                if starts == 3 {
                    assert_eq!(steers, 2, "no empty steer");
                    return;
                }
            }
            panic!("server ended before final start");
        });
        client.initialize().await.unwrap();
        client
            .start_thread(tr_agent::ThreadOptions {
                cwd: "empty".into(),
                base_instructions: "test".into(),
                ephemeral: true,
                dynamic_tools: vec![],
            })
            .await
            .unwrap();
        let outcome = |id, summary: &str| {
            vec![WorkflowResult::Confirmation {
                proposal_id: id,
                status: WorkflowStatus::Approved,
                summary: summary.into(),
            }]
        };
        let mut pending = Vec::new();
        deliver_workflow_results(&client, &mut pending, outcome(1, "inactive outcome")).await;
        assert_eq!(pending.len(), 1, "no active turn must carry over");
        for turn in 1..=3 {
            client
                .send_turn(tr_agent::TurnInput {
                    model: "fake".into(),
                    text: "next".into(),
                    workflow_results: pending.clone(),
                    ..Default::default()
                })
                .await
                .unwrap();
            pending.clear(); // same successful-start acknowledgement as run_bridge
            if turn == 1 {
                deliver_workflow_results(&client, &mut pending, outcome(2, "failed outcome")).await;
                assert_eq!(pending.len(), 1);
            } else if turn == 2 {
                deliver_workflow_results(&client, &mut pending, outcome(3, "delivered outcome")).await;
                assert!(pending.is_empty(), "successful steer must not be queued for next start");
                deliver_workflow_results(&client, &mut pending, vec![]).await;
                client.workflow_results(&[]).await.unwrap();
            }
        }
        server.await.unwrap();
        client.shutdown();
    }

    #[tokio::test]
    async fn a0_fixture_through_fake_app_server_renders_deterministic_blocks() {
        const FIXTURE: &str = include_str!("../../crates/agent/tests/fixtures/codex-0.163.0-live-trend-canary.jsonl");
        let (client_io, server_io) = tokio::io::duplex(256 * 1024);
        let (read, write) = tokio::io::split(client_io);
        let transport = Transport::connect(
            read,
            write,
            Arc::new(|_| Box::pin(async { Ok(json!({"success":false,"contentItems":[]})) })),
            Arc::new(|| {}),
            None,
        );
        let mut events = transport.events();
        let client = CodexAppServer::new(transport);
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server_io);
            let mut read = BufReader::new(read).lines();
            while let Some(line) = read.next_line().await.unwrap() {
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let result = match request["method"].as_str().unwrap_or_default() {
                    "initialized" => continue,
                    "initialize" => json!({}),
                    "skills/list" => FIXTURE
                        .lines()
                        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                        .find(|v| v["id"] == 2 && v.get("result").is_some())
                        .unwrap()["result"]
                        .clone(),
                    "account/read" => json!({"account":{"type":"chatgpt"}}),
                    "thread/start" => json!({"thread":{"id":"thread"}}),
                    "turn/start" => json!({"turn":{"id":"turn"}}),
                    _ => continue,
                };
                write.write_all(format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes()).await.unwrap();
                if request["method"] != "turn/start" {
                    continue;
                }
                let mut started = false;
                for line in FIXTURE.lines() {
                    let value: serde_json::Value = serde_json::from_str(line).unwrap();
                    if value["method"] == "turn/start" {
                        started = true;
                        continue;
                    }
                    if !started || value.get("method").is_none() {
                        continue;
                    }
                    write.write_all(format!("{value}\n").as_bytes()).await.unwrap();
                    if value["method"] == "item/tool/call" {
                        let reply: serde_json::Value =
                            serde_json::from_str(&read.next_line().await.unwrap().unwrap()).unwrap();
                        assert_eq!(reply["id"], value["id"]);
                    }
                    if value["method"] == "turn/completed" {
                        return;
                    }
                }
            }
        });
        client.initialize().await.unwrap();
        client
            .start_thread(tr_agent::ThreadOptions {
                cwd: "empty".into(),
                base_instructions: "chart analyst".into(),
                ephemeral: true,
                dynamic_tools: Vec::new(),
            })
            .await
            .unwrap();
        client
            .send_turn(tr_agent::TurnInput {
                model: "fake".into(),
                effort: "low".into(),
                text: "trend".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut pane = crate::agent_pane::AgentPane::default();
        let mut expected_text = String::new();
        for line in FIXTURE.lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            if value["method"] == "item/agentMessage/delta" {
                expected_text.push_str(value["params"]["delta"].as_str().unwrap());
            }
            if value["method"] == "turn/completed" {
                break;
            }
        }
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(event) = events.recv().await {
                if let WireEvent::Notification { method, .. } = &event
                    && method == "turn/completed"
                {
                    break;
                }
                if let Some(event) = project(BridgeEvent::Wire(event)) {
                    pane.event(event);
                }
            }
        })
        .await
        .unwrap();
        pane.event(BridgeEvent::Completed {
            turn: "turn".into(),
            status: "completed".into(),
            calls: 2,
            interrupt_elapsed: None,
        });
        server.await.unwrap();
        let tools: Vec<_> = pane
            .blocks
            .iter()
            .filter_map(|block| match block {
                crate::agent_pane::RenderedBlock::Tool(trace) => {
                    Some((trace.name.as_str(), trace.success, trace.duration_ms))
                }
                _ => None,
            })
            .collect();
        assert_eq!(tools, [("read_chart", Some(true), Some(0)), ("read_data", Some(true), Some(0))]);
        let replies: Vec<_> = pane
            .blocks
            .iter()
            .filter_map(|block| match block {
                crate::agent_pane::RenderedBlock::Reply(blocks) => Some(blocks.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(replies, [crate::agent_markdown::parse(&expected_text)]);
        assert!(!format!("{:?}", pane.blocks).contains("base64"));
        client.shutdown();
    }

    #[test]
    fn edit_rows_project_ids_and_short_labels_without_raw_results_or_coordinates() {
        for (name, args, result, expected) in [
            (
                "create_drawing",
                json!({"kind":"horizontal","anchors":[{"time":123456789,"price":227.123456}]}),
                json!({"id":42}),
                "#42 · Horizontal line at 227.12",
            ),
            (
                "create_drawing",
                json!({"kind":"trend_line","anchors":[{"time":123456789,"price":227.123456},{"time":987654321,"price":228.123456}]}),
                json!({"id":43}),
                "#43 · Trend line",
            ),
            (
                "update_drawing",
                json!({"id":42,"anchors":[{"time":123456789,"price":229.123456}]}),
                json!({"id":42}),
                "#42 · drawing update",
            ),
            ("delete_drawing", json!({"id":42}), json!({"id":42}), "#42 · drawing removed"),
            ("add_indicator", json!({"code":"rsi","params":[14]}), json!({"id":"7:4"}), "#7:4 · RSI 14"),
            ("remove_indicator", json!({"id":"7:4"}), json!({"id":"7:4"}), "#7:4 · indicator removed"),
            (
                "set_view",
                json!({"from":123456789,"to":987654321,"timeframe":"1W"}),
                json!({"uid":7,"from":123456789,"to":987654321}),
                "#7 · chart view 1W",
            ),
            (
                "compare",
                json!({"symbol":"MSFT","action":"add"}),
                json!({"symbol":"MSFT US Equity"}),
                "comparison MSFT US Equity · add",
            ),
        ] {
            let event = project(BridgeEvent::Wire(WireEvent::Notification {
                method: "item/completed".into(), params: json!({"item":{
                    "type":"dynamicToolCall", "id":"call", "tool":name, "arguments":args, "success":true,
                    "contentItems":[{"type":"inputText","text":result.to_string()},{"type":"inputImage","imageUrl":"data:image/png;base64,never-forward"}]
                }}),
            })).unwrap();
            let mut pane = crate::agent_pane::AgentPane::default();
            pane.event(event);
            let crate::agent_pane::RenderedBlock::Tool(trace) = &pane.blocks[0] else { panic!("tool row") };
            assert_eq!(trace.summary.as_deref(), Some(expected));
            let rendered = format!("{:?}", pane.blocks);
            for private in
                ["123456789", "987654321", "227.123456", "contentItems", "base64", "never-forward", "\"anchors\""]
            {
                assert!(!rendered.contains(private), "{name} leaked {private}");
            }
        }
    }

    // A5b-MULTI: exercise the real session-to-pane projection, not only demo events.
    #[test]
    fn multi_completion_projection_keeps_numeric_rows_but_discards_raw_tool_content() {
        let result = json!({"version":1,"timeframe":"1D","bars_used":120,"symbols":[
            {"symbol":"NVDA US Equity","return_percent":10.,"annualised_volatility_percent":20.,"max_drawdown_percent":-5.,"correlation":1.},
            {"symbol":"XLK US Equity","return_percent":5.,"correlation":0.8}],"private":"never-forward"});
        let event = project(BridgeEvent::Wire(WireEvent::Notification {method:"item/completed".into(),params:json!({"item":{
            "type":"dynamicToolCall","id":"multi","tool":"compare_symbols","success":true,
            "arguments":{"symbols":["NVDA US Equity","XLK US Equity"],"timeframe":"1D"},
            "contentItems":[{"type":"inputText","text":result.to_string()},{"type":"inputImage","imageUrl":"never-forward"}]
        }})})).unwrap();
        let mut pane = crate::agent_pane::AgentPane::default();
        pane.event(event);
        let crate::agent_pane::RenderedBlock::Tool(trace) = &pane.blocks[0] else { panic!("tool row") };
        assert_eq!(trace.summary.as_deref(), Some("Compared 2 symbols · 1D · 120 bars"));
        let projected = format!("{trace:?}");
        assert!(projected.contains("return_percent") && projected.contains("20.0"));
        assert!(!projected.contains("never-forward") && !projected.contains("contentItems"));
    }
    // End A5b-MULTI session test.

    #[test]
    fn failed_rectangle_projection_keeps_only_short_actionable_error_not_raw_content() {
        let event = project(BridgeEvent::Wire(WireEvent::Notification { method: "item/completed".into(), params: json!({"item": {
            "type":"dynamicToolCall", "id":"failed", "tool":"create_drawing", "success":false,
            "arguments":{"kind":"rectangle","zone_role":"support","anchors":[{"time":123456789,"price":987654321}]},
            "contentItems":[{"type":"inputText","text":json!({"error":{"code":"invalid_args","message":"anchors[1].time must be within the loaded range 2025-01-01–2026-01-01"}, "private":"never-forward"}).to_string()}]
        }}) })).unwrap();
        let mut pane = crate::agent_pane::AgentPane::default();
        pane.event(event);
        let crate::agent_pane::RenderedBlock::Tool(trace) = &pane.blocks[0] else { panic!() };
        assert_eq!(
            trace.error.as_deref(),
            Some("anchors[1].time must be within the loaded range 2025-01-01–2026-01-01")
        );
        assert_eq!(trace.summary.as_deref(), Some("support zone"));
        for private in ["never-forward", "123456789", "987654321", "contentItems"] {
            assert!(!format!("{trace:?}").contains(private));
        }
    }

    #[test]
    fn pane_session_start_options_keep_readonly_inventory_and_reject_edits_over_transport() {
        use tr_agent::bridge::{ToolCall, ToolExecutor, dynamic_tools};
        use tr_agent_tools::{Registry, ToolSet};
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/agent-wire-readonly");
        let app = crate::app::TerminalApp::test_workspace(&root);
        // Drop the inert app's worker runtime outside the async test runtime.
        tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut pane = crate::agent_pane::AgentPane::default();
        let tools = pane.prepare_session_start().unwrap();
        let options = bridge_options("empty-test-cwd".into(), tools);
        assert_eq!(options.tools, ToolSet::ReadOnly);
        let registry = Arc::new(Registry::new(options.tools));
        let inventory = dynamic_tools(&registry);
        let executor = AppExecutor::new(&app);
        let canonical = tr_agent::bridge::TurnIds::new().unwrap().allocate().unwrap();
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (read, write) = tokio::io::split(client_io);
        let client = CodexAppServer::new(Transport::connect(
            read,
            write,
            Arc::new(move |params| {
                let executor = executor.clone();
                let registry = registry.clone();
                let canonical = canonical.clone();
                Box::pin(async move {
                    let result = executor
                        .execute(
                            registry,
                            ToolCall {
                                turn: canonical,
                                pin: tr_agent::bridge::ChartPin {
                                    uid: 0,
                                    symbol: "AAA".into(),
                                    timeframe: "1D".into(),
                                    visible_range: None,
                                },
                                name: params["tool"].as_str().unwrap().into(),
                                arguments: params["arguments"].to_string(),
                                confirmed: false,
                            },
                        )
                        .await;
                    let content: Vec<_> = result
                        .content
                        .into_iter()
                        .map(|content| {
                            let tr_agent_tools::Content::Text(text) = content else {
                                panic!("ReadOnly rejection must be text")
                            };
                            json!({"type":"inputText","text":text})
                        })
                        .collect();
                    Ok(json!({"success":result.success,"contentItems":content}))
                })
            }),
            Arc::new(|| {}),
            None,
        ));
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server_io);
            let mut read = BufReader::new(read).lines();
            while let Some(line) = read.next_line().await.unwrap() {
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let result = match request["method"].as_str().unwrap_or_default() {
                    "initialized" => continue,
                    "initialize" => json!({}),
                    "skills/list" => {
                        json!({"data":[{"errors":[],"skills":tr_agent::codex::BUNDLED_SKILLS.iter().map(|name| json!({"name":name,"enabled":false})).collect::<Vec<_>>()}]})
                    }
                    "account/read" => json!({"account":{"type":"chatgpt"}}),
                    "thread/start" => {
                        let tools = request["params"]["dynamicTools"][0]["tools"].as_array().unwrap();
                        assert_eq!(tools.len(), Registry::new(ToolSet::ReadOnly).schemas().len());
                        assert!(
                            !tools.iter().any(|tool| crate::agent_pane::is_edit_tool(tool["name"].as_str().unwrap()))
                        );
                        json!({"thread":{"id":"thread"}})
                    }
                    "turn/start" => json!({"turn":{"id":"wire-turn"}}),
                    _ => panic!("unexpected request"),
                };
                write.write_all(format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes()).await.unwrap();
                if request["method"] == "turn/start" {
                    for name in [
                        "create_drawing",
                        "add_indicator",
                        "compare",
                        "set_view",
                        "update_drawing",
                        "delete_drawing",
                        "remove_indicator",
                    ] {
                        write
                            .write_all(
                                format!(
                                    "{}\n",
                                    json!({"id":900,"method":"item/tool/call","params":{
                                        "namespace":"chart","threadId":"thread","turnId":"wire-turn","tool":name,
                                        "arguments":{"code":"RSI","params":[14]}
                                    }})
                                )
                                .as_bytes(),
                            )
                            .await
                            .unwrap();
                        let reply: serde_json::Value =
                            serde_json::from_str(&read.next_line().await.unwrap().unwrap()).unwrap();
                        assert_eq!(reply["result"]["success"], false);
                        assert!(reply.to_string().contains("tool_disabled"), "{reply}");
                    }
                    return;
                }
            }
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            client.initialize().await.unwrap();
            client
                .start_thread(tr_agent::ThreadOptions {
                    cwd: options.cwd,
                    base_instructions: options.base_instructions,
                    ephemeral: true,
                    dynamic_tools: inventory,
                })
                .await
                .unwrap();
            client
                .send_turn(tr_agent::TurnInput {
                    model: "fake".into(),
                    text: "please edit".into(),
                    ..Default::default()
                })
                .await
                .unwrap();
            server.await.unwrap();
        })
        .await
        .unwrap();
        assert_eq!(app.agent_queue.len(), 0, "ReadOnly mutations must never reach UI work");
        client.shutdown();
        assert_eq!(bridge_options("empty".into(), ToolSet::Edit).tools, ToolSet::Edit);
        assert_eq!(bridge_options("empty".into(), ToolSet::Workflows).tools, ToolSet::Workflows);
        });
    }

    #[test]
    fn hidden_inbox_coalesces_text_without_scheduling_repaints() {
        let inbox = Inbox {
            events: Mutex::new(Vec::new()),
            visible: AtomicBool::new(false),
            ctx: egui::Context::default(),
            last_text_wake: Mutex::new(None),
        };
        for delta in ["a", "b"] {
            inbox.push(Event::Bridge(BridgeEvent::Wire(WireEvent::Notification {
                method: "item/agentMessage/delta".into(),
                params: json!({"delta":delta}),
            })));
        }
        assert!(inbox.last_text_wake.lock().unwrap().is_none());
        let events = inbox.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], Event::Bridge(BridgeEvent::Wire(WireEvent::Notification { params, .. })) if params["delta"] == "ab")
        );
    }

    #[tokio::test]
    async fn login_reopens_only_reply_url_cancels_and_scrubs_completion_failure() {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let (read, write) = tokio::io::split(client_io);
        let client = CodexAppServer::new(Transport::connect(
            read,
            write,
            Arc::new(|_| Box::pin(async { bail!("no login tools") })),
            Arc::new(|| {}),
            None,
        ));
        let inbox = Inbox {
            events: Mutex::new(Vec::new()),
            visible: AtomicBool::new(false),
            ctx: egui::Context::default(),
            last_text_wake: Mutex::new(None),
        };
        let (sender, mut receive) = mpsc::unbounded_channel();
        let server = async move {
            let (read, mut write) = tokio::io::split(server_io);
            let mut read = BufReader::new(read).lines();
            let mut accounts = 0;
            let mut starts = 0;
            while let Some(line) = read.next_line().await.unwrap() {
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "account/read" => {
                        accounts += 1;
                        json!({"account":null})
                    }
                    "account/login/start" => {
                        starts += 1;
                        json!({"type":"chatgpt","authUrl":format!("https://example.invalid/login/{starts}"),"loginId":format!("login-{starts}")})
                    }
                    "account/login/cancel" => {
                        assert_eq!(request["params"]["loginId"], "login-1");
                        json!({})
                    }
                    _ => panic!("unexpected login RPC"),
                };
                write.write_all(format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes()).await.unwrap();
                match request["method"].as_str().unwrap() {
                    "account/read" if accounts == 1 => sender.send(Command::SignIn).unwrap(),
                    "account/read" if accounts == 3 => return,
                    "account/login/start" if starts == 1 => {
                        sender.send(Command::ReopenLogin).unwrap();
                        sender.send(Command::CancelLogin).unwrap();
                    }
                    "account/login/start" => {
                        write.write_all(format!("{}\n", json!({"method":"account/login/completed","params":{"loginId":"login-2","success":false,"error":"expired; Bearer fixture-secret"}})).as_bytes()).await.unwrap();
                    }
                    "account/login/cancel" => sender.send(Command::SignIn).unwrap(),
                    _ => {}
                }
            }
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(onboarding(&mut receive, &inbox, &client), server)
        })
        .await
        .unwrap();
        assert!(!result.unwrap());
        let events = inbox.events.lock().unwrap();
        let urls: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::LoginUrl(url) => Some(url.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            urls,
            ["https://example.invalid/login/1", "https://example.invalid/login/1", "https://example.invalid/login/2"]
        );
        assert!(events.iter().any(|event| matches!(event, Event::State(Onboarding::SigningIn))));
        assert!(events.iter().any(|event| matches!(event, Event::Notice(reason) if reason.contains("expired") && reason.contains("[REDACTED]") && !reason.contains("fixture-secret"))));
        client.shutdown();
    }
}

#[cfg(test)]
mod fix_tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tr_agent::transport::{Transport, WireEvent};

    fn inbox() -> Inbox {
        Inbox {
            events: Mutex::new(Vec::new()),
            visible: AtomicBool::new(false),
            ctx: egui::Context::default(),
            last_text_wake: Mutex::new(None),
        }
    }

    #[test]
    fn ordinary_failures_offer_restart_and_only_reported_taint_is_sticky() {
        for reason in [
            "account/read network error",
            "rate_limits unavailable",
            "Check limits failed",
            "failed recovery attempt",
            "three consecutive recovery attempts",
        ] {
            let error = classify(Err(anyhow::anyhow!("{reason}; Bearer fixture-secret")), false).unwrap_err();
            let state = failure_state(error);
            assert!(
                matches!(&state, Onboarding::Stopped { unsafe_backend: false, reason } if !reason.contains("fixture-secret"))
            );
            assert_eq!(state.primary(), "Restart");
            let mut pane = crate::agent_pane::AgentPane::default();
            pane.transition(state);
            pane.transition(Onboarding::Checking);
            assert_eq!(pane.state, Onboarding::Checking);
        }
        let state = failure_state(classify(Err(anyhow::anyhow!("tripwire")), true).unwrap_err());
        assert!(matches!(state, Onboarding::Stopped { unsafe_backend: true, .. }));
        let mut pane = crate::agent_pane::AgentPane::default();
        pane.transition(state.clone());
        pane.transition(Onboarding::Checking);
        assert_eq!(pane.state, state);
    }

    #[test]
    fn binary_search_uses_injected_order_and_canonical_paths_without_executing() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target").join(format!(
            "bs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let search: Vec<_> =
            ["path", "homebrew", "usr-local", "local", "npm-global"].into_iter().map(|name| root.join(name)).collect();
        for directory in &search {
            std::fs::create_dir(directory).unwrap();
        }
        assert_eq!(find_binary(search.clone()), None);
        for directory in search.iter().rev() {
            let binary = directory.join("codex");
            std::fs::write(&binary, b"inert resolver fixture; never execute").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            assert_eq!(find_binary(search.clone()), Some(binary.canonicalize().unwrap()));
        }
        let found = find_binary(search.clone()).unwrap();
        let error = version_at_path(anyhow::Error::new(WrongVersion { found: "codex-cli 0.1.0".into() }), &found);
        assert!(matches!(failure_state(error), Onboarding::Unsupported { found: text }
            if text.contains("0.1.0") && text.contains(&scrub_text(found.to_str().unwrap()))));
        #[cfg(unix)]
        {
            std::fs::remove_file(search[0].join("codex")).unwrap();
            std::os::unix::fs::symlink(search[1].join("codex").canonicalize().unwrap(), search[0].join("codex"))
                .unwrap();
            assert_eq!(find_binary(search.clone()), Some(search[1].join("codex").canonicalize().unwrap()));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn owner_wait_is_bounded_and_detached_owner_still_cleans_up() {
        let (release, waiting) = std::sync::mpsc::channel();
        let (finished, completion) = std::sync::mpsc::channel();
        let (cleaned, cleanup) = std::sync::mpsc::channel();
        let owner = std::thread::spawn(move || {
            waiting.recv().unwrap();
            cleaned.send(()).unwrap();
            finished.send(()).unwrap();
        });
        let start = Instant::now();
        finish_owner(owner, &completion, Duration::from_millis(20));
        assert!(start.elapsed() < Duration::from_millis(500));
        assert!(cleanup.try_recv().is_err());
        release.send(()).unwrap();
        cleanup.recv_timeout(Duration::from_secs(2)).unwrap();
        completion.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn inbox_never_merges_distinct_message_items_or_crosses_tool_rows() {
        let inbox = inbox();
        for id in ["preamble", "preamble", "answer"] {
            inbox.push(Event::Bridge(BridgeEvent::Wire(WireEvent::Notification {
                method: "item/agentMessage/delta".into(),
                params: json!({"itemId":id,"delta":"x"}),
            })));
        }
        let events = inbox.events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert!(
            matches!(&events[0], Event::Bridge(BridgeEvent::Wire(WireEvent::Notification { params, .. })) if params["delta"] == "xx")
        );
        assert!(
            matches!(&events[1], Event::Bridge(BridgeEvent::Wire(WireEvent::Notification { params, .. })) if params["itemId"] == "answer" && params["delta"] == "x")
        );
    }

    #[tokio::test]
    async fn cancel_after_completed_login_checks_account_and_continues_without_notice() {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let (read, write) = tokio::io::split(client_io);
        let client = CodexAppServer::new(Transport::connect(
            read,
            write,
            Arc::new(|_| Box::pin(async { bail!("no login tools") })),
            Arc::new(|| {}),
            None,
        ));
        let inbox = inbox();
        let (sender, mut receive) = mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server_io);
            let mut read = BufReader::new(read).lines();
            let mut accounts = 0;
            while let Ok(Some(line)) = read.next_line().await {
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "initialized" => continue,
                    "initialize" => json!({}),
                    "skills/list" => json!({"data":[{"errors":[],"skills":tr_agent::codex::BUNDLED_SKILLS.iter()
                        .map(|name| json!({"name":name,"enabled":false})).collect::<Vec<_>>()}]}),
                    "account/read" => {
                        accounts += 1;
                        if accounts < 3 { json!({"account":null}) } else { json!({"account":{"type":"chatgpt"}}) }
                    }
                    "account/login/start" => {
                        json!({"type":"chatgpt","authUrl":"https://example.invalid/login","loginId":"login"})
                    }
                    "account/login/cancel" => {
                        write
                            .write_all(
                                format!(
                                    "{}\n",
                                    json!({"id":request["id"],"error":{"code":-32600,"message":"login id not found"}})
                                )
                                .as_bytes(),
                            )
                            .await
                            .unwrap();
                        continue;
                    }
                    "account/rateLimits/read" => json!({"ordinaryUsageAllowed":true}),
                    _ => panic!("unexpected RPC"),
                };
                write.write_all(format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes()).await.unwrap();
                match request["method"].as_str().unwrap() {
                    "account/read" if accounts == 2 => sender.send(Command::SignIn).unwrap(),
                    "account/login/start" => sender.send(Command::CancelLogin).unwrap(),
                    _ => {}
                }
            }
        });
        let ready = tokio::time::timeout(Duration::from_secs(2), async {
            client.initialize().await.unwrap();
            onboarding(&mut receive, &inbox, &client).await
        })
        .await
        .unwrap()
        .unwrap();
        assert!(ready);
        let events = inbox.events.lock().unwrap();
        assert!(events.iter().any(|e| matches!(e, Event::State(Onboarding::Checking))));
        assert!(!events.iter().any(|e| matches!(e, Event::Notice(_))));
        drop(events);
        client.shutdown();
        server.abort();
    }

    #[tokio::test]
    async fn successful_login_notification_replaces_cancel_before_account_rpc() {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let (read, write) = tokio::io::split(client_io);
        let client = CodexAppServer::new(Transport::connect(
            read,
            write,
            Arc::new(|_| Box::pin(async { bail!("no login tools") })),
            Arc::new(|| {}),
            None,
        ));
        let inbox = inbox();
        let (sender, mut receive) = mpsc::unbounded_channel();
        let server = async {
            let (read, mut write) = tokio::io::split(server_io);
            let mut read = BufReader::new(read).lines();
            let mut accounts = 0;
            while let Some(line) = read.next_line().await.unwrap() {
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                match request["method"].as_str().unwrap() {
                    "account/read" => {
                        accounts += 1;
                        if accounts == 2 {
                            assert!(matches!(
                                inbox.events.lock().unwrap().last(),
                                Some(Event::State(Onboarding::Checking))
                            ));
                            drop(sender);
                            write
                                .write_all(
                                    format!("{}\n", json!({"id":request["id"],"result":{"account":null}})).as_bytes(),
                                )
                                .await
                                .unwrap();
                            return;
                        }
                        write
                            .write_all(
                                format!("{}\n", json!({"id":request["id"],"result":{"account":null}})).as_bytes(),
                            )
                            .await
                            .unwrap();
                        sender.send(Command::SignIn).unwrap();
                    }
                    "account/login/start" => {
                        write.write_all(format!("{}\n", json!({"id":request["id"],"result":{"type":"chatgpt","authUrl":"https://example.invalid/login","loginId":"login"}})).as_bytes()).await.unwrap();
                        write.write_all(format!("{}\n", json!({"method":"account/login/completed","params":{"loginId":"login","success":true}})).as_bytes()).await.unwrap();
                    }
                    _ => panic!("unexpected RPC"),
                }
            }
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(onboarding(&mut receive, &inbox, &client), server)
        })
        .await
        .unwrap();
        assert!(!result.unwrap());
        client.shutdown();
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tr_agent::transport::Transport;

    /// The user clicks Cancel just as the browser sign-in finishes: Codex has already
    /// cleared its active login, so `account/login/cancel` answers "login id not found".
    #[tokio::test]
    async fn cancel_racing_completed_login_is_not_fatal() {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let (read, write) = tokio::io::split(client_io);
        let client = CodexAppServer::new(Transport::connect(
            read,
            write,
            Arc::new(|_| Box::pin(async { bail!("no login tools") })),
            Arc::new(|| {}),
            None,
        ));
        let inbox = Inbox {
            events: Mutex::new(Vec::new()),
            visible: AtomicBool::new(false),
            ctx: egui::Context::default(),
            last_text_wake: Mutex::new(None),
        };
        let (sender, mut receive) = mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let mut sender = Some(sender);
            let (read, mut write) = tokio::io::split(server_io);
            let mut read = BufReader::new(read).lines();
            let mut accounts = 0;
            while let Ok(Some(line)) = read.next_line().await {
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let reply = match request["method"].as_str().unwrap() {
                    "account/read" => {
                        accounts += 1;
                        if accounts == 2 {
                            sender.take();
                        }
                        json!({"id":request["id"],"result":{"account":null}})
                    }
                    "account/login/start" => {
                        json!({"id":request["id"],"result":{"type":"chatgpt","authUrl":"https://example.invalid/login","loginId":"login-1"}})
                    }
                    "account/login/cancel" => {
                        json!({"id":request["id"],"error":{"code":-32600,"message":"login id not found: login-1"}})
                    }
                    _ => continue,
                };
                write.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
                match request["method"].as_str().unwrap() {
                    "account/read" if accounts == 1 => sender.as_ref().unwrap().send(Command::SignIn).unwrap(),
                    "account/login/start" => sender.as_ref().unwrap().send(Command::CancelLogin).unwrap(),
                    _ => {}
                }
            }
        });
        let result =
            tokio::time::timeout(Duration::from_secs(3), onboarding(&mut receive, &inbox, &client)).await.unwrap();
        client.shutdown();
        server.abort();
        assert!(result.is_ok(), "cancel race became a fatal session error: {:#}", result.unwrap_err());
    }
}

/// Review N7 test support: an inert session with no backend owner thread, so
/// host-side session policy can be exercised without launching an agent.
#[cfg(test)]
impl Session {
    pub(crate) fn review_n7_inert() -> Self {
        let (sender, receive) = mpsc::unbounded_channel();
        std::mem::forget(receive);
        let (shutdown, stopping) = watch::channel(());
        std::mem::forget(stopping);
        let (_done, finished) = std::sync::mpsc::channel();
        let inbox = Arc::new(Inbox {
            events: Mutex::new(Vec::new()),
            visible: AtomicBool::new(true),
            ctx: egui::Context::default(),
            last_text_wake: Mutex::new(None),
        });
        Session { sender: Some(sender), inbox, shutdown, finished, owner: None }
    }
}
