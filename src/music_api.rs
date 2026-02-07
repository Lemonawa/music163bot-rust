use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aes::Aes128;
use cipher::{BlockDecryptMut, BlockEncryptMut, KeyInit, block_padding::Pkcs7};
use ecb::{Decryptor, Encryptor};
use hex::encode_upper;
use image::{DynamicImage, GenericImageView, ImageFormat};
use md5::compute as md5_compute;
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Config;
use crate::error::{BotError, Result};

#[derive(Debug)]
pub struct MusicApi {
    client: Client,
    pub music_u: Option<String>,
    base_url: String,
    song_detail_cache: std::sync::Mutex<HashMap<u64, TimedCacheEntry<SongDetail>>>,
    song_url_cache: std::sync::Mutex<HashMap<(u64, u64), TimedCacheEntry<SongUrl>>>,
    song_lyric_cache: std::sync::Mutex<HashMap<u64, TimedCacheEntry<String>>>,
    batch_circuit: std::sync::Mutex<BatchCircuitState>,
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
    pub url: String,
    pub br: u64,
    pub size: u64,
    pub md5: String,
    #[serde(rename = "type")]
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
    pub code: i32,
    pub result: SearchResult,
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
const BATCH_CIRCUIT_BREAKER_THRESHOLD: u8 = 3;
const BATCH_CIRCUIT_BREAKER_COOLDOWN: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, Default)]
struct BatchCircuitState {
    consecutive_failures: u8,
    disabled_until: Option<Instant>,
}

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

fn lock_or_recover<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

impl MusicApi {
    #[must_use]
    pub fn new(music_u: Option<String>, base_url: String) -> Self {
        Self::new_with_options(music_u, base_url, 0, 10, 60)
    }

    #[must_use]
    pub fn new_with_config(config: &Config) -> Self {
        Self::new_with_options(
            config.music_u.clone(),
            config.music_api.clone(),
            config.download_pool_max_idle_per_host,
            config.download_connect_timeout_secs,
            config.download_timeout,
        )
    }

    fn new_with_options(
        music_u: Option<String>,
        base_url: String,
        pool_max_idle_per_host: usize,
        connect_timeout_secs: u64,
        request_timeout_secs: u64,
    ) -> Self {
        let mut client_builder = Client::builder();

        // Use rustls TLS for better compatibility
        client_builder = client_builder.use_rustls_tls();

        // Performance optimizations
        // pool_max_idle_per_host(0) prevents connection pool memory accumulation
        client_builder = client_builder
            .tcp_nodelay(true)
            .pool_max_idle_per_host(pool_max_idle_per_host)
            .connect_timeout(std::time::Duration::from_secs(connect_timeout_secs))
            .timeout(std::time::Duration::from_secs(request_timeout_secs.max(1)));

        // Add user agent
        client_builder = client_builder
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36");

        let client = build_http_client(client_builder).unwrap_or_else(|e| {
            tracing::error!("Failed to build HTTP client: {e}");
            Client::new()
        });

        Self {
            client,
            music_u,
            base_url,
            song_detail_cache: std::sync::Mutex::new(HashMap::new()),
            song_url_cache: std::sync::Mutex::new(HashMap::new()),
            song_lyric_cache: std::sync::Mutex::new(HashMap::new()),
            batch_circuit: std::sync::Mutex::new(BatchCircuitState::default()),
        }
    }

    fn get_cached_song_detail(&self, song_id: u64) -> Option<SongDetail> {
        let now = Instant::now();
        let mut cache = lock_or_recover(&self.song_detail_cache);
        if let Some(entry) = cache.get(&song_id)
            && entry.is_fresh_at(now)
        {
            return Some(entry.value.clone());
        }

        cache.remove(&song_id);
        None
    }

    fn cache_song_detail(&self, song_id: u64, detail: SongDetail) {
        let mut cache = lock_or_recover(&self.song_detail_cache);
        cache.insert(song_id, TimedCacheEntry::new(detail, SONG_DETAIL_CACHE_TTL));
    }

