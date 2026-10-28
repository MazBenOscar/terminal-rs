use super::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tr_agent::mcp::{read_line, socket_path, write_line};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target").canonicalize().unwrap();
        let path =
            root.join(format!("a4-{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn call(app: &mut TerminalApp, stream: &mut BufReader<UnixStream>, id: u64, name: &str, args: Value) -> Value {
    write_line(
        stream.get_mut(),
        &json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}}),
    )
    .await
    .unwrap();
    let receive = read_line(stream);
    tokio::pin!(receive);
    let deadline = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            line = &mut receive => return serde_json::from_slice(&line.unwrap().unwrap()).unwrap(),
            _ = tokio::time::sleep(Duration::from_millis(1)) => app.process_logic(),
            _ = &mut deadline => panic!("app tool did not finish"),
        }
    }
}

#[test]
fn external_notices_coalesce_per_connection_across_scope_rotation() {
    use crate::agent_pane::RenderedBlock;
    let mut pane = crate::agent_pane::AgentPane::default();
    let now = std::time::Instant::now();
    pane.external_notice("external-1", now);
    for _ in 0..10 {
        pane.external_notice("external-1", now + Duration::from_secs(29));
    }
    assert_eq!(pane.blocks.len(), 1);
    assert!(matches!(&pane.blocks[0], RenderedBlock::Notice(s) if s.contains("11 requests")));
    pane.external_notice("external-2", now + Duration::from_secs(29));
    assert_eq!(pane.blocks.len(), 2);
    pane.external_notice("external-1", now + Duration::from_secs(30));
    assert_eq!(pane.blocks.len(), 3);
}

#[test]
fn disabled_app_startup_removes_only_stale_socket() {
    let temp = Temp::new();
    std::fs::create_dir(temp.0.join("agent")).unwrap();
    let path = socket_path(&temp.0);
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    let mut app = TerminalApp::test_workspace(&temp.0);
    app.external_agent_logic();
    assert!(!path.exists());
    // Cleanup is a startup operation, not a per-frame probe.
    let live = std::os::unix::net::UnixListener::bind(&path).unwrap();
    app.external_agent_logic();
    assert!(path.exists());
    drop(live);
}

#[test]
fn external_outcomes_are_readable_only_by_originating_connection_for_both_backends() {
    use crate::agent_mcp::Backend;
    for backend in [Backend::Codex, Backend::Mimo] {
        let temp = Temp::new();
        let mut app = TerminalApp::test_workspace(&temp.0);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            app.agent_config.backend = backend;
            app.agent_config.allow_external = true;
            app.agent_config.allow_external_edits = true;
            app.external_agent_logic();
            let mut owner = BufReader::new(UnixStream::connect(socket_path(&temp.0)).await.unwrap());
            let mut foreign = BufReader::new(UnixStream::connect(socket_path(&temp.0)).await.unwrap());
            let proposed = call(&mut app, &mut owner, 1, "create_watchlist", json!({"name":"Own decision"})).await;
            assert_eq!(proposed["result"]["isError"], false);
            let proposal = app.agent_pane.workflows.proposals[0].clone();
            app.confirm_workflows(&proposal.turn, true);
            for id in [2, 3] {
                let result =
                    call(&mut app, &mut owner, id, "read_proposal_outcomes", json!({"proposal_id":proposal.id})).await;
                let value: Value =
                    serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
                assert_eq!(value["proposals"][0]["status"], "approved");
                assert_eq!(value["proposals"][0]["proposal_id"], proposal.id);
            }
            let result =
                call(&mut app, &mut foreign, 1, "read_proposal_outcomes", json!({"proposal_id":proposal.id})).await;
            let value: Value = serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(value["proposals"], json!([]));
            call(&mut app, &mut owner, 4, "create_watchlist", json!({"name":"Declined decision"})).await;
            let proposal = app.agent_pane.workflows.proposals.last().unwrap().clone();
            app.confirm_workflows(&proposal.turn, false);
            let result = call(&mut app, &mut owner, 5, "read_proposal_outcomes", json!({})).await;
            assert!(result.to_string().contains("declined"));
            call(&mut app, &mut owner, 6, "create_watchlist", json!({"name":"Expired decision"})).await;
            let proposal = app.agent_pane.workflows.proposals.last_mut().unwrap();
            proposal.deadline = std::time::Instant::now();
            let result = call(&mut app, &mut owner, 7, "read_proposal_outcomes", json!({})).await;
            assert!(result.to_string().contains("expired"));
            let approved = app.agent_pane.workflows.proposals[0].clone();
            app.undo_workflows(&approved.turn, &mut Default::default());
            let result =
                call(&mut app, &mut owner, 8, "read_proposal_outcomes", json!({"proposal_id":approved.id})).await;
            let value: Value = serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(value["proposals"][0]["undone"], true);
            assert!(app.agent_pane.workflows.context.is_empty());
            app.agent_config.allow_external = false;
            app.external_agent_logic();
        });
    }
}

