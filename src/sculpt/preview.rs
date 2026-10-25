//! The live preview of a stroke being drawn (Stage 7 of
//! docs/SCULPT_PLAN.md): the editor resolves the stroke so far, the same
//! way its request will resolve when it is sent, and this folds it into
//! a scratch copy of the grid the request will commit to. Nothing here
//! touches the scene: the editor draws the copy in place of the clay
//! until the request's record installs.
//!
//! A sculpt stroke runs only the dabs it has not run yet: the engine
//! applies dabs one after another, each from the grid before it, so a
//! stroke run in pieces gives the grid it gives run at once, bit for bit
//! (`ledger::SculptInput::run`). Dabs near the end of the path can still
//! move as it grows (the last resampled point, the direction of the dab
//! before it): `hold` of them run on a copy each time. So the grid shown
//! is the one the stroke so far commits, and the last one is the
//! committed grid.
//!
//! When a run dab changes after all (symmetric copies come one copy after
//! another, so each new sample lands in the middle of the list; taper
//! rescales dabs already run; LazyMouse's Backtrack takes the path back;
//! a stroke's own `lazy` moves only the end, which is held), running the
//! stroke
//! again from the start on every update would cost the whole stroke per
//! sample. Instead the preview goes on from its grid with each copy's new
//! dabs (copy after copy, as the commit orders them within an update):
//! close to, but not bit for bit, what the stroke so far commits, the
//! stroke as it ended too (`exact` says so; the editor shows it until the
//! record is in and does not check it against the record: running the
//! whole stroke again would only race the commit for the cores; its drift
//! is bounded by `an_inexact_preview_stays_close_to_the_commit`). Should
//! the dabs not split evenly into the copies, it reruns whole instead. A
//! path
//! brush sweeps the whole path as one dab per copy; it runs whole each
//! time (one dab, as long as the path).
//!
//! Paint, mask and group strokes are cheap per dab; they run whole each
//! time (`Grid::apply`, as their commit does).

use crate::brush::resolve::{Dab, Resolved};
use crate::clay::{Clay, Detail};
use crate::layers::{self, Layered};
use crate::ledger::{self, Grid, SculptInput};

/// A sculpt stroke's preview on one layer of one clay.
pub struct SculptPreview {
    /// The clay at the press (settled, with the stroke's alphas).
    clay: Clay,
    layer: String,
    /// Set by the first update: the layer's view with the stroke's mask
    /// and groups frozen, its grid, and the engine's input.
    state: Option<State>,
}

struct State {
    view: Clay,
    own: Option<Detail>,
    input: SculptInput,
    /// What the stroke reads besides its dabs: when it changes, it runs
    /// again from the start.
    key: Resolved,
    /// The dabs run so far, from the input, giving `grid`.
    run: Vec<Dab>,
    grid: Option<Detail>,
    /// Dabs run in all (every update's), for checks.
    runs: usize,
    /// Set once a run dab changed in a dab brush's stroke: how many of
    /// each symmetric copy's dabs `grid` holds (it is then close to, not
    /// exactly, the stroke so far; `run` is unused).
    approx: Option<Vec<usize>>,
    /// Times the approximation was left for a whole rerun (the dabs did
    /// not split into the copies), for checks.
    fallbacks: usize,
}

impl SculptPreview {
    /// A preview of strokes on `layer` ("base" or a sculpt layer) of
    /// `clay` as it is at the press (settled: see `ledger::settle_clay`).
    pub fn new(clay: Clay, layer: &str) -> SculptPreview {
        SculptPreview { clay, layer: layer.to_string(), state: None }
    }

