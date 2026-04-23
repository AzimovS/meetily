//! Google Calendar API client helpers.
//!
//! Handles the common concern of "get me a valid access token" —
//! transparently refreshing via the OAuth refresh_token when the stored
//! access_token is near expiry. All callers go through
//! `get_fresh_access_token` so tokens stay in sync.
//!
//! Network concerns live here. OAuth URL construction + the initial
//! consent dance live in `oauth.rs`. Keychain I/O lives in
//! `token_store.rs`.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::calendar::oauth;
use crate::calendar::token_store::{StoredTokens, TokenKey, TokenStore};
use crate::calendar::types::CalendarEventDto;

/// Refresh access tokens when they're within this many seconds of expiry.
/// 60s is enough slack to avoid races with in-flight requests.
const REFRESH_LEEWAY_SECS: u64 = 60;

const CALENDAR_API_BASE: &str = "https://www.googleapis.com/calendar/v3";

/// Load the stored tokens, refreshing if near expiry, and return a valid
/// access_token plus the (possibly updated) StoredTokens. Callers that
/// persist the returned StoredTokens back to the keychain keep email +
/// refresh_token intact across rotations.
pub async fn get_fresh_access_token<S: TokenStore>(
    store: &S,
    key: TokenKey,
) -> Result<(String, StoredTokens), String> {
    let tokens = store
        .load(key)
        .await?
        .ok_or_else(|| "No stored tokens — user is not connected".to_string())?;

    let now = now_secs();
    let expired = tokens
        .expires_at
        .map(|t| now + REFRESH_LEEWAY_SECS >= t)
        .unwrap_or(false);

    if !expired {
        return Ok((tokens.access_token.clone(), tokens));
    }

    let refresh_token = tokens
        .refresh_token
        .as_deref()
        .ok_or_else(|| "Access token expired and no refresh_token available — reconnect required".to_string())?;

    log::info!("[calendar] Access token near expiry; refreshing");
    let refreshed = oauth::refresh_access_token(refresh_token).await?;

    let new_tokens = StoredTokens {
        access_token: refreshed.access_token.clone(),
        // Google often omits refresh_token on refresh — keep the old one.
        refresh_token: refreshed.refresh_token.or(tokens.refresh_token.clone()),
        expires_at: refreshed.expires_in_secs.map(|s| now + s),
        email: tokens.email.clone(),
    };

    store.save(key, &new_tokens).await?;
    Ok((new_tokens.access_token.clone(), new_tokens))
}

/// Fetch the user's primary calendar metadata. Google returns the
/// account email in the `id` field (and `summary`) for the primary
/// calendar.
pub async fn fetch_primary_calendar_email(access_token: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct CalendarResource {
        id: String,
    }

    let http = reqwest::Client::new();
    let response = http
        .get(format!("{CALENDAR_API_BASE}/calendars/primary"))
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("Fetch primary calendar failed: {e}"))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Read primary calendar body failed: {e}"))?;

    if !status.is_success() {
        return Err(format!("Fetch primary calendar returned {status}: {body}"));
    }

    let parsed: CalendarResource = serde_json::from_str(&body)
        .map_err(|e| format!("Primary calendar response invalid JSON: {e}; body: {body}"))?;
    Ok(parsed.id)
}

/// List events on the primary calendar from now to end-of-tomorrow in
/// the user's local timezone. Filters out cancelled events, all-day
/// events, working-location / focus / OOO blocks, and events the user
/// has declined.
pub async fn list_upcoming_events(
    access_token: &str,
) -> Result<Vec<CalendarEventDto>, String> {
    let (time_min, time_max) = window_today_plus_tomorrow();

    let url = format!(
        "{CALENDAR_API_BASE}/calendars/primary/events\
         ?timeMin={time_min}\
         &timeMax={time_max}\
         &singleEvents=true\
         &orderBy=startTime\
         &maxResults=50"
    );

    let http = reqwest::Client::new();
    let response = http
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("List events request failed: {e}"))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Read list events body failed: {e}"))?;

    if !status.is_success() {
        return Err(format!("List events returned {status}: {body}"));
    }

    let raw: RawEventsResponse = serde_json::from_str(&body)
        .map_err(|e| format!("List events response invalid JSON: {e}"))?;

    let events = raw
        .items
        .into_iter()
        .filter(keep_event)
        .map(to_dto)
        .collect();

    Ok(events)
}

