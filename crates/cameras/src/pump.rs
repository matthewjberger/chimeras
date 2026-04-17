//! Long-running background pump that pulls frames from a [`Camera`] and hands
//! each one to a caller-provided sink closure.
//!
//! The pump owns the camera, so all frame I/O is serialized through a single
//! worker thread. Callers can:
//! - Pause and resume streaming without closing the camera ([`set_active`]).
//! - Grab a single fresh frame on demand whether the pump is streaming or
//!   paused ([`capture_frame`]). The camera stays warm so latency is one
//!   frame interval plus up to 20ms of pause-wake.
//! - Stop the pump deterministically ([`stop_and_join`]) or let the
//!   [`Pump`]'s `Drop` tear it down asynchronously.
//!
//! # Pause semantics
//!
//! [`set_active(false)`](set_active) eliminates *Rust-side* per-frame work:
//! no more [`next_frame`] calls, no sink invocations, effectively zero CPU
//! for the pump thread. The OS-level camera pipeline, however, keeps
//! running: AVFoundation still delivers sample buffers, Media Foundation's
//! source reader still decodes, V4L2 still DMAs frames into userspace, and
//! they land in cameras' bounded internal channel and get dropped when it
//! overflows.
//!
//! On AC power this OS-side cost is typically <5% of one core for 1080p30,
//! and negligible. On battery it is measurable; if that matters, close the
//! [`Camera`] entirely (drop the [`Pump`]). Truly stopping the OS pipeline
//! without closing the device would require a separate primitive at this
//! layer (AVFoundation `stopRunning`, MF source-reader flush, V4L2
//! `STREAMOFF`); not provided today.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::thread::{JoinHandle, sleep};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::source::{CameraSource, open_source};
use crate::types::StreamConfig;
use crate::{Camera, DEFAULT_FRAME_TIMEOUT, Error, Frame, next_frame};

const PAUSED_POLL_INTERVAL: Duration = Duration::from_millis(20);
const COMMAND_QUEUE_CAPACITY: usize = 16;
const MAX_BACKOFF_SHIFT: u32 = 20;

/// Policy controlling automatic RTSP session re-establishment by a pump.
///
/// Applies only when a pump is created via [`spawn_with_policy`] with an
/// `CameraSource::Rtsp` source. USB sources ignore the policy — their
/// hotplug monitor handles device-loss.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ReconnectPolicy {
    /// Initial backoff duration after the first failure.
    pub initial_backoff: Duration,
    /// Cap on backoff between attempts.
    pub max_backoff: Duration,
    /// Upper bound on retry attempts. `None` means retry forever.
    pub max_attempts: Option<u32>,
    /// Fractional jitter (0.0..1.0) applied symmetrically to each backoff.
    pub jitter: f32,
    /// How long without a frame before the pump decides the session has stalled.
    pub stall_timeout: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            max_attempts: None,
            jitter: 0.2,
            stall_timeout: Duration::from_secs(15),
        }
    }
}

/// Lifecycle events emitted on the optional status channel of a pump created
/// via [`spawn_with_policy`].
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum PumpStatus {
    /// The pump is opening the initial session.
    Connecting,
    /// The pump has an active session and is delivering frames.
    Connected,
    /// The pump is between sessions and waiting `next_delay` before attempt `attempt`.
    Reconnecting {
        /// Zero-based attempt counter; `0` for the first retry after failure.
        attempt: u32,
        /// Duration the pump will wait before the next `open_source` call.
        next_delay: Duration,
        /// Short machine-diagnosable identifier for why the previous session ended.
        reason: String,
    },
    /// The pump hit the policy's `max_attempts` cap and stopped.
    GaveUp {
        /// Short machine-diagnosable identifier for the last failure.
        reason: String,
    },
}

/// A running camera pump.
///
/// Obtained from [`spawn`]. The struct holds private worker state; interact
/// with it through the free functions in this module.
pub struct Pump {
    pub(crate) worker: Option<JoinHandle<()>>,
    pub(crate) shutdown: Arc<AtomicBool>,
    pub(crate) active: Arc<AtomicBool>,
    pub(crate) commands: SyncSender<PumpCommand>,
}

impl Drop for Pump {
    fn drop(&mut self) {
        if self.worker.is_some() {
            self.shutdown.store(true, Ordering::Relaxed);
        }
    }
}

