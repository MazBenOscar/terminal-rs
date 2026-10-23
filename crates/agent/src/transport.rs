use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, mpsc, oneshot, watch};

use crate::transcript::Transcript;

pub const ALLOWED_ITEMS: &[&str] =
    &["agentMessage", "reasoning", "userMessage", "dynamicToolCall", "contextCompaction", "plan"];
const CAPACITY: usize = 128;
const MAX_LINE: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);
type Pending = HashMap<u64, oneshot::Sender<Result<Value>>>;
type ShutdownCheck = Box<dyn Fn() -> Result<()> + Send + Sync>;
pub type ToolFuture = Pin<Box<dyn Future<Output = Result<Value>> + Send>>;
pub type ToolHandler = Arc<dyn Fn(Value) -> ToolFuture + Send + Sync>;

/// Reviewed subset of ServerNotification in the pinned schema. No prefix matching.
pub const ALLOWED_NOTIFICATIONS: &[&str] = &[
    "error",
    "warning",
    "deprecationNotice",
    "configWarning",
    "thread/started",
    "thread/status/changed",
    "thread/readState/changed",
    "thread/archived",
    "thread/deleted",
    "thread/unarchived",
    "thread/closed",
    "thread/reverted",
    "thread/name/updated",
    "thread/settings/updated",
    "thread/tokenUsage/updated",
    "thread/compacted",
    "turn/started",
    "turn/completed",
    "turn/plan/updated",
    // Moderation verdict on the prompt; informational, never surfaced or acted on.
    "turn/moderationMetadata",
    "item/started",
    "item/completed",
    "item/agentMessage/delta",
    "item/plan/delta",
    "item/reasoning/textDelta",
    "item/reasoning/summaryTextDelta",
    "item/reasoning/summaryPartAdded",
    "account/updated",
    "account/gatewayOAuth/changed",
    "account/rateLimits/updated",
    "account/login/completed",
    "serverRequest/resolved",
];
const SERVER_REQUESTS: &[&str] = &[
    "item/tool/call",
    "item/tool/requestUserInput",
    "mcpServer/elicitation/request",
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "item/permissions/requestApproval",
    "applyPatchApproval",
    "execCommandApproval",
    "account/chatgptAuthTokens/refresh",
    "attestation/generate",
    "currentTime/read",
];

#[derive(Clone)]
pub enum WireEvent {
    Notification { method: String, params: Value },
    Error(String),
}

impl std::fmt::Debug for WireEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Notification { method, params } => {
                let mut safe = params.clone();
                crate::transcript::scrub_value(&mut safe);
                f.debug_struct("Notification")
                    .field("method", &crate::transcript::scrub_text(method))
                    .field("params", &safe)
                    .finish()
            }
            Self::Error(message) => f.debug_tuple("Error").field(&crate::transcript::scrub_text(message)).finish(),
        }
    }
}

#[derive(Default)]
struct EventQueue {
    queue: Mutex<VecDeque<WireEvent>>,
    ready: Notify,
    dropped: AtomicU64,
}

impl EventQueue {
    fn push(&self, event: WireEvent) {
        let mut queue = self.queue.lock().unwrap();
        if queue.len() >= CAPACITY {
            if let WireEvent::Notification { method, params } = &event
                && matches!(
                    method.as_str(),
                    "item/agentMessage/delta" | "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta"
                )
                && let Some(WireEvent::Notification { method: last_method, params: last }) = queue.back_mut()
                && last_method == method
                && ["threadId", "turnId", "itemId", "contentIndex", "summaryIndex"]
                    .iter()
                    .all(|key| last[key] == params[key])
                && let (Some(old), Some(new)) = (last["delta"].as_str(), params["delta"].as_str())
                && old.len() + new.len() <= 64 * 1024
            {
                last["delta"] = Value::String(format!("{old}{new}"));
                self.ready.notify_one();
                return;
            }
            queue.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        queue.push_back(event);
        self.ready.notify_one();
    }
}

struct Outbound {
    value: Value,
    stop_after: Option<String>,
}

struct Shared {
    next_id: AtomicU64,
    pending: Mutex<Pending>,
    output: mpsc::Sender<Outbound>,
    events: Mutex<Vec<std::sync::Weak<EventQueue>>>,
    terminal: watch::Sender<Option<String>>,
    unsafe_backend: AtomicBool,
    bridge_safety: Mutex<Option<Arc<AtomicBool>>>,
    cleanup: watch::Sender<Option<std::result::Result<(), String>>>,
    stopped: AtomicBool,
    kill: Arc<dyn Fn() + Send + Sync>,
    transcript: Option<Transcript>,
    tool_cancel: watch::Sender<u64>,
    login_only: Mutex<Option<bool>>,
    shutdown_check: Mutex<Option<ShutdownCheck>>,
}

impl Shared {
    fn taint(&self) {
        self.unsafe_backend.store(true, Ordering::SeqCst);
        if let Some(safety) = self.bridge_safety.lock().unwrap().as_ref() {
            safety.store(true, Ordering::SeqCst);
        }
    }

