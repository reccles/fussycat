use fussycat_core::report::prefix_key;
use serde_json::{Value, json};

use super::*;
use crate::providers::testkit::{
    NO_RETRIES, assert_golden, assert_wire_contract, capped_at, chat_view, first_turn, paid_key,
    reasoning_at, refusal, serve_once, wire_bytes,
};
use fussycat_core::log::Part;
use fussycat_core::provider::Reasoning;

/// The paid test's model: cheap, with priced cache reads.
const TEST_MODEL: &str = "claude-haiku-5-5";

fn wire(request: &Request) -> rig_core::http_client::Bytes {
    wire_bytes(
        |http| Venice::with_http("not-a-real-key".into(), TEST_MODEL, NO_RETRIES, http),
        request,
    )
}

fn wire_json(request: &Request) -> Value {
    serde_json::from_slice(&wire(request)).unwrap()
}

#[test]
fn arch_render_is_deterministic_on_the_wire() {
    assert_golden("venice-first-turn.json", &wire(&first_turn()));
}

#[test]
fn arch_nothing_volatile_above_log_on_the_wire() {
    assert_wire_contract(wire, chat_view);
}

#[test]
fn never_lets_venice_add_its_own_system_prompt() {
    let wire = wire_json(&first_turn());
    assert_eq!(
        wire["venice_parameters"]["include_venice_system_prompt"],
        json!(false)
    );
}

#[test]
fn routes_the_cache_by_the_prefix() {
    let wire = wire_json(&first_turn());
    assert_eq!(
        wire["prompt_cache_key"],
        json!(format!("fussycat-{}", prefix_key(&first_turn())))
    );
}

/// Unset, Venice's own default decides (and a reply it cuts off fails the
/// run); set, the budget's limit goes on the wire.
#[test]
fn caps_the_reply_only_when_the_budget_does() {
    assert_eq!(wire_json(&first_turn()).get("max_tokens"), None);
    assert_eq!(wire_json(&capped_at(1_000))["max_tokens"], json!(1_000));
}

/// Unset, Venice picks the effort per model (`qwen-3-8-27b` defaults to
/// `xhigh`); set, it goes on the wire as `reasoning_effort`.
#[test]
fn sends_reasoning_effort_only_when_asked() {
    assert_eq!(wire_json(&first_turn()).get("reasoning_effort"), None);
    assert_eq!(
        wire_json(&reasoning_at(Reasoning::Low))["reasoning_effort"],
        json!("low")
    );
    assert_eq!(
        wire_json(&reasoning_at(Reasoning::Off))["reasoning_effort"],
        json!("none")
    );
}

/// Each model takes its own levels (`qwen-3-8-27b`: none, low, medium,
/// xhigh, as `/models` lists): one it lacks fails unsent.
#[test]
fn a_level_the_model_lacks_is_refused_unsent() {
    let qwen = |http| Venice::with_http("not-a-real-key".into(), "qwen-3-8-27b", NO_RETRIES, http);
    let error = refusal(qwen, &reasoning_at(Reasoning::High));
    assert!(error.contains("no `high` effort level"), "{error}");
}

#[test]
fn the_key_never_reaches_the_body() {
    let body = wire(&first_turn());
    assert!(!String::from_utf8_lossy(&body).contains("not-a-real-key"));
}

fn reply(raw: &Value) -> CompletionResponse {
    serde_json::from_value(json!({
        "choice": [],
        "usage": {"input_tokens": 10, "output_tokens": 2},
        "origin": {"api": "chat", "provider": "venice", "model": TEST_MODEL},
        "raw": raw
    }))
    .unwrap()
}

#[test]
fn cost_is_what_venice_reported() {
    let priced = reply(&json!({"cost": {"usd": 0.000_042, "diem": 0.000_042}}));
    assert_eq!(reported_cost(&priced), Some(0.000_042));
    assert_eq!(
        reported_cost(&reply(&json!({}))),
        None,
        "unreported is unknown"
    );
}

/// A Venice reply as a thinking model writes it (`venice:qwen-3-8-27b`):
/// the reasoning in `reasoning_content`, the answer in `content`.
fn thinking_reply(content: &str) -> Value {
    json!({
        "id": "chatcmpl-1", "object": "chat.completion", "created": 1_761_696_000,
        "model": TEST_MODEL,
        "choices": [{
            "index": 0, "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "reasoning_content": "The user wants the time. I have no clock tool.",
                "content": content,
            },
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 12, "total_tokens": 22},
    })
}

/// Venice at `url`, as [`Venice::new`] builds it.
fn venice_at(url: &str) -> Venice {
    let client = OpenAIConfig::with_key(&VENICE, "not-a-real-key")
        .with_base_url(url)
        .connect(crate::providers::http());
    Venice {
        model: client.chat(TEST_MODEL),
        retry: NO_RETRIES,
    }
}

