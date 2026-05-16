use std::time::{SystemTime, UNIX_EPOCH};

use super::{Config, CoverMode};

fn load_temp_config(prefix: &str, content: &str) -> Config {
    let temp_name = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp_path = std::env::temp_dir().join(format!("music163bot_{prefix}_{temp_name}.ini"));

    std::fs::write(&temp_path, content).expect("write temp config");
    let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");
    std::fs::remove_file(&temp_path).expect("remove temp config");
    loaded
}

#[test]
fn download_pool_defaults_are_tunable() {
    let config = Config::default();
    assert!(config.download_pool_max_idle_per_host > 0);
    assert!(config.download_connect_timeout_secs > 0);
}

#[test]
fn download_chunk_size_has_default() {
    let config = Config::default();
    assert!(config.download_chunk_size_kb >= 64);
}

#[test]
fn max_batch_download_tracks_has_default() {
    let config = Config::default();
    assert_eq!(config.max_batch_download_tracks, 20);
}

#[test]
fn memory_max_file_has_default() {
    let config = Config::default();
    assert_eq!(config.memory_max_file_mb, 100);
}

#[test]
fn max_disk_download_has_default() {
    let config = Config::default();
    assert!(
        config.max_disk_download_mb >= 100,
        "default disk download cap should be at least 100 MB"
    );
}

#[test]
fn max_disk_download_parses() {
    let content = "bot.token=token\n\
download.max_disk_download_mb=512\n";

    let loaded = load_temp_config("disk_cap", content);

    assert_eq!(loaded.max_disk_download_mb, 512);
}

#[test]
fn upload_defaults_use_reuse_settings() {
    let config = Config::default();
    assert_eq!(config.upload_client_reuse_requests, 0);
    assert_eq!(config.upload_max_concurrent, 1);
    assert_eq!(config.upload_pool_max_idle_per_host, 1);
    assert_eq!(config.upload_pool_idle_timeout_secs, 300);
    assert_eq!(config.upload_timeout_secs, 300);
}

#[test]
fn maintenance_interval_defaults_exist() {
    let config = Config::default();
    assert!(config.memory_release_interval_requests >= 1);
    assert!(config.db_analyze_interval_requests >= 1);
}

#[test]
fn default_cover_mode_is_thumbnail() {
    let config = Config::default();
    assert_eq!(config.cover_mode, CoverMode::Thumbnail);
}

#[test]
fn invalid_numeric_config_keeps_defaults() {
    let default_config = Config::default();
    let content = "bot.token=token\n\
download.memory_max_file_mb=not-a-number\n\
maintenance.memory_release_interval_requests=bad\n\
maintenance.db_analyze_interval_requests=bad\n";

    let loaded = load_temp_config("config", content);

    assert_eq!(loaded.memory_max_file_mb, default_config.memory_max_file_mb);
    assert_eq!(
        loaded.memory_release_interval_requests,
        default_config.memory_release_interval_requests
    );
    assert_eq!(
        loaded.db_analyze_interval_requests,
        default_config.db_analyze_interval_requests
    );
}

#[test]
fn upload_pool_config_parses() {
    let content = "bot.token=token\n\
upload.pool_max_idle_per_host=2\n\
upload.pool_idle_timeout_secs=120\n";

    let loaded = load_temp_config("upload_pool", content);

    assert_eq!(loaded.upload_pool_max_idle_per_host, 2);
    assert_eq!(loaded.upload_pool_idle_timeout_secs, 120);
}

#[test]
fn max_batch_download_tracks_parses() {
    let content = "bot.token=token\n\
download.max_batch_tracks=42\n";

    let loaded = load_temp_config("batch_limit", content);

    assert_eq!(loaded.max_batch_download_tracks, 42);
}

#[test]
fn upload_max_concurrent_parses() {
    let content = "bot.token=token\n\
upload.max_concurrent=6\n";

    let loaded = load_temp_config("upload_limit", content);

    assert_eq!(loaded.upload_max_concurrent, 6);
}

#[test]
fn upload_client_reuse_requests_allows_zero() {
    let content = "bot.token=token\n\
upload.client_reuse_requests=0\n";

    let loaded = load_temp_config("upload_reuse", content);

    assert_eq!(loaded.upload_client_reuse_requests, 0);
}

#[test]
fn upload_pool_max_idle_falls_back_to_default() {
    let default_config = Config::default();
    let content = "bot.token=token\n\
upload.pool_max_idle_per_host=not-a-number\n";

    let loaded = load_temp_config("upload_pool_bad", content);

    assert_eq!(
        loaded.upload_pool_max_idle_per_host,
        default_config.upload_pool_max_idle_per_host
    );
}

