//! `bake udim: true` (docs/QUALITY_PLAN.md Stage 4b): a clay's colour,
//! roughness, normal and AO baked into a Poly's UDIM tiles, tile by tile
//! (the bake's own texel code over each tile's triangles of the Poly's
//! level, `poly::low::Low::tile`), as a background job.
//!
//! The guide's rule for the two colour sources: paint on the clay before
//! the retopology, bake to the tiles, paint on the Poly after. A bake
//! replaces the four channels it writes and drops the paint strokes on
//! colour and roughness (it says how many); the mask channel stays.

use std::sync::Arc;

use serde_json::{Value, json};

use super::super::ClideApp;
use super::super::jobs::{Finish, Started};
use super::super::tools::{object_json_in, text};
use crate::poly::texture::paint::PolyPaint;
use crate::poly::texture::{BAKED, Channel, MAX_PAGES, check_resolution};

/// Texels kept around each triangle (the bake fills its gaps; pages
/// farther than this from any triangle are left out).
const MARGIN: f32 = 4.0;

impl ClideApp {
    /// `bake {target: clay, lowpoly: {poly, level}, udim: true, texture}`
    /// (see the module doc). Refused before the job starts: a target that
    /// is not a clay, no Poly with UVs, a size that is not a tile
    /// resolution, a placement maps cannot follow, the bake's other
    /// arguments, or more pages than the budget from the UV layout's
    /// counts.
    pub(in crate::app) fn bake_udim(&mut self, clay: u64, args: &Value) -> Result<Started, String> {
        for key in ["maps", "triangles", "region_density", "show", "off", "exact", "save_dir", "map_size", "map_format", "verify"] {
            if args.get(key).is_some_and(|v| !v.is_null()) {
                return Err(format!("{key} is not for udim: true (it bakes the clay's colour, roughness, normal and AO into the Poly's tiles)"));
            }
        }
        let c = self.scene.get(clay).ok_or("object vanished")?;
        if c.clay.is_none() {
            return Err(format!("{} is not a clay object", c.name));
        }
        let clay_name = c.name.clone();
        let l = args.get("lowpoly").and_then(Value::as_object).ok_or("udim: true bakes into a Poly's tiles: send lowpoly {poly, level}")?;
        let poly_id = self.resolve(l.get("poly").ok_or("lowpoly needs poly (a Poly object)")?)?;
        let level = match l.get("level").filter(|v| !v.is_null()) {
            None => 0,
            Some(v) => v.as_u64().filter(|n| *n <= crate::poly::MAX_LEVELS as u64).ok_or(format!("lowpoly.level must be a whole number from 0 to {}", crate::poly::MAX_LEVELS))? as u8,
        };
        let o = self.scene.get(poly_id).ok_or("object vanished")?;
        let poly_name = o.name.clone();
        let poly = o.poly.clone().ok_or("lowpoly.poly must be a Poly object (retopo or poly from_clay makes one)")?;
        if poly.uv.is_none() {
            return Err("the Poly has no UVs: run poly uv (udim: true spreads them over tiles) first".into());
        }
        if poly.faces_at(level) > crate::poly::MAX_SHOWN_FACES {
            return Err(format!("level {level} of that Poly has {} faces; a bake's simple mesh may have at most {} (a lower level)", poly.faces_at(level), crate::poly::MAX_SHOWN_FACES));
        }
        let resolution = match args.get("texture").filter(|v| !v.is_null()) {
            None => crate::poly::texture::DEFAULT_RESOLUTION,
            Some(v) => {
                let r = v.as_u64().filter(|r| crate::bake::TEXTURE_SIZES.iter().any(|t| *t as u64 == *r)).ok_or(format!("texture must be one of {:?} (texels per tile side)", crate::bake::TEXTURE_SIZES))? as u32;
                check_resolution(r)?;
                r
            }
        };
        let place = crate::poly::export::Placement::of(self.scene.world_matrix(clay).inverse() * self.scene.world_matrix(poly_id))?;
        // The pages the bake would hold, from the UV layout's counts,
        // before anything runs.
        let (by_tile, pages) = crate::poly::texture::bake_estimate(&poly, resolution, MARGIN);
        let kept = poly.paint.as_ref().map_or(0, |p| p.textures.pages() - BAKED.iter().map(|c| p.textures.tiles(*c).iter().map(|t| p.textures.tile_pages(*c, *t)).sum::<usize>()).sum::<usize>());
        if pages + kept > MAX_PAGES {
            return Err(format!("baking {} tiles at {resolution} would hold about {} texture pages, more than {MAX_PAGES} ({} GiB); a lower texture size", by_tile.len(), pages + kept, crate::poly::texture::MAX_BYTES >> 30));
        }
        // The composed field is read: unchecked ledgers settle first.
        let unsettled = self.scene.get(clay).and_then(|o| o.clay.clone());
        self.settle_clays([clay])?;
        let settled = self.scene.get(clay).and_then(|o| o.clay.as_ref()) != unsettled.as_ref();
        self.meshes.sync(&self.scene);
        let settings = crate::bake::Settings { texture: resolution, ..crate::bake::Settings::default() };
        let job = self.meshes.bake_inputs(&self.scene, clay, settings)?;
        let before = poly.clone();
        let label = format!("Bake {clay_name} into the tiles of {poly_name}");
        Ok(Started::Job(label, Box::new(move |progress| {
            let timer = std::time::Instant::now();
            progress.range(0.0, 0.1);
            progress.stage("the Poly's level", 0.0);
            let low = crate::poly::low::Low::new_udim(&before, level, place.to_clay, job.source(None).rest, progress)?;
            let mut textures = before.paint.as_ref().map(|p| p.textures.clone()).unwrap_or_default();
            for c in BAKED {
                textures.clear(Some(c));
                textures.set_resolution(c, resolution)?;
            }
            // Exactly what will be baked: every tile the level's triangles
            // lie on (as `Low::tile` takes them) and the pages under its
            // triangles, counted against the budget before any bake.
            let plan = low.bake_plan(resolution, MARGIN, BAKED.len(), textures.pages(), MAX_PAGES, progress).map_err(|p| p.map_or_else(|e| e, |n| too_many(n, resolution)))?;
            let n = plan.len().max(1) as f32;
            let mut done = Vec::new();
            for (i, (t, lt, under)) in plan.into_iter().enumerate() {
                progress.range(0.1 + 0.85 * i as f32 / n, 0.1 + 0.85 * (i + 1) as f32 / n);
                progress.check()?;
                let src = job.source(Some(&lt));
                let (mesh, _) = crate::bake::bake(&src, crate::bake::Settings { texture: resolution, lowpoly: Some(crate::bake::LowPoly { poly: poly_id, level }), ..crate::bake::Settings::default() }, progress)?;
                let tex = mesh.textures.ok_or("the bake made no textures")?;
                // Held to the budget as the pages land, too.
                if textures.pages() + under.len() * BAKED.len() > MAX_PAGES {
                    return Err(too_many(textures.pages() + under.len() * BAKED.len(), resolution));
                }
                crate::poly::texture::put_baked(&mut textures, t, &tex, &under);
                done.push(t);
            }
            progress.check()?;
            let seconds = timer.elapsed().as_secs_f32();
            Ok(Box::new(move |app: &mut ClideApp| {
                if app.scene.get(poly_id).and_then(|o| o.poly.as_ref()) != Some(&before) {
                    return Err("the Poly changed while it was baking; nothing was changed, send the call again".into());
                }
                let mut notes = Vec::new();
                let records: Vec<_> = before.paint.as_ref().map(|p| p.records.clone()).unwrap_or_default();
                let dropped = records.iter().filter(|r| matches!(r.channel, Channel::Color | Channel::Roughness)).count();
                if dropped > 0 {
                    notes.push(format!("{dropped} paint strokes on color and roughness were dropped: the bake replaced those channels (paint on the Poly after baking)"));
                }
                let records = records.into_iter().filter(|r| !matches!(r.channel, Channel::Color | Channel::Roughness)).collect();
                let pages = textures.pages();
                let mb = (textures.bytes() as f64 / 1048576.0 * 10.0).round() / 10.0;
                let mut next = before.clone();
                next.paint = Some(Arc::new(PolyPaint { textures, records, imported: true }));
                let hash = next.hash();
                app.scene.get_mut(poly_id).expect("checked").poly = Some(next);
                Ok(text(json!({
                    "target": clay,
                    "poly": poly_id,
                    "object": object_json_in(&app.scene, poly_id),
                    "udim": true,
                    "level": level,
                    "resolution": resolution,
                    "tiles": done,
                    "channels": BAKED.map(|c| c.name()),
                    "pages": pages,
                    "texture_mb": mb,
                    "settled": settled,
                    "seconds": (seconds * 10.0).round() / 10.0,
                    "hash": hash,
                    "notes": notes,
                })))
            }) as Finish)
        })))
    }
}

/// The refusal of a bake past the page budget.
fn too_many(pages: usize, resolution: u32) -> String {
    format!("baking at {resolution} would hold {pages} texture pages, more than {MAX_PAGES} ({} GiB); a lower texture size", crate::poly::texture::MAX_BYTES >> 30)
}
