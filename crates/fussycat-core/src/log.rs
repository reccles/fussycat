//! The log: the append-only history of one run.

use crate::toolbox::{ToolCall, ToolResult};

/// Data an adapter needs to replay an entry byte-for-byte (for example a
/// provider's signed thinking). Opaque to the core: only the adapter that
/// produced it reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderData(pub serde_json::Value);

/// One part of an assistant turn, in the order the model produced it.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Thinking(String),
    Text(String),
    ToolCall(ToolCall),
}

/// What the model said in one turn.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistantTurn {
    pub parts: Vec<Part>,
    pub provider_data: Option<ProviderData>,
}

impl AssistantTurn {
    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.parts.iter().filter_map(|part| match part {
            Part::ToolCall(call) => Some(call),
            _ => None,
        })
    }

    /// The text parts, concatenated: the answer when there are no tool calls.
    #[must_use]
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|part| match part {
                Part::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// One log entry. Entries are provider-neutral.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// The task (stdin). Always the first entry.
    Task(String),
    Assistant(AssistantTurn),
    /// One result per tool call of the preceding assistant turn.
    ToolResults(Vec<ToolResult>),
}

/// Append-only: there is no way to change or remove an entry, which is what
/// keeps the prefix cacheable (framework `append`).
#[derive(Debug, Clone, PartialEq)]
pub struct Log {
    entries: Vec<Entry>,
}

impl Log {
    #[must_use]
    pub fn starting_with(task: &str) -> Self {
        Self {
            entries: vec![Entry::Task(task.to_owned())],
        }
    }

    pub fn append(&mut self, entry: Entry) {
        self.entries.push(entry);
    }

    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolbox::fakes::call;

    #[test]
    fn text_joins_text_parts_and_skips_the_rest() {
        let turn = AssistantTurn {
            parts: vec![
                Part::Thinking("hmm".into()),
                Part::Text("It is ".into()),
                Part::ToolCall(call("now")),
                Part::Text("noon.".into()),
            ],
            provider_data: None,
        };
        assert_eq!(turn.text(), "It is noon.");
        assert_eq!(turn.tool_calls().count(), 1);
    }

    #[test]
    fn log_starts_with_the_task_and_only_grows() {
        let mut log = Log::starting_with("What time is it?");
        log.append(Entry::ToolResults(vec![]));
        assert_eq!(log.entries()[0], Entry::Task("What time is it?".into()));
        assert_eq!(log.entries().len(), 2);
    }
}
