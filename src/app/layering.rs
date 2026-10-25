//! The `sculpt_layer` and `morph` tools (docs/SCULPT_PLAN.md 4.2): a
//! clay's sculpt and paint layers (`layers.rs`) and its morph targets.
//! Both answer at once: the work is composing grids and recording what
//! changed (outside edits of a layer's grid become opaque records on its
//! ledger, as `edit_clay` does). `select` is the editor's state (which
//! layer its Sculpt tool strokes) and never an undo step; `record` sets
//! the layer agents' and the editor's calls without `layer` write, saved
//! with the clay so the same call does the same wherever the human
//! clicked.

use std::time::Instant;

use serde_json::{Value, json};

use super::ClideApp;
use super::tools::text;
use crate::clay::{Clay, Detail};
use crate::layers::{self, Kind, Layer, MAX_AMOUNT};
use crate::ledger;
use crate::mcp::ToolResult;

fn name_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key].as_str().ok_or_else(|| format!("{key} is required: a layer name (sculpt_layer list)"))
}

/// A layer's amount from the call: -2..2 for sculpt layers, 0..1 (its
/// opacity) for paint layers.
fn amount_arg(v: &Value, kind: Kind) -> Result<f32, String> {
    let a = v.as_f64().ok_or("amount must be a number")? as f32;
    let ok = a.is_finite()
        && match kind {
            Kind::Sculpt => a.abs() <= MAX_AMOUNT,
            Kind::Paint => (0.0..=1.0).contains(&a),
        };
    if !ok {
        return Err(match kind {
            Kind::Sculpt => format!("a sculpt layer's amount is -{MAX_AMOUNT} to {MAX_AMOUNT}"),
            Kind::Paint => "a paint layer's amount (opacity) is 0 to 1".into(),
        });
    }
    Ok(a)
}

/// Refuses a new layer name the clay has or keeps for itself (base,
/// paint, mask, groups, archived:...): a file with one is not sane.
fn taken(clay: &Clay, name: &str) -> Result<(), String> {
    if layers::names(clay).iter().any(|n| n == name) || layers::fixed(name) {
        return Err(format!("the clay already has a layer named {name} (or the name is reserved: mask, groups and names starting with archived)"));
    }
    Ok(())
}

/// The extra layer `name` (base and paint are fixed at amount 1, on).
fn extra_index(clay: &Clay, name: &str, what: &str) -> Result<usize, String> {
    if name == layers::BASE || name == layers::PAINT {
        return Err(format!("{name} is the clay's own layer: it stays at amount 1 and cannot be {what}; add a layer (sculpt_layer add) for what should change"));
    }
    layers::extra(clay).iter().position(|l| l.name == name).ok_or_else(|| format!("no layer {name}; the clay has {}", layers::names(clay).join(", ")))
}

impl ClideApp {
    /// A clay to edit: the object's, its unchecked ledgers settled first
    /// (the edits below change what replays read).
    pub(super) fn layered_clay(&self, id: u64) -> Result<Clay, String> {
        let mut clay = self.scene.get(id).and_then(|o| o.clay.clone()).ok_or("not a clay object")?;
        ledger::settle_clay(&mut clay, Some(&self.replay_cache))?;
        Ok(clay)
    }

    /// Records outside edits, checks the grid budget (with what the
    /// records add) and the point cap, and puts the clay back; a refusal
    /// leaves the scene as it was.
    pub(super) fn put_layered(&mut self, id: u64, mut clay: Clay) -> Result<(), String> {
        layers::sync_within_budget(&mut clay)?;
        ledger::check_clay_points(&clay)?;
        self.scene.get_mut(id).ok_or("object vanished")?.clay = Some(clay);
        Ok(())
    }

    fn layers_reply(&self, id: u64, extra: Value) -> ToolResult {
        let clay = self.scene.get(id).and_then(|o| o.clay.as_ref()).ok_or("not a clay object")?;
        let mut out = layers::list_json(clay);
        out["target"] = json!(id);
        out["selected"] = json!(self.layer_selection.get(&id));
        if let Value::Object(m) = extra {
            for (k, v) in m {
                out[k] = v;
            }
        }
        Ok(text(out))
    }

