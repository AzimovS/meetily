use std::time::{SystemTime, UNIX_EPOCH};

use crate::calendar::oauth;
use crate::calendar::token_store::{KeyringTokenStore, StoredTokens, TokenKey, TokenStore};
use crate::calendar::types::ConnectionStatus;

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
    // Clear any prior tokens before starting a fresh consent — prevents
    // stale state if the user re-connects after an error.
    let store = KeyringTokenStore;
    store.delete(TokenKey::GOOGLE_CALENDAR_DEFAULT).await?;

    let tokens = oauth::connect().await?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stored = StoredTokens {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at: tokens.expires_in_secs.map(|s| now + s),
        email: None, // Fetched on first Calendar API call in the next commit.
    };

    store
        .save(TokenKey::GOOGLE_CALENDAR_DEFAULT, &stored)
        .await?;

    log::info!(
        "[calendar] Connected; tokens persisted (refresh_token present={})",
        stored.refresh_token.is_some()
    );

    Ok(ConnectionStatus::Connected {
        email: "Connected".to_string(),
    })
}

#[tauri::command]
pub async fn api_calendar_disconnect() -> Result<(), String> {
    let store = KeyringTokenStore;

    // Best-effort revoke against Google. We delete local tokens
    // unconditionally even if revoke fails — a stale server-side grant
    // is less dangerous than leaking tokens on a shared machine.
    if let Some(tokens) = store.load(TokenKey::GOOGLE_CALENDAR_DEFAULT).await? {
        // Prefer the refresh_token (revokes the whole grant). Fall back
        // to access_token if no refresh is stored.
        let token_to_revoke = tokens
            .refresh_token
            .as_deref()
            .unwrap_or(tokens.access_token.as_str());

        let http = match reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[calendar] Revoke HTTP client build failed: {e}");
                store.delete(TokenKey::GOOGLE_CALENDAR_DEFAULT).await?;
                return Ok(());
            }
        };

        match http
            .post(REVOKE_URL)
            .form(&[("token", token_to_revoke)])
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                log::info!("[calendar] Google revoke succeeded");
            }
            Ok(resp) => {
                log::warn!(
                    "[calendar] Google revoke returned {}; deleting local tokens anyway",
                    resp.status()
                );
            }
            Err(e) => {
                log::warn!("[calendar] Google revoke request failed: {e}; deleting local tokens anyway");
            }
        }
    }

    store.delete(TokenKey::GOOGLE_CALENDAR_DEFAULT).await?;
    log::info!("[calendar] Local tokens deleted");
    Ok(())
}

#[tauri::command]
pub async fn api_calendar_list_upcoming() -> Result<Vec<serde_json::Value>, String> {
    // Lands in the next commit alongside the refresh-token helper.
    Ok(Vec::new())
}
