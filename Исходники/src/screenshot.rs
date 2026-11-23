//! Screenshots: one frame of the capture source through Windows Graphics Capture (the same way
//! clips are captured, nothing touches the game), saved as PNG.

use std::path::Path;
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

use crate::capture::{sources, Capture};
use crate::config::CaptureSource;
use crate::gpu::Gpu;

/// Grabs the source and writes `path`. Returns the picture size.
pub fn take(source: &CaptureSource, path: &Path) -> Result<(u32, u32), String> {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    let gpu = Gpu::new().map_err(|e| e.to_string())?;
    let (target, _) = sources::resolve(source).ok_or(tr!("Источник захвата не найден", "Capture source not found", "Джерело захоплення не знайдено"))?;
    let mut cap = Capture::new(&gpu.device, &target, false).map_err(|e| e.to_string())?;
    let mut tex: Option<ID3D11Texture2D> = None;
    let started = Instant::now();
    loop {
        if cap.poll(&gpu.context, &gpu.device, &mut tex).map_err(|e| e.to_string())?.is_some() {
            break;
        }
        if started.elapsed() > Duration::from_millis(1500) {
            return Err(tr!("Нет кадра (окно свёрнуто?)", "No frame (is the window minimized?)", "Немає кадру (вікно згорнуто?)").into());
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    drop(cap);
    let tex = tex.ok_or("no texture")?;
    let (w, h, rgba) = read_back(&gpu, &tex).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    write_png(path, w, h, &rgba)?;
    Ok((w, h))
}

/// Copies a BGRA texture to memory as RGBA (alpha forced opaque).
fn read_back(gpu: &Gpu, tex: &ID3D11Texture2D) -> windows::core::Result<(u32, u32, Vec<u8>)> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { tex.GetDesc(&mut desc) };
    let staging_desc = D3D11_TEXTURE2D_DESC {
        Width: desc.Width,
        Height: desc.Height,
        MipLevels: 1,
        ArraySize: 1,
        Format: desc.Format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut staging = None;
    unsafe { gpu.device.CreateTexture2D(&staging_desc, None, Some(&mut staging))? };
    let staging = staging.unwrap();
    let (w, h) = (desc.Width, desc.Height);
    let mut out = vec![0u8; (w * h * 4) as usize];
    unsafe {
        gpu.context.CopyResource(&staging, tex);
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        gpu.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
        let pitch = m.RowPitch as usize;
        let src = std::slice::from_raw_parts(m.pData as *const u8, pitch * h as usize);
        for y in 0..h as usize {
            bgra_row_to_rgba(&src[y * pitch..y * pitch + w as usize * 4], &mut out[y * w as usize * 4..(y + 1) * w as usize * 4]);
        }
        gpu.context.Unmap(&staging, 0);
    }
    Ok((w, h, out))
}

fn bgra_row_to_rgba(src: &[u8], dst: &mut [u8]) {
    for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
        d[0] = s[2];
        d[1] = s[1];
        d[2] = s[0];
        d[3] = 255;
    }
}

pub fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut wr = enc.write_header().map_err(|e| e.to_string())?;
    wr.write_image_data(rgba).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_becomes_opaque_rgba() {
        let mut d = [0u8; 8];
        bgra_row_to_rgba(&[1, 2, 3, 0, 10, 20, 30, 7], &mut d);
        assert_eq!(d, [3, 2, 1, 255, 30, 20, 10, 255]);
    }

    #[test]
    fn png_is_written() {
        let p = std::env::temp_dir().join(format!("ir-shot-{}.png", std::process::id()));
        write_png(&p, 2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]).unwrap();
        let b = std::fs::read(&p).unwrap();
        assert_eq!(&b[1..4], b"PNG");
        let _ = std::fs::remove_file(&p);
    }
}
