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
async fn session_models_select_supported_method_and_reapply_saved_choice() {
    for config in [false, true] {
        let (backend, mut read, mut write, _) = fake();
        let metadata = if config {
            json!({"configOptions":[
                {"id":"agent","category":"mode","type":"select","currentValue":"build",
                 "options":[{"value":"build","name":"Build"}]},
                {"id":"provider-model","category":"model","type":"select","currentValue":"plan/a",
                 "options":[{"group":"plan","name":"Token plan","options":[
                     {"value":"plan/a","name":"Model A"},{"value":"plan/b","name":"Model B"}]}]}]})
        } else {
            json!({"models":{"currentModelId":"plan/a","availableModels":[
                {"modelId":"plan/a","name":"Model A"},{"modelId":"plan/b","name":"Model B"}]},
                "modes":{"currentModeId":"build","availableModes":[{"id":"build","name":"Build"}]}})
        };
        let server = async {
            let init = receive(&mut read).await;
            send(&mut write, crate::mcp::success(init["id"].clone(), json!({"protocolVersion":1}))).await;
            let new = receive(&mut read).await;
            assert_eq!(new["method"], "session/new");
            let mut result = metadata.clone();
            result["sessionId"] = json!("first");
            send(&mut write, crate::mcp::success(new["id"].clone(), result)).await;
        };
        let (result, ()) = tokio::join!(backend.start_thread(options()), server);
        result.unwrap();
        let models = backend.list_models().await.unwrap();
        assert_eq!(models.iter().map(|m| m.display_name.as_str()).collect::<Vec<_>>(), ["plan/a", "plan/b"]);
        assert!(models[0].is_default && !models[1].is_default);
        let method = if config { "session/set_config_option" } else { "session/set_model" };
        let server = async {
            let set = receive(&mut read).await;
            assert_eq!(set["method"], method);
            assert_eq!(set["params"]["sessionId"], "first");
            if config {
                assert_eq!(set["params"]["configId"], "provider-model");
                assert_eq!(set["params"]["value"], "plan/b");
                assert!(set["params"].get("modelId").is_none());
            } else {
                assert_eq!(set["params"]["modelId"], "plan/b");
            }
            send(&mut write, crate::mcp::success(set["id"].clone(), json!({}))).await;
        };
        let (result, ()) = tokio::join!(backend.select_model("plan/b"), server);
        result.unwrap();
        assert_eq!(*backend.preferred_model.lock().unwrap(), "plan/b");
        // Simulate the settings boundary and a newly connected child. Neither
        // persistence nor reconnect reads any installed provider configuration.
        let saved = serde_json::to_string(&*backend.preferred_model.lock().unwrap()).unwrap();
        let (next, mut next_read, mut next_write, _) = fake();
        next.remember_model(serde_json::from_str(&saved).unwrap());
        let server = async {
            let init = receive(&mut next_read).await;
            send(&mut next_write, crate::mcp::success(init["id"].clone(), json!({"protocolVersion":1}))).await;
            let new = receive(&mut next_read).await;
            let mut result = metadata.clone();
            result["sessionId"] = json!("next");
            send(&mut next_write, crate::mcp::success(new["id"].clone(), result)).await;
            let set = receive(&mut next_read).await;
            assert_eq!(set["method"], method);
            assert_eq!(set["params"]["sessionId"], "next");
            assert_eq!(set["params"][if config { "value" } else { "modelId" }], "plan/b");
            send(&mut next_write, crate::mcp::success(set["id"].clone(), json!({}))).await;
        };
        let (result, ()) = tokio::join!(next.start_thread(options()), server);
        assert_eq!(result.unwrap(), "next");
    }
}

