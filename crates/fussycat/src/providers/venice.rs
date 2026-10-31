//! Venice through rig-core's `venice` dialect of `OpenAI` Chat Completions.
//! Paid: its live test is `#[ignore = "paid"]`, and runs only on purpose.

use fussycat_core::provider::{Request, Response};
use fussycat_core::report::prefix_key;
use rig_core::completion::{CompletionRequest, CompletionResponse};
use rig_core::http_client::DynHttpClient;
use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::{Chat, VENICE};
use rig_core::providers::venice::extension::{VeniceExt, VeniceOptions};

use super::adapter::{Adapter, Dialect};
use super::listing::{self, Listed, Price};
use super::retry::Policy;

struct Venice;

#[must_use]
pub fn connect(api_key: String, model: &str, retry: Policy) -> Adapter {
    with_http(api_key, model, retry, super::http())
}

fn with_http(api_key: String, model: &str, retry: Policy, http: DynHttpClient) -> Adapter {
    let client = OpenAIConfig::with_key(&VENICE, api_key).connect(http);
    let model: rig_core::Model<Chat> = client.chat(model);
    Adapter::new(model, retry, Venice)
}

impl Dialect for Venice {
    fn request(&self, request: &Request, rig: CompletionRequest) -> CompletionRequest {
        rig.provider_option(options(request))
    }

    fn response(&self, rig: &CompletionResponse, neutral: &mut Response) {
        neutral.usage.cost_usd = reported_cost(rig);
    }
}

/// - `include_venice_system_prompt: false`: by default Venice prepends its
///   own system prompt, which is context the author never declared (R3).
/// - `prompt_cache_key`: routes every turn, and every run of the same
///   agent, to the same cache. Derived from the prefix, so it's never
///   hidden state.
fn options(request: &Request) -> VeniceOptions {
    VeniceOptions::new()
        .include_venice_system_prompt(false)
        .prompt_cache_key(format!("{}-{}", crate::NAME, prefix_key(request)))
}

/// What Venice says the call cost, in USD. Unknown if it didn't say.
fn reported_cost(response: &CompletionResponse) -> Option<f64> {
    response.extras::<VeniceExt>()?.ok()?.cost?.usd
}

/// Venice's text models, from `/models`: it lists tool support and prices
/// in `model_spec`, which rig's listing drops. Needs no key.
pub async fn list() -> Result<Vec<Listed>, String> {
    let url = format!("{}/models?type=text", VENICE.base_url);
    listing::get(&url).await.map(listed)
}

#[derive(serde::Deserialize)]
struct Models {
    data: Vec<Model>,
}

#[derive(serde::Deserialize)]
struct Model {
    id: String,
    context_length: Option<u64>,
    #[serde(default, rename = "model_spec")]
    spec: Spec,
}

#[derive(Default, serde::Deserialize)]
struct Spec {
    #[serde(default)]
    capabilities: Capabilities,
    pricing: Option<Pricing>,
}

#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    supports_function_calling: Option<bool>,
}

#[derive(serde::Deserialize)]
struct Pricing {
    input: Usd,
    output: Usd,
    cache_input: Option<Usd>,
}

#[derive(serde::Deserialize)]
struct Usd {
    usd: f64,
}

fn listed(models: Models) -> Vec<Listed> {
    listing::sorted(
        models
            .data
            .into_iter()
            .map(|model| Listed {
                model: format!("venice:{}", model.id),
                context: model.context_length,
                tools: model.spec.capabilities.supports_function_calling,
                price: model.spec.pricing.map(|pricing| Price {
                    input: pricing.input.usd,
                    output: pricing.output.usd,
                    cache_read: pricing.cache_input.map(|usd| usd.usd),
                }),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests;
