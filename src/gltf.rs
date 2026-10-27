//! Export to glTF 2.0 binary (.glb), the file Blender, Unity, Unreal, Godot
//! and three.js all read.
//!
//! What goes in:
//! - one node per visible object, with its local translate / rotate / scale,
//!   under its parent (groups become empty nodes);
//! - one mesh per object with normals and vertex colors (blob color, paint
//!   and object color multiplied together, in linear color as glTF wants)
//!   and its materials (see `material.rs`): a mesh with several becomes one
//!   primitive per material, sharing the vertex data. Roughness and metallic
//!   map to the core material; clearcoat and sheen to the
//!   KHR_materials_clearcoat / KHR_materials_sheen extensions, emission to
//!   emissive (+ KHR_materials_emissive_strength), glass to
//!   KHR_materials_transmission + KHR_materials_ior. Base color is
//!   white so the vertex colors show unchanged;
//! - baked clay (see `bake.rs`) instead carries UVs, tangents and PNG
//!   textures: base color, normal, and one AO/roughness/metallic image used
//!   as both occlusionTexture (R) and metallicRoughnessTexture (G, B);
//! - for rigged clay: the bones as joint nodes under the object's node, a
//!   skin with up to 4 weights per vertex, and the rest-pose mesh;
//! - every clip as an animation, sampled once per frame with linear steps,
//!   so smooth and step keys look the same as in the editor. Clips with the
//!   same name on different objects become one animation.
//!
//! glTF and Clide3D agree on axes: Y up, characters face +Z.

use std::collections::BTreeMap;

use glam::{Mat4, Quat, Vec3};
use serde_json::{Value, json};

use crate::material::Material;
use std::sync::Arc;

use crate::mesh::{Mesh, MeshLib, Textures};
use crate::rig::{Rig, Skin};
use crate::scene::{Object, Scene, Shape};

const FLOAT: u32 = 5126;
const UNSIGNED_SHORT: u32 = 5123;
const UNSIGNED_INT: u32 = 5125;
const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;

pub struct Options {
    /// Objects to export with everything under them; empty = the whole scene.
    pub roots: Vec<u64>,
    pub animations: bool,
    /// Time-slider frames per second, for animation times.
    pub fps: f32,
    /// FBX only: start of the texture file names written next to the file.
    pub file_stem: String,
    /// Baked objects also get LOD1-LOD3 (see `bake::lods`).
    pub lods: bool,
    pub atlas: bool,
    pub texel_density: Option<f32>,
    /// Scattered copies as one node per source with EXT_mesh_gpu_instancing
    /// transforms, instead of a node per copy.
    pub gpu_instancing: bool,
    /// Blend shapes (morph targets) for clay objects' meshes.
    pub blend_shapes: Vec<BlendShape>,
}

/// One blend shape of an object's exported mesh: how far each vertex
/// moves at weight 1 (see `layers::project_shape`).
#[derive(Clone, Debug)]
pub struct BlendShape {
    pub object: u64,
    pub name: String,
    pub deltas: Arc<Vec<[f32; 3]>>,
}

/// The file and what it holds.
pub struct Export {
    pub bytes: Vec<u8>,
    pub summary: Value,
    /// Files to write next to the main file (FBX textures): (name, bytes).
    pub files: Vec<(String, Vec<u8>)>,
    /// Blend shapes the file holds: (object, shape name). One passed in
    /// `Options` and missing here was not written (its object was hidden,
    /// or its mesh had other vertices).
    pub shapes_written: Vec<(u64, String)>,
}

#[derive(Default)]
struct Builder {
    /// Blend shapes written as morph targets: (object, shape name).
    shapes_written: Vec<(u64, String)>,
    atlas: crate::export_atlas::Atlas,
    bin: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
    nodes: Vec<Value>,
    meshes: Vec<Value>,
    /// Distinct (material, texture set, linear tint), in the order of `doc.materials`.
    materials: Vec<(Material, Option<usize>, [u32; 3])>,
    images: Vec<Value>,
    textures: Vec<Value>,
    /// Uploaded texture sets: (source, [color, normal, orm] texture indices).
    texture_sets: Vec<(Arc<Textures>, [usize; 3])>,
    skins: Vec<Value>,
    /// Animation name -> (samplers, channels).
    animations: BTreeMap<String, (Vec<Value>, Vec<Value>)>,
    vertices: usize,
    triangles: usize,
    /// (object id, bone name) -> (joint node, bone head), for objects on bones.
    joints: std::collections::HashMap<(u64, String), (usize, Vec3)>,
    /// Meshes written for plain (unskinned) nodes, with what they were
    /// written from (mesh, tint, own material): scattered copies and
    /// identical clay objects point at the same glTF mesh.
    shared: Vec<(Arc<Mesh>, [f32; 3], Material, usize)>,
    /// Nodes that point at a mesh written for another node.
    reused: usize,
    /// Nodes drawing their mesh at many places (EXT_mesh_gpu_instancing).
    instanced_nodes: usize,
    /// KHR_lights_punctual lights.
    lights: Vec<Value>,
    /// Copies written (nodes sharing a mesh).
    copies: usize,
    /// Triangles of each LOD written: (object, [LOD1, LOD2, LOD3]).
    lods: Vec<(String, Vec<usize>)>,
}

impl Builder {
    /// The glTF mesh for an unskinned node: one already written from the
    /// same mesh, tint and material, else a new one.
    fn shared_mesh(&mut self, name: &str, mesh: &Arc<Mesh>, tint: [f32; 3], own: &Material) -> Result<usize, String> {
        if let Some(&(_, _, _, m)) = self.shared.iter().find(|(a, t, o, _)| Arc::ptr_eq(a, mesh) && t.map(f32::to_bits) == tint.map(f32::to_bits) && o == own) {
            self.vertices += mesh.vertices.len();
            self.triangles += mesh.indices.len() / 3;
            self.reused += 1;
            return Ok(m);
        }
        let m = self.mesh(name, mesh, tint, own, None)?;
        self.shared.push((mesh.clone(), tint, own.clone(), m));
        Ok(m)
    }

    fn view(&mut self, data: &[u8], target: Option<u32>) -> usize {
        while !self.bin.len().is_multiple_of(4) {
            self.bin.push(0);
        }
        let mut v = json!({ "buffer": 0, "byteOffset": self.bin.len(), "byteLength": data.len() });
        if let Some(t) = target {
            v["target"] = json!(t);
        }
        self.bin.extend_from_slice(data);
        self.views.push(v);
        self.views.len() - 1
    }

    /// Floats in groups of `width` (1 SCALAR, 3 VEC3, 4 VEC4, 16 MAT4).
    fn floats(&mut self, data: &[f32], width: usize, target: Option<u32>, bounds: bool) -> usize {
        let ty = match width {
            1 => "SCALAR",
            2 => "VEC2",
            3 => "VEC3",
            4 => "VEC4",
            _ => "MAT4",
        };
        let view = self.view(bytemuck::cast_slice(data), target);
        let mut a = json!({ "bufferView": view, "componentType": FLOAT, "count": data.len() / width, "type": ty });
        if bounds {
            let mut lo = vec![f32::MAX; width];
            let mut hi = vec![f32::MIN; width];
            for c in data.chunks_exact(width) {
                for i in 0..width {
                    lo[i] = lo[i].min(c[i]);
                    hi[i] = hi[i].max(c[i]);
                }
            }
            a["min"] = json!(lo);
            a["max"] = json!(hi);
        }
        self.accessors.push(a);
        self.accessors.len() - 1
    }

    fn accessor(&mut self, view: usize, component: u32, count: usize, ty: &str) -> usize {
        self.accessors.push(json!({ "bufferView": view, "componentType": component, "count": count, "type": ty }));
        self.accessors.len() - 1
    }

    fn node(&mut self, node: Value) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    fn material(&mut self, m: &Material, set: Option<usize>, tint: [f32; 3]) -> usize {
        let key = (m.clone(), set, tint.map(f32::to_bits));
        match self.materials.iter().position(|x| *x == key) {
            Some(i) => i,
            None => {
                self.materials.push(key);
                self.materials.len() - 1
            }
        }
    }

    /// A PNG image and a texture that uses it.
    fn image(&mut self, rgba: &[u8], size: u32, keep_alpha: bool) -> Result<usize, String> {
        let png = crate::bake::png(rgba, size, keep_alpha)?;
        let view = self.view(&png, None);
        self.images.push(json!({ "bufferView": view, "mimeType": "image/png" }));
        self.textures.push(json!({ "source": self.images.len() - 1, "sampler": 0 }));
        Ok(self.textures.len() - 1)
    }

