//! The run loop at its edges. These pin behaviour the rest of the suite
//! takes for granted.

use super::*;
use crate::toolbox::ToolCall;

#[test]
fn an_answer_on_the_last_allowed_turn_is_answered_not_exhausted() {
    let agent = agent(CountingSource::default(), now_tool(), 2);
    let provider = FakeProvider::new(|turn| {
        Ok(if turn == 1 {
            calls("now")
        } else {
            says("noon")
        })
    });

    assert_eq!(run(&agent, &provider).0, Outcome::Answered("noon".into()));
}

#[test]
fn the_cost_limit_wins_when_both_limits_are_reached_on_one_turn() {
    // Two turns at $0.50 each reach both a $1 limit and a two-turn limit.
    let budget = Budget::new(2).unwrap().with_max_cost(1.0).unwrap();
    let agent = budgeted(vec![], now_tool(), budget);
    let always_calls = FakeProvider::new(|_| Ok(calls("now")));

    assert_eq!(
        run(&agent, &always_calls).0,
        Outcome::Exhausted(Limit::MaxCost {
            cap_usd: 1.0,
            spent_usd: 1.0
        })
    );
}

#[test]
fn several_tool_calls_in_one_turn_report_in_call_order_with_their_ids() {
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(FixedTool::new("now", "noon")),
        Box::new(FixedTool::new("date", "monday")),
    ];
    let agent = agent(CountingSource::default(), tools, 3);
    let provider = FakeProvider::new(|turn| {
        Ok(if turn == 1 {
            AssistantTurn {
                parts: vec![
                    Part::ToolCall(ToolCall {
                        id: "z9".into(),
                        ..call("now")
                    }),
                    Part::ToolCall(ToolCall {
                        id: "a1".into(),
                        ..call("date")
                    }),
                ],
                provider_data: None,
            }
        } else {
            says("noon on monday")
        })
    });

    let (_, reports) = run(&agent, &provider);

    let Entry::ToolResults(results) = &provider.requests()[1].log[2] else {
        panic!("expected tool results");
    };
    let ids: Vec<_> = results
        .iter()
        .map(|r| (r.call_id.as_str(), r.output.as_deref()))
        .collect();
    assert_eq!(ids, [("z9", Ok("noon")), ("a1", Ok("monday"))]);
    assert_eq!(reports.turns[0].tool_calls, 2);
}

#[test]
fn a_provider_failure_after_good_turns_keeps_their_reports_and_totals() {
    let agent = agent(CountingSource::default(), now_tool(), 5);
    let gave_up = ProviderError::new("503 (gave up after 3 retries)").after_retries(3);
    let provider = FakeProvider::new(|turn| {
        if turn < 3 {
            Ok(calls("now"))
        } else {
            Err(gave_up.clone())
        }
    });

    let (outcome, reports) = run(&agent, &provider);

    assert_eq!(outcome, Outcome::Failed(Failure::Provider(gave_up.clone())));
    let turns: Vec<_> = reports
        .turns
        .iter()
        .map(|t| (t.turn, t.retries, t.stop))
        .collect();
    assert_eq!(
        turns,
        [
            (1, 1, Some(Stop::End)),
            (2, 0, Some(Stop::End)),
            (3, 3, None),
        ],
        "the failed call is reported too, with its retries and no reply"
    );
    assert_eq!(reports.turns[2].usage, Usage::default(), "nothing reported");
    let (summary, _) = reports.finished.unwrap();
    assert_eq!(summary.turns, 2, "turns with a reply");
    assert_eq!(summary.retries, 4, "1 + 0, and the failed call's 3");
    assert_eq!(summary.usage.input, Some(300), "turns 1 and 2: 100 + 200");
    assert_eq!(summary.usage.cost_usd, Some(1.0));
}
