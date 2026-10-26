//! A run: one execution of an Agent against a Task, ending in an Outcome.
//!
//! ```text
//! Preparing --(stable sources read)--> Turning --(a whole answer)----> Answered
//!                                         |--(budget reached)------> Exhausted
//!     any failure, or a reply that isn't -+------------------------> Failed
//!     a whole answer (empty, or cut off)
//! ```

use crate::agent::{Agent, Task};
use crate::clock::Clock;
use crate::live::{self, LiveBlock, LiveReport};
use crate::log::{Entry, Log};
use crate::provider::{Provider, ProviderError, Request, Response, Stop};
use crate::quilt::{Block, BlockName, Kind, Quilt, SourceError};
use crate::report::{Reporter, RunSummary, TurnReport, Usage, prefix_fingerprints};
use crate::toolbox::ToolResult;

/// Which budget limit ended a run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Limit {
    MaxTurns(u32),
    /// The cap, and what the provider says was spent when it was reached.
    MaxCost {
        cap_usd: f64,
        spent_usd: f64,
    },
}

impl std::fmt::Display for Limit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MaxTurns(turns) => write!(f, "max turns ({turns}) reached"),
            Self::MaxCost { cap_usd, spent_usd } => {
                write!(f, "max cost (${cap_usd}) reached: ${spent_usd:.6} spent")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    #[error("stable block `{block}`: {error}")]
    Source {
        block: BlockName,
        error: SourceError,
    },
    #[error("provider: {0}")]
    Provider(#[from] ProviderError),
    /// A cost limit can't be kept blind, so the run stops rather than
    /// spend more than it can count.
    #[error("a cost limit is set, but the provider didn't report what turn {turn} cost")]
    CostUnreported { turn: u32 },
    /// A reply with no tool calls and no text (or only whitespace).
    #[error("the model gave no answer: its reply on turn {turn} was empty")]
    NoAnswer { turn: u32 },
    /// Part of an answer isn't the answer, and stdout holds only the answer.
    #[error(
        "the answer was cut off at the output limit (max output tokens) on turn {turn}, \
         so it isn't printed"
    )]
    AnswerCutOff { turn: u32 },
    /// The cut may fall inside a call's arguments, and running half a plan
    /// is worse than running none of it.
    #[error(
        "turn {turn} was cut off at the output limit (max output tokens) while calling \
         tools, so none of them ran"
    )]
    ToolCallsCutOff { turn: u32 },
}

/// How a run ended. The adapter decides what each means for stdout and
/// exit codes; the core never prints.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Answered(String),
    Exhausted(Limit),
    Failed(Failure),
}

/// Run `agent` on `task` until the model answers, a budget limit is
/// reached, or something fails. Every turn is reported, and so is the run.
pub async fn execute(
    agent: &Agent,
    task: &Task,
    provider: &impl Provider,
    clock: &impl Clock,
    reporter: &mut impl Reporter,
) -> Outcome {
    let started = clock.now();
    let mut run = Run {
        agent,
        provider,
        clock,
        reporter,
        turns: 0,
        usage: None,
        retries: 0,
    };
    let outcome = run.turn_until_done(task).await;
    let summary = RunSummary {
        turns: run.turns,
        elapsed_ms: clock.now().millis_since(started),
        usage: run.usage.unwrap_or_default(),
        retries: run.retries,
    };
    run.reporter.finish(&summary, &outcome);
    outcome
}

/// The aggregate root: one execution, its ports and its running totals.
struct Run<'a, P, C, R> {
    agent: &'a Agent,
    provider: &'a P,
    clock: &'a C,
    reporter: &'a mut R,
    turns: u32,
    /// `None` until the first turn: no turns means nothing was reported.
    usage: Option<Usage>,
    retries: u32,
}

