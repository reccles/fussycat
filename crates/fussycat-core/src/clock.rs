//! The `Clock` port and the time values the core passes around. The core
//! never reads the wall clock itself (eval S2), so tests run on fake time.

use serde::{Serialize, Serializer};

/// A point in time: milliseconds since the Unix epoch, UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(u64);

impl Timestamp {
    #[must_use]
    pub const fn from_unix_millis(millis: u64) -> Self {
        Self(millis)
    }

    /// Milliseconds from `earlier` to `self`; zero if `earlier` is later.
    #[must_use]
    pub fn millis_since(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

/// RFC 3339 to the second, e.g. `2026-10-23T20:15:00Z`.
impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let seconds = self.0 / 1000;
        let (year, month, day) = civil_from_days(seconds / 86_400);
        let clock = seconds % 86_400;
        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            clock / 3600,
            clock / 60 % 60,
            clock % 60
        )
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// Howard Hinnant's `civil_from_days`, for days since 1970-01-01.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

/// A short human duration: `45s`, `3m07s`, `2h05m`.
#[must_use]
pub fn human_duration(millis: u64) -> String {
    let seconds = millis / 1000;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3600, seconds / 60 % 60),
    }
}

pub trait Clock {
    fn now(&self) -> Timestamp;
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::{Clock, Timestamp};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Starts at `start` and moves on `step` milliseconds every time it's read.
    pub struct TickingClock {
        next: AtomicU64,
        step: u64,
    }

    impl TickingClock {
        pub fn new(start: u64, step: u64) -> Self {
            Self {
                next: AtomicU64::new(start),
                step,
            }
        }
    }

    impl Clock for TickingClock {
        fn now(&self) -> Timestamp {
            Timestamp(self.next.fetch_add(self.step, Ordering::SeqCst))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_rfc3339_utc() {
        assert_eq!(Timestamp(0).to_string(), "1970-01-01T00:00:00Z");
        assert_eq!(
            Timestamp(1_792_786_500_999).to_string(),
            "2026-10-23T20:15:00Z"
        );
        assert_eq!(
            Timestamp(951_868_799_000).to_string(),
            "2000-02-29T23:59:59Z"
        );
    }

    #[test]
    fn serializes_as_its_display() {
        assert_eq!(
            serde_json::to_string(&Timestamp(0)).unwrap(),
            "\"1970-01-01T00:00:00Z\""
        );
    }

    #[test]
    fn millis_since_never_goes_negative() {
        assert_eq!(Timestamp(5_000).millis_since(Timestamp(2_000)), 3_000);
        assert_eq!(Timestamp(2_000).millis_since(Timestamp(5_000)), 0);
    }

    #[test]
    fn human_durations() {
        assert_eq!(human_duration(45_999), "45s");
        assert_eq!(human_duration(187_000), "3m07s");
        assert_eq!(human_duration(7_500_000), "2h05m");
    }
}
