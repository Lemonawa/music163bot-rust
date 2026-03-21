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

#[tokio::test]
async fn get_program_main_track_returns_metadata() {
    let song_id = 3354598175;
    let server = MockMusicApiServer::start(song_id, HashMap::new()).await;
    let api = MusicApi::new(None, server.base_url());

    let program = api
        .get_program_main_track(3_714_760_479)
        .await
        .expect("program main track should be returned");

    assert_eq!(program.program_id, 3_714_760_479);
    assert_eq!(program.main_track_id, song_id);
    assert_eq!(program.program_name, "Mock Program 3714760479");
    assert_eq!(program.author_name, "Mock DJ");
    assert_eq!(program.radio_name, "Mock Radio");
    assert_eq!(
        program.cover_url.as_deref(),
        Some("https://mock.example/program-cover.jpg")
    );
}

#[tokio::test]
async fn get_djradio_program_main_tracks_returns_tracks_and_sets_referer() {
    let song_id = 3354598175;
    let server = MockMusicApiServer::start(song_id, HashMap::new()).await;
    let api = MusicApi::new(None, server.base_url());

    let (total, tracks) = api
        .get_djradio_program_main_tracks(985_936_420, 3)
        .await
        .expect("djradio program tracks should be returned");

    assert_eq!(total, 5);
    assert_eq!(tracks.len(), 3);
    assert_eq!(tracks[0].program_id, 3_714_760_479);
    assert_eq!(tracks[0].main_track_id, song_id);
    assert_eq!(tracks[1].main_track_id, song_id + 1);
    assert_eq!(tracks[2].main_track_id, song_id + 2);
    assert!(server.saw_byradio_referer());
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
use super::*;
