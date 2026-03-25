// audio/meeting_detection/commands.rs
//
// Tauri commands for meeting detection + settings persistence.

use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_store::StoreExt;

use super::coordinator::{MeetingDetectionHandle, spawn_actor};
use super::{DetectionEvent, DetectionSettings, SideEffect};

// ============================================================================
// MANAGED STATE
// ============================================================================

pub struct MeetingDetectionState {
    pub handle: Option<MeetingDetectionHandle>,
    pub shutdown: Option<tokio_util::sync::CancellationToken>,
    pub settings: DetectionSettings,
}

impl Default for MeetingDetectionState {
    fn default() -> Self {
        Self {
            handle: None,
            shutdown: None,
            settings: DetectionSettings::default(),
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
        Ok(store) => {
            match store.get(SETTINGS_KEY) {
                Some(value) => {
                    serde_json::from_value::<DetectionSettings>(value.clone())
                        .unwrap_or_default()
                }
                None => DetectionSettings::default(),
            }
        }
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
/// Runs as a background task, creating popup windows and starting/stopping recording.
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
        SideEffect::ShowPopup {
            app_name,
            app_identifier,
            meeting_title,
            generation,
        } => {
            show_meeting_popup(app, &app_name, &app_identifier, meeting_title.as_deref(), generation);
        }
        SideEffect::DismissPopup => {
            dismiss_meeting_popup(app);
        }
        SideEffect::StartRecording { meeting_name } => {
            tracing::info!("Auto-detection starting recording: {}", meeting_name);
            // Call the shared internal recording function
            match super::super::recording_commands::start_recording_with_meeting_name(
                app.clone(),
                Some(meeting_name),
            )
            .await
            {
                Ok(()) => {
                    // Emit event for frontend sync
                    let _ = app.emit("recording-started-by-detection", serde_json::json!({
                        "source": "meeting-detection"
                    }));
                }
                Err(e) => {
                    tracing::error!("Failed to start recording from detection: {}", e);
                }
            }
        }
        SideEffect::StopRecording => {
            tracing::info!("Auto-detection stopping recording");
            let args = super::super::recording_commands::RecordingArgs {
                save_path: String::new(),
            };
            match super::super::recording_commands::stop_recording(app.clone(), args).await {
                Ok(()) => {
                    tracing::info!("Auto-stop recording completed");
                }
                Err(e) => {
                    tracing::error!("Failed to auto-stop recording: {}", e);
                }
            }
        }
        SideEffect::ShowGraceNotification { seconds_remaining } => {
            // Use basic notification for grace period warning
            tracing::info!(
                "Grace period: recording stops in {}s",
                seconds_remaining
            );
            // TODO: Show notification via tauri-plugin-notification (no action needed)
        }
    }
}

// ============================================================================
// POPUP WINDOW MANAGEMENT
// ============================================================================

fn show_meeting_popup<R: Runtime>(
    app: &AppHandle<R>,
    app_name: &str,
    app_identifier: &str,
    meeting_title: Option<&str>,
    generation: u64,
) {
    // Close existing popup if any
    dismiss_meeting_popup(app);

    // Create the popup window
    match tauri::WebviewWindowBuilder::new(
        app,
        "meeting-popup",
        tauri::WebviewUrl::App("/popup/meeting-detected".into()),
    )
    .title("Meeting Detected")
    .inner_size(380.0, 180.0)
    .decorations(false)
    .always_on_top(true)
    .resizable(false)
    .skip_taskbar(true)
    .focused(false) // CRITICAL: don't steal focus from meeting
    .center()
    .build()
    {
        Ok(_window) => {
            // Send data to the popup after a short delay to ensure it's loaded
            let app_clone = app.clone();
            let app_name_owned = app_name.to_string();
            let app_identifier_owned = app_identifier.to_string();
            let meeting_title_owned = meeting_title.map(|s| s.to_string());

            tracing::info!("Meeting popup shown for: {}", app_name_owned);

            tokio::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                let _ = app_clone.emit_to(
                    "meeting-popup",
                    "meeting-detected-data",
                    serde_json::json!({
                        "appName": app_name_owned,
                        "appIdentifier": app_identifier_owned,
                        "meetingTitle": meeting_title_owned,
                        "generation": generation,
                    }),
                );
            });
        }
        Err(e) => {
            tracing::error!("Failed to create meeting popup window: {}", e);
        }
    }
}

