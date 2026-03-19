
    #[test]
    fn redact_bot_token_in_error_message_masks_bot_path_segment() {
        let raw = "error sending request for url (http://127.0.0.1:8081/bot123456789:fake_test_token/sendAudio)";

        let redacted = super::redact_bot_token_in_error_message(raw);

        assert!(!redacted.contains("123456789:fake_test_token"));
        assert!(redacted.contains("/bot<redacted>/sendAudio"));
    }

    #[test]
    fn parse_telegram_api_response_returns_error_when_http_200_ok_false() {
        let body = r#"{"ok": false, "description": "chat not found"}"#;
        let err = super::parse_telegram_api_response(body, reqwest::StatusCode::OK, "sendAudio")
            .expect_err("ok=false should be treated as Telegram API error");
        let err_msg = err.to_string();

        assert!(err_msg.contains("chat not found"));
        assert!(err_msg.contains("HTTP 200"));
    }

    #[test]
    fn parse_telegram_api_response_returns_error_when_http_500_for_any_ok_flag() {
        let cases = [
            (r#"{"ok": true, "result": {}}"#, "unknown error"),
            (
                r#"{"ok": false, "description": "server failed"}"#,
                "server failed",
            ),
        ];

        for (body, expected_desc) in cases {
            let err = super::parse_telegram_api_response(
                body,
                reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                "sendAudio",
            )
            .expect_err("non-2xx status should always be treated as error");
            let err_msg = err.to_string();

            assert!(err_msg.contains(expected_desc));
            assert!(err_msg.contains("HTTP 500"));
        }
    }

    #[test]
    fn parse_telegram_api_response_returns_parse_error_for_non_json_body() {
        let err = super::parse_telegram_api_response(
            "<html>502 bad gateway</html>",
            reqwest::StatusCode::OK,
            "sendAudio",
        )
        .expect_err("non-JSON body should fail response parsing");
        let err_msg = err.to_string();

        assert!(err_msg.contains("Failed to parse upload response"));
    }

    #[test]
    fn parse_telegram_api_response_uses_unknown_error_when_description_missing() {
        let err = super::parse_telegram_api_response(
            r#"{"ok": false}"#,
            reqwest::StatusCode::OK,
            "sendAudio",
        )
        .expect_err("missing description should still return a Telegram API error");
        let err_msg = err.to_string();

        assert!(err_msg.contains("unknown error"));
        assert!(err_msg.contains("HTTP 200"));
    }

    #[test]
    fn parse_telegram_api_response_redacts_sensitive_description_text() {
        let body = r#"{"ok": false, "description": "proxy said http://127.0.0.1:8081/bot123456789:fake_test_token/sendAudio failed"}"#;
        let err = super::parse_telegram_api_response(body, reqwest::StatusCode::BAD_GATEWAY, "sendAudio")
            .expect_err("sensitive description should still return an error");
        let err_msg = err.to_string();

        assert!(!err_msg.contains("123456789:fake_test_token"));
        assert!(err_msg.contains("/bot<redacted>/sendAudio"));
    }

    #[test]
    fn extract_file_id_reads_audio_field() {
        let payload = serde_json::json!({
            "ok": true,
            "result": {
                "audio": {
                    "file_id": "audio_file_123"
                }
            }
        });

        assert_eq!(
            super::extract_file_id_from_response(&payload),
            Some("audio_file_123".to_string())
        );
    }

    #[test]
    fn inflight_registry_first_is_leader() {
        let inflight = Arc::new(super::InflightDownloads::default());
        let claim = inflight.begin(42);
        assert!(matches!(claim, super::InflightClaim::Leader(_)));
    }

    #[tokio::test]
    async fn inflight_registry_second_waits() {
        let inflight = Arc::new(super::InflightDownloads::default());
        let leader = match inflight.begin(99) {
            super::InflightClaim::Leader(guard) => guard,
            super::InflightClaim::Follower(_) => panic!("first claim should be leader"),
        };

        let follower_entry = match inflight.begin(99) {
            super::InflightClaim::Leader(_) => panic!("second claim should be follower"),
            super::InflightClaim::Follower(entry) => entry,
        };

        let pending = tokio::time::timeout(Duration::from_millis(20), follower_entry.wait()).await;
        assert!(pending.is_err(), "follower should wait while leader active");

        drop(leader);

        tokio::time::timeout(Duration::from_secs(1), follower_entry.wait())
            .await
            .expect("follower should be released after leader finishes");
    }

    #[tokio::test]
    async fn singleflight_claim_helper_waits_for_existing_leader() {
        let inflight = Arc::new(super::InflightDownloads::default());
        let leader = super::acquire_download_leader(&inflight, 7)
            .await
            .expect("first claim should be leader");

        let inflight_for_follower = Arc::clone(&inflight);
        let follower = tokio::spawn(async move {
            super::acquire_download_leader(&inflight_for_follower, 7)
                .await
                .is_none()
        });

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!follower.is_finished(), "follower should still be waiting");

        drop(leader);

        let waited = tokio::time::timeout(Duration::from_secs(1), follower)
            .await
            .expect("follower task should complete")
            .expect("follower task join should succeed");
        assert!(waited, "follower claim should resolve as waiting follower");
    }

    #[tokio::test]
    async fn tagging_wrapper_returns_buffer_for_unknown_format() {
        let buffer = crate::audio_buffer::AudioBuffer::Memory {
            data: vec![1, 2, 3],
            filename: "sample.bin".to_string(),
        };
        let detail = crate::music_api::SongDetail {
            id: 1,
            name: "Song".to_string(),
            dt: Some(1_000),
            ar: Some(vec![]),
            al: None,
        };

        let tagged = super::apply_tags_in_blocking(buffer, "bin", Arc::new(detail), None, false)
            .await
            .expect("unknown format should keep buffer unchanged");

        assert_eq!(tagged.size().await, 3);
    }

    #[tokio::test]
    async fn tagging_wrapper_adds_mp3_id3_header() {
        let buffer = crate::audio_buffer::AudioBuffer::Memory {
            data: vec![0xFF, 0xFB, 0x90, 0x64],
            filename: "sample.mp3".to_string(),
        };
        let detail = crate::music_api::SongDetail {
            id: 2,
            name: "Song".to_string(),
            dt: Some(120_000),
            ar: Some(vec![crate::music_api::Artist {
                id: 1,
                name: "Artist".to_string(),
            }]),
            al: Some(crate::music_api::Album {
                id: 1,
                name: "Album".to_string(),
                pic_url: None,
            }),
        };

        let tagged = super::apply_tags_in_blocking(buffer, "mp3", Arc::new(detail), None, false)
            .await
            .expect("mp3 tagging should succeed");
        let data = tagged.get_data().await.expect("read tagged data");
        assert!(data.starts_with(b"ID3"));
    }
