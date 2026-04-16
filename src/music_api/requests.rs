mod bitrate_selection;
mod collections;
mod core_detail;
mod misc;

use tokio::time::Duration;

use super::SongUrl;
use super::shared;
use crate::error::BotError;

/// Construct the standard "API returned non-200 code" error.
pub(super) fn api_code_error(code: i32) -> BotError {
    BotError::MusicApi(format!("API returned code {code}"))
}

pub(super) fn fallback_bitrate_candidates(
    bitrate_candidates: &[u64],
    primary_attempted_unavailable: bool,
) -> &[u64] {
    bitrate_selection::fallback_bitrate_candidates_impl(
        bitrate_candidates,
        primary_attempted_unavailable,
    )
}

pub(super) fn song_url_has_download_url(song_url: &SongUrl) -> bool {
    shared::song_url_has_download_url(song_url)
}

pub(super) fn log_music_api_perf(song_id: u64, stage: &str, duration: Duration) {
    bitrate_selection::log_music_api_perf_impl(song_id, stage, duration);
}
