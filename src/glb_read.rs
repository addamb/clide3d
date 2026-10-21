//! Loads binary glTF from disk for export checks, independently of the editor.

use std::{collections::HashSet, path::Path, sync::Arc};

use glam::{Mat3, Mat4, Quat, Vec3};
use serde_json::{Value, json};

use crate::{film, material::Material, mesh::{ImportedMaterial, Mesh, Textures, Vertex}};

pub struct Loaded {
    pub items: Vec<film::Item>,
    pub summary: Value,
    pub bounds: (Vec3, Vec3),
}

type Attribute = Vec<Vec<f32>>;
type TextureSlot = Option<(usize, [u32; 2])>;
type ReadMaterial = (Material, [f32; 4], Option<Arc<Textures>>, ImportedMaterial);

/// Limits on what one file may make the reader allocate in all (each
/// accessor and image is also checked on its own).
const MAX_IMAGES: usize = 64;
const MAX_IMAGE_SIDE: u32 = 8192;
const MAX_TOTAL_PIXELS: u64 = 128 * 1024 * 1024;
/// Bytes of decoded texture sets over all materials.
const MAX_TEXTURE_BYTES: u64 = 1536 * 1024 * 1024;
const MAX_MATERIALS: usize = 256;
/// Vertices and triangles over every mesh instance (a mesh used by many
/// nodes counts each time: each is expanded).
const MAX_INSTANCED: usize = 20_000_000;

struct Reader {
    doc: Value,
    bin: Vec<u8>,
    images: Vec<image::RgbaImage>,
    warnings: Vec<String>,
    /// Largest side a material's texture set is read at (smaller when big
    /// exports would not fit in memory: a check needs no full 8K maps).
    cap: u32,
    texture_cache: std::cell::RefCell<std::collections::HashMap<String,Arc<Textures>>>,
}

fn list<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v[key].as_array().map_or(&[], Vec::as_slice)
}
fn index(v: &Value) -> Result<usize, String> {
    v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| "expected an unsigned index".into())
}
fn number(v: &Value, default: f32) -> f32 {
    v.as_f64().map_or(default, |n| n as f32)
}
fn array<const N: usize>(v: &Value, default: [f32; N]) -> Result<[f32; N], String> {
    if v.is_null() { return Ok(default); }
    let a = v.as_array().filter(|a| a.len() == N).ok_or("invalid vector length")?;
    let mut out = default;
    for (dst, src) in out.iter_mut().zip(a) {
        *dst = src.as_f64().ok_or("invalid vector component")? as f32;
        if !dst.is_finite() { return Err("non-finite transform or material value".into()); }
    }
    Ok(out)
}
fn entry<'a>(doc: &'a Value, key: &str, i: usize) -> Result<&'a Value, String> {
    list(doc, key).get(i).ok_or_else(|| format!("{key} index {i} out of range"))
}

