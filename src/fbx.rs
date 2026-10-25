//! Export to binary FBX 7.4 (.fbx), for tools that want FBX: Maya, Unity,
//! Unreal, Blender, Cinema 4D. (Blender cannot read text FBX, so this
//! writes the binary form.)
//!
//! The scene maps onto FBX like the glTF export (see `gltf.rs`):
//! - every visible object is a Model with its local translate / rotate /
//!   scale; groups are Null models;
//! - meshes are triangles with per-vertex normals and vertex colors, and
//!   one Phong material per Clide3D material (see `material.rs`), assigned
//!   per triangle. Each material's color is the average color of its part,
//!   because most tools show the material color before the vertex colors.
//!   Roughness becomes Shininess and metallic ReflectionFactor, which is how
//!   Blender reads them back; emission becomes EmissiveColor/Factor and
//!   glass TransparencyFactor; clearcoat and sheen have no FBX equivalent;
//! - rigged clay is a Null holding the mesh and a LimbNode skeleton, with a
//!   Skin of one Cluster per bone and a BindPose;
//! - every clip is an AnimationStack, sampled once per frame with linear
//!   keys (rotations as XYZ Euler degrees, kept continuous).
//!
//! Units: one scene unit is one meter (UnitScaleFactor 100 cm). Y is up and
//! characters face +Z, as in Clide3D.
//!
//! File layout: a header, then nested node records (name, typed properties,
//! child nodes), then a footer. The fixed FileId / CreationTime / footer
//! bytes are the ones Blender's exporter writes; FBX readers check that they
//! match each other.

use std::collections::BTreeMap;

use glam::{EulerRot, Mat4, Quat, Vec3};
use serde_json::json;

use crate::gltf::Options;
use crate::material::Material;
use std::sync::Arc;

use crate::mesh::{Mesh, MeshLib, Textures};
use crate::rig::{Rig, Skin};
use crate::scene::{Scene, Shape};

const VERSION: u32 = 7400;
/// FBX time units per second.
const KTIME_SECOND: f64 = 46_186_158_000.0;
const FILE_ID: [u8; 16] = [0x28, 0xb3, 0x2a, 0xeb, 0xb6, 0x24, 0xcc, 0xc2, 0xbf, 0xc8, 0xb0, 0x2a, 0xa9, 0x2b, 0xfc, 0xf1];
const CREATION_TIME: &str = "1970-01-01 10:00:00:000";
const FOOT_ID: [u8; 16] = [0xfa, 0xbc, 0xab, 0x09, 0xd0, 0xc8, 0xd4, 0x66, 0xb1, 0x76, 0xfb, 0x83, 0x1c, 0xf7, 0x26, 0x7e];
const FOOT_MAGIC: [u8; 16] = [0xf8, 0x5a, 0x8c, 0x6a, 0xde, 0xf5, 0xd9, 0x7e, 0xec, 0xe9, 0x0c, 0xe3, 0x75, 0x8f, 0x29, 0x0b];

/// One typed property of a node record.
enum P {
    Bool(bool),
    I32(i32),
    I64(i64),
    F64(f64),
    Str(String),
    Raw(Vec<u8>),
    F32s(Vec<f32>),
    F64s(Vec<f64>),
    I32s(Vec<i32>),
    I64s(Vec<i64>),
}

struct Node {
    name: &'static str,
    props: Vec<P>,
    children: Vec<Node>,
}

fn n(name: &'static str, props: Vec<P>) -> Node {
    Node { name, props, children: Vec::new() }
}

fn nc(name: &'static str, props: Vec<P>, children: Vec<Node>) -> Node {
    Node { name, props, children }
}

fn s(v: &str) -> P {
    P::Str(v.to_string())
}

/// "name" + class, the way FBX names objects inside binary files.
fn obj_name(name: &str, class: &str) -> P {
    P::Str(format!("{name}\u{0}\u{1}{class}"))
}

/// One `P:` line of a Properties70 block.
fn prop(name: &str, ty: &str, label: &str, flags: &str, values: Vec<P>) -> Node {
    let mut props = vec![s(name), s(ty), s(label), s(flags)];
    props.extend(values);
    n("P", props)
}

fn vec3_prop(name: &str, v: Vec3) -> Node {
    prop(name, name, "", "A", vec![P::F64(v.x as f64), P::F64(v.y as f64), P::F64(v.z as f64)])
}

impl P {
    fn write(&self, out: &mut Vec<u8>) {
        let array = |out: &mut Vec<u8>, code: u8, len: usize, bytes: &[u8]| {
            out.push(code);
            out.extend_from_slice(&(len as u32).to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes()); // not compressed
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
        };
        match self {
            P::Bool(b) => out.extend_from_slice(&[b'C', *b as u8]),
            P::I32(v) => {
                out.push(b'I');
                out.extend_from_slice(&v.to_le_bytes());
            }
            P::I64(v) => {
                out.push(b'L');
                out.extend_from_slice(&v.to_le_bytes());
            }
            P::F64(v) => {
                out.push(b'D');
                out.extend_from_slice(&v.to_le_bytes());
            }
            P::Str(v) => {
                out.push(b'S');
                out.extend_from_slice(&(v.len() as u32).to_le_bytes());
                out.extend_from_slice(v.as_bytes());
            }
            P::Raw(v) => {
                out.push(b'R');
                out.extend_from_slice(&(v.len() as u32).to_le_bytes());
                out.extend_from_slice(v);
            }
            P::F32s(v) => array(out, b'f', v.len(), bytemuck::cast_slice(v)),
            P::F64s(v) => array(out, b'd', v.len(), bytemuck::cast_slice(v)),
            P::I32s(v) => array(out, b'i', v.len(), bytemuck::cast_slice(v)),
            P::I64s(v) => array(out, b'l', v.len(), bytemuck::cast_slice(v)),
        }
    }
}

impl Node {
    /// Node record: end offset, property count, property bytes, name, properties, children.
    fn write(&self, out: &mut Vec<u8>) {
        let start = out.len();
        out.extend_from_slice(&[0; 12]);
        out.push(self.name.len() as u8);
        out.extend_from_slice(self.name.as_bytes());
        let props_start = out.len();
        for p in &self.props {
            p.write(out);
        }
        let props_len = out.len() - props_start;
        if !self.children.is_empty() {
            for c in &self.children {
                c.write(out);
            }
            out.extend_from_slice(&[0; 13]);
        } else if self.props.is_empty() {
            out.extend_from_slice(&[0; 13]);
        }
        let end = out.len() as u32;
        out[start..start + 4].copy_from_slice(&end.to_le_bytes());
        out[start + 4..start + 8].copy_from_slice(&(self.props.len() as u32).to_le_bytes());
        out[start + 8..start + 12].copy_from_slice(&(props_len as u32).to_le_bytes());
    }
}