    fn get_cached_song_url(&self, song_id: u64, br: u64) -> Option<SongUrl> {
        let key = song_url_cache_key(song_id, br);
        let now = Instant::now();
        let mut cache = lock_or_recover(&self.song_url_cache);
        if let Some(entry) = cache.get(&key)
            && entry.is_fresh_at(now)
        {
            return Some(entry.value.clone());
        }

        cache.remove(&key);
        None
    }

    fn cache_song_url(&self, song_id: u64, br: u64, song_url: SongUrl) {
        let key = song_url_cache_key(song_id, br);
        let mut cache = lock_or_recover(&self.song_url_cache);
        cache.insert(key, TimedCacheEntry::new(song_url, SONG_URL_CACHE_TTL));
    }

    fn get_cached_song_lyric(&self, song_id: u64) -> Option<String> {
        let now = Instant::now();
        let mut cache = lock_or_recover(&self.song_lyric_cache);
        if let Some(entry) = cache.get(&song_id)
            && entry.is_fresh_at(now)
        {
            return Some(entry.value.clone());
        }

        cache.remove(&song_id);
        None
    }

    fn cache_song_lyric(&self, song_id: u64, lyric: String) {
        let mut cache = lock_or_recover(&self.song_lyric_cache);
        cache.insert(song_id, TimedCacheEntry::new(lyric, SONG_LYRIC_CACHE_TTL));
    }

    fn batch_circuit_allows_attempt(&self) -> bool {
        self.batch_circuit_allows_attempt_at(Instant::now())
    }

    fn batch_circuit_allows_attempt_at(&self, now: Instant) -> bool {
        let mut state = lock_or_recover(&self.batch_circuit);
        match state.disabled_until {
            Some(until) if now < until => false,
            Some(_) => {
                state.disabled_until = None;
                state.consecutive_failures = 0;
                true
            }
            None => true,
        }
    }

    fn record_batch_failure(&self) -> Option<Instant> {
        self.record_batch_failure_at(Instant::now())
    }

    fn record_batch_failure_at(&self, now: Instant) -> Option<Instant> {
        let mut state = lock_or_recover(&self.batch_circuit);
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);

        if state.consecutive_failures >= BATCH_CIRCUIT_BREAKER_THRESHOLD {
            let until = now + BATCH_CIRCUIT_BREAKER_COOLDOWN;
            state.disabled_until = Some(until);
            state.consecutive_failures = 0;
            return Some(until);
        }

