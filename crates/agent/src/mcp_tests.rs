use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use tr_agent_tools::{CallBudget, CallContext, DataHost, History, HostFuture, LiveHost, Registry, ToolResult, ToolSet};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").canonicalize().unwrap();
        let path = root.join(format!("m{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Host;
impl DataHost for Host {
    fn history<'a>(&'a self, _: &'a str, _: tr_core::Timeframe, _: Option<i64>, _: usize) -> HostFuture<'a, History> {
        Box::pin(async { Err("offline".into()) })
    }
    fn search<'a>(&'a self, _: &'a str) -> HostFuture<'a, Vec<tr_data::SearchHit>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn quotes<'a>(&'a self, _: &'a [String]) -> HostFuture<'a, Vec<tr_data::Quote>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}
impl LiveHost for Host {
    fn call_live<'a>(
        &'a self,
        name: &'a str,
        _: tr_agent_tools::args::LiveArgs,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move { ToolResult::json(json!({"tool":name,"symbol":"BTC-USD","source":"fake app"})) })
    }
}
fn factory() -> Arc<dyn Fn() -> ToolHandler + Send + Sync> {
    Arc::new(|| {
        let registry = Arc::new(Registry::new(ToolSet::ReadOnly));
        Arc::new(move |request| {
            let registry = registry.clone();
            Box::pin(async move {
                if request["method"] == "tools/list" {
                    return Ok(schemas(&registry));
                }
                let args = request["params"].get("arguments").cloned().unwrap_or(json!({}));
                let mut budget = CallBudget::new(1);
                let mut ctx = CallContext::new(&Host, &Host, &mut budget);
                Ok(tool_result(
                    registry
                        .call(request["params"]["name"].as_str().unwrap_or_default(), &args.to_string(), &mut ctx)
                        .await,
                ))
            })
        })
    })
}
async fn receive<R: AsyncRead + Unpin>(read: &mut BufReader<R>) -> Value {
    let line = tokio::time::timeout(Duration::from_secs(3), read_line(read)).await.unwrap().unwrap().unwrap();
    serde_json::from_slice(&line).unwrap()
}
async fn round_trip<W: AsyncWrite + Unpin, R: AsyncRead + Unpin>(
    write: &mut W,
    read: &mut BufReader<R>,
    id: u64,
    method: &str,
    params: Value,
) -> Value {
    write_line(write, &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).await.unwrap();
    let reply = receive(read).await;
    assert_eq!(reply["id"], id);
    reply
}

#[test]
fn socket_path_limit_counts_bytes_and_reserves_the_trailing_nul() {
    use std::os::unix::ffi::OsStrExt;
    let temp = Temp::new();
    let prefix_bytes = temp.0.as_os_str().as_bytes().len() + 1;
    let boundary = temp.0.join("x".repeat(MAX_SOCKET_PATH_BYTES - prefix_bytes));
    assert!(validate_socket_path(&boundary).is_ok());
    let too_long = boundary.with_file_name(format!("{}x", boundary.file_name().unwrap().to_str().unwrap()));
    let error = validate_socket_path(&too_long).unwrap_err();
    assert_eq!(error.bytes, MAX_SOCKET_PATH_BYTES + 1);
    assert_eq!(
        error.to_string(),
        format!(
            "External agent: data folder path is too long for a Unix socket ({} bytes, max {MAX_SOCKET_PATH_BYTES}). Use a shorter TR_DATA_DIR.",
            MAX_SOCKET_PATH_BYTES + 1
        )
    );
    let unicode = temp.0.join("é".repeat(MAX_SOCKET_PATH_BYTES - prefix_bytes));
    assert_eq!(validate_socket_path(&unicode).unwrap_err().bytes, unicode.as_os_str().as_bytes().len());
}

#[tokio::test]
async fn long_socket_path_is_rejected_before_bind_or_stdio_io() {
    let temp = Temp::new();
    let data = temp.0.join("x".repeat(MAX_SOCKET_PATH_BYTES));
    let path = socket_path(&data);
    let expected = validate_socket_path(&path).unwrap_err().to_string();
    let error = AppSocket::bind(&data, factory()).err().expect("long bind must fail");
    assert_eq!(error.downcast_ref::<SocketPathTooLong>().unwrap().to_string(), expected);
    assert!(!data.exists(), "validation must precede filesystem changes");
    let error = serve_stdio(tokio::io::empty(), tokio::io::sink(), path).await.unwrap_err();
    assert_eq!(error.downcast_ref::<SocketPathTooLong>().unwrap().to_string(), expected);
}

