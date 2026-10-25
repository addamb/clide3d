use std::collections::HashMap;

use glam::Vec3;
use serde_json::json;

use super::*;
use crate::harness::{figure, geom, sphere};

fn clay(v: serde_json::Value) -> Clay {
    serde_json::from_value(v).expect("clay")
}

fn torus() -> Clay {
    clay(json!({ "resolution": 96, "blobs": [{ "type": "torus", "center": [0, 0, 0], "radius": 0.4, "thickness": 0.12, "rotate": [30, 0, 10] }] }))
}

fn human() -> Clay {
    clay(json!({ "resolution": 96, "blobs": [{ "type": "human" }] }))
}

/// A triangle as its corners' bits, turned so its smallest corner leads
/// (winding kept).
fn tri_key(c: [Vec3; 3]) -> [[u32; 3]; 3] {
    let k = c.map(|p| p.to_array().map(f32::to_bits));
    let first = (0..3).min_by_key(|&i| k[i]).expect("three");
    [k[first], k[(first + 1) % 3], k[(first + 2) % 3]]
}

fn triangles(c: &ChunkMesh) -> Vec<[[u32; 3]; 3]> {
    c.indices.chunks_exact(3).map(|t| tri_key([c.positions[t[0] as usize], c.positions[t[1] as usize], c.positions[t[2] as usize]])).collect()
}

