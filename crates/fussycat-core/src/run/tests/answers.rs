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
    for reply in [says(""), says(" \n\t"), thinks(" \n")] {
        let agent = agent(CountingSource::default(), now_tool(), 5);
        let provider = FakeProvider::new(move |_| Ok(reply.clone()));

        let (outcome, reporter) = run(&agent, &provider);

        assert_eq!(
            outcome,
            Outcome::Failed(Failure::NoAnswer {
                turn: 1,
                tools: true
            })
        );
        assert!(
            outcome_text(&outcome).starts_with("the model gave no answer"),
            "{outcome:?}"
        );
        assert_eq!(reporter.turns.len(), 1, "the turn was paid for: reported");
        assert_eq!(provider.requests().len(), 1, "and the run stops there");
    }
}

/// A model that reasoned and wrote nothing stopped mid-thought: the
/// error says so, with how much it reasoned and how the reasoning ended,
/// so the author can tell it from a provider that sent nothing.
#[test]
fn reasoning_with_no_reply_says_the_model_reasoned() {
    let agent = agent(CountingSource::default(), vec![], 5);
    let provider =
        FakeProvider::new(|_| Ok(thinks("The user wants the time.\nI could say noon.\n")));

    let (outcome, _) = run(&agent, &provider);

    assert_eq!(
        outcome_text(&outcome),
        "the model gave no answer: on turn 1 it reasoned (43 characters, ending \
         \"I could say noon.\") but wrote no reply; another reasoning effort may get one"
    );
}

/// With tools granted, an empty reply may be a tool call the provider
/// couldn't read and dropped (some local models lose about half), so the
/// error says so, rather than only blaming the reasoning effort.
#[test]
fn an_empty_reply_with_tools_granted_mentions_a_dropped_call() {
    let agent = agent(CountingSource::default(), now_tool(), 5);
    let reasoned = FakeProvider::new(|_| Ok(thinks("Let me make the tool call now.")));
    let text = outcome_text(&run(&agent, &reasoned).0);
    assert!(
        text.ends_with(
            "but wrote no reply; with tools granted, the provider may have dropped a tool \
             call it couldn't read (if the reasoning ends by making one, try another model), \
             or another reasoning effort may get one"
        ),
        "{text}"
    );
    let silent = FakeProvider::new(|_| Ok(says("")));
    let text = outcome_text(&run(&agent, &silent).0);
    assert_eq!(
        text,
        "the model gave no answer: its reply on turn 1 was empty; with tools granted, \
         the provider may have dropped a tool call it couldn't read"
    );
    let bare = agent_with(vec![], vec![], 5);
    let text = outcome_text(&run(&bare, &FakeProvider::new(|_| Ok(says("")))).0);
    assert_eq!(
        text,
        "the model gave no answer: its reply on turn 1 was empty"
    );
}

/// However long the reasoning, the error quotes only the end of its last
/// line.
#[test]
fn a_long_reasoning_is_quoted_by_its_end() {
    let thought = format!("{}the end.", "word ".repeat(1000));
    let agent = agent(CountingSource::default(), now_tool(), 5);
    let provider = FakeProvider::new(move |_| Ok(thinks(&thought)));

    let (outcome, _) = run(&agent, &provider);

    let Outcome::Failed(Failure::OnlyReasoned { chars, ending, .. }) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(chars, 5008);
    assert_eq!(ending.chars().count(), ENDING_CHARS + 1, "{ending}");
    assert!(ending.starts_with('…') && ending.ends_with("word the end."));
}

#[test]
fn an_empty_reply_after_tool_calls_is_still_not_an_answer() {
    let agent = agent(CountingSource::default(), now_tool(), 5);
    let provider = FakeProvider::new(|turn| Ok(if turn == 1 { calls("now") } else { says("") }));

    let (outcome, _) = run(&agent, &provider);

    assert_eq!(
        outcome,
        Outcome::Failed(Failure::NoAnswer {
            turn: 2,
            tools: true
        })
    );
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
    assert_eq!(reporter.turns[0].stop, Some(Stop::OutputLimit));
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
        (0, Some(Stop::OutputLimit))
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
    let stops: Vec<_> = reporter.turns.iter().map(|turn| turn.stop).collect();
    assert_eq!(stops, [Some(Stop::End), Some(Stop::End)]);
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
