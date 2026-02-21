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

impl MusicApi {
    #[must_use]
    pub fn new(music_u: Option<String>, base_url: String) -> Self {
        Self::new_with_options(
            music_u,
            base_url,
            0,
            10,
            60,
            DEFAULT_AUTO_RETRY,
            DEFAULT_MAX_RETRY_TIMES,
        )
    }

    #[must_use]
    pub fn new_with_config(config: &Config) -> Self {
        Self::new_with_options(
            config.music_u.clone(),
            config.music_api.clone(),
            config.download_pool_max_idle_per_host,
            config.download_connect_timeout_secs,
            config.download_timeout,
            config.auto_retry,
            config.max_retry_times,
        )
    }

    fn new_with_options(
        music_u: Option<String>,
        base_url: String,
        pool_max_idle_per_host: usize,
        connect_timeout_secs: u64,
        request_timeout_secs: u64,
        auto_retry: bool,
        max_retry_times: u32,
    ) -> Self {
        let mut client_builder = Client::builder();

        // Use rustls TLS for better compatibility
        client_builder = client_builder.use_rustls_tls();

        // Performance optimizations
        // pool_max_idle_per_host(0) prevents connection pool memory accumulation
        client_builder = client_builder
            .tcp_nodelay(true)
            .pool_max_idle_per_host(pool_max_idle_per_host)
            .connect_timeout(Duration::from_secs(connect_timeout_secs))
            .timeout(Duration::from_secs(request_timeout_secs.max(1)));

        // Add user agent
        client_builder = client_builder.user_agent(BROWSER_USER_AGENT);

        let client = build_http_client(client_builder).unwrap_or_else(|e| {
            tracing::error!("Failed to build HTTP client: {e}");
            Client::new()
        });

        let eapi_cookie = Self::generate_eapi_cookie(music_u.as_deref());
        let music_u_cookie = music_u.as_ref().map(|u| format!("MUSIC_U={u}"));

        Self {
            client,
            music_u,
            base_url,
            auto_retry,
            max_retry_times,
            eapi_cookie,
            music_u_cookie,
            song_detail_cache: DashMap::new(),
            song_url_cache: DashMap::new(),
            song_lyric_cache: DashMap::new(),
        }
    }

    fn album_art_total_attempts(&self) -> u32 {
        if self.auto_retry {
            self.max_retry_times.saturating_add(1)
        } else {
            1
        }
    }

    fn get_cached_song_detail(&self, song_id: u64) -> Option<Arc<SongDetail>> {
        let now = Instant::now();
        let entry = self.song_detail_cache.get(&song_id)?;
        if entry.is_fresh_at(now) {
            Some(Arc::clone(&entry.value))
        } else {
            drop(entry);
            self.song_detail_cache.remove(&song_id);
            None
        }
    }

    #[cfg(test)]
    fn cache_song_detail(&self, song_id: u64, detail: SongDetail) {
        self.cache_song_detail_shared(song_id, Arc::new(detail));
    }

    fn cache_song_detail_shared(&self, song_id: u64, detail: Arc<SongDetail>) {
        self.song_detail_cache
            .insert(song_id, TimedCacheEntry::new(detail, SONG_DETAIL_CACHE_TTL));
    }

    fn get_cached_song_url(&self, song_id: u64, br: u64) -> Option<Arc<SongUrl>> {
        let key = song_url_cache_key(song_id, br);
        let now = Instant::now();
        let entry = self.song_url_cache.get(&key)?;
        if entry.is_fresh_at(now) {
            Some(Arc::clone(&entry.value))
        } else {
            drop(entry);
            self.song_url_cache.remove(&key);
            None
        }
    }

    fn get_first_cached_song_url(
        &self,
        song_id: u64,
        bitrate_candidates: &[u64],
    ) -> Option<Arc<SongUrl>> {
        for &bitrate in bitrate_candidates {
            if let Some(song_url) = self.get_cached_song_url(song_id, bitrate)
                && song_url_has_download_url(&song_url)
            {
                return Some(song_url);
            }
        }
        None
    }

    #[cfg(test)]
    fn cache_song_url(&self, song_id: u64, br: u64, song_url: SongUrl) {
        self.cache_song_url_shared(song_id, br, Arc::new(song_url));
    }

    fn cache_song_url_shared(&self, song_id: u64, br: u64, song_url: Arc<SongUrl>) {
        let key = song_url_cache_key(song_id, br);
        self.song_url_cache
            .insert(key, TimedCacheEntry::new(song_url, SONG_URL_CACHE_TTL));
    }

    fn get_cached_song_lyric(&self, song_id: u64) -> Option<String> {
        let now = Instant::now();
        let entry = self.song_lyric_cache.get(&song_id)?;
        if entry.is_fresh_at(now) {
            Some(entry.value.clone())
        } else {
            drop(entry);
            self.song_lyric_cache.remove(&song_id);
            None
        }
    }

    fn cache_song_lyric(&self, song_id: u64, lyric: String) {
        self.song_lyric_cache
            .insert(song_id, TimedCacheEntry::new(lyric, SONG_LYRIC_CACHE_TTL));
    }

