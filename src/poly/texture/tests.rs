//! Stage 4b acceptance: UDIM layouts, tiles, bounds, save and load.

use std::sync::Arc;

use glam::Vec3;
use serde_json::json;

use super::paint::PolyPaint;
use super::*;
use crate::poly::Poly;
use crate::poly::uv::{Udim, chart_stats, unwrap_udim};

/// `n` flat rectangles `w` x `h` (each a 4 x 2 grid of quads), apart in
/// space: n charts.
pub(crate) fn plates(n: usize, w: f32, h: f32) -> Poly {
    let mut points = Vec::new();
    let mut faces = Vec::new();
    for k in 0..n {
        let base = points.len() as u32;
        let o = Vec3::new(0.0, 0.0, k as f32 * 2.0);
        for j in 0..=2 {
            for i in 0..=4 {
                points.push(o + Vec3::new(w * i as f32 / 4.0, h * j as f32 / 2.0, 0.0));
            }
        }
        for j in 0..2u32 {
            for i in 0..4u32 {
                let a = base + j * 5 + i;
                faces.push(vec![a, a + 1, a + 6, a + 5]);
            }
        }
    }
    Poly::from_faces(&points, &faces).unwrap()
}

/// The plan's `udim_layout`: 12 charts over 4 tiles; no chart crosses a
/// tile, and every chart's texel density is within 10% of the request.
#[test]
fn udim_layout() {
    // Each plate 1 x 0.3 at 480 texels per unit: 480 x 144 texels, three
    // to a 512 tile with padding.
    let p = plates(12, 1.0, 0.3);
    let made = unwrap_udim(&p, 512, 4, Udim { tiles: 4, texel_density: Some(480.0) }).unwrap();
    let stats = chart_stats(&p, &made.uv, &made.face_chart, 512);
    eprintln!("charts {} tiles {:?}; per chart {:?}", made.charts, made.tiles, stats.values().map(|s| (s.0, s.1.clone())).collect::<Vec<_>>());
    assert_eq!(made.charts, 12);
    assert_eq!(made.tiles, vec![1001, 1002, 1003, 1004]);
    for (density, tiles) in stats.values() {
        assert_eq!(tiles.len(), 1, "a chart on {tiles:?}");
        assert!((density / 480.0 - 1.0).abs() <= 0.1, "density {density}");
    }
    // Each face whole on one tile, and every UV inside its tile.
    let (starts, _) = p.cage_faces();
    for w in starts.windows(2) {
        let t: Vec<Option<u16>> = made.uv[w[0] as usize..w[1] as usize].iter().map(|u| tile_of(*u)).collect();
        assert!(t.iter().all(|x| x.is_some() && *x == t[0]), "{t:?}");
    }
    // Too dense for 4 tiles: refused, naming the fix.
    let e = unwrap_udim(&p, 512, 4, Udim { tiles: 4, texel_density: Some(1500.0) }).err().unwrap();
    assert!(e.contains("texel_density"), "{e}");
    // Without a density: one that fills about the tiles asked for.
    let made = unwrap_udim(&p, 512, 4, Udim { tiles: 6, texel_density: None }).unwrap();
    assert!(!made.tiles.is_empty() && made.tiles.len() <= 6, "{:?}", made.tiles);
}

/// A few texels written into two channels of a Poly with UVs.
pub(crate) fn painted(p: &Poly) -> Poly {
    let mut t = Textures::default();
    t.set_resolution(Channel::Color, 512).unwrap();
    t.set_resolution(Channel::Roughness, 256).unwrap();
    for (k, (x, y)) in [(3u32, 5u32), (300, 7), (511, 511)].into_iter().enumerate() {
        *t.texel_mut(Channel::Color, 1001 + k as u16, x, y) = [100 * k as u16, 2000, 65535, 40000];
    }
    *t.texel_mut(Channel::Roughness, 1012, 0, 255) = [9, 9, 9, 65535];
    let mut out = p.clone();
    out.paint = Some(Arc::new(PolyPaint { textures: t, records: Vec::new(), imported: true }));
    out
}

