#[test]
fn cover_policy_embeds_for_thumbnail_mode() {
    let policy = resolve_cover_policy(CoverMode::Thumbnail);
    assert!(policy.embed_cover);
    assert!(policy.download_thumbnail);
    assert!(!policy.download_original);
}

#[test]
fn cover_policy_requires_download_when_embed_or_thumbnail() {
    let embed_only = super::CoverPolicy {
        download_original: false,
        download_thumbnail: false,
        embed_cover: true,
    };
    assert!(should_download_cover(embed_only));

    let thumbnail_only = super::CoverPolicy {
        download_original: false,
        download_thumbnail: true,
        embed_cover: false,
    };
    assert!(should_download_cover(thumbnail_only));

    let none = super::CoverPolicy {
        download_original: false,
        download_thumbnail: false,
        embed_cover: false,
    };
    assert!(!should_download_cover(none));
}

#[test]
fn cover_download_failure_notice_mentions_retry_budget_and_fallback() {
    let notice = super::cover_download_failure_notice(&crate::i18n::default_lang_zh());
    assert!(notice.contains('5'));
    assert!(notice.contains("无封面"));
}

#[test]
fn perf_timer_formats_label_and_duration() {
    let label = "fetch_url";
    let formatted = format_perf(label, std::time::Duration::from_millis(12));
    assert!(formatted.contains("fetch_url"));
    assert!(formatted.contains("12"));
}

#[test]
fn build_music_url_accepts_valid_base() {
    let url = build_music_url("https://music.163.com", 123).expect("valid url");
    assert_eq!(url.as_str(), "https://music.163.com/song?id=123");
}

#[test]
fn build_music_url_rejects_invalid_base() {
    assert!(build_music_url("ht!tp:// bad", 1).is_err());
}

#[test]
fn build_program_url_accepts_valid_base() {
    let url = build_program_url("https://music.163.com", 123).expect("valid url");
    assert_eq!(url.as_str(), "https://music.163.com/program?id=123");
}

#[test]
fn cached_music_link_target_prefers_program_when_present() {
    let target = super::cached_music_link_target(Some(3_714_760_479), 1_962_146_519);
    assert_eq!(target, super::MusicLinkTarget::Program(3_714_760_479));
}

#[test]
fn cached_music_link_target_falls_back_to_song() {
    let target = super::cached_music_link_target(None, 1_962_146_519);
    assert_eq!(target, super::MusicLinkTarget::Song(1_962_146_519));
}

#[test]
fn parse_api_url_accepts_valid_base() {
    let url = parse_api_url("https://api.telegram.org/").expect("valid url");
    assert_eq!(url.as_str(), "https://api.telegram.org/");
}

#[test]
fn parse_api_url_rejects_invalid_base() {
    assert!(parse_api_url("not a url").is_err());
}

#[tokio::test]
async fn local_file_uri_disabled_by_default() {
    let config = crate::config::Config {
        bot_api: "http://localhost:8081".to_string(),
        ..crate::config::Config::default()
    };

    let path = create_temp_file();
    let uri = super::maybe_local_file_uri(&config, false, &path).await;
    fs::remove_file(&path).expect("remove temp file");

    assert!(uri.is_none());
}

#[tokio::test]
async fn local_file_uri_skips_official_api() {
    let config = crate::config::Config {
        flags: {
            let mut f = crate::config::ConfigFlags::default();
            f.upload.upload_local_file_uri = true;
            f
        },
        ..crate::config::Config::default()
    };

    let path = create_temp_file();
    let uri = super::maybe_local_file_uri(&config, true, &path).await;
    fs::remove_file(&path).expect("remove temp file");

    assert!(uri.is_none());
}

#[tokio::test]
async fn local_file_uri_builds_from_existing_path() {
    let config = crate::config::Config {
        flags: {
            let mut f = crate::config::ConfigFlags::default();
            f.upload.upload_local_file_uri = true;
            f
        },
        ..crate::config::Config::default()
    };

    let path = create_temp_file();
    let uri = super::maybe_local_file_uri(&config, false, &path).await;
    fs::remove_file(&path).expect("remove temp file");

    let Some(uri) = uri else {
        panic!("expected local file uri");
    };
    assert!(uri.starts_with("file://"));
}

#[tokio::test]
async fn local_file_uri_returns_none_for_missing_path() {
    let config = crate::config::Config {
        flags: {
            let mut f = crate::config::ConfigFlags::default();
            f.upload.upload_local_file_uri = true;
            f
        },
        ..crate::config::Config::default()
    };

    let path = std::env::temp_dir().join(format!("missing_{}", Uuid::new_v4()));
    if path.exists() {
        fs::remove_file(&path).expect("remove temp file");
    }

    let uri = super::maybe_local_file_uri(&config, false, &path).await;
    assert!(uri.is_none());
}

