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
}

impl Default for Progress {
    fn default() -> Self {
        Progress { done: AtomicU32::new(0), stage: Mutex::new(String::new()), base: AtomicU32::new(0), span: AtomicU32::new(1_000_000), cancel: AtomicBool::new(false), started: Instant::now() }
    }
}

impl Progress {
    /// Sets how far along the work (or the current section) is, 0..1.
    pub fn set(&self, fraction: f32) {
        let (base, span) = (self.base.load(Ordering::Relaxed) as f32 / 1e6, self.span.load(Ordering::Relaxed) as f32 / 1e6);
        self.done.store(((base + span * fraction.clamp(0.0, 1.0)) * 1e6) as u32, Ordering::Relaxed);
    }

    /// From now on `set` fills `from..to` of the whole (a step that takes
    /// that share of the time).
    pub fn range(&self, from: f32, to: f32) {
        let (from, to) = (from.clamp(0.0, 1.0), to.clamp(0.0, 1.0));
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

    pub fn stage_name(&self) -> String {
        self.stage.lock().map(|s| s.clone()).unwrap_or_default()
    }
}
