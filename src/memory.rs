//! Memory management utilities for explicit memory optimization
//!
//! Provides functions to force memory release and reduce process footprint.

/// Force jemalloc to release unused memory back to the OS
///
/// This should be called periodically (e.g., after large operations)
/// to ensure memory is returned to the system rather than retained
/// in the allocator's pools.
#[cfg(not(target_env = "msvc"))]
pub fn force_memory_release() {
    // Strategy 1: Trigger decay to encourage memory return
    // This is less aggressive than purge but more efficient
    let decay_result = unsafe {
        tikv_jemalloc_sys::mallctl(
            c"arena.all.decay".as_ptr().cast(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    if decay_result != 0 {
        tracing::debug!(
            "jemalloc mallctl arena.all.decay failed with code {}",
            decay_result
        );
    }

    // Strategy 2: Force purge of dirty pages if decay didn't free enough
    // This is more aggressive and ensures immediate memory return
    let purge_result = unsafe {
        tikv_jemalloc_sys::mallctl(
            c"arena.all.purge".as_ptr().cast(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    if purge_result != 0 {
        tracing::debug!(
            "jemalloc mallctl arena.all.purge failed with code {}",
            purge_result
        );
    }
}

/// Stub for non-jemalloc platforms
#[cfg(target_env = "msvc")]
pub fn force_memory_release() {
    // Windows uses system allocator, no explicit purge available
}

/// Report current memory usage stats (debug builds only)
#[cfg(all(debug_assertions, not(target_env = "msvc")))]
pub fn log_memory_stats() {
    let mut epoch: u64 = 1;
    let epoch_size = std::mem::size_of::<u64>();
    let epoch_result = unsafe {
        tikv_jemalloc_sys::mallctl(
            c"epoch".as_ptr().cast(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            (&raw mut epoch).cast(),
            epoch_size,
        )
    };
    if epoch_result != 0 {
        tracing::debug!("jemalloc mallctl epoch failed with code {}", epoch_result);
    }

    let mut allocated: usize = 0;
    let mut size = std::mem::size_of::<usize>();
    let allocated_result = unsafe {
        tikv_jemalloc_sys::mallctl(
            c"stats.allocated".as_ptr().cast(),
            (&raw mut allocated).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if allocated_result != 0 {
        tracing::debug!(
            "jemalloc mallctl stats.allocated failed with code {}",
            allocated_result
        );
    }

    let mut resident: usize = 0;
    let resident_result = unsafe {
        tikv_jemalloc_sys::mallctl(
            c"stats.resident".as_ptr().cast(),
            (&raw mut resident).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if resident_result != 0 {
        tracing::debug!(
            "jemalloc mallctl stats.resident failed with code {}",
            resident_result
        );
    }

    tracing::debug!(
        "jemalloc stats: allocated={}MB, resident={}MB",
        allocated / 1024 / 1024,
        resident / 1024 / 1024
    );
}

#[cfg(not(all(debug_assertions, not(target_env = "msvc"))))]
pub fn log_memory_stats() {
    // No-op in release builds or on Windows
}
