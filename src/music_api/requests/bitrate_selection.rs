use std::sync::Arc;
use std::time::Instant;

use tokio::time::Duration;

use super::super::{MusicApi, PERF_API_LOG_PREFIX, Result, SongDetail, SongUrl};
use super::{
    fallback_bitrate_candidates as request_fallback_bitrate_candidates,
    log_music_api_perf as request_log_music_api_perf,
    song_url_has_download_url as request_song_url_has_download_url,
};
use crate::error::BotError;

pub(super) fn fallback_bitrate_candidates_impl(
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

pub(super) fn log_music_api_perf_impl(song_id: u64, stage: &str, duration: Duration) {
    tracing::debug!(
        "{PERF_API_LOG_PREFIX}|music_id={song_id}|stage={stage}|elapsed_ms={}",
        duration.as_millis()
    );
}

impl MusicApi {
    /// # Errors
    /// Returns an error if song detail or download URL cannot be obtained.
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
            request_log_music_api_perf(
                song_id,
                "select_url_total",
                select_url_total_start.elapsed(),
            );
            return Ok((Arc::clone(detail), song_url));
        }

        let mut primary_url = self.get_cached_song_url(song_id, primary_bitrate);
        let mut primary_attempted_unavailable = false;

        if cached_detail.is_none() || primary_url.is_none() {
            let (detail_out, url_out, unavailable) = self
                .fetch_detail_and_primary_url(
                    song_id,
                    primary_bitrate,
                    cached_detail.is_none(),
                    primary_url.is_none(),
                )
                .await?;
            if let Some(d) = detail_out {
                cached_detail = Some(d);
            }
            if let Some(u) = url_out {
                primary_url = Some(u);
            }
            primary_attempted_unavailable = unavailable;
        }

        let Some(detail) = cached_detail else {
            request_log_music_api_perf(
                song_id,
                "select_url_total",
                select_url_total_start.elapsed(),
            );
            return Err(BotError::MusicApi(format!(
                "Failed to get song detail for {song_id}"
            )));
        };

        self.try_fallback_bitrates(
            song_id,
            bitrate_candidates,
            primary_attempted_unavailable,
            primary_url,
            &detail,
            select_url_total_start,
        )
        .await
    }

    async fn fetch_detail_and_primary_url(
        &self,
        song_id: u64,
        primary_bitrate: u64,
        need_detail: bool,
        need_url: bool,
    ) -> Result<(Option<Arc<SongDetail>>, Option<Arc<SongUrl>>, bool)> {
        let parallel_start = Instant::now();
        let mut primary_attempted_unavailable = false;

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

        let mut fetched_detail = None;
        if let Some(result) = detail_result {
            fetched_detail = Some(result?);
        }

        let mut fetched_url = None;
        if let Some(result) = url_result {
            match result {
                Ok(song_url) if request_song_url_has_download_url(&song_url) => {
                    fetched_url = Some(song_url);
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
                        "Primary bitrate {primary_bitrate} request failed for music_id {song_id}: {}",
                        crate::utils::sanitize_sensitive_text(&e.to_string())
                    );
                }
            }
        }

        tracing::debug!(
            "[parallel_fetch] {}ms (detail={need_detail}, url={need_url})",
            parallel_start.elapsed().as_millis()
        );
        request_log_music_api_perf(song_id, "parallel_fetch", parallel_start.elapsed());
        Ok((fetched_detail, fetched_url, primary_attempted_unavailable))
    }

    async fn try_fallback_bitrates(
        &self,
        song_id: u64,
        bitrate_candidates: &[u64],
        primary_attempted_unavailable: bool,
        mut primary_url: Option<Arc<SongUrl>>,
        detail: &Arc<SongDetail>,
        select_url_total_start: Instant,
    ) -> Result<(Arc<SongDetail>, Arc<SongUrl>)> {
        let primary_bitrate = bitrate_candidates.first().copied().unwrap_or(0);
        let mut last_error = None;
        let mut fallback_url_start = None;
        for &bitrate in
            request_fallback_bitrate_candidates(bitrate_candidates, primary_attempted_unavailable)
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
                Ok(song_url) if request_song_url_has_download_url(&song_url) => {
                    log_url_selection_completion(
                        song_id,
                        fallback_url_start,
                        select_url_total_start,
                    );
                    return Ok((Arc::clone(detail), song_url));
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
                        crate::utils::sanitize_sensitive_text(&e.to_string())
                    );
                    last_error = Some(e);
                }
            }
        }

        log_url_selection_completion(song_id, fallback_url_start, select_url_total_start);
        if let Some(e) = last_error {
            Err(e)
        } else {
            Err(BotError::MusicApi("No download URL found".to_string()))
        }
    }
}

fn log_url_selection_completion(
    song_id: u64,
    fallback_url_start: Option<Instant>,
    select_url_total_start: Instant,
) {
    if let Some(start) = fallback_url_start {
        let fallback_duration = start.elapsed();
        tracing::debug!("[fallback_url] {}ms", fallback_duration.as_millis());
        request_log_music_api_perf(song_id, "fallback_url", fallback_duration);
    }

    request_log_music_api_perf(
        song_id,
        "select_url_total",
        select_url_total_start.elapsed(),
    );
}
