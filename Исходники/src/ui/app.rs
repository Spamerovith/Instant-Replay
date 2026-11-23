//! Main window.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{vec2, Align, Color32, Layout, RichText, Rounding, Ui};

use super::theme::{self, color, glyph, radius, size, space};
use super::viewer::{Viewer, ViewerPrefs};
use super::widgets::{self as w, Kind, KeycapState};
use crate::audio::{self, AudioDevice};
use crate::capture::sources::{self, MonitorInfo, WindowInfo};
use crate::config::{CaptureSource, Codec, Config, Hotkey, Preset, RateControl, Resolution};
use crate::engine::{Command, EngineHandle, Status};
use crate::hotkeys::{self, Action, CaptureResult, Hotkeys};
use crate::util;

pub struct UiShared {
    pub hwnd: AtomicIsize,
    pub visible: AtomicBool,
    pub quitting: AtomicBool,
    pub overlay_request: AtomicBool,
    pub in_overlay: AtomicBool,
    /// Set when a clip opens from the overlay: the game must not take the focus back from it.
    skip_refocus: AtomicBool,
    /// Window state before the overlay took the window over; restored exactly when it closes.
    overlay_saved: parking_lot::Mutex<Option<OverlaySaved>>,
}

impl UiShared {
    pub fn new(visible: bool) -> Arc<Self> {
        Arc::new(UiShared {
            hwnd: AtomicIsize::new(0),
            visible: AtomicBool::new(visible),
            quitting: AtomicBool::new(false),
            overlay_request: AtomicBool::new(false),
            in_overlay: AtomicBool::new(false),
            skip_refocus: AtomicBool::new(false),
            overlay_saved: parking_lot::Mutex::new(None),
        })
    }

    pub fn show(&self) {
        let h = self.hwnd.load(Ordering::SeqCst);
        if h == 0 {
            return;
        }
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::*;
        unsafe {
            let hwnd = HWND(h as _);
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            } else {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
            let _ = SetForegroundWindow(hwnd);
        }
        self.visible.store(true, Ordering::SeqCst);
    }

    /// Called from the hotkey thread: turns the window into the overlay right away, so the
    /// regular program window never shows up in its usual place.
    pub fn request_overlay(&self) {
        if !self.in_overlay.load(Ordering::SeqCst) && self.overlay_saved.lock().is_none() {
            self.place_as_overlay();
        }
        self.overlay_request.store(true, Ordering::SeqCst);
    }

    /// Remembers the window state (hidden / minimized / maximized / normal, its place and the
    /// game window), then puts the window at the right edge of the game's monitor, frameless and
    /// on top. Done with Win32 directly: the window must not appear at its usual size first.
    fn place_as_overlay(&self) {
        use windows::Win32::Foundation::{HWND, RECT};
        use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
        use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
        use windows::Win32::UI::WindowsAndMessaging::*;
        let h = self.hwnd.load(Ordering::SeqCst);
        if h == 0 {
            return;
        }
        unsafe {
            let hwnd = HWND(h as _);
            let fg = GetForegroundWindow();
            let mon = MonitorFromWindow(fg, MONITOR_DEFAULTTOPRIMARY);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(mon, &mut mi).as_bool() {
                return;
            }
            let (mut dpi, mut dpi_y) = (96u32, 96u32);
            if GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y).is_err() {
                dpi = 96;
            }
            let work = mi.rcWork;
            let w = (size::OVERLAY_W * dpi.max(96) as f32 / 96.0).round() as i32;
            let rect = RECT { left: work.right - w, top: work.top, right: work.right, bottom: work.bottom };

            let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            let _ = GetWindowPlacement(hwnd, &mut wp);
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
            *self.overlay_saved.lock() = Some(OverlaySaved {
                placement: wp,
                style,
                hidden: !self.visible.load(Ordering::SeqCst),
                prev_fg: fg.0 as isize,
            });

            // The frame is removed here and not through egui's `Decorations`: any winit
            // window-flag change re-applies its own idea of visibility and would show the
            // program window again after the overlay closes.
            // Invisible until the overlay has drawn its first frame (no flash of the old content).
            set_transitions(h, false);
            set_opaque(h, false);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(600));
                set_opaque(h, true); // safety net: never leave the window invisible
            });
            SetWindowLongPtrW(hwnd, GWL_STYLE, style & !OVERLAY_FRAME);
            if IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() {
                // Leave the minimized / maximized state straight into the overlay rectangle
                // (rcNormalPosition is in workspace coordinates; SetWindowPos below fixes any offset).
                let (ox, oy) = (work.left - mi.rcMonitor.left, work.top - mi.rcMonitor.top);
                let mut ov = wp;
                ov.flags = WINDOWPLACEMENT_FLAGS(0);
                ov.showCmd = SW_SHOWNOACTIVATE.0 as u32;
                ov.rcNormalPosition = RECT { left: rect.left - ox, top: rect.top - oy, right: rect.right - ox, bottom: rect.bottom - oy };
                let _ = SetWindowPlacement(hwnd, &ov);
            }
            // SWP_NOSENDCHANGING skips the 840 px minimum width of the regular window.
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                rect.left,
                rect.top,
                w,
                rect.bottom - rect.top,
                SWP_NOSENDCHANGING | SWP_FRAMECHANGED | SWP_SHOWWINDOW | SWP_NOACTIVATE,
            );
            // The window takes the focus only once the hotkey's modifiers are released (a lone Alt
            // key-up arriving at the freshly focused window would start its menu mode). Done on
            // its own thread so that the overlay itself shows up without waiting for the keys.
            std::thread::spawn(move || {
                use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
                for _ in 0..40 {
                    let down = |vk: u16| unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 };
                    if !(down(VK_MENU.0) || down(VK_CONTROL.0) || down(VK_SHIFT.0)) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                unsafe {
                    let _ = SetForegroundWindow(HWND(h as _));
                }
            });
        }
        self.visible.store(true, Ordering::SeqCst);
    }

    /// Puts the window back exactly as it was before the overlay. With `open_full` the regular
    /// window is shown and focused; otherwise a hidden window is hidden again, a minimized one is
    /// minimized again and an open one goes back behind the game, which gets the focus back.
    fn restore_after_overlay(&self, open_full: bool) {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::*;
        let Some(s) = self.overlay_saved.lock().take() else { return };
        let skip_refocus = self.skip_refocus.swap(false, Ordering::SeqCst);
        let h = self.hwnd.load(Ordering::SeqCst);
        if h == 0 {
            return;
        }
        unsafe {
            let hwnd = HWND(h as _);
            let prev_fg = HWND(s.prev_fg as _);
            let mut wp = s.placement;
            let was_min = wp.showCmd == SW_SHOWMINIMIZED.0 as u32;
            let was_max = wp.showCmd == SW_SHOWMAXIMIZED.0 as u32 || (was_min && (wp.flags.0 & WPF_RESTORETOMAXIMIZED.0) != 0);
            if !open_full {
                // Give the focus back to the game while we still own it.
                if !skip_refocus && !prev_fg.is_invalid() && prev_fg != hwnd && GetForegroundWindow() == hwnd {
                    let _ = SetForegroundWindow(prev_fg);
                }
                if s.hidden {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
            // Frame back first, then the old placement: it restores the outer rectangle.
            let cur = GetWindowLongPtrW(hwnd, GWL_STYLE);
            SetWindowLongPtrW(hwnd, GWL_STYLE, (cur & !OVERLAY_FRAME) | (s.style & OVERLAY_FRAME));
            // Behind the game the window goes to the very bottom of the z-order, so it can never
            // end up above the game (a top-most game used to leave it on top).
            let behind_game = !open_full && !s.hidden && !prev_fg.is_invalid() && prev_fg != hwnd;
            let _ = SetWindowPos(
                hwnd,
                if behind_game { HWND_BOTTOM } else { HWND_NOTOPMOST },
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
            if open_full {
                wp.showCmd = if was_max { SW_SHOWMAXIMIZED.0 } else { SW_SHOWNORMAL.0 } as u32;
                let _ = SetWindowPlacement(hwnd, &wp);
                let _ = SetForegroundWindow(hwnd);
                self.visible.store(true, Ordering::SeqCst);
                return;
            }
            if s.hidden {
                wp.showCmd = SW_HIDE.0 as u32;
                let _ = SetWindowPlacement(hwnd, &wp);
                self.visible.store(false, Ordering::SeqCst);
            } else if was_min {
                wp.showCmd = SW_SHOWMINNOACTIVE.0 as u32;
                let _ = SetWindowPlacement(hwnd, &wp);
            } else {
                wp.showCmd = if was_max { SW_SHOWMAXIMIZED.0 } else { SW_SHOWNOACTIVATE.0 } as u32;
                let _ = SetWindowPlacement(hwnd, &wp);
                // Back behind the game, not on top of it.
                if behind_game {
                    let _ = SetWindowPos(hwnd, HWND_BOTTOM, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
            }
        }
    }

    /// Overlay window as the side panel at the right edge of its monitor.
    pub fn overlay_panel_rect(&self) {
        self.overlay_resize(false);
    }

    /// Overlay window as the quick-view card in the middle of its monitor.
    pub fn overlay_quick_rect(&self) {
        self.overlay_resize(true);
    }

    fn overlay_resize(&self, quick: bool) {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
        use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
        use windows::Win32::UI::WindowsAndMessaging::*;
        let h = self.hwnd.load(Ordering::SeqCst);
        if h == 0 || !self.in_overlay.load(Ordering::SeqCst) {
            return;
        }
        unsafe {
            let hwnd = HWND(h as _);
            let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(mon, &mut mi).as_bool() {
                return;
            }
            let (mut dpi, mut dpi_y) = (96u32, 96u32);
            if GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y).is_err() {
                dpi = 96;
            }
            let k = dpi.max(96) as f32 / 96.0;
            let work = mi.rcWork;
            let (ww, wh) = ((work.right - work.left) as f32, (work.bottom - work.top) as f32);
            let (x, y, w, hgt) = if quick {
                let (w, hgt) = quick_card_size(ww, wh, k);
                (work.left + ((ww - w) / 2.0) as i32, work.top + ((wh - hgt) / 2.0) as i32, w as i32, hgt as i32)
            } else {
                let w = (size::OVERLAY_W * k).round() as i32;
                (work.right - w, work.top, w, work.bottom - work.top)
            };
            let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, hgt, SWP_NOSENDCHANGING | SWP_SHOWWINDOW);
        }
    }

    /// Keeps the overlay out of screen capture (it is never recorded).
    pub fn exclude_from_capture(&self, exclude: bool) {
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE};
        let h = self.hwnd.load(Ordering::SeqCst);
        // Diagnostic switch for taking screenshots of the overlay itself.
        let exclude = exclude && std::env::var_os("INSTANT_REPLAY_ALLOW_CAPTURE").is_none();
        if h != 0 {
            unsafe {
                let _ = SetWindowDisplayAffinity(windows::Win32::Foundation::HWND(h as _), if exclude { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE });
            }
        }
    }

    pub fn hide(&self) {
        let h = self.hwnd.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(windows::Win32::Foundation::HWND(h as _), windows::Win32::UI::WindowsAndMessaging::SW_HIDE);
            }
        }
        self.visible.store(false, Ordering::SeqCst);
    }
}

/// Quick-view window size in pixels for a work area of `ww` x `wh` (`k` = DPI scale): 72 % of the
/// width, a 16:9 picture plus header and controls, at most 85 % of the height.
fn quick_card_size(ww: f32, wh: f32, k: f32) -> (f32, f32) {
    let chrome = (20.0 * 2.0 + 32.0 + 4.0 + 18.0 + 12.0 + 12.0 + 40.0) * k; // padding, header, controls
    let side = (40.0 + 2.0 * super::quick::ARROW_COL) * k; // padding and the arrow columns
    let mut w = (ww * 0.72).round();
    let mut h = (w - side) * 9.0 / 16.0 + chrome;
    let max_h = (wh * 0.85).round();
    if h > max_h {
        h = max_h;
        w = (h - chrome) * 16.0 / 9.0 + side;
    }
    (w.round(), h.round())
}

/// Style bits of the regular window frame that the overlay goes without.
const OVERLAY_FRAME: isize = (windows::Win32::UI::WindowsAndMessaging::WS_CAPTION.0 | windows::Win32::UI::WindowsAndMessaging::WS_THICKFRAME.0) as isize;

#[derive(Clone, Copy)]
struct OverlaySaved {
    placement: windows::Win32::UI::WindowsAndMessaging::WINDOWPLACEMENT,
    /// Window style before the frame was taken off.
    style: isize,
    hidden: bool,
    prev_fg: isize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Video,
    Audio,
    Games,
    Hotkeys,
    General,
}

fn pages() -> [(Page, &'static str, &'static str); 6] {
    [
    (Page::Home, glyph::HOME, tr!("Главная", "Home", "Головна")),
    (Page::Video, glyph::VIDEO, tr!("Видео", "Video", "Відео")),
    (Page::Audio, glyph::VOLUME, tr!("Звук", "Audio", "Звук")),
    (Page::Games, glyph::GAME, tr!("Игры", "Games", "Ігри")),
    (Page::Hotkeys, glyph::KEYBOARD, tr!("Горячие клавиши", "Hotkeys", "Гарячі клавіші")),
    (Page::General, glyph::SETTINGS, tr!("Общие", "General", "Загальні")),
    ]
}

const REPLAY_STEPS: [u32; 11] = [15, 30, 45, 60, 90, 120, 180, 300, 600, 900, 1200];
const LAST_STEPS: [u32; 11] = [5, 10, 15, 20, 30, 45, 60, 90, 120, 180, 300];
const FPS_STEPS:[u32; 6] = [30, 60, 120, 144, 165, 240];
const AUDIO_BITRATES: [u32; 3] = [128, 160, 192];

struct OverlayMode {
    focused: bool,
    opened: Instant,
    /// Frames drawn so far: the overlay stays see-through until its first frame is on screen.
    frames: u32,
    /// The overlay shows the full gallery instead of the main page.
    gallery: bool,
}

