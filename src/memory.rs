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
    // Flush dirty pages and purge unused arenas
    // This forces jemalloc to return memory to the OS
    unsafe {
        // Use jemalloc's mallctl to trigger purge
        let mut ptr: *mut libc::c_void = std::ptr::null_mut();
        let mut zero = 0_usize;
        let _ = tikv_jemalloc_sys::mallctl(
            b"arena.all.purge\0".as_ptr() as *const _,
            std::ptr::null_mut(),
            &mut zero,
            &mut ptr as *mut _ as *mut libc::c_void,
            0,
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
    unsafe {
        let mut epoch: u64 = 1;
        let mut epoch_size = std::mem::size_of::<u64>();
        let _ = tikv_jemalloc_sys::mallctl(
            b"epoch\0".as_ptr() as *const _,
            &mut epoch as *mut _ as *mut libc::c_void,
            &mut epoch_size,
            std::ptr::null_mut(),
            0,
        );

        let mut allocated: usize = 0;
        let mut size = std::mem::size_of::<usize>();
        let _ = tikv_jemalloc_sys::mallctl(
            b"stats.allocated\0".as_ptr() as *const _,
            &mut allocated as *mut _ as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        );

        let mut resident: usize = 0;
        let _ = tikv_jemalloc_sys::mallctl(
            b"stats.resident\0".as_ptr() as *const _,
            &mut resident as *mut _ as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        );

        tracing::debug!(
            "jemalloc stats: allocated={}MB, resident={}MB",
            allocated / 1024 / 1024,
            resident / 1024 / 1024
        );
    }
}

#[cfg(not(all(debug_assertions, not(target_env = "msvc"))))]
pub fn log_memory_stats() {
    // No-op in release builds or on Windows
}
