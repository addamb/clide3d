//! Turnaround sheets (Quality Stage 5a, decision D22): `set_reference
//! {sheet}` loads one image holding several views of a subject (front,
//! side, back in reading order), splits it into per-view references by the
//! subjects' boxes, and gives every view one shared scale (the tallest
//! subject's height in pixels over the subject's world height) and its own
//! ground line (the subject's lowest edge). Each view fills its slot (D13)
//! with that fit, so `compare_reference` and `quality_report` line the
//! model up once for all views (`align`) instead of fitting each view on
//! its own, which would hide proportion errors. Each view also stands in
//! the scene as an image plane (`refplane.rs`) at the same scale, so the
//! slots come back when the scene is saved and opened.
//!
//! Splitting: the subject mask (`subject_mask`, the threshold of a single
//! reference) is labelled into 8-connected components in scan order;
//! specks and components under 15% of the tallest one's height (labels,
//! captions, arrows) are dropped; components whose columns overlap merge
//! into one view, except caption-like parts (wider than tall, under 40% of
//! the view's tallest part and clear of it by a gap of rows), which are
//! left out and reported; views read left to right. A sheet that does not split
//! into as many views as named takes explicit `boxes`. Everything is a
//! pure function of the image and the arguments.

use glam::Vec3;
use serde_json::{Value, json};

use super::ClideApp;
use super::eyes::{Reference, SheetFit, Slot};
use crate::mcp::ToolResult;
use crate::refplane::RefPlane;
use crate::scene::Shape;

/// Sheets larger than this on a side are refused before decoding.
pub(super) const SHEET_MAX: u32 = 8192;
/// At most this many pixels in a sheet.
const SHEET_MAX_PIXELS: u64 = 40_000_000;
/// Sheets are split at this long side at most (larger ones are scaled
/// down first, every view by the same factor).
const SPLIT_MAX: u32 = 4096;
/// Views in one sheet.
pub(super) const MAX_SHEET_VIEWS: usize = 6;
/// A component under this share of the tallest one's height is a label.
const LABEL_SHARE: f32 = 0.15;
/// Margin around each view's cut, as a share of the tallest subject.
const MARGIN: f32 = 0.04;
/// The slots a sheet view can fill: side views, whose ground line is the
/// subject's lowest edge.
const SHEET_SLOTS: &str = "front, back, left, right (or side)";
const KEYS: [&str; 5] = ["path", "views", "boxes", "height", "threshold"];

/// A component or view box in sheet pixels: min x, min y, max x, max y
/// (inclusive).
pub(super) type PxBox = [usize; 4];