    fn emit(&self, event: WireEvent) {
        self.events.lock().unwrap().retain(|subscriber| {
            if let Some(queue) = subscriber.upgrade() {
                queue.push(event.clone());
                true
            } else {
                false
            }
        });
    }

    fn fail(&self, message: &str) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        (self.kill)();
        let mut message = crate::transcript::scrub_text(message);
        if let Some(check) = self.shutdown_check.lock().unwrap().as_ref()
            && let Err(error) = check()
        {
            self.taint();
            message.push_str(&format!("; cleanup findings: {}", crate::transcript::scrub_text(&error.to_string())));
        }
        for (_, sender) in self.pending.lock().unwrap().drain() {
            let _ = sender.send(Err(anyhow!(message.to_owned())));
        }
        self.terminal.send_replace(Some(message.to_owned()));
        self.emit(WireEvent::Error(message));
    }

    fn enqueue(&self, value: Value, stop_after: Option<String>) -> Result<()> {
        if self.stopped.load(Ordering::SeqCst) {
            bail!("app-server stopped");
        }
        if self.output.try_send(Outbound { value, stop_after }).is_err() {
            self.fail("app-server output queue overflow");
            bail!("app-server output queue overflow");
        }
        Ok(())
    }

    fn record(&self, value: &Value) -> Result<()> {
        if let Some(transcript) = &self.transcript {
            transcript.record(value)?;
        }
        Ok(())
    }

    // A violation immediately follows these replies. If the writer is blocked,
    // preserve the finding rather than replacing it with a generic queue error.
    fn security_reply(&self, value: Value) {
        let _ = self.output.try_send(Outbound { value, stop_after: None });
    }

    fn violation(&self, params: &Value, message: String) {
        let already_stopped = self.stopped.load(Ordering::SeqCst);
        self.taint();
        self.tool_cancel.send_modify(|generation| *generation += 1);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let packet = Outbound {
            value: json!({"id": id, "method": "turn/interrupt", "params": {
                "threadId": params["threadId"], "turnId": params["turnId"]
            }}),
            stop_after: Some(message.clone()),
        };
        if self.output.try_send(packet).is_err() {
            // Do not replace the security finding with a generic queue error.
            self.fail(&message);
        }
        if already_stopped {
            let message = crate::transcript::scrub_text(&message);
            self.terminal.send_replace(Some(message.clone()));
            self.emit(WireEvent::Error(message));
        }
    }
}

pub struct Transport {
    shared: Arc<Shared>,
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.shared.fail("app-server dropped");
    }
}

pub struct EventStream {
    queue: Arc<EventQueue>,
    terminal: watch::Receiver<Option<String>>,
    ended: bool,
}

impl EventStream {
    /// Number of events lost to this consumer's bounded queue (coalescing is lossless).
    pub fn dropped_events(&self) -> u64 {
        self.queue.dropped.load(Ordering::Relaxed)
    }

    pub async fn recv(&mut self) -> Option<WireEvent> {
        loop {
            if self.ended {
                return None;
            }
            let ready = self.queue.ready.notified();
            if let Some(event) = self.queue.queue.lock().unwrap().pop_front() {
                self.ended = matches!(event, WireEvent::Error(_));
                return Some(event);
            }
            if let Some(message) = self.terminal.borrow().clone() {
                self.ended = true;
                return Some(WireEvent::Error(message));
            }
            tokio::select! {
                _ = ready => {},
                _ = self.terminal.changed() => {},
            }
        }
    }
}

