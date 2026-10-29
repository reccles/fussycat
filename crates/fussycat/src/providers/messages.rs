//! The neutral request as rig messages, and rig's response back: shared by
//! every provider, which only adds its own request options and reply extras.

use std::collections::HashMap;
use std::sync::Arc;

use fussycat_core::log::{AssistantTurn, Entry, Part, ProviderData};
use fussycat_core::provider::{ProviderError, Request, Response, Stop};
use fussycat_core::quilt::Block;
use fussycat_core::report::Usage;
use fussycat_core::toolbox::{ToolCall, ToolResult};
use rig_core::completion::{CachePoint, CompletionRequest, CompletionResponse, ToolDefinition};
use rig_core::message::{AssistantContent, CallId, Message, StopReason, ToolName};

/// Between two stable blocks in the system message: a blank line, so each
/// block still reads as its own section.
const STABLE_SEPARATOR: &str = "\n\n";

/// Layout on the wire: the stable blocks joined, in order, into one system
/// message, then the log, then the live blocks as one final user message (so
/// they never break the prefix), with the tools alongside.
///
/// One system message, not one per block: some chat
/// templates keep only the first (Ollama's qwen3-coder renderer), so later
/// blocks would silently vanish. The prefix stays the same bytes, so it
/// caches as before.
///
/// The seams become cache points (A1): after the tools, after the stable
/// blocks, and after the log, so the live blocks are never cached. Only a
/// provider with explicit breakpoints (Anthropic) reads them; the others
/// cache whatever prefix repeats.
pub fn to_rig(request: &Request) -> Result<CompletionRequest, ProviderError> {
    let mut history: Vec<Message> = Vec::new();
    let mut cache_points = Vec::new();
    if !request.tools.is_empty() {
        cache_points.push(CachePoint::Tools);
    }
    if !request.stable.is_empty() {
        history.push(Message::system(joined(&request.stable, STABLE_SEPARATOR)));
        cache_points.push(CachePoint::System);
    }
    let system_messages = history.len();
    // A result must carry the exact id rig gave its call, which a string
    // can't reconstruct, so look it up in the assistant turn being answered.
    let mut call_ids: HashMap<String, CallId> = HashMap::new();
    for entry in &request.log {
        match entry {
            Entry::Task(task) => history.push(Message::user(task.clone())),
            Entry::Assistant(turn) => {
                let message = replay(turn)?;
                if let Message::Assistant(assistant) = &message {
                    for call in assistant.tool_calls() {
                        call_ids.insert(call.id.to_string(), call.id.clone());
                    }
                }
                history.push(message);
            }
            Entry::ToolResults(results) => {
                for result in results {
                    history.push(tool_result(result, &call_ids)?);
                }
            }
        }
    }
    // Counted as rig counts them: user and assistant messages, not system.
    let log_messages = history.len() - system_messages;
    if log_messages > 0 {
        cache_points.push(CachePoint::Messages(log_messages));
    }
    if !request.live.is_empty() {
        history.push(Message::user(joined(&request.live, "\n")));
    }
    let mut rig_request = CompletionRequest::from(history);
    rig_request.cache_points = cache_points;
    rig_request.max_tokens = request.max_output_tokens;
    rig_request.tools = request
        .tools
        .iter()
        .map(|spec| {
            let name = tool_name(&spec.name)?;
            Ok(Arc::new(ToolDefinition::new(
                name,
                spec.description.clone(),
                spec.parameters.clone(),
            )))
        })
        .collect::<Result<_, ProviderError>>()?;
    Ok(rig_request)
}

fn joined(blocks: &[Block], separator: &str) -> String {
    let contents: Vec<&str> = blocks.iter().map(|block| block.content.as_str()).collect();
    contents.join(separator)
}

/// Replays an assistant turn byte for byte from the rig message kept when
/// it was received (its `ProviderData`).
fn replay(turn: &AssistantTurn) -> Result<Message, ProviderError> {
    let data = turn
        .provider_data
        .as_ref()
        .ok_or_else(|| ProviderError::new("assistant turn has no provider data to replay"))?;
    serde_json::from_value(data.0.clone())
        .map_err(|error| ProviderError::new(format!("cannot replay assistant turn: {error}")))
}

fn tool_result(
    result: &ToolResult,
    call_ids: &HashMap<String, CallId>,
) -> Result<Message, ProviderError> {
    let id = call_ids.get(&result.call_id).cloned().ok_or_else(|| {
        ProviderError::new(format!("tool result for unknown call `{}`", result.call_id))
    })?;
    let content = match &result.output {
        Ok(output) => output.clone(),
        Err(error) => format!("error: {error}"),
    };
    Ok(Message::tool_result(id, tool_name(&result.name)?, content))
}

fn tool_name(name: &str) -> Result<ToolName, ProviderError> {
    ToolName::new(name).map_err(|error| ProviderError::new(format!("tool `{name}`: {error}")))
}

/// The neutral response, after `retries` failed attempts. `cost_usd` is
/// left unknown: only the provider knows whether it reports one.
pub fn from_rig(response: &CompletionResponse, retries: u32) -> Result<Response, ProviderError> {
    let stop = stop(response.stop()).map_err(|error| error.after_retries(retries))?;
    let parts = response
        .choice
        .iter()
        .filter_map(|content| match content {
            AssistantContent::Text(text) => Some(Part::Text(text.text.clone())),
            AssistantContent::Reasoning(reasoning) => Some(Part::Thinking(reasoning.text.clone())),
            AssistantContent::ToolCall(call) => Some(Part::ToolCall(ToolCall {
                id: call.id.to_string(),
                name: call.function.name.to_string(),
                arguments: serde_json::Value::Object(call.function.arguments.clone()),
            })),
            _ => None,
        })
        .collect();
    let provider_data = response
        .message()
        .map(|message| serde_json::to_value(message).map(ProviderData))
        .transpose()
        .map_err(|error| {
            ProviderError::new(format!("cannot keep assistant turn: {error}"))
                .after_retries(retries)
        })?;
    Ok(Response {
        turn: AssistantTurn {
            parts,
            provider_data,
        },
        stop,
        usage: usage(&response.usage),
        retries,
    })
}

/// rig's verdict on how the reply ended, which reads every provider's own
/// spelling (`max_tokens`, `length`, `done_reason`...). A turn the provider
/// failed (a refusal, filtered content, a reason it doesn't document) is a
/// provider error: rig never replays it, so the run can't go on from it.
fn stop(reason: StopReason) -> Result<Stop, ProviderError> {
    match reason {
        StopReason::Length => Ok(Stop::OutputLimit),
        StopReason::Error(why) | StopReason::Aborted(why) => Err(ProviderError::new(format!(
            "the model's turn failed: {why}"
        ))),
        // `Stop`, `ToolUse`, and whatever rig adds that isn't a failure.
        other if !other.is_failure() => Ok(Stop::End),
        other => Err(ProviderError::new(format!(
            "the model's turn failed: {other:?}"
        ))),
    }
}

/// rig already normalizes usage the way the core counts it: `input` is
/// every prompt token, and the cache counts are parts of it.
fn usage(usage: &rig_core::completion::Usage) -> Usage {
    Usage {
        input: usage.input_tokens,
        output: usage.output_tokens,
        cache_read: usage.cached_input_tokens,
        cache_write: usage.cache_creation_input_tokens,
        cost_usd: None,
    }
}

#[cfg(test)]
mod tests;