/// The 8-connected components of `mask` (w wide), in scan order of their
/// first pixel: (box, pixel count).
pub(super) fn components(mask: &[bool], w: usize) -> Vec<(PxBox, usize)> {
    let h = mask.len() / w.max(1);
    let mut seen = vec![false; mask.len()];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut b, mut n) = ([start % w, start / w, start % w, start / w], 0usize);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            n += 1;
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            for dy in -1i64..=1 {
                for dx in -1i64..=1 {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if (dx, dy) == (0, 0) || nx < 0 || ny < 0 || nx >= w as i64 || ny >= h as i64 {
                        continue;
                    }
                    let j = ny as usize * w + nx as usize;
                    if mask[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        out.push((b, n));
    }
    out
}

/// The views' subject boxes found in `mask` (w × h): see the module doc.
/// Ok also gives the caption-like parts left out of the views' boxes
/// (`captions`); Err gives the boxes it found, left to right.
pub(super) fn split(mask: &[bool], w: usize, h: usize, views: usize) -> Result<(Vec<PxBox>, Vec<PxBox>), Vec<PxBox>> {
    let specks = ((w * h) as f32 * 2e-5).max(4.0) as usize;
    let comps: Vec<PxBox> = components(mask, w).into_iter().filter(|(_, n)| *n >= specks).map(|(b, _)| b).collect();
    let tallest = comps.iter().map(|b| b[3] + 1 - b[1]).max().unwrap_or(0);
    let mut keep: Vec<PxBox> = comps.into_iter().filter(|b| (b[3] + 1 - b[1]) as f32 >= LABEL_SHARE * tallest as f32).collect();
    keep.sort_by_key(|b| (b[0], b[1], b[2], b[3]));
    // Each view: its box and the parts in it.
    let mut groups: Vec<(PxBox, Vec<PxBox>)> = Vec::new();
    for b in keep {
        match groups.last_mut() {
            Some((g, parts)) if b[0] <= g[2] => {
                *g = [g[0].min(b[0]), g[1].min(b[1]), g[2].max(b[2]), g[3].max(b[3])];
                parts.push(b);
            }
            _ => groups.push((b, vec![b])),
        }
    }
    // A caption big enough to pass the label filter would move a view's
    // ground line (and with it the shared scale): parts clear of the
    // view's tallest part by a gap of rows, wider than tall and under 40%
    // of its height, are left out (and reported).
    let mut captions = Vec::new();
    let boxes: Vec<PxBox> = groups
        .into_iter()
        .map(|(g, parts)| {
            let main = *parts.iter().max_by_key(|b| (b[3] + 1 - b[1], std::cmp::Reverse(b[0]))).expect("a part");
            let mh = main[3] + 1 - main[1];
            let caption = |b: &PxBox| {
                let (bw, bh) = (b[2] + 1 - b[0], b[3] + 1 - b[1]);
                (b[1] > main[3] + 1 || b[3] + 1 < main[1]) && bw as f32 >= 1.5 * bh as f32 && (bh as f32) < 0.4 * mh as f32
            };
            let (out, kept): (Vec<PxBox>, Vec<PxBox>) = parts.iter().partition(|b| caption(b));
            if out.is_empty() {
                return g;
            }
            captions.extend(out);
            kept.iter().fold(main, |c, b| [c[0].min(b[0]), c[1].min(b[1]), c[2].max(b[2]), c[3].max(b[3])])
        })
        .collect();
    if boxes.len() == views { Ok((boxes, captions)) } else { Err(boxes) }
}

/// `region` of `img` (w wide, RGBA) scaled by `k` (≤ 1) with area weights,
/// every view by the same `k`, so the shared scale stays exact: pixel
/// edge `e` of the region lands on edge `e · k`.
fn resample(img: &[u8], w: usize, region: PxBox, k: f32) -> (usize, usize, Vec<u8>) {
    let (rw, rh) = (region[2] + 1 - region[0], region[3] + 1 - region[1]);
    // (A hair under, so a factor that should land on a whole size does
    // not round up past it.)
    let (dw, dh) = (((rw as f32 * k - 1e-3).ceil() as usize).max(1), ((rh as f32 * k - 1e-3).ceil() as usize).max(1));
    // Per destination pixel: (source index in the region, weight).
    let taps = |n: usize, d: usize| -> Vec<Vec<(usize, f32)>> {
        (0..d).map(|i| {
            let (a, b) = (i as f32 / k, ((i + 1) as f32 / k).min(n as f32));
            let mut t = Vec::new();
            let mut s = a.floor() as usize;
            while (s as f32) < b && s < n {
                let cover = (b.min(s as f32 + 1.0) - a.max(s as f32)).max(0.0);
                if cover > 0.0 {
                    t.push((s, cover));
                }
                s += 1;
            }
            t
        }).collect()
    };
    let (tx, ty) = (taps(rw, dw), taps(rh, dh));
    let mut out = vec![0u8; dw * dh * 4];
    for (y, ys) in ty.iter().enumerate() {
        for (x, xs) in tx.iter().enumerate() {
            let mut acc = [0f32; 4];
            let mut total = 0.0;
            for &(sy, wy) in ys {
                for &(sx, wx) in xs {
                    let p = &img[((region[1] + sy) * w + region[0] + sx) * 4..][..4];
                    let wgt = wy * wx;
                    for c in 0..4 {
                        acc[c] += p[c] as f32 * wgt;
                    }
                    total += wgt;
                }
            }
            for c in 0..4 {
                out[(y * dw + x) * 4 + c] = (acc[c] / total.max(1e-9)).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    (dw, dh, out)
}

/// What a sheet call asks for, checked from the arguments alone.
pub(super) struct SheetArgs {
    pub path: std::path::PathBuf,
    pub views: Vec<Slot>,
    pub boxes: Option<Vec<[f64; 4]>>,
    pub height: Option<f32>,
    pub threshold: u8,
    pub planes: bool,
    pub opacity: f32,
}

pub(super) fn parse(args: &Value) -> Result<SheetArgs, String> {
    let sheet = args["sheet"].as_object().ok_or("sheet must be an object {path, views, boxes, height, threshold}")?;
    if let Some(k) = sheet.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("unknown sheet field {k}; use {}", KEYS.join(", ")));
    }
    for k in ["path", "view", "clear"] {
        if args.get(k).is_some_and(|v| !v.is_null()) {
            return Err(format!("{k} does not go with sheet (the sheet names its own path and views)"));
        }
    }
    let path = super::eyes::image_path(sheet.get("path").and_then(Value::as_str).ok_or("sheet.path is required: an absolute .png or .jpg")?)?;
    let views: Vec<Slot> = match sheet.get("views").filter(|v| !v.is_null()) {
        None => vec![Slot::Front, Slot::Right, Slot::Back],
        Some(Value::Array(v)) if v.is_empty() || v.len() > MAX_SHEET_VIEWS => return Err(format!("sheet.views takes 1 to {MAX_SHEET_VIEWS} views ({} sent)", v.len())),
        Some(Value::Array(v)) => v.iter().map(|s| {
            let name = s.as_str().ok_or("sheet.views are slot names")?;
            match Slot::parse(name) {
                Some(s @ (Slot::Front | Slot::Back | Slot::Left | Slot::Right)) => Ok(s),
                _ => Err(format!("a sheet view is {SHEET_SLOTS}, not {name}: its ground line is the subject's lowest edge, which top and three-quarter views do not have (load those with path and view)")),
            }
        }).collect::<Result<_, String>>()?,
        Some(_) => return Err("sheet.views must be a list of slot names".into()),
    };
    for (i, v) in views.iter().enumerate() {
        if views[..i].contains(v) {
            return Err(format!("sheet.views names {} twice", v.name()));
        }
    }
    let boxes = match sheet.get("boxes").filter(|v| !v.is_null()) {
        None => None,
        Some(Value::Array(b)) => {
            if b.len() != views.len() {
                return Err(format!("sheet.boxes needs one box per view ({} views, {} boxes)", views.len(), b.len()));
            }
            let list: Vec<[f64; 4]> = b.iter().map(|x| serde_json::from_value::<[f64; 4]>(x.clone()).ok().filter(|b| b.iter().all(|v| v.is_finite() && *v >= 0.0) && b[0] < b[2] && b[1] < b[3]).ok_or("each box is [x0, y0, x1, y1] in sheet pixels, x0 < x1 and y0 < y1")).collect::<Result<_, _>>()?;
            for (i, a) in list.iter().enumerate() {
                if let Some(j) = list[..i].iter().position(|c| a[0] <= c[2] && c[0] <= a[2] && a[1] <= c[3] && c[1] <= a[3]) {
                    return Err(format!("sheet.boxes {j} and {i} overlap"));
                }
            }
            Some(list)
        }
        Some(_) => return Err("sheet.boxes must be a list of [x0, y0, x1, y1]".into()),
    };
    let height = match sheet.get("height").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(v.as_f64().filter(|h| h.is_finite() && *h > 0.0 && *h <= 1e4).ok_or("sheet.height must be a positive number of world units (the subject's height)")? as f32),
    };
    let threshold = match sheet.get("threshold").filter(|v| !v.is_null()) {
        None => 40,
        Some(v) => v.as_f64().filter(|t| (1.0..=255.0).contains(t)).ok_or("sheet.threshold must be 1 to 255")? as u8,
    };
    let planes = match args.get("planes").filter(|v| !v.is_null()) {
        None => true,
        Some(v) => v.as_bool().ok_or("planes must be true or false")?,
    };
    let opacity = match args.get("plane_opacity").filter(|v| !v.is_null()) {
        None => 0.6,
        Some(v) => v.as_f64().filter(|o| (0.0..=1.0).contains(o)).ok_or("plane_opacity must be 0 to 1")? as f32,
    };
    Ok(SheetArgs { path, views, boxes, height, threshold, planes, opacity })
}

/// Decodes a sheet, its size checked before decoding (`SHEET_MAX` a side,
/// `SHEET_MAX_PIXELS`), scaled down to `SPLIT_MAX` on the long side.
/// Returns the image and the factor from sheet pixels to its pixels.
pub(super) fn load_sheet(path: &std::path::Path) -> Result<(image::RgbaImage, f32), String> {
    let open = || image::ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string());
    let (w, h) = open()?.into_dimensions().map_err(|_| format!("{} is not a readable PNG or JPEG image", path.display()))?;
    if w > SHEET_MAX || h > SHEET_MAX || w as u64 * h as u64 > SHEET_MAX_PIXELS {
        return Err(format!("the sheet is {w} x {h}; at most {SHEET_MAX} a side and {SHEET_MAX_PIXELS} pixels"));
    }
    let mut reader = open()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(SHEET_MAX);
    limits.max_image_height = Some(SHEET_MAX);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().map_err(|_| format!("{} is not a readable PNG or JPEG image", path.display()))?.to_rgba8();
    if img.width().max(img.height()) <= SPLIT_MAX {
        return Ok((img, 1.0));
    }
    let k = SPLIT_MAX as f32 / img.width().max(img.height()) as f32;
    let (dw, dh, rgba) = resample(img.as_raw(), img.width() as usize, [0, 0, img.width() as usize - 1, img.height() as usize - 1], k);
    Ok((image::RgbaImage::from_raw(dw as u32, dh as u32, rgba).expect("sized"), k))
}

