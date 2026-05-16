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
use super::*;