impl Transport {
    pub fn connect<R, W>(
        read: R,
        write: W,
        handler: ToolHandler,
        kill: Arc<dyn Fn() + Send + Sync>,
        transcript: Option<Transcript>,
    ) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (output, receiver) = mpsc::channel(CAPACITY);
        let (terminal, _) = watch::channel(None);
        let shared = Arc::new(Shared {
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            output,
            events: Mutex::new(Vec::new()),
            terminal,
            unsafe_backend: AtomicBool::new(false),
            bridge_safety: Mutex::new(None),
            // Duplex transports have no child to reap. Isolated sessions arm this
            // before returning the transport to their caller.
            cleanup: watch::channel(Some(Ok(()))).0,
            stopped: AtomicBool::new(false),
            kill,
            transcript,
            tool_cancel: watch::channel(0).0,
            login_only: Mutex::new(None),
            shutdown_check: Mutex::new(None),
        });
        tokio::spawn(write_loop(write, receiver, shared.clone()));
        tokio::spawn(read_loop(read, shared.clone(), handler));
        Self { shared }
    }

    pub fn events(&self) -> EventStream {
        let queue = Arc::new(EventQueue::default());
        self.shared.events.lock().unwrap().push(Arc::downgrade(&queue));
        EventStream { queue, terminal: self.shared.terminal.subscribe(), ended: false }
    }

    pub fn is_unsafe(&self) -> bool {
        self.shared.unsafe_backend.load(Ordering::SeqCst)
    }

    pub fn cancel_tools(&self) {
        self.shared.tool_cancel.send_modify(|generation| *generation += 1);
    }

    pub fn shutdown(&self) {
        self.shared.fail("app-server shutdown");
    }

    pub fn child_exited(&self) -> impl Fn() + Send + 'static {
        let shared = self.shared.clone();
        move || shared.fail("app-server child exited")
    }

    pub fn closed(&self) -> watch::Receiver<Option<String>> {
        self.shared.terminal.subscribe()
    }

    pub fn notify(&self, method: &str) -> Result<()> {
        self.shared.enqueue(json!({"method": method}), None)
    }

    pub(crate) fn set_shutdown_check(&self, check: impl Fn() -> Result<()> + Send + Sync + 'static) {
        *self.shared.shutdown_check.lock().unwrap() = Some(Box::new(check));
    }

    pub(crate) fn set_login_only(&self, login: bool) {
        *self.shared.login_only.lock().unwrap() = Some(login);
    }

    pub(crate) fn bind_bridge_safety(&self, safety: Arc<AtomicBool>) {
        let mut observer = self.shared.bridge_safety.lock().unwrap();
        if self.is_unsafe() {
            safety.store(true, Ordering::SeqCst);
        }
        *observer = Some(safety);
    }

    pub(crate) fn stop(&self, reason: &str) {
        self.shared.fail(reason);
    }

    /// The supervisor reports even successful cleanup, after wait() and the final
    /// process/home checks. A timer is not evidence that the old child is gone.
    pub(crate) fn cleanup_reporter(&self) -> impl Fn(std::result::Result<(), String>) + Send + 'static {
        self.shared.cleanup.send_replace(None);
        let shared = self.shared.clone();
        move |result| {
            if let Err(message) = &result {
                shared.taint();
                let message = crate::transcript::scrub_text(message);
                shared.terminal.send_replace(Some(message.clone()));
                shared.emit(WireEvent::Error(message));
            }
            shared.cleanup.send_replace(Some(result));
        }
    }

    pub(crate) async fn wait_cleanup(&self) -> Result<()> {
        let mut cleanup = self.shared.cleanup.subscribe();
        loop {
            if let Some(result) = cleanup.borrow().clone() {
                return result.map_err(anyhow::Error::msg);
            }
            cleanup.changed().await?;
        }
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        match *self.shared.login_only.lock().unwrap() {
            Some(true) if method.starts_with("thread/") || method.starts_with("turn/") => {
                bail!("login session cannot run agent work; restart in normal mode")
            }
            Some(false) if method.starts_with("account/login/") => bail!("login requires a separate login session"),
            _ => {}
        }
        if self.is_unsafe() {
            bail!("backend unsafe; restart required");
        }
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().unwrap().insert(id, tx);
        let _cleanup = PendingGuard { id, shared: self.shared.clone() };
        self.shared.enqueue(json!({"id": id, "method": method, "params": params}), None)?;
        match tokio::time::timeout(TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(anyhow!("app-server response channel closed")),
            Err(_) => Err(anyhow!("app-server request timed out")),
        }
    }
}