    /// `sculpt_layer`.
    pub(super) fn sculpt_layer(&mut self, args: &Value) -> ToolResult {
        let (id, clay) = self.clay_of(args)?;
        let action = args["action"].as_str().unwrap_or("list");
        match action {
            "list" => return self.layers_reply(id, json!({})),
            "select" => {
                // The editor's state only: no scene change, no undo step.
                let name = name_arg(args, "name")?;
                layers::kind_of(clay, name)?;
                self.layer_selection.insert(id, name.to_string());
                return self.layers_reply(id, json!({ "next": "Selected for the editor's Sculpt and Paint tools (their strokes send layer: NAME). Agent calls without layer still write the recording layer (sculpt_layer record), else base." }));
            }
            _ => {}
        }
        let mut clay = self.layered_clay(id)?;
        let mut extra = json!({});
        // A rename or delete of the selected layer moves or clears the
        // selection, once the clay is put back (a refusal changes nothing).
        let mut reselect: Option<(String, Option<String>)> = None;
        match action {
            "add" => {
                let name = name_arg(args, "name")?;
                layers::valid_name(name)?;
                taken(&clay, name)?;
                if layers::extra(&clay).len() + 2 >= layers::MAX_LAYERS {
                    return Err(format!("a clay has at most {} layers (base and paint included): bake or merge some down (sculpt_layer bake, merge_down) first", layers::MAX_LAYERS));
                }
                let kind = match args["kind"].as_str().unwrap_or("sculpt") {
                    "sculpt" => Kind::Sculpt,
                    "paint" => Kind::Paint,
                    other => return Err(format!("kind must be sculpt or paint, not {other}")),
                };
                let amount = match args.get("amount").filter(|v| !v.is_null()) {
                    Some(v) => amount_arg(v, kind)?,
                    None => 1.0,
                };
                ledger::stack_mut(&mut clay).layers.push(Layer::new(name, kind, amount));
            }
            "amount" | "enable" | "disable" => {
                let name = name_arg(args, "name")?;
                let k = extra_index(&clay, name, "changed")?;
                let l = layers::extra(&clay)[k].clone();
                let (amount, enabled) = match action {
                    "amount" => (amount_arg(&args["amount"], l.kind)?, l.enabled),
                    a => (l.amount, a == "enable"),
                };
                // Only the layer's own bricks are composed again; the rest of
                // the composite is shared.
                let t = Instant::now();
                let planted = match l.kind {
                    Kind::Sculpt if l.detail.is_some() => {
                        let old = layers::composed_detail(&clay);
                        Some(layers::recompose(&clay, name, if enabled { amount } else { 0.0 }, old.as_ref())?)
                    }
                    _ => None,
                };
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                let s = ledger::stack_mut(&mut clay);
                (s.layers[k].amount, s.layers[k].enabled) = (amount, enabled);
                if let Some(c) = planted {
                    layers::plant(&clay, c);
                    extra["composite_ms"] = json!((ms * 100.0).round() / 100.0);
                }
                if l.kind == Kind::Sculpt && amount.abs() < layers::MIN_AMOUNT {
                    extra["note"] = json!(format!("strokes on {name} are refused while its amount is below {} (they are stored divided by it)", layers::MIN_AMOUNT));
                }
            }
            "rename" => {
                let name = name_arg(args, "name")?;
                let to = name_arg(args, "to")?;
                let k = extra_index(&clay, name, "renamed")?;
                layers::valid_name(to)?;
                taken(&clay, to)?;
                let s = ledger::stack_mut(&mut clay);
                s.layers[k].name = to.to_string();
                if s.record_layer.as_deref() == Some(name) {
                    s.record_layer = Some(to.to_string());
                }
                reselect = Some((name.to_string(), Some(to.to_string())));
            }
            "move" => {
                let name = name_arg(args, "name")?;
                let k = extra_index(&clay, name, "moved")?;
                let n = layers::extra(&clay).len();
                let to = args["to"].as_u64().ok_or("move needs to: the layer's new place above base and paint (1 = just above them)")? as usize;
                let to = to.clamp(1, n) - 1;
                let s = ledger::stack_mut(&mut clay);
                let l = s.layers.remove(k);
                s.layers.insert(to, l);
            }
            "bake" | "merge_down" => {
                let name = name_arg(args, "name")?;
                let k = extra_index(&clay, name, "folded")?;
                let kind = layers::extra(&clay)[k].kind;
                let bottom = if kind == Kind::Sculpt { layers::BASE } else { layers::PAINT };
                let into = match action {
                    "bake" => bottom.to_string(),
                    _ => layers::extra(&clay)[..k].iter().rev().find(|l| l.kind == kind).map_or_else(|| bottom.to_string(), |l| l.name.clone()),
                };
                layers::fold(&mut clay, name, &into)?;
                extra["into"] = json!(into);
                extra["archived"] = json!(format!("{name}'s records are kept for reading: stroke_log {{layer: \"archived:{name}\"}}"));
            }
            "delete" => {
                let name = name_arg(args, "name")?;
                let k = extra_index(&clay, name, "deleted")?;
                let s = ledger::stack_mut(&mut clay);
                s.layers.remove(k);
                if s.record_layer.as_deref() == Some(name) {
                    s.record_layer = None;
                }
                reselect = Some((name.to_string(), None));
            }
            "split_by_mask" => {
                let name = args["name"].as_str().unwrap_or(layers::BASE);
                if layers::kind_of(&clay, name)? != Kind::Sculpt {
                    return Err(format!("{name} is a paint layer; split_by_mask splits sculpt layers"));
                }
                let mask = clay.sculpt_mask.clone().ok_or("the clay has no sculpt mask: mask the part to split off first (mask tool)")?;
                let to = args["to"].as_str().map_or_else(|| format!("{name} masked"), str::to_string);
                layers::valid_name(&to)?;
                taken(&clay, &to)?;
                if layers::extra(&clay).len() + 2 >= layers::MAX_LAYERS {
                    return Err(format!("a clay has at most {} layers", layers::MAX_LAYERS));
                }
                let grid = layers::grid_of::<Detail>(&clay, name)?.ok_or_else(|| format!("{name} holds no sculpting to split"))?;
                let m = mask.grid();
                let masked = grid.map_points(|p, v| v * m.value(p));
                let kept = grid.map_points(|p, v| v * (1.0 - m.value(p)));
                let amount = layers::layer(&clay, name).map_or(1.0, |l| l.amount);
                let enabled = layers::layer(&clay, name).is_none_or(|l| l.enabled);
                layers::set_grid::<Detail>(&mut clay, name, Some(kept));
                let at = layers::extra(&clay).iter().position(|l| l.name == name).map_or(0, |k| k + 1);
                let mut l = Layer::new(&to, Kind::Sculpt, amount);
                l.enabled = enabled;
                l.detail = Some(masked);
                ledger::stack_mut(&mut clay).layers.insert(at, l);
                extra["made"] = json!(to);
            }
            "record" => {
                let name = match &args["name"] {
                    Value::Null => None,
                    Value::String(n) if n == layers::BASE => None,
                    Value::String(n) => {
                        if layers::kind_of(&clay, n)? != Kind::Sculpt {
                            return Err(format!("{n} is a paint layer; record names the sculpt layer sculpt calls without layer write"));
                        }
                        Some(n.clone())
                    }
                    _ => return Err("name must be a sculpt layer's name (or null: base)".into()),
                };
                ledger::stack_mut(&mut clay).record_layer = name;
            }
            other => return Err(format!("unknown action {other}; use add, list, select, amount, enable, disable, rename, move, bake, merge_down, delete, split_by_mask or record")),
        }
        self.put_layered(id, clay)?;
        if let Some((from, to)) = reselect
            && self.layer_selection.get(&id).is_some_and(|n| *n == from)
        {
            match to {
                Some(to) => self.layer_selection.insert(id, to),
                None => self.layer_selection.remove(&id),
            };
        }
        extra["step"] = json!(self.history.current() + 1);
        self.layers_reply(id, extra)
    }

