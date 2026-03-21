use std::sync::Arc;

use super::super::{
    DjProgramItem, MusicApi, ProgramMainTrack, Result, SongDetail, SongDetailResponse, SongUrl,
    SongUrlResponse,
};
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
}
