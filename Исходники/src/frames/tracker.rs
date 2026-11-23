//! Taken from AfterCore (ac-telemetry/src/frames.rs) unchanged; the benchmark parts are not used here.
#![allow(dead_code)]
//! Per-process present tracking and frame statistics.
//!
//! * Presents are tracked **per swapchain**: a process with several swapchains (launchers,
//!   overlays, secondary windows) no longer mixes their timestamps into one jittery series.
//! * ETW delivers per-CPU buffers out of order, so the newest ~0.3 s may still be incomplete.
//!   Statistics are computed up to a "settled" point that lags the newest event by [`SETTLE_S`].
//! * NVIDIA Reflex PC-Latency markers (PCLStats) give the game's own frame rate. With frame
//!   generation the number of presents is higher than the number of game frames - the
//!   difference is how "real" vs "generated" FPS is shown.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

/// Where a present timestamp came from, in priority order (user-mode runtime first).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Dxgi = 0,
    D3d9 = 1,
    KernelHistoryDetailed = 2,
    KernelHistory = 3,
    KernelPresent = 4,
    KernelFlip = 5,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Dxgi => "DX10-12",
            Source::D3d9 => "DX9",
            Source::KernelHistoryDetailed | Source::KernelHistory | Source::KernelPresent | Source::KernelFlip => {
                "Vulkan/GL"
            }
        }
    }
    pub fn is_kernel(self) -> bool {
        (self as u8) >= 2
    }
}

/// Keep at most this much history per track.
const RETENTION_S: f64 = 62.0;
const MAX_STAMPS: usize = 60_000;
/// A process that hasn't presented for this long is considered idle.
pub const STALE_S: f64 = 1.5;
/// Lag behind the newest event so late ETW buffers can fill gaps first.
pub const SETTLE_S: f64 = 0.3;
/// FPS history bucket width (seconds) and count.
pub const FPS_BUCKET_S: f64 = 0.5;
pub const FPS_BUCKETS: usize = 120;

pub const PCL_SIMULATION_START: u32 = 0;
pub const PCL_PRESENT_START: u32 = 4;

#[derive(Default)]
struct Track {
    stamps: VecDeque<i64>,
    last_frame_id: u64,
}

impl Track {
    fn push(&mut self, ts: i64, qpf: f64) {
        match self.stamps.back() {
            Some(&last) if ts < last => {
                // Out-of-order buffers across CPUs; keep sorted.
                let pos = self.stamps.partition_point(|&t| t <= ts);
                self.stamps.insert(pos, ts);
            }
            _ => self.stamps.push_back(ts),
        }
        let newest = *self.stamps.back().unwrap();
        let horizon = newest - (RETENTION_S * qpf) as i64;
        while self.stamps.len() > MAX_STAMPS || self.stamps.front().is_some_and(|&t| t < horizon) {
            self.stamps.pop_front();
        }
    }

    fn last(&self) -> i64 {
        self.stamps.back().copied().unwrap_or(0)
    }

    fn count_since(&self, from: i64) -> usize {
        self.stamps.iter().rev().take_while(|&&t| t >= from).count()
    }
}

#[derive(Default)]
struct ProcFrames {
    tracks: HashMap<(Source, u64), Track>,
    pcl_present: Track,
    pcl_sim: Track,
    last: i64,
}

impl ProcFrames {
    /// Highest-priority source; within it the busiest swapchain.
    fn active(&self, qpf: f64) -> Option<(Source, &Track)> {
        // Judge every swapchain against the process's newest present, not its own last one:
        // a swapchain the game dropped (e.g. after toggling frame generation) must lose
        // immediately instead of freezing the counter at its old rate.
        let alive_from = self.last - (0.5 * qpf) as i64;
        let window_from = self.last - qpf as i64;
        let mut best: Option<(Source, usize, &Track)> = None;
        for ((src, _), t) in self.tracks.iter() {
            if t.last() < alive_from {
                continue;
            }
            let n = t.count_since(window_from);
            if n < 2 {
                continue;
            }
            let better = match best {
                None => true,
                Some((bs, bn, _)) => (*src as u8) < (bs as u8) || (*src == bs && n > bn),
            };
            if better {
                best = Some((*src, n, t));
            }
        }
        best.map(|(s, _, t)| (s, t))
    }
}

