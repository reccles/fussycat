//! A failed rig call, said plainly: the cause in the provider's own words,
//! not the wrapper's. `HTTP 404: model 'x' not found`, not a JSON envelope;
//! `Connection refused`, not "error sending request".

use rig_core::ProviderError as RigError;
use serde_json::Value;

/// The longest provider message quoted, in characters: an HTML error page
/// shouldn't flood stderr.
const QUOTE_LIMIT: usize = 300;

/// Bounded on every path: a decode error can carry the reply's body too.
pub fn describe(error: &RigError) -> String {
    let report = error.report();
    if let Some(status) = report.http_status {
        return replied(
            status,
            report.provider_response_body().unwrap_or(&report.message),
        );
    }
    if let RigError::Http(transport) = error {
        // rig's wrapper only adds "Http client error: " to what the
        // transport said, so start from the transport's own error.
        let transport: &(dyn std::error::Error + 'static) = transport.as_ref();
        return with_root_cause(transport.source().unwrap_or(transport));
    }
    quote(&report.message)
}

/// A provider's error reply: its status, and what it said.
pub fn replied(status: u16, body: &str) -> String {
    let said = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|envelope| message_in(&envelope))
        .unwrap_or_else(|| body.to_owned());
    format!("HTTP {status}: {}", quote(said.trim()))
}

/// Where providers put the human part of an error envelope: Ollama and
/// Venice say `{"error": "..."}`, `OpenAI` `{"error": {"message": "..."}}`.
fn message_in(envelope: &Value) -> Option<String> {
    [
        &envelope["error"],
        &envelope["error"]["message"],
        &envelope["message"],
    ]
    .into_iter()
    .find_map(Value::as_str)
    .map(str::to_owned)
}

/// The outermost error, then its innermost cause: the request that failed
/// and why. The links between ("client error (Connect)") add nothing.
/// Bounded, like a reply.
pub fn with_root_cause(error: &(dyn std::error::Error + 'static)) -> String {
    let outer = error.to_string();
    let Some(mut root) = error.source() else {
        return quote(&outer);
    };
    while let Some(source) = root.source() {
        root = source;
    }
    let cause = root.to_string();
    let said = if outer.contains(&cause) {
        outer
    } else {
        format!("{outer}: {cause}")
    };
    quote(&said)
}

/// `text`, cut at [`QUOTE_LIMIT`] characters, visibly.
pub(super) fn quote(text: &str) -> String {
    match text.char_indices().nth(QUOTE_LIMIT) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests;
