//! Stage 5 acceptance for the readers and writers: exact triangle counts
//! and boxes through every format, the icosphere as clay at res 128, and
//! fuzz-lite (truncated and corrupted files never panic).

use std::io::Write;

use glam::Vec3;
use serde_json::json;

use super::{Format, parse, write};
use crate::clay::{Blob, Clay, Form, Op};
use crate::mesh_asset::tests::icosphere;
use crate::mesh_asset::{self, MeshAsset, Sign};

fn bounds(points: &[Vec3]) -> (Vec3, Vec3) {
    points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(a, b), p| (a.min(*p), b.max(*p)))
}

fn ascii_stl(points: &[Vec3], tris: &[[u32; 3]]) -> Vec<u8> {
    let mut out = b"solid ico\n".to_vec();
    for t in tris {
        let _ = writeln!(out, " facet normal 0 0 0\n  outer loop");
        for &i in t {
            let p = points[i as usize];
            let _ = writeln!(out, "   vertex {} {} {}", p.x, p.y, p.z);
        }
        let _ = writeln!(out, "  endloop\n endfacet");
    }
    out.extend(b"endsolid ico\n");
    out
}

fn ascii_ply(points: &[Vec3], tris: &[[u32; 3]]) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = write!(out, "ply\nformat ascii 1.0\nelement vertex {}\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nelement face {}\nproperty list uchar int vertex_indices\nelement edge 1\nproperty int vertex1\nproperty int vertex2\nend_header\n", points.len(), tris.len());
    for p in points {
        let _ = writeln!(out, "{} {} {} 200", p.x, p.y, p.z);
    }
    for t in tris {
        let _ = writeln!(out, "3 {} {} {}", t[0], t[1], t[2]);
    }
    out.extend(b"0 1\n");
    out
}

fn big_endian_ply(points: &[Vec3], tris: &[[u32; 3]]) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = write!(out, "ply\nformat binary_big_endian 1.0\nelement vertex {}\nproperty double x\nproperty double y\nproperty double z\nelement face {}\nproperty list uchar uint vertex_index\nend_header\n", points.len(), tris.len());
    for p in points {
        for c in p.to_array() {
            out.extend((c as f64).to_be_bytes());
        }
    }
    for t in tris {
        out.push(3);
        for i in t {
            out.extend(i.to_be_bytes());
        }
    }
    out
}

/// A GLB of one primitive holding the mesh, its node moved by `offset`.
pub(crate) fn glb(points: &[Vec3], tris: &[[u32; 3]], offset: Vec3) -> Vec<u8> {
    glb_scaled(points, tris, offset, Vec3::ONE)
}

/// `glb` with the node scaled (a negative scale mirrors).
fn glb_scaled(points: &[Vec3], tris: &[[u32; 3]], offset: Vec3, scale: Vec3) -> Vec<u8> {
    let mut bin: Vec<u8> = bytemuck::cast_slice(points).to_vec();
    let idx_at = bin.len();
    bin.extend(bytemuck::cast_slice::<[u32; 3], u8>(tris));
    let (lo, hi) = bounds(points);
    let doc = json!({"asset": {"version": "2.0"}, "buffers": [{"byteLength": bin.len()}],
        "bufferViews": [{"buffer": 0, "byteLength": idx_at}, {"buffer": 0, "byteOffset": idx_at, "byteLength": bin.len() - idx_at}],
        "accessors": [{"bufferView": 0, "componentType": 5126, "count": points.len(), "type": "VEC3", "min": lo.to_array(), "max": hi.to_array()},
                      {"bufferView": 1, "componentType": 5125, "count": tris.len() * 3, "type": "SCALAR"}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "indices": 1}]}],
        "nodes": [{"mesh": 0, "translation": offset.to_array(), "scale": scale.to_array()}], "scenes": [{"nodes": [0]}], "scene": 0});
    let mut json = serde_json::to_vec(&doc).unwrap();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let mut bytes = b"glTF".to_vec();
    for v in [2, (28 + json.len() + bin.len()) as u32, json.len() as u32, 0x4e4f534a] {
        bytes.extend(v.to_le_bytes());
    }
    bytes.extend(json);
    bytes.extend((bin.len() as u32).to_le_bytes());
    bytes.extend(0x004e4942u32.to_le_bytes());
    bytes.extend(bin);
    bytes
}

