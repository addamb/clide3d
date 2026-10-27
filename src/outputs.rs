//! Files made for other programs (sculpt Stage 8b): 16-bit PNG and float
//! EXR images (displacement maps, render passes), and movies through an
//! external ffmpeg. ffmpeg is optional: it is found through `CLIDE_FFMPEG`,
//! the PATH or the usual install folders (an app opened from the Finder
//! has no Homebrew on its PATH), runs with its arguments as a list (never
//! through a shell), and callers fall back to a PNG sequence without it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

/// How a map or pass is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// 16-bit PNG (8-bit for color passes), values encoded as documented
    /// per map.
    Png,
    /// 32-bit float OpenEXR, values as they are.
    Exr,
}

impl Format {
    /// `v` as "png" or "exr" (null: `default`).
    pub fn parse(v: &Value, key: &str, default: Format) -> Result<Format, String> {
        match v {
            Value::Null => Ok(default),
            Value::String(s) if s == "png" => Ok(Format::Png),
            Value::String(s) if s == "exr" => Ok(Format::Exr),
            _ => Err(format!("{key} must be png or exr")),
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Exr => "exr",
        }
    }
}

/// A PNG of `channels` (1 = gray, 3 = RGB, 4 = RGBA) 16-bit samples per
/// pixel, row by row from the top.
pub fn png16(width: usize, height: usize, channels: usize, data: &[u16]) -> Result<Vec<u8>, String> {
    let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_be_bytes()).collect();
    encode_png(width, height, channels, png::BitDepth::Sixteen, &bytes)
}

/// A PNG of `channels` 8-bit samples per pixel.
pub fn png8(width: usize, height: usize, channels: usize, data: &[u8]) -> Result<Vec<u8>, String> {
    encode_png(width, height, channels, png::BitDepth::Eight, data)
}

fn encode_png(width: usize, height: usize, channels: usize, depth: png::BitDepth, data: &[u8]) -> Result<Vec<u8>, String> {
    let color = match channels {
        1 => png::ColorType::Grayscale,
        3 => png::ColorType::Rgb,
        4 => png::ColorType::Rgba,
        _ => return Err(format!("{channels} channels cannot be a PNG")),
    };
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width as u32, height as u32);
    enc.set_color(color);
    enc.set_depth(depth);
    enc.set_compression(png::Compression::Fast);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(data).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

/// A 16-bit PNG's (width, height, channels, samples), as `png16` wrote it.
pub fn read_png16(bytes: &[u8]) -> Result<(usize, usize, usize, Vec<u16>), String> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("PNG too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    if info.bit_depth != png::BitDepth::Sixteen {
        return Err("not a 16-bit PNG".into());
    }
    let channels = info.color_type.samples();
    let data = buf[..info.buffer_size()].chunks_exact(2).map(|b| u16::from_be_bytes([b[0], b[1]])).collect();
    Ok((info.width as usize, info.height as usize, channels, data))
}

/// `read_png16` of an image that must be `width` x `height` with
/// `channels` 16-bit samples: its header is checked before any pixel is
/// decoded or allocated.
pub fn read_png16_sized(bytes: &[u8], width: usize, height: usize, channels: usize) -> Result<Vec<u16>, String> {
    let reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().map_err(|e| e.to_string())?;
    let info = reader.info();
    if (info.width as usize, info.height as usize) != (width, height) || info.bit_depth != png::BitDepth::Sixteen || info.color_type.samples() != channels {
        return Err(format!("the image is {} x {} with {} {:?}-bit samples, not {width} x {height} with {channels} 16-bit samples", info.width, info.height, info.color_type.samples(), info.bit_depth));
    }
    let (w, h, c, data) = read_png16(bytes)?;
    if (w, h, c) != (width, height, channels) || data.len() != width * height * channels {
        return Err("the image's pixels do not match its header".into());
    }
    Ok(data)
}

