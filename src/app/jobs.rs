//! Background jobs: long agent work (film renders, bakes, exports) runs on
//! its own thread so the window stays live, with a progress bar in the
//! "Working on" panel.
//!
//! A job's thread does the heavy part and returns a finisher, which runs on
//! the window's thread to apply the results (it may touch the scene and
//! meshes) and make the reply.
//!
//! No agent call waits longer than `REPLY_AFTER` (agent connections give up
//! after about a minute). Every call that can be slow carries a `Ticket`:
//! a job thread, a call that is done but answers once meshes are built
//! (`defer_reply`), and a call waiting in line behind mesh builds. When the
//! time is up, the agent gets a short "still working as job N" reply, and
//! later fetches the real answer with `job_status` (which can wait for it).

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::ClideApp;
use crate::mcp::{ToolCall, ToolResult};
use crate::progress::Progress;

/// Applies a finished job's results on the window's thread and replies.
pub(super) type Finish = Box<dyn FnOnce(&mut ClideApp) -> ToolResult + Send>;

/// How long an agent call waits before getting a job id.
const REPLY_AFTER: Duration = Duration::from_secs(40);
/// The longest `job_status` wait (it is an agent call too).
pub(super) const MAX_WAIT: f64 = 45.0;
/// Finished results kept for `job_status` (the oldest are dropped).
const KEEP_FINISHED: Duration = Duration::from_secs(24 * 60 * 60);

/// An agent call that may answer late.
pub(super) struct Ticket {
    pub id: u64,
    pub label: String,
    pub started: Instant,
    /// The waiting agent call; None once it was told the job id.
    reply: Option<Sender<ToolResult>>,
}

pub(super) struct Job {
    pub ticket: Ticket,
    pub progress: Arc<Progress>,
    /// It edits the scene when done, so edits wait in line behind it.
    pub edits: bool,
    /// The call's tool, undo label and source: what its finisher changes
    /// is one history step.
    pub step: Option<(String, String, crate::ledger::Source)>,
    thread: Option<JoinHandle<Result<Finish, String>>>,
}

/// The heavy part of a job, run on its own thread.
pub(super) type Work = Box<dyn FnOnce(&Progress) -> Result<Finish, String> + Send>;

/// What a job tool does with a call: answer now, or run a job.
pub(super) enum Started {
    Now(ToolResult),
    Job(String, Work),
}

/// An answer to send once meshes are built (see `ClideApp::defer_reply`).
pub(super) type Answer = Box<dyn FnOnce(&mut ClideApp) -> ToolResult>;

#[derive(Default)]
pub(super) struct Jobs {
    pub running: Vec<Job>,
    /// Set by a tool that answers later; taken by `process_mcp`.
    pub defer: Option<Answer>,
    /// Calls that are done and wait for background mesh builds, in order.
    pub deferred: VecDeque<(Ticket, Answer)>,
    /// Set with `defer` by a tool that answers sooner when the build is
    /// slow (`edit_clay wait_s`): how long to wait, and that answer.
    pub defer_early: Option<(Duration, Answer)>,
    /// Those answers for calls in `deferred`, by ticket, with when.
    pub early: BTreeMap<u64, (Instant, Answer)>,
    /// Calls not run yet: they wait in line while meshes build.
    pub queue: VecDeque<(ToolCall, Ticket)>,
    /// `job_status` calls waiting for a job: (job, give up at, reply).
    watchers: Vec<(u64, Instant, Sender<ToolResult>)>,
    finished: BTreeMap<u64, (String, ToolResult, Instant)>,
    next: u64,
}

impl Jobs {
    pub fn ticket(&mut self, label: String, reply: Sender<ToolResult>) -> Ticket {
        self.next += 1;
        Ticket { id: self.next, label, started: Instant::now(), reply: Some(reply) }
    }