fn file(nodes: &[Node]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"Kaydara FBX Binary  \x00\x1a\x00");
    out.extend_from_slice(&VERSION.to_le_bytes());
    for node in nodes {
        node.write(&mut out);
    }
    out.extend_from_slice(&[0; 13]);
    out.extend_from_slice(&FOOT_ID);
    let pad = match out.len() % 16 {
        0 => 16,
        r => 16 - r,
    };
    out.extend(std::iter::repeat_n(0u8, pad));
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&[0; 120]);
    out.extend_from_slice(&FOOT_MAGIC);
    out
}

/// True if the bytes start like a binary .fbx file.
pub fn is_fbx(bytes: &[u8]) -> bool {
    bytes.starts_with(b"Kaydara FBX Binary")
}

/// Objects and the links between them.
#[derive(Default)]
struct Builder {
    next_id: i64,
    objects: Vec<Node>,
    /// (child, parent, property) ; property None = object-to-object.
    links: Vec<(i64, i64, Option<&'static str>)>,
    counts: BTreeMap<&'static str, i32>,
    /// Clip name -> (stack id, layer id, start, stop in KTime).
    stacks: BTreeMap<String, (i64, i64, i64, i64)>,
    vertices: usize,
    triangles: usize,
    skins: usize,
    /// (object id, bone name) -> (LimbNode id, bone head), for objects on bones.
    joints: std::collections::HashMap<(u64, String), (i64, Vec3)>,
    /// Start of texture file names, and the files to write next to the .fbx.
    stem: String,
    files: Vec<(String, Vec<u8>)>,
    /// Written texture sets: (source, [color, normal] Texture ids).
    texture_sets: Vec<(Arc<Textures>, [i64; 2])>,
    /// Geometry written for unskinned Models, with what it was written
    /// from (mesh, tint, own material) and its materials: scattered copies
    /// and identical clay objects share them (FBX instancing).
    shared: Vec<SharedGeometry>,
    /// Materials made by the last `mesh` call.
    last_materials: Vec<i64>,
    /// Triangles of each LOD written: (object, [LOD1, LOD2, LOD3]).
    lods: Vec<(String, Vec<usize>)>,
    /// Blend shapes written: (object, shape name).
    shapes_written: Vec<(u64, String)>,
}

impl Builder {
    fn id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    fn add(&mut self, kind: &'static str, node: Node) {
        *self.counts.entry(kind).or_default() += 1;
        self.objects.push(node);
    }

    fn link(&mut self, child: i64, parent: i64) {
        self.links.push((child, parent, None));
    }

    /// A Model; `kind` is Mesh, Null or LimbNode.
    fn model(&mut self, name: &str, kind: &str, local: Mat4, parent: i64) -> i64 {
        let id = self.id();
        let (scale, rot, t) = local.to_scale_rotation_translation();
        let props = vec![
            vec3_prop("Lcl Translation", t),
            vec3_prop("Lcl Rotation", euler_degrees(rot)),
            vec3_prop("Lcl Scaling", scale),
            prop("InheritType", "enum", "", "", vec![P::I32(1)]),
            prop("DefaultAttributeIndex", "int", "Integer", "", vec![P::I32(0)]),
        ];
        let node = nc(
            "Model",
            vec![P::I64(id), obj_name(name, "Model"), s(kind)],
            vec![n("Version", vec![P::I32(232)]), nc("Properties70", vec![], props), n("Shading", vec![P::Bool(true)]), n("Culling", vec![s("CullingOff")])],
        );
        self.add("Model", node);
        self.link(id, parent);
        id
    }

    /// Geometry and its materials for `model` (`own` when the mesh carries
    /// none). Returns the geometry id.
    /// A Texture (with its Video) reading a PNG written next to the .fbx.
    fn texture(&mut self, name: &str, rgba: &[u8], size: u32, alpha: bool) -> Result<i64, String> {
        let file = format!("{}_{name}.png", self.stem);
        self.files.push((file.clone(), crate::bake::png(rgba, size, alpha)?));
        let video = self.id();
        let node = nc(
            "Video",
            vec![P::I64(video), obj_name(name, "Video"), s("Clip")],
            vec![
                n("Type", vec![s("Clip")]),
                nc("Properties70", vec![], vec![prop("Path", "KString", "XRefUrl", "", vec![s(&file)])]),
                n("UseMipMap", vec![P::I32(0)]),
                n("Filename", vec![s(&file)]),
                n("RelativeFilename", vec![s(&file)]),
            ],
        );
        self.add("Video", node);
        let tex = self.id();
        let node = nc(
            "Texture",
            vec![P::I64(tex), obj_name(name, "Texture"), s("")],
            vec![
                n("Type", vec![s("TextureVideoClip")]),
                n("Version", vec![P::I32(202)]),
                n("TextureName", vec![obj_name(name, "Texture")]),
                nc("Properties70", vec![], vec![prop("UseMaterial", "bool", "", "", vec![P::I32(1)])]),
                n("Media", vec![obj_name(name, "Video")]),
                n("FileName", vec![s(&file)]),
                n("RelativeFilename", vec![s(&file)]),
                n("ModelUVTranslation", vec![P::F64(0.0), P::F64(0.0)]),
                n("ModelUVScaling", vec![P::F64(1.0), P::F64(1.0)]),
                n("Texture_Alpha_Source", vec![s("None")]),
                n("Cropping", vec![P::I32(0), P::I32(0), P::I32(0), P::I32(0)]),
            ],
        );
        self.add("Texture", node);
        self.link(video, tex);
        Ok(tex)
    }

    /// Color and normal-map Textures for a baked mesh (written once).
    fn texture_set(&mut self, name: &str, t: &Arc<Textures>) -> Result<[i64; 2], String> {
        if let Some((_, ids)) = self.texture_sets.iter().find(|(s, _)| Arc::ptr_eq(s, t)) {
            return Ok(*ids);
        }
        let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        let ids = [self.texture(&format!("{safe}_color"), &t.color, t.size, t.alpha)?, self.texture(&format!("{safe}_normal"), &t.normal, t.size, false)?];
        self.texture_sets.push((t.clone(), ids));
        Ok(ids)
    }