    /// The layer's grid after `stroke` (the stroke so far), as committing
    /// it gives (`Detail::apply`). `hold`: how many of its last dabs may
    /// still change as the stroke grows. They run (on a copy) only for the
    /// stroke as it ended (`last`); before that the preview trails the
    /// mouse by those dabs (a fraction of the radius) and each update runs
    /// only the dabs new since the last.
    pub fn update(&mut self, stroke: &Resolved, hold: usize, last: bool) -> Result<Option<Detail>, String> {
        let mut stroke = stroke.clone();
        let mut key = Resolved { dabs: Vec::new(), paint_dabs: None, ..stroke.clone() };
        if self.state.is_none() {
            // The mask and groups it reads, kept by hash, as the commit does
            // (a drag's mask settings stay as they were at the press).
            let mut clay = self.clay.clone();
            ledger::freeze(&mut clay, &mut stroke)?;
            let view = layers::view_for::<Detail>(&clay, &self.layer)?.into_owned();
            let own = <Detail as Grid>::current(&view).cloned();
            let input = SculptInput::of(&view, own.as_ref())?;
            key.frozen = stroke.frozen;
            let grid = input.grid.clone();
            self.state = Some(State { view, own, input, key: key.clone(), run: Vec::new(), grid, runs: 0, approx: None, fallbacks: 0 });
        }
        let s = self.state.as_mut().expect("set above");
        stroke.frozen = s.key.frozen;
        key.frozen = s.key.frozen;
        let dabs = &stroke.dabs;
        if s.key != key {
            (s.key, s.run, s.grid, s.approx) = (key, Vec::new(), s.input.grid.clone(), None);
        }
        let copies = stroke.instances.max(1);
        let per = if dabs.len() % copies == 0 { dabs.len() / copies } else { 0 };
        let same = s.run.len() <= dabs.len() && s.run.iter().zip(dabs).all(|(a, b)| a == b);
        // The per-copy approximation reads the dabs as `copies` runs of
        // `per` each (as the resolver lays symmetric copies out). Should a
        // stroke not split so, it would add nothing and stop following the
        // mouse: it runs whole from the start instead (the exact path).
        if s.approx.as_ref().is_some_and(|ran| per == 0 || ran.len() != copies) {
            s.approx = None;
            s.run.clear();
            s.grid = s.input.grid.clone();
            s.fallbacks += 1;
        }
        if s.approx.is_none() && !same {
            s.run.clear();
            // From here on, each copy's new dabs (see the module doc); a
            // path brush (one dab per copy) runs whole.
            if !last && per > 1 && stroke.params.brush.dabs() {
                s.approx = Some(vec![0; copies]);
            }
            s.grid = s.input.grid.clone();
        }
        if let Some(ran) = s.approx.as_mut() {
            let mut new = Vec::new();
            {
                for (j, n) in ran.iter_mut().enumerate() {
                    // The stroke as it ended runs each copy's held end too.
                    let keep = if last { per } else { per.saturating_sub(hold) };
                    if keep > *n {
                        new.extend_from_slice(&dabs[j * per + *n..j * per + keep]);
                        *n = keep;
                    }
                }
            }
            if !new.is_empty() {
                s.runs += new.len();
                s.grid = Some(s.input.run(&s.view, s.grid.as_ref(), &stroke, &new)?.detail);
            }
            let Some(detail) = s.grid.clone() else { return Ok(s.own.clone()) };
            let diff = crate::sculpt::engine::Diff { detail, changed_points: 0, radius: dabs.last().map_or(0.0, |d| d.radius), dabs: dabs.len() };
            return Ok(s.input.finish(s.own.as_ref(), diff, &stroke)?.grid);
        }
        let keep = dabs.len().saturating_sub(hold);
        if keep > s.run.len() {
            let diff = s.input.run(&s.view, s.grid.as_ref(), &stroke, &dabs[s.run.len()..keep])?;
            s.runs += keep - s.run.len();
            s.grid = Some(diff.detail);
            s.run = dabs[..keep].to_vec();
        }
        let tail = if last { &dabs[s.run.len()..] } else { &[] };
        let (grid, radius) = if tail.is_empty() {
            (s.grid.clone(), dabs.last().map_or(0.0, |d| d.radius))
        } else {
            s.runs += tail.len();
            let diff = s.input.run(&s.view, s.grid.as_ref(), &stroke, tail)?;
            (Some(diff.detail), diff.radius)
        };
        let Some(detail) = grid else { return Ok(s.own.clone()) };
        let diff = crate::sculpt::engine::Diff { detail, changed_points: 0, radius, dabs: dabs.len() };
        Ok(s.input.finish(s.own.as_ref(), diff, &stroke)?.grid)
    }

