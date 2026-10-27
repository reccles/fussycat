use serde_json::{Value, json};

use super::*;
use crate::providers::ProviderName;
use crate::providers::testkit::{
    NO_RETRIES, assert_golden, assert_wire_contract, capped_at, first_turn, messages_view,
    paid_key, second_turn, wire_bytes, with_live,
};

/// The paid tests' model: the cheapest.
const TEST_MODEL: &str = "claude-haiku-5-5";

fn wire(request: &Request) -> rig_core::http_client::Bytes {
    wire_for(TEST_MODEL, request)
}

fn wire_for(model: &str, request: &Request) -> rig_core::http_client::Bytes {
    wire_bytes(
        |http| Anthropic::with_http("not-a-real-key".into(), model, NO_RETRIES, http),
        request,
    )
}

fn wire_json(request: &Request) -> Value {
    serde_json::from_slice(&wire(request)).unwrap()
}

fn max_tokens(model: &str, request: &Request) -> Value {
    serde_json::from_slice::<Value>(&wire_for(model, request)).unwrap()["max_tokens"].clone()
}

#[test]
fn arch_render_is_deterministic_on_the_wire() {
    assert_golden("anthropic-first-turn.json", &wire(&first_turn()));
}

#[test]
fn arch_nothing_volatile_above_log_on_the_wire() {
    assert_wire_contract(wire, messages_view);
}

/// Every `cache_control` marker in `wire`, as the JSON path to its block.
fn markers(value: &Value, path: &str, found: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(fields) => {
            if let Some(marker) = fields.get("cache_control") {
                found.push((path.to_owned(), marker.clone()));
            }
            for (key, field) in fields {
                markers(field, &format!("{path}.{key}"), found);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                markers(item, &format!("{path}[{index}]"), found);
            }
        }
        _ => {}
    }
}

/// A1: one marker at each seam (the last tool, the system prompt, the end
/// of the log), none on the live block, and all at the 5-minute default
/// (no `ttl`). Three of Anthropic's four.
#[test]
fn the_seams_are_the_cache_markers_and_live_is_never_cached() {
    let wire = wire_json(&with_live(second_turn(), "<live>noon</live>"));
    let mut found = Vec::new();
    markers(&wire, "", &mut found);
    let paths: Vec<&str> = found.iter().map(|(path, _)| path.as_str()).collect();
    assert_eq!(
        paths,
        [".messages[2].content[0]", ".system[0]", ".tools[0]"],
        "the log's last block (the tool result), the system prompt, the last tool"
    );
    for (path, marker) in &found {
        assert_eq!(marker, &json!({"type": "ephemeral"}), "{path}: 5 minutes");
    }
    let live = &wire["messages"][2]["content"][1];
    assert_eq!(live["text"], "<live>noon</live>");
    assert!(live.get("cache_control").is_none());
}

/// Anthropic requires `max_tokens`, and rig's catalog doesn't list models
/// newer than itself: they get 64k, as they always did.
#[test]
fn every_request_caps_its_reply() {
    assert_eq!(max_tokens(TEST_MODEL, &first_turn()), MAX_TOKENS);
}

/// A model whose output limit is under 64k would reject 64k with a
/// 400. rig's catalog lists Opus 4.1 at 32k; one listed higher is still
/// asked for 64k at most.
#[test]
fn a_listed_model_is_never_asked_for_more_than_its_limit() {
    assert_eq!(max_tokens("claude-opus-4-1", &first_turn()), 32_000);
    assert_eq!(max_tokens("claude-sonnet-5-5", &first_turn()), MAX_TOKENS);
    assert_eq!(default_max_tokens(Some(8_192)), 8_192);
    assert_eq!(default_max_tokens(None), MAX_TOKENS);
}

#[test]
fn the_budgets_limit_wins_over_the_default() {
    assert_eq!(max_tokens(TEST_MODEL, &capped_at(1_000)), 1_000);
    assert_eq!(max_tokens("claude-opus-4-1", &capped_at(100_000)), 100_000);
}

#[test]
fn the_key_never_reaches_the_body() {
    let body = wire(&first_turn());
    assert!(!String::from_utf8_lossy(&body).contains("not-a-real-key"));
}

#[test]
fn the_listing_keeps_the_context_window() {
    let models: Models = serde_json::from_value(json!({"data": [
        {"type": "model", "id": "claude-sonnet-5-5", "display_name": "Claude Sonnet 5.5",
         "max_input_tokens": 1_000_000, "max_tokens": 128_000},
        {"type": "model", "id": "claude-haiku-5-5", "max_input_tokens": 1_000_000},
        {"type": "model", "id": "bare"}
    ], "has_more": false}))
    .unwrap();
    let lines: Vec<String> = listed(models).iter().map(ToString::to_string).collect();
    assert_eq!(
        lines,
        [
            "anthropic:bare",
            "anthropic:claude-haiku-5-5 context=1000000",
            "anthropic:claude-sonnet-5-5 context=1000000",
        ]
    );
}

/// Costs real money (about a cent). Run on purpose:
/// `FUSSYCAT_ANTHROPIC_KEY_COMMAND='<a command that prints the key>' cargo test -p fussycat anthropic -- --ignored`
///
/// The same request twice: the first writes the prefix to the cache, the
/// second reads it back. The prefix must pass Haiku's cacheable minimum, so
/// the stable block is padded. No cost is reported: Anthropic gives tokens.
#[test]
#[ignore = "paid: calls the Anthropic API"]
fn paid_the_prefix_is_written_then_read_from_the_cache() {
    let anthropic = Anthropic::new(
        paid_key(ProviderName::Anthropic),
        TEST_MODEL,
        Policy::default(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut request = first_turn();
    // Unique per run, so an earlier run's cache can't answer the first call.
    let run = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    request.stable[0].content = format!(
        "Run {run}. Be terse.\n\n{}",
        "Reference: the sky is blue, grass is green, snow is white.\n".repeat(400)
    );
    request.log = vec![fussycat_core::log::Entry::Task(
        "Reply with the word ok.".into(),
    )];
    let write = runtime.block_on(anthropic.complete(&request)).unwrap();
    let read = runtime.block_on(anthropic.complete(&request)).unwrap();
    assert!(
        write.usage.cache_write.is_some_and(|tokens| tokens > 0),
        "{:?}",
        write.usage
    );
    assert!(
        read.usage.cache_read.is_some_and(|tokens| tokens > 0),
        "{:?}",
        read.usage
    );
    assert!(
        read.usage.input >= read.usage.cache_read,
        "input counts every prompt token, cached ones included: {:?}",
        read.usage
    );
    assert_eq!(read.usage.cost_usd, None, "Anthropic reports no dollars");
}
