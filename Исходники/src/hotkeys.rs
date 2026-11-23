//! Global hotkeys via RegisterHotKey on a dedicated message thread.

use std::sync::Arc;

use parking_lot::Mutex;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Hotkey;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    SaveReplay,
    SaveLast,
    ToggleRecording,
    ToggleReplay,
    ToggleOverlay,
    Screenshot,
    ToggleHud,
}

impl Action {
    pub const ALL: [Action; 7] = [Action::SaveReplay, Action::SaveLast, Action::ToggleRecording, Action::ToggleReplay, Action::ToggleOverlay, Action::Screenshot, Action::ToggleHud];
    fn id(self) -> i32 {
        match self {
            Action::SaveReplay => 1,
            Action::ToggleRecording => 2,
            Action::ToggleReplay => 3,
            Action::ToggleOverlay => 4,
            Action::SaveLast => 5,
            Action::Screenshot => 6,
            Action::ToggleHud => 7,
        }
    }
    fn from_id(id: i32) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.id() == id)
    }
}

const WM_APP_RELOAD: u32 = WM_APP + 1;
const WM_APP_SUSPEND: u32 = WM_APP + 2;

#[derive(Default)]
struct Shared {
    bindings: Vec<(Action, Hotkey)>,
    failed: Vec<Action>,
}

#[derive(Clone)]
pub struct Hotkeys {
    thread_id: u32,
    shared: Arc<Mutex<Shared>>,
}

impl Hotkeys {
    pub fn start(bindings: Vec<(Action, Hotkey)>, on_action: impl Fn(Action) + Send + 'static) -> Hotkeys {
        let shared = Arc::new(Mutex::new(Shared { bindings, failed: vec![] }));
        let (tx, rx) = std::sync::mpsc::channel();
        let sh = shared.clone();
        std::thread::Builder::new()
            .name("hotkeys".into())
            .spawn(move || unsafe {
                let mut msg = MSG::default();
                // Create the thread message queue before reporting the id.
                let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
                let _ = tx.send(GetCurrentThreadId());
                register_all(&sh);
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    match msg.message {
                        WM_HOTKEY => {
                            if let Some(a) = Action::from_id(msg.wParam.0 as i32) {
                                on_action(a);
                            }
                        }
                        WM_APP_RELOAD => register_all(&sh),
                        WM_APP_SUSPEND => unregister_all(),
                        _ => {}
                    }
                }
            })
            .expect("hotkey thread");
        let thread_id = rx.recv().unwrap_or(0);
        Hotkeys { thread_id, shared }
    }

    pub fn update(&self, bindings: Vec<(Action, Hotkey)>) {
        self.shared.lock().bindings = bindings;
        self.post(WM_APP_RELOAD);
    }

    /// Temporarily releases all hotkeys (while the user records a new combination).
    pub fn suspend(&self) {
        self.post(WM_APP_SUSPEND);
    }

    pub fn resume(&self) {
        self.post(WM_APP_RELOAD);
    }

    pub fn failed(&self) -> Vec<Action> {
        self.shared.lock().failed.clone()
    }

    fn post(&self, m: u32) {
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, m, WPARAM(0), LPARAM(0));
        }
    }
}

unsafe fn unregister_all() {
    for a in Action::ALL {
        let _ = UnregisterHotKey(None, a.id());
    }
}

unsafe fn register_all(sh: &Mutex<Shared>) {
    unregister_all();
    let bindings = sh.lock().bindings.clone();
    let mut failed = Vec::new();
    for (a, hk) in bindings {
        if !hk.is_set() {
            continue;
        }
        let mut mods = MOD_NOREPEAT;
        if hk.ctrl {
            mods |= MOD_CONTROL;
        }
        if hk.alt {
            mods |= MOD_ALT;
        }
        if hk.shift {
            mods |= MOD_SHIFT;
        }
        if hk.win {
            mods |= MOD_WIN;
        }
        if RegisterHotKey(None, a.id(), mods, hk.vk).is_err() {
            log::warn!("hotkey {:?} ({}) is taken by another program", a, describe(&hk));
            failed.push(a);
        }
    }
    sh.lock().failed = failed;
}

/// Human-readable combination, e.g. `Alt + F10`.
pub fn describe(hk: &Hotkey) -> String {
    if !hk.is_set() {
        return tr!("Не задано", "Not set", "Не задано").into();
    }
    let mut parts: Vec<String> = Vec::new();
    if hk.ctrl {
        parts.push("Ctrl".into());
    }
    if hk.alt {
        parts.push("Alt".into());
    }
    if hk.shift {
        parts.push("Shift".into());
    }
    if hk.win {
        parts.push("Win".into());
    }
    parts.push(key_name(hk.vk));
    parts.join(" + ")
}

pub fn key_name(vk: u32) -> String {
    match vk {
        0x70..=0x87 => return format!("F{}", vk - 0x6F),
        0x30..=0x39 | 0x41..=0x5A => return char::from_u32(vk).unwrap().to_string(),
        0x60..=0x69 => return format!("Num {}", vk - 0x60),
        0x20 => return "Space".into(),
        0x2C => return "PrtSc".into(),
        0x13 => return "Pause".into(),
        0x91 => return "Scroll Lock".into(),
        0x2D => return "Insert".into(),
        0x2E => return "Delete".into(),
        0x24 => return "Home".into(),
        0x23 => return "End".into(),
        0x21 => return "PgUp".into(),
        0x22 => return "PgDn".into(),
        0x25 => return "←".into(),
        0x26 => return "↑".into(),
        0x27 => return "→".into(),
        0x28 => return "↓".into(),
        0x0D => return "Enter".into(),
        0x09 => return "Tab".into(),
        0x6A => return "Num *".into(),
        0x6B => return "Num +".into(),
        0x6D => return "Num -".into(),
        0x6E => return "Num .".into(),
        0x6F => return "Num /".into(),
        _ => {}
    }
    unsafe {
        let sc = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC);
        let mut buf = [0u16; 64];
        let n = GetKeyNameTextW((sc << 16) as i32, &mut buf);
        if n > 0 {
            return String::from_utf16_lossy(&buf[..n as usize]);
        }
    }
    format!("Key {vk:#04x}")
}

fn is_down(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
}

/// Result of polling the keyboard while the user assigns a hotkey.
pub enum CaptureResult {
    Pending { ctrl: bool, alt: bool, shift: bool, win: bool },
    Done(Hotkey),
    Cancel,
    Clear,
}

/// Polls the physical keyboard state. Works for any key, including ones the UI toolkit
/// does not report (numpad, PrtSc, Pause, F13–F24).
pub fn poll_capture() -> CaptureResult {
    let ctrl = is_down(VK_CONTROL.0);
    let alt = is_down(VK_MENU.0);
    let shift = is_down(VK_SHIFT.0);
    let win = is_down(VK_LWIN.0) || is_down(VK_RWIN.0);
    let modifier_vks: [u16; 11] = [0x10, 0x11, 0x12, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x5B, 0x5C];
    for vk in 0x08u16..=0xFE {
        if modifier_vks.contains(&vk) || (0x01..=0x06).contains(&vk) {
            continue;
        }
        if !is_down(vk) {
            continue;
        }
        let none = !ctrl && !alt && !shift && !win;
        if vk == 0x1B && none {
            return CaptureResult::Cancel;
        }
        if (vk == 0x08 || vk == 0x2E) && none {
            return CaptureResult::Clear;
        }
        return CaptureResult::Done(Hotkey { vk: vk as u32, ctrl, alt, shift, win });
    }
    CaptureResult::Pending { ctrl, alt, shift, win }
}
