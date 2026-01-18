use crate::error::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

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

pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// Create a new database connection
    pub async fn new(database_url: &str) -> Result<Self> {
        // Create database directory if it doesn't exist
        if let Some(parent) = std::path::Path::new(database_url).parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let pool = SqlitePool::connect(&format!("sqlite://{database_url}")).await?;

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
                    created_at: row
                        .get::<String, _>("created_at")
                        .parse()
                        .unwrap_or_else(|_| Utc::now()),
                    updated_at: row
                        .get::<String, _>("updated_at")
                        .parse()
                        .unwrap_or_else(|_| Utc::now()),
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
}
