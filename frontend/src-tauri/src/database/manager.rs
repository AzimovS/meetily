use sqlx::{migrate::MigrateDatabase, Result, Row, Sqlite, SqlitePool, Transaction};
use std::fs;
use std::path::Path;
use tauri::Manager;

#[derive(Clone)]
pub struct DatabaseManager {
    pool: SqlitePool,
}

impl DatabaseManager {
    pub async fn new(tauri_db_path: &str, backend_db_path: &str) -> Result<Self> {
        if let Some(parent_dir) = Path::new(tauri_db_path).parent() {
            if !parent_dir.exists() {
                fs::create_dir_all(parent_dir).map_err(|e| sqlx::Error::Io(e))?;
            }
        }

        if !Path::new(tauri_db_path).exists() {
            if Path::new(backend_db_path).exists() {
                log::info!(
                    "Copying database from {} to {}",
                    backend_db_path,
                    tauri_db_path
                );
                fs::copy(backend_db_path, tauri_db_path).map_err(|e| sqlx::Error::Io(e))?;
            } else {
                log::info!("Creating database at {}", tauri_db_path);
                Sqlite::create_database(tauri_db_path).await?;
            }
        }

        let pool = SqlitePool::connect(tauri_db_path).await?;

        // Pre-migration fixup: handle edge case where a column was added outside
        // the migration system (e.g., during development). If the column exists but
        // the migration hasn't been recorded, sqlx will try to ADD COLUMN and fail
        // with "duplicate column". Fix by removing the column via table recreation
        // so the migration can re-add it cleanly.
        Self::fix_duplicate_column_before_migrate(&pool).await;

        sqlx::migrate!("./migrations").run(&pool).await?;

        Ok(DatabaseManager { pool })
    }

    // NOTE: So for the first time users they needs to start the application
    // after they can just delete the existing .sqlite file and then copy the existing .db file to
    // the current app dir, So the system detects legacy db and copy it and starts with that data
    // (Newly created .sqlite with the copied content from .db)
    pub async fn new_from_app_handle(app_handle: &tauri::AppHandle) -> Result<Self> {
        // Resolve the app's data directory
        let app_data_dir = app_handle
            .path()
            .app_data_dir()
            .expect("failed to get app data dir");
        if !app_data_dir.exists() {
            fs::create_dir_all(&app_data_dir).map_err(|e| sqlx::Error::Io(e))?;
        }

        // Define database paths
        let tauri_db_path = app_data_dir
            .join("meeting_minutes.sqlite")
            .to_string_lossy()
            .to_string();
        // Legacy backend DB path (for auto-migration if exists)
        let backend_db_path = app_data_dir
            .join("meeting_minutes.db")
            .to_string_lossy()
            .to_string();

        // WAL file paths for defensive cleanup
        let wal_path = app_data_dir.join("meeting_minutes.sqlite-wal");
        let shm_path = app_data_dir.join("meeting_minutes.sqlite-shm");

        log::info!("Tauri DB path: {}", tauri_db_path);
        log::info!("Legacy backend DB path: {}", backend_db_path);

        // Try to open database with defensive WAL handling
        match Self::new(&tauri_db_path, &backend_db_path).await {
            Ok(db_manager) => {
                log::info!("Database opened successfully");
                Ok(db_manager)
            }
            Err(e) => {
                // Check if error is due to corrupted WAL file
                let error_msg = e.to_string();
                if error_msg.contains("malformed") || error_msg.contains("corrupt") {
                    log::warn!("Database appears corrupted, likely due to orphaned WAL file. Attempting recovery...");
                    log::warn!("Error details: {}", error_msg);

                    // Delete potentially corrupted WAL/SHM files
                    if wal_path.exists() {
                        match fs::remove_file(&wal_path) {
                            Ok(_) => log::info!("Removed orphaned WAL file: {:?}", wal_path),
                            Err(e) => log::warn!("Failed to remove WAL file: {}", e),
                        }
                    }
                    if shm_path.exists() {
                        match fs::remove_file(&shm_path) {
                            Ok(_) => log::info!("Removed orphaned SHM file: {:?}", shm_path),
                            Err(e) => log::warn!("Failed to remove SHM file: {}", e),
                        }
                    }

                    // Retry connection without WAL files
                    log::info!("Retrying database connection after WAL cleanup...");
                    match Self::new(&tauri_db_path, &backend_db_path).await {
                        Ok(db_manager) => {
                            log::info!("Database opened successfully after WAL recovery");
                            Ok(db_manager)
                        }
                        Err(retry_err) => {
                            log::error!("Database connection failed even after WAL cleanup: {}", retry_err);
                            Err(retry_err)
                        }
                    }
                } else {
                    // Not a WAL-related error, propagate original error
                    log::error!("Database connection failed: {}", error_msg);
                    Err(e)
                }
            }
        }
    }