impl Reader {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let u32_at = |at: usize| -> Result<u32, String> {
            let b = bytes.get(at..at + 4).ok_or("truncated GLB")?;
            Ok(u32::from_le_bytes(b.try_into().unwrap()))
        };
        if bytes.get(..4) != Some(b"glTF") || u32_at(4)? != 2 { return Err("expected a glTF 2.0 GLB".into()); }
        if u32_at(8)? as usize != bytes.len() { return Err("GLB length does not match the file".into()); }
        let (mut doc, mut bin) = (None, None);
        let mut at = 12;
        while at < bytes.len() {
            let len = u32_at(at)? as usize;
            let ty = u32_at(at + 4)?;
            let end = at.checked_add(8).and_then(|n| n.checked_add(len)).ok_or("chunk overflow")?;
            let data = bytes.get(at + 8..end).ok_or("truncated GLB chunk")?;
            if !len.is_multiple_of(4) { return Err("GLB chunk is not aligned".into()); }
            match ty {
                0x4e4f534a if doc.is_none() && at == 12 => doc = Some(serde_json::from_slice::<Value>(data).map_err(|e| e.to_string())?),
                0x004e4942 if bin.is_none() => bin = Some(data.to_vec()),
                0x4e4f534a | 0x004e4942 => return Err("duplicate or misplaced GLB chunk".into()),
                _ => {},
            }
            at = end;
        }
        let doc = doc.ok_or("GLB has no JSON chunk")?;
        if doc["asset"]["version"] != "2.0" { return Err("unsupported glTF version".into()); }
        let bin = bin.unwrap_or_default();
        if list(&doc, "buffers").len() != 1 || !doc["buffers"][0]["uri"].is_null() { return Err("only a GLB's embedded buffer is supported".into()); }
        let len = index(&doc["buffers"][0]["byteLength"])?;
        if len > bin.len() || bin.len() - len > 3 { return Err("invalid embedded buffer length".into()); }
        let mut r = Self { doc, bin: bin[..len].to_vec(), images: Vec::new(), warnings: Vec::new(), cap: 4096, texture_cache: Default::default() };
        for ext in list(&r.doc, "extensionsRequired") {
            if !matches!(ext.as_str(), Some("KHR_materials_clearcoat" | "KHR_materials_sheen" | "KHR_materials_emissive_strength" | "KHR_materials_transmission" | "KHR_materials_ior")) {
                return Err(format!("unsupported required extension: {ext}"));
            }
        }
        let images = list(&r.doc, "images");
        if images.len() > MAX_IMAGES { return Err(format!("more than {MAX_IMAGES} images")); }
        let mut pixels = 0u64;
        for img in images {
            let bytes = if !img["bufferView"].is_null() {
                r.view(index(&img["bufferView"])?)?.to_vec()
            } else if let Some(uri) = img["uri"].as_str() {
                use base64::Engine;
                let (header, data) = uri.split_once(',').ok_or("invalid image data URI")?;
                if !header.starts_with("data:image/") || !header.ends_with(";base64") { return Err("only embedded PNG/JPEG images are supported".into()); }
                base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| e.to_string())?
            } else { return Err("image has no embedded data".into()); };
            let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
            if !matches!(decoder.format(), Some(image::ImageFormat::Png | image::ImageFormat::Jpeg)) { return Err("only PNG/JPEG images are supported".into()); }
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(MAX_IMAGE_SIDE);
            limits.max_image_height = Some(MAX_IMAGE_SIDE);
            limits.max_alloc = Some(MAX_IMAGE_SIDE as u64 * MAX_IMAGE_SIDE as u64 * 8);
            decoder.limits(limits);
            let mut image = decoder.decode().map_err(|e| format!("image decode: {e}"))?.to_rgba8();
            // A check needs no 8K maps: read them at 4K (the file keeps them).
            if image.width().max(image.height()) > 4096 {
                let k = 4096.0 / image.width().max(image.height()) as f32;
                let (w, h) = (((image.width() as f32 * k).round() as u32).max(1), ((image.height() as f32 * k).round() as u32).max(1));
                image = image::imageops::resize(&image, w, h, image::imageops::FilterType::Triangle);
                if !r.warnings.iter().any(|w| w.starts_with("images larger than 4096")) {
                    r.warnings.push("images larger than 4096 px were read at 4096 for this check (the file keeps its full size)".into());
                }
            }
            pixels += image.width() as u64 * image.height() as u64;
            if pixels > MAX_TOTAL_PIXELS { return Err(format!("images hold more than {MAX_TOTAL_PIXELS} pixels in all")); }
            r.images.push(image);
        }
        Ok(r)
    }

    fn view(&self, i: usize) -> Result<&[u8], String> {
        let v = entry(&self.doc, "bufferViews", i)?;
        if index(&v["buffer"])? != 0 { return Err("external buffer is unsupported".into()); }
        let start = v["byteOffset"].as_u64().unwrap_or(0) as usize;
        let end = start.checked_add(index(&v["byteLength"])?).ok_or("buffer view overflow")?;
        self.bin.get(start..end).ok_or_else(|| "buffer view exceeds BIN chunk".into())
    }

    /// Accessors may be interleaved, normalized integer, or sparse.
    fn accessor(&self, i: usize) -> Result<Attribute, String> {
        let a = entry(&self.doc, "accessors", i)?;
        let width = match a["type"].as_str() {
            Some("SCALAR") => 1, Some("VEC2") => 2, Some("VEC3") => 3, Some("VEC4") => 4, Some("MAT4") => 16,
            _ => return Err("unsupported accessor type".into()),
        };
        let component = index(&a["componentType"])?;
        let size = match component { 5120 | 5121 => 1, 5122 | 5123 => 2, 5125 | 5126 => 4, _ => return Err("unsupported component type".into()) };
        let count = index(&a["count"])?;
        if count > 20_000_000 || count.saturating_mul(width) > 80_000_000 { return Err("accessor too large".into()); }
        let normalized = a["normalized"].as_bool().unwrap_or(false);
        let read = |data: &[u8], at: usize| -> Result<Vec<f32>, String> {
            let end = at.checked_add(width * size).ok_or("accessor overflow")?;
            let data = data.get(at..end).ok_or("accessor exceeds buffer view")?;
            Ok(data.chunks_exact(size).map(|b| {
                let n = match component {
                    5120 => (b[0] as i8) as f32, 5121 => b[0] as f32,
                    5122 => i16::from_le_bytes(b.try_into().unwrap()) as f32,
                    5123 => u16::from_le_bytes(b.try_into().unwrap()) as f32,
                    5125 => u32::from_le_bytes(b.try_into().unwrap()) as f32,
                    _ => f32::from_le_bytes(b.try_into().unwrap()),
                };
                if normalized { match component { 5120 => (n / 127.0).max(-1.0), 5121 => n / 255.0, 5122 => (n / 32767.0).max(-1.0), 5123 => n / 65535.0, _ => n } } else { n }
            }).collect())
        };
        let mut out = if a["bufferView"].is_null() { vec![vec![0.0; width]; count] } else {
            let vi = index(&a["bufferView"])?;
            let data = self.view(vi)?;
            let stride = self.doc["bufferViews"][vi]["byteStride"].as_u64().unwrap_or((size * width) as u64) as usize;
            if stride < size * width { return Err("accessor stride is too small".into()); }
            let offset = a["byteOffset"].as_u64().unwrap_or(0) as usize;
            (0..count).map(|j| {
                let at = j.checked_mul(stride).and_then(|n| n.checked_add(offset)).ok_or("accessor offset overflow")?;
                read(data, at)
            }).collect::<Result<Vec<_>, _>>()?
        };
        let s = &a["sparse"];
        if !s.is_null() {
            let n = index(&s["count"])?;
            if n > count { return Err("sparse count exceeds accessor".into()); }
            let ids = self.view(index(&s["indices"]["bufferView"])?)?;
            let values = self.view(index(&s["values"]["bufferView"])?)?;
            let ic = index(&s["indices"]["componentType"])?;
            let is = match ic { 5121 => 1, 5123 => 2, 5125 => 4, _ => return Err("invalid sparse index type".into()) };
            let io = s["indices"]["byteOffset"].as_u64().unwrap_or(0) as usize;
            let vo = s["values"]["byteOffset"].as_u64().unwrap_or(0) as usize;
            let mut prev = None;
            for j in 0..n {
                let at = j.checked_mul(is).and_then(|n| n.checked_add(io)).ok_or("sparse offset overflow")?;
                let b = ids.get(at..at + is).ok_or("truncated sparse indices")?;
                let id = match is { 1 => b[0] as usize, 2 => u16::from_le_bytes(b.try_into().unwrap()) as usize, _ => u32::from_le_bytes(b.try_into().unwrap()) as usize };
                if id >= count || prev.is_some_and(|p| p >= id) { return Err("invalid sparse index order or range".into()); }
                prev = Some(id);
                let at = j.checked_mul(width * size).and_then(|n| n.checked_add(vo)).ok_or("sparse offset overflow")?;
                out[id] = read(values, at)?;
            }
        }
        Ok(out)
    }

    fn attribute(&self, p: &Value, name: &str, n: usize, widths: &[usize]) -> Result<Option<Attribute>, String> {
        let Some(i) = p["attributes"][name].as_u64() else { return Ok(None) };
        let data = self.accessor(i as usize)?;
        if data.len() != n || data.iter().any(|v| !widths.contains(&v.len())) { return Err(format!("{name} has invalid shape")); }
        Ok(Some(data))
    }

    fn texture(&self, info: &Value) -> Result<TextureSlot, String> {
        if info.is_null() { return Ok(None); }
        if info["texCoord"].as_u64().unwrap_or(0) != 0 || !info["extensions"].is_null() { return Err("only untransformed TEXCOORD_0 textures are supported".into()); }
        let t = entry(&self.doc, "textures", index(&info["index"])?)?;
        let source = index(&t["source"])?;
        if source >= self.images.len() { return Err("texture source out of range".into()); }
        let s = if t["sampler"].is_null() { &Value::Null } else { entry(&self.doc, "samplers", index(&t["sampler"])?)? };
        Ok(Some((source, [s["wrapS"].as_u64().unwrap_or(10497) as u32, s["wrapT"].as_u64().unwrap_or(10497) as u32])))
    }

    /// Bytes a material's decoded texture set takes (see `material`).
    fn texture_bytes(&self, i: usize) -> Result<u64, String> {
        let m = entry(&self.doc, "materials", i)?;
        let pbr = &m["pbrMetallicRoughness"];
        let slots = [self.texture(&pbr["baseColorTexture"])?, self.texture(&m["normalTexture"])?, self.texture(&pbr["metallicRoughnessTexture"])?, self.texture(&m["occlusionTexture"])?, self.texture(&m["emissiveTexture"])?];
        let size = slots.iter().flatten().map(|(i, _)| self.images[*i].width().max(self.images[*i].height())).max().unwrap_or(1).min(self.cap) as u64;
        Ok(if slots.iter().any(Option::is_some) || m["alphaMode"] == "MASK" { size * size * 4 * 6 } else { 0 })
    }

    fn texture_key(&self, material:&Value)->String {
        let p=&material["pbrMetallicRoughness"];
        json!([p["baseColorTexture"],material["normalTexture"],p["metallicRoughnessTexture"],material["occlusionTexture"],p["roughnessFactor"],p["metallicFactor"],material["alphaMode"],material["emissiveTexture"]]).to_string()
    }

    fn material(&self, p: &Value) -> Result<ReadMaterial, String> {
        let m = if p["material"].is_null() { &Value::Null } else { entry(&self.doc, "materials", index(&p["material"])?)? };
        let pbr = &m["pbrMetallicRoughness"];
        let ext = &m["extensions"];
        let emissive = array(&m["emissiveFactor"], [0.0; 3])?.map(|n| n * number(&ext["KHR_materials_emissive_strength"]["emissiveStrength"], 1.0));
        let material = Material {
            name: m["name"].as_str().unwrap_or("default").into(),
            roughness: number(&pbr["roughnessFactor"], 1.0), metallic: number(&pbr["metallicFactor"], 1.0),
            clearcoat: number(&ext["KHR_materials_clearcoat"]["clearcoatFactor"], 0.0),
            sheen: array(&ext["KHR_materials_sheen"]["sheenColorFactor"], [0.0; 3])?.into_iter().fold(0.0, f32::max),
            transmission: number(&ext["KHR_materials_transmission"]["transmissionFactor"], 0.0),
            ior: number(&ext["KHR_materials_ior"]["ior"], 1.5), emission: 0.0, ..Material::default()
        };
        if material.surface().iter().chain([material.transmission, material.ior].iter()).chain(emissive.iter()).any(|v| !v.is_finite()) {
            return Err("non-finite material value".into());
        }
        let factor = array(&pbr["baseColorFactor"], [1.0; 4])?;
        let slots = [self.texture(&pbr["baseColorTexture"])?, self.texture(&m["normalTexture"])?, self.texture(&pbr["metallicRoughnessTexture"])?, self.texture(&m["occlusionTexture"])?, self.texture(&m["emissiveTexture"])?];
        let size = slots.iter().flatten().map(|(i, _)| self.images[*i].width().max(self.images[*i].height())).max().unwrap_or(1).min(self.cap);
        let image = |slot: Option<(usize, [u32; 2])>, default: [u8; 4]| -> Vec<u8> {
            slot.map_or_else(|| default.repeat((size * size) as usize), |(i, _)| image::imageops::resize(&self.images[i], size, size, image::imageops::FilterType::Triangle).into_raw())
        };
        let alpha = m["alphaMode"] == "MASK";
        let mut extra = ImportedMaterial { emissive, base_alpha: factor[3], alpha_cutoff: alpha.then(|| number(&m["alphaCutoff"], 0.5)), wraps: slots.map(|s| s.map_or([33071; 2], |s| s.1)), ..Default::default() };
        if slots[4].is_some() { extra.emissive_texture = Some(image(slots[4], [255; 4])); }
        let cache_key=self.texture_key(m);
        let cached=if slots.iter().any(Option::is_some)||alpha {self.texture_cache.borrow().get(&cache_key).cloned()}else{None};
        let textures = if let Some(texture)=cached {extra.size=size;Some(texture)} else if slots.iter().any(Option::is_some) || alpha {
            let color = image(slots[0], [255; 4]);
            let mut normal = image(slots[1], [128, 128, 255, 255]);
            let scale = number(&m["normalTexture"]["scale"], 1.0);
            for c in normal.chunks_exact_mut(4).filter(|_| scale != 1.0) {
                let n = Vec3::new((c[0] as f32 / 255.0 * 2.0 - 1.0) * scale, (c[1] as f32 / 255.0 * 2.0 - 1.0) * scale, c[2] as f32 / 255.0 * 2.0 - 1.0).normalize_or(Vec3::Z);
                for i in 0..3 { c[i] = ((n[i] * 0.5 + 0.5) * 255.0).round() as u8; }
            }
            if !scale.is_finite() { return Err("non-finite normal scale".into()); }
            let mut orm = image(slots[2], [255; 4]);
            let ao = image(slots[3], [255; 4]);
            let strength = number(&m["occlusionTexture"]["strength"], 1.0);
            if !strength.is_finite() || extra.alpha_cutoff.is_some_and(|c| !c.is_finite()) { return Err("non-finite texture factor".into()); }
            for (c, a) in orm.chunks_exact_mut(4).zip(ao.chunks_exact(4)) {
                c[0] = ((1.0 + strength * (a[0] as f32 / 255.0 - 1.0)) * 255.0).round() as u8;
                c[1] = (c[1] as f32 * material.roughness).round() as u8;
                c[2] = (c[2] as f32 * material.metallic).round() as u8;
            }
            extra.size = size;
            let texture=Arc::new(Textures { size, color, normal, orm, alpha });
            self.texture_cache.borrow_mut().insert(cache_key,texture.clone());
            Some(texture)
        } else { None };
        Ok((material, factor, textures, extra))
    }

    fn load(mut self, animation: Option<&str>, frame: Option<f32>) -> Result<Loaded, String> {
        let nodes = list(&self.doc, "nodes");
        let mut parents = vec![None; nodes.len()];
        for (i, node) in nodes.iter().enumerate() {
            for child in list(node, "children") {
                let child = index(child)?;
                let parent = parents.get_mut(child).ok_or("child node out of range")?;
                if parent.replace(i).is_some() { return Err("node has more than one parent".into()); }
            }
        }
        let mut trs = Vec::new();
        let mut local = Vec::new();
        for node in nodes {
            let t = Vec3::from(array(&node["translation"], [0.0; 3])?);
            let q = Quat::from_array(array(&node["rotation"], [0.0, 0.0, 0.0, 1.0])?);
            if q.length_squared() < 1e-12 { return Err("zero node quaternion".into()); }
            let s = Vec3::from(array(&node["scale"], [1.0; 3])?);
            trs.push((s, q.normalize(), t));
            local.push(if node["matrix"].is_null() { Mat4::from_scale_rotation_translation(s, q.normalize(), t) } else { Mat4::from_cols_array(&array(&node["matrix"], Mat4::IDENTITY.to_cols_array())?) });
        }
        let mut anim_summary = Vec::new();
        let mut clips = Vec::new();
        for (i, a) in list(&self.doc, "animations").iter().enumerate() {
            let mut channels = Vec::new();
            let mut start = f32::MAX;
            let mut end = f32::MIN;
            let mut steps = Vec::new();
            for c in list(a, "channels") {
                let sampler = list(a, "samplers").get(index(&c["sampler"])?).ok_or("animation sampler out of range")?;
                let times = self.accessor(index(&sampler["input"])?)?;
                let values = self.accessor(index(&sampler["output"])?)?;
                if times.is_empty() || times.iter().any(|t| t.len() != 1 || !t[0].is_finite()) || times.windows(2).any(|t| t[0][0] >= t[1][0]) { return Err("invalid animation times".into()); }
                let path = c["target"]["path"].as_str().ok_or("animation target has no path")?;
                let width = match path { "rotation" => 4, "translation" | "scale" => 3, _ => return Err(format!("unsupported animation channel: {path}")) };
                if values.len() != times.len() || values.iter().any(|v| v.len() != width || v.iter().any(|v| !v.is_finite())) { return Err("invalid animation output".into()); }
                let interpolation = sampler["interpolation"].as_str().unwrap_or("LINEAR");
                if !matches!(interpolation, "LINEAR" | "STEP") { return Err(format!("unsupported animation interpolation: {interpolation}")); }
                let node = index(&c["target"]["node"])?;
                if node >= nodes.len() || !nodes[node]["matrix"].is_null() { return Err("invalid animation target node".into()); }
                start = start.min(times[0][0]); end = end.max(times.last().unwrap()[0]);
                for pair in times.windows(2) { steps.push(pair[1][0] - pair[0][0]); }
                channels.push((node, path.to_string(), interpolation == "STEP", times, values));
            }
            if channels.is_empty() { start = 0.0; end = 0.0; }
            steps.sort_by(f32::total_cmp);
            let step = steps.get(steps.len() / 2).copied().unwrap_or(f32::MAX);
            let fps = number(&a["extras"]["fps"], number(&self.doc["asset"]["extras"]["fps"], if step < f32::MAX { (1.0 / step).round().max(1.0) } else { 24.0 }));
            if !fps.is_finite() || fps <= 0.0 { return Err("invalid animation fps".into()); }
            if a["extras"]["fps"].is_null() && self.doc["asset"]["extras"]["fps"].is_null() {
                self.warnings.push(format!("animation {} has no fps metadata; inferred {fps} fps (glTF times are seconds)", a["name"].as_str().unwrap_or("unnamed")));
            }
            let name = a["name"].as_str().map_or_else(|| format!("animation_{i}"), str::to_string);
            anim_summary.push(json!({"name": name, "fps": fps, "frames": [start * fps, end * fps], "seconds": [start, end]}));
            clips.push((name, fps, channels));
        }
        let chosen = match animation {
            Some(name) => Some(clips.iter().find(|a| a.0 == name).ok_or_else(|| format!("animation {name:?} not found"))?),
            None if frame.is_some() => clips.first(),
            None => None,
        };
        if frame.is_some() && chosen.is_none() { self.warnings.push("frame requested but the file has no animation".into()); }
        if let Some((_, fps, channels)) = chosen {
            let time = frame.unwrap_or(0.0) / fps;
            if !time.is_finite() { return Err("frame must be finite".into()); }
            for (node, path, step, times, values) in channels {
                let upper = times.partition_point(|t| t[0] <= time).min(times.len() - 1);
                let lower = if time >= times.last().unwrap()[0] { upper } else { upper.saturating_sub(1) };
                let a = if *step || lower == upper { 0.0 } else { ((time - times[lower][0]) / (times[upper][0] - times[lower][0])).clamp(0.0, 1.0) };
                let (s, q, t) = &mut trs[*node];
                if path == "rotation" {
                    let qa = Quat::from_slice(&values[lower]); let qb = Quat::from_slice(&values[upper]);
                    if qa.length_squared() < 1e-12 || qb.length_squared() < 1e-12 { return Err("zero animation quaternion".into()); }
                    *q = qa.normalize().slerp(qb.normalize(), a);
                } else {
                    let v = Vec3::from_slice(&values[lower]).lerp(Vec3::from_slice(&values[upper]), a);
                    if path == "translation" { *t = v; } else { *s = v; }
                }
                local[*node] = Mat4::from_scale_rotation_translation(*s, *q, *t);
            }
        }
        // Resolve without recursion: malformed deep trees cannot overflow the stack.
        let mut world = vec![None; nodes.len()];
        for i in 0..nodes.len() {
            let mut chain = Vec::new(); let mut seen = HashSet::new(); let mut n = i;
            while world[n].is_none() {
                if !seen.insert(n) { return Err("cycle in node tree".into()); }
                chain.push(n);
                match parents[n] { Some(p) => n = p, None => break }
            }
            for n in chain.into_iter().rev() {
                let m = parents[n].and_then(|p| world[p]).unwrap_or(Mat4::IDENTITY) * local[n];
                if !m.is_finite() { return Err("non-finite node transform".into()); }
                world[n] = Some(m);
            }
        }
        let mut active = HashSet::new();
        let mut stack: Vec<usize> = if list(&self.doc, "scenes").is_empty() {
            parents.iter().enumerate().filter_map(|(i, p)| p.is_none().then_some(i)).collect()
        } else {
            let scene = entry(&self.doc, "scenes", self.doc["scene"].as_u64().unwrap_or(0) as usize)?;
            list(scene, "nodes").iter().map(index).collect::<Result<_, _>>()?
        };
        while let Some(n) = stack.pop() {
            if n >= nodes.len() { return Err("scene node out of range".into()); }
            if active.insert(n) { for c in list(&nodes[n], "children") { stack.push(index(c)?); } }
        }
        let mut skins = Vec::new();
        let mut used_joints = Vec::new();
        for skin in list(&self.doc, "skins") {
            let joints = list(skin, "joints").iter().map(index).collect::<Result<Vec<_>, _>>()?;
            if joints.is_empty() || joints.iter().any(|i| *i >= nodes.len()) { return Err("invalid skin joints".into()); }
            let ibm = if skin["inverseBindMatrices"].is_null() { vec![Mat4::IDENTITY; joints.len()] } else {
                let values = self.accessor(index(&skin["inverseBindMatrices"])?)?;
                if values.len() != joints.len() || values.iter().any(|v| v.len() != 16 || v.iter().any(|x| !x.is_finite())) { return Err("invalid inverse bind matrices".into()); }
                values.iter().map(|v| Mat4::from_cols_slice(v)).collect()
            };
            skins.push(joints.iter().zip(ibm).map(|(j, ib)| world[*j].unwrap() * ib).collect::<Vec<_>>());
            used_joints.push(vec![false; joints.len()]);
        }
        let material_count = list(&self.doc, "materials").len();
        if material_count > MAX_MATERIALS { return Err(format!("more than {MAX_MATERIALS} materials")); }
        // Each textured material decodes a full texture set: bound the sum.
        // Big exports (several materials on 4K or 8K maps) are read at a
        // smaller size rather than refused; the file is not changed.
        loop {
            let mut seen=HashSet::new();
            let texture_bytes: u64 = (0..material_count).filter(|i|seen.insert(self.texture_key(&self.doc["materials"][*i]))).map(|i| self.texture_bytes(i)).sum::<Result<u64, String>>()?;
            if texture_bytes <= MAX_TEXTURE_BYTES { break; }
            if self.cap <= 256 { return Err("the materials' textures are too large to load in all".into()); }
            self.cap /= 2;
        }
        if self.cap < 4096 && self.images.iter().any(|i| i.width().max(i.height()) > self.cap) {
            self.warnings.push(format!("textures were read at {} px for this check to fit in memory (the file keeps its full size)", self.cap));
        }
        let materials = (0..material_count).map(|i| self.material(&json!({"material": i}))).collect::<Result<Vec<_>, _>>()?;
        let default_material = self.material(&json!({}))?;
        let mut items = Vec::new();
        let mut lo = Vec3::splat(f32::MAX); let mut hi = Vec3::splat(f32::MIN);
        let mut counts = Vec::new();
        let (mut vertices, mut triangles) = (0, 0);
        let mut instanced = 0usize;
        let mut node_order: Vec<_> = active.into_iter().collect(); node_order.sort_unstable();
        for node in node_order {
            let n = &nodes[node];
            if n["mesh"].is_null() { continue; }
            let mi = index(&n["mesh"])?;
            let model = entry(&self.doc, "meshes", mi)?;
            let skin = if n["skin"].is_null() { None } else { Some(index(&n["skin"])?) };
            if skin.is_some_and(|i| i >= skins.len()) { return Err("skin index out of range".into()); }
            if skin.is_some() && clips.is_empty() { self.warnings.push(format!("mesh {mi} is skinned but has no animation")); }
            let (mut nv, mut nt) = (0, 0);
            let mut counted = HashSet::new();
            for p in list(model, "primitives") {
                if p["mode"].as_u64().unwrap_or(4) != 4 { return Err("only triangle primitives are supported".into()); }
                if !list(p, "targets").is_empty() { self.warnings.push(format!("mesh {mi}: morph targets are not applied")); }
                let pi = index(&p["attributes"]["POSITION"])?;
                let positions = self.accessor(pi)?;
                if positions.iter().any(|v| v.len() != 3) { return Err("POSITION must be VEC3".into()); }
                let count = positions.len();
                let normals = self.attribute(p, "NORMAL", count, &[3])?;
                let colors = self.attribute(p, "COLOR_0", count, &[3, 4])?;
                let uvs = self.attribute(p, "TEXCOORD_0", count, &[2])?;
                let tangents = self.attribute(p, "TANGENT", count, &[4])?;
                let joints = self.attribute(p, "JOINTS_0", count, &[4])?;
                let weights = self.attribute(p, "WEIGHTS_0", count, &[4])?;
                let indices = if p["indices"].is_null() { (0..count as u32).collect() } else {
                    let id = index(&p["indices"])?;
                    let a = entry(&self.doc, "accessors", id)?;
                    if !matches!(a["componentType"].as_u64(), Some(5121 | 5123 | 5125)) || a["normalized"] == true { return Err("invalid index accessor".into()); }
                    self.accessor(id)?.iter().map(|v| {
                        if v.len() != 1 || v[0] < 0.0 || v[0] as usize >= count { Err("mesh index out of range".to_string()) } else { Ok(v[0] as u32) }
                    }).collect::<Result<Vec<_>, _>>()?
                };
                if indices.len() % 3 != 0 { return Err("triangle indices are not a multiple of three".into()); }
                let (material, factor, textures, mut extra) = if p["material"].is_null() { default_material.clone() } else {
                    materials.get(index(&p["material"])?).ok_or("material index out of range")?.clone()
                };
                extra.vertex_alpha = colors.as_ref().map_or_else(Vec::new, |c| c.iter().map(|v| v.get(3).copied().unwrap_or(1.0)).collect());
                if textures.is_none() { self.warnings.push(format!("mesh {mi}, material {}: untextured (uses vertex color/base factor)", material.name)); }
                if textures.is_some() && uvs.is_none() { self.warnings.push(format!("mesh {mi}: textures have no TEXCOORD_0")); }
                let mut mesh = Mesh { indices, materials: vec![material.clone()], textures, imported: Some(Arc::new(extra)), ..Default::default() };
                let mut bad = 0;
                let mut zero_weights = 0;
                for (i, pos) in positions.iter().enumerate() {
                    let mut point = Vec3::from_slice(pos);
                    let mut normal = normals.as_ref().map_or(Vec3::ZERO, |n| Vec3::from_slice(&n[i]));
                    let mut tangent = tangents.as_ref().map_or([0.0; 4], |t| [t[i][0], t[i][1], t[i][2], t[i][3]]);
                    let transform = if let Some(skin) = skin {
                        let (Some(js), Some(ws)) = (&joints, &weights) else { return Err("skinned primitive lacks JOINTS_0/WEIGHTS_0".into()); };
                        let mut mat = Mat4::ZERO; let mut sum = 0.0;
                        for k in 0..4 {
                            let j = js[i][k]; let w = ws[i][k];
                            if !j.is_finite() || j < 0.0 || j.fract() != 0.0 || j as usize >= skins[skin].len() || !w.is_finite() || w < 0.0 { return Err("invalid skin influence".into()); }
                            if w > 0.0 { mat += skins[skin][j as usize] * w; sum += w; used_joints[skin][j as usize] = true; }
                        }
                        if sum <= 1e-8 { zero_weights += 1; world[node].unwrap() } else { mat * (1.0 / sum) }
                    } else { world[node].unwrap() };
                    point = transform.transform_point3(point);
                    let basis = Mat3::from_mat4(transform);
                    if basis.determinant().abs() > 1e-12 { normal = basis.inverse().transpose() * normal; } else { normal = Vec3::ZERO; }
                    let tv = basis * Vec3::from_slice(&tangent);
                    tangent[..3].copy_from_slice(&tv.normalize_or_zero().to_array());
                    if basis.determinant() < 0.0 { tangent[3] = -tangent[3]; }
                    let color = colors.as_ref().map_or([1.0; 3], |c| [c[i][0], c[i][1], c[i][2]]);
                    let uv = uvs.as_ref().map_or([0.0; 2], |u| [u[i][0], u[i][1]]);
                    if !point.is_finite() || !normal.is_finite() || color.iter().chain(uv.iter()).chain(tangent.iter()).any(|v| !v.is_finite()) { bad += 1; point = Vec3::ZERO; normal = Vec3::ZERO; }
                    let color = [0, 1, 2].map(|k| (color[k] * factor[k]).max(0.0).powf(1.0 / 2.2));
                    mesh.vertices.push(Vertex { pos: point.to_array(), normal: normal.normalize_or_zero().to_array(), color, surface: material.surface(), material: 0, uv, tangent, light: material.light(), base: [color[0], color[1], color[2], -1.0], wear: [0.0; 3], painted: [0; 4] });
                }
                if bad > 0 { return Err(format!("mesh {mi}: {bad} vertices contain NaNs or infinities; cannot render safely")); }
                if zero_weights > 0 { self.warnings.push(format!("mesh {mi}: {zero_weights} vertices have no weights")); }
                if normals.is_none() || mesh.vertices.iter().any(|v| Vec3::from(v.normal).length_squared() < 0.5) {
                    self.warnings.push(format!("mesh {mi}: missing or zero normals; generated from triangles")); generate_normals(&mut mesh);
                }
                if mesh.textures.is_some() && tangents.is_none() { generate_tangents(&mut mesh); }
                for v in &mesh.vertices { let p = Vec3::from(v.pos); lo = lo.min(p); hi = hi.max(p); }
                if counted.insert(pi) { nv += count; }
                nt += mesh.indices.len() / 3;
                instanced = instanced.saturating_add(mesh.vertices.len() + mesh.indices.len() / 3);
                if instanced > MAX_INSTANCED { return Err(format!("the scene's mesh instances hold more than {MAX_INSTANCED} vertices and triangles")); }
                items.push(film::Item { mesh: Arc::new(mesh), world: Mat4::IDENTITY, tint: [1.0; 3], surface: material.surface(), material });
            }
            counts.push(json!({"mesh": mi, "node": node, "name": model["name"], "vertices": nv, "triangles": nt}));
            vertices += nv; triangles += nt;
        }
        if triangles == 0 { return Err("file's active scene has no triangles".into()); }
        if !(hi - lo).is_finite() { return Err("file bounds overflow".into()); }
        for (i, used) in used_joints.iter().enumerate() {
            let missing: Vec<_> = used.iter().enumerate().filter_map(|(j, u)| (!u).then_some(j)).collect();
            if !missing.is_empty() { self.warnings.push(format!("skin {i}: joints with no weights: {missing:?}")); }
        }
        for m in list(&self.doc, "materials") {
            if m["alphaMode"] == "BLEND" { self.warnings.push("alpha BLEND rendered opaque; only MASK is supported".into()); }
        }
        self.warnings.sort(); self.warnings.dedup();
        let material_summary: Vec<_> = list(&self.doc, "materials").iter().enumerate().map(|(i, m)| json!({
            "index": i, "name": m["name"], "base_color": m["pbrMetallicRoughness"]["baseColorFactor"],
            "emissive": m["emissiveFactor"], "emissive_strength": m["extensions"]["KHR_materials_emissive_strength"]["emissiveStrength"],
            "transmission": m["extensions"]["KHR_materials_transmission"]["transmissionFactor"], "ior": m["extensions"]["KHR_materials_ior"]["ior"],
            "textures": {"base_color": m["pbrMetallicRoughness"]["baseColorTexture"]["index"], "normal": m["normalTexture"]["index"], "metallic_roughness": m["pbrMetallicRoughness"]["metallicRoughnessTexture"]["index"], "occlusion": m["occlusionTexture"]["index"], "emissive": m["emissiveTexture"]["index"]}
        })).collect();
        let image_summary: Vec<_> = self.images.iter().enumerate().map(|(i, img)| json!({"index": i, "width": img.width(), "height": img.height()})).collect();
        let skin_summary: Vec<_> = list(&self.doc, "skins").iter().enumerate().map(|(i, s)| json!({"index": i, "name": s["name"], "joints": list(s, "joints").len()})).collect();
        let summary = json!({"vertices": vertices, "triangles": triangles, "meshes": counts, "materials": material_summary, "images": image_summary, "textures": list(&self.doc, "textures"), "skins": skin_summary, "animations": anim_summary, "pose": chosen.map(|c| json!({"animation": c.0, "frame": frame.unwrap_or(0.0), "fps": c.1})), "bounds": {"min": lo.to_array(), "max": hi.to_array()}, "warnings": self.warnings});
        Ok(Loaded { items, summary, bounds: (lo, hi) })
    }
}