#[derive(Clone, Debug, Default)]
pub struct FrameStats {
    pub pid: u32,
    pub source: Option<Source>,
    /// Presented frames per second (includes generated frames).
    pub fps: f32,
    /// Game frames per second from Reflex PCL markers, when the game emits them.
    pub real_fps: Option<f32>,
    pub frametime_ms: f32,
    pub low1: f32,
    pub low01: f32,
    pub avg_fps: f32,
    /// Best 1-second FPS in the statistics window.
    pub max_fps: f32,
    pub min_fps: f32,
    /// Most recent frametimes in ms, oldest first.
    pub graph: Vec<f32>,
    /// FPS per 0.5 s bucket, oldest first (last 60 s).
    pub fps_hist: Vec<f32>,
    pub presenting: bool,
}

impl FrameStats {
    /// Frame generation is active when presents clearly outnumber game frames.
    pub fn frame_generation(&self) -> bool {
        matches!(self.real_fps, Some(r) if r > 1.0 && self.fps > r * 1.35)
    }
}

pub struct Recording {
    pub pid: u32,
    pub source: Source,
    pub swapchain: u64,
    pub stamps: Vec<i64>,
    pub started_qpc: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BenchSummary {
    pub frames: usize,
    pub duration_s: f32,
    pub avg_fps: f32,
    pub low1: f32,
    pub low01: f32,
    pub min_fps: f32,
    pub max_fps: f32,
    pub median_ms: f32,
    /// Frames longer than 2.5× the median frametime.
    pub stutters: usize,
}

pub struct FrameTracker {
    qpf: f64,
    procs: Mutex<HashMap<u32, ProcFrames>>,
    rec: Mutex<Option<Recording>>,
    pub events: AtomicU64,
    pub pcl_events: AtomicU64,
    self_pid: u32,
}

impl Default for FrameTracker {
    fn default() -> Self {
        Self::new()
    }
}

fn to_ms(d: i64, qpf: f64) -> f32 {
    (d as f64 * 1000.0 / qpf) as f32
}

impl FrameTracker {
    pub fn new() -> Self {
        let mut f = 0i64;
        unsafe { windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut f) };
        Self {
            qpf: f.max(1) as f64,
            procs: Mutex::new(HashMap::new()),
            rec: Mutex::new(None),
            events: AtomicU64::new(0),
            pcl_events: AtomicU64::new(0),
            self_pid: std::process::id(),
        }
    }

    pub fn qpf(&self) -> f64 {
        self.qpf
    }

