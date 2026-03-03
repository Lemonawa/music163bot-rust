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
