//! Smart storage system for audio file processing (v1.1.0+)
//!
//! Provides three storage modes for temporary file handling during download:
//! - Disk: Traditional file-based storage (stable, low memory)
//! - Memory: In-memory processing (faster, reduces disk I/O)
//! - Hybrid: Smart selection based on file size and available memory (recommended)

use anyhow::{Context, Result};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use sysinfo::System;
use teloxide::types::InputFile;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

/// 缓存的 System 实例，避免每次检查内存都创建新对象
static SYSTEM: LazyLock<Mutex<System>> = LazyLock::new(|| {
    let mut sys = System::new();
    sys.refresh_memory();
    Mutex::new(sys)
});

use crate::config::{Config, StorageMode};
use crate::music_api::SongDetail;

/// Audio file buffer supporting both disk and memory storage
pub enum AudioBuffer {
    /// Disk-based storage with file handle
    Disk {
        path: PathBuf,
        file: Option<File>,
        filename: String,
    },
    /// Memory-based storage with byte vector
    Memory {
        data: Vec<u8>,
        filename: String,
        capacity: usize,
    },
}

/// Thumbnail buffer for album art
pub enum ThumbnailBuffer {
    /// Disk-based thumbnail
    Disk { path: PathBuf },
    /// Memory-based thumbnail
    Memory { data: Vec<u8> },
}

impl AudioBuffer {
    /// Create a new audio buffer based on configuration and file characteristics
    ///
    /// # Arguments
    /// * `config` - Application configuration
    /// * `content_length` - Expected file size in bytes (0 if unknown)
    /// * `filename` - Target filename
    /// * `file_ext` - File extension (mp3, flac)
    /// * `cache_dir` - Directory for disk storage
    pub async fn new(
        config: &Config,
        content_length: u64,
        filename: String,
        _file_ext: &str,
        cache_dir: &str,
    ) -> Result<Self> {
        let use_memory = Self::should_use_memory(config, content_length);

        if use_memory {
            let capacity = if content_length > 0 {
                content_length as usize
            } else {
                // Default capacity for unknown size
                10 * 1024 * 1024 // 10MB
            };

            tracing::debug!(
                "AudioBuffer: using memory mode (capacity: {} bytes)",
                capacity
            );

            Ok(Self::Memory {
                data: Vec::with_capacity(capacity),
                filename,
                capacity,
            })
        } else {
            let file_path = PathBuf::from(cache_dir).join(&filename);

            tracing::debug!(
                "AudioBuffer: using disk mode (path: {})",
                file_path.display()
            );

            let file = File::create(&file_path)
                .await
                .with_context(|| format!("Failed to create file: {}", file_path.display()))?;

            Ok(Self::Disk {
                path: file_path,
                file: Some(file),
                filename,
            })
        }
    }

    /// Force creation of a disk-based buffer (for fallback scenarios)
    pub async fn new_disk(filename: String, cache_dir: &str) -> Result<Self> {
        let file_path = PathBuf::from(cache_dir).join(&filename);

        tracing::debug!(
            "AudioBuffer: forced disk mode (path: {})",
            file_path.display()
        );

        let file = File::create(&file_path)
            .await
            .with_context(|| format!("Failed to create file: {}", file_path.display()))?;

        Ok(Self::Disk {
            path: file_path,
            file: Some(file),
            filename,
        })
    }

    /// Determine if memory mode should be used based on configuration and system state
    fn should_use_memory(config: &Config, content_length: u64) -> bool {
        match config.storage_mode {
            StorageMode::Disk => false,
            StorageMode::Memory => {
                // Always use memory, but check if we have enough
                let available_mb = Self::get_available_memory_mb();
                let required_mb = (content_length / (1024 * 1024)) + config.memory_buffer_mb;

                if available_mb >= required_mb {
                    true
                } else {
                    tracing::error!(
                        "Memory mode requested but insufficient memory: available={}MB, required={}MB. Falling back to disk.",
                        available_mb,
                        required_mb
                    );
                    false
                }
            }
            StorageMode::Hybrid => {
                let file_size_mb = content_length / (1024 * 1024);

                // Check threshold first
                if file_size_mb > config.memory_threshold_mb {
                    tracing::debug!(
                        "Hybrid mode: file size {}MB exceeds threshold {}MB, using disk",
                        file_size_mb,
                        config.memory_threshold_mb
                    );
                    return false;
                }

                // Check available memory
                let available_mb = Self::get_available_memory_mb();
                let required_mb = file_size_mb + config.memory_buffer_mb;

                if available_mb >= required_mb {
                    tracing::debug!(
                        "Hybrid mode: using memory (file={}MB, available={}MB, buffer={}MB)",
                        file_size_mb,
                        available_mb,
                        config.memory_buffer_mb
                    );
                    true
                } else {
                    tracing::debug!(
                        "Hybrid mode: insufficient memory (available={}MB < required={}MB), using disk",
                        available_mb,
                        required_mb
                    );
                    false
                }
            }
        }
    }