    /// Links `model` to Geometry (and materials) already written from the
    /// same mesh, tint and material, else writes them.
    fn shared_mesh(&mut self, name: &str, mesh: &Arc<Mesh>, tint: [f32; 3], own: &Material, model: i64) -> Result<i64, String> {
        if let Some((_, _, _, geo, mats)) = self.shared.iter().find(|(a, t, o, _, _)| Arc::ptr_eq(a, mesh) && t.map(f32::to_bits) == tint.map(f32::to_bits) && o == own).cloned() {
            self.link(geo, model);
            for m in mats {
                self.link(m, model);
            }
            self.vertices += mesh.vertices.len();
            self.triangles += mesh.indices.len() / 3;
            return Ok(geo);
        }
        let geo = self.mesh(name, mesh, tint, own, model)?;
        self.shared.push((mesh.clone(), tint, own.clone(), geo, self.last_materials.clone()));
        Ok(geo)
    }

    fn mesh(&mut self, name: &str, mesh: &Mesh, tint: [f32; 3], own: &Material, model: i64) -> Result<i64, String> {
        let textures = mesh.textures.as_ref().map(|t| self.texture_set(name, t)).transpose()?;
        let geo = self.id();
        self.last_materials.clear();
        let vertices: Vec<f64> = mesh.vertices.iter().flat_map(|v| v.pos.map(f64::from)).collect();
        let normals: Vec<f64> = mesh.vertices.iter().flat_map(|v| Vec3::from(v.normal).normalize_or(Vec3::Y).to_array().map(f64::from)).collect();
        let colors: Vec<[f64; 3]> = mesh.vertices.iter().map(|v| [0, 1, 2].map(|i| (v.color[i] * tint[i]).clamp(0.0, 1.0) as f64)).collect();
        let rgba: Vec<f64> = colors.iter().flat_map(|c| [c[0], c[1], c[2], 1.0]).collect();
        let index: Vec<i32> = mesh.indices.chunks_exact(3).flat_map(|t| [t[0] as i32, t[1] as i32, !(t[2] as i32)]).collect();
        let groups = crate::gltf::split_by_material(mesh);
        let material_element = if groups.len() == 1 {
            ("AllSame", vec![0])
        } else {
            // Polygon order is triangle order; slot = the group's position.
            let slot = |m: usize| groups.iter().position(|g| g.0 == m).unwrap_or(0) as i32;
            ("ByPolygon", mesh.indices.chunks_exact(3).map(|t| slot(crate::gltf::triangle_material(mesh, t))).collect())
        };
        let layer = |ty: &str| nc("LayerElement", vec![], vec![n("Type", vec![s(ty)]), n("TypedIndex", vec![P::I32(0)])]);
        let element = |name: &'static str, version: i32, mapping: &str, reference: &str, data: Vec<Node>| {
            let mut children = vec![n("Version", vec![P::I32(version)]), n("Name", vec![s("")]), n("MappingInformationType", vec![s(mapping)]), n("ReferenceInformationType", vec![s(reference)])];
            children.extend(data);
            nc(name, vec![P::I32(0)], children)
        };
        let mut children = vec![
            n("Vertices", vec![P::F64s(vertices)]),
            n("PolygonVertexIndex", vec![P::I32s(index)]),
            n("GeometryVersion", vec![P::I32(124)]),
            element("LayerElementNormal", 101, "ByVertice", "Direct", vec![n("Normals", vec![P::F64s(normals)])]),
            element("LayerElementColor", 101, "ByVertice", "Direct", vec![n("Colors", vec![P::F64s(rgba)])]),
            element("LayerElementMaterial", 101, material_element.0, "IndexToDirect", vec![n("Materials", vec![P::I32s(material_element.1)])]),
        ];
        let mut layers = vec![n("Version", vec![P::I32(100)]), layer("LayerElementNormal"), layer("LayerElementColor"), layer("LayerElementMaterial")];
        if textures.is_some() {
            // FBX puts v = 0 at the bottom of the image; glTF and Clide3D at the top.
            let uv: Vec<f64> = mesh.vertices.iter().flat_map(|v| [v.uv[0] as f64, 1.0 - v.uv[1] as f64]).collect();
            let uv_index: Vec<i32> = mesh.indices.iter().map(|&i| i as i32).collect();
            children.push(element("LayerElementUV", 101, "ByPolygonVertex", "IndexToDirect", vec![n("UV", vec![P::F64s(uv)]), n("UVIndex", vec![P::I32s(uv_index)])]));
            layers.push(layer("LayerElementUV"));
        }
        children.push(nc("Layer", vec![P::I32(0)], layers));
        let node = nc("Geometry", vec![P::I64(geo), obj_name(name, "Geometry"), s("Mesh")], children);
        self.add("Geometry", node);
        self.link(geo, model);

