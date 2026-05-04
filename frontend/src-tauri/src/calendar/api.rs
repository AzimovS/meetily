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

use once_cell::sync::Lazy;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::calendar::oauth;
use crate::calendar::token_store::{StoredTokens, TokenKey, TokenStore};
use crate::calendar::types::{
    CalendarEventDto, FrozenAttendee, FrozenCalendarContext, FrozenPerson,
};

/// Refresh access tokens when they're within this many seconds of expiry.
/// 60s is enough slack to avoid races with in-flight requests.
const REFRESH_LEEWAY_SECS: u64 = 60;

const CALENDAR_API_BASE: &str = "https://www.googleapis.com/calendar/v3";

/// Global singleflight lock for token refresh. Multi-account (if it
/// lands) would key this by `TokenKey`; v1 has one account slot, so a
/// single Mutex suffices. Without this, two concurrent callers that
/// both observe an expired access_token double-refresh, causing quota
/// waste and a last-write-wins race on keyring save.
static REFRESH_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

/// Load the stored tokens, refreshing if near expiry, and return a valid
/// access_token plus the (possibly updated) StoredTokens. Refresh is
/// serialized via a global Mutex with double-checked locking — only the
/// first caller of N concurrent refresh candidates actually hits
/// Google's token endpoint; the others observe the refreshed tokens
/// post-lock and skip the refresh.
pub async fn get_fresh_access_token<S: TokenStore>(
    store: &S,
    key: TokenKey,
) -> Result<(String, StoredTokens), String> {
    let tokens = store
        .load(key)
        .await?
        .ok_or_else(|| "No stored tokens — user is not connected".to_string())?;

    if !is_near_expiry(&tokens) {
        return Ok((tokens.access_token.clone(), tokens));
    }

    // Acquire the refresh lock. Hold it across the network call and the
    // keyring write so concurrent callers serialize.
    let _guard = REFRESH_LOCK.lock().await;

    // Double-check: another caller may have refreshed while we waited.
    let tokens = store
        .load(key)
        .await?
        .ok_or_else(|| "No stored tokens — user is not connected".to_string())?;
    if !is_near_expiry(&tokens) {
        return Ok((tokens.access_token.clone(), tokens));
    }

    let refresh_token = tokens
        .refresh_token
        .as_deref()
        .ok_or_else(|| {
            "Access token expired and no refresh_token available — reconnect required".to_string()
        })?;

    log::info!("[calendar] Access token near expiry; refreshing");
    let refreshed = oauth::refresh_access_token(refresh_token).await?;

    let now = now_secs();
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

fn is_near_expiry(tokens: &StoredTokens) -> bool {
    let now = now_secs();
    tokens
        .expires_at
        .map(|t| now + REFRESH_LEEWAY_SECS >= t)
        .unwrap_or(false)
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
        return Err(format!("Fetch primary calendar returned {status}"));
    }

    let parsed: CalendarResource = serde_json::from_str(&body)
        .map_err(|_| format!("Primary calendar response malformed (status {status})"))?;
    Ok(parsed.id)
}

/// List events on the primary calendar from now to end-of-tomorrow in
/// the user's local timezone. Filters out cancelled events, all-day
/// events, working-location / focus / OOO blocks, and events the user
/// has declined.
///
/// **Use this for the UI event list only.** For matching against a
/// completed recording, use `list_events_in_window`: this function's
/// `timeMin = now` excludes events that ended at or just before the
/// recording stopped — exactly the events you want to match against.
pub async fn list_upcoming_events(
    access_token: &str,
) -> Result<Vec<CalendarEventDto>, String> {
    let (time_min, time_max) = window_today_plus_tomorrow();
    fetch_events_in_window(access_token, &time_min, &time_max).await
}

/// List events on the primary calendar that overlap a given window.
/// Used by `api_calendar_auto_match_and_link` so a recording that
/// stops just after a meeting ended still sees that meeting.
///
/// Google's `timeMin` is exclusive on event *end* time, so passing
/// `time_min = recording_start` (rather than `now`) is the correct
/// way to capture events that ended during the recording.
pub async fn list_events_in_window(
    access_token: &str,
    time_min: chrono::DateTime<chrono::Utc>,
    time_max: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<CalendarEventDto>, String> {
    let tmin = urlencoding(&time_min.to_rfc3339());
    let tmax = urlencoding(&time_max.to_rfc3339());
    fetch_events_in_window(access_token, &tmin, &tmax).await
}

/// Internal: hit the events endpoint with a pre-encoded window, run
/// the standard filter+map. Both public listers funnel through here.
async fn fetch_events_in_window(
    access_token: &str,
    time_min_encoded: &str,
    time_max_encoded: &str,
) -> Result<Vec<CalendarEventDto>, String> {
    let url = format!(
        "{CALENDAR_API_BASE}/calendars/primary/events\
         ?timeMin={time_min_encoded}\
         &timeMax={time_max_encoded}\
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
        return Err(format!("List events returned {status}"));
    }

    let raw: RawEventsResponse = serde_json::from_str(&body)
        .map_err(|_| format!("List events response malformed (status {status})"))?;

    let events = raw
        .items
        .into_iter()
        .filter(keep_event)
        .map(to_dto)
        .collect();

    Ok(events)
}

/// Fetch a single event by id from the user's primary calendar and
/// freeze it into a `FrozenCalendarContext` snapshot ready for
/// persistence. The `captured_at` field is set to "now" inside the
/// snapshot so the UI can show "captured at <time>" if it ever wants
/// to.
pub async fn fetch_event_as_frozen_context(
    access_token: &str,
    event_id: &str,
) -> Result<FrozenCalendarContext, String> {
    let url = format!(
        "{CALENDAR_API_BASE}/calendars/primary/events/{}",
        urlencoding(event_id)
    );

    let http = reqwest::Client::new();
    let response = http
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| format!("Fetch event request failed: {e}"))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Read event body failed: {e}"))?;

    if !status.is_success() {
        return Err(format!("Fetch event returned {status}"));
    }

    let raw: RawEvent = serde_json::from_str(&body)
        .map_err(|_| format!("Event response malformed (status {status})"))?;

    Ok(freeze_raw_event(raw))
}

/// Build a `FrozenCalendarContext` from a parsed Google event. Pure;
/// unit-tested in isolation from any HTTP plumbing.
fn freeze_raw_event(raw: RawEvent) -> FrozenCalendarContext {
    let captured_at = chrono::Utc::now().to_rfc3339();
    let start = raw
        .start
        .as_ref()
        .and_then(|t| t.date_time.clone())
        .unwrap_or_default();
    let end = raw
        .end
        .as_ref()
        .and_then(|t| t.date_time.clone())
        .unwrap_or_default();
    let organizer = raw.organizer.as_ref().map(|p| FrozenPerson {
        display_name: p.display_name.clone(),
        email: p.email.clone(),
    });
    let attendees = raw
        .attendees
        .unwrap_or_default()
        .into_iter()
        .map(|a| FrozenAttendee {
            display_name: a.display_name,
            email: a.email,
            response_status: a.response_status,
        })
        .collect();

    FrozenCalendarContext {
        schema_version: 1,
        source: "google".to_string(),
        event_id: raw.id,
        recurrence_id: raw.recurring_event_id,
        title: raw.summary.unwrap_or_else(|| "(no title)".to_string()),
        description: raw.description,
        organizer,
        attendees,
        start,
        end,
        captured_at,
    }
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
    description: Option<String>,
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
    email: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
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