    /// Dabs the engine ran over every update (a whole rerun counts again).
    pub fn dabs_run(&self) -> usize {
        self.state.as_ref().map_or(0, |s| s.runs)
    }

    /// Times the per-copy approximation was left for a whole rerun.
    #[cfg(test)]
    pub fn fallbacks(&self) -> usize {
        self.state.as_ref().map_or(0, |s| s.fallbacks)
    }

    /// Whether the last update's grid is exactly what the stroke so far
    /// commits (not the per-copy approximation: see the module doc).
    pub fn exact(&self) -> bool {
        self.state.as_ref().is_none_or(|s| s.approx.is_none())
    }

    /// The clay at the press with `grid` as its layer's grid: what the
    /// editor draws while the stroke records.
    pub fn shown(&self, grid: Option<Detail>) -> Clay {
        let mut clay = self.clay.clone();
        layers::set_grid::<Detail>(&mut clay, &self.layer, grid);
        clay
    }
}

/// A paint, mask or group stroke's preview: the stroke so far applied
/// whole to the grid of kind G before it, as its commit does (paint
/// strokes keep the mask and groups they read first: `freeze`). Returns
/// the clay with the new grid and the grid's hash.
pub fn whole<G: Layered>(clay: &Clay, layer: &str, stroke: &Resolved, freeze: bool) -> Result<(Clay, ledger::Hash128), String> {
    let mut clay = clay.clone();
    let mut stroke = stroke.clone();
    if freeze {
        ledger::freeze(&mut clay, &mut stroke)?;
    }
    let view = layers::view_for::<G>(&clay, layer)?.into_owned();
    let before = G::current(&view).cloned();
    let after = G::apply(&view, before.as_ref(), &stroke)?.grid;
    let hash = G::hash(after.as_ref());
    layers::set_grid::<G>(&mut clay, layer, after);
    Ok((clay, hash))
}

#[cfg(test)]
mod tests {
    use glam::Vec3;

    use super::*;
    use crate::brush::resolve::{self, ResolveCtx};
    use crate::brush::spec::{Brush, StrokeSpec};

    fn ball() -> Clay {
        serde_json::from_value(serde_json::json!({ "blobs": [{ "type": "sphere", "center": [0, 0, 0], "radius": 0.5 }], "resolution": 48, "detail_cell": 0.01 })).unwrap()
    }

    /// A path over the ball's top, `n` points long.
    fn path(n: usize) -> Vec<Vec3> {
        (0..n).map(|i| {
            let a = -0.6 + 1.2 * i as f32 / 40.0;
            Vec3::new(a.sin() * 0.5, a.cos() * 0.5, 0.05 * (i as f32 * 0.7).sin())
        }).collect()
    }

    fn resolved(brush: Brush, n: usize, f: impl Fn(&mut StrokeSpec)) -> Resolved {
        let mut s = StrokeSpec::new(brush, &path(n), 0.06);
        s.strength = Some(0.6);
        f(&mut s);
        resolve::resolve(&s, &ResolveCtx::local()).unwrap()
    }

    fn hash(g: Option<&Detail>) -> ledger::Hash128 {
        <Detail as Grid>::hash(g)
    }

