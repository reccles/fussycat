//! Blocks, sources and the quilt: the ordered context for one run.
//!
//! Kinds so far: stable, the log and live. Pinned blocks arrive with the
//! feature that needs them (INTENT principle 7).

use serde::Serialize;

use crate::agent::DeclarationError;
use crate::log::{Entry, Log};
use crate::provider::Request;
use crate::toolbox::Toolbox;

/// A block's name: unique within a quilt, used by reports and errors.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct BlockName(String);

impl BlockName {
    pub fn new(name: impl Into<String>) -> Result<Self, DeclarationError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(DeclarationError::EmptyBlockName);
        }
        Ok(Self(name))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BlockName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A named unit of context with its content.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Block {
    pub name: BlockName,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SourceError(pub String);

/// Where a block's content comes from (a file, a command). Implemented by
/// the adapters; the core only asks for content.
pub trait Source: Send + Sync {
    fn fetch(&self) -> Result<String, SourceError>;
}

/// Literal content: the framework's standard Text block, e.g. fussycat's core
/// prompt. Pure, so it lives in the core; the same text every time.
pub struct Text(pub String);

impl Source for Text {
    fn fetch(&self) -> Result<String, SourceError> {
        Ok(self.0.clone())
    }
}

/// A declared block's kind. Variants are in layout order (L1): stable
/// blocks, then the log (implicit, one per run), then live blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Fetched once, at run start (L4).
    Stable,
    /// Fetched before every turn; rendered after the log.
    Live,
}

/// A declared block: its name, kind and source. Content is fetched by the run.
pub struct BlockDecl {
    pub name: BlockName,
    pub kind: Kind,
    pub source: Box<dyn Source>,
}

impl std::fmt::Debug for BlockDecl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockDecl")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// Stable blocks (fixed for the run, framework L4) followed by the log.
/// Live blocks aren't kept: each turn's are passed to [`Quilt::render`].
#[derive(Debug, Clone, PartialEq)]
pub struct Quilt {
    stable: Vec<Block>,
    log: Log,
}

impl Quilt {
    #[must_use]
    pub fn new(stable: Vec<Block>, log: Log) -> Self {
        Self { stable, log }
    }

    pub fn append(&mut self, entry: Entry) {
        self.log.append(entry);
    }

    /// The provider-neutral request for this turn. Deterministic: the same
    /// quilt, toolbox and live blocks always render the same request (L3),
    /// and only `live` can vary between two renders of one log (P3). The
    /// reply's length is the budget's business, not the quilt's: the Run
    /// sets `max_output_tokens`.
    #[must_use]
    pub fn render(&self, toolbox: &Toolbox, live: Vec<Block>) -> Request {
        Request {
            tools: toolbox.specs(),
            stable: self.stable.clone(),
            log: self.log.entries().to_vec(),
            live,
            max_output_tokens: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolbox::fakes::FixedTool;

    fn block(name: &str, content: &str) -> Block {
        Block {
            name: BlockName::new(name).unwrap(),
            content: content.into(),
        }
    }

    #[test]
    fn a_text_source_gives_its_text_every_time() {
        let text = Text("be terse".into());
        assert_eq!(text.fetch(), Ok("be terse".into()));
        assert_eq!(text.fetch(), text.fetch());
    }

    #[test]
    fn blank_block_names_are_rejected() {
        assert_eq!(BlockName::new("  "), Err(DeclarationError::EmptyBlockName));
    }

    #[test]
    fn stable_blocks_render_in_declaration_order() {
        let quilt = Quilt::new(
            vec![block("b", "2"), block("a", "1")],
            Log::starting_with("t"),
        );
        let names: Vec<_> = quilt
            .render(&Toolbox::default(), vec![])
            .stable
            .into_iter()
            .map(|b| b.name.to_string())
            .collect();
        assert_eq!(names, ["b", "a"]);
    }

    #[test]
    fn arch_render_is_deterministic() {
        let quilt = Quilt::new(vec![block("core.md", "be terse")], Log::starting_with("t"));
        let forwards = Toolbox::grant(vec![
            Box::new(FixedTool::new("a", "")),
            Box::new(FixedTool::new("b", "")),
        ])
        .unwrap();
        let backwards = Toolbox::grant(vec![
            Box::new(FixedTool::new("b", "")),
            Box::new(FixedTool::new("a", "")),
        ])
        .unwrap();
        assert_eq!(
            quilt.render(&forwards, vec![]),
            quilt.render(&forwards, vec![])
        );
        assert_eq!(
            quilt.render(&forwards, vec![]),
            quilt.render(&backwards, vec![])
        );
    }
}
