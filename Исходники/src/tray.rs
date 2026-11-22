//! Notification-area icon with a native (dark) context menu, built fresh on every open so it
//! always reflects the current state.

use std::sync::{Arc, OnceLock};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::icon::{self, IconState};
use crate::util::wide;

pub const CLASS_NAME: PCWSTR = w!("InstantReplayTrayWindow");
pub const WM_APP_SHOW: u32 = WM_APP + 11;
const WM_APP_TRAY: u32 = WM_APP + 10;
const WM_APP_ICON: u32 = WM_APP + 12;

#[derive(Clone, Copy, Debug)]
pub enum TrayCmd {
    Show,
    SaveReplay,
    ToggleRecording,
    ToggleReplay,
    OpenFolder,
    Exit,
}

pub struct MenuState {
    pub replay_on: bool,
    pub recording: bool,
    pub save_hotkey: String,
    pub record_hotkey: String,
}

struct Ctx {
    state: Arc<dyn Fn() -> MenuState + Send + Sync>,
    on_cmd: Arc<dyn Fn(TrayCmd) + Send + Sync>,
}

static CTX: OnceLock<Ctx> = OnceLock::new();
static HWND_TRAY: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

pub fn start(
    state: Arc<dyn Fn() -> MenuState + Send + Sync>,
    on_cmd: Arc<dyn Fn(TrayCmd) + Send + Sync>,
) {
    let _ = CTX.set(Ctx { state, on_cmd });
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(|| unsafe { tray_thread() })
        .expect("tray thread");
}

/// Switches the tray icon between idle / buffering / recording.
pub fn set_icon(state: IconState) {
    let h = HWND_TRAY.load(std::sync::atomic::Ordering::SeqCst);
    if h != 0 {
        let v = match state {
            IconState::Idle => 0,
            IconState::Active => 1,
            IconState::Recording => 2,
        };
        unsafe {
            let _ = PostMessageW(HWND(h as _), WM_APP_ICON, WPARAM(v), LPARAM(0));
        }
    }
}

/// Opts the process into dark native menus (Windows 10 1903+ / 11).
pub fn enable_dark_menus() {
    unsafe {
        let Ok(ux) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        // SetPreferredAppMode (ordinal 135): 2 = ForceDark. FlushMenuThemes (ordinal 136).
        if let Some(f) = GetProcAddress(ux, windows::core::PCSTR(135 as *const u8)) {
            let f: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            f(2);
        }
        if let Some(f) = GetProcAddress(ux, windows::core::PCSTR(136 as *const u8)) {
            let f: extern "system" fn() = std::mem::transmute(f);
            f();
        }
    }
}

static mut TASKBAR_CREATED: u32 = 0;
static mut ICON_STATE: usize = 1;

unsafe fn add_icon(hwnd: HWND, modify: bool) {
    let state = match ICON_STATE {
        0 => IconState::Idle,
        2 => IconState::Recording,
        _ => IconState::Active,
    };
    let size = GetSystemMetrics(SM_CXSMICON).max(16) as u32;
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_APP_TRAY,
        hIcon: icon::hicon(size, state),
        ..Default::default()
    };
    let tip = match state {
        IconState::Recording => tr!("Instant Replay - идёт запись", "Instant Replay - recording", "Instant Replay - триває запис"),
        IconState::Active => tr!("Instant Replay - буфер активен", "Instant Replay - buffer active", "Instant Replay - буфер активний"),
        IconState::Idle => tr!("Instant Replay - выключен", "Instant Replay - off", "Instant Replay - вимкнено"),
    };
    let t = wide(tip);
    for (i, c) in t.iter().take(nid.szTip.len() - 1).enumerate() {
        nid.szTip[i] = *c;
    }
    let _ = Shell_NotifyIconW(if modify { NIM_MODIFY } else { NIM_ADD }, &nid);
    let _ = DestroyIcon(nid.hIcon);
}

unsafe fn remove_icon(hwnd: HWND) {
    let nid = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 1, ..Default::default() };
    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
}

const ID_SHOW: usize = 1;
const ID_SAVE: usize = 2;
const ID_REC: usize = 3;
const ID_REPLAY: usize = 4;
const ID_FOLDER: usize = 5;
const ID_EXIT: usize = 6;

