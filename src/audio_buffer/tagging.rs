use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};

use super::AudioBuffer;
use crate::music_api::SongDetail;

impl AudioBuffer {
    /// Add ID3 tags to MP3 file (supports both disk and memory modes)
    pub fn add_id3_tags(
        &mut self,
        song_detail: &SongDetail,
        artwork_data: Option<&[u8]>,
    ) -> Result<()> {
        use id3::Version;

        let tag = Self::build_id3_tag(song_detail, artwork_data);

        match self {
            Self::Disk {
                path,
                written_bytes,
                ..
            } => {
                tag.write_to_path(path.as_path(), Version::Id3v24)
                    .context("Failed to write ID3 tags to disk file")?;
                *written_bytes = std::fs::metadata(path.as_path())
                    .map_or(0, |m| m.len());
            }
            Self::Memory { data, .. } => {
                let mut tag_buffer = Vec::new();
                tag.write_to(&mut tag_buffer, Version::Id3v24)
                    .context("Failed to write ID3 tags to memory")?;

                let has_existing_id3 = data.len() >= 3 && &data[0..3] == b"ID3";
                if has_existing_id3 {
                    let audio_start = Self::find_mp3_audio_start(data);
                    // Use a single reallocation approach
                    let mut new_data =
                        Vec::with_capacity(tag_buffer.len() + data.len() - audio_start);
                    new_data.extend_from_slice(&tag_buffer);
                    new_data.extend_from_slice(&data[audio_start..]);
                    *data = new_data;
                } else {
                    let mut new_data = Vec::with_capacity(tag_buffer.len() + data.len());
                    new_data.extend_from_slice(&tag_buffer);
                    new_data.extend_from_slice(data);
                    *data = new_data;
                }
            }
        }

        Ok(())
    }

    fn build_id3_tag(song_detail: &SongDetail, artwork_data: Option<&[u8]>) -> id3::Tag {
        use crate::music_api::format_artists;
        use id3::{Tag, TagLike, frame};

        let mut tag = Tag::new();

        tag.set_title(&song_detail.name);
        let album_name = song_detail
            .al
            .as_ref()
            .map_or("Unknown Album", |al| al.name.as_str());
        tag.set_album(album_name);
        tag.set_artist(format_artists(song_detail.ar.as_deref().unwrap_or(&[])));
        tag.set_duration((song_detail.dt.unwrap_or(0) / 1000) as u32);

        if let Some(artwork) = artwork_data {
            let picture = frame::Picture {
                mime_type: "image/jpeg".to_string(),
                picture_type: frame::PictureType::CoverFront,
                description: "Album Cover".to_string(),
                data: artwork.to_vec(),
            };
            tag.add_frame(picture);
        }

        tag
    }

    /// Find the start of MP3 audio data (after ID3v2 tag)
    pub(super) fn find_mp3_audio_start(data: &[u8]) -> usize {
        if data.len() < 10 || &data[0..3] != b"ID3" {
            return 0;
        }

        // ID3v2 header: "ID3" + version (2 bytes) + flags (1 byte) + size (4 bytes syncsafe)
        let size_bytes = &data[6..10];
        let size = ((size_bytes[0] as usize & 0x7F) << 21)
            | ((size_bytes[1] as usize & 0x7F) << 14)
            | ((size_bytes[2] as usize & 0x7F) << 7)
            | (size_bytes[3] as usize & 0x7F);

        std::cmp::min(10 + size, data.len()) // Prevent out of bounds
    }

    /// Add FLAC metadata (picture block + vorbis comments) - supports both disk and memory modes
    pub fn add_flac_metadata(
        &mut self,
        song_detail: &SongDetail,
        artwork_data: Option<&[u8]>,
    ) -> Result<()> {
        match self {
            Self::Disk {
                path,
                written_bytes,
                ..
            } => {
                Self::add_flac_metadata_disk(path.as_path(), song_detail, artwork_data)?;
                *written_bytes = std::fs::metadata(path.as_path())
                    .map_or(0, |m| m.len());
                Ok(())
            }
            Self::Memory { data, .. } => {
                Self::add_flac_metadata_memory(data, song_detail, artwork_data)
            }
        }
    }

