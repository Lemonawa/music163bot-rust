//! Memory management utilities for explicit memory optimization
//!
//! Provides functions to force memory release and reduce process footprint.

#![allow(
    clippy::ptr_as_ptr,
    clippy::manual_c_str_literals,
    clippy::borrow_as_ptr,
    clippy::ref_as_ptr
)]

/// Force jemalloc to release unused memory back to the OS
///
/// This should be called periodically (e.g., after large operations)
/// to ensure memory is returned to the system rather than retained
/// in the allocator's pools.
#[cfg(not(target_env = "msvc"))]
pub fn force_memory_release() {
    unsafe {
        // Trigger purge of all arenas' dirty pages
        // mallctl signature: name, oldp, oldlenp, newp, newlen
        // For arena.all.purge: no read value, no write value (command only)
        let _ = tikv_jemalloc_sys::mallctl(
            b"arena.all.purge\0".as_ptr().cast(),
            std::ptr::null_mut(), // oldp - not reading
            std::ptr::null_mut(), // oldlenp - not reading
            std::ptr::null_mut(), // newp - no input parameter
            0,                    // newlen - no input parameter
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
            b"epoch\0".as_ptr().cast(),
            (&mut epoch as *mut u64).cast(),
            &mut epoch_size,
            std::ptr::null_mut(),
            0,
        );

        let mut allocated: usize = 0;
        let mut size = std::mem::size_of::<usize>();
        let _ = tikv_jemalloc_sys::mallctl(
            b"stats.allocated\0".as_ptr().cast(),
            (&mut allocated as *mut usize).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        );

        let mut resident: usize = 0;
        let _ = tikv_jemalloc_sys::mallctl(
            b"stats.resident\0".as_ptr().cast(),
            (&mut resident as *mut usize).cast(),
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
