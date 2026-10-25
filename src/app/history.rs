//! Undo history: whole-scene snapshots, one per edit, each with what made
//! it (`StepInfo`). Grids and ledgers are Arcs compared by identity, so a
//! snapshot costs only what the edit changed.
//!
//! Agent calls each get their own step, labelled like "sculpt standard x3
//! on Head [agent]"; a continuous UI edit (a drag) is one step. The list
//! is capped at `MAX_STEPS` steps and `MAX_BYTES` of grid data held only
//! by the history (oldest dropped first, counted in `dropped`), and the
//! `history` tool lists, jumps, marks and labels steps.
//!
//! Step numbers count every step since the scene was new or opened: the
//! scene after step n is "snapshot n", and dropping old steps does not
//! renumber the rest.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::ledger::{Grid, ledger};
use crate::scene::Scene;

/// Most undo steps kept.
pub const MAX_STEPS: usize = 300;
/// Most bytes of grid data the history may hold beyond the live scene.
pub const MAX_BYTES: u64 = 2 << 30;

/// What an undo step was.
#[derive(Clone, Debug, Default)]
pub struct StepInfo {
    pub label: String,
    /// agent, ui, or agent+ui (an agent call during a UI drag).
    pub source: String,
    pub tools: Vec<String>,
    /// Unix seconds.
    pub time: u64,
    pub mark: Option<String>,
    /// Grid bytes only this step's snapshot holds (see `unique_bytes`).
    pub bytes: u64,
}

impl StepInfo {
    pub fn new(label: impl Into<String>, source: &str, tools: Vec<String>) -> StepInfo {
        StepInfo { label: label.into(), source: source.into(), tools, time: crate::ledger::now_secs(), mark: None, bytes: 0 }
    }
}

pub struct History {
    /// Snapshots before each step, oldest first, with the step they undo.
    undo: Vec<(Scene, StepInfo)>,
    /// Snapshots after undone steps, the next one last.
    redo: Vec<(Scene, StepInfo)>,
    /// Steps dropped from the front by the caps.
    pub dropped: u64,
    pub max_steps: usize,
    pub max_bytes: u64,
}

impl Default for History {
    fn default() -> Self {
        History { undo: Vec::new(), redo: Vec::new(), dropped: 0, max_steps: MAX_STEPS, max_bytes: MAX_BYTES }
    }
}

impl History {
    /// Records a step: `before` is the scene before it, `after` the scene
    /// now. Returns how many old steps the caps dropped.
    pub fn push(&mut self, before: Scene, after: &Scene, mut info: StepInfo) -> usize {
        info.bytes = unique_bytes(&before, after);
        self.undo.push((before, info));
        self.redo.clear();
        self.enforce()
    }

    /// Drops the oldest steps until the caps hold.
    fn enforce(&mut self) -> usize {
        let mut gone = 0;
        while !self.undo.is_empty() && (self.undo.len() > self.max_steps || self.bytes() > self.max_bytes) {
            self.undo.remove(0);
            self.dropped += 1;
            gone += 1;
        }
        gone
    }

