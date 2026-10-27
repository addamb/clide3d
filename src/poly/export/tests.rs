//! Stage 3d: the files `export_poly` writes, read back with known answers.

use std::collections::BTreeMap;
use std::sync::Arc;

use glam::Vec3;

use super::*;
use crate::poly::Chunked;
use crate::poly::project::tests::cube_sphere;
use crate::poly::subdiv::evaluate;

/// A pentagonal prism (two pentagons, five quads), a tetrahedron beside
/// it (triangles) and a point no face uses; creases infinite, fractional
/// and small; UVs per corner (seams everywhere: every face its own spot).
pub(crate) fn mixed() -> Poly {
    let mut points = Vec::new();
    for z in [-0.5f32, 0.5] {
        for k in 0..5 {
            let a = k as f32 * std::f32::consts::TAU / 5.0;
            points.push(Vec3::new(a.cos() * 0.7, a.sin() * 0.7, z));
        }
    }
    points.extend([Vec3::new(2.0, 0.0, 0.0), Vec3::new(3.0, 0.0, 0.0), Vec3::new(2.5, 1.0, 0.0), Vec3::new(2.5, 0.3, 0.9)]);
    points.push(Vec3::new(9.0, 9.0, 9.0));
    let mut faces = vec![vec![4, 3, 2, 1, 0], vec![5, 6, 7, 8, 9]];
    for k in 0..5u32 {
        faces.push(vec![k, (k + 1) % 5, (k + 1) % 5 + 5, k + 5]);
    }
    faces.extend([vec![10, 12, 11], vec![10, 11, 13], vec![11, 12, 13], vec![12, 10, 13]]);
    let mut p = Poly::from_faces(&points, &faces).unwrap();
    let mut creases = BTreeMap::new();
    creases.insert((0, 1), 10.0);
    creases.insert((5, 6), 2.5);
    creases.insert((0, 5), 0.3);
    creases.insert((10, 11), 1.0);
    p.creases = Arc::new(creases);
    let uv: Vec<[f32; 2]> = (0..p.corners.len()).map(|i| [(i as f32 * 0.037).fract(), (i as f32 * 0.0731 + 1e-3).fract()]).collect();
    p.uv = Some(Chunked::new(&uv));
    p
}

/// What a file can carry: the cage, creases and UVs (no levels, shown
/// level, displacement or provenance).
fn stripped(p: &Poly) -> Poly {
    Poly { levels: Vec::new(), level: 0, made_by: None, projected: None, ..p.clone() }
}

/// The plan's `obj_cage_round_trip`: write, read back and compare. The
/// topology and creases are identical and the points and UVs come back
/// bit for bit (shortest round-trip floats; v flipped in f64), so the
/// re-read cage has the same hash as the Poly's cage.
#[test]
fn obj_cage_round_trip() {
    let mut sphere = cube_sphere(5, 0.5);
    sphere.uv = Some(Chunked::new(&crate::poly::uv::unwrap(&sphere, 1024, 4).unwrap().uv));
    let t = sphere.topology();
    sphere.creases = Arc::new(t.edges.iter().take(12).enumerate().map(|(i, e)| ((e[0], e[1]), [10.0, 1.5, 0.25][i % 3])).collect());
    let mut no_uv = cube_sphere(3, 1.0);
    no_uv.made_by = Some(Arc::new(serde_json::json!({ "action": "test" })));
    for p in [mixed(), sphere, no_uv] {
        let cage = Cage::of(&p, 0).unwrap();
        let bytes = obj(&cage, "Test Poly", &["a note".into()]);
        assert!(bytes.starts_with(OBJ_HEADER.as_bytes()));
        let (back, warnings) = crate::mesh_import::read_cage(&bytes).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!((back.face_start.clone(), back.corners.clone(), back.creases.clone()), (cage.face_start.clone(), cage.corners.clone(), cage.creases.clone()));
        assert_eq!(back.points.iter().map(|p| p.to_array().map(f32::to_bits)).collect::<Vec<_>>(), cage.points.iter().map(|p| p.to_array().map(f32::to_bits)).collect::<Vec<_>>(), "points bit for bit");
        match (&back.uv, &cage.uv) {
            (Some(a), Some(b)) => {
                let worst = a.iter().zip(b).map(|(x, y)| (x[0] - y[0]).abs().max((x[1] - y[1]).abs())).fold(0.0f32, f32::max);
                assert_eq!(worst, 0.0, "UVs bit for bit");
            }
            (None, None) => {}
            other => panic!("UVs {other:?}"),
        }
        let poly = back.to_poly().unwrap();
        assert_eq!(poly.hash(), stripped(&p).hash(), "the same cage, creases and UVs");
    }
}

