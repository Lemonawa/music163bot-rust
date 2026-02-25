
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
        let text = super::build_status_text(
            100,
            20,
            8,
            cache_snapshot,
            resource_snapshot,
            "00:10:00",
            "下载: 实时 <code>6.00</code> MB/s | 平均 <code>4.00</code> MB/s | P95 <code>5.00</code> MB/s | 样本 <code>12</code> (窗口 <code>12</code>)",
            "上传: 实时 <code>2.00</code> MB/s | 平均 <code>1.50</code> MB/s | P95 <code>1.80</code> MB/s | 样本 <code>12</code> (窗口 <code>12</code>)",
        );
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
