fn sample_png_bytes() -> Vec<u8> {
    let mut image = image::RgbImage::new(2, 2);
    for pixel in image.pixels_mut() {
        *pixel = image::Rgb([200, 10, 10]);
    }

    let dynamic = image::DynamicImage::ImageRgb8(image);
    let mut png_bytes = Vec::new();
    dynamic
        .write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .expect("encode png");
    png_bytes
}

#[test]
fn thumbnail_transform_generates_jpeg_output() {
    let png_bytes = sample_png_bytes();

    let thumbnail = super::resize_album_art_to_thumbnail(&png_bytes).expect("thumbnail bytes");
    assert!(!thumbnail.is_empty());
    assert_eq!(thumbnail[0], 0xFF);
    assert_eq!(thumbnail[1], 0xD8);
}

#[test]
fn thumbnail_resize_is_320_square_jpeg() {
    let png_bytes = sample_png_bytes();

    let out = super::resize_album_art_to_thumbnail(&png_bytes).expect("thumbnail bytes");
    let img = image::load_from_memory(&out).expect("decode");
    assert_eq!(img.width(), 320);
    assert_eq!(img.height(), 320);
}

#[test]
fn song_url_cache_key_includes_bitrate() {
    let low = super::song_url_cache_key(42, 128_000);
    let high = super::song_url_cache_key(42, 320_000);
    assert_ne!(low, high);
}

#[test]
fn album_art_download_uses_five_attempt_budget() {
    assert_eq!(MusicApi::album_art_total_attempts(), 5);
}

#[tokio::test]
async fn run_with_attempts_and_overall_timeout_fails_when_attempts_outlast_overall() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    let attempts = std::sync::Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    let started = std::time::Instant::now();
    let result: std::result::Result<(), String> =
        crate::music_api::run_with_attempts_and_overall_timeout(
            5,
            Duration::from_millis(50),
            move |_attempt| {
                let counter = attempts_clone.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_mins(1)).await;
                    Err::<(), String>("never reached".to_string())
                }
            },
        )
        .await;
    let elapsed = started.elapsed();

    assert!(result.is_err(), "overall timeout should produce an error");
    let err = result.unwrap_err();
    assert!(
        err.contains("timed out") || err.contains("timeout"),
        "error should mention timeout, got: {err}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "helper must abort promptly via overall timeout, took {elapsed:?}"
    );
}

#[test]
fn cache_entry_expires_after_ttl() {
    let created_at = std::time::Instant::now();
    let ttl = Duration::from_secs(1);
    let before_expire = created_at + Duration::from_millis(900);
    let after_expire = created_at + Duration::from_secs(2);

    assert!(super::cache_entry_is_fresh(created_at, ttl, before_expire));
    assert!(!super::cache_entry_is_fresh(created_at, ttl, after_expire));
}

#[test]
fn song_url_deserializes_null_fields() {
    let payload = r#"{"id":1,"url":null,"br":320000,"size":123,"md5":null,"type":null}"#;
    let parsed: super::SongUrl = serde_json::from_str(payload).expect("deserialize song url");
    assert_eq!(parsed.url, "");
    assert_eq!(parsed.md5, "");
    assert_eq!(parsed.format, "");
}

#[test]
fn song_url_without_download_url_is_rejected() {
    let song_url = sample_song_url(42, 320_000, "");
    assert!(!song_url.has_download_url());
}

#[test]
fn song_detail_deserializes_null_name_fields() {
    let payload = r#"{"id":1,"name":null,"dt":240000,"ar":[{"id":2,"name":null}],"al":{"id":3,"name":null,"picUrl":null}}"#;
    let parsed: super::SongDetail = serde_json::from_str(payload).expect("deserialize song detail");
    assert_eq!(parsed.name, "");
    assert_eq!(
        parsed
            .ar
            .as_ref()
            .and_then(|artists| artists.first())
            .map(|artist| artist.name.as_str()),
        Some("")
    );
    assert_eq!(
        parsed.al.as_ref().map(|album| album.name.as_str()),
        Some("")
    );
}

#[test]
fn dashmap_cache_insert_and_retrieve() {
    let api = MusicApi::new(None, "http://localhost".to_string());

    let detail = super::SongDetail {
        id: 12345,
        name: "Test Song".to_string(),
        dt: Some(240_000),
        ar: Some(vec![super::Artist {
            id: 1,
            name: "Test Artist".to_string(),
        }]),
        al: Some(super::Album {
            id: 10,
            name: "Test Album".to_string(),
            pic_url: None,
        }),
    };

    api.cache_song_detail(12345, detail);

    let cached = api.get_cached_song_detail(12345);
    assert!(cached.is_some(), "cached entry should be present");
    let cached = cached.unwrap();
    assert_eq!(cached.id, 12345);
    assert_eq!(cached.name, "Test Song");
    assert_eq!(cached.dt, Some(240_000));

    let missing = api.get_cached_song_detail(99999);
    assert!(missing.is_none(), "non-existent key should return None");
}

#[test]
fn dashmap_cache_url_keyed_by_bitrate() {
    let api = MusicApi::new(None, "http://localhost".to_string());

    let url_low = super::SongUrl {
        id: 42,
        url: "https://example.com/low.mp3".to_string(),
        br: 128_000,
        size: 3_000_000,
        md5: "abc123".to_string(),
        format: "mp3".to_string(),
    };

    let url_high = super::SongUrl {
        id: 42,
        url: "https://example.com/high.flac".to_string(),
        br: 320_000,
        size: 10_000_000,
        md5: "def456".to_string(),
        format: "flac".to_string(),
    };

    api.cache_song_url(42, 128_000, url_low);
    api.cache_song_url(42, 320_000, url_high);

    let cached_low = api
        .get_cached_song_url(42, 128_000)
        .expect("low bitrate entry should be present");
    assert_eq!(cached_low.br, 128_000);
    assert_eq!(cached_low.url, "https://example.com/low.mp3");
    assert_eq!(cached_low.format, "mp3");

    let cached_high = api
        .get_cached_song_url(42, 320_000)
        .expect("high bitrate entry should be present");
    assert_eq!(cached_high.br, 320_000);
    assert_eq!(cached_high.url, "https://example.com/high.flac");
    assert_eq!(cached_high.format, "flac");

    let missing = api.get_cached_song_url(42, 192_000);
    assert!(missing.is_none(), "uncached bitrate should return None");
}

