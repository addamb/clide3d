//! Polygroups for clay (docs/SCULPT_PLAN.md 3.5, Stage 3): a sparse grid
//! of u16 ids on the clay's surface band, with a name and a visibility
//! per id. Spatial labels, so they survive remeshing; there are no group
//! edges for loop operations (that waits for explicit topology).
//!
//! Brushes skip hidden groups (as they skip masked surface, see
//! `sculpt::engine::Gate`) and strokes may keep to some groups
//! (`mask.groups`). The viewport drops hidden groups' triangles and can
//! tint groups (`set_display overlay: groups`). The `group` tool's
//! actions are records on the clay's group ledger (layer "groups").

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use glam::{IVec3, Vec3};
use serde::{Deserialize, Serialize};

use crate::band::Band;
use crate::clay::{BRICK_VALUES, Clay, MAX_BRICKS, split};
use crate::ledger::Hash128;

type Brick = Arc<[u16; BRICK_VALUES]>;

/// Most groups a clay may have.
pub const MAX_GROUPS: usize = 4096;
/// Longest group name.
pub const MAX_NAME: usize = 64;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct GroupInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub visible: bool,
}

fn yes() -> bool {
    true
}

fn is_yes(b: &bool) -> bool {
    *b
}

/// Shared copy-on-write and compared by identity, like `clay::Detail`.
#[derive(Clone, Debug)]
pub struct Groups(Arc<GroupData>);

#[derive(Clone, Debug, Default)]
pub struct GroupData {
    pub voxel: f32,
    bricks: HashMap<[i32; 3], Brick>,
    /// Every id the grid holds (0 is no group and has none).
    pub info: BTreeMap<u16, GroupInfo>,
}

impl PartialEq for Groups {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Groups {
    pub fn new(voxel: f32) -> Self {
        Groups(Arc::new(GroupData { voxel, bricks: HashMap::new(), info: BTreeMap::new() }))
    }

    pub fn addr(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }

    pub fn data(&self) -> &GroupData {
        &self.0
    }

    pub fn data_mut(&mut self) -> &mut GroupData {
        Arc::make_mut(&mut self.0)
    }

    /// The group at `p`: the nearest labelled grid point among the 8
    /// around it (0: none).
    pub fn label(&self, p: Vec3) -> u16 {
        self.0.label(p)
    }

    pub fn hidden(&self) -> Vec<u16> {
        self.0.info.iter().filter(|(_, i)| !i.visible).map(|(id, _)| *id).collect()
    }

    pub fn mirrored_x(&self) -> Groups {
        let mut out = GroupData { voxel: self.0.voxel, bricks: HashMap::new(), info: self.0.info.clone() };
        for (key, brick) in &self.0.bricks {
            for (at, &v) in brick.iter().enumerate().filter(|(_, v)| **v != 0) {
                let l = [at as i32 % 8, at as i32 / 8 % 8, at as i32 / 64];
                out.set([-(key[0] * 8 + l[0]), key[1] * 8 + l[1], key[2] * 8 + l[2]], v);
            }
        }
        Groups(Arc::new(out))
    }

    pub fn is_sane(&self) -> bool {
        let g = &self.0;
        g.voxel.is_finite() && (1e-5..=1e3).contains(&g.voxel) && g.bricks.len() <= MAX_BRICKS && g.info.len() <= MAX_GROUPS && !g.info.contains_key(&0)
            && g.info.values().all(|i| i.name.chars().count() <= MAX_NAME) && g.bricks.keys().all(|k| k.iter().all(|c| c.abs() < 1 << 20))
    }
}

impl GroupData {
    pub fn get(&self, i: [i32; 3]) -> u16 {
        let (key, at) = split(i);
        self.bricks.get(&key).map_or(0, |b| b[at])
    }

    pub fn set(&mut self, i: [i32; 3], v: u16) {
        let (key, at) = split(i);
        if v == 0 && !self.bricks.contains_key(&key) {
            return;
        }
        Arc::make_mut(self.bricks.entry(key).or_insert_with(|| Arc::new([0; BRICK_VALUES])))[at] = v;
    }

    pub(crate) fn bricks(&self) -> impl Iterator<Item = (&[i32; 3], &Brick)> {
        self.bricks.iter()
    }