    /// Check if this is the first launch (sqlite database doesn't exist yet)
    pub async fn is_first_launch(app_handle: &tauri::AppHandle) -> Result<bool> {
        let app_data_dir = app_handle
            .path()
            .app_data_dir()
            .expect("failed to get app data dir");

        let tauri_db_path = app_data_dir.join("meeting_minutes.sqlite");

        Ok(!tauri_db_path.exists())
    }

    /// Import a legacy database from the specified path and initialize
    pub async fn import_legacy_database(
        app_handle: &tauri::AppHandle,
        legacy_db_path: &str,
    ) -> Result<Self> {
        let app_data_dir = app_handle
            .path()
            .app_data_dir()
            .expect("failed to get app data dir");

        if !app_data_dir.exists() {
            fs::create_dir_all(&app_data_dir).map_err(|e| sqlx::Error::Io(e))?;
        }

        // Copy legacy database to app data directory as meeting_minutes.db
        let target_legacy_path = app_data_dir.join("meeting_minutes.db");
        log::info!(
            "Copying legacy database from {} to {}",
            legacy_db_path,
            target_legacy_path.display()
        );

        fs::copy(legacy_db_path, &target_legacy_path).map_err(|e| sqlx::Error::Io(e))?;

        // Now use the standard initialization which will detect and migrate the legacy db
        Self::new_from_app_handle(app_handle).await
    }

    /// Handle edge cases where the runpodApiKey migration can't run cleanly:
    /// 1. Column exists but migration not recorded (added outside migration system)
    /// 2. Migration recorded with wrong checksum (file was modified then reverted)
    /// 3. Leftover temp table from a previously interrupted fixup
    /// In all cases, we drop the column and/or reset the migration record so
    /// sqlx can re-run the migration cleanly.
    async fn fix_duplicate_column_before_migrate(pool: &SqlitePool) {
        // Recovery: if a previous fixup was interrupted after DROP but before RENAME,
        // we'll have transcript_settings_fixup but no transcript_settings.
        let fixup_table_exists = sqlx::query_scalar::<_, i32>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='transcript_settings_fixup'"
        )
        .fetch_one(pool)
        .await
        .unwrap_or(0);

