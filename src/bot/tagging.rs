//! Audio enrichment before upload: ID3/FLAC tagging dispatch, buffer
//! cleanup, and the raw-upload client bundle the sender consumes.

use crate::error::{BotError, Result};

use std::sync::Arc;

use super::wiring::{AudioFormat, UploadClientState};
use crate::audio_buffer::{AudioBuffer, ThumbnailBuffer};
use crate::telegram::TelegramBot as Bot;

pub(super) async fn apply_tags_in_blocking(
    mut audio_buffer: AudioBuffer,
    audio_format: AudioFormat,
    song_detail: Arc<crate::music_api::SongDetail>,
    artwork_data: Option<bytes::Bytes>,
    embed_cover: bool,
) -> Result<AudioBuffer> {
    tokio::task::spawn_blocking(move || {
        let embed_artwork = if embed_cover {
            artwork_data.as_ref().map(std::convert::AsRef::as_ref)
        } else {
            None
        };

        match audio_format {
            AudioFormat::Mp3 => {
                let cover_label = if embed_cover { "320" } else { "none" };
                tracing::debug!("Adding ID3 tags to MP3 (cover: {})", cover_label);
                match audio_buffer.add_id3_tags(&song_detail, embed_artwork) {
                    Ok(()) => tracing::debug!("MP3 tags added successfully"),
                    Err(e) => tracing::warn!("Failed to add MP3 tags: {}", e),
                }
            }
            AudioFormat::Flac => {
                let cover_label = if embed_cover { "320" } else { "none" };
                tracing::debug!("Adding FLAC metadata (cover: {})", cover_label);
                match audio_buffer.add_flac_metadata(&song_detail, embed_artwork) {
                    Ok(()) => tracing::debug!("FLAC metadata added successfully"),
                    Err(e) => tracing::warn!("Failed to add FLAC metadata: {}", e),
                }
            }
        }

        audio_buffer
    })
    .await
    .map_err(|e| BotError::Other(anyhow::anyhow!("metadata task join failed: {e}")))
}

pub(super) async fn cleanup_audio_buffer(buffer: AudioBuffer) {
    if let Err(e) = buffer.cleanup().await {
        tracing::warn!("Audio buffer cleanup failed: {}", e);
    }
}

pub(super) async fn cleanup_thumbnail_buffer(buffer: Option<ThumbnailBuffer>) {
    if let Some(thumbnail) = buffer
        && let Err(e) = thumbnail.cleanup().await
    {
        tracing::warn!("Thumbnail cleanup failed: {}", e);
    }
}

pub(super) fn should_refresh_upload_client(
    upload_state: &UploadClientState,
    reuse_limit: u32,
) -> bool {
    upload_state.raw_client.is_none()
        || (reuse_limit != 0 && upload_state.reuse_count >= reuse_limit)
}

pub(super) struct UploadBotBundle {
    pub(super) bot: Option<Bot>,
    pub(super) raw_client: reqwest::Client,
    /// Full API base URL including bot token, e.g. "http://host:port/bot<TOKEN>/"
    pub(super) api_base_url: String,
}

pub(super) struct RawUploadParams<'a> {
    pub(super) chat_id: i64,
    pub(super) caption: &'a str,
    pub(super) reply_to_message_id: i32,
    pub(super) reply_markup_json: Option<String>,
    pub(super) title: Option<&'a str>,
    pub(super) performer: Option<&'a str>,
    pub(super) duration: Option<u32>,
    pub(super) thumbnail: Option<&'a ThumbnailBuffer>,
}

pub(super) const RAW_UPLOAD_CHUNK_SIZE: usize = 256 * 1024;

pub(super) fn log_perf(label: &str, duration: std::time::Duration) {
    tracing::debug!("[{label}] {}ms", duration.as_millis());
    tracing::debug!("PERF_RAW|stage={label}|elapsed_ms={}", duration.as_millis());
}

#[cfg(test)]
pub(super) fn format_perf(label: &str, duration: std::time::Duration) -> String {
    format!("[{label}] {}ms", duration.as_millis())
}
