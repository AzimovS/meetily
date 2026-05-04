//! SQLite read/write of the frozen calendar-context blob.
//!
//! The blob is stored opaquely in `meetings.calendar_context_json` and
//! is always serialized/deserialized as a `FrozenCalendarContext`
//! (versioned with `schema_version`). The summary pipeline is the only
//! reader.

use sqlx::SqlitePool;

use crate::calendar::types::FrozenCalendarContext;

/// Hard cap on the persisted JSON payload. Above this we reject the
/// write rather than letting a malformed Google response (e.g. a
/// 100-attendee invite with full description on every member) bloat the
/// row to the point where summary prompts grow unboundedly. Slice 4
/// applies its own per-field caps inside the prompt block; this guard
/// is the outer envelope.
pub const MAX_CONTEXT_JSON_BYTES: usize = 32 * 1024;

#[derive(Debug)]
pub enum RepositoryError {
    ContextTooLarge { bytes: usize },
    Serialize(serde_json::Error),
    Sql(sqlx::Error),
    NotFound,
}

impl std::fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ContextTooLarge { bytes } => write!(
                f,
                "calendar context too large: {bytes} bytes (max {MAX_CONTEXT_JSON_BYTES})"
            ),
            Self::Serialize(e) => write!(f, "calendar context serialize failed: {e}"),
            Self::Sql(e) => write!(f, "calendar context db error: {e}"),
            Self::NotFound => write!(f, "meeting not found"),
        }
    }
}

impl std::error::Error for RepositoryError {}

impl From<sqlx::Error> for RepositoryError {
    fn from(value: sqlx::Error) -> Self {
        Self::Sql(value)
    }
}

impl From<serde_json::Error> for RepositoryError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serialize(value)
    }
}

/// Persist a frozen context against an existing meeting row. Returns
/// `Err(NotFound)` if the meeting does not exist (caller decides
/// whether that's user-facing). `Err(ContextTooLarge)` when the
/// serialized JSON exceeds `MAX_CONTEXT_JSON_BYTES` — caller should log
/// and fall back to the no-context summary path.
pub async fn persist_context(
    pool: &SqlitePool,
    meeting_id: &str,
    ctx: &FrozenCalendarContext,
) -> Result<(), RepositoryError> {
    let json = serde_json::to_string(ctx)?;
    if json.len() > MAX_CONTEXT_JSON_BYTES {
        return Err(RepositoryError::ContextTooLarge { bytes: json.len() });
    }
    let result = sqlx::query("UPDATE meetings SET calendar_context_json = ? WHERE id = ?")
        .bind(&json)
        .bind(meeting_id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(RepositoryError::NotFound);
    }
    Ok(())
}

