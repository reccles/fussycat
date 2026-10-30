use serde_json::{Value, json};

use super::*;
use crate::providers::testkit::NO_RETRIES;
use crate::providers::testkit::{
    assert_golden, assert_wire_contract, capped_at, chat_view, first_turn, reasoning_at, refusal,
    second_turn, wire_bytes,
};
use fussycat_core::provider::Reasoning;

fn wire(request: &Request) -> rig_core::http_client::Bytes {
    wire_bytes(
        |http| {
            Ollama::with_http(
                "http://ollama.invalid:11434",
                "gpt-oss:20b",
                NO_RETRIES,
                http,
            )
        },
        request,
    )
}

fn wire_json(request: &Request) -> Value {
    serde_json::from_slice(&wire(request)).unwrap()
}

fn ollama(http: rig_core::http_client::DynHttpClient) -> Ollama {
    Ollama::with_http(
        "http://ollama.invalid:11434",
        "gpt-oss:20b",
        NO_RETRIES,
        http,
    )
}

/// Ollama's `think`: `false`, or a level. gpt-oss follows the levels
/// but can't stop thinking; Qwen 3 can, but treats every level as on.
#[test]
fn reasoning_goes_on_the_wire_as_think() {
    assert_eq!(wire_json(&first_turn()).get("think"), None);
    assert_eq!(
        wire_json(&reasoning_at(Reasoning::High))["think"],
        json!("high")
    );
    let qwen =
        |http| Ollama::with_http("http://ollama.invalid:11434", "qwen3:8b", NO_RETRIES, http);
    let off: Value =
        serde_json::from_slice(&wire_bytes(qwen, &reasoning_at(Reasoning::Off))).unwrap();
    assert_eq!(off["think"], json!(false));
}

/// Ollama has no `xhigh` or `minimal`: the run fails before anything is
/// sent, rather than think at a level nobody asked for.
#[test]
fn a_level_ollama_lacks_is_refused_unsent() {
    let error = refusal(ollama, &reasoning_at(Reasoning::XHigh));
    assert!(error.contains("no `xhigh` thinking level"), "{error}");
    let error = refusal(ollama, &reasoning_at(Reasoning::Off));
    assert!(error.contains("cannot be turned off"), "{error}");
}

#[test]
fn arch_render_is_deterministic_on_the_wire() {
    assert_golden("ollama-first-turn.json", &wire(&first_turn()));
}

#[test]
fn arch_nothing_volatile_above_log_on_the_wire() {
    assert_wire_contract(wire, chat_view);
}

#[test]
fn never_lets_ollama_truncate() {
    assert_eq!(wire_json(&first_turn())["truncate"], json!(false));
}

#[test]
fn never_sends_runner_settings_that_force_a_model_reload() {
    let wire = wire_json(&second_turn());
    assert_eq!(wire.get("shift"), None);
    assert_eq!(wire.get("options"), None);
}

/// Unset, nothing is sent (Ollama's default: until the model stops or the
/// context fills). Set, it's `num_predict`, a per-request option.
#[test]
fn caps_the_reply_only_when_the_budget_does() {
    assert_eq!(wire_json(&first_turn()).get("options"), None);
    assert_eq!(
        wire_json(&capped_at(1_000))["options"],
        json!({"num_predict": 1_000})
    );
}

#[test]
fn the_listing_keeps_context_and_tool_support() {
    let tags: Tags = serde_json::from_value(json!({"models": [
        {"name": "qwen3-coder:30b", "size": 1,
         "details": {"family": "qwen3moe", "context_length": 262_144},
         "capabilities": ["completion", "tools"]},
        {"name": "gemma2:2b", "details": {"context_length": 8192},
         "capabilities": ["completion"]},
        {"name": "old:latest", "details": {}}
    ]}))
    .unwrap();
    let lines: Vec<String> = listed(tags).iter().map(ToString::to_string).collect();
    assert_eq!(
        lines,
        [
            "ollama:gemma2:2b context=8192 tools=no",
            "ollama:old:latest",
            "ollama:qwen3-coder:30b context=262144 tools=yes",
        ]
    );
}

/// An Ollama that takes the connection and never answers, through a client
/// with a tiny read timeout (the real one is minutes): the call fails like a
/// dropped connection, so it's worth a retry, and it fails as a normal
/// provider error, not a hang.
#[tokio::test]
async fn a_stalled_ollama_times_out_and_is_worth_a_retry() {
    use std::time::{Duration, Instant};

    use crate::providers::retry::{Verdict, verdict};

    // The OS accepts connections into the backlog; nothing ever answers.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", silent.local_addr().unwrap());
    let client = super::super::client_with(Duration::from_secs(1), Duration::from_millis(100));
    let http = DynHttpClient::new(rig_reqwest::ReqwestClient::from(client));
    let ollama = Ollama::with_http(&url, "m", NO_RETRIES, http);
    let started = Instant::now();

    let rig_request = crate::providers::messages::to_rig(&first_turn()).unwrap();
    let error = ollama.model.call(rig_request).await.unwrap_err();
    assert!(
        matches!(verdict(&error), Verdict::Retry { .. }),
        "{error:?}"
    );

    let error = ollama.complete(&first_turn()).await.unwrap_err();
    assert!(error.message.contains("timed out"), "{error}");
    // Two 100 ms reads, but a loaded machine can add a dropped SYN's 1 s
    // retransmit to a connect. A hang would be the real READ_TIMEOUT
    // (minutes), so seconds still tell them apart.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}
