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
    Turn(Box<BridgeTurn>),
    Interrupt,
}
// The URL is deliberately neither Debug nor serializable and is never stored in pane state.
pub enum Event {
    State(Onboarding),
    LoginUrl(String),
    Models(Vec<Model>),
    Limits(serde_json::Value),
    Bridge(BridgeEvent),
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
    pub fn start(ctx: egui::Context, executor: AppExecutor, parent: PathBuf) -> Self {
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
                runtime.block_on(owner(receive, stopping, &background, executor, parent))
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

async fn owner(
    mut receive: mpsc::UnboundedReceiver<Command>,
    mut stopping: watch::Receiver<()>,
    inbox: &Inbox,
    executor: AppExecutor,
    parent: PathBuf,
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
        let mut bridge = executor.start(connector, BridgeOptions::new(cwd)).await?;
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
    loop {
        tokio::select! {
            event = bridge.next_event() => {
                let event = event?;
                if let Some(event) = project(event) { inbox.push(Event::Bridge(event)); }
            }
            command = receive.recv() => match command {
                Some(Command::Turn(turn)) => {
                    if let Err(error) = bridge.send(*turn).await {
                        inbox.push(Event::Notice(scrub_text(&error.to_string())));
                        // Recovery is bridge-controlled; it never replays the failed turn.
                        let recovery = bridge.recover().await?;
                        inbox.push(Event::Bridge(BridgeEvent::Recovered(recovery)));
                    }
                }
                Some(Command::Interrupt) => { if let Err(error) = bridge.interrupt().await { inbox.push(Event::Notice(scrub_text(&error.to_string()))); } }
                Some(Command::RefreshLimits) => { inbox.state(Onboarding::Ready); inbox.limits(&bridge.client().rate_limits().await?); }
                Some(_) => {}
                None => return Ok(()),
            }
        }
    }
}

fn project(event: BridgeEvent) -> Option<BridgeEvent> {
    match event {
        BridgeEvent::Wire(tr_agent::transport::WireEvent::Notification { method, mut params }) => {
            // Only visible text, tool metadata, limits and failures wake the pane.
            match method.as_str() {
                "item/started" | "item/completed" if params["item"]["type"] == "dynamicToolCall" => {
                    if let Some(map) = params["item"].as_object_mut() {
                        map.retain(|key, _| {
                            matches!(key.as_str(), "id" | "type" | "tool" | "arguments" | "success" | "durationMs")
                        });
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
            assert_eq!(state.primary(), "Restart agent");
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
