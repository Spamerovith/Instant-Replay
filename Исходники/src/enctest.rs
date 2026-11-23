//! Encoder bench (`--encode-test <video> <out dir> [frames]`): the same video goes through our
//! hardware encoder with several settings; each result's size and SSIM against the source are
//! logged (`ENCTEST ...` in log.txt). Used to tune size against quality on identical frames.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::{MFStartup, MFSTARTUP_FULL, MF_VERSION};

use crate::config::{Codec, Preset, RateControl};
use crate::encoder::video::{EncodedFrame, VideoEncoder, VideoSettings};
use crate::gpu::{Converter, Gpu};
use crate::player::ffmpeg;

struct Variant {
    name: &'static str,
    rc: RateControl,
    mbps: u32,
    quality: u32,
    preset: Preset,
    low_latency: bool,
    gop_secs: u32,
    peak: u32,
}

pub fn run(input: &Path, out_dir: &Path, frames: usize, codec: Codec, base_mbps: u32) {
    unsafe {
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    }
    let _ = std::fs::create_dir_all(out_dir);
    let Some(ff) = ffmpeg::ffmpeg() else {
        log::error!("ENCTEST no ffmpeg");
        return;
    };
    let info = match ffmpeg::probe(input) {
        Ok(i) => i,
        Err(e) => {
            log::error!("ENCTEST probe: {e}");
            return;
        }
    };
    let (w, h) = (info.width & !1, info.height & !1);
    let fps = info.fps.round().max(1.0) as u32;
    let m = base_mbps;
    let v = |name, rc, mbps, quality, peak| Variant { name, rc, mbps, quality, preset: Preset::Balanced, low_latency: true, gop_secs: 2, peak };
    let variants = [
        v("vbr", RateControl::Vbr, m, 80, 0),
        v("auto-compact", RateControl::Cqp, m, crate::config::BitrateQuality::Compact.cq_level(), 0),
        v("auto-standard", RateControl::Cqp, m, crate::config::BitrateQuality::Standard.cq_level(), 0),
        v("auto-high", RateControl::Cqp, m, crate::config::BitrateQuality::High.cq_level(), 0),
    ];
    let n_src = frames;
    log::info!("ENCTEST source {}x{} @ {fps} fps, {frames} frames, base {m} Mbps, {:?}", w, h, codec);
    let ref_path = out_dir.join("ref.y4m");
    let _ = ffmpeg::hidden_command(ff)
        .args(["-v", "error", "-y", "-i"])
        .arg(input)
        .args(["-frames:v", &n_src.to_string(), "-vf", &format!("scale={w}:{h},format=yuv420p")])
        .arg(&ref_path)
        .status();

    for v in &variants {
        let path = out_dir.join(format!("{}.{}", v.name, if codec == Codec::Av1 { "ivf" } else if codec == Codec::Hevc { "hevc" } else { "h264" }));
        match encode(ff, input, frames, w, h, fps, codec, v, &path) {
            Ok(n) => {
                let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let ssim = ssim(ff, &path, &ref_path);
                let mbps = bytes as f64 * 8.0 / (n as f64 / fps as f64) / 1e6;
                log::info!("ENCTEST {:<16} frames={n} bytes={bytes} avg={mbps:.2} Mbps ssim={ssim}", v.name);
            }
            Err(e) => log::error!("ENCTEST {} failed: {e}", v.name),
        }
    }
    log::info!("ENCTEST DONE");
    log::logger().flush();
}