        None
    }

    fn record_batch_success(&self) {
        let mut state = lock_or_recover(&self.batch_circuit);
        state.consecutive_failures = 0;
        state.disabled_until = None;
    }

    fn build_eapi_cookie(&self) -> String {
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

        if let Some(music_u) = &self.music_u {
            cookie_parts.push(format!("MUSIC_U={music_u}"));
        } else {
            cookie_parts.push("MUSIC_A=4ee5f776c9ed1e4d5f031b09e084c6cb333e43ee4a841afeebbef9bbf4b7e4152b51ff20ecb9e8ee9e89ab23044cf50d1609e4781e805e73a138419e5583bc7fd1e5933c52368d9127ba9ce4e2f233bf5a77ba40ea6045ae1fc612ead95d7b0e0edf70a74334194e1a190979f5fc12e9968c3666a981495b33a649814e309366".to_string());
        }

        cookie_parts.join("; ")
    }

    fn eapi_splice(path: &str, json: &str) -> String {
        let marker = "36cd479b6b5";
        let text = format!("nobody{path}use{json}md5forencrypt");
        let digest = format!("{:x}", md5_compute(text.as_bytes()));
        format!("{path}-{marker}-{json}-{marker}-{digest}")
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

    /// Get song details
    pub async fn get_song_detail(&self, song_id: u64) -> Result<SongDetail> {
        if let Some(cached) = self.get_cached_song_detail(song_id) {
            return Ok(cached);
        }

        let url = format!("{}/api/song/detail", self.base_url);
        let mut params = HashMap::new();
        params.insert("id", song_id.to_string());
        params.insert("ids", format!("[{song_id}]"));

        let mut request = self.client.post(url).form(&params);

        // Add MUSIC_U cookie if available
        if let Some(music_u) = &self.music_u {
            request = request.header("Cookie", format!("MUSIC_U={music_u}"));
        }

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

        self.cache_song_detail(song_id, detail.clone());
        Ok(detail)
    }

    /// Get song download URL
    pub async fn get_song_url(&self, song_id: u64, br: u64) -> Result<SongUrl> {
        if let Some(cached) = self.get_cached_song_url(song_id, br) {
            return Ok(cached);
        }

        let url = format!("{}/api/song/enhance/player/url", self.base_url);
        let mut params = HashMap::new();
        params.insert("ids", format!("[{song_id}]"));
        params.insert("br", br.to_string());

        let mut request = self.client.post(url).form(&params);

        if let Some(music_u) = &self.music_u {
            request = request.header("Cookie", format!("MUSIC_U={music_u}"));
        }

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

        self.cache_song_url(song_id, br, song_url.clone());
        Ok(song_url)
    }

    /// Get song details and best available URL using a batch-first strategy with safe fallback.
    pub async fn get_song_detail_and_best_url(
        &self,
        song_id: u64,
        bitrate_candidates: &[u64],
    ) -> Result<(SongDetail, SongUrl)> {
        let Some((&primary_bitrate, _)) = bitrate_candidates.split_first() else {
            return Err(BotError::MusicApi(
                "No bitrate candidates provided".to_string(),
            ));
        };

        let mut cached_detail = self.get_cached_song_detail(song_id);
        if let Some(detail) = cached_detail.clone() {
            for &bitrate in bitrate_candidates {
                if let Some(song_url) = self.get_cached_song_url(song_id, bitrate)
                    && !song_url.url.is_empty()
                {
                    return Ok((detail, song_url));
                }
            }
        }

        let mut primary_url = self.get_cached_song_url(song_id, primary_bitrate);
        if cached_detail.is_none() || primary_url.is_none() {
            if self.batch_circuit_allows_attempt() {
                let batch_start = Instant::now();
                match self
                    .fetch_song_detail_and_url_batch(song_id, primary_bitrate)
                    .await
                {
                    Ok((detail, song_url)) => {
                        self.record_batch_success();
                        self.cache_song_detail(song_id, detail.clone());
                        self.cache_song_url(song_id, primary_bitrate, song_url.clone());
                        cached_detail = Some(detail);
                        primary_url = Some(song_url);
                    }
                    Err(e) => {
                        let breaker_until = self.record_batch_failure();
                        tracing::warn!(
                            "Batch detail+url fetch failed for music_id {} (br={}): {}. Falling back.",
                            song_id,
                            primary_bitrate,
                            e
                        );
                        if let Some(until) = breaker_until {
                            tracing::warn!(
                                "Batch circuit opened until {:?} after consecutive failures",
                                until
                            );
                        }
                    }
                }

                tracing::info!("[batch_attempt] {}ms", batch_start.elapsed().as_millis());
            } else {
                tracing::info!(
                    "Skipping batch detail+url fetch for music_id {} due to open circuit",
                    song_id
                );
            }
        }

        let detail = if let Some(detail) = cached_detail {
            detail
        } else {
            let fallback_detail_start = Instant::now();
            let detail = self.get_song_detail(song_id).await?;
            tracing::info!(
                "[fallback_detail] {}ms",
                fallback_detail_start.elapsed().as_millis()
            );
            detail
        };

        let mut last_error = None;
        let mut fallback_url_start = None;
        for (index, &bitrate) in bitrate_candidates.iter().enumerate() {
            let fetched_url = if index == 0 {
                if let Some(song_url) = primary_url.take() {
                    Ok(song_url)
                } else {
                    fallback_url_start.get_or_insert_with(Instant::now);
                    self.get_song_url(song_id, bitrate).await
                }
            } else {
                fallback_url_start.get_or_insert_with(Instant::now);
                self.get_song_url(song_id, bitrate).await
            };

            match fetched_url {
                Ok(song_url) if !song_url.url.is_empty() => {
                    if let Some(start) = fallback_url_start {
                        tracing::info!("[fallback_url] {}ms", start.elapsed().as_millis());
                    }
                    return Ok((detail.clone(), song_url));
                }
                Ok(_) => {
                    tracing::info!(
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
            tracing::info!("[fallback_url] {}ms", start.elapsed().as_millis());
        }

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

        if let Some(music_u) = &self.music_u {
            request = request.header("Cookie", format!("MUSIC_U={music_u}"));
        }

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
        let path = "/api/v1/search/song/get";
        let url = format!("{}/eapi/v1/search/song/get", self.base_url);
        let payload = serde_json::json!({
            "s": keyword,
            "offset": 0,
            "limit": limit.max(1),
        });
        let payload_str = payload.to_string();
        let body = Self::eapi_params(path, &payload_str)?;
        let request = self
            .client
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("User-Agent", Self::choose_eapi_user_agent())
            .header("Cookie", self.build_eapi_cookie())
            .body(body);

        let response = request.send().await?.error_for_status()?;
        let raw_body = response.text().await?;
        let trimmed = raw_body.trim_start();
        let data: EapiSearchResponse = if trimmed.starts_with('{') {
            serde_json::from_str(trimmed)?
        } else {
            let decrypted = Self::eapi_decrypt(trimmed)?;
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

        let mut request = self.client.get(&processed_url);

        // Add MUSIC_U cookie if available
        if let Some(music_u) = &self.music_u {
            request = request.header("Cookie", format!("MUSIC_U={music_u}"));
        }

        // Add comprehensive headers to avoid 403 errors
        request = request
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36")
            .header("Referer", "https://music.163.com/")
            .header("Accept", "audio/mpeg, audio/*, */*")
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
            .header("Cache-Control", "no-cache")
            .header("DNT", "1")
            .header("Sec-Fetch-Dest", "audio")
            .header("Sec-Fetch-Mode", "cors")
            .header("Sec-Fetch-Site", "cross-site");

        let response = request.send().await?;
        Ok(response)
    }

    /// Resolve final URL for share links with minimal body transfer
    pub async fn resolve_share_link(&self, url: &str) -> Result<reqwest::Url> {
        let response = self
            .client
            .get(url)
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
            )
            .header("Accept", "*/*")
            .header(reqwest::header::RANGE, "bytes=0-0")
            .send()
            .await?
            .error_for_status()?;

        Ok(response.url().clone())
    }

    /// Download and resize album art image
    pub async fn download_album_art(&self, pic_url: &str, output_path: &Path) -> Result<()> {
        let data = self.download_album_art_data(pic_url).await?;
        tokio::fs::write(output_path, data).await?;
        Ok(())
    }

    /// Download and resize album art image into memory
    /// Uses spawn_blocking for CPU-intensive image processing to avoid blocking async runtime
    pub async fn download_album_art_data(&self, pic_url: &str) -> Result<Vec<u8>> {
        if pic_url.is_empty() {
            return Err(BotError::MusicApi("Empty album art URL".to_string()));
        }

        // Download the image
        let mut request = self.client.get(pic_url);

        // Add headers for image download
        request = request
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
            )
            .header("Referer", "https://music.163.com/")
            .header(
                "Accept",
                "image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
            );

        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(BotError::MusicApi(format!(
                "Failed to download album art: {}",
                response.status()
            )));
        }

        let bytes = response.bytes().await?;
        let bytes_vec = bytes.to_vec();

        // Process image in spawn_blocking to avoid blocking async runtime
        let processed =
            tokio::task::spawn_blocking(move || resize_album_art_to_thumbnail(&bytes_vec))
                .await
                .map_err(|e| BotError::MusicApi(format!("Image processing task failed: {e}")))??;

        Ok(processed)
    }

    /// Download original high-resolution album art without resizing (for embedding in audio files)
    pub async fn download_album_art_original(&self, pic_url: &str) -> Result<Vec<u8>> {
        if pic_url.is_empty() {
            return Err(BotError::MusicApi("Empty album art URL".to_string()));
        }

        // Download the image
        let mut request = self.client.get(pic_url);

        // Add headers for image download
        request = request
            .header(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
            )
            .header("Referer", "https://music.163.com/")
            .header(
                "Accept",
                "image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
            );

        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(BotError::MusicApi(format!(
                "Failed to download album art: {}",
                response.status()
            )));
        }

        let bytes = response.bytes().await?;

        Ok(bytes.to_vec())
    }

    async fn fetch_song_detail_and_url_batch(
        &self,
        song_id: u64,
        bitrate: u64,
    ) -> Result<(SongDetail, SongUrl)> {
        let url = format!("{}/api/batch", self.base_url);
        let mut params = HashMap::new();
        params.insert(
            "/api/v3/song/detail".to_string(),
            format!("c=[{{\"id\":{song_id}}}]"),
        );
        params.insert("/api/song/detail".to_string(), format!("ids=[{song_id}]"));
        params.insert(
            "/api/song/enhance/player/url".to_string(),
            format!("ids=[{song_id}]&br={bitrate}"),
        );

        let mut request = self.client.post(url).form(&params);
        if let Some(music_u) = &self.music_u {
            request = request.header("Cookie", format!("MUSIC_U={music_u}"));
        }

        let response = request.send().await?.error_for_status()?;
        let payload: serde_json::Value = response.json().await?;
        let result = parse_batch_detail_and_url(&payload);
        if result.is_err() {
            // One-shot diagnostic: log the payload's top-level structure (keys only, no values)
            // so we can see what the API actually returns and fix the parser.
            let shape = describe_payload_shape(&payload);
            tracing::warn!("Batch parse failed for music_id {song_id}. Payload shape: {shape}");
        }
        result
    }
}

