use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bytes::Bytes;
use teloxide::types::InputFile;

use super::{ThumbnailBuffer, remove_file_if_exists};
use crate::config::{Config, StorageMode};

impl ThumbnailBuffer {
    /// Create a new thumbnail buffer.
    pub async fn new(
        config: &Config,
        data: Bytes,
        cache_dir: &str,
        filename: &str,
    ) -> Result<Self> {
        let use_memory = match config.storage_mode {
            StorageMode::Disk => false,
            StorageMode::Memory | StorageMode::Hybrid => {
                let size_mb = data.len() as u64 / (1024 * 1024);
                size_mb < 5
            }
        };

        if use_memory {
            Ok(Self::Memory { data })
        } else {
            super::ensure_safe_cache_filename(filename)?;
            let path = PathBuf::from(cache_dir).join(filename);
            tokio::fs::write(&path, data.as_ref())
                .await
                .with_context(|| format!("Failed to write thumbnail: {}", path.display()))?;
            Ok(Self::Disk { path })
        }
    }

    /// Create from existing file path (for backward compatibility).
    #[must_use]
    pub fn from_path(path: PathBuf) -> Self {
        Self::Disk { path }
    }

    /// Create from memory data.
    #[must_use]
    pub fn from_memory(data: Vec<u8>) -> Self {
        Self::Memory {
            data: Bytes::from(data),
        }
    }

    /// Create from Bytes data.
    #[must_use]
    pub fn from_bytes(data: Bytes) -> Self {
        Self::Memory { data }
    }

    /// Get the thumbnail data.
    pub async fn get_data(&self) -> Result<Vec<u8>> {
        match self {
            Self::Disk { path } => tokio::fs::read(path)
                .await
                .with_context(|| format!("Failed to read thumbnail: {}", path.display())),
            Self::Memory { data } => Ok(data.to_vec()),
        }
    }

    /// Get the path (only for disk mode).
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Disk { path } => Some(path),
            Self::Memory { .. } => None,
        }
    }

    /// Check if this is memory-based.
    #[must_use]
    pub fn is_memory(&self) -> bool {
        matches!(self, Self::Memory { .. })
    }

    /// Convert to InputFile for Telegram.
    pub fn to_input_file(&self) -> Result<InputFile> {
        match self {
            Self::Disk { path } => Ok(InputFile::file(path)),
            Self::Memory { data } => Ok(InputFile::memory(data.clone()).file_name("thumb.jpg")),
        }
    }

    /// Convert to InputFile for Telegram (consumes self, avoids cloning).
    #[must_use]
    pub fn into_input_file(self) -> InputFile {
        match self {
            Self::Disk { path } => InputFile::file(path),
            Self::Memory { data } => InputFile::memory(data).file_name("thumb.jpg"),
        }
    }

    /// Cleanup resources.
    pub async fn cleanup(self) -> Result<()> {
        match self {
            Self::Disk { path } => remove_file_if_exists(&path).await?,
            Self::Memory { .. } => {}
        }

        Ok(())
    }
}