fn generate_normals(mesh: &mut Mesh) {
    let mut normals = vec![Vec3::ZERO; mesh.vertices.len()];
    for tri in mesh.indices.chunks_exact(3) {
        let p = |i: usize| Vec3::from(mesh.vertices[tri[i] as usize].pos);
        let normal = (p(1) - p(0)).cross(p(2) - p(0));
        for &i in tri { normals[i as usize] += normal; }
    }
    for (v, n) in mesh.vertices.iter_mut().zip(normals.drain(..)) { v.normal = n.normalize_or(Vec3::Y).to_array(); }
}

fn generate_tangents(mesh: &mut Mesh) {
    let mut tangents = vec![Vec3::ZERO; mesh.vertices.len()];
    let mut bits = tangents.clone();
    for tri in mesh.indices.chunks_exact(3) {
        let p = |i: usize| Vec3::from(mesh.vertices[tri[i] as usize].pos);
        let uv = |i: usize| glam::Vec2::from(mesh.vertices[tri[i] as usize].uv);
        let (e1, e2) = (p(1) - p(0), p(2) - p(0));
        let (u1, u2) = (uv(1) - uv(0), uv(2) - uv(0));
        let det = u1.x * u2.y - u1.y * u2.x;
        if det.abs() < 1e-12 { continue; }
        let t = (e1 * u2.y - e2 * u1.y) / det;
        let b = (e2 * u1.x - e1 * u2.x) / det;
        for &i in tri { tangents[i as usize] += t; bits[i as usize] += b; }
    }
    for (i, v) in mesh.vertices.iter_mut().enumerate() {
        let n = Vec3::from(v.normal);
        let t = (tangents[i] - n * n.dot(tangents[i])).normalize_or(n.any_orthonormal_vector());
        v.tangent = [t.x, t.y, t.z, if n.cross(t).dot(bits[i]) < 0.0 { -1.0 } else { 1.0 }];
    }
}

