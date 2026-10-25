//! QuickSave: every `EVERY` while the scene has unsaved changes, a copy
//! goes to the backups folder (`~/Library/Application Support/Clide3D/
//! backups` on macOS), keeping the newest `KEEP`. It is an app setting,
//! not scene state; it never touches the open file or its unsaved mark.
//! The scene is snapshotted on the window's thread (cheap: grids are
//! shared) and written on a thread of its own.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::scene::Scene;

pub const EVERY: Duration = Duration::from_secs(5 * 60);
pub const KEEP: usize = 10;

pub struct QuickSave {
    pub enabled: bool,
    pub dir: PathBuf,
    /// When the scene last had a backup (or the app started).
    last: Instant,
    /// The scene changed since the last backup.
    pub pending: bool,
    busy: Arc<AtomicBool>,
    pub last_path: Option<PathBuf>,
}

impl Default for QuickSave {
    fn default() -> Self {
        QuickSave { enabled: true, dir: default_dir(), last: Instant::now(), pending: false, busy: Arc::new(AtomicBool::new(false)), last_path: None }
    }
}

/// The backups folder for this platform.
pub fn default_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Clide3D/backups")
    } else {
        std::env::var_os("XDG_DATA_HOME").map_or_else(|| home.join(".local/share"), PathBuf::from).join("clide3d/backups")
    }
}

impl QuickSave {
    pub fn new(enabled: bool) -> QuickSave {
        QuickSave { enabled, ..QuickSave::default() }
    }

    /// How long until the next backup is due (None: nothing to back up).
    pub fn due_in(&self) -> Option<Duration> {
        (self.enabled && self.pending).then(|| EVERY.saturating_sub(self.last.elapsed()))
    }

    /// Starts a backup of `scene` if one is due. Call every frame.
    pub fn tick(&mut self, scene: &Scene) {
        if self.due_in() == Some(Duration::ZERO) && !self.busy.load(Ordering::Acquire) {
            self.start(scene.clone());
        }
    }

    /// Writes a backup of `scene` now, on a thread; returns its path.
    pub fn start(&mut self, scene: Scene) -> PathBuf {
        let path = self.dir.join(file_name());
        self.last = Instant::now();
        self.pending = false;
        self.busy.store(true, Ordering::Release);
        let (busy, dir, out) = (self.busy.clone(), self.dir.clone(), path.clone());
        std::thread::Builder::new()
            .name("quicksave".into())
            .spawn(move || {
                if let Err(e) = write(&scene, &dir, &out) {
                    eprintln!("quicksave: {e}");
                }
                busy.store(false, Ordering::Release);
            })
            .expect("quicksave thread starts");
        self.last_path = Some(path.clone());
        path
    }
}

fn file_name() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("quicksave-{}-{:03}.json", t.as_secs(), t.subsec_millis())
}

/// Writes `scene` to `path` in `dir` (made if missing), then keeps only
/// the newest `KEEP` quicksaves there.
pub fn write(scene: &Scene, dir: &Path, path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = path.with_extension("json.part");
    crate::app::write_scene(scene, &tmp)?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    prune(dir, KEEP)
}

/// Deletes all but the newest `keep` quicksaves in `dir`.
pub fn prune(dir: &Path, keep: usize) -> Result<(), String> {
    let mut saves: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("quicksave-") && n.ends_with(".json")))
        .collect();
    // Names sort by time (seconds, then milliseconds, zero-padded).
    saves.sort_by_key(|p| {
        let n = p.file_stem().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let mut parts = n.trim_start_matches("quicksave-").split('-').map(|x| x.parse::<u64>().unwrap_or(0));
        (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
    });
    let extra = saves.len().saturating_sub(keep);
    for p in &saves[..extra] {
        std::fs::remove_file(p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quicksaves_rotate_and_read_back() {
        let dir = std::env::temp_dir().join(format!("clide3d-quicksave-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut scene = Scene::default();
        scene.add(crate::scene::Shape::Torus);
        for k in 0..13u64 {
            write(&scene, &dir, &dir.join(format!("quicksave-{}-{:03}.json", 1000 + k, 0))).unwrap();
        }
        // Someone else's file is left alone.
        std::fs::write(dir.join("notes.json"), "{}").unwrap();
        prune(&dir, KEEP).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(names.len(), KEEP + 1);
        assert!(names.contains(&"notes.json".to_string()));
        assert!(!names.contains(&"quicksave-1002-000.json".to_string()) && names.contains(&"quicksave-1003-000.json".to_string()));
        let back = crate::app::tools::load_scene(&dir.join("quicksave-1012-000.json")).unwrap();
        assert!(back.objects.len() == 1 && back.objects[0].shape == crate::scene::Shape::Torus);

        // The timer: nothing is due until the scene changes, then after EVERY.
        let mut q = QuickSave { dir: dir.clone(), ..QuickSave::default() };
        assert_eq!(q.due_in(), None);
        q.pending = true;
        assert!(q.due_in().unwrap() > Duration::from_secs(290));
        q.last = Instant::now() - EVERY;
        assert_eq!(q.due_in(), Some(Duration::ZERO));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
