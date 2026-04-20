//! Linux mic-activity sampler via PulseAudio introspection.
//!
//! Works with both real PulseAudio and PipeWire's pulse-compat layer
//! (which every major distro ships as default today).
//!
//! Architecture: libpulse-binding uses a C-originated mainloop
//! incompatible with tokio. So we spawn a dedicated native thread that
//! owns the Mainloop + Context and continuously refreshes a shared
//! snapshot. The tokio-side `snapshot()` call just clones the current
//! contents of a `Mutex<Vec<String>>`.
//!
//! Snapshot algorithm on the pulse thread:
//! 1. Every ~1s, call `introspect.get_source_output_info_list(...)`.
//! 2. For each item: read `application.process.id` (skip self) and
//!    `application.process.binary` (fall back to `application.name`).
//! 3. On `ListResult::End`, swap collected Vec into the shared snapshot.
//!
//! If the PulseAudio connection drops (daemon restart), we back off and
//! reconnect. Maximum reconnect wait is 10s.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use libpulse_binding::{
    callbacks::ListResult,
    context::{Context, FlagSet as ContextFlagSet, State as ContextState},
    mainloop::threaded::Mainloop,
};
use log::{debug, info, trace, warn};

use crate::detection::signals::SignalSampler;
use crate::detection::types::MicSnapshot;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const RECONNECT_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(10);

pub struct LinuxMicActivitySampler {
    shared: Arc<SharedState>,
    thread_handle: Option<JoinHandle<()>>,
}

struct SharedState {
    snapshot: Mutex<Vec<String>>,
    shutdown: AtomicBool,
}

impl LinuxMicActivitySampler {
    pub fn new() -> Result<Self> {
        let shared = Arc::new(SharedState {
            snapshot: Mutex::new(Vec::new()),
            shutdown: AtomicBool::new(false),
        });

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let shared_for_thread = shared.clone();

        let handle = thread::Builder::new()
            .name("meetily-pulse".to_string())
            .spawn(move || pulse_worker(shared_for_thread, ready_tx))
            .map_err(|e| anyhow!("failed to spawn pulse thread: {}", e))?;

        match ready_rx.recv_timeout(CONNECT_TIMEOUT + Duration::from_secs(1)) {
            Ok(Ok(())) => {
                info!("Linux mic-activity sampler: PulseAudio connected");
                Ok(Self {
                    shared,
                    thread_handle: Some(handle),
                })
            }
            Ok(Err(e)) => {
                shared.shutdown.store(true, Ordering::Release);
                let _ = handle.join();
                Err(anyhow!("PulseAudio init failed: {}", e))
            }
            Err(_) => {
                shared.shutdown.store(true, Ordering::Release);
                // Give the worker up to 2s to observe the shutdown
                // flag and exit cleanly. If it doesn't, detach with
                // a warning — the thread holds a live pulse Context
                // that will only drop at process exit.
                let deadline = Instant::now() + Duration::from_secs(2);
                while !handle.is_finished() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(50));
                }
                if handle.is_finished() {
                    let _ = handle.join();
                } else {
                    warn!("PulseAudio init timed out and worker did not exit within 2s; detaching");
                }
                Err(anyhow!("PulseAudio init timed out"))
            }
        }
    }
}

impl SignalSampler for LinuxMicActivitySampler {
    fn snapshot(&self) -> Result<MicSnapshot> {
        let guard = self.shared.snapshot.lock().unwrap();
        Ok(MicSnapshot {
            active_bundles: guard.clone(),
        })
    }
}

impl Drop for LinuxMicActivitySampler {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        if let Some(h) = self.thread_handle.take() {
            let _ = h.join();
        }
    }
}