fn find_batch_section<'a>(
    payload: &'a serde_json::Value,
    needle: &str,
) -> Option<&'a serde_json::Value> {
    let section_matches_shape = |section: &serde_json::Value| {
        if needle.contains("/song/detail") {
            return section
                .get("songs")
                .and_then(serde_json::Value::as_array)
                .is_some();
        }
        if needle.contains("/song/enhance/player/url") {
            return section
                .get("data")
                .and_then(serde_json::Value::as_array)
                .is_some();
        }
        false
    };

    let section_payload = |section: &'a serde_json::Value| {
        section
            .get("body")
            .or_else(|| section.get("result"))
            .unwrap_or(section)
    };

    let from_object = |value: &'a serde_json::Value| {
        value.as_object().and_then(|obj| {
            obj.iter().find_map(|(key, section)| {
                if key.contains(needle) {
                    return Some(section);
                }

                let path_match = section
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|path| path.contains(needle));
                if path_match {
                    return Some(section_payload(section));
                }

                let candidate = section_payload(section);
                if section_matches_shape(candidate) {
                    return Some(candidate);
                }

                None
            })
        })
    };

    let from_array = |value: &'a serde_json::Value| {
        value.as_array().and_then(|entries| {
            entries.iter().find_map(|entry| {
                let path_match = entry
                    .get("path")
                    .or_else(|| entry.get("url"))
                    .or_else(|| entry.get("key"))
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|path| path.contains(needle));
                if !path_match {
                    let candidate = section_payload(entry);
                    if section_matches_shape(candidate) {
                        return Some(candidate);
                    }
                    return None;
                }

                Some(section_payload(entry))
            })
        })
    };

    payload
        .get(needle)
        .or_else(|| payload.get("body").and_then(|body| body.get(needle)))
        .or_else(|| from_object(payload))
        .or_else(|| payload.get("body").and_then(from_object))
        .or_else(|| from_array(payload))
        .or_else(|| payload.get("body").and_then(from_array))
        .or_else(|| section_matches_shape(payload).then_some(payload))
        .or_else(|| {
            payload
                .get("body")
                .filter(|body| section_matches_shape(body))
        })
}

