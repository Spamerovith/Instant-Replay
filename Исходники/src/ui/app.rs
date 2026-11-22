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
    /// Work area (px) of the monitor showing the game when the overlay was requested.
    pub overlay_target: parking_lot::Mutex<Option<(i32, i32, i32, i32)>>,
    pub overlay_was_visible: AtomicBool,
}

impl UiShared {
    pub fn new(visible: bool) -> Arc<Self> {
        Arc::new(UiShared {
            hwnd: AtomicIsize::new(0),
            visible: AtomicBool::new(visible),
            quitting: AtomicBool::new(false),
            overlay_request: AtomicBool::new(false),
            in_overlay: AtomicBool::new(false),
            overlay_target: parking_lot::Mutex::new(None),
            overlay_was_visible: AtomicBool::new(visible),
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

    /// Called from the hotkey thread: remember where the game is and bring the window up.
    pub fn request_overlay(&self) {
        use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        if !self.in_overlay.load(Ordering::SeqCst) {
            unsafe {
                let mon = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
                let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                if GetMonitorInfoW(mon, &mut mi).as_bool() {
                    let r = mi.rcWork;
                    *self.overlay_target.lock() = Some((r.left, r.top, r.right, r.bottom));
                }
            }
            self.overlay_was_visible.store(self.visible.load(Ordering::SeqCst), Ordering::SeqCst);
            self.show();
        }
        self.overlay_request.store(true, Ordering::SeqCst);
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Video,
    Audio,
    Hotkeys,
    General,
}

fn pages() -> [(Page, &'static str, &'static str); 5] {
    [
    (Page::Home, glyph::HOME, tr!("Главная", "Home", "Головна")),
    (Page::Video, glyph::VIDEO, tr!("Видео", "Video", "Відео")),
    (Page::Audio, glyph::VOLUME, tr!("Звук", "Audio", "Звук")),
    (Page::Hotkeys, glyph::KEYBOARD, tr!("Горячие клавиши", "Hotkeys", "Гарячі клавіші")),
    (Page::General, glyph::SETTINGS, tr!("Общие", "General", "Загальні")),
    ]
}

const REPLAY_STEPS: [u32; 11] = [15, 30, 45, 60, 90, 120, 180, 300, 600, 900, 1200];
const FPS_STEPS: [u32; 6] = [30, 60, 120, 144, 165, 240];
const AUDIO_BITRATES: [u32; 3] = [128, 160, 192];

struct OverlayMode {
    was_visible: bool,
    prev_inner: Option<egui::Rect>,
    prev_outer: Option<egui::Rect>,
    focused: bool,
    opened: Instant,
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
            (Action::ToggleRecording, c.hotkey_record),
            (Action::ToggleReplay, c.hotkey_toggle_replay),
            (Action::ToggleOverlay, c.hotkey_overlay),
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
            let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
            if focused == Some(true) {
                ov.focused = true;
            }
            let lost = ov.focused && focused == Some(false) && ov.opened.elapsed() > Duration::from_millis(400);
            if esc || lost {
                self.exit_overlay(ctx, false);
            }
        }
        if self.overlay.is_some() {
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
            .frame(egui::Frame::none().fill(color::CANVAS).inner_margin(egui::Margin { left: space::S16, right: space::S8, top: space::S24, bottom: space::S16 }))
            .show(ctx, |ui| self.sidebar(ui, &status));

        egui::CentralPanel::default().frame(egui::Frame::none().fill(color::CANVAS)).show(ctx, |ui| {
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
        egui::Frame::none().fill(color::SURFACE).rounding(Rounding::same(radius::GROUP)).inner_margin(egui::Margin::symmetric(space::S16, space::S12)).show(ui, |ui| {
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
                egui::Frame::none().fill(color::SURFACE).rounding(Rounding::same(radius::CARD)).inner_margin(egui::Margin::same(space::S24)).show(ui, |ui| {
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
            if p == Page::Home || p == Page::Audio {
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
        ui.painter().rect_filled(ind, Rounding::same(radius::INPUT), color::SURFACE_RAISED);

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
        egui::Frame::none().fill(color::SURFACE).rounding(Rounding::same(radius::CARD)).inner_margin(egui::Margin::same(space::S24)).show(ui, |ui| {
            ui.set_width(width - space::S24 * 2.0);
            ui.horizontal(|ui| {
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
        egui::Frame::none().fill(color::SURFACE).rounding(Rounding::same(radius::CARD)).inner_margin(egui::Margin::same(space::S24)).show(ui, |ui| {
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
                        ui.label(RichText::new(tr!("Запись", "Recording", "Запис")).font(theme::headline()).color(color::TEXT));
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
        let mut open_clip: Option<std::path::PathBuf> = None;
        w::group(ui, None, |g| {
            if st.recent.is_empty() {
                g.row(tr!("Пока пусто", "Nothing yet", "Поки порожньо"), None, |ui| {
                    w::value_text(ui, &trf!(["{} или {}", "{} or {}", "{} або {}"], hotkeys::describe(&self.cfg.hotkey_save_replay), hotkeys::describe(&self.cfg.hotkey_record)));
                });
            }
            for clip in st.recent.iter() {
                let name = clip.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let meta = format!("{} · {}", util::format_duration(clip.secs), util::format_bytes(clip.bytes));
                let path = clip.path.clone();
                g.custom(size::ROW_MIN, |ui| {
                    let row = ui.max_rect();
                    let resp = ui.interact(row, egui::Id::new(("clip", &path)), egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), w::dur(theme::motion::HOVER));
                    if h > 0.0 {
                        ui.painter().rect_filled(row.expand2(vec2(space::S8, -4.0)), Rounding::same(radius::INPUT), w::lerp_color(Color32::TRANSPARENT, color::SURFACE_HOVER, h));
                    }
                    ui.label(RichText::new(if clip.is_replay { glyph::FILM } else { glyph::VIDEO }).font(theme::icon(14.0)).color(color::TEXT_SECONDARY));
                    ui.add(egui::Label::new(RichText::new(&name).font(theme::body()).color(color::TEXT)).truncate());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        w::value_text(ui, &meta);
                    });
                    if resp.clicked() {
                        open_clip = Some(path.clone());
                    }
                });
            }
        });
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
            egui::Frame::none().fill(color::SURFACE).rounding(Rounding::same(radius::GROUP)).inner_margin(egui::Margin::symmetric(space::S16, space::S12)).show(ui, |ui| {
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
        let mem = estimate_memory(&self.cfg);
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
                let hint = bitrate_hint(&self.cfg, st.out_size);
                g.row(tr!("Битрейт", "Bitrate", "Бітрейт"), Some(&hint), |ui| {
                    w::value_text(ui, &trf!(["{} Мбит/с", "{} Mbps", "{} Мбіт/с"], self.cfg.bitrate_mbps));
                    let mut v = self.cfg.bitrate_mbps as f32;
                    if w::slider(ui, "bitrate", &mut v, 5.0, 150.0, 1.0, 220.0).changed() {
                        self.cfg.bitrate_mbps = v as u32;
                    }
                });
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
            Action::ToggleRecording => &mut self.cfg.hotkey_record,
            Action::ToggleReplay => &mut self.cfg.hotkey_toggle_replay,
            Action::ToggleOverlay => &mut self.cfg.hotkey_overlay,
        }
    }

    fn page_hotkeys(&mut self, ui: &mut Ui) {
        page_title(ui, tr!("Горячие клавиши", "Hotkeys", "Гарячі клавіші"));
        let failed = self.hotkeys.failed();
        let rows = [
            (Action::SaveReplay, tr!("Сохранить повтор", "Save replay", "Зберегти повтор")),
            (Action::ToggleRecording, tr!("Начать / остановить запись", "Start / stop recording", "Почати / зупинити запис")),
            (Action::ToggleReplay, tr!("Включить / выключить Instant Replay", "Turn Instant Replay on / off", "Увімкнути / вимкнути Instant Replay")),
            (Action::ToggleOverlay, tr!("Открыть / закрыть оверлей", "Open / close overlay", "Відкрити / закрити оверлей")),
        ];
        let mut start: Option<Action> = None;
        w::group(ui, None, |g| {
            for (a, label) in rows {
                let hk = match a {
                    Action::SaveReplay => self.cfg.hotkey_save_replay,
                    Action::ToggleRecording => self.cfg.hotkey_record,
                    Action::ToggleReplay => self.cfg.hotkey_toggle_replay,
                    Action::ToggleOverlay => self.cfg.hotkey_overlay,
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
        if let Some(a) = start {
            self.capturing = Some(a);
            self.capture_hint = tr!("Нажмите сочетание…", "Press a shortcut…", "Натисніть комбінацію…").into();
            self.hotkeys.suspend();
        }
    }

    fn applied_hotkey(&self, a: Action) -> Hotkey {
        match a {
            Action::SaveReplay => self.applied.hotkey_save_replay,
            Action::ToggleRecording => self.applied.hotkey_record,
            Action::ToggleReplay => self.applied.hotkey_toggle_replay,
            Action::ToggleOverlay => self.applied.hotkey_overlay,
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
        });
        ui.add_space(space::S24);
        w::group(ui, Some(tr!("Запуск", "Startup", "Запуск")), |g| {
            g.row(tr!("Запускать вместе с Windows", "Start with Windows", "Запускати разом з Windows"), None, |ui| {
                w::switch(ui, &mut self.cfg.autostart, tr!("Запускать вместе с Windows", "Start with Windows", "Запускати разом з Windows"));
            });
            g.row(tr!("Открывать свёрнутым в трей", "Start minimized to tray", "Відкривати згорнутим у трей"), None, |ui| {
                w::switch(ui, &mut self.cfg.start_minimized, tr!("Открывать свёрнутым в трей", "Start minimized to tray", "Відкривати згорнутим у трей"));
            });
            g.row(tr!("Кнопка «Закрыть» сворачивает в трей", "The Close button minimizes to tray", "Кнопка «Закрити» згортає в трей"), Some(tr!("Выйти можно из меню значка в трее", "Quit from the tray icon menu", "Вийти можна з меню значка в треї")), |ui| {
                w::switch(ui, &mut self.cfg.close_to_tray, tr!("Закрытие сворачивает в трей", "Closing minimizes to tray", "Закриття згортає в трей"));
            });
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
    ui.horizontal(|ui| {
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

fn estimate_memory(cfg: &Config) -> String {
    let mbps = if cfg.rate_control == RateControl::Cqp { cfg.bitrate_mbps.max(40) as f64 } else { cfg.bitrate_mbps as f64 * if cfg.rate_control == RateControl::Vbr { 1.1 } else { 1.0 } };
    let bytes = (mbps * 1_000_000.0 / 8.0 + 3.0 * 24_000.0) * cfg.replay_seconds as f64;
    let prefix = if cfg.rate_control == RateControl::Cqp { tr!("до ≈", "up to ≈", "до ≈") } else { "≈" };
    trf!(["Займёт {} {} оперативной памяти", "Uses {} {} of RAM", "Займе {} {} оперативної пам'яті"], prefix, util::format_bytes(bytes as u64))
}

// ------------------------------------------------------------------ in-game overlay

impl App {
    fn enter_overlay(&mut self, ctx: &egui::Context) {
        let ppp = ctx.pixels_per_point();
        let (_l, t, r, b) = self.shared.overlay_target.lock().take().unwrap_or((0, 0, 1920, 1040));
        let w = size::OVERLAY_W;
        let (prev_inner, prev_outer) = ctx.input(|i| (i.viewport().inner_rect, i.viewport().outer_rect));
        let was_visible = self.shared.overlay_was_visible.load(Ordering::SeqCst);
        self.overlay = Some(OverlayMode { was_visible, prev_inner, prev_outer, focused: false, opened: Instant::now() });
        self.shared.in_overlay.store(true, Ordering::SeqCst);
        self.shared.exclude_from_capture(true);
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(vec2(w, 300.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(w, (b - t) as f32 / ppp)));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(r as f32 / ppp - w, t as f32 / ppp)));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Leaves overlay mode. `open_full` keeps the regular window open afterwards.
    fn exit_overlay(&mut self, ctx: &egui::Context, open_full: bool) {
        let Some(ov) = self.overlay.take() else { return };
        self.shared.in_overlay.store(false, Ordering::SeqCst);
        self.shared.exclude_from_capture(false);
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(vec2(840.0, 560.0)));
        let inner = ov.prev_inner.filter(|r| r.width() >= 840.0).map(|r| r.size()).unwrap_or(vec2(980.0, 700.0));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(inner));
        if let Some(o) = ov.prev_outer.filter(|_| ov.was_visible) {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(o.min));
        } else if let Some(cmd) = egui::ViewportCommand::center_on_screen(ctx) {
            ctx.send_viewport_cmd(cmd);
        }
        if !ov.was_visible && !open_full {
            self.hide_at = Some(Instant::now() + Duration::from_millis(120));
            ctx.request_repaint_after(Duration::from_millis(130));
        }
    }

    fn overlay_ui(&mut self, ctx: &egui::Context, st: &Status) {
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
                let _ = std::fs::create_dir_all(&self.cfg.output_dir);
                util::open_in_explorer(&self.cfg.output_dir, false);
            }
            ui.add_space(space::S8);
            if st.recent.is_empty() {
                ui.label(RichText::new(tr!("Пока пусто", "Nothing yet", "Поки порожньо")).font(theme::subhead()).color(color::TEXT_SECONDARY));
            } else {
                let avail = ui.available_width();
                let tile_w = ((avail - space::S8 * 2.0) / 3.0).floor();
                let mut picked: Option<std::path::PathBuf> = None;
                ui.horizontal(|ui| {
                    for clip in st.recent.iter().take(3) {
                        if gallery_tile(ui, clip, tile_w).clicked() {
                            picked = Some(clip.path.clone());
                        }
                    }
                });
                if let Some(p) = picked {
                    self.exit_overlay(ctx, true);
                    self.open_clip(ctx, &p);
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

fn gallery_tile(ui: &mut Ui, clip: &crate::engine::ClipInfo, tile_w: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(tile_w, size::OVERLAY_TILE_H), egui::Sense::click());
    let name = clip.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(name);
    let hover = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), w::dur(theme::motion::HOVER));
    let fill = w::lerp_color(color::SURFACE_RAISED, color::SURFACE_HOVER, hover);
    let p = ui.painter();
    p.rect_filled(rect, Rounding::same(radius::INPUT), fill);
    p.text(rect.center() - vec2(0.0, 4.0), egui::Align2::CENTER_CENTER, if clip.is_replay { glyph::HISTORY } else { glyph::VIDEO }, theme::icon(18.0), color::TEXT_SECONDARY);
    let dur = util::format_duration(clip.secs);
    let g = ui.fonts(|f| f.layout_no_wrap(dur, theme::caption(), Color32::PLACEHOLDER));
    let pill = egui::Rect::from_min_size(rect.min + vec2(6.0, 6.0), g.size() + vec2(10.0, 4.0));
    p.rect_filled(pill, Rounding::same(radius::PILL), Color32::from_black_alpha(180));
    p.galley(pill.min + vec2(5.0, 2.0), g, color::TEXT);
    p.text(egui::pos2(rect.center().x, rect.bottom() - 8.0), egui::Align2::CENTER_BOTTOM, util::format_bytes(clip.bytes), theme::caption(), color::TEXT_SECONDARY);
    resp
}