    /// Sends a ticket's answer: to the waiting call, or else to whoever
    /// waits for the job, or else keeps it for `job_status`.
    pub fn answer(&mut self, mut ticket: Ticket, result: ToolResult) {
        // Keep the result even if a receiver accepts it: an HTTP client may
        // already have timed out while the UI channel was still alive.
        self.finished.insert(ticket.id, (ticket.label.clone(), result.clone(), Instant::now()));
        if let Some(reply) = ticket.reply.take() { let _ = reply.send(result.clone()); }
        let (watchers, keep): (Vec<_>,Vec<_>) = self.watchers.drain(..).partition(|w| w.0 == ticket.id);
        self.watchers = keep;
        for (_,_,reply) in watchers { let _ = reply.send(result.clone()); }
        self.finished.retain(|_,(_,_,at)|at.elapsed()<KEEP_FINISHED);
    }

    /// A finished call's result (kept for `job_status`).
    pub fn result(&self, id: u64) -> Option<&ToolResult> {
        self.finished.get(&id).map(|(_, r, _)| r)
    }

    /// Where a call is: "queued", "running" or "awaiting_meshes"; None
    /// once finished (or unknown).
    pub fn state(&self, id: u64) -> Option<Value> {
        self.status(id).map(|s| s["state"].clone())
    }

    /// Tells calls that waited too long their job id, and `job_status`
    /// waits that ran out how far their job is.
    fn reply_late(&mut self) {
        let mut late = Vec::new();
        for job in &mut self.running {
            if let Some(m) = job.ticket.late() {
                late.push((m, json!({ "status": "running", "state":"running", "progress": progress_json(&job.progress), "next": format!("Still working. Call job_status with id {} and wait: 45 to get the result.", job.ticket.id) })));
            }
        }
        for (t, _) in &mut self.deferred {
            if let Some(m) = t.late() {
                late.push((m, json!({ "status": "done, answer pending", "next": format!("Your change is made. The answer (mesh counts, warnings) comes once meshes are built: call job_status with id {} and wait: 45, or go on working.", t.id) })));
            }
        }
        for (_, t) in &mut self.queue {
            if let Some(m) = t.late() {
                late.push((m, json!({ "status": "waiting in line (not run yet)", "state":"queued", "next": format!("Meshes are still building; this call runs after them, in order. Do not send it again. Call job_status with id {} and wait: 45 to get its answer, or cancel: true to drop it.", t.id) })));
            }
        }
        for ((reply, id, label, seconds), mut extra) in late {
            extra["job"] = json!(id);
            extra["doing"] = json!(label);
            extra["seconds"] = json!(seconds);
            let _ = reply.send(Ok(super::tools::text(extra)));
        }
        let now = Instant::now();
        let (due, keep): (Vec<_>, Vec<_>) = self.watchers.drain(..).partition(|w| now >= w.1);
        self.watchers = keep;
        for (id, _, reply) in due {
            let _ = reply.send(Ok(super::tools::text(self.status(id).unwrap_or(json!({ "job": id, "status": "unknown" })))));
        }
    }

    /// How far a pending job is; None if it is not pending.
    fn status(&self, id: u64) -> Option<Value> {
        if let Some(job) = self.running.iter().find(|j| j.ticket.id == id) {
            return Some(json!({ "job": id, "status": "running", "state":"running", "doing": job.ticket.label, "progress": progress_json(&job.progress), "seconds": job.ticket.started.elapsed().as_secs() }));
        }
        if let Some((t, _)) = self.deferred.iter().find(|(t, _)| t.id == id) {
            return Some(json!({ "job": id, "status": "done, answer pending (meshes building)", "state":"awaiting_meshes", "doing": t.label, "seconds": t.started.elapsed().as_secs() }));
        }
        let (place, (_, t)) = self.queue.iter().enumerate().find(|(_, (_, t))| t.id == id)?;
        Some(json!({ "job": id, "status": "waiting in line (not run yet)", "state":"queued", "doing": t.label, "place_in_line": place + 1, "seconds": t.started.elapsed().as_secs() }))
    }
}

