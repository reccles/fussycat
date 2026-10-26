use std::cell::RefCell;

use rig_core::ProviderResponseError;
use rig_core::http_client::StatusCode;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fake {
    Transient,
    AskedToWait(u64),
    Fatal,
}

fn judge(error: &Fake) -> Verdict {
    match *error {
        Fake::Transient => Verdict::Retry { after: None },
        Fake::AskedToWait(secs) => Verdict::Retry {
            after: Some(Duration::from_secs(secs)),
        },
        Fake::Fatal => Verdict::Fatal,
    }
}

type Scripted = Result<(&'static str, u32), (Fake, GaveUp)>;

/// Runs `with_retries` against scripted failures, then success. Returns
/// the result and every wait it slept (in seconds), instantly.
fn script(max_retries: u32, failures: &[Fake]) -> (Scripted, Vec<u64>) {
    let mut failures = failures.iter().copied();
    let slept = RefCell::new(Vec::new());
    let result = block_on(with_retries(
        Policy { max_retries },
        || std::future::ready(failures.next().map_or(Ok("answer"), Err)),
        judge,
        |wait: Duration| {
            slept.borrow_mut().push(wait.as_secs());
            std::future::ready(())
        },
    ));
    (result, slept.into_inner())
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn transient_failures_are_retried_with_backoff_and_counted() {
    let (result, slept) = script(3, &[Fake::Transient, Fake::Transient]);
    assert_eq!(result, Ok(("answer", 2)));
    assert_eq!(slept, [1, 2]);
}

#[test]
fn a_wait_the_provider_asks_for_is_honoured() {
    let (result, slept) = script(3, &[Fake::AskedToWait(7)]);
    assert_eq!(result, Ok(("answer", 1)));
    assert_eq!(slept, [7]);
}

#[test]
fn a_fatal_failure_is_never_retried() {
    let (result, slept) = script(3, &[Fake::Fatal]);
    assert_eq!(result, Err((Fake::Fatal, GaveUp::Fatal { retries: 0 })));
    assert_eq!(slept, [0u64; 0], "never slept");
}

#[test]
fn retries_run_out() {
    let (result, slept) = script(2, &[Fake::Transient; 5]);
    assert_eq!(
        result,
        Err((Fake::Transient, GaveUp::OutOfRetries { retries: 2 }))
    );
    assert_eq!(slept, [1, 2]);
    let (result, _) = script(0, &[Fake::Transient]);
    assert_eq!(
        result,
        Err((Fake::Transient, GaveUp::OutOfRetries { retries: 0 })),
        "--retries 0 fails fast"
    );
}

#[test]
fn a_wait_longer_than_the_cap_fails_instead_of_stalling() {
    let (result, slept) = script(3, &[Fake::AskedToWait(3600)]);
    let Err((_, gave_up)) = result else {
        panic!("waited an hour")
    };
    assert_eq!(slept, [0u64; 0], "never slept");
    assert_eq!(
        gave_up.to_string(),
        " (the provider asked to wait 3600s; fussycat waits at most 30s)"
    );
}

#[test]
fn backoff_doubles_up_to_the_cap() {
    let waits: Vec<u64> = (0..7).map(|n| Policy::backoff(n).as_secs()).collect();
    assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30]);
}

#[test]
fn the_message_says_why_fussycat_stopped() {
    assert_eq!(GaveUp::Fatal { retries: 0 }.to_string(), "");
    assert_eq!(
        GaveUp::OutOfRetries { retries: 0 }.to_string(),
        "",
        "--retries 0: nothing to say"
    );
    assert_eq!(GaveUp::Fatal { retries: 1 }.to_string(), " (after 1 retry)");
    assert_eq!(
        GaveUp::OutOfRetries { retries: 3 }.to_string(),
        " (gave up after 3 retries)"
    );
}

#[test]
fn rig_judges_rate_limits_and_server_errors_transient_but_not_bad_requests() {
    let replied = |status| RigError::ProviderResponse(ProviderResponseError::new(status, "{}"));
    for status in [
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        assert!(
            matches!(verdict(&replied(status)), Verdict::Retry { .. }),
            "{status}"
        );
    }
    for status in [
        StatusCode::BAD_REQUEST,
        StatusCode::UNAUTHORIZED,
        StatusCode::NOT_FOUND,
    ] {
        assert_eq!(verdict(&replied(status)), Verdict::Fatal, "{status}");
    }
}
