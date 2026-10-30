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
        assert_eq!(new["params"]["mcpServers"], json!([]));
        send(write,crate::mcp::success(new["id"].clone(),json!({"sessionId":"fake-session","models":{"currentModelId":"model-a","availableModels":[{"modelId":"model-a","name":"Model A"}]}}))).await;
    };
    let (session, ()) = tokio::join!(backend.start_thread(options()), server);
    assert_eq!(session.unwrap(), "fake-session");
}

async fn attach_analysis(
    backend: &AcpBackend,
) -> (Arc<AcpBackend>, BufReader<ReadHalf<DuplexStream>>, WriteHalf<DuplexStream>, Arc<AtomicBool>) {
    let (mut child, mut read, mut write, killed) = fake();
    handshake(&child, &mut read, &mut write).await;
    child.workspace = PathBuf::from("/private/analysis-empty");
    child.rpc.analysis.store(true, Ordering::SeqCst);
    let child = Arc::new(child);
    *backend.analysis_child.lock().unwrap() = Some(child.clone());
    (child, read, write, killed)
}

#[tokio::test]
async fn story_analysis_is_history_free_restores_main_and_rejects_retired_tools() {
    let (backend, mut read, mut write, killed) = fake();
    handshake(&backend, &mut read, &mut write).await;
    let instructions = backend.instructions.lock().unwrap().clone();
    let (child, mut read, mut write, child_killed) = attach_analysis(&backend).await;
    let server = async {
        let new = receive(&mut read).await;
        assert_eq!(new["method"], "session/new");
        assert_eq!(new["params"]["mcpServers"], json!([]));
        send(&mut write, crate::mcp::success(new["id"].clone(), json!({"sessionId":"story"}))).await;
        let prompt = receive(&mut read).await;
        assert_eq!(prompt["params"]["sessionId"], "story");
        assert_eq!(prompt["params"]["prompt"][0]["text"], "untrusted news");
        send(&mut write, json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"story","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Isolated answer\n"}}}})).await;
        send(&mut write, crate::mcp::success(prompt["id"].clone(), json!({"stopReason":"end_turn"}))).await;
    };
    let (result, ()) =
        tokio::join!(backend.analyze(TurnInput { text: "untrusted news".into(), ..Default::default() }), server);
    assert_eq!(result.unwrap(), "Isolated answer\n");
    assert_eq!(backend.rpc.session.lock().unwrap().as_deref(), Some("fake-session"));
    assert_eq!(*backend.instructions.lock().unwrap(), instructions);
    assert!(!killed.load(Ordering::SeqCst));
    let mut events = backend.events();
    child.rpc.route(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"story","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"late injection"}}}})).await.unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(10), events.recv()).await.is_err());
    child.rpc.route(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"story","update":{"sessionUpdate":"tool_call_update","toolCallId":"late"}}})).await.unwrap();
    assert!(child.is_unsafe());
    assert!(child_killed.load(Ordering::SeqCst));
    assert!(!backend.is_unsafe());
    assert!(!killed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn story_analysis_tool_call_is_a_tripwire_even_for_terminal_tools() {
    let (backend, mut read, mut write, killed) = fake();
    handshake(&backend, &mut read, &mut write).await;
    let (child, mut read, mut write, child_killed) = attach_analysis(&backend).await;
    let server = async {
        let new = receive(&mut read).await;
        send(&mut write, crate::mcp::success(new["id"].clone(), json!({"sessionId":"story"}))).await;
        let _prompt = receive(&mut read).await;
        send(&mut write, json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"story","update":{"sessionUpdate":"tool_call","toolCallId":"x","title":"terminal-rs_read_chart","kind":"other"}}})).await;
    };
    let (result, ()) = tokio::join!(backend.analyze(TurnInput::default()), server);
    assert_eq!(result, Err(crate::AnalysisError::ToolAttempt));
    assert!(child.is_unsafe());
    assert!(child_killed.load(Ordering::SeqCst));
    assert!(!backend.is_unsafe());
    assert!(!killed.load(Ordering::SeqCst));
    assert_eq!(backend.rpc.session.lock().unwrap().as_deref(), Some("fake-session"));
    // The same main connection still accepts the user's next prompt.
    backend.send_turn(TurnInput { text: "continue main".into(), ..Default::default() }).await.unwrap();
}

/// The process-wide MIMOCODE_CONFIG_CONTENT registers terminal-rs for every
/// session, and the tool-enabled main session is also created with
/// `mcpServers: []`. An analysis session/new identical to the main one is
/// therefore not tool-less: isolation would rest solely on tripwires.
#[tokio::test]
async fn review_iso_mimo_analysis_session_inherits_process_terminal_mcp() {
    let config = terminal_mcp_config(Path::new("/app/terminal-rs"), Path::new("/data"), "label");
    let global_terminal_mcp = config["mcp"].get("terminal-rs").is_some() && config["tools"]["terminal-rs_*"] == true;
    let (backend, mut read, mut write, _) = fake();
    handshake(&backend, &mut read, &mut write).await;
    let main_params = json!({"cwd":"/private/empty","mcpServers":[]});
    let (_, mut read, mut write, _) = attach_analysis(&backend).await;
    // This is the exact config selected by launch_once for a child without an
    // MCP client label. Unlike session/new, it is process-wide.
    let analysis_config = process_config(Path::new("/app/terminal-rs"), Path::new("/data"), None);
    assert!(analysis_config.get("mcp").is_none());
    assert!(analysis_config["tools"].as_object().unwrap().values().all(|value| value == false));
    assert_eq!(analysis_config["permission"], config["permission"]);
    let server = async {
        let new = receive(&mut read).await;
        assert_eq!(new["method"], "session/new");
        send(&mut write, json!({"jsonrpc":"2.0","id":new["id"],"error":{"code":-32000,"message":"stop here"}})).await;
        new["params"].clone()
    };
    let (_, analysis_params) = tokio::join!(backend.analyze(TurnInput::default()), server);
    assert!(
        !(global_terminal_mcp && analysis_params == main_params),
        "analysis session/new {analysis_params} equals the tool-enabled main session's request while the process config injects terminal-rs"
    );
}