    /// Running a stroke's dabs in pieces, each piece from the last one's
    /// grid, gives the grid running them at once gives (the brick set
    /// too: the hash sums every brick), on grids with and without a
    /// refined region and with a first stroke that makes the grid.
    #[test]
    fn dabs_run_in_pieces_give_the_whole_strokes_grid() {
        let mut clay = ball();
        let first = resolved(Brush::Standard, 12, |_| {});
        let fresh = <Detail as Grid>::apply(&clay, None, &first).unwrap().grid;
        let mut refined = clay.clone();
        refined.detail = fresh.clone();
        refined.set_detail_regions(&[crate::clay::Region { name: "top".into(), center: Vec3::new(0.0, 0.45, 0.0), radii: Vec3::splat(0.2), cell: 0.005 }]).unwrap();
        for (what, c) in [("no grid", clay.clone()), ("a grid", { clay.detail = fresh.clone(); clay.clone() }), ("a refined region", refined)] {
            for brush in [Brush::Standard, Brush::Clay, Brush::ClayBuildup] {
                let stroke = resolved(brush, 41, |_| {});
                let want = <Detail as Grid>::apply(&c, c.detail.as_ref(), &stroke).unwrap().grid;
                let input = SculptInput::of(&c, c.detail.as_ref()).unwrap();
                for cuts in [vec![1], vec![3, 4, 20], vec![stroke.dabs.len() - 1]] {
                    let mut grid = input.grid.clone();
                    let mut from = 0;
                    for to in cuts.into_iter().chain([stroke.dabs.len()]) {
                        grid = Some(input.run(&c, grid.as_ref(), &stroke, &stroke.dabs[from..to]).unwrap().detail);
                        from = to;
                    }
                    assert_eq!(hash(grid.as_ref()), hash(want.as_ref()), "{brush:?} on {what}");
                }
            }
        }
    }

    /// The preview, fed the stroke as it grows (as the editor's drag
    /// does), shows at each step the grid that step's stroke commits;
    /// most dabs run once. Symmetric and tapered strokes (whose run dabs
    /// change as the path grows) show each copy's new dabs on the grid
    /// so far, to the end, and say they are inexact: their work over the
    /// drag stays about one stroke's, not one per sample.
    #[test]
    fn a_growing_stroke_previews_what_each_step_commits() {
        let mut clay = ball();
        clay.detail = <Detail as Grid>::apply(&clay, None, &resolved(Brush::Inflate, 6, |_| {})).unwrap().grid;
        let sym = |s: &mut StrokeSpec| s.symmetry = Some(serde_json::from_value(serde_json::json!({ "axes": ["x"] })).unwrap());
        let taper = |s: &mut StrokeSpec| s.taper = Some([0.2, 0.3]);
        let plain = |_: &mut StrokeSpec| {};
        let cases: [(Brush, &str, &dyn Fn(&mut StrokeSpec)); 6] = [(Brush::Standard, "plain", &plain), (Brush::Clay, "plain", &plain), (Brush::Standard, "symmetric", &sym), (Brush::Clay, "symmetric", &sym), (Brush::Standard, "tapered", &taper), (Brush::Inflate, "plain", &plain)];
        for (brush, what, f) in cases {
            let mut p = SculptPreview::new(clay.clone(), "base");
            let mut total = 0;
            // The editor's hold: spaced dabs whose end may still move.
            let hold = if brush.dabs() { 2 } else { 0 };
            let exact = what == "plain";
            let mut approximated = false;
            for n in (2..=41).step_by(3) {
                let stroke = resolved(brush, n, f);
                let last = n + 3 > 41;
                let got = p.update(&stroke, hold, last).unwrap();
                approximated |= !p.exact();
                // Whenever the preview says it is exact, it is.
                if p.exact() {
                    // Before the end: the stroke without its held dabs.
                    let shown = if last { stroke.clone() } else { Resolved { dabs: stroke.dabs[..stroke.dabs.len().saturating_sub(hold)].to_vec(), ..stroke.clone() } };
                    let want = if shown.dabs.is_empty() { clay.detail.clone() } else { <Detail as Grid>::apply(&clay, clay.detail.as_ref(), &shown).unwrap().grid };
                    assert_eq!(hash(got.as_ref()), hash(want.as_ref()), "{brush:?} {what} at {n} points");
                }
                total = stroke.dabs.len();
            }
            eprintln!("{brush:?} ({what}): {total} dabs, {} run over the drag", p.dabs_run());
            assert_eq!(approximated, !exact && brush.dabs(), "{brush:?} {what}");
            if brush.dabs() {
                // About once over the drag, not once per update.
                assert!(p.dabs_run() < total * 2, "{brush:?} {what}: {} dabs run for {total}", p.dabs_run());
            }
        }
    }

