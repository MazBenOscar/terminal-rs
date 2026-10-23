//! App-thread integration. No bridge future owns or locks the app.
use super::{PaneKind, TerminalApp};
use crate::agent_bridge::AppExecutor;
use crate::agent_pane::{self, Action, Onboarding, Settings};
use crate::agent_session::{Command, Event, Session};
use crate::dock::PaneId;
use tr_agent::TurnInput;
use tr_agent::bridge::{BridgeTurn, ChartPin};

fn open_agent_help(url: &str, open: impl FnOnce(&str) -> std::io::Result<()>) -> std::io::Result<()> {
    if !url.starts_with("https://") {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Agent help requires HTTPS"));
    }
    open(url)
}

fn pane_id(id: PaneId) -> egui::Id {
    egui::Id::new(("agent-pane", id))
}

impl TerminalApp {
    fn first_agent(&self) -> Option<PaneId> {
        self.panes.iter().find(|p| matches!(p.kind, PaneKind::Agent(_))).map(|p| p.id)
    }

    pub(super) fn focus_agent_input(&mut self, ctx: &egui::Context) -> bool {
        let Some(id) = self.first_agent() else {
            return false;
        };
        self.focus = id;
        self.dock.activate(id);
        self.focus_cmd = false;
        ctx.memory_mut(|memory| memory.request_focus(agent_pane::input_id(pane_id(id))));
        true
    }

    pub(super) fn agent_captures_focus(&self, ctx: &egui::Context) -> bool {
        agent_pane::captures_focus(
            ctx.memory(|m| m.focused()),
            self.panes.iter().filter(|p| matches!(p.kind, PaneKind::Agent(_))).map(|p| pane_id(p.id)),
        )
    }

