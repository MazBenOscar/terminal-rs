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

fn workflow_rows(key: &str, rows: Vec<serde_json::Value>, total: usize) -> tr_agent_tools::ToolResult {
    let mut value = serde_json::json!({"total":total,"truncated":false});
    value[key] = serde_json::json!([]);
    for row in rows.into_iter().take(50) {
        value[key].as_array_mut().unwrap().push(row);
        if value.to_string().len() > tr_agent_tools::MAX_TEXT_BYTES - 128 {
            value[key].as_array_mut().unwrap().pop();
            break;
        }
    }
    value["truncated"] = (value[key].as_array().unwrap().len() < total).into();
    tr_agent_tools::ToolResult::json(value)
}

/// Runtime firing is not a configuration conflict. User edits still change the
/// configuration revision, even when a value is subsequently changed back.
fn alert_configuration(alert: &tr_core::price_alerts::Alert) -> serde_json::Value {
    let mut value = serde_json::json!(alert);
    if let Some(fields) = value.as_object_mut() {
        for field in ["status", "last_fired", "fired_through"] {
            fields.remove(field);
        }
    }
    value
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
        ctx.memory(|m| m.focused()).is_some_and(|id| self.agent_pane.proposal_focus.contains_key(&id))
            || agent_pane::captures_focus(
                ctx.memory(|m| m.focused()),
                self.panes.iter().filter(|p| matches!(p.kind, PaneKind::Agent(_))).map(|p| pane_id(p.id)),
            )
    }

    pub(super) fn agent_escape(&mut self, ctx: &egui::Context) -> bool {
        let focused = ctx.memory(|m| m.focused());
        if !self.palette.open
            && let Some(turn) = focused.and_then(|id| self.agent_pane.proposal_focus.get(&id)).cloned()
        {
            let decline = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            let enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            if decline || enter {
                let action = if decline {
                    Some(false)
                } else {
                    focused.and_then(|id| self.agent_pane.proposal_buttons.get(&id).copied())
                };
                if self.agent_demo.is_none()
                    && let Some(approve) = action
                {
                    let ids = self.agent_pane.proposal_shown.get(&turn).cloned().unwrap_or_default();
                    self.confirm_shown_workflows(&turn, approve, Some(&ids));
                }
                return true;
            }
        }
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

    fn workflow_list_index(&self, name: &str) -> Option<usize> {
        self.watchlists
            .iter()
            .position(|list| super::watchlist_name_key(&list.name) == super::watchlist_name_key(name.trim()))
    }

    fn workflow_snapshot(&self, args: &tr_agent_tools::args::LiveArgs) -> serde_json::Value {
        use serde_json::json;
        use tr_agent_tools::args::LiveArgs::*;
        match args {
            CreateWatchlist(a) => {
                self.workflow_list_index(&a.name).map(|i| json!(self.watchlists[i])).unwrap_or_default()
            }
            AddWatchlistSymbols(a) => {
                self.workflow_list_index(&a.watchlist).map(|i| json!(self.watchlists[i])).unwrap_or_default()
            }
            RemoveWatchlistSymbols(a) => {
                self.workflow_list_index(&a.watchlist).map(|i| json!(self.watchlists[i])).unwrap_or_default()
            }
            DeleteWatchlist(a) => {
                self.workflow_list_index(&a.watchlist).map(|i| json!(self.watchlists[i])).unwrap_or_default()
            }
            UpdateAlert(a) => self
                .price_alerts
                .book
                .alerts
                .iter()
                .find(|alert| alert.id == a.id)
                .map(alert_configuration)
                .unwrap_or_default(),
            DeleteAlert(a) => self
                .price_alerts
                .book
                .alerts
                .iter()
                .find(|alert| alert.id == a.id)
                .map(alert_configuration)
                .unwrap_or_default(),
            _ => serde_json::Value::Null,
        }
    }

    /// Called on the app thread. Workflow calls never apply, including calls with confirmed=true.
    pub(crate) fn agent_workflow(
        &mut self,
        turn: &str,
        mut args: tr_agent_tools::args::LiveArgs,
    ) -> tr_agent_tools::ToolResult {
        use crate::agent_bridge::{Proposal, ProposalStatus};
        use serde_json::json;
        use tr_agent_tools::{
            ToolResult,
            args::{AlertKind, LiveArgs::*},
        };
        // Resolve removals against the actual list before constructing a proposal.
        // Search is only a best-effort normalization step, never a membership gate.
        let mut symbol_errors = Vec::new();
        if let RemoveWatchlistSymbols(a) = &mut args {
            let Some(index) = self.workflow_list_index(&a.watchlist) else {
                return ToolResult::error("not_found", "Watchlist no longer exists", None);
            };
            a.symbols.retain_mut(|symbol| {
                if let Some(stored) =
                    self.watchlists[index].symbols.iter().find(|stored| crate::symbol_search::same_key(stored, symbol))
                {
                    *symbol = stored.clone();
                    true
                } else {
                    symbol_errors
                        .push(json!({"symbol":tr_agent_tools::scrub_text(symbol).chars().take(64).collect::<String>(),
                        "error":"not_in_list","message":"not in list"}));
                    false
                }
            });
            if a.symbols.is_empty() {
                let mut result = ToolResult::json(
                    json!({"error":{"code":"unknown_symbols","message":"Symbols not in list"},"symbols":symbol_errors}),
                );
                result.success = false;
                return result.capped();
            }
        }
        let before = self.workflow_snapshot(&args);
        let label = |s: &str| crate::security_names::label(s);
        let matched_name = before["name"].as_str().unwrap_or_default();
        let summary = match &args {
            ListWatchlists(_) => return workflow_rows("watchlists", self.watchlists.iter().take(50).map(|list|
                json!({"name":list.name,"symbol_count":list.symbols.len(),"symbols":list.symbols.iter().take(50).collect::<Vec<_>>(),"symbols_truncated":list.symbols.len()>50,"agent_turn":list.agent_turn,"agent_entries":list.agent_entries.iter().take(50).collect::<std::collections::BTreeMap<_,_>>()})).collect(), self.watchlists.len()),
            ListAlerts(a) => {
                let alerts: Vec<_> = self.price_alerts.book.alerts.iter().filter(|alert| a.symbol.as_ref().is_none_or(|symbol| crate::symbol_search::same_key(symbol, &alert.symbol))).collect();
                return workflow_rows("alerts", alerts.iter().take(50).map(|a| json!(a)).collect(), alerts.len());
            },
            CreateWatchlist(a) => {
                if let Err(error) = self.check_watchlist_name(&a.name, None) { return ToolResult::error("invalid_watchlist", &error, None); }
                format!("Create watchlist · {}", a.name.trim())
            }
            AddWatchlistSymbols(a) => format!("Add {} to {}: {}", symbol_count(a.symbols.len()), matched_name, a.symbols.iter().map(|s| label(s)).collect::<Vec<_>>().join(", ")),
            RemoveWatchlistSymbols(a) => format!("Remove {} from {}: {}", symbol_count(a.symbols.len()), matched_name, a.symbols.iter().map(|s| label(s)).collect::<Vec<_>>().join(", ")),
            DeleteWatchlist(_) => {
                if self.watchlists.len() == 1 { return ToolResult::error("last_watchlist", "Cannot delete the last watchlist", None); }
                format!("Delete watchlist · {}", matched_name)
            }
            CreateAlert(a) => {
                if self.price_alerts.book.alerts.iter().filter(|a| a.agent_created || a.agent_turn.is_some()).count() >= 50 {
                    return ToolResult::error("alert_limit", "At most 50 agent-created alerts", None);
                }
                format!("Create alert · {} · {} {} {} {} · {}", label(&a.symbol),
                    a.timeframe.as_deref().filter(|_| matches!(a.kind, AlertKind::Close)).unwrap_or("Live"),
                    if matches!(a.kind, AlertKind::Close) { "finished close" } else { "price crosses" },
                    match a.direction { tr_agent_tools::args::AlertRule::Above => "above", _ => "below" }, crate::price_alerts::display_price(&a.symbol, a.level),
                    if matches!(a.repeat, Some(tr_agent_tools::args::AlertRepeat::Rearm)) { "rearm · 60s cooldown" } else { "once" })
            }
            UpdateAlert(a) => {
                let Some(alert) = self.price_alerts.book.alerts.iter().find(|alert| alert.id == a.id) else { return ToolResult::error("not_found", "Alert no longer exists", None); };
                let mut changes = Vec::new();
                if let Some(level) = a.level { changes.push(format!("level {}", crate::price_alerts::display_price(&alert.symbol, level))); }
                if let Some(direction) = a.direction { changes.push(match direction { tr_agent_tools::args::AlertRule::Above => "above".into(), _ => "below".into() }); }
                if let Some(repeat) = a.repeat { changes.push(match repeat { tr_agent_tools::args::AlertRepeat::Once => "once".into(), _ => "rearm · 60s cooldown".into() }); }
                format!("Update alert · {} · #{} · {}", label(&alert.symbol), a.id, changes.join(" · "))
            }
            DeleteAlert(a) => {
                let Some(alert) = self.price_alerts.book.alerts.iter().find(|alert| alert.id == a.id) else { return ToolResult::error("not_found", "Alert no longer exists", None); };
                format!("Delete alert · {} · #{}", label(&alert.symbol), a.id)
            },
            _ => return ToolResult::error("tool_disabled", "Not a workflow tool", None),
        };
        if matches!(
            &args,
            AddWatchlistSymbols(_) | RemoveWatchlistSymbols(_) | DeleteWatchlist(_) | UpdateAlert(_) | DeleteAlert(_)
        ) && before.is_null()
        {
            return ToolResult::error("not_found", "Watchlist or alert no longer exists", None);
        }
        self.agent_pane.workflows.expire(std::time::Instant::now(), None);
        if self.agent_pane.workflows.proposals.iter().filter(|p| p.turn == turn).count() >= 10 {
            return ToolResult::error("proposal_limit", "At most 10 proposals per turn", None);
        }
        let user_owned = matches!(&args, UpdateAlert(_) | DeleteAlert(_))
            && before["agent_turn"]
                .as_str()
                .is_none_or(|owner| owner.split_once(':').map(|p| p.0) != turn.split_once(':').map(|p| p.0));
        self.agent_pane.workflows.next_id += 1;
        let id = self.agent_pane.workflows.next_id;
        self.agent_pane.workflows.proposals.push(Proposal {
            id,
            turn: turn.into(),
            symbols: Proposal::symbol_choices(&args),
            decided_at: None,
            args,
            summary: summary.clone(),
            user_owned,
            before,
            selected: true,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(600),
            status: ProposalStatus::Pending,
        });
        let mut value = json!({"status":"awaiting_user_confirmation","proposal_id":id,"summary":summary});
        if !symbol_errors.is_empty() {
            value["symbols"] = json!(symbol_errors);
        }
        ToolResult::json(value).capped()
    }

    fn apply_workflow(
        &mut self,
        proposal: &crate::agent_bridge::Proposal,
    ) -> Result<crate::agent_bridge::WorkflowUndo, String> {
        use crate::agent_bridge::WorkflowUndo;
        use tr_agent_tools::args::{AlertKind, AlertRepeat, AlertRule, LiveArgs::*};
        use tr_core::price_alerts::{Direction, Repeat, Rule};
        let direction = |d| match d {
            AlertRule::Above => Direction::Above,
            AlertRule::Below => Direction::Below,
        };
        let repeat = |r| match r {
            AlertRepeat::Once => Repeat::Once,
            AlertRepeat::Rearm => Repeat::Rearm { cooldown_secs: 60 },
        };
        // Recheck at click time: a stale approval must not approve a different object/configuration.
        if self.workflow_snapshot(&proposal.args) != proposal.before {
            return Err("Target changed since this proposal; ask the agent to propose it again".into());
        }
        let turn = &proposal.turn;
        match &proposal.args {
            CreateAlert(a) => {
                if self.price_alerts.load_error.is_some() {
                    return Err("Alerts could not be loaded".into());
                }
                if self.price_alerts.book.alerts.iter().filter(|a| a.agent_created || a.agent_turn.is_some()).count()
                    >= 50
                {
                    return Err("At most 50 agent-created alerts".into());
                }
                let rule = if matches!(a.kind, AlertKind::Close) {
                    Some(Rule::Formula {
                        expression: "C".into(),
                        timeframe: a
                            .timeframe
                            .as_deref()
                            .ok_or("Close requires timeframe")?
                            .parse()
                            .map_err(|_| "Invalid timeframe")?,
                    })
                } else {
                    None
                };
                if let Some(rule) = &rule {
                    rule.validate()?;
                }
                let id = self.price_alerts.book.add(
                    a.symbol.clone(),
                    a.level,
                    direction(a.direction),
                    self.price_alerts.simulated,
                )?;
                self.price_alerts.book.set_rule(id, rule)?;
                self.price_alerts.book.set_repeat(id, repeat(a.repeat.unwrap_or(AlertRepeat::Once)))?;
                let alert = self.price_alerts.book.alerts.iter_mut().find(|a| a.id == id).unwrap();
                alert.completed_only = matches!(a.kind, AlertKind::Close);
                alert.agent_turn = Some(turn.clone());
                alert.agent_created = true;
                alert.note = a.note.clone();
                let after = alert.clone();
                self.price_alerts.changed(true);
                Ok(WorkflowUndo::Alert {
                    before: None,
                    after: Some(after),
                    position: self.price_alerts.book.alerts.len() - 1,
                })
            }
            UpdateAlert(a) => {
                if self.price_alerts.load_error.is_some() {
                    return Err("Alerts could not be loaded".into());
                }
                let before = self
                    .price_alerts
                    .book
                    .alerts
                    .iter()
                    .find(|alert| alert.id == a.id)
                    .cloned()
                    .ok_or("Alert no longer exists")?;
                // User-owned targets reach this path ONLY via an explicit confirmation-card click.
                self.price_alerts.book.edit(
                    a.id,
                    a.level.unwrap_or(before.level),
                    a.direction.map(direction).unwrap_or(before.direction),
                )?;
                if let Some(r) = a.repeat {
                    self.price_alerts.book.set_repeat(a.id, repeat(r))?;
                }
                let after = self.price_alerts.book.alerts.iter_mut().find(|alert| alert.id == a.id).unwrap();
                after.agent_turn = before.agent_turn.clone();
                let after = after.clone();
                self.price_alerts.changed(true);
                let position = self.price_alerts.book.alerts.iter().position(|alert| alert.id == a.id).unwrap();
                Ok(WorkflowUndo::Alert { before: Some(before), after: Some(after), position })
            }
            DeleteAlert(a) => {
                if self.price_alerts.load_error.is_some() {
                    return Err("Alerts could not be loaded".into());
                }
                let before = self
                    .price_alerts
                    .book
                    .alerts
                    .iter()
                    .find(|alert| alert.id == a.id)
                    .cloned()
                    .ok_or("Alert no longer exists")?;
                let position = self.price_alerts.book.alerts.iter().position(|alert| alert.id == a.id).unwrap();
                self.price_alerts.book.delete(a.id);
                self.price_alerts.changed(true);
                Ok(WorkflowUndo::Alert { before: Some(before), after: None, position })
            }
            CreateWatchlist(a) => {
                self.create_watchlist(a.name.clone(), Vec::new())?;
                let list = self.watchlists.last_mut().unwrap();
                list.agent_turn = Some(turn.clone());
                list.workflow_id = Some(format!("{turn}:{}", proposal.id));
                Ok(WorkflowUndo::Watchlist { before: None, after: Some(list.clone()) })
            }
            AddWatchlistSymbols(a) => {
                let index = self.workflow_list_index(&a.watchlist).ok_or("Watchlist no longer exists")?;
                let before = self.watchlists[index].clone();
                let (next, _, truncated) = super::watchlist_append(&before.symbols, a.symbols.clone());
                if truncated {
                    return Err("Watchlist is full (200 symbols)".into());
                }
                for symbol in &a.symbols {
                    self.add_to_named_watchlist(&before.name, symbol.clone(), None)?;
                }
                let list = &mut self.watchlists[index];
                list.agent_turn = before.agent_turn.clone();
                list.agent_entries = before.agent_entries.clone();
                if list.workflow_id.is_none() {
                    list.workflow_id = Some(format!("{turn}:{}", proposal.id));
                }
                for symbol in
                    next.iter().filter(|s| !before.symbols.iter().any(|old| crate::symbol_search::same_key(s, old)))
                {
                    list.agent_entries.insert(symbol.clone(), turn.clone());
                }
                Ok(WorkflowUndo::Watchlist { before: Some(before), after: Some(list.clone()) })
            }
            RemoveWatchlistSymbols(a) => {
                let index = self.workflow_list_index(&a.watchlist).ok_or("Watchlist no longer exists")?;
                let before = self.watchlists[index].clone();
                let items: Vec<_> = before
                    .items()
                    .into_iter()
                    .filter(|item| !matches!(item, crate::watch_sections::Item::Symbol(s) if a.symbols.iter().any(|candidate| crate::symbol_search::same_key(s, candidate))))
                    .collect();
                if index == self.active_watchlist {
                    self.set_watchlist_items(&items);
                } else {
                    self.watchlists[index].set_items(&items);
                }
                if self.watchlists[index].workflow_id.is_none() {
                    self.watchlists[index].workflow_id = Some(format!("{turn}:{}", proposal.id));
                }
                Ok(WorkflowUndo::Watchlist { before: Some(before), after: Some(self.watchlists[index].clone()) })
            }
            DeleteWatchlist(a) => {
                let index = self.workflow_list_index(&a.watchlist).ok_or("Watchlist no longer exists")?;
                let before = self.watchlists[index].clone();
                self.delete_watchlist(index)?;
                Ok(WorkflowUndo::Watchlist { before: Some(before), after: None })
            }
            _ => Err("Not a mutating workflow".into()),
        }
    }

    pub(crate) fn confirm_workflows(&mut self, turn: &str, approve: bool) {
        self.confirm_shown_workflows(turn, approve, None);
    }

    /// `shown` limits the decision to proposals the user saw on the card (UI paths
    /// always pass it); `None` decides every pending proposal of the turn.
    pub(crate) fn confirm_shown_workflows(&mut self, turn: &str, approve: bool, shown: Option<&[u64]>) {
        use crate::agent_bridge::ProposalStatus;
        use tr_agent::codex::{WorkflowResult, WorkflowStatus};
        self.agent_pane.workflows.expire(std::time::Instant::now(), None);
        let context_start = self.agent_pane.workflows.context.len();
        let proposals: Vec<_> = self
            .agent_pane
            .workflows
            .proposals
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.turn == turn
                    && p.status == ProposalStatus::Pending
                    && (!approve || p.is_selected())
                    && shown.is_none_or(|ids| ids.contains(&p.id))
                    && (shown.is_none()
                        || p.symbols.iter().enumerate().all(|(n, s)| {
                            self.agent_pane
                                .symbol_shown
                                .get(turn)
                                .is_some_and(|rows| rows.contains(&(p.id, n, s.symbol.clone())))
                        }))
            })
            .map(|(i, _)| i)
            .collect();
        for index in proposals {
            let proposal = self.agent_pane.workflows.proposals[index].clone();
            let mut selected = proposal.clone();
            if !proposal.symbols.is_empty() {
                let symbols = proposal.symbols.iter().filter(|s| s.selected).map(|s| s.symbol.clone()).collect();
                match &mut selected.args {
                    tr_agent_tools::args::LiveArgs::AddWatchlistSymbols(a) => a.symbols = symbols,
                    tr_agent_tools::args::LiveArgs::RemoveWatchlistSymbols(a) => a.symbols = symbols,
                    _ => unreachable!("only symbol proposals have sub-rows"),
                }
            }
            let (status, result) = if approve && proposal.is_selected() {
                match self.apply_workflow(&selected) {
                    Ok(undo) => {
                        // Earlier approvals in this card are not user conflicts. Rebase
                        // proposals that captured the same target/configuration, but never
                        // rebase over a user edit or a different conversation turn.
                        if !proposal.before.is_null() {
                            let after = self.workflow_snapshot(&proposal.args);
                            for pending in &mut self.agent_pane.workflows.proposals {
                                if pending.turn == turn
                                    && pending.id != proposal.id
                                    && pending.status == ProposalStatus::Pending
                                    && pending.before == proposal.before
                                {
                                    pending.before = after.clone();
                                }
                            }
                        }
                        self.agent_pane.workflows.undo.push((turn.into(), undo));
                        if !self.agent_turns.iter().any(|t| t == turn) {
                            self.agent_turns.push(turn.into());
                        }
                        (ProposalStatus::Approved, WorkflowStatus::Approved)
                    }
                    Err(error) => (ProposalStatus::Failed(error), WorkflowStatus::Failed),
                }
            } else {
                (ProposalStatus::Declined, WorkflowStatus::Declined)
            };
            let decided = &mut self.agent_pane.workflows.proposals[index];
            decided.status = status;
            decided.decided_at = Some(chrono::Local::now().timestamp());
            let add = matches!(proposal.args, tr_agent_tools::args::LiveArgs::AddWatchlistSymbols(_));
            for symbol in &mut decided.symbols {
                let existed = proposal.before["symbols"].as_array().is_some_and(|items| {
                    items.iter().any(|s| s.as_str().is_some_and(|s| crate::symbol_search::same_key(s, &symbol.symbol)))
                });
                symbol.outcome = Some(
                    if !approve || !symbol.selected {
                        if add { "Declined" } else { "Kept" }
                    } else if decided.status != ProposalStatus::Approved {
                        "Failed"
                    } else if add {
                        if existed { "Already in list" } else { "Added" }
                    } else if existed {
                        "Removed"
                    } else {
                        "Not in list"
                    }
                    .into(),
                );
            }
            self.agent_pane.workflows.context.push(WorkflowResult::Confirmation {
                proposal_id: proposal.id,
                status: result,
                summary: decided.decision_summary(),
            });
        }
        if self.agent_pane.running
            && self.agent_pane.workflows.context.len() > context_start
            && let Some(session) = &self.agent_pane.session
            && session.send(Command::WorkflowResults(self.agent_pane.workflows.context[context_start..].to_vec()))
        {
            // Transfer ownership, not a second copy: the session serializes steer
            // and start, retaining these only if steer fails (including completion races).
            self.agent_pane.workflows.context.truncate(context_start);
        }
        self.agent_save_pending = true;
        self.autosave();
    }

    pub(crate) fn undo_workflows(&mut self, turn: &str, summary: &mut crate::agent_bridge::UndoSummary) {
        use crate::agent_bridge::WorkflowUndo;
        self.agent_pane.workflows.expire(std::time::Instant::now(), Some(turn));
        let mut retained = Vec::new();
        let undo = std::mem::take(&mut self.agent_pane.workflows.undo);
        for (owner, undo) in undo.into_iter().rev() {
            if owner != turn {
                retained.push((owner, undo));
                continue;
            }
            let result: Result<(), String> = (|| {
                match undo {
                    WorkflowUndo::Alert { before, after, position } => {
                        let id = before.as_ref().or(after.as_ref()).unwrap().id;
                        let current = self.price_alerts.book.alerts.iter().find(|a| a.id == id);
                        if current.is_none() && after.is_some() {
                            return Err("you deleted that alert".into());
                        }
                        let same_config = match (current, after.as_ref()) {
                            (Some(current), Some(expected)) => {
                                alert_configuration(current) == alert_configuration(expected)
                            }
                            (None, _) => true,
                            _ => false,
                        };
                        if !same_config {
                            return Err("you edited that alert".into());
                        }
                        if let Some(before) = before {
                            if current.is_none()
                                && self.price_alerts.book.alerts.len() >= tr_core::price_alerts::MAX_ALERTS
                            {
                                return Err("alert book is full".into());
                            }
                            // Preserve ordering (and duplicate protection) on inverse updates.
                            self.price_alerts.book.alerts.retain(|alert| alert.id != id);
                            let position = position.min(self.price_alerts.book.alerts.len());
                            self.price_alerts.book.alerts.insert(position, before);
                            self.price_alerts.book.reset_rule(id);
                        } else {
                            self.price_alerts.book.delete(id);
                        }
                        self.price_alerts.changed(true);
                    }
                    WorkflowUndo::Watchlist { before: None, after: Some(after) } => {
                        let index =
                            self.workflow_list_index(&after.name).ok_or("watchlist was renamed or no longer exists")?;
                        {
                            let current = &self.watchlists[index];
                            if current.workflow_id != after.workflow_id
                                || current.agent_turn.as_deref() != Some(turn)
                                || current.symbols.iter().any(|s| !current.agent_entries.contains_key(s))
                            {
                                return Err("you added to that watchlist".into());
                            }
                            self.delete_watchlist(index)?;
                        }
                    }
                    WorkflowUndo::Watchlist { before: Some(before), after: None } => {
                        if self.workflow_list_index(&before.name).is_some() {
                            return Err("a watchlist now has that name".into());
                        }
                        if self.watchlists.len() >= 50 {
                            return Err("watchlist limit reached".into());
                        }
                        self.watchlists.push(before);
                    }
                    WorkflowUndo::Watchlist { before: Some(before), after: Some(after) } => {
                        let index = self.workflow_list_index(&after.name).ok_or("watchlist no longer exists")?;
                        let current = self.watchlists[index].clone();
                        if current.workflow_id != after.workflow_id {
                            return Err("that watchlist was replaced".into());
                        }
                        let same_key = crate::symbol_search::same_key;
                        let added: Vec<_> = after
                            .symbols
                            .iter()
                            .filter(|s| !before.symbols.iter().any(|old| same_key(old, s)))
                            .collect();
                        let claimed: Vec<_> =
                            added
                                .iter()
                                .copied()
                                .filter(|s| {
                                    current.symbols.iter().find(|now| same_key(now, s)).is_some_and(|now| {
                                        current.agent_entries.get(now).map(String::as_str) != Some(turn)
                                    })
                                })
                                .collect();
                        let mut symbols = current.symbols.clone();
                        symbols.retain(|s| {
                            !added.iter().any(|a| same_key(s, a)) || claimed.iter().any(|a| same_key(s, a))
                        });
                        let removed: Vec<_> = before
                            .symbols
                            .iter()
                            .filter(|s| !after.symbols.iter().any(|a| same_key(s, a)))
                            .cloned()
                            .collect();
                        let readded = removed.iter().any(|s| current.symbols.iter().any(|now| same_key(s, now)));
                        let to_restore = removed
                            .iter()
                            .filter(|s| !current.symbols.iter().any(|now| same_key(s, now)))
                            .cloned()
                            .collect();
                        let (symbols, _, truncated) = super::watchlist_append(&symbols, to_restore);
                        if symbols == current.symbols {
                            return Err(if !claimed.is_empty() || readded {
                                "you re-added those symbols"
                            } else {
                                "those symbols no longer exist"
                            }
                            .into());
                        }
                        if !claimed.is_empty() || readded {
                            summary.skipped.push("you re-added those symbols".into());
                        }
                        if truncated {
                            return Err("watchlist is full".into());
                        }
                        let items = crate::watch_sections::resync(&current.items(), &symbols);
                        if index == self.active_watchlist {
                            self.set_watchlist_items(&items);
                        } else {
                            self.watchlists[index].set_items(&items);
                        }
                        self.watchlists[index].agent_turn = current.agent_turn;
                        for s in removed {
                            if !current.symbols.iter().any(|now| same_key(now, &s))
                                && let Some(owner) = before.agent_entries.get(&s)
                            {
                                self.watchlists[index].agent_entries.insert(s, owner.clone());
                            }
                        }
                    }
                    _ => {}
                }
                Ok(())
            })();
            match result {
                Ok(()) => summary.reverted += 1,
                Err(error) => summary.skipped.push(error),
            }
        }
        retained.reverse();
        self.agent_pane.workflows.undo = retained;
        self.agent_pane
            .workflows
            .undone
            .insert(turn.into(), (chrono::Local::now().timestamp(), !summary.skipped.is_empty()));
        self.agent_pane.workflows.context.push(tr_agent::codex::WorkflowResult::Undo {
            turn: turn.into(),
            reverted: summary.reverted,
            kept: summary.skipped.clone(),
        });
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
        self.agent_pane.workflows.expire(std::time::Instant::now(), None);
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
        self.agent_pane.workflows.reset();
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
                let changes: usize = self
                    .panes
                    .iter()
                    .filter_map(|pane| match &pane.kind {
                        PaneKind::Chart(chart) => Some(chart.agent_turn_change_count(id)),
                        _ => None,
                    })
                    .sum();
                if turn.undo_available || turn.chart_changes.is_none() {
                    turn.chart_changes =
                        Some(changes + self.agent_pane.workflows.undo.iter().filter(|(owner, _)| owner == id).count());
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
            // A5b-SCAN: click-only preload; no request is submitted or refresh scheduled.
            Action::OpenScanner(mut settings) => {
                settings.refresh = None;
                if settings.definition.validate().is_ok() {
                    self.open_panel(PaneKind::Scanner(Box::new(super::scanner_view::ScannerPane::from_settings(
                        settings,
                    ))));
                }
            }
            Action::Confirm { turn, approve, ids } => {
                self.confirm_shown_workflows(&turn, approve, Some(&ids));
                self.refresh_agent_undo();
            }
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
        let workflow_context = std::mem::take(&mut self.agent_pane.workflows.context);
        self.agent_command(Command::Turn(Box::new(BridgeTurn {
            input: TurnInput {
                model,
                effort: self.agent_pane.settings.effort.clone(),
                text: prompt.text,
                response_style: Some(self.agent_pane.settings.response_style),
                workflow_results: workflow_context,
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

    fn workflow(app: &mut TerminalApp, turn: &str, name: &str, value: serde_json::Value) -> tr_agent_tools::ToolResult {
        use tr_agent_tools::args::*;
        let args = match name {
            "create_alert" => LiveArgs::CreateAlert(serde_json::from_value(value).unwrap()),
            "update_alert" => LiveArgs::UpdateAlert(serde_json::from_value(value).unwrap()),
            "delete_alert" => LiveArgs::DeleteAlert(serde_json::from_value(value).unwrap()),
            "create_watchlist" => LiveArgs::CreateWatchlist(serde_json::from_value(value).unwrap()),
            "add_watchlist_symbols" => LiveArgs::AddWatchlistSymbols(serde_json::from_value(value).unwrap()),
            "remove_watchlist_symbols" => LiveArgs::RemoveWatchlistSymbols(serde_json::from_value(value).unwrap()),
            "delete_watchlist" => LiveArgs::DeleteWatchlist(serde_json::from_value(value).unwrap()),
            _ => panic!("workflow fixture"),
        };
        app.agent_workflow(turn, args)
    }
    fn workflow_app(name: &str) -> TerminalApp {
        let mut app =
            TerminalApp::test_workspace(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target").join(name));
        app.price_alerts.book = Default::default();
        app
    }
    // A5b-SCAN: application action only preloads a NEW pane, never starts a scan.
    #[test]
    fn open_scanner_action_preloads_definition_universe_and_disables_refresh() {
        let mut app = workflow_app("agent-open-scanner");
        let before = app.panes.len();
        let mut settings = crate::layouts::ScanSettings::from(tr_core::scanner::ScanDefinition::volume_thrust());
        settings.universe = tr_data::universe::Universe::Nasdaq;
        settings.refresh = Some(1);
        app.agent_action(&egui::Context::default(), 0, Action::OpenScanner(settings.clone()));
        assert_eq!(app.panes.len(), before + 1);
        let PaneKind::Scanner(scanner) = &app.panes.last().unwrap().kind else { panic!() };
        assert_eq!(scanner.definition, settings.definition);
        assert_eq!(scanner.universe, settings.universe);
        assert!(!scanner.running);
        assert!(scanner.rows.is_empty());
        assert_eq!(scanner.generation, 0);
        assert_eq!(scanner.refresh, None);
    }
    // End A5b-SCAN action test.

    fn price() -> serde_json::Value {
        serde_json::json!({"symbol":"AAPL US Equity","kind":"price","direction":"above","level":100})
    }

    #[test]
    fn symbol_selection_partial_add_preserves_order_reports_decisions_once_and_undoes_only_additions() {
        use serde_json::json;
        let mut app = workflow_app("symbol-partial-add");
        app.create_watchlist("Agent Test".into(), vec![]).unwrap();
        workflow(
            &mut app,
            "pick:1",
            "add_watchlist_symbols",
            json!({"watchlist":"Agent Test","symbols":["NVDA US Equity","MSFT US Equity","AAPL US Equity"]}),
        );
        let p = &mut app.agent_pane.workflows.proposals[0];
        assert!(p.symbols.iter().all(|s| s.selected));
        p.symbols[1].selected = false;
        assert!(app.watchlists[1].symbols.is_empty());
        assert!(app.agent_pane.workflows.decision_line("pick:1").is_none());
        app.confirm_workflows("pick:1", true);
        assert_eq!(app.watchlists[1].symbols, ["NVDA US Equity", "AAPL US Equity"]);
        let p = &app.agent_pane.workflows.proposals[0];
        assert_eq!(
            p.symbols.iter().map(|s| s.outcome.as_deref().unwrap()).collect::<Vec<_>>(),
            ["Added", "Declined", "Added"]
        );
        assert_eq!(p.outcome_summary(), "Added 2 of 3 symbols to Agent Test");
        let tr_agent::codex::WorkflowResult::Confirmation { summary, .. } = &app.agent_pane.workflows.context[0] else {
            panic!("confirmation")
        };
        assert!(summary.contains("approved symbols: [NVDA US Equity, AAPL US Equity]"), "{summary}");
        assert!(summary.contains("declined symbols: [MSFT US Equity]"), "{summary}");
        assert!(app.agent_pane.workflows.decision_line("pick:1").unwrap().starts_with("You approved 2 of 3 · "));
        app.confirm_workflows("pick:1", true);
        app.confirm_workflows("pick:1", false);
        assert_eq!(app.agent_pane.workflows.context.len(), 1, "delivery-once decision");
        app.add_to_named_watchlist("Agent Test", "MSFT US Equity".into(), None).unwrap();
        assert_eq!(app.undo_agent_turn("pick:1").unwrap().reverted, 1);
        assert_eq!(app.watchlists[1].symbols, ["MSFT US Equity"], "undo leaves the unapproved symbol alone");
    }

    #[test]
    fn symbol_selection_partial_remove_reports_kept_and_undoes_only_removals() {
        use serde_json::json;
        let mut app = workflow_app("symbol-partial-remove");
        app.create_watchlist(
            "Agent Test".into(),
            vec!["AAPL US Equity".into(), "MSFT US Equity".into(), "NVDA US Equity".into()],
        )
        .unwrap();
        workflow(
            &mut app,
            "pick:2",
            "remove_watchlist_symbols",
            json!({"watchlist":"Agent Test","symbols":["AAPL US Equity","MSFT US Equity","NVDA US Equity"]}),
        );
        app.agent_pane.workflows.proposals[0].symbols[1].selected = false;
        app.confirm_workflows("pick:2", true);
        assert_eq!(app.watchlists[1].symbols, ["MSFT US Equity"]);
        let p = &app.agent_pane.workflows.proposals[0];
        assert_eq!(
            p.symbols.iter().map(|s| s.outcome.as_deref().unwrap()).collect::<Vec<_>>(),
            ["Removed", "Kept", "Removed"]
        );
        assert_eq!(p.outcome_summary(), "Removed 2 of 3 symbols from Agent Test");
        assert!(p.decision_summary().contains("declined symbols: [MSFT US Equity]"));
        app.add_to_named_watchlist("Agent Test", "TSLA US Equity".into(), None).unwrap();
        assert_eq!(app.undo_agent_turn("pick:2").unwrap().reverted, 1);
        assert_eq!(app.watchlists[1].symbols, ["MSFT US Equity", "TSLA US Equity", "AAPL US Equity", "NVDA US Equity"]);
    }

    #[test]
    fn symbol_selection_all_unticked_stays_pending_until_explicitly_declined_without_undo() {
        use crate::agent_bridge::ProposalStatus;
        use serde_json::json;
        let mut app = workflow_app("symbol-none");
        app.create_watchlist("Agent Test".into(), vec![]).unwrap();
        workflow(
            &mut app,
            "none:1",
            "add_watchlist_symbols",
            json!({"watchlist":"Agent Test","symbols":["AAPL US Equity","MSFT US Equity"]}),
        );
        for symbol in &mut app.agent_pane.workflows.proposals[0].symbols {
            symbol.selected = false;
        }
        assert!(!app.agent_pane.workflows.proposals[0].is_selected());
        workflow(&mut app, "none:1", "create_watchlist", json!({"name":"Selected"}));
        app.confirm_workflows("none:1", true);
        assert_eq!(app.agent_pane.workflows.proposals[0].status, ProposalStatus::Pending);
        assert!(app.agent_pane.workflows.proposals[0].decided_at.is_none());
        assert!(app.agent_pane.workflows.proposals[0].symbols.iter().all(|s| s.outcome.is_none()));
        assert_eq!(app.agent_pane.workflows.context.len(), 1, "only the selected row is reported");
        assert!(app.agent_pane.workflows.decision_line("none:1").is_none());
        app.confirm_workflows("none:1", false);
        assert_eq!(app.agent_pane.workflows.proposals[0].status, ProposalStatus::Declined);
        assert_eq!(app.agent_pane.workflows.context.len(), 2);
        assert_eq!(app.agent_pane.workflows.proposals[0].outcome_summary(), "Declined adding 2 symbols to Agent Test");
        assert!(app.watchlists[1].symbols.is_empty());
        assert_eq!(app.agent_pane.workflows.undo.len(), 1, "only the selected list creation has undo");
        assert!(
            app.agent_pane.workflows.proposals[0]
                .decision_summary()
                .contains("approved symbols: []; declined symbols: [AAPL US Equity, MSFT US Equity]")
        );
    }

    #[test]
    fn symbol_selection_subitem_host_guard_applies_to_both_approve_and_decline() {
        use crate::agent_bridge::ProposalStatus;
        use serde_json::json;
        for approve in [true, false] {
            let mut app = workflow_app(&format!("symbol-host-guard-{approve}"));
            app.create_watchlist("Agent Test".into(), vec![]).unwrap();
            workflow(
                &mut app,
                "guard:1",
                "add_watchlist_symbols",
                json!({"watchlist":"Agent Test","symbols":["AAPL US Equity","MSFT US Equity"]}),
            );
            let id = app.agent_pane.workflows.proposals[0].id;
            app.agent_pane.symbol_shown.insert("guard:1".into(), vec![(id, 0, "AAPL US Equity".into())]);
            app.confirm_shown_workflows("guard:1", approve, Some(&[id]));
            assert_eq!(app.agent_pane.workflows.proposals[0].status, ProposalStatus::Pending);
            assert!(app.watchlists[1].symbols.is_empty());
            assert!(app.agent_pane.workflows.context.is_empty());
            app.agent_pane.symbol_shown.get_mut("guard:1").unwrap().push((id, 1, "MSFT US Equity".into()));
            app.confirm_shown_workflows("guard:1", approve, Some(&[id]));
            assert_eq!(
                app.agent_pane.workflows.proposals[0].status,
                if approve { ProposalStatus::Approved } else { ProposalStatus::Declined }
            );
        }
    }

    #[test]
    fn symbol_selection_already_present_missing_and_expired_rows_have_truthful_status() {
        use serde_json::json;
        let mut app = workflow_app("symbol-noops");
        app.create_watchlist("Agent Test".into(), vec!["AAPL US Equity".into()]).unwrap();
        workflow(
            &mut app,
            "noop:1",
            "add_watchlist_symbols",
            json!({"watchlist":"Agent Test","symbols":["AAPL US Equity","MSFT US Equity"]}),
        );
        app.confirm_workflows("noop:1", true);
        assert_eq!(app.agent_pane.workflows.proposals[0].symbols[0].outcome.as_deref(), Some("Already in list"));
        let tr_agent::codex::WorkflowResult::Confirmation { summary, .. } = &app.agent_pane.workflows.context[0] else {
            panic!("confirmation")
        };
        assert!(summary.contains("approved symbols: [MSFT US Equity]"), "{summary}");
        assert!(summary.contains("approved, no change (already in list) symbols: [AAPL US Equity]"), "{summary}");
        assert_eq!(
            app.agent_pane.workflows.turn_summary("noop:1").as_deref(),
            Some("Added 1 of 2 symbols to Agent Test · 1 already in list")
        );
        assert_eq!(app.undo_agent_turn("noop:1").unwrap().reverted, 1);
        assert_eq!(app.watchlists[1].symbols, ["AAPL US Equity"]);
        let result = workflow(
            &mut app,
            "noop:2",
            "remove_watchlist_symbols",
            json!({"watchlist":"Agent Test","symbols":["AAPL US Equity","MSFT US Equity"]}),
        );
        assert!(result.success);
        let tr_agent_tools::Content::Text(text) = &result.content[0] else { panic!("JSON output") };
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(value["symbols"][0]["symbol"], "MSFT US Equity");
        assert_eq!(value["symbols"][0]["error"], "not_in_list");
        app.confirm_workflows("noop:2", true);
        assert!(app.agent_pane.workflows.proposals[1].symbols.is_empty());
        let tr_agent::codex::WorkflowResult::Confirmation { summary, .. } =
            app.agent_pane.workflows.context.last().unwrap()
        else {
            panic!("confirmation")
        };
        assert!(summary.contains("approved symbols: [AAPL US Equity]"), "{summary}");
        assert!(!summary.contains("MSFT US Equity"), "missing entries are rejected before proposal: {summary}");
        assert_eq!(
            app.agent_pane.workflows.turn_summary("noop:2").as_deref(),
            Some("Removed 1 symbol from Agent Test")
        );
        workflow(
            &mut app,
            "noop:3",
            "add_watchlist_symbols",
            json!({"watchlist":"Agent Test","symbols":["AAPL US Equity","MSFT US Equity"]}),
        );
        let deadline = app.agent_pane.workflows.proposals[2].deadline;
        app.agent_pane.workflows.expire(deadline, None);
        assert!(app.agent_pane.workflows.proposals[2].symbols.iter().all(|s| s.outcome.as_deref() == Some("Expired")));
        assert_eq!(app.agent_pane.workflows.turn_summary("noop:3").as_deref(), Some("Proposal expired"));
        let first = app.agent_pane.workflows.decision_line("noop:3").unwrap();
        assert!(first.starts_with("Expired · "));
        app.agent_pane.workflows.expire(deadline, None);
        assert_eq!(app.agent_pane.workflows.decision_line("noop:3").as_deref(), Some(first.as_str()));
    }

    #[test]
    fn workflow_feedback_headers_cover_create_decline_alert_and_mixed_decisions() {
        use serde_json::json;
        let mut app = workflow_app("workflow-feedback-headers");
        workflow(&mut app, "feedback:1", "create_watchlist", json!({"name":"Agent Test"}));
        assert_eq!(
            app.agent_pane.workflows.turn_summary("feedback:1").as_deref(),
            Some("Awaiting approval · 1 change")
        );
        app.confirm_workflows("feedback:1", true);
        assert_eq!(app.agent_pane.workflows.turn_summary("feedback:1").as_deref(), Some("Created Agent Test"));
        app.agent_pane.workflows.proposals[0].decided_at = Some(crate::agent_demo::SAMPLE_NOW);
        let time = chrono::DateTime::from_timestamp(crate::agent_demo::SAMPLE_NOW, 0)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%H:%M")
            .to_string();
        assert_eq!(
            app.agent_pane.workflows.decision_line("feedback:1").unwrap(),
            format!("You approved this · {time}")
        );
        workflow(
            &mut app,
            "feedback:2",
            "add_watchlist_symbols",
            json!({"watchlist":"Agent Test","symbols":["NVDA US Equity","TSLA US Equity","COINBASE:ETH-USD"]}),
        );
        app.confirm_workflows("feedback:2", false);
        assert_eq!(
            app.agent_pane.workflows.turn_summary("feedback:2").as_deref(),
            Some("Declined adding 3 symbols to Agent Test")
        );
        assert!(app.agent_pane.workflows.decision_line("feedback:2").unwrap().starts_with("You declined this · "));
        workflow(
            &mut app,
            "feedback:3",
            "create_alert",
            json!({"symbol":"COINBASE:BTC-USD Curncy","kind":"close","direction":"above","level":85000,"timeframe":"4H"}),
        );
        app.confirm_workflows("feedback:3", true);
        let summary = app.agent_pane.workflows.turn_summary("feedback:3").unwrap();
        assert!(summary.starts_with("Created alert BTC/USD · Coinbase close above "), "{summary}");
        assert!(summary.ends_with(" · 4H"), "{summary}");
        workflow(&mut app, "feedback:4", "create_watchlist", json!({"name":"One"}));
        workflow(&mut app, "feedback:4", "create_watchlist", json!({"name":"Two"}));
        app.agent_pane.workflows.proposals.last_mut().unwrap().selected = false;
        app.confirm_workflows("feedback:4", true);
        assert!(app.agent_pane.workflows.decision_line("feedback:4").is_none());
        app.confirm_workflows("feedback:4", false);
        assert_eq!(
            app.agent_pane.workflows.turn_summary("feedback:4").as_deref(),
            Some("Created One, declined creating Two")
        );
        assert!(app.agent_pane.workflows.decision_line("feedback:4").unwrap().starts_with("You approved 1 of 2 · "));
    }

    #[test]
    fn review_all_unticked_symbol_proposal_is_left_pending_like_an_unselected_row() {
        use crate::agent_bridge::ProposalStatus;
        use serde_json::json;
        let mut app = workflow_app("review-symbol-none-pending");
        app.create_watchlist("Agent Test".into(), vec![]).unwrap();
        workflow(&mut app, "rnone:1", "create_watchlist", json!({"name":"Unselected"}));
        workflow(
            &mut app,
            "rnone:1",
            "add_watchlist_symbols",
            json!({"watchlist":"Agent Test","symbols":["AAPL US Equity","MSFT US Equity"]}),
        );
        workflow(&mut app, "rnone:1", "create_watchlist", json!({"name":"Selected"}));
        app.agent_pane.workflows.proposals[0].selected = false;
        for symbol in &mut app.agent_pane.workflows.proposals[1].symbols {
            symbol.selected = false;
        }
        app.confirm_workflows("rnone:1", true);
        assert_eq!(app.agent_pane.workflows.proposals[0].status, ProposalStatus::Pending, "unselected row stays");
        assert_eq!(
            app.agent_pane.workflows.proposals[1].status,
            ProposalStatus::Pending,
            "all-unticked counts as unselected, so it must be treated the same"
        );
    }

    #[test]
    fn review_failed_approval_decision_line_does_not_claim_nothing_was_approved() {
        use crate::agent_bridge::ProposalStatus;
        use serde_json::json;
        let mut app = workflow_app("review-failed-line");
        workflow(&mut app, "rfail:1", "create_watchlist", json!({"name":"Agent Test"}));
        // The user creates the same list before approving, so the approval fails at click time.
        app.create_watchlist("Agent Test".into(), vec![]).unwrap();
        app.confirm_workflows("rfail:1", true);
        assert!(matches!(app.agent_pane.workflows.proposals[0].status, ProposalStatus::Failed(_)));
        let line = app.agent_pane.workflows.decision_line("rfail:1").unwrap();
        assert!(line.starts_with("You approved this · 1 change failed · "), "{line}");
        workflow(&mut app, "rfail:2", "create_watchlist", json!({"name":"One"}));
        workflow(&mut app, "rfail:2", "create_watchlist", json!({"name":"Two"}));
        workflow(&mut app, "rfail:2", "create_watchlist", json!({"name":"Three"}));
        app.create_watchlist("Two".into(), vec![]).unwrap();
        app.agent_pane.workflows.proposals.last_mut().unwrap().selected = false;
        app.confirm_workflows("rfail:2", true);
        app.confirm_workflows("rfail:2", false);
        let line = app.agent_pane.workflows.decision_line("rfail:2").unwrap();
        assert!(line.starts_with("You approved 2 of 3 · 1 failed · "), "{line}");
    }

    #[test]
    fn review_expired_status_time_is_the_deadline_not_when_it_was_noticed() {
        use serde_json::json;
        let mut app = workflow_app("review-expired-time");
        workflow(&mut app, "rexp:1", "create_watchlist", json!({"name":"Agent Test"}));
        // e.g. the laptop slept through the deadline and the app noticed two hours later.
        let now = std::time::Instant::now();
        app.agent_pane.workflows.proposals[0].deadline = now - std::time::Duration::from_secs(7200);
        app.agent_pane.workflows.expire(now, None);
        let decided = app.agent_pane.workflows.proposals[0].decided_at.unwrap();
        let expected = chrono::Local::now().timestamp() - 7200;
        assert!((decided - expected).abs() < 2, "decided_at {decided} should be about the deadline {expected}");
        let time = chrono::DateTime::from_timestamp(decided, 0)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%H:%M")
            .to_string();
        assert_eq!(app.agent_pane.workflows.decision_line("rexp:1").unwrap(), format!("Expired · {time}"));
    }

    #[test]
    fn workflow_decision_line_reports_undo_time_and_kept_items() {
        use serde_json::json;
        for kept in [false, true] {
            let mut app = workflow_app(&format!("workflow-undo-line-{kept}"));
            workflow(&mut app, "undo-line:1", "create_watchlist", json!({"name":"Research"}));
            app.confirm_workflows("undo-line:1", true);
            if kept {
                app.add_to_named_watchlist("Research", "AAPL US Equity".into(), None).unwrap();
            }
            let summary = app.undo_agent_turn("undo-line:1").unwrap();
            assert_eq!(!summary.skipped.is_empty(), kept);
            let (timestamp, recorded_kept) = app.agent_pane.workflows.undone["undo-line:1"];
            assert_eq!(recorded_kept, kept);
            assert!((timestamp - chrono::Local::now().timestamp()).abs() < 2);
            let time = chrono::DateTime::from_timestamp(timestamp, 0)
                .unwrap()
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string();
            assert_eq!(
                app.agent_pane.workflows.decision_line("undo-line:1").unwrap(),
                format!("You approved this · {} {time}", if kept { "partly undone" } else { "undone" })
            );
        }
    }

    #[test]
    fn workflow_decision_line_distinguishes_declined_and_expired() {
        use serde_json::json;
        let mut app = workflow_app("workflow-declined-expired-line");
        workflow(&mut app, "mixed-expiry:1", "create_watchlist", json!({"name":"One"}));
        workflow(&mut app, "mixed-expiry:1", "create_watchlist", json!({"name":"Two"}));
        let shown = [app.agent_pane.workflows.proposals[0].id];
        app.confirm_shown_workflows("mixed-expiry:1", false, Some(&shown));
        app.agent_pane.workflows.proposals[1].deadline = std::time::Instant::now() - std::time::Duration::from_secs(1);
        app.agent_pane.workflows.expire(std::time::Instant::now(), None);
        for p in &mut app.agent_pane.workflows.proposals {
            p.decided_at = Some(crate::agent_demo::SAMPLE_NOW);
        }
        let time = chrono::DateTime::from_timestamp(crate::agent_demo::SAMPLE_NOW, 0)
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%H:%M")
            .to_string();
        assert_eq!(
            app.agent_pane.workflows.decision_line("mixed-expiry:1").unwrap(),
            format!("You declined 1 · 1 expired · {time}")
        );
    }

    #[test]
    fn single_symbol_noop_confirmation_reports_approved_without_change() {
        use serde_json::json;
        let mut app = workflow_app("single-symbol-noop-model-report");
        app.create_watchlist("Research".into(), vec!["AAPL US Equity".into()]).unwrap();
        workflow(
            &mut app,
            "add",
            "add_watchlist_symbols",
            json!({"watchlist":"Research","symbols":["AAPL US Equity"]}),
        );
        app.confirm_workflows("add", true);
        let tr_agent::codex::WorkflowResult::Confirmation { status, summary, .. } =
            app.agent_pane.workflows.context.last().unwrap()
        else {
            panic!("confirmation")
        };
        assert!(matches!(status, tr_agent::codex::WorkflowStatus::Approved));
        assert!(summary.contains("approved, no change (already in list) symbols: [AAPL US Equity]"), "{summary}");
        assert!(summary.contains("approved symbols: []"), "{summary}");
        assert!(app.agent_pane.workflows.decision_line("add").unwrap().starts_with("You approved this · "));
        let result = workflow(
            &mut app,
            "remove",
            "remove_watchlist_symbols",
            json!({"watchlist":"Research","symbols":["MSFT US Equity"]}),
        );
        assert!(!result.success, "missing removals fail before a proposal exists");
        assert_eq!(app.agent_pane.workflows.proposals.len(), 1);
        assert_eq!(app.watchlists[1].symbols, ["AAPL US Equity"]);
    }

    #[test]
    fn review_remove_watchlist_symbols_matches_same_instrument_key() {
        use serde_json::json;
        let mut app = workflow_app("review-remove-crypto-key");
        app.create_watchlist("Crypto".into(), vec!["BINANCE:XRPUSDT".into()]).unwrap();
        let stored = app.watchlists[1].symbols[0].clone();
        // The registry normalizes every symbol argument to security(hit).key() before the host sees it.
        let normalized = tr_data::security("BINANCE:XRPUSDT").key();
        assert!(crate::symbol_search::same_key(&stored, &normalized), "{stored} vs {normalized}");
        assert!(
            workflow(
                &mut app,
                "rm:1",
                "remove_watchlist_symbols",
                json!({"watchlist":"Crypto","symbols":[normalized]})
            )
            .success
        );
        app.confirm_workflows("rm:1", true);
        assert_eq!(app.agent_pane.workflows.proposals[0].status, crate::agent_bridge::ProposalStatus::Approved);
        assert!(app.watchlists[1].symbols.is_empty(), "approved removal left {:?}", app.watchlists[1].symbols);
    }

    #[test]
    fn review_undo_does_not_report_renamed_watchlist_as_reverted() {
        use serde_json::json;
        let mut app = workflow_app("review-undo-renamed-list");
        workflow(&mut app, "ren:1", "create_watchlist", json!({"name":"Crypto"}));
        app.confirm_workflows("ren:1", true);
        let index = app.workflow_list_index("Crypto").unwrap();
        app.rename_watchlist(index, "Coins".into()).unwrap();
        let report = app.undo_agent_turn("ren:1").unwrap();
        assert!(app.workflow_list_index("Coins").is_some(), "user-renamed list is kept");
        assert_eq!((report.reverted, report.skipped.len()), (0, 1), "{report:?}");
    }

    #[test]
    fn workflow_undo_keeps_missing_targets_and_equivalent_user_readds() {
        use serde_json::json;
        let mut app = workflow_app("workflow-kept-targets");
        workflow(&mut app, "gone:1", "create_watchlist", json!({"name":"Crypto"}));
        app.confirm_workflows("gone:1", true);
        app.delete_watchlist(1).unwrap();
        let report = app.undo_agent_turn("gone:1").unwrap();
        assert_eq!((report.reverted, report.skipped.len()), (0, 1));
        workflow(&mut app, "gone:2", "create_alert", price());
        app.confirm_workflows("gone:2", true);
        let id = app.price_alerts.book.alerts[0].id;
        app.price_alerts.book.delete(id);
        let report = app.undo_agent_turn("gone:2").unwrap();
        assert_eq!((report.reverted, report.skipped.len()), (0, 1));
        app.create_watchlist("Crypto".into(), vec!["BINANCE:XRPUSDT".into()]).unwrap();
        let canonical = tr_data::security("BINANCE:XRPUSDT").key();
        workflow(&mut app, "key:1", "remove_watchlist_symbols", json!({"watchlist":"Crypto","symbols":[canonical]}));
        app.confirm_workflows("key:1", true);
        assert_eq!(app.undo_agent_turn("key:1").unwrap().reverted, 1);
        assert_eq!(app.watchlists[1].symbols.len(), 1);
        workflow(&mut app, "key:2", "remove_watchlist_symbols", json!({"watchlist":"Crypto","symbols":[canonical]}));
        app.confirm_workflows("key:2", true);
        app.add_to_named_watchlist("Crypto", canonical.clone(), None).unwrap();
        // The user's equivalent spelling is not the removed string, but is the same instrument.
        app.watchlists[1].symbols[0] = canonical;
        let before = app.watchlists[1].clone();
        let report = app.undo_agent_turn("key:2").unwrap();
        assert_eq!((report.reverted, report.skipped.len()), (0, 1));
        assert_eq!(app.watchlists[1], before);
    }

    #[test]
    fn workflow_alert_snapshot_ignores_firing_but_rejects_configuration_changes() {
        use crate::agent_bridge::ProposalStatus;
        use serde_json::json;
        use tr_core::price_alerts::{Direction, Observation};
        for tool in ["update_alert", "delete_alert"] {
            for changed in ["runtime", "symbol", "level", "direction", "rule", "repeat"] {
                let mut app = workflow_app(&format!("workflow-alert-snapshot-{tool}-{changed}"));
                let id = app.price_alerts.book.add("AAPL US Equity".into(), 100., Direction::Above, true).unwrap();
                workflow(
                    &mut app,
                    "snap:1",
                    tool,
                    if tool == "update_alert" { json!({"id":id,"level":120}) } else { json!({"id":id}) },
                );
                if changed == "runtime" {
                    let now = chrono::Utc::now().timestamp();
                    let q = |price, ts| Observation {
                        symbol: "AAPL US Equity".into(),
                        price,
                        ts,
                        simulated: true,
                        source: "test".into(),
                    };
                    assert!(app.price_alerts.book.observe(q(99., now - 1), now).is_empty());
                    assert_eq!(app.price_alerts.book.observe(q(101., now), now).len(), 1);
                } else {
                    let alert = &mut app.price_alerts.book.alerts[0];
                    match changed {
                        "symbol" => alert.symbol = "MSFT US Equity".into(),
                        "level" => alert.level = 101.,
                        "direction" => alert.direction = Direction::Below,
                        "rule" => {
                            alert.rule = Some(tr_core::price_alerts::Rule::Formula {
                                expression: "C".into(),
                                timeframe: tr_core::Timeframe::H1,
                            })
                        }
                        "repeat" => alert.repeat = tr_core::price_alerts::Repeat::Rearm { cooldown_secs: 60 },
                        _ => unreachable!(),
                    }
                }
                app.confirm_workflows("snap:1", true);
                let status = &app.agent_pane.workflows.proposals[0].status;
                if changed == "runtime" {
                    assert_eq!(*status, ProposalStatus::Approved, "{tool}");
                } else {
                    assert!(matches!(status, ProposalStatus::Failed(_)), "{tool} {changed}: {status:?}");
                    assert_eq!(app.price_alerts.book.alerts.len(), 1);
                }
            }
        }
    }

    #[test]
    fn workflow_alert_undo_restores_original_order() {
        use serde_json::json;
        let mut app = workflow_app("workflow-alert-order");
        for _ in 0..3 {
            workflow(&mut app, "order:1", "create_alert", price());
        }
        app.confirm_workflows("order:1", true);
        let original = app.price_alerts.book.alerts.clone();
        for (turn, tool) in [("order:2", "update_alert"), ("order:3", "delete_alert")] {
            workflow(
                &mut app,
                turn,
                tool,
                if tool == "update_alert" {
                    json!({"id":original[1].id,"level":123})
                } else {
                    json!({"id":original[1].id})
                },
            );
            app.confirm_workflows(turn, true);
            assert_eq!(app.undo_agent_turn(turn).unwrap().reverted, 1);
            assert_eq!(app.price_alerts.book.alerts, original);
        }
    }

    #[test]
    fn workflow_names_and_creator_labels_use_actual_targets() {
        use serde_json::json;
        let mut app = workflow_app("workflow-card-labels");
        app.create_watchlist("Actual Name".into(), vec![]).unwrap();
        for tool in ["add_watchlist_symbols", "remove_watchlist_symbols", "delete_watchlist"] {
            workflow(
                &mut app,
                "label:1",
                tool,
                if tool == "delete_watchlist" {
                    json!({"watchlist":"aCTUAL nAME"})
                } else {
                    json!({"watchlist":"aCTUAL nAME","symbols":["AAPL US Equity"]})
                },
            );
            let summary = &app.agent_pane.workflows.proposals.last().unwrap().summary;
            assert!(summary.contains("Actual Name"), "{summary}");
            assert!(!summary.contains("aCTUAL nAME"));
        }
        workflow(&mut app, "old:1", "create_alert", price());
        app.confirm_workflows("old:1", true);
        let id = app.price_alerts.book.alerts[0].id;
        workflow(&mut app, "new:1", "delete_alert", json!({"id":id}));
        assert_eq!(
            app.agent_pane.workflows.proposals.last().unwrap().ownership_label(),
            Some("Created by the agent in an earlier conversation")
        );
        app.price_alerts.book.edit(id, 101., tr_core::price_alerts::Direction::Above).unwrap();
        workflow(&mut app, "new:2", "delete_alert", json!({"id":id}));
        assert_eq!(
            app.agent_pane.workflows.proposals.last().unwrap().ownership_label(),
            Some("Created by the agent in an earlier conversation")
        );
        let id = app
            .price_alerts
            .book
            .add("AAPL US Equity".into(), 100., tr_core::price_alerts::Direction::Above, true)
            .unwrap();
        workflow(&mut app, "new:3", "delete_alert", json!({"id":id}));
        assert_eq!(
            app.agent_pane.workflows.proposals.last().unwrap().ownership_label(),
            Some("This alert was created by you")
        );
    }

    #[test]
    fn workflow_proposals_expire_at_session_boundaries() {
        use crate::agent_bridge::ProposalStatus;
        for boundary in ["stop", "new-thread", "read-only"] {
            let mut app = workflow_app(&format!("workflow-expire-{boundary}"));
            workflow(&mut app, "expire:1", "create_alert", price());
            match boundary {
                "stop" => {
                    app.agent_pane.transition(Onboarding::Stopped { reason: "stopped".into(), unsafe_backend: false })
                }
                "new-thread" => app.agent_pane.event(tr_agent::bridge::BridgeEvent::Recovered(
                    tr_agent::bridge::Recovery::HistoryLost { reason: "new thread".into() },
                )),
                "read-only" => {
                    app.agent_pane.state = Onboarding::Ready;
                    app.agent_pane.settings.tools = agent_pane::ToolsMode::Edit;
                    assert!(app.agent_pane.select_tools(agent_pane::ToolsMode::ReadOnly, true));
                }
                _ => unreachable!(),
            }
            app.confirm_workflows("expire:1", true);
            assert_eq!(app.agent_pane.workflows.proposals[0].status, ProposalStatus::Expired, "{boundary}");
            assert!(app.price_alerts.book.alerts.is_empty());
        }
    }

    #[test]
    fn workflow_multiselect_approve_decline_expire_reset_and_limits() {
        use crate::agent_bridge::ProposalStatus;
        use serde_json::json;
        let mut app = workflow_app("workflow-confirmation");
        for name in ["Crypto", "Research"] {
            assert!(workflow(&mut app, "session:1", "create_watchlist", json!({"name":name})).success);
        }
        assert_eq!(app.watchlists.len(), 1, "proposing must never apply");
        app.agent_pane.workflows.proposals[1].selected = false;
        app.confirm_workflows("session:1", true);
        assert_eq!(app.watchlists.len(), 2);
        assert!(app.workflow_list_index("cRyPtO").is_some());
        assert_eq!(app.agent_pane.workflows.proposals[0].status, ProposalStatus::Approved);
        assert_eq!(app.agent_pane.workflows.proposals[1].status, ProposalStatus::Pending);
        app.confirm_workflows("session:1", false);
        assert_eq!(app.watchlists.len(), 2);
        assert_eq!(app.agent_pane.workflows.proposals[1].status, ProposalStatus::Declined);
        workflow(&mut app, "session:2", "create_alert", price());
        assert!(app.price_alerts.book.alerts.is_empty());
        let deadline = app.agent_pane.workflows.proposals.last().unwrap().deadline;
        app.agent_pane.workflows.expire(deadline, None);
        app.confirm_workflows("session:2", true);
        assert!(app.price_alerts.book.alerts.is_empty());
        assert_eq!(app.agent_pane.workflows.proposals.last().unwrap().status, ProposalStatus::Expired);
        workflow(&mut app, "session:3", "create_alert", price());
        app.agent_pane.workflows.reset();
        app.confirm_workflows("session:3", true);
        assert!(app.price_alerts.book.alerts.is_empty());
        for n in 0..10 {
            assert!(workflow(&mut app, "session:4", "create_watchlist", json!({"name":format!("List{n}")})).success);
        }
        assert!(!workflow(&mut app, "session:4", "create_alert", price()).success);
        app.undo_agent_turn("session:4").unwrap_err();
        assert!(
            app.agent_pane
                .workflows
                .proposals
                .iter()
                .filter(|p| p.turn == "session:4")
                .all(|p| p.status == ProposalStatus::Expired)
        );
    }

    #[test]
    fn workflow_alert_ownership_stale_approval_undo_and_persistence() {
        use serde_json::json;
        use tr_core::price_alerts::Direction;
        let mut app = workflow_app("workflow-alert-ownership");
        assert!(workflow(&mut app, "session:1", "create_alert", price()).success);
        app.confirm_workflows("session:1", true);
        let original = app.price_alerts.book.alerts[0].clone();
        assert_eq!(original.agent_turn.as_deref(), Some("session:1"));
        assert!(app.price_alerts.save_error.is_none());
        let encoded = serde_json::to_value(&app.price_alerts.book).unwrap();
        let decoded: tr_core::price_alerts::Book = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded.alerts[0], original);
        let mut legacy = encoded;
        legacy["alerts"][0].as_object_mut().unwrap().remove("agent_turn");
        legacy["alerts"][0].as_object_mut().unwrap().remove("completed_only");
        legacy["alerts"][0].as_object_mut().unwrap().remove("note");
        legacy["alerts"][0].as_object_mut().unwrap().remove("config_revision");
        legacy["alerts"][0].as_object_mut().unwrap().remove("agent_created");
        legacy["alerts"][0].as_object_mut().unwrap().remove("created_at");
        let legacy: tr_core::price_alerts::Book = serde_json::from_value(legacy).unwrap();
        assert!(legacy.alerts[0].agent_turn.is_none());
        assert!(!legacy.alerts[0].completed_only);
        assert!(!legacy.alerts[0].agent_created);
        assert_eq!(legacy.alerts[0].config_revision, 0);
        workflow(&mut app, "session:2", "update_alert", json!({"id":original.id,"level":101}));
        assert!(!app.agent_pane.workflows.proposals.last().unwrap().user_owned);
        assert_eq!(app.price_alerts.book.alerts[0].level, 100.);
        app.confirm_workflows("session:2", true);
        assert_eq!(app.price_alerts.book.alerts[0].level, 101.);
        assert_eq!(app.undo_agent_turn("session:2").unwrap().reverted, 1);
        assert_eq!(app.price_alerts.book.alerts[0], original);
        workflow(&mut app, "other:1", "delete_alert", json!({"id":original.id}));
        assert!(app.agent_pane.workflows.proposals.last().unwrap().user_owned);
        assert_eq!(app.price_alerts.book.alerts.len(), 1);
        app.confirm_workflows("other:1", false);
        assert_eq!(app.price_alerts.book.alerts.len(), 1);
        workflow(&mut app, "other:2", "delete_alert", json!({"id":original.id}));
        app.confirm_workflows("other:2", true);
        assert!(app.price_alerts.book.alerts.is_empty());
        assert_eq!(app.undo_agent_turn("other:2").unwrap().reverted, 1);
        assert_eq!(app.price_alerts.book.alerts[0], original);
        workflow(&mut app, "session:3", "delete_alert", json!({"id":original.id}));
        app.price_alerts.book.edit(original.id, 102., Direction::Above).unwrap();
        assert!(app.price_alerts.book.alerts[0].agent_turn.is_none());
        app.confirm_workflows("session:3", true);
        assert!(matches!(
            app.agent_pane.workflows.proposals.last().unwrap().status,
            crate::agent_bridge::ProposalStatus::Failed(_)
        ));
        let undo = app.undo_agent_turn("session:1").unwrap();
        assert_eq!(undo.reverted, 0);
        assert_eq!(undo.skipped, ["you edited that alert"]);
        assert_eq!(app.price_alerts.book.alerts[0].level, 102.);
    }

    #[test]
    fn workflow_watchlist_inverse_merges_unrelated_user_changes_but_keeps_readded_entries() {
        use serde_json::json;
        let mut app = workflow_app("workflow-watchlist-undo");
        assert!(!workflow(&mut app, "s:0", "delete_watchlist", json!({"watchlist":"MAIN"})).success);
        workflow(&mut app, "s:1", "create_watchlist", json!({"name":"Crypto"}));
        app.confirm_workflows("s:1", true);
        workflow(
            &mut app,
            "s:2",
            "add_watchlist_symbols",
            json!({"watchlist":"cRyPtO","symbols":["AAPL US Equity","MSFT US Equity"]}),
        );
        app.confirm_workflows("s:2", true);
        let index = app.workflow_list_index("Crypto").unwrap();
        assert_eq!(app.watchlists[index].agent_entries.len(), 2);
        let bytes = serde_json::to_vec(&app.watchlists[index]).unwrap();
        let restored: super::super::NamedWatchlist = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored, app.watchlists[index]);
        app.add_to_named_watchlist("Crypto", "NVDA US Equity".into(), None).unwrap();
        assert_eq!(app.undo_agent_turn("s:2").unwrap().reverted, 1);
        assert_eq!(app.watchlists[index].symbols, ["NVDA US Equity"]);
        let kept = app.undo_agent_turn("s:1").unwrap();
        assert_eq!(kept.skipped, ["you added to that watchlist"]);
        workflow(
            &mut app,
            "s:3",
            "remove_watchlist_symbols",
            json!({"watchlist":"Crypto","symbols":["NVDA US Equity"]}),
        );
        app.confirm_workflows("s:3", true);
        assert!(app.watchlists[index].symbols.is_empty());
        assert_eq!(app.undo_agent_turn("s:3").unwrap().reverted, 1);
        assert_eq!(app.watchlists[index].symbols, ["NVDA US Equity"]);
        workflow(&mut app, "s:4", "add_watchlist_symbols", json!({"watchlist":"Crypto","symbols":["AAPL US Equity"]}));
        app.confirm_workflows("s:4", true);
        // Explicit user add of an existing entry claims it, too.
        app.add_to_named_watchlist("Crypto", "AAPL US Equity".into(), None).unwrap();
        let kept = app.undo_agent_turn("s:4").unwrap();
        assert_eq!(kept.skipped, ["you re-added those symbols"]);
        workflow(&mut app, "s:5", "delete_watchlist", json!({"watchlist":"Crypto"}));
        app.confirm_workflows("s:5", true);
        assert!(app.workflow_list_index("Crypto").is_none());
        assert_eq!(app.undo_agent_turn("s:5").unwrap().reverted, 1);
        assert!(app.workflow_list_index("Crypto").is_some());
    }

    #[test]
    fn workflow_close_creation_and_fifty_alert_cap() {
        use serde_json::json;
        let mut app = workflow_app("workflow-alert-limit");
        workflow(
            &mut app,
            "s:1",
            "create_alert",
            json!({"symbol":"AAPL US Equity","kind":"close","direction":"below","level":100,"timeframe":"4H","note":"Finished close only"}),
        );
        app.confirm_workflows("s:1", true);
        let alert = &app.price_alerts.book.alerts[0];
        assert!(alert.completed_only);
        assert!(
            matches!(&alert.rule, Some(tr_core::price_alerts::Rule::Formula { expression, timeframe: tr_core::Timeframe::H4 }) if expression == "C")
        );
        assert_eq!(alert.note.as_deref(), Some("Finished close only"));
        for n in 0..49 {
            let id = app
                .price_alerts
                .book
                .add("AAPL US Equity".into(), 200. + n as f64, tr_core::price_alerts::Direction::Above, true)
                .unwrap();
            app.price_alerts.book.alerts.iter_mut().find(|a| a.id == id).unwrap().agent_turn = Some("s:1".into());
        }
        assert!(!workflow(&mut app, "s:2", "create_alert", price()).success);
        // User configuration changes release ownership, but not the creator count.
        let original = app.price_alerts.book.alerts[0].clone();
        app.price_alerts.book.edit(original.id, 101., tr_core::price_alerts::Direction::Above).unwrap();
        assert!(app.price_alerts.book.alerts[0].agent_turn.is_none());
        assert!(!workflow(&mut app, "s:2", "create_alert", price()).success);
        app.price_alerts.book.alerts[0] = original;
        assert_eq!(app.undo_agent_turn("s:1").unwrap().reverted, 1);
        assert_eq!(app.price_alerts.book.alerts.len(), 49);
    }

    #[test]
    fn workflow_user_owned_alert_update_requires_approval_and_is_reversible() {
        use serde_json::json;
        let mut app = workflow_app("workflow-user-alert-approval");
        let id = app
            .price_alerts
            .book
            .add("AAPL US Equity".into(), 100., tr_core::price_alerts::Direction::Above, true)
            .unwrap();
        let original = app.price_alerts.book.alerts[0].clone();
        workflow(&mut app, "user:1", "update_alert", json!({"id":id,"level":120}));
        assert!(app.agent_pane.workflows.proposals[0].user_owned);
        assert_eq!(app.price_alerts.book.alerts[0], original);
        app.confirm_workflows("user:1", false);
        assert_eq!(app.price_alerts.book.alerts[0], original);
        workflow(&mut app, "user:2", "update_alert", json!({"id":id,"level":120}));
        app.confirm_workflows("user:2", true);
        assert_eq!(app.price_alerts.book.alerts[0].level, 120.);
        assert!(app.price_alerts.book.alerts[0].agent_turn.is_none(), "approval is not ownership transfer");
        assert_eq!(app.undo_agent_turn("user:2").unwrap().reverted, 1);
        assert_eq!(app.price_alerts.book.alerts[0], original);
    }

    #[test]
    fn workflow_same_target_batch_applies_all_selected_and_undoes_in_reverse() {
        use crate::agent_bridge::ProposalStatus;
        use serde_json::json;
        let mut app = workflow_app("workflow-related-batch");
        for symbol in ["AAPL US Equity", "MSFT US Equity"] {
            assert!(
                workflow(&mut app, "batch:1", "add_watchlist_symbols", json!({"watchlist":"Main","symbols":[symbol]}))
                    .success
            );
        }
        let before = app.watchlists[0].symbols.clone();
        app.confirm_workflows("batch:1", true);
        assert!(app.watchlists[0].symbols.contains(&"AAPL US Equity".into()));
        assert!(app.watchlists[0].symbols.contains(&"MSFT US Equity".into()));
        assert!(app.agent_pane.workflows.proposals.iter().all(|p| p.status == ProposalStatus::Approved));
        assert_eq!(app.undo_agent_turn("batch:1").unwrap().reverted, 2);
        assert_eq!(app.watchlists[0].symbols, before);

        workflow(&mut app, "batch:2", "create_alert", price());
        app.confirm_workflows("batch:2", true);
        let original = app.price_alerts.book.alerts[0].clone();
        workflow(&mut app, "batch:3", "update_alert", json!({"id":original.id,"level":123}));
        workflow(&mut app, "batch:3", "update_alert", json!({"id":original.id,"direction":"below"}));
        app.confirm_workflows("batch:3", true);
        assert_eq!(app.price_alerts.book.alerts[0].level, 123.);
        assert_eq!(app.price_alerts.book.alerts[0].direction, tr_core::price_alerts::Direction::Below);
        assert_eq!(app.undo_agent_turn("batch:3").unwrap().reverted, 2);
        assert_eq!(app.price_alerts.book.alerts[0], original);
    }

    #[test]
    fn workflow_watchlist_undo_keeps_user_claims_but_removes_other_agent_entries() {
        use serde_json::json;
        let mut app = workflow_app("workflow-partial-undo");
        app.create_watchlist("Empty".into(), vec![]).unwrap();
        workflow(
            &mut app,
            "partial:1",
            "add_watchlist_symbols",
            json!({
            "watchlist":"Empty","symbols":["AAPL US Equity","MSFT US Equity"]}),
        );
        app.confirm_workflows("partial:1", true);
        app.add_watchlist_symbols(vec!["AAPL".into()]); // Bulk/search/command path, including duplicate adds.
        assert!(!app.watchlists[1].agent_entries.contains_key("AAPL US Equity"));
        let report = app.undo_agent_turn("partial:1").unwrap();
        assert_eq!(report.reverted, 1);
        assert_eq!(report.skipped, ["you re-added those symbols"]);
        assert_eq!(app.watchlists[1].symbols, ["AAPL US Equity"]);

        workflow(
            &mut app,
            "partial:2",
            "remove_watchlist_symbols",
            json!({"watchlist":"Empty","symbols":["AAPL US Equity"]}),
        );
        app.confirm_workflows("partial:2", true);
        app.delete_watchlist(1).unwrap();
        app.create_watchlist("Empty".into(), vec![]).unwrap();
        let report = app.undo_agent_turn("partial:2").unwrap();
        assert_eq!(report.reverted, 0);
        assert_eq!(report.skipped, ["that watchlist was replaced"]);
        assert!(app.watchlists[1].symbols.is_empty());
    }

    #[test]
    fn workflow_user_move_claims_only_moved_entries_and_agent_only_list_can_be_undone() {
        use serde_json::json;
        let mut app = workflow_app("workflow-move-ownership");
        workflow(&mut app, "move:1", "create_watchlist", json!({"name":"Crypto"}));
        app.confirm_workflows("move:1", true);
        workflow(
            &mut app,
            "move:2",
            "add_watchlist_symbols",
            json!({"watchlist":"Crypto", "symbols":["AAPL US Equity","MSFT US Equity"]}),
        );
        app.confirm_workflows("move:2", true);
        // A later agent addition is not a user addition to this agent-created list.
        assert_eq!(app.undo_agent_turn("move:1").unwrap().reverted, 1);
        assert!(app.workflow_list_index("Crypto").is_none());

        app.create_watchlist("Moved".into(), vec![]).unwrap();
        workflow(
            &mut app,
            "move:3",
            "add_watchlist_symbols",
            json!({"watchlist":"Moved", "symbols":["AAPL US Equity","MSFT US Equity"]}),
        );
        app.confirm_workflows("move:3", true);
        app.move_watchlist_item(0, 1); // Own gap: a no-op does not claim ownership.
        assert_eq!(app.watchlists[1].agent_entries.len(), 2);
        app.move_watchlist_item(0, 2);
        assert!(!app.watchlists[1].agent_entries.contains_key("AAPL US Equity"));
        assert!(app.watchlists[1].agent_entries.contains_key("MSFT US Equity"));
        let report = app.undo_agent_turn("move:3").unwrap();
        assert_eq!(report.reverted, 1);
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(app.watchlists[1].symbols, ["AAPL US Equity"]);
    }

    #[test]
    fn workflow_stale_user_changes_fail_closed_and_last_list_is_rechecked_at_click() {
        use crate::agent_bridge::ProposalStatus;
        use serde_json::json;
        let mut app = workflow_app("workflow-stale-list");
        app.create_watchlist("Other".into(), vec![]).unwrap();
        workflow(&mut app, "stale:1", "delete_watchlist", json!({"watchlist":"Other"}));
        app.delete_watchlist(0).unwrap();
        app.confirm_workflows("stale:1", true);
        assert_eq!(app.watchlists.len(), 1);
        assert!(matches!(app.agent_pane.workflows.proposals[0].status, ProposalStatus::Failed(_)));
        for symbol in ["AAPL US Equity", "MSFT US Equity"] {
            workflow(&mut app, "stale:2", "add_watchlist_symbols", json!({"watchlist":"Other","symbols":[symbol]}));
        }
        app.add_to_named_watchlist("Other", "NVDA US Equity".into(), None).unwrap();
        app.confirm_workflows("stale:2", true);
        assert_eq!(app.watchlists[0].symbols, ["NVDA US Equity"]);
        assert!(app.agent_pane.workflows.proposals.iter().all(|p| matches!(p.status, ProposalStatus::Failed(_))));
    }

    #[test]
    fn review_list_alerts_symbol_filter_matches_equivalent_crypto_keys() {
        use serde_json::json;
        use tr_agent_tools::{Content, args::*};
        let mut app = workflow_app("workflow-review-crypto-alert-filter");
        let aliases = ["BTCUSD Curncy", "BTC-USD", "COINBASE:BTC-USD Curncy"];
        for stored in aliases.into_iter().chain(["BINANCE:BTCUSDT Curncy", "BTC US Equity"]) {
            let id = app
                .price_alerts
                .book
                .add("BTCUSD Curncy".into(), 100_000., tr_core::price_alerts::Direction::Above, true)
                .unwrap();
            // Include legacy persisted spellings, even those new-alert validation normalizes.
            app.price_alerts.book.alerts.iter_mut().find(|a| a.id == id).unwrap().symbol = stored.into();
        }
        for symbol in aliases {
            let result = app.agent_workflow("read:1", LiveArgs::ListAlerts(ListAlerts { symbol: Some(symbol.into()) }));
            let Content::Text(text) = &result.content[0] else { panic!("JSON output") };
            let value: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(value["total"], json!(3), "{symbol}: {value}");
        }
        assert!(app.agent_pane.workflows.proposals.is_empty());
    }

    #[test]
    fn removal_membership_is_validated_before_proposal_and_only_matches_are_proposed() {
        use serde_json::json;
        use tr_agent_tools::{Content, args::LiveArgs};
        let mut app = workflow_app("workflow-remove-membership");
        app.watchlists[0].symbols = vec!["BTCUSD Curncy".into(), "ZZZZ US Equity".into(), "FAIL US Equity".into()];
        let before = app.watchlists[0].symbols.clone();
        let name = app.watchlists[0].name.clone();
        let args = |symbols: serde_json::Value| {
            LiveArgs::RemoveWatchlistSymbols(
                serde_json::from_value(json!({"watchlist":name,"symbols":symbols})).unwrap(),
            )
        };
        let result = app.agent_workflow("remove:0", args(json!(["?MISSING"])));
        assert!(!result.success);
        let Content::Text(text) = &result.content[0] else { panic!("JSON output") };
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(value["symbols"][0]["error"], "not_in_list");
        assert!(app.agent_pane.workflows.proposals.is_empty());

        let result =
            app.agent_workflow("remove:1", args(json!(["BTC-USD", "ZZZZ US Equity", "FAIL US Equity", "?MISSING"])));
        assert!(result.success);
        let Content::Text(text) = &result.content[0] else { panic!("JSON output") };
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(value["symbols"][0]["symbol"], "?MISSING");
        assert_eq!(value["symbols"][0]["message"], "not in list");
        let LiveArgs::RemoveWatchlistSymbols(a) = &app.agent_pane.workflows.proposals[0].args else {
            panic!("removal")
        };
        assert_eq!(a.symbols, before);
        assert_eq!(app.watchlists[0].symbols, before, "a proposal must not mutate the list");
        app.confirm_workflows("remove:1", true);
        assert!(app.watchlists[0].symbols.is_empty());
    }

    #[test]
    fn workflow_lists_are_bounded_and_old_watchlist_provenance_defaults() {
        use serde_json::json;
        use tr_agent_tools::{Content, args::*};
        let mut app = workflow_app("workflow-read-bounds");
        for n in 0..51 {
            let id = app
                .price_alerts
                .book
                .add("AAPL US Equity".into(), 100. + n as f64, tr_core::price_alerts::Direction::Above, true)
                .unwrap();
            app.price_alerts.book.alerts.iter_mut().find(|a| a.id == id).unwrap().note =
                Some("quoted data ".repeat(180));
        }
        let result = app.agent_workflow("read:1", LiveArgs::ListAlerts(ListAlerts { symbol: None }));
        assert!(result.success);
        let Content::Text(text) = &result.content[0] else { panic!("JSON output") };
        assert!(text.len() <= tr_agent_tools::MAX_TEXT_BYTES);
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(value["total"], 51);
        assert_eq!(value["truncated"], true);
        assert!(value["alerts"].as_array().unwrap().len() <= 50);
        assert!(app.agent_pane.workflows.proposals.is_empty());
        app.watchlists = (0..60).map(|n| super::super::NamedWatchlist::new(format!("List {n}"), vec![])).collect();
        let result = app.agent_workflow("read:1", LiveArgs::ListWatchlists(ListWatchlists {}));
        let Content::Text(text) = &result.content[0] else { panic!("JSON output") };
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(value["total"], 60);
        assert_eq!(value["truncated"], true);
        assert_eq!(value["watchlists"].as_array().unwrap().len(), 50);
        let old: super::super::NamedWatchlist =
            serde_json::from_value(json!({"name":"Legacy","symbols":["AAPL US Equity"]})).unwrap();
        assert!(old.agent_turn.is_none());
        assert!(old.agent_entries.is_empty());
        assert!(old.workflow_id.is_none());
    }

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
        assert_eq!(app.agent_pane.tool_set(), tr_agent_tools::ToolSet::Workflows);
        assert!(app.agent_pane.session.is_none(), "choosing before Start must not launch a child");
        let json = serde_json::to_string(&app.current_doc()).unwrap();
        assert!(json.contains("\"tools\":\"Edit\""));
        assert!(!json.contains("Queue stays paused"));
        app.agent_pane.running = true;
        app.agent_action(&ctx, id, Action::SetTools(agent_pane::ToolsMode::ReadOnly));
        assert_eq!(app.agent_pane.tool_set(), tr_agent_tools::ToolSet::Workflows);
        app.agent_pane.running = false;
        app.agent_pane.transition(Onboarding::Stopped { reason: "tripwire".into(), unsafe_backend: true });
        app.agent_action(&ctx, id, Action::SetTools(agent_pane::ToolsMode::ReadOnly));
        app.agent_action(&ctx, id, Action::Check);
        assert!(app.agent_pane.session.is_none());
        assert_eq!(app.agent_pane.tool_set(), tr_agent_tools::ToolSet::Workflows);
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
    fn review_enter_on_focused_decline_button_must_not_approve() {
        use crate::agent_bridge::ProposalStatus;
        let setup = |n: usize| {
            let (mut app, id) = agent_app(&format!("workflow-card-decline-enter-{n}"));
            app.focus_cmd = false;
            app.agent_pane.start_prompt(agent_pane::Prompt { text: "Propose a list".into(), requires_tools: false });
            app.agent_pane.event(BridgeEvent::Started { turn: "decline:1".into() });
            app.agent_workflow(
                "decline:1",
                tr_agent_tools::args::LiveArgs::CreateWatchlist(tr_agent_tools::args::CreateWatchlist {
                    name: "Crypto".into(),
                }),
            );
            app.agent_pane.wire(
                "item/completed",
                &serde_json::json!({"item":{
                    "id":"workflow-decline", "type":"dynamicToolCall", "tool":"create_watchlist", "success":true
                }}),
            );
            app.agent_pane.event(BridgeEvent::Completed {
                turn: "decline:1".into(),
                status: "completed".into(),
                calls: 1,
                interrupt_elapsed: None,
            });
            let ctx = egui::Context::default();
            frame(&ctx, vec![], |ui| app.agent_panel(ui, id));
            frame(&ctx, vec![], |ui| app.agent_panel(ui, id));
            let mut ids: Vec<_> = app.agent_pane.proposal_focus.keys().copied().collect();
            ids.sort_by_key(|id| id.value());
            (app, ctx, ids)
        };
        let (_, _, ids) = setup(0);
        assert_eq!(ids.len(), 2, "single-row card exposes Approve and Decline");
        let mut outcomes = Vec::new();
        for n in 0..ids.len() {
            let (mut app, ctx, ids) = setup(n + 1);
            let focus = ids[n];
            frame(&ctx, vec![key(Key::Enter)], |_| {
                ctx.memory_mut(|m| m.request_focus(focus));
                app.hotkeys(&ctx);
            });
            outcomes.push(app.agent_pane.workflows.proposals[0].status.clone());
        }
        // One of the two focused buttons is Decline: Enter there must not approve.
        assert_eq!(outcomes.iter().filter(|s| **s == ProposalStatus::Approved).count(), 1, "{outcomes:?}");
        assert_eq!(outcomes.iter().filter(|s| **s == ProposalStatus::Declined).count(), 1, "{outcomes:?}");
    }

    #[test]
    fn approve_applies_only_proposals_shown_on_the_card() {
        use crate::agent_bridge::ProposalStatus;
        let (mut app, id) = agent_app("workflow-shown-only");
        for name in ["Seen", "Late"] {
            app.agent_workflow(
                "shown:1",
                tr_agent_tools::args::LiveArgs::CreateWatchlist(tr_agent_tools::args::CreateWatchlist {
                    name: name.into(),
                }),
            );
        }
        let seen = app.agent_pane.workflows.proposals[0].id;
        app.agent_action(
            &egui::Context::default(),
            id,
            agent_pane::Action::Confirm { turn: "shown:1".into(), approve: true, ids: vec![seen] },
        );
        assert_eq!(app.agent_pane.workflows.proposals[0].status, ProposalStatus::Approved);
        assert_eq!(app.agent_pane.workflows.proposals[1].status, ProposalStatus::Pending, "unseen stays pending");
        assert!(app.watchlists.iter().any(|w| w.name == "Seen"));
        assert!(!app.watchlists.iter().any(|w| w.name == "Late"));
    }

    #[test]
    fn focused_confirmation_card_enter_approves_selected_and_escape_declines_remaining() {
        use crate::agent_bridge::ProposalStatus;
        let (mut app, id) = agent_app("workflow-card-keyboard");
        app.focus_cmd = false;
        app.agent_pane.start_prompt(agent_pane::Prompt { text: "Propose two lists".into(), requires_tools: false });
        app.agent_pane.event(BridgeEvent::Started { turn: "keyboard:1".into() });
        for name in ["Crypto", "Research"] {
            app.agent_workflow(
                "keyboard:1",
                tr_agent_tools::args::LiveArgs::CreateWatchlist(tr_agent_tools::args::CreateWatchlist {
                    name: name.into(),
                }),
            );
        }
        app.agent_pane.workflows.proposals[1].selected = false;
        app.agent_pane.wire(
            "item/completed",
            &serde_json::json!({"item":{
                "id":"workflow-keyboard", "type":"dynamicToolCall", "tool":"create_watchlist", "success":true
            }}),
        );
        app.agent_pane.event(BridgeEvent::Completed {
            turn: "keyboard:1".into(),
            status: "completed".into(),
            calls: 2,
            interrupt_elapsed: None,
        });
        let ctx = egui::Context::default();
        frame(&ctx, vec![], |ui| app.agent_panel(ui, id));
        frame(&ctx, vec![], |ui| app.agent_panel(ui, id));
        let checkbox =
            *app.agent_pane.proposal_focus.keys().find(|id| !app.agent_pane.proposal_buttons.contains_key(id)).unwrap();
        frame(&ctx, vec![key(Key::Enter)], |_| {
            ctx.memory_mut(|m| m.request_focus(checkbox));
            assert!(app.agent_escape(&ctx));
        });
        assert_eq!(app.watchlists.len(), 1, "Enter on a checkbox cannot approve");
        assert!(app.agent_pane.workflows.proposals.iter().all(|p| p.status == ProposalStatus::Pending));
        for key_event in [Key::Enter, Key::Escape] {
            frame(&ctx, vec![key(key_event)], |_| {
                ctx.memory_mut(|m| m.request_focus(egui::Id::new("outside-card")));
                assert!(!app.agent_escape(&ctx));
            });
        }
        assert!(app.agent_pane.workflows.proposals.iter().all(|p| p.status == ProposalStatus::Pending));
        let focus = *app
            .agent_pane
            .proposal_buttons
            .iter()
            .find(|(_, approve)| **approve)
            .expect("inline card must expose Approve")
            .0;
        frame(&ctx, vec![key(Key::Enter)], |_| {
            ctx.memory_mut(|m| m.request_focus(focus));
            app.hotkeys(&ctx);
        });
        assert_eq!(app.watchlists.len(), 2);
        assert_eq!(app.agent_pane.workflows.proposals[0].status, ProposalStatus::Approved);
        assert_eq!(app.agent_pane.workflows.proposals[1].status, ProposalStatus::Pending);
        assert!(matches!(
            app.agent_pane.workflows.context[0],
            tr_agent::codex::WorkflowResult::Confirmation { status: tr_agent::codex::WorkflowStatus::Approved, .. }
        ));
        frame(&ctx, vec![], |ui| app.agent_panel(ui, id));
        let focus = *app.agent_pane.proposal_focus.keys().next().unwrap();
        frame(&ctx, vec![key(Key::Escape)], |_| {
            ctx.memory_mut(|m| m.request_focus(focus));
            app.hotkeys(&ctx);
        });
        assert_eq!(app.watchlists.len(), 2);
        assert_eq!(app.agent_pane.workflows.proposals[1].status, ProposalStatus::Declined);
        assert!(app.agent_pane.session.is_none());
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

fn symbol_count(n: usize) -> String {
    crate::agent_pane::count_label(n as u64, "symbol", "symbols")
}
