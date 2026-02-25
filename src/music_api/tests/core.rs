
    #[tokio::test]
    async fn get_playlist_song_ids_returns_track_ids() {
        let song_id = 2201;
        let server = MockMusicApiServer::start(song_id, HashMap::new()).await;
        let api = MusicApi::new(None, server.base_url());

        let song_ids = api
            .get_playlist_song_ids(17_607_381_913)
            .await
            .expect("playlist songs should be returned");

        assert_eq!(song_ids, vec![song_id]);
    }

    #[tokio::test]
    async fn get_album_song_ids_returns_track_ids() {
        let song_id = 2202;
        let server = MockMusicApiServer::start(song_id, HashMap::new()).await;
        let api = MusicApi::new(None, server.base_url());

        let song_ids = api
            .get_album_song_ids(121_344_602)
            .await
            .expect("album songs should be returned");

        assert_eq!(song_ids, vec![song_id]);
    }

    #[test]
    fn eapi_encrypt_rejects_invalid_key_length() {
        let result = MusicApi::eapi_encrypt_with_key("data", b"short");
        assert!(result.is_err());
    }

    #[test]
    fn eapi_encrypt_accepts_valid_key_length() {
        let result = MusicApi::eapi_encrypt_with_key("data", b"e82ckenh8dichen8");
        assert!(result.is_ok());
    }

    #[test]
    fn build_http_client_returns_client() {
        let client = build_http_client(reqwest::Client::builder()).expect("client should be built");
        let _ = client;
    }

    #[test]
    fn eapi_decrypt_rejects_invalid_key_length() {
        let result = MusicApi::eapi_decrypt_with_key("deadbeef", b"short");
        assert!(result.is_err());
    }

    #[test]
    fn eapi_encrypt_decrypt_round_trip() {
        let key = b"e82ckenh8dichen8";
        let plaintext = "roundtrip";
        let encrypted = MusicApi::eapi_encrypt_with_key(plaintext, key).expect("encrypted");
        let decrypted = MusicApi::eapi_decrypt_with_key(&encrypted, key).expect("decrypted");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn request_policy_rewrites_expected_hosts() {
        assert_eq!(
            super::rewrite_media_url("https://m8.music.126.net/song.mp3"),
            "https://m7.music.126.net/song.mp3"
        );
        assert_eq!(
            super::rewrite_media_url("https://m801.music.126.net/song.mp3"),
            "https://m701.music.126.net/song.mp3"
        );
        assert_eq!(
            super::rewrite_media_url("https://m804.music.126.net/song.mp3"),
            "https://m701.music.126.net/song.mp3"
        );
        assert_eq!(
            super::rewrite_media_url("https://m704.music.126.net/song.mp3"),
            "https://m701.music.126.net/song.mp3"
        );
    }

    #[test]
    fn request_policy_keeps_other_hosts_unchanged() {
        let url = "https://example.com/song.mp3";
        assert_eq!(super::rewrite_media_url(url), url);
    }
