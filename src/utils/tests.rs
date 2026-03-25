use std::time::Duration;

use super::{
    MusicCollectionTarget, clean_filename, ensure_dir, extract_retry_after_seconds,
    parse_music_collection_target, parse_music_id, parse_music_program_id, throughput_mbps,
    update_peak,
};

#[test]
fn parse_music_id_fast_path_detects_direct_numeric() {
    assert_eq!(super::parse_direct_numeric_id("123456"), Some(123_456));
    assert_eq!(super::parse_direct_numeric_id("  123456  "), Some(123_456));
    assert_eq!(super::parse_direct_numeric_id("abc123"), None);
}

#[test]
fn parse_music_id_fast_path_handles_canonical_song_url() {
    assert_eq!(
        super::extract_music_id_from_canonical_song_url("https://music.163.com/song?id=424242"),
        Some(424_242)
    );
    assert_eq!(
        super::extract_music_id_from_canonical_song_url(
            "https://music.163.com/#/song?id=424242&foo=bar"
        ),
        Some(424_242)
    );
    assert_eq!(
        super::extract_music_id_from_canonical_song_url("https://example.com/song?id=424242"),
        None
    );
    assert_eq!(
        super::extract_music_id_from_canonical_song_url(
            "https://music.163.com/song?userid=123&id=424242"
        ),
        Some(424_242)
    );
}

#[test]
fn throughput_mbps_calculates_expected_value() {
    let bytes = 10 * 1024 * 1024;
    let duration = Duration::from_secs(2);
    let value = throughput_mbps(bytes, duration);
    assert!((value - 5.0).abs() < 0.01);
}

#[test]
fn update_peak_tracks_highest_value() {
    let counter = std::sync::atomic::AtomicU32::new(0);
    assert_eq!(update_peak(&counter, 1), 1);
    assert_eq!(update_peak(&counter, 2), 2);
    assert_eq!(update_peak(&counter, 2), 2);
    assert_eq!(update_peak(&counter, 1), 2);
}

#[test]
fn parse_music_id_handles_direct_numeric_input() {
    assert_eq!(parse_music_id("123456"), Some(123_456));
    assert_eq!(parse_music_id("  123456  "), Some(123_456));
}

#[test]
fn parse_music_collection_target_detects_playlist_with_optional_uct2() {
    let url = "https://music.163.com/playlist?id=17607381913&uct2=U2FsdGVkX18AISWPo4dHRIRF8KPygbcmfo67g4xh6S8=";
    assert_eq!(
        parse_music_collection_target(url),
        Some(MusicCollectionTarget::Playlist(17_607_381_913))
    );
}

#[test]
fn parse_music_collection_target_detects_album_without_uct2() {
    let url = "https://music.163.com/album?id=121344602";
    assert_eq!(
        parse_music_collection_target(url),
        Some(MusicCollectionTarget::Album(121_344_602))
    );
}

#[test]
fn parse_music_collection_target_rejects_song_link() {
    let url = "https://music.163.com/song?id=424242";
    assert_eq!(parse_music_collection_target(url), None);
}

#[test]
fn parse_music_collection_target_detects_djradio() {
    let url = "https://music.163.com/djradio?id=985936420";
    assert_eq!(
        parse_music_collection_target(url),
        Some(MusicCollectionTarget::DjRadio(985_936_420))
    );
}

#[test]
fn parse_music_program_id_detects_program_link() {
    let url = "https://music.163.com/program?id=3714760479&uct2=foo";
    assert_eq!(parse_music_program_id(url), Some(3_714_760_479));
}

#[test]
fn parse_music_program_id_detects_dj_link() {
    let url = "https://music.163.com/dj?id=3714760479&uct2=foo";
    assert_eq!(parse_music_program_id(url), Some(3_714_760_479));
}

#[test]
fn parse_music_program_id_rejects_song_link() {
    let url = "https://music.163.com/song?id=3714760479";
    assert_eq!(parse_music_program_id(url), None);
}

#[test]
fn is_trusted_music_share_url_accepts_netease_domains() {
    assert!(super::is_trusted_music_share_url(
        "https://music.163.com/song?id=123"
    ));
    assert!(super::is_trusted_music_share_url(
        "https://foo.music.163.com/song?id=123"
    ));
    assert!(super::is_trusted_music_share_url("https://163cn.tv/abcd"));
    assert!(super::is_trusted_music_share_url("https://a.163cn.tv/abcd"));
    assert!(super::is_trusted_music_share_url("https://163cn.link/abcd"));
    assert!(super::is_trusted_music_share_url(
        "https://a.163cn.link/abcd"
    ));
}

#[test]
fn is_trusted_music_share_url_rejects_untrusted_domains() {
    assert!(!super::is_trusted_music_share_url(
        "https://example.com/song?id=123"
    ));
    assert!(!super::is_trusted_music_share_url(
        "https://attacker.test/?q=music.163.com"
    ));
    assert!(!super::is_trusted_music_share_url(
        "ftp://music.163.com/song?id=123"
    ));
}

#[test]
fn extract_first_trusted_music_share_url_skips_untrusted_urls() {
    let text = "see https://example.com/x and https://163cn.link/abcd";
    assert_eq!(
        super::extract_first_trusted_music_share_url(text),
        Some("https://163cn.link/abcd".to_string())
    );
}

