//! Anthropic's own Messages API, through rig-core's `anthropic` wire: the
//! one provider with explicit cache breakpoints, so the seams `to_rig`
//! marks (tools, stable, log; never live) become its `cache_control`
//! markers (A1). It reports cache writes as well as reads, but no cost.
//! Paid: its live tests are `#[ignore = "paid"]`, and run only on purpose.

use fussycat_core::provider::Request;
use rig_core::completion::CompletionRequest;
use rig_core::http_client::DynHttpClient;
use rig_core::providers::anthropic::completion::ANTHROPIC_VERSION_LATEST;
use rig_core::providers::anthropic::{ANTHROPIC, AnthropicConfig, Messages};

use super::adapter::{Adapter, Dialect};
use super::listing::{self, Listed};
use super::retry::Policy;

/// The most a request asks for unless `--max-output-tokens` says otherwise:
/// what fussycat has always sent, and rig's own default for a model its
/// catalog doesn't list yet.
const MAX_TOKENS: u64 = 64_000;

struct Anthropic {
    /// `max_tokens` for a request whose budget sets none.
    default_max_tokens: u64,
}

#[must_use]
pub fn connect(api_key: String, model: &str, retry: Policy) -> Adapter {
    with_http(api_key, model, retry, super::http())
}

fn with_http(api_key: String, model: &str, retry: Policy, http: DynHttpClient) -> Adapter {
    let config = AnthropicConfig::with_key(&ANTHROPIC, api_key);
    let dialect = Anthropic {
        default_max_tokens: default_max_tokens(config.completion(model).default_max_tokens),
    };
    let model: rig_core::Model<Messages> = config.connect(http).completion(model);
    Adapter::new(model, retry, dialect)
}

/// Anthropic requires `max_tokens`, and a model rejects one over its output
/// limit. `listed` is that limit from rig's model catalog (the
/// models.dev data rig-core ships), `None` for a model newer than rig. It's
/// capped at [`MAX_TOKENS`], so no model is asked for more than before.
fn default_max_tokens(listed: Option<u64>) -> u64 {
    listed.map_or(MAX_TOKENS, |limit| limit.min(MAX_TOKENS))
}

impl Dialect for Anthropic {
    fn request(&self, _request: &Request, mut rig: CompletionRequest) -> CompletionRequest {
        rig.max_tokens.get_or_insert(self.default_max_tokens);
        rig
    }
}

/// Anthropic's models, from `/v1/models`, which needs the key. rig's
/// listing drops the context window, so this reads it. Anthropic lists no
/// prices and no tool flag, so those are left out, not guessed.
pub async fn list(api_key: &str) -> Result<Vec<Listed>, String> {
    let url = format!("{}/v1/models?limit=1000", ANTHROPIC.base_url);
    let request = super::client()
        .get(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION_LATEST);
    listing::fetch(request, &url).await.map(listed)
}

#[derive(serde::Deserialize)]
struct Models {
    data: Vec<Model>,
}

#[derive(serde::Deserialize)]
struct Model {
    id: String,
    max_input_tokens: Option<u64>,
}

fn listed(models: Models) -> Vec<Listed> {
    listing::sorted(
        models
            .data
            .into_iter()
            .map(|model| Listed {
                model: format!("anthropic:{}", model.id),
                context: model.max_input_tokens,
                tools: None,
                price: None,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests;
