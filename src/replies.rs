//! Small, valid JSON text replies with explicit truncation, never broken JSON.
use serde_json::{Value, json};
pub const MAX_TEXT: usize = 7800;
pub fn verbose(args: &Value) -> bool {
    args["verbose"].as_bool() == Some(true) || args["detail"].as_bool() == Some(true)
}

pub fn bound(content: &mut [Value], detailed: bool) {
    if detailed {
        return;
    }
    let count = content
        .iter()
        .filter(|v| v["type"] == "text")
        .count()
        .max(1);
    let budget = MAX_TEXT / count;
    for block in content.iter_mut().filter(|v| v["type"] == "text") {
        let Some(text) = block["text"].as_str() else {
            continue;
        };
        if text.len() <= budget {
            continue;
        }
        let mut value =
            serde_json::from_str::<Value>(text).unwrap_or_else(|_| json!({"message":text}));
        for limit in [24, 12, 6, 3, 1, 0] {
            shrink(&mut value, limit);
            let wrapped = json!({"summary":value,"truncated":true,"next":"Use detail:true or verbose:true, with target/filter and offset/limit for listings."});
            let encoded = wrapped.to_string();
            if encoded.len() <= budget {
                block["text"] = json!(encoded);
                break;
            }
        }
    }
}
fn shrink(v: &mut Value, limit: usize) {
    match v {
        Value::Array(a) => {
            a.truncate(limit);
            for v in a {
                shrink(v, limit);
            }
        }
        Value::Object(o) => {
            for v in o.values_mut() {
                shrink(v, limit);
            }
            if limit == 0 && o.len() > 12 {
                *v = json!({"fields":o.len(),"omitted":true});
            }
        }
        Value::String(s) if s.len() > 256 => {
            *s = s.chars().take(128).collect::<String>() + "…";
        }
        _ => {}
    }
}

pub fn scene(scene: &crate::scene::Scene, args: &Value) -> Value {
    let top = args.get("parent").is_none()
        && args.get("filter").is_none()
        && args.get("target").is_none()
        && !verbose(args)
        && args["all"].as_bool() != Some(true);
    let objects: Vec<_> = scene
        .objects
        .iter()
        .filter(|o| {
            (!top || o.parent.is_none())
                && args.get("parent").is_none_or(|p| {
                    if p.is_null() {
                        o.parent.is_none()
                    } else {
                        o.parent
                            .and_then(|id| scene.get(id))
                            .is_some_and(|pobj| p == &json!(pobj.name) || p == &json!(pobj.id))
                    }
                })
                && args["filter"]
                    .as_str()
                    .is_none_or(|p| crate::material_management::glob(p, &o.name))
                && args
                    .get("target")
                    .is_none_or(|t| t == &json!(o.id) || t == &json!(o.name))
        })
        .collect();
    let total = objects.len();
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"]
        .as_u64()
        .unwrap_or(if verbose(args) { usize::MAX as u64 } else { 32 }) as usize;
    let mut rows:Vec<_>=objects.into_iter().skip(offset).take(limit).map(|o| {
        if verbose(args) {crate::app::tools::object_json_in(scene,o.id)} else {
            json!({"id":o.id,"name":o.name,"shape":o.shape,"children":scene.objects.iter().filter(|c|c.parent==Some(o.id)).count(),"material":o.material,"mirror_of":o.link.as_ref().map(|l|l.source)})
        }
    }).collect();
    fit_page(&mut rows, verbose(args));
    let count = rows.len();
    json!({"object_count":scene.objects.len(),"matched":total,"top_level":top,"objects":rows,"offset":offset,"next_offset":offset.checked_add(count).filter(|n|*n<total)})
}

/// Preserve pagination continuity when a page contains unusually long names/users.
pub fn fit_page(rows: &mut Vec<Value>, verbose: bool) {
    if verbose {
        return;
    }
    while rows.len() > 1 && serde_json::to_string_pretty(rows).is_ok_and(|s| s.len() > 5800) {
        rows.pop();
    }
}
