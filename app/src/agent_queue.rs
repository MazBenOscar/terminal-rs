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

type WaitCheck = dyn FnMut(&mut TerminalApp, bool) -> Option<AgentReply> + Send;

pub(crate) struct AgentWaitJob {
    pub id: String,
    pub turn: String,
    pub deadline: Instant,
    /// `expired` permits a structured timeout with the current switched view.
    pub check: Box<WaitCheck>,
    pub reply: tokio::sync::oneshot::Sender<AgentReply>,
}

#[derive(Clone)]
pub struct AgentToolQueue {
    jobs: Arc<Mutex<VecDeque<AgentJob>>>,
    waiting: Arc<Mutex<VecDeque<AgentWaitJob>>>,
    ctx: egui::Context,
}

impl AgentToolQueue {
    pub fn new(ctx: egui::Context) -> Self {
        Self { jobs: Default::default(), waiting: Default::default(), ctx }
    }

    pub fn push(&self, job: AgentJob) {
        self.jobs.lock().unwrap().push_back(job);
        self.ctx.request_repaint();
    }

    #[cfg(test)]
    pub(crate) fn expire_waits_for_test(&self) {
        for job in self.waiting.lock().unwrap().iter_mut() {
            job.deadline = Instant::now();
        }
    }

    pub(crate) fn push_wait(&self, job: AgentWaitJob) {
        // Data wakes through the existing worker response path, including hidden
        // window logic. The executor's oneshot timeout enforces the absolute
        // deadline even without a tick; do not leave an idle repaint timer behind.
        self.waiting.lock().unwrap().push_back(job);
        self.ctx.request_repaint();
    }

    pub(crate) fn check_waiting(&self, app: &mut TerminalApp, started: Instant, remaining: usize) {
        let jobs = std::mem::take(&mut *self.waiting.lock().unwrap());
        let mut kept = VecDeque::new();
        let mut checked = 0;
        for mut job in jobs {
            if job.reply.is_closed() {
                continue;
            }
            let expired = job.deadline <= Instant::now();
            if !expired && (checked >= remaining || started.elapsed() >= MAX_JOB_TIME_PER_FRAME) {
                // Work was deferred by this tick's drain budget, not by data.
                self.ctx.request_repaint();
                kept.push_back(job);
                continue;
            }
            checked += 1;
            if let Some(reply) = (job.check)(app, expired) {
                let _ = job.reply.send(reply);
            } else {
                kept.push_back(job);
            }
        }
        self.waiting.lock().unwrap().extend(kept);
    }

    pub fn cancel_job(&self, id: &str) {
        self.waiting.lock().unwrap().retain(|job| job.id != id);
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(index) = jobs.iter().position(|job| job.id == id) {
            let job = jobs.remove(index).unwrap();
            let _ = job.reply.send(AgentReply::error("cancelled"));
        }
    }

    pub fn cancel(&self, turn: &str) {
        self.waiting.lock().unwrap().retain(|job| job.turn != turn);
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
