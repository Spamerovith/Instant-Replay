//! JustView and FFmpeg built into the exe. The packed files are unpacked once into
//! `%LOCALAPPDATA%\InstantReplay\tools` (again after an update), so the app works from a single
//! download. Without packed files (plain `cargo test`) installed tools are used instead.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

include!(concat!(env!("OUT_DIR"), "/embedded_tools.rs"));

const STAMP: &str = ".stamp";

fn root() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("APPDATA")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("InstantReplay").join("tools")
}

/// Identifies the packed set: the app version and every file's name and size.
fn stamp() -> String {
    let mut s = env!("CARGO_PKG_VERSION").to_string();
    for (name, data) in EMBEDDED {
        s.push_str(&format!("|{name}:{}", data.len()));
    }
    s
}

fn unpack(name: &str, data: &[u8], dir: &Path) -> std::io::Result<()> {
    let target = dir.join(name);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = target.with_extension("tmp");
    {
        let mut out = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        std::io::copy(&mut flate2::read::GzDecoder::new(data), &mut out)?;
    }
    std::fs::rename(&tmp, &target)
}

fn prepare(dir: &Path) -> bool {
    let want = stamp();
    if std::fs::read_to_string(dir.join(STAMP)).map(|s| s == want).unwrap_or(false) {
        return true;
    }
    let mut ok = true;
    for (name, data) in EMBEDDED {
        if let Err(e) = unpack(name, data, dir) {
            // A running JustView keeps its exe locked: use the old copy then.
            log::warn!("tools: could not unpack {name}: {e}");
            ok = false;
        }
    }
    if ok {
        let _ = std::fs::write(dir.join(STAMP), want);
    }
    true
}

/// Folder with the unpacked tools; unpacks them on the first call (a few seconds the first time).
/// `None` when the exe carries no packed tools.
pub fn dir() -> Option<&'static Path> {
    static D: OnceLock<Option<PathBuf>> = OnceLock::new();
    D.get_or_init(|| {
        if EMBEDDED.is_empty() {
            return None;
        }
        let d = root();
        let _ = std::fs::create_dir_all(&d);
        prepare(&d).then_some(d)
    })
    .as_deref()
}

/// A tool inside the folder, e.g. `JustView.exe` or `ffmpeg/ffprobe.exe`.
pub fn file(rel: &str) -> Option<PathBuf> {
    let p = dir()?.join(rel);
    p.is_file().then_some(p)
}

/// Unpacks in the background at startup so the first clip opens at once.
pub fn warm_up() {
    if !EMBEDDED.is_empty() {
        let _ = std::thread::Builder::new().name("tools".into()).spawn(|| {
            let _ = dir();
        });
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn unpack_restores_gzipped_file_in_subfolder() {
        let dir = std::env::temp_dir().join(format!("ir-tools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(b"hello ffmpeg").unwrap();
        let gz = enc.finish().unwrap();
        unpack("ffmpeg/ffmpeg.exe", &gz, &dir).unwrap();
        assert_eq!(std::fs::read(dir.join("ffmpeg").join("ffmpeg.exe")).unwrap(), b"hello ffmpeg");
        assert!(!dir.join("ffmpeg").join("ffmpeg.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