    pub(crate) fn brick(&self, key: &[i32; 3]) -> Option<&Brick> {
        self.bricks.get(key)
    }

    fn prune(&mut self) {
        self.bricks.retain(|_, b| b.iter().any(|v| *v != 0));
        let used: std::collections::BTreeSet<u16> = self.bricks.values().flat_map(|b| b.iter().copied()).filter(|v| *v != 0).collect();
        self.info.retain(|id, _| used.contains(id));
    }

    pub fn label(&self, p: Vec3) -> u16 {
        if self.bricks.is_empty() || !p.is_finite() {
            return 0;
        }
        let g = p / self.voxel;
        if g.abs().max_element() > (1 << 28) as f32 {
            return 0;
        }
        let r = g.round().as_ivec3();
        let v = self.get(r.to_array());
        if v != 0 {
            return v;
        }
        let f = g.floor();
        let mut corners: Vec<(f32, [i32; 3])> = (0..8)
            .map(|n| {
                let c = f + Vec3::new((n & 1) as f32, ((n >> 1) & 1) as f32, ((n >> 2) & 1) as f32);
                (c.distance_squared(g), c.as_ivec3().to_array())
            })
            .collect();
        corners.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        corners.into_iter().map(|(_, c)| self.get(c)).find(|v| *v != 0).unwrap_or(0)
    }

    /// The ids in use with the band points each holds.
    pub fn counts(&self) -> BTreeMap<u16, usize> {
        let mut out = BTreeMap::new();
        for b in self.bricks.values() {
            for v in b.iter().filter(|v| **v != 0) {
                *out.entry(*v).or_insert(0) += 1;
            }
        }
        out
    }

    fn next_id(&self) -> u16 {
        let used = self.counts();
        (1..=u16::MAX).find(|id| !used.contains_key(id) && !self.info.contains_key(id)).unwrap_or(u16::MAX)
    }
}

/// File form: two bytes per grid point, deflated, and the ids' names.
#[derive(Serialize, Deserialize)]
struct GroupFile {
    voxel: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    packed: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", deserialize_with = "info_map")]
    info: BTreeMap<u16, GroupInfo>,
}

fn info_map<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<u16, GroupInfo>, D::Error> {
    crate::brush::alpha::capped_map::<D, u16, GroupInfo, MAX_GROUPS>(d)
}

impl Serialize for Groups {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let g = &self.0;
        let mut keys: Vec<[i32; 3]> = g.bricks.keys().copied().collect();
        keys.sort();
        let values: Vec<u8> = keys.iter().flat_map(|k| g.bricks[k].iter().flat_map(|v| v.to_le_bytes())).collect();
        let packed = (!keys.is_empty()).then(|| crate::clay::pack(&keys, &values, 2, false));
        GroupFile { voxel: g.voxel, packed, info: g.info.clone() }.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Groups {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        crate::clay::in_inflate_budget_with(d, |d| {
            let file = GroupFile::deserialize(d)?;
            let mut bricks = HashMap::new();
            if let Some(text) = file.packed {
                let (keys, values) = crate::clay::unpack(&text, 2, false, MAX_BRICKS).map_err(|e| serde::de::Error::custom(format!("group bricks: {e}")))?;
                for (at, bytes) in keys.into_iter().zip(values.chunks_exact(BRICK_VALUES * 2)) {
                    let mut b = [0u16; BRICK_VALUES];
                    for (v, c) in b.iter_mut().zip(bytes.chunks_exact(2)) {
                        *v = u16::from_le_bytes([c[0], c[1]]);
                    }
                    bricks.insert(at, Arc::new(b));
                }
            }
            Ok(Groups(Arc::new(GroupData { voxel: file.voxel, bricks, info: file.info })))
        })
    }
}

/// A group action as a record keeps it (the `group` tool's actions but
/// paint, which records strokes, and list).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum GroupOp {
    /// One group per blob that makes surface (mirrored blobs apart).
    FromBlobs,
    /// Where the mask (the grid by hash: `ledger::Frozen`) is at least
    /// `threshold`, group `id` (None: a new one).
    FromMask { threshold: f32, id: Option<u16>, mask: Hash128 },
    /// Regions whose normals stay within `angle` degrees of where each
    /// started.
    FromNormals { angle: f32 },
    Rename { id: u16, name: String },
    Show { ids: Vec<u16> },
    Hide { ids: Vec<u16> },
    /// Shows these and hides every other group.
    Isolate { ids: Vec<u16> },
    ShowAll,
    Clear,
}

