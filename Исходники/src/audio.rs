//! System audio (WASAPI loopback) and microphone capture, mixed on a QPC timeline that is
//! shared with video, then AAC-encoded per track.
//!
//! Every captured packet is placed on the timeline by its QPC timestamp, gaps are filled with
//! silence and the mixer consumes the timeline with a fixed lag, so tracks stay in sync with
//! video for hours regardless of when applications start or stop playing sound.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use windows::core::{Result, PCWSTR};
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::{SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;

use crate::encoder::aac::AacEncoder;
use crate::media::{bitstream, AudioInfo, Packet, AAC_FRAME, AUDIO_RATE};
use crate::util::{now_hns, HNS};

const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// Mixer lag behind real time (samples). Covers WASAPI packet jitter.
const LAG: i64 = AUDIO_RATE as i64 / 10;
/// Timestamp tolerance before inserting silence / dropping overlap (10 ms).
const TOL: i64 = AUDIO_RATE as i64 / 100;

#[derive(Clone, Debug)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}

pub fn list_microphones() -> Vec<AudioDevice> {
    let mut out = Vec::new();
    unsafe {
        let Ok(en) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else {
            return out;
        };
        let Ok(col) = en.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) else { return out };
        let n = col.GetCount().unwrap_or(0);
        for i in 0..n {
            let Ok(dev) = col.Item(i) else { continue };
            let Ok(id) = dev.GetId() else { continue };
            let id_s = id.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(id.0 as _));
            let name = device_name(&dev).unwrap_or_else(|| id_s.clone());
            out.push(AudioDevice { id: id_s, name });
        }
    }
    out
}

unsafe fn device_name(dev: &IMMDevice) -> Option<String> {
    let store = dev.OpenPropertyStore(STGM_READ).ok()?;
    let v = store.GetValue(&windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName).ok()?;
    let p = PropVariantToStringAlloc(&v).ok()?;
    let s = p.to_string().ok();
    windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
    s
}

#[derive(Clone, PartialEq)]
enum SourceKind {
    Loopback,
    Mic(Option<String>),
}

struct WasapiSource {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    device_id: String,
}

impl WasapiSource {
    unsafe fn open(kind: &SourceKind) -> Result<Self> {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = match kind {
            SourceKind::Loopback => en.GetDefaultAudioEndpoint(eRender, eConsole)?,
            SourceKind::Mic(Some(id)) => {
                let w = crate::util::wide(id);
                match en.GetDevice(PCWSTR(w.as_ptr())) {
                    Ok(d) => d,
                    Err(_) => en.GetDefaultAudioEndpoint(eCapture, eConsole)?,
                }
            }
            SourceKind::Mic(None) => en.GetDefaultAudioEndpoint(eCapture, eConsole)?,
        };
        let id = dev.GetId()?;
        let device_id = id.to_string().unwrap_or_default();
        windows::Win32::System::Com::CoTaskMemFree(Some(id.0 as _));
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
        let fmt = WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_EXTENSIBLE,
                nChannels: 2,
                nSamplesPerSec: AUDIO_RATE,
                nAvgBytesPerSec: AUDIO_RATE * 8,
                nBlockAlign: 8,
                wBitsPerSample: 32,
                cbSize: 22,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: 32 },
            dwChannelMask: SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT,
            SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
        };
        let _ = WAVE_FORMAT_IEEE_FLOAT;
        let mut flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        if *kind == SourceKind::Loopback {
            flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
        }
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 2_000_000, 0, &fmt as *const _ as *const WAVEFORMATEX, None)?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        Ok(WasapiSource { client, capture, device_id })
    }

    /// Reads all pending packets into the timeline.
    unsafe fn read(&mut self, tl: &mut Timeline, t0: i64) -> Result<()> {
        loop {
            let n = self.capture.GetNextPacketSize()?;
            if n == 0 {
                return Ok(());
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            let mut qpc = 0u64;
            self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))?;
            let idx = if qpc != 0 && flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0 {
                ((qpc as i64 - t0) as i128 * AUDIO_RATE as i128 / HNS as i128) as i64
            } else {
                tl.end()
            };
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                tl.push_silence(idx, frames as usize);
            } else {
                let s = std::slice::from_raw_parts(data as *const f32, frames as usize * 2);
                tl.push(idx, s);
            }
            self.capture.ReleaseBuffer(frames)?;
        }
    }
}