/// Tiles go through save and load bit for bit (the Poly's hash holds the
/// texels), pages are made only where written, and a clone shares them.
#[test]
fn tiles_save_and_load() {
    let mut p = plates(3, 1.0, 0.3);
    p.uv = Some(crate::poly::Chunked::new(&unwrap_udim(&p, 512, 4, Udim { tiles: 3, texel_density: Some(300.0) }).unwrap().uv));
    let q = painted(&p);
    let t = &q.paint.as_ref().unwrap().textures;
    assert_eq!(t.pages(), 4);
    assert_eq!(t.tiles(Channel::Color), vec![1001, 1002, 1003]);
    assert_eq!(t.texel(Channel::Color, 1002, 300, 7), [100, 2000, 65535, 40000]);
    assert_eq!(t.texel(Channel::Color, 1002, 301, 7), [0; 4]);
    assert_ne!(q.hash(), p.hash());
    let text = serde_json::to_string(&q).unwrap();
    let back: Poly = serde_json::from_str(&text).unwrap();
    assert_eq!(back.hash(), q.hash());
    assert!(back == q);
    // A clone shares every page: an undo snapshot of it costs nothing.
    assert_eq!(q.clone().unshared_bytes(Some(&q)), 0);
    let mut edited = q.clone();
    let mut paint = (**edited.paint.as_ref().unwrap()).clone();
    *paint.textures.texel_mut(Channel::Color, 1001, 3, 6) = [1, 1, 1, 1];
    edited.paint = Some(Arc::new(paint));
    assert_eq!(edited.unshared_bytes(Some(&q)), PAGE_BYTES as usize, "only the touched page");
}

