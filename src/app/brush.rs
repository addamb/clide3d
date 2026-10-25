//! Stroke capture for the brush tools (Stage 7 of docs/SCULPT_PLAN.md,
//! §4.3): LazyMouse, pen pressure, and the live preview worker.
//!
//! The pointer becomes samples in the press's view (`Trail`): LazyMouse
//! trails the brush behind the mouse on a string, so the stored samples
//! are the smoothed path and a replay never smooths again (the request
//! leaves `lazy` out). Pressure comes from a touch's force (matched by
//! its TouchId), the macOS tablet monitor (`pen_macos`, opt-in), or, when
//! asked for, the pointer's speed; the request names its source.
//!
//! While the button is down a worker thread resolves the stroke so far
//! exactly as its request will resolve, folds it into a scratch copy of
//! the clay (`sculpt::preview`), and hands the copy to the viewport,
//! newest first (`Live`). Nothing in the scene changes until the request,
//! sent on release through the same handler as an agent's, installs.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use egui::{Event, TouchId, TouchPhase};
use serde::{Deserialize, Serialize};

use crate::clay::Clay;
use crate::ledger::{Hash128, PressureSource};

/// LazyMouse, in points of the view (the recorder scales by the pixels
/// per point).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lazy {
    /// The string: the brush trails the mouse by up to this much.
    pub radius: f32,
    /// Least distance between recorded samples.
    #[serde(default = "default_step")]
    pub step: f32,
    /// LazySnap: on release the stroke runs on to where the mouse is.
    #[serde(default)]
    pub snap: bool,
    /// Backtrack: bringing the mouse back over the stroke takes the stroke
    /// back to there.
    #[serde(default)]
    pub backtrack: bool,
}

fn default_step() -> f32 {
    3.0
}

impl Lazy {
    pub fn validate(&self) -> Result<(), String> {
        if !(0.0..=200.0).contains(&self.radius) || !(1.0..=50.0).contains(&self.step) {
            return Err("lazy radius must be 0 to 200 and step 1 to 50 (points)".into());
        }
        Ok(())
    }
}

/// A recorded sample: a pixel of the press's view and the pen pressure.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub px: [f32; 2],
    pub pressure: Option<f32>,
}

impl Sample {
    /// As a screen frame's `px` entry: [x, y] or [x, y, pressure].
    pub fn json(&self) -> Vec<f32> {
        match self.pressure {
            Some(p) => vec![self.px[0], self.px[1], p],
            None => vec![self.px[0], self.px[1]],
        }
    }
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// The brush's path as the mouse moves: LazyMouse's string, the sample
/// spacing, Backtrack and LazySnap. Lengths are pixels.
#[derive(Clone, Debug, Default)]
pub struct Trail {
    /// Where the brush is (the string's end).
    brush: Option<[f32; 2]>,
    pub samples: Vec<Sample>,
}

impl Trail {
    /// The mouse is at `cursor`: the brush follows it on a string of
    /// `radius`, and is recorded once it moved `step` from the last sample.
    pub fn push(&mut self, cursor: [f32; 2], pressure: Option<f32>, radius: f32, step: f32, backtrack: bool) {
        let Some(b) = self.brush.as_mut() else {
            self.brush = Some(cursor);
            self.samples.push(Sample { px: cursor, pressure });
            return;
        };
        // Backtrack: the mouse back on an earlier part of the stroke (not
        // the part it just drew) takes the stroke back to there.
        if backtrack && radius > 0.0 && self.samples.len() > 3 {
            let n = self.samples.len();
            let reach = radius.max(step);
            if let Some(k) = (0..n - 3).find(|&k| dist(self.samples[k].px, cursor) <= reach * 0.5) {
                self.samples.truncate(k + 1);
                *b = self.samples[k].px;
                return;
            }
        }
        let d = dist(cursor, *b);
        if d > radius {
            let t = (d - radius) / d;
            *b = [b[0] + (cursor[0] - b[0]) * t, b[1] + (cursor[1] - b[1]) * t];
        }
        let at = *b;
        if self.samples.last().is_none_or(|s| dist(s.px, at) >= step) {
            self.samples.push(Sample { px: at, pressure });
        }
    }

