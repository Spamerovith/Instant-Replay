//! Procedurally drawn app / tray icon: a ring (the replay loop) around a dot (the lens).

use windows::Win32::Graphics::Gdi::{CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

#[derive(Clone, Copy, PartialEq)]
pub enum IconState {
    Idle,
    Active,
    Recording,
}

/// Returns straight-alpha RGBA pixels.
pub fn rgba(size: u32, state: IconState) -> Vec<u8> {
    let (ring, dot): ([u8; 3], [u8; 3]) = match state {
        IconState::Idle => ([0x86, 0x86, 0x8b], [0x86, 0x86, 0x8b]),
        IconState::Active => ([0xf5, 0xf5, 0xf7], [0xf5, 0xf5, 0xf7]),
        IconState::Recording => ([0xf5, 0xf5, 0xf7], [0xff, 0x45, 0x3a]),
    };
    let s = size as f32;
    let c = s / 2.0;
    let r_out = s * 0.46;
    let r_in = s * 0.36;
    let r_dot = s * 0.2;
    let mut px = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            // 4×4 supersampling for clean edges at 16 px.
            let mut ring_cov = 0.0f32;
            let mut dot_cov = 0.0f32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = x as f32 + (sx as f32 + 0.5) / 4.0 - c;
                    let fy = y as f32 + (sy as f32 + 0.5) / 4.0 - c;
                    let d = (fx * fx + fy * fy).sqrt();
                    if d <= r_out && d >= r_in {
                        ring_cov += 1.0 / 16.0;
                    }
                    if d <= r_dot {
                        dot_cov += 1.0 / 16.0;
                    }
                }
            }
            let i = ((y * size + x) * 4) as usize;
            let (col, a) = if dot_cov > 0.0 { (dot, dot_cov) } else { (ring, ring_cov) };
            px[i] = col[0];
            px[i + 1] = col[1];
            px[i + 2] = col[2];
            px[i + 3] = (a * 255.0).round() as u8;
        }
    }
    px
}

pub fn hicon(size: u32, state: IconState) -> HICON {
    let px = rgba(size, state);
    unsafe {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let Ok(color) = CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            return HICON::default();
        };
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, px.len());
        for i in (0..px.len()).step_by(4) {
            dst[i] = px[i + 2];
            dst[i + 1] = px[i + 1];
            dst[i + 2] = px[i];
            dst[i + 3] = px[i + 3];
        }
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, None);
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(color);
        let _ = DeleteObject(mask);
        icon
    }
}