/// Load and deserialize the frozen context for a meeting. Returns
/// `Ok(None)` when the column is NULL (meeting exists but no event
/// linked) or when the meeting does not exist. Returns `Err(Serialize)`
/// when the column has data that cannot be parsed — caller decides
/// whether to clear the corrupt blob or surface the error.
pub async fn load_context(
    pool: &SqlitePool,
    meeting_id: &str,
) -> Result<Option<FrozenCalendarContext>, RepositoryError> {
    let row: Option<Option<String>> =
        sqlx::query_scalar("SELECT calendar_context_json FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(pool)
            .await?;
    let Some(Some(json)) = row else {
        return Ok(None);
    };
    let ctx: FrozenCalendarContext = serde_json::from_str(&json)?;
    Ok(Some(ctx))
}

/// Clear the snapshot for a meeting (the "Unlink" affordance on the
/// meeting detail page). Returns `Err(NotFound)` if the meeting row
/// does not exist; idempotent if it does — clearing an already-NULL
/// column is a no-op success.
pub async fn clear_context(pool: &SqlitePool, meeting_id: &str) -> Result<(), RepositoryError> {
    let result =
        sqlx::query("UPDATE meetings SET calendar_context_json = NULL WHERE id = ?")
            .bind(meeting_id)
            .execute(pool)
            .await?;
    if result.rows_affected() == 0 {
        return Err(RepositoryError::NotFound);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::types::{FrozenAttendee, FrozenPerson};
    use sqlx::sqlite::SqlitePoolOptions;

    /// Build an in-memory SQLite pool with just enough schema for the
    /// repository tests. We only need the columns the repository
    /// reads/writes, not the full migration chain.
    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE meetings (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                folder_path TEXT,
                custom_prompt TEXT,
                calendar_context_json TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO meetings (id, title, created_at, updated_at) VALUES (?, ?, ?, ?)",
        )
        .bind("m-1")
        .bind("Standup")
        .bind("2026-05-03T10:00:00Z")
        .bind("2026-05-03T10:00:00Z")
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn sample_ctx() -> FrozenCalendarContext {
        FrozenCalendarContext {
            schema_version: 1,
            source: "google".to_string(),
            event_id: "evt-1".to_string(),
            recurrence_id: None,
            title: "Sprint review".to_string(),
            description: Some("Discuss Q2 OKRs".to_string()),
            organizer: Some(FrozenPerson {
                display_name: Some("Alex".to_string()),
                email: Some("alex@example.com".to_string()),
            }),
            attendees: vec![FrozenAttendee {
                display_name: Some("Sam".to_string()),
                email: Some("sam@example.com".to_string()),
                response_status: Some("accepted".to_string()),
            }],
            start: "2026-05-03T11:00:00Z".to_string(),
            end: "2026-05-03T12:00:00Z".to_string(),
            captured_at: "2026-05-03T12:00:01Z".to_string(),
        }
    }

    #[tokio::test]
    async fn persist_and_load_round_trip() {
        let pool = test_pool().await;
        let ctx = sample_ctx();

        persist_context(&pool, "m-1", &ctx).await.unwrap();
        let loaded = load_context(&pool, "m-1").await.unwrap();

        assert_eq!(loaded, Some(ctx));
    }

    #[tokio::test]
    async fn load_returns_none_for_unwritten_row() {
        let pool = test_pool().await;
        let loaded = load_context(&pool, "m-1").await.unwrap();
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn load_returns_none_for_missing_meeting() {
        let pool = test_pool().await;
        let loaded = load_context(&pool, "no-such-meeting").await.unwrap();
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn persist_rejects_oversized_payload() {
        let pool = test_pool().await;
        let mut ctx = sample_ctx();
        // Pad description until total JSON > MAX_CONTEXT_JSON_BYTES.
        ctx.description = Some("x".repeat(MAX_CONTEXT_JSON_BYTES + 100));

        let err = persist_context(&pool, "m-1", &ctx).await.unwrap_err();
        match err {
            RepositoryError::ContextTooLarge { bytes } => {
                assert!(bytes > MAX_CONTEXT_JSON_BYTES);
            }
            other => panic!("expected ContextTooLarge, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn persist_returns_not_found_for_missing_meeting() {
        let pool = test_pool().await;
        let err = persist_context(&pool, "no-such-meeting", &sample_ctx())
            .await
            .unwrap_err();
        assert!(matches!(err, RepositoryError::NotFound));
    }

    #[tokio::test]
    async fn clear_removes_persisted_blob() {
        let pool = test_pool().await;
        persist_context(&pool, "m-1", &sample_ctx()).await.unwrap();
        clear_context(&pool, "m-1").await.unwrap();
        let loaded = load_context(&pool, "m-1").await.unwrap();
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn load_returns_serialize_error_on_corrupt_json() {
        let pool = test_pool().await;
        sqlx::query("UPDATE meetings SET calendar_context_json = ? WHERE id = ?")
            .bind("{not valid json")
            .bind("m-1")
            .execute(&pool)
            .await
            .unwrap();
        let err = load_context(&pool, "m-1").await.unwrap_err();
        assert!(matches!(err, RepositoryError::Serialize(_)));
    }
}
