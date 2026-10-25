//! Picture checks for the viewport renderer (ignored tests, run by hand):
//! `cargo test --release graphics_sheets -- --ignored --nocapture` writes
//! the shared fixture and a studio shot to `graphics_check/` (see its
//! README). OUT=dir writes elsewhere; GRAPHICS_SIZE=WxH sets the size.

use glam::{Mat4, Quat, Vec3};

use serde_json::json;

use crate::render::post::ViewLook;
use crate::render::test_support::{self, BACKGROUND, Fixture};
use std::collections::HashSet;
use std::sync::Arc;

use crate::render::{Ao, Shadows, ViewportCallback};
use crate::view_light::ViewLighting;
use crate::scene::{Scene, Shape};

/// A floorless studio scene seen like an agent screenshot: objects on and
/// above the grid, a glass ball, a clay figure.
pub(crate) struct Studio {
    pub scene: Scene,
    pub meshes: crate::mesh::MeshLib,
    pub eye: Vec3,
    pub target: Vec3,
}

impl Studio {
    pub fn new() -> Self {
        let mut scene = Scene::default();
        scene.materials.push(crate::material::Material { name: "glass".into(), transmission: 1.0, roughness: 0.05, ..Default::default() });
        fn add(scene: &mut Scene, shape: Shape, at: [f32; 3], color: [f32; 3]) -> &mut crate::scene::Object {
            let id = scene.add(shape);
            let o = scene.get_mut(id).unwrap();
            o.translation = at.into();
            o.color = color;
            o
        }
        add(&mut scene, Shape::Cube, [-1.3, 0.5, 0.1], [0.8, 0.32, 0.25]).rotation = Quat::from_rotation_y(0.5);
        add(&mut scene, Shape::Sphere, [0.1, 0.5, 1.0], [0.3, 0.5, 0.82]);
        add(&mut scene, Shape::Torus, [1.5, 1.1, 0.2], [0.85, 0.7, 0.3]).rotation = Quat::from_rotation_x(1.1);
        add(&mut scene, Shape::Cone, [1.3, 0.5, -1.4], [0.4, 0.72, 0.36]);
        add(&mut scene, Shape::Sphere, [-0.4, 0.5, 1.9], [0.9, 0.95, 1.0]).material = Some("glass".into());
        add(&mut scene, Shape::Clay, [-0.2, 0.0, -1.0], [0.85, 0.72, 0.55]).clay = Some(
            serde_json::from_value(serde_json::json!({
                "blobs": [
                    { "type": "capsule", "a": [0, 0.25, 0], "b": [0, 1.2, 0], "radius": 0.24 },
                    { "type": "sphere", "center": [0, 1.5, 0], "radius": 0.26 },
                    { "type": "capsule", "a": [0, 1.05, 0], "b": [0.55, 0.75, 0.15], "radius": 0.09 },
                    { "type": "capsule", "a": [0, 1.05, 0], "b": [-0.5, 1.35, 0.1], "radius": 0.09 },
                ],
                "resolution": 32,
            }))
            .unwrap(),
        );
        let mut meshes = crate::mesh::MeshLib::new();
        meshes.sync(&scene);
        Studio { scene, meshes, eye: Vec3::new(4.6, 3.6, 5.6), target: Vec3::new(0.0, 0.6, 0.0) }
    }

    pub fn view_proj(&self, width: u32, height: u32) -> Mat4 {
        Mat4::perspective_rh(35f32.to_radians(), width as f32 / height as f32, 0.05, 60.0) * Mat4::look_at_rh(self.eye, self.target, Vec3::Y)
    }

    /// The callback an agent screenshot with the grid would use.
    pub fn callback(&self, width: u32, height: u32) -> crate::render::ViewportCallback {
        crate::app::scene_callback_of(&self.scene, &self.meshes, self.view_proj(width, height), self.eye, false, true)
    }
}

fn save(path: &std::path::Path, img: &egui::ColorImage) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let rgb: Vec<u8> = img.pixels.iter().flat_map(|p| [p.r(), p.g(), p.b()]).collect();
    image::save_buffer(path, &rgb, img.size[0] as u32, img.size[1] as u32, image::ColorType::Rgb8).unwrap();
    eprintln!("wrote {}", path.display());
}

fn out_dir() -> std::path::PathBuf {
    let dir = std::env::var("OUT").map(std::path::PathBuf::from).unwrap_or_else(|_| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("graphics_check"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn size() -> (u32, u32) {
    std::env::var("GRAPHICS_SIZE")
        .ok()
        .and_then(|s| s.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))))
        .unwrap_or((480, 360))
}

/// The renderer's look before shadows: none, and the old key direction.
pub(crate) fn legacy() -> Shadows {
    Shadows { enabled: false, ground: false, key_dir: LEGACY_KEY, ..Default::default() }
}

/// The viewport's key direction before it moved to render_film's.
pub(crate) const LEGACY_KEY: Vec3 = Vec3::new(0.4, 0.8, 0.5);

/// Before and after pictures of the fixture and the studio shot, a
/// side-by-side sheet of each, soft shadows and an orbit strip.
#[test]
#[ignore]
fn graphics_sheets() {
    let Some(rs) = test_support::headless_with(test_support::LINEAR) else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let dir = out_dir();
    let (w, h) = size();
    // Stage 1's pictures stay on the direct path (no HDR finishing) and
    // the classic lighting.
    let shoot = |mut cb: ViewportCallback, shadows: Shadows| {
        cb.shadows = shadows;
        cb.post.enabled = false;
        cb.lighting = ViewLighting::classic();
        test_support::render(&rs, &cb, w, h, BACKGROUND).unwrap()
    };
    let fixture = Fixture::new();
    let studio = Studio::new();
    let soft = Shadows { soft: true, ..Default::default() };
    let pairs = [
        ("fixture", shoot(fixture.callback(w, h, false), legacy()), shoot(fixture.callback(w, h, false), Shadows::default())),
        ("studio", shoot(studio.callback(w, h), legacy()), shoot(studio.callback(w, h), Shadows::default())),
    ];
    for (name, before, after) in &pairs {
        save(&dir.join(format!("{name}_before.png")), before);
        save(&dir.join(format!("{name}_after.png")), after);
        save(&dir.join(format!("{name}_before_after.png")), &crate::app::contact_sheet(&[before.clone(), after.clone()], 2));
    }
    save(&dir.join("studio_soft.png"), &shoot(studio.callback(w, h), soft));
    // Orbiting keeps the map's fit, so shadows hold still between frames.
    let (tw, th) = (w / 2, h / 2);
    let tiles: Vec<_> = (0..4)
        .map(|i| {
            let a = i as f32 * std::f32::consts::FRAC_PI_2 * 0.5;
            let offset = Quat::from_rotation_y(a) * (studio.eye - studio.target);
            let eye = studio.target + offset;
            let view_proj = Mat4::perspective_rh(35f32.to_radians(), tw as f32 / th as f32, 0.05, 60.0) * Mat4::look_at_rh(eye, studio.target, Vec3::Y);
            let mut cb = crate::app::scene_callback_of(&studio.scene, &studio.meshes, view_proj, eye, false, true);
            cb.shadow_focus = Some((studio.target, offset.length()));
            cb.post.enabled = false;
            cb.lighting = ViewLighting::classic();
            test_support::render(&rs, &cb, tw, th, BACKGROUND).unwrap()
        })
        .collect();
    save(&dir.join("studio_orbit.png"), &crate::app::contact_sheet(&tiles, 4));
}

/// A dark stone corner lit by what is in it: two torches on the wall, a
/// glowing ball (the glow preset), a lit window and a ceiling panel.
pub(crate) fn glow_room() -> (Scene, crate::mesh::MeshLib, Vec3, Vec3) {
    use crate::envkit::kit::{Kit, Piece};
    let mut scene = Scene::default();
    fn add(scene: &mut Scene, shape: Shape, at: [f32; 3]) -> &mut crate::scene::Object {
        let id = scene.add(shape);
        let o = scene.get_mut(id).unwrap();
        o.translation = at.into();
        o
    }
    for (piece, at) in [(Piece::Floor, [0.0, 0.0, 0.0]), (Piece::Wall, [0.0, 0.0, -1.5])] {
        add(&mut scene, Shape::Kit, at).kit = Some(Kit::new(piece));
    }
    for x in [-1.3, 1.3] {
        add(&mut scene, Shape::Kit, [x, 1.4, -1.25]).kit = Some(Kit::new(Piece::Torch));
    }
    scene.materials.push(crate::material::Material::preset("glow").unwrap());
    scene.materials.push(crate::material::Material { name: "window_glow".into(), emission: 6.0, ..Default::default() });
    scene.materials.push(crate::material::Material { name: "panel".into(), emission: 8.0, ..Default::default() });
    let ball = add(&mut scene, Shape::Sphere, [-0.5, 0.35, 0.4]);
    ball.scale = Vec3::splat(0.7);
    ball.color = [0.3, 0.75, 1.0];
    ball.material = Some("glow".into());
    let window = add(&mut scene, Shape::Cube, [0.0, 1.5, -1.2]);
    window.scale = Vec3::new(0.7, 0.9, 0.04);
    window.color = [1.0, 0.8, 0.5];
    window.material = Some("window_glow".into());
    let panel = add(&mut scene, Shape::Cube, [0.8, 2.6, 0.0]);
    panel.scale = Vec3::new(0.8, 0.03, 0.3);
    panel.color = [0.9, 0.95, 1.0];
    panel.material = Some("panel".into());
    let cube = add(&mut scene, Shape::Cube, [0.7, 0.4, 0.5]);
    cube.scale = Vec3::splat(0.8);
    cube.color = [0.7, 0.6, 0.5];
    let mut meshes = crate::mesh::MeshLib::new();
    meshes.sync(&scene);
    (scene, meshes, Vec3::new(2.4, 1.9, 4.2), Vec3::new(0.0, 1.0, -0.3))
}

