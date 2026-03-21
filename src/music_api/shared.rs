use super::SongUrl;

pub(super) fn song_url_has_download_url(song_url: &SongUrl) -> bool {
    !song_url.url.is_empty()
}
