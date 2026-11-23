#![windows_subsystem = "windows"]
//! Instant Replay — background replay buffer and screen recorder for Windows.

#[macro_use]
mod i18n;
mod audio;
mod autostart;
mod capture;
mod config;
mod encoder;
mod engine;
mod gpu;
mod hotkeys;
mod hud;
mod icon;
mod media;
mod overlay;
mod player;
mod recycle;
mod replay;
mod screenshot;
mod sound;
mod thumbs;
mod tools;
mod tray;
mod update;
mod ui;
mod util;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::core::w;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::Media::MediaFoundation::{MFStartup, MFSTARTUP_FULL, MF_VERSION};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};

use config::Config;
use engine::{Command, Notice, NoticeKind};
use hotkeys::Action;
use ui::app::{App, UiShared};

static EGUI_CTX: OnceLock<egui::Context> = OnceLock::new();

fn main() {
    util::init_logging();
    log::info!("Instant Replay {} starting", env!("CARGO_PKG_VERSION"));
    std::panic::set_hook(Box::new(|info| {
        log::error!("panic: {info}");
    }));

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // Single instance: a second launch just brings the running window forward.
        // Right after a self-update the old process may still be shutting down: wait for it.
        let updated = std::env::args().any(|a| a == "--updated");
        let mut tries = 0;
        loop {
            let m = CreateMutexW(None, true, w!("Local\\InstantReplay.SingleInstance"));
            if GetLastError() == ERROR_ALREADY_EXISTS && !std::env::args().any(|a| a == "--selftest") {
                if updated && tries < 75 {
                    if let Ok(h) = m {
                        let _ = CloseHandle(h);
                    }
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
                tray::signal_existing_instance();
                return;
            }
            std::mem::forget(m);
            break;
        }
        if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
            log::error!("MFStartup: {e}");
        }
    }

    if let Some(i) = std::env::args().position(|a| a == "--selftest") {
        let args: Vec<String> = std::env::args().collect();
        selftest(args.get(i + 1).cloned().unwrap_or_else(|| ".".into()), args.get(i + 2).cloned());
        return;
    }

    let cfg = Config::load();
    i18n::set(cfg.language.as_deref().and_then(i18n::Lang::from_code).unwrap_or_else(i18n::system));
    update::cleanup();
    autostart::set(cfg.autostart);
    tools::warm_up();
    let start_hidden = std::env::args().any(|a| a == "--tray") || cfg.start_minimized;

    tray::enable_dark_menus();
    overlay::start();
    hud::start(&cfg);

    let shared = UiShared::new(!start_hidden);

    let notifier: engine::Notifier = Arc::new(|n: Notice| {
        let prefs = Config::load();
        if prefs.notify_sound {
            sound::play(match n.kind {
                NoticeKind::ReplaySaved => sound::Chime::Saved,
                NoticeKind::RecordingSaved => sound::Chime::RecStop,
                NoticeKind::RecordingStarted => sound::Chime::RecStart,
                NoticeKind::ReplayOn | NoticeKind::ReplayOff => sound::Chime::Toggle,
                NoticeKind::Error => sound::Chime::Error,
            });
        }
        if prefs.notify_overlay {
            overlay::show(n.clone());
        }
        if let Some(ctx) = EGUI_CTX.get() {
            ctx.request_repaint();
        }
    });

    let engine = engine::start(cfg.clone(), notifier.clone());

    let hk = {
        let engine = engine.clone();
        let shared_hk = shared.clone();
        let bindings = vec![
            (Action::SaveReplay, cfg.hotkey_save_replay),
            (Action::SaveLast, cfg.hotkey_save_last),
            (Action::ToggleRecording, cfg.hotkey_record),
            (Action::ToggleReplay, cfg.hotkey_toggle_replay),
            (Action::ToggleOverlay, cfg.hotkey_overlay),
            (Action::Screenshot, cfg.hotkey_screenshot),
            (Action::ToggleHud, cfg.hotkey_hud),
        ];
        hotkeys::Hotkeys::start(bindings, move |a| match a {
            Action::SaveReplay => engine.send(Command::SaveReplay),
            Action::SaveLast => engine.send(Command::SaveLast),
            Action::ToggleRecording => {
                // Stopping gets its own chime here; starting is confirmed by the engine once
                // the first keyframe is on disk.
                engine.send(Command::ToggleRecording);
            }
            Action::ToggleReplay => engine.send(Command::ToggleReplay),
            Action::Screenshot => engine.send(Command::Screenshot(0)),
            Action::ToggleHud => {
                hud::toggle();
                if let Some(ctx) = EGUI_CTX.get() {
                    ctx.request_repaint();
                }
            }
            Action::ToggleOverlay => {
                shared_hk.request_overlay();
                if let Some(ctx) = EGUI_CTX.get() {
                    ctx.request_repaint();
                }
            }
        })
    };

    let quit: Arc<dyn Fn() + Send + Sync> = {
        let engine = engine.clone();
        let shared = shared.clone();
        Arc::new(move || {
            shared.quitting.store(true, std::sync::atomic::Ordering::SeqCst);
            shared.hide();
            let (tx, rx) = crossbeam_channel::bounded(1);
            engine.send(Command::Shutdown(tx));
            let _ = rx.recv_timeout(Duration::from_secs(10));
            tray::remove();
            log::info!("exit");
            log::logger().flush();
            std::process::exit(0);
        })
    };

    // Tray icon + menu.
    {
        let engine_s = engine.clone();
        let state = Arc::new(move || {
            let c = Config::load();
            let s = engine_s.status.read();
            tray::MenuState {
                replay_on: s.replay_enabled,
                recording: s.recording_since.is_some(),
                save_hotkey: hotkeys::describe(&c.hotkey_save_replay),
                record_hotkey: hotkeys::describe(&c.hotkey_record),
            }
        });
        let engine_c = engine.clone();
        let shared_c = shared.clone();
        let quit_c = quit.clone();
        let on_cmd = Arc::new(move |c: tray::TrayCmd| match c {
            tray::TrayCmd::Show => {
                shared_c.show();
                if let Some(ctx) = EGUI_CTX.get() {
                    ctx.request_repaint();
                }
            }
            tray::TrayCmd::SaveReplay => engine_c.send(Command::SaveReplay),
            tray::TrayCmd::ToggleRecording => engine_c.send(Command::ToggleRecording),
            tray::TrayCmd::ToggleReplay => engine_c.send(Command::ToggleReplay),
            tray::TrayCmd::OpenFolder => {
                let dir = Config::load().output_dir;
                let _ = std::fs::create_dir_all(&dir);
                util::open_in_explorer(&dir, false);
            }
            tray::TrayCmd::Exit => {
                let q = quit_c.clone();
                std::thread::spawn(move || q());
            }
        });
        tray::start(state, on_cmd);
    }

    // Keep the tray icon in step with the engine state (works while the window is hidden).
    {
        let status = engine.status.clone();
        std::thread::Builder::new()
            .name("tray-state".into())
            .spawn(move || {
                let mut last = None;
                loop {
                    let st = {
                        let s = status.read();
                        if s.recording_since.is_some() {
                            icon::IconState::Recording
                        } else if s.replay_active {
                            icon::IconState::Active
                        } else {
                            icon::IconState::Idle
                        }
                    };
                    if last != Some(st) {
                        tray::set_icon(st);
                        last = Some(st);
                    }
                    std::thread::sleep(Duration::from_millis(400));
                }
            })
            .ok();
    }

    let icon_px = icon::rgba(64, icon::IconState::Active);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Instant Replay")
            .with_app_id("InstantReplay")
            .with_inner_size([980.0, 700.0])
            .with_min_inner_size([840.0, 560.0])
            .with_icon(egui::IconData { rgba: icon_px, width: 64, height: 64 })
            .with_visible(!start_hidden),
        centered: true,
        ..Default::default()
    };

    let cfg_ui = cfg.clone();
    let engine_ui = engine.clone();
    let shared_ui = shared.clone();
    let quit_ui = quit.clone();
    let r = eframe::run_native(
        "Instant Replay",
        options,
        Box::new(move |cc| {
            let _ = EGUI_CTX.set(cc.egui_ctx.clone());
            if let Ok(h) = cc.window_handle() {
                if let RawWindowHandle::Win32(h) = h.as_raw() {
                    let hwnd = HWND(h.hwnd.get() as _);
                    shared_ui.hwnd.store(h.hwnd.get(), std::sync::atomic::Ordering::SeqCst);
                    dark_title_bar(hwnd);
                }
            }
            Ok(Box::new(App::new(cc, cfg_ui, engine_ui, hk, shared_ui, quit_ui)))
        }),
    );
    if let Err(e) = r {
        log::error!("UI failed: {e}");
    }
    quit();
}

