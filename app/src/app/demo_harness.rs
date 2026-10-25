//! Development-only assembly of a two-pane workspace. No session/connector is constructed.
use super::{PaneKind, TerminalApp};
use crate::agent_demo::{self, Config, Demo, Outcome, TURN};
use crate::chart_view::ChartState;
use crate::dock::{Dir, Node};
use crate::worker::Worker;
use egui::{Context, Rect};
use serde_json::json;
use tr_agent_tools::args::LiveArgs;
use tr_agent_tools::edits::{Chart, Session};
use tr_core::Timeframe;

impl TerminalApp {
    pub(crate) fn new_agent_demo(ctx: &Context, worker: Worker, config: Config, outcome: Outcome) -> Self {
        Self::agent_demo_workspace(ctx, worker, &tr_alerts::data_dir(), config, outcome)
    }

    fn agent_demo_workspace(
        ctx: &Context,
        worker: Worker,
        root: &std::path::Path,
        config: Config,
        outcome: Outcome,
    ) -> Self {
        let mut app = Self::isolated_workspace(root, worker);
        // Set the persistence/session guard before creating any panes or inverse edits.
        let state = config.state;
        app.agent_demo = Some(Demo::new(config, outcome));
        app.agent_queue = crate::agent_queue::AgentToolQueue::new(ctx.clone());
        app.themes = crate::themes::builtins();
        if let Ok(name) = std::env::var("TR_THEME") {
            app.prefs.appearance.theme = match name.to_ascii_lowercase().as_str() {
                "dark" => app.prefs.appearance.dark_theme.clone(),
                "light" => app.prefs.appearance.light_theme.clone(),
                _ => crate::themes::find(&app.themes, &name)
                    .map_or(app.prefs.appearance.theme.clone(), |t| t.name.clone()),
            };
        }
        app.system_theme = ctx.system_theme();
        app.system_accent =
            tr_macos::accent_color().map_or(crate::theme::DEFAULT_ACCENT, |[r, g, b]| egui::Color32::from_rgb(r, g, b));
        app.sidebar_open = false;
        app.focus_cmd = false;
        app.layout_name = "Agent pane demo".into();
        app.status = "Sample data · offline agent pane demo · nothing is sent or saved".into();
        let chart = app.new_pane(PaneKind::Chart(Box::new(ChartState::new(0, agent_demo::SYMBOL, Timeframe::W1))));
        let uid = app.peek_chart().expect("demo chart").uid;
        app.pinned_agent_chart = Some(uid);
        app.agent_pane = agent_demo::pane(state, uid);
        if state.workflows() {
            app.create_watchlist("Agent Test".into(), Vec::new()).expect("offline fixture watchlist");
            let before = serde_json::json!(app.watchlists.last().unwrap());
            app.agent_pane.workflows.proposals[1].before = before;
        }
        let agent = app.new_pane(PaneKind::Agent(app.agent_pane.settings.clone()));
        app.dock = Node::split_with(Dir::Row, vec![Node::leaf(chart), Node::leaf(agent)], vec![0.58, 0.42]);
        app.focus = agent;
        app
    }

    pub(super) fn size_agent_demo_dock(&mut self, area: Rect, gap: f32) {
        if let Some(width) = self.agent_demo.as_ref().and_then(|demo| demo.config.agent_width)
            && let Node::Split { shares, .. } = &mut self.dock
        {
            let right = (width / (area.width() - gap).max(1.0)).clamp(0.1, 0.9);
            *shares = vec![1.0 - right, right];
        }
    }

    pub(super) fn populate_agent_demo(&mut self, ctx: &Context) {
        let Some(demo) = &self.agent_demo else { return };
        if demo.populated || self.peek_chart().is_none_or(|c| c.displayed_bars().bars.is_empty()) {
            return;
        }
        let state = demo.config.state;
        self.agent_demo.as_mut().unwrap().populated = true;
        if state.approved() {
            // Construct the already-approved offline fixture; interactive demo controls stay inert.
            self.confirm_workflows(TURN, true);
        }
        if state.edits()
            && let Err(reason) = self.populate_demo_edits(state.undone())
        {
            self.agent_demo.as_ref().unwrap().finish(ctx, Err(reason));
        }
    }