/// A binary FBX 7.4 with one Geometry (doubles, zlib-packed indices) and
/// UnitScaleFactor 100 (meters).
pub(crate) fn fbx(points: &[Vec3], tris: &[[u32; 3]]) -> Vec<u8> {
    enum P<'a> {
        S(&'a str),
        D(f64),
        Ds(Vec<f64>),
        Is(Vec<i32>),
    }
    fn node(out: &mut Vec<u8>, name: &str, props: &[P], children: &dyn Fn(&mut Vec<u8>)) {
        let start = out.len();
        out.extend([0u8; 12]);
        out.push(name.len() as u8);
        out.extend(name.as_bytes());
        let props_at = out.len();
        for p in props {
            match p {
                P::S(s) => {
                    out.push(b'S');
                    out.extend((s.len() as u32).to_le_bytes());
                    out.extend(s.as_bytes());
                }
                P::D(x) => {
                    out.push(b'D');
                    out.extend(x.to_le_bytes());
                }
                P::Ds(v) => {
                    out.push(b'd');
                    out.extend((v.len() as u32).to_le_bytes());
                    out.extend(0u32.to_le_bytes());
                    out.extend(((v.len() * 8) as u32).to_le_bytes());
                    v.iter().for_each(|x| out.extend(x.to_le_bytes()));
                }
                P::Is(v) => {
                    let raw: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
                    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                    z.write_all(&raw).unwrap();
                    let packed = z.finish().unwrap();
                    out.push(b'i');
                    out.extend((v.len() as u32).to_le_bytes());
                    out.extend(1u32.to_le_bytes());
                    out.extend((packed.len() as u32).to_le_bytes());
                    out.extend(packed);
                }
            }
        }
        let list_len = out.len() - props_at;
        let before = out.len();
        children(out);
        if out.len() > before {
            out.extend([0u8; 13]);
        }
        let end = out.len() as u32;
        out[start..start + 4].copy_from_slice(&end.to_le_bytes());
        out[start + 4..start + 8].copy_from_slice(&(props.len() as u32).to_le_bytes());
        out[start + 8..start + 12].copy_from_slice(&(list_len as u32).to_le_bytes());
    }
    let mut out = b"Kaydara FBX Binary  \x00\x1a\x00".to_vec();
    out.extend(7400u32.to_le_bytes());
    node(&mut out, "GlobalSettings", &[], &|o| node(o, "Properties70", &[], &|o| node(o, "P", &[P::S("UnitScaleFactor"), P::S("double"), P::S("Number"), P::S(""), P::D(100.0)], &|_| {})));
    let v: Vec<f64> = points.iter().flat_map(|p| p.to_array().map(|c| c as f64)).collect();
    let idx: Vec<i32> = tris.iter().flat_map(|t| [t[0] as i32, t[1] as i32, !(t[2] as i32)]).collect();
    node(&mut out, "Objects", &[], &|o| node(o, "Geometry", &[P::S("Geometry::ico")], &|o| {
        node(o, "Vertices", &[P::Ds(v.clone())], &|_| {});
        node(o, "PolygonVertexIndex", &[P::Is(idx.clone())], &|_| {});
    }));
    out.extend([0u8; 13]);
    out
}

/// Every reader on the same icosphere: exact triangle count, box within 1e-5.
#[test]
fn every_format_reads_the_icosphere_exactly() {
    let (p, t) = icosphere(3, 0.5);
    let (lo, hi) = bounds(&p);
    let files = [
        ("obj", Format::Obj, write::obj(&p, &t)),
        ("stl", Format::Stl, write::stl(&p, &t)),
        ("ascii stl", Format::Stl, ascii_stl(&p, &t)),
        ("ply", Format::Ply, write::ply(&p, &t)),
        ("ascii ply", Format::Ply, ascii_ply(&p, &t)),
        ("big-endian ply", Format::Ply, big_endian_ply(&p, &t)),
        ("glb", Format::Glb, glb(&p, &t, Vec3::ZERO)),
        ("fbx", Format::Fbx, fbx(&p, &t)),
    ];
    for (name, format, bytes) in files {
        let s = parse(&bytes, format).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(s.tris.len(), t.len(), "{name}: triangles");
        assert_eq!(s.points.len(), p.len(), "{name}: welded points");
        let (a, b) = bounds(&s.points);
        assert!((a - lo).abs().max_element() <= 1e-5 && (b - hi).abs().max_element() <= 1e-5, "{name}: box {a} {b} against {lo} {hi}");
        assert!(mesh_asset::watertight(&s.points, &s.tris), "{name}: still closed");
    }
    // A mirrored GLB node keeps the mesh wound outward.
    let s = parse(&glb_scaled(&p, &t, Vec3::X, Vec3::new(-1.0, 1.0, 1.0)), Format::Glb).unwrap();
    assert!(mesh_asset::watertight(&s.points, &s.tris) && mesh_asset::volume(&s.points, &s.tris) > 0.0, "outward after a mirror");
    // OBJ quads, negative indices and texture/normal slots.
    let quad = b"v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nf -4/1/1 -3/1/1 -2/1/1 -1/1/1\n";
    assert_eq!(parse(quad, Format::Obj).unwrap().tris, vec![[0, 1, 2], [0, 2, 3]]);
    // 3MF is a zip holding the model.
    let zip = write::three_mf(&p, &t).unwrap();
    assert!(zip.starts_with(b"PK"));
    let model = String::from_utf8_lossy(&zip);
    assert!(model.contains("3D/3dmodel.model") && model.contains(&format!("<triangle v1=\"{}\"", t[0][0])));
}

