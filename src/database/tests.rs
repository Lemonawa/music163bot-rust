use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{Database, SongInfo};
use chrono::{TimeZone, Timelike, Utc};
use sqlx::Row;

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
    let temp_path = std::env::temp_dir().join(format!("music163bot_status_counts_{temp_name}.db"));
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
        program_id: Some(54321),
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
    assert_eq!(fetched.program_id, Some(54321));
    assert_eq!(fetched.song_name, "Test");
    assert_eq!(fetched.file_id, Some("file_abc".to_string()));
    assert_eq!(fetched.bit_rate, 320_000);
    assert_eq!(fetched.duration, 180);
}

#[tokio::test]
async fn get_song_by_music_id_parses_sqlite_timestamp() {
    let (db, temp_path) = create_temp_db("music163bot_timestamp_parse").await;

    let mut song = sample_song_info(42);
    song.song_name = "Song Timestamp".to_string();
    song.song_artists = "Artist Timestamp".to_string();
    song.song_album = "Album Timestamp".to_string();
    song.file_id = None;
    song.thumb_file_id = None;
    song.from_user_name = "user-timestamp".to_string();
    song.from_chat_name = "chat-timestamp".to_string();

    db.save_song_info(&song).await.expect("insert row");

    sqlx::query("UPDATE song_infos SET created_at = ?, updated_at = ? WHERE music_id = ?")
        .bind("2024-02-01 12:34:56")
        .bind("2024-02-01 12:34:56")
        .bind(song.music_id)
        .execute(&db.pool)
        .await
        .expect("set sqlite timestamps");

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
    first.program_id = Some(333_001);
    first.bit_rate = 128_000;
    first.duration = 180;
    first.file_id = Some("file_old".to_string());
    first.thumb_file_id = Some("thumb_old".to_string());
    db.save_song_info(&first).await.expect("insert first");

    let mut second = first.clone();
    second.song_name = "Updated Song".to_string();
    second.file_ext = "flac".to_string();
    second.program_id = Some(333_002);
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
    assert_eq!(fetched.program_id, Some(333_002));
    assert_eq!(fetched.bit_rate, 320_000);
    assert_eq!(fetched.duration, 240);
    assert_eq!(fetched.file_id.as_deref(), Some("file_new"));
    assert_eq!(fetched.thumb_file_id.as_deref(), Some("thumb_new"));
    assert_eq!(db.count_total_songs().await.expect("count songs"), 1);

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

#[tokio::test]
async fn clear_all_songs_truncates_wal_sidecar() {
    let (db, temp_path) = create_temp_db("music163bot_clear_truncates_wal").await;

    for music_id in 8_000_i64..8_050_i64 {
        db.save_song_info(&sample_song_info(music_id))
            .await
            .expect("insert song");
    }

    let wal_path = format!("{}-wal", temp_path.display());
    let wal_size_before = std::fs::metadata(&wal_path).map_or(0, |m| m.len());
    assert!(
        wal_size_before > 0,
        "precondition: WAL sidecar should be non-empty after writes ({wal_size_before} bytes)"
    );

    db.clear_all_songs().await.expect("clear all songs");

    let wal_size_after = std::fs::metadata(&wal_path)
        .map(|m| m.len())
        .expect("wal sidecar should exist after clear");
    assert_eq!(
        wal_size_after, 0,
        "WAL sidecar should be truncated after clearallcache (was {wal_size_after} bytes)"
    );

    drop(db);
    cleanup_db_files(&temp_path);
}

#[tokio::test]
async fn in_memory_dsn_does_not_create_file_in_cwd() {
    let original_dir = std::env::current_dir().expect("get cwd");
    let temp_dir = std::env::temp_dir().join(format!(
        "music163bot_memtest_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp_dir).expect("create temp dir");
    std::env::set_current_dir(&temp_dir).expect("chdir to temp");

    let db = Database::new("sqlite::memory:")
        .await
        .expect("in-memory DB should succeed");

    let suspect = temp_dir.join("sqlite::memory:");
    assert!(
        !suspect.exists(),
        "sqlite::memory: DSN must not create a file in the working directory"
    );

    drop(db);
    std::env::set_current_dir(&original_dir).expect("restore cwd");
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn colon_memory_dsn_does_not_create_file_in_cwd() {
    let db = Database::new(":memory:")
        .await
        .expect(":memory: DB should succeed");

    let cwd = std::env::current_dir().expect("get cwd");
    let suspect = cwd.join(":memory:");
    assert!(
        !suspect.exists(),
        ":memory: DSN must not create a file in the current directory"
    );

    drop(db);
}

#[tokio::test]
async fn migrate_add_program_id_column_to_existing_db() {
    let temp_path = build_temp_db_path("music163bot_migrate_program_id");
    let temp_path_str = temp_path.to_string_lossy().to_string();

    {
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&temp_path_str)
            .create_if_missing(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .expect("connect");

        sqlx::query(
            "CREATE TABLE song_infos (
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
            )",
        )
        .execute(&pool)
        .await
        .expect("create legacy schema");

        pool.close().await;
    }

    let db = Database::new(&temp_path_str)
        .await
        .expect("DB init should run migration");

    let mut song = sample_song_info(99_001);
    song.program_id = Some(42);
    db.save_song_info(&song)
        .await
        .expect("save with program_id");

    let row = sqlx::query("SELECT program_id FROM song_infos WHERE music_id = 99001")
        .fetch_one(&db.pool)
        .await
        .expect("fetch");
    let pid: Option<i64> = row.get("program_id");
    assert_eq!(pid, Some(42), "program_id should be stored after migration");

    drop(db);
    cleanup_db_files(&temp_path);
}

#[tokio::test]
async fn chat_language_roundtrip_set_get_clear() {
    let (db, temp_path) = create_temp_db("music163bot_lang").await;

    assert!(
        db.get_chat_language(555_001).await.unwrap().is_none(),
        "unset chat should have no language override"
    );

    db.set_chat_language(555_001, "en").await.unwrap();
    assert_eq!(
        db.get_chat_language(555_001).await.unwrap().as_deref(),
        Some("en")
    );

    // Upsert replaces the previous value.
    db.set_chat_language(555_001, "zh").await.unwrap();
    assert_eq!(
        db.get_chat_language(555_001).await.unwrap().as_deref(),
        Some("zh")
    );

    db.clear_chat_language(555_001).await.unwrap();
    assert!(db.get_chat_language(555_001).await.unwrap().is_none());

    // Clearing an absent row is a no-op, not an error.
    db.clear_chat_language(555_001).await.unwrap();

    drop(db);
    cleanup_db_files(&temp_path);
}

#[tokio::test]
async fn chat_settings_table_created_on_init() {
    let (db, temp_path) = create_temp_db("music163bot_lang_schema").await;

    let row = sqlx::query(
        "SELECT COUNT(*) AS n FROM sqlite_master WHERE type='table' AND name='chat_settings'",
    )
    .fetch_one(&db.pool)
    .await
    .expect("query schema");
    let n: i64 = row.get("n");
    assert_eq!(n, 1, "chat_settings table should exist after init");

    drop(db);
    cleanup_db_files(&temp_path);
}

#[tokio::test]
async fn decode_sqlite_datetime_warns_on_unparseable_format() {
    let (db, temp_path) = create_temp_db("music163bot_ts_warn").await;

    let song = sample_song_info(99_002);
    db.save_song_info(&song).await.expect("insert row");

    sqlx::query("UPDATE song_infos SET created_at = ? WHERE music_id = ?")
        .bind("not-a-timestamp")
        .bind(song.music_id)
        .execute(&db.pool)
        .await
        .expect("set bad timestamp");

    let fetched = db
        .get_song_by_music_id(99_002)
        .await
        .expect("fetch")
        .expect("row exists");

    let now = chrono::Utc::now();
    let diff = (now - fetched.created_at).num_seconds().unsigned_abs();
    assert!(
        diff < 5,
        "should fall back to approximately Utc::now(); diff = {diff}s"
    );

    drop(db);
    cleanup_db_files(&temp_path);
}