        for (m, tris) in &groups {
            let material = mesh.materials.get(*m).unwrap_or(own);
            let used: std::collections::BTreeSet<u32> = tris.iter().copied().collect();
            let count = used.len().max(1) as f64;
            let avg = used.iter().fold([0.0; 3], |a, &i| {
                let c = colors[i as usize];
                [a[0] + c[0], a[1] + c[1], a[2] + c[2]]
            });
            // Textured: the image gives the color, the material only tints it.
            let avg = if textures.is_some() { tint.map(|c| c.clamp(0.0, 1.0) as f64) } else { avg.map(|x| x / count) };
            let mat = self.id();
            let color = || vec![P::F64(avg[0]), P::F64(avg[1]), P::F64(avg[2])];
            let gray = |v: f64| vec![P::F64(v), P::F64(v), P::F64(v)];
            // Blender reads roughness = 1 - sqrt(Shininess) / 10.
            let shininess = ((1.0 - material.roughness as f64) * 10.0).powi(2);
            let label = if groups.len() == 1 { name.to_string() } else { format!("{name}_{}", material.name) };
            let node = nc(
                "Material",
                vec![P::I64(mat), obj_name(&label, "Material"), s("")],
                vec![
                    n("Version", vec![P::I32(102)]),
                    n("ShadingModel", vec![s("phong")]),
                    n("MultiLayer", vec![P::I32(0)]),
                    nc(
                        "Properties70",
                        vec![],
                        vec![
                            prop("DiffuseColor", "Color", "", "A", color()),
                            prop("Diffuse", "Vector3D", "Vector", "", color()),
                            prop("DiffuseFactor", "double", "Number", "", vec![P::F64(1.0)]),
                            // Plain surfaces reflect a little white light; metal reflects its color.
                            prop("SpecularColor", "Color", "", "A", if material.metallic > 0.5 { color() } else { gray(0.2) }),
                            prop("SpecularFactor", "double", "Number", "", vec![P::F64(0.5)]),
                            prop("Shininess", "double", "Number", "", vec![P::F64(shininess)]),
                            prop("ShininessExponent", "double", "Number", "", vec![P::F64(shininess)]),
                            prop("ReflectionFactor", "double", "Number", "", vec![P::F64(material.metallic as f64)]),
                            // Glow in the surface color; glass as transparency.
                            prop("EmissiveColor", "Color", "", "A", if material.emission > 0.0 { color() } else { gray(0.0) }),
                            prop("EmissiveFactor", "double", "Number", "", vec![P::F64(material.emission as f64)]),
                            prop("TransparencyFactor", "double", "Number", "", vec![P::F64(material.transmission as f64)]),
                        ],
                    ),
                ],
            );
            self.add("Material", node);
            self.link(mat, model);
            self.last_materials.push(mat);
            if let Some([color, normal]) = textures {
                self.links.push((color, mat, Some("DiffuseColor")));
                self.links.push((normal, mat, Some("NormalMap")));
                // Hair cards: the color's alpha cuts the card out.
                if mesh.textures.as_ref().is_some_and(|t| t.alpha) {
                    self.links.push((color, mat, Some("TransparentColor")));
                }
            }
        }
        self.vertices += mesh.vertices.len();
        self.triangles += mesh.indices.len() / 3;
        Ok(geo)
    }

    /// An animation curve node (T, R or S of one model) with its 3 curves.
    fn curves(&mut self, layer: i64, model: i64, channel: &'static str, property: &'static str, times: &[i64], values: &[Vec3]) {
        let cn = self.id();
        let first = values[0];
        let node = nc(
            "AnimationCurveNode",
            vec![P::I64(cn), obj_name(channel, "AnimCurveNode"), s("")],
            vec![nc(
                "Properties70",
                vec![],
                ["d|X", "d|Y", "d|Z"].iter().zip(first.to_array()).map(|(a, v)| prop(a, "Number", "", "A", vec![P::F64(v as f64)])).collect(),
            )],
        );
        self.add("AnimationCurveNode", node);
        self.link(cn, layer);
        self.links.push((cn, model, Some(property)));
        for (axis, attr) in ["d|X", "d|Y", "d|Z"].into_iter().enumerate() {
            let id = self.id();
            let keys: Vec<f32> = values.iter().map(|v| v[axis]).collect();
            let node = nc(
                "AnimationCurve",
                vec![P::I64(id), obj_name("", "AnimCurve"), s("")],
                vec![
                    n("Default", vec![P::F64(keys[0] as f64)]),
                    n("KeyVer", vec![P::I32(4008)]),
                    n("KeyTime", vec![P::I64s(times.to_vec())]),
                    n("KeyValueFloat", vec![P::F32s(keys)]),
                    // Linear keys, as Blender writes them.
                    n("KeyAttrFlags", vec![P::I32s(vec![(1 << 2) | (1 << 8) | (1 << 13) | (1 << 14)])]),
                    n("KeyAttrDataFloat", vec![P::F32s(vec![0.0, 0.0, 9.419_963e-30, 0.0])]),
                    n("KeyAttrRefCount", vec![P::I32s(vec![times.len() as i32])]),
                ],
            );
            self.add("AnimationCurve", node);
            self.links.push((id, cn, Some(attr)));
        }
    }

    /// The stack and layer for a clip, made on first use.
    fn stack(&mut self, name: &str, start: i64, stop: i64) -> i64 {
        if let Some(e) = self.stacks.get_mut(name) {
            e.2 = e.2.min(start);
            e.3 = e.3.max(stop);
            return e.1;
        }
        let (stack, layer) = (self.id(), self.id());
        self.stacks.insert(name.to_string(), (stack, layer, start, stop));
        self.add("AnimationLayer", nc("AnimationLayer", vec![P::I64(layer), obj_name("BaseLayer", "AnimLayer"), s("")], vec![]));
        self.link(layer, stack);
        layer
    }
}

/// Geometry written once: (mesh, tint, own material, Geometry id, its
/// Material ids).
type SharedGeometry = (Arc<Mesh>, [f32; 3], Material, i64, Vec<i64>);

/// glam rotation -> FBX "Lcl Rotation" (XYZ order: X turns first), degrees.
fn euler_degrees(q: Quat) -> Vec3 {
    let (z, y, x) = q.to_euler(EulerRot::ZYX);
    Vec3::new(x, y, z) * (180.0 / std::f32::consts::PI)
}

/// The Euler angles for `q` closest to `prev`, so curves do not jump by 360.
fn continuous_euler(q: Quat, prev: Vec3) -> Vec3 {
    let a = euler_degrees(q);
    // The same rotation written the other way round.
    let b = Vec3::new(a.x + 180.0, 180.0 - a.y, a.z + 180.0);
    let unwrap = |e: Vec3| e + ((prev - e) / 360.0).round() * 360.0;
    let (a, b) = (unwrap(a), unwrap(b));
    if a.distance_squared(prev) <= b.distance_squared(prev) { a } else { b }
}

