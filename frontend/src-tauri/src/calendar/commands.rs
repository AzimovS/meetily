use std::time::{SystemTime, UNIX_EPOCH};

use crate::calendar::api;
use crate::calendar::oauth;
use crate::calendar::token_store::{KeyringTokenStore, StoredTokens, TokenKey, TokenStore};
use crate::calendar::types::{CalendarEventDto, ConnectionStatus};

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
