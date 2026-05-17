use super::*;

#[test]
fn inline_query_search_prefix_parsed_once() {
    let (keyword, is_search) = super::parse_inline_query_keyword("search keyword");
    assert!(is_search);
    assert_eq!(keyword, "keyword");

    let (keyword, is_search) = super::parse_inline_query_keyword("search");
    assert!(is_search);
    assert!(keyword.is_empty());

    let (keyword, is_search) = super::parse_inline_query_keyword("hello world");
    assert!(!is_search);
    assert_eq!(keyword, "hello world");
}

#[test]
fn start_with_music_id_uses_direct_process_path() {
    assert_eq!(super::parse_start_music_id(Some("123")), Some(123));
    assert_eq!(super::parse_start_music_id(Some("  456  ")), Some(456));
    assert_eq!(super::parse_start_music_id(Some("invalid")), None);
    assert_eq!(super::parse_start_music_id(None), None);
}

#[test]
fn parse_direct_music_target_detects_song_program_and_collection() {
    assert!(matches!(
        super::parse_direct_music_target("123456"),
        Some(super::ParsedMusicTarget::Song(123_456))
    ));
    assert!(matches!(
        super::parse_direct_music_target("https://music.163.com/program?id=3714760479"),
        Some(super::ParsedMusicTarget::Program(3_714_760_479))
    ));
    assert!(matches!(
        super::parse_direct_music_target("https://music.163.com/playlist?id=17607381913"),
        Some(super::ParsedMusicTarget::Collection(
            crate::utils::MusicCollectionTarget::Playlist(17_607_381_913)
        ))
    ));
}

#[test]
fn parse_direct_music_target_returns_none_for_unmatched_text() {
    assert!(super::parse_direct_music_target("hello world").is_none());
}

#[test]
fn parse_command_and_args_handles_bot_mention_and_whitespace() {
    let (cmd, args) = super::parse_command_and_args("/search@mybot    hello world");
    assert_eq!(cmd, "search");
    assert_eq!(args.as_deref(), Some("hello world"));

    let (cmd, args) = super::parse_command_and_args("/status@mybot");
    assert_eq!(cmd, "status");
    assert_eq!(args, None);

    let (cmd, args) = super::parse_command_and_args("/start   ");
    assert_eq!(cmd, "start");
    assert_eq!(args, None);
}

#[test]
fn runtime_metrics_cache_hit_rate_tracks_counts() {
    let metrics = super::RuntimeMetrics::new();
    metrics.record_cache_hit();
    metrics.record_cache_hit();
    metrics.record_cache_miss();

    let snapshot = metrics.cache_snapshot();
    assert_eq!(snapshot.hits, 2);
    assert_eq!(snapshot.misses, 1);
    assert!((snapshot.hit_rate_percent - 66.67).abs() < 0.1);
}

#[test]
fn runtime_metrics_speed_window_keeps_recent_samples() {
    let metrics = super::RuntimeMetrics::new();
    for mb in 1..=25u64 {
        metrics.record_download_speed(mb * 1024 * 1024, Duration::from_secs(1));
    }

    let (download, upload) = metrics.speed_snapshots();
    let download = download.expect("download snapshot should exist");
    assert!(upload.is_none(), "upload should have no samples");
    assert_eq!(download.samples, 25);
    assert_eq!(download.recent_samples, 20);
    assert!((download.last_mbps - 25.0).abs() < 0.01);
    assert!(download.p95_mbps >= 24.0);
}

#[test]
fn speed_line_reports_cache_hit_when_no_samples() {
    let line = super::format_speed_line("下载", None);
    assert!(line.contains("暂无非缓存测速样本"));
}

#[test]
fn speed_line_uses_monospace_for_numeric_values() {
    let line = super::format_speed_line(
        "下载",
        Some(super::SpeedSnapshot {
            last_mbps: 6.0,
            avg_mbps: 4.5,
            p95_mbps: 5.2,
            samples: 12,
            recent_samples: 12,
        }),
    );
    assert!(line.contains("<code>6.00</code>"));
    assert!(line.contains("<code>4.50</code>"));
    assert!(line.contains("<code>5.20</code>"));
    assert!(line.contains("<code>12</code>"));
}