impl<P: Provider, C: Clock, R: Reporter> Run<'_, P, C, R> {
    async fn turn_until_done(&mut self, task: &Task) -> Outcome {
        let stable = match self.read_stable() {
            Ok(stable) => stable,
            Err(failure) => return Outcome::Failed(failure),
        };
        let mut live: Vec<LiveBlock> = self.agent.blocks(Kind::Live).map(LiveBlock::new).collect();
        let mut quilt = Quilt::new(stable, Log::starting_with(task.as_str()));
        let max_turns = self.agent.budget.max_turns();
        while self.turns < max_turns {
            match self.turn(&mut quilt, &mut live).await {
                Ok(Some(answer)) => return Outcome::Answered(answer),
                Ok(None) => {}
                Err(failure) => return Outcome::Failed(failure),
            }
            if let Some(stop) = self.over_cost() {
                return stop;
            }
        }
        Outcome::Exhausted(Limit::MaxTurns(max_turns))
    }

    /// Whether the cost limit stops the run before another model call. An
    /// answer is never thrown away for cost: it's already paid for.
    fn over_cost(&self) -> Option<Outcome> {
        let cap_usd = self.agent.budget.max_cost_usd()?;
        match self.usage.and_then(|usage| usage.cost_usd) {
            None => Some(Outcome::Failed(Failure::CostUnreported {
                turn: self.turns,
            })),
            Some(spent_usd) if spent_usd >= cap_usd => {
                Some(Outcome::Exhausted(Limit::MaxCost { cap_usd, spent_usd }))
            }
            Some(_) => None,
        }
    }

    /// One model call and its tool calls. `Some(answer)` when the model
    /// answered. A turn that fails the run is still counted and reported:
    /// it was paid for.
    async fn turn(
        &mut self,
        quilt: &mut Quilt,
        live: &mut [LiveBlock<'_>],
    ) -> Result<Option<String>, Failure> {
        let started = self.clock.now();
        let (live_blocks, live_reports) = refresh(live, self.clock);
        let request = Request {
            max_output_tokens: self.agent.budget.max_output_tokens(),
            ..quilt.render(&self.agent.toolbox, live_blocks)
        };
        let response = self.provider.complete(&request).await?;
        self.turns += 1;
        let step = Step::of(&response, self.turns);
        let results: Vec<ToolResult> = if step == Step::CallTools {
            response
                .turn
                .tool_calls()
                .map(|call| {
                    self.agent
                        .toolbox
                        .call(call)
                        .capped(self.agent.budget.max_tool_output())
                })
                .collect()
        } else {
            Vec::new()
        };
        quilt.append(Entry::Assistant(response.turn));
        let tool_calls = results.len();
        if tool_calls > 0 {
            quilt.append(Entry::ToolResults(results));
        }
        self.retries += response.retries;
        self.usage = Some(
            self.usage
                .map_or(response.usage, |total| total.plus(response.usage)),
        );
        self.reporter.turn(&TurnReport {
            turn: self.turns,
            elapsed_ms: self.clock.now().millis_since(started),
            usage: response.usage,
            tool_calls,
            retries: response.retries,
            stop: response.stop,
            prefix: prefix_fingerprints(&request),
            live: live_reports,
        });
        match step {
            Step::CallTools => Ok(None),
            Step::Answer(answer) => Ok(Some(answer)),
            Step::Fail(failure) => Err(failure),
        }
    }

    /// Stable sources are read once, here, and never again (framework L4).
    fn read_stable(&self) -> Result<Vec<Block>, Failure> {
        self.agent
            .blocks(Kind::Stable)
            .map(|decl| {
                decl.source
                    .fetch()
                    .map(|content| Block {
                        name: decl.name.clone(),
                        content,
                    })
                    .map_err(|error| Failure::Source {
                        block: decl.name.clone(),
                        error,
                    })
            })
            .collect()
    }
}

/// What a reply means for the run. This is the rule for what an answer is:
/// a reply the model ended itself, with no tool calls and some text. A reply
/// cut off at the output limit is never used, answer or tool calls.
#[derive(Debug, PartialEq)]
enum Step {
    CallTools,
    Answer(String),
    Fail(Failure),
}

impl Step {
    fn of(response: &Response, turn: u32) -> Self {
        let calls_tools = response.turn.tool_calls().next().is_some();
        match (response.stop, calls_tools) {
            (Stop::OutputLimit, true) => Self::Fail(Failure::ToolCallsCutOff { turn }),
            (Stop::OutputLimit, false) => Self::Fail(Failure::AnswerCutOff { turn }),
            (Stop::End, true) => Self::CallTools,
            (Stop::End, false) => {
                let answer = response.turn.text();
                if answer.trim().is_empty() {
                    Self::Fail(Failure::NoAnswer { turn })
                } else {
                    Self::Answer(answer)
                }
            }
        }
    }
}

/// Fetch every live block (framework `refresh`). Never fails: a failed
/// fetch renders stale or failed instead.
fn refresh(live: &mut [LiveBlock<'_>], clock: &impl Clock) -> (Vec<Block>, Vec<LiveReport>) {
    live.iter_mut()
        .map(|block| {
            let now = clock.now();
            let freshness = block.refresh(now);
            (
                live::render(block.name(), &freshness, now),
                live::report(block.name(), &freshness),
            )
        })
        .unzip()
}

#[cfg(test)]
mod tests;