/// Before and after pictures of the HDR finishing: the fixture (with a
/// selected cube and the grid), the tonemaps and two film looks, and a
/// room lit by emitters; plus a timing of the extra passes.
#[test]
#[ignore]
fn hdr_sheets() {
    let Some(rs) = test_support::headless_with(test_support::LINEAR) else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let dir = out_dir();
    let (w, h) = size();
    // Stage 3's pictures keep the classic lighting they were made with.
    let fixture = Fixture::new();
    let classic = |rs: &egui_wgpu::RenderState, look, w, h| finished_lit(rs, &fixture, look, w, h, BACKGROUND, ViewLighting::classic());
    let shot = |look| classic(&rs, look, w, h);
    let (before, after) = (shot(direct()), shot(ViewLook::default()));
    save(&dir.join("post_before.png"), &before);
    save(&dir.join("post_after.png"), &after);
    save(&dir.join("post_before_after.png"), &crate::app::contact_sheet(&[before, after], 2));
    // ACES, AgX, Filmic, none; trailer, night_ops (their grades and vignettes).
    let (tw, th) = (w / 2, h / 2);
    let looks = [json!({ "tonemap": "aces" }), json!({ "tonemap": "agx" }), json!({ "tonemap": "filmic" }), json!({ "look": "trailer" }), json!({ "look": "night_ops" })];
    let mut tiles: Vec<_> = looks.into_iter().map(|args| classic(&rs, ViewLook { bloom: 0.08, ..film_look(args) }, tw, th)).collect();
    tiles.insert(3, classic(&rs, ViewLook { tone: crate::render::post::Tone::None, ..Default::default() }, tw, th));
    save(&dir.join("looks.png"), &crate::app::contact_sheet(&tiles, 3));
    let (scene, meshes, eye, target) = glow_room();
    let view_proj = Mat4::perspective_rh(40f32.to_radians(), w as f32 / h as f32, 0.05, 60.0) * Mat4::look_at_rh(eye, target, Vec3::Y);
    let room = |look| {
        let mut cb = crate::app::scene_callback_of(&scene, &meshes, view_proj, eye, false, false);
        cb.post = look;
        cb.lighting = ViewLighting::classic();
        test_support::render(&rs, &cb, w, h, BACKGROUND).unwrap()
    };
    let (before, after) = (room(direct()), room(ViewLook::default()));
    save(&dir.join("glow_before.png"), &before);
    save(&dir.join("glow_after.png"), &after);
    save(&dir.join("glow_before_after.png"), &crate::app::contact_sheet(&[before, after], 2));
    // Time a 1280x720 picture each way (readback included).
    let (pw, ph) = (1280, 720);
    let studio = Studio::new();
    let time = |look| {
        let mut cb = studio.callback(pw, ph);
        cb.post = look;
        cb.lighting = ViewLighting::classic();
        test_support::render(&rs, &cb, pw, ph, BACKGROUND).unwrap();
        let start = std::time::Instant::now();
        for _ in 0..10 {
            crate::render::render_image(&rs, &cb, pw, ph, BACKGROUND).unwrap();
        }
        start.elapsed().as_secs_f64() * 100.0
    };
    eprintln!("render_image {pw}x{ph}: direct {:.1} ms, HDR {:.1} ms (mean of 10)", time(direct()), time(ViewLook::default()));
}

// GPU tests of the viewport's shadows. Each runs on the sRGB target and on
// the window's kind (manual gamma), and skips where there is no GPU.

const W: u32 = 160;
const H: u32 = 120;

/// The fixture after `edit`, with its meshes rebuilt.
fn edited(edit: impl FnOnce(&mut Scene)) -> Fixture {
    let mut f = Fixture::new();
    edit(&mut f.scene);
    f.meshes.sync(&f.scene);
    f
}

fn object(scene: &mut Scene, shape: Shape) -> &mut crate::scene::Object {
    scene.objects.iter_mut().find(|o| o.shape == shape).unwrap()
}

/// The fixture's floor and ball only, the ball raised so its shadow lies
/// apart from it, seen from the far side so the shadow falls toward the
/// camera. `floor` false leaves the ball alone over the grid.
fn ball_over_floor(floor: bool) -> Fixture {
    let mut f = edited(|s| {
        s.objects.retain(|o| o.shape == Shape::Sphere || (floor && o.shape == Shape::Plane));
        object(s, Shape::Sphere).translation.y = 1.0;
    });
    f.eye = Vec3::new(-3.2, 2.6, -4.4);
    f.target = Vec3::new(0.0, 0.3, 0.0);
    f
}

/// Renders the fixture without the grid (its lines are never shadowed).
fn shot(rs: &egui_wgpu::RenderState, f: &Fixture, shadows: Shadows) -> egui::ColorImage {
    let mut cb = f.callback(W, H, false);
    cb.show_grid = false;
    cb.shadows = shadows;
    test_support::render(rs, &cb, W, H, BACKGROUND).unwrap()
}

/// A few pixels around where `p` lands in the fixture's view.
fn around(f: &Fixture, p: Vec3) -> test_support::Rect {
    let c = f.view_proj(W, H).project_point3(p);
    let (x, y) = (((c.x * 0.5 + 0.5) * W as f32) as usize, ((0.5 - c.y * 0.5) * H as f32) as usize);
    assert!((3..W as usize - 3).contains(&x) && (3..H as usize - 3).contains(&y), "{p} is off screen");
    [x - 2, y - 2, x + 3, y + 3]
}

fn grey(img: &egui::ColorImage, r: test_support::Rect) -> f64 {
    test_support::region_mean(img, r).iter().sum::<f64>() / 3.0
}

/// Where the shadow of the fixture's sphere centre falls on the floor.
fn sphere_shadow(f: &Fixture) -> Vec3 {
    let c = f.scene.objects.iter().find(|o| o.shape == Shape::Sphere).unwrap().translation;
    let k = Shadows::default().key_dir.normalize();
    c - k * (c.y / k.y)
}

fn off() -> Shadows {
    Shadows { enabled: false, ..Default::default() }
}

fn each_format(mut test: impl FnMut(egui_wgpu::wgpu::TextureFormat, &egui_wgpu::RenderState)) {
    for format in test_support::FORMATS {
        let Some(rs) = test_support::headless_with(format) else {
            eprintln!("no GPU adapter: skipped");
            return;
        };
        test(format, &rs);
    }
}

#[test]
fn shadow_pipelines_build_and_draw_without_errors() {
    each_format(|format, rs| {
        let f = Fixture::new();
        // Every filter and both map sizes (the resize path), then back.
        for shadows in [Shadows::default(), Shadows { soft: true, ..Default::default() }, Shadows { quality: crate::render::ShadowQuality::High, ..Default::default() }, Shadows::default()] {
            let mut cb = f.callback(W, H, false);
            cb.shadows = shadows;
            test_support::render(rs, &cb, W, H, BACKGROUND).unwrap_or_else(|e| panic!("{format:?} {shadows:?}: {e}"));
        }
    });
}

#[test]
fn shadows_off_matches_the_renderer_before_shadows() {
    each_format(|format, rs| {
        let name = if format.is_srgb() { "srgb" } else { "linear" };
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("graphics_check/baseline/fixture_{name}.png"));
        let png = image::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())).to_rgb8();
        let before = test_support::from_rgb(png.width() as usize, png.height() as usize, png.as_raw());
        let (w, h) = (png.width(), png.height());
        let f = Fixture::new();
        let render = |shadows| {
            let mut cb = f.callback(w, h, false);
            cb.shadows = shadows;
            // The baselines predate the HDR path and the environment lighting.
            cb.post.enabled = false;
            cb.lighting = ViewLighting::classic();
            test_support::render(rs, &cb, w, h, BACKGROUND).unwrap()
        };
        // Shadows off with the old key reproduces the old renderer.
        let legacy = render(legacy());
        let diff = test_support::max_abs_diff(&legacy, &before);
        assert!(diff <= 1, "{format:?}: shadows off differs from before by {diff}/255");
        // Moving the key to render_film's direction is a small, intended change.
        let moved = render(off());
        let mad = test_support::mean_abs_diff(&moved, &before);
        eprintln!("{format:?}: the film key changes the fixture by mad {mad:.2}");
        assert!(mad > 0.05 && mad < 3.0, "{format:?}: key change mad {mad:.2}");
    });
}

#[test]
fn shadow_darkens_the_floor_under_the_ball() {
    each_format(|format, rs| {
        let f = ball_over_floor(true);
        let (on, before) = (shot(rs, &f, Shadows::default()), shot(rs, &f, off()));
        let shade = around(&f, sphere_shadow(&f));
        let (lit, dark) = (grey(&before, shade), grey(&on, shade));
        assert!(dark < lit * 0.75, "{format:?}: floor under the ball {dark:.1} vs lit {lit:.1}");
        // Open floor away from every shadow is untouched.
        let open = around(&f, Vec3::new(-1.5, 0.0, -1.5));
        let (a, b) = (grey(&before, open), grey(&on, open));
        assert!((a - b).abs() <= 1.0, "{format:?}: open floor {b:.1} vs {a:.1}");
        // Soft shadows darken it too.
        let soft = shot(rs, &f, Shadows { soft: true, ..Default::default() });
        assert!(grey(&soft, shade) < lit * 0.8, "{format:?}: soft shadow {:.1} vs lit {lit:.1}", grey(&soft, shade));
    });
}

#[test]
fn shadows_leave_a_lone_floor_alone() {
    // A surface casting onto itself must not speckle (acne), flat or tilted.
    each_format(|format, rs| {
        for tilt in [0.0f32, 0.6] {
            let f = edited(|s| {
                s.objects.retain(|o| o.shape == Shape::Plane);
                let p = object(s, Shape::Plane);
                p.rotation = Quat::from_rotation_x(tilt);
                p.translation.y = if tilt > 0.0 { 1.5 } else { 0.0 };
            });
            let shadows = Shadows { ground: false, ..Default::default() };
            let (on, before) = (shot(rs, &f, shadows), shot(rs, &f, off()));
            let (max, mad) = (test_support::max_abs_diff(&on, &before), test_support::mean_abs_diff(&on, &before));
            assert!(max <= 2 && mad < 0.1, "{format:?} tilt {tilt}: max {max}, mad {mad:.3}");
        }
    });
}

#[test]
fn a_real_floor_hides_the_ground_catcher() {
    each_format(|format, rs| {
        // A floor larger than every shadow: the catcher must not darken it twice.
        let mut f = Fixture::new();
        object(&mut f.scene, Shape::Plane).scale = Vec3::splat(30.0);
        f.meshes.sync(&f.scene);
        let caught = shot(rs, &f, Shadows::default());
        let floor_only = shot(rs, &f, Shadows { ground: false, ..Default::default() });
        let diff = test_support::max_abs_diff(&caught, &floor_only);
        assert!(diff <= 1, "{format:?}: the catcher shows through the floor ({diff}/255)");
    });
}

#[test]
fn the_ground_catcher_shows_shadows_without_a_floor() {
    each_format(|format, rs| {
        let f = ball_over_floor(false);
        let shade = around(&f, sphere_shadow(&f));
        let caught = shot(rs, &f, Shadows::default());
        let bare = shot(rs, &f, Shadows { ground: false, ..Default::default() });
        let (a, b) = (grey(&caught, shade), grey(&bare, shade));
        assert!(a < b * 0.8, "{format:?}: catcher {a:.1} vs background {b:.1}");
        // Away from shadows the background stays exactly as it was.
        let sky = [0, 0, W as usize, 3];
        assert_eq!(test_support::region_mean(&caught, sky), test_support::region_mean(&bare, sky), "{format:?}");
    });
}