impl GroupOp {
    pub fn validate(&self) -> Result<(), String> {
        let ok = |c: bool, m: &str| if c { Ok(()) } else { Err(m.to_string()) };
        match self {
            GroupOp::FromMask { threshold, id, .. } => ok(threshold.is_finite() && (0.0..=1.0).contains(threshold) && *id != Some(0), "from_mask needs threshold 0 to 1 and an id above 0"),
            GroupOp::FromNormals { angle } => ok(angle.is_finite() && (1.0..=180.0).contains(angle), "from_normals needs angle 1 to 180 degrees"),
            GroupOp::Rename { id, name } => ok(*id != 0 && name.chars().count() <= MAX_NAME, &format!("rename needs an id above 0 and a name of at most {MAX_NAME} characters")),
            GroupOp::Show { ids } | GroupOp::Hide { ids } | GroupOp::Isolate { ids } => ok(!ids.is_empty() && ids.len() <= MAX_GROUPS && !ids.contains(&0), "pass ids: group ids above 0"),
            _ => Ok(()),
        }
    }
}

/// The group cell a clay uses (its detail cell, as the mask does).
pub fn voxel_of(clay: &Clay) -> Result<f32, String> {
    if let Some(g) = &clay.groups {
        return Ok(g.data().voxel);
    }
    clay.detail_cell.or_else(|| clay.detail.as_ref().map(|d| d.voxel())).or_else(|| clay.cell_size()).ok_or_else(|| "add blobs before grouping".into())
}

fn finish(mut g: Groups) -> Result<Option<Groups>, String> {
    let d = g.data_mut();
    d.prune();
    if d.bricks.len() > MAX_BRICKS {
        return Err(format!("too many group bricks (over {MAX_BRICKS})"));
    }
    if d.info.len() > MAX_GROUPS {
        return Err(format!("more than {MAX_GROUPS} groups; use a larger angle or fewer blobs"));
    }
    Ok((!d.bricks.is_empty()).then_some(g))
}

