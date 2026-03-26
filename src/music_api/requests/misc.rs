use bytes::Bytes;
use futures_util::StreamExt;
use serde::Serialize;

use super::super::{
    EapiSearchResponse, LyricResponse, MusicApi, Result, SHORT_USER_AGENT, SearchSong,
    resize_album_art_to_thumbnail, rewrite_media_url,
};
use crate::error::BotError;
use crate::utils::{is_trusted_music_share_url, sanitize_sensitive_text};

impl MusicApi {
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
        // Apply host replacement similar to the original Go project.
        // VPS sampling shows the original m704/m804 hosts can return 403 while m701 succeeds.
        let processed_url = rewrite_media_url(url);
        let response = self
            .build_audio_download_request(processed_url.as_ref())
            .send()
            .await?;
        Ok(response)
    }

    /// Resolve final URL for share links with minimal body transfer
    pub async fn resolve_share_link(&self, url: &str) -> Result<reqwest::Url> {
        if !is_trusted_music_share_url(url) {
            return Err(BotError::MusicApi("Untrusted share-link host".to_string()));
        }

        let mut current = reqwest::Url::parse(url)
            .map_err(|e| BotError::MusicApi(format!("Invalid share-link URL: {e}")))?;

        for _ in 0..5 {
            let response = self
                .resolve_client
                .get(current.clone())
                .header("User-Agent", SHORT_USER_AGENT)
                .header("Accept", "*/*")
                .header(reqwest::header::RANGE, "bytes=0-0")
                .send()
                .await?;

            let status = response.status();
            if matches!(
                status,
                reqwest::StatusCode::MOVED_PERMANENTLY
                    | reqwest::StatusCode::FOUND
                    | reqwest::StatusCode::SEE_OTHER
                    | reqwest::StatusCode::TEMPORARY_REDIRECT
                    | reqwest::StatusCode::PERMANENT_REDIRECT
            ) {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .ok_or_else(|| {
                        BotError::MusicApi("Missing Location header in redirect".to_string())
                    })?
                    .to_str()
                    .map_err(|e| BotError::MusicApi(format!("Invalid redirect location: {e}")))?;
                let next = current
                    .join(location)
                    .map_err(|e| BotError::MusicApi(format!("Invalid redirect target URL: {e}")))?;
                if !is_trusted_music_share_url(next.as_str()) {
                    return Err(BotError::MusicApi(
                        "Untrusted share-link redirect host".to_string(),
                    ));
                }
                current = next;
                continue;
            }
            if status.is_redirection() {
                return Err(BotError::MusicApi(format!(
                    "Unsupported redirect status without Location handling: {status}"
                )));
            }

            response.error_for_status_ref()?;
            let final_url = response.url().clone();
            if !is_trusted_music_share_url(final_url.as_str()) {
                return Err(BotError::MusicApi(
                    "Untrusted share-link final host".to_string(),
                ));
            }
            return Ok(final_url);
        }

        Err(BotError::MusicApi(
            "Too many share-link redirects".to_string(),
        ))
    }

    /// Download and resize album art image into memory
    /// Uses spawn_blocking for CPU-intensive image processing to avoid blocking async runtime
    pub async fn download_album_art_data(&self, pic_url: &str, max_bytes: u64) -> Result<Vec<u8>> {
        let total_attempts = Self::album_art_total_attempts();
        let mut last_error = None;

        for attempt in 1..=total_attempts {
            match self.download_album_art_data_once(pic_url, max_bytes).await {
                Ok(data) => return Ok(data),
                Err(e) => {
                    if attempt < total_attempts {
                        tracing::warn!(
                            "Album art download attempt {}/{} failed for {}: {}",
                            attempt,
                            total_attempts,
                            sanitize_sensitive_text(pic_url),
                            sanitize_sensitive_text(&e.to_string())
                        );
                    }
                    last_error = Some(e);
                }
            }
        }

        Err(last_error
            .unwrap_or_else(|| BotError::MusicApi("Album art download failed".to_string())))
    }

    pub async fn download_album_art_original(
        &self,
        pic_url: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>> {
        if pic_url.is_empty() {
            return Err(BotError::MusicApi("Empty album art URL".to_string()));
        }

        let total_attempts = Self::album_art_total_attempts();
        let mut last_error = None;

        for attempt in 1..=total_attempts {
            match self.download_image_with_limit(pic_url, max_bytes).await {
                Ok(data) => return Ok(data),
                Err(e) => {
                    if attempt < total_attempts {
                        tracing::warn!(
                            "Original album art download attempt {}/{} failed for {}: {}",
                            attempt,
                            total_attempts,
                            sanitize_sensitive_text(pic_url),
                            sanitize_sensitive_text(&e.to_string())
                        );
                    }
                    last_error = Some(e);
                }
            }
        }

        Err(last_error
            .unwrap_or_else(|| BotError::MusicApi("Album art download failed".to_string())))
    }

    async fn download_album_art_data_once(&self, pic_url: &str, max_bytes: u64) -> Result<Vec<u8>> {
        // Download the image with common headers
        let bytes = self.download_image_with_limit(pic_url, max_bytes).await?;

        // Process image in spawn_blocking to avoid blocking async runtime
        let processed = self.build_thumbnail_from_bytes(Bytes::from(bytes)).await?;

        Ok(processed)
    }

    async fn download_image_with_limit(&self, pic_url: &str, max_bytes: u64) -> Result<Vec<u8>> {
        if pic_url.is_empty() {
            return Err(BotError::MusicApi("Empty album art URL".to_string()));
        }

        let request = self.client.get(pic_url);
        let request = Self::apply_image_download_headers(request);

        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(BotError::MusicApi(format!(
                "Failed to download album art: {}",
                response.status()
            )));
        }

        let limit_mb = max_bytes.div_ceil(1024 * 1024).max(1);
        let mut data = if let Some(len) = response.content_length() {
            if len == 0 {
                return Err(BotError::MusicApi("Album art is empty".to_string()));
            }
            if len > max_bytes {
                return Err(BotError::MusicApi(format!(
                    "Album art too large ({len} bytes > {limit_mb} MB limit)"
                )));
            }
            let reserve_len = std::cmp::min(len, max_bytes);
            if let Ok(capacity) = usize::try_from(reserve_len) {
                Vec::with_capacity(capacity)
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            let next_size = data
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| BotError::MusicApi("Album art size overflow".to_string()))?;
            if (next_size as u64) > max_bytes {
                return Err(BotError::MusicApi(format!(
                    "Album art too large (exceeded {limit_mb} MB limit)"
                )));
            }
            data.extend_from_slice(&chunk);
        }

        if data.is_empty() {
            return Err(BotError::MusicApi(
                "Album art download returned empty body".to_string(),
            ));
        }

        Ok(data)
    }

    pub(crate) async fn build_thumbnail_from_bytes(&self, bytes: Bytes) -> Result<Vec<u8>> {
        tokio::task::spawn_blocking(move || resize_album_art_to_thumbnail(&bytes))
            .await
            .map_err(|e| BotError::MusicApi(format!("Image processing task failed: {e}")))?
    }

    fn build_audio_download_request(&self, url: &str) -> reqwest::RequestBuilder {
        let request = self.client.get(url);
        let request = self.apply_music_u_cookie(request);
        Self::apply_audio_download_headers(request)
    }
}
