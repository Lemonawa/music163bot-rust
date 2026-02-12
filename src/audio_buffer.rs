//! Smart storage system for audio file processing (v1.1.0+)
//!
//! Provides three storage modes for temporary file handling during download:
//! - Disk: Traditional file-based storage (stable, low memory)
//! - Memory: In-memory processing (faster, reduces disk I/O)
//! - Hybrid: Smart selection based on file size and available memory (recommended)

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Instant as StdInstant;

use anyhow::{Context, Result};
use bytes::Bytes;
use sysinfo::System;
use teloxide::types::InputFile;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

/// Cached System instance with last-refresh timestamp for throttling.
static SYSTEM: LazyLock<Mutex<(System, StdInstant)>> = LazyLock::new(|| {
    let mut sys = System::new();
    sys.refresh_memory();
    Mutex::new((sys, StdInstant::now()))
});

/// Minimum interval between memory refreshes (500ms)
const MEMORY_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

use crate::config::{Config, StorageMode};
use crate::music_api::SongDetail;

/// Audio file buffer supporting both disk and memory storage
pub enum AudioBuffer {
    /// Disk-based storage with file handle
    Disk {
        path: PathBuf,
        file: Option<File>,
        filename: String,
        written_bytes: u64,
    },
    /// Memory-based storage with byte vector
    Memory { data: Vec<u8>, filename: String },
}

/// Thumbnail buffer for album art
pub enum ThumbnailBuffer {
    /// Disk-based thumbnail
    Disk { path: PathBuf },
    /// Memory-based thumbnail
    Memory { data: Bytes },
}

