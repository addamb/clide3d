//! Perf assertions. Every timing is logged to `target/perf.json` (by
//! name, merged across tests and runs). With `CLIDE_PERF=1` a timing must
//! meet its bound; otherwise the bound gets 3x slack, so a loaded machine
//! running the whole suite does not fail on noise while a real regression
//! still does.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Value, json};

/// Slack on bounds unless `CLIDE_PERF=1`.
pub const SLACK: f64 = 3.0;

pub fn strict() -> bool {
    std::env::var("CLIDE_PERF").is_ok_and(|v| v == "1")
}

/// Where timings go: `$CARGO_TARGET_DIR/perf.json`, else `target/perf.json`.
pub fn path() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"), PathBuf::from).join("perf.json")
}

/// Runs `f` `runs` times and returns its result and the median time in ms.
pub fn time<R>(runs: usize, mut f: impl FnMut() -> R) -> (R, f64) {
    let mut times = Vec::with_capacity(runs.max(1));
    let mut out = None;
    for _ in 0..runs.max(1) {
        let t = Instant::now();
        out = Some(f());
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    (out.expect("ran once"), times[times.len() / 2])
}

/// Logs `ms` (with any `extra` facts, such as triangle counts) under
/// `name` in perf.json.
pub fn log(name: &str, ms: f64, extra: Value) {
    static FILE: Mutex<()> = Mutex::new(());
    let _guard = FILE.lock().unwrap_or_else(|e| e.into_inner());
    let path = path();
    let mut all: serde_json::Map<String, Value> = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut entry = json!({ "ms": (ms * 100.0).round() / 100.0 });
    if let (Value::Object(e), Value::Object(x)) = (&mut entry, extra) {
        e.extend(x);
    }
    all.insert(name.into(), entry);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, serde_json::to_string_pretty(&Value::Object(all)).expect("json")) {
        eprintln!("perf: could not write {}: {e}", path.display());
    }
}

/// Logs `ms` and asserts it is within `bound_ms` (3x that without
/// `CLIDE_PERF=1`).
pub fn check(name: &str, ms: f64, bound_ms: f64, extra: Value) {
    let mut extra = extra;
    if let Value::Object(x) = &mut extra {
        x.insert("bound_ms".into(), json!(bound_ms));
    }
    log(name, ms, extra);
    let limit = if strict() { bound_ms } else { bound_ms * SLACK };
    assert!(ms <= limit, "{name}: {ms:.1} ms, over {limit:.1} ms (bound {bound_ms} ms{})", if strict() { "" } else { " x3 slack" });
}