impl Ticket {
    /// The reply and what to tell it, once the call has waited too long.
    fn late(&mut self) -> Option<(Sender<ToolResult>, u64, String, u64)> {
        if self.started.elapsed() < REPLY_AFTER {
            return None;
        }
        let reply = self.reply.take()?;
        Some((reply, self.id, self.label.clone(), self.started.elapsed().as_secs()))
    }
}

impl ClideApp {
    /// Starts `work` on its own thread; the ticket's call gets the result.
    pub(super) fn spawn_job(&mut self, label: String, mut ticket: Ticket, work: Work) -> u64 {
        ticket.label = label;
        let id = ticket.id;
        let progress = Arc::new(Progress::default());
        let p = progress.clone();
        let thread = std::thread::Builder::new()
            .name(format!("job {id}"))
            .spawn(move || work(&p))
            .expect("job thread starts");
        self.jobs.running.push(Job { ticket, progress, edits: false, step: None, thread: Some(thread) });
        id
    }

    /// Answers the current call later, once background mesh builds are
    /// done, with what `answer` returns then.
    pub(super) fn defer_reply(&mut self, answer: impl FnOnce(&mut ClideApp) -> ToolResult + 'static) -> ToolResult {
        self.jobs.defer = Some(Box::new(answer));
        Ok(Vec::new())
    }

    /// `defer_reply`, but if the builds take longer than `wait`, answers
    /// then with `early` instead (which must not wait for meshes).
    pub(super) fn defer_reply_within(&mut self, wait: Duration, answer: impl FnOnce(&mut ClideApp) -> ToolResult + 'static, early: impl FnOnce(&mut ClideApp) -> ToolResult + 'static) -> ToolResult {
        self.jobs.defer_early = Some((wait, Box::new(early)));
        self.defer_reply(answer)
    }