pub struct App {
    overlay: Option<OverlayMode>,
    /// Hide back to the tray once the window-restore commands have been applied.
    hide_at: Option<Instant>,
    cfg: Config,
    applied: Config,
    changed_at: Option<Instant>,
    engine: EngineHandle,
    hotkeys: Hotkeys,
    shared: Arc<UiShared>,
    quit: Arc<dyn Fn() + Send + Sync>,
    page: Page,
    page_changed: Instant,
    monitors: Vec<MonitorInfo>,
    windows: Vec<WindowInfo>,
    mics: Vec<AudioDevice>,
    capturing: Option<Action>,
    capture_hint: String,
    /// Встроенный плеер открытого клипа (если JustView не найден).
    viewer: Option<Viewer>,
    viewer_prefs: ViewerPrefs,
    /// Clip playing in the overlay's quick view.
    quick: Option<super::quick::QuickView>,
    /// Clips the quick view steps through with its arrows, and the one shown.
    quick_list: Vec<crate::engine::ClipInfo>,
    quick_index: usize,
    /// Newest clips on disk for the overlay gallery (all time).
    overlay_clips: Vec<crate::engine::ClipInfo>,
    overlay_clips_at: Option<Instant>,
    home_clips: Vec<crate::engine::ClipInfo>,
    home_clips_at: Option<Instant>,
    /// Clip waiting for the delete confirmation, and the error of the last attempt.
    delete_pending: Option<std::path::PathBuf>,
    delete_error: Option<String>,
    /// Every clip of the save folder for the overlay gallery page.
    gallery_clips: Vec<crate::engine::ClipInfo>,
    gallery_at: Option<Instant>,
    /// Gallery filter: 0 all, 1 replays, 2 recordings; and the chosen game.
    gallery_kind: u8,
    gallery_game: Option<String>,
    /// Preview pictures of clips: asked for once, `None` until ready (or when none could be made).
    thumbs: crate::thumbs::Thumbs,
    thumb_tex: std::collections::HashMap<std::path::PathBuf, Option<egui::TextureHandle>>,
    update: Option<Arc<parking_lot::Mutex<Option<crate::update::Release>>>>,
    update_done: Arc<AtomicBool>,
    update_at: Instant,
    update_hidden: bool,
    update_manual: bool,
    update_busy: bool,
}

const REPO: &str = "Spamerovith/Instant-Replay";
const UA: &str = "InstantReplay-update-check";

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cfg: Config,
        engine: EngineHandle,
        hotkeys: Hotkeys,
        shared: Arc<UiShared>,
        quit: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        let start_visible = shared.visible.load(Ordering::SeqCst);
        let reduced = theme::system_reduced_motion();
        w::set_reduced_motion(reduced);
        theme::install(&cc.egui_ctx, reduced);
        App {
            applied: cfg.clone(),
            cfg,
            changed_at: None,
            engine,
            hotkeys,
            shared,
            quit,
            page: Page::Home,
            page_changed: Instant::now() - Duration::from_secs(1),
            monitors: sources::monitors(),
            windows: sources::windows(),
            mics: audio::list_microphones(),
            capturing: None,
            capture_hint: String::new(),
            overlay: None,
            viewer: None,
            viewer_prefs: ViewerPrefs::default(),
            quick: None,
            quick_list: Vec::new(),
            quick_index: 0,
            overlay_clips: Vec::new(),
            overlay_clips_at: None,
            home_clips: Vec::new(),
            home_clips_at: None,
            delete_pending: None,
            delete_error: None,
            gallery_clips: Vec::new(),
            gallery_at: None,
            gallery_kind: 0,
            gallery_game: None,
            thumbs: crate::thumbs::Thumbs::start(cc.egui_ctx.clone()),
            thumb_tex: std::collections::HashMap::new(),
            update: None,
            update_done: Arc::new(AtomicBool::new(false)),
            update_at: Instant::now() - Duration::from_secs(48 * 3600),
            update_hidden: false,
            update_manual: false,
            update_busy: false,
            hide_at: if start_visible { None } else { Some(Instant::now() + Duration::from_millis(400)) },
        }
    }

    fn refresh_sources(&mut self) {
        self.monitors = sources::monitors();
        self.windows = sources::windows();
        self.mics = audio::list_microphones();
    }

    fn hotkey_bindings(c: &Config) -> Vec<(Action, Hotkey)> {
        vec![
            (Action::SaveReplay, c.hotkey_save_replay),
            (Action::SaveLast, c.hotkey_save_last),
            (Action::ToggleRecording, c.hotkey_record),
            (Action::ToggleReplay, c.hotkey_toggle_replay),
            (Action::ToggleOverlay, c.hotkey_overlay),
            (Action::Screenshot, c.hotkey_screenshot),
            (Action::ToggleHud, c.hotkey_hud),
        ]
    }

    fn sync_config(&mut self, ctx: &egui::Context) {
        if self.cfg != self.applied && self.changed_at.is_none() {
            self.changed_at = Some(Instant::now());
        }
        let Some(at) = self.changed_at else { return };
        let dragging = ctx.input(|i| i.pointer.any_down());
        if dragging || at.elapsed() < Duration::from_millis(350) {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        self.changed_at = None;
        self.cfg.sanitize();
        if Self::hotkey_bindings(&self.cfg) != Self::hotkey_bindings(&self.applied) {
            self.hotkeys.update(Self::hotkey_bindings(&self.cfg));
        }
        if self.cfg.autostart != self.applied.autostart {
            crate::autostart::set(self.cfg.autostart);
        }
        self.cfg.save();
        crate::hud::configure(&self.cfg);
        self.engine.send(Command::ApplyConfig(Box::new(self.cfg.clone())));
        self.applied = self.cfg.clone();
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.shared.quitting.load(Ordering::SeqCst) {
            if self.cfg.close_to_tray {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.close_viewer(ctx);
                self.shared.hide();
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                (self.quit)();
            }
        }
        // Проверка обновлений: при запуске и раз в сутки.
        if self.update_at.elapsed() > Duration::from_secs(24 * 3600) {
            self.check_updates(ctx, false);
        }
        // The engine's replay toggle may change the config behind our back (hotkey/tray).
        let status = self.engine.status.read().clone();
        if status.replay_enabled != self.applied.replay_enabled {
            self.applied.replay_enabled = status.replay_enabled;
            self.cfg.replay_enabled = status.replay_enabled;
        }
        let hud_on = crate::hud::enabled();
        if hud_on != self.applied.hud_enabled && self.changed_at.is_none() {
            self.applied.hud_enabled = hud_on;
            self.cfg.hud_enabled = hud_on;
        }

        self.poll_thumbs(ctx);
        self.poll_hotkey_capture(ctx);

        if let Some(t) = self.hide_at {
            if Instant::now() >= t {
                self.hide_at = None;
                if self.overlay.is_none() {
                    self.shared.hide();
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(30));
            }
        }
        if self.shared.overlay_request.swap(false, Ordering::SeqCst) {
            self.hide_at = None;
            if self.overlay.is_some() {
                self.exit_overlay(ctx, false);
            } else {
                self.enter_overlay(ctx);
            }
        }
        if let Some(ov) = &mut self.overlay {
            let focused = ctx.input(|i| i.viewport().focused);
            // Esc of the delete confirmation belongs to it.
            let esc = self.delete_pending.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape));
            if focused == Some(true) {
                ov.focused = true;
            }
            let mut lost = ov.focused && focused == Some(false) && ov.opened.elapsed() > Duration::from_millis(400);
            if lost {
                // A screenshot tool (PrintScreen, Win+Shift+S) takes the focus for a moment: the
                // overlay stays so that it gets into the picture.
                let snip = sources::foreground_exe().map_or(false, |e| sources::is_screenshot_tool(&e));
                if snip {
                    lost = false;
                    ctx.request_repaint_after(Duration::from_millis(150));
                }
            }
            if esc && self.quick.as_mut().map_or(false, |q| q.close_menu()) {
                // Esc closes an open speed / track menu first
            } else if esc && self.quick.is_some() {
                self.quick = None; // then the quick view
                self.shared.overlay_panel_rect();
            } else if esc && ov.gallery {
                ov.gallery = false; // Esc leaves the gallery first, then the overlay
            } else if esc || lost {
                self.exit_overlay(ctx, false);
            }
        }
        self.delete_dialog(ctx);
        if self.overlay.is_some() {
            if let Some(ov) = &mut self.overlay {
                if ov.frames < 4 {
                    // Frames count only once egui knows the overlay size (the first one is drawn at the old size).
                    if (ctx.screen_rect().width() - size::OVERLAY_W).abs() < 2.0 {
                        ov.frames += 1;
                    }
                    if ov.frames == 4 {
                        set_opaque(self.shared.hwnd.load(Ordering::SeqCst), true);
                    } else {
                        ctx.request_repaint();
                    }
                }
            }
            self.overlay_ui(ctx, &status);
            self.sync_config(ctx);
            ctx.request_repaint_after(Duration::from_millis(250));
            return;
        }

        if self.cfg.language.is_none() && self.overlay.is_none() {
            self.language_modal(ctx);
        }
        if self.viewer.is_some() {
            let mut closed = false;
            egui::CentralPanel::default().frame(egui::Frame::none().fill(color::CANVAS)).show(ctx, |ui| {
                if let Some(v) = &mut self.viewer {
                    closed = v.ui(ui, &mut self.viewer_prefs);
                }
            });
            if closed || !self.shared.visible.load(Ordering::SeqCst) {
                self.close_viewer(ctx);
            }
            self.sync_config(ctx);
            return;
        }

        egui::SidePanel::left("nav")
            .exact_width(size::SIDEBAR_W)
            .resizable(false)
            .frame(egui::Frame::none().fill(color::SIDEBAR).inner_margin(egui::Margin { left: space::S16, right: space::S8, top: space::S24, bottom: space::S16 }))
            .show(ctx, |ui| self.sidebar(ui, &status));

        egui::CentralPanel::default().frame(egui::Frame::none().fill(color::WINDOW)).show(ctx, |ui| {
            // Hairline between the menu column and the page, like the overlay's dividers.
            let edge = ui.max_rect();
            ui.painter().vline(edge.left(), edge.y_range(), egui::Stroke::new(1.0_f32, color::CARD_BORDER));
            let t = ((self.page_changed.elapsed().as_secs_f32()) / w::dur(theme::motion::PAGE).max(0.0001)).clamp(0.0, 1.0);
            if t < 1.0 {
                ctx.request_repaint();
            }
            ui.set_opacity(0.0_f32.max(t));
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let avail = ui.available_width();
                let content_w = (avail - size::PAGE_PAD * 2.0).min(size::CONTENT_MAX_W);
                let side = ((avail - content_w) / 2.0).max(size::PAGE_PAD);
                ui.add_space(size::PAGE_PAD - 8.0);
                ui.horizontal(|ui| {
                    ui.add_space(side);
                    ui.vertical(|ui| {
                        ui.set_width(content_w);
                        ui.spacing_mut().item_spacing.y = space::S8;
                        match self.page {
                            Page::Home => self.page_home(ui, &status),
                            Page::Video => self.page_video(ui, &status),
                            Page::Audio => self.page_audio(ui),
                            Page::Games => self.page_games(ui, &status),
                            Page::Hotkeys => self.page_hotkeys(ui),
                            Page::General => self.page_general(ui, &status),
                        }
                        ui.add_space(size::PAGE_PAD);
                    });
                });
            });
        });

        self.sync_config(ctx);
        if self.shared.visible.load(Ordering::SeqCst) {
            let rec = status.recording_since.is_some();
            ctx.request_repaint_after(Duration::from_millis(if rec || status.replay_active { 250 } else { 1000 }));
        }
    }
}

impl App {
    /// Клип открывается в отдельном окне JustView; без JustView — во встроенном плеере.
    fn open_clip(&mut self, ctx: &egui::Context, path: &std::path::Path) {
        if let Some(jv) = util::find_justview() {
            if std::process::Command::new(&jv).arg(path).spawn().is_ok() {
                return;
            }
        }
        self.viewer = Some(Viewer::open(ctx, path, &self.viewer_prefs));
    }

    fn check_updates(&mut self, ctx: &egui::Context, manual: bool) {
        let slot = Arc::new(parking_lot::Mutex::new(None));
        let done = Arc::new(AtomicBool::new(false));
        let (s2, d2, ctx2) = (slot.clone(), done.clone(), ctx.clone());
        let skip = if manual { String::new() } else { self.cfg.skip_version.clone() };
        std::thread::spawn(move || {
            if let Some(r) = crate::update::latest(REPO, UA) {
                if crate::update::newer(&r.version, env!("CARGO_PKG_VERSION")) && r.version != skip {
                    *s2.lock() = Some(r);
                }
            }
            d2.store(true, Ordering::SeqCst);
            ctx2.request_repaint();
        });
        self.update = Some(slot);
        self.update_done = done;
        self.update_at = Instant::now();
        self.update_manual = manual;
        if manual {
            self.update_hidden = false;
        }
    }

    fn pending_update(&self) -> Option<crate::update::Release> {
        if self.update_hidden {
            return None;
        }
        self.update.as_ref().and_then(|s| s.lock().clone())
    }