    /// Grid bytes the history holds.
    pub fn bytes(&self) -> u64 {
        self.undo.iter().chain(&self.redo).map(|(_, i)| i.bytes).sum()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The first and last snapshot `goto` reaches (the dropped steps are
    /// gone; redo's are ahead).
    pub fn bounds(&self) -> (u64, u64) {
        (self.dropped, self.dropped + (self.undo.len() + self.redo.len()) as u64)
    }

    /// Steps applied, counting dropped ones: the scene is snapshot `current`.
    pub fn current(&self) -> u64 {
        self.dropped + self.undo.len() as u64
    }

    pub fn undo(&mut self, scene: &mut Scene) -> bool {
        let Some((prev, info)) = self.undo.pop() else { return false };
        let now = std::mem::replace(scene, prev);
        self.redo.push((now, info));
        true
    }

    pub fn redo(&mut self, scene: &mut Scene) -> bool {
        let Some((next, info)) = self.redo.pop() else { return false };
        let now = std::mem::replace(scene, next);
        self.undo.push((now, info));
        true
    }

    /// Undoes or redoes to snapshot `n`.
    pub fn goto(&mut self, n: u64, scene: &mut Scene) -> Result<(), String> {
        let last = self.current() + self.redo.len() as u64;
        if n < self.dropped || n > last {
            return Err(format!("no snapshot {n}: the history holds {} to {last}{}", self.dropped, if self.dropped > 0 { format!(" ({} older steps were dropped by the cap)", self.dropped) } else { String::new() }));
        }
        while self.current() > n {
            self.undo(scene);
        }
        while self.current() < n {
            self.redo(scene);
        }
        Ok(())
    }

    fn info_mut(&mut self, index: u64) -> Result<&mut StepInfo, String> {
        let current = self.current();
        let at = index.checked_sub(self.dropped + 1).ok_or_else(|| format!("no step {index}"))? as usize;
        if index <= current {
            return Ok(&mut self.undo[at].1);
        }
        let back = (index - current - 1) as usize;
        let len = self.redo.len();
        if back >= len {
            return Err(format!("no step {index}"));
        }
        Ok(&mut self.redo[len - 1 - back].1)
    }

    pub fn mark(&mut self, index: u64, name: Option<String>) -> Result<(), String> {
        self.info_mut(index)?.mark = name;
        Ok(())
    }

    pub fn label(&mut self, index: u64, label: String) -> Result<(), String> {
        self.info_mut(index)?.label = label;
        Ok(())
    }

    /// The step a mark names.
    pub fn find_mark(&self, name: &str) -> Option<u64> {
        let current = self.current();
        let undo = self.undo.iter().enumerate().map(|(i, s)| (self.dropped + i as u64 + 1, &s.1));
        let redo = self.redo.iter().rev().enumerate().map(|(i, s)| (current + i as u64 + 1, &s.1));
        undo.chain(redo).find(|(_, i)| i.mark.as_deref() == Some(name)).map(|(n, _)| n)
    }

    /// `history list`: every step, the current snapshot, how many were dropped.
    pub fn list(&self) -> Value {
        let current = self.current();
        let entry = |index: u64, i: &StepInfo, done: bool| json!({ "index": index, "label": i.label, "source": i.source, "tools": i.tools, "time": i.time, "mark": i.mark, "bytes": i.bytes, "undone": !done });
        let mut steps: Vec<Value> = self.undo.iter().enumerate().map(|(k, (_, i))| entry(self.dropped + k as u64 + 1, i, true)).collect();
        steps.extend(self.redo.iter().rev().enumerate().map(|(k, (_, i))| entry(current + k as u64 + 1, i, false)));
        json!({ "steps": steps, "current": current, "dropped": self.dropped, "bytes": self.bytes(), "caps": { "steps": self.max_steps, "bytes": self.max_bytes } })
    }
}

/// Grid bytes `before` holds that `after` does not share: sculpt bricks
/// whose Arcs differ (by key), whole paint and mask grids that differ,
/// ledger records that differ. Only references the two scenes hold count,
/// so caches holding the same Arcs elsewhere change nothing.
pub fn unique_bytes(before: &Scene, after: &Scene) -> u64 {
    let mut total = 0u64;
    for o in &before.objects {
        let Some(b) = &o.clay else { continue };
        let a = after.get(o.id).and_then(|x| x.clay.as_ref());
        // Sculpt bricks, by Arc: every detail grid of the clay (base, extra
        // layers, morph bases and morph layers, each with its levels) in
        // `before`, against every brick `after`'s clay holds. Morphs share
        // their bricks with the grids they were stored from, so each brick
        // counts once, and only if `after` does not hold it too.
        let theirs: Vec<&crate::clay::Detail> = a.map(detail_grids).unwrap_or_default();
        let shared: HashSet<usize> = theirs.iter().map(|d| d.addr()).collect();
        let mine: Vec<&crate::clay::Detail> = detail_grids(b).into_iter().filter(|d| !shared.contains(&d.addr())).collect();
        if !mine.is_empty() {
            let held: HashSet<usize> = theirs.iter().flat_map(|d| grid_bricks(d)).collect();
            let mut seen: HashSet<usize> = HashSet::new();
            let mut grids: HashSet<usize> = HashSet::new();
            for d in mine.into_iter().filter(|d| grids.insert(d.addr())) {
                for brick in grid_bricks(d) {
                    if !held.contains(&brick) && seen.insert(brick) {
                        total += (crate::clay::BRICK_VALUES * 4) as u64;
                    }
                }
            }
        }
        let paint_bytes = |p: &crate::paint::Paint| (p.grid().brick_count() * crate::clay::BRICK_VALUES * 4) as u64;
        if let Some(p) = &b.paint
            && a.and_then(|a| a.paint.as_ref()) != Some(p)
        {
            total += paint_bytes(p);
        }
        // Extra paint layers, against the same layer in `after`.
        for l in crate::layers::extra(b).iter().filter(|l| l.kind == crate::layers::Kind::Paint) {
            if let Some(p) = &l.paint
                && a.and_then(|a| crate::layers::layer(a, &l.name)).and_then(|x| x.paint.as_ref()) != Some(p)
            {
                total += paint_bytes(p);
            }
        }
        for (name, m) in &b.masks {
            if a.and_then(|a| a.masks.get(name)) != Some(m) {
                total += paint_bytes(m);
            }
        }
        if b.sculpt.is_some() && a.map(|a| &a.sculpt) != Some(&b.sculpt) {
            total += records_bytes::<crate::clay::Detail>(b, a) + records_bytes::<crate::paint::Paint>(b, a);
        }
    }
    total
}

/// Every detail grid a clay holds: its own, its extra sculpt layers',
/// and its morphs' bases and layers.
fn detail_grids(c: &crate::clay::Clay) -> Vec<&crate::clay::Detail> {
    let mut out: Vec<&crate::clay::Detail> = c.detail.iter().collect();
    out.extend(crate::layers::extra(c).iter().filter_map(|l| l.detail.as_ref()));
    if let Some(s) = c.sculpt.as_ref() {
        for m in &s.data().morphs {
            out.extend(m.base.iter().chain(m.layers.iter().filter_map(|l| l.detail.as_ref())));
        }
    }
    out
}

/// The addresses of a detail grid's bricks, its levels' included.
fn grid_bricks(d: &crate::clay::Detail) -> impl Iterator<Item = usize> + '_ {
    let g = d.grid();
    std::iter::once(g).chain(g.levels().iter().map(|l| &l.grid)).flat_map(|g| g.brick_keys().map(move |k| Arc::as_ptr(g.brick(k).expect("listed")) as usize))
}

/// About the bytes of `b`'s records `a` does not share.
fn records_bytes<G: Grid>(b: &crate::clay::Clay, a: Option<&crate::clay::Clay>) -> u64 {
    let mine = ledger::<G>(b);
    let theirs: HashSet<usize> = a.map(|a| ledger::<G>(a).records().iter().map(|r| Arc::as_ptr(r) as usize).collect()).unwrap_or_default();
    mine.records()
        .iter()
        .filter(|r| !theirs.contains(&(Arc::as_ptr(r) as usize)))
        .map(|r| 256 + r.stroke().map_or(0, |(_, s)| s.dabs.iter().map(|d| d.points.len() * 12 + 64).sum::<usize>()) as u64)
        .sum()
}

/// The undo label of an agent call: "sculpt standard x3 on Head [agent]".
pub fn call_label(tool: &str, args: &Value, scene: &Scene, source: &str) -> String {
    let name = args.get("target").filter(|t| !t.is_null()).and_then(|t| match t {
        Value::Number(n) => n.as_u64().and_then(|id| scene.get(id)).map(|o| o.name.clone()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    });
    let mut what = tool.to_string();
    if tool == "sculpt" || tool == "paint" {
        let brushes: Vec<String> = args["strokes"].as_array().into_iter().flatten().map(|s| s["brush"].as_str().unwrap_or(if tool == "paint" { "paint" } else { "stroke" }).to_string()).collect();
        let mut groups: Vec<(String, usize)> = Vec::new();
        for b in brushes {
            match groups.last_mut() {
                Some((g, n)) if *g == b => *n += 1,
                _ => groups.push((b, 1)),
            }
        }
        let parts: Vec<String> = groups.iter().map(|(b, n)| if *n > 1 { format!("{b} x{n}") } else { b.clone() }).collect();
        if !parts.is_empty() {
            what = format!("{tool} {}", parts.join(", "));
        }
    } else if tool == "mask" || tool == "group" {
        let names: Vec<String> = args["actions"].as_array().into_iter().flatten().take(8).filter_map(|a| a.as_str().map(str::to_string).or_else(|| a.as_object().and_then(|o| o.keys().next().cloned()))).collect();
        if !names.is_empty() {
            what = format!("{tool} {}", names.join(", "));
        }
    } else if let Some(a) = args["action"].as_str() {
        what = format!("{tool} {a}");
    }
    if let Some(l) = args["label"].as_str().filter(|l| !l.is_empty()) {
        what = format!("{what} \"{l}\"");
    }
    match name {
        Some(n) => format!("{what} on {n} [{source}]"),
        None => format!("{what} [{source}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Shape;

    fn scene_with(n: usize) -> Scene {
        let mut s = Scene::default();
        for _ in 0..n {
            s.add(Shape::Cube);
        }
        s
    }

    #[test]
    fn steps_split_per_call_and_goto_restores_each_snapshot() {
        let mut h = History::default();
        let mut scene = Scene::default();
        let mut snapshots = vec![scene.clone()];
        // Five agent calls in one frame: five steps.
        for k in 0..5 {
            let before = scene.clone();
            scene.add(Shape::Sphere);
            let args = json!({ "target": "pSphere1", "strokes": [{"brush": "standard"}, {"brush": "standard"}, {"brush": "standard"}] });
            let tool = if k == 2 { "sculpt" } else { "create_object" };
            h.push(before, &scene, StepInfo::new(call_label(tool, &args, &scene, "agent"), "agent", vec![tool.into()]));
            snapshots.push(scene.clone());
        }
        let list = h.list();
        assert_eq!(list["steps"].as_array().unwrap().len(), 5);
        assert_eq!(list["current"], 5);
        assert_eq!(list["steps"][2]["label"], "sculpt standard x3 on pSphere1 [agent]");
        assert!(list["steps"][2]["label"].as_str().unwrap().contains("sculpt"));
        for n in [0u64, 3, 1, 5, 2] {
            h.goto(n, &mut scene).unwrap();
            assert!(scene == snapshots[n as usize], "snapshot {n}");
            assert_eq!(h.current(), n);
        }
        // Undone steps are listed after the current one, and marks find them.
        h.goto(2, &mut scene).unwrap();
        h.mark(4, Some("before eyes".into())).unwrap();
        h.label(1, "rough form".into()).unwrap();
        assert_eq!(h.find_mark("before eyes"), Some(4));
        let list = h.list();
        assert_eq!((list["steps"][3]["undone"].as_bool(), list["steps"][0]["label"].as_str()), (Some(true), Some("rough form")));
        assert!(h.goto(9, &mut scene).is_err());
        // A new edit drops the undone steps.
        let before = scene.clone();
        scene.add(Shape::Cone);
        h.push(before, &scene, StepInfo::new("x", "ui", vec![]));
        assert_eq!((h.current(), h.can_redo()), (3, false));
    }

    #[test]
    fn the_caps_drop_the_oldest_steps_and_say_so() {
        let mut h = History { max_steps: 3, ..History::default() };
        let mut scene = Scene::default();
        let mut snapshots = vec![scene.clone()];
        for _ in 0..5 {
            let before = scene.clone();
            scene.add(Shape::Torus);
            h.push(before, &scene, StepInfo::new("add", "agent", vec![]));
            snapshots.push(scene.clone());
        }
        assert_eq!((h.dropped, h.current()), (2, 5));
        let list = h.list();
        assert_eq!(list["dropped"], 2);
        assert_eq!(list["steps"][0]["index"], 3);
        assert!(h.goto(1, &mut scene).unwrap_err().contains("dropped"));
        h.goto(2, &mut scene).unwrap();
        assert!(scene == snapshots[2]);

        // The byte cap: sculpt bricks only the history holds.
        let mut h = History { max_bytes: 3 * 2048 * 10, ..History::default() };
        let mut scene = scene_with(1);
        let id = scene.objects[0].id;
        scene.get_mut(id).unwrap().clay = Some(crate::clay::Clay::default());
        for k in 0..6 {
            let before = scene.clone();
            let clay = scene.get_mut(id).unwrap().clay.as_mut().unwrap();
            let mut d = clay.detail.clone().unwrap_or_else(|| crate::clay::Detail::new(0.01));
            // The same 10 bricks rewritten: each snapshot holds its own.
            for b in 0..10 {
                d.grid_mut().set([b * 8, 0, 0], 0.001 * (k + 1) as f32);
            }
            clay.detail = Some(d);
            h.push(before, &scene, StepInfo::new("stroke", "agent", vec![]));
        }
        // 6 steps: the first held no bricks, the other 5 held 10 each.
        assert_eq!(h.list()["steps"][h.list()["steps"].as_array().unwrap().len() - 1]["bytes"], 10 * 2048);
        assert!(h.bytes() <= 3 * 2048 * 10, "{}", h.bytes());
        assert_eq!((h.dropped, h.current()), (3, 6));
    }

    #[test]
    fn morph_grids_count_toward_the_byte_cap_once() {
        let mut scene = scene_with(1);
        let id = scene.objects[0].id;
        let mut d = crate::clay::Detail::new(0.01);
        for b in 0..10 {
            d.grid_mut().set([b * 8, 0, 0], 0.001);
        }
        let mut clay = crate::clay::Clay { detail: Some(d.clone()), ..Default::default() };
        crate::ledger::stack_mut(&mut clay);
        scene.get_mut(id).unwrap().clay = Some(clay.clone());
        let morph = |base: crate::clay::Detail| crate::layers::Morph { name: "m".into(), context: Default::default(), base: Some(base), base_strokes: Default::default(), layers: Vec::new() };
        let with = |m: Option<crate::layers::Morph>| {
            let mut s = scene.clone();
            let c = s.get_mut(id).unwrap().clay.as_mut().unwrap();
            crate::ledger::stack_mut(c).morphs = m.into_iter().collect();
            s
        };
        // A morph of grids the clay shows holds nothing of its own.
        let stored = with(Some(morph(d.clone())));
        assert_eq!(unique_bytes(&stored, &scene), 0, "deleting it frees nothing: the clay holds its bricks");
        // A morph whose grid only it holds (the clay sculpted on since,
        // here: replaced): its bricks count, once each.
        let mut own = crate::clay::Detail::new(0.01);
        for b in 0..4 {
            own.grid_mut().set([b * 8, 8, 0], 0.002);
        }
        let mut both = with(Some(morph(own.clone())));
        let c = both.get_mut(id).unwrap().clay.as_mut().unwrap();
        crate::ledger::stack_mut(c).layers.push(crate::layers::Layer::new("L", crate::layers::Kind::Sculpt, 1.0));
        crate::ledger::stack_mut(c).layers[0].detail = Some(own);
        assert_eq!(unique_bytes(&both, &scene), 4 * 2048, "the morph and the layer share the bricks: counted once");
        assert_eq!(unique_bytes(&with(Some(morph({
            let mut x = crate::clay::Detail::new(0.01);
            x.grid_mut().set([0, 16, 0], 0.003);
            x
        }))), &scene), 2048, "a morph's own brick counts");
    }
}
