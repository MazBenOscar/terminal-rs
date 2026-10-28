//! Bounded JSON-RPC MCP stdio shim and owner-only app socket. No HTTP listener.
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::transport::ToolHandler;

pub const MAX_LINE: usize = 1024 * 1024;
pub const UNAVAILABLE: &str = "terminal-rs is not running or external agents are disabled (Settings › Agent)";
pub const SOURCE: &str = "External agent";

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;

/// Resolve once before startup threads; creating first also supports a new,
/// relative TR_DATA_DIR and canonicalizes symlinked ancestors (e.g. /tmp on macOS).
pub fn canonical_data_dir(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(path)?;
    path.canonicalize()
}

pub fn socket_path(data: &Path) -> PathBuf {
    data.join("agent/mcp.sock")
}

/// sun_path includes a trailing NUL: 104 bytes on macOS, 108 on Linux.
#[cfg(target_os = "macos")]
pub const MAX_SOCKET_PATH_BYTES: usize = 103;
#[cfg(not(target_os = "macos"))]
pub const MAX_SOCKET_PATH_BYTES: usize = 107;

#[derive(Debug)]
pub struct SocketPathTooLong {
    pub bytes: usize,
}
impl std::fmt::Display for SocketPathTooLong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "External agent: data folder path is too long for a Unix socket ({} bytes, max {MAX_SOCKET_PATH_BYTES}). Use a shorter TR_DATA_DIR.",
            self.bytes
        )
    }
}
impl std::error::Error for SocketPathTooLong {}

/// Check encoded bytes, not characters, before any bind/connect or relocation.
pub fn validate_socket_path(path: &Path) -> std::result::Result<(), SocketPathTooLong> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes().len();
    if bytes > MAX_SOCKET_PATH_BYTES {
        return Err(SocketPathTooLong { bytes });
    }
    Ok(())
}

pub fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
pub fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
pub fn tool_error(message: &str) -> Value {
    json!({"content":[{"type":"text","text":message}],"isError":true})
}
pub fn tool_result(result: tr_agent_tools::ToolResult) -> Value {
    let mut failed = !result.success;
    let content: Vec<_> = result
        .content
        .into_iter()
        .map(|part| match part {
            tr_agent_tools::Content::Text(text) => json!({"type":"text","text":text}),
            // External image export requires an independent consent mechanism. Do not
            // silently move screenshots into another client's conversation.
            tr_agent_tools::Content::ImagePng(_) => {
                failed = true;
                json!({"type":"text","text":"Image export is unavailable over MCP"})
            }
        })
        .collect();
    json!({"content":content,"isError":failed})
}
pub fn schemas(registry: &tr_agent_tools::Registry) -> Value {
    json!({"tools":registry.schemas().iter().filter(|spec| !spec.requires_host_consent()).map(|spec| json!({
        "name":spec.name,"description":spec.description,"inputSchema":spec.input_schema,
        "annotations":{"readOnlyHint":!spec.mutating,"destructiveHint":spec.mutating}
    })).collect::<Vec<_>>()})
}

