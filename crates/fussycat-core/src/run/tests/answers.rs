//! What counts as an answer (`Step::of`): a reply the model ended itself,
//! with no tool calls and some text. Everything else fails the run, after
//! the turn is counted and reported.

use super::*;

fn thinks(thought: &str) -> AssistantTurn {
    AssistantTurn {
        parts: vec![Part::Thinking(thought.into())],
        provider_data: None,
    }
}

#[test]
fn an_empty_reply_is_not_an_answer() {
    for reply in [says(""), says(" \n\t"), thinks("I could say noon.")] {
        let agent = agent(CountingSource::default(), now_tool(), 5);
        let provider = FakeProvider::new(move |_| Ok(reply.clone()));

        let (outcome, reporter) = run(&agent, &provider);

        assert_eq!(outcome, Outcome::Failed(Failure::NoAnswer { turn: 1 }));
        assert!(
            outcome_text(&outcome).starts_with("the model gave no answer"),
            "{outcome:?}"
        );
        assert_eq!(reporter.turns.len(), 1, "the turn was paid for: reported");
        assert_eq!(provider.requests().len(), 1, "and the run stops there");
    }
}

#[test]
fn an_empty_reply_after_tool_calls_is_still_not_an_answer() {
    let agent = agent(CountingSource::default(), now_tool(), 5);
    let provider = FakeProvider::new(|turn| Ok(if turn == 1 { calls("now") } else { says("") }));

    let (outcome, _) = run(&agent, &provider);

    assert_eq!(outcome, Outcome::Failed(Failure::NoAnswer { turn: 2 }));
}

#[test]
fn an_answer_cut_off_at_the_output_limit_is_a_failure_not_an_answer() {
    let agent = agent(CountingSource::default(), now_tool(), 5);
    let provider = FakeProvider::new(|_| Ok(says("It is twelve o'"))).cut_off_on(1);

    let (outcome, reporter) = run(&agent, &provider);

    assert_eq!(outcome, Outcome::Failed(Failure::AnswerCutOff { turn: 1 }));
    let text = outcome_text(&outcome);
    assert!(text.contains("cut off at the output limit"), "{text}");
    assert!(
        !text.contains("twelve"),
        "the partial answer stays out: {text}"
    );
    assert_eq!(reporter.turns[0].stop, Stop::OutputLimit);
}

#[test]
fn tool_calls_cut_off_at_the_output_limit_never_run() {
    let now = Arc::new(FixedTool::new("now", "noon"));
    let agent = agent(
        CountingSource::default(),
        vec![Box::new(SharedTool(Arc::clone(&now)))],
        5,
    );
    let provider = FakeProvider::new(|_| Ok(calls("now"))).cut_off_on(1);

    let (outcome, reporter) = run(&agent, &provider);

    assert_eq!(
        outcome,
        Outcome::Failed(Failure::ToolCallsCutOff { turn: 1 })
    );
    assert_eq!(now.invocations.load(Ordering::SeqCst), 0, "no tool ran");
    assert_eq!(provider.requests().len(), 1, "no further model call");
    assert_eq!(
        (reporter.turns[0].tool_calls, reporter.turns[0].stop),
        (0, Stop::OutputLimit)
    );
}

#[test]
fn a_whole_answer_reports_how_each_turn_ended() {
    let agent = agent(CountingSource::default(), now_tool(), 5);
    let provider = FakeProvider::new(|turn| {
        Ok(if turn == 1 {
            calls("now")
        } else {
            says("noon")
        })
    });

    let (outcome, reporter) = run(&agent, &provider);

    assert_eq!(outcome, Outcome::Answered("noon".into()));
    let stops: Vec<Stop> = reporter.turns.iter().map(|turn| turn.stop).collect();
    assert_eq!(stops, [Stop::End, Stop::End]);
}

#[test]
fn every_request_asks_for_the_budgets_output_limit() {
    let reply = |turn| {
        Ok(if turn == 1 {
            calls("now")
        } else {
            says("noon")
        })
    };

    let unset = FakeProvider::new(reply);
    run(&agent(CountingSource::default(), now_tool(), 5), &unset);
    assert!(
        unset
            .requests()
            .iter()
            .all(|r| r.max_output_tokens.is_none()),
        "unset: the adapter's default"
    );

    let budget = Budget::new(5).unwrap().with_max_output_tokens(256).unwrap();
    let capped = FakeProvider::new(reply);
    run(&budgeted(vec![], now_tool(), budget), &capped);
    let asked: Vec<_> = capped
        .requests()
        .iter()
        .map(|r| r.max_output_tokens)
        .collect();
    assert_eq!(asked, [Some(256), Some(256)]);
}

fn outcome_text(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Failed(failure) => failure.to_string(),
        other => panic!("expected a failure, got {other:?}"),
    }
}
