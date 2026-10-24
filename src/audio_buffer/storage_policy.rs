use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::Instant as StdInstant;

use anyhow::{Context, Result};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, get_current_pid};
use tokio::fs::File;
use uuid::Uuid;

use super::{AudioBuffer, MemoryReservation};
use crate::config::{Config, StorageMode};

/// Cached System instance with last-refresh timestamp for throttling.
static SYSTEM: LazyLock<Mutex<(System, StdInstant, u64)>> = LazyLock::new(|| {
    let mut sys = System::new();
    let available = sample_available_memory(&mut sys);
    Mutex::new((sys, StdInstant::now(), available))
});

/// Minimum interval between memory refreshes (500ms).
const MEMORY_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

impl AudioBuffer {
    /// Create a new audio buffer based on configuration and file characteristics.
    ///
    /// # Errors
    /// Returns an error if creating the disk file fails.
    pub async fn new(
        config: &Config,
        content_length: u64,
        filename: String,
        cache_dir: &str,
    ) -> Result<Self> {
        let reservation = if Self::should_use_memory(config, content_length) {
            MemoryReservation::try_acquire(
                content_length,
                Self::available_memory_bytes(),
                config.storage.memory_buffer_mb.saturating_mul(1024 * 1024),
            )
        } else {
            None
        };
        if let Some(reservation) = reservation {
            let capacity = if content_length > 0 {
                usize::try_from(content_length).unwrap_or(usize::MAX)
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
                reservation: Some(reservation),
            })
        } else {
            // The display name (sent to Telegram) intentionally omits the music_id, so two
            // distinct tracks that sanitize to the same "artist - title.ext" would otherwise
            // race on the same on-disk path (singleflight only dedupes by music_id). Give the
            // on-disk file a unique name while keeping the display filename clean.
            let disk_filename = Self::unique_disk_filename(&filename);
            Self::create_disk_buffer(filename, disk_filename, cache_dir, "using disk mode").await
        }
    }

    /// Build a collision-free on-disk filename from a display filename.
    fn unique_disk_filename(display_filename: &str) -> String {
        format!("{}_{display_filename}", Uuid::new_v4().simple())
    }

    /// Force creation of a disk-based buffer (for fallback scenarios).
    ///
    /// # Errors
    /// Returns an error if creating the disk file fails.
    pub async fn new_disk(filename: String, cache_dir: &str) -> Result<Self> {
        let disk_filename = filename.clone();
        Self::create_disk_buffer(filename, disk_filename, cache_dir, "forced disk mode").await
    }

    async fn create_disk_buffer(
        display_filename: String,
        disk_filename: String,
        cache_dir: &str,
        mode_label: &str,
    ) -> Result<Self> {
        super::ensure_safe_cache_filename(&disk_filename)?;
        let file_path = PathBuf::from(cache_dir).join(&disk_filename);

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
            filename: display_filename,
            written_bytes: 0,
        })
    }

    /// File limits use bytes so fractional MiB cannot slip over the configured cap.
    fn should_use_memory(config: &Config, content_length: u64) -> bool {
        content_length > 0
            && content_length
                <= config
                    .storage
                    .memory_max_file_mb
                    .saturating_mul(1024 * 1024)
            && match config.storage.storage_mode {
                StorageMode::Disk => false,
                StorageMode::Memory => true,
                StorageMode::Hybrid => {
                    content_length
                        <= config
                            .storage
                            .memory_threshold_mb
                            .saturating_mul(1024 * 1024)
                }
            }
    }

    pub fn get_available_memory_mb() -> u64 {
        Self::available_memory_bytes() / (1024 * 1024)
    }

    fn available_memory_bytes() -> u64 {
        let Ok(mut guard) = SYSTEM.lock() else {
            return 0;
        };
        let (system, last_refresh, available) = &mut *guard;
        if last_refresh.elapsed() >= MEMORY_REFRESH_INTERVAL {
            *available = sample_available_memory(system);
            *last_refresh = StdInstant::now();
        }
        *available
    }
}

fn sample_available_memory(system: &mut System) -> u64 {
    system.refresh_memory();
    let process_limits = get_current_pid().ok().and_then(|pid| {
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            false,
            ProcessRefreshKind::nothing(),
        );
        system
            .process(pid)
            .and_then(|process| process.cgroup_limits())
    });
    let limits = process_limits.or_else(|| system.cgroup_limits());
    effective_available_memory(
        system.available_memory(),
        limits.map(|limits| limits.free_memory),
    )
}

fn effective_available_memory(host: u64, cgroup: Option<u64>) -> u64 {
    cgroup.map_or(host, |free| free.min(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_memory_is_bounded_by_both_host_and_cgroup() {
        assert_eq!(effective_available_memory(8192, Some(128)), 128);
        assert_eq!(effective_available_memory(64, Some(128)), 64);
        assert_eq!(effective_available_memory(8192, Some(0)), 0);
        assert_eq!(effective_available_memory(8192, None), 8192);
    }
}
