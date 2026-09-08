//! Periodic maintenance: counters collected per request decide which signals
//! to enqueue; a single background worker drains them.

use crate::config::Config;
use crate::database::Database;
use crate::music_api::MusicApi;

use std::sync::Arc;

use super::wiring::{CACHE_PRUNE_INTERVAL_REQUESTS, MaintenanceCounters, MaintenanceSignal};

pub(super) fn collect_maintenance_signals(
    counters: &MaintenanceCounters,
    config: &Config,
) -> Vec<MaintenanceSignal> {
    let mut signals = Vec::with_capacity(3);
    for (counter, interval, signal) in [
        (
            &counters.db_analyze,
            config.maintenance.db_analyze_interval_requests,
            MaintenanceSignal::AnalyzeDb,
        ),
        (
            &counters.memory_release,
            config.maintenance.memory_release_interval_requests,
            MaintenanceSignal::ReleaseMemory,
        ),
        (
            &counters.api_cache_prune,
            CACHE_PRUNE_INTERVAL_REQUESTS,
            MaintenanceSignal::PruneApiCache,
        ),
    ] {
        if MaintenanceCounters::should_run(counter, interval) {
            signals.push(signal);
        }
    }

    signals
}

pub(super) async fn maintenance_worker(
    mut rx: tokio::sync::mpsc::Receiver<MaintenanceSignal>,
    database: Database,
    music_api: Arc<MusicApi>,
) {
    while let Some(signal) = rx.recv().await {
        match signal {
            MaintenanceSignal::AnalyzeDb => {
                if let Err(e) = database.optimize_planner().await {
                    tracing::warn!("Database planner optimize failed: {}", e);
                }
            }
            MaintenanceSignal::ReleaseMemory => {
                if let Err(e) = tokio::task::spawn_blocking(|| {
                    crate::memory::force_memory_release();
                    crate::memory::log_memory_stats();
                })
                .await
                {
                    tracing::warn!("Memory release background task failed: {}", e);
                }
            }
            MaintenanceSignal::PruneApiCache => {
                let stats = music_api.prune_expired_cache_entries();
                if stats.total_removed() > 0 {
                    tracing::debug!(
                        "Pruned API cache entries: detail={}, url={}, lyric={}",
                        stats.song_detail_removed,
                        stats.song_url_removed,
                        stats.song_lyric_removed
                    );
                }
            }
        }
    }
}
