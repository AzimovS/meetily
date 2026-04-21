//! Long-lived detection task. Polls the mic-activity sampler at a fixed
//! cadence, advances the state machine, and routes detection events to
//! the notification manager.
//!
//! The cadence is intentionally conservative (1s). CoreAudio property
//! reads are cheap; the per-process enumeration only runs when the
//! "device is running somewhere" gate is hot. True event-driven
//! listeners are a future optimization — see the plan's "Technical
//! Approach" section.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::sync::Mutex;

use crate::detection::signals::mic_activity;
use crate::detection::state::{DetectorConfig, DetectorState};
use crate::detection::types::{DetectedMeetingEvent, DetectionEvent};
use crate::notifications::commands::{
    show_meeting_detected_notification, show_meeting_ended_notification,
    NotificationManagerState,
};

const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Handle for the detection task. Registered in Tauri state so future
/// command handlers can reach in for `dismiss` / `get_state` surfaces.
pub struct DetectionService {
    state: Arc<Mutex<DetectorState>>,
    running: Arc<AtomicBool>,
    /// External recording flag, pushed in from the audio layer.
    /// Lock-free so `set_recording` stays sync and strictly ordered
    /// even if the poll loop is mid-`advance()`. The poll loop
    /// snapshots this into `DetectorState` before each tick.
    is_recording: Arc<AtomicBool>,
}

impl DetectionService {
    fn new(config: DetectorConfig, running: Arc<AtomicBool>) -> Self {
        Self {
            state: Arc::new(Mutex::new(DetectorState::new(config))),
            running,
            is_recording: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Signals the task loop to exit at its next tick. Used on app
    /// teardown so the detector stops sampling CoreAudio/WASAPI/pulse
    /// before the runtime starts dropping state.
    pub fn shutdown(&self) {
        self.running.store(false, Ordering::Release);
        debug!("DetectionService::shutdown signalled");
    }

    /// Record that Meetily started/stopped recording. Lock-free atomic
    /// write — safe to call from any thread, sync or async, in any
    /// order, without contending on the state-machine mutex.
    pub fn set_recording(&self, recording: bool) {
        self.is_recording.store(recording, Ordering::Release);
        debug!("DetectionService: set_recording({})", recording);
    }

    /// Dismiss a detected bundle so further MeetingDetected banners
    /// for it are suppressed for the cooldown window. Exposed via
    /// Tauri command; also callable from internal code once we add a
    /// tap-to-dismiss handler.
    pub async fn dismiss(&self, bundle_id: &str) {
        let mut guard = self.state.lock().await;
        guard.dismiss(bundle_id, Instant::now());
        info!("DetectionService: dismissed");
    }

    /// Snapshot the detector's current phase for UI / agent queries.
    pub async fn current_phase(&self) -> crate::detection::state::DetectorPhaseSnapshot {
        let guard = self.state.lock().await;
        guard.phase_snapshot(Instant::now())
    }
}

/// Spawn the detection task. Returns a `DetectionService` handle that
/// should be registered in Tauri state.
pub fn spawn<R>(app: AppHandle<R>) -> DetectionService
where
    R: Runtime,
{
    let running = Arc::new(AtomicBool::new(true));
    let service = DetectionService::new(DetectorConfig::DEFAULT, running.clone());
    let state = service.state.clone();
    let running_for_task = running.clone();
    let is_recording_for_task = service.is_recording.clone();

    let sampler = match mic_activity::create() {
        Ok(s) => s,
        Err(e) => {
            warn!("Meeting detection disabled — failed to init mic-activity sampler: {}", e);
            return service;
        }
    };

    info!("Meeting detection: spawning poll task ({}s interval)", POLL_INTERVAL.as_secs());

    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // First tick fires immediately; skip it to avoid hammering the
        // platform audio API before the rest of the app is up.
        ticker.tick().await;

        while running_for_task.load(Ordering::Acquire) {
            ticker.tick().await;

            let snapshot = match sampler.snapshot() {
                Ok(s) => s,
                Err(e) => {
                    warn!("Meeting detection: snapshot failed: {}", e);
                    continue;
                }
            };

            // Sync the externally-pushed recording flag into the state
            // machine right before advancing. Done under the state lock
            // so `advance()` sees a coherent value.
            let event = {
                let mut guard = state.lock().await;
                guard.set_recording(is_recording_for_task.load(Ordering::Acquire));
                guard.advance(Instant::now(), &snapshot)
            };

            let Some(event) = event else { continue };

            let mgr_state = app.state::<NotificationManagerState<R>>();
            match event {
                DetectionEvent::MeetingDetected(m) => {
                    info!("Meeting detection: DETECTED {}", m.display_name);
                    debug!("Meeting detection: DETECTED bundle={}", m.bundle_id);
                    // Only emit the event when the corresponding banner
                    // preference is enabled. Keeps the event and the
                    // user-facing banner under the same user control,
                    // and strips `bundle_id` from the payload — agents
                    // can still read it via `get_detection_state`.
                    if pref_show_meeting_detected(mgr_state.inner()).await {
                        let payload = DetectedMeetingEvent { display_name: m.display_name.clone() };
                        if let Err(e) = app.emit("meeting-detected", &payload) {
                            debug!("failed to emit meeting-detected event: {}", e);
                        }
                    }
                    if let Err(e) = show_meeting_detected_notification(
                        &app, mgr_state.inner(), m.display_name,
                    ).await {
                        error!("Failed to show meeting-detected notification: {}", e);
                    }
                }
                DetectionEvent::MeetingEnded(m) => {
                    info!("Meeting detection: ENDED {}", m.display_name);
                    debug!("Meeting detection: ENDED bundle={}", m.bundle_id);
                    if pref_show_meeting_ended(mgr_state.inner()).await {
                        let payload = DetectedMeetingEvent { display_name: m.display_name.clone() };
                        if let Err(e) = app.emit("meeting-ended", &payload) {
                            debug!("failed to emit meeting-ended event: {}", e);
                        }
                    }
                    if let Err(e) = show_meeting_ended_notification(
                        &app, mgr_state.inner(), m.display_name,
                    ).await {
                        error!("Failed to show meeting-ended notification: {}", e);
                    }
                }
            }
        }

        info!("Meeting detection: poll task exiting");
    });

    service
}

/// Read the `show_meeting_detected` preference from the live notification
/// manager. Defaults to `true` if the manager isn't initialized yet so
/// the first detection after startup isn't silently dropped.
async fn pref_show_meeting_detected<R: Runtime>(
    manager_state: &NotificationManagerState<R>,
) -> bool {
    let guard = manager_state.read().await;
    match guard.as_ref() {
        Some(manager) => {
            manager
                .get_settings()
                .await
                .notification_preferences
                .show_meeting_detected
        }
        None => true,
    }
}

async fn pref_show_meeting_ended<R: Runtime>(
    manager_state: &NotificationManagerState<R>,
) -> bool {
    let guard = manager_state.read().await;
    match guard.as_ref() {
        Some(manager) => {
            manager
                .get_settings()
                .await
                .notification_preferences
                .show_meeting_ended
        }
        None => true,
    }
}