/// Describe the shape of a JSON value (keys and types, no actual values) for diagnostics.
fn describe_payload_shape(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let entries: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    let child = match v {
                        serde_json::Value::Object(inner) => {
                            let keys: Vec<&str> = inner.keys().map(String::as_str).collect();
                            format!("obj{{{}}}", keys.join(","))
                        }
                        serde_json::Value::Array(arr) => format!("arr[{}]", arr.len()),
                        serde_json::Value::String(_) => "str".to_string(),
                        serde_json::Value::Number(_) => "num".to_string(),
                        serde_json::Value::Bool(_) => "bool".to_string(),
                        serde_json::Value::Null => "null".to_string(),
                    };
                    format!("{k}:{child}")
                })
                .collect();
            format!("{{{}}}", entries.join(", "))
        }
        serde_json::Value::Array(arr) => format!("arr[{}]", arr.len()),
        other => format!("{other}"),
    }
}

fn merge_code_into_body(section: &serde_json::Value) -> Option<serde_json::Value> {
    let body = section.get("body")?.as_object()?;
    let mut merged = body.clone();

    if !merged.contains_key("code") {
        merged.insert(
            "code".to_string(),
            section
                .get("code")
                .cloned()
                .unwrap_or_else(|| serde_json::Value::from(200)),
        );
    }

    Some(serde_json::Value::Object(merged))
}