/// Hostile files: refused before allocating, never a panic.
#[test]
fn malformed_files_are_errors() {
    let bad: [(&[u8], Format); 8] = [
        (b"", Format::Obj),
        (b"f 1 2 3\n", Format::Obj),
        (b"v nan 0 0\nv 0 0 0\nv 1 0 0\nf 1 2 3\n", Format::Obj),
        (b"v 1e30 0 0\nv 0 0 0\nv 1 0 0\nf 1 2 3\n", Format::Obj),
        (b"ply\nformat binary_little_endian 1.0\nelement vertex 4000000000\nproperty float x\nproperty float y\nproperty float z\nend_header\n", Format::Ply),
        (b"ply\nformat ascii 1.0\nelement vertex 3\nproperty float x\nproperty float y\nproperty float z\nelement face 1\nproperty list uchar int vertex_indices\nend_header\n0 0 0\n1 0 0\n0 1 0\n3 0 1 7\n", Format::Ply),
        (b"Kaydara FBX Binary  \x00\x1a\x00\xe8\x1c\x00\x00\xff\xff\xff\xff\x00\x00\x00\x00", Format::Fbx),
        (b"; FBX 7.4.0 project file", Format::Fbx),
    ];
    for (bytes, f) in bad {
        assert!(parse(bytes, f).is_err(), "{:?}: {:?}", f, String::from_utf8_lossy(bytes));
    }
    // A binary STL whose count claims more than its bytes is not binary.
    let mut stl = vec![0u8; 84];
    stl[80..84].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(parse(&stl, Format::Stl).is_err());
}

/// Fuzz-lite: 200 truncations of each format (and a few corrupted bytes)
/// never panic.
#[test]
fn truncated_files_never_panic() {
    let (p, t) = icosphere(2, 0.5);
    let files = [
        (Format::Obj, write::obj(&p, &t)),
        (Format::Stl, write::stl(&p, &t)),
        (Format::Stl, ascii_stl(&p, &t)),
        (Format::Ply, write::ply(&p, &t)),
        (Format::Ply, ascii_ply(&p, &t)),
        (Format::Ply, big_endian_ply(&p, &t)),
        (Format::Glb, glb(&p, &t, Vec3::ZERO)),
        (Format::Fbx, fbx(&p, &t)),
    ];
    let mut rng = crate::brush::rng::Pcg32::new(7, 1);
    for (format, bytes) in files {
        let mut errors = 0;
        for k in 0..200 {
            let cut = bytes.len() * k / 200;
            let r = std::panic::catch_unwind(|| parse(&bytes[..cut], format).is_err());
            assert!(r.is_ok(), "{format:?} cut at {cut} panicked");
            errors += r.unwrap() as usize;
        }
        assert!(errors > 0, "{format:?}: some cuts must fail");
        for _ in 0..50 {
            let mut b = bytes.clone();
            for _ in 0..4 {
                let i = rng.next_u32() as usize % b.len();
                b[i] = rng.next_u32() as u8;
            }
            assert!(std::panic::catch_unwind(|| parse(&b, format).is_ok()).is_ok(), "{format:?} corrupted panicked");
        }
    }
}