impl Drop for WasapiSource {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

/// Interleaved stereo samples anchored to absolute frame indices on the session timeline.
struct Timeline {
    buf: VecDeque<f32>,
    start: i64,
}

impl Timeline {
    fn new() -> Self {
        Timeline { buf: VecDeque::with_capacity(AUDIO_RATE as usize), start: 0 }
    }
    fn end(&self) -> i64 {
        self.start + (self.buf.len() / 2) as i64
    }
    /// Positions incoming data at `idx`; returns how many leading frames must be dropped.
    fn align(&mut self, idx: i64) -> i64 {
        if self.buf.is_empty() {
            // Nothing pending: place the packet exactly where its timestamp says.
            if idx >= self.start {
                self.start = idx;
                return 0;
            }
            return self.start - idx;
        }
        let delta = idx - self.end();
        if delta > TOL {
            for _ in 0..delta * 2 {
                self.buf.push_back(0.0);
            }
            0
        } else if delta < -TOL {
            -delta // frames to drop from the incoming data
        } else {
            0
        }
    }
    fn push(&mut self, idx: i64, s: &[f32]) {
        let drop = self.align(idx) as usize;
        if drop * 2 >= s.len() {
            return;
        }
        self.buf.extend(&s[drop * 2..]);
        self.cap();
    }
    fn push_silence(&mut self, idx: i64, frames: usize) {
        let drop = self.align(idx) as usize;
        if drop >= frames {
            return;
        }
        for _ in 0..(frames - drop) * 2 {
            self.buf.push_back(0.0);
        }
        self.cap();
    }
    fn cap(&mut self) {
        // Never hold more than 2 s ahead (protects against clock anomalies).
        let max = AUDIO_RATE as usize * 4;
        while self.buf.len() > max {
            self.buf.pop_front();
            self.buf.pop_front();
            self.start += 1;
        }
    }
    /// Fills `out` (interleaved, `frames` frames) with timeline range [from, from+frames).
    fn take(&mut self, from: i64, out: &mut [f32]) {
        let frames = out.len() / 2;
        while self.start < from && !self.buf.is_empty() {
            self.buf.pop_front();
            self.buf.pop_front();
            self.start += 1;
        }
        if self.buf.is_empty() && self.start < from {
            self.start = from;
        }
        let lead = (self.start - from).clamp(0, frames as i64) as usize; // silence before data
        for v in out.iter_mut().take(lead * 2) {
            *v = 0.0;
        }
        let mut i = lead * 2;
        while i < out.len() {
            if self.start >= from + frames as i64 {
                break;
            }
            match (self.buf.pop_front(), self.buf.pop_front()) {
                (Some(l), Some(r)) => {
                    out[i] = l;
                    out[i + 1] = r;
                    self.start += 1;
                }
                _ => break,
            }
            i += 2;
        }
        for v in out.iter_mut().skip(i) {
            *v = 0.0;
        }
        if self.buf.is_empty() && self.start < from + frames as i64 {
            self.start = from + frames as i64;
        }
    }
}

pub struct AudioConfig {
    pub system: bool,
    pub mic: bool,
    pub mic_device: Option<String>,
    pub separate: bool,
    pub bitrate_kbps: u32,
    pub volumes: Arc<parking_lot::Mutex<(f32, f32)>>,
}

#[derive(Clone, Copy, PartialEq)]
enum TrackMix {
    Mix,
    System,
    Mic,
}

/// Describes the audio tracks a config produces (also used by the muxer).
pub fn track_layout(system: bool, mic: bool, separate: bool) -> Vec<&'static str> {
    match (system, mic) {
        (false, false) => vec![],
        (true, false) => vec!["System audio"],
        (false, true) => vec!["Microphone"],
        (true, true) if separate => vec!["System audio", "Microphone", "Mix"],
        (true, true) => vec!["Mix"],
    }
}

pub fn track_infos(cfg: &AudioConfig) -> Vec<AudioInfo> {
    let bitrate = match cfg.bitrate_kbps {
        96 => 96_000,
        128 => 128_000,
        160 => 160_000,
        _ => 192_000,
    };
    track_layout(cfg.system, cfg.mic, cfg.separate)
        .into_iter()
        .map(|name| AudioInfo {
            sample_rate: AUDIO_RATE,
            channels: 2,
            bitrate,
            asc: bitstream::aac_asc(AUDIO_RATE, 2),
            name,
        })
        .collect()
}

pub enum AudioOut {
    Packet(usize, Packet),
    Error(String),
}

