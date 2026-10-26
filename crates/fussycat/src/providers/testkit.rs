//! Offline provider tests: capture the bytes an adapter would send, and the
//! wire contract every provider must keep. Nothing here leaves the machine.

use std::sync::{Arc, Mutex};

use fussycat_core::log::{AssistantTurn, Entry, ProviderData};
use fussycat_core::provider::{Provider, Request};
use fussycat_core::quilt::{Block, BlockName};
use fussycat_core::toolbox::{ToolResult, ToolSpec};
use rig_core::http_client::{Bytes, DynHttpClient, Error, Exchange, HeaderMap, HttpMiddleware};
use rig_core::message::{AssistantContent, AssistantMessage, Message, ToolName};
use rig_core::wasm_compat::WasmBoxedFuture;
use serde_json::{Value, json};

/// The capture's abort is a transport error, which is retryable: tests
/// must not sleep through backoff for it.
pub const NO_RETRIES: super::retry::Policy = super::retry::Policy { max_retries: 0 };

/// Records each request body, then aborts it.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<Bytes>>>);

impl HttpMiddleware for Capture {
    fn before_request_body<'a>(
        &'a self,
        _exchange: Exchange<'a>,
        _headers: &'a HeaderMap,
        body: Bytes,
    ) -> WasmBoxedFuture<'a, rig_core::http_client::Result<Bytes>> {
        self.0.lock().unwrap().push(body);
        Box::pin(async { Err(Error::Instance("captured, not sent".into())) })
    }
}

/// The bytes `connect`'s provider would send for `request`.
pub fn wire_bytes<P: Provider>(
    connect: impl FnOnce(DynHttpClient) -> P,
    request: &Request,
) -> Bytes {
    let capture = Capture::default();
    let provider = connect(super::http().with_middleware(capture.clone()));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let outcome = runtime.block_on(provider.complete(request));
    assert!(outcome.is_err(), "the capture aborts every request");
    capture.0.lock().unwrap().pop().expect("a request body")
}

pub fn first_turn() -> Request {
    Request {
        tools: vec![ToolSpec {
            name: "now".into(),
            description: "Runs `date -u` and returns its output. Takes no arguments.".into(),
            parameters: json!({"type": "object", "properties": {}}),
        }],
        stable: vec![Block {
            name: BlockName::new("core.md").unwrap(),
            content: "Be terse.".into(),
        }],
        log: vec![Entry::Task("What time is it?".into())],
        live: vec![],
        max_output_tokens: None,
    }
}

/// [`first_turn`], with the budget asking for replies of at most `tokens`.
pub fn capped_at(tokens: u64) -> Request {
    Request {
        max_output_tokens: Some(tokens),
        ..first_turn()
    }
}

/// A second turn: the model called `now` and the tool answered.
pub fn second_turn() -> Request {
    let name = ToolName::new("now").unwrap();
    let assistant = Message::Assistant(AssistantMessage::new(vec![AssistantContent::tool_call(
        "call_1",
        name,
        json!({}),
    )]));
    let mut request = first_turn();
    request.log.push(Entry::Assistant(AssistantTurn {
        parts: vec![],
        provider_data: Some(ProviderData(serde_json::to_value(assistant).unwrap())),
    }));
    request.log.push(Entry::ToolResults(vec![ToolResult {
        call_id: "call_1".into(),
        name: "now".into(),
        output: Ok("Fri Oct 23 20:15:00 UTC 2026\n".into()),
    }]));
    request
}

pub fn with_live(mut request: Request, content: &str) -> Request {
    request.live = vec![Block {
        name: BlockName::new("date -u").unwrap(),
        content: content.into(),
    }];
    request
}

/// A paid test's key, from the provider's key command; panics with how to
/// set it.
pub fn paid_key(provider: super::ProviderName) -> String {
    let env = provider.key_command_env().expect("a provider with a key");
    let command = std::env::var(env)
        .unwrap_or_else(|_| panic!("set {env} to a command that prints the key to run paid tests"));
    crate::system::read_key(env, &command).unwrap_or_else(|error| panic!("{error}"))
}

/// Golden bytes: a rig upgrade that changes what goes on the wire fails
/// here first, not as a silent cache miss. Review the diff, then
/// regenerate deliberately with `UPDATE_GOLDEN=1 cargo test`.
pub fn assert_golden(name: &str, bytes: &[u8]) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, bytes).unwrap();
    }
    let golden = std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert!(
        golden == bytes,
        "wire bytes differ from {}:\n{}",
        path.display(),
        String::from_utf8_lossy(bytes)
    );
}

