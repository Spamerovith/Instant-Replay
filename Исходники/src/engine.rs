//! Pipeline orchestration: capture → convert → encode (video thread), audio thread, and the
//! sink thread that feeds the replay ring buffer and the streaming recorder.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender};
use parking_lot::{Mutex, RwLock};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::System::Threading::*;
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

use crate::audio::{self, AudioConfig, AudioOut};
use crate::capture::{self, sources, Capture};
use crate::config::{Codec, Config, PipelineKey};
use crate::encoder::video::{EncodedFrame, EncoderOption, VideoEncoder, VideoSettings};
use crate::gpu::{self, Converter, Gpu};
use crate::media::bitstream::{self, BitstreamConverter};
use crate::media::mp4::{self, StreamWriter, TrackKind};
use crate::media::{AudioInfo, Packet, VideoInfo};
use crate::replay::ReplayBuffer;
use crate::util::{self, now_hns, HNS};

// ------------------------------------------------------------------ public API

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoticeKind {
    ReplaySaved,
    RecordingStarted,
    RecordingSaved,
    ReplayOn,
    ReplayOff,
    Error,
}

#[derive(Clone, Debug)]
pub struct Notice {
    pub kind: NoticeKind,
    pub title: String,
    pub detail: String,
}

pub type Notifier = Arc<dyn Fn(Notice) + Send + Sync>;

#[derive(Clone, Debug)]
pub struct ClipInfo {
    pub path: PathBuf,
    pub secs: f64,
    pub bytes: u64,
    pub is_replay: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub running: bool,
    pub replay_active: bool,
    pub buffered_secs: f64,
    pub buffer_bytes: u64,
    pub recording_since: Option<Instant>,
    pub recording_bytes: u64,
    pub saving: u32,
    pub error: Option<String>,
    pub encoder_name: String,
    pub adapter_name: String,
    pub out_size: (u32, u32),
    pub source_closed: bool,
    pub dropped_frames: u64,
    pub encoders: Vec<EncoderOption>,
    pub recent: Vec<ClipInfo>,
    pub pending_restart: bool,
    /// Mirror of `Config::replay_enabled` as the engine sees it (hotkey/tray can flip it).
    pub replay_enabled: bool,
}

pub type SharedStatus = Arc<RwLock<Status>>;

pub enum Command {
    SaveReplay,
    SaveLast,
    ToggleRecording,
    ToggleReplay,
    ApplyConfig(Box<Config>),
    Shutdown(Sender<()>),
}

#[derive(Clone)]
pub struct EngineHandle {
    pub tx: Sender<Command>,
    pub status: SharedStatus,
}

impl EngineHandle {
    pub fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }
}

pub fn start(config: Config, notifier: Notifier) -> EngineHandle {
    let (tx, rx) = unbounded();
    let status: SharedStatus = Arc::new(RwLock::new(Status::default()));
    let st = status.clone();
    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || Engine::new(config, st, notifier).run(rx))
        .expect("engine thread");
    EngineHandle { tx, status }
}

// ------------------------------------------------------------------ engine

struct Engine {
    config: Config,
    status: SharedStatus,
    notify: Notifier,
    pipeline: Option<Pipeline>,
    recording: bool,
    retry_at: Option<Instant>,
}

impl Engine {
    fn new(config: Config, status: SharedStatus, notify: Notifier) -> Self {
        Engine { config, status, notify, pipeline: None, recording: false, retry_at: None }
    }

    fn run(mut self, rx: Receiver<Command>) {
        unsafe {
            let _ = RoInitialize(RO_INIT_MULTITHREADED);
        }
        // Probe encoders once for the settings UI.
        if let Ok(g) = Gpu::new() {
            let list = crate::encoder::video::probe(&g);
            let mut s = self.status.write();
            s.encoders = list;
            s.adapter_name = g.adapter_name.clone();
        }
        self.sync_pipeline();
        loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(Command::SaveReplay) => self.save_replay(None),
                Ok(Command::SaveLast) => {
                    let n = self.config.save_last_seconds;
                    self.save_replay(Some(n))
                }
                Ok(Command::ToggleRecording) => self.toggle_recording(),
                Ok(Command::ToggleReplay) => {
                    self.config.replay_enabled = !self.config.replay_enabled;
                    self.config.save();
                    let on = self.config.replay_enabled;
                    self.sync_pipeline();
                    (self.notify)(Notice {
                        kind: if on { NoticeKind::ReplayOn } else { NoticeKind::ReplayOff },
                        title: if on { tr!("Instant Replay включён", "Instant Replay on", "Instant Replay увімкнено").into() } else { tr!("Instant Replay выключен", "Instant Replay off", "Instant Replay вимкнено").into() },
                        detail: if on { trf!(["Буфер {}", "Buffer {}", "Буфер {}"], util::format_duration(self.config.replay_seconds as f64)) } else { String::new() },
                    });
                }
                Ok(Command::ApplyConfig(c)) => self.apply_config(*c),
                Ok(Command::Shutdown(done)) => {
                    self.stop_pipeline();
                    let _ = done.send(());
                    break;
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
            self.watchdog();
        }
        unsafe { RoUninitialize() };
    }