    #[must_use]
    pub fn prune_expired_cache_entries(&self) -> CachePruneStats {
        let now = Instant::now();

        let detail_before = self.song_detail_cache.len();
        self.song_detail_cache
            .retain(|_, entry| entry.is_fresh_at(now));
        let song_detail_removed = detail_before.saturating_sub(self.song_detail_cache.len());

        let url_before = self.song_url_cache.len();
        self.song_url_cache
            .retain(|_, entry| entry.is_fresh_at(now));
        let song_url_removed = url_before.saturating_sub(self.song_url_cache.len());

        let lyric_before = self.song_lyric_cache.len();
        self.song_lyric_cache
            .retain(|_, entry| entry.is_fresh_at(now));
        let song_lyric_removed = lyric_before.saturating_sub(self.song_lyric_cache.len());

        CachePruneStats {
            song_detail_removed,
            song_url_removed,
            song_lyric_removed,
        }
    }

    fn generate_eapi_cookie(music_u: Option<&str>) -> String {
        let device_id = Uuid::new_v4().simple().to_string();
        let appver = "9.3.40";
        let buildver = SystemTime::now().duration_since(UNIX_EPOCH).map_or_else(
            |_| "0".to_string(),
            |duration| duration.as_secs().to_string(),
        );
        let mut cookie_parts = vec![
            format!("deviceId={}", device_id),
            format!("appver={}", appver),
            format!("buildver={}", &buildver[..buildver.len().min(10)]),
            "resolution=1920x1080".to_string(),
            "os=Android".to_string(),
        ];

        if let Some(music_u) = music_u {
            cookie_parts.push(format!("MUSIC_U={music_u}"));
        } else {
            cookie_parts.push("MUSIC_A=4ee5f776c9ed1e4d5f031b09e084c6cb333e43ee4a841afeebbef9bbf4b7e4152b51ff20ecb9e8ee9e89ab23044cf50d1609e4781e805e73a138419e5583bc7fd1e5933c52368d9127ba9ce4e2f233bf5a77ba40ea6045ae1fc612ead95d7b0e0edf70a74334194e1a190979f5fc12e9968c3666a981495b33a649814e309366".to_string());
        }

        cookie_parts.join("; ")
    }

    fn build_eapi_cookie(&self) -> &str {
        &self.eapi_cookie
    }

    /// Conditionally add the pre-computed MUSIC_U cookie header to a request.
    fn apply_music_u_cookie(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(cookie) = &self.music_u_cookie {
            request.header("Cookie", cookie)
        } else {
            request
        }
    }

    /// Build common headers for image downloads (album art).
    fn apply_image_download_headers(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .header("User-Agent", SHORT_USER_AGENT)
            .header("Referer", "https://music.163.com/")
            .header(
                "Accept",
                "image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
            )
    }

    /// Build common headers for audio file downloads.
    fn apply_audio_download_headers(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .header("User-Agent", BROWSER_USER_AGENT)
            .header("Referer", "https://music.163.com/")
            .header("Accept", "audio/mpeg, audio/*, */*")
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
            .header("Cache-Control", "no-cache")
            .header("DNT", "1")
            .header("Sec-Fetch-Dest", "audio")
            .header("Sec-Fetch-Mode", "cors")
            .header("Sec-Fetch-Site", "cross-site")
    }

    fn eapi_splice(path: &str, json: &str) -> String {
        let text = format!("nobody{path}use{json}md5forencrypt");
        let digest = md5_compute(text.as_bytes());
        let hex_digest = format!("{digest:x}");
        // Pre-allocate: path + "-36cd479b6b5-" + json + "-36cd479b6b5-" + hex_digest
        let mut result = String::with_capacity(path.len() + json.len() + hex_digest.len() + 26);
        result.push_str(path);
        result.push_str("-36cd479b6b5-");
        result.push_str(json);
        result.push_str("-36cd479b6b5-");
        result.push_str(&hex_digest);
        result
    }

    fn eapi_encrypt(data: &str) -> Result<String> {
        Self::eapi_encrypt_with_key(data, b"e82ckenh8dichen8")
    }

    fn eapi_encrypt_with_key(data: &str, key: &[u8]) -> Result<String> {
        let block_size = 16;
        let data_len = data.len();
        let padded_len = ((data_len + block_size) / block_size) * block_size;
        let mut buf = vec![0u8; padded_len];
        buf[..data_len].copy_from_slice(data.as_bytes());
        let encrypted = Encryptor::<Aes128>::new_from_slice(key)
            .map_err(|_| BotError::MusicApi("Invalid eapi key length".to_string()))?
            .encrypt_padded_mut::<Pkcs7>(&mut buf, data_len)
            .map_err(|_| BotError::MusicApi("Failed to encrypt eapi payload".to_string()))?;
        Ok(encode_upper(encrypted))
    }

    fn eapi_decrypt(hex_data: &str) -> Result<String> {
        Self::eapi_decrypt_with_key(hex_data, b"e82ckenh8dichen8")
    }