#[test]
fn status_text_uses_section_layout_and_split_memory_fields() {
    let cache_snapshot = super::CacheSnapshot {
        hits: 9,
        misses: 3,
        hit_rate_percent: 75.0,
    };
    let resource_snapshot = super::ResourceSnapshot {
        cpu_percent: 12.5,
        system_used_memory_mb: 512,
        system_total_memory_mb: 1024,
        bot_memory_mb: Some(12),
    };
    let text = super::build_status_text(&super::StatusTextParams {
        total_count: 100,
        user_count: 20,
        chat_count: 8,
        cache_snapshot,
        resource_snapshot,
        uptime: "00:10:00",
        download_line: "下载: 实时 <code>6.00</code> MB/s | 平均 <code>4.00</code> MB/s | P95 <code>5.00</code> MB/s | 样本 <code>12</code> (窗口 <code>12</code>)",
        upload_line: "上传: 实时 <code>2.00</code> MB/s | 平均 <code>1.50</code> MB/s | P95 <code>1.80</code> MB/s | 样本 <code>12</code> (窗口 <code>12</code>)",
    });
    assert!(text.contains("<b>系统状态</b>"));
    assert!(text.contains("<b>实时运行指标</b>"));
    assert!(text.contains("<b>💾 缓存</b>"));
    assert!(text.contains("• 总缓存: <code>100</code>"));
    assert!(text.contains("• 用户缓存: <code>20</code>"));
    assert!(text.contains("• 群组缓存: <code>8</code>"));
    assert!(text.contains("• 系统内存: <code>512/1024 MB</code>"));
    assert!(text.contains("• Bot 内存: <code>12 MB</code>"));
    assert!(text.contains("• 下载: 实时 <code>6.00</code> MB/s"));
}

#[test]
fn rmcache_usage_prompt_uses_html_code() {
    let text = super::rmcache_usage_prompt();
    assert!(text.contains("<code>/rmcache &lt;音乐ID&gt;</code>"));
}

#[test]
fn clearallcache_confirmation_prompt_uses_html_code() {
    let text = super::clearallcache_confirmation_prompt();
    assert!(text.contains("<code>/clearallcache confirm</code>"));
}

#[test]
fn about_text_includes_build_commit_in_version_line() {
    let text = super::build_about_text();
    assert!(text.contains(&format!("v{}", env!("CARGO_PKG_VERSION"))));
    assert!(text.contains(&format!("({})", super::BUILD_GIT_COMMIT)));
}

#[test]
fn is_spawnable_command_text_requires_leading_slash() {
    assert!(super::is_spawnable_command_text("/start"));
    assert!(super::is_spawnable_command_text("/music 123"));
    assert!(!super::is_spawnable_command_text("  /start"));
    assert!(!super::is_spawnable_command_text("hello"));
}

#[test]
fn is_command_text_requires_leading_slash() {
    assert!(super::is_command_text("/start"));
    assert!(!super::is_command_text("  /start"));
    assert!(!super::is_command_text("hello"));
}

#[test]
fn message_task_route_prefers_commands_over_music_links() {
    assert_eq!(
        super::classify_message_task("/music https://music.163.com/song?id=1"),
        Some(super::MessageTaskRoute::Command)
    );
    assert_eq!(
        super::classify_message_task("https://music.163.com/song?id=1"),
        Some(super::MessageTaskRoute::MusicLink)
    );
}

// --- percentile_95 direct unit tests ---

#[test]
fn percentile_95_single_element_returns_that_element() {
    let samples: VecDeque<f64> = VecDeque::from([42.0]);
    assert!((super::percentile_95(&samples) - 42.0).abs() < f64::EPSILON);
}

#[test]
fn percentile_95_two_elements_returns_higher() {
    let samples: VecDeque<f64> = VecDeque::from([1.0, 10.0]);
    assert!((super::percentile_95(&samples) - 10.0).abs() < f64::EPSILON);
}

#[test]
fn percentile_95_sorted_hundred_elements() {
    let samples: VecDeque<f64> = (1..=100).map(f64::from).collect();
    let p95 = super::percentile_95(&samples);
    assert!(
        (p95 - 95.0).abs() < 0.01 || (p95 - 96.0).abs() < 0.01,
        "p95 of 1..=100 should be around 95 or 96, got {p95}"
    );
}

#[test]
fn percentile_95_unsorted_input() {
    let samples: VecDeque<f64> = VecDeque::from([5.0, 1.0, 3.0, 2.0, 4.0]);
    let p95 = super::percentile_95(&samples);
    assert!(
        (p95 - 5.0).abs() < 0.01 || (p95 - 4.0).abs() < 0.01,
        "p95 should be near the top, got {p95}"
    );
}