/// Writes a float EXR (zip compressed) with these named channels, each
/// `width * height` values row by row from the top.
pub fn write_exr(path: &Path, width: usize, height: usize, channels: Vec<(&str, Vec<f32>)>) -> Result<(), String> {
    use exr::prelude::*;
    if channels.iter().any(|(_, v)| v.len() != width * height) {
        return Err("an EXR channel has the wrong size".into());
    }
    let list: Vec<AnyChannel<FlatSamples>> = channels.into_iter().map(|(name, v)| AnyChannel::new(name, FlatSamples::F32(v))).collect();
    let layer = Layer::new((width, height), LayerAttributes::named("clide3d"), Encoding::SMALL_LOSSLESS, AnyChannels::sort(list.into()));
    let mut bytes = std::io::Cursor::new(Vec::new());
    Image::from_layer(layer).write().non_parallel().to_buffered(&mut bytes).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    write_over(path, "Exr", |h| Kind::Exr.is(h), &bytes.into_inner())
}

/// An EXR's (width, height, channels by name), as `write_exr` wrote it.
pub fn read_exr(path: &Path) -> Result<(usize, usize, Vec<(String, Vec<f32>)>), String> {
    use exr::prelude::*;
    let image = read_first_flat_layer_from_file(path).map_err(|e| e.to_string())?;
    let size = image.layer_data.size;
    let channels = image.layer_data.channel_data.list.iter().map(|c| {
        let values = match &c.sample_data {
            FlatSamples::F32(v) => v.clone(),
            FlatSamples::F16(v) => v.iter().map(|h| h.to_f32()).collect(),
            FlatSamples::U32(v) => v.iter().map(|u| *u as f32).collect(),
        };
        (c.name.to_string(), values)
    }).collect();
    Ok((size.0, size.1, channels))
}

/// What an existing file at an output path may be overwritten as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Png,
    Exr,
    Mp4,
}

impl Kind {
    pub fn is(self, head: &[u8]) -> bool {
        match self {
            Kind::Png => head.starts_with(b"\x89PNG"),
            Kind::Exr => head.starts_with(&[0x76, 0x2f, 0x31, 0x01]),
            Kind::Mp4 => head.len() >= 8 && &head[4..8] == b"ftyp",
        }
    }

    pub fn of(format: Format) -> Kind {
        match format {
            Format::Png => Kind::Png,
            Format::Exr => Kind::Exr,
        }
    }
}

/// Whether the start of a file (its first bytes) is a `kind` file.
fn is_kind(path: &Path, kind: Kind) -> bool {
    use std::io::Read;
    let mut head = [0u8; 12];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    kind.is(&head[..n])
}

/// Refuses to write `path` when something other than a `kind` file is
/// already there (an agent's typo must not overwrite a document). An
/// empty file holds nothing to lose (an encode cut short can leave one),
/// so it may be overwritten. See `overwritable`.
pub fn writable(path: &Path, kind: Kind) -> Result<(), String> {
    overwritable(path, &format!("{kind:?}"), |head| kind.is(head))
}

/// The overwrite rule every writer of agent-named paths keeps, failing
/// closed: nothing there may be written; an empty regular file may; a
/// regular file whose first bytes can be read and `is` accepts may.
/// Anything else is refused: a folder, a symbolic link (it would write
/// through to wherever it points), a device or pipe, a file that cannot
/// be read, or one of another kind.
pub fn overwritable(path: &Path, what: &str, is: impl Fn(&[u8]) -> bool) -> Result<(), String> {
    overwritable_by(path, what, 64, is)
}

/// `overwritable`, with `is` reading up to `head` first bytes (a format
/// whose maker is named past its first bytes, as a .glb's JSON).
pub fn overwritable_by(path: &Path, what: &str, head: usize, is: impl Fn(&[u8]) -> bool) -> Result<(), String> {
    use std::io::Read;
    let head_len = head.clamp(1, 1 << 16);
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}; refusing to write there", path.display())),
    };
    if meta.is_dir() {
        return Err(format!("{} is a folder", path.display()));
    }
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a regular file (a link or a device); refusing to write there", path.display()));
    }
    if meta.len() == 0 {
        return Ok(());
    }
    let mut head = Vec::with_capacity(head_len);
    let n = std::fs::File::open(path).and_then(|f| f.take(head_len as u64).read_to_end(&mut head)).map_err(|e| format!("{} cannot be read ({e}); refusing to overwrite it", path.display()))?;
    if is(&head[..n]) {
        Ok(())
    } else {
        Err(format!("{} exists and is not a {what} file; refusing to overwrite it (delete it, or give another path)", path.display()))
    }
}

