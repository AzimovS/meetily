// audio/meeting_detection/commands.rs
//
// Tauri commands for meeting detection + settings persistence.
// Uses tray menu + native notification (no popup windows).

use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_store::StoreExt;

use super::coordinator::{MeetingDetectionHandle, spawn_actor};
use super::{DetectedMeeting, DetectionEvent, DetectionSettings, SideEffect};

// ============================================================================
// MANAGED STATE
// ============================================================================

pub struct MeetingDetectionState {
    pub handle: Option<MeetingDetectionHandle>,
    pub shutdown: Option<tokio_util::sync::CancellationToken>,
    pub settings: DetectionSettings,
    /// Currently detected meeting (shown in tray menu)
    pub detected_meeting: Option<DetectedMeeting>,
}

impl Default for MeetingDetectionState {
    fn default() -> Self {
        Self {
            handle: None,
            shutdown: None,
            settings: DetectionSettings::default(),
            detected_meeting: None,
        }
    }
}

pub type MeetingDetectionManagedState = Arc<Mutex<MeetingDetectionState>>;

pub fn init_meeting_detection_state() -> MeetingDetectionManagedState {
    Arc::new(Mutex::new(MeetingDetectionState::default()))
}

// ============================================================================
// SETTINGS PERSISTENCE
// ============================================================================

const STORE_FILE: &str = "meeting_detection_settings.json";
const SETTINGS_KEY: &str = "detection_settings";

pub async fn load_settings<R: Runtime>(app: &AppHandle<R>) -> DetectionSettings {
    match app.store(STORE_FILE) {
        Ok(store) => match store.get(SETTINGS_KEY) {
            Some(value) => {
                serde_json::from_value::<DetectionSettings>(value.clone()).unwrap_or_default()
            }
            None => DetectionSettings::default(),
        },
        Err(_) => DetectionSettings::default(),
    }
}

pub async fn save_settings<R: Runtime>(
    app: &AppHandle<R>,
    settings: &DetectionSettings,
) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| e.to_string())?;
    let value = serde_json::to_value(settings).map_err(|e| e.to_string())?;
    store.set(SETTINGS_KEY, value);
    store.save().map_err(|e| e.to_string())?;
    Ok(())
}

// ============================================================================
// SIDE EFFECT HANDLER
// ============================================================================

/// Process side effects from the detection actor.
/// Updates tray menu and shows native notifications.
pub fn spawn_effect_handler<R: Runtime>(
    app: AppHandle<R>,
    mut effect_rx: mpsc::Receiver<SideEffect>,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let app_clone = app.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                Some(effect) = effect_rx.recv() => {
                    handle_effect(&app_clone, effect).await;
                }
                else => break,
            }
        }
        tracing::info!("Meeting detection effect handler stopped");
    });
}

async fn handle_effect<R: Runtime>(app: &AppHandle<R>, effect: SideEffect) {
    match effect {
        SideEffect::MeetingDetected {
            app_name,
            app_identifier,
            meeting_title,
        } => {
            let display_name = meeting_title
                .as_deref()
                .unwrap_or(&app_name);

            tracing::info!("Meeting detected: {} — updating tray", display_name);

            // Store detected meeting in shared state
            {
                let state = app.state::<MeetingDetectionManagedState>();
                let mut guard = state.lock().await;
                guard.detected_meeting = Some(DetectedMeeting {
                    app_name: app_name.clone(),
                    app_identifier,
                    meeting_title: meeting_title.clone(),
                });
            }

            // Update tray menu to show "Start Recording (app)"
            crate::tray::update_tray_menu(app);

            // Show native notification (sound + banner, no action buttons needed)
            let body = format!("{} — click tray to start recording", display_name);
            if let Err(e) = app
                .notification()
                .builder()
                .title("Meeting Detected")
                .body(&body)
                .show()
            {
                tracing::warn!("Failed to show meeting notification: {}", e);
            }
        }
        SideEffect::MeetingDetectionCleared => {
            tracing::info!("Meeting detection cleared — reverting tray");

            // Clear detected meeting
            {
                let state = app.state::<MeetingDetectionManagedState>();
                let mut guard = state.lock().await;
                guard.detected_meeting = None;
            }

            // Revert tray menu
            crate::tray::update_tray_menu(app);
        }
        SideEffect::StartRecording { meeting_name } => {
            tracing::info!("Auto-detection starting recording: {}", meeting_name);

            // Clear detected meeting from tray
            {
                let state = app.state::<MeetingDetectionManagedState>();
                let mut guard = state.lock().await;
                guard.detected_meeting = None;
            }

            let app_clone = app.clone();
            tokio::spawn(async move {
                match super::super::recording_commands::start_recording_with_meeting_name(
                    app_clone.clone(),
                    Some(meeting_name),
                )
                .await
                {
                    Ok(()) => {
                        let _ = app_clone.emit(
                            "recording-started-by-detection",
                            serde_json::json!({"source": "meeting-detection"}),
                        );
                    }
                    Err(e) => {
                        tracing::error!("Failed to start recording from detection: {}", e);
                        let _ = app_clone.emit(
                            "recording-error",
                            serde_json::json!({"error": e.to_string(), "source": "meeting-detection"}),
                        );
                    }
                }
            });
        }
        SideEffect::StopRecording => {
            tracing::info!("Auto-detection stopping recording");
            let args = super::super::recording_commands::RecordingArgs {
                save_path: String::new(),
            };
            match super::super::recording_commands::stop_recording(app.clone(), args).await {
                Ok(()) => tracing::info!("Auto-stop recording completed"),
                Err(e) => tracing::error!("Failed to auto-stop recording: {}", e),
            }
        }
        SideEffect::ShowGraceNotification { seconds_remaining } => {
            tracing::info!("Grace period: recording stops in {}s", seconds_remaining);
        }
    }
}