    pub fn now(&self) -> i64 {
        let mut c = 0i64;
        unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut c) };
        c
    }

    /// Called from the ETW callback thread. `ts` is a raw QPC timestamp.
    pub fn on_present(&self, pid: u32, src: Source, swapchain: u64, ts: i64) {
        if pid == self.self_pid || pid == 0 || pid == 4 {
            return;
        }
        self.events.fetch_add(1, Ordering::Relaxed);
        {
            let mut map = self.procs.lock();
            let p = map.entry(pid).or_default();
            p.tracks.entry((src, swapchain)).or_default().push(ts, self.qpf);
            p.last = p.last.max(ts);
            // Forget swapchains that stopped presenting (recreated on settings changes).
            let dead = p.last - (5.0 * self.qpf) as i64;
            if p.tracks.len() > 1 {
                p.tracks.retain(|_, t| t.last() >= dead);
            }
        }
        let mut rec = self.rec.lock();
        if let Some(r) = rec.as_mut() {
            if r.pid == pid && r.source == src && r.swapchain == swapchain && ts >= r.started_qpc {
                r.stamps.push(ts);
            }
        }
    }

    /// Reflex PCL latency marker from the game itself.
    pub fn on_pcl_marker(&self, pid: u32, marker: u32, frame_id: u64, ts: i64) {
        if pid == self.self_pid {
            return;
        }
        self.pcl_events.fetch_add(1, Ordering::Relaxed);
        let mut map = self.procs.lock();
        let p = map.entry(pid).or_default();
        let t = match marker {
            PCL_PRESENT_START => &mut p.pcl_present,
            PCL_SIMULATION_START => &mut p.pcl_sim,
            _ => return,
        };
        if frame_id != 0 && frame_id == t.last_frame_id {
            return;
        }
        t.last_frame_id = frame_id;
        t.push(ts, self.qpf);
    }

    /// Forget processes that stopped presenting a while ago.
    pub fn prune(&self) {
        let horizon = self.now() - (10.0 * self.qpf) as i64;
        self.procs.lock().retain(|_, p| p.last >= horizon);
    }

    /// Processes that presented within the last [`STALE_S`] seconds: (pid, fps, source).
    pub fn presenting(&self) -> Vec<(u32, f32, Source)> {
        let now = self.now();
        let map = self.procs.lock();
        let mut out: Vec<(u32, f32, Source)> = map
            .iter()
            .filter(|(_, p)| (now - p.last) as f64 / self.qpf < STALE_S)
            .filter_map(|(pid, p)| {
                let (src, t) = p.active(self.qpf)?;
                let end = t.last() - (SETTLE_S * self.qpf) as i64;
                Some((*pid, fps_over(t, end, 1.0, self.qpf), src))
            })
            .collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1));
        out
    }

    pub fn stats(&self, pid: u32, window_s: f64, graph_frames: usize) -> Option<FrameStats> {
        let now = self.now();
        let q = self.qpf;
        let map = self.procs.lock();
        let p = map.get(&pid)?;
        let presenting = (now - p.last) as f64 / q < STALE_S;
        let (src, t) = p.active(q)?;
        let newest = t.last();
        // Settled end of data: lag behind the newest event, but never beyond what we have.
        let end = newest - (SETTLE_S * q) as i64;
        let settled: Vec<i64> = {
            let from = end - ((window_s.max(1.0) + 1.0) * q) as i64;
            t.stamps.iter().copied().filter(|&s| s >= from && s <= end).collect()
        };
        if settled.len() < 2 {
            return None;
        }

        let fps = fps_over(t, end, 1.0, q);

        // Statistics window.
        let wfrom = end - (window_s * q) as i64;
        let win: Vec<i64> = settled.iter().copied().filter(|&s| s >= wfrom).collect();
        let mut fts: Vec<f32> = win.windows(2).map(|w| to_ms(w[1] - w[0], q)).collect();
        let avg_fps = if win.len() >= 2 {
            ((win.len() - 1) as f64 * q / (win[win.len() - 1] - win[0]).max(1) as f64) as f32
        } else {
            0.0
        };
        // Best / worst complete 1-second bucket.
        let mut max_fps: f32 = 0.0;
        let mut min_fps: f32 = f32::MAX;
        let mut k = 0;
        loop {
            let hi = end - (k as f64 * q) as i64;
            let lo = hi - q as i64;
            if lo < wfrom || lo < win.first().copied().unwrap_or(i64::MAX) {
                break;
            }
            let n = win.iter().filter(|&&s| s > lo && s <= hi).count() as f32;
            max_fps = max_fps.max(n);
            min_fps = min_fps.min(n);
            k += 1;
        }
        if k == 0 {
            max_fps = fps;
            min_fps = fps;
        }

        // Recent frametime: mean over ~250 ms before the settled end.
        let rfrom = end - (0.25 * q) as i64;
        let recent: Vec<f32> =
            settled.windows(2).filter(|w| w[0] >= rfrom).map(|w| to_ms(w[1] - w[0], q)).collect();
        let frametime_ms = if recent.is_empty() {
            if fps > 0.0 { 1000.0 / fps } else { 0.0 }
        } else {
            recent.iter().sum::<f32>() / recent.len() as f32
        };

        let graph: Vec<f32> = {
            let n = settled.len();
            let take = graph_frames.min(n - 1);
            settled[n - take - 1..].windows(2).map(|w| to_ms(w[1] - w[0], q)).collect()
        };

        // FPS history in 0.5 s buckets.
        let mut fps_hist = vec![0f32; FPS_BUCKETS];
        let bw = (FPS_BUCKET_S * q) as i64;
        let hist_from = end - bw * FPS_BUCKETS as i64;
        let first = t.stamps.front().copied().unwrap_or(end);
        for &s in t.stamps.iter().filter(|&&s| s > hist_from && s <= end) {
            let idx = ((s - hist_from - 1) / bw) as usize;
            if idx < FPS_BUCKETS {
                fps_hist[idx] += 1.0;
            }
        }
        let first_bucket = ((first - hist_from).max(0) / bw) as usize + 1;
        let fps_hist: Vec<f32> = fps_hist
            .into_iter()
            .enumerate()
            .filter(|(i, _)| *i >= first_bucket.min(FPS_BUCKETS))
            .map(|(_, v)| v / FPS_BUCKET_S as f32)
            .collect();

        // Game frame rate from Reflex markers (present markers preferred).
        let real_fps = [&p.pcl_present, &p.pcl_sim].into_iter().find_map(|pt| {
            let last = pt.last();
            if pt.stamps.len() < 3 || (now - last) as f64 / q > STALE_S {
                return None;
            }
            let pend = last - (SETTLE_S * q) as i64;
            let f = fps_over(pt, pend, 1.0, q);
            (f > 0.0).then_some(f)
        });

        let (low1, low01, _, _) = lows(&mut fts);

        Some(FrameStats {
            pid,
            source: Some(src),
            fps: if presenting { fps } else { 0.0 },
            real_fps: if presenting { real_fps } else { None },
            frametime_ms,
            low1,
            low01,
            avg_fps,
            max_fps,
            min_fps: if min_fps == f32::MAX { 0.0 } else { min_fps },
            graph,
            fps_hist,
            presenting,
        })
    }

    // ───────── Benchmark recording ─────────

    pub fn is_recording(&self) -> Option<(u32, f64)> {
        let rec = self.rec.lock();
        rec.as_ref().map(|r| (r.pid, (self.now() - r.started_qpc) as f64 / self.qpf))
    }

    pub fn start_recording(&self, pid: u32) -> Result<Source, String> {
        let (src, swapchain) = {
            let map = self.procs.lock();
            let p = map.get(&pid).ok_or("процесс не выводит кадры")?;
            let (src, track) = p.active(self.qpf).ok_or("процесс не выводит кадры")?;
            let sc = p
                .tracks
                .iter()
                .find(|(_, t)| std::ptr::eq(*t, track))
                .map(|((_, sc), _)| *sc)
                .unwrap_or(0);
            (src, sc)
        };
        *self.rec.lock() = Some(Recording {
            pid,
            source: src,
            swapchain,
            stamps: Vec::with_capacity(1 << 16),
            started_qpc: self.now(),
        });
        Ok(src)
    }

    /// Stop and return frametimes (ms).
    pub fn stop_recording(&self) -> Option<(u32, Source, Vec<f32>)> {
        let mut r = self.rec.lock().take()?;
        r.stamps.sort_unstable();
        let fts = r.stamps.windows(2).map(|w| to_ms(w[1] - w[0], self.qpf)).collect();
        Some((r.pid, r.source, fts))
    }
}

