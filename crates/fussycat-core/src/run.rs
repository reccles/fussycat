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
/// reached, or something fails: [`Run::turn`] in a loop. Every turn is
/// reported, and so is the run.
pub async fn execute(
    agent: &Agent,
    task: &Task,
    provider: &impl Provider,
    clock: &impl Clock,
    reporter: &mut impl Reporter,
) -> Outcome {
    let started = clock.now();
    let (outcome, run) = match Run::start(agent, task) {
        Ok(mut run) => loop {
            if let Some(outcome) = run.turn(provider, clock, reporter).await {
                break (outcome, Some(run));
            }
        },
        Err(failure) => (Outcome::Failed(failure), None),
    };
    let elapsed_ms = clock.now().millis_since(started);
    let summary = run.map_or(
        RunSummary {
            turns: 0,
            elapsed_ms,
            usage: Usage::default(),
            retries: 0,
        },
        |run| run.summary(elapsed_ms),
    );
    reporter.finish(&summary, &outcome);
    outcome
}

/// The aggregate root: one execution, its quilt and its running totals.
/// The ports are lent to each turn, so a host that keeps a run between
/// events can append to it and ask for one turn at a time.
pub struct Run<'a> {
    agent: &'a Agent,
    quilt: Quilt,
    live: Vec<LiveBlock<'a>>,
    turns: u32,
    /// `None` until the first turn: no turns means nothing was reported.
    usage: Option<Usage>,
    retries: u32,
}

impl<'a> Run<'a> {
    /// Read the stable sources, once and never again (framework L4), and
    /// start the log with `task`.
    pub fn start(agent: &'a Agent, task: &Task) -> Result<Self, Failure> {
        Ok(Self {
            agent,
            quilt: Quilt::new(read_stable(agent)?, Log::starting_with(task.as_str())),
            live: agent.blocks(Kind::Live).map(LiveBlock::new).collect(),
            turns: 0,
            usage: None,
            retries: 0,
        })
    }

    /// Add an entry to the log, for the next turn to see.
    pub fn append(&mut self, entry: Entry) {
        self.quilt.append(entry);
    }

    /// One turn (framework `turn`): check the budget, then one model call
    /// and its tool calls. `None` when the model called tools and another
    /// turn will read their results; otherwise how the run ended. Every
    /// turn is reported.
    pub async fn turn(
        &mut self,
        provider: &impl Provider,
        clock: &impl Clock,
        reporter: &mut impl Reporter,
    ) -> Option<Outcome> {
        if let Some(stop) = self.over_budget() {
            return Some(stop);
        }
        self.call(provider, clock, reporter).await
    }

    /// The totals so far, for a run that took `elapsed_ms`.
    #[must_use]
    pub fn summary(&self, elapsed_ms: u64) -> RunSummary {
        RunSummary {
            turns: self.turns,
            elapsed_ms,
            usage: self.usage.unwrap_or_default(),
            retries: self.retries,
        }
    }

    /// Whether a budget limit stops the run before another model call. The
    /// cost limit is checked first, so it wins when both are reached on one
    /// turn. An answer is never thrown away for cost: it's already paid for.
    fn over_budget(&self) -> Option<Outcome> {
        if self.turns == 0 {
            return None;
        }
        if let Some(cap_usd) = self.agent.budget.max_cost_usd() {
            match self.usage.and_then(|usage| usage.cost_usd) {
                None => {
                    return Some(Outcome::Failed(Failure::CostUnreported {
                        turn: self.turns,
                    }));
                }
                Some(spent_usd) if spent_usd >= cap_usd => {
                    return Some(Outcome::Exhausted(Limit::MaxCost { cap_usd, spent_usd }));
                }
                Some(_) => {}
            }
        }
        let max_turns = self.agent.budget.max_turns();
        (self.turns >= max_turns).then_some(Outcome::Exhausted(Limit::MaxTurns(max_turns)))
    }

    /// One model call and its tool calls, which run only if another turn
    /// will read their results. A turn that fails the run is still counted
    /// and reported: it was paid for. A call that fails outright is reported
    /// too, with its retries, but isn't counted: there was no reply.
    async fn call(
        &mut self,
        provider: &impl Provider,
        clock: &impl Clock,
        reporter: &mut impl Reporter,
    ) -> Option<Outcome> {
        let started = clock.now();
        let (live_blocks, live_reports) = refresh(&mut self.live, clock);
        let request = Request {
            max_output_tokens: self.agent.budget.max_output_tokens(),
            ..self.quilt.render(&self.agent.toolbox, live_blocks)
        };
        let response = match provider.complete(&request).await {
            Ok(response) => response,
            Err(error) => {
                // No reply, so not a turn the summary counts, but its
                // retries and time are reported all the same.
                self.retries += error.retries;
                reporter.turn(&TurnReport {
                    turn: self.turns + 1,
                    elapsed_ms: clock.now().millis_since(started),
                    usage: Usage::default(),
                    tool_calls: 0,
                    skipped_tool_calls: 0,
                    retries: error.retries,
                    stop: None,
                    prefix: prefix_fingerprints(&request),
                    live: live_reports,
                });
                return Some(Outcome::Failed(error.into()));
            }
        };
        self.turns += 1;
        self.retries += response.retries;
        self.usage = Some(
            self.usage
                .map_or(response.usage, |total| total.plus(response.usage)),
        );
        let (ended, calls_tools) = match Step::of(&response, self.turns) {
            // When no turn follows, nothing would read the tools' results,
            // so they don't run (a tool that sends mail would send it for
            // nothing).
            Step::CallTools => (self.over_budget(), true),
            Step::Answer(answer) => (Some(Outcome::Answered(answer)), false),
            Step::Fail(failure) => (Some(Outcome::Failed(failure)), false),
        };
        let asked = response.turn.tool_calls().count();
        let results: Vec<ToolResult> = if calls_tools && ended.is_none() {
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
        self.quilt.append(Entry::Assistant(response.turn));
        let tool_calls = results.len();
        if tool_calls > 0 {
            self.quilt.append(Entry::ToolResults(results));
        }
        reporter.turn(&TurnReport {
            turn: self.turns,
            elapsed_ms: clock.now().millis_since(started),
            usage: response.usage,
            tool_calls,
            skipped_tool_calls: if calls_tools { asked - tool_calls } else { 0 },
            retries: response.retries,
            stop: Some(response.stop),
            prefix: prefix_fingerprints(&request),
            live: live_reports,
        });
        ended
    }
}

/// Stable sources are read once, at the start of a run (framework L4).
fn read_stable(agent: &Agent) -> Result<Vec<Block>, Failure> {
    agent
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