/// A finer level written as the cage: its creases are the cage's less one
/// per level (infinite kept), its UVs face-varying linear, so the
/// re-read level subdivided once more equals the Poly's next level.
#[test]
fn finer_levels_write_decayed_creases() {
    let mut p = mixed();
    p.set_levels(3).unwrap();
    let written = Cage::of(&p, 2).unwrap();
    let infinite = written.creases.iter().filter(|c| c.2 == INFINITE_SHARPNESS).count();
    let half = written.creases.iter().filter(|c| (c.2 - 0.5).abs() < 1e-6).count();
    // (0, 1) infinite: 4 child edges at level 2; (5, 6) 2.5: 0.5 on 4.
    assert_eq!((infinite, half, written.creases.len()), (4, 4, 8), "{:?}", written.creases);
    let (back, _) = crate::mesh_import::read_cage(&obj(&written, "level 2", &[])).unwrap();
    let mut again = back.to_poly().unwrap();
    again.set_levels(1).unwrap();
    let next = evaluate(&again, 1).unwrap();
    let want = evaluate(&p, 3).unwrap();
    assert_eq!((next.face_start.len(), next.corners.len()), (want.face_start.len(), want.corners.len()));
    // Points are numbered differently (edges are sorted afresh), so
    // compare each level-3 face's corner points as sets.
    let mut a: Vec<[i64; 3]> = next.points.iter().map(|q| (*q * 1e5).round().as_i64vec3().to_array()).collect();
    let mut b: Vec<[i64; 3]> = want.points.iter().map(|q| (*q * 1e5).round().as_i64vec3().to_array()).collect();
    a.sort_unstable();
    b.sort_unstable();
    let off = a.iter().zip(&b).filter(|(x, y)| x.iter().zip(y.iter()).any(|(u, v)| (u - v).abs() > 1)).count();
    assert_eq!(off, 0, "level 2 written then subdivided once is level 3 (to 1e-5)");
    // UVs: level 3's from the written level equal the Poly's level 3 as
    // a multiset too.
    let key = |u: &[f32; 2]| [(u[0] * 1e5).round() as i64, (u[1] * 1e5).round() as i64];
    let mut ua: Vec<[i64; 2]> = crate::poly::uv::at_level(&again, 1).unwrap().iter().map(key).collect();
    let mut ub: Vec<[i64; 2]> = crate::poly::uv::at_level(&p, 3).unwrap().iter().map(key).collect();
    ua.sort_unstable();
    ub.sort_unstable();
    assert_eq!(ua, ub);
}

/// The reader refuses what a cage cannot be, from the counts, and never
/// panics on damaged files.
#[test]
fn obj_reader_refuses_and_never_panics() {
    let cases: [(&str, &str); 8] = [
        ("v 0 0 0\nv 1 0 0\nf 1 2\n", "at least 3"),
        ("v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 4\n", "outside"),
        ("v 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nf 1/1 2 3\n", "all or none"),
        ("v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\nt crease 2/1 0 7 2\n", "past the"),
        ("v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\nt crease 2/1 0 1 -2\n", "positive"),
        ("v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\nt crease 2/1 0 1 2\nt crease 2/1 1 0 3\n", "twice"),
        ("v 0 0\n", "x y z"),
        ("# nothing\n", "no faces"),
    ];
    for (text, says) in cases {
        let e = crate::mesh_import::read_cage(text.as_bytes()).unwrap_err();
        assert!(e.contains(says), "{text:?}: {e}");
    }
    let big: String = std::iter::once("v 0 0 0\n".repeat(70)).chain(std::iter::once(format!("f {}\n", (1..=65).map(|i| i.to_string()).collect::<Vec<_>>().join(" ")))).collect();
    assert!(crate::mesh_import::read_cage(big.as_bytes()).unwrap_err().contains("more than 64 sides"));
    // A crease that is not an edge passes the reader and is refused
    // when the Poly is made.
    let (c, _) = crate::mesh_import::read_cage(b"v 0 0 0\nv 1 0 0\nv 0 1 0\nv 1 1 0\nf 1 2 4 3\nt crease 2/1 0 3 2\n").unwrap();
    assert!(c.to_poly().unwrap_err().contains("not an edge"));
    // Damaged copies of a real file: refused or read, never a panic.
    let good = obj(&Cage::of(&mixed(), 0).unwrap(), "m", &[]);
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let (mut read, mut refused) = (0, 0);
    for _ in 0..3000 {
        let mut b = good.clone();
        for _ in 0..1 + next() % 6 {
            let i = (next() % b.len() as u64) as usize;
            b[i] = b"0123456789 -./\nfvt#e"[(next() % 20) as usize];
        }
        let r = std::panic::catch_unwind(|| crate::mesh_import::read_cage(&b).and_then(|(c, _)| c.to_poly()));
        match r.expect("never panics") {
            Ok(_) => read += 1,
            Err(_) => refused += 1,
        }
    }
    eprintln!("damaged OBJ cages: {read} read, {refused} refused");
    assert!(read > 50 && refused > 50, "{read} {refused}");
}

/// A sculpted head retopologized (5k quads) and projected over 4 levels
/// with UVs: (clay, Poly).
fn head(resolution: u32) -> (crate::clay::Clay, Poly) {
    use crate::poly::project::tests::{retopo_cage, run, sculpt};
    use crate::poly::project::{Args, Mode, REACH_EDGES, mean_edge};
    let mut c = crate::poly::retopo::tests::clay(crate::poly::retopo::tests::head(), resolution);
    c.detail_cell = Some(0.006);
    sculpt(&mut c, &[
        serde_json::json!({ "brush": "noise", "point": [0.0, 0.1, 0.35], "radius": 0.3, "strength": 1.0, "scale": 0.05, "pattern": "bumps" }),
        serde_json::json!({ "brush": "standard", "points": [[0.2, 0.2, 0.25], [0.25, 0.1, 0.25]], "radius": 0.05, "strength": 0.5 }),
        serde_json::json!({ "brush": "inflate", "points": [[-0.2, -0.1, 0.28]], "radius": 0.06, "strength": 0.5 }),
    ]);
    let cage = retopo_cage(&c, 5000);
    let (mut p, _, _) = run(&cage, &c, Args { levels: 4, mode: Mode::Vector, cone: 45.0, reach: REACH_EDGES * mean_edge(&cage) });
    p.uv = Some(Chunked::new(&crate::poly::uv::unwrap(&p, 4096, 4).unwrap().uv));
    (c, p)
}

