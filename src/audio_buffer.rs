//! Smart storage system for audio file processing (v1.1.0+)
//!
//! Provides three storage modes for temporary file handling during download:
//! - Disk: Traditional file-based storage (stable, low memory)
//! - Memory: In-memory processing (faster, reduces disk I/O)
//! - Hybrid: Smart selection based on file size and available memory (recommended)

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

mod tagging;

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
        if let Self::Disk { file, .. } = self
            && let Some(f) = file
        {
            f.flush().await.context("Failed to flush file")?;
        }
        Ok(())
    }

    async fn disk_size(path: &Path, written_bytes: u64) -> u64 {
        if written_bytes > 0 {
            written_bytes
        } else {
            tokio::fs::metadata(path)
                .await
                .map(|m| m.len())
                .unwrap_or(0)
        }
    }

    fn disk_size_fast(path: &Path, written_bytes: u64) -> u64 {
        if written_bytes > 0 {
            written_bytes
        } else {
            std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
        }
    }

    /// Get the current size of the buffer (async to avoid blocking)
    pub async fn size(&self) -> u64 {
        match self {
            Self::Disk {
                path,
                written_bytes,
                ..
            } => Self::disk_size(path, *written_bytes).await,
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
            } => Self::disk_size_fast(path, *written_bytes),
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
mod tests;
