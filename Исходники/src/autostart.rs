//! "Start with Windows" via HKCU\...\Run.

use windows::core::w;
use windows::Win32::System::Registry::*;

use crate::util::wide;

const RUN_KEY: windows::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE: windows::core::PCWSTR = w!("InstantReplay");

pub fn set(enabled: bool) {
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