    fn wants_pipeline(&self) -> bool {
        self.config.replay_enabled || self.recording
    }

    fn sync_pipeline(&mut self) {
        let want = self.wants_pipeline();
        if want && self.pipeline.is_none() {
            self.start_pipeline();
        } else if !want && self.pipeline.is_some() {
            self.stop_pipeline();
        }
        if let Some(p) = &self.pipeline {
            let secs = if self.config.replay_enabled { Some(self.config.replay_seconds) } else { None };
            let _ = p.sink_tx.send(SinkMsg::SetReplay(secs));
        }
        let mut s = self.status.write();
        s.replay_enabled = self.config.replay_enabled;
        s.replay_active = self.config.replay_enabled && self.pipeline.is_some();
        s.running = self.pipeline.is_some();
    }

    fn start_pipeline(&mut self) {
        match Pipeline::start(&self.config, self.status.clone(), self.notify.clone()) {
            Ok(p) => {
                self.pipeline = Some(p);
                self.retry_at = None;
                self.status.write().error = None;
            }
            Err(e) => {
                log::error!("pipeline start failed: {e}");
                self.status.write().error = Some(e);
                self.retry_at = Some(Instant::now() + Duration::from_secs(5));
            }
        }
    }

    fn stop_pipeline(&mut self) {
        if let Some(p) = self.pipeline.take() {
            p.stop();
        }
        self.recording = false;
        let mut s = self.status.write();
        s.running = false;
        s.replay_active = false;
        s.recording_since = None;
        s.buffered_secs = 0.0;
        s.buffer_bytes = 0;
    }

    fn watchdog(&mut self) {
        let failed = self.pipeline.as_ref().map(|p| p.video_failed()).unwrap_or(false);
        if failed {
            log::warn!("video thread stopped, restarting pipeline");
            let err = self.pipeline.as_ref().and_then(|p| p.error.lock().clone());
            let was_recording = self.recording;
            self.stop_pipeline();
            if let Some(e) = err {
                self.status.write().error = Some(e);
            }
            if was_recording {
                (self.notify)(Notice {
                    kind: NoticeKind::Error,
                    title: tr!("Запись остановлена", "Recording stopped", "Запис зупинено").into(),
                    detail: tr!("Захват прервался, файл сохранён", "Capture stopped, file saved", "Захоплення перервалося, файл збережено").into(),
                });
            }
            self.retry_at = Some(Instant::now() + Duration::from_secs(2));
        }
        if let Some(t) = self.retry_at {
            if Instant::now() >= t && self.pipeline.is_none() && self.wants_pipeline() {
                self.retry_at = None;
                self.sync_pipeline();
            }
        }
        // Apply deferred settings once recording is over.
        let pending = self.pipeline.as_ref().map(|p| p.key != self.config.pipeline_key()).unwrap_or(false);
        if pending && !self.recording {
            self.stop_pipeline();
            self.sync_pipeline();
        }
        self.status.write().pending_restart = pending && self.recording;
    }

    fn apply_config(&mut self, c: Config) {
        let hot_changed = c.replay_seconds != self.config.replay_seconds || c.replay_enabled != self.config.replay_enabled;
        if let Some(p) = &self.pipeline {
            let mut v = p.volumes.lock();
            *v = (if c.system_audio { c.system_volume } else { 0.0 }, if c.mic_enabled { c.mic_volume } else { 0.0 });
        }
        self.config = c;
        if hot_changed {
            self.sync_pipeline();
        }
        // Pipeline-affecting changes are handled by the watchdog (deferred while recording).
        self.watchdog();
    }

    fn output_path(&self, kind: &str) -> PathBuf {
        let dir = self.config.output_dir.clone();
        let _ = std::fs::create_dir_all(&dir);
        let label = self.pipeline.as_ref().map(|p| p.label.clone()).unwrap_or_else(|| "Desktop".into());
        let base = format!("{} - {} {}", util::sanitize_file_name(&label), kind, util::timestamp_for_file());
        let mut p = dir.join(format!("{base}.mp4"));
        let mut i = 2;
        while p.exists() {
            p = dir.join(format!("{base} ({i}).mp4"));
            i += 1;
        }
        p
    }