impl AudioBuffer {
    /// Create a new audio buffer based on configuration and file characteristics
    ///
    /// # Arguments
    /// * `config` - Application configuration
    /// * `content_length` - Expected file size in bytes (0 if unknown)
    /// * `filename` - Target filename
    /// * `cache_dir` - Directory for disk storage
    pub async fn new(
        config: &Config,
        content_length: u64,
        filename: String,
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
                written_bytes: 0,
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
            written_bytes: 0,
        })
    }

    /// Determine if memory mode should be used based on configuration and system state
    fn should_use_memory(config: &Config, content_length: u64) -> bool {
        match config.storage_mode {
            StorageMode::Disk => false,
            StorageMode::Memory => {
                if content_length > 0 {
                    let file_size_mb = content_length / (1024 * 1024);
                    if file_size_mb > config.memory_max_file_mb {
                        tracing::debug!(
                            "Memory mode: file size {}MB exceeds max {}MB, using disk",
                            file_size_mb,
                            config.memory_max_file_mb
                        );
                        return false;
                    }
                }

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

                if file_size_mb > config.memory_max_file_mb {
                    tracing::debug!(
                        "Hybrid mode: file size {}MB exceeds max {}MB, using disk",
                        file_size_mb,
                        config.memory_max_file_mb
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

    /// Get available system memory in MB (throttled to avoid frequent syscalls)
    pub fn get_available_memory_mb() -> u64 {
        if let Ok(mut guard) = SYSTEM.lock() {
            let (sys, last_refresh) = &mut *guard;
            if last_refresh.elapsed() >= MEMORY_REFRESH_INTERVAL {
                sys.refresh_memory();
                *last_refresh = StdInstant::now();
            }
            sys.available_memory() / (1024 * 1024)
        } else {
            tracing::warn!("Failed to lock SYSTEM mutex, using conservative memory estimate");
            512
        }
    }

    /// Write a chunk of data to the buffer
    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        match self {
            Self::Disk {
                file,
                written_bytes,
                ..
            } => {
                let f = file.as_mut().context("Disk buffer missing file handle")?;
                f.write_all(chunk)
                    .await
                    .context("Failed to write chunk to disk")?;
                *written_bytes += chunk.len() as u64;
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
            Self::Disk {
                path,
                written_bytes,
                ..
            } => {
                if *written_bytes > 0 {
                    *written_bytes
                } else {
                    tokio::fs::metadata(path)
                        .await
                        .map(|m| m.len())
                        .unwrap_or(0)
                }
            }
            Self::Memory { data, .. } => data.len() as u64,
        }
    }

    /// Get the current size without async I/O.
    /// For disk mode uses blocking `std::fs::metadata` — only call from
    /// contexts where a brief blocking stat is acceptable (e.g. after
    /// `spawn_blocking` tag processing has just written the file).
    pub fn size_fast(&self) -> u64 {
        match self {
            Self::Disk {
                path,
                written_bytes,
                ..
            } => {
                if *written_bytes > 0 {
                    *written_bytes
                } else {
                    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
                }
            }
            Self::Memory { data, .. } => data.len() as u64,
        }
    }

    /// Check if this is a memory-based buffer
    pub fn is_memory(&self) -> bool {
        matches!(self, Self::Memory { .. })
    }

    /// Check if this is a disk-based buffer
    pub fn is_disk(&self) -> bool {
        matches!(self, Self::Disk { .. })
    }

    /// Get a mutable handle to the disk file (disk mode only)
    #[must_use]
    pub fn disk_file_mut(&mut self) -> Option<&mut File> {
        match self {
            Self::Disk { file, .. } => file.as_mut(),
            Self::Memory { .. } => None,
        }
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
        use id3::Version;

        let tag = Self::build_id3_tag(song_detail, artwork_data);

        match self {
            Self::Disk { path, .. } => {
                tag.write_to_path(path, Version::Id3v24)
                    .context("Failed to write ID3 tags to disk file")?;
            }
            Self::Memory { data, .. } => {
                // Memory mode: create new tag and prepend to audio data
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

        // 1. Find where audio data starts
        let audio_start = Self::find_flac_audio_start(data)?;
        // Clone only the audio portion we need
        let audio_data = &data[audio_start..];

        // 2. Read existing metadata
        let mut cursor = Cursor::new(&data[..]);
        let mut tag = match Tag::read_from(&mut cursor) {
            Ok(tag) => tag,
            Err(err) => {
                tracing::warn!(error = %err, "Failed to read FLAC tags from memory");
                Tag::new()
            }
        };

        Self::build_flac_tag_updates(&mut tag, song_detail, artwork_data);

        // 5. Build new data with estimated pre-allocation
        let estimated_capacity = audio_data.len() + 4096; // metadata overhead estimate
        let mut new_data = Vec::with_capacity(estimated_capacity);
        tag.write_to(&mut new_data)
            .map_err(|e| anyhow::anyhow!("Failed to write FLAC metadata to memory: {e}"))?;
        new_data.extend_from_slice(audio_data);
        *data = new_data;

        Ok(())
    }

    fn build_flac_tag_updates(
        tag: &mut metaflac::Tag,
        song_detail: &SongDetail,
        artwork_data: Option<&[u8]>,
    ) {
        use crate::music_api::format_artists;
        use metaflac::block::{Picture, PictureType};

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

            // 优化：使用 ImageReader 避免完整解码，减少内存占用
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

    /// Move memory-mode bytes out for upload without copying.
    #[must_use]
    pub fn take_memory_bytes_for_upload(&mut self) -> Option<Bytes> {
        match self {
            Self::Memory { data, .. } => Some(Bytes::from(std::mem::take(data))),
            Self::Disk { .. } => None,
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
                remove_file_if_exists(&path).await?;
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
        data: Bytes,
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
            tokio::fs::write(&path, data.as_ref())
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
        Self::Memory {
            data: Bytes::from(data),
        }
    }

    /// Create from Bytes data
    #[must_use]
    pub fn from_bytes(data: Bytes) -> Self {
        Self::Memory { data }
    }

    /// Get the thumbnail data
    pub async fn get_data(&self) -> Result<Vec<u8>> {
        match self {
            Self::Disk { path } => tokio::fs::read(path)
                .await
                .with_context(|| format!("Failed to read thumbnail: {}", path.display())),
            Self::Memory { data } => Ok(data.to_vec()),
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
                remove_file_if_exists(&path).await?;
            }
            Self::Memory { .. } => {
                // Memory is automatically freed
            }
        }
        Ok(())
    }
}

async fn remove_file_if_exists(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to remove file: {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    struct BufferWriter {
        buffer: Arc<Mutex<Vec<u8>>>,
    }

    struct BufferGuard {
        buffer: Arc<Mutex<Vec<u8>>>,
    }

    impl std::io::Write for BufferGuard {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut buffer = self
                .buffer
                .lock()
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::Other, "lock poisoned"))?;
            buffer.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for BufferWriter {
        type Writer = BufferGuard;

        fn make_writer(&'a self) -> Self::Writer {
            BufferGuard {
                buffer: Arc::clone(&self.buffer),
            }
        }
    }

    fn capture_warn_logs<F>(action: F) -> String
    where
        F: FnOnce(),
    {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt::Subscriber::builder()
            .with_writer(BufferWriter {
                buffer: Arc::clone(&buffer),
            })
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .finish();

        tracing::subscriber::with_default(subscriber, action);

        let buffer = buffer
            .lock()
            .expect("log buffer lock should succeed")
            .clone();
        String::from_utf8_lossy(&buffer).to_string()
    }

    fn sample_song_detail() -> SongDetail {
        SongDetail {
            id: 7,
            name: "Sample Song".to_string(),
            dt: Some(123_000),
            ar: Some(vec![crate::music_api::Artist {
                id: 1,
                name: "Artist".to_string(),
            }]),
            al: Some(crate::music_api::Album {
                id: 1,
                name: "Album".to_string(),
                pic_url: None,
            }),
        }
    }

    fn sample_cover_jpeg() -> Vec<u8> {
        let mut image = image::RgbImage::new(2, 2);
        for pixel in image.pixels_mut() {
            *pixel = image::Rgb([10, 20, 30]);
        }

        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut out),
                image::ImageFormat::Jpeg,
            )
            .expect("encode jpeg");
        out
    }

    fn sample_flac_bytes() -> Vec<u8> {
        let mut flac_data = b"fLaC".to_vec();
        flac_data.push(0x80);
        flac_data.extend_from_slice(&[0x00, 0x00, 0x22]);
        flac_data.extend_from_slice(&[0u8; 34]);
        flac_data.extend_from_slice(b"AUDIO_FRAMES");
        flac_data
    }

    fn sample_flac_bytes_with_invalid_vorbis() -> Vec<u8> {
        let mut flac_data = b"fLaC".to_vec();
        flac_data.push(0x80 | 0x04);
        flac_data.extend_from_slice(&[0x00, 0x00, 0x09]);
        flac_data.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
        flac_data.push(0xFF);
        flac_data.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        flac_data.extend_from_slice(b"AUDIO_FRAMES");
        flac_data
    }

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

    #[tokio::test]
    async fn cleanup_helper_ignores_missing_file() {
        let temp_name = format!(
            "music163bot_missing_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let path = std::env::temp_dir().join(temp_name);
        super::remove_file_if_exists(&path)
            .await
            .expect("missing file cleanup should succeed");
    }

    #[tokio::test]
    async fn audio_buffer_is_disk() {
        let temp_name = format!(
            "music163bot_audio_buffer_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let cache_dir = std::env::temp_dir();

        let disk_buffer = AudioBuffer::new_disk(temp_name.clone(), cache_dir.to_str().unwrap())
            .await
            .expect("create disk buffer");
        assert!(disk_buffer.is_disk());
        assert!(!disk_buffer.is_memory());
        disk_buffer.cleanup().await.expect("cleanup disk buffer");

        let mut config = Config::default();
        config.storage_mode = StorageMode::Memory;
        config.memory_buffer_mb = 0;
        config.memory_max_file_mb = u64::MAX;

        let memory_buffer = AudioBuffer::new(
            &config,
            1024,
            "test.mp3".to_string(),
            cache_dir.to_str().unwrap(),
        )
        .await
        .expect("create memory buffer");

        assert!(!memory_buffer.is_disk());
        assert!(memory_buffer.is_memory());
    }

    #[tokio::test]
    async fn write_chunk_errors_without_disk_handle() {
        let path = std::env::temp_dir().join(format!(
            "music163bot_audio_buffer_none_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let mut buffer = AudioBuffer::Disk {
            path,
            file: None,
            filename: "missing.mp3".to_string(),
            written_bytes: 0,
        };

        let result = buffer.write_chunk(b"abc").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn thumbnail_buffer_memory_bytes_roundtrip() {
        let data = bytes::Bytes::from_static(b"abc");
        let buf = ThumbnailBuffer::from_bytes(data.clone());
        assert_eq!(buf.get_data().await.unwrap_or_default(), b"abc");
    }

    #[test]
    fn mp3_tagging_is_byte_identical_for_same_input() {
        let detail = sample_song_detail();
        let cover = sample_cover_jpeg();
        let mut first = AudioBuffer::Memory {
            data: vec![0xFF, 0xFB, 0x90, 0x64],
            filename: "a.mp3".to_string(),
        };
        let mut second = AudioBuffer::Memory {
            data: vec![0xFF, 0xFB, 0x90, 0x64],
            filename: "b.mp3".to_string(),
        };

        first
            .add_id3_tags(&detail, Some(&cover))
            .expect("first mp3 tagging");
        second
            .add_id3_tags(&detail, Some(&cover))
            .expect("second mp3 tagging");

        let first_data = match first {
            AudioBuffer::Memory { data, .. } => data,
            AudioBuffer::Disk { .. } => panic!("expected memory buffer"),
        };
        let second_data = match second {
            AudioBuffer::Memory { data, .. } => data,
            AudioBuffer::Disk { .. } => panic!("expected memory buffer"),
        };

        assert_eq!(first_data, second_data);
    }

    #[test]
    fn flac_tagging_keeps_equivalent_metadata_and_audio_payload() {
        let detail = sample_song_detail();
        let cover = sample_cover_jpeg();
        let source = sample_flac_bytes();
        let mut first = AudioBuffer::Memory {
            data: source.clone(),
            filename: "a.flac".to_string(),
        };
        let mut second = AudioBuffer::Memory {
            data: source,
            filename: "b.flac".to_string(),
        };

        first
            .add_flac_metadata(&detail, Some(&cover))
            .expect("first flac tagging");
        second
            .add_flac_metadata(&detail, Some(&cover))
            .expect("second flac tagging");

        let first_data = match first {
            AudioBuffer::Memory { data, .. } => data,
            AudioBuffer::Disk { .. } => panic!("expected memory buffer"),
        };
        let second_data = match second {
            AudioBuffer::Memory { data, .. } => data,
            AudioBuffer::Disk { .. } => panic!("expected memory buffer"),
        };

        let mut first_cursor = std::io::Cursor::new(first_data.as_slice());
        let first_tag = metaflac::Tag::read_from(&mut first_cursor).expect("parse first flac tag");
        let mut second_cursor = std::io::Cursor::new(second_data.as_slice());
        let second_tag =
            metaflac::Tag::read_from(&mut second_cursor).expect("parse second flac tag");

        let collect_values = |tag: &metaflac::Tag, key: &str| {
            tag.get_vorbis(key).map(|iter| {
                iter.map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
            })
        };

        assert_eq!(
            collect_values(&first_tag, "TITLE"),
            collect_values(&second_tag, "TITLE")
        );
        assert_eq!(
            collect_values(&first_tag, "ALBUM"),
            collect_values(&second_tag, "ALBUM")
        );
        assert_eq!(
            collect_values(&first_tag, "ARTIST"),
            collect_values(&second_tag, "ARTIST")
        );

        let first_cover = first_tag
            .pictures()
            .find(|pic| pic.picture_type == metaflac::block::PictureType::CoverFront)
            .map(|pic| pic.data.clone())
            .expect("first cover picture");
        let second_cover = second_tag
            .pictures()
            .find(|pic| pic.picture_type == metaflac::block::PictureType::CoverFront)
            .map(|pic| pic.data.clone())
            .expect("second cover picture");
        assert_eq!(first_cover, second_cover);

        let first_audio_start =
            AudioBuffer::find_flac_audio_start(&first_data).expect("first audio start");
        let second_audio_start =
            AudioBuffer::find_flac_audio_start(&second_data).expect("second audio start");
        assert_eq!(&first_data[first_audio_start..], b"AUDIO_FRAMES");
        assert_eq!(&second_data[second_audio_start..], b"AUDIO_FRAMES");
    }

    #[test]
    fn flac_disk_logs_warning_on_tag_read_failure() {
        let detail = sample_song_detail();
        let path = std::env::temp_dir().join(format!(
            "music163bot_bad_flac_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::write(&path, b"NOTFLAC").expect("write temp flac");
        let mut buffer = AudioBuffer::Disk {
            path: path.clone(),
            file: None,
            filename: "bad.flac".to_string(),
            written_bytes: 0,
        };

        let logs = capture_warn_logs(|| {
            let _ = buffer.add_flac_metadata(&detail, None);
        });

        let _ = std::fs::remove_file(&path);
        assert!(logs.contains("Failed to read FLAC tags from disk"));
    }

    #[test]
    fn flac_memory_logs_warning_on_tag_read_failure() {
        let detail = sample_song_detail();
        let data = sample_flac_bytes_with_invalid_vorbis();
        let mut buffer = AudioBuffer::Memory {
            data,
            filename: "bad.flac".to_string(),
        };

        let logs = capture_warn_logs(|| {
            let _ = buffer.add_flac_metadata(&detail, None);
        });

        assert!(logs.contains("Failed to read FLAC tags from memory"));
    }

    #[test]
    fn memory_buffer_take_bytes_moves_data_without_copy() {
        let mut buffer = AudioBuffer::Memory {
            data: vec![1, 2, 3, 4, 5],
            filename: "sample.mp3".to_string(),
        };

        let original_ptr = match &buffer {
            AudioBuffer::Memory { data, .. } => data.as_ptr(),
            AudioBuffer::Disk { .. } => panic!("expected memory buffer"),
        };

        let bytes = buffer
            .take_memory_bytes_for_upload()
            .expect("memory buffer should produce bytes");

        assert_eq!(bytes.as_ptr(), original_ptr);
        assert_eq!(bytes.as_ref(), &[1, 2, 3, 4, 5]);

        match buffer {
            AudioBuffer::Memory { ref data, .. } => assert!(data.is_empty()),
            AudioBuffer::Disk { .. } => panic!("expected memory buffer"),
        }
    }

    #[test]
    fn get_available_memory_mb_is_stable() {
        let mb1 = AudioBuffer::get_available_memory_mb();
        let mb2 = AudioBuffer::get_available_memory_mb();
        if mb1 > 0 {
            // Non-sandboxed: value should be plausible (under 16 TB)
            assert!(
                mb1 < 16 * 1024 * 1024,
                "available memory looks implausibly large: {mb1} MB"
            );
            // Second call should also be positive
            assert!(mb2 > 0, "second call returned 0 while first returned {mb1}");
        } else {
            // Sandboxed env where sysinfo returns 0 — both calls should agree
            assert_eq!(mb2, 0, "first call returned 0 but second returned {mb2}");
        }
    }

    #[test]
    fn get_available_memory_mb_is_consistent() {
        let mb1 = AudioBuffer::get_available_memory_mb();
        let mb2 = AudioBuffer::get_available_memory_mb();
        // Within the same second, throttled calls should return similar values
        // (exact same if within throttle window)
        let diff = if mb1 > mb2 { mb1 - mb2 } else { mb2 - mb1 };
        assert!(
            diff < 1024,
            "Two rapid calls should return similar values: {mb1} vs {mb2}"
        );
    }

    #[tokio::test]
    async fn disk_written_bytes_tracks_sequential_writes() {
        let temp_name = format!(
            "music163bot_written_bytes_{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let cache_dir = std::env::temp_dir();
        let mut buffer = AudioBuffer::new_disk(temp_name, cache_dir.to_str().unwrap())
            .await
            .expect("create disk buffer");

        assert_eq!(buffer.size().await, 0, "size should be 0 before any writes");

        buffer.write_chunk(b"hello").await.expect("first write");
        assert_eq!(buffer.size().await, 5, "size after first write");
        assert_eq!(buffer.size_fast(), 5, "size_fast after first write");

        buffer.write_chunk(b" world").await.expect("second write");
        assert_eq!(buffer.size().await, 11, "size after second write");
        assert_eq!(buffer.size_fast(), 11, "size_fast after second write");

        buffer.cleanup().await.expect("cleanup disk buffer");
    }
}