    /// Get available system memory in MB (使用缓存的 System 实例)
    fn get_available_memory_mb() -> u64 {
        if let Ok(mut sys) = SYSTEM.lock() {
            sys.refresh_memory();
            sys.available_memory() / (1024 * 1024)
        } else {
            // 降级方案：返回保守估计
            tracing::warn!("Failed to lock SYSTEM mutex, using conservative memory estimate");
            512 // 保守估计 512MB 可用
        }
    }

    /// Write a chunk of data to the buffer
    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        match self {
            Self::Disk { file, .. } => {
                if let Some(f) = file {
                    f.write_all(chunk)
                        .await
                        .context("Failed to write chunk to disk")?;
                }
            }
            Self::Memory { data, .. } => {
                data.extend_from_slice(chunk);
            }
        }
        Ok(())
    }

    /// Finish writing and flush any buffers
    pub async fn finish(&mut self) -> Result<()> {
        match self {
            Self::Disk { file, .. } => {
                if let Some(f) = file {
                    f.flush().await.context("Failed to flush file")?;
                }
            }
            Self::Memory { .. } => {
                // Nothing to flush for memory buffer
            }
        }
        Ok(())
    }

    /// Get the current size of the buffer (async to avoid blocking)
    pub async fn size(&self) -> u64 {
        match self {
            Self::Disk { path, .. } => {
                tokio::fs::metadata(path).await.map(|m| m.len()).unwrap_or(0)
            }
            Self::Memory { data, .. } => data.len() as u64,
        }
    }

    /// Check if this is a memory-based buffer
    pub fn is_memory(&self) -> bool {
        matches!(self, Self::Memory { .. })
    }

    /// Get the filename
    pub fn filename(&self) -> &str {
        match self {
            Self::Disk { filename, .. } | Self::Memory { filename, .. } => filename,
        }
    }

    /// Get the file path (only for disk mode)
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Disk { path, .. } => Some(path),
            Self::Memory { .. } => None,
        }
    }

    /// Add ID3 tags to MP3 file (supports both disk and memory modes)
    pub fn add_id3_tags(
        &mut self,
        song_detail: &SongDetail,
        artwork_data: Option<&[u8]>,
    ) -> Result<()> {
        use crate::music_api::format_artists;
        use id3::{Tag, TagLike, Version, frame};

        match self {
            Self::Disk { path, .. } => {
                // Disk mode: use existing file-based approach
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

                tag.write_to_path(path, Version::Id3v24)
                    .context("Failed to write ID3 tags to disk file")?;
            }
            Self::Memory { data, .. } => {
                // Memory mode: create new tag and prepend to audio data
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

                // Write tag to buffer
                let mut tag_buffer = Vec::new();
                tag.write_to(&mut tag_buffer, Version::Id3v24)
                    .context("Failed to write ID3 tags to memory")?;

                // For MP3: ID3v2 tag goes at the beginning
                // Check if data already starts with ID3
                let has_existing_id3 = data.len() >= 3 && &data[0..3] == b"ID3";
                if has_existing_id3 {
                    // Skip existing ID3 tag and replace with new one
                    let audio_start = Self::find_mp3_audio_start(data);
                    // Use a single reallocation approach
                    let mut new_data =
                        Vec::with_capacity(tag_buffer.len() + data.len() - audio_start);
                    new_data.extend_from_slice(&tag_buffer);
                    new_data.extend_from_slice(&data[audio_start..]);
                    *data = new_data;
                } else {
                    // No existing ID3, just prepend - use single allocation
                    let mut new_data = Vec::with_capacity(tag_buffer.len() + data.len());
                    new_data.extend_from_slice(&tag_buffer);
                    new_data.extend_from_slice(data);
                    *data = new_data;
                }

                // Release unused capacity to reduce memory footprint
                data.shrink_to_fit();
            }
        }

        Ok(())
    }

    /// Find the start of MP3 audio data (after ID3v2 tag)
    fn find_mp3_audio_start(data: &[u8]) -> usize {
        if data.len() < 10 || &data[0..3] != b"ID3" {
            return 0; // No ID3 tag
        }

        // ID3v2 header: "ID3" + version (2 bytes) + flags (1 byte) + size (4 bytes syncsafe)
        let size_bytes = &data[6..10];
        let size = ((size_bytes[0] as usize & 0x7F) << 21)
            | ((size_bytes[1] as usize & 0x7F) << 14)
            | ((size_bytes[2] as usize & 0x7F) << 7)
            | (size_bytes[3] as usize & 0x7F);

        10 + size // Header (10 bytes) + tag data
    }

    /// Add FLAC metadata (picture block + vorbis comments) - supports both disk and memory modes
    pub fn add_flac_metadata(
        &mut self,
        song_detail: &SongDetail,
        artwork_data: Option<&[u8]>,
    ) -> Result<()> {
        match self {
            Self::Disk { path, .. } => {
                // Disk mode: use metaflac directly
                Self::add_flac_metadata_disk(path, song_detail, artwork_data)
            }
            Self::Memory { data, .. } => {
                // Memory mode: parse and rebuild FLAC in memory
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
        use crate::music_api::format_artists;
        use metaflac::Tag;
        use metaflac::block::{Picture, PictureType};

        let mut tag = Tag::read_from_path(path).unwrap_or_else(|_| Tag::new());

        // Add Vorbis Comments (text metadata)
        // Title
        tag.set_vorbis("TITLE", vec![song_detail.name.clone()]);

        // Album
        let album_name = song_detail
            .al
            .as_ref()
            .map_or("Unknown Album", |al| al.name.as_str());
        tag.set_vorbis("ALBUM", vec![album_name.to_string()]);

        // Artist (Performer)
        let artist = format_artists(song_detail.ar.as_deref().unwrap_or(&[]));
        tag.set_vorbis("ARTIST", vec![artist]);

        // Description (163 key) - preserve existing value if present, otherwise don't add
        // The original FLAC file from NetEase may already contain the 163 key
        // We don't generate a fake key, just preserve what's already there

        // Add album artwork if provided
        if let Some(artwork_data) = artwork_data {
            tag.remove_picture_type(PictureType::CoverFront);

            let (width, height) = match image::load_from_memory(artwork_data) {
                Ok(img) => (img.width(), img.height()),
                Err(_) => (0, 0),
            };

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
        use crate::music_api::format_artists;
        use metaflac::Tag;
        use metaflac::block::{Picture, PictureType};

        // 1. Find where audio data starts
        let audio_start = Self::find_flac_audio_start(data)?;
        // Clone only the audio portion we need
        let audio_data = &data[audio_start..];

        // 2. Read existing metadata
        let mut cursor = Cursor::new(&data[..]);
        let mut tag = Tag::read_from(&mut cursor).unwrap_or_else(|_| Tag::new());

        // 3. Add Vorbis Comments (text metadata)
        tag.set_vorbis("TITLE", vec![song_detail.name.clone()]);

        let album_name = song_detail
            .al
            .as_ref()
            .map_or("Unknown Album", |al| al.name.as_str());
        tag.set_vorbis("ALBUM", vec![album_name.to_string()]);

        let artist = format_artists(song_detail.ar.as_deref().unwrap_or(&[]));
        tag.set_vorbis("ARTIST", vec![artist]);

        // 4. Add album artwork if provided
        if let Some(artwork_data) = artwork_data {
            tag.remove_picture_type(PictureType::CoverFront);

            let (width, height) = match image::load_from_memory(artwork_data) {
                Ok(img) => (img.width(), img.height()),
                Err(_) => (0, 0),
            };

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

        // 5. Build new data with single allocation
        let mut new_data = Vec::new();
        tag.write_to(&mut new_data)
            .map_err(|e| anyhow::anyhow!("Failed to write FLAC metadata to memory: {e}"))?;
        new_data.extend_from_slice(audio_data);
        *data = new_data;
        data.shrink_to_fit();

        Ok(())
    }

    /// Find the start of FLAC audio frames (after all metadata blocks)
    fn find_flac_audio_start(data: &[u8]) -> Result<usize> {
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

    /// Convert to InputFile for Telegram upload (borrows)
    pub fn to_input_file(&self) -> InputFile {
        match self {
            Self::Disk { path, .. } => InputFile::file(path),
            Self::Memory { data, filename, .. } => {
                InputFile::memory(data.clone()).file_name(filename.clone())
            }
        }
    }

    /// Convert to InputFile for Telegram upload (consumes self, avoids cloning)
    pub fn into_input_file(self) -> InputFile {
        match self {
            Self::Disk { path, .. } => InputFile::file(path),
            Self::Memory { data, filename, .. } => InputFile::memory(data).file_name(filename),
        }
    }

    /// Get raw data (for memory mode) or read from disk
    pub async fn get_data(&self) -> Result<Vec<u8>> {
        match self {
            Self::Disk { path, .. } => tokio::fs::read(path)
                .await
                .with_context(|| format!("Failed to read file: {}", path.display())),
            Self::Memory { data, .. } => Ok(data.clone()),
        }
    }

    /// Cleanup resources
    pub async fn cleanup(self) -> Result<()> {
        match self {
            Self::Disk { path, file, .. } => {
                // Close file handle first
                drop(file);
                // Then remove the file
                if path.exists() {
                    tokio::fs::remove_file(&path)
                        .await
                        .with_context(|| format!("Failed to remove file: {}", path.display()))?;
                }
            }
            Self::Memory { .. } => {
                // Memory is automatically freed when dropped
            }
        }
        Ok(())
    }
}

impl ThumbnailBuffer {
    /// Create a new thumbnail buffer
    pub async fn new(
        config: &Config,
        data: Vec<u8>,
        cache_dir: &str,
        filename: &str,
    ) -> Result<Self> {
        let use_memory = match config.storage_mode {
            StorageMode::Disk => false,
            StorageMode::Memory | StorageMode::Hybrid => {
                // Thumbnails are usually small, prefer memory
                let size_mb = data.len() as u64 / (1024 * 1024);
                size_mb < 5 // Use memory for thumbnails under 5MB
            }
        };

        if use_memory {
            Ok(Self::Memory { data })
        } else {
            let path = PathBuf::from(cache_dir).join(filename);
            tokio::fs::write(&path, &data)
                .await
                .with_context(|| format!("Failed to write thumbnail: {}", path.display()))?;
            Ok(Self::Disk { path })
        }
    }

    /// Create from existing file path (for backward compatibility)
    #[must_use]
    pub fn from_path(path: PathBuf) -> Self {
        Self::Disk { path }
    }

    /// Create from memory data
    #[must_use]
    pub fn from_memory(data: Vec<u8>) -> Self {
        Self::Memory { data }
    }

    /// Get the thumbnail data
    pub async fn get_data(&self) -> Result<Vec<u8>> {
        match self {
            Self::Disk { path } => tokio::fs::read(path)
                .await
                .with_context(|| format!("Failed to read thumbnail: {}", path.display())),
            Self::Memory { data } => Ok(data.clone()),
        }
    }

    /// Get the path (only for disk mode)
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Disk { path } => Some(path),
            Self::Memory { .. } => None,
        }
    }

    /// Check if this is memory-based
    #[must_use]
    pub fn is_memory(&self) -> bool {
        matches!(self, Self::Memory { .. })
    }

    /// Convert to InputFile for Telegram
    pub fn to_input_file(&self) -> Result<InputFile> {
        match self {
            Self::Disk { path } => Ok(InputFile::file(path)),
            Self::Memory { data } => Ok(InputFile::memory(data.clone()).file_name("thumb.jpg")),
        }
    }

    /// Convert to InputFile for Telegram (consumes self, avoids cloning)
    #[must_use]
    pub fn into_input_file(self) -> InputFile {
        match self {
            Self::Disk { path } => InputFile::file(path),
            Self::Memory { data } => InputFile::memory(data).file_name("thumb.jpg"),
        }
    }

    /// Cleanup resources
    pub async fn cleanup(self) -> Result<()> {
        match self {
            Self::Disk { path } => {
                if path.exists() {
                    tokio::fs::remove_file(&path).await.with_context(|| {
                        format!("Failed to remove thumbnail: {}", path.display())
                    })?;
                }
            }
            Self::Memory { .. } => {
                // Memory is automatically freed
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_flac_audio_start() {
        // Minimal FLAC with just streaminfo block (is_last=true)
        // "fLaC" + header (0x80 | 0x00 = StreamInfo, last) + 34 bytes length + 34 bytes data
        let mut flac_data = b"fLaC".to_vec();
        flac_data.push(0x80); // Last block, type 0 (StreamInfo)
        flac_data.extend_from_slice(&[0x00, 0x00, 0x22]); // Length = 34
        flac_data.extend_from_slice(&[0u8; 34]); // StreamInfo data
        flac_data.extend_from_slice(b"AUDIO_FRAMES"); // Audio data

        let result = AudioBuffer::find_flac_audio_start(&flac_data);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 4 + 4 + 34); // magic + header + data
    }

    #[test]
    fn test_find_mp3_audio_start() {
        // ID3v2 header with size 0
        let mut mp3_data = b"ID3".to_vec();
        mp3_data.extend_from_slice(&[0x04, 0x00]); // Version 2.4.0
        mp3_data.push(0x00); // Flags
        mp3_data.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // Size = 0 (syncsafe)
        mp3_data.extend_from_slice(b"\xFF\xFB"); // MP3 sync word

        let result = AudioBuffer::find_mp3_audio_start(&mp3_data);
        assert_eq!(result, 10); // 10 byte header
    }
}