#[test]
fn external_preferences_default_off_and_are_not_layout_settings() {
    let settings: crate::agent_mcp::Settings = serde_json::from_str("{}").unwrap();
    assert!(!settings.allow_external && !settings.allow_external_edits);
    assert_eq!(settings.backend, crate::agent_mcp::Backend::Codex);
    let temp = Temp::new();
    let mut app = TerminalApp::test_workspace(&temp.0);
    app.external_agent_logic();
    assert!(!socket_path(&temp.0).exists());
    app.agent_config.backend = crate::agent_mcp::Backend::Mimo;
    let persisted: Value = serde_json::from_slice(&app.settings_json()).unwrap();
    assert_eq!(persisted["agent"]["backend"], "mimo");
    let layout = serde_json::to_value(agent_pane::Settings::default()).unwrap();
    assert!(layout.get("allow_external").is_none() && layout.get("backend").is_none());
    // Changing providers must not reuse the OpenAI/ChatGPT privacy disclosure
    // or present Codex's toolset toggle as mimo's actual socket policy.
    app.agent_pane.backend = "mimo";
    let ctx = egui::Context::default();
    let mut shown = String::new();
    for _ in 0..2 {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900., 1400.))),
                ..Default::default()
            },
            |ui| {
                app.agent_pane.show(ui, "No chart", egui::Id::new("mimo-privacy"));
            },
        );
        for shape in &output.shapes {
            if let egui::Shape::Text(text) = &shape.shape {
                shown.push_str(text.galley.text());
                shown.push('\n');
            }
        }
        output.textures_delta.clear();
    }
    assert!(shown.contains("MiniMax (China-hosted)") && shown.contains("MCP policy"));
    assert!(!shown.contains("go to OpenAI under your ChatGPT account"));
    assert!(!app.agent_pane.acknowledged);
}

#[test]
fn real_app_socket_readonly_budget_and_workflow_approval_share_ui_queue() {
    let temp = Temp::new();
    let mut app = TerminalApp::test_workspace(&temp.0);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        app.agent_config.allow_external = true;
        app.external_agent_logic();
        let mut stream = BufReader::new(UnixStream::connect(socket_path(&temp.0)).await.unwrap());
        let refused = call(&mut app, &mut stream, 1, "create_watchlist", json!({"name":"External proposal"})).await;
        assert_eq!(refused["result"]["isError"], true);
        assert!(refused.to_string().contains("tool_disabled"));
        assert!(app.agent_pane.workflows.proposals.is_empty());
        for id in 2..=8 {
            assert_eq!(call(&mut app, &mut stream, id, "list_watchlists", json!({})).await["result"]["isError"], false);
        }
        let budget = call(&mut app, &mut stream, 9, "list_watchlists", json!({})).await;
        assert!(budget.to_string().contains("budget exhausted"));
        // A policy change closes the old connection, never widens it in place.
        app.external_agent = None;
        app.external_attempted = false;
        app.agent_config.allow_external_edits = true;
        app.external_agent_logic();
        let mut stream = BufReader::new(UnixStream::connect(socket_path(&temp.0)).await.unwrap());
        let proposed = call(&mut app, &mut stream, 1, "create_watchlist", json!({"name":"External proposal"})).await;
        assert_eq!(proposed["result"]["isError"], false, "{proposed}");
        assert_eq!(app.agent_pane.workflows.proposals.len(), 1);
        assert!(app.watchlists.iter().all(|w| w.name != "External proposal"), "no write without a user click");
        let proposal = app.agent_pane.workflows.proposals[0].clone();
        assert!(proposal.turn.starts_with("external-") && proposal.summary.starts_with("External agent"));
        app.confirm_workflows(&proposal.turn, true);
        assert!(app.watchlists.iter().any(|w| w.name == "External proposal"));
        stream.get_mut().shutdown().await.unwrap();
        app.agent_config.allow_external = false;
        app.external_agent_logic();
        assert!(!socket_path(&temp.0).exists());
    });
}

