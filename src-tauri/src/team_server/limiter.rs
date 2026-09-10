use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const AUTH_CAPACITY: f64 = 100.0;
const AUTH_REFILL_PER_SECOND: f64 = 1.0;
const CATALOG_CAPACITY: f64 = 200.0;
const CATALOG_REFILL_PER_SECOND: f64 = 20.0;
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
    capacity: f64,
    refill_per_second: f64,
}

impl AuthLimiter {
    pub(super) fn pair() -> (Self, Self) {
        Self::pair_with_clock_inner(Arc::new(SystemMonotonic(Instant::now())))
    }

    #[cfg(test)]
    fn pair_with_clock(clock: Arc<dyn MonotonicClock>) -> (Self, Self) {
        Self::pair_with_clock_inner(clock)
    }

    fn pair_with_clock_inner(clock: Arc<dyn MonotonicClock>) -> (Self, Self) {
        let concurrent = Arc::new(Semaphore::new(MAX_CONCURRENT));
        (
            Self::with_clock(
                clock.clone(),
                concurrent.clone(),
                AUTH_CAPACITY,
                AUTH_REFILL_PER_SECOND,
            ),
            Self::with_clock(
                clock,
                concurrent,
                CATALOG_CAPACITY,
                CATALOG_REFILL_PER_SECOND,
            ),
        )
    }

    fn with_clock(
        clock: Arc<dyn MonotonicClock>,
        concurrent: Arc<Semaphore>,
        capacity: f64,
        refill_per_second: f64,
    ) -> Self {
        Self {
            clock,
            bucket: Mutex::new(Bucket {
                tokens: capacity,
                last: Duration::ZERO,
            }),
            concurrent,
            capacity,
            refill_per_second,
        }
    }

    pub(super) fn try_acquire(&self) -> Result<OwnedSemaphorePermit, LimitError> {
        let now = self.clock.now();
        let mut bucket = self.bucket.lock().map_err(|_| LimitError::Unavailable)?;
        let elapsed = now.saturating_sub(bucket.last).as_secs_f64();
        bucket.tokens =
            (bucket.tokens + elapsed * self.refill_per_second).min(self.capacity);
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
        let (limiter, _) = AuthLimiter::pair_with_clock(clock.clone());
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
        let (limiter, _) = AuthLimiter::pair();
        let permits: Vec<_> = (0..32)
            .map(|_| limiter.try_acquire().expect("concurrent permit"))
            .collect();
        assert!(matches!(limiter.try_acquire(), Err(LimitError::RateLimited)));
        drop(permits);
        drop(limiter.try_acquire().expect("released concurrent permit"));
    }

    #[test]
    fn http_catalog_and_auth_buckets_are_independent_with_exact_bounded_refill() {
        let clock = Arc::new(TestMonotonic(AtomicU64::new(0)));
        let (auth, catalog) = AuthLimiter::pair_with_clock(clock.clone());

        for _ in 0..100 {
            drop(auth.try_acquire().expect("initial auth token"));
        }
        assert!(matches!(auth.try_acquire(), Err(LimitError::RateLimited)));
        for _ in 0..200 {
            drop(catalog.try_acquire().expect("independent catalog token"));
        }
        assert!(matches!(catalog.try_acquire(), Err(LimitError::RateLimited)));

        clock.0.store(20, Ordering::SeqCst);
        for _ in 0..20 {
            drop(auth.try_acquire().expect("bounded auth refill"));
        }
        assert!(matches!(auth.try_acquire(), Err(LimitError::RateLimited)));
        for _ in 0..200 {
            drop(catalog.try_acquire().expect("bounded catalog refill"));
        }
        assert!(matches!(catalog.try_acquire(), Err(LimitError::RateLimited)));

        let reverse_clock = Arc::new(TestMonotonic(AtomicU64::new(0)));
        let (auth, catalog) = AuthLimiter::pair_with_clock(reverse_clock.clone());
        for _ in 0..200 {
            drop(catalog.try_acquire().expect("initial catalog token"));
        }
        assert!(matches!(catalog.try_acquire(), Err(LimitError::RateLimited)));
        reverse_clock.0.store(1, Ordering::SeqCst);
        for _ in 0..20 {
            drop(catalog.try_acquire().expect("one-second catalog refill"));
        }
        assert!(matches!(catalog.try_acquire(), Err(LimitError::RateLimited)));
        for _ in 0..100 {
            drop(auth.try_acquire().expect("independent auth token"));
        }
        assert!(matches!(auth.try_acquire(), Err(LimitError::RateLimited)));
    }

    #[test]
    fn http_catalog_and_auth_share_one_total_concurrency_bound() {
        let clock = Arc::new(TestMonotonic(AtomicU64::new(0)));
        let (auth, catalog) = AuthLimiter::pair_with_clock(clock);
        let mut permits = Vec::new();
        for _ in 0..16 {
            permits.push(auth.try_acquire().expect("auth concurrency permit"));
            permits.push(catalog.try_acquire().expect("catalog concurrency permit"));
        }
        assert!(matches!(auth.try_acquire(), Err(LimitError::RateLimited)));
        assert!(matches!(catalog.try_acquire(), Err(LimitError::RateLimited)));
        drop(permits.pop());
        permits.push(catalog.try_acquire().expect("shared released permit"));
    }
}