    /// A symmetric stroke whose dabs do not split evenly into its copies
    /// (no resolver makes one, but the preview must not rely on it) leaves
    /// the per-copy approximation and runs whole, exactly, instead of
    /// adding nothing: the preview keeps following the stroke.
    #[test]
    fn an_uneven_symmetric_stroke_falls_back_to_a_whole_rerun() {
        let mut clay = ball();
        clay.detail = <Detail as Grid>::apply(&clay, None, &resolved(Brush::Inflate, 6, |_| {})).unwrap().grid;
        let sym = |s: &mut StrokeSpec| s.symmetry = Some(serde_json::from_value(serde_json::json!({ "axes": ["x"] })).unwrap());
        let mut p = SculptPreview::new(clay.clone(), "base");
        let hold = 2;
        for n in [5, 9, 13] {
            p.update(&resolved(Brush::Standard, n, sym), hold, false).unwrap();
        }
        assert!(!p.exact(), "a growing symmetric stroke goes per copy");
        let mut prev = hash(p.update(&resolved(Brush::Standard, 17, sym), hold, false).unwrap().as_ref());
        for n in [21, 25] {
            // One dab short of the copies' split.
            let mut odd = resolved(Brush::Standard, n, sym);
            assert_eq!(odd.instances, 2);
            odd.dabs.pop();
            assert!(odd.dabs.len() % 2 == 1);
            let before = p.dabs_run();
            let got = hash(p.update(&odd, hold, false).unwrap().as_ref());
            assert!(p.dabs_run() > before, "{n}: it ran dabs");
            assert_ne!(got, prev, "{n}: the preview moved on");
            assert!(p.exact(), "{n}: a whole rerun is exact");
            let shown = Resolved { dabs: odd.dabs[..odd.dabs.len() - hold].to_vec(), ..odd.clone() };
            let want = <Detail as Grid>::apply(&clay, clay.detail.as_ref(), &shown).unwrap().grid;
            assert_eq!(got, hash(want.as_ref()), "{n}: the stroke so far, exactly");
            prev = got;
        }
        assert_eq!(p.fallbacks(), 1, "left the approximation once (the next uneven update ran on exactly)");
        // An even stroke after it goes on (per copy again: its dabs moved).
        let got = hash(p.update(&resolved(Brush::Standard, 29, sym), hold, true).unwrap().as_ref());
        assert_ne!(got, prev);
    }

    /// How far `preview` is from `commit`, both made from `before`, over
    /// the bricks either touches (one level: the ball has no refined
    /// regions).
    struct Drift {
        /// max |preview - commit| at a grid point.
        worst: f32,
        /// The most the commit moved a point (the stroke's depth).
        depth: f32,
        /// The most the preview moved a point further than the commit did.
        over: f32,
        /// |points both changed| / |points either changed|.
        overlap: f64,
    }

    fn drift(before: Option<&Detail>, preview: Option<&Detail>, commit: Option<&Detail>) -> Drift {
        let get = |g: Option<&Detail>, i: [i32; 3]| g.map_or(0.0, |g| g.grid().get(i));
        let mut keys: Vec<[i32; 3]> = [preview, commit].into_iter().flatten().flat_map(|g| g.grid().bricks().map(|(k, _)| *k).collect::<Vec<_>>()).collect();
        keys.sort_unstable();
        keys.dedup();
        let mut d = Drift { worst: 0.0, depth: 0.0, over: 0.0, overlap: 1.0 };
        let (mut both, mut either) = (0usize, 0usize);
        for k in keys {
            for at in 0..512 {
                let i = [k[0] * 8 + at % 8, k[1] * 8 + at / 8 % 8, k[2] * 8 + at / 64];
                let (b, p, c) = (get(before, i), get(preview, i), get(commit, i));
                d.worst = d.worst.max((p - c).abs());
                d.depth = d.depth.max((c - b).abs());
                d.over = d.over.max((p - b).abs() - (c - b).abs());
                let (dp, dc) = (p != b, c != b);
                both += usize::from(dp && dc);
                either += usize::from(dp || dc);
            }
        }
        if either > 0 {
            d.overlap = both as f64 / either as f64;
        }
        d
    }