    fn check_disk(&self) -> bool {
        if let Some(free) = util::free_disk_bytes(&self.config.output_dir) {
            if free < 512 * 1024 * 1024 {
                (self.notify)(Notice {
                    kind: NoticeKind::Error,
                    title: tr!("Мало места на диске", "Low disk space", "Мало місця на диску").into(),
                    detail: trf!(["Свободно {}", "{} free", "Вільно {}"], util::format_bytes(free)),
                });
                return false;
            }
        }
        true
    }

    fn save_replay(&mut self, last: Option<u32>) {
        let Some(p) = &self.pipeline else {
            (self.notify)(Notice {
                kind: NoticeKind::Error,
                title: tr!("Instant Replay выключен", "Instant Replay off", "Instant Replay вимкнено").into(),
                detail: tr!("Включите его, чтобы сохранять повторы", "Turn it on to save replays", "Увімкніть його, щоб зберігати повтори").into(),
            });
            return;
        };
        if !self.config.replay_enabled {
            (self.notify)(Notice {
                kind: NoticeKind::Error,
                title: tr!("Instant Replay выключен", "Instant Replay off", "Instant Replay вимкнено").into(),
                detail: tr!("Включите его, чтобы сохранять повторы", "Turn it on to save replays", "Увімкніть його, щоб зберігати повтори").into(),
            });
            return;
        }
        if !self.check_disk() {
            return;
        }
        let path = self.output_path("Replay");
        let _ = p.sink_tx.send(SinkMsg::SaveReplay(path, last));
    }

    fn toggle_recording(&mut self) {
        if self.recording {
            if let Some(p) = &self.pipeline {
                let _ = p.sink_tx.send(SinkMsg::StopRecording);
            }
            self.recording = false;
            self.status.write().recording_since = None;
            self.sync_pipeline();
            return;
        }
        if !self.check_disk() {
            return;
        }
        self.recording = true;
        self.sync_pipeline();
        let path = self.output_path("Recording");
        match &self.pipeline {
            Some(p) => {
                p.force_key.store(true, Ordering::SeqCst);
                let _ = p.sink_tx.send(SinkMsg::StartRecording(path));
                self.status.write().recording_since = Some(Instant::now());
            }
            None => {
                self.recording = false;
                let e = self.status.read().error.clone().unwrap_or_default();
                (self.notify)(Notice { kind: NoticeKind::Error, title: tr!("Не удалось начать запись", "Could not start recording", "Не вдалося почати запис").into(), detail: e });
            }
        }
    }
}

// ------------------------------------------------------------------ pipeline

enum SinkMsg {
    VideoInfo(VideoInfo),
    Video(Packet),
    SetReplay(Option<u32>),
    SaveReplay(PathBuf, Option<u32>),
    StartRecording(PathBuf),
    StopRecording,
    Stop,
}

struct Pipeline {
    key: PipelineKey,
    label: String,
    stop: Arc<AtomicBool>,
    force_key: Arc<AtomicBool>,
    volumes: Arc<Mutex<(f32, f32)>>,
    sink_tx: Sender<SinkMsg>,
    video: Option<JoinHandle<()>>,
    audio: Option<JoinHandle<()>>,
    sink: Option<JoinHandle<()>>,
    video_done: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
}

struct VideoStarted {
    encoder: String,
    out: (u32, u32),
}