    fn texture_set(&mut self, t: &Arc<Textures>) -> Result<usize, String> {
        if let Some(i) = self.texture_sets.iter().position(|(s, _)| Arc::ptr_eq(s, t)) {
            return Ok(i);
        }
        let ids = [self.image(&t.color, t.size, t.alpha)?, self.image(&t.normal, t.size, false)?, self.image(&t.orm, t.size, false)?];
        self.texture_sets.push((t.clone(), ids));
        Ok(self.texture_sets.len() - 1)
    }

    /// A mesh with vertex colors times `tint` (sRGB), and a skin's joints and
    /// weights. `own` is the object's material, for meshes that carry none.
    fn mesh(&mut self, name: &str, mesh: &Mesh, tint: [f32; 3], own: &Material, skin: Option<&Skin>) -> Result<usize, String> {
        let tile = mesh.textures.as_ref().and_then(|t| self.atlas.get(&crate::export_atlas::key(t))).cloned();
        let textures = tile.as_ref().map(|t| &t.textures).or(mesh.textures.as_ref());
        let set = textures.map(|t| self.texture_set(t)).transpose()?;
        let pos: Vec<f32> = mesh.vertices.iter().flat_map(|v| v.pos).collect();
        let nrm: Vec<f32> = mesh.vertices.iter().flat_map(|v| Vec3::from(v.normal).normalize_or(Vec3::Y).to_array()).collect();
        let col: Vec<f32> = mesh
            .vertices
            .iter()
            .flat_map(|v| {
                let c = [0, 1, 2].map(|i| linear(v.color[i].clamp(0.0, 1.0) * tint[i].clamp(0.0, 1.0)));
                [c[0], c[1], c[2], 1.0]
            })
            .collect();
        let mut attributes = json!({
            "POSITION": self.floats(&pos, 3, Some(ARRAY_BUFFER), true),
            "NORMAL": self.floats(&nrm, 3, Some(ARRAY_BUFFER), false),
        });
        if set.is_some() {
            // Color is in the texture; the tint goes in the material.
            let uv: Vec<f32> = mesh.vertices.iter().flat_map(|v| tile.as_ref().map_or(v.uv,|t| [v.uv[0]*t.scale+t.offset[0],v.uv[1]*t.scale+t.offset[1]])).collect();
            attributes["TEXCOORD_0"] = json!(self.floats(&uv, 2, Some(ARRAY_BUFFER), false));
            let tan: Vec<f32> = mesh.vertices.iter().flat_map(|v| v.tangent).collect();
            attributes["TANGENT"] = json!(self.floats(&tan, 4, Some(ARRAY_BUFFER), false));
        } else {
            attributes["COLOR_0"] = json!(self.floats(&col, 4, Some(ARRAY_BUFFER), false));
            // Meshes laid out with UVs but no textures yet (kitbash parts)
            // keep them, for texturing in other tools.
            if mesh.vertices.iter().any(|v| v.uv != [0.0, 0.0]) {
                let uv: Vec<f32> = mesh.vertices.iter().flat_map(|v| v.uv).collect();
                attributes["TEXCOORD_0"] = json!(self.floats(&uv, 2, Some(ARRAY_BUFFER), false));
            }
        }
        if let Some(s) = skin {
            let joints: Vec<u16> = s.joints.iter().flatten().copied().collect();
            let view = self.view(bytemuck::cast_slice(&joints), Some(ARRAY_BUFFER));
            attributes["JOINTS_0"] = json!(self.accessor(view, UNSIGNED_SHORT, s.joints.len(), "VEC4"));
            let weights: Vec<f32> = s.weights.iter().flatten().copied().collect();
            attributes["WEIGHTS_0"] = json!(self.floats(&weights, 4, Some(ARRAY_BUFFER), false));
        }
        let mut primitives = Vec::new();
        // Core glTF has no per-vertex roughness/metallic. Unbaked damage
        // uses small quantized PBR groups; baked assets retain exact ORM maps.
        let damage_groups = crate::damage::export_groups(mesh);
        let groups: Vec<_> = if let Some(groups)=damage_groups {groups}else{split_by_material(mesh).into_iter().map(|(m,t)|(m,t,None)).collect()};
        for (material, tris, pbr) in groups {
            let m = match set {
                Some(_) => self.material(mesh.materials.get(material).unwrap_or(own), set, tint),
                None => {
                    // Vertex colors carry the color; a glow needs it in the
                    // material (emissive has no vertex color), so use the
                    // part's mean color.
                    let mut m = mesh.materials.get(material).unwrap_or(own).clone();
                    if let Some([rough,metal])=pbr {m.roughness=rough;m.metallic=metal;}
                    let glow = if m.emission > 0.0 {
                        let sum = tris.iter().fold([0.0f32; 3], |a, &i| {
                            let c = mesh.vertices[i as usize].color;
                            [0, 1, 2].map(|k| a[k] + c[k] * tint[k])
                        });
                        sum.map(|c| c / tris.len().max(1) as f32)
                    } else {
                        [1.0; 3]
                    };
                    self.material(&m, None, glow)
                }
            };
            let view = self.view(bytemuck::cast_slice(&tris), Some(ELEMENT_ARRAY_BUFFER));
            let indices = self.accessor(view, UNSIGNED_INT, tris.len(), "SCALAR");
            primitives.push(json!({ "attributes": attributes.clone(), "indices": indices, "material": m }));
        }
        self.vertices += mesh.vertices.len();
        self.triangles += mesh.indices.len() / 3;
        self.meshes.push(json!({ "name": name, "primitives": primitives }));
        Ok(self.meshes.len() - 1)
    }

    /// Morph targets on mesh `m` (every primitive shares its vertices):
    /// POSITION offsets, weights 0, names in `extras.targetNames`.
    fn targets(&mut self, m: usize, shapes: &[&BlendShape]) {
        let targets: Vec<Value> = shapes
            .iter()
            .map(|s| {
                let d: Vec<f32> = s.deltas.iter().flatten().copied().collect();
                json!({ "POSITION": self.floats(&d, 3, Some(ARRAY_BUFFER), true) })
            })
            .collect();
        let mesh = &mut self.meshes[m];
        for p in mesh["primitives"].as_array_mut().into_iter().flatten() {
            p["targets"] = json!(targets);
        }
        mesh["weights"] = json!(vec![0.0f32; shapes.len()]);
        mesh["extras"]["targetNames"] = json!(shapes.iter().map(|s| &s.name).collect::<Vec<_>>());
        self.shapes_written.extend(shapes.iter().map(|s| (s.object, s.name.clone())));
    }

    fn sampler(&mut self, anim: &str, times: usize, values: &[f32], width: usize, node: usize, path: &str) {
        let output = self.floats(values, width, None, false);
        let (samplers, channels) = self.animations.entry(anim.to_string()).or_default();
        samplers.push(json!({ "input": times, "output": output, "interpolation": "LINEAR" }));
        channels.push(json!({ "sampler": samplers.len() - 1, "target": { "node": node, "path": path } }));
    }
}

/// sRGB 0..1 -> linear 0..1.
fn linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// A node's translate / rotate / scale from a local matrix.
fn trs(name: &str, m: Mat4) -> Value {
    let (s, r, t) = m.to_scale_rotation_translation();
    let mut n = json!({ "name": name });
    if t != Vec3::ZERO {
        n["translation"] = json!(t.to_array());
    }
    if r != Quat::IDENTITY {
        n["rotation"] = json!(r.normalize().to_array());
    }
    if s != Vec3::ONE {
        n["scale"] = json!(s.to_array());
    }
    n
}

