//! Stage 4b review fixes, each with its regression test.

use std::sync::Arc;

use glam::Vec3;
use serde_json::json;

use super::paint::{self, PolyPaint};
use super::tests::{plates, sphere_udim, strokes};
use super::*;
use crate::poly::Poly;

/// UVs per corner from a function of the corner's point.
fn with_uv(mut p: Poly, f: impl Fn(Vec3) -> [f32; 2]) -> Poly {
    let points = p.cage.to_vec();
    let (_, corners) = p.cage_faces();
    let uv: Vec<[f32; 2]> = corners.iter().map(|&c| f(points[c as usize])).collect();
    p.uv = Some(crate::poly::Chunked::new(&uv));
    p
}

fn paint_at(p: &Poly, resolution: u32, raws: &[serde_json::Value]) -> Textures {
    let surface = paint::Surface::new(p, p.level).unwrap();
    let out = paint::paint_poly(p, &surface, Channel::Color, resolution, None, false, strokes(raws)).unwrap().poly;
    out.paint.as_ref().map(|p| p.textures.clone()).unwrap_or_default()
}

/// 1: an ordinary OBJ or USDA whose UVs leave 0-1 (tiling, or float
/// noise past 1) reads as plain v up; only a file export_poly marks (or
/// an import that asks) reads tile by tile.
#[test]
fn only_marked_files_read_as_udim() {
    let obj = |mark: bool| {
        let mut s = String::new();
        if mark {
            s.push_str(&format!("# {}\n", crate::poly::uv::UDIM_MARK));
        }
        s.push_str("v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0.9\nvt 1 1.1\nvt 1 1.0000001\nvt 0 0.25\nf 1/1 2/2 3/3 4/4\n");
        s.into_bytes()
    };
    let (plain, _) = crate::mesh_import::read_cage(&obj(false)).unwrap();
    let uv = plain.uv.unwrap();
    let want = [0.1f32, -0.1, -1.0000001e0 + 1.0, 0.75];
    for (u, w) in uv.iter().zip(want) {
        assert!((u[1] - w).abs() < 1e-6, "plain 1 - v: {uv:?}");
    }
    // Neighbouring corners stay neighbours (0.9 and 1.1 are 0.2 apart).
    assert!((uv[0][1] - uv[1][1]).abs() < 0.2 + 1e-6);
    let (marked, _) = crate::mesh_import::read_cage(&obj(true)).unwrap();
    let m = marked.uv.unwrap();
    assert_eq!(m[1][1], crate::poly::uv::v_in(1.1, true));
    let (forced, _) = crate::mesh_import::read_cage_as(&obj(false), Some(true)).unwrap();
    assert_eq!(forced.uv.unwrap(), m);
    let (off, _) = crate::mesh_import::read_cage_as(&obj(true), Some(false)).unwrap();
    assert_eq!(off.uv.unwrap(), uv);
    // The writers mark a UDIM layout, and their files read back exactly.
    let cage = crate::poly::export::Cage { points: vec![Vec3::ZERO, Vec3::X, Vec3::ONE, Vec3::Y], face_start: vec![0, 4], corners: vec![0, 1, 2, 3], uv: Some(vec![[1.2, 0.3], [1.9, 0.3], [1.9, 0.9], [1.2, 1.6]]), creases: Vec::new() };
    let o = crate::poly::export::obj(&cage, "q", &[]);
    assert!(String::from_utf8_lossy(&o).contains(crate::poly::uv::UDIM_MARK));
    assert_eq!(crate::mesh_import::read_cage(&o).unwrap().0.uv, cage.uv);
    let u = crate::poly::export::usda::write(&cage, "q", glam::Mat4::IDENTITY, &[], "");
    assert!(String::from_utf8_lossy(&u).contains(crate::poly::uv::UDIM_MARK));
    assert_eq!(crate::poly::export::usda::read(&u).unwrap().uv, cage.uv);
    // A USDA without the mark reads plainly.
    let unmarked = String::from_utf8(u).unwrap().replace(&format!("            doc = \"{}\"\n", crate::poly::uv::UDIM_MARK), "");
    let plain = crate::poly::export::usda::read(unmarked.as_bytes()).unwrap().uv.unwrap();
    assert!((plain[3][1] - (1.0 - crate::poly::uv::v_out(1.6, true) as f32)).abs() < 1e-6, "{plain:?}");
    // One tile's file has no mark.
    let one = crate::poly::export::Cage { uv: Some(vec![[0.2, 0.3], [0.9, 0.3], [0.9, 0.9], [0.2, 0.6]]), ..cage };
    assert!(!String::from_utf8_lossy(&crate::poly::export::obj(&one, "q", &[])).contains(crate::poly::uv::UDIM_MARK));
}