impl Pipeline {
    fn start(cfg: &Config, status: SharedStatus, notify: Notifier) -> Result<Pipeline, String> {
        let (target_label, _) = match sources::resolve(&cfg.source) {
            Some((_, l)) => (l, ()),
            None => {
                return Err(match cfg.source {
                    crate::config::CaptureSource::Window { .. } => tr!("Окно для захвата не найдено", "Capture window not found", "Вікно для захоплення не знайдено").into(),
                    _ => tr!("Монитор не найден", "Monitor not found", "Монітор не знайдено").into(),
                })
            }
        };
        if !capture::is_capture_supported() {
            return Err(tr!("Windows Graphics Capture недоступен в этой системе", "Windows Graphics Capture is not available on this system", "Windows Graphics Capture недоступний у цій системі").into());
        }
        let t0 = now_hns();
        let stop = Arc::new(AtomicBool::new(false));
        let force_key = Arc::new(AtomicBool::new(false));
        let video_done = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let dropped = Arc::new(AtomicU64::new(0));
        let volumes = Arc::new(Mutex::new((
            if cfg.system_audio { cfg.system_volume } else { 0.0 },
            if cfg.mic_enabled { cfg.mic_volume } else { 0.0 },
        )));

        let audio_cfg = AudioConfig {
            system: cfg.system_audio,
            mic: cfg.mic_enabled,
            mic_device: cfg.mic_device.clone(),
            separate: cfg.separate_tracks,
            bitrate_kbps: cfg.audio_bitrate_kbps,
            volumes: volumes.clone(),
        };
        let audio_infos = audio::track_infos(&audio_cfg);

        let (sink_tx, sink_rx) = bounded::<SinkMsg>(4096);
        let (audio_tx, audio_rx) = bounded::<AudioOut>(4096);
        let (started_tx, started_rx) = bounded::<Result<VideoStarted, String>>(1);

        // Video thread.
        let video = {
            let cfg = cfg.clone();
            let stop = stop.clone();
            let force_key = force_key.clone();
            let sink_tx = sink_tx.clone();
            let video_done = video_done.clone();
            let error = error.clone();
            let dropped = dropped.clone();
            let status = status.clone();
            std::thread::Builder::new()
                .name("video".into())
                .spawn(move || {
                    let r = video_thread(&cfg, t0, &stop, &force_key, &sink_tx, &started_tx, &dropped, &status);
                    if let Err(e) = r {
                        log::error!("video thread: {e}");
                        let _ = started_tx.try_send(Err(e.clone()));
                        *error.lock() = Some(e);
                    }
                    video_done.store(true, Ordering::SeqCst);
                })
                .map_err(|e| e.to_string())?
        };

        let started = match started_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                stop.store(true, Ordering::SeqCst);
                let _ = video.join();
                return Err(e);
            }
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                return Err(tr!("Видеокодер не ответил при запуске", "The video encoder did not respond at startup", "Відеокодер не відповів під час запуску").into());
            }
        };
        {
            let mut s = status.write();
            s.encoder_name = started.encoder.clone();
            s.out_size = started.out;
            s.dropped_frames = 0;
            s.source_closed = false;
        }

        // Audio thread.
        let audio = if !audio_infos.is_empty() {
            let stop = stop.clone();
            Some(
                std::thread::Builder::new()
                    .name("audio".into())
                    .spawn(move || audio::run(audio_cfg, t0, stop, audio_tx))
                    .map_err(|e| e.to_string())?,
            )
        } else {
            drop(audio_tx);
            None
        };

        // Sink thread.
        let sink = {
            let status = status.clone();
            let notify = notify.clone();
            let infos = audio_infos.clone();
            let replay = if cfg.replay_enabled { Some(cfg.replay_seconds) } else { None };
            std::thread::Builder::new()
                .name("sink".into())
                .spawn(move || sink_thread(sink_rx, audio_rx, infos, replay, status, notify))
                .map_err(|e| e.to_string())?
        };

        Ok(Pipeline {
            key: cfg.pipeline_key(),
            label: target_label,
            stop,
            force_key,
            volumes,
            sink_tx,
            video: Some(video),
            audio,
            sink: Some(sink),
            video_done,
            error,
        })
    }

    fn video_failed(&self) -> bool {
        self.video_done.load(Ordering::SeqCst) && !self.stop.load(Ordering::SeqCst)
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(v) = self.video.take() {
            let _ = v.join();
        }
        if let Some(a) = self.audio.take() {
            let _ = a.join();
        }
        let _ = self.sink_tx.send(SinkMsg::Stop);
        if let Some(s) = self.sink.take() {
            let _ = s.join();
        }
    }
}

// ------------------------------------------------------------------ video thread

struct Timer(HANDLE);