#[tokio::test(start_paused = true)]
async fn story_analysis_timeout_cancels_retires_and_restores_main() {
    let (backend, mut read, mut write, killed) = fake();
    handshake(&backend, &mut read, &mut write).await;
    let (child, mut read, mut write, child_killed) = attach_analysis(&backend).await;
    let server = async {
        let new = receive(&mut read).await;
        send(&mut write, crate::mcp::success(new["id"].clone(), json!({"sessionId":"slow-story"}))).await;
        let _prompt = receive(&mut read).await;
        tokio::time::sleep(Duration::from_secs(91)).await;
        // An unconfirmed timeout now kills the dedicated process, rather than
        // depending on delivery of a best-effort cancellation notification.
        assert!(child_killed.load(Ordering::SeqCst));
    };
    let (result, ()) = tokio::join!(backend.analyze(TurnInput::default()), server);
    assert_eq!(result, Err(crate::AnalysisError::TimedOut));
    assert!(child.rpc.cancelled_sessions.lock().unwrap().contains("slow-story"));
    assert!(child.is_closed());
    assert!(!killed.load(Ordering::SeqCst));
    assert_eq!(backend.rpc.session.lock().unwrap().as_deref(), Some("fake-session"));
}

#[tokio::test(start_paused = true)]
async fn analysis_child_is_lazy_no_mcp_canary_once_reused_and_expires_after_ten_idle_minutes() {
    let (mut backend, mut main_read, mut main_write, main_killed) = fake();
    handshake(&backend, &mut main_read, &mut main_write).await;
    let (mut child, mut read, mut write, killed) = fake();
    child.workspace = PathBuf::from("/private/analysis-empty");
    let child = Arc::new(Mutex::new(Some(child)));
    let launches = Arc::new(AtomicU64::new(0));
    let count = launches.clone();
    backend.analysis_factory = Some(Arc::new(move |config| {
        assert!(config.get("mcp").is_none(), "analysis process must register no MCP servers");
        assert!(config["tools"].as_object().unwrap().values().all(|value| value == false));
        count.fetch_add(1, Ordering::SeqCst);
        let child = child.lock().unwrap().take().unwrap();
        Box::pin(async move { Ok(child) })
    }));
    assert!(backend.analysis_child.lock().unwrap().is_none());
    assert_eq!(launches.load(Ordering::SeqCst), 0);
    let canaries = Arc::new(AtomicU64::new(0));
    let checks = canaries.clone();
    let server = tokio::spawn(async move {
        let mut sessions = 0;
        loop {
            let Some(line) = crate::mcp::read_line(&mut read).await.unwrap() else { break };
            let request: Value = serde_json::from_slice(&line).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "initialize" => json!({"protocolVersion":1}),
                "session/new" => {
                    sessions += 1;
                    assert_eq!(request["params"]["cwd"], "/private/analysis-empty");
                    assert_eq!(request["params"]["mcpServers"], json!([]));
                    json!({"sessionId":format!("analysis-{sessions}")})
                }
                "session/cancel" => continue,
                "session/prompt" => {
                    let text = request["params"]["prompt"][0]["text"].as_str().unwrap();
                    if text.contains("Isolation self-test") {
                        checks.fetch_add(1, Ordering::SeqCst);
                    } else {
                        assert_eq!(text, "story data");
                        send(
                            &mut write,
                            json!({"jsonrpc":"2.0","method":"session/update","params":{
                            "sessionId":request["params"]["sessionId"],"update":{"sessionUpdate":"agent_message_chunk",
                            "content":{"type":"text","text":"Answer\n"}}}}),
                        )
                        .await;
                    }
                    json!({"stopReason":"end_turn"})
                }
                method => panic!("unexpected analysis method {method}"),
            };
            send(&mut write, crate::mcp::success(request["id"].clone(), result)).await;
        }
    });
    let input = || TurnInput { text: "story data".into(), ..Default::default() };
    assert_eq!(backend.analyze(input()).await, Ok("Answer\n".into()));
    tokio::time::advance(Duration::from_secs(500)).await;
    assert!(!killed.load(Ordering::SeqCst));
    assert_eq!(backend.analyze(input()).await, Ok("Answer\n".into()));
    assert_eq!(launches.load(Ordering::SeqCst), 1);
    assert_eq!(canaries.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(599)).await;
    tokio::task::yield_now().await;
    assert!(!killed.load(Ordering::SeqCst), "old idle timer must not stop a reused process");
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert!(killed.load(Ordering::SeqCst));
    assert!(backend.analysis_child.lock().unwrap().is_none());
    assert!(!main_killed.load(Ordering::SeqCst));
    assert!(!backend.is_closed());
    assert_eq!(backend.rpc.session.lock().unwrap().as_deref(), Some("fake-session"));
    assert!(
        tokio::time::timeout(Duration::from_millis(1), crate::mcp::read_line(&mut main_read)).await.is_err(),
        "analysis must never send an RPC to main"
    );
    server.abort();
}

