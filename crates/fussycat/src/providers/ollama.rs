//! Ollama through rig-core's native `/api/chat` wire: the only route that
//! takes model options and reports prompt-eval timings.

use fussycat_core::provider::Request;
use rig_core::completion::CompletionRequest;
use rig_core::http_client::DynHttpClient;
use rig_core::providers::ollama::{Chat, OllamaConfig};
use serde_json::{Map, Value};

use super::adapter::{Adapter, Dialect};
use super::listing::{self, Listed};
use super::retry::Policy;

/// Reads nothing from the reply: cost stays unknown, because Ollama reports
/// none and its `-cloud` models are billed, so 0 would be a guess.
struct Ollama;

#[must_use]
pub fn connect(base_url: &str, model: &str, retry: Policy) -> Adapter {
    with_http(base_url, model, retry, super::http())
}

fn with_http(base_url: &str, model: &str, retry: Policy, http: DynHttpClient) -> Adapter {
    let client = OllamaConfig::new().with_base_url(base_url).connect(http);
    let model: rig_core::Model<Chat> = client.native_completion(model);
    Adapter::new(model, retry, Ollama)
}

impl Dialect for Ollama {
    fn request(&self, _request: &Request, mut rig: CompletionRequest) -> CompletionRequest {
        rig.additional_params = Some(never_truncate(rig.additional_params.take()));
        rig
    }
}

/// Ollama silently drops tokens from a prompt longer than the context
/// window; `truncate: false` makes it a 400 instead.
///
/// Deliberately not sent: `shift` and `options.num_ctx`. Both are runner
/// settings, so a value differing from other clients' reloads the model and
/// throws away every cached prefix (checked on Ollama 0.40.10). The only
/// option that can go is `num_predict`, from `--max-output-tokens`: a limit
/// on one reply, not a runner setting (not yet checked against a reload).
///
/// Added to `params`, so whatever else is there still goes. Anything but
/// an object can't be merged into the body, so it's replaced.
fn never_truncate(params: Option<Value>) -> Value {
    let mut params = match params {
        Some(Value::Object(params)) => params,
        _ => Map::new(),
    };
    params.insert("truncate".into(), Value::Bool(false));
    Value::Object(params)
}

/// The server's models, from `/api/tags`: it reports each model's context
/// length and capabilities, which rig's listing drops.
pub async fn list(base_url: &str) -> Result<Vec<Listed>, String> {
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    listing::get(&url).await.map(listed)
}

#[derive(serde::Deserialize)]
struct Tags {
    models: Vec<Tag>,
}

#[derive(serde::Deserialize)]
struct Tag {
    name: String,
    #[serde(default)]
    details: Details,
    /// Absent before Ollama 0.6 or so: then tool support is unknown.
    capabilities: Option<Vec<String>>,
}

#[derive(Default, serde::Deserialize)]
struct Details {
    context_length: Option<u64>,
}

fn listed(tags: Tags) -> Vec<Listed> {
    listing::sorted(
        tags.models
            .into_iter()
            .map(|tag| Listed {
                model: format!("ollama:{}", tag.name),
                context: tag.details.context_length,
                tools: tag
                    .capabilities
                    .map(|capabilities| capabilities.iter().any(|c| c == "tools")),
                price: None,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests;