    /// Sends the deferred answers once the meshes are built, and early
    /// answers whose time is up while they build. An early answer may go
    /// out before an earlier call's: each call has its own reply channel,
    /// and over MCP its own HTTP request whose response carries its
    /// JSON-RPC id (a batch is answered in order inside one request), so
    /// replies pair with their calls whatever order they are sent in. The
    /// calls themselves still ran in order.
    pub(super) fn answer_deferred(&mut self, ctx: &egui::Context) {
        if self.meshes.busy() {
            let now = Instant::now();
            // Only to a call still waiting: past REPLY_AFTER it was told its
            // job id, and `job_status` gets the full answer once built.
            let due: Vec<u64> = self.jobs.deferred.iter().filter(|(t, _)| t.reply.is_some() && self.jobs.early.get(&t.id).is_some_and(|(at, _)| now >= *at)).map(|(t, _)| t.id).collect();
            for id in due {
                let i = self.jobs.deferred.iter().position(|(t, _)| t.id == id).expect("listed");
                let (ticket, _) = self.jobs.deferred.remove(i).expect("listed");
                let (_, early) = self.jobs.early.remove(&id).expect("listed");
                let result = early(self);
                self.jobs.answer(ticket, result);
            }
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        while let Some((ticket, answer)) = self.jobs.deferred.pop_front() {
            self.jobs.early.remove(&ticket.id);
            let result = answer(self);
            self.jobs.answer(ticket, result);
        }
    }

    /// Finishes done jobs and answers calls that waited too long. Call
    /// every frame.
    pub(super) fn poll_jobs(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.jobs.running.len() {
            if !self.jobs.running[i].thread.as_ref().is_none_or(JoinHandle::is_finished) {
                i += 1;
                continue;
            }
            let mut job = self.jobs.running.remove(i);
            self.flush_ui_step();
            let jumps = self.history_jumps;
            let result = match job.thread.take().map(JoinHandle::join) {
                Some(Ok(Ok(finish))) => finish(self),
                Some(Ok(Err(e))) => Err(e),
                Some(Err(_)) | None => Err(format!("job {} ({}) stopped unexpectedly", job.ticket.id, job.ticket.label)),
            };
            if let Some((tool, label, source)) = job.step.take() {
                self.agent_step(&tool, label, jumps, source);
            }
            self.status = format!("Done: {}", job.ticket.label);
            // A finisher may answer once meshes are built, as a tool can
            // (`defer_reply`; edit_clay's polish waits for its mesh).
            if let Some(answer) = self.jobs.defer.take() {
                let mut ticket = job.ticket;
                if let Some((wait, early)) = self.jobs.defer_early.take() {
                    self.jobs.early.insert(ticket.id, (Instant::now() + wait, early));
                }
                ticket.label = format!("{} (answer waits for meshes)", ticket.label);
                self.jobs.deferred.push_back((ticket, answer));
                continue;
            }
            self.jobs.defer_early = None;
            self.jobs.answer(job.ticket, result);
        }
        self.jobs.reply_late();
        if !self.jobs.running.is_empty() || !self.jobs.queue.is_empty() || !self.jobs.deferred.is_empty() || !self.jobs.watchers.is_empty() {
            // Keep the bars moving and the late replies on time.
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    /// `job_status`: a finished job's result, how far a pending one is
    /// (waiting up to `wait` seconds for it to finish), or a list of all.
    /// `cancel: true` stops a running job or drops a call still in line.
    /// Returns None when the answer comes later (a wait).
    pub(super) fn job_status(&mut self, args: &Value, reply: &Sender<ToolResult>) -> Option<ToolResult> {
        let Some(id) = args["id"].as_u64() else {
            let mut pending: Vec<Value> = self.jobs.running.iter().map(|j| j.ticket.id).chain(self.jobs.deferred.iter().map(|d| d.0.id)).chain(self.jobs.queue.iter().map(|q| q.1.id)).filter_map(|id| self.jobs.status(id)).collect();
            pending.sort_by_key(|v| v["job"].as_u64());
            let pending_count=pending.len();
            let offset=args["offset"].as_u64().unwrap_or(0) as usize;
            let limit=args["limit"].as_u64().unwrap_or(24) as usize;
            let pending:Vec<_>=pending.into_iter().skip(offset).take(limit).collect();
            let finished_count=self.jobs.finished.len();
            let finished: Vec<Value> = self.jobs.finished.iter().skip(offset).take(limit).map(|(id, (label, r, _))| json!({ "job": id, "did": label, "ok": r.is_ok(), "status": if r.is_ok(){"finished"}else{"failed"} })).collect();
            return Some(Ok(super::tools::text(json!({ "pending": pending, "finished": finished, "pending_count":pending_count, "finished_count":finished_count, "offset":offset, "next_offset":offset.checked_add(limit).filter(|n|*n<pending_count.max(finished_count)), "builds": self.builds_json(), "ui_activity": self.ui_activity_json(false) }))));
        };
        if args["cancel"].as_bool().unwrap_or(false) {
            return Some(self.cancel_job(id));
        }
        if let Some((_, result, _)) = self.jobs.finished.get(&id) {
            return Some(result.clone());
        }
        let Some(status) = self.jobs.status(id) else {return Some(Err(format!("unknown or expired job {id}")));};
        let wait = args["wait"].as_f64().unwrap_or(0.0).clamp(0.0, MAX_WAIT);
        if wait <= 0.0 {
            return Some(Ok(super::tools::text(status)));
        }
        // Answer when the job does, or with its status when the wait ends.
        self.jobs.watchers.push((id, Instant::now() + Duration::from_secs_f64(wait), reply.clone()));
        None
    }

    /// Mesh builds running in the background (`job_status` without id):
    /// object, what, seconds so far and the fraction done.
    pub(super) fn builds_json(&self) -> Vec<Value> {
        self.meshes.building().into_iter().map(|b| json!({ "object": b.object, "what": b.what, "seconds": (b.seconds * 10.0).round() / 10.0, "fraction": (b.fraction * 1000.0).round() / 1000.0 })).collect()
    }

    fn cancel_job(&mut self, id: u64) -> ToolResult {
        if let Some(job) = self.jobs.running.iter().find(|j| j.ticket.id == id) {
            job.progress.cancel();
            return Ok(super::tools::text(json!({ "job": id, "status": "stopping", "next": "The job stops at its next step; nothing it was making is kept." })));
        }
        if let Some(i) = self.jobs.queue.iter().position(|q| q.1.id == id) {
            let (_, ticket) = self.jobs.queue.remove(i).expect("found");
            let label = ticket.label.clone();
            self.jobs.answer(ticket, Err(format!("cancelled: {label} was not run")));
            return Ok(super::tools::text(json!({ "job": id, "status": "cancelled", "next": "The call was dropped before it ran; nothing changed." })));
        }
        if self.jobs.deferred.iter().any(|d| d.0.id == id) {
            return Err(format!("job {id} is already done (its change is made; only the answer waits for meshes). Use undo to take it back."));
        }
        Err(format!("no pending job {id}"))
    }

    /// Whether the "Working on" panel has anything to show.
    pub(super) fn working(&self) -> bool {
        !self.jobs.running.is_empty() || self.meshes.busy()
    }

    /// The "Working on" panel: one bar per running job and mesh build.
    pub(super) fn jobs_panel(&mut self, ui: &mut egui::Ui) {
        for b in self.meshes.building() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&b.what).color(super::theme::TEXT));
                let waiting = self.jobs.deferred.len() + self.jobs.queue.len();
                let (percent, seconds) = (b.fraction * 100.0, b.seconds);
                let text = if waiting > 0 { format!("{percent:.0}%  ·  {seconds:.0}s  ·  {waiting} agent call(s) waiting") } else { format!("{percent:.0}%  ·  {seconds:.0}s") };
                ui.add(egui::ProgressBar::new(b.fraction).animate(true).desired_width(ui.available_width().min(420.0)).text(text));
            });
        }
        for job in &self.jobs.running {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&job.ticket.label).color(super::theme::TEXT));
                let f = job.progress.fraction();
                let stage = job.progress.stage_name();
                let text = format!("{stage}  {:.0}%  ·  {}s", f * 100.0, job.ticket.started.elapsed().as_secs());
                ui.add(egui::ProgressBar::new(f).desired_width(ui.available_width().min(420.0)).text(text));
            });
        }
    }
}

