//! Progress of background work, shared between the worker thread (which
//! updates it) and the window (which draws it as a bar).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

pub struct Progress {
    /// 0..=1_000_000 for 0..=1.
    done: AtomicU32,
    stage: Mutex<String>,
    /// The part of the whole that `set` fills (see `section`), in millionths.
    base: AtomicU32,
    span: AtomicU32,
    /// Set by `cancel`; long work checks `cancelled` and stops.
    cancel: AtomicBool,
    started: Instant,
    /// Every value the bar took, in order, and where each `range` call
    /// came (tests read the sequence instead of sampling it in time).
    #[cfg(test)]
    trace: Mutex<Vec<Traced>>,
}

/// One step of `Progress::trace`.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Traced {
    /// The bar after a `set` or `at_least`.
    Value(f32),
    /// A `range(from, to)` call (its `set(0.0)` follows as a value).
    Range(f32, f32),
}

impl Default for Progress {
    fn default() -> Self {
        Progress { done: AtomicU32::new(0), stage: Mutex::new(String::new()), base: AtomicU32::new(0), span: AtomicU32::new(1_000_000), cancel: AtomicBool::new(false), started: Instant::now(), #[cfg(test)] trace: Mutex::new(Vec::new()) }
    }
}

impl Progress {
    /// Sets how far along the work (or the current section) is, 0..1.
    pub fn set(&self, fraction: f32) {
        #[cfg(test)]
        let mut trace = self.trace.lock().expect("no panics while locked");
        let (base, span) = (self.base.load(Ordering::Relaxed) as f32 / 1e6, self.span.load(Ordering::Relaxed) as f32 / 1e6);
        self.done.store(((base + span * fraction.clamp(0.0, 1.0)) * 1e6) as u32, Ordering::Relaxed);
        #[cfg(test)]
        trace.push(Traced::Value(self.fraction()));
    }

    /// `set`, but never backwards: for parts of the work that finish out
    /// of order on several threads.
    pub fn at_least(&self, fraction: f32) {
        #[cfg(test)]
        let mut trace = self.trace.lock().expect("no panics while locked");
        let (base, span) = (self.base.load(Ordering::Relaxed) as f32 / 1e6, self.span.load(Ordering::Relaxed) as f32 / 1e6);
        self.done.fetch_max(((base + span * fraction.clamp(0.0, 1.0)) * 1e6) as u32, Ordering::Relaxed);
        #[cfg(test)]
        trace.push(Traced::Value(self.fraction()));
    }

    /// From now on `set` fills `from..to` of the whole (a step that takes
    /// that share of the time).
    pub fn range(&self, from: f32, to: f32) {
        let (from, to) = (from.clamp(0.0, 1.0), to.clamp(0.0, 1.0));
        #[cfg(test)]
        self.trace.lock().expect("no panics while locked").push(Traced::Range(from, to));
        self.base.store((from * 1e6) as u32, Ordering::Relaxed);
        self.span.store(((to - from).max(0.0) * 1e6) as u32, Ordering::Relaxed);
        self.set(0.0);
    }

    /// Asks the work to stop.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Err("cancelled") once `cancel` was called.
    pub fn check(&self) -> Result<(), String> {
        if self.cancelled() { Err("cancelled".into()) } else { Ok(()) }
    }

    /// Seconds left, guessed from the speed so far (None early on).
    pub fn eta(&self) -> Option<f32> {
        let f = self.fraction();
        (0.03..1.0).contains(&f).then(|| self.started.elapsed().as_secs_f32() * (1.0 - f) / f)
    }

    /// Names the current step ("making textures") and where it starts.
    pub fn stage(&self, stage: &str, fraction: f32) {
        if let Ok(mut s) = self.stage.lock() {
            *s = stage.to_string();
        }
        self.set(fraction);
    }

    pub fn fraction(&self) -> f32 {
        self.done.load(Ordering::Relaxed) as f32 / 1e6
    }

    /// Every value the bar took and every `range` call, in order.
    #[cfg(test)]
    pub fn trace(&self) -> Vec<Traced> {
        self.trace.lock().expect("no panics while locked").clone()
    }

    pub fn stage_name(&self) -> String {
        self.stage.lock().map(|s| s.clone()).unwrap_or_default()
    }
}