#[test]
fn model_labels_show_variants_without_changing_selection_ids() {
    for (id, name, label) in [
        ("xiaomi/mimo-v2.6-pro-ultraspeed", "MiMo", "xiaomi/mimo-v2.6-pro-ultraspeed"),
        ("deepseek/deepseek-v4-pro/high", "DeepSeek", "deepseek/deepseek-v4-pro · high"),
        ("minimax-coding-plan/MiniMax-M3", "MiniMax", "minimax-coding-plan/MiniMax-M3"),
        ("unqualified", "Reported model", "Reported model"),
    ] {
        let model = acp_model(id, Some(name), true);
        assert_eq!(model.display_name, label);
        assert_eq!(model.id, id);
        assert_eq!(model.model, id);
        assert!(model.is_default);
        assert!(model.supported_reasoning_efforts.is_empty());
    }
}

#[tokio::test]
async fn model_category_wins_over_legacy_models_and_modes_never_become_models() {
    let (backend, _, _, _) = fake();
    let current = "xiaomi/mimo-v2.6-pro-ultraspeed";
    let ids = [current, "deepseek/deepseek-v4-pro/high", "minimax-coding-plan/MiniMax-M3"];
    // Both forms are returned by mimo 0.1.16. Deliberately give the legacy
    // catalogue a different value to prove the category-based one takes priority.
    let modes = json!({"id":"model","category":"mode","type":"select","currentValue":"build",
        "options":[{"value":"build","name":"Build"},{"value":"plan","name":"Plan"}]});
    let config = json!({"id":"provider-model","category":"model","type":"select","currentValue":current,
        "options":ids.iter().map(|id| json!({"value":id,"name":"Reported name"})).collect::<Vec<_>>()});
    backend.update_models(&json!({"configOptions":[modes.clone(),config],
        "models":{"currentModelId":"legacy","availableModels":[{"modelId":"legacy"}]}}));
    assert_eq!(backend.models.lock().unwrap().iter().map(|m| m.id.clone()).collect::<Vec<_>>(), ids);
    assert_eq!(backend.model_config.lock().unwrap().as_deref(), Some("provider-model"));
    // No usable model config: mode-only and empty model configs both fall back.
    for empty_config in [false, true] {
        let mut options = vec![modes.clone()];
        if empty_config {
            options.push(json!({"id":"provider-model","category":"model","type":"select","options":[]}));
        }
        backend.update_models(&json!({"configOptions":options,
            "models":{"currentModelId":current,
                "availableModels":ids.iter().map(|id| json!({"modelId":id})).collect::<Vec<_>>()}}));
        assert!(backend.model_config.lock().unwrap().is_none());
        let models = backend.models.lock().unwrap();
        assert_eq!(models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ids);
        assert!(models[0].is_default && !models[1].is_default && !models[2].is_default);
        assert_eq!(models[1].display_name, "deepseek/deepseek-v4-pro · high");
    }
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
async fn unsafe_kinds_cannot_be_disguised_as_terminal_rs_permissions_or_tools() {
    for kind in ["execute", "edit", "delete", "move", "fetch", "search", "unknown"] {
        for permission in [true, false] {
            let (backend, mut read, mut write, killed) = fake();
            let mut events = backend.events();
            let mut call = json!({"toolCallId":"spoofed","title":"terminal-rs/read_chart","kind":kind,
                "rawInput":{"serverName":"terminal-rs","name":"read_chart"}});
            let frame = if permission {
                json!({"jsonrpc":"2.0","id":"permission","method":"session/request_permission","params":{
                    "toolCall":call,"options":[{"kind":"allow_once","optionId":"yes"},{"kind":"reject_once","optionId":"no"}]
                }})
            } else {
                call["sessionUpdate"] = json!("tool_call");
                json!({"jsonrpc":"2.0","method":"session/update","params":{"update":call}})
            };
            send(&mut write, frame).await;
            if permission {
                assert_eq!(receive(&mut read).await["result"]["outcome"]["optionId"], "no", "{kind}");
            }
            assert_eq!(receive(&mut read).await["method"], "session/cancel");
            assert!(matches!(events.recv().await, Some(AgentEvent::Error(_))));
            assert!(backend.is_unsafe() && killed.load(Ordering::SeqCst), "{kind}");
        }
    }
}

#[tokio::test]
async fn safe_kinds_with_agreeing_structured_identity_allow_once_and_complete() {
    for kind in [Some("other"), Some("read"), None] {
        for title in ["terminal-rs/read_chart", "read_chart"] {
            let (backend, mut read, mut write, killed) = fake();
            let mut events = backend.events();
            let mut call = json!({"toolCallId":"safe","title":title,
                "_meta":{"mcp":{"serverName":"terminal-rs","toolName":"read_chart"}},
                "rawInput":{"serverName":"terminal-rs","name":"read_chart"}});
            if let Some(kind) = kind {
                call["kind"] = json!(kind);
            }
            send(&mut write, json!({"jsonrpc":"2.0","id":"permission","method":"session/request_permission","params":{
                "toolCall":call,"options":[{"kind":"allow_always","optionId":"forever"},{"kind":"allow_once","optionId":"once"},{"kind":"reject_once","optionId":"no"}]
            }})).await;
            assert_eq!(receive(&mut read).await["result"]["outcome"]["optionId"], "once");
            call["sessionUpdate"] = json!("tool_call");
            send(&mut write, json!({"jsonrpc":"2.0","method":"session/update","params":{"update":call}})).await;
            send(
                &mut write,
                json!({"jsonrpc":"2.0","method":"session/update","params":{"update":{
                    "sessionUpdate":"tool_call_update","toolCallId":"safe","status":"completed"}
                }}),
            )
            .await;
            assert!(matches!(events.recv().await, Some(AgentEvent::ToolStarted(_))));
            assert!(matches!(events.recv().await, Some(AgentEvent::ToolFinished(_))));
            assert!(!backend.is_unsafe() && !killed.load(Ordering::SeqCst));
        }
    }
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
fn tool_provenance_is_exact_and_sandbox_has_no_foreign_exec() {
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
        assert!(rpc.terminal_tool(&json!({"title":title}), None).is_some());
    }
    for title in ["bash terminal-rs/read_chart", "terminal-rs/bash", "terminal-rs/read_chart; bash", "read_chart"] {
        assert!(rpc.terminal_tool(&json!({"title":title}), None).is_none());
    }
    let profile = sandbox_profile(
        Path::new("/private/data/agent/mimo"),
        &[PathBuf::from("/private/data/agent/mimo-home")],
        Path::new("/opt/mimo/bin/mimo"),
        Path::new("/app/terminal-rs"),
        Path::new("/private/data/agent/mcp.sock"),
        Path::new("/Users/test"),
    )
    .unwrap();
    assert!(profile.contains("(deny default)"));
    assert_localhost_and_system_rules(&profile);
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
        "/Users/test/.config/opencode/bin/mimo",
        "/Users/test/.ssh/bin/mimo",
        "/Users/test/.codex/bin/mimo",
        "/Users/test/Library/Caches/mimo/bin/mimo",
    ] {
        assert!(
            sandbox_profile(
                Path::new("/private/data/agent/mimo"),
                &[PathBuf::from("/private/data/agent/mimo-home")],
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

fn assert_localhost_and_system_rules(profile: &str) {
    assert!(profile.contains("(subpath \"/usr\")"));
    assert!(!profile.contains("(subpath \"/usr/lib\")"));
    let network: Vec<_> = profile.lines().filter(|line| line.starts_with("(allow network-")).collect();
    assert_eq!(
        network,
        [
            "(allow network-outbound (literal \"/private/data/agent/mcp.sock\") (literal \"/private/var/run/mDNSResponder\") (remote tcp \"*:443\"))",
            "(allow network-bind (local ip \"localhost:*\"))",
            "(allow network-inbound (local ip \"localhost:*\"))",
            "(allow network-outbound (remote ip \"localhost:*\"))",
        ]
    );
}

#[test]
fn own_and_private_profiles_have_narrow_write_and_read_roots() {
    let user = Path::new("/Users/test");
    let own: Vec<_> = OWN_DIRECTORIES.iter().map(|relative| user.join(relative)).collect();
    let private = vec![PathBuf::from("/private/data/agent/mimo-home")];
    for (roots, is_own) in [(&own, true), (&private, false)] {
        let profile = sandbox_profile(
            Path::new("/private/data/agent/workspace"),
            roots,
            Path::new("/opt/mimo/bin/mimo"),
            Path::new("/app/terminal-rs"),
            Path::new("/private/data/agent/mcp.sock"),
            user,
        )
        .unwrap();
        assert!(profile.starts_with("(version 1)\n(deny default)"));
        for path in &own {
            let grant = format!("(subpath {})", crate::isolation::quote(path).unwrap());
            assert_eq!(profile.matches(&grant).count(), if is_own { 2 } else { 0 });
        }
        assert_eq!(profile.contains("(subpath \"/private/data/agent/mimo-home\")"), !is_own);
        assert!(profile.contains("(allow file-write* (subpath \"/private/data/agent/workspace\")"));
        assert!(!profile.contains("(subpath \"/Users/test\")"));
        for forbidden in
            ["/Users/test/.config/opencode", "/Users/test/.codex", "/Users/test/.ssh", "/Users/test/Library"]
        {
            assert!(!profile.contains(forbidden), "{profile}");
        }
        assert_localhost_and_system_rules(&profile);
    }
    // The normal installed binary tree is read-only in Private; Own additionally
    // grants its .mimocode root by explicit consent, never the home root.
    assert!(
        sandbox_profile(
            Path::new("/private/workspace"),
            &own,
            Path::new("/Users/test/.mimocode/bin/mimo"),
            Path::new("/app/terminal-rs"),
            Path::new("/private/mcp.sock"),
            user
        )
        .is_ok()
    );
}

#[test]
fn own_environment_unsets_xdg_and_plugins_are_opt_in() {
    let source = [
        ("HOME", "/wrong"),
        ("PATH", "/bin"),
        ("TMPDIR", "/shared/tmp"),
        ("LANG", "en_US.UTF-8"),
        ("LC_CTYPE", "UTF-8"),
        ("API_KEY", "secret"),
        ("CODEX_HOME", "/wrong"),
        ("XDG_CONFIG_HOME", "/wrong"),
        ("XDG_DATA_HOME", "/wrong"),
        ("XDG_CACHE_HOME", "/wrong"),
        ("XDG_STATE_HOME", "/wrong"),
    ]
    .map(|(key, value)| (key.into(), value.into()));
    let own = mimo_environment(
        source.clone(),
        Path::new("/private/workspace"),
        Path::new("/private/workspace/home"),
        MimoProfile::Own,
    );
    assert_eq!(own.get(std::ffi::OsStr::new("HOME")).unwrap(), "/private/workspace/home");
    assert_eq!(own.get(std::ffi::OsStr::new("TMPDIR")).unwrap(), "/private/workspace/tmp");
    assert_eq!(own.len(), 5);
    assert!(own.keys().all(|key| !key.to_string_lossy().starts_with("XDG_")));
    let private = mimo_environment(
        source,
        Path::new("/private/workspace"),
        Path::new("/private/mimo-home"),
        MimoProfile::Private,
    );
    assert_eq!(private.get(std::ffi::OsStr::new("HOME")).unwrap(), "/private/mimo-home");
    assert_eq!(private.get(std::ffi::OsStr::new("XDG_CONFIG_HOME")).unwrap(), "/private/mimo-home/config");
    assert_eq!(private.get(std::ffi::OsStr::new("XDG_DATA_HOME")).unwrap(), "/private/mimo-home/data");
    assert!(!private.contains_key(std::ffi::OsStr::new("XDG_CACHE_HOME")));
    assert!(!private.contains_key(std::ffi::OsStr::new("XDG_STATE_HOME")));
    assert_eq!(acp_arguments(false), ["acp", "--pure"]);
    assert_eq!(acp_arguments(true), ["acp"]);
}

#[test]
fn own_shadow_home_links_only_existing_roots_with_private_permissions_and_cleanup() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").canonicalize().unwrap();
    let fixture = root.join(format!("mimo-shadow-home-{}", std::process::id()));
    std::fs::DirBuilder::new().mode(0o700).create(&fixture).unwrap();
    let _cleanup = Workspace(fixture.clone());
    // Exercise every subset, including no setup and all five installed roots.
    for mask in 0..(1 << OWN_DIRECTORIES.len()) {
        let case = fixture.join(mask.to_string());
        std::fs::create_dir(&case).unwrap();
        let real = case.join("real");
        std::fs::create_dir(&real).unwrap();
        let expected: Vec<_> = OWN_DIRECTORIES
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1 << index) != 0)
            .map(|(_, relative)| real.join(relative))
            .collect();
        for directory in &expected {
            std::fs::create_dir_all(directory).unwrap();
        }
        let private = case.join("workspace");
        std::fs::DirBuilder::new().mode(0o700).create(&private).unwrap();
        let workspace = Workspace(private.clone());
        let (home, writable) = own_shadow_home(&private, &real).unwrap();
        assert_eq!(home, private.join("home"));
        assert_eq!(writable, expected);
        let env = mimo_environment([], &private, &home, MimoProfile::Own);
        assert_eq!(env.get(std::ffi::OsStr::new("HOME")), Some(&home.clone().into_os_string()));
        assert!(env.keys().all(|key| !key.to_string_lossy().starts_with("XDG_")));
        let parents = ["", ".config", ".local", ".local/share", ".local/state", ".cache"];
        for relative in parents {
            let directory = home.join(relative);
            let metadata = std::fs::symlink_metadata(&directory).unwrap();
            assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
            let mut entries: Vec<_> =
                std::fs::read_dir(&directory).unwrap().map(|entry| entry.unwrap().path()).collect();
            let mut allowed: Vec<_> = parents
                .iter()
                .filter(|p| !p.is_empty())
                .map(|p| home.join(p))
                .chain(expected.iter().map(|p| home.join(p.strip_prefix(&real).unwrap())))
                .filter(|p| p.parent() == Some(directory.as_path()))
                .collect();
            entries.sort();
            allowed.sort();
            assert_eq!(entries, allowed);
        }
        for relative in OWN_DIRECTORIES {
            let target = real.join(relative);
            let link = home.join(relative);
            if expected.contains(&target) {
                assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
                assert_eq!(std::fs::read_link(&link).unwrap(), target);
            } else {
                assert!(!link.exists());
                assert!(!target.exists());
            }
        }
        for absent in [".claude.json", ".claude", ".config/opencode", ".codex", ".ssh"] {
            assert_eq!(std::fs::symlink_metadata(home.join(absent)).unwrap_err().kind(), std::io::ErrorKind::NotFound);
        }
        drop(workspace);
        assert!(!home.exists());
        assert!(!private.exists());
        assert_eq!(own_directories(&real).unwrap(), expected);
    }
}

#[test]
fn own_directory_resolution_never_creates_and_refuses_symlink_redirects() {
    use std::os::unix::fs::symlink;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").canonicalize().unwrap();
    let fixture = root.join(format!("mimo-own-paths-{}", std::process::id()));
    std::fs::create_dir(&fixture).unwrap();
    let _cleanup = Workspace(fixture.clone());
    let home = fixture.join("home");
    std::fs::create_dir(&home).unwrap();
    assert!(own_directories(&home).unwrap().is_empty());
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
    for relative in OWN_DIRECTORIES {
        std::fs::create_dir_all(home.join(relative)).unwrap();
    }
    assert_eq!(own_directories(&home).unwrap(), OWN_DIRECTORIES.map(|relative| home.join(relative)));
    std::fs::remove_dir(home.join(".mimocode")).unwrap();
    let outside = fixture.join("unrelated");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, home.join(".mimocode")).unwrap();
    assert!(own_directories(&home).is_err());
    std::fs::remove_file(home.join(".mimocode")).unwrap();
    std::fs::remove_dir_all(home.join(".config")).unwrap();
    std::fs::create_dir(outside.join("mimocode")).unwrap();
    symlink(&outside, home.join(".config")).unwrap();
    assert!(own_directories(&home).is_err());
}