pub fn export(scene: &Scene, meshes: &MeshLib, opts: &Options) -> Result<Export, String> {
    // Everything is written in the rest pose.
    let mut scene = scene.clone();
    scene.frame = None;
    let scene = &scene;
    let mut b = Builder::default();
    let roots: Vec<u64> = if opts.roots.is_empty() { scene.children(None) } else { opts.roots.clone() };
    if opts.atlas {
        let mut ids = std::collections::HashSet::new();
        for id in &roots { ids.insert(*id); ids.extend(scene.descendants(*id)); }
        let inputs = scene.objects.iter().filter(|o| ids.contains(&o.id) && scene.is_visible(o.id)).filter_map(|o| {
            let mesh = if o.bake.is_some() { meshes.baked(o.id).map(|(m,_)|m.clone()) } else { meshes.get(o).cloned() }?;
            let w = scene.world_matrix(o.id);
            let scale = (w.x_axis.truncate().length()+w.y_axis.truncate().length()+w.z_axis.truncate().length())/3.0;
            Some((mesh,scale))
        }).collect();
        b.atlas = crate::export_atlas::pack(inputs,opts.texel_density)?;
    }
    let mut scene_nodes = Vec::new();
    let mut skipped = Vec::new();
    // Depth-first, each object after its parent; children are linked afterwards.
    let mut stack: Vec<(u64, Option<usize>)> = roots.iter().rev().map(|&id| (id, None)).collect();
    // An object on a bone hangs under that joint, offset back from the bone head.
    let mut children: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    while let Some((id, parent)) = stack.pop() {
        let Some(o) = scene.get(id) else { continue };
        if !scene.is_visible(id) {
            skipped.push(o.name.clone());
            continue;
        }
        // An exported root keeps its world placement.
        let joint = o.bone.as_ref().zip(o.parent).and_then(|(bone, p)| b.joints.get(&(p, bone.clone())).copied()).filter(|_| parent.is_some());
        let (parent, local) = match joint {
            Some((j, head)) => (Some(j), Mat4::from_translation(-head) * o.local_matrix()),
            None if parent.is_none() => (None, scene.world_matrix(id)),
            None => (parent, o.local_matrix()),
        };
        let node = b.node(trs(&o.name, local));
        match parent {
            Some(p) => children.entry(p).or_default().push(node),
            None => scene_nodes.push(node),
        }
        add_object(&mut b, o, &scene.material_of(o), node, meshes, opts, &mut children, &mut scene_nodes, scene)?;
        for c in scene.children(Some(id)).into_iter().rev() {
            stack.push((c, Some(node)));
        }
    }
    for (node, kids) in children {
        b.nodes[node]["children"] = json!(kids);
    }
    if b.meshes.is_empty() {
        return Err("nothing to export: no visible meshes".into());
    }

    let animations: Vec<Value> = b
        .animations
        .iter()
        .map(|(name, (samplers, channels))| json!({ "name": name, "samplers": samplers, "channels": channels }))
        .collect();
    let anim_names: Vec<&String> = b.animations.keys().collect();
    let mut doc = json!({
        "asset": { "version": "2.0", "generator": "Clide3D", "extras": { "fps": opts.fps.clamp(1.0, 120.0) } },
        "scene": 0,
        "scenes": [{ "nodes": scene_nodes }],
        "nodes": b.nodes,
        "meshes": b.meshes,
        "materials": b.materials.iter().map(|(m, set, tint)| material_json(m, set.map(|s| (b.texture_sets[s].1, b.texture_sets[s].0.alpha)), tint.map(f32::from_bits))).collect::<Vec<_>>(),
        "accessors": b.accessors,
        "bufferViews": b.views,
        "buffers": [{ "byteLength": b.bin.len() }],
    });
    if !b.skins.is_empty() {
        doc["skins"] = json!(b.skins);
    }
    if !b.textures.is_empty() {
        doc["images"] = json!(b.images);
        doc["textures"] = json!(b.textures);
        doc["samplers"] = json!([{ "magFilter": 9729, "minFilter": 9987, "wrapS": 33071, "wrapT": 33071 }]);
    }
    let mut used = material_extensions(&b);
    if !b.lods.is_empty() {
        used.push("MSFT_lod");
    }
    if b.instanced_nodes > 0 {
        // Used, not required: readers without it still show one copy.
        used.push("EXT_mesh_gpu_instancing");
    }
    if !b.lights.is_empty() {
        used.push("KHR_lights_punctual");
        doc["extensions"] = json!({ "KHR_lights_punctual": { "lights": b.lights } });
    }
    if !used.is_empty() {
        doc["extensionsUsed"] = json!(used);
    }
    if !animations.is_empty() {
        doc["animations"] = json!(animations);
    }
    let summary = json!({
        "meshes": doc["meshes"].as_array().map_or(0, Vec::len),
        "vertices": b.vertices,
        "triangles": b.triangles,
        "skins": b.skins.len(),
        "materials": b.materials.iter().map(|m| m.0.name.as_str()).collect::<Vec<_>>(),
        "textures": b.images.len(),
        "atlas_packed_sets": b.atlas.len(),
        "texel_density_target": opts.texel_density,
        "animations": anim_names,
        "skipped_hidden": skipped,
        "scatter_copies": b.copies,
        "shared_mesh_nodes": b.reused,
        "instanced_nodes": b.instanced_nodes,
        "lights": b.lights.len(),
        "blend_shapes": b.shapes_written.len(),
        "lods": b.lods.iter().map(|(n, t)| (n.clone(), json!({ "lod1_3_triangles": t }))).collect::<serde_json::Map<_, _>>(),
    });
    Ok(Export { bytes: glb(&doc, b.bin), summary, files: Vec::new(), shapes_written: b.shapes_written })
}

/// Triangle indices grouped by material (index into `mesh.materials`;
/// usize::MAX when the mesh has none). A triangle takes the material most of
/// its corners have.
pub(crate) fn split_by_material(mesh: &Mesh) -> Vec<(usize, Vec<u32>)> {
    if mesh.materials.len() <= 1 {
        return vec![(if mesh.materials.is_empty() { usize::MAX } else { 0 }, mesh.indices.clone())];
    }
    let mut groups: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
    for t in mesh.indices.chunks_exact(3) {
        groups.entry(triangle_material(mesh, t)).or_default().extend_from_slice(t);
    }
    groups.into_iter().collect()
}

/// The material most corners of triangle `t` have (the first corner's on a tie).
pub(crate) fn triangle_material(mesh: &Mesh, t: &[u32]) -> usize {
    let m = t.iter().map(|&i| mesh.vertices[i as usize].material as usize).collect::<Vec<_>>();
    let m = if m[1] == m[2] { m[1] } else { m[0] };
    m.min(mesh.materials.len().saturating_sub(1))
}

/// A material; with textures ([color, normal, orm], and whether color's
/// alpha is a cut-out mask), roughness and metallic come from the orm image
/// and `tint` (sRGB) scales the color.
fn material_json(m: &Material, textures: Option<([usize; 3], bool)>, tint: [f32; 3]) -> Value {
    let (textures, cutout) = match textures {
        Some((t, a)) => (Some(t), a),
        None => (None, false),
    };
    let mut v = json!({
        "name": m.name,
        "doubleSided": true,
        "pbrMetallicRoughness": { "baseColorFactor": [1.0, 1.0, 1.0, 1.0], "metallicFactor": m.metallic, "roughnessFactor": m.roughness },
    });
    if let Some([color, normal, orm]) = textures {
        let t = tint.map(|c| linear(c.clamp(0.0, 1.0)));
        v["pbrMetallicRoughness"] = json!({
            "baseColorFactor": [t[0], t[1], t[2], 1.0],
            "baseColorTexture": { "index": color },
            "metallicFactor": 1.0,
            "roughnessFactor": 1.0,
            "metallicRoughnessTexture": { "index": orm },
        });
        v["normalTexture"] = json!({ "index": normal });
        v["occlusionTexture"] = json!({ "index": orm });
        if cutout {
            v["alphaMode"] = json!("MASK");
            v["alphaCutoff"] = json!(0.5);
        }
    }
    if m.name == "fire_flipbook" { v["alphaMode"] = json!("BLEND"); v.as_object_mut().unwrap().remove("alphaCutoff"); }
    if m.clearcoat > 0.0 {
        v["extensions"]["KHR_materials_clearcoat"] = json!({ "clearcoatFactor": m.clearcoat, "clearcoatRoughnessFactor": 0.06 });
    }
    if m.sheen > 0.0 {
        // Sheen takes the surface color, so dark cloth is not washed out:
        // the color texture when baked; a dim gray over vertex colors.
        let mut sheen = json!({ "sheenColorFactor": [m.sheen, m.sheen, m.sheen], "sheenRoughnessFactor": 0.5 });
        match textures {
            Some([color, _, _]) => sheen["sheenColorTexture"] = json!({ "index": color }),
            None => sheen["sheenColorFactor"] = json!([0.25 * m.sheen, 0.25 * m.sheen, 0.25 * m.sheen]),
        }
        v["extensions"]["KHR_materials_sheen"] = sheen;
    }
    if m.emission > 0.0 {
        // The glow takes the surface color: the color texture when baked,
        // else the object tint.
        match textures {
            Some([color, _, _]) => {
                v["emissiveTexture"] = json!({ "index": color });
                v["emissiveFactor"] = json!([1.0, 1.0, 1.0]);
            }
            None => v["emissiveFactor"] = json!(tint.map(|c| linear(c.clamp(0.0, 1.0)))),
        }
        if m.emission != 1.0 {
            v["extensions"]["KHR_materials_emissive_strength"] = json!({ "emissiveStrength": m.emission });
        }
    }
    if m.transmission > 0.0 {
        v["extensions"]["KHR_materials_transmission"] = json!({ "transmissionFactor": m.transmission });
        v["extensions"]["KHR_materials_ior"] = json!({ "ior": m.ior });
    }
    v
}