#[test]
fn ensure_dir_is_idempotent() {
    let temp_name = format!(
        "music163bot_utils_dir_{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let temp_path = std::env::temp_dir().join(temp_name);
    let temp_path_str = temp_path.to_string_lossy().to_string();

    ensure_dir(&temp_path_str).expect("create dir first time");
    ensure_dir(&temp_path_str).expect("create dir second time");

    std::fs::remove_dir_all(&temp_path).expect("cleanup dir");
}

#[test]
fn clean_filename_handles_all_invalid_chars() {
    let cleaned = clean_filename("/\\?*:|<>\"\n\t\r");
    assert_eq!(cleaned, "untitled");
}

#[test]
fn clean_filename_preserves_valid_names() {
    assert_eq!(clean_filename("hello world.mp3"), "hello world.mp3");
    assert_eq!(clean_filename("  spaced  "), "spaced");
    assert_eq!(clean_filename("a/b"), "a b");
}

#[test]
fn clean_filename_handles_unicode() {
    assert_eq!(clean_filename("你好世界.flac"), "你好世界.flac");
    assert_eq!(clean_filename("café.mp3"), "café.mp3");
}

#[test]
fn is_timeout_error_detects_timeout_message() {
    let err = std::io::Error::new(std::io::ErrorKind::TimedOut, "connection timeout");
    assert!(super::is_timeout_error(&err));
}

#[test]
fn is_timeout_error_rejects_non_timeout() {
    let err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
    assert!(!super::is_timeout_error(&err));
}

#[test]
fn build_http_client_returns_client() {
    let client =
        super::build_http_client(reqwest::Client::builder()).expect("client should be built");
    let _ = client;
}

#[test]
fn build_http_client_sanitizes_builder_failures() {
    let err = super::build_http_client(reqwest::Client::builder().user_agent("bad\nagent"))
        .expect_err("invalid user agent should fail client build");

    let err_msg = err.to_string();
    assert!(err_msg.contains("Failed to build HTTP client"));
}

#[test]
fn build_http_client_uses_dedicated_error_variant() {
    let err = super::build_http_client(reqwest::Client::builder().user_agent("bad\nagent"))
        .expect_err("invalid user agent should fail client build");

    assert!(matches!(err, crate::error::BotError::HttpClientBuild(_)));
}

#[test]
fn sanitize_sensitive_text_redacts_telegram_bot_token_path() {
    let raw = "error sending request for url (http://127.0.0.1:8081/bot123456789:fake_test_token/sendAudio)";

    let sanitized = super::sanitize_sensitive_text(raw);

    assert!(!sanitized.contains("123456789:fake_test_token"));
    assert!(sanitized.contains("/bot<redacted>/sendAudio"));
}

#[test]
fn sanitize_sensitive_text_redacts_url_credentials() {
    let raw = "proxy error for https://user:secret@example.com/path";

    let sanitized = super::sanitize_sensitive_text(raw);

    assert!(!sanitized.contains("user:secret@"));
    assert!(sanitized.contains("https://<redacted>@example.com/path"));
}

#[test]
fn sanitize_sensitive_text_redacts_music_u_cookie() {
    let raw = "MUSIC_U=super_secret_cookie";

    let sanitized = super::sanitize_sensitive_text(raw);

    assert!(!sanitized.contains("super_secret_cookie"));
    assert!(sanitized.contains("MUSIC_U=<redacted>"));
}

#[test]
fn sanitize_sensitive_text_redacts_custom_authorization_scheme() {
    let raw = "Authorization: Token super_secret_value";

    let sanitized = super::sanitize_sensitive_text(raw);

    assert!(!sanitized.contains("super_secret_value"));
    assert_eq!(sanitized, "Authorization: <redacted>");
}

#[test]
fn sanitize_sensitive_text_redacts_secret_query_parameters() {
    let raw = "https://example.com/file?token=abc123&signature=def456&download=1&X-Amz-Credential=ghi789&X-Amz-Security-Token=jkl012&GoogleAccessId=mno345";

    let sanitized = super::sanitize_sensitive_text(raw);

    assert!(!sanitized.contains("abc123"));
    assert!(!sanitized.contains("def456"));
    assert!(!sanitized.contains("ghi789"));
    assert!(!sanitized.contains("jkl012"));
    assert!(!sanitized.contains("mno345"));
    assert!(sanitized.contains("token=<redacted>"));
    assert!(sanitized.contains("signature=<redacted>"));
    assert!(sanitized.contains("download=1"));
    assert!(sanitized.contains("X-Amz-Credential=<redacted>"));
    assert!(sanitized.contains("X-Amz-Security-Token=<redacted>"));
    assert!(sanitized.contains("GoogleAccessId=<redacted>"));
}

#[test]
fn sanitize_sensitive_text_redacts_telegram_bot_token_without_trailing_slash() {
    let raw = "Invalid custom API URL 'https://proxy.local/bot123456789:fake_test_token'";

    let sanitized = super::sanitize_sensitive_text(raw);

    assert!(!sanitized.contains("123456789:fake_test_token"));
    assert!(sanitized.contains("/bot<redacted>"));
}

#[test]
fn extract_retry_after_seconds_parses_common_telegram_formats() {
    assert_eq!(
        extract_retry_after_seconds(
            "Telegram API error: Too Many Requests: retry after 26 (HTTP 429 Too Many Requests)"
        ),
        Some(26)
    );
    assert_eq!(extract_retry_after_seconds("Retry after 26s"), Some(26));
    assert_eq!(extract_retry_after_seconds("retry after 7"), Some(7));
}

#[test]
fn extract_retry_after_seconds_rejects_unrelated_messages() {
    assert_eq!(
        extract_retry_after_seconds("HTTP 429 without retry hint"),
        None
    );
    assert_eq!(
        extract_retry_after_seconds("Upload failed after 2.04s (15.25 MB/s)"),
        None
    );
}