    fn populate_demo_edits(&mut self, undone: bool) -> Result<(), String> {
        let chart = self.peek_chart().ok_or("Demo chart missing")?;
        let bars = chart.source_bars().bars;
        if bars.len() < 60 {
            return Err("Demo requires at least 60 sample bars".into());
        }
        let a = bars[bars.len() - 40];
        let b = bars[bars.len() - 10];
        let zones = tr_agent_tools::analytics::support_resistance(bars, 10);
        let support = zones
            .iter()
            .filter(|z| z.low <= bars.last().unwrap().close)
            .min_by(|a, b| {
                (a.high - bars.last().unwrap().close).abs().total_cmp(&(b.high - bars.last().unwrap().close).abs())
            })
            .ok_or("Demo support missing")?;
        let resistance = zones
            .iter()
            .filter(|z| z.high >= bars.last().unwrap().close)
            .min_by(|a, b| {
                (a.low - bars.last().unwrap().close).abs().total_cmp(&(b.low - bars.last().unwrap().close).abs())
            })
            .ok_or("Demo resistance missing")?;
        let edits: [(&str, LiveArgs, String); 3] = [
            ("create_drawing", LiveArgs::CreateDrawing(serde_json::from_value(json!({
                "symbol":agent_demo::SYMBOL, "kind":"rectangle", "zone_role":"support", "source_timeframe":"1W", "anchors":[{"time":a.ts,"price":support.low},{"time":b.ts,"price":support.high}]
            })).map_err(|e| e.to_string())?), "Support zone".into()),
            ("create_drawing", LiveArgs::CreateDrawing(serde_json::from_value(json!({
                "symbol":agent_demo::SYMBOL, "kind":"rectangle", "zone_role":"resistance", "source_timeframe":"1W", "anchors":[{"time":a.ts,"price":resistance.low},{"time":b.ts,"price":resistance.high}]
            })).map_err(|e| e.to_string())?), "Resistance zone".into()),
            ("add_indicator", LiveArgs::AddIndicator(serde_json::from_value(json!({"code":"RSI","params":[14]})).map_err(|e| e.to_string())?), "RSI 14".into()),
        ];
        let mut session = Session::default();
        self.agent_pane.wire("item/completed", &json!({"item":{
            "type":"dynamicToolCall", "id":"edit-retry", "tool":"create_drawing", "success":false,
            "arguments":{"kind":"rectangle","zone_role":"support"},
            "error":{"message":"anchors[1].price must differ from anchors[0].price; use analyze low and high for a non-zero zone"}
        }}));
        for (index, (name, args, label)) in edits.into_iter().enumerate() {
            let uid = self.pinned_agent_chart.ok_or("Demo pin missing")?;
            let chart = self.agent_chart_mut(crate::agent_queue::ChartTarget::Uid(uid)).ok_or("Demo chart missing")?;
            let comparisons = chart.comparison_objects();
            let view = Chart {
                uid,
                symbol: &chart.symbol,
                timeframe: chart.tf,
                price_decimals: chart.agent_price_decimals,
                bars: chart.source_bars().bars,
                drawings: &chart.drawings,
                indicators: &chart.indicators,
                comparisons: &comparisons,
            };
            session.observe(&view);
            let plan = session.plan(&view, TURN, args).map_err(|_| "Demo edit failed existing edit validation")?;
            // The same mutation/provenance/inverse implementation used by live edit jobs.
            let chart = self
                .panes
                .iter_mut()
                .find_map(|p| match &mut p.kind {
                    PaneKind::Chart(c) if c.uid == uid => Some(c),
                    _ => None,
                })
                .unwrap();
            chart.begin_agent_edit(TURN);
            let result = chart.apply_agent_plan(plan, TURN, &self.worker);
            chart.finish_drawing_edit();
            let id = result["id"].as_str().map(str::to_owned).unwrap_or_else(|| result["id"].to_string());
            agent_demo::tool(
                &mut self.agent_pane,
                &format!("edit-{index}"),
                name,
                Some(true),
                Some(&format!("#{id} · {label}")),
            );
        }
        self.agent_turns.push(TURN.into());
        self.agent_pane.wire("item/completed", &json!({"item":{
            "type":"agentMessage", "id":"edits-reply", "text":"Drew labelled weekly support and resistance zones and added **RSI 14** to the sample chart.\n\nThe first rectangle attempt failed; its corrected retry succeeded. Labels show W1 and the zone bounds. Undo this turn removes all three changes; your own chart objects are unchanged. Synthetic sample data only."
        }}));
        agent_demo::complete(&mut self.agent_pane, "completed");
        if undone {
            let summary = self.undo_agent_turn(TURN);
            self.agent_pane.undo_result(TURN, summary);
        }
        let available = self.undoable_agent_turns().into_iter().map(str::to_owned).collect::<Vec<_>>();
        self.agent_pane.refresh_undo(&available, available.last().map(String::as_str));
        Ok(())
    }

