
    #[tokio::test]
    async fn get_song_detail_and_best_url_returns_cached_detail_and_cached_fallback_without_network()
     {
        let song_id = 1001;
        let api = MusicApi::new(None, "http://127.0.0.1:0".to_string());
        api.cache_song_detail(song_id, sample_song_detail(song_id));
        api.cache_song_url(
            song_id,
            192_000,
            sample_song_url(song_id, 192_000, "https://cache.example/fallback-192.mp3"),
        );

        let (detail, song_url) = api
            .get_song_detail_and_best_url(song_id, &[320_000, 192_000, 128_000])
            .await
            .expect("cached detail + cached fallback URL should return immediately");

        assert_eq!(detail.id, song_id);
        assert_eq!(song_url.br, 192_000);
        assert_eq!(song_url.url, "https://cache.example/fallback-192.mp3");
    }

    #[tokio::test]
    async fn get_song_detail_and_best_url_falls_back_when_primary_returns_empty_url() {
        let song_id = 1002;
        let server = MockMusicApiServer::start(
            song_id,
            HashMap::from([
                (320_000, vec![MockSongUrlReply::OkEmptyUrl]),
                (
                    192_000,
                    vec![MockSongUrlReply::OkWithUrl(
                        "https://mock.example/fallback-192.mp3",
                    )],
                ),
            ]),
        )
        .await;
        let api = MusicApi::new(None, server.base_url());
        api.cache_song_detail(song_id, sample_song_detail(song_id));

        let (_, song_url) = api
            .get_song_detail_and_best_url(song_id, &[320_000, 192_000])
            .await
            .expect("fallback bitrate should succeed when primary URL is empty");

        assert_eq!(song_url.br, 192_000);
        assert_eq!(song_url.url, "https://mock.example/fallback-192.mp3");
        assert_eq!(server.calls_for_bitrate(320_000), 1);
        assert_eq!(server.calls_for_bitrate(192_000), 1);
    }

    #[tokio::test]
    async fn get_song_detail_and_best_url_returns_last_error_when_all_fallbacks_fail() {
        let song_id = 1003;
        let server = MockMusicApiServer::start(
            song_id,
            HashMap::from([
                (192_000, vec![MockSongUrlReply::ApiCode(500)]),
                (128_000, vec![MockSongUrlReply::ApiCode(404)]),
            ]),
        )
        .await;
        let api = MusicApi::new(None, server.base_url());
        api.cache_song_detail(song_id, sample_song_detail(song_id));
        api.cache_song_url(song_id, 320_000, sample_song_url(song_id, 320_000, ""));

        let error = api
            .get_song_detail_and_best_url(song_id, &[320_000, 192_000, 128_000])
            .await
            .expect_err("all fallback attempts should fail");

        match error {
            BotError::MusicApi(message) => {
                assert!(
                    message.contains("404"),
                    "expected last fallback error (code 404), got: {message}"
                );
            }
            other => panic!("expected BotError::MusicApi, got: {other:?}"),
        }
        assert_eq!(server.calls_for_bitrate(320_000), 0);
        assert_eq!(server.calls_for_bitrate(192_000), 1);
        assert_eq!(server.calls_for_bitrate(128_000), 1);
    }

    #[tokio::test]
    async fn get_song_detail_and_best_url_returns_error_when_primary_unavailable_and_fallback_fails()
     {
        let song_id = 1004;
        let server = MockMusicApiServer::start(
            song_id,
            HashMap::from([
                (320_000, vec![MockSongUrlReply::OkEmptyUrl]),
                (192_000, vec![MockSongUrlReply::ApiCode(503)]),
            ]),
        )
        .await;
        let api = MusicApi::new(None, server.base_url());
        api.cache_song_detail(song_id, sample_song_detail(song_id));

        let result = api
            .get_song_detail_and_best_url(song_id, &[320_000, 192_000])
            .await;

        assert!(
            result.is_err(),
            "should return error when primary unavailable and fallback also fails"
        );
        assert_eq!(server.calls_for_bitrate(320_000), 1);
        assert_eq!(server.calls_for_bitrate(192_000), 1);
    }

    #[tokio::test]
    async fn get_song_detail_and_best_url_accepts_fallback_without_retrying_primary_for_downgraded_bitrate()
     {
        let song_id = 1005;
        let server = MockMusicApiServer::start(
            song_id,
            HashMap::from([
                (999_000, vec![MockSongUrlReply::OkEmptyUrl]),
                (
                    320_000,
                    vec![MockSongUrlReply::OkWithUrl(
                        "https://mock.example/fallback-320.mp3",
                    )],
                ),
            ]),
        )
        .await;
        let mut config = Config::default();
        config.music_api = server.base_url();
        config.music_u = Some("cookie".to_string());
        config.auto_retry = true;
        config.max_retry_times = 1;
        let api = MusicApi::new_with_config(&config);
        api.cache_song_detail(song_id, sample_song_detail(song_id));

        let (_, song_url) = api
            .get_song_detail_and_best_url(song_id, &[999_000, 320_000, 128_000])
            .await
            .expect("should return fallback bitrate without retrying primary");

        assert_eq!(song_url.br, 320_000);
        assert_eq!(song_url.url, "https://mock.example/fallback-320.mp3");
        // primary should only be tried once (no retry)
        assert_eq!(server.calls_for_bitrate(999_000), 1);
        assert_eq!(server.calls_for_bitrate(320_000), 1);
    }
