//! A run: one execution of an Agent against a Task, ending in an Outcome.
//!
//! ```text
//! Preparing --(stable sources read)--> Turning --(a whole answer)----> Answered
//!                                         |--(budget reached)------> Exhausted
//!     any failure, or a reply that isn't -+------------------------> Failed
//!     a whole answer (empty, or cut off)
//! ```

use serde::Serialize;
use serde::ser::SerializeStruct;

use crate::agent::{Agent, Task};
use crate::clock::Clock;
use crate::live::{self, LiveBlock, LiveReport};
use crate::log::{AssistantTurn, Entry, Log, Part};
use crate::provider::{Provider, ProviderError, Request, Response, Stop};
use crate::quilt::{Block, BlockName, Kind, Quilt, SourceError};
use crate::report::{Reporter, RunSummary, TurnReport, Usage, prefix_fingerprints};
use crate::toolbox::ToolResult;

/// For an empty reply in a run with tools: what else it can be.
fn dropped_call(tools: bool) -> &'static str {
    if tools {
        "; with tools granted, the provider may have dropped a tool call it couldn't read"
    } else {
        ""
    }
}

/// For a reply that only reasoned: what to try.
fn what_may_help(tools: bool) -> &'static str {
    if tools {
        "with tools granted, the provider may have dropped a tool call it couldn't read \
         (if the reasoning ends by making one, try another model), or another reasoning \
         effort may get one"
    } else {
        "another reasoning effort may get one"
    }
}

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

/// `{"kind": "max_turns", "turns": 3}` or `{"kind": "max_cost", "cap_usd":
/// 1.0, "spent_usd": 1.02}`: a report's structured reason.
impl Serialize for Limit {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match *self {
            Self::MaxTurns(turns) => {
                let mut limit = serializer.serialize_struct("Limit", 2)?;
                limit.serialize_field("kind", "max_turns")?;
                limit.serialize_field("turns", &turns)?;
                limit.end()
            }
            Self::MaxCost { cap_usd, spent_usd } => {
                let mut limit = serializer.serialize_struct("Limit", 3)?;
                limit.serialize_field("kind", "max_cost")?;
                limit.serialize_field("cap_usd", &cap_usd)?;
                limit.serialize_field("spent_usd", &spent_usd)?;
                limit.end()
            }
        }
    }
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

/// Serialized as `{"kind": "no_answer", "turn": 2}` and the like: a
/// report's structured reason, beside the sentence `Display` gives.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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
    /// A reply with no tool calls and no text (or only whitespace). `tools`:
    /// the run had tools, so the reply may have been a call the provider
    /// failed to read and dropped (seen with some local models).
    #[error("the model gave no answer: its reply on turn {turn} was empty{}", dropped_call(*.tools))]
    NoAnswer { turn: u32, tools: bool },
    /// [`Failure::NoAnswer`] from a model that reasoned first: it stopped
    /// mid-thought, rather than the provider sending nothing. `ending` is
    /// the reasoning's last line, cut to its last [`ENDING_CHARS`].
    #[error(
        "the model gave no answer: on turn {turn} it reasoned ({chars} characters, \
         ending {ending:?}) but wrote no reply; {}",
        what_may_help(*.tools)
    )]
    OnlyReasoned {
        turn: u32,
        chars: usize,
        ending: String,
        tools: bool,
    },
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

impl Outcome {
    /// `answered`, `exhausted` or `failed`: how every report names it.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Answered(_) => "answered",
            Self::Exhausted(_) => "exhausted",
            Self::Failed(_) => "failed",
        }
    }

    /// Why it didn't answer, structured for reports; `None` when it did.
    /// Never the answer itself, which belongs on stdout alone.
    #[must_use]
    pub fn reason(&self) -> Option<Reason<'_>> {
        match self {
            Self::Answered(_) => None,
            Self::Exhausted(limit) => Some(Reason::Limit(limit)),
            Self::Failed(failure) => Some(Reason::Failure(failure)),
        }
    }
}