/// Adds an object's mesh (and rig) under its node.
/// glTF point light intensity (candela) for one unit of `LightSource::intensity`.
pub const CANDELA_PER_UNIT: f32 = 50.0;

/// A KHR_lights_punctual point light under `node`, at the light's offset.
fn add_light(b: &mut Builder, light: &crate::envkit::light::LightSource, name: &str, node: usize, children: &mut BTreeMap<usize, Vec<usize>>) {
    let color = light.color.map_or_else(|| crate::envkit::light::blackbody(light.temperature), Vec3::from);
    let color = color / color.max_element().max(1e-6);
    let lum = color.dot(Vec3::new(0.2126, 0.7152, 0.0722)).max(1e-6);
    b.lights.push(json!({ "type": "point", "name": name, "color": color.to_array(), "intensity": light.intensity / lum * CANDELA_PER_UNIT }));
    let l = b.node(json!({ "name": format!("{name}_light"), "translation": light.offset.to_array(), "extensions": { "KHR_lights_punctual": { "light": b.lights.len() - 1 } } }));
    children.entry(node).or_default().push(l);
}

#[allow(clippy::too_many_arguments)]
fn add_object(b: &mut Builder, o: &Object, own: &Material, node: usize, meshes: &MeshLib, opts: &Options, children: &mut BTreeMap<usize, Vec<usize>>, scene_nodes: &mut Vec<usize>, scene: &Scene) -> Result<(), String> {
    if let Some(l) = &o.light {
        add_light(b, l, &o.name, node, children);
    }
    if let Some(fire) = o.fire.as_ref().filter(|f| f.valid()) {
        let mesh = crate::fire::cards(fire, None, true);
        let m = b.mesh(&o.name, &mesh, [1.0;3], own, None)?;
        b.nodes[node]["mesh"] = json!(m);
        let atlas_size = mesh.textures.as_ref().unwrap().size;
        b.nodes[node]["extras"]["maya_fire"] = json!({"version":1,"layout":"columns=frames; rows=views; top-left origin", "tile_size":64,"atlas_size":atlas_size,"frames":fire.cache.len().min(16),"views":2,"fps":fire.settings.fps*(fire.cache.len().min(16).saturating_sub(1).max(1) as f32)/(fire.cache.len().saturating_sub(1).max(1) as f32),"simulation_fps":fire.settings.fps,"sample_frames":(0..fire.cache.len().min(16)).map(|i|i*(fire.cache.len()-1)/fire.cache.len().min(16).saturating_sub(1).max(1)).collect::<Vec<_>>(),"start_frame":fire.settings.start,"duration":fire.cache.len() as f32/fire.settings.fps,"loop":false,"cards":"cross; optionally rotate to camera about local Y","uv_step":[64.0/atlas_size as f32,0.0],"emissive":"color atlas times emissiveStrength; animate UV in engine"});
        return Ok(());
    }
    if o.shape == Shape::Group {
        return Ok(());
    }
    // Scattered copies: a node each, sharing their source's mesh.
    if let Some(sc) = o.scatter.as_ref().filter(|_| o.shape == Shape::Scatter) {
        // Or one node per source carrying every copy's placement, for
        // sources with several copies and no light (lights need a node each).
        let mut grouped = vec![false; sc.sources.len()];
        if opts.gpu_instancing {
            for (k, &id) in sc.sources.iter().enumerate() {
                let Some(src) = scene.get(id).filter(|s| s.light.is_none()) else { continue };
                let Some(mesh) = meshes.get(src).filter(|m| !m.vertices.is_empty()) else { continue };
                let places: Vec<Mat4> = sc.instances.iter().filter(|i| i.source as usize == k).map(|i| i.matrix() * crate::envkit::scatter::source_shape(src)).collect();
                if places.len() < 2 {
                    continue;
                }
                grouped[k] = true;
                let m = b.shared_mesh(&src.name, mesh, src.color, &scene.material_of(src))?;
                let (mut t, mut r, mut s) = (Vec::new(), Vec::new(), Vec::new());
                for p in &places {
                    let (sc, rot, tr) = p.to_scale_rotation_translation();
                    t.extend(tr.to_array());
                    r.extend(rot.normalize().to_array());
                    s.extend(sc.to_array());
                }
                let attributes = json!({ "TRANSLATION": b.floats(&t, 3, None, false), "ROTATION": b.floats(&r, 4, None, false), "SCALE": b.floats(&s, 3, None, false) });
                let copy = b.node(json!({ "name": format!("{}_copies", src.name), "mesh": m, "extensions": { "EXT_mesh_gpu_instancing": { "attributes": attributes } } }));
                children.entry(node).or_default().push(copy);
                // Counted per copy, like copies written as nodes.
                b.vertices += mesh.vertices.len() * (places.len() - 1);
                b.triangles += mesh.indices.len() / 3 * (places.len() - 1);
                b.copies += places.len();
                b.instanced_nodes += 1;
            }
        }
        for (i, inst) in sc.instances.iter().enumerate().filter(|(_, i)| !grouped.get(i.source as usize).copied().unwrap_or(false)) {
            let Some(src) = sc.sources.get(inst.source as usize).and_then(|&id| scene.get(id)) else { continue };
            let Some(mesh) = meshes.get(src).filter(|m| !m.vertices.is_empty()) else { continue };
            let m = b.shared_mesh(&src.name, mesh, src.color, &scene.material_of(src))?;
            let mut n = trs(&format!("{}_{i}", src.name), inst.matrix() * crate::envkit::scatter::source_shape(src));
            n["mesh"] = json!(m);
            let copy = b.node(n);
            children.entry(node).or_default().push(copy);
            b.copies += 1;
            if let Some(l) = &src.light {
                add_light(b, l, &format!("{}_{i}", src.name), copy, children);
            }
        }
        return Ok(());
    }
    if o.shape == Shape::Part && add_span(b, o, own, scene, opts, children, scene_nodes)? {
        return Ok(());
    }
    let (mesh, skin) = match o.shape {
        // Baked clay exports its game mesh (see `bake.rs`).
        Shape::Clay | Shape::Cloth if o.bake.is_some() && meshes.baked(o.id).is_some() => {
            let (m, s) = meshes.baked(o.id).expect("checked");
            (m.clone(), s.cloned())
        }
        Shape::Clay | Shape::Cloth => match meshes.rest(o.id) {
            Some((m, s)) => (m.clone(), s.cloned()),
            None => return Ok(()),
        },
        _ => match meshes.get(o) {
            Some(m) => (m.clone(), None),
            None => return Ok(()),
        },
    };
    if mesh.vertices.is_empty() {
        return Ok(());
    }
    let cache = o.cloth.as_ref().filter(|_| opts.animations).and_then(|c| crate::cloth::export_cache(c, &mesh, own, scene.rig_of(o)));
    let skin = cache.as_ref().map(|c| std::sync::Arc::new(c.1.clone())).or(skin);
    let rig = cache.as_ref().map(|c| &c.0).or_else(|| scene.rig_of(o)).filter(|r| !r.bones.is_empty());
    // Lower levels of detail for baked game meshes (MSFT_lod: the LOD
    // nodes hang off the LOD0 node only, so readers without the extension
    // show just LOD0).
    let lods = if opts.lods && mesh.textures.is_some() && cache.is_none() { crate::bake::lods(&mesh, skin.as_deref()) } else { Vec::new() };
    // Blend shapes of this object's mesh (`layers::project_shape`).
    let shapes: Vec<&BlendShape> = opts.blend_shapes.iter().filter(|s| s.object == o.id && s.deltas.len() == mesh.vertices.len()).collect();
    let (Some(rig), Some(skin)) = (rig, skin.filter(|s| s.joints.len() == mesh.vertices.len())) else {
        // Objects showing the same mesh (identical clay) share it.
        let m = if lods.is_empty() && cache.is_none() && shapes.is_empty() { b.shared_mesh(&o.name, &mesh, o.color, own)? } else { b.mesh(&o.name, &mesh, o.color, own, None)? };
        if !shapes.is_empty() {
            b.targets(m, &shapes);
        }
        b.nodes[node]["mesh"] = json!(m);
        add_lods(b, node, &o.name, &lods, o.color, own, None)?;
        return Ok(());
    };

    // Joints under the object's node, so its placement moves the skeleton.
    let mut joints = Vec::with_capacity(rig.bones.len());
    for (i, bone) in rig.bones.iter().enumerate() {
        let parent = bone.parent.as_deref().and_then(|p| rig.bones[..i].iter().position(|x| x.name == p));
        let base = parent.map_or(Vec3::ZERO, |p| rig.bones[p].head);
        let j = b.node(json!({ "name": bone.name, "translation": (bone.head - base).to_array() }));
        children.entry(parent.map_or(node, |p| joints[p])).or_default().push(j);
        joints.push(j);
        b.joints.insert((o.id, bone.name.clone()), (j, bone.head));
    }
    let ibm: Vec<f32> = rig.bones.iter().flat_map(|bone| Mat4::from_translation(-bone.head).to_cols_array()).collect();
    let ibm = b.floats(&ibm, 16, None, false);
    let root = rig.bones.iter().position(|x| x.parent.is_none()).map(|i| joints[i]);
    let mut skin_json = json!({ "name": o.name, "joints": joints, "inverseBindMatrices": ibm });
    if let Some(r) = root {
        skin_json["skeleton"] = json!(r);
    }
    b.skins.push(skin_json);
    let skin_index = b.skins.len() - 1;
    // glTF ignores a skinned mesh node's own transform; keep it at the root.
    let m = b.mesh(&o.name, &mesh, o.color, own, Some(&*skin))?;
    if !shapes.is_empty() {
        b.targets(m, &shapes);
    }
    let mesh_node = b.node(json!({ "name": format!("{}_skin", o.name), "mesh": m, "skin": skin_index }));
    scene_nodes.push(mesh_node);
    add_lods(b, mesh_node, &o.name, &lods, o.color, own, Some(skin_index))?;

    if opts.animations {
        add_clips(b, rig, &joints, opts.fps);
    }
    Ok(())
}