pub fn export(scene: &Scene, meshes: &MeshLib, opts: &Options) -> Result<crate::gltf::Export, String> {
    // Everything is written in the rest pose.
    let mut scene = scene.clone();
    scene.frame = None;
    let scene = &scene;
    let stem = if opts.file_stem.is_empty() { "texture".to_string() } else { opts.file_stem.clone() };
    let mut b = Builder { next_id: 1_000_000, stem, ..Default::default() };
    let roots: Vec<u64> = if opts.roots.is_empty() { scene.children(None) } else { opts.roots.clone() };
    let mut skipped = Vec::new();
    let mut bind_pose: Vec<(i64, Mat4)> = Vec::new();
    let mut stack: Vec<(u64, i64, Mat4)> = roots.iter().rev().map(|&id| (id, 0, Mat4::IDENTITY)).collect();
    while let Some((id, parent, parent_world)) = stack.pop() {
        let Some(o) = scene.get(id) else { continue };
        if !scene.is_visible(id) {
            skipped.push(o.name.clone());
            continue;
        }
        // An exported root keeps its world placement; an object on a bone
        // hangs under that LimbNode, offset back from the bone head.
        let joint = o.bone.as_ref().zip(o.parent).and_then(|(bone, p)| b.joints.get(&(p, bone.clone())).copied()).filter(|_| parent != 0);
        let world = if parent == 0 { scene.world_matrix(id) } else { parent_world * o.local_matrix() };
        let (parent, local) = match joint {
            Some((j, head)) => (j, Mat4::from_translation(-head) * o.local_matrix()),
            None if parent == 0 => (0, world),
            None => (parent, o.local_matrix()),
        };
        let (mesh, skin): (Option<std::sync::Arc<Mesh>>, Option<std::sync::Arc<Skin>>) = match o.shape {
            Shape::Group => (None, None),
            // Baked clay exports its game mesh (see `bake.rs`).
            Shape::Clay | Shape::Cloth if o.bake.is_some() && meshes.baked(id).is_some() => meshes.baked(id).map_or((None, None), |(m, s)| (Some(m.clone()), s.cloned())),
            Shape::Clay | Shape::Cloth => meshes.rest(id).map_or((None, None), |(m, s)| (Some(m.clone()), s.cloned())),
            _ => (meshes.get(o).cloned(), None),
        };
        let mesh = mesh.filter(|m| !m.vertices.is_empty());
        let cache = mesh.as_ref().and_then(|m| o.cloth.as_ref().filter(|_| opts.animations).and_then(|c| crate::cloth::export_cache(c, m, &scene.material_of(o), scene.rig_of(o))));
        let skin = cache.as_ref().map(|c| std::sync::Arc::new(c.1.clone())).or(skin);
        let rig = cache.as_ref().map(|c| &c.0).or_else(|| scene.rig_of(o)).filter(|r| !r.bones.is_empty());
        let skin = skin.filter(|s| mesh.as_ref().is_some_and(|m| s.joints.len() == m.vertices.len()));
        // Baked game meshes may carry LOD1-LOD3: a LodGroup of LOD0..LOD3.
        let lods = match &mesh {
            Some(m) if opts.lods && m.textures.is_some() && cache.is_none() => crate::bake::lods(m, skin.as_deref()),
            _ => Vec::new(),
        };
        // Blend shapes of this object's mesh (`layers::project_shape`).
        let shapes: Vec<&crate::gltf::BlendShape> = opts.blend_shapes.iter().filter(|s| s.object == id && mesh.as_ref().is_some_and(|m| m.vertices.len() == s.deltas.len())).collect();
        let model = match (&mesh, rig, skin) {
            (Some(mesh), Some(rig), Some(skin)) if lods.is_empty() => {
                let holder = b.model(&o.name, "Null", local, parent);
                let mesh_model = b.model(&format!("{}_mesh", o.name), "Mesh", Mat4::IDENTITY, holder);
                let geo = b.mesh(&o.name, mesh, o.color, &scene.material_of(o), mesh_model)?;
                blend_shapes(&mut b, id, &o.name, geo, &shapes);
                bind_pose.push((mesh_model, world));
                add_rig(&mut b, o.id, &o.name, rig, &[(geo, &skin)], holder, world, &mut bind_pose, opts);
                holder
            }
            (Some(mesh), Some(rig), Some(skin)) => {
                let holder = b.model(&o.name, "Null", local, parent);
                let group = lod_group(&mut b, &format!("{}_LODGroup", o.name), Mat4::IDENTITY, holder);
                let mut geos = Vec::new();
                for (k, (m, s)) in std::iter::once((mesh.as_ref(), Some(&*skin))).chain(lods.iter().map(|(m, s)| (m, s.as_ref()))).enumerate() {
                    let mesh_model = b.model(&format!("{}_LOD{k}", o.name), "Mesh", Mat4::IDENTITY, group);
                    let counts = (b.vertices, b.triangles);
                    let geo = b.mesh(&format!("{}_LOD{k}", o.name), m, o.color, &scene.material_of(o), mesh_model)?;
                    // The shapes were fitted to LOD0's vertices: LOD1-3 have others.
                    if k == 0 { blend_shapes(&mut b, id, &o.name, geo, &shapes); }
                    if k > 0 { (b.vertices, b.triangles) = counts; }
                    bind_pose.push((mesh_model, world));
                    if let Some(s) = s { geos.push((geo, s)); }
                }
                add_rig(&mut b, o.id, &o.name, rig, &geos, holder, world, &mut bind_pose, opts);
                b.lods.push((o.name.clone(), lods.iter().map(|l| l.0.indices.len() / 3).collect()));
                holder
            }
            (Some(mesh), _, _) if !lods.is_empty() => {
                let group = lod_group(&mut b, &o.name, local, parent);
                for (k, m) in std::iter::once(mesh.as_ref()).chain(lods.iter().map(|l| &l.0)).enumerate() {
                    let model = b.model(&format!("{}_LOD{k}", o.name), "Mesh", Mat4::IDENTITY, group);
                    let counts = (b.vertices, b.triangles);
                    let geo = b.mesh(&format!("{}_LOD{k}", o.name), m, o.color, &scene.material_of(o), model)?;
                    if k == 0 { blend_shapes(&mut b, id, &o.name, geo, &shapes); }
                    if k > 0 { (b.vertices, b.triangles) = counts; }
                }
                b.lods.push((o.name.clone(), lods.iter().map(|l| l.0.indices.len() / 3).collect()));
                group
            }
            (Some(mesh), _, _) if !shapes.is_empty() => {
                let model = b.model(&o.name, "Mesh", local, parent);
                let geo = b.mesh(&o.name, mesh, o.color, &scene.material_of(o), model)?;
                blend_shapes(&mut b, id, &o.name, geo, &shapes);
                model
            }
            (Some(mesh), _, _) => {
                let model = b.model(&o.name, "Mesh", local, parent);
                b.shared_mesh(&o.name, mesh, o.color, &scene.material_of(o), model)?;
                model
            }
            (None, _, _) => b.model(&o.name, "Null", local, parent),
        };
        // Scattered copies: one Model each, all sharing their source's
        // Geometry and Materials (instances in Maya, linked duplicates in Blender).
        if let Some(sc) = o.scatter.as_ref().filter(|_| o.shape == Shape::Scatter) {
            for (i, inst) in sc.instances.iter().enumerate() {
                let Some(src) = sc.sources.get(inst.source as usize).and_then(|&id| scene.get(id)) else { continue };
                let Some(src_mesh) = meshes.get(src).filter(|m| !m.vertices.is_empty()) else { continue };
                let copy = b.model(&format!("{}_{i}", src.name), "Mesh", inst.matrix() * crate::envkit::scatter::source_shape(src), model);
                b.shared_mesh(&src.name, src_mesh, src.color, &scene.material_of(src), copy)?;
            }
        }
        for c in scene.children(Some(id)).into_iter().rev() {
            stack.push((c, model, world));
        }
    }
    if b.vertices == 0 {
        return Err("nothing to export: no visible meshes".into());
    }

    if !bind_pose.is_empty() {
        let pose = b.id();
        let mut children = vec![n("Type", vec![s("BindPose")]), n("Version", vec![P::I32(100)]), n("NbPoseNodes", vec![P::I32(bind_pose.len() as i32)])];
        for (node, m) in &bind_pose {
            children.push(nc("PoseNode", vec![], vec![n("Node", vec![P::I64(*node)]), n("Matrix", vec![P::F64s(m.to_cols_array().map(f64::from).to_vec())])]));
        }
        b.add("Pose", nc("Pose", vec![P::I64(pose), obj_name("BindPose", "Pose"), s("BindPose")], children));
    }
    let stacks: Vec<(String, (i64, i64, i64, i64))> = b.stacks.iter().map(|(k, v)| (k.clone(), *v)).collect();
    let mut takes = vec![n("Current", vec![s(stacks.first().map_or("", |s| s.0.as_str()))])];
    for (name, (id, _, start, stop)) in &stacks {
        let time = |k: &str, v: i64| prop(k, "KTime", "Time", "", vec![P::I64(v)]);
        let props = vec![time("LocalStart", *start), time("LocalStop", *stop), time("ReferenceStart", *start), time("ReferenceStop", *stop)];
        b.add("AnimationStack", nc("AnimationStack", vec![P::I64(*id), obj_name(name, "AnimStack"), s("")], vec![nc("Properties70", vec![], props)]));
        takes.push(nc(
            "Take",
            vec![s(name)],
            vec![n("FileName", vec![s(&format!("{name}.tak"))]), n("LocalTime", vec![P::I64(*start), P::I64(*stop)]), n("ReferenceTime", vec![P::I64(*start), P::I64(*stop)])],
        ));
    }

    let fps = opts.fps.clamp(1.0, 120.0) as f64;
    let (time_mode, custom) = match fps as i32 {
        24 if fps.fract() == 0.0 => (11, fps),
        25 if fps.fract() == 0.0 => (10, fps),
        30 if fps.fract() == 0.0 => (6, fps),
        48 if fps.fract() == 0.0 => (5, fps),
        60 if fps.fract() == 0.0 => (3, fps),
        _ => (14, fps),
    };
    let axis = |name: &str, v: i32| prop(name, "int", "Integer", "", vec![P::I32(v)]);
    let global = nc(
        "GlobalSettings",
        vec![],
        vec![
            n("Version", vec![P::I32(1000)]),
            nc(
                "Properties70",
                vec![],
                vec![
                    axis("UpAxis", 1),
                    axis("UpAxisSign", 1),
                    axis("FrontAxis", 2),
                    axis("FrontAxisSign", 1),
                    axis("CoordAxis", 0),
                    axis("CoordAxisSign", 1),
                    axis("OriginalUpAxis", 1),
                    axis("OriginalUpAxisSign", 1),
                    prop("UnitScaleFactor", "double", "Number", "", vec![P::F64(100.0)]),
                    prop("OriginalUnitScaleFactor", "double", "Number", "", vec![P::F64(100.0)]),
                    prop("TimeMode", "enum", "", "", vec![P::I32(time_mode)]),
                    prop("CustomFrameRate", "double", "Number", "", vec![P::F64(custom)]),
                ],
            ),
        ],
    );
    let stamp = |name: &'static str, v: i32| n(name, vec![P::I32(v)]);
    let header = nc(
        "FBXHeaderExtension",
        vec![],
        vec![
            stamp("FBXHeaderVersion", 1003),
            stamp("FBXVersion", VERSION as i32),
            nc(
                "CreationTimeStamp",
                vec![],
                vec![stamp("Version", 1000), stamp("Year", 1970), stamp("Month", 1), stamp("Day", 1), stamp("Hour", 10), stamp("Minute", 0), stamp("Second", 0), stamp("Millisecond", 0)],
            ),
            n("Creator", vec![s("Clide3D")]),
        ],
    );
    let doc_id = b.id();
    let documents = nc(
        "Documents",
        vec![],
        vec![
            n("Count", vec![P::I32(1)]),
            nc(
                "Document",
                vec![P::I64(doc_id), s(""), s("Scene")],
                vec![
                    nc("Properties70", vec![], vec![prop("SourceObject", "object", "", "", vec![]), prop("ActiveAnimStackName", "KString", "", "", vec![s(stacks.first().map_or("", |s| s.0.as_str()))])]),
                    n("RootNode", vec![P::I64(0)]),
                ],
            ),
        ],
    );
    let mut defs = vec![n("Version", vec![P::I32(100)]), n("Count", vec![P::I32(1 + b.counts.values().sum::<i32>())])];
    defs.push(nc("ObjectType", vec![s("GlobalSettings")], vec![n("Count", vec![P::I32(1)])]));
    for (kind, count) in &b.counts {
        defs.push(nc("ObjectType", vec![s(kind)], vec![n("Count", vec![P::I32(*count)])]));
    }
    let connections: Vec<Node> = b
        .links
        .iter()
        .map(|&(c, p, property)| match property {
            None => n("C", vec![s("OO"), P::I64(c), P::I64(p)]),
            Some(name) => n("C", vec![s("OP"), P::I64(c), P::I64(p), s(name)]),
        })
        .collect();

    let summary = json!({
        "vertices": b.vertices,
        "triangles": b.triangles,
        "skins": b.skins,
        "geometries": b.counts.get("Geometry").copied().unwrap_or(0),
        "blend_shapes": b.shapes_written.len(),
        "animations": stacks.iter().map(|s| &s.0).collect::<Vec<_>>(),
        "skipped_hidden": skipped,
        "lods": b.lods.iter().map(|(n, t)| (n.clone(), json!({ "lod1_3_triangles": t }))).collect::<serde_json::Map<_, _>>(),
    });
    let objects = std::mem::take(&mut b.objects);
    let nodes = vec![
        header,
        n("FileId", vec![P::Raw(FILE_ID.to_vec())]),
        n("CreationTime", vec![s(CREATION_TIME)]),
        n("Creator", vec![s("Clide3D")]),
        global,
        documents,
        nc("References", vec![], vec![]),
        nc("Definitions", vec![], defs),
        nc("Objects", vec![], objects),
        nc("Connections", vec![], connections),
        nc("Takes", vec![], takes),
    ];
    Ok(crate::gltf::Export { bytes: file(&nodes), summary, files: b.files, shapes_written: b.shapes_written })
}