/// The plan's `udim_bounds` (the tile and resolution parts; strokes'
/// in the MCP test): 101 tiles or a resolution of 16384 are refused from
/// the arguments, and damaged files of any size are refused, never a
/// panic, before a page is inflated.
#[test]
fn udim_bounds() {
    let p = plates(2, 1.0, 0.3);
    assert!(unwrap_udim(&p, 512, 4, Udim { tiles: 101, texel_density: None }).err().unwrap().contains("tiles"));
    assert!(check_resolution(16384).is_err() && check_resolution(300).is_err() && check_resolution(8192).is_ok());
    let mut t = Textures::default();
    assert!(t.set_resolution(Channel::Color, 16384).is_err());
    t.set_resolution(Channel::Color, 512).unwrap();
    *t.texel_mut(Channel::Color, 1001, 0, 0) = [1; 4];
    assert!(t.set_resolution(Channel::Color, 1024).err().unwrap().contains("512"), "a channel keeps its resolution");
    let good = serde_json::to_value(t.to_file().unwrap()).unwrap();
    let page = good["layers"][0]["tiles"][0]["pages"][0].clone();
    let with = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut v = good.clone();
        f(&mut v);
        serde_json::from_value::<TexturesFile>(v).map_err(|e| e.to_string()).and_then(TexturesFile::read)
    };
    assert!(with(&|_| {}).is_ok());
    for (name, bad) in [
        ("tile 1101", Box::new(|v: &mut serde_json::Value| v["layers"][0]["tiles"][0]["udim"] = json!(1101)) as Box<dyn Fn(&mut serde_json::Value)>),
        ("tile 1000", Box::new(|v: &mut serde_json::Value| v["layers"][0]["tiles"][0]["udim"] = json!(1000))),
        ("resolution 16384", Box::new(|v: &mut serde_json::Value| v["layers"][0]["resolution"] = json!(16384))),
        ("page outside", Box::new(|v: &mut serde_json::Value| v["layers"][0]["tiles"][0]["pages"][0]["at"] = json!(4))),
        ("page twice", Box::new(move |v: &mut serde_json::Value| v["layers"][0]["tiles"][0]["pages"] = json!([page.clone(), page.clone()]))),
        ("short page", Box::new(|v: &mut serde_json::Value| v["layers"][0]["tiles"][0]["pages"][0]["data"] = json!("AAAA"))),
        ("101 tiles", Box::new(|v: &mut serde_json::Value| v["layers"][0]["tiles"] = json!((0..101).map(|k| json!({ "udim": 1001 + k, "pages": [] })).collect::<Vec<_>>()))),
        ("channel twice", Box::new(|v: &mut serde_json::Value| {
            let l = v["layers"][0].clone();
            v["layers"] = json!([l.clone(), l]);
        })),
        ("too many pages", Box::new(|v: &mut serde_json::Value| {
            // 9 tiles of 1024 pages each (8K): past the 4 GiB budget, refused
            // from the counts before anything is inflated.
            v["layers"][0]["resolution"] = json!(8192);
            v["layers"][0]["tiles"] = json!((0..9).map(|k| json!({ "udim": 1001 + k, "pages": (0..1024).map(|i| json!({ "at": i, "data": "" })).collect::<Vec<_>>() })).collect::<Vec<_>>());
        })),
    ] {
        let r = with(&*bad);
        eprintln!("{name}: {r:?}");
        assert!(r.is_err(), "{name}");
    }
    // Random damage to a saved Poly with tiles: read or refused, no panic.
    let mut q = p.clone();
    q.uv = Some(crate::poly::Chunked::new(&unwrap_udim(&p, 512, 4, Udim { tiles: 2, texel_density: Some(300.0) }).unwrap().uv));
    let text = serde_json::to_string(&painted(&q)).unwrap();
    let bytes = text.as_bytes();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let (mut read, mut refused) = (0, 0);
    for _ in 0..1500 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let mut b = bytes.to_vec();
        for k in 0..1 + (seed % 4) as usize {
            let at = ((seed >> (8 * k)) as usize) % b.len();
            b[at] = b"0123456789{}[]\":,-.e"[(seed as usize >> 20).wrapping_add(k) % 20];
        }
        let r = std::panic::catch_unwind(|| serde_json::from_slice::<Poly>(&b).is_ok());
        match r {
            Ok(true) => read += 1,
            Ok(false) => refused += 1,
            Err(_) => panic!("a damaged file panicked"),
        }
    }
    eprintln!("damaged Polys with tiles: {read} read, {refused} refused");
}

/// Resolved paint strokes from JSON (object space, nothing to snap to).
pub(crate) fn strokes(raws: &[serde_json::Value]) -> Vec<crate::brush::resolve::Resolved> {
    crate::sculpt::prepare(raws, Some(crate::sculpt::Brush::Paint), &crate::brush::resolve::ResolveCtx::local()).unwrap().into_iter().map(|x| x.1).collect()
}

/// A cube-sphere cage (radius 0.5) unwrapped over UDIM tiles, shown at
/// level 2.
pub(crate) fn sphere_udim() -> Poly {
    let mut p = crate::poly::project::tests::cube_sphere(8, 0.5);
    p.uv = Some(crate::poly::Chunked::new(&unwrap_udim(&p, 512, 4, Udim { tiles: 4, texel_density: None }).unwrap().uv));
    p.set_levels(2).unwrap();
    p.level = 2;
    p
}

/// Points of the Poly's shown level on a UV seam whose corners lie in
/// two tiles, with their corners' UVs.
fn seam_points(p: &Poly) -> Vec<(Vec3, Vec<[f32; 2]>)> {
    let s = crate::poly::subdiv::evaluate(p, p.level).unwrap();
    let uv = crate::poly::uv::at_level(p, p.level).unwrap();
    let mut by_point: std::collections::BTreeMap<u32, Vec<[f32; 2]>> = Default::default();
    for (c, &v) in s.corners.iter().enumerate() {
        let e = by_point.entry(v).or_default();
        if !e.contains(&uv[c]) {
            e.push(uv[c]);
        }
    }
    by_point.into_iter().filter(|(_, u)| u.len() > 1).map(|(v, u)| (s.points[v as usize], u)).collect()
}

