use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::agent::Budget;
use crate::block_on;
use crate::clock::fakes::TickingClock;
use crate::live::Status;
use crate::log::{AssistantTurn, Part};
use crate::provider::{Request, Response, Stop};
use crate::quilt::{BlockDecl, Fetch, Source};
use crate::report::fakes::Recorder;
use crate::toolbox::fakes::{FixedTool, call};
use crate::toolbox::{Invocation, Tool, ToolSpec, Toolbox};

/// What counts as an answer: empty and cut-off replies.
mod answers;
/// The loop at its edges: the last turn, two limits at once, several
/// calls in one turn, a failure mid-run.
mod edges;
/// A host driving a run one turn at a time.
mod host;

/// Answers with whatever `reply` returns for turn n (1-based), reports
/// usage of `100 * n` input tokens, and remembers every request.
struct FakeProvider<F> {
    reply: F,
    seen: Mutex<Vec<Request>>,
    /// What each turn costs; `None` for a provider that doesn't say.
    cost_usd: Option<f64>,
    /// The turn whose reply the output limit cuts off, if any.
    cut_off_on: Option<usize>,
}

impl<F: Fn(usize) -> Result<AssistantTurn, ProviderError> + Sync> FakeProvider<F> {
    fn new(reply: F) -> Self {
        Self {
            reply,
            seen: Mutex::new(Vec::new()),
            cost_usd: Some(0.5),
            cut_off_on: None,
        }
    }

    fn cut_off_on(self, turn: usize) -> Self {
        Self {
            cut_off_on: Some(turn),
            ..self
        }
    }

    fn without_cost(self) -> Self {
        Self {
            cost_usd: None,
            ..self
        }
    }

    fn requests(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }
}

fn usage_for(turn: usize) -> Usage {
    let turn = turn as u64;
    Usage {
        input: Some(100 * turn),
        output: Some(10),
        cache_read: Some(90 * (turn - 1)),
        cache_write: None,
        cost_usd: Some(0.5),
    }
}

impl<F: Fn(usize) -> Result<AssistantTurn, ProviderError> + Sync> Provider for FakeProvider<F> {
    fn complete(
        &self,
        request: &Request,
    ) -> impl Future<Output = Result<Response, ProviderError>> + Send {
        let mut seen = self.seen.lock().unwrap();
        seen.push(request.clone());
        let turn = seen.len();
        let reply = (self.reply)(turn).map(|reply| Response {
            turn: reply,
            stop: if self.cut_off_on == Some(turn) {
                Stop::OutputLimit
            } else {
                Stop::End
            },
            usage: Usage {
                cost_usd: self.cost_usd,
                ..usage_for(turn)
            },
            retries: u32::try_from(turn % 2).unwrap(),
        });
        std::future::ready(reply)
    }
}

/// Counts fetches; answers `be terse`.
#[derive(Clone, Default)]
struct CountingSource(Arc<AtomicUsize>);

impl Source for CountingSource {
    fn fetch(&self) -> Fetch<'_> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok("be terse".into())))
    }
}

/// Answers `prefix-<n>` on the nth fetch, or fails on the fetches listed.
struct Counter {
    prefix: &'static str,
    fail_on: Vec<usize>,
    fetches: AtomicUsize,
}

impl Counter {
    fn new(prefix: &'static str, fail_on: Vec<usize>) -> Self {
        Self {
            prefix,
            fail_on,
            fetches: AtomicUsize::new(0),
        }
    }
}

impl Source for Counter {
    fn fetch(&self) -> Fetch<'_> {
        let n = self.fetches.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(std::future::ready(if self.fail_on.contains(&n) {
            Err(SourceError(format!("fetch {n} failed")))
        } else {
            Ok(format!("{}-{n}", self.prefix))
        }))
    }
}

struct BrokenSource;

impl Source for BrokenSource {
    fn fetch(&self) -> Fetch<'_> {
        Box::pin(std::future::ready(Err(SourceError("no such file".into()))))
    }
}

/// Runs with a clock starting at `start` seconds that ticks a second per read.
fn run_at(start: u64, agent: &Agent, provider: &impl Provider) -> (Outcome, Recorder) {
    let clock = TickingClock::new(start * 1000, 1000);
    let mut reporter = Recorder::default();
    let outcome = block_on(execute(agent, &task(), provider, &clock, &mut reporter));
    (outcome, reporter)
}

fn run(agent: &Agent, provider: &impl Provider) -> (Outcome, Recorder) {
    run_at(0, agent, provider)
}

fn calls(name: &str) -> AssistantTurn {
    AssistantTurn {
        parts: vec![Part::ToolCall(call(name))],
        provider_data: None,
    }
}

