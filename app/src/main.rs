//! terminal-rs: a keyboard-driven, Bloomberg-style market terminal with GPU charts and
//! congressional (STOCK Act) trade tracking.

pub use app::agent_bridge;
mod agent_demo;
mod agent_markdown;
mod agent_mcp;
mod agent_pane;
pub mod agent_queue;
mod agent_session;
mod alert_delivery;
mod anim;
pub mod app;
mod chart_account;
mod chart_compare;
mod chart_context;
mod chart_controls;
mod chart_data;
mod chart_events;
// N2
mod chart_export;
mod chart_fill;
mod chart_legend;
mod chart_navigation;
mod chart_studies;
mod chart_templates;
pub mod chart_view;
mod chart_workflow;
mod command;
mod congress_view;
mod dock;
pub mod drawing_history;
mod drawing_paint;
pub mod edit_log;
mod feed_time;
mod filings_view;
mod layout;
mod layouts;
mod markets_view;
mod palette;
mod prefs;
mod price_alerts;
mod symbol_search;
mod theme;
mod themes;
mod watch_sections;
mod widgets;
use chart_view::drawing_toolbar;
mod security_names;
pub mod worker;

use std::time::Instant;

fn main() -> eframe::Result {
    let stdio = std::env::args_os().nth(1).is_some_and(|arg| arg == "mcp-stdio");
    // Before runtimes, tracing, providers or eframe: all consumers (including
    // sandbox grants and the child shim's cwd) see one absolute canonical path.
    let data = tr_agent::mcp::canonical_data_dir(&tr_alerts::data_dir()).map_err(|_| {
        if stdio {
            eprintln!("terminal-rs MCP stdio stopped: data directory unavailable");
        }
        demo_error("Data directory unavailable".into())
    })?;
    // SAFETY: startup is single-threaded and there are no environment readers yet.
    unsafe {
        std::env::set_var("TR_DATA_DIR", &data);
    }
    if stdio {
        let result =
            tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(anyhow::Error::from).and_then(
                |runtime| {
                    runtime.block_on(tr_agent::mcp::serve_stdio(
                        tokio::io::stdin(),
                        tokio::io::stdout(),
                        tr_agent::mcp::socket_path(&data),
                    ))
                },
            );
        if result.is_err() {
            eprintln!("terminal-rs MCP stdio stopped: protocol I/O failed");
        }
        return result.map_err(|_| demo_error("MCP stdio I/O failed".into()));
    }
    let demo = agent_demo::Config::from_env().map_err(demo_error)?;
    let shot_requested = demo.as_ref().is_some_and(|demo| demo.shot.is_some());
    let outcome: agent_demo::Outcome = Default::default();
    let window_outcome = outcome.clone();
    let _scratch = if demo.is_some() {
        let parent = std::env::var_os("TR_DATA_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target"));
        let scratch = agent_demo::Scratch::create(&parent).map_err(|e| demo_error(e.to_string()))?;
        // SAFETY: startup is still single-threaded, before tracing, eframe, workers,
        // provider construction, or any other reader of the process environment.
        unsafe {
            std::env::set_var("TR_DATA_DIR", &scratch.0);
            std::env::set_var("TR_PROVIDER", "sample");
            std::env::set_var("TR_CONGRESS_REFRESH_MIN", "0");
        }
        Some(scratch)
    } else {
        None
    };
    let _app_lock = tr_alerts::storage::Lock::acquire_app(&tr_alerts::data_dir().join("app.lock"))
        .map_err(|e| eframe::Error::AppCreation(Box::new(e)))?;
    let t0 = Instant::now();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wgpu_core=warn,wgpu_hal=warn,naga=warn".into()),
        )
        .init();

    // MSAA smooths chart lines and drawings; 4x by default on macOS (TR_MSAA overrides).
    let msaa: u16 = std::env::var("TR_MSAA")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| matches!(n, 1 | 2 | 4 | 8))
        .unwrap_or(if cfg!(target_os = "macos") { 4 } else { 1 });
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Terminal")
        .with_app_id("terminal-rs")
        .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).expect("app icon"))
        .with_inner_size([1600.0, 960.0])
        .with_min_inner_size([900.0, 560.0]);
    if let Some(demo) = &demo {
        viewport = viewport
            .with_inner_size([demo.size[0] as f32, demo.size[1] as f32])
            .with_min_inner_size([320.0, 240.0])
            .with_clamp_size_to_monitor_size(false);
    }
    if cfg!(target_os = "macos") {
        // Content extends under a transparent title bar; tr-macos adds the unified
        // toolbar (inset traffic lights) and sidebar vibrancy on the first frame.
        viewport = viewport
            .with_transparent(true)
            .with_fullsize_content_view(true)
            .with_title_shown(false)
            .with_titlebar_shown(false);
    }
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport,
        multisampling: if msaa > 1 { msaa } else { 0 },
        ..Default::default()
    };
    eframe::run_native(
        "terminal-rs",
        options,
        Box::new(move |cc| {
            let rs = cc.wgpu_render_state.as_ref().ok_or("terminal-rs requires the wgpu renderer")?;
            rs.renderer.write().callback_resources.insert(tr_chart::gpu::ChartRenderer::new(
                &rs.device,
                rs.target_format,
                u32::from(msaa),
            ));
            if let Some(demo) = demo {
                let provider: tr_data::Provider =
                    std::sync::Arc::new(tr_data::sample::SampleProvider::at(agent_demo::SAMPLE_NOW, 300));
                let worker = worker::Worker::spawn(cc.egui_ctx.clone(), provider)?;
                return Ok(Box::new(app::TerminalApp::new_agent_demo(&cc.egui_ctx, worker, demo, window_outcome)));
            }
            let questrade = app::questrade_handle();
            let provider = tr_data::from_env_with(questrade.clone());
            let name = provider.name().to_string();
            let worker = worker::Worker::spawn(cc.egui_ctx.clone(), provider)?;
            tracing::info!("window + GPU ready in {:?} (adapter: {})", t0.elapsed(), rs.adapter.get_info().name);
            Ok(Box::new(app::TerminalApp::new(&cc.egui_ctx, worker, name, questrade)))
        }),
    )?;
    if shot_requested {
        outcome
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| Err("Demo window closed before a PNG was written".into()))
            .map_err(demo_error)?;
    }
    Ok(())
}

fn demo_error(message: String) -> eframe::Error {
    eframe::Error::AppCreation(Box::new(std::io::Error::other(message)))
}
