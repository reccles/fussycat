//! The `Provider` port: one model call, provider-neutral on both sides.
//! Each provider adapter implements it; wire formats, cache
//! breakpoints and retries stay inside them (framework A2).

use std::future::Future;

use serde::Serialize;

use crate::log::{AssistantTurn, Entry};
use crate::quilt::Block;
use crate::report::Usage;
use crate::toolbox::ToolSpec;

/// What the quilt renders for one turn, in layout order.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// Granted tools, sorted by name.
    pub tools: Vec<ToolSpec>,
    pub stable: Vec<Block>,
    pub log: Vec<Entry>,
    /// This turn's live blocks, already rendered with their fetch times.
    /// The adapter puts them after the log.
    pub live: Vec<Block>,
    /// The longest reply to ask for, in tokens (`Budget`). `None` leaves it
    /// to the adapter's default for its provider.
    pub max_output_tokens: Option<u64>,
}

/// Why the model's reply ended, as the provider reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    /// The model ended the reply itself: an answer, or to call tools. Also
    /// what a provider that reports no reason gets.
    End,
    /// The output limit cut the reply off: the request's max output tokens,
    /// or the model's context window.
    OutputLimit,
}

/// What the model answered, and what the provider says it cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub turn: AssistantTurn,
    pub stop: Stop,
    pub usage: Usage,
    /// Failed attempts the adapter retried before this response. Reported,
    /// so a flaky provider is visible rather than just slow.
    pub retries: u32,
}

/// The provider failed after the adapter gave up (including its retries).
/// The message names the cause, in the provider's own words if it gave any.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProviderError(pub String);

pub trait Provider {
    fn complete(
        &self,
        request: &Request,
    ) -> impl Future<Output = Result<Response, ProviderError>> + Send;
}