/// Headless end-to-end check: buffer → save replay → record 6 s → stop. Results go to the log.
fn selftest(dir: String, codec: Option<String>) {
    log::info!("TOOLS dir={:?} justview={:?} ffprobe={:?}", tools::dir(), util::find_justview(), player::ffmpeg::ffprobe());
    let mut cfg = Config::default();
    cfg.output_dir = std::path::PathBuf::from(dir);
    cfg.replay_seconds = 15;
    cfg.save_last_seconds = 5;
    cfg.mic_enabled = std::env::args().any(|a| a == "--mic");
    cfg.codec = match codec.as_deref() {
        Some("hevc") => config::Codec::Hevc,
        Some("av1") => config::Codec::Av1,
        _ => config::Codec::H264,
    };
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--window") {
        cfg.source = config::CaptureSource::Window { title: String::new(), exe: args.get(i + 1).cloned().unwrap_or_default() };
    }
    if let Some(i) = args.iter().position(|a| a == "--bitrate") {
        cfg.auto_bitrate = false;
        cfg.bitrate_mbps = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(50);
    }
    if let Some(i) = args.iter().position(|a| a == "--res") {
        cfg.resolution = if args.get(i + 1).map(|s| s.as_str()) == Some("720") { config::Resolution::P720 } else { config::Resolution::P1080 };
    }
    if let Some(i) = args.iter().position(|a| a == "--rc") {
        cfg.rate_control = match args.get(i + 1).map(|s| s.as_str()) {
            Some("cbr") => config::RateControl::Cbr,
            Some("cqp") => config::RateControl::Cqp,
            _ => config::RateControl::Vbr,
        };
    }
    if let Some(i) = args.iter().position(|a| a == "--profile") {
        let exe = args.get(i + 1).cloned().unwrap_or_default();
        let mut p = config::GameProfile::from_config(&exe, "TestGame", &cfg);
        p.fps = 30;
        p.resolution = config::Resolution::P720;
        cfg.profiles.push(p);
    }
    let notifier: engine::Notifier = Arc::new(|n: Notice| log::info!("NOTICE {:?}: {} | {}", n.kind, n.title, n.detail));
    let e = engine::start(cfg, notifier);
    std::thread::sleep(Duration::from_secs(8));
    e.send(Command::SaveLast);
    std::thread::sleep(Duration::from_millis(1500));
    {
        let s = e.status.read();
        log::info!("STATUS running={} buffered={:.1}s bytes={} enc='{}' out={:?} dropped={} err={:?} profile={:?}", s.running, s.buffered_secs, s.buffer_bytes, s.encoder_name, s.out_size, s.dropped_frames, s.error, s.active_profile);
    }
    e.send(Command::SaveReplay);
    std::thread::sleep(Duration::from_secs(2));
    e.send(Command::ToggleRecording);
    std::thread::sleep(Duration::from_secs(6));
    e.send(Command::ToggleRecording);
    std::thread::sleep(Duration::from_secs(2));
    {
        let s = e.status.read();
        log::info!("STATUS dropped={} recent={:?}", s.dropped_frames, s.recent.iter().map(|c| (c.path.display().to_string(), c.secs, c.bytes)).collect::<Vec<_>>());
    }
    let (tx, rx) = crossbeam_channel::bounded(1);
    e.send(Command::Shutdown(tx));
    let _ = rx.recv_timeout(Duration::from_secs(10));
    log::info!("SELFTEST DONE");
    log::logger().flush();
}

fn dark_title_bar(hwnd: HWND) {
    unsafe {
        let on: i32 = 1;
        // DWMWA_USE_IMMERSIVE_DARK_MODE
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(20), &on as *const _ as _, 4);
        // DWMWA_CAPTION_COLOR = canvas, DWMWA_TEXT_COLOR = text
        let caption: u32 = 0x0000_0000;
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(35), &caption as *const _ as _, 4);
        let text: u32 = 0x00f7_f5f5;
        let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(36), &text as *const _ as _, 4);
    }
}