/// Applies `op` to `groups` (None: none) on `clay`; `mask` resolves a
/// from_mask snapshot.
pub fn act(clay: &Clay, groups: Option<&Groups>, op: &GroupOp, mask: &dyn Fn(Hash128) -> Option<crate::mask::SculptMask>) -> Result<Option<Groups>, String> {
    op.validate()?;
    let voxel = voxel_of(clay)?;
    let mut g = groups.cloned().unwrap_or_else(|| Groups::new(voxel));
    let known = |g: &Groups, ids: &[u16]| ids.iter().find(|id| !g.data().info.contains_key(id)).map_or(Ok(()), |id| Err(format!("no group {id} (group list)")));
    match op {
        GroupOp::Clear => return Ok(None),
        GroupOp::Rename { id, name } => {
            known(&g, &[*id])?;
            g.data_mut().info.get_mut(id).expect("known").name = name.clone();
            return Ok(Some(g));
        }
        GroupOp::Show { ids } | GroupOp::Hide { ids } => {
            known(&g, ids)?;
            let show = matches!(op, GroupOp::Show { .. });
            for id in ids {
                g.data_mut().info.get_mut(id).expect("known").visible = show;
            }
            return Ok(Some(g));
        }
        GroupOp::Isolate { ids } => {
            known(&g, ids)?;
            for (id, info) in g.data_mut().info.iter_mut() {
                info.visible = ids.contains(id);
            }
            return Ok(Some(g));
        }
        GroupOp::ShowAll => {
            for info in g.data_mut().info.values_mut() {
                info.visible = true;
            }
            return Ok(groups.is_some().then_some(g));
        }
        _ => {}
    }
    if g.data().voxel != voxel {
        return Err("the groups' cell changed; clear the groups first".into());
    }
    let field = crate::band::field(clay, voxel);
    let band = Band::with(clay, &field, voxel)?;
    match op {
        GroupOp::FromBlobs => {
            // Group n + 1 is blob n (`Clay::blobs`), both sides of a
            // mirrored blob in one: the field counts prims, and a mirrored
            // blob makes two.
            let blob_of = clay.prim_blobs();
            let owners = band.per_point(&field, |p, _| field.owner_index(p).and_then(|i| blob_of.get(i)).map_or(0u16, |b| *b as u16 + 1));
            let d = g.data_mut();
            (d.bricks, d.info) = (HashMap::new(), BTreeMap::new());
            for (i, id) in band.keys.iter().zip(&owners) {
                d.set(*i, *id);
            }
            let ids: std::collections::BTreeSet<u16> = owners.iter().copied().filter(|v| *v != 0).collect();
            for id in ids {
                let name = clay.blobs.get(id as usize - 1).map(|b| b.name.clone()).unwrap_or_default();
                d.info.insert(id, GroupInfo { name: if name.is_empty() { format!("blob {}", id - 1) } else { name }, visible: true });
            }
        }
        GroupOp::FromMask { threshold, id, mask: hash } => {
            let m = mask(*hash).ok_or("the mask this record read is gone from the clay; group from the mask again")?;
            let id = id.unwrap_or_else(|| g.data().next_id());
            let d = g.data_mut();
            let mut any = false;
            for i in &band.keys {
                if m.grid().value(IVec3::from(*i).as_vec3() * voxel) >= *threshold {
                    d.set(*i, id);
                    any = true;
                }
            }
            if any {
                d.info.entry(id).or_insert(GroupInfo { name: format!("group {id}"), visible: true });
            }
        }
        GroupOp::FromNormals { angle } => {
            let normals = band.normals(&field);
            let cos = angle.to_radians().cos();
            let mut label = vec![0u16; band.len()];
            let mut next = 0u16;
            for s in 0..band.len() {
                if label[s] != 0 {
                    continue;
                }
                if next as usize >= MAX_GROUPS {
                    return Err(format!("from_normals makes more than {MAX_GROUPS} groups; use a larger angle"));
                }
                next += 1;
                label[s] = next;
                let mut stack = vec![s];
                while let Some(k) = stack.pop() {
                    for n in band.neighbours(k).into_iter().flatten() {
                        if label[n] == 0 && normals[n].dot(normals[s]) >= cos {
                            label[n] = next;
                            stack.push(n);
                        }
                    }
                }
            }
            let d = g.data_mut();
            (d.bricks, d.info) = (HashMap::new(), BTreeMap::new());
            for (i, id) in band.keys.iter().zip(&label) {
                d.set(*i, *id);
            }
            for id in 1..=next {
                d.info.insert(id, GroupInfo { name: format!("group {id}"), visible: true });
            }
        }
        _ => unreachable!("handled above"),
    }
    finish(g)
}

/// Group painting: band points the stroke covers at half weight or more
/// take its id.
pub fn paint(clay: &Clay, groups: Option<&Groups>, stroke: &crate::brush::resolve::Resolved) -> Result<Option<Groups>, String> {
    let id = stroke.group.filter(|id| *id != 0).ok_or("a group stroke needs an id above 0")?;
    let voxel = voxel_of(clay)?;
    let field = crate::band::field(clay, voxel);
    let width = crate::band::width(clay, voxel);
    let carved = crate::layers::surface_detail(clay).map_or(0.0, |d| d.grid().max_abs());
    let mut g = groups.cloned().unwrap_or_else(|| Groups::new(voxel));
    if g.data().voxel != voxel {
        return Err("the groups' cell changed; clear the groups first".into());
    }
    let alpha = crate::sculpt::engine::alpha_of(clay, &stroke.params)?;
    for dab in &stroke.dabs {
        dab.check()?;
        let marks = crate::mask::marks(clay, &field, voxel, width + carved, width, dab, &stroke.params, alpha.as_ref(), stroke.mask.as_ref())?;
        let d = g.data_mut();
        for (i, w) in marks {
            if w * stroke.params.strength.clamp(0.0, 1.0) >= 0.5 {
                d.set(i, id);
            }
        }
    }
    let d = g.data_mut();
    if d.counts().contains_key(&id) {
        d.info.entry(id).or_insert(GroupInfo { name: format!("group {id}"), visible: true });
    }
    finish(g)
}