pub fn read(path: &Path, animation: Option<&str>, frame: Option<f32>) -> Result<Loaded, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Reader::parse(&bytes)?.load(animation, frame)
}

#[cfg(test)]
pub fn from_bytes(bytes: &[u8], animation: Option<&str>, frame: Option<f32>) -> Result<Loaded, String> {
    Reader::parse(bytes)?.load(animation, frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(doc: &Value, mut bin: Vec<u8>) -> Vec<u8> {
        let mut json = serde_json::to_vec(doc).unwrap();
        while !json.len().is_multiple_of(4) { json.push(b' '); }
        while !bin.len().is_multiple_of(4) { bin.push(0); }
        let mut bytes = b"glTF".to_vec();
        for v in [2, (28 + json.len() + bin.len()) as u32, json.len() as u32, 0x4e4f534a] { bytes.extend(v.to_le_bytes()); }
        bytes.extend(json);
        bytes.extend((bin.len() as u32).to_le_bytes()); bytes.extend(0x004e4942u32.to_le_bytes()); bytes.extend(bin);
        bytes
    }

    fn triangle() -> (Value, Vec<u8>) {
        let floats: [f32; 9] = [0., 0., 0., 1., 0., 0., 0., 1., 0.];
        let bin = bytemuck::cast_slice(&floats).to_vec();
        (json!({"asset": {"version": "2.0"}, "buffers": [{"byteLength": bin.len()}],
            "bufferViews": [{"buffer": 0, "byteLength": bin.len()}],
            "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
            "nodes": [{"mesh": 0}], "scenes": [{"nodes": [0]}], "scene": 0}), bin)
    }

    #[test]
    fn matrix_hierarchy_and_missing_normals() {
        let (mut doc, bin) = triangle();
        doc["nodes"] = json!([{"translation": [2, 0, 0], "children": [1]}, {"mesh": 0, "matrix": Mat4::from_translation(Vec3::Y).to_cols_array()}]);
        let loaded = from_bytes(&pack(&doc, bin), None, None).unwrap();
        assert_eq!(loaded.summary["vertices"], 3);
        assert_eq!(loaded.summary["triangles"], 1);
        assert_eq!(loaded.bounds, (Vec3::new(2., 1., 0.), Vec3::new(3., 2., 0.)));
        assert_eq!(loaded.items[0].mesh.vertices[0].normal, [0., 0., 1.]);
        assert!(loaded.summary["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("normals; generated")));
    }

    #[test]
    fn interleaved_normalized_and_sparse_accessors() {
        let bin = vec![0, 255, 99, 99, 128, 64, 99, 99, 1, 0, 255, 128];
        let doc = json!({"asset": {"version": "2.0"}, "buffers": [{"byteLength": bin.len()}],
            "bufferViews": [{"buffer": 0, "byteLength": 8, "byteStride": 4}, {"buffer": 0, "byteOffset": 8, "byteLength": 1}, {"buffer": 0, "byteOffset": 10, "byteLength": 2}],
            "accessors": [{"bufferView": 0, "componentType": 5121, "normalized": true, "count": 2, "type": "VEC2"},
                {"componentType": 5121, "normalized": true, "count": 2, "type": "VEC2", "sparse": {"count": 1, "indices": {"bufferView": 1, "componentType": 5121}, "values": {"bufferView": 2}}}]});
        let r = Reader::parse(&pack(&doc, bin)).unwrap();
        let a = r.accessor(0).unwrap();
        assert_eq!(a[0], [0., 1.]); assert!((a[1][0] - 128. / 255.).abs() < 1e-6);
        let a = r.accessor(1).unwrap(); assert_eq!(a[0], [0., 0.]); assert_eq!(a[1], [1., 128. / 255.]);
    }

    #[test]
    fn linear_and_step_animation_clamp_at_endpoints() {
        let (mut doc, mut bin) = triangle();
        let floats: [f32; 8] = [0., 1., 0., 0., 0., 2., 0., 0.];
        bin.extend(bytemuck::cast_slice(&floats));
        doc["buffers"][0]["byteLength"] = json!(bin.len());
        doc["bufferViews"].as_array_mut().unwrap().extend([json!({"buffer": 0, "byteOffset": 36, "byteLength": 8}), json!({"buffer": 0, "byteOffset": 44, "byteLength": 24})]);
        doc["accessors"].as_array_mut().unwrap().extend([json!({"bufferView": 1, "componentType": 5126, "count": 2, "type": "SCALAR"}), json!({"bufferView": 2, "componentType": 5126, "count": 2, "type": "VEC3"})]);
        doc["animations"] = json!([{"name": "move", "extras": {"fps": 24}, "samplers": [{"input": 1, "output": 2}], "channels": [{"sampler": 0, "target": {"node": 0, "path": "translation"}}]}]);
        let load = |d: &Value, f| from_bytes(&pack(d, bin.clone()), Some("move"), Some(f)).unwrap();
        assert_eq!(load(&doc, 12.).bounds.0.x, 1.);
        assert_eq!(load(&doc, -2.).bounds.0.x, 0.);
        assert_eq!(load(&doc, 48.).bounds.0.x, 2.);
        doc["animations"][0]["samplers"][0]["interpolation"] = json!("STEP");
        assert_eq!(load(&doc, 12.).bounds.0.x, 0.);
        assert_eq!(load(&doc, 24.).bounds.0.x, 2.);
        assert!(from_bytes(&pack(&doc, bin), Some("missing"), Some(0.)).is_err());
    }

    #[test]
    fn embedded_images_pbr_factors_and_optics() {
        let (mut doc, mut bin) = triangle();
        let rgba = image::RgbaImage::from_pixel(2, 1, image::Rgba([64, 128, 200, 128]));
        let rgb = image::RgbImage::from_pixel(2, 1, image::Rgb([128, 128, 255]));
        let mut images = Vec::new();
        for (image, format) in [(image::DynamicImage::ImageRgba8(rgba), image::ImageFormat::Png), (image::DynamicImage::ImageRgb8(rgb), image::ImageFormat::Jpeg)] {
            while !bin.len().is_multiple_of(4) { bin.push(0); }
            let mut bytes = std::io::Cursor::new(Vec::new()); image.write_to(&mut bytes, format).unwrap();
            let bytes = bytes.into_inner();
            let view = doc["bufferViews"].as_array().unwrap().len();
            doc["bufferViews"].as_array_mut().unwrap().push(json!({"buffer": 0, "byteOffset": bin.len(), "byteLength": bytes.len()}));
            bin.extend(bytes); images.push(json!({"bufferView": view}));
        }
        doc["buffers"][0]["byteLength"] = json!(bin.len());
        doc["images"] = json!(images);
        doc["textures"] = json!([{"source": 0}, {"source": 1}]);
        doc["materials"] = json!([{"name": "test", "alphaMode": "MASK", "alphaCutoff": 0.4,
            "pbrMetallicRoughness": {"baseColorFactor": [0.25, 0.5, 1, 0.5], "baseColorTexture": {"index": 0}, "metallicRoughnessTexture": {"index": 0}, "roughnessFactor": 0.5, "metallicFactor": 0.25},
            "normalTexture": {"index": 1}, "occlusionTexture": {"index": 0, "strength": 0.5}, "emissiveTexture": {"index": 0}, "emissiveFactor": [1, 0.5, 0],
            "extensions": {"KHR_materials_emissive_strength": {"emissiveStrength": 3}, "KHR_materials_transmission": {"transmissionFactor": 0.7}, "KHR_materials_ior": {"ior": 1.7}}}]);
        doc["meshes"][0]["primitives"][0]["material"] = json!(0);
        let loaded = from_bytes(&pack(&doc, bin), None, None).unwrap();
        assert_eq!(loaded.summary["images"][0]["width"], 2);
        assert_eq!(loaded.summary["images"][0]["height"], 1);
        assert_eq!(loaded.summary["images"].as_array().unwrap().len(), 2);
        let item = &loaded.items[0]; let tex = item.mesh.textures.as_ref().unwrap();
        assert_eq!(&tex.color[..4], &[64, 128, 200, 128]);
        assert_eq!(&tex.orm[..3], &[160, 64, 50]);
        assert!(tex.alpha);
        assert_eq!(item.material.transmission, 0.7); assert_eq!(item.material.ior, 1.7);
        let extra = item.mesh.imported.as_ref().unwrap();
        assert_eq!(extra.emissive, [3., 1.5, 0.]); assert_eq!(extra.alpha_cutoff, Some(0.4));
        assert_eq!(extra.base_alpha, 0.5); assert_eq!(extra.wraps[0], [10497; 2]);
        assert!((item.mesh.vertices[0].color[0].powf(2.2) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn malformed_files_return_errors() {
        let (mut doc, bin) = triangle();
        let bytes = pack(&doc, bin.clone());
        for len in 0..bytes.len() { assert!(from_bytes(&bytes[..len], None, None).is_err()); }
        doc["nodes"][0]["children"] = json!([0]);
        assert!(from_bytes(&pack(&doc, bin.clone()), None, None).is_err());
        doc["nodes"][0]["children"] = json!([]);
        doc["bufferViews"][0]["byteLength"] = json!(3);
        assert!(from_bytes(&pack(&doc, bin.clone()), None, None).is_err());
        let (doc, mut bin) = triangle();
        bin[..4].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(from_bytes(&pack(&doc, bin), None, None).err().unwrap().contains("NaNs"));
    }

    /// `GLB=/absolute/file.glb cargo test --release render_glb -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn render_glb() {
        let path = std::path::PathBuf::from(std::env::var("GLB").expect("GLB=/absolute/path.glb"));
        let rest = read(&path, None, None).unwrap();
        let animation = rest.summary["animations"].as_array().unwrap().first();
        let clip = animation.map(|a| a["name"].as_str().unwrap().to_string());
        let mid = animation.map_or(0.0, |a| (a["frames"][0].as_f64().unwrap() + a["frames"][1].as_f64().unwrap()) as f32 * 0.5);
        let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("check_out");
        std::fs::create_dir_all(&out).unwrap();
        let stem = path.file_stem().unwrap().to_str().unwrap();
        for (label, frame) in [("frame0", 0.0), ("mid", mid)] {
            let loaded = read(&path, clip.as_deref(), Some(frame)).unwrap();
            eprintln!("{stem} {label}: {}", loaded.summary);
            std::fs::write(out.join(format!("{stem}_{label}.json")), serde_json::to_vec_pretty(&loaded.summary).unwrap()).unwrap();
            let (lo, hi) = loaded.bounds;
            let center = (lo + hi) * 0.5;
            let r = (hi - lo).length() * 0.5 * 1.08;
            let cams = [Vec3::Z, Vec3::new(0.6, 0.25, 0.75).normalize()].map(|d| film::Camera { eye: center + d * r / 15.0f32.to_radians().sin(), target: center, up: Vec3::Y, fov_deg: 30.0, ortho: None });
            let meshes = film::Meshes::new(loaded.items);
            let images = film::render_views(&meshes, &[], &cams, film::Settings { width: 512, height: 512, samples: 32, bounces: 4, time_limit: std::time::Duration::from_secs(40), floor: true, exposure: 1.0, denoise: true }, &Default::default());
            let mut sheet = image::RgbImage::new(1024, 512);
            for (i, img) in images.into_iter().enumerate() {
                assert!(img.samples > 0);
                let tile = image::RgbImage::from_raw(img.width as u32, img.height as u32, img.pixels.into_iter().flatten().collect()).unwrap();
                image::imageops::replace(&mut sheet, &tile, i as i64 * 512, 0);
            }
            let file = out.join(format!("{stem}_{label}.png")); sheet.save(&file).unwrap(); eprintln!("saved {}", file.display());
        }
    }
}