pub(crate) enum PumpCommand {
    Capture { reply: Sender<Option<Frame>> },
}

/// Spawn a worker thread that pulls frames from `camera` and hands each one
/// to `on_frame`.
///
/// The pump starts in the active state (streaming). The worker stops when the
/// returned [`Pump`] is dropped, [`stop_and_join`] is called, or the camera
/// reports a non-timeout error.
pub fn spawn<F>(camera: Camera, mut on_frame: F) -> Pump
where
    F: FnMut(Frame) + Send + 'static,
{
    let shutdown = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicBool::new(true));
    let (command_tx, command_rx) = mpsc::sync_channel::<PumpCommand>(COMMAND_QUEUE_CAPACITY);

    let shutdown_for_worker = Arc::clone(&shutdown);
    let active_for_worker = Arc::clone(&active);
    let worker = std::thread::Builder::new()
        .name("cameras-pump".into())
        .spawn(move || {
            let camera = camera;
            let mut last_frame_at = Instant::now();
            loop {
                if shutdown_for_worker.load(Ordering::Relaxed) {
                    break;
                }

                if drain_command_queue(&command_rx, &camera, &mut on_frame, &mut last_frame_at) {
                    continue;
                }

                if !active_for_worker.load(Ordering::Relaxed) {
                    sleep(PAUSED_POLL_INTERVAL);
                    last_frame_at = Instant::now();
                    continue;
                }

                match next_frame(&camera, DEFAULT_FRAME_TIMEOUT) {
                    Ok(frame) => {
                        last_frame_at = Instant::now();
                        on_frame(frame);
                    }
                    Err(Error::Timeout) => continue,
                    Err(_) => break,
                }
            }
        })
        .expect("failed to spawn cameras pump thread");

    Pump {
        worker: Some(worker),
        shutdown,
        active,
        commands: command_tx,
    }
}

fn drain_command_queue<F>(
    command_rx: &mpsc::Receiver<PumpCommand>,
    camera: &Camera,
    on_frame: &mut F,
    last_frame_at: &mut Instant,
) -> bool
where
    F: FnMut(Frame),
{
    let mut handled = false;
    while let Ok(command) = command_rx.try_recv() {
        match command {
            PumpCommand::Capture { reply } => {
                let frame = match next_frame(camera, DEFAULT_FRAME_TIMEOUT) {
                    Ok(frame) => {
                        *last_frame_at = Instant::now();
                        on_frame(frame.clone());
                        Some(frame)
                    }
                    Err(_) => None,
                };
                let _ = reply.send(frame);
            }
        }
        handled = true;
    }
    handled
}

/// Toggle whether the pump actively streams frames to its sink.
///
/// - `true` (the default): worker pulls frames continuously and calls the
///   sink for each one.
/// - `false`: worker parks. No [`next_frame`] calls, no sink invocations,
///   no per-frame Rust work. The camera handle stays open so
///   [`capture_frame`] remains fast.
pub fn set_active(pump: &Pump, active: bool) {
    pump.active.store(active, Ordering::Relaxed);
}

/// Request a single fresh frame from the pump.
///
/// Works whether the pump is active or paused. The request is queued to the
/// worker thread, which pulls one frame via [`next_frame`], hands it to the
/// sink (so any attached listener also receives it), and returns it.
///
/// Blocks the calling thread until the worker replies, typically one frame
/// interval, plus up to 20ms of wake latency if the pump is paused.
///
/// Returns `None` if the command queue is full, the worker has shut down, the
/// camera errored while reading, or the pump is currently inside a
/// [`ReconnectPolicy`] backoff window (for RTSP sources created via
/// [`spawn_with_policy`]). Callers that want visibility into the reconnect
/// state should subscribe to the status channel passed to
/// [`spawn_with_policy`] rather than inferring from a `None` return.
pub fn capture_frame(pump: &Pump) -> Option<Frame> {
    let (reply_tx, reply_rx) = mpsc::channel();
    pump.commands
        .try_send(PumpCommand::Capture { reply: reply_tx })
        .ok()?;
    reply_rx.recv().ok().flatten()
}