/// 7: `v_out` and `v_in` are exact inverses on every row, below the first
/// too.
#[test]
fn v_out_and_v_in_are_inverses_on_every_row() {
    for v in [-3.5f32, -2.0, -1.0, -0.999, -0.25, 0.0, 0.3, 0.999, 1.0, 1.7, 2.0, 9.99] {
        assert_eq!(crate::poly::uv::v_in(crate::poly::uv::v_out(v, true), true), v, "{v}");
        assert_eq!(crate::poly::uv::v_in(crate::poly::uv::v_out(v, false), false), v, "{v}");
    }
}

/// 2: a texel inside one chart never takes another chart's gutter paint,
/// even when that chart is far away in 3D (only faces near the dab were
/// asked before).
#[test]
fn gutters_do_not_bleed_into_other_charts() {
    // Two plates 2 apart in z whose charts touch at u = 0.5 in the tile.
    let p = with_uv(plates(2, 1.0, 0.3), |q| {
        let k = (q.z / 2.0).round();
        [0.5 * k + q.x * 0.5, q.y / 0.3 * 0.25]
    });
    let t = paint_at(&p, 256, &[json!({ "point": [0.95, 0.15, 0.0], "radius": 0.2, "color": [1, 0, 0], "hardness": 0.9 })]);
    let row = 30;
    // Chart 0 ends at texel 128: its own last texel is painted, chart 1's
    // texels next to it are not.
    assert!(t.texel(Channel::Color, 1001, 127, row)[3] > 60000);
    for x in 128..133 {
        assert_eq!(t.texel(Channel::Color, 1001, x, row), [0; 4], "texel {x} is chart 1's");
    }
    // Where no chart is, the gutter is still painted (bilinear reads at
    // the border find paint).
    let below = paint_at(&p, 256, &[json!({ "point": [0.5, 0.3, 0.0], "radius": 0.2, "color": [1, 0, 0], "hardness": 0.9 })]);
    assert!(below.texel(Channel::Color, 1001, 64, 64)[3] > 0, "the gutter past the chart's top edge");
}

/// 5: a thin part's far side is left alone: the way the surface under a
/// dab turns comes from its nearest surface point, not from the face
/// whose box middle is nearest.
#[test]
fn thin_parts_paint_the_near_side() {
    // A big top face (+z) over a 4 x 2 grid of small faces 2 mm below
    // facing -z.
    let mut points = vec![Vec3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0), Vec3::new(1.0, 0.3, 0.0), Vec3::new(0.0, 0.3, 0.0)];
    let mut faces = vec![vec![0u32, 1, 2, 3]];
    for j in 0..=2 {
        for i in 0..=4 {
            points.push(Vec3::new(i as f32 / 4.0, 0.3 * j as f32 / 2.0, -0.002));
        }
    }
    for j in 0..2u32 {
        for i in 0..4u32 {
            let a = 4 + j * 5 + i;
            faces.push(vec![a, a + 5, a + 6, a + 1]);
        }
    }
    let p = with_uv(Poly::from_faces(&points, &faces).unwrap(), |q| if q.z == 0.0 { [q.x * 0.45, q.y / 0.3 * 0.4] } else { [0.55 + q.x * 0.45, q.y / 0.3 * 0.4] });
    let t = paint_at(&p, 256, &[json!({ "point": [0.03, 0.03, 0.0], "radius": 0.02, "color": [0, 1, 0], "hardness": 0.9 })]);
    let texels = t.tile_texels(Channel::Color, 1001).unwrap();
    let (mut top, mut bottom) = (0, 0);
    for (i, v) in texels.iter().enumerate() {
        if v[3] > 0 {
            if i % 256 < 128 { top += 1 } else { bottom += 1 }
        }
    }
    eprintln!("thin part: {top} texels on the near side, {bottom} on the far side");
    assert!(top > 10 && bottom == 0, "{top} {bottom}");
}

