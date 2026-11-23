//! Видео/аудио-плеер на процессах ffmpeg: кадры RGBA и PCM f32 через каналы.
//! Скорость 0.25–3× с сохранением высоты звука (atempo).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Color32, ColorImage};

use crate::audio::Shared;
use crate::ffmpeg::{self, MediaInfo};
use crate::winutil::hidden_command;

pub const SPEEDS: [f32; 10] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

pub struct VFrame {
    pub pts: f64,
    pub image: Arc<ColorImage>,
}

type ChildSlot = Arc<Mutex<Option<Child>>>;

struct Session {
    rx: Option<Receiver<VFrame>>,
    children: Vec<ChildSlot>,
    video_eof: Arc<AtomicBool>,
    audio_eof: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        for c in &self.children {
            if let Some(mut ch) = c.lock().unwrap().take() {
                let _ = ch.kill();
                let _ = ch.wait();
            }
        }
    }
}

pub struct Player {
    path: PathBuf,
    pub info: MediaInfo,
    out_size: [usize; 2],
    audio: Option<Arc<Shared>>,
    ctx: egui::Context,
    session: Option<Session>,

    pub paused: bool,
    pub speed: f32,
    pub looping: bool,
    pub ended: bool,
    /// Индекс звуковой дорожки (`-map 0:a:N`).
    pub audio_track: usize,

    session_start: f64,
    wall_anchor: Option<(Instant, f64)>,
    paused_pos: f64,
    audio_drained: Option<(Instant, f64)>,
    pending: Option<VFrame>,
    pub current: Option<VFrame>,
    pub frame_changed: bool,
    want_preview: bool,
    pos_dirty: bool,
    last_scrub: Option<Instant>,
    pub error: Option<String>,
    pub waveform: Arc<Mutex<Option<Vec<f32>>>>,
}

fn atempo(speed: f32) -> String {
    let mut s = speed as f64;
    let mut parts = Vec::new();
    while s < 0.5 {
        parts.push("atempo=0.5".to_string());
        s /= 0.5;
    }
    while s > 2.0 {
        parts.push("atempo=2.0".to_string());
        s /= 2.0;
    }
    if (s - 1.0).abs() > 1e-3 || parts.is_empty() {
        parts.push(format!("atempo={s:.4}"));
    }
    parts.join(",")
}

impl Player {
    pub fn new(
        path: &Path,
        info: MediaInfo,
        audio: Option<Arc<Shared>>,
        ctx: &egui::Context,
        max_box: [u32; 2],
        speed: f32,
        looping: bool,
    ) -> Self {
        let mut out_size = [0, 0];
        if info.has_video && info.width > 0 && info.height > 0 {
            let (bw, bh) = if info.fps > 31.0 {
                (max_box[0].min(2560), max_box[1].min(1600))
            } else {
                (max_box[0].min(3840), max_box[1].min(2400))
            };
            let s = (bw as f64 / info.width as f64).min(bh as f64 / info.height as f64).min(1.0);
            let w = ((info.width as f64 * s / 2.0).round() * 2.0).max(2.0) as usize;
            let h = ((info.height as f64 * s / 2.0).round() * 2.0).max(2.0) as usize;
            out_size = [w, h];
        }
        let audio = if info.has_audio { audio } else { None };
        let audio_track = info.default_audio.min(info.audio_tracks.len().saturating_sub(1));
        let mut p = Self {
            path: path.to_path_buf(),
            info,
            out_size,
            audio,
            ctx: ctx.clone(),
            session: None,
            paused: false,
            speed,
            looping,
            ended: false,
            audio_track,
            session_start: 0.0,
            wall_anchor: None,
            paused_pos: 0.0,
            audio_drained: None,
            pending: None,
            current: None,
            frame_changed: false,
            want_preview: true,
            pos_dirty: false,
            last_scrub: None,
            error: None,
            waveform: Arc::new(Mutex::new(None)),
        };
        if ffmpeg::ffmpeg().is_none() {
            p.error = Some(ffmpeg::missing().into());
            p.paused = true;
            return p;
        }
        if !p.info.has_video && p.info.duration > 0.0 && p.info.duration < 4.0 * 3600.0 {
            p.spawn_waveform();
        }
        p.restart(0.0, true);
        p
    }

