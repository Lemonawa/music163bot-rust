use std::path::Path;

use anyhow::{Context, Result};
use tokio::io::AsyncWriteExt;

use super::{AudioBuffer, remove_file_if_exists};

impl AudioBuffer {
    /// Write a chunk of data to the buffer.
    pub async fn write_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        match self {
            Self::Disk {
                file,
                written_bytes,
                ..
            } => {
                let file = file.as_mut().context("Disk buffer missing file handle")?;
                file.write_all(chunk)
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

    /// Finish writing and flush any buffers.
    pub async fn finish(&mut self) -> Result<()> {
        if let Self::Disk { file, .. } = self
            && let Some(file) = file
        {
            file.flush().await.context("Failed to flush file")?;
        }

        Ok(())
    }

    async fn disk_size(path: &Path, written_bytes: u64) -> u64 {
        if written_bytes > 0 {
            written_bytes
        } else {
            tokio::fs::metadata(path)
                .await
                .map_or(0, |metadata| metadata.len())
        }
    }

    fn disk_size_fast(path: &Path, written_bytes: u64) -> u64 {
        if written_bytes > 0 {
            written_bytes
        } else {
            std::fs::metadata(path)
                .map_or(0, |metadata| metadata.len())
        }
    }

    /// Get the current size of the buffer (async to avoid blocking).
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
    /// For disk mode uses blocking `std::fs::metadata` - only call from
    /// contexts where a brief blocking stat is acceptable.
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

    /// Get raw data (for memory mode) or read from disk.
    pub async fn get_data(&self) -> Result<Vec<u8>> {
        match self {
            Self::Disk { path, .. } => tokio::fs::read(path)
                .await
                .with_context(|| format!("Failed to read file: {}", path.display())),
            Self::Memory { data, .. } => Ok(data.clone()),
        }
    }

    /// Cleanup resources.
    pub async fn cleanup(self) -> Result<()> {
        match self {
            Self::Disk { path, file, .. } => {
                drop(file);
                remove_file_if_exists(&path).await?;
            }
            Self::Memory { .. } => {}
        }

        Ok(())
    }
}
