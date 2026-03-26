// audio/meeting_detection/mod.rs
//
// Automatic meeting detection via system audio monitoring.
// Uses CoreAudio event-driven detection on macOS, with an actor-based
// state machine for threshold timing, cooldown, and grace periods.

pub mod coordinator;
pub mod commands;
pub mod browser_detector;
pub mod mic_detector;

use serde::{Deserialize, Serialize};
use std::time::Instant;

// ============================================================================
// APP INFO
// ============================================================================

/// Information about an app using audio, with both identifiers
#[derive(Debug, Clone)]
pub struct AppInfo {
    /// Bundle ID on macOS (e.g., "us.zoom.xos"), process name on Windows
    pub identifier: String,
    /// Human-readable display name (e.g., "zoom.us")
    pub display_name: String,
    /// Whether this is a known browser (triggers title checking)
    pub is_browser: bool,
    /// Process ID (used for accessibility API queries on macOS)
    pub pid: Option<i32>,
}

// ============================================================================
// DETECTION STATE (per monitored app)
// ============================================================================

#[derive(Debug, Clone)]
pub enum DetectionState {
    Idle {
        cooldown_until: Option<Instant>,
    },
    ThresholdPending {
        generation: u64,
    },
    PopupShown {
        generation: u64,
        shown_at: Instant,
    },
    Recording {
        triggered_by_app: String,
    },
    GracePeriod {
        generation: u64,
    },
}

// ============================================================================
// EVENTS (inbound to actor)
// ============================================================================

#[derive(Debug)]
pub enum DetectionEvent {
    /// Updated list of apps currently using audio
    AppsUsingAudio(Vec<AppInfo>),
    /// User clicked "Start Recording" in popup
    PopupAccepted {
        app_identifier: String,
        generation: u64,
    },
    /// User dismissed popup or it timed out
    PopupDismissed {
        app_identifier: String,
    },
    /// Manual recording started (suppress detection)
    ManualRecordingStarted,
    /// Manual recording stopped
    ManualRecordingStopped,
    /// Shut down the actor
    Shutdown,
}

// ============================================================================
// SIDE EFFECTS (outbound from actor)
// ============================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum SideEffect {
    /// Meeting detected — show native notification + update tray menu
    MeetingDetected {
        app_name: String,
        app_identifier: String,
        meeting_title: Option<String>,
    },
    /// Meeting detection cleared (timeout/dismissed) — revert tray menu
    MeetingDetectionCleared,
    StartRecording {
        meeting_name: String,
    },
    StopRecording,
    ShowGraceNotification {
        seconds_remaining: u32,
    },
}

/// Detected meeting info shared between coordinator and tray menu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedMeeting {
    pub app_name: String,
    pub app_identifier: String,
    pub meeting_title: Option<String>,
}

// ============================================================================
// TIMER TYPES
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimerKind {
    Threshold { app_identifier_hash: u64 },
    PopupTimeout,
    GracePeriod,
}

// ============================================================================
// SETTINGS
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionSettings {
    pub enabled: bool,
    pub threshold_seconds: u64,
    pub cooldown_minutes: u64,
    pub grace_period_seconds: u64,
}

impl Default for DetectionSettings {
    fn default() -> Self {
        Self {
            enabled: true, // Enabled by default
            threshold_seconds: 15,
            cooldown_minutes: 10,
            grace_period_seconds: 30,
        }
    }
}

// ============================================================================
// DEFAULT IGNORE LISTS
// ============================================================================

/// macOS bundle IDs to ignore (non-meeting audio apps)
#[cfg(target_os = "macos")]
pub(crate) const MACOS_IGNORE_BUNDLE_IDS: &[&str] = &[
    "com.apple.SpeechRecognitionCore",
    "com.apple.dictation",
    "com.apple.Siri",
    "com.apple.ScreenCaptureAgent",
    "com.apple.QuickTimePlayerX",
    "com.apple.Music",
    "com.spotify.client",
    "com.meetily.app",
    "com.apple.garageband",
    "com.apple.LogicPro",
    "org.audacityteam.audacity",
    "com.apple.SystemSounds",
    "com.apple.audio.SandboxHelper",
];

/// Known browser bundle IDs on macOS (trigger title checking)
#[cfg(target_os = "macos")]
pub(crate) const MACOS_BROWSER_BUNDLE_IDS: &[&str] = &[
    "com.google.Chrome",
    "com.microsoft.edgemac",
    "com.brave.Browser",
    "company.thebrowser.Browser", // Arc
];

/// Meeting title patterns to match in browser window titles
pub(crate) const MEETING_TITLE_PATTERNS: &[&str] = &[
    "Meet - ",         // Google Meet: "Meet - Weekly Standup"
    "Google Meet",     // Alternative Google Meet title format
    "Microsoft Teams", // Teams web
    "Zoom Meeting",    // Zoom web client
    "Zoom Webinar",    // Zoom webinar
];

/// Sanitize externally-sourced meeting name for display.
/// Truncates, strips control characters, prevents injection.
pub(crate) fn sanitize_meeting_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control() || *c == ' ')
        .take(100)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_meeting_name_normal() {
        assert_eq!(sanitize_meeting_name("Weekly Standup"), "Weekly Standup");
    }

    #[test]
    fn test_sanitize_meeting_name_strips_control_chars() {
        assert_eq!(sanitize_meeting_name("Meet\x00ing\x07"), "Meeting");
    }

    #[test]
    fn test_sanitize_meeting_name_truncates() {
        let long_name = "a".repeat(200);
        assert_eq!(sanitize_meeting_name(&long_name).len(), 100);
    }

    #[test]
    fn test_sanitize_meeting_name_strips_newlines() {
        assert_eq!(sanitize_meeting_name("Meet\ning"), "Meeting");
    }

    #[test]
    fn test_sanitize_meeting_name_trims_whitespace() {
        assert_eq!(sanitize_meeting_name("  Hello  "), "Hello");
    }
}