#[tokio::test]
async fn upload_target_defaults_to_multipart() {
    let config = Config::default();
    let path = std::path::Path::new("/tmp/test.mp3");
    assert_eq!(
        super::select_local_upload_target(&config, false, path).await,
        super::UploadFileTarget::Multipart
    );
}

#[tokio::test]
async fn upload_target_uses_local_uri_when_enabled() {
    let config = Config {
        flags: {
            let mut f = crate::config::ConfigFlags::default();
            f.upload.upload_local_file_uri = true;
            f
        },
        ..Config::default()
    };

    let path = create_temp_file();
    let target = super::select_local_upload_target(&config, false, &path).await;
    fs::remove_file(&path).expect("remove temp file");

    match target {
        super::UploadFileTarget::LocalUri(uri) => assert!(uri.starts_with("file://")),
        super::UploadFileTarget::Multipart => panic!("expected local uri"),
    }
}

#[test]
fn get_upload_bot_returns_error_when_missing() {
    let state = UploadClientState {
        bot: None,
        raw_client: None,
        upload_api_url: String::new(),
        reuse_count: 0,
    };
    assert!(get_upload_bot(&state).is_err());
}

#[test]
fn get_upload_bot_returns_bot_when_present() {
    let bot = Bot::new("token");
    let state = UploadClientState {
        bot: Some(bot),
        raw_client: None,
        upload_api_url: String::new(),
        reuse_count: 0,
    };
    assert!(get_upload_bot(&state).is_ok());
}

#[test]
fn upload_prewarm_success_uses_global_info_level() {
    let logs = capture_logs(tracing::Level::INFO, || {
        let runtime = tokio::runtime::Runtime::new().expect("create runtime");
        let success = runtime.block_on(async {
            run_upload_prewarm(|| async { Ok::<(), crate::error::BotError>(()) }).await
        });
        assert!(success);
    });

    assert!(logs.contains("Upload prewarm completed"));
}

#[tokio::test]
async fn acquire_download_permit_returns_error_when_closed() {
    let semaphore = tokio::sync::Semaphore::new(1);
    semaphore.close();

    let Err(err) = acquire_download_permit(&semaphore).await else {
        panic!("expected error for closed semaphore");
    };
    let err_str = format!("{err}");
    assert!(err_str.contains("download semaphore closed"));
}

#[test]
fn is_admin_rejects_message_with_no_sender_even_if_admin_list_contains_zero() {
    use crate::bot::upload::is_admin;
    use crate::config::Config;
    use crate::telegram::{Chat, ChatId, Message, MessageId};

    let config = Config {
        bot_admin: vec![0_i64],
        ..Config::default()
    };

    let msg = Message {
        id: MessageId(1),
        chat: Chat {
            id: ChatId(42),
            type_: "private".to_string(),
            username: None,
        },
        from: None,
        date: 0,
        text: None,
        reply_to_message: None,
    };

    assert!(
        !is_admin(&msg, &config),
        "messages without sender must never be treated as admin"
    );
}

#[test]
fn is_official_telegram_api_treats_empty_string_as_official() {
    assert!(
        super::is_official_telegram_api(""),
        "empty bot_api means default upstream (api.telegram.org), which is the official API"
    );
}

#[test]
fn is_official_telegram_api_recognizes_default_official_url() {
    assert!(super::is_official_telegram_api("https://api.telegram.org"));
    assert!(super::is_official_telegram_api("https://api.telegram.org/"));
}

#[test]
fn is_official_telegram_api_rejects_local_or_custom_hosts() {
    assert!(!super::is_official_telegram_api("http://localhost:8081"));
    assert!(!super::is_official_telegram_api(
        "https://tg-api.example.com"
    ));
}

#[test]
fn post_upload_db_failure_is_logged_not_surfaced() {
    // The audio is already uploaded/delivered before the cache write, so a persistence
    // failure must be downgraded to a log-and-continue (never a user-facing failure).
    assert_eq!(
        super::classify_post_upload_db_result(false),
        super::PostUploadDbAction::LogAndContinue
    );
}

#[test]
fn post_upload_db_success_persists() {
    assert_eq!(
        super::classify_post_upload_db_result(true),
        super::PostUploadDbAction::Persisted
    );
}

#[test]
fn max_download_size_bytes_converts_mb_to_bytes() {
    assert_eq!(super::max_download_size_bytes(0), 0);
    assert_eq!(super::max_download_size_bytes(1), 1024 * 1024);
    assert_eq!(super::max_download_size_bytes(2000), 2000 * 1024 * 1024);
}

#[test]
fn max_download_size_bytes_saturates_instead_of_overflowing() {
    // An absurd configured MB value must clamp to u64::MAX rather than wrap to a small number
    // (which would silently shrink the cap) or panic.
    assert_eq!(super::max_download_size_bytes(u64::MAX), u64::MAX);
    assert_eq!(super::max_download_size_bytes(u64::MAX / 1024), u64::MAX);
}

use super::*;
