use std::sync::Arc;

use rig_core::ProviderResponseError;
use rig_core::http_client::{Error as HttpError, StatusCode};

use super::*;

fn replied(status: StatusCode, body: &str) -> RigError {
    RigError::ProviderResponse(ProviderResponseError::new(status, body))
}

#[test]
fn a_reply_is_its_status_and_the_providers_own_words() {
    let ollama = replied(
        StatusCode::NOT_FOUND,
        r#"{"error":"model 'no-such-model' not found"}"#,
    );
    assert_eq!(
        describe(&ollama),
        "HTTP 404: model 'no-such-model' not found"
    );
    let openai = replied(
        StatusCode::TOO_MANY_REQUESTS,
        r#"{"error":{"message":"Rate limit reached","type":"requests"}}"#,
    );
    assert_eq!(describe(&openai), "HTTP 429: Rate limit reached");
}

#[test]
fn a_reply_without_an_envelope_is_quoted_and_bounded() {
    let html = format!("<html>{}</html>", "x".repeat(1000));
    let described = describe(&replied(StatusCode::BAD_GATEWAY, &html));
    assert!(described.starts_with("HTTP 502: <html>xxx"), "{described}");
    assert!(described.ends_with('…'));
    assert!(described.chars().count() < 320);
}

/// reqwest's shape: the request that failed, wrapping why.
#[derive(Debug, thiserror::Error)]
#[error("error sending request for url (http://localhost:1/api/chat)")]
struct Sending(#[source] std::io::Error);

#[test]
fn a_transport_failure_names_the_request_and_its_root_cause() {
    let refused = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "Connection refused");
    let error = RigError::Http(Arc::new(HttpError::Instance(Box::new(Sending(refused)))));
    assert_eq!(
        describe(&error),
        "error sending request for url (http://localhost:1/api/chat): Connection refused"
    );
}

/// Not only replies: a decode error or a transport's message can carry a
/// body too, so every description is bounded.
#[test]
fn every_description_is_bounded() {
    let bounded = |described: String| {
        assert!(described.ends_with('…'), "{described}");
        assert!(described.chars().count() <= QUOTE_LIMIT + 1, "{described}");
    };
    let body = "x".repeat(1000);
    bounded(describe(&RigError::Response(body.clone())));
    bounded(with_root_cause(&std::io::Error::other(body.clone())));
    let refused = std::io::Error::other(body);
    bounded(describe(&RigError::Http(Arc::new(HttpError::Instance(
        Box::new(Sending(refused)),
    )))));
}

#[test]
fn an_error_without_a_cause_is_just_itself() {
    let alone = std::io::Error::other("timed out");
    assert_eq!(with_root_cause(&alone), "timed out");
}