/// Unlike read_line/read_until this never allocates beyond the cap, including
/// when a hostile client omits the newline. An oversized connection is closed.
pub async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let bytes = reader.fill_buf().await?;
        if bytes.is_empty() {
            return if line.is_empty() { Ok(None) } else { Ok(Some(line)) };
        }
        let size = bytes.iter().position(|b| *b == b'\n').map_or(bytes.len(), |n| n + 1);
        if line.len() + size > MAX_LINE {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "JSON-RPC line exceeds 1 MiB"));
        }
        let complete = bytes[size - 1] == b'\n';
        line.extend_from_slice(&bytes[..size]);
        reader.consume(size);
        if complete {
            return Ok(Some(line));
        }
    }
}
pub async fn write_line<W: AsyncWrite + Unpin>(writer: &mut W, value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() + 1 > MAX_LINE {
        bail!("JSON-RPC response exceeds 1 MiB");
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

fn valid_request(value: &Value) -> bool {
    value.is_object()
        && value["method"].is_string()
        && value.get("jsonrpc").is_none_or(|v| v == "2.0")
        && value.get("id").is_none_or(|id| id.is_null() || id.is_string() || id.is_number())
        && value.get("params").is_none_or(Value::is_object)
}

fn request_id(value: &Value) -> Value {
    value.get("id").filter(|id| id.is_null() || id.is_string() || id.is_number()).cloned().unwrap_or(Value::Null)
}
fn valid_tool_params(value: &Value) -> bool {
    value["params"]["name"].is_string() && value["params"].get("arguments").is_none_or(Value::is_object)
}

/// Per-connection serial dispatcher: one call in flight, sliding 60 calls/minute.
/// The handler is connection-local so its registry budget/cache cannot cross peers.
pub async fn serve_app<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    read: R,
    mut write: W,
    handler: ToolHandler,
) -> Result<()> {
    let mut reader = BufReader::new(read);
    let mut calls = VecDeque::new();
    loop {
        let line = match read_line(&mut reader).await {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(()),
            Err(_) => {
                write_line(&mut write, &error(Value::Null, -32600, "Payload exceeds 1 MiB or read failed")).await?;
                return Ok(());
            }
        };
        let value: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(_) => {
                write_line(&mut write, &error(Value::Null, -32700, "Parse error")).await?;
                continue;
            }
        };
        let id = request_id(&value);
        if !valid_request(&value) || value.get("id").is_none() {
            write_line(&mut write, &error(id, -32600, "Invalid request")).await?;
            continue;
        }
        let reply = match value["method"].as_str() {
            Some("tools/call") if !valid_tool_params(&value) => {
                error(id, -32602, "Expected tool name and object arguments")
            }
            Some("tools/list" | "tools/call") => {
                let now = Instant::now();
                while calls.front().is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60)) {
                    calls.pop_front();
                }
                if value["method"] == "tools/call" && calls.len() >= 60 {
                    error(id, -32000, "Rate limit: 60 calls/minute per connection")
                } else {
                    if value["method"] == "tools/call" {
                        calls.push_back(now);
                    }
                    // Panic isolation protects the socket and UI from an adapter bug.
                    let invoke = handler.clone();
                    let mut task = tokio::spawn(async move { invoke(value).await });
                    let _abort = TaskAbort(task.abort_handle());
                    let result = tokio::time::timeout(Duration::from_secs(80), &mut task).await;
                    task.abort();
                    match result {
                        Ok(Ok(Ok(result))) => success(id, result),
                        _ => success(id, tool_error("External agent tool failed or timed out")),
                    }
                }
            }
            _ => error(id, -32601, "Method not found"),
        };
        write_line(&mut write, &reply).await?;
    }
}

struct TaskAbort(tokio::task::AbortHandle);
impl Drop for TaskAbort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Proxy {
    path: PathBuf,
    stream: Option<BufReader<UnixStream>>,
    next: u64,
}
impl Proxy {
    async fn call(&mut self, method: &str, params: Value) -> Option<std::result::Result<Value, Value>> {
        let result = tokio::time::timeout(Duration::from_secs(85), async {
            if self.stream.is_none() {
                self.stream = Some(BufReader::new(UnixStream::connect(&self.path).await?));
            }
            let stream = self.stream.as_mut().expect("connected stream");
            self.next += 1;
            write_line(stream.get_mut(), &json!({"jsonrpc":"2.0","id":self.next,"method":method,"params":params}))
                .await?;
            let line = read_line(stream).await?.ok_or_else(|| anyhow::anyhow!("app socket closed"))?;
            let reply: Value = serde_json::from_slice(&line)?;
            if reply["id"] != self.next || reply.get("result").is_some() == reply.get("error").is_some() {
                bail!("invalid app socket response");
            }
            Ok::<_, anyhow::Error>(if let Some(error) = reply.get("error") {
                Err(error.clone())
            } else {
                Ok(reply["result"].clone())
            })
        })
        .await;
        match result {
            Ok(Ok(value)) => Some(value),
            _ => {
                self.stream = None;
                None
            }
        }
    }
}

