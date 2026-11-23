//! Update check against GitHub Releases and in-place self-update.
//! Uses the curl.exe that ships with Windows 10/11, so no TLS stack is linked in.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub page: String,
    pub exe_url: Option<String>,
}

fn curl() -> Command {
    let sys = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let exe = sys.join("System32").join("curl.exe");
    let mut c = Command::new(if exe.is_file() { exe } else { PathBuf::from("curl.exe") });
    c.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null()).stderr(Stdio::null());
    c
}

fn parse_version(v: &str) -> Vec<u64> {
    v.trim().trim_start_matches(['v', 'V']).split(['.', '-']).take(3).map(|p| p.parse().unwrap_or(0)).collect()
}

/// `a` is newer than `b`.
pub fn newer(a: &str, b: &str) -> bool {
    let (mut x, mut y) = (parse_version(a), parse_version(b));
    x.resize(3, 0);
    y.resize(3, 0);
    x > y
}

/// Latest release of `repo` ("owner/name"). `asset_suffix` picks the exe asset (e.g. ".exe").
pub fn latest(repo: &str, user_agent: &str) -> Option<Release> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let out = curl()
        .args(["-sfL", "--max-time", "15", "-H", "Accept: application/vnd.github+json", "-A", user_agent])
        .arg(&url)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let version = v["tag_name"].as_str()?.trim_start_matches(['v', 'V']).to_string();
    let page = v["html_url"].as_str().unwrap_or("").to_string();
    let exe_url = v["assets"].as_array().and_then(|a| {
        a.iter()
            .find(|x| x["name"].as_str().map(|n| n.to_ascii_lowercase().ends_with(".exe")).unwrap_or(false))
            .and_then(|x| x["browser_download_url"].as_str().map(|s| s.to_string()))
    });
    Some(Release { version, page, exe_url })
}

#[allow(dead_code)]
/// Background check. The slot gets `Some` only when a newer, not-skipped version exists.
pub fn spawn_check(repo: &'static str, user_agent: &'static str, current: &'static str, skip: String, done: impl FnOnce() + Send + 'static) -> Arc<Mutex<Option<Release>>> {
    let slot = Arc::new(Mutex::new(None));
    let s2 = slot.clone();
    std::thread::spawn(move || {
        if let Some(r) = latest(repo, user_agent) {
            if newer(&r.version, current) && r.version != skip {
                *s2.lock().unwrap() = Some(r);
                done();
            }
        }
    });
    slot
}

fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("old")
}

/// Removes the previous exe left after an update.
pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
    }
}

/// Downloads the new exe, swaps it in place of the running one and starts it with `args`.
/// On success the caller must exit right away.
pub fn install(rel: &Release, user_agent: &str, args: &[std::ffi::OsString]) -> Result<(), String> {
    let url = rel.exe_url.as_ref().ok_or("no exe in release")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let tmp = exe.with_extension("download");
    let _ = std::fs::remove_file(&tmp);
    let ok = curl()
        .args(["-sfL", "--max-time", "600", "-A", user_agent, "-o"])
        .arg(&tmp)
        .arg(url)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let head = std::fs::read(&tmp).ok().map(|b| b.len() > 512 * 1024 && b.starts_with(b"MZ")).unwrap_or(false);
    if !ok || !head {
        let _ = std::fs::remove_file(&tmp);
        return Err("download failed".into());
    }
    let old = old_path(&exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::rename(&tmp, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(e.to_string());
    }
    Command::new(&exe).args(args).spawn().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn open_page(url: &str) {
    let _ = Command::new("explorer.exe").arg(url).spawn();
}

#[allow(dead_code)]
/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}
