use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConnectionStatus {
    Disconnected,
    Connected { email: String },
}

/// Shape surfaced to the frontend event list. Times are ISO-8601 strings
/// with timezone offsets — the frontend uses `new Date()` for formatting
/// so we preserve the original zone info from Google.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEventDto {
    pub id: String,
    pub title: String,
    pub start: String,
    pub end: String,
    pub organizer: Option<String>,
    pub attendee_count: u32,
    pub is_recurring: bool,
}