    fn update_card(&mut self, ui: &mut Ui) {
        let Some(rel) = self.pending_update() else { return };
        let mut act = 0;
        egui::Frame::none().fill(color::CARD).stroke(egui::Stroke::new(1.0_f32, color::CARD_BORDER)).rounding(Rounding::same(radius::GROUP)).inner_margin(egui::Margin::symmetric(space::S16, space::S12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(glyph::DOWNLOAD).font(theme::icon(14.0)).color(color::FOCUS));
                ui.label(RichText::new(trf!(["Доступна версия {}", "Version {} is available", "Доступна версія {}"], rel.version)).font(theme::body_strong()).color(color::TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = space::S8;
                    let busy = self.update_busy;
                    let label = if busy { tr!("Загрузка…", "Downloading…", "Завантаження…") } else { tr!("Обновить", "Update", "Оновити") };
                    if w::pill_button(ui, label, Kind::Primary, size::BUTTON_SM, !busy).clicked() {
                        act = 1;
                    }
                    if !busy {
                        if w::pill_button(ui, tr!("Позже", "Later", "Пізніше"), Kind::Ghost, size::BUTTON_SM, true).clicked() {
                            act = 2;
                        }
                        if w::pill_button(ui, tr!("Пропустить", "Skip", "Пропустити"), Kind::Ghost, size::BUTTON_SM, true).clicked() {
                            act = 3;
                        }
                    }
                });
            });
        });
        ui.add_space(space::S8);
        match act {
            1 => {
                self.update_busy = true;
                let quit = self.quit.clone();
                std::thread::spawn(move || match crate::update::install(&rel, UA, &["--updated".into()]) {
                    Ok(()) => quit(),
                    Err(_) => crate::update::open_page(&rel.page),
                });
            }
            2 => self.update_hidden = true,
            3 => {
                self.cfg.skip_version = rel.version.clone();
                self.update_hidden = true;
            }
            _ => {}
        }
    }

    /// Выбор языка при первом запуске: поверх окна, без возможности пропустить.
    fn language_modal(&mut self, ctx: &egui::Context) {
        let screen = ctx.screen_rect();
        egui::Area::new(egui::Id::new("lang_modal")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
            let (_, scrim) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
            let _ = scrim;
            ui.painter().rect_filled(screen, Rounding::ZERO, Color32::from_black_alpha(170));
            let w = 360.0;
            let card = egui::Rect::from_center_size(screen.center(), vec2(w, 260.0));
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(card), |ui| {
                egui::Frame::none().fill(color::CARD).stroke(egui::Stroke::new(1.0_f32, color::CARD_BORDER)).rounding(Rounding::same(radius::CARD)).inner_margin(egui::Margin::same(space::S24)).show(ui, |ui| {
                    ui.set_width(w - space::S24 * 2.0);
                    ui.label(RichText::new("Язык · Language · Мова").font(theme::title()).color(color::TEXT));
                    ui.add_space(space::S16);
                    let cur = crate::i18n::lang();
                    for l in crate::i18n::ALL {
                        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), egui::Sense::click());
                        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                        let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), w::dur(theme::motion::HOVER));
                        let sel = l == cur;
                        let fill = if sel { color::SURFACE_PRESSED } else { w::lerp_color(color::SURFACE_RAISED, color::SURFACE_HOVER, h) };
                        ui.painter().rect_filled(r, Rounding::same(radius::INPUT), fill);
                        ui.painter().text(egui::pos2(r.left() + space::S16, r.center().y), egui::Align2::LEFT_CENTER, l.native_name(), theme::body(), if sel { color::FOCUS } else { color::TEXT });
                        if sel {
                            ui.painter().text(egui::pos2(r.right() - space::S16, r.center().y), egui::Align2::RIGHT_CENTER, glyph::CHECK, theme::icon(13.0), color::FOCUS);
                        }
                        if resp.clicked() {
                            crate::i18n::set(l);
                            self.cfg.language = Some(l.code().to_string());
                        }
                        ui.add_space(space::S4);
                    }
                });
            });
        });
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.cfg.language = Some(crate::i18n::lang().code().to_string());
        }
    }

    fn close_viewer(&mut self, ctx: &egui::Context) {
        if self.viewer.take().is_some() && ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
    }

    fn set_page(&mut self, p: Page) {
        if self.page != p {
            self.page = p;
            self.page_changed = Instant::now();
            if p == Page::Home || p == Page::Audio || p == Page::Games {
                self.refresh_sources();
            }
        }
    }

    // -------------------------------------------------------------- sidebar

    fn sidebar(&mut self, ui: &mut Ui, status: &Status) {
        ui.horizontal(|ui| {
            ui.add_space(space::S8);
            let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::hover());
            let c = r.center();
            let dot = if status.recording_since.is_some() { color::RECORD } else if status.replay_active { color::TEXT } else { color::FIELD_BORDER };
            let ring = if status.replay_active || status.recording_since.is_some() { color::TEXT } else { color::FIELD_BORDER };
            ui.painter().circle_stroke(c, 9.0, egui::Stroke::new(2.2_f32, ring));
            ui.painter().circle_filled(c, 4.2, dot);
            ui.add_space(space::S4);
            ui.label(RichText::new("Instant Replay").font(theme::headline()).color(color::TEXT));
        });
        ui.add_space(space::S24);

        // Sliding selection indicator behind the items.
        let top = ui.cursor().top();
        let gap = space::S4;
        let idx = pages().iter().position(|(p, _, _)| *p == self.page).unwrap_or(0) as f32;
        let target_y = top + idx * (size::NAV_ITEM + gap);
        let y = ui.ctx().animate_value_with_time(egui::Id::new("nav-indicator"), target_y, w::dur(theme::motion::INDICATOR));
        let x0 = ui.cursor().left();
        let ind = egui::Rect::from_min_size(egui::pos2(x0, y), vec2(ui.available_width(), size::NAV_ITEM));
        ui.painter().rect_filled(ind, Rounding::same(radius::INPUT), color::CARD);

        ui.spacing_mut().item_spacing.y = gap;
        for (p, g, label) in pages() {
            if w::nav_item(ui, g, label, self.page == p).clicked() {
                self.set_page(p);
            }
        }

        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.horizontal(|ui| {
                ui.add_space(space::S12);
                ui.label(RichText::new(trf!(["Версия {}", "Version {}", "Версія {}"], env!("CARGO_PKG_VERSION"))).font(theme::caption()).color(color::TEXT_SECONDARY));
            });
        });
    }

    // -------------------------------------------------------------- home

    fn page_home(&mut self, ui: &mut Ui, st: &Status) {
        self.update_card(ui);
        self.banners(ui, st);
        let width = ui.available_width();

        // Hero: Instant Replay with the tape bar.
        egui::Frame::none().fill(color::CARD).stroke(egui::Stroke::new(1.0_f32, color::CARD_BORDER)).rounding(Rounding::same(radius::CARD)).inner_margin(egui::Margin::same(space::S24)).show(ui, |ui| {
            ui.set_width(width - space::S24 * 2.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(glyph::HISTORY).font(theme::icon(16.0)).color(color::TEXT));
                ui.label(RichText::new(tr!("Буфер повтора", "Replay buffer", "Буфер повтору")).font(theme::headline()).color(color::TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let mut on = self.cfg.replay_enabled;
                    if w::switch(ui, &mut on, "Instant Replay").changed() {
                        self.cfg.replay_enabled = on;
                    }
                });
            });
            ui.add_space(space::S12);
            let total = self.cfg.replay_seconds as f64;
            let buffered = if st.replay_active { st.buffered_secs.min(total) } else { 0.0 };
            ui.label(
                RichText::new(util::format_duration(buffered))
                    .font(theme::display())
                    .color(if st.replay_active { color::TEXT } else { color::TEXT_DISABLED }),
            );
            let sub = if st.replay_active {
                trf!(["из {} в буфере · {}", "of {} in buffer · {}", "з {} у буфері · {}"], util::format_duration(total), util::format_bytes(st.buffer_bytes))
            } else if self.cfg.replay_enabled {
                tr!("Запуск буфера…", "Starting buffer…", "Запуск буфера…").to_string()
            } else {
                trf!(["Включите, чтобы всегда хранить последние {}", "Turn on to always keep the last {}", "Увімкніть, щоб завжди зберігати останні {}"], util::format_duration(total))
            };
            ui.label(RichText::new(sub).font(theme::subhead()).color(color::TEXT_SECONDARY));
            ui.add_space(space::S16);
            let frac = if total > 0.0 { (buffered / total) as f32 } else { 0.0 };
            w::tape(ui, "replay", frac, ui.available_width(), color::TEXT);
            ui.add_space(space::S20);
            ui.horizontal(|ui| {
                let enabled = st.replay_active && st.buffered_secs > 0.5;
                let label = if st.saving > 0 { tr!("Сохранение…", "Saving…", "Збереження…") } else { tr!("Сохранить повтор", "Save replay", "Зберегти повтор") };
                if w::pill_button(ui, label, Kind::Primary, size::BUTTON_LG, enabled && st.saving == 0).clicked() {
                    self.engine.send(Command::SaveReplay);
                }
                ui.add_space(space::S4);
                w::value_text(ui, &hotkeys::describe(&self.cfg.hotkey_save_replay));
            });
        });

        ui.add_space(space::S16);

        // Recording card.
        egui::Frame::none().fill(color::CARD).stroke(egui::Stroke::new(1.0_f32, color::CARD_BORDER)).rounding(Rounding::same(radius::CARD)).inner_margin(egui::Margin::same(space::S24)).show(ui, |ui| {
            ui.set_width(width - space::S24 * 2.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = space::S4;
                    if let Some(since) = st.recording_since {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(10.0, 18.0), egui::Sense::hover());
                            ui.painter().circle_filled(r.center(), 5.0, color::RECORD);
                            ui.label(RichText::new(trf!(["Идёт запись  {}", "Recording  {}", "Триває запис  {}"], util::format_duration(since.elapsed().as_secs_f64()))).font(theme::headline()).color(color::TEXT));
                        });
                        ui.label(RichText::new(trf!(["{} записано", "{} recorded", "{} записано"], util::format_bytes(st.recording_bytes))).font(theme::subhead()).color(color::TEXT_SECONDARY));
                    } else {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(glyph::VIDEO).font(theme::icon(16.0)).color(color::TEXT));
                            ui.label(RichText::new(tr!("Запись", "Recording", "Запис")).font(theme::headline()).color(color::TEXT));
                        });
                        ui.label(RichText::new(tr!("Сохраняет всё до остановки", "Saves everything until you stop", "Зберігає все до зупинки")).font(theme::subhead()).color(color::TEXT_SECONDARY));
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let recording = st.recording_since.is_some();
                    let (label, kind) = if recording { (tr!("Остановить", "Stop", "Зупинити"), Kind::Record) } else { (tr!("Начать запись", "Start recording", "Почати запис"), Kind::Ghost) };
                    if w::pill_button(ui, label, kind, size::BUTTON_MD, true).clicked() {
                        self.engine.send(Command::ToggleRecording);
                    }
                    ui.add_space(space::S4);
                    w::value_text(ui, &hotkeys::describe(&self.cfg.hotkey_record));
                });
            });
        });

        ui.add_space(space::S24);

        // Capture source.
        let info = if st.running && !st.encoder_name.is_empty() {
            Some(trf!(["{} · {}×{} · {} к/с", "{} · {}×{} · {} fps", "{} · {}×{} · {} к/с"], st.encoder_name, st.out_size.0, st.out_size.1, self.cfg.fps))
        } else {
            None
        };
        if let Some(name) = &st.active_profile {
            caption(ui, &trf!(["Профиль игры «{}»: записывается окно игры", "Game profile \"{}\": the game window is recorded", "Профіль гри «{}»: записується вікно гри"], name));
            ui.add_space(space::S8);
        }
        w::group(ui, Some(tr!("Источник", "Source", "Джерело")), |g| {
            g.row(tr!("Что записывать", "What to record", "Що записувати"), info.as_deref(), |ui| {
                if w::icon_button(ui, glyph::REFRESH, tr!("Обновить список", "Refresh list", "Оновити список")).clicked() {
                    self.monitors = sources::monitors();
                    self.windows = sources::windows();
                }
                self.source_combo(ui);
            });
            if st.source_closed {
                g.row_rich(tr!("Окно закрыто", "Window closed", "Вікно закрито"), Some(tr!("В видео остаётся последний кадр. Выберите другой источник", "The video keeps the last frame. Choose another source", "У відео лишається останній кадр. Виберіть інше джерело")), Some(color::WARNING), |_| {});
            }
        });

        ui.add_space(space::S24);

        // Recent clips.
        ui.horizontal(|ui| {
            ui.add_space(space::S4);
            ui.label(RichText::new(tr!("Последние клипы", "Recent clips", "Останні кліпи")).font(theme::headline()).color(color::TEXT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if w::link(ui, tr!("Открыть папку", "Open folder", "Відкрити теку")).clicked() {
                    let _ = std::fs::create_dir_all(&self.cfg.output_dir);
                    util::open_in_explorer(&self.cfg.output_dir, false);
                }
            });
        });
        ui.add_space(space::S8 - ui.spacing().item_spacing.y);
        // The newest clips of all time (not just this session), refreshed every few seconds.
        if self.home_clips_at.map_or(true, |t| t.elapsed() > Duration::from_secs(3)) {
            self.home_clips = crate::engine::scan_clips(&self.cfg.output_dir, 8);
            self.home_clips_at = Some(Instant::now());
        }
        let home_clips = self.home_clips.clone();
        let recent_tex: Vec<Option<egui::TextureId>> = home_clips.iter().map(|c| self.thumb_for(c)).collect();
        let mut open_clip: Option<std::path::PathBuf> = None;
        let mut delete_req: Option<std::path::PathBuf> = None;
        w::group(ui, None, |g| {
            if home_clips.is_empty() {
                g.row(tr!("Пока пусто", "Nothing yet", "Поки порожньо"), None, |ui| {
                    w::value_text(ui, &trf!(["{} или {}", "{} or {}", "{} або {}"], hotkeys::describe(&self.cfg.hotkey_save_replay), hotkeys::describe(&self.cfg.hotkey_record)));
                });
            }
            for (clip, tex) in home_clips.iter().zip(recent_tex.iter()) {
                let name = clip.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let meta = format!("{} · {}", util::format_duration(clip.secs), util::format_bytes(clip.bytes));
                let path = clip.path.clone();
                g.custom(size::ROW_MIN + 14.0, |ui| {
                    let row = ui.max_rect();
                    let resp = ui.interact(row, egui::Id::new(("clip", &path)), egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), w::dur(theme::motion::HOVER));
                    if h > 0.0 {
                        ui.painter().rect_filled(row.expand2(vec2(space::S8, -4.0)), Rounding::same(radius::INPUT), w::lerp_color(Color32::TRANSPARENT, color::SURFACE_HOVER, h));
                    }
                    clip_thumb(ui, *tex, clip.is_replay, vec2(80.0, 45.0));
                    ui.add(egui::Label::new(RichText::new(&name).font(theme::body()).color(color::TEXT)).truncate());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        w::value_text(ui, &meta);
                    });
                    if resp.clicked() {
                        open_clip = Some(path.clone());
                    }
                    resp.context_menu(|ui| {
                        if clip_menu(ui, &path) {
                            delete_req = Some(path.clone());
                        }
                    });
                });
            }
        });
        if delete_req.is_some() {
            self.delete_pending = delete_req;
            self.delete_error = None;
        }
        if let Some(p) = open_clip {
            self.open_clip(ui.ctx(), &p);
        }
    }

    fn source_label(&self) -> String {
        match &self.cfg.source {
            CaptureSource::Monitor { device } => {
                let idx = self.monitors.iter().position(|m| &m.device == device || (device.is_empty() && m.primary));
                match idx {
                    Some(i) => monitor_label(i, &self.monitors[i]),
                    None => tr!("Основной монитор", "Primary monitor", "Основний монітор").into(),
                }
            }
            CaptureSource::Window { title, exe } => {
                let t: String = title.chars().take(36).collect();
                if exe.is_empty() {
                    t
                } else {
                    format!("{t} — {exe}")
                }
            }
        }
    }

    fn source_combo(&mut self, ui: &mut Ui) {
        let label = self.source_label();
        egui::ComboBox::from_id_salt("source").width(280.0).height(420.0).selected_text(RichText::new(label).font(theme::subhead())).show_ui(ui, |ui| {
            ui.set_min_width(320.0);
            ui.label(RichText::new(tr!("Мониторы", "Monitors", "Монітори")).font(theme::caption()).color(color::TEXT_SECONDARY));
            for (i, m) in self.monitors.iter().enumerate() {
                let selected = matches!(&self.cfg.source, CaptureSource::Monitor { device } if device == &m.device || (device.is_empty() && m.primary));
                if ui.selectable_label(selected, monitor_label(i, m)).clicked() {
                    self.cfg.source = CaptureSource::Monitor { device: m.device.clone() };
                }
            }
            ui.add_space(space::S8);
            ui.label(RichText::new(tr!("Окна и игры", "Windows and games", "Вікна та ігри")).font(theme::caption()).color(color::TEXT_SECONDARY));
            for wi in &self.windows {
                let selected = matches!(&self.cfg.source, CaptureSource::Window { title, exe } if title == &wi.title && exe == &wi.exe);
                let t: String = wi.title.chars().take(48).collect();
                if ui.selectable_label(selected, format!("{t} — {}", wi.exe)).clicked() {
                    self.cfg.source = CaptureSource::Window { title: wi.title.clone(), exe: wi.exe.clone() };
                }
            }
        });
    }

    fn banners(&mut self, ui: &mut Ui, st: &Status) {
        let failed = self.hotkeys.failed();
        let mut msgs: Vec<String> = Vec::new();
        if let Some(e) = &st.error {
            msgs.push(e.clone());
        }
        if !failed.is_empty() {
            msgs.push(tr!("Часть горячих клавиш занята другой программой - назначьте другие во вкладке «Горячие клавиши»", "Some hotkeys are taken by another program - assign others on the “Hotkeys” tab", "Частину гарячих клавіш зайняла інша програма - призначте інші на вкладці «Гарячі клавіші»").into());
        }
        if st.pending_restart {
            msgs.push(tr!("Новые видеонастройки применятся после остановки записи", "New video settings apply after recording stops", "Нові відеоналаштування застосуються після зупинки запису").into());
        }
        for m in msgs {
            egui::Frame::none().fill(color::CARD).stroke(egui::Stroke::new(1.0_f32, color::CARD_BORDER)).rounding(Rounding::same(radius::GROUP)).inner_margin(egui::Margin::symmetric(space::S16, space::S12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(glyph::WARNING).font(theme::icon(14.0)).color(color::WARNING));
                    ui.label(RichText::new(m).font(theme::subhead()).color(color::TEXT));
                });
            });
            ui.add_space(space::S8);
        }
    }

    // -------------------------------------------------------------- video

    fn page_video(&mut self, ui: &mut Ui, st: &Status) {
        page_title(ui, tr!("Видео", "Video", "Відео"));

        let idx = REPLAY_STEPS.iter().position(|&s| s >= self.cfg.replay_seconds).unwrap_or(REPLAY_STEPS.len() - 1);
        let mut fidx = idx as f32;
        let mem = estimate_memory(&self.cfg, st.out_size);
        w::group(ui, Some("Instant Replay"), |g| {
            g.row(tr!("Длительность буфера", "Buffer length", "Тривалість буфера"), Some(&mem), |ui| {
                w::value_text(ui, &util::format_duration(self.cfg.replay_seconds as f64));
                if w::slider(ui, "replay-len", &mut fidx, 0.0, (REPLAY_STEPS.len() - 1) as f32, 1.0, 220.0).changed() {
                    self.cfg.replay_seconds = REPLAY_STEPS[fidx.round() as usize];
                }
            });
        });

        ui.add_space(space::S24);
        let available: Vec<Codec> = st.encoders.iter().map(|e| e.codec).collect();
        let encoder_line = st.encoders.iter().find(|e| e.codec == self.cfg.codec).map(|e| {
            if e.hardware {
                trf!(["{} · аппаратное кодирование", "{} · hardware encoding", "{} · апаратне кодування"], e.name)
            } else {
                trf!(["{} · программное кодирование (нагружает процессор)", "{} · software encoding (loads the CPU)", "{} · програмне кодування (навантажує процесор)"], e.name)
            }
        });
        w::group(ui, Some(tr!("Качество", "Quality", "Якість")), |g| {
            g.row(tr!("Кодек", "Codec", "Кодек"), encoder_line.as_deref(), |ui| {
                let opts: Vec<(&str, bool)> = Codec::ALL.iter().map(|c| (c.label(), available.is_empty() || available.contains(c))).collect();
                let mut sel = Codec::ALL.iter().position(|c| *c == self.cfg.codec).unwrap_or(0);
                if w::segmented(ui, "codec", &opts, &mut sel) {
                    self.cfg.codec = Codec::ALL[sel];
                }
            });
            g.row(tr!("Управление битрейтом", "Rate control", "Керування бітрейтом"), Some(rate_hint(self.cfg.rate_control)), |ui| {
                let modes = [RateControl::Cbr, RateControl::Vbr, RateControl::Cqp];
                let opts = [("CBR", true), ("VBR", true), ("CQP", true)];
                let mut sel = modes.iter().position(|m| *m == self.cfg.rate_control).unwrap_or(1);
                if w::segmented(ui, "rc", &opts, &mut sel) {
                    self.cfg.rate_control = modes[sel];
                }
            });
            if self.cfg.rate_control == RateControl::Cqp {
                g.row(tr!("Качество", "Quality", "Якість"), Some(tr!("Выше - чётче и больше файл", "Higher is sharper with bigger files", "Вище - чіткіше й більший файл")), |ui| {
                    w::value_text(ui, &format!("{}", self.cfg.quality));
                    let mut v = self.cfg.quality as f32;
                    if w::slider(ui, "quality", &mut v, 1.0, 100.0, 1.0, 220.0).changed() {
                        self.cfg.quality = v as u32;
                    }
                });
            } else {
                let (aw, ah) = picture_size(&self.cfg, st.out_size);
                let auto = crate::config::auto_bitrate_mbps(self.cfg.codec, aw, ah, self.cfg.fps, self.cfg.bitrate_quality);
                g.row(
                    tr!("Автобитрейт", "Auto bitrate", "Автобітрейт"),
                    Some(tr!(
                        "Постоянное качество: спокойные сцены и рабочий стол весят в разы меньше, динамичные - сколько нужно",
                        "Constant quality: calm scenes and the desktop take a fraction of the size, busy ones what they need",
                        "Постійна якість: спокійні сцени й робочий стіл важать у рази менше, динамічні - скільки потрібно"
                    )),
                    |ui| {
                        if self.cfg.auto_bitrate {
                            w::value_text(ui, &trf!(["обычно до {} Мбит/с", "usually up to {} Mbps", "зазвичай до {} Мбіт/с"], auto));
                        }
                        w::switch(ui, &mut self.cfg.auto_bitrate, tr!("Автобитрейт", "Auto bitrate", "Автобітрейт"));
                    },
                );
                if self.cfg.auto_bitrate {
                    g.row(
                        tr!("Качество", "Quality", "Якість"),
                        Some(tr!("Компактно - файлы легче, Высокое - чётче в динамичных сценах", "Compact - lighter files, High - sharper in busy scenes", "Компактно - файли легші, Високе - чіткіше в динамічних сценах")),
                        |ui| {
                            let opts: Vec<(&str, bool)> = crate::config::BitrateQuality::ALL.iter().map(|q| (q.label(), true)).collect();
                            let mut sel = crate::config::BitrateQuality::ALL.iter().position(|q| *q == self.cfg.bitrate_quality).unwrap_or(1);
                            if w::segmented(ui, "bitrate-quality", &opts, &mut sel) {
                                self.cfg.bitrate_quality = crate::config::BitrateQuality::ALL[sel];
                            }
                        },
                    );
                }
                if !self.cfg.auto_bitrate {
                    let hint = bitrate_hint(&self.cfg, st.out_size);
                    g.row(tr!("Битрейт", "Bitrate", "Бітрейт"), Some(&hint), |ui| {
                        w::value_text(ui, &trf!(["{} Мбит/с", "{} Mbps", "{} Мбіт/с"], self.cfg.bitrate_mbps));
                        let mut v = self.cfg.bitrate_mbps as f32;
                        if w::slider(ui, "bitrate", &mut v, 3.0, 150.0, 1.0, 220.0).changed() {
                            self.cfg.bitrate_mbps = v as u32;
                        }
                    });
                }
            }
            g.row(tr!("Пресет кодера", "Encoder preset", "Пресет кодера"), None, |ui| {
                let presets = [Preset::Speed, Preset::Balanced, Preset::Quality];
                let opts = [(tr!("Быстрее", "Faster", "Швидше"), true), (tr!("Баланс", "Balanced", "Баланс"), true), (tr!("Качество", "Quality", "Якість"), true)];
                let mut sel = presets.iter().position(|p| *p == self.cfg.preset).unwrap_or(1);
                if w::segmented(ui, "preset", &opts, &mut sel) {
                    self.cfg.preset = presets[sel];
                }
            });
        });

        ui.add_space(space::S24);
        w::group(ui, Some(tr!("Изображение", "Image", "Зображення")), |g| {
            g.row(tr!("Разрешение", "Resolution", "Роздільність"), None, |ui| {
                egui::ComboBox::from_id_salt("res").width(180.0).selected_text(RichText::new(self.cfg.resolution.label()).font(theme::subhead())).show_ui(ui, |ui| {
                    for r in Resolution::ALL {
                        if ui.selectable_label(self.cfg.resolution == r, r.label()).clicked() {
                            self.cfg.resolution = r;
                        }
                    }
                });
            });
            g.row(tr!("Частота кадров", "Frame rate", "Частота кадрів"), None, |ui| {
                let labels: Vec<String> = FPS_STEPS.iter().map(|f| f.to_string()).collect();
                let opts: Vec<(&str, bool)> = labels.iter().map(|s| (s.as_str(), true)).collect();
                let mut sel = FPS_STEPS.iter().position(|&f| f == self.cfg.fps).unwrap_or(1);
                if w::segmented(ui, "fps", &opts, &mut sel) {
                    self.cfg.fps = FPS_STEPS[sel];
                }
            });
            g.row(tr!("Показывать курсор", "Show cursor", "Показувати курсор"), None, |ui| {
                w::switch(ui, &mut self.cfg.capture_cursor, tr!("Показывать курсор", "Show cursor", "Показувати курсор"));
            });
        });
        ui.add_space(space::S8);
        caption(ui, tr!("Изменение видеонастроек перезапускает буфер повтора", "Changing video settings restarts the replay buffer", "Зміна відеоналаштувань перезапускає буфер повтору"));
    }

    // -------------------------------------------------------------- audio

    fn page_audio(&mut self, ui: &mut Ui) {
        page_title(ui, tr!("Звук", "Audio", "Звук"));
        w::group(ui, Some(tr!("Системный звук", "System audio", "Системний звук")), |g| {
            g.row(tr!("Записывать звук системы и игр", "Record system and game audio", "Записувати звук системи та ігор"), None, |ui| {
                w::switch(ui, &mut self.cfg.system_audio, tr!("Системный звук", "System audio", "Системний звук"));
            });
            if self.cfg.system_audio {
                g.row(tr!("Громкость", "Volume", "Гучність"), None, |ui| {
                    w::value_text(ui, &format!("{:.0}%", self.cfg.system_volume * 100.0));
                    let mut v = self.cfg.system_volume * 100.0;
                    if w::slider(ui, "sysvol", &mut v, 0.0, 200.0, 5.0, 220.0).changed() {
                        self.cfg.system_volume = v / 100.0;
                    }
                });
            }
        });
        ui.add_space(space::S24);
        w::group(ui, Some(tr!("Микрофон", "Microphone", "Мікрофон")), |g| {
            g.row(tr!("Записывать микрофон", "Record microphone", "Записувати мікрофон"), None, |ui| {
                w::switch(ui, &mut self.cfg.mic_enabled, tr!("Микрофон", "Microphone", "Мікрофон"));
            });
            if self.cfg.mic_enabled {
                g.row(tr!("Устройство", "Device", "Пристрій"), None, |ui| {
                    let current = self
                        .cfg
                        .mic_device
                        .as_ref()
                        .and_then(|id| self.mics.iter().find(|m| &m.id == id).map(|m| m.name.clone()))
                        .unwrap_or_else(|| tr!("По умолчанию", "Default", "За замовчуванням").into());
                    egui::ComboBox::from_id_salt("mic").width(260.0).selected_text(RichText::new(current).font(theme::subhead())).show_ui(ui, |ui| {
                        ui.set_min_width(300.0);
                        if ui.selectable_label(self.cfg.mic_device.is_none(), tr!("По умолчанию", "Default", "За замовчуванням")).clicked() {
                            self.cfg.mic_device = None;
                        }
                        for m in &self.mics {
                            if ui.selectable_label(self.cfg.mic_device.as_deref() == Some(m.id.as_str()), &m.name).clicked() {
                                self.cfg.mic_device = Some(m.id.clone());
                            }
                        }
                    });
                });
                g.row(tr!("Громкость", "Volume", "Гучність"), None, |ui| {
                    w::value_text(ui, &format!("{:.0}%", self.cfg.mic_volume * 100.0));
                    let mut v = self.cfg.mic_volume * 100.0;
                    if w::slider(ui, "micvol", &mut v, 0.0, 200.0, 5.0, 220.0).changed() {
                        self.cfg.mic_volume = v / 100.0;
                    }
                });
            }
        });
        ui.add_space(space::S24);
        w::group(ui, Some(tr!("Дорожки", "Tracks", "Доріжки")), |g| {
            let both = self.cfg.system_audio && self.cfg.mic_enabled;
            g.row(
                tr!("Раздельные дорожки", "Separate tracks", "Окремі доріжки"),
                Some(if both { tr!("Дорожка 1 - система, 2 - микрофон, 3 - общий звук (игра + микрофон)", "Track 1 - system, 2 - mic, 3 - combined (game + mic)", "Доріжка 1 - система, 2 - мікрофон, 3 - спільний звук (гра + мікрофон)") } else { tr!("Работает, когда включены система и микрофон", "Works when both system audio and the mic are on", "Працює, коли увімкнені система й мікрофон") }),
                |ui| {
                    w::switch(ui, &mut self.cfg.separate_tracks, tr!("Раздельные дорожки", "Separate tracks", "Окремі доріжки"));
                },
            );
            g.row(tr!("Битрейт AAC", "AAC bitrate", "Бітрейт AAC"), None, |ui| {
                let labels = [tr!("128 кбит/с", "128 kbps", "128 кбіт/с"), tr!("160 кбит/с", "160 kbps", "160 кбіт/с"), tr!("192 кбит/с", "192 kbps", "192 кбіт/с")];
                let opts: Vec<(&str, bool)> = labels.iter().map(|s| (*s, true)).collect();
                let mut sel = AUDIO_BITRATES.iter().position(|&b| b == self.cfg.audio_bitrate_kbps).unwrap_or(2);
                if w::segmented(ui, "abr", &opts, &mut sel) {
                    self.cfg.audio_bitrate_kbps = AUDIO_BITRATES[sel];
                }
            });
        });
    }

    // -------------------------------------------------------------- hotkeys

    fn poll_hotkey_capture(&mut self, ctx: &egui::Context) {
        let Some(action) = self.capturing else { return };
        ctx.request_repaint_after(Duration::from_millis(16));
        match hotkeys::poll_capture() {
            CaptureResult::Pending { ctrl, alt, shift, win } => {
                let mut parts = Vec::new();
                if ctrl {
                    parts.push("Ctrl");
                }
                if alt {
                    parts.push("Alt");
                }
                if shift {
                    parts.push("Shift");
                }
                if win {
                    parts.push("Win");
                }
                self.capture_hint = if parts.is_empty() { tr!("Нажмите сочетание…", "Press a shortcut…", "Натисніть комбінацію…").into() } else { format!("{} + …", parts.join(" + ")) };
            }
            CaptureResult::Done(hk) => {
                *self.hotkey_mut(action) = hk;
                // A combination may only be used once.
                for other in Action::ALL {
                    if other != action && *self.hotkey_mut(other) == hk {
                        *self.hotkey_mut(other) = Hotkey::default();
                    }
                }
                self.finish_capture();
            }
            CaptureResult::Clear => {
                *self.hotkey_mut(action) = Hotkey::default();
                self.finish_capture();
            }
            CaptureResult::Cancel => self.finish_capture(),
        }
    }

    fn finish_capture(&mut self) {
        self.capturing = None;
        self.capture_hint.clear();
        // New bindings are registered by sync_config; if nothing changed, restore the old ones.
        if Self::hotkey_bindings(&self.cfg) == Self::hotkey_bindings(&self.applied) {
            self.hotkeys.resume();
        } else {
            self.changed_at = Some(Instant::now() - Duration::from_secs(1));
        }
    }

    fn hotkey_mut(&mut self, a: Action) -> &mut Hotkey {
        match a {
            Action::SaveReplay => &mut self.cfg.hotkey_save_replay,
            Action::SaveLast => &mut self.cfg.hotkey_save_last,
            Action::ToggleRecording => &mut self.cfg.hotkey_record,
            Action::ToggleReplay => &mut self.cfg.hotkey_toggle_replay,
            Action::ToggleOverlay => &mut self.cfg.hotkey_overlay,
            Action::Screenshot => &mut self.cfg.hotkey_screenshot,
            Action::ToggleHud => &mut self.cfg.hotkey_hud,
        }
    }

    fn page_hotkeys(&mut self, ui: &mut Ui) {
        page_title(ui, tr!("Горячие клавиши", "Hotkeys", "Гарячі клавіші"));
        let failed = self.hotkeys.failed();
        let last_label = trf!(["Сохранить последние {} с", "Save the last {} s", "Зберегти останні {} с"], self.cfg.save_last_seconds);
        let rows = [
            (Action::SaveReplay, tr!("Сохранить повтор", "Save replay", "Зберегти повтор")),
            (Action::SaveLast, last_label.as_str()),
            (Action::ToggleRecording, tr!("Начать / остановить запись", "Start / stop recording", "Почати / зупинити запис")),
            (Action::ToggleReplay, tr!("Включить / выключить Instant Replay", "Turn Instant Replay on / off", "Увімкнути / вимкнути Instant Replay")),
            (Action::ToggleOverlay, tr!("Открыть / закрыть оверлей", "Open / close overlay", "Відкрити / закрити оверлей")),
            (Action::Screenshot, tr!("Скриншот", "Screenshot", "Скриншот")),
            (Action::ToggleHud, tr!("Показать / скрыть счётчик FPS", "Show / hide the FPS counter", "Показати / сховати лічильник FPS")),
        ];
        let mut start: Option<Action> = None;
        w::group(ui, None, |g| {
            for (a, label) in rows {
                let hk = match a {
                    Action::SaveReplay => self.cfg.hotkey_save_replay,
                    Action::SaveLast => self.cfg.hotkey_save_last,
                    Action::ToggleRecording => self.cfg.hotkey_record,
                    Action::ToggleReplay => self.cfg.hotkey_toggle_replay,
                    Action::ToggleOverlay => self.cfg.hotkey_overlay,
                    Action::Screenshot => self.cfg.hotkey_screenshot,
                    Action::ToggleHud => self.cfg.hotkey_hud,
                };
                let listening = self.capturing == Some(a);
                let conflict = failed.contains(&a) && !listening && hk == self.applied_hotkey(a);
                let bare = hk.is_set() && !hk.ctrl && !hk.alt && !hk.shift && !hk.win && (0x30..=0x5A).contains(&hk.vk);
                let sub = if listening {
                    Some(tr!("Esc - отмена, Backspace - очистить", "Esc - cancel, Backspace - clear", "Esc - скасувати, Backspace - очистити"))
                } else if conflict {
                    Some(tr!("Занято другой программой - выберите другое сочетание", "Taken by another program - choose another shortcut", "Зайнято іншою програмою - виберіть іншу комбінацію"))
                } else if bare {
                    Some(tr!("Без Ctrl/Alt/Shift эта клавиша перестанет печататься в других программах", "Without Ctrl/Alt/Shift this key stops typing in other programs", "Без Ctrl/Alt/Shift ця клавіша перестане друкуватися в інших програмах"))
                } else {
                    None
                };
                let sub_color = if conflict { Some(color::RECORD) } else { None };
                g.row_rich(label, sub, sub_color, |ui| {
                    let text = if listening { self.capture_hint.clone() } else { hotkeys::describe(&hk) };
                    let state = if listening { KeycapState::Listening } else if conflict { KeycapState::Conflict } else { KeycapState::Idle };
                    if w::keycap(ui, &text, state).clicked() && !listening {
                        start = Some(a);
                    }
                });
            }
        });
        ui.add_space(space::S24);
        let max_last = self.cfg.replay_seconds.max(5);
        let mut steps: Vec<u32> = LAST_STEPS.iter().copied().filter(|&s| s <= max_last).collect();
        if steps.is_empty() {
            steps.push(5);
        }
        let idx = steps.iter().position(|&s| s >= self.cfg.save_last_seconds).unwrap_or(steps.len() - 1);
        let mut fidx = idx as f32;
        w::group(ui, Some(tr!("Быстрое сохранение", "Quick save", "Швидке збереження")), |g| {
            g.row(
                tr!("Длина быстрого клипа", "Quick clip length", "Довжина швидкого кліпу"),
                Some(tr!("Берётся конец буфера, не больше его длины", "Takes the end of the buffer, no more than its length", "Береться кінець буфера, не більше його довжини")),
                |ui| {
                    w::value_text(ui, &util::format_duration(self.cfg.save_last_seconds as f64));
                    if steps.len() > 1 && w::slider(ui, "save-last-len", &mut fidx, 0.0, (steps.len() - 1) as f32, 1.0, 220.0).changed() {
                        self.cfg.save_last_seconds = steps[fidx.round() as usize];
                    }
                },
            );
        });
        if let Some(a) = start {
            self.capturing = Some(a);
            self.capture_hint = tr!("Нажмите сочетание…", "Press a shortcut…", "Натисніть комбінацію…").into();
            self.hotkeys.suspend();
        }
    }

    fn applied_hotkey(&self, a: Action) -> Hotkey {
        match a {
            Action::SaveReplay => self.applied.hotkey_save_replay,
            Action::SaveLast => self.applied.hotkey_save_last,
            Action::ToggleRecording => self.applied.hotkey_record,
            Action::ToggleReplay => self.applied.hotkey_toggle_replay,
            Action::ToggleOverlay => self.applied.hotkey_overlay,
            Action::Screenshot => self.applied.hotkey_screenshot,
            Action::ToggleHud => self.applied.hotkey_hud,
        }
    }

    // -------------------------------------------------------------- general

    fn page_general(&mut self, ui: &mut Ui, st: &Status) {
        page_title(ui, tr!("Общие", "General", "Загальні"));
        w::group(ui, Some(tr!("Язык", "Language", "Мова")), |g| {
            g.row(tr!("Язык интерфейса", "Interface language", "Мова інтерфейсу"), None, |ui| {
                let opts: Vec<(&str, bool)> = crate::i18n::ALL.iter().map(|l| (l.native_name(), true)).collect();
                let mut sel = crate::i18n::ALL.iter().position(|l| *l == crate::i18n::lang()).unwrap_or(0);
                if w::segmented(ui, "lang", &opts, &mut sel) {
                    let l = crate::i18n::ALL[sel];
                    crate::i18n::set(l);
                    self.cfg.language = Some(l.code().to_string());
                }
            });
        });
        ui.add_space(space::S24);
        let dir = self.cfg.output_dir.display().to_string();
        w::group(ui, Some(tr!("Сохранение", "Saving", "Збереження")), |g| {
            g.row(tr!("Папка для клипов", "Clips folder", "Тека для кліпів"), Some(&dir), |ui| {
                if w::pill_button(ui, tr!("Изменить", "Change", "Змінити"), Kind::Ghost, size::BUTTON_SM, true).clicked() {
                    if let Some(p) = rfd::FileDialog::new().set_directory(&self.cfg.output_dir).pick_folder() {
                        self.cfg.output_dir = p;
                    }
                }
                if w::icon_button(ui, glyph::FOLDER, tr!("Открыть папку", "Open folder", "Відкрити теку")).clicked() {
                    let _ = std::fs::create_dir_all(&self.cfg.output_dir);
                    util::open_in_explorer(&self.cfg.output_dir, false);
                }
            });
            g.row(
                tr!("Папки по играм", "Folders by game", "Теки за іграми"),
                Some(tr!("Клип кладётся в папку с названием игры, как в NVIDIA", "A clip goes into a folder named after the game, like NVIDIA does", "Кліп кладеться в теку з назвою гри, як у NVIDIA")),
                |ui| {
                    w::switch(ui, &mut self.cfg.game_folders, tr!("Папки по играм", "Folders by game", "Теки за іграми"));
                },
            );
        });
        ui.add_space(space::S24);
        w::group(ui, Some(tr!("Запуск", "Startup", "Запуск")), |g| {
            g.row(tr!("Запускать вместе с Windows", "Start with Windows", "Запускати разом з Windows"), None, |ui| {
                w::switch(ui, &mut self.cfg.autostart, tr!("Запускать вместе с Windows", "Start with Windows", "Запускати разом з Windows"));
            });
            g.row(
                tr!("Запускать от имени администратора", "Run as administrator", "Запускати від імені адміністратора"),
                Some(tr!(
                    "Нужно для точного FPS в счётчике. Применится при следующем запуске",
                    "Needed for the exact FPS in the counter. Applies at the next start",
                    "Потрібно для точного FPS у лічильнику. Застосується під час наступного запуску"
                )),
                |ui| {
                    w::switch(ui, &mut self.cfg.run_as_admin, tr!("Запускать от имени администратора", "Run as administrator", "Запускати від імені адміністратора"));
                },
            );
            g.row(tr!("Открывать свёрнутым в трей", "Start minimized to tray", "Відкривати згорнутим у трей"), None, |ui| {
                w::switch(ui, &mut self.cfg.start_minimized, tr!("Открывать свёрнутым в трей", "Start minimized to tray", "Відкривати згорнутим у трей"));
            });
            g.row(tr!("Кнопка «Закрыть» сворачивает в трей", "The Close button minimizes to tray", "Кнопка «Закрити» згортає в трей"), Some(tr!("Выйти можно из меню значка в трее", "Quit from the tray icon menu", "Вийти можна з меню значка в треї")), |ui| {
                w::switch(ui, &mut self.cfg.close_to_tray, tr!("Закрытие сворачивает в трей", "Closing minimizes to tray", "Закриття згортає в трей"));
            });
        });
        ui.add_space(space::S24);
        w::group(ui, Some(tr!("Оверлей", "Overlay", "Оверлей")), |g| {
            g.row(
                tr!("Виден в демонстрации экрана", "Visible in screen sharing", "Видно в демонстрації екрана"),
                Some(tr!(
                    "Discord, OBS и скриншоты видят оверлей. Если записывается весь монитор, он попадает и в клип",
                    "Discord, OBS and screenshots see the overlay. When the whole monitor is recorded, it gets into the clip too",
                    "Discord, OBS і скриншоти бачать оверлей. Якщо записується весь монітор, він потрапляє і в кліп"
                )),
                |ui| {
                    w::switch(ui, &mut self.cfg.overlay_visible_in_capture, tr!("Виден в демонстрации экрана", "Visible in screen sharing", "Видно в демонстрації екрана"));
                },
            );
        });
        ui.add_space(space::S24);
        let hud_key = hotkeys::describe(&self.cfg.hotkey_hud);
        w::group(ui, Some(tr!("Счётчик FPS", "FPS counter", "Лічильник FPS")), |g| {
            let sub = trf!(["Поверх игры, {} - показать или скрыть", "Over the game, {} shows or hides it", "Поверх гри, {} - показати або сховати"], hud_key);
            g.row(tr!("Показывать счётчик", "Show the counter", "Показувати лічильник"), Some(&sub), |ui| {
                w::switch(ui, &mut self.cfg.hud_enabled, tr!("Показывать счётчик", "Show the counter", "Показувати лічильник"));
            });
            g.row(tr!("Положение", "Position", "Положення"), None, |ui| {
                let opts = [
                    (tr!("Слева вверху", "Top left", "Ліворуч угорі"), true),
                    (tr!("Справа вверху", "Top right", "Праворуч угорі"), true),
                    (tr!("Слева внизу", "Bottom left", "Ліворуч унизу"), true),
                    (tr!("Справа внизу", "Bottom right", "Праворуч унизу"), true),
                ];
                let mut sel = self.cfg.hud_corner.min(3) as usize;
                if w::segmented(ui, "hud-corner", &opts, &mut sel) {
                    self.cfg.hud_corner = sel as u8;
                }
            });
            g.row(tr!("FPS", "FPS", "FPS"), Some(tr!("Сколько новых кадров игра выводит в секунду", "How many new frames the game shows per second", "Скільки нових кадрів гра виводить за секунду")), |ui| {
                w::switch(ui, &mut self.cfg.hud_fps, "FPS");
            });
            g.row(
                tr!("Задержка (LAT)", "Latency (LAT)", "Затримка (LAT)"),
                Some(tr!(
                    "От нажатия до экрана: кадры игры и ожидание экрана по меткам времени Windows",
                    "From input to the screen: the game's frames and the wait for the screen, from Windows timestamps",
                    "Від натискання до екрана: кадри гри та очікування екрана за мітками часу Windows"
                )),
                |ui| {
                    w::switch(ui, &mut self.cfg.hud_latency, "LAT");
                },
            );
            g.row(
                tr!("Задержка вывода (DISP)", "Display latency (DISP)", "Затримка виводу (DISP)"),
                Some(tr!("От готового кадра до экрана", "From a finished frame to the screen", "Від готового кадру до екрана")),
                |ui| {
                    w::switch(ui, &mut self.cfg.hud_display_latency, "DISP");
                },
            );
            g.row(
                tr!("Пинг (PING)", "Ping (PING)", "Пінг (PING)"),
                Some(tr!("До сервера игры, если сервер отвечает на ping", "To the game's server, when it answers ping", "До сервера гри, якщо сервер відповідає на ping")),
                |ui| {
                    w::switch(ui, &mut self.cfg.hud_ping, "PING");
                },
            );
            let elevated = crate::elevation::is_elevated();
            g.row_rich(
                tr!("Точный FPS", "Exact FPS", "Точний FPS"),
                Some(if elevated {
                    tr!(
                        "Каждый кадр игры, и выше частоты монитора, по системной трассировке Windows (как в AfterCore). С генерацией кадров настоящий FPS серым",
                        "Every frame of the game, also above the screen's refresh rate, from the Windows event trace (as in AfterCore). With frame generation the real FPS in grey",
                        "Кожен кадр гри, і вище частоти монітора, за системним трасуванням Windows (як в AfterCore). З генерацією кадрів справжній FPS сірим"
                    )
                } else {
                    tr!(
                        "Нужен запуск от имени администратора («Запуск» ниже). Сейчас FPS не выше частоты монитора",
                        "Needs running as administrator (Startup below). For now FPS stops at the screen's refresh rate",
                        "Потрібен запуск від імені адміністратора («Запуск» нижче). Зараз FPS не вище частоти монітора"
                    )
                }),
                if elevated { None } else { Some(color::WARNING) },
                |ui| {
                    w::value_text(ui, if elevated { tr!("Включён", "On", "Увімкнено") } else { tr!("Недоступен", "Unavailable", "Недоступний") });
                },
            );
            g.row(tr!("Время кадра (FT)", "Frame time (FT)", "Час кадру (FT)"), Some(tr!("Миллисекунды на кадр", "Milliseconds per frame", "Мілісекунди на кадр")), |ui| {
                w::switch(ui, &mut self.cfg.hud_frame_time, tr!("Время кадра", "Frame time", "Час кадру"));
            });
            g.row(tr!("Загрузка видеокарты", "Graphics card load", "Завантаження відеокарти"), None, |ui| {
                w::switch(ui, &mut self.cfg.hud_gpu, "GPU");
            });
            g.row(tr!("Загрузка процессора", "Processor load", "Завантаження процесора"), None, |ui| {
                w::switch(ui, &mut self.cfg.hud_cpu, "CPU");
            });
            g.row(tr!("Занятая память", "Memory in use", "Зайнята пам'ять"), None, |ui| {
                w::switch(ui, &mut self.cfg.hud_ram, "RAM");
            });
            g.row(tr!("Часы", "Clock", "Годинник"), None, |ui| {
                w::switch(ui, &mut self.cfg.hud_clock, tr!("Часы", "Clock", "Годинник"));
            });
            g.row(
                tr!("Виден в записи", "Visible in recordings", "Видно в записі"),
                Some(tr!("Счётчик попадает в клипы, скриншоты и демонстрацию экрана", "The counter gets into clips, screenshots and screen sharing", "Лічильник потрапляє в кліпи, скриншоти та демонстрацію екрана")),
                |ui| {
                    w::switch(ui, &mut self.cfg.hud_in_capture, tr!("Виден в записи", "Visible in recordings", "Видно в записі"));
                },
            );
        });
        ui.add_space(space::S24);
        w::group(ui, Some(tr!("Уведомления", "Notifications", "Сповіщення")), |g| {
            g.row(tr!("Уведомление поверх игры", "Notification over the game", "Сповіщення поверх гри"), Some(tr!("Не попадает в запись", "Not captured in recordings", "Не потрапляє в запис")), |ui| {
                w::switch(ui, &mut self.cfg.notify_overlay, tr!("Уведомление поверх игры", "Notification over the game", "Сповіщення поверх гри"));
            });
            g.row(tr!("Звуковой сигнал", "Sound cue", "Звуковий сигнал"), None, |ui| {
                w::switch(ui, &mut self.cfg.notify_sound, tr!("Звуковой сигнал", "Sound cue", "Звуковий сигнал"));
            });
        });
        ui.add_space(space::S24);
        let mut check_now = false;
        w::group(ui, Some(tr!("О программе", "About", "Про програму")), |g| {
            if !st.adapter_name.is_empty() {
                g.row(tr!("Видеокарта", "Graphics card", "Відеокарта"), None, |ui| w::value_text(ui, &st.adapter_name));
            }
            let checking = self.update_manual && !self.update_done.load(Ordering::SeqCst);
            let found = self.update.as_ref().and_then(|s| s.lock().clone());
            let sub = if checking {
                tr!("Проверка…", "Checking…", "Перевірка…").to_string()
            } else if let Some(r) = &found {
                trf!(["Доступна версия {} - кнопка на главной", "Version {} is available - see Home", "Доступна версія {} - кнопка на головній"], r.version)
            } else if self.update_manual {
                tr!("У вас последняя версия", "You have the latest version", "У вас остання версія").to_string()
            } else {
                trf!(["Версия {}", "Version {}", "Версія {}"], env!("CARGO_PKG_VERSION"))
            };
            g.row(tr!("Обновления", "Updates", "Оновлення"), Some(&sub), |ui| {
                if w::pill_button(ui, tr!("Проверить", "Check", "Перевірити"), Kind::Ghost, size::BUTTON_SM, !checking).clicked() {
                    check_now = true;
                }
            });
            g.row(tr!("Выход из приложения", "Quit the app", "Вихід із програми"), None, |ui| {
                if w::pill_button(ui, tr!("Выйти", "Quit", "Вийти"), Kind::Ghost, size::BUTTON_SM, true).clicked() {
                    (self.quit)();
                }
            });
        });
        if check_now {
            self.check_updates(ui.ctx(), true);
        }
    }
}

