//! Обёртки над Win32: заголовок окна, корзина, Проводник, WIC, миниатюры оболочки.

use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use windows::Win32::Foundation::{GENERIC_READ, HWND, SIZE};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HGDIOBJ, ReleaseDC,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICImagingFactory,
    WICConvertBitmapSource, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, IShellItemImageFactory,
    SHCreateItemFromParsingName, SHFILEOPSTRUCTW, SHFileOperationW, SIIGBF_BIGGERSIZEOK,
    StrCmpLogicalW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETCLIENTAREAANIMATION, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SystemParametersInfoW,
};
use windows::core::{BOOL, PCWSTR, w};

pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

pub fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

/// Чёрный заголовок окна, сливающийся с холстом (Windows 11; на 10 — тёмная тема).
pub fn style_titlebar(hwnd: isize) {
    unsafe {
        let hwnd = HWND(hwnd as *mut _);
        let dark: BOOL = true.into();
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const _,
            std::mem::size_of::<BOOL>() as u32,
        );
        let black: u32 = 0x0000_0000; // COLORREF 0x00BBGGRR
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &black as *const _ as *const _, 4);
        let border: u32 = 0x0017_1616;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as *const _, 4);
        let text: u32 = 0x00a6_a1a1; // text-secondary
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TEXT_COLOR, &text as *const _ as *const _, 4);
    }
}

pub fn animations_disabled() -> bool {
    let mut on: BOOL = true.into();
    unsafe {
        let ok = SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut on as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        ok.is_ok() && !on.as_bool()
    }
}

/// Размер рабочей области основного монитора (в физических пикселях).
pub fn work_area() -> Option<(f32, f32)> {
    let mut r = windows::Win32::Foundation::RECT::default();
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut r as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .ok()?;
    }
    Some(((r.right - r.left) as f32, (r.bottom - r.top) as f32))
}

/// Сортировка как в Проводнике («file2» < «file10»).
pub fn natural_cmp(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> std::cmp::Ordering {
    let wa = wide(a);
    let wb = wide(b);
    let r = unsafe { StrCmpLogicalW(PCWSTR(wa.as_ptr()), PCWSTR(wb.as_ptr())) };
    r.cmp(&0)
}

/// Путь в виде, который понимает SHFileOperation: только обратные слэши и без префикса `\\?\`
/// (иначе она отвечает кодом 124, «неверный путь»).
fn shell_path(path: &Path) -> Vec<u16> {
    let s = path.to_string_lossy().replace('/', "\\");
    let s = match s.strip_prefix("\\\\?\\UNC\\") {
        Some(rest) => format!("\\\\{rest}"),
        None => s.strip_prefix("\\\\?\\").map(str::to_string).unwrap_or(s),
    };
    s.encode_utf16().collect()
}

/// Переместить в Корзину (восстановимо).
pub fn recycle(path: &Path) -> Result<(), String> {
    let mut from: Vec<u16> = shell_path(path);
    from.push(0);
    from.push(0);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        ..Default::default()
    };
    let r = unsafe { SHFileOperationW(&mut op) };
    if r == 0 && !op.fAnyOperationsAborted.as_bool() {
        Ok(())
    } else {
        Err(trf!(["код ошибки {r}", "error code {r}", "код помилки {r}"]))
    }
}

pub fn show_in_explorer(path: &Path) {
    let mut arg = std::ffi::OsString::from("/select,\"");
    arg.push(path.as_os_str());
    arg.push("\"");
    let _ = Command::new("explorer.exe").raw_arg(arg).spawn();
}

pub fn open_with_dialog(path: &Path) {
    let mut arg = std::ffi::OsString::from("shell32.dll,OpenAs_RunDLL ");
    arg.push(path.as_os_str());
    let _ = Command::new("rundll32.exe").raw_arg(arg).spawn();
}

pub fn open_url(url: &str) {
    let _ = Command::new("explorer.exe").arg(url).spawn();
}

fn com_init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