/// A two-bone part (see `kitbash/span.rs`) on the joints of its rig: a
/// hose or cable as a mesh skinned to both joints, weights blended along
/// its length; a piston as a cylinder node under the first joint and a rod
/// node under the second, each animated to aim at the other. False when
/// it is not one (or its rig's joints were not written).
#[allow(clippy::too_many_arguments)]
fn add_span(b: &mut Builder, o: &Object, own: &Material, scene: &Scene, opts: &Options, children: &mut BTreeMap<usize, Vec<usize>>, scene_nodes: &mut Vec<usize>) -> Result<bool, String> {
    use crate::kitbash::span;
    let Some(part) = o.part.as_ref().filter(|p| p.span.is_some() && p.points.len() >= 2) else { return Ok(false) };
    let sp = part.span.as_ref().expect("checked");
    let Some(h) = span::hang(scene, o) else { return Ok(false) };
    let Some(rig) = scene.get(h.rig).and_then(|r| r.rig.as_ref()) else { return Ok(false) };
    let (Some(&(ja, head_a)), Some(&(jb, head_b))) = (b.joints.get(&(h.rig, sp.bones[0].clone())), b.joints.get(&(h.rig, sp.bones[1].clone()))) else { return Ok(false) };
    let (Some(ia), Some(ib)) = (rig.bone_index(&sp.bones[0]), rig.bone_index(&sp.bones[1])) else { return Ok(false) };
    let pts: Vec<Vec3> = part.points.iter().map(|p| h.to_rig.transform_point3(Vec3::from(*p))).collect();
    // Part space -> a space; normals turn with it.
    let moved = |mesh: &Mesh, m: Mat4| -> Mesh {
        let mut out = mesh.clone();
        let n = glam::Mat3::from_mat4(m).inverse().transpose();
        for v in &mut out.vertices {
            v.pos = m.transform_point3(Vec3::from(v.pos)).to_array();
            v.normal = (n * Vec3::from(v.normal)).normalize_or(Vec3::Y).to_array();
        }
        out
    };
    if part.kind != crate::kitbash::Kind::Piston {
        let mesh = moved(&part.build(&scene.materials), h.to_rig);
        // How far along the part each vertex is.
        let w = span::blend_weights(&pts);
        let along = |p: Vec3| -> f32 {
            let mut best = (f32::MAX, 0.0);
            for i in 0..pts.len() - 1 {
                let (a, c) = (pts[i], pts[i + 1]);
                let d = c - a;
                let t = ((p - a).dot(d) / d.length_squared().max(1e-12)).clamp(0.0, 1.0);
                let dist = p.distance(a + d * t);
                if dist < best.0 {
                    best = (dist, w[i] + (w[i + 1] - w[i]) * t);
                }
            }
            best.1
        };
        let weights: Vec<[f32; 4]> = mesh.vertices.iter().map(|v| { let t = along(Vec3::from(v.pos)); [1.0 - t, t, 0.0, 0.0] }).collect();
        let skin = Skin { joints: vec![[0, 1, 0, 0]; mesh.vertices.len()], weights };
        let ibm: Vec<f32> = [head_a, head_b].iter().flat_map(|h| Mat4::from_translation(-*h).to_cols_array()).collect();
        let ibm = b.floats(&ibm, 16, None, false);
        b.skins.push(json!({ "name": o.name, "joints": [ja, jb], "inverseBindMatrices": ibm }));
        let skin_index = b.skins.len() - 1;
        let m = b.mesh(&o.name, &mesh, o.color, own, Some(&skin))?;
        scene_nodes.push(b.node(json!({ "name": format!("{}_skin", o.name), "mesh": m, "skin": skin_index })));
        return Ok(true);
    }
    // Piston: two rigid halves aimed at each other.
    let (a, c) = (pts[0], pts[pts.len() - 1]);
    let dir = (c - a).normalize_or(Vec3::Y);
    let side = dir.cross(Vec3::Y).normalize_or(dir.any_orthonormal_vector());
    let r_cyl = Quat::from_mat3(&glam::Mat3::from_cols(side, dir, side.cross(dir))).normalize();
    let r_rod = (r_cyl * Quat::from_rotation_x(std::f32::consts::PI)).normalize();
    let rest = a.distance(c);
    for (half, joint, head, origin, r0, bone) in [(0u8, ja, head_a, a, r_cyl, ia), (1u8, jb, head_b, c, r_rod, ib)] {
        let mut p = part.clone();
        if let Some(s) = p.span.as_mut() {
            s.half = Some(half);
            s.rest_length = Some(rest);
        }
        let frame = Mat4::from_rotation_translation(r0, origin);
        let mesh = moved(&p.build(&scene.materials), frame.inverse() * h.to_rig);
        let m = b.mesh(&format!("{}_{}", o.name, if half == 0 { "cylinder" } else { "rod" }), &mesh, o.color, own, None)?;
        let node = b.node(json!({ "name": format!("{}_{}", o.name, if half == 0 { "cylinder" } else { "rod" }), "mesh": m, "translation": (origin - head).to_array(), "rotation": r0.to_array() }));
        children.entry(joint).or_default().push(node);
        if !opts.animations {
            continue;
        }
        // Every clip, every frame: aim at the other end as the rig moves it.
        let (other, other_bone) = if half == 0 { (c, ib) } else { (a, ia) };
        let fps = opts.fps.clamp(1.0, 120.0);
        for clip in &rig.clips {
            let Some((start, end)) = clip.range() else { continue };
            let mut frames: Vec<f32> = (0..).map(|i| start + i as f32).take_while(|f| *f < end - 1e-3).take(100_000).collect();
            frames.push(end);
            let times: Vec<f32> = frames.iter().map(|f| (f - start) / fps).collect();
            let input = b.floats(&times, 1, None, true);
            let mut rot = Vec::with_capacity(frames.len() * 4);
            let mut prev = Quat::IDENTITY;
            for &f in &frames {
                let g = rig.globals(&rig.clip_poses(clip, f));
                let skin_other = g[other_bone] * Mat4::from_translation(-rig.bones[other_bone].head);
                let target = g[bone].inverse().transform_point3(skin_other.transform_point3(other));
                let d = (target - (origin - head)).normalize_or(r0 * Vec3::Y);
                let q = (Quat::from_rotation_arc((r0 * Vec3::Y).normalize(), d) * r0).normalize();
                let q = if q.dot(prev) < 0.0 { -q } else { q };
                rot.extend_from_slice(&q.to_array());
                prev = q;
            }
            b.sampler(&clip.name, input, &rot, 4, node, "rotation");
        }
    }
    Ok(true)
}

