use serde::{Deserialize, Serialize};

/// A meeting detected by the mic-activity signal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedMeeting {
    pub bundle_id: String,
    pub display_name: String,
}

/// Events the detector emits as its internal state advances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectionEvent {
    /// A meeting has crossed the sustain threshold.
    MeetingDetected(DetectedMeeting),
    /// A previously-detected meeting has released the mic for the end-silence window.
    MeetingEnded(DetectedMeeting),
}

/// Snapshot of which non-Meetily apps are currently holding the mic.
#[derive(Debug, Clone, Default)]
pub struct MicSnapshot {
    pub active_bundles: Vec<String>,
}

impl MicSnapshot {
    pub fn contains(&self, bundle: &str) -> bool {
        self.active_bundles.iter().any(|b| b == bundle)
    }
}