fn says(text: &str) -> AssistantTurn {
    AssistantTurn {
        parts: vec![Part::Text(text.into())],
        provider_data: None,
    }
}

fn block(kind: Kind, name: &str, source: impl Source + 'static) -> BlockDecl {
    BlockDecl {
        name: BlockName::new(name).unwrap(),
        kind,
        source: Box::new(source),
    }
}

fn agent_with(blocks: Vec<BlockDecl>, tools: Vec<Box<dyn Tool>>, turns: u32) -> Agent {
    budgeted(blocks, tools, Budget::new(turns).unwrap())
}

fn budgeted(blocks: Vec<BlockDecl>, tools: Vec<Box<dyn Tool>>, budget: Budget) -> Agent {
    Agent::new(blocks, Toolbox::grant(tools).unwrap(), budget).unwrap()
}

/// An agent with the `now` tool, ten turns and a cost limit.
fn capped_at(usd: f64) -> Agent {
    let budget = Budget::new(10).unwrap().with_max_cost(usd).unwrap();
    budgeted(vec![], now_tool(), budget)
}

fn agent(source: impl Source + 'static, tools: Vec<Box<dyn Tool>>, turns: u32) -> Agent {
    agent_with(vec![block(Kind::Stable, "core.md", source)], tools, turns)
}

fn now_tool() -> Vec<Box<dyn Tool>> {
    vec![Box::new(FixedTool::new("now", "noon"))]
}

fn task() -> Task {
    Task::new("What time is it?").unwrap()
}

#[test]
fn a_tool_call_then_an_answer() {
    let agent = agent(CountingSource::default(), now_tool(), 5);
    let provider = FakeProvider::new(|turn| {
        Ok(if turn == 1 {
            calls("now")
        } else {
            says("It is noon.")
        })
    });

    let (outcome, _) = run(&agent, &provider);

    assert_eq!(outcome, Outcome::Answered("It is noon.".into()));
    let second = &provider.requests()[1];
    assert_eq!(second.tools.len(), 1);
    assert_eq!(
        second.log[2],
        Entry::ToolResults(vec![ToolResult {
            call_id: "c1".into(),
            name: "now".into(),
            output: Ok("noon".into()),
        }])
    );
}

#[test]
fn an_unreadable_stable_source_fails_the_run_before_any_call() {
    let agent = agent(BrokenSource, vec![], 5);
    let provider = FakeProvider::new(|_| Ok(says("unreachable")));

    let (outcome, reports) = run(&agent, &provider);

    assert!(
        matches!(outcome, Outcome::Failed(Failure::Source { block, .. }) if block.as_str() == "core.md")
    );
    assert_eq!(provider.requests(), Vec::<Request>::new());
    let (summary, _) = reports.finished.expect("a failed run is still summarised");
    assert_eq!(summary.turns, 0);
    assert_eq!(summary.usage, Usage::default());
}

#[test]
fn arch_run_always_terminates() {
    let looping = agent(CountingSource::default(), now_tool(), 3);
    let always_calls = FakeProvider::new(|_| Ok(calls("now")));
    assert_eq!(
        run(&looping, &always_calls).0,
        Outcome::Exhausted(Limit::MaxTurns(3))
    );
    assert_eq!(always_calls.requests().len(), 3);

    let always_errors = FakeProvider::new(|_| Err(ProviderError::new("503")));
    assert_eq!(
        run(&looping, &always_errors).0,
        Outcome::Failed(Failure::Provider(ProviderError::new("503")))
    );
}

#[test]
fn arch_log_prefix_survives_turns() {
    let agent = agent(CountingSource::default(), now_tool(), 4);
    let provider = FakeProvider::new(|turn| Ok(if turn < 3 { calls("now") } else { says("noon") }));

    run(&agent, &provider);

    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    for pair in requests.windows(2) {
        assert_eq!(pair[0].tools, pair[1].tools);
        assert_eq!(pair[0].stable, pair[1].stable);
        assert!(
            pair[1].log.starts_with(&pair[0].log),
            "turn n's log is a prefix of turn n+1's"
        );
    }
}

#[test]
fn arch_stable_sources_read_once() {
    let source = CountingSource::default();
    let fetches = Arc::clone(&source.0);
    let agent = agent(source, now_tool(), 5);
    let provider = FakeProvider::new(|turn| Ok(if turn < 4 { calls("now") } else { says("noon") }));

    run(&agent, &provider);

    assert_eq!(provider.requests().len(), 4);
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
}