    fn eapi_decrypt_with_key(hex_data: &str, key: &[u8]) -> Result<String> {
        let mut bytes = hex::decode(hex_data).map_err(|e| BotError::MusicApi(e.to_string()))?;
        let decrypted = Decryptor::<Aes128>::new_from_slice(key)
            .map_err(|_| BotError::MusicApi("Invalid eapi key length".to_string()))?
            .decrypt_padded_mut::<Pkcs7>(&mut bytes)
            .map_err(|e| BotError::MusicApi(e.to_string()))?;
        String::from_utf8(decrypted.to_vec()).map_err(|e| BotError::MusicApi(e.to_string()))
    }

    fn eapi_params(path: &str, json: &str) -> Result<String> {
        let data = Self::eapi_splice(path, json);
        let encrypted = Self::eapi_encrypt(&data)?;
        Ok(format!("params={encrypted}"))
    }

    fn choose_eapi_user_agent() -> &'static str {
        "NeteaseMusic/9.3.40.1753206443(164);Dalvik/2.1.0 (Linux; U; Android 9; MIX 2 MIUI/V12.0.1.0.PDECNXM)"
    }

    async fn get_song_detail_shared(&self, song_id: u64) -> Result<Arc<SongDetail>> {
        if let Some(cached) = self.get_cached_song_detail(song_id) {
            return Ok(cached);
        }

        let url = format!("{}/api/song/detail", self.base_url);
        let single_id = song_id.to_string();
        let wrapped_ids = format!("[{song_id}]");

        let mut request = self
            .client
            .post(url)
            .form(&[("id", &*single_id), ("ids", &*wrapped_ids)]);

        request = self.apply_music_u_cookie(request);

        let response = request.send().await?.error_for_status()?;
        let data: SongDetailResponse = response.json().await?;

        if data.code != 200 {
            return Err(BotError::MusicApi(format!(
                "API returned code {}",
                data.code
            )));
        }

        let detail = data
            .songs
            .into_iter()
            .next()
            .ok_or_else(|| BotError::MusicApi("No song found".to_string()))?;
        let detail = Arc::new(detail);
        self.cache_song_detail_shared(song_id, Arc::clone(&detail));
        Ok(detail)
    }

    async fn get_song_url_shared(&self, song_id: u64, br: u64) -> Result<Arc<SongUrl>> {
        if let Some(cached) = self.get_cached_song_url(song_id, br) {
            return Ok(cached);
        }

        let url = format!("{}/api/song/enhance/player/url", self.base_url);
        let ids_str = format!("[{song_id}]");
        let br_str = br.to_string();

        let mut request = self
            .client
            .post(url)
            .form(&[("ids", &*ids_str), ("br", &*br_str)]);

        request = self.apply_music_u_cookie(request);

        let response = request.send().await?.error_for_status()?;
        let data: SongUrlResponse = response.json().await?;

        if data.code != 200 {
            return Err(BotError::MusicApi(format!(
                "API returned code {}",
                data.code
            )));
        }

        let song_url = data
            .data
            .into_iter()
            .next()
            .ok_or_else(|| BotError::MusicApi("No download URL found".to_string()))?;
        let song_url = Arc::new(song_url);
        self.cache_song_url_shared(song_id, br, Arc::clone(&song_url));
        Ok(song_url)
    }

    /// Get song details
    pub async fn get_song_detail(&self, song_id: u64) -> Result<SongDetail> {
        self.get_song_detail_shared(song_id)
            .await
            .map(|detail| detail.as_ref().clone())
    }

    /// Get song download URL
    pub async fn get_song_url(&self, song_id: u64, br: u64) -> Result<SongUrl> {
        self.get_song_url_shared(song_id, br)
            .await
            .map(|song_url| song_url.as_ref().clone())
    }

    /// Get song details and best available URL using a batch-first strategy with safe fallback.
    pub async fn get_song_detail_and_best_url(
        &self,
        song_id: u64,
        bitrate_candidates: &[u64],
    ) -> Result<(Arc<SongDetail>, Arc<SongUrl>)> {
        let select_url_total_start = Instant::now();
        let Some((&primary_bitrate, _)) = bitrate_candidates.split_first() else {
            return Err(BotError::MusicApi(
                "No bitrate candidates provided".to_string(),
            ));
        };

        let mut cached_detail = self.get_cached_song_detail(song_id);
        if let Some(ref detail) = cached_detail
            && let Some(song_url) = self.get_first_cached_song_url(song_id, bitrate_candidates)
        {
            log_music_api_perf(
                song_id,
                "select_url_total",
                select_url_total_start.elapsed(),
            );
            return Ok((Arc::clone(detail), song_url));
        }

        let mut primary_url = self.get_cached_song_url(song_id, primary_bitrate);
        let mut primary_attempted_unavailable = false;

        // Fetch detail and primary URL in parallel when either is missing
        if cached_detail.is_none() || primary_url.is_none() {
            let parallel_start = Instant::now();
            let need_detail = cached_detail.is_none();
            let need_url = primary_url.is_none();

            let detail_fut = async {
                if need_detail {
                    Some(self.get_song_detail_shared(song_id).await)
                } else {
                    None
                }
            };
            let url_fut = async {
                if need_url {
                    Some(self.get_song_url_shared(song_id, primary_bitrate).await)
                } else {
                    None
                }
            };

            let (detail_result, url_result) = tokio::join!(detail_fut, url_fut);

            if let Some(result) = detail_result {
                cached_detail = Some(result?);
            }
            if let Some(result) = url_result {
                match result {
                    Ok(song_url) if song_url_has_download_url(&song_url) => {
                        primary_url = Some(song_url);
                    }
                    Ok(_) => {
                        primary_attempted_unavailable = true;
                        tracing::debug!(
                            "Primary bitrate {primary_bitrate} returned empty URL for music_id {song_id}"
                        );
                    }
                    Err(e) => {
                        primary_attempted_unavailable = true;
                        tracing::warn!(
                            "Primary bitrate {primary_bitrate} request failed for music_id {song_id}: {e}"
                        );
                    }
                }
            }

            tracing::debug!(
                "[parallel_fetch] {}ms (detail={need_detail}, url={need_url})",
                parallel_start.elapsed().as_millis()
            );
            log_music_api_perf(song_id, "parallel_fetch", parallel_start.elapsed());
        }

        let Some(detail) = cached_detail else {
            log_music_api_perf(
                song_id,
                "select_url_total",
                select_url_total_start.elapsed(),
            );
            return Err(BotError::MusicApi(format!(
                "Failed to get song detail for {song_id}"
            )));
        };

        let mut last_error = None;
        let mut fallback_url_start = None;
        for &bitrate in
            fallback_bitrate_candidates(bitrate_candidates, primary_attempted_unavailable)
        {
            let fetched_url = if bitrate == primary_bitrate {
                if let Some(song_url) = primary_url.take() {
                    Ok(song_url)
                } else {
                    fallback_url_start.get_or_insert_with(Instant::now);
                    self.get_song_url_shared(song_id, bitrate).await
                }
            } else {
                fallback_url_start.get_or_insert_with(Instant::now);
                self.get_song_url_shared(song_id, bitrate).await
            };

            match fetched_url {
                Ok(song_url) if song_url_has_download_url(&song_url) => {
                    if let Some(start) = fallback_url_start {
                        let fallback_duration = start.elapsed();
                        tracing::debug!("[fallback_url] {}ms", fallback_duration.as_millis());
                        log_music_api_perf(song_id, "fallback_url", fallback_duration);
                    }
                    log_music_api_perf(
                        song_id,
                        "select_url_total",
                        select_url_total_start.elapsed(),
                    );
                    return Ok((Arc::clone(&detail), song_url));
                }
                Ok(_) => {
                    tracing::debug!(
                        "Bitrate {} returned empty URL for music_id {}, trying next fallback",
                        bitrate,
                        song_id
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "Bitrate {} request failed for music_id {}: {}",
                        bitrate,
                        song_id,
                        e
                    );
                    last_error = Some(e);
                }
            }
        }

        if let Some(start) = fallback_url_start {
            let fallback_duration = start.elapsed();
            tracing::debug!("[fallback_url] {}ms", fallback_duration.as_millis());
            log_music_api_perf(song_id, "fallback_url", fallback_duration);
        }

        log_music_api_perf(
            song_id,
            "select_url_total",
            select_url_total_start.elapsed(),
        );
        if let Some(e) = last_error {
            Err(e)
        } else {
            Err(BotError::MusicApi("No download URL found".to_string()))
        }
    }

    /// Get song lyrics
    pub async fn get_song_lyric(&self, song_id: u64) -> Result<String> {
        if let Some(cached) = self.get_cached_song_lyric(song_id) {
            return Ok(cached);
        }

        let url = format!("{}/api/song/lyric?id={}&lv=1&tv=1", self.base_url, song_id);

        let mut request = self.client.get(&url);

        request = self.apply_music_u_cookie(request);

        let response = request.send().await?.error_for_status()?;
        let data: LyricResponse = response.json().await?;

        if data.code != 200 {
            return Err(BotError::MusicApi(format!(
                "API returned code {}",
                data.code
            )));
        }

        let lyric = data
            .lrc
            .map_or_else(|| "No lyrics available".to_string(), |l| l.lyric);

        self.cache_song_lyric(song_id, lyric.clone());

        Ok(lyric)
    }

    /// Search songs
    pub async fn search_songs(&self, keyword: &str, limit: u32) -> Result<Vec<SearchSong>> {
        #[derive(Serialize)]
        struct SearchPayload<'a> {
            s: &'a str,
            offset: u32,
            limit: u32,
        }

        let path = "/api/v1/search/song/get";
        let url = format!("{}/eapi/v1/search/song/get", self.base_url);
        let payload_str = serde_json::to_string(&SearchPayload {
            s: keyword,
            offset: 0,
            limit: limit.max(1),
        })?;
        let body = Self::eapi_params(path, &payload_str)?;
        let request = self
            .client
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("User-Agent", Self::choose_eapi_user_agent())
            .header("Cookie", self.build_eapi_cookie())
            .body(body);

        let response = request.send().await?.error_for_status()?;
        let raw_bytes = response.bytes().await?;
        // Skip leading whitespace bytes
        let trimmed_bytes = raw_bytes
            .iter()
            .position(|&b| !b.is_ascii_whitespace())
            .map_or(&raw_bytes[..], |pos| &raw_bytes[pos..]);
        let data: EapiSearchResponse = if trimmed_bytes.first() == Some(&b'{') {
            serde_json::from_slice(trimmed_bytes)?
        } else {
            let trimmed_str = std::str::from_utf8(trimmed_bytes)
                .map_err(|e| BotError::MusicApi(format!("Invalid UTF-8 in response: {e}")))?;
            let decrypted = Self::eapi_decrypt(trimmed_str)?;
            serde_json::from_str(&decrypted)?
        };

        if data.code != 200 {
            return Err(BotError::MusicApi(format!(
                "API returned code {}",
                data.code
            )));
        }

        Ok(data.result.songs)
    }

    /// Download file with proper headers and cookies
    pub async fn download_file(&self, url: &str) -> Result<reqwest::Response> {
        // Apply host replacement similar to the original Go project
        // This helps avoid 403 errors from NetEase servers
        let processed_url = rewrite_media_url(url);

        let request = self.client.get(processed_url.as_ref());

        // Add MUSIC_U cookie if available
        let request = self.apply_music_u_cookie(request);

        // Add comprehensive headers to avoid 403 errors
        let request = Self::apply_audio_download_headers(request);

        let response = request.send().await?;
        Ok(response)
    }

    /// Resolve final URL for share links with minimal body transfer
    pub async fn resolve_share_link(&self, url: &str) -> Result<reqwest::Url> {
        let response = self
            .client
            .get(url)
            .header("User-Agent", SHORT_USER_AGENT)
            .header("Accept", "*/*")
            .header(reqwest::header::RANGE, "bytes=0-0")
            .send()
            .await?
            .error_for_status()?;

        Ok(response.url().clone())
    }

    /// Download and resize album art image into memory
    /// Uses spawn_blocking for CPU-intensive image processing to avoid blocking async runtime
    pub async fn download_album_art_data(&self, pic_url: &str) -> Result<Vec<u8>> {
        if pic_url.is_empty() {
            return Err(BotError::MusicApi("Empty album art URL".to_string()));
        }

        let total_attempts = self.album_art_total_attempts();
        let mut last_error = None;

        for attempt in 1..=total_attempts {
            match self.download_album_art_data_once(pic_url).await {
                Ok(data) => return Ok(data),
                Err(e) => {
                    if attempt < total_attempts {
                        tracing::warn!(
                            "Album art download attempt {}/{} failed for {}: {}",
                            attempt,
                            total_attempts,
                            pic_url,
                            e
                        );
                    }
                    last_error = Some(e);
                }
            }
        }

        Err(last_error
            .unwrap_or_else(|| BotError::MusicApi("Album art download failed".to_string())))
    }

    async fn download_album_art_data_once(&self, pic_url: &str) -> Result<Vec<u8>> {
        // Download the image with common headers
        let request = self.client.get(pic_url);
        let request = Self::apply_image_download_headers(request);

        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(BotError::MusicApi(format!(
                "Failed to download album art: {}",
                response.status()
            )));
        }

        let bytes = response.bytes().await?;

        // Process image in spawn_blocking to avoid blocking async runtime
        let processed = tokio::task::spawn_blocking(move || resize_album_art_to_thumbnail(&bytes))
            .await
            .map_err(|e| BotError::MusicApi(format!("Image processing task failed: {e}")))??;

        Ok(processed)
    }
}