/// Edges of a welded mesh used once (cracks) and more than twice.
fn open_edges(m: &Mesh) -> (usize, usize) {
    let mut count: HashMap<(u32, u32), usize> = HashMap::new();
    for t in m.indices.chunks_exact(3) {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *count.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    (count.values().filter(|c| **c == 1).count(), count.values().filter(|c| **c > 2).count())
}

fn whole_mesh(c: &ChunkMesh, palette: &[Material]) -> Mesh {
    Mesh { vertices: (0..c.positions.len()).map(|i| vertex(c, i, palette, None)).collect(), indices: c.indices.clone(), materials: palette.to_vec(), ..Mesh::default() }
}

/// Chunked against the monolithic anchored reference: the same triangles
/// (bit for bit, as sets), the same vertices once welded, no cracks.
fn seams_agree(name: &str, clay: &Clay) -> (ChunkSet, ChunkMesh) {
    let palette = clay.palette(&[], "");
    let mesher = Mesher::new(clay, &palette).expect("eligible");
    let set = ChunkSet::build(&mesher);
    let whole = mesher.mesh_whole();
    let mut a: Vec<_> = set.chunks.values().flat_map(|c| triangles(c)).collect();
    let mut b = triangles(&whole);
    assert_eq!(a.len(), b.len(), "{name}: triangle counts");
    a.sort_unstable();
    b.sort_unstable();
    assert!(a == b, "{name}: the chunks' triangles differ from the monolithic mesh's");
    let welded = set.assemble(&palette, None);
    let key = |v: &Vertex| (v.pos.map(f32::to_bits), v.normal.map(f32::to_bits), v.color.map(f32::to_bits));
    let mut va: Vec<_> = welded.vertices.iter().map(key).collect();
    let mut vb: Vec<_> = (0..whole.positions.len()).map(|i| key(&vertex(&whole, i, &palette, None))).collect();
    va.sort_unstable();
    vb.sort_unstable();
    assert!(va == vb, "{name}: the welded chunk vertices differ from the monolithic mesh's ({} against {})", va.len(), vb.len());
    let (cracks, crowded) = open_edges(&welded);
    eprintln!("{name}: {} chunks, {} triangles, {} cracks, {} edges in more than two triangles, cell {}", set.chunks.len(), a.len(), cracks, crowded, mesher.voxel);
    assert_eq!(cracks, 0, "{name}: crack edges");
    (set, whole)
}

#[test]
fn chunks_match_the_monolithic_anchored_mesh() {
    seams_agree("sphere", &sphere(0.5, 128));
    seams_agree("torus", &torus());
    seams_agree("organic figure", &figure(96, false));
    // Box feet: sharp features (and the hard-surface resolution floor).
    let hard = figure(96, true);
    assert!(hard.sharp_features());
    seams_agree("hard-floor figure", &hard);
    seams_agree("human", &human());
}

#[test]
fn the_anchored_reference_meets_the_stage_0_sphere_bounds() {
    let radius = 0.5;
    let c = sphere(radius, 128);
    let cell = c.cell_size().unwrap();
    let palette = c.palette(&[], "");
    let m = Mesher::new(&c, &palette).unwrap();
    let mesh = whole_mesh(&m.mesh_whole(), &palette);
    let e = geom::sdf_error(&mesh, |p| p.length() - radius, &geom::sphere_points(Vec3::ZERO, radius, 20_000));
    let truth = 4.0 / 3.0 * std::f64::consts::PI * f64::from(radius).powi(3);
    let volume = (geom::volume(&mesh) - truth).abs() / truth;
    let normals = geom::normal_error(&mesh, |p| p);
    eprintln!("anchored sphere res 128: hausdorff {:.3} cell, volume error {:.4}%, normals mean {:.2} max {:.2} deg", e.hausdorff() / cell, volume * 100.0, normals.mean_deg, normals.max_deg);
    assert!(e.hausdorff() <= 0.6 * cell);
    assert!(volume <= 0.01);
    assert!(normals.max_deg <= 5.0, "{normals:?}");
}

/// Same pieces as the plain cell rule where a cell holds one sheet; two
/// sheets through one cell (a saddle at the crease of two blobs a cell
/// apart) get a vertex each, and the welded mesh has no edge in more
/// than two triangles there.
#[test]
fn saddles_resolve_inside_their_cells() {
    // The organic figure's arm-torso creases fall back whole in the
    // regular mesher at res 80; here they stay surface nets.
    let c = figure(80, false);
    let palette = c.palette(&[], "");
    let m = Mesher::new(&c, &palette).unwrap();
    let set = ChunkSet::build(&m);
    let (cracks, crowded) = open_edges(&set.assemble(&palette, None));
    assert_eq!((cracks, crowded), (0, 0));
}

#[test]
fn a_dab_then_a_dirty_only_remesh_equals_a_full_rebuild() {
    let mut c = figure(96, false);
    c.set_detail_cell(0.004).unwrap();
    let palette = c.palette(&[], "");
    let s: crate::sculpt::Stroke = serde_json::from_value(json!({"brush": "inflate", "points": [[0.0, 1.25, 0.18]], "radius": 0.05})).unwrap();
    crate::sculpt::apply(&mut c, &s).unwrap();
    let before = c.clone();
    let set0 = ChunkSet::build(&Mesher::new(&before, &palette).unwrap());
    let s: crate::sculpt::Stroke = serde_json::from_value(json!({"brush": "standard", "points": [[0.02, 1.27, 0.18], [0.06, 1.3, 0.17]], "radius": 0.04, "strength": 0.6})).unwrap();
    crate::sculpt::apply(&mut c, &s).unwrap();
    let mesher = Mesher::new(&c, &palette).unwrap();
    let dirty = dirty_detail(before.detail.as_ref(), c.detail.as_ref(), mesher.voxel).expect("same layout");
    assert!(!dirty.is_empty() && dirty.len() < set0.chunks.len() / 4, "{} dirty of {}", dirty.len(), set0.chunks.len());
    let partial = set0.rebuilt(&mesher, &dirty);
    let full = ChunkSet::build(&mesher);
    assert_eq!(partial.chunks.keys().collect::<Vec<_>>(), full.chunks.keys().collect::<Vec<_>>());
    for (k, a) in &full.chunks {
        let b = &partial.chunks[k];
        let same = a.positions == b.positions && a.normals == b.normals && a.indices == b.indices && a.base == b.base && a.mats == b.mats;
        assert!(same, "chunk {k:?} differs");
    }
    // A smaller margin misses chunks whose meshes did change.
    let changed: Vec<ChunkKey> = full.chunks.iter().filter(|(k, a)| set0.chunks.get(*k).is_none_or(|b| b.positions != a.positions || b.normals != a.normals)).map(|(k, _)| *k).collect();
    assert!(changed.iter().all(|k| dirty.contains(k)));
    eprintln!("dab: {} dirty chunks, {} changed, of {}", dirty.len(), changed.len(), full.chunks.len());
}

#[test]
fn chunks_are_the_same_on_one_thread_and_many() {
    let mut c = figure(96, true);
    c.set_detail_cell(0.006).unwrap();
    let palette = c.palette(&[], "");
    let m = Mesher::new(&c, &palette).unwrap();
    let many = ChunkSet::build(&m);
    let one = crate::clay::with_threads(1, || ChunkSet::build(&m));
    assert_eq!(many.chunks.len(), one.chunks.len());
    for (k, a) in &many.chunks {
        let b = &one.chunks[k];
        assert!(a.positions == b.positions && a.indices == b.indices && a.normals == b.normals, "{k:?}");
    }
}

#[test]
fn clays_that_need_the_whole_mesh_are_not_chunked() {
    let b = clay(json!({ "blobs": [{ "type": "box", "center": [0, 0, 0], "size": [1, 1, 1] }] }));
    assert!(eligible(&b, &b.palette(&[], "")).is_err());
    let mut d = sphere(0.5, 64);
    d.decals.push(serde_json::from_value(json!({ "center": [0, 0, 0.5], "normal": [0, 0, 1], "size": 0.1 })).unwrap());
    assert!(eligible(&d, &d.palette(&[], "")).is_err());
    // Far-out clay is refused before any lattice math.
    let far = clay(json!({ "blobs": [{ "type": "sphere", "center": [3.0e9, 0, 0], "radius": 0.5 }] }));
    assert!(Mesher::new(&far, &far.palette(&[], "")).is_err());
}

#[test]
fn pick_field_meets_the_chunk_mesh_where_rays_hit_it() {
    let mut c = figure(96, false);
    c.set_detail_cell(0.005).unwrap();
    let s: crate::sculpt::Stroke = serde_json::from_value(json!({"brush": "inflate", "points": [[0.0, 1.25, 0.18]], "radius": 0.05})).unwrap();
    crate::sculpt::apply(&mut c, &s).unwrap();
    let palette = c.palette(&[], "");
    let m = Mesher::new(&c, &palette).unwrap();
    let mesh = ChunkSet::build(&m).assemble(&palette, None);
    let bvh = crate::mesh::MeshBvh::new(&mesh);
    let mut rng = 0x1234_5678_9abc_def1u64;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 40) as f32 / (1u64 << 24) as f32
    };
    let (mut both, mut worst, mut differ) = (0, 0.0f32, 0);
    for _ in 0..500 {
        let origin = Vec3::new(next() * 2.0 - 1.0, 0.2 + next() * 1.6, 2.0);
        let target = Vec3::new(next() * 0.5 - 0.25, 0.2 + next() * 1.6, 0.0);
        let dir = target - origin;
        match (m.pick(origin, dir), bvh.hit(&mesh, origin, dir)) {
            (Some((t, n)), Some(h)) => {
                both += 1;
                worst = worst.max((t - h.t).abs() * dir.length() / m.voxel);
                assert!(n.dot(h.normal) > 0.5, "normals disagree");
            }
            (None, None) => {}
            _ => differ += 1,
        }
    }
    eprintln!("pick_field against ray_hit: {both} hits, worst |dt| {worst:.3} cell, {differ} of 500 disagree on hitting (silhouettes)");
    assert!(both > 150 && worst <= 0.5, "{both} hits, worst {worst} cells");
    assert!(differ <= 5);
}