/// The plan's `udim_paint_seamless`, at the texels: a soft stroke across
/// a UV seam between charts on two tiles paints both sides, and every
/// seam point under it reads the same colour from each of its charts
/// (within 2/255), so the seam does not show. (The film render is in
/// `film::subdiv::tests::udim_paint_renders_seamless`.)
#[test]
fn udim_paint_seamless() {
    let p = sphere_udim();
    let seams = seam_points(&p);
    let across: Vec<&(Vec3, Vec<[f32; 2]>)> = seams.iter().filter(|(_, u)| u.iter().map(|t| tile_of(*t)).collect::<std::collections::BTreeSet<_>>().len() > 1).collect();
    assert!(!across.is_empty(), "the fixture has seams between tiles");
    let center = across[across.len() / 2].0;
    let radius = 0.15;
    let surface = paint::Surface::new(&p, p.level).unwrap();
    let painted = paint::paint_poly(&p, &surface, Channel::Color, 1024, None, false, strokes(&[json!({ "point": center.to_array(), "radius": radius, "color": [0.9, 0.2, 0.1], "hardness": 0.3 })])).unwrap();
    let t = &painted.poly.paint.as_ref().unwrap().textures;
    eprintln!("painted {:?} texels on tiles {:?}", painted.touched, t.tiles(Channel::Color));
    assert!(t.tiles(Channel::Color).len() >= 2, "both sides' tiles painted");
    let (mut worst, mut checked, mut inner) = (0.0f32, 0, 0);
    for (q, uvs) in &seams {
        let d = q.distance(center);
        if d > radius {
            continue;
        }
        let samples: Vec<[f32; 4]> = uvs.iter().map(|u| t.sample(Channel::Color, *u).unwrap()).collect();
        for a in &samples {
            for b in &samples {
                for k in 0..4 {
                    worst = worst.max((a[k] - b[k]).abs() * 255.0);
                }
            }
        }
        if d < 0.3 * radius {
            assert!(samples.iter().all(|s| s[3] > 0.9), "both sides painted near the middle: {samples:?}");
            inner += 1;
        }
        checked += 1;
    }
    eprintln!("{checked} seam points under the dab ({inner} near its middle): largest difference between charts {worst:.3}/255");
    assert!(checked > 4 && inner > 0);
    assert!(worst <= 2.0, "{worst}");
}

/// The plan's `udim_paint_replays`: strokes on color and roughness (an
/// erase among them) replayed give the same tiles, bit for bit, on 1 and
/// 12 threads and after save and load; records carry their digests.
#[test]
fn udim_paint_replays() {
    let p = sphere_udim();
    let surface = paint::Surface::new(&p, p.level).unwrap();
    let a = paint::paint_poly(&p, &surface, Channel::Color, 512, None, false, strokes(&[
        json!({ "points": [[0.0, 0.5, 0.0], [0.3, 0.4, 0.1], [0.5, 0.0, 0.0]], "radius": 0.08, "color": [0.1, 0.4, 0.9] }),
        json!({ "point": [0.2, 0.45, 0.0], "radius": 0.05, "erase": true, "strength": 0.6 }),
    ])).unwrap().poly;
    let b = paint::paint_poly(&a, &surface, Channel::Roughness, 256, Some(0.3), false, strokes(&[json!({ "point": [0.0, 0.0, 0.5], "radius": 0.2, "color": [1, 1, 1], "symmetry": { "axes": ["x"] } })])).unwrap().poly;
    let paint = b.paint.as_ref().unwrap();
    assert_eq!(paint.records.len(), 3);
    assert!(paint.records.iter().all(|r| r.digest == r.digest()));
    let h = paint.textures.hash();
    let again = |threads: usize| crate::clay::with_threads(threads, || paint::replay(&b, &paint.records).unwrap().hash());
    assert_eq!(again(1), h);
    assert_eq!(again(12), h);
    let back: Poly = serde_json::from_str(&serde_json::to_string(&b).unwrap()).unwrap();
    assert_eq!(back.hash(), b.hash());
    assert_eq!(paint::replay(&back, &back.paint.as_ref().unwrap().records).unwrap().hash(), h);
    // A changed digest is refused on load (a record that would write
    // something else than it says).
    let mut v = serde_json::to_value(&b).unwrap();
    v["paint"]["records"][0]["value"] = json!([0.5, 0.5, 0.5]);
    assert!(serde_json::from_value::<Poly>(v).err().unwrap().to_string().contains("digest"));
}

