use std::time::{Duration, Instant};
use std::{path::Path, str::FromStr};

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SongInfo {
    pub id: i64,
    pub music_id: i64,
    pub program_id: Option<i64>,
    pub song_name: String,
    pub song_artists: String,
    pub song_album: String,
    pub file_ext: String,
    pub music_size: i64,
    pub pic_size: i64,
    pub emb_pic_size: i64,
    pub bit_rate: i64,
    pub duration: i64,
    pub file_id: Option<String>,
    pub thumb_file_id: Option<String>,
    pub from_user_id: i64,
    pub from_user_name: String,
    pub from_chat_id: i64,
    pub from_chat_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

const PERF_DB_LOG_PREFIX: &str = "PERF_DB";

fn log_db_perf(op: &str, duration: Duration) {
    tracing::debug!(
        "{PERF_DB_LOG_PREFIX}|op={op}|elapsed_ms={}",
        duration.as_millis()
    );
}

impl Database {
    /// Create a new database connection with limited pool size
    ///
    /// # Errors
    /// Returns an error if the database connection or schema setup fails.
    pub async fn new(database_url: &str) -> Result<Self> {
        let is_sqlite_dsn = database_url.starts_with("sqlite:") || database_url == ":memory:";

        // Create database directory if it doesn't exist (file-path mode only)
        if !is_sqlite_dsn
            && let Some(parent) = Path::new(database_url).parent()
            && !parent.exists()
        {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Configure connection pool with WAL mode for better concurrency
        // WAL mode allows readers and writers to operate concurrently
        let mut options = if is_sqlite_dsn {
            SqliteConnectOptions::from_str(database_url)?
        } else {
            SqliteConnectOptions::new().filename(database_url)
        };

        options = options
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(30))
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .min_connections(1)
            .connect_with(options)
            .await?;

        // Create tables if they don't exist
        sqlx::query(
            r"
            CREATE TABLE IF NOT EXISTS song_infos (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                music_id INTEGER UNIQUE NOT NULL,
                program_id INTEGER,
                song_name TEXT NOT NULL,
                song_artists TEXT NOT NULL,
                song_album TEXT NOT NULL,
                file_ext TEXT NOT NULL,
                music_size INTEGER NOT NULL,
                pic_size INTEGER NOT NULL,
                emb_pic_size INTEGER NOT NULL,
                bit_rate INTEGER NOT NULL,
                duration INTEGER NOT NULL,
                file_id TEXT,
                thumb_file_id TEXT,
                from_user_id INTEGER NOT NULL,
                from_user_name TEXT NOT NULL,
                from_chat_id INTEGER NOT NULL,
                from_chat_name TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            ",
        )
        .execute(&pool)
        .await?;

        // Migration for existing databases created before podcast support.
        ensure_song_infos_has_program_id(&pool).await?;

        // Per-chat bot settings (currently: language override via /lang).
        sqlx::query(
            r"
            CREATE TABLE IF NOT EXISTS chat_settings (
                chat_id INTEGER PRIMARY KEY,
                language TEXT NOT NULL
            )
            ",
        )
        .execute(&pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_song_infos_from_user_id ON song_infos(from_user_id)",
        )
        .execute(&pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_song_infos_from_chat_id ON song_infos(from_chat_id)",
        )
        .execute(&pool)
        .await?;

        Ok(Self { pool })
    }

    /// Get song info by music ID
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub async fn get_song_by_music_id(&self, music_id: i64) -> Result<Option<SongInfo>> {
        let query_start = Instant::now();
        let row_result = sqlx::query(
            "SELECT id, music_id, song_name, song_artists, song_album, file_ext, \
             program_id, \
             music_size, pic_size, emb_pic_size, bit_rate, duration, file_id, \
             thumb_file_id, from_user_id, from_user_name, from_chat_id, \
             from_chat_name, created_at, updated_at \
             FROM song_infos WHERE music_id = ? LIMIT 1",
        )
        .bind(music_id)
        .fetch_optional(&self.pool)
        .await;
        log_db_perf("get_song_by_music_id", query_start.elapsed());
        let row = row_result?;

        Ok(row.as_ref().map(map_song_info_row))
    }

    /// Save or update song info
    ///
    /// # Errors
    /// Returns an error if the database insert or update fails.
    pub async fn save_song_info(&self, song_info: &SongInfo) -> Result<i64> {
        let query_start = Instant::now();
        let result = sqlx::query(
            r"
            INSERT INTO song_infos (
                music_id, song_name, song_artists, song_album, file_ext,
                program_id,
                music_size, pic_size, emb_pic_size, bit_rate, duration,
                file_id, thumb_file_id, from_user_id, from_user_name,
                from_chat_id, from_chat_name, created_at, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            ON CONFLICT(music_id) DO UPDATE SET
                program_id = excluded.program_id,
                song_name = excluded.song_name,
                song_artists = excluded.song_artists,
                song_album = excluded.song_album,
                file_ext = excluded.file_ext,
                music_size = excluded.music_size,
                pic_size = excluded.pic_size,
                emb_pic_size = excluded.emb_pic_size,
                bit_rate = excluded.bit_rate,
                duration = excluded.duration,
                file_id = excluded.file_id,
                thumb_file_id = excluded.thumb_file_id,
                updated_at = CURRENT_TIMESTAMP
            ",
        )
        .bind(song_info.music_id)
        .bind(&song_info.song_name)
        .bind(&song_info.song_artists)
        .bind(&song_info.song_album)
        .bind(&song_info.file_ext)
        .bind(song_info.program_id)
        .bind(song_info.music_size)
        .bind(song_info.pic_size)
        .bind(song_info.emb_pic_size)
        .bind(song_info.bit_rate)
        .bind(song_info.duration)
        .bind(&song_info.file_id)
        .bind(&song_info.thumb_file_id)
        .bind(song_info.from_user_id)
        .bind(&song_info.from_user_name)
        .bind(song_info.from_chat_id)
        .bind(&song_info.from_chat_name)
        .execute(&self.pool)
        .await;
        log_db_perf("save_song_info", query_start.elapsed());
        let result = result?;

        Ok(result.last_insert_rowid())
    }

    /// Get the persisted `/lang` override for a chat, if any.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub async fn get_chat_language(&self, chat_id: i64) -> Result<Option<String>> {
        let query_start = Instant::now();
        let row = sqlx::query("SELECT language FROM chat_settings WHERE chat_id = ?")
            .bind(chat_id)
            .fetch_optional(&self.pool)
            .await;
        log_db_perf("get_chat_language", query_start.elapsed());
        Ok(row?.map(|row| row.get("language")))
    }

    /// Persist (or replace) the `/lang` override for a chat.
    ///
    /// # Errors
    /// Returns an error if the database write fails.
    pub async fn set_chat_language(&self, chat_id: i64, language: &str) -> Result<()> {
        let query_start = Instant::now();
        let result = sqlx::query(
            "INSERT INTO chat_settings (chat_id, language) VALUES (?, ?) \
             ON CONFLICT(chat_id) DO UPDATE SET language = excluded.language",
        )
        .bind(chat_id)
        .bind(language)
        .execute(&self.pool)
        .await;
        log_db_perf("set_chat_language", query_start.elapsed());
        result.map(|_| ()).map_err(Into::into)
    }

    /// Remove the `/lang` override for a chat ("Auto" restores detection).
    ///
    /// # Errors
    /// Returns an error if the database write fails.
    pub async fn clear_chat_language(&self, chat_id: i64) -> Result<()> {
        let query_start = Instant::now();
        let result = sqlx::query("DELETE FROM chat_settings WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&self.pool)
            .await;
        log_db_perf("clear_chat_language", query_start.elapsed());
        result.map(|_| ()).map_err(Into::into)
    }

    /// Count total songs
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub async fn count_total_songs(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) as count FROM song_infos")
            .fetch_one(&self.pool)
            .await?;

        Ok(row.get("count"))
    }

    /// Count status metrics in one query: total songs, songs from user, songs from chat
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub async fn count_status_stats(&self, user_id: i64, chat_id: i64) -> Result<(i64, i64, i64)> {
        let row = sqlx::query(status_stats_query_sql())
            .bind(user_id)
            .bind(chat_id)
            .fetch_one(&self.pool)
            .await?;

        Ok((
            row.get("total_count"),
            row.get("user_count"),
            row.get("chat_count"),
        ))
    }

    /// Delete song by music ID
    ///
    /// # Errors
    /// Returns an error if the database delete fails.
    pub async fn delete_song_by_music_id(&self, music_id: i64) -> Result<bool> {
        let query_start = Instant::now();
        let result = sqlx::query("DELETE FROM song_infos WHERE music_id = ?")
            .bind(music_id)
            .execute(&self.pool)
            .await;
        log_db_perf("delete_song_by_music_id", query_start.elapsed());
        let result = result?;

        Ok(result.rows_affected() > 0)
    }

    /// Delete all songs from cache (admin only)
    ///
    /// # Errors
    /// Returns an error if the database delete fails.
    pub async fn clear_all_songs(&self) -> Result<u64> {
        let result = sqlx::query("DELETE FROM song_infos")
            .execute(&self.pool)
            .await?;

        if let Err(err) = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.pool)
            .await
        {
            tracing::warn!("WAL checkpoint after clear_all_songs failed: {err}");
        }

        Ok(result.rows_affected())
    }

    /// Optimize database by running VACUUM to reclaim space and defragment
    /// Should be called periodically after many deletions
    ///
    /// # Errors
    /// Returns an error if the VACUUM operation fails.
    pub async fn optimize(&self) -> Result<()> {
        sqlx::query("VACUUM").execute(&self.pool).await?;
        tracing::info!("Database VACUUM completed successfully");
        Ok(())
    }

    /// Run lightweight `SQLite` planner maintenance
    ///
    /// # Errors
    /// Returns an error if the PRAGMA optimize operation fails.
    pub async fn optimize_planner(&self) -> Result<()> {
        sqlx::query("PRAGMA optimize").execute(&self.pool).await?;
        tracing::debug!("Database PRAGMA optimize completed");
        Ok(())
    }
}

fn status_stats_query_sql() -> &'static str {
    r"
    SELECT
        (SELECT COUNT(*) FROM song_infos) AS total_count,
        (SELECT COUNT(*) FROM song_infos WHERE from_user_id = ?) AS user_count,
        (SELECT COUNT(*) FROM song_infos WHERE from_chat_id = ?) AS chat_count
    "
}