/// The icosphere through each file as clay at res 128: Hausdorff within
/// 0.75 cell, volume within 1.5%.
#[test]
fn imported_icosphere_clay_matches_the_mesh() {
    let (p, t) = icosphere(4, 0.5);
    let source = crate::mesh::Mesh::imported(&MeshAsset::new(p.clone(), t.clone()).unwrap());
    let source_volume = crate::harness::geom::volume(&source);
    for (format, bytes) in [(Format::Obj, write::obj(&p, &t)), (Format::Stl, write::stl(&p, &t)), (Format::Ply, write::ply(&p, &t))] {
        let s = parse(&bytes, format).unwrap();
        let asset = MeshAsset::new(s.points, s.tris).unwrap();
        let mut clay = Clay { resolution: 128, ..Clay::default() };
        clay.blobs.push(Blob::of(Form::Mesh { asset: asset.hash().into(), sign: Sign::Closed, center: Vec3::ZERO, rotate: Vec3::ZERO, scale: 1.0 }, Op::Add, [0.8; 3]));
        clay.assets.insert(asset.hash().into(), asset);
        let cell = clay.cell_size().unwrap();
        let mesh = clay.build_mesh();
        let h = crate::harness::geom::hausdorff(&mesh, &source);
        let v = crate::harness::geom::volume(&mesh);
        eprintln!("{format:?}: hausdorff {:.3} cell, volume {:+.3}%", h / cell, (v / source_volume - 1.0) * 100.0);
        assert!(h <= 0.75 * cell, "{format:?}: hausdorff {} cells", h / cell);
        assert!((v / source_volume - 1.0).abs() <= 0.015, "{format:?}: volume {v} against {source_volume}");
    }
}

/// An open cup (a cylinder wall and a floor, no lid) read by its winding
/// number makes a closed, watertight clay facing the way the cup does.
#[test]
fn an_open_cup_closes() {
    let (seg, rings) = (48u32, 16u32);
    let (r, h) = (0.4f32, 0.8f32);
    let mut points = vec![Vec3::new(0.0, -h / 2.0, 0.0)];
    for j in 0..=rings {
        for i in 0..seg {
            let a = std::f32::consts::TAU * i as f32 / seg as f32;
            points.push(Vec3::new(r * a.cos(), -h / 2.0 + h * j as f32 / rings as f32, r * a.sin()));
        }
    }
    let at = |i: u32, j: u32| 1 + j * seg + i % seg;
    let mut tris = Vec::new();
    for i in 0..seg {
        // The floor faces down, the wall out.
        tris.push([0, at(i, 0), at(i + 1, 0)]);
        for j in 0..rings {
            tris.push([at(i, j), at(i, j + 1), at(i + 1, j + 1)]);
            tris.push([at(i, j), at(i + 1, j + 1), at(i + 1, j)]);
        }
    }
    assert!(!mesh_asset::watertight(&points, &tris), "the cup is open");
    let asset = MeshAsset::new(points.clone(), tris.clone()).unwrap();
    let mut clay = Clay { resolution: 96, ..Clay::default() };
    clay.blobs.push(Blob::of(Form::Mesh { asset: asset.hash().into(), sign: Sign::Winding, center: Vec3::ZERO, rotate: Vec3::ZERO, scale: 1.0 }, Op::Add, [0.8; 3]));
    clay.assets.insert(asset.hash().into(), asset.clone());
    let mesh = clay.build_mesh();
    let (mp, mt): (Vec<Vec3>, Vec<[u32; 3]>) = (mesh.vertices.iter().map(|v| Vec3::from(v.pos)).collect(), mesh.indices.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect());
    assert!(mesh_asset::watertight(&mp, &mt), "the clay is closed");
    // The source's normals at its vertices against the clay's surface there.
    let field = crate::clay::Field::new(&clay, 0.01);
    let list: Vec<u32> = (0..field.prim_count() as u32).collect();
    let e = 1e-3;
    let normal = |q: Vec3| {
        let g = |d: Vec3| field.sample(q + d * e, &list).0 - field.sample(q - d * e, &list).0;
        Vec3::new(g(Vec3::X), g(Vec3::Y), g(Vec3::Z)).normalize_or_zero()
    };
    let mut vn = vec![Vec3::ZERO; points.len()];
    for t in &tris {
        let n = (points[t[1] as usize] - points[t[0] as usize]).cross(points[t[2] as usize] - points[t[0] as usize]);
        t.iter().for_each(|&i| vn[i as usize] += n);
    }
    let agree = points.iter().zip(&vn).filter(|(p, n)| normal(**p).dot(n.normalize_or_zero()) > 0.0).count();
    let share = agree as f64 / points.len() as f64;
    eprintln!("open cup: {:.2}% of source normals within 90 degrees", share * 100.0);
    assert!(share >= 0.99, "{share}");
}