    pub(super) fn agent_escape(&mut self, ctx: &egui::Context) -> bool {
        let focused = ctx.memory(|m| m.focused());
        let owns_input =
            self.agent_captures_focus(ctx) || (focused.is_none() && self.first_agent() == Some(self.focus));
        if !self.palette.open
            && owns_input
            && self.agent_pane.running
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.agent_command(Command::Interrupt);
            return true;
        }
        false
    }

    fn initialize_agent_choices(&mut self) {
        if self.agent_pane.initialized {
            return;
        }
        let Some(settings) = self.panes.iter().find_map(|p| match &p.kind {
            PaneKind::Agent(settings) => Some(settings.clone()),
            _ => None,
        }) else {
            return;
        };
        self.pinned_agent_chart = settings
            .pinned_chart
            .filter(|uid| self.panes.iter().any(|p| matches!(&p.kind, PaneKind::Chart(c) if c.uid == *uid)));
        self.agent_pane.settings = settings;
        self.agent_pane.settings.pinned_chart = self.pinned_agent_chart;
        self.agent_pane.initialized = true;
    }

    pub(super) fn agent_saved_settings(&self, settings: &Settings) -> Settings {
        let mut settings =
            if self.agent_pane.initialized { self.agent_pane.settings.clone() } else { settings.clone() };
        settings.pinned_chart =
            if self.agent_pane.initialized { self.pinned_agent_chart } else { settings.pinned_chart };
        settings.pinned_chart = settings
            .pinned_chart
            .filter(|uid| self.panes.iter().any(|p| matches!(&p.kind, PaneKind::Chart(c) if c.uid == *uid)));
        settings
    }

    pub(super) fn agent_logic(&mut self, ctx: &egui::Context) {
        self.initialize_agent_choices();
        if let Some(chart) = self.idx(self.focus).and_then(|i| match &self.panes[i].kind {
            PaneKind::Chart(chart) => Some(chart.uid),
            _ => None,
        }) {
            self.agent_pane.active_chart = Some(chart);
        }
        let visible = self.first_agent().is_some_and(|id| self.visible_panes().iter().any(|&i| self.panes[i].id == id))
            && ctx.input(|i| {
                i.raw.viewports.values().any(|info| info.minimized != Some(true) && info.focused != Some(false))
            });
        let events = self.agent_pane.session.as_ref().map_or_else(Vec::new, |session| session.drain(visible));
        for event in events {
            match event {
                Event::State(state) => {
                    if let Onboarding::Stopped { reason, .. } = &state {
                        self.agent_pane.running = false;
                        self.agent_pane.notice(reason);
                    }
                    self.agent_pane.transition(state);
                }
                Event::LoginUrl(url) => {
                    // Only the account/login/start reply supplies this URL. Never echo it.
                    if !url.starts_with("https://") || super::broker::open_external(&url).is_err() {
                        self.agent_pane.notice("Could not open the sign-in page. Use ‘Open sign-in page again’.");
                    }
                }
                Event::Models(models) => {
                    self.agent_pane.settings.select_defaults(&models);
                    self.agent_pane.models = models;
                }
                Event::Limits(limits) => self.agent_pane.set_limits(&limits),
                Event::Bridge(event) => self.agent_pane.event(event),
                Event::Notice(reason) => self.agent_pane.error_notice(&reason),
            }
        }
        if self
            .pinned_agent_chart
            .is_some_and(|uid| !self.panes.iter().any(|p| matches!(&p.kind, PaneKind::Chart(c) if c.uid == uid)))
        {
            self.pinned_agent_chart = None;
        }
        self.agent_pane.settings.pinned_chart = self.pinned_agent_chart;
        self.refresh_agent_undo();
        self.send_next_agent_prompt();
    }

    pub(super) fn agent_panel(&mut self, ui: &mut egui::Ui, id: PaneId) {
        if self.first_agent() != Some(id) {
            crate::agent_markdown::wrapped(
                ui,
                "Agent is open in another pane. Use its Agent tab; this app shares one session.",
                crate::theme::text(),
            );
            return;
        }
        self.initialize_agent_choices();
        let pin_label = self
            .pinned_agent_chart
            .and_then(|uid| {
                self.panes.iter().find_map(|p| match &p.kind {
                    PaneKind::Chart(c) if c.uid == uid => {
                        Some(format!("{} · {}", crate::security_names::label(&c.symbol), c.tf))
                    }
                    _ => None,
                })
            })
            .unwrap_or_else(|| "No chart pinned".into());
        self.agent_pane.data_basis = self
            .pinned_agent_chart
            .and_then(|uid| {
                self.panes.iter().find_map(|pane| match &pane.kind {
                    PaneKind::Chart(chart) if chart.uid == uid => {
                        Some(agent_pane::DataBasis::from_chart(chart, self.provider.eq_ignore_ascii_case("sample")))
                    }
                    _ => None,
                })
            })
            .unwrap_or_default();
        self.agent_pane.focus_shortcut = self
            .prefs
            .hotkeys
            .chord(crate::prefs::Action::FocusAgentInput)
            .map_or_else(|| "Shortcut unassigned".into(), |chord| chord.display());
        self.refresh_agent_undo();
        let actions = self.agent_pane.show(ui, &pin_label, pane_id(id));
        for action in actions {
            self.agent_action(ui.ctx(), id, action);
        }
        self.send_next_agent_prompt();
    }

    fn start_agent_session(&mut self, ctx: &egui::Context) {
        if self.agent_demo.is_some() {
            return;
        }
        let Some(tools) = self.agent_pane.prepare_session_start() else { return };
        self.agent_pane.session = None;
        self.agent_pane.session =
            Some(Session::start(ctx.clone(), AppExecutor::new(self), tr_alerts::data_dir(), tools));
    }

    fn refresh_agent_undo(&mut self) {
        let available: Vec<_> = self.undoable_agent_turns().into_iter().map(str::to_owned).collect();
        let last = self.last_agent_turn().map(str::to_owned);
        self.agent_pane.refresh_undo(&available, last.as_deref());
        for turn in &mut self.agent_pane.turns {
            if let Some(id) = &turn.id {
                let changes = self
                    .panes
                    .iter()
                    .filter_map(|pane| match &pane.kind {
                        PaneKind::Chart(chart) => Some(chart.agent_turn_change_count(id)),
                        _ => None,
                    })
                    .sum();
                if turn.undo_available || turn.chart_changes.is_none() {
                    turn.chart_changes = Some(changes);
                }
            }
        }
    }

    fn agent_action(&mut self, ctx: &egui::Context, id: PaneId, action: Action) {
        // Fixtures are for visual review only, including their enabled-looking controls.
        if self.agent_demo.is_some() {
            return;
        }
        match action {
            Action::Check => self.start_agent_session(ctx),
            Action::SetTools(mode) => {
                if self.agent_pane.select_tools(mode, crate::agent_bridge::agent_edit_tools_available())
                    && self.agent_pane.session.is_some()
                {
                    self.start_agent_session(ctx);
                }
            }
            Action::UndoTurn(turn) => {
                if !self.agent_pane.running {
                    let result = self.undo_agent_turn(&turn);
                    self.agent_pane.undo_result(&turn, result);
                    self.refresh_agent_undo();
                }
            }
            Action::SignIn => self.agent_command(Command::SignIn),
            Action::CancelLogin => self.agent_command(Command::CancelLogin),
            Action::ReopenLogin => self.agent_command(Command::ReopenLogin),
            Action::InstallationInstructions => {
                if open_agent_help(agent_pane::INSTALLATION_URL, super::broker::open_external).is_err() {
                    self.agent_pane.error_notice(
                        "Could not open installation instructions. Visit https://developers.openai.com/codex.",
                    );
                }
            }
            Action::RefreshLimits => self.agent_command(Command::RefreshLimits),
            Action::Interrupt => self.agent_command(Command::Interrupt),
            Action::Retry => {
                self.agent_pane.transition(Onboarding::Ready);
                if let Some(prompt) = self.agent_pane.last_prompt.clone()
                    && let Some(prompt) = self.agent_pane.start_prompt(prompt)
                {
                    self.send_agent_prompt(prompt);
                }
            }
            Action::Send(prompt) => {
                if matches!(self.agent_pane.state, Onboarding::ToolsUnavailable { .. }) {
                    self.agent_pane.transition(Onboarding::Ready);
                }
                self.agent_pane.enqueue(prompt);
            }
            Action::Pin => {
                self.pinned_agent_chart = self
                    .agent_pane
                    .active_chart
                    .filter(|uid| self.agent_chart(crate::agent_queue::ChartTarget::Uid(*uid)).is_some())
                    .or_else(|| self.peek_chart().map(|c| c.uid));
                self.agent_pane.settings.pinned_chart = self.pinned_agent_chart;
            }
            Action::Close => {
                self.close_pane(id);
            }
        }
    }

    fn agent_command(&mut self, command: Command) {
        if self.agent_demo.is_some() {
            return;
        }
        if matches!(command, Command::Interrupt) {
            self.agent_pane.interrupt();
        }
        if !self.agent_pane.session.as_ref().is_some_and(|session| session.send(command)) {
            self.agent_pane.running = false;
            self.agent_pane.notice("Agent background task ended. No prompt was resent.");
            self.agent_pane.transition(Onboarding::Stopped {
                reason: "Agent background task ended".into(),
                unsafe_backend: false,
            });
        }
    }

    fn send_next_agent_prompt(&mut self) {
        if self.agent_pane.session.is_none() || self.agent_pane.models.is_empty() {
            return;
        }
        let Some(prompt) = self.agent_pane.next_prompt() else { return };
        self.send_agent_prompt(prompt);
    }

    fn send_agent_prompt(&mut self, prompt: agent_pane::Prompt) {
        // Without a valid pin, pin the chart the user last focused (else the visible chart) so
        // the turn's chart and later Pinned lookups agree; the header shows the pin.
        let pinned_valid = self
            .pinned_agent_chart
            .is_some_and(|uid| self.agent_chart(crate::agent_queue::ChartTarget::Uid(uid)).is_some());
        if !pinned_valid {
            self.pinned_agent_chart = self
                .agent_pane
                .active_chart
                .filter(|uid| self.agent_chart(crate::agent_queue::ChartTarget::Uid(*uid)).is_some())
                .or_else(|| self.peek_chart().map(|c| c.uid));
            self.agent_pane.settings.pinned_chart = self.pinned_agent_chart;
        }
        let pin = self
            .pinned_agent_chart
            .and_then(|uid| self.agent_chart(crate::agent_queue::ChartTarget::Uid(uid)))
            .map(|chart| ChartPin {
                uid: chart.uid,
                symbol: chart.symbol.clone(),
                timeframe: chart.tf.to_string(),
                visible_range: chart.visible_date_range().map(|range| (range.start, range.end)),
            })
            .unwrap_or(ChartPin { uid: 0, symbol: String::new(), timeframe: String::new(), visible_range: None });
        let model = self
            .agent_pane
            .models
            .iter()
            .find(|m| m.id == self.agent_pane.settings.model)
            .map_or_else(|| self.agent_pane.settings.model.clone(), |m| m.model.clone());
        self.agent_command(Command::Turn(Box::new(BridgeTurn {
            input: TurnInput {
                model,
                effort: self.agent_pane.settings.effort.clone(),
                text: prompt.text,
                response_style: Some(self.agent_pane.settings.response_style),
                ..Default::default()
            },
            pin,
            requires_tools: prompt.requires_tools,
            screenshot_consent: false,
        })));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_help_uses_the_exact_https_url_and_rejects_other_schemes_before_opening() {
        let mut opened = None;
        open_agent_help(agent_pane::INSTALLATION_URL, |url| {
            opened = Some(url.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(opened.as_deref(), Some("https://developers.openai.com/codex"));
        for url in ["http://developers.openai.com/codex", "file:///tmp/help", "javascript:help()", "https-not-a-url"] {
            assert!(open_agent_help(url, |_| panic!("non-HTTPS reached external opener")).is_err());
        }
        assert!(
            open_agent_help(agent_pane::INSTALLATION_URL, |_| Err(std::io::Error::other("opener failed"))).is_err()
        );
    }

    #[test]
    fn agent_input_shortcut_focuses_existing_pane_without_starting_or_acknowledging() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/agent-input-shortcut");
        let mut app = TerminalApp::test_workspace(&root);
        let ctx = egui::Context::default();
        assert!(!app.focus_agent_input(&ctx));
        app.execute("NEW AGENT");
        let agent = app.first_agent().unwrap();
        app.execute("NEW CHART");
        let binding = crate::prefs::Keymap::default().chord(crate::prefs::Action::FocusAgentInput).unwrap();
        assert_eq!(
            app.prefs.hotkeys.action_for(egui::Key::L, egui::Modifiers::COMMAND),
            Some(crate::prefs::Action::FocusAgentInput)
        );
        assert!(binding.matches(egui::Key::L, egui::Modifiers::COMMAND));
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::L,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND,
                }],
                ..Default::default()
            },
            |_| {
                ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("command-line")));
                app.hotkeys(&ctx);
                assert_eq!(ctx.memory(|memory| memory.focused()), Some(agent_pane::input_id(pane_id(agent))));
            },
        );
        output.textures_delta.clear();
        assert_eq!(app.focus, agent);
        assert!(app.dock.visible().contains(&agent));
        assert!(!app.focus_cmd);
        assert!(app.agent_pane.session.is_none());
        assert!(!app.agent_pane.acknowledged, "focus never skips privacy consent");
    }

    #[test]
    fn demo_cannot_start_a_session_even_when_start_signin_or_retry_is_requested() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/agent-demo-action-tests");
        let scratch = crate::agent_demo::Scratch::create(&root).unwrap();
        let mut app = TerminalApp::test_workspace(&scratch.0);
        let config = crate::agent_demo::Config {
            state: crate::agent_demo::State::parse("dormant").unwrap(),
            shot: None,
            size: [1100, 800],
            pane_only: false,
            agent_width: None,
        };
        app.agent_demo = Some(crate::agent_demo::Demo::new(config, Default::default()));
        app.execute("NEW AGENT");
        let id = app.first_agent().unwrap();
        let ctx = egui::Context::default();
        app.start_agent_session(&ctx);
        for action in [Action::Check, Action::SignIn, Action::Retry, Action::RefreshLimits] {
            app.agent_action(&ctx, id, action);
        }
        app.agent_command(Command::SignIn);
        app.send_next_agent_prompt();
        assert!(app.agent_pane.session.is_none());
        assert_eq!(app.agent_pane.state, Onboarding::Dormant);
        assert!(app.agent_pane.blocks.is_empty());
    }

    #[test]
    fn escape_interrupts_only_agent_input_or_a_pane_without_another_focus() {
        for (focus, palette, interrupted) in [
            (None, false, true),
            (Some("agent"), false, true),
            (Some("agent"), true, false),
            (Some("other-text"), false, false),
            (Some("command-line"), false, false),
        ] {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/agent-escape-owner");
            let mut app = TerminalApp::test_workspace(&root);
            app.execute("NEW AGENT");
            let id = app.first_agent().unwrap();
            app.focus = id;
            app.palette.open = palette;
            app.agent_pane.running = true;
            let ctx = egui::Context::default();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |_| {
                    if let Some(focus) = focus {
                        ctx.memory_mut(|m| {
                            m.request_focus(if focus == "agent" {
                                agent_pane::input_id(pane_id(id))
                            } else {
                                egui::Id::new(focus)
                            })
                        });
                    }
                    app.hotkeys(&ctx);
                },
            );
            output.textures_delta.clear();
            // No real session: attempting the interrupt gives a recoverable stop.
            assert_eq!(!app.agent_pane.running, interrupted, "focus={focus:?}, palette={palette}");
        }
    }

    #[test]
    fn pane_undo_action_runs_on_app_and_reports_conflicts_and_layout_unavailability() {
        use std::sync::Arc;
        use tr_agent::bridge::{BridgeEvent, ToolCall, ToolExecutor};
        use tr_agent_tools::{Content, Registry, ToolSet};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/agent-wire-undo");
        let mut app = TerminalApp::test_workspace(&root);
        app.execute("NEW CHART");
        app.execute("NEW AGENT");
        let id = app.first_agent().unwrap();
        app.initialize_agent_choices();
        app.pinned_agent_chart = app.peek_chart().map(|c| c.uid);
        app.agent_pane.state = Onboarding::Ready;
        let pin = AppExecutor::pin(&app).unwrap();
        let executor = AppExecutor::new(&app);
        let turn = tr_agent::bridge::TurnIds::new().unwrap().allocate().unwrap();
        app.agent_pane
            .start_prompt(agent_pane::Prompt { text: "add two RSI studies".into(), requires_tools: false })
            .unwrap();
        app.agent_pane.event(BridgeEvent::Started { turn: turn.clone() });
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            for keep in [true, false] {
                let mut future = executor.execute(
                    Arc::new(Registry::new(ToolSet::Edit)),
                    ToolCall {
                        turn: turn.clone(),
                        pin: pin.clone(),
                        name: "add_indicator".into(),
                        arguments: "{\"code\":\"RSI\",\"params\":[14]}".into(),
                        confirmed: false,
                    },
                );
                assert!(futures::poll!(future.as_mut()).is_pending());
                app.process_logic();
                let result = future.await;
                assert!(result.success);
                if keep {
                    let Content::Text(text) = &result.content[0] else { panic!("text result") };
                    let value: serde_json::Value = serde_json::from_str(text).unwrap();
                    let study = value["id"].as_str().unwrap().split_once(':').unwrap().1.parse().unwrap();
                    app.agent_chart_mut(crate::agent_queue::ChartTarget::Pinned).unwrap().keep_agent_study(study);
                }
            }
        });
        app.agent_pane.event(BridgeEvent::Completed {
            turn: turn.clone(),
            status: "completed".into(),
            calls: 2,
            interrupt_elapsed: None,
        });
        app.refresh_agent_undo();
        assert!(app.agent_pane.turns[0].undo_available);
        let ctx = egui::Context::default();
        app.agent_pane.running = true;
        app.agent_action(&ctx, id, Action::UndoTurn(turn.clone()));
        assert!(app.agent_turn_undo_available(&turn), "undo must wait for the active turn");
        app.agent_pane.running = false;
        app.agent_action(&ctx, id, Action::UndoTurn(turn.clone()));
        assert!(!app.agent_turn_undo_available(&turn));
        assert!(!app.agent_pane.turns[0].undo_available);
        assert_eq!(
            app.agent_pane.turns[0].undo_summary.as_deref(),
            Some("1 change reverted · 1 kept because you edited them")
        );
        app.agent_action(&ctx, id, Action::UndoTurn(turn));
        assert_eq!(app.agent_pane.turns[0].undo_summary.as_deref(), Some(agent_pane::UNDO_UNAVAILABLE));

        // The conversation survives layout changes, but inverse edits do not.
        let second = tr_agent::bridge::TurnIds::new().unwrap().allocate().unwrap();
        app.agent_pane.start_prompt(agent_pane::Prompt { text: "RSI".into(), requires_tools: false }).unwrap();
        app.agent_pane.event(BridgeEvent::Started { turn: second.clone() });
        runtime.block_on(async {
            let mut future = executor.execute(
                Arc::new(Registry::new(ToolSet::Edit)),
                ToolCall {
                    turn: second.clone(),
                    pin,
                    name: "add_indicator".into(),
                    arguments: "{\"code\":\"RSI\"}".into(),
                    confirmed: false,
                },
            );
            assert!(futures::poll!(future.as_mut()).is_pending());
            app.process_logic();
            assert!(future.await.success);
        });
        app.agent_pane.event(BridgeEvent::Completed {
            turn: second,
            status: "completed".into(),
            calls: 1,
            interrupt_elapsed: None,
        });
        app.refresh_agent_undo();
        assert!(app.agent_pane.turns[1].undo_available);
        let doc = app.current_doc();
        app.apply_doc(doc);
        app.refresh_agent_undo();
        assert!(app.agent_pane.turns[1].made_edits);
        assert!(!app.agent_pane.turns[1].undo_available);
    }

    #[test]
    fn tools_mode_action_persists_only_choices_and_cannot_bypass_unsafe_or_active_turn() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/agent-wire-mode");
        let mut app = TerminalApp::test_workspace(&root);
        app.execute("NEW AGENT");
        let id = app.first_agent().unwrap();
        app.initialize_agent_choices();
        app.agent_pane.state = Onboarding::Ready;
        let ctx = egui::Context::default();
        app.agent_action(&ctx, id, Action::SetTools(agent_pane::ToolsMode::Edit));
        assert_eq!(app.agent_pane.tool_set(), tr_agent_tools::ToolSet::Edit);
        assert!(app.agent_pane.session.is_none(), "choosing before Start must not launch a child");
        let json = serde_json::to_string(&app.current_doc()).unwrap();
        assert!(json.contains("\"tools\":\"Edit\""));
        assert!(!json.contains("Queue stays paused"));
        app.agent_pane.running = true;
        app.agent_action(&ctx, id, Action::SetTools(agent_pane::ToolsMode::ReadOnly));
        assert_eq!(app.agent_pane.tool_set(), tr_agent_tools::ToolSet::Edit);
        app.agent_pane.running = false;
        app.agent_pane.transition(Onboarding::Stopped { reason: "tripwire".into(), unsafe_backend: true });
        app.agent_action(&ctx, id, Action::SetTools(agent_pane::ToolsMode::ReadOnly));
        app.agent_action(&ctx, id, Action::Check);
        assert!(app.agent_pane.session.is_none());
        assert_eq!(app.agent_pane.tool_set(), tr_agent_tools::ToolSet::Edit);
    }

    #[test]
    fn sending_without_a_pin_pins_the_last_focused_chart() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/agent-ui-autopin");
        let mut app = TerminalApp::test_workspace(&root);
        app.execute("NEW CHART");
        app.execute("NEW CHART");
        app.execute("NEW AGENT");
        let charts: Vec<u64> = app
            .panes
            .iter()
            .filter_map(|p| match &p.kind {
                PaneKind::Chart(c) => Some(c.uid),
                _ => None,
            })
            .collect();
        let last_used = *charts.last().unwrap();
        app.agent_pane.active_chart = Some(last_used);
        app.focus = app.first_agent().unwrap();
        app.pinned_agent_chart = None;
        app.send_agent_prompt(agent_pane::Prompt { text: "What's the trend?".into(), requires_tools: true });
        assert_eq!(app.pinned_agent_chart, Some(last_used));
        assert_eq!(app.agent_pane.settings.pinned_chart, Some(last_used));
        assert_eq!(app.agent_chart_uid(crate::agent_queue::ChartTarget::Pinned), Some(last_used));
    }

    #[test]
    fn agent_command_and_layout_only_save_choices_with_valid_pin() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/agent-ui-tests");
        let mut app = TerminalApp::test_workspace(&root);
        app.execute("NEW CHART");
        app.execute("NEW AGENT");
        assert!(app.panes.iter().any(|p| matches!(p.kind, PaneKind::Agent(_))));
        app.initialize_agent_choices();
        app.agent_pane.input = "not in layouts".into();
        app.agent_pane.notice("not in layouts either");
        app.agent_pane.settings.model = "fake-model".into();
        app.pinned_agent_chart = app.peek_chart().map(|c| c.uid);
        let doc = app.current_doc();
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.contains("fake-model"));
        assert!(!json.contains("not in layouts"));
        let chart = app.panes.iter().find(|p| matches!(p.kind, PaneKind::Chart(_))).unwrap().id;
        app.close_pane(chart);
        let doc = app.current_doc();
        let saved = doc
            .panes
            .iter()
            .find_map(|p| match &p.settings {
                crate::layouts::PaneSettings::Agent(settings) => Some(settings),
                _ => None,
            })
            .unwrap();
        assert_eq!(saved.pinned_chart, None);
        app.execute("AGENT");
        assert!(app.panes.iter().any(|p| matches!(p.kind, PaneKind::Agent(_))));
        app.agent_pane.running = true;
        app.agent_command(Command::Interrupt);
        assert!(!app.agent_pane.running);
        assert!(matches!(app.agent_pane.state, Onboarding::Stopped { .. }));
        assert!(matches!(app.agent_pane.blocks.last(), Some(agent_pane::RenderedBlock::Notice(_))));
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    use egui::{Event as Ev, Key, Modifiers, RawInput, Rect, pos2, vec2};
    use tr_agent::bridge::{BridgeEvent, Recovery};

    fn frame(ctx: &egui::Context, events: Vec<Ev>, mut run: impl FnMut(&mut egui::Ui)) {
        let mut output = ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 700.0))),
                events,
                ..Default::default()
            },
            |ui| run(ui),
        );
        output.textures_delta.clear();
    }
    fn key(key: Key) -> Ev {
        Ev::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }
    }
    fn agent_app(name: &str) -> (TerminalApp, PaneId) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target").join(name);
        let mut app = TerminalApp::test_workspace(&root);
        app.execute("NEW AGENT");
        let id = app.first_agent().unwrap();
        app.focus = id;
        app.agent_pane.acknowledged = true;
        app.agent_pane.state = Onboarding::Ready;
        (app, id)
    }

    #[test]
    fn escape_for_palette_or_command_line_does_not_interrupt_agent() {
        for case in ["command-line", "palette"] {
            let (mut app, _) = agent_app("agent-review-escape");
            app.agent_pane.running = true;
            let ctx = egui::Context::default();
            if case == "palette" {
                app.palette.toggle();
                assert!(app.palette.open);
            } else {
                app.cmd = "AAPL".into();
            }
            frame(&ctx, vec![key(Key::Escape)], |_| {
                if case == "command-line" {
                    ctx.memory_mut(|m| m.request_focus(egui::Id::new("command-line")));
                }
                app.hotkeys(&ctx);
                assert!(ctx.input(|i| i.key_pressed(Key::Escape)), "{case}: Esc was consumed by the agent");
            });
            // No session here, so an interrupt attempt shows up as a Stopped pane.
            assert!(app.agent_pane.running, "{case}: agent turn was interrupted");
        }
    }

    fn paused_queue(app: &mut TerminalApp) {
        for text in ["A", "B"] {
            app.agent_pane.enqueue(agent_pane::Prompt { text: text.into(), requires_tools: false });
        }
        app.agent_pane.event(BridgeEvent::Recovered(Recovery::Resumed));
        assert!(app.agent_pane.next_prompt().is_none(), "recovery must pause the queue");
    }

    #[test]
    fn enter_on_empty_input_keeps_paused_queue_paused() {
        let (mut app, id) = agent_app("agent-review-empty-enter");
        paused_queue(&mut app);
        let ctx = egui::Context::default();
        frame(&ctx, vec![key(Key::Enter)], |ui| {
            ctx.memory_mut(|m| m.request_focus(agent_pane::input_id(pane_id(id))));
            app.agent_panel(ui, id);
        });
        assert!(app.agent_pane.next_prompt().is_none(), "empty Enter resumed the paused queue");
    }

    #[test]
    fn new_prompt_does_not_implicitly_resume_stale_paused_prompts() {
        let (mut app, id) = agent_app("agent-review-send-resume");
        paused_queue(&mut app);
        app.agent_pane.input = "C".into();
        let ctx = egui::Context::default();
        frame(&ctx, vec![key(Key::Enter)], |ui| {
            ctx.memory_mut(|m| m.request_focus(agent_pane::input_id(pane_id(id))));
            app.agent_panel(ui, id);
        });
        assert_ne!(app.agent_pane.next_prompt().map(|p| p.text), Some("A".into()));
    }
}