impl Timer {
    fn new() -> Option<Timer> {
        unsafe {
            CreateWaitableTimerExW(None, None, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS.0)
                .ok()
                .or_else(|| CreateWaitableTimerExW(None, None, 0, TIMER_ALL_ACCESS.0).ok())
                .map(Timer)
        }
    }
    fn sleep_hns(&self, hns: i64) {
        if hns <= 0 {
            return;
        }
        unsafe {
            let due = -hns;
            if SetWaitableTimer(self.0, &due, 0, None, None, false).is_ok() {
                WaitForSingleObject(self.0, 1000);
            } else {
                std::thread::sleep(Duration::from_nanos(hns as u64 * 100));
            }
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn codec_max_width(c: Codec) -> u32 {
    match c {
        Codec::H264 => 4096,
        _ => 8192,
    }
}

fn fit_codec_limits(codec: Codec, (w, h): (u32, u32)) -> (u32, u32) {
    let max_w = codec_max_width(codec);
    let max_h = if codec == Codec::H264 { 4096 } else { 8192 };
    if w <= max_w && h <= max_h {
        return (w, h);
    }
    let s = (max_w as f64 / w as f64).min(max_h as f64 / h as f64);
    (((w as f64 * s) as u32) & !1, ((h as f64 * s) as u32) & !1)
}

#[allow(clippy::too_many_arguments)]
fn video_thread(
    cfg: &Config,
    t0: i64,
    stop: &AtomicBool,
    force_key: &AtomicBool,
    sink: &Sender<SinkMsg>,
    started: &Sender<Result<VideoStarted, String>>,
    dropped: &AtomicU64,
    status: &SharedStatus,
) -> Result<(), String> {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
    }
    let gpu = Gpu::new().map_err(|e| trf!(["Не удалось создать D3D11-устройство: {e}", "Could not create a D3D11 device: {e}", "Не вдалося створити D3D11-пристрій: {e}"]))?;
    let (target, _label) = sources::resolve(&cfg.source).ok_or(tr!("Источник захвата не найден", "Capture source not found", "Джерело захоплення не знайдено"))?;
    let mut cap = Capture::new(&gpu.device, &target, cfg.capture_cursor)
        .map_err(|e| trf!(["Не удалось начать захват: {e}", "Could not start capture: {e}", "Не вдалося почати захоплення: {e}"]))?;

    // Size of the source at start determines the (fixed) encode size.
    let src_size = initial_source_size(&target);
    let out = gpu::output_size(src_size.0, src_size.1, cfg.resolution.height());

    let mut codec = cfg.codec;
    let mut settings = VideoSettings {
        codec,
        width: 0,
        height: 0,
        fps: cfg.fps,
        rate_control: cfg.rate_control,
        bitrate_mbps: 0,
        quality: cfg.quality,
        preset: cfg.preset,
    };
    let enc = loop {
        let (w, h) = fit_codec_limits(codec, out);
        settings.codec = codec;
        settings.bitrate_mbps = if cfg.auto_bitrate { crate::config::auto_bitrate_mbps(codec, w, h, cfg.fps) } else { cfg.bitrate_mbps };
        settings.width = w;
        settings.height = h;
        match VideoEncoder::new(&gpu, &settings) {
            Ok(e) => break e,
            Err(e) if codec != Codec::H264 => {
                log::warn!("{:?} encoder unavailable ({e}), falling back to H.264", codec);
                codec = Codec::H264;
            }
            Err(e) => return Err(trf!(["Нет доступного видеокодера: {e}", "No video encoder available: {e}", "Немає доступного відеокодера: {e}"])),
        }
    };
    let mut enc = enc;
    let (out_w, out_h) = (settings.width, settings.height);
    let mut conv = Converter::new(&gpu, out_w, out_h).map_err(|e| trf!(["Видеопроцессор: {e}", "Video processor: {e}", "Відеопроцесор: {e}"]))?;

    // Black placeholder until the first frame (e.g. a minimized window at start).
    let mut src_tex: Option<ID3D11Texture2D> = Some(black_texture(&gpu, out_w, out_h).map_err(|e| e.to_string())?);
    let mut have_real_frame = false;

    let _ = started.send(Ok(VideoStarted {
        encoder: format!("{} · {}", enc.name, codec.label()),
        out: (out_w, out_h),
    }));

    let mut bs = BitstreamConverter::new(codec);
    let mut info_sent = false;
    let mut outs: Vec<EncodedFrame> = Vec::new();
    let timer = Timer::new();
    let period = HNS / cfg.fps.max(1) as i64;
    let mut n = (now_hns() - t0) / period + 1;
    let mut last_status = Instant::now();
    let mut consecutive_errors = 0u32;

    while !stop.load(Ordering::Relaxed) {
        let due = t0 + n * period;
        let now = now_hns();
        if due > now {
            match &timer {
                Some(t) => t.sleep_hns(due - now),
                None => std::thread::sleep(Duration::from_nanos(((due - now) * 100) as u64)),
            }
        } else if now - due > period * 2 {
            // Fell behind (system stall): skip to the present instead of bursting frames.
            let skip = (now - t0) / period + 1 - n;
            dropped.fetch_add(skip.max(0) as u64, Ordering::Relaxed);
            n += skip.max(0);
            continue;
        }

        match cap.poll(&gpu.context, &gpu.device, &mut src_tex) {
            Ok(Some(_)) => {
                if !have_real_frame {
                    have_real_frame = true;
                    conv.forget_input();
                }
            }
            Ok(None) => {}
            Err(e) => {
                consecutive_errors += 1;
                log::warn!("capture poll: {e}");
                if consecutive_errors > 100 {
                    return Err(trf!(["Захват прерван: {e}", "Capture interrupted: {e}", "Захоплення перервано: {e}"]));
                }
            }
        }

        enc.poll(&mut outs).map_err(|e| trf!(["Ошибка кодера: {e}", "Encoder error: {e}", "Помилка кодера: {e}"]))?;
        if force_key.swap(false, Ordering::SeqCst) {
            enc.force_keyframe();
        }
        if enc.can_accept() {
            match enc.alloc_surface() {
                Ok(Some((sample, tex, sub))) => {
                    if let Some(src) = &src_tex {
                        if let Err(e) = conv.convert(src, &tex, sub) {
                            log::warn!("convert: {e}");
                        }
                    }
                    enc.encode(&sample, due - t0, &mut outs).map_err(|e| trf!(["Ошибка кодера: {e}", "Encoder error: {e}", "Помилка кодера: {e}"]))?;
                    consecutive_errors = 0;
                }
                Ok(None) => {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => return Err(trf!(["Ошибка видеопамяти: {e}", "Video memory error: {e}", "Помилка відеопам'яті: {e}"])),
            }
        } else {
            dropped.fetch_add(1, Ordering::Relaxed);
        }

        for f in outs.drain(..) {
            let data = bs.convert(&f.data);
            if !info_sent {
                if let Some(cfgrec) = bs.config_record() {
                    let _ = sink.send(SinkMsg::VideoInfo(VideoInfo {
                        codec,
                        width: out_w,
                        height: out_h,
                        fps: cfg.fps,
                        config: cfgrec,
                    }));
                    info_sent = true;
                } else {
                    continue; // no parameter sets yet: cannot mux
                }
            }
            if data.is_empty() {
                continue;
            }
            let key = f.key || bitstream::is_keyframe(codec, &data);
            let _ = sink.send(SinkMsg::Video(Packet { pts: f.pts, key, data: data.into() }));
        }

        if last_status.elapsed() > Duration::from_millis(500) {
            last_status = Instant::now();
            let mut s = status.write();
            s.dropped_frames = dropped.load(Ordering::Relaxed);
            s.source_closed = cap.closed.load(Ordering::Relaxed);
        }
        n += 1;
    }
    drop(enc);
    drop(cap);
    unsafe { RoUninitialize() };
    Ok(())
}

fn initial_source_size(target: &capture::Target) -> (u32, u32) {
    use windows::Graphics::Capture::GraphicsCaptureItem;
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
    unsafe {
        let Ok(interop) = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>() else {
            return (1920, 1080);
        };
        let item: windows::core::Result<GraphicsCaptureItem> = match target {
            capture::Target::Monitor(m) => interop.CreateForMonitor(*m),
            capture::Target::Window(h) => interop.CreateForWindow(*h),
        };
        match item.and_then(|i| i.Size()) {
            Ok(s) if s.Width > 0 && s.Height > 0 => (s.Width as u32, s.Height as u32),
            _ => (1920, 1080),
        }
    }
}

fn black_texture(gpu: &Gpu, w: u32, h: u32) -> windows::core::Result<ID3D11Texture2D> {
    let data = vec![0u8; (w * h * 4) as usize];
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let init = D3D11_SUBRESOURCE_DATA { pSysMem: data.as_ptr() as _, SysMemPitch: w * 4, SysMemSlicePitch: 0 };
    let mut t = None;
    unsafe { gpu.device.CreateTexture2D(&desc, Some(&init), Some(&mut t))? };
    Ok(t.unwrap())
}

// ------------------------------------------------------------------ sink thread

struct Recorder {
    path: PathBuf,
    writer: Option<StreamWriter>,
    base: i64,
}

/// The newest `n` clips in the output folder (all time, not just this session).
pub fn scan_clips(dir: &std::path::Path, n: usize) -> Vec<ClipInfo> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<(std::time::SystemTime, PathBuf, u64)> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if !p.extension().map(|x| x.eq_ignore_ascii_case("mp4")).unwrap_or(false) {
                return None;
            }
            let m = e.metadata().ok()?;
            m.is_file().then(|| (m.modified().unwrap_or(std::time::UNIX_EPOCH), p, m.len()))
        })
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files
        .into_iter()
        .take(n)
        .map(|(_, p, bytes)| {
            let secs = mp4::probe_duration(&p).unwrap_or(0.0);
            let is_replay = p.file_name().map(|f| f.to_string_lossy().contains(" - Replay ")).unwrap_or(false);
            ClipInfo { path: p, secs, bytes, is_replay }
        })
        .collect()
}

