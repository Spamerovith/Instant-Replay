//! Running with administrator rights (needed for the exact FPS trace), the way AfterCore does it:
//! the exe is `asInvoker`, at start it restarts itself through "runas" when the setting asks for
//! it; when UAC is declined the program just goes on without the rights.

use windows::core::{w, PCWSTR};

/// This process runs elevated.
pub fn is_elevated() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elev = TOKEN_ELEVATION::default();
        let mut ret = 0u32;
        let ok = GetTokenInformation(token, TokenElevation, Some(&mut elev as *mut _ as *mut _), std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut ret).is_ok();
        let _ = CloseHandle(token);
        ok && elev.TokenIsElevated != 0
    }
}

/// Starts this exe again through "runas" with `args`. False when UAC was declined.
pub fn relaunch_elevated(args: &str) -> bool {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let Ok(exe) = std::env::current_exe() else { return false };
    let exe = crate::util::wide(&exe.to_string_lossy());
    let params = crate::util::wide(args);
    let r = unsafe { ShellExecuteW(None, w!("runas"), PCWSTR(exe.as_ptr()), PCWSTR(params.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL) };
    r.0 as isize > 32
}

/// Removes the task of the old exact-FPS helper (1.18.0 test builds); the trace now runs in the
/// program itself. Needs the rights the task was made with, so only when elevated.
pub fn remove_old_fps_task() {
    std::thread::spawn(|| {
        use std::os::windows::process::CommandExt;
        let run = |args: &[&str]| std::process::Command::new("schtasks").args(args).creation_flags(0x0800_0000).output().map(|o| o.status.success()).unwrap_or(false);
        if run(&["/Query", "/TN", "Instant Replay FPS"]) {
            let _ = run(&["/End", "/TN", "Instant Replay FPS"]);
            let ok = run(&["/Delete", "/TN", "Instant Replay FPS", "/F"]);
            log::info!("old FPS helper task removed: {ok}");
        }
    });
}

/// Arguments of this run for the elevated copy (quoted when they hold spaces) plus the marker.
pub fn forward_args() -> String {
    let mut v: Vec<String> = std::env::args().skip(1).map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a }).collect();
    v.push("--elevated-attempt".into());
    v.join(" ")
}
