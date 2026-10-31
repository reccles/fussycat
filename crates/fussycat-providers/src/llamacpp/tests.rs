use fussycat_core::log::Part;
use fussycat_core::provider::{Provider, Reasoning, Request, Response};
use serde_json::{Value, json};

use super::*;
use crate::testkit::{
    NO_RETRIES, assert_golden, assert_wire_contract, capped_at, chat_view, first_turn,
    reasoning_at, serve_once, wire_bytes,
};

const SERVER: &str = "http://llama.invalid:8080/v1";

fn wire(request: &Request) -> rig_core::http_client::Bytes {
    wire_bytes(
        |http| connect(SERVER, None, "qwen3.8-27b", NO_RETRIES, http),
        request,
    )
}

fn wire_json(request: &Request) -> Value {
    serde_json::from_slice(&wire(request)).unwrap()
}

#[test]
fn arch_render_is_deterministic_on_the_wire() {
    assert_golden("llamacpp-first-turn.json", &wire(&first_turn()));
}

#[test]
fn arch_nothing_volatile_above_log_on_the_wire() {
    assert_wire_contract(wire, chat_view);
}

/// Unset, the server's template decides; set, it goes on the wire as
/// `reasoning_effort`, which llama-server hands the chat template (`off`
/// turned qwen3.8's thinking off, `high` made gpt-oss think longer).
#[test]
fn reasoning_effort_goes_on_the_wire_only_when_asked() {
    assert_eq!(wire_json(&first_turn()).get("reasoning_effort"), None);
    assert_eq!(
        wire_json(&reasoning_at(Reasoning::High))["reasoning_effort"],
        json!("high")
    );
}

#[test]
fn caps_the_reply_only_when_the_budget_does() {
    assert_eq!(wire_json(&first_turn()).get("max_tokens"), None);
    assert_eq!(wire_json(&capped_at(1_000))["max_tokens"], json!(1_000));
}

/// A reply as llama-server writes one for a thinking model: the reasoning
/// in `reasoning_content`, the tool call in `tool_calls`, and the prompt
/// tokens it took from its cache.
fn tool_call_reply() -> Value {
    json!({
        "id": "chatcmpl-1", "object": "chat.completion", "created": 1_793_422_135,
        "model": "qwen3.8-27b",
        "choices": [{
            "index": 0, "finish_reason": "tool_calls",
            "message": {
                "role": "assistant", "content": "",
                "reasoning_content": "The user wants the time. I'll call now.",
                "tool_calls": [{"id": "call_1", "type": "function",
                                "function": {"name": "now", "arguments": "{}"}}],
            },
        }],
        "usage": {"prompt_tokens": 669, "completion_tokens": 44, "total_tokens": 713,
                  "prompt_tokens_details": {"cached_tokens": 665}},
        "timings": {"cache_n": 665, "prompt_n": 4, "predicted_n": 44},
    })
}

fn complete(reply: &Value) -> Response {
    let (url, server) = serve_once(reply);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let adapter = connect(
        &url,
        None,
        "qwen3.8-27b",
        NO_RETRIES,
        crate::http(crate::LOCAL_READ_TIMEOUT),
    );
    let response = runtime.block_on(adapter.complete(&first_turn()));
    server.join().unwrap();
    response.unwrap()
}

/// The reasoning is thinking (shown with -v, replayed next turn), the
/// call is a call, and the cache hit is reported as the server gave it.
#[test]
fn a_reply_keeps_its_reasoning_call_and_cache_hit() {
    let response = complete(&tool_call_reply());
    assert!(
        matches!(&response.turn.parts[0], Part::Thinking(thought) if thought.contains("I'll call now")),
        "{:?}",
        response.turn.parts
    );
    let calls: Vec<_> = response
        .turn
        .tool_calls()
        .map(|call| call.name.as_str())
        .collect();
    assert_eq!(calls, ["now"]);
    assert_eq!(response.usage.input, Some(669));
    assert_eq!(response.usage.cache_read, Some(665));
    assert_eq!(
        response.usage.cost_usd, None,
        "llama-server reports no price"
    );
}

/// A router lists every model it can load with its context; a server
/// running one model reports the context it was trained for, in `meta`.
#[test]
fn the_listing_keeps_the_context_window() {
    let models: Models = serde_json::from_value(json!({"data": [
        {"id": "qwen3.8-27b", "object": "model", "context_length": 262_144,
         "status": {"value": "loaded"}},
        {"id": "single.gguf", "object": "model", "meta": {"n_ctx_train": 131_072}},
        {"id": "bare"}
    ]}))
    .unwrap();
    let lines: Vec<String> = listed(models).iter().map(ToString::to_string).collect();
    assert_eq!(
        lines,
        [
            "llamacpp:bare",
            "llamacpp:qwen3.8-27b context=262144",
            "llamacpp:single.gguf context=131072",
        ]
    );
}