fn complete(reply: &Value) -> Response {
    let (url, server) = serve_once(reply);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let response = runtime.block_on(venice_at(&url).complete(&first_turn()));
    server.join().unwrap();
    response.unwrap()
}

/// `-v` and the log show a thinking model's reasoning, and it's replayed
/// on later turns: it arrives as thinking, before the answer.
#[test]
fn reasoning_content_is_thinking() {
    let response = complete(&thinking_reply("I can't tell the time."));
    assert_eq!(
        response.turn.parts,
        [
            Part::Thinking("The user wants the time. I have no clock tool.".into()),
            Part::Text("I can't tell the time.".into()),
        ]
    );
}

/// The failure that was seen: all the output went to reasoning, and
/// `content` came back empty. The reasoning still shows.
#[test]
fn reasoning_with_no_answer_still_shows() {
    let response = complete(&thinking_reply(""));
    assert!(
        response.turn.parts.contains(&Part::Thinking(
            "The user wants the time. I have no clock tool.".into()
        )),
        "{:?}",
        response.turn.parts
    );
}

/// The reasoning goes back to Venice on the next turn, in the field it
/// arrived in, so the model keeps its own train of thought.
#[test]
fn reasoning_is_replayed_on_the_next_turn() {
    let turn = complete(&thinking_reply("Checking.")).turn;
    let mut next = first_turn();
    next.log.push(fussycat_core::log::Entry::Assistant(turn));
    next.log
        .push(fussycat_core::log::Entry::Task("And now?".into()));
    let wire = wire_json(&next);
    let assistant = wire["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "assistant")
        .expect("the assistant turn is replayed");
    assert_eq!(
        assistant["reasoning_content"],
        json!("The user wants the time. I have no clock tool."),
        "{assistant}"
    );
}

/// Costs real money (fractions of a cent). Run on purpose:
/// `FUSSYCAT_VENICE_KEY_COMMAND='<a command that prints the key>' cargo test -p fussycat venice -- --ignored`
#[test]
#[ignore = "paid: calls the Venice API"]
fn paid_a_real_call_reports_usage_and_cost() {
    let key = paid_key(crate::providers::ProviderName::Venice);
    let venice = Venice::new(key, TEST_MODEL, Policy::default());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut request = first_turn();
    request.log = vec![fussycat_core::log::Entry::Task(
        "Reply with the word ok.".into(),
    )];
    let response = runtime.block_on(venice.complete(&request)).unwrap();
    assert!(response.usage.input.is_some_and(|tokens| tokens > 0));
    assert!(
        response.usage.cost_usd.is_some_and(|usd| usd > 0.0),
        "{:?}",
        response.usage
    );
}

#[test]
fn the_listing_keeps_tool_support_and_prices() {
    let models: Models = serde_json::from_value(json!({"data": [
        {"id": "claude-haiku-5-5", "context_length": 1_000_000, "type": "text",
         "model_spec": {
            "pricing": {"input": {"usd": 0.1, "diem": 0.1}, "output": {"usd": 0.5, "diem": 0.5},
                        "cache_input": {"usd": 0.01, "diem": 0.01}},
            "capabilities": {"supportsFunctionCalling": true, "supportsVision": true}}},
        {"id": "plain-model", "context_length": 32_768,
         "model_spec": {"pricing": {"input": {"usd": 0.2}, "output": {"usd": 0.9}},
                        "capabilities": {"supportsFunctionCalling": false}}},
        {"id": "bare"}
    ]}))
    .unwrap();
    let lines: Vec<String> = listed(models).iter().map(ToString::to_string).collect();
    assert_eq!(
        lines,
        [
            "venice:bare",
            "venice:claude-haiku-5-5 context=1000000 tools=yes in=$0.1/M out=$0.5/M cached=$0.01/M",
            "venice:plain-model context=32768 tools=no in=$0.2/M out=$0.9/M",
        ]
    );
}

/// The stable blocks travel as one joined system
/// message, because some backends reject or drop all but the first. Both
/// blocks must reach the model: lowercase from the first, the word from the
/// second.
#[test]
#[ignore = "paid: calls the Venice API"]
fn paid_every_stable_block_reaches_the_model() {
    let key = paid_key(crate::providers::ProviderName::Venice);
    let venice = Venice::new(key, TEST_MODEL, Policy::default());
    let block = |name: &str, content: &str| fussycat_core::quilt::Block {
        name: fussycat_core::quilt::BlockName::new(name).unwrap(),
        content: content.into(),
    };
    let request = Request {
        tools: vec![],
        stable: vec![
            block("a.md", "Always answer in lowercase letters only."),
            block(
                "b.md",
                "The secret word is PLUM. When asked for the secret word, reply with it and nothing else.",
            ),
        ],
        log: vec![fussycat_core::log::Entry::Task(
            "What is the secret word?".into(),
        )],
        live: vec![],
        max_output_tokens: None,
        reasoning: None,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let response = runtime.block_on(venice.complete(&request)).unwrap();
    assert_eq!(response.turn.text().trim(), "plum");
}