fn page_title(ui: &mut Ui, t: &str) {
    ui.add_space(space::S4);
    ui.label(RichText::new(t).font(theme::title()).color(color::TEXT));
    ui.add_space(space::S16);
}

fn caption(ui: &mut Ui, t: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.add_space(space::S4);
        ui.label(RichText::new(t).font(theme::caption()).color(color::TEXT_SECONDARY));
    });
}

fn monitor_label(i: usize, m: &MonitorInfo) -> String {
    trf!(["Монитор {} - {}×{}{}", "Monitor {} - {}×{}{}", "Монітор {} - {}×{}{}"], i + 1, m.width, m.height, if m.primary { tr!(" (основной)", " (primary)", " (основний)") } else { "" })
}

fn rate_hint(rc: RateControl) -> &'static str {
    match rc {
        RateControl::Cbr => tr!("Постоянный битрейт: предсказуемый размер файла", "Constant bitrate: predictable file size", "Постійний бітрейт: передбачуваний розмір файлу"),
        RateControl::Vbr => tr!("Переменный битрейт: больше данных в динамичных сценах", "Variable bitrate: more data in busy scenes", "Змінний бітрейт: більше даних у динамічних сценах"),
        RateControl::Cqp => tr!("Постоянное качество: размер зависит от сцены", "Constant quality: size depends on the scene", "Постійна якість: розмір залежить від сцени"),
    }
}

