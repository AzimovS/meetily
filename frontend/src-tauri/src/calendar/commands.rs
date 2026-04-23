use crate::calendar::oauth;
use crate::calendar::types::ConnectionStatus;

#[tauri::command]
pub async fn api_calendar_status() -> Result<ConnectionStatus, String> {
    // Persistence not yet implemented — always reports disconnected on launch.
    Ok(ConnectionStatus::Disconnected)
}

#[tauri::command]
pub async fn api_calendar_connect() -> Result<ConnectionStatus, String> {
    let tokens = oauth::connect().await?;

    log::info!(
        "[calendar] OAuth connect succeeded: access_token len={}, refresh_token present={}, expires_in={:?}",
        tokens.access_token.len(),
        tokens.refresh_token.is_some(),
        tokens.expires_in_secs
    );

    // TODO: persist tokens to keyring, fetch user email via Calendar API.
    // For now we report a placeholder so the UI flips to Connected and
    // we can verify the round-trip end-to-end.
    Ok(ConnectionStatus::Connected {
        email: "connected (email fetch not yet implemented)".to_string(),
    })
}

#[tauri::command]
pub async fn api_calendar_disconnect() -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub async fn api_calendar_list_upcoming() -> Result<Vec<serde_json::Value>, String> {
    Ok(Vec::new())
}