#[test]
fn percentile_95_empty_returns_zero() {
    let samples: VecDeque<f64> = VecDeque::new();
    assert!(super::percentile_95(&samples).abs() < f64::EPSILON);
}

// --- should_download_cover tests ---

#[test]
fn should_download_cover_true_when_embed_cover_set() {
    let policy = super::CoverPolicy {
        download_original: false,
        download_thumbnail: false,
        embed_cover: true,
    };
    assert!(super::should_download_cover(policy));
}

#[test]
fn should_download_cover_true_when_download_thumbnail_set() {
    let policy = super::CoverPolicy {
        download_original: false,
        download_thumbnail: true,
        embed_cover: false,
    };
    assert!(super::should_download_cover(policy));
}

#[test]
fn should_download_cover_false_when_nothing_set() {
    let policy = super::CoverPolicy {
        download_original: false,
        download_thumbnail: false,
        embed_cover: false,
    };
    assert!(!super::should_download_cover(policy));
}

// --- resolve_cover_policy covers all modes ---

#[test]
fn resolve_cover_policy_thumbnail_mode() {
    let policy = super::resolve_cover_policy(CoverMode::Thumbnail);
    assert!(!policy.download_original);
    assert!(policy.download_thumbnail);
    assert!(policy.embed_cover);
}

#[test]
fn resolve_cover_policy_original_mode() {
    let policy = super::resolve_cover_policy(CoverMode::Original);
    assert!(policy.download_original);
    assert!(!policy.download_thumbnail);
    assert!(policy.embed_cover);
}

#[test]
fn resolve_cover_policy_both_mode() {
    let policy = super::resolve_cover_policy(CoverMode::Both);
    assert!(policy.download_original);
    assert!(policy.download_thumbnail);
    assert!(policy.embed_cover);
}

// Note: CoverMode has only Thumbnail (default), Original, Both - no None variant.
// resolve_cover_policy always sets embed_cover=true because every variant
// enables either download_original or download_thumbnail.

#[test]
fn should_download_cover_returns_false_when_only_download_original_without_embed() {
    // This state cannot arise from resolve_cover_policy (which always sets
    // embed_cover = download_original || download_thumbnail), but we test
    // the function's own contract: only embed_cover and download_thumbnail
    // are checked, not download_original.
    let policy = super::CoverPolicy {
        download_original: true,
        download_thumbnail: false,
        embed_cover: false,
    };
    assert!(
        !super::should_download_cover(policy),
        "download_original alone should not trigger a download — only embed_cover and download_thumbnail are checked"
    );
}

// --- is_clearallcache_confirm edge cases ---

#[test]
fn is_clearallcache_confirm_accepts_exact_confirm() {
    assert!(super::is_clearallcache_confirm(Some("confirm")));
}

#[test]
fn is_clearallcache_confirm_trims_whitespace() {
    assert!(super::is_clearallcache_confirm(Some("  confirm  ")));
}

#[test]
fn is_clearallcache_confirm_rejects_none() {
    assert!(!super::is_clearallcache_confirm(None));
}

#[test]
fn is_clearallcache_confirm_rejects_other_text() {
    assert!(!super::is_clearallcache_confirm(Some("yes")));
    assert!(!super::is_clearallcache_confirm(Some("Confirm")));
}

// --- format_bitrate_kbps ---

#[test]
fn format_bitrate_kbps_converts_bps_to_kbps() {
    assert_eq!(super::format_bitrate_kbps(320_000), "320.00");
}

#[test]
fn format_bitrate_kbps_handles_zero() {
    assert_eq!(super::format_bitrate_kbps(0), "0.00");
}

#[test]
fn format_bitrate_kbps_clamps_negative_to_zero() {
    assert_eq!(super::format_bitrate_kbps(-1), "0.00");
}

#[test]
fn format_bitrate_kbps_handles_flac_bitrate() {
    assert_eq!(super::format_bitrate_kbps(999_000), "999.00");
}

// --- format_uptime ---

#[test]
fn format_uptime_zero() {
    assert_eq!(super::format_uptime(Duration::ZERO), "00:00:00");
}

#[test]
fn format_uptime_one_hour() {
    assert_eq!(super::format_uptime(Duration::from_hours(1)), "01:00:00");
}

#[test]
fn format_uptime_complex() {
    assert_eq!(super::format_uptime(Duration::from_secs(3661)), "01:01:01");
}

#[test]
fn format_uptime_over_24_hours() {
    // 90061 seconds = 25h 1m 1s
    assert_eq!(super::format_uptime(Duration::from_secs(90061)), "25:01:01");
}

