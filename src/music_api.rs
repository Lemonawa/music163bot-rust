use std::borrow::Cow;
use std::io::Cursor;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;

use tokio::time::Duration;

use aes::Aes128;
use cipher::{BlockDecryptMut, BlockEncryptMut, KeyInit, block_padding::Pkcs7};
use ecb::{Decryptor, Encryptor};
use hex::encode_upper;
use image::{DynamicImage, GenericImageView, ImageFormat};
use md5::compute as md5_compute;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Config;
use crate::error::{BotError, Result};
use crate::utils::build_http_client;

#[derive(Debug)]
pub struct MusicApi {
    client: Client,
    pub music_u: Option<String>,
    base_url: String,
    auto_retry: bool,
    max_retry_times: u32,
    eapi_cookie: String,
    music_u_cookie: Option<String>,
    song_detail_cache: DashMap<u64, TimedCacheEntry<Arc<SongDetail>>>,
    song_url_cache: DashMap<(u64, u64), TimedCacheEntry<Arc<SongUrl>>>,
    song_lyric_cache: DashMap<u64, TimedCacheEntry<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SongDetailResponse {
    pub code: i32,
    pub songs: Vec<SongDetail>,
}

#[derive(Debug, Deserialize)]
struct PlaylistDetailResponse {
    code: i32,
    playlist: Option<PlaylistDetail>,
}

#[derive(Debug, Deserialize)]
struct PlaylistDetail {
    #[serde(rename = "trackIds")]
    track_ids: Vec<PlaylistTrackId>,
}

#[derive(Debug, Deserialize)]
struct PlaylistTrackId {
    id: u64,
}

#[derive(Debug, Deserialize)]
struct AlbumSongsResponse {
    code: i32,
    songs: Vec<AlbumSong>,
}

#[derive(Debug, Deserialize)]
struct AlbumSong {
    id: u64,
}

#[derive(Debug, Deserialize)]
struct DjProgramDetailResponse {
    code: i32,
    program: Option<DjProgramItem>,
}

#[derive(Debug, Deserialize)]
struct DjProgramListResponse {
    code: i32,
    count: usize,
    #[serde(default)]
    programs: Vec<DjProgramItem>,
}

#[derive(Debug, Clone, Deserialize)]
struct DjProgramItem {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(rename = "mainTrackId")]
    main_track_id: Option<u64>,
    #[serde(default)]
    dj: Option<DjProgramDj>,
    #[serde(default)]
    radio: Option<DjProgramRadio>,
    #[serde(rename = "coverUrl")]
    cover_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct DjProgramDj {
    #[serde(default)]
    nickname: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DjProgramRadio {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramMainTrack {
    pub program_id: u64,
    pub main_track_id: u64,
    pub program_name: String,
    pub author_name: String,
    pub radio_name: String,
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SongDetail {
    pub id: u64,
    pub name: String,
    #[serde(alias = "duration")]
    pub dt: Option<u64>, // Duration in milliseconds (may be missing)
    #[serde(alias = "artists")]
    pub ar: Option<Vec<Artist>>, // Artists array (may be missing)
    #[serde(alias = "album")]
    pub al: Option<Album>, // Album info (may be missing)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artist {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Album {
    pub id: u64,
    pub name: String,
    #[serde(rename = "picUrl")]
    pub pic_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SongUrlResponse {
    pub code: i32,
    pub data: Vec<SongUrl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SongUrl {
    pub id: u64,
    #[serde(default, deserialize_with = "deserialize_string_or_null")]
    pub url: String,
    pub br: u64,
    pub size: u64,
    #[serde(default, deserialize_with = "deserialize_string_or_null")]
    pub md5: String,
    #[serde(rename = "type")]
    #[serde(default, deserialize_with = "deserialize_string_or_null")]
    pub format: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LyricResponse {
    pub code: i32,
    pub lrc: Option<LyricContent>,
    pub tlyric: Option<LyricContent>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LyricContent {
    pub lyric: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResponse {
    pub code: i32,
    pub result: SearchResult,
}

#[derive(Debug, Serialize, Deserialize)]
struct EapiSearchResponse {
    code: i32,
    result: SearchResult,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub songs: Vec<SearchSong>,
    #[serde(rename = "songCount")]
    pub song_count: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchSong {
    pub id: u64,
    pub name: String,
    pub artists: Vec<Artist>,
    pub album: Album,
    pub duration: u64,
}

const SONG_DETAIL_CACHE_TTL: Duration = Duration::from_secs(300);
const SONG_URL_CACHE_TTL: Duration = Duration::from_secs(30);
const SONG_LYRIC_CACHE_TTL: Duration = Duration::from_secs(300);
const PERF_API_LOG_PREFIX: &str = "PERF_API";
const DEFAULT_AUTO_RETRY: bool = true;
const DEFAULT_MAX_RETRY_TIMES: u32 = 3;
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

fn fallback_bitrate_candidates(
    bitrate_candidates: &[u64],
    primary_attempted_unavailable: bool,
) -> &[u64] {
    if primary_attempted_unavailable {
        bitrate_candidates
            .split_first()
            .map_or(bitrate_candidates, |(_, tail)| tail)
    } else {
        bitrate_candidates
    }
}

fn song_url_has_download_url(song_url: &SongUrl) -> bool {
    !song_url.url.is_empty()
}

fn log_music_api_perf(song_id: u64, stage: &str, duration: Duration) {
    tracing::debug!(
        "{PERF_API_LOG_PREFIX}|music_id={song_id}|stage={stage}|elapsed_ms={}",
        duration.as_millis()
    );
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

include!("music_api/cache_and_crypto.rs");
include!("music_api/requests.rs");
include!("music_api/media.rs");

#[cfg(test)]
mod tests;