#[tokio::test]
async fn analysis_canary_tripwire_stops_only_child_and_never_runs_story() {
    let (mut backend, _main_read, _main_write, main_killed) = fake();
    let (child, mut read, mut write, child_killed) = fake();
    let child = Arc::new(Mutex::new(Some(child)));
    backend.analysis_factory = Some(Arc::new(move |_| {
        let child = child.lock().unwrap().take().unwrap();
        Box::pin(async { Ok(child) })
    }));
    let server = async {
        let init = receive(&mut read).await;
        send(&mut write, crate::mcp::success(init["id"].clone(), json!({"protocolVersion":1}))).await;
        let new = receive(&mut read).await;
        send(&mut write, crate::mcp::success(new["id"].clone(), json!({"sessionId":"canary"}))).await;
        let prompt = receive(&mut read).await;
        assert!(prompt["params"]["prompt"][0]["text"].as_str().unwrap().contains("Isolation self-test"));
        send(&mut write, json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"canary","update":{"sessionUpdate":"tool_call","toolCallId":"attempt","title":"terminal-rs_read_chart","kind":"other"}}})).await;
    };
    let (result, ()) =
        tokio::join!(backend.analyze(TurnInput { text: "story must not run".into(), ..Default::default() }), server);
    assert_eq!(result, Err(crate::AnalysisError::ToolAttempt));
    assert!(child_killed.load(Ordering::SeqCst));
    assert!(!main_killed.load(Ordering::SeqCst));
    assert!(!backend.is_closed());
    assert!(!backend.is_unsafe());
    assert!(backend.analysis_child.lock().unwrap().is_none());
}

#[tokio::test]
async fn analysis_owner_shutdown_stops_both_processes() {
    let (backend, _read, _write, killed) = fake();
    let (child, _read, _write, child_killed) = attach_analysis(&backend).await;
    backend.shutdown();
    assert!(killed.load(Ordering::SeqCst));
    assert!(child_killed.load(Ordering::SeqCst));
    assert!(child.is_closed());
}

#[tokio::test]
async fn analysis_confirmed_cancel_reuses_child_and_preserves_main() {
    let (backend, mut read, mut write, killed) = fake();
    handshake(&backend, &mut read, &mut write).await;
    let (child, mut read, mut write, child_killed) = attach_analysis(&backend).await;
    let server = async {
        let new = receive(&mut read).await;
        send(&mut write, crate::mcp::success(new["id"].clone(), json!({"sessionId":"cancel-story"}))).await;
        let prompt = receive(&mut read).await;
        backend.interrupt_analysis().await;
        let cancel = receive(&mut read).await;
        assert_eq!(cancel["method"], "session/cancel");
        assert_eq!(cancel["params"]["sessionId"], "cancel-story");
        send(&mut write, crate::mcp::success(prompt["id"].clone(), json!({"stopReason":"cancelled"}))).await;
    };
    let (result, ()) = tokio::join!(backend.analyze(TurnInput::default()), server);
    assert_eq!(result, Err(crate::AnalysisError::Cancelled));
    assert!(!killed.load(Ordering::SeqCst));
    assert!(!child_killed.load(Ordering::SeqCst));
    assert!(Arc::ptr_eq(backend.analysis_child.lock().unwrap().as_ref().unwrap(), &child));
    assert_eq!(backend.rpc.session.lock().unwrap().as_deref(), Some("fake-session"));
}