#[test]
fn glass_casts_no_shadow() {
    each_format(|format, rs| {
        let mut f = ball_over_floor(true);
        f.scene.materials.push(crate::material::Material { name: "glass".into(), transmission: 1.0, ..Default::default() });
        object(&mut f.scene, Shape::Sphere).material = Some("glass".into());
        f.meshes.sync(&f.scene);
        let shade = around(&f, sphere_shadow(&f));
        let (on, before) = (shot(rs, &f, Shadows::default()), shot(rs, &f, off()));
        let (a, b) = (grey(&on, shade), grey(&before, shade));
        assert!((a - b).abs() <= 2.0, "{format:?}: under the glass ball {a:.1} vs {b:.1}");
    });
}

#[test]
fn masks_ignore_shadows() {
    each_format(|format, rs| {
        let f = Fixture::new();
        let mask = |shadows| {
            let mut cb = f.callback(W, H, true);
            cb.shadows = shadows;
            test_support::render(rs, &cb, W, H, [0, 0, 0]).unwrap()
        };
        let (on, before) = (mask(Shadows::default()), mask(off()));
        assert!(on.pixels == before.pixels, "{format:?}: shadows changed a mask");
        assert_eq!(on.pixels[0], egui::Color32::BLACK, "{format:?}");
        assert!(on.pixels.iter().all(|p| p.r() == p.g() && p.g() == p.b()), "{format:?}: a mask has colour");
        assert!(on.pixels.contains(&egui::Color32::WHITE), "{format:?}: the mask is empty");
    });
}

#[test]
fn the_shadow_cache_follows_scene_changes() {
    each_format(|format, rs| {
        let a = ball_over_floor(true);
        let mut b = ball_over_floor(true);
        object(&mut b.scene, Shape::Sphere).translation.x -= 0.6;
        b.meshes.sync(&b.scene);
        // A again after B: the offscreen map must be redrawn, not reused.
        let first = shot(rs, &a, Shadows::default());
        let moved = shot(rs, &b, Shadows::default());
        let again = shot(rs, &a, Shadows::default());
        let diff = test_support::max_abs_diff(&first, &again);
        assert!(diff <= 1, "{format:?}: A then B then A differs by {diff}/255");
        let shade = around(&a, sphere_shadow(&a));
        assert!((grey(&moved, shade) - grey(&first, shade)).abs() > 5.0, "{format:?}: moving the ball did not move its shadow");
    });
}

#[test]
fn scenes_over_the_triangle_budget_draw_without_shadows() {
    each_format(|format, rs| {
        let f = ball_over_floor(true);
        let mut cb = f.callback(W, H, false);
        cb.show_grid = false;
        let (key, ball) = *cb.fills.iter().find(|(k, _)| *k == crate::mesh::MeshKey::Prim(Shape::Sphere)).unwrap();
        let per = test_support::checked(rs, || {
            let mut one = f.callback(W, H, false);
            one.fills = vec![(key, ball)];
            crate::render::render_image(rs, &one, W, H, BACKGROUND).unwrap();
            test_support::offscreen_shadow_status(rs)
        })
        .unwrap();
        assert!(per.active && per.triangles > 0, "{format:?}: {per:?}");
        // Copies of the ball, all in one place, past the budget.
        let copies = (crate::render::SHADOW_TRIANGLE_BUDGET / per.triangles + 1) as usize;
        let at = cb.fills.iter().position(|(k, _)| *k == key).unwrap();
        cb.fills.splice(at..at, std::iter::repeat_n((key, ball), copies));
        let (img, status) = test_support::checked(rs, || (crate::render::render_image(rs, &cb, W, H, BACKGROUND).unwrap(), test_support::offscreen_shadow_status(rs))).unwrap();
        assert!(!status.active && status.reason.is_some_and(|r| r.contains("budget")), "{format:?}: {status:?}");
        // The same count and gate turn ambient occlusion off.
        let ao = test_support::checked(rs, || test_support::offscreen_ao_status(rs)).unwrap();
        assert!(!ao.active && ao.reason.is_some_and(|r| r.contains("budget")), "{format:?}: {ao:?}");
        cb.shadows = off();
        let plain = test_support::render(rs, &cb, W, H, BACKGROUND).unwrap();
        assert!(test_support::max_abs_diff(&img, &plain) <= 1, "{format:?}: over budget should look unshadowed");
    });
}

// GPU tests of the HDR finishing (render::post), on both targets.

/// The fixture with `look`, a selection wire around the cube and the grid.
fn finished(rs: &egui_wgpu::RenderState, f: &Fixture, look: ViewLook, w: u32, h: u32, clear: [u8; 3]) -> egui::ColorImage {
    finished_lit(rs, f, look, w, h, clear, ViewLighting::studio_default())
}

fn finished_lit(rs: &egui_wgpu::RenderState, f: &Fixture, look: ViewLook, w: u32, h: u32, clear: [u8; 3], lighting: std::sync::Arc<ViewLighting>) -> egui::ColorImage {
    let mut cb = f.callback(w, h, false);
    cb.post = look;
    cb.lighting = lighting;
    if let Some((key, cube)) = cb.fills.iter().find(|(k, _)| *k == crate::mesh::MeshKey::Prim(Shape::Cube)).copied() {
        cb.wires.push((key, cube.with_color(crate::app::theme::WIRE_SELECTED)));
    }
    test_support::render(rs, &cb, w, h, clear).unwrap()
}

fn direct() -> ViewLook {
    ViewLook { enabled: false, ..Default::default() }
}

/// A film look by its render_film arguments.
fn film_look(args: serde_json::Value) -> ViewLook {
    crate::film::Look::parse(&args).unwrap().view_look()
}

/// A glowing ball (emission 10) alone above the grid plane, no floor, seen
/// from the front.
fn glow_ball(emission: f32) -> Fixture {
    let mut f = edited(|s| {
        s.objects.retain(|o| o.shape == Shape::Sphere);
        s.materials.push(crate::material::Material { name: "glow".into(), emission, ..Default::default() });
        let ball = object(s, Shape::Sphere);
        ball.material = Some("glow".into());
        ball.color = [1.0, 0.75, 0.45];
        ball.translation = Vec3::new(0.0, 1.0, 0.0);
        ball.scale = Vec3::splat(0.6);
    });
    f.eye = Vec3::new(0.0, 1.2, 6.0);
    f.target = Vec3::new(0.0, 1.0, 0.0);
    f
}

fn glow_shot(rs: &egui_wgpu::RenderState, f: &Fixture, look: ViewLook, clear: [u8; 3]) -> egui::ColorImage {
    let mut cb = f.callback(W, H, false);
    cb.show_grid = false;
    cb.post = look;
    test_support::render(rs, &cb, W, H, clear).unwrap()
}

/// Pixels 3 to 8 pixels outside the ball's silhouette (from its mask).
fn ring(rs: &egui_wgpu::RenderState, f: &Fixture) -> Vec<usize> {
    let mask = test_support::render(rs, &f.callback(W, H, true), W, H, [0, 0, 0]).unwrap();
    let inside: Vec<bool> = mask.pixels.iter().map(|p| p.r() > 0).collect();
    let (w, h) = (W as i32, H as i32);
    (0..(w * h) as usize)
        .filter(|&i| {
            let (x, y) = (i as i32 % w, i as i32 / w);
            let near = |r: i32| (-r..=r).any(|dy| (-r..=r).any(|dx| dx * dx + dy * dy <= r * r && (0..w).contains(&(x + dx)) && (0..h).contains(&(y + dy)) && inside[((y + dy) * w + x + dx) as usize]));
            !near(2) && near(8)
        })
        .collect()
}

fn mean_grey(img: &egui::ColorImage, at: &[usize]) -> f64 {
    at.iter().map(|&i| img.pixels[i]).map(|p| (f64::from(p.r()) + f64::from(p.g()) + f64::from(p.b())) / 3.0).sum::<f64>() / at.len().max(1) as f64
}

#[test]
fn hdr_passes_build_and_draw_without_errors() {
    each_format(|format, rs| {
        let f = glow_ball(10.0);
        let looks = [ViewLook::default(), film_look(json!({ "tonemap": "agx" })), film_look(json!({ "tonemap": "filmic", "vignette": 0.4, "white_balance": 3500 })), film_look(json!({ "look": "trailer" })), ViewLook::plain(), ViewLook { bloom: 0.0, ..Default::default() }];
        for look in looks {
            for (w, h) in [(16, 16), (17, 33), (W, H)] {
                let mut cb = f.callback(w, h, false);
                cb.post = look;
                test_support::render(rs, &cb, w, h, BACKGROUND).unwrap_or_else(|e| panic!("{format:?} {w}x{h} {look:?}: {e}"));
            }
        }
        let mut cb = Fixture::new().callback(1024, 768, false);
        cb.post = ViewLook::default();
        test_support::render(rs, &cb, 1024, 768, BACKGROUND).unwrap_or_else(|e| panic!("{format:?} 1024x768: {e}"));
    });
}

#[test]
fn plain_hdr_matches_the_direct_path() {
    // No tonemap, bloom or grade: the HDR path must draw what the direct
    // path draws, edges, grid and wires included (the fixture is opaque).
    each_format(|format, rs| {
        let f = Fixture::new();
        let (hdr, before) = (finished(rs, &f, ViewLook::plain(), W, H, BACKGROUND), finished(rs, &f, direct(), W, H, BACKGROUND));
        let (max, mad) = (test_support::max_abs_diff(&hdr, &before), test_support::mean_abs_diff(&hdr, &before));
        eprintln!("{format:?}: plain HDR vs direct: max {max}, mad {mad:.3}");
        assert!(max <= 2 && mad <= 0.5, "{format:?}: plain HDR differs from the direct path: max {max}, mad {mad:.3}");
    });
}

#[test]
fn glass_blends_in_linear_light_through_hdr() {
    // The studio's glass ball blends premultiplied in linear light on the
    // HDR path; the direct path blends in the target's space (gamma on the
    // window's). Characterised, not equal.
    each_format(|format, rs| {
        let studio = Studio::new();
        let shoot = |look| {
            let mut cb = studio.callback(W, H);
            cb.post = look;
            test_support::render(rs, &cb, W, H, BACKGROUND).unwrap()
        };
        let (hdr, before) = (shoot(ViewLook::plain()), shoot(direct()));
        let (max, mad, db) = (test_support::max_abs_diff(&hdr, &before), test_support::mean_abs_diff(&hdr, &before), test_support::psnr(&hdr, &before));
        eprintln!("{format:?}: glass studio plain HDR vs direct: max {max}, mad {mad:.3}, psnr {db:.1} dB");
        assert!(mad <= 0.5 && db >= 45.0, "{format:?}: glass through HDR: max {max}, mad {mad:.3}, psnr {db:.1} dB");
    });
}

