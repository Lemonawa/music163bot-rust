use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use aes::Aes128;
use cipher::{BlockDecryptMut, BlockEncryptMut, KeyInit, block_padding::Pkcs7};
use dashmap::DashMap;
use ecb::{Decryptor, Encryptor};
use hex::encode_upper;
use md5::compute as md5_compute;
use reqwest::Client;
use tokio::time::Duration;
use uuid::Uuid;

use super::shared::song_url_has_download_url;
use super::{
    ALBUM_ART_DOWNLOAD_TOTAL_ATTEMPTS, BROWSER_USER_AGENT, CachePruneStats, MusicApi,
    SHORT_USER_AGENT, SONG_DETAIL_CACHE_TTL, SONG_LYRIC_CACHE_TTL, SONG_URL_CACHE_TTL, SongDetail,
    SongUrl, TimedCacheEntry, song_url_cache_key,
};
use crate::config::Config;
use crate::error::{BotError, Result};
use crate::utils::build_http_client;

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
        let client = build_music_api_http_client(
            pool_max_idle_per_host,
            connect_timeout_secs,
            request_timeout_secs,
            false,
        )
        .unwrap_or_else(|e| {
            tracing::error!(
                "Failed to build HTTP client: {}",
                crate::utils::sanitize_sensitive_text(&e.to_string())
            );
            Client::new()
        });
        let resolve_client = build_music_api_http_client(
            pool_max_idle_per_host,
            connect_timeout_secs,
            request_timeout_secs,
            true,
        )
        .unwrap_or_else(|e| {
            tracing::error!(
                "Failed to build share-link resolve client: {}",
                crate::utils::sanitize_sensitive_text(&e.to_string())
            );
            build_redirect_disabled_fallback_client()
        });

        let eapi_cookie = Self::generate_eapi_cookie(music_u.as_deref());
        let music_u_cookie = music_u.as_ref().map(|u| format!("MUSIC_U={u}"));

        Self {
            client,
            resolve_client,
            music_u,
            base_url,
            eapi_cookie,
            music_u_cookie,
            song_detail_cache: DashMap::new(),
            song_url_cache: DashMap::new(),
            song_lyric_cache: DashMap::new(),
        }
    }

    pub(super) fn album_art_total_attempts() -> u32 {
        ALBUM_ART_DOWNLOAD_TOTAL_ATTEMPTS
    }

    pub(super) fn get_cached_song_detail(&self, song_id: u64) -> Option<Arc<SongDetail>> {
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
    pub(super) fn cache_song_detail(&self, song_id: u64, detail: SongDetail) {
        self.cache_song_detail_shared(song_id, Arc::new(detail));
    }

    pub(super) fn cache_song_detail_shared(&self, song_id: u64, detail: Arc<SongDetail>) {
        self.song_detail_cache
            .insert(song_id, TimedCacheEntry::new(detail, SONG_DETAIL_CACHE_TTL));
    }

    pub(super) fn get_cached_song_url(&self, song_id: u64, br: u64) -> Option<Arc<SongUrl>> {
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

    pub(super) fn get_first_cached_song_url(
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
    pub(super) fn cache_song_url(&self, song_id: u64, br: u64, song_url: SongUrl) {
        self.cache_song_url_shared(song_id, br, Arc::new(song_url));
    }

    pub(super) fn cache_song_url_shared(&self, song_id: u64, br: u64, song_url: Arc<SongUrl>) {
        let key = song_url_cache_key(song_id, br);
        self.song_url_cache
            .insert(key, TimedCacheEntry::new(song_url, SONG_URL_CACHE_TTL));
    }

    pub(super) fn get_cached_song_lyric(&self, song_id: u64) -> Option<String> {
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

    pub(super) fn cache_song_lyric(&self, song_id: u64, lyric: String) {
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

    pub(super) fn build_eapi_cookie(&self) -> &str {
        &self.eapi_cookie
    }

    /// Conditionally add the pre-computed MUSIC_U cookie header to a request.
    pub(super) fn apply_music_u_cookie(
        &self,
        request: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
        if let Some(cookie) = &self.music_u_cookie {
            request.header("Cookie", cookie)
        } else {
            request
        }
    }

    /// Build common headers for image downloads (album art).
    pub(super) fn apply_image_download_headers(
        request: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
        request
            .header("User-Agent", SHORT_USER_AGENT)
            .header("Referer", "https://music.163.com/")
            .header(
                "Accept",
                "image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
            )
    }

    /// Build common headers for audio file downloads.
    pub(super) fn apply_audio_download_headers(
        request: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
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

    pub(super) fn eapi_encrypt_with_key(data: &str, key: &[u8]) -> Result<String> {
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

    pub(super) fn eapi_decrypt(hex_data: &str) -> Result<String> {
        Self::eapi_decrypt_with_key(hex_data, b"e82ckenh8dichen8")
    }

    pub(super) fn eapi_decrypt_with_key(hex_data: &str, key: &[u8]) -> Result<String> {
        let mut bytes = hex::decode(hex_data).map_err(|e| BotError::MusicApi(e.to_string()))?;
        let decrypted = Decryptor::<Aes128>::new_from_slice(key)
            .map_err(|_| BotError::MusicApi("Invalid eapi key length".to_string()))?
            .decrypt_padded_mut::<Pkcs7>(&mut bytes)
            .map_err(|e| BotError::MusicApi(e.to_string()))?;
        String::from_utf8(decrypted.to_vec()).map_err(|e| BotError::MusicApi(e.to_string()))
    }

    pub(super) fn eapi_params(path: &str, json: &str) -> Result<String> {
        let data = Self::eapi_splice(path, json);
        let encrypted = Self::eapi_encrypt(&data)?;
        Ok(format!("params={encrypted}"))
    }

    pub(super) fn choose_eapi_user_agent() -> &'static str {
        "NeteaseMusic/9.3.40.1753206443(164);Dalvik/2.1.0 (Linux; U; Android 9; MIX 2 MIUI/V12.0.1.0.PDECNXM)"
    }
}

fn build_music_api_http_client(
    pool_max_idle_per_host: usize,
    connect_timeout_secs: u64,
    request_timeout_secs: u64,
    disable_redirects: bool,
) -> Result<Client> {
    let mut builder = Client::builder();

    builder = builder.use_rustls_tls();
    builder = builder
        .tcp_nodelay(true)
        .pool_max_idle_per_host(pool_max_idle_per_host)
        .connect_timeout(Duration::from_secs(connect_timeout_secs))
        .timeout(Duration::from_secs(request_timeout_secs.max(1)))
        .user_agent(BROWSER_USER_AGENT);
    if disable_redirects {
        builder = builder.redirect(reqwest::redirect::Policy::none());
    }

    build_http_client(builder)
}

fn build_redirect_disabled_fallback_client() -> Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("failed to build redirect-disabled HTTP client")
}