fn ssim(ff: &Path, enc: &Path, reference: &Path) -> String {
    let o = ffmpeg::hidden_command(ff)
        .args(["-v", "info", "-i"])
        .arg(enc)
        .arg("-i")
        .arg(reference)
        .args(["-lavfi", "[0:v]format=yuv420p[a];[a][1:v]ssim", "-f", "null", "-"])
        .output();
    match o {
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            err.lines().filter(|l| l.contains("SSIM")).last().map(|l| l.split("All:").nth(1).unwrap_or(l).trim().to_string()).unwrap_or_else(|| "?".into())
        }
        Err(e) => e.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
fn encode(ff: &Path, input: &Path, frames: usize, w: u32, h: u32, fps: u32, codec: Codec, v: &Variant, path: &PathBuf) -> Result<usize, String> {
    let mut child = ffmpeg::hidden_command(ff)
        .args(["-v", "error", "-i"])
        .arg(input)
        .args(["-frames:v", &frames.to_string(), "-vf", &format!("scale={w}:{h}"), "-f", "rawvideo", "-pix_fmt", "bgra", "-"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut pipe = child.stdout.take().unwrap();
    let mut frame = vec![0u8; (w * h * 4) as usize];
    let gpu = Gpu::new().map_err(|e| e.to_string())?;
    let s = VideoSettings { codec, width: w, height: h, fps, rate_control: v.rc, bitrate_mbps: v.mbps, quality: v.quality, preset: v.preset, low_latency: v.low_latency, gop_secs: v.gop_secs, peak_mbps: v.peak };
    let mut enc = VideoEncoder::new(&gpu, &s).map_err(|e| e.to_string())?;
    let mut conv = Converter::new(&gpu, w, h).map_err(|e| e.to_string())?;
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex = None;
    unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut tex)).map_err(|e| e.to_string())? };
    let tex = tex.unwrap();
    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let ivf = codec == Codec::Av1;
    if ivf {
        let mut hdr = Vec::new();
        hdr.extend_from_slice(b"DKIF");
        hdr.extend_from_slice(&0u16.to_le_bytes());
        hdr.extend_from_slice(&32u16.to_le_bytes());
        hdr.extend_from_slice(b"AV01");
        hdr.extend_from_slice(&(w as u16).to_le_bytes());
        hdr.extend_from_slice(&(h as u16).to_le_bytes());
        hdr.extend_from_slice(&fps.to_le_bytes());
        hdr.extend_from_slice(&1u32.to_le_bytes());
        hdr.extend_from_slice(&(frames as u32).to_le_bytes());
        hdr.extend_from_slice(&0u32.to_le_bytes());
        file.write_all(&hdr).map_err(|e| e.to_string())?;
    }
    let mut written = 0usize;
    let write = |frames: &mut Vec<EncodedFrame>, file: &mut std::fs::File, written: &mut usize| -> std::io::Result<()> {
        for f in frames.drain(..) {
            if ivf {
                file.write_all(&(f.data.len() as u32).to_le_bytes())?;
                file.write_all(&(*written as u64).to_le_bytes())?;
            }
            file.write_all(&f.data)?;
            *written += 1;
        }
        Ok(())
    };
    let period = 10_000_000 / fps as i64;
    let mut outs = Vec::new();
    let mut i = 0usize;
    while pipe.read_exact(&mut frame).is_ok() {
        unsafe { gpu.context.UpdateSubresource(&tex, 0, None, frame.as_ptr() as _, w * 4, 0) };
        let started = Instant::now();
        loop {
            enc.poll(&mut outs).map_err(|e| e.to_string())?;
            if enc.can_accept() {
                if let Some((sample, dst, sub)) = enc.alloc_surface().map_err(|e| e.to_string())? {
                    conv.convert(&tex, &dst, sub).map_err(|e| e.to_string())?;
                    enc.encode(&sample, i as i64 * period, &mut outs).map_err(|e| e.to_string())?;
                    break;
                }
            }
            if started.elapsed() > Duration::from_secs(5) {
                return Err("encoder stalled".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        write(&mut outs, &mut file, &mut written).map_err(|e| e.to_string())?;
        i += 1;
    }
    let _ = child.wait();
    // Collect what is still inside the encoder.
    let started = Instant::now();
    while written < i && started.elapsed() < Duration::from_secs(3) {
        enc.poll(&mut outs).map_err(|e| e.to_string())?;
        write(&mut outs, &mut file, &mut written).map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(written)
}
