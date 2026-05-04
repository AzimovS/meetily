use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::calendar::api;
use crate::calendar::matching;
use crate::calendar::oauth;
use crate::calendar::repository;
use crate::calendar::token_store::{KeyringTokenStore, StoredTokens, TokenKey, TokenStore};
use crate::calendar::types::{CalendarEventDto, ConnectionStatus};
use crate::database::repositories::meeting::MeetingsRepository;
use crate::state::AppState;

/// Google's OAuth 2.0 token revocation endpoint (RFC 7009).
const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";

#[tauri::command]
pub async fn api_calendar_status() -> Result<ConnectionStatus, String> {
    let store = KeyringTokenStore;
    match store.load(TokenKey::GOOGLE_CALENDAR_DEFAULT).await? {
        Some(tokens) => Ok(ConnectionStatus::Connected {
            email: tokens.email.unwrap_or_else(|| "Connected".to_string()),
        }),
        None => Ok(ConnectionStatus::Disconnected),
    }
}

#[tauri::command]
pub async fn api_calendar_connect() -> Result<ConnectionStatus, String> {
    let store = KeyringTokenStore;

    // NOTE: we deliberately do NOT delete existing tokens before consent.
    // If the OAuth flow fails (user cancels, timeout, network), the user
    // stays connected with their prior tokens rather than being silently
    // logged out.
    let tokens = oauth::connect().await?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut stored = StoredTokens {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at: tokens.expires_in_secs.map(|s| now + s),
        email: None,
    };

    // Best-effort email fetch. Failure is non-blocking.
    match api::fetch_primary_calendar_email(&stored.access_token).await {
        Ok(email) => {
            log::info!("[calendar] Connected to Google Calendar");
            stored.email = Some(email);
        }
        Err(e) => {
            log::warn!("[calendar] Email fetch failed during connect: {e}");
        }
    }

    // Save the new tokens — last write wins, overwriting any prior entry.
    store
        .save(TokenKey::GOOGLE_CALENDAR_DEFAULT, &stored)
        .await?;

    Ok(ConnectionStatus::Connected {
        email: stored.email.unwrap_or_else(|| "Connected".to_string()),
    })
}

#[tauri::command]
pub async fn api_calendar_disconnect() -> Result<(), String> {
    let store = KeyringTokenStore;

    // Delete-first: the keyring is the source of truth for "is this user
    // connected?". Revoking on Google but failing to delete locally would
    // leave the app in a broken state where the next API call hits
    // invalid_grant with no reconnect path.
    let prior = store.load(TokenKey::GOOGLE_CALENDAR_DEFAULT).await?;
    store.delete(TokenKey::GOOGLE_CALENDAR_DEFAULT).await?;
    log::info!("[calendar] Local tokens deleted");

    // Best-effort Google revoke using the refresh_token we just loaded.
    // Failure leaves Google's server-side grant active; there's no code
    // path that will retry. Log loudly so users who care can clean up
    // via https://myaccount.google.com/permissions.
    if let Some(tokens) = prior {
        let token_to_revoke = tokens
            .refresh_token
            .as_deref()
            .unwrap_or(tokens.access_token.as_str())
            .to_string();
        tokio::spawn(async move {
            let http = match reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("[calendar] Revoke HTTP client build failed: {e}");
                    return;
                }
            };
            match http
                .post(REVOKE_URL)
                .form(&[("token", token_to_revoke.as_str())])
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    log::info!("[calendar] Google revoke succeeded");
                }
                Ok(resp) => {
                    log::warn!(
                        "[calendar] Google revoke returned {}; remote grant may still be active",
                        resp.status()
                    );
                }
                Err(e) => {
                    log::warn!(
                        "[calendar] Google revoke request failed: {e}; remote grant may still be active"
                    );
                }
            }
        });
    }

    Ok(())
}

#[tauri::command]
pub async fn api_calendar_list_upcoming() -> Result<Vec<CalendarEventDto>, String> {
    let store = KeyringTokenStore;
    let (access_token, _tokens) =
        api::get_fresh_access_token(&store, TokenKey::GOOGLE_CALENDAR_DEFAULT).await?;
    api::list_upcoming_events(&access_token).await
}

/// Freeze the chosen Google Calendar event into a JSON snapshot on the
/// meeting row. This is the *write* side of the calendar context — the
/// summary pipeline reads it via `calendar::repository::load_context`.
///
/// Errors:
/// - "not connected" — caller should prompt the user to reconnect.
/// - "meeting not found" — caller likely raced a delete; surface to UI.
/// - "calendar context too large" — already-rare; fall back to summary
///   without context.
/// - Bare network/Google errors propagate verbatim from `fetch_event_*`.
#[tauri::command]
pub async fn api_link_meeting_to_calendar_event(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    event_id: String,
) -> Result<(), String> {
    let store = KeyringTokenStore;
    let (access_token, _tokens) =
        api::get_fresh_access_token(&store, TokenKey::GOOGLE_CALENDAR_DEFAULT).await?;
    let ctx = api::fetch_event_as_frozen_context(&access_token, &event_id).await?;
    let pool = state.db_manager.pool();
    repository::persist_context(pool, &meeting_id, &ctx)
        .await
        .map_err(|e| e.to_string())?;
    log::info!(
        "[calendar] Linked meeting {meeting_id} to event {event_id} ({} attendees)",
        ctx.attendees.len()
    );
    Ok(())
}