/// 3: an erase makes no page where nothing is, an erase that empties a
/// page drops it, and an all-zero page hashes as no page.
#[test]
fn erase_makes_and_keeps_no_empty_pages() {
    let p = sphere_udim();
    let t = paint_at(&p, 512, &[json!({ "point": [0.0, 0.0, 0.5], "radius": 0.08, "color": [1, 0, 0] })]);
    let pages = t.pages();
    assert!(pages > 0);
    let surface = paint::Surface::new(&p, p.level).unwrap();
    let mut base = p.clone();
    base.paint = Some(Arc::new(PolyPaint { textures: t.clone(), records: Vec::new(), imported: true }));
    let erase = |raws: &[serde_json::Value]| paint::paint_poly(&base, &surface, Channel::Color, 512, None, false, strokes(raws)).unwrap().poly.paint.map(|p| p.textures.clone()).unwrap_or_default();
    // Far from the paint: nothing made.
    let far = erase(&[json!({ "point": [0.0, 0.0, -0.5], "radius": 0.2, "erase": true })]);
    assert_eq!(far.pages(), pages);
    assert_eq!(far.hash(), t.hash());
    // All of it, fully: every page dropped.
    let all = erase(&[json!({ "point": [0.0, 0.0, 0.5], "radius": 0.3, "erase": true, "hardness": 1.0 })]);
    eprintln!("erase: {pages} pages painted, {} after erasing all of it", all.pages());
    assert_eq!(all.pages(), 0);
    assert!(all.tiles(Channel::Color).is_empty());
    // A zero page is the same texels as none.
    let mut z = Textures::default();
    z.set_resolution(Channel::Color, 512).unwrap();
    let empty = z.hash();
    *z.texel_mut(Channel::Color, 1001, 5, 5) = [0; 4];
    assert_eq!(z.hash(), empty);
}

/// 4: a dab is bounded from the layout before anything is allocated, and
/// the path index finds the same weights as a full scan.
#[test]
fn dabs_are_bounded_before_allocating_and_indexed() {
    let p = with_uv(plates(1, 1.0, 1.0), |q| [q.x * 0.999, q.y * 0.999]);
    let surface = paint::Surface::new(&p, p.level).unwrap();
    let s = strokes(&[json!({ "point": [0.5, 0.5, 0.0], "radius": 2.0, "color": [1, 0, 0] })]);
    let curve = s[0].paint.clone().unwrap().curve;
    let ink = paint::Ink { channel: Channel::Color, resolution: 8192, value: [1.0, 0.0, 0.0], erase: false, opacity: 1.0 };
    let mut t = Textures::default();
    let mut visited = 0;
    let timer = std::time::Instant::now();
    let e = paint::apply(&mut t, &surface, &ink, &curve, &s[0].dabs, &mut visited).err().unwrap();
    eprintln!("{e} ({:.2} ms)", timer.elapsed().as_secs_f64() * 1e3);
    assert!(e.contains("rasterize") && t.pages() == 0, "{e}");
    // The index: every weight bit for bit as the whole path gives it.
    let path: Vec<Vec3> = (0..60).map(|k| Vec3::new((k as f32 * 0.37).sin(), k as f32 * 0.02, (k as f32 * 0.23).cos())).collect();
    let radii: Vec<f32> = (0..60).map(|k| 0.05 + 0.03 * (k % 5) as f32).collect();
    let gains: Vec<f32> = (0..60).map(|k| 0.5 + 0.01 * k as f32).collect();
    for (radii, gains) in [(None, None), (Some(radii.clone()), Some(&gains[..]))] {
        let sweep = crate::sculpt::engine::Sweep { points: &path, radius: 0.1, radii, gains };
        let index = paint::PathIndex::new(&path, sweep.reach());
        for k in 0..4000 {
            let f = k as f32;
            let q = Vec3::new((f * 0.731).sin() * 1.1, (f * 0.017) % 1.3, (f * 0.413).cos() * 1.1);
            assert_eq!(sweep.weight(&curve, q).to_bits(), sweep.weight_among(&curve, q, &index.segments_near(q)).to_bits(), "{q}");
            let full = path.iter().enumerate().map(|(i, p)| (p.distance_squared(q), i)).min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1))).unwrap().1;
            assert_eq!(index.nearest(q, &path), full);
        }
    }
}