/// Headless entry point. Does not construct providers, settings or an eframe app.
pub async fn serve_stdio<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    read: R,
    mut write: W,
    path: PathBuf,
) -> Result<()> {
    validate_socket_path(&path)?;
    let mut read = BufReader::new(read);
    let mut proxy = Proxy { path, stream: None, next: 0 };
    let mut initialized = false;
    loop {
        let line = match read_line(&mut read).await {
            Ok(Some(line)) => line,
            Ok(None) => return Ok(()),
            Err(_) => {
                write_line(&mut write, &error(Value::Null, -32600, "Payload exceeds 1 MiB or read failed")).await?;
                return Ok(());
            }
        };
        let request: Value = match serde_json::from_slice(&line) {
            Ok(value) => value,
            Err(_) => {
                write_line(&mut write, &error(Value::Null, -32700, "Parse error")).await?;
                continue;
            }
        };
        // Classify before validation: even malformed notifications must not
        // elicit output, and replying to responses can create an RPC error loop.
        if request.is_object()
            && ((request.get("method").is_some() && request.get("id").is_none_or(Value::is_null))
                || request.get("result").is_some()
                || request.get("error").is_some())
        {
            continue;
        }
        let id = request_id(&request);
        if !valid_request(&request) || request["jsonrpc"] != "2.0" {
            write_line(&mut write, &error(id, -32600, "Invalid request")).await?;
            continue;
        }
        // MCP notifications never receive responses, including unknown ones.
        if request.get("id").is_none() {
            continue;
        }
        let method = request["method"].as_str().unwrap_or_default();
        let reply = match method {
            "initialize" => {
                initialized = true;
                let version = match request["params"]["protocolVersion"].as_str() {
                    Some("2025-03-26") => "2025-03-26",
                    _ => "2025-06-18",
                };
                success(
                    id,
                    json!({"protocolVersion":version,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"terminal-rs","version":env!("CARGO_PKG_VERSION")}}),
                )
            }
            "ping" => success(id, json!({})),
            "tools/list" | "tools/call" | "prompts/list" | "resources/list" | "resources/templates/list"
                if !initialized =>
            {
                error(id, -32000, "Initialize first")
            }
            "prompts/list" => success(id, json!({"prompts":[]})),
            "resources/list" => success(id, json!({"resources":[]})),
            "resources/templates/list" => success(id, json!({"resourceTemplates":[]})),
            "tools/call" if !valid_tool_params(&request) => {
                error(id, -32602, "Expected tool name and object arguments")
            }
            "tools/list" | "tools/call" => {
                let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
                match proxy.call(method, params).await {
                    Some(Ok(result)) => success(id, result),
                    Some(Err(error)) => json!({"jsonrpc":"2.0","id":id,"error":error}),
                    None => {
                        success(id, if method == "tools/list" { json!({"tools":[]}) } else { tool_error(UNAVAILABLE) })
                    }
                }
            }
            _ => error(id, -32601, "Method not found"),
        };
        write_line(&mut write, &reply).await?;
    }
}