    pub(super) fn capture_agent_demo(&mut self, ctx: &Context, screen: Rect) {
        if self.agent_demo.is_none() {
            return;
        }
        let ready = self.peek_chart().is_some_and(|chart| {
            !chart.source_bars().bars.is_empty()
                && !chart.displayed_bars().bars.is_empty()
                && chart.studies().iter().all(|study| {
                    study.error.is_none()
                        && !study.outputs.is_empty()
                        && study.outputs.iter().any(|output| output.values.iter().any(Option::is_some))
                })
        });
        let demo = self.agent_demo.as_mut().unwrap();
        demo.frame(ctx, screen, ready && demo.populated);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_pane::RenderedBlock;
    use crate::worker::{Request, Response};
    use tr_data::MarketData;

    fn fixture(root: &std::path::Path, name: &str) -> TerminalApp {
        let ctx = Context::default();
        let config = Config {
            state: agent_demo::State::parse(name).unwrap(),
            shot: None,
            size: [1100, 800],
            pane_only: false,
            agent_width: Some(360.0),
        };
        let (worker, mut requests) = Worker::recording();
        let mut app = TerminalApp::agent_demo_workspace(&ctx, worker, root, config, Default::default());
        let (pane, generation, symbol, tf) = std::iter::from_fn(|| requests.try_recv().ok())
            .find_map(|req| {
                if let Request::History { pane, generation, symbol, tf, .. } = req {
                    Some((pane, generation, symbol, tf))
                } else {
                    None
                }
            })
            .expect("sample history request");
        let bars = Arc::new(
            futures::executor::block_on(
                tr_data::sample::SampleProvider::at(agent_demo::SAMPLE_NOW, 300).history(&symbol, tf, None, 300),
            )
            .unwrap(),
        );
        let (out, rx) = std::sync::mpsc::channel();
        app.worker.rx = rx;
        out.send(Response::History {
            pane,
            generation,
            revision: 1,
            symbol,
            tf,
            gpu: tr_chart::scene::SeriesGpu::build(&bars),
            bars,
            prepended: 0,
            from_cache: false,
            exhausted: true,
            live: false,
            crypto_source: None,
        })
        .unwrap();
        app.process_logic();
        app.populate_agent_demo(&ctx);
        app.agent_logic(&ctx);
        app
    }
    use std::sync::Arc;
    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/agent-demo-workspace-tests")
    }

