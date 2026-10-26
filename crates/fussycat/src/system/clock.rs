use std::time::{SystemTime, UNIX_EPOCH};

use fussycat_core::clock::{Clock, Timestamp};

/// The wall clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Timestamp::from_unix_millis(u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX))
    }
}
