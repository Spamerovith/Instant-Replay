//! Small shared helpers: time base, paths, wide strings, logging.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

use parking_lot::Mutex;
use windows::core::PCWSTR;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

/// 100-ns units per second (Media Foundation time base used across the app).
pub const HNS: i64 = 10_000_000;

fn qpc_freq() -> i64 {
    static F: OnceLock<i64> = OnceLock::new();
    *F.get_or_init(|| {
        let mut f = 0i64;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut f);
        }
        f.max(1)
    })
}

/// Current QPC time in 100-ns units. Same clock as WASAPI `u64QPCPosition`.
pub fn now_hns() -> i64 {
    let mut c = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut c);
    }
    let f = qpc_freq();
    ((c as i128 * HNS as i128) / f as i128) as i64
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn pcwstr(v: &[u16]) -> PCWSTR {
    PCWSTR(v.as_ptr())
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub fn app_data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("InstantReplay");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn default_videos_dir() -> PathBuf {
    use windows::Win32::UI::Shell::{FOLDERID_Videos, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, None) {
            let s = p.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
            if !s.is_empty() {
                return PathBuf::from(s).join("Instant Replay");
            }
        }
    }
    std::env::var_os("USERPROFILE")
        .map(|p| PathBuf::from(p).join("Videos").join("Instant Replay"))
        .unwrap_or_else(|| PathBuf::from("Instant Replay"))
}

/// Local timestamp for file names: `2026-11-22 18-30-12`.
pub fn timestamp_for_file() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}-{:02}-{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

pub fn sanitize_file_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { ' ' } else { c })
        .collect();
    let t = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let t: String = t.chars().take(60).collect();
    let t = t.trim_end_matches(['.', ' ']).to_string();
    if t.is_empty() {
        "Clip".into()
    } else {
        t
    }
}

pub fn format_duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

pub fn format_bytes(b: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let f = b as f64;
    if f >= 1024.0 * MB {
        trf!(["{:.2} ГБ", "{:.2} GB", "{:.2} ГБ"], f / (1024.0 * MB))
    } else if f >= MB {
        trf!(["{:.0} МБ", "{:.0} MB", "{:.0} МБ"], f / MB)
    } else {
        trf!(["{:.0} КБ", "{:.0} KB", "{:.0} КБ"], f / 1024.0)
    }
}

pub fn free_disk_bytes(path: &std::path::Path) -> Option<u64> {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let mut p = path.to_path_buf();
    while !p.exists() {
        if !p.pop() {
            return None;
        }
    }
    let w = wide(&p.to_string_lossy());
    let mut free = 0u64;
    unsafe { GetDiskFreeSpaceExW(pcwstr(&w), Some(&mut free), None, None).ok()? };
    Some(free)
}

pub fn open_in_explorer(path: &std::path::Path, select: bool) {
    let mut cmd = std::process::Command::new("explorer.exe");
    if select {
        cmd.arg(format!("/select,{}", path.display()));
    } else {
        cmd.arg(path);
    }
    let _ = cmd.spawn();
}

// ---------------------------------------------------------------- logging

struct FileLogger {
    file: Mutex<Option<std::fs::File>>,
}

impl log::Log for FileLogger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info || m.target().starts_with("instant_replay")
    }
    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        let line = format!(
            "{:02}:{:02}:{:02}.{:03} [{}] {}\n",
            t.wHour,
            t.wMinute,
            t.wSecond,
            t.wMilliseconds,
            r.level(),
            r.args()
        );
        if let Some(f) = self.file.lock().as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
        #[cfg(debug_assertions)]
        eprint!("{line}");
    }
    fn flush(&self) {
        if let Some(f) = self.file.lock().as_mut() {
            let _ = f.flush();
        }
    }
}

pub fn init_logging() {
    let dir = app_data_dir();
    let path = dir.join("log.txt");
    // Keep the log bounded: rotate when larger than 2 MB.
    if std::fs::metadata(&path).map(|m| m.len() > 2 * 1024 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("log.old.txt"));
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok();
    static LOGGER: OnceLock<FileLogger> = OnceLock::new();
    let logger = LOGGER.get_or_init(|| FileLogger { file: Mutex::new(file) });
    let _ = log::set_logger(logger);
    log::set_max_level(log::LevelFilter::Info);
}

/// Path of JustView.exe: Explorer registration first, then known folders next to this app.
pub fn find_justview() -> Option<std::path::PathBuf> {
    // The copy built into this exe comes first: it is unpacked together with its FFmpeg.
    if let Some(p) = crate::tools::file("JustView.exe") {
        return Some(p);
    }
    use std::path::PathBuf;
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Classes\\Applications\\JustView.exe\\shell\\open\\command"),
            None,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if ok.is_ok() {
        let s = String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]);
        let exe = match s.strip_prefix('"') {
            Some(rest) => rest.split('"').next().unwrap_or("").to_string(),
            None => s.split(' ').next().unwrap_or("").to_string(),
        };
        let p = PathBuf::from(exe);
        if p.is_file() {
            return Some(p);
        }
    }
    let me = std::env::current_exe().ok()?;
    let dir = me.parent()?;
    let mut c = vec![dir.join("JustView.exe"), dir.join("JustView").join("JustView.exe")];
    if let Some(up2) = dir.parent().and_then(|p| p.parent()) {
        c.push(up2.join("JustView").join("JustView").join("JustView.exe"));
        c.push(up2.join("JustView").join("JustView.exe"));
    }
    if let Some(l) = std::env::var_os("LOCALAPPDATA") {
        c.push(PathBuf::from(l).join("Programs").join("JustView").join("JustView.exe"));
    }
    c.into_iter().find(|p| p.is_file())
}
