use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::Instant as StdInstant;

use anyhow::{Context, Result};
use sysinfo::System;
use tokio::fs::File;

use super::AudioBuffer;
use crate::config::{Config, StorageMode};

/// Cached System instance with last-refresh timestamp for throttling.
static SYSTEM: LazyLock<Mutex<(System, StdInstant)>> = LazyLock::new(|| {
    let mut sys = System::new();
    sys.refresh_memory();
    Mutex::new((sys, StdInstant::now()))
});

/// Minimum interval between memory refreshes (500ms).
const MEMORY_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

impl AudioBuffer {
    /// Create a new audio buffer based on configuration and file characteristics.
    pub async fn new(
        config: &Config,
        content_length: u64,
        filename: String,
        cache_dir: &str,
    ) -> Result<Self> {
        if Self::should_use_memory(config, content_length) {
            let capacity = if content_length > 0 {
                content_length as usize
            } else {
                10 * 1024 * 1024
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
            Self::create_disk_buffer(filename, cache_dir, "using disk mode").await
        }
    }

    /// Force creation of a disk-based buffer (for fallback scenarios).
    pub async fn new_disk(filename: String, cache_dir: &str) -> Result<Self> {
        Self::create_disk_buffer(filename, cache_dir, "forced disk mode").await
    }

    async fn create_disk_buffer(
        filename: String,
        cache_dir: &str,
        mode_label: &str,
    ) -> Result<Self> {
        super::ensure_safe_cache_filename(&filename)?;
        let file_path = PathBuf::from(cache_dir).join(&filename);

        tracing::debug!(
            "AudioBuffer: {} (path: {})",
            mode_label,
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

    /// Determine if memory mode should be used based on configuration and system state.
    fn should_use_memory(config: &Config, content_length: u64) -> bool {
        match config.storage_mode {
            StorageMode::Disk => false,
            StorageMode::Memory => {
                if content_length == 0 {
                    tracing::debug!(
                        "Memory mode: content_length unknown, falling back to disk to avoid unbounded buffering"
                    );
                    return false;
                }

                let file_size_mb = content_length / (1024 * 1024);
                if file_size_mb > config.memory_max_file_mb {
                    tracing::debug!(
                        "Memory mode: file size {}MB exceeds max {}MB, using disk",
                        file_size_mb,
                        config.memory_max_file_mb
                    );
                    return false;
                }

                let available_mb = Self::get_available_memory_mb();
                let required_mb = file_size_mb + config.memory_buffer_mb;

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
                if content_length == 0 {
                    tracing::debug!(
                        "Hybrid mode: content_length unknown, using disk to avoid unbounded buffering"
                    );
                    return false;
                }

                let file_size_mb = content_length / (1024 * 1024);

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

    /// Get available system memory in MB (throttled to avoid frequent syscalls).
    pub fn get_available_memory_mb() -> u64 {
        if let Ok(mut guard) = SYSTEM.lock() {
            let (system, last_refresh) = &mut *guard;
            if last_refresh.elapsed() >= MEMORY_REFRESH_INTERVAL {
                system.refresh_memory();
                *last_refresh = StdInstant::now();
            }
            system.available_memory() / (1024 * 1024)
        } else {
            tracing::warn!("Failed to lock SYSTEM mutex, using conservative memory estimate");
            512
        }
    }
}