/// Consume the pump, signal the worker to stop, and block until it has
/// exited.
///
/// Use when you need to guarantee the camera has released its handle before
/// returning, for example before re-opening the same device. Equivalent to
/// `drop(pump)` plus an explicit join.
pub fn stop_and_join(mut pump: Pump) {
    pump.shutdown.store(true, Ordering::Relaxed);
    if let Some(worker) = pump.worker.take() {
        let _ = worker.join();
    }
}

/// Spawn a reconnection-aware pump that opens `source` with `config`,
/// delivers frames to `on_frame`, and (for RTSP sources) re-opens the session
/// on failure per `policy`.
///
/// Pass `policy = None` or a non-RTSP source to get behavior equivalent to
/// [`spawn`] with no reconnection. Non-RTSP sources ignore the policy.
/// When `status` is provided, the pump emits [`PumpStatus`] events on it.
///
/// Fails if the initial [`open_source`] call fails; reconnection engages only
/// after a successful first connection.
pub fn spawn_with_policy<F>(
    source: CameraSource,
    config: StreamConfig,
    mut on_frame: F,
    policy: Option<ReconnectPolicy>,
    status: Option<SyncSender<PumpStatus>>,
) -> Result<Pump, Error>
where
    F: FnMut(Frame) + Send + 'static,
{
    let reconnect_eligible = source_is_rtsp(&source) && policy.is_some();
    let effective_policy = policy.unwrap_or_default();

    emit_status(&status, PumpStatus::Connecting);
    let initial_camera = open_source(source.clone(), config)?;
    emit_status(&status, PumpStatus::Connected);

    let shutdown = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicBool::new(true));
    let (command_tx, command_rx) = mpsc::sync_channel::<PumpCommand>(COMMAND_QUEUE_CAPACITY);

    let shutdown_for_worker = Arc::clone(&shutdown);
    let active_for_worker = Arc::clone(&active);
    let worker = std::thread::Builder::new()
        .name("cameras-pump".into())
        .spawn(move || {
            let mut camera = initial_camera;
            let source = source;
            let config = config;
            let status = status;
            let mut last_frame_at = Instant::now();

            loop {
                if shutdown_for_worker.load(Ordering::Relaxed) {
                    break;
                }

                if drain_command_queue(&command_rx, &camera, &mut on_frame, &mut last_frame_at) {
                    continue;
                }

                if !active_for_worker.load(Ordering::Relaxed) {
                    sleep(PAUSED_POLL_INTERVAL);
                    last_frame_at = Instant::now();
                    continue;
                }

                match next_frame(&camera, DEFAULT_FRAME_TIMEOUT) {
                    Ok(frame) => {
                        last_frame_at = Instant::now();
                        on_frame(frame);
                    }
                    Err(Error::Timeout) => {
                        if reconnect_eligible
                            && last_frame_at.elapsed() > effective_policy.stall_timeout
                        {
                            match run_reconnect_loop(
                                ReconnectContext {
                                    command_rx: &command_rx,
                                    source: &source,
                                    config,
                                    policy: &effective_policy,
                                    status: &status,
                                    shutdown: &shutdown_for_worker,
                                    active: &active_for_worker,
                                },
                                "stall_timeout_exceeded",
                            ) {
                                ReconnectOutcome::Reconnected(new_camera) => {
                                    camera = new_camera;
                                    last_frame_at = Instant::now();
                                }
                                ReconnectOutcome::Shutdown | ReconnectOutcome::GaveUp => break,
                            }
                        }
                    }
                    Err(_) => {
                        if !reconnect_eligible {
                            break;
                        }
                        match run_reconnect_loop(
                            ReconnectContext {
                                command_rx: &command_rx,
                                source: &source,
                                config,
                                policy: &effective_policy,
                                status: &status,
                                shutdown: &shutdown_for_worker,
                                active: &active_for_worker,
                            },
                            "next_frame_error",
                        ) {
                            ReconnectOutcome::Reconnected(new_camera) => {
                                camera = new_camera;
                                last_frame_at = Instant::now();
                            }
                            ReconnectOutcome::Shutdown | ReconnectOutcome::GaveUp => break,
                        }
                    }
                }
            }
        })
        .expect("failed to spawn cameras pump thread");

    Ok(Pump {
        worker: Some(worker),
        shutdown,
        active,
        commands: command_tx,
    })
}

enum ReconnectOutcome {
    Reconnected(Camera),
    GaveUp,
    Shutdown,
}