/// A 1M-triangle binary STL reads (and becomes a mesh asset) within 3 s.
#[test]
fn a_million_triangle_stl_imports_quickly() {
    let (u, v) = (1000u32, 500u32);
    let mut points = Vec::with_capacity((u * v) as usize);
    for i in 0..u {
        let a = std::f32::consts::TAU * i as f32 / u as f32;
        for j in 0..v {
            let b = std::f32::consts::TAU * j as f32 / v as f32;
            let r = 0.4 + 0.15 * b.cos();
            points.push(Vec3::new(r * a.cos(), 0.15 * b.sin(), r * a.sin()));
        }
    }
    let at = |i: u32, j: u32| (i % u) * v + j % v;
    let tris: Vec<[u32; 3]> = (0..u).flat_map(|i| (0..v).flat_map(move |j| [[at(i, j), at(i, j + 1), at(i + 1, j + 1)], [at(i, j), at(i + 1, j + 1), at(i + 1, j)]])).collect();
    let bytes = write::stl(&points, &tris);
    let ((n, closed), ms) = crate::harness::perf::time(1, || {
        let s = parse(&bytes, Format::Stl).unwrap();
        let closed = mesh_asset::watertight(&s.points, &s.tris);
        let a = MeshAsset::new(s.points, s.tris).unwrap();
        (a.tris().len(), closed)
    });
    eprintln!("1M-triangle STL ({} MB): {ms:.0} ms", bytes.len() >> 20);
    assert_eq!(n, 1_000_000);
    assert!(closed);
    crate::harness::perf::check("import_stl_1m", ms, 3000.0, json!({ "triangles": n }));
}

/// Points no face uses are dropped on read and by `decimated`, so a clay
/// built from them stays within the field's point cap.
#[test]
fn unused_points_are_dropped() {
    let mut obj = b"v 9 9 9\nv 0 0 0\nv 8 8 8\nv 1 0 0\nv 0 1 0\n".to_vec();
    obj.extend(b"f 2 4 5\n");
    let s = parse(&obj, Format::Obj).unwrap();
    assert_eq!(s.points, vec![Vec3::ZERO, Vec3::X, Vec3::Y]);
    assert_eq!(s.tris, vec![[0, 1, 2]]);
    // An asset carrying stray points (as a mesh object's may) compacts.
    let points: Vec<Vec3> = (0..100).map(|i| Vec3::splat(i as f32)).chain([Vec3::ZERO, Vec3::X, Vec3::Y]).collect();
    let a = MeshAsset::new(points, vec![[100, 101, 102]]).unwrap();
    let d = a.decimated(mesh_asset::MAX_FIELD_TRIANGLES).unwrap();
    assert_eq!((d.points().len(), d.tris().len()), (3, 1));
    assert!(mesh_asset::check_clay_assets([&d]).is_ok());
    // An asset whose points are all used is itself.
    let (p, t) = icosphere(1, 0.5);
    let ico = MeshAsset::new(p, t).unwrap();
    assert_eq!(ico.decimated(mesh_asset::MAX_FIELD_TRIANGLES).unwrap().hash(), ico.hash());
}

/// A child node ending past its parent is refused, never re-parsed.
#[test]
fn fbx_children_stay_inside_their_parent() {
    let (p, t) = icosphere(1, 0.5);
    let mut bytes = fbx(&p, &t);
    // GlobalSettings starts after the 27-byte header; Properties70 is its
    // first child, after its 13-byte record head and 14-byte name.
    let parent_end = u32::from_le_bytes(bytes[27..31].try_into().unwrap());
    let child = 27 + 13 + "GlobalSettings".len();
    assert_eq!(&bytes[child + 13..child + 13 + 12], b"Properties70");
    bytes[child..child + 4].copy_from_slice(&(parent_end + 1).to_le_bytes());
    let e = parse(&bytes, Format::Fbx).unwrap_err();
    assert!(e.contains("outside its parent"), "{e}");
}
