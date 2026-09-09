use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const CAPACITY: f64 = 100.0;
const REFILL_PER_SECOND: f64 = 1.0;
const MAX_CONCURRENT: usize = 32;

trait MonotonicClock: Send + Sync {
    fn now(&self) -> Duration;
}

struct SystemMonotonic(Instant);

impl MonotonicClock for SystemMonotonic {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

struct Bucket {
    tokens: f64,
    last: Duration,
}

pub(super) struct AuthLimiter {
    clock: Arc<dyn MonotonicClock>,
    bucket: Mutex<Bucket>,
    concurrent: Arc<Semaphore>,
}

impl AuthLimiter {
    pub(super) fn new() -> Self {
        Self::with_clock(Arc::new(SystemMonotonic(Instant::now())))
    }

    fn with_clock(clock: Arc<dyn MonotonicClock>) -> Self {
        Self {
            clock,
            bucket: Mutex::new(Bucket {
                tokens: CAPACITY,
                last: Duration::ZERO,
            }),
            concurrent: Arc::new(Semaphore::new(MAX_CONCURRENT)),
        }
    }

    pub(super) fn try_acquire(&self) -> Result<OwnedSemaphorePermit, LimitError> {
        let now = self.clock.now();
        let mut bucket = self.bucket.lock().map_err(|_| LimitError::Unavailable)?;
        let elapsed = now.saturating_sub(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * REFILL_PER_SECOND).min(CAPACITY);
        bucket.last = now;
        if bucket.tokens < 1.0 {
            return Err(LimitError::RateLimited);
        }
        let permit = self
            .concurrent
            .clone()
            .try_acquire_owned()
            .map_err(|_| LimitError::RateLimited)?;
        bucket.tokens -= 1.0;
        Ok(permit)
    }
}

#[derive(Debug)]
pub(super) enum LimitError {
    RateLimited,
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestMonotonic(AtomicU64);

    impl MonotonicClock for TestMonotonic {
        fn now(&self) -> Duration {
            Duration::from_secs(self.0.load(Ordering::SeqCst))
        }
    }

    #[test]
    fn bucket_is_bounded_and_refills_from_monotonic_time() {
        let clock = Arc::new(TestMonotonic(AtomicU64::new(0)));
        let limiter = AuthLimiter::with_clock(clock.clone());
        for _ in 0..100 {
            drop(limiter.try_acquire().expect("initial token"));
        }
        assert!(matches!(limiter.try_acquire(), Err(LimitError::RateLimited)));
        clock.0.store(1, Ordering::SeqCst);
        drop(limiter.try_acquire().expect("one refilled token"));
        assert!(matches!(limiter.try_acquire(), Err(LimitError::RateLimited)));
    }

    #[test]
    fn concurrent_authentication_operations_are_bounded() {
        let limiter = AuthLimiter::new();
        let permits: Vec<_> = (0..32)
            .map(|_| limiter.try_acquire().expect("concurrent permit"))
            .collect();
        assert!(matches!(limiter.try_acquire(), Err(LimitError::RateLimited)));
        drop(permits);
        drop(limiter.try_acquire().expect("released concurrent permit"));
    }
}