// ============================================================================
// TAURI COMMANDS
// ============================================================================

#[tauri::command]
pub async fn enable_meeting_detection<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, MeetingDetectionManagedState>,
) -> Result<(), String> {
    let mut guard = state.lock().await;

    if guard.handle.is_some() {
        return Ok(());
    }

    let settings = load_settings(&app).await;
    guard.settings = settings.clone();
    guard.settings.enabled = true;

    save_settings(&app, &guard.settings).await?;

    let (effect_tx, effect_rx) = mpsc::channel(32);
    let (handle, shutdown) = spawn_actor(guard.settings.clone(), effect_tx);

    spawn_effect_handler(app.clone(), effect_rx, shutdown.clone());
    start_audio_monitoring(app.clone(), handle.clone(), shutdown.clone());

    guard.handle = Some(handle);
    guard.shutdown = Some(shutdown);

    tracing::info!("Meeting detection enabled");
    Ok(())
}

#[tauri::command]
pub async fn disable_meeting_detection<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, MeetingDetectionManagedState>,
) -> Result<(), String> {
    let mut guard = state.lock().await;

    if let Some(shutdown) = guard.shutdown.take() {
        shutdown.cancel();
    }
    guard.handle = None;
    guard.detected_meeting = None;

    guard.settings.enabled = false;
    save_settings(&app, &guard.settings).await?;

    crate::tray::update_tray_menu(&app);

    tracing::info!("Meeting detection disabled");
    Ok(())
}

#[tauri::command]
pub async fn get_meeting_detection_enabled(
    state: tauri::State<'_, MeetingDetectionManagedState>,
) -> Result<bool, String> {
    let guard = state.lock().await;
    Ok(guard.handle.is_some())
}

/// Called by the tray menu when user clicks "Start Recording (app)"
pub async fn start_detected_recording<R: Runtime>(app: &AppHandle<R>) {
    let (meeting, handle) = {
        let state = app.state::<MeetingDetectionManagedState>();
        let mut guard = state.lock().await;
        let meeting = guard.detected_meeting.take();
        let handle = guard.handle.clone();
        (meeting, handle)
    };

    if let (Some(meeting), Some(handle)) = (meeting, handle) {
        // Tell coordinator the user accepted
        handle
            .send(DetectionEvent::PopupAccepted {
                app_identifier: meeting.app_identifier,
                generation: 0, // Not used for tray-based flow
            })
            .await;
    }

    // Update tray to remove the detected meeting item
    crate::tray::update_tray_menu(app);

    // Hide the app so focus returns to the meeting
    #[cfg(target_os = "macos")]
    {
        let _ = app.hide();
    }
}

// ============================================================================
// MICROPHONE MONITORING (primary meeting detection signal)
// ============================================================================

/// Polls which apps are using the microphone every few seconds.
fn start_audio_monitoring<R: Runtime>(
    _app: AppHandle<R>,
    handle: MeetingDetectionHandle,
    shutdown: tokio_util::sync::CancellationToken,
) {
    tokio::spawn(async move {
        tracing::info!("Meeting detection: mic monitoring started (polling every 3s)");

        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(3));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    tracing::info!("Meeting detection: mic monitoring stopped");
                    break;
                }
                _ = interval.tick() => {
                    let apps = super::mic_detector::list_mic_using_apps();

                    if !apps.is_empty() {
                        tracing::info!(
                            "Meeting detection: apps using mic: {:?}",
                            apps.iter().map(|a| format!("{}({})", a.display_name, a.identifier)).collect::<Vec<_>>()
                        );
                    }

                    handle.try_send(DetectionEvent::AppsUsingAudio(apps));
                }
            }
        }
    });
}
