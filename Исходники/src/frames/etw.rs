//! Taken from AfterCore (ac-telemetry/src/etw.rs); changed: the module path and the session name.
//! Minimal real-time ETW consumer for present events.
//!
//! Providers (IDs verified against the manifests registered on Windows 11):
//! * Microsoft-Windows-DXGI    `CA11C036-…`  event 42 `Present_Start`, 55 `PresentMultiplaneOverlay_Start`
//! * Microsoft-Windows-D3D9    `783ACA0A-…`  event 1  `Present_Start`
//! * Microsoft-Windows-DxgKrnl `802EC45A-…`  event 215 `PresentHistoryDetailed_Start`, 171 `PresentHistory_Start`,
//!   184 `Present`, 168 `Flip` - kernel side, covers Vulkan / OpenGL / anything else.
//!
//! The session uses the QPC clock and raw timestamps so frametimes have sub-microsecond precision.
//! A helper thread forces buffer flushes every 100 ms so the overlay is near real time.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use windows_sys::core::GUID;
use windows_sys::Win32::System::Diagnostics::Etw::*;

use super::tracker::{FrameTracker, Source};

// Own session name: AfterCore may run at the same time with its "AfterCore-FrameTrace".
pub const SESSION_NAME: &str = "InstantReplay-FrameTrace";

const DXGI: GUID = GUID::from_u128(0xCA11C036_0102_4A2D_A6AD_F03CFED5D3C9);
const D3D9: GUID = GUID::from_u128(0x783ACA0A_790E_4D7F_8451_AA850511C6B9);
const DXGKRNL: GUID = GUID::from_u128(0x802EC45A_1E99_4B83_9920_87C98277BA9D);
/// NVIDIA Reflex "PCLStatsTraceLoggingProvider" - GUID read from the TraceLogging provider
/// metadata embedded in Streamline's sl.pcl.dll. Games with Reflex emit one marker set per
/// game frame, which is what lets us tell real frames from generated ones.
const PCL: GUID = GUID::from_u128(0x0D216F06_82A6_4D49_BC4F_8F38AE56EFAB);

const KW_DXGI_EVENTS: u64 = 0x2;
const KW_D3D9_EVENTS: u64 = 0x2;
const KW_DXGKRNL_BASE: u64 = 0x1;
const KW_DXGKRNL_PRESENT: u64 = 0x800_0000;

const DXGI_PRESENT_TEST: u32 = 0x1;

const EVENT_HEADER_FLAG_32_BIT: u16 = 0x0020;

#[repr(C)]
struct Props {
    p: EVENT_TRACE_PROPERTIES,
    name: [u16; 128],
}

