//! Crash diagnostics: an access violation (or another hard fault) is written to log.txt with the
//! thread, the faulting address relative to its module, and the return addresses inside our exe
//! found on the stack (offsets; a linker map of the same build turns them into function names).

use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::System::Diagnostics::Debug::{AddVectoredExceptionHandler, EXCEPTION_POINTERS};

static LOGGED: AtomicU32 = AtomicU32::new(0);

fn image_range(base: usize) -> (usize, usize) {
    unsafe {
        let pe = *((base + 0x3c) as *const u32) as usize;
        let size = *((base + pe + 24 + 56) as *const u32) as usize; // OptionalHeader.SizeOfImage
        (base, base + size)
    }
}

fn module_of(addr: usize) -> String {
    use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT};
    unsafe {
        let mut h = windows::Win32::Foundation::HMODULE::default();
        if GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, windows::core::PCWSTR(addr as *const u16), &mut h).is_err() {
            return format!("{addr:#x}");
        }
        let mut buf = [0u16; 260];
        let n = GetModuleFileNameW(h, &mut buf) as usize;
        let name = String::from_utf16_lossy(&buf[..n]);
        let name = name.rsplit('\\').next().unwrap_or(&name).to_string();
        format!("{name}+{:#x}", addr - h.0 as usize)
    }
}

unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    const CONTINUE_SEARCH: i32 = 0;
    let rec = &*(*info).ExceptionRecord;
    let code = rec.ExceptionCode.0 as u32;
    // Hard faults only (access violation, heap corruption, stack overflow, illegal instruction).
    if !matches!(code, 0xC000_0005 | 0xC000_0374 | 0xC000_00FD | 0xC000_001D | 0xC000_0409) {
        return CONTINUE_SEARCH;
    }
    if LOGGED.fetch_add(1, Ordering::SeqCst) >= 8 {
        return CONTINUE_SEARCH;
    }
    let ctx = &*(*info).ContextRecord;
    let our = image_range(windows::Win32::System::LibraryLoader::GetModuleHandleW(None).map(|h| h.0 as usize).unwrap_or(0));
    let mut frames = Vec::new();
    let rsp = ctx.Rsp as usize;
    // Only within this thread's stack (reading past its top would fault in here).
    let (mut low, mut high) = (0usize, 0usize);
    windows::Win32::System::Threading::GetCurrentThreadStackLimits(&mut low, &mut high);
    let top = if high > rsp { high.min(rsp + 64 * 1024) } else { rsp };
    let words = top.saturating_sub(rsp) / 8;
    for i in 0..words {
        let p = rsp + i * 8;
        let v = std::ptr::read_volatile(p as *const usize);
        if v > our.0 && v < our.1 {
            frames.push(format!("{:#x}", v - our.0));
            if frames.len() >= 40 {
                break;
            }
        }
    }
    let thread = std::thread::current().name().map(|s| s.to_string()).unwrap_or_else(|| "?".into());
    log::error!(
        "CRASH code={code:#x} thread={thread} at={} rip={} info=[{:#x} {:#x}] stack(exe offsets)={}",
        module_of(rec.ExceptionAddress as usize),
        module_of(ctx.Rip as usize),
        rec.ExceptionInformation[0],
        rec.ExceptionInformation[1],
        frames.join(" ")
    );
    log::logger().flush();
    CONTINUE_SEARCH
}

pub fn install() {
    unsafe {
        AddVectoredExceptionHandler(1, Some(handler));
    }
}
