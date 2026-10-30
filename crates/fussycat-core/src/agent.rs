//! The Agent (what the author declares) and the Task (what to do).

use std::collections::BTreeSet;

use crate::provider::Reasoning;
use crate::quilt::{BlockDecl, Kind};
use crate::toolbox::Toolbox;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DeclarationError {
    #[error("block names must not be empty")]
    EmptyBlockName,
    #[error("two blocks are named `{0}`; block names must be unique")]
    DuplicateBlock(String),
    #[error("two tools are named `{0}`; tool names must be unique")]
    DuplicateTool(String),
    #[error("the task is empty: pipe a task in on stdin")]
    EmptyTask,
    #[error("max turns must be at least 1")]
    NoTurns,
    #[error("max cost must be a positive number of US dollars, got {0}")]
    BadMaxCost(f64),
    #[error("max tool output must be at least 1 byte")]
    NoToolOutput,
    #[error("max output tokens must be at least 1")]
    NoOutputTokens,
}

/// Limits that guarantee a run ends, and keep it from flooding the context.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budget {
    turns: u32,
    cost_usd: Option<f64>,
    tool_output: usize,
    output_tokens: Option<u64>,
    reasoning: Option<Reasoning>,
}

impl Budget {
    pub const DEFAULT_MAX_TURNS: u32 = 10;
    /// About 8k tokens: room for a real result, not for a whole log file.
    pub const DEFAULT_MAX_TOOL_OUTPUT: usize = 32 * 1024;

    pub fn new(max_turns: u32) -> Result<Self, DeclarationError> {
        if max_turns == 0 {
            return Err(DeclarationError::NoTurns);
        }
        Ok(Self {
            turns: max_turns,
            ..Self::default()
        })
    }

    /// Stop once the provider-reported cost reaches `usd`. Checked after
    /// each turn, so the turn that crosses it is the last one paid for.
    pub fn with_max_cost(self, usd: f64) -> Result<Self, DeclarationError> {
        if !(usd.is_finite() && usd > 0.0) {
            return Err(DeclarationError::BadMaxCost(usd));
        }
        Ok(Self {
            cost_usd: Some(usd),
            ..self
        })
    }

    /// Cut each tool result to `bytes`, visibly.
    pub fn with_max_tool_output(self, bytes: usize) -> Result<Self, DeclarationError> {
        if bytes == 0 {
            return Err(DeclarationError::NoToolOutput);
        }
        Ok(Self {
            tool_output: bytes,
            ..self
        })
    }

    /// Ask for replies of at most `tokens`. Unset, each provider adapter
    /// picks a default its models take. A reply cut at the limit fails the
    /// run rather than pass as an answer.
    pub fn with_max_output_tokens(self, tokens: u64) -> Result<Self, DeclarationError> {
        if tokens == 0 {
            return Err(DeclarationError::NoOutputTokens);
        }
        Ok(Self {
            output_tokens: Some(tokens),
            ..self
        })
    }

    /// Ask a thinking model to reason at `level`. Unset, the provider picks
    /// per model, and some pick their highest, so it's part of the budget:
    /// reasoning tokens are paid for.
    #[must_use]
    pub fn with_reasoning(self, level: Reasoning) -> Self {
        Self {
            reasoning: Some(level),
            ..self
        }
    }

    #[must_use]
    pub fn max_turns(self) -> u32 {
        self.turns
    }

    #[must_use]
    pub fn max_cost_usd(self) -> Option<f64> {
        self.cost_usd
    }

    #[must_use]
    pub fn max_tool_output(self) -> usize {
        self.tool_output
    }

    #[must_use]
    pub fn max_output_tokens(self) -> Option<u64> {
        self.output_tokens
    }

    #[must_use]
    pub fn reasoning(self) -> Option<Reasoning> {
        self.reasoning
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            turns: Self::DEFAULT_MAX_TURNS,
            cost_usd: None,
            tool_output: Self::DEFAULT_MAX_TOOL_OUTPUT,
            output_tokens: None,
            reasoning: None,
        }
    }
}

/// Everything the author declares. Holds no run state, so one Agent run
/// twice renders the same stable prefix.
#[derive(Debug)]
pub struct Agent {
    /// In layout order: by kind, then declaration order within a kind (D2).
    blocks: Vec<BlockDecl>,
    pub(crate) toolbox: Toolbox,
    pub(crate) budget: Budget,
}

impl Agent {
    /// Blocks may be declared in any kind order: they're grouped by kind,
    /// keeping declaration order within each (D2, not rejected as in L2).
    pub fn new(
        mut blocks: Vec<BlockDecl>,
        toolbox: Toolbox,
        budget: Budget,
    ) -> Result<Self, DeclarationError> {
        let mut seen = BTreeSet::new();
        for block in &blocks {
            if !seen.insert(block.name.as_str()) {
                return Err(DeclarationError::DuplicateBlock(block.name.to_string()));
            }
        }
        blocks.sort_by_key(|block| block.kind); // stable sort: keeps declaration order
        Ok(Self {
            blocks,
            toolbox,
            budget,
        })
    }

