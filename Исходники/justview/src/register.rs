//! Регистрация в Проводнике (только для текущего пользователя, без прав администратора).
//! Windows не даёт программе самой назначить себя приложением по умолчанию —
//! это подтверждает пользователь в «Параметры → Приложения по умолчанию → JustView».

use winreg::RegKey;
use winreg::enums::HKEY_CURRENT_USER;

use crate::formats::{Kind, registered_exts};

const APP: &str = "JustView";

fn progid(k: Kind) -> &'static str {
    match k {
        Kind::Video => "JustView.Video",
        Kind::Audio => "JustView.Audio",
        Kind::Pdf => "JustView.Pdf",
        _ => "JustView.Image",
    }
}

fn type_name(k: Kind) -> &'static str {
    match k {
        Kind::Video => tr!("Видео (JustView)", "Video (JustView)", "Відео (JustView)"),
        Kind::Audio => tr!("Аудио (JustView)", "Audio (JustView)", "Аудіо (JustView)"),
        Kind::Pdf => tr!("Документ PDF (JustView)", "PDF document (JustView)", "Документ PDF (JustView)"),
        _ => tr!("Изображение (JustView)", "Image (JustView)", "Зображення (JustView)"),
    }
}

pub fn register() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let exe_s = exe.to_string_lossy().to_string();
    let cmd = format!("\"{exe_s}\" \"%1\"");
    let icon = format!("\"{exe_s}\",0");
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = hkcu.create_subkey(r"Software\Classes")?.0;

    for k in [Kind::Image, Kind::Video, Kind::Audio, Kind::Pdf] {
        let (key, _) = classes.create_subkey(progid(k))?;
        key.set_value("", &type_name(k))?;
        key.set_value("FriendlyTypeName", &type_name(k))?;
        key.create_subkey("DefaultIcon")?.0.set_value("", &icon)?;
        let (open, _) = key.create_subkey(r"shell\open")?;
        open.set_value("FriendlyAppName", &APP)?;
        open.create_subkey("command")?.0.set_value("", &cmd)?;
    }

    let (app, _) = classes.create_subkey(r"Applications\JustView.exe")?;
    app.set_value("FriendlyAppName", &APP)?;
    app.create_subkey("DefaultIcon")?.0.set_value("", &icon)?;
    app.create_subkey(r"shell\open\command")?.0.set_value("", &cmd)?;
    let (supported, _) = app.create_subkey("SupportedTypes")?;

    let (caps, _) = hkcu.create_subkey(r"Software\JustView\Capabilities")?;
    caps.set_value("ApplicationName", &APP)?;
    caps.set_value(
        "ApplicationDescription",
        &tr!("Ультрабыстрый просмотр фото, видео, аудио и документов", "Ultra-fast viewer for photos, videos, audio and documents", "Ультрашвидкий перегляд фото, відео, аудіо та документів"),
    )?;
    caps.set_value("ApplicationIcon", &icon)?;
    let (assoc, _) = caps.create_subkey("FileAssociations")?;

    for (ext, k) in registered_exts() {
        let dot = format!(".{ext}");
        let pid = progid(k);
        let (ek, _) = classes.create_subkey(format!(r"{dot}\OpenWithProgids"))?;
        ek.set_value(pid, &"")?;
        supported.set_value(&dot, &"")?;
        assoc.set_value(&dot, &pid)?;
    }

    hkcu.create_subkey(r"Software\RegisteredApplications")?
        .0
        .set_value(APP, &r"Software\JustView\Capabilities")?;

    notify();
    Ok(())
}

pub fn unregister() -> std::io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = hkcu.open_subkey_with_flags(r"Software\Classes", winreg::enums::KEY_ALL_ACCESS)?;
    for (ext, k) in registered_exts() {
        if let Ok(ek) = classes.open_subkey_with_flags(
            format!(r".{ext}\OpenWithProgids"),
            winreg::enums::KEY_ALL_ACCESS,
        ) {
            let _ = ek.delete_value(progid(k));
        }
    }
    for k in ["JustView.Image", "JustView.Video", "JustView.Audio", "JustView.Pdf", r"Applications\JustView.exe"] {
        let _ = classes.delete_subkey_all(k);
    }
    let _ = hkcu.delete_subkey_all(r"Software\JustView\Capabilities");
    if let Ok(r) = hkcu.open_subkey_with_flags(r"Software\RegisteredApplications", winreg::enums::KEY_ALL_ACCESS) {
        let _ = r.delete_value(APP);
    }
    notify();
    Ok(())
}

/// Зарегистрировано ли приложение именно по текущему пути exe.
pub fn is_registered() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey(r"Software\Classes\JustView.Image\shell\open\command")
        .and_then(|k| k.get_value::<String, _>(""))
        .map(|v| v.contains(&*exe.to_string_lossy()))
        .unwrap_or(false)
}

fn notify() {
    use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

/// Открыть страницу JustView в «Приложения по умолчанию».
pub fn open_default_apps() {
    crate::winutil::open_url("ms-settings:defaultapps?registeredAppUser=JustView");
}
