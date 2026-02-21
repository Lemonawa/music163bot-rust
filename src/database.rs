use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};

use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SongInfo {
    pub id: i64,
    pub music_id: i64,
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
    pub async fn new(database_url: &str) -> Result<Self> {
        // Create database directory if it doesn't exist
        if let Some(parent) = std::path::Path::new(database_url).parent()
            && !parent.exists()
        {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Configure connection pool with WAL mode for better concurrency
        // WAL mode allows readers and writers to operate concurrently
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(database_url)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal) // 启用 WAL 模式
            .busy_timeout(Duration::from_secs(30)) // 忙等待超时
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal) // 平衡性能和耐久性
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
    pub async fn get_song_by_music_id(&self, music_id: i64) -> Result<Option<SongInfo>> {
        let query_start = Instant::now();
        let row_result = sqlx::query(
            "SELECT id, music_id, song_name, song_artists, song_album, file_ext, \
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
    pub async fn save_song_info(&self, song_info: &SongInfo) -> Result<i64> {
        let query_start = Instant::now();
        let result = sqlx::query(
            r"
            INSERT INTO song_infos (
                music_id, song_name, song_artists, song_album, file_ext,
                music_size, pic_size, emb_pic_size, bit_rate, duration,
                file_id, thumb_file_id, from_user_id, from_user_name,
                from_chat_id, from_chat_name, created_at, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            ON CONFLICT(music_id) DO UPDATE SET
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

    /// Update `file_id` and `thumb_file_id` for a song
    pub async fn update_file_ids(
        &self,
        music_id: i64,
        file_id: Option<String>,
        thumb_file_id: Option<String>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE song_infos SET file_id = ?, thumb_file_id = ?, updated_at = CURRENT_TIMESTAMP WHERE music_id = ?"
        )
        .bind(&file_id)
        .bind(&thumb_file_id)
        .bind(music_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Count total songs
    pub async fn count_total_songs(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) as count FROM song_infos")
            .fetch_one(&self.pool)
            .await?;

        Ok(row.get("count"))
    }

    /// Count songs from specific user
    pub async fn count_songs_from_user(&self, user_id: i64) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) as count FROM song_infos WHERE from_user_id = ?")
            .bind(user_id)
            .fetch_one(&self.pool)
            .await?;

        Ok(row.get("count"))
    }

    /// Count songs from specific chat
    pub async fn count_songs_from_chat(&self, chat_id: i64) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) as count FROM song_infos WHERE from_chat_id = ?")
            .bind(chat_id)
            .fetch_one(&self.pool)
            .await?;

        Ok(row.get("count"))
    }

    /// Count status metrics in one query: total songs, songs from user, songs from chat
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
    pub async fn clear_all_songs(&self) -> Result<u64> {
        let result = sqlx::query("DELETE FROM song_infos")
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected())
    }

    /// Optimize database by running VACUUM to reclaim space and defragment
    /// Should be called periodically after many deletions
    pub async fn optimize(&self) -> Result<()> {
        sqlx::query("VACUUM").execute(&self.pool).await?;
        tracing::info!("Database VACUUM completed successfully");
        Ok(())
    }

    /// Run ANALYZE to update query planner statistics
    pub async fn analyze(&self) -> Result<()> {
        sqlx::query("ANALYZE").execute(&self.pool).await?;
        tracing::debug!("Database ANALYZE completed");
        Ok(())
    }

    /// Run lightweight SQLite planner maintenance
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

    row.try_get::<String, _>(column)
        .ok()
        .and_then(|value| parse_sqlite_timestamp(&value))
        .unwrap_or_else(Utc::now)
}