fn bitrate_hint(cfg: &Config, out: (u32, u32)) -> String {
    let h = cfg.resolution.height().unwrap_or(if out.1 > 0 { out.1 } else { 1080 });
    let base = match h {
        0..=720 => 15.0,
        721..=1080 => 30.0,
        1081..=1440 => 50.0,
        _ => 90.0,
    } * (cfg.fps as f64 / 60.0).max(0.5).sqrt()
        * if cfg.codec == Codec::H264 { 1.0 } else { 0.7 };
    trf!(["Для {}p {} к/с чётко от {:.0} Мбит/с", "For {}p {} fps, sharp from {:.0} Mbps", "Для {}p {} к/с чітко від {:.0} Мбіт/с"], h, cfg.fps, (base / 5.0).round() * 5.0)
}

fn estimate_memory(cfg: &Config, out: (u32, u32)) -> String {
    let (pw, ph) = picture_size(cfg, out);
    let rate = if cfg.auto_bitrate { crate::config::auto_bitrate_mbps(cfg.codec, pw, ph, cfg.fps, cfg.bitrate_quality) } else { cfg.bitrate_mbps };
    let mbps = if cfg.rate_control == RateControl::Cqp {
        rate.max(40) as f64
    } else if cfg.auto_bitrate {
        rate as f64 * 1.3 // constant quality: busy scenes may go above the usual rate
    } else {
        rate as f64 * if cfg.rate_control == RateControl::Vbr { 1.1 } else { 1.0 }
    };
    let bytes = (mbps * 1_000_000.0 / 8.0 + 3.0 * 24_000.0) * cfg.replay_seconds as f64;
    let prefix = if cfg.rate_control == RateControl::Cqp || cfg.auto_bitrate { tr!("до ≈", "up to ≈", "до ≈") } else { "≈" };
    trf!(["Займёт {} {} оперативной памяти", "Uses {} {} of RAM", "Займе {} {} оперативної пам'яті"], prefix, util::format_bytes(bytes as u64))
}

