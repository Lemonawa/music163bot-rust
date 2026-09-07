//! Semaphore permits and the singleflight download leader: every place the
//! bot throttles itself before doing work.

use crate::error::{BotError, Result};

use std::sync::Arc;

use super::wiring::{InflightClaim, InflightDownloads, InflightLeaderGuard};

pub(super) async fn acquire_download_permit(
    semaphore: &tokio::sync::Semaphore,
) -> Result<tokio::sync::SemaphorePermit<'_>> {
    acquire_semaphore_permit(semaphore, "download").await
}

pub(super) async fn acquire_upload_permit(
    semaphore: &tokio::sync::Semaphore,
) -> Result<tokio::sync::SemaphorePermit<'_>> {
    acquire_semaphore_permit(semaphore, "upload").await
}

async fn acquire_semaphore_permit<'a>(
    semaphore: &'a tokio::sync::Semaphore,
    label: &str,
) -> Result<tokio::sync::SemaphorePermit<'a>> {
    semaphore.acquire().await.map_err(|e| {
        tracing::error!("{} semaphore closed: {}", label, e);
        BotError::Other(anyhow::anyhow!("{label} semaphore closed"))
    })
}

pub(super) async fn acquire_upload_permit_owned(
    semaphore: Arc<tokio::sync::Semaphore>,
) -> Result<tokio::sync::OwnedSemaphorePermit> {
    semaphore.acquire_owned().await.map_err(|e| {
        tracing::error!("Upload semaphore closed: {}", e);
        BotError::Other(anyhow::anyhow!("upload semaphore closed"))
    })
}

/// Claim the singleflight slot for `music_id`: the leader gets a guard to
/// hold until its download completes; followers wait for the leader and get
/// `None` (the leader's cache write will serve them).
pub(super) async fn acquire_download_leader(
    inflight: &Arc<InflightDownloads>,
    music_id: u64,
) -> Option<InflightLeaderGuard> {
    match inflight.begin(music_id) {
        InflightClaim::Leader(guard) => Some(guard),
        InflightClaim::Follower(entry) => {
            entry.wait().await;
            None
        }
    }
}
