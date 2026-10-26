//! Venice through rig-core's `venice` dialect of `OpenAI` Chat Completions.
//! Paid: its live test is `#[ignore = "paid"]`, and runs only on purpose.

use std::future::Future;

use fussycat_core::provider::{Provider, ProviderError, Request, Response};
use fussycat_core::report::prefix_key;
use rig_core::completion::CompletionResponse;
use rig_core::http_client::DynHttpClient;
use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::{Chat, VENICE};
use rig_core::providers::venice::extension::{VeniceExt, VeniceOptions};

use super::listing::{self, Listed, Price};
use super::messages::{from_rig, to_rig};
use super::retry::{self, Policy};

pub struct Venice {
    model: rig_core::Model<Chat>,
    retry: Policy,
}

impl Venice {
    #[must_use]
    pub fn new(api_key: String, model: &str, retry: Policy) -> Self {
        Self::with_http(api_key, model, retry, super::http())
    }

    fn with_http(api_key: String, model: &str, retry: Policy, http: DynHttpClient) -> Self {
        let client = OpenAIConfig::with_key(&VENICE, api_key).connect(http);
        Self {
            model: client.chat(model),
            retry,
        }
    }
}

impl Provider for Venice {
    fn complete(
        &self,
        request: &Request,
    ) -> impl Future<Output = Result<Response, ProviderError>> + Send {
        let rig_request =
            to_rig(request).map(|rig_request| rig_request.provider_option(options(request)));
        async move {
            let rig_request = rig_request?;
            let (response, retries) =
                retry::call(self.retry, || self.model.call(rig_request.clone())).await?;
            let mut neutral = from_rig(&response, retries)?;
            neutral.usage.cost_usd = reported_cost(&response);
            Ok(neutral)
        }
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
