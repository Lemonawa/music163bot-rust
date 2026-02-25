
    #[test]
    fn eapi_cookie_device_id_is_stable_per_instance() {
        let api = MusicApi::new(None, "http://localhost".to_string());
        let cookie1 = api.build_eapi_cookie();
        let cookie2 = api.build_eapi_cookie();

        // Extract deviceId from cookie string
        // Format: deviceId=...; appver=...
        let get_device_id = |c: &str| -> String {
            c.split("; ")
                .find(|p| p.starts_with("deviceId="))
                .expect("cookie should have deviceId")
                .to_string()
        };

        assert_eq!(
            get_device_id(&cookie1),
            get_device_id(&cookie2),
            "device_id should be stable"
        );
    }

    #[test]
    fn eapi_cookie_includes_music_u() {
        let music_u = "test_cookie_value";
        let api = MusicApi::new(Some(music_u.to_string()), "http://localhost".to_string());
        let cookie = api.build_eapi_cookie();

        assert!(cookie.contains(&format!("MUSIC_U={music_u}")));
    }

    // --- B.2: music_u_cookie helper tests ---

    #[test]
    fn music_u_cookie_precomputed_with_value() {
        let api = MusicApi::new(Some("abc123".to_string()), "http://localhost".to_string());
        assert_eq!(api.music_u_cookie.as_deref(), Some("MUSIC_U=abc123"));
    }

    #[test]
    fn music_u_cookie_none_without_value() {
        let api = MusicApi::new(None, "http://localhost".to_string());
        assert!(api.music_u_cookie.is_none());
    }

    // --- B.3: rewrite_media_url Cow tests ---

    #[test]
    fn rewrite_media_url_returns_borrowed_when_unchanged() {
        let url = "https://example.com/song.mp3";
        let result = super::rewrite_media_url(url);
        assert!(
            matches!(result, std::borrow::Cow::Borrowed(_)),
            "should return Cow::Borrowed for non-matching URL"
        );
        assert_eq!(result, url);
    }

    #[test]
    fn rewrite_media_url_returns_owned_when_changed() {
        let url = "https://m8.music.126.net/song.mp3";
        let result = super::rewrite_media_url(url);
        assert!(
            matches!(result, std::borrow::Cow::Owned(_)),
            "should return Cow::Owned for matching URL"
        );
        assert_eq!(result, "https://m7.music.126.net/song.mp3");
    }

    #[test]
    fn rewrite_media_url_handles_http_scheme() {
        assert_eq!(
            super::rewrite_media_url("http://m8.music.126.net/song.mp3"),
            "http://m7.music.126.net/song.mp3"
        );
        assert_eq!(
            super::rewrite_media_url("http://m801.music.126.net/song.mp3"),
            "http://m701.music.126.net/song.mp3"
        );
        assert_eq!(
            super::rewrite_media_url("http://m804.music.126.net/song.mp3"),
            "http://m701.music.126.net/song.mp3"
        );
        assert_eq!(
            super::rewrite_media_url("http://m704.music.126.net/song.mp3"),
            "http://m701.music.126.net/song.mp3"
        );
    }

    #[test]
    fn rewrite_media_url_empty_input() {
        let result = super::rewrite_media_url("");
        assert_eq!(result, "");
        assert!(matches!(result, std::borrow::Cow::Borrowed(_)));
    }

    // --- B.3: format_artists tests ---

    #[test]
    fn format_artists_empty() {
        assert_eq!(super::format_artists(&[]), "");
    }

    #[test]
    fn format_artists_single() {
        let artists = vec![super::Artist {
            id: 1,
            name: "Alice".to_string(),
        }];
        assert_eq!(super::format_artists(&artists), "Alice");
    }

    #[test]
    fn format_artists_multiple() {
        let artists = vec![
            super::Artist {
                id: 1,
                name: "Alice".to_string(),
            },
            super::Artist {
                id: 2,
                name: "Bob".to_string(),
            },
            super::Artist {
                id: 3,
                name: "Charlie".to_string(),
            },
        ];
        assert_eq!(super::format_artists(&artists), "Alice/Bob/Charlie");
    }

    #[test]
    fn format_artists_unicode_names() {
        let artists = vec![
            super::Artist {
                id: 1,
                name: "周杰伦".to_string(),
            },
            super::Artist {
                id: 2,
                name: "林俊杰".to_string(),
            },
        ];
        assert_eq!(super::format_artists(&artists), "周杰伦/林俊杰");
    }

    #[test]
    fn resize_image_with_padding_zero_target_returns_original() {
        let img = DynamicImage::new_rgb8(100, 100);
        let result = super::resize_image_with_padding(img.clone(), 0, 100);
        assert_eq!(result.dimensions(), (100, 100));
        let result = super::resize_image_with_padding(img, 100, 0);
        assert_eq!(result.dimensions(), (100, 100));
    }

    #[test]
    fn resize_image_with_padding_zero_source_returns_blank_canvas() {
        let img = DynamicImage::new_rgb8(0, 0);
        let result = super::resize_image_with_padding(img, 320, 320);
        assert_eq!(result.dimensions(), (320, 320));
    }
