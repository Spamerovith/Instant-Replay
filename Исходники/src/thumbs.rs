//! Preview pictures of clips: one frame per clip, taken with the built-in FFmpeg and cached on disk
//! (`%LOCALAPPDATA%\InstantReplay\thumbs`). The cache holds raw RGBA, packed with gzip.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crossbeam_channel::{unbounded, Receiver, Sender};

pub const W: usize = 320;
pub const H: usize = 180;
const BYTES: usize = W * H * 4;

fn cache_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("APPDATA")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("InstantReplay").join("thumbs")
}

/// FNV-1a: stable between runs and builds, unlike `DefaultHasher`.
fn hash(data: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Cache file of a clip; it changes when the clip's size or date changes.
pub fn cache_file(clip: &Path) -> Option<PathBuf> {
    let md = std::fs::metadata(clip).ok()?;
    let mtime = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let key = format!("{}|{}|{}", clip.to_string_lossy().to_lowercase(), md.len(), mtime);
    Some(cache_dir().join(format!("{:016x}.thumb", hash(key.as_bytes()))))
}

/// Which moment of the clip is shown: a replay ends with what was worth saving, a recording is
/// shown a little after its start.
pub fn frame_time(secs: f64, is_replay: bool) -> f64 {
    if is_replay {
        (secs - 1.5).max(0.0)
    } else {
        (secs / 3.0).min(10.0)
    }
}

fn read_cache(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut out = Vec::with_capacity(BYTES);
    flate2::read::GzDecoder::new(file).read_to_end(&mut out).ok()?;
    (out.len() == BYTES).then_some(out)
}

fn write_cache(path: &Path, rgba: &[u8]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(file) = std::fs::File::create(path) {
        let mut enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let _ = enc.write_all(rgba);
        let _ = enc.finish();
    }
}

/// Takes the frame with FFmpeg: scaled into 320x180 with bars, as raw RGBA.
fn make(clip: &Path, secs: f64, is_replay: bool) -> Option<Vec<u8>> {
    let ffmpeg = crate::player::ffmpeg::ffmpeg()?;
    let filter = format!("scale={W}:{H}:force_original_aspect_ratio=decrease,pad={W}:{H}:(ow-iw)/2:(oh-ih)/2:color=0x1c1c1e");
    let out = crate::player::ffmpeg::hidden_command(ffmpeg)
        .args(["-v", "error", "-ss", &format!("{:.2}", frame_time(secs, is_replay)), "-i"])
        .arg(clip)
        .args(["-frames:v", "1", "-vf", &filter, "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    (out.stdout.len() == BYTES).then_some(out.stdout)
}

/// The picture of a clip, from the cache or made now (slow: run it off the UI thread).
pub fn get(clip: &Path, secs: f64, is_replay: bool) -> Option<Vec<u8>> {
    let cache = cache_file(clip)?;
    if let Some(v) = read_cache(&cache) {
        return Some(v);
    }
    let v = make(clip, secs, is_replay)?;
    write_cache(&cache, &v);
    Some(v)
}

/// Drops the cached picture of a deleted clip.
pub fn forget(clip: &Path) {
    if let Some(f) = cache_file(clip) {
        let _ = std::fs::remove_file(f);
    }
}

type Job = (PathBuf, f64, bool);

/// Background maker of pictures: requests go in, finished pictures come out.
pub struct Thumbs {
    jobs: Sender<Job>,
    done: Receiver<(PathBuf, Option<Vec<u8>>)>,
}

impl Thumbs {
    pub fn start(ctx: egui::Context) -> Thumbs {
        let (jobs, job_rx) = unbounded::<Job>();
        let (done_tx, done) = unbounded();
        let _ = std::thread::Builder::new().name("thumbs".into()).spawn(move || {
            while let Ok((path, secs, is_replay)) = job_rx.recv() {
                let pic = get(&path, secs, is_replay);
                let _ = done_tx.send((path, pic));
                ctx.request_repaint();
            }
        });
        Thumbs { jobs, done }
    }

    pub fn request(&self, path: PathBuf, secs: f64, is_replay: bool) {
        let _ = self.jobs.send((path, secs, is_replay));
    }

    pub fn finished(&self) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        self.done.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_time_picks_the_end_of_a_replay_and_the_start_of_a_recording() {
        assert_eq!(frame_time(10.0, true), 8.5);
        assert_eq!(frame_time(1.0, true), 0.0);
        assert_eq!(frame_time(30.0, false), 10.0);
        assert!((frame_time(6.0, false) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn hash_is_stable_and_cache_file_follows_the_clip() {
        assert_eq!(hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        let dir = std::env::temp_dir().join(format!("ir-thumbs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("x.mp4");
        std::fs::write(&clip, b"1234").unwrap();
        let a = cache_file(&clip).unwrap();
        assert_eq!(a, cache_file(&clip).unwrap());
        std::fs::write(&clip, b"12345678").unwrap(); // a different size is a different clip
        assert_ne!(a, cache_file(&clip).unwrap());
        assert!(cache_file(&dir.join("missing.mp4")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_round_trip_and_wrong_size_is_rejected() {
        let dir = std::env::temp_dir().join(format!("ir-thumbs-cache-{}", std::process::id()));
        let f = dir.join("a.thumb");
        let pic: Vec<u8> = (0..BYTES).map(|i| (i % 251) as u8).collect();
        write_cache(&f, &pic);
        assert_eq!(read_cache(&f).unwrap(), pic);
        write_cache(&f, &pic[..100]);
        assert!(read_cache(&f).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn makes_a_picture_from_a_real_clip() {
        let Some(ffmpeg) = crate::player::ffmpeg::ffmpeg() else { return }; // no FFmpeg here
        let dir = std::env::temp_dir().join(format!("ir-thumbs-clip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("t - Replay 1.mp4");
        let ok = crate::player::ffmpeg::hidden_command(ffmpeg)
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=10", "-t", "3", "-c:v", "mpeg4", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            let pic = make(&clip, 3.0, true).expect("a picture");
            assert_eq!(pic.len(), BYTES);
            // The test pattern is not a flat colour.
            assert!(pic.chunks(4).any(|p| p[..3] != pic[..3]));
            assert!(get(&clip, 3.0, true).is_some());
            forget(&clip);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
