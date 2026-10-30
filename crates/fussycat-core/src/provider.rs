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
    /// How hard a thinking model should think (`Budget`). `None` leaves it
    /// to the provider, which picks per model.
    pub reasoning: Option<Reasoning>,
}

/// How much a thinking model reasons before it answers, lowest first. The
/// adapter maps it to its provider's own control, and refuses a level the
/// model doesn't take rather than send something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Reasoning {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Reasoning {
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

impl std::fmt::Display for Reasoning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("reasoning effort must be one of off, minimal, low, medium, high, xhigh or max, got `{0}`")]
pub struct BadReasoning(String);

impl std::str::FromStr for Reasoning {
    type Err = BadReasoning;

    fn from_str(level: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|known| known.as_str() == level)
            .ok_or_else(|| BadReasoning(level.to_owned()))
    }
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
#[error("{message}")]
pub struct ProviderError {
    pub message: String,
    /// Failed attempts the adapter retried before it gave up. Counted like
    /// `Response::retries`, so a flaky provider is visible even when the
    /// call fails in the end.
    pub retries: u32,
}

impl ProviderError {
    /// A failure with no retries before it.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retries: 0,
        }
    }

    /// The same failure, after `retries` failed attempts.
    #[must_use]
    pub fn after_retries(self, retries: u32) -> Self {
        Self { retries, ..self }
    }
}

pub trait Provider {
    fn complete(
        &self,
        request: &Request,
    ) -> impl Future<Output = Result<Response, ProviderError>> + Send;
}