fn push_recent(status: &SharedStatus, clip: ClipInfo) {
    let mut s = status.write();
    s.recent.insert(0, clip);
    s.recent.truncate(8);
}

fn sink_thread(
    rx: Receiver<SinkMsg>,
    audio_rx: Receiver<AudioOut>,
    audio_infos: Vec<AudioInfo>,
    replay_secs: Option<u32>,
    status: SharedStatus,
    notify: Notifier,
) {
    let mut video_info: Option<VideoInfo> = None;
    let mut replay: Option<ReplayBuffer> = replay_secs.map(|s| ReplayBuffer::new(s, audio_infos.len()));
    let mut recorder: Option<Recorder> = None;
    let mut pending_save: Option<(PathBuf, Option<u32>, Instant)> = None;
    let mut last_status = Instant::now();
    let mut audio_open = true;

    let tracks = |vi: &VideoInfo| -> Vec<TrackKind> {
        let mut t = vec![TrackKind::Video(vi.clone())];
        t.extend(audio_infos.iter().cloned().map(TrackKind::Audio));
        t
    };

    loop {
        let msg = if audio_open {
            select! {
                recv(rx) -> m => m.ok().map(Ok),
                recv(audio_rx) -> a => match a {
                    Ok(a) => Some(Err(a)),
                    Err(_) => { audio_open = false; continue; }
                },
                default(Duration::from_millis(100)) => None,
            }
        } else {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(m) => Some(Ok(m)),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
                Err(_) => Some(Ok(SinkMsg::Stop)),
            }
        };

        match msg {
            Some(Ok(SinkMsg::VideoInfo(v))) => video_info = Some(v),
            Some(Ok(SinkMsg::Video(p))) => {
                if let Some(r) = &mut recorder {
                    if r.writer.is_none() && p.key {
                        if let Some(vi) = &video_info {
                            match StreamWriter::create(&r.path, tracks(vi), p.pts) {
                                Ok(w) => {
                                    r.writer = Some(w);
                                    r.base = p.pts;
                                    notify(Notice {
                                        kind: NoticeKind::RecordingStarted,
                                        title: tr!("Запись началась", "Recording started", "Запис почався").into(),
                                        detail: String::new(),
                                    });
                                }
                                Err(e) => {
                                    notify(Notice { kind: NoticeKind::Error, title: tr!("Не удалось начать запись", "Could not start recording", "Не вдалося почати запис").into(), detail: e.to_string() });
                                    recorder = None;
                                    status.write().recording_since = None;
                                }
                            }
                        }
                    }
                    if let Some(r) = &mut recorder {
                        if let Some(w) = &mut r.writer {
                            if let Err(e) = w.write(0, &p) {
                                log::error!("recording write: {e}");
                                let r = recorder.take().unwrap();
                                finish_recording(r, &status, &notify);
                                notify(Notice { kind: NoticeKind::Error, title: tr!("Запись остановлена", "Recording stopped", "Запис зупинено").into(), detail: trf!(["Ошибка записи на диск: {e}", "Disk write error: {e}", "Помилка запису на диск: {e}"]) });
                            }
                        }
                    }
                }
                if let Some(b) = &mut replay {
                    b.push_video(p);
                }
            }
            Some(Err(AudioOut::Packet(track, p))) => {
                if let Some(Recorder { writer: Some(w), base, .. }) = &mut recorder {
                    if p.pts >= *base {
                        let _ = w.write(track + 1, &p);
                    }
                }
                if let Some(b) = &mut replay {
                    b.push_audio(track, p);
                }
            }
            Some(Err(AudioOut::Error(e))) => {
                log::error!("audio: {e}");
                status.write().error = Some(e);
            }
            Some(Ok(SinkMsg::SetReplay(secs))) => match (secs, &mut replay) {
                (Some(s), Some(b)) => b.set_duration(s),
                (Some(s), None) => replay = Some(ReplayBuffer::new(s, audio_infos.len())),
                (None, _) => replay = None,
            },
            Some(Ok(SinkMsg::SaveReplay(path, last))) => {
                // Give the audio pipeline (≈100 ms behind) time to deliver the tail.
                pending_save = Some((path, last, Instant::now() + Duration::from_millis(250)));
            }
            Some(Ok(SinkMsg::StartRecording(path))) => {
                if recorder.is_none() {
                    recorder = Some(Recorder { path, writer: None, base: 0 });
                }
            }
            Some(Ok(SinkMsg::StopRecording)) => {
                if let Some(r) = recorder.take() {
                    finish_recording(r, &status, &notify);
                }
            }
            Some(Ok(SinkMsg::Stop)) => {
                if let Some(r) = recorder.take() {
                    finish_recording(r, &status, &notify);
                }
                break;
            }
            None => {}
        }

        if let Some((_, _, at)) = &pending_save {
            if Instant::now() >= *at {
                let (path, last, _) = pending_save.take().unwrap();
                match (&mut replay, &video_info) {
                    (Some(b), Some(vi)) => match b.snapshot_last(last) {
                        Some(snap) => {
                            if last.is_none() {
                                b.clear(); // a full save restarts the counter; a quick save keeps the buffer
                            }
                            let mut packets = vec![snap.video];
                            packets.extend(snap.audio);
                            let tr = tracks(vi);
                            let status = status.clone();
                            let notify = notify.clone();
                            status.write().saving += 1;
                            std::thread::spawn(move || save_clip(path, tr, packets, snap.base_pts, status, notify));
                        }
                        None => notify(Notice { kind: NoticeKind::Error, title: tr!("Буфер ещё пуст", "The buffer is still empty", "Буфер ще порожній").into(), detail: tr!("Подождите пару секунд", "Wait a couple of seconds", "Зачекайте кілька секунд").into() }),
                    },
                    _ => notify(Notice { kind: NoticeKind::Error, title: tr!("Буфер ещё пуст", "The buffer is still empty", "Буфер ще порожній").into(), detail: tr!("Подождите пару секунд", "Wait a couple of seconds", "Зачекайте кілька секунд").into() }),
                }
            }
        }

        if last_status.elapsed() > Duration::from_millis(250) {
            last_status = Instant::now();
            let mut s = status.write();
            match &replay {
                Some(b) => {
                    s.buffered_secs = b.buffered_secs();
                    s.buffer_bytes = b.bytes() as u64;
                }
                None => {
                    s.buffered_secs = 0.0;
                    s.buffer_bytes = 0;
                }
            }
            s.recording_bytes = recorder.as_ref().and_then(|r| r.writer.as_ref()).map(|w| w.bytes).unwrap_or(0);
        }
    }
}