/// Writes `bytes` to `path` under `overwritable`'s rule, checked again
/// at the moment of writing (a call is checked when it comes in, and a
/// job may write minutes later): the file is opened without following a
/// symbolic link (one put there meanwhile is refused, not written
/// through), the opened file must be a regular file, and it is cut to
/// nothing only then.
pub fn write_over(path: &Path, what: &str, is: impl Fn(&[u8]) -> bool, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    overwritable_by(path, what, 1 << 16, is)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(NOFOLLOW);
    }
    let mut file = options.open(path).map_err(|e| format!("could not write {}: {e} (refusing to write through a link)", path.display()))?;
    if !file.metadata().is_ok_and(|m| m.file_type().is_file()) {
        return Err(format!("{} is not a regular file; refusing to write there", path.display()));
    }
    file.set_len(0).and_then(|_| file.write_all(bytes)).map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// Opens `path` for reading without following a link at it (`O_NOFOLLOW`
/// where the platform has it), and only when what it opened is a regular
/// file.
pub fn open_regular(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(NOFOLLOW);
    }
    let file = options.open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !file.metadata().map_err(|e| format!("{}: {e}", path.display()))?.is_file() {
        return Err(format!("{}: not a regular file", path.display()));
    }
    Ok(file)
}

/// `O_NOFOLLOW` (no new crate for one constant).
#[cfg(target_os = "macos")]
const NOFOLLOW: i32 = 0x0100;
#[cfg(all(target_os = "linux", any(target_arch = "aarch64", target_arch = "arm")))]
const NOFOLLOW: i32 = 0o100000;
#[cfg(all(target_os = "linux", not(any(target_arch = "aarch64", target_arch = "arm"))))]
const NOFOLLOW: i32 = 0o400000;
#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
const NOFOLLOW: i32 = 0;

/// An absolute folder argument (not made yet: `make_folder` makes it when
/// the job writes, so a call refused later leaves no folder behind).
pub fn folder(v: &Value, key: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(v.as_str().ok_or_else(|| format!("{key} must be an absolute folder path"))?);
    if !path.is_absolute() {
        return Err(format!("{key} must be an absolute folder path"));
    }
    if path.exists() && !path.is_dir() {
        return Err(format!("{key}: {} exists and is not a folder", path.display()));
    }
    Ok(path)
}

/// Makes an output folder (`folder`'s) when missing.
pub fn make_folder(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| format!("could not make {}: {e}", path.display()))
}

/// A name made safe for a file name (letters, digits, - and _).
pub fn file_stem(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(64).collect();
    if s.is_empty() { "object".into() } else { s }
}

/// The ffmpeg program: `CLIDE_FFMPEG` when set (and a file), else the
/// first `ffmpeg` on the PATH or in the usual install folders.
pub fn find_ffmpeg() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CLIDE_FFMPEG") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let extra = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from);
    std::env::split_paths(&path).chain(extra).map(|d| d.join("ffmpeg")).find(|p| p.is_file())
}

/// Most a movie encode may take.
const ENCODE_LIMIT: Duration = Duration::from_secs(600);

