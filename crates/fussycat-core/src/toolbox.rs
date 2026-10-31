//! Grants, tools and the toolbox. Deny by default: an empty toolbox is the
//! starting point, and only explicit grants put tools in it.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use serde::Serialize;

use crate::agent::DeclarationError;

/// What the model is told about a tool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments.
    pub parameters: serde_json::Value,
}

/// The model asking for a tool to run.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// What came back. Both `Ok` and `Err` go to the model: a failing tool is a
/// result, never a run failure.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub output: Result<String, String>,
}

/// What [`Tool::invoke`] returns: the tool's output once it's done. Boxed,
/// so a [`Toolbox`] can hold tools of any kind as `Box<dyn Tool>`.
pub type Invocation<'a> = Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;

/// A granted capability: a spec plus a way to invoke it. Shell tools and
/// batteries implement this in the adapters, so the loop never branches on
/// tool type. (The `ToolRunner` port in the domain spec.)
pub trait Tool: Send + Sync {
    fn spec(&self) -> &ToolSpec;

    /// Run the tool. Dropping the future before it's done stops the tool:
    /// whatever it started is ended, not left running.
    fn invoke<'a>(&'a self, arguments: &'a serde_json::Value) -> Invocation<'a>;
}

/// The granted tools, keyed (and so sorted) by name: the order the model
/// sees never depends on the order grants were given in (eval B2).
#[derive(Default)]
pub struct Toolbox {
    tools: BTreeMap<String, Box<dyn Tool>>,
}

impl Toolbox {
    /// Grant `tools`. Two tools with one name is a declaration error.
    pub fn grant(tools: Vec<Box<dyn Tool>>) -> Result<Self, DeclarationError> {
        let mut granted = BTreeMap::new();
        for tool in tools {
            let name = tool.spec().name.clone();
            if granted.insert(name.clone(), tool).is_some() {
                return Err(DeclarationError::DuplicateTool(name));
            }
        }
        Ok(Self { tools: granted })
    }

    /// Whether nothing is granted: the model can only answer.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools
            .values()
            .map(|tool| tool.spec().clone())
            .collect()
    }

    /// Run `call` if its tool is granted. An ungranted name is an `Err`
    /// result, never an execution (eval B5).
    pub async fn call(&self, call: &ToolCall) -> ToolResult {
        let output = match self.tools.get(&call.name) {
            Some(tool) => tool.invoke(&call.arguments).await,
            None => Err(format!("no tool named `{}` is granted", call.name)),
        };
        ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            output,
        }
    }
}

impl ToolResult {
    /// This result with its output (or error) cut to `limit` bytes.
    #[must_use]
    pub fn capped(self, limit: usize) -> Self {
        Self {
            output: match self.output {
                Ok(text) => Ok(cap(text, limit, "output")),
                Err(text) => Err(cap(text, limit, "error")),
            },
            ..self
        }
    }
}

/// `text` cut to at most `limit` bytes (on a character boundary), with a
/// note saying so: the model must know it's seeing part of something, or it
/// will reason as if it saw all of it. `what` names the text in the note.
#[must_use]
pub fn cap(text: String, limit: usize, what: &str) -> String {
    if text.len() <= limit {
        return text;
    }
    let total = text.len();
    cut(text, limit, total, what)
}

/// [`cap`] for `text` that's the start of something `total` bytes long: an
/// adapter that stopped keeping a command's output past the cap (a bounded
/// read) knows how much there was, and the text doesn't. The result, note
/// included, fits in `limit` (unless the note alone doesn't), so the core's
/// own cap leaves it as it is.
#[must_use]
pub fn cap_within(text: String, total: usize, limit: usize, what: &str) -> String {
    // The note for `limit` is at least as long as the one for any shorter
    // cut, plus the newline before it.
    let room = limit.saturating_sub(note(what, limit, total).len() + 1);
    cut(text, room, total, what)
}

/// `text` cut to at most `limit` bytes on a character boundary, then the
/// note, on its own line, saying it was `total` bytes.
fn cut(mut text: String, limit: usize, total: usize, what: &str) -> String {
    let mut end = limit.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text + &note(what, end, total)
}

fn note(what: &str, shown: usize, total: usize) -> String {
    format!("[{what} cut: showing the first {shown} of {total} bytes]")
}

impl std::fmt::Debug for Toolbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.tools.keys()).finish()
    }
}

#[cfg(test)]
mod cap_tests {
    use super::*;