/// [`Outcome::reason`]: a limit or a failure, each `{"kind": ..., ...}`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Reason<'a> {
    Limit(&'a Limit),
    Failure(&'a Failure),
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
    let (outcome, run) = match Run::start(agent, task).await {
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
    pub async fn start(agent: &'a Agent, task: &Task) -> Result<Self, Failure> {
        Ok(Self {
            agent,
            quilt: Quilt::new(read_stable(agent).await?, Log::starting_with(task.as_str())),
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
        let (live_blocks, live_reports) = refresh(&mut self.live, clock).await;
        let request = Request {
            max_output_tokens: self.agent.budget.max_output_tokens(),
            reasoning: self.agent.budget.reasoning(),
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
        let tools = !self.agent.toolbox.is_empty();
        let (ended, calls_tools) = match Step::of(&response, self.turns, tools) {
            // When no turn follows, nothing would read the tools' results,
            // so they don't run (a tool that sends mail would send it for
            // nothing).
            Step::CallTools => (self.over_budget(), true),
            Step::Answer(answer) => (Some(Outcome::Answered(answer)), false),
            Step::Fail(failure) => (Some(Outcome::Failed(failure)), false),
        };
        let asked = response.turn.tool_calls().count();
        // One at a time, in the order the model asked: a call may depend on
        // the one before it, and results go back in call order.
        let mut results: Vec<ToolResult> = Vec::new();
        if calls_tools && ended.is_none() {
            for call in response.turn.tool_calls() {
                let result = self.agent.toolbox.call(call).await;
                results.push(result.capped(self.agent.budget.max_tool_output()));
            }
        }
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

/// Stable sources are read once, at the start of a run (framework L4), in
/// layout order.
async fn read_stable(agent: &Agent) -> Result<Vec<Block>, Failure> {
    let mut blocks = Vec::new();
    for decl in agent.blocks(Kind::Stable) {
        let content = decl.source.fetch().await.map_err(|error| Failure::Source {
            block: decl.name.clone(),
            error,
        })?;
        blocks.push(Block {
            name: decl.name.clone(),
            content,
        });
    }
    Ok(blocks)
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
    /// `tools`: whether the run has tools, for what an empty reply may be.
    fn of(response: &Response, turn: u32, tools: bool) -> Self {
        let calls_tools = response.turn.tool_calls().next().is_some();
        match (response.stop, calls_tools) {
            (Stop::OutputLimit, true) => Self::Fail(Failure::ToolCallsCutOff { turn }),
            (Stop::OutputLimit, false) => Self::Fail(Failure::AnswerCutOff { turn }),
            (Stop::End, true) => Self::CallTools,
            (Stop::End, false) => {
                let answer = response.turn.text();
                if !answer.trim().is_empty() {
                    Self::Answer(answer)
                } else if let Some((chars, ending)) = reasoned(&response.turn) {
                    Self::Fail(Failure::OnlyReasoned {
                        turn,
                        chars,
                        ending,
                        tools,
                    })
                } else {
                    Self::Fail(Failure::NoAnswer { turn, tools })
                }
            }
        }
    }
}

/// How much of a reasoning's end an error quotes: enough to tell a
/// thought cut short from one that concluded, short enough for one line.
pub const ENDING_CHARS: usize = 80;

/// The reasoning in `turn`, if any: its length in characters, and its last
/// line cut to the last [`ENDING_CHARS`] (marked `…` when cut).
fn reasoned(turn: &AssistantTurn) -> Option<(usize, String)> {
    let reasoning: String = turn
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Thinking(thought) => Some(thought.as_str()),
            _ => None,
        })
        .collect();
    let last = reasoning
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())?;
    let count = last.chars().count();
    let ending = if count > ENDING_CHARS {
        let kept: String = last.chars().skip(count - ENDING_CHARS).collect();
        format!("…{kept}")
    } else {
        last.to_owned()
    };
    Some((reasoning.chars().count(), ending))
}

/// Fetch every live block (framework `refresh`), one at a time in layout
/// order. Never fails: a failed fetch renders stale or failed instead.
async fn refresh(live: &mut [LiveBlock<'_>], clock: &impl Clock) -> (Vec<Block>, Vec<LiveReport>) {
    let mut blocks = Vec::with_capacity(live.len());
    let mut reports = Vec::with_capacity(live.len());
    for block in live.iter_mut() {
        let now = clock.now();
        let freshness = block.refresh(now).await;
        blocks.push(live::render(block.name(), &freshness, now));
        reports.push(live::report(block.name(), &freshness));
    }
    (blocks, reports)
}

#[cfg(test)]
mod tests;
