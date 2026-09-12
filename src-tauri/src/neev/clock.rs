use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) trait Clock: Send + Sync {
    fn now_unix(&self) -> i64;
}

pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
            .unwrap_or_default()
    }
}
