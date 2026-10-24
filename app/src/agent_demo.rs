//! Opt-in, offline development fixtures and viewport capture. Never a Codex session.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Context, Rect};
use serde_json::json;
use tr_agent::bridge::BridgeEvent;

use crate::agent_pane::{AgentPane, Onboarding, Prompt, ToolsMode};

pub const STATES: &[&str] = &[
    "dormant",
    "missing",
    "unsupported",
    "signed-out",
    "signing-in",
    "checking",
    "empty-ready",
    "streaming",
    "answer",
    "markdown",
    "edits",
    "undone",
    "interrupted",
    "queue",
    "tools-unavailable",
    "rate-limited",
    "stopped-unsafe",
    "stopped-recoverable",
    "privacy",
    "proposal",
    "approved",
];
pub const TURN: &str = "000000000000000000000000000000a2c0:1";
pub const SAMPLE_NOW: i64 = 1_792_713_600; // 2026-10-23 UTC, fixed across captures.
pub const SYMBOL: &str = "BTCUSD Crypto";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State(&'static str);
impl State {
    pub fn parse(name: &str) -> Result<Self, String> {
        STATES
            .iter()
            .find(|&&state| state == name)
            .copied()
            .map(Self)
            .ok_or_else(|| format!("Unknown TR_AGENT_DEMO state {name:?}; expected one of: {}", STATES.join(", ")))
    }
    pub fn approved(self) -> bool {
        self.0 == "approved"
    }
    pub fn edits(self) -> bool {
        matches!(self.0, "edits" | "undone")
    }
    pub fn undone(self) -> bool {
        self.0 == "undone"
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub state: State,
    pub shot: Option<PathBuf>,
    pub size: [u32; 2],
    pub pane_only: bool,
    pub agent_width: Option<f32>,
}
impl Config {
    /// Called once, before the app lock, worker, provider or any agent home exists.
    pub fn from_env() -> Result<Option<Self>, String> {
        Self::parse(|key| std::env::var(key).ok())
    }
    fn parse(get: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let Some(name) = get("TR_AGENT_DEMO") else { return Ok(None) };
        let state = State::parse(&name)?;
        let size = get("TR_DEMO_SIZE").unwrap_or_else(|| "1600x960".into());
        let size = size
            .split_once('x')
            .and_then(|(w, h)| Some([w.parse::<u32>().ok()?, h.parse::<u32>().ok()?]))
            .filter(|[w, h]| (320..=8192).contains(w) && (240..=8192).contains(h))
            .ok_or("TR_DEMO_SIZE must be <width>x<height> (320..8192 by 240..8192 logical pixels)")?;
        let agent_width = get("TR_DEMO_AGENT_WIDTH")
            .map(|width| {
                width
                    .parse::<f32>()
                    .ok()
                    .filter(|v| v.is_finite() && *v >= 240.0 && *v <= size[0] as f32 - 240.0)
                    .ok_or("TR_DEMO_AGENT_WIDTH must leave at least 240 logical pixels for each pane")
            })
            .transpose()?;
        let shot = get("TR_DEMO_SHOT").map(PathBuf::from);
        if shot.as_ref().is_some_and(|path| path.as_os_str().is_empty() || path.extension().is_none_or(|e| e != "png"))
        {
            return Err("TR_DEMO_SHOT must name a .png file".into());
        }
        Ok(Some(Self { state, shot, size, pane_only: get("TR_DEMO_PANE_ONLY").as_deref() == Some("1"), agent_width }))
    }
}

/// Even TR_AGENT_DEMO alone cannot read the normal app data or credentials.
/// The caller keeps this guard until the window and all worker threads are gone.
pub struct Scratch(pub PathBuf);
static NEXT_SCRATCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
impl Scratch {
    pub fn create(parent: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(parent)?;
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let sequence = NEXT_SCRATCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = parent.join(format!("agent-pane-demo-{}-{stamp}-{sequence}", std::process::id()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn pane(state: State, chart: u64) -> AgentPane {
    let mut pane = AgentPane::default();
    pane.initialized = true;
    pane.acknowledged = true;
    pane.disclosure = false;
    pane.settings.pinned_chart = Some(chart);
    pane.active_chart = Some(chart);
    // Matches the sample provider's weekly source bars; app integration refreshes this from the chart.
    pane.data_basis = crate::agent_pane::DataBasis {
        sample: true,
        last_bar: Some(tr_core::Timeframe::W1.bucket_start(SAMPLE_NOW)),
        incomplete: Some("Weekly".into()),
    };
    // Same model projection as a model/list reply, with no account or backend access.
    pane.models = vec![
        serde_json::from_value(json!({
            "id":"gpt-6.1-sol", "model":"gpt-6.1-sol", "displayName":"GPT-6.1 Sol", "isDefault":true,
            "defaultReasoningEffort":"low", "supportedReasoningEfforts":[
                {"reasoningEffort":"low","description":"Fast analysis"},
                {"reasoningEffort":"medium","description":"Deeper analysis"}
            ]
        }))
        .expect("demo model fixture"),
    ];
    pane.settings.select_defaults(&pane.models);
    pane.state = match state.0 {
        "dormant" | "privacy" => Onboarding::Dormant,
        "missing" => Onboarding::Missing,
        "unsupported" => Onboarding::Unsupported { found: "codex-cli 0.160.0".into() },
        "signed-out" => Onboarding::SignedOut,
        "signing-in" => Onboarding::SigningIn,
        "checking" => Onboarding::Checking,
        "rate-limited" => Onboarding::RateLimited { reset: demo_reset() },
        "stopped-unsafe" => Onboarding::Stopped {
            reason:
                "Isolation check refused an unexpected backend capability; restart the app after resolving the cause"
                    .into(),
            unsafe_backend: true,
        },
        "stopped-recoverable" => Onboarding::Stopped {
            reason: "The local backend disconnected after three recovery attempts".into(),
            unsafe_backend: false,
        },
        _ => Onboarding::Ready,
    };
    if state.0 == "privacy" {
        pane.acknowledged = false;
        pane.disclosure = true;
    }
    if matches!(
        state.0,
        "streaming" | "answer" | "markdown" | "edits" | "undone" | "interrupted" | "queue" | "tools-unavailable"
    ) {
        let prompt = if state.edits() {
            "Mark support and the rising trend; add RSI 14. Use sample bars only."
        } else if state.0 == "markdown" {
            "Give me a structured weekly review with a comparison table."
        } else {
            "What's the weekly trend on BTC? Use the loaded sample bars."
        };
        pane.start_prompt(Prompt { text: prompt.into(), requires_tools: true });
        pane.event(BridgeEvent::Started { turn: TURN.into() });
        pane.turns[0].local_time = crate::agent_pane::local_time(SAMPLE_NOW);
        if !state.edits() {
            let success = Some(state.0 != "tools-unavailable");
            tool(&mut pane, "read-chart", "read_chart", success, None);
            tool(&mut pane, "read-bars", "read_data", success, None);
            if matches!(state.0, "streaming" | "interrupted") {
                tool(&mut pane, "analyze", "analyze", None, None);
            } else if state.0 != "tools-unavailable" {
                tool(&mut pane, "analyze", "analyze", Some(true), None);
            }
            let reply = match state.0 {
                "streaming" | "interrupted" => {
                    "BTC's weekly trend is recovering. The last three closes are above the 20-week average, but confirmation still depends on"
                }
                "markdown" => MARKDOWN,
                "tools-unavailable" => "I cannot verify the weekly trend because the chart tools did not return data.",
                _ => ANSWER,
            };
            pane.wire("item/completed", &json!({"item":{"type":"agentMessage","id":"reply","text":reply}}));
            if state.0 == "interrupted" {
                pane.interrupt();
                complete(&mut pane, "interrupted");
            } else if state.0 == "tools-unavailable" {
                pane.event(BridgeEvent::ToolsUnavailable {
                    reason: "The backend did not expose the direct chart namespace. No data-backed answer was accepted"
                        .into(),
                });
            } else if state.0 != "streaming" {
                complete(&mut pane, "completed");
            }
        } else {
            pane.expanded_tools.insert(0);
            pane.settings.tools = ToolsMode::Edit;
            pane.session_tools = Some(tr_agent_tools::ToolSet::Edit);
            // Chart object IDs and the completed answer are filled after sample history arrives.
        }
    }
    if matches!(state.0, "proposal" | "approved") {
        use crate::agent_bridge::{Proposal, ProposalStatus};
        pane.settings.tools = ToolsMode::Edit;
        pane.session_tools = Some(tr_agent_tools::ToolSet::Workflows);
        pane.start_prompt(Prompt {
            text: "Create Crypto and Research watchlists for me.".into(),
            requires_tools: false,
        });
        pane.event(BridgeEvent::Started { turn: TURN.into() });
        pane.turns[0].local_time = crate::agent_pane::local_time(SAMPLE_NOW);
        for (id, name) in [(1, "Crypto"), (2, "Research")] {
            tool(
                &mut pane,
                &format!("proposal-{id}"),
                "create_watchlist",
                Some(true),
                Some("Awaiting user confirmation"),
            );
            pane.workflows.proposals.push(Proposal {
                id,
                turn: TURN.into(),
                args: tr_agent_tools::args::LiveArgs::CreateWatchlist(tr_agent_tools::args::CreateWatchlist {
                    name: name.into(),
                }),
                summary: format!("Create watchlist · {name}"),
                user_owned: false,
                selected: true,
                deadline: Instant::now() + Duration::from_secs(600),
                status: ProposalStatus::Pending,
                before: serde_json::Value::Null,
            });
        }
        pane.workflows.next_id = 2;
        pane.wire("item/completed", &json!({"item":{"type":"agentMessage","id":"workflow-reply","text":"These changes are proposals only. Approve the lists you want to create."}}));
        complete(&mut pane, "completed");
    }
    if state.0 == "queue" {
        pane.queue_review = true;
        pane.input = "Check momentum after those prompts.".into();
        for text in ["Compare the last 20 weekly closes.", "Identify the nearest support and resistance."] {
            pane.enqueue(Prompt { text: text.into(), requires_tools: true });
        }
        pane.interrupt(); // Same queue pause as an explicit interruption; no session involved.
    }
    if state.0 == "rate-limited" {
        pane.limits.push(crate::agent_pane::Limit {
            id: "ordinary".into(),
            used: 100.0,
            reset: demo_reset(),
            minutes: 300,
        });
    }
    pane
}

fn demo_reset() -> String {
    crate::agent_pane::reset_time_at(Some(SAMPLE_NOW + 42 * 60), SAMPLE_NOW)
}

pub fn tool(pane: &mut AgentPane, id: &str, name: &str, success: Option<bool>, summary: Option<&str>) {
    let arguments = match name {
        "read_data" => json!({"symbol":SYMBOL, "timeframe":"1W", "limit":20}),
        "analyze" => json!({"symbol":SYMBOL, "mode":"trend", "timeframe":"1W", "limit":20}),
        _ => json!({"chart_id":"pinned"}),
    };
    pane.wire(
        if success.is_some() { "item/completed" } else { "item/started" },
        &json!({"item":{
            "type":"dynamicToolCall", "id":id, "tool":name, "success":success,
            "durationMs":success.map(|_| 42), "arguments":arguments, "editSummary":summary,
        }}),
    );
}
pub fn complete(pane: &mut AgentPane, status: &str) {
    if let Some(turn) = pane.turns.last_mut() {
        turn.started_at = Instant::now() - Duration::from_secs(7);
    }
    pane.event(BridgeEvent::Completed { turn: TURN.into(), status: status.into(), calls: 3, interrupt_elapsed: None });
    if let Some(turn) = pane.turns.last_mut() {
        turn.duration = Some(Duration::from_secs(7));
    }
}

const ANSWER: &str = "BTC's weekly trend is recovering, but it is not yet a confirmed breakout.\n\n- **Momentum:** 3 consecutive higher closes; RSI 14 at **56.8**.\n- **Levels:** support near **$62,400**; resistance near **$68,200**.\n- **Risk:** a weekly close below support would weaken the recovery.\n\nThese are synthetic sample bars, not live quotes. The current weekly bar is provisional.";
pub(crate) const MARKDOWN: &str = "# Weekly chart review\n\n## Recovery, not confirmation\n\nBTC is building **higher lows**, while *momentum* improves. Use `read_data` to verify the loaded bars.\n\n- Support: **$62,400**\n- Resistance: **$68,200**\n\n1. Wait for a completed weekly close.\n2. Compare volume with the 20-week median.\n\n```text\nRSI(14) = 56.8\nclose > SMA(20)\n```\n\n| Window | Close | Change | RSI | Bias |\n| --- | --- | --- | --- | --- |\n| 1 week | $65,840 | +2.4% | 56.8 | Recovering |\n| 4 weeks | $62,100 | +6.0% | 49.2 | Neutral |\n\nLong-line review: BTCUSD-weekly-sample-support-62400-resistance-68200-volume-confirmation-required-before-treating-the-recovery-as-a-breakout.\n\nSynthetic sample data only; not investment advice.";

pub type Outcome = Arc<Mutex<Option<Result<(), String>>>>;
#[derive(Clone, Copy)]
struct DemoShot;

pub struct Demo {
    pub config: Config,
    pub pane_rect: Option<Rect>,
    pub populated: bool,
    started: Instant,
    stable_since: Instant,
    stable_frames: usize,
    geometry: Option<(Rect, Rect)>,
    requested: Option<(Option<Rect>, f32)>,
    focused: bool,
    pub outcome: Outcome,
}
impl Demo {
    pub fn new(config: Config, outcome: Outcome) -> Self {
        Self {
            config,
            pane_rect: None,
            populated: false,
            started: Instant::now(),
            stable_since: Instant::now(),
            stable_frames: 0,
            geometry: None,
            requested: None,
            focused: false,
            outcome,
        }
    }
    pub fn frame(&mut self, ctx: &Context, screen: Rect, ready: bool) {
        if self.config.shot.is_none() || self.outcome.lock().unwrap().is_some() {
            return;
        }
        // macOS does not paint a window hidden behind others; bring the capture window forward once.
        if !self.focused {
            self.focused = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        let image = ctx.input(|input| {
            input.events.iter().find_map(|event| {
                if let egui::Event::Screenshot { image, user_data, .. } = event {
                    user_data.data.as_ref()?.downcast_ref::<DemoShot>()?;
                    Some(image.clone())
                } else {
                    None
                }
            })
        });
        if let Some(image) = image
            && let Some((rect, scale)) = self.requested
        {
            let result = write_shot(self.config.shot.as_ref().unwrap(), &image, rect, scale);
            self.finish(ctx, result);
            return;
        }
        if self.started.elapsed() > Duration::from_secs(30) {
            self.finish(ctx, Err("Demo capture timed out waiting for a stable frame or viewport screenshot".into()));
            return;
        }
        if self.requested.is_none() {
            let geometry = self.pane_rect.map(|pane| (screen, pane));
            if !ready || geometry.is_none() || self.geometry != geometry {
                self.geometry = geometry;
                self.stable_frames = 0;
                self.stable_since = Instant::now();
            } else {
                self.stable_frames += 1;
            }
            // Fonts are installed synchronously by theme::configure. Allow layout, study
            // uploads, load-in sweeps and dock animations to settle, not just a timer.
            if self.stable_frames >= 24 && self.stable_since.elapsed() >= Duration::from_secs(2) {
                self.requested =
                    Some((if self.config.pane_only { self.pane_rect } else { None }, ctx.pixels_per_point()));
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(DemoShot)));
            }
        }
        ctx.request_repaint_after(Duration::from_millis(16));
    }
    pub fn finish(&self, ctx: &Context, result: Result<(), String>) {
        if let Err(reason) = &result {
            tracing::error!("agent demo: {reason}");
        }
        *self.outcome.lock().unwrap() = Some(result);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

fn write_shot(path: &Path, image: &egui::ColorImage, rect: Option<Rect>, scale: f32) -> Result<(), String> {
    let crop = rect.map_or([0, 0, image.width(), image.height()], |rect| {
        let min = (rect.min.to_vec2() * scale).floor().max(egui::Vec2::ZERO);
        let max = (rect.max.to_vec2() * scale).ceil().max(min);
        [min.x as usize, min.y as usize, (max.x - min.x) as usize, (max.y - min.y) as usize]
    });
    let bytes = crate::chart_export::crop_png(image, crop)?;
    std::fs::write(path, bytes).map_err(|e| format!("Writing {}: {e}", path.display()))?;
    println!("{}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_pane::RenderedBlock;

    fn config(values: &[(&str, &str)]) -> Result<Option<Config>, String> {
        Config::parse(|key| values.iter().find(|(k, _)| *k == key).map(|(_, v)| (*v).into()))
    }
    pub(super) fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/agent-demo-tests")
    }

    #[test]
    fn all_exact_state_names_populate_a_session_free_pane() {
        assert_eq!(STATES.len(), 21);
        for name in STATES {
            let state = State::parse(name).unwrap();
            let mut pane = pane(state, 7);
            assert!(pane.session.is_none(), "{name}");
            assert!(pane.initialized);
            assert_eq!(pane.settings.pinned_chart, Some(7));
            assert!(!pane.models.is_empty());
            assert!(pane.data_basis.sample);
            assert_eq!(pane.data_basis.incomplete.as_deref(), Some("Weekly"));
            assert!(pane.data_basis.last_bar.is_some());
            assert!(!pane.settings.model.is_empty());
            assert!(!pane.state.primary().is_empty());
            assert_eq!(pane.settings.response_style, crate::agent_pane::ResponseStyle::Analyst);
            assert_eq!(pane.expanded_tools.contains(&0), state.edits());
            assert!(pane.turns.iter().all(|turn| turn.local_time == crate::agent_pane::local_time(SAMPLE_NOW)));
            let ctx = Context::default();
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                pane.show(ui, "Pinned: BTC/USD · 1W", egui::Id::new("demo-test"));
            });
            assert!(!output.shapes.is_empty(), "{name}");
            output.textures_delta.clear();
            assert!(pane.session.is_none());
            if matches!(*name, "answer" | "markdown" | "streaming" | "interrupted" | "queue" | "tools-unavailable") {
                assert!(
                    pane.blocks.iter().any(|b| matches!(b, RenderedBlock::Reply(blocks) if !blocks.is_empty())),
                    "{name}"
                );
            }
            if *name == "streaming" {
                let tools: Vec<_> = pane
                    .blocks
                    .iter()
                    .filter_map(|b| if let RenderedBlock::Tool(t) = b { Some(t) } else { None })
                    .collect();
                assert_eq!(tools.len(), 3);
                assert_eq!(tools.iter().filter(|t| t.success == Some(true)).count(), 2);
                assert_eq!(tools.iter().filter(|t| t.success.is_none()).count(), 1);
                assert!(pane.running);
            }
            if *name == "queue" {
                assert_eq!(pane.queue.len(), 2);
                assert!(pane.queue_review);
                assert!(!pane.input.trim().is_empty(), "fixture demonstrates Add to queue with an appendable prompt");
                assert!(pane.next_prompt().is_none());
            }
            if *name == "privacy" {
                assert!(!pane.acknowledged && pane.disclosure);
            }
            if *name == "rate-limited" {
                assert!(pane.limits[0].reset.starts_with("in 42 minutes · "));
                assert!(pane.limits[0].reset.ends_with(&crate::agent_pane::local_time(SAMPLE_NOW + 42 * 60)));
                assert!(!pane.limits[0].reset.contains("AM") && !pane.limits[0].reset.contains("PM"));
            }
        }
    }

    #[test]
    fn every_state_has_bounded_stable_layout_at_360_in_all_review_themes() {
        for name in ["Terminal Dark", "Terminal Light", "Bloomberg"] {
            for state in STATES {
                let ctx = Context::default();
                let themes = crate::themes::builtins();
                crate::theme::configure(
                    &ctx,
                    themes.iter().find(|t| t.name == name).unwrap(),
                    crate::theme::DEFAULT_ACCENT,
                );
                let mut pane = pane(State::parse(state).unwrap(), 7);
                let screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 700.0));
                let mut previous = None;
                for frame in 0..8 {
                    let mut measured = Rect::NOTHING;
                    let mut output =
                        ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
                            pane.show(ui, "Pinned: BTC/USD · 1W", egui::Id::new("narrow-demo"));
                            measured = ui.min_rect();
                        });
                    output.textures_delta.clear();
                    assert!(measured.width() <= 360.0 && measured.bottom() <= 700.0, "{name} {state}: {measured:?}");
                    if frame >= 4 {
                        assert_eq!(previous, Some(measured), "unstable {name} {state}");
                    }
                    previous = Some(measured);
                }
                assert!(pane.session.is_none());
            }
        }
    }

    #[test]
    fn unknown_states_fail_and_unset_demo_is_completely_inert() {
        for name in ["", "ready", "Streaming", "signed_out", " answer", "missing "] {
            assert!(State::parse(name).is_err());
            assert!(config(&[("TR_AGENT_DEMO", name)]).is_err());
        }
        assert!(config(&[("TR_DEMO_SIZE", "bad"), ("TR_DEMO_SHOT", "bad")]).unwrap().is_none());
    }

    #[test]
    fn capture_options_validate_sizes_paths_and_narrow_width() {
        let parsed = config(&[
            ("TR_AGENT_DEMO", "answer"),
            ("TR_DEMO_SIZE", "1100x800"),
            ("TR_DEMO_AGENT_WIDTH", "360"),
            ("TR_DEMO_PANE_ONLY", "1"),
            ("TR_DEMO_SHOT", "answer.png"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(parsed.size, [1100, 800]);
        assert_eq!(parsed.agent_width, Some(360.0));
        assert!(parsed.pane_only);
        for size in ["0x0", "900", "900X700", "axb", "8193x700", "100x700"] {
            assert!(config(&[("TR_AGENT_DEMO", "answer"), ("TR_DEMO_SIZE", size)]).is_err());
        }
        for width in ["NaN", "inf", "-2", "9000"] {
            assert!(config(&[("TR_AGENT_DEMO", "answer"), ("TR_DEMO_AGENT_WIDTH", width)]).is_err());
        }
        assert!(config(&[("TR_AGENT_DEMO", "answer"), ("TR_DEMO_SHOT", "")]).is_err());
        assert!(config(&[("TR_AGENT_DEMO", "answer"), ("TR_DEMO_SHOT", "image.jpg")]).is_err());
    }

    #[test]
    fn png_capture_crops_logical_pane_at_retina_scale_and_reports_write_failure() {
        let scratch = Scratch::create(&root()).unwrap();
        let image = egui::ColorImage::filled([200, 160], egui::Color32::from_rgb(12, 34, 56));
        let path = scratch.0.join("pane.png");
        write_shot(&path, &image, Some(Rect::from_min_max(egui::pos2(60.0, 10.0), egui::pos2(100.0, 70.0))), 2.0)
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 80);
        assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 120);
        write_shot(&path, &image, None, 2.0).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 200);
        assert!(write_shot(&scratch.0.join("missing/shot.png"), &image, None, 1.0).is_err());
        assert!(write_shot(&path, &image, Some(Rect::NOTHING), 2.0).is_err());
    }

    #[test]
    fn capture_waits_for_loaded_content_and_stable_frames_then_uses_viewport_event() {
        let scratch = Scratch::create(&root()).unwrap();
        let shot = scratch.0.join("frame.png");
        let config = Config {
            state: State::parse("answer").unwrap(),
            shot: Some(shot.clone()),
            size: [900, 600],
            pane_only: false,
            agent_width: None,
        };
        let outcome: Outcome = Default::default();
        let mut demo = Demo::new(config, outcome.clone());
        let screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
        demo.pane_rect = Some(screen);
        let ctx = Context::default();
        for _ in 0..30 {
            demo.frame(&ctx, screen, false);
        }
        assert!(demo.requested.is_none());
        demo.frame(&ctx, screen, true);
        demo.stable_since = Instant::now() - Duration::from_secs(3);
        for _ in 0..22 {
            demo.frame(&ctx, screen, true);
        }
        assert!(demo.requested.is_none());
        demo.frame(&ctx, screen, true);
        assert!(demo.requested.is_some());
        assert!(!shot.exists());
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Screenshot {
                    viewport_id: egui::ViewportId::ROOT,
                    user_data: egui::UserData::new(DemoShot),
                    image: Arc::new(egui::ColorImage::filled([9, 6], egui::Color32::BLACK)),
                }],
                ..Default::default()
            },
            |_| demo.frame(&ctx, screen, true),
        );
        output.textures_delta.clear();
        assert!(shot.exists());
        assert_eq!(*outcome.lock().unwrap(), Some(Ok(())));
    }

    #[test]
    fn capture_timeout_records_failure_and_closes_instead_of_hanging() {
        let config = config(&[("TR_AGENT_DEMO", "answer"), ("TR_DEMO_SHOT", "unused.png")]).unwrap().unwrap();
        let outcome: Outcome = Default::default();
        let mut demo = Demo::new(config, outcome.clone());
        demo.started = Instant::now() - Duration::from_secs(31);
        demo.frame(&Context::default(), Rect::NOTHING, false);
        assert!(matches!(&*outcome.lock().unwrap(), Some(Err(reason)) if reason.contains("timed out")));
    }
}