#[test]
fn arch_deny_by_default_in_the_loop() {
    let now = Arc::new(FixedTool::new("now", "noon"));
    let agent = agent(
        CountingSource::default(),
        vec![Box::new(SharedTool(Arc::clone(&now)))],
        3,
    );
    let provider = FakeProvider::new(|turn| Ok(if turn == 1 { calls("rm") } else { says("ok") }));

    run(&agent, &provider);

    let results = &provider.requests()[1].log[2];
    assert!(matches!(results, Entry::ToolResults(r) if r[0].output.is_err()));
    assert_eq!(now.invocations.load(Ordering::SeqCst), 0);
}

#[test]
fn live_blocks_are_refreshed_every_turn_and_rendered_after_the_log() {
    let agent = agent_with(
        vec![
            block(Kind::Live, "clock", Counter::new("tick", vec![])),
            block(Kind::Stable, "core.md", CountingSource::default()),
        ],
        now_tool(),
        3,
    );
    let provider = FakeProvider::new(|turn| {
        Ok(if turn == 1 {
            calls("now")
        } else {
            says("noon")
        })
    });

    let (_, reports) = run(&agent, &provider);

    let requests = provider.requests();
    assert!(requests[0].live[0].content.contains("\ntick-1\n"));
    assert!(requests[1].live[0].content.contains("\ntick-2\n"));
    assert_eq!(
        requests[1].stable.len(),
        1,
        "live blocks never land above the log"
    );
    assert_eq!(reports.turns[1].live[0].status, Status::Fresh);
}

#[test]
fn arch_nothing_volatile_above_log() {
    let agent_for = |prefix| {
        agent_with(
            vec![
                block(Kind::Stable, "core.md", CountingSource::default()),
                block(Kind::Live, "clock", Counter::new(prefix, vec![])),
            ],
            now_tool(),
            1,
        )
    };
    let monday = FakeProvider::new(|_| Ok(says("noon")));
    let tuesday = FakeProvider::new(|_| Ok(says("noon")));

    run_at(1_000, &agent_for("monday"), &monday);
    run_at(90_000, &agent_for("tuesday"), &tuesday);

    let (a, b) = (&monday.requests()[0], &tuesday.requests()[0]);
    assert_ne!(a.live, b.live, "different clocks and live content");
    assert_eq!((&a.tools, &a.stable, &a.log), (&b.tools, &b.stable, &b.log));
}

#[test]
fn arch_live_failure_never_aborts() {
    let agent = agent_with(
        vec![block(Kind::Live, "clock", Counter::new("tick", vec![1, 3]))],
        now_tool(),
        4,
    );
    let provider = FakeProvider::new(|turn| Ok(if turn < 3 { calls("now") } else { says("noon") }));

    let (outcome, reports) = run(&agent, &provider);

    assert_eq!(outcome, Outcome::Answered("noon".into()));
    let statuses: Vec<_> = reports.turns.iter().map(|t| t.live[0].status).collect();
    assert_eq!(statuses, [Status::Failed, Status::Fresh, Status::Stale]);
    let third = &provider.requests()[2].live[0].content;
    assert!(
        third.contains("status=\"stale\"") && third.contains("\ntick-2\n"),
        "{third}"
    );
}

#[test]
fn arch_usage_is_reported_not_estimated() {
    let agent = agent(CountingSource::default(), now_tool(), 5);
    let provider = FakeProvider::new(|turn| Ok(if turn < 3 { calls("now") } else { says("noon") }));

    let (_, reports) = run(&agent, &provider);

    let reported: Vec<Usage> = reports.turns.iter().map(|t| t.usage).collect();
    assert_eq!(reported, [usage_for(1), usage_for(2), usage_for(3)]);
    let retries: Vec<u32> = reports.turns.iter().map(|t| t.retries).collect();
    assert_eq!(retries, [1, 0, 1], "each turn reports its own retries");
    let (summary, outcome) = reports.finished.unwrap();
    assert_eq!(outcome, Outcome::Answered("noon".into()));
    assert_eq!(summary.turns, 3);
    assert_eq!(summary.usage.input, Some(600));
    assert_eq!(summary.usage.cache_read, Some(270));
    assert_eq!(
        summary.usage.cache_write, None,
        "never reported, so unknown, not 0"
    );
    assert_eq!(summary.usage.cost_usd, Some(1.5), "reported cost, summed");
    assert_eq!(summary.retries, 2, "turns 1 and 3 each retried once");
}