fn progress_json(p: &Progress) -> Value {
    let mut v = json!({ "percent": (p.fraction() * 100.0).round(), "step": p.stage_name() });
    if let Some(eta) = p.eta() {
        v["seconds_left"] = json!(eta.round());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finished_jobs_survive_disconnected_receivers_and_later_jobs() {
        let mut jobs=Jobs::default();
        let (tx,rx)=std::sync::mpsc::channel();
        let ticket=jobs.ticket("material".into(),tx);
        let id=ticket.id;drop(rx);
        let (tx,rx)=std::sync::mpsc::channel();drop(rx);
        jobs.watchers.push((id,Instant::now(),tx));
        jobs.answer(ticket,Ok(super::super::tools::text(json!({"done":true}))));
        for i in 0..100 {
            let (tx,_)=std::sync::mpsc::channel();let t=jobs.ticket(format!("other {i}"),tx);
            jobs.answer(t,Err("cancelled".into()));
        }
        assert!(jobs.finished.contains_key(&id));
        assert!(jobs.finished[&id].1.is_ok());
        assert_eq!(jobs.finished.len(),101);
    }
    #[test]
    fn all_simultaneous_job_watchers_receive_the_answer_and_can_read_again() {
        let mut jobs=Jobs::default();let (tx,_)=std::sync::mpsc::channel();let mut ticket=jobs.ticket("bake".into(),tx);ticket.reply=None;
        let id=ticket.id;
        let mut receivers=Vec::new();
        for _ in 0..3 {let (tx,rx)=std::sync::mpsc::channel();jobs.watchers.push((id,Instant::now()+Duration::from_secs(20),tx));receivers.push(rx);}
        jobs.answer(ticket,Ok(super::super::tools::text(json!({"done":true}))));
        for rx in receivers {assert!(rx.try_recv().unwrap().is_ok());}
        assert!(jobs.finished[&id].1.is_ok());assert!(jobs.watchers.is_empty());
    }
}