/// Blend shapes on Geometry `geo`: a BlendShape deformer with one
/// BlendShapeChannel per shape, each driving a Shape geometry of the
/// vertices it moves (offsets, at full weight).
fn blend_shapes(b: &mut Builder, object: u64, name: &str, geo: i64, shapes: &[&crate::gltf::BlendShape]) {
    if shapes.is_empty() {
        return;
    }
    let deformer = b.id();
    b.add("Deformer", nc("Deformer", vec![P::I64(deformer), obj_name(name, "Deformer"), s("BlendShape")], vec![n("Version", vec![P::I32(100)])]));
    b.link(deformer, geo);
    for shape in shapes {
        let label = shape.name.replace(':', "_");
        let mut index: Vec<i32> = shape.deltas.iter().enumerate().filter(|(_, d)| d.iter().any(|x| *x != 0.0)).map(|(i, _)| i as i32).collect();
        if index.is_empty() {
            index.push(0);
        }
        let vertices: Vec<f64> = index.iter().flat_map(|&i| shape.deltas[i as usize].map(f64::from)).collect();
        let normals = vec![0.0f64; vertices.len()];
        let geometry = b.id();
        b.add("Geometry", nc("Geometry", vec![P::I64(geometry), obj_name(&label, "Geometry"), s("Shape")], vec![n("Version", vec![P::I32(100)]), n("Indexes", vec![P::I32s(index)]), n("Vertices", vec![P::F64s(vertices)]), n("Normals", vec![P::F64s(normals)])]));
        let channel = b.id();
        b.add("Deformer", nc("Deformer", vec![P::I64(channel), obj_name(&label, "SubDeformer"), s("BlendShapeChannel")], vec![n("Version", vec![P::I32(100)]), n("DeformPercent", vec![P::F64(0.0)]), n("FullWeights", vec![P::F64s(vec![100.0])])]));
        b.link(geometry, channel);
        b.link(channel, deformer);
        b.shapes_written.push((object, shape.name.clone()));
    }
}