#[tokio::test]
async fn external_schemas_omit_consent_only_tools_but_calls_still_refuse() {
    for set in [ToolSet::ReadOnly, ToolSet::Edit, ToolSet::Workflows] {
        let registry = Registry::new(set);
        let external = schemas(&registry);
        let tools = external["tools"].as_array().unwrap();
        assert!(tools.iter().all(|tool| tool["name"] != "screenshot"));
        for spec in registry.schemas() {
            assert_eq!(tools.iter().any(|tool| tool["name"] == spec.name), !spec.requires_host_consent());
        }
        if set == ToolSet::Workflows {
            assert!(tools.iter().any(|tool| tool["name"] == "create_watchlist"));
        }
        let mut budget = CallBudget::new(1);
        let mut ctx = CallContext::new(&Host, &Host, &mut budget);
        let refused = tool_result(registry.call("screenshot", "{}", &mut ctx).await);
        assert_eq!(refused["isError"], true);
        assert!(refused.to_string().contains("confirmation_required"));
    }
}

#[test]
fn relative_data_dir_is_created_and_canonicalized() {
    let temp = Temp::new();
    let mut base = std::env::current_dir().unwrap();
    let mut relative = PathBuf::new();
    while !temp.0.starts_with(&base) {
        base.pop();
        relative.push("..");
    }
    relative.push(temp.0.strip_prefix(base).unwrap());
    relative.push("new/../new/instance");
    let resolved = canonical_data_dir(&relative).unwrap();
    assert!(resolved.is_absolute());
    assert_eq!(resolved, temp.0.join("new/instance").canonicalize().unwrap());
    assert!(socket_path(&resolved).is_absolute());
}

#[tokio::test(start_paused = true)]
async fn injected_transient_accept_errors_back_off_and_fatal_errors_stop() {
    use std::io::{Error, ErrorKind};
    for error in [
        Error::from_raw_os_error(24),
        Error::from(ErrorKind::ConnectionAborted),
        Error::from(ErrorKind::Interrupted),
        Error::from(ErrorKind::WouldBlock),
    ] {
        let start = tokio::time::Instant::now();
        assert!(backoff_accept_error(&error).await);
        assert!(start.elapsed() >= Duration::from_millis(50));
    }
    assert!(!backoff_accept_error(&Error::from(ErrorKind::PermissionDenied)).await);
}

#[test]
fn disabled_startup_cleanup_preserves_live_regular_and_symlink_paths() {
    let temp = Temp::new();
    assert!(cleanup_stale_socket(&temp.0).is_ok());
    std::fs::create_dir(temp.0.join("agent")).unwrap();
    let path = socket_path(&temp.0);
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    assert!(cleanup_stale_socket(&temp.0).is_err());
    assert!(path.exists());
    drop(listener);
    cleanup_stale_socket(&temp.0).unwrap();
    assert!(!path.exists());
    std::fs::write(&path, b"keep").unwrap();
    assert!(cleanup_stale_socket(&temp.0).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"keep");
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(temp.0.join("missing"), &path).unwrap();
    assert!(cleanup_stale_socket(&temp.0).is_err());
    assert!(std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink());
}

#[tokio::test]
async fn stdio_never_answers_notifications_or_response_messages() {
    let temp = Temp::new();
    let (client, shim) = tokio::io::duplex(8192);
    let (r, w) = tokio::io::split(shim);
    let task = tokio::spawn(serve_stdio(r, w, socket_path(&temp.0)));
    let (r, mut w) = tokio::io::split(client);
    let mut r = BufReader::new(r);
    for frame in [
        json!({"jsonrpc":"2.0","method":"unknown"}),
        json!({"jsonrpc":"bad","method":12,"params":[]}),
        json!({"jsonrpc":"2.0","id":null,"method":"ping","params":[]}),
        json!({"jsonrpc":"2.0","id":1,"result":{}}),
        json!({"jsonrpc":"2.0","result":{}}),
        json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600}}),
    ] {
        write_line(&mut w, &frame).await.unwrap();
    }
    // A sentinel proves every preceding frame was silent, without a timing assertion.
    assert_eq!(round_trip(&mut w, &mut r, 99, "ping", json!({})).await["result"], json!({}));
    w.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    assert!(read_line(&mut r).await.unwrap().is_none());
}