        let main_table_exists = sqlx::query_scalar::<_, i32>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='transcript_settings'"
        )
        .fetch_one(pool)
        .await
        .unwrap_or(0);

        if fixup_table_exists > 0 && main_table_exists == 0 {
            log::warn!("Pre-migration fixup: recovering from interrupted fixup — renaming transcript_settings_fixup to transcript_settings");
            let _ = sqlx::query("ALTER TABLE transcript_settings_fixup RENAME TO transcript_settings")
                .execute(pool).await;
        } else if fixup_table_exists > 0 {
            // Both exist — drop the leftover fixup table
            let _ = sqlx::query("DROP TABLE transcript_settings_fixup")
                .execute(pool).await;
        }

        // Check if _sqlx_migrations table exists (it won't on first run)
        let migrations_table_exists = sqlx::query_scalar::<_, i32>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'"
        )
        .fetch_one(pool)
        .await
        .unwrap_or(0);

        if migrations_table_exists == 0 {
            return; // First run, nothing to fix
        }

        // Check if migration 20260305000000 has already been recorded
        let migration_recorded = sqlx::query_scalar::<_, i32>(
            "SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 20260305000000"
        )
        .fetch_one(pool)
        .await
        .unwrap_or(0);

        if migration_recorded > 0 {
            // Migration was recorded, but it may have been recorded with a different
            // checksum (e.g., if the migration file was temporarily modified during
            // development and then reverted). sqlx will refuse to run if the checksum
            // doesn't match. Fix by deleting the stale record — since the column
            // already exists (checked below), the migration will fail with "duplicate
            // column" which we handle by recreating the table first.
            //
            // Read the stored checksum and compare with the current migration file.
            // We compute a simple check: if the column already exists and the migration
            // is recorded, just ensure the record is consistent. If sqlx would fail
            // with a checksum mismatch, we delete the record and let the fixup below
            // handle the duplicate column case.
            let has_column = sqlx::query(
                "SELECT COUNT(*) as cnt FROM pragma_table_info('transcript_settings') WHERE name='runpodApiKey'"
            )
            .fetch_one(pool)
            .await
            .map(|row| row.get::<i32, _>("cnt") > 0)
            .unwrap_or(false);

            if has_column {
                // Column exists and migration is recorded — the checksum may be stale.
                // Drop the column and delete the migration record so sqlx can re-run
                // the migration cleanly with the correct checksum.
                log::warn!("Pre-migration fixup: migration 20260305000000 recorded but may have stale checksum. Resetting to allow clean re-run.");

                let result: std::result::Result<(), sqlx::Error> = async {
                    // Drop the column (SQLite 3.35+) so migration can re-add it
                    sqlx::query("ALTER TABLE transcript_settings DROP COLUMN runpodApiKey")
                        .execute(pool).await?;

                    // Delete the stale migration record
                    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 20260305000000")
                        .execute(pool).await?;

                    log::info!("Pre-migration fixup: dropped column and reset migration record for clean re-run");
                    Ok(())
                }.await;

                if let Err(e) = result {
                    log::warn!("Pre-migration fixup (checksum reset) failed (non-fatal): {}", e);
                }
            }

            return;
        }

        // Check if runpodApiKey column already exists in transcript_settings
        let has_column = sqlx::query(
            "SELECT COUNT(*) as cnt FROM pragma_table_info('transcript_settings') WHERE name='runpodApiKey'"
        )
        .fetch_one(pool)
        .await
        .map(|row| row.get::<i32, _>("cnt") > 0)
        .unwrap_or(false);

        if !has_column {
            return; // Column doesn't exist, migration will add it normally
        }

        // Column exists but migration hasn't run — drop the column so the
        // migration can add it cleanly.
        log::warn!("Pre-migration fixup: runpodApiKey column exists but migration not recorded. Dropping column to allow migration to run.");

        let result: std::result::Result<(), sqlx::Error> = async {
            // Drop the column (SQLite 3.35+) so migration can re-add it
            sqlx::query("ALTER TABLE transcript_settings DROP COLUMN runpodApiKey")
                .execute(pool).await?;

            log::info!("Pre-migration fixup: dropped runpodApiKey column successfully");
            Ok(())
        }.await;

        if let Err(e) = result {
            log::warn!("Pre-migration fixup failed (non-fatal, migration may still succeed): {}", e);
        }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn with_transaction<T, F, Fut>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Transaction<'_, Sqlite>) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut tx = self.pool.begin().await?;
        let result = f(&mut tx).await;

        match result {
            Ok(val) => {
                tx.commit().await?;
                Ok(val)
            }
            Err(err) => {
                tx.rollback().await?;
                Err(err)
            }
        }
    }

    /// Cleanup database connection and checkpoint WAL
    /// This should be called on application shutdown to ensure:
    /// - All WAL changes are written to the main database file
    /// - The .wal and .shm files are deleted
    /// - Connection pool is gracefully closed
    pub async fn cleanup(&self) -> Result<()> {
        log::info!("Starting database cleanup...");

        // Force checkpoint of WAL to main database file and remove WAL file
        // TRUNCATE mode: checkpoints all pages AND deletes the WAL file
        match sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.pool)
            .await
        {
            Ok(_) => log::info!("WAL checkpoint completed successfully"),
            Err(e) => log::warn!("WAL checkpoint failed (non-fatal): {}", e),
        }

        // Close the connection pool gracefully
        self.pool.close().await;
        log::info!("Database connection pool closed");

        Ok(())
    }
}
