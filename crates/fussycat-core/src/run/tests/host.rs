//! A host that keeps a run between events: it appends to the log and asks
//! for one turn at a time, without `execute`.

use super::*;

#[test]
fn a_host_appends_events_between_turns() {
    let agent = agent(CountingSource::default(), now_tool(), 5);
    let provider = FakeProvider::new(|turn| {
        Ok(match turn {
            1 => says("Noon."),
            2 => calls("now"),
            _ => says("Still noon."),
        })
    });
    let clock = TickingClock::new(0, 1000);
    let mut reporter = Recorder::default();
    let mut run = block_on(Run::start(&agent, &task())).unwrap();

    let mut turn = |run: &mut Run| block_on(run.turn(&provider, &clock, &mut reporter));
    assert_eq!(turn(&mut run), Some(Outcome::Answered("Noon.".into())));
    run.append(Entry::Task("And now?".into()));
    assert_eq!(turn(&mut run), None, "the model called a tool");
    assert_eq!(
        turn(&mut run),
        Some(Outcome::Answered("Still noon.".into()))
    );

    let second = &provider.requests()[1].log;
    assert_eq!(second.len(), 3);
    assert_eq!(second[2], Entry::Task("And now?".into()));
    assert_eq!(reporter.turns.len(), 3, "every turn reported");
    assert_eq!(run.summary(0).turns, 3);
    assert_eq!(reporter.finished, None, "only execute finishes a run");
}