fn pulse_worker(shared: Arc<SharedState>, ready: Sender<Result<(), String>>) {
    let own_pid = std::process::id();
    // Sender is taken by `run_session` on the first attempt; after
    // that it's `None` and no further ready signals fire.
    let mut ready_once: Option<Sender<Result<(), String>>> = Some(ready);
    let mut backoff = RECONNECT_BACKOFF_INITIAL;

    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            return;
        }

        let is_first_attempt = ready_once.is_some();
        let notify = ready_once.take();

        match run_session(&shared, own_pid, notify) {
            SessionOutcome::Shutdown => return,
            SessionOutcome::Connected => {
                // Session exited cleanly after Ready (shutdown or
                // connection drop). Reset backoff and try again.
                backoff = RECONNECT_BACKOFF_INITIAL;
            }
            SessionOutcome::ConnectFailed(msg) => {
                if is_first_attempt {
                    // The constructor was already notified by
                    // run_session itself (which consumed the Sender).
                    warn!("PulseAudio initial connect failed: {}", msg);
                    return;
                }
                warn!("PulseAudio reconnect failed: {}", msg);
            }
        }

        // Backoff before reconnecting. ±20% jitter to avoid thundering
        // herd when multiple Meetily instances on the same host (or
        // many users on a shared workstation) reconnect in lockstep
        // after a pulseaudio restart.
        let jitter_pct = rand::Rng::gen_range(&mut rand::thread_rng(), -0.2f32..=0.2f32);
        let jittered = backoff.as_secs_f32() * (1.0 + jitter_pct);
        let sleep_until = Instant::now() + Duration::from_secs_f32(jittered.max(0.0));
        while Instant::now() < sleep_until {
            if shared.shutdown.load(Ordering::Acquire) {
                return;
            }
            thread::sleep(Duration::from_millis(200));
        }
        backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
    }
}

enum SessionOutcome {
    /// Shutdown requested; exit worker.
    Shutdown,
    /// Connected successfully, ran until connection dropped or shutdown.
    Connected,
    /// Failed to reach Ready. Notify carried the error out if this was
    /// the first attempt.
    ConnectFailed(String),
}

