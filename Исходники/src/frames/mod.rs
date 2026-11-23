//! Exact frame rate from the Windows event trace (ETW), the AfterCore implementation: every
//! present of every program, also above the screen's refresh rate, per swapchain; the game's own
//! frames from NVIDIA Reflex markers tell real frames from generated ones. Needs administrator
//! rights (the program restarts itself elevated, see `elevation`). Nothing touches the game.

pub mod etw;
pub mod tracker;

use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

pub use tracker::{FrameStats, FrameTracker};

struct Trace {
    tracker: Arc<FrameTracker>,
    session: Option<etw::EtwSession>,
    state: Option<etw::EtwState>,
}

static TRACE: OnceLock<Mutex<Trace>> = OnceLock::new();

fn trace() -> &'static Mutex<Trace> {
    TRACE.get_or_init(|| Mutex::new(Trace { tracker: Arc::new(FrameTracker::new()), session: None, state: None }))
}

/// Starts the trace session once (needs administrator rights). True while it runs.
pub fn ensure_running() -> bool {
    let mut t = trace().lock();
    if t.session.is_some() {
        return true;
    }
    if t.state.is_some() {
        return false; // tried and failed (no rights): do not retry every tick
    }
    match etw::EtwSession::start(t.tracker.clone()) {
        Ok(s) => {
            log::info!("frame trace: {:?}", s.providers);
            t.session = Some(s);
            t.state = Some(etw::EtwState::Stopped);
            true
        }
        Err(e) => {
            log::warn!("frame trace: {e:?}");
            t.state = Some(e);
            false
        }
    }
}

/// Stops the trace (the counter was turned off).
pub fn stop() {
    let mut t = trace().lock();
    if let Some(s) = t.session.take() {
        s.stop();
    }
    t.state = None;
}

/// Frame statistics of `pid` over the last two seconds, when the trace runs and it presents.
pub fn stats(pid: u32) -> Option<FrameStats> {
    let tracker = {
        let t = trace().lock();
        t.session.as_ref()?;
        t.tracker.clone()
    };
    tracker.prune();
    tracker.stats(pid, 2.0, 0).filter(|s| s.presenting && s.fps > 0.0)
}
