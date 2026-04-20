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
                // Don't join — worker may be stuck in the pulse
                // mainloop. It sees the shutdown flag and bails next
                // wake-up; thread leaks but exits cleanly within ~1s.
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

        // Backoff before reconnecting, responsive to shutdown.
        let sleep_until = Instant::now() + backoff;
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

        collect_snapshot(shared, &mainloop, &context, own_pid);

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
    mainloop: &Mainloop,
    context: &Context,
    own_pid: u32,
) {
    let collected = Arc::new(Mutex::new(Vec::<String>::new()));
    let done = Arc::new(AtomicBool::new(false));

    let collected_cb = collected.clone();
    let done_cb = done.clone();

    mainloop.lock();
    let introspect = context.introspect();
    let _op = introspect.get_source_output_info_list(move |result| match result {
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
            done_cb.store(true, Ordering::Release);
        }
        ListResult::Error => {
            warn!("pulse get_source_output_info_list reported Error");
            done_cb.store(true, Ordering::Release);
        }
    });
    mainloop.unlock();

    // Wait for the callback to finish. Cap at 2s — if it really takes
    // that long, pulseaudio is probably wedged; bail and try next tick.
    let deadline = Instant::now() + Duration::from_secs(2);
    while !done.load(Ordering::Acquire) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }

    if !done.load(Ordering::Acquire) {
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
