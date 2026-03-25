// audio/meeting_detection/coordinator.rs
//
// Meeting Detection Actor — channel-based event loop with inline timers.
// Single tokio task owns all state. No shared mutexes needed.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::time::Sleep;
use tokio_util::sync::CancellationToken;

use super::{
    AppInfo, DetectionEvent, DetectionSettings, DetectionState, SideEffect, TimerKind,
    sanitize_meeting_name, MEETING_TITLE_PATTERNS,
};

#[cfg(target_os = "macos")]
use super::{MACOS_BROWSER_BUNDLE_IDS, MACOS_IGNORE_BUNDLE_IDS};

// ============================================================================
// ACTOR HANDLE (public API — cloneable, Send + Sync)
// ============================================================================

#[derive(Clone)]
pub struct MeetingDetectionHandle {
    sender: mpsc::Sender<DetectionEvent>,
}

impl MeetingDetectionHandle {
    /// Send an event to the actor. Non-blocking if channel has capacity.
    pub async fn send(&self, event: DetectionEvent) {
        if let Err(e) = self.sender.send(event).await {
            tracing::warn!("Meeting detection actor not running: {}", e);
        }
    }

    /// Try to send without awaiting (for use from non-async contexts)
    pub fn try_send(&self, event: DetectionEvent) {
        if let Err(e) = self.sender.try_send(event) {
            tracing::warn!("Meeting detection channel full or closed: {}", e);
        }
    }
}

// ============================================================================
// ACTOR
// ============================================================================

pub struct MeetingDetectionActor {
    /// Per-app detection state
    app_states: HashMap<String, DetectionState>,
    /// Previous snapshot of audio-using app identifiers (for diffing)
    previous_apps: HashSet<String>,
    /// Apps to ignore (bundle IDs on macOS)
    ignore_list: HashSet<String>,
    /// Settings
    settings: DetectionSettings,
    /// Whether a manual recording is active (suppresses detection)
    manual_recording_active: bool,
    /// Active timer (kind, generation, sleep future)
    active_timer: Option<(TimerKind, u64, Pin<Box<Sleep>>)>,
    /// Generation counter for timer cancellation
    generation: u64,
    /// Inbound events
    receiver: mpsc::Receiver<DetectionEvent>,
    /// Outbound side effects
    effect_sender: mpsc::Sender<SideEffect>,
    /// Shutdown signal
    shutdown: CancellationToken,
}

/// Spawn the actor and return a handle + cancellation token.
pub fn spawn_actor(
    settings: DetectionSettings,
    effect_sender: mpsc::Sender<SideEffect>,
) -> (MeetingDetectionHandle, CancellationToken) {
    let (tx, rx) = mpsc::channel(64);
    let shutdown = CancellationToken::new();

    let mut ignore_list = HashSet::new();
    #[cfg(target_os = "macos")]
    {
        for id in MACOS_IGNORE_BUNDLE_IDS {
            ignore_list.insert(id.to_string());
        }
    }

    let actor = MeetingDetectionActor {
        app_states: HashMap::new(),
        previous_apps: HashSet::new(),
        ignore_list,
        settings,
        manual_recording_active: false,
        active_timer: None,
        generation: 0,
        receiver: rx,
        effect_sender,
        shutdown: shutdown.clone(),
    };

    tokio::spawn(actor.run());

    let handle = MeetingDetectionHandle { sender: tx };
    (handle, shutdown)
}