#[test]
fn upload_local_file_uri_defaults_false() {
    let config = Config::default();
    assert!(!config.upload_local_file_uri);
}

#[test]
fn upload_local_file_uri_parses_true() {
    let content = "bot.token=token\nupload.local_file_uri=true\n";
    let loaded = load_temp_config("local_uri", content);
    assert!(loaded.upload_local_file_uri);
}

#[test]
fn config_bool_parsing_handles_common_values() {
    assert_eq!(super::parse_bool_like("true"), Some(true));
    assert_eq!(super::parse_bool_like("TRUE"), Some(true));
    assert_eq!(super::parse_bool_like(" false "), Some(false));
    assert_eq!(super::parse_bool_like("invalid"), None);
}

#[test]
fn cover_mode_parses_mixed_case() {
    use super::CoverMode;
    assert_eq!(
        "ThUmBnAiL".parse::<CoverMode>().unwrap(),
        CoverMode::Thumbnail
    );
    assert_eq!(
        "ORIGINAL".parse::<CoverMode>().unwrap(),
        CoverMode::Original
    );
    assert_eq!("BoTh".parse::<CoverMode>().unwrap(), CoverMode::Both);
    assert!("invalid".parse::<CoverMode>().is_err());
}

#[test]
fn storage_mode_parses_mixed_case() {
    use super::StorageMode;
    assert_eq!(
        "HyBrId".parse::<StorageMode>().unwrap(),
        StorageMode::Hybrid
    );
    assert_eq!("DISK".parse::<StorageMode>().unwrap(), StorageMode::Disk);
    assert_eq!(
        "Memory".parse::<StorageMode>().unwrap(),
        StorageMode::Memory
    );
    assert!("bogus".parse::<StorageMode>().is_err());
}

#[test]
fn ini_section_with_non_ascii_name_does_not_panic() {
    let content = "[音乐]\n\
unused=1\n\
[bot]\n\
token=token\n";

    let loaded = load_temp_config("non_ascii_section", content);

    assert_eq!(loaded.bot_token, "token");
}

#[test]
fn parse_field_returns_parsed_value() {
    let result: u32 = super::parse_field("42", 0, "test_key");
    assert_eq!(result, 42);
}

#[test]
fn parse_field_returns_default_on_invalid() {
    let result: u32 = super::parse_field("not_a_number", 99, "test_key");
    assert_eq!(result, 99);
}

#[test]
fn parse_bool_field_updates_target() {
    let mut target = false;
    super::apply_bool_field("true", &mut target, "test_key");
    assert!(target);
}

#[test]
fn parse_bool_field_keeps_default_on_invalid() {
    let mut target = true;
    super::apply_bool_field("banana", &mut target, "test_key");
    assert!(target); // unchanged
}

#[test]
fn legacy_upload_log_level_is_ignored() {
    let default_config = Config::default();
    let content = "bot.token=token\n\
upload.log_level=debug\n";

    let loaded = load_temp_config("legacy_upload_log", content);

    assert_eq!(loaded.log_level, default_config.log_level);
}

#[test]
fn load_returns_error_when_file_missing() {
    let result = Config::load("/tmp/music163bot_missing_config_does_not_exist.ini");
    assert!(
        result.is_err(),
        "should return error for missing config file"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Config file not found"),
        "error should mention missing file: {err_msg}"
    );
}

#[test]
fn ini_section_names_are_case_insensitive() {
    let content = "[Bot]\n\
token=case_insensitive_token\n\
[Music]\n\
api=https://custom.api\n";

    let loaded = load_temp_config("case_insensitive_section", content);

    assert_eq!(loaded.bot_token, "case_insensitive_token");
    assert_eq!(loaded.music_api, "https://custom.api");
}

#[test]
fn ini_section_uppercase_matches_lookups() {
    let content = "[BOT]\n\
token=case_insensitive_token\n\
[DATABASE]\n\
url=postgresql://localhost/db\n";

    let loaded = load_temp_config("uppercase_section", content);

    assert_eq!(loaded.bot_token, "case_insensitive_token");
    assert_eq!(loaded.database, "postgresql://localhost/db");
}

#[test]
fn max_concurrent_downloads_parses_from_download_section() {
    let content = "bot.token=token\n\
[download]\n\
max_concurrent=7\n";

    let loaded = load_temp_config("max_concurrent_download", content);

    assert_eq!(loaded.max_concurrent_downloads, 7);
}

#[test]
fn parse_admin_list_drops_zero_and_negative_ids() {
    let parsed = super::parse_admin_list("123,0,-5,456");

    assert_eq!(parsed, vec![123_i64, 456_i64]);
}

#[test]
fn parse_admin_list_returns_empty_when_only_invalid_entries() {
    assert!(super::parse_admin_list("0,-1,abc").is_empty());
}