#[test]
fn background_stays_exact_under_hdr() {
    each_format(|format, rs| {
        let f = Fixture::new();
        let sky = [0, 0, W as usize, 4];
        for clear in [BACKGROUND, [0, 0, 0]] {
            let before = finished(rs, &f, direct(), W, H, clear);
            // Without bloom nothing reaches the background at all.
            let no_bloom = finished(rs, &f, ViewLook { bloom: 0.0, ..Default::default() }, W, H, clear);
            assert_eq!(test_support::region_mean(&no_bloom, sky), test_support::region_mean(&before, sky), "{format:?} {clear:?}");
            if !format.is_srgb() {
                assert!(no_bloom.pixels[..W as usize * 4].iter().all(|p| [p.r(), p.g(), p.b()] == clear), "{format:?}: sky is not the clear colour");
            }
            // The fixture has no emitters: a default bloom leaves it all but alone.
            let bloom = finished(rs, &f, ViewLook::default(), W, H, clear);
            let corner = |img: &egui::ColorImage| test_support::from_rgb(W as usize, 4, &img.pixels[..W as usize * 4].iter().flat_map(|p| [p.r(), p.g(), p.b()]).collect::<Vec<_>>());
            let diff = test_support::max_abs_diff(&corner(&bloom), &corner(&before));
            assert!(diff <= 1, "{format:?} {clear:?}: bloom moved the empty sky by {diff}/255");
        }
    });
}

#[test]
fn masks_ignore_hdr() {
    each_format(|format, rs| {
        let f = Fixture::new();
        let mask = |look| {
            let mut cb = f.callback(W, H, true);
            cb.post = look;
            test_support::render(rs, &cb, W, H, [0, 0, 0]).unwrap()
        };
        let (on, before) = (mask(ViewLook::default()), mask(direct()));
        assert!(on.pixels == before.pixels, "{format:?}: HDR changed a mask");
        assert!(on.pixels.iter().all(|p| *p == egui::Color32::BLACK || *p == egui::Color32::WHITE || (p.r() == p.g() && p.g() == p.b())), "{format:?}: a mask has colour");
        assert!(on.pixels.contains(&egui::Color32::WHITE), "{format:?}: the mask is empty");
    });
}

#[test]
fn emitters_glow_and_nothing_else_does() {
    each_format(|format, rs| {
        let f = glow_ball(10.0);
        let around = ring(rs, &f);
        assert!(around.len() > 50, "{format:?}: ring of {} pixels", around.len());
        let bloom = glow_shot(rs, &f, ViewLook::default(), BACKGROUND);
        let none = glow_shot(rs, &f, ViewLook { bloom: 0.0, ..Default::default() }, BACKGROUND);
        let bg = BACKGROUND.iter().map(|&c| f64::from(c)).sum::<f64>() / 3.0;
        let (lit, plain) = (mean_grey(&bloom, &around), mean_grey(&none, &around));
        eprintln!("{format:?}: ring around the glowing ball {lit:.1} with bloom, {plain:.1} without, background {bg:.1}");
        // About +5/255 at this size, film's strength 0.08.
        assert!(lit > bg + 3.0, "{format:?}: no halo: ring {lit:.1} vs background {bg:.1}");
        assert!((plain - bg).abs() < 0.01, "{format:?}: without bloom the ring {plain:.1} should be background {bg:.1}");
        // A ball that does not glow keeps the background clean.
        let dull = glow_ball(0.0);
        let quiet = mean_grey(&glow_shot(rs, &dull, ViewLook::default(), BACKGROUND), &around);
        assert!((quiet - bg).abs() <= 1.0, "{format:?}: a lit, unlit-emission ball bloomed: ring {quiet:.1} vs {bg:.1}");
        // The halo grows with the picture (a share of its size, like
        // film's): twice the size, four times the haloed pixels (a halo of
        // fixed pixel width would only double).
        let haloed = |w: u32, h: u32| {
            let mut cb = f.callback(w, h, false);
            cb.show_grid = false;
            let img = test_support::render(rs, &cb, w, h, BACKGROUND).unwrap();
            let mask = test_support::render(rs, &f.callback(w, h, true), w, h, [0, 0, 0]).unwrap();
            img.pixels.iter().zip(&mask.pixels).filter(|(p, m)| m.r() == 0 && (f64::from(p.r()) + f64::from(p.g()) + f64::from(p.b())) / 3.0 > bg + 2.0).count()
        };
        let (small, big) = (haloed(W, H), haloed(2 * W, 2 * H));
        eprintln!("{format:?}: haloed pixels {small} at {W}x{H}, {big} at twice the size");
        assert!(big as f64 > small as f64 * 3.0, "{format:?}: haloed pixels {small} at {W}x{H} vs {big} at twice the size");
    });
}

