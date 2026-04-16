use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use reqwest::Client;
use tokio::time::Duration;

use crate::error::Result;

mod cache_and_crypto;
mod media;
mod models;
mod requests;
mod shared;

pub use self::media::{format_artists, resize_album_art_to_thumbnail};
pub use self::models::{
    Album, Artist, LyricContent, LyricResponse, ProgramMainTrack, SearchResponse, SearchResult,
    SearchSong, SongDetail, SongDetailResponse, SongUrl, SongUrlResponse,
};

#[cfg(test)]
use self::media::resize_image_with_padding;
use self::media::rewrite_media_url;
use self::models::{
    AlbumSongsResponse, DjProgramDetailResponse, DjProgramItem, DjProgramListResponse,
    PlaylistDetailResponse,
};

#[derive(Debug)]
pub struct MusicApi {
    client: Client,
    resolve_client: Client,
    pub music_u: Option<String>,
    base_url: String,
    eapi_cookie: String,
    music_u_cookie: Option<String>,
    song_detail_cache: DashMap<u64, TimedCacheEntry<Arc<SongDetail>>>,
    song_url_cache: DashMap<(u64, u64), TimedCacheEntry<Arc<SongUrl>>>,
    song_lyric_cache: DashMap<u64, TimedCacheEntry<String>>,
}

const SONG_DETAIL_CACHE_TTL: Duration = Duration::from_secs(300);
const SONG_URL_CACHE_TTL: Duration = Duration::from_secs(30);
const SONG_LYRIC_CACHE_TTL: Duration = Duration::from_secs(300);
pub(crate) const ALBUM_ART_DOWNLOAD_TOTAL_ATTEMPTS: u32 = 5;
const PERF_API_LOG_PREFIX: &str = "PERF_API";
const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36";
const SHORT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";
const MEDIA_URL_REWRITE_RULES: [(&str, &str); 8] = [
    ("https://m8.", "https://m7."),
    ("https://m801.", "https://m701."),
    ("https://m804.", "https://m701."),
    ("https://m704.", "https://m701."),
    ("http://m8.", "http://m7."),
    ("http://m801.", "http://m701."),
    ("http://m804.", "http://m701."),
    ("http://m704.", "http://m701."),
];

#[derive(Debug, Clone)]
struct TimedCacheEntry<T> {
    value: T,
    created_at: Instant,
    ttl: Duration,
}

impl<T> TimedCacheEntry<T> {
    fn new(value: T, ttl: Duration) -> Self {
        Self {
            value,
            created_at: Instant::now(),
            ttl,
        }
    }

    fn is_fresh_at(&self, now: Instant) -> bool {
        cache_entry_is_fresh(self.created_at, self.ttl, now)
    }
}

fn cache_entry_is_fresh(created_at: Instant, ttl: Duration, now: Instant) -> bool {
    if let Some(expires_at) = created_at.checked_add(ttl) {
        now < expires_at
    } else {
        false
    }
}

fn song_url_cache_key(song_id: u64, br: u64) -> (u64, u64) {
    (song_id, br)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CachePruneStats {
    pub song_detail_removed: usize,
    pub song_url_removed: usize,
    pub song_lyric_removed: usize,
}

impl CachePruneStats {
    #[must_use]
    pub fn total_removed(self) -> usize {
        self.song_detail_removed + self.song_url_removed + self.song_lyric_removed
    }
}

#[cfg(test)]
mod tests;
