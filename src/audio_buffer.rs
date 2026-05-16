//! Smart storage system for audio file processing (v1.1.0+)
//!
//! Provides three storage modes for temporary file handling during download:
//! - Disk: Traditional file-based storage (stable, low memory)
//! - Memory: In-memory processing (faster, reduces disk I/O)
//! - Hybrid: Smart selection based on file size and available memory (recommended)

use std::path::{Path, PathBuf};

use crate::telegram::InputFile;
use anyhow::{Context, Result};
use bytes::Bytes;
use tokio::fs::File;

mod backend;
mod storage_policy;
mod tagging;
mod thumbnail;

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
    pub fn into_input_file(mut self) -> InputFile {
        match &mut self {
            Self::Disk { path, .. } => {
                let path = std::mem::take(path);
                InputFile::file(path)
            }
            Self::Memory { data, filename, .. } => {
                let data = std::mem::take(data);
                let filename = std::mem::take(filename);
                InputFile::memory(data).file_name(filename)
            }
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
}

impl Drop for AudioBuffer {
    fn drop(&mut self) {
        if let Self::Disk { path, file, .. } = self {
            file.take();
            if path.as_os_str().is_empty() {
                return;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(
                        "Failed to remove audio cache file on drop ({}): {}",
                        path.display(),
                        e
                    );
                }
            }
        }
    }
}

async fn remove_file_if_exists(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to remove file: {}", path.display())),
    }
}

pub(super) fn ensure_safe_cache_filename(filename: &str) -> Result<()> {
    use std::path::Component;

    if filename.is_empty() {
        return Err(anyhow::anyhow!("Empty filename for cache path"));
    }

    let path = Path::new(filename);

    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            _ => {
                return Err(anyhow::anyhow!("Unsafe filename for cache path"));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests;