/// One view cut from the sheet: its plane (image, scale, ground line).
pub(super) struct Cut {
    pub slot: Slot,
    /// Subject box in sheet pixels.
    pub subject: [f32; 4],
    /// The cut's region in sheet pixels.
    pub region: [f32; 4],
    pub plane: RefPlane,
}

/// Cuts each view out of `img` (`k_sheet`: its pixels per sheet pixel)
/// around `boxes` (its pixels), at one scale: `ppu` of its pixels per
/// world unit. Every cut is scaled by the same factor (so the longest
/// fits `refplane::MAX_SIDE`) and spans the tallest subject's height
/// above its own ground line, plus a margin.
pub(super) fn cut(img: &image::RgbaImage, k_sheet: f32, views: &[Slot], boxes: &[PxBox], ppu: f32, threshold: u8, ground_y: f32, opacity: f32, source: &str) -> Vec<Cut> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let tallest = boxes.iter().map(|b| b[3] + 1 - b[1]).max().unwrap_or(1);
    let m = ((tallest as f32 * MARGIN).round() as usize).max(2);
    let regions: Vec<PxBox> = boxes.iter().map(|b| {
        let ground = b[3] + 1;
        [b[0].saturating_sub(m), (ground + m).saturating_sub(tallest + 2 * m), (b[2] + m).min(w - 1), (ground + m).min(h) - 1]
    }).collect();
    let longest = regions.iter().map(|r| (r[2] + 1 - r[0]).max(r[3] + 1 - r[1])).max().unwrap_or(1);
    let k = (crate::refplane::MAX_SIDE as f32 / longest as f32).min(1.0);
    views.iter().zip(boxes).zip(&regions).map(|((&slot, b), r)| {
        let (dw, dh, rgba) = resample(img.as_raw(), w, *r, k);
        let px = |v: usize, o: usize| (v - o) as f32 * k;
        let subject_box = [(px(b[0], r[0]).floor() as u32).min(dw as u32 - 1), (px(b[1], r[1]).floor() as u32).min(dh as u32 - 1), ((px(b[2] + 1, r[0]).ceil() as u32).saturating_sub(1)).min(dw as u32 - 1), ((px(b[3] + 1, r[1]).ceil() as u32).saturating_sub(1)).min(dh as u32 - 1)];
        let plane = RefPlane {
            slot: slot.name().into(),
            image: crate::refplane::Image::new(dw as u32, dh as u32, rgba).expect("cut within MAX_SIDE"),
            px_per_unit: ppu * k,
            ground_row: px(b[3] + 1, r[1]),
            center_col: (px(b[0], r[0]) + px(b[2] + 1, r[0])) * 0.5,
            subject_box,
            threshold,
            ground_y,
            opacity,
            source: source.into(),
        };
        let s = |v: usize| v as f32 / k_sheet;
        Cut { slot, subject: [s(b[0]), s(b[1]), s(b[2] + 1) - 1.0, s(b[3] + 1) - 1.0], region: [s(r[0]), s(r[1]), s(r[2] + 1) - 1.0, s(r[3] + 1) - 1.0], plane }
    }).collect()
}

