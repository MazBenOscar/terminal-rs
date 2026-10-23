//! UI-thread jobs submitted by background clients.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// Yield to rendering/input after eight jobs or four milliseconds, whichever comes
// first. A running job is not preempted; expiry scans are cheap and unbudgeted.
pub(crate) const MAX_JOBS_PER_FRAME: usize = 8;
pub(crate) const MAX_JOB_TIME_PER_FRAME: Duration = Duration::from_millis(4);

use crate::app::TerminalApp;

#[derive(Debug, Clone, Copy)]
pub enum ChartTarget {
    Pinned,
    Uid(u64),
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentReply {
    pub success: bool,
    pub value: serde_json::Value,
}

impl AgentReply {
    pub fn error(error: &str) -> Self {
        Self { success: false, value: serde_json::json!({ "error": error }) }
    }
}

pub struct AgentJob {
    pub id: String,
    pub turn: String,
    pub chart: ChartTarget,
    pub deadline: Instant,
    pub run: Box<dyn FnOnce(&mut TerminalApp) -> AgentReply + Send>,
    pub reply: tokio::sync::oneshot::Sender<AgentReply>,
}

#[derive(Clone)]
pub struct AgentToolQueue {
    jobs: Arc<Mutex<VecDeque<AgentJob>>>,
    ctx: egui::Context,
}

impl AgentToolQueue {
    pub fn new(ctx: egui::Context) -> Self {
        Self { jobs: Default::default(), ctx }
    }

    pub fn push(&self, job: AgentJob) {
        self.jobs.lock().unwrap().push_back(job);
        self.ctx.request_repaint();
    }

    pub fn cancel(&self, turn: &str) {
        let cancelled = {
            let mut jobs = self.jobs.lock().unwrap();
            let mut kept = VecDeque::new();
            let mut cancelled = Vec::new();
            while let Some(job) = jobs.pop_front() {
                if job.turn == turn {
                    cancelled.push(job);
                } else {
                    kept.push_back(job);
                }
            }
            *jobs = kept;
            cancelled
        };
        for job in cancelled {
            let _ = job.reply.send(AgentReply::error("cancelled"));
        }
    }

    /// Expire the entire backlog, including jobs beyond this frame's work budget.
    pub(crate) fn expire(&self) {
        let now = Instant::now();
        let expired = {
            let mut jobs = self.jobs.lock().unwrap();
            let mut expired = Vec::new();
            let mut kept = VecDeque::new();
            while let Some(job) = jobs.pop_front() {
                if job.deadline <= now {
                    expired.push(job);
                } else {
                    kept.push_back(job);
                }
            }
            *jobs = kept;
            expired
        };
        for job in expired {
            let _ = job.reply.send(AgentReply::error("timeout"));
        }
    }

    pub(crate) fn repaint_pending(&self) {
        if self.len() > 0 {
            self.ctx.request_repaint();
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.jobs.lock().unwrap().len()
    }

    pub(crate) fn pop(&self) -> Option<AgentJob> {
        self.jobs.lock().unwrap().pop_front()
    }
}