#[test]
fn format_uptime_99_hours() {
    // 356401 seconds = 99h 0m 1s
    assert_eq!(
        super::format_uptime(Duration::from_secs(356_401)),
        "99:00:01"
    );
}

// --- build_caption ---

#[test]
fn build_caption_formats_all_fields() {
    let caption = super::build_caption(
        "Test Song",
        "Artist A",
        "Album B",
        "flac",
        5_242_880, // 5 MB
        999_000,
        "mybot",
    );
    assert!(caption.contains("「Test Song」- Artist A"));
    assert!(caption.contains("专辑: Album B"));
    assert!(caption.contains("#网易云音乐 #flac"));
    assert!(caption.contains("via @mybot"));
    assert!(caption.contains("999.00kbps"));
}

#[test]
fn build_caption_zero_size() {
    let caption = super::build_caption("S", "A", "B", "mp3", 0, 320_000, "bot");
    assert!(caption.contains("0.00MB"));
    assert!(caption.contains("320.00kbps"));
}

#[test]
fn build_caption_handles_empty_strings() {
    let caption = super::build_caption("", "", "", "mp3", 1024, 128_000, "bot");
    assert!(caption.contains("「」- "));
    assert!(caption.contains("专辑: "));
    assert!(caption.contains("#网易云音乐 #mp3"));
    assert!(caption.contains("via @bot"));
}

// --- clearallcache confirmation window expiry logic ---
// Exercises the same DashMap + elapsed pattern used by
// handle_clearallcache_confirm_command without needing a Bot instance.

use crate::telegram::ChatId;
use dashmap::DashMap;
use std::sync::Arc;

/// Mirrors the expiry check from support.rs using the shared constant.
fn is_clearallcache_confirmation_valid(
    confirms: &DashMap<(i64, ChatId), std::time::Instant>,
    user_id: i64,
    chat_id: ChatId,
) -> bool {
    confirms
        .remove(&(user_id, chat_id))
        .and_then(|(_, at)| (at.elapsed() <= super::CLEARALLCACHE_CONFIRM_WINDOW).then_some(()))
        .is_some()
}

#[test]
fn clearallcache_confirmation_accepts_within_window() {
    let confirms: Arc<DashMap<(i64, ChatId), std::time::Instant>> = Arc::new(DashMap::new());
    let user_id = 42i64;
    let chat_id = ChatId(100);

    confirms.insert((user_id, chat_id), std::time::Instant::now());

    assert!(is_clearallcache_confirmation_valid(
        &confirms, user_id, chat_id
    ));
    // Second attempt should fail (entry was consumed by remove)
    assert!(!is_clearallcache_confirmation_valid(
        &confirms, user_id, chat_id
    ));
}

#[test]
fn clearallcache_confirmation_rejects_missing_entry() {
    let confirms: DashMap<(i64, ChatId), std::time::Instant> = DashMap::new();
    assert!(!is_clearallcache_confirmation_valid(
        &confirms,
        1,
        ChatId(2)
    ));
}

#[test]
fn clearallcache_confirmation_rejects_wrong_user() {
    let confirms: DashMap<(i64, ChatId), std::time::Instant> = DashMap::new();
    confirms.insert((1, ChatId(10)), std::time::Instant::now());
    assert!(!is_clearallcache_confirmation_valid(
        &confirms,
        2,
        ChatId(10)
    ));
}

#[test]
fn clearallcache_confirmation_rejects_wrong_chat() {
    let confirms: DashMap<(i64, ChatId), std::time::Instant> = DashMap::new();
    confirms.insert((1, ChatId(10)), std::time::Instant::now());
    assert!(!is_clearallcache_confirmation_valid(
        &confirms,
        1,
        ChatId(20)
    ));
}

#[test]
fn clearallcache_prune_removes_expired_entries() {
    let confirms: DashMap<(i64, ChatId), std::time::Instant> = DashMap::new();

    // Insert an expired entry (60 seconds ago, well beyond the 30s window)
    let expired = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_mins(1))
        .expect("instant - 1m should not underflow");
    confirms.insert((1, ChatId(10)), expired);
    confirms.insert((2, ChatId(20)), std::time::Instant::now()); // fresh

    super::prune_expired_confirmations(&confirms);

    assert!(
        confirms.get(&(1, ChatId(10))).is_none(),
        "expired entry should be removed"
    );
    assert!(
        confirms.get(&(2, ChatId(20))).is_some(),
        "fresh entry should remain"
    );
}