fn request(dir: &std::path::Path, stem: &str, maps: Vec<MapKind>, map_size: u32, map_level: u8) -> Request {
    Request { path: dir.join(format!("{stem}.obj")), geometry: Geometry::Obj, level: 0, maps, map_level, map_size, vector_format: crate::outputs::Format::Exr, height_format: crate::outputs::Format::Exr, space: Space::Tangent, verify: true, udim: None }
}

/// Exports `p` with `c` as the maps' clay (identity placement).
fn export_with(c: &crate::clay::Clay, p: &Poly, req: &Request) -> serde_json::Value {
    let composed = crate::layers::composed_clay(c);
    let rest = composed.build_mesh();
    let src = crate::bake::Source { clay: &composed, rest: &rest, skin: None, parts: &[], library: &[], fallback: "", fine: None, low: None };
    let maps = MapSource { src, place: Placement::of(glam::Mat4::IDENTITY).unwrap() };
    req.check_paths().unwrap();
    export(p, "head", glam::Mat4::IDENTITY, &crate::material::Material::default(), req, Some(&maps), &Default::default()).unwrap()
}

/// The plan's `export_verify`: the head exported at level 0 with a
/// level-4 vector map; the files read back alone (the OBJ's cage
/// subdivided 4 times, the EXR added) lie within 0.75 cell of the clay.
#[test]
fn export_verify() {
    let (c, p) = head(96);
    let dir = std::env::temp_dir().join(format!("clide3d_export_verify_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let t = std::time::Instant::now();
    let r = export_with(&c, &p, &request(&dir, "head", vec![MapKind::VectorDisplacement], 2048, 4));
    eprintln!("head export ({} faces, level-4 2K vector map) in {:.2}s: {}", p.faces(), t.elapsed().as_secs_f32(), r["verify"]);
    let v = &r["verify"];
    assert_eq!(v["cage"]["same_topology"], serde_json::json!(true));
    assert!(v["maps"]["max_error_cells"].as_f64().unwrap() <= 0.75, "{v}");
    // Level 1 written (creases decayed, UVs refined): the re-read level
    // subdivided 2 more times with a level-3 map verifies too.
    let mut finer = request(&dir, "head1", vec![MapKind::VectorDisplacement], 1024, 3);
    finer.level = 1;
    let r1 = export_with(&c, &p, &finer);
    eprintln!("level 1 with a level-3 map: {}", r1["verify"]);
    assert_eq!(r1["verify"]["cage"]["same_topology"], serde_json::json!(true));
    assert!(r1["verify"]["maps"]["max_error_cells"].as_f64().unwrap() <= 0.75, "{}", r1["verify"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The G channel's direction, measured (the sidecar says it): on the
/// maps' level, the bitangent cross(normal, tangent) * w points toward
/// -v in the top-left UVs maps use (toward +v in the OBJ's).
#[test]
fn bitangent_points_up_the_image() {
    let mut p = cube_sphere(6, 0.5);
    p.uv = Some(Chunked::new(&crate::poly::uv::unwrap(&p, 1024, 4).unwrap().uv));
    let rest = crate::mesh::Mesh::default();
    let low = crate::poly::low::Low::new(&p, 1, glam::Mat4::IDENTITY, &rest, 1024, &Default::default()).unwrap();
    let m = &low.mesh;
    let (mut up, mut down) = (0, 0);
    for t in m.indices.chunks_exact(3) {
        let v = |i: usize| &m.vertices[t[i] as usize];
        let (p0, p1, p2) = (Vec3::from(v(0).pos), Vec3::from(v(1).pos), Vec3::from(v(2).pos));
        let (w0, w1, w2) = (glam::Vec2::from(v(0).uv), glam::Vec2::from(v(1).uv), glam::Vec2::from(v(2).uv));
        let (e1, e2, d1, d2) = (p1 - p0, p2 - p0, w1 - w0, w2 - w0);
        let r = d1.perp_dot(d2);
        if r.abs() < 1e-12 {
            continue;
        }
        let dpdv = (e2 * d1.x - e1 * d2.x) / r;
        let n = Vec3::from(v(0).normal);
        let tan = Vec3::from_slice(&v(0).tangent);
        let bit = n.cross(tan) * v(0).tangent[3];
        if bit.dot(dpdv) < 0.0 { up += 1 } else { down += 1 }
    }
    assert!(up > 0 && down == 0, "toward -v (top-left UVs) on {up} triangles, +v on {down}");
}

/// The plan's performance bound: a 5k cage with two 4K EXR maps (vector
/// and height) in at most 10 s, verify included (CLIDE_PERF=1 holds it).
#[test]
#[ignore]
fn export_perf() {
    let (c, p) = head(160);
    let dir = std::env::temp_dir().join(format!("clide3d_export_perf_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let t = std::time::Instant::now();
    let r = export_with(&c, &p, &request(&dir, "head", vec![MapKind::VectorDisplacement, MapKind::Displacement], 4096, 4));
    let s = t.elapsed().as_secs_f32();
    eprintln!("export of {} faces with two 4K EXR maps at level 4 in {s:.2}s (bound 10): {} {}", p.faces(), r["timings"], r["verify"]);
    assert!(r["verify"]["maps"]["max_error_cells"].as_f64().unwrap() <= 0.75);
    if std::env::var("CLIDE_PERF").is_ok() {
        assert!(s <= 10.0, "{s}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The overwrite rule fails closed (security review): a file that cannot
/// be read, a folder, a symbolic link (even to a file this writer made)
/// and a file of another format at any path the call would write (the
/// geometry, the sidecar, a map) are refused and left as they were; an
/// empty file and this writer's own files may be replaced.
#[test]
fn overwrite_rule_fails_closed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("clide3d_overwrite_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let req = |stem: &str| Request { path: dir.join(format!("{stem}.obj")), geometry: Geometry::Obj, level: 0, maps: vec![MapKind::VectorDisplacement, MapKind::Normal], map_level: 0, map_size: 512, vector_format: crate::outputs::Format::Exr, height_format: crate::outputs::Format::Png, space: Space::Tangent, verify: false, udim: None };
    let ours = format!("{OBJ_HEADER}: a cage\n").into_bytes();
    // Refused: each case at one of the paths, the rest clear.
    let locked = |p: &std::path::Path, bytes: &[u8]| {
        std::fs::write(p, bytes).unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o000)).unwrap();
    };
    locked(&dir.join("a.obj"), &ours);
    std::fs::create_dir_all(dir.join("b.obj")).unwrap();
    std::fs::write(dir.join("c.obj"), b"\x89PNG\r\n\x1a\nnot a cage").unwrap();
    std::fs::write(dir.join("real.obj"), &ours).unwrap();
    std::os::unix::fs::symlink(dir.join("real.obj"), dir.join("d.obj")).unwrap();
    std::fs::write(dir.join("e.json"), b"{\"mine\": true}").unwrap();
    std::fs::create_dir_all(dir.join("f.json")).unwrap();
    locked(&dir.join("g_vector_displacement.exr"), &[0x76, 0x2f, 0x31, 0x01, 0, 0]);
    std::fs::write(dir.join("h_normal.png"), b"plain text notes").unwrap();
    std::os::unix::fs::symlink(dir.join("real.obj"), dir.join("i.json")).unwrap();
    for (stem, says) in [("a", "cannot be read"), ("b", "folder"), ("c", "not a export_poly .obj"), ("d", "not a regular file"), ("e", "sidecar"), ("f", "folder"), ("g", "cannot be read"), ("h", "not a Png"), ("i", "not a regular file")] {
        let e = req(stem).check_paths().unwrap_err();
        assert!(e.contains(says), "{stem}: {e}");
    }
    std::fs::set_permissions(dir.join("a.obj"), std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(dir.join("g_vector_displacement.exr"), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(std::fs::read(dir.join("a.obj")).unwrap(), ours, "left untouched");
    assert_eq!(std::fs::read(dir.join("c.obj")).unwrap(), b"\x89PNG\r\n\x1a\nnot a cage");
    assert_eq!(std::fs::read(dir.join("real.obj")).unwrap(), ours);
    // Allowed: nothing there, an empty file, this writer's own files.
    req("new").check_paths().unwrap();
    std::fs::write(dir.join("empty.obj"), b"").unwrap();
    req("empty").check_paths().unwrap();
    req("real").check_paths().unwrap();
    // The image rule the bake tool and renders use refuses the same.
    locked(&dir.join("x.png"), b"\x89PNG\r\n\x1a\n");
    assert!(crate::outputs::writable(&dir.join("x.png"), crate::outputs::Kind::Png).unwrap_err().contains("cannot be read"));
    std::fs::set_permissions(dir.join("x.png"), std::fs::Permissions::from_mode(0o644)).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Splits a .glb into its JSON and BIN chunks.
fn chunks(bytes: &[u8]) -> (serde_json::Value, Vec<u8>) {
    let jlen = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    (serde_json::from_slice(&bytes[20..20 + jlen]).unwrap(), bytes[20 + jlen + 8..].to_vec())
}

/// Packs JSON and BIN chunks into a .glb (for damaged copies).
fn pack(doc: &serde_json::Value, bin: &[u8]) -> Vec<u8> {
    let mut json = serde_json::to_vec(doc).unwrap();
    while json.len() % 4 != 0 {
        json.push(b' ');
    }
    let total = 28 + json.len() + bin.len();
    let mut out = Vec::new();
    out.extend(b"glTF");
    out.extend(2u32.to_le_bytes());
    out.extend((total as u32).to_le_bytes());
    out.extend((json.len() as u32).to_le_bytes());
    out.extend(b"JSON");
    out.extend(&json);
    out.extend((bin.len() as u32).to_le_bytes());
    out.extend(b"BIN\0");
    out.extend(bin);
    out
}

/// A Poly level as a .glb validates structurally (views inside the
/// buffer, accessors inside their views with true bounds, indices within
/// the vertices) and carries its cage back exactly; damaged copies are
/// refused, each for its reason.
#[test]
fn glb_validates_and_carries_the_cage() {
    let mut p = mixed();
    p.set_levels(2).unwrap();
    let cage = Cage::of(&p, 2).unwrap();
    let mesh = cage_mesh(&cage);
    let bytes = crate::gltf::poly_glb("mixed", &mesh, &crate::material::Material::default(), glam::Mat4::from_translation(Vec3::X), &Cage::of(&p, 0).unwrap(), serde_json::json!({ "level_written": 2 })).unwrap();
    let check = check_glb(&bytes, &Default::default()).unwrap();
    eprintln!("glb: {}", check.summary);
    assert_eq!(check.summary["triangles"].as_u64(), Some(p.faces_at(2) * 2), "two triangles a quad at level 2");
    let back = check.cage.unwrap();
    assert_eq!(back.to_poly().unwrap().hash(), stripped(&p).hash(), "the cage, creases and UVs, bit for bit");
    // Also readable by the importer's own glTF reader.
    let loaded = crate::glb_read::from_bytes(&bytes, None, None).unwrap();
    assert_eq!(loaded.items.len(), 1);
    let (doc, bin) = chunks(&bytes);
    let damaged = |f: &dyn Fn(&mut serde_json::Value, &mut Vec<u8>)| {
        let (mut d, mut b) = (doc.clone(), bin.clone());
        f(&mut d, &mut b);
        check_glb(&pack(&d, &b), &Default::default()).err().unwrap_or_default()
    };
    assert!(damaged(&|d, _| d["accessors"][0]["count"] = serde_json::json!(d["accessors"][0]["count"].as_u64().unwrap() * 10)).contains("passes bufferView"));
    assert!(damaged(&|d, _| d["bufferViews"][0]["byteLength"] = serde_json::json!(1u64 << 30)).contains("passes the buffer"));
    assert!(damaged(&|d, _| d["accessors"][0]["max"][0] = serde_json::json!(99.0)).contains("not the data's bounds"));
    assert!(damaged(&|d, _| d["buffers"][0]["byteLength"] = serde_json::json!(4)).contains("BIN chunk"));
    assert!(damaged(&|d, _| d["nodes"][0]["mesh"] = serde_json::json!(7)).contains("mesh 7 does not exist"));
    // An index past the vertices.
    let ia = doc["meshes"][0]["primitives"][0]["indices"].as_u64().unwrap() as usize;
    let view = doc["accessors"][ia]["bufferView"].as_u64().unwrap() as usize;
    let off = doc["bufferViews"][view]["byteOffset"].as_u64().unwrap() as usize;
    assert!(damaged(&|_, b| b[off..off + 4].copy_from_slice(&u32::MAX.to_le_bytes())).contains("past the"));
    let mut cut = bytes.clone();
    cut.truncate(cut.len() - 8);
    assert!(check_glb(&cut, &Default::default()).is_err());
}

/// The plan's `usda_is_wellformed` (optional, D18): the USDA parses with
/// the in-house checker (balanced brackets, required attributes,
/// consistent counts) and gives the cage, creases and UVs back exactly,
/// at the cage and at a finer level; damaged copies are refused, and
/// none panics.
#[test]
fn usda_is_wellformed() {
    let p = mixed();
    let tex = [usda::Texture { file: "m_displacement.png".into(), input: "displacement", output: "r", kind: "float", raw: true, scale: [0.2, 0.2, 0.2, 1.0], bias: [-0.1, -0.1, -0.1, 0.0] }];
    let text = usda::write(&Cage::of(&p, 0).unwrap(), "Mixed Poly", glam::Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0)), &tex, "a \"quoted\" note");
    let s = std::str::from_utf8(&text).unwrap();
    for needed in ["#usda 1.0", "subdivisionScheme = \"catmullClark\"", "interpolateBoundary = \"edgeAndCorner\"", "faceVaryingLinearInterpolation = \"all\"", "creaseSharpnesses", "interpolation = \"faceVarying\"", "UsdPreviewSurface", "outputs:displacement.connect", "@m_displacement.png@", "( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (1, 2, 3, 1) )"] {
        assert!(s.contains(needed), "{needed}\n{s}");
    }
    let back = usda::read(&text).unwrap();
    assert_eq!(back.to_poly().unwrap().hash(), stripped(&p).hash(), "the cage, creases and UVs, bit for bit");
    let mut q = p.clone();
    q.set_levels(2).unwrap();
    let level2 = Cage::of(&q, 2).unwrap();
    assert_eq!(usda::read(&usda::write(&level2, "q", glam::Mat4::IDENTITY, &[], "")).unwrap(), level2);
    let cases: [(&dyn Fn(&str) -> String, &str); 7] = [
        (&|t| t.replacen("#usda 1.0", "#usda 9", 1), "start with"),
        (&|t| t.replacen("def Mesh \"cage\"\n    {", "def Mesh \"cage\"\n    {{", 1), "open"),
        (&|t| t.replacen("int[] faceVertexCounts = [5,", "int[] faceVertexCounts = [6,", 1), "add up"),
        (&|t| t.replacen("int[] faceVertexIndices = [4,", "int[] faceVertexIndices = [99,", 1), "names point"),
        (&|t| t.replacen("interpolation = \"faceVarying\"", "interpolation = \"vertex\"", 1), "faceVarying"),
        (&|t| t.replacen("int[] creaseLengths = [2,", "int[] creaseLengths = [3,", 1), "do not agree"),
        (&|t| t.replacen("uniform token subdivisionScheme = \"catmullClark\"", "uniform token subdivisionScheme = \"none\"", 1), "catmullClark"),
    ];
    for (f, says) in cases {
        let e = usda::read(f(s).as_bytes()).unwrap_err();
        assert!(e.contains(says), "{says}: {e}");
    }
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let (mut read, mut refused) = (0, 0);
    for _ in 0..2000 {
        let mut b = text.clone();
        for _ in 0..1 + next() % 4 {
            let i = (next() % b.len() as u64) as usize;
            b[i] = b"0123456789 ,()[]{}\"@-."[(next() % 22) as usize];
        }
        match std::panic::catch_unwind(|| usda::read(&b).and_then(|c| c.to_poly())).expect("never panics") {
            Ok(_) => read += 1,
            Err(_) => refused += 1,
        }
    }
    eprintln!("damaged USDA: {read} read, {refused} refused");
    assert!(read > 20 && refused > 200, "{read} {refused}");
}

/// §0.2 rule 4: the files are the same bits on 1 and 12 threads (the
/// maps' level, both bakes and the parallel nearest-vertex lookups).
#[test]
fn export_is_thread_independent() {
    use crate::poly::project::tests::noise_sphere;
    let c = noise_sphere();
    let mut p = cube_sphere(8, 0.5);
    p.uv = Some(Chunked::new(&crate::poly::uv::unwrap(&p, 512, 4).unwrap().uv));
    let files = |threads: usize| {
        let dir = std::env::temp_dir().join(format!("clide3d_export_threads_{}_{threads}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut req = request(&dir, "s", vec![MapKind::VectorDisplacement, MapKind::Displacement, MapKind::Normal, MapKind::Ao], 512, 1);
        req.verify = false;
        crate::clay::with_threads(threads, || export_with(&c, &p, &req));
        let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap()).map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap())).collect();
        out.sort();
        let _ = std::fs::remove_dir_all(&dir);
        out
    };
    let (one, many) = (files(1), files(12));
    assert_eq!(one.len(), 6, "geometry, sidecar and four maps");
    for ((a, x), (b, y)) in one.iter().zip(&many) {
        assert!(a == b && x == y, "{a} differs between 1 and 12 threads");
    }
}

/// Review (3d, items 1-2): OpenSubdiv reads `nint/nfloat/nstring` counts
/// and RenderMan's boundary values (1 edge-and-corner, 2 edge-only): the
/// writer says `2/1/0` and `1/0/0 1`, the reader takes them, and files
/// from before (`2/1`, `1/0 2`) still read the same, with no warning.
#[test]
fn obj_tags_follow_opensubdiv() {
    let p = mixed();
    let text = String::from_utf8(obj(&Cage::of(&p, 0).unwrap(), "m", &[])).unwrap();
    assert!(text.lines().any(|l| l == "t interpolateboundary 1/0/0 1"), "{text}");
    let creases: Vec<&str> = text.lines().filter(|l| l.starts_with("t crease ")).collect();
    assert_eq!(creases.len(), p.creases.len());
    assert!(creases.iter().all(|l| l.starts_with("t crease 2/1/0 ")), "{creases:?}");
    assert!(!text.lines().any(|l| l.starts_with("t ") && (l.contains(" 2/1 ") || l.contains(" 1/0 "))));
    let (back, warnings) = crate::mesh_import::read_cage(text.as_bytes()).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(back.to_poly().unwrap().hash(), stripped(&p).hash());
    let quad = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n";
    for (tags, warned) in [
        ("t interpolateboundary 1/0 2\nt crease 2/1 0 1 2\n", false),
        ("t interpolateboundary 1/0/0 1\nt crease 2/1/0 0 1 2\n", false),
        ("t interpolateboundary 1/0/0 2\nt crease 2/1/0 0 1 2\n", true),
        ("t interpolateboundary 1/0 1\nt crease 2/1 0 1 2\n", true),
    ] {
        let (c, w) = crate::mesh_import::read_cage(format!("{quad}{tags}").as_bytes()).unwrap();
        assert_eq!(c.creases, vec![(0, 1, 2.0)], "{tags}");
        assert_eq!(w.iter().any(|w| w.contains("boundary")), warned, "{tags}: {w:?}");
    }
}

/// Review (3d, item 3): a `t` line is read only as far as its counts
/// say, so a huge one allocates nothing; a known tag with more words is
/// refused. OpenSubdiv's crease chains (`N/1/0 i j k ... s`, up to
/// `MAX_CREASE_CHAIN` points) read as their edges.
#[test]
fn long_tag_lines_are_refused_without_collecting() {
    let quad = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\n";
    let mut text = format!("{quad}t crease 2/1/0 0 1 2");
    text.push_str(&" 7".repeat(4_000_000));
    let t = std::time::Instant::now();
    let e = crate::mesh_import::read_cage(text.as_bytes()).err().unwrap();
    assert!(e.contains("needs 2 points and a sharpness"), "{e}");
    eprintln!("an 8 MB t line refused in {:?}", t.elapsed());
    // A huge unknown tag is skipped without being kept.
    let mut other = format!("{quad}t corner 1/1/0 0 2");
    other.push_str(&" 7".repeat(4_000_000));
    let (_, w) = crate::mesh_import::read_cage(other.as_bytes()).unwrap();
    assert!(w.iter().any(|w| w.contains("skipped")), "{w:?}");
    assert!(crate::mesh_import::read_cage(format!("{quad}t crease 2/1/0 0 1 2 x\n").as_bytes()).unwrap_err().contains("needs"));
    assert!(crate::mesh_import::read_cage(format!("{quad}t crease 99999999/1/0 0 1 2\n").as_bytes()).unwrap_err().contains("chain"));
    // A chain of four points around the quad: three edges.
    let (c, w) = crate::mesh_import::read_cage(format!("{quad}t crease 4/1/0 0 1 2 3 2.5\n").as_bytes()).unwrap();
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(c.creases, vec![(0, 1, 2.5), (1, 2, 2.5), (2, 3, 2.5)]);
}

/// Review (3d, item 4): a .glb whose accessors all read one large view
/// is refused by the scan budget (twice the BIN chunk), and a cancelled
/// check stops; counts past `MAX_GLB_ITEMS` are refused from the JSON.
#[test]
fn glb_checks_are_bounded_and_cancellable() {
    let mut p = mixed();
    p.set_levels(1).unwrap();
    let cage = Cage::of(&p, 1).unwrap();
    let bytes = crate::gltf::poly_glb("mixed", &cage_mesh(&cage), &crate::material::Material::default(), glam::Mat4::IDENTITY, &Cage::of(&p, 0).unwrap(), serde_json::json!({})).unwrap();
    let (doc, bin) = chunks(&bytes);
    // The POSITION accessor, repeated: each copy rescans its bytes.
    let pa = doc["meshes"][0]["primitives"][0]["attributes"]["POSITION"].as_u64().unwrap() as usize;
    let mut d = doc.clone();
    let copy = d["accessors"][pa].clone();
    let per = (copy["count"].as_u64().unwrap() * 12) as usize;
    let n = (2 * bin.len() + (1 << 20)) / per + 2;
    assert!(n < MAX_GLB_ITEMS, "{n}");
    for _ in 0..n {
        d["accessors"].as_array_mut().unwrap().push(copy.clone());
    }
    let e = check_glb(&pack(&d, &bin), &Default::default()).err().unwrap_or_default();
    assert!(e.contains("over and over"), "{e}");
    let mut d = doc.clone();
    d["accessors"] = serde_json::Value::Array(vec![copy; MAX_GLB_ITEMS + 1]);
    assert!(check_glb(&pack(&d, &bin), &Default::default()).err().unwrap_or_default().contains("more than"));
    let cancelled = crate::progress::Progress::default();
    cancelled.cancel();
    assert!(check_glb(&bytes, &cancelled).is_err(), "a cancelled check stops");
    check_glb(&bytes, &Default::default()).unwrap();
}

/// Review (3d, items 5, 7, 8 and 9): a file name with @ is refused (a
/// USD asset path cannot hold it); a .glb or .usda is replaced only when
/// this writer made it; a link put at a path after the check is not
/// written through; names and notes cannot start lines of their own.
#[test]
fn paths_names_and_late_links() {
    let dir = std::env::temp_dir().join(format!("clide3d_review3d_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let req = |file: &str, geometry: Geometry| Request { path: dir.join(file), geometry, level: 0, maps: vec![MapKind::Displacement], map_level: 0, map_size: 512, vector_format: crate::outputs::Format::Exr, height_format: crate::outputs::Format::Png, space: Space::Tangent, verify: false, udim: None };
    assert!(req("a@b.usda", Geometry::Usda).check_paths().unwrap_err().contains('@'));
    assert!(req("a\rb.obj", Geometry::Obj).check_paths().unwrap_err().contains("control"));
    // Another program's glTF and USDA files are kept; this writer's go.
    std::fs::write(dir.join("theirs.glb"), b"glTF\x02\0\0\0 someone else's scene").unwrap();
    std::fs::write(dir.join("theirs.usda"), b"#usda 1.0\n(\n    doc = \"a set\"\n)\n").unwrap();
    assert!(req("theirs.glb", Geometry::Glb).check_paths().unwrap_err().contains("refusing"));
    assert!(req("theirs.usda", Geometry::Usda).check_paths().unwrap_err().contains("refusing"));
    let p = mixed();
    let ours = crate::gltf::poly_glb("mixed", &cage_mesh(&Cage::of(&p, 0).unwrap()), &crate::material::Material::default(), glam::Mat4::IDENTITY, &Cage::of(&p, 0).unwrap(), serde_json::json!({})).unwrap();
    std::fs::write(dir.join("ours.glb"), &ours).unwrap();
    req("ours.glb", Geometry::Glb).check_paths().unwrap();
    std::fs::write(dir.join("ours.usda"), usda::write(&Cage::of(&p, 0).unwrap(), "m", glam::Mat4::IDENTITY, &[], "note")).unwrap();
    req("ours.usda", Geometry::Usda).check_paths().unwrap();
    // A link appears after the check: the write refuses it.
    std::fs::write(dir.join("target.txt"), b"my notes").unwrap();
    std::os::unix::fs::symlink(dir.join("target.txt"), dir.join("late.png")).unwrap();
    let e = crate::outputs::write_over(&dir.join("late.png"), "Png", |_| true, b"\x89PNG data").unwrap_err();
    assert!(e.contains("not a regular file") || e.contains("link"), "{e}");
    assert_eq!(std::fs::read(dir.join("target.txt")).unwrap(), b"my notes");
    crate::outputs::write_over(&dir.join("fresh.png"), "Png", |_| false, b"\x89PNG data").unwrap();
    crate::outputs::write_over(&dir.join("fresh.png"), "Png", |h| h.starts_with(b"\x89PNG"), b"\x89PNG").unwrap();
    assert_eq!(std::fs::read(dir.join("fresh.png")).unwrap(), b"\x89PNG", "replaced, cut to the new length");
    // Lone carriage returns and other controls stay inside comments.
    let text = String::from_utf8(obj(&Cage::of(&p, 0).unwrap(), "x\rv 7 7 7", &["note\rv 7 7 7\u{b}f 1 2 3".into()])).unwrap();
    for line in text.split(['\n', '\r']) {
        assert!(!line.starts_with("v 7") && !line.starts_with("f 1 2 3") && !line.contains(char::is_control), "an injected line: {line:?}");
    }
    let u = String::from_utf8(usda::write(&Cage::of(&p, 0).unwrap(), "m", glam::Mat4::IDENTITY, &[], "a\rdef Xform \"evil\"\u{0}")).unwrap();
    assert!(!u.split(['\n', '\r']).any(|l| l.starts_with("def Xform \"evil")), "{u}");
    usda::read(u.as_bytes()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review (3d, item 10): every reader refuses an edge creased twice
/// (`Cage::to_poly`, as the OBJ reader), the USDA checker bounds its
/// bracket nesting, and a .glb's reply and sidecar count the creases it
/// holds (its cage's).
#[test]
fn duplicate_creases_nesting_and_crease_counts() {
    let p = mixed();
    let mut c = Cage::of(&p, 0).unwrap();
    c.creases.push(c.creases[0]);
    assert!(c.to_poly().unwrap_err().contains("twice"));
    let text = String::from_utf8(usda::write(&c, "m", glam::Mat4::IDENTITY, &[], "")).unwrap();
    assert!(usda::read(text.as_bytes()).unwrap().to_poly().unwrap_err().contains("twice"));
    let deep = format!("{}\n{}{}", usda::HEADER, "(".repeat(100), ")".repeat(100));
    assert!(usda::read(deep.as_bytes()).unwrap_err().contains("nest"));
    // A .glb at level 1 holds the cage's 4 creases (level 1 has fewer).
    let dir = std::env::temp_dir().join(format!("clide3d_review3d_creases_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut q = mixed();
    q.set_levels(1).unwrap();
    let req = Request { path: dir.join("m.glb"), geometry: Geometry::Glb, level: 1, maps: vec![], map_level: 1, map_size: 512, vector_format: crate::outputs::Format::Exr, height_format: crate::outputs::Format::Png, space: Space::Tangent, verify: false, udim: None };
    req.check_paths().unwrap();
    let reply = export(&q, "m", glam::Mat4::IDENTITY, &crate::material::Material::default(), &req, None, &Default::default()).unwrap();
    let held = check_glb(&std::fs::read(&req.path).unwrap(), &Default::default()).unwrap().cage.unwrap().creases.len();
    let side: serde_json::Value = serde_json::from_slice(&std::fs::read(req.sidecar()).unwrap()).unwrap();
    assert_eq!(held, q.creases.len());
    assert_eq!(reply["subdivision"]["creases"].as_u64(), Some(held as u64), "{reply}");
    assert_eq!(side["subdivision"]["creases"].as_u64(), Some(held as u64));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Security review fix: tile textures a sidecar lists are read only when
/// they are regular files no larger than their resolution allows (a link
/// or an oversized file is refused before it is read), and a tile listed
/// twice is refused.
#[test]
fn tile_reads_are_bounded() {
    use crate::poly::texture::{Channel, Textures};
    let dir = std::env::temp_dir().join(format!("clide3d_tile_reads_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let geometry = dir.join("t.obj");
    let file = texture_file(&geometry, Channel::Color, 1001);
    let mut t = Textures::default();
    t.set_resolution(Channel::Color, 256).unwrap();
    *t.texel_mut(Channel::Color, 1001, 1, 2) = [1, 2, 3, 4];
    let png = crate::outputs::png16(256, 256, 4, bytemuck::cast_slice(&t.tile_texels(Channel::Color, 1001).unwrap())).unwrap();
    std::fs::write(&file, &png).unwrap();
    let side = |tiles: serde_json::Value| {
        let v = serde_json::json!({ "generator": GENERATOR, "udim": { "textures": { "color": { "resolution": 256, "tiles": tiles, "files": { "1001": "t.color.1001.png" } } } } });
        std::fs::write(geometry.with_extension("json"), serde_json::to_vec(&v).unwrap()).unwrap();
    };
    side(serde_json::json!([1001]));
    let p = crate::progress::Progress::default();
    assert_eq!(read_textures(&geometry, &p).unwrap().unwrap().hash(), t.hash());
    side(serde_json::json!([1001, 1001]));
    assert!(read_textures(&geometry, &p).err().unwrap().contains("twice"));
    side(serde_json::json!([1001]));
    // Oversized: refused from its length.
    std::fs::write(&file, vec![0u8; tile_file_limit(256) as usize + 1]).unwrap();
    let e = read_textures(&geometry, &p).err().unwrap();
    assert!(e.contains("more than"), "{e}");
    // A link: refused, not followed.
    std::fs::remove_file(&file).unwrap();
    let target = dir.join("elsewhere.png");
    std::fs::write(&target, &png).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, &file).unwrap();
        let e = read_textures(&geometry, &p).err().unwrap();
        assert!(e.contains("regular file"), "{e}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