fn dismiss_meeting_popup<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("meeting-popup") {
        let _ = window.close();
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
        return Ok(()); // Already running
    }

    // Load settings
    let settings = load_settings(&app).await;
    guard.settings = settings.clone();
    guard.settings.enabled = true;

    // Save enabled state
    save_settings(&app, &guard.settings).await?;

    // Spawn actor
    let (effect_tx, effect_rx) = mpsc::channel(32);
    let (handle, shutdown) = spawn_actor(guard.settings.clone(), effect_tx);

    // Spawn effect handler
    spawn_effect_handler(app.clone(), effect_rx, shutdown.clone());

    // Start system audio monitoring to feed events to the actor
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

    guard.settings.enabled = false;
    save_settings(&app, &guard.settings).await?;

    // Dismiss any open popup
    dismiss_meeting_popup(&app);

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

/// Called by the popup window when user clicks "Start Recording"
#[tauri::command]
pub async fn popup_start_recording(
    app_identifier: String,
    generation: u64,
    state: tauri::State<'_, MeetingDetectionManagedState>,
) -> Result<(), String> {
    let guard = state.lock().await;
    if let Some(handle) = &guard.handle {
        handle
            .send(DetectionEvent::PopupAccepted {
                app_identifier,
                generation,
            })
            .await;
    }
    Ok(())
}

/// Called by the popup window when user clicks "Dismiss"
#[tauri::command]
pub async fn popup_dismiss(
    app_identifier: String,
    state: tauri::State<'_, MeetingDetectionManagedState>,
) -> Result<(), String> {
    let guard = state.lock().await;
    if let Some(handle) = &guard.handle {
        handle
            .send(DetectionEvent::PopupDismissed { app_identifier })
            .await;
    }
    Ok(())
}

// ============================================================================
// SYSTEM AUDIO MONITORING BRIDGE
// ============================================================================

/// Starts CoreAudio monitoring and feeds events to the detection actor.
/// Uses the detailed app list (with bundle IDs) for proper identification.
fn start_audio_monitoring<R: Runtime>(
    _app: AppHandle<R>,
    handle: MeetingDetectionHandle,
    shutdown: tokio_util::sync::CancellationToken,
) {
    #[cfg(target_os = "macos")]
    {
        use crate::audio::system_detector::{
            new_system_audio_callback, SystemAudioDetector, SystemAudioEvent,
        };

        let handle_clone = handle.clone();

        // Create callback that sends events to the actor.
        // AudioAppInfo now comes directly from the event (resolved in the
        // same thread that queries CoreAudio, avoiding empty re-query issues).
        let callback = new_system_audio_callback(move |event| {
            match event {
                SystemAudioEvent::SystemAudioStarted(audio_apps) => {
                    tracing::info!(
                        "Meeting detection: audio apps detected: {:?}",
                        audio_apps.iter().map(|a| format!("{}({})", a.display_name, a.bundle_id)).collect::<Vec<_>>()
                    );

                    let apps: Vec<super::AppInfo> = audio_apps
                        .into_iter()
                        .map(|app| {
                            // Use starts_with for browser matching — Chrome helper
                            // processes have IDs like "com.google.Chrome.helper"
                            let is_browser = super::MACOS_BROWSER_BUNDLE_IDS
                                .iter()
                                .any(|bid| app.bundle_id.starts_with(bid));

                            if is_browser {
                                tracing::info!(
                                    "Meeting detection: identified browser: {} ({}), pid={}",
                                    app.display_name, app.bundle_id, app.pid
                                );
                            }

                            super::AppInfo {
                                identifier: app.bundle_id,
                                display_name: app.display_name,
                                is_browser,
                                pid: Some(app.pid),
                            }
                        })
                        .collect();

                    handle_clone.try_send(DetectionEvent::AppsUsingAudio(apps));
                }
                SystemAudioEvent::SystemAudioStopped => {
                    // Send empty list to trigger diff (stopped apps detected)
                    handle_clone.try_send(DetectionEvent::AppsUsingAudio(vec![]));
                }
            }
        });

        // Start the detector in a background task
        tokio::spawn(async move {
            let mut detector = SystemAudioDetector::new();
            detector.start(callback);

            // Keep alive until shutdown
            shutdown.cancelled().await;
            detector.stop();
            tracing::info!("System audio monitoring stopped");
        });
    }

    #[cfg(not(target_os = "macos"))]
    {
        tracing::warn!("Meeting detection audio monitoring not yet supported on this platform");
    }
}