    #[test]
    fn every_demo_state_has_only_two_docked_panes_sample_bars_and_no_session() {
        for name in agent_demo::STATES {
            let scratch = agent_demo::Scratch::create(&root()).unwrap();
            let mut app = fixture(&scratch.0, name);
            assert_eq!(app.provider, "sample");
            assert_eq!(app.panes.len(), 2, "{name}");
            assert_eq!(app.dock.visible().len(), 2);
            assert!(matches!(app.panes[0].kind, PaneKind::Chart(_)));
            assert!(matches!(app.panes[1].kind, PaneKind::Agent(_)));
            assert_eq!(app.peek_chart().unwrap().displayed_bars().bars.len(), 300);
            assert!(app.agent_demo.as_ref().unwrap().populated);
            assert!(app.agent_pane.session.is_none());
            let rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1088.0, 700.0));
            app.size_agent_demo_dock(rect, 6.0);
            let groups = app.dock.layout(rect, 6.0).0;
            assert!((groups[1].rect.width() - 360.0).abs() < 0.01);
            let ctx = Context::default();
            let id = app.panes[1].id;
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.agent_panel(ui, id));
            output.textures_delta.clear();
            assert!(app.agent_pane.session.is_none());
            assert!(!app.agent_pane.running || matches!(*name, "streaming"));
        }
    }

    #[test]
    fn workflow_fixtures_show_two_pending_rows_or_applied_undoable_changes() {
        use crate::agent_bridge::ProposalStatus;
        for name in ["proposal", "approved"] {
            let scratch = agent_demo::Scratch::create(&root()).unwrap();
            let mut app = fixture(&scratch.0, name);
            assert_eq!(app.agent_pane.workflows.proposals.len(), 2);
            let approved = name == "approved";
            assert!(
                app.agent_pane
                    .workflows
                    .proposals
                    .iter()
                    .all(|p| p.status == if approved { ProposalStatus::Approved } else { ProposalStatus::Pending })
            );
            assert_eq!(app.watchlists.len(), if approved { 3 } else { 2 });
            let symbols = &app.agent_pane.workflows.proposals[1].symbols;
            assert_eq!(symbols.len(), 3);
            assert!(!symbols[1].selected);
            if approved {
                assert_eq!(symbols[0].outcome.as_deref(), Some("Added"));
                assert_eq!(symbols[1].outcome.as_deref(), Some("Declined"));
                assert!(
                    app.agent_pane.workflows.turn_summary(TURN).unwrap().contains("added 2 of 3 symbols to Agent Test")
                );
                assert!(app.agent_pane.workflows.decision_line(TURN).unwrap().starts_with("You approved 3 of 4 · "));
                assert!(app.agent_pane.turns[0].undo_available);
                assert_eq!(app.agent_pane.turns[0].chart_changes, Some(2));
                assert_eq!(app.undo_agent_turn(TURN).unwrap().reverted, 2);
                assert_eq!(app.watchlists.len(), 2);
                assert!(app.watchlists[1].symbols.is_empty());
            } else {
                assert_eq!(
                    app.agent_pane.workflows.turn_summary(TURN).as_deref(),
                    Some("Awaiting approval · 4 changes")
                );
                assert!(app.agent_pane.workflows.decision_line(TURN).is_none());
            }
        }
    }

    #[test]
    fn edit_fixtures_use_real_agent_objects_ids_and_inverse_undo() {
        for name in ["edits", "undone"] {
            let scratch = agent_demo::Scratch::create(&root()).unwrap();
            let app = fixture(&scratch.0, name);
            let traces = app
                .agent_pane
                .blocks
                .iter()
                .filter_map(|block| if let RenderedBlock::Tool(t) = block { Some(t) } else { None })
                .collect::<Vec<_>>();
            assert_eq!(
                traces.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
                ["create_drawing", "create_drawing", "create_drawing", "add_indicator"]
            );
            assert!(
                traces[1..]
                    .iter()
                    .all(|t| t.success == Some(true) && t.summary.as_ref().is_some_and(|s| s.starts_with('#')))
            );
            assert_eq!(traces[0].success, Some(false));
            assert!(traces[0].error.as_deref().unwrap().contains("anchors[1].price"));
            let chart = app.peek_chart().unwrap();
            let turn = &app.agent_pane.turns[0];
            assert!(turn.finished && turn.made_edits);
            if name == "edits" {
                assert_eq!(chart.drawings.len(), 2);
                for (index, drawing) in chart.drawings.iter().enumerate() {
                    assert_eq!(drawing.created_by, tr_chart::drawings::Author::Agent);
                    assert_eq!(drawing.turn_id.as_deref(), Some(TURN));
                    assert!(traces[index + 1].summary.as_ref().unwrap().starts_with(&format!("#{} ·", drawing.id)));
                    assert_eq!(drawing.tool, tr_chart::drawings::Tool::Rectangle);
                    assert!(drawing.zone_role.is_some());
                    assert!(drawing.text.starts_with("W1 ") && drawing.text.contains('–'));
                }
                let rsi = chart.indicators.iter().find(|s| s.code == "RSI").unwrap().chart.as_ref().unwrap();
                assert!(rsi.agent_highlight());
                assert!(traces[3].summary.as_ref().unwrap().starts_with(&format!("#{}:{} ·", chart.uid, rsi.id)));
                assert!(turn.undo_available);
                assert_eq!(turn.chart_changes, Some(3));
                assert!(app.agent_turn_undo_available(TURN));
            } else {
                assert!(chart.drawings.is_empty());
                assert!(!chart.indicators.iter().any(|s| s.code == "RSI"));
                assert!(!turn.undo_available);
                assert_eq!(turn.undo_summary.as_deref(), Some("3 changes reverted"));
                assert_eq!(turn.reverted_changes, Some(3));
            }
        }
    }

    #[test]
    fn demo_never_writes_layouts_settings_or_preferences_even_on_edit_and_exit() {
        let scratch = agent_demo::Scratch::create(&root()).unwrap();
        let sentinel = b"existing settings must not be loaded or replaced";
        std::fs::write(scratch.0.join("settings.json"), sentinel).unwrap();
        std::fs::write(scratch.0.join("prefs.json"), sentinel).unwrap();
        let mut app = fixture(&scratch.0, "edits");
        app.saves.writer = Arc::new(|_, _| panic!("demo invoked persistent writer"));
        app.autosave();
        app.save_settings();
        app.save_prefs();
        app.save_layout();
        app.periodic();
        app.exit_save();
        eframe::App::on_exit(&mut app);
        assert!(app.saves.job.is_none());
        assert_eq!(std::fs::read(scratch.0.join("settings.json")).unwrap(), sentinel);
        assert_eq!(std::fs::read(scratch.0.join("prefs.json")).unwrap(), sentinel);
        assert!(!scratch.0.join("layouts").exists());
        assert!(!scratch.0.join("themes").exists());
        assert!(!scratch.0.join("palette.json").exists());
    }
}
