use super::*;

#[tokio::test]
async fn get_song_url_uses_eapi_channel_not_legacy_web_post() {
    let song_id = 9001;
    let server = MockMusicApiServer::start(
        song_id,
        HashMap::from([(
            320_000,
            vec![MockSongUrlReply::OkWithUrl(
                "https://mock.example/eapi-320.mp3",
            )],
        )]),
    )
    .await;
    let api = MusicApi::new(None, server.base_url());
    api.cache_song_detail(song_id, sample_song_detail(song_id));

    let (_, song_url) = api
        .get_song_detail_and_best_url(song_id, &[320_000, 128_000])
        .await
        .expect("eapi song/url should succeed");

    assert_eq!(song_url.url, "https://mock.example/eapi-320.mp3");
    assert_eq!(
        server.eapi_calls_for_bitrate(320_000),
        1,
        "song url request must reach the eapi /eapi/song/enhance/player/url/v1 path"
    );
    assert_eq!(
        server.legacy_calls_for_bitrate(320_000),
        0,
        "legacy /api/song/enhance/player/url path must not be used (it is rate-limited by upstream)"
    );
}

/// Regression: the highest candidate must map to the eapi `hires` level so the
/// bot can pull 24-bit Hi-Res FLAC when the account/song permits, rather than
/// capping at 16-bit `lossless`. The previous candidate ceiling of 999_000 only
/// ever produced `lossless`.
#[tokio::test]
async fn hires_candidate_requests_hires_level_and_is_preferred() {
    let song_id = 7_777;
    let server = MockMusicApiServer::start(
        song_id,
        HashMap::from([
            (
                1_999_000,
                vec![MockSongUrlReply::OkWithUrl(
                    "https://mock.example/hires-24bit.flac",
                )],
            ),
            (
                999_000,
                vec![MockSongUrlReply::OkWithUrl(
                    "https://mock.example/lossless.flac",
                )],
            ),
        ]),
    )
    .await;
    let api = MusicApi::new(None, server.base_url());
    api.cache_song_detail(song_id, sample_song_detail(song_id));

    let (_, song_url) = api
        .get_song_detail_and_best_url(song_id, &[1_999_000, 999_000, 320_000, 128_000])
        .await
        .expect("hires song/url should succeed");

    assert_eq!(song_url.url, "https://mock.example/hires-24bit.flac");
    assert_eq!(
        server.eapi_calls_for_bitrate(1_999_000),
        1,
        "the hires candidate must be requested via the eapi path"
    );
    assert_eq!(
        server.eapi_calls_for_bitrate(999_000),
        0,
        "lossless must not be requested when hires already yields a download URL"
    );
}

/// When the server cannot serve the requested hires tier (empty URL), the
/// selector must fall back to lossless rather than failing or silently capping.
#[tokio::test]
async fn hires_candidate_falls_back_to_lossless_when_unavailable() {
    let song_id = 8_888;
    let server = MockMusicApiServer::start(
        song_id,
        HashMap::from([
            (1_999_000, vec![MockSongUrlReply::OkEmptyUrl]),
            (
                999_000,
                vec![MockSongUrlReply::OkWithUrl(
                    "https://mock.example/lossless.flac",
                )],
            ),
        ]),
    )
    .await;
    let api = MusicApi::new(None, server.base_url());
    api.cache_song_detail(song_id, sample_song_detail(song_id));

    let (_, song_url) = api
        .get_song_detail_and_best_url(song_id, &[1_999_000, 999_000, 320_000, 128_000])
        .await
        .expect("lossless fallback should succeed");

    assert_eq!(song_url.url, "https://mock.example/lossless.flac");
    assert_eq!(server.eapi_calls_for_bitrate(1_999_000), 1);
    assert_eq!(server.eapi_calls_for_bitrate(999_000), 1);
}