pub struct Rgba {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

/// Декодирование через Windows Imaging Component (HEIC/AVIF/RAW/JXR и др. при наличии кодеков).
pub fn wic_decode(path: &Path) -> Result<(Rgba, u16), String> {
    com_init();
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| e.message())?;
        let wpath = wide(path.as_os_str());
        let decoder = factory
            .CreateDecoderFromFilename(
                PCWSTR(wpath.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
            .map_err(|e| e.message())?;
        let frame = decoder.GetFrame(0).map_err(|e| e.message())?;
        let mut orientation = 1u16;
        if let Ok(reader) = frame.GetMetadataQueryReader() {
            let mut pv = windows::Win32::System::Com::StructuredStorage::PROPVARIANT::default();
            if reader.GetMetadataByName(w!("System.Photo.Orientation"), &mut pv).is_ok() {
                let inner = &pv.Anonymous.Anonymous;
                if inner.vt == windows::Win32::System::Variant::VT_UI2 {
                    orientation = inner.Anonymous.uiVal;
                }
            }
        }
        let src = WICConvertBitmapSource(&GUID_WICPixelFormat32bppRGBA, &frame)
            .map_err(|e| e.message())?;
        let (mut w, mut h) = (0u32, 0u32);
        src.GetSize(&mut w, &mut h).map_err(|e| e.message())?;
        if w == 0 || h == 0 {
            return Err(tr!("пустое изображение", "empty image", "порожнє зображення").into());
        }
        let stride = w * 4;
        let mut px = vec![0u8; (stride * h) as usize];
        src.CopyPixels(std::ptr::null(), stride, &mut px)
            .map_err(|e| e.message())?;
        Ok((Rgba { w, h, px }, orientation))
    }
}

/// Миниатюра оболочки Windows — превью для любых файлов, которые умеет Проводник.
pub fn shell_thumbnail(path: &Path, size: i32) -> Option<Rgba> {
    com_init();
    unsafe {
        let wpath = wide(path.as_os_str());
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR(wpath.as_ptr()), None).ok()?;
        let hbmp = factory
            .GetImage(SIZE { cx: size, cy: size }, SIIGBF_BIGGERSIZEOK)
            .ok()?;
        let mut bm = BITMAP::default();
        let got = GetObjectW(
            HGDIOBJ(hbmp.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut _),
        );
        if got == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 {
            let _ = DeleteObject(HGDIOBJ(hbmp.0));
            return None;
        }
        let (w, h) = (bm.bmWidth as u32, bm.bmHeight as u32);
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(None);
        let lines = GetDIBits(
            dc,
            hbmp,
            0,
            h,
            Some(px.as_mut_ptr() as *mut _),
            &mut info,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, dc);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        if lines == 0 {
            return None;
        }
        // BGRA (премультиплицированный) → RGBA
        let any_alpha = px.chunks_exact(4).any(|p| p[3] != 0);
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2);
            if !any_alpha {
                p[3] = 255;
            } else if p[3] > 0 && p[3] < 255 {
                let a = p[3] as u32;
                for c in &mut p[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        Some(Rgba { w, h, px })
    }
}

/// Локальная дата и время «21.11.2026, 21:46».
pub fn fmt_local(t: std::time::SystemTime) -> String {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) else { return String::new() };
    let ticks = d.as_nanos() as u64 / 100 + 116_444_736_000_000_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    unsafe {
        if FileTimeToSystemTime(&ft, &mut utc).is_err() {
            return String::new();
        }
        if SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_err() {
            local = utc;
        }
    }
    format!(
        "{:02}.{:02}.{}, {:02}:{:02}",
        local.wDay, local.wMonth, local.wYear, local.wHour, local.wMinute
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("justview-test-{}-{tag}.txt", std::process::id()));
        std::fs::write(&p, b"x").unwrap();
        p
    }

    #[test]
    fn recycle_accepts_plain_forward_slash_and_extended_paths() {
        let p = temp_file("plain");
        recycle(&p).unwrap();
        assert!(!p.exists());

        let p = temp_file("slash");
        recycle(Path::new(&p.to_string_lossy().replace('\\', "/"))).unwrap();
        assert!(!p.exists());

        let p = temp_file("ext");
        let ext = std::fs::canonicalize(&p).unwrap(); // gives the \?\ form
        assert!(ext.to_string_lossy().starts_with(r"\\?\"));
        recycle(&ext).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn shell_path_strips_extended_prefix() {
        let s = String::from_utf16(&shell_path(Path::new(r"\\?\C:/a/b.mp4"))).unwrap();
        assert_eq!(s, r"C:\a\b.mp4");
        let s = String::from_utf16(&shell_path(Path::new(r"\\?\UNC\srv\share\x.mp4"))).unwrap();
        assert_eq!(s, r"\\srv\share\x.mp4");
    }
}