    /// The button went up at `cursor`: the brush's last place is
    /// recorded, and with LazySnap the stroke runs on to the mouse.
    pub fn finish(&mut self, cursor: [f32; 2], pressure: Option<f32>, snap: bool) {
        let Some(b) = self.brush else { return };
        if self.samples.last().is_some_and(|s| s.px != b) {
            self.samples.push(Sample { px: b, pressure });
        }
        if snap && self.samples.last().is_some_and(|s| s.px != cursor) {
            self.samples.push(Sample { px: cursor, pressure });
            self.brush = Some(cursor);
        }
    }
}

/// The samples a mouse path records with `lazy` (None: plain spacing of
/// `step`), for checks.
#[cfg(test)]
pub fn lazy_path(path: &[[f32; 2]], lazy: Option<&Lazy>, step: f32) -> Vec<[f32; 2]> {
    let mut t = Trail::default();
    let (radius, step, backtrack) = lazy.map_or((0.0, step, false), |l| (l.radius, l.step.max(step), l.backtrack));
    for p in path {
        t.push(*p, None, radius, step, backtrack);
    }
    if let Some(last) = path.last() {
        t.finish(*last, None, lazy.is_some_and(|l| l.snap));
    }
    t.samples.iter().map(|s| s.px).collect()
}

/// Pen pressure during a stroke: a touch's force, matched by its id; the
/// macOS tablet monitor; or the pointer's speed, when asked for.
#[derive(Default)]
pub struct Pen {
    /// The touch the stroke follows, and its latest force.
    touch: Option<(TouchId, Option<f32>)>,
    /// The last sample (pixel, seconds), for the speed.
    last: Option<([f32; 2], f64)>,
    speed: f32,
}

/// Pointer speed (pixels per second) at which velocity pressure bottoms out.
const FAST: f32 = 2500.0;

impl Pen {
    /// Reads this frame's touch events.
    pub fn read(&mut self, events: &[Event]) {
        for e in events {
            if let Event::Touch { id, phase, force, .. } = e {
                match phase {
                    // The first touch seen (a pen already down at the press
                    // shows up moving) is the one the stroke follows.
                    TouchPhase::Start | TouchPhase::Move if self.touch.is_none_or(|t| t.0 == *id) => self.touch = Some((*id, *force)),
                    TouchPhase::End | TouchPhase::Cancel if self.touch.is_some_and(|t| t.0 == *id) => {
                        // The last force stays for the release's sample.
                    }
                    _ => {}
                }
            }
        }
    }

