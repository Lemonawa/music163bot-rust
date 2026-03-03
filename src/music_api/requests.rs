impl MusicApi {
    fn map_program_main_track(program: DjProgramItem) -> Result<ProgramMainTrack> {
        let main_track_id = program
            .main_track_id
            .filter(|id| *id > 0)
            .ok_or_else(|| BotError::MusicApi("No mainTrackId found for program".to_string()))?;
        let author_name = program
            .dj
            .as_ref()
            .map_or_else(String::new, |dj| dj.nickname.clone());
        let radio_name = program
            .radio
            .as_ref()
            .map_or_else(String::new, |radio| radio.name.clone());

        Ok(ProgramMainTrack {
            program_id: program.id,
            main_track_id,
            program_name: program.name,
            author_name,
            radio_name,
            cover_url: program.cover_url.filter(|url| !url.is_empty()),
        })
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
    pub async fn get_song_detail(&self, song_id: u64) -> Result<Arc<SongDetail>> {
        self.get_song_detail_shared(song_id).await
    }

    /// Get all song IDs from a playlist.
    pub async fn get_playlist_song_ids(&self, playlist_id: u64) -> Result<Vec<u64>> {
        let url = format!("{}/api/v6/playlist/detail", self.base_url);
        let playlist_id_str = playlist_id.to_string();
        let mut request = self.client.post(url).form(&[
            ("id", playlist_id_str.as_str()),
            ("n", "10000"),
            ("s", "0"),
        ]);
        request = self.apply_music_u_cookie(request);

        let response = request.send().await?.error_for_status()?;
        let data: PlaylistDetailResponse = response.json().await?;

        if data.code != 200 {
            return Err(BotError::MusicApi(format!(
                "API returned code {}",
                data.code
            )));
        }

        let playlist = data
            .playlist
            .ok_or_else(|| BotError::MusicApi("No playlist data found".to_string()))?;
        Ok(playlist
            .track_ids
            .into_iter()
            .map(|track| track.id)
            .collect())
    }

    /// Get all song IDs from an album.
    pub async fn get_album_song_ids(&self, album_id: u64) -> Result<Vec<u64>> {
        let url = format!("{}/api/v1/album/{}", self.base_url, album_id);
        let mut request = self.client.get(url);
        request = self.apply_music_u_cookie(request);

        let response = request.send().await?.error_for_status()?;
        let data: AlbumSongsResponse = response.json().await?;

        if data.code != 200 {
            return Err(BotError::MusicApi(format!(
                "API returned code {}",
                data.code
            )));
        }

        Ok(data.songs.into_iter().map(|song| song.id).collect())
    }

    /// Get main track metadata from a program.
    pub async fn get_program_main_track(&self, program_id: u64) -> Result<ProgramMainTrack> {
        let url = format!("{}/api/dj/program/detail?id={}", self.base_url, program_id);
        let mut request = self.client.get(url);
        request = self.apply_music_u_cookie(request);

        let response = request.send().await?.error_for_status()?;
        let data: DjProgramDetailResponse = response.json().await?;

        if data.code != 200 {
            return Err(BotError::MusicApi(format!(
                "API returned code {}",
                data.code
            )));
        }

        let program = data
            .program
            .ok_or_else(|| BotError::MusicApi("No program data found".to_string()))?;
        Self::map_program_main_track(program)
    }

    /// Get latest program main tracks from a djradio.
    pub async fn get_djradio_program_main_tracks(
        &self,
        radio_id: u64,
        limit: usize,
    ) -> Result<(usize, Vec<ProgramMainTrack>)> {
        let limit = limit.max(1);
        let url = format!(
            "{}/api/dj/program/byradio?radioId={}&limit={}&offset=0&asc=false",
            self.base_url, radio_id, limit
        );
        let mut request = self
            .client
            .get(url)
            .header("Referer", "https://music.163.com/");
        request = self.apply_music_u_cookie(request);

        let response = request.send().await?.error_for_status()?;
        let data: DjProgramListResponse = response.json().await?;

        if data.code != 200 {
            return Err(BotError::MusicApi(format!(
                "API returned code {}",
                data.code
            )));
        }

        let mut tracks = Vec::with_capacity(data.programs.len());
        for program in data.programs {
            if let Ok(track) = Self::map_program_main_track(program) {
                tracks.push(track);
            }
        }
        Ok((data.count, tracks))
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