struct PendingGuard {
    id: u64,
    shared: Arc<Shared>,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.shared.pending.lock().unwrap().remove(&self.id);
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(mut write: W, mut input: mpsc::Receiver<Outbound>, shared: Arc<Shared>) {
    let mut terminal = shared.terminal.subscribe();
    loop {
        if shared.stopped.load(Ordering::SeqCst) {
            return;
        }
        let packet = tokio::select! {
            _ = terminal.changed() => break,
            packet = input.recv() => match packet { Some(packet) => packet, None => break },
        };
        let result = async {
            shared.record(&packet.value)?;
            let mut bytes = serde_json::to_vec(&packet.value)?;
            bytes.push(b'\n');
            write.write_all(&bytes).await?;
            write.flush().await?;
            Ok::<_, anyhow::Error>(())
        };
        if !matches!(tokio::time::timeout(Duration::from_secs(1), result).await, Ok(Ok(()))) {
            shared.fail(packet.stop_after.as_deref().unwrap_or("app-server write failed"));
            break;
        }
        if let Some(message) = packet.stop_after {
            shared.fail(&message);
            break;
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin>(read: R, shared: Arc<Shared>, handler: ToolHandler) {
    let mut reader = BufReader::new(read);
    let mut terminal = shared.terminal.subscribe();
    let slots = Arc::new(tokio::sync::Semaphore::new(16));
    loop {
        if shared.stopped.load(Ordering::SeqCst) {
            return;
        }
        let mut line = Vec::new();
        // Limit allocation even when a peer never sends a newline.
        let read = async {
            loop {
                let buffer = reader.fill_buf().await?;
                if buffer.is_empty() {
                    return Ok::<_, std::io::Error>(());
                }
                let count = buffer.iter().position(|b| *b == b'\n').map_or(buffer.len(), |n| n + 1);
                if line.len() + count > MAX_LINE {
                    return Err(std::io::Error::other("line too large"));
                }
                let complete = buffer[count - 1] == b'\n';
                line.extend_from_slice(&buffer[..count]);
                reader.consume(count);
                if complete {
                    return Ok(());
                }
            }
        };
        let result = tokio::select! { _ = terminal.changed() => return, result = read => result };
        if result.is_err() || line.is_empty() {
            shared.fail("app-server exited or read failed");
            return;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            shared.fail("app-server invalid JSON");
            return;
        };
        if shared.record(&value).is_err() {
            shared.fail("transcript recorder failed or overflowed");
            return;
        }
        if !route(value, &shared, &handler, &slots) {
            return;
        }
    }
}

fn route(value: Value, shared: &Arc<Shared>, handler: &ToolHandler, slots: &Arc<tokio::sync::Semaphore>) -> bool {
    let method_name = value["method"].as_str().unwrap_or("<response>");
    if value.get("method").is_some() && !value["method"].is_string() {
        shared.violation(&value["params"], "unsafe non-string notification method".into());
        return false;
    }
    if let Some(kind) = forbidden_item(&value) {
        if let Some(id) = value.get("id")
            && value.get("method").is_some()
        {
            shared.security_reply(json!({"id":id,"error":{"code":-32600,"message":"Unsafe item payload"}}));
        }
        shared.violation(&value["params"], format!("unsafe item type in {method_name}: {kind}"));
        return false;
    }
    let Some(method) = value["method"].as_str() else {
        if value.get("id").is_none() || (value.get("result").is_some() == value.get("error").is_some()) {
            shared.violation(&Value::Null, "unsafe malformed response envelope".into());
            return false;
        }
        if let Some(id) = value["id"].as_u64()
            && let Some(sender) = shared.pending.lock().unwrap().remove(&id)
        {
            let result = if value.get("error").is_some() {
                Err(anyhow!("app-server rejected request"))
            } else {
                Ok(value["result"].clone())
            };
            let _ = sender.send(result);
        }
        return true;
    };
    let params = value["params"].clone();
    // Approval-shaped messages are always violations, even when they are not
    // schema requests (for example a future method, or one without an id).
    if method.ends_with("requestApproval") || matches!(method, "applyPatchApproval" | "execCommandApproval") {
        if let Some(id) = value.get("id") {
            let result = if method == "item/permissions/requestApproval" {
                json!({"permissions": {}, "scope": "turn"})
            } else if matches!(method, "applyPatchApproval" | "execCommandApproval") {
                json!({"decision": "denied"})
            } else {
                json!({"decision": "decline"})
            };
            shared.security_reply(json!({"id": id, "result": result}));
        }
        shared.violation(&params, format!("unsafe approval method: {method}"));
        return false;
    }
    if let Some(id) = value.get("id")
        && SERVER_REQUESTS.contains(&method)
    {
        if method != "item/tool/call" {
            shared
                .security_reply(json!({"id": id, "error": {"code": -32601, "message": "Unsupported server request"}}));
            shared.violation(&params, format!("unsafe server request: {method}"));
            return false;
        }
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            shared.fail("tool handler queue overflow");
            return false;
        };
        let shared = shared.clone();
        let handler = handler.clone();
        let id = id.clone();
        let mut cancelled = shared.tool_cancel.subscribe();
        tokio::spawn(async move {
            if shared.stopped.load(Ordering::SeqCst) {
                return;
            }
            let _permit = permit;
            let mut terminal = shared.terminal.subscribe();
            // Invocation itself can panic as well as the returned future. Join errors
            // become unsuccessful replies, and the abort guard covers cancellation.
            let mut task = tokio::spawn(async move { handler(params).await });
            let abort = AbortTool(task.abort_handle());
            let result = tokio::select! {
                _ = terminal.changed() => return,
                _ = cancelled.changed() => None,
                result = tokio::time::timeout(TIMEOUT, &mut task) => result.ok().and_then(Result::ok).and_then(Result::ok),
            };
            drop(abort);
            let result = match result {
                Some(result) if valid_tool_result(&result) => result,
                _ => {
                    json!({"success": false, "contentItems": [{"type": "inputText", "text": "Tool failed or timed out"}]})
                }
            };
            let _ = shared.enqueue(json!({"id": id, "result": result}), None);
        });
        return true;
    }
    // Codex announces remote control at startup; only the disabled state is acceptable,
    // since an enabled or paired state lets a remote client drive this app-server.
    if method == "remoteControl/status/changed" {
        if params["status"].as_str() != Some("disabled") {
            shared.violation(&params, "remote control is not disabled".to_owned());
            return false;
        }
        return true;
    }
    if !ALLOWED_NOTIFICATIONS.contains(&method) {
        if let Some(id) = value.get("id") {
            shared.security_reply(json!({"id":id,"error":{"code":-32601,"message":"Unsupported notification"}}));
        }
        shared.violation(&params, format!("unsafe notification method: {method}"));
        return false;
    }
    if matches!(method, "item/started" | "item/completed") {
        let kind = params["item"]["type"].as_str().unwrap_or("<missing>");
        if !ALLOWED_ITEMS.contains(&kind) {
            shared.violation(&params, format!("unsafe item type in {method}: {kind}"));
            return false;
        }
    }
    shared.emit(WireEvent::Notification { method: method.to_owned(), params });
    true
}

struct AbortTool(tokio::task::AbortHandle);
impl Drop for AbortTool {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn forbidden_item(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            for (key, entry) in map {
                if key == "item" {
                    let kind = entry["type"].as_str().unwrap_or("<missing>");
                    if !ALLOWED_ITEMS.contains(&kind) {
                        return Some(kind.to_owned());
                    }
                }
                if key == "items"
                    && let Some(items) = entry.as_array()
                {
                    for item in items {
                        let kind = item["type"].as_str().unwrap_or("<missing>");
                        if !ALLOWED_ITEMS.contains(&kind) {
                            return Some(kind.to_owned());
                        }
                    }
                }
                if let Some(kind) = forbidden_item(entry) {
                    return Some(kind);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(forbidden_item),
        _ => None,
    }
}

/// Strict RFC 4648 base64, including canonical padding bits. No URL fetching.
fn image_bytes(url: &str) -> Option<usize> {
    let data =
        url.strip_prefix("data:image/png;base64,").or_else(|| url.strip_prefix("data:image/jpeg;base64,"))?.as_bytes();
    if data.is_empty() || data.len() % 4 != 0 {
        return None;
    }
    let padding = data.iter().rev().take_while(|byte| **byte == b'=').count();
    if padding > 2 {
        return None;
    }
    let payload = &data[..data.len() - padding];
    let digit = |byte| match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    if payload.iter().any(|byte| digit(*byte).is_none()) {
        return None;
    }
    let last = digit(*payload.last()?)?;
    if (padding == 1 && last & 3 != 0) || (padding == 2 && last & 15 != 0) {
        return None;
    }
    Some(data.len() / 4 * 3 - padding)
}

pub const MAX_TOOL_IMAGE_BYTES: usize = 4 * 1024 * 1024;
fn valid_tool_result(value: &Value) -> bool {
    let Some(items) = value["contentItems"].as_array() else {
        return false;
    };
    if !value["success"].is_boolean() {
        return false;
    }
    let (mut text, mut images) = (0usize, 0usize);
    for item in items {
        match item["type"].as_str() {
            Some("inputText") => {
                let Some(content) = item["text"].as_str() else {
                    return false;
                };
                text = text.saturating_add(content.len());
            }
            Some("inputImage") => {
                let Some(size) = item["imageUrl"].as_str().and_then(image_bytes) else {
                    return false;
                };
                images = images.saturating_add(size);
            }
            _ => return false,
        }
    }
    text <= 16 * 1024 && images <= MAX_TOOL_IMAGE_BYTES
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tokio::io::{DuplexStream, split};

    pub(crate) fn fake(
        handler: ToolHandler,
    ) -> (Transport, BufReader<tokio::io::ReadHalf<DuplexStream>>, tokio::io::WriteHalf<DuplexStream>, Arc<AtomicBool>)
    {
        let (client, server) = tokio::io::duplex(65536);
        let (read, write) = split(client);
        let (server_read, server_write) = split(server);
        let killed = Arc::new(AtomicBool::new(false));
        let flag = killed.clone();
        let transport = Transport::connect(
            read,
            write,
            handler,
            Arc::new(move || {
                flag.store(true, Ordering::SeqCst);
            }),
            None,
        );
        (transport, BufReader::new(server_read), server_write, killed)
    }

    pub(crate) fn no_tools() -> ToolHandler {
        Arc::new(|_| Box::pin(async { bail!("no tools") }))
    }
    pub(crate) async fn receive<R: AsyncRead + Unpin>(read: &mut BufReader<R>) -> Value {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), read.read_line(&mut line)).await.unwrap().unwrap();
        serde_json::from_str(&line).unwrap()
    }
    pub(crate) async fn send<W: AsyncWrite + Unpin>(write: &mut W, value: Value) {
        write.write_all(format!("{value}\n").as_bytes()).await.unwrap();
    }

    #[test]
    fn image_validation_and_independent_content_budgets() {
        for url in ["data:image/png;base64,AA==", "data:image/jpeg;base64,AAA=", "data:image/png;base64,AAAA"] {
            assert!(image_bytes(url).is_some(), "{url}");
        }
        for url in [
            "data:image/svg+xml;base64,AAAA",
            "data:text/html;base64,AAAA",
            "file:///x",
            "https://x/",
            "data:image/png;base64,",
            "data:image/png;base64,a",
            "data:image/png;base64,AB==",
            "data:image/png;base64,AAB=",
            "data:image/png;base64,AA=A",
            "data:image/png;base64,AA\n=",
            "data:image/png;base64,!!!!",
        ] {
            assert!(image_bytes(url).is_none(), "{url}");
        }
        let image = format!("data:image/png;base64,{}", "AAAA".repeat(MAX_TOOL_IMAGE_BYTES / 3));
        let mut result = json!({"success":true,"contentItems":[{"type":"inputText","text":"x".repeat(16*1024)}, {"type":"inputImage","imageUrl":image}]});
        assert!(valid_tool_result(&result));
        result["contentItems"][0]["text"] = json!("x".repeat(16 * 1024 + 1));
        assert!(!valid_tool_result(&result));
        result["contentItems"][0]["text"] = json!("");
        result["contentItems"][1]["imageUrl"] = json!(format!("{image}AAAA"));
        assert!(!valid_tool_result(&result));
    }

    #[tokio::test]
    async fn all_item_carriers_and_unknown_notifications_trip() {
        for message in [
            json!({"method":"item/updated","params":{"item":{"type":"commandExecution"}}}),
            json!({"method":"thread/started","params":{"thread":{"turns":[{"items":[{"type":"fileChange"}]}]}}}),
            json!({"id":999,"result":{"item":{"type":"mcpToolCall"}}}),
            json!({"method":"item/agentMessage/delta","params":{"item":{"type":"future"}}}),
            json!({"method":"future/notification","params":{}}),
            json!({"method":42,"params":{}}),
            json!({}),
            json!(null),
        ] {
            let (client, mut read, mut write, killed) = fake(no_tools());
            let mut events = client.events();
            send(&mut write, message).await;
            assert_eq!(receive(&mut read).await["method"], "turn/interrupt");
            assert!(matches!(events.recv().await, Some(WireEvent::Error(_))));
            assert!(client.is_unsafe() && killed.load(Ordering::SeqCst));
        }
        let (client, _read, mut write, killed) = fake(no_tools());
        let mut events = client.events();
        send(&mut write, json!({"id":null,"method":"item/started","params":{"item":{"type":"agentMessage"}}})).await;
        assert!(
            matches!(events.recv().await, Some(WireEvent::Notification { method, .. }) if method == "item/started")
        );
        assert!(!client.is_unsafe() && !killed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn handler_invocation_panic_does_not_end_the_turn() {
        let handler: ToolHandler = Arc::new(|_| panic!("synchronous handler invocation panic"));
        let (client, mut read, mut write, killed) = fake(handler);
        send(&mut write, json!({"id":11,"method":"item/tool/call","params":{}})).await;
        assert_eq!(receive(&mut read).await["result"]["success"], false);
        let server = async {
            let request = receive(&mut read).await;
            send(&mut write, json!({"id":request["id"],"result":"still running"})).await;
        };
        let (result, ()) = tokio::join!(client.request("model/list", json!({})), server);
        assert_eq!(result.unwrap(), "still running");
        assert!(!client.is_unsafe() && !killed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn session_modes_and_cleanup_findings_are_enforced() {
        let (client, _read, _write, _) = fake(no_tools());
        client.set_login_only(true);
        assert!(client.request("thread/start", json!({})).await.is_err());
        assert!(client.request("turn/start", json!({})).await.is_err());
        client.set_login_only(false);
        assert!(client.request("account/login/start", json!({})).await.is_err());
        client.set_shutdown_check(|| bail!("descendant survivors after shutdown: [123]"));
        let mut events = client.events();
        client.shutdown();
        assert!(
            matches!(events.recv().await, Some(WireEvent::Error(message)) if message.contains("descendant survivors"))
        );
    }

    #[test]
    fn event_debug_redacts_nested_params_and_errors() {
        let params = json!({"nested":[{"password":"hunter2","cookie":"x","secret":{"value":"hidden"},"api_key":"short"}],"text":"Authorization: Basic dXNlcjpwYXNz"});
        let outputs = [
            format!(
                "{:?}",
                WireEvent::Notification { method: "item/agentMessage/delta".into(), params: params.clone() }
            ),
            format!("{:?}", crate::AgentEvent::TextDelta(params)),
            format!("{:?}", WireEvent::Error("password=hunter2".into())),
            format!("{:?}", crate::AgentEvent::Error("cookie=x".into())),
        ];
        for output in outputs {
            for secret in ["hunter2", "hidden", "short", "dXNlcjpwYXNz", "cookie=x"] {
                assert!(!output.contains(secret), "{output}");
            }
        }
    }

    #[test]
    fn tool_results_are_shape_checked_and_bounded() {
        assert!(valid_tool_result(&json!({"success":false,"contentItems":[]})));
        assert!(!valid_tool_result(&json!({"success":true,"contentItems":[{"type":"inputAudio","audioUrl":"x"}]})));
        assert!(!valid_tool_result(
            &json!({"success":true,"contentItems":[{"type":"inputText","text":"x".repeat(17000)}]})
        ));
    }

    #[tokio::test]
    async fn routes_interleaved_responses_and_notifications() {
        let (client, mut read, mut write, _) = fake(no_tools());
        let mut events = client.events();
        let server = async {
            let first = receive(&mut read).await;
            let second = receive(&mut read).await;
            assert!(first.get("jsonrpc").is_none());
            send(&mut write, json!({"method":"turn/completed","params":{}})).await;
            send(&mut write, json!({"id":second["id"],"result":2})).await;
            send(&mut write, json!({"id":first["id"],"result":1})).await;
        };
        let (a, b, ()) = tokio::join!(client.request("a", json!({})), client.request("b", json!({})), server);
        assert_eq!(a.unwrap(), 1);
        assert_eq!(b.unwrap(), 2);
        assert!(matches!(events.recv().await, Some(WireEvent::Notification { .. })));
    }

    #[tokio::test]
    async fn tool_round_trip_text_and_image_uses_request_id() {
        let handler: ToolHandler = Arc::new(|_| {
            Box::pin(async {
                Ok(json!({"success":true,"contentItems":[
                    {"type":"inputText","text":"bars"}, {"type":"inputImage","imageUrl":"data:image/png;base64,AA=="}
                ]}))
            })
        });
        let (_client, mut read, mut write, _) = fake(handler);
        send(&mut write, json!({"id":"rpc-42","method":"item/tool/call","params":{"callId":"different"}})).await;
        let reply = receive(&mut read).await;
        assert_eq!(reply["id"], "rpc-42");
        assert_eq!(reply["result"]["contentItems"][1]["type"], "inputImage");
    }

    #[tokio::test]
    async fn tripwire_kills_and_stays_unsafe() {
        for kind in ["commandExecution", "fileChange", "mcpToolCall", "webSearch", "unknown"] {
            let (client, mut read, mut write, killed) = fake(no_tools());
            let mut events = client.events();
            send(
                &mut write,
                json!({"method":"item/started","params":{"threadId":"t","turnId":"u","item":{"type":kind}}}),
            )
            .await;
            assert_eq!(receive(&mut read).await["method"], "turn/interrupt");
            let Some(WireEvent::Error(message)) = events.recv().await else { panic!("missing error") };
            assert!(message.contains(kind));
            assert!(killed.load(Ordering::SeqCst));
            assert!(client.is_unsafe());
            assert!(client.request("model/list", json!({})).await.is_err());
        }
    }

    #[tokio::test]
    async fn approvals_declined_and_violate() {
        for method in [
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
            "item/permissions/requestApproval",
        ] {
            let (client, mut read, mut write, killed) = fake(no_tools());
            let mut events = client.events();
            send(&mut write, json!({"id":99,"method":method,"params":{"threadId":"t","turnId":"u"}})).await;
            let reply = receive(&mut read).await;
            assert_eq!(reply["id"], 99);
            if method.contains("permissions") {
                assert_eq!(reply["result"]["permissions"], json!({}));
            } else {
                assert_eq!(reply["result"]["decision"], "decline");
            }
            assert_eq!(receive(&mut read).await["method"], "turn/interrupt");
            assert!(matches!(events.recv().await, Some(WireEvent::Error(_))));
            assert!(client.is_unsafe());
            assert!(killed.load(Ordering::SeqCst));
        }
    }

    #[tokio::test]
    async fn crash_fails_pending_requests() {
        let (client, mut read, write, _) = fake(no_tools());
        let mut events = client.events();
        let server = async {
            receive(&mut read).await;
            drop(write);
            drop(read);
        };
        let (result, ()) = tokio::join!(client.request("model/list", json!({})), server);
        assert!(result.is_err());
        assert!(matches!(events.recv().await, Some(WireEvent::Error(_))));
    }

    #[tokio::test]
    async fn interrupt_cancels_in_flight_tool_handlers() {
        let (started, mut observed) = mpsc::channel(1);
        let handler: ToolHandler = Arc::new(move |_| {
            let started = started.clone();
            Box::pin(async move {
                let _ = started.send(()).await;
                std::future::pending().await
            })
        });
        let (client, mut read, mut write, _) = fake(handler);
        send(&mut write, json!({"id":7,"method":"item/tool/call","params":{}})).await;
        observed.recv().await.unwrap();
        client.cancel_tools();
        let reply = receive(&mut read).await;
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["result"]["success"], false);
    }

    #[tokio::test]
    async fn malformed_input_fails_pending_and_emits_error() {
        let (client, mut read, mut write, killed) = fake(no_tools());
        let mut events = client.events();
        let server = async {
            receive(&mut read).await;
            write.write_all(b"not-json\n").await.unwrap();
        };
        let (result, ()) = tokio::join!(client.request("model/list", json!({})), server);
        assert!(result.is_err());
        assert!(matches!(events.recv().await, Some(WireEvent::Error(_))));
        assert!(killed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn output_overflow_and_drop_kill_the_child() {
        let (client, _read, _write, killed) = fake(no_tools());
        for _ in 0..CAPACITY {
            client.notify("initialized").unwrap();
        }
        assert!(client.notify("initialized").is_err());
        assert!(killed.load(Ordering::SeqCst));
        let (client, _read, _write, killed) = fake(no_tools());
        drop(client);
        assert!(killed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn event_overflow_coalesces_without_killing_and_counts_drops() {
        let (client, _read, mut write, killed) = fake(no_tools());
        let mut events = client.events();
        for _ in 0..CAPACITY + 1 {
            send(&mut write, json!({"method":"item/agentMessage/delta","params":{"delta":"x"}})).await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut text = String::new();
        for _ in 0..CAPACITY {
            let Some(WireEvent::Notification { params, .. }) = events.recv().await else { panic!("missing delta") };
            text.push_str(params["delta"].as_str().unwrap());
        }
        assert_eq!(text.len(), CAPACITY + 1);
        assert_eq!(events.dropped_events(), 0);
        for _ in 0..CAPACITY + 1 {
            send(&mut write, json!({"method":"thread/tokenUsage/updated","params":{}})).await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(events.dropped_events(), 1);
        assert!(!killed.load(Ordering::SeqCst));
        // Even a full, lagging subscriber cannot delay security processing.
        send(&mut write, json!({"method":"future/unknown","params":{}})).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(client.is_unsafe() && killed.load(Ordering::SeqCst));
    }
}