struct ReconnectContext<'a> {
    command_rx: &'a mpsc::Receiver<PumpCommand>,
    source: &'a CameraSource,
    config: StreamConfig,
    policy: &'a ReconnectPolicy,
    status: &'a Option<SyncSender<PumpStatus>>,
    shutdown: &'a AtomicBool,
    active: &'a AtomicBool,
}

fn run_reconnect_loop(context: ReconnectContext<'_>, initial_reason: &str) -> ReconnectOutcome {
    let ReconnectContext {
        command_rx,
        source,
        config,
        policy,
        status,
        shutdown,
        active,
    } = context;
    let mut attempt: u32 = 0;
    let mut reason = initial_reason.to_string();
    loop {
        if shutdown.load(Ordering::Relaxed) {
            reply_none_to_pending_commands(command_rx);
            return ReconnectOutcome::Shutdown;
        }

        reply_none_to_pending_commands(command_rx);

        if !active.load(Ordering::Relaxed) {
            sleep(PAUSED_POLL_INTERVAL);
            continue;
        }

        if let Some(max) = policy.max_attempts
            && attempt >= max
        {
            emit_status(
                status,
                PumpStatus::GaveUp {
                    reason: format!("max_attempts_reached:{reason}"),
                },
            );
            return ReconnectOutcome::GaveUp;
        }

        let delay = compute_backoff(policy, attempt);
        emit_status(
            status,
            PumpStatus::Reconnecting {
                attempt,
                next_delay: delay,
                reason: reason.clone(),
            },
        );

        sleep_responsive(delay, shutdown, command_rx);
        if shutdown.load(Ordering::Relaxed) {
            return ReconnectOutcome::Shutdown;
        }

        match open_source(source.clone(), config) {
            Ok(new_camera) => {
                emit_status(status, PumpStatus::Connected);
                return ReconnectOutcome::Reconnected(new_camera);
            }
            Err(error) => {
                reason = format!("open_failed:{error}");
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

fn reply_none_to_pending_commands(command_rx: &mpsc::Receiver<PumpCommand>) {
    while let Ok(command) = command_rx.try_recv() {
        match command {
            PumpCommand::Capture { reply } => {
                let _ = reply.send(None);
            }
        }
    }
}

fn sleep_responsive(
    total: Duration,
    shutdown: &AtomicBool,
    command_rx: &mpsc::Receiver<PumpCommand>,
) {
    let tick = Duration::from_millis(100);
    let mut remaining = total;
    while remaining > Duration::ZERO {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        reply_none_to_pending_commands(command_rx);
        let chunk = remaining.min(tick);
        sleep(chunk);
        remaining = remaining.saturating_sub(chunk);
    }
}

fn compute_backoff(policy: &ReconnectPolicy, attempt: u32) -> Duration {
    let base_nanos = policy.initial_backoff.as_nanos() as u64;
    let factor = 1u64
        .checked_shl(attempt.min(MAX_BACKOFF_SHIFT))
        .unwrap_or(u64::MAX);
    let scaled_nanos = base_nanos.saturating_mul(factor);
    let max_nanos = policy.max_backoff.as_nanos() as u64;
    let capped_nanos = scaled_nanos.min(max_nanos);

    let jitter_magnitude = (capped_nanos as f32 * policy.jitter.abs()) as u64;
    if jitter_magnitude == 0 {
        return Duration::from_nanos(capped_nanos);
    }

    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos() as u64)
        .unwrap_or(attempt as u64);
    let offset = (seed % (jitter_magnitude * 2)) as i64 - jitter_magnitude as i64;
    let final_nanos = (capped_nanos as i64 + offset).max(0) as u64;
    Duration::from_nanos(final_nanos)
}

fn emit_status(status: &Option<SyncSender<PumpStatus>>, event: PumpStatus) {
    if let Some(tx) = status {
        let _ = tx.try_send(event);
    }
}

fn source_is_rtsp(source: &CameraSource) -> bool {
    #[cfg(all(feature = "rtsp", any(target_os = "macos", target_os = "windows")))]
    {
        matches!(source, CameraSource::Rtsp { .. })
    }
    #[cfg(not(all(feature = "rtsp", any(target_os = "macos", target_os = "windows"))))]
    {
        let _ = source;
        false
    }
}