#[must_use]
fn parse_sqlite_timestamp(value: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f"))
        .ok()
        .map(|naive| naive.and_utc())
}

fn decode_sqlite_datetime(row: &SqliteRow, column: &'static str) -> DateTime<Utc> {
    if let Ok(dt) = row.try_get::<DateTime<Utc>, _>(column) {
        return dt;
    }

    match row.try_get::<String, _>(column).ok() {
        Some(value) => {
            if let Some(dt) = parse_sqlite_timestamp(&value) {
                dt
            } else {
                tracing::warn!(
                    column,
                    value = crate::utils::sanitize_sensitive_text(&value),
                    "Failed to parse sqlite timestamp; falling back to Utc::now()"
                );
                Utc::now()
            }
        }
        None => Utc::now(),
    }
}

fn map_song_info_row(row: &SqliteRow) -> SongInfo {
    SongInfo {
        id: row.get("id"),
        music_id: row.get("music_id"),
        program_id: row.get("program_id"),
        song_name: row.get("song_name"),
        song_artists: row.get("song_artists"),
        song_album: row.get("song_album"),
        file_ext: row.get("file_ext"),
        music_size: row.get("music_size"),
        pic_size: row.get("pic_size"),
        emb_pic_size: row.get("emb_pic_size"),
        bit_rate: row.get("bit_rate"),
        duration: row.get("duration"),
        file_id: row.get("file_id"),
        thumb_file_id: row.get("thumb_file_id"),
        from_user_id: row.get("from_user_id"),
        from_user_name: row.get("from_user_name"),
        from_chat_id: row.get("from_chat_id"),
        from_chat_name: row.get("from_chat_name"),
        created_at: decode_sqlite_datetime(row, "created_at"),
        updated_at: decode_sqlite_datetime(row, "updated_at"),
    }
}

async fn ensure_song_infos_has_program_id(pool: &SqlitePool) -> Result<()> {
    // Prefer schema inspection over matching error strings from ALTER TABLE.
    let rows = sqlx::query("PRAGMA table_info(song_infos)")
        .fetch_all(pool)
        .await?;

    let has_program_id = rows.iter().any(|row| {
        row.try_get::<String, _>("name")
            .is_ok_and(|name| name == "program_id")
    });

    if !has_program_id {
        sqlx::query("ALTER TABLE song_infos ADD COLUMN program_id INTEGER")
            .execute(pool)
            .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests;