    /// Add FLAC metadata using disk-based metaflac
    fn add_flac_metadata_disk(
        path: &Path,
        song_detail: &SongDetail,
        artwork_data: Option<&[u8]>,
    ) -> Result<()> {
        use metaflac::Tag;
        let mut tag = match Tag::read_from_path(path) {
            Ok(tag) => tag,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    path = %path.display(),
                    "Failed to read FLAC tags from disk"
                );
                Tag::new()
            }
        };

        Self::build_flac_tag_updates(&mut tag, song_detail, artwork_data);

        tag.write_to_path(path)
            .map_err(|e| anyhow::anyhow!("Failed to write FLAC metadata: {e}"))?;

        Ok(())
    }

    /// Add FLAC metadata in memory by parsing and rebuilding the file
    fn add_flac_metadata_memory(
        data: &mut Vec<u8>,
        song_detail: &SongDetail,
        artwork_data: Option<&[u8]>,
    ) -> Result<()> {
        use metaflac::Tag;

        let audio_start = Self::find_flac_audio_start(data)?;
        let audio_data = &data[audio_start..];

        let mut cursor = Cursor::new(&data[..]);
        let mut tag = match Tag::read_from(&mut cursor) {
            Ok(tag) => tag,
            Err(err) => {
                tracing::warn!(error = %err, "Failed to read FLAC tags from memory");
                Tag::new()
            }
        };

        Self::build_flac_tag_updates(&mut tag, song_detail, artwork_data);

        let artwork_overhead = artwork_data.map_or(0, <[u8]>::len);
        let mut metadata_bytes = Vec::with_capacity(artwork_overhead + 4096); // metadata overhead estimate
        tag.write_to(&mut metadata_bytes)
            .map_err(|e| anyhow::anyhow!("Failed to write FLAC metadata to memory: {e}"))?;

        // 6. Keep audio in-place and prepend metadata without building a full second audio copy.
        let audio_len = audio_data.len();
        data.copy_within(audio_start.., 0);
        data.truncate(audio_len);

        let metadata_len = metadata_bytes.len();
        data.resize(audio_len + metadata_len, 0);
        data.copy_within(0..audio_len, metadata_len);
        data[..metadata_len].copy_from_slice(&metadata_bytes);

        Ok(())
    }

    fn build_flac_tag_updates(
        tag: &mut metaflac::Tag,
        song_detail: &SongDetail,
        artwork_data: Option<&[u8]>,
    ) {
        use crate::music_api::format_artists;
        use metaflac::block::{Picture, PictureType};

        tag.set_vorbis("TITLE", vec![song_detail.name.clone()]);

        // Album
        let album_name = song_detail
            .al
            .as_ref()
            .map_or("Unknown Album", |al| al.name.as_str());
        tag.set_vorbis("ALBUM", vec![album_name.to_string()]);

        let artist = format_artists(song_detail.ar.as_deref().unwrap_or(&[]));
        tag.set_vorbis("ARTIST", vec![artist]);

        // The 163 key is preserved if already present; a fake key is never generated.

        if let Some(artwork_data) = artwork_data {
            tag.remove_picture_type(PictureType::CoverFront);

            // Use ImageReader to avoid full decode and reduce memory usage.
            let (width, height) = image::ImageReader::new(std::io::Cursor::new(artwork_data))
                .with_guessed_format()
                .ok()
                .and_then(|r| r.into_dimensions().ok())
                .unwrap_or((0, 0));

            let mut pic = Picture::new();
            pic.picture_type = PictureType::CoverFront;
            pic.mime_type = "image/jpeg".to_string();
            pic.description = "Front cover".to_string();
            pic.width = width;
            pic.height = height;
            pic.depth = 24;
            pic.num_colors = 0;
            pic.data = artwork_data.to_vec();

            tag.push_block(metaflac::Block::Picture(pic));
        }
    }

    /// Find the start of FLAC audio frames (after all metadata blocks)
    pub(super) fn find_flac_audio_start(data: &[u8]) -> Result<usize> {
        // FLAC format: "fLaC" (4 bytes) + metadata blocks + audio frames
        if data.len() < 8 || &data[0..4] != b"fLaC" {
            return Err(anyhow::anyhow!("Not a valid FLAC file"));
        }

        let mut pos = 4; // Skip magic

        loop {
            if pos + 4 > data.len() {
                return Err(anyhow::anyhow!("Unexpected end of FLAC metadata"));
            }

            let header = data[pos];
            let is_last = (header & 0x80) != 0;

            // Block length is 3 bytes big-endian
            let block_len =
                u32::from_be_bytes([0, data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;

            pos += 4 + block_len; // Skip header + block data

            if is_last {
                break;
            }
        }

        Ok(pos)
    }
}