/// Window covering "today through end of tomorrow" as ISO-8601 strings
/// acceptable to Google's `timeMin`/`timeMax`. We use UTC; Google
/// interprets these correctly and returns events with their original
/// timezone metadata for the frontend to render.
fn window_today_plus_tomorrow() -> (String, String) {
    use chrono::{Duration, Utc};
    let now = Utc::now();
    let end = now + Duration::hours(48);
    // URL-encode the `:` in the ISO timestamp to keep Google happy.
    (
        urlencoding(&now.to_rfc3339()),
        urlencoding(&end.to_rfc3339()),
    )
}

fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// --- Raw response shape from Google. Keep private to this module. ----

#[derive(Deserialize)]
struct RawEventsResponse {
    #[serde(default)]
    items: Vec<RawEvent>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEvent {
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    start: Option<RawEventTime>,
    #[serde(default)]
    end: Option<RawEventTime>,
    #[serde(default)]
    organizer: Option<RawPerson>,
    #[serde(default)]
    attendees: Option<Vec<RawAttendee>>,
    #[serde(default)]
    recurring_event_id: Option<String>,
    #[serde(default)]
    event_type: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEventTime {
    #[serde(default)]
    date_time: Option<String>,
    #[serde(default)]
    _date: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPerson {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default, rename = "self")]
    _is_self: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAttendee {
    #[serde(default)]
    _email: Option<String>,
    #[serde(default)]
    response_status: Option<String>,
    #[serde(default, rename = "self")]
    is_self: Option<bool>,
}

fn keep_event(e: &RawEvent) -> bool {
    // Drop cancelled events.
    if e.status.as_deref() == Some("cancelled") {
        return false;
    }
    // Drop all-day events (no dateTime on either end).
    let has_times = e
        .start
        .as_ref()
        .and_then(|t| t.date_time.as_deref())
        .is_some()
        && e.end
            .as_ref()
            .and_then(|t| t.date_time.as_deref())
            .is_some();
    if !has_times {
        return false;
    }
    // Drop OOO/focus/workingLocation synthetic events.
    match e.event_type.as_deref() {
        Some("outOfOffice") | Some("focusTime") | Some("workingLocation") => return false,
        _ => {}
    }
    // Drop events the user has declined.
    if let Some(attendees) = &e.attendees {
        for a in attendees {
            if a.is_self == Some(true) && a.response_status.as_deref() == Some("declined") {
                return false;
            }
        }
    }
    // Drop private events — v1 conservative default. Settings toggle to
    // include them lands with PR 2 privacy controls.
    if e.visibility.as_deref() == Some("private") {
        return false;
    }
    true
}

fn to_dto(e: RawEvent) -> CalendarEventDto {
    let organizer = e
        .organizer
        .as_ref()
        .and_then(|o| o.display_name.clone().or_else(|| o.email.clone()));
    let attendee_count = e.attendees.as_ref().map(|a| a.len() as u32).unwrap_or(0);
    let start = e
        .start
        .as_ref()
        .and_then(|t| t.date_time.clone())
        .unwrap_or_default();
    let end = e
        .end
        .as_ref()
        .and_then(|t| t.date_time.clone())
        .unwrap_or_default();

    CalendarEventDto {
        id: e.id,
        title: e.summary.unwrap_or_else(|| "(no title)".to_string()),
        start,
        end,
        organizer,
        attendee_count,
        is_recurring: e.recurring_event_id.is_some(),
    }
}