/// Frames per second over `seconds` ending at `end`.
fn fps_over(t: &Track, end: i64, seconds: f64, qpf: f64) -> f32 {
    let from = end - (seconds * qpf) as i64;
    let mut n = 0usize;
    let mut first = 0i64;
    let mut last = 0i64;
    for &s in t.stamps.iter().rev() {
        if s > end {
            continue;
        }
        if s < from {
            break;
        }
        if n == 0 {
            last = s;
        }
        n += 1;
        first = s;
    }
    if n < 2 || last <= first {
        return 0.0;
    }
    ((n - 1) as f64 * qpf / (last - first) as f64) as f32
}

/// Percentile lows: "1% low" = 1000 / 99th-percentile frametime.
/// Returns (low1, low01, min_fps, max_fps). Sorts `fts` in place.
pub fn lows(fts: &mut [f32]) -> (f32, f32, f32, f32) {
    if fts.is_empty() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    fts.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| -> f32 {
        let idx = ((fts.len() as f64 - 1.0) * p).round() as usize;
        fts[idx.min(fts.len() - 1)]
    };
    let inv = |ms: f32| if ms > 0.0 { 1000.0 / ms } else { 0.0 };
    (inv(pct(0.99)), inv(pct(0.999)), inv(fts[fts.len() - 1]), inv(fts[0]))
}