#[tokio::test]
async fn full_stdio_socket_registry_round_trip_read_and_write_refused() {
    let temp = Temp::new();
    let _app = AppSocket::bind(&temp.0, factory()).unwrap();
    let (client, shim) = tokio::io::duplex(MAX_LINE * 2);
    let (shim_read, shim_write) = tokio::io::split(shim);
    let path = socket_path(&temp.0);
    let task = tokio::spawn(serve_stdio(shim_read, shim_write, path));
    let (read, mut write) = tokio::io::split(client);
    let mut read = BufReader::new(read);
    let init = round_trip(&mut write, &mut read, 1, "initialize", json!({"protocolVersion":"2025-06-18"})).await;
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    write_line(&mut write, &json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await.unwrap();
    let list = round_trip(&mut write, &mut read, 2, "tools/list", json!({})).await;
    let tools = list["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "read_chart"));
    assert!(tools.iter().all(|t| t["annotations"]["readOnlyHint"] == true));
    let registry = Registry::new(ToolSet::ReadOnly);
    for spec in registry.schemas().iter().filter(|spec| !spec.requires_host_consent()) {
        assert_eq!(tools.iter().find(|t| t["name"] == spec.name).unwrap()["inputSchema"], spec.input_schema);
    }
    assert!(tools.iter().all(|t| t["name"] != "screenshot"));
    let read_tool =
        round_trip(&mut write, &mut read, 3, "tools/call", json!({"name":"read_chart","arguments":{}})).await;
    assert_eq!(read_tool["result"]["isError"], false);
    assert!(read_tool["result"]["content"][0]["text"].as_str().unwrap().contains("fake app"));
    let write_tool =
        round_trip(&mut write, &mut read, 4, "tools/call", json!({"name":"create_drawing","arguments":{}})).await;
    assert_eq!(write_tool["result"]["isError"], true);
    assert!(write_tool.to_string().contains("tool_disabled"));
    assert_eq!(round_trip(&mut write, &mut read, 5, "bogus", json!({})).await["error"]["code"], -32601);
    write.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn unavailable_app_initializes_and_returns_empty_tools_and_helpful_error() {
    let temp = Temp::new();
    let (client, shim) = tokio::io::duplex(8192);
    let (r, w) = tokio::io::split(shim);
    let task = tokio::spawn(serve_stdio(r, w, socket_path(&temp.0)));
    let (r, mut w) = tokio::io::split(client);
    let mut r = BufReader::new(r);
    let init = round_trip(&mut w, &mut r, 1, "initialize", json!({"protocolVersion":"2025-03-26"})).await;
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(round_trip(&mut w, &mut r, 2, "tools/list", json!({})).await["result"]["tools"], json!([]));
    let reply = round_trip(&mut w, &mut r, 3, "tools/call", json!({"name":"read_chart"})).await;
    assert_eq!(reply["result"], tool_error(UNAVAILABLE));
    assert_eq!(round_trip(&mut w, &mut r, 4, "ping", json!({})).await["result"], json!({}));
    w.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn malformed_and_oversized_lines_return_errors_without_panicking() {
    for app in [false, true] {
        let temp = Temp::new();
        let (client, server) = tokio::io::duplex(MAX_LINE * 2);
        let (r, w) = tokio::io::split(server);
        let path = socket_path(&temp.0);
        let task = tokio::spawn(async move {
            if app { serve_app(r, w, factory()()).await } else { serve_stdio(r, w, path).await }
        });
        let (r, mut w) = tokio::io::split(client);
        let mut r = BufReader::new(r);
        w.write_all(b"not-json\n").await.unwrap();
        assert_eq!(receive(&mut r).await["error"]["code"], -32700);
        w.write_all(b"[]\n").await.unwrap();
        assert_eq!(receive(&mut r).await["error"]["code"], -32600);
        write_line(&mut w, &json!({"jsonrpc":"2.0","id":[],"method":"bad"})).await.unwrap();
        let invalid = receive(&mut r).await;
        assert_eq!(invalid["error"]["code"], -32600);
        assert!(invalid["id"].is_null());
        if !app {
            round_trip(&mut w, &mut r, 1, "initialize", json!({})).await;
        }
        let invalid_params =
            round_trip(&mut w, &mut r, 2, "tools/call", json!({"name":"read_chart","arguments":[]})).await;
        assert_eq!(invalid_params["error"]["code"], -32602);
        w.write_all(&vec![b'x'; MAX_LINE + 1]).await.unwrap();
        assert_eq!(receive(&mut r).await["error"]["code"], -32600);
        task.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn socket_is_private_replaces_only_stale_and_unlinks_on_drop() {
    use std::os::unix::fs::PermissionsExt;
    let temp = Temp::new();
    std::fs::create_dir(temp.0.join("agent")).unwrap();
    let path = socket_path(&temp.0);
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    let app = AppSocket::bind(&temp.0, factory()).unwrap();
    assert_eq!(std::fs::metadata(temp.0.join("agent")).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert!(AppSocket::bind(&temp.0, factory()).is_err(), "must not replace a live listener");
    drop(app);
    assert!(!path.exists());
    std::fs::write(&path, b"not a socket").unwrap();
    assert!(AppSocket::bind(&temp.0, factory()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"not a socket");
}

#[tokio::test]
async fn rate_limit_is_per_connection_and_dispatch_is_serial() {
    let (client, server) = tokio::io::duplex(65536);
    let (r, w) = tokio::io::split(server);
    let count = Arc::new(AtomicU64::new(0));
    let called = count.clone();
    let handler: ToolHandler = Arc::new(move |_| {
        let called = called.clone();
        Box::pin(async move {
            called.fetch_add(1, Ordering::SeqCst);
            Ok(json!({}))
        })
    });
    let task = tokio::spawn(serve_app(r, w, handler));
    let (r, mut w) = tokio::io::split(client);
    let mut r = BufReader::new(r);
    // Pipeline requests; the reader cannot dispatch a second one until the first finishes.
    for id in 1..=61 {
        write_line(&mut w, &json!({"id":id,"method":"tools/call","params":{"name":"read_chart"}})).await.unwrap();
    }
    for _ in 0..60 {
        assert!(receive(&mut r).await.get("result").is_some());
    }
    assert_eq!(receive(&mut r).await["error"]["code"], -32000);
    assert_eq!(count.load(Ordering::SeqCst), 60);
    w.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn proxy_preserves_rpc_rate_error_without_reconnecting_or_replaying() {
    let temp = Temp::new();
    let _app = AppSocket::bind(&temp.0, factory()).unwrap();
    let (client, server) = tokio::io::duplex(8192);
    let (r, w) = tokio::io::split(server);
    let task = tokio::spawn(serve_stdio(r, w, socket_path(&temp.0)));
    let (r, mut w) = tokio::io::split(client);
    let mut r = BufReader::new(r);
    round_trip(&mut w, &mut r, 1, "initialize", json!({})).await;
    for id in 2..=61 {
        assert_eq!(
            round_trip(&mut w, &mut r, id, "tools/call", json!({"name":"read_chart"})).await["result"]["isError"],
            false
        );
    }
    for id in [62, 63] {
        let reply = round_trip(&mut w, &mut r, id, "tools/call", json!({"name":"read_chart"})).await;
        assert_eq!(reply["error"]["code"], -32000);
        assert!(!reply.to_string().contains(UNAVAILABLE));
    }
    assert!(round_trip(&mut w, &mut r, 64, "tools/list", json!({})).await["result"]["tools"].is_array());
    w.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn handler_panic_is_contained_and_next_request_still_works() {
    let (client, server) = tokio::io::duplex(8192);
    let (r, w) = tokio::io::split(server);
    let handler: ToolHandler = Arc::new(|_| panic!("adapter failed"));
    let task = tokio::spawn(serve_app(r, w, handler));
    let (r, mut w) = tokio::io::split(client);
    let mut r = BufReader::new(r);
    assert_eq!(
        round_trip(&mut w, &mut r, 1, "tools/call", json!({"name":"read_chart"})).await["result"]["isError"],
        true
    );
    assert_eq!(round_trip(&mut w, &mut r, 2, "unknown", json!({})).await["error"]["code"], -32601);
    w.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}