#[test]
fn disabling_external_access_cancels_queued_ui_write_before_execution() {
    let temp = Temp::new();
    let mut app = TerminalApp::test_workspace(&temp.0);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
    app.agent_config.allow_external=true;
    app.agent_config.allow_external_edits=true;
    app.external_agent_logic();
    let mut stream=UnixStream::connect(socket_path(&temp.0)).await.unwrap();
    write_line(&mut stream,&json!({"id":1,"method":"tools/call","params":{"name":"create_watchlist","arguments":{"name":"Never applied"}}})).await.unwrap();
    for _ in 0..200 {
        if app.agent_queue.len()>0 { break; }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(app.agent_queue.len(),1);
    assert!(app.agent_pane.workflows.proposals.is_empty());
    app.agent_config.allow_external=false;
    app.external_agent_logic();
    assert_eq!(app.agent_queue.len(),0);
    app.process_logic();
    assert!(app.agent_pane.workflows.proposals.is_empty());
    assert!(app.watchlists.iter().all(|w| w.name!="Never applied"));
    });
}

#[test]
fn external_chart_analysis_cache_is_connection_local_and_preserves_guardrails() {
    use crate::app::Pane;
    use crate::chart_view::ChartState;
    use std::sync::Arc;
    use tr_core::Timeframe;
    let temp = Temp::new();
    let mut app = TerminalApp::test_workspace(&temp.0);
    let mut chart = ChartState::new(1, "BTCUSD Curncy", Timeframe::D1);
    let bars = Arc::new(tr_core::sample::generate("BTCUSD", Timeframe::D1, 300, 86400));
    chart.on_history(0, bars.clone(), tr_chart::scene::SeriesGpu::build(&bars), 0, false, true, false, &app.worker);
    app.panes.push(Pane { id: 1, kind: PaneKind::Chart(Box::new(chart)), link: None, title: None, window: None });
    app.focus = 1;
    app.observe_edits(false);
    app.agent_config.allow_external = true;
    app.agent_config.allow_external_edits = true;
    app.external_agent_logic();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut first = BufReader::new(UnixStream::connect(socket_path(&temp.0)).await.unwrap());
        let mut second = BufReader::new(UnixStream::connect(socket_path(&temp.0)).await.unwrap());
        let analyzed = call(
            &mut app,
            &mut first,
            1,
            "analyze",
            json!({"symbol":"COINBASE:BTC-USD","timeframe":"1D","mode":"levels","version":2}),
        )
        .await;
        assert_eq!(analyzed["result"]["isError"], false, "{analyzed}");
        let value: Value = serde_json::from_str(analyzed["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        let reference = json!({"analysis_id":value["analysis_id"],"item_id":value["support_resistance"][0]["item_id"]});
        let stolen = call(
            &mut app,
            &mut second,
            1,
            "create_drawing",
            json!({"symbol":"BTCUSD Curncy","kind":"rectangle","analysis_ref":reference}),
        )
        .await;
        assert_eq!(stolen["result"]["isError"], true, "analysis references cannot cross connections");
        let wrong_target = call(
            &mut app,
            &mut first,
            2,
            "create_drawing",
            json!({"symbol":"BTCUSD Curncy","kind":"rectangle","chart_id":"999","analysis_ref":reference}),
        )
        .await;
        assert_eq!(wrong_target["result"]["isError"], true);
        let drawn = call(
            &mut app,
            &mut first,
            3,
            "create_drawing",
            json!({"symbol":"BTCUSD Curncy","kind":"rectangle","analysis_ref":reference}),
        )
        .await;
        assert_eq!(drawn["result"]["isError"], false, "another connection must not erase the first cache: {drawn}");
        let PaneKind::Chart(chart) = &mut app.panes[0].kind else { panic!("chart") };
        assert_eq!(chart.drawings.len(), 1);
        assert_eq!(chart.drawings[0].created_by, tr_chart::drawings::Author::Agent);
        assert!(chart.drawings[0].turn_id.as_deref().unwrap().starts_with("external-"));
        chart.symbol = "ETHUSD Curncy".into();
        let changed = call(
            &mut app,
            &mut first,
            4,
            "create_drawing",
            json!({"symbol":"BTCUSD Curncy","kind":"rectangle","analysis_ref":reference}),
        )
        .await;
        assert_eq!(changed["result"]["isError"], true, "a user-switched pinned chart must fail closed");
        app.agent_config.allow_external = false;
        app.external_agent_logic();
    });
}

