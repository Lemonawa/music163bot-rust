use serde::{Deserialize, Serialize};

fn deserialize_string_or_null<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Option::unwrap_or_default)
}

/// eapi song-url entries come back with `null` fields when a song is
/// unavailable at the requested level (seen in the wild with batched hires
/// probes); treat null as 0 instead of failing the whole response.
fn deserialize_u64_or_null<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<u64>::deserialize(deserializer)?.unwrap_or(0))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SongDetailResponse {
    pub code: i32,
    pub songs: Vec<SongDetail>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PlaylistDetailResponse {
    pub(super) code: i32,
    pub(super) playlist: Option<PlaylistDetail>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PlaylistDetail {
    #[serde(rename = "trackIds")]
    pub(super) track_ids: Vec<PlaylistTrackId>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PlaylistTrackId {
    pub(super) id: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct AlbumSongsResponse {
    pub(super) code: i32,
    pub(super) songs: Vec<AlbumSong>,
}

#[derive(Debug, Deserialize)]
pub(super) struct AlbumSong {
    pub(super) id: u64,
}

#[derive(Debug, Deserialize)]
pub(super) struct DjProgramDetailResponse {
    pub(super) code: i32,
    pub(super) program: Option<DjProgramItem>,
}

#[derive(Debug, Deserialize)]
pub(super) struct DjProgramListResponse {
    pub(super) code: i32,
    pub(super) count: usize,
    #[serde(default)]
    pub(super) programs: Vec<DjProgramItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct DjProgramItem {
    pub(super) id: u64,
    #[serde(default)]
    pub(super) name: String,
    #[serde(rename = "mainTrackId")]
    pub(super) main_track_id: Option<u64>,
    #[serde(default)]
    pub(super) dj: Option<DjProgramDj>,
    #[serde(default)]
    pub(super) radio: Option<DjProgramRadio>,
    #[serde(rename = "coverUrl")]
    pub(super) cover_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct DjProgramDj {
    #[serde(default)]
    pub(super) nickname: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct DjProgramRadio {
    #[serde(default)]
    pub(super) name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramMainTrack {
    pub program_id: u64,
    pub main_track_id: u64,
    pub program_name: String,
    pub author_name: String,
    pub radio_name: String,
    pub cover_url: Option<String>,
}

/// What the eapi song-url endpoint would currently serve for one song at a
/// given level: bitrate, file size, and container. Ground truth for tools
/// comparing against cached copies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedSize {
    pub br: i64,
    pub size: i64,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SongDetail {
    pub id: u64,
    #[serde(default, deserialize_with = "deserialize_string_or_null")]
    pub name: String,
    #[serde(alias = "duration")]
    pub dt: Option<u64>,
    #[serde(alias = "artists")]
    pub ar: Option<Vec<Artist>>,
    #[serde(alias = "album")]
    pub al: Option<Album>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artist {
    pub id: u64,
    #[serde(default, deserialize_with = "deserialize_string_or_null")]
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Album {
    pub id: u64,
    #[serde(default, deserialize_with = "deserialize_string_or_null")]
    pub name: String,
    #[serde(rename = "picUrl")]
    pub pic_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SongUrlResponse {
    pub code: i32,
    pub data: Vec<SongUrl>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SongUrl {
    pub id: u64,
    #[serde(default, deserialize_with = "deserialize_string_or_null")]
    pub url: String,
    #[serde(default, deserialize_with = "deserialize_u64_or_null")]
    pub br: u64,
    #[serde(default, deserialize_with = "deserialize_u64_or_null")]
    pub size: u64,
    #[serde(default, deserialize_with = "deserialize_string_or_null")]
    pub md5: String,
    #[serde(rename = "type")]
    #[serde(default, deserialize_with = "deserialize_string_or_null")]
    pub format: String,
}

impl SongUrl {
    /// Whether this entry carries a usable download URL (empty means the
    /// tier is unavailable for this song/account).
    #[must_use]
    pub fn has_download_url(&self) -> bool {
        !self.url.is_empty()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LyricResponse {
    pub code: i32,
    pub lrc: Option<LyricContent>,
    pub tlyric: Option<LyricContent>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LyricContent {
    pub lyric: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResponse {
    pub code: i32,
    pub result: SearchResult,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct EapiSearchResponse {
    pub(super) code: i32,
    pub(super) result: SearchResult,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub songs: Vec<SearchSong>,
    #[serde(rename = "songCount")]
    pub song_count: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchSong {
    pub id: u64,
    pub name: String,
    pub artists: Vec<Artist>,
    pub album: Album,
    pub duration: u64,
}
