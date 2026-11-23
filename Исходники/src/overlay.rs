//! Unobtrusive on-screen toast drawn over games (borderless / windowed / fullscreen-optimized).
//!
//! A click-through, never-activated, topmost layered window rendered with Direct2D. It is
//! excluded from screen capture, so it never shows up in replays or recordings.

use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::Instant;

use parking_lot::Mutex;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::engine::{Notice, NoticeKind};

const WM_APP_NOTICE: u32 = WM_APP + 20;
const TIMER_ID: usize = 1;

// Timing (ms) — from DESIGN.md motion tokens.
const FADE_IN: f32 = 180.0;
const HOLD: f32 = 2400.0;
const FADE_OUT: f32 = 280.0;
const SLIDE: f32 = 10.0; // DIP

static QUEUE: OnceLock<Mutex<VecDeque<Notice>>> = OnceLock::new();
static THREAD: OnceLock<u32> = OnceLock::new();

pub fn start() {
    let _ = QUEUE.set(Mutex::new(VecDeque::new()));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("overlay".into())
        .spawn(move || unsafe {
            let mut msg = MSG::default();
            let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
            let _ = tx.send(GetCurrentThreadId());
            run();
        })
        .expect("overlay thread");
    if let Ok(id) = rx.recv() {
        let _ = THREAD.set(id);
    }
}

pub fn show(n: Notice) {
    if let (Some(q), Some(&tid)) = (QUEUE.get(), THREAD.get()) {
        let mut q = q.lock();
        q.clear(); // newest wins: never queue stale toasts
        q.push_back(n);
        unsafe {
            let _ = PostThreadMessageW(tid, WM_APP_NOTICE, WPARAM(0), LPARAM(0));
        }
    }
}

struct Toast {
    notice: Notice,
    started: Instant,
    origin: POINT,
    size: SIZE,
    scale: f32,
}

struct State {
    hwnd: HWND,
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
    toast: Option<Toast>,
    memdc: HDC,
    bitmap: HBITMAP,
    bits: *mut u8,
    bmp_size: SIZE,
}

static mut STATE: Option<State> = None;

#[allow(static_mut_refs)]
unsafe fn state() -> Option<&'static mut State> {
    STATE.as_mut()
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            tick();
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(-1), // HTTRANSPARENT
        WM_MOUSEACTIVATE => LRESULT(3), // MA_NOACTIVATE
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn run() {
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let class = w!("InstantReplayOverlay");
    let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
    RegisterClassW(&wc);
    let ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    let Ok(hwnd) = CreateWindowExW(ex, class, w!(""), WS_POPUP, 0, 0, 1, 1, None, None, hinst, None) else { return };
    // Keep the toast out of our own (and any other) screen capture.
    let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
    let Ok(d2d) = D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) else { return };
    let Ok(dw) = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) else { return };
    let screen = GetDC(None);
    let memdc = CreateCompatibleDC(screen);
    ReleaseDC(None, screen);
    STATE = Some(State { hwnd, d2d, dw, toast: None, memdc, bitmap: HBITMAP::default(), bits: std::ptr::null_mut(), bmp_size: SIZE::default() });

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        if msg.message == WM_APP_NOTICE {
            if let Some(n) = QUEUE.get().and_then(|q| q.lock().pop_back()) {
                begin(n);
            }
            continue;
        }
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

