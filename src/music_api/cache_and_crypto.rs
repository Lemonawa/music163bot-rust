use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use reqwest::Client;
use tokio::time::Duration;

use super::eapi_crypto;
use super::{
    ALBUM_ART_DOWNLOAD_TOTAL_ATTEMPTS, BROWSER_USER_AGENT, CachePruneStats,
    MUSIC_API_CACHE_MAX_ENTRIES, MusicApi, SHORT_USER_AGENT, SONG_DETAIL_CACHE_TTL,
    SONG_LYRIC_CACHE_TTL, SONG_URL_CACHE_TTL, SongDetail, SongUrl, TimedCacheEntry,
    song_url_cache_key,
};
use crate::config::Config;
use crate::error::Result;
use crate::utils::build_http_client;

fn enforce_cache_capacity<K, V>(cache: &DashMap<K, TimedCacheEntry<V>>, max_entries: usize)
where
    K: std::hash::Hash + Eq + Clone,
{
    if max_entries == 0 || cache.len() < max_entries {
        return;
    }

    let now = Instant::now();
    cache.retain(|_, entry| entry.is_fresh_at(now));

    while cache.len() >= max_entries {
        let oldest_key = cache
            .iter()
            .min_by_key(|entry| entry.value().created_at())
            .map(|entry| entry.key().clone());

        match oldest_key {
            Some(key) => {
                cache.remove(&key);
            }
            None => break,
        }
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
            build_fallback_client()
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

        let eapi_cookie = eapi_crypto::eapi_cookie(music_u.as_deref());
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
        enforce_cache_capacity(&self.song_detail_cache, MUSIC_API_CACHE_MAX_ENTRIES);
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
                && song_url.has_download_url()
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
        enforce_cache_capacity(&self.song_url_cache, MUSIC_API_CACHE_MAX_ENTRIES);
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
        enforce_cache_capacity(&self.song_lyric_cache, MUSIC_API_CACHE_MAX_ENTRIES);
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

    pub(super) fn build_eapi_cookie(&self) -> &str {
        &self.eapi_cookie
    }

    /// Conditionally add the pre-computed `MUSIC_U` cookie header to a request.
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

fn build_fallback_client() -> Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_mins(1))
        .build()
        .expect("failed to build fallback HTTP client")
}

fn build_redirect_disabled_fallback_client() -> Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_mins(1))
        .build()
        .expect("failed to build redirect-disabled HTTP client")
}
