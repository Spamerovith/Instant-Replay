//! Performance counter over the game (Alt+R): FPS, latency, frame time, GPU, CPU, RAM, ping and
//! the clock in one small row in a corner of the game's monitor.
//!
//! Anti-cheat friendly: nothing is injected into the game and no handle to it is opened. FPS and
//! frame times come from the timestamps of the pictures the game's window hands to Windows Graphics
//! Capture (the same API the recorder uses), the wait for the screen from the DWM vblank timing,
//! GPU load from the system performance counters, CPU and memory from the system totals and the
//! ping from the game's TCP connections in the system table (ICMP echo to its server).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use parking_lot::Mutex;
use windows::core::{factory, w, Interface};
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{COLORREF, FILETIME, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Performance::*;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Config;

const TIMER_ID: usize = 7;
const TICK_MS: u32 = 500;

/// What the counter shows; mirrors the HUD part of the config.
#[derive(Clone, PartialEq)]
pub struct HudConfig {
    pub corner: u8,
    pub fps: bool,
    pub latency: bool,
    pub display_latency: bool,
    pub ping: bool,
    pub frame_time: bool,
    pub gpu: bool,
    pub cpu: bool,
    pub ram: bool,
    pub clock: bool,
    pub in_capture: bool,
}

impl HudConfig {
    pub fn from(c: &Config) -> Self {
        HudConfig {
            corner: c.hud_corner,
            fps: c.hud_fps,
            latency: c.hud_latency,
            display_latency: c.hud_display_latency,
            ping: c.hud_ping,
            frame_time: c.hud_frame_time,
            gpu: c.hud_gpu,
            cpu: c.hud_cpu,
            ram: c.hud_ram,
            clock: c.hud_clock,
            in_capture: c.hud_in_capture,

        }
    }
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static CFG: OnceLock<Mutex<HudConfig>> = OnceLock::new();

pub fn start(c: &Config) {
    ENABLED.store(c.hud_enabled, Ordering::SeqCst);
    let _ = CFG.set(Mutex::new(HudConfig::from(c)));
    std::thread::Builder::new()
        .name("hud".into())
        .spawn(|| unsafe { run() })
        .expect("hud thread");
}

/// Applies the settings from the program window.
pub fn configure(c: &Config) {
    ENABLED.store(c.hud_enabled, Ordering::SeqCst);
    if let Some(m) = CFG.get() {
        *m.lock() = HudConfig::from(c);
    }
}

/// The counter is on and sits in the top right corner, where the toast shows up too.
pub fn in_top_right() -> bool {
    ENABLED.load(Ordering::SeqCst) && CFG.get().map_or(false, |m| m.lock().corner == 1)
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// Hotkey: shows / hides the counter and remembers it in the settings file right away (the
/// program window may be in the tray and never redraw).
pub fn toggle() {
    let on = !ENABLED.load(Ordering::SeqCst);
    ENABLED.store(on, Ordering::SeqCst);
    let mut c = Config::load();
    c.hud_enabled = on;
    c.save();
}

// ------------------------------------------------------------------ measurements

/// Frame timestamps of the measured window: intervals between its pictures and the wait from a
/// picture to the next vblank of the screen (all in milliseconds, newest last).
#[derive(Default)]
struct Timing {
    last_t: Option<i64>,
    intervals: std::collections::VecDeque<f64>,
    waits: std::collections::VecDeque<f64>,
    refresh_ms: f64,
}

impl Timing {
    const KEEP: usize = 240;

    fn push(&mut self, t100ns: i64) {
        if let Some(prev) = self.last_t {
            let d = (t100ns - prev) as f64 / 10_000.0;
            if d > 0.05 && d < 1000.0 {
                self.intervals.push_back(d);
            }
        }
        self.last_t = Some(t100ns);
        if let Some((wait, period)) = screen_wait(t100ns) {
            self.waits.push_back(wait);
            self.refresh_ms = period;
        }
        while self.intervals.len() > Self::KEEP {
            self.intervals.pop_front();
        }
        while self.waits.len() > Self::KEEP {
            self.waits.pop_front();
        }
    }
}

/// Wait from a picture (QPC time in 100 ns) to the next vblank of the screen, and the refresh
/// period, both in milliseconds. From the DWM composition timing.
fn screen_wait(t100ns: i64) -> Option<(f64, f64)> {
    use windows::Win32::Graphics::Dwm::{DwmGetCompositionTimingInfo, DWM_TIMING_INFO};
    use windows::Win32::System::Performance::QueryPerformanceFrequency;
    unsafe {
        let mut ti = DWM_TIMING_INFO { cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32, ..Default::default() };
        DwmGetCompositionTimingInfo(None, &mut ti).ok()?;
        let mut f = 0i64;
        QueryPerformanceFrequency(&mut f).ok()?;
        if f <= 0 || ti.qpcRefreshPeriod == 0 {
            return None;
        }
        let period = ti.qpcRefreshPeriod as f64;
        let frame = t100ns as f64 * f as f64 / 1e7;
        let k = ((frame - ti.qpcVBlank as f64) / period).ceil();
        let next = ti.qpcVBlank as f64 + k * period;
        let to_ms = 1000.0 / f as f64;
        Some(((next - frame).max(0.0) * to_ms, period * to_ms))
    }
}

/// Average of the newest `n` values.
fn mean_tail(v: &std::collections::VecDeque<f64>, n: usize) -> Option<f64> {
    let k = v.len().min(n);
    if k == 0 {
        return None;
    }
    Some(v.iter().rev().take(k).sum::<f64>() / k as f64)
}

/// Latency from input to the screen: the game samples input at the start of a frame, needs about
/// two of its frame times to simulate, render and queue it, then the picture waits for the
/// screen's vblank and is scanned out (half a refresh to the middle of the screen). Every part
/// is measured from timestamps, only the two-frame pipeline is the usual one of a game.
pub fn smart_latency(frame_ms: f64, wait_ms: f64, refresh_ms: f64) -> f64 {
    2.0 * frame_ms + wait_ms + refresh_ms / 2.0
}

/// Smart latency with frame generation: the game's real frame is `fg` shown frames long, and the
/// generator holds one real frame back to build the in-between pictures.
pub fn smart_latency_fg(shown_frame_ms: f64, fg: f64, wait_ms: f64, refresh_ms: f64) -> f64 {
    if fg <= 1.0 {
        return smart_latency(shown_frame_ms, wait_ms, refresh_ms);
    }
    let real = shown_frame_ms * fg;
    smart_latency(real, wait_ms, refresh_ms) + real
}

/// Counts the pictures a window (or monitor) hands to Windows Graphics Capture.
struct FpsProbe {
    target: isize,
    _item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    count: Arc<AtomicU32>,
    timing: Arc<Mutex<Timing>>,
}

impl FpsProbe {
    fn new(device: &IDirect3DDevice, hwnd: HWND, monitor: Option<HMONITOR>) -> windows::core::Result<Self> {
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem = unsafe {
            match monitor {
                Some(m) => interop.CreateForMonitor(m)?,
                None => interop.CreateForWindow(hwnd)?,
            }
        };
        let size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(device, DirectXPixelFormat::B8G8R8A8UIntNormalized, 1, size)?;
        let count = Arc::new(AtomicU32::new(0));
        let timing = Arc::new(Mutex::new(Timing::default()));
        {
            let (count, timing) = (count.clone(), timing.clone());
            pool.FrameArrived(&TypedEventHandler::new(move |p: &Option<Direct3D11CaptureFramePool>, _| {
                if let Some(p) = p {
                    if let Ok(f) = p.TryGetNextFrame() {
                        count.fetch_add(1, Ordering::Relaxed);
                        if let Ok(t) = f.SystemRelativeTime() {
                            timing.lock().push(t.Duration);
                        }
                        let _ = f.Close();
                    }
                }
                Ok(())
            }))?;
        }
        let session = pool.CreateCaptureSession(&item)?;
        let _ = session.SetIsCursorCaptureEnabled(false);
        let _ = session.SetIsBorderRequired(false);
        session.StartCapture()?;
        let target = monitor.map(|m| m.0 as isize).unwrap_or(hwnd.0 as isize);
        Ok(FpsProbe { target, _item: item, pool, session, count, timing })
    }
}

impl Drop for FpsProbe {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

struct Gauges {
    device: Option<IDirect3DDevice>,
    probe: Option<FpsProbe>,
    probe_at: Instant,
    last_count: u32,
    fps: Option<f32>,
    pdh: isize,
    gpu_counter: isize,
    cpu_prev: Option<(u64, u64)>,
    last: Instant,
    /// Process of the measured game (for the ping), and the ping thread's last result.
    game_pid: Arc<AtomicU32>,
    ping: Arc<Mutex<Option<f32>>>,
    ping_on: Arc<AtomicBool>,
}

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

impl Gauges {
    fn new() -> Self {
        let device = crate::gpu::Gpu::new().ok().and_then(|g| {
            let dxgi: IDXGIDevice = g.device.cast().ok()?;
            let insp = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi).ok()? };
            insp.cast::<IDirect3DDevice>().ok()
        });
        let (mut q, mut c) = (0isize, 0isize);
        unsafe {
            if PdhOpenQueryW(None, 0, &mut q) == 0 {
                if PdhAddEnglishCounterW(q, w!("\\GPU Engine(*engtype_3D)\\Utilization Percentage"), 0, &mut c) != 0 {
                    c = 0;
                }
                let _ = PdhCollectQueryData(q);
            } else {
                q = 0;
            }
        }
        let (game_pid, ping, ping_on) = (Arc::new(AtomicU32::new(0)), Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(false)));
        {
            let (game_pid, ping, ping_on) = (game_pid.clone(), ping.clone(), ping_on.clone());
            std::thread::Builder::new()
                .name("hud-ping".into())
                .spawn(move || loop {
                    if ping_on.load(Ordering::Relaxed) {
                        let pid = game_pid.load(Ordering::Relaxed);
                        *ping.lock() = if pid != 0 { game_ping(pid) } else { None };
                        std::thread::sleep(std::time::Duration::from_millis(2000));
                    } else {
                        *ping.lock() = None;
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                })
                .ok();
        }
        Gauges { device, probe: None, probe_at: Instant::now(), last_count: 0, fps: None, pdh: q, gpu_counter: c, cpu_prev: None, last: Instant::now(), game_pid, ping, ping_on }
    }

    /// Follows the foreground window: a game window is measured itself, the desktop by its monitor.
    fn follow_foreground(&mut self) {
        let Some(dev) = &self.device else { return };
        unsafe {
            let fg = GetForegroundWindow();
            if fg.is_invalid() {
                return;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(fg, Some(&mut pid));
            if pid == std::process::id() {
                return; // our own overlay or window: keep measuring the game behind it
            }
            let shell = crate::capture::sources::foreground_program().is_none();
            self.game_pid.store(if shell { 0 } else { pid }, Ordering::Relaxed);
            let monitor = if shell { Some(MonitorFromWindow(fg, MONITOR_DEFAULTTOPRIMARY)) } else { None };
            let target = monitor.map(|m| m.0 as isize).unwrap_or(fg.0 as isize);
            if self.probe.as_ref().map_or(false, |p| p.target == target) {
                return;
            }
            self.probe = None;
            self.fps = None;
            self.last_count = 0;
            self.probe_at = Instant::now();
            match FpsProbe::new(dev, fg, monitor) {
                Ok(p) => self.probe = Some(p),
                Err(e) => log::warn!("hud probe: {e}"),
            }
        }
    }

    fn sample(&mut self, want_ping: bool, _cfg: &HudConfig) -> Sample {
        self.ping_on.store(want_ping, Ordering::Relaxed);
        let dt = self.last.elapsed().as_secs_f32().max(0.05);
        self.last = Instant::now();
        self.follow_foreground();
        if let Some(p) = &self.probe {
            let n = p.count.load(Ordering::Relaxed);
            let fresh = (n.wrapping_sub(self.last_count)) as f32 / dt;
            self.last_count = n;
            if self.probe_at.elapsed().as_millis() > 400 {
                // Light smoothing so the number does not flicker.
                self.fps = Some(match self.fps {
                    Some(old) if (old - fresh).abs() < old * 0.5 => old * 0.4 + fresh * 0.6,
                    _ => fresh,
                });
            }
        }
        let mut gpu = None;
        unsafe {
            if self.pdh != 0 && self.gpu_counter != 0 && PdhCollectQueryData(self.pdh) == 0 {
                let (mut size, mut count) = (0u32, 0u32);
                let _ = PdhGetFormattedCounterArrayW(self.gpu_counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
                if size > 0 {
                    let mut buf = vec![0u8; size as usize];
                    let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
                    if PdhGetFormattedCounterArrayW(self.gpu_counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) == 0 {
                        let mut sum = 0.0f64;
                        for i in 0..count as usize {
                            let it = &*items.add(i);
                            if it.FmtValue.CStatus == 0 {
                                sum += it.FmtValue.Anonymous.doubleValue;
                            }
                        }
                        gpu = Some(sum.clamp(0.0, 100.0) as f32);
                    }
                }
            }
        }
        let mut cpu = None;
        unsafe {
            let (mut idle, mut kern, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
            if GetSystemTimes(Some(&mut idle), Some(&mut kern), Some(&mut user)).is_ok() {
                let total = ft(kern) + ft(user); // kernel time includes idle time
                let idle = ft(idle);
                if let Some((pt, pi)) = self.cpu_prev {
                    let (dt, di) = (total.saturating_sub(pt), idle.saturating_sub(pi));
                    if dt > 0 {
                        cpu = Some((100.0 * (1.0 - di as f64 / dt as f64)).clamp(0.0, 100.0) as f32);
                    }
                }
                self.cpu_prev = Some((total, idle));
            }
        }
        let mut ram = None;
        unsafe {
            let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
            if GlobalMemoryStatusEx(&mut m).is_ok() {
                ram = Some(m.dwMemoryLoad as f32);
            }
        }
        // Frame time and the wait for the screen over the last second of pictures.
        let (mut frame_ms, mut wait_ms, mut refresh_ms) = (None, None, 0.0);
        if let Some(p) = &self.probe {
            let t = p.timing.lock();
            let n = (self.fps.unwrap_or(60.0).round() as usize).clamp(8, Timing::KEEP);
            frame_ms = mean_tail(&t.intervals, n);
            wait_ms = mean_tail(&t.waits, n);
            refresh_ms = t.refresh_ms;
        }
        // A still picture sends nothing: no fresh frames, no frame time.
        if self.fps.map_or(true, |f| f < 1.0) {
            frame_ms = None;
        }
        // Exact FPS from the event trace (AfterCore's tracker): every frame the game presents,
        // also above the screen's refresh rate; with frame generation the game's own frames come
        // from its NVIDIA Reflex markers. Without administrator rights: the capture's count.
        let pid = self.game_pid.load(Ordering::Relaxed);
        let exact = if pid != 0 { crate::frames::stats(pid) } else { None };
        let mut fps = self.fps;
        let mut real_fps = None;
        let mut fg = 1.0f64;
        if let Some(st) = &exact {
            fps = Some(st.fps);
            if st.frametime_ms > 0.0 {
                frame_ms = Some(st.frametime_ms as f64);
            }
            if st.frame_generation() {
                if let Some(r) = st.real_fps {
                    real_fps = Some(r);
                    fg = (st.fps / r) as f64;
                }
            }
        }
        let latency = match (frame_ms, wait_ms) {
            (Some(f), Some(w)) => Some(smart_latency_fg(f, fg, w, refresh_ms) as f32),
            _ => None,
        };
        let display = wait_ms.filter(|_| frame_ms.is_some()).map(|w| (w + refresh_ms / 2.0) as f32);
        let ping = *self.ping.lock();
        Sample { fps, real_fps, frame_ms: frame_ms.map(|f| f as f32), latency, display, ping, gpu, cpu, ram }
    }

    fn stop_probe(&mut self) {
        self.probe = None;
        self.fps = None;
    }
}

impl Drop for Gauges {
    fn drop(&mut self) {
        if self.pdh != 0 {
            unsafe {
                let _ = PdhCloseQuery(self.pdh);
            }
        }
    }
}

struct Sample {
    fps: Option<f32>,
    /// FPS the game really renders when frame generation is on (shown grey next to the FPS).
    real_fps: Option<f32>,
    frame_ms: Option<f32>,
    latency: Option<f32>,
    display: Option<f32>,
    ping: Option<f32>,
    gpu: Option<f32>,
    cpu: Option<f32>,
    ram: Option<f32>,
}

/// The row's parts: label and value.
/// The row's parts: label, value and an optional grey value after it.
fn segments(s: &Sample, c: &HudConfig) -> Vec<(&'static str, String, Option<String>)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let dash = "-".to_string();
    let mut fps_extra = None;
    if c.fps {
        out.push(("FPS", s.fps.map(|f| format!("{:.0}", f)).unwrap_or(dash.clone())));
        // With frame generation: the shown FPS in the main colour, the real one grey after it.
        fps_extra = s.real_fps.map(|f| format!("{:.0}", f));
    }
    if c.latency {
        out.push(("LAT", s.latency.map(|v| format!("{:.1} ms", v)).unwrap_or(dash.clone())));
    }
    if c.display_latency {
        out.push(("DISP", s.display.map(|v| format!("{:.1} ms", v)).unwrap_or(dash.clone())));
    }
    if c.frame_time {
        out.push(("FT", s.frame_ms.map(|v| format!("{:.1} ms", v)).unwrap_or(dash.clone())));
    }
    if c.ping {
        out.push(("PING", s.ping.map(|v| format!("{:.0} ms", v)).unwrap_or(dash.clone())));
    }
    if c.gpu {
        out.push(("GPU", s.gpu.map(|v| format!("{:.0}%", v)).unwrap_or(dash.clone())));
    }
    if c.cpu {
        out.push(("CPU", s.cpu.map(|v| format!("{:.0}%", v)).unwrap_or(dash.clone())));
    }
    if c.ram {
        out.push(("RAM", s.ram.map(|v| format!("{:.0}%", v)).unwrap_or(dash.clone())));
    }
    if c.clock {
        out.push(("", crate::util::clock_hhmm()));
    }
    out.into_iter().map(|(l, v)| (l, v, if l == "FPS" { fps_extra.clone() } else { None })).collect()
}

// ------------------------------------------------------------------ window

struct State {
    hwnd: HWND,
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
    memdc: HDC,
    bitmap: HBITMAP,
    bmp_size: SIZE,
    gauges: Option<Gauges>,
    shown: bool,
    affinity_excluded: Option<bool>,
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
        WM_NCHITTEST => LRESULT(-1),
        WM_MOUSEACTIVATE => LRESULT(3),
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn run() {
    let _ = RoInitialize(RO_INIT_MULTITHREADED);
    let hinst = GetModuleHandleW(None).unwrap_or_default();
    let class = w!("InstantReplayHud");
    let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
    RegisterClassW(&wc);
    let ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    let Ok(hwnd) = CreateWindowExW(ex, class, w!(""), WS_POPUP, 0, 0, 1, 1, None, None, hinst, None) else { return };
    let Ok(d2d) = D2D1CreateFactory::<ID2D1Factory>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None) else { return };
    let Ok(dw) = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) else { return };
    let screen = GetDC(None);
    let memdc = CreateCompatibleDC(screen);
    ReleaseDC(None, screen);
    STATE = Some(State { hwnd, d2d, dw, memdc, bitmap: HBITMAP::default(), bmp_size: SIZE::default(), gauges: None, shown: false, affinity_excluded: None });
    SetTimer(hwnd, TIMER_ID, TICK_MS, None);
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

unsafe fn tick() {
    let Some(st) = state() else { return };
    if !ENABLED.load(Ordering::SeqCst) {
        if st.shown {
            let _ = ShowWindow(st.hwnd, SW_HIDE);
            st.shown = false;
        }
        if let Some(g) = &mut st.gauges {
            g.stop_probe(); // no capture session while the counter is off
        }
        crate::frames::stop();
        return;
    }
    let cfg = CFG.get().map(|m| m.lock().clone());
    let Some(cfg) = cfg else { return };
    let exclude = !cfg.in_capture;
    if st.affinity_excluded != Some(exclude) {
        let _ = SetWindowDisplayAffinity(st.hwnd, if exclude { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE });
        st.affinity_excluded = Some(exclude);
    }
    let g = st.gauges.get_or_insert_with(Gauges::new);
    if !(cfg.fps || cfg.frame_time || cfg.latency || cfg.display_latency) {
        g.stop_probe();
    }
    if cfg.fps || cfg.frame_time || cfg.latency {
        crate::frames::ensure_running();
    }
    let sample = g.sample(cfg.ping, &cfg);
    let segs = segments(&sample, &cfg);
    if segs.is_empty() {
        if st.shown {
            let _ = ShowWindow(st.hwnd, SW_HIDE);
            st.shown = false;
        }
        return;
    }
    // Corner of the monitor the game is on.
    let fg = GetForegroundWindow();
    let mon = MonitorFromWindow(fg, MONITOR_DEFAULTTOPRIMARY);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = GetMonitorInfoW(mon, &mut mi);
    let (mut dx, mut dy) = (96u32, 96u32);
    let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    let s = dx.max(96) as f32 / 96.0;
    let Ok((w, h)) = render(st, &segs, s) else { return };
    let r = mi.rcMonitor;
    let m = (16.0 * s) as i32;
    let x = if cfg.corner % 2 == 0 { r.left + m } else { r.right - w - m };
    let y = if cfg.corner < 2 { r.top + m } else { r.bottom - h - m };
    let pos = POINT { x, y };
    let size = SIZE { cx: w, cy: h };
    let src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
    let screen = GetDC(None);
    let _ = UpdateLayeredWindow(st.hwnd, screen, Some(&pos), Some(&size), st.memdc, Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
    ReleaseDC(None, screen);
    if !st.shown {
        let _ = SetWindowPos(st.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        st.shown = true;
    } else {
        // Stay above a game that keeps putting itself on top.
        let _ = SetWindowPos(st.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

fn color(hex: u32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: ((hex >> 16) & 0xff) as f32 / 255.0, g: ((hex >> 8) & 0xff) as f32 / 255.0, b: (hex & 0xff) as f32 / 255.0, a }
}

unsafe fn text_width(dw: &IDWriteFactory, text: &[u16], fmt: &IDWriteTextFormat) -> f32 {
    match dw.CreateTextLayout(text, fmt, 4000.0, 100.0) {
        Ok(l) => {
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = l.GetMetrics(&mut m);
            m.widthIncludingTrailingWhitespace
        }
        Err(_) => 0.0,
    }
}

/// Draws the row into the window bitmap; returns its size in pixels.
unsafe fn render(st: &mut State, segs: &[(&'static str, String, Option<String>)], s: f32) -> windows::core::Result<(i32, i32)> {
    let fam = w!("Segoe UI Variable Text");
    let label_fmt = st.dw.CreateTextFormat(fam, None, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 11.0 * s, w!("en-us"))?;
    let value_fmt = st.dw.CreateTextFormat(fam, None, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 14.0 * s, w!("en-us"))?;
    for f in [&label_fmt, &value_fmt] {
        let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
    }
    let pad = 12.0 * s;
    let gap = 6.0 * s; // label to value
    let sep = 21.0 * s; // between segments, the divider in the middle
    // Values get a fixed minimum width so the row does not jump while numbers change.
    let digit = text_width(&st.dw, &"0".encode_utf16().collect::<Vec<_>>(), &value_fmt);
    let mut parts = Vec::new();
    let mut total = pad * 2.0;
    for (i, (label, value, extra)) in segs.iter().enumerate() {
        let l: Vec<u16> = label.encode_utf16().collect();
        let v: Vec<u16> = value.encode_utf16().collect();
        let lw = if label.is_empty() { 0.0 } else { text_width(&st.dw, &l, &label_fmt) };
        let vw = text_width(&st.dw, &v, &value_fmt).max(digit * value.chars().count().max(3) as f32 * 0.98);
        let e: Vec<u16> = extra.as_deref().unwrap_or("").encode_utf16().collect();
        let ew = if e.is_empty() { 0.0 } else { text_width(&st.dw, &e, &value_fmt).max(digit * e.len().max(2) as f32 * 0.98) };
        let w = lw + if label.is_empty() { 0.0 } else { gap } + vw + if e.is_empty() { 0.0 } else { gap + ew };
        if i > 0 {
            total += sep;
        }
        total += w;
        parts.push((l, v, lw, vw, e, ew));
    }
    let (w, h) = (total.ceil() as i32, (30.0 * s).ceil() as i32);
    if st.bmp_size.cx != w || st.bmp_size.cy != h || st.bitmap.is_invalid() {
        if !st.bitmap.is_invalid() {
            let _ = DeleteObject(st.bitmap);
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        st.bitmap = CreateDIBSection(st.memdc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
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
    rt.BindDC(st.memdc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
    rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
    rt.BeginDraw();
    rt.Clear(Some(&color(0, 0.0)));
    let inset = 0.5 * s;
    let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: inset, top: inset, right: w as f32 - inset, bottom: h as f32 - inset }, radiusX: h as f32 / 2.0, radiusY: h as f32 / 2.0 };
    // surface-raised, a little see-through, with the toast's hairline (DESIGN.md: hud)
    let bg = rt.CreateSolidColorBrush(&color(0x1d1d1f, 0.82), None)?;
    let line = rt.CreateSolidColorBrush(&color(0xffffff, 0.10), None)?;
    rt.FillRoundedRectangle(&rr, &bg);
    rt.DrawRoundedRectangle(&rr, &line, 1.0 * s, None);
    let label_brush = rt.CreateSolidColorBrush(&color(0xa1a1a6, 1.0), None)?;
    let value_brush = rt.CreateSolidColorBrush(&color(0xf5f5f7, 1.0), None)?;
    let div = rt.CreateSolidColorBrush(&color(0xffffff, 0.16), None)?;
    let mut x = pad;
    for (i, (l, v, lw, vw, e, ew)) in parts.iter().enumerate() {
        if i > 0 {
            let cx = x + sep / 2.0;
            rt.DrawLine(D2D_POINT_2F { x: cx, y: 9.0 * s }, D2D_POINT_2F { x: cx, y: h as f32 - 9.0 * s }, &div, 1.0 * s, None);
            x += sep;
        }
        if !l.is_empty() {
            let r = D2D_RECT_F { left: x, top: 0.0, right: x + lw + 1.0, bottom: h as f32 + 1.0 * s };
            rt.DrawText(l, &label_fmt, &r, &label_brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            x += lw + gap;
        }
        let r = D2D_RECT_F { left: x, top: 0.0, right: x + vw + 1.0, bottom: h as f32 };
        rt.DrawText(v, &value_fmt, &r, &value_brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        x += vw;
        if !e.is_empty() {
            x += gap;
            let r = D2D_RECT_F { left: x, top: 0.0, right: x + ew + 1.0, bottom: h as f32 };
            rt.DrawText(e, &value_fmt, &r, &label_brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            x += ew;
        }
    }
    rt.EndDraw(None, None)?;
    Ok((w, h))
}

/// Round trip to the game's server: the remote ends of the game's TCP connections come from the
/// system connection table (no handle to the game), the first one that answers an ICMP echo wins.
fn game_ping(pid: u32) -> Option<f32> {
    use windows::Win32::NetworkManagement::IpHelper::*;
    let mut remotes: Vec<(u32, u16)> = Vec::new();
    unsafe {
        let mut size = 0u32;
        let _ = GetExtendedTcpTable(None, &mut size, false, 2, TCP_TABLE_OWNER_PID_CONNECTIONS, 0);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize + 1024];
        size = buf.len() as u32;
        if GetExtendedTcpTable(Some(buf.as_mut_ptr() as _), &mut size, false, 2, TCP_TABLE_OWNER_PID_CONNECTIONS, 0) != 0 {
            return None;
        }
        let table = &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
        let rows = std::slice::from_raw_parts(table.table.as_ptr(), table.dwNumEntries as usize);
        for r in rows {
            // 5 = established; skip the local machine.
            let ip = r.dwRemoteAddr;
            if r.dwOwningPid == pid && r.dwState == 5 && (ip & 0xff) != 127 && ip != 0 {
                let port = u16::from_be((r.dwRemotePort & 0xffff) as u16);
                if !remotes.iter().any(|(a, _)| *a == ip) {
                    remotes.push((ip, port));
                }
            }
        }
    }
    // Game traffic first: web ports (store, chat, telemetry) last.
    remotes.sort_by_key(|(_, port)| matches!(port, 80 | 443) as u8);
    let icmp = unsafe { IcmpCreateFile().ok()? };
    let mut best = None;
    for (ip, _) in remotes.into_iter().take(3) {
        let data = [0u8; 32];
        let mut reply = vec![0u8; std::mem::size_of::<ICMP_ECHO_REPLY>() + 64];
        let n = unsafe { IcmpSendEcho(icmp, ip, data.as_ptr() as _, data.len() as u16, None, reply.as_mut_ptr() as _, reply.len() as u32, 800) };
        if n > 0 {
            let r = unsafe { &*(reply.as_ptr() as *const ICMP_ECHO_REPLY) };
            if r.Status == 0 {
                best = Some(r.RoundTripTime as f32);
                break;
            }
        }
    }
    unsafe {
        let _ = IcmpCloseHandle(icmp);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_latency_adds_the_measured_parts() {
        // 120 fps (8.33 ms), 3 ms to the vblank, 144 Hz screen (6.94 ms)
        let l = smart_latency(8.33, 3.0, 6.94);
        assert!((l - (16.66 + 3.0 + 3.47)).abs() < 0.01);
    }

    #[test]
    fn frame_generation_counts_real_frames() {
        // 240 shown fps with 2x frame generation = 120 real fps (8.33 ms real frames).
        let shown = 1000.0 / 240.0;
        let l = smart_latency_fg(shown, 2.0, 1.0, 4.17);
        let real = shown * 2.0;
        assert!((l - (2.0 * real + 1.0 + 2.085 + real)).abs() < 0.01);
        assert_eq!(smart_latency_fg(shown, 1.0, 1.0, 4.17), smart_latency(shown, 1.0, 4.17));
    }

    #[test]
    fn timing_keeps_intervals_in_ms() {
        let mut t = Timing::default();
        t.push(0);
        t.push(83_333); // 8.33 ms later
        t.push(166_666);
        let m = mean_tail(&t.intervals, 10).unwrap();
        assert!((m - 8.3333).abs() < 0.01);
    }
}