#[test]
fn bloom_moves_light_without_making_any() {
    // With no tonemap and no threshold, strength 1 sends all of the light
    // into the halo; the total over the picture must stay put. The sRGB
    // target adds in linear light, on a grey clear so 8-bit rounding
    // averages out.
    let Some(rs) = test_support::headless_with(test_support::SRGB) else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let f = glow_ball(2.0);
    let clear = [64, 64, 64];
    let decode = |v: u8| {
        let c = f64::from(v) / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let light = |strength: f32| {
        let look = ViewLook { tone: crate::render::post::Tone::None, bloom: strength, bloom_threshold: 0.0, exposure: 0.25, ..Default::default() };
        let img = glow_shot(&rs, &f, look, clear);
        assert!(img.pixels.iter().all(|p| p.r() < 250), "the ball clips");
        img.pixels.iter().map(|p| decode(p.r()) + decode(p.g()) + decode(p.b()) - 3.0 * decode(clear[0])).sum::<f64>()
    };
    let (kept, spread) = (light(0.0), light(1.0));
    eprintln!("light over the clear: {kept:.2} without bloom, {spread:.2} all bloomed");
    assert!(kept > 50.0, "the ball is too dim to measure ({kept:.2})");
    assert!((spread / kept - 1.0).abs() < 0.05, "bloom changed the total light: {kept:.2} -> {spread:.2}");
}

#[test]
fn wires_and_grid_stay_crisp_over_hdr() {
    each_format(|format, rs| {
        let f = Fixture::new();
        let shoot = |look, wire: bool, grid: bool| {
            let mut cb = f.callback(W, H, false);
            cb.post = look;
            cb.show_grid = grid;
            if wire && let Some((key, cube)) = cb.fills.iter().find(|(k, _)| *k == crate::mesh::MeshKey::Prim(Shape::Cube)).copied() {
                cb.wires.push((key, cube.with_color(crate::app::theme::WIRE_SELECTED)));
            }
            test_support::render(rs, &cb, W, H, BACKGROUND).unwrap()
        };
        let look = ViewLook::default();
        let (bare, wired) = (shoot(direct(), false, false), shoot(direct(), true, false));
        let (hdr_bare, hdr_wired) = (shoot(look, false, false), shoot(look, true, false));
        // Where the wire covers a whole pixel it shows its own colour (the
        // greenest it gets), never tonemapped.
        let changed: Vec<usize> = (0..bare.pixels.len()).filter(|&i| wired.pixels[i] != bare.pixels[i]).collect();
        let solid = changed.iter().map(|&i| wired.pixels[i]).max_by_key(|p| p.g() as i32 - p.r() as i32).unwrap();
        let full: Vec<usize> = changed.into_iter().filter(|&i| (0..3).all(|c| wired.pixels[i][c].abs_diff(solid[c]) <= 2)).collect();
        assert!(full.len() > 10, "{format:?}: only {} solid wire pixels", full.len());
        for &i in &full {
            for c in 0..3 {
                assert!(hdr_wired.pixels[i][c].abs_diff(wired.pixels[i][c]) <= 2, "{format:?}: wire pixel {i} is {:?} through HDR, {:?} direct", hdr_wired.pixels[i], wired.pixels[i]);
            }
        }
        // The wire changes nothing it does not cover.
        let untouched: Vec<usize> = (0..bare.pixels.len()).filter(|&i| wired.pixels[i] == bare.pixels[i]).collect();
        assert!(untouched.iter().all(|&i| (0..3).all(|c| hdr_wired.pixels[i][c].abs_diff(hdr_bare.pixels[i][c]) <= 1)), "{format:?}: the wire leaks through HDR");
        // The grid hides behind the same fills (occlusion is unchanged).
        let (grid, hdr_grid) = (shoot(direct(), false, true), shoot(look, false, true));
        let hidden: Vec<usize> = (0..bare.pixels.len()).filter(|&i| grid.pixels[i] == bare.pixels[i]).collect();
        let leaks = hidden.iter().filter(|&&i| (0..3).any(|c| hdr_grid.pixels[i][c].abs_diff(hdr_bare.pixels[i][c]) > 1)).count();
        assert!(leaks * 100 <= hidden.len(), "{format:?}: the grid shows through {leaks} of {} hidden pixels", hidden.len());
        assert!(grid.pixels != bare.pixels && hdr_grid.pixels != hdr_bare.pixels, "{format:?}: no grid drawn");
    });
}

#[test]
fn the_window_draws_what_render_image_draws() {
    // A pane through prepare and paint (egui's path) against render_image,
    // both finished and direct, whole and partly off screen.
    each_format(|format, rs| {
        let f = glow_ball(10.0);
        let (sw, sh) = (W + 40, H + 30);
        for look in [ViewLook::default(), direct()] {
            let mut cb = f.callback(W, H, false);
            cb.post = look;
            cb.slot = 0;
            cb.frame = 1;
            cb.background = [egui::Color32::from_rgb(BACKGROUND[0], BACKGROUND[1], BACKGROUND[2]); 2];
            let alone = test_support::render(rs, &cb, W, H, BACKGROUND).unwrap();
            // A pane wholly on screen, 20 px in.
            let pane = egui::Rect::from_min_size(egui::pos2(20.0, 10.0), egui::vec2(W as f32, H as f32));
            cb.pane = Some(pane);
            let screen = test_support::paint_window(rs, &cb, pane, [sw, sh], BACKGROUND).unwrap();
            let crop = |img: &egui::ColorImage, x0: usize, y0: usize, w: usize, h: usize| test_support::from_rgb(w, h, &(y0..y0 + h).flat_map(|y| img.pixels[y * img.size[0] + x0..][..w].iter().flat_map(|p| [p.r(), p.g(), p.b()])).collect::<Vec<_>>());
            let inside = crop(&screen, 20, 10, W as usize, H as usize);
            let diff = test_support::max_abs_diff(&inside, &alone);
            assert!(diff <= 1, "{format:?} {}: the pane differs from render_image by {diff}/255", look.enabled);
            // A pane hanging 50 px off the left and 40 px off the bottom:
            // what is left of it shows the same picture in the same place.
            let off = egui::Rect::from_min_size(egui::pos2(-50.0, sh as f32 - H as f32 + 40.0), egui::vec2(W as f32, H as f32));
            cb.pane = Some(off);
            cb.post.bloom = 0.0;
            let alone = {
                let mut cb = f.callback(W, H, false);
                cb.post = ViewLook { bloom: 0.0, ..look };
                test_support::render(rs, &cb, W, H, BACKGROUND).unwrap()
            };
            let screen = test_support::paint_window(rs, &cb, off, [sw, sh], BACKGROUND).unwrap();
            let (kw, kh) = (W as usize - 50, H as usize - 40);
            let shown = crop(&screen, 0, sh as usize - kh, kw, kh);
            let expected = crop(&alone, 50, 0, kw, kh);
            let (max, mad) = (test_support::max_abs_diff(&shown, &expected), test_support::mean_abs_diff(&shown, &expected));
            assert!(max <= 2 && mad < 0.2, "{format:?} {}: an off-screen pane is squashed or moved: max {max}, mad {mad:.3}", look.enabled);
        }
    });
}


// Environment lighting and ambient occlusion (Stage 2): GPU tests on both
// targets, and the pictures in graphics_check/.

fn no_ao() -> Ao {
    Ao { enabled: false, ..Default::default() }
}

/// The fixture without the grid, lit by `lighting` with `ao`, finished by
/// `post` (`direct()` measures the lighting alone).
fn lit(rs: &egui_wgpu::RenderState, f: &Fixture, lighting: &Arc<ViewLighting>, ao: Ao, post: ViewLook, w: u32, h: u32) -> egui::ColorImage {
    let mut cb = f.callback(w, h, false);
    cb.show_grid = false;
    cb.lighting = lighting.clone();
    cb.ao = ao;
    cb.post = post;
    test_support::render(rs, &cb, w, h, BACKGROUND).unwrap()
}

fn lighting(v: serde_json::Value) -> Arc<ViewLighting> {
    ViewLighting::parse(&v).unwrap_or_else(|e| panic!("{v}: {e}"))
}

/// One ball alone (no floor) at y = 1, of `material`, seen from the front.
fn lone_ball(material: crate::material::Material, color: [f32; 3]) -> Fixture {
    let mut f = edited(|s| {
        s.objects.retain(|o| o.shape == Shape::Sphere);
        s.materials.push(material.clone());
        let ball = object(s, Shape::Sphere);
        ball.material = Some(material.name.clone());
        ball.color = color;
        ball.translation = Vec3::new(0.0, 1.0, 0.0);
    });
    f.eye = Vec3::new(0.0, 1.3, 3.4);
    f.target = Vec3::new(0.0, 1.0, 0.0);
    f
}

/// The pixels a fixture's subject covers (its mask), `inset` pixels in
/// from the silhouette.
fn subject(rs: &egui_wgpu::RenderState, f: &Fixture, w: u32, h: u32, inset: i32) -> Vec<usize> {
    let mut cb = f.callback(w, h, true);
    cb.show_grid = false;
    let mask = test_support::render(rs, &cb, w, h, [0, 0, 0]).unwrap();
    let inside = |x: i32, y: i32| (0..w as i32).contains(&x) && (0..h as i32).contains(&y) && mask.pixels[(y * w as i32 + x) as usize].r() > 127;
    (0..(w * h) as usize)
        .filter(|&i| {
            let (x, y) = (i as i32 % w as i32, i as i32 / w as i32);
            (-inset..=inset).all(|dy| (-inset..=inset).all(|dx| inside(x + dx, y + dy)))
        })
        .collect()
}

/// render_film's items for a fixture's scene.
fn film_items(f: &Fixture) -> Vec<crate::film::Item> {
    let eval = f.scene.evaluate();
    crate::envkit::scatter::draws(&f.scene, &eval)
        .iter()
        .filter(|d| d.object.shape.is_mesh())
        .filter_map(|d| Some(crate::film::Item { mesh: f.meshes.get(d.object)?.clone(), world: d.world, tint: d.object.color, surface: f.scene.surface_of(d.object), material: f.scene.material_of(d.object) }))
        .collect()
}

/// A render_film picture of a fixture with render_film arguments `args`
/// (no floor), on the CPU unless `args` says otherwise.
fn film_shot(f: &Fixture, args: serde_json::Value, w: usize, h: usize, samples: u32) -> crate::film::Image {
    let mut args = args;
    if args["device"].is_null() {
        args["device"] = json!("cpu");
    }
    let look = crate::film::Look::parse(&args).unwrap();
    let s = crate::film::Settings { width: w, height: h, samples, bounces: 4, time_limit: std::time::Duration::from_secs(300), floor: false, exposure: 1.0, denoise: true };
    crate::film::render_views_with_look(&crate::film::Meshes::new(film_items(f)), &[], &[f.film_camera()], s, Arc::new(look), &Default::default()).pop().unwrap()
}

/// 8-bit display value to linear light: the sRGB curve on an sRGB target,
/// else pow 2.2 (what the shader and render_film encode with).
fn decode(v: u8, srgb: bool) -> f64 {
    let c = f64::from(v) / 255.0;
    if !srgb {
        c.powf(2.2)
    } else if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Mean linear RGB over `at`.
fn linear_mean(img: &egui::ColorImage, at: &[usize], srgb: bool) -> Vec3 {
    let sum = at.iter().map(|&i| img.pixels[i]).fold(Vec3::ZERO, |s, p| s + Vec3::new(decode(p.r(), srgb) as f32, decode(p.g(), srgb) as f32, decode(p.b(), srgb) as f32));
    sum / at.len().max(1) as f32
}

/// The viewport's finishing with no tonemap, bloom or grade at `exposure`
/// (so pixels decode back to radiance times `exposure`).
fn linear_post(exposure: f32) -> ViewLook {
    ViewLook { tone: crate::render::post::Tone::None, exposure, bloom: 0.0, ..ViewLook::plain() }
}

#[test]
fn lighting_pipelines_build_and_draw_without_errors() {
    each_format(|format, rs| {
        let f = Fixture::new();
        for name in ["studio", "overcast", "sunset", "golden_hour", "forest", "night", "storm", "sky_golden", "classic", "trailer"] {
            let l = lighting(json!(name));
            for (ao, post) in [(Ao::default(), ViewLook::default()), (no_ao(), direct()), (Ao { radius: 3.0, strength: 2.0, ..Default::default() }, direct())] {
                for (w, h) in [(16, 16), (17, 33), (W, H)] {
                    let mut cb = f.callback(w, h, false);
                    cb.lighting = l.clone();
                    cb.ao = ao;
                    cb.post = post;
                    test_support::render(rs, &cb, w, h, BACKGROUND).unwrap_or_else(|e| panic!("{format:?} {name} {w}x{h} {ao:?}: {e}"));
                }
            }
        }
        // The environment cache stays bounded.
        assert!(test_support::gpu_environments(rs) <= 8);
    });
}

#[test]
fn ao_darkens_creases_but_not_open_floor() {
    each_format(|format, rs| {
        let f = Fixture::new();
        // Under an overcast sky alone, ambient light is all there is; in
        // the studio the suns light the floor too, so occlusion shows less.
        for (args, darker) in [(json!({ "environment": "overcast", "lights": "none" }), 8.0), (json!("studio"), 2.0)] {
            let l = lighting(args.clone());
            let (on, off) = test_support::checked(rs, || {
                let on = lit(rs, &f, &l, Ao::default(), direct(), W, H);
                let status = test_support::offscreen_ao_status(rs);
                assert!(status.active, "{format:?}: {status:?}");
                (on, lit(rs, &f, &l, no_ao(), direct(), W, H))
            })
            .unwrap();
            // The floor at the cube's front left foot, and just in front of
            // where the ball touches it.
            for (name, p) in [("cube foot", Vec3::new(-1.45, 0.0, 0.26)), ("ball contact", Vec3::new(0.2, 0.0, 0.95))] {
                let r = around(&f, p);
                let (a, b) = (grey(&on, r), grey(&off, r));
                eprintln!("{format:?} {args}: {name} {a:.1} with AO, {b:.1} without");
                assert!(a <= b - darker, "{format:?} {args}: {name} {a:.1} with AO vs {b:.1} without");
            }
            // Open floor and the ball's top are left alone.
            for (name, p) in [("open floor", Vec3::new(1.0, 0.0, 1.6)), ("ball top", Vec3::new(0.2, 1.0, 0.6))] {
                let r = around(&f, p);
                let (a, b) = (grey(&on, r), grey(&off, r));
                assert!((a - b).abs() <= 2.0, "{format:?} {args}: {name} {a:.1} with AO vs {b:.1} without");
            }
            // Ambient occlusion only darkens.
            let brighter = on.pixels.iter().zip(&off.pixels).filter(|(a, b)| a.r() as i32 + a.g() as i32 + a.b() as i32 > b.r() as i32 + b.g() as i32 + b.b() as i32 + 3).count();
            assert!(brighter * 200 < on.pixels.len(), "{format:?} {args}: {brighter} pixels got brighter with AO");
        }
    });
}

#[test]
fn ao_and_environment_render_repeatably() {
    each_format(|format, rs| {
        let f = Fixture::new();
        let l = lighting(json!("golden_hour"));
        let a = lit(rs, &f, &l, Ao::default(), ViewLook::default(), W, H);
        let b = lit(rs, &f, &l, Ao::default(), ViewLook::default(), W, H);
        let diff = test_support::max_abs_diff(&a, &b);
        assert!(diff <= 1, "{format:?}: two renders differ by {diff}/255");
    });
}

#[test]
fn masks_ignore_lighting_and_ao() {
    each_format(|format, rs| {
        let f = Fixture::new();
        let mask = |l: Arc<ViewLighting>, ao| {
            let mut cb = f.callback(W, H, true);
            cb.lighting = l;
            cb.ao = ao;
            test_support::render(rs, &cb, W, H, [0, 0, 0]).unwrap()
        };
        let (a, b) = (mask(lighting(json!("sunset")), Ao::default()), mask(ViewLighting::classic(), no_ao()));
        assert!(a.pixels == b.pixels, "{format:?}: lighting changed a mask");
        assert_eq!(a.pixels[0], egui::Color32::BLACK, "{format:?}");
        assert!(a.pixels.contains(&egui::Color32::WHITE), "{format:?}: the mask is empty");
        assert!(!test_support::offscreen_ao_status(rs).active);
    });
}

#[test]
fn metal_reflects_the_environment() {
    each_format(|format, rs| {
        let chrome = crate::material::Material { name: "chrome".into(), metallic: 1.0, roughness: 0.15, ..Default::default() };
        let f = lone_ball(chrome, [0.95, 0.95, 0.95]);
        let at = subject(rs, &f, W, H, 2);
        let shot = |name: &str| {
            let img = lit(rs, &f, &lighting(json!({ "environment": name, "lights": "none" })), no_ao(), direct(), W, H);
            linear_mean(&img, &at, format.is_srgb())
        };
        let (overcast, sunset, night) = (shot("overcast"), shot("sunset"), shot("night"));
        eprintln!("{format:?}: chrome ball under overcast {overcast}, sunset {sunset}, night {night}");
        // A bright white-blue sky, a dim blue sky with an orange horizon,
        // and night: the ball shows each.
        assert!(overcast.min_element() > sunset.max_element() * 1.5, "{format:?}: overcast {overcast} vs sunset {sunset}");
        assert!(night.max_element() < 0.05, "{format:?}: night {night}");
        assert!(overcast.z > overcast.x, "{format:?}: overcast's sky is blue-white: {overcast}");
        // Its top mirrors the sky, its bottom the darker ground.
        let (mut top, mut bottom) = (Vec::new(), Vec::new());
        let ys: Vec<usize> = at.iter().map(|&i| i / W as usize).collect();
        let mid = (ys.iter().min().unwrap() + ys.iter().max().unwrap()) / 2;
        for (&i, &y) in at.iter().zip(&ys) {
            if y + 3 < mid { top.push(i) } else if y > mid + 3 { bottom.push(i) }
        }
        let img = lit(rs, &f, &lighting(json!({ "environment": "overcast", "lights": "none" })), no_ao(), direct(), W, H);
        let (t, b) = (linear_mean(&img, &top, format.is_srgb()), linear_mean(&img, &bottom, format.is_srgb()));
        assert!(t.y > b.y * 2.0, "{format:?}: top {t} vs bottom {b}");
    });
}

#[test]
fn reflection_blur_tracks_roughness() {
    // A storm sky (clouds, a bright horizon, no sun) in metal balls:
    // rougher balls blur it, so neighbouring pixels differ less.
    each_format(|format, rs| {
        let detail: Vec<f64> = [0.1, 0.5, 0.9]
            .into_iter()
            .map(|rough| {
                let m = crate::material::Material { name: "metal".into(), metallic: 1.0, roughness: rough, ..Default::default() };
                let f = lone_ball(m, [0.95, 0.95, 0.95]);
                let at = subject(rs, &f, W, H, 2);
                let img = lit(rs, &f, &lighting(json!({ "environment": "storm", "lights": "none" })), no_ao(), direct(), W, H);
                let grey = |i: usize| img.pixels[i].r() as f64 + img.pixels[i].g() as f64 + img.pixels[i].b() as f64;
                let inside: HashSet<usize> = at.iter().copied().collect();
                let steps: Vec<f64> = at.iter().filter(|&&i| inside.contains(&(i + 1)) && inside.contains(&(i + W as usize))).map(|&i| (grey(i + 1) - grey(i)).abs() + (grey(i + W as usize) - grey(i)).abs()).collect();
                steps.iter().sum::<f64>() / steps.len() as f64
            })
            .collect();
        eprintln!("{format:?}: reflection detail by roughness 0.1, 0.5, 0.9: {detail:?}");
        assert!(detail[0] > detail[1] * 1.2 && detail[1] > detail[2] * 1.2, "{format:?}: {detail:?}");
    });
}

#[test]
fn environments_never_overwrite_each_other() {
    // The window and screenshots with different lighting, interleaved:
    // each environment keeps its own texture.
    each_format(|format, rs| {
        let f = Fixture::new();
        let (sunset, overcast) = (lighting(json!("sunset")), lighting(json!("overcast")));
        let pane = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(W as f32, H as f32));
        let window = |l: &Arc<ViewLighting>| {
            let mut cb = f.callback(W, H, false);
            cb.lighting = l.clone();
            cb.slot = 0;
            cb.frame = 1;
            cb.pane = Some(pane);
            cb.background = [egui::Color32::from_rgb(BACKGROUND[0], BACKGROUND[1], BACKGROUND[2]); 2];
            test_support::paint_window(rs, &cb, pane, [W, H], BACKGROUND).unwrap()
        };
        let first = window(&sunset);
        let shot = lit(rs, &f, &overcast, Ao::default(), ViewLook::default(), W, H);
        let again = window(&sunset);
        let diff = test_support::max_abs_diff(&first, &again);
        assert!(diff <= 1, "{format:?}: the window's sunset changed by {diff}/255 after an overcast screenshot");
        assert!(test_support::mean_abs_diff(&first, &shot) > 3.0, "{format:?}: sunset and overcast look alike");
        // The window path (prepare and paint, AO in slot 0) draws what
        // render_image draws.
        let alone = lit(rs, &f, &sunset, Ao::default(), ViewLook::default(), W, H);
        let mut cb = f.callback(W, H, false);
        cb.lighting = sunset.clone();
        cb.slot = 0;
        cb.frame = 1;
        cb.pane = Some(pane);
        cb.show_grid = false;
        cb.background = [egui::Color32::from_rgb(BACKGROUND[0], BACKGROUND[1], BACKGROUND[2]); 2];
        let pane_shot = test_support::paint_window(rs, &cb, pane, [W, H], BACKGROUND).unwrap();
        let diff = test_support::max_abs_diff(&pane_shot, &alone);
        assert!(diff <= 1, "{format:?}: the pane differs from render_image by {diff}/255");
    });
}

#[test]
fn viewport_matches_film_energy() {
    // A lone matte ball (no floor, no studio lights) under overcast, then
    // under sunset whose sun becomes a directional light, then under the
    // physical sky at 30, 8 and 1.5 degrees: the viewport's linear light over the
    // ball against render_film's (CPU, 256 samples, denoised). The film half always runs; the viewport half needs a GPU.
    const FW: u32 = 96;
    const FH: u32 = 72;
    let paint = crate::material::Material { name: "paint".into(), roughness: 0.7, ..Default::default() };
    let f = lone_ball(paint, [0.8, 0.8, 0.8]);
    let cases = [("overcast", 0.05), ("sunset", 0.06), ("sky_afternoon", 0.03), ("sky_golden", 0.03), ("sky_sunset", 0.03)];
    let films: Vec<crate::film::Image> = cases.iter().map(|(env, _)| film_shot(&f, json!({ "environment": env, "lights": "none" }), FW as usize, FH as usize, 256)).collect();
    for film in &films {
        assert!(film.linear.iter().any(|c| c.max_element() > 0.05), "film rendered nothing");
    }
    each_format(|format, rs| {
        let at = subject(rs, &f, FW, FH, 2);
        assert!(at.len() > 300, "{format:?}: the ball covers {} pixels", at.len());
        // Bands across the ball: top, middle, bottom; left and right.
        let xs: Vec<usize> = at.iter().map(|&i| i % FW as usize).collect();
        let ys: Vec<usize> = at.iter().map(|&i| i / FW as usize).collect();
        let (x0, x1, y0, y1) = (*xs.iter().min().unwrap(), *xs.iter().max().unwrap(), *ys.iter().min().unwrap(), *ys.iter().max().unwrap());
        let band = |pick: &dyn Fn(usize, usize) -> bool| at.iter().zip(xs.iter().zip(&ys)).filter(|(_, (x, y))| pick(**x, **y)).map(|(i, _)| *i).collect::<Vec<_>>();
        let (h3, w2) = ((y1 - y0) / 3, (x0 + x1) / 2);
        let regions = [("top", band(&|_, y| y < y0 + h3)), ("middle", band(&|_, y| y >= y0 + h3 && y < y1 - h3)), ("bottom", band(&|_, y| y >= y1 - h3)), ("left", band(&|x, _| x < w2)), ("right", band(&|x, _| x > w2))];
        for ((env, tolerance), film) in cases.iter().zip(&films) {
            let exposure = 0.5;
            let img = lit(rs, &f, &lighting(json!({ "environment": env, "lights": "none" })), Ao::default(), linear_post(exposure), FW, FH);
            for (name, r) in &regions {
                let view = linear_mean(&img, r, format.is_srgb()) / exposure;
                let truth = r.iter().fold(Vec3::ZERO, |s, &i| s + film.linear[i]) / r.len() as f32;
                let lum = |c: Vec3| c.dot(Vec3::new(0.2126, 0.7152, 0.0722));
                let err = (lum(view) - lum(truth)).abs() / lum(truth).max(1e-3);
                eprintln!("{format:?} {env} {name}: viewport {view:.3} vs film {truth:.3} ({:.1}%)", err * 100.0);
                assert!(err <= *tolerance, "{format:?} {env} {name}: viewport {view:.3} vs film {truth:.3} ({:.1}% off)", err * 100.0);
            }
        }
    });
}

// The physical sun and sky (Stage 5E): one environment for render_film and
// the viewport.

/// The physical sky `sun_elevation` and `sun_azimuth` give, as
/// render_film's environment argument.
fn physical(elevation: f32, azimuth: f32) -> serde_json::Value {
    json!({ "preset": "physical", "sun_elevation": elevation, "sun_azimuth": azimuth })
}

/// Where a physical sun puts the shadow of the fixture's sphere centre.
fn sun_shadow(f: &Fixture, env: &serde_json::Value) -> Vec3 {
    let c = f.scene.objects.iter().find(|o| o.shape == Shape::Sphere).unwrap().translation;
    let s = crate::film::sky::Sky::parse(env, crate::film::sky::Sky::named("physical").unwrap()).unwrap().sun_direction();
    c - s * (c.y / s.y)
}

#[test]
fn physical_sun_moves_shadows() {
    // The sun on either side of the camera's line of sight (azimuth 126
    // and 306 degrees): the ball's shadow lies where the sun puts it, in
    // render_film (CPU) and in the viewport, on both targets.
    let f = ball_over_floor(true);
    let skies = [physical(35.0, 126.0), physical(35.0, 306.0)];
    let spots = skies.each_ref().map(|e| around(&f, sun_shadow(&f, e)));
    let check = |what: &str, shots: &[egui::ColorImage; 2]| {
        for k in 0..2 {
            let (dark, lit) = (grey(&shots[k], spots[k]), grey(&shots[1 - k], spots[k]));
            eprintln!("{what}: shadow spot {k} {dark:.1} in its sun, {lit:.1} in the other");
            assert!(dark < lit * 0.75, "{what}: spot {k} is {dark:.1} under its sun vs {lit:.1}");
        }
    };
    let films = skies.each_ref().map(|e| {
        let img = film_shot(&f, json!({ "environment": e, "lights": "none" }), W as usize, H as usize, 32);
        test_support::from_rgb(W as usize, H as usize, &img.pixels.concat())
    });
    check("render_film", &films);
    each_format(|format, rs| {
        let shots = skies.each_ref().map(|e| lit(rs, &f, &lighting(json!({ "environment": e, "lights": "none" })), Ao::default(), ViewLook::default(), W, H));
        assert!(test_support::offscreen_shadow_status(rs).active, "{format:?}: {:?}", test_support::offscreen_shadow_status(rs));
        check(&format!("viewport {format:?}"), &shots);
    });
}

/// The physical sky's pictures: golden_hour against a physical sun in the
/// same place, render_film across sun elevations, and the viewport next
/// to render_film under the physical presets.
#[test]
#[ignore]
fn sky_sheets() {
    let dir = out_dir();
    let (w, h) = size();
    // The environments sheet's scene, seen low so the sky shows.
    let mut scene = edited(|s| {
        s.objects.retain(|o| matches!(o.shape, Shape::Plane | Shape::Cube | Shape::Sphere));
        s.materials.push(crate::material::Material { name: "chrome".into(), metallic: 1.0, roughness: 0.15, ..Default::default() });
        let id = s.add(Shape::Sphere);
        let o = s.get_mut(id).unwrap();
        o.translation = Vec3::new(-0.2, 0.5, -1.1);
        o.color = [0.95, 0.95, 0.95];
        o.material = Some("chrome".into());
    });
    scene.eye = Vec3::new(1.8, 1.1, 5.2);
    scene.target = Vec3::new(-0.3, 0.9, 0.0);
    let film = |env: &serde_json::Value, w: u32, h: u32, samples: u32| {
        let img = film_shot(&scene, json!({ "environment": env, "lights": "none", "background": "environment", "device": "auto" }), w as usize, h as usize, samples);
        eprintln!("{env}: traced by {:?}", img.traced);
        test_support::from_rgb(w as usize, h as usize, &img.pixels.concat())
    };
    // Before: golden_hour, whose sun stands at 16.9 degrees, azimuth 45.
    // After: a physical sun there.
    let golden = json!("golden_hour");
    let after = physical(16.9, 45.0);
    let (b, a) = (film(&golden, w, h, 128), film(&after, w, h, 128));
    save(&dir.join("sky_before.png"), &b);
    save(&dir.join("sky_after.png"), &a);
    save(&dir.join("sky_before_after.png"), &crate::app::contact_sheet(&[b, a], 2));
    // render_film as the sun goes down (azimuth 45).
    let (tw, th) = (w / 2, h / 2);
    let elevations = [90.0, 60.0, 30.0, 10.0, 3.0, 0.0];
    let tiles: Vec<egui::ColorImage> = elevations.iter().map(|&e| film(&physical(e, 45.0), tw, th, 64)).collect();
    save(&dir.join("sky_elevation_sheet.png"), &crate::app::contact_sheet(&tiles, 3));
    // Viewport | render_film under each physical preset.
    let presets = ["sky_noon", "sky_afternoon", "sky_golden", "sky_sunset"];
    let mut tiles = Vec::new();
    if let Some(rs) = test_support::headless_with(test_support::LINEAR) {
        for name in presets {
            let mut cb = scene.callback(tw, th, false);
            cb.show_grid = false;
            cb.lighting = lighting(json!({ "environment": name, "lights": "none" }));
            tiles.push(test_support::render(&rs, &cb, tw, th, BACKGROUND).unwrap());
            tiles.push(film(&json!(name), tw, th, 64));
        }
        save(&dir.join("sky_viewport.png"), &crate::app::contact_sheet(&tiles, 4));
    } else {
        eprintln!("no GPU adapter: sky_viewport.png skipped");
    }
    // A bake's cost (settings no other picture used, so not cached).
    let fresh = crate::film::sky::Sky { azimuth: 12.345, ..crate::film::sky::Sky::named("physical").unwrap() };
    let start = std::time::Instant::now();
    fresh.bake();
    let bake_ms = start.elapsed().as_secs_f64() * 1000.0;
    eprintln!("sky bake {}x{}: {bake_ms:.0} ms", crate::film::sky::WIDTH, crate::film::sky::HEIGHT);
    let settings = json!({
        "size": [w, h],
        "camera": { "eye": scene.eye.to_array(), "target": scene.target.to_array(), "fov": scene.fov },
        "render_film": { "lights": "none", "background": "environment", "floor": false, "device": "auto" },
        "sky_before_after": { "samples": 128, "before": golden, "after": after },
        "sky_elevation_sheet": { "samples": 64, "size": [tw, th], "rows": "left to right, top to bottom", "environments": elevations.map(|e| physical(e, 45.0)) },
        "sky_viewport": { "samples": 64, "size": [tw, th], "layout": "viewport | render_film pairs, two pairs per row", "environments": presets, "viewport": { "lighting": { "lights": "none" }, "ambient_occlusion": true, "post": "default" } },
        "bake_ms": bake_ms,
    });
    std::fs::write(dir.join("sky.json"), serde_json::to_vec_pretty(&settings).unwrap()).unwrap();
}

/// Before and after pictures of the environment lighting and ambient
/// occlusion; every environment preset in the viewport next to
/// render_film; the energy check's ball; and a timing.
#[test]
#[ignore]
fn lighting_sheets() {
    let Some(rs) = test_support::headless_with(test_support::LINEAR) else {
        eprintln!("no GPU adapter: skipped");
        return;
    };
    let dir = out_dir();
    let (w, h) = size();
    let fixture = Fixture::new();
    let studio = Studio::new();
    // Before: the renderer before this stage (classic lighting, no
    // occlusion); after: render_film's studio with occlusion. Both through
    // the default HDR finishing.
    let shoot = |cb: ViewportCallback, l: &Arc<ViewLighting>, ao: Ao| {
        let mut cb = cb;
        cb.lighting = l.clone();
        cb.ao = ao;
        test_support::render(&rs, &cb, w, h, BACKGROUND).unwrap()
    };
    let classic = ViewLighting::classic();
    let film = ViewLighting::studio_default();
    for (name, cb) in [("ibl_fixture", fixture.callback(w, h, false)), ("ibl_studio", studio.callback(w, h))] {
        let before = shoot(cb, &classic, no_ao());
        let cb = if name == "ibl_fixture" { fixture.callback(w, h, false) } else { studio.callback(w, h) };
        let after = shoot(cb, &film, Ao::default());
        save(&dir.join(format!("{name}_before.png")), &before);
        save(&dir.join(format!("{name}_after.png")), &after);
        save(&dir.join(format!("{name}_before_after.png")), &crate::app::contact_sheet(&[before, after], 2));
    }
    // Ambient occlusion alone: the same lighting without and with it.
    let before = shoot(fixture.callback(w, h, false), &film, no_ao());
    let after = shoot(fixture.callback(w, h, false), &film, Ao::default());
    save(&dir.join("ao_before.png"), &before);
    save(&dir.join("ao_after.png"), &after);
    save(&dir.join("ao_before_after.png"), &crate::app::contact_sheet(&[before, after], 2));
    // Every environment (with render_film's studio lights, as
    // render_film's environment argument gives): viewport | render_film,
    // on a chrome ball, a matte ball and the fixture's cube on a floor.
    let scene = edited(|s| {
        s.objects.retain(|o| matches!(o.shape, Shape::Plane | Shape::Cube | Shape::Sphere));
        s.materials.push(crate::material::Material { name: "chrome".into(), metallic: 1.0, roughness: 0.15, ..Default::default() });
        let id = s.add(Shape::Sphere);
        let o = s.get_mut(id).unwrap();
        o.translation = Vec3::new(-0.2, 0.5, -1.1);
        o.color = [0.95, 0.95, 0.95];
        o.material = Some("chrome".into());
    });
    let (tw, th) = (w / 2, h / 2);
    let mut tiles = Vec::new();
    let mut sidecar = Vec::new();
    for env in ["studio", "overcast", "sunset", "golden_hour", "forest", "night", "storm"] {
        let args = json!({ "environment": env });
        let mut cb = scene.callback(tw, th, false);
        cb.show_grid = false;
        cb.lighting = lighting(args.clone());
        tiles.push(test_support::render(&rs, &cb, tw, th, BACKGROUND).unwrap());
        let mut fa = args.clone();
        fa["device"] = json!("auto");
        let img = film_shot(&scene, fa, tw as usize, th as usize, 64);
        tiles.push(test_support::from_rgb(tw as usize, th as usize, &img.pixels.concat()));
        sidecar.push(json!({ "environment": env, "viewport": { "set_display": { "lighting": args, "ambient_occlusion": true, "post": {} } }, "render_film": { "environment": env, "floor": false, "samples": 64, "size": [tw, th] } }));
    }
    save(&dir.join("environments.png"), &crate::app::contact_sheet(&tiles, 4));
    std::fs::write(dir.join("environments.json"), serde_json::to_vec_pretty(&json!({ "camera": { "eye": scene.eye.to_array(), "target": scene.target.to_array(), "fov": scene.fov }, "layout": "rows of viewport | render_film pairs, two pairs per row, in this order", "tiles": sidecar })).unwrap()).unwrap();
    // The energy check's ball: viewport | render_film (both ACES).
    let paint = crate::material::Material { name: "paint".into(), roughness: 0.7, ..Default::default() };
    let ball = lone_ball(paint, [0.8, 0.8, 0.8]);
    let mut tiles = Vec::new();
    for env in ["overcast", "sunset"] {
        let args = json!({ "environment": env, "lights": "none" });
        tiles.push(lit(&rs, &ball, &lighting(args.clone()), Ao::default(), ViewLook { bloom: 0.0, ..ViewLook::default() }, tw, th));
        let img = film_shot(&ball, args, tw as usize, th as usize, 256);
        tiles.push(test_support::from_rgb(tw as usize, th as usize, &img.pixels.concat()));
    }
    save(&dir.join("film_match.png"), &crate::app::contact_sheet(&tiles, 2));
    let camera = |f: &Fixture| json!({ "eye": f.eye.to_array(), "target": f.target.to_array(), "fov": f.fov });
    let ao = Ao::default();
    let settings = json!({
        "size": [w, h],
        "ibl_fixture": { "camera": camera(&fixture), "before": { "lighting": "classic", "ambient_occlusion": false }, "after": { "lighting": "studio", "ambient_occlusion": { "radius": ao.radius, "strength": ao.strength } }, "post": "default (ACES, bloom 0.08)" },
        "ibl_studio": { "camera": { "eye": studio.eye.to_array(), "target": studio.target.to_array(), "fov": 35.0 }, "before": { "lighting": "classic", "ambient_occlusion": false }, "after": { "lighting": "studio", "ambient_occlusion": true } },
        "ao": { "camera": camera(&fixture), "lighting": "studio", "before": { "ambient_occlusion": false }, "after": { "ambient_occlusion": { "radius": ao.radius, "strength": ao.strength } } },
        "film_match": { "camera": camera(&ball), "size": [tw, th], "rows": ["overcast", "sunset"], "lighting": { "lights": "none" }, "viewport_post": { "bloom": 0 }, "render_film": { "lights": "none", "floor": false, "samples": 256, "device": "cpu" } },
    });
    std::fs::write(dir.join("lighting.json"), serde_json::to_vec_pretty(&settings).unwrap()).unwrap();
    // Time a 1280x720 picture of the studio shot (readback included).
    let (pw, ph) = (1280, 720);
    let time = |l: &Arc<ViewLighting>, ao: Ao| {
        let mut cb = studio.callback(pw, ph);
        cb.lighting = l.clone();
        cb.ao = ao;
        test_support::render(&rs, &cb, pw, ph, BACKGROUND).unwrap();
        let start = std::time::Instant::now();
        for _ in 0..10 {
            crate::render::render_image(&rs, &cb, pw, ph, BACKGROUND).unwrap();
        }
        start.elapsed().as_secs_f64() * 100.0
    };
    eprintln!("render_image {pw}x{ph}: classic {:.1} ms, environment {:.1} ms, environment + AO {:.1} ms (mean of 10)", time(&classic, no_ao()), time(&film, no_ao()), time(&film, Ao::default()));
}


// The film denoiser (Stage 5D): one low-sample trace denoised by the
// classic and the variance-guided filter, against a high-sample picture.

/// A film test shot: its items, hair, camera and render_film arguments.
struct FilmCase {
    name: &'static str,
    meshes: crate::film::Meshes,
    fibers: Vec<crate::film::Fiber>,
    camera: crate::film::Camera,
    args: serde_json::Value,
    floor: bool,
}

/// The shared fixture as render_film sees it (no floor: it has its own).
fn fixture_case() -> FilmCase {
    let f = Fixture::new();
    FilmCase { name: "fixture", meshes: crate::film::Meshes::new(film_items(&f)), fibers: Vec::new(), camera: f.film_camera(), args: json!({}), floor: false }
}

/// A close-up of a scaly, colour-textured ball (a library detail map that
/// changes the albedo) under a cap of hair strands, in golden-hour light
/// on a floor, from `d` away: 2.2 shows the whole ball and its shadow,
/// 1.4 fills the picture with the texture.
fn closeup_at(d: f32) -> FilmCase {
    let mut m = crate::material::Material::preset("leather").unwrap();
    m.detail = Some("scales".into());
    m.detail_scale = 0.15;
    m.detail_strength = 2.0;
    m.detail_color = 1.0;
    let clay: crate::clay::Clay = serde_json::from_value(json!({
        "blobs": [{ "type": "sphere", "center": [0, 0.5, 0], "radius": 0.5, "color": [0.6, 0.45, 0.35], "blend": 0.0 }], "resolution": 32
    })).unwrap();
    let mut mesh = clay.build_mesh();
    mesh.vertices.iter_mut().for_each(|v| v.surface = crate::mesh::OBJECT_SURFACE);
    let ball = crate::film::Item { mesh: Arc::new(mesh), world: Mat4::IDENTITY, tint: [1.0; 3], surface: m.surface(), material: m };
    let fibers = (0..2000).map(|i| {
        let a = i as f32 * 2.399_963;
        let r = 0.45 * ((i as f32 + 0.5) / 2000.0).sqrt();
        let root = Vec3::new(r * a.cos(), 0.5 + (0.25 - r * r).max(0.0).sqrt(), r * a.sin());
        let out = (root - Vec3::new(0.0, 0.5, 0.0)).normalize();
        let points = (0..8).map(|k| root + out * 0.01 + (out * 0.3 + Vec3::NEG_Y * 0.4) * (k as f32 / 7.0) * 0.5).collect();
        crate::film::Fiber { points, root_width: 0.004, tip_width: 0.001, root_color: Vec3::new(0.5, 0.3, 0.1), tip_color: Vec3::new(0.7, 0.5, 0.25) }
    }).collect();
    let camera = crate::film::Camera { eye: Vec3::new(0.0, 0.5 + 0.175 * d, d), target: Vec3::new(0.0, 0.5, 0.0), up: Vec3::Y, fov_deg: 30.0, ortho: None };
    FilmCase { name: if d < 2.0 { "texture" } else { "closeup" }, meshes: crate::film::Meshes::new(vec![ball]), fibers, camera, args: json!({ "environment": "golden_hour", "background": "environment" }), floor: true }
}

/// [raw, classic, variance] from one trace of `samples` (CPU unless the
/// case's arguments say otherwise).
fn denoised(case: &FilmCase, w: usize, h: usize, samples: u32) -> [egui::ColorImage; 3] {
    let mut args = case.args.clone();
    if args["device"].is_null() {
        args["device"] = json!("cpu");
    }
    let look = Arc::new(crate::film::Look::parse(&args).unwrap());
    let s = crate::film::Settings { width: w, height: h, samples, bounces: 4, time_limit: std::time::Duration::from_secs(600), floor: case.floor, exposure: 1.0, denoise: true };
    crate::film::render_each_denoiser(&case.meshes, &case.fibers, &case.camera, s, look).map(|img| {
        assert_eq!(img.samples, samples);
        test_support::from_rgb(w, h, &img.pixels.concat())
    })
}

/// Display luma per pixel.
fn luma(img: &egui::ColorImage) -> Vec<f32> {
    img.pixels.iter().map(|p| 0.2126 * f32::from(p.r()) + 0.7152 * f32::from(p.g()) + 0.0722 * f32::from(p.b())).collect()
}

/// Gradient magnitude per pixel (forward differences; 0 on the last row
/// and column).
fn gradient(img: &egui::ColorImage) -> Vec<f32> {
    let [w, h] = img.size;
    let l = luma(img);
    (0..w * h).map(|i| {
        let (x, y) = (i % w, i / w);
        if x + 1 >= w || y + 1 >= h { return 0.0; }
        ((l[i + 1] - l[i]).powi(2) + (l[i + w] - l[i]).powi(2)).sqrt()
    }).collect()
}

/// Edge sharpness kept: the mean gradient of `img` over the reference's
/// strongest 5% of gradients, as a fraction of the reference's there.
fn edge_kept(img: &egui::ColorImage, reference: &egui::ColorImage) -> f32 {
    let (g, r) = (gradient(img), gradient(reference));
    let mut sorted = r.clone();
    sorted.sort_by(f32::total_cmp);
    let cut = sorted[sorted.len() * 95 / 100];
    let edges: Vec<usize> = (0..r.len()).filter(|&i| r[i] >= cut && r[i] > 0.0).collect();
    edges.iter().map(|&i| g[i]).sum::<f32>() / edges.iter().map(|&i| r[i]).sum::<f32>()
}

/// Fine detail in `rect`: luma minus its 3x3 mean.
fn high_pass(img: &egui::ColorImage, rect: test_support::Rect) -> Vec<f32> {
    let w = img.size[0];
    let l = luma(img);
    let [x0, y0, x1, y1] = rect;
    (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| {
        let mean = (0..9).map(|k| l[(y + k / 3 - 1) * w + x + k % 3 - 1]).sum::<f32>() / 9.0;
        l[y * w + x] - mean
    }).collect()
}

/// How much of the reference's fine detail in `rect` `img` keeps: the
/// correlation of their high-pass images (1: all of it, 0: none).
fn detail_kept(img: &egui::ColorImage, reference: &egui::ColorImage, rect: test_support::Rect) -> f32 {
    let (a, b) = (high_pass(img, rect), high_pass(reference, rect));
    let dot = |p: &[f32], q: &[f32]| p.iter().zip(q).map(|(x, y)| x * y).sum::<f32>();
    dot(&a, &b) / (dot(&a, &a) * dot(&b, &b)).sqrt().max(1e-6)
}

/// A textured, hair-free band across the close-up ball's middle.
fn closeup_texture(w: usize, h: usize) -> test_support::Rect {
    [w * 3 / 8, h / 2, w * 5 / 8, h * 3 / 4]
}

/// What a check prints and asserts about one case at one sample count.
struct DenoiseScore {
    psnr: [f64; 3],
    edges: [f32; 3],
    detail: Option<[f32; 3]>,
}

fn denoise_score(case: &FilmCase, w: usize, h: usize, low: u32, high: u32) -> (DenoiseScore, [egui::ColorImage; 3], egui::ColorImage) {
    let pics = denoised(case, w, h, low);
    let [reference, _, _] = denoised(case, w, h, high);
    let score = DenoiseScore {
        psnr: [0, 1, 2].map(|k| test_support::psnr(&pics[k], &reference)),
        edges: [0, 1, 2].map(|k| edge_kept(&pics[k], &reference)),
        detail: (case.name != "fixture").then(|| [0, 1, 2].map(|k| detail_kept(&pics[k], &reference, closeup_texture(w, h)))),
    };
    eprintln!("{} at {low} spp vs {high}: PSNR raw {:.2} classic {:.2} variance {:.2} dB; edges kept {:.3} {:.3} {:.3}; detail kept {:?}",
        case.name, score.psnr[0], score.psnr[1], score.psnr[2], score.edges[0], score.edges[1], score.edges[2], score.detail);
    (score, pics, reference)
}

/// At a low sample count, the variance-guided denoiser is closer to a
/// high-sample picture than the classic one (and the raw samples) on the
/// fixture and the textured, hairy close-up, and keeps the edges
/// (silhouettes, creases, shadow edges, strands) at least as well. Where
/// fine baked texture fills the picture, it keeps as much of the
/// reference's detail as the classic filter, and is no further off.
#[test]
fn variance_denoiser_beats_classic_at_low_samples() {
    for (case, w, h) in [(fixture_case(), 96, 72), (closeup_at(2.2), 128, 96), (closeup_at(1.4), 96, 72)] {
        let (score, _, _) = denoise_score(&case, w, h, 16, 1024);
        let [raw, classic, variance] = score.psnr;
        if let Some(detail) = score.detail.filter(|_| case.name == "texture") {
            assert!(variance > classic - 0.1 && variance > raw + 1.0, "{}: variance {variance:.2} dB, classic {classic:.2} dB, raw {raw:.2} dB", case.name);
            assert!(detail[2] >= detail[1] - 0.02 && detail[2] > detail[0] + 0.05, "{}: detail kept {detail:?}", case.name);
        } else {
            assert!(variance > classic + 1.0 && variance > raw, "{}: variance {variance:.2} dB, classic {classic:.2} dB, raw {raw:.2} dB", case.name);
        }
        assert!(score.edges[2] >= score.edges[1] && score.edges[2] > 0.95, "{}: edges kept {:?}", case.name, score.edges);
    }
}

/// Same-sample before (classic) and after (variance-guided) pictures of
/// the fixture and the close-ups for graphics_check/, with the raw samples
/// and a 2048-sample reference: `cargo test --release denoise_sheets --
/// --ignored --nocapture` (OUT=dir writes elsewhere).
#[test]
#[ignore]
fn denoise_sheets() {
    let dir = out_dir();
    let (w, h) = (320, 240);
    for (case, low) in [(fixture_case(), 16), (fixture_case(), 64), (closeup_at(2.2), 16), (closeup_at(1.4), 16)] {
        let (_, [raw, classic, variance], reference) = denoise_score(&case, w, h, low, 2048);
        let stem = format!("denoise_{}_{low}spp", case.name);
        save(&dir.join(format!("{stem}_before.png")), &classic);
        save(&dir.join(format!("{stem}_after.png")), &variance);
        save(&dir.join(format!("{stem}_before_after.png")), &crate::app::contact_sheet(&[raw, classic, variance, reference], 4));
    }
}