#[test]
fn prune_expired_cache_entries_removes_stale_entries_only() {
    let api = MusicApi::new(None, "http://localhost".to_string());
    let now = std::time::Instant::now();

    api.song_detail_cache.insert(
        1,
        super::TimedCacheEntry {
            value: Arc::new(sample_song_detail(1)),
            created_at: now
                .checked_sub(super::SONG_DETAIL_CACHE_TTL)
                .and_then(|t| t.checked_sub(Duration::from_secs(1)))
                .expect("instant subtraction should not underflow"),
            ttl: super::SONG_DETAIL_CACHE_TTL,
        },
    );
    api.song_detail_cache.insert(
        2,
        super::TimedCacheEntry {
            value: Arc::new(sample_song_detail(2)),
            created_at: now,
            ttl: super::SONG_DETAIL_CACHE_TTL,
        },
    );
    api.song_url_cache.insert(
        super::song_url_cache_key(1, 320_000),
        super::TimedCacheEntry {
            value: Arc::new(sample_song_url(1, 320_000, "https://stale.example/1.mp3")),
            created_at: now
                .checked_sub(super::SONG_URL_CACHE_TTL)
                .and_then(|t| t.checked_sub(Duration::from_secs(1)))
                .expect("instant subtraction should not underflow"),
            ttl: super::SONG_URL_CACHE_TTL,
        },
    );
    api.song_url_cache.insert(
        super::song_url_cache_key(2, 320_000),
        super::TimedCacheEntry {
            value: Arc::new(sample_song_url(2, 320_000, "https://fresh.example/2.mp3")),
            created_at: now,
            ttl: super::SONG_URL_CACHE_TTL,
        },
    );
    api.song_lyric_cache.insert(
        1,
        super::TimedCacheEntry {
            value: "stale lyric".to_string(),
            created_at: now
                .checked_sub(super::SONG_LYRIC_CACHE_TTL)
                .and_then(|t| t.checked_sub(Duration::from_secs(1)))
                .expect("instant subtraction should not underflow"),
            ttl: super::SONG_LYRIC_CACHE_TTL,
        },
    );
    api.song_lyric_cache.insert(
        2,
        super::TimedCacheEntry {
            value: "fresh lyric".to_string(),
            created_at: now,
            ttl: super::SONG_LYRIC_CACHE_TTL,
        },
    );

    let stats = api.prune_expired_cache_entries();

    assert_eq!(
        stats,
        super::CachePruneStats {
            song_detail_removed: 1,
            song_url_removed: 1,
            song_lyric_removed: 1,
        }
    );
    assert_eq!(stats.total_removed(), 3);
    assert!(api.song_detail_cache.get(&1).is_none());
    assert!(
        api.song_url_cache
            .get(&super::song_url_cache_key(1, 320_000))
            .is_none()
    );
    assert!(api.song_lyric_cache.get(&1).is_none());
    assert!(api.song_detail_cache.get(&2).is_some());
    assert!(
        api.song_url_cache
            .get(&super::song_url_cache_key(2, 320_000))
            .is_some()
    );
    assert!(api.song_lyric_cache.get(&2).is_some());
}

#[test]
fn song_detail_cache_capped_at_configured_max_entries() {
    let api = MusicApi::new(None, "http://localhost".to_string());
    let cap = super::MUSIC_API_CACHE_MAX_ENTRIES;

    for id in 0..(cap as u64 + 50) {
        let detail = super::SongDetail {
            id,
            name: format!("Song {id}"),
            dt: None,
            ar: None,
            al: None,
        };
        api.cache_song_detail(id, detail);
    }

    assert!(
        api.song_detail_cache.len() <= cap,
        "song_detail_cache should be capped at {cap}, got {}",
        api.song_detail_cache.len()
    );
}

#[test]
fn song_url_cache_capped_at_configured_max_entries() {
    let api = MusicApi::new(None, "http://localhost".to_string());
    let cap = super::MUSIC_API_CACHE_MAX_ENTRIES;

    for id in 0..(cap as u64 + 50) {
        let url = super::SongUrl {
            id,
            url: format!("https://example.com/{id}.mp3"),
            br: 320_000,
            size: 3_000_000,
            md5: "x".to_string(),
            format: "mp3".to_string(),
        };
        api.cache_song_url(id, 320_000, url);
    }

    assert!(
        api.song_url_cache.len() <= cap,
        "song_url_cache should be capped at {cap}, got {}",
        api.song_url_cache.len()
    );
}

#[test]
fn song_lyric_cache_capped_at_configured_max_entries() {
    let api = MusicApi::new(None, "http://localhost".to_string());
    let cap = super::MUSIC_API_CACHE_MAX_ENTRIES;

    for id in 0..(cap as u64 + 50) {
        api.cache_song_lyric(id, format!("lyric-{id}"));
    }

    assert!(
        api.song_lyric_cache.len() <= cap,
        "song_lyric_cache should be capped at {cap}, got {}",
        api.song_lyric_cache.len()
    );
}

use super::*;