/// Memory stays bounded: a stroke that would make more pages than the
/// budget is refused before its dab writes anything (the textures are as
/// they were), counted from the texels it would touch; within the budget
/// the same stroke paints. The budget here is 3 pages (the Poly's is
/// `MAX_PAGES`, 4 GiB).
#[test]
fn paint_budget_is_kept() {
    let p = sphere_udim();
    let surface = paint::Surface::new(&p, p.level).unwrap();
    let s = strokes(&[json!({ "point": [0.0, 0.0, 0.5], "radius": 0.45, "color": [1, 0, 0] })]);
    let p0 = s[0].paint.clone().unwrap();
    let ink = paint::Ink { channel: Channel::Color, resolution: 2048, value: [1.0, 0.0, 0.0], erase: false, opacity: 1.0 };
    let mut t = Textures::default();
    let mut visited = 0;
    let e = paint::apply_within(&mut t, &surface, &ink, &p0.curve, &s[0].dabs, &mut visited, 3, None).err().unwrap();
    eprintln!("{e}");
    assert!(e.contains("pages") && t.pages() == 0, "{e}");
    let mut visited = 0;
    let n = paint::apply_within(&mut t, &surface, &ink, &p0.curve, &s[0].dabs, &mut visited, MAX_PAGES, None).unwrap();
    eprintln!("{n} texels in {} pages ({} MiB)", t.pages(), t.bytes() >> 20);
    assert!(t.pages() > 3 && n > 0);
}