impl MeetingDetectionActor {
    /// Main actor loop — select over events, timers, and shutdown.
    async fn run(mut self) {
        tracing::info!("Meeting detection actor started");

        loop {
            tokio::select! {
                // Shutdown takes priority
                _ = self.shutdown.cancelled() => {
                    tracing::info!("Meeting detection actor shutting down");
                    break;
                }

                // Receive an external event
                Some(event) = self.receiver.recv() => {
                    self.handle_event(event).await;
                }

                // Timer fires (only if one is active)
                () = async {
                    if let Some((_, _, ref mut sleep)) = self.active_timer {
                        sleep.as_mut().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    if let Some((kind, gen, _)) = self.active_timer.take() {
                        if gen == self.generation {
                            self.handle_timer(kind).await;
                        }
                        // else: stale timer, ignore
                    }
                }

                // All senders dropped
                else => break,
            }
        }

        tracing::info!("Meeting detection actor stopped");
    }

    // ========================================================================
    // EVENT HANDLING
    // ========================================================================

    async fn handle_event(&mut self, event: DetectionEvent) {
        match event {
            DetectionEvent::AppsUsingAudio(apps) => {
                self.handle_apps_update(apps).await;
            }
            DetectionEvent::PopupAccepted { app_identifier, generation } => {
                self.handle_popup_accepted(&app_identifier, generation).await;
            }
            DetectionEvent::PopupDismissed { app_identifier } => {
                self.handle_popup_dismissed(&app_identifier);
            }
            DetectionEvent::ManualRecordingStarted => {
                self.manual_recording_active = true;
                // Dismiss any active popup
                let _ = self.effect_sender.send(SideEffect::DismissPopup).await;
            }
            DetectionEvent::ManualRecordingStopped => {
                self.manual_recording_active = false;
            }
            DetectionEvent::Shutdown => {
                self.shutdown.cancel();
            }
        }
    }

    /// Core detection logic: diff current apps against previous snapshot.
    async fn handle_apps_update(&mut self, current_apps: Vec<AppInfo>) {
        // Build current set of identifiers
        let current_ids: HashSet<String> = current_apps
            .iter()
            .map(|a| a.identifier.clone())
            .collect();

        // Compute diff
        let started: Vec<&AppInfo> = current_apps
            .iter()
            .filter(|a| !self.previous_apps.contains(&a.identifier))
            .collect();

        let stopped: Vec<String> = self
            .previous_apps
            .difference(&current_ids)
            .cloned()
            .collect();

        self.previous_apps = current_ids;

        // Suppress all detection while recording (manual or auto)
        if self.manual_recording_active {
            return;
        }

        // Check if we're currently recording via detection
        let currently_recording_app = self.app_states.values().find_map(|s| {
            if let DetectionState::Recording { triggered_by_app } = s {
                Some(triggered_by_app.clone())
            } else {
                None
            }
        });

        // Handle newly started apps
        for app in &started {
            if self.ignore_list.contains(&app.identifier) {
                continue;
            }
            // Skip browsers without meeting titles (handled separately in Phase 2)
            if app.is_browser {
                // For now, skip browsers entirely. Phase 2 adds title checking.
                continue;
            }
            if currently_recording_app.is_some() {
                // Already recording — don't show another popup
                continue;
            }
            self.on_app_audio_started(app);
        }

        // Handle stopped apps
        for app_id in &stopped {
            self.on_app_audio_stopped(app_id).await;
        }
    }

    /// App started using audio — begin threshold timer if eligible.
    fn on_app_audio_started(&mut self, app: &AppInfo) {
        let state = self
            .app_states
            .entry(app.identifier.clone())
            .or_insert(DetectionState::Idle {
                cooldown_until: None,
            });

        match state {
            DetectionState::Idle { cooldown_until } => {
                // Check cooldown
                if let Some(until) = cooldown_until {
                    if Instant::now() < *until {
                        tracing::debug!(
                            "App {} still in cooldown, skipping",
                            app.display_name
                        );
                        return;
                    }
                }

                // Start threshold timer
                self.generation += 1;
                let gen = self.generation;
                *state = DetectionState::ThresholdPending { generation: gen };

                let hash = hash_string(&app.identifier);
                self.set_timer(
                    TimerKind::Threshold {
                        app_identifier_hash: hash,
                    },
                    Duration::from_secs(self.settings.threshold_seconds),
                    gen,
                );

                tracing::info!(
                    "Meeting app detected: {} — threshold timer started ({}s)",
                    app.display_name,
                    self.settings.threshold_seconds
                );
            }
            DetectionState::GracePeriod { .. } => {
                // Audio resumed during grace period — cancel auto-stop, back to recording
                tracing::info!(
                    "Audio resumed during grace period for {}",
                    app.display_name
                );
                if let Some(recording_app) = self.find_recording_app() {
                    *self.app_states.get_mut(&app.identifier).unwrap() =
                        DetectionState::Recording {
                            triggered_by_app: recording_app,
                        };
                    self.cancel_timer();
                }
            }
            _ => {
                // Already in threshold/popup/recording — ignore
            }
        }
    }

    /// App stopped using audio — may trigger grace period.
    async fn on_app_audio_stopped(&mut self, app_id: &str) {
        let state = match self.app_states.get(app_id) {
            Some(s) => s.clone(),
            None => return,
        };

        match state {
            DetectionState::ThresholdPending { .. } => {
                // Audio stopped before threshold — cancel, return to idle
                tracing::info!("Audio stopped before threshold for {}", app_id);
                self.app_states.insert(
                    app_id.to_string(),
                    DetectionState::Idle {
                        cooldown_until: None,
                    },
                );
                self.cancel_timer();
            }
            DetectionState::Recording { .. } => {
                // Meeting app audio stopped — start grace period
                self.generation += 1;
                let gen = self.generation;
                self.app_states.insert(
                    app_id.to_string(),
                    DetectionState::GracePeriod { generation: gen },
                );
                self.set_timer(
                    TimerKind::GracePeriod,
                    Duration::from_secs(self.settings.grace_period_seconds),
                    gen,
                );

                tracing::info!(
                    "Meeting audio ended for {} — grace period started ({}s)",
                    app_id,
                    self.settings.grace_period_seconds
                );

                let _ = self
                    .effect_sender
                    .send(SideEffect::ShowGraceNotification {
                        seconds_remaining: self.settings.grace_period_seconds as u32,
                    })
                    .await;
            }
            _ => {}
        }
    }

    /// User clicked "Start Recording" in popup.
    async fn handle_popup_accepted(&mut self, app_id: &str, generation: u64) {
        let state = match self.app_states.get(app_id) {
            Some(s) => s,
            None => {
                tracing::warn!("Popup accepted for unknown app: {}", app_id);
                return;
            }
        };

        // Verify generation matches (prevents stale popup clicks)
        if let DetectionState::PopupShown {
            generation: current_gen,
            ..
        } = state
        {
            if *current_gen != generation {
                tracing::warn!("Stale popup click for {} (gen {} vs {})", app_id, generation, current_gen);
                return;
            }
        } else {
            tracing::warn!("Popup accepted but app {} not in PopupShown state", app_id);
            return;
        }

        // Transition to Recording
        self.app_states.insert(
            app_id.to_string(),
            DetectionState::Recording {
                triggered_by_app: app_id.to_string(),
            },
        );
        self.cancel_timer();

        // Determine meeting name from app display name
        let meeting_name = self
            .previous_apps
            .iter()
            .find(|id| id.as_str() == app_id)
            .cloned()
            .unwrap_or_else(|| app_id.to_string());

        let _ = self
            .effect_sender
            .send(SideEffect::StartRecording {
                meeting_name: sanitize_meeting_name(&meeting_name),
            })
            .await;
    }

    /// User dismissed popup.
    fn handle_popup_dismissed(&mut self, app_id: &str) {
        let cooldown_until = Instant::now()
            + Duration::from_secs(self.settings.cooldown_minutes * 60);

        self.app_states.insert(
            app_id.to_string(),
            DetectionState::Idle {
                cooldown_until: Some(cooldown_until),
            },
        );
        self.cancel_timer();

        tracing::info!(
            "Popup dismissed for {} — cooldown for {} minutes",
            app_id,
            self.settings.cooldown_minutes
        );
    }

    // ========================================================================
    // TIMER HANDLING
    // ========================================================================

    async fn handle_timer(&mut self, kind: TimerKind) {
        match kind {
            TimerKind::Threshold { app_identifier_hash } => {
                // Find the app that matches this threshold
                let app_id = self
                    .app_states
                    .iter()
                    .find(|(id, state)| {
                        matches!(state, DetectionState::ThresholdPending { .. })
                            && hash_string(id) == app_identifier_hash
                    })
                    .map(|(id, _)| id.clone());

                if let Some(app_id) = app_id {
                    // Threshold elapsed — show popup
                    self.generation += 1;
                    let gen = self.generation;
                    self.app_states.insert(
                        app_id.clone(),
                        DetectionState::PopupShown {
                            generation: gen,
                            shown_at: Instant::now(),
                        },
                    );

                    // Set popup timeout (60 seconds)
                    self.set_timer(TimerKind::PopupTimeout, Duration::from_secs(60), gen);

                    let display_name = app_id.clone(); // TODO: resolve to display name

                    tracing::info!("Threshold elapsed for {} — showing popup", display_name);

                    let _ = self
                        .effect_sender
                        .send(SideEffect::ShowPopup {
                            app_name: display_name,
                            app_identifier: app_id,
                            meeting_title: None,
                            generation: gen,
                        })
                        .await;
                }
            }
            TimerKind::PopupTimeout => {
                // Popup timed out — treat as dismiss
                let popup_app = self
                    .app_states
                    .iter()
                    .find(|(_, state)| matches!(state, DetectionState::PopupShown { .. }))
                    .map(|(id, _)| id.clone());

                if let Some(app_id) = popup_app {
                    tracing::info!("Popup timed out for {}", app_id);
                    self.handle_popup_dismissed(&app_id);
                    let _ = self.effect_sender.send(SideEffect::DismissPopup).await;
                }
            }
            TimerKind::GracePeriod => {
                // Grace period expired — auto-stop recording
                let grace_app = self
                    .app_states
                    .iter()
                    .find(|(_, state)| matches!(state, DetectionState::GracePeriod { .. }))
                    .map(|(id, _)| id.clone());

                if let Some(app_id) = grace_app {
                    tracing::info!("Grace period expired for {} — auto-stopping recording", app_id);

                    let cooldown_until = Instant::now()
                        + Duration::from_secs(self.settings.cooldown_minutes * 60);

                    self.app_states.insert(
                        app_id,
                        DetectionState::Idle {
                            cooldown_until: Some(cooldown_until),
                        },
                    );

                    let _ = self.effect_sender.send(SideEffect::StopRecording).await;
                }
            }
        }
    }

    // ========================================================================
    // TIMER HELPERS
    // ========================================================================

    fn set_timer(&mut self, kind: TimerKind, duration: Duration, generation: u64) {
        let sleep = Box::pin(tokio::time::sleep(duration));
        self.active_timer = Some((kind, generation, sleep));
    }

    fn cancel_timer(&mut self) {
        self.generation += 1;
        self.active_timer = None;
    }

    // ========================================================================
    // UTILITY
    // ========================================================================

    fn find_recording_app(&self) -> Option<String> {
        self.app_states
            .iter()
            .find_map(|(_, state)| {
                if let DetectionState::Recording { triggered_by_app } = state {
                    Some(triggered_by_app.clone())
                } else {
                    None
                }
            })
    }
}

/// Simple string hash for timer identification
fn hash_string(s: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app(id: &str, name: &str) -> AppInfo {
        AppInfo {
            identifier: id.to_string(),
            display_name: name.to_string(),
            is_browser: false,
        }
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_detection_threshold_fires_popup() {
        let (effect_tx, mut effect_rx) = mpsc::channel(32);
        let (handle, _shutdown) = spawn_actor(
            DetectionSettings {
                enabled: true,
                threshold_seconds: 5,
                cooldown_minutes: 10,
                grace_period_seconds: 30,
            },
            effect_tx,
        );

        // Detect a meeting app
        handle
            .send(DetectionEvent::AppsUsingAudio(vec![test_app(
                "us.zoom.xos",
                "zoom.us",
            )]))
            .await;

        // Advance past threshold
        tokio::time::advance(Duration::from_secs(6)).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        // Should get ShowPopup effect
        let effect = tokio::time::timeout(Duration::from_millis(100), effect_rx.recv())
            .await
            .expect("timeout waiting for effect")
            .expect("channel closed");

        assert!(
            matches!(effect, SideEffect::ShowPopup { .. }),
            "expected ShowPopup, got {:?}",
            effect
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_audio_stop_cancels_threshold() {
        let (effect_tx, mut effect_rx) = mpsc::channel(32);
        let (handle, _shutdown) = spawn_actor(
            DetectionSettings {
                enabled: true,
                threshold_seconds: 5,
                cooldown_minutes: 10,
                grace_period_seconds: 30,
            },
            effect_tx,
        );

        // Detect app
        handle
            .send(DetectionEvent::AppsUsingAudio(vec![test_app(
                "us.zoom.xos",
                "zoom.us",
            )]))
            .await;

        // Wait 3s (before threshold)
        tokio::time::advance(Duration::from_secs(3)).await;
        tokio::task::yield_now().await;

        // App audio stops
        handle
            .send(DetectionEvent::AppsUsingAudio(vec![]))
            .await;
        tokio::task::yield_now().await;

        // Advance past when threshold would have fired
        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::task::yield_now().await;

        // Should NOT get any effect
        let result = tokio::time::timeout(Duration::from_millis(100), effect_rx.recv()).await;
        assert!(result.is_err(), "should not have received any effect");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_manual_recording_suppresses_detection() {
        let (effect_tx, mut effect_rx) = mpsc::channel(32);
        let (handle, _shutdown) = spawn_actor(
            DetectionSettings {
                enabled: true,
                threshold_seconds: 5,
                cooldown_minutes: 10,
                grace_period_seconds: 30,
            },
            effect_tx,
        );

        // Start manual recording
        handle.send(DetectionEvent::ManualRecordingStarted).await;
        tokio::task::yield_now().await;

        // Detect app — should be suppressed
        handle
            .send(DetectionEvent::AppsUsingAudio(vec![test_app(
                "us.zoom.xos",
                "zoom.us",
            )]))
            .await;

        tokio::time::advance(Duration::from_secs(20)).await;
        tokio::task::yield_now().await;

        let result = tokio::time::timeout(Duration::from_millis(100), effect_rx.recv()).await;
        assert!(result.is_err(), "should not show popup during manual recording");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_cooldown_prevents_repeated_popup() {
        let (effect_tx, mut effect_rx) = mpsc::channel(32);
        let (handle, _shutdown) = spawn_actor(
            DetectionSettings {
                enabled: true,
                threshold_seconds: 2,
                cooldown_minutes: 10,
                grace_period_seconds: 30,
            },
            effect_tx,
        );

        // First detection cycle
        handle
            .send(DetectionEvent::AppsUsingAudio(vec![test_app(
                "us.zoom.xos",
                "zoom.us",
            )]))
            .await;

        tokio::time::advance(Duration::from_secs(3)).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        // Consume the ShowPopup
        let _ = tokio::time::timeout(Duration::from_millis(100), effect_rx.recv()).await;

        // Dismiss popup
        handle
            .send(DetectionEvent::PopupDismissed {
                app_identifier: "us.zoom.xos".to_string(),
            })
            .await;
        tokio::task::yield_now().await;

        // Audio stops then starts again
        handle.send(DetectionEvent::AppsUsingAudio(vec![])).await;
        tokio::task::yield_now().await;

        handle
            .send(DetectionEvent::AppsUsingAudio(vec![test_app(
                "us.zoom.xos",
                "zoom.us",
            )]))
            .await;

        tokio::time::advance(Duration::from_secs(3)).await;
        tokio::task::yield_now().await;

        // Should NOT get another popup (cooldown active)
        let result = tokio::time::timeout(Duration::from_millis(100), effect_rx.recv()).await;
        // We might get a DismissPopup from the popup timeout, but not another ShowPopup
        if let Ok(Some(effect)) = result {
            assert!(
                !matches!(effect, SideEffect::ShowPopup { .. }),
                "should not show popup during cooldown"
            );
        }
    }
}
