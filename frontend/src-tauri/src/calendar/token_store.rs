//! OAuth token persistence as JSON files in the per-user app data
//! dir, one file per (provider, account) pair so future multi-account
//! integrations slot in cleanly.
//!
//! Path: `<app_data_dir>/<provider>.<account>.json`. The dir comes
//! from Tauri's `app.path().app_data_dir()` and is stashed in
//! `APP_DATA_DIR` during `setup()`. Reading before init returns Err.
//!
//! Writes are atomic via tempfile + rename. On Unix the file mode is
//! set to `0600` after write.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::fs;

static APP_DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Idempotent. Call once from Tauri `setup()`.
pub fn init_app_data_dir(dir: PathBuf) {
    let _ = APP_DATA_DIR.set(dir);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds. `None` when the provider omitted `expires_in`.
    pub expires_at: Option<u64>,
    /// Backfilled after the first Calendar API call.
    pub email: Option<String>,
}

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

pub struct FileTokenStore;

impl FileTokenStore {
    fn path_for(key: TokenKey) -> Result<PathBuf, String> {
        let dir = APP_DATA_DIR
            .get()
            .ok_or_else(|| "Token store not initialized".to_string())?;
        Ok(dir.join(format!("{}.{}.json", key.provider, key.account)))
    }
}

#[async_trait]
impl TokenStore for FileTokenStore {
    async fn save(&self, key: TokenKey, tokens: &StoredTokens) -> Result<(), String> {
        let path = FileTokenStore::path_for(key)?;
        let payload = serde_json::to_string(tokens)
            .map_err(|e| format!("Failed to serialize tokens: {e}"))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create token dir: {e}"))?;
        }
        // Atomic: write tmp, chmod, rename. A crash mid-write never
        // leaves partial JSON in `path`.
        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, &payload)
            .await
            .map_err(|e| format!("Failed to write token file: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&tmp_path)
                .map_err(|e| format!("Failed to stat token file: {e}"))?
                .permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&tmp_path, perms)
                .map_err(|e| format!("Failed to chmod token file: {e}"))?;
        }
        fs::rename(&tmp_path, &path)
            .await
            .map_err(|e| format!("Failed to commit token file: {e}"))?;
        Ok(())
    }

    async fn load(&self, key: TokenKey) -> Result<Option<StoredTokens>, String> {
        let path = FileTokenStore::path_for(key)?;
        match fs::read_to_string(&path).await {
            Ok(raw) => serde_json::from_str::<StoredTokens>(&raw)
                .map(Some)
                .map_err(|e| format!("Failed to deserialize tokens: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("Failed to read token file: {e}")),
        }
    }

    async fn delete(&self, key: TokenKey) -> Result<(), String> {
        let path = FileTokenStore::path_for(key)?;
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            // NotFound counts as success — same post-condition.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("Failed to delete token file: {e}")),
        }
    }
}

/// In-memory token store for tests.
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