#[tokio::test]
async fn contradictory_tool_provenance_is_rejected_even_with_a_valid_title() {
    for call in [
        json!({"title":"terminal-rs/read_chart","rawInput":{"serverName":"foreign","name":"read_chart"}}),
        json!({"title":"mcp__terminal-rs__read_chart","rawInput":{"serverName":"terminal-rs","name":"bash"}}),
        json!({"title":"mcp__foreign__bash","rawInput":{"serverName":"terminal-rs","name":"read_chart"}}),
        json!({"title":"terminal-rs/read_chart","kind":"other","rawInput":{"serverName":"terminal-rs","name":"read_data"}}),
        json!({"title":"read_data","rawInput":{"serverName":"terminal-rs","name":"read_chart"}}),
        json!({"title":"terminal-rs/read_chart","_meta":{"serverName":"foreign","toolName":"read_chart"}}),
        json!({"title":"terminal-rs/read_chart","_meta":{"serverName":"terminal-rs","toolName":"read_data"}}),
        json!({"title":"terminal-rs/read_chart","_meta":{"mcp":{"server":"terminal-rs","tool":"read_data"}},"rawInput":{"serverName":"terminal-rs","name":"read_chart"}}),
    ] {
        for permission in [true, false] {
            let (backend, mut read, mut write, killed) = fake();
            let mut events = backend.events();
            if permission {
                send(&mut write, json!({"jsonrpc":"2.0","id":"permission","method":"session/request_permission","params":{
                    "toolCall":call,"options":[{"kind":"allow_once","optionId":"yes"},{"kind":"reject_once","optionId":"no"}]
                }})).await;
                assert_eq!(receive(&mut read).await["result"]["outcome"]["optionId"], "no");
            } else {
                let mut update = call.clone();
                update["sessionUpdate"] = json!("tool_call");
                update["toolCallId"] = json!("spoofed");
                send(&mut write, json!({"jsonrpc":"2.0","method":"session/update","params":{"update":update}})).await;
            }
            assert_eq!(receive(&mut read).await["method"], "session/cancel");
            assert!(matches!(events.recv().await, Some(AgentEvent::Error(_))));
            assert!(backend.is_unsafe() && killed.load(Ordering::SeqCst));
        }
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
async fn approved_tool_updates_cannot_change_kind_or_structured_identity() {
    for fields in [
        json!({"kind":"execute"}),
        json!({"kind":"fetch"}),
        json!({"rawInput":{"name":"read_data"}}),
        json!({"_meta":{"toolName":"read_data"}}),
        json!({"title":"terminal-rs/read_data"}),
    ] {
        let (backend, mut read, mut write, killed) = fake();
        let mut events = backend.events();
        let call =
            json!({"sessionUpdate":"tool_call","toolCallId":"known","title":"terminal-rs/read_chart","kind":"other"});
        send(&mut write, json!({"jsonrpc":"2.0","method":"session/update","params":{"update":call}})).await;
        assert!(matches!(events.recv().await, Some(AgentEvent::ToolStarted(_))));
        let mut update = json!({"sessionUpdate":"tool_call_update","toolCallId":"known","status":"completed"});
        update.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        send(&mut write, json!({"jsonrpc":"2.0","method":"session/update","params":{"update":update}})).await;
        assert_eq!(receive(&mut read).await["method"], "session/cancel");
        assert!(matches!(events.recv().await, Some(AgentEvent::Error(_))));
        assert!(backend.is_unsafe() && killed.load(Ordering::SeqCst));
    }
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