#[test]
fn turn_reports_carry_timing_and_the_prefix_fingerprints() {
    let agent = agent(CountingSource::default(), now_tool(), 5);
    let provider = FakeProvider::new(|turn| {
        Ok(if turn == 1 {
            calls("now")
        } else {
            says("noon")
        })
    });

    let (_, reports) = run(&agent, &provider);

    let [first, second] = reports.turns.as_slice() else {
        panic!("two turns expected");
    };
    assert_eq!((first.turn, first.tool_calls), (1, 1));
    assert_eq!((second.turn, second.tool_calls), (2, 0));
    assert!(first.elapsed_ms > 0);
    assert_eq!(
        first.prefix, second.prefix,
        "nothing above the log changes in a run"
    );
    assert_eq!(first.prefix.len(), 2);
}

/// Lets a test keep a handle on a tool it granted.
struct SharedTool(Arc<FixedTool>);

impl Tool for SharedTool {
    fn spec(&self) -> &ToolSpec {
        self.0.spec()
    }

    fn invoke<'a>(&'a self, arguments: &'a serde_json::Value) -> Invocation<'a> {
        self.0.invoke(arguments)
    }
}

#[test]
fn arch_run_stops_at_its_cost_limit() {
    // Each fake turn costs $0.50, so a $1 limit allows two.
    let always_calls = FakeProvider::new(|_| Ok(calls("now")));
    assert_eq!(
        run(&capped_at(1.0), &always_calls).0,
        Outcome::Exhausted(Limit::MaxCost {
            cap_usd: 1.0,
            spent_usd: 1.0
        })
    );
    assert_eq!(always_calls.requests().len(), 2);
}

#[test]
fn an_answer_is_kept_even_when_it_crosses_the_cost_limit() {
    let answers = FakeProvider::new(|_| Ok(says("noon")));
    assert_eq!(
        run(&capped_at(0.1), &answers).0,
        Outcome::Answered("noon".into())
    );
}

#[test]
fn a_cost_limit_with_unreported_cost_stops_rather_than_spend_blind() {
    let silent = FakeProvider::new(|_| Ok(calls("now"))).without_cost();
    assert_eq!(
        run(&capped_at(5.0), &silent).0,
        Outcome::Failed(Failure::CostUnreported { turn: 1 })
    );
    assert_eq!(silent.requests().len(), 1);

    let uncapped = agent_with(vec![], now_tool(), 2);
    let silent = FakeProvider::new(|_| Ok(calls("now"))).without_cost();
    assert_eq!(
        run(&uncapped, &silent).0,
        Outcome::Exhausted(Limit::MaxTurns(2)),
        "no limit, no problem"
    );
}

#[test]
fn arch_tool_output_is_capped_before_it_reaches_the_log() {
    let chatty: Vec<Box<dyn Tool>> = vec![Box::new(FixedTool::new("now", "twelve o'clock"))];
    let budget = Budget::new(2).unwrap().with_max_tool_output(6).unwrap();
    let agent = budgeted(vec![], chatty, budget);
    let provider = FakeProvider::new(|turn| Ok(if turn == 1 { calls("now") } else { says("ok") }));

    run(&agent, &provider);

    let Entry::ToolResults(results) = &provider.requests()[1].log[2] else {
        panic!("expected tool results");
    };
    assert_eq!(
        results[0].output,
        Ok("twelve\n[output cut: showing the first 6 of 14 bytes]".into())
    );
}

/// A report's structured reason: a limit or a failure, each with its
/// `kind`, so scripts read `.reason.kind` rather than parse a sentence.
#[test]
fn an_outcome_reason_is_structured_by_kind() {
    let reason = |outcome: &Outcome| serde_json::to_value(outcome.reason()).unwrap();
    assert_eq!(
        reason(&Outcome::Answered("noon".into())),
        serde_json::Value::Null
    );
    assert_eq!(
        reason(&Outcome::Exhausted(Limit::MaxTurns(3))),
        serde_json::json!({"kind": "max_turns", "turns": 3})
    );
    assert_eq!(
        reason(&Outcome::Exhausted(Limit::MaxCost {
            cap_usd: 1.0,
            spent_usd: 1.5
        })),
        serde_json::json!({"kind": "max_cost", "cap_usd": 1.0, "spent_usd": 1.5})
    );
    assert_eq!(
        reason(&Outcome::Failed(Failure::NoAnswer {
            turn: 2,
            tools: false
        })),
        serde_json::json!({"kind": "no_answer", "turn": 2, "tools": false})
    );
    assert_eq!(
        reason(&Outcome::Failed(Failure::Provider(
            ProviderError::new("429").after_retries(3)
        ))),
        serde_json::json!({"kind": "provider", "message": "429", "retries": 3})
    );
    assert_eq!(
        reason(&Outcome::Failed(Failure::Source {
            block: crate::quilt::BlockName::new("a.md").unwrap(),
            error: SourceError("gone".into()),
        })),
        serde_json::json!({"kind": "source", "block": "a.md", "error": "gone"})
    );
}