fn merge_default_code(section: &serde_json::Value) -> Option<serde_json::Value> {
    let object = section.as_object()?;
    if object.contains_key("code") {
        return None;
    }

    let mut merged = object.clone();
    merged.insert("code".to_string(), serde_json::Value::from(200));
    Some(serde_json::Value::Object(merged))
}

fn parse_batch_response_section<T>(section: &serde_json::Value, section_name: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    let mut candidates: Vec<serde_json::Value> = vec![section.clone()];
    if let Some(merged) = merge_default_code(section) {
        candidates.push(merged);
    }
    if let Some(body) = section.get("body") {
        candidates.push(body.clone());
        if let Some(merged) = merge_default_code(body) {
            candidates.push(merged);
        }
    }
    if let Some(merged) = merge_code_into_body(section) {
        candidates.push(merged);
    }

    for candidate in candidates {
        if let Ok(parsed) = serde_json::from_value::<T>(candidate) {
            return Ok(parsed);
        }
    }

    Err(BotError::MusicApi(format!(
        "Batch {section_name} section has unexpected structure"
    )))
}

fn parse_batch_detail_and_url(payload: &serde_json::Value) -> Result<(SongDetail, SongUrl)> {
    let detail_section = find_batch_section(payload, "/song/detail").ok_or_else(|| {
        BotError::MusicApi("Batch payload missing song detail section".to_string())
    })?;
    let url_section = find_batch_section(payload, "/song/enhance/player/url")
        .ok_or_else(|| BotError::MusicApi("Batch payload missing song URL section".to_string()))?;

    let detail_response: SongDetailResponse =
        parse_batch_response_section(detail_section, "song detail")?;
    let url_response: SongUrlResponse = parse_batch_response_section(url_section, "song URL")?;

    if detail_response.code != 200 {
        return Err(BotError::MusicApi(format!(
            "Batch detail API returned code {}",
            detail_response.code
        )));
    }

    if url_response.code != 200 {
        return Err(BotError::MusicApi(format!(
            "Batch URL API returned code {}",
            url_response.code
        )));
    }

    let detail = detail_response
        .songs
        .into_iter()
        .next()
        .ok_or_else(|| BotError::MusicApi("Batch response has no song detail".to_string()))?;
    let song_url = url_response
        .data
        .into_iter()
        .next()
        .ok_or_else(|| BotError::MusicApi("Batch response has no song URL".to_string()))?;

    Ok((detail, song_url))
}

