#[test]
fn append_search_result_line_formats_output() {
    let mut results = String::new();
    append_search_result_line(&mut results, 1, "Song", "Artist");
    assert_eq!(results, "1.「Song」 - Artist\n");
}

#[test]
fn perf_log_includes_stage_label() {
    let s = format_perf("download", std::time::Duration::from_millis(50));
    assert!(s.contains("download"));
}

#[test]
fn structured_perf_line_contains_required_fields() {
    let line = super::format_perf_stage_line(
        "trace-123",
        42,
        "official_api",
        "miss_cold",
        "e2e_total",
        std::time::Duration::from_millis(88),
    );
    assert!(line.starts_with("PERF|"));
    assert!(line.contains("trace_id=trace-123"));
    assert!(line.contains("music_id=42"));
    assert!(line.contains("topology=official_api"));
    assert!(line.contains("cache_path=miss_cold"));
    assert!(line.contains("stage=e2e_total"));
    assert!(line.contains("elapsed_ms=88"));
}

#[test]
fn upload_topology_label_matches_mode() {
    let mut config = Config::default();
    config.upload_local_file_uri = false;
    assert_eq!(super::upload_topology_label(&config, true), "official_api");

    config.upload_local_file_uri = true;
    assert_eq!(
        super::upload_topology_label(&config, false),
        "selfhost_api_uri_upload"
    );

    config.upload_local_file_uri = false;
    assert_eq!(
        super::upload_topology_label(&config, false),
        "selfhost_api_multipart_upload"
    );
}

#[test]
fn critical_path_stage_labels_are_stable() {
    assert_eq!(
        critical_path_stage_labels(),
        ["select_url", "pre_upload_path"]
    );
}

#[test]
fn url_bitrate_candidates_prefers_flac_with_music_u() {
    assert_eq!(
        super::url_bitrate_candidates(true),
        &[999_000, 320_000, 128_000]
    );
}

#[test]
fn url_bitrate_candidates_uses_mp3_without_music_u() {
    assert_eq!(super::url_bitrate_candidates(false), &[320_000, 128_000]);
}

#[test]
fn spawn_gate_identifies_supported_messages() {
    assert!(super::should_spawn_message_task("/start"));
    assert!(
        !super::should_spawn_message_task("   /start"),
        "leading whitespace should not be treated as a command"
    );
    assert!(super::should_spawn_message_task(
        "https://music.163.com/song?id=1"
    ));
    assert!(super::should_spawn_message_task("https://163cn.tv/abcd"));
    assert!(super::should_spawn_message_task("https://163cn.link/abcd"));
    assert!(!super::should_spawn_message_task("hello world"));
}

#[test]
fn spawn_gate_calculates_reasonable_limit() {
    assert_eq!(super::message_task_limit(0), 8);
    assert_eq!(super::message_task_limit(1), 8);
    assert_eq!(super::message_task_limit(3), 12);
    assert_eq!(super::message_task_limit(200), 256);
}

#[test]
fn batch_download_limit_rejects_only_when_over_limit() {
    assert!(!super::exceeds_batch_download_limit(20, 20));
    assert!(super::exceeds_batch_download_limit(21, 20));
}

#[test]
fn maintenance_scheduler_emits_expected_signals() {
    let counters = super::MaintenanceCounters::new();
    let mut config = crate::config::Config::default();
    config.db_analyze_interval_requests = 2;
    config.memory_release_interval_requests = 3;

    assert!(super::collect_maintenance_signals(&counters, &config).is_empty());

    let second = super::collect_maintenance_signals(&counters, &config);
    assert_eq!(second, vec![super::MaintenanceSignal::AnalyzeDb]);

    let third = super::collect_maintenance_signals(&counters, &config);
    assert_eq!(third, vec![super::MaintenanceSignal::ReleaseMemory]);
}

#[test]
fn maintenance_scheduler_emits_cache_prune_signal_on_interval() {
    let counters = super::MaintenanceCounters::new();
    let mut config = crate::config::Config::default();
    config.db_analyze_interval_requests = 0;
    config.memory_release_interval_requests = 0;

    for _ in 0..super::CACHE_PRUNE_INTERVAL_REQUESTS.saturating_sub(1) {
        let _ = super::collect_maintenance_signals(&counters, &config);
    }

    let signals = super::collect_maintenance_signals(&counters, &config);
    assert_eq!(signals, vec![super::MaintenanceSignal::PruneApiCache]);
}

#[test]
fn upload_pool_idle_timeout_disabled_when_zero() {
    assert!(!super::should_set_upload_pool_idle_timeout(0));
    assert!(super::should_set_upload_pool_idle_timeout(60));
}

#[test]
fn download_chunk_bytes_uses_configured_kib() {
    let mut config = Config::default();
    config.download_chunk_size_kb = 512;

    assert_eq!(super::download_chunk_bytes(&config), 512 * 1024);
}

#[test]
fn download_chunk_bytes_clamps_zero_to_minimum() {
    let mut config = Config::default();
    config.download_chunk_size_kb = 0;

    assert_eq!(
        super::download_chunk_bytes(&config),
        MIN_DOWNLOAD_CHUNK_BYTES
    );
}

#[test]
fn upload_limit_clamps_bounds() {
    assert_eq!(super::upload_task_limit(0), 1);
    assert_eq!(super::upload_task_limit(1), 1);
    assert_eq!(super::upload_task_limit(4), 4);
    assert_eq!(super::upload_task_limit(1000), 64);
}

#[test]
fn collection_retry_delay_seconds_retries_first_rate_limit_once() {
    let err = crate::error::BotError::Other(anyhow::anyhow!("Retry after 26s"));

    assert_eq!(super::collection_retry_delay_seconds(&err, 0), Some(27));
    assert_eq!(super::collection_retry_delay_seconds(&err, 1), None);
}

#[test]
fn collection_retry_delay_seconds_ignores_non_rate_limit_errors() {
    let err = crate::error::BotError::Other(anyhow::anyhow!("ordinary upload failure"));

    assert_eq!(super::collection_retry_delay_seconds(&err, 0), None);
}

#[test]
fn upload_client_refresh_decision_works() {
    let has_bot = UploadClientState {
        bot: Some(Bot::new("token")),
        raw_client: None,
        upload_api_url: String::new(),
        reuse_count: 0,
    };
    let no_bot = UploadClientState {
        bot: None,
        raw_client: None,
        upload_api_url: String::new(),
        reuse_count: 0,
    };
    let exhausted = UploadClientState {
        bot: Some(Bot::new("token")),
        raw_client: None,
        upload_api_url: String::new(),
        reuse_count: 10,
    };

    assert!(super::should_refresh_upload_client(&no_bot, 10));
    assert!(!super::should_refresh_upload_client(&has_bot, 10));
    assert!(super::should_refresh_upload_client(&exhausted, 10));
    assert!(!super::should_refresh_upload_client(&exhausted, 0));
}

#[tokio::test]
async fn upload_prewarm_failure_is_non_fatal() {
    let ok = super::run_upload_prewarm(|| async {
        Err::<(), crate::error::BotError>(crate::error::BotError::MusicApi(
            "simulated prewarm failure".to_string(),
        ))
    })
    .await;

    assert!(!ok);
}

#[tokio::test]
async fn upload_prewarm_runs_warmup_path() {
    let ok = super::run_upload_prewarm(|| async { Ok::<(), crate::error::BotError>(()) }).await;

    assert!(ok);
}
use super::*;
use crate::bot::wiring::MIN_DOWNLOAD_CHUNK_BYTES;