/// The sheet with its views' boxes drawn (green: subject, white: cut),
/// at most 768 on the long side, for the reply.
fn split_image(img: &image::RgbaImage, boxes: &[PxBox], regions: &[[f32; 4]], k_sheet: f32) -> egui::ColorImage {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut px: Vec<egui::Color32> = img.pixels().map(|p| egui::Color32::from_rgb(p[0], p[1], p[2])).collect();
    let mut rect = |b: [usize; 4], c: egui::Color32, t: usize| {
        for y in b[1]..=b[3].min(h - 1) {
            for x in b[0]..=b[2].min(w - 1) {
                if x < b[0] + t || x + t > b[2] || y < b[1] + t || y + t > b[3] {
                    px[y * w + x] = c;
                }
            }
        }
    };
    let t = (w.max(h) / 400).max(1);
    for r in regions {
        let r = r.map(|v| (v * k_sheet) as usize);
        rect(r, egui::Color32::WHITE, t);
    }
    for b in boxes {
        rect(*b, egui::Color32::from_rgb(40, 220, 90), t);
    }
    super::tools::downscale(&egui::ColorImage::new([w, h], px), 768)
}

/// File > Load Turnaround Sheet…'s request: the sheet split automatically
/// into front, right and back, at the model's height, with planes (what an
/// agent sends for the same sheet).
pub(super) fn sheet_request(path: &std::path::Path) -> Value {
    json!({ "sheet": { "path": path.display().to_string() } })
}