fn map_song_info_row(row: &SqliteRow) -> SongInfo {
    SongInfo {
        id: row.get("id"),
        music_id: row.get("music_id"),
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{Database, SongInfo};
    use chrono::{TimeZone, Timelike, Utc};

    fn cleanup_db_files(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{path}-wal", path = path.display()));
        let _ = std::fs::remove_file(format!("{path}-shm", path = path.display()));
    }

    fn build_temp_db_path(prefix: &str) -> PathBuf {
        let temp_name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}_{temp_name}.db"))
    }

    async fn create_temp_db(prefix: &str) -> (Database, PathBuf) {
        let temp_path = build_temp_db_path(prefix);
        let temp_path_str = temp_path.to_string_lossy().to_string();
        let db = Database::new(&temp_path_str).await.expect("create db");
        (db, temp_path)
    }

    fn sample_song_info(music_id: i64) -> SongInfo {
        SongInfo {
            music_id,
            song_name: format!("Song {music_id}"),
            song_artists: "Artist".to_string(),
            song_album: "Album".to_string(),
            file_ext: "mp3".to_string(),
            music_size: 2_048,
            pic_size: 0,
            emb_pic_size: 0,
            bit_rate: 128_000,
            duration: 180,
            file_id: Some(format!("file_{music_id}")),
            thumb_file_id: Some(format!("thumb_{music_id}")),
            from_user_id: 100,
            from_user_name: "user".to_string(),
            from_chat_id: 200,
            from_chat_name: "chat".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn status_stats_query_uses_index_friendly_subqueries() {
        let sql = super::status_stats_query_sql();
        assert!(sql.contains("(SELECT COUNT(*) FROM song_infos)"));
        assert!(sql.contains("WHERE from_user_id = ?"));
        assert!(sql.contains("WHERE from_chat_id = ?"));
    }

    #[tokio::test]
    async fn status_counts_returns_total_user_and_chat_counts() {
        let temp_name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let temp_path =
            std::env::temp_dir().join(format!("music163bot_status_counts_{temp_name}.db"));
        let temp_path_str = temp_path.to_string_lossy().to_string();

        let db = Database::new(&temp_path_str).await.expect("create db");

        let first = SongInfo {
            music_id: 1,
            song_name: "Song A".to_string(),
            song_artists: "Artist A".to_string(),
            song_album: "Album A".to_string(),
            file_ext: "mp3".to_string(),
            music_size: 2048,
            pic_size: 0,
            emb_pic_size: 0,
            bit_rate: 128_000,
            duration: 180,
            from_user_id: 100,
            from_user_name: "user-a".to_string(),
            from_chat_id: 200,
            from_chat_name: "chat-a".to_string(),
            ..Default::default()
        };

        let second = SongInfo {
            music_id: 2,
            from_user_id: 101,
            from_user_name: "user-b".to_string(),
            ..first.clone()
        };

        db.save_song_info(&first).await.expect("insert first");
        db.save_song_info(&second).await.expect("insert second");

        let (total, from_user, from_chat) = db
            .count_status_stats(100, 200)
            .await
            .expect("status counts");

        assert_eq!(total, 2);
        assert_eq!(from_user, 1);
        assert_eq!(from_chat, 2);

        drop(db);
        cleanup_db_files(&temp_path);
    }

    #[tokio::test]
    async fn pool_options_init_succeeds_with_explicit_bounds() {
        // Verifies that SqlitePoolOptions with explicit max/min connections works
        let db = Database::new("sqlite::memory:").await;
        assert!(
            db.is_ok(),
            "DB init with explicit pool bounds should succeed"
        );
    }

    #[tokio::test]
    async fn get_song_returns_all_mapped_fields() {
        let db = Database::new("sqlite::memory:").await.unwrap();
        let now = chrono::Utc::now();
        let song = SongInfo {
            id: 0,
            music_id: 12345,
            song_name: "Test".to_string(),
            song_artists: "Artist".to_string(),
            song_album: "Album".to_string(),
            file_ext: "mp3".to_string(),
            music_size: 5_000_000,
            pic_size: 0,
            emb_pic_size: 0,
            bit_rate: 320_000,
            duration: 180,
            file_id: Some("file_abc".to_string()),
            thumb_file_id: Some("thumb_abc".to_string()),
            from_user_id: 100,
            from_user_name: "user".to_string(),
            from_chat_id: 200,
            from_chat_name: "chat".to_string(),
            created_at: now,
            updated_at: now,
        };
        db.save_song_info(&song).await.unwrap();
        let fetched = db.get_song_by_music_id(12345).await.unwrap().unwrap();
        assert_eq!(fetched.music_id, 12345);
        assert_eq!(fetched.song_name, "Test");
        assert_eq!(fetched.file_id, Some("file_abc".to_string()));
        assert_eq!(fetched.bit_rate, 320_000);
        assert_eq!(fetched.duration, 180);
    }

    #[tokio::test]
    async fn get_song_by_music_id_parses_sqlite_timestamp() {
        let temp_name = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let temp_path =
            std::env::temp_dir().join(format!("music163bot_timestamp_parse_{temp_name}.db"));
        let temp_path_str = temp_path.to_string_lossy().to_string();

        let db = Database::new(&temp_path_str).await.expect("create db");

        sqlx::query(
            r"
            INSERT INTO song_infos (
                music_id, song_name, song_artists, song_album, file_ext,
                music_size, pic_size, emb_pic_size, bit_rate, duration,
                file_id, thumb_file_id, from_user_id, from_user_name,
                from_chat_id, from_chat_name, created_at, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ",
        )
        .bind(42_i64)
        .bind("Song Timestamp")
        .bind("Artist Timestamp")
        .bind("Album Timestamp")
        .bind("mp3")
        .bind(2048_i64)
        .bind(0_i64)
        .bind(0_i64)
        .bind(128_000_i64)
        .bind(200_i64)
        .bind::<Option<String>>(None)
        .bind::<Option<String>>(None)
        .bind(100_i64)
        .bind("user-timestamp")
        .bind(200_i64)
        .bind("chat-timestamp")
        .bind("2024-02-01 12:34:56")
        .bind("2024-02-01 12:34:56")
        .execute(&db.pool)
        .await
        .expect("insert row");

        let song = db
            .get_song_by_music_id(42)
            .await
            .expect("get song")
            .expect("song exists");

        let expected = Utc.with_ymd_and_hms(2024, 2, 1, 12, 34, 56).unwrap();
        assert_eq!(song.created_at, expected);
        assert_eq!(song.updated_at, expected);

        drop(db);
        cleanup_db_files(&temp_path);
    }

    #[test]
    fn parse_sqlite_timestamp_handles_fractional_seconds() {
        let without_frac = super::parse_sqlite_timestamp("2024-02-01 12:34:56");
        let expected = Utc.with_ymd_and_hms(2024, 2, 1, 12, 34, 56).unwrap();
        assert_eq!(without_frac, Some(expected));

        let with_frac = super::parse_sqlite_timestamp("2024-02-01 12:34:56.123");
        assert!(with_frac.is_some());
        let parsed = with_frac.unwrap();
        assert_eq!(parsed.date_naive(), expected.date_naive());
        assert_eq!(parsed.time().hour(), 12);
        assert_eq!(parsed.time().minute(), 34);
        assert_eq!(parsed.time().second(), 56);
    }

    #[tokio::test]
    async fn save_song_info_upsert_updates_fields_for_existing_music_id() {
        let (db, temp_path) = create_temp_db("music163bot_upsert").await;

        let mut first = sample_song_info(7_001);
        first.song_name = "Original Song".to_string();
        first.file_ext = "mp3".to_string();
        first.bit_rate = 128_000;
        first.duration = 180;
        first.file_id = Some("file_old".to_string());
        first.thumb_file_id = Some("thumb_old".to_string());
        db.save_song_info(&first).await.expect("insert first");

        let mut second = first.clone();
        second.song_name = "Updated Song".to_string();
        second.file_ext = "flac".to_string();
        second.bit_rate = 320_000;
        second.duration = 240;
        second.file_id = Some("file_new".to_string());
        second.thumb_file_id = Some("thumb_new".to_string());
        db.save_song_info(&second).await.expect("upsert second");

        let fetched = db
            .get_song_by_music_id(first.music_id)
            .await
            .expect("get song")
            .expect("song exists");

        assert_eq!(fetched.song_name, "Updated Song");
        assert_eq!(fetched.file_ext, "flac");
        assert_eq!(fetched.bit_rate, 320_000);
        assert_eq!(fetched.duration, 240);
        assert_eq!(fetched.file_id.as_deref(), Some("file_new"));
        assert_eq!(fetched.thumb_file_id.as_deref(), Some("thumb_new"));
        assert_eq!(db.count_total_songs().await.expect("count songs"), 1);

        drop(db);
        cleanup_db_files(&temp_path);
    }

    #[tokio::test]
    async fn update_file_ids_only_changes_target_file_fields() {
        let (db, temp_path) = create_temp_db("music163bot_update_file_ids").await;

        let mut original = sample_song_info(7_002);
        original.song_name = "No Field Drift".to_string();
        original.song_artists = "Artist Stable".to_string();
        original.song_album = "Album Stable".to_string();
        original.file_ext = "ogg".to_string();
        original.bit_rate = 192_000;
        original.duration = 222;
        original.file_id = Some("file_before".to_string());
        original.thumb_file_id = Some("thumb_before".to_string());

        db.save_song_info(&original).await.expect("insert song");
        db.update_file_ids(
            original.music_id,
            Some("file_after".to_string()),
            Some("thumb_after".to_string()),
        )
        .await
        .expect("update file ids");

        let fetched = db
            .get_song_by_music_id(original.music_id)
            .await
            .expect("get song")
            .expect("song exists");

        assert_eq!(fetched.file_id.as_deref(), Some("file_after"));
        assert_eq!(fetched.thumb_file_id.as_deref(), Some("thumb_after"));
        assert_eq!(fetched.song_name, original.song_name);
        assert_eq!(fetched.song_artists, original.song_artists);
        assert_eq!(fetched.song_album, original.song_album);
        assert_eq!(fetched.file_ext, original.file_ext);
        assert_eq!(fetched.bit_rate, original.bit_rate);
        assert_eq!(fetched.duration, original.duration);
        assert_eq!(fetched.music_size, original.music_size);
        assert_eq!(fetched.pic_size, original.pic_size);
        assert_eq!(fetched.emb_pic_size, original.emb_pic_size);
        assert_eq!(fetched.from_user_id, original.from_user_id);
        assert_eq!(fetched.from_chat_id, original.from_chat_id);

        drop(db);
        cleanup_db_files(&temp_path);
    }

    #[tokio::test]
    async fn delete_song_by_music_id_returns_true_when_deleted_and_false_when_missing() {
        let (db, temp_path) = create_temp_db("music163bot_delete_semantics").await;
        let song = sample_song_info(7_003);

        db.save_song_info(&song).await.expect("insert song");

        let first_delete = db
            .delete_song_by_music_id(song.music_id)
            .await
            .expect("delete existing song");
        let second_delete = db
            .delete_song_by_music_id(song.music_id)
            .await
            .expect("delete missing song");
        let unrelated_delete = db
            .delete_song_by_music_id(999_999)
            .await
            .expect("delete unrelated missing song");

        assert!(first_delete);
        assert!(!second_delete);
        assert!(!unrelated_delete);
        assert_eq!(db.count_total_songs().await.expect("count songs"), 0);

        drop(db);
        cleanup_db_files(&temp_path);
    }

    #[tokio::test]
    async fn clear_all_songs_returns_deleted_count_and_empties_table() {
        let (db, temp_path) = create_temp_db("music163bot_clear_all").await;

        for music_id in [7_004_i64, 7_005_i64, 7_006_i64] {
            db.save_song_info(&sample_song_info(music_id))
                .await
                .expect("insert song");
        }

        let deleted = db.clear_all_songs().await.expect("clear all songs");
        let remaining = db.count_total_songs().await.expect("count remaining songs");

        assert_eq!(deleted, 3);
        assert_eq!(remaining, 0);
        assert!(
            db.get_song_by_music_id(7_004)
                .await
                .expect("query cleared song")
                .is_none()
        );

        drop(db);
        cleanup_db_files(&temp_path);
    }
}
