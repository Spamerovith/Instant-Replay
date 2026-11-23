//! "Start with Windows": with administrator rights a Task Scheduler task with the highest rights
//! at logon (no UAC prompt at every logon, like AfterCore), otherwise HKCU\...\Run.

use windows::core::w;
use windows::Win32::System::Registry::*;

use crate::util::wide;

const RUN_KEY: windows::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE: windows::core::PCWSTR = w!("InstantReplay");

const TASK: &str = "Instant Replay";

fn hidden(args: &[&str]) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("schtasks").args(args).creation_flags(0x0800_0000).output().map(|o| o.status.success()).unwrap_or(false)
}

pub fn set(enabled: bool) {
    // Both ways are cleaned first, then one is written.
    let _ = hidden(&["/Delete", "/TN", TASK, "/F"]);
    if enabled && crate::elevation::is_elevated() {
        if let Ok(exe) = std::env::current_exe() {
            let tr = format!("\"{}\" --tray", exe.display());
            if hidden(&["/Create", "/TN", TASK, "/TR", &tr, "/SC", "ONLOGON", "/RL", "HIGHEST", "/F"]) {
                unsafe {
                    let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE);
                }
                return;
            }
        }
    }
    unsafe {
        if enabled {
            let Ok(exe) = std::env::current_exe() else { return };
            let cmd = format!("\"{}\" --tray", exe.display());
            let data = wide(&cmd);
            let bytes = std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2);
            let r = RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE, REG_SZ.0, Some(bytes.as_ptr() as _), bytes.len() as u32);
            if r.is_err() {
                log::warn!("autostart: {:?}", r);
            }
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE);
        }
    }
}
