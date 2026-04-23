//! Persistent storage for OAuth tokens.
//!
//! Tokens live in the OS keychain via the `keyring` crate (Keychain on
//! macOS, Credential Manager on Windows, Secret Service on GNOME/KDE).
//! Never in SQLite, never in a config file.
//!
//! The `keyring` crate is synchronous and can block for seconds on user
//! prompts (macOS "Meetily wants to access the keychain"), so all calls
//! are wrapped in `tokio::task::spawn_blocking` to avoid stalling the
//! Tokio runtime.

use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::task::spawn_blocking;

/// OAuth tokens and associated account metadata persisted together.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix timestamp (seconds) when the access token expires. `None`
    /// when the provider did not supply `expires_in`.
    pub expires_at: Option<u64>,
    /// The account email we associate these tokens with. Populated
    /// after the first successful call to the Calendar API. May be
    /// `None` immediately after connect — in that case the UI shows
    /// "Connected" without an email until we backfill.
    pub email: Option<String>,
}

/// Identifies a (provider, account) slot in the keychain. Future
/// multi-account support extends the `account` field beyond "default".
#[derive(Debug, Clone, Copy)]
pub struct TokenKey {
    pub provider: &'static str,
    pub account: &'static str,
}

impl TokenKey {
    pub const GOOGLE_CALENDAR_DEFAULT: Self = Self {
        provider: "meetily.calendar.google",
        account: "default",
    };
}

#[async_trait]
pub trait TokenStore: Send + Sync {
    async fn save(&self, key: TokenKey, tokens: &StoredTokens) -> Result<(), String>;
    async fn load(&self, key: TokenKey) -> Result<Option<StoredTokens>, String>;
    async fn delete(&self, key: TokenKey) -> Result<(), String>;
}

/// Production token store backed by the OS keychain.
pub struct KeyringTokenStore;

#[async_trait]
impl TokenStore for KeyringTokenStore {
    async fn save(&self, key: TokenKey, tokens: &StoredTokens) -> Result<(), String> {
        let payload = serde_json::to_string(tokens)
            .map_err(|e| format!("Failed to serialize tokens: {e}"))?;
        spawn_blocking(move || {
            let entry = keyring::Entry::new(key.provider, key.account)
                .map_err(|e| format!("Keyring entry error: {e}"))?;
            entry
                .set_password(&payload)
                .map_err(|e| format!("Keyring save error: {e}"))
        })
        .await
        .map_err(|e| format!("spawn_blocking failure: {e}"))?
    }

    async fn load(&self, key: TokenKey) -> Result<Option<StoredTokens>, String> {
        spawn_blocking(move || {
            let entry = keyring::Entry::new(key.provider, key.account)
                .map_err(|e| format!("Keyring entry error: {e}"))?;
            match entry.get_password() {
                Ok(raw) => serde_json::from_str::<StoredTokens>(&raw)
                    .map(Some)
                    .map_err(|e| format!("Failed to deserialize tokens: {e}")),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(format!("Keyring load error: {e}")),
            }
        })
        .await
        .map_err(|e| format!("spawn_blocking failure: {e}"))?
    }

    async fn delete(&self, key: TokenKey) -> Result<(), String> {
        spawn_blocking(move || {
            let entry = keyring::Entry::new(key.provider, key.account)
                .map_err(|e| format!("Keyring entry error: {e}"))?;
            match entry.delete_credential() {
                Ok(()) => Ok(()),
                // Treat "no entry" as success — the post-condition is the
                // same whether we actively deleted or it was already gone.
                Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(format!("Keyring delete error: {e}")),
            }
        })
        .await
        .map_err(|e| format!("spawn_blocking failure: {e}"))?
    }
}

/// In-memory token store for tests. Never touches the OS.
#[allow(dead_code)]
pub struct InMemoryTokenStore {
    inner: Mutex<std::collections::HashMap<(&'static str, &'static str), StoredTokens>>,
}

#[allow(dead_code)]
impl InMemoryTokenStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(std::collections::HashMap::new()),
        }
    }
}

#[async_trait]
impl TokenStore for InMemoryTokenStore {
    async fn save(&self, key: TokenKey, tokens: &StoredTokens) -> Result<(), String> {
        self.inner
            .lock()
            .unwrap()
            .insert((key.provider, key.account), tokens.clone());
        Ok(())
    }

    async fn load(&self, key: TokenKey) -> Result<Option<StoredTokens>, String> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .get(&(key.provider, key.account))
            .cloned())
    }

    async fn delete(&self, key: TokenKey) -> Result<(), String> {
        self.inner
            .lock()
            .unwrap()
            .remove(&(key.provider, key.account));
        Ok(())
    }
}
