//! The vsync grid the compositor is paced against.
//!
//! A free-running grid of ticks at the refresh interval, declared to SteamVR a fixed lead ahead of
//! each tick, with Present held until the tick after the one just declared. The grid does not
//! follow the frames: a frame that overran costs one frame rather than moving the schedule the
//! compositor hands the application.
//!
//! It lives here rather than in the driver because three separate places in the driver read or
//! write it, and a second copy of the rules on another platform would drift from this one.

use alvr_common::{error, parking_lot::Mutex};
use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// How far ahead of a tick the vsync is declared. A fixed time rather than a fraction of the
/// frame: declaring no headroom advertises none, and heavy applications fall to half the refresh
/// rate, while a lead inside the running start band Valve documents for direct mode drivers holds
/// at any refresh rate.
const RUNNING_START: Duration = Duration::from_micros(2000);

/// The refresh interval, in nanoseconds, kept where the announcer can read it without crossing
/// back into the driver on every tick. Written wherever the driver's settings are built, so it
/// tracks a refresh rate change the same way reading the setting each tick did.
static FRAME_INTERVAL_NS: AtomicU64 = AtomicU64::new(0);

static ANNOUNCER_EXIT: AtomicBool = AtomicBool::new(false);
static ANNOUNCER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static GRID: Mutex<Grid> = Mutex::new(Grid::EMPTY);

struct Grid {
    /// The next tick. None until the first one is placed, which is one interval after the grid
    /// starts running.
    next_vsync: Option<Instant>,
    /// The tick most recently declared. Present is paced from this rather than from the next tick,
    /// so shedding a tick does not lengthen the hold.
    last_tick: Option<Instant>,
    /// Ticks the grid passed without declaring, because the announcer did not run in time.
    skipped_vsyncs: u64,
}

impl Grid {
    const EMPTY: Self = Self {
        next_vsync: None,
        last_tick: None,
        skipped_vsyncs: 0,
    };
}

fn frame_interval() -> Duration {
    Duration::from_nanos(FRAME_INTERVAL_NS.load(Ordering::Relaxed))
}

/// Called wherever the driver's settings are built, so the grid runs at the rate the driver was
/// told about.
pub fn set_refresh_rate(refresh_rate: f32) {
    if refresh_rate <= 0. {
        return;
    }

    FRAME_INTERVAL_NS.store((1e9 / refresh_rate as f64) as u64, Ordering::Relaxed);
}

fn announcer_loop() {
    {
        let mut grid = GRID.lock();
        *grid = Grid::EMPTY;
        grid.next_vsync = Some(Instant::now() + frame_interval());
    }

    while !ANNOUNCER_EXIT.load(Ordering::Relaxed) {
        let interval = frame_interval();

        let Some(next) = GRID.lock().next_vsync else {
            break;
        };

        // Wake a lead before the tick and declare the vsync at that announce point, so releasing
        // WaitGetPoses and predicting poses both start ahead of the real deadline.
        let announce = next - RUNNING_START;
        let before = Instant::now();
        if announce > before {
            thread::sleep(announce - before);
        }

        if ANNOUNCER_EXIT.load(Ordering::Relaxed) {
            break;
        }

        // Signed, and negative once the sleep overshoots, which reads as the vsync having just
        // passed. That is what absorbs wake jitter instead of letting it accumulate.
        let now = Instant::now();
        let offset = if announce >= now {
            (announce - now).as_secs_f64()
        } else {
            -((now - announce).as_secs_f64())
        };

        unsafe { crate::SendVSync(offset) };

        // Advance along the grid, losing whole ticks that were missed rather than accumulating
        // debt and snapping.
        let mut grid = GRID.lock();
        grid.last_tick = Some(next);

        let mut next = next + interval;
        let now = Instant::now();
        while next <= now {
            next += interval;
            grid.skipped_vsyncs += 1;
        }
        grid.next_vsync = Some(next);
    }

    *GRID.lock() = Grid::EMPTY;
}

#[unsafe(export_name = "StartVsyncAnnouncer")]
extern "C" fn start_vsync_announcer() {
    let mut announcer = ANNOUNCER.lock();
    if announcer.is_some() {
        return;
    }

    if FRAME_INTERVAL_NS.load(Ordering::Relaxed) == 0 {
        error!("Vsync announcer started before a refresh rate was set; not pacing.");
        return;
    }

    ANNOUNCER_EXIT.store(false, Ordering::Relaxed);
    *announcer = Some(thread::spawn(announcer_loop));
}