// ------------------------------------------------------------------ in-game overlay

impl App {
    fn enter_overlay(&mut self, ctx: &egui::Context) {
        // The window was already moved, resized and put on top by `UiShared::place_as_overlay`.
        self.overlay = Some(OverlayMode { focused: false, opened: Instant::now(), frames: 0, gallery: false });
        self.overlay_clips_at = None;
        self.mics = audio::list_microphones(); // a headset may have been plugged in since
        self.shared.in_overlay.store(true, Ordering::SeqCst);
        self.shared.exclude_from_capture(!self.cfg.overlay_visible_in_capture);
    }

    /// Leaves overlay mode. `open_full` keeps the regular window open afterwards.
    fn exit_overlay(&mut self, ctx: &egui::Context, open_full: bool) {
        self.quick = None;
        if self.overlay.take().is_none() {
            return;
        }
        self.shared.in_overlay.store(false, Ordering::SeqCst);
        set_opaque(self.shared.hwnd.load(Ordering::SeqCst), true);
        self.shared.exclude_from_capture(false);
        self.shared.restore_after_overlay(open_full);
        set_transitions(self.shared.hwnd.load(Ordering::SeqCst), true);
        ctx.request_repaint();
    }

    fn overlay_ui(&mut self, ctx: &egui::Context, st: &Status) {
        if let Some(q) = &mut self.quick {
            match q.ui(ctx, &mut self.viewer_prefs) {
                super::quick::QuickAction::Close => {
                    self.quick = None;
                    self.shared.overlay_panel_rect();
                }
                super::quick::QuickAction::Delete => {
                    self.delete_pending = Some(q.path.clone());
                    self.delete_error = None;
                }
                super::quick::QuickAction::Step(d) => {
                    let i = self.quick_index as i64 + d as i64;
                    if i >= 0 && (i as usize) < self.quick_list.len() {
                        self.show_quick(ctx, i as usize);
                    }
                }
                super::quick::QuickAction::None => {}
            }
            return;
        }
        if self.overlay.as_ref().map_or(false, |o| o.gallery) {
            self.overlay_gallery_ui(ctx);
            return;
        }
        let frame = egui::Frame::none().fill(color::SURFACE).inner_margin(egui::Margin::symmetric(space::S20, space::S16));
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            ui.spacing_mut().item_spacing = vec2(space::S8, 0.0);
            // Header
            ui.horizontal(|ui| {
                ui.set_min_height(size::BUTTON_MD);
                ui.label(RichText::new("Instant Replay").font(theme::headline()).color(color::TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if w::icon_button(ui, glyph::CLOSE, tr!("Закрыть", "Close", "Закрити")).clicked() {
                        self.exit_overlay(ctx, false);
                    }
                    if w::icon_button(ui, glyph::SETTINGS, tr!("Открыть окно настроек", "Open the settings window", "Відкрити вікно налаштувань")).clicked() {
                        self.exit_overlay(ctx, true);
                    }
                });
            });
            overlay_divider(ui);

            // Gallery: recent clips.
            let open_folder = overlay_row(ui, glyph::FOLDER, tr!("Галерея", "Gallery", "Галерея"), None, |ui| {
                ui.label(RichText::new(glyph::CHEVRON_RIGHT).font(theme::icon(12.0)).color(color::TEXT_SECONDARY));
            });
            if open_folder {
                if let Some(ov) = &mut self.overlay {
                    ov.gallery = true;
                }
                self.gallery_at = None;
            }
            ui.add_space(space::S8);
            if self.overlay_clips_at.map_or(true, |t| t.elapsed() > Duration::from_secs(2)) {
                self.overlay_clips = crate::engine::scan_clips(&self.cfg.output_dir, 3);
                self.overlay_clips_at = Some(Instant::now());
            }
            if self.overlay_clips.is_empty() {
                ui.label(RichText::new(tr!("Пока пусто", "Nothing yet", "Поки порожньо")).font(theme::subhead()).color(color::TEXT_SECONDARY));
            } else {
                let avail = ui.available_width();
                let tile_w = ((avail - space::S8 * 2.0) / 3.0).floor();
                let mut picked: Option<usize> = None;
                let mut delete_req: Option<std::path::PathBuf> = None;
                let clips_now = self.overlay_clips.clone();
                let tile_tex: Vec<Option<egui::TextureId>> = clips_now.iter().map(|c| self.thumb_for(c)).collect();
                ui.horizontal(|ui| {
                    for (i, (clip, tex)) in self.overlay_clips.iter().zip(tile_tex.iter()).enumerate() {
                        let resp = gallery_tile(ui, clip, tile_w, size::OVERLAY_TILE_H, *tex);
                        if resp.clicked() {
                            picked = Some(i);
                        }
                        resp.context_menu(|ui| {
                            if clip_menu(ui, &clip.path) {
                                delete_req = Some(clip.path.clone());
                            }
                        });
                    }
                });
                if delete_req.is_some() {
                    self.delete_pending = delete_req;
                    self.delete_error = None;
                }
                if let Some(i) = picked {
                    // The arrows go on through every clip, not only the three tiles.
                    let all = crate::engine::scan_clips(&self.cfg.output_dir, 300);
                    let path = &clips_now[i].path;
                    let idx = all.iter().position(|c| &c.path == path);
                    match idx {
                        Some(idx) => self.open_from_overlay(ctx, all, idx),
                        None => self.open_from_overlay(ctx, clips_now.clone(), i),
                    }
                }
            }
            ui.add_space(space::S8);
            overlay_divider(ui);

            // Recording
            let rec = st.recording_since;
            let rec_sub = match rec {
                Some(t) => trf!(["Идёт запись {} · {} - стоп", "Recording {} · {} - stop", "Триває запис {} · {} - стоп"], util::format_duration(t.elapsed().as_secs_f64()), hotkeys::describe(&self.cfg.hotkey_record)),
                None => trf!(["{} - запуск", "{} - start", "{} - запуск"], hotkeys::describe(&self.cfg.hotkey_record)),
            };
            let mut toggle_rec = false;
            overlay_row(ui, glyph::VIDEO, tr!("Запись", "Recording", "Запис"), Some(&rec_sub), |ui| {
                let (g, name) = if rec.is_some() { (glyph::STOP, tr!("Остановить запись", "Stop recording", "Зупинити запис")) } else { (glyph::PLAY, tr!("Начать запись", "Start recording", "Почати запис")) };
                if w::icon_button(ui, g, name).clicked() {
                    toggle_rec = true;
                }
            });
            if toggle_rec {
                self.engine.send(Command::ToggleRecording);
            }

            // Screenshot: the overlay closes first, so the picture shows the game only.
            let shot_sub = trf!(["{} - скриншот", "{} - screenshot", "{} - скриншот"], hotkeys::describe(&self.cfg.hotkey_screenshot));
            let mut shoot = false;
            overlay_row(ui, glyph::CAMERA, tr!("Скриншот", "Screenshot", "Скриншот"), Some(&shot_sub), |ui| {
                if w::icon_button(ui, glyph::CAMERA, tr!("Сделать скриншот", "Take a screenshot", "Зробити скриншот")).clicked() {
                    shoot = true;
                }
            });
            if shoot {
                self.exit_overlay(ctx, false);
                self.engine.send(Command::Screenshot(350));
            }

            // Instant Replay
            let replay_sub = trf!(["{} - включение/выключение", "{} - on/off", "{} - увімк/вимк"], hotkeys::describe(&self.cfg.hotkey_toggle_replay));
            overlay_row(ui, glyph::HISTORY, tr!("Мгновенный повтор", "Instant replay", "Миттєвий повтор"), Some(&replay_sub), |ui| {
                let mut on = self.cfg.replay_enabled;
                if w::switch(ui, &mut on, tr!("Мгновенный повтор", "Instant replay", "Миттєвий повтор")).changed() {
                    self.cfg.replay_enabled = on;
                }
            });
            if self.cfg.replay_enabled {
                ui.add_space(space::S4);
                ui.horizontal(|ui| {
                    ui.add_space(space::S32);
                    let enabled = st.replay_active && st.buffered_secs > 0.5 && st.saving == 0;
                    let label = if st.saving > 0 { tr!("Сохранение…", "Saving…", "Збереження…") } else { tr!("Сохранить повтор", "Save replay", "Зберегти повтор") };
                    if w::pill_button(ui, label, Kind::Primary, size::BUTTON_MD, enabled).clicked() {
                        self.engine.send(Command::SaveReplay);
                    }
                    w::value_text(ui, &format!("{} · {}", util::format_duration(st.buffered_secs), hotkeys::describe(&self.cfg.hotkey_save_replay)));
                });
                ui.add_space(space::S8);
            }
            overlay_divider(ui);

            // Audio
            overlay_row(ui, glyph::MIC, tr!("Микрофон", "Microphone", "Мікрофон"), None, |ui| {
                w::switch(ui, &mut self.cfg.mic_enabled, tr!("Микрофон", "Microphone", "Мікрофон"));
            });
            if self.cfg.mic_enabled {
                // Which microphone: the same choice as on the Audio page.
                ui.horizontal(|ui| {
                    ui.add_space(space::S32);
                    let default = tr!("По умолчанию", "Default", "За замовчуванням");
                    let current = self
                        .cfg
                        .mic_device
                        .as_ref()
                        .and_then(|id| self.mics.iter().find(|m| &m.id == id).map(|m| m.name.clone()))
                        .unwrap_or_else(|| default.into());
                    let width = ui.available_width();
                    egui::ComboBox::from_id_salt("ov-mic").width(width).selected_text(RichText::new(current).font(theme::subhead())).show_ui(ui, |ui| {
                        if ui.selectable_label(self.cfg.mic_device.is_none(), default).clicked() {
                            self.cfg.mic_device = None;
                        }
                        for m in &self.mics {
                            if ui.selectable_label(self.cfg.mic_device.as_deref() == Some(m.id.as_str()), &m.name).clicked() {
                                self.cfg.mic_device = Some(m.id.clone());
                            }
                        }
                    });
                });
                ui.add_space(space::S8);
            }
            overlay_row(ui, glyph::VOLUME, tr!("Звук системы", "System audio", "Звук системи"), None, |ui| {
                w::switch(ui, &mut self.cfg.system_audio, tr!("Звук системы", "System audio", "Звук системи"));
            });
            if st.pending_restart {
                ui.add_space(space::S8);
                ui.label(RichText::new(tr!("Изменения применятся после остановки записи", "Changes apply after recording stops", "Зміни застосуються після зупинки запису")).font(theme::caption()).color(color::TEXT_SECONDARY));
            }
        });
    }
}

