//! Tauri command surface for the detection module.
//!
//! Exposes the otherwise-private `DetectorState` API to the frontend
//! and to agents via the invoke bridge.

use tauri::{State, Wry};

use crate::detection::service::DetectionService;
use crate::detection::state::DetectorPhaseSnapshot;

/// Suppress detection events for this bundle for the dismissal cooldown
/// (default 10 min). Used by the "ignore this app" UX path.
#[tauri::command]
pub async fn dismiss_detected_meeting(
    bundle_id: String,
    service: State<'_, DetectionService>,
) -> Result<(), String> {
    service.dismiss(&bundle_id).await;
    Ok(())
}

/// Return the detector's current phase, timing, and recording state.
/// Useful for UI indicators + agent observation.
#[tauri::command]
pub async fn get_detection_state(
    service: State<'_, DetectionService>,
) -> Result<DetectorPhaseSnapshot, String> {
    Ok(service.current_phase().await)
}

// Keep Wry in scope so the generated handler types match the rest of
// the app's invoke_handler registrations.
#[allow(dead_code)]
fn _check_handler_type(_: tauri::AppHandle<Wry>) {}