unsafe fn show_menu(hwnd: HWND) {
    let Some(ctx) = CTX.get() else { return };
    let st = (ctx.state)();
    let Ok(menu) = CreatePopupMenu() else { return };
    let item = |flags: MENU_ITEM_FLAGS, id: usize, text: String| {
        let w = wide(&text);
        let _ = AppendMenuW(menu, flags, id, PCWSTR(w.as_ptr()));
    };
    let save_flags = if st.replay_on { MF_STRING } else { MF_STRING | MF_GRAYED };
    item(save_flags, ID_SAVE, trf!(["Сохранить повтор\t{}", "Save replay\t{}", "Зберегти повтор\t{}"], st.save_hotkey));
    item(
        MF_STRING,
        ID_REC,
        if st.recording { trf!(["Остановить запись\t{}", "Stop recording\t{}", "Зупинити запис\t{}"], st.record_hotkey) } else { trf!(["Начать запись\t{}", "Start recording\t{}", "Почати запис\t{}"], st.record_hotkey) },
    );
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    item(if st.replay_on { MF_STRING | MF_CHECKED } else { MF_STRING }, ID_REPLAY, "Instant Replay".into());
    item(MF_STRING, ID_FOLDER, tr!("Открыть папку с клипами", "Open clips folder", "Відкрити теку з кліпами").into());
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
    item(MF_STRING, ID_SHOW, tr!("Открыть Instant Replay", "Open Instant Replay", "Відкрити Instant Replay").into());
    item(MF_STRING, ID_EXIT, tr!("Выход", "Exit", "Вихід").into());
    let _ = SetMenuDefaultItem(menu, ID_SHOW as u32, 0);

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_RIGHTALIGN, pt.x, pt.y, 0, hwnd, None);
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
    let c = match cmd.0 as usize {
        ID_SHOW => Some(TrayCmd::Show),
        ID_SAVE => Some(TrayCmd::SaveReplay),
        ID_REC => Some(TrayCmd::ToggleRecording),
        ID_REPLAY => Some(TrayCmd::ToggleReplay),
        ID_FOLDER => Some(TrayCmd::OpenFolder),
        ID_EXIT => Some(TrayCmd::Exit),
        _ => None,
    };
    if let Some(c) = c {
        (ctx.on_cmd)(c);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_APP_TRAY => {
            match lp.0 as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    if let Some(ctx) = CTX.get() {
                        let _ = SetForegroundWindow(hwnd);
                        (ctx.on_cmd)(TrayCmd::Show);
                    }
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_SHOW => {
            if let Some(ctx) = CTX.get() {
                (ctx.on_cmd)(TrayCmd::Show);
            }
            LRESULT(0)
        }
        WM_APP_ICON => {
            ICON_STATE = wp.0;
            add_icon(hwnd, true);
            LRESULT(0)
        }
        WM_DESTROY => {
            remove_icon(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        m if m == TASKBAR_CREATED && m != 0 => {
            // Explorer restarted: the icon must be added again.
            add_icon(hwnd, false);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn tray_thread() {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: CLASS_NAME, ..Default::default() };
    RegisterClassW(&wc);
    TASKBAR_CREATED = RegisterWindowMessageW(w!("TaskbarCreated"));
    let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE(0), CLASS_NAME, w!("Instant Replay Tray"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, hinst, None) else {
        return;
    };
    // Allow the TaskbarCreated broadcast through UIPI.
    let _ = ChangeWindowMessageFilterEx(hwnd, TASKBAR_CREATED, MSGFLT_ALLOW, None);
    let _ = ChangeWindowMessageFilterEx(hwnd, WM_APP_SHOW, MSGFLT_ALLOW, None);
    HWND_TRAY.store(hwnd.0 as isize, std::sync::atomic::Ordering::SeqCst);
    add_icon(hwnd, false);
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

pub fn remove() {
    let h = HWND_TRAY.load(std::sync::atomic::Ordering::SeqCst);
    if h != 0 {
        unsafe { remove_icon(HWND(h as _)) };
    }
}

/// Asks an already running instance to show its window. Returns true if one was found.
pub fn signal_existing_instance() -> bool {
    unsafe {
        match FindWindowW(CLASS_NAME, None) {
            Ok(h) if !h.is_invalid() => {
                let _ = PostMessageW(h, WM_APP_SHOW, WPARAM(0), LPARAM(0));
                true
            }
            _ => false,
        }
    }
}