    /// The pressure of a sample at `at` (pixels) at `time` (seconds) and
    /// where it came from.
    pub fn pressure(&mut self, at: [f32; 2], time: f64, velocity: bool) -> (Option<f32>, Option<PressureSource>) {
        if let Some(prev) = self.last.replace((at, time)) {
            let dt = (time - prev.1) as f32;
            if dt > 1e-4 {
                // Smoothed, so one quick frame does not thin the stroke.
                self.speed = self.speed * 0.6 + dist(at, prev.0) / dt * 0.4;
            }
        }
        if let Some((_, Some(f))) = self.touch {
            return (Some(f.clamp(0.0, 1.0)), Some(PressureSource::TouchForce));
        }
        if let Some(p) = super::pen_macos::pressure() {
            return (Some(p.clamp(0.0, 1.0)), Some(PressureSource::Nsevent));
        }
        if velocity {
            return (Some((1.0 - self.speed / FAST).clamp(0.1, 1.0)), Some(PressureSource::Velocity));
        }
        (None, None)
    }
}

/// What the preview worker gets: the samples so far (screen strokes), or
/// a whole stroke built on the window's thread (grab, snakehook).
pub enum Input {
    Samples(Vec<Sample>),
    Stroke(serde_json::Value),
}

/// The worker's newest result.
pub struct Shown {
    /// Which input it shows.
    pub seq: u64,
    /// The clay as the stroke so far leaves it, and the hash of the grid
    /// the stroke writes (what its record's `result_hash` will be).
    pub clay: Result<(Clay, Hash128), String>,
    /// How long the worker took (ms).
    pub ms: f64,
}

/// A stroke's preview work: input (and whether it is the stroke as it
/// ended) to the clay it gives and the hash of the grid it writes (only
/// for the stroke as it ended; zero before).
pub type Work = Box<dyn FnMut(&Input, bool) -> Result<(Clay, Hash128), String> + Send>;

/// An input for the worker: its number, the stroke so far, and whether it
/// is the stroke as it ended.
type Job = (u64, Input, bool);

#[derive(Default)]
struct Shared {
    /// The newest input not yet taken, and whether to stop.
    input: Mutex<(Option<Job>, bool)>,
    ready: Condvar,
    output: Mutex<Option<Shown>>,
}

/// A stroke's live preview: a worker thread that always takes the newest
/// samples (older ones still waiting are skipped).
pub struct Live {
    pub target: u64,
    /// The tool its request goes to and the layer it writes (where its
    /// record is checked against it).
    pub tool: &'static str,
    pub layer: String,
    /// `ClideApp::scene_generation` at the press.
    pub generation: u64,
    /// The newest input sent.
    pub seq: u64,
    /// The input the stroke ended with (set on release).
    pub last: Option<u64>,
    /// The job of the request it previews, once sent.
    pub job: Option<u64>,
    /// When each input was sent, for the latency of what is drawn.
    pub sent: Vec<(u64, Instant)>,
    /// The newest result taken from the worker.
    pub shown: Option<Shown>,
    /// Set by the work while what it shows is not exactly what the stroke
    /// so far commits (`sculpt::preview`'s per-copy approximation, or a
    /// preview over another stroke's that had not settled): such a
    /// preview is not checked against its record, and goes once the
    /// record is in. See `inexact()`, which also counts `mesh_moved`.
    pub inexact: Arc<std::sync::atomic::AtomicBool>,
    /// The regular mesh the worker casts screen samples on (None: the
    /// stroke does not cast, as Grab and Snakehook).
    pub cast_on: Option<Arc<crate::mesh::Mesh>>,
    /// Set when the request is sent if the mesh its handler casts on is not
    /// `cast_on` (the clay changed since the press, say the stroke before
    /// it installed, and its mesh was rebuilt): the preview's samples
    /// landed elsewhere than the record's, so it is inexact.
    pub mesh_moved: bool,
    /// Dabs the sculpt preview ran over the drag (`SculptPreview::dabs_run`).
    pub dabs_run: Arc<std::sync::atomic::AtomicUsize>,
    shared: Arc<Shared>,
}

impl Live {
    /// Starts the worker; it asks `ctx` to draw again after each result.
    pub fn start(target: u64, tool: &'static str, layer: String, generation: u64, mut work: Work, ctx: egui::Context) -> Live {
        let shared = Arc::new(Shared::default());
        let s = shared.clone();
        std::thread::Builder::new()
            .name("stroke preview".into())
            .spawn(move || {
                loop {
                    let (seq, input, last) = {
                        let mut slot = s.input.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if slot.1 {
                                return;
                            }
                            if let Some(job) = slot.0.take() {
                                break job;
                            }
                            slot = s.ready.wait(slot).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    let t0 = Instant::now();
                    // A panic in the work is a failed preview, not a lost
                    // worker: the stroke still settles and goes.
                    let clay = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&input, last))).unwrap_or_else(|_| Err("the preview failed".into()));
                    *s.output.lock().unwrap_or_else(|e| e.into_inner()) = Some(Shown { seq, clay, ms: t0.elapsed().as_secs_f64() * 1000.0 });
                    ctx.request_repaint();
                }
            })
            .expect("preview thread starts");
        Live { target, tool, layer, generation, seq: 0, last: None, job: None, sent: Vec::new(), shown: None, inexact: Default::default(), cast_on: None, mesh_moved: false, dabs_run: Default::default(), shared }
    }

    /// Sends the stroke so far as input `seq` (numbered across strokes; it
    /// replaces any input still waiting); `last` when the stroke ended.
    pub fn send(&mut self, seq: u64, input: Input, last: bool) {
        self.seq = seq;
        self.sent.push((seq, Instant::now()));
        if last {
            self.last = Some(seq);
        }
        self.shared.input.lock().unwrap_or_else(|e| e.into_inner()).0 = Some((seq, input, last));
        self.shared.ready.notify_one();
    }

    /// Takes the worker's newest result; true if there was one.
    pub fn poll(&mut self) -> bool {
        let out = self.shared.output.lock().unwrap_or_else(|e| e.into_inner()).take();
        let new = out.is_some();
        if out.is_some() {
            self.shown = out;
        }
        new
    }

    /// Whether the worker has shown the stroke as it ended.
    pub fn settled(&self) -> bool {
        self.last.is_some_and(|l| self.shown.as_ref().is_some_and(|s| s.seq == l))
    }

    /// Whether what it shows may differ from what its request commits:
    /// the work said so, or the request casts on another mesh.
    pub fn inexact(&self) -> bool {
        self.mesh_moved || self.inexact.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.shared.input.lock().unwrap_or_else(|e| e.into_inner()).1 = true;
        self.shared.ready.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A seeded normal deviate (Box-Muller over a small LCG).
    fn gauss(state: &mut u64) -> f32 {
        let mut u = || {
            *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((*state >> 33) as f32 + 0.5) / (1u64 << 31) as f32
        };
        let (a, b) = (u(), u());
        (-2.0 * a.ln()).sqrt() * (std::f32::consts::TAU * b).cos()
    }

    /// Turning angle over segment length at each inner sample.
    fn curvatures(p: &[[f32; 2]]) -> Vec<f32> {
        p.windows(3)
            .filter_map(|w| {
                let (a, b) = ([w[1][0] - w[0][0], w[1][1] - w[0][1]], [w[2][0] - w[1][0], w[2][1] - w[1][1]]);
                let (la, lb) = (a[0].hypot(a[1]), b[0].hypot(b[1]));
                (la > 1e-6 && lb > 1e-6).then(|| (a[0] * b[1] - a[1] * b[0]).atan2(a[0] * b[0] + a[1] * b[1]) / (0.5 * (la + lb)))
            })
            .collect()
    }

    fn variance(v: &[f32]) -> f32 {
        let m = v.iter().sum::<f32>() / v.len() as f32;
        v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / v.len() as f32
    }

    /// LazyMouse on a jittered path (sigma 3 px about an arc): the
    /// curvature varies at least 60% less than the raw samples', and the
    /// stroke ends within the string of the mouse.
    #[test]
    fn lazy_mouse_smooths_a_jittered_path_and_ends_near_the_mouse() {
        let mut seed = 7u64;
        let path: Vec<[f32; 2]> = (0..300)
            .map(|i| {
                let a = i as f32 * 0.004;
                [400.0 + 300.0 * a.cos() + 3.0 * gauss(&mut seed), 300.0 + 300.0 * a.sin() + 3.0 * gauss(&mut seed)]
            })
            .collect();
        let raw = lazy_path(&path, None, 3.0);
        let lazy = Lazy { radius: 20.0, step: 3.0, snap: false, backtrack: false };
        let smooth = lazy_path(&path, Some(&lazy), 3.0);
        let (vr, vs) = (variance(&curvatures(&raw)), variance(&curvatures(&smooth)));
        eprintln!("curvature variance: raw {vr:.5}, lazy {vs:.5} ({:.0}% less); {} and {} samples", 100.0 * (1.0 - vs / vr), raw.len(), smooth.len());
        assert!(vs <= 0.4 * vr, "lazy {vs} against raw {vr}");
        let end = *path.last().unwrap();
        let err = dist(*smooth.last().unwrap(), end);
        assert!(err <= lazy.radius + 1e-3, "the stroke ends {err} px from the mouse (string {})", lazy.radius);
        // LazySnap runs the stroke on to the mouse.
        let snapped = lazy_path(&path, Some(&Lazy { snap: true, ..lazy }), 3.0);
        assert_eq!(*snapped.last().unwrap(), end);
        // Samples are at least a step apart.
        assert!(smooth.windows(2).take(smooth.len() - 2).all(|w| dist(w[0], w[1]) >= 3.0 - 1e-3));
    }

    /// Backtrack: the mouse brought back over the stroke takes it back.
    #[test]
    fn backtrack_takes_the_stroke_back_along_itself() {
        let out: Vec<[f32; 2]> = (0..60).map(|i| [i as f32 * 2.0, 0.0]).collect();
        let back: Vec<[f32; 2]> = (0..30).map(|i| [118.0 - i as f32 * 2.0, 0.0]).collect();
        let path: Vec<[f32; 2]> = out.iter().chain(&back).copied().collect();
        let lazy = Lazy { radius: 6.0, step: 3.0, snap: false, backtrack: true };
        let s = lazy_path(&path, Some(&lazy), 3.0);
        let far = s.iter().map(|p| p[0]).fold(0.0, f32::max);
        assert!(s.last().unwrap()[0] < 80.0 && far < 80.0, "the stroke went back: ends at {:?}, reaches {far}", s.last());
        let plain = lazy_path(&path, Some(&Lazy { backtrack: false, ..lazy }), 3.0);
        assert!(plain.iter().map(|p| p[0]).fold(0.0, f32::max) > 100.0);
    }

    /// A touch's force is the pressure, matched by its id; other touches
    /// do not change it; without a touch, speed gives it when asked.
    #[test]
    fn touch_force_is_matched_by_id() {
        let touch = |id: u64, phase, force| Event::Touch { device_id: egui::TouchDeviceId(1), id: TouchId(id), phase, pos: egui::pos2(0.0, 0.0), force: Some(force) };
        let mut pen = Pen::default();
        pen.read(&[touch(4, TouchPhase::Start, 0.25)]);
        assert_eq!(pen.pressure([0.0, 0.0], 0.0, false), (Some(0.25), Some(PressureSource::TouchForce)));
        pen.read(&[touch(9, TouchPhase::Start, 0.9), touch(9, TouchPhase::Move, 0.8), touch(4, TouchPhase::End, 0.25)]);
        assert_eq!(pen.pressure([1.0, 0.0], 0.01, false).0, Some(0.25), "another finger");
        pen.read(&[touch(4, TouchPhase::Move, 0.5)]);
        assert_eq!(pen.pressure([2.0, 0.0], 0.02, false).0, Some(0.5));
        let mut mouse = Pen::default();
        assert_eq!(mouse.pressure([0.0, 0.0], 0.0, false), (None, None));
        let (slow, _) = mouse.pressure([1.0, 0.0], 0.1, true);
        let mut fast = Pen::default();
        fast.pressure([0.0, 0.0], 0.0, true);
        let (quick, src) = fast.pressure([200.0, 0.0], 0.02, true);
        assert_eq!(src, Some(PressureSource::Velocity));
        assert!(quick < slow, "faster strokes press less: {quick:?} {slow:?}");
    }
}