pub fn summarize(frametimes: &[f32]) -> BenchSummary {
    if frametimes.is_empty() {
        return BenchSummary::default();
    }
    let total_ms: f64 = frametimes.iter().map(|&f| f as f64).sum();
    let mut sorted = frametimes.to_vec();
    let (low1, low01, min_fps, max_fps) = lows(&mut sorted);
    let median = sorted[sorted.len() / 2];
    BenchSummary {
        frames: frametimes.len(),
        duration_s: (total_ms / 1000.0) as f32,
        avg_fps: (frametimes.len() as f64 * 1000.0 / total_ms.max(1e-6)) as f32,
        low1,
        low01,
        min_fps,
        max_fps,
        median_ms: median,
        stutters: frametimes.iter().filter(|&&f| f > median * 2.5).count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(t: &FrameTracker, pid: u32, src: Source, sc: u64, fps: f64, secs: f64) -> i64 {
        let q = t.qpf();
        let base = t.now() - (secs * q) as i64;
        let n = (fps * secs) as i64;
        for i in 0..=n {
            t.on_present(pid, src, sc, base + (i as f64 * q / fps) as i64);
        }
        base
    }

    #[test]
    fn steady_60fps() {
        let t = FrameTracker::new();
        feed(&t, 1234, Source::Dxgi, 1, 60.0, 3.0);
        let s = t.stats(1234, 10.0, 100).unwrap();
        assert!((s.fps - 60.0).abs() < 0.5, "fps {}", s.fps);
        assert!((s.frametime_ms - 16.67).abs() < 0.2, "ft {}", s.frametime_ms);
        assert!((s.low1 - 60.0).abs() < 1.0);
        assert!((s.avg_fps - 60.0).abs() < 0.5, "avg {}", s.avg_fps);
        assert!((s.max_fps - 60.0).abs() <= 1.0, "max {}", s.max_fps);
        assert_eq!(s.graph.len(), 100);
        assert!(!s.fps_hist.is_empty());
        assert_eq!(s.source, Some(Source::Dxgi));
        assert!(!s.frame_generation());
    }

    #[test]
    fn second_swapchain_does_not_add_jitter() {
        let t = FrameTracker::new();
        feed(&t, 5, Source::Dxgi, 0xA, 120.0, 3.0);
        feed(&t, 5, Source::Dxgi, 0xB, 7.0, 3.0); // e.g. a launcher overlay swapchain
        let s = t.stats(5, 10.0, 50).unwrap();
        assert!((s.fps - 120.0).abs() < 1.0, "fps {}", s.fps);
        assert!(s.low1 > 110.0, "low1 {}", s.low1);
    }

    #[test]
    fn switching_off_frame_generation_does_not_freeze() {
        let t = FrameTracker::new();
        let q = t.qpf();
        let now = t.now();
        // 3 s with frame generation on an old swapchain (230 presents/s)…
        let base = now - (5.0 * q) as i64;
        for i in 0..=690 {
            t.on_present(3, Source::Dxgi, 0xA, base + (i as f64 * q / 230.0) as i64);
        }
        for i in 0..=345u64 {
            t.on_pcl_marker(3, PCL_PRESENT_START, i + 1, base + (i as f64 * q / 115.0) as i64);
        }
        // …then FG off: the game recreates its swapchain and runs at 115 FPS for 2 s.
        let base2 = now - (2.0 * q) as i64;
        for i in 0..=230 {
            t.on_present(3, Source::Dxgi, 0xB, base2 + (i as f64 * q / 115.0) as i64);
        }
        for i in 0..=230u64 {
            t.on_pcl_marker(3, PCL_PRESENT_START, 1000 + i, base2 + (i as f64 * q / 115.0) as i64);
        }
        let s = t.stats(3, 10.0, 10).unwrap();
        assert!((s.fps - 115.0).abs() < 2.0, "fps froze at {}", s.fps);
        assert!(!s.frame_generation(), "FG still reported");
    }

    #[test]
    fn prefers_runtime_source_over_kernel() {
        let t = FrameTracker::new();
        feed(&t, 7, Source::KernelFlip, 0, 60.0, 2.0);
        feed(&t, 7, Source::Dxgi, 1, 60.0, 2.0);
        assert_eq!(t.stats(7, 5.0, 10).unwrap().source, Some(Source::Dxgi));
    }

    #[test]
    fn real_fps_from_pcl_markers() {
        let t = FrameTracker::new();
        let q = t.qpf();
        let base = feed(&t, 9, Source::Dxgi, 1, 230.0, 3.0);
        for i in 0..=345u64 {
            t.on_pcl_marker(9, PCL_PRESENT_START, i + 1, base + (i as f64 * q / 115.0) as i64);
        }
        let s = t.stats(9, 10.0, 10).unwrap();
        let r = s.real_fps.unwrap();
        assert!((r - 115.0).abs() < 2.0, "real {r}");
        assert!(s.frame_generation());
    }

    #[test]
    fn lows_and_summary() {
        let mut fts = vec![10.0f32; 990];
        fts.extend(std::iter::repeat(50.0).take(10));
        let (_l1, l01, min, max) = lows(&mut fts);
        assert!((l01 - 20.0).abs() < 0.01);
        assert!((min - 20.0).abs() < 0.01);
        assert!((max - 100.0).abs() < 0.01);
        let s = summarize(&[10.0; 100]);
        assert!((s.avg_fps - 100.0).abs() < 0.01);
        assert_eq!(s.stutters, 0);
    }

    #[test]
    fn recording() {
        let t = FrameTracker::new();
        feed(&t, 9, Source::D3d9, 3, 30.0, 1.0);
        assert_eq!(t.start_recording(9).unwrap(), Source::D3d9);
        let q = t.qpf();
        let now = t.now();
        for i in 1..=10 {
            t.on_present(9, Source::D3d9, 3, now + (i as f64 * q / 100.0) as i64);
            t.on_present(9, Source::D3d9, 4, now + (i as f64 * q / 33.0) as i64); // other swapchain ignored
        }
        let (_, _, fts) = t.stop_recording().unwrap();
        assert_eq!(fts.len(), 9);
        assert!((fts[0] - 10.0).abs() < 0.1);
    }
}
