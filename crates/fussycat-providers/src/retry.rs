//! Retrying a provider call. rig judges each failure (worth retrying? did
//! the provider ask for a wait?); this only counts, waits and gives up.
//! Retries are reported in `Response::retries`, never hidden.

use std::fmt;
use std::future::Future;
use std::time::{Duration, SystemTime};

use fussycat_core::provider::ProviderError;
use rig_core::ProviderError as RigError;

use super::failure::describe;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub max_retries: u32,
}

impl Policy {
    pub const DEFAULT_RETRIES: u32 = 3;
    const FIRST_WAIT: Duration = Duration::from_secs(1);
    /// The longest fussycat waits between attempts, asked or not. A provider
    /// asking for more gets a failure, not a silently stalled cron job.
    pub const MAX_WAIT: Duration = Duration::from_secs(30);

    /// 1s, 2s, 4s, ...: when the provider didn't say how long to wait.
    fn backoff(retry: u32) -> Duration {
        Self::FIRST_WAIT
            .saturating_mul(2u32.saturating_pow(retry))
            .min(Self::MAX_WAIT)
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            max_retries: Self::DEFAULT_RETRIES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Fatal,
    /// Transient. `after` is the wait the provider asked for, if any.
    Retry {
        after: Option<Duration>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GaveUp {
    Fatal { retries: u32 },
    OutOfRetries { retries: u32 },
    WaitTooLong { retries: u32, asked: Duration },
}

/// What the error message adds: why fussycat stopped trying.
impl fmt::Display for GaveUp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Fatal { retries: 0 } | Self::OutOfRetries { retries: 0 } => Ok(()),
            Self::Fatal { retries } => write!(f, " (after {})", plural(retries)),
            Self::OutOfRetries { retries } => write!(f, " (gave up after {})", plural(retries)),
            Self::WaitTooLong { asked, .. } => write!(
                f,
                " (the provider asked to wait {}s; fussycat waits at most {}s)",
                asked.as_secs(),
                Policy::MAX_WAIT.as_secs()
            ),
        }
    }
}

impl GaveUp {
    pub fn retries(self) -> u32 {
        match self {
            Self::Fatal { retries }
            | Self::OutOfRetries { retries }
            | Self::WaitTooLong { retries, .. } => retries,
        }
    }
}

fn plural(retries: u32) -> String {
    match retries {
        1 => "1 retry".into(),
        n => format!("{n} retries"),
    }
}

/// Attempt until success, a fatal failure, or the policy runs out. Ok
/// carries how many retries it took.
pub async fn with_retries<T, E, Attempt, Sleep>(
    policy: Policy,
    mut attempt: impl FnMut() -> Attempt,
    verdict: impl Fn(&E) -> Verdict,
    sleep: impl Fn(Duration) -> Sleep,
) -> Result<(T, u32), (E, GaveUp)>
where
    Attempt: Future<Output = Result<T, E>>,
    Sleep: Future<Output = ()>,
{
    let mut retries = 0;
    loop {
        let error = match attempt().await {
            Ok(value) => return Ok((value, retries)),
            Err(error) => error,
        };
        let Verdict::Retry { after } = verdict(&error) else {
            return Err((error, GaveUp::Fatal { retries }));
        };
        if retries >= policy.max_retries {
            return Err((error, GaveUp::OutOfRetries { retries }));
        }
        let wait = after.unwrap_or_else(|| Policy::backoff(retries));
        if wait > Policy::MAX_WAIT {
            return Err((
                error,
                GaveUp::WaitTooLong {
                    retries,
                    asked: wait,
                },
            ));
        }
        sleep(wait).await;
        retries += 1;
    }
}

/// rig's judgement of a failure: 429s, 5xx and dropped connections are
/// worth another try; a bad request or a rejected key never is.
pub fn verdict(error: &RigError) -> Verdict {
    let report = error.report();
    if report.retryable {
        Verdict::Retry {
            after: report.retry_after(SystemTime::now()),
        }
    } else {
        Verdict::Fatal
    }
}

/// Every adapter's call: `attempt` under `policy`, failures described.
pub async fn call<T, Attempt>(
    policy: Policy,
    attempt: impl FnMut() -> Attempt,
) -> Result<(T, u32), ProviderError>
where
    Attempt: Future<Output = Result<T, RigError>>,
{
    with_retries(policy, attempt, verdict, tokio::time::sleep)
        .await
        .map_err(|(error, gave_up)| given_up(&error, gave_up))
}

/// The error the run sees: the cause, why fussycat stopped trying, and the
/// retries it took, which the run counts like a success's.
fn given_up(error: &RigError, gave_up: GaveUp) -> ProviderError {
    ProviderError::new(format!("{}{gave_up}", describe(error))).after_retries(gave_up.retries())
}

#[cfg(test)]
mod tests;
