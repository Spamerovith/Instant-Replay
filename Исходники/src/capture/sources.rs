//! Enumeration of capturable monitors and windows.

use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, DISPLAY_DEVICEW, HDC,
    HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::Target;
use crate::config::CaptureSource;
use crate::util::from_wide;

#[derive(Clone, Debug)]
pub struct MonitorInfo {
    pub device: String,
    #[allow(dead_code)]
    pub name: String,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
    pub handle: isize,
}

#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
    pub exe: String,
}

pub fn monitors() -> Vec<MonitorInfo> {
    unsafe extern "system" fn cb(h: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
        let list = &mut *(lp.0 as *mut Vec<MonitorInfo>);
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(h, &mut mi as *mut _ as *mut MONITORINFO).as_bool() {
            let device = from_wide(&mi.szDevice);
            let r = mi.monitorInfo.rcMonitor;
            let mut dd = DISPLAY_DEVICEW { cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
            let dev_w = crate::util::wide(&device);
            let friendly = if EnumDisplayDevicesW(crate::util::pcwstr(&dev_w), 0, &mut dd, 0).as_bool() {
                from_wide(&dd.DeviceString)
            } else {
                String::new()
            };
            let primary = mi.monitorInfo.dwFlags & 1 != 0;
            list.push(MonitorInfo {
                device,
                name: friendly,
                width: r.right - r.left,
                height: r.bottom - r.top,
                primary,
                handle: h.0 as isize,
            });
        }
        TRUE
    }
    let mut list: Vec<MonitorInfo> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut list as *mut _ as isize));
    }
    list.sort_by_key(|m| (!m.primary, m.device.clone()));
    list
}

fn exe_of(hwnd: HWND) -> String {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit('\\').next().unwrap_or("").to_string()
    }
}

pub fn windows() -> Vec<WindowInfo> {
    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
        let list = &mut *(lp.0 as *mut Vec<HWND>);
        list.push(h);
        TRUE
    }
    let mut hwnds: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut hwnds as *mut _ as isize));
    }
    let own_pid = std::process::id();
    let mut out = Vec::new();
    for h in hwnds {
        unsafe {
            if !IsWindowVisible(h).as_bool() {
                continue;
            }
            let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
            if ex & WS_EX_TOOLWINDOW.0 != 0 {
                continue;
            }
            if GetWindow(h, GW_OWNER).map(|o| !o.is_invalid()).unwrap_or(false) {
                continue;
            }
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut cloaked as *mut _ as _, 4);
            if cloaked != 0 {
                continue;
            }
            let len = GetWindowTextLengthW(h);
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len as usize + 1];
            GetWindowTextW(h, &mut buf);
            let title = from_wide(&buf);
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid == own_pid {
                continue;
            }
            let exe = exe_of(h);
            if exe.eq_ignore_ascii_case("explorer.exe") && title == "Program Manager" {
                continue;
            }
            if exe.eq_ignore_ascii_case("TextInputHost.exe") || exe.eq_ignore_ascii_case("ApplicationFrameHost.exe") && title.is_empty() {
                continue;
            }
            out.push(WindowInfo { hwnd: h.0 as isize, title, exe });
        }
    }
    out
}

/// Resolves a configured source into a live capture target.
pub fn resolve(src: &CaptureSource) -> Option<(Target, String)> {
    match src {
        CaptureSource::Monitor { device } => {
            let mons = monitors();
            let m = mons
                .iter()
                .find(|m| !device.is_empty() && &m.device == device)
                .or_else(|| mons.iter().find(|m| m.primary))
                .or_else(|| mons.first())?;
            Some((Target::Monitor(HMONITOR(m.handle as _)), "Desktop".to_string()))
        }
        CaptureSource::Window { title, exe } => {
            let wins = windows();
            let w = wins
                .iter()
                .find(|w| w.exe.eq_ignore_ascii_case(exe) && &w.title == title)
                .or_else(|| wins.iter().find(|w| !exe.is_empty() && w.exe.eq_ignore_ascii_case(exe)))
                .or_else(|| wins.iter().find(|w| &w.title == title))?;
            let label = exe.trim_end_matches(".exe").trim_end_matches(".EXE").to_string();
            Some((Target::Window(HWND(w.hwnd as _)), if label.is_empty() { w.title.clone() } else { label }))
        }
    }
}