fn build_http_client(builder: reqwest::ClientBuilder) -> Result<reqwest::Client> {
    builder.build().map_err(|e| {
        tracing::error!("Failed to build HTTP client: {}", e);
        BotError::Network(e)
    })
}

fn rewrite_media_url(url: &str) -> String {
    url.replace("m8.", "m7.")
        .replace("m801.", "m701.")
        .replace("m804.", "m701.")
        .replace("m704.", "m701.")
}

pub fn resize_album_art_to_thumbnail(image_bytes: &[u8]) -> Result<Vec<u8>> {
    let img = image::load_from_memory(image_bytes)
        .map_err(|e| BotError::MusicApi(format!("Failed to decode image: {e}")))?;

    let resized = resize_image_with_padding(img, 320, 320);

    let mut cursor = Cursor::new(Vec::new());
    resized
        .write_to(&mut cursor, ImageFormat::Jpeg)
        .map_err(|e| BotError::MusicApi(format!("Failed to encode image: {e}")))?;

    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::MusicApi;
    use super::build_http_client;

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
    fn song_url_cache_key_includes_bitrate() {
        let low = super::song_url_cache_key(42, 128_000);
        let high = super::song_url_cache_key(42, 320_000);
        assert_ne!(low, high);
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
    fn batch_parser_extracts_detail_and_url() {
        let payload = serde_json::json!({
            "code": 200,
            "body": {
                "/api/song/detail?ids=[123]": {
                    "code": 200,
                    "songs": [
                        {
                            "id": 123,
                            "name": "Song",
                            "dt": 180000,
                            "ar": [{"id": 7, "name": "Artist"}],
                            "al": {"id": 9, "name": "Album", "picUrl": "https://img"}
                        }
                    ]
                },
                "/api/song/enhance/player/url": {
                    "code": 200,
                    "data": [
                        {
                            "id": 123,
                            "url": "https://audio.test/song.flac",
                            "br": 999000,
                            "size": 123456,
                            "md5": "abc",
                            "type": "flac"
                        }
                    ]
                }
            }
        });

        let (detail, song_url) = super::parse_batch_detail_and_url(&payload).expect("parse batch");
        assert_eq!(detail.id, 123);
        assert_eq!(song_url.id, 123);
        assert_eq!(song_url.url, "https://audio.test/song.flac");
        assert_eq!(song_url.br, 999000);
    }

    #[test]
    fn batch_parser_supports_nested_body_sections() {
        let payload = serde_json::json!({
            "code": 200,
            "body": {
                "/api/v3/song/detail": {
                    "code": 200,
                    "body": {
                        "songs": [
                            {
                                "id": 456,
                                "name": "Nested Song",
                                "dt": 200000,
                                "ar": [{"id": 8, "name": "Nested Artist"}],
                                "al": {"id": 10, "name": "Nested Album", "picUrl": "https://img2"}
                            }
                        ]
                    }
                },
                "/api/song/enhance/player/url": {
                    "code": 200,
                    "body": {
                        "data": [
                            {
                                "id": 456,
                                "url": "https://audio.test/song2.flac",
                                "br": 999000,
                                "size": 223344,
                                "md5": "def",
                                "type": "flac"
                            }
                        ]
                    }
                }
            }
        });

        let (detail, song_url) =
            super::parse_batch_detail_and_url(&payload).expect("parse nested batch");
        assert_eq!(detail.id, 456);
        assert_eq!(song_url.id, 456);
        assert_eq!(song_url.url, "https://audio.test/song2.flac");
    }

    #[test]
    fn batch_parser_supports_array_body_entries() {
        let payload = serde_json::json!({
            "code": 200,
            "body": [
                {
                    "path": "/api/v3/song/detail",
                    "body": {
                        "code": 200,
                        "songs": [
                            {
                                "id": 789,
                                "name": "Array Song",
                                "dt": 180000,
                                "ar": [{"id": 9, "name": "Array Artist"}],
                                "al": {"id": 11, "name": "Array Album", "picUrl": "https://img3"}
                            }
                        ]
                    }
                },
                {
                    "path": "/api/song/enhance/player/url",
                    "body": {
                        "code": 200,
                        "data": [
                            {
                                "id": 789,
                                "url": "https://audio.test/song3.flac",
                                "br": 320000,
                                "size": 998877,
                                "md5": "ghi",
                                "type": "mp3"
                            }
                        ]
                    }
                }
            ]
        });

        let (detail, song_url) =
            super::parse_batch_detail_and_url(&payload).expect("parse array batch");
        assert_eq!(detail.id, 789);
        assert_eq!(song_url.id, 789);
        assert_eq!(song_url.url, "https://audio.test/song3.flac");
    }

    #[test]
    fn batch_parser_supports_array_entries_without_path() {
        let payload = serde_json::json!({
            "code": 200,
            "body": [
                {
                    "code": 200,
                    "body": {
                        "songs": [
                            {
                                "id": 321,
                                "name": "Shape Song",
                                "dt": 123000,
                                "ar": [{"id": 77, "name": "Shape Artist"}],
                                "al": {"id": 88, "name": "Shape Album", "picUrl": "https://img-shape"}
                            }
                        ]
                    }
                },
                {
                    "code": 200,
                    "body": {
                        "data": [
                            {
                                "id": 321,
                                "url": "https://audio.test/shape.flac",
                                "br": 999000,
                                "size": 888777,
                                "md5": "shape",
                                "type": "flac"
                            }
                        ]
                    }
                }
            ]
        });

        let (detail, song_url) =
            super::parse_batch_detail_and_url(&payload).expect("parse shape-only array batch");
        assert_eq!(detail.id, 321);
        assert_eq!(song_url.id, 321);
        assert_eq!(song_url.url, "https://audio.test/shape.flac");
    }

    #[test]
    fn batch_circuit_breaker_opens_after_repeated_failures() {
        let api = MusicApi::new(None, "https://music.163.com".to_string());
        let now = std::time::Instant::now();

        assert!(api.batch_circuit_allows_attempt_at(now));
        assert!(api.record_batch_failure_at(now).is_none());
        assert!(api.record_batch_failure_at(now).is_none());
        let opened_until = api
            .record_batch_failure_at(now)
            .expect("circuit should open at threshold");
        assert!(opened_until > now);
        assert!(!api.batch_circuit_allows_attempt_at(now));

        api.record_batch_success();
        assert!(api.batch_circuit_allows_attempt_at(now));
    }
}

/// Parse artists into a formatted string
#[must_use]
pub fn format_artists(artists: &[Artist]) -> String {
    artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

/// Resize image with black padding to maintain aspect ratio (like the original Go project)
fn resize_image_with_padding(
    img: DynamicImage,
    target_width: u32,
    target_height: u32,
) -> DynamicImage {
    use image::{Rgb, RgbImage};

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
    for pixel in canvas.pixels_mut() {
        *pixel = Rgb([0, 0, 0]); // Black background
    }

    // Calculate position to center the resized image
    let offset_x = (target_width - new_width) / 2;
    let offset_y = (target_height - new_height) / 2;

    // Convert resized image to RGB and overlay on canvas
    let resized_rgb = resized.to_rgb8();
    for (x, y, pixel) in resized_rgb.enumerate_pixels() {
        if x + offset_x < target_width && y + offset_y < target_height {
            canvas.put_pixel(x + offset_x, y + offset_y, *pixel);
        }
    }

    DynamicImage::ImageRgb8(canvas)
}
