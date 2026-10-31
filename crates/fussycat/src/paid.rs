//! Tests that call the real provider APIs and cost real money, so they're
//! `#[ignore = "paid"]` and run only on purpose. They live in the binary
//! because the key comes from the provider's key command, which only the
//! binary runs:
//!
//! `FUSSYCAT_VENICE_KEY_COMMAND='<a command that prints the key>' cargo test -p fussycat venice -- --ignored`
//! `FUSSYCAT_ANTHROPIC_KEY_COMMAND='<a command that prints the key>' cargo test -p fussycat anthropic -- --ignored`

use fussycat_core::log::Entry;
use fussycat_core::provider::{Provider, Request, Response};
use fussycat_core::quilt::{Block, BlockName};
use fussycat_core::toolbox::ToolSpec;
use fussycat_providers::{ModelRef, RetryPolicy};

/// `model`, connected with the key its provider's key command prints;
/// panics with how to set it.
fn connect(model: &str) -> fussycat_providers::Adapter {
    let model: ModelRef = model.parse().unwrap();
    let env = model
        .provider
        .key_command_env()
        .expect("a provider with a key");
    let command = std::env::var(env)
        .unwrap_or_else(|_| panic!("set {env} to a command that prints the key to run paid tests"));
    let key = crate::system::read_key(env, &command).unwrap_or_else(|error| panic!("{error}"));
    model
        .connect("http://unused", Some(key), RetryPolicy::default())
        .unwrap()
}

fn block(name: &str, content: &str) -> Block {
    Block {
        name: BlockName::new(name).unwrap(),
        content: content.into(),
    }
}

fn request(stable: Vec<Block>, task: &str) -> Request {
    Request {
        tools: vec![ToolSpec {
            name: "now".into(),
            description: "Runs `date -u` and returns its output. Takes no arguments.".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }],
        stable,
        log: vec![Entry::Task(task.into())],
        live: vec![],
        max_output_tokens: None,
        reasoning: None,
    }
}

fn complete(provider: &impl Provider, request: &Request) -> Response {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(provider.complete(request))
        .unwrap()
}

/// About a cent. The same request twice: the first writes the prefix to
/// the cache, the second reads it back. The prefix must pass Haiku's
/// cacheable minimum, so the stable block is padded. No cost is reported:
/// Anthropic gives tokens.
#[test]
#[ignore = "paid: calls the Anthropic API"]
fn anthropic_the_prefix_is_written_then_read_from_the_cache() {
    let anthropic = connect("anthropic:claude-haiku-5-5");
    // Unique per run, so an earlier run's cache can't answer the first call.
    let run = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let padded = format!(
        "Run {run}. Be terse.\n\n{}",
        "Reference: the sky is blue, grass is green, snow is white.\n".repeat(400)
    );
    let request = request(vec![block("core.md", &padded)], "Reply with the word ok.");
    let write = complete(&anthropic, &request);
    let read = complete(&anthropic, &request);
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

/// Fractions of a cent, on a cheap model with priced cache reads.
#[test]
#[ignore = "paid: calls the Venice API"]
fn venice_a_real_call_reports_usage_and_cost() {
    let venice = connect("venice:claude-haiku-5-5");
    let request = request(
        vec![block("core.md", "Be terse.")],
        "Reply with the word ok.",
    );
    let response = complete(&venice, &request);
    assert!(response.usage.input.is_some_and(|tokens| tokens > 0));
    assert!(
        response.usage.cost_usd.is_some_and(|usd| usd > 0.0),
        "{:?}",
        response.usage
    );
}

/// The stable blocks travel as one joined system message, because some
/// backends reject or drop all but the first. Both blocks must reach the
/// model: lowercase from the first, the word from the second.
#[test]
#[ignore = "paid: calls the Venice API"]
fn venice_every_stable_block_reaches_the_model() {
    let venice = connect("venice:claude-haiku-5-5");
    let mut request = request(
        vec![
            block("a.md", "Always answer in lowercase letters only."),
            block(
                "b.md",
                "The secret word is PLUM. When asked for the secret word, reply with it and nothing else.",
            ),
        ],
        "What is the secret word?",
    );
    request.tools.clear();
    let response = complete(&venice, &request);
    assert_eq!(response.turn.text().trim(), "plum");
}