/// §6: a dab of radius 40 texels on an 8K tile in ≤ 3 ms, and a 4-tile
/// 4K colour bake from clay (the bake's texel code per tile, put into
/// pages) in ≤ 20 s (`--ignored`; CLIDE_PERF=1 holds the bounds).
#[test]
#[ignore]
fn udim_perf() {
    // Dabs: one tile of 8192 over a 1,536-quad cube sphere, shown at
    // level 1.
    let mut p = crate::poly::project::tests::cube_sphere(16, 0.5);
    let made = unwrap_udim(&p, 8192, 8, Udim { tiles: 1, texel_density: None }).unwrap();
    let density = chart_stats(&p, &made.uv, &made.face_chart, 8192).values().map(|s| s.0).fold(f32::MAX, f32::min);
    p.uv = Some(crate::poly::Chunked::new(&made.uv));
    p.set_levels(1).unwrap();
    p.level = 1;
    let surface = paint::Surface::new(&p, 1).unwrap();
    let radius = 40.0 / density;
    let s = strokes(&[json!({ "point": [0, 0, 0.5], "radius": radius, "color": [1, 0, 0] })]);
    let curve = s[0].paint.clone().unwrap().curve;
    let ink = paint::Ink { channel: Channel::Color, resolution: 8192, value: [1.0, 0.0, 0.0], erase: false, opacity: 1.0 };
    let mut t = Textures::default();
    let mut times = Vec::new();
    let mut touched = 0;
    for k in 0..64 {
        let a = k as f32 * 0.37;
        let q = Vec3::new(a.cos() * 0.3, a.sin() * 0.3, 0.4).normalize() * 0.5;
        let mut dab = s[0].dabs[0].clone();
        dab.points = vec![q];
        let mut visited = 0;
        let timer = std::time::Instant::now();
        touched += paint::apply(&mut t, &surface, &ink, &curve, &[dab], &mut visited).unwrap();
        times.push(timer.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    let median = times[times.len() / 2];
    eprintln!("dab of 40 texels on an 8K tile ({density:.0} texels per unit, radius {radius:.4}): median {median:.2} ms, slowest {:.2} ms, {} texels a dab", times[times.len() - 1], touched / 64);
    // Bake: the noise sphere into 4 tiles of 4096.
    let c = crate::poly::project::tests::noise_sphere();
    let mut q = crate::poly::project::tests::cube_sphere(12, 0.5);
    q.uv = Some(crate::poly::Chunked::new(&unwrap_udim(&q, 4096, 8, Udim { tiles: 4, texel_density: None }).unwrap().uv));
    let composed = crate::layers::composed_clay(&c);
    let rest = composed.build_mesh();
    let progress = crate::progress::Progress::default();
    let timer = std::time::Instant::now();
    let low = crate::poly::low::Low::new_udim(&q, 0, glam::Mat4::IDENTITY, &rest, &progress).unwrap();
    let mut textures = Textures::default();
    for c in BAKED {
        textures.set_resolution(c, 4096).unwrap();
    }
    let mut tiles = 0;
    for tile in 1001..1005 {
        let Some(lt) = low.tile(tile) else { continue };
        let src = crate::bake::Source { clay: &composed, rest: &rest, skin: None, parts: &[], library: &[], fallback: "", fine: None, low: Some(&lt) };
        let (mesh, _) = crate::bake::bake(&src, crate::bake::Settings { texture: 4096, ..Default::default() }, &progress).unwrap();
        let under = Textures::pages_under(lt.mesh.indices.chunks_exact(3).map(|k| [k[0], k[1], k[2]].map(|v| lt.mesh.vertices[v as usize].uv)), 4096, 4.0);
        put_baked(&mut textures, tile, mesh.textures.as_ref().unwrap(), &under);
        tiles += 1;
    }
    let bake_s = timer.elapsed().as_secs_f32();
    eprintln!("{tiles}-tile 4K bake: {bake_s:.2} s, {} pages ({} MiB)", textures.pages(), textures.bytes() >> 20);
    assert_eq!(tiles, 4);
    if std::env::var("CLIDE_PERF").is_ok() {
        assert!(median <= 3.0 && bake_s <= 20.0, "{median} ms, {bake_s} s");
    }
}

/// Strokes are spatial: after a new UV layout (other tiles, another
/// density) the records painted again give the same colour at every
/// point of the shown level, within 2/255, though the texels moved.
#[test]
fn paint_survives_a_new_unwrap() {
    let p = sphere_udim();
    let surface = paint::Surface::new(&p, p.level).unwrap();
    let a = paint::paint_poly(&p, &surface, Channel::Color, 1024, None, false, strokes(&[
        json!({ "points": [[0.0, 0.5, 0.0], [0.35, 0.35, 0.0], [0.5, 0.0, 0.0]], "radius": 0.15, "color": [0.1, 0.4, 0.9], "hardness": 0.2 }),
    ])).unwrap().poly;
    let colors = |q: &Poly| {
        let s = crate::poly::subdiv::evaluate(q, q.level).unwrap();
        let uv = crate::poly::uv::at_level(q, q.level).unwrap();
        point_colors(&q.paint.as_ref().unwrap().textures, &s.corners, &uv, s.points.len()).unwrap()
    };
    let before = colors(&a);
    let mut b = a.clone();
    b.uv = Some(crate::poly::Chunked::new(&unwrap_udim(&b, 512, 4, Udim { tiles: 7, texel_density: None }).unwrap().uv));
    let records = a.paint.as_ref().unwrap().records.clone();
    b.paint = Some(Arc::new(PolyPaint { textures: paint::replay(&b, &records).unwrap(), records, imported: false }));
    assert_ne!(b.paint.as_ref().unwrap().textures.hash(), a.paint.as_ref().unwrap().textures.hash(), "the texels moved");
    let after = colors(&b);
    let worst = before.iter().zip(&after).map(|(x, y)| (0..3).map(|k| (x[k] - y[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max) * 255.0;
    let painted = before.iter().filter(|c| c[0] < 0.9).count();
    eprintln!("{painted} painted points: colours after the new unwrap within {worst:.3}/255");
    assert!(painted > 50 && worst <= 2.0, "{worst}");
}

/// Memory, for bakes: `bake udim` counts the pages from the UV layout
/// before its job, and 12 tiles of 8K each mostly covered (12 plates, one
/// a tile) pass the 4 GiB budget, while at 2048 they fit.
#[test]
fn bake_estimate_counts_pages() {
    let mut p = plates(12, 1.0, 0.9);
    p.uv = Some(crate::poly::Chunked::new(&unwrap_udim(&p, 2048, 8, Udim { tiles: 12, texel_density: Some(2000.0) }).unwrap().uv));
    let (tiles, big) = bake_estimate(&p, 8192, 4.0);
    let (_, small) = bake_estimate(&p, 2048, 4.0);
    eprintln!("{} tiles: {big} pages at 8192 ({} GiB), {small} at 2048", tiles.len(), (big as u64 * PAGE_BYTES) >> 30);
    assert_eq!(tiles.len(), 12);
    assert!(big > MAX_PAGES && small <= MAX_PAGES, "{big} {small}");
}

/// Security review fix: face queries far away or of any size cost no
/// more than the surface (the cell span in i64, clamped to the surface's
/// box: an i32 overflow once made a huge loop), and a file's records with
/// dabs far outside the Poly, or radii larger than it, are refused.
#[test]
fn far_and_huge_dabs_are_bounded() {
    let p = sphere_udim();
    let surface = paint::Surface::new(&p, p.level).unwrap();
    let ink = paint::Ink { channel: Channel::Color, resolution: 512, value: [1.0, 0.0, 0.0], erase: false, opacity: 1.0 };
    let s = strokes(&[json!({ "point": [0.0, 0.0, 0.5], "radius": 0.1, "color": [1, 0, 0] })]);
    let curve = s[0].paint.clone().unwrap().curve;
    for (point, radius) in [(Vec3::splat(3.0e9), 1.0e9f32), (Vec3::new(-3.0e38, 0.0, 3.0e38), 1.0), (Vec3::new(1.0e6, 0.0, 0.0), 0.1), (Vec3::ZERO, 1.0e30)] {
        let mut dab = s[0].dabs[0].clone();
        dab.points = vec![point];
        dab.radius = radius;
        let mut t = Textures::default();
        let mut visited = 0;
        let timer = std::time::Instant::now();
        let r = paint::apply(&mut t, &surface, &ink, &curve, &[dab], &mut visited);
        eprintln!("dab at {point} radius {radius}: {:?} in {:.1} ms", r.as_ref().map(|_| t.pages()), timer.elapsed().as_secs_f64() * 1000.0);
        assert!(visited <= paint::MAX_VISITED);
        assert!(surface.snap(point).is_some());
    }
    // On load.
    let painted = paint::paint_poly(&p, &surface, Channel::Color, 512, None, false, s).unwrap().poly;
    let good = serde_json::to_value(&painted).unwrap();
    assert!(serde_json::from_value::<Poly>(good.clone()).is_ok());
    for (key, value) in [("points", json!([[1.0e9, 0.0, 0.0]])), ("radius", json!(1.0e9))] {
        let mut v = good.clone();
        v["paint"]["records"][0]["stroke"]["dabs"][0][key] = value;
        // The digest is made again, so only the bound refuses it.
        let mut r: paint::Record = serde_json::from_value(v["paint"]["records"][0].clone()).unwrap();
        r.digest = r.digest();
        v["paint"]["records"][0] = serde_json::to_value(&r).unwrap();
        let e = serde_json::from_value::<Poly>(v).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(e.contains("far outside"), "{key}: {e}");
    }
}
