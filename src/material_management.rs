//! Material references, compact listings and atomic scene-wide rename/merge.
use crate::{
    material::{MAX_MATERIALS, Material, PRESETS},
    scene::Scene,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub fn glob(pattern: &str, name: &str) -> bool {
    let (p, n) = (pattern.as_bytes(), name.as_bytes());
    let (mut i, mut j, mut star, mut retry) = (0, 0, None, 0);
    while j < n.len() {
        if i < p.len() && (p[i] == b'?' || p[i] == n[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            retry = j;
        } else if let Some(s) = star {
            retry += 1;
            j = retry;
            i = s + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}

pub fn users(scene: &Scene, name: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for o in &scene.objects {
        if o.material.as_deref() == Some(name)
            || (o.material.is_none() && o.shape.is_mesh() && name == "clay")
        {
            out.push(json!({"object":o.name}));
        }
        if let Some(c) = &o.clay {
            for (i, b) in c
                .blobs
                .iter()
                .enumerate()
                .filter(|(_, b)| b.material == name)
            {
                out.push(json!({"object":o.name,"blob":if b.name.is_empty(){i.to_string()}else{b.name.clone()}}));
            }
        }
        if let Some(p) = &o.part {
            for (m, _) in p
                .roles(&scene.materials)
                .iter()
                .filter(|(m, _)| m.name == name)
            {
                out.push(json!({"object":o.name,"part_material":m.name}));
            }
        }
        // Kit/terrain roles have implicit built-in names. Keep overrides of
        // these roles when cleaning unused definitions.
        if (o.kit.is_some() || o.terrain.is_some())
            && [
                "stone",
                "mortar",
                "grout",
                "iron",
                "wood",
                "charred_wood",
                "wax",
                "flame",
                "ember",
                "coal",
                "bone",
                "dirt",
                "rock",
                "ground",
                "cloth",
            ]
            .contains(&name)
        {
            out.push(json!({"object":o.name,"implicit_role":name}));
        }
    }
    out
}

pub fn listing(scene: &Scene, args: &Value) -> Value {
    let names: BTreeSet<String> = scene
        .materials
        .iter()
        .map(|m| m.name.clone())
        .chain(PRESETS.iter().map(|p| p.0.into()))
        .chain(crate::smart::PRESETS.iter().map(|p| p.0.into()))
        .chain(scene.objects.iter().flat_map(|o| {
            o.material
                .iter()
                .cloned()
                .chain(o.clay.iter().flat_map(|c| {
                    c.blobs
                        .iter()
                        .filter(|b| !b.material.is_empty())
                        .map(|b| b.material.clone())
                }))
        }))
        .collect();
    let filter = args["filter"].as_str().unwrap_or("*");
    let verbose = crate::replies::verbose(args);
    let all: Vec<Value> = names
        .into_iter()
        .filter(|n| glob(filter, n))
        .filter_map(|name| {
            let mut used = users(scene, &name);
            let custom = scene.materials.iter().any(|m| m.name == name);
            if args["unused"].as_bool() == Some(true) && (!used.is_empty() || !custom) {
                return None;
            }
            let user_count = used.len();
            if !verbose {
                used.truncate(8);
            }
            let mut v =
                json!({"name":name,"users":used,"user_count":user_count,"scene_material":custom});
            if !verbose && user_count > 8 {
                v["users_note"] = json!(
                    "First 8 references; use filter with this name and verbose:true for all users."
                );
            }
            if verbose {
                v["material"] = json!(Material::find(&scene.materials, &name));
            }
            Some(v)
        })
        .collect();
    let total = all.len();
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"]
        .as_u64()
        .unwrap_or(if verbose { 1000 } else { 24 }) as usize;
    let mut page: Vec<_> = all.into_iter().skip(offset).take(limit.max(1)).collect();
    crate::replies::fit_page(&mut page, verbose);
    let count = page.len();
    let mut out = json!({"materials":page,"total":total,"offset":offset,"next_offset":offset.checked_add(count).filter(|n|*n<total)});
    if verbose {
        out["detail_maps"] = json!(
            crate::detail::LIBRARY
                .iter()
                .map(|e| json!({"name":e.name,"source":e.source,"about":e.about,"tile":e.tile}))
                .collect::<Vec<_>>()
        );
    }
    out
}

pub fn manage(scene: &mut Scene, args: &Value) -> Result<Option<Value>, String> {
    if args["delete_unused"].as_bool() == Some(true) {
        let gone: Vec<_> = scene
            .materials
            .iter()
            .filter(|m| {
                users(scene, &m.name).is_empty()
                    && glob(args["filter"].as_str().unwrap_or("*"), &m.name)
            })
            .map(|m| m.name.clone())
            .collect();
        scene.materials.retain(|m| !gone.contains(&m.name));
        return Ok(Some(
            json!({"deleted":gone,"materials":scene.materials.len()}),
        ));
    }
    let rename = args["rename"].as_str();
    let pattern = args["replace"]
        .as_str()
        .or_else(|| args.get("replace_with").and_then(|_| args["name"].as_str()))
        .or_else(|| rename.and_then(|_| args["name"].as_str()));
    let Some(pattern) = pattern else {
        return Ok(None);
    };
    let to = args["with"]
        .as_str()
        .or(args["replace_with"].as_str())
        .or(rename)
        .ok_or("replace requires with: material name")?
        .trim();
    if to.is_empty() || to.chars().count() > 64 {
        return Err("destination must contain 1..64 characters".into());
    }
    if rename.is_some() {
        if pattern.contains(['*', '?']) {
            return Err("rename needs one exact source name; use replace to merge patterns".into());
        }
        let source = Material::find(&scene.materials, pattern).ok_or("unknown rename source")?;
        if Material::find(&scene.materials, to).is_some() {
            return Err("rename destination exists; use replace to merge".into());
        }
        if scene.materials.len() >= MAX_MATERIALS {
            return Err("material library is full".into());
        }
        scene.materials.push(Material {
            name: to.into(),
            ..source
        });
    } else if Material::find(&scene.materials, to).is_none() {
        return Err(format!("unknown replacement material {to}"));
    }
    let mut changed = 0;
    let replace = |s: &mut String, changed: &mut usize| {
        if s != to && glob(pattern, s) {
            *s = to.into();
            *changed += 1;
        }
    };
    for o in &mut scene.objects {
        if let Some(n) = &mut o.material {
            replace(n, &mut changed);
        }
        if let Some(c) = &mut o.clay {
            for b in &mut c.blobs {
                if !b.material.is_empty() {
                    replace(&mut b.material, &mut changed);
                }
            }
        }
        if let Some(p) = &mut o.part {
            if let Some(n) = &mut p.paint {
                replace(n, &mut changed);
            }
            for n in p.materials.values_mut() {
                replace(n, &mut changed);
            }
            // Replace implicit part roles as well as explicit assignments.
            for role in crate::kitbash::geo::Role::ALL {
                let name = format!("kb_{}", role.name());
                if glob(pattern, &name) && name != to && !p.materials.contains_key(role.name()) {
                    p.materials.insert(role.name().into(), to.into());
                    changed += 1;
                }
            }
        }
    }
    scene
        .materials
        .retain(|m| m.name == to || !glob(pattern, &m.name));
    Ok(Some(
        json!({"replace":pattern,"with":to,"references_changed":changed,"materials":scene.materials.len()}),
    ))
}