/// Runs the audio pipeline until `stop` is set.
pub fn run(cfg: AudioConfig, t0: i64, stop: Arc<AtomicBool>, tx: Sender<AudioOut>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let mut task = 0u32;
        let _ = windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW(windows::core::w!("Audio"), &mut task);
    }
    let layout: Vec<TrackMix> = match (cfg.system, cfg.mic) {
        (false, false) => vec![],
        (true, false) => vec![TrackMix::System],
        (false, true) => vec![TrackMix::Mic],
        (true, true) if cfg.separate => vec![TrackMix::System, TrackMix::Mic, TrackMix::Mix],
        (true, true) => vec![TrackMix::Mix],
    };
    if layout.is_empty() {
        return;
    }
    let mut encoders: Vec<AacEncoder> = Vec::new();
    for _ in &layout {
        match AacEncoder::new(cfg.bitrate_kbps) {
            Ok(e) => encoders.push(e),
            Err(e) => {
                let _ = tx.send(AudioOut::Error(trf!(["AAC-кодер недоступен: {e}", "AAC encoder unavailable: {e}", "AAC-кодер недоступний: {e}"])));
                return;
            }
        }
    }

    let sys_kind = SourceKind::Loopback;
    let mic_kind = SourceKind::Mic(cfg.mic_device.clone());
    let mut sys_src: Option<WasapiSource> = None;
    let mut mic_src: Option<WasapiSource> = None;
    let mut sys_tl = Timeline::new();
    let mut mic_tl = Timeline::new();
    let mut last_retry = Instant::now() - Duration::from_secs(10);
    let mut last_default_check = Instant::now();

    let mut mixed: i64 = 0;
    let mut counts = vec![0i64; layout.len()];
    let mut sys_buf = vec![0f32; AAC_FRAME as usize * 2];
    let mut mic_buf = vec![0f32; AAC_FRAME as usize * 2];
    let mut pcm = vec![0i16; AAC_FRAME as usize * 2];
    let mut frames_out: Vec<Vec<u8>> = Vec::new();

    while !stop.load(Ordering::Relaxed) {
        // (Re)open devices when missing; follow default output device changes.
        if last_retry.elapsed() > Duration::from_secs(2) {
            last_retry = Instant::now();
            if cfg.system && sys_src.is_none() {
                sys_src = unsafe { WasapiSource::open(&sys_kind) }.map_err(|e| log::warn!("loopback: {e}")).ok();
            }
            if cfg.mic && mic_src.is_none() {
                mic_src = unsafe { WasapiSource::open(&mic_kind) }.map_err(|e| log::warn!("mic: {e}")).ok();
            }
        }
        if last_default_check.elapsed() > Duration::from_secs(3) {
            last_default_check = Instant::now();
            if let Some(s) = &sys_src {
                if default_render_id().map(|id| id != s.device_id).unwrap_or(false) {
                    log::info!("default output device changed, reopening loopback");
                    sys_src = None;
                    last_retry = Instant::now() - Duration::from_secs(10);
                }
            }
        }
        if let Some(s) = &mut sys_src {
            if let Err(e) = unsafe { s.read(&mut sys_tl, t0) } {
                log::warn!("loopback read: {e}");
                sys_src = None;
            }
        }
        if let Some(s) = &mut mic_src {
            if let Err(e) = unsafe { s.read(&mut mic_tl, t0) } {
                log::warn!("mic read: {e}");
                mic_src = None;
            }
        }

        let target = ((now_hns() - t0) as i128 * AUDIO_RATE as i128 / HNS as i128) as i64 - LAG;
        let (vs, vm) = *cfg.volumes.lock();
        while mixed + AAC_FRAME <= target {
            if cfg.system {
                sys_tl.take(mixed, &mut sys_buf);
            }
            if cfg.mic {
                mic_tl.take(mixed, &mut mic_buf);
            }
            for (ti, mix) in layout.iter().enumerate() {
                for i in 0..pcm.len() {
                    let v = match mix {
                        TrackMix::Mix => sys_buf[i] * vs + mic_buf[i] * vm,
                        TrackMix::System => sys_buf[i] * vs,
                        TrackMix::Mic => mic_buf[i] * vm,
                    };
                    pcm[i] = (v.clamp(-1.0, 1.0) * 32767.0) as i16;
                }
                let pts = mixed * HNS / AUDIO_RATE as i64;
                frames_out.clear();
                if let Err(e) = encoders[ti].encode(&pcm, pts, &mut frames_out) {
                    log::warn!("aac encode: {e}");
                    continue;
                }
                for f in frames_out.drain(..) {
                    let pts = counts[ti] * AAC_FRAME * HNS / AUDIO_RATE as i64;
                    counts[ti] += 1;
                    let _ = tx.send(AudioOut::Packet(ti, Packet { pts, key: true, data: f.into() }));
                }
            }
            mixed += AAC_FRAME;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(sys_src);
    drop(mic_src);
    drop(encoders);
    unsafe { CoUninitialize() };
}

fn default_render_id() -> Option<String> {
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let id = dev.GetId().ok()?;
        let s = id.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(id.0 as _));
        s
    }
}