/// Encodes the first `count` frames `frames_dir/frame_00000.png ..` (as
/// `frame_name` names them) at `fps` into an H.264 mp4 at `out` (MPEG-4
/// part 2 when this ffmpeg has no H.264 encoder). Returns the codec used.
/// Cancelling `progress` stops it.
pub fn encode_mp4(ffmpeg: &Path, frames_dir: &Path, count: usize, fps: f32, out: &Path, progress: &crate::progress::Progress) -> Result<&'static str, String> {
    let pattern = frame_pattern(frames_dir)?;
    let mut last = String::new();
    for (codec, extra) in [("libx264", ["-crf", "18"]), ("mpeg4", ["-q:v", "3"])] {
        let mut cmd = std::process::Command::new(ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-framerate"])
            .arg(format!("{fps}"))
            .arg("-i")
            .arg(&pattern)
            // ffmpeg reads every consecutive file from 0: stop at `count`, so
            // frames left by an earlier, longer run never reach the movie.
            .arg("-frames:v")
            .arg(count.to_string())
            .args(["-c:v", codec])
            .args(extra)
            .args(["-pix_fmt", "yuv420p", "-movflags", "+faststart"])
            .arg(out)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("could not start {}: {e}", ffmpeg.display()))?;
        // Read its messages on the side, so a full pipe never stalls it.
        let mut stderr = child.stderr.take().expect("piped");
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut stderr, &mut s);
            s
        });
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            if progress.cancelled() || start.elapsed() > ENCODE_LIMIT {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(if progress.cancelled() { "cancelled".into() } else { format!("ffmpeg took more than {} s", ENCODE_LIMIT.as_secs()) });
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let messages = reader.join().unwrap_or_default();
        if status.success() && out.is_file() {
            return Ok(codec);
        }
        last = format!("ffmpeg failed ({status}): {}", messages.trim().lines().last().unwrap_or(""));
    }
    Err(last)
}

/// ffmpeg's image2 pattern for the frames in `dir`: a '%' in the folder
/// is written '%%' (ffmpeg would read it as a format).
fn frame_pattern(dir: &Path) -> Result<PathBuf, String> {
    let dir = dir.to_str().ok_or_else(|| format!("{} is not valid text", dir.display()))?;
    Ok(PathBuf::from(dir.replace('%', "%%")).join("frame_%05d.png"))
}

/// A sequence frame's file name (what `encode_mp4` reads).
pub fn frame_name(i: usize) -> String {
    format!("frame_{i:05}.png")
}