impl ClideApp {
    /// File > Load Turnaround Sheet…: `sheet_request` through the same
    /// handler as an agent's call.
    pub(super) fn sheet_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg"]).pick_file() else { return };
        self.send_ui_call("set_reference", sheet_request(&path));
    }

    /// `set_reference {sheet}` (called at rest): splits the sheet, fills
    /// its views' slots at the shared scale, and places the image planes
    /// (one undo step with the scene change).
    pub(super) fn set_reference_sheet(&mut self, args: &Value) -> ToolResult {
        let a = parse(args)?;
        let t = std::time::Instant::now();
        let (img, k_sheet) = load_sheet(&a.path)?;
        let (w, h) = (img.width() as usize, img.height() as usize);
        let color = egui::ColorImage::from_rgba_unmultiplied([w, h], img.as_raw());
        let (mask, mode) = super::eyes::subject_mask(&color, a.threshold);
        let mut captions: Vec<PxBox> = Vec::new();
        let (boxes, found_by) = match &a.boxes {
            None => (split(&mask, w, h, a.views.len()).map(|(b, c)| {
                captions = c;
                b
            }).map_err(|found| {
                format!("the sheet splits into {} views, not {} ({}): pass sheet.boxes, one [x0, y0, x1, y1] per view in sheet pixels, or another threshold; found (sheet pixels): {}",
                    found.len(), a.views.len(), a.views.iter().map(|v| v.name()).collect::<Vec<_>>().join(", "),
                    json!(found.iter().map(|b| b.map(|v| (v as f32 / k_sheet).round())).collect::<Vec<_>>()))
            })?, "automatic"),
            Some(list) => {
                let (sw, sh) = (w as f64 / k_sheet as f64, h as f64 / k_sheet as f64);
                if let Some(i) = list.iter().position(|b| b[2] >= sw || b[3] >= sh) {
                    return Err(format!("sheet.boxes {i} reaches past the sheet ({sw:.0} x {sh:.0} pixels)"));
                }
                let mut out = Vec::new();
                for (i, b) in list.iter().enumerate() {
                    let q = [b[0] * k_sheet as f64, b[1] * k_sheet as f64, b[2] * k_sheet as f64, b[3] * k_sheet as f64].map(|v| (v as usize).min(w.max(h)));
                    let inside = (q[1]..=q[3].min(h - 1)).flat_map(|y| (q[0]..=q[2].min(w - 1)).map(move |x| (x, y))).filter(|&(x, y)| mask[y * w + x]);
                    let found = inside.fold(None, |acc: Option<PxBox>, (x, y)| Some(match acc { None => [x, y, x, y], Some(c) => [c[0].min(x), c[1].min(y), c[2].max(x), c[3].max(y)] }));
                    out.push(found.ok_or(format!("sheet.boxes {i} holds no subject pixels (threshold {})", a.threshold))?);
                }
                (out, "boxes")
            }
        };
        let split_ms = t.elapsed().as_secs_f64() * 1e3;
        // The model at rest, if any: its height (the default) and ground.
        // Planes are left out at any depth (`world_bounds_of_roots`), the
        // objects under them kept.
        let roots = self.scene.children(None);
        self.meshes.sync(&self.scene);
        let model = self.world_bounds_of_roots(&roots);
        let height = match (a.height, model) {
            (Some(h), _) => h,
            (None, Some((lo, hi))) if hi.y - lo.y > 1e-6 => hi.y - lo.y,
            _ => return Err("nothing in the scene to take the subject's height from: pass sheet.height (the subject's height in world units)".into()),
        };
        let tallest = boxes.iter().map(|b| b[3] + 1 - b[1]).max().expect("a view") as f32;
        let ppu = tallest / height;
        let ground_y = model.map_or(0.0, |(lo, _)| lo.y);
        let cuts = cut(&img, k_sheet, &a.views, &boxes, ppu, a.threshold, ground_y, a.opacity, &a.path.to_string_lossy());
        // Planes: replace the ones of these slots.
        let names: Vec<&str> = a.views.iter().map(|v| v.name()).collect();
        let stale: Vec<u64> = self.scene.objects.iter().filter(|o| o.reference.as_ref().is_some_and(|r| names.contains(&r.slot.as_str()))).map(|o| o.id).collect();
        for id in stale {
            self.scene.remove_reference_plane(id);
        }
        let (center, back) = match model {
            Some((lo, hi)) => ((lo + hi) * 0.5, (hi - lo).length() * 0.6 + 0.01),
            None => (Vec3::ZERO, height),
        };
        let mut views = Vec::new();
        for c in &cuts {
            let mut r = reference_of(&c.plane).expect("a fresh cut is sane");
            let plane_id = if a.planes {
                let id = self.scene.add(Shape::Plane);
                let (origin, turn) = RefPlane::placement(c.slot.camera_dir(), center, ground_y, back);
                let o = self.scene.get_mut(id).expect("added");
                o.name = format!("Reference {}", c.slot.name());
                o.translation = origin;
                o.rotation = turn;
                o.scale = Vec3::ONE;
                o.color = [1.0; 3];
                o.reference = Some(c.plane.clone());
                let name = o.name.clone();
                Some((id, name))
            } else {
                None
            };
            r.sheet.as_mut().expect("sheet slot").plane = plane_id.as_ref().map(|p| p.0);
            let subject_h = (r.bbox()[3] + 1 - r.bbox()[1]) as f32 / c.plane.px_per_unit;
            views.push(json!({
                "view": c.slot.name(),
                "subject_box_px": c.subject.map(|v| v.round()),
                "cut_px": c.region.map(|v| v.round()),
                "ground_line_px": c.subject[3] + 1.0,
                "px_per_unit": round(ppu / k_sheet),
                "image_px_per_unit": round(c.plane.px_per_unit),
                "image_size": c.plane.image.size(),
                "subject_height": round(subject_h),
                "plane": plane_id.map(|p| p.1),
            }));
            self.references.insert(r);
        }
        self.selection.retain(|id| self.scene.get(*id).is_some_and(|o| !o.is_reference()));
        let regions: Vec<[f32; 4]> = cuts.iter().map(|c| c.region).collect();
        let preview = split_image(&img, &boxes, &regions, k_sheet);
        let top = boxes.iter().map(|b| b[1]).min().expect("a view") as f32 / k_sheet;
        let info = json!({
            "sheet": a.path,
            "size": [(w as f32 / k_sheet).round(), (h as f32 / k_sheet).round()],
            "found_by": found_by,
            "mask_from": mode,
            "views": views,
            "px_per_unit": round(ppu / k_sheet),
            "height": round(height),
            "height_from": if a.height.is_some() { "sheet.height" } else { "the model's height at rest" },
            "ground_y": round(ground_y),
            "top_line_px": top,
            "tallest_px": round(tallest / k_sheet),
            "planes": a.planes,
            "split_ms": round(split_ms as f32),
            "captions_left_out": captions.iter().map(|b| b.map(|v| (v as f32 / k_sheet).round())).collect::<Vec<_>>(),
            "slots": self.references.loaded().iter().map(|s| s.name()).collect::<Vec<_>>(),
            "note": "Every view shares px_per_unit (the tallest subject over height) and stands on its own ground line; compare_reference and quality_report line the model up once at this scale for all of them. The image shows the split: green the subjects, white the cuts.",
        });
        let mut info = info;
        if !captions.is_empty() {
            info["warning"] = json!(format!("{} caption-like part(s) under or over the views were left out of their boxes (captions_left_out, sheet pixels); if one is part of the subject, pass sheet.boxes", captions.len()));
        }
        let mut out = super::tools::text(info);
        out.extend(super::tools::encode_png(&preview)?);
        Ok(out)
    }

    /// World bounds (at rest, as drawn) of `roots` and everything under
    /// them, reference planes left out.
    pub(super) fn world_bounds_of_roots(&self, roots: &[u64]) -> Option<(Vec3, Vec3)> {
        let bounds = |o: &crate::scene::Object| if o.is_reference() { None } else { self.meshes.bounds(o) };
        roots.iter().filter(|&&id| self.scene.is_visible(id)).filter_map(|&id| self.scene.subtree_bounds(id, self.scene.world_matrix(id), &bounds)).reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
    }

    /// Fills the slots from the scene's reference planes (open, new): a
    /// saved sheet's views come back as they were loaded; slots that came
    /// from planes no longer in the scene go.
    pub(super) fn references_from_planes(&mut self) {
        self.references.drop_planes();
        let planes = self.current_planes();
        self.plane_basis = planes.clone();
        for (id, p) in planes {
            if let Some(mut r) = reference_of(&p) {
                r.sheet.as_mut().expect("sheet slot").plane = Some(id);
                self.references.insert_from_plane(r);
            }
        }
    }
}

