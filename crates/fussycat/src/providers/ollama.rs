//! Ollama through rig-core's native `/api/chat` wire: the only route that
//! takes model options and reports prompt-eval timings.

use std::future::Future;

use fussycat_core::provider::{Provider, ProviderError, Request, Response};
use rig_core::http_client::DynHttpClient;
use rig_core::providers::ollama::{Chat, OllamaConfig};

use super::listing::{self, Listed};
use super::messages::{from_rig, to_rig};
use super::retry::{self, Policy};

pub struct Ollama {
    model: rig_core::Model<Chat>,
    retry: Policy,
}

impl Ollama {
    #[must_use]
    pub fn new(base_url: &str, model: &str, retry: Policy) -> Self {
        Self::with_http(base_url, model, retry, super::http())
    }

    fn with_http(base_url: &str, model: &str, retry: Policy, http: DynHttpClient) -> Self {
        let client = OllamaConfig::new().with_base_url(base_url).connect(http);
        Self {
            model: client.native_completion(model),
            retry,
        }
    }
}

impl Provider for Ollama {
    fn complete(
        &self,
        request: &Request,
    ) -> impl Future<Output = Result<Response, ProviderError>> + Send {
        let rig_request = to_rig(request).map(|mut rig_request| {
            rig_request.additional_params = Some(never_truncate());
            rig_request
        });
        async move {
            let rig_request = rig_request?;
            let (response, retries) =
                retry::call(self.retry, || self.model.call(rig_request.clone())).await?;
            // Cost stays unknown: Ollama reports none, and its `-cloud`
            // models are billed, so 0 would be a guess.
            from_rig(&response, retries)
        }
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
fn never_truncate() -> serde_json::Value {
    serde_json::json!({"truncate": false})
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
