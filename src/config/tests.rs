use std::time::{SystemTime, UNIX_EPOCH};

use super::{Config, CoverMode, UploadLogLevel};

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
    let temp_name = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp_path = std::env::temp_dir().join(format!("music163bot_config_{temp_name}.ini"));
    let content = "bot.token=token\n\
download.memory_max_file_mb=not-a-number\n\
maintenance.memory_release_interval_requests=bad\n\
maintenance.db_analyze_interval_requests=bad\n";

    std::fs::write(&temp_path, content).expect("write temp config");

    let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");

    std::fs::remove_file(&temp_path).expect("remove temp config");

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
    let temp_name = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp_path = std::env::temp_dir().join(format!("music163bot_upload_pool_{temp_name}.ini"));
    let content = "bot.token=token\n\
upload.pool_max_idle_per_host=2\n\
upload.pool_idle_timeout_secs=120\n";

    std::fs::write(&temp_path, content).expect("write temp config");

    let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");

    std::fs::remove_file(&temp_path).expect("remove temp config");

    assert_eq!(loaded.upload_pool_max_idle_per_host, 2);
    assert_eq!(loaded.upload_pool_idle_timeout_secs, 120);
}

#[test]
fn max_batch_download_tracks_parses() {
    let temp_name = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp_path = std::env::temp_dir().join(format!("music163bot_batch_limit_{temp_name}.ini"));
    let content = "bot.token=token\n\
download.max_batch_tracks=42\n";

    std::fs::write(&temp_path, content).expect("write temp config");

    let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");

    let _ = std::fs::remove_file(&temp_path);

    assert_eq!(loaded.max_batch_download_tracks, 42);
}

#[test]
fn upload_log_level_defaults_to_info() {
    let config = Config::default();
    assert_eq!(config.upload_log_level, UploadLogLevel::Info);
}

#[test]
fn upload_log_level_parses_values() {
    let temp_name = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp_path = std::env::temp_dir().join(format!("music163bot_upload_log_{temp_name}.ini"));
    let content = "bot.token=token\n\
upload.log_level=warn\n";

    std::fs::write(&temp_path, content).expect("write temp config");

    let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");

    let _ = std::fs::remove_file(&temp_path);

    assert_eq!(loaded.upload_log_level, UploadLogLevel::Warning);
}

#[test]
fn upload_max_concurrent_parses() {
    let temp_name = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp_path = std::env::temp_dir().join(format!("music163bot_upload_limit_{temp_name}.ini"));
    let content = "bot.token=token\n\
upload.max_concurrent=6\n";

    std::fs::write(&temp_path, content).expect("write temp config");

    let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");

    let _ = std::fs::remove_file(&temp_path);

    assert_eq!(loaded.upload_max_concurrent, 6);
}

#[test]
fn upload_client_reuse_requests_allows_zero() {
    let temp_name = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp_path = std::env::temp_dir().join(format!("music163bot_upload_reuse_{temp_name}.ini"));
    let content = "bot.token=token\n\
upload.client_reuse_requests=0\n";

    std::fs::write(&temp_path, content).expect("write temp config");

    let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");

    let _ = std::fs::remove_file(&temp_path);

    assert_eq!(loaded.upload_client_reuse_requests, 0);
}

#[test]
fn upload_pool_max_idle_falls_back_to_default() {
    let default_config = Config::default();
    let temp_name = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp_path =
        std::env::temp_dir().join(format!("music163bot_upload_pool_bad_{temp_name}.ini"));
    let content = "bot.token=token\n\
upload.pool_max_idle_per_host=not-a-number\n";

    std::fs::write(&temp_path, content).expect("write temp config");

    let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");

    let _ = std::fs::remove_file(&temp_path);

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
    let temp_name = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp_path = std::env::temp_dir().join(format!("music163bot_local_uri_{temp_name}.ini"));
    let content = "bot.token=token\nupload.local_file_uri=true\n";
    std::fs::write(&temp_path, content).expect("write temp config");
    let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");
    let _ = std::fs::remove_file(&temp_path);
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
fn upload_log_level_parses_mixed_case() {
    assert_eq!(
        "WaRn".parse::<UploadLogLevel>().unwrap(),
        UploadLogLevel::Warning
    );
    assert_eq!(
        "WARNING".parse::<UploadLogLevel>().unwrap(),
        UploadLogLevel::Warning
    );
    assert_eq!(
        "  Debug  ".parse::<UploadLogLevel>().unwrap(),
        UploadLogLevel::Debug
    );
    assert_eq!(
        "NONE".parse::<UploadLogLevel>().unwrap(),
        UploadLogLevel::None
    );
    assert!("garbage".parse::<UploadLogLevel>().is_err());
}

#[test]
fn ini_section_with_non_ascii_name_does_not_panic() {
    let temp_name = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp_path =
        std::env::temp_dir().join(format!("music163bot_non_ascii_section_{temp_name}.ini"));
    let content = "[音乐]\n\
unused=1\n\
[bot]\n\
token=token\n";

    std::fs::write(&temp_path, content).expect("write temp config");

    let loaded = Config::load(temp_path.to_str().expect("temp path")).expect("load config");

    let _ = std::fs::remove_file(&temp_path);

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