    /// The declared blocks of one kind, in layout order.
    pub fn blocks(&self, kind: Kind) -> impl Iterator<Item = &BlockDecl> {
        self.blocks.iter().filter(move |block| block.kind == kind)
    }

    #[must_use]
    pub fn budget(&self) -> Budget {
        self.budget
    }
}

/// The text on stdin. A run without a task is an error, not an empty chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task(String);

impl Task {
    pub fn new(text: impl Into<String>) -> Result<Self, DeclarationError> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(DeclarationError::EmptyTask);
        }
        Ok(Self(text))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quilt::{BlockName, Source, SourceError};

    struct Empty;
    impl Source for Empty {
        fn fetch(&self) -> Result<String, SourceError> {
            Ok(String::new())
        }
    }

    fn decl(name: &str) -> BlockDecl {
        decl_of(Kind::Stable, name)
    }

    fn decl_of(kind: Kind, name: &str) -> BlockDecl {
        BlockDecl {
            name: BlockName::new(name).unwrap(),
            kind,
            source: Box::new(Empty),
        }
    }

    fn layout(agent: &Agent) -> Vec<&str> {
        [Kind::Stable, Kind::Live]
            .into_iter()
            .flat_map(|kind| agent.blocks(kind).map(|b| b.name.as_str()))
            .collect()
    }

    #[test]
    fn arch_layout_groups_by_kind() {
        let mixed = || {
            vec![
                decl_of(Kind::Live, "date -u"),
                decl_of(Kind::Stable, "core.md"),
                decl_of(Kind::Live, "uptime"),
                decl_of(Kind::Stable, "brief.md"),
            ]
        };
        let agent = Agent::new(mixed(), Toolbox::default(), Budget::default()).unwrap();
        assert_eq!(layout(&agent), ["core.md", "brief.md", "date -u", "uptime"]);
        let again = Agent::new(mixed(), Toolbox::default(), Budget::default()).unwrap();
        assert_eq!(layout(&agent), layout(&again));
    }

    #[test]
    fn block_names_are_unique_across_kinds() {
        let result = Agent::new(
            vec![decl_of(Kind::Stable, "x"), decl_of(Kind::Live, "x")],
            Toolbox::default(),
            Budget::default(),
        );
        assert!(matches!(result, Err(DeclarationError::DuplicateBlock(_))));
    }

    #[test]
    fn duplicate_block_names_are_rejected() {
        let result = Agent::new(
            vec![decl("a.md"), decl("a.md")],
            Toolbox::default(),
            Budget::default(),
        );
        assert!(matches!(result, Err(DeclarationError::DuplicateBlock(name)) if name == "a.md"));
    }

    #[test]
    fn blank_tasks_are_rejected() {
        assert_eq!(Task::new(" \n"), Err(DeclarationError::EmptyTask));
    }

    #[test]
    fn a_budget_needs_at_least_one_turn() {
        assert_eq!(Budget::new(0), Err(DeclarationError::NoTurns));
        assert_eq!(Budget::new(3).unwrap().max_turns(), 3);
    }

    #[test]
    fn a_cost_limit_must_be_a_positive_amount() {
        let budget = Budget::default();
        assert_eq!(budget.max_cost_usd(), None, "no cost limit unless asked");
        assert_eq!(
            budget.with_max_cost(0.25).unwrap().max_cost_usd(),
            Some(0.25)
        );
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(budget.with_max_cost(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn reasoning_is_left_to_the_provider_unless_set() {
        use crate::provider::Reasoning;
        assert_eq!(Budget::default().reasoning(), None);
        let budget = Budget::default().with_reasoning(Reasoning::Low);
        assert_eq!(budget.reasoning(), Some(Reasoning::Low));
        for level in Reasoning::ALL {
            assert_eq!(level.as_str().parse(), Ok(level));
        }
        assert!("none".parse::<Reasoning>().is_err(), "one spelling: `off`");
    }

    #[test]
    fn output_tokens_are_left_to_the_provider_unless_set_and_never_zero() {
        let budget = Budget::default();
        assert_eq!(budget.max_output_tokens(), None);
        assert_eq!(
            budget
                .with_max_output_tokens(500)
                .unwrap()
                .max_output_tokens(),
            Some(500)
        );
        assert_eq!(
            budget.with_max_output_tokens(0),
            Err(DeclarationError::NoOutputTokens)
        );
    }

    #[test]
    fn tool_output_is_capped_by_default_and_never_at_zero() {
        let budget = Budget::default();
        assert_eq!(budget.max_tool_output(), Budget::DEFAULT_MAX_TOOL_OUTPUT);
        assert_eq!(
            budget.with_max_tool_output(10).unwrap().max_tool_output(),
            10
        );
        assert_eq!(
            budget.with_max_tool_output(0),
            Err(DeclarationError::NoToolOutput)
        );
    }
}