    /// `morph`.
    pub(super) fn morph_tool(&mut self, args: &Value) -> ToolResult {
        let (id, _) = self.clay_of(args)?;
        let action = args["action"].as_str().unwrap_or("list");
        let mut clay = self.layered_clay(id)?;
        let find = |clay: &Clay, name: &str| -> Result<layers::Morph, String> {
            clay.sculpt.as_ref().and_then(|s| s.data().morphs.iter().find(|m| m.name == name).cloned()).ok_or_else(|| format!("no morph {name} (morph list)"))
        };
        let morph_json = |clay: &Clay, m: &layers::Morph| {
            let mut d = layers::distance(layers::composed_detail(clay).as_ref(), m.composed().as_ref());
            d["name"] = json!(m.name);
            d["layers"] = json!(m.layers.iter().map(|l| &l.name).collect::<Vec<_>>());
            d["blobs_changed"] = json!(m.context != ledger::clay_context(clay));
            d
        };
        let mut extra = json!({});
        match action {
            "list" => {
                let list: Vec<Value> = clay.sculpt.as_ref().map(|s| s.data().morphs.iter().map(|m| morph_json(&clay, m)).collect()).unwrap_or_default();
                return Ok(text(json!({ "target": id, "morphs": list, "units": "max, mean and rms: how far each morph's surface is from the clay's (scene units of field offset)" })));
            }
            "diff" => {
                let name = name_arg(args, "name")?;
                let m = find(&clay, name)?;
                return Ok(text(json!({ "target": id, "morph": morph_json(&clay, &m) })));
            }
            "store" => {
                let name = name_arg(args, "name")?;
                layers::valid_name(name)?;
                let snap = layers::snapshot(&clay, name);
                let s = ledger::stack_mut(&mut clay);
                match s.morphs.iter().position(|m| m.name == name) {
                    Some(i) => s.morphs[i] = snap,
                    None if s.morphs.len() >= layers::MAX_MORPHS => return Err(format!("a clay keeps at most {} morphs: delete one first", layers::MAX_MORPHS)),
                    None => s.morphs.push(snap),
                }
            }
            "switch" => {
                let name = name_arg(args, "name")?;
                let m = find(&clay, name)?;
                let old = layers::apply_morph(&mut clay, &m)?;
                let s = ledger::stack_mut(&mut clay);
                if let Some(slot) = s.morphs.iter_mut().find(|x| x.name == name) {
                    *slot = old;
                }
                extra["next"] = json!(format!("The clay shows {name}; what it showed is stored as {name}, so switching again goes back."));
            }
            "blend" => {
                let mut list: Vec<(layers::Morph, f32)> = Vec::new();
                if let Some(w) = args["weights"].as_object() {
                    if w.is_empty() || w.len() > layers::MAX_MORPHS {
                        return Err("weights names 1 or more morphs: {name: weight 0..1}".into());
                    }
                    for (n, v) in w {
                        list.push((find(&clay, n)?, v.as_f64().ok_or("weights are numbers 0..1")? as f32));
                    }
                } else {
                    let name = name_arg(args, "name")?;
                    list.push((find(&clay, name)?, args["weight"].as_f64().ok_or("blend needs weight 0..1 (or weights {name: weight})")? as f32));
                }
                let refs: Vec<(&layers::Morph, f32)> = list.iter().map(|(m, w)| (m, *w)).collect();
                layers::blend(&mut clay, &refs)?;
            }
            "delete" => {
                let name = name_arg(args, "name")?;
                let s = ledger::stack_mut(&mut clay);
                let i = s.morphs.iter().position(|m| m.name == name).ok_or_else(|| format!("no morph {name}"))?;
                s.morphs.remove(i);
            }
            other => return Err(format!("unknown action {other}; use store, switch, blend, delete, list or diff")),
        }
        self.put_layered(id, clay)?;
        let clay = self.scene.get(id).and_then(|o| o.clay.as_ref()).expect("put back");
        let list: Vec<Value> = clay.sculpt.as_ref().map(|s| s.data().morphs.iter().map(|m| morph_json(clay, m)).collect()).unwrap_or_default();
        let mut out = json!({ "target": id, "action": action, "morphs": list, "step": self.history.current() + 1 });
        if let Value::Object(m) = extra {
            for (k, v) in m {
                out[k] = v;
            }
        }
        Ok(text(out))
    }
}

#[cfg(test)]
mod tests;