impl Props {
    fn new() -> Box<Self> {
        let mut b: Box<Props> = Box::new(unsafe { std::mem::zeroed() });
        b.p.Wnode.BufferSize = std::mem::size_of::<Props>() as u32;
        b.p.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        b.p.Wnode.ClientContext = 1; // QPC clock
        b.p.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
        b.p.LoggerNameOffset = std::mem::size_of::<EVENT_TRACE_PROPERTIES>() as u32;
        b.p.BufferSize = 16; // KB - small buffers = low latency
        b.p.MinimumBuffers = 16;
        b.p.MaximumBuffers = 128;
        b.p.FlushTimer = 1;
        b
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn guid_eq(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

struct Ctx {
    tracker: Arc<FrameTracker>,
}

unsafe extern "system" fn on_event(rec: *mut EVENT_RECORD) {
    let r = &*rec;
    if r.UserContext.is_null() {
        return;
    }
    let ctx = &*(r.UserContext as *const Ctx);
    let h = &r.EventHeader;
    let id = h.EventDescriptor.Id;
    let data = if r.UserData.is_null() {
        &[][..]
    } else {
        std::slice::from_raw_parts(r.UserData as *const u8, r.UserDataLength as usize)
    };
    let ptr_size = if h.Flags & EVENT_HEADER_FLAG_32_BIT != 0 { 4 } else { 8 };
    // First field of DXGI / D3D9 present events is the swapchain pointer.
    let swapchain = || -> u64 {
        match ptr_size {
            4 if data.len() >= 4 => u32::from_le_bytes(data[..4].try_into().unwrap()) as u64,
            8 if data.len() >= 8 => u64::from_le_bytes(data[..8].try_into().unwrap()),
            _ => 0,
        }
    };

    if guid_eq(&h.ProviderId, &PCL) {
        // TraceLogging "PCLStatsEvent" { UInt32 Marker; UInt64 FrameID } (schema read from sl.pcl.dll)
        if data.len() >= 12 && tl_event_name(r).is_none_or(|n| n == b"PCLStatsEvent") {
            let marker = u32::from_le_bytes(data[..4].try_into().unwrap());
            let frame = u64::from_le_bytes(data[4..12].try_into().unwrap());
            ctx.tracker.on_pcl_marker(h.ProcessId, marker, frame, h.TimeStamp);
        }
        return;
    }

    let (src, sc) = if guid_eq(&h.ProviderId, &DXGI) {
        match id {
            42 => {
                // Present_Start { void* pSwapChain; u32 Flags; i32 SyncInterval }
                if data.len() >= ptr_size + 4 {
                    let flags = u32::from_le_bytes(data[ptr_size..ptr_size + 4].try_into().unwrap());
                    if flags & DXGI_PRESENT_TEST != 0 {
                        return;
                    }
                }
                (Source::Dxgi, swapchain())
            }
            55 => (Source::Dxgi, swapchain()),
            _ => return,
        }
    } else if guid_eq(&h.ProviderId, &D3D9) {
        if id != 1 {
            return;
        }
        (Source::D3d9, swapchain())
    } else if guid_eq(&h.ProviderId, &DXGKRNL) {
        let s = match id {
            215 => Source::KernelHistoryDetailed,
            171 => Source::KernelHistory,
            184 => Source::KernelPresent,
            168 => Source::KernelFlip,
            _ => return,
        };
        (s, 0)
    } else {
        return;
    };
    ctx.tracker.on_present(h.ProcessId, src, sc, h.TimeStamp);
}

/// Event name from the TraceLogging schema extended data item, if present.
/// Layout: u16 size, tag bytes (high bit = more), NUL-terminated UTF-8 name, fields…
unsafe fn tl_event_name(r: &EVENT_RECORD) -> Option<&[u8]> {
    const EXT_SCHEMA_TL: u16 = 11;
    if r.ExtendedData.is_null() {
        return None;
    }
    for i in 0..r.ExtendedDataCount as usize {
        let item = &*r.ExtendedData.add(i);
        if item.ExtType != EXT_SCHEMA_TL || item.DataPtr == 0 {
            continue;
        }
        let d = std::slice::from_raw_parts(item.DataPtr as *const u8, item.DataSize as usize);
        let mut p = 2;
        while p < d.len() && d[p] & 0x80 != 0 {
            p += 1;
        }
        p += 1;
        let end = d[p.min(d.len())..].iter().position(|&b| b == 0)? + p;
        return Some(&d[p..end]);
    }
    None
}

#[derive(Clone, Debug, PartialEq)]
pub enum EtwState {
    Stopped,
    Running { providers: Vec<&'static str> },
    NeedsAdmin,
    Failed(String),
}

pub struct EtwSession {
    props: Box<Props>,
    handle: CONTROLTRACE_HANDLE,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    _ctx: Box<Ctx>,
    pub providers: Vec<&'static str>,
}

unsafe impl Send for EtwSession {}

/// Stop a session left over by a crashed previous run.
pub fn stop_stale_session() {
    let mut props = Props::new();
    let name = wide(SESSION_NAME);
    unsafe {
        ControlTraceW(std::mem::zeroed(), name.as_ptr(), &mut props.p, EVENT_TRACE_CONTROL_STOP);
    }
}

fn enable(
    handle: CONTROLTRACE_HANDLE,
    guid: &GUID,
    any: u64,
    ids: &[u16],
) -> u32 {
    // EVENT_FILTER_EVENT_ID { BOOLEAN FilterIn; UCHAR Reserved; USHORT Count; USHORT Events[] }
    let mut filter = Vec::<u8>::with_capacity(4 + ids.len() * 2);
    filter.push(1);
    filter.push(0);
    filter.extend_from_slice(&(ids.len() as u16).to_le_bytes());
    for id in ids {
        filter.extend_from_slice(&id.to_le_bytes());
    }
    let mut desc = EVENT_FILTER_DESCRIPTOR {
        Ptr: filter.as_ptr() as u64,
        Size: filter.len() as u32,
        Type: EVENT_FILTER_TYPE_EVENT_ID,
    };
    let mut params: ENABLE_TRACE_PARAMETERS = unsafe { std::mem::zeroed() };
    params.Version = ENABLE_TRACE_PARAMETERS_VERSION_2;
    params.EnableFilterDesc = &mut desc;
    params.FilterDescCount = 1;
    let rc = unsafe {
        EnableTraceEx2(
            handle,
            guid,
            EVENT_CONTROL_CODE_ENABLE_PROVIDER,
            TRACE_LEVEL_VERBOSE as u8,
            any,
            0,
            0,
            &params,
        )
    };
    if rc == 0 {
        return 0;
    }
    // Older systems without event-id filtering: enable unfiltered.
    unsafe {
        EnableTraceEx2(
            handle,
            guid,
            EVENT_CONTROL_CODE_ENABLE_PROVIDER,
            TRACE_LEVEL_VERBOSE as u8,
            any,
            0,
            0,
            std::ptr::null(),
        )
    }
}

impl EtwSession {
    pub fn start(tracker: Arc<FrameTracker>) -> Result<Self, EtwState> {
        stop_stale_session();
        let mut props = Props::new();
        let name = wide(SESSION_NAME);
        let mut handle: CONTROLTRACE_HANDLE = unsafe { std::mem::zeroed() };
        let rc = unsafe { StartTraceW(&mut handle, name.as_ptr(), &mut props.p) };
        match rc {
            0 => {}
            5 => return Err(EtwState::NeedsAdmin),
            other => {
                return Err(EtwState::Failed(format!(
                    "StartTrace: {}",
                    std::io::Error::from_raw_os_error(other as i32)
                )))
            }
        }

        let mut providers = Vec::new();
        if enable(handle, &DXGI, KW_DXGI_EVENTS, &[42, 55]) == 0 {
            providers.push("DXGI");
        }
        if enable(handle, &D3D9, KW_D3D9_EVENTS, &[1]) == 0 {
            providers.push("D3D9");
        }
        if enable(handle, &DXGKRNL, KW_DXGKRNL_BASE | KW_DXGKRNL_PRESENT, &[215, 171, 184, 168]) == 0 {
            providers.push("DxgKrnl");
        }
        // TraceLogging events all use id 0, so no event-id filter here.
        let rc = unsafe {
            EnableTraceEx2(handle, &PCL, EVENT_CONTROL_CODE_ENABLE_PROVIDER, TRACE_LEVEL_VERBOSE as u8, u64::MAX, 0, 0, std::ptr::null())
        };
        if rc == 0 {
            providers.push("Reflex PCL");
        }
        if providers.is_empty() {
            unsafe { ControlTraceW(handle, std::ptr::null(), &mut props.p, EVENT_TRACE_CONTROL_STOP) };
            return Err(EtwState::Failed("ни один провайдер не включился".into()));
        }

        let ctx = Box::new(Ctx { tracker });
        let mut logname = wide(SESSION_NAME);
        let mut log: EVENT_TRACE_LOGFILEW = unsafe { std::mem::zeroed() };
        log.LoggerName = logname.as_mut_ptr();
        log.Anonymous1.ProcessTraceMode =
            PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD | PROCESS_TRACE_MODE_RAW_TIMESTAMP;
        log.Anonymous2.EventRecordCallback = Some(on_event);
        log.Context = (&*ctx as *const Ctx) as *mut _;
        let trace = unsafe { OpenTraceW(&mut log) };
        if trace.Value == u64::MAX {
            let err = std::io::Error::last_os_error();
            unsafe { ControlTraceW(handle, std::ptr::null(), &mut props.p, EVENT_TRACE_CONTROL_STOP) };
            return Err(EtwState::Failed(format!("OpenTrace: {err}")));
        }

        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();

        let trace_value = trace.Value;
        threads.push(
            std::thread::Builder::new()
                .name("etw-consumer".into())
                .spawn(move || unsafe {
                    let t = PROCESSTRACE_HANDLE { Value: trace_value };
                    ProcessTrace(&t, 1, std::ptr::null(), std::ptr::null());
                    CloseTrace(t);
                })
                .expect("spawn etw thread"),
        );

        // Flusher: pushes partially filled buffers to us every 100 ms.
        {
            let stop = stop.clone();
            let handle_value = handle.Value;
            threads.push(
                std::thread::Builder::new()
                    .name("etw-flush".into())
                    .spawn(move || {
                        let mut p = Props::new();
                        let h = CONTROLTRACE_HANDLE { Value: handle_value };
                        while !stop.load(Ordering::Relaxed) {
                            std::thread::sleep(std::time::Duration::from_millis(100));
                            unsafe { ControlTraceW(h, std::ptr::null(), &mut p.p, EVENT_TRACE_CONTROL_FLUSH) };
                        }
                    })
                    .expect("spawn flush thread"),
            );
        }

        Ok(Self { props, handle, stop, threads, _ctx: ctx, providers })
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        unsafe { ControlTraceW(self.handle, std::ptr::null(), &mut self.props.p, EVENT_TRACE_CONTROL_STOP) };
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for EtwSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}
