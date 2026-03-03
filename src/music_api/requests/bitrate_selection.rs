impl MusicApi {
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
}