unsafe fn begin(n: Notice) {
    let Some(st) = state() else { return };
    // Place on the monitor of the foreground window (the game), top-right corner.
    let fg = GetForegroundWindow();
    let mon = MonitorFromWindow(fg, MONITOR_DEFAULTTOPRIMARY);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = GetMonitorInfoW(mon, &mut mi);
    let mut dx = 96u32;
    let mut dy = 96u32;
    let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    let scale = dx as f32 / 96.0;
    let w = (340.0 * scale) as i32;
    let h = (if n.detail.is_empty() { 52.0 } else { 68.0 } * scale) as i32;
    let margin = (24.0 * scale) as i32;
    let r = mi.rcMonitor;
    // Below the performance counter when it takes the same corner.
    let below_hud = if crate::hud::in_top_right() { (38.0 * scale) as i32 } else { 0 };
    let origin = POINT { x: r.right - w - margin, y: r.top + margin + below_hud };
    st.toast = Some(Toast { notice: n, started: Instant::now(), origin, size: SIZE { cx: w, cy: h }, scale });
    if render().is_ok() {
        let _ = SetWindowPos(st.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        SetTimer(st.hwnd, TIMER_ID, 15, None);
        tick();
    }
}

fn ease_out(t: f32) -> f32 {
    // cubic-bezier(0.28, 0.11, 0.32, 1) approximated by an ease-out cubic.
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

unsafe fn tick() {
    let Some(st) = state() else { return };
    let Some(t) = &st.toast else {
        let _ = KillTimer(st.hwnd, TIMER_ID);
        return;
    };
    let ms = t.started.elapsed().as_secs_f32() * 1000.0;
    let (alpha, offset) = if ms < FADE_IN {
        let k = ease_out(ms / FADE_IN);
        (k, (1.0 - k) * -SLIDE)
    } else if ms < FADE_IN + HOLD {
        (1.0, 0.0)
    } else if ms < FADE_IN + HOLD + FADE_OUT {
        let k = (ms - FADE_IN - HOLD) / FADE_OUT;
        (1.0 - k, 0.0)
    } else {
        let _ = KillTimer(st.hwnd, TIMER_ID);
        let _ = ShowWindow(st.hwnd, SW_HIDE);
        st.toast = None;
        return;
    };
    let pos = POINT { x: t.origin.x, y: t.origin.y + (offset * t.scale) as i32 };
    let size = t.size;
    let src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: (alpha * 255.0) as u8, AlphaFormat: AC_SRC_ALPHA as u8 };
    let screen = GetDC(None);
    let _ = UpdateLayeredWindow(st.hwnd, screen, Some(&pos), Some(&size), st.memdc, Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
    ReleaseDC(None, screen);
}

fn color(hex: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a,
    }
}

unsafe fn render() -> windows::core::Result<()> {
    let Some(st) = state() else { return Ok(()) };
    let Some(t) = &st.toast else { return Ok(()) };
    let (w, h, s) = (t.size.cx, t.size.cy, t.scale);

    if st.bmp_size.cx != w || st.bmp_size.cy != h || st.bitmap.is_invalid() {
        if !st.bitmap.is_invalid() {
            let _ = DeleteObject(st.bitmap);
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        st.bitmap = CreateDIBSection(st.memdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
        st.bits = bits as *mut u8;
        SelectObject(st.memdc, st.bitmap);
        st.bmp_size = SIZE { cx: w, cy: h };
    }

    let props = D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
        usage: D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    };
    let rt = st.d2d.CreateDCRenderTarget(&props)?;
    let rect = RECT { left: 0, top: 0, right: w, bottom: h };
    rt.BindDC(st.memdc, &rect)?;
    rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
    rt.BeginDraw();
    rt.Clear(Some(&color(0, 0.0)));

    let radius = 16.0 * s;
    let inset = 0.5 * s;
    let rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F { left: inset, top: inset, right: w as f32 - inset, bottom: h as f32 - inset },
        radiusX: radius,
        radiusY: radius,
    };
    // surface-raised at 96% + hairline (DESIGN.md: overlay toast)
    let bg = rt.CreateSolidColorBrush(&color(0x1d1d1f, 0.96), None)?;
    let line = rt.CreateSolidColorBrush(&color(0xffffff, 0.10), None)?;
    rt.FillRoundedRectangle(&rr, &bg);
    rt.DrawRoundedRectangle(&rr, &line, 1.0 * s, None);

    // Status mark: 10 px dot, colour carries meaning together with the title text.
    let (mark, mark_hex) = match t.notice.kind {
        NoticeKind::RecordingStarted => (Mark::Dot, 0xff453a),
        NoticeKind::ReplaySaved | NoticeKind::RecordingSaved => (Mark::Check, 0x30d158),
        NoticeKind::ReplayOn => (Mark::Ring, 0xf5f5f7),
        NoticeKind::ReplayOff => (Mark::Ring, 0x86868b),
        NoticeKind::Error => (Mark::Bang, 0xff9f0a),
    };
    let cx = 26.0 * s;
    let cy = h as f32 / 2.0;
    let mb = rt.CreateSolidColorBrush(&color(mark_hex, 1.0), None)?;
    match mark {
        Mark::Dot => {
            let e = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: 6.0 * s, radiusY: 6.0 * s };
            rt.FillEllipse(&e, &mb);
        }
        Mark::Ring => {
            let e = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: 7.0 * s, radiusY: 7.0 * s };
            rt.DrawEllipse(&e, &mb, 2.0 * s, None);
            let d = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: 3.0 * s, radiusY: 3.0 * s };
            rt.FillEllipse(&d, &mb);
        }
        Mark::Check => {
            let e = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: 9.0 * s, radiusY: 9.0 * s };
            rt.FillEllipse(&e, &mb);
            let ink = rt.CreateSolidColorBrush(&color(0x000000, 1.0), None)?;
            let p1 = D2D_POINT_2F { x: cx - 4.0 * s, y: cy + 0.2 * s };
            let p2 = D2D_POINT_2F { x: cx - 1.2 * s, y: cy + 3.0 * s };
            let p3 = D2D_POINT_2F { x: cx + 4.4 * s, y: cy - 3.2 * s };
            rt.DrawLine(p1, p2, &ink, 2.0 * s, None);
            rt.DrawLine(p2, p3, &ink, 2.0 * s, None);
        }
        Mark::Bang => {
            let e = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: 9.0 * s, radiusY: 9.0 * s };
            rt.FillEllipse(&e, &mb);
            let ink = rt.CreateSolidColorBrush(&color(0x000000, 1.0), None)?;
            rt.DrawLine(D2D_POINT_2F { x: cx, y: cy - 4.5 * s }, D2D_POINT_2F { x: cx, y: cy + 1.0 * s }, &ink, 2.0 * s, None);
            let d = D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy + 4.0 * s }, radiusX: 1.2 * s, radiusY: 1.2 * s };
            rt.FillEllipse(&d, &ink);
        }
    }

    let fam = w!("Segoe UI Variable Text");
    let title_fmt = st.dw.CreateTextFormat(fam, None, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 15.0 * s, w!("ru-ru"))?;
    let body_fmt = st.dw.CreateTextFormat(fam, None, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 13.0 * s, w!("ru-ru"))?;
    for f in [&title_fmt, &body_fmt] {
        let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
    }
    let text_brush = rt.CreateSolidColorBrush(&color(0xf5f5f7, 1.0), None)?;
    let sec_brush = rt.CreateSolidColorBrush(&color(0xa1a1a6, 1.0), None)?;
    let left = 48.0 * s;
    let right = w as f32 - 16.0 * s;
    let title: Vec<u16> = t.notice.title.encode_utf16().collect();
    if t.notice.detail.is_empty() {
        let r = D2D_RECT_F { left, top: 0.0, right, bottom: h as f32 };
        rt.DrawText(&title, &title_fmt, &r, &text_brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
    } else {
        let detail: Vec<u16> = t.notice.detail.encode_utf16().collect();
        let r1 = D2D_RECT_F { left, top: 12.0 * s, right, bottom: 36.0 * s };
        let r2 = D2D_RECT_F { left, top: 34.0 * s, right, bottom: 56.0 * s };
        rt.DrawText(&title, &title_fmt, &r1, &text_brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        rt.DrawText(&detail, &body_fmt, &r2, &sec_brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
    }
    rt.EndDraw(None, None)?;
    let _ = PCWSTR::null();
    Ok(())
}

enum Mark {
    Dot,
    Ring,
    Check,
    Bang,
}
