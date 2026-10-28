use super::*;
use crate::AgentEvents;
use tokio::io::{AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

fn fake() -> (AcpBackend, BufReader<ReadHalf<DuplexStream>>, WriteHalf<DuplexStream>, Arc<AtomicBool>) {
    let (client, server) = tokio::io::duplex(65536);
    let (r, w) = tokio::io::split(client);
    let killed = Arc::new(AtomicBool::new(false));
    let flag = killed.clone();
    let backend = AcpBackend::connect(
        r,
        w,
        PathBuf::from("/private/empty"),
        PathBuf::from("/app/terminal-rs"),
        Arc::new(move || {
            flag.store(true, Ordering::SeqCst);
        }),
    );
    let (r, w) = tokio::io::split(server);
    (backend, BufReader::new(r), w, killed)
}
async fn receive<R: AsyncRead + Unpin>(read: &mut BufReader<R>) -> Value {
    let line =
        tokio::time::timeout(Duration::from_secs(2), crate::mcp::read_line(read)).await.unwrap().unwrap().unwrap();
    serde_json::from_slice(&line).unwrap()
}
async fn send<W: AsyncWrite + Unpin>(write: &mut W, value: Value) {
    crate::mcp::write_line(write, &value).await.unwrap();
}
fn options() -> ThreadOptions {
    ThreadOptions {
        cwd: "/ignored/untrusted".into(),
        base_instructions: "Chart analyst. Use only terminal-rs MCP.".into(),
        ephemeral: true,
        dynamic_tools: Vec::new(),
    }
}
async fn handshake(
    backend: &AcpBackend,
    read: &mut BufReader<ReadHalf<DuplexStream>>,
    write: &mut WriteHalf<DuplexStream>,
) {
    let server = async {
        let init = receive(read).await;
        assert_eq!(init["jsonrpc"], "2.0");
        assert_eq!(init["method"], "initialize");
        assert_eq!(init["params"]["protocolVersion"], 1);
        assert_eq!(
            init["params"]["clientCapabilities"],
            json!({"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false})
        );
        send(write, crate::mcp::success(init["id"].clone(), json!({"protocolVersion":1}))).await;
        let new = receive(read).await;
        assert_eq!(new["method"], "session/new");
        assert_eq!(new["params"]["cwd"], "/private/empty");
        assert_eq!(
            new["params"]["mcpServers"],
            json!([{"name":"terminal-rs","command":"/app/terminal-rs","args":["mcp-stdio"],"env":[]}])
        );
        send(write,crate::mcp::success(new["id"].clone(),json!({"sessionId":"fake-session","models":{"currentModelId":"model-a","availableModels":[{"modelId":"model-a","name":"Model A"}]}}))).await;
    };
    let (session, ()) = tokio::join!(backend.start_thread(options()), server);
    assert_eq!(session.unwrap(), "fake-session");
}

#[tokio::test]
async fn handshake_mcp_entry_streamed_chunks_tools_plan_and_models() {
    let (backend, mut read, mut write, killed) = fake();
    handshake(&backend, &mut read, &mut write).await;
    let models = backend.list_models().await.unwrap();
    assert_eq!(models[0].display_name, "Model A");
    let mut events = backend.events();
    let turn = backend.send_turn(TurnInput { text: "Describe this chart".into(), ..Default::default() }).await.unwrap();
    let prompt = receive(&mut read).await;
    assert_eq!(prompt["method"], "session/prompt");
    assert_eq!(prompt["params"]["sessionId"], "fake-session");
    assert!(prompt["params"]["prompt"][0]["text"].as_str().unwrap().contains("Chart analyst"));
    for update in [
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Trend is up\n"}}),
        json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Check bars\n"}}),
        json!({"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"mcp__terminal-rs__read_chart","kind":"other"}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"tool-1","status":"completed"}),
        json!({"sessionUpdate":"plan","entries":[{"content":"Review finished bars","status":"in_progress","priority":"high"}]}),
    ] {
        send(
            &mut write,
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fake-session","update":update}}),
        )
        .await;
    }
    send(&mut write, crate::mcp::success(prompt["id"].clone(), json!({"stopReason":"end_turn"}))).await;
    assert!(
        matches!(events.recv().await,Some(AgentEvent::TextDelta(v)) if v["delta"] == "Trend is up\n" && v["turnId"] == turn)
    );
    assert!(matches!(events.recv().await, Some(AgentEvent::ReasoningDelta(_))));
    assert!(matches!(events.recv().await, Some(AgentEvent::ToolStarted(_))));
    assert!(matches!(events.recv().await, Some(AgentEvent::ToolFinished(_))));
    assert!(
        matches!(events.recv().await, Some(AgentEvent::ReasoningDelta(v)) if v["delta"].as_str().unwrap().contains("[in progress] Review finished bars"))
    );
    assert!(matches!(events.recv().await, Some(AgentEvent::Done(_))));
    assert!(!killed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn canary_uses_fresh_session_and_foreign_attempt_fails_readiness() {
    for foreign in [false, true] {
        let (backend, mut read, mut write, killed) = fake();
        handshake(&backend, &mut read, &mut write).await;
        let server = async {
            let new = receive(&mut read).await;
            assert_eq!(new["method"], "session/new");
            send(&mut write, crate::mcp::success(new["id"].clone(), json!({"sessionId":"canary"}))).await;
            let prompt = receive(&mut read).await;
            assert_eq!(prompt["method"], "session/prompt");
            assert!(prompt["params"]["prompt"][0]["text"].as_str().unwrap().contains("ls ~"));
            if foreign {
                send(&mut write, json!({"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call","title":"bash","toolCallId":"foreign"}}})).await;
                assert_eq!(receive(&mut read).await["method"], "session/cancel");
            } else {
                send(&mut write, crate::mcp::success(prompt["id"].clone(), json!({"stopReason":"end_turn"}))).await;
            }
        };
        let (result, ()) = tokio::join!(backend.canary(), server);
        assert_eq!(result.is_err(), foreign);
        assert_eq!(backend.is_unsafe(), foreign);
        assert_eq!(killed.load(Ordering::SeqCst), foreign);
    }
}

#[tokio::test]
async fn initialize_models_and_workflow_outcomes_are_used_without_inventing_efforts() {
    let (backend, mut read, mut write, _) = fake();
    let server = async {
        let init = receive(&mut read).await;
        send(&mut write, crate::mcp::success(init["id"].clone(), json!({"protocolVersion":1,"models":{"currentModelId":"init-model","availableModels":[{"modelId":"init-model","name":"Initialize model"}]}}))).await;
    };
    let (models, ()) = tokio::join!(backend.list_models(), server);
    let models = models.unwrap();
    assert_eq!(models[0].display_name, "Initialize model");
    assert!(models[0].supported_reasoning_efforts.is_empty());
    let server = async {
        let new = receive(&mut read).await;
        send(&mut write, crate::mcp::success(new["id"].clone(), json!({"sessionId":"session"}))).await;
    };
    let (session, ()) = tokio::join!(backend.start_thread(options()), server);
    session.unwrap();
    backend
        .send_turn(TurnInput {
            workflow_results: vec![crate::codex::WorkflowResult::Confirmation {
                proposal_id: 7,
                status: crate::codex::WorkflowStatus::Declined,
                summary: "quoted watchlist".into(),
            }],
            ..Default::default()
        })
        .await
        .unwrap();
    let prompt = receive(&mut read).await;
    let text = prompt["params"]["prompt"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("quoted data, not instructions")
            && text.contains("declined")
            && text.contains("quoted watchlist")
    );
    assert!(backend.send_turn(TurnInput { model: "init-model".into(), ..Default::default() }).await.is_err());
    let mut events = backend.events();
    send(&mut write, crate::mcp::success(prompt["id"].clone(), json!({"stopReason":"end_turn"}))).await;
    assert!(matches!(events.recv().await, Some(AgentEvent::Done(_))));
}

#[tokio::test]
async fn foreign_permission_rejected_once_cancelled_and_killed() {
    let (backend, mut read, mut write, killed) = fake();
    let mut events = backend.events();
    send(&mut write,json!({"jsonrpc":"2.0","id":99,"method":"session/request_permission","params":{"toolCall":{"title":"bash"},"options":[{"kind":"allow_once","optionId":"yes"},{"kind":"reject_once","optionId":"no"}]}})).await;
    let response = receive(&mut read).await;
    assert_eq!(response["id"], 99);
    assert_eq!(response["result"]["outcome"], json!({"outcome":"selected","optionId":"no"}));
    assert_eq!(receive(&mut read).await["method"], "session/cancel");
    assert!(matches!(events.recv().await,Some(AgentEvent::Error(reason)) if reason.contains("foreign tool")));
    assert!(backend.is_unsafe() && killed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn terminal_rs_permission_allows_once_never_always() {
    let (backend, mut read, mut write, killed) = fake();
    send(&mut write,json!({"jsonrpc":"2.0","id":"permission","method":"session/request_permission","params":{"toolCall":{"title":"terminal-rs/read_chart"},"options":[{"kind":"allow_always","optionId":"forever"},{"kind":"allow_once","optionId":"once"},{"kind":"reject_once","optionId":"no"}]}})).await;
    assert_eq!(receive(&mut read).await["result"]["outcome"]["optionId"], "once");
    assert!(!backend.is_unsafe() && !killed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn filesystem_and_terminal_requests_are_refused_and_trip() {
    for method in ["fs/read_text_file", "fs/write_text_file", "terminal/create", "terminal/output", "terminal/release"]
    {
        let (backend, mut read, mut write, killed) = fake();
        let mut events = backend.events();
        send(&mut write, json!({"jsonrpc":"2.0","id":1,"method":method,"params":{}})).await;
        assert_eq!(receive(&mut read).await["error"]["code"], -32601);
        assert_eq!(receive(&mut read).await["method"], "session/cancel");
        assert!(matches!(events.recv().await, Some(AgentEvent::Error(_))));
        assert!(backend.is_unsafe() && killed.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn foreign_tool_update_and_unknown_notifications_trip() {
    for update in [
        json!({"sessionUpdate":"tool_call","title":"webfetch","toolCallId":"bad"}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"unknown","status":"completed"}),
        json!({"sessionUpdate":"future_execution"}),
    ] {
        let (backend, mut read, mut write, killed) = fake();
        let mut events = backend.events();
        send(&mut write, json!({"jsonrpc":"2.0","method":"session/update","params":{"update":update}})).await;
        assert_eq!(receive(&mut read).await["method"], "session/cancel");
        assert!(matches!(events.recv().await, Some(AgentEvent::Error(_))));
        assert!(backend.is_unsafe() && killed.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn cancellation_uses_notification_and_turn_finishes_without_replay() {
    let (backend, mut read, mut write, _) = fake();
    handshake(&backend, &mut read, &mut write).await;
    let mut events = backend.events();
    backend.send_turn(TurnInput::default()).await.unwrap();
    let prompt = receive(&mut read).await;
    backend.interrupt().await.unwrap();
    let cancel = receive(&mut read).await;
    assert_eq!(cancel["method"], "session/cancel");
    assert_eq!(cancel["params"]["sessionId"], "fake-session");
    assert!(cancel.get("id").is_none());
    send(&mut write, crate::mcp::success(prompt["id"].clone(), json!({"stopReason":"cancelled"}))).await;
    assert!(matches!(events.recv().await, Some(AgentEvent::Done(_))));
}

#[tokio::test]
async fn child_crash_fails_pending_request_and_is_visible_to_late_subscriber() {
    let (backend, mut read, write, killed) = fake();
    let server = async {
        receive(&mut read).await;
        drop(read);
        drop(write);
    };
    let (init, ()) = tokio::join!(backend.initialize(), server);
    assert!(init.is_err());
    assert!(matches!(backend.events().recv().await,Some(AgentEvent::Error(reason)) if reason.contains("exited")));
    assert!(killed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn scrubber_handles_split_secrets_and_flushes_final_chunk() {
    let (backend, mut read, mut write, _) = fake();
    handshake(&backend, &mut read, &mut write).await;
    let mut events = backend.events();
    backend.send_turn(TurnInput::default()).await.unwrap();
    let prompt = receive(&mut read).await;
    for text in ["Contact trader@", "example.com; Bearer ", "secret-token"] {
        send(&mut write,json!({"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}}})).await;
    }
    send(&mut write, crate::mcp::success(prompt["id"].clone(), json!({"stopReason":"end_turn"}))).await;
    let Some(AgentEvent::TextDelta(value)) = events.recv().await else { panic!("missing final scrubbed chunk") };
    assert!(!value.to_string().contains("trader@"));
    assert!(!value.to_string().contains("secret-token"));
    assert!(matches!(events.recv().await, Some(AgentEvent::Done(_))));
}

#[tokio::test]
async fn malformed_acp_json_emits_clear_error() {
    let (backend, _read, mut write, killed) = fake();
    let mut events = backend.events();
    write.write_all(b"not JSON\n").await.unwrap();
    assert!(matches!(events.recv().await,Some(AgentEvent::Error(reason)) if reason.contains("malformed JSON")));
    assert!(killed.load(Ordering::SeqCst));
}

#[test]
fn tool_provenance_is_exact_and_sandbox_has_no_tcp_listener_or_foreign_exec() {
    let (output, _) = mpsc::channel(1);
    let rpc = Rpc {
        output,
        pending: Default::default(),
        next: AtomicU64::new(1),
        events: broadcast::channel(1).0,
        closed: watch::channel(None).0,
        unsafe_backend: AtomicBool::new(false),
        kill: Arc::new(|| {}),
        tools: HashSet::from(["read_chart".into()]),
        session: Default::default(),
        retired_sessions: Default::default(),
        turn: Default::default(),
        text: Default::default(),
        thought: Default::default(),
        tool_ids: Default::default(),
        busy: Arc::new(AtomicBool::new(false)),
    };
    for title in ["terminal-rs/read_chart", "mcp__terminal-rs__read_chart"] {
        assert!(rpc.terminal_tool(&json!({"title":title})));
    }
    for title in ["bash terminal-rs/read_chart", "terminal-rs/bash", "terminal-rs/read_chart; bash", "read_chart"] {
        assert!(!rpc.terminal_tool(&json!({"title":title})));
    }
    let profile = sandbox_profile(
        Path::new("/private/data/agent/mimo"),
        Path::new("/private/data/agent/mimo-home"),
        Path::new("/opt/mimo/bin/mimo"),
        Path::new("/app/terminal-rs"),
        Path::new("/private/data/agent/mcp.sock"),
        Path::new("/Users/test"),
    )
    .unwrap();
    assert!(profile.contains("(deny default)"));
    assert!(!profile.contains("network-inbound") && !profile.contains("network-bind"));
    assert!(!profile.contains("/bin/bash") && !profile.contains("/bin/sh"));
    assert!(profile.contains("(literal \"/private/data/agent/mcp.sock\")"));
    for binary in [
        "/mimo",
        "/Users/mimo",
        "/Users/test/mimo",
        "/Users/test/.ssh/mimo",
        "/Users/test/.codex/mimo",
        "/Users/test/.mimocode/mimo",
        "/Users/test/Documents/mimo",
    ] {
        assert!(
            sandbox_profile(
                Path::new("/private/data/agent/mimo"),
                Path::new("/private/data/agent/mimo-home"),
                Path::new(binary),
                Path::new("/app/terminal-rs"),
                Path::new("/private/data/agent/mcp.sock"),
                Path::new("/Users/test"),
            )
            .is_err(),
            "{binary}"
        );
    }
}

#[tokio::test]
async fn contradictory_tool_provenance_is_rejected_even_with_a_valid_title() {
    for call in [
        json!({"title":"terminal-rs/read_chart","rawInput":{"serverName":"foreign","name":"read_chart"}}),
        json!({"title":"mcp__terminal-rs__read_chart","rawInput":{"serverName":"terminal-rs","name":"bash"}}),
        json!({"title":"mcp__foreign__bash","rawInput":{"serverName":"terminal-rs","name":"read_chart"}}),
    ] {
        let (backend, mut read, mut write, killed) = fake();
        let mut events = backend.events();
        send(&mut write, json!({"jsonrpc":"2.0","id":"permission","method":"session/request_permission","params":{
            "toolCall":call,"options":[{"kind":"allow_once","optionId":"yes"},{"kind":"reject_once","optionId":"no"}]
        }})).await;
        assert_eq!(receive(&mut read).await["result"]["outcome"]["optionId"], "no");
        assert_eq!(receive(&mut read).await["method"], "session/cancel");
        assert!(matches!(events.recv().await, Some(AgentEvent::Error(_))));
        assert!(backend.is_unsafe() && killed.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn stale_session_updates_cannot_be_attributed_to_the_current_turn() {
    let (backend, mut read, mut write, killed) = fake();
    handshake(&backend, &mut read, &mut write).await;
    let mut events = backend.events();
    send(&mut write, json!({"jsonrpc":"2.0","method":"session/update","params":{
        "sessionId":"old-canary-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"old content\n"}}
    }})).await;
    assert_eq!(receive(&mut read).await["method"], "session/cancel");
    assert!(matches!(events.recv().await, Some(AgentEvent::Error(reason)) if reason.contains("another session")));
    assert!(backend.is_unsafe() && killed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn malformed_acp_envelopes_and_oversized_frames_fail_closed() {
    for value in [
        json!({"jsonrpc":"2.0","id":1,"method":false,"result":{}}),
        json!({"jsonrpc":"2.0","result":{}}),
        json!({"jsonrpc":"2.0","id":[],"result":{}}),
    ] {
        let (backend, _read, mut write, killed) = fake();
        let mut events = backend.events();
        send(&mut write, value).await;
        assert!(matches!(events.recv().await, Some(AgentEvent::Error(_))));
        assert!(killed.load(Ordering::SeqCst));
    }
    let (backend, _read, mut write, killed) = fake();
    let mut events = backend.events();
    // The peer reader consumes incrementally, not by allocating an unbounded line.
    let _ = write.write_all(&vec![b'x'; crate::mcp::MAX_LINE + 1]).await;
    assert!(matches!(events.recv().await, Some(AgentEvent::Error(_))));
    assert!(killed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn cleanup_waits_for_supervisor_and_propagates_findings() {
    let (mut backend, _read, _write, _) = fake();
    let (finished, cleaned) = watch::channel(None);
    backend.cleaned = Some(cleaned);
    assert!(tokio::time::timeout(Duration::from_millis(10), backend.wait_cleanup()).await.is_err());
    let _ = finished.send_replace(Some(Err("mimo cleanup findings: descendant survived".into())));
    let error = backend.wait_cleanup().await.unwrap_err();
    assert!(error.to_string().contains("descendant survived"));
}

#[tokio::test]
async fn approved_tool_id_does_not_allow_a_foreign_tool_update() {
    let (backend, mut read, mut write, killed) = fake();
    let mut events = backend.events();
    for update in [
        json!({"sessionUpdate":"tool_call","toolCallId":"known","title":"terminal-rs/read_chart"}),
        json!({"sessionUpdate":"tool_call_update","toolCallId":"known","rawInput":{"name":"bash"},"status":"completed"}),
    ] {
        send(&mut write, json!({"jsonrpc":"2.0","method":"session/update","params":{"update":update}})).await;
    }
    assert_eq!(receive(&mut read).await["method"], "session/cancel");
    // The error is sticky and may supersede the queued ToolStarted event.
    while let Some(event) = events.recv().await {
        if let AgentEvent::Error(reason) = event {
            assert!(reason.contains("unapproved tool"));
            break;
        }
    }
    assert!(backend.is_unsafe() && killed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn models_fall_back_to_one_mimo_default_when_acp_exposes_none() {
    let (backend, mut read, mut write, _) = fake();
    let server = async {
        let init = receive(&mut read).await;
        send(&mut write, crate::mcp::success(init["id"].clone(), json!({"protocolVersion":1}))).await;
    };
    let (models, ()) = tokio::join!(backend.list_models(), server);
    let models = models.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].display_name, "mimo default");
    assert_eq!(models[0].id, "mimo-default");
    assert!(models[0].is_default && models[0].supported_reasoning_efforts.is_empty());
}

#[tokio::test]
async fn review_a4_update_right_after_second_session_new_is_not_a_foreign_session() {
    let (backend, mut read, mut write, killed) = fake();
    // First session (as the canary does) sets a current session id.
    handshake(&backend, &mut read, &mut write).await;
    let server = async {
        let new = receive(&mut read).await;
        assert_eq!(new["method"], "session/new");
        // Agents commonly emit session metadata right after session/new; both
        // lines arrive in the same read.
        let mut bytes =
            serde_json::to_vec(&crate::mcp::success(new["id"].clone(), json!({"sessionId":"real"}))).unwrap();
        bytes.push(b'\n');
        bytes.extend(
            serde_json::to_vec(&json!({"jsonrpc":"2.0","method":"session/update","params":{
                "sessionId":"real","update":{"sessionUpdate":"available_commands_update","availableCommands":[]}
            }}))
            .unwrap(),
        );
        bytes.push(b'\n');
        write.write_all(&bytes).await.unwrap();
    };
    let (session, ()) = tokio::join!(backend.start_thread(options()), server);
    assert_eq!(session.unwrap(), "real");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!backend.is_unsafe(), "an update for the session just created must not trip the tripwire");
    send(
        &mut write,
        json!({"jsonrpc":"2.0","method":"session/update","params":{
            "sessionId":"fake-session","update":{"sessionUpdate":"available_commands_update"}
        }}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!backend.is_unsafe(), "late canary metadata is harmless");
    assert!(!killed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn prompt_response_followed_by_eof_delivers_final_partial_text_and_done() {
    let (backend, mut read, mut write, _) = fake();
    handshake(&backend, &mut read, &mut write).await;
    let mut events = backend.events();
    backend.send_turn(TurnInput::default()).await.unwrap();
    let prompt = receive(&mut read).await;
    send(&mut write, json!({"jsonrpc":"2.0","method":"session/update","params":{
        "sessionId":"fake-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"final partial"}}
    }})).await;
    send(&mut write, crate::mcp::success(prompt["id"].clone(), json!({"stopReason":"end_turn"}))).await;
    write.shutdown().await.unwrap();
    // Let both the reader and closed watch complete before consuming anything.
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(matches!(events.recv().await, Some(AgentEvent::TextDelta(v)) if v["delta"] == "final partial"));
    assert!(matches!(events.recv().await, Some(AgentEvent::Done(_))));
    assert!(matches!(events.recv().await, Some(AgentEvent::Error(_))));
    assert!(events.recv().await.is_none());
}

#[tokio::test]
async fn prompt_rpc_rejection_is_recoverable_without_replaying() {
    let (backend, mut read, mut write, killed) = fake();
    handshake(&backend, &mut read, &mut write).await;
    let mut events = backend.events();
    backend.send_turn(TurnInput::default()).await.unwrap();
    let first = receive(&mut read).await;
    send(&mut write, crate::mcp::error(first["id"].clone(), -32602, "Bearer private-error")).await;
    assert!(
        matches!(events.recv().await, Some(AgentEvent::Done(v)) if v["turn"]["status"] == "failed" && !v.to_string().contains("private-error"))
    );
    assert!(!backend.is_closed());
    backend.send_turn(TurnInput::default()).await.unwrap();
    let next = receive(&mut read).await;
    assert_ne!(next["id"], first["id"]);
    send(&mut write, crate::mcp::success(next["id"].clone(), json!({"stopReason":"end_turn"}))).await;
    assert!(matches!(events.recv().await, Some(AgentEvent::Done(v)) if v["turn"]["status"] == "completed"));
    assert!(!killed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn queued_final_deltas_and_done_are_drained_before_closed() {
    let (backend, _read, _write, _) = fake();
    let mut events = backend.events();
    backend.rpc.emit(AgentEvent::TextDelta(json!({"delta":"final"})));
    backend.rpc.emit(AgentEvent::Done(json!({"turnId":"last"})));
    backend.rpc.stop("child exited");
    assert!(matches!(events.recv().await, Some(AgentEvent::TextDelta(v)) if v["delta"] == "final"));
    assert!(matches!(events.recv().await, Some(AgentEvent::Done(_))));
    assert!(matches!(events.recv().await, Some(AgentEvent::Error(_))));
    assert!(events.recv().await.is_none());
    assert!(matches!(backend.events().recv().await, Some(AgentEvent::Error(_))));
}

#[tokio::test]
async fn permission_approval_announces_tool_id_but_foreign_update_still_trips() {
    for foreign in [false, true] {
        let (backend, mut read, mut write, _) = fake();
        handshake(&backend, &mut read, &mut write).await;
        let mut events = backend.events();
        send(
            &mut write,
            json!({"jsonrpc":"2.0","id":800,"method":"session/request_permission","params":{
                "sessionId":"fake-session","toolCall":{"toolCallId":"permitted","title":"terminal-rs/read_chart"},
                "options":[{"kind":"allow_once","optionId":"yes"}]
            }}),
        )
        .await;
        assert_eq!(receive(&mut read).await["result"]["outcome"]["optionId"], "yes");
        send(
            &mut write,
            json!({"jsonrpc":"2.0","method":"session/update","params":{
                "sessionId":"fake-session","update":{"sessionUpdate":"tool_call_update",
                    "toolCallId":if foreign {"foreign"} else {"permitted"},"status":"completed"}
            }}),
        )
        .await;
        let event = events.recv().await;
        if foreign {
            assert!(matches!(event, Some(AgentEvent::Error(_))));
            assert!(backend.is_unsafe());
        } else {
            assert!(matches!(event, Some(AgentEvent::ToolFinished(_))));
            assert!(!backend.is_unsafe());
        }
    }
}