    #[test]
    fn short_text_is_untouched() {
        assert_eq!(cap("hello".into(), 5, "output"), "hello");
    }

    #[test]
    fn long_text_is_cut_with_a_note() {
        assert_eq!(
            cap("abcdefgh".into(), 3, "output"),
            "abc\n[output cut: showing the first 3 of 8 bytes]"
        );
        assert_eq!(
            cap("ab\ncdefgh".into(), 3, "help"),
            "ab\n[help cut: showing the first 3 of 9 bytes]",
            "no blank line when the cut lands after a newline"
        );
    }

    #[test]
    fn cuts_on_a_character_boundary() {
        // "é" is two bytes: cutting at 2 would split it.
        assert_eq!(
            cap("aé!".into(), 2, "output"),
            "a\n[output cut: showing the first 1 of 4 bytes]"
        );
    }

    /// A bounded read kept the first part of 1000 bytes: the note says
    /// 1000, and the whole result fits the cap, so capping it again is a
    /// no-op and the model sees one note, not two.
    #[test]
    fn a_cut_within_the_cap_tells_the_whole_size_and_is_cut_once() {
        let limit = 60;
        let within = cap_within("x".repeat(limit), 1000, limit, "output");
        assert_eq!(
            within,
            format!(
                "{}\n[output cut: showing the first 11 of 1000 bytes]",
                "x".repeat(11)
            )
        );
        assert_eq!(within.len(), limit);
        assert_eq!(cap(within.clone(), limit, "output"), within);
        assert_eq!(
            cap_within("short".into(), 1000, limit, "help"),
            "short\n[help cut: showing the first 5 of 1000 bytes]",
            "text already shorter than the room is kept whole, and still noted"
        );
    }

    #[test]
    fn errors_are_capped_too() {
        let result = ToolResult {
            call_id: "c1".into(),
            name: "x".into(),
            output: Err("boom boom".into()),
        };
        assert_eq!(
            result.capped(4).output,
            Err("boom\n[error cut: showing the first 4 of 9 bytes]".into())
        );
    }
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::{Invocation, Tool, ToolCall, ToolSpec};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A call to `name` with id `c1` and no arguments.
    pub fn call(name: &str) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments: serde_json::json!({}),
        }
    }

    /// A tool that answers with a fixed string and counts its invocations.
    pub struct FixedTool {
        spec: ToolSpec,
        answer: String,
        pub invocations: AtomicUsize,
    }

    impl FixedTool {
        pub fn new(name: &str, answer: &str) -> Self {
            Self {
                spec: ToolSpec {
                    name: name.to_owned(),
                    description: format!("The {name} tool."),
                    parameters: serde_json::json!({"type": "object", "properties": {}}),
                },
                answer: answer.to_owned(),
                invocations: AtomicUsize::new(0),
            }
        }
    }

    impl Tool for FixedTool {
        fn spec(&self) -> &ToolSpec {
            &self.spec
        }

        fn invoke<'a>(&'a self, _arguments: &'a serde_json::Value) -> Invocation<'a> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(Ok(self.answer.clone())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fakes::{FixedTool, call};
    use super::*;

    #[test]
    fn specs_are_sorted_by_name_whatever_the_grant_order() {
        let toolbox = Toolbox::grant(vec![
            Box::new(FixedTool::new("zebra", "")),
            Box::new(FixedTool::new("alpha", "")),
        ])
        .unwrap();
        let names: Vec<_> = toolbox.specs().into_iter().map(|spec| spec.name).collect();
        assert_eq!(names, ["alpha", "zebra"]);
    }

    #[test]
    fn duplicate_tool_names_are_rejected() {
        let result = Toolbox::grant(vec![
            Box::new(FixedTool::new("now", "")),
            Box::new(FixedTool::new("now", "")),
        ]);
        assert!(matches!(result, Err(DeclarationError::DuplicateTool(name)) if name == "now"));
    }

    #[test]
    fn a_granted_tool_runs() {
        let toolbox = Toolbox::grant(vec![Box::new(FixedTool::new("now", "noon"))]).unwrap();
        let result = crate::block_on(toolbox.call(&call("now")));
        assert_eq!(result.output, Ok("noon".into()));
        assert_eq!(result.call_id, "c1");
    }

    #[test]
    fn arch_deny_by_default() {
        let empty = Toolbox::default();
        assert_eq!(empty.specs(), Vec::<ToolSpec>::new());
        let result = crate::block_on(empty.call(&call("rm")));
        assert_eq!(result.output, Err("no tool named `rm` is granted".into()));
    }
}
