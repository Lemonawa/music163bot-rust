
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