    pub fn has_video(&self) -> bool {
        self.info.has_video && self.out_size[0] > 0
    }

    pub fn duration(&self) -> f64 {
        self.info.duration.max(0.0)
    }

    fn uses_audio_clock(&self) -> bool {
        self.audio.is_some()
    }

    pub fn position(&self) -> f64 {
        if self.paused {
            return self.paused_pos;
        }
        let pos = if let Some((t, p)) = self.audio_drained {
            p + t.elapsed().as_secs_f64() * self.speed as f64
        } else if let Some(a) = &self.audio {
            let played = a.played.load(Ordering::SeqCst) as f64;
            self.session_start + played / a.rate as f64 * self.speed as f64
        } else if let Some((t, p)) = self.wall_anchor {
            p + t.elapsed().as_secs_f64() * self.speed as f64
        } else {
            self.session_start
        };
        if self.duration() > 0.0 { pos.min(self.duration()) } else { pos }
    }

    /// Перезапуск декодеров с позиции `pos`.
    fn restart(&mut self, pos: f64, with_audio: bool) {
        self.session = None; // убивает старые процессы
        let pos = pos.clamp(0.0, (self.duration() - 0.05).max(0.0));
        self.session_start = pos;
        self.paused_pos = pos;
        self.wall_anchor = None;
        self.audio_drained = None;
        self.pending = None;
        self.ended = false;
        self.want_preview = self.has_video();
        self.pos_dirty = false;

        let cancel = Arc::new(AtomicBool::new(false));
        let video_eof = Arc::new(AtomicBool::new(!self.has_video()));
        let audio_eof = Arc::new(AtomicBool::new(self.audio.is_none() || !with_audio));
        let mut children = Vec::new();
        let mut rx = None;
        let ff = ffmpeg::ffmpeg().unwrap().to_path_buf();

        if self.has_video() {
            let (tx, r) = sync_channel::<VFrame>(4);
            rx = Some(r);
            let fps = if self.speed > 1.0 {
                self.info.fps.min(62.0 / self.speed as f64)
            } else {
                self.info.fps
            };
            let [w, h] = self.out_size;
            let vf = format!("fps={fps:.5},scale={w}:{h}:flags=bilinear");
            let mut cmd = hidden_command(&ff);
            cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-hwaccel", "auto"])
                .args(["-ss", &format!("{pos:.3}")])
                .arg("-i")
                .arg(&self.path)
                .args(["-map", "0:V:0?", "-an", "-sn", "-dn", "-vf", &vf])
                .args(["-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let slot: ChildSlot = Arc::new(Mutex::new(None));
            children.push(slot.clone());
            let ctx = self.ctx.clone();
            let eof = video_eof.clone();
            let cancel2 = cancel.clone();
            std::thread::spawn(move || video_thread(cmd, slot, tx, [w, h], fps, pos, ctx, eof, cancel2));
        }

        if let (Some(a), true) = (&self.audio, with_audio) {
            let generation = a.reset();
            a.paused.store(self.paused, Ordering::SeqCst);
            let mut cmd = hidden_command(&ff);
            cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"])
                .args(["-ss", &format!("{pos:.3}")])
                .arg("-i")
                .arg(&self.path)
                .args(["-map", &format!("0:a:{}", self.audio_track), "-vn", "-sn", "-dn", "-af", &atempo(self.speed)])
                .args(["-f", "f32le", "-ac", "2", "-ar", &a.rate.to_string(), "pipe:1"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let slot: ChildSlot = Arc::new(Mutex::new(None));
            children.push(slot.clone());
            let a2 = a.clone();
            let eof = audio_eof.clone();
            let cancel2 = cancel.clone();
            std::thread::spawn(move || audio_thread(cmd, slot, a2, generation, eof, cancel2));
        } else if let Some(a) = &self.audio {
            a.reset();
        }

        self.session = Some(Session { rx, children, video_eof, audio_eof, cancel });
        self.ctx.request_repaint();
    }

    fn try_next(&mut self) -> Option<VFrame> {
        if let Some(f) = self.pending.take() {
            return Some(f);
        }
        let s = self.session.as_ref()?;
        match s.rx.as_ref()?.try_recv() {
            Ok(f) => Some(f),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }

    /// Вызывается каждый кадр интерфейса. Возвращает, через сколько секунд перерисовать.
    pub fn update(&mut self) -> Option<f64> {
        self.frame_changed = false;
        if self.session.is_none() {
            return None;
        }
        if self.want_preview {
            if let Some(f) = self.try_next() {
                if self.paused {
                    self.paused_pos = f.pts;
                }
                self.current = Some(f);
                self.frame_changed = true;
                self.want_preview = false;
                if !self.paused && !self.uses_audio_clock() && self.wall_anchor.is_none() {
                    self.wall_anchor = Some((Instant::now(), self.session_start));
                }
            }
        }
        if self.paused {
            return if self.want_preview { Some(0.01) } else { None };
        }
        if !self.uses_audio_clock() && self.wall_anchor.is_none() && !self.has_video() {
            self.wall_anchor = Some((Instant::now(), self.session_start));
        }

        // Звук закончился раньше видео — дальше идём по системным часам.
        if let (Some(a), Some(s)) = (&self.audio, &self.session) {
            if self.audio_drained.is_none() && s.audio_eof.load(Ordering::SeqCst) && a.buffered() == 0 {
                let p = self.position();
                self.audio_drained = Some((Instant::now(), p));
            }
        }

        let pos = self.position();
        let video_eof_before = self
            .session
            .as_ref()
            .map(|s| s.video_eof.load(Ordering::SeqCst))
            .unwrap_or(true);
        loop {
            match self.try_next() {
                Some(f) if f.pts <= pos + 0.004 => {
                    self.current = Some(f);
                    self.frame_changed = true;
                }
                Some(f) => {
                    self.pending = Some(f);
                    break;
                }
                None => break,
            }
        }

        // Конец файла
        let s = self.session.as_ref().unwrap();
        let video_done = !self.has_video() || (video_eof_before && self.pending.is_none());
        let audio_done = self.audio.is_none()
            || s.audio_eof.load(Ordering::SeqCst) && self.audio.as_ref().unwrap().buffered() == 0;
        let past_end = self.duration() > 0.0 && pos >= self.duration() - 0.02;
        let done = if self.has_video() || self.audio.is_some() { video_done && audio_done } else { past_end };
        if done {
            if self.looping {
                self.restart(0.0, true);
                return Some(0.01);
            }
            self.paused_pos = if self.duration() > 0.0 { self.duration() } else { pos };
            self.paused = true;
            self.ended = true;
            if let Some(a) = &self.audio {
                a.paused.store(true, Ordering::SeqCst);
            }
            return None;
        }

        Some(match &self.pending {
            Some(f) => ((f.pts - pos) / self.speed as f64).clamp(0.001, 0.05),
            None => 0.008,
        })
    }

    pub fn set_paused(&mut self, paused: bool) {
        if paused == self.paused {
            return;
        }
        if paused {
            self.paused_pos = self.position();
            self.paused = true;
            if let Some(a) = &self.audio {
                a.paused.store(true, Ordering::SeqCst);
            }
        } else {
            if self.ended {
                self.paused = false;
                self.restart(0.0, true);
                return;
            }
            self.paused = false;
            if self.pos_dirty {
                let p = self.paused_pos;
                self.restart(p, true);
                return;
            }
            if let Some((_, _)) = self.audio_drained {
                self.audio_drained = Some((Instant::now(), self.paused_pos));
            }
            if self.audio.is_none() || self.audio_drained.is_some() {
                self.wall_anchor = Some((Instant::now(), self.paused_pos));
            }
            if let Some(a) = &self.audio {
                a.paused.store(false, Ordering::SeqCst);
            }
        }
        self.ctx.request_repaint();
    }

    pub fn toggle(&mut self) {
        let p = !self.paused;
        self.set_paused(p);
    }

    pub fn seek(&mut self, pos: f64) {
        let was_paused = self.paused;
        self.restart(pos, true);
        self.paused = was_paused;
        if let Some(a) = &self.audio {
            a.paused.store(was_paused, Ordering::SeqCst);
        }
    }

    /// Быстрый предпросмотр при перетаскивании шкалы (без звука, с ограничением частоты).
    pub fn scrub(&mut self, pos: f64) {
        self.paused_pos = pos;
        let due = self.last_scrub.map(|t| t.elapsed() > Duration::from_millis(90)).unwrap_or(true);
        if due && self.has_video() {
            self.last_scrub = Some(Instant::now());
            let was = self.paused;
            self.paused = true;
            self.restart(pos, false);
            self.paused = was;
            self.pos_dirty = true;
        }
    }

    pub fn end_scrub(&mut self, pos: f64, resume: bool) {
        self.last_scrub = None;
        self.paused = !resume;
        self.restart(pos, true);
        if let Some(a) = &self.audio {
            a.paused.store(!resume, Ordering::SeqCst);
        }
    }

    pub fn set_speed(&mut self, speed: f32) {
        let speed = speed.clamp(0.25, 3.0);
        if (speed - self.speed).abs() < 1e-3 {
            return;
        }
        let pos = self.position();
        self.speed = speed;
        if self.session.is_some() && !self.ended {
            let was = self.paused;
            self.restart(pos, true);
            self.paused = was;
            if let Some(a) = &self.audio {
                a.paused.store(was, Ordering::SeqCst);
            }
        }
    }

    /// Переключение звуковой дорожки с сохранением позиции и паузы.
    pub fn set_audio_track(&mut self, track: usize) {
        if track >= self.info.audio_tracks.len() || track == self.audio_track {
            return;
        }
        let pos = self.position();
        self.audio_track = track;
        if !self.info.has_video {
            *self.waveform.lock().unwrap() = None;
            if self.info.duration > 0.0 && self.info.duration < 4.0 * 3600.0 && ffmpeg::ffmpeg().is_some() {
                self.spawn_waveform();
            }
        }
        if self.session.is_some() && !self.ended {
            let was = self.paused;
            self.restart(pos, true);
            self.paused = was;
            if let Some(a) = &self.audio {
                a.paused.store(was, Ordering::SeqCst);
            }
        }
    }

    /// Покадрово вперёд (на паузе).
    pub fn step_forward(&mut self) {
        if !self.has_video() {
            return;
        }
        self.set_paused(true);
        let deadline = Instant::now() + Duration::from_millis(800);
        loop {
            if let Some(f) = self.try_next() {
                self.paused_pos = f.pts;
                self.current = Some(f);
                self.frame_changed = true;
                self.pos_dirty = true;
                return;
            }
            let eof = self.session.as_ref().map(|s| s.video_eof.load(Ordering::SeqCst)).unwrap_or(true);
            if eof || Instant::now() > deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub fn step_back(&mut self) {
        if !self.has_video() {
            return;
        }
        self.set_paused(true);
        let cur = self.current.as_ref().map(|f| f.pts).unwrap_or(self.paused_pos);
        let target = (cur - 1.0 / self.info.fps.max(1.0)).max(0.0);
        self.restart(target, false);
        self.paused = true;
        self.pos_dirty = true;
    }

    fn spawn_waveform(&self) {
        let ff = ffmpeg::ffmpeg().unwrap().to_path_buf();
        let path = self.path.clone();
        let out = self.waveform.clone();
        let ctx = self.ctx.clone();
        let dur = self.info.duration;
        let map = format!("0:a:{}", self.audio_track);
        std::thread::spawn(move || {
            let rate = if dur > 1800.0 { 200 } else { 1000 };
            let Ok(o) = hidden_command(&ff)
                .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
                .arg(&path)
                .args(["-map", &map, "-vn", "-ac", "1", "-ar", &rate.to_string(), "-f", "f32le", "pipe:1"])
                .stderr(Stdio::null())
                .output()
            else {
                return;
            };
            let samples: Vec<f32> = o
                .stdout
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]).abs())
                .collect();
            if samples.is_empty() {
                return;
            }
            const BINS: usize = 480;
            let per = (samples.len() as f64 / BINS as f64).max(1.0);
            let mut bins = Vec::with_capacity(BINS);
            for i in 0..BINS {
                let a = (i as f64 * per) as usize;
                let b = (((i + 1) as f64 * per) as usize).min(samples.len());
                if a >= b {
                    bins.push(0.0);
                    continue;
                }
                let s = &samples[a..b];
                let rms = (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
                bins.push(rms);
            }
            let max = bins.iter().cloned().fold(0.0001f32, f32::max);
            for b in &mut bins {
                *b = (*b / max).sqrt();
            }
            *out.lock().unwrap() = Some(bins);
            ctx.request_repaint();
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn video_thread(
    mut cmd: std::process::Command,
    slot: ChildSlot,
    tx: SyncSender<VFrame>,
    size: [usize; 2],
    fps: f64,
    start: f64,
    ctx: egui::Context,
    eof: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
) {
    let Ok(mut child) = cmd.spawn() else {
        eof.store(true, Ordering::SeqCst);
        return;
    };
    let mut out = child.stdout.take().unwrap();
    {
        let mut s = slot.lock().unwrap();
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        *s = Some(child);
    }
    let n = size[0] * size[1];
    let mut i: u64 = 0;
    loop {
        let mut px = vec![Color32::BLACK; n];
        if out.read_exact(bytemuck::cast_slice_mut(&mut px)).is_err() {
            break;
        }
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let f = VFrame { pts: start + i as f64 / fps, image: Arc::new(ColorImage::new(size, px)) };
        if tx.send(f).is_err() {
            break;
        }
        ctx.request_repaint();
        i += 1;
    }
    eof.store(true, Ordering::SeqCst);
    ctx.request_repaint();
}

fn audio_thread(
    mut cmd: std::process::Command,
    slot: ChildSlot,
    a: Arc<Shared>,
    generation: u64,
    eof: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
) {
    let Ok(mut child) = cmd.spawn() else {
        eof.store(true, Ordering::SeqCst);
        return;
    };
    let mut out = child.stdout.take().unwrap();
    {
        let mut s = slot.lock().unwrap();
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        *s = Some(child);
    }
    let mut bytes = vec![0u8; 16384];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let n = match out.read(&mut bytes) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        carry.extend_from_slice(&bytes[..n]);
        let usable = carry.len() / 8 * 8; // целые стерео-пары
        let samples: Vec<f32> = carry[..usable]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        carry.drain(..usable);
        let mut idx = 0;
        while idx < samples.len() {
            if cancel.load(Ordering::SeqCst) || a.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let mut buf = a.buf.lock().unwrap();
            let free = a.capacity.saturating_sub(buf.len());
            if free < 2 {
                let _ = a.space.wait_timeout(buf, Duration::from_millis(30)).unwrap();
                continue;
            }
            let take = (free / 2 * 2).min(samples.len() - idx);
            buf.extend(&samples[idx..idx + take]);
            idx += take;
        }
    }
    if a.generation.load(Ordering::SeqCst) == generation {
        eof.store(true, Ordering::SeqCst);
    }
}