fn finish_recording(r: Recorder, status: &SharedStatus, notify: &Notifier) {
    status.write().recording_since = None;
    let Some(w) = r.writer else {
        let _ = std::fs::remove_file(&r.path);
        return;
    };
    let bytes = w.bytes;
    match w.finish() {
        Ok(secs) => {
            let size = std::fs::metadata(&r.path).map(|m| m.len()).unwrap_or(bytes);
            push_recent(status, ClipInfo { path: r.path.clone(), secs, bytes: size, is_replay: false });
            notify(Notice {
                kind: NoticeKind::RecordingSaved,
                title: tr!("Запись сохранена", "Recording saved", "Запис збережено").into(),
                detail: format!("{} · {}", util::format_duration(secs), util::format_bytes(size)),
            });
        }
        Err(e) => notify(Notice { kind: NoticeKind::Error, title: tr!("Ошибка сохранения записи", "Could not save the recording", "Помилка збереження запису").into(), detail: e.to_string() }),
    }
}

fn save_clip(path: PathBuf, tracks: Vec<TrackKind>, packets: Vec<Vec<Packet>>, base: i64, status: SharedStatus, notify: Notifier) {
    let r = mp4::write_clip(&path, tracks, packets, base);
    status.write().saving -= 1;
    match r {
        Ok(secs) => {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            push_recent(&status, ClipInfo { path: path.clone(), secs, bytes: size, is_replay: true });
            notify(Notice {
                kind: NoticeKind::ReplaySaved,
                title: tr!("Повтор сохранён", "Replay saved", "Повтор збережено").into(),
                detail: format!("{} · {}", util::format_duration(secs), util::format_bytes(size)),
            });
        }
        Err(e) => {
            log::error!("save replay {}: {e}", path.display());
            notify(Notice { kind: NoticeKind::Error, title: tr!("Не удалось сохранить повтор", "Could not save the replay", "Не вдалося зберегти повтор").into(), detail: e.to_string() });
        }
    }
}

#[allow(dead_code)]
fn _path_ok(p: &Path) -> bool {
    p.parent().map(|d| d.exists()).unwrap_or(false)
}

#[cfg(test)]
mod scan_tests {
    use super::*;

    #[test]
    fn scan_clips_lists_newest_mp4_first_and_skips_other_files() {
        let dir = std::env::temp_dir().join(format!("ir-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a - Replay 1.mp4"), b"not a real mp4").unwrap();
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(dir.join("b - Recording 2.mp4"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let clips = scan_clips(&dir, 5);
        assert_eq!(clips.len(), 2);
        assert!(clips[0].path.ends_with("b - Recording 2.mp4") && !clips[0].is_replay);
        assert!(clips[1].is_replay);
        assert_eq!(clips[1].secs, 0.0); // unreadable mp4: duration unknown, clip still listed
        assert_eq!(scan_clips(&dir, 1).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