fn overlay_divider(ui: &mut Ui) {
    ui.add_space(space::S8);
    let r = ui.available_rect_before_wrap();
    ui.painter().hline(r.left()..=r.right(), r.top(), egui::Stroke::new(1.0_f32, color::HAIRLINE));
    ui.add_space(space::S8);
}

/// Icon + title (+ hotkey line) on the left, control on the right. Returns true when the
/// row itself (not the control) was clicked.
fn overlay_row(ui: &mut Ui, g: &str, title: &str, sub: Option<&str>, control: impl FnOnce(&mut Ui)) -> bool {
    let h = if sub.is_some() { size::OVERLAY_ROW + 8.0 } else { size::OVERLAY_ROW };
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, h), egui::Sense::click());
    let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), w::dur(theme::motion::HOVER));
    if hover > 0.0 {
        ui.painter().rect_filled(rect.expand2(vec2(space::S8, 0.0)), Rounding::same(radius::INPUT), w::lerp_color(Color32::TRANSPARENT, color::SURFACE_HOVER, hover));
    }
    let p = ui.painter();
    p.text(egui::pos2(rect.left() + 10.0, rect.center().y), egui::Align2::CENTER_CENTER, g, theme::icon(18.0), color::TEXT);
    let x = rect.left() + space::S32;
    match sub {
        Some(s) => {
            p.text(egui::pos2(x, rect.center().y - 1.0), egui::Align2::LEFT_BOTTOM, title, theme::body_strong(), color::TEXT);
            p.text(egui::pos2(x, rect.center().y + 2.0), egui::Align2::LEFT_TOP, s, theme::caption(), color::TEXT_SECONDARY);
        }
        None => {
            p.text(egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, title, theme::body_strong(), color::TEXT);
        }
    }
    let ctrl_rect = egui::Rect::from_min_max(egui::pos2(rect.center().x, rect.top()), rect.max);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ctrl_rect).layout(Layout::right_to_left(Align::Center)));
    control(&mut child);
    resp.clicked()
}

fn gallery_tile(ui: &mut Ui, clip: &crate::engine::ClipInfo, tile_w: f32, tile_h: f32, tex: Option<egui::TextureId>) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(tile_w, tile_h), egui::Sense::click());
    let name = clip.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(name);
    let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), w::dur(theme::motion::HOVER));
    let rounding = Rounding::same(radius::INPUT);
    match tex {
        Some(id) => {
            // The 16:9 picture fills the tile; the sides that do not fit are cropped.
            let want = tile_w / tile_h;
            let have = crate::thumbs::W as f32 / crate::thumbs::H as f32;
            let uv = if want < have {
                let k = want / have;
                egui::Rect::from_min_max(egui::pos2((1.0 - k) / 2.0, 0.0), egui::pos2((1.0 + k) / 2.0, 1.0))
            } else {
                let k = have / want;
                egui::Rect::from_min_max(egui::pos2(0.0, (1.0 - k) / 2.0), egui::pos2(1.0, (1.0 + k) / 2.0))
            };
            let tint = w::lerp_color(Color32::from_gray(205), Color32::WHITE, hover);
            egui::Image::new(egui::load::SizedTexture::new(id, rect.size())).uv(uv).rounding(rounding).tint(tint).paint_at(ui, rect);
        }
        None => {
            let fill = w::lerp_color(color::SURFACE_RAISED, color::SURFACE_HOVER, hover);
            ui.painter().rect_filled(rect, rounding, fill);
            ui.painter().text(rect.center() - vec2(0.0, 4.0), egui::Align2::CENTER_CENTER, if clip.is_replay { glyph::HISTORY } else { glyph::VIDEO }, theme::icon(18.0), color::TEXT_SECONDARY);
        }
    }
    let pill_text = |ui: &mut Ui, at: egui::Pos2, align_right: bool, text: String| {
        let g = ui.fonts(|f| f.layout_no_wrap(text, theme::caption(), Color32::PLACEHOLDER));
        let size = g.size() + vec2(10.0, 4.0);
        let min = if align_right { egui::pos2(at.x - size.x, at.y - size.y) } else { at };
        let pill = egui::Rect::from_min_size(min, size);
        let p = ui.painter();
        p.rect_filled(pill, Rounding::same(radius::PILL), Color32::from_black_alpha(180));
        p.galley(pill.min + vec2(5.0, 2.0), g, color::TEXT);
    };
    pill_text(ui, rect.min + vec2(6.0, 6.0), false, util::format_duration(clip.secs));
    pill_text(ui, rect.right_bottom() - vec2(6.0, 6.0), true, util::format_bytes(clip.bytes));
    resp
}

/// Picture size the encoder works at (the running pipeline's, or an estimate from the settings).
fn picture_size(cfg: &Config, out: (u32, u32)) -> (u32, u32) {
    if let Some(h) = cfg.resolution.height() {
        return ((h * 16 / 9) & !1, h);
    }
    if out.0 > 0 && out.1 > 0 {
        out
    } else {
        (1920, 1080)
    }
}

/// Right-click menu of a clip: "Show in folder" is done here, true when "Delete" was chosen.
fn clip_menu(ui: &mut Ui, path: &std::path::Path) -> bool {
    let mut delete = false;
    ui.set_min_width(190.0);
    let item = |ui: &mut Ui, text: &str, col: Color32| ui.add(egui::Button::new(RichText::new(text).font(theme::subhead()).color(col)).frame(false)).clicked();
    if item(ui, tr!("Открыть расположение", "Show in folder", "Відкрити розташування"), color::TEXT) {
        util::open_in_explorer(path, true);
        ui.close_menu();
    }
    if item(ui, tr!("Удалить", "Delete", "Видалити"), color::RECORD) {
        delete = true;
        ui.close_menu();
    }
    delete
}

impl App {
    /// Confirmation before a clip goes to the Recycle Bin (opened by the right-click menu).
    fn delete_dialog(&mut self, ctx: &egui::Context) {
        let Some(path) = self.delete_pending.clone() else { return };
        let mut confirm = false;
        let mut cancel = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let screen = ctx.screen_rect();
        egui::Area::new(egui::Id::new("delete_modal")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
            let (_, scrim) = ui.allocate_exact_size(screen.size(), egui::Sense::click());
            ui.painter().rect_filled(screen, Rounding::ZERO, Color32::from_black_alpha(170));
            if scrim.clicked() {
                cancel = true;
            }
            let w = (screen.width() - space::S24 * 2.0).clamp(260.0, 380.0);
            let card = egui::Rect::from_center_size(screen.center(), vec2(w, 200.0));
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(card), |ui| {
                egui::Frame::none().fill(color::SURFACE).rounding(Rounding::same(radius::CARD)).inner_margin(egui::Margin::same(space::S20)).show(ui, |ui| {
                    ui.set_width(w - space::S20 * 2.0);
                    ui.label(RichText::new(tr!("Удалить клип?", "Delete this clip?", "Видалити кліп?")).font(theme::title()).color(color::TEXT));
                    ui.add_space(space::S8);
                    let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    ui.add(egui::Label::new(RichText::new(name).font(theme::body()).color(color::TEXT_SECONDARY)).truncate());
                    ui.add_space(space::S4);
                    let note = match &self.delete_error {
                        Some(e) => trf!(["Не удалось удалить: {}", "Could not delete: {}", "Не вдалося видалити: {}"], e),
                        None => tr!("Файл будет перемещён в Корзину", "The file will be moved to the Recycle Bin", "Файл буде переміщено до Кошика").to_string(),
                    };
                    ui.label(RichText::new(note).font(theme::subhead()).color(if self.delete_error.is_some() { color::RECORD } else { color::TEXT_SECONDARY }));
                    ui.add_space(space::S16);
                    ui.horizontal(|ui| {
                        if w::pill_button(ui, tr!("Удалить", "Delete", "Видалити"), Kind::Record, size::BUTTON_MD, true).clicked() {
                            confirm = true;
                        }
                        if w::pill_button(ui, tr!("Отмена", "Cancel", "Скасувати"), Kind::Ghost, size::BUTTON_MD, true).clicked() {
                            cancel = true;
                        }
                    });
                });
            });
        });
        if confirm {
            if self.quick.as_ref().map_or(false, |q| q.path == path) {
                // The player holds the file open: stop it first, and back to the overlay panel.
                self.quick = None;
                self.shared.overlay_panel_rect();
            }
            crate::thumbs::forget(&path); // needs the file to find its picture: before it goes away
            match crate::recycle::recycle(&path) {
                Ok(()) => {
                    self.engine.status.write().recent.retain(|c| c.path != path);
                    self.overlay_clips.retain(|c| c.path != path);
                    self.gallery_clips.retain(|c| c.path != path);
                    self.gallery_at = None;
                    self.thumb_tex.remove(&path);
                    self.overlay_clips_at = None;
                    self.home_clips.retain(|c| c.path != path);
                    self.quick_list.retain(|c| c.path != path);
                    self.home_clips_at = None;
                    self.delete_pending = None;
                    self.delete_error = None;
                }
                Err(e) => {
                    log::error!("delete {}: {e}", path.display());
                    self.delete_error = Some(e);
                }
            }
        } else if cancel {
            self.delete_pending = None;
            self.delete_error = None;
        }
    }
}

// ------------------------------------------------------------------ game profiles