#[test]
fn external_approval_cards_render_without_an_inapp_turn_or_consent_flow() {
    use tr_agent_tools::args::{CreateWatchlist, LiveArgs};
    let temp = Temp::new();
    let mut app = TerminalApp::test_workspace(&temp.0);
    app.execute("NEW AGENT");
    app.agent_workflow(
        "external-review:0",
        LiveArgs::CreateWatchlist(CreateWatchlist { name: "External pending".into() }),
    );
    let id = app.first_agent().unwrap();
    let ctx = egui::Context::default();
    for _ in 0..2 {
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.agent_panel(ui, id));
        output.textures_delta.clear();
    }
    assert!(!app.agent_pane.acknowledged);
    assert!(!app.agent_pane.proposal_focus.is_empty(), "external approval must be visible even before starting Codex");
}

#[test]
fn backend_switch_clears_conversation_without_replaying_and_closes_old_socket() {
    use crate::agent_mcp::Backend;
    let temp = Temp::new();
    let mut app = TerminalApp::test_workspace(&temp.0);
    app.agent_config.allow_external = true;
    app.external_agent_logic();
    assert!(socket_path(&temp.0).exists());
    for backend in [Backend::Mimo, Backend::Codex] {
        app.agent_pane.input = "Do not replay this old prompt".into();
        app.agent_pane.notice("Old conversation");
        app.agent_pane.acknowledged = true;
        app.agent_pane.settings.tools = agent_pane::ToolsMode::Edit;
        let old = app.agent_config.clone();
        app.agent_config.backend = backend;
        app.apply_agent_settings(&old);
        assert!(app.agent_pane.input.is_empty() && app.agent_pane.queue.is_empty());
        assert!(app.agent_pane.turns.is_empty() && app.agent_pane.last_prompt.is_none());
        assert_eq!(app.agent_pane.blocks.len(), 1, "only the new-thread notice survives");
        assert_eq!(app.agent_pane.backend, backend.label());
        assert!(matches!(app.agent_pane.state, Onboarding::Dormant));
        assert!(!app.agent_pane.acknowledged && app.agent_pane.session.is_none());
        assert_eq!(app.agent_pane.settings.tools, agent_pane::ToolsMode::Edit);
        assert!(app.external_agent.is_none() && !socket_path(&temp.0).exists());
        app.external_agent_logic();
        assert!(socket_path(&temp.0).exists());
    }
    app.agent_config.allow_external = false;
    app.external_agent_logic();
}

#[test]
fn mimo_session_policy_tracks_external_switch_and_leaves_codex_presets_unchanged() {
    use crate::agent_mcp::{Backend, Settings};
    use tr_agent_tools::ToolSet;
    let mut settings = Settings::default();
    for preset in [ToolSet::ReadOnly, ToolSet::Edit, ToolSet::Workflows] {
        assert_eq!(settings.session_tools(preset), preset);
    }
    settings.backend = Backend::Mimo;
    assert_eq!(settings.session_tools(ToolSet::Workflows), ToolSet::ReadOnly);
    settings.allow_external_edits = true;
    assert_eq!(settings.session_tools(ToolSet::ReadOnly), ToolSet::Workflows);
}

#[test]
fn review_a4_external_approval_outcome_is_not_reported_to_the_inapp_codex_conversation() {
    use tr_agent_tools::args::{CreateWatchlist, LiveArgs};
    let temp = Temp::new();
    let mut app = TerminalApp::test_workspace(&temp.0);
    assert_eq!(app.agent_config.backend, crate::agent_mcp::Backend::Codex);
    app.execute("NEW AGENT");
    app.agent_workflow("external-7:0", LiveArgs::CreateWatchlist(CreateWatchlist { name: "External pick".into() }));
    app.confirm_workflows("external-7:0", true);
    assert!(app.watchlists.iter().any(|w| w.name == "External pick"));
    assert!(
        app.agent_pane.workflows.context.is_empty(),
        "external outcome queued for the Codex prompt: {:?}",
        app.agent_pane.workflows.context
    );
}