fn build_http_client(builder: reqwest::ClientBuilder) -> Result<reqwest::Client> {
    builder.build().map_err(|e| {
        tracing::error!("Failed to build HTTP client: {}", e);
        BotError::Network(e)
    })
}

fn rewrite_media_url(url: &str) -> Cow<'_, str> {
    for (from_prefix, to_prefix) in MEDIA_URL_REWRITE_RULES {
        if let Some(rest) = url.strip_prefix(from_prefix) {
            return Cow::Owned(format!("{to_prefix}{rest}"));
        }
    }
    Cow::Borrowed(url)
}

pub fn resize_album_art_to_thumbnail(image_bytes: &[u8]) -> Result<Vec<u8>> {
    let img = image::load_from_memory(image_bytes)
        .map_err(|e| BotError::MusicApi(format!("Failed to decode image: {e}")))?;

    let resized = resize_image_with_padding(img, 320, 320);

    let mut cursor = Cursor::new(Vec::with_capacity(32 * 1024));
    resized
        .write_to(&mut cursor, ImageFormat::Jpeg)
        .map_err(|e| BotError::MusicApi(format!("Failed to encode image: {e}")))?;

    Ok(cursor.into_inner())
}

fn deserialize_string_or_null<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Option::unwrap_or_default)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinHandle;
    use tokio::time::Duration;

    use crate::config::Config;
    use crate::error::BotError;

    use super::build_http_client;
    use super::{Album, Artist, MusicApi, SongDetail, SongUrl};

    #[derive(Clone, Debug)]
    enum MockSongUrlReply {
        OkWithUrl(&'static str),
        OkEmptyUrl,
        ApiCode(i32),
    }

    #[derive(Debug)]
    struct MockMusicApiServerState {
        song_id: u64,
        song_url_sequences: HashMap<u64, VecDeque<MockSongUrlReply>>,
        calls_by_bitrate: HashMap<u64, usize>,
    }

    impl MockMusicApiServerState {
        fn new(song_id: u64, responses: HashMap<u64, Vec<MockSongUrlReply>>) -> Self {
            let song_url_sequences = responses
                .into_iter()
                .map(|(bitrate, items)| (bitrate, VecDeque::from(items)))
                .collect();
            Self {
                song_id,
                song_url_sequences,
                calls_by_bitrate: HashMap::new(),
            }
        }
    }

    struct MockMusicApiServer {
        base_url: String,
        state: Arc<Mutex<MockMusicApiServerState>>,
        accept_loop_task: JoinHandle<()>,
    }

    impl MockMusicApiServer {
        async fn start(song_id: u64, responses: HashMap<u64, Vec<MockSongUrlReply>>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind mock server");
            let address = listener.local_addr().expect("mock server local addr");
            let state = Arc::new(Mutex::new(MockMusicApiServerState::new(song_id, responses)));
            let shared_state = Arc::clone(&state);

            let accept_loop_task = tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let connection_state = Arc::clone(&shared_state);
                    tokio::spawn(async move {
                        let _ = handle_mock_music_api_connection(stream, connection_state).await;
                    });
                }
            });

            Self {
                base_url: format!("http://{address}"),
                state,
                accept_loop_task,
            }
        }

        fn base_url(&self) -> String {
            self.base_url.clone()
        }

        fn calls_for_bitrate(&self, bitrate: u64) -> usize {
            let state = self.state.lock().expect("lock mock server state");
            *state.calls_by_bitrate.get(&bitrate).unwrap_or(&0)
        }
    }

    impl Drop for MockMusicApiServer {
        fn drop(&mut self) {
            self.accept_loop_task.abort();
        }
    }

    async fn handle_mock_music_api_connection(
        mut stream: TcpStream,
        state: Arc<Mutex<MockMusicApiServerState>>,
    ) -> std::io::Result<()> {
        let Some((path, request_body)) = read_http_request(&mut stream).await? else {
            return Ok(());
        };

        let body = match path.as_str() {
            "/api/song/detail" => {
                let song_id = state.lock().expect("lock mock server state").song_id;
                mock_song_detail_response_json(song_id)
            }
            "/api/song/enhance/player/url" => mock_song_url_response_json(&state, &request_body),
            _ => r#"{"code":404}"#.to_string(),
        };

        write_json_response(&mut stream, &body).await
    }

    async fn write_json_response(stream: &mut TcpStream, body: &str) -> std::io::Result<()> {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await
    }

    async fn read_http_request(
        stream: &mut TcpStream,
    ) -> std::io::Result<Option<(String, String)>> {
        let mut request_buffer = Vec::new();
        let mut chunk = [0u8; 1024];

        let header_end = loop {
            let read_size = stream.read(&mut chunk).await?;
            if read_size == 0 {
                return Ok(None);
            }
            request_buffer.extend_from_slice(&chunk[..read_size]);
            if let Some(pos) = find_byte_sequence(&request_buffer, b"\r\n\r\n") {
                break pos;
            }
        };

        let headers = String::from_utf8_lossy(&request_buffer[..header_end]);
        let request_line = headers.lines().next().unwrap_or_default();
        let path = request_line
            .split_whitespace()
            .nth(1)
            .map_or_else(|| "/".to_string(), ToString::to_string);

        let content_length = parse_content_length(&headers);
        let body_start = header_end + 4;
        let mut body = if body_start < request_buffer.len() {
            request_buffer[body_start..].to_vec()
        } else {
            Vec::new()
        };

        while body.len() < content_length {
            let read_size = stream.read(&mut chunk).await?;
            if read_size == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read_size]);
        }
        body.truncate(content_length);

        let body = String::from_utf8_lossy(&body).into_owned();
        Ok(Some((path, body)))
    }

    fn parse_content_length(headers: &str) -> usize {
        headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                if name.trim().eq_ignore_ascii_case("content-length") {
                    value.trim().parse().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }

    fn find_byte_sequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.is_empty() {
            return Some(0);
        }
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn parse_form_field_as_u64(body: &str, field: &str) -> Option<u64> {
        url::form_urlencoded::parse(body.as_bytes()).find_map(|(k, v)| {
            if k == field {
                v.parse::<u64>().ok()
            } else {
                None
            }
        })
    }

    fn mock_song_detail_response_json(song_id: u64) -> String {
        format!(
            r#"{{"code":200,"songs":[{{"id":{song_id},"name":"Mock Song {song_id}","dt":240000,"ar":[{{"id":1,"name":"Mock Artist"}}],"al":{{"id":2,"name":"Mock Album","picUrl":null}}}}]}}"#
        )
    }

    fn mock_song_url_response_json(
        state: &Arc<Mutex<MockMusicApiServerState>>,
        request_body: &str,
    ) -> String {
        let bitrate = parse_form_field_as_u64(request_body, "br").unwrap_or_default();
        let (song_id, reply) = {
            let mut guard = state.lock().expect("lock mock server state");
            let calls = guard.calls_by_bitrate.entry(bitrate).or_insert(0);
            *calls += 1;
            let reply = guard
                .song_url_sequences
                .get_mut(&bitrate)
                .and_then(VecDeque::pop_front)
                .unwrap_or(MockSongUrlReply::ApiCode(598));
            (guard.song_id, reply)
        };

        match reply {
            MockSongUrlReply::OkWithUrl(url) => format!(
                r#"{{"code":200,"data":[{{"id":{song_id},"url":"{url}","br":{bitrate},"size":12345,"md5":"abc","type":"mp3"}}]}}"#
            ),
            MockSongUrlReply::OkEmptyUrl => format!(
                r#"{{"code":200,"data":[{{"id":{song_id},"url":null,"br":{bitrate},"size":12345,"md5":null,"type":null}}]}}"#
            ),
            MockSongUrlReply::ApiCode(code) => {
                format!(r#"{{"code":{code},"data":[]}}"#)
            }
        }
    }

    fn sample_song_detail(song_id: u64) -> SongDetail {
        SongDetail {
            id: song_id,
            name: format!("Sample Song {song_id}"),
            dt: Some(180_000),
            ar: Some(vec![Artist {
                id: 7,
                name: "Sample Artist".to_string(),
            }]),
            al: Some(Album {
                id: 8,
                name: "Sample Album".to_string(),
                pic_url: None,
            }),
        }
    }

    fn sample_song_url(song_id: u64, bitrate: u64, url: &str) -> SongUrl {
        SongUrl {
            id: song_id,
            url: url.to_string(),
            br: bitrate,
            size: 1_024,
            md5: "md5".to_string(),
            format: "mp3".to_string(),
        }
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

    #[test]
    fn thumbnail_transform_generates_jpeg_output() {
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

        let thumbnail = super::resize_album_art_to_thumbnail(&png_bytes).expect("thumbnail bytes");
        assert!(!thumbnail.is_empty());
        assert_eq!(thumbnail[0], 0xFF);
        assert_eq!(thumbnail[1], 0xD8);
    }

    #[test]
    fn thumbnail_resize_is_320_square_jpeg() {
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
    fn fallback_candidates_skip_primary_after_attempt() {
        let candidates = [320_000, 192_000, 128_000];
        let fallback = super::fallback_bitrate_candidates(&candidates, true);
        assert_eq!(fallback, &[192_000, 128_000]);
    }

    #[test]
    fn fallback_candidates_keep_primary_when_not_attempted() {
        let candidates = [320_000, 192_000, 128_000];
        let fallback = super::fallback_bitrate_candidates(&candidates, false);
        assert_eq!(fallback, &[320_000, 192_000, 128_000]);
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
                created_at: now - super::SONG_DETAIL_CACHE_TTL - Duration::from_secs(1),
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
                created_at: now - super::SONG_URL_CACHE_TTL - Duration::from_secs(1),
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
                created_at: now - super::SONG_LYRIC_CACHE_TTL - Duration::from_secs(1),
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

    #[tokio::test]
    async fn get_song_detail_and_best_url_returns_cached_detail_and_cached_fallback_without_network()
     {
        let song_id = 1001;
        let api = MusicApi::new(None, "http://127.0.0.1:0".to_string());
        api.cache_song_detail(song_id, sample_song_detail(song_id));
        api.cache_song_url(
            song_id,
            192_000,
            sample_song_url(song_id, 192_000, "https://cache.example/fallback-192.mp3"),
        );

        let (detail, song_url) = api
            .get_song_detail_and_best_url(song_id, &[320_000, 192_000, 128_000])
            .await
            .expect("cached detail + cached fallback URL should return immediately");

        assert_eq!(detail.id, song_id);
        assert_eq!(song_url.br, 192_000);
        assert_eq!(song_url.url, "https://cache.example/fallback-192.mp3");
    }

    #[tokio::test]
    async fn get_song_detail_and_best_url_falls_back_when_primary_returns_empty_url() {
        let song_id = 1002;
        let server = MockMusicApiServer::start(
            song_id,
            HashMap::from([
                (320_000, vec![MockSongUrlReply::OkEmptyUrl]),
                (
                    192_000,
                    vec![MockSongUrlReply::OkWithUrl(
                        "https://mock.example/fallback-192.mp3",
                    )],
                ),
            ]),
        )
        .await;
        let api = MusicApi::new(None, server.base_url());
        api.cache_song_detail(song_id, sample_song_detail(song_id));

        let (_, song_url) = api
            .get_song_detail_and_best_url(song_id, &[320_000, 192_000])
            .await
            .expect("fallback bitrate should succeed when primary URL is empty");

        assert_eq!(song_url.br, 192_000);
        assert_eq!(song_url.url, "https://mock.example/fallback-192.mp3");
        assert_eq!(server.calls_for_bitrate(320_000), 1);
        assert_eq!(server.calls_for_bitrate(192_000), 1);
    }

    #[tokio::test]
    async fn get_song_detail_and_best_url_returns_last_error_when_all_fallbacks_fail() {
        let song_id = 1003;
        let server = MockMusicApiServer::start(
            song_id,
            HashMap::from([
                (192_000, vec![MockSongUrlReply::ApiCode(500)]),
                (128_000, vec![MockSongUrlReply::ApiCode(404)]),
            ]),
        )
        .await;
        let api = MusicApi::new(None, server.base_url());
        api.cache_song_detail(song_id, sample_song_detail(song_id));
        api.cache_song_url(song_id, 320_000, sample_song_url(song_id, 320_000, ""));

        let error = api
            .get_song_detail_and_best_url(song_id, &[320_000, 192_000, 128_000])
            .await
            .expect_err("all fallback attempts should fail");

        match error {
            BotError::MusicApi(message) => {
                assert!(
                    message.contains("404"),
                    "expected last fallback error (code 404), got: {message}"
                );
            }
            other => panic!("expected BotError::MusicApi, got: {other:?}"),
        }
        assert_eq!(server.calls_for_bitrate(320_000), 0);
        assert_eq!(server.calls_for_bitrate(192_000), 1);
        assert_eq!(server.calls_for_bitrate(128_000), 1);
    }

    #[tokio::test]
    async fn get_song_detail_and_best_url_returns_error_when_primary_unavailable_and_fallback_fails()
     {
        let song_id = 1004;
        let server = MockMusicApiServer::start(
            song_id,
            HashMap::from([
                (320_000, vec![MockSongUrlReply::OkEmptyUrl]),
                (192_000, vec![MockSongUrlReply::ApiCode(503)]),
            ]),
        )
        .await;
        let api = MusicApi::new(None, server.base_url());
        api.cache_song_detail(song_id, sample_song_detail(song_id));

        let result = api
            .get_song_detail_and_best_url(song_id, &[320_000, 192_000])
            .await;

        assert!(
            result.is_err(),
            "should return error when primary unavailable and fallback also fails"
        );
        assert_eq!(server.calls_for_bitrate(320_000), 1);
        assert_eq!(server.calls_for_bitrate(192_000), 1);
    }

    #[tokio::test]
    async fn get_song_detail_and_best_url_accepts_fallback_without_retrying_primary_for_downgraded_bitrate()
     {
        let song_id = 1005;
        let server = MockMusicApiServer::start(
            song_id,
            HashMap::from([
                (999_000, vec![MockSongUrlReply::OkEmptyUrl]),
                (
                    320_000,
                    vec![MockSongUrlReply::OkWithUrl(
                        "https://mock.example/fallback-320.mp3",
                    )],
                ),
            ]),
        )
        .await;
        let mut config = Config::default();
        config.music_api = server.base_url();
        config.music_u = Some("cookie".to_string());
        config.auto_retry = true;
        config.max_retry_times = 1;
        let api = MusicApi::new_with_config(&config);
        api.cache_song_detail(song_id, sample_song_detail(song_id));

        let (_, song_url) = api
            .get_song_detail_and_best_url(song_id, &[999_000, 320_000, 128_000])
            .await
            .expect("should return fallback bitrate without retrying primary");

        assert_eq!(song_url.br, 320_000);
        assert_eq!(song_url.url, "https://mock.example/fallback-320.mp3");
        // primary should only be tried once (no retry)
        assert_eq!(server.calls_for_bitrate(999_000), 1);
        assert_eq!(server.calls_for_bitrate(320_000), 1);
    }

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
}

/// Parse artists into a formatted string
#[must_use]
pub fn format_artists(artists: &[Artist]) -> String {
    let mut iter = artists.iter();
    let Some(first) = iter.next() else {
        return String::new();
    };
    // Pre-allocate: sum of name lengths + separators
    let capacity =
        artists.iter().map(|a| a.name.len()).sum::<usize>() + artists.len().saturating_sub(1);
    let mut formatted = String::with_capacity(capacity);
    formatted.push_str(&first.name);
    for artist in iter {
        formatted.push('/');
        formatted.push_str(&artist.name);
    }
    formatted
}

/// Resize image with black padding to maintain aspect ratio (like the original Go project)
fn resize_image_with_padding(
    img: DynamicImage,
    target_width: u32,
    target_height: u32,
) -> DynamicImage {
    use image::RgbImage;

    let (orig_width, orig_height) = img.dimensions();
    let aspect_ratio = orig_width as f32 / orig_height as f32;
    let target_aspect_ratio = target_width as f32 / target_height as f32;

    // Calculate new dimensions while maintaining aspect ratio
    let (new_width, new_height) = if aspect_ratio > target_aspect_ratio {
        // Image is wider than target ratio, fit by width
        let new_width = target_width;
        let new_height = (target_width as f32 / aspect_ratio) as u32;
        (new_width, new_height)
    } else {
        // Image is taller than target ratio, fit by height
        let new_height = target_height;
        let new_width = (target_height as f32 * aspect_ratio) as u32;
        (new_width, new_height)
    };

    // Resize the image
    let resized = img.resize(new_width, new_height, image::imageops::FilterType::Lanczos3);

    // Create black background canvas
    let mut canvas = RgbImage::new(target_width, target_height);

    // Calculate position to center the resized image
    let offset_x = (target_width - new_width) / 2;
    let offset_y = (target_height - new_height) / 2;

    // Overlay resized image onto canvas using imageops::overlay (avoids per-pixel loop)
    image::imageops::overlay(
        &mut canvas,
        &resized.to_rgb8(),
        i64::from(offset_x),
        i64::from(offset_y),
    );

    DynamicImage::ImageRgb8(canvas)
}
