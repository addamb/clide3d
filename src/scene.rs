//! Scene data. This is exactly what gets saved to / loaded from JSON.
//!
//! Objects form a hierarchy through `parent`. Each object's local matrix maps
//! its own object space into its parent's object space (or world space for
//! roots), Maya-style with a pivot:
//!
//!   local = T(translation + pivot_comp + pivot) · R · S · T(-pivot)
//!
//! `pivot_comp` plays the role of Maya's hidden rotate/scale pivot translate:
//! moving the pivot updates it so the object stays put and `translation`
//! (what the Channel Box shows) does not jump.

use std::collections::{HashMap, HashSet};

use glam::{EulerRot, Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::clay::Clay;
use crate::eyeball::Eye;
use crate::material::Material;
use crate::rig::Rig;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum Shape {
    Cube,
    Sphere,
    Cylinder,
    Cone,
    Plane,
    Torus,
    /// An empty transform used to group other objects (no mesh).
    Group,
    /// Sculpted clay: its mesh is built from `Object::clay` (see `clay.rs`).
    Clay,
    /// An eyeball with iris and pupil, built from `Object::eye` (see `eyeball.rs`).
    Eye,
    /// Hair cards grown along guide curves, from `Object::hair` (see `hair.rs`).
    Hair,
    Cloth,
    Fire,
    /// A dungeon kit piece, rock or prop, from `Object::kit` (see `envkit/kit.rs`).
    Kit,
    /// A heightfield, from `Object::terrain` (see `envkit/terrain.rs`).
    Terrain,
    /// Copies of other objects, from `Object::scatter` (see `envkit/scatter.rs`).
    /// It has no mesh of its own: its instances draw their sources' meshes.
    Scatter,
    /// A kitbash part (bolt, vent, piston...), from `Object::part` (see `kitbash`).
    Part,
    /// An imported triangle mesh, from `Object::mesh` (see `mesh_asset.rs`;
    /// `import_mesh`, `to_clay` turns it into clay).
    Mesh,
    /// A polygon cage with subdivision levels, from `Object::poly` (see
    /// `poly/`; `poly from_clay` makes one).
    Poly,
}

impl Shape {
    /// Mesh primitives (everything except `Group`).
    pub const ALL: [Shape; 6] = [
        Shape::Cube,
        Shape::Sphere,
        Shape::Cylinder,
        Shape::Cone,
        Shape::Plane,
        Shape::Torus,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Shape::Cube => "Cube",
            Shape::Sphere => "Sphere",
            Shape::Cylinder => "Cylinder",
            Shape::Cone => "Cone",
            Shape::Plane => "Plane",
            Shape::Torus => "Torus",
            Shape::Group => "Group",
            Shape::Clay => "Clay",
            Shape::Eye => "Eye",
            Shape::Hair => "Hair",
            Shape::Cloth => "Cloth",
            Shape::Fire => "Fire",
            Shape::Kit => "Kit",
            Shape::Terrain => "Terrain",
            Shape::Scatter => "Scatter",
            Shape::Part => "Part",
            Shape::Mesh => "Mesh",
            Shape::Poly => "Poly",
        }
    }

    pub fn is_mesh(self) -> bool {
        !matches!(self, Shape::Group | Shape::Scatter)
    }

    /// Shapes whose mesh is built for the object itself (not a shared primitive).
    pub fn own_mesh(self) -> bool {
        matches!(self, Shape::Clay | Shape::Eye | Shape::Hair | Shape::Cloth | Shape::Fire | Shape::Kit | Shape::Terrain | Shape::Part | Shape::Mesh | Shape::Poly)
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Object {
    pub id: u64,
    pub name: String,
    pub shape: Shape,
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
    /// sRGB color, 0..1.
    pub color: [f32; 3],
    pub visible: bool,
    #[serde(default)]
    pub parent: Option<u64>,
    /// Pivot point in the object's own space.
    #[serde(default)]
    pub pivot: Vec3,
    /// Keeps the object in place when the pivot moves (see module docs).
    #[serde(default)]
    pub pivot_comp: Vec3,
    /// Blobs for `Shape::Clay` objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clay: Option<Clay>,
    /// Skeleton and animation for `Shape::Clay` objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rig: Option<Rig>,
    /// Follow this bone of the parent's rig (the parent must be rigged
    /// clay): a held prop, a hat. The local values are in the parent's
    /// rest-pose space, so the object sits where it was placed at rest and
    /// moves rigidly with the bone in every pose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bone: Option<String>,
    /// Material name (scene library or preset). For clay it is the default
    /// for blobs that name none; eyes bring their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    /// Damage stamps for kit/primitive meshes (clay keeps stamps in its field).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub damage: Vec<crate::smart::Decal>,
    /// Iris, pupil and cornea settings for `Shape::Eye` objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eye: Option<Eye>,
    /// Guide curves and look for `Shape::Hair` objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hair: Option<crate::hair::Hair>,
    /// Clay only: export (and show, with Show > Baked) as a baked game mesh
    /// with textures, made with these settings (see `bake.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bake: Option<crate::bake::Settings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloth: Option<crate::cloth::Cloth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire: Option<crate::fire::Fire>,
    /// Settings of a `Shape::Kit` piece.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kit: Option<crate::envkit::kit::Kit>,
    /// Settings and brush grids of a `Shape::Terrain`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terrain: Option<crate::envkit::terrain::Terrain>,
    /// Sources and instances of a `Shape::Scatter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scatter: Option<crate::envkit::scatter::Scatter>,
    /// A light this object gives off (any shape; torches, candles and
    /// braziers come with one). Film renders light the scene with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<crate::envkit::light::LightSource>,
    /// Settings of a `Shape::Part` (a kitbash part).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<crate::kitbash::Part>,
    /// The kitbash group this object was added with (see `kitbash::place`):
    /// removing the group removes every object carrying its tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kitbash: Option<String>,
    /// Kitbash groups: the object whose surface the parts sit on. When it
    /// moves (or is re-posed, or mirrored) the group follows it (see `pose.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow: Option<crate::pose::Follow>,
    /// A linked mirror copy: kept the reflection of its source object
    /// (shape, materials, attached parts) until unlinked (see `pose.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<crate::pose::Link>,
    /// The triangles of a `Shape::Mesh` (an import).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<crate::mesh_asset::MeshAsset>,
    /// The cage and levels of a `Shape::Poly`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poly: Option<crate::poly::Poly>,
    /// A reference image plane (`Shape::Plane`, Quality Stage 5a): one
    /// view of a turnaround sheet. Not geometry: see `is_reference`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<crate::refplane::RefPlane>,
}

impl Object {
    /// A reference image plane: drawn in the viewport and `render_views`,
    /// left out of exports, print, film (unless `show_references`),
    /// silhouettes, close-ups, framing and picking.
    pub fn is_reference(&self) -> bool {
        self.reference.is_some()
    }

    pub fn local_matrix(&self) -> Mat4 {
        Mat4::from_translation(self.translation + self.pivot_comp + self.pivot)
            * Mat4::from_quat(self.rotation)
            * Mat4::from_scale(self.scale)
            * Mat4::from_translation(-self.pivot)
    }

    /// Sets translation/rotation/scale so `local_matrix()` equals `m`
    /// (shear, if any, is dropped).
    pub fn set_local_matrix(&mut self, m: Mat4) {
        let (s, r, full) = m.to_scale_rotation_translation();
        self.scale = s;
        self.rotation = r.normalize();
        self.translation = full - self.pivot_comp - self.pivot + r * (s * self.pivot);
    }

    /// Rotation in degrees, Maya's default "xyz" rotate order (X applied first).
    pub fn euler_degrees(&self) -> Vec3 {
        let (z, y, x) = self.rotation.to_euler(EulerRot::ZYX);
        // `+ 0.0` turns -0.0 into 0.0 so panels never show "-0.0".
        Vec3::new(x, y, z) * (180.0 / std::f32::consts::PI) + Vec3::ZERO
    }

    pub fn set_euler_degrees(&mut self, deg: Vec3) {
        let r = deg * (std::f32::consts::PI / 180.0);
        self.rotation = Quat::from_euler(EulerRot::ZYX, r.z, r.y, r.x);
    }
}

/// Deepest allowed hierarchy. Edits that would go deeper are refused, and
/// loaded files are cut back to it, so no tree walk can run away.
pub const MAX_DEPTH: usize = 256;

/// Ids above this in a loaded file trigger renumbering (keeps `next_id` safe).
const MAX_ID: u64 = 1 << 52;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Scene {
    /// Read within one inflate budget for all their grids.
    #[serde(deserialize_with = "crate::clay::in_inflate_budget")]
    pub objects: Vec<Object>,
    pub next_id: u64,
    /// Materials defined for this scene; they override presets of the same name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub materials: Vec<Material>,
    /// Alpha images for sculpt strokes, by name (`alpha import`); clays keep
    /// copies of the ones their strokes use (see `brush::alpha`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty", deserialize_with = "crate::brush::alpha::library")]
    pub alphas: std::collections::BTreeMap<String, crate::brush::alpha::Alpha>,
    /// Brush presets saved in the scene (`brush_preset save`); the built-in
    /// ones are compiled in (see `brush::preset`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty", deserialize_with = "crate::brush::preset::library")]
    pub brushes: std::collections::BTreeMap<String, crate::brush::preset::Preset>,
    /// Time-slider frame that bone-attached objects follow (None = rest).
    /// Display state only: not saved and not compared, so it is never an edit.
    #[serde(skip)]
    pub frame: Option<f32>,
    /// Proportion guides (`guides`, Quality Stage 5b): head units, lines and
    /// targets. Not objects: drawn over views, never exported.
    #[serde(default, skip_serializing_if = "crate::guides::Guides::is_empty", deserialize_with = "crate::guides::lenient")]
    pub guides: crate::guides::Guides,
}

impl PartialEq for Scene {
    fn eq(&self, other: &Self) -> bool {
        self.objects == other.objects && self.next_id == other.next_id && self.materials == other.materials && self.alphas == other.alphas && self.brushes == other.brushes && self.guides == other.guides
    }
}

impl Scene {
    /// Adds a primitive with a Maya-style name (pCube1, pSphere2, group1, ...). Returns its id.
    pub fn add(&mut self, shape: Shape) -> u64 {
        let id = self.alloc_id();
        let name = match shape {
            Shape::Group => self.unique_name("group"),
            Shape::Clay => self.unique_name("clay"),
            Shape::Eye => self.unique_name("eye"),
            Shape::Hair => self.unique_name("hair"),
            Shape::Cloth => self.unique_name("cloth"),
            Shape::Fire => self.unique_name("fire"),
            Shape::Kit => self.unique_name("kit"),
            Shape::Terrain => self.unique_name("terrain"),
            Shape::Scatter => self.unique_name("scatter"),
            Shape::Part => self.unique_name("part"),
            Shape::Mesh => self.unique_name("mesh"),
            Shape::Poly => self.unique_name("poly"),
            _ => self.unique_name(&format!("p{}", shape.label())),
        };
        // Lift solids so they sit on the grid; a plane lies flat on it.
        // Clay blobs are placed in the object's space, so it starts at the origin.
        let y = if shape.is_mesh() && !shape.own_mesh() && shape != Shape::Plane { 0.5 } else { 0.0 };
        let scale = if shape == Shape::Plane { Vec3::splat(4.0) } else { Vec3::ONE };
        self.objects.push(Object {
            id,
            name,
            shape,
            translation: Vec3::new(0.0, y, 0.0),
            rotation: Quat::IDENTITY,
            scale,
            // Clay shows its blob colors, so its tint starts white.
            color: if shape.own_mesh() { [1.0; 3] } else { [0.72, 0.72, 0.72] },
            visible: true,
            parent: None,
            pivot: Vec3::ZERO,
            pivot_comp: Vec3::ZERO,
            clay: (shape == Shape::Clay).then(Clay::default),
            rig: None,
            bone: None,
            material: None,
            damage: Vec::new(),
            eye: (shape == Shape::Eye).then(Eye::default),
            hair: (shape == Shape::Hair).then(crate::hair::Hair::default),
            bake: None,
            cloth: None,
            fire: None,
            kit: (shape == Shape::Kit).then(|| crate::envkit::kit::Kit::new(crate::envkit::kit::Piece::Wall)),
            terrain: (shape == Shape::Terrain).then(crate::envkit::terrain::Terrain::default),
            scatter: (shape == Shape::Scatter).then(crate::envkit::scatter::Scatter::default),
            light: None,
            part: (shape == Shape::Part).then(|| crate::kitbash::Part::new(crate::kitbash::Kind::Bolt)),
            kitbash: None,
            follow: None,
            link: None,
            mesh: None,
            poly: None,
            reference: None,
        });
        id
    }

    /// The object's material resolved against this scene's library.
    pub fn material_of(&self, o: &Object) -> Material {
        crate::damage::world_material(Material::resolve(&self.materials, o.material.as_deref().unwrap_or("")),self.world_matrix(o.id))
    }

    /// `material_of` packed for the shader.
    pub fn surface_of(&self, o: &Object) -> [f32; 4] {
        self.material_of(o).surface()
    }

    /// Garments follow the parent rig in the parent's rest coordinates.
    pub fn rig_of<'a>(&'a self, o: &'a Object) -> Option<&'a Rig> {
        if o.shape == Shape::Cloth { o.parent.and_then(|id| self.get(id)).and_then(|p| p.rig.as_ref()) } else { o.rig.as_ref() }
    }

    pub fn get(&self, id: u64) -> Option<&Object> {
        self.objects.iter().find(|o| o.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Object> {
        self.objects.iter_mut().find(|o| o.id == id)
    }

    // ---------- hierarchy queries ----------

    fn index(&self) -> HashMap<u64, usize> {
        self.objects.iter().enumerate().map(|(i, o)| (o.id, i)).collect()
    }

    /// Children of every object (and of the world, key None), built in one pass.
    fn child_map(&self) -> HashMap<Option<u64>, Vec<u64>> {
        let mut map: HashMap<Option<u64>, Vec<u64>> = HashMap::new();
        for o in &self.objects {
            map.entry(o.parent).or_default().push(o.id);
        }
        map
    }

    /// Parent space -> object's local space for an object on a bone: the
    /// bone's skinning matrix at `frame`. Identity for everything else.
    pub fn bone_matrix(&self, o: &Object) -> Mat4 {
        self.bone_matrix_at(o, self.frame)
    }

    /// `bone_matrix` at `frame` (None: at rest).
    fn bone_matrix_at(&self, o: &Object, frame: Option<f32>) -> Mat4 {
        let Some(bone) = &o.bone else { return Mat4::IDENTITY };
        let Some(rig) = o.parent.and_then(|p| self.get(p)).and_then(|p| p.rig.as_ref()) else { return Mat4::IDENTITY };
        let Some(i) = rig.bone_index(bone) else { return Mat4::IDENTITY };
        rig.skin_matrices(frame).get(i).copied().unwrap_or(Mat4::IDENTITY)
    }

    /// The object's matrix in its parent's space, bone included.
    pub fn local_in_parent(&self, o: &Object) -> Mat4 {
        self.bone_matrix(o) * o.local_matrix()
    }

    /// World matrix and effective visibility of every object, in one O(n)
    /// pass. Use this per frame instead of calling `world_matrix` per object.
    pub fn evaluate(&self) -> HashMap<u64, (Mat4, bool)> {
        let index = self.index();
        let mut done: HashMap<u64, (Mat4, bool)> = HashMap::with_capacity(self.objects.len());
        for start in &self.objects {
            // Walk up to the first evaluated ancestor (or the root), then fill
            // in the path top-down. `MAX_DEPTH` also stops corrupt cycles.
            let mut path = Vec::new();
            let mut cur = Some(start.id);
            while let Some(id) = cur {
                if done.contains_key(&id) || path.len() > MAX_DEPTH {
                    break;
                }
                let Some(&i) = index.get(&id) else { break };
                path.push(i);
                cur = self.objects[i].parent;
            }
            let mut parent_state = cur.and_then(|id| done.get(&id).copied());
            for &i in path.iter().rev() {
                let o = &self.objects[i];
                let (pm, pv) = parent_state.unwrap_or((Mat4::IDENTITY, true));
                let state = (pm * self.local_in_parent(o), pv && o.visible);
                done.insert(o.id, state);
                parent_state = Some(state);
            }
        }
        done
    }

    pub fn children(&self, id: Option<u64>) -> Vec<u64> {
        self.objects.iter().filter(|o| o.parent == id).map(|o| o.id).collect()
    }

    /// All objects below `id`, depth first (not including `id`).
    pub fn descendants(&self, id: u64) -> Vec<u64> {
        let map = self.child_map();
        let kids = |c: u64| map.get(&Some(c)).map_or(&[][..], |v| v.as_slice());
        let mut out = Vec::new();
        let mut seen = HashSet::from([id]);
        let mut stack: Vec<u64> = kids(id).iter().rev().copied().collect();
        while let Some(c) = stack.pop() {
            if !seen.insert(c) {
                continue;
            }
            out.push(c);
            stack.extend(kids(c).iter().rev());
        }
        out
    }

    /// Parent chain from the immediate parent up to the root.
    pub fn ancestors(&self, id: u64) -> Vec<u64> {
        let mut out = Vec::new();
        let mut cur = self.get(id).and_then(|o| o.parent);
        while let Some(p) = cur {
            if out.len() >= MAX_DEPTH || out.contains(&p) {
                break;
            }
            out.push(p);
            cur = self.get(p).and_then(|o| o.parent);
        }
        out
    }

    /// Levels below `id` (0 for a leaf). Iterative, so deep trees cannot overflow the stack.
    pub fn subtree_height(&self, id: u64) -> usize {
        let map = self.child_map();
        let mut deepest = 0;
        let mut stack = vec![(id, 0usize)];
        let mut seen = HashSet::new();
        while let Some((cur, depth)) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            deepest = deepest.max(depth);
            stack.extend(map.get(&Some(cur)).into_iter().flatten().map(|&c| (c, depth + 1)));
        }
        deepest
    }

    pub fn is_ancestor(&self, ancestor: u64, of: u64) -> bool {
        self.ancestors(of).contains(&ancestor)
    }

    /// The ids in `ids` that have no ancestor also in `ids`. Transforming
    /// these moves everything once (children follow their parents).
    pub fn roots_of(&self, ids: &[u64]) -> Vec<u64> {
        ids.iter()
            .copied()
            .filter(|&id| self.get(id).is_some() && !self.ancestors(id).iter().any(|a| ids.contains(a)))
            .collect()
    }

    /// Visible only if the object and every ancestor are visible (like Maya).
    pub fn is_visible(&self, id: u64) -> bool {
        self.get(id).is_some_and(|o| o.visible) && self.ancestors(id).iter().all(|&a| self.get(a).is_some_and(|o| o.visible))
    }

    pub fn world_matrix(&self, id: u64) -> Mat4 {
        let Some(obj) = self.get(id) else { return Mat4::IDENTITY };
        self.parent_world(obj.parent) * self.local_in_parent(obj)
    }

    /// `world_matrix` with every rig at rest, as inside `pose::at_rest`
    /// (bones placed by their rest pose), without changing the scene.
    pub fn rest_world_matrix(&self, id: u64) -> Mat4 {
        let Some(obj) = self.get(id) else { return Mat4::IDENTITY };
        let mut m = self.bone_matrix_at(obj, None) * obj.local_matrix();
        for a in self.ancestors(id) {
            if let Some(o) = self.get(a) {
                m = self.bone_matrix_at(o, None) * o.local_matrix() * m;
            }
        }
        m
    }

    fn parent_world(&self, parent: Option<u64>) -> Mat4 {
        match parent {
            Some(p) => {
                let mut m = Mat4::IDENTITY;
                for a in std::iter::once(p).chain(self.ancestors(p)) {
                    if let Some(o) = self.get(a) {
                        m = self.local_in_parent(o) * m;
                    }
                }
                m
            }
            None => Mat4::IDENTITY,
        }
    }

    /// Changes the object's local values so its world matrix becomes `world`.
    pub fn set_world_matrix(&mut self, id: u64, world: Mat4) {
        let Some(o) = self.get(id) else { return };
        let local = (self.parent_world(o.parent) * self.bone_matrix(o)).inverse() * world;
        if let Some(o) = self.get_mut(id) {
            o.set_local_matrix(local);
        }
    }

    /// World position of the object's pivot (where the manipulator sits).
    pub fn pivot_world(&self, id: u64) -> Vec3 {
        let pivot = self.get(id).map_or(Vec3::ZERO, |o| o.pivot);
        self.world_matrix(id).transform_point3(pivot)
    }

    // ---------- hierarchy edits ----------

    /// Moves `id` under `new_parent` (None = world) keeping it where it is.
    pub fn reparent(&mut self, id: u64, new_parent: Option<u64>) -> Result<(), String> {
        let name = |s: &Scene, i: u64| s.get(i).map_or(i.to_string(), |o| o.name.clone());
        if self.get(id).is_none() {
            return Err(format!("no object {id}"));
        }
        if let Some(p) = new_parent {
            if self.get(p).is_none() {
                return Err(format!("no object {p}"));
            }
            if p == id || self.is_ancestor(id, p) {
                return Err(format!("cannot parent {} under its own child {}", name(self, id), name(self, p)));
            }
            if self.ancestors(p).len() + 1 + self.subtree_height(id) >= MAX_DEPTH {
                return Err(format!("hierarchy would be deeper than {MAX_DEPTH} levels"));
            }
        }
        let world = self.world_matrix(id);
        let o = self.get_mut(id).expect("checked");
        o.parent = new_parent;
        o.bone = None;
        self.set_world_matrix(id, world);
        Ok(())
    }

    /// Puts the roots of `ids` under a new empty group (Maya: Ctrl+G).
    /// The group sits under the objects' shared parent, if they have one.
    pub fn group(&mut self, ids: &[u64]) -> u64 {
        let roots = self.roots_of(ids);
        let parents: HashSet<Option<u64>> = roots.iter().map(|&r| self.get(r).and_then(|o| o.parent)).collect();
        let shared = if parents.len() == 1 { parents.into_iter().next().flatten() } else { None };
        let group = self.add(Shape::Group);
        self.get_mut(group).expect("just added").parent = shared;
        for r in roots {
            // Cannot fail: the group is new, so it is nobody's descendant.
            let _ = self.reparent(r, Some(group));
        }
        group
    }

    /// Removes a group, moving its children up to the group's parent.
    pub fn ungroup(&mut self, group: u64) -> Vec<u64> {
        let parent = self.get(group).and_then(|o| o.parent);
        let kids = self.children(Some(group));
        for &k in &kids {
            let _ = self.reparent(k, parent);
        }
        self.objects.retain(|o| o.id != group);
        kids
    }

    /// The objects that are geometry: everything but reference image
    /// planes. Rays (`surface_point`, picking), framing and bounds walk
    /// these, so nothing a sheet places is ever hit, framed or measured.
    pub fn geometry(&self) -> impl Iterator<Item = &Object> {
        self.objects.iter().filter(|o| !o.is_reference())
    }

    /// Removes a reference plane alone: objects parented under it move to
    /// its parent, keeping where they stand in the world.
    pub fn remove_reference_plane(&mut self, id: u64) {
        let parent = self.get(id).and_then(|o| o.parent);
        for child in self.children(Some(id)) {
            if self.reparent(child, parent).is_err() {
                let _ = self.reparent(child, None);
            }
        }
        self.objects.retain(|o| o.id != id);
    }

    /// Every reference plane removed (`remove_reference_plane`): the scene
    /// exports write.
    pub fn without_references(&mut self) {
        let planes: Vec<u64> = self.objects.iter().filter(|o| o.is_reference()).map(|o| o.id).collect();
        for id in planes {
            self.remove_reference_plane(id);
        }
    }

    /// Deletes the object and everything under it (like Maya).
    pub fn remove_tree(&mut self, id: u64) {
        let mut doomed = self.descendants(id);
        doomed.push(id);
        self.objects.retain(|o| !doomed.contains(&o.id));
    }

    /// Copies the object and its subtree next to the original. Returns the copy's id.
    pub fn duplicate_tree(&mut self, id: u64) -> Option<u64> {
        let original = self.get(id)?.clone();
        let copy = self.copy_one(original, None)?;
        let mut queue = vec![(id, copy)];
        while let Some((from, to)) = queue.pop() {
            for child in self.children(Some(from)) {
                let obj = self.get(child)?.clone();
                let new_child = self.copy_one(obj, Some(Some(to)))?;
                queue.push((child, new_child));
            }
        }
        Some(copy)
    }

    /// Copies one object with a fresh id and name. `parent: None` keeps the original parent.
    fn copy_one(&mut self, mut obj: Object, parent: Option<Option<u64>>) -> Option<u64> {
        obj.id = self.alloc_id();
        // A duplicate is free: it does not stay a linked mirror.
        obj.link = None;
        let base = obj.name.trim_end_matches(|c: char| c.is_ascii_digit()).to_string();
        obj.name = self.unique_name(&base);
        if let Some(p) = parent {
            obj.parent = p;
        }
        let id = obj.id;
        self.objects.push(obj);
        Some(id)
    }

    // ---------- pivots ----------

    /// Moves the pivot to a world-space point without moving the object.
    pub fn set_pivot_world(&mut self, id: u64, world_point: Vec3) {
        let local = self.world_matrix(id).inverse().transform_point3(world_point);
        if let Some(o) = self.get_mut(id) {
            let d = o.pivot - local;
            o.pivot_comp += d - o.rotation * (o.scale * d);
            o.pivot = local;
        }
    }

    /// Moves the pivot to the center of the object's (and its children's)
    /// bounding box. `mesh_bounds` gives each object's local mesh min/max.
    pub fn center_pivot(&mut self, id: u64, mesh_bounds: &dyn Fn(&Object) -> Option<(Vec3, Vec3)>) {
        if let Some((min, max)) = self.subtree_bounds(id, Mat4::IDENTITY, mesh_bounds) {
            let center = self.world_matrix(id).transform_point3((min + max) * 0.5);
            self.set_pivot_world(id, center);
        }
    }

    /// Bounds of `id` and its subtree (reference planes under it left
    /// out), in the space `to_space` maps `id`'s object space into.
    pub fn subtree_bounds(&self, id: u64, to_space: Mat4, mesh_bounds: &dyn Fn(&Object) -> Option<(Vec3, Vec3)>) -> Option<(Vec3, Vec3)> {
        self.get(id)?;
        let index = self.index();
        let map = self.child_map();
        let mut acc: Option<(Vec3, Vec3)> = None;
        // Iterative walk (no recursion), visiting each object once, so a deep
        // or corrupt hierarchy cannot overflow the stack or loop forever.
        let mut stack = vec![(id, to_space)];
        let mut seen = HashSet::new();
        while let Some((cur, space)) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            let Some(obj) = index.get(&cur).map(|&i| &self.objects[i]) else { continue };
            // Reference planes under the object are not part of it (framing,
            // pivots, placement): only one asked for itself counts.
            if let Some((lo, hi)) = mesh_bounds(obj).filter(|_| cur == id || !obj.is_reference()) {
                for i in 0..8 {
                    let corner = Vec3::new(
                        if i & 1 == 0 { lo.x } else { hi.x },
                        if i & 2 == 0 { lo.y } else { hi.y },
                        if i & 4 == 0 { lo.z } else { hi.z },
                    );
                    let p = space.transform_point3(corner);
                    acc = Some(match acc {
                        Some((a, b)) => (a.min(p), b.max(p)),
                        None => (p, p),
                    });
                }
            }
            for &child in map.get(&Some(cur)).into_iter().flatten() {
                if let Some(&i) = index.get(&child) {
                    stack.push((child, space * self.local_in_parent(&self.objects[i])));
                }
            }
        }
        acc
    }

    /// Repairs files from outside the app: drops parents that point to
    /// missing objects, form cycles, or nest deeper than `MAX_DEPTH`.
    pub fn sanitize(&mut self) {
        self.renumber_if_needed();
        for o in &mut self.objects { if o.cloth.as_ref().is_some_and(|c| !c.valid()) { o.cloth = None; } }
        // A damaged reference plane (or one on another shape) is dropped,
        // leaving a plain plane.
        for o in &mut self.objects {
            if o.reference.as_ref().is_some_and(|r| o.shape != Shape::Plane || !r.is_sane()) {
                o.reference = None;
            }
        }
        for clay in self.objects.iter_mut().filter_map(|o| o.clay.as_mut()) {
            clay.sanitize();
        }
        for rig in self.objects.iter_mut().filter_map(|o| o.rig.as_mut()) {
            rig.sanitize();
        }
        for eye in self.objects.iter_mut().filter_map(|o| o.eye.as_mut()) {
            eye.sanitize();
        }
        for b in self.objects.iter_mut().filter_map(|o| o.bake.as_mut()) {
            b.sanitize();
        }
        for h in self.objects.iter_mut().filter_map(|o| o.hair.as_mut()) {
            h.sanitize();
        }
        for k in self.objects.iter_mut().filter_map(|o| o.kit.as_mut()) {
            k.sanitize();
        }
        for t in self.objects.iter_mut().filter_map(|o| o.terrain.as_mut()) {
            t.sanitize();
        }
        for p in self.objects.iter_mut().filter_map(|o| o.part.as_mut()) {
            p.sanitize();
        }
        for l in self.objects.iter_mut().filter_map(|o| o.light.as_mut()) {
            l.sanitize();
        }
        self.materials.truncate(crate::material::MAX_MATERIALS);
        // Damaged guides in a file are dropped (none, rather than wrong).
        if self.guides.check().is_err() {
            self.guides = Default::default();
        }
        for m in &mut self.materials {
            m.sanitize();
        }
        for o in &mut self.objects {
            o.damage.truncate(crate::smart::MAX_DECALS);
            for d in &mut o.damage {d.sanitize();}
            if o.bone.as_ref().is_some_and(|b| b.is_empty() || b.len() > crate::rig::MAX_NAME) {
                o.bone = None;
            }
        }
        let index = self.index();
        for o in &mut self.objects {
            if o.parent.is_some_and(|p| !index.contains_key(&p) || p == o.id) {
                o.parent = None;
            }
            if o.follow.as_ref().is_some_and(|f| !index.contains_key(&f.object) || f.object == o.id || !f.offset.is_finite()) {
                o.follow = None;
            }
            if o.link.as_ref().is_some_and(|l| !index.contains_key(&l.source) || l.source == o.id || !l.valid()) {
                o.link = None;
            }
        }
        // One pass with memoized depths: walk up to a known object, then
        // assign depths top-down, cutting cycles and anything too deep.
        let mut depth: HashMap<u64, usize> = HashMap::with_capacity(self.objects.len());
        for start in 0..self.objects.len() {
            let mut path: Vec<usize> = Vec::new();
            let mut on_path = HashSet::new();
            let mut cur = Some(self.objects[start].id);
            while let Some(id) = cur {
                if depth.contains_key(&id) {
                    break;
                }
                if !on_path.insert(id) {
                    // Cycle: cut the link that closes it.
                    let &last = path.last().expect("cycle needs a path");
                    self.objects[last].parent = None;
                    cur = None;
                    break;
                }
                let i = index[&id];
                path.push(i);
                cur = self.objects[i].parent;
            }
            let mut d = cur.and_then(|id| depth.get(&id).copied());
            for &i in path.iter().rev() {
                let next = d.map_or(0, |pd| pd + 1);
                let next = if next >= MAX_DEPTH - 1 {
                    self.objects[i].parent = None;
                    0
                } else if self.objects[i].parent.is_none() {
                    0
                } else {
                    next
                };
                depth.insert(self.objects[i].id, next);
                d = Some(next);
            }
        }
        self.next_id = self.next_id.max(self.objects.iter().map(|o| o.id).max().unwrap_or(0));
    }

    /// Gives every object a fresh id 1..=n when ids are duplicated, zero, or
    /// so large the counter could overflow. Every lookup then agrees on which
    /// object an id means. A parent link to a duplicated id keeps pointing at
    /// the first object that had it.
    fn renumber_if_needed(&mut self) {
        let mut seen = HashSet::new();
        let bad = self.objects.iter().any(|o| o.id == 0 || o.id > MAX_ID || !seen.insert(o.id))
            || self.next_id > MAX_ID;
        if !bad {
            return;
        }
        let mut first_new: HashMap<u64, u64> = HashMap::new();
        for (i, o) in self.objects.iter().enumerate() {
            first_new.entry(o.id).or_insert(i as u64 + 1);
        }
        for (i, o) in self.objects.iter_mut().enumerate() {
            o.id = i as u64 + 1;
            o.parent = o.parent.and_then(|p| first_new.get(&p).copied());
            if let Some(f) = &mut o.follow {
                f.object = first_new.get(&f.object).copied().unwrap_or(0);
            }
            if let Some(l) = &mut o.link {
                l.source = first_new.get(&l.source).copied().unwrap_or(0);
            }
            // A bake's simple mesh from a Poly (`bake lowpoly: {poly}`).
            if let Some(l) = o.bake.as_mut().and_then(|b| b.lowpoly.as_mut()) {
                l.poly = first_new.get(&l.poly).copied().unwrap_or(0);
            }
            // The clay a Poly was made from (`poly rebuild`, `quality_report`)
            // and the one its detail was projected from (`poly project`).
            if let Some(p) = o.poly.as_mut() {
                for m in [p.made_by.as_mut(), p.projected.as_mut()].into_iter().flatten().filter(|m| m["source"].is_u64()).map(std::sync::Arc::make_mut) {
                    m["source"] = first_new.get(&m["source"].as_u64().expect("checked")).copied().unwrap_or(0).into();
                }
            }
        }
        self.next_id = self.objects.len() as u64;
    }

    fn alloc_id(&mut self) -> u64 {
        // `sanitize` keeps ids far below u64::MAX, so this cannot overflow.
        self.next_id = self.next_id.saturating_add(1);
        self.next_id
    }

    fn unique_name(&self, base: &str) -> String {
        // Collect once: checking every candidate against every object made
        // big scenes quadratic per new object.
        let taken: HashSet<&str> = self.objects.iter().map(|o| o.name.as_str()).collect();
        (1..)
            .map(|n| format!("{base}{n}"))
            .find(|name| !taken.contains(name.as_str()))
            .expect("unbounded range always yields a name")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-4
    }

    #[test]
    fn maya_style_names_and_duplicate() {
        let mut s = Scene::default();
        let a = s.add(Shape::Cube);
        s.add(Shape::Cube);
        assert_eq!(s.get(a).unwrap().name, "pCube1");
        assert_eq!(s.objects[1].name, "pCube2");
        let d = s.duplicate_tree(a).unwrap();
        assert_eq!(s.get(d).unwrap().name, "pCube3");
        let g = s.add(Shape::Group);
        assert_eq!(s.get(g).unwrap().name, "group1");
    }

    #[test]
    fn euler_round_trip() {
        let mut s = Scene::default();
        let id = s.add(Shape::Cone);
        let o = s.get_mut(id).unwrap();
        o.set_euler_degrees(Vec3::new(30.0, 45.0, -60.0));
        assert!((o.euler_degrees() - Vec3::new(30.0, 45.0, -60.0)).length() < 1e-3);
    }

    #[test]
    fn json_round_trip_and_old_files_load() {
        let mut s = Scene::default();
        s.add(Shape::Torus);
        s.add(Shape::Plane);
        let back: Scene = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);
        // A file saved before parents/pivots existed still loads.
        let old = r#"{"objects":[{"id":1,"name":"pCube1","shape":"Cube","translation":[0,0.5,0],
            "rotation":[0,0,0,1],"scale":[1,1,1],"color":[1,1,1],"visible":true}],"next_id":1}"#;
        let s: Scene = serde_json::from_str(old).unwrap();
        assert_eq!(s.objects[0].parent, None);
    }

    #[test]
    fn reparent_keeps_world_position() {
        let mut s = Scene::default();
        let parent = s.add(Shape::Cube);
        let child = s.add(Shape::Sphere);
        s.get_mut(parent).unwrap().translation = Vec3::new(5.0, 0.0, 0.0);
        s.get_mut(parent).unwrap().set_euler_degrees(Vec3::new(0.0, 90.0, 0.0));
        s.get_mut(child).unwrap().translation = Vec3::new(1.0, 2.0, 3.0);
        let before = s.world_matrix(child).w_axis.truncate();
        s.reparent(child, Some(parent)).unwrap();
        assert!(close(s.world_matrix(child).w_axis.truncate(), before));
        // Moving the parent now carries the child.
        s.get_mut(parent).unwrap().translation.x += 1.0;
        assert!(close(s.world_matrix(child).w_axis.truncate(), before + Vec3::X));
        s.reparent(child, None).unwrap();
        assert!(close(s.world_matrix(child).w_axis.truncate(), before + Vec3::X));
    }

    #[test]
    fn no_cycles() {
        let mut s = Scene::default();
        let a = s.add(Shape::Cube);
        let b = s.add(Shape::Cube);
        s.reparent(b, Some(a)).unwrap();
        assert!(s.reparent(a, Some(b)).is_err());
        assert!(s.reparent(a, Some(a)).is_err());
    }

    #[test]
    fn group_ungroup_and_delete_tree() {
        let mut s = Scene::default();
        let a = s.add(Shape::Cube);
        let b = s.add(Shape::Sphere);
        let g = s.group(&[a, b]);
        assert_eq!(s.children(Some(g)), vec![a, b]);
        assert_eq!(s.roots_of(&[g, a]), vec![g]);
        let copy = s.duplicate_tree(g).unwrap();
        assert_eq!(s.descendants(copy).len(), 2);
        s.ungroup(g);
        assert!(s.get(g).is_none());
        assert_eq!(s.get(a).unwrap().parent, None);
        s.remove_tree(copy);
        assert_eq!(s.objects.len(), 2);
    }

    #[test]
    fn hidden_parent_hides_children() {
        let mut s = Scene::default();
        let a = s.add(Shape::Cube);
        let g = s.group(&[a]);
        s.get_mut(g).unwrap().visible = false;
        assert!(!s.is_visible(a));
    }

    #[test]
    fn moving_pivot_keeps_object_and_translate() {
        let mut s = Scene::default();
        let id = s.add(Shape::Cube);
        let o = s.get_mut(id).unwrap();
        o.set_euler_degrees(Vec3::new(10.0, 40.0, 0.0));
        o.scale = Vec3::new(2.0, 1.0, 1.0);
        let before = s.world_matrix(id);
        let translate = s.get(id).unwrap().translation;
        s.set_pivot_world(id, Vec3::new(3.0, 1.0, -2.0));
        assert!(s.world_matrix(id).abs_diff_eq(before, 1e-4));
        assert!(close(s.get(id).unwrap().translation, translate));
        assert!(close(s.pivot_world(id), Vec3::new(3.0, 1.0, -2.0)));
        // Rotating now turns around the new pivot: the pivot stays fixed.
        s.get_mut(id).unwrap().set_euler_degrees(Vec3::new(0.0, 0.0, 77.0));
        assert!(close(s.pivot_world(id), Vec3::new(3.0, 1.0, -2.0)));
    }

    #[test]
    fn center_pivot_of_group() {
        let mut s = Scene::default();
        let a = s.add(Shape::Cube);
        let b = s.add(Shape::Cube);
        s.get_mut(a).unwrap().translation = Vec3::new(-2.0, 0.5, 0.0);
        s.get_mut(b).unwrap().translation = Vec3::new(4.0, 0.5, 0.0);
        let g = s.group(&[a, b]);
        let unit = |o: &Object| o.shape.is_mesh().then_some((Vec3::splat(-0.5), Vec3::splat(0.5)));
        s.center_pivot(g, &unit);
        assert!(close(s.pivot_world(g), Vec3::new(1.0, 0.5, 0.0)));
    }

    #[test]
    fn deep_hierarchies_are_capped_and_safe() {
        let mut s = Scene::default();
        let mut prev = s.add(Shape::Cube);
        let root = prev;
        let mut refused = false;
        for _ in 0..(MAX_DEPTH + 10) {
            let next = s.add(Shape::Cube);
            if s.reparent(next, Some(prev)).is_err() {
                refused = true;
                break;
            }
            prev = next;
        }
        assert!(refused, "depth limit was not enforced");
        let unit = |o: &Object| o.shape.is_mesh().then_some((Vec3::splat(-0.5), Vec3::splat(0.5)));
        assert!(s.subtree_bounds(root, Mat4::IDENTITY, &unit).is_some());

        // A file with a 10,000-deep chain loads and walks without overflowing.
        let mut deep = Scene::default();
        let first = deep.add(Shape::Cube);
        let template = deep.get(first).unwrap().clone();
        for id in 2..=10_001 {
            deep.objects.push(Object { id, name: format!("g{id}"), shape: Shape::Group, parent: Some(id - 1), ..template.clone() });
        }
        deep.next_id = 10_001;
        assert!(deep.subtree_bounds(first, Mat4::IDENTITY, &unit).is_some());
        deep.sanitize();
        let parent: HashMap<u64, Option<u64>> = deep.objects.iter().map(|o| (o.id, o.parent)).collect();
        let depth = |mut id: u64| {
            let mut d = 0;
            while let Some(Some(p)) = parent.get(&id) {
                id = *p;
                d += 1;
            }
            d
        };
        assert!(deep.objects.iter().all(|o| depth(o.id) < MAX_DEPTH));
        assert_eq!(deep.evaluate().len(), deep.objects.len());
    }

    #[test]
    fn sanitize_fixes_duplicate_and_huge_ids() {
        let mut s = Scene::default();
        let a = s.add(Shape::Cube);
        let b = s.add(Shape::Sphere);
        let c = s.add(Shape::Cone);
        // b and c both claim a's id; c is a child of "a"; next_id is at the limit.
        s.get_mut(b).unwrap().id = a;
        s.objects[2].parent = Some(a);
        s.objects[2].id = u64::MAX;
        let _ = c;
        s.next_id = u64::MAX;
        s.sanitize();
        let ids: HashSet<u64> = s.objects.iter().map(|o| o.id).collect();
        assert_eq!(ids.len(), 3, "ids must be unique");
        assert_eq!(s.objects[2].parent, Some(s.objects[0].id));
        // Adding works without overflowing.
        let d = s.add(Shape::Torus);
        assert!(!ids.contains(&d));
    }

    /// A renumbered file keeps each Poly pointing at the clay it was made
    /// from (`made_by`), and one whose clay is gone at none.
    #[test]
    fn renumbering_keeps_poly_sources() {
        let mut s = Scene::default();
        let cube = s.add(Shape::Cube);
        let clay = s.add(Shape::Clay);
        let poly = s.add(Shape::Poly);
        let lost = s.add(Shape::Poly);
        // The clay and the first Poly share an id: the file is renumbered.
        s.get_mut(clay).unwrap().id = 9;
        s.get_mut(poly).unwrap().id = 9;
        let made = |source: u64| crate::poly::Poly { made_by: Some(std::sync::Arc::new(serde_json::json!({ "action": "from_clay", "source": source }))), ..crate::poly::tests::cube() };
        s.objects[2].poly = Some(crate::poly::Poly { projected: Some(std::sync::Arc::new(serde_json::json!({ "source": 9, "levels": 2 }))), ..made(9) });
        s.objects[3].poly = Some(made(77));
        let _ = (cube, lost);
        s.sanitize();
        let source = |i: usize| s.objects[i].poly.as_ref().unwrap().made_by.as_ref().unwrap()["source"].clone();
        assert_eq!(source(2), serde_json::json!(s.objects[1].id), "the clay, by its new id");
        assert_eq!(source(3), serde_json::json!(0), "a clay that is gone: none");
        let projected = s.objects[2].poly.as_ref().unwrap().projected.as_ref().unwrap()["source"].clone();
        assert_eq!(projected, serde_json::json!(s.objects[1].id), "the projection's clay too");
    }

    #[test]
    fn sanitize_breaks_cycles() {
        let mut s = Scene::default();
        let a = s.add(Shape::Cube);
        let b = s.add(Shape::Cube);
        s.get_mut(a).unwrap().parent = Some(b);
        s.get_mut(b).unwrap().parent = Some(a);
        s.sanitize();
        assert!(s.get(a).unwrap().parent.is_none() || s.get(b).unwrap().parent.is_none());
    }

    #[test]
    fn an_object_on_a_bone_follows_it() {
        use crate::rig::{Bone, Clip, Key, Rig};
        let mut s = Scene::default();
        let body = s.add(Shape::Clay);
        let prop = s.add(Shape::Cube);
        let mut clip = Clip::new("turn");
        let mut k = Key::new(1.0);
        k.rotate = Vec3::new(0.0, 0.0, 90.0);
        clip.track_mut("arm").insert(k);
        let arm = Bone { name: "arm".into(), parent: None, head: Vec3::ZERO, tail: Vec3::X, deform: true };
        s.get_mut(body).unwrap().rig = Some(Rig { bones: vec![arm], clips: vec![clip], active: Some("turn".into()), split: true, weights: Vec::new() });
        let o = s.get_mut(prop).unwrap();
        o.parent = Some(body);
        o.bone = Some("arm".into());
        o.translation = Vec3::new(1.0, 0.0, 0.0);
        // Rest pose: where it was placed.
        assert!(close(s.world_matrix(prop).w_axis.truncate(), Vec3::X));
        // Posed: the arm turned 90 degrees, so the prop swung up to +Y.
        s.frame = Some(1.0);
        assert!(close(s.world_matrix(prop).w_axis.truncate(), Vec3::Y));
        assert!(close(s.evaluate()[&prop].0.w_axis.truncate(), Vec3::Y));
        // Moving it while posed keeps the rest-space values consistent.
        let w = s.world_matrix(prop);
        s.set_world_matrix(prop, w);
        assert!(close(s.get(prop).unwrap().translation, Vec3::X));
        // Reparenting drops the bone.
        s.reparent(prop, None).unwrap();
        assert!(s.get(prop).unwrap().bone.is_none());
        assert!(close(s.world_matrix(prop).w_axis.truncate(), Vec3::Y));
    }
}
