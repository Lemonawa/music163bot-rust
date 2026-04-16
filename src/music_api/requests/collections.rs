use super::super::{
    AlbumSongsResponse, DjProgramDetailResponse, DjProgramListResponse, MusicApi,
    PlaylistDetailResponse, ProgramMainTrack, Result,
};
use super::api_code_error;
use crate::error::BotError;

impl MusicApi {
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
            return Err(api_code_error(data.code));
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
            return Err(api_code_error(data.code));
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
            return Err(api_code_error(data.code));
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
            return Err(api_code_error(data.code));
        }

        let mut tracks = Vec::with_capacity(data.programs.len());
        for program in data.programs {
            if let Ok(track) = Self::map_program_main_track(program) {
                tracks.push(track);
            }
        }
        Ok((data.count, tracks))
    }
}
