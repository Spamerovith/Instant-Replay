#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(deprecated)]

#[macro_use]
mod i18n;
mod app;
mod audio;
mod decode;
mod ffmpeg;
mod formats;
mod loader;
mod pdf;
mod player;
mod register;
mod settings;
mod theme;
mod update;
mod widgets;
mod winutil;

use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let settings = settings::Settings::load();
    i18n::set(settings.lang.as_deref().and_then(i18n::Lang::from_code).unwrap_or_else(i18n::system));
    update::cleanup();

    // Служебные команды для установки/удаления ассоциаций.
    if let Some(a) = args.first().and_then(|a| a.to_str()) {
        match a {
            "--register" => {
                let ok = register::register().is_ok();
                if args.iter().any(|a| a == "--open-settings") {
                    register::open_default_apps();
                }
                std::process::exit(if ok { 0 } else { 1 });
            }
            "--unregister" => {
                std::process::exit(if register::unregister().is_ok() { 0 } else { 1 });
            }
            _ => {}
        }
    }

    let initial: Option<PathBuf> = args
        .iter()
        .find(|a| !a.to_string_lossy().starts_with("--"))
        .map(PathBuf::from)
        .map(|p| std::fs::canonicalize(&p).map(strip_unc).unwrap_or(p));

    // Декодирование стартует ДО создания окна — параллельно с инициализацией GPU.
    let loader = loader::Loader::new(3);
    if let Some(p) = &initial {
        if p.is_file() {
            loader.request(loader::Job::Decode(p.clone()), true);
            if let Some(dir) = p.parent() {
                loader.request(loader::Job::ScanDir(dir.to_path_buf(), p.clone()), false);
            }
        }
    }

    let (w, h) = initial_size(&settings);
    let icon = image::load_from_memory(include_bytes!("../assets/icon-256.png"))
        .map(|i| {
            let i = i.into_rgba8();
            egui::IconData { width: i.width(), height: i.height(), rgba: i.into_raw() }
        })
        .ok();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("JustView")
        .with_app_id("JustView")
        .with_inner_size([w, h])
        .with_min_inner_size([480.0, 360.0])
        .with_maximized(settings.maximized)
        .with_drag_and_drop(true);
    if let Some(i) = icon {
        viewport = viewport.with_icon(std::sync::Arc::new(i));
    }

    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        centered: true,
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        ..Default::default()
    };

    let initial_for_app = initial.filter(|p| p.exists());
    let dir_open = initial_for_app.as_ref().map(|p| p.is_dir()).unwrap_or(false);
    eframe::run_native(
        "JustView",
        options,
        Box::new(move |cc| {
            let mut app = app::App::new(cc, loader, if dir_open { None } else { initial_for_app.clone() }, settings);
            if dir_open {
                app.open_later(initial_for_app.unwrap());
            }
            Ok(Box::new(app))
        }),
    )
}

/// `\?\C:\...` → `C:\...` (для внешних программ и красивого пути).
fn strip_unc(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\?\UNC\") {
        PathBuf::from(format!(r"\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\?\") {
        PathBuf::from(rest)
    } else {
        p
    }
}

fn initial_size(s: &settings::Settings) -> (f32, f32) {
    if s.width >= 480.0 && s.height >= 360.0 {
        return (s.width, s.height);
    }
    let scale = unsafe { windows::Win32::UI::HiDpi::GetDpiForSystem() } as f32 / 96.0;
    match winutil::work_area() {
        Some((w, h)) => ((w / scale * 0.72).round(), (h / scale * 0.78).round()),
        None => (1280.0, 820.0),
    }
}
