use std::sync::Arc;

use super::super::eapi_crypto;
use super::super::{
    DjProgramItem, MusicApi, ProgramMainTrack, Result, SongDetail, SongDetailResponse, SongUrl,
    SongUrlResponse,
};
use super::bitrate_selection::bitrate_to_eapi_level;
use crate::error::BotError;

impl MusicApi {
    pub(super) fn map_program_main_track(program: DjProgramItem) -> Result<ProgramMainTrack> {
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

    pub(super) async fn get_song_detail_shared(&self, song_id: u64) -> Result<Arc<SongDetail>> {
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

    pub(super) async fn get_song_url_shared(&self, song_id: u64, br: u64) -> Result<Arc<SongUrl>> {
        if let Some(cached) = self.get_cached_song_url(song_id, br) {
            return Ok(cached);
        }

        let path = "/api/song/enhance/player/url/v1";
        let url = format!("{}/eapi/song/enhance/player/url/v1", self.base_url);
        let ids_str = format!("[{song_id}]");
        let payload = serde_json::json!({
            "ids": ids_str,
            "level": bitrate_to_eapi_level(br),
            "encodeType": "mp3",
            "header": "{}",
        });
        let payload_str = serde_json::to_string(&payload)?;
        let body = eapi_crypto::eapi_params(path, &payload_str)
            .map_err(|e| BotError::MusicApi(e.to_string()))?;

        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("User-Agent", eapi_crypto::EAPI_USER_AGENT)
            .header("Cookie", self.build_eapi_cookie())
            .body(body)
            .send()
            .await?
            .error_for_status()?;

        let raw_bytes = response.bytes().await?;
        let trimmed_bytes = raw_bytes
            .iter()
            .position(|&b| !b.is_ascii_whitespace())
            .map_or(&raw_bytes[..], |pos| &raw_bytes[pos..]);
        let data: SongUrlResponse = if trimmed_bytes.first() == Some(&b'{') {
            serde_json::from_slice(trimmed_bytes)?
        } else {
            let trimmed_str = std::str::from_utf8(trimmed_bytes)
                .map_err(|e| BotError::MusicApi(format!("Invalid UTF-8 in response: {e}")))?;
            let decrypted = eapi_crypto::eapi_decrypt(trimmed_str)
                .map_err(|e| BotError::MusicApi(e.to_string()))?;
            serde_json::from_str(&decrypted)?
        };

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
    ///
    /// # Errors
    /// Returns an error if the API request fails or returns an error code.
    pub async fn get_song_detail(&self, song_id: u64) -> Result<Arc<SongDetail>> {
        self.get_song_detail_shared(song_id).await
    }
}
