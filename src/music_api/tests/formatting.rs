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

    let id1 = get_device_id(cookie1);
    let id2 = get_device_id(cookie2);
    assert_eq!(id1, id2, "device_id should be stable");
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
use super::*;

// --- deserialize_string_or_null edge-case tests ---

#[test]
fn song_detail_deserializes_missing_name_field_to_default() {
    // The `name` field has #[serde(default)] + deserialize_string_or_null.
    // A missing key should yield "".
    let payload = r#"{"id":1}"#;
    let parsed: SongDetail = serde_json::from_str(payload).expect("deserialize song detail");
    assert_eq!(parsed.name, "");
    assert!(parsed.ar.is_none());
    assert!(parsed.al.is_none());
}

#[test]
fn song_detail_deserializes_empty_string_name() {
    let payload = r#"{"id":1,"name":""}"#;
    let parsed: SongDetail = serde_json::from_str(payload).expect("deserialize song detail");
    assert_eq!(parsed.name, "");
}

#[test]
fn song_detail_deserializes_normal_name() {
    let payload = r#"{"id":1,"name":"Hello"}"#;
    let parsed: SongDetail = serde_json::from_str(payload).expect("deserialize song detail");
    assert_eq!(parsed.name, "Hello");
}

#[test]
fn artist_deserializes_null_name() {
    let payload = r#"{"id":1,"name":null}"#;
    let parsed: Artist = serde_json::from_str(payload).expect("deserialize artist");
    assert_eq!(parsed.name, "");
}

#[test]
fn artist_deserializes_missing_name_to_default() {
    let payload = r#"{"id":1}"#;
    let parsed: Artist = serde_json::from_str(payload).expect("deserialize artist");
    assert_eq!(parsed.name, "");
}

#[test]
fn album_deserializes_null_name() {
    let payload = r#"{"id":1,"name":null}"#;
    let parsed: Album = serde_json::from_str(payload).expect("deserialize album");
    assert_eq!(parsed.name, "");
}

#[test]
fn album_deserializes_missing_name_to_default() {
    let payload = r#"{"id":1}"#;
    let parsed: Album = serde_json::from_str(payload).expect("deserialize album");
    assert_eq!(parsed.name, "");
}

#[test]
fn song_url_deserializes_missing_optional_fields_to_default() {
    // url, md5, format all have #[serde(default)] + deserialize_string_or_null.
    let payload = r#"{"id":1,"br":320000,"size":123}"#;
    let parsed: SongUrl = serde_json::from_str(payload).expect("deserialize song url");
    assert_eq!(parsed.url, "");
    assert_eq!(parsed.md5, "");
    assert_eq!(parsed.format, "");
}

#[test]
fn song_url_deserializes_normal_values() {
    let payload = r#"{"id":1,"url":"https://example.com/song.mp3","br":320000,"size":12345,"md5":"abc123","type":"mp3"}"#;
    let parsed: SongUrl = serde_json::from_str(payload).expect("deserialize song url");
    assert_eq!(parsed.url, "https://example.com/song.mp3");
    assert_eq!(parsed.md5, "abc123");
    assert_eq!(parsed.format, "mp3");
}

#[test]
fn song_detail_accepts_duration_alias() {
    // `dt` field has #[serde(alias = "duration")]
    let payload = r#"{"id":1,"name":"test","duration":180000}"#;
    let parsed: SongDetail = serde_json::from_str(payload).expect("deserialize song detail");
    assert_eq!(parsed.dt, Some(180_000));
}

#[test]
fn song_detail_accepts_artists_alias() {
    // `ar` field has #[serde(alias = "artists")]
    let payload = r#"{"id":1,"name":"test","artists":[{"id":2,"name":"Bob"}]}"#;
    let parsed: SongDetail = serde_json::from_str(payload).expect("deserialize song detail");
    let artists = parsed.ar.expect("artists should be present");
    assert_eq!(artists.len(), 1);
    assert_eq!(artists[0].name, "Bob");
}

#[test]
fn song_detail_accepts_album_alias() {
    // `al` field has #[serde(alias = "album")]
    let payload = r#"{"id":1,"name":"test","album":{"id":3,"name":"Test Album"}}"#;
    let parsed: SongDetail = serde_json::from_str(payload).expect("deserialize song detail");
    let album = parsed.al.expect("album should be present");
    assert_eq!(album.name, "Test Album");
}