    /// The path `path` takes, turned about Y after point `from` (a stroke
    /// taken back and drawn on elsewhere).
    fn branch(n: usize, from: usize) -> Vec<Vec3> {
        path(n).into_iter().enumerate().map(|(i, p)| if i <= from { p } else { glam::Quat::from_rotation_y(0.04 * (i - from) as f32) * p }).collect()
    }

    /// An inexact preview (dabs already run change as the stroke grows:
    /// symmetric copies, taper, a stroke LazyMouse's Backtrack takes back)
    /// stays within measured bounds of what the stroke commits: fed the
    /// stroke as it grows (a point at a time and three at a time), its
    /// grid for the stroke as it ended against the committed grid, over
    /// every grid point of the bricks either touches.
    ///
    /// Measured (ball r = 0.5 at a 0.01 cell, radius 0.06, strength 0.6,
    /// 41 path points, holding the editor's dabs; see each case's
    /// eprintln; preview and commit are deterministic, so they repeat run
    /// to run), as fractions of the stroke's depth (the most the commit
    /// moved a point):
    /// - symmetric copies (standard, clay, clay_buildup): max |preview -
    ///   commit| 0.03-0.125 (0.07-0.37 cell), the preview past the commit
    ///   at most as much, changed points overlap 0.957 or more. Bounds 0.2,
    ///   0.2 and 0.93.
    /// - taper: each dab ran with the gain it had when it was near the
    ///   path's end (taper-out fades it) and its start (taper-in, which
    ///   lowers as the path grows), so the preview is shallower along the
    ///   middle and deeper near the start: standard 0.97 of the depth (2.4
    ///   cells), 0.66 deeper, overlap 0.876; clay 0.80 (1.1 cells), overlap
    ///   0.984; symmetric clay 0.71, overlap 0.98. Bounds standard 1.5, 1.0
    ///   and 0.8; clay 1.2, 1.2 and 0.97; symmetric clay 1.1, 1.1 and 0.97.
    /// - LazyMouse's Backtrack (a stroke taken back and drawn on another
    ///   way; the stroke's own `lazy` respaces only its end, which the
    ///   preview holds, so it stays exact): a plain stroke reruns from the
    ///   start once its dabs change and so ends as the commit up to float
    ///   rounding (1e-7, 4e-6 of the depth; overlap 1; bounds 1e-4, 1e-4
    ///   and 0.999); a symmetric one 0.086, 0.052 deeper, overlap 0.98.
    ///   Bounds 0.13, 0.08 and 0.97.
    ///
    /// Bounds are about 1.5x the worst measured (overlap: 1.5x the
    /// shortfall), rounded. The taper figures are large: the record
    /// replaces such a preview as soon as it is in.
    #[test]
    fn an_inexact_preview_stays_close_to_the_commit() {
        let mut clay = ball();
        clay.detail = <Detail as Grid>::apply(&clay, None, &resolved(Brush::Inflate, 6, |_| {})).unwrap().grid;
        let voxel = clay.detail.as_ref().unwrap().voxel();
        let sym = |s: &mut StrokeSpec| s.symmetry = Some(serde_json::from_value(serde_json::json!({ "axes": ["x"] })).unwrap());
        let taper = |s: &mut StrokeSpec| s.taper = Some([0.2, 0.3]);
        let plain = |_: &mut StrokeSpec| {};
        let sym_taper = |s: &mut StrokeSpec| {
            sym(s);
            taper(s);
        };
        // (brush, case, spec, taken back, max |p - c| / depth, max over / depth, min overlap)
        type Case<'a> = (Brush, &'a str, &'a dyn Fn(&mut StrokeSpec), bool, f32, f32, f64);
        let cases: [Case; 8] = [
            (Brush::Standard, "symmetric", &sym, false, 0.2, 0.2, 0.93),
            (Brush::Clay, "symmetric", &sym, false, 0.2, 0.2, 0.93),
            (Brush::ClayBuildup, "symmetric", &sym, false, 0.2, 0.2, 0.93),
            (Brush::Standard, "tapered", &taper, false, 1.5, 1.0, 0.8),
            (Brush::Clay, "tapered", &taper, false, 1.2, 1.2, 0.97),
            (Brush::Clay, "symmetric, tapered", &sym_taper, false, 1.1, 1.1, 0.97),
            (Brush::Standard, "taken back", &plain, true, 1e-4, 1e-4, 0.999),
            (Brush::Clay, "symmetric, taken back", &sym, true, 0.13, 0.08, 0.97),
        ];
        for (brush, what, f, back, max_worst, max_over, min_overlap) in cases {
            for step in [1, 3] {
                // The stroke as fed: grown to 41 points, or grown to 29,
                // taken back to 17 and drawn on another way to 41.
                let mut feeds: Vec<Vec<Vec3>> = (2..=41).step_by(step).map(path).collect();
                if back {
                    feeds = (2..=29).step_by(step).map(path).chain([path(17)]).chain((18..=41).step_by(step).map(|n| branch(n, 16))).collect();
                }
                if feeds.last().map(Vec::len) != Some(41) {
                    feeds.push(if back { branch(41, 16) } else { path(41) });
                }
                let mut p = SculptPreview::new(clay.clone(), "base");
                let hold = if brush == Brush::Standard { 1 } else { 2 };
                let mut got = None;
                let mut stroke = None;
                for (k, pts) in feeds.iter().enumerate() {
                    let mut spec = StrokeSpec::new(brush, pts, 0.06);
                    spec.strength = Some(0.6);
                    f(&mut spec);
                    let s = resolve::resolve(&spec, &ResolveCtx::local()).unwrap();
                    got = p.update(&s, hold, k + 1 == feeds.len()).unwrap();
                    stroke = Some(s);
                }
                let stroke = stroke.unwrap();
                let commit = <Detail as Grid>::apply(&clay, clay.detail.as_ref(), &stroke).unwrap().grid;
                let d = drift(clay.detail.as_ref(), got.as_ref(), commit.as_ref());
                eprintln!("{brush:?} ({what}, {step} at a time): {} dabs, {} run, exact {}; max |preview - commit| {:.5} = {:.3} cell = {:.3} of the depth {:.5}; over {:.3} of it; changed points overlap {:.4}", stroke.dabs.len(), p.dabs_run(), p.exact(), d.worst, d.worst / voxel, d.worst / d.depth, d.depth, d.over / d.depth, d.overlap);
                assert!(!p.exact(), "{brush:?} {what}: the preview is the per-copy approximation");
                assert!(d.depth > 0.0, "{brush:?} {what}: the stroke changed the grid");
                assert!(d.worst <= max_worst * d.depth, "{brush:?} {what}, {step} at a time: max |preview - commit| {} over the depth {}", d.worst, d.depth);
                assert!(d.over <= max_over * d.depth, "{brush:?} {what}, {step} at a time: the preview goes {} further than the commit (depth {})", d.over, d.depth);
                assert!(d.overlap >= min_overlap, "{brush:?} {what}, {step} at a time: changed points overlap {}", d.overlap);
            }
        }
    }

    /// A stroke on a sculpt layer previews the layer grid its commit
    /// writes (the change folded back at the layer's amount).
    #[test]
    fn a_layer_stroke_previews_the_layers_grid() {
        let mut clay = ball();
        clay.detail = <Detail as Grid>::apply(&clay, None, &resolved(Brush::Inflate, 6, |_| {})).unwrap().grid;
        ledger::stack_mut(&mut clay).layers.push(layers::Layer::new("detail", layers::Kind::Sculpt, 0.7));
        let mut p = SculptPreview::new(clay.clone(), "detail");
        for n in [3, 9, 20, 41] {
            let stroke = resolved(Brush::Standard, n, |_| {});
            let view = layers::view_for::<Detail>(&clay, "detail").unwrap();
            let want = <Detail as Grid>::apply(&view, <Detail as Grid>::current(&view), &stroke).unwrap().grid;
            assert_eq!(hash(p.update(&stroke, 1, true).unwrap().as_ref()), hash(want.as_ref()), "{n} points");
        }
    }
}