/// Run one session: create mainloop + context, connect, poll until the
/// connection drops or shutdown is requested. If `notify` is `Some`,
/// send the ready/failed signal through it on first connect attempt.
fn run_session(
    shared: &Arc<SharedState>,
    own_pid: u32,
    notify: Option<Sender<Result<(), String>>>,
) -> SessionOutcome {
    let mut mainloop = match Mainloop::new() {
        Some(m) => m,
        None => {
            let msg = "Mainloop::new returned None".to_string();
            if let Some(tx) = notify {
                let _ = tx.send(Err(msg.clone()));
            }
            return SessionOutcome::ConnectFailed(msg);
        }
    };

    let mut context = match Context::new(&mainloop, "meetily-detection") {
        Some(c) => c,
        None => {
            let msg = "Context::new returned None".to_string();
            if let Some(tx) = notify {
                let _ = tx.send(Err(msg.clone()));
            }
            return SessionOutcome::ConnectFailed(msg);
        }
    };

    if let Err(e) = context.connect(None, ContextFlagSet::NOFLAGS, None) {
        let msg = format!("connect: {:?}", e);
        if let Some(tx) = notify {
            let _ = tx.send(Err(msg.clone()));
        }
        return SessionOutcome::ConnectFailed(msg);
    }

    if let Err(e) = mainloop.start() {
        let msg = format!("mainloop.start: {:?}", e);
        if let Some(tx) = notify {
            let _ = tx.send(Err(msg.clone()));
        }
        return SessionOutcome::ConnectFailed(msg);
    }

    // Poll for Ready up to CONNECT_TIMEOUT.
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut ready = false;
    while Instant::now() < deadline {
        if shared.shutdown.load(Ordering::Acquire) {
            return SessionOutcome::Shutdown;
        }
        mainloop.lock();
        let state = context.get_state();
        mainloop.unlock();
        match state {
            ContextState::Ready => {
                ready = true;
                break;
            }
            ContextState::Failed | ContextState::Terminated => {
                let msg = format!("unexpected state during connect: {:?}", state);
                if let Some(tx) = notify {
                    let _ = tx.send(Err(msg.clone()));
                }
                return SessionOutcome::ConnectFailed(msg);
            }
            _ => thread::sleep(Duration::from_millis(50)),
        }
    }
    if !ready {
        let msg = "connect timed out".to_string();
        if let Some(tx) = notify {
            let _ = tx.send(Err(msg.clone()));
        }
        return SessionOutcome::ConnectFailed(msg);
    }

    // Notify the constructor that we're up.
    if let Some(tx) = notify {
        let _ = tx.send(Ok(()));
    }

    // Main poll loop.
    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            return SessionOutcome::Shutdown;
        }
        mainloop.lock();
        let state = context.get_state();
        mainloop.unlock();
        if state != ContextState::Ready {
            warn!("PulseAudio context dropped to state {:?}", state);
            return SessionOutcome::Connected;
        }

        collect_snapshot(shared, &mut mainloop, &context, own_pid);

        let sleep_until = Instant::now() + POLL_INTERVAL;
        while Instant::now() < sleep_until {
            if shared.shutdown.load(Ordering::Acquire) {
                return SessionOutcome::Shutdown;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn collect_snapshot(
    shared: &Arc<SharedState>,
    mainloop: &mut Mainloop,
    context: &Context,
    own_pid: u32,
) {
    let collected = Arc::new(Mutex::new(Vec::<String>::new()));
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);

    let collected_cb = collected.clone();
    // `SyncSender<()>` is Send + Clone. The callback runs on the
    // pulse mainloop thread; once End/Error fires we signal via
    // `try_send` (non-blocking — channel size is 1 so a duplicate
    // send is swallowed).
    let done_tx_cb = done_tx.clone();

    // Hold the mainloop lock across the full Operation lifecycle:
    // creation, in-flight callback dispatch, and Drop (which calls
    // `pa_operation_unref`). Releasing the lock while the Operation
    // is alive can race with the mainloop's internal state, a
    // documented pulse-binding UB class.
    mainloop.lock();
    let introspect = context.introspect();
    let op = introspect.get_source_output_info_list(move |result| match result {
        ListResult::Item(info) => {
            // Self-filter by PID.
            if let Some(pid_str) = info.proplist.get_str("application.process.id") {
                if let Ok(pid) = pid_str.parse::<u32>() {
                    if pid == own_pid {
                        return;
                    }
                }
            }
            // Prefer binary name; fall back to application name.
            let bundle = info
                .proplist
                .get_str("application.process.binary")
                .or_else(|| info.proplist.get_str("application.name"));
            if let Some(name) = bundle {
                if !name.is_empty() {
                    collected_cb.lock().unwrap().push(name);
                }
            }
        }
        ListResult::End => {
            let _ = done_tx_cb.try_send(());
        }
        ListResult::Error => {
            warn!("pulse get_source_output_info_list reported Error");
            let _ = done_tx_cb.try_send(());
        }
    });
    mainloop.unlock();

    // Wait for the callback to signal completion via the channel.
    // The mainloop keeps driving callbacks on its own thread; we
    // just block on the rendezvous rather than poll-sleeping.
    let completed = done_rx.recv_timeout(Duration::from_secs(2)).is_ok();

    // Re-acquire the mainloop lock so the Operation can be dropped
    // (→ pa_operation_unref) while the lock is held. Even on timeout
    // this is the safe place to drop — pulse handles unref of an
    // in-flight op by cancelling it.
    mainloop.lock();
    drop(op);
    mainloop.unlock();

    if !completed {
        trace!("pulse introspect callback did not complete within 2s");
        return;
    }

    let new_snapshot = std::mem::take(&mut *collected.lock().unwrap());
    debug!(
        "Linux mic snapshot: {} active {:?}",
        new_snapshot.len(),
        new_snapshot
    );
    *shared.snapshot.lock().unwrap() = new_snapshot;
}
