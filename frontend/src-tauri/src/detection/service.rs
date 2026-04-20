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

use log::{error, info, warn};
use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::Mutex;

use crate::detection::signals::mic_activity;
use crate::detection::state::{DetectorConfig, DetectorState};
use crate::detection::types::DetectionEvent;
use crate::notifications::commands::{
    show_meeting_detected_notification, show_meeting_ended_notification,
    NotificationManagerState,
};

/// Poll interval. Small enough that 30s thresholds feel snappy;
/// large enough that we don't burn CPU when idle.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Shared handle that lets external callers query/influence the detector.
/// Kept in Tauri state so future tap-handler + dismiss commands can
/// reach in without recreating plumbing.
pub struct DetectionService {
    state: Arc<Mutex<DetectorState>>,
}

impl DetectionService {
    pub fn new(config: DetectorConfig) -> Self {
        Self {
            state: Arc::new(Mutex::new(DetectorState::new(config))),
        }
    }
}

/// Spawn the detection task. Returns a `DetectionService` handle that
/// should be registered in Tauri state.
///
/// `is_recording` is a cheap function that returns whether recording is
/// currently active — used to gate the `MeetingEnded` event path.
pub fn spawn<R, F>(app: AppHandle<R>, is_recording: F) -> DetectionService
where
    R: Runtime,
    F: Fn() -> bool + Send + Sync + 'static,
{
    let service = DetectionService::new(DetectorConfig::DEFAULT);
    let state = service.state.clone();

    let sampler = match mic_activity::create() {
        Ok(s) => s,
        Err(e) => {
            warn!("Meeting detection disabled — failed to init mic-activity sampler: {}", e);
            return service;
        }
    };

    // Flag flipped when the runtime is shutting down. On macOS tauri
    // drops app state at teardown, and we want the loop to exit cleanly.
    let running = Arc::new(AtomicBool::new(true));
    let running_cloned = running.clone();

    info!("Meeting detection: spawning poll task ({}s interval)", POLL_INTERVAL.as_secs());

    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // First tick fires immediately; skip it to avoid hammering CoreAudio
        // at startup before the rest of the app is up.
        ticker.tick().await;

        while running_cloned.load(Ordering::Acquire) {
            ticker.tick().await;

            let snapshot = match sampler.snapshot() {
                Ok(s) => s,
                Err(e) => {
                    warn!("Meeting detection: snapshot failed: {}", e);
                    continue;
                }
            };

            let event = {
                let mut guard = state.lock().await;
                guard.advance(Instant::now(), &snapshot, is_recording())
            };

            let Some(event) = event else { continue };

            let mgr_state = app.state::<NotificationManagerState<R>>();
            match event {
                DetectionEvent::MeetingDetected(m) => {
                    info!(
                        "Meeting detection: DETECTED {} ({})",
                        m.display_name, m.bundle_id
                    );
                    if let Err(e) =
                        show_meeting_detected_notification(&app, mgr_state.inner(), m.display_name)
                            .await
                    {
                        error!("Failed to show meeting-detected notification: {}", e);
                    }
                }
                DetectionEvent::MeetingEnded(m) => {
                    info!(
                        "Meeting detection: ENDED {} ({})",
                        m.display_name, m.bundle_id
                    );
                    if let Err(e) =
                        show_meeting_ended_notification(&app, mgr_state.inner(), m.display_name)
                            .await
                    {
                        error!("Failed to show meeting-ended notification: {}", e);
                    }
                }
            }
        }

        info!("Meeting detection: poll task exiting");
    });

    // Keep the running flag alive for the lifetime of the process;
    // dropped only when the detector itself is dropped (never, in practice).
    std::mem::forget(running);

    service
}