#[unsafe(export_name = "StopVsyncAnnouncer")]
extern "C" fn stop_vsync_announcer() {
    ANNOUNCER_EXIT.store(true, Ordering::Relaxed);

    let handle = ANNOUNCER.lock().take();
    if let Some(handle) = handle {
        handle.join().ok();
    }
}

/// Holds the presenting thread until the tick after the one just declared, plus whatever extra
/// frames SteamVR asked to be throttled by.
///
/// Pacing here rather than owning the vsync declaration is what keeps a slow frame costing one
/// frame: the announcer fires on the grid whatever happens on this thread. Without the hold the
/// compositor free-runs at encode speed and floods the client's decoder.
#[unsafe(export_name = "PaceAfterPresent")]
extern "C" fn pace_after_present(throttle_frames: u32) {
    let interval = frame_interval();

    let Some(last_tick) = GRID.lock().last_tick else {
        // No tick has been declared yet, so there is no grid position to measure from.
        thread::sleep(interval);
        return;
    };

    let until = last_tick + interval * (1 + throttle_frames);
    let now = Instant::now();
    if until > now {
        thread::sleep(until - now);
    }
}

/// Reads and clears the skipped tick count.
#[unsafe(export_name = "TakeSkippedVsyncs")]
extern "C" fn take_skipped_vsyncs() -> u32 {
    let mut grid = GRID.lock();
    let skipped = grid.skipped_vsyncs;
    grid.skipped_vsyncs = 0;

    skipped.min(u32::MAX as u64) as u32
}

// KiraVR: What happened to each frame handed to the encoder. The Linux encode
// mailbox keeps only the newest frame, so when encoding falls behind (a high
// refresh rate or resolution), frames are dropped there without a trace. This
// counts them for the statistics and logs a summary at most every 10 s.
const ENCODE_QUEUED: u32 = 0;
const ENCODE_REPLACED: u32 = 1;
const ENCODE_TOO_OLD: u32 = 2;
const DROP_LOG_INTERVAL: Duration = Duration::from_secs(10);

static ENCODE_JOBS: AtomicU64 = AtomicU64::new(0);
static ENCODE_REPLACED_COUNT: AtomicU64 = AtomicU64::new(0);
static ENCODE_TOO_OLD_COUNT: AtomicU64 = AtomicU64::new(0);
// (when, jobs, replaced, too old) at the last summary.
static DROP_LOG: Mutex<Option<(Instant, u64, u64, u64)>> = Mutex::new(None);

#[unsafe(export_name = "ReportEncodeJob")]
extern "C" fn report_encode_job(outcome: u32) {
    match outcome {
        ENCODE_QUEUED => {
            ENCODE_JOBS.fetch_add(1, Ordering::Relaxed);
        }
        ENCODE_REPLACED => {
            ENCODE_JOBS.fetch_add(1, Ordering::Relaxed);
            ENCODE_REPLACED_COUNT.fetch_add(1, Ordering::Relaxed);
            alvr_server_core::report_streamer_drop();
        }
        ENCODE_TOO_OLD => {
            ENCODE_TOO_OLD_COUNT.fetch_add(1, Ordering::Relaxed);
            alvr_server_core::report_streamer_drop();
        }
        _ => (),
    }

    let now = Instant::now();
    let jobs = ENCODE_JOBS.load(Ordering::Relaxed);
    let replaced = ENCODE_REPLACED_COUNT.load(Ordering::Relaxed);
    let too_old = ENCODE_TOO_OLD_COUNT.load(Ordering::Relaxed);
    let mut log = DROP_LOG.lock();
    let Some((since, jobs0, replaced0, too_old0)) = *log else {
        *log = Some((now, jobs, replaced, too_old));
        return;
    };
    if now.duration_since(since) < DROP_LOG_INTERVAL {
        return;
    }
    *log = Some((now, jobs, replaced, too_old));
    let (frames, replaced, too_old) = (jobs - jobs0, replaced - replaced0, too_old - too_old0);
    if replaced + too_old > 0 {
        alvr_common::warn!(
            "Streamer dropped {} of {frames} frames in the last {:.0} s before encoding \
             ({replaced} replaced while the encoder was busy, {too_old} too old). \
             The encoder is not keeping up with the refresh rate at this resolution.",
            replaced + too_old,
            now.duration_since(since).as_secs_f32()
        );
    }
}
