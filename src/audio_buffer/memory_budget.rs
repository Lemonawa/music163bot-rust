use std::sync::{Arc, LazyLock, Mutex};

static BUDGET: LazyLock<Arc<MemoryBudget>> = LazyLock::new(Arc::default);

#[derive(Debug, Default)]
struct MemoryBudget {
    reserved: Mutex<u64>,
}

/// A reservation follows the audio allocation through tagging and shared upload bytes.
#[derive(Debug)]
pub struct MemoryReservation {
    budget: Arc<MemoryBudget>,
    bytes: u64,
}

impl MemoryBudget {
    fn reserve(
        self: &Arc<Self>,
        bytes: u64,
        available: u64,
        safety: u64,
    ) -> Option<MemoryReservation> {
        let mut reserved = self.reserved.lock().ok()?;
        let total = reserved.checked_add(bytes)?;
        if total > available.saturating_sub(safety) {
            return None;
        }
        *reserved = total;
        Some(MemoryReservation {
            budget: Arc::clone(self),
            bytes,
        })
    }
}

impl MemoryReservation {
    pub(super) fn try_acquire(content_length: u64, available: u64, safety: u64) -> Option<Self> {
        // Allow for a second audio allocation during tagging, Vec growth, and covers.
        let bytes = content_length
            .checked_mul(3)?
            .checked_add(20 * 1024 * 1024)?;
        BUDGET.reserve(bytes, available, safety)
    }
}

impl Drop for MemoryReservation {
    fn drop(&mut self) {
        let mut reserved = self
            .budget
            .reserved
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *reserved -= self.bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_reservations_cannot_spend_the_same_capacity() {
        let budget = Arc::new(MemoryBudget::default());
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let budget = Arc::clone(&budget);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let reservation = budget.reserve(80, 300, 100);
                    barrier.wait();
                    reservation
                })
            })
            .collect();
        let reservations: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(
            reservations.iter().filter(|value| value.is_some()).count(),
            2
        );
        assert!(budget.reserve(80, 300, 100).is_none());
        drop(reservations);
        assert!(budget.reserve(200, 300, 100).is_some());
    }

    #[test]
    fn reservation_outlives_buffer_when_upload_bytes_are_shared() {
        let budget = Arc::new(MemoryBudget::default());
        let mut audio = crate::audio_buffer::AudioBuffer::Memory {
            data: vec![1, 2, 3],
            filename: "test.mp3".into(),
            reservation: budget.reserve(80, 100, 0),
        };
        let bytes = audio.take_memory_bytes_for_upload().unwrap();
        let replay = bytes.clone();
        drop(audio);
        drop(bytes);
        assert!(budget.reserve(80, 100, 0).is_none());
        assert_eq!(replay.as_ref(), &[1, 2, 3]);
        drop(replay);
        assert!(budget.reserve(80, 100, 0).is_some());
    }
}