/// LOD1-LOD3 of the mesh on `node` (MSFT_lod), each its own mesh and node
/// (with the same skin when skinned).
fn add_lods(b: &mut Builder, node: usize, name: &str, lods: &[(Mesh, Option<Skin>)], tint: [f32; 3], own: &Material, skin: Option<usize>) -> Result<(), String> {
    if lods.is_empty() {
        return Ok(());
    }
    let (vertices, triangles) = (b.vertices, b.triangles);
    let mut ids = Vec::new();
    for (k, (lod, s)) in lods.iter().enumerate() {
        let m = b.mesh(&format!("{name}_LOD{}", k + 1), lod, tint, own, s.as_ref().filter(|_| skin.is_some()))?;
        let mut n = json!({ "name": format!("{name}_LOD{}", k + 1), "mesh": m });
        if let Some(sk) = skin {
            n["skin"] = json!(sk);
        }
        ids.push(b.node(n));
    }
    // Counts stay those of the shown (LOD0) meshes.
    (b.vertices, b.triangles) = (vertices, triangles);
    b.lods.push((name.to_string(), lods.iter().map(|l| l.0.indices.len() / 3).collect()));
    // Swap to the next level as the object shrinks on screen.
    let coverage: Vec<f32> = [0.4, 0.2, 0.08, 0.0].into_iter().take(ids.len() + 1).collect();
    b.nodes[node]["extensions"] = json!({ "MSFT_lod": { "ids": ids } });
    b.nodes[node]["extras"] = json!({ "MSFT_screencoverage": coverage });
    Ok(())
}

/// Samples every clip once per frame (plus the exact last frame).
fn add_clips(b: &mut Builder, rig: &Rig, joints: &[usize], fps: f32) {
    let fps = fps.clamp(1.0, 120.0);
    for clip in &rig.clips {
        let Some((start, end)) = clip.range() else { continue };
        let mut frames: Vec<f32> = (0..).map(|i| start + i as f32).take_while(|f| *f < end - 1e-3).take(100_000).collect();
        frames.push(end);
        let times: Vec<f32> = frames.iter().map(|f| (f - start) / fps).collect();
        let input = b.floats(&times, 1, None, true);
        for track in &clip.tracks {
            let Some(i) = rig.bone_index(&track.bone) else { continue };
            let bone = &rig.bones[i];
            let parent = bone.parent.as_deref().and_then(|p| rig.bone_index(p));
            let base = bone.head - parent.map_or(Vec3::ZERO, |p| rig.bones[p].head);
            let poses: Vec<_> = frames.iter().map(|&f| track.sample(f)).collect();
            let mut rot: Vec<f32> = Vec::with_capacity(poses.len() * 4);
            let mut prev = Quat::IDENTITY;
            for p in &poses {
                // Neighbors in the same hemisphere, so players blend the short way.
                let q = if p.rotate.dot(prev) < 0.0 { -p.rotate } else { p.rotate };
                rot.extend_from_slice(&q.to_array());
                prev = q;
            }
            b.sampler(&clip.name, input, &rot, 4, joints[i], "rotation");
            if poses.iter().any(|p| p.translate != Vec3::ZERO) {
                let t: Vec<f32> = poses.iter().flat_map(|p| (base + p.translate).to_array()).collect();
                b.sampler(&clip.name, input, &t, 3, joints[i], "translation");
            }
            if poses.iter().any(|p| p.scale != Vec3::ONE) {
                let s: Vec<f32> = poses.iter().flat_map(|p| p.scale.to_array()).collect();
                b.sampler(&clip.name, input, &s, 3, joints[i], "scale");
            }
        }
    }
}

/// The extensions the materials written use.
fn material_extensions(b: &Builder) -> Vec<&'static str> {
    let mut used = Vec::new();
    if b.materials.iter().any(|m| m.0.clearcoat > 0.0) {
        used.push("KHR_materials_clearcoat");
    }
    if b.materials.iter().any(|m| m.0.sheen > 0.0) {
        used.push("KHR_materials_sheen");
    }
    if b.materials.iter().any(|m| m.0.emission > 0.0 && m.0.emission != 1.0) {
        used.push("KHR_materials_emissive_strength");
    }
    if b.materials.iter().any(|m| m.0.transmission > 0.0) {
        used.extend(["KHR_materials_transmission", "KHR_materials_ior"]);
    }
    used
}

/// One Poly level as a .glb (`export_poly`): `mesh` (with its textures,
/// if baked) under one node at `world`, and the subdivision cage in the
/// mesh's `extras.clide3d_cage`: the points as a float VEC3 accessor, the
/// UVs (per corner, top-left origin as glTF's) as a VEC2 accessor, and
/// the integer arrays as plain buffer views (little-endian u32: face
/// sizes, corners, and creases as (u32 a, u32 b, f32 sharpness)
/// records), since glTF keeps UNSIGNED_INT accessors for indices.
/// `extras` adds the conventions.
pub(crate) fn poly_glb(name: &str, mesh: &Mesh, own: &Material, world: Mat4, cage: &crate::poly::export::Cage, extras: Value) -> Result<Vec<u8>, String> {
    let mut b = Builder::default();
    let m = b.mesh(name, mesh, [1.0; 3], own, None)?;
    let points: Vec<f32> = cage.points.iter().flat_map(|p| p.to_array()).collect();
    let mut x = json!({ "points": b.floats(&points, 3, None, true) });
    let sizes: Vec<u32> = cage.face_start.windows(2).map(|w| w[1] - w[0]).collect();
    x["face_sizes"] = json!({ "bufferView": b.view(bytemuck::cast_slice(&sizes), None), "count": sizes.len(), "componentType": "u32" });
    x["corners"] = json!({ "bufferView": b.view(bytemuck::cast_slice(&cage.corners), None), "count": cage.corners.len(), "componentType": "u32" });
    if let Some(uv) = &cage.uv {
        let flat: Vec<f32> = uv.iter().flatten().copied().collect();
        x["uv"] = json!(b.floats(&flat, 2, None, false));
    }
    if !cage.creases.is_empty() {
        let mut rec = Vec::with_capacity(cage.creases.len() * 12);
        for &(a, c, s) in &cage.creases {
            rec.extend(a.to_le_bytes());
            rec.extend(c.to_le_bytes());
            rec.extend(s.to_le_bytes());
        }
        x["creases"] = json!({ "bufferView": b.view(&rec, None), "count": cage.creases.len(), "layout": "u32 a, u32 b, f32 sharpness" });
    }
    for (k, v) in extras.as_object().into_iter().flatten() {
        x[k] = v.clone();
    }
    b.meshes[m]["extras"] = json!({ "clide3d_cage": x });
    let mut node = trs(name, world);
    node["mesh"] = json!(m);
    let node = b.node(node);
    let mut doc = json!({
        "asset": { "version": "2.0", "generator": "Clide3D export_poly" },
        "scene": 0,
        "scenes": [{ "nodes": [node] }],
        "nodes": b.nodes,
        "meshes": b.meshes,
        "materials": b.materials.iter().map(|(m, set, tint)| material_json(m, set.map(|s| (b.texture_sets[s].1, b.texture_sets[s].0.alpha)), tint.map(f32::from_bits))).collect::<Vec<_>>(),
        "accessors": b.accessors,
        "bufferViews": b.views,
        "buffers": [{ "byteLength": b.bin.len() }],
    });
    if !b.textures.is_empty() {
        doc["images"] = json!(b.images);
        doc["textures"] = json!(b.textures);
        doc["samplers"] = json!([{ "magFilter": 9729, "minFilter": 9987, "wrapS": 33071, "wrapT": 33071 }]);
    }
    let used = material_extensions(&b);
    if !used.is_empty() {
        doc["extensionsUsed"] = json!(used);
    }
    // A .glb's lengths are 32-bit.
    if b.bin.len() as u64 + (1 << 20) > u32::MAX as u64 {
        return Err("the .glb would pass 4 GiB: write a lower level".into());
    }
    Ok(glb(&doc, b.bin))
}