#[tokio::test]
async fn acp_session_tombstones_are_bounded() {
    let (backend, _read, _write, _) = fake();
    for index in 0..1000 {
        let id = format!("session-{index}");
        backend.rpc.retired_sessions.lock().unwrap().insert(id.clone());
        backend.rpc.cancelled_sessions.lock().unwrap().insert(id);
    }
    for ids in [&backend.rpc.retired_sessions, &backend.rpc.cancelled_sessions] {
        let ids = ids.lock().unwrap();
        assert!(!ids.contains("session-935"));
        assert!(ids.contains("session-936"));
        assert!(ids.contains("session-999"));
    }
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
        json!({"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"terminal-rs_read_chart","kind":"other","rawInput":{},"_meta":null}),
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
async fn echoed_user_chunks_are_ignored_and_tool_boundaries_split_text_without_final_replay() {
    let (backend, _read, _write, killed) = fake();
    // Route directly for deterministic inspection of every emitted event.
    *backend.rpc.turn.lock().unwrap() = "mimo-test".into();
    let mut events = backend.rpc.events.subscribe();
    for update in [
        json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"What's the trend?"}}),
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Reading bars.\nKeep these separated."}}),
        json!({"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"terminal-rs_read_chart","kind":"other"}),
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Verdict: up\n80,799.51"}}),
    ] {
        backend.rpc.route(json!({"jsonrpc":"2.0","method":"session/update","params":{"update":update}})).await.unwrap();
    }
    backend.rpc.finish_chunks();
    backend.rpc.finish_chunks(); // A second flush must emit nothing.
    let mut messages = Vec::<(String, String)>::new();
    let mut order = Vec::new();
    while let Ok(event) = events.try_recv() {
        match event {
            AgentEvent::TextDelta(v) => {
                order.push("text");
                let id = v["itemId"].as_str().unwrap().to_owned();
                let delta = v["delta"].as_str().unwrap();
                if let Some((_, text)) = messages.iter_mut().find(|(key, _)| *key == id) {
                    text.push_str(delta);
                } else {
                    messages.push((id, delta.into()));
                }
            }
            AgentEvent::ToolStarted(_) => order.push("tool"),
            _ => panic!("user echoes must neither display nor stop the backend"),
        }
    }
    assert_eq!(order, ["text", "text", "tool", "text", "text"]);
    assert_eq!(messages.len(), 2);
    assert_ne!(messages[0].0, messages[1].0);
    assert_eq!(messages[0].1, "Reading bars.\nKeep these separated.");
    assert_eq!(messages[1].1, "Verdict: up\n80,799.51");
    assert!(!killed.load(Ordering::SeqCst));
    assert!(!backend.is_unsafe());
}

#[test]
fn child_config_has_one_eager_mcp_server_and_fixed_switches_in_both_profiles() {
    let config = terminal_mcp_config(Path::new("/app/terminal-rs"), Path::new("/canonical/data"), "mimo-launch-token");
    assert_eq!(
        config["mcp"],
        json!({"terminal-rs":{"type":"local","command":["/app/terminal-rs","mcp-stdio"],
            "environment":{"TR_DATA_DIR":"/canonical/data","TR_MCP_CLIENT":"mimo-launch-token"}}})
    );
    // Only terminal-rs tools: every built-in is off and its permission denied.
    assert_eq!(config["tools"]["*"], json!(false));
    assert_eq!(config["tools"]["terminal-rs_*"], json!(true));
    for name in ["bash", "read", "edit", "write", "webfetch", "task"] {
        assert_eq!(config["tools"][name], json!(false), "{name}");
    }
    for name in ["bash", "edit", "webfetch", "read"] {
        assert_eq!(config["permission"][name], json!("deny"), "{name}");
    }
    for profile in [MimoProfile::Own, MimoProfile::Private] {
        let env = mimo_environment(
            [("MIMOCODE_CONFIG_CONTENT".into(), "personal".into()), ("MIMOCODE_MCP_TOOL_SEARCH".into(), "true".into())],
            Path::new("/workspace"),
            Path::new("/home"),
            profile,
        );
        for (key, expected) in [
            ("MIMOCODE_MCP_TOOL_SEARCH", "false"),
            ("MIMOCODE_DISABLE_CLAUDE_CODE", "1"),
            ("MIMOCODE_DISABLE_MODELS_FETCH", "1"),
            ("MIMOCODE_DISABLE_AUTOUPDATE", "1"),
        ] {
            assert_eq!(env.get(std::ffi::OsStr::new(key)).unwrap(), expected);
        }
        assert!(!env.contains_key(std::ffi::OsStr::new("MIMOCODE_CONFIG_CONTENT")));
    }
}

#[tokio::test]
async fn default_model_rejection_is_actionable_and_restart_applies_saved_choice() {
    for saved in [false, true] {
        let (backend, mut read, mut write, killed) = fake();
        if saved {
            backend.remember_model("provider/working".into());
        }
        let server = async {
            let init = receive(&mut read).await;
            send(&mut write, crate::mcp::success(init["id"].clone(), json!({"protocolVersion":1}))).await;
            let new = receive(&mut read).await;
            send(
                &mut write,
                crate::mcp::success(
                    new["id"].clone(),
                    json!({"sessionId":"canary", "models":{
                "currentModelId":"provider/default", "availableModels":[
                    {"modelId":"provider/default"}, {"modelId":"provider/working"}]}}),
                ),
            )
            .await;
            if saved {
                let select = receive(&mut read).await;
                assert_eq!(select["method"], "session/set_model");
                assert_eq!(select["params"]["modelId"], "provider/working");
                send(&mut write, crate::mcp::success(select["id"].clone(), json!({}))).await;
            }
            let prompt = receive(&mut read).await;
            if saved {
                send(&mut write, crate::mcp::success(prompt["id"].clone(), json!({"stopReason":"end_turn"}))).await;
            } else {
                send(
                    &mut write,
                    crate::mcp::error(prompt["id"].clone(), -32603, "Not supported model: private-provider-detail"),
                )
                .await;
                assert_eq!(receive(&mut read).await["method"], "session/cancel");
            }
        };
        let (result, ()) = tokio::join!(backend.canary(), server);
        if saved {
            result.unwrap();
        } else {
            assert_eq!(
                result.unwrap_err().to_string(),
                "mimo's default model isn't available on your plan — pick a model in the Agent pane and Restart"
            );
        }
        assert_eq!(backend.list_models().await.unwrap().len(), 2);
        assert!(!backend.is_unsafe() && !killed.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn canary_uses_fresh_session_and_foreign_attempt_fails_readiness_without_retry() {
    for foreign in [false, true] {
        let (backend, mut read, mut write, killed) = fake();
        handshake(&backend, &mut read, &mut write).await;
        let mut events = backend.rpc.events.subscribe();
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
        let mut attempts = 0;
        while let Ok(event) = events.try_recv() {
            if matches!(event, AgentEvent::Notice(message) if message.starts_with("Checking mimo…")) {
                attempts += 1;
            }
        }
        assert_eq!(attempts, 1, "an unsafe first attempt must never be retried");
        assert_eq!(backend.is_unsafe(), foreign);
        assert_eq!(killed.load(Ordering::SeqCst), foreign);
    }
}

fn retry_update(attempt: u64, kind: &str) -> Value {
    json!({"jsonrpc":"2.0","method":"session/update","params":{"update":{
        "sessionUpdate":"agent_message_chunk","content":{"type":"retry","attempt":attempt,
        "kind":kind,"message":"Cannot connect to API; Bearer fixture-secret"}
    }}})
}

#[tokio::test(start_paused = true)]
async fn user_turn_retries_are_scrubbed_notices_and_continue_until_interrupt() {
    let (backend, mut read, mut write, killed) = fake();
    handshake(&backend, &mut read, &mut write).await;
    let mut events = backend.events();
    backend.send_turn(TurnInput::default()).await.unwrap();
    let prompt = receive(&mut read).await;
    for attempt in 1..=5 {
        send(&mut write, retry_update(attempt, "network")).await;
        let Some(AgentEvent::Notice(message)) = events.recv().await else { panic!("missing retry notice") };
        assert!(message.contains(&format!("provider unreachable (attempt {attempt})")), "{message}");
        assert!(message.contains("Cannot connect to API"), "{message}");
        assert!(!message.contains("fixture-secret"), "{message}");
    }
    tokio::time::advance(Duration::from_secs(301)).await;
    tokio::task::yield_now().await;
    assert!(!backend.is_closed());
    assert!(backend.busy.load(Ordering::SeqCst));
    assert!(!backend.is_unsafe());
    assert!(!killed.load(Ordering::SeqCst));
    assert!(backend.rpc.canary_retries.borrow().is_none());
    backend.interrupt().await.unwrap();
    assert_eq!(receive(&mut read).await["method"], "session/cancel");
    send(&mut write, crate::mcp::success(prompt["id"].clone(), json!({"stopReason":"cancelled"}))).await;
    assert!(matches!(events.recv().await, Some(AgentEvent::Done(_))));
}

#[tokio::test(start_paused = true)]
async fn network_failure_retries_once_on_fresh_session_without_marking_unsafe() {
    for recover in [false, true] {
        let (backend, mut read, mut write, killed) = fake();
        handshake(&backend, &mut read, &mut write).await;
        let mut events = backend.rpc.events.subscribe();
        let server = async {
            for attempt in 1..=2 {
                let new = receive(&mut read).await;
                assert_eq!(new["method"], "session/new");
                let session = format!("canary-{attempt}");
                send(&mut write, crate::mcp::success(new["id"].clone(), json!({"sessionId":session}))).await;
                let prompt = receive(&mut read).await;
                assert_eq!(prompt["method"], "session/prompt");
                assert_eq!(prompt["params"]["sessionId"], session);
                if recover && attempt == 2 {
                    // Two network updates must not inherit the first attempt's count.
                    for _ in 0..2 {
                        send(&mut write, retry_update(1, "network")).await;
                    }
                    send(&mut write, crate::mcp::success(prompt["id"].clone(), json!({"stopReason":"end_turn"}))).await;
                } else {
                    // Count updates, not the untrusted attempt number or other retry kinds.
                    send(&mut write, retry_update(99, "rate_limit")).await;
                    for _ in 0..3 {
                        send(&mut write, retry_update(1, "network")).await;
                    }
                    let cancel = receive(&mut read).await;
                    assert_eq!(cancel["method"], "session/cancel");
                    assert_eq!(cancel["params"]["sessionId"], session);
                }
            }
        };
        let (result, ()) = tokio::join!(backend.canary_with_timeout(Duration::from_millis(100)), server);
        if recover {
            result.unwrap();
        } else {
            assert_eq!(
                result.unwrap_err().to_string(),
                "mimo couldn't reach its model provider for the startup check (network; tried twice). Your provider may be slow or offline — press Restart to try again."
            );
        }
        let mut progress = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let AgentEvent::Notice(message) = event {
                assert!(!message.contains("fixture-secret"));
                if message.starts_with("Checking mimo…") {
                    progress.push(message);
                }
            }
        }
        assert_eq!(progress, ["Checking mimo… (attempt 1 of 2)", "Checking mimo… (attempt 2 of 2)"]);
        assert!(!backend.is_unsafe());
        assert!(!killed.load(Ordering::SeqCst));
        assert!(backend.rpc.pending.lock().unwrap().is_empty());
        assert!(backend.rpc.canary_retries.borrow().is_none());
    }
}

#[tokio::test(start_paused = true)]
async fn canary_overall_timeout_covers_session_setup_and_prompt_and_cancels() {
    for hang_in_setup in [false, true] {
        let (backend, mut read, mut write, _) = fake();
        handshake(&backend, &mut read, &mut write).await;
        let server = async {
            for attempt in 1..=2 {
                let new = receive(&mut read).await;
                assert_eq!(new["method"], "session/new");
                if !hang_in_setup {
                    let session = format!("canary-{attempt}");
                    send(&mut write, crate::mcp::success(new["id"].clone(), json!({"sessionId":session}))).await;
                    assert_eq!(receive(&mut read).await["method"], "session/prompt");
                }
                assert_eq!(receive(&mut read).await["method"], "session/cancel");
            }
        };
        let (result, ()) = tokio::join!(backend.canary_with_timeout(Duration::from_millis(50)), server);
        assert_eq!(
            result.unwrap_err().to_string(),
            "mimo didn't answer its startup check within 2 minutes (tried twice). Your provider may be slow or offline — press Restart to try again."
        );
        assert!(!backend.is_unsafe());
        assert!(backend.rpc.pending.lock().unwrap().is_empty());
        assert!(backend.rpc.canary_retries.borrow().is_none());
    }
}

#[tokio::test(start_paused = true)]
async fn canary_timeout_then_success_retries_on_a_fresh_thread() {
    let (backend, mut read, mut write, killed) = fake();
    handshake(&backend, &mut read, &mut write).await;
    let server = async {
        let first = receive(&mut read).await;
        assert_eq!(first["method"], "session/new");
        send(&mut write, crate::mcp::success(first["id"].clone(), json!({"sessionId":"first-canary"}))).await;
        let old_prompt = receive(&mut read).await;
        assert_eq!(old_prompt["method"], "session/prompt");
        let cancel = receive(&mut read).await;
        assert_eq!(cancel["method"], "session/cancel");
        assert_eq!(cancel["params"]["sessionId"], "first-canary");
        let second = receive(&mut read).await;
        assert_eq!(second["method"], "session/new");
        send(&mut write, crate::mcp::success(second["id"].clone(), json!({"sessionId":"second-canary"}))).await;
        let prompt = receive(&mut read).await;
        assert_eq!(prompt["method"], "session/prompt");
        assert_eq!(prompt["params"]["sessionId"], "second-canary");
        // Late completion of the cancelled turn cannot satisfy the new check.
        send(&mut write, crate::mcp::success(old_prompt["id"].clone(), json!({"stopReason":"cancelled"}))).await;
        send(&mut write, crate::mcp::success(prompt["id"].clone(), json!({"stopReason":"end_turn"}))).await;
    };
    let (result, ()) = tokio::join!(backend.canary_with_timeout(Duration::from_millis(50)), server);
    result.unwrap();
    assert_eq!(backend.rpc.session.lock().unwrap().as_deref(), Some("second-canary"));
    assert!(backend.rpc.retired_sessions.lock().unwrap().contains("first-canary"));
    assert!(backend.rpc.pending.lock().unwrap().is_empty());
    assert!(backend.rpc.canary_retries.borrow().is_none());
    assert!(!backend.is_unsafe() && !killed.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn late_tool_call_from_cancelled_canary_trips_after_retry() {
    for kind in ["tool_call", "tool_call_update"] {
        let (backend, mut read, mut write, killed) = fake();
        handshake(&backend, &mut read, &mut write).await;
        let server = async {
            let first = receive(&mut read).await;
            send(&mut write, crate::mcp::success(first["id"].clone(), json!({"sessionId":"first-canary"}))).await;
            let _old_prompt = receive(&mut read).await;
            let cancel = receive(&mut read).await;
            assert_eq!(cancel["method"], "session/cancel");
            let second = receive(&mut read).await;
            send(&mut write, crate::mcp::success(second["id"].clone(), json!({"sessionId":"second-canary"}))).await;
            let prompt = receive(&mut read).await;
            assert_eq!(prompt["params"]["sessionId"], "second-canary");
            // The abandoned turn keeps running and tries a tool after the retry began.
            send(
                &mut write,
                json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"first-canary",
                "update":{"sessionUpdate":kind,"toolCallId":"t1","title":"bash","status":"in_progress"}}}),
            )
            .await;
            send(&mut write, crate::mcp::success(prompt["id"].clone(), json!({"stopReason":"end_turn"}))).await;
        };
        let (result, ()) = tokio::join!(backend.canary_with_timeout(Duration::from_millis(50)), server);
        assert!(result.is_err(), "{kind}: cancelled canary tool traffic must fail readiness");
        assert!(backend.is_unsafe() && killed.load(Ordering::SeqCst), "{kind}");
    }
}

#[tokio::test]
async fn saved_available_model_is_applied_before_canary_otherwise_current_model_is_used() {
    for config in [false, true] {
        for saved in ["provider/saved", "provider/missing", ""] {
            let (backend, mut read, mut write, _) = fake();
            backend.remember_model(saved.into());
            let server = async {
                let init = receive(&mut read).await;
                send(&mut write, crate::mcp::success(init["id"].clone(), json!({"protocolVersion":1}))).await;
                let new = receive(&mut read).await;
                let metadata = if config {
                    json!({"sessionId":"canary","configOptions":[{"id":"model","category":"model","type":"select",
                        "currentValue":"provider/current","options":[{"value":"provider/current"},{"value":"provider/saved"}]}]})
                } else {
                    json!({"sessionId":"canary","models":{"currentModelId":"provider/current",
                        "availableModels":[{"modelId":"provider/current"},{"modelId":"provider/saved"}]}})
                };
                send(&mut write, crate::mcp::success(new["id"].clone(), metadata)).await;
                if saved == "provider/saved" {
                    let select = receive(&mut read).await;
                    assert_eq!(select["params"]["sessionId"], "canary");
                    if config {
                        assert_eq!(select["method"], "session/set_config_option");
                        assert_eq!(select["params"]["value"], saved);
                    } else {
                        assert_eq!(select["method"], "session/set_model");
                        assert_eq!(select["params"]["modelId"], saved);
                    }
                    send(&mut write, crate::mcp::success(select["id"].clone(), json!({}))).await;
                }
                let prompt = receive(&mut read).await;
                assert_eq!(prompt["method"], "session/prompt");
                send(&mut write, crate::mcp::success(prompt["id"].clone(), json!({"stopReason":"end_turn"}))).await;
            };
            let (result, ()) = tokio::join!(backend.canary(), server);
            result.unwrap();
        }
    }
}

#[tokio::test]
async fn retry_parts_do_not_bypass_tool_tripwires() {
    let (backend, mut read, mut write, _) = fake();
    handshake(&backend, &mut read, &mut write).await;
    let mut events = backend.events();
    send(
        &mut write,
        json!({"jsonrpc":"2.0","method":"session/update","params":{"update":{
            "sessionUpdate":"tool_call","title":"bash","type":"retry","kind":"network"
        }}}),
    )
    .await;
    assert!(matches!(events.recv().await, Some(AgentEvent::Error(_))));
    assert!(backend.is_unsafe());
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
        cancelled_sessions: Default::default(),
        analysis: AtomicBool::new(false),
        turn: Default::default(),
        text: Default::default(),
        message_segment: AtomicU64::new(0),
        thought: Default::default(),
        tool_ids: Default::default(),
        busy: Arc::new(AtomicBool::new(false)),
        canary_retries: watch::channel(None).0,
    };
    for title in
        ["terminal-rs/read_chart", "mcp__terminal-rs__read_chart", "terminal-rs.read_chart", "terminal-rs_read_chart"]
    {
        assert!(
            rpc.terminal_tool(&json!({"title":title,"kind":"other","rawInput":{"chart_id":42},"_meta":null}), None)
                .is_some()
        );
        for kind in ["execute", "edit", "fetch", "delete", "search"] {
            assert!(rpc.terminal_tool(&json!({"title":title,"kind":kind}), None).is_none());
        }
    }
    for title in [
        "bash terminal-rs/read_chart",
        "terminal-rs/bash",
        "terminal-rs/read_chart; bash",
        "read_chart",
        "mcp_tool_search",
        "mcp_tool_use",
        "terminal-rs_mcp_tool_use",
        "terminal-rs_bash",
        "terminal-rs_read_chart extra",
    ] {
        assert!(rpc.terminal_tool(&json!({"title":title}), None).is_none());
    }
    assert!(rpc.terminal_tool(&json!({"title":"terminal-rs_read_chart","kind":"other","rawInput":{"name":"an ordinary argument"},"_meta":null}), None).is_some());
    assert!(rpc.terminal_tool(&json!({"title":"terminal-rs_read_chart","kind":"other","rawInput":{"serverName":"foreign","name":"read_chart"},"_meta":null}), None).is_none());
    // Live mimo 0.1.16: the final update for an approved call has an empty title
    // and repeats the call's arguments; it keeps the approved identity.
    let completed = json!({"title":"","kind":"other","status":"completed","rawInput":{"name":"an argument"},"rawOutput":{},"content":[]});
    assert_eq!(rpc.terminal_tool(&completed, Some("read_chart")).as_deref(), Some("read_chart"));
    // Without an approved ID an empty title is never a terminal-rs tool,
    // and an update may not switch to another tool or an unsafe kind.
    assert!(rpc.terminal_tool(&completed, None).is_none());
    assert!(rpc.terminal_tool(&json!({"title":"terminal-rs_bash","kind":"other"}), Some("read_chart")).is_none());
    assert!(rpc.terminal_tool(&json!({"title":"","kind":"execute"}), Some("read_chart")).is_none());
    let profile = sandbox_profile(
        Path::new("/private/data/agent/mimo"),
        &SandboxRoots {
            readable: vec![PathBuf::from("/private/data/agent/mimo-home")],
            writable: vec![PathBuf::from("/private/data/agent/mimo-home")],
        },
        Path::new("/opt/mimo/bin/mimo"),
        Path::new("/app/terminal-rs"),
        Path::new("/private/data/agent/mcp.sock"),
        Path::new("/Users/test"),
        43123,
    )
    .unwrap();
    assert!(profile.contains("(deny default)"));
    assert_localhost_and_system_rules(&profile, 43123);
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
        "/Users/test/.config/mimocode/bin/mimo",
        "/Users/test/.ssh/bin/mimo",
        "/Users/test/.codex/bin/mimo",
        "/Users/test/Library/Caches/mimo/bin/mimo",
    ] {
        assert!(
            sandbox_profile(
                Path::new("/private/data/agent/mimo"),
                &SandboxRoots {
                    readable: vec![PathBuf::from("/private/data/agent/mimo-home")],
                    writable: vec![PathBuf::from("/private/data/agent/mimo-home")],
                },
                Path::new(binary),
                Path::new("/app/terminal-rs"),
                Path::new("/private/data/agent/mcp.sock"),
                Path::new("/Users/test"),
                43123,
            )
            .is_err(),
            "{binary}"
        );
    }
}

fn assert_localhost_and_system_rules(profile: &str, port: u16) {
    let usr_subpaths: Vec<_> = profile
        .split("(subpath \"")
        .skip(1)
        .filter_map(|part| part.split_once("\")").map(|(path, _)| path))
        .filter(|path| path.starts_with("/usr"))
        .collect();
    assert_eq!(usr_subpaths, ["/usr/lib", "/usr/share"]);
    let metadata = profile.lines().find(|line| line.starts_with("(allow file-read-metadata ")).unwrap();
    let literals: Vec<_> =
        metadata.split("(literal \"").skip(1).map(|part| part.split_once("\")").unwrap().0).collect();
    let unique: HashSet<_> = literals.iter().collect();
    assert_eq!(literals.len(), unique.len(), "duplicate metadata ancestors: {metadata}");
    for path in ["/var", "/etc", "/tmp", "/private", "/private/var", "/private/var/run", "/private/etc", "/private/tmp"]
    {
        assert_eq!(metadata.matches(&format!("(literal \"{path}\")")).count(), 1, "{metadata}");
        assert!(!profile.contains(&format!("(subpath \"{path}\")")), "broad system grant: {path}");
    }
    assert_eq!(profile.matches("(subpath \"/private/etc/ssl\")").count(), 1);
    let reads =
        profile.split_once("(allow file-read* ").unwrap().1.split_once("\n(allow file-read-metadata").unwrap().0;
    assert!(reads.contains("(subpath \"/private/etc/ssl\")"));
    assert!(profile.contains("(allow mach-lookup (global-name \"com.apple.dnssd.service\") (global-name \"com.apple.trustd\") (global-name \"com.apple.trustd.agent\"))"));
    let network: Vec<_> = profile.lines().filter(|line| line.starts_with("(allow network-")).collect();
    assert_eq!(
        network,
        [
            "(allow network-outbound (literal \"/private/data/agent/mcp.sock\") (literal \"/private/var/run/mDNSResponder\") (remote tcp \"*:443\"))".to_owned(),
            format!("(allow network-bind (local ip \"localhost:{port}\"))"),
            format!("(allow network-inbound (local ip \"localhost:{port}\"))"),
            format!("(allow network-outbound (remote ip \"localhost:{port}\"))"),
        ]
    );
    assert!(!profile.lines().filter(|line| line.contains("localhost:")).any(|line| line.contains('*')));
}

#[test]
fn own_and_private_profiles_have_narrow_write_and_read_roots() {
    let user = Path::new("/Users/test");
    let own_paths: Vec<_> = OWN_DIRECTORIES.iter().map(|relative| user.join(relative)).collect();
    let own = SandboxRoots::own(own_paths.clone(), user);
    // Hard-code the expected policy independently of the production constants.
    assert_eq!(own.writable, [user.join(".local/share/mimocode"), user.join(".local/state/mimocode")]);
    let private = SandboxRoots {
        readable: vec![PathBuf::from("/private/data/agent/mimo-home")],
        writable: vec![PathBuf::from("/private/data/agent/mimo-home")],
    };
    for (roots, is_own) in [(&own, true), (&private, false)] {
        for port in [1, 43123, 65535] {
            let profile = sandbox_profile(
                Path::new("/private/data/agent/workspace"),
                roots,
                Path::new("/Users/test/.mimocode/bin/mimo"),
                Path::new("/app/terminal-rs"),
                Path::new("/private/data/agent/mcp.sock"),
                user,
                port,
            )
            .unwrap();
            assert!(profile.starts_with("(version 1)\n(deny default)"));
            let reads = profile.lines().find(|line| line.starts_with("(allow file-read* ")).unwrap();
            let writes = profile.lines().find(|line| line.starts_with("(allow file-write* ")).unwrap();
            let expected_writes = if is_own {
                "(allow file-write* (subpath \"/private/data/agent/workspace\") (subpath \"/Users/test/.local/share/mimocode\") (subpath \"/Users/test/.local/state/mimocode\"))"
            } else {
                "(allow file-write* (subpath \"/private/data/agent/workspace\") (subpath \"/private/data/agent/mimo-home\"))"
            };
            assert_eq!(writes, expected_writes);
            for path in &own_paths {
                let grant = format!("(subpath {})", crate::isolation::quote(path).unwrap());
                assert_eq!(reads.contains(&grant), is_own);
                assert_eq!(writes.contains(&grant), is_own && own.writable.contains(path));
            }
            let install = "(subpath \"/Users/test/.mimocode/bin\")";
            assert!(reads.contains(install));
            assert!(!writes.contains(install));
            assert_eq!(reads.contains("(subpath \"/private/data/agent/mimo-home\")"), !is_own);
            assert!(!profile.contains("(subpath \"/Users/test\")"));
            for forbidden in
                ["/Users/test/.config/opencode", "/Users/test/.codex", "/Users/test/.ssh", "/Users/test/Library"]
            {
                assert!(!profile.contains(forbidden), "{profile}");
            }
            assert_localhost_and_system_rules(&profile, port);
        }
    }
}

#[test]
fn writable_paths_cannot_contain_binary_or_install_directory() {
    let user = Path::new("/Users/test");
    for writable in ["/opt", "/opt/mimo", "/opt/mimo/bin", "/opt/mimo/bin/mimo"] {
        let roots = SandboxRoots { readable: vec![writable.into()], writable: vec![writable.into()] };
        let error = sandbox_profile(
            Path::new("/private/data/agent/workspace"),
            &roots,
            Path::new("/opt/mimo/bin/mimo"),
            Path::new("/app/terminal-rs"),
            Path::new("/private/data/agent/mcp.sock"),
            user,
            43123,
        )
        .unwrap_err();
        assert!(error.to_string().contains("writable path would contain"), "{writable}: {error}");
    }
    let private = SandboxRoots {
        readable: vec![PathBuf::from("/private/data/agent/mimo-home")],
        writable: vec![PathBuf::from("/private/data/agent/mimo-home")],
    };
    for binary in ["/private/data/agent/workspace/bin/mimo", "/private/data/agent/mimo-home/bin/mimo"] {
        assert!(
            sandbox_profile(
                Path::new("/private/data/agent/workspace"),
                &private,
                Path::new(binary),
                Path::new("/app/terminal-rs"),
                Path::new("/private/data/agent/mcp.sock"),
                user,
                43123,
            )
            .unwrap_err()
            .to_string()
            .contains("writable path would contain")
        );
    }
    let own = SandboxRoots::own(OWN_DIRECTORIES.map(|relative| user.join(relative)).to_vec(), user);
    for relative in [".local/share/mimocode/bin/mimo", ".local/state/mimocode/bin/mimo"] {
        assert!(
            sandbox_profile(
                Path::new("/private/data/agent/workspace"),
                &own,
                &user.join(relative),
                Path::new("/app/terminal-rs"),
                Path::new("/private/data/agent/mcp.sock"),
                user,
                43123,
            )
            .is_err()
        );
    }
    assert!(
        sandbox_profile(
            Path::new("/private/data/agent/workspace"),
            &own,
            Path::new("/opt/mimo/bin/mimo"),
            Path::new("/app/terminal-rs"),
            Path::new("/private/data/agent/mcp.sock"),
            user,
            0,
        )
        .unwrap_err()
        .to_string()
        .contains("port must be nonzero")
    );
}

#[test]
fn port_race_retries_once_but_never_retries_an_unsafe_backend() {
    assert!(retry_port_race(0, true, false));
    for (attempt, occupied, unsafe_backend) in
        [(1, true, false), (2, true, false), (0, false, false), (0, true, true), (1, true, true)]
    {
        assert!(!retry_port_race(attempt, occupied, unsafe_backend));
    }
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
    assert_eq!(own.len(), 9);
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
    for port in [1, 43123, 65535] {
        for plugins in [false, true] {
            assert_eq!(acp_arguments(MimoProfile::Own, plugins, port), ["acp", "--port", &port.to_string(), "--pure"]);
        }
        assert_eq!(acp_arguments(MimoProfile::Private, false, port), ["acp", "--port", &port.to_string(), "--pure"]);
        assert_eq!(acp_arguments(MimoProfile::Private, true, port), ["acp", "--port", &port.to_string()]);
    }
}

#[test]
fn own_shadow_home_links_only_existing_roots_with_private_permissions_and_cleanup() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").canonicalize().unwrap();
    let fixture = root.join(format!("mimo-shadow-home-{}", std::process::id()));
    std::fs::DirBuilder::new().mode(0o700).create(&fixture).unwrap();
    let _cleanup = Workspace(fixture.clone());
    // Exercise every subset, including no setup and all four installed roots.
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
        // Personal config exists but must never be linked or granted read access.
        std::fs::create_dir_all(real.join(".config/mimocode")).unwrap();
        let private = case.join("workspace");
        std::fs::DirBuilder::new().mode(0o700).create(&private).unwrap();
        let workspace = Workspace(private.clone());
        let (home, roots) = own_shadow_home(&private, &real).unwrap();
        assert_eq!(home, private.join("home"));
        assert_eq!(roots.readable, expected);
        assert_eq!(
            roots.writable,
            [real.join(".local/share/mimocode"), real.join(".local/state/mimocode")]
                .into_iter()
                .filter(|path| expected.contains(path))
                .collect::<Vec<_>>()
        );
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
        for absent in [".claude.json", ".claude", ".config/opencode", ".config/mimocode", ".codex", ".ssh"] {
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
    std::fs::remove_dir_all(home.join(".local")).unwrap();
    std::fs::create_dir_all(outside.join("share/mimocode")).unwrap();
    symlink(&outside, home.join(".local")).unwrap();
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