/// A Null model with a LodGroup attribute: its children are LOD0, LOD1...
/// (Unreal and Maya read it as a LOD group; Unity groups the `_LODn`
/// names). Levels switch at shrinking shares of the screen.
fn lod_group(b: &mut Builder, name: &str, local: Mat4, parent: i64) -> i64 {
    let id = b.model(name, "Null", local, parent);
    let attr = b.id();
    let props = vec![
        prop("ThresholdsUsedAsPercentage", "bool", "", "", vec![P::I32(1)]),
        prop("Thresholds|Level0", "Distance", "", "", vec![P::F64(40.0)]),
        prop("Thresholds|Level1", "Distance", "", "", vec![P::F64(20.0)]),
        prop("Thresholds|Level2", "Distance", "", "", vec![P::F64(8.0)]),
        prop("MinMaxDistance", "bool", "", "", vec![P::I32(0)]),
    ];
    b.add("NodeAttribute", nc("NodeAttribute", vec![P::I64(attr), obj_name(name, "NodeAttribute"), s("LodGroup")], vec![nc("Properties70", vec![], props), n("TypeFlags", vec![s("LodGroup")])]));
    b.link(attr, id);
    id
}

/// Skeleton, skin and clips of one rigged clay object.
#[allow(clippy::too_many_arguments)]
fn add_rig(b: &mut Builder, object: u64, name: &str, rig: &Rig, geos: &[(i64, &Skin)], holder: i64, world: Mat4, bind_pose: &mut Vec<(i64, Mat4)>, opts: &Options) {
    let mut bones = Vec::with_capacity(rig.bones.len());
    for (i, bone) in rig.bones.iter().enumerate() {
        let parent = bone.parent.as_deref().and_then(|p| rig.bones[..i].iter().position(|x| x.name == p));
        let base = parent.map_or(Vec3::ZERO, |p| rig.bones[p].head);
        let id = b.model(&bone.name, "LimbNode", Mat4::from_translation(bone.head - base), parent.map_or(holder, |p| bones[p]));
        let attr = b.id();
        let size = bone.head.distance(bone.tail).max(0.01) as f64 * 100.0;
        b.add(
            "NodeAttribute",
            nc(
                "NodeAttribute",
                vec![P::I64(attr), obj_name(&bone.name, "NodeAttribute"), s("LimbNode")],
                vec![nc("Properties70", vec![], vec![prop("Size", "double", "Number", "", vec![P::F64(size)])]), n("TypeFlags", vec![s("Skeleton")])],
            ),
        );
        b.link(attr, id);
        bind_pose.push((id, world * Mat4::from_translation(bone.head)));
        bones.push(id);
        b.joints.insert((object, bone.name.clone()), (id, bone.head));
    }

    // One Skin per geometry (each level of detail has its own vertices).
    for &(geo, skin) in geos {
    let skin_id = b.id();
    b.add("Deformer", nc("Deformer", vec![P::I64(skin_id), obj_name(name, "Deformer"), s("Skin")], vec![n("Version", vec![P::I32(101)]), n("Link_DeformAcuracy", vec![P::F64(50.0)])]));
    b.link(skin_id, geo);
    b.skins += 1;
    let mut per_bone: Vec<(Vec<i32>, Vec<f64>)> = vec![(Vec::new(), Vec::new()); rig.bones.len()];
    for (vi, (j, w)) in skin.joints.iter().zip(&skin.weights).enumerate() {
        for k in 0..4 {
            if w[k] > 0.0
                && let Some(slot) = per_bone.get_mut(j[k] as usize)
            {
                slot.0.push(vi as i32);
                slot.1.push(w[k] as f64);
            }
        }
    }
    for (i, (indexes, weights)) in per_bone.into_iter().enumerate() {
        if indexes.is_empty() {
            continue;
        }
        let link = world * Mat4::from_translation(rig.bones[i].head);
        let transform = link.inverse() * world;
        let cluster = b.id();
        b.add(
            "Deformer",
            nc(
                "Deformer",
                vec![P::I64(cluster), obj_name(&rig.bones[i].name, "SubDeformer"), s("Cluster")],
                vec![
                    n("Version", vec![P::I32(100)]),
                    n("UserData", vec![s(""), s("")]),
                    n("Indexes", vec![P::I32s(indexes)]),
                    n("Weights", vec![P::F64s(weights)]),
                    n("Transform", vec![P::F64s(transform.to_cols_array().map(f64::from).to_vec())]),
                    n("TransformLink", vec![P::F64s(link.to_cols_array().map(f64::from).to_vec())]),
                ],
            ),
        );
        b.link(cluster, skin_id);
        b.link(bones[i], cluster);
    }
    }

    if !opts.animations {
        return;
    }
    let fps = opts.fps.clamp(1.0, 120.0);
    for clip in &rig.clips {
        let Some((start, end)) = clip.range() else { continue };
        let mut frames: Vec<f32> = (0..).map(|i| start + i as f32).take_while(|f| *f < end - 1e-3).take(100_000).collect();
        frames.push(end);
        let times: Vec<i64> = frames.iter().map(|f| ((f - start) as f64 / fps as f64 * KTIME_SECOND).round() as i64).collect();
        let layer = b.stack(&clip.name, 0, *times.last().unwrap_or(&0));
        for track in &clip.tracks {
            let Some(i) = rig.bone_index(&track.bone) else { continue };
            let bone = &rig.bones[i];
            let base = bone.head - bone.parent.as_deref().and_then(|p| rig.bone_index(p)).map_or(Vec3::ZERO, |p| rig.bones[p].head);
            let poses: Vec<_> = frames.iter().map(|&f| track.sample(f)).collect();
            let mut prev = Vec3::ZERO;
            let rot: Vec<Vec3> = poses
                .iter()
                .map(|p| {
                    prev = continuous_euler(p.rotate, prev);
                    prev
                })
                .collect();
            b.curves(layer, bones[i], "R", "Lcl Rotation", &times, &rot);
            if poses.iter().any(|p| p.translate != Vec3::ZERO) {
                let t: Vec<Vec3> = poses.iter().map(|p| base + p.translate).collect();
                b.curves(layer, bones[i], "T", "Lcl Translation", &times, &t);
            }
            if poses.iter().any(|p| p.scale != Vec3::ONE) {
                let sc: Vec<Vec3> = poses.iter().map(|p| p.scale).collect();
                b.curves(layer, bones[i], "S", "Lcl Scaling", &times, &sc);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn euler_round_trips_and_stays_continuous() {
        let q = crate::clay::rotation(Vec3::new(10.0, 20.0, 30.0));
        let e = euler_degrees(q);
        assert!(e.distance(Vec3::new(10.0, 20.0, 30.0)) < 1e-3, "{e:?}");
        // 179 -> -179 is a 2 degree step, not 358.
        let a = continuous_euler(crate::clay::rotation(Vec3::new(0.0, 0.0, 179.0)), Vec3::ZERO);
        let b = continuous_euler(crate::clay::rotation(Vec3::new(0.0, 0.0, -179.0)), a);
        assert!((b.z - 181.0).abs() < 1e-2, "{b:?}");
    }

    #[test]
    fn bone_clusters_export_the_painted_skin_weights() {
        use crate::rig::{Bone, WeightMode, WeightStroke};
        let clay: crate::clay::Clay = serde_json::from_value(serde_json::json!({"blobs":[{"type":"sphere","center":[0,0,0],"radius":0.5,"bone":"base","blend":0}],"resolution":24})).unwrap();
        let mesh = clay.build_mesh();
        let rig = Rig {bones:vec![Bone {name:"base".into(),parent:None,head:Vec3::ZERO,tail:Vec3::Y,deform:true},Bone {name:"painted".into(),parent:Some("base".into()),head:Vec3::Y,tail:Vec3::Y*2.0,deform:true}],weights:vec![WeightStroke {bone:"painted".into(),points:vec![Vec3::ZERO],radius:1.0,strength:0.7,mode:WeightMode::Replace,space:Mat4::IDENTITY}],..Default::default()};
        let skin = crate::rig::skin(&clay,&mesh,&rig);
        let mut builder = Builder::default();
        add_rig(&mut builder,1,"test",&rig,&[(101,&skin)],100,Mat4::IDENTITY,&mut Vec::new(),&Options {roots:Vec::new(),animations:false,fps:24.0,file_stem:String::new(),lods:false, atlas: true, texel_density: None, gpu_instancing: false, blend_shapes: Vec::new()});
        let cluster = builder.objects.iter().find(|n| n.props.iter().any(|p| matches!(p, P::Str(name) if name.starts_with("painted\0"))) && n.children.iter().any(|n| n.name == "Weights")).unwrap();
        let indexes = &cluster.children.iter().find(|n| n.name == "Indexes").unwrap().props[0];
        let weights = &cluster.children.iter().find(|n| n.name == "Weights").unwrap().props[0];
        let (P::I32s(indexes),P::F64s(weights)) = (indexes,weights) else { panic!("cluster arrays") };
        assert!(!indexes.is_empty());
        for (&i,&actual) in indexes.iter().zip(weights) {
            let i = i as usize;
            let expected: f32 = skin.joints[i].iter().zip(skin.weights[i]).filter(|(j,_)| **j == 1).map(|(_,w)| w).sum();
            assert_eq!(actual,expected as f64);
            assert!(actual > 0.2);
        }
        assert!(is_fbx(&file(&builder.objects)));
    }

    #[test]
    fn file_has_header_records_and_footer() {
        let nodes = vec![nc("Objects", vec![], vec![n("Model", vec![P::I64(7), s("x")])])];
        let bytes = file(&nodes);
        assert!(is_fbx(&bytes));
        assert_eq!(u32::from_le_bytes(bytes[23..27].try_into().unwrap()), VERSION);
        // The first record ends where it says it ends.
        let end = u32::from_le_bytes(bytes[27..31].try_into().unwrap()) as usize;
        assert_eq!(&bytes[end..end + 13], &[0; 13]);
        assert!(bytes.ends_with(&FOOT_MAGIC));
        assert_eq!((bytes.len() - FOOT_MAGIC.len() - 120 - 4) % 16, 0);
    }
}
