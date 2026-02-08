use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use std::time::Duration;

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

        let pool = SqlitePool::connect_with(options).await?;

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
        let row = sqlx::query("SELECT * FROM song_infos WHERE music_id = ? LIMIT 1")
            .bind(music_id)
            .fetch_optional(&self.pool)
            .await?;

        match row {
            Some(row) => {
                let song_info = SongInfo {
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
                    created_at: parse_sqlite_timestamp(&row.get::<String, _>("created_at"))
                        .unwrap_or_else(Utc::now),
                    updated_at: parse_sqlite_timestamp(&row.get::<String, _>("updated_at"))
                        .unwrap_or_else(Utc::now),
                };
                Ok(Some(song_info))
            }
            None => Ok(None),
        }
    }

    /// Save or update song info
    pub async fn save_song_info(&self, song_info: &SongInfo) -> Result<i64> {
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
        .await?;

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
        let result = sqlx::query("DELETE FROM song_infos WHERE music_id = ?")
            .bind(music_id)
            .execute(&self.pool)
            .await?;

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
        COUNT(*) AS total_count,
        COALESCE(SUM(CASE WHEN from_user_id = ? THEN 1 ELSE 0 END), 0) AS user_count,
        COALESCE(SUM(CASE WHEN from_chat_id = ? THEN 1 ELSE 0 END), 0) AS chat_count
    FROM song_infos
    "
}

#[must_use]
fn parse_sqlite_timestamp(value: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|naive| naive.and_utc())
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{Database, SongInfo};
    use chrono::{TimeZone, Utc};

    fn cleanup_db_files(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{path}-wal", path = path.display()));
        let _ = std::fs::remove_file(format!("{path}-shm", path = path.display()));
    }

    #[test]
    fn status_stats_query_uses_single_scan_aggregation() {
        let sql = super::status_stats_query_sql();
        assert!(!sql.contains("(SELECT COUNT(*)"));
        assert!(sql.contains("SUM(CASE"));
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
}