/// 6: a new unwrap whose layout cannot hold a stroke leaves that stroke
/// out (named) and keeps the rest; a cancel stops the replay.
#[test]
fn replay_on_a_new_layout_keeps_what_fits() {
    let small = with_uv(plates(1, 1.0, 0.5), |q| [q.x * 0.1, q.y * 0.1]);
    let surface = paint::Surface::new(&small, small.level).unwrap();
    let painted = paint::paint_poly(&small, &surface, Channel::Color, 8192, None, false, strokes(&[
        json!({ "point": [0.5, 0.25, 0.0], "radius": 2.0, "color": [1, 0, 0] }),
        json!({ "point": [0.05, 0.05, 0.0], "radius": 0.04, "color": [0, 0, 1] }),
    ])).unwrap().poly;
    let records = painted.paint.as_ref().unwrap().records.clone();
    let big = with_uv(painted.clone(), |q| [q.x * 0.999, q.y * 0.999]);
    let progress = crate::progress::Progress::default();
    assert!(paint::replay(&big, &records).is_err(), "the strict replay refuses");
    let (t, kept, notes) = paint::replay_kept(&big, &records, &progress).unwrap();
    eprintln!("replay on a bigger layout: kept {:?}, {notes:?}", kept.iter().map(|r| r.id).collect::<Vec<_>>());
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].id, records[1].id);
    assert!(notes.len() == 1 && notes[0].contains("left out"));
    assert!(t.pages() > 0);
    let cancelled = crate::progress::Progress::default();
    cancelled.cancel();
    assert!(paint::replay_kept(&big, &records, &cancelled).is_err());
}

/// 8: a cage face whose first corner sits on a tile's edge is baked on
/// the tile its middle is on (as the bake's simple mesh takes it).
#[test]
fn bake_tiles_by_the_face_middle() {
    // The plate's right edge at u = 1.0 exactly: the faces there have a
    // corner on tile 1002's left edge.
    let p = with_uv(plates(1, 1.0, 0.5), |q| [q.x, q.y]);
    let (tiles, _) = bake_estimate(&p, 512, 4.0);
    assert_eq!(tiles.keys().copied().collect::<Vec<_>>(), vec![1001]);
    assert_eq!(tiles[&1001].len(), 16, "every face's two triangles");
}

/// 10: a file's paint records keep one resolution per channel, that of
/// its texels.
#[test]
fn paint_files_keep_one_resolution_per_channel() {
    let p = sphere_udim();
    let surface = paint::Surface::new(&p, p.level).unwrap();
    let painted = paint::paint_poly(&p, &surface, Channel::Color, 512, None, false, strokes(&[
        json!({ "point": [0.0, 0.0, 0.5], "radius": 0.05, "color": [1, 0, 0] }),
        json!({ "point": [0.0, 0.5, 0.0], "radius": 0.05, "color": [0, 1, 0] }),
    ])).unwrap().poly;
    let good = serde_json::to_value(&painted).unwrap();
    assert!(serde_json::from_value::<Poly>(good.clone()).is_ok());
    let mut v = good.clone();
    let mut r: paint::Record = serde_json::from_value(v["paint"]["records"][1].clone()).unwrap();
    r.resolution = 1024;
    r.digest = r.digest();
    v["paint"]["records"][1] = serde_json::to_value(&r).unwrap();
    let e = serde_json::from_value::<Poly>(v).err().map(|e| e.to_string()).unwrap_or_default();
    assert!(e.contains("texels a tile"), "{e}");
}