impl ClideApp {
    /// The scene's reference planes, in order (cheap: images are shared).
    fn current_planes(&self) -> Vec<(u64, RefPlane)> {
        self.scene.objects.iter().filter_map(|o| o.reference.clone().map(|r| (o.id, r))).collect()
    }

    /// The slots match the planes as they are now (a handler that set
    /// both together).
    pub(super) fn planes_synced(&mut self) {
        self.plane_basis = self.current_planes();
    }

    /// Refills the sheet slots from the planes when the planes changed
    /// since the slots were made: undo, redo, history goto, a plane
    /// deleted or edited. Slots of single images stay.
    pub(super) fn sync_reference_slots(&mut self) {
        let now = self.current_planes();
        if now != self.plane_basis {
            self.references_from_planes();
        }
    }
}

/// A plane's view as a reference slot: its image, the subject inside its
/// box, and the sheet's fit. None for a damaged plane.
pub(super) fn reference_of(p: &RefPlane) -> Option<Reference> {
    if !p.is_sane() {
        return None;
    }
    let slot = Slot::parse(&p.slot)?;
    let image = p.image.color_image();
    let [w, _] = image.size;
    let (mask, _) = super::eyes::subject_mask(&image, p.threshold);
    let b = p.subject_box.map(|v| v as usize);
    let mask: Vec<bool> = mask.iter().enumerate().map(|(i, &m)| m && (b[0]..=b[2]).contains(&(i % w)) && (b[1]..=b[3]).contains(&(i / w))).collect();
    let fit = SheetFit { px_per_unit: p.px_per_unit, ground_row: p.ground_row, ground_y: p.ground_y, plane: None };
    Some(Reference::new(image, slot, mask, b, std::path::PathBuf::from(&p.source), Some(fit)))
}

fn round(x: f32) -> f64 {
    (x as f64 * 1e4).round() / 1e4
}

/// Synthetic turnaround sheets for tests: a character of a box body
/// (0.4 wide, `depth` deep, 0.6 tall, standing on y = 0) and a ball head
/// (radius 0.18 at y = 0.75; 0.93 tall in all), drawn front, right and
/// back on one sheet at `ppu` pixels per unit, `spacing` units apart,
/// dark on light, with a caption under each view when `labels`.
#[cfg(test)]
pub(super) mod synthetic {
    use super::PxBox;

    pub const HEIGHT: f32 = 0.93;
    pub const WIDTH: f32 = 0.4;
    pub const DEPTH: f32 = 0.25;

    /// The sheet and each view's true subject box (from its own pixels).
    pub fn sheet(ppu: f32, spacing: f32, depth: f32, labels: bool) -> (image::RgbaImage, Vec<PxBox>) {
        let halves = [WIDTH * 0.5, depth * 0.5, WIDTH * 0.5];
        let w = ((spacing * 3.0) * ppu).ceil() as u32;
        let ground = (1.25 * ppu).round();
        let h = (1.45 * ppu).ceil() as u32;
        let mut img = image::RgbaImage::from_pixel(w, h, image::Rgba([232, 230, 226, 255]));
        let mut boxes = Vec::new();
        for (i, half) in halves.iter().enumerate() {
            let cx = (spacing * (i as f32 + 0.5)) * ppu;
            let mut b: Option<PxBox> = None;
            for y in 0..h {
                for x in 0..w {
                    let u = (x as f32 + 0.5 - cx) / ppu;
                    let v = (ground - (y as f32 + 0.5)) / ppu;
                    let body = u.abs() <= *half && (0.0..=0.6).contains(&v);
                    let head = u * u + (v - 0.75) * (v - 0.75) <= 0.18 * 0.18;
                    if body || head {
                        img.put_pixel(x, y, image::Rgba([40, 44, 70, 255]));
                        let (x, y) = (x as usize, y as usize);
                        b = Some(b.map_or([x, y, x, y], |c| [c[0].min(x), c[1].min(y), c[2].max(x), c[3].max(y)]));
                    }
                }
            }
            boxes.push(b.expect("a view"));
            if labels {
                // A caption: a few letter blocks under the view.
                let (lx, ly) = ((cx - 0.12 * ppu) as u32, (ground + 0.08 * ppu) as u32);
                for letter in 0..5 {
                    for y in ly..ly + (0.05 * ppu) as u32 {
                        for x in lx + letter * (0.05 * ppu) as u32..lx + letter * (0.05 * ppu) as u32 + (0.035 * ppu) as u32 {
                            img.put_pixel(x, y, image::Rgba([30, 30, 30, 255]));
                        }
                    }
                }
            }
        }
        (img, boxes)
    }