/// Refuse ambiguous paths/listeners; share exactly the bind-time stale checks
/// with disabled-at-startup cleanup. This never creates a directory or socket.
pub fn cleanup_stale_socket(data: &Path) -> Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    let directory = data.join("agent");
    let meta = match std::fs::symlink_metadata(&directory) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != uid() {
        bail!("Agent directory must be owned and not a symlink");
    }
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    let path = socket_path(data);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if !meta.file_type().is_socket() || meta.uid() != uid() {
        bail!("Refusing non-owned or non-socket MCP path");
    }
    // A full backlog must not hang the UI. Only definitive ECONNREFUSED
    // authorizes removal; timeout/permissions/other errors fail closed.
    let probe = path.clone();
    let stale = std::thread::spawn(move || -> Result<bool> {
        tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
            match tokio::time::timeout(Duration::from_millis(250), UnixStream::connect(probe)).await {
                Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => Ok(true),
                Ok(Ok(_)) => Ok(false),
                _ => bail!("Could not establish that the MCP socket is stale"),
            }
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("MCP socket probe failed"))??;
    if !stale {
        bail!("An MCP listener is already running");
    }
    let current = std::fs::symlink_metadata(&path)?;
    if !current.file_type().is_socket()
        || current.uid() != uid()
        || current.dev() != meta.dev()
        || current.ino() != meta.ino()
    {
        bail!("MCP socket changed during the stale listener check");
    }
    std::fs::remove_file(path)?;
    Ok(())
}

async fn backoff_accept_error(error: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    let transient = matches!(error.kind(), ErrorKind::ConnectionAborted | ErrorKind::Interrupted | ErrorKind::WouldBlock)
        // EMFILE / ENFILE on supported Unix hosts; no new dependency for errno.
        || matches!(error.raw_os_error(), Some(23 | 24));
    if transient {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    transient
}

/// The guard owns the socket inode, listener runtime and all accepted peers.
/// Drop cancels calls/connections before unlinking; an active listener is never replaced.
pub struct AppSocket {
    path: PathBuf,
    inode: u64,
    stop: tokio::sync::watch::Sender<()>,
    owner: Option<std::thread::JoinHandle<()>>,
}
impl AppSocket {
    pub fn bind(data: &Path, factory: Arc<dyn Fn() -> ToolHandler + Send + Sync>) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let path = socket_path(data);
        validate_socket_path(&path)?;
        let directory = data.join("agent");
        match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let meta = std::fs::symlink_metadata(&directory)?;
        if !meta.is_dir() || meta.file_type().is_symlink() || meta.uid() != uid() {
            bail!("Agent directory must be owned and not a symlink");
        }
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        cleanup_stale_socket(data)?;
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let inode = std::fs::symlink_metadata(&path)?.ino();
        let (stop, mut stopping) = tokio::sync::watch::channel(());
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let owner = std::thread::spawn(move || {
            runtime.block_on(async move {
                let Ok(listener) = tokio::net::UnixListener::from_std(listener) else { return };
                let mut tasks = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        biased;
                        _ = stopping.changed() => break,
                        Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                        peer = listener.accept() => {
                            let (stream, _) = match peer {
                                Ok(peer) => peer,
                                Err(error) => {
                                    tokio::select! {
                                        biased;
                                        _ = stopping.changed() => break,
                                        retry = backoff_accept_error(&error) => if retry { continue; } else { break; },
                                    }
                                }
                            };
                            // Bound local resource use as well as per-connection work.
                            if tasks.len() >= 16 || !same_uid(&stream) { continue; }
                            let handler = factory();
                            tasks.spawn(async move {
                                let (read, write) = stream.into_split();
                                let _ = serve_app(read, write, handler).await;
                            });
                        }
                    }
                }
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
            })
        });
        Ok(Self { path, inode, stop, owner: Some(owner) })
    }
}
impl Drop for AppSocket {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        let _ = self.stop.send(());
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
        if std::fs::symlink_metadata(&self.path).is_ok_and(|meta| meta.ino() == self.inode) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
fn uid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid has no preconditions.
    unsafe { geteuid() }
}
fn same_uid(stream: &UnixStream) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        unsafe extern "C" {
            fn getpeereid(socket: i32, uid: *mut u32, gid: *mut u32) -> i32;
        }
        let (mut peer, mut group) = (0, 0);
        // SAFETY: a live socket fd and valid uid/gid output pointers.
        unsafe { getpeereid(stream.as_raw_fd(), &mut peer, &mut group) == 0 && peer == uid() }
    }
    #[cfg(not(target_os = "macos"))]
    {
        stream.peer_cred().is_ok_and(|peer| peer.uid() == uid())
    }
}