/// Removes the sequence frames numbered `count` and up from `dir` (left by
/// an earlier, longer or failed run), so the folder holds exactly the
/// frames a reply counts. Only PNG files named as `frame_name` names them
/// are touched. Returns how many were removed.
pub fn clear_frames_from(dir: &Path, count: usize) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(index) = name.to_str().and_then(|n| n.strip_prefix("frame_")).and_then(|n| n.strip_suffix(".png")) else { continue };
        if index.len() < 5 || !index.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let stale = index.parse::<usize>().map_or(true, |i| i >= count);
        if stale && entry.file_type().is_ok_and(|t| t.is_file()) && is_kind(&entry.path(), Kind::Png) && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png16_and_exr_round_trip() {
        let dir = std::env::temp_dir().join(format!("clide-outputs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let data: Vec<u16> = (0..6 * 4 * 3).map(|i| (i * 900) as u16).collect();
        let (w, h, c, back) = read_png16(&png16(6, 4, 3, &data).unwrap()).unwrap();
        assert_eq!((w, h, c), (6, 4, 3));
        assert_eq!(back, data);
        let path = dir.join("planes.exr");
        let z: Vec<f32> = (0..24).map(|i| i as f32 * 0.25 - 2.0).collect();
        write_exr(&path, 6, 4, vec![("Z", z.clone())]).unwrap();
        let (w, h, channels) = read_exr(&path).unwrap();
        assert_eq!((w, h), (6, 4));
        assert_eq!(channels, vec![("Z".to_string(), z)]);
        // Overwriting: an EXR over an EXR, never over another file.
        writable(&path, Kind::Exr).unwrap();
        assert!(writable(&path, Kind::Png).is_err());
        let text = dir.join("notes.png");
        std::fs::write(&text, "my notes").unwrap();
        assert!(writable(&text, Kind::Png).unwrap_err().contains("refusing"));
        assert!(writable(&dir, Kind::Png).is_err());
        // An empty file (an encode cut short) may be overwritten; a short
        // one without the header may not, and the refusal says what to do.
        let movie = dir.join("clip.mp4");
        std::fs::write(&movie, b"").unwrap();
        writable(&movie, Kind::Mp4).unwrap();
        std::fs::write(&movie, b"abc").unwrap();
        assert!(writable(&movie, Kind::Mp4).unwrap_err().contains("delete it, or give another path"));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(file_stem("Head / v2"), "Head___v2");
        assert!(Format::parse(&serde_json::json!("tiff"), "format", Format::Png).is_err());
    }

    /// Only the tail of a sequence goes; other files stay.
    #[test]
    fn clear_frames_from_drops_only_the_stale_tail() {
        let dir = std::env::temp_dir().join(format!("clide-tail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let png = png8(1, 1, 3, &[0; 3]).unwrap();
        for i in 0..8 {
            std::fs::write(dir.join(frame_name(i)), &png).unwrap();
        }
        for other in ["frame_7.png", "frame_00009.txt", "notes.png", "frame_000x1.png"] {
            std::fs::write(dir.join(other), &png).unwrap();
        }
        // Named as a frame but not a PNG: someone else's file, kept.
        std::fs::write(dir.join(frame_name(12)), b"my notes").unwrap();
        assert_eq!(clear_frames_from(&dir, 5), 3);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["frame_00000.png", "frame_00001.png", "frame_00002.png", "frame_00003.png", "frame_00004.png", "frame_00009.txt", "frame_00012.png", "frame_000x1.png", "frame_7.png", "notes.png"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A folder argument is checked, not made: `make_folder` makes it.
    #[test]
    fn folders_are_made_only_when_asked() {
        let dir = std::env::temp_dir().join(format!("clide-folder-{}", std::process::id())).join("deep");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        let path = folder(&serde_json::json!(dir.to_str().unwrap()), "path").unwrap();
        assert!(!path.exists(), "parsing makes no folder");
        assert!(folder(&serde_json::json!("relative/dir"), "path").is_err());
        make_folder(&path).unwrap();
        assert!(path.is_dir());
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    /// ffmpeg's pattern: a '%' in the folder is doubled, the frame number
    /// stays a format.
    #[test]
    fn the_frame_pattern_escapes_percent_in_the_folder() {
        let p = frame_pattern(Path::new("/tmp/100% done/clip_frames")).unwrap();
        assert_eq!(p, PathBuf::from("/tmp/100%% done/clip_frames/frame_%05d.png"));
        assert_eq!(frame_pattern(Path::new("/a/b")).unwrap(), PathBuf::from("/a/b/frame_%05d.png"));
    }

    /// The mp4 path with synthetic frames (no GPU): skipped, saying why,
    /// when this machine has no ffmpeg.
    #[test]
    fn mp4_encodes_a_frame_sequence_when_ffmpeg_is_present() {
        let Some(ffmpeg) = find_ffmpeg() else {
            eprintln!("skipped: no ffmpeg (CLIDE_FFMPEG unset or not a file, and none on the PATH or in /opt/homebrew/bin, /usr/local/bin, /usr/bin)");
            return;
        };
        // A '%' in the folder must reach ffmpeg as itself.
        let dir = std::env::temp_dir().join(format!("clide-mp4-{}-100%", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..12 {
            let rgb: Vec<u8> = (0..64 * 48).flat_map(|_| [i as u8 * 20, 100, 255 - i as u8 * 20]).collect();
            std::fs::write(dir.join(frame_name(i)), png8(64, 48, 3, &rgb).unwrap()).unwrap();
        }
        // A stale tail from an earlier run: ffmpeg must stop at 12.
        for i in 12..16 {
            std::fs::write(dir.join(frame_name(i)), png8(32, 32, 3, &[0; 32 * 32 * 3]).unwrap()).unwrap();
        }
        let out = dir.join("clip.mp4");
        let t = Instant::now();
        let codec = encode_mp4(&ffmpeg, &dir, 12, 24.0, &out, &Default::default()).unwrap();
        eprintln!("mp4: 12 frames 64x48 with {codec} in {:.2} s", t.elapsed().as_secs_f32());
        let head = std::fs::read(&out).unwrap();
        assert!(Kind::Mp4.is(&head[..12]), "not an mp4");
        writable(&out, Kind::Mp4).unwrap();
        let ffprobe = ffmpeg.with_file_name("ffprobe");
        if ffprobe.is_file() {
            let probe = std::process::Command::new(&ffprobe)
                .args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
                .arg(&out)
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), "12");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Stage 8b timings (run with --ignored --nocapture): displacement
    /// maps against the bake they come with, render passes against the
    /// picture, and mp4 encoding per 100 frames.
    #[test]
    #[ignore]
    fn stage8b_timings() {
        use crate::bake::{Maps, Settings, Source};
        use glam::Vec3;
        let mut blobs = vec![serde_json::json!({ "type": "sphere", "center": [0, 0, 0], "radius": 0.3 })];
        for i in 0..24 {
            let y = 1.0 - (i as f32 + 0.5) / 12.0;
            let (r, phi) = ((1.0 - y * y).sqrt(), i as f32 * 2.399_963);
            blobs.push(serde_json::json!({ "type": "sphere", "center": (Vec3::new(r * phi.cos(), y, r * phi.sin()) * 0.3).to_array(), "radius": 0.05, "blend": 0.03 }));
        }
        let clay: crate::clay::Clay = serde_json::from_value(serde_json::json!({ "blobs": blobs, "resolution": 192 })).unwrap();
        let rest = clay.build_mesh();
        let src = Source { clay: &clay, rest: &rest, skin: None, parts: &[], library: &[], fallback: "", fine: None, low: None };
        let t = Instant::now();
        let (mesh, _) = crate::bake::bake(&src, Settings { triangles: 30_000, texture: 2048, ..Default::default() }, &Default::default()).unwrap();
        let bake = t.elapsed().as_secs_f32();
        let t = Instant::now();
        let disp = crate::bake::displacement(&src, &mesh, Maps { displacement: true, vector: true, size: 2048, object: false }, &Default::default()).unwrap();
        let maps = t.elapsed().as_secs_f32();
        let t = Instant::now();
        let codes = crate::bake::Displacement::encode16(disp.height.clone().unwrap().into_iter(), disp.scale());
        let png = png16(2048, 2048, 1, &codes).unwrap();
        let encode = t.elapsed().as_secs_f32();
        eprintln!("bake 2048 ({} clay triangles -> 30k): {bake:.2} s; displacement + vector maps 2048: {maps:.2} s; png16 encode {encode:.2} s ({} KB)", rest.indices.len() / 3, png.len() / 1024);
        // Passes against the picture (CPU, 640 px).
        use crate::film::{Camera, Item, Look, Meshes, Settings as Film, passes};
        let item = Item { mesh: std::sync::Arc::new(rest.clone()), world: glam::Mat4::IDENTITY, tint: [0.8; 3], surface: crate::material::Material::default().surface(), material: Default::default() };
        let meshes = Meshes::new(vec![item]);
        let cam = Camera { eye: Vec3::new(0.0, 0.3, 1.5), target: Vec3::ZERO, up: Vec3::Y, fov_deg: 40.0, ortho: None };
        let mut look = Look::default();
        look.device = crate::film::look::Device::Cpu;
        let look = std::sync::Arc::new(look);
        let s = Film { width: 640, height: 640, samples: 128, bounces: 4, time_limit: Duration::from_secs(300), floor: true, exposure: 1.0, denoise: true };
        let t = Instant::now();
        crate::film::render_views_with_look(&meshes, &[], &[cam], s, look.clone(), &Default::default());
        let picture = t.elapsed().as_secs_f32();
        let t = Instant::now();
        passes::render(&meshes, &[], &cam, s, look, &passes::Pass::ALL, passes::Options { samples: 8, ao_rays: 8, ao_distance: 0.1, camera_normals: false }, &Default::default());
        eprintln!("film 640 CPU 128 samples: {picture:.2} s; all seven passes (8 samples, 64 ao rays): {:.2} s", t.elapsed().as_secs_f32());
        if let Some(ffmpeg) = find_ffmpeg() {
            let dir = std::env::temp_dir().join(format!("clide-mp4-time-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            for i in 0..100 {
                let rgb: Vec<u8> = (0..512 * 512).flat_map(|k| [(k % 512) as u8, (i * 2) as u8, (k / 2048) as u8]).collect();
                std::fs::write(dir.join(frame_name(i)), png8(512, 512, 3, &rgb).unwrap()).unwrap();
            }
            let t = Instant::now();
            let codec = encode_mp4(&ffmpeg, &dir, 100, 24.0, &dir.join("t.mp4"), &Default::default()).unwrap();
            eprintln!("mp4 ({codec}): 100 frames of 512x512 in {:.2} s", t.elapsed().as_secs_f32());
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}