    /// A big title (0.6 wide, 0.2 tall: past the label filter's 15%)
    /// over each view of a `sheet(ppu, spacing, ..)`, 0.05 above its head.
    pub fn big_captions(img: &mut image::RgbaImage, ppu: f32, spacing: f32) {
        let ground = (1.25 * ppu).round();
        for i in 0..3 {
            let cx = (spacing * (i as f32 + 0.5)) * ppu;
            let (x0, y0) = ((cx - 0.3 * ppu) as u32, (ground - 1.18 * ppu) as u32);
            for y in y0..y0 + (0.2 * ppu) as u32 {
                for x in x0..x0 + (0.6 * ppu) as u32 {
                    img.put_pixel(x, y, image::Rgba([30, 30, 30, 255]));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask_of(img: &image::RgbaImage) -> (Vec<bool>, usize, usize) {
        let (w, h) = (img.width() as usize, img.height() as usize);
        let color = egui::ColorImage::from_rgba_unmultiplied([w, h], img.as_raw());
        (super::super::eyes::subject_mask(&color, 40).0, w, h)
    }

    /// `sheet_split`: three views of known boxes on one sheet, each with a
    /// caption under it, split automatically: every box within 2 pixels
    /// (exactly, here), captions ignored; a 4K sheet the same, timed.
    #[test]
    fn sheet_split_finds_the_panels() {
        for (ppu, labels) in [(400.0, true), (400.0, false), (1000.0, true)] {
            let (img, truth) = synthetic::sheet(ppu, 1.25, synthetic::DEPTH, labels);
            let t = std::time::Instant::now();
            let (mask, w, h) = mask_of(&img);
            let found = split(&mask, w, h, 3).expect("three views").0;
            let ms = t.elapsed().as_secs_f64() * 1e3;
            eprintln!("{w} x {h} sheet (labels {labels}): split in {ms:.0} ms, boxes {found:?}");
            for (f, t) in found.iter().zip(&truth) {
                for c in 0..4 {
                    assert!(f[c].abs_diff(t[c]) <= 2, "{f:?} against {t:?}");
                }
            }
        }
        // Big titles (a fifth of the subject's height, past the label
        // filter) are left out of the views' boxes and reported, so the
        // ground line and the scale stay the subjects'.
        let (mut img, truth) = synthetic::sheet(400.0, 1.25, synthetic::DEPTH, false);
        synthetic::big_captions(&mut img, 400.0, 1.25);
        let (mask, w, h) = mask_of(&img);
        let (found, captions) = split(&mask, w, h, 3).expect("three views");
        assert_eq!(found, truth, "the captions stay out of the boxes");
        assert_eq!(captions.len(), 3, "{captions:?}");
        // The wrong count of views: the boxes it did find.
        let (img, _) = synthetic::sheet(400.0, 1.25, synthetic::DEPTH, true);
        let (mask, w, h) = mask_of(&img);
        assert_eq!(split(&mask, w, h, 2).unwrap_err().len(), 3);
    }

    /// Every cut shares one scale: the subjects (all 0.93 tall) are the
    /// same height in world units to within a pixel, each cut is the same
    /// height, the ground row is each subject's lowest edge, and a sheet
    /// too big for `MAX_SIDE` is scaled by one factor for every view.
    #[test]
    fn cuts_share_one_scale() {
        for (ppu, spacing) in [(400.0, 1.25), (1300.0, 1.25)] {
            let (img, truth) = synthetic::sheet(ppu, spacing, synthetic::DEPTH, true);
            let views = [Slot::Front, Slot::Right, Slot::Back];
            let tallest = truth.iter().map(|b| b[3] + 1 - b[1]).max().unwrap() as f32;
            let cuts = cut(&img, 1.0, &views, &truth, tallest / synthetic::HEIGHT, 40, 0.0, 1.0, "");
            let k = cuts[0].plane.px_per_unit / (tallest / synthetic::HEIGHT);
            let mut heights = Vec::new();
            for c in &cuts {
                let r = reference_of(&c.plane).expect("sane");
                let b = r.bbox();
                let subject = (b[3] + 1 - b[1]) as f32 / c.plane.px_per_unit;
                eprintln!("{}: image {:?}, {} px a unit, subject {subject:.4} tall (truth {}), ground row {} (subject's bottom edge {})", c.slot.name(), c.plane.image.size(), c.plane.px_per_unit, synthetic::HEIGHT, c.plane.ground_row, b[3] + 1);
                assert_eq!(c.plane.px_per_unit, cuts[0].plane.px_per_unit);
                // Each measured edge may take in one partly covered pixel.
                assert!((subject - synthetic::HEIGHT).abs() <= 2.0 / c.plane.px_per_unit, "{subject}");
                heights.push(subject);
                assert!((c.plane.ground_row - (b[3] + 1) as f32).abs() <= 1.0);
                assert_eq!(c.plane.image.size()[1], cuts[0].plane.image.size()[1]);
                assert!(c.plane.image.size().iter().all(|s| *s as u32 <= crate::refplane::MAX_SIDE));
            }
            let spread = heights.iter().cloned().fold(f32::MIN, f32::max) - heights.iter().cloned().fold(f32::MAX, f32::min);
            eprintln!("ppu {ppu}: one factor {k:.4} for every view; subject heights within {spread:.5} of each other");
            // The views line up to within a pixel of each other.
            assert!(spread <= 1.0 / cuts[0].plane.px_per_unit, "{heights:?}");
        }
    }

    /// The resample keeps edges where the factor puts them, and is the
    /// identity at 1.
    #[test]
    fn resample_is_exact_at_one_and_area_weighted() {
        let img: Vec<u8> = (0..16 * 4).map(|i| (i * 3) as u8).collect();
        let (w, h, out) = resample(&img, 4, [0, 0, 3, 3], 1.0);
        assert_eq!((w, h), (4, 4));
        assert_eq!(out, img);
        let half: Vec<u8> = (0..4 * 4).flat_map(|i| if i % 4 < 2 { [0u8, 0, 0, 255] } else { [200, 200, 200, 255] }).collect();
        let (w, _, out) = resample(&half, 4, [0, 0, 3, 3], 0.5);
        assert_eq!(w, 2);
        assert_eq!(&out[..8], &[0, 0, 0, 255, 200, 200, 200, 255]);
    }

    /// A 4K sheet from its file: decoded, masked, split and cut (timed,
    /// printed); a sheet past `SPLIT_MAX` is split scaled down, its boxes
    /// still within 2 sheet pixels once scaled back.
    #[test]
    fn sheet_from_a_file_at_4k_and_past_the_split_size() {
        let dir = std::env::temp_dir().join(format!("clide3d_sheet_4k_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for ppu in [1092.0, 1400.0] {
            let (img, truth) = synthetic::sheet(ppu, 1.25, synthetic::DEPTH, true);
            let p = dir.join(format!("sheet_{ppu}.png"));
            img.save(&p).unwrap();
            let t = std::time::Instant::now();
            let (loaded, k) = load_sheet(&p).unwrap();
            let decoded = t.elapsed().as_secs_f64() * 1e3;
            let (mask, w, h) = mask_of(&loaded);
            let found = split(&mask, w, h, 3).unwrap().0;
            let tallest = found.iter().map(|b| b[3] + 1 - b[1]).max().unwrap() as f32;
            let cuts = cut(&loaded, k, &[Slot::Front, Slot::Right, Slot::Back], &found, tallest / synthetic::HEIGHT, 40, 0.0, 1.0, "");
            let all = t.elapsed().as_secs_f64() * 1e3;
            eprintln!("{} x {} sheet: decoded (and scaled by {k:.3}) in {decoded:.0} ms, masked, split and cut in {:.0} ms more ({all:.0} ms in all); cuts {:?}", img.width(), img.height(), all - decoded, cuts.iter().map(|c| c.plane.image.size()).collect::<Vec<_>>());
            for (c, t) in cuts.iter().zip(&truth) {
                for i in 0..4 {
                    assert!((c.subject[i] - t[i] as f32).abs() <= 2.0 / k, "{:?} against {t:?}", c.subject);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The arguments are checked before anything is read.
    #[test]
    fn sheet_arguments_are_bounded() {
        let dir = std::env::temp_dir().join(format!("clide3d_sheet_args_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 255])).save(&p).unwrap();
        let path = p.to_string_lossy().to_string();
        let bad = |sheet: Value, extra: Value, says: &str| {
            let mut args = json!({ "sheet": sheet });
            for (k, v) in extra.as_object().unwrap() {
                args[k] = v.clone();
            }
            let e = parse(&args).err().unwrap_or_else(|| panic!("{args} was accepted"));
            assert!(e.contains(says), "{args}: {e}");
        };
        bad(json!({ "path": "relative.png" }), json!({}), "absolute");
        bad(json!({ "path": path, "views": ["front", "top"] }), json!({}), "not top");
        bad(json!({ "path": path, "views": ["front", "three_quarter"] }), json!({}), "not three_quarter");
        bad(json!({ "path": path, "views": ["front", "front"] }), json!({}), "twice");
        bad(json!({ "path": path, "views": ["front", "back", "left", "right", "front", "back", "left"] }), json!({}), "1 to 6");
        bad(json!({ "path": path, "boxes": [[0, 0, 4, 4]] }), json!({}), "one box per view");
        bad(json!({ "path": path, "views": ["front", "back"], "boxes": [[0, 0, 4, 4], [3, 3, 6, 6]] }), json!({}), "overlap");
        bad(json!({ "path": path, "views": ["front"], "boxes": [[4, 0, 2, 4]] }), json!({}), "x0 < x1");
        bad(json!({ "path": path, "height": -1 }), json!({}), "positive");
        bad(json!({ "path": path, "threshold": 0 }), json!({}), "1 to 255");
        bad(json!({ "path": path, "colour": 1 }), json!({}), "unknown sheet field");
        bad(json!({ "path": path }), json!({ "plane_opacity": 2 }), "0 to 1");
        bad(json!({ "path": path }), json!({ "path": path }), "does not go with sheet");
        bad(json!({ "path": path }), json!({ "view": "front" }), "does not go with sheet");
        assert!(parse(&json!({ "sheet": { "path": path } })).is_ok());
        // A sheet past the size bound is refused from its header.
        let big = dir.join("big.png");
        image::RgbaImage::from_pixel(SHEET_MAX + 1, 1, image::Rgba([0, 0, 0, 255])).save(&big).unwrap();
        let e = load_sheet(&big).unwrap_err();
        assert!(e.contains("at most 8192"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