/// Clear the frozen calendar context on a meeting row (the "Unlink"
/// affordance on the meeting detail page). Safe on rows that have
/// no linked event — no-op success.
#[tauri::command]
pub async fn api_unlink_meeting_calendar_context(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<(), String> {
    let pool = state.db_manager.pool();
    repository::clear_context(pool, &meeting_id)
        .await
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoMatchOutcome {
    /// The event the recording was linked to, when overlap exceeded
    /// `MATCH_THRESHOLD`. `None` covers every fall-through path —
    /// disconnected, no events nearby, all candidates below
    /// threshold, network blip — so the frontend renders a single
    /// "no match" state regardless of cause.
    pub matched: Option<MatchedEvent>,
    /// Set to true when the auto-match also rewrote the meeting
    /// title from the auto-generated `Meeting DD_MM_YY_HH_MM_SS`
    /// pattern to the calendar event's title.
    pub renamed_meeting: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchedEvent {
    pub event_id: String,
    pub title: String,
}

/// Score the user's upcoming events against `[start_iso, end_iso]`
/// and, if the highest-overlap candidate exceeds the threshold,
/// freeze its context onto the meeting row. Also rewrites the meeting
/// title to the event title when the existing title is the
/// auto-generated `Meeting DD_MM_YY_HH_MM_SS` shape — preserves any
/// title the user (or the summary's title-extraction) already chose.
///
/// Best-effort by design: every fall-through (disconnected, network
/// error, no candidates, no overlap above threshold, freeze size cap
/// hit) returns `Ok(AutoMatchOutcome { matched: None, renamed_meeting: false })`
/// rather than an `Err`. The recording itself already succeeded;
/// calendar enrichment is decoration.
#[tauri::command]
pub async fn api_calendar_auto_match_and_link(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    start_iso: String,
    end_iso: String,
) -> Result<AutoMatchOutcome, String> {
    let store = KeyringTokenStore;
    if matches!(store.load(TokenKey::GOOGLE_CALENDAR_DEFAULT).await, Ok(None) | Err(_)) {
        // Disconnected or keyring unavailable — silent no-match.
        return Ok(AutoMatchOutcome { matched: None, renamed_meeting: false });
    }

    let access_token = match api::get_fresh_access_token(&store, TokenKey::GOOGLE_CALENDAR_DEFAULT)
        .await
    {
        Ok((t, _)) => t,
        Err(e) => {
            log::warn!("[calendar] auto-match: token refresh failed ({e}); skipping");
            return Ok(AutoMatchOutcome { matched: None, renamed_meeting: false });
        }
    };

    let events = match api::list_upcoming_events(&access_token).await {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[calendar] auto-match: list_upcoming failed ({e}); skipping");
            return Ok(AutoMatchOutcome { matched: None, renamed_meeting: false });
        }
    };

    let rec_start = match chrono::DateTime::parse_from_rfc3339(&start_iso) {
        Ok(dt) => dt.with_timezone(&chrono::Utc),
        Err(_) => return Err(format!("Invalid start_iso: {start_iso}")),
    };
    let rec_end = match chrono::DateTime::parse_from_rfc3339(&end_iso) {
        Ok(dt) => dt.with_timezone(&chrono::Utc),
        Err(_) => return Err(format!("Invalid end_iso: {end_iso}")),
    };

    let pairs: Vec<(String, String)> = events
        .iter()
        .map(|e| (e.start.clone(), e.end.clone()))
        .collect();
    let result = matching::score_candidates(rec_start, rec_end, &pairs);

    let Some(idx) = result.winner_index else {
        log::info!(
            "[calendar] auto-match: no event overlapped {}% of recording",
            (matching::MATCH_THRESHOLD * 100.0) as u32
        );
        return Ok(AutoMatchOutcome { matched: None, renamed_meeting: false });
    };
    let chosen = &events[idx];
    log::info!(
        "[calendar] auto-match: linking meeting {meeting_id} to '{}' ({}s overlap)",
        chosen.title,
        result.overlap_seconds
    );

    let ctx = match api::fetch_event_as_frozen_context(&access_token, &chosen.id).await {
        Ok(c) => c,
        Err(e) => {
            log::warn!("[calendar] auto-match: freeze fetch failed ({e}); skipping");
            return Ok(AutoMatchOutcome { matched: None, renamed_meeting: false });
        }
    };

    let pool = state.db_manager.pool();
    if let Err(e) = repository::persist_context(pool, &meeting_id, &ctx).await {
        log::warn!("[calendar] auto-match: persist failed ({e}); skipping");
        return Ok(AutoMatchOutcome { matched: None, renamed_meeting: false });
    }

    // Title replacement: only when the meeting still carries the
    // auto-generated `Meeting DD_MM_YY_HH_MM_SS` shape. Reads current
    // title via get_meeting_metadata so we don't fight a concurrent
    // rename from `extract_meeting_name_from_markdown`.
    let mut renamed = false;
    if let Ok(Some(model)) = MeetingsRepository::get_meeting_metadata(pool, &meeting_id).await {
        if matching::is_auto_generated_title(&model.title) {
            match MeetingsRepository::update_meeting_title(pool, &meeting_id, &chosen.title).await {
                Ok(true) => renamed = true,
                Ok(false) => {}
                Err(e) => log::warn!("[calendar] auto-match: title rewrite failed ({e})"),
            }
        }
    }

    Ok(AutoMatchOutcome {
        matched: Some(MatchedEvent {
            event_id: chosen.id.clone(),
            title: chosen.title.clone(),
        }),
        renamed_meeting: renamed,
    })
}