/// A message's text, however the wire encodes it: a string (Ollama), an
/// array of text parts (`OpenAI` Chat Completions, Anthropic), or none.
fn text(content: &Value) -> String {
    match content {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect(),
        other => panic!("unexpected content: {other}"),
    }
}

/// One thing said on the wire, whatever the dialect: who, what, and the
/// tools it called. What [`assert_wire_contract`] checks.
#[derive(Debug, Clone, PartialEq)]
pub struct Said {
    pub role: String,
    pub text: String,
    pub calls: Vec<String>,
}

impl Said {
    fn new(role: &str, text: String) -> Self {
        Self {
            role: role.into(),
            text,
            calls: Vec::new(),
        }
    }
}

/// Chat Completions (Ollama, Venice): one entry per message, system ones
/// included, a tool result said by `tool`.
pub fn chat_view(wire: &Value) -> Vec<Said> {
    wire["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| Said {
            calls: message["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|call| call["function"]["name"].as_str().unwrap().to_owned())
                .collect(),
            ..Said::new(message["role"].as_str().unwrap(), text(&message["content"]))
        })
        .collect()
}

/// Anthropic Messages: the top-level `system` first, then one entry per
/// content block, so a `tool_result` (said by `tool`) and the live text
/// sharing one user message are two entries, as they'd be in Chat.
pub fn messages_view(wire: &Value) -> Vec<Said> {
    let system = wire
        .get("system")
        .map(|system| Said::new("system", text(system)));
    let blocks = wire["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| {
            let role = message["role"].as_str().unwrap();
            message["content"]
                .as_array()
                .unwrap()
                .iter()
                .map(move |block| match block["type"].as_str().unwrap() {
                    "text" => Said::new(role, text(&block["text"])),
                    "tool_use" => Said {
                        calls: vec![block["name"].as_str().unwrap().to_owned()],
                        ..Said::new(role, String::new())
                    },
                    "tool_result" => Said::new("tool", text(&block["content"])),
                    other => panic!("unexpected block: {other}"),
                })
        });
    system.into_iter().chain(blocks).collect()
}

/// What every provider must put on the wire (B2, B3 and the layout), read
/// through its dialect's `view`: deterministic bytes, the stable blocks as
/// one leading system message, a tool result answering its call, and live
/// blocks last, after a prefix that doesn't change when they do.
pub fn assert_wire_contract(wire: impl Fn(&Request) -> Bytes, view: fn(&Value) -> Vec<Said>) {
    assert_eq!(
        wire(&first_turn()),
        wire(&first_turn()),
        "B2: deterministic"
    );

    let said = |request: &Request| view(&serde_json::from_slice(&wire(request)).unwrap());
    let roles = |said: &[Said]| -> Vec<String> { said.iter().map(|s| s.role.clone()).collect() };

    let first = said(&first_turn());
    assert_eq!(roles(&first), ["system", "user"]);
    assert_eq!(first[0].text, "Be terse.");

    let mut two_blocks = first_turn();
    two_blocks.stable.push(Block {
        name: BlockName::new("brief.md").unwrap(),
        content: "Cite every URL.".into(),
    });
    let joined = said(&two_blocks);
    assert_eq!(
        roles(&joined),
        ["system", "user"],
        "one system message, however many stable blocks"
    );
    assert_eq!(joined[0].text, "Be terse.\n\nCite every URL.");

    let second = said(&second_turn());
    assert_eq!(roles(&second), ["system", "user", "assistant", "tool"]);
    assert_eq!(second[2].calls, ["now"]);
    assert_eq!(second[3].text, "Fri Oct 23 20:15:00 UTC 2026\n");

    let live = said(&with_live(second_turn(), "<live>noon</live>"));
    assert_eq!(
        live.last(),
        Some(&Said::new("user", "<live>noon</live>".into()))
    );

    let monday = wire(&with_live(second_turn(), "<live>monday</live>"));
    let tuesday = wire(&with_live(second_turn(), "<live>tuesday</live>"));
    let shared = monday
        .iter()
        .zip(tuesday.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let live_starts = std::str::from_utf8(&monday)
        .unwrap()
        .find("<live>")
        .unwrap();
    assert!(
        shared >= live_starts,
        "B3: bytes differ at {shared}, before the live block at {live_starts}"
    );
}