/// 10: a sidecar listing an empty tile twice is refused like any other.
#[test]
fn sidecar_lists_each_tile_once_even_empty() {
    use crate::poly::export::{GENERATOR, read_textures, texture_file};
    let dir = std::env::temp_dir().join(format!("clide3d_empty_twice_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let geometry = dir.join("t.obj");
    let png = crate::outputs::png16(256, 256, 4, &vec![0u16; 256 * 256 * 4]).unwrap();
    std::fs::write(texture_file(&geometry, Channel::Color, 1001), &png).unwrap();
    let v = json!({ "generator": GENERATOR, "udim": { "textures": { "color": { "resolution": 256, "tiles": [1001, 1001], "files": { "1001": "t.color.1001.png" } } } } });
    std::fs::write(geometry.with_extension("json"), serde_json::to_vec(&v).unwrap()).unwrap();
    let e = read_textures(&geometry, &crate::progress::Progress::default()).err().unwrap();
    assert!(e.contains("twice"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Security review: the texel-ownership scan is bounded. A layout of
/// thousands of overlapping charts (every texel inside thousands of UV
/// triangles) is refused within the call's visit budget, quickly, with
/// nothing written; and a cancelled job stops before its first triangle.
#[test]
fn ownership_scans_are_bounded_and_cancellable() {
    // Plate 0's chart on its own at u 0.55-1; 12,000 more plates whose
    // charts lie over each other in a sliver just left of it (u 0.5486 to
    // 0.5492, between texel centres at 1024), so every gutter texel on
    // plate 0's left edge is tested against their 192,000 triangles and
    // none holds it.
    let p = with_uv(plates(12001, 1.0, 0.3), |q| if q.z < 1.0 { [0.55 + q.x * 0.45, q.y / 0.3 * 0.25] } else { [0.5486 + q.x * 0.0006, q.y / 0.3 * 0.25] });
    let surface = paint::Surface::new(&p, p.level).unwrap();
    let s = strokes(&[json!({ "point": [0.5, 0.15, 0.0], "radius": 0.6, "color": [1, 0, 0] })]);
    let curve = s[0].paint.clone().unwrap().curve;
    let ink = paint::Ink { channel: Channel::Color, resolution: 1024, value: [1.0, 0.0, 0.0], erase: false, opacity: 1.0 };
    let mut t = Textures::default();
    let mut visited = 0;
    let timer = std::time::Instant::now();
    let r = paint::apply_within(&mut t, &surface, &ink, &curve, &s[0].dabs, &mut visited, MAX_PAGES, None);
    let ms = timer.elapsed().as_secs_f64() * 1e3;
    eprintln!("overlapping charts: {:?} after {visited} visits in {ms:.0} ms", r.as_ref().map_err(|e| e.as_str()));
    assert!(r.is_err_and(|e| e.contains("texels")), "refused by the budget");
    assert_eq!(t.pages(), 0, "nothing written");
    assert!(visited <= paint::MAX_VISITED + paint::MAX_DAB_TEXELS);
    // Cancelled: stops at once.
    let cancelled = crate::progress::Progress::default();
    cancelled.cancel();
    let mut visited = 0;
    let p1 = with_uv(plates(1, 1.0, 0.3), |q| [q.x * 0.5, q.y / 0.3 * 0.25]);
    let one = paint::Surface::new(&p1, p1.level).unwrap();
    assert!(paint::apply_within(&mut t, &one, &ink, &curve, &s[0].dabs, &mut visited, MAX_PAGES, Some(&cancelled)).is_err());
}

/// Security review: a UDIM bake counts the pages of exactly the tiles and
/// triangles it bakes (the level's, by their UV middle), refusing past the
/// budget before any bake.
#[test]
fn bake_plans_exactly_what_it_bakes() {
    let p = sphere_udim();
    let progress = crate::progress::Progress::default();
    let low = crate::poly::low::Low::new_udim(&p, 2, glam::Mat4::IDENTITY, &crate::mesh::Mesh::default(), &progress).unwrap();
    let plan = low.bake_plan(2048, 4.0, BAKED.len(), 0, MAX_PAGES, &progress).ok().unwrap();
    let tiles: Vec<u16> = plan.iter().map(|x| x.0).collect();
    assert_eq!(tiles, low.tiles(), "every tile the level is on");
    let pages: usize = plan.iter().map(|x| x.2.len() * BAKED.len()).sum();
    let triangles: usize = plan.iter().map(|x| x.1.mesh.indices.len() / 3).sum();
    eprintln!("bake plan: tiles {tiles:?}, {pages} pages, {triangles} of {} triangles", low.mesh.indices.len() / 3);
    assert_eq!(triangles, low.mesh.indices.len() / 3, "no triangle left out");
    // One page under the budget: refused, naming the count.
    match low.bake_plan(2048, 4.0, BAKED.len(), 0, pages - 1, &progress) {
        Err(Ok(n)) => assert!(n > pages - 1),
        _ => panic!("refused past the budget"),
    }
    // Pages already held count too.
    assert!(matches!(low.bake_plan(2048, 4.0, BAKED.len(), MAX_PAGES - pages + 1, MAX_PAGES, &progress), Err(Ok(_))));
}
