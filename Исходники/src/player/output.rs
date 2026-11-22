//! Звуковой выход: кольцевой буфер f32 stereo → устройство по умолчанию.
//! Счётчик сыгранных сэмплов служит главными часами плеера.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub struct Shared {
    pub buf: Mutex<VecDeque<f32>>,
    pub space: Condvar,
    /// Номер сеанса: сэмплы старого сеанса отбрасываются.
    pub generation: AtomicU64,
    /// Сыграно кадров (стерео-пар) в текущем сеансе.
    pub played: AtomicU64,
    pub volume: AtomicU32,
    pub paused: AtomicBool,
    pub rate: u32,
    pub capacity: usize,
}

impl Shared {
    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.to_bits(), Ordering::Relaxed);
    }
    /// Сбросить буфер для нового сеанса.
    pub fn reset(&self) -> u64 {
        let g = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.buf.lock().unwrap().clear();
        self.played.store(0, Ordering::SeqCst);
        self.space.notify_all();
        g
    }
    pub fn buffered(&self) -> usize {
        self.buf.lock().unwrap().len()
    }
}

pub struct AudioOut {
    pub shared: Arc<Shared>,
    _stream: cpal::Stream,
}

impl AudioOut {
    pub fn new() -> Option<Self> {
        let host = cpal::default_host();
        let dev = host.default_output_device()?;
        let cfg = dev.default_output_config().ok()?;
        let rate = cfg.sample_rate();
        let dev_channels = cfg.channels() as usize;
        let shared = Arc::new(Shared {
            buf: Mutex::new(VecDeque::with_capacity(rate as usize * 2)),
            space: Condvar::new(),
            generation: AtomicU64::new(0),
            played: AtomicU64::new(0),
            volume: AtomicU32::new(1.0f32.to_bits()),
            paused: AtomicBool::new(true),
            rate,
            capacity: rate as usize * 2 / 2, // ~0.5 c стерео
        });
        let s2 = shared.clone();
        let config = cpal::StreamConfig {
            channels: dev_channels as u16,
            sample_rate: rate,
            buffer_size: cpal::BufferSize::Default,
        };
        let stream = dev
            .build_output_stream::<f32, _, _>(
                config,
                move |data: &mut [f32], _| fill(&s2, data, dev_channels),
                |_| {},
                None,
            )
            .ok()?;
        stream.play().ok()?;
        Some(Self { shared, _stream: stream })
    }
}

fn fill(s: &Shared, out: &mut [f32], ch: usize) {
    if s.paused.load(Ordering::Relaxed) {
        out.fill(0.0);
        return;
    }
    let vol = f32::from_bits(s.volume.load(Ordering::Relaxed));
    let vol = vol * vol; // перцептивная кривая
    let Ok(mut buf) = s.buf.try_lock() else {
        out.fill(0.0);
        return;
    };
    let frames = out.len() / ch.max(1);
    let avail = buf.len() / 2;
    let n = frames.min(avail);
    for f in 0..frames {
        let (l, r) = if f < n {
            (buf.pop_front().unwrap_or(0.0), buf.pop_front().unwrap_or(0.0))
        } else {
            (0.0, 0.0)
        };
        let o = &mut out[f * ch..f * ch + ch];
        match ch {
            1 => o[0] = (l + r) * 0.5 * vol,
            _ => {
                o[0] = l * vol;
                o[1] = r * vol;
                for x in &mut o[2..] {
                    *x = 0.0;
                }
            }
        }
    }
    drop(buf);
    if n > 0 {
        s.played.fetch_add(n as u64, Ordering::SeqCst);
        s.space.notify_all();
    }
}
