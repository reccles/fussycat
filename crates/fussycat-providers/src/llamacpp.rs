//! llama.cpp's `llama-server`, through rig-core's `llamacpp` dialect of
//! `OpenAI` Chat Completions: a model on your own hardware, like Ollama,
//! served with the model's own chat template and tool-call parsing.
//! A server started with `--api-key` takes the key as a bearer token; one
//! without takes none, and rejects one sent anyway.

use rig_core::http_client::DynHttpClient;
use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::{Chat, LLAMACPP};

use super::adapter::{Adapter, Dialect};
use super::listing::{self, Listed};
use super::retry::Policy;

/// Nothing to add on the way out or read on the way back: rig's dialect
/// covers the server's quirks. Cost stays unknown: it reports none.
struct LlamaCpp;

impl Dialect for LlamaCpp {}

/// The server at `base_url` (`http://localhost:8080/v1`), with `api_key`
/// if it was started with one. `http` sets the calls' timeouts.
#[must_use]
pub fn connect(
    base_url: &str,
    api_key: Option<String>,
    model: &str,
    retry: Policy,
    http: DynHttpClient,
) -> Adapter {
    let client = OpenAIConfig::with_key(&LLAMACPP, api_key.unwrap_or_default())
        .with_base_url(base_url)
        .connect(http);
    let model: rig_core::Model<Chat> = client.chat(model);
    Adapter::new(model, retry, LlamaCpp)
}

/// The server's models, from `/models`. A router (`llama-server` with
/// presets) lists every model it can load, with its context length; one
/// serving a single model reports its training context under `meta`.
/// Tool support and prices aren't listed, so they're left out.
pub async fn list(base_url: &str) -> Result<Vec<Listed>, String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
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
    meta: Option<Meta>,
}

#[derive(serde::Deserialize)]
struct Meta {
    n_ctx_train: Option<u64>,
}

fn listed(models: Models) -> Vec<Listed> {
    listing::sorted(
        models
            .data
            .into_iter()
            .map(|model| Listed {
                model: format!("llamacpp:{}", model.id),
                context: model
                    .context_length
                    .or(model.meta.and_then(|meta| meta.n_ctx_train)),
                tools: None,
                price: None,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests;