/// Packs the JSON and the binary buffer into a .glb file.
fn glb(doc: &Value, mut bin: Vec<u8>) -> Vec<u8> {
    let mut json = serde_json::to_vec(doc).expect("JSON values always serialize");
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

/// True if the bytes start like a .glb file.
pub fn is_glb(bytes: &[u8]) -> bool {
    bytes.starts_with(b"glTF")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clay::{Blob, Clay, Form};
    use crate::rig::{Bone, Clip, Key};

    /// Splits a .glb into its JSON and binary chunk, checking the framing.
    fn parse(bytes: &[u8]) -> (Value, Vec<u8>) {
        let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
        assert_eq!(&bytes[0..4], b"glTF");
        assert_eq!(u32_at(4), 2);
        assert_eq!(u32_at(8), bytes.len());
        let jlen = u32_at(12);
        assert_eq!(&bytes[16..20], b"JSON");
        let doc: Value = serde_json::from_slice(&bytes[20..20 + jlen]).unwrap();
        let b0 = 20 + jlen;
        let blen = u32_at(b0);
        assert_eq!(&bytes[b0 + 4..b0 + 8], b"BIN\0");
        assert_eq!(b0 + 8 + blen, bytes.len());
        (doc, bytes[b0 + 8..].to_vec())
    }

    #[test]
    fn scatter_copies_share_one_mesh_and_lights_export() {
        use crate::envkit::kit::{Kit, Piece};
        use crate::envkit::scatter::{Instance, Scatter};
        let mut scene = Scene::default();
        let candle = scene.add(Shape::Kit);
        let o = scene.get_mut(candle).unwrap();
        o.kit = Some(Kit::new(Piece::Candle));
        o.light = Piece::Candle.light();
        o.visible = false;
        let torch = scene.add(Shape::Kit);
        let o = scene.get_mut(torch).unwrap();
        o.kit = Some(Kit::new(Piece::Torch));
        o.light = Piece::Torch.light();
        let sc = scene.add(Shape::Scatter);
        let instances = (0..3).map(|i| Instance { source: 0, position: Vec3::new(i as f32, 0.0, 0.0), rotation: Quat::IDENTITY, scale: 1.0 }).collect();
        scene.get_mut(sc).unwrap().scatter = Some(Scatter { sources: vec![candle], instances, settings: Default::default(), placed_by: None });
        let mut meshes = MeshLib::new();
        meshes.sync(&scene);
        let opts = Options { roots: vec![], animations: true, fps: 24.0, file_stem: String::new(), lods: false, atlas: true, texel_density: None, gpu_instancing: false, blend_shapes: Vec::new() };
        let out = export(&scene, &meshes, &opts).unwrap();
        let (doc, _) = parse(&out.bytes);
        // The torch and one candle mesh; three nodes point at the candle's.
        assert_eq!(doc["meshes"].as_array().unwrap().len(), 2);
        let users: Vec<u64> = doc["nodes"].as_array().unwrap().iter().filter_map(|n| n["mesh"].as_u64()).collect();
        let candle_mesh = users.iter().copied().find(|m| users.iter().filter(|x| *x == m).count() == 3).expect("three copies share a mesh");
        assert_eq!(users.iter().filter(|m| **m != candle_mesh).count(), 1);
        // Every lit copy and the torch carry a point light.
        assert_eq!(doc["extensions"]["KHR_lights_punctual"]["lights"].as_array().unwrap().len(), 4);
        assert!(doc["extensionsUsed"].as_array().unwrap().iter().any(|e| e == "KHR_lights_punctual"));
        assert_eq!(out.summary["scatter_copies"], 3);
        // FBX: the copies are Models sharing one Geometry.
        let fbx = crate::fbx::export(&scene, &meshes, &opts).unwrap();
        assert_eq!(fbx.summary["geometries"], 2, "{}", fbx.summary);
    }

    #[test]
    fn rigged_animated_clay_exports_a_consistent_file() {
        let mut scene = Scene::default();
        let cube = scene.add(Shape::Cube);
        let id = scene.add(Shape::Clay);
        scene.get_mut(cube).unwrap().translation = Vec3::new(2.0, 0.0, 0.0);
        let o = scene.get_mut(id).unwrap();
        let capsule = Form::Capsule { a: Vec3::ZERO, b: Vec3::new(0.0, 2.0, 0.0), radius: 0.3, radius_b: None };
        o.clay = Some(Clay { blobs: vec![Blob { name: String::new(), form: capsule, op: Default::default(), blend: 0.1, color: [0.8; 3], mirror: false, flip: false, material: String::new(), bone: String::new(), color_blend: None, join: Default::default(), join_size: None, wall: 0.004, rim: 0.0, ridge: 0.0, flare: 0.0, smooth: 0.0, offset: 0.004 }], resolution: 32, ..Default::default() });
        let bone = |n: &str, p: Option<&str>, a: f32, b: f32| Bone { name: n.into(), parent: p.map(Into::into), head: Vec3::new(0.0, a, 0.0), tail: Vec3::new(0.0, b, 0.0), deform: true };
        let mut clip = Clip::new("bend");
        let mut k = Key::new(10.0);
        k.rotate = Vec3::new(0.0, 0.0, 60.0);
        clip.track_mut("top").insert(Key::new(1.0));
        clip.track_mut("top").insert(k);
        o.rig = Some(Rig { bones: vec![bone("base", None, 0.0, 1.0), bone("top", Some("base"), 1.0, 2.0)], clips: vec![clip], active: None, split: true, weights: Vec::new() });
        o.rig.as_mut().unwrap().weights.push(crate::rig::WeightStroke {
            bone: "base".into(), points: vec![Vec3::new(0.0,1.8,0.0)], radius: 0.7,
            strength: 0.65, mode: crate::rig::WeightMode::Replace, space: glam::Mat4::IDENTITY,
        });
        let mut meshes = MeshLib::new();
        meshes.sync(&scene);

        let out = export(&scene, &meshes, &Options { roots: vec![], animations: true, fps: 24.0, file_stem: String::new(), lods: false, atlas: true, texel_density: None, gpu_instancing: false, blend_shapes: Vec::new() }).unwrap();
        let (doc, bin) = parse(&out.bytes);
        assert_eq!(doc["meshes"].as_array().unwrap().len(), 2);
        assert_eq!(doc["skins"][0]["joints"].as_array().unwrap().len(), 2);
        let loaded = crate::glb_read::from_bytes(&out.bytes, None, None).unwrap();
        assert_eq!(loaded.summary["vertices"], out.summary["vertices"]);
        assert_eq!(loaded.summary["triangles"], out.summary["triangles"]);
        assert_eq!(loaded.summary["skins"][0]["joints"], 2);
        assert_eq!(loaded.summary["animations"][0]["fps"], 24.0);
        let posed = crate::glb_read::from_bytes(&out.bytes, Some("bend"), Some(5.0)).unwrap();
        let (rest, skin) = meshes.rest(id).unwrap();
        let mut rig = scene.get(id).unwrap().rig.clone().unwrap();
        rig.active = Some("bend".into());
        // glTF clip time zero is editor frame 1; file frame 5 is editor frame 6.
        let expected = crate::rig::pose_mesh(rest, skin.unwrap(), &rig.skin_matrices(Some(6.0)));
        let actual = &posed.items.last().unwrap().mesh;
        assert_eq!(actual.vertices.len(), expected.vertices.len());
        for (a, b) in actual.vertices.iter().zip(&expected.vertices) {
            assert!(Vec3::from(a.pos).distance(Vec3::from(b.pos)) < 2e-5);
        }
        let anim = &doc["animations"][0];
        assert_eq!(anim["name"], "bend");
        // Frames 1..=10 -> 10 samples, last at 9/24 s.
        let input = &doc["accessors"][anim["samplers"][0]["input"].as_u64().unwrap() as usize];
        assert_eq!(input["count"], 10);
        assert!((input["max"][0].as_f64().unwrap() - 9.0 / 24.0).abs() < 1e-6);

        // Every accessor fits its view, every view fits the buffer.
        assert_eq!(doc["buffers"][0]["byteLength"].as_u64().unwrap() as usize, bin.len());
        for a in doc["accessors"].as_array().unwrap() {
            let v = &doc["bufferViews"][a["bufferView"].as_u64().unwrap() as usize];
            let comp = match a["componentType"].as_u64().unwrap() {
                5123 => 2,
                _ => 4,
            };
            let width = match a["type"].as_str().unwrap() {
                "SCALAR" => 1,
                "VEC3" => 3,
                "VEC4" => 4,
                _ => 16,
            };
            assert_eq!(a["count"].as_u64().unwrap() as usize * comp * width, v["byteLength"].as_u64().unwrap() as usize);
            let end = v["byteOffset"].as_u64().unwrap() + v["byteLength"].as_u64().unwrap();
            assert!(end as usize <= bin.len());
            assert_eq!(v["byteOffset"].as_u64().unwrap() % 4, 0);
        }
        // The skinned mesh node is a scene root and the joints hang under the clay's node.
        let roots = doc["scenes"][0]["nodes"].as_array().unwrap();
        assert!(roots.iter().any(|n| doc["nodes"][n.as_u64().unwrap() as usize]["skin"] == 0));
        assert!(out.summary["vertices"].as_u64().unwrap() > 100);
    }

    /// LODs: glTF gets three MSFT_lod levels off the LOD0 node (not in the
    /// scene, so plain readers see one mesh), FBX a LodGroup of LOD0-LOD3;
    /// each level has fewer triangles, all share the bake's textures.
    #[test]
    fn baked_exports_carry_three_lods() {
        let mut scene = Scene::default();
        let id = scene.add(Shape::Clay);
        let o = scene.get_mut(id).unwrap();
        o.name = "Rock".into();
        o.clay = Some(serde_json::from_value(json!({ "blobs": [{ "type": "ellipsoid", "center": [0, 0, 0], "radii": [0.5, 0.3, 0.4] }], "resolution": 64 })).unwrap());
        let settings = crate::bake::Settings { triangles: 8000, texture: 512, ..Default::default() };
        o.bake = Some(settings);
        let mut meshes = MeshLib::new();
        meshes.sync(&scene);
        meshes.bake(&scene, id, settings).unwrap();
        let opts = Options { roots: vec![], animations: true, fps: 24.0, file_stem: String::new(), lods: true, atlas: true, texel_density: None, gpu_instancing: false, blend_shapes: Vec::new() };
        let out = export(&scene, &meshes, &opts).unwrap();
        let (doc, _) = parse(&out.bytes);
        assert!(doc["extensionsUsed"].as_array().unwrap().iter().any(|e| e == "MSFT_lod"));
        let lod0 = doc["nodes"].as_array().unwrap().iter().find(|n| n["extensions"]["MSFT_lod"].is_object()).expect("a LOD0 node");
        let ids: Vec<usize> = lod0["extensions"]["MSFT_lod"]["ids"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(lod0["extras"]["MSFT_screencoverage"].as_array().unwrap().len(), 4);
        let roots: Vec<usize> = doc["scenes"][0]["nodes"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
        let triangles = |node: &Value| -> usize {
            let mesh = &doc["meshes"][node["mesh"].as_u64().unwrap() as usize];
            mesh["primitives"].as_array().unwrap().iter().map(|p| doc["accessors"][p["indices"].as_u64().unwrap() as usize]["count"].as_u64().unwrap() as usize / 3).sum()
        };
        let mut counts = vec![triangles(lod0)];
        for &i in &ids {
            assert!(!roots.contains(&i), "LOD nodes stay out of the scene");
            assert_eq!(doc["nodes"][i]["name"], format!("Rock_LOD{}", counts.len()));
            counts.push(triangles(&doc["nodes"][i]));
        }
        println!("LOD triangles {counts:?}");
        assert!(counts.windows(2).all(|w| w[1] < w[0]), "{counts:?}");
        assert!(counts[3] * 4 <= counts[0], "{counts:?}");
        assert_eq!(out.summary["triangles"].as_u64().unwrap() as usize, counts[0]);
        // Every primitive points at the one baked texture set.
        assert_eq!(doc["images"].as_array().unwrap().len(), 3);
        // Plain readers load just LOD0.
        let loaded = crate::glb_read::from_bytes(&out.bytes, None, None).unwrap();
        assert_eq!(loaded.summary["triangles"].as_u64().unwrap() as usize, counts[0]);
        // FBX: a LodGroup with four mesh models.
        let fbx = crate::fbx::export(&scene, &meshes, &Options { file_stem: "rock".into(), ..opts }).unwrap();
        assert!(crate::fbx::is_fbx(&fbx.bytes));
        assert_eq!(fbx.summary["geometries"], 4);
        assert_eq!(fbx.summary["lods"]["Rock"]["lod1_3_triangles"], json!(counts[1..].to_vec()));
        assert!(fbx.bytes.windows(8).any(|w| w == b"LodGroup"));
        for k in 0..4 {
            let name = format!("Rock_LOD{k}");
            assert!(fbx.bytes.windows(name.len()).any(|w| w == name.as_bytes()), "{name}");
        }
        // Blend shapes on a LOD object go on LOD0 (fitted to its vertices),
        // in both formats; one fitted to other vertices is left out and
        // not listed as written.
        let n = meshes.baked(id).unwrap().0.vertices.len();
        let mut deltas = vec![[0.0f32; 3]; n];
        deltas[0] = [0.0, 0.01, 0.0];
        let shape = |name: &str, deltas: Vec<[f32; 3]>| BlendShape { object: id, name: name.into(), deltas: Arc::new(deltas) };
        let opts = Options { roots: vec![], animations: true, fps: 24.0, file_stem: "rock".into(), lods: true, atlas: true, texel_density: None, gpu_instancing: false, blend_shapes: vec![shape("bump", deltas), shape("other", vec![[0.0; 3]; n + 1])] };
        let fbx = crate::fbx::export(&scene, &meshes, &opts).unwrap();
        assert_eq!(fbx.shapes_written, vec![(id, "bump".to_string())]);
        assert_eq!(fbx.summary["blend_shapes"], 1);
        assert!(fbx.bytes.windows(17).any(|w| w == b"BlendShapeChannel"));
        let out = export(&scene, &meshes, &opts).unwrap();
        assert_eq!(out.shapes_written, vec![(id, "bump".to_string())]);
        let (doc, _) = parse(&out.bytes);
        let lod0 = doc["nodes"].as_array().unwrap().iter().find(|n| n["extensions"]["MSFT_lod"].is_object()).expect("a LOD0 node");
        assert_eq!(doc["meshes"][lod0["mesh"].as_u64().unwrap() as usize]["extras"]["targetNames"], json!(["bump"]));
    }

    #[test]
    fn baked_clay_exports_uvs_tangents_and_textures() {
        let mut scene = Scene::default();
        let id = scene.add(Shape::Clay);
        let o = scene.get_mut(id).unwrap();
        o.clay = Some(Clay { blobs: vec![Blob { name: String::new(), form: Form::Sphere { center: Vec3::ZERO, radius: 0.5 }, op: Default::default(), blend: 0.1, color: [0.8; 3], mirror: false, flip: false, material: "skin".into(), bone: String::new(), color_blend: None, join: Default::default(), join_size: None, wall: 0.004, rim: 0.0, ridge: 0.0, flare: 0.0, smooth: 0.0, offset: 0.004 }], resolution: 40, ..Default::default() });
        let settings = crate::bake::Settings { triangles: 600, texture: 512, ..Default::default() };
        o.bake = Some(settings);
        let mut meshes = MeshLib::new();
        meshes.sync(&scene);
        meshes.bake(&scene, id, settings).unwrap();
        let out = export(&scene, &meshes, &Options { roots: vec![], animations: true, fps: 24.0, file_stem: String::new(), lods: false, atlas: true, texel_density: None, gpu_instancing: false, blend_shapes: Vec::new() }).unwrap();
        let (doc, bin) = parse(&out.bytes);
        let loaded = crate::glb_read::from_bytes(&out.bytes, None, None).unwrap();
        assert_eq!(loaded.summary["triangles"], out.summary["triangles"]);
        assert_eq!(loaded.summary["vertices"], out.summary["vertices"]);
        assert_eq!(loaded.summary["images"].as_array().unwrap().len(), 3);
        let original = &meshes.baked(id).unwrap().0.textures.as_ref().unwrap();
        let decoded = loaded.items[0].mesh.textures.as_ref().unwrap();
        assert_eq!(decoded.size, original.size);
        assert_eq!(decoded.color, original.color);
        assert!(decoded.normal.chunks_exact(4).zip(original.normal.chunks_exact(4)).all(|(a, b)| (0..3).all(|i| (a[i] as i16 - b[i] as i16).abs() <= 1)));
        assert!(loaded.items[0].mesh.vertices.iter().all(|v| v.tangent[3] != 0.0));
        let prim = &doc["meshes"][0]["primitives"][0];
        assert!(prim["attributes"]["TEXCOORD_0"].is_u64() && prim["attributes"]["TANGENT"].is_u64());
        assert!(prim["attributes"]["COLOR_0"].is_null());
        assert!(doc["accessors"][prim["indices"].as_u64().unwrap() as usize]["count"].as_u64().unwrap() <= 650 * 3);
        let m = &doc["materials"][prim["material"].as_u64().unwrap() as usize];
        assert_eq!(m["name"], "skin");
        assert!(m["normalTexture"]["index"].is_u64() && m["pbrMetallicRoughness"]["baseColorTexture"]["index"].is_u64());
        assert_eq!(doc["images"].as_array().unwrap().len(), 3);
        // Every image is a real PNG inside the binary chunk.
        for img in doc["images"].as_array().unwrap() {
            let v = &doc["bufferViews"][img["bufferView"].as_u64().unwrap() as usize];
            let at = v["byteOffset"].as_u64().unwrap() as usize;
            assert!(bin[at..].starts_with(b"\x89PNG"));
        }
    }

    #[test]
    fn empty_scene_is_refused() {
        let scene = Scene::default();
        let meshes = MeshLib::new();
        assert!(export(&scene, &meshes, &Options { roots: vec![], animations: true, fps: 24.0, file_stem: String::new(), lods: false, atlas: true, texel_density: None, gpu_instancing: false, blend_shapes: Vec::new() }).is_err());
    }
}
