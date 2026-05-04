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

/// Frozen-at-meeting-end snapshot of a linked calendar event.
///
/// Persisted as JSON in `meetings.calendar_context_json`. The summary
/// pipeline reads this blob to render the `<meeting_context>` block.
/// Versioned from day one (`schema_version`) so future shape changes
/// stay backwards-compatible — old rows decode against v1, new rows
/// against the current version.
///
/// `source` allows for future providers (Outlook, iCloud, .ics drop)
/// without changing the column type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FrozenCalendarContext {
    pub schema_version: u8,
    pub source: String,
    pub event_id: String,
    pub recurrence_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub organizer: Option<FrozenPerson>,
    pub attendees: Vec<FrozenAttendee>,
    pub start: String,
    pub end: String,
    pub captured_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FrozenPerson {
    pub display_name: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FrozenAttendee {
    pub display_name: Option<String>,
    pub email: Option<String>,
    /// Google `responseStatus`: `accepted`, `declined`, `tentative`,
    /// `needsAction`. Captured verbatim — slice 4 (the prompt module)
    /// uses this to order attendees and may filter declined.
    pub response_status: Option<String>,
}