impl App {
    fn page_games(&mut self, ui: &mut Ui, st: &Status) {
        page_title(ui, tr!("Игры", "Games", "Ігри"));
        caption(ui, tr!(
            "Пока игра запущена, записывается её окно, а настройки ниже заменяют общие. Когда игра закрыта, всё возвращается как было",
            "While a game runs, its window is recorded and the settings below replace the general ones. When it closes, everything goes back",
            "Поки гра запущена, записується її вікно, а налаштування нижче замінюють загальні. Коли гру закрито, все повертається як було"
        ));
        ui.add_space(space::S12);

        // Add a game from the running windows.
        let mut candidates: Vec<(String, String)> = Vec::new();
        for win in &self.windows {
            let known = self.cfg.profiles.iter().any(|p| p.exe.eq_ignore_ascii_case(&win.exe)) || candidates.iter().any(|(e, _)| e.eq_ignore_ascii_case(&win.exe));
            if !win.exe.is_empty() && !known {
                candidates.push((win.exe.clone(), win.title.clone()));
            }
        }
        let mut add: Option<(String, String)> = None;
        let mut refresh = false;
        w::group(ui, Some(tr!("Добавить игру", "Add a game", "Додати гру")), |g| {
            g.row(
                tr!("Запущенная программа", "Running program", "Запущена програма"),
                Some(tr!("Запустите игру и выберите её из списка", "Start the game and pick it from the list", "Запустіть гру та виберіть її зі списку")),
                |ui| {
                    if w::icon_button(ui, glyph::REFRESH, tr!("Обновить список", "Refresh list", "Оновити список")).clicked() {
                        refresh = true;
                    }
                    egui::ComboBox::from_id_salt("game-add")
                        .width(220.0)
                        .selected_text(RichText::new(tr!("Выбрать…", "Choose…", "Вибрати…")).font(theme::subhead()))
                        .show_ui(ui, |ui| {
                            for (exe, title) in &candidates {
                                let label = format!("{}  ·  {}", exe, title);
                                if ui.selectable_label(false, label).clicked() {
                                    add = Some((exe.clone(), exe.trim_end_matches(".exe").trim_end_matches(".EXE").to_string()));
                                }
                            }
                        });
                },
            );
        });
        if refresh {
            self.refresh_sources();
        }
        if let Some((exe, name)) = add {
            self.cfg.profiles.push(crate::config::GameProfile::from_config(&exe, &name, &self.cfg));
        }

        let mut remove: Option<usize> = None;
        for i in 0..self.cfg.profiles.len() {
            ui.add_space(space::S24);
            let active = st.active_profile.as_deref().map_or(false, |a| {
                let p = &self.cfg.profiles[i];
                a == if p.name.is_empty() { p.exe.as_str() } else { p.name.as_str() }
            });
            let title = {
                let p = &self.cfg.profiles[i];
                let base = if p.name.is_empty() { p.exe.clone() } else { p.name.clone() };
                if active { format!("{base}  ·  {}", tr!("сейчас активна", "active now", "зараз активна")) } else { base }
            };
            let p = &mut self.cfg.profiles[i];
            w::group(ui, Some(&title), |g| {
                g.row(tr!("Программа", "Program", "Програма"), None, |ui| {
                    if w::pill_button(ui, tr!("Удалить", "Remove", "Видалити"), Kind::Ghost, size::BUTTON_SM, true).clicked() {
                        remove = Some(i);
                    }
                    w::value_text(ui, &p.exe);
                });
                g.row(tr!("Частота кадров", "Frame rate", "Частота кадрів"), None, |ui| {
                    let labels: Vec<String> = FPS_STEPS.iter().map(|f| f.to_string()).collect();
                    let opts: Vec<(&str, bool)> = labels.iter().map(|s| (s.as_str(), true)).collect();
                    let mut sel = FPS_STEPS.iter().position(|&f| f == p.fps).unwrap_or(1);
                    if w::segmented(ui, &format!("game-fps-{i}"), &opts, &mut sel) {
                        p.fps = FPS_STEPS[sel];
                    }
                });
                g.row(tr!("Разрешение", "Resolution", "Роздільність"), None, |ui| {
                    egui::ComboBox::from_id_salt(("game-res", i)).width(180.0).selected_text(RichText::new(p.resolution.label()).font(theme::subhead())).show_ui(ui, |ui| {
                        for r in Resolution::ALL {
                            if ui.selectable_label(p.resolution == r, r.label()).clicked() {
                                p.resolution = r;
                            }
                        }
                    });
                });
                g.row(tr!("Качество", "Quality", "Якість"), None, |ui| {
                    let opts: Vec<(&str, bool)> = crate::config::BitrateQuality::ALL.iter().map(|q| (q.label(), true)).collect();
                    let mut sel = crate::config::BitrateQuality::ALL.iter().position(|q| *q == p.quality).unwrap_or(1);
                    if w::segmented(ui, &format!("game-q-{i}"), &opts, &mut sel) {
                        p.quality = crate::config::BitrateQuality::ALL[sel];
                    }
                });
                g.row(tr!("Длительность буфера", "Buffer length", "Тривалість буфера"), None, |ui| {
                    let idx = REPLAY_STEPS.iter().position(|&s| s >= p.replay_seconds).unwrap_or(REPLAY_STEPS.len() - 1);
                    let mut fidx = idx as f32;
                    w::value_text(ui, &util::format_duration(p.replay_seconds as f64));
                    if w::slider(ui, &format!("game-len-{i}"), &mut fidx, 0.0, (REPLAY_STEPS.len() - 1) as f32, 1.0, 180.0).changed() {
                        p.replay_seconds = REPLAY_STEPS[fidx.round() as usize];
                    }
                });
            });
        }
        if let Some(i) = remove {
            self.cfg.profiles.remove(i);
        }
    }
}

/// Makes the window fully see-through (layered, alpha 0) or normal again. Used while the overlay
/// shows up, so the window never flashes with stale content before its first frame is drawn.
fn set_opaque(hwnd: isize, opaque: bool) {
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::UI::WindowsAndMessaging::*;
    if hwnd == 0 {
        return;
    }
    unsafe {
        let h = HWND(hwnd as _);
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        let layered = WS_EX_LAYERED.0 as isize;
        if opaque {
            if ex & layered != 0 {
                let _ = SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA);
                SetWindowLongPtrW(h, GWL_EXSTYLE, ex & !layered);
            }
        } else {
            if ex & layered == 0 {
                SetWindowLongPtrW(h, GWL_EXSTYLE, ex | layered);
            }
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), 0, LWA_ALPHA);
        }
    }
}

/// Windows animates a window that appears or disappears (fade and scale); for the overlay that
/// shows up as a flicker, so the animation is off while the overlay is in use.
fn set_transitions(hwnd: isize, enabled: bool) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
    if hwnd == 0 {
        return;
    }
    const DWMWA_TRANSITIONS_FORCEDISABLED: i32 = 3;
    let disabled: u32 = if enabled { 0 } else { 1 };
    unsafe {
        let _ = DwmSetWindowAttribute(HWND(hwnd as _), DWMWINDOWATTRIBUTE(DWMWA_TRANSITIONS_FORCEDISABLED), &disabled as *const u32 as *const _, 4);
    }
}

// ------------------------------------------------------------------ clip previews

impl App {
    /// Turns finished preview pictures into textures.
    fn poll_thumbs(&mut self, ctx: &egui::Context) {
        for (path, pic) in self.thumbs.finished() {
            let tex = pic.map(|rgba| {
                let img = egui::ColorImage::from_rgba_unmultiplied([crate::thumbs::W, crate::thumbs::H], &rgba);
                ctx.load_texture(format!("thumb:{}", path.display()), img, egui::TextureOptions::LINEAR)
            });
            self.thumb_tex.insert(path, tex);
        }
    }

    /// Texture of a clip's preview; the first call asks for it to be made in the background.
    fn thumb_for(&mut self, clip: &crate::engine::ClipInfo) -> Option<egui::TextureId> {
        if !self.thumb_tex.contains_key(&clip.path) {
            self.thumb_tex.insert(clip.path.clone(), None);
            self.thumbs.request(clip.path.clone(), clip.secs, clip.is_replay);
        }
        self.thumb_tex.get(&clip.path).and_then(|t| t.as_ref()).map(|t| t.id())
    }
}

/// A clip's preview in a list row (a film icon on a plain tile while it is being made).
fn clip_thumb(ui: &mut Ui, tex: Option<egui::TextureId>, is_replay: bool, size: egui::Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let rounding = Rounding::same(radius::INPUT);
    match tex {
        Some(id) => {
            egui::Image::new(egui::load::SizedTexture::new(id, size)).rounding(rounding).paint_at(ui, rect);
        }
        None => {
            ui.painter().rect_filled(rect, rounding, color::SURFACE_RAISED);
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, if is_replay { glyph::FILM } else { glyph::VIDEO }, theme::icon(16.0), color::TEXT_SECONDARY);
        }
    }
    ui.add_space(space::S4);
}

// ------------------------------------------------------------------ overlay gallery page

impl App {
    /// Opens a clip from the overlay in the quick view: it plays right over the game, the overlay
    /// window grows into a card in the middle of the monitor. Closing it brings the panel back.
    fn open_from_overlay(&mut self, ctx: &egui::Context, list: Vec<crate::engine::ClipInfo>, index: usize) {
        self.quick_list = list;
        self.show_quick(ctx, index);
        self.shared.overlay_quick_rect();
    }

    /// Shows clip `index` of the quick-view list (the previous player stops first).
    fn show_quick(&mut self, ctx: &egui::Context, index: usize) {
        let Some(clip) = self.quick_list.get(index).cloned() else { return };
        self.quick = None;
        self.quick_index = index;
        let (prev, next) = (index > 0, index + 1 < self.quick_list.len());
        self.quick = Some(super::quick::QuickView::open(ctx, &clip.path, clip.bytes, &self.viewer_prefs, prev, next));
        ctx.request_repaint();
    }

    /// The overlay's gallery: every clip of the save folder as a grid of previews.
    fn overlay_gallery_ui(&mut self, ctx: &egui::Context) {
        if self.gallery_at.map_or(true, |t| t.elapsed() > Duration::from_secs(3)) {
            self.gallery_clips = crate::engine::scan_clips(&self.cfg.output_dir, 300);
            self.gallery_at = Some(Instant::now());
        }
        let mut back = false;
        let mut close = false;
        let mut folder = false;
        let mut picked: Option<(Vec<crate::engine::ClipInfo>, usize)> = None;
        let mut delete_req: Option<std::path::PathBuf> = None;
        let all_clips = self.gallery_clips.clone();
        let games = crate::engine::clip_games(&all_clips);
        let mut kind = self.gallery_kind;
        let mut game = self.gallery_game.clone().filter(|g| games.iter().any(|(n, _)| n == g));
        let frame = egui::Frame::none().fill(color::SURFACE).inner_margin(egui::Margin::symmetric(space::S20, space::S16));
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            ui.spacing_mut().item_spacing = vec2(space::S8, 0.0);
            ui.horizontal(|ui| {
                ui.set_min_height(size::BUTTON_MD);
                if w::icon_button(ui, glyph::BACK, tr!("Назад", "Back", "Назад")).clicked() {
                    back = true;
                }
                ui.label(RichText::new(tr!("Галерея", "Gallery", "Галерея")).font(theme::headline()).color(color::TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if w::icon_button(ui, glyph::CLOSE, tr!("Закрыть", "Close", "Закрити")).clicked() {
                        close = true;
                    }
                    if w::icon_button(ui, glyph::FOLDER, tr!("Открыть папку", "Open folder", "Відкрити теку")).clicked() {
                        folder = true;
                    }
                });
            });
            overlay_divider(ui);
            ui.add_space(space::S4);
            let kinds = [tr!("Все", "All", "Усі"), tr!("Повторы", "Replays", "Повтори"), tr!("Записи", "Recordings", "Записи")];
            let kind_opts: Vec<(&str, bool)> = kinds.iter().map(|k| (*k, true)).collect();
            let mut sel = kind as usize;
            if w::segmented(ui, "gal-kind", &kind_opts, &mut sel) {
                kind = sel as u8;
            }
            if !games.is_empty() {
                ui.add_space(space::S8);
                egui::ScrollArea::horizontal().id_salt("gal-games").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let all_kind = if game.is_none() { Kind::Primary } else { Kind::Ghost };
                        if w::pill_button(ui, tr!("Все игры", "All games", "Усі ігри"), all_kind, size::BUTTON_SM, true).clicked() {
                            game = None;
                        }
                        for (name, n) in &games {
                            let k = if game.as_deref() == Some(name.as_str()) { Kind::Primary } else { Kind::Ghost };
                            if w::pill_button(ui, &format!("{} · {}", game_label(name), n), k, size::BUTTON_SM, true).clicked() {
                                game = Some(name.clone());
                            }
                        }
                    });
                });
            }
            ui.add_space(space::S12);
            let clips = crate::engine::filter_clips(&all_clips, kind, game.as_deref());
            if clips.is_empty() {
                let empty = if all_clips.is_empty() { tr!("Пока пусто", "Nothing yet", "Поки порожньо") } else { tr!("Ничего не найдено", "Nothing found", "Нічого не знайдено") };
                ui.label(RichText::new(empty).font(theme::subhead()).color(color::TEXT_SECONDARY));
                return;
            }
            let gap = space::S8;
            let tile_w = ((ui.available_width() - gap) / 2.0).floor();
            let tile_h = (tile_w * 9.0 / 16.0).round();
            let rows = (clips.len() + 1) / 2;
            // Only the visible rows are drawn, so only their previews are asked for.
            egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, tile_h + gap, rows, |ui, range| {
                ui.spacing_mut().item_spacing = vec2(gap, gap);
                for r in range {
                    ui.horizontal(|ui| {
                        for c in 0..2 {
                            let Some(clip) = clips.get(r * 2 + c) else { continue };
                            let tex = self.thumb_for(clip);
                            let resp = gallery_tile(ui, clip, tile_w, tile_h, tex);
                            if resp.clicked() {
                                picked = Some((clips.clone(), r * 2 + c));
                            }
                            resp.context_menu(|ui| {
                                if clip_menu(ui, &clip.path) {
                                    delete_req = Some(clip.path.clone());
                                }
                            });
                        }
                    });
                }
            });
        });
        self.gallery_kind = kind;
        self.gallery_game = game;
        if delete_req.is_some() {
            self.delete_pending = delete_req;
            self.delete_error = None;
        }
        if folder {
            let _ = std::fs::create_dir_all(&self.cfg.output_dir);
            util::open_in_explorer(&self.cfg.output_dir, false);
        }
        if back {
            if let Some(ov) = &mut self.overlay {
                ov.gallery = false;
            }
        }
        if close {
            self.exit_overlay(ctx, false);
        } else if let Some((list, i)) = picked {
            self.open_from_overlay(ctx, list, i);
        }
    }
}

/// How a game (capture source) is named in the gallery filter.
fn game_label(name: &str) -> String {
    if name == "Desktop" {
        tr!("Рабочий стол", "Desktop", "Робочий стіл").to_string()
    } else {
        name.to_string()
    }
}
